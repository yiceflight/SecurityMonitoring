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
    count_date: String,
    today_count: usize,
    cleanup_date: String,
    next_capture: Instant,
    shutdown: bool,
}

pub struct Monitor {
    host: Arc<dyn Host>,
    runtime: Mutex<Runtime>,
    wake: Condvar,
    worker_started: std::sync::atomic::AtomicBool,
}

impl Monitor {
    pub fn new(root: PathBuf, host: Arc<dyn Host>) -> Result<Arc<Self>> {
        let storage = Storage::new(root)?;
        let (settings, config_error) = match storage.load_settings() {
            Ok(settings) => (settings, None),
            Err(error) => (Settings::default(), Some(error)),
        };
        let last_capture = storage.recent_capture()?;
        let today_count = storage.records(&today(), 1)?.total;
        let monitor = Arc::new(Self {
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
                count_date: today(),
                today_count,
                cleanup_date: String::new(),
                next_capture: Instant::now(),
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
            monitor.audit(&mut rt, "startup", "客户端启动");
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
                if monitor.host.supported()
                    && rt.settings.enabled
                    && Instant::now() >= rt.next_capture
                {
                    let _ = monitor.capture(&mut rt);
                    rt.next_capture =
                        Instant::now() + Duration::from_secs(rt.settings.interval_seconds);
                }
                let delay = if rt.settings.enabled && monitor.host.supported() {
                    rt.next_capture.saturating_duration_since(Instant::now())
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
        rt.last_error = None;
        rt.next_capture = Instant::now();
        self.audit(
            &mut rt,
            if enabled { "started" } else { "stopped" },
            if enabled {
                "开始监控"
            } else {
                "停止监控"
            },
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
        rt.settings = settings;
        rt.auto_start_error = None;
        rt.next_capture = Instant::now() + Duration::from_secs(rt.settings.interval_seconds);
        let message = format!(
            "设置已保存：截图间隔 {} 秒，保留 {} 天，截图空间 {} MB，登录启动 {}",
            rt.settings.interval_seconds,
            rt.settings.retention_days,
            rt.settings.screenshot_storage_limit_mb,
            rt.settings.auto_start
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
        let result = self.capture(&mut rt);
        rt.next_capture = Instant::now() + Duration::from_secs(rt.settings.interval_seconds);
        self.wake.notify_one();
        result
    }

    fn capture(&self, rt: &mut Runtime) -> Result<Option<CaptureRecord>> {
        self.rollover(rt)?;
        self.cleanup(rt);
        match self.host.capture().and_then(|outcome| match outcome {
            CaptureOutcome::Captured(captured) => rt.storage.save_capture(captured).map(Some),
            CaptureOutcome::Unavailable(reason) => {
                if rt.waiting.as_ref() != Some(&reason) {
                    self.audit(rt, "skipped", &reason);
                }
                rt.waiting = Some(reason);
                Ok(None)
            }
        }) {
            Ok(Some(record)) => {
                rt.last_error = None;
                rt.waiting = None;
                let path = rt.storage.image_path(&record.id)?;
                self.audit(rt, "captured", &format!("截图：{}", path.display()));
                self.cleanup(rt);
                rt.today_count = rt.storage.records(&today(), 1)?.total;
                rt.last_capture = rt.storage.recent_capture()?;
                Ok(Some(record))
            }
            Ok(None) => {
                rt.last_error = None;
                Ok(None)
            }
            Err(error) => {
                rt.last_error = Some(error.clone());
                rt.waiting = None;
                self.audit(rt, "error", &error);
                Err(error)
            }
        }
    }

    fn write_event(&self, kind: &str, message: &str) -> Result<()> {
        self.host.write_event(&AuditEvent {
            created_at: Local::now().to_rfc3339(),
            kind: kind.into(),
            message: message.into(),
        })
    }

    fn audit(&self, rt: &mut Runtime, kind: &str, message: &str) {
        rt.audit_error = self.write_event(kind, message).err();
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
            match rt.storage.prune(rt.settings.retention_days, |path| {
                if let Err(error) = self.write_event(
                    "retention",
                    &format!("截图已过期，自动删除：{}", path.display()),
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
                .prune_storage(rt.settings.screenshot_storage_limit_mb, |path| {
                    if let Err(error) = self.write_event(
                        "retention",
                        &format!("截图超过空间限制，自动删除：{}", path.display()),
                    ) {
                        audit_errors.push(error);
                    }
                })
        {
            errors.push(error);
        }
        rt.cleanup_error = if errors.is_empty() {
            None
        } else {
            Some(errors.join("；"))
        };
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
        self.audit(&mut rt, "settings", "Windows 事件日志源已注册");
        Ok(())
    }
    pub fn shutdown(&self) {
        if let Ok(mut rt) = self.lock() {
            if rt.shutdown {
                return;
            }
            self.audit(&mut rt, "shutdown", "客户端退出，采集调度已停止");
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
        let monitor = Monitor::new(dir.path().to_owned(), host.clone()).unwrap();
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
            assert_eq!(event.message, format!("截图：{}", path.display()));
        }
        assert!(!dir
            .path()
            .join("records")
            .join(today())
            .join("events.jsonl")
            .exists());
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
        let restarted = Monitor::new(dir.path().to_owned(), host.clone()).unwrap();
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
                    .filter(|event| event.message
                        == format!("截图已过期，自动删除：{}", path.display()))
                    .count(),
                1
            );
        }
        assert!(!path.exists());
        assert_eq!(
            deletions[2].message,
            format!("截图超过空间限制，自动删除：{}", path.display())
        );
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
        let restarted = Monitor::new(dir.path().to_owned(), host).unwrap();
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
        let (dir, _host, monitor) = fixture();
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
