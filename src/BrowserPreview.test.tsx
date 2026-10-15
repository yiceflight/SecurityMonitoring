import { afterEach, expect, it, vi } from "vitest";
import { cleanup, render, screen, within } from "@testing-library/react";
const bridge = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({
  isTauri: () => false,
  invoke: bridge.invoke,
}));
import App from "./App";
import userEvent from "@testing-library/user-event";
afterEach(cleanup);

it("requires password after ten logo clicks in browser preview without enabling native actions", async () => {
  HTMLDialogElement.prototype.showModal = function () { this.setAttribute("open", ""); };
  render(<App />);
  expect(screen.getByText("预览模式，监控未运行")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "立即截屏一次" })).toBeNull();
  expect(screen.queryByRole("button", { name: "开始监控" })).toBeNull();
  expect(screen.queryByText("监控设置")).toBeNull();
  const user = userEvent.setup();
  const logo = screen.getByRole("button", { name: "安全监控 logo" });
  for (let i = 0; i < 9; i += 1) await user.click(logo);
  expect(screen.queryByRole("dialog")).toBeNull();
  await user.click(logo);
  const passwordDialog = await screen.findByRole("dialog", { name: "身份验证" });
  expect(screen.queryByRole("dialog", { name: "监控设置" })).toBeNull();
  await user.type(within(passwordDialog).getByLabelText("请输入密码"), "admin123{Enter}");
  const dialog = await screen.findByRole("dialog", { name: "监控设置" });
  for (const name of ["开始监控", "保存设置", "注册日志源", "打开记录目录"]) {
    expect((within(dialog).getByRole("button", { name }) as HTMLButtonElement).disabled).toBe(true);
  }
  expect((within(dialog).getByRole("spinbutton", { name: /截屏间隔/ }).closest("fieldset") as HTMLFieldSetElement).disabled).toBe(true);
  expect(bridge.invoke).not.toHaveBeenCalled();
});
