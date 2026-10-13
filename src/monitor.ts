import { invoke, isTauri } from "@tauri-apps/api/core";
export interface Settings {
  intervalSeconds: number;
  retentionDays: number;
  autoStart: boolean;
  enabled: boolean;
  screenshotStorageLimitMb: number;
}
export interface CaptureRecord {
  id: string;
  capturedAt: string;
  width: number;
  height: number;
  bytes: number;
  windowTitle: string;
  processName: string;
  idleSeconds: number;
}
export interface Status {
  supported: boolean;
  settings: Settings;
  state: "unsupported" | "stopped" | "running" | "waiting" | "error";
  storagePath: string;
  todayCount: number;
  lastCapture: CaptureRecord | null;
  lastError: string | null;
  auditError: string | null;
  autoStartActual: boolean;
  eventSourceRegistered: boolean;
}
export interface RecordPage {
  records: CaptureRecord[];
  total: number;
  page: number;
  pageSize: number;
}
export const native = isTauri();
export const defaultSettings: Settings = {
  intervalSeconds: 60,
  retentionDays: 30,
  autoStart: false,
  enabled: false,
  screenshotStorageLimitMb: 1024,
};
export function call<T>(
  command: string,
  args?: Record<string, unknown>,
): Promise<T> {
  if (!native)
    return Promise.reject(new Error("请在 Windows 客户端中使用此功能"));
  return invoke<T>(command, args);
}
export const localDate = () => {
  const d = new Date();
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
};
export const formatTime = (v?: string) =>
  v ? new Date(v).toLocaleString("zh-CN", { hour12: false }) : "暂无记录";
export const formatBytes = (n: number) =>
  n < 1048576
    ? `${(n / 1024).toFixed(0)} KB`
    : `${(n / 1048576).toFixed(1)} MB`;
