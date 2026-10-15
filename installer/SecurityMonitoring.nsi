; Build inputs are produced by scripts/build-unified-installer.ps1.
; The launcher is the only shortcut the user sees. It selects the legacy
; Win32 client on XP/7/8 and the Tauri client on Windows 10/11.
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
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\SecurityMonitoring"
  DeleteRegKey HKLM "SYSTEM\CurrentControlSet\Services\EventLog\System\SecurityMonitoring"
SectionEnd
