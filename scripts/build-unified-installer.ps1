param(
  [string]$Configuration = "Release",
  [string]$WebView2OfflineInstaller = ""
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
$Legacy = Join-Path $Root "legacy-win32"
$Build = Join-Path $Legacy "build"
$WebView2FileName = "MicrosoftEdgeWebView2RuntimeInstallerX64.exe"

if (-not (Get-Command cmake -ErrorAction SilentlyContinue)) {
  throw "需要在 Windows 上安装 CMake。"
}
if (-not (Get-Command makensis -ErrorAction SilentlyContinue)) {
  throw "需要安装 NSIS，并让 makensis.exe 出现在 PATH。"
}

New-Item -ItemType Directory -Force $Build | Out-Null

if ([string]::IsNullOrWhiteSpace($WebView2OfflineInstaller)) {
  $WebView2OfflineInstaller = $env:SECURITY_MONITORING_WEBVIEW2_OFFLINE_INSTALLER
}
if ([string]::IsNullOrWhiteSpace($WebView2OfflineInstaller)) {
  $WebView2OfflineInstaller = Join-Path $Build $WebView2FileName
}
if (-not (Test-Path $WebView2OfflineInstaller)) {
  throw "缺少 WebView2 Evergreen Standalone x64 离线安装器。请从 Microsoft 官方 WebView2 下载页下载 MicrosoftEdgeWebView2RuntimeInstallerX64.exe，并通过 -WebView2OfflineInstaller 或 SECURITY_MONITORING_WEBVIEW2_OFFLINE_INSTALLER 提供路径。统一安装器脚本不会把联网 bootstrapper 当作离线包。"
}
$webView2Length = (Get-Item $WebView2OfflineInstaller).Length
if ($webView2Length -lt 50MB) {
  throw "WebView2 文件过小（$webView2Length 字节），它可能是联网 bootstrapper 而不是 Evergreen Standalone x64 离线安装器：$WebView2OfflineInstaller"
}

$cmakeArgs = @("-S", $Legacy, "-B", $Build, "-A", "Win32", "-DCMAKE_GENERATOR_PLATFORM=Win32")
if ($env:SECURITY_MONITORING_CMAKE_TOOLSET) {
  $cmakeArgs += @("-T", $env:SECURITY_MONITORING_CMAKE_TOOLSET)
}
cmake @cmakeArgs
cmake --build $Build --config $Configuration

$launcherRelease = Join-Path $Build "Release\SecurityMonitoring.exe"
if (-not (Test-Path $launcherRelease)) {
  throw "未找到启动器。请检查 legacy-win32 的 CMake 构建是否同时生成 SecurityMonitoring.exe。"
}

$modern = Join-Path $Root "src-tauri\target\release\security-monitoring.exe"
if (-not (Test-Path $modern)) {
  throw "请先执行 pnpm desktop:build，再生成统一安装包。"
}

Copy-Item $launcherRelease (Join-Path $Build "SecurityMonitoring.exe") -Force
Copy-Item (Join-Path $Build "Release\SecurityMonitoringLegacy.exe") (Join-Path $Build "SecurityMonitoringLegacy.exe") -Force
Copy-Item $modern (Join-Path $Build "SecurityMonitoringModern.exe") -Force
Copy-Item $WebView2OfflineInstaller (Join-Path $Build $WebView2FileName) -Force
Push-Location $Root
try {
  & makensis (Join-Path $Root "installer\SecurityMonitoring.nsi")
} finally {
  Pop-Location
}
Write-Host "统一安装包已生成：$Build\SecurityMonitoringSetup.exe"
