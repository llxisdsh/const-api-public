// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { changeAppLanguage } from "../i18n";
import { DeviceCodeAuthorization as MuseCodeAuthorization } from "./DeviceCodeAuthorization";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const call = vi.mocked(invoke);
const device = () => ({ device_code: "device-test", user_code: "AB-CD", verification_uri: "https://auth.meta.com/device", expires_at_unix_ms: Date.now() + 60_000, interval_seconds: 5 });
const tick = (ms: number) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const start = () => act(async () => { fireEvent.click(screen.getByRole("button", { name: "登录获取订阅密钥" })); });

describe("Muse Code subscription sign-in", () => {
  beforeEach(async () => {
    await changeAppLanguage("zh-CN");
    vi.useFakeTimers();
    call.mockReset();
  });
  afterEach(() => { cleanup(); vi.useRealTimers(); });

  test("Copilot uses GitHub device authorization without running model checks", async () => {
    call.mockResolvedValueOnce({ ...device(), verification_uri: "https://github.com/login/device" })
      .mockResolvedValueOnce({ state: "authorized", api_key: "github-oauth-test" });
    const onAuthorized = vi.fn();
    render(<MuseCodeAuthorization provider="copilot" disabled={false} onAuthorized={onAuthorized} onOpenUrl={vi.fn()} />);
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "登录 GitHub Copilot" })); });
    await tick(5000);
    expect(onAuthorized).toHaveBeenCalledWith("github-oauth-test");
    expect(call.mock.calls.map(([command]) => command)).toEqual(["create_copilot_auth_session", "poll_copilot_auth_session"]);
  });

  test("Cursor browser authorization needs no device code and does not save or test models", async () => {
    call.mockResolvedValueOnce({ ...device(), user_code: "", verification_uri: "https://cursor.com/loginDeepControl?challenge=test", interval_seconds: 3 })
      .mockResolvedValueOnce({ state: "authorized", api_key: '{"provider":"cursor","access_token":"test-token"}' });
    const onAuthorized = vi.fn();
    const onStarting = vi.fn();
    render(<MuseCodeAuthorization provider="cursor" disabled={false} onStarting={onStarting} onAuthorized={onAuthorized} onOpenUrl={vi.fn()} />);
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "登录 Cursor" })); });
    expect(onStarting).toHaveBeenCalledOnce();
    expect(screen.getByRole("status")).not.toHaveTextContent("登录确认码");
    await tick(3000);
    expect(onAuthorized).toHaveBeenCalledWith('{"provider":"cursor","access_token":"test-token"}');
    expect(call.mock.calls.map(([command]) => command)).toEqual(["create_cursor_auth_session", "poll_cursor_auth_session", "cancel_cursor_auth_session"]);
  });

  test("imports the authorized key without saving or testing a model", async () => {
    call.mockResolvedValueOnce(device()).mockResolvedValueOnce({ state: "authorized", api_key: "test-key" });
    const onAuthorized = vi.fn();
    const onOpenUrl = vi.fn();
    render(<MuseCodeAuthorization disabled={false} onAuthorized={onAuthorized} onOpenUrl={onOpenUrl} />);
    expect(call).not.toHaveBeenCalled();
    await start();
    expect(screen.getByRole("status")).toHaveTextContent("AB-CD");
    fireEvent.click(screen.getByRole("button", { name: "打开 Meta 登录页" }));
    expect(onOpenUrl).toHaveBeenCalledWith("https://auth.meta.com/device");
    await tick(5000);
    expect(onAuthorized).toHaveBeenCalledWith("test-key");
    expect(call.mock.calls.map(([command]) => command)).toEqual(["create_muse_code_auth_session", "poll_muse_code_auth_session"]);
    expect(screen.getByRole("status")).toHaveTextContent("已填入订阅专用密钥");
    await tick(60_000);
    expect(call).toHaveBeenCalledTimes(2);
  });

  test("honors slow-down and expires without polling indefinitely", async () => {
    call.mockResolvedValueOnce({ ...device(), expires_at_unix_ms: Date.now() + 16_000 })
      .mockResolvedValueOnce({ state: "slow_down" }).mockResolvedValueOnce({ state: "pending" });
    render(<MuseCodeAuthorization disabled={false} onAuthorized={vi.fn()} onOpenUrl={vi.fn()} />);
    await start();
    await tick(5000);
    await tick(9999);
    expect(call).toHaveBeenCalledTimes(2);
    await tick(1);
    expect(call).toHaveBeenCalledTimes(3);
    await tick(1000);
    expect(screen.getByRole("alert")).toHaveTextContent("登录码已过期");
    await tick(60_000);
    expect(call).toHaveBeenCalledTimes(3);
  });

  test.each(["cancel", "switch", "expire"] as const)("ignores late keys after %s", async (action) => {
    let complete: (value: unknown) => void = () => {};
    call.mockResolvedValueOnce(device()).mockImplementationOnce(() => new Promise((resolve) => { complete = resolve; }));
    const onAuthorized = vi.fn();
    const view = render(<MuseCodeAuthorization key="first" disabled={false} onAuthorized={onAuthorized} onOpenUrl={vi.fn()} />);
    await start();
    await tick(5000);
    if (action === "cancel") {
      fireEvent.click(screen.getByRole("button", { name: "取消" }));
    } else if (action === "switch") {
      view.rerender(<MuseCodeAuthorization key="second" disabled={false} onAuthorized={onAuthorized} onOpenUrl={vi.fn()} />);
    } else {
      await tick(55_000);
      expect(screen.getByRole("alert")).toHaveTextContent("登录码已过期");
    }
    await act(async () => { complete({ state: "authorized", api_key: "late-key" }); });
    expect(onAuthorized).not.toHaveBeenCalled();
    await tick(60_000);
    expect(call).toHaveBeenCalledTimes(2);
  });

  test("shows subscription/payment failures without importing any key", async () => {
    call.mockResolvedValueOnce(device()).mockRejectedValueOnce({ code: "payment_required", status: 402 });
    const onAuthorized = vi.fn();
    render(<MuseCodeAuthorization disabled={false} onAuthorized={onAuthorized} onOpenUrl={vi.fn()} />);
    await start();
    await tick(5000);
    expect(screen.getByRole("alert")).toHaveTextContent("不会自动付款或改用按量 Key");
    expect(screen.getByRole("alert")).toHaveTextContent("HTTP 402");
    expect(onAuthorized).not.toHaveBeenCalled();
  });
});
