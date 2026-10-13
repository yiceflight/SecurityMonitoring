use crate::{AuditEvent, CaptureOutcome, Host, Result};
use image::{codecs::jpeg::JpegEncoder, ColorType};
use std::{ffi::OsStr, io::Cursor, os::windows::ffi::OsStrExt, path::Path, ptr};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HWND},
    Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
        SRCCOPY,
    },
    System::{
        EventLog::{
            DeregisterEventSource, RegisterEventSourceW, ReportEventW, EVENTLOG_ERROR_TYPE,
            EVENTLOG_INFORMATION_TYPE,
        },
        SystemInformation::GetTickCount,
        Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION},
    },
    UI::{
        Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO},
        WindowsAndMessaging::{
            GetForegroundWindow, GetSystemMetrics, GetWindowTextW, GetWindowThreadProcessId,
            SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
        },
    },
};
use winreg::{enums::HKEY_CURRENT_USER, RegKey};

pub struct WindowsHost;

impl Host for WindowsHost {
    fn session_context(&self) -> String {
        use windows_sys::Win32::System::{
            RemoteDesktop::ProcessIdToSessionId, WindowsProgramming::GetUserNameW,
        };
        let mut username = [0u16; 257];
        let mut size = username.len() as u32;
        let user = if unsafe { GetUserNameW(username.as_mut_ptr(), &mut size) } != 0 {
            format!(
                "{:?}",
                String::from_utf16_lossy(&username[..size.saturating_sub(1) as usize])
            )
        } else {
            format!("不可用（{}）", std::io::Error::last_os_error())
        };
        let mut session = 0;
        let session = if unsafe { ProcessIdToSessionId(std::process::id(), &mut session) } != 0 {
            session.to_string()
        } else {
            format!("不可用（{}）", std::io::Error::last_os_error())
        };
        format!("用户：{user}\n会话 ID：{session}")
    }

    fn supported(&self) -> bool {
        true
    }

    fn capture(&self) -> Result<CaptureOutcome> {
        if !input_desktop_available() {
            return Ok(CaptureOutcome::Unavailable(
                "当前为锁屏或无交互桌面，已暂停截图".into(),
            ));
        }
        let (jpeg, width, height) = capture_screen()?;
        if !input_desktop_available() {
            return Ok(CaptureOutcome::Unavailable(
                "桌面已锁定，本次截图已丢弃".into(),
            ));
        }
        let hwnd = unsafe { GetForegroundWindow() };
        let (window_title, process_name) = foreground_details(hwnd);
        Ok(CaptureOutcome::Captured(crate::Captured {
            jpeg,
            width,
            height,
            window_title,
            process_name,
            idle_seconds: idle_seconds(),
        }))
    }

    fn auto_start(&self) -> Result<bool> {
        let key = match RegKey::predef(HKEY_CURRENT_USER).open_subkey(RUN_KEY) {
            Ok(key) => key,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.to_string()),
        };
        Ok(key.get_value::<String, _>(APP_NAME).ok().as_deref() == Some(&auto_start_command()?))
    }

    fn set_auto_start(&self, enabled: bool) -> Result<()> {
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey(RUN_KEY)
            .map_err(|e| format!("无法设置登录启动：{e}"))?;
        if enabled {
            key.set_value(APP_NAME, &auto_start_command()?)
                .map_err(|e| format!("无法写入登录启动：{e}"))?;
        } else {
            if let Err(error) = key.delete_value(APP_NAME) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    return Err(format!("无法关闭登录启动：{error}"));
                }
            }
        }
        Ok(())
    }

    fn event_source_registered(&self) -> bool {
        RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE)
            .open_subkey(format!(
                "SYSTEM\\CurrentControlSet\\Services\\EventLog\\{EVENT_LOG_NAME}\\{EVENT_SOURCE_NAME}"
            ))
            .is_ok()
    }

    fn write_event(&self, event: &AuditEvent) -> Result<()> {
        if !self.event_source_registered() {
            return Err(
                "Windows 系统日志源尚未注册，请点击“注册日志源”；本次系统日志未写入".into(),
            );
        }
        let source = wides(EVENT_SOURCE_NAME);
        let handle = unsafe { RegisterEventSourceW(ptr::null(), source.as_ptr()) };
        if handle.is_null() {
            return Err(format!(
                "Windows 事件日志源未注册（请用管理员权限安装客户端）：{}",
                std::io::Error::last_os_error()
            ));
        }
        let text = wides(&format!(
            "事件时间：{}\n{}",
            event.created_at, event.message
        ));
        let strings = [text.as_ptr()];
        let event_type = if event.is_error() {
            EVENTLOG_ERROR_TYPE
        } else {
            EVENTLOG_INFORMATION_TYPE
        };
        let ok = unsafe {
            ReportEventW(
                handle,
                event_type,
                0,
                event.event_id(),
                ptr::null_mut(),
                1,
                0,
                strings.as_ptr(),
                ptr::null(),
            )
        } != 0;
        // Capture the error before closing the handle, which may alter last-error.
        let error = (!ok).then(std::io::Error::last_os_error);
        unsafe {
            DeregisterEventSource(handle);
        }
        if ok {
            Ok(())
        } else {
            Err(format!("无法写入 Windows 事件日志：{}", error.unwrap()))
        }
    }

    fn open_path(&self, path: &Path) -> Result<()> {
        let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let open = wides("open");
        let result = unsafe {
            windows_sys::Win32::UI::Shell::ShellExecuteW(
                ptr::null_mut(),
                open.as_ptr(),
                path.as_ptr(),
                ptr::null(),
                ptr::null(),
                1,
            )
        } as isize;
        if result > 32 {
            Ok(())
        } else {
            Err(format!("无法打开记录，Windows 错误码 {result}"))
        }
    }

    fn register_event_source(&self) -> Result<()> {
        if self.event_source_registered() {
            return Ok(());
        }
        use base64::Engine;
        use windows_sys::Win32::{
            Foundation::WAIT_OBJECT_0,
            System::Threading::{GetExitCodeProcess, WaitForSingleObject},
            UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW},
        };
        // A fixed, embedded script. No caller-provided arguments or writable script files
        // are executed as administrator. This installs only this application's source.
        let script = format!(
            r#"$ErrorActionPreference = 'Stop'
try {{
    $source = '{EVENT_SOURCE_NAME}'
    $log = '{EVENT_LOG_NAME}'
    if ([System.Diagnostics.EventLog]::SourceExists($source)) {{
        if ([System.Diagnostics.EventLog]::LogNameFromSourceName($source, '.') -ne $log) {{
            throw 'Event source is registered to a different log'
        }}
    }} else {{
        New-EventLog -LogName $log -Source $source
    }}
    Write-EventLog -LogName $log -Source $source -EventId {EVENT_ID} -EntryType Information -Message 'YiceSecurityMonitoring System event source registered'
    exit 0
}} catch {{
    exit 1
}}"#
        );
        let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let parameters = wides(&format!(
            "-NoProfile -NonInteractive -EncodedCommand {}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ));
        let powershell = wides(&format!(
            "{}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
            std::env::var("SystemRoot").map_err(|e| e.to_string())?
        ));
        let runas = wides("runas");
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            lpVerb: runas.as_ptr(),
            lpFile: powershell.as_ptr(),
            lpParameters: parameters.as_ptr(),
            nShow: 0,
            ..Default::default()
        };
        if unsafe { ShellExecuteExW(&mut info) } == 0 {
            return Err(format!(
                "日志源注册未完成（可能取消了管理员授权）：{}",
                std::io::Error::last_os_error()
            ));
        }
        if info.hProcess.is_null() {
            return Err("无法读取日志源注册结果".into());
        }
        let waited = unsafe { WaitForSingleObject(info.hProcess, 60_000) };
        let mut code = 1;
        let read = unsafe { GetExitCodeProcess(info.hProcess, &mut code) } != 0;
        unsafe {
            CloseHandle(info.hProcess);
        }
        if waited != WAIT_OBJECT_0 {
            return Err("日志源注册尚未完成，请稍后刷新确认".into());
        }
        if !read || code != 0 || !self.event_source_registered() {
            return Err("日志源注册失败，请检查管理员权限及 Windows 事件日志服务".into());
        }
        Ok(())
    }
}

const APP_NAME: &str = "YiceSecurityMonitoring";
const EVENT_LOG_NAME: &str = "System";
// Use a new source because Windows caches source-to-log mappings. Reassigning the
// legacy Application source requires a reboot; leave it intact for old entries.
const EVENT_SOURCE_NAME: &str = "YiceSecurityMonitoring.System";
const EVENT_ID: u32 = 1000;
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";

fn wides(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}
fn auto_start_command() -> Result<String> {
    Ok(format!(
        "\"{}\" --background",
        std::env::current_exe()
            .map_err(|e| e.to_string())?
            .display()
    ))
}

fn input_desktop_available() -> bool {
    use windows_sys::Win32::System::{
        RemoteDesktop::{
            WTSActive, WTSConnectState, WTSFreeMemory, WTSQuerySessionInformationW,
            WTS_CURRENT_SESSION,
        },
        StationsAndDesktops::{
            CloseDesktop, GetUserObjectInformationW, OpenInputDesktop, DESKTOP_READOBJECTS,
            UOI_NAME,
        },
    };
    let mut buffer = ptr::null_mut();
    let mut size = 0;
    if unsafe {
        WTSQuerySessionInformationW(
            ptr::null_mut(),
            WTS_CURRENT_SESSION,
            WTSConnectState,
            &mut buffer,
            &mut size,
        )
    } == 0
    {
        return false;
    }
    let active = !buffer.is_null()
        && size >= 4
        && unsafe { ptr::read_unaligned(buffer.cast::<i32>()) } == WTSActive;
    unsafe {
        WTSFreeMemory(buffer.cast());
    }
    if !active {
        return false;
    }
    let desktop = unsafe { OpenInputDesktop(0, 0, DESKTOP_READOBJECTS) };
    if desktop.is_null() {
        return false;
    }
    let mut name = [0u16; 256];
    let mut needed = 0;
    let ok = unsafe {
        GetUserObjectInformationW(
            desktop,
            UOI_NAME,
            name.as_mut_ptr().cast(),
            std::mem::size_of_val(&name) as u32,
            &mut needed,
        )
    } != 0;
    unsafe {
        CloseDesktop(desktop);
    }
    let length = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    ok && String::from_utf16_lossy(&name[..length]).eq_ignore_ascii_case("Default")
}

fn capture_screen() -> Result<(Vec<u8>, u32, u32)> {
    let x = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let y = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    let width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    let height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
    if width <= 0 || height <= 0 {
        return Err(format!(
            "阶段：采集；无法读取 Windows 虚拟屏幕尺寸：{width} × {height}"
        ));
    }
    let pixel_count = (width as usize)
        .checked_mul(height as usize)
        .filter(|&count| count <= 40_000_000)
        .ok_or("屏幕总分辨率超过 4000 万像素，无法安全分配截图缓冲区")?;
    let screen = unsafe { GetDC(ptr::null_mut()) };
    if screen.is_null() {
        return Err("阶段：采集；GetDC 无法打开桌面设备上下文".into());
    }
    let memory = unsafe { CreateCompatibleDC(screen) };
    let bitmap = unsafe { CreateCompatibleBitmap(screen, width, height) };
    if memory.is_null() || bitmap.is_null() {
        if !memory.is_null() {
            unsafe {
                DeleteDC(memory);
            }
        }
        if !bitmap.is_null() {
            unsafe {
                DeleteObject(bitmap);
            }
        }
        unsafe {
            ReleaseDC(ptr::null_mut(), screen);
        }
        return Err(format!("阶段：采集；CreateCompatibleDC / CreateCompatibleBitmap 无法创建截图缓冲区；尺寸：{width} × {height}"));
    }
    let previous = unsafe { SelectObject(memory, bitmap as _) };
    let selected = !previous.is_null() && previous as isize != -1;
    let copied = selected
        && unsafe {
            BitBlt(
                memory,
                0,
                0,
                width,
                height,
                screen,
                x,
                y,
                SRCCOPY | windows_sys::Win32::Graphics::Gdi::CAPTUREBLT,
            )
        } != 0;
    let copy_error = if !selected {
        Some("SelectObject 选择截图缓冲区失败".to_string())
    } else if !copied {
        Some(format!("BitBlt 失败：{}", std::io::Error::last_os_error()))
    } else {
        None
    };
    // GetDIBits requires the bitmap to be deselected from every device context.
    unsafe {
        SelectObject(memory, previous);
    }
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bgra = vec![0u8; pixel_count * 4];
    let rows = if copied {
        unsafe {
            GetDIBits(
                screen,
                bitmap,
                0,
                height as u32,
                bgra.as_mut_ptr().cast(),
                &mut info,
                DIB_RGB_COLORS,
            )
        }
    } else {
        0
    };
    let read_error = if copied && rows == 0 {
        Some(format!(
            "GetDIBits 失败：{}",
            std::io::Error::last_os_error()
        ))
    } else {
        None
    };
    unsafe {
        DeleteObject(bitmap as _);
        DeleteDC(memory);
        ReleaseDC(ptr::null_mut(), screen);
    }
    if rows != height {
        return Err(format!(
            "阶段：采集；{}；尺寸：{} × {}；已读取行数：{}",
            copy_error
                .or(read_error)
                .unwrap_or_else(|| "GetDIBits 未读取完整屏幕".into()),
            width,
            height,
            rows
        ));
    }
    let rgb: Vec<u8> = bgra
        .chunks_exact(4)
        .flat_map(|pixel| [pixel[2], pixel[1], pixel[0]])
        .collect();
    let mut jpeg = Cursor::new(Vec::new());
    JpegEncoder::new_with_quality(&mut jpeg, 82)
        .encode(&rgb, width as u32, height as u32, ColorType::Rgb8.into())
        .map_err(|e| format!("阶段：JPEG 编码；{e}"))?;
    Ok((jpeg.into_inner(), width as u32, height as u32))
}

fn foreground_details(hwnd: HWND) -> (String, String) {
    let mut title = vec![0u16; 512];
    let len = unsafe { GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32) };
    let title = String::from_utf16_lossy(&title[..len.max(0) as usize]);
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, &mut pid);
    }
    let process_name = if pid == 0 {
        String::new()
    } else {
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            String::new()
        } else {
            let mut path = vec![0u16; 1024];
            let mut length = path.len() as u32;
            let ok =
                unsafe { QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut length) }
                    != 0;
            unsafe {
                CloseHandle(process);
            }
            if ok {
                std::path::PathBuf::from(String::from_utf16_lossy(&path[..length as usize]))
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .to_string()
            } else {
                String::new()
            }
        }
    };
    (title, process_name)
}

fn idle_seconds() -> u32 {
    let mut info = LASTINPUTINFO {
        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    if unsafe { GetLastInputInfo(&mut info) } == 0 {
        return 0;
    }
    unsafe { GetTickCount().wrapping_sub(info.dwTime) / 1000 }
}
