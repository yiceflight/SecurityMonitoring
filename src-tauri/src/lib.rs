use monitor_core::{CaptureRecord, Monitor, RecordPage, Settings, Status};
use std::sync::Arc;
use tauri::{Manager, State};

#[tauri::command]
async fn get_status(monitor: State<'_, Arc<Monitor>>) -> Result<Status, String> {
    let monitor = monitor.inner().clone();
    blocking(move || monitor.status()).await
}

#[tauri::command]
async fn set_monitoring_enabled(
    enabled: bool,
    monitor: State<'_, Arc<Monitor>>,
) -> Result<Status, String> {
    let monitor = monitor.inner().clone();
    blocking(move || {
        monitor.set_enabled(enabled)?;
        monitor.status()
    })
    .await
}

#[tauri::command]
async fn save_settings(
    settings: Settings,
    monitor: State<'_, Arc<Monitor>>,
) -> Result<Status, String> {
    let monitor = monitor.inner().clone();
    blocking(move || {
        monitor.save_settings(settings)?;
        monitor.status()
    })
    .await
}

#[tauri::command]
async fn capture_now(monitor: State<'_, Arc<Monitor>>) -> Result<Option<CaptureRecord>, String> {
    let monitor = monitor.inner().clone();
    blocking(move || monitor.capture_now()).await
}

#[tauri::command]
async fn list_records(
    date: String,
    page: usize,
    monitor: State<'_, Arc<Monitor>>,
) -> Result<RecordPage, String> {
    let monitor = monitor.inner().clone();
    blocking(move || monitor.records(&date, page)).await
}

#[tauri::command]
async fn list_replay_records(
    monitor: State<'_, Arc<Monitor>>,
) -> Result<Vec<CaptureRecord>, String> {
    let monitor = monitor.inner().clone();
    blocking(move || monitor.replay_records()).await
}

#[tauri::command]
async fn get_record_image(id: String, monitor: State<'_, Arc<Monitor>>) -> Result<String, String> {
    let monitor = monitor.inner().clone();
    blocking(move || monitor.image_data(&id)).await
}

#[tauri::command]
async fn open_record(id: String, monitor: State<'_, Arc<Monitor>>) -> Result<(), String> {
    let monitor = monitor.inner().clone();
    blocking(move || monitor.open_record(&id)).await
}

#[tauri::command]
async fn open_records_folder(monitor: State<'_, Arc<Monitor>>) -> Result<(), String> {
    let monitor = monitor.inner().clone();
    blocking(move || monitor.open_folder()).await
}

#[tauri::command]
async fn register_event_source(monitor: State<'_, Arc<Monitor>>) -> Result<Status, String> {
    let monitor = monitor.inner().clone();
    blocking(move || {
        monitor.register_event_source()?;
        monitor.status()
    })
    .await
}

async fn blocking<T: Send + 'static>(
    action: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(action)
        .await
        .map_err(|e| e.to_string())?
}

pub fn run() {
    let builder = tauri::Builder::default();
    #[cfg(windows)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, args, _| {
        if !args.iter().any(|arg| arg == "--background") {
            show_main_window(app);
        }
    }));
    let app = builder
        .invoke_handler(tauri::generate_handler![
            get_status,
            set_monitoring_enabled,
            save_settings,
            capture_now,
            list_records,
            list_replay_records,
            get_record_image,
            open_record,
            open_records_folder,
            register_event_source
        ])
        .setup(|app| {
            let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
            let monitor =
                Monitor::new(root, monitor_core::native_host(), env!("CARGO_PKG_VERSION"))
                    .map_err(std::io::Error::other)?;
            app.manage(Arc::clone(&monitor));
            monitor.start_worker();
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building application");

    app.run(|app, event| match event {
        tauri::RunEvent::Exit => {
            if let Some(monitor) = app.try_state::<Arc<Monitor>>() {
                monitor.shutdown();
            }
        }
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen { .. } => show_main_window(app),
        _ => {}
    });
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
