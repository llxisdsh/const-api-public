import { beforeAll, describe, expect, it, vi } from "vitest";
import mainSource from "./main.tsx?raw";
import controllerSource from "./appController.tsx?raw";
import zhLocaleSource from "./i18n/locales/zh-CN.json?raw";
import tauriMainSource from "../src-tauri/src/main.rs?raw";
import operationCoreSource from "../src-tauri/src/tool_config/production/core.rs?raw";

const rendererSource = `${mainSource}\n${controllerSource}`;
const sharedUseSource = controllerSource.slice(
  controllerSource.indexOf("  async function useTool("),
  controllerSource.indexOf("  async function removeToolConfig("),
);
const zhLocale = JSON.parse(zhLocaleSource) as {
  labels: Record<string, string>;
  toolOperations: {
    dialogs: Record<string, string>;
    cleanup: Record<string, string>;
  };
};
const zhToolOperations = zhLocale.toolOperations;

type ProgressStatus = {
  operation_id: string;
  state: string;
  terminal: boolean;
  cancellation_requested: boolean;
  progress_version: number;
  progress_stage: string;
  progress_completed?: number | null;
  progress_total?: number | null;
  progress_ratio?: number | null;
};

type ToolOperationExports = {
  advanceToolConfigProgress: (
    previous: { stage: string; ratio?: number | null } | undefined,
    next: {
      stage: string;
      completed?: number | null;
      total?: number | null;
      stageRatio?: number | null;
    },
  ) => {
    stage: string;
    completed?: number | null;
    total?: number | null;
    stageRatio?: number | null;
    ratio?: number | null;
  };
  toolConfigProgressLabel: (progress: {
    stage: string;
    completed?: number | null;
    total?: number | null;
    stageRatio?: number | null;
  }) => string;
  toolConfigProgressRatio: (progress?: {
    stage: string;
    completed?: number | null;
    total?: number | null;
    stageRatio?: number | null;
  } | null) => number | null;
  toolConfigVisualProgressRatio: (progress?: {
    stage: string;
    completed?: number | null;
    total?: number | null;
    stageRatio?: number | null;
    ratio?: number | null;
  } | null) => number | null;
  toolConfigOperationResultNotice: (
    title: string,
    restartAfterConfig: boolean,
    result: { details?: Record<string, string> },
  ) => { text: string; tone: string; duration: number; restoreHint?: boolean } | null;
  toolFirstConfigurationNotice: (input: {
    title: string;
    restartAfterConfig: boolean;
    wasConfiguredBefore: boolean;
    result: { details?: Record<string, string> };
    refreshedStatus: { details?: Record<string, string> };
    statusRefreshVerified: boolean;
  }) => {
    title: string;
    message: string;
    confirmText?: string;
    mode?: string;
  } | null;
  watchToolConfigOperation: (
    operationId: string,
    onUpdate: (status: ProgressStatus) => void,
    onError: (error: unknown) => void,
    control: {
      waitForUpdate: (
        operationId: string,
        afterVersion: number,
        waitMs: number,
      ) => Promise<ProgressStatus>;
    },
  ) => () => void;
  toolConfigSuccessNotice: (message: string) => string;
};

let toolOperationExports: ToolOperationExports;

beforeAll(async () => {
  const appWindow = window as typeof window & { __CONST_API_ROOT__?: { render: ReturnType<typeof vi.fn> } };
  appWindow.__CONST_API_ROOT__ = { render: vi.fn() };
  toolOperationExports = (await import("./main")) as unknown as ToolOperationExports;
});

describe("tool config operation IPC", () => {
  it("requests restart through the atomic backend operation", () => {
    expect(rendererSource).toMatch(/restart:\s*action === "launch"/);
    expect(rendererSource).not.toContain('"launch_tool_program_after_config_command"');
  });

  it("routes restart through the registered atomic Tauri command", () => {
    const handlers = tauriMainSource.match(/tauri::generate_handler!\[([\s\S]*?)\]\)/)?.[1] ?? "";
    expect(handlers.split(",").map((entry) => entry.trim())).toEqual(
      expect.arrayContaining([
        "begin_tool_config_operation",
        "execute_tool_config_operation",
        "cancel_tool_config_operation",
      ]),
    );
    const rustFunction = (name: string) =>
      tauriMainSource.match(new RegExp("\\bfn " + name + "\\b[\\s\\S]*?^}", "m"))?.[0] ?? "";
    function expectSteps(source: string, steps: RegExp[]) {
      for (const step of steps) {
        const match = step.exec(source);
        expect(match, step.source).not.toBeNull();
        if (match) source = source.slice(match.index + match[0].length);
      }
    }
    expectSteps(rustFunction("execute_tool_config_operation"), [
      /start_tool_config_operation\(&operation_id, &tool, action, restart\)/,
      /acquire_tool_config_operation_lease\(&lease_operation\)/,
      /execute_tool_config_operation_inner\(\s*&state,\s*&operation,/,
      /confirmation_token,/,
      /finish_tool_config_operation\(&operation, result\.is_ok\(\)\)/,
      /drop\(lease\)/,
    ]);
    const dispatch = rustFunction("execute_tool_config_operation_inner");
    expect(dispatch).toMatch(
      /if action == ToolConfigAction::Remove[\s\S]*?execute_atomic_tool_config_operation\(/,
    );
    const hermes = dispatch.match(/else if tool == "hermes" \{([\s\S]*?)\n    } else/)?.[1] ?? "";
    expectSteps(hermes, [
      /execute_previewed_tool_config_operation\(\s*operation,/,
      /confirmation_token\.as_deref\(\)/,
      /apply_hermes_config_for_protocol\([^)]*&image_models/,
      /launch_tool_program_for_atomic_operation/,
    ]);
    expectSteps(rustFunction("execute_previewed_tool_config_operation"), [
      /let apply_with_skill = \|\| \{[\s\S]*?apply\(\)\?[\s\S]*?sync_tool_image_skill\(/,
      /execute_atomic_tool_config_operation_with_preview\(\s*operation,\s*protocol,\s*&context_digest,\s*confirmation_token,/,
      /\|\| preview_tool_config_apply\(&?apply_with_skill\),\s*&?apply_with_skill,\s*launch,/,
    ]);
    expect(rustFunction("execute_codex_tool_config_operation_inner")).toContain(
      "execute_atomic_tool_config_operation(",
    );
    expect(rustFunction("execute_trae_tool_config_operation")).toContain(
      "execute_atomic_tool_config_operation_with_preview(",
    );
    expect(tauriMainSource).toMatch(
      /matches!\(\s*tool,\s*"workbuddy"\s*\|\s*"workbuddy-ai"\s*\|\s*"codebuddy"\s*\|\s*"codebuddy-cn"\s*\)/,
    );
    expect(tauriMainSource).toMatch(
      /"copilot"[\s\S]*?"raven"[\s\S]*?"pi"[\s\S]*?"cline"[\s\S]*?"reasonix"[\s\S]*?"deepseek-harness"[\s\S]*?"open-design"/,
    );
    expect(tauriMainSource).not.toContain(
      "launch_tool_program_after_config_command",
    );
  });

  it("keeps Codex migration off the async request runtime", () => {
    expect(tauriMainSource).toMatch(
      /execute_codex_tool_config_operation_inner[\s\S]*?spawn_blocking/,
    );
  });

  it("injects configured launch environment only for the direct-launch branch", () => {
    expect(sharedUseSource).toMatch(
      /action === "configure_and_launch"[\s\S]*?startToolProgram\(tool, title, true\)/,
    );
    expect(tauriMainSource).toMatch(
      /use_configured_environment[\s\S]*?matches!\([\s\S]*?tool\.as_str\(\)[\s\S]*?"claude" \| "claude-science" \| "gemini" \| "copilot" \| "goose"/,
    );
  });

  it("routes every primary action through a fresh check and the shared controller", () => {
    expect(mainSource).toContain("useTool(card.tool, card.title, configurationChanged)");
    expect(sharedUseSource).toMatch(
      /await refreshSingleToolConfigStatus\(tool, undefined, false\)[\s\S]*?toolPrimaryAction\(status, configurationChanged\)/,
    );
    expect(sharedUseSource).toMatch(
      /action === "locate"[\s\S]*?openToolLocator\(tool, title, true\)/,
    );
    expect(sharedUseSource).toMatch(
      /action === "configure_and_launch"[\s\S]*?launchToolConfig\(tool, title\)/,
    );
    expect(mainSource).toMatch(
      /onConfigure=\{\(\) => \{[\s\S]*?prepareToolUse\(\)[\s\S]*?applyToolConfig\(card\.tool, card\.title\)/,
    );
    expect(controllerSource).toContain("cards = visibleToolCards");
    expect(controllerSource).toContain("Promise.all(cards.map(async (card) =>");
    expect(controllerSource).toMatch(
      /continueWithLaunch[\s\S]*?setToolLocator\(null\)[\s\S]*?launchToolConfig\(tool, title\)/,
    );
    expect(controllerSource).not.toContain("stableErrorCode(error)");
  });

  it("applies and removes Claude model choices through the shared configuration actions", () => {
    expect(controllerSource).toMatch(
      /effectiveClaudeSettings = tool === "claude"[\s\S]*?claudeSettings \?\? claudeModelDraft/,
    );
    expect(controllerSource).toMatch(
      /resetClaudeSettings = tool === "claude"[\s\S]*?DEFAULT_CLAUDE_MODEL_SETTINGS[\s\S]*?storeAppliedClaudeModelSettings\(resetClaudeSettings\)/,
    );
    expect(rendererSource).not.toContain("applyClaudeModelSettings");
    expect(rendererSource).not.toContain("launchClaudeCodeRuntime");
  });

  it("explains how to disconnect after successful configuration from either entry point", () => {
    expect(toolOperationExports.toolConfigSuccessNotice("Codex 配置完成。")).toBe(
      "Codex 配置完成。 如需清理 CONST API 接入，右键该工具选择“取消配置”即可。",
    );
    expect(sharedUseSource).toContain("launchToolConfig(tool, title)");
    expect(mainSource).toContain("applyToolConfig(card.tool, card.title)");
    expect(controllerSource).toMatch(
      /async function configureTool[\s\S]*?showConfiguredResult[\s\S]*?toolConfigSuccessNotice/,
    );
  });

  it("reports an unchanged launch without a misleading restore hint", () => {
    expect(toolOperationExports.toolConfigOperationResultNotice(
      "VS Code",
      true,
      { details: { configuration_unchanged: "true" } },
    )).toEqual({
      text: "VS Code 已启动。",
      tone: "success",
      duration: 4200,
      restoreHint: false,
    });
  });

  it("shows one guided result only after a first changed configuration launches", () => {
    const completed = {
      details: {
        configuration_changed: "true",
        restart_command: "launch",
        restart_launcher_path: "C:/Tools/Code.exe",
      },
    };
    expect(toolOperationExports.toolFirstConfigurationNotice({
      title: "VS Code",
      restartAfterConfig: true,
      wasConfiguredBefore: false,
      result: completed,
      refreshedStatus: { details: {} },
      statusRefreshVerified: true,
    })).toEqual({
      title: "VS Code 已配置并启动",
      message: expect.stringContaining("以后直接打开 VS Code 也可使用 CONST API。"),
      hint: "使用工具时请保持 CONST API 运行。",
      confirmText: "我知道了",
      mode: "notice",
    });
    expect(toolOperationExports.toolFirstConfigurationNotice({
      title: "VS Code",
      restartAfterConfig: true,
      wasConfiguredBefore: true,
      result: completed,
      refreshedStatus: { details: {} },
      statusRefreshVerified: true,
    })).toBeNull();
    expect(toolOperationExports.toolFirstConfigurationNotice({
      title: "VS Code",
      restartAfterConfig: true,
      wasConfiguredBefore: false,
      result: { details: { configuration_unchanged: "true" } },
      refreshedStatus: { details: {} },
      statusRefreshVerified: true,
    })).toBeNull();
  });

  it.each(["const_api_launch_environment", "provider_and_const_api_launch_environment"])("explains launch-only configuration for %s", (scope) => {
    const notice = toolOperationExports.toolFirstConfigurationNotice({
      title: "Copilot",
      restartAfterConfig: true,
      wasConfiguredBefore: false,
      result: { details: {
        configuration_changed: "true",
        restart_command: "launch",
        restart_launcher_path: "C:/Tools/copilot.exe",
        configuration_scope: scope,
      } },
      refreshedStatus: { details: {} },
      statusRefreshVerified: true,
    });
    expect(notice?.message).toContain("以后请从 CONST API 启动 Copilot");
    expect(notice?.message).toContain("直接从外部启动时，仍使用它原来的设置");
    expect(notice?.message).not.toContain("也可使用 CONST API");
  });

  it("does not promise external launches when an environment conflict is detected", () => {
    expect(toolOperationExports.toolFirstConfigurationNotice({
      title: "Claude Code",
      restartAfterConfig: true,
      wasConfiguredBefore: false,
      result: {
        details: {
          configuration_changed: "true",
          restart_command: "launch",
          restart_launcher_path: "C:/Tools/claude.exe",
        },
      },
      refreshedStatus: {
        details: { external_launch_warning: "environment conflict" },
      },
      statusRefreshVerified: true,
    })?.message).toContain(
      "配置已生效，但外部环境设置可能覆盖它。请优先从 CONST API 启动 Claude Code。",
    );
  });

  it("shows one guided result dialog after every completed remove outcome", () => {
    expect(controllerSource).toMatch(
      /async function removeToolConfig[\s\S]*?await askConfirm\(\{[\s\S]*?removeResultSuccessTitle[\s\S]*?mode: "notice"/,
    );
    expect(controllerSource).toMatch(
      /catch \(checkError\)[\s\S]*?await askConfirm\(\{[\s\S]*?removeResultAttentionMessage[\s\S]*?mode: "notice"/,
    );
    expect(controllerSource).toMatch(
      /catch \(error\)[\s\S]*?removeResultFailedTitle[\s\S]*?removeResultFailureMessage[\s\S]*?mode: "notice"/,
    );
  });

  it("folds normal process shutdown into the first removal confirmation", () => {
    expect(controllerSource).toMatch(
      /closeRunningProcessConfirmed = false[\s\S]*?processCloseAlreadyConfirmed =[\s\S]*?action === "remove" && closeRunningProcessConfirmed/,
    );
    expect(controllerSource).toMatch(
      /const confirmed = processCloseAlreadyConfirmed\s*\? true\s*:\s*await askConfirm/,
    );
    expect(controllerSource).toMatch(
      /executeToolConfigOperation\([\s\S]*?"remove",[\s\S]*?undefined,\s*true,/,
    );
    expect(zhToolOperations.cleanup.closeWarning).toContain("自动关闭相关进程");
  });

  it("reports a non-blocking Codex session migration problem after native cleanup", () => {
    expect(controllerSource).toMatch(
      /sessionSyncError = result\.details\?\.session_sync_error[\s\S]*?labels\.sessionMigrationFailed/,
    );
    expect(controllerSource).toMatch(
      /sessionIndexInvalidRows = Number\.parseInt\([\s\S]*?session_migrated_sqlite_invalid_rows[\s\S]*?labels\.sessionIndexInvalidRows/,
    );
    expect(zhLocale.labels.sessionIndexInvalidRows).toContain("缺少会话 ID");
  });

  it("uses concise conditional guidance without file or backup counts", () => {
    const success = zhToolOperations.dialogs.removeResultSuccessMessage;
    const attention = zhToolOperations.dialogs.removeResultAttentionMessage;
    const failure = zhToolOperations.dialogs.removeResultFailureMessage;
    expect(success).toContain("如果重新打开 {{tool}} 后仍在使用 CONST API");
    for (const message of [success, attention, failure]) {
      expect(message).toContain(
        "在 {{tool}} 中退出当前账号或断开当前连接，再重新登录 {{tool}}",
      );
      expect(message).not.toContain("退出当前账号或连接后重新登录");
    }
    expect(success).not.toMatch(/文件|备份/);
    expect(attention).toContain("{{reason}}");
    expect(attention).not.toMatch(/文件|备份/);
    expect(failure).toContain("重启电脑");
    expect(failure).not.toMatch(/文件|备份/);
    expect(controllerSource).not.toMatch(
      /async function removeToolConfig[\s\S]*?files: result\.files\.length[\s\S]*?finishToolOperation/,
    );
  });

  it("uses a server-issued one-time confirmation token instead of a trusted boolean", () => {
    expect(rendererSource).not.toMatch(/confirmed:\s*(?:true|false|confirmed)/);
    expect(rendererSource).toMatch(/confirmationToken:\s*confirmationToken/);
    expect(rendererSource).toMatch(/response\.confirmation_token/);
    expect(tauriMainSource).not.toMatch(/\bconfirmed:\s*bool/);
    expect(tauriMainSource).toMatch(/confirmation_token:\s*Option<String>/);
  });

  it("uses progress long-polling without a renderer-wide deadline or timeout cancellation", () => {
    expect(rendererSource).toContain('"wait_tool_config_operation_update"');
    expect(rendererSource).not.toContain('"cancel_tool_config_operation"');
    expect(rendererSource).not.toContain("TOOL_CONFIG_TERMINAL_WAIT_TIMEOUT");
    expect(tauriMainSource).toContain("wait_tool_config_operation_update,");
    expect(operationCoreSource).not.toContain("deadline_ms");
  });

  it("maps every stage into one monotonic whole-operation progress", () => {
    expect(toolOperationExports.toolConfigProgressLabel({
      stage: "scanning_sessions",
      completed: 38,
      total: 195,
    })).toBe("扫描会话 38/195");
    expect(toolOperationExports.toolConfigProgressRatio({
      stage: "migrating_sessions",
      completed: 12,
      total: 48,
    })).toBeCloseTo(0.625);
    expect(toolOperationExports.toolConfigProgressRatio({
      stage: "migrating_sessions",
      completed: 12,
      total: 48,
      stageRatio: 0.5,
    })).toBeCloseTo(0.7);
    expect(toolOperationExports.toolConfigProgressLabel({
      stage: "migrating_sessions",
      completed: 12,
      total: 48,
      stageRatio: 0.5,
    })).toBe("迁移会话 12/48");
    expect(toolOperationExports.toolConfigVisualProgressRatio({
      stage: "migrating_sessions",
      completed: 12,
      total: 48,
      stageRatio: 0.5,
      ratio: 0.7,
    })).toBe(0.7);
    expect(toolOperationExports.toolConfigProgressLabel({
      stage: "checking_config",
    })).toBe("检查配置变化");
    expect(toolOperationExports.toolConfigProgressRatio({
      stage: "checking_config",
    })).toBe(0.11);
    expect(toolOperationExports.toolConfigProgressRatio({
      stage: "writing_config",
    })).toBe(0.32);
    expect(toolOperationExports.toolConfigProgressLabel({
      stage: "writing_config", completed: 3, total: 12,
    })).toBe("写入配置 3/12");
    expect(toolOperationExports.toolConfigProgressRatio({
      stage: "writing_config", completed: 3, total: 12,
    })).toBeCloseTo(0.46);
    expect(toolOperationExports.advanceToolConfigProgress(
      { stage: "awaiting_confirmation", ratio: 0.18 },
      { stage: "preparing" },
    ).ratio).toBe(0.18);

    const scanCompleted = toolOperationExports.advanceToolConfigProgress(undefined, {
      stage: "scanning_sessions",
      completed: 48,
      total: 48,
    });
    const migrationStarted = toolOperationExports.advanceToolConfigProgress(scanCompleted, {
      stage: "migrating_sessions",
      completed: 0,
      total: 12,
      stageRatio: 0,
    });
    const migrationAdvanced = toolOperationExports.advanceToolConfigProgress(migrationStarted, {
      stage: "migrating_sessions",
      completed: 1,
      total: 12,
      stageRatio: 0.5,
    });
    const staleMigrationUpdate = toolOperationExports.advanceToolConfigProgress(migrationAdvanced, {
      stage: "migrating_sessions",
      completed: 1,
      total: 12,
      stageRatio: 0.4,
    });
    expect(toolOperationExports.toolConfigVisualProgressRatio(scanCompleted)).toBeCloseTo(0.5);
    expect(toolOperationExports.toolConfigVisualProgressRatio(migrationStarted)).toBeCloseTo(0.55);
    expect(toolOperationExports.toolConfigVisualProgressRatio(migrationAdvanced)).toBeCloseTo(0.7);
    expect(toolOperationExports.toolConfigVisualProgressRatio(staleMigrationUpdate)).toBeCloseTo(0.7);
  });

  it("delivers only changed progress versions and stops at the terminal update", async () => {
    const statuses: ProgressStatus[] = [
      {
        operation_id: "progress-operation",
        state: "committing",
        terminal: false,
        cancellation_requested: false,
        progress_version: 1,
        progress_stage: "scanning_sessions",
        progress_completed: 1,
        progress_total: 3,
      },
      {
        operation_id: "progress-operation",
        state: "committing",
        terminal: false,
        cancellation_requested: false,
        progress_version: 2,
        progress_stage: "migrating_sessions",
        progress_completed: 2,
        progress_total: 3,
      },
      {
        operation_id: "progress-operation",
        state: "completed",
        terminal: true,
        cancellation_requested: false,
        progress_version: 3,
        progress_stage: "completed",
      },
    ];
    const waitForUpdate = vi.fn().mockImplementation(async () => statuses.shift());
    const received: ProgressStatus[] = [];
    await new Promise<void>((resolve, reject) => {
      toolOperationExports.watchToolConfigOperation(
        "progress-operation",
        (status) => {
          received.push(status);
          if (status.terminal) resolve();
        },
        reject,
        { waitForUpdate },
      );
    });

    expect(received.map((status) => status.progress_version)).toEqual([1, 2, 3]);
    expect(waitForUpdate.mock.calls.map((call) => call[1])).toEqual([0, 1, 2]);
  });

  it("does not expose the unused standalone Codex migration command", () => {
    expect(rendererSource).not.toContain('"migrate_codex_legacy_session_history"');
    expect(tauriMainSource).not.toContain("migrate_codex_legacy_session_history");
  });
});
