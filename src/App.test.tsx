import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  act,
  cleanup,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { CaptureRecord, Settings, Status } from "./monitor";

const bridge = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({
  isTauri: () => true,
  invoke: bridge.invoke,
}));
import App from "./App";

const record: CaptureRecord = {
  id: "2026-10-12-12345",
  capturedAt: "2026-10-12T14:20:00+08:00",
  width: 1920,
  height: 1080,
  bytes: 100_000,
  windowTitle: "工作文件",
  processName: "notepad.exe",
  idleSeconds: 12,
};
let status: Status;
let saved: Settings | undefined;
let failSave: boolean;

beforeEach(() => {
  status = {
    supported: true,
    settings: {
      intervalSeconds: 60,
      retentionDays: 30,
      autoStart: false,
      enabled: false,
      screenshotStorageLimitMb: 1024,
    },
    state: "stopped",
    storagePath:
      "C:\\Users\\test\\AppData\\Roaming\\com.yice.securitymonitoring",
    todayCount: 0,
    lastCapture: null,
    lastError: null,
    auditError: null,
    autoStartActual: false,
    eventSourceRegistered: false,
  };
  failSave = false;
  saved = undefined;
  bridge.invoke.mockReset();
  bridge.invoke.mockImplementation(
    async (command: string, args: Record<string, unknown>) => {
      switch (command) {
        case "list_replay_records":
          return [];
        case "get_status":
          return structuredClone(status);
        case "set_monitoring_enabled": {
          status.settings.enabled = args.enabled as boolean;
          status.state = status.settings.enabled ? "running" : "stopped";
          return structuredClone(status);
        }
        case "save_settings": {
          if (failSave) throw new Error("配置写入失败");
          saved = args.settings as Settings;
          status.settings = { ...saved };
          status.autoStartActual = saved.autoStart;
          return structuredClone(status);
        }
        case "register_event_source":
          throw new Error("用户取消了管理员授权");
        default:
          return undefined;
      }
    },
  );
  HTMLDialogElement.prototype.showModal = function () {
    this.setAttribute("open", "");
  };
  HTMLDialogElement.prototype.close = function () {
    this.removeAttribute("open");
    this.dispatchEvent(new Event("close"));
  };
});
afterEach(cleanup);

async function openSettings(user: ReturnType<typeof userEvent.setup>) {
  const logo = screen.getByRole("button", { name: "宜测 logo" });
  for (let i = 0; i < 10; i += 1) await user.click(logo);
  await unlockSettings(user);
  return screen.findByRole("dialog", { name: "监控设置" });
}

async function unlockSettings(user: ReturnType<typeof userEvent.setup>) {
  const passwordDialog = await screen.findByRole("dialog", { name: "身份验证" });
  expect(screen.queryByRole("dialog", { name: "监控设置" })).toBeNull();
  expect(screen.queryByRole("spinbutton")).toBeNull();
  await user.type(within(passwordDialog).getByLabelText("请输入密码"), "yice123");
  await user.click(within(passwordDialog).getByRole("button", { name: "确认" }));
}

describe("Windows monitoring controls", () => {
  it("starts and stops only after backend acknowledgement", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByText("监控已停止");
    await openSettings(user);
    const start = (await screen.findByRole("button", {
      name: "开始监控",
    })) as HTMLButtonElement;
    await waitFor(() => expect(start.disabled).toBe(false));
    await user.click(start);
    expect(await screen.findByText("监控正在运行")).toBeTruthy();
    await user.click(await screen.findByRole("button", { name: "停止监控" }));
    expect(await screen.findByText("监控已停止")).toBeTruthy();
    const changes = bridge.invoke.mock.calls.filter(
      ([command]) => command === "set_monitoring_enabled",
    );
    expect(changes.map(([, args]) => args.enabled)).toEqual([true, false]);
  });

  it.each([1, 7200])("keeps a %i-second interval after write failure and submits the successful retry", async (seconds) => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByText("监控已停止");
    await openSettings(user);
    const interval = screen.getByRole("spinbutton", {
      name: /截屏间隔/,
    }) as HTMLInputElement;
    await waitFor(() =>
      expect(interval.closest("fieldset")?.disabled).toBe(false),
    );
    await user.clear(interval);
    await user.type(interval, String(seconds));
    await user.click(
      screen.getByRole("checkbox", { name: "登录 Windows 后自动启动" }),
    );
    failSave = true;
    await user.click(screen.getByRole("button", { name: "保存设置" }));
    expect(await screen.findByText(/配置写入失败/)).toBeTruthy();
    expect(interval.value).toBe(String(seconds));
    expect(screen.getByText("设置尚未保存")).toBeTruthy();
    failSave = false;
    await waitFor(() =>
      expect(
        (screen.getByRole("button", { name: "保存设置" }) as HTMLButtonElement)
          .disabled,
      ).toBe(false),
    );
    await user.click(screen.getByRole("button", { name: "保存设置" }));
    expect(await screen.findByText("监控设置已保存")).toBeTruthy();
    expect(saved).toMatchObject({
      intervalSeconds: seconds,
      autoStart: true,
      enabled: false,
    });
    await waitFor(() => expect(screen.queryByText("设置尚未保存")).toBeNull());
  });

  it("shows header status and playback without exposing private diagnostics", async () => {
    status.state = "running";
    status.settings.enabled = true;
    status.todayCount = 42;
    status.lastCapture = record;
    status.lastError = "private capture diagnostic";
    render(<App />);
    await screen.findByText("监控正在运行");
    for (const text of ["YICE SECURITY MONITORING", "今日截图", "最近截图", "使用记录", "运行日志", "private capture diagnostic", "private audit event", "notepad.exe"]) {
      expect(screen.queryByText(text)).toBeNull();
    }
    expect(screen.getByText("监控正在运行").closest("header")).toBeTruthy();
    expect(screen.getByRole("heading", { name: "时间回放" })).toBeTruthy();
    expect(await screen.findByText("暂无回放记录")).toBeTruthy();
    expect(screen.queryByText("电脑使用规范")).toBeNull();
  });

  it("replaces a stale running state after polling fails and recovers without exposing diagnostics", async () => {
    vi.useFakeTimers();
    try {
      status.state = "running";
      status.settings.enabled = true;
      render(<App />);
      await act(async () => {});
      expect(screen.getByText("监控正在运行")).toBeTruthy();
      bridge.invoke.mockRejectedValueOnce(new Error("private backend path"));
      await act(async () => { await vi.advanceTimersByTimeAsync(3000); });
      expect(screen.getByText("暂时无法获取监控状态")).toBeTruthy();
      expect(screen.queryByText("监控正在运行")).toBeNull();
      expect(screen.queryByText(/private backend path/)).toBeNull();
      await act(async () => { await vi.advanceTimersByTimeAsync(3000); });
      expect(screen.getByText("监控正在运行")).toBeTruthy();
    } finally {
      cleanup();
      vi.useRealTimers();
    }
  });

  it("surfaces administrator cancellation without claiming registration success", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByText("监控已停止");
    await openSettings(user);
    const register = screen.getByRole("button", {
      name: "注册日志源",
    }) as HTMLButtonElement;
    await waitFor(() => expect(register.disabled).toBe(false));
    await user.click(register);
    expect(await screen.findByText(/用户取消了管理员授权/)).toBeTruthy();
    expect(screen.queryByText("Windows 事件日志源已注册")).toBeNull();
  });

  it("requires ten logo clicks and a password each time, saves all settings together", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByText("监控已停止");
    const logo = screen.getByRole("button", { name: "宜测 logo" });
    expect(screen.queryByText("监控设置")).toBeNull();
    expect(screen.queryByRole("spinbutton")).toBeNull();
    expect(screen.queryByRole("button", { name: "注册日志源" })).toBeNull();
    for (let i = 0; i < 9; i += 1) await user.click(logo);
    expect(screen.queryByRole("dialog")).toBeNull();
    await user.click(logo);
    await unlockSettings(user);
    const dialog = await screen.findByRole("dialog", { name: "监控设置" });
    await user.clear(within(dialog).getByRole("spinbutton", { name: /截屏间隔/ }));
    await user.type(within(dialog).getByRole("spinbutton", { name: /截屏间隔/ }), "120");
    await user.clear(within(dialog).getByRole("spinbutton", { name: /截图保留/ }));
    await user.type(within(dialog).getByRole("spinbutton", { name: /截图保留/ }), "45");
    await user.click(within(dialog).getByRole("checkbox", { name: "登录 Windows 后自动启动" }));
    expect(within(dialog).queryByRole("spinbutton", { name: "截图日志写入间隔" })).toBeNull();
    expect(within(dialog).queryByRole("checkbox", { name: "同时写入 Windows 事件日志" })).toBeNull();
    await user.clear(within(dialog).getByRole("spinbutton", { name: "截图空间限制" }));
    await user.type(
      within(dialog).getByRole("spinbutton", { name: "截图空间限制" }),
      "2048",
    );
    await user.click(within(dialog).getByRole("button", { name: "保存设置" }));
    expect(await screen.findByText("监控设置已保存")).toBeTruthy();
    expect(saved).toMatchObject({
      intervalSeconds: 120,
      retentionDays: 45,
      autoStart: true,
      screenshotStorageLimitMb: 2048,
    });
    await user.click(within(dialog).getByRole("button", { name: "关闭" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    for (let i = 0; i < 9; i += 1) await user.click(logo);
    expect(screen.queryByRole("dialog")).toBeNull();
    await user.click(logo);
    await unlockSettings(user);
    const reopened = await screen.findByRole("dialog", { name: "监控设置" });
    expect((within(reopened).getByRole("spinbutton", { name: /截屏间隔/ }) as HTMLInputElement).value).toBe("120");
    expect((within(reopened).getByRole("spinbutton", { name: "截图空间限制" }) as HTMLInputElement).value).toBe("2048");
    act(() => (reopened as HTMLDialogElement).close());
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("rejects incorrect passwords and clears authentication after cancel or Escape", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByText("监控已停止");
    const logo = screen.getByRole("button", { name: "宜测 logo" });
    for (let i = 0; i < 10; i += 1) await user.click(logo);
    const passwordDialog = await screen.findByRole("dialog", { name: "身份验证" });
    const passwordInput = within(passwordDialog).getByLabelText("请输入密码") as HTMLInputElement;
    expect(passwordInput.type).toBe("password");
    await user.type(passwordInput, "wrong-password{Enter}");
    expect(within(passwordDialog).getByRole("alert").textContent).toContain("密码错误");
    expect(screen.queryByRole("dialog", { name: "监控设置" })).toBeNull();
    expect(bridge.invoke.mock.calls.every(([command]) => ["get_status", "list_replay_records"].includes(command))).toBe(true);
    await user.click(within(passwordDialog).getByRole("button", { name: "取消" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    for (let i = 0; i < 9; i += 1) await user.click(logo);
    expect(screen.queryByRole("dialog")).toBeNull();
    await user.click(logo);
    const reopened = await screen.findByRole("dialog", { name: "身份验证" });
    expect((within(reopened).getByLabelText("请输入密码") as HTMLInputElement).value).toBe("");
    expect(within(reopened).queryByRole("alert")).toBeNull();
    // The browser fires close after Escape dismisses a native dialog.
    act(() => (reopened as HTMLDialogElement).close());
    expect(screen.queryByRole("dialog")).toBeNull();
    for (let i = 0; i < 10; i += 1) await user.click(logo);
    const retry = await screen.findByRole("dialog", { name: "身份验证" });
    await user.type(within(retry).getByLabelText("请输入密码"), "incorrect{Enter}");
    await user.clear(within(retry).getByLabelText("请输入密码"));
    await user.type(within(retry).getByLabelText("请输入密码"), "yice123{Enter}");
    expect(await screen.findByRole("dialog", { name: "监控设置" })).toBeTruthy();
    expect(screen.queryByRole("dialog", { name: "身份验证" })).toBeNull();
  });

  it("opens settings on unsupported systems while keeping native actions disabled", async () => {
    status.supported = false;
    status.state = "unsupported";
    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => expect(screen.queryByText("正在读取监控状态…")).toBeNull());
    expect(screen.getByText("当前系统不支持监控")).toBeTruthy();
    expect(screen.queryByText(/请在 Windows 上运行客户端/)).toBeNull();
    const dialog = await openSettings(user);
    expect(within(dialog).getByText(/当前仅可查看设置/)).toBeTruthy();
    expect((within(dialog).getByRole("spinbutton", { name: /截屏间隔/ }).closest("fieldset") as HTMLFieldSetElement).disabled).toBe(true);
    expect((within(dialog).getByRole("button", { name: "注册日志源" }) as HTMLButtonElement).disabled).toBe(true);
    expect(bridge.invoke.mock.calls.every(([command]) => ["get_status", "list_replay_records"].includes(command))).toBe(true);
  });
});
