#define UNICODE
#define _UNICODE
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <shellapi.h>

#include <string>

namespace {
std::wstring DirectoryOfModule() {
    wchar_t path[MAX_PATH] = {0};
    GetModuleFileNameW(NULL, path, MAX_PATH);
    std::wstring value(path);
    const std::wstring::size_type slash = value.find_last_of(L"\\/");
    return slash == std::wstring::npos ? L"." : value.substr(0, slash);
}

bool LegacySystem() {
    typedef LONG (WINAPI *RtlGetVersionFn)(OSVERSIONINFOW*);
    RtlGetVersionFn rtlGetVersion = reinterpret_cast<RtlGetVersionFn>(
        GetProcAddress(GetModuleHandleW(L"ntdll.dll"), "RtlGetVersion"));
    OSVERSIONINFOEXW version = {0};
    version.dwOSVersionInfoSize = sizeof(version);
    // RtlGetVersion is exported by XP and returns the real version even when
    // a compatibility manifest changes GetVersionEx behaviour.
    if (rtlGetVersion) {
        if (rtlGetVersion(reinterpret_cast<OSVERSIONINFOW*>(&version)) != 0) return true;
    } else if (!GetVersionExW(reinterpret_cast<OSVERSIONINFOW*>(&version))) {
        return true;
    }
    // WebView2/Tauri's supported Windows 10 baseline starts at 1803
    // (build 17134). Keep older 64-bit Windows 10 builds on Legacy too.
    if (version.dwMajorVersion < 10 ||
        (version.dwMajorVersion == 10 && version.dwBuildNumber < 17134)) {
        return true;
    }
    // The bundled modern binary is x64. Keep 32-bit Windows on the x86
    // compatibility client so Windows 10 x86 is covered as well.
    SYSTEM_INFO system = {0};
    GetNativeSystemInfo(&system);
    return system.wProcessorArchitecture == PROCESSOR_ARCHITECTURE_INTEL;
}
}

int WINAPI wWinMain(HINSTANCE, HINSTANCE, LPWSTR commandLine, int show) {
    const std::wstring directory = DirectoryOfModule();
    const std::wstring executable = directory + (LegacySystem() ? L"\\SecurityMonitoringLegacy.exe" : L"\\SecurityMonitoringModern.exe");
    const std::wstring command = L"\"" + executable + L"\" " + (commandLine ? commandLine : L"");
    STARTUPINFOW startup = {0};
    startup.cb = sizeof(startup);
    PROCESS_INFORMATION process = {0};
    wchar_t* mutableCommand = new wchar_t[command.size() + 1];
    lstrcpyW(mutableCommand, command.c_str());
    const BOOL created = CreateProcessW(NULL, mutableCommand, NULL, NULL, FALSE, 0, NULL, directory.c_str(), &startup, &process);
    delete[] mutableCommand;
    if (!created) {
        MessageBoxW(NULL, L"无法启动兼容客户端，请重新安装。", L"安全监控", MB_OK | MB_ICONERROR);
        return 1;
    }
    CloseHandle(process.hThread);
    CloseHandle(process.hProcess);
    return 0;
}
