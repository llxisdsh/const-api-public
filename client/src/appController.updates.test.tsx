import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useAppController } from "./appController";
import { emptyConfig } from "./appHelpers";
import { emptyAccountStatus } from "./account/accountTypes";
import { sourceDriverById, sourceDriverInitialChannelV5 } from "./sourceDrivers";

const edition = vi.hoisted(() => ({ updates: false, development: false }));
vi.mock("./runtimeProfile", async (importOriginal) => ({
  ...await importOriginal<typeof import("./runtimeProfile")>(),
  get DEVELOPMENT_PROFILE() { return edition.development; },
  get RELEASE_UPDATES_SUPPORTED() { return edition.updates; },
  get RELEASE_UPDATES_ENABLED() { return edition.updates && !edition.development; },
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));

describe("application runtime preferences", () => {
  beforeEach(() => {
    edition.updates = false;
    edition.development = false;
    vi.stubGlobal("__TAURI_INTERNALS__", {});
    vi.mocked(listen).mockClear();
    vi.mocked(invoke).mockReset().mockImplementation(async (command) => {
      if (command === "get_config") return {
        ...emptyConfig, config_version: 5, proxy_auto_start: false, supplier_auto_start: false,
        prevent_sleep_while_supplying: false,
        channels: [sourceDriverInitialChannelV5(sourceDriverById("openrouter"), {
          id: "update-boundary", name: "Fixture", credentialRef: "fixture",
        })],
      };
      if (command === "proxy_status") return { running: false, listen: emptyConfig.listen };
      if (command === "supplier_status") return { running: false, starting: false, channels: [], route_statuses: [] };
      if (command === "account_status") return emptyAccountStatus();
      if (command === "get_release_source_status") return { updateSources: [], endpointSources: [] };
      if (command === "autostart_status") return false;
      if (command === "startup_log_from_ui" || command === "sync_native_theme") return null;
      if (command.startsWith("fetch_")) return { data: [] };
      if (command === "check_tool_config") return { detected: false, configured: false, files: [], messages: [] };
      if (command === "native_update_status" || command === "request_native_update_check") return {
        status: "idle", sources: [], checkedAtUnixMs: 0,
      };
      throw new Error(`Unexpected fixture command: ${command}`);
    });
  });
  afterEach(() => vi.unstubAllGlobals());

  it("does not subscribe, poll or invoke updates in the local edition", async () => {
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.selectedChannel.id).toBe("update-boundary"));
    await act(async () => {
      window.dispatchEvent(new Event("focus"));
      document.dispatchEvent(new Event("visibilitychange"));
      await result.current.checkAndDownloadUpdate();
      await result.current.installPreparedUpdate();
    });
    expect(listen).not.toHaveBeenCalledWith("const-api://update-state", expect.any(Function));
    expect(vi.mocked(invoke).mock.calls.filter(([name]) => [
      "native_update_status", "request_native_update_check", "request_native_update_install",
    ].includes(name))).toEqual([]);
    expect(result.current.updateState).toEqual({ status: "idle" });
  });

  it("syncs the hosted updater and allows manual checks even with a legacy opt-out", async () => {
    edition.updates = true;
    window.localStorage.setItem("const-api:auto-install-updates", "false");
    try {
      const { result } = renderHook(() => useAppController());
      await waitFor(() => expect(invoke).toHaveBeenCalledWith("native_update_status"));
      expect(listen).toHaveBeenCalledWith("const-api://update-state", expect.any(Function));
      await act(() => result.current.checkAndDownloadUpdate());
      expect(invoke).toHaveBeenCalledWith("request_native_update_check");
      expect(invoke).not.toHaveBeenCalledWith("set_automatic_updates", expect.anything());
    } finally {
      window.localStorage.removeItem("const-api:auto-install-updates");
    }
  });

  it("checks a release manually in development without scheduling a download or install", async () => {
    edition.updates = true;
    edition.development = true;
    const base = vi.mocked(invoke).getMockImplementation()!;
    vi.mocked(invoke).mockImplementation(async (command, args) => command === "request_native_update_check"
      ? { status: "available", version: "9.8.7", sources: [], checkedAtUnixMs: 1000 }
      : base(command, args));
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.selectedChannel.id).toBe("update-boundary"));
    expect(invoke).not.toHaveBeenCalledWith("request_native_update_check");
    await act(() => result.current.checkAndDownloadUpdate());
    expect(result.current.updateState).toMatchObject({ status: "available", version: "9.8.7" });
    await act(() => result.current.installPreparedUpdate());
    expect(invoke).not.toHaveBeenCalledWith("request_native_update_install");
  });

  it("keeps update errors in the existing update information without an application alarm", async () => {
    edition.updates = true;
    const base = vi.mocked(invoke).getMockImplementation()!;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "native_update_status") return {
        status: "error", sources: [], checkedAtUnixMs: 0, error: "fixture update unavailable",
      };
      return base(command, args);
    });
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.updateState.status).toBe("error"));
    expect(result.current.appMeta.updateLastError).toContain("fixture update unavailable");
    expect(result.current.applicationDiagnostics).toEqual([]);
  });

  it("saves sleep prevention locally without changing drafts or restarting services", async () => {
    const base = vi.mocked(invoke).getMockImplementation()!;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "set_prevent_sleep_while_supplying") return (args as { enabled: boolean }).enabled;
      return base(command, args);
    });
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.selectedChannel.id).toBe("update-boundary"));
    act(() => result.current.setConfig((current) => ({ ...current, listen: "127.0.0.1:39999" })));
    vi.mocked(invoke).mockClear();
    await act(() => result.current.changeSleepPrevention(true));
    expect(result.current.config.prevent_sleep_while_supplying).toBe(true);
    expect(result.current.config.listen).toBe("127.0.0.1:39999");
    expect(invoke).toHaveBeenCalledWith("set_prevent_sleep_while_supplying", { enabled: true });
    expect(vi.mocked(invoke).mock.calls.some(([command]) => ["save_config", "save_supplier_config", "start_supplier", "stop_supplier", "start_proxy", "stop_proxy"].includes(command))).toBe(false);
    await act(() => result.current.changeSleepPrevention(false));
    expect(result.current.config.prevent_sleep_while_supplying).toBe(false);
  });

  it("keeps the switch unchanged and reports a failed sleep-prevention save", async () => {
    const base = vi.mocked(invoke).getMockImplementation()!;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "set_prevent_sleep_while_supplying") throw new Error("PowerCreateRequest failed");
      return base(command, args);
    });
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.selectedChannel.id).toBe("update-boundary"));
    await act(() => result.current.changeSleepPrevention(true));
    expect(result.current.config.prevent_sleep_while_supplying).not.toBe(true);
    expect(result.current.sleepPreventionBusy).toBe(false);
    expect(result.current.toast?.text).toContain("PowerCreateRequest failed");
  });
});
