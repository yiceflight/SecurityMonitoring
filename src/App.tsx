import { useEffect, useRef, useState } from "react";
import type { FormEvent, ReactNode } from "react";
import { call, defaultSettings, native } from "./monitor";
import type { Settings, Status } from "./monitor";
import Replay from "./Replay";
import "./App.css";
const labels = {
  unsupported: "当前系统不支持监控",
  stopped: "监控已停止",
  running: "监控正在运行",
  waiting: "等待桌面解锁",
  error: "监控异常，正在重试",
};
function App() {
  const [status, setStatus] = useState<Status | null>(null),
    [error, setError] = useState(""),
    [notice, setNotice] = useState(""),
    [busy, setBusy] = useState(""),
    [statusError, setStatusError] = useState(""),
    [settingsAccess, setSettingsAccess] = useState<"closed" | "password" | "settings">("closed");
  const logoClicks = useRef(0);
  const busyRef = useRef(false);
  const refreshVersion = useRef(0);
  const refresh = async () => {
    if (!native) return;
    const version = ++refreshVersion.current;
    try {
      const next = await call<Status>("get_status");
      if (version === refreshVersion.current) {
        setStatus(next);
        setStatusError("");
      }
    } catch (cause) {
      if (version === refreshVersion.current) setStatusError(String(cause));
    }
  };
  useEffect(() => {
    if (!native) return;
    let active = true;
    let id: ReturnType<typeof setTimeout>;
    async function poll() {
      if (!active) return;
      if (!busyRef.current) await refresh();
      if (active) id = setTimeout(poll, 3000);
    }
    void poll();
    return () => {
      active = false;
      clearTimeout(id);
      refreshVersion.current++;
    };
  }, []);
  const usable = native && status?.supported === true,
    settings = status?.settings ?? defaultSettings;
  async function run<T>(
    name: string,
    fn: () => Promise<T>,
    msg: (v: T) => string,
  ) {
    if (busyRef.current) return false;
    busyRef.current = true;
    refreshVersion.current++;
    setBusy(name);
    setError("");
    setNotice("");
    try {
      const v = await fn();
      setNotice(msg(v));
      return true;
    } catch (e) {
      setError(String(e));
      return false;
    } finally {
      await refresh();
      busyRef.current = false;
      setBusy("");
    }
  }
  const toggle = () =>
    void run(
      "toggle",
      () =>
        call<Status>("set_monitoring_enabled", { enabled: !settings.enabled }),
      (v) =>
        v.settings.enabled
          ? "已开始监控"
          : "已停止自动监控",
    );
  const save = (next: Settings) =>
    run(
      "save",
      () => call<Status>("save_settings", { settings: next }),
      () => "监控设置已保存",
    );
  const clickLogo = () => {
    if (settingsAccess !== "closed") return;
    logoClicks.current += 1;
    if (logoClicks.current === 10) {
      logoClicks.current = 0;
      setSettingsAccess("password");
    }
  };
  const state = !native
    ? "preview"
    : statusError
      ? "error"
      : status?.state ?? "loading";
  const statusLabel = !native
    ? "预览模式，监控未运行"
    : statusError
      ? "暂时无法获取监控状态"
      : status
        ? labels[status.state]
        : "正在读取监控状态…";
  return (
    <main className="app-shell">
      <header className="app-header">
        <div className="brand">
          <button className="brand-logo" aria-label="宜测 logo" onClick={clickLogo}>
            <img src="/shield.svg" alt="" width="44" height="44" draggable={false} />
          </button>
          <h1>宜测安全监控</h1>
        </div>
        <div className={`runtime-status ${state}`} role="status" aria-live="polite">
          <span className="status-dot" aria-hidden="true" />
          {statusLabel}
        </div>
      </header>
      <Replay available={usable} />
      {settingsAccess === "password" && (
        <SettingsPasswordDialog
          onClose={() => setSettingsAccess("closed")}
          onSuccess={() => setSettingsAccess("settings")}
        />
      )}
      {settingsAccess === "settings" && (
        <SettingsDialog onClose={() => setSettingsAccess("closed")}>
          {!usable && (
            <p className="banner info">监控功能仅支持 Windows，当前仅可查看设置。</p>
          )}
          {error && <p className="banner error" role="alert">{error}</p>}
          {notice && <p className="banner success" role="status">{notice}</p>}
          <div className="settings-monitor-control">
            <span>监控状态</span>
            <button
              className={settings.enabled ? "danger-button" : "primary-button"}
              disabled={!usable || !!busy}
              onClick={toggle}
            >
              {busy === "toggle" ? "正在更新…" : settings.enabled ? "停止监控" : "开始监控"}
            </button>
          </div>
          <SettingsForm
            settings={settings}
            disabled={!usable || !!busy}
            saving={busy === "save"}
            onSave={save}
          />
          <div className="storage-row">
            <div>
              <span>本地保存位置</span>
              <p>
                {status?.storagePath || "Windows 客户端的当前用户应用数据目录"}
              </p>
            </div>
            <button
              disabled={!usable || !!busy}
              onClick={() =>
                void run(
                  "folder",
                  () => call<void>("open_records_folder"),
                  () => "已打开记录目录",
                )
              }
            >
              打开记录目录
            </button>
          </div>
          <div className="event-source-row">
            <div>
              <strong>Windows 事件日志</strong>
              <p>
                {status?.eventSourceRegistered
                  ? "日志源已注册，可在事件查看器的“Windows 日志 → 系统”中查找 YiceSecurityMonitoring.System。"
                  : "首次使用或从旧版本升级后，请注册系统日志源，Windows 会要求管理员授权。日志写入“Windows 日志 → 系统”。"}
              </p>
            </div>
            <button
              disabled={!usable || !!busy || status?.eventSourceRegistered}
              onClick={() =>
                void run(
                  "register",
                  () => call<Status>("register_event_source"),
                  () => "Windows 事件日志源已注册",
                )
              }
            >
              {busy === "register"
                ? "等待管理员授权…"
                : status?.eventSourceRegistered
                  ? "已注册"
                  : "注册日志源"}
            </button>
          </div>
          {status?.auditError && (
            <p className="inline-warning">系统日志提示：{status.auditError}</p>
          )}
          {usable && settings.autoStart !== status.autoStartActual && (
            <p className="inline-warning">
              登录启动设置与系统状态不一致，请重新保存设置。
            </p>
          )}
        </SettingsDialog>
      )}
    </main>
  );
}

function SettingsPasswordDialog({
  onClose,
  onSuccess,
}: {
  onClose: () => void;
  onSuccess: () => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [password, setPassword] = useState("");
  const [invalid, setInvalid] = useState(false);
  useEffect(() => {
    if (!dialog.current?.open) dialog.current?.showModal();
  }, []);
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (password !== "yice123") {
      setInvalid(true);
      return;
    }
    onSuccess();
  };
  return (
    <dialog
      ref={dialog}
      className="settings-dialog password-dialog"
      onClose={onClose}
      aria-labelledby="settings-password-title"
    >
      <h3 id="settings-password-title">身份验证</h3>
      <form onSubmit={submit}>
        <label htmlFor="settings-password">请输入密码</label>
        <input
          id="settings-password"
          type="password"
          autoFocus
          autoComplete="off"
          required
          value={password}
          aria-invalid={invalid}
          aria-describedby={invalid ? "settings-password-error" : undefined}
          onChange={(event) => {
            setPassword(event.target.value);
            setInvalid(false);
          }}
        />
        {invalid && (
          <p id="settings-password-error" className="inline-warning" role="alert">
            密码错误，请重试。
          </p>
        )}
        <div className="password-actions">
          <button type="button" onClick={() => dialog.current?.close()}>取消</button>
          <button type="submit" className="primary-button">确认</button>
        </div>
      </form>
    </dialog>
  );
}

function SettingsDialog({
  children,
  onClose,
}: {
  children: ReactNode;
  onClose: () => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    if (!dialog.current?.open) dialog.current?.showModal();
  }, []);
  return (
    <dialog
      ref={dialog}
      className="settings-dialog"
      onClose={onClose}
      aria-labelledby="settings-title"
    >
      <div className="dialog-heading">
        <div>
          <h3 id="settings-title">监控设置</h3>
          <p>保存后立即应用监控设置。</p>
        </div>
        <button onClick={() => dialog.current?.close()}>关闭</button>
      </div>
      {children}
    </dialog>
  );
}
function SettingsForm({
  settings,
  disabled,
  saving,
  onSave,
}: {
  settings: Settings;
  disabled: boolean;
  saving: boolean;
  onSave: (s: Settings) => Promise<boolean>;
}) {
  const [draft, setDraft] = useState(settings),
    [dirty, setDirty] = useState(false),
    [intervalText, setIntervalText] = useState(
      String(settings.intervalSeconds),
    ),
    [retentionText, setRetentionText] = useState(
      String(settings.retentionDays),
    );
  useEffect(() => {
    if (!dirty) {
      setDraft(settings);
      setIntervalText(String(settings.intervalSeconds));
      setRetentionText(String(settings.retentionDays));
    }
  }, [settings, dirty]);
  const set = <K extends keyof Settings>(k: K, v: Settings[K]) => {
    setDraft({ ...draft, [k]: v });
    setDirty(true);
  };
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    try {
      if (
        await onSave({
          ...draft,
          intervalSeconds: Number(intervalText),
          retentionDays: Number(retentionText),
        })
      )
        setDirty(false);
    } catch {}
  };
  return (
    <form onSubmit={(e) => void submit(e)}>
      <fieldset disabled={disabled} className="settings-fields">
        <label>
          截屏间隔
          <div className="number-input">
            <input
              type="number"
              required
              min="10"
              max="3600"
              value={intervalText}
              onChange={(e) => {
                setIntervalText(e.target.value);
                setDirty(true);
              }}
            />
            <span>秒</span>
          </div>
          <small>10–3600 秒，默认 60 秒；每次截图同时写入系统日志，记录图片路径。</small>
        </label>
        <label>
          截图保留
          <div className="number-input">
            <input
              type="number"
              required
              min="1"
              max="365"
              value={retentionText}
              onChange={(e) => {
                setRetentionText(e.target.value);
                setDirty(true);
              }}
            />
            <span>天</span>
          </div>
          <small>过期截图自动删除，并将删除原因和图片路径写入系统日志。</small>
        </label>
        <label>
          截图空间限制
          <div className="number-input">
            <input
              aria-label="截图空间限制"
              type="number"
              min="100"
              max="1048576"
              required
              value={draft.screenshotStorageLimitMb}
              onChange={(event) => set("screenshotStorageLimitMb", Number(event.target.value))}
            />
            <span>MB</span>
          </div>
          <small>超过限制后优先删除最早的截图，保留最新记录。</small>
        </label>
        <div className="checkbox-fields">
          <label>
            <input
              type="checkbox"
              checked={draft.autoStart}
              onChange={(e) => set("autoStart", e.target.checked)}
            />
            登录 Windows 后自动启动
          </label>
        </div>
        <button className="primary-button" type="submit" disabled={!dirty}>
          {saving ? "正在保存…" : "保存设置"}
        </button>
      </fieldset>
      <p className="settings-note">
        重启客户端后恢复上次的启停状态。{dirty && <span>设置尚未保存</span>}
      </p>
    </form>
  );
}

export default App;
