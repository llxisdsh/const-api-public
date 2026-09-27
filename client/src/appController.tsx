import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  Bot,
  BrainCircuit,
  Cable,
  CircleDollarSign,
  Cloud,
  CloudUpload,
  Code2,
  Cpu,
  Flame,
  Footprints,
  KeyRound,
  Laptop,
  MessageSquareText,
  Moon,
  Network,
  Plug,
  Server,
  Sparkles,
  Sun,
  Terminal,
  Waves,
  Zap,
  type LucideIcon
} from "lucide-react";
import React, { useEffect, useMemo, useRef, useState } from "react";
import {
  siAnthropic,
  siBaidu,
  siClaudecode,
  siDeepseek,
  siHuggingface,
  siLmstudio,
  siMinimax,
  siMistralai,
  siMoonshotai,
  siNvidia,
  siOllama,
  siOpenrouter,
  siPerplexity,
  siQwen,
  siVllm,
} from "simple-icons";
import packageInfo from "../package.json";
import { tauriAccountApi } from "./account/accountApi";
import { emptyAccountStatus, type AccountAuthMode } from "./account/accountTypes";
import { useAccountCenter } from "./account/useAccountCenter";
import {
  accessTestRequestPath,
  applyChannelDetection,
  applySupplierUpstreamObservation,
  buildRuntimeDiagnostics,
  channelCanDetect,
  channelCanSupply,
  channelFieldProfile,
  channelFromSupplier,
  channelIsPlatformPooled,
  channelIsPooled,
  channelKindLabel,
  channelLocallyReady,
  channelPrimaryProtocolOptions,
  channelV5FromRenderer,
  compactSourceLabel,
  debugRecommendedTargetProtocol,
  debugTargetOptions,
  defaultSurfaceOperationUrl,
  emptyConfig,
  formatRelativeTime,
  hydrateClientConfigV5,
  isEstablishedSupplierTransport,
  localListenAddressForLanAccess,
  localLoopbackApiRoot,
  modelCompatibilityGroupsFromPayload,
  normalizedPriceRatio,
  normalizedRouteState,
  normalizeModelList,
  normalizeRendererChannel,
  normalizeSurfaceBindings,
  platformModelOptions,
  projectRendererChannel,
  protocolLabel,
  protocolDebugOutcome,
  protocolVerification,
  rendererChannelFromV5,
  routePlanInvokeErrorText,
  routePlanRequestBody,
  sanitizeApiFormat,
  shouldShowSupplierRouteStatusToast,
  supplierChannelRuntimeStarted,
  supplierChannelTransportForChannel,
  supplierFromChannel,
  supplierQuicFromEndpoint,
  supplierRouteStatusBlocksReady,
  supplierRouteStatusDetail,
  supplierRouteStatusForChannel,
  supplierRouteStatusInlineLabel,
  supplierRouteStatusSuppressesToast,
  supplierRouteStatusToastIdentity,
  supplierRouteStatusToastSnapshot,
  supplierRouteStatusToastText,
  supplierRouteStatusToastTone,
  supplierWsFromEndpoint,
  surfaceAuthScheme,
  surfaceEditorRows,
  surfaceEndpointProfile,
  surfaceForProtocol,
  surfaceOperationOptions,
  surfaceProtocols,
  utf8ByteLength
} from "./appHelpers";
import type {
  ApiSurface,
  AppLogEntry,
  AppMetaState,
  BrandIconData,
  ChannelConfig,
  ChannelDetectionResult,
  ChannelDuplicateCandidate,
  ChannelDuplicateDecision,
  ChannelDuplicateDialogState,
  ChannelSurfaceBinding,
  ChannelUpstreamTestResult,
  ClientConfig,
  ClientConfigV5Wire,
  CodexSessionScanResult,
  ConfirmDialogState,
  CopyField,
  CustomBrandIcon,
  DebugProtocol,
  DebugTargetOption,
  DebugTargetType,
  EndpointDiscoveryResult,
  ModelCatalogVersionInfo,
  ModelCompatibilityCatalogModel,
  ModelCompatibilityGroup,
  ModelCompatibilityPolicyState,
  NativeUpdateStatus,
  OperationEndpointOverride,
  PlatformProvider,
  PlatformSupplierNode,
  ProtocolDebugResult,
  ProxyStatus,
  ReleaseSourceStatus,
  RouteDecision,
  RoutePlanState,
  SubscriptionOAuthSession,
  SubscriptionProvider,
  SupplierChannelHealth,
  SupplierChannelValidationResult,
  SupplierStatus,
  TestResult,
  Toast,
  ToolApplyResult,
  ToolConfigOperationResponse,
  ToolConfigOperationStatus,
  ToolConfigOperationTicket,
  ToolConfigProgressControl,
  ToolOperationProgress,
  ToolProgramLocationResult,
  ToolRemoveDecision,
  ToolRemoveDialogState,
  ToolRemoveMode,
  UpdateState
} from "./appTypes";
import {
  callLogRefreshErrorText,
  callLogStatsForRefresh,
  callLogCursorForRefresh,
  callSyncState,
  callSyncIsIncremental,
  callRecordID,
  callRecordsCursorMs,
  mergeCallRecords,
  snapshotCursorMs,
  type ActiveCallLog,
  type CallLogSource,
  type CallRecord,
  type CallSyncState,
  type UsageStat
} from "./callLogs";
import {
  channelsVisibleInModelMarket,
  configuredModelMarketChannelCount,
  moveChannelInVisibleOrder,
} from "./channelOrdering";
import {
  DEFAULT_CLAUDE_MODEL_SETTINGS,
  readClaudeModelSettings,
  writeClaudeModelSettings,
  type ClaudeModelSettings,
} from "./claudeModelSettings";
import {
  SupplierChoiceCard
} from "./components/AppControls";
import { overlayAccessConfig, overlaySupplierConfig } from "./configScopes";
import { useDiagnostics, type DiagnosticIssue } from "./diagnostics";
import {
  dismissGuidanceNotice,
  upsertGuidanceNotice,
  type GuidanceNotice,
} from "./guidanceNotices";
import { localizedError, tr } from "./i18n";
import type { LanShareConnectionInfo } from "./lanShareConnectionInfo";
import {
  DEFAULT_LOCAL_API_CONNECTION_ID,
  localApiConnectionById,
  localApiSurfaceById,
  type LocalApiConnectionId,
} from "./localApiSurfaces";
import { DEVELOPMENT_PROFILE } from "./runtimeProfile";
import {
  UI_SNAPSHOT_AUTO_REFRESH_MS,
  createRefreshGate,
  type RefreshGate,
  type RefreshOptions,
} from "./refreshPolicy";
import { sourceDriverTitle } from "./sourceDriverPresentation";
import {
  normalizeChannelV5,
  sourceDriverById,
  sourceDriverInitialChannelV5,
  sourceDriversForApiAccess,
  sourceDriversForCategory,
  sourceDriversForCustom,
  sourceDriversForLanSharing,
  type ChannelConfigV5,
  type SourceDriver
} from "./sourceDrivers";
import { keepPreviousIfStructurallyEqual } from "./stateSnapshots";
import { toolConfigCanLaunchExistingAfterFailure } from "./toolConfigRecovery";
import {
  cancelSupplierEditor,
  channelDetectionInputChanged,
  channelEditorSettingsChanged,
  channelRuntimeNeedsActivation,
  diagnosticToastEventKey,
  finishSupplierDeletion,
  mergeEndpointDiscoveryConfig,
  reconcileDetectedDefaultModel,
  resolveDebugModelSelection,
  supplierDeleteConfirmation,
  supplierDeletedNotice,
  supplierLifecyclePresentation,
} from "./supplierEditorState";
import {
  parseSupplierQuotaSnapshots,
  rememberSupplierQuotaSnapshot,
  resolveSupplierQuotaHealth,
  type SupplierQuotaSnapshotMap,
} from "./supplierQuotaSnapshot";
import {
  applyDocumentTheme,
  readThemePreference,
  systemPrefersDark,
  writeThemePreference,
  type ThemePreference
} from "./theme";
import { TOOL_CATALOG, TOOL_CATALOG_ORDER, type ToolCatalogId } from "./toolCatalog";
import {
  DEFAULT_TOOL_PROTOCOLS,
  TOOL_PROTOCOLS_BY_TOOL,
  codexModelSourceFromStatus,
  toolProtocolId,
  type CodexModelSource,
  type ToolProtocolId
} from "./toolMenuPresentation";
import {
  defaultToolOrder,
  moveToolToEnd,
  readHiddenToolIds,
  readToolOrder,
  writeHiddenToolIds,
  writeToolOrder,
} from "./toolVisibility";
import {
  formatUpdateError,
  updateInstallWaitingText,
} from "./updateSupport";

export const INITIAL_SYSTEM_PREFERS_DARK = systemPrefersDark();

const STORED_THEME_PREFERENCE = readThemePreference();

export const INITIAL_THEME_PREFERENCE: ThemePreference = STORED_THEME_PREFERENCE === "system"
  ? (INITIAL_SYSTEM_PREFERS_DARK ? "dark" : "light")
  : STORED_THEME_PREFERENCE;

export const UI_SCRIPT_STARTED_AT = performance.now();

export let firstAppRenderLogged = false;

export const CURRENT_VERSION = packageInfo.version;

export const SIMPLE_ENGLISH_TEST_PROMPTS = [
  "Reply with one short English sentence about coding.",
  "Say hello in one short English sentence.",
  "Write one simple English sentence about a helpful assistant.",
  "Describe a useful tool in one short English sentence.",
];

const LEGACY_AUTO_INSTALL_UPDATES_KEY = "const-api:auto-install-updates";
const DEBUG_EXPERIMENT_CLAUDE_SERVER_SIDE_COMPACTION = "claude_server_side_compaction";

export const SUPPLIER_QUOTA_SNAPSHOTS_KEY = "const-api:supplier-quota-snapshots";

export const TOOL_STATUS_TIMEOUT_MS = 10_000;

const NATIVE_ROUTE_REMOVE_TOOLS = new Set([
  "codex",
  "claude",
  "claude-desktop",
  "gemini",
]);

function matchesNativeRemoveTool(tool: string) {
  return NATIVE_ROUTE_REMOVE_TOOLS.has(tool);
}

export const DEFAULT_TOAST_DURATION_MS = 4_200;

export const ERROR_TOAST_MIN_DURATION_MS = 9_000;

export const SUPPLIER_PLATFORM_ACK_TIMEOUT_MS = 10_000;

export const SUPPLIER_PLATFORM_ACK_POLL_MS = 400;

export const SUPPLIER_OPERATION_TIMEOUT_MS = 75_000;

export const SUPPLIER_QUICK_OPERATION_TIMEOUT_MS = 20_000;

export const SUBSCRIPTION_IMPORT_TIMEOUT_MS = 30_000;

export const THEME_OPTIONS: Array<{ id: ThemePreference; icon: LucideIcon }> = [
  { id: "light", icon: Sun },
  { id: "dark", icon: Moon },
];

export type SupplierChannelOperationKind =
  | "saving"
  | "refreshing"
  | "testing"
  | "enabling"
  | "disabling"
  | "deleting";

export type SupplierChannelOperation = {
  channelId: string;
  kind: SupplierChannelOperationKind;
  token: number;
  checksUpstream?: boolean;
};

export type SubscriptionWizardProgress = {
  tone: "info" | "error";
  message: string;
};

export function supplierChannelOperationLabel(kind: SupplierChannelOperationKind) {
  switch (kind) {
    case "saving": return tr("labels.toolMenu.operation.save");
    case "refreshing": return tr("labels.toolMenu.operation.refreshChannel");
    case "testing": return tr("labels.toolMenu.operation.test");
    case "enabling": return tr("labels.toolMenu.operation.enable");
    case "disabling": return tr("labels.toolMenu.operation.disable");
    case "deleting": return tr("labels.toolMenu.operation.delete");
  }
}

function emptyModelCompatibilityPolicy(status: ModelCompatibilityPolicyState["status"] = "idle"): ModelCompatibilityPolicyState {
  return {
    status,
    aliases: {},
    modelGroups: [],
    templateModelGroups: [],
    catalogModels: [],
    platformAvailableModels: [],
    localAvailableModels: [],
  };
}

function modelCompatibilityCatalogFromPayload(value: unknown): ModelCompatibilityCatalogModel[] {
  if (!Array.isArray(value)) return [];
  const seen = new Set<string>();
  return value.flatMap((item) => {
    if (!item || typeof item !== "object" || Array.isArray(item)) return [];
    const payload = item as Record<string, unknown>;
    const id = typeof payload.id === "string" ? payload.id.trim() : "";
    const key = id.toLowerCase();
    if (!id || seen.has(key)) return [];
    seen.add(key);
    return [{
      id,
      display_name: typeof payload.display_name === "string" ? payload.display_name.trim() : "",
      vendor: typeof payload.vendor === "string" ? payload.vendor.trim() : "",
      family: typeof payload.family === "string" ? payload.family.trim() : "",
      context_tokens: typeof payload.context_tokens === "number"
        && Number.isSafeInteger(payload.context_tokens)
        && payload.context_tokens >= 0
        ? payload.context_tokens
        : 0,
      output_tokens: typeof payload.output_tokens === "number"
        && Number.isSafeInteger(payload.output_tokens)
        && payload.output_tokens >= 0
        ? payload.output_tokens
        : 0,
      input_modalities: stringListFromPayload(payload.input_modalities),
      output_modalities: stringListFromPayload(payload.output_modalities),
      reasoning: payload.reasoning === true,
      tool_call: payload.tool_call === true,
    }];
  });
}

function stringListFromPayload(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  const seen = new Set<string>();
  return value.flatMap((item) => {
    if (typeof item !== "string") return [];
    const normalized = item.trim();
    const key = normalized.toLowerCase();
    if (!normalized || seen.has(key)) return [];
    seen.add(key);
    return [normalized];
  });
}

function modelCompatibilityStateFromPayload(payload: unknown): ModelCompatibilityPolicyState {
  if (!payload || typeof payload !== "object" || Array.isArray(payload)) {
    throw new Error(tr("modelCompatibility.invalidPolicy"));
  }
  const value = payload as Record<string, unknown>;
  if (!value.aliases || typeof value.aliases !== "object" || Array.isArray(value.aliases)) {
    throw new Error(tr("modelCompatibility.missingAliases"));
  }
  const aliases = Object.fromEntries(
    Object.entries(value.aliases).filter(([, candidate]) => typeof candidate === "string"),
  ) as Record<string, string>;
  const modelGroups = modelCompatibilityGroupsFromPayload(value.model_groups);
  const templateModelGroups = modelCompatibilityGroupsFromPayload(value.template_model_groups);
  const syncStatus = value.sync_status === "pending" || value.sync_status === "conflict" || value.sync_status === "local"
    ? value.sync_status
    : "synced";
  return {
    status: "ready",
    aliases,
    modelGroups,
    templateModelGroups: templateModelGroups.length > 0 ? templateModelGroups : modelGroups,
    catalogModels: modelCompatibilityCatalogFromPayload(value.catalog_models),
    platformAvailableModels: stringListFromPayload(value.platform_available_models),
    localAvailableModels: stringListFromPayload(value.local_available_models),
    source: typeof value.source === "string" ? value.source : "server",
    customized: value.customized === true,
    platformSyncAvailable: value.platform_sync_available !== false,
    baseReleaseId: typeof value.release_id === "string"
      ? value.release_id
      : typeof value.base_release_id === "string" ? value.base_release_id : "",
    revision: typeof value.revision === "number" && Number.isFinite(value.revision) ? value.revision : 0,
    templateChanged: value.template_changed === true,
    syncStatus,
    syncError: typeof value.sync_error === "string" ? value.sync_error : undefined,
  };
}

export function randomSimpleEnglishPrompt() {
  return SIMPLE_ENGLISH_TEST_PROMPTS[Math.floor(Math.random() * SIMPLE_ENGLISH_TEST_PROMPTS.length)];
}

export function hasTauriRuntime() {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

function readLegacyAutoInstallUpdatesPreference() {
  if (typeof window === "undefined") return undefined;
  try {
    const value = window.localStorage.getItem(LEGACY_AUTO_INSTALL_UPDATES_KEY);
    return value === null ? undefined : value !== "false";
  } catch {
    return undefined;
  }
}

function clearLegacyAutoInstallUpdatesPreference() {
  try {
    window.localStorage.removeItem(LEGACY_AUTO_INSTALL_UPDATES_KEY);
  } catch {
    // A blocked legacy store does not affect the native preference.
  }
}

export function readSupplierQuotaSnapshots() {
  if (typeof window === "undefined") return {};
  try {
    return parseSupplierQuotaSnapshots(window.localStorage.getItem(SUPPLIER_QUOTA_SNAPSHOTS_KEY));
  } catch {
    return {};
  }
}

export function writeSupplierQuotaSnapshots(snapshots: SupplierQuotaSnapshotMap) {
  try {
    window.localStorage.setItem(SUPPLIER_QUOTA_SNAPSHOTS_KEY, JSON.stringify(snapshots));
  } catch {
    // Quota display remains available for this process when storage is unavailable.
  }
}

export function withUiTimeout<T>(promise: Promise<T>, timeoutMs: number, message: string) {
  let timer = 0;
  const timeout = new Promise<never>((_, reject) => {
    timer = window.setTimeout(() => reject(new Error(message)), timeoutMs);
  });
  return Promise.race([promise, timeout]).finally(() => window.clearTimeout(timer));
}

export const TOOL_CONFIG_PROGRESS_WAIT_MS = 15_000;

export const DEFAULT_TOOL_CONFIG_PROGRESS_CONTROL: ToolConfigProgressControl = {
  waitForUpdate: (operationId, afterVersion, waitMs) =>
    invoke<ToolConfigOperationStatus>("wait_tool_config_operation_update", {
      operationId,
      afterVersion,
      waitMs,
    }),
};

export function toolConfigProgressLabel(progress: ToolOperationProgress) {
  const count = Number.isSafeInteger(progress.completed)
    && Number.isSafeInteger(progress.total)
    && Number(progress.total) > 0
    ? `${Math.min(Number(progress.completed), Number(progress.total))}/${Number(progress.total)}`
    : "";
  switch (progress.stage) {
    case "preparing": return tr("labels.toolMenu.progress.preparing");
    case "loading_models": return tr("labels.toolMenu.progress.loadingModels");
    case "checking_config": return tr("labels.toolMenu.progress.checkingConfig");
    case "checking_process": return tr("labels.toolMenu.progress.checkingProcess");
    case "awaiting_confirmation": return tr("labels.toolMenu.progress.awaitingConfirmation");
    case "awaiting_manual_close": return tr("labels.toolMenu.progress.awaitingManualClose");
    case "closing_process": return tr("labels.toolMenu.progress.closingProcess");
    case "writing_config": return tr("labels.toolMenu.progress.writingConfig");
    case "scanning_sessions":
      return tr("labels.toolMenu.progress.scanningSessions", { displayCount: count ? ` ${count}` : "" });
    case "planning_session_index": return tr("labels.toolMenu.progress.planningIndex");
    case "migrating_sessions":
      return tr("labels.toolMenu.progress.migratingSessions", { displayCount: count ? ` ${count}` : "" });
    case "syncing_sessions":
      return tr("labels.toolMenu.progress.syncingSessions", { displayCount: count ? ` ${count}` : "" });
    case "updating_session_index": return tr("labels.toolMenu.progress.updatingIndex");
    case "launching": return tr("labels.toolMenu.progress.launching");
    case "completed": return tr("labels.toolMenu.progress.completed");
    case "committed_not_started": return tr("labels.toolMenu.progress.committed");
    case "failed": return tr("labels.toolMenu.progress.failed");
    case "cancelled": return tr("labels.toolMenu.progress.cancelled");
    default: return tr("labels.toolMenu.progress.working");
  }
}

export function toolConfigProgressRatio(progress?: ToolOperationProgress | null) {
  if (!progress) return null;
  if (Number.isFinite(progress.ratio)) {
    return Math.max(0, Math.min(1, Number(progress.ratio)));
  }
  const stageFraction = Number.isFinite(progress.stageRatio)
    ? Math.max(0, Math.min(1, Number(progress.stageRatio)))
    : Number.isFinite(progress.completed)
      && Number.isFinite(progress.total)
      && Number(progress.total) > 0
      ? Math.max(0, Math.min(1, Number(progress.completed) / Number(progress.total)))
      : 0;
  switch (progress.stage) {
    case "preparing": return 0.03;
    case "loading_models": return 0.08;
    case "checking_config": return 0.11;
    case "checking_process": return 0.16;
    case "awaiting_confirmation": return 0.18;
    case "awaiting_manual_close": return 0.2;
    case "closing_process": return 0.26;
    case "writing_config": return 0.32;
    case "scanning_sessions": return 0.32 + stageFraction * 0.18;
    case "planning_session_index": return 0.52;
    case "migrating_sessions": return 0.55 + stageFraction * 0.3;
    case "syncing_sessions": return 0.55 + stageFraction * 0.3;
    case "updating_session_index": return 0.88;
    case "launching": return 0.95;
    case "completed": return 1;
    case "committed_not_started": return 1;
    case "failed": return 1;
    case "cancelled": return 1;
    default: return 0.03;
  }
}

export function toolConfigVisualProgressRatio(progress?: ToolOperationProgress | null) {
  return toolConfigProgressRatio(progress);
}

export function advanceToolConfigProgress(
  previous: ToolOperationProgress | undefined,
  next: ToolOperationProgress,
) {
  const previousRatio = toolConfigProgressRatio(previous) ?? 0;
  const nextRatio = toolConfigProgressRatio(next) ?? previousRatio;
  return { ...next, ratio: Math.max(previousRatio, nextRatio) };
}

export function watchToolConfigOperation(
  operationId: string,
  onUpdate: (status: ToolConfigOperationStatus) => void,
  onError: (error: unknown) => void = () => undefined,
  control: ToolConfigProgressControl = DEFAULT_TOOL_CONFIG_PROGRESS_CONTROL,
) {
  let stopped = false;
  let progressVersion = 0;
  void (async () => {
    while (!stopped) {
      try {
        const status = await control.waitForUpdate(
          operationId,
          progressVersion,
          TOOL_CONFIG_PROGRESS_WAIT_MS,
        );
        if (stopped) return;
        if (status.progress_version !== progressVersion) {
          progressVersion = status.progress_version;
          onUpdate(status);
        }
        if (status.terminal) return;
      } catch (error) {
        if (!stopped) onError(error);
        return;
      }
    }
  })();
  return () => {
    stopped = true;
  };
}

export function startupConsoleInfo(message: string, ...args: unknown[]) {
  if (import.meta.env.DEV) {
    console.info(message, ...args);
  }
}

export function startupLogFromUi(message: string) {
  if (!import.meta.env.DEV || !hasTauriRuntime()) return;
  void invoke("startup_log_from_ui", {
    message,
    elapsedMs: Math.round(performance.now() - UI_SCRIPT_STARTED_AT),
  }).catch(() => {
    // Startup diagnostics must never affect rendering.
  });
}

export function toolConfigOperationResultNotice(
  title: string,
  restartAfterConfig: boolean,
  result: Pick<ToolApplyResult, "details">,
): { text: string; tone: "info" | "success" | "error"; duration: number; restoreHint?: boolean } | null {
  if (restartAfterConfig && result.details?.configuration_unchanged === "true") {
    return {
      text: tr("toolOperations.launched", { tool: title }),
      tone: "success",
      duration: 4200,
      restoreHint: false,
    };
  }
  if (restartAfterConfig && result.details?.operation_status === "committed_not_started") {
    return {
      text: tr("toolOperations.committedNotStarted", { tool: title }),
      tone: "info",
      duration: 6200,
    };
  }
  return null;
}

export function toolConfigSuccessNotice(message: string) {
  return `${message} ${tr("toolOperations.restoreHint")}`;
}

export function toolFirstConfigurationNotice({
  title,
  restartAfterConfig,
  wasConfiguredBefore,
  result,
  refreshedStatus,
  statusRefreshVerified,
}: {
  title: string;
  restartAfterConfig: boolean;
  wasConfiguredBefore: boolean;
  result: Pick<ToolApplyResult, "details">;
  refreshedStatus: Pick<ToolApplyResult, "details">;
  statusRefreshVerified: boolean;
}): ConfirmDialogState | null {
  const restartCommand = result.details?.restart_command?.trim() ?? "";
  const launcherPath = result.details?.restart_launcher_path?.trim() ?? "";
  const launchConfirmed = Boolean(
    restartCommand
      && launcherPath
      && !launcherPath.startsWith("manual start required"),
  );
  if (
    !restartAfterConfig
    || wasConfiguredBefore
    || result.details?.configuration_changed !== "true"
    || !launchConfirmed
  ) {
    return null;
  }

  const externalLaunchWarning = statusRefreshVerified
    ? refreshedStatus.details?.external_launch_warning?.trim()
    : "";
  const configurationScope = refreshedStatus.details?.configuration_scope
    ?? result.details?.configuration_scope;
  const launchOnly = configurationScope === "const_api_launch_environment"
    || configurationScope === "provider_and_const_api_launch_environment";
  const messageKey = !statusRefreshVerified
    ? "toolOperations.dialogs.firstConfigurationUnverifiedMessage"
    : launchOnly
      ? "toolOperations.dialogs.firstConfigurationLaunchOnlyMessage"
      : externalLaunchWarning
        ? "toolOperations.dialogs.firstConfigurationExternalWarningMessage"
        : "toolOperations.dialogs.firstConfigurationMessage";
  return {
    title: tr("toolOperations.dialogs.firstConfigurationTitle", { tool: title }),
    message: `${tr(messageKey, { tool: title })}\n\n${tr("toolOperations.dialogs.configurationRecoveryMessage", { tool: title })}`,
    confirmText: tr("toolOperations.actions.acknowledge"),
    mode: "notice",
  };
}

export const openAiBrandIcon: BrandIconData = {
  title: "OpenAI",
  hex: "000000",
  path: "M9.205 8.658v-2.26c0-.19.072-.333.238-.428l4.543-2.616c.619-.357 1.356-.523 2.117-.523 2.854 0 4.662 2.212 4.662 4.566 0 .167 0 .357-.024.547l-4.71-2.759a.797.797 0 00-.856 0l-5.97 3.473zm10.609 8.8V12.06c0-.333-.143-.57-.429-.737l-5.97-3.473 1.95-1.118a.433.433 0 01.476 0l4.543 2.617c1.309.76 2.189 2.378 2.189 3.948 0 1.808-1.07 3.473-2.76 4.163zM7.802 12.703l-1.95-1.142c-.167-.095-.239-.238-.239-.428V5.899c0-2.545 1.95-4.472 4.591-4.472 1 0 1.927.333 2.712.928L8.23 5.067c-.285.166-.428.404-.428.737v6.898zM12 15.128l-2.795-1.57v-3.33L12 8.658l2.795 1.57v3.33L12 15.128zm1.796 7.23c-1 0-1.927-.332-2.712-.927l4.686-2.712c.285-.166.428-.404.428-.737v-6.898l1.974 1.142c.167.095.238.238.238.428v5.233c0 2.545-1.974 4.472-4.614 4.472zm-5.637-5.303l-4.544-2.617c-1.308-.761-2.188-2.378-2.188-3.948A4.482 4.482 0 014.21 6.327v5.423c0 .333.143.571.428.738l5.947 3.449-1.95 1.118a.432.432 0 01-.476 0zm-.262 3.9c-2.688 0-4.662-2.021-4.662-4.519 0-.19.024-.38.047-.57l4.686 2.71c.286.167.571.167.856 0l5.97-3.448v2.26c0 .19-.07.333-.237.428l-4.543 2.616c-.619.357-1.356.523-2.117.523zm5.899 2.83a5.947 5.947 0 005.827-4.756C22.287 18.339 24 15.84 24 13.296c0-1.665-.713-3.282-1.998-4.448.119-.5.19-.999.19-1.498 0-3.401-2.759-5.947-5.946-5.947-.642 0-1.26.095-1.88.31A5.962 5.962 0 0010.205 0a5.947 5.947 0 00-5.827 4.757C1.713 5.447 0 7.945 0 10.49c0 1.666.713 3.283 1.998 4.448-.119.5-.19 1-.19 1.499 0 3.401 2.759 5.946 5.946 5.946.642 0 1.26-.095 1.88-.309a5.96 5.96 0 004.162 1.713z",
};

export function sourceDriverCardPresentation(driver: SourceDriver): Pick<React.ComponentProps<typeof SupplierChoiceCard>, "icon" | "customIcon" | "fallback" | "tone"> {
  switch (driver.iconKey) {
    case "openai":
      return { icon: openAiBrandIcon, fallback: driver.category === "subscription" ? CircleDollarSign : KeyRound, tone: "var(--text)" };
    case "anthropic":
      return { icon: siAnthropic, fallback: MessageSquareText, tone: "var(--text)" };
    case "gemini":
      return { icon: null, customIcon: "gemini", fallback: BrainCircuit, tone: "#1d4ed8" };
    case "azure":
      return { icon: null, customIcon: "azure", fallback: Server, tone: "#0078d4" };
    case "bedrock":
      return { icon: null, customIcon: "bedrock", fallback: CloudUpload, tone: "#6350fb" };
    case "ollama":
      return { icon: siOllama, fallback: Cpu, tone: "var(--text)" };
    case "lmstudio":
      return { icon: siLmstudio, fallback: Laptop, tone: "#0f766e" };
    case "vllm":
      return { icon: siVllm, fallback: Server, tone: "#7c3aed" };
    case "openrouter":
      return { icon: siOpenrouter, fallback: CloudUpload, tone: "#6566f1" };
    case "opencode":
      return { icon: null, customIcon: "opencode", fallback: Code2, tone: "var(--text)" };
    case "cline":
      return { icon: null, customIcon: "cline", fallback: Code2, tone: "var(--text)" };
    case "omniroute":
      return { icon: null, customIcon: "omniroute", fallback: Cable, tone: "#c93d4e" };
    case "claude":
      return { icon: null, customIcon: "claude", fallback: MessageSquareText, tone: "#b45309" };
    case "xai":
      return { icon: null, fallback: Zap, tone: "var(--text)" };
    case "mistral":
      return { icon: siMistralai, fallback: BrainCircuit, tone: "#f97316" };
    case "deepseek":
      return { icon: siDeepseek, fallback: BrainCircuit, tone: "#4d6bfe" };
    case "dashscope":
      return { icon: siQwen, fallback: Cloud, tone: "#615ced" };
    case "moonshot":
      return { icon: siMoonshotai, fallback: Moon, tone: "var(--text)" };
    case "zhipu":
      return { icon: null, fallback: BrainCircuit, tone: "#2563eb" };
    case "minimax":
      return { icon: siMinimax, fallback: BrainCircuit, tone: "var(--text)" };
    case "stepfun":
      return { icon: null, fallback: Footprints, tone: "#7c3aed" };
    case "groq":
      return { icon: null, fallback: Zap, tone: "#f55036" };
    case "together":
      return { icon: null, fallback: Cable, tone: "#6366f1" };
    case "fireworks":
      return { icon: null, fallback: Sparkles, tone: "#ef4444" };
    case "perplexity":
      return { icon: siPerplexity, fallback: BrainCircuit, tone: "#20b8a6" };
    case "huggingface":
      return { icon: siHuggingface, fallback: BrainCircuit, tone: "#f59e0b" };
    case "nvidia":
      return { icon: siNvidia, fallback: Cpu, tone: "#76b900" };
    case "siliconflow":
      return { icon: null, fallback: Waves, tone: "#00a67e" };
    case "volcengine":
      return { icon: null, fallback: Flame, tone: "#1664ff" };
    case "baidu":
      return { icon: siBaidu, fallback: Cloud, tone: "#2932e1" };
    case "tencent":
      return { icon: null, fallback: Cloud, tone: "#00a4ff" };
    case "custom":
      return { icon: null, fallback: Plug, tone: "var(--text)" };
    case "network":
      return { icon: null, fallback: Network, tone: "var(--text)" };
  }
}

export type AppTab = "access" | "supplier" | "about" | "account";

type AppControllerOptions = {
  initialTab?: AppTab;
  initialAccountAuthMode?: AccountAuthMode;
  showDevelopmentPresentation?: boolean;
};

export function useAppController({
  initialTab = "access",
  initialAccountAuthMode = "password",
  showDevelopmentPresentation = import.meta.env.DEV,
}: AppControllerOptions = {}) {
  if (!firstAppRenderLogged) {
      firstAppRenderLogged = true;
      startupConsoleInfo("[const-api][startup][ui] App first render in %dms", Math.round(performance.now() - UI_SCRIPT_STARTED_AT));
      startupLogFromUi("App first render");
    }

  const [tab, setTab] = useState<AppTab>(initialTab);

  const [config, setConfig] = useState<ClientConfig>(emptyConfig);

  const [savedConfig, setSavedConfig] = useState<ClientConfig>(emptyConfig);

  const [configActionBusy, setConfigActionBusy] = useState(false);

  const [status, setStatus] = useState<ProxyStatus>({ running: false, listen: emptyConfig.listen });

  const [supplierStatus, setSupplierStatus] = useState<SupplierStatus>({
      running: false,
      starting: false,
      node_id: "",
      server_ws_url: "",
      server_quic_url: "",
      transport_preference: "",
      connected_transport: "",
      active_transport: "",
      last_transport_error: "",
      route_statuses: [],
      channel_transports: [],
    });

  const [supplierQuotaSnapshots, setSupplierQuotaSnapshots] = useState<SupplierQuotaSnapshotMap>(readSupplierQuotaSnapshots);

  const account = useAccountCenter({
      api: tauriAccountApi,
      initialStatus: emptyAccountStatus(),
      initialAuthMode: initialAccountAuthMode,
      notify: (notice) => showToast(notice.text, notice.tone, notice.tone === "error" ? 4200 : undefined),
      afterIdentityChange: async (next) => {
        platformCallGenerationRef.current += 1;
        platformCallSyncRef.current = {};
        accountStateRef.current = next.state;
        setUsage([]);
        setCallRecords([]);
        setUsageStats([]);
        if (activeCallLogRef.current?.source === "platform") closeCallLog();
        const reloaded = await reload();
        if (next.state === "signed_in" && reloaded) {
          void ensureAutoStartup(reloaded.cfg, reloaded.st, reloaded.supplierSt);
        }
      },
    });

  const [usage, setUsage] = useState<any[]>([]);

  const [callRecords, setCallRecords] = useState<CallRecord[]>([]);

  const [usageStats, setUsageStats] = useState<UsageStat[]>([]);

  const [localChannelCalls, setLocalChannelCalls] = useState<CallRecord[]>([]);

  const [localChannelStats, setLocalChannelStats] = useState<UsageStat[]>([]);

  const [activeCallLog, setActiveCallLog] = useState<ActiveCallLog | null>(null);

  const [selectedCallRecord, setSelectedCallRecord] = useState<CallRecord | null>(null);

  const [providers, setProviders] = useState<PlatformProvider[]>([]);

  const [platformSupplierNodes, setPlatformSupplierNodes] = useState<PlatformSupplierNode[]>([]);

  const [toolConfigStatuses, setToolConfigStatuses] = useState<Record<string, ToolApplyResult | null>>({});

  const [hiddenToolIds, setHiddenToolIds] = useState<Set<ToolCatalogId>>(readHiddenToolIds);

  const [toolOrder, setToolOrder] = useState<ToolCatalogId[]>(readToolOrder);

  const [toolConfigChecking, setToolConfigChecking] = useState(false);

  const toolConfigStatusEpochRef = useRef(0);

  const toolProtocolDraftTouchedRef = useRef<Record<string, boolean>>({});

  const codexModelSourceTouchedRef = useRef(false);
  const [codexModelSource, setCodexModelSource] = useState<CodexModelSource>("codex");

  function changeCodexModelSource(value: CodexModelSource) {
    codexModelSourceTouchedRef.current = true;
    setCodexModelSource(value);
  }

  const [toolProtocolSelections, setToolProtocolSelections] = useState<Record<string, ToolProtocolId>>(
      DEFAULT_TOOL_PROTOCOLS,
    );

  const [claudeModelSettings, setClaudeModelSettings] = useState<ClaudeModelSettings>(
      readClaudeModelSettings,
    );

  const [claudeModelDraft, setClaudeModelDraft] = useState<ClaudeModelSettings>(
      () => ({ ...claudeModelSettings }),
    );

  const toolOperationRunningRef = useRef<Record<string, boolean>>({});

  const [toolLaunching, setToolLaunching] = useState<Record<string, boolean>>({});

  const [toolOperationLabels, setToolOperationLabels] = useState<Record<string, string>>({});

  const [toolOperationProgress, setToolOperationProgress] = useState<Record<string, ToolOperationProgress>>({});

  const [codexSessionScan, setCodexSessionScan] = useState<CodexSessionScanResult | null>(null);

  const [toolLocator, setToolLocator] = useState<{
      tool: string;
      title: string;
      result: ToolProgramLocationResult | null;
      loading: boolean;
      error: string;
      continueWithLaunch: boolean;
    } | null>(null);

  const [showMissingToolCandidates, setShowMissingToolCandidates] = useState(false);

  const [supplierModels, setSupplierModels] = useState<string[]>([]);

  const [subscriptionWizardProvider, setSubscriptionWizardProvider] = useState<SubscriptionProvider>("openai");

  const [subscriptionWizardOpen, setSubscriptionWizardOpen] = useState(false);

  const [subscriptionOAuthSession, setSubscriptionOAuthSession] = useState<SubscriptionOAuthSession | null>(null);

  const subscriptionOAuthActiveStateRef = useRef<string | null>(null);

  const subscriptionOAuthExchangeInFlightRef = useRef(false);

  const [subscriptionCallbackInput, setSubscriptionCallbackInput] = useState("");


  const [subscriptionTokenJson, setSubscriptionTokenJson] = useState("");

  const [subscriptionManualOpen, setSubscriptionManualOpen] = useState(false);

  const [subscriptionWizardBusy, setSubscriptionWizardBusy] = useState(false);

  const [subscriptionWizardProgress, setSubscriptionWizardProgress] =
    useState<SubscriptionWizardProgress | null>(null);

  const [subscriptionImportRetryChannel, setSubscriptionImportRetryChannel] =
    useState<ChannelConfigV5 | null>(null);

  useEffect(() => () => {
    const oauthState = subscriptionOAuthActiveStateRef.current;
    if (oauthState) {
      void invoke<void>("cancel_subscription_oauth_callback", { oauthState }).catch(() => {
        // Automatic callback capture is optional; manual paste remains available.
      });
    }
  }, []);

  const [selectedChannelId, setSelectedChannelId] = useState<string>("");

  const [supplierAddOpen, setSupplierAddOpen] = useState(false);

  const [supplierDetailOpen, setSupplierDetailOpen] = useState(false);

  const [supplierStartingChannelId, setSupplierStartingChannelId] = useState<string | null>(null);

  const [supplierChannelOperation, setSupplierChannelOperation] = useState<SupplierChannelOperation | null>(null);

  const [pendingSupplierChannelIds, setPendingSupplierChannelIds] = useState<string[]>([]);

  const [showAccessAdvanced, setShowAccessAdvanced] = useState(false);

  const [accessGuideOpen, setAccessGuideOpen] = useState(false);

  const [localApiConnectionId, setLocalApiConnectionId] =
    useState<LocalApiConnectionId>(DEFAULT_LOCAL_API_CONNECTION_ID);

  const [modelAliasPreviewOpen, setModelAliasPreviewOpen] = useState(false);

  const [modelCompatibilityPolicy, setModelCompatibilityPolicy] = useState<ModelCompatibilityPolicyState>(emptyModelCompatibilityPolicy);

  const [modelCompatibilityDraft, setModelCompatibilityDraft] = useState<ModelCompatibilityGroup[]>([]);

  const [modelCompatibilitySaving, setModelCompatibilitySaving] = useState(false);

  const [showSupplierAdvanced, setShowSupplierAdvanced] = useState(false);

  const [showSupplierSurfaceBindings, setShowSupplierSurfaceBindings] = useState(false);

  const [showSupplierProtocolDeclaration, setShowSupplierProtocolDeclaration] = useState(false);

  useEffect(() => {
    if (tab !== "access") {
      setShowAccessAdvanced(false);
    }
    if (tab !== "supplier") {
      setShowSupplierAdvanced(false);
      setShowSupplierSurfaceBindings(false);
      setShowSupplierProtocolDeclaration(false);
    }
  }, [tab]);

  const [debugConsoleOpen, setDebugConsoleOpen] = useState(false);

  const [debugTargetType, setDebugTargetType] = useState<DebugTargetType>("platform_auto");

  const [debugTargetId, setDebugTargetId] = useState("");

  const [expandedDebugTargetGroups, setExpandedDebugTargetGroups] = useState<DebugTargetType[]>([]);

  const [debugInboundProtocol, setDebugInboundProtocol] = useState<DebugProtocol>("openai_chat");

  const [debugTargetProtocol, setDebugTargetProtocol] = useState<DebugProtocol>("openai_chat");

  const [debugModel, setDebugModel] = useState("");

  const [debugPrompt, setDebugPrompt] = useState(randomSimpleEnglishPrompt);

  const [debugStream, setDebugStream] = useState(false);

  const [debugSkipLocalShortCircuit, setDebugSkipLocalShortCircuit] = useState(true);

  const [debugClaudeServerSideCompaction, setDebugClaudeServerSideCompaction] = useState(false);

  const [debugResult, setDebugResult] = useState<ProtocolDebugResult | null>(null);

  const [debugRunning, setDebugRunning] = useState<"idle" | "preview" | "execute">("idle");

  const [routePlanState, setRoutePlanState] = useState<RoutePlanState>({ status: "idle" });

  const [debugEndpointRefreshing, setDebugEndpointRefreshing] = useState(false);

  const [debugEndpointLastRefresh, setDebugEndpointLastRefresh] = useState<number | null>(null);

  const [debugEndpointError, setDebugEndpointError] = useState("");

  const debugEndpointAutoRefreshAttemptedRef = useRef(false);

  const debugEndpointRefreshInFlightRef = useRef(false);

  const [supplierTestPrompt, setSupplierTestPrompt] = useState(randomSimpleEnglishPrompt);

  const [supplierTestResult, setSupplierTestResult] = useState<TestResult>({ status: "idle" });

  const [updateState, setUpdateState] = useState<UpdateState>({ status: "idle" });

  const [autoInstallUpdates, setAutoInstallUpdatesState] = useState(!DEVELOPMENT_PROFILE);

  const [autostartEnabled, setAutostartEnabled] = useState(false);

  const [autostartBusy, setAutostartBusy] = useState(false);

  const [developmentEndpointBusy, setDevelopmentEndpointBusy] = useState(false);

  const [themePreference, setThemePreference] = useState<ThemePreference>(INITIAL_THEME_PREFERENCE);

  const [appMeta, setAppMeta] = useState<AppMetaState>({
      currentVersion: CURRENT_VERSION,
      updateSources: [],
      endpointSources: emptyConfig.registry_sources,
      endpointStatus: "idle",
    });

  const [toast, setToast] = useState<Toast | null>(null);

  const [guidanceNotices, setGuidanceNotices] = useState<GuidanceNotice[]>([]);

  const [confirmDialog, setConfirmDialog] = useState<ConfirmDialogState | null>(null);

  const confirmResolverRef = useRef<((confirmed: boolean) => void) | null>(null);

  const [toolRemoveDialog, setToolRemoveDialog] =
    useState<ToolRemoveDialogState | null>(null);

  const toolRemoveResolverRef =
    useRef<((decision: ToolRemoveDecision) => void) | null>(null);

  const [channelDuplicateDialog, setChannelDuplicateDialog] =
    useState<ChannelDuplicateDialogState | null>(null);

  const channelDuplicateResolverRef =
    useRef<((decision: ChannelDuplicateDecision) => void) | null>(null);

  const [appLogs, setAppLogs] = useState<AppLogEntry[]>([]);

  const [logOpen, setLogOpen] = useState(false);

  const [copiedField, setCopiedField] = useState<CopyField | null>(null);

  const [localEntryDiagnosticReady, setLocalEntryDiagnosticReady] = useState(false);

  const localEntryStartupCompleteRef = useRef(false);

  const proxyStatusEpochRef = useRef(0);

  const toastTimer = useRef<number | null>(null);

  const guidanceNoticesRef = useRef<GuidanceNotice[]>([]);

  const copyTimer = useRef<number | null>(null);

  const supplierChannelOperationRef = useRef<SupplierChannelOperation | null>(null);

  const supplierChannelOperationTokenRef = useRef(0);

  const supplierStatusRef = useRef(supplierStatus);

  const accountStateRef = useRef(account.status.state);

  const configRef = useRef(config);

  const savedConfigRef = useRef(savedConfig);

  const configActionInFlightRef = useRef(false);

  const selectedCallRecordRef = useRef<CallRecord | null>(null);

  const activeCallLogRef = useRef<ActiveCallLog | null>(null);

  const platformCallSyncRef = useRef<CallSyncState>({});

  const platformCallGenerationRef = useRef(0);

  const diagnosticToastMessagesRef = useRef<Map<string, string>>(new Map());

  const routeStatusToastMessagesRef = useRef<Map<string, ReturnType<typeof supplierRouteStatusToastSnapshot>>>(new Map());

  const endpointRefreshWarningRef = useRef<string | undefined>(undefined);

  const endpointRegistryRefreshInFlightRef = useRef(false);

  const modelCompatibilityRefreshGateRef = useRef(createRefreshGate());

  const modelCompatibilityRefreshInFlightRef = useRef<Promise<void> | null>(null);

  const modelCompatibilityContextRef = useRef<string | null>(null);

  const supplierModelRefreshGatesRef = useRef(new Map<string, RefreshGate>());

  const hasChanges = useMemo(
      () => JSON.stringify(config) !== JSON.stringify(savedConfig),
      [config, savedConfig],
    );

  const runtimeDiagnostics = useMemo(
      () => buildRuntimeDiagnostics(status, supplierStatus, appMeta, config, localEntryDiagnosticReady && !configActionBusy),
      [
        status.running,
        status.active_endpoint,
        supplierStatus.running,
        supplierStatus.starting,
        supplierStatus.active_transport,
        supplierStatus.last_transport_error,
        supplierStatus.last_error,
        supplierStatus.route_statuses,
        appMeta.endpointStatus,
        appMeta.endpointLastError,
        appMeta.updateLastError,
        config.proxy_auto_start,
        config.listen,
        config.channels,
        localEntryDiagnosticReady,
        configActionBusy,
      ],
    );

  const { activeDiagnostics, hasDiagnostics, pushDiagnostic, resolveDiagnostic } = useDiagnostics(updateState, runtimeDiagnostics);

  const localEntryIssue = activeDiagnostics.find((issue) => issue.id === "local-entry");

  const brandVersion = (updateState.status === "ready" || updateState.status === "ready_waiting_idle") && updateState.version
    ? updateState.version
    : appMeta.currentVersion;

  const updateReady = (updateState.status === "ready" || updateState.status === "ready_waiting_idle")
    && Boolean(updateState.version);

  const localApiListen = status.running ? status.listen : config.listen;

  const selectedLocalApiConnection = localApiConnectionById(localApiConnectionId, localApiListen);

  const localApiRootBase = localLoopbackApiRoot(localApiListen) ?? "";

  const localApiRoot = localApiRootBase ? `${localApiRootBase}/` : "";

  const localBaseUrl = localApiSurfaceById("openai", localApiListen).url;

  const localOpenAiChatUrl = localApiConnectionById("openai-chat", localApiListen).url;

  const localAnthropicBaseUrl = localApiSurfaceById("anthropic", localApiListen).url;

  const localGeminiBaseUrl = localApiSurfaceById("gemini", localApiListen).url;

  const channels = useMemo(
      () => config.channels.length > 0 ? config.channels : [channelFromSupplier("channel-1", config.supplier)],
      [config.channels, config.supplier],
    );

  const displayChannels = useMemo(
      () => channelsVisibleInModelMarket(channels, showDevelopmentPresentation),
      [channels, showDevelopmentPresentation],
    );

  const selectedChannel = displayChannels.find((channel) => channel.id === selectedChannelId)
    ?? displayChannels[0]
    ?? channels.find((channel) => channel.id === selectedChannelId)
    ?? channels[0];

  const selectedApiFormat = sanitizeApiFormat(selectedChannel.kind, selectedChannel.api_format);

  const selectedModelOptions = selectedChannel.models.length > 0 ? selectedChannel.models : supplierModels;

  const selectedChannelKindLabel = channelKindLabel(selectedChannel.kind);

  const selectedChannelFields = channelFieldProfile(
      selectedChannel.kind,
      selectedChannel.source_driver,
    );

  const selectedPrimaryProtocolOptions = channelPrimaryProtocolOptions(
      selectedChannel.kind,
      selectedChannel.supported_protocols,
    );

  const selectedSurfaceBindings = useMemo(
      () => normalizeSurfaceBindings(selectedChannel),
      [selectedChannel],
    );

  const selectedSurfaceEditorRows = useMemo(
      () => surfaceEditorRows(selectedChannel, selectedSurfaceBindings),
      [selectedChannel, selectedSurfaceBindings],
    );

  const selectedChannelModel = selectedChannel.default_model || selectedChannel.upstream_model || selectedChannel.public_model || selectedChannel.models[0] || "";

  const selectedChannelCanDetect = channelCanDetect(selectedChannel);

  const selectedChannelCanSupply = channelCanSupply(selectedChannel);

  const selectedChannelHealth = supplierStatus.channels?.find((item) => item.channel_id === selectedChannel.id);

  const selectedChannelProtocolLatencyHealth = selectedChannelHealth?.upstream_http_version?.trim()
      ? selectedChannelHealth
      : supplierTestResult.status === "success" && supplierTestResult.upstreamHttpVersion?.trim()
        ? {
            upstream_http_version: supplierTestResult.upstreamHttpVersion,
            latency_ms: supplierTestResult.latencyMs ?? 0,
          }
        : selectedChannelHealth;

  const selectedChannelQuotaHealth = resolveSupplierQuotaHealth(
      selectedChannelHealth,
      supplierQuotaSnapshots[selectedChannel.id],
    );

  const selectedChannelRouteStatus = supplierRouteStatusForChannel(supplierStatus, selectedChannel.id);

  const selectedChannelRouteLabel = supplierRouteStatusInlineLabel(selectedChannelRouteStatus);

  const selectedChannelTransport = supplierChannelTransportForChannel(supplierStatus, selectedChannel.id);

  const selectedChannelSwitchOn = channelIsPooled(selectedChannel);

  const savedChannels = useMemo(
      () => savedConfig.channels.length > 0 ? savedConfig.channels : [channelFromSupplier("channel-1", savedConfig.supplier)],
      [savedConfig.channels, savedConfig.supplier],
    );

  const savedSelectedChannel = savedChannels.find((channel) => channel.id === selectedChannel.id);

  const selectedChannelHasChanges = useMemo(
      () => channelEditorSettingsChanged(savedSelectedChannel, selectedChannel),
      [savedSelectedChannel, selectedChannel],
    );

  const selectedChannelOperation = supplierChannelOperation?.channelId === selectedChannel.id
      ? supplierChannelOperation.kind
      : null;

  const selectedChannelBusy = Boolean(
      selectedChannelOperation
        || supplierStartingChannelId === selectedChannel.id
        || pendingSupplierChannelIds.includes(selectedChannel.id),
    );

  const supplierLocalReadyCount = displayChannels.filter((channel) => {
      const health = supplierStatus.channels?.find((item) => item.channel_id === channel.id);
      return channelLocallyReady(channel, health);
    }).length;

  const supplierPlatformOnlineCount = displayChannels.filter((channel) => {
      const health = supplierStatus.channels?.find((item) => item.channel_id === channel.id);
      const routeStatus = supplierRouteStatusForChannel(supplierStatus, channel.id);
      const channelTransport = supplierChannelTransportForChannel(supplierStatus, channel.id);
      return isEstablishedSupplierTransport(channelTransport?.active_transport)
        && channelTransport?.platform_registered === true
        && health?.status === "available"
        && !supplierRouteStatusBlocksReady(routeStatus);
    }).length;

  const selectedChannelLifecycle = supplierLifecyclePresentation({
      saved: !selectedChannelHasChanges,
      enabledForSupply: selectedChannelSwitchOn,
      busy: selectedChannelBusy,
      checking: supplierChannelOperation?.checksUpstream,
      detectionReady: selectedChannel.models.length > 0,
      localStatus: selectedChannelHealth?.status,
      accountState: account.status.state,
      runtimeRunning: supplierStatus.running || supplierStatus.starting === true,
      connectedTransport: selectedChannelTransport?.connected_transport,
      activeTransport: selectedChannelTransport?.active_transport,
      routeState: selectedChannelRouteStatus?.state,
      routeLabel: selectedChannelRouteLabel,
    });

  const supplierTotalCount = configuredModelMarketChannelCount(channels);

  const platformModels = useMemo(
      () => platformModelOptions(providers, platformSupplierNodes),
      [platformSupplierNodes, providers],
    );

  const localDebugTargets = useMemo(
      () => debugTargetOptions("local_channel", channels, platformSupplierNodes, platformModels),
      [channels, platformModels, platformSupplierNodes],
    );

  const platformAutoDebugTargets = useMemo(
      () => debugTargetOptions("platform_auto", channels, platformSupplierNodes, platformModels),
      [channels, platformModels, platformSupplierNodes],
    );

  const platformNodeDebugTargets = useMemo(
      () => debugTargetOptions("platform_node", channels, platformSupplierNodes, platformModels),
      [channels, platformModels, platformSupplierNodes],
    );

  const debugTargetGroups = [
      { type: "platform_auto" as DebugTargetType, title: tr("debugConsole.targetTypes.platformAuto"), targets: platformAutoDebugTargets },
      { type: "local_channel" as DebugTargetType, title: tr("debugConsole.targetTypes.local"), targets: localDebugTargets },
      { type: "platform_node" as DebugTargetType, title: tr("debugConsole.targetTypes.platformNode"), targets: platformNodeDebugTargets },
    ];

  const debugTargets = debugTargetType === "local_channel"
      ? localDebugTargets
      : debugTargetType === "platform_node"
        ? platformNodeDebugTargets
        : platformAutoDebugTargets;

  const selectedDebugTarget = debugTargets.find((target) => target.id === debugTargetId) ?? debugTargets[0];

  const effectiveDebugTargetId = selectedDebugTarget?.id ?? "";

  const debugModelOptions = selectedDebugTarget?.models ?? [];

  const effectiveDebugModel = resolveDebugModelSelection(
      debugModel,
      selectedDebugTarget?.defaultModel ?? "",
      debugModelOptions,
    );

  const debugResponseContent = debugResult?.executed
      ? (debugResult.content || debugResult.raw || tr("debugConsole.noExtractedContent"))
      : tr("debugConsole.waitingExecute");

  const debugRawResponse = debugResult?.executed && debugResult.raw && debugResult.raw !== debugResult.content
      ? debugResult.raw
      : "";

  useEffect(() => {
      for (const issue of activeDiagnostics) {
        if (issue.createdAt !== 0) continue;
        const messageKey = diagnosticToastEventKey(issue);
        if (diagnosticToastMessagesRef.current.get(issue.id) === messageKey) continue;
        diagnosticToastMessagesRef.current.set(issue.id, messageKey);
        showToast(`${issue.title}: ${issue.message}`, "error", 5200);
      }
      if (appMeta.endpointStatus === "success") diagnosticToastMessagesRef.current.delete("endpoint-registry");
      if (!appMeta.updateLastError && updateState.status !== "error") diagnosticToastMessagesRef.current.delete("update-source");
      if (!localEntryDiagnosticReady || !config.proxy_auto_start || status.running) diagnosticToastMessagesRef.current.delete("local-entry");
      const activeDiagnosticIds = new Set(activeDiagnostics.map((issue) => issue.id));
      for (const issueId of diagnosticToastMessagesRef.current.keys()) {
        if (issueId.startsWith("supplier-model-catalog-") && !activeDiagnosticIds.has(issueId)) {
          diagnosticToastMessagesRef.current.delete(issueId);
        }
      }
    }, [
      activeDiagnostics,
      appMeta.endpointStatus,
      appMeta.updateLastError,
      updateState.status,
      supplierStatus.active_transport,
      config.proxy_auto_start,
      status.running,
      localEntryDiagnosticReady,
    ]);

  useEffect(() => {
      for (const routeStatus of supplierStatus.route_statuses ?? []) {
        const state = normalizedRouteState(routeStatus);
        if (!state) continue;
        const channelId = routeStatus.channel_id?.trim() ?? "";
        const channel = channels.find((item) => item.id === channelId);
        const channelName = channel?.name
          || channelId
          || routeStatus.supplier_unit_id
          || routeStatus.node_id
          || tr("supplier.channel");
        const nextToastStatus = supplierRouteStatusToastSnapshot(routeStatus);
        const statusIdentity = supplierRouteStatusToastIdentity(routeStatus) || nextToastStatus.signature;
        const previousToastStatus = routeStatusToastMessagesRef.current.get(statusIdentity);
        routeStatusToastMessagesRef.current.set(statusIdentity, nextToastStatus);
        if (!shouldShowSupplierRouteStatusToast(previousToastStatus, nextToastStatus)) continue;
        const toastText = supplierRouteStatusToastText(routeStatus, channelName);
        if (toastText) showToast(toastText, supplierRouteStatusToastTone(routeStatus), 5200);
      }
      if (routeStatusToastMessagesRef.current.size > 120) {
        routeStatusToastMessagesRef.current = new Map(Array.from(routeStatusToastMessagesRef.current).slice(-80));
      }
    }, [supplierStatus.route_statuses, channels]);

  const debugMetrics = debugResult?.metrics;

  const routePlanPath = accessTestRequestPath(debugInboundProtocol, effectiveDebugModel);

  const routePlanUnavailableReason = debugTargetType !== "platform_auto"
      ? tr("debugConsole.route.platformOnly")
      : "";

  const routePlanSourceText = tr("debugConsole.route.source", { path: routePlanPath });

  const debugEndpointStatusText = debugEndpointRefreshing
      ? tr("debugConsole.endpointStatus.refreshing")
      : debugEndpointError
        ? tr("debugConsole.endpointStatus.failed")
        : debugEndpointLastRefresh
          ? tr("debugConsole.endpointStatus.refreshed", {
            time: formatRelativeTime(debugEndpointLastRefresh),
          })
          : tr("debugConsole.endpointStatus.notRefreshed");

  const recommendedDebugTargetProtocol = debugRecommendedTargetProtocol(
      selectedDebugTarget,
      debugInboundProtocol,
      debugTargetType,
    );

  const debugConversionText = debugInboundProtocol === debugTargetProtocol
      ? tr("debugConsole.noConversion")
      : `${protocolLabel(debugInboundProtocol)} -> ${protocolLabel(debugTargetProtocol)}`;

  const toolCardPresentation: Record<ToolCatalogId, { icon: BrandIconData | null; customIcon?: CustomBrandIcon; fallback: LucideIcon; tone: string }> = {
      codex: { icon: null, customIcon: "codex", fallback: Terminal, tone: "#111827" },
      claude: { icon: siClaudecode, fallback: Code2, tone: "#d97706" },
      "claude-desktop": { icon: null, customIcon: "claude", fallback: Laptop, tone: "#b45309" },
      "claude-science": { icon: null, customIcon: "claude-science", fallback: Bot, tone: "#d97757" },
      gemini: { icon: null, customIcon: "gemini", fallback: BrainCircuit, tone: "#1d4ed8" },
      copilot: { icon: null, customIcon: "copilot", fallback: Code2, tone: "#8534f3" },
      vscode: { icon: null, customIcon: "vscode", fallback: Code2, tone: "#007acc" },
      cline: { icon: null, customIcon: "cline", fallback: Bot, tone: "#24292f" },
      opencode: { icon: null, customIcon: "opencode", fallback: Bot, tone: "#211e1e" },
      openclaw: { icon: null, customIcon: "openclaw", fallback: Zap, tone: "#dc2626" },
      goose: { icon: null, customIcon: "goose", fallback: Bot, tone: "#111827" },
      raven: { icon: null, customIcon: "raven", fallback: Bot, tone: "#0153e5" },
      reasonix: { icon: null, customIcon: "reasonix", fallback: BrainCircuit, tone: "#09090b" },
      "deepseek-harness": { icon: null, customIcon: "deepseek-harness", fallback: BrainCircuit, tone: "var(--text)" },
      pi: { icon: null, customIcon: "pi", fallback: Terminal, tone: "#171717" },
      kimicode: { icon: null, customIcon: "kimi", fallback: Terminal, tone: "#111827" },
      mimocode: { icon: null, customIcon: "mimocode", fallback: Code2, tone: "#ff7a45" },
      qwencode: { icon: null, customIcon: "qwen", fallback: BrainCircuit, tone: "#6d44e8" },
      "mistral-vibe": { icon: null, customIcon: "mistral-vibe", fallback: Terminal, tone: "#fa500f" },
      hermes: { icon: null, customIcon: "hermes", fallback: Cable, tone: "#475569" },
      "open-interpreter": { icon: null, customIcon: "open-interpreter", fallback: Terminal, tone: "#171717" },
      anythingllm: { icon: null, customIcon: "anythingllm", fallback: Bot, tone: "#171717" },
      openscience: { icon: null, customIcon: "openscience", fallback: Bot, tone: "#123a8c" },
      "open-design": { icon: null, customIcon: "open-design", fallback: Bot, tone: "#202020" },
      workbuddy: { icon: null, customIcon: "workbuddy", fallback: Bot, tone: "#0bc89f" },
      "vibe-trading": { icon: null, customIcon: "vibe-trading", fallback: Bot, tone: "#ef5a46" },
      zcode: { icon: null, customIcon: "zcode", fallback: Code2, tone: "#111315" },
    };
  const toolCards = TOOL_CATALOG_ORDER.map((tool) => ({
      ...TOOL_CATALOG[tool],
      ...toolCardPresentation[tool],
    }));
  const visibleToolCards = toolOrder
    .filter((tool) => !hiddenToolIds.has(tool))
    .map((tool) => ({
      ...TOOL_CATALOG[tool],
      ...toolCardPresentation[tool],
    }));
  const hiddenToolCards = toolCards.filter((card) => hiddenToolIds.has(card.tool));

  function hideToolCard(tool: ToolCatalogId) {
      setHiddenToolIds((current) => {
        const next = new Set(current);
        next.add(tool);
        writeHiddenToolIds(next);
        return next;
      });
    }

  function showToolCard(tool: ToolCatalogId) {
      setHiddenToolIds((current) => {
        const next = new Set(current);
        next.delete(tool);
        writeHiddenToolIds(next);
        return next;
      });
      setToolOrder((current) => {
        const next = moveToolToEnd(current, tool);
        writeToolOrder(next);
        return next;
      });
    }

  function resetToolOrder() {
      const next = defaultToolOrder();
      writeToolOrder(next);
      setToolOrder(next);
    }

  const subscriptionCards = sourceDriversForCategory("subscription");

  const apiAccessCards = sourceDriversForApiAccess();

  const lanShareCards = sourceDriversForLanSharing();

  const customCards = sourceDriversForCustom();

  useEffect(() => {
      startupConsoleInfo("[const-api][startup][ui] App mounted in %dms", Math.round(performance.now() - UI_SCRIPT_STARTED_AT));
      startupLogFromUi("App mounted");
      void initialize();
      return () => {
        if (toastTimer.current) {
          window.clearTimeout(toastTimer.current);
        }
        if (copyTimer.current) {
          window.clearTimeout(copyTimer.current);
        }
      };
    }, []);

  useEffect(() => {
      if (tab !== "access" && !modelAliasPreviewOpen && !accessGuideOpen) return;
      const context = `${savedConfig.listen}\u0000${savedConfig.api_key}\u0000${account.status.state}`;
      const contextChanged = modelCompatibilityContextRef.current !== null
        && modelCompatibilityContextRef.current !== context;
      modelCompatibilityContextRef.current = context;
      void refreshModelCompatibilityPolicy(contextChanged ? { force: true } : { throttle: true });
      const timer = window.setInterval(() => {
        if (document.visibilityState !== "visible") return;
        void refreshModelCompatibilityPolicy({ throttle: true });
      }, UI_SNAPSHOT_AUTO_REFRESH_MS);
      return () => window.clearInterval(timer);
    }, [
      accessGuideOpen,
      account.status.state,
      modelAliasPreviewOpen,
      savedConfig.api_key,
      savedConfig.listen,
      tab,
    ]);

  useEffect(() => {
      if (tab !== "about" || !isTauriRuntime()) return;
      void invoke<ProxyStatus>("refresh_platform_status")
        .then((next) => {
          setStatus((current) => keepPreviousIfStructurallyEqual(current, next));
        })
        .catch(() => {
          // The regular native status loop keeps the last healthy snapshot.
        });
      void refreshModelCatalogVersionInfo();
      const timer = window.setInterval(() => {
        void refreshModelCatalogVersionInfo();
      }, 30_000);
      return () => window.clearInterval(timer);
    }, [tab, account.status.state]);

  useEffect(() => {
      if (DEVELOPMENT_PROFILE || !isTauriRuntime()) return;
      let disposed = false;
      let unlisten: (() => void) | undefined;
      void (async () => {
        try {
          const stop = await listen<NativeUpdateStatus>("const-api://update-state", (event) => {
            if (!disposed) applyNativeUpdateStatus(event.payload);
          });
          if (disposed) {
            stop();
            return;
          }
          unlisten = stop;
          let status = await invoke<NativeUpdateStatus>("native_update_status");
          const legacyPreference = readLegacyAutoInstallUpdatesPreference();
          if (legacyPreference !== undefined) {
            status = await invoke<NativeUpdateStatus>("set_automatic_updates", {
              enabled: legacyPreference,
            });
            clearLegacyAutoInstallUpdatesPreference();
          }
          if (!disposed) applyNativeUpdateStatus(status);
        } catch (error) {
          recordAppLog(`native updater: ${String(error)}`, "error");
        }
      })();
      return () => {
        disposed = true;
        unlisten?.();
      };
    }, []);

  useEffect(() => {
      if (DEVELOPMENT_PROFILE || !isTauriRuntime()) return;
      const syncNativeUpdateStatus = () => {
        if (document.visibilityState === "hidden") return;
        void invoke<NativeUpdateStatus>("native_update_status")
          .then(applyNativeUpdateStatus)
          .catch(() => undefined);
      };
      window.addEventListener("focus", syncNativeUpdateStatus);
      document.addEventListener("visibilitychange", syncNativeUpdateStatus);
      return () => {
        window.removeEventListener("focus", syncNativeUpdateStatus);
        document.removeEventListener("visibilitychange", syncNativeUpdateStatus);
      };
    }, []);

  useEffect(() => {
      configRef.current = config;
    }, [config]);

  useEffect(() => {
      savedConfigRef.current = savedConfig;
    }, [savedConfig]);

  useEffect(() => {
      supplierStatusRef.current = supplierStatus;
    }, [supplierStatus]);

  useEffect(() => {
      accountStateRef.current = account.status.state;
    }, [account.status.state]);

  useEffect(() => {
      selectedCallRecordRef.current = selectedCallRecord;
    }, [selectedCallRecord]);

  useEffect(() => {
      activeCallLogRef.current = activeCallLog;
    }, [activeCallLog]);

  useEffect(() => {
      if (!isTauriRuntime() || !config.listen) return;
      void refreshToolConfigStatuses();
    }, [config.listen, claudeModelSettings]);

  useEffect(() => {
      const nextChannels = config.channels.length > 0 ? config.channels : [channelFromSupplier("channel-1", config.supplier)];
      if (!nextChannels.some((channel) => channel.id === selectedChannelId)) {
        setSelectedChannelId(nextChannels[0]?.id ?? "");
      }
    }, [config.channels, config.supplier, selectedChannelId]);

  useEffect(() => {
      if (!supplierStatus.channels?.length) return;
      setSupplierQuotaSnapshots((current) => {
        let next = current;
        for (const health of supplierStatus.channels ?? []) {
          next = rememberSupplierQuotaSnapshot(next, health.channel_id, health, health.checked_at_unix);
        }
        if (next !== current) writeSupplierQuotaSnapshots(next);
        return next;
      });
    }, [supplierStatus.channels]);

  useEffect(() => {
      if (!isTauriRuntime()) return;
      let cancelled = false;
      let timer: number | undefined;
      const refreshSupplierStatus = async () => {
        try {
          const next = await invoke<SupplierStatus>("supplier_status");
          if (!cancelled) {
            setSupplierStatus((current) => keepPreviousIfStructurallyEqual(current, next));
          }
        } catch {
          // Keep the last known UI state; explicit actions still surface errors.
        } finally {
          if (!cancelled) timer = window.setTimeout(refreshSupplierStatus, 5000);
        }
      };
      timer = window.setTimeout(refreshSupplierStatus, 750);
      return () => {
        cancelled = true;
        if (timer !== undefined) window.clearTimeout(timer);
      };
    }, []);

  useEffect(() => {
      if (!isTauriRuntime()) return;
      let cancelled = false;
      let timer: number | undefined;
      const refreshProxyStatus = async () => {
        try {
          // Startup owns the initial status. A timer is not evidence that the
          // listener has finished starting, and pre-action reads may be stale.
          if (!localEntryStartupCompleteRef.current || configActionInFlightRef.current) return;
          const epoch = proxyStatusEpochRef.current;
          const next = await invoke<ProxyStatus>("proxy_status");
          if (!cancelled && epoch === proxyStatusEpochRef.current && !configActionInFlightRef.current) {
            setStatus((current) => keepPreviousIfStructurallyEqual(current, next));
            setLocalEntryDiagnosticReady(true);
          }
        } catch {
          // Keep the last known local status; explicit proxy actions report errors.
        } finally {
          if (!cancelled) timer = window.setTimeout(refreshProxyStatus, 5000);
        }
      };
      timer = window.setTimeout(refreshProxyStatus, 1000);
      return () => {
        cancelled = true;
        if (timer !== undefined) window.clearTimeout(timer);
      };
    }, []);

  useEffect(() => {
      if (!isTauriRuntime()) return;
      let cancelled = false;
      let timer: number | undefined;
      const refresh = async () => {
        try {
          await account.actions.readStatus();
        } catch {
          // Account actions surface explicit errors; background status stays quiet.
        } finally {
          if (!cancelled) timer = window.setTimeout(refresh, 30000);
        }
      };
      timer = window.setTimeout(refresh, 30000);
      return () => {
        cancelled = true;
        if (timer !== undefined) window.clearTimeout(timer);
      };
    }, []);

  useEffect(() => {
      const media = typeof window.matchMedia === "function"
        ? window.matchMedia("(prefers-color-scheme: dark)")
        : undefined;
      const syncTheme = () => {
        const resolved = applyDocumentTheme(themePreference, media?.matches ?? false);
        if (isTauriRuntime()) {
          void invoke("sync_native_theme", { dark: resolved === "dark" }).catch((error) => {
            console.warn("Failed to synchronize the native window theme", error);
          });
        }
      };
      writeThemePreference(themePreference);
      syncTheme();
      if (themePreference !== "system" || !media) return;
      media.addEventListener("change", syncTheme);
      return () => media.removeEventListener("change", syncTheme);
    }, [themePreference]);

  useEffect(() => {
      setSupplierTestResult({ status: "idle" });
      setShowSupplierSurfaceBindings(false);
      setShowSupplierProtocolDeclaration(false);
    }, [selectedChannelId]);

  useEffect(() => {
      if (!debugConsoleOpen) {
        debugEndpointAutoRefreshAttemptedRef.current = false;
        return;
      }
      if (!isTauriRuntime() || debugEndpointLastRefresh || debugEndpointAutoRefreshAttemptedRef.current) return;
      debugEndpointAutoRefreshAttemptedRef.current = true;
      void refreshAccount({ silent: true });
    }, [debugConsoleOpen, debugEndpointLastRefresh]);

  useEffect(() => {
      if (!activeCallLog) return;
      let cancelled = false;
      let timer: number | undefined;
      const source = activeCallLog.source;
      let nextFullRefreshAt = 0;
      const refreshLiveCallLog = async () => {
        if (document.visibilityState !== "visible") {
          if (!cancelled) timer = window.setTimeout(refreshLiveCallLog, 2500);
          return;
        }
        const current = activeCallLogRef.current;
        const fullRefresh = Date.now() >= nextFullRefreshAt;
        const records = await refreshCallRecords(fullRefresh
          ? { silent: true, source }
          : {
            silent: true,
            source,
            afterMs: current?.source === source ? current.cursorMs : 0,
            incremental: true,
          });
        if (!cancelled) {
          if (fullRefresh) nextFullRefreshAt = Date.now() + UI_SNAPSHOT_AUTO_REFRESH_MS;
          updateActiveCallLog(source, records);
          timer = window.setTimeout(refreshLiveCallLog, 2500);
        }
      };
      void refreshLiveCallLog();
      return () => {
        cancelled = true;
        if (timer !== undefined) window.clearTimeout(timer);
      };
    }, [activeCallLog?.source]);

  useEffect(() => {
      setRoutePlanState({ status: "idle" });
    }, [debugTargetType, effectiveDebugModel, debugInboundProtocol]);

  useEffect(() => {
      if (!hasChanges) return;
      const handleBeforeUnload = (event: BeforeUnloadEvent) => {
        event.preventDefault();
        event.returnValue = "";
      };
      window.addEventListener("beforeunload", handleBeforeUnload);
      return () => window.removeEventListener("beforeunload", handleBeforeUnload);
    }, [hasChanges]);

  async function refreshModelCompatibilityPolicy(options: RefreshOptions = { throttle: true }) {
      if (modelCompatibilityRefreshInFlightRef.current) {
        if (!options.force) return modelCompatibilityRefreshInFlightRef.current;
        await modelCompatibilityRefreshInFlightRef.current;
      }
      const refreshGate = modelCompatibilityRefreshGateRef.current;
      if (!refreshGate.begin(options)) return;

      const request = (async () => {
        setModelCompatibilityPolicy((current) => ({ ...current, status: "loading", error: undefined }));
        try {
          const payload = await invoke<any>("fetch_model_compatibility");
          const next = modelCompatibilityStateFromPayload(payload);
          setModelCompatibilityPolicy(next);
          setModelCompatibilityDraft(next.modelGroups.map((group) => ({
            ...group,
            aliases: [...(group.aliases ?? [])],
            models: [...group.models],
          })));
        } catch (error) {
          refreshGate.reset();
          setModelCompatibilityPolicy((current) => ({ ...current, status: "error", error: String(error) }));
        }
      })();
      modelCompatibilityRefreshInFlightRef.current = request;
      try {
        await request;
      } finally {
        if (modelCompatibilityRefreshInFlightRef.current === request) {
          modelCompatibilityRefreshInFlightRef.current = null;
        }
      }
    }

  async function saveModelCompatibilityPolicy() {
      if (modelCompatibilityPolicy.status !== "ready" || modelCompatibilitySaving) return;
      setModelCompatibilitySaving(true);
      try {
        const payload = await invoke<any>("save_model_compatibility", {
          input: {
            base_release_id: modelCompatibilityPolicy.baseReleaseId ?? "",
            revision: modelCompatibilityPolicy.revision ?? 0,
            model_groups: modelCompatibilityDraft,
            template_model_groups: modelCompatibilityPolicy.templateModelGroups,
            catalog_models: modelCompatibilityPolicy.catalogModels,
          },
        });
        const next = modelCompatibilityStateFromPayload(payload);
        setModelCompatibilityPolicy(next);
        setModelCompatibilityDraft(next.modelGroups.map((group) => ({
          ...group,
          aliases: [...(group.aliases ?? [])],
          models: [...group.models],
        })));
        if (next.syncStatus === "synced") {
          showToast(tr("modelCompatibility.messages.savedAndSynced"), "success");
        } else if (next.syncStatus === "conflict") {
          showToast(tr("modelCompatibility.messages.savedWithConflict"), "error", 6200);
        } else if (next.platformSyncAvailable === false) {
          showToast(tr("modelCompatibility.messages.savedLocally"), "info", 6200);
        } else {
          showToast(tr("modelCompatibility.messages.savedPendingNetwork"), "info", 6200);
        }
      } catch (error) {
        showToast(tr("modelCompatibility.messages.saveFailed", { error: localizedError(error) }), "error", 6200);
      } finally {
        setModelCompatibilitySaving(false);
      }
    }

  async function resetModelCompatibilityPolicy() {
      if (modelCompatibilityPolicy.status !== "ready" || modelCompatibilitySaving) return;
      setModelCompatibilitySaving(true);
      try {
        const payload = await invoke<any>("reset_model_compatibility", {
          input: {
            base_release_id: modelCompatibilityPolicy.baseReleaseId ?? "",
            revision: modelCompatibilityPolicy.revision ?? 0,
            template_model_groups: modelCompatibilityPolicy.templateModelGroups,
            catalog_models: modelCompatibilityPolicy.catalogModels,
          },
        });
        const next = modelCompatibilityStateFromPayload(payload);
        setModelCompatibilityPolicy(next);
        setModelCompatibilityDraft(next.modelGroups.map((group) => ({
          ...group,
          aliases: [...(group.aliases ?? [])],
          models: [...group.models],
        })));
        if (next.platformSyncAvailable === false) {
          showToast(tr("modelCompatibility.messages.resetLocal"), "success");
        } else if (next.syncStatus === "synced") {
          showToast(tr("modelCompatibility.messages.resetServer"), "success");
        } else if (next.syncStatus === "conflict") {
          showToast(tr("modelCompatibility.messages.resetConflict"), "error", 6200);
        } else {
          showToast(tr("modelCompatibility.messages.resetPendingNetwork"), "info", 6200);
        }
      } catch (error) {
        showToast(tr("modelCompatibility.messages.resetFailed", { error: localizedError(error) }), "error", 6200);
      } finally {
        setModelCompatibilitySaving(false);
      }
    }

  function recordAppLog(text: string, tone: Toast["tone"] = "info") {
      const entry: AppLogEntry = {
        id: Date.now() + Math.floor(Math.random() * 1000),
        time: new Date().toLocaleTimeString(),
        text,
        tone,
      };
      setAppLogs((current) => [entry, ...current].slice(0, 120));
    }

  function showToast(
      text: string,
      tone: Toast["tone"] = "info",
      duration = DEFAULT_TOAST_DURATION_MS,
      recordLog = true,
    ) {
      if (toastTimer.current) {
        window.clearTimeout(toastTimer.current);
      }
      if (recordLog) recordAppLog(text, tone);
      setToast({ text, tone });
      const visibleDuration = tone === "error" ? Math.max(duration, ERROR_TOAST_MIN_DURATION_MS) : duration;
      toastTimer.current = window.setTimeout(() => {
        setToast(null);
        toastTimer.current = null;
      }, visibleDuration);
    }

  function dismissToast() {
      if (toastTimer.current) {
        window.clearTimeout(toastTimer.current);
        toastTimer.current = null;
      }
      setToast(null);
    }

  function showGuidanceNotice(notice: GuidanceNotice, recordLog = true) {
      const alreadyVisible = guidanceNoticesRef.current.some((item) => (
        item.dedupeKey === notice.dedupeKey
      ));
      const next = upsertGuidanceNotice(guidanceNoticesRef.current, notice);
      guidanceNoticesRef.current = next;
      setGuidanceNotices(next);
      if (recordLog && !alreadyVisible) {
        const logTone = notice.tone === "error" || notice.tone === "success"
          ? notice.tone
          : "info";
        recordAppLog(notice.body, logTone);
      }
    }

  function dismissGuidanceNoticeByKey(dedupeKey: string) {
      const next = dismissGuidanceNotice(guidanceNoticesRef.current, dedupeKey);
      guidanceNoticesRef.current = next;
      setGuidanceNotices(next);
    }

  function logText() {
      return appLogs
        .map((entry) => `[${entry.time}] ${entry.tone.toUpperCase()} ${entry.text}`)
        .join("\n");
    }

  function handleDiagnosticAction(issue: DiagnosticIssue) {
      switch (issue.action) {
        case "check_update":
          void checkAndDownloadUpdate(false);
          return;
        case "refresh_registry":
          void refreshEndpointRegistry();
          return;
        case "start_proxy":
          void start();
          return;
        case "open_access":
          setTab("access");
          return;
        case "open_supplier":
          setTab("supplier");
          return;
        case "open_logs":
          setLogOpen(true);
          return;
        default:
          return;
      }
    }

  async function initialize() {
      const startedAt = performance.now();
      startupConsoleInfo("[const-api][startup][ui] initialize begin");
      if (!isTauriRuntime()) {
        startupConsoleInfo("[const-api][startup][ui] browser runtime, skip initialize");
        return;
      }
      const result = await reload();
      startupConsoleInfo("[const-api][startup][ui] reload finished in %dms", Math.round(performance.now() - startedAt));
      if (!result) {
        // A failed initial read must not disable status monitoring forever.
        localEntryStartupCompleteRef.current = true;
        return;
      }
      void refreshReleaseSourceStatus();
      void ensureAutoStartup(result.cfg, result.st, result.supplierSt);
      void account.actions.initializeStatus();
      if (!DEVELOPMENT_PROFILE) {
        void refreshAutostartStatus();
      }
      void refreshCallRecords({ silent: true });
    }

  async function refreshReleaseSourceStatus() {
      if (!isTauriRuntime()) return;
      try {
        const status = await invoke<ReleaseSourceStatus>("get_release_source_status");
        setAppMeta((current) => ({
          ...current,
          updateSources: status.updateSources,
          endpointSources: status.endpointSources,
        }));
      } catch (error) {
        recordAppLog(`release sources: ${String(error)}`, "error");
      }
    }

  async function refreshAutostartStatus() {
      if (DEVELOPMENT_PROFILE) {
        setAutostartEnabled(false);
        return;
      }
      if (!isTauriRuntime()) return;
      try {
        setAutostartEnabled(await invoke<boolean>("autostart_status"));
      } catch (error) {
        recordAppLog(tr("about.autostartReadFailed", { error: localizedError(error) }), "error");
      }
    }

  async function refreshModelCatalogVersionInfo() {
      if (!isTauriRuntime()) return;
      try {
        const info = await invoke<ModelCatalogVersionInfo>("get_model_catalog_version_info");
        if (
          !info
          || !info.release_id?.trim()
          || !info.model_version_id?.trim()
          || !info.compatibility_version_id?.trim()
        ) {
          throw new Error(tr("about.catalogInfoIncomplete"));
        }
        setAppMeta((current) => ({
          ...current,
          modelCatalog: info,
          modelCatalogLastError: undefined,
        }));
      } catch (error) {
        setAppMeta((current) => ({
          ...current,
          modelCatalogLastError: String(error),
        }));
      }
    }

  async function changeAutostart(enabled: boolean) {
      if (DEVELOPMENT_PROFILE) return;
      if (!isTauriRuntime() || autostartBusy) return;
      setAutostartBusy(true);
      try {
        const actual = await invoke<boolean>("set_autostart", { enabled });
        setAutostartEnabled(actual);
        showToast(actual ? tr("about.autostartEnabled") : tr("about.autostartDisabled"), "success");
      } catch (error) {
        await refreshAutostartStatus();
        showToast(tr("about.autostartFailed", { error: localizedError(error) }), "error", 5200);
      } finally {
        setAutostartBusy(false);
      }
    }

  async function changeDevelopmentEndpoint(endpointId: string) {
      if (!import.meta.env.DEV || developmentEndpointBusy || !isTauriRuntime()) return;
      setDevelopmentEndpointBusy(true);
      try {
        const wire = await invoke<ClientConfigV5Wire>("set_development_endpoint", { endpointId });
        const authoritative = hydrateClientConfigV5(wire);
        const firstEndpoint = authoritative.endpoints.find(
          (endpoint) => endpoint.enabled && endpoint.base_url.trim(),
        );
        const serverWsUrl = firstEndpoint ? supplierWsFromEndpoint(firstEndpoint) : "";
        const serverQuicUrl = firstEndpoint ? supplierQuicFromEndpoint(firstEndpoint) : "";
        const mergeSelection = (current: ClientConfig) => ({
          ...mergeEndpointDiscoveryConfig(current, {
            platform_id: authoritative.platform_id,
            version: authoritative.registry_version,
            endpoints: authoritative.endpoints,
            serverWsUrl,
            serverQuicUrl,
          }),
          development_endpoint: authoritative.development_endpoint,
        });
        const nextConfig = mergeSelection(configRef.current);
        const nextSavedConfig = mergeSelection(savedConfigRef.current);
        configRef.current = nextConfig;
        savedConfigRef.current = nextSavedConfig;
        setConfig(nextConfig);
        setSavedConfig(nextSavedConfig);
        setStatus((current) => ({
          ...current,
          active_endpoint: firstEndpoint?.name ?? null,
        }));

        const supplierWasRunning = supplierStatusRef.current.running
          || supplierStatusRef.current.starting;
        let supplierRestartError = "";
        if (supplierWasRunning) {
          try {
            const stopped = await invoke<SupplierStatus>("stop_supplier");
            supplierStatusRef.current = stopped;
            setSupplierStatus(stopped);
            const restarted = await invoke<SupplierStatus>("start_supplier", {
              channelId: null,
              channelIds: null,
            });
            supplierStatusRef.current = restarted;
            setSupplierStatus(restarted);
          } catch (error) {
            supplierRestartError = localizedError(error);
            try {
              const latest = await invoke<SupplierStatus>("supplier_status");
              supplierStatusRef.current = latest;
              setSupplierStatus(latest);
            } catch {
              // Keep the last known state; the regular status poll will reconcile it.
            }
          }
        } else {
          setSupplierStatus((current) => ({
            ...current,
            server_ws_url: serverWsUrl,
            server_quic_url: serverQuicUrl,
          }));
        }

        void refreshAccount({ silent: true });
        const selectedEndpoint = firstEndpoint?.name ?? authoritative.development_endpoint;
        showToast(
          supplierRestartError
            ? tr("about.developmentEndpointChangedRestartFailed", {
              endpoint: selectedEndpoint,
              error: supplierRestartError,
            })
            : tr("about.developmentEndpointChanged", { endpoint: selectedEndpoint }),
          supplierRestartError ? "error" : "success",
          supplierRestartError ? 6200 : undefined,
        );
      } catch (error) {
        showToast(tr("about.developmentEndpointFailed", {
          error: localizedError(error),
        }), "error", 5200);
      } finally {
        setDevelopmentEndpointBusy(false);
      }
    }

  async function reload() {
      const startedAt = performance.now();
      try {
        const [cfg, st, supplierSt] = await Promise.all([
          invoke<ClientConfigV5Wire>("get_config"),
          invoke<ProxyStatus>("proxy_status"),
          invoke<SupplierStatus>("supplier_status"),
        ]);
        startupConsoleInfo(
          "[const-api][startup][ui] config and runtime snapshot finished in %dms",
          Math.round(performance.now() - startedAt),
        );
        const normalizedCfg = hydrateClientConfigV5(cfg);
        setConfig(normalizedCfg);
        setSavedConfig(normalizedCfg);
        setStatus(st);
        setSupplierStatus(supplierSt);
        setAppMeta((current) => ({
          ...current,
          endpointSources: normalizedCfg.registry_sources,
        }));
        resolveDiagnostic("config-load");
        startupConsoleInfo("[const-api][startup][ui] reload complete in %dms", Math.round(performance.now() - startedAt));
        return { cfg: normalizedCfg, st, supplierSt };
      } catch (error) {
        pushDiagnostic({
          id: "config-load",
          scope: "about",
          severity: "error",
          title: tr("diagnostics.configLoadFailed"),
          message: localizedError(error),
          action: "open_logs",
        });
        showToast(localizedError(error), "error", 4200);
        return null;
      }
    }

  async function ensureAutoStartup(cfg: ClientConfig, st: ProxyStatus, supplierSt: SupplierStatus) {
      const startedAt = performance.now();
      startupConsoleInfo("[const-api][startup][ui] ensureAutoStartup begin");
      if (cfg.proxy_auto_start && !st.running) {
        try {
          const proxyStartedAt = performance.now();
          const nextStatus = await invoke<ProxyStatus>("start_proxy");
          startupConsoleInfo("[const-api][startup][ui] start_proxy finished in %dms", Math.round(performance.now() - proxyStartedAt));
          setStatus(nextStatus);
          resolveDiagnostic("local-entry");
        } catch (error) {
          pushDiagnostic({
            id: "local-entry",
            scope: "access",
            severity: "error",
            title: tr("diagnostics.localEntryStartFailed"),
            message: localizedError(error),
            action: "start_proxy",
          });
          showToast(tr("access.messages.autoStartFailed", { error: localizedError(error) }), "error", 5200);
        }
      }
      localEntryStartupCompleteRef.current = true;
      proxyStatusEpochRef.current += 1;
      setLocalEntryDiagnosticReady(true);
      if (cfg.supplier_auto_start && !supplierSt.running) {
        try {
          const supplierStartedAt = performance.now();
          setPendingSupplierChannelIds(sharedSupplierChannelIds(cfg));
          setSupplierStatus({
            ...supplierSt,
            starting: true,
            active_transport: "starting",
            last_transport_error: "",
          });
          const nextSupplierStatus = await invoke<SupplierStatus>("start_supplier", { channelId: null, channelIds: null });
          startupConsoleInfo("[const-api][startup][ui] start_supplier finished in %dms", Math.round(performance.now() - supplierStartedAt));
          setSupplierStatus(nextSupplierStatus);
          resolveDiagnostic("supplier-auto");
        } catch (error) {
          setSupplierStatus({ ...supplierSt, starting: false });
          pushDiagnostic({
            id: "supplier-auto",
            scope: "supplier",
            severity: "error",
            title: tr("diagnostics.supplierAutoRestoreFailed"),
            message: localizedError(error),
            action: "open_supplier",
          });
          showToast(tr("access.messages.supplierAutoRestoreFailed", { error: localizedError(error) }), "error", 5200);
        } finally {
          setPendingSupplierChannelIds([]);
        }
      }
      void refreshCallRecords({ silent: true });
      startupConsoleInfo("[const-api][startup][ui] ensureAutoStartup complete in %dms", Math.round(performance.now() - startedAt));
    }

  async function runConfigAction(action: () => Promise<void>) {
      if (configActionInFlightRef.current) return;
      configActionInFlightRef.current = true;
      proxyStatusEpochRef.current += 1;
      setConfigActionBusy(true);
      try {
        await action();
      } finally {
        proxyStatusEpochRef.current += 1;
        configActionInFlightRef.current = false;
        setConfigActionBusy(false);
      }
    }

  async function saveAccessConfig() {
      // Access saves own only the access fields. Keep the current supplier
      // draft byte-for-byte so mechanical channel normalization cannot look
      // like a user edit and mask a concurrent detection refresh in the UI.
      const draft = configRef.current;
      const baseline = savedConfigRef.current;
      const wire = await invoke<ClientConfigV5Wire>("save_config", {
        config: {
          listen: draft.listen,
          allow_lan_access: draft.allow_lan_access,
          proxy_auto_start: draft.proxy_auto_start,
          api_key: draft.api_key,
          allow_model_equivalence: draft.allow_model_equivalence,
          prefer_local_supply: draft.prefer_local_supply,
        },
      });
      const authoritative = hydrateClientConfigV5(wire);
      const nextDraft = overlaySupplierConfig(authoritative, draft, baseline);
      configRef.current = nextDraft;
      savedConfigRef.current = authoritative;
      setConfig(nextDraft);
      setSavedConfig(authoritative);
      if (supplierStatusRef.current.running) {
        try {
          const st = await invoke<SupplierStatus>("refresh_supplier_registration");
          supplierStatusRef.current = st;
          setSupplierStatus(st);
          showToast(tr("access.messages.savedAndSynchronized"), "success");
        } catch (error) {
          showToast(tr("access.messages.savedSynchronizationFailed", { error: localizedError(error) }), "error", 5200);
        }
        return;
      }
      showToast(tr("access.messages.saved"), "success");
    }

  async function save() {
      await runConfigAction(async () => {
        try {
          await saveAccessConfig();
        } catch (error) {
          showToast(tr("access.messages.saveFailed", { error: localizedError(error) }), "error", 5200);
        }
      });
    }

  async function start() {
      await runConfigAction(async () => {
        try {
          await saveAccessConfig();
        } catch (error) {
          showToast(tr("access.messages.saveFailed", { error: localizedError(error) }), "error", 5200);
          return;
        }
        try {
          const st = await invoke<ProxyStatus>("start_proxy");
          setStatus(st);
          resolveDiagnostic("local-entry");
          showToast(tr("access.messages.started"), "success");
        } catch (error) {
          pushDiagnostic({
            id: "local-entry",
            scope: "access",
            severity: "error",
            title: tr("diagnostics.localEntryStartFailed"),
            message: localizedError(error),
            action: "start_proxy",
          });
          showToast(tr("access.messages.startFailed", { error: localizedError(error) }), "error", 5200);
        }
      });
    }

  async function stop() {
      await runConfigAction(async () => {
        try {
          const st = await invoke<ProxyStatus>("stop_proxy");
          setStatus(st);
          showToast(tr("access.messages.stopped"), "info");
        } catch (error) {
          showToast(localizedError(error), "error", 5200);
        }
      });
    }

  async function restart() {
      await runConfigAction(async () => {
        try {
          // Save first: a failed disk commit must never stop a healthy proxy.
          await saveAccessConfig();
        } catch (error) {
          showToast(tr("access.messages.saveFailed", { error: localizedError(error) }), "error", 5200);
          return;
        }
        try {
          const stopped = await invoke<ProxyStatus>("stop_proxy");
          setStatus(stopped);
          const st = await invoke<ProxyStatus>("start_proxy");
          setStatus(st);
          resolveDiagnostic("local-entry");
          showToast(tr("access.messages.started"), "success");
        } catch (error) {
          showToast(tr("access.messages.startFailed", { error: localizedError(error) }), "error", 5200);
        }
      });
    }

  async function cancelChanges() {
      await runConfigAction(async () => {
        try {
          const wire = await invoke<ClientConfigV5Wire>("get_config");
          const authoritative = hydrateClientConfigV5(wire);
          const cancelled = cancelSupplierEditor(selectedChannel.id, authoritative);
          configRef.current = cancelled.config;
          savedConfigRef.current = authoritative;
          setConfig(cancelled.config);
          setSavedConfig(authoritative);
          setSelectedChannelId(cancelled.selectedChannelId);
          if (cancelled.closeDetail) {
            setSupplierDetailOpen(false);
          }
          showToast(tr("access.messages.changesCanceled"), "info");
        } catch (error) {
          showToast(tr("access.messages.reloadFailed", { error: localizedError(error) }), "error", 5200);
        }
      });
    }

  async function changeLanAccess(enabled: boolean) {
      const nextListen = localListenAddressForLanAccess(config.listen, enabled);
      if (!nextListen) {
        showToast(tr("access.messages.invalidListenAddress"), "error");
        return;
      }
      if (enabled) {
        const confirmed = await askConfirm({
          title: tr("access.dialogs.allowLanTitle"),
          message: tr("access.dialogs.allowLanMessage"),
          confirmText: tr("access.dialogs.allowLanConfirm"),
        });
        if (!confirmed) return;
      }
      await runConfigAction(async () => {
        const previousDraft = configRef.current;
        const nextDraft = {
          ...previousDraft,
          allow_lan_access: enabled,
          listen: localListenAddressForLanAccess(previousDraft.listen, enabled) ?? nextListen,
        };
        configRef.current = nextDraft;
        setConfig(nextDraft);
        try {
          await saveAccessConfig();
        } catch (error) {
          configRef.current = previousDraft;
          setConfig(previousDraft);
          showToast(tr("access.messages.saveFailed", { error: localizedError(error) }), "error", 5200);
          return;
        }
        if (!status.running) return;
        try {
          const stopped = await invoke<ProxyStatus>("stop_proxy");
          setStatus(stopped);
          const started = await invoke<ProxyStatus>("start_proxy");
          setStatus(started);
          showToast(tr("lanShare.listenerUpdated"), "success");
        } catch (error) {
          showToast(tr("access.messages.startFailed", { error: localizedError(error) }), "error", 5200);
        }
      });
    }

  function confirmDiscardSelectedChanges() {
      if (!selectedChannelHasChanges) return true;
      return askConfirm({
        title: tr("access.dialogs.discardTitle"),
        message: tr("access.dialogs.discardMessage"),
        confirmText: tr("access.dialogs.discardConfirm"),
        tone: "danger",
      });
    }

  async function closeSupplierDetail() {
      if (!(await confirmDiscardSelectedChanges())) return;
      if (selectedChannelHasChanges) {
        setConfig(savedConfig);
      }
      setSupplierDetailOpen(false);
    }

  async function openSupplierChannel(id: string) {
      if (id !== selectedChannel.id && !(await confirmDiscardSelectedChanges())) return;
      if (id !== selectedChannel.id && selectedChannelHasChanges) {
        setConfig(savedConfig);
      }
      setSelectedChannelId(id);
      setSupplierDetailOpen(true);
    }

  function openCallLog(
    source: CallLogSource,
    title: string,
    subtitle: string,
    records: CallRecord[],
    stats: UsageStat[],
    refreshError = "",
  ) {
      const nextRecords = records ?? [];
      setActiveCallLog({
        source,
        title,
        subtitle,
        records: nextRecords,
        stats: stats ?? [],
        cursorMs: callRecordsCursorMs(nextRecords),
        refreshedAt: Date.now(),
        refreshError: refreshError || undefined,
        history: source === "platform" ? platformCallSyncRef.current.history : undefined,
        hasMore: source === "platform" ? platformCallSyncRef.current.hasMore : undefined,
      });
      setSelectedCallRecord(nextRecords[0] ?? null);
    }

  function updateActiveCallLog(source: CallLogSource, snapshot: Awaited<ReturnType<typeof refreshCallRecords>>) {
      if (source === "platform" && snapshot.platformGeneration !== platformCallGenerationRef.current) return;
      const incomingRecords = source === "platform"
        ? (snapshot.platformRecords.length ? snapshot.platformRecords : snapshot.usageRecords)
        : snapshot.localRecords;
      const incomingStats = source === "platform" ? snapshot.platformStats : snapshot.localStats;
      const refreshError = source === "platform" ? snapshot.platformError : snapshot.localError;
      let nextSelected: CallRecord | null = null;
      let didUpdate = false;
      setActiveCallLog((current) => {
        if (!current || current.source !== source) return current;
        const nextRecords = snapshot.incremental
          ? mergeCallRecords(current.records, incomingRecords).slice(0, 1000)
          : incomingRecords;
        // Incremental records are a recent page, not a replacement for the
        // cumulative summary. Keep the last full summary until its next refresh.
        const nextStats = callLogStatsForRefresh(current.stats, incomingStats, snapshot.incremental);
        const selectedId = selectedCallRecordRef.current ? callRecordID(selectedCallRecordRef.current) : "";
        nextSelected = selectedId
          ? nextRecords.find((record) => callRecordID(record) === selectedId) ?? nextRecords[0] ?? null
          : nextRecords[0] ?? null;
        didUpdate = true;
        return {
          ...current,
          records: nextRecords,
          stats: nextStats,
          cursorMs: callLogCursorForRefresh(current.cursorMs,
            snapshot.incremental ? snapshotCursorMs(snapshot, source) : callRecordsCursorMs(nextRecords), snapshot.incremental),
          refreshedAt: Date.now(),
          refreshError: refreshError || undefined,
          history: source === "platform" ? snapshot.platformSync.history : undefined,
          hasMore: nextRecords.length >= 1000 || (source === "platform" ? snapshot.platformSync.hasMore : undefined),
        };
      });
      if (didUpdate) {
        setSelectedCallRecord(nextSelected);
      }
    }

  async function refreshCallRecords(options: { silent?: boolean; source?: CallLogSource; afterMs?: number; incremental?: boolean } = {}) {
      const incremental = Boolean(options.incremental && options.source);
      const platformGeneration = platformCallGenerationRef.current;
      const next = {
        usageRecords: usage,
        platformRecords: callRecords,
        platformStats: usageStats,
        localRecords: localChannelCalls,
        localStats: localChannelStats,
        platformCursorMs: callRecordsCursorMs(callRecords.length ? callRecords : usage),
        localCursorMs: callRecordsCursorMs(localChannelCalls),
        platformError: "",
        localError: "",
        platformSync: platformCallSyncRef.current,
        platformGeneration,
        incremental,
      };
      const wantsPlatform = !options.source || options.source === "platform";
      const wantsLocal = !options.source || options.source === "local_supplier";
      const platformUnavailable = wantsPlatform && accountStateRef.current !== "signed_in";
      if (platformUnavailable) {
        next.platformError = callLogRefreshErrorText(
          "platform",
          undefined,
          accountStateRef.current,
        );
      }
      if (!isTauriRuntime()) return next;

      if (incremental) {
        if (options.source === "platform" && accountStateRef.current !== "signed_in") {
          next.platformError = callLogRefreshErrorText(
            "platform",
            undefined,
            accountStateRef.current,
          );
          return next;
        }
        try {
          if (options.source === "platform") {
            const result = await invoke<any>("fetch_calls", {
              afterMs: options.afterMs ?? 0, cursor: platformCallSyncRef.current.cursor, limit: 200,
            });
            if (platformGeneration !== platformCallGenerationRef.current) return next;
            const incoming = result.data ?? [];
            next.incremental = callSyncIsIncremental(result, true);
            // has_more on an incremental page means a pending poll backlog, not
            // necessarily older history. Keep the last full-page boundary hint.
            next.platformSync = { ...callSyncState(result), hasMore: next.incremental
              ? platformCallSyncRef.current.hasMore : result.has_more };
            platformCallSyncRef.current = next.platformSync;
            next.platformRecords = incoming;
            next.platformCursorMs = Number(result.cursor_ms ?? result.cursorMs ?? callRecordsCursorMs(incoming) ?? 0);
            if (incoming.length > 0 || !next.incremental) {
              setCallRecords((current) => next.incremental ? mergeCallRecords(current, incoming).slice(0, 200) : incoming);
            }
            if (result.resync_required === true) {
              const stats = await invoke<any>("fetch_usage_stats");
              if (platformGeneration !== platformCallGenerationRef.current) return next;
              next.platformStats = stats.data ?? [];
              setUsageStats(next.platformStats);
            }
          } else if (options.source === "local_supplier") {
            const result = await invoke<any>("fetch_local_channel_calls", { limit: 100, afterMs: options.afterMs ?? 0 });
            const incoming = result.data ?? [];
            next.localRecords = incoming;
            next.localCursorMs = Number(result.cursor_ms ?? result.cursorMs ?? callRecordsCursorMs(incoming) ?? 0);
            if (incoming.length > 0) {
              setLocalChannelCalls((current) => mergeCallRecords(current, incoming).slice(0, 200));
            }
          }
        } catch (err) {
          if (options.source === "platform" && platformGeneration !== platformCallGenerationRef.current) return next;
          if (options.source === "platform") {
            next.platformError = callLogRefreshErrorText(
              "platform",
              err,
              accountStateRef.current,
            );
          } else if (options.source === "local_supplier") {
            next.localError = callLogRefreshErrorText(
              "local_supplier",
              err,
              accountStateRef.current,
            );
          }
          if (!options.silent) {
            showToast(tr("callLogs.partialRefreshFailed", { error: localizedError(err) }), "error", 4200);
          }
        }
        return next;
      }

      const usagePromise = !options.source && !platformUnavailable ? invoke<any>("fetch_usage") : Promise.resolve({ data: next.usageRecords });
      const callsPromise = wantsPlatform && !platformUnavailable ? invoke<any>("fetch_calls", { limit: 200 }) : Promise.resolve({ data: next.platformRecords, cursor_ms: next.platformCursorMs });
      const statsPromise = wantsPlatform && !platformUnavailable ? invoke<any>("fetch_usage_stats") : Promise.resolve({ data: next.platformStats });
      const localCallsPromise = wantsLocal ? invoke<any>("fetch_local_channel_calls", { limit: 200 }) : Promise.resolve({ data: next.localRecords, cursor_ms: next.localCursorMs });
      const localStatsPromise = wantsLocal ? invoke<any>("fetch_local_channel_stats") : Promise.resolve({ data: next.localStats });
      const [usageResult, callsResult, statsResult, localCallsResult, localStatsResult] = await Promise.allSettled([
        usagePromise,
        callsPromise,
        statsPromise,
        localCallsPromise,
        localStatsPromise,
      ]);
      // A response issued before login/logout must not restore the previous
      // user's records or cursor after the account caches have been cleared.
      if (platformGeneration !== platformCallGenerationRef.current) return next;

      if (wantsPlatform && !platformUnavailable) {
        if (!options.source && usageResult.status === "fulfilled") {
          next.usageRecords = usageResult.value.data ?? [];
          setUsage(next.usageRecords);
        }
        if (callsResult.status === "fulfilled") {
          next.platformSync = callSyncState(callsResult.value);
          platformCallSyncRef.current = next.platformSync;
          next.platformRecords = callsResult.value.data ?? next.usageRecords ?? [];
          next.platformCursorMs = Number(callsResult.value.cursor_ms ?? callsResult.value.cursorMs ?? callRecordsCursorMs(next.platformRecords));
          setCallRecords(next.platformRecords);
        } else if (!options.source && usageResult.status === "fulfilled") {
          next.platformRecords = next.usageRecords;
          next.platformCursorMs = callRecordsCursorMs(next.platformRecords);
          setCallRecords(next.platformRecords);
        }
        if (statsResult.status === "fulfilled") {
          next.platformStats = statsResult.value.data ?? [];
          setUsageStats(next.platformStats);
        }
      }
      if (wantsLocal && localCallsResult.status === "fulfilled") {
        next.localRecords = localCallsResult.value.data ?? [];
        next.localCursorMs = Number(localCallsResult.value.cursor_ms ?? localCallsResult.value.cursorMs ?? callRecordsCursorMs(next.localRecords));
        setLocalChannelCalls(next.localRecords);
      }
      if (wantsLocal && localStatsResult.status === "fulfilled") {
        next.localStats = localStatsResult.value.data ?? [];
        setLocalChannelStats(next.localStats);
      }

      const platformFailure = [usageResult, callsResult, statsResult]
        .find((result): result is PromiseRejectedResult => result.status === "rejected");
      if (wantsPlatform && platformFailure) {
        next.platformError = callLogRefreshErrorText(
          "platform",
          platformFailure.reason,
          accountStateRef.current,
        );
      }
      const localFailure = [localCallsResult, localStatsResult]
        .find((result): result is PromiseRejectedResult => result.status === "rejected");
      if (wantsLocal && localFailure) {
        next.localError = callLogRefreshErrorText(
          "local_supplier",
          localFailure.reason,
          accountStateRef.current,
        );
      }

      const failures = [usageResult, callsResult, statsResult, localCallsResult, localStatsResult]
        .filter((result): result is PromiseRejectedResult => result.status === "rejected")
        .map((result) => String(result.reason));
      if (failures.length > 0 && !options.silent) {
        showToast(tr("callLogs.partialRefreshFailed", { error: failures[0] }), "error", 4200);
      }
      return next;
    }

  function openPlatformCallLog() {
      const refreshError = account.status.state === "signed_in"
        ? ""
        : callLogRefreshErrorText("platform", undefined, account.status.state);
      openCallLog(
        "platform",
        tr("callLogs.usageTitle"),
        tr("callLogs.usageSubtitle"),
        callRecords.length ? callRecords : usage,
        usageStats,
        refreshError,
      );
    }

  function openLocalSupplierCallLog() {
      openCallLog(
        "local_supplier",
        tr("callLogs.supplyTitle"),
        tr("callLogs.supplySubtitle"),
        localChannelCalls,
        localChannelStats,
      );
    }

  function closeCallLog() {
      setActiveCallLog(null);
      setSelectedCallRecord(null);
    }

  async function refreshAccount(options: { silent?: boolean } = {}) {
      if (debugEndpointRefreshInFlightRef.current) return;
      debugEndpointRefreshInFlightRef.current = true;
      setDebugEndpointRefreshing(true);
      setDebugEndpointError("");
      try {
        const providerResp = await invoke<any>("fetch_providers");
        const supplierResp = await invoke<any>("fetch_supplier_nodes");
        setProviders(providerResp.data ?? []);
        setPlatformSupplierNodes(supplierResp.data ?? []);
        setDebugEndpointLastRefresh(Date.now());
        if (!options.silent) showToast(tr("common.refreshed"), "success");
      } catch (error) {
        const message = localizedError(error);
        setDebugEndpointError(message);
        if (!options.silent) showToast(message, "error", 4200);
      } finally {
        debugEndpointRefreshInFlightRef.current = false;
        setDebugEndpointRefreshing(false);
      }
    }

  async function refreshEndpointRegistry(silent = false) {
      if (endpointRegistryRefreshInFlightRef.current) return;
      endpointRegistryRefreshInFlightRef.current = true;
      const currentConfig = configRef.current;
      const checkedAt = new Date().toLocaleString();
      setAppMeta((current) => ({
        ...current,
        endpointSources: currentConfig.registry_sources,
        endpointStatus: "checking",
        endpointLastError: undefined,
      }));
      try {
        const result = await invoke<EndpointDiscoveryResult>("refresh_endpoint_registry");
        const firstEndpoint = result.endpoints.find((endpoint) => endpoint.enabled && endpoint.base_url.trim());
        const serverWsUrl = firstEndpoint ? supplierWsFromEndpoint(firstEndpoint) : "";
        const serverQuicUrl = firstEndpoint ? supplierQuicFromEndpoint(firstEndpoint) : "";
        const mergeDiscovery = (current: ClientConfig) => mergeEndpointDiscoveryConfig(current, {
          platform_id: result.platform_id,
          version: result.version,
          endpoints: result.endpoints,
          serverWsUrl,
          serverQuicUrl,
        });
        setConfig(mergeDiscovery);
        setSavedConfig(mergeDiscovery);
        if (result.refresh_warning && result.refresh_warning !== endpointRefreshWarningRef.current) {
          recordAppLog(tr("about.endpointRefreshWarning", {
            source: compactSourceLabel(result.source),
            error: result.refresh_warning,
          }), "info");
        }
        endpointRefreshWarningRef.current = result.refresh_warning;
        setAppMeta((current) => ({
          ...current,
          endpointSources: currentConfig.registry_sources,
          endpointStatus: "success",
          endpointSource: result.source,
          endpointVersion: result.version,
          endpointCount: result.endpoints.length,
          endpointLastCheckedAt: checkedAt,
          endpointLastError: undefined,
          endpointRefreshWarning: result.refresh_warning,
        }));
        resolveDiagnostic("endpoint-registry");
        if (!silent) {
          showToast(tr("about.endpointsDiscovered", { count: result.endpoints.length }), "success");
        }
      } catch (error) {
        setAppMeta((current) => ({
          ...current,
          endpointSources: currentConfig.registry_sources,
          endpointStatus: "error",
          endpointLastCheckedAt: checkedAt,
          endpointLastError: String(error),
        }));
        if (!silent) {
          pushDiagnostic({
            id: "endpoint-registry",
            scope: "about",
            severity: "error",
            title: tr("diagnostics.endpointSourceFailed"),
            message: localizedError(error),
            action: "refresh_registry",
          });
          showToast(localizedError(error), "error", 4200);
        }
      } finally {
        endpointRegistryRefreshInFlightRef.current = false;
      }
    }

  function isTauriRuntime() {
      return hasTauriRuntime();
    }

  function applyNativeUpdateStatus(status: NativeUpdateStatus) {
      const checkedAt = status.checkedAtUnixMs > 0
        ? new Date(status.checkedAtUnixMs).toLocaleString()
        : undefined;
      const error = status.error ? formatUpdateError(status.error) : undefined;
      const waitingReason = status.status === "ready_waiting_idle"
        ? updateInstallWaitingText(status.readiness)
        : undefined;
      setUpdateState({
        status: status.status,
        version: status.version || undefined,
        error,
        waitingReason,
        checkedAt,
        showDiagnostic: Boolean(error),
      });
      setAutoInstallUpdatesState(status.automatic);
      setAppMeta((current) => ({
        ...current,
        updateSources: status.sources.length > 0
          ? status.sources.map((source) => source.sourceUrl)
          : current.updateSources,
        updateSource: status.sourceUrl || status.preferredSourceUrl || current.updateSource,
        updateLastCheckedAt: checkedAt || current.updateLastCheckedAt,
        updateLastError: error,
      }));
      if (!error) {
        resolveDiagnostic("update-source");
      }
    }

  function setAutoInstallUpdates(enabled: boolean) {
      setAutoInstallUpdatesState(enabled);
      if (DEVELOPMENT_PROFILE || !isTauriRuntime()) return;
      void invoke<NativeUpdateStatus>("set_automatic_updates", { enabled })
        .then((status) => {
          clearLegacyAutoInstallUpdatesPreference();
          applyNativeUpdateStatus(status);
        })
        .catch((error) => {
          recordAppLog(`native updater preference: ${String(error)}`, "error");
          void invoke<NativeUpdateStatus>("native_update_status")
            .then(applyNativeUpdateStatus)
            .catch(() => undefined);
        });
    }

  async function checkAndDownloadUpdate(silent = false) {
      if (DEVELOPMENT_PROFILE) return;
      if (!isTauriRuntime()) {
        if (!silent) showToast(tr("updates.desktopOnly"), "error", 4200);
        return;
      }
      if (["checking", "downloading", "installing"].includes(updateState.status)) {
        if (!silent) showToast(tr("updates.alreadyRunning"), "info", 3200);
        return;
      }
      try {
        const status = await invoke<NativeUpdateStatus>("request_native_update_check");
        applyNativeUpdateStatus(status);
      } catch (error) {
        const message = formatUpdateError(error);
        setUpdateState((current) => ({
          ...current,
          status: "error",
          error: message,
          checkedAt: new Date().toLocaleString(),
          showDiagnostic: !silent,
        }));
        if (!silent) showToast(tr("updates.checkFailed", { error: message }), "error", 5200);
      }
    }

  async function installPreparedUpdate() {
      if (!isTauriRuntime()) return;
      if (updateState.status !== "ready") {
        showToast(tr("updates.notReady"), "error", 4200);
        return;
      }
      try {
        const status = await invoke<NativeUpdateStatus>("request_native_update_install");
        applyNativeUpdateStatus(status);
      } catch (error) {
        const message = formatUpdateError(error);
        setUpdateState((current) => ({
          ...current,
          status: "ready",
          error: message,
          showDiagnostic: true,
        }));
        showToast(tr("updates.installFailed", { error: message }), "error", 5200);
      }
    }

  function updateSupplier(patch: Partial<ChannelConfig>) {
      updateChannel(patch);
    }

  function updateSupplierSurfaceBinding(surface: ApiSurface, patch: Partial<ChannelSurfaceBinding>) {
      const existing = selectedSurfaceBindings.find((binding) => binding.surface === surface);
      const fallbackProtocol = surfaceProtocols(surface)[0];
      const nextBinding: ChannelSurfaceBinding = {
        surface,
        base_url: selectedChannel.upstream_base_url,
        endpoint_profile: surfaceEndpointProfile(selectedChannel.kind, surface, selectedChannel.subscription?.platform),
        auth_scheme: surfaceAuthScheme(selectedChannel.kind, surface),
        protocols: fallbackProtocol ? [{
          protocol: fallbackProtocol,
          preferred: true,
          verification: protocolVerification(selectedChannel, fallbackProtocol),
        }] : [],
        operation_overrides: [],
        verification: { state: "declared", checked_at_unix: 0, summary: "" },
        ...existing,
        ...patch,
      };
      applySupplierSurfaceBindings([
        ...selectedSurfaceBindings.filter((binding) => binding.surface !== surface),
        nextBinding,
      ]);
    }

  function toggleSupplierSurfaceProtocol(surface: ApiSurface, protocol: DebugProtocol, enabled: boolean) {
      const existing = selectedSurfaceBindings.find((binding) => binding.surface === surface);
      const currentProtocols = existing?.protocols ?? [];
      const nextProtocols = enabled
        ? [...currentProtocols, {
          protocol,
          preferred: currentProtocols.length === 0,
          verification: protocolVerification(selectedChannel, protocol),
        }].filter((item, index, values) => values.findIndex((candidate) => candidate.protocol === item.protocol) === index)
        : currentProtocols.filter((item) => item.protocol !== protocol);
      const totalProtocols = selectedSurfaceBindings.reduce((count, binding) => count + binding.protocols.length, 0);
      if (!enabled && totalProtocols <= 1) {
        showToast(tr("supplier.messages.keepOneProtocol"), "info");
        return;
      }
      if (nextProtocols.length === 0) {
        applySupplierSurfaceBindings(selectedSurfaceBindings.filter((binding) => binding.surface !== surface));
        return;
      }
      const preferredCount = nextProtocols.filter((item) => item.preferred).length;
      updateSupplierSurfaceBinding(surface, {
        protocols: preferredCount === 1
          ? nextProtocols
          : nextProtocols.map((item, index) => ({ ...item, preferred: index === 0 })),
      });
    }

  function selectCustomSupplierDefaultProtocol(protocol: DebugProtocol) {
      const surface = surfaceForProtocol(protocol);
      const existing = selectedSurfaceBindings.find((binding) => binding.surface === surface);
      const nextBinding: ChannelSurfaceBinding = existing
        ? {
          ...existing,
          protocols: [
            ...existing.protocols,
            ...(!existing.protocols.some((item) => item.protocol === protocol)
              ? [{
                protocol,
                preferred: existing.protocols.length === 0,
                verification: protocolVerification(selectedChannel, protocol),
              }]
              : []),
          ],
        }
        : {
          surface,
          base_url: selectedChannel.upstream_base_url,
          endpoint_profile: surfaceEndpointProfile(selectedChannel.kind, surface, selectedChannel.subscription?.platform),
          auth_scheme: surfaceAuthScheme(selectedChannel.kind, surface),
          protocols: [{ protocol, preferred: true, verification: protocolVerification(selectedChannel, protocol) }],
          operation_overrides: [],
          verification: { state: "declared", checked_at_unix: 0, summary: "" },
        };
      const bindings = existing
        ? selectedSurfaceBindings.map((binding) => binding.surface === surface ? nextBinding : binding)
        : [...selectedSurfaceBindings, nextBinding];
      applySupplierSurfaceBindings(bindings, protocol);
    }

  function addSupplierOperationOverride(surface: ApiSurface) {
      const binding = selectedSurfaceBindings.find((item) => item.surface === surface);
      if (!binding) return;
      const configured = new Set(binding.operation_overrides.map((item) => item.operation));
      const option = surfaceOperationOptions(surface).find((item) => !configured.has(item.operation));
      if (!option) {
        showToast(tr("supplier.messages.allOperationsConfigured"), "info");
        return;
      }
      updateSupplierSurfaceBinding(surface, {
        operation_overrides: [
          ...binding.operation_overrides,
          {
            operation: option.operation,
            method: option.method,
            url: defaultSurfaceOperationUrl(binding.base_url, binding.surface, option.operation),
          },
        ],
      });
    }

  function updateSupplierOperationOverride(
      surface: ApiSurface,
      index: number,
      patch: Partial<OperationEndpointOverride>,
    ) {
      const binding = selectedSurfaceBindings.find((item) => item.surface === surface);
      if (!binding) return;
      updateSupplierSurfaceBinding(surface, {
        operation_overrides: binding.operation_overrides.map((item, itemIndex) => (
          itemIndex === index ? { ...item, ...patch } : item
        )),
      });
    }

  function removeSupplierOperationOverride(surface: ApiSurface, index: number) {
      const binding = selectedSurfaceBindings.find((item) => item.surface === surface);
      if (!binding) return;
      updateSupplierSurfaceBinding(surface, {
        operation_overrides: binding.operation_overrides.filter((_, itemIndex) => itemIndex !== index),
      });
    }

  function applySupplierSurfaceBindings(bindings: ChannelSurfaceBinding[], preferredProtocol?: DebugProtocol) {
      const usableBindings = bindings
        .filter((binding) => binding.protocols.length > 0)
        .map((binding) => {
          const preferred = binding.protocols.filter((protocol) => protocol.preferred);
          if (preferred.length === 1) return binding;
          const configuredDefault = binding.surface === selectedChannel.default_target.surface
            ? binding.protocols.find((protocol) => (
              protocol.protocol === selectedChannel.default_target.protocol
            ))?.protocol
            : undefined;
          const selected = configuredDefault ?? preferred[0]?.protocol ?? binding.protocols[0].protocol;
          return {
            ...binding,
            protocols: binding.protocols.map((protocol) => ({
              ...protocol,
              preferred: protocol.protocol === selected,
            })),
          };
        });
      const requestedTarget = preferredProtocol
        ? { surface: surfaceForProtocol(preferredProtocol), protocol: preferredProtocol }
        : selectedChannel.default_target;
      const targetExists = (target: typeof selectedChannel.default_target) => usableBindings.some(
        (binding) => binding.surface === target.surface
          && binding.protocols.some((protocol) => protocol.protocol === target.protocol),
      );
      const fallbackTarget = usableBindings.flatMap((binding) => binding.protocols
        .filter((protocol) => protocol.preferred)
        .map((protocol) => ({ surface: binding.surface, protocol: protocol.protocol })))[0]
        ?? usableBindings.flatMap((binding) => binding.protocols
          .map((protocol) => ({ surface: binding.surface, protocol: protocol.protocol })))[0];
      const nextTarget = targetExists(requestedTarget)
        ? requestedTarget
        : fallbackTarget ?? selectedChannel.default_target;
      updateSupplier({
        default_target: nextTarget,
        surfaces: usableBindings,
        surface_bindings: usableBindings,
        upstream_base_url: usableBindings.find((binding) => binding.surface === nextTarget.surface)?.base_url ?? "",
        capability_profiles: [],
        detection_checks: [],
        model_capability_evidence: [],
        detection_evidence: [],
      });
    }

  function updateChannel(patch: Partial<ChannelConfig>) {
      setConfig((current) => {
        const currentChannels = current.channels.length > 0 ? current.channels : [channelFromSupplier("channel-1", current.supplier)];
        const targetId = selectedChannelId || currentChannels[0]?.id || "channel-1";
        const channels = currentChannels.map((channel) => {
          if (channel.id !== targetId) {
            return channel;
          }
          const patched = { ...channel, ...patch };
          const surfaces = patch.surfaces
            ?? patch.surface_bindings
            ?? channel.surfaces;
          const defaultTarget = patch.default_target ?? channel.default_target;
          const credentialRef = patch.credential_ref
            ?? (patch.upstream_api_key !== undefined ? patch.upstream_api_key : channel.credential_ref);
          const updatedSurfaces = patch.upstream_base_url === undefined
            ? surfaces
            : surfaces.map((surface) => surface.surface === defaultTarget.surface
              ? { ...surface, base_url: patch.upstream_base_url ?? "" }
              : surface);
          const models = normalizeModelList(patched.models, patched.public_model, patched.upstream_model);
          const configuredDefault = patch.default_model
            ?? patch.upstream_model
            ?? patch.public_model
            ?? patched.default_model
            ?? "";
          const reconciled = reconcileDetectedDefaultModel(configuredDefault, models);
          const upstreamModel = reconciled.defaultModel;
          const publicModel = sourceDriverById(channel.source_driver).category === "subscription"
            ? (patched.public_model || upstreamModel)
            : upstreamModel;
          return projectRendererChannel({
            ...patched,
            source_driver: channel.source_driver,
            executor: channel.executor,
            credential_ref: credentialRef,
            surfaces: updatedSurfaces,
            surface_bindings: updatedSurfaces,
            default_target: defaultTarget,
            default_model: upstreamModel,
            model_capability_evidence: patch.model_capability_evidence
              ?? (patch.capability_profiles as ChannelConfig["model_capability_evidence"] | undefined)
              ?? channel.model_capability_evidence,
            detection_evidence: patch.detection_evidence
              ?? patch.detection_checks
              ?? channel.detection_evidence,
            upstream_model: upstreamModel,
            public_model: publicModel,
            models: reconciled.models,
          });
        });
        const supplier = supplierFromChannel(channels[0] ?? channelFromSupplier("channel-1", current.supplier));
        return { ...current, channels, supplier };
      });
    }

  async function runSupplierChannelOperation(
      channelId: string,
      kind: SupplierChannelOperationKind,
      task: () => Promise<void>,
      timeoutMs = SUPPLIER_OPERATION_TIMEOUT_MS,
      checksUpstream = kind === "testing" || kind === "enabling",
    ) {
      const active = supplierChannelOperationRef.current;
      if (active) {
        showToast(tr("supplier.messages.operationInProgress", {
          operation: supplierChannelOperationLabel(active.kind),
        }), "info", 3200);
        return false;
      }
      const operation: SupplierChannelOperation = {
        channelId,
        kind,
        token: ++supplierChannelOperationTokenRef.current,
        checksUpstream,
      };
      supplierChannelOperationRef.current = operation;
      setSupplierChannelOperation(operation);
      let timeoutId: number | null = null;
      const timeoutError = new Error(tr("supplier.messages.operationTimeout", {
        operation: supplierChannelOperationLabel(kind),
      }));
      try {
        if (timeoutMs > 0) {
          await Promise.race([
            task(),
            new Promise<never>((_, reject) => {
              timeoutId = window.setTimeout(() => reject(timeoutError), timeoutMs);
            }),
          ]);
        } else {
          await task();
        }
        return true;
      } catch (error) {
        if (error === timeoutError) {
          showToast(timeoutError.message, "error", 6200);
          try {
            const latest = await invoke<SupplierStatus>("supplier_status");
            setSupplierStatus(latest);
          } catch {
            // The original timeout is the actionable error.
          }
        } else {
          showToast(localizedError(error), "error", 4200);
        }
        return false;
      } finally {
        if (timeoutId !== null) window.clearTimeout(timeoutId);
        if (supplierChannelOperationRef.current?.token === operation.token) {
          supplierChannelOperationRef.current = null;
          setSupplierChannelOperation(null);
        }
      }
    }

  async function removeSelectedChannel(id: string) {
      const currentConfig = configRef.current;
      const currentChannels = currentConfig.channels.length > 0
        ? currentConfig.channels
        : [channelFromSupplier("channel-1", currentConfig.supplier)];
      const channel = currentChannels.find((item) => item.id === id);
      if (!channel) {
        showToast(tr("supplier.messages.channelMissing"), "info");
        return;
      }

      const confirmed = await askConfirm(supplierDeleteConfirmation(channel.name || channel.id));
      if (!confirmed) return;

      let preflightStatus: SupplierStatus;
      try {
        preflightStatus = await invoke<SupplierStatus>("supplier_status");
        supplierStatusRef.current = preflightStatus;
        setSupplierStatus(preflightStatus);
      } catch (error) {
        showToast(tr("supplier.messages.deletePreflightFailed", {
          error: localizedError(error),
        }), "error", 5200);
        return;
      }

      await runSupplierChannelOperation(id, "deleting", async () => {
        const latestStatus = await invoke<SupplierStatus>("supplier_status");
        supplierStatusRef.current = latestStatus;
        setSupplierStatus(latestStatus);
        const runningNow = supplierChannelRuntimeStarted(latestStatus, id);

        let stoppedRuntime = false;
        if (runningNow) {
          let stoppedStatus: SupplierStatus;
          try {
            stoppedStatus = await stopSupplierChannelRuntime(id);
          } catch (error) {
            throw new Error(tr("supplier.messages.stopBeforeDeleteFailed", {
              error: localizedError(error),
            }));
          }
          if (supplierChannelRuntimeStarted(stoppedStatus, id)) {
            throw new Error(tr("supplier.messages.runtimeStillPresent"));
          }
          stoppedRuntime = true;
          supplierStatusRef.current = stoppedStatus;
          setSupplierStatus(stoppedStatus);
        }

        const beforeDelete = configRef.current;
        const beforeChannels = beforeDelete.channels.length > 0
          ? beforeDelete.channels
          : [channelFromSupplier("channel-1", beforeDelete.supplier)];
        const remainingChannels = beforeChannels.filter((item) => item.id !== id);
        const nextChannels = remainingChannels.length > 0
          ? remainingChannels
          : [channelFromSupplier("channel-1", emptyConfig.supplier)];
        const next = normalizeConfigForSave({
          ...beforeDelete,
          channels: nextChannels,
          supplier: supplierFromChannel(nextChannels[0]),
        });
        try {
          await persistConfig(next);
        } catch (error) {
          configRef.current = beforeDelete;
          setConfig(beforeDelete);
          setSelectedChannelId(id);
          throw new Error(
            stoppedRuntime
              ? tr("supplier.messages.deleteConfigFailedAfterStop", { error: localizedError(error) })
              : tr("supplier.messages.deleteConfigFailed", { error: localizedError(error) }),
          );
        }

        const deletionState = finishSupplierDeletion(nextChannels);
        if (deletionState.closeDetail) setSupplierDetailOpen(false);
        setSelectedChannelId(deletionState.selectedChannelId);
        setSupplierModels(deletionState.models);
        setSupplierTestResult({ status: "idle" });
        setSupplierQuotaSnapshots((current) => {
          if (!Object.prototype.hasOwnProperty.call(current, id)) return current;
          const nextSnapshots = { ...current };
          delete nextSnapshots[id];
          writeSupplierQuotaSnapshots(nextSnapshots);
          return nextSnapshots;
        });
        resolveDiagnostic(`subscription-detection-${id}`);
        showToast(supplierDeletedNotice(channel.name || channel.id, stoppedRuntime), "info");
      }, 0);
    }

  function normalizeConfigForSave(current: ClientConfig): ClientConfig {
      const firstEndpoint = current.endpoints.find((endpoint) => endpoint.enabled && endpoint.base_url.trim()) ?? current.endpoints.find((endpoint) => endpoint.base_url.trim());
      const serverWsUrl = firstEndpoint ? supplierWsFromEndpoint(firstEndpoint) : current.supplier.server_ws_url;
      const serverQuicUrl = firstEndpoint ? supplierQuicFromEndpoint(firstEndpoint) : current.supplier.server_quic_url;
      const rawChannels = current.channels.length > 0 ? current.channels : [channelFromSupplier("channel-1", current.supplier)];
      const channels = rawChannels.map((channel, index) => normalizeRendererChannel({
        ...channel,
        id: channel.id || `channel-${index + 1}`,
        name: channel.name || (index === 0 ? "Local Channel" : `Local Channel ${index + 1}`),
        server_ws_url: channel.server_ws_url || serverWsUrl,
        server_quic_url: channel.server_quic_url || serverQuicUrl,
        price_ratio: normalizedPriceRatio(channel.price_ratio),
      }));
      const supplier = supplierFromChannel(channels[0] ?? channelFromSupplier("channel-1", current.supplier));
      return {
        ...current,
        config_version: 5,
        supplier_auto_start: channels.some(channelIsPlatformPooled),
        channels,
        supplier,
      };
    }

  function activeSupplierChannelIds(excludeId?: string) {
      return (supplierStatus.channels ?? [])
        .filter((health) => health.status === "available" && health.channel_id !== excludeId)
        .map((health) => health.channel_id);
    }

  function runnableChannelIds(ids: string[], cfg: ClientConfig) {
      return Array.from(new Set(ids))
        .filter((id) => cfg.channels.some((channel) => channel.id === id && channelIsPlatformPooled(channel)));
    }

  function sharedSupplierChannelIds(cfg: ClientConfig) {
      return cfg.channels
        .filter(channelIsPlatformPooled)
        .map((channel) => channel.id);
    }

  async function persistConfig(next: ClientConfig) {
      const normalized = normalizeConfigForSave(next);
      const baseline = savedConfigRef.current;
      configRef.current = normalized;
      setConfig(normalized);
      const wire = await invoke<ClientConfigV5Wire>("save_supplier_config", {
        channels: normalized.channels.map(channelV5FromRenderer),
        baseChannels: baseline.channels.map(channelV5FromRenderer),
      });
      const authoritative = hydrateClientConfigV5(wire);
      const nextDraft = overlayAccessConfig(authoritative, normalized, baseline);
      configRef.current = nextDraft;
      savedConfigRef.current = authoritative;
      setConfig(nextDraft);
      setSavedConfig(authoritative);
      return nextDraft;
    }

  async function moveLocalChannel(channelId: string, direction: -1 | 1) {
      if (selectedChannelHasChanges) {
        showToast(tr("supplier.messages.saveBeforeReorder"), "info", 4200);
        return;
      }
      await runConfigAction(async () => {
        const previous = configRef.current;
        const visibleIds = channelsVisibleInModelMarket(
          previous.channels,
          showDevelopmentPresentation,
        ).map((channel) => channel.id);
        const channels = moveChannelInVisibleOrder(
          previous.channels,
          visibleIds,
          channelId,
          direction,
        );
        if (!channels) return;
        try {
          await persistConfig({
            ...previous,
            channels,
            supplier: supplierFromChannel(channels[0]),
          });
        } catch (error) {
          configRef.current = previous;
          setConfig(previous);
          showToast(tr("supplier.messages.reorderFailed", {
            error: localizedError(error),
          }), "error", 5200);
        }
      });
    }

  function rememberDetectedQuota(channelId: string, observation: ChannelDetectionResult | SupplierChannelHealth) {
      setSupplierQuotaSnapshots((current) => {
        const next = rememberSupplierQuotaSnapshot(current, channelId, observation);
        if (next !== current) writeSupplierQuotaSnapshots(next);
        return next;
      });
    }

  function supplierChannelNeedsDetection(channel: ChannelConfig) {
      const saved = savedChannels.find((item) => item.id === channel.id);
      if (!saved || channel.models.length === 0) return true;
      return channelDetectionInputChanged(saved, channel);
    }

  async function decideDuplicateChannel(
      channel: ChannelConfigV5,
    ): Promise<ChannelDuplicateDecision | null> {
      const candidates = await invoke<ChannelDuplicateCandidate[]>(
        "find_duplicate_channels",
        { channel },
      );
      if (candidates.length === 0) return null;
      return askChannelDuplicate({
        draftName: channel.name || channel.id,
        candidates,
      });
    }

  function restoreDraftAndOpenExisting(draftId: string, existingId: string) {
      const persisted = savedConfigRef.current;
      setConfig((current) => {
        const persistedDraft = persisted.channels.find((channel) => channel.id === draftId);
        let nextChannels = current.channels;
        if (persistedDraft) {
          nextChannels = current.channels.map(
            (channel) => channel.id === draftId ? persistedDraft : channel,
          );
        } else {
          nextChannels = current.channels.filter((channel) => channel.id !== draftId);
        }
        const firstChannel = nextChannels[0]
          ?? channelFromSupplier("channel-1", persisted.supplier);
        return {
          ...current,
          channels: nextChannels,
          supplier: supplierFromChannel(firstChannel),
        };
      });
      setSelectedChannelId(existingId);
      setShowSupplierAdvanced(false);
      setSupplierAddOpen(false);
      setSupplierDetailOpen(true);
      const existing = persisted.channels.find((channel) => channel.id === existingId);
      setSupplierModels(existing?.models ?? []);
    }

  function duplicateDraftChannel(channel: ChannelConfig) {
      const persisted = savedConfigRef.current.channels.find(
        (candidate) => candidate.id === channel.id,
      );
      if (!persisted) return channel;
      let suffix = Date.now().toString(36);
      let id = `channel-${suffix}`;
      let attempt = 0;
      while (configRef.current.channels.some((candidate) => candidate.id === id)) {
        attempt += 1;
        suffix = `${Date.now().toString(36)}-${attempt}`;
        id = `channel-${suffix}`;
      }
      return {
        ...channel,
        id,
        created_at_unix_ms: Date.now(),
      };
    }

  async function saveSupplierChanges(channelArg = selectedChannel, forceDetection = false) {
      const persistedChannel = savedConfigRef.current.channels.find(
        (candidate) => candidate.id === channelArg.id,
      );
      const changedToFree = normalizedPriceRatio(channelArg.price_ratio) === 0
        && normalizedPriceRatio(persistedChannel?.price_ratio ?? 1) !== 0;
      if (changedToFree) {
        const confirmed = await askConfirm({
          title: tr("supplier.messages.freeSupplyTitle"),
          message: tr("supplier.messages.freeSupplyMessage"),
          confirmText: tr("supplier.messages.freeSupplyConfirm"),
        });
        if (!confirmed) return;
      }
      let channelToSave = channelArg;
      try {
        if (channelToSave.source_driver === "lan_share") {
          channelToSave = normalizeRendererChannel(channelToSave);
        }
        const duplicateDecision = await decideDuplicateChannel(
          channelV5FromRenderer(channelToSave),
        );
        if (duplicateDecision?.action === "cancel") return;
        if (duplicateDecision?.action === "open_existing") {
          restoreDraftAndOpenExisting(channelArg.id, duplicateDecision.channelId);
          return;
        }
        if (duplicateDecision?.action === "create_new") {
          channelToSave = duplicateDraftChannel(channelToSave);
        }
      } catch (error) {
        showToast(tr("supplier.messages.duplicateCheckFailed", {
          error: localizedError(error),
        }), "error", 5200);
        return;
      }
      const needsDetection = forceDetection || supplierChannelNeedsDetection(channelToSave);
      await runSupplierChannelOperation(channelToSave.id, "saving", async () => {
        let channelPersisted = false;
        let registrationOnly = false;
        try {
          let channel = channelToSave;
          let validationWarning = "";
          if (needsDetection) {
            const validation = await invoke<SupplierChannelValidationResult>(
              "validate_channel_upstream",
              { channel: channelToSave },
            );
            channel = rendererChannelFromV5(validation.channel, channelToSave);
            rememberDetectedQuota(channel.id, validation.health);
            setSupplierModels(channel.models);
            if (validation.health.status !== "available") validationWarning = validation.health.message || tr("supplier.availability.noDetails");
          }
          const persistedOriginal = savedConfigRef.current.channels.find(
            (item) => item.id === channelArg.id,
          );
          let currentChannels = configRef.current.channels;
          if (channel.id !== channelArg.id && persistedOriginal) {
            currentChannels = currentChannels.map(
              (item) => item.id === channelArg.id ? persistedOriginal : item,
            );
          }
          currentChannels = currentChannels.some((item) => item.id === channel.id)
            ? currentChannels.map((item) => item.id === channel.id ? channel : item)
            : [...currentChannels, channel];
          const next = normalizeConfigForSave({
            ...configRef.current,
            channels: currentChannels,
            supplier: supplierFromChannel(currentChannels[0]),
          });
          await persistConfig(next);
          channelPersisted = true;
          if (needsDetection) {
            // Read the saved channel's effective group snapshot, including retained
            // conclusions after an inconclusive check. This is a local, free read.
            try { setSupplierStatus(await invoke<SupplierStatus>("supplier_status")); }
            catch { /* Keep existing observations; the regular status read will retry. */ }
          }
          if (validationWarning) {
            setSelectedChannelId(channel.id);
            showToast(tr("supplier.messages.savedRuntimeValidationFailed", { error: validationWarning }), "error", 6200);
            return;
          }
          if (channelIsPlatformPooled(channel) && (supplierStatus.running || supplierStatus.starting)) {
            if (needsDetection || channelRuntimeNeedsActivation(persistedOriginal, channel)) {
              const ensured = await startSupplierChannelRuntime(channel.id);
              const finalSupplierStatus = await waitForSupplierPlatformAck(channel.id, ensured);
              setSupplierStatus(finalSupplierStatus);
            } else {
              // Saving policy/limits is not an activation or an inference check.
              // Update the existing connection; its monitor/recovery owns network work.
              registrationOnly = true;
              setSupplierStatus(await invoke<SupplierStatus>("refresh_supplier_registration", {
                channelId: channel.id,
              }));
            }
          }
          setSelectedChannelId(channel.id);
          showToast(
            needsDetection
              ? tr("supplier.messages.validatedAndSaved")
              : tr("supplier.messages.saved"),
            "success",
          );
        } catch (error) {
          showToast(
            tr(
              channelPersisted
                ? registrationOnly
                  ? "access.messages.savedSynchronizationFailed"
                  : "supplier.messages.savedRuntimeValidationFailed"
                : "supplier.messages.validationSaveFailed",
              { error: localizedError(error) },
            ),
            "error",
            6200,
          );
        }
      }, SUPPLIER_OPERATION_TIMEOUT_MS, needsDetection);
    }

  async function startSupplierChannelRuntime(channelId: string, previousStatus = supplierStatus) {
      setPendingSupplierChannelIds([channelId]);
      setSupplierStatus({
        ...previousStatus,
        starting: true,
        active_transport: previousStatus.active_transport || "starting",
        last_transport_error: "",
      });
      try {
        return await invoke<SupplierStatus>("start_supplier", { channelId, channelIds: null });
      } finally {
        setPendingSupplierChannelIds([]);
      }
    }

  async function activateSupplierChannelRuntime(channelId: string, previousStatus = supplierStatus) {
      setPendingSupplierChannelIds([channelId]);
      const pendingStatus = {
        ...previousStatus,
        starting: true,
        active_transport: previousStatus.active_transport || "starting",
        last_transport_error: "",
      };
      supplierStatusRef.current = pendingStatus;
      setSupplierStatus(pendingStatus);
      try {
        const activated = await invoke<SupplierStatus>("activate_supplier_channel", { channelId });
        supplierStatusRef.current = activated;
        setSupplierStatus(activated);
        return activated;
      } catch (error) {
        try {
          const latest = await invoke<SupplierStatus>("supplier_status");
          supplierStatusRef.current = latest;
          setSupplierStatus(latest);
        } catch {
          // Preserve the activation error; the regular status poll will reconcile later.
        }
        throw error;
      } finally {
        setPendingSupplierChannelIds([]);
      }
    }

  async function waitForSupplierPlatformAck(channelId: string, initialStatus: SupplierStatus) {
      let latest = initialStatus;
      if (accountStateRef.current !== "signed_in") return latest;

      const deadline = Date.now() + SUPPLIER_PLATFORM_ACK_TIMEOUT_MS;
      while (Date.now() < deadline) {
        const channelTransport = supplierChannelTransportForChannel(latest, channelId);
        if (
          isEstablishedSupplierTransport(channelTransport?.active_transport)
          && channelTransport?.platform_registered === true
        ) return latest;
        await new Promise((resolve) => window.setTimeout(resolve, SUPPLIER_PLATFORM_ACK_POLL_MS));
        latest = await invoke<SupplierStatus>("supplier_status");
        setSupplierStatus(latest);
      }
      return latest;
    }

  async function stopSupplierChannelRuntime(channelId: string) {
      setPendingSupplierChannelIds([]);
      return await invoke<SupplierStatus>("stop_supplier_channel", { channelId });
    }

  async function startSupplier(channelArg = selectedChannel) {
      await runSupplierChannelOperation(channelArg.id, "enabling", async () => {
        const targetModel = channelArg.upstream_model || channelArg.public_model || channelArg.models[0] || "health check";
        setSelectedChannelId(channelArg.id);
        setSupplierStartingChannelId(channelArg.id);
        setSupplierTestResult({ status: "running", model: targetModel });
        if (channelArg.source_driver === "lan_share") {
          try {
            const stagedChannel = { ...channelArg, enabled: false, share_enabled: false };
            const nextChannels = configRef.current.channels.map((item) => (
              item.id === stagedChannel.id ? stagedChannel : item
            ));
            await persistConfig({
              ...configRef.current,
              channels: nextChannels,
              supplier: supplierFromChannel(nextChannels[0]),
            });
            const st = await activateSupplierChannelRuntime(stagedChannel.id);
            const latest = await invoke<ClientConfigV5Wire>("get_config");
            const normalizedLatest = hydrateClientConfigV5(latest);
            const nextDraft = overlayAccessConfig(
              normalizedLatest,
              configRef.current,
              savedConfigRef.current,
            );
            configRef.current = nextDraft;
            savedConfigRef.current = normalizedLatest;
            setConfig(nextDraft);
            setSavedConfig(normalizedLatest);
            const activatedChannel = normalizedLatest.channels.find(
              (item) => item.id === stagedChannel.id,
            ) ?? stagedChannel;
            const channelHealth = st.channels?.find(
              (item) => item.channel_id === stagedChannel.id,
            );
            if (!channelHealth || channelHealth.status !== "available") {
              throw new Error(channelHealth?.message || tr("supplier.messages.liveValidationFailed"));
            }
            setSupplierModels(activatedChannel.models);
            setSupplierTestResult({
              status: "success",
              latencyMs: channelHealth.latency_ms,
              upstreamHttpVersion: channelHealth.upstream_http_version,
              model: channelHealth.model || activatedChannel.upstream_model,
              content: tr("lanShare.channelEnabled"),
            });
            showToast(tr("lanShare.channelEnabled"), "success");
          } catch (error) {
            setSupplierTestResult({
              status: "error",
              model: targetModel,
              error: localizedError(error),
            });
            showToast(localizedError(error), "error", 4200);
          } finally {
            setSupplierStartingChannelId(null);
          }
          return;
        }
        try {
          const stagedChannel = { ...channelArg, enabled: false, share_enabled: false };
          const currentChannels = channels.map(
            (item) => item.id === stagedChannel.id ? stagedChannel : item,
          );
          await persistConfig({
            ...config,
            channels: currentChannels,
            supplier: supplierFromChannel(currentChannels[0]),
          });
          const startedStatus = await activateSupplierChannelRuntime(stagedChannel.id);
          setSupplierStatus(startedStatus);
          const st = await waitForSupplierPlatformAck(stagedChannel.id, startedStatus);
          setSupplierStatus(st);
          const latest = await invoke<ClientConfigV5Wire>("get_config");
          const normalizedLatest = hydrateClientConfigV5(latest);
          const nextDraft = overlayAccessConfig(
            normalizedLatest,
            configRef.current,
            savedConfigRef.current,
          );
          configRef.current = nextDraft;
          savedConfigRef.current = normalizedLatest;
          setConfig(nextDraft);
          setSavedConfig(normalizedLatest);
          const activatedChannel = normalizedLatest.channels.find(
            (item) => item.id === stagedChannel.id,
          ) ?? stagedChannel;
          const channelHealth = st.channels?.find(
            (item) => item.channel_id === stagedChannel.id,
          );
          if (!channelHealth || channelHealth.status !== "available") {
            throw new Error(channelHealth?.message || tr("supplier.messages.liveValidationFailed"));
          }
          rememberDetectedQuota(stagedChannel.id, channelHealth);
          setSupplierModels(activatedChannel.models);
          setSupplierTestResult({
            status: "success",
            latencyMs: channelHealth?.latency_ms,
            upstreamHttpVersion: channelHealth?.upstream_http_version,
            model: channelHealth?.model || activatedChannel.upstream_model,
            content: channelHealth?.message || tr("supplier.messages.liveValidationStarted"),
          });
          resolveDiagnostic("supplier-auto");
          const channelName = activatedChannel.name || activatedChannel.id;
          const channelTransport = supplierChannelTransportForChannel(st, activatedChannel.id);
          const routeStatus = supplierRouteStatusForChannel(st, activatedChannel.id);
          const routeDetail = supplierRouteStatusDetail(routeStatus);
          if (
            isEstablishedSupplierTransport(channelTransport?.active_transport)
            && channelTransport?.platform_registered === true
          ) {
            if (routeDetail && !supplierRouteStatusSuppressesToast(routeStatus)) {
              showToast(
                tr("supplier.messages.localValidatedWithRoute", {
                  channel: channelName,
                  detail: routeDetail,
                }),
                supplierRouteStatusBlocksReady(routeStatus) ? "info" : "success",
                5200,
              );
            } else {
              showToast(tr("supplier.messages.platformOnline", {
                channel: channelName,
              }), "success");
            }
          } else if (account.status.state !== "signed_in") {
            showToast(tr("supplier.messages.localValidatedSignInLater", {
              channel: channelName,
            }), "info", 5200);
          } else if (channelTransport?.last_transport_error || st.last_transport_error) {
            showToast(
              tr("supplier.messages.platformConnectionIncomplete", {
                channel: channelName,
                error: channelTransport?.last_transport_error || st.last_transport_error,
              }),
              "error",
              6200,
            );
          } else {
            showToast(tr("supplier.messages.platformNotConfirmed", {
              channel: channelName,
            }), "info", 5200);
          }
        } catch (error) {
          setSupplierStatus((current) => ({ ...current, starting: false }));
          setPendingSupplierChannelIds([]);
          setSupplierTestResult({
            status: "error",
            model: targetModel,
            error: localizedError(error),
          });
          showToast(localizedError(error), "error", 4200);
        } finally {
          setSupplierStartingChannelId(null);
        }
      });
    }

  async function stopSelectedChannel(channel = selectedChannel) {
      await runSupplierChannelOperation(channel.id, "disabling", async () => {
        try {
          const nextChannels = channels.map((item) => item.id === channel.id ? { ...item, enabled: false, share_enabled: false } : item);
          const runningChannelIds = runnableChannelIds(activeSupplierChannelIds(channel.id), { ...config, channels: nextChannels });
          const shouldKeepSupplierRunning = runningChannelIds.length > 0;
          const next = normalizeConfigForSave({
            ...config,
            supplier_auto_start: shouldKeepSupplierRunning,
            channels: nextChannels,
            supplier: supplierFromChannel(nextChannels[0]),
          });
          await persistConfig(next);
          const st = await stopSupplierChannelRuntime(channel.id);
          if (!shouldKeepSupplierRunning) {
            setSupplierStatus(st);
            showToast(tr("supplier.messages.channelDisabled", {
              channel: channel.name || channel.id,
            }), "info");
            return;
          }
          setSupplierStatus(st);
          const latest = await invoke<ClientConfigV5Wire>("get_config");
          const normalizedLatest = hydrateClientConfigV5(latest);
          const nextDraft = overlayAccessConfig(
            normalizedLatest,
            configRef.current,
            savedConfigRef.current,
          );
          configRef.current = nextDraft;
          savedConfigRef.current = normalizedLatest;
          setConfig(nextDraft);
          setSavedConfig(normalizedLatest);
          showToast(tr("supplier.messages.channelDisabledOthersContinue", {
            channel: channel.name || channel.id,
          }), "info");
        } catch (error) {
          setSupplierStatus((current) => ({ ...current, starting: false }));
          showToast(localizedError(error), "error", 4200);
        }
      }, SUPPLIER_QUICK_OPERATION_TIMEOUT_MS);
    }

  async function refreshSupplierModels(options: RefreshOptions = { throttle: true }) {
      if (selectedChannelHasChanges) {
        showToast(tr("supplier.messages.saveBeforeRefresh"), "info", 4200);
        return;
      }
      let refreshGate = supplierModelRefreshGatesRef.current.get(selectedChannel.id);
      if (!refreshGate) {
        refreshGate = createRefreshGate();
        supplierModelRefreshGatesRef.current.set(selectedChannel.id, refreshGate);
      }
      if (!refreshGate.begin(options)) return;
      const started = await runSupplierChannelOperation(selectedChannel.id, "refreshing", async () => {
        try {
          const detection = await invoke<ChannelDetectionResult>("refresh_channel_upstream", { channel: selectedChannel });
          setSupplierModels(detection.models);
          const detectedChannel = applyChannelDetection(selectedChannel, detection);
          rememberDetectedQuota(detectedChannel.id, detection);
          const currentChannels = channels.map((item) => item.id === detectedChannel.id ? detectedChannel : item);
          await persistConfig({
            ...config,
            channels: currentChannels,
            supplier: supplierFromChannel(currentChannels[0]),
          });
          if (channelIsPlatformPooled(detectedChannel) && (supplierStatus.running || supplierStatus.starting)) {
            const refreshed = await invoke<SupplierStatus>("refresh_supplier_registration");
            supplierStatusRef.current = refreshed;
            setSupplierStatus(refreshed);
          }
          resolveDiagnostic(`subscription-detection-${selectedChannel.id}`);
          const warningText = detection.warnings.length > 0 ? `，${detection.warnings[0]}` : "";
          const action = tr("supplier.messages.refreshedChannelInformation");
          showToast(tr("supplier.messages.refreshedModels", {
            action,
            count: detection.models.length,
            warning: warningText,
          }), detection.warnings.length > 0 ? "info" : "success", 5200);
        } catch (error) {
          refreshGate.reset();
          showToast(localizedError(error), "error", 4200);
        }
      });
      if (!started) refreshGate.reset();
    }

  async function refreshRoutePlan() {
      if (debugTargetType !== "platform_auto") {
        setRoutePlanState({
          status: "unavailable",
          error: tr("debugConsole.route.platformOnly"),
        });
        return;
      }
      if (!effectiveDebugModel.trim()) {
        setRoutePlanState({
          status: "unavailable",
          error: tr("debugConsole.selectModelFirst"),
        });
        return;
      }
      const requestedPath = accessTestRequestPath(debugInboundProtocol, effectiveDebugModel);
      const body = routePlanRequestBody(
        debugInboundProtocol,
        effectiveDebugModel,
        debugPrompt,
        debugStream,
        { claudeServerSideCompaction: debugClaudeServerSideCompaction },
      );
      setRoutePlanState({
        status: "loading",
        requestedModel: effectiveDebugModel,
        requestedProtocol: debugInboundProtocol,
        requestedPath,
      });
      try {
        const payload = await invoke<{ data?: RouteDecision }>("fetch_supplier_route_plan", {
          model: effectiveDebugModel,
          path: requestedPath,
          protocol: debugInboundProtocol,
          body,
        });
        if (!payload?.data) {
          throw new Error("route plan response missing data");
        }
        setRoutePlanState({
          status: "success",
          decision: payload.data,
          requestedModel: effectiveDebugModel,
          requestedProtocol: debugInboundProtocol,
          requestedPath,
          refreshedAt: Date.now(),
        });
        showToast(
          tr("debugConsole.route.refreshed"),
          payload.data.selected ? "success" : "info",
          3200,
        );
      } catch (error) {
        setRoutePlanState({
          status: "error",
          error: routePlanInvokeErrorText(error),
          requestedModel: effectiveDebugModel,
          requestedProtocol: debugInboundProtocol,
          requestedPath,
          refreshedAt: Date.now(),
        });
        showToast(routePlanInvokeErrorText(error), "error", 4200);
      }
    }

  async function runProtocolDebug(execute: boolean) {
      if (!effectiveDebugModel.trim()) {
        showToast(tr("debugConsole.selectModelFirst"), "error", 3200);
        return;
      }
      setDebugRunning(execute ? "execute" : "preview");
      setDebugResult(null);
      try {
        const result = await invoke<ProtocolDebugResult>("debug_protocol_exchange", {
          targetType: debugTargetType,
          targetId: debugTargetType === "platform_auto" ? null : effectiveDebugTargetId || null,
          inboundProtocol: debugInboundProtocol,
          targetProtocol: debugTargetProtocol,
          model: effectiveDebugModel,
          prompt: debugPrompt,
          stream: debugStream,
          skipLocalShortCircuit: debugSkipLocalShortCircuit,
          experimentalFeatures: debugClaudeServerSideCompaction
            ? [DEBUG_EXPERIMENT_CLAUDE_SERVER_SIDE_COMPACTION]
            : [],
          execute,
        });
        setDebugResult(result);
        const outcome = protocolDebugOutcome(result);
        if (outcome === "request_failed") {
          showToast(
            tr("debugConsole.result.outcome.failedToast", {
              status: result.http_status ?? "-",
            }),
            "error",
            5200,
          );
        } else if (outcome === "compaction_not_triggered") {
          showToast(tr("debugConsole.result.outcome.notTriggeredTitle"), "info", 4200);
        } else if (outcome === "compaction_triggered") {
          showToast(tr("debugConsole.result.outcome.triggeredTitle"), "success", 4200);
        } else {
          showToast(
            execute
              ? tr("debugConsole.requestComplete")
              : tr("debugConsole.previewComplete"),
            "success",
            3600,
          );
        }
      } catch (error) {
        const errorText = String(error);
        const errorBytes = utf8ByteLength(errorText);
        setDebugResult({
          target_type: debugTargetType,
          target_id: effectiveDebugTargetId,
          inbound_protocol: debugInboundProtocol,
          target_protocol: debugTargetProtocol,
          requested_model: effectiveDebugModel,
          upstream_model: effectiveDebugModel,
          conversion_level: "unsupported",
          path: accessTestRequestPath(debugTargetProtocol, effectiveDebugModel),
          request_headers: {},
          request_body: {},
          unsupported_fields: [],
          lossy_warnings: [errorText],
          executed: execute,
          content_type: "",
          content: errorText,
          raw: errorText,
          metrics: {
            request_body_bytes: 0,
            response_raw_bytes: errorBytes,
            response_content_bytes: errorBytes,
            response_content_chars: errorText.length,
            response_raw_lines: 1,
            response_sse_events: 0,
            response_sse_done: false,
            response_sse_last_event_type: "",
            response_kind: "error",
            tool_call_count: 0,
            finish_reason: "",
          },
          error_layer: errorText.startsWith("conversion:")
            ? "conversion"
            : errorText.startsWith("experiment:")
              ? "experiment"
              : "debug_command",
        });
        showToast(errorText, "error", 4200);
      } finally {
        setDebugRunning("idle");
        if (execute) {
          const source = activeCallLogRef.current?.source;
          const afterMs = activeCallLogRef.current?.cursorMs ?? 0;
          void refreshCallRecords({ silent: true, source, afterMs, incremental: Boolean(source) }).then((records) => {
            if (source) updateActiveCallLog(source, records);
          });
        }
      }
    }

  async function testSupplierUpstream() {
      const model = selectedChannelModel.trim();
      if (!model) {
        setSupplierTestResult({
          status: "error",
          error: tr("supplier.messages.refreshOrSelectModel"),
        });
        showToast(tr("supplier.messages.refreshOrSelectModel"), "error", 3200);
        return;
      }
      await runSupplierChannelOperation(selectedChannel.id, "testing", async () => {
        setSupplierTestResult({ status: "running", model });
        try {
          const channel = {
            ...selectedChannel,
            api_format: selectedApiFormat,
          };
          const result = await invoke<ChannelUpstreamTestResult>("test_channel_upstream", {
            channel,
            model,
            prompt: supplierTestPrompt,
          });
          setSupplierTestResult({
            status: "success",
            httpStatus: result.http_status,
            latencyMs: result.latency_ms,
            upstreamHttpVersion: result.upstream_http_version,
            model: result.model,
            inputTokens: result.input_tokens,
            outputTokens: result.output_tokens,
            content: result.content,
            raw: result.raw,
          });
          setSupplierStatus((current) => applySupplierUpstreamObservation(
            current,
            selectedChannel.id,
            result,
          ));
          showToast(tr("supplier.messages.testComplete"), "success");
        } catch (error) {
          setSupplierTestResult({
            status: "error",
            model,
            error: localizedError(error),
          });
          showToast(localizedError(error), "error", 4200);
        }
      });
    }

  function selectDebugTarget(type: DebugTargetType, target: DebugTargetOption) {
      setDebugTargetType(type);
      setDebugTargetId(target.id);
      setDebugModel(target.defaultModel || target.models[0] || "");
      setDebugTargetProtocol(debugClaudeServerSideCompaction
        ? "anthropic_messages"
        : debugRecommendedTargetProtocol(target, debugInboundProtocol, type));
      setDebugResult(null);
    }

  function updateDebugInboundProtocol(protocol: DebugProtocol) {
      setDebugInboundProtocol(protocol);
      if (debugTargetType === "platform_auto") {
        setDebugTargetProtocol(protocol);
      }
    }

  function changeDebugClaudeServerSideCompaction(enabled: boolean) {
      setDebugClaudeServerSideCompaction(enabled);
      setDebugResult(null);
      setRoutePlanState({ status: "idle" });
      if (enabled) {
        setDebugInboundProtocol("anthropic_messages");
        setDebugTargetProtocol("anthropic_messages");
      }
    }

  function toggleDebugTargetGroup(type: DebugTargetType) {
      setExpandedDebugTargetGroups((current) => (
        current.includes(type) ? current.filter((item) => item !== type) : [...current, type]
      ));
    }

  async function refreshToolConfigStatuses() {
      if (!isTauriRuntime()) return;
      const epoch = ++toolConfigStatusEpochRef.current;
      setToolConfigChecking(true);
      try {
        const entries = await Promise.all(toolCards.map(async (card) => {
          try {
            const result = await withUiTimeout(
              invoke<ToolApplyResult>("check_tool_config", {
                tool: card.tool,
                claudeModelSettings: card.tool === "claude" ? claudeModelSettings : undefined,
              }),
              TOOL_STATUS_TIMEOUT_MS,
              tr("toolOperations.statusCheckTimeout", { tool: card.title }),
            );
            return [card.tool, result] as const;
          } catch {
            return [card.tool, null] as const;
          }
        }));
        if (epoch !== toolConfigStatusEpochRef.current) return;
        setToolConfigStatuses(Object.fromEntries(entries));
        const codexStatus = entries.find(([tool]) => tool === "codex")?.[1];
        if (codexStatus && !codexModelSourceTouchedRef.current) {
          setCodexModelSource(codexModelSourceFromStatus(codexStatus));
        }
        setToolProtocolSelections((current) => {
          const next = { ...current };
          for (const [tool, status] of entries) {
            if (toolProtocolDraftTouchedRef.current[tool]) continue;
            const detected = toolProtocolId(status?.details?.tool_protocol);
            if (detected && TOOL_PROTOCOLS_BY_TOOL[tool]?.includes(detected)) {
              next[tool] = detected;
            }
          }
          return next;
        });
        void refreshCodexSessionScan();
      } finally {
        if (epoch === toolConfigStatusEpochRef.current) {
          setToolConfigChecking(false);
        }
      }
    }

  async function refreshCodexSessionScan() {
      if (!isTauriRuntime()) return;
      try {
        const result = await invoke<CodexSessionScanResult>("scan_codex_session_history");
        setCodexSessionScan(result);
      } catch {
        setCodexSessionScan(null);
      }
    }

  async function refreshSingleToolConfigStatus(
      tool: string,
      claudeSettings: ClaudeModelSettings | undefined = tool === "claude"
        ? claudeModelSettings
        : undefined,
    ) {
      const epoch = ++toolConfigStatusEpochRef.current;
      setToolConfigChecking(false);
      const status = await withUiTimeout(
        invoke<ToolApplyResult>("check_tool_config", {
          tool,
          claudeModelSettings: claudeSettings,
        }),
        TOOL_STATUS_TIMEOUT_MS,
        tr("toolOperations.statusCheckTimeout"),
      );
      if (epoch === toolConfigStatusEpochRef.current) {
        setToolConfigStatuses((current) => ({ ...current, [tool]: status }));
        if (tool === "codex") {
          codexModelSourceTouchedRef.current = false;
          setCodexModelSource(codexModelSourceFromStatus(status));
        }
        const detected = toolProtocolId(status.details?.tool_protocol);
        if (detected && TOOL_PROTOCOLS_BY_TOOL[tool]?.includes(detected)) {
          toolProtocolDraftTouchedRef.current[tool] = false;
          setToolProtocolSelections((current) => ({ ...current, [tool]: detected }));
        }
      }
      return status;
    }

  async function refreshToolConfigStatusAfterSuccess(
      tool: string,
      title: string,
      result: ToolApplyResult,
      claudeSettings?: ClaudeModelSettings,
    ) {
      try {
        return {
          status: await refreshSingleToolConfigStatus(tool, claudeSettings),
          warning: "",
        };
      } catch (error) {
        const optimisticStatus: ToolApplyResult = {
          ...result,
        };
        setToolConfigStatuses((current) => ({ ...current, [tool]: optimisticStatus }));
        return {
          status: optimisticStatus,
          warning: tr("toolOperations.statusRefreshFailed", {
            tool: title,
            error: localizedError(error),
          }),
        };
      }
    }

  function askConfirm(dialog: ConfirmDialogState) {
      confirmResolverRef.current?.(false);
      return new Promise<boolean>((resolve) => {
        confirmResolverRef.current = resolve;
        setConfirmDialog(dialog);
      });
    }

  function resolveConfirmDialog(confirmed: boolean) {
      const resolve = confirmResolverRef.current;
      confirmResolverRef.current = null;
      setConfirmDialog(null);
      resolve?.(confirmed);
    }

  function askToolRemove(dialog: ToolRemoveDialogState) {
      toolRemoveResolverRef.current?.("cancel");
      return new Promise<ToolRemoveDecision>((resolve) => {
        toolRemoveResolverRef.current = resolve;
        setToolRemoveDialog(dialog);
      });
    }

  function resolveToolRemoveDialog(decision: ToolRemoveDecision) {
      const resolve = toolRemoveResolverRef.current;
      toolRemoveResolverRef.current = null;
      setToolRemoveDialog(null);
      resolve?.(decision);
    }

  function toolNativeRemoveOption(tool: string) {
      const key = tool === "claude-desktop" ? "claudeDesktop" : tool;
      if (!matchesNativeRemoveTool(tool)) return undefined;
      return {
        title: tr(`toolOperations.removeModes.nativeOptions.${key}.title`),
        description: tr(`toolOperations.removeModes.nativeOptions.${key}.description`),
      };
    }

  function askChannelDuplicate(dialog: ChannelDuplicateDialogState) {
      channelDuplicateResolverRef.current?.({ action: "cancel" });
      return new Promise<ChannelDuplicateDecision>((resolve) => {
        channelDuplicateResolverRef.current = resolve;
        setChannelDuplicateDialog(dialog);
      });
    }

  function resolveChannelDuplicateDialog(decision: ChannelDuplicateDecision) {
      const resolve = channelDuplicateResolverRef.current;
      channelDuplicateResolverRef.current = null;
      setChannelDuplicateDialog(null);
      resolve?.(decision);
    }

  function beginToolOperation(tool: string) {
      if (toolOperationRunningRef.current[tool]) {
        return false;
      }
      toolOperationRunningRef.current = { ...toolOperationRunningRef.current, [tool]: true };
      setToolLaunching((current) => ({ ...current, [tool]: true }));
      setToolOperationLabels((current) => ({
        ...current,
        [tool]: tr("toolOperations.progress.awaitingConfirmation"),
      }));
      setToolOperationProgress((current) => ({
        ...current,
        [tool]: advanceToolConfigProgress(current[tool], {
          stage: "preparing",
          completed: null,
          total: null,
        }),
      }));
      return true;
    }

  function updateToolOperationLabel(tool: string, label: string, title?: string) {
      setToolOperationLabels((current) => ({ ...current, [tool]: label }));
      if (title) showToast(`${title}：${label}`, "info", 2200, false);
    }

  function finishToolOperation(tool: string) {
      const next = { ...toolOperationRunningRef.current };
      delete next[tool];
      toolOperationRunningRef.current = next;
      setToolLaunching((current) => ({ ...current, [tool]: false }));
      setToolOperationLabels((current) => {
        const nextLabels = { ...current };
        delete nextLabels[tool];
        return nextLabels;
      });
      setToolOperationProgress((current) => {
        const nextProgress = { ...current };
        delete nextProgress[tool];
        return nextProgress;
      });
    }

  async function executeToolConfigOperation(
      tool: string,
      title: string,
      action: "configure" | "launch" | "remove",
      claudeSettings?: ClaudeModelSettings,
      removeMode?: ToolRemoveMode,
      closeRunningProcessConfirmed = false,
    ): Promise<ToolApplyResult | null> {
      const backendAction = action === "remove" ? "remove" : "apply";
      const protocol = toolProtocolSelections[tool] ?? DEFAULT_TOOL_PROTOCOLS[tool];
      const effectiveClaudeSettings = tool === "claude" && backendAction === "apply"
        ? claudeSettings ?? claudeModelSettings
        : undefined;
      const request = async (confirmationToken?: string) => {
        const ticket = await invoke<ToolConfigOperationTicket>("begin_tool_config_operation", {
          tool,
          action: backendAction,
          restart: action === "launch",
          confirmationToken: confirmationToken,
        });
        const operationId = ticket.operation_id;
        const stopWatching = watchToolConfigOperation(
          operationId,
          (status) => {
            if (status.terminal) return;
            const progress = {
              stage: status.progress_stage,
              completed: status.progress_completed,
              total: status.progress_total,
              stageRatio: status.progress_ratio,
            };
            setToolOperationProgress((current) => ({
              ...current,
              [tool]: advanceToolConfigProgress(current[tool], progress),
            }));
            setToolOperationLabels((current) => ({
              ...current,
              [tool]: toolConfigProgressLabel(progress),
            }));
          },
          (error) => console.warn("[tool-config] progress monitor stopped", error),
        );
        try {
          return await invoke<ToolConfigOperationResponse>("execute_tool_config_operation", {
            operationId: operationId,
            tool,
            action: backendAction,
            protocol: backendAction === "apply" ? protocol : undefined,
            claudeModelSettings: effectiveClaudeSettings,
            codexModelSource: tool === "codex" && backendAction === "apply" ? codexModelSource : undefined,
            removeMode: backendAction === "remove" ? removeMode : undefined,
            confirmationToken: confirmationToken,
            restart: action === "launch",
          });
        } finally {
          stopWatching();
        }
      };

      updateToolOperationLabel(
        tool,
        action === "remove"
          ? tr("toolOperations.progress.preparingRestore")
          : tr("labels.toolMenu.progress.checkingConfig"),
        title,
      );
      let response = await request();
      if (response.confirmation_required) {
        setToolOperationLabels((current) => ({
          ...current,
          [tool]: tr("toolOperations.progress.awaitingConfirmation"),
        }));
        setToolOperationProgress((current) => ({
          ...current,
          [tool]: advanceToolConfigProgress(current[tool], {
            stage: "awaiting_confirmation",
            completed: null,
            total: null,
          }),
        }));
        const confirmationToken = response.confirmation_token;
        if (!confirmationToken) {
          throw new Error("TOOL_CONFIG_CONFIRMATION_MISSING: backend did not return a token");
        }
        const actionText = action === "launch"
          ? tr("toolOperations.actions.configureAndRestart")
          : action === "configure"
            ? tr("toolOperations.actions.configureOnly")
            : tr("toolOperations.actions.removeConfiguration");
        const processCloseAlreadyConfirmed =
          action === "remove" && closeRunningProcessConfirmed;
        const confirmed = processCloseAlreadyConfirmed
          ? true
          : await askConfirm({
              title: response.running
                ? tr("toolOperations.dialogs.runningTitle", { tool: title })
                : tr("toolOperations.dialogs.removeTitle", { tool: title }),
              message: response.running
                ? tr("toolOperations.dialogs.runningMessage", { action: actionText })
                : tool === "codex"
                  ? tr("toolOperations.dialogs.removeCodexMessage", { tool: title })
                  : tr("toolOperations.dialogs.removeMessage", { tool: title }),
              confirmText: actionText,
              tone: action === "remove" ? "danger" : "default",
            });
        if (!confirmed) return null;
        updateToolOperationLabel(
          tool,
          response.running
            ? action === "remove"
              ? tr("toolOperations.progress.forceStopRestore")
              : action === "launch"
                ? tr("toolOperations.progress.forceStopLaunch")
                : tr("toolOperations.progress.forceStopConfigure")
            : tr("toolOperations.progress.restore"),
          title,
        );
        response = await request(confirmationToken);
      }
      if (response.manual_close_required) {
        setToolOperationLabels((current) => ({
          ...current,
          [tool]: tr("toolOperations.progress.awaitingManualClose"),
        }));
        setToolOperationProgress((current) => ({
          ...current,
          [tool]: advanceToolConfigProgress(current[tool], {
            stage: "awaiting_manual_close",
            completed: null,
            total: null,
          }),
        }));
        const forceContinueToken = response.confirmation_token;
        if (!forceContinueToken) {
          throw new Error("TOOL_CONFIG_FORCE_CONTINUE_MISSING: backend did not return a token");
        }
        const actionText = action === "launch"
          ? tr("toolOperations.actions.configureAndLaunch")
          : action === "configure"
            ? tr("toolOperations.actions.configureOnly")
            : tr("toolOperations.actions.removeConfiguration");
        const confirmed = await askConfirm({
          title: tr("toolOperations.dialogs.unableToConfirmClosedTitle", { tool: title }),
          message: tr("toolOperations.dialogs.closeManuallyMessage", {
            detail: response.message
              ? localizedError(response.message)
              : tr("toolOperations.dialogs.processStillRunning"),
            tool: title,
            action: actionText,
          }),
          confirmText: tr("toolOperations.actions.continue"),
          cancelText: tr("toolOperations.actions.cancelFlow"),
          tone: "danger",
        });
        if (!confirmed) return null;
        updateToolOperationLabel(
          tool,
          tr("toolOperations.progress.ignoreCloseAndContinue", { action: actionText }),
          title,
        );
        response = await request(forceContinueToken);
      }
      if (response.confirmation_required || response.manual_close_required || !response.result) {
        throw new Error(tr("toolOperations.incomplete", { tool: title }));
      }
      setToolOperationLabels((current) => ({
        ...current,
        [tool]: tr("toolOperations.progress.completed"),
      }));
      setToolOperationProgress((current) => ({
        ...current,
        [tool]: advanceToolConfigProgress(current[tool], {
          stage: "completed",
          completed: 1,
          total: 1,
        }),
      }));
      return response.result;
    }

  function storeAppliedClaudeModelSettings(settings: ClaudeModelSettings) {
      const next = { ...settings };
      setClaudeModelSettings(next);
      setClaudeModelDraft(next);
      writeClaudeModelSettings(next);
    }

  async function configureTool(
      tool: string,
      title: string,
      restartAfterConfig: boolean,
      claudeSettings?: ClaudeModelSettings,
    ) {
      if (!beginToolOperation(tool)) return false;
      const wasConfiguredBefore = Boolean(toolConfigStatuses[tool]?.already_configured);
      const effectiveClaudeSettings = tool === "claude"
        ? { ...(claudeSettings ?? claudeModelDraft) }
        : undefined;
      try {
        const result = await executeToolConfigOperation(
          tool,
          title,
          restartAfterConfig ? "launch" : "configure",
          effectiveClaudeSettings,
        );
        if (!result) return false;
        const restartCommand = result.details?.restart_command;
        const restartLauncherPath = result.details?.restart_launcher_path;
        const operationNotice = toolConfigOperationResultNotice(title, restartAfterConfig, result);
        if (restartAfterConfig && !operationNotice) {
          if (!restartCommand || !restartLauncherPath) {
            throw new Error(tr("toolOperations.launchConfirmationMissing", { tool: title }));
          }
        }
        updateToolOperationLabel(tool, tr("toolOperations.progress.refreshStatus"));
        const {
          status: refreshedStatus,
          warning: statusRefreshWarning,
        } = await refreshToolConfigStatusAfterSuccess(
          tool,
          title,
          result,
          effectiveClaudeSettings,
        );
        const changedCount = result.files.length;
        const checkedCount = result.file_statuses?.length ?? changedCount;
        const migratedFiles = Number(result.details?.session_migrated_files ?? 0);
        const migratedRows = Number(result.details?.session_migrated_sqlite_rows ?? 0);
        const sessionChanged = migratedFiles + migratedRows;
        const showConfiguredResult = (
          message: string,
          tone: Toast["tone"] = "success",
          duration = 6200,
          restoreHint = true,
        ) => showToast(restoreHint ? toolConfigSuccessNotice(message) : message, tone, duration);
        const firstConfigurationNotice = toolFirstConfigurationNotice({
          title,
          restartAfterConfig,
          wasConfiguredBefore,
          result,
          refreshedStatus,
          statusRefreshVerified: !statusRefreshWarning,
        });
        if (firstConfigurationNotice) {
          await askConfirm(firstConfigurationNotice);
        } else if (operationNotice) {
          showConfiguredResult(
            operationNotice.text,
            operationNotice.tone,
            operationNotice.duration,
            operationNotice.restoreHint,
          );
        } else if (restartCommand && restartLauncherPath) {
          const manualStart = restartLauncherPath.startsWith("manual start required");
          showConfiguredResult(
            manualStart
              ? tr("toolOperations.manualStartRequired", { tool: title })
              : tr("toolOperations.restartCommand", { tool: title, command: restartCommand }),
            manualStart ? "info" : "success",
          );
        } else if (sessionChanged > 0) {
          showConfiguredResult(tr("toolOperations.sessionMigrationComplete", {
            tool: title,
            config: changedCount > 0
              ? tr("toolOperations.filesWritten", { changed: changedCount, checked: checkedCount })
              : tr("toolOperations.filesCurrent"),
            target: result.details?.session_target_provider
              ? tr("toolOperations.targetProvider", { provider: result.details.session_target_provider })
              : "",
            files: migratedFiles,
            rows: migratedRows,
          }));
        } else if (result.already_configured) {
          showConfiguredResult(tr("toolOperations.alreadyConfigured", { tool: title }));
        } else {
          showConfiguredResult(tr("toolOperations.configured", {
            tool: title,
            changed: changedCount,
            checked: checkedCount,
            backups: result.backups.length,
          }));
        }
        if (statusRefreshWarning) showToast(statusRefreshWarning, "error", 5200);
        if (effectiveClaudeSettings) {
          storeAppliedClaudeModelSettings(effectiveClaudeSettings);
        }
        return true;
      } catch (error) {
        if (
          restartAfterConfig
          && toolConfigCanLaunchExistingAfterFailure(error)
        ) {
          try {
            updateToolOperationLabel(tool, tr("toolOperations.progress.launchProgram"));
            await invoke<string>("start_tool_program", {
              tool,
              useConfiguredEnvironment: true,
            });
            setToolOperationLabels((current) => ({
              ...current,
              [tool]: tr("toolOperations.progress.completed"),
            }));
            showToast(
              tr("toolOperations.modelRefreshUnavailableUsedExisting", { tool: title }),
              "info",
              5200,
            );
            return true;
          } catch (launchError) {
            showToast(tr("toolOperations.launchFailed", {
              tool: title,
              error: localizedError(launchError),
            }), "error", 5200);
            return false;
          }
        }
        showToast(tr("toolOperations.configureFailed", {
          tool: title,
          action: restartAfterConfig
            ? tr("toolOperations.actions.configureOrLaunch")
            : tr("common.configure"),
          error: localizedError(error),
        }), "error", 5200);
        return false;
      } finally {
        finishToolOperation(tool);
      }
    }

  async function applyToolConfig(tool: string, title: string) {
      await configureTool(tool, title, false);
    }

  async function startToolProgram(
      tool: string,
      title: string,
      useConfiguredEnvironment: boolean,
    ) {
      if (!beginToolOperation(tool)) return;
      try {
        updateToolOperationLabel(tool, tr("toolOperations.progress.launchProgram"));
        await invoke<string>("start_tool_program", { tool, useConfiguredEnvironment });
        showToast(tr("toolOperations.launched", { tool: title }), "success");
      } catch (error) {
        showToast(tr("toolOperations.launchFailed", { tool: title, error: localizedError(error) }), "error", 5200);
      } finally {
        finishToolOperation(tool);
      }
    }

  async function launchToolConfig(tool: string, title: string) {
      await configureTool(tool, title, true);
    }

  async function removeToolConfig(tool: string, title: string) {
      const removeDecision = await askToolRemove({
        tool,
        toolTitle: title,
        nativeOption: toolNativeRemoveOption(tool),
      });
      if (removeDecision === "cancel") return;
      if (!beginToolOperation(tool)) return;
      try {
        const result = await executeToolConfigOperation(
          tool,
          title,
          "remove",
          undefined,
          removeDecision,
          true,
        );
        if (!result) return;
        const resetClaudeSettings = tool === "claude"
          ? { ...DEFAULT_CLAUDE_MODEL_SETTINGS }
          : undefined;
        if (resetClaudeSettings) {
          storeAppliedClaudeModelSettings(resetClaudeSettings);
        }
        toolConfigStatusEpochRef.current += 1;
        setToolConfigStatuses((current) => {
          const next = { ...current };
          delete next[tool];
          return next;
        });
        try {
          updateToolOperationLabel(tool, tr("toolOperations.progress.refreshStatus"));
          const checked = await refreshSingleToolConfigStatus(tool, resetClaudeSettings);
          const noManifestOwnership = removeDecision === "restore_pre_const"
            && tool !== "codex"
            && result.details?.manifest_entries_restored === "0"
            && result.files.length === 0;
          const sessionIndexError = result.details?.session_migrated_sqlite_error;
          const sessionIndexInvalidRows = Number.parseInt(
            result.details?.session_migrated_sqlite_invalid_rows ?? "0",
            10,
          );
          const sessionSyncError = result.details?.session_sync_error;
          const attentionReason = noManifestOwnership
            ? tr("toolOperations.removeNoOwnedConfig", { tool: title })
            : checked.already_configured
              ? tr("toolOperations.removeStillDetected", { tool: title })
              : sessionIndexError
                ? tr("labels.sessionIndexFailed", { error: sessionIndexError })
                : sessionIndexInvalidRows > 0
                  ? tr("labels.sessionIndexInvalidRows", {
                      count: sessionIndexInvalidRows,
                    })
                  : sessionSyncError
                    ? tr("labels.sessionMigrationFailed", { error: sessionSyncError })
                    : "";
          const needsAttention = Boolean(attentionReason);
          await askConfirm({
            title: needsAttention
              ? tr("toolOperations.dialogs.removeResultAttentionTitle", { tool: title })
              : tr("toolOperations.dialogs.removeResultSuccessTitle", { tool: title }),
            message: needsAttention
              ? tr("toolOperations.dialogs.removeResultAttentionMessage", {
                  tool: title,
                  reason: attentionReason,
                })
              : tr("toolOperations.dialogs.removeResultSuccessMessage", { tool: title }),
            confirmText: tr("toolOperations.actions.acknowledge"),
            tone: needsAttention ? "danger" : "default",
            mode: "notice",
          });
        } catch (checkError) {
          setToolConfigStatuses((current) => ({
            ...current,
            [tool]: { ...result, already_configured: false },
          }));
          await askConfirm({
            title: tr("toolOperations.dialogs.removeResultAttentionTitle", { tool: title }),
            message: tr("toolOperations.dialogs.removeResultAttentionMessage", {
              tool: title,
              reason: tr("toolOperations.removedStatusRefreshFailed", {
                tool: title,
                error: localizedError(checkError),
              }),
            }),
            confirmText: tr("toolOperations.actions.acknowledge"),
            tone: "danger",
            mode: "notice",
          });
        }
      } catch (error) {
        await askConfirm({
          title: tr("toolOperations.dialogs.removeResultFailedTitle", { tool: title }),
          message: tr("toolOperations.dialogs.removeResultFailureMessage", {
            tool: title,
            error: localizedError(error),
          }),
          confirmText: tr("toolOperations.actions.acknowledge"),
          tone: "danger",
          mode: "notice",
        });
      } finally {
        finishToolOperation(tool);
      }
    }

  async function openToolLocator(tool: string, title: string, continueWithLaunch = false) {
      setShowMissingToolCandidates(false);
      setToolLocator({ tool, title, result: null, loading: true, error: "", continueWithLaunch });
      try {
        const result = await invoke<ToolProgramLocationResult>("locate_tool_program", { tool });
        setToolLocator({ tool, title, result, loading: false, error: "", continueWithLaunch });
      } catch (error) {
        setToolLocator({ tool, title, result: null, loading: false, error: String(error), continueWithLaunch });
      }
    }

  async function chooseToolProgram(tool: string, title: string, continueWithLaunch = false) {
      setShowMissingToolCandidates(false);
      setToolLocator((current) => current ? { ...current, loading: true, error: "" } : current);
      try {
        const result = await invoke<ToolProgramLocationResult>("choose_tool_program", { tool });
        showToast(tr("toolOperations.programLocationSaved", { tool: title }), "success");
        if (continueWithLaunch) {
          setToolLocator(null);
          await launchToolConfig(tool, title);
          return;
        }
        setToolLocator({ tool, title, result, loading: false, error: "", continueWithLaunch: false });
      } catch (error) {
        setToolLocator((current) => current ? { ...current, loading: false, error: String(error) } : current);
      }
    }

  async function saveToolProgram(tool: string, title: string, path: string, continueWithLaunch = false) {
      setToolLocator((current) => current ? { ...current, loading: true, error: "" } : current);
      try {
        const result = await invoke<ToolProgramLocationResult>("save_tool_program", { tool, path });
        showToast(tr("toolOperations.programLocationSaved", { tool: title }), "success");
        if (continueWithLaunch) {
          setToolLocator(null);
          await launchToolConfig(tool, title);
          return;
        }
        setToolLocator({ tool, title, result, loading: false, error: "", continueWithLaunch: false });
      } catch (error) {
        setToolLocator((current) => current ? { ...current, loading: false, error: String(error) } : current);
      }
    }

  function openSubscriptionWizard(provider: SubscriptionProvider) {
      const previousState = subscriptionOAuthActiveStateRef.current;
      subscriptionOAuthActiveStateRef.current = null;
      subscriptionOAuthExchangeInFlightRef.current = false;
      if (previousState) {
        void cancelAutomaticSubscriptionCallback(previousState);
      }
      setSubscriptionWizardProvider(provider);
      // Keep the add-channel dialog mounted so the wizard behaves as a
      // stacked overlay and closing it returns to the source picker.
      setSubscriptionWizardOpen(true);
      setSubscriptionOAuthSession(null);
      setSubscriptionCallbackInput("");
      setSubscriptionTokenJson("");
      setSubscriptionManualOpen(false);
      setSubscriptionImportRetryChannel(null);
      setSubscriptionWizardProgress(null);
    }

  function discardPendingSubscriptionImport(
      channel = subscriptionImportRetryChannel,
    ) {
      if (!channel) return;
      void invoke<boolean>("discard_subscription_import", { channel }).catch(() => {
        // Finalized credentials are retained; only isolated pending files are removable.
      });
    }

  function replaceSubscriptionDraftInput(kind: "callback" | "json", value: string) {
      discardPendingSubscriptionImport();
      setSubscriptionImportRetryChannel(null);
      setSubscriptionWizardProgress(null);
      if (kind === "callback") {
        setSubscriptionCallbackInput(value);
      } else {
        setSubscriptionTokenJson(value);
      }
    }

  async function createSubscriptionOAuth(provider = subscriptionWizardProvider) {
      setSubscriptionWizardBusy(true);
      const previousState = subscriptionOAuthActiveStateRef.current;
      if (previousState) {
        await cancelAutomaticSubscriptionCallback(previousState);
      }
      discardPendingSubscriptionImport();
      setSubscriptionImportRetryChannel(null);
      setSubscriptionWizardProgress({
        tone: "info",
        message: tr("subscriptionImport.progress.creatingLink"),
      });
      try {
        const session = await invoke<SubscriptionOAuthSession>("create_subscription_oauth_session", { provider });
        setSubscriptionWizardProvider(provider);
        setSubscriptionWizardOpen(true);
        setSubscriptionOAuthSession(session);
        subscriptionOAuthActiveStateRef.current = session.state;
        setSubscriptionWizardProgress(!session.auto_callback && session.redirect_uri.startsWith("http://")
          ? { tone: "info", message: tr("subscriptionImport.manualCallbackRequired") }
          : null);
        showToast(tr("subscriptionImport.linkCreated"), "success");
        if (session.auto_callback) {
          void invoke<string | null>("wait_subscription_oauth_callback", {
            oauthState: session.state,
          }).then((callback) => {
            if (!callback || subscriptionOAuthActiveStateRef.current !== session.state) return;
            setSubscriptionCallbackInput(callback);
            void completeSubscriptionOAuth(session, callback, null);
          }).catch(() => {
            // Listener timeout/cancellation silently falls back to manual paste.
          });
        }
        void invoke<void>("open_external_url", { url: session.auth_url }).catch(() => {
          // The authorization URL remains visible and copyable when opening fails.
        });
      } catch (error) {
        setSubscriptionWizardProgress({ tone: "error", message: localizedError(error) });
        showToast(localizedError(error), "error", 5200);
      } finally {
        setSubscriptionWizardBusy(false);
      }
    }

  async function exchangeSubscriptionOAuth() {
      const retryChannel = subscriptionImportRetryChannel;
      const session = subscriptionOAuthSession;
      if (!retryChannel && !session) {
        showToast(tr("subscriptionImport.createLinkFirst"), "error");
        return;
      }
      if (!retryChannel && !subscriptionCallbackInput.trim()) {
        showToast(tr("subscriptionImport.pasteCallback"), "error");
        return;
      }
      await completeSubscriptionOAuth(session, subscriptionCallbackInput, retryChannel);
    }

  async function completeSubscriptionOAuth(
      session: SubscriptionOAuthSession | null,
      callback: string,
      retryChannel: ChannelConfigV5 | null,
    ) {
      if (subscriptionOAuthExchangeInFlightRef.current) return;
      if (!retryChannel && (!session || !callback.trim())) return;
      subscriptionOAuthExchangeInFlightRef.current = true;
      setSubscriptionWizardBusy(true);
      setSubscriptionWizardProgress({
        tone: "info",
        message: retryChannel
          ? tr("subscriptionImport.progress.rechecking")
          : tr("subscriptionImport.progress.exchanging"),
      });
      if (session) {
        void cancelAutomaticSubscriptionCallback(session.state);
      }
      try {
        const channel = retryChannel ?? await invoke<ChannelConfigV5>("exchange_subscription_oauth_code", {
          provider: session!.provider,
          codeOrCallback: callback,
          state: session!.state,
          codeVerifier: session!.code_verifier,
          redirectUri: session!.redirect_uri,
        });
        setSubscriptionImportRetryChannel(channel);
        const finalizedChannel = await finalizePreparedSubscriptionChannel(channel);
        setSubscriptionImportRetryChannel(finalizedChannel);
        setSubscriptionWizardProgress({
          tone: "info",
          message: tr("subscriptionImport.progress.validating"),
        });
        await withUiTimeout(
          addImportedSubscriptionChannel(finalizedChannel),
          SUBSCRIPTION_IMPORT_TIMEOUT_MS,
          tr("subscriptionImport.timeout"),
        );
        closeSubscriptionWizard();
      } catch (error) {
        const timedOut = error instanceof Error && error.message === tr("subscriptionImport.timeout");
        const message = timedOut ? tr("subscriptionImport.timeout") : localizedError(error);
        setSubscriptionWizardProgress({
          tone: "error",
          message: timedOut
            ? tr("subscriptionImport.timeoutCallbackHint")
            : tr("subscriptionImport.incomplete", { error: localizedError(error) }),
        });
        showToast(message, "error", timedOut ? 7200 : 6200);
      } finally {
        subscriptionOAuthExchangeInFlightRef.current = false;
        setSubscriptionWizardBusy(false);
      }
    }

  async function importSubscriptionOAuthFromClipboard() {
      const session = subscriptionOAuthSession;
      if (!session || subscriptionWizardBusy) return;
      let callback = "";
      try {
        callback = (await navigator.clipboard.readText()).trim();
      } catch {
        showToast(tr("subscriptionImport.clipboardReadFallback"), "info", 3600, false);
        return;
      }
      if (!callback) {
        showToast(tr("subscriptionImport.clipboardEmptyFallback"), "info", 3600, false);
        return;
      }
      replaceSubscriptionDraftInput("callback", callback);
      await completeSubscriptionOAuth(session, callback, null);
    }

  async function cancelAutomaticSubscriptionCallback(oauthState: string) {
      try {
        await invoke<void>("cancel_subscription_oauth_callback", { oauthState });
      } catch {
        // Automatic callback capture is best-effort and never blocks manual paste.
      }
    }

  async function importSubscriptionJson() {
      const retryChannel = subscriptionImportRetryChannel;
      if (!retryChannel && !subscriptionTokenJson.trim()) {
        showToast(tr("subscriptionImport.pasteTokenJson"), "error");
        return;
      }
      setSubscriptionWizardBusy(true);
      setSubscriptionWizardProgress({
        tone: "info",
        message: tr("subscriptionImport.progress.savingAndValidating"),
      });
      try {
        const channel = retryChannel ?? await invoke<ChannelConfigV5>("import_subscription_token_json", {
          provider: subscriptionWizardProvider,
          tokenJson: subscriptionTokenJson,
        });
        setSubscriptionImportRetryChannel(channel);
        const finalizedChannel = await finalizePreparedSubscriptionChannel(channel);
        setSubscriptionImportRetryChannel(finalizedChannel);
        await withUiTimeout(
          addImportedSubscriptionChannel(finalizedChannel),
          SUBSCRIPTION_IMPORT_TIMEOUT_MS,
          tr("subscriptionImport.timeout"),
        );
        closeSubscriptionWizard();
      } catch (error) {
        const timedOut = error instanceof Error && error.message === tr("subscriptionImport.timeout");
        const message = timedOut ? tr("subscriptionImport.timeout") : localizedError(error);
        setSubscriptionWizardProgress({
          tone: "error",
          message: timedOut
            ? tr("subscriptionImport.timeoutJsonHint")
            : tr("subscriptionImport.incomplete", { error: localizedError(error) }),
        });
        showToast(message, "error", timedOut ? 7200 : 5200);
      } finally {
        setSubscriptionWizardBusy(false);
      }
    }

  function closeSubscriptionWizard() {
      const oauthState = subscriptionOAuthActiveStateRef.current;
      subscriptionOAuthActiveStateRef.current = null;
      if (oauthState) {
        void cancelAutomaticSubscriptionCallback(oauthState);
      }
      discardPendingSubscriptionImport();
      setSubscriptionWizardOpen(false);
      setSubscriptionOAuthSession(null);
      setSubscriptionCallbackInput("");
      setSubscriptionTokenJson("");
      setSubscriptionManualOpen(false);
      setSubscriptionWizardBusy(false);
      setSubscriptionWizardProgress(null);
      setSubscriptionImportRetryChannel(null);
  }

  async function finalizePreparedSubscriptionChannel(
      prepared: ChannelConfigV5,
    ): Promise<ChannelConfigV5> {
      return invoke<ChannelConfigV5>("finalize_subscription_import", {
        channel: prepared,
        createDuplicate: true,
      });
    }

  async function addImportedSubscriptionChannel(imported: ChannelConfigV5) {
      const current = configRef.current;
      const firstEndpoint = current.endpoints.find(
        (endpoint) => endpoint.enabled && endpoint.base_url.trim(),
      );
      const serverWsUrl = firstEndpoint
        ? supplierWsFromEndpoint(firstEndpoint)
        : current.supplier.server_ws_url;
      const serverQuicUrl = firstEndpoint
        ? supplierQuicFromEndpoint(firstEndpoint)
        : current.supplier.server_quic_url;
      const importedChannel = projectRendererChannel({
        ...rendererChannelFromV5(imported),
        created_at_unix_ms: imported.created_at_unix_ms > 0
          ? imported.created_at_unix_ms
          : Date.now(),
        enabled: false,
        share_enabled: false,
        models: [],
        public_model: "",
        upstream_model: "",
        server_ws_url: serverWsUrl,
        server_quic_url: serverQuicUrl,
      });
      const currentChannels = current.channels.length > 0
        ? current.channels
        : [channelFromSupplier("channel-1", current.supplier)];
      const existing = currentChannels.find(
        (item) => item.id === importedChannel.id,
      );
      const channel = existing ?? importedChannel;
      const retainedChannel = existing
        ? { ...existing, enabled: false, share_enabled: false }
        : channel;
      const stagedChannels = existing
        ? currentChannels.map((item) => item.id === existing.id ? retainedChannel : item)
        : [...currentChannels, retainedChannel];
      let channelRetained = false;
      try {
        await persistConfig({
          ...current,
          channels: stagedChannels,
          supplier: supplierFromChannel(stagedChannels[0]),
        });
        channelRetained = true;
        setSelectedChannelId(channel.id);
        setShowSupplierAdvanced(false);
        setSupplierAddOpen(false);
        setSupplierDetailOpen(true);
        setSupplierModels(existing?.models ?? []);
        const status = await activateSupplierChannelRuntime(channel.id, supplierStatusRef.current);
        supplierStatusRef.current = status;
        setSupplierStatus(status);
        const latestWire = await invoke<ClientConfigV5Wire>("get_config");
        const latestConfig = hydrateClientConfigV5(latestWire);
        const activatedChannel = latestConfig.channels.find((item) => item.id === channel.id);
        if (!activatedChannel?.enabled || activatedChannel.models.length === 0) {
          throw new Error(tr("subscriptionImport.emptyActivatedCatalog"));
        }
        const health = status.channels?.find((item) => item.channel_id === channel.id);
        if (!health || health.status !== "available") {
          throw new Error(health?.message || tr("subscriptionImport.channelUnavailable"));
        }
        const nextDraft = overlayAccessConfig(
          latestConfig,
          configRef.current,
          savedConfigRef.current,
        );
        configRef.current = nextDraft;
        savedConfigRef.current = latestConfig;
        setConfig(nextDraft);
        setSavedConfig(latestConfig);
        rememberDetectedQuota(activatedChannel.id, health);
        setSupplierModels(activatedChannel.models);
        resolveDiagnostic(`subscription-detection-${activatedChannel.id}`);
        resolveDiagnostic("supplier-auto");
        showToast(
          tr("subscriptionImport.channelEnabled", { count: activatedChannel.models.length }),
          "success",
          5200,
        );
      } catch (error) {
        pushDiagnostic({
          id: `subscription-detection-${channel.id}`,
          scope: "supplier",
          severity: "error",
          title: tr("subscriptionImport.validationFailedTitle", { channel: channel.name }),
          message: channelRetained
            ? tr("subscriptionImport.validationFailedRetained", { error: localizedError(error) })
            : tr("subscriptionImport.saveFailed", { error: localizedError(error) }),
          action: "open_supplier",
        });
        if (channelRetained) {
          throw new Error(tr("subscriptionImport.retainedDisabled", { error: localizedError(error) }));
        }
        throw new Error(tr("subscriptionImport.saveFailed", { error: localizedError(error) }));
      }
    }

  function addSubscriptionPreset(driver: SourceDriver) {
      if (!driver.subscriptionProvider) return;
      openSubscriptionWizard(driver.subscriptionProvider);
    }

  async function addPresetChannelWithConnection(driver: SourceDriver, connection?: LanShareConnectionInfo) {
      const id = `channel-${Date.now().toString(36)}`;
      const firstEndpoint = config.endpoints.find((endpoint) => endpoint.enabled && endpoint.base_url.trim());
      const serverWsUrl = firstEndpoint ? supplierWsFromEndpoint(firstEndpoint) : config.supplier.server_ws_url;
      const serverQuicUrl = firstEndpoint ? supplierQuicFromEndpoint(firstEndpoint) : config.supplier.server_quic_url;
      const lanShareConnection = driver.id === "lan_share" ? connection : undefined;
      let initialChannel = sourceDriverInitialChannelV5(driver, {
        id,
        name: lanShareConnection?.channelName || driver.title,
        createdAtUnixMs: Date.now(),
        credentialRef: lanShareConnection?.apiKey,
        nodeId: "",
        serverWsUrl,
        serverQuicUrl,
      });
      if (lanShareConnection) {
        initialChannel = normalizeChannelV5({
          ...initialChannel,
          surfaces: initialChannel.surfaces.map((surface) => (
            surface.surface === initialChannel.default_target.surface
              ? { ...surface, base_url: lanShareConnection.baseUrl }
              : surface
          )),
        });
      }
      let channel = rendererChannelFromV5(initialChannel);

      if (driver.detection.creation === "on_add") {
        try {
          const duplicateDecision = await decideDuplicateChannel(
            channelV5FromRenderer(channel),
          );
          if (duplicateDecision?.action === "cancel") return;
          if (duplicateDecision?.action === "open_existing") {
            restoreDraftAndOpenExisting(channel.id, duplicateDecision.channelId);
            return;
          }
          const detection = await invoke<ChannelDetectionResult>("detect_channel_upstream", { channel });
          channel = applyChannelDetection(channel, detection);
          rememberDetectedQuota(channel.id, detection);
          setSupplierModels(detection.models);
          const current = configRef.current;
          const currentChannels = current.channels.length > 0 ? current.channels : [channelFromSupplier("channel-1", current.supplier)];
          const nextChannels = [...currentChannels, channel];
          await persistConfig({ ...current, channels: nextChannels, supplier: supplierFromChannel(nextChannels[0]) });
        } catch (error) {
          showToast(tr("supplier.driverValidationFailed", {
            source: sourceDriverTitle(driver),
            error: localizedError(error),
          }), "error", 5200);
          return;
        }
      } else {
        setConfig((current) => {
          const currentChannels = current.channels.length > 0 ? current.channels : [channelFromSupplier("channel-1", current.supplier)];
          const channels = [...currentChannels, channel];
          return { ...current, channels, supplier: supplierFromChannel(channels[0]) };
        });
        setSupplierModels([]);
      }
      setSelectedChannelId(id);
      setShowSupplierAdvanced(false);
      setSupplierAddOpen(false);
      setSupplierDetailOpen(true);
      showToast(
        driver.detection.creation === "on_add"
          ? tr("supplier.driverAdded", {
              source: sourceDriverTitle(driver),
              count: channel.models.length,
            })
          : tr("supplier.driverConfigure", { source: sourceDriverTitle(driver) }),
        driver.detection.creation === "on_add" ? "success" : "info",
        5200,
      );
    }

  async function addPresetChannel(driver: SourceDriver) {
      await addPresetChannelWithConnection(driver);
    }

  async function addLanSharePresetChannel(driver: SourceDriver, connection?: LanShareConnectionInfo) {
      if (driver.id !== "lan_share") return;
      await addPresetChannelWithConnection(driver, connection);
    }

    async function copyValue(field: CopyField, label: string, value: string) {
      if (!value) {
        showToast(tr("clipboard.emptyField", { label }), "error", 2600, false);
        return false;
      }
      try {
        await navigator.clipboard.writeText(value);
        if (copyTimer.current) {
          window.clearTimeout(copyTimer.current);
        }
        setCopiedField(field);
        copyTimer.current = window.setTimeout(() => {
          setCopiedField(null);
          copyTimer.current = null;
        }, 1200);
        return true;
      } catch (error) {
        showToast(tr("clipboard.copyFailed", { error: localizedError(error) }), "error", 4200, false);
        return false;
      }
    }

  async function copyText(value: string, message: string) {
      if (!value) {
        showToast(tr("clipboard.emptyContent"), "error", 2600, false);
        return false;
      }
      try {
        await navigator.clipboard.writeText(value);
        showToast(message, "success", 2600, false);
        return true;
      } catch (error) {
        showToast(tr("clipboard.copyFailed", { error: localizedError(error) }), "error", 4200, false);
        return false;
      }
    }

  async function openExternalUrl(url: string) {
      try {
        await invoke<void>("open_external_url", { url });
      } catch (error) {
        showToast(tr("clipboard.openLinkFailed", { error: localizedError(error) }), "error", 4200);
      }
    }

  return {
    tab,
    setTab,
    config,
    setConfig,
    status,
    supplierStatus,
    account,
    activeCallLog,
    selectedCallRecord,
    setSelectedCallRecord,
    toolConfigStatuses,
    toolConfigChecking,
    toolProtocolDraftTouchedRef,
    toolProtocolSelections,
    codexModelSource,
    changeCodexModelSource,
    setToolProtocolSelections,
    claudeModelSettings,
    claudeModelDraft,
    setClaudeModelDraft,
    toolLaunching,
    toolOperationLabels,
    toolOperationProgress,
    codexSessionScan,
    toolLocator,
    setToolLocator,
    showMissingToolCandidates,
    setShowMissingToolCandidates,
    subscriptionWizardProvider,
    subscriptionWizardOpen,
    subscriptionOAuthSession,
    subscriptionCallbackInput,
    setSubscriptionCallbackInput: (value: string) => replaceSubscriptionDraftInput("callback", value),
    subscriptionTokenJson,
    setSubscriptionTokenJson: (value: string) => replaceSubscriptionDraftInput("json", value),
    subscriptionManualOpen,
    setSubscriptionManualOpen,
    subscriptionWizardBusy,
    subscriptionWizardProgress,
    subscriptionImportCanRetry: subscriptionImportRetryChannel !== null,
    supplierAddOpen,
    setSupplierAddOpen,
    supplierDetailOpen,
    supplierStartingChannelId,
    pendingSupplierChannelIds,
    showAccessAdvanced,
    setShowAccessAdvanced,
    accessGuideOpen,
    setAccessGuideOpen,
    localApiConnectionId,
    setLocalApiConnectionId,
    modelAliasPreviewOpen,
    setModelAliasPreviewOpen,
    modelCompatibilityPolicy,
    modelCompatibilityDraft,
    setModelCompatibilityDraft,
    modelCompatibilitySaving,
    showSupplierAdvanced,
    setShowSupplierAdvanced,
    showSupplierSurfaceBindings,
    setShowSupplierSurfaceBindings,
    showSupplierProtocolDeclaration,
    setShowSupplierProtocolDeclaration,
    debugConsoleOpen,
    setDebugConsoleOpen,
    debugTargetType,
    expandedDebugTargetGroups,
    debugInboundProtocol,
    debugTargetProtocol,
    setDebugTargetProtocol,
    setDebugModel,
    debugPrompt,
    setDebugPrompt,
    debugStream,
    setDebugStream,
    debugSkipLocalShortCircuit,
    setDebugSkipLocalShortCircuit,
    debugClaudeServerSideCompaction,
    changeDebugClaudeServerSideCompaction,
    debugResult,
    debugRunning,
    routePlanState,
    debugEndpointRefreshing,
    debugEndpointLastRefresh,
    debugEndpointError,
    supplierTestPrompt,
    setSupplierTestPrompt,
    supplierTestResult,
    updateState,
    autoInstallUpdates,
    setAutoInstallUpdates,
    autostartEnabled,
    autostartBusy,
    changeAutostart,
    developmentEndpointBusy,
    changeDevelopmentEndpoint,
    themePreference,
    setThemePreference,
    appMeta,
    toast,
    showToast,
    dismissToast,
    guidanceNotices,
    showGuidanceNotice,
    dismissGuidanceNotice: dismissGuidanceNoticeByKey,
    confirmDialog,
    toolRemoveDialog,
    channelDuplicateDialog,
    appLogs,
    setAppLogs,
    logOpen,
    setLogOpen,
    copiedField,
    hasChanges,
    configActionBusy,
    activeDiagnostics,
    hasDiagnostics,
    localEntryIssue,
    brandVersion,
    updateReady,
    selectedLocalApiConnection,
    localApiRoot,
    localBaseUrl,
    localOpenAiChatUrl,
    localAnthropicBaseUrl,
    localGeminiBaseUrl,
    channels,
    displayChannels,
    selectedChannel,
    selectedApiFormat,
    selectedModelOptions,
    selectedChannelKindLabel,
    selectedChannelFields,
    selectedPrimaryProtocolOptions,
    selectedSurfaceBindings,
    selectedSurfaceEditorRows,
    selectedChannelModel,
    selectedChannelCanDetect,
    selectedChannelCanSupply,
    selectedChannelHealth,
    selectedChannelProtocolLatencyHealth,
    selectedChannelQuotaHealth,
    selectedChannelRouteStatus,
    selectedChannelRouteLabel,
    selectedChannelSwitchOn,
    selectedChannelHasChanges,
    selectedChannelOperation,
    selectedChannelBusy,
    supplierLocalReadyCount,
    supplierPlatformOnlineCount,
    selectedChannelLifecycle,
    supplierTotalCount,
    debugTargetGroups,
    selectedDebugTarget,
    effectiveDebugTargetId,
    debugModelOptions,
    effectiveDebugModel,
    debugResponseContent,
    debugRawResponse,
    debugMetrics,
    routePlanUnavailableReason,
    routePlanSourceText,
    debugEndpointStatusText,
    recommendedDebugTargetProtocol,
    debugConversionText,
    toolCards,
    visibleToolCards,
    hiddenToolCards,
    hideToolCard,
    showToolCard,
    resetToolOrder,
    subscriptionCards,
    apiAccessCards,
    lanShareCards,
    customCards,
    logText,
    handleDiagnosticAction,
    refreshModelCompatibilityPolicy,
    saveModelCompatibilityPolicy,
    resetModelCompatibilityPolicy,
    save,
    start,
    stop,
    restart,
    cancelChanges,
    changeLanAccess,
    closeSupplierDetail,
    openSupplierChannel,
    moveLocalChannel,
    openPlatformCallLog,
    openLocalSupplierCallLog,
    closeCallLog,
    refreshAccount,
    checkAndDownloadUpdate,
    installPreparedUpdate,
    updateSupplier,
    updateSupplierSurfaceBinding,
    toggleSupplierSurfaceProtocol,
    selectCustomSupplierDefaultProtocol,
    addSupplierOperationOverride,
    updateSupplierOperationOverride,
    removeSupplierOperationOverride,
    removeSelectedChannel,
    supplierChannelNeedsDetection,
    saveSupplierChanges,
    startSupplier,
    stopSelectedChannel,
    refreshSupplierModels,
    refreshRoutePlan,
    runProtocolDebug,
    testSupplierUpstream,
    selectDebugTarget,
    updateDebugInboundProtocol,
    toggleDebugTargetGroup,
    askConfirm,
    resolveConfirmDialog,
    resolveToolRemoveDialog,
    resolveChannelDuplicateDialog,
    applyToolConfig,
    startToolProgram,
    launchToolConfig,
    removeToolConfig,
    openToolLocator,
    chooseToolProgram,
    saveToolProgram,
    createSubscriptionOAuth,
    exchangeSubscriptionOAuth,
    importSubscriptionOAuthFromClipboard,
    importSubscriptionJson,
    closeSubscriptionWizard,
    addSubscriptionPreset,
    addPresetChannel,
    addLanSharePresetChannel,
    copyValue,
    copyText,
    openExternalUrl,
  };
}
