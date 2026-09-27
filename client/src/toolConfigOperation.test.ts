import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import mainSource from "./main.tsx?raw";
import controllerSource from "./appController.tsx?raw";
import appControlsSource from "./components/AppControls.tsx?raw";
import zhLocaleSource from "./i18n/locales/zh-CN.json?raw";
import tauriMainSource from "../src-tauri/src/main.rs?raw";
import operationCoreSource from "../src-tauri/src/tool_config/production/core.rs?raw";

const rendererSource = `${mainSource}\n${controllerSource}`;
const dialogStylesSource = readFileSync(
  resolve(process.cwd(), "src/styles/dialogs-and-supplier.css"),
  "utf8",
);
const zhLocale = JSON.parse(zhLocaleSource) as {
  labels: Record<string, string>;
  toolOperations: {
    dialogs: Record<string, string>;
    removeModes: Record<string, string>;
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
    expect(tauriMainSource).toMatch(
      /async fn execute_tool_config_operation[\s\S]*?restart: bool/,
    );
    expect(
      tauriMainSource.match(/execute_atomic_tool_config_operation\(/g),
    ).toHaveLength(2);
    expect(
      tauriMainSource.match(/execute_previewed_tool_config_operation\(/g),
    ).toHaveLength(8);
    expect(
      tauriMainSource.match(/execute_atomic_tool_config_operation_with_preview\(/g),
    ).toHaveLength(1);
    expect(tauriMainSource).toContain('tool == "workbuddy"');
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
    expect(mainSource).toMatch(
      /primaryAction === "configure_and_launch"[\s\S]*?startToolProgram\(card\.tool, card\.title, true\)/,
    );
    expect(tauriMainSource).toMatch(
      /use_configured_environment[\s\S]*?matches!\([\s\S]*?tool\.as_str\(\)[\s\S]*?"claude" \| "claude-science" \| "gemini" \| "copilot" \| "goose"/,
    );
  });

  it("routes the primary launch action through location, configuration, and direct launch", () => {
    expect(mainSource).toContain("toolPrimaryAction(configStatus, configurationChanged)");
    expect(mainSource).toMatch(
      /primaryAction === "locate"[\s\S]*?openToolLocator\(card\.tool, card\.title, true\)/,
    );
    expect(mainSource).toMatch(
      /primaryAction === "configure_and_launch"[\s\S]*?launchToolConfig\(card\.tool, card\.title\)/,
    );
    expect(mainSource).toMatch(
      /onConfigure=\{\(\) => \{[\s\S]*?prepareToolUse\(\)[\s\S]*?applyToolConfig\(card\.tool, card\.title\)/,
    );
    expect(controllerSource).toContain("Promise.all(toolCards.map(async (card) =>");
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

  it("adds the restore path to successful configuration notices from both entry points", () => {
    expect(toolOperationExports.toolConfigSuccessNotice("Codex 配置完成。")).toBe(
      "Codex 配置完成。 如需恢复原来的接入设置，右键该工具选择“取消配置”即可。",
    );
    expect(mainSource).toContain("launchToolConfig(card.tool, card.title)");
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
      message: expect.stringContaining("以后直接打开 VS Code，也可使用 CONST API。"),
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

  it("uses one removal flow with an explicit mode and keeps restore as the default", () => {
    expect(controllerSource).toMatch(
      /askToolRemove\([\s\S]*?removeDecision === "cancel"[\s\S]*?executeToolConfigOperation\([\s\S]*?removeDecision/,
    );
    expect(appControlsSource).toContain('useState<ToolRemoveMode>("restore_pre_const")');
    const nativeRemoveTools = controllerSource.match(
      /const NATIVE_ROUTE_REMOVE_TOOLS = new Set\(\[[\s\S]*?\]\);/,
    )?.[0] ?? "";
    expect(nativeRemoveTools).toMatch(
      /"codex"[\s\S]*?"claude"[\s\S]*?"claude-desktop"[\s\S]*?"gemini"/,
    );
    expect(nativeRemoveTools).not.toContain('"vscode"');
    expect(controllerSource).toMatch(
      /removeMode:\s*backendAction === "remove" \? removeMode : undefined/,
    );
    expect(operationCoreSource).toMatch(
      /parse\(value: Option<&str>\)[\s\S]*?unwrap_or\("restore_pre_const"\)/,
    );
    expect(tauriMainSource).toMatch(
      /remove_mode\.as_str\(\)[\s\S]*?remove_tool_config_by_name_with_mode/,
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
      /executeToolConfigOperation\([\s\S]*?"remove",[\s\S]*?removeDecision,\s*true,/,
    );
    expect(zhToolOperations.removeModes.restartWarning).toContain("自动关闭相关进程");
  });

  it("keeps the radio native-sized without the generic text-input rectangle", () => {
    const radioRule = dialogStylesSource.match(
      /\.tool-remove-option input\[type="radio"\]\s*\{[\s\S]*?\}/,
    )?.[0] ?? "";
    expect(radioRule).toContain("width: 16px");
    expect(radioRule).toContain("min-height: 16px");
    expect(radioRule).toContain("padding: 0");
    expect(radioRule).toContain("box-shadow: none");
    expect(dialogStylesSource).toContain(
      '.tool-remove-option:has(input[type="radio"]:focus-visible)',
    );
    expect(dialogStylesSource).toMatch(
      /\.tool-remove-option\s*\{[\s\S]*?background:\s*var\(--panel\)/,
    );
    const defaultBadgeRule = dialogStylesSource.match(
      /\.tool-remove-option-badge\s*\{[\s\S]*?\}/,
    )?.[0] ?? "";
    expect(defaultBadgeRule).not.toMatch(/background|border-radius|padding/);
    expect(appControlsSource).toContain('className="tool-remove-option-badge"');
    expect(appControlsSource).not.toContain(
      't("toolOperations.removeModes.chooseMessage")',
    );
    expect(appControlsSource).not.toContain(
      't("toolOperations.removeModes.restoreOnlyMessage")',
    );
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
