; Build inputs are produced by scripts/build-unified-installer.ps1.
; The launcher is the only shortcut the user sees. It selects the legacy
; Win32 client on XP/7/8/older Win10 and the Tauri client on Win10 1803+/11.
!include WinVer.nsh
Unicode true
SetCompressor /SOLID lzma
Name "安全监控"
OutFile "build\SecurityMonitoringSetup.exe"
InstallDir "$PROGRAMFILES\SecurityMonitoring"
RequestExecutionLevel admin

Page directory
Page instfiles
UninstPage uninstConfirm
UninstPage instfiles

Section "Application"
  SetOutPath "$INSTDIR"
  File "build\SecurityMonitoring.exe"
  File "build\SecurityMonitoringLegacy.exe"
  File "build\SecurityMonitoringModern.exe"
  ; Keep the unified installer usable on an offline Windows 10/11 machine
  ; where WebView2 is absent. XP/7/8 use Legacy and skip this installer.
  File "build\MicrosoftEdgeWebView2RuntimeInstallerX64.exe"
  ${If} ${AtLeastWin10}
    ExecWait '"$INSTDIR\MicrosoftEdgeWebView2RuntimeInstallerX64.exe" /silent /install'
  ${EndIf}
  Delete "$INSTDIR\MicrosoftEdgeWebView2RuntimeInstallerX64.exe"
  CreateDirectory "$SMPROGRAMS\安全监控"
  CreateShortCut "$DESKTOP\安全监控.lnk" "$INSTDIR\SecurityMonitoring.exe"
  CreateShortCut "$SMPROGRAMS\安全监控\安全监控.lnk" "$INSTDIR\SecurityMonitoring.exe"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\SecurityMonitoring" "DisplayName" "安全监控"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\SecurityMonitoring" "UninstallString" "$INSTDIR\uninstall.exe"
  ; The legacy client writes to this source without PowerShell, which is not
  ; available on the XP baseline. The modern client keeps its existing source
  ; registration flow for upgrades and systems where it is already installed.
  WriteRegStr HKLM "SYSTEM\CurrentControlSet\Services\EventLog\System\SecurityMonitoring" "EventMessageFile" "$SYSDIR\eventlog.dll"
  WriteRegDWORD HKLM "SYSTEM\CurrentControlSet\Services\EventLog\System\SecurityMonitoring" "TypesSupported" 7
SectionEnd

Section "Uninstall"
  Delete "$DESKTOP\安全监控.lnk"
  Delete "$SMPROGRAMS\安全监控\安全监控.lnk"
  RMDir "$SMPROGRAMS\安全监控"
  Delete "$INSTDIR\SecurityMonitoring.exe"
  Delete "$INSTDIR\SecurityMonitoringLegacy.exe"
  Delete "$INSTDIR\SecurityMonitoringModern.exe"
  Delete "$INSTDIR\MicrosoftEdgeWebView2RuntimeInstallerX64.exe"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\SecurityMonitoring"
  DeleteRegKey HKLM "SYSTEM\CurrentControlSet\Services\EventLog\System\SecurityMonitoring"
SectionEnd
