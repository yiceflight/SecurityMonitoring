use crate::{Captured, Result, Settings};
use chrono::{Local, NaiveDate};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureRecord {
    pub id: String,
    pub captured_at: String,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
    pub window_title: String,
    pub process_name: String,
    pub idle_seconds: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEvent {
    pub created_at: String,
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordPage {
    pub records: Vec<CaptureRecord>,
    pub total: usize,
    pub page: usize,
    pub page_size: usize,
}

pub struct Storage {
    pub root: PathBuf,
}

impl Storage {
    pub fn new(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(root.join("records")).map_err(|e| format!("无法创建记录目录：{e}"))?;
        Ok(Self { root })
    }

    pub fn load_settings(&self) -> Result<Settings> {
        let path = self.root.join("settings.json");
        if !path.exists() {
            return Ok(Settings::default());
        }
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
                .map_err(|e| format!("配置文件损坏，已停止自动监控。请检查 settings.json：{e}"))?;
        // Migrate only the retired options; continue rejecting unknown settings.
        if let Some(object) = value.as_object_mut() {
            object.remove("eventLog");
            object.remove("logIntervalSeconds");
        }
        let settings: Settings = serde_json::from_value(value)
            .map_err(|e| format!("配置文件损坏，已停止自动监控。请检查 settings.json：{e}"))?;
        settings.validate()?;
        Ok(settings)
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<()> {
        atomic_write(
            &self.root.join("settings.json"),
            &serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?,
        )
    }

    fn day_dir(&self, date: &str) -> Result<PathBuf> {
        validate_date(date)?;
        let path = self.root.join("records").join(date);
        if path.is_symlink() {
            return Err("记录日期目录不能是符号链接".into());
        }
        Ok(path)
    }

    pub fn records(&self, date: &str, page: usize) -> Result<RecordPage> {
        let mut records: Vec<CaptureRecord> =
            read_lines(&self.day_dir(date)?.join("captures.jsonl"))?;
        records.reverse();
        let total = records.len();
        let page_size = 20;
        let offset = page.saturating_sub(1).saturating_mul(page_size);
        Ok(RecordPage {
            records: records.into_iter().skip(offset).take(page_size).collect(),
            total,
            page: page.max(1),
            page_size,
        })
    }

    pub fn recent_capture(&self) -> Result<Option<CaptureRecord>> {
        for date in self.dates()?.into_iter().rev() {
            let record = self.records(&date, 1)?.records.into_iter().next();
            if record.is_some() {
                return Ok(record);
            }
        }
        Ok(None)
    }

    pub fn save_capture(&self, captured: Captured) -> Result<CaptureRecord> {
        let now = Local::now();
        let date = now.format("%Y-%m-%d").to_string();
        let dir = self.day_dir(&date)?;
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let id = format!(
            "{}-{}",
            date,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_nanos()
        );
        let record = CaptureRecord {
            id: id.clone(),
            captured_at: now.to_rfc3339(),
            width: captured.width,
            height: captured.height,
            bytes: captured.jpeg.len() as u64,
            window_title: captured.window_title,
            process_name: captured.process_name,
            idle_seconds: captured.idle_seconds,
        };
        let path = dir.join(format!("{id}.jpg"));
        atomic_write(&path, &captured.jpeg)?;
        if let Err(error) = append_line(&dir.join("captures.jsonl"), &record) {
            let _ = fs::remove_file(path);
            return Err(error);
        }
        Ok(record)
    }

    pub fn image_path(&self, id: &str) -> Result<PathBuf> {
        let date = id.get(..10).ok_or("无效的截图编号")?;
        validate_date(date)?;
        if id.as_bytes().get(10) != Some(&b'-')
            || id.len() > 50
            || id.len() < 12
            || !id[11..].bytes().all(|b| b.is_ascii_digit())
        {
            return Err("无效的截图编号".into());
        }
        let path = self.day_dir(date)?.join(format!("{id}.jpg"));
        if path.is_symlink() || !path.is_file() {
            return Err("截图已过期或文件不存在".into());
        }
        Ok(path)
    }

    pub fn image_data(&self, id: &str) -> Result<String> {
        use base64::Engine;
        let path = self.image_path(id)?;
        if fs::metadata(&path).map_err(|e| e.to_string())?.len() > 64 * 1024 * 1024 {
            return Err("图片过大，请在系统查看器中打开".into());
        }
        let bytes = fs::read(path).map_err(|e| e.to_string())?;
        Ok(format!(
            "data:image/jpeg;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ))
    }

    pub fn dates(&self) -> Result<Vec<String>> {
        let mut dates = Vec::new();
        for entry in fs::read_dir(self.root.join("records")).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().to_string();
            if entry.file_type().map_err(|e| e.to_string())?.is_dir()
                && validate_date(&name).is_ok()
            {
                dates.push(name);
            }
        }
        dates.sort();
        Ok(dates)
    }

    pub fn prune(&self, retention_days: u32, mut on_deleted: impl FnMut(&Path)) -> Result<usize> {
        let cutoff = Local::now().date_naive()
            - chrono::Duration::days(i64::from(retention_days.saturating_sub(1)));
        let mut removed = 0;
        for date in self.dates()? {
            if validate_date(&date)? < cutoff {
                // Report each successful image deletion immediately, even if a later
                // deletion fails. Never follow symlinks outside an owned date directory.
                for entry in fs::read_dir(self.day_dir(&date)?).map_err(|e| e.to_string())? {
                    let entry = entry.map_err(|e| e.to_string())?;
                    let path = entry.path();
                    if path.extension().and_then(|v| v.to_str()) == Some("jpg")
                        && entry.file_type().map_err(|e| e.to_string())?.is_file()
                    {
                        fs::remove_file(&path).map_err(|e| format!("清理过期截图失败：{e}"))?;
                        on_deleted(&path);
                    }
                }
                // Only date directories owned by this app; never follow symlinks.
                fs::remove_dir_all(self.day_dir(&date)?)
                    .map_err(|e| format!("清理过期记录失败：{e}"))?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Remove the oldest JPEGs until screenshot storage is within the configured limit.
    /// The capture index is rewritten together with each removed image so records cannot
    /// point at files that no longer exist.
    pub fn prune_storage(&self, limit_mb: u64, mut on_deleted: impl FnMut(&Path)) -> Result<usize> {
        let limit = limit_mb
            .checked_mul(1024 * 1024)
            .ok_or("截图空间限制过大")?;
        let mut files = Vec::new();
        let mut total = 0u64;
        for date in self.dates()? {
            let dir = self.day_dir(&date)?;
            for entry in fs::read_dir(&dir).map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                let path = entry.path();
                if path.extension().and_then(|v| v.to_str()) != Some("jpg")
                    || path.is_symlink()
                    || !path.is_file()
                {
                    continue;
                }
                let bytes = entry.metadata().map_err(|e| e.to_string())?.len();
                total = total.saturating_add(bytes);
                files.push((date.clone(), path, bytes));
            }
        }
        if total <= limit {
            return Ok(0);
        }
        files.sort_by(|a, b| a.1.cmp(&b.1));
        let mut removed = 0;
        for (date, path, bytes) in files {
            if total <= limit {
                break;
            }
            let id = path
                .file_stem()
                .and_then(|v| v.to_str())
                .ok_or("截图文件名无效")?
                .to_string();
            fs::remove_file(&path).map_err(|e| e.to_string())?;
            on_deleted(&path);
            let index = self.day_dir(&date)?.join("captures.jsonl");
            let records: Vec<CaptureRecord> = read_lines::<CaptureRecord>(&index)?
                .into_iter()
                .filter(|record: &CaptureRecord| record.id != id)
                .collect();
            rewrite_lines(&index, &records)?;
            total = total.saturating_sub(bytes);
            removed += 1;
        }
        Ok(removed)
    }
}

pub fn today() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

fn validate_date(date: &str) -> Result<NaiveDate> {
    if date.len() != 10 {
        return Err("日期格式须为 YYYY-MM-DD".into());
    }
    let parsed =
        NaiveDate::parse_from_str(date, "%Y-%m-%d").map_err(|_| "无效的日期".to_string())?;
    if parsed.format("%Y-%m-%d").to_string() != date {
        return Err("无效的日期".into());
    }
    Ok(parsed)
}

fn read_lines<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(file);
    let mut records = Vec::new();
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            break;
        }
        // A crash may leave the final append incomplete. It was not committed.
        if !line.ends_with('\n') {
            break;
        }
        records.push(
            serde_json::from_str(&line)
                .map_err(|e| format!("记录文件损坏 {}：{e}", path.display()))?,
        );
    }
    Ok(records)
}

fn append_line<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    // Repair an interrupted final write before appending a new committed line.
    use std::io::{Read, Seek, SeekFrom};
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    let mut committed = len;
    while committed > 0 {
        file.seek(SeekFrom::Start(committed - 1))
            .map_err(|e| e.to_string())?;
        let mut byte = [0];
        file.read_exact(&mut byte).map_err(|e| e.to_string())?;
        if byte[0] == b'\n' {
            break;
        }
        committed -= 1;
    }
    if committed != len {
        file.set_len(committed).map_err(|e| e.to_string())?;
    }
    if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
        let rollback = file.set_len(committed).and_then(|_| file.sync_all());
        return Err(match rollback {
            Ok(()) => error.to_string(),
            Err(rollback) => format!("记录落盘失败：{error}；尾部回滚失败：{rollback}"),
        });
    }
    Ok(())
}

fn rewrite_lines<T: Serialize>(path: &Path, values: &[T]) -> Result<()> {
    let mut bytes = Vec::new();
    for value in values {
        bytes.extend(serde_json::to_vec(value).map_err(|e| e.to_string())?);
        bytes.push(b'\n');
    }
    atomic_write(path, &bytes)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let temp = path.with_extension("tmp");
    let mut file = File::create(&temp).map_err(|e| e.to_string())?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    drop(file);
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        };
        let from: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        if unsafe {
            MoveFileExW(
                from.as_ptr(),
                to.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
    }
    #[cfg(not(windows))]
    fs::rename(temp, path).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_survive_restart_and_truncated_append() {
        let dir = tempfile::tempdir().unwrap();
        let store = Storage::new(dir.path().to_owned()).unwrap();
        let record = store
            .save_capture(Captured {
                jpeg: vec![1, 2, 3],
                width: 10,
                height: 20,
                window_title: "窗口".into(),
                process_name: "app.exe".into(),
                idle_seconds: 2,
            })
            .unwrap();
        let path = store.day_dir(&today()).unwrap().join("captures.jsonl");
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{incomplete")
            .unwrap();
        assert_eq!(store.records(&today(), 1).unwrap().total, 1);
        append_line(&path, &record).unwrap();
        assert_eq!(store.records(&today(), 1).unwrap().total, 2);
        assert!(store
            .image_data(&record.id)
            .unwrap()
            .starts_with("data:image/jpeg;base64,"));
        assert!(store.image_path("2026-10-12-../../secret").is_err());
        assert!(store.records("../../", 1).is_err());
    }

    #[test]
    fn retention_removes_only_expired_date_directories() {
        let dir = tempfile::tempdir().unwrap();
        let store = Storage::new(dir.path().to_owned()).unwrap();
        fs::create_dir_all(store.root.join("records/2000-01-01")).unwrap();
        fs::create_dir_all(store.root.join("records/notes")).unwrap();
        fs::create_dir_all(store.root.join("records").join(today())).unwrap();
        assert_eq!(store.prune(1, |_| {}).unwrap(), 1);
        assert!(store.root.join("records/notes").exists());
        assert!(store.root.join("records").join(today()).exists());
    }

    #[test]
    fn storage_limit_removes_oldest_screenshots_and_index_entries() {
        let dir = tempfile::tempdir().unwrap();
        let store = Storage::new(dir.path().to_owned()).unwrap();
        let first = store
            .save_capture(Captured {
                jpeg: vec![1; 80],
                width: 10,
                height: 20,
                window_title: "first".into(),
                process_name: "app.exe".into(),
                idle_seconds: 0,
            })
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1));
        let second = store
            .save_capture(Captured {
                jpeg: vec![2; 80],
                width: 10,
                height: 20,
                window_title: "second".into(),
                process_name: "app.exe".into(),
                idle_seconds: 0,
            })
            .unwrap();
        assert_eq!(store.prune_storage(0, |_| {}).unwrap(), 2);
        assert_eq!(store.records(&today(), 1).unwrap().total, 0);
        assert!(store.image_path(&first.id).is_err());
        assert!(store.image_path(&second.id).is_err());
    }

    #[test]
    fn legacy_settings_keep_capture_preferences_but_drop_retired_log_options() {
        let dir = tempfile::tempdir().unwrap();
        let store = Storage::new(dir.path().to_owned()).unwrap();
        let path = store.root.join("settings.json");
        fs::write(&path, r#"{"intervalSeconds":120,"retentionDays":45,"enabled":true,"autoStart":true,"eventLog":false,"logIntervalSeconds":3600,"screenshotStorageLimitMb":2048}"#).unwrap();
        let settings = store.load_settings().unwrap();
        assert_eq!(settings.interval_seconds, 120);
        assert_eq!(settings.retention_days, 45);
        assert!(settings.enabled && settings.auto_start);
        assert_eq!(settings.screenshot_storage_limit_mb, 2048);
        store.save_settings(&settings).unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        assert!(!saved.contains("eventLog") && !saved.contains("logIntervalSeconds"));
        fs::write(&path, r#"{"intervalSecond":120}"#).unwrap();
        assert!(store.load_settings().is_err());
    }

    #[test]
    fn deleted_image_is_reported_even_if_index_update_fails() {
        let dir = tempfile::tempdir().unwrap();
        let store = Storage::new(dir.path().to_owned()).unwrap();
        let day = store.day_dir(&today()).unwrap();
        fs::create_dir_all(day.join("captures.jsonl")).unwrap();
        let image = day.join(format!("{}-1.jpg", today()));
        fs::write(&image, [1]).unwrap();
        let mut deleted = Vec::new();
        assert!(store
            .prune_storage(0, |path| deleted.push(path.to_owned()))
            .is_err());
        assert_eq!(deleted, vec![image.clone()]);
        assert!(!image.exists());
    }

    #[test]
    fn invalid_settings_do_not_silently_resume() {
        let dir = tempfile::tempdir().unwrap();
        let store = Storage::new(dir.path().to_owned()).unwrap();
        fs::write(store.root.join("settings.json"), b"broken").unwrap();
        assert!(store.load_settings().is_err());
        let settings = Settings::default();
        store.save_settings(&settings).unwrap();
        let updated = Settings {
            enabled: true,
            ..settings
        };
        store.save_settings(&updated).unwrap();
        assert!(store.load_settings().unwrap().enabled);
    }
}
