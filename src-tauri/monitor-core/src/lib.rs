mod engine;
mod storage;
#[cfg(windows)]
mod windows;

pub use engine::Monitor;
use serde::{Deserialize, Serialize};
use std::path::Path;
pub use storage::{AuditEvent, CaptureRecord, RecordPage};

pub type Result<T> = std::result::Result<T, String>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct Settings {
    pub interval_seconds: u64,
    pub retention_days: u32,
    pub auto_start: bool,
    pub enabled: bool,
    /// Soft maximum storage reserved for JPEG screenshots, in MiB.
    ///
    /// There is no configured upper bound. The monitor also protects the
    /// underlying volume by removing old screenshots when less than 100 MiB
    /// remains available.
    pub screenshot_storage_limit_mb: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            interval_seconds: 60,
            retention_days: 30,
            auto_start: false,
            enabled: false,
            screenshot_storage_limit_mb: 1024,
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<()> {
        if self.interval_seconds < 1 {
            return Err("截屏间隔须为至少 1 秒的整数".into());
        }
        if !(1..=365).contains(&self.retention_days) {
            return Err("保留时间须为 1–365 天".into());
        }
        if self.screenshot_storage_limit_mb < 100 {
            return Err("截图空间限制须为至少 100 MB".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub supported: bool,
    pub settings: Settings,
    pub state: String,
    pub storage_path: String,
    pub today_count: usize,
    pub last_capture: Option<CaptureRecord>,
    pub last_error: Option<String>,
    pub audit_error: Option<String>,
    pub auto_start_actual: bool,
    pub event_source_registered: bool,
}

pub struct Captured {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub window_title: String,
    pub process_name: String,
    pub idle_seconds: u32,
}

pub enum CaptureOutcome {
    Captured(Captured),
    Unavailable(String),
}

/// No non-Windows capture implementation is provided. Tests use an injected host.
pub trait Host: Send + Sync {
    fn supported(&self) -> bool;
    fn capture(&self) -> Result<CaptureOutcome>;
    fn auto_start(&self) -> Result<bool>;
    fn set_auto_start(&self, enabled: bool) -> Result<()>;
    fn event_source_registered(&self) -> bool;
    fn write_event(&self, event: &AuditEvent) -> Result<()>;
    fn open_path(&self, path: &Path) -> Result<()>;
    fn register_event_source(&self) -> Result<()>;
    fn session_context(&self) -> String {
        "用户：不可用\n会话 ID：不可用".into()
    }
}

pub fn native_host() -> std::sync::Arc<dyn Host> {
    #[cfg(windows)]
    {
        std::sync::Arc::new(windows::WindowsHost)
    }
    #[cfg(not(windows))]
    {
        std::sync::Arc::new(UnsupportedHost)
    }
}

#[cfg(not(windows))]
struct UnsupportedHost;
#[cfg(not(windows))]
impl Host for UnsupportedHost {
    fn supported(&self) -> bool {
        false
    }
    fn capture(&self) -> Result<CaptureOutcome> {
        Err("监控仅支持 Windows".into())
    }
    fn auto_start(&self) -> Result<bool> {
        Ok(false)
    }
    fn set_auto_start(&self, _: bool) -> Result<()> {
        Err("自动启动仅支持 Windows".into())
    }
    fn event_source_registered(&self) -> bool {
        false
    }
    fn write_event(&self, _: &AuditEvent) -> Result<()> {
        Err("事件日志仅支持 Windows".into())
    }
    fn open_path(&self, _: &Path) -> Result<()> {
        Err("打开记录仅支持 Windows".into())
    }
    fn register_event_source(&self) -> Result<()> {
        Err("事件日志仅支持 Windows".into())
    }
}
