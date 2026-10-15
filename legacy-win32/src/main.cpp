#define UNICODE
#define _UNICODE
#define WIN32_LEAN_AND_MEAN
#define _WIN32_WINNT 0x0501

#include <windows.h>
#include <gdiplus.h>
#define PSAPI_VERSION 1
#include <psapi.h>
#include <wincrypt.h>
#include <shellapi.h>
#include <shlobj.h>
#include <strsafe.h>

#include <algorithm>
#include <cstdlib>
#include <cwchar>
#include <fstream>
#include <sstream>
#include <string>
#include <vector>

#pragma comment(lib, "gdiplus.lib")
#pragma comment(lib, "psapi.lib")

namespace {

const wchar_t kClassName[] = L"SecurityMonitoringLegacyWindow";
const wchar_t kAppName[] = L"SecurityMonitoring";
const wchar_t kMutexName[] = L"Local\\SecurityMonitoring.SingleInstance";
const wchar_t kEventLog[] = L"System";
const wchar_t kEventSource[] = L"SecurityMonitoring";
const UINT_PTR kTimerId = 1;
const UINT kStartId = 1001;
const UINT kStopId = 1002;
const UINT kFolderId = 1003;
const UINT kIntervalId = 1004;
const UINT kAutoStartId = 1005;

struct Settings {
    unsigned long intervalSeconds;
    unsigned long retentionDays;
    unsigned long storageLimitMb;
    bool enabled;
    bool autoStart;
    Settings() : intervalSeconds(60), retentionDays(30), storageLimitMb(1024), enabled(false), autoStart(false) {}
};

HINSTANCE g_instance = NULL;
HWND g_window = NULL;
HWND g_status = NULL;
HWND g_interval = NULL;
HWND g_autoStart = NULL;
HANDLE g_mutex = NULL;
ULONG_PTR g_gdiplusToken = 0;
std::wstring g_root;
Settings g_settings;
bool g_running = false;

std::wstring Join(const std::wstring& a, const std::wstring& b) {
    if (a.empty()) return b;
    if (a[a.size() - 1] == L'\\') return a + b;
    return a + L"\\" + b;
}

std::wstring AppDataRoot() {
    wchar_t path[MAX_PATH] = {0};
    if (SHGetFolderPathW(NULL, CSIDL_APPDATA, NULL, SHGFP_TYPE_CURRENT, path) != S_OK) {
        return L".";
    }
    return Join(path, L"com.securitymonitoring.app");
}

bool EnsureDirectory(const std::wstring& path) {
    if (CreateDirectoryW(path.c_str(), NULL) || GetLastError() == ERROR_ALREADY_EXISTS) {
        return true;
    }
    const std::wstring parent = path.substr(0, path.find_last_of(L'\\'));
    if (parent == path || parent.empty()) return false;
    if (!EnsureDirectory(parent)) return false;
    return CreateDirectoryW(path.c_str(), NULL) || GetLastError() == ERROR_ALREADY_EXISTS;
}

std::string ToUtf8(const std::wstring& value) {
    if (value.empty()) return std::string();
    int length = WideCharToMultiByte(CP_UTF8, 0, value.c_str(), -1, NULL, 0, NULL, NULL);
    if (length <= 1) return std::string();
    std::string result(static_cast<size_t>(length - 1), '\0');
    WideCharToMultiByte(CP_UTF8, 0, value.c_str(), -1, &result[0], length - 1, NULL, NULL);
    return result;
}

std::wstring JsonString(const std::wstring& value) {
    std::wstring escaped;
    for (size_t i = 0; i < value.size(); ++i) {
        const wchar_t c = value[i];
        if (c == L'\\') escaped += L"\\\\";
        else if (c == L'"') escaped += L"\\\"";
        else if (c == L'\r') escaped += L"\\r";
        else if (c == L'\n') escaped += L"\\n";
        else escaped += c;
    }
    return escaped;
}

std::wstring ReadText(const std::wstring& path) {
    std::ifstream input(path.c_str(), std::ios::binary);
    if (!input) return std::wstring();
    std::ostringstream buffer;
    buffer << input.rdbuf();
    const std::string bytes = buffer.str();
    if (bytes.empty()) return std::wstring();
    int length = MultiByteToWideChar(CP_UTF8, 0, bytes.c_str(), static_cast<int>(bytes.size()), NULL, 0);
    if (length <= 0) return std::wstring();
    std::wstring result(static_cast<size_t>(length), L'\0');
    MultiByteToWideChar(CP_UTF8, 0, bytes.c_str(), static_cast<int>(bytes.size()), &result[0], length);
    return result;
}

bool JsonBool(const std::wstring& text, const wchar_t* key, bool fallback) {
    const std::wstring needle = std::wstring(L"\"") + key + L"\"";
    const size_t keyPosition = text.find(needle);
    if (keyPosition == std::wstring::npos) return fallback;
    const size_t colon = text.find(L':', keyPosition + needle.size());
    if (colon == std::wstring::npos) return fallback;
    const size_t value = text.find_first_not_of(L" \t\r\n", colon + 1);
    if (value == std::wstring::npos) return fallback;
    if (text.compare(value, 4, L"true") == 0) return true;
    if (text.compare(value, 5, L"false") == 0) return false;
    return fallback;
}

unsigned long JsonNumber(const std::wstring& text, const wchar_t* key, unsigned long fallback) {
    const std::wstring needle = std::wstring(L"\"") + key + L"\"";
    const size_t keyPosition = text.find(needle);
    if (keyPosition == std::wstring::npos) return fallback;
    const size_t colon = text.find(L':', keyPosition + needle.size());
    if (colon == std::wstring::npos) return fallback;
    const size_t value = text.find_first_not_of(L" \t\r\n", colon + 1);
    if (value == std::wstring::npos || value >= text.size() || text[value] < L'0' || text[value] > L'9') {
        return fallback;
    }
    unsigned long result = 0;
    for (size_t i = value; i < text.size() && text[i] >= L'0' && text[i] <= L'9'; ++i) {
        if (result > 86400UL) return fallback;
        result = result * 10UL + static_cast<unsigned long>(text[i] - L'0');
    }
    return result == 0 ? fallback : result;
}

void LoadSettings() {
    const std::wstring text = ReadText(Join(g_root, L"settings.json"));
    g_settings.intervalSeconds = JsonNumber(text, L"intervalSeconds", 60);
    if (g_settings.intervalSeconds < 1) g_settings.intervalSeconds = 1;
    if (g_settings.intervalSeconds > 86400) g_settings.intervalSeconds = 86400;
    g_settings.retentionDays = JsonNumber(text, L"retentionDays", 30);
    if (g_settings.retentionDays < 1) g_settings.retentionDays = 1;
    if (g_settings.retentionDays > 365) g_settings.retentionDays = 365;
    g_settings.storageLimitMb = JsonNumber(text, L"screenshotStorageLimitMb", 1024);
    if (g_settings.storageLimitMb < 100) g_settings.storageLimitMb = 100;
    g_settings.enabled = JsonBool(text, L"enabled", false);
    g_settings.autoStart = JsonBool(text, L"autoStart", false);
}

void SaveSettings() {
    EnsureDirectory(g_root);
    std::ofstream output(Join(g_root, L"settings.json").c_str(), std::ios::trunc);
    if (!output) return;
    output << "{\n  \"intervalSeconds\": " << g_settings.intervalSeconds
           << ",\n  \"retentionDays\": " << g_settings.retentionDays
           << ",\n  \"autoStart\": " << (g_settings.autoStart ? "true" : "false")
           << ",\n  \"enabled\": " << (g_settings.enabled ? "true" : "false")
           << ",\n  \"screenshotStorageLimitMb\": " << g_settings.storageLimitMb << "\n}\n";
}

std::wstring DateName() {
    SYSTEMTIME now;
    GetLocalTime(&now);
    wchar_t result[32] = {0};
    StringCchPrintfW(result, 32, L"%04u-%02u-%02u", now.wYear, now.wMonth, now.wDay);
    return result;
}

std::wstring FileId() {
    SYSTEMTIME now;
    GetLocalTime(&now);
    wchar_t result[64] = {0};
    StringCchPrintfW(result, 64, L"%04u%02u%02u%02u%02u%02u%03u",
        now.wYear, now.wMonth, now.wDay, now.wHour, now.wMinute, now.wSecond, now.wMilliseconds);
    return result;
}

std::wstring Timestamp() {
    SYSTEMTIME now;
    GetLocalTime(&now);
    wchar_t result[64] = {0};
    StringCchPrintfW(result, 64, L"%04u-%02u-%02uT%02u:%02u:%02u.%03u+08:00",
        now.wYear, now.wMonth, now.wDay, now.wHour, now.wMinute, now.wSecond,
        now.wMilliseconds);
    return result;
}

int EncoderClsid(const WCHAR* mime, CLSID* clsid) {
    UINT count = 0;
    UINT bytes = 0;
    if (Gdiplus::GetImageEncodersSize(&count, &bytes) != Gdiplus::Ok || bytes == 0) return -1;
    std::vector<BYTE> buffer(bytes);
    Gdiplus::ImageCodecInfo* codecs = reinterpret_cast<Gdiplus::ImageCodecInfo*>(&buffer[0]);
    if (Gdiplus::GetImageEncoders(count, bytes, codecs) != Gdiplus::Ok) return -1;
    for (UINT i = 0; i < count; ++i) {
        if (wcscmp(codecs[i].MimeType, mime) == 0) {
            *clsid = codecs[i].Clsid;
            return static_cast<int>(i);
        }
    }
    return -1;
}

bool InputDesktopAvailable();

std::string FileMd5(const std::wstring& path) {
    HCRYPTPROV provider = 0;
    HCRYPTHASH hash = 0;
    HANDLE file = CreateFileW(path.c_str(), GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, NULL);
    if (file == INVALID_HANDLE_VALUE) return std::string();
    std::string result;
    if (CryptAcquireContextW(&provider, NULL, NULL, PROV_RSA_FULL, CRYPT_VERIFYCONTEXT) &&
        CryptCreateHash(provider, CALG_MD5, 0, 0, &hash)) {
        BYTE buffer[64 * 1024];
        DWORD read = 0;
        bool ok = true;
        while (ReadFile(file, buffer, sizeof(buffer), &read, NULL) && read > 0) {
            if (!CryptHashData(hash, buffer, read, 0)) { ok = false; break; }
        }
        if (ok) {
            BYTE digest[16] = {0};
            DWORD length = sizeof(digest);
            if (CryptGetHashParam(hash, HP_HASHVAL, digest, &length, 0)) {
                char hex[33] = {0};
                for (DWORD i = 0; i < length; ++i) StringCchPrintfA(hex + i * 2, 3, "%02x", digest[i]);
                result = hex;
            }
        }
    }
    if (hash) CryptDestroyHash(hash);
    if (provider) CryptReleaseContext(provider, 0);
    CloseHandle(file);
    return result;
}

void ForegroundDetails(std::wstring* title, std::wstring* process) {
    HWND window = GetForegroundWindow();
    wchar_t windowTitle[512] = {0};
    const int titleLength = GetWindowTextW(window, windowTitle, 512);
    if (titleLength > 0) *title = std::wstring(windowTitle, static_cast<size_t>(titleLength));
    DWORD pid = 0;
    GetWindowThreadProcessId(window, &pid);
    if (!pid) return;
    HANDLE handle = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, FALSE, pid);
    if (!handle) return;
    wchar_t path[MAX_PATH] = {0};
    if (GetModuleFileNameExW(handle, NULL, path, MAX_PATH) > 0) {
        const wchar_t* slash = wcsrchr(path, L'\\');
        *process = slash ? slash + 1 : path;
    }
    CloseHandle(handle);
}

unsigned long IdleSeconds() {
    LASTINPUTINFO info = {0};
    info.cbSize = sizeof(info);
    if (!GetLastInputInfo(&info)) return 0;
    return (GetTickCount() - info.dwTime) / 1000UL;
}

bool CaptureScreen(std::wstring* savedPath) {
    if (!InputDesktopAvailable()) return false;
    const int x = GetSystemMetrics(SM_XVIRTUALSCREEN);
    const int y = GetSystemMetrics(SM_YVIRTUALSCREEN);
    const int width = GetSystemMetrics(SM_CXVIRTUALSCREEN);
    const int height = GetSystemMetrics(SM_CYVIRTUALSCREEN);
    if (width <= 0 || height <= 0) return false;
    HDC screen = GetDC(NULL);
    HDC memory = screen ? CreateCompatibleDC(screen) : NULL;
    HBITMAP bitmap = (screen && memory) ? CreateCompatibleBitmap(screen, width, height) : NULL;
    if (!screen || !memory || !bitmap) {
        if (bitmap) DeleteObject(bitmap);
        if (memory) DeleteDC(memory);
        if (screen) ReleaseDC(NULL, screen);
        return false;
    }
    HGDIOBJ old = SelectObject(memory, bitmap);
    const BOOL copied = BitBlt(memory, 0, 0, width, height, screen, x, y, SRCCOPY | CAPTUREBLT);
    SelectObject(memory, old);
    if (!copied) {
        DeleteObject(bitmap); DeleteDC(memory); ReleaseDC(NULL, screen); return false;
    }
    if (!InputDesktopAvailable()) {
        DeleteObject(bitmap); DeleteDC(memory); ReleaseDC(NULL, screen); return false;
    }
    const std::wstring day = Join(Join(g_root, L"records"), DateName());
    if (!EnsureDirectory(day)) {
        DeleteObject(bitmap); DeleteDC(memory); ReleaseDC(NULL, screen); return false;
    }
    const std::wstring id = FileId();
    const std::wstring capturedAt = Timestamp();
    const std::wstring path = Join(day, id + L".jpg");
    Gdiplus::Bitmap image(bitmap, NULL);
    CLSID jpeg;
    const bool encoded = EncoderClsid(L"image/jpeg", &jpeg) >= 0 && image.Save(path.c_str(), &jpeg, NULL) == Gdiplus::Ok;
    DeleteObject(bitmap); DeleteDC(memory); ReleaseDC(NULL, screen);
    if (!encoded) return false;
    std::ofstream index(Join(day, L"captures.jsonl").c_str(), std::ios::app);
    if (!index) return false;
    WIN32_FILE_ATTRIBUTE_DATA attributes = {0};
    ULARGE_INTEGER fileSize = {0};
    if (GetFileAttributesExW(path.c_str(), GetFileExInfoStandard, &attributes)) {
        fileSize.HighPart = attributes.nFileSizeHigh;
        fileSize.LowPart = attributes.nFileSizeLow;
    }
    std::wstring title;
    std::wstring process;
    ForegroundDetails(&title, &process);
    const std::string md5 = FileMd5(path);
    const std::string md5Json = md5.empty() ? "null" : (std::string("\"") + md5 + "\"");
    index << "{\"id\":\"" << ToUtf8(JsonString(id)) << "\",\"capturedAt\":\"" << ToUtf8(JsonString(capturedAt))
          << "\",\"width\":" << width << ",\"height\":" << height
          << ",\"bytes\":" << fileSize.QuadPart << ",\"md5\":" << md5Json << ",\"windowTitle\":\""
          << ToUtf8(JsonString(title)) << "\",\"processName\":\"" << ToUtf8(JsonString(process))
          << "\",\"idleSeconds\":" << IdleSeconds() << "}\n";
    *savedPath = path;
    return true;
}

bool InputDesktopAvailable() {
    HDESK desktop = OpenInputDesktop(0, FALSE, DESKTOP_READOBJECTS);
    if (!desktop) return false;
    wchar_t name[64] = {0};
    DWORD needed = 0;
    const BOOL read = GetUserObjectInformationW(desktop, UOI_NAME, name, sizeof(name), &needed);
    CloseDesktop(desktop);
    return read && lstrcmpiW(name, L"Default") == 0;
}

void ReportEvent(WORD type, DWORD id, const std::wstring& message) {
    HKEY key = NULL;
    const std::wstring path = std::wstring(L"SYSTEM\\CurrentControlSet\\Services\\EventLog\\") + kEventLog + L"\\" + kEventSource;
    if (RegOpenKeyExW(HKEY_LOCAL_MACHINE, path.c_str(), 0, KEY_READ, &key) != ERROR_SUCCESS) return;
    RegCloseKey(key);
    HANDLE source = RegisterEventSourceW(NULL, kEventSource);
    if (!source) return;
    LPCWSTR strings[1] = {message.c_str()};
    ::ReportEventW(source, type, 0, id, NULL, 1, 0, strings, NULL);
    DeregisterEventSource(source);
}

void SetStatus(const std::wstring& message) {
    if (g_status) SetWindowTextW(g_status, message.c_str());
}

void InstallAutoStart(bool enabled);

void ApplyUiSettings() {
    wchar_t value[32] = {0};
    if (g_interval && GetWindowTextW(g_interval, value, 32) > 0) {
        const unsigned long interval = wcstoul(value, NULL, 10);
        if (interval >= 1 && interval <= 86400) g_settings.intervalSeconds = interval;
    }
    g_settings.autoStart = g_autoStart && SendMessageW(g_autoStart, BM_GETCHECK, 0, 0) == BST_CHECKED;
    InstallAutoStart(g_settings.autoStart);
}

void StartMonitoring() {
    ApplyUiSettings();
    g_running = true;
    g_settings.enabled = true;
    SaveSettings();
    SetTimer(g_window, kTimerId, static_cast<UINT>(g_settings.intervalSeconds * 1000UL), NULL);
    SetStatus(L"监控运行中（兼容模式）");
    ReportEvent(EVENTLOG_INFORMATION_TYPE, 1003, L"SecurityMonitoring legacy client started");
    std::wstring path;
    if (CaptureScreen(&path)) ReportEvent(EVENTLOG_INFORMATION_TYPE, 1100, L"截图已保存：" + path);
}

void StopMonitoring(bool persist) {
    g_running = false;
    if (persist) {
        ApplyUiSettings();
        g_settings.enabled = false;
        SaveSettings();
    }
    KillTimer(g_window, kTimerId);
    SetStatus(L"监控已停止（兼容模式）");
    ReportEvent(EVENTLOG_INFORMATION_TYPE, 1004, L"SecurityMonitoring legacy client stopped");
}

void OpenRecords() {
    const std::wstring records = Join(g_root, L"records");
    EnsureDirectory(records);
    ShellExecuteW(g_window, L"open", records.c_str(), NULL, NULL, SW_SHOWNORMAL);
}

void InstallAutoStart(bool enabled) {
    HKEY key = NULL;
    if (RegOpenKeyExW(HKEY_CURRENT_USER, L"Software\\Microsoft\\Windows\\CurrentVersion\\Run", 0, KEY_SET_VALUE, &key) == ERROR_SUCCESS) {
        if (enabled) {
            wchar_t path[MAX_PATH] = {0};
            GetModuleFileNameW(NULL, path, MAX_PATH);
            const std::wstring command = std::wstring(L"\"") + path + L"\" --background";
            RegSetValueExW(key, kAppName, 0, REG_SZ, reinterpret_cast<const BYTE*>(command.c_str()), static_cast<DWORD>((command.size() + 1) * sizeof(wchar_t)));
        } else {
            RegDeleteValueW(key, kAppName);
        }
        RegCloseKey(key);
    }
}

LRESULT CALLBACK WindowProc(HWND hwnd, UINT message, WPARAM wParam, LPARAM lParam) {
    switch (message) {
        case WM_COMMAND:
            if (LOWORD(wParam) == kStartId) StartMonitoring();
            else if (LOWORD(wParam) == kStopId) StopMonitoring(true);
            else if (LOWORD(wParam) == kFolderId) OpenRecords();
            return 0;
        case WM_TIMER:
            if (wParam == kTimerId && g_running) {
                std::wstring path;
                if (CaptureScreen(&path)) {
                    SetStatus(L"监控运行中（兼容模式）");
                    ReportEvent(EVENTLOG_INFORMATION_TYPE, 1100, L"截图已保存：" + path);
                } else {
                    SetStatus(L"截图失败，将按间隔重试（兼容模式）");
                    ReportEvent(EVENTLOG_ERROR_TYPE, 1101, L"Legacy client capture failed");
                }
            }
            return 0;
        case WM_CLOSE:
            ApplyUiSettings();
            SaveSettings();
            StopMonitoring(false);
            DestroyWindow(hwnd);
            return 0;
        case WM_DESTROY:
            PostQuitMessage(0);
            return 0;
    }
    return DefWindowProcW(hwnd, message, wParam, lParam);
}

int RunLegacy(HINSTANCE instance, int show) {
    g_instance = instance;
    g_root = AppDataRoot();
    EnsureDirectory(Join(g_root, L"records"));
    LoadSettings();
    InstallAutoStart(g_settings.autoStart);
    WNDCLASSW wc = {0};
    wc.lpfnWndProc = WindowProc;
    wc.hInstance = instance;
    wc.lpszClassName = kClassName;
    wc.hCursor = LoadCursor(NULL, IDC_ARROW);
    wc.hbrBackground = reinterpret_cast<HBRUSH>(COLOR_WINDOW + 1);
    RegisterClassW(&wc);
    g_window = CreateWindowExW(0, kClassName, L"安全监控（兼容模式）", WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
        CW_USEDEFAULT, CW_USEDEFAULT, 520, 220, NULL, NULL, instance, NULL);
    if (!g_window) return 1;
    g_status = CreateWindowW(L"STATIC", L"监控已停止（兼容模式）", WS_CHILD | WS_VISIBLE, 24, 20, 450, 24, g_window, NULL, instance, NULL);
    CreateWindowW(L"STATIC", L"间隔（秒）", WS_CHILD | WS_VISIBLE, 24, 56, 72, 24, g_window, NULL, instance, NULL);
    g_interval = CreateWindowExW(WS_EX_CLIENTEDGE, L"EDIT", L"60", WS_CHILD | WS_VISIBLE | ES_NUMBER, 100, 53, 90, 26, g_window, reinterpret_cast<HMENU>(kIntervalId), instance, NULL);
    g_autoStart = CreateWindowW(L"BUTTON", L"登录 Windows 后自动启动", WS_CHILD | WS_VISIBLE | BS_AUTOCHECKBOX, 210, 54, 210, 24, g_window, reinterpret_cast<HMENU>(kAutoStartId), instance, NULL);
    wchar_t interval[32] = {0};
    StringCchPrintfW(interval, 32, L"%lu", g_settings.intervalSeconds);
    SetWindowTextW(g_interval, interval);
    SendMessageW(g_autoStart, BM_SETCHECK, g_settings.autoStart ? BST_CHECKED : BST_UNCHECKED, 0);
    CreateWindowW(L"BUTTON", L"开始监控", WS_CHILD | WS_VISIBLE | BS_PUSHBUTTON, 24, 96, 130, 34, g_window, reinterpret_cast<HMENU>(kStartId), instance, NULL);
    CreateWindowW(L"BUTTON", L"停止监控", WS_CHILD | WS_VISIBLE | BS_PUSHBUTTON, 170, 96, 130, 34, g_window, reinterpret_cast<HMENU>(kStopId), instance, NULL);
    CreateWindowW(L"BUTTON", L"打开记录目录", WS_CHILD | WS_VISIBLE | BS_PUSHBUTTON, 316, 96, 150, 34, g_window, reinterpret_cast<HMENU>(kFolderId), instance, NULL);
    CreateWindowW(L"STATIC", L"XP/Win7/Win8 兼容客户端；数据与现代客户端共用。", WS_CHILD | WS_VISIBLE, 24, 145, 450, 28, g_window, NULL, instance, NULL);
    ShowWindow(g_window, show);
    UpdateWindow(g_window);
    // An enabled setting is restored only for an explicit background login.
    int argc = 0;
    LPWSTR* argv = CommandLineToArgvW(GetCommandLineW(), &argc);
    bool background = false;
    if (argv) {
        for (int i = 1; i < argc; ++i) background = background || wcscmp(argv[i], L"--background") == 0;
        LocalFree(argv);
    }
    if (background && g_settings.enabled) StartMonitoring();
    MSG message;
    while (GetMessageW(&message, NULL, 0, 0) > 0) {
        TranslateMessage(&message);
        DispatchMessageW(&message);
    }
    return static_cast<int>(message.wParam);
}

} // namespace

int WINAPI wWinMain(HINSTANCE instance, HINSTANCE, LPWSTR, int show) {
    g_mutex = CreateMutexW(NULL, TRUE, kMutexName);
    if (!g_mutex || GetLastError() == ERROR_ALREADY_EXISTS) {
        if (g_mutex) CloseHandle(g_mutex);
        return 0;
    }
    Gdiplus::GdiplusStartupInput input;
    if (Gdiplus::GdiplusStartup(&g_gdiplusToken, &input, NULL) != Gdiplus::Ok) return 1;
    const int result = RunLegacy(instance, show);
    Gdiplus::GdiplusShutdown(g_gdiplusToken);
    CloseHandle(g_mutex);
    return result;
}
