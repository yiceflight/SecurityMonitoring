import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import Replay from "./Replay";
import type { CaptureRecord } from "./monitor";

const bridge = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ isTauri: () => true, invoke: bridge.invoke }));
const capture = (time: string, id: string): CaptureRecord => ({
  id, capturedAt: `${time}+08:00`, width: 1920, height: 1080,
  bytes: 100, windowTitle: `窗口 ${id}`, processName: "work.exe", idleSeconds: 0,
});
let records: CaptureRecord[];
const source = (id: string) => `data:image/jpeg;base64,${id}`;
const displayed = () => screen.getByRole("img", { name: /^电脑截图/ }) as HTMLImageElement;
async function loaded(id: string) {
  await waitFor(() => expect(displayed().getAttribute("src")).toBe(source(id)));
  fireEvent.load(displayed());
}
function scrub(time: string) {
  fireEvent.change(screen.getByRole("slider"), { target: { value: (new Date(time).getTime() - new Date(records[0].capturedAt).getTime()) / 1000 } });
}

beforeEach(() => {
  records = [capture("2026-10-12T08:00:00", "first"), capture("2026-10-13T08:01:00", "second"), capture("2026-10-13T10:30:00", "last")];
  bridge.invoke.mockReset();
  bridge.invoke.mockImplementation(async (command: string, args?: Record<string, string>) => {
    if (command === "list_replay_records") return records;
    if (command === "get_record_image") return source(args!.id);
    throw new Error("unexpected command");
  });
  HTMLDialogElement.prototype.showModal = function () { this.setAttribute("open", ""); };
});
afterEach(() => { cleanup(); vi.useRealTimers(); });

describe("timeline playback", () => {
  it("defaults to the latest record without date or time forms, drags across days and enlarges", async () => {
    render(<Replay available />);
    await loaded("last");
    expect(document.querySelector('input[type="date"], input[type="time"], select')).toBeNull();
    expect(bridge.invoke.mock.calls.filter(([command]) => command === "get_record_image")).toHaveLength(1);
    expect(screen.getByText("3 / 3 张")).toBeTruthy();
    scrub("2026-10-12T08:00:00+08:00");
    await loaded("first");
    expect(screen.getByText("1 / 3 张")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "放大查看" }));
    const dialog = screen.getByRole("dialog");
    expect(within(dialog).getByRole("img").getAttribute("src")).toBe(source("first"));
    fireEvent.click(within(dialog).getByRole("button", { name: "原始尺寸" }));
    expect(within(dialog).getByRole("button", { name: "适应窗口" })).toBeTruthy();
    fireEvent.click(within(dialog).getByRole("button", { name: "关闭" }));
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("keeps the cursor at the dragged time and identifies the nearest real screenshot in a gap", async () => {
    render(<Replay available />);
    await loaded("last");
    scrub("2026-10-13T08:20:00+08:00");
    await loaded("second");
    expect(screen.getByText("该时刻无截图，显示最近记录")).toBeTruthy();
    expect((screen.getByRole("slider") as HTMLInputElement).value).toBe("87600");
    expect(screen.getByRole("slider").getAttribute("aria-valuetext")).toContain("2026-10-13 08:01:00");
    fireEvent.keyDown(screen.getByRole("slider"), { key: "ArrowLeft" });
    await loaded("first");
    fireEvent.keyDown(screen.getByRole("slider"), { key: "End" });
    await loaded("last");
  });

  it("debounces fast dragging and ignores late image responses", async () => {
    let resolveOld!: (value: string) => void;
    bridge.invoke.mockImplementation(async (command: string, args?: Record<string, string>) => {
      if (command === "list_replay_records") return records;
      if (args?.id === "last") return new Promise<string>((resolve) => { resolveOld = resolve; });
      return source(args!.id);
    });
    render(<Replay available />);
    await waitFor(() => expect(resolveOld).toBeTypeOf("function"));
    vi.useFakeTimers();
    scrub("2026-10-12T08:00:00+08:00");
    scrub("2026-10-13T08:01:00+08:00");
    await act(async () => { await vi.advanceTimersByTimeAsync(100); });
    expect(displayed().getAttribute("src")).toBe(source("second"));
    expect(bridge.invoke).not.toHaveBeenCalledWith("get_record_image", { id: "first" });
    await act(async () => { resolveOld(source("last")); });
    expect(displayed().getAttribute("src")).toBe(source("second"));
  });

  it("does not let stale index responses overwrite a new availability session", async () => {
    let resolveOld!: (value: CaptureRecord[]) => void;
    bridge.invoke.mockImplementationOnce(() => new Promise<CaptureRecord[]>((resolve) => { resolveOld = resolve; }));
    const view = render(<Replay available />);
    view.rerender(<Replay available={false} />);
    view.rerender(<Replay available />);
    await loaded("last");
    await act(async () => { resolveOld([capture("2026-10-13T11:00:00", "stale")]); });
    expect(displayed().getAttribute("src")).toBe(source("last"));
  });

  it("reports read failures separately and refreshes to include new records", async () => {
    bridge.invoke.mockRejectedValueOnce(new Error("private path"));
    render(<Replay available />);
    expect(await screen.findByText("无法读取回放记录，请刷新重试")).toBeTruthy();
    expect(screen.queryByText("暂无回放记录")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "刷新记录" }));
    await loaded("last");
    records = [...records, capture("2026-10-13T11:00:00", "new")];
    fireEvent.click(screen.getByRole("button", { name: "刷新记录" }));
    await loaded("new");
    expect(screen.getByText("4 / 4 张")).toBeTruthy();
  });

  it("clears missing images, allows retry and handles decode failure", async () => {
    render(<Replay available />);
    await loaded("last");
    bridge.invoke.mockRejectedValueOnce(new Error("file deleted"));
    scrub("2026-10-12T08:00:00+08:00");
    await screen.findByText("截图无法读取，可能已清理或文件不可用");
    expect(screen.queryByRole("img")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "重试图片" }));
    await loaded("first");
    fireEvent.error(displayed());
    expect(screen.getByText("截图无法读取，可能已清理或文件不可用")).toBeTruthy();
  });

  it("handles empty history and a single screenshot", async () => {
    records = [];
    render(<Replay available />);
    expect(await screen.findByText("暂无回放记录")).toBeTruthy();
    expect(screen.queryByRole("slider")).toBeNull();
    records = [capture("2026-10-13T10:00:00", "only")];
    fireEvent.click(screen.getByRole("button", { name: "刷新记录" }));
    await loaded("only");
    expect((screen.getByRole("slider") as HTMLInputElement).disabled).toBe(true);
    expect(screen.getByText("1 / 1 张")).toBeTruthy();
  });
});
