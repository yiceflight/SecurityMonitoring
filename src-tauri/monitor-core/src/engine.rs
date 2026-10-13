use crate::{
    storage::{today, Storage},
    AuditEvent, CaptureOutcome, CaptureRecord, Host, RecordPage, Result, Settings, Status,
};
use chrono::Local;
use std::{
    path::PathBuf,
    sync::{Arc, Condvar, Mutex, MutexGuard},
    time::{Duration, Instant},
};

struct Runtime {
    settings: Settings,
    storage: Storage,
    last_capture: Option<CaptureRecord>,
    last_error: Option<String>,
    cleanup_error: Option<String>,
    auto_start_error: Option<String>,
    audit_error: Option<String>,
    waiting: Option<String>,
    waiting_since: Option<Instant>,
    consecutive_failures: u64,
    count_date: String,
    today_count: usize,
    cleanup_date: String,
    capture_timer_started: Option<Instant>,
    shutdown: bool,
}

impl Runtime {
    fn capture_delay(&self) -> Duration {
        self.capture_timer_started
            .map_or(Duration::ZERO, |started| {
                Duration::from_secs(self.settings.interval_seconds)
                    .saturating_sub(started.elapsed())
            })
    }
}

pub struct Monitor {
    host: Arc<dyn Host>,
    runtime: Mutex<Runtime>,
    wake: Condvar,
    worker_started: std::sync::atomic::AtomicBool,
    event_context: String,
}

impl Monitor {
    pub fn new(root: PathBuf, host: Arc<dyn Host>, app_version: &str) -> Result<Arc<Self>> {
        let storage = Storage::new(root)?;
        let (settings, config_error) = match storage.load_settings() {
            Ok(settings) => (settings, None),
            Err(error) => (Settings::default(), Some(error)),
        };
        let last_capture = storage.recent_capture()?;
        let today_count = storage.records(&today(), 1)?.total;
        let monitor = Arc::new(Self {
            event_context: format!(
                "客户端版本：{}\n进程 ID：{}\n运行批次：{}-{}\n{}",
                app_version,
                std::process::id(),
                std::process::id(),
                Local::now().format("%Y%m%dT%H%M%S%.9f%z"),
                host.session_context()
            ),
            host,
            runtime: Mutex::new(Runtime {
                settings,
                storage,
                last_capture,
                last_error: config_error,
                cleanup_error: None,
                auto_start_error: None,
                audit_error: None,
                waiting: None,
                waiting_since: None,
                consecutive_failures: 0,
                count_date: today(),
                today_count,
                cleanup_date: String::new(),
                capture_timer_started: None,
                shutdown: false,
            }),
            wake: Condvar::new(),
            worker_started: std::sync::atomic::AtomicBool::new(false),
        });
        if monitor.host.supported() {
            let mut rt = monitor.lock()?;
            // Reconcile the login entry after an executable has moved or updated.
            let desired = rt.settings.auto_start;
            if let Err(error) = monitor.host.set_auto_start(desired) {
                rt.auto_start_error = Some(error);
            }
            let message = format!(
                "客户端启动\n启动模式：{}\n生效配置：{}\n记录目录：{}\n配置读取错误：{}\n登录启动同步错误：{}",
                if std::env::args().any(|arg| arg == "--background") { "后台启动" } else { "普通启动" },
                settings_summary(&rt.settings), rt.storage.root.display(),
                rt.last_error.as_deref().unwrap_or("无"), rt.auto_start_error.as_deref().unwrap_or("无")
            );
            monitor.audit(&mut rt, "startup", &message);
            monitor.cleanup(&mut rt);
        }
        Ok(monitor)
    }

    fn lock(&self) -> Result<MutexGuard<'_, Runtime>> {
        self.runtime
            .lock()
            .map_err(|_| "监控线程发生异常，请重新启动客户端".into())
    }

    pub fn start_worker(self: &Arc<Self>) {
        if !self.host.supported()
            || self
                .worker_started
                .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        let monitor = Arc::clone(self);
        std::thread::spawn(move || {
            let mut rt = match monitor.lock() {
                Ok(rt) => rt,
                Err(_) => return,
            };
            while !rt.shutdown {
                if monitor.host.supported() && rt.settings.enabled && rt.capture_delay().is_zero() {
                    let _ = monitor.capture(&mut rt, "定时");
                    rt.capture_timer_started = Some(Instant::now());
                }
                let delay = if rt.settings.enabled && monitor.host.supported() {
                    // Bound each OS wait without limiting the configured interval.
                    rt.capture_delay().min(Duration::from_secs(3600))
                } else {
                    Duration::from_secs(3600)
                };
                match monitor.wake.wait_timeout(rt, delay) {
                    Ok((guard, _)) => rt = guard,
                    Err(_) => return,
                }
            }
        });
    }

    pub fn status(&self) -> Result<Status> {
        let mut rt = self.lock()?;
        self.rollover(&mut rt)?;
        let supported = self.host.supported();
        let state = if !supported {
            "unsupported"
        } else if !rt.settings.enabled {
            "stopped"
        } else if rt.last_error.is_some() {
            "error"
        } else if rt.waiting.is_some() {
            "waiting"
        } else {
            "running"
        };
        let auto_start_actual = match self.host.auto_start() {
            Ok(value) => value,
            Err(error) => {
                rt.auto_start_error = Some(format!("无法读取登录启动状态：{error}"));
                false
            }
        };
        let errors: Vec<_> = [&rt.last_error, &rt.cleanup_error, &rt.auto_start_error]
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        Ok(Status {
            supported,
            settings: rt.settings.clone(),
            state: state.into(),
            storage_path: rt.storage.root.display().to_string(),
            today_count: rt.today_count,
            last_capture: rt.last_capture.clone(),
            last_error: if errors.is_empty() {
                None
            } else {
                Some(errors.join("；"))
            },
            audit_error: rt.audit_error.clone(),
            auto_start_actual,
            event_source_registered: self.host.event_source_registered(),
        })
    }

    pub fn set_enabled(&self, enabled: bool) -> Result<()> {
        self.require_windows()?;
        let mut rt = self.lock()?;
        if rt.settings.enabled == enabled {
            return Ok(());
        }
        let settings = Settings {
            enabled,
            ..rt.settings.clone()
        };
        rt.storage.save_settings(&settings)?;
        rt.settings = settings;
        rt.waiting = None;
        rt.waiting_since = None;
        rt.consecutive_failures = 0;
        rt.last_error = None;
        rt.capture_timer_started = None;
        rt.audit_error = None;
        let message = format!(
            "{}\n原因：用户操作\n生效配置：{}",
            if enabled {
                "开始监控"
            } else {
                "停止监控"
            },
            settings_summary(&rt.settings)
        );
        self.audit(
            &mut rt,
            if enabled { "started" } else { "stopped" },
            &message,
        );
        self.wake.notify_one();
        Ok(())
    }

    pub fn save_settings(&self, mut settings: Settings) -> Result<()> {
        self.require_windows()?;
        settings.validate()?;
        let mut rt = self.lock()?;
        settings.enabled = rt.settings.enabled;
        let previous_auto_start = self.host.auto_start()?;
        self.host.set_auto_start(settings.auto_start)?;
        if let Err(error) = rt.storage.save_settings(&settings) {
            let rollback = self.host.set_auto_start(previous_auto_start);
            return Err(match rollback {
                Ok(()) => error,
                Err(rollback) => format!("{error}；自动启动回滚失败：{rollback}"),
            });
        }
        let previous_settings = settings_summary(&rt.settings);
        rt.settings = settings;
        rt.auto_start_error = None;
        rt.capture_timer_started = Some(Instant::now());
        rt.audit_error = None;
        let message = format!(
            "设置已保存\n修改前：{}\n修改后：{}",
            previous_settings,
            settings_summary(&rt.settings)
        );
        self.audit(&mut rt, "settings", &message);
        rt.cleanup_date.clear();
        self.cleanup(&mut rt);
        self.wake.notify_one();
        Ok(())
    }

    pub fn capture_now(&self) -> Result<Option<CaptureRecord>> {
        self.require_windows()?;
        let mut rt = self.lock()?;
        let result = self.capture(&mut rt, "手动");
        rt.capture_timer_started = Some(Instant::now());
        self.wake.notify_one();
        result
    }

    fn capture(&self, rt: &mut Runtime, trigger: &str) -> Result<Option<CaptureRecord>> {
        let previous_audit_error = rt.audit_error.take();
        if let Err(error) = self.rollover(rt) {
            return self.capture_failed(rt, trigger, format!("阶段：读取当天记录；{error}"));
        }
        self.cleanup(rt);
        let capture_started = Instant::now();
        let mut capture_ms = 0;
        let mut save_ms = 0;
        match self
            .host
            .capture()
            .map_err(|e| format!("阶段：采集或编码；{e}"))
            .and_then(|outcome| match outcome {
                CaptureOutcome::Captured(captured) => {
                    capture_ms = capture_started.elapsed().as_millis();
                    let save_started = Instant::now();
                    let result = rt.storage.save_capture(captured).map(Some);
                    save_ms = save_started.elapsed().as_millis();
                    result
                }
                CaptureOutcome::Unavailable(reason) => {
                    rt.waiting_since.get_or_insert_with(Instant::now);
                    if rt.waiting.as_ref() != Some(&reason) {
                        self.audit(
                            rt,
                            "skipped",
                            &format!(
                                "暂停截图\n原因：{reason}\n触发方式：{trigger}\n截图间隔：{} 秒",
                                rt.settings.interval_seconds
                            ),
                        );
                    } else if rt.audit_error.is_none() {
                        // An unchanged pause emits no event; it cannot prove that logging recovered.
                        rt.audit_error = previous_audit_error.clone();
                    }
                    rt.waiting = Some(reason);
                    Ok(None)
                }
            }) {
            Ok(Some(record)) => {
                let path = match rt.storage.image_path(&record.id) {
                    Ok(path) => path,
                    Err(error) => {
                        return self.capture_failed(
                            rt,
                            trigger,
                            format!("阶段：确认图片路径；截图编号：{}；{error}", record.id),
                        )
                    }
                };
                let recovery = if let Some(since) = rt.waiting_since.take() {
                    Some(format!("截图已恢复\n此前暂停原因：{}\n暂停时长：{} 秒\n此前连续失败次数：{}\n恢复截图编号：{}", rt.waiting.as_deref().unwrap_or("不可用"), since.elapsed().as_secs(), rt.consecutive_failures, record.id))
                } else if rt.consecutive_failures > 0 {
                    Some(format!(
                        "截图已恢复\n此前连续失败次数：{}\n恢复截图编号：{}",
                        rt.consecutive_failures, record.id
                    ))
                } else {
                    None
                };
                rt.last_error = None;
                rt.waiting = None;
                rt.consecutive_failures = 0;
                let mut errors: Vec<_> = rt.audit_error.take().into_iter().collect();
                if let Some(message) = recovery {
                    if let Err(error) = self.write_event("resumed", &message) {
                        errors.push(error);
                    }
                }
                let message = format!(
                    "截图成功\n图片路径：{}\n截图编号：{}\n记录时间：{}\nMD5：{}\n图片分辨率：{} × {}\n文件大小：{} 字节\n前台进程：{}\n用户空闲：{} 秒\n触发方式：{}\n截图间隔：{} 秒\n采集及编码耗时：{} ms\n校验及保存耗时：{} ms",
                    path.display(), record.id, record.captured_at, record.md5.as_deref().unwrap_or("未记录"),
                    record.width, record.height, record.bytes,
                    if record.process_name.is_empty() { "不可用".into() } else { format!("{:?}", record.process_name) },
                    record.idle_seconds, trigger, rt.settings.interval_seconds, capture_ms, save_ms
                );
                if let Err(error) = self.write_event("captured", &message) {
                    errors.push(error);
                }
                errors.dedup();
                rt.audit_error = if errors.is_empty() {
                    None
                } else {
                    Some(errors.join("；"))
                };
                self.cleanup(rt);
                match rt.storage.records(&today(), 1).and_then(|page| {
                    rt.today_count = page.total;
                    rt.storage.recent_capture()
                }) {
                    Ok(latest) => rt.last_capture = latest,
                    Err(error) => {
                        let message = format!(
                            "截图已保存，但刷新记录失败；截图编号：{}；{error}",
                            record.id
                        );
                        rt.last_error = Some(message.clone());
                        if let Err(error) = self.write_event("error", &message) {
                            rt.audit_error = Some(error);
                        }
                    }
                }
                Ok(Some(record))
            }
            Ok(None) => {
                rt.last_error = None;
                Ok(None)
            }
            Err(error) => self.capture_failed(rt, trigger, error),
        }
    }

    fn capture_failed(
        &self,
        rt: &mut Runtime,
        trigger: &str,
        error: String,
    ) -> Result<Option<CaptureRecord>> {
        rt.consecutive_failures = rt.consecutive_failures.saturating_add(1);
        let message = format!(
            "截图失败\n触发方式：{}\n截图间隔：{} 秒\n连续失败次数：{}\n记录目录：{}\n错误详情：{}",
            trigger,
            rt.settings.interval_seconds,
            rt.consecutive_failures,
            rt.storage.root.display(),
            error
        );
        rt.last_error = Some(error.clone());
        self.audit(rt, "capture_error", &message);
        Err(error)
    }

    fn write_event(&self, kind: &str, message: &str) -> Result<()> {
        self.host.write_event(&AuditEvent {
            created_at: Local::now().to_rfc3339(),
            kind: kind.into(),
            message: format!("{}\n{}", message, self.event_context),
        })
    }

    fn audit(&self, rt: &mut Runtime, kind: &str, message: &str) {
        if let Err(error) = self.write_event(kind, message) {
            rt.audit_error = Some(match rt.audit_error.take() {
                Some(previous) if previous != error => format!("{previous}；{error}"),
                _ => error,
            });
        }
    }

    fn rollover(&self, rt: &mut Runtime) -> Result<()> {
        let date = today();
        if rt.count_date != date {
            rt.today_count = rt.storage.records(&date, 1)?.total;
            rt.count_date = date;
        }
        Ok(())
    }

    fn cleanup(&self, rt: &mut Runtime) {
        let date = today();
        let mut errors = Vec::new();
        let mut audit_errors = Vec::new();
        if rt.cleanup_date != date {
            match rt.storage.prune(rt.settings.retention_days, |path, bytes| {
                if let Err(error) = self.write_event(
                    "retention",
                    &format!("截图已过期，自动删除：{}\n删除结果：成功\n文件大小：{} 字节\n保留时间：{} 天", path.display(), bytes, rt.settings.retention_days),
                ) {
                    audit_errors.push(error);
                }
            }) {
                Ok(_) => rt.cleanup_date = date,
                Err(error) => errors.push(error),
            }
        }
        if let Err(error) =
            rt.storage
                .prune_storage(rt.settings.screenshot_storage_limit_mb, |path, bytes, before, after| {
                    if let Err(error) = self.write_event(
                        "retention",
                        &format!("截图超过空间限制，自动删除：{}\n删除结果：成功\n文件大小：{} 字节\n空间上限：{} MiB\n删除前截图占用：{} 字节\n删除后截图占用：{} 字节", path.display(), bytes, rt.settings.screenshot_storage_limit_mb, before, after),
                    ) {
                        audit_errors.push(error);
                    }
                })
        {
            errors.push(error);
        }
        let cleanup_error = if errors.is_empty() {
            None
        } else {
            Some(errors.join("；"))
        };
        if cleanup_error != rt.cleanup_error {
            if let Some(error) = &cleanup_error {
                if let Err(error) = self.write_event(
                    "cleanup_error",
                    &format!(
                        "清理失败\n记录目录：{}\n错误详情：{}",
                        rt.storage.root.display(),
                        error
                    ),
                ) {
                    audit_errors.push(error);
                }
            }
        }
        rt.cleanup_error = cleanup_error;
        // A later successful deletion must not hide a failed event from this cleanup.
        if !audit_errors.is_empty() {
            audit_errors.dedup();
            rt.audit_error = Some(audit_errors.join("；"));
        }
        if let Ok(latest) = rt.storage.recent_capture() {
            rt.last_capture = latest;
        }
    }

    fn require_windows(&self) -> Result<()> {
        if self.host.supported() {
            Ok(())
        } else {
            Err("请在 Windows 客户端中使用监控功能".into())
        }
    }

    pub fn records(&self, date: &str, page: usize) -> Result<RecordPage> {
        self.lock()?.storage.records(date, page)
    }
    pub fn replay_records(&self) -> Result<Vec<CaptureRecord>> {
        self.lock()?.storage.replay_records()
    }
    pub fn image_data(&self, id: &str) -> Result<String> {
        self.lock()?.storage.image_data(id)
    }
    pub fn open_record(&self, id: &str) -> Result<()> {
        self.host.open_path(&self.lock()?.storage.image_path(id)?)
    }
    pub fn open_folder(&self) -> Result<()> {
        self.host
            .open_path(&self.lock()?.storage.root.join("records"))
    }
    pub fn register_event_source(&self) -> Result<()> {
        self.require_windows()?;
        self.host.register_event_source()?;
        let mut rt = self.lock()?;
        rt.audit_error = None;
        self.audit(&mut rt, "settings", "Windows 事件日志源已注册");
        Ok(())
    }
    pub fn shutdown(&self) {
        if let Ok(mut rt) = self.lock() {
            if rt.shutdown {
                return;
            }
            self.audit(&mut rt, "shutdown", "客户端正常退出，采集调度已停止");
            // Keep the last explicit start/stop preference for the next login.
            rt.shutdown = true;
            self.wake.notify_one();
        }
    }

    pub fn report_error(&self, error: String) {
        if let Ok(mut rt) = self.lock() {
            rt.last_error = Some(error.clone());
            self.audit(&mut rt, "error", &error);
        }
    }
}

fn settings_summary(settings: &Settings) -> String {
    format!(
        "截图间隔 {} 秒，保留 {} 天，截图空间 {} MiB，登录启动 {}，监控启用 {}",
        settings.interval_seconds,
        settings.retention_days,
        settings.screenshot_storage_limit_mb,
        settings.auto_start,
        settings.enabled
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Captured;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct FakeHost {
        count: AtomicUsize,
        locked: AtomicBool,
        auto: AtomicBool,
        fail: AtomicBool,
        fail_events: AtomicBool,
        events: Mutex<Vec<AuditEvent>>,
    }
    impl Host for FakeHost {
        fn supported(&self) -> bool {
            true
        }
        fn capture(&self) -> Result<CaptureOutcome> {
            if self.locked.load(Ordering::SeqCst) {
                return Ok(CaptureOutcome::Unavailable("锁屏".into()));
            }
            if self.fail.load(Ordering::SeqCst) {
                return Err("截图失败".into());
            }
            self.count.fetch_add(1, Ordering::SeqCst);
            Ok(CaptureOutcome::Captured(Captured {
                jpeg: vec![1],
                width: 1,
                height: 1,
                window_title: "test".into(),
                process_name: "test.exe".into(),
                idle_seconds: 0,
            }))
        }
        fn auto_start(&self) -> Result<bool> {
            Ok(self.auto.load(Ordering::SeqCst))
        }
        fn set_auto_start(&self, enabled: bool) -> Result<()> {
            self.auto.store(enabled, Ordering::SeqCst);
            Ok(())
        }
        fn event_source_registered(&self) -> bool {
            false
        }
        fn write_event(&self, event: &AuditEvent) -> Result<()> {
            if self.fail_events.load(Ordering::SeqCst) {
                return Err("事件日志不可写".into());
            }
            self.events.lock().unwrap().push(event.clone());
            Ok(())
        }
        fn open_path(&self, _: &std::path::Path) -> Result<()> {
            Ok(())
        }
        fn register_event_source(&self) -> Result<()> {
            Ok(())
        }
    }

    fn fixture() -> (tempfile::TempDir, Arc<FakeHost>, Arc<Monitor>) {
        let dir = tempfile::tempdir().unwrap();
        let host = Arc::new(FakeHost {
            count: AtomicUsize::new(0),
            locked: AtomicBool::new(false),
            auto: AtomicBool::new(false),
            fail: AtomicBool::new(false),
            fail_events: AtomicBool::new(false),
            events: Mutex::new(Vec::new()),
        });
        let monitor = Monitor::new(dir.path().to_owned(), host.clone(), "test-1.2.3").unwrap();
        (dir, host, monitor)
    }

    #[test]
    fn every_capture_writes_its_full_path_without_local_audit_files() {
        let (dir, host, monitor) = fixture();
        let first = monitor.capture_now().unwrap().unwrap();
        let second = monitor.capture_now().unwrap().unwrap();
        let events = host.events.lock().unwrap();
        let captures: Vec<_> = events
            .iter()
            .filter(|event| event.kind == "captured")
            .collect();
        assert_eq!(captures.len(), 2);
        for (event, record) in captures.iter().zip([first, second]) {
            let path = monitor
                .lock()
                .unwrap()
                .storage
                .image_path(&record.id)
                .unwrap();
            assert!(event
                .message
                .contains(&format!("图片路径：{}", path.display())));
            assert!(event.message.contains(&format!("截图编号：{}", record.id)));
            assert!(event
                .message
                .contains("MD5：55a54008ad1ba589aa210d2629c1df41"));
            assert!(event.message.contains("图片分辨率：1 × 1"));
            assert!(event.message.contains("文件大小：1 字节"));
            assert!(event.message.contains("test.exe"));
            assert!(event.message.contains("触发方式：手动"));
            assert!(event.message.contains("采集及编码耗时："));
            assert!(event.message.contains("校验及保存耗时："));
            assert!(event.message.contains("客户端版本：test-1.2.3"));
            assert_eq!(event.event_id(), 1100);
            assert_eq!(std::fs::read(path).unwrap(), vec![1]);
        }
        assert!(!dir
            .path()
            .join("records")
            .join(today())
            .join("events.jsonl")
            .exists());
    }

    #[test]
    fn failures_and_pauses_log_one_recovery_with_previous_context() {
        let (_dir, host, monitor) = fixture();
        host.fail.store(true, Ordering::SeqCst);
        assert!(monitor.capture_now().is_err());
        assert!(monitor.capture_now().is_err());
        host.fail.store(false, Ordering::SeqCst);
        monitor.capture_now().unwrap();
        monitor.capture_now().unwrap();
        host.locked.store(true, Ordering::SeqCst);
        assert!(monitor.capture_now().unwrap().is_none());
        assert!(monitor.capture_now().unwrap().is_none());
        host.locked.store(false, Ordering::SeqCst);
        monitor.capture_now().unwrap();
        let events = host.events.lock().unwrap();
        let failures: Vec<_> = events
            .iter()
            .filter(|e| e.kind == "capture_error")
            .collect();
        assert_eq!(failures.len(), 2);
        assert!(failures[1].message.contains("连续失败次数：2"));
        assert!(failures[1].message.contains("阶段：采集或编码"));
        assert_eq!(failures[1].event_id(), 1101);
        assert!(failures[1].is_error());
        let recovered: Vec<_> = events.iter().filter(|e| e.kind == "resumed").collect();
        assert_eq!(recovered.len(), 2);
        assert!(recovered[0].message.contains("此前连续失败次数：2"));
        assert!(recovered[1].message.contains("此前暂停原因：锁屏"));
        assert!(recovered[1].message.contains("暂停时长："));
        assert_eq!(events.iter().filter(|e| e.kind == "skipped").count(), 1);
        assert_eq!(monitor.lock().unwrap().consecutive_failures, 0);
    }

    #[test]
    fn unchanged_pause_does_not_hide_a_failed_log_write() {
        let (_dir, host, monitor) = fixture();
        host.locked.store(true, Ordering::SeqCst);
        host.fail_events.store(true, Ordering::SeqCst);
        monitor.capture_now().unwrap();
        host.fail_events.store(false, Ordering::SeqCst);
        monitor.capture_now().unwrap();
        assert!(monitor.status().unwrap().audit_error.is_some());
        host.locked.store(false, Ordering::SeqCst);
        monitor.capture_now().unwrap();
        assert!(monitor.status().unwrap().audit_error.is_none());
    }

    #[test]
    fn startup_settings_and_scheduled_capture_include_runtime_context() {
        let (_dir, host, monitor) = fixture();
        monitor
            .save_settings(Settings {
                interval_seconds: 120,
                ..Settings::default()
            })
            .unwrap();
        monitor
            .capture(&mut monitor.lock().unwrap(), "定时")
            .unwrap();
        let events = host.events.lock().unwrap();
        let startup = &events[0];
        assert_eq!(startup.kind, "startup");
        assert!(startup.message.contains("生效配置：截图间隔 60 秒"));
        assert!(startup.message.contains("客户端版本：test-1.2.3"));
        let changed = events.iter().find(|e| e.kind == "settings").unwrap();
        assert!(changed.message.contains("修改前：截图间隔 60 秒"));
        assert!(changed.message.contains("修改后：截图间隔 120 秒"));
        let captured = events.iter().find(|e| e.kind == "captured").unwrap();
        assert!(captured.message.contains("触发方式：定时"));
        assert!(captured.message.contains("截图间隔：120 秒"));
        assert!(events
            .iter()
            .all(|e| e.message.contains(&monitor.event_context)));
    }

    #[test]
    fn failed_image_write_reports_stage_without_a_success_event() {
        let (dir, host, monitor) = fixture();
        // A file in place of today's directory makes image-directory creation fail.
        std::fs::write(dir.path().join("records").join(today()), b"blocked").unwrap();
        let error = monitor.capture_now().unwrap_err();
        assert!(error.contains("阶段：创建截图目录"));
        let events = host.events.lock().unwrap();
        assert!(!events.iter().any(|e| e.kind == "captured"));
        let failed = events.iter().find(|e| e.kind == "capture_error").unwrap();
        assert!(failed.message.contains("阶段：创建截图目录"));
        assert!(failed.message.contains(&dir.path().display().to_string()));
    }

    #[test]
    fn automatic_deletions_log_each_path_and_the_actual_reason() {
        let (dir, host, monitor) = fixture();
        let expired_dir = dir.path().join("records").join("2000-01-01");
        std::fs::create_dir_all(&expired_dir).unwrap();
        let expired: Vec<_> = ["2000-01-01-1.jpg", "2000-01-01-2.jpg"]
            .iter()
            .map(|name| expired_dir.join(name))
            .collect();
        for path in &expired {
            std::fs::write(path, [1]).unwrap();
        }
        // Restart runs the same retention path as scheduled cleanup and settings changes.
        let restarted = Monitor::new(dir.path().to_owned(), host.clone(), "test-1.2.3").unwrap();
        let record = restarted.capture_now().unwrap().unwrap();
        let path = restarted
            .lock()
            .unwrap()
            .storage
            .image_path(&record.id)
            .unwrap();
        {
            let mut rt = restarted.lock().unwrap();
            // Force a tiny quota in the test without allocating a 100 MB screenshot.
            rt.settings.screenshot_storage_limit_mb = 0;
            restarted.cleanup(&mut rt);
        }
        let events = host.events.lock().unwrap();
        let deletions: Vec<_> = events
            .iter()
            .filter(|event| event.kind == "retention")
            .collect();
        assert_eq!(deletions.len(), 3);
        for path in &expired {
            assert!(!path.exists());
            assert_eq!(
                deletions
                    .iter()
                    .filter(|event| event
                        .message
                        .starts_with(&format!("截图已过期，自动删除：{}\n", path.display())))
                    .count(),
                1
            );
        }
        assert!(!path.exists());
        assert!(deletions[2]
            .message
            .starts_with(&format!("截图超过空间限制，自动删除：{}\n", path.display())));
        assert!(deletions[2].message.contains("删除前截图占用：1 字节"));
        assert!(deletions[2].message.contains("删除后截图占用：0 字节"));
        assert_eq!(restarted.records(&today(), 1).unwrap().total, 0);
        assert!(!dir
            .path()
            .join("records")
            .join(today())
            .join("events.jsonl")
            .exists());
        drop(monitor);
    }

    #[test]
    fn deletion_log_failure_is_visible_without_a_local_fallback() {
        let (dir, host, monitor) = fixture();
        let record = monitor.capture_now().unwrap().unwrap();
        let path = monitor
            .lock()
            .unwrap()
            .storage
            .image_path(&record.id)
            .unwrap();
        host.fail_events.store(true, Ordering::SeqCst);
        {
            let mut rt = monitor.lock().unwrap();
            rt.settings.screenshot_storage_limit_mb = 0;
            monitor.cleanup(&mut rt);
        }
        assert!(!path.exists());
        assert_eq!(
            monitor.status().unwrap().audit_error.as_deref(),
            Some("事件日志不可写")
        );
        assert!(!dir
            .path()
            .join("records")
            .join(today())
            .join("events.jsonl")
            .exists());
    }

    #[test]
    fn stop_prevents_worker_captures_and_settings_cannot_resume_it() {
        let (_dir, host, monitor) = fixture();
        monitor.start_worker();
        assert!(!monitor.status().unwrap().settings.enabled);
        monitor.set_enabled(true).unwrap();
        for _ in 0..100 {
            if host.count.load(Ordering::SeqCst) > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        monitor.set_enabled(false).unwrap();
        let count = host.count.load(Ordering::SeqCst);
        assert!(count > 0);
        monitor
            .save_settings(Settings {
                enabled: true,
                interval_seconds: 10,
                ..Settings::default()
            })
            .unwrap();
        assert!(!monitor.status().unwrap().settings.enabled);
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(host.count.load(Ordering::SeqCst), count);
        monitor.shutdown();
    }

    #[test]
    fn locked_sessions_skip_and_audit_failure_does_not_discard_screenshots() {
        let (_dir, host, monitor) = fixture();
        monitor.set_enabled(true).unwrap();
        host.locked.store(true, Ordering::SeqCst);
        assert!(monitor.capture_now().unwrap().is_none());
        assert_eq!(monitor.status().unwrap().state, "waiting");
        assert_eq!(monitor.status().unwrap().today_count, 0);
        host.locked.store(false, Ordering::SeqCst);
        host.fail_events.store(true, Ordering::SeqCst);
        assert!(monitor.capture_now().unwrap().is_some());
        let status = monitor.status().unwrap();
        assert_eq!(status.state, "running");
        assert_eq!(status.today_count, 1);
        assert!(status.audit_error.is_some());
        host.fail.store(true, Ordering::SeqCst);
        assert!(monitor.capture_now().is_err());
        assert_eq!(monitor.status().unwrap().state, "error");
        assert_eq!(monitor.records(&today(), 1).unwrap().total, 1);
    }

    #[test]
    fn explicit_start_stop_survives_restart_and_invalid_settings_are_rejected() {
        let (dir, host, monitor) = fixture();
        monitor.set_enabled(true).unwrap();
        let restarted = Monitor::new(dir.path().to_owned(), host, "test-1.2.3").unwrap();
        assert!(restarted.status().unwrap().settings.enabled);
        assert!(monitor
            .save_settings(Settings {
                interval_seconds: 0,
                ..Settings::default()
            })
            .is_err());
        assert_eq!(monitor.status().unwrap().settings.interval_seconds, 60);
    }

    #[test]
    fn positive_intervals_persist_and_schedule_without_overflow() {
        let (dir, host, monitor) = fixture();
        for seconds in [1, 9, 3601, 7200, u64::MAX] {
            monitor
                .save_settings(Settings {
                    interval_seconds: seconds,
                    ..Settings::default()
                })
                .unwrap();
            let restarted =
                Monitor::new(dir.path().to_owned(), host.clone(), "test-1.2.3").unwrap();
            assert_eq!(
                restarted.status().unwrap().settings.interval_seconds,
                seconds
            );
            monitor.capture_now().unwrap();
            let mut rt = monitor.lock().unwrap();
            rt.capture_timer_started = Some(Instant::now() - Duration::from_secs(1));
            let delay = rt.capture_delay();
            if seconds == 1 {
                assert!(delay.is_zero());
            } else {
                assert!(!delay.is_zero());
                assert!(delay <= Duration::from_secs(seconds - 1));
            }
        }
        monitor.set_enabled(true).unwrap();
        assert!(monitor.lock().unwrap().capture_delay().is_zero());
    }

    #[test]
    fn failed_config_write_rolls_back_login_startup_and_preserves_settings() {
        let (dir, host, monitor) = fixture();
        std::fs::create_dir(dir.path().join("settings.json")).unwrap();
        let result = monitor.save_settings(Settings {
            auto_start: true,
            interval_seconds: 120,
            ..Settings::default()
        });
        assert!(result.is_err());
        assert!(!host.auto.load(Ordering::SeqCst));
        assert_eq!(monitor.status().unwrap().settings, Settings::default());
    }

    #[test]
    fn failed_metadata_write_does_not_leave_a_successful_image() {
        let (dir, host, monitor) = fixture();
        let date_dir = dir.path().join("records").join(today());
        std::fs::create_dir_all(date_dir.join("captures.jsonl")).unwrap();
        assert!(monitor.capture_now().is_err());
        let images = std::fs::read_dir(&date_dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jpg"))
            .count();
        assert_eq!(images, 0);
        assert_eq!(monitor.lock().unwrap().today_count, 0);
        assert!(monitor.lock().unwrap().last_error.is_some());
        let events = host.events.lock().unwrap();
        assert!(!events.iter().any(|e| e.kind == "captured"));
        assert!(events.iter().any(|e| e.kind == "capture_error"
            && e.message.contains("阶段：写入索引")
            && e.message.contains("captures.jsonl")));
    }

    #[test]
    fn starting_worker_twice_does_not_duplicate_immediate_captures() {
        let (_dir, host, monitor) = fixture();
        monitor.set_enabled(true).unwrap();
        monitor.start_worker();
        monitor.start_worker();
        for _ in 0..100 {
            if host.count.load(Ordering::SeqCst) > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        monitor.set_enabled(false).unwrap();
        assert_eq!(host.count.load(Ordering::SeqCst), 1);
        monitor.shutdown();
    }
}
