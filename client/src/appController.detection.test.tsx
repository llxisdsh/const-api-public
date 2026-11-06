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
      if (command === "save_routing_preference") {
        const preference = input?.preference as "prefer_local_supply" | "allow_model_equivalence";
        wire = { ...wire, [preference]: input?.enabled as boolean };
        return wire[preference];
      }
      if (command === "save_tool_model_injection_settings") {
        wire = { ...wire, tool_model_injection: input?.settings as typeof wire.tool_model_injection };
        return structuredClone(wire);
      }
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

  it.each([true, false])("automatic OAuth exchange uses the chosen proxy setting %s", async (useSystemProxy) => {
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.selectedChannel.id).toBe("refresh-channel"));
    const base = vi.mocked(invoke).getMockImplementation()!;
    let callback!: (url: string) => void;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "create_subscription_oauth_session") return {
        provider: "openai", state: "fixture-state", code_verifier: "fixture-verifier",
        auth_url: "https://example.test/authorize", redirect_uri: "http://localhost/callback", auto_callback: true,
      };
      if (command === "wait_subscription_oauth_callback") return new Promise((resolve) => { callback = resolve; });
      if (command === "cancel_subscription_oauth_callback" || command === "open_external_url") return null;
      if (command === "exchange_subscription_oauth_code") throw new Error("stop before importing fixture");
      return base(command, args);
    });
    act(() => result.current.addSubscriptionPreset(sourceDriverById("openai_subscription")));
    expect(result.current.subscriptionUseSystemProxy).toBe(true);
    act(() => result.current.changeSubscriptionProxy(useSystemProxy));
    await act(async () => { await result.current.createSubscriptionOAuth(); });
    await act(async () => { callback("http://localhost/callback?code=fixture&state=fixture-state"); });
    expect(invoke).toHaveBeenCalledWith("exchange_subscription_oauth_code", expect.objectContaining({ useSystemProxy }));
  });

  it("changing OAuth egress invalidates the old callback without exchanging a code", async () => {
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.selectedChannel.id).toBe("refresh-channel"));
    const base = vi.mocked(invoke).getMockImplementation()!;
    let callback!: (url: string) => void;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "create_subscription_oauth_session") return {
        provider: "openai", state: "old-state", code_verifier: "fixture",
        auth_url: "https://example.test/authorize", redirect_uri: "http://localhost/callback", auto_callback: true,
      };
      if (command === "wait_subscription_oauth_callback") return new Promise((resolve) => { callback = resolve; });
      if (command === "cancel_subscription_oauth_callback" || command === "open_external_url") return null;
      return base(command, args);
    });
    act(() => result.current.addSubscriptionPreset(sourceDriverById("openai_subscription")));
    await act(async () => { await result.current.createSubscriptionOAuth(); });
    act(() => result.current.changeSubscriptionProxy(false));
    await act(async () => { callback("http://localhost/callback?code=fixture&state=old-state"); });
    expect(result.current.subscriptionOAuthSession).toBeNull();
    expect(invoke).not.toHaveBeenCalledWith("exchange_subscription_oauth_code", expect.anything());
    expect(invoke).toHaveBeenCalledWith("cancel_subscription_oauth_callback", { oauthState: "old-state" });
  });

  it.each(["change proxy", "close"])("ignores a delayed clipboard callback after %s", async (action) => {
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.selectedChannel.id).toBe("refresh-channel"));
    const base = vi.mocked(invoke).getMockImplementation()!;
    let readClipboard!: (value: string) => void;
    vi.stubGlobal("navigator", { clipboard: {
      readText: () => new Promise<string>((resolve) => { readClipboard = resolve; }),
    } });
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "create_subscription_oauth_session") return {
        provider: "openai", state: "old-state", code_verifier: "fixture",
        auth_url: "https://example.test/authorize", redirect_uri: "http://localhost/callback", auto_callback: false,
      };
      if (command === "cancel_subscription_oauth_callback" || command === "open_external_url") return null;
      if (command === "exchange_subscription_oauth_code") throw new Error("stale code must not be exchanged");
      return base(command, args);
    });
    act(() => result.current.addSubscriptionPreset(sourceDriverById("openai_subscription")));
    await act(async () => { await result.current.createSubscriptionOAuth(); });
    let pending!: Promise<void>;
    act(() => { pending = result.current.importSubscriptionOAuthFromClipboard(); });
    act(() => {
      if (action === "change proxy") result.current.changeSubscriptionProxy(false);
      else result.current.closeSubscriptionWizard();
    });
    await act(async () => {
      readClipboard("http://localhost/callback?code=fixture&state=old-state");
      await pending;
    });
    expect(invoke).not.toHaveBeenCalledWith("exchange_subscription_oauth_code", expect.anything());
    expect(result.current.subscriptionCallbackInput).toBe("");
    expect(result.current.subscriptionWizardProgress).toBeNull();
  });

  it("invalidates the previous OAuth callback before awaiting listener cancellation", async () => {
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.selectedChannel.id).toBe("refresh-channel"));
    const base = vi.mocked(invoke).getMockImplementation()!;
    let callback!: (value: string) => void;
    let finishCancellation!: () => void;
    let sessionNumber = 0;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "create_subscription_oauth_session") return {
        provider: "openai", state: `state-${++sessionNumber}`, code_verifier: "fixture",
        auth_url: "https://example.test/authorize", redirect_uri: "http://localhost/callback", auto_callback: true,
      };
      if (command === "wait_subscription_oauth_callback") return new Promise((resolve) => { callback = resolve; });
      if (command === "cancel_subscription_oauth_callback") return new Promise<void>((resolve) => { finishCancellation = resolve; });
      if (command === "open_external_url") return null;
      if (command === "exchange_subscription_oauth_code") throw new Error("superseded code must not be exchanged");
      return base(command, args);
    });
    act(() => result.current.addSubscriptionPreset(sourceDriverById("openai_subscription")));
    await act(async () => { await result.current.createSubscriptionOAuth(); });
    let pending!: Promise<void>;
    act(() => { pending = result.current.createSubscriptionOAuth(); });
    const finishOldCancellation = finishCancellation;
    await act(async () => { callback("http://localhost/callback?code=fixture&state=state-1"); });
    await act(async () => { finishOldCancellation(); await pending; });
    expect(invoke).not.toHaveBeenCalledWith("exchange_subscription_oauth_code", expect.anything());
    expect(result.current.subscriptionOAuthSession?.state).toBe("state-2");
    expect(result.current.subscriptionCallbackInput).toBe("");
  });

  it.each(["prefer_local_supply", "allow_model_equivalence"] as const)(
    "immediately saves %s without restarting access or saving other pages", async (preference) => {
      const { result } = renderHook(() => useAppController());
      await waitFor(() => expect(result.current.selectedChannel.id).toBe("refresh-channel"));
      const enabled = !result.current.config[preference];
      vi.mocked(invoke).mockClear();
      await act(async () => { await result.current.saveRoutingPreference(preference, enabled); });
      expect(invoke).toHaveBeenCalledExactlyOnceWith("save_routing_preference", { preference, enabled });
      expect(wire[preference]).toBe(enabled);
      expect(result.current.config[preference]).toBe(enabled);
      expect(result.current.hasChanges).toBe(false);
      expect(result.current.configActionBusy).toBe(false);
      expect(result.current.routingPreferenceSaving[preference]).toBe(false);
    },
  );

  it("saves different routing switches independently and blocks duplicate clicks on the same switch", async () => {
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.selectedChannel.id).toBe("refresh-channel"));
    const base = vi.mocked(invoke).getMockImplementation()!;
    let releaseLocal!: () => void;
    let releaseCompatibility!: () => void;
    const localGate = new Promise<void>((resolve) => { releaseLocal = resolve; });
    const compatibilityGate = new Promise<void>((resolve) => { releaseCompatibility = resolve; });
    vi.mocked(invoke).mockClear().mockImplementation(async (command, args) => {
      if (command === "save_routing_preference") {
        await ((args as { preference: string }).preference === "prefer_local_supply" ? localGate : compatibilityGate);
      }
      return base(command, args);
    });
    const localEnabled = !result.current.config.prefer_local_supply;
    const compatibilityEnabled = !result.current.config.allow_model_equivalence;
    let localSave!: Promise<void>;
    let compatibilitySave!: Promise<void>;
    act(() => {
      localSave = result.current.saveRoutingPreference("prefer_local_supply", localEnabled);
      compatibilitySave = result.current.saveRoutingPreference("allow_model_equivalence", compatibilityEnabled);
      void result.current.saveRoutingPreference("prefer_local_supply", !localEnabled);
    });
    expect(invoke).toHaveBeenCalledTimes(2);
    expect(result.current.configActionBusy).toBe(false);
    expect(result.current.routingPreferenceSaving).toEqual({ prefer_local_supply: true, allow_model_equivalence: true });
    await act(async () => { releaseCompatibility(); await compatibilitySave; });
    expect(result.current.routingPreferenceSaving).toEqual({ prefer_local_supply: true, allow_model_equivalence: false });
    expect(result.current.config.allow_model_equivalence).toBe(compatibilityEnabled);
    await act(async () => { releaseLocal(); await localSave; });
    expect(result.current.config.prefer_local_supply).toBe(localEnabled);
    expect(result.current.config.allow_model_equivalence).toBe(compatibilityEnabled);
    expect(result.current.configActionBusy).toBe(false);
    expect(result.current.hasChanges).toBe(false);
  });

  it("waits for pending routing preferences before saving and restarting access", async () => {
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.selectedChannel.id).toBe("refresh-channel"));
    const base = vi.mocked(invoke).getMockImplementation()!;
    let release!: () => void;
    const gate = new Promise<void>((resolve) => { release = resolve; });
    vi.mocked(invoke).mockClear().mockImplementation(async (command, args) => {
      if (command === "save_routing_preference") await gate;
      if (command === "save_config") {
        wire = { ...wire, ...(args as { config: object }).config };
        return structuredClone(wire);
      }
      if (command === "stop_proxy" || command === "start_proxy") return { running: command === "start_proxy", listen: wire.listen };
      return base(command, args);
    });
    const enabled = !result.current.config.prefer_local_supply;
    let saving!: Promise<void>;
    let restarting!: Promise<void>;
    act(() => { saving = result.current.saveRoutingPreference("prefer_local_supply", enabled); });
    act(() => result.current.setConfig((current) => ({ ...current, listen: "127.0.0.1:39999" })));
    act(() => { restarting = result.current.restart(); });
    expect(result.current.configActionBusy).toBe(true);
    const accessCommands = () => vi.mocked(invoke).mock.calls.map(([command]) => command).filter((command) =>
      ["save_routing_preference", "save_config", "stop_proxy", "start_proxy"].includes(command));
    expect(accessCommands()).toEqual(["save_routing_preference"]);
    await act(async () => { release(); await saving; await restarting; });
    expect(accessCommands()).toEqual([
      "save_routing_preference", "save_config", "stop_proxy", "start_proxy",
    ]);
    expect(wire.prefer_local_supply).toBe(enabled);
    expect(wire.listen).toBe("127.0.0.1:39999");
    expect(result.current.config.prefer_local_supply).toBe(enabled);
    expect(result.current.configActionBusy).toBe(false);
    expect(result.current.hasChanges).toBe(false);
  });

  it("preserves drafts and other settings saved while a routing preference is in flight", async () => {
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.selectedChannel.id).toBe("refresh-channel"));
    const original = structuredClone(wire);
    const base = vi.mocked(invoke).getMockImplementation()!;
    let release!: () => void;
    const gate = new Promise<void>((resolve) => { release = resolve; });
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "save_routing_preference") await gate;
      return base(command, args);
    });
    const enabled = !result.current.config.prefer_local_supply;
    let saving!: Promise<void>;
    act(() => { saving = result.current.saveRoutingPreference("prefer_local_supply", enabled); });
    expect(result.current.configActionBusy).toBe(false);
    expect(result.current.routingPreferenceSaving.prefer_local_supply).toBe(true);
    expect(result.current.routingPreferenceSaving.allow_model_equivalence).toBeFalsy();
    expect(result.current.config.prefer_local_supply).not.toBe(enabled);
    act(() => result.current.setConfig((current) => ({
      ...current, listen: "127.0.0.1:39999", api_key: "draft-local-key",
      channels: current.channels.map((channel) => ({ ...channel, name: "Unsaved channel" })),
    })));
    const injection = { all_models: false, extra_models: ["vendor/selected"] };
    await act(async () => { await result.current.saveToolModelInjectionSettings(injection, vi.fn(), false); });
    await act(async () => { release(); await saving; });
    expect(result.current.config).toMatchObject({
      listen: "127.0.0.1:39999", api_key: "draft-local-key", prefer_local_supply: enabled,
      channels: [expect.objectContaining({ name: "Unsaved channel" })], tool_model_injection: injection,
    });
    expect(wire.listen).toBe(original.listen);
    expect(wire.api_key).toBe(original.api_key);
    expect(wire.channels).toEqual(original.channels);
    expect(wire.tool_model_injection).toEqual(injection);
    expect(result.current.hasChanges).toBe(true);
    expect(result.current.configActionBusy).toBe(false);
  });

  it("keeps a newly saved routing preference when an older injection snapshot arrives later", async () => {
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.selectedChannel.id).toBe("refresh-channel"));
    const base = vi.mocked(invoke).getMockImplementation()!;
    let release!: () => void;
    const gate = new Promise<void>((resolve) => { release = resolve; });
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      const response = await base(command, args);
      if (command === "save_tool_model_injection_settings") await gate;
      return response;
    });
    const injection = { all_models: false, extra_models: [] };
    let saving!: ReturnType<typeof result.current.saveToolModelInjectionSettings>;
    act(() => { saving = result.current.saveToolModelInjectionSettings(injection, vi.fn(), false); });
    await waitFor(() => expect(wire.tool_model_injection).toEqual(injection));
    const enabled = !result.current.config.prefer_local_supply;
    await act(async () => { await result.current.saveRoutingPreference("prefer_local_supply", enabled); });
    await act(async () => { release(); await saving; });
    expect(result.current.config.prefer_local_supply).toBe(enabled);
    expect(result.current.config.tool_model_injection).toEqual(injection);
    expect(result.current.hasChanges).toBe(false);
  });

  it("keeps the confirmed routing preference and reports a persistence failure", async () => {
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.selectedChannel.id).toBe("refresh-channel"));
    const original = result.current.config.allow_model_equivalence;
    const base = vi.mocked(invoke).getMockImplementation()!;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "save_routing_preference") throw new Error("fixture disk unavailable");
      return base(command, args);
    });
    await act(async () => { await result.current.saveRoutingPreference("allow_model_equivalence", !original); });
    expect(result.current.config.allow_model_equivalence).toBe(original);
    expect(wire.allow_model_equivalence).toBe(original);
    expect(result.current.hasChanges).toBe(false);
    expect(result.current.configActionBusy).toBe(false);
    expect(result.current.routingPreferenceSaving.allow_model_equivalence).toBe(false);
    expect(result.current.toast?.text).toContain("fixture disk unavailable");
  });

  it("saves explicit priority groups without restarting or validating channels", async () => {
    wire.channels.push({ ...originalChannel(), id: "priority-second", name: "Second" });
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.displayChannels).toHaveLength(2));
    expect(result.current.displayChannels.map((c) => c.local_priority)).toEqual([1, 2]);
    vi.mocked(invoke).mockClear();
    await act(async () => {
      expect(await result.current.saveLocalChannelPriorities({ "priority-second": 1 })).toBe(true);
    });
    expect(result.current.displayChannels.map((c) => c.local_priority)).toEqual([1, 1]);
    expect(invoke).toHaveBeenCalledWith("save_supplier_config", expect.objectContaining({
      channels: expect.arrayContaining([expect.objectContaining({ id: "priority-second", local_priority: 1 })]),
    }));
    expect(vi.mocked(invoke).mock.calls.some(([command]) => ["validate_channel_upstream", "start_supplier", "stop_supplier", "start_proxy", "stop_proxy"].includes(command))).toBe(false);
    expect(result.current.selectedChannelHasChanges).toBe(false);
  });

  it("rolls back failed priority persistence and leaves channel edits unsaved", async () => {
    const base = vi.mocked(invoke).getMockImplementation()!;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "save_supplier_config") throw new Error("fixture disk unavailable");
      return base(command, args);
    });
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.selectedChannel.id).toBe("refresh-channel"));
    await act(async () => {
      expect(await result.current.saveLocalChannelPriorities({ "refresh-channel": 8 })).toBe(false);
    });
    expect(result.current.selectedChannel.local_priority).toBe(1);
    expect(result.current.toast?.text).toContain("fixture disk unavailable");
    act(() => result.current.setConfig((current) => ({ ...current, channels: current.channels.map((c) => ({ ...c, name: "Unsaved draft" })) })));
    vi.mocked(invoke).mockClear();
    await act(async () => {
      expect(await result.current.saveLocalChannelPriorities({ "refresh-channel": 8 })).toBe(false);
    });
    expect(invoke).not.toHaveBeenCalledWith("save_supplier_config", expect.anything());
    expect(result.current.selectedChannel.name).toBe("Unsaved draft");
  });

  it("restores account identity without waiting for unrelated configuration loading", async () => {
    let resolveConfig!: (value: typeof wire) => void;
    const pendingConfig = new Promise<typeof wire>((resolve) => { resolveConfig = resolve; });
    const base = vi.mocked(invoke).getMockImplementation()!;
    const signedIn = { ...emptyAccountStatus(), state: "signed_in", user_id: "restored-user" };
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "get_config") return pendingConfig;
      if (command === "account_status" || command === "account_refresh") return signedIn;
      return base(command, args);
    });
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.account.statusReady).toBe(true));
    expect(result.current.account.status.user_id).toBe("restored-user");
    expect(result.current.selectedChannel.id).not.toBe("refresh-channel");
    await act(async () => { resolveConfig(structuredClone(wire)); });
    await waitFor(() => expect(result.current.selectedChannel.id).toBe("refresh-channel"));
  });

  it.each([true, false])("subscription import persists proxy choice %s through activation", async (useSystemProxy) => {
    const driver = sourceDriverById("openai_subscription");
    wire.channels[0] = sourceDriverInitialChannelV5(driver, {
      id: "refresh-channel", name: "Existing subscription", credentialRef: "old-managed.json",
    });
    const existing = structuredClone(wire.channels[0]);
    const prepared = sourceDriverInitialChannelV5(driver, {
      id: "new-subscription", name: "New subscription", credentialRef: ".pending/new.json",
    });
    prepared.use_system_proxy = useSystemProxy;
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
    act(() => { result.current.changeSubscriptionProxy(useSystemProxy); });
    vi.mocked(invoke).mockClear();
    await act(async () => {
      await result.current.importSubscriptionJson('{"refresh_token":"fixture"}');
    });
    expect(invoke).toHaveBeenCalledWith("import_subscription_token_json", {
      provider: "openai",
      tokenJson: '{"refresh_token":"fixture"}',
      useSystemProxy,
    });
    expect(invoke).toHaveBeenCalledWith("finalize_subscription_import", { channel: prepared, createDuplicate: true });
    expect(wire.channels).toHaveLength(2);
    expect(wire.channels[0]).toMatchObject(existing);
    expect(wire.channels[1]).toMatchObject({ id: "new-subscription", credential_ref: "new-managed.json", enabled: true, use_system_proxy: useSystemProxy });
    expect(result.current.subscriptionWizardOpen).toBe(false);
  });

  it("on-add presets use a fresh channel id without duplicate discovery", async () => {
    const driver = sourceDriverById("ollama");
    const base = vi.mocked(invoke).getMockImplementation()!;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "detect_channel_upstream") {
        return {
          models: ["local-fixture"], surface_results: [],
          model_capability_evidence: [], detection_evidence: [], quota_status: "unknown",
          remaining_ratio: 0, quota_windows: [], checks: ["catalog ok"], warnings: [],
        };
      }
      return base(command, args);
    });
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.selectedChannel.id).toBe("refresh-channel"));
    vi.mocked(invoke).mockClear();
    act(() => { result.current.addSubscriptionPreset(driver); });
    await waitFor(() => expect(wire.channels).toHaveLength(2));
    expect(wire.channels[1].id).toMatch(/^channel-/);
    expect(wire.channels[1].id).not.toBe("refresh-channel");
    expect(vi.mocked(invoke).mock.calls.map(([command]) => command)).toEqual([
      "detect_channel_upstream", "save_supplier_config",
    ]);
  });

  it.each(["muse_code", "copilot_subscription"] as const)(
    "%s opens its key sign-in flow from account subscriptions without probing", async (id) => {
      const { result } = renderHook(() => useAppController());
      await waitFor(() => expect(result.current.selectedChannel.id).toBe("refresh-channel"));
      vi.mocked(invoke).mockClear();
      await act(async () => { result.current.addSubscriptionPreset(sourceDriverById(id)); });
      expect(result.current.selectedChannel.source_driver).toBe(id);
      expect(result.current.selectedChannel.upstream_api_key).toBe("");
      expect(result.current.subscriptionWizardOpen).toBe(false);
      for (const command of ["detect_channel_upstream", "activate_supplier_channel", "save_supplier_config"]) {
        expect(vi.mocked(invoke).mock.calls.map(([name]) => name)).not.toContain(command);
      }
    },
  );

  it("keeps endpoint errors and retries on the existing source row", async () => {
    const base = vi.mocked(invoke).getMockImplementation()!;
    let fail = true;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "refresh_endpoint_registry") {
        if (fail) throw new Error("fixture endpoint unavailable");
        return { platform_id: "fixture", version: 1, endpoints: [], source: "fixture", refresh_warning: "" };
      }
      return base(command, args);
    });
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.selectedChannel.id).toBe("refresh-channel"));
    await act(() => result.current.refreshEndpointRegistry());
    expect(result.current.appMeta.endpointStatus).toBe("error");
    expect(result.current.appMeta.endpointLastError).toContain("fixture endpoint unavailable");
    expect(result.current.applicationDiagnostics).toEqual([]);
    fail = false;
    await act(() => result.current.refreshEndpointRegistry());
    expect(result.current.appMeta.endpointStatus).toBe("success");
    expect(result.current.appMeta.endpointLastError).toBeUndefined();
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
        "save_supplier_config",
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
      "save_supplier_config",
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

  it("does not turn a deliberate stop into an error on the next status refresh", async () => {
    vi.useFakeTimers();
    wire.proxy_auto_start = true;
    const base = vi.mocked(invoke).getMockImplementation()!;
    let running = true;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "proxy_status") return { running, listen: emptyConfig.listen };
      if (command === "stop_proxy") { running = false; return { running, listen: emptyConfig.listen }; }
      return base(command, args);
    });
    const { result } = renderHook(() => useAppController());
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    await act(() => result.current.stop());
    await act(async () => { await vi.advanceTimersByTimeAsync(10000); });
    expect(result.current.status.running).toBe(false);
    expect(result.current.localEntryIssue).toBeUndefined();
    expect(result.current.applicationDiagnostics).toEqual([]);
  });

  it("shows manual start failure with auto-start off and clears it after native recovery", async () => {
    vi.useFakeTimers();
    const base = vi.mocked(invoke).getMockImplementation()!;
    let running = false;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "proxy_status") return { running, listen: emptyConfig.listen };
      if (command === "save_config") return structuredClone(wire);
      if (command === "start_proxy") throw new Error("fixture address in use");
      return base(command, args);
    });
    const { result } = renderHook(() => useAppController());
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    await act(() => result.current.start());
    expect(result.current.localEntryIssue?.message).toContain("fixture address in use");
    expect(result.current.applicationDiagnostics).toEqual([]);
    await act(async () => { running = true; await vi.advanceTimersByTimeAsync(5000); });
    expect(result.current.localEntryIssue).toBeUndefined();
    await act(async () => { running = false; await vi.advanceTimersByTimeAsync(5000); });
    expect(result.current.localEntryIssue).toBeUndefined();
  });

  it("does not show a platform startup failure for Local channels", async () => {
    vi.useFakeTimers();
    wire.supplier_auto_start = true;
    wire.channels[0].enabled = true;
    wire.channels[0].share_enabled = true;
    const base = vi.mocked(invoke).getMockImplementation()!;
    let runtime = { running: false, starting: false, channels: [], route_statuses: [], last_error: "" };
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "supplier_status") return runtime;
      if (command === "start_supplier") {
        runtime = { ...runtime, last_error: "no channel passed supplier validation" };
        throw new Error(runtime.last_error);
      }
      return base(command, args);
    });
    const { result } = renderHook(() => useAppController());
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    expect(result.current.supplierStartupIssue).toBeUndefined();
    expect(result.current.applicationDiagnostics).toEqual([]);
    runtime = { ...runtime, running: true, last_error: "" };
    await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
    expect(result.current.supplierStartupIssue).toBeUndefined();
    expect(result.current.applicationDiagnostics).toEqual([]);
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
