# Legacy Windows client

`SecurityMonitoringLegacy.exe` is the compatibility client for Windows XP SP3 x86,
Windows 7, Windows 8/8.1, older Windows 10 builds below 1803 and 32-bit
Windows 10. It is deliberately a
small 32-bit Win32 program:
it does not load WebView2, .NET, Node.js or Rust at runtime.

The legacy client uses the same configuration root and file format as the Tauri
client. Screenshots are round-robin distributed across locally fixed drives;
the application-data drive keeps the existing path and other fixed drives use
`X:\SecurityMonitoring\records`. Removable, hot-pluggable and network drives
are excluded.

```text
%APPDATA%\com.securitymonitoring.app\settings.json
%APPDATA%\com.securitymonitoring.app\records\YYYY-MM-DD\captures.jsonl
%APPDATA%\com.securitymonitoring.app\records\YYYY-MM-DD\*.jpg
X:\SecurityMonitoring\records\YYYY-MM-DD\captures.jsonl
X:\SecurityMonitoring\records\YYYY-MM-DD\*.jpg
```

Build it on Windows with Visual Studio 2017 or newer and the v141 XP toolset.
The supplied CMake file sets the Win32 subsystem version to 5.01 and links
only against XP-era system libraries.

The launcher and client use the static MSVC runtime (`/MT`), so the installer
does not require a separate Visual C++ Redistributable package on XP. The
unified installer carries the WebView2 x64 offline installer for the Modern
client; XP, Windows 7 and Windows 8/8.1 never launch or install that runtime.

This client is intentionally a compatibility path. The modern Tauri client is
used by the launcher only on 64-bit Windows 10 1803+ and Windows 11.
