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
            setup_tray(app)?;
            if std::env::args().any(|arg| arg == "--background") {
                if let Some(window) = app.get_webview_window("main") {
                    window.hide()?;
                }
            }
            monitor.start_worker();
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                if let Err(error) = window.hide() {
                    if let Some(monitor) = window.try_state::<Arc<Monitor>>() {
                        monitor.report_error(format!("收起窗口失败：{error}"));
                    }
                }
            }
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

fn setup_tray(app: &mut tauri::App) -> tauri::Result<()> {
    use tauri::{
        menu::{Menu, MenuItem},
        tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    };
    let show = MenuItem::with_id(app, "show", "打开监控面板", true, None::<&str>)?;
    let start = MenuItem::with_id(app, "start", "开始监控", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "stop", "停止监控", true, None::<&str>)?;
    let folder = MenuItem::with_id(app, "folder", "打开记录目录", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出客户端", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &start, &stop, &folder, &quit])?;
    TrayIconBuilder::with_id("monitor-tray")
        .icon(
            app.default_window_icon()
                .ok_or_else(|| tauri::Error::AssetNotFound("应用图标".into()))?
                .clone(),
        )
        .tooltip("宜测安全监控")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| {
            let Some(state) = app.try_state::<Arc<Monitor>>() else {
                return;
            };
            let monitor = state.inner().clone();
            match event.id().as_ref() {
                "show" => {
                    show_main_window(app);
                }
                "start" | "stop" | "folder" => {
                    let action = event.id().as_ref().to_string();
                    let app = app.clone();
                    tauri::async_runtime::spawn_blocking(move || {
                        let result = match action.as_str() {
                            "start" => monitor.set_enabled(true),
                            "stop" => monitor.set_enabled(false),
                            _ => monitor.open_folder(),
                        };
                        if let Err(error) = result {
                            monitor.report_error(format!("托盘操作失败：{error}"));
                            show_main_window(&app);
                        }
                    });
                }
                "quit" => {
                    monitor.shutdown();
                    app.exit(0);
                }
                _ => {}
            }
        })
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } | TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                }
            ) {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}
