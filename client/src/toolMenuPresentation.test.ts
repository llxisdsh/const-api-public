import { describe, expect, test } from "vitest";
import {
  DEFAULT_TOOL_PROTOCOLS,
  codexModelSourceFromStatus,
  toolConfigDetailState,
  toolConfigurationActionKey,
  toolPrimaryAction,
  toolProtocolMenuState,
  toolSyncsModelsOnLaunch,
} from "./toolMenuPresentation";

describe("tool menu presentation", () => {
  test("Codex injection is opt-in and follows the applied configuration", () => {
    expect(codexModelSourceFromStatus(undefined)).toBe("codex");
    expect(codexModelSourceFromStatus({ details: {} })).toBe("codex");
    expect(codexModelSourceFromStatus({ details: { codex_model_source: "const" } })).toBe("const");
    expect(codexModelSourceFromStatus({ details: { codex_model_source: "future" } })).toBe("codex");
  });
  test("uses the strongest native protocol available by default", () => {
    expect(DEFAULT_TOOL_PROTOCOLS.opencode).toBe("openai_responses");
    expect(DEFAULT_TOOL_PROTOCOLS["claude-science"]).toBe("anthropic_messages");
    expect(DEFAULT_TOOL_PROTOCOLS.openclaw).toBe("openai_responses");
    expect(DEFAULT_TOOL_PROTOCOLS.copilot).toBe("openai_chat");
    expect(DEFAULT_TOOL_PROTOCOLS.cline).toBe("openai_responses");
    expect(DEFAULT_TOOL_PROTOCOLS.goose).toBe("openai_chat");
    expect(DEFAULT_TOOL_PROTOCOLS.raven).toBe("openai_chat");
    expect(DEFAULT_TOOL_PROTOCOLS.reasonix).toBe("openai_chat");
    expect(DEFAULT_TOOL_PROTOCOLS["deepseek-harness"]).toBe("openai_chat");
    expect(DEFAULT_TOOL_PROTOCOLS.pi).toBe("openai_responses");
    expect(DEFAULT_TOOL_PROTOCOLS["open-interpreter"]).toBe("openai_chat");
    expect(DEFAULT_TOOL_PROTOCOLS["mistral-vibe"]).toBe("openai_chat");
    expect(DEFAULT_TOOL_PROTOCOLS["open-design"]).toBe("openai_responses");
    expect(DEFAULT_TOOL_PROTOCOLS.hermes).toBe("openai_chat");
    expect(DEFAULT_TOOL_PROTOCOLS.vscode).toBe("openai_responses");
    expect(DEFAULT_TOOL_PROTOCOLS.kimicode).toBe("openai_responses");
    expect(DEFAULT_TOOL_PROTOCOLS.mimocode).toBe("openai_chat");
    expect(DEFAULT_TOOL_PROTOCOLS.qwencode).toBe("openai_chat");
    expect(DEFAULT_TOOL_PROTOCOLS.openscience).toBe("openai_chat");
    expect(DEFAULT_TOOL_PROTOCOLS["vibe-trading"]).toBe("openai_chat");
    expect(DEFAULT_TOOL_PROTOCOLS.zcode).toBe("openai_chat");
  });

  test("presents a single supported protocol as fixed", () => {
    const state = toolProtocolMenuState("codex", "openai_responses", "openai_responses");

    expect(state.protocols).toEqual(["openai_responses"]);
    expect(state.fixed).toBe(true);
    expect(state.summary).toBe("Responses · 固定");
    expect(state.changed).toBe(false);
  });

  test("exposes every protocol supported by DeepSeek Harness", () => {
    const state = toolProtocolMenuState("deepseek-harness", undefined, "openai_chat");

    expect(state.protocols).toEqual([
      "openai_chat",
      "openai_responses",
      "anthropic_messages",
    ]);
    expect(state.selected).toBe("openai_chat");
    expect(state.fixed).toBe(false);
    expect(state.changed).toBe(false);
  });

  test("marks a different selected protocol as waiting to be applied", () => {
    const state = toolProtocolMenuState("opencode", "openai_responses", "openai_chat");

    expect(state.fixed).toBe(false);
    expect(state.summary).toBe("Responses · 待配置");
    expect(state.changed).toBe(true);
  });

  test("summarizes generic config details from checked files", () => {
    const state = toolConfigDetailState({
      already_configured: true,
      files: [],
      file_statuses: [
        { path: "C:/Users/me/.config/tool.json", already_configured: true },
        { path: "C:/Users/me/.config/tool.env", already_configured: true },
      ],
    }, false);

    expect(state.status).toBe("已配置");
    expect(state.tone).toBe("ready");
    expect(state.primaryPath).toBe("C:/Users/me/.config/tool.json");
    expect(state.fileCount).toBe(2);
  });

  test("emphasizes inherited environment variables without exposing values", () => {
    const warning = "检测到当前环境已定义：GEMINI_API_KEY。外部启动可能无法使用 CONST API。";
    const state = toolConfigDetailState({
      already_configured: true,
      details: {
        defined_environment_variables: "GEMINI_API_KEY",
        external_launch_warning: warning,
      },
    }, false);

    expect(state.status).toBe("已配置 · 外部启动有风险");
    expect(state.tone).toBe("ready");
    expect(state.externalLaunchWarning).toBe(warning);
    expect(state.externalLaunchWarning).not.toContain("secret");
  });

  test("distinguishes checking from a failed config check", () => {
    expect(toolConfigDetailState(undefined, true)).toMatchObject({
      status: "检查中",
      tone: "unknown",
    });
    expect(toolConfigDetailState(undefined, false)).toMatchObject({
      status: "等待检查",
      tone: "unknown",
    });
    expect(toolConfigDetailState(null, false)).toMatchObject({
      status: "检查失败",
      tone: "error",
    });
  });

  test("uses the gray missing-program state consistently in the menu", () => {
    expect(toolConfigDetailState({
      already_configured: false,
      details: { program_located: "false" },
    }, false, true)).toMatchObject({
      status: "未定位程序",
      tone: "missing",
    });
  });

  test("presents any pending tool setting through the shared configuration state", () => {
    expect(toolConfigDetailState({ already_configured: true }, false, true)).toMatchObject({
      status: "待配置",
      tone: "pending",
    });
  });

  test("routes the primary tool action through locate, configure, then launch", () => {
    expect(toolPrimaryAction({
      already_configured: true,
      details: { program_located: "false" },
    })).toBe("locate");
    expect(toolPrimaryAction(undefined)).toBe("configure_and_launch");
    expect(toolPrimaryAction(null)).toBe("configure_and_launch");
    expect(toolPrimaryAction({ already_configured: false })).toBe("configure_and_launch");
    expect(toolPrimaryAction({ already_configured: true }, true)).toBe("configure_and_launch");
    expect(toolPrimaryAction({ already_configured: true })).toBe("launch");
  });

  test("reconciles only tools whose backend status declares model synchronization", () => {
    expect(toolSyncsModelsOnLaunch({ details: { model_sync_policy: "catalog" } })).toBe(true);
    expect(toolSyncsModelsOnLaunch({ details: { model_sync_policy: "selected" } })).toBe(true);
    expect(toolSyncsModelsOnLaunch({ details: { model_sync_policy: "none" } })).toBe(false);
    expect(toolSyncsModelsOnLaunch({ details: {} })).toBe(false);
    expect(toolSyncsModelsOnLaunch(undefined)).toBe(false);
  });

  test("labels configuration as reconfiguration only after a valid setup exists", () => {
    expect(toolConfigurationActionKey({ already_configured: true })).toBe("access.reconfigure");
    expect(toolConfigurationActionKey({ already_configured: false })).toBe("access.configure");
    expect(toolConfigurationActionKey(undefined)).toBe("access.configure");
  });
});
