# Legacy Windows client

`SecurityMonitoringLegacy.exe` is the compatibility client for Windows XP SP3,
Windows 7 and Windows 8/8.1 (and 32-bit Windows 10). It is deliberately a
small 32-bit Win32 program:
it does not load WebView2, .NET, Node.js or Rust at runtime.

The legacy client uses the same data root and file format as the Tauri client:

```text
%APPDATA%\com.securitymonitoring.app\settings.json
%APPDATA%\com.securitymonitoring.app\records\YYYY-MM-DD\captures.jsonl
%APPDATA%\com.securitymonitoring.app\records\YYYY-MM-DD\*.jpg
```

Build it on Windows with Visual Studio 2017 or newer and the v141 XP toolset.
The supplied CMake file sets the Win32 subsystem version to 5.01 and links
only against XP-era system libraries.

The launcher and client use the static MSVC runtime (`/MT`), so the installer
does not require a separate Visual C++ Redistributable package on XP.

This client is intentionally a compatibility path. The modern Tauri client is
still used on Windows 10 and 11 by the launcher.
