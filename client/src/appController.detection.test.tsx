import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { useAppController } from "./appController";
import { emptyConfig } from "./appHelpers";
import { emptyAccountStatus } from "./account/accountTypes";
import { sourceDriverById, sourceDriverInitialChannelV5 } from "./sourceDrivers";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));

const originalChannel = () => ({
  ...sourceDriverInitialChannelV5(sourceDriverById("openrouter"), {
    id: "refresh-channel", name: "OpenRouter fixture", credentialRef: "fixture-key",
  }),
  enabled: false, share_enabled: false,
  default_model: "vendor/selected", models: ["vendor/first", "vendor/selected"],
});

describe("channel refresh and inference command boundaries", () => {
  let wire: ReturnType<typeof configFixture>;
  let failRefresh = false;
  const configFixture = () => ({ ...emptyConfig, config_version: 5, proxy_auto_start: false, supplier_auto_start: false, channels: [originalChannel()] });
  beforeEach(() => {
    vi.stubGlobal("__TAURI_INTERNALS__", {});
    wire = configFixture();
    failRefresh = false;
    vi.mocked(invoke).mockReset().mockImplementation(async (command, args) => {
      const input = args as Record<string, unknown> | undefined;
      if (command === "get_config") return structuredClone(wire);
      if (command === "find_duplicate_channels") return [];
      if (command === "save_supplier_config") {
        wire = { ...wire, channels: input?.channels as typeof wire.channels };
        return structuredClone(wire);
      }
      if (command === "refresh_channel_upstream") {
        if (failRefresh) throw new Error("catalog HTTP 401: fixture rejected");
        return {
          models: ["vendor/first", "vendor/new", "vendor/selected"], surface_results: [],
          model_capability_evidence: [], detection_evidence: [], quota_status: "unknown",
          remaining_ratio: 0, quota_windows: [], checks: ["catalog ok"], warnings: [],
        };
      }
      if (command === "test_channel_upstream") return { http_status: 200, model: input?.model, content: "test succeeded", raw: "{}", latency_ms: 10, input_tokens: 5, output_tokens: 2 };
      if (command === "proxy_status") return { running: false, listen: emptyConfig.listen };
      if (command === "refresh_platform_status") return {
        running: true,
        listen: emptyConfig.listen,
        active_endpoint: "cn-legacy",
        platform_server_version: "0.1.46",
      };
      if (command === "supplier_status") return { running: false, starting: false, channels: [], route_statuses: [] };
      if (command === "account_status") return emptyAccountStatus();
      if (command === "get_release_source_status") return { updateSources: [], endpointSources: [] };
      if (command === "autostart_status") return false;
      if (command === "startup_log_from_ui" || command === "sync_native_theme") return null;
      if (command.startsWith("fetch_")) return { data: [] };
      if (command === "check_tool_config") return { detected: false, configured: false, files: [], messages: [] };
      throw new Error(`Unexpected fixture command: ${command}`);
    });
  });
  afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); });

  it("new subscription authorization creates a separate channel without a duplicate prompt", async () => {
    const driver = sourceDriverById("openai_subscription");
    wire.channels[0] = sourceDriverInitialChannelV5(driver, {
      id: "refresh-channel", name: "Existing subscription", credentialRef: "old-managed.json",
    });
    const existing = structuredClone(wire.channels[0]);
    const prepared = sourceDriverInitialChannelV5(driver, {
      id: "new-subscription", name: "New subscription", credentialRef: ".pending/new.json",
    });
    const finalized = { ...prepared, credential_ref: "new-managed.json" };
    const base = vi.mocked(invoke).getMockImplementation()!;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "import_subscription_token_json") return prepared;
      if (command === "finalize_subscription_import") return finalized;
      if (command === "activate_supplier_channel") {
        wire.channels = wire.channels.map((channel) => channel.id === finalized.id
          ? { ...channel, enabled: true, models: ["gpt-fixture"] }
          : channel);
        return { running: true, starting: false, channels: [{ channel_id: finalized.id, status: "available" }], route_statuses: [] };
      }
      return base(command, args);
    });
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.selectedChannel.id).toBe("refresh-channel"));
    act(() => { result.current.addSubscriptionPreset(driver); });
    act(() => { result.current.setSubscriptionTokenJson('{"refresh_token":"fixture"}'); });
    vi.mocked(invoke).mockClear();
    await act(async () => { await result.current.importSubscriptionJson(); });
    expect(invoke).not.toHaveBeenCalledWith("find_duplicate_channels", expect.anything());
    expect(invoke).toHaveBeenCalledWith("finalize_subscription_import", { channel: prepared, createDuplicate: true });
    expect(wire.channels).toHaveLength(2);
    expect(wire.channels[0]).toMatchObject(existing);
    expect(wire.channels[1]).toMatchObject({ id: "new-subscription", credential_ref: "new-managed.json", enabled: true });
    expect(result.current.subscriptionWizardOpen).toBe(false);
  });

  it("refreshes native platform status when the About page is entered", async () => {
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.selectedChannel.id).toBe("refresh-channel"));
    vi.mocked(invoke).mockClear();
    act(() => result.current.setTab("about"));
    await waitFor(() => expect(result.current.status.platform_server_version).toBe("0.1.46"));
    expect(invoke).toHaveBeenCalledWith("refresh_platform_status");

    vi.mocked(invoke).mockClear();
    act(() => result.current.setTab("access"));
    act(() => result.current.setTab("about"));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("refresh_platform_status"));
    expect(
      vi.mocked(invoke).mock.calls.filter(([command]) => command === "refresh_platform_status"),
    ).toHaveLength(1);
  });

  it.each(["openrouter", "openai_subscription", "gemini_subscription", "claude_subscription", "custom_endpoint"] as const)(
    "%s concurrency save hot-updates without activation, probing or ACK polling", async (driver) => {
      wire.channels[0] = {
        ...sourceDriverInitialChannelV5(sourceDriverById(driver), {
          id: "refresh-channel", name: "Save fixture", credentialRef: "fixture-key",
        }),
        enabled: true, share_enabled: true, default_model: "fixture-model", models: ["fixture-model"],
      };
      const base = vi.mocked(invoke).getMockImplementation()!;
      const runtime = { running: true, starting: false, channels: [{ channel_id: "refresh-channel", status: "available" }], route_statuses: [], channel_transports: [] };
      vi.mocked(invoke).mockImplementation(async (command, args) => {
        if (["supplier_status", "refresh_supplier_registration"].includes(command)) return runtime;
        if (command === "account_status") return { ...emptyAccountStatus(), state: "signed_in" };
        return base(command, args);
      });
      const { result } = renderHook(() => useAppController());
      await waitFor(() => expect(result.current.selectedChannel.id).toBe("refresh-channel"));
      act(() => result.current.updateSupplier({ max_concurrency: 7 }));
      expect(result.current.selectedChannelHasChanges).toBe(true);
      expect(result.current.supplierChannelNeedsDetection(result.current.selectedChannel)).toBe(false);
      vi.mocked(invoke).mockClear();
      await act(async () => { await result.current.saveSupplierChanges(); });
      expect(wire.channels[0].max_concurrency).toBe(7);
      expect(vi.mocked(invoke).mock.calls.map(([command]) => command)).toEqual([
        "find_duplicate_channels", "save_supplier_config",
      ]);
      expect(invoke).not.toHaveBeenCalledWith("refresh_supplier_registration", expect.anything());
      expect(result.current.toast?.text).toContain("已保存");
    },
  );

  it("a policy save keeps the existing check status while the local update is pending", async () => {
    wire.channels[0].enabled = true;
    wire.channels[0].share_enabled = true;
    const base = vi.mocked(invoke).getMockImplementation()!;
    const runtime = { running: true, starting: false, channels: [{ channel_id: "refresh-channel", status: "available" }], route_statuses: [] };
    let finish!: (value: unknown) => void;
    const refresh = new Promise((resolve) => { finish = resolve; });
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "supplier_status") return runtime;
      if (command === "save_supplier_config") await refresh;
      return base(command, args);
    });
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.selectedChannel.id).toBe("refresh-channel"));
    act(() => result.current.updateSupplier({ name: "Renamed", price_ratio: 2, quota_reserve_percent: 10, default_model: "vendor/first" }));
    let saving!: Promise<unknown>;
    act(() => { saving = result.current.saveSupplierChanges(); });
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("save_supplier_config", expect.anything()));
    expect(result.current.selectedChannelOperation).toBe("saving");
    expect(result.current.selectedChannelLifecycle.localLabel).toBe("检测：已通过");
    await act(async () => { finish(runtime); await saving; });
    expect(wire.channels[0]).toMatchObject({ name: "Renamed", price_ratio: 2, quota_reserve_percent: 10, default_model: "vendor/first" });
  });

  it("explicit full check validates locally without joining the platform", async () => {
    wire.channels[0].enabled = true;
    const base = vi.mocked(invoke).getMockImplementation()!;
    const runtime = { running: true, starting: false, channels: [], route_statuses: [] };
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (["supplier_status", "start_supplier"].includes(command)) return runtime;
      if (command === "validate_channel_upstream") return { channel: wire.channels[0], health: { status: "available" } };
      return base(command, args);
    });
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.selectedChannel.id).toBe("refresh-channel"));
    vi.mocked(invoke).mockClear();
    await act(async () => { await result.current.saveSupplierChanges(result.current.selectedChannel, true); });
    expect(invoke).toHaveBeenCalledWith("validate_channel_upstream", expect.anything());
    expect(invoke).not.toHaveBeenCalledWith("start_supplier", expect.anything());
    expect(vi.mocked(invoke).mock.calls.some(([command]) => command === "refresh_supplier_registration")).toBe(false);
  });

  it("local saves do not depend on hosted registration availability", async () => {
    wire.channels[0].enabled = true;
    const base = vi.mocked(invoke).getMockImplementation()!;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "supplier_status") return { running: true, starting: false, channels: [], route_statuses: [] };
      if (command === "refresh_supplier_registration") throw new Error("fixture registration unavailable");
      return base(command, args);
    });
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.selectedChannel.id).toBe("refresh-channel"));
    act(() => result.current.updateSupplier({ max_concurrency: 7 }));
    vi.mocked(invoke).mockClear();
    await act(async () => { await result.current.saveSupplierChanges(); });
    expect(wire.channels[0].max_concurrency).toBe(7);
    expect(vi.mocked(invoke).mock.calls.map(([command]) => command)).toEqual([
      "find_duplicate_channels", "save_supplier_config",
    ]);
    expect(result.current.toast?.text).toContain("已保存");
    expect(invoke).not.toHaveBeenCalledWith("refresh_supplier_registration", expect.anything());
  });

  it("waits for startup rather than a timer before diagnosing the local listener", async () => {
    vi.useFakeTimers();
    wire.proxy_auto_start = true;
    const base = vi.mocked(invoke).getMockImplementation()!;
    let finishStart!: (value: unknown) => void;
    let running = false;
    const pending = new Promise((resolve) => { finishStart = resolve; });
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "start_proxy") return pending;
      if (command === "proxy_status") return { running, listen: emptyConfig.listen };
      return base(command, args);
    });
    const { result } = renderHook(() => useAppController());
    await act(async () => { await vi.advanceTimersByTimeAsync(15000); });
    expect(invoke).toHaveBeenCalledWith("start_proxy");
    expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "proxy_status")).toHaveLength(1);
    expect(result.current.localEntryIssue).toBeUndefined();
    expect(result.current.toast?.text || "").not.toContain("未监听");
    await act(async () => {
      running = true;
      finishStart({ running, listen: emptyConfig.listen });
    });
    expect(result.current.status.running).toBe(true);
    expect(result.current.localEntryIssue).toBeUndefined();
    await act(async () => { running = false; await vi.advanceTimersByTimeAsync(5000); });
    expect(result.current.localEntryIssue?.message).toContain("未监听");
  });

  it("still reports a real startup failure immediately", async () => {
    wire.proxy_auto_start = true;
    const base = vi.mocked(invoke).getMockImplementation()!;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "start_proxy") throw new Error("fixture bind permission denied");
      return base(command, args);
    });
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.toast?.text).toContain("fixture bind permission denied"));
    expect(result.current.localEntryIssue).toBeDefined();
  });

  it("does not apply a stale status read across a manual restart", async () => {
    vi.useFakeTimers();
    wire.proxy_auto_start = true;
    const base = vi.mocked(invoke).getMockImplementation()!;
    let finishRead!: (value: unknown) => void;
    let holdRead = false;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "proxy_status") return holdRead
        ? new Promise((resolve) => { finishRead = resolve; })
        : { running: true, listen: emptyConfig.listen };
      if (command === "save_config") return structuredClone(wire);
      if (command === "stop_proxy" || command === "start_proxy") return { running: command === "start_proxy", listen: emptyConfig.listen };
      return base(command, args);
    });
    const { result } = renderHook(() => useAppController());
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    holdRead = true;
    await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
    await act(async () => { await result.current.restart(); });
    expect(result.current.status.running).toBe(true);
    await act(async () => { finishRead({ running: false, listen: emptyConfig.listen }); });
    expect(result.current.status.running).toBe(true);
    expect(result.current.localEntryIssue).toBeUndefined();
  });

  it("refresh saves metadata without inference, while test still invokes the configured model", async () => {
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.selectedChannel.id).toBe("refresh-channel"));
    expect(result.current.selectedChannelHasChanges).toBe(false);
    vi.mocked(invoke).mockClear();
    await act(async () => { await result.current.refreshSupplierModels({ throttle: false }); });
    expect(invoke).toHaveBeenCalledWith("refresh_channel_upstream", expect.anything());
    expect(wire.channels[0].models).toContain("vendor/new");
    expect(result.current.toast?.text).toContain("已刷新渠道信息");
    expect(vi.mocked(invoke).mock.calls.some(([command]) => ["detect_channel_upstream", "validate_channel_upstream", "test_channel_upstream", "activate_supplier_channel", "start_supplier"].includes(command))).toBe(false);
    await act(async () => { await result.current.testSupplierUpstream(); });
    expect(invoke).toHaveBeenCalledWith("test_channel_upstream", expect.objectContaining({ model: "vendor/selected" }));
    expect(result.current.supplierTestResult.status).toBe("success");
  });

  it("refresh errors are visible and never trigger a paid fallback or overwrite saved models", async () => {
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.selectedChannel.id).toBe("refresh-channel"));
    failRefresh = true;
    vi.mocked(invoke).mockClear();
    await act(async () => { await result.current.refreshSupplierModels({ throttle: false }); });
    expect(result.current.toast?.text).toContain("catalog HTTP 401");
    expect(wire.channels[0].models).toEqual(originalChannel().models);
    expect(vi.mocked(invoke).mock.calls.map(([command]) => command)).toEqual(["refresh_channel_upstream"]);
  });
});
