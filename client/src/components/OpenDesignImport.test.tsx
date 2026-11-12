// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { changeAppLanguage } from "../i18n";
import { OpenDesignImport } from "./OpenDesignImport";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const call = vi.mocked(invoke);
beforeEach(async () => { call.mockReset(); await changeAppLanguage("zh-CN"); });
afterEach(cleanup);

test("imports only on click, using the selected proxy, without a generation call", async () => {
  const imported = vi.fn();
  call.mockResolvedValue('{"provider":"opendesign","access_token":"synthetic"}');
  render(<OpenDesignImport useSystemProxy={false} disabled={false} onImported={imported} />);
  expect(call).not.toHaveBeenCalled();
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "导入本机 OpenDesign 登录" })); });
  expect(call).toHaveBeenCalledExactlyOnceWith("import_opendesign_local_credential", { useSystemProxy: false });
  expect(imported).toHaveBeenCalledWith('{"provider":"opendesign","access_token":"synthetic"}');
});

test("shows a busy import while account lookup is pending and allows retry after failure", async () => {
  let fail!: (reason: string) => void;
  call.mockImplementationOnce(() => new Promise((_, reject) => { fail = reject; }));
  const imported = vi.fn();
  render(<OpenDesignImport useSystemProxy={true} disabled={false} onImported={imported} />);
  fireEvent.click(screen.getByRole("button", { name: "导入本机 OpenDesign 登录" }));
  const pending = screen.getByRole("button", { name: "正在读取登录与工作区…" });
  expect(pending).toBeDisabled();
  expect(pending).toHaveAttribute("aria-busy", "true");
  expect(screen.getByRole("button", { name: "选择 Vela 登录文件" })).toBeDisabled();
  fireEvent.click(pending);
  expect(call).toHaveBeenCalledTimes(1);
  await act(async () => { fail("OpenDesign account query timed out"); });
  expect(screen.getByRole("alert")).toHaveTextContent("timed out");
  expect(screen.getByRole("button", { name: "导入本机 OpenDesign 登录" })).toBeEnabled();
  call.mockResolvedValueOnce("retry-credential");
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "导入本机 OpenDesign 登录" })); });
  expect(imported).toHaveBeenCalledExactlyOnceWith("retry-credential");
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
});

test.each(["close", "proxy"])("ignores a late credential after %s", async (action) => {
  let finish!: (value: string) => void;
  call.mockImplementation(() => new Promise(resolve => { finish = resolve as (value: string) => void; }));
  const imported = vi.fn();
  const view = render(<OpenDesignImport useSystemProxy={true} disabled={false} onImported={imported} />);
  fireEvent.click(screen.getByRole("button", { name: "导入本机 OpenDesign 登录" }));
  if (action === "close") view.unmount();
  else view.rerender(<OpenDesignImport useSystemProxy={false} disabled={false} onImported={imported} />);
  await act(async () => { finish("late-credential"); });
  expect(imported).not.toHaveBeenCalled();
});

test("shows a failed account lookup without importing a credential", async () => {
  call.mockRejectedValue("OpenDesign account query returned HTTP 403");
  const imported = vi.fn();
  render(<OpenDesignImport useSystemProxy={true} disabled={false} onImported={imported} />);
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "导入本机 OpenDesign 登录" })); });
  expect(screen.getByRole("alert")).toHaveTextContent("HTTP 403");
  expect(imported).not.toHaveBeenCalled();
});

test("imports an explicitly selected isolated Vela config without using the default login", async () => {
  const imported = vi.fn();
  const source = '{"profiles":{"prod":{"runtimeKey":"fixture-runtime","controlKey":"fixture-control"}}}';
  call.mockResolvedValue("normalized-credential");
  render(<OpenDesignImport useSystemProxy={true} disabled={false} onImported={imported} />);
  const file = new File([source], "config.json", { type: "application/json" });
  Object.defineProperty(file, "text", { value: async () => source });
  await act(async () => { fireEvent.change(screen.getByLabelText("选择 Vela 登录文件"), { target: { files: [file] } }); });
  expect(call).toHaveBeenCalledExactlyOnceWith("import_opendesign_local_credential", { useSystemProxy: true, configJson: source });
  expect(imported).toHaveBeenCalledWith("normalized-credential");
});
