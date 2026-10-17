use crate::{Captured, Result, Settings};
use chrono::{Local, NaiveDate};
use md5::{Digest, Md5};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureRecord {
    pub id: String,
    pub captured_at: String,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
    /// Older records did not store a digest; do not invent one on read.
    #[serde(default)]
    pub md5: Option<String>,
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

impl AuditEvent {
    /// Only screenshot creation and screenshot deletion are persisted to the
    /// Windows event log. Other kinds remain useful as internal state and
    /// diagnostics, but are intentionally excluded from the event stream.
    pub fn is_persisted(&self) -> bool {
        matches!(self.kind.as_str(), "captured" | "retention")
    }

    pub fn event_id(&self) -> u32 {
        match self.kind.as_str() {
            "startup" => 1001,
            "shutdown" => 1002,
            "started" => 1003,
            "stopped" => 1004,
            "settings" => 1005,
            "captured" => 1100,
            "capture_error" => 1101,
            "skipped" => 1102,
            "resumed" => 1103,
            "retention" => 1200,
            "cleanup_error" => 1201,
            "error" => 1999,
            _ => 1000,
        }
    }

    pub fn is_error(&self) -> bool {
        matches!(
            self.kind.as_str(),
            "error" | "capture_error" | "cleanup_error"
        )
    }
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
    roots: Vec<PathBuf>,
    next_root: Mutex<usize>,
}

/// Keep this much free space on the volume that contains the records.
pub const MIN_FREE_SPACE_BYTES: u64 = 100 * 1024 * 1024;

impl Storage {
    pub fn new(root: PathBuf) -> Result<Self> {
        let roots = storage_roots(&root);
        let mut usable = Vec::with_capacity(roots.len());
        for path in roots {
            match fs::create_dir_all(path.join("records")) {
                Ok(()) => usable.push(path),
                Err(error) if path != root => {
                    // A fixed drive can be present but unavailable to this user. Keep
                    // monitoring on the primary application-data volume in that case.
                    eprintln!("跳过不可写的截图磁盘：{}；{}", path.display(), error);
                }
                Err(error) => return Err(format!("无法创建记录目录：{error}")),
            }
        }
        let next = if usable.len() > 1 {
            total_jpegs(&usable).unwrap_or(0) % usable.len()
        } else {
            0
        };
        Ok(Self {
            root: root.clone(),
            roots: usable,
            next_root: Mutex::new(next),
        })
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

    fn available_space_for(&self, root: &Path) -> Result<u64> {
        available_space_bytes_at(root)
    }

    fn choose_root(&self) -> Result<PathBuf> {
        let mut next = self
            .next_root
            .lock()
            .map_err(|_| "截图磁盘选择器发生异常，请重新启动客户端".to_string())?;
        let root = self
            .roots
            .get(*next % self.roots.len())
            .cloned()
            .ok_or("没有可用的截图记录目录".to_string())?;
        *next = (*next + 1) % self.roots.len();
        Ok(root)
    }

    pub fn roots_description(&self) -> String {
        self.roots
            .iter()
            .map(|root| root.display().to_string())
            .collect::<Vec<_>>()
            .join("；")
    }

    fn all_day_dirs(&self, date: &str) -> Result<Vec<PathBuf>> {
        validate_date(date)?;
        let mut dirs = Vec::new();
        for root in &self.roots {
            let path = root.join("records").join(date);
            if path.is_symlink() {
                return Err("记录日期目录不能是符号链接".into());
            }
            if path.is_dir() {
                dirs.push(path);
            }
        }
        Ok(dirs)
    }

    #[cfg(test)]
    fn day_dir(&self, date: &str) -> Result<PathBuf> {
        validate_date(date)?;
        let path = self.root.join("records").join(date);
        if path.is_symlink() {
            return Err("记录日期目录不能是符号链接".into());
        }
        Ok(path)
    }

    pub fn records(&self, date: &str, page: usize) -> Result<RecordPage> {
        let mut records = Vec::new();
        for dir in self.all_day_dirs(date)? {
            let mut local = read_lines::<CaptureRecord>(&dir.join("captures.jsonl"))?;
            local.reverse();
            records.extend(local);
        }
        records.sort_by(|a, b| {
            b.captured_at
                .cmp(&a.captured_at)
                .then_with(|| b.id.cmp(&a.id))
        });
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

    /// Snapshot all retained metadata for a continuous history timeline.
    /// Image bytes are fetched separately, only for the selected record.
    pub fn replay_records(&self) -> Result<Vec<CaptureRecord>> {
        let mut timed = Vec::new();
        for date in self.dates()? {
            for dir in self.all_day_dirs(&date)? {
                let records: Vec<CaptureRecord> = read_lines(&dir.join("captures.jsonl"))?;
                for record in records {
                    let time = chrono::DateTime::parse_from_rfc3339(&record.captured_at)
                        .map_err(|_| "截图时间无效，无法读取回放记录".to_string())?;
                    timed.push((time, record));
                }
            }
        }
        timed.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.id.cmp(&b.1.id)));
        Ok(timed.into_iter().map(|(_, record)| record).collect())
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
        let id = format!(
            "{}-{}",
            date,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_nanos()
        );
        let jpeg = captured.jpeg;
        let record = CaptureRecord {
            id: id.clone(),
            captured_at: now.to_rfc3339(),
            width: captured.width,
            height: captured.height,
            bytes: jpeg.len() as u64,
            md5: Some(format!("{:x}", Md5::digest(&jpeg))),
            window_title: captured.window_title,
            process_name: captured.process_name,
            idle_seconds: captured.idle_seconds,
        };
        let first = self.choose_root()?;
        let mut roots = vec![first.clone()];
        roots.extend(
            self.roots
                .iter()
                .filter(|root| root.as_path() != first.as_path())
                .cloned(),
        );
        let mut errors = Vec::new();
        for root in roots {
            let dir = root.join("records").join(&date);
            if let Err(error) = fs::create_dir_all(&dir) {
                errors.push(format!(
                    "阶段：创建截图目录；路径：{}；{error}",
                    dir.display()
                ));
                continue;
            }
            let path = dir.join(format!("{id}.jpg"));
            if let Err(error) = atomic_write(&path, &jpeg) {
                errors.push(format!("阶段：保存图片；路径：{}；{error}", path.display()));
                continue;
            }
            let index = dir.join("captures.jsonl");
            if let Err(error) = append_line(&index, &record) {
                let rollback = fs::remove_file(&path);
                errors.push(format!(
                    "阶段：写入索引；路径：{}；截图编号：{}；{}{}",
                    index.display(),
                    record.id,
                    error,
                    rollback
                        .err()
                        .map(|e| format!("；图片回滚删除失败：{}；{e}", path.display()))
                        .unwrap_or_default()
                ));
                continue;
            }
            return Ok(record);
        }
        Err(errors.join("；"))
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
        for root in &self.roots {
            let path = root.join("records").join(date).join(format!("{id}.jpg"));
            if !path.is_symlink() && path.is_file() {
                return Ok(path);
            }
        }
        Err("截图已过期或文件不存在".into())
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
        for root in &self.roots {
            let Ok(entries) = fs::read_dir(root.join("records")) else {
                continue;
            };
            for entry in entries {
                let entry = entry.map_err(|e| e.to_string())?;
                let name = entry.file_name().to_string_lossy().to_string();
                if entry.file_type().map_err(|e| e.to_string())?.is_dir()
                    && validate_date(&name).is_ok()
                {
                    dates.push(name);
                }
            }
        }
        dates.sort();
        dates.dedup();
        Ok(dates)
    }

    pub fn prune(
        &self,
        retention_days: u32,
        mut on_deleted: impl FnMut(&Path, u64),
    ) -> Result<usize> {
        let cutoff = Local::now().date_naive()
            - chrono::Duration::days(i64::from(retention_days.saturating_sub(1)));
        let mut removed = 0;
        for date in self.dates()? {
            if validate_date(&date)? < cutoff {
                // Report each successful image deletion immediately, even if a later
                // deletion fails. Never follow symlinks outside an owned date directory.
                for dir in self.all_day_dirs(&date)? {
                    for entry in fs::read_dir(&dir).map_err(|e| e.to_string())? {
                        let entry = entry.map_err(|e| e.to_string())?;
                        let path = entry.path();
                        if path.extension().and_then(|v| v.to_str()) == Some("jpg")
                            && entry.file_type().map_err(|e| e.to_string())?.is_file()
                        {
                            let bytes = entry.metadata().map_err(|e| e.to_string())?.len();
                            fs::remove_file(&path).map_err(|e| {
                                format!("清理过期截图失败：{}；{e}", path.display())
                            })?;
                            on_deleted(&path, bytes);
                        }
                    }
                    // Only date directories owned by this app; never follow symlinks.
                    fs::remove_dir_all(&dir).map_err(|e| format!("清理过期记录失败：{e}"))?;
                }
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Remove the oldest JPEGs until screenshot storage is within the configured limit.
    /// The capture index is rewritten together with each removed image so records cannot
    /// point at files that no longer exist.
    pub fn prune_storage(
        &self,
        limit_mb: u64,
        mut on_deleted: impl FnMut(&Path, u64, u64, u64),
    ) -> Result<usize> {
        let limit = limit_mb.saturating_mul(1024 * 1024);
        let mut files = Vec::new();
        let mut total = 0u64;
        for date in self.dates()? {
            for dir in self.all_day_dirs(&date)? {
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
        }
        if total <= limit {
            return Ok(0);
        }
        files.sort_by(|a, b| {
            a.1.file_name()
                .cmp(&b.1.file_name())
                .then_with(|| a.1.cmp(&b.1))
        });
        let mut removed = 0;
        for (_date, path, bytes) in files {
            if total <= limit {
                break;
            }
            let id = path
                .file_stem()
                .and_then(|v| v.to_str())
                .ok_or("截图文件名无效")?
                .to_string();
            fs::remove_file(&path)
                .map_err(|e| format!("空间清理删除失败：{}；{e}", path.display()))?;
            on_deleted(&path, bytes, total, total.saturating_sub(bytes));
            let index = path.parent().ok_or("截图目录无效")?.join("captures.jsonl");
            let records: Vec<CaptureRecord> = read_lines::<CaptureRecord>(&index)
                .map_err(|e| format!("清理后读取索引失败：{}；{e}", index.display()))?
                .into_iter()
                .filter(|record: &CaptureRecord| record.id != id)
                .collect();
            rewrite_lines(&index, &records)
                .map_err(|e| format!("清理后更新索引失败：{}；{e}", index.display()))?;
            total = total.saturating_sub(bytes);
            removed += 1;
        }
        Ok(removed)
    }

    /// Remove the oldest JPEGs until the records volume has the requested
    /// amount of free space, or until this application's screenshots are gone.
    pub fn prune_for_free_space(
        &self,
        reserve_bytes: u64,
        mut on_deleted: impl FnMut(&Path, u64, u64, u64),
    ) -> Result<usize> {
        let mut removed = 0;
        loop {
            let Some((volume_root, available)) = self
                .roots
                .iter()
                .filter_map(|root| self.available_space_for(root).ok().map(|free| (root, free)))
                .filter(|(_, free)| *free <= reserve_bytes)
                .min_by_key(|(_, free)| *free)
            else {
                return Ok(removed);
            };
            let mut files = Vec::new();
            for date in self.dates()? {
                let dir = volume_root.join("records").join(&date);
                if !dir.is_dir() {
                    continue;
                }
                for entry in fs::read_dir(&dir).map_err(|e| e.to_string())? {
                    let entry = entry.map_err(|e| e.to_string())?;
                    let path = entry.path();
                    if path.extension().and_then(|v| v.to_str()) == Some("jpg")
                        && !path.is_symlink()
                        && path.is_file()
                    {
                        let bytes = entry.metadata().map_err(|e| e.to_string())?.len();
                        files.push((date.clone(), path, bytes));
                    }
                }
            }
            files.sort_by(|a, b| a.1.cmp(&b.1));
            let Some((_date, path, bytes)) = files.into_iter().next() else {
                return Ok(removed);
            };
            let id = path
                .file_stem()
                .and_then(|v| v.to_str())
                .ok_or("截图文件名无效")?
                .to_string();
            fs::remove_file(&path)
                .map_err(|e| format!("磁盘空间清理删除失败：{}；{e}", path.display()))?;
            let after = self.available_space_for(volume_root)?;
            on_deleted(&path, bytes, available, after);
            let index = path.parent().ok_or("截图目录无效")?.join("captures.jsonl");
            let records: Vec<CaptureRecord> = read_lines::<CaptureRecord>(&index)
                .map_err(|e| format!("清理后读取索引失败：{}；{e}", index.display()))?
                .into_iter()
                .filter(|record: &CaptureRecord| record.id != id)
                .collect();
            rewrite_lines(&index, &records)
                .map_err(|e| format!("清理后更新索引失败：{}；{e}", index.display()))?;
            removed += 1;
        }
    }
}

fn storage_roots(primary: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    #[cfg(all(windows, not(test)))]
    {
        let mut primary_included = false;
        for drive in fixed_drive_roots() {
            let same_drive = drive_letter(primary)
                .zip(drive_letter(&drive))
                .map(|(left, right)| left.eq_ignore_ascii_case(&right))
                .unwrap_or(false);
            let candidate = if same_drive {
                primary_included = true;
                primary.to_owned()
            } else {
                drive.join("SecurityMonitoring")
            };
            if !roots.iter().any(|existing| existing == &candidate) {
                roots.push(candidate);
            }
        }
        if !primary_included {
            roots.insert(0, primary.to_owned());
        }
        roots.sort_by_key(|path| drive_letter(path).unwrap_or('\0'));
    }
    if roots.is_empty() {
        roots.push(primary.to_owned());
    }
    roots
}

#[cfg(windows)]
fn drive_letter(path: &Path) -> Option<char> {
    let text = path.to_string_lossy();
    let mut chars = text.chars();
    let letter = chars.next()?;
    (chars.next() == Some(':')).then_some(letter)
}

#[cfg(windows)]
fn fixed_drive_roots() -> Vec<PathBuf> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
    const DRIVE_FIXED: u32 = 3;
    let mask = unsafe { GetLogicalDrives() };
    let mut drives = Vec::new();
    for index in 0..26u32 {
        if mask & (1 << index) == 0 {
            continue;
        }
        let letter = (b'A' + index as u8) as char;
        let root = format!("{letter}:\\");
        let wide: Vec<u16> = std::ffi::OsStr::new(&root)
            .encode_wide()
            .chain(Some(0))
            .collect();
        if unsafe { GetDriveTypeW(wide.as_ptr()) } == DRIVE_FIXED && !is_hotpluggable_drive(letter)
        {
            drives.push(PathBuf::from(root));
        }
    }
    drives
}

#[cfg(windows)]
fn is_hotpluggable_drive(letter: char) -> bool {
    use std::{mem, os::windows::ffi::OsStrExt, ptr};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING},
        System::{
            Ioctl::{IOCTL_STORAGE_GET_HOTPLUG_INFO, STORAGE_HOTPLUG_INFO},
            IO::DeviceIoControl,
        },
    };
    let device = format!(r"\\.{letter}:");
    let wide: Vec<u16> = std::ffi::OsStr::new(&device)
        .encode_wide()
        .chain(Some(0))
        .collect();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null(),
            OPEN_EXISTING,
            0,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        // If the bus type cannot be checked, do not write to the volume.
        return true;
    }
    let mut info = STORAGE_HOTPLUG_INFO {
        Size: mem::size_of::<STORAGE_HOTPLUG_INFO>() as u32,
        ..Default::default()
    };
    let mut returned = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_GET_HOTPLUG_INFO,
            ptr::null(),
            0,
            &mut info as *mut _ as *mut _,
            mem::size_of::<STORAGE_HOTPLUG_INFO>() as u32,
            &mut returned,
            ptr::null_mut(),
        )
    } != 0;
    unsafe { CloseHandle(handle) };
    !ok || info.MediaRemovable || info.MediaHotplug || info.DeviceHotplug
}

#[cfg(windows)]
fn available_space_bytes_at(path: &Path) -> Result<u64> {
    use std::{os::windows::ffi::OsStrExt, ptr};
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut available = 0u64;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut available,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(format!(
            "无法读取记录磁盘剩余空间：{}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(available)
}

#[cfg(not(windows))]
fn available_space_bytes_at(_: &Path) -> Result<u64> {
    // Native capture is Windows-only. Tests and browser preview should not
    // pretend that the host volume is low on space.
    Ok(u64::MAX)
}

fn total_jpegs(roots: &[PathBuf]) -> Result<usize> {
    let mut total = 0;
    for root in roots {
        let records = root.join("records");
        if !records.is_dir() {
            continue;
        }
        let Ok(dates) = fs::read_dir(records) else {
            continue;
        };
        for date in dates {
            let date = date.map_err(|e| e.to_string())?.path();
            if !date.is_dir() {
                continue;
            }
            let Ok(entries) = fs::read_dir(date) else {
                continue;
            };
            for entry in entries {
                let entry = entry.map_err(|e| e.to_string())?;
                if entry.path().extension().and_then(|v| v.to_str()) == Some("jpg")
                    && entry.file_type().map_err(|e| e.to_string())?.is_file()
                {
                    total += 1;
                }
            }
        }
    }
    Ok(total)
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
        // Windows append-only handles cannot truncate an interrupted write.
        // The monitor's runtime mutex serializes repair and append operations.
        .write(true)
        .truncate(false)
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
    file.seek(SeekFrom::Start(committed))
        .map_err(|e| e.to_string())?;
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
    fn only_capture_and_delete_events_are_persisted() {
        for kind in ["captured", "retention"] {
            assert!(AuditEvent {
                created_at: String::new(),
                kind: kind.into(),
                message: String::new(),
            }
            .is_persisted());
        }
        for kind in [
            "startup",
            "shutdown",
            "started",
            "stopped",
            "settings",
            "capture_error",
            "skipped",
            "resumed",
            "cleanup_error",
            "error",
        ] {
            assert!(!AuditEvent {
                created_at: String::new(),
                kind: kind.into(),
                message: String::new(),
            }
            .is_persisted());
        }
    }

    #[test]
    fn digest_matches_saved_bytes_and_survives_restart_with_legacy_records() {
        let dir = tempfile::tempdir().unwrap();
        let store = Storage::new(dir.path().to_owned()).unwrap();
        let record = store
            .save_capture(Captured {
                jpeg: b"abc".to_vec(),
                width: 10,
                height: 20,
                window_title: "窗口".into(),
                process_name: "app.exe".into(),
                idle_seconds: 2,
            })
            .unwrap();
        assert_eq!(
            record.md5.as_deref(),
            Some("900150983cd24fb0d6963f7d28e17f72")
        );
        assert_eq!(
            fs::read(store.image_path(&record.id).unwrap()).unwrap(),
            b"abc"
        );
        let mut legacy = serde_json::to_value(&record).unwrap();
        legacy.as_object_mut().unwrap().remove("md5");
        append_line(
            &store.day_dir(&today()).unwrap().join("captures.jsonl"),
            &legacy,
        )
        .unwrap();
        let reopened = Storage::new(dir.path().to_owned()).unwrap();
        let records = reopened.records(&today(), 1).unwrap().records;
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].md5, None);
        assert_eq!(records[1].md5, record.md5);
        assert_eq!(reopened.replay_records().unwrap().len(), 2);
    }

    #[test]
    fn captures_round_robin_across_configured_storage_roots() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let mut store = Storage::new(first.path().to_owned()).unwrap();
        store.roots.push(second.path().to_owned());
        fs::create_dir_all(second.path().join("records")).unwrap();
        *store.next_root.lock().unwrap() = 0;

        for value in 0..4u8 {
            store
                .save_capture(Captured {
                    jpeg: vec![value],
                    width: 1,
                    height: 1,
                    window_title: String::new(),
                    process_name: String::new(),
                    idle_seconds: 0,
                })
                .unwrap();
        }

        let first_count = total_jpegs(&[first.path().to_owned()]).unwrap();
        let second_count = total_jpegs(&[second.path().to_owned()]).unwrap();
        assert_eq!((first_count, second_count), (2, 2));
        assert_eq!(store.replay_records().unwrap().len(), 4);
    }

    #[test]
    fn replay_index_covers_all_days_and_pages_in_chronological_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = Storage::new(dir.path().to_owned()).unwrap();
        let day = store.day_dir("2026-10-13").unwrap();
        fs::create_dir_all(&day).unwrap();
        let path = day.join("captures.jsonl");
        for minute in (0..45).rev() {
            append_line(
                &path,
                &CaptureRecord {
                    id: format!("2026-10-13-{minute:02}"),
                    captured_at: format!("2026-10-13T08:{minute:02}:00+08:00"),
                    width: 10,
                    height: 10,
                    bytes: 3,
                    md5: None,
                    window_title: "工作窗口".into(),
                    process_name: "app.exe".into(),
                    idle_seconds: 0,
                },
            )
            .unwrap();
        }
        let previous_day = store.day_dir("2026-10-12").unwrap();
        fs::create_dir_all(&previous_day).unwrap();
        let mut older = store.records("2026-10-13", 1).unwrap().records[0].clone();
        older.id = "2026-10-12-00".into();
        older.captured_at = "2026-10-12T23:59:00+08:00".into();
        append_line(&previous_day.join("captures.jsonl"), &older).unwrap();
        let records = store.replay_records().unwrap();
        assert_eq!(records.len(), 46);
        assert_eq!(records.first().unwrap().id, "2026-10-12-00");
        assert_eq!(records[1].id, "2026-10-13-00");
        assert_eq!(records.last().unwrap().id, "2026-10-13-44");
        let mut invalid = records[0].clone();
        invalid.captured_at = "invalid".into();
        append_line(&path, &invalid).unwrap();
        assert!(store.replay_records().is_err());
    }

    #[test]
    fn append_repairs_partial_tail_and_preserves_committed_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("captures.jsonl");
        for initial in [b"".as_slice(), b"{incomplete", b"1\n", b"1\n{incomplete"] {
            fs::write(&path, initial).unwrap();
            append_line(&path, &2).unwrap();
            append_line(&path, &3).unwrap();
            let expected = if initial.starts_with(b"1\n") {
                b"1\n2\n3\n".as_slice()
            } else {
                b"2\n3\n".as_slice()
            };
            assert_eq!(fs::read(&path).unwrap(), expected);
        }
    }

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
        assert_eq!(store.prune(1, |_, _| {}).unwrap(), 1);
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
        assert_eq!(store.prune_storage(0, |_, _, _, _| {}).unwrap(), 2);
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
            .prune_storage(0, |path, _, _, _| deleted.push(path.to_owned()))
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
