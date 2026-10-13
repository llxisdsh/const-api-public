import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { useAppController } from "./appController";
import { emptyConfig } from "./appHelpers";
import { emptyAccountStatus } from "./account/accountTypes";
import { sourceDriverById, sourceDriverInitialChannelV5 } from "./sourceDrivers";
import type { ToolApplyResult } from "./appTypes";
import { toolHasManagedConfig } from "./toolMenuPresentation";
import { TOOL_ORDER_STORAGE_KEY, TOOL_VISIBILITY_STORAGE_KEY } from "./toolVisibility";
import { i18n } from "./i18n";

function configStatus(tool: string, ready: boolean, details: Record<string, string> = {}): ToolApplyResult {
  return { tool, already_configured: ready, files: [], file_statuses: [], backups: [], details };
}

vi.mock("./runtimeProfile", async (importOriginal) => ({
  ...await importOriginal<typeof import("./runtimeProfile")>(),
  RELEASE_UPDATES_ENABLED: false,
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));

describe("shared tool program selection", () => {
  let selected: string;
  const desktop = "D:/apps/Xiaomi MiMo.exe";
  beforeEach(() => {
    selected = "";
    vi.stubGlobal("__TAURI_INTERNALS__", {});
    vi.mocked(invoke).mockReset().mockImplementation(async (command, args) => {
      if (command === "get_config") return {
        ...emptyConfig, config_version: 5, proxy_auto_start: false, supplier_auto_start: false,
        channels: [sourceDriverInitialChannelV5(sourceDriverById("openrouter"), {
          id: "program-fixture", name: "Fixture", credentialRef: "fixture",
        })],
      };
      if (command === "proxy_status") return { running: false, listen: emptyConfig.listen };
      if (command === "supplier_status") return { running: false, starting: false, channels: [], route_statuses: [] };
      if (command === "account_status") return emptyAccountStatus();
      if (command === "get_release_source_status") return { updateSources: [], endpointSources: [] };
      if (command === "autostart_status") return false;
      if (command === "startup_log_from_ui" || command === "sync_native_theme") return null;
      if (command.startsWith("fetch_")) return { data: [] };
      if (command === "check_tool_config") return { already_configured: true, files: [], details: {} };
      if (command === "save_tool_program") selected = (args as { path: string }).path;
      if (command === "locate_tool_program" || command === "save_tool_program") return {
        tool: "mimocode", selected_path: selected,
        candidates: [desktop, "D:/apps/Xiaomi MiMo AI.exe"].map((path) => ({
          path, kind: "windows_exe", exists: true, launchable: true, selected: path === selected,
        })),
      };
      if (command === "begin_tool_config_operation") return { operation_id: "choose-model-program" };
      if (command === "wait_tool_config_operation_update") return {
        operation_id: "choose-model-program", state: "completed", terminal: true,
        progress_version: 1, progress_stage: "completed",
      };
      if (command === "execute_tool_config_operation" || command === "start_tool_program") {
        if (!selected) throw new Error("TOOL_PROGRAM_SELECTION_REQUIRED: multiple desktop editions");
        if (command === "start_tool_program") return selected;
        return { result: { tool: "mimocode", already_configured: true, files: [], file_statuses: [], backups: [],
          details: { restart_command: "launched", restart_launcher_path: selected } } };
      }
      throw new Error(`Unexpected fixture command: ${command}`);
    });
  });
  afterEach(() => vi.unstubAllGlobals());

  it.each([
    ["zh-CN", "WorkBuddy", "WorkBuddy AI"],
    ["en-US", "WorkBuddy", "WorkBuddy AI"],
  ])("keeps concise WorkBuddy names in the %s dock and picker", async (language, chinaTitle, internationalTitle) => {
    const previousLanguage = i18n.language;
    await i18n.changeLanguage(language);
    const { result, unmount } = renderHook(() => useAppController());
    try {
      await waitFor(() => expect(result.current.selectedChannel.id).toBe("program-fixture"));
      for (const cards of [result.current.toolCards, [...result.current.visibleToolCards, ...result.current.hiddenToolCards]]) {
        expect(cards.find(({ tool }) => tool === "workbuddy")?.title).toBe(chinaTitle);
        expect(cards.find(({ tool }) => tool === "workbuddy-ai")?.title).toBe(internationalTitle);
      }
    } finally {
      unmount();
      await i18n.changeLanguage(previousLanguage);
    }
  });

  it.each([null, "[]"])("excludes retired tools from the dock and picker with stored visibility %s", async (hidden) => {
    const previousHidden = localStorage.getItem(TOOL_VISIBILITY_STORAGE_KEY);
    const previousOrder = localStorage.getItem(TOOL_ORDER_STORAGE_KEY);
    const restore = (key: string, value: string | null) => {
      if (value === null) localStorage.removeItem(key);
      else localStorage.setItem(key, value);
    };
    restore(TOOL_VISIBILITY_STORAGE_KEY, hidden);
    localStorage.setItem(TOOL_ORDER_STORAGE_KEY, JSON.stringify(["trae-work", "trae", "trae-cn"]));
    const { result, unmount } = renderHook(() => useAppController());
    try {
      await waitFor(() => expect(result.current.selectedChannel.id).toBe("program-fixture"));
      expect(result.current.visibleToolCards.some(({ tool }) => tool === "trae-work")).toBe(false);
      expect(result.current.hiddenToolCards.some(({ tool }) => tool === "trae-work")).toBe(false);
      const offered = [...result.current.visibleToolCards, ...result.current.hiddenToolCards].map(({ tool }) => tool);
      expect(offered).toContain("trae");
      expect(offered).toContain("trae-cn");
      expect(invoke).not.toHaveBeenCalledWith("check_tool_config", expect.objectContaining({ tool: "trae-work" }));
    } finally {
      unmount();
      restore(TOOL_VISIBILITY_STORAGE_KEY, previousHidden);
      restore(TOOL_ORDER_STORAGE_KEY, previousOrder);
    }
  });

  it("asks then resumes opening, even after rescan, without configuring", async () => {
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.selectedChannel.id).toBe("program-fixture"));
    await act(() => result.current.startToolProgram("mimocode", "MiMo Code", false));
    expect(result.current.toolLocator?.result?.candidates).toHaveLength(2);
    expect(result.current.toolLocator?.continueWithLaunch).toBe(true);
    await act(() => result.current.openToolLocator("mimocode", "MiMo Code", true));
    await act(() => result.current.saveToolProgram("mimocode", "MiMo Code", desktop, true));
    expect(result.current.toolLocator).toBeNull();
    expect(invoke).toHaveBeenLastCalledWith("start_tool_program", { tool: "mimocode", useConfiguredEnvironment: false });
    expect(vi.mocked(invoke).mock.calls.some(([command]) => command === "execute_tool_config_operation")).toBe(false);
  });

  it("saves injection preferences alone and syncs owned catalogs using applied, not draft, selections", async () => {
    const base = vi.mocked(invoke).getMockImplementation()!;
    const settings = { all_models: false, extra_models: ["extra-model"] };
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "save_tool_model_injection_settings") {
        const config = await base("get_config", undefined) as object;
        return { ...config, tool_model_injection: settings };
      }
      if (command === "check_tool_config") {
        const tool = (args as { tool: string }).tool;
        return configStatus(tool, tool === "opencode", {
          has_managed_config: String(["opencode", "workbuddy-ai", "codex", "claude", "claude-desktop", "gemini", "hermes", "copilot"].includes(tool)),
          tool_protocol: "openai_chat", codex_model_source: "codex",
        });
      }
      if (command === "execute_tool_config_operation") {
        if ((args as { tool: string }).tool === "workbuddy-ai") throw new Error("fixture sync failure");
        return { result: configStatus("opencode", true, { has_managed_config: "true" }) };
      }
      return base(command, args);
    });
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.selectedChannel.id).toBe("program-fixture"));
    await act(() => {
      result.current.setConfig((config) => ({ ...config, api_key: "unsaved-local-key" }));
      result.current.toolProtocolDraftTouchedRef.current.opencode = true;
      result.current.setToolProtocolSelections((current) => ({ ...current, opencode: "anthropic_messages" }));
    });
    vi.mocked(invoke).mockClear();
    let syncResult;
    await act(async () => { syncResult = await result.current.saveToolModelInjectionSettings(settings, vi.fn()); });
    expect(syncResult).toEqual({ synced: 1, errors: ["WorkBuddy AI: fixture sync failure"] });
    expect(result.current.config.api_key).toBe("unsaved-local-key");
    expect(result.current.config.tool_model_injection).toEqual(settings);
    expect(result.current.toolProtocolSelections.opencode).toBe("anthropic_messages");
    expect(invoke).toHaveBeenCalledWith("save_tool_model_injection_settings", { settings });
    const executions = vi.mocked(invoke).mock.calls.filter(([command]) => command === "execute_tool_config_operation").map(([, args]) => args);
    expect(executions).toHaveLength(2);
    expect(executions[0]).toMatchObject({ tool: "opencode", action: "apply", restart: false, protocol: "openai_chat" });
    expect(executions[1]).toMatchObject({ tool: "workbuddy-ai", action: "apply", restart: false });
    expect(vi.mocked(invoke).mock.calls.some(([command]) => ["save_config", "start_proxy", "stop_proxy", "refresh_supplier_registration", "start_tool_program"].includes(command))).toBe(false);
  });

  it("can persist the injection switch without discovering or rewriting tools", async () => {
    const base = vi.mocked(invoke).getMockImplementation()!;
    const settings = { all_models: false, extra_models: [] };
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "save_tool_model_injection_settings") {
        return { ...await base("get_config", undefined) as object, tool_model_injection: settings };
      }
      return base(command, args);
    });
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.selectedChannel.id).toBe("program-fixture"));
    await waitFor(() => expect(result.current.toolConfigChecking).toBe(false));
    vi.mocked(invoke).mockClear();
    await act(async () => {
      expect(await result.current.saveToolModelInjectionSettings(settings, vi.fn(), false)).toEqual({ synced: 0, errors: [] });
    });
    expect(vi.mocked(invoke).mock.calls.map(([command]) => command)).toEqual(["save_tool_model_injection_settings"]);
    expect(result.current.config.tool_model_injection).toEqual(settings);
  });

  it("reports incomplete injection readback instead of counting it as synced", async () => {
    const base = vi.mocked(invoke).getMockImplementation()!;
    const settings = { all_models: false, extra_models: ["extra-model"] };
    let applied = false;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "save_tool_model_injection_settings") {
        return { ...await base("get_config", undefined) as object, tool_model_injection: settings };
      }
      if (command === "check_tool_config") {
        const tool = (args as { tool: string }).tool;
        return configStatus(tool, tool === "opencode" && !applied, {
          has_managed_config: String(tool === "opencode"),
        });
      }
      if (command === "execute_tool_config_operation") {
        applied = true;
        return { result: configStatus("opencode", true, { has_managed_config: "true" }) };
      }
      return base(command, args);
    });
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.selectedChannel.id).toBe("program-fixture"));
    let syncResult;
    await act(async () => { syncResult = await result.current.saveToolModelInjectionSettings(settings, vi.fn()); });
    expect(syncResult).toMatchObject({ synced: 0, errors: [expect.stringContaining("配置仍不完整")] });
    expect(result.current.toolConfigStatuses.opencode?.already_configured).toBe(false);
    expect(toolHasManagedConfig(result.current.toolConfigStatuses.opencode)).toBe(true);
    expect(result.current.config.tool_model_injection).toEqual(settings);
  });

  it("asks before configuration, resumes configure-and-launch, then remembers the choice", async () => {
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.selectedChannel.id).toBe("program-fixture"));
    await act(() => result.current.launchToolConfig("mimocode", "MiMo Code"));
    expect(result.current.toolLocator?.result?.candidates).toHaveLength(2);
    await act(() => result.current.saveToolProgram("mimocode", "MiMo Code", desktop, true));
    expect(result.current.toolLocator).toBeNull();
    const executions = vi.mocked(invoke).mock.calls.filter(([name]) => name === "execute_tool_config_operation");
    expect(executions).toHaveLength(2);
    expect(executions[1][1]).toMatchObject({ tool: "mimocode", action: "apply", restart: true });
    await act(() => result.current.startToolProgram("mimocode", "MiMo Code", true));
    expect(result.current.toolLocator).toBeNull();
  });

  it("rechecks on use and repairs incomplete configuration despite its green dot", async () => {
    selected = desktop;
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.toolConfigStatuses.mimocode?.already_configured).toBe(true));
    const base = vi.mocked(invoke).getMockImplementation()!;
    let checked = false;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "check_tool_config" && !checked) {
        checked = true;
        return configStatus("mimocode", false, { has_managed_config: "true" });
      }
      return base(command, args);
    });
    await act(() => result.current.useTool("mimocode", "MiMo Code"));
    expect(checked).toBe(true);
    expect(invoke).toHaveBeenCalledWith("execute_tool_config_operation", expect.objectContaining({ tool: "mimocode", action: "apply", restart: true }));
    expect(result.current.toolConfigStatuses.mimocode?.already_configured).toBe(true);
  });

  it("does not reconfigure a freshly checked ready tool without model synchronization", async () => {
    selected = desktop;
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.toolConfigChecking).toBe(false));
    vi.mocked(invoke).mockClear();
    await act(() => result.current.useTool("mimocode", "MiMo Code"));
    const commands = vi.mocked(invoke).mock.calls.map(([command]) => command);
    expect(commands).toEqual(["check_tool_config", "start_tool_program"]);
  });

  it("keeps known configuration when a use-time check fails, without launching or rewriting", async () => {
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.toolConfigStatuses.mimocode?.already_configured).toBe(true));
    vi.mocked(invoke).mockClear().mockRejectedValue(new Error("read temporarily unavailable"));
    await act(() => result.current.useTool("mimocode", "MiMo Code"));
    const status = result.current.toolConfigStatuses.mimocode;
    expect(toolHasManagedConfig(status)).toBe(true);
    expect(status?.already_configured).toBe(false);
    expect(status?.details?.config_check_failed).toBe("true");
    expect(vi.mocked(invoke).mock.calls.map(([command]) => command)).toEqual(["check_tool_config"]);
  });

  it("merges a bulk check with a newer single-tool check instead of discarding other cards", async () => {
    selected = desktop;
    const base = vi.mocked(invoke).getMockImplementation()!;
    let releaseBulk!: (status: ToolApplyResult) => void;
    let mimoChecks = 0;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "check_tool_config") {
        const { tool } = args as { tool: string };
        if (tool === "trae") return new Promise<ToolApplyResult>((resolve) => { releaseBulk = resolve; });
        if (tool === "mimocode") return configStatus(tool, true, { revision: String(++mimoChecks) });
      }
      return base(command, args);
    });
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(releaseBulk).toBeTypeOf("function"));
    await act(() => result.current.useTool("mimocode", "MiMo Code"));
    expect(result.current.toolConfigStatuses.mimocode?.details?.revision).toBe("2");
    await act(async () => releaseBulk(configStatus("trae", false)));
    await waitFor(() => expect(result.current.toolConfigChecking).toBe(false));
    expect(result.current.toolConfigStatuses.mimocode?.details?.revision).toBe("2");
    expect(result.current.toolConfigStatuses.cline?.already_configured).toBe(true);
    expect(result.current.toolConfigStatuses.trae?.already_configured).toBe(false);
  });

  it("refreshes partial writes after failure and keeps concurrent tool checks independent", async () => {
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.toolConfigStatuses.mimocode?.already_configured).toBe(true));
    const base = vi.mocked(invoke).getMockImplementation()!;
    const releases: Record<string, (status: ToolApplyResult) => void> = {};
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "execute_tool_config_operation") throw new Error("partial write failed");
      if (command === "check_tool_config") {
        const { tool } = args as { tool: string };
        return new Promise<ToolApplyResult>((resolve) => { releases[tool] = resolve; });
      }
      return base(command, args);
    });
    let first!: Promise<void>;
    let second!: Promise<void>;
    act(() => {
      first = result.current.applyToolConfig("mimocode", "MiMo Code");
      second = result.current.applyToolConfig("kimicode", "Kimi Code");
    });
    await waitFor(() => expect(Object.keys(releases)).toHaveLength(2));
    await act(async () => {
      releases.kimicode(configStatus("kimicode", false, { has_managed_config: "true" }));
      await second;
      releases.mimocode(configStatus("mimocode", false, { has_managed_config: "true" }));
      await first;
    });
    for (const tool of ["mimocode", "kimicode"]) {
      expect(result.current.toolConfigStatuses[tool]?.already_configured).toBe(false);
      expect(toolHasManagedConfig(result.current.toolConfigStatuses[tool])).toBe(true);
    }
  });

  it("does not use a pre-write apply result as verified readiness when refresh fails", async () => {
    selected = desktop;
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.toolConfigStatuses.mimocode?.already_configured).toBe(true));
    const base = vi.mocked(invoke).getMockImplementation()!;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "execute_tool_config_operation") return { result: {
        ...configStatus("mimocode", false), files: ["test/config.json"],
      } };
      if (command === "check_tool_config") throw new Error("status unavailable after apply");
      return base(command, args);
    });
    await act(() => result.current.applyToolConfig("mimocode", "MiMo Code"));
    const status = result.current.toolConfigStatuses.mimocode;
    expect(status?.already_configured).toBe(false);
    expect(status?.details?.config_check_failed).toBe("true");
    expect(toolHasManagedConfig(status)).toBe(true);
  });

  it("clears presence after verified removal and refreshes partial failed removal", async () => {
    const { result } = renderHook(() => useAppController());
    await waitFor(() => expect(result.current.toolConfigStatuses.mimocode?.already_configured).toBe(true));
    const base = vi.mocked(invoke).getMockImplementation()!;
    let failed = true;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command === "execute_tool_config_operation") {
        if (failed) throw new Error("partial removal failed");
        return { result: configStatus("mimocode", false, { manifest_entries_restored: "1" }) };
      }
      if (command === "check_tool_config") return configStatus("mimocode", false, { has_managed_config: String(failed) });
      return base(command, args);
    });
    for (const shouldFail of [true, false]) {
      failed = shouldFail;
      let removal!: Promise<void>;
      act(() => { removal = result.current.removeToolConfig("mimocode", "MiMo Code"); });
      await act(async () => result.current.resolveToolRemoveDialog("restore_pre_const"));
      await waitFor(() => expect(result.current.confirmDialog).not.toBeNull());
      expect(toolHasManagedConfig(result.current.toolConfigStatuses.mimocode)).toBe(shouldFail);
      await act(async () => {
        result.current.resolveConfirmDialog(true);
        await removal;
      });
    }
  });
});
