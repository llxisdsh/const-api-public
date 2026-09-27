import { tr } from "./i18n";
import {
  TOOL_CATALOG,
  type ToolModelSyncPolicy,
  type ToolProtocolId,
} from "./toolCatalog";

export type ToolConfigTone = "ready" | "pending" | "error" | "missing" | "unknown";
export type CodexModelSource = "codex" | "const";

export function codexModelSourceFromStatus(status: ToolConfigStatusLike | null | undefined): CodexModelSource {
  return status?.details?.codex_model_source === "const" ? "const" : "codex";
}
export type { ToolModelSyncPolicy, ToolProtocolId } from "./toolCatalog";

export const TOOL_PROTOCOL_OPTIONS: Record<ToolProtocolId, { label: string; surface: string }> = {
  openai_responses: { label: "Responses", surface: "/v1" },
  openai_chat: { label: "Chat", surface: "/v1" },
  anthropic_messages: { label: "Messages", surface: "/anthropic" },
  gemini_native: { label: "Gemini", surface: "/gemini" },
};

export const TOOL_PROTOCOLS_BY_TOOL: Record<string, readonly ToolProtocolId[]> =
  Object.fromEntries(Object.values(TOOL_CATALOG).map((tool) => [tool.tool, tool.protocols]));

export const DEFAULT_TOOL_PROTOCOLS: Record<string, ToolProtocolId> =
  Object.fromEntries(Object.values(TOOL_CATALOG).map((tool) => [tool.tool, tool.defaultProtocol]));

export function toolProtocolId(value: string | undefined): ToolProtocolId | null {
  if (value && Object.prototype.hasOwnProperty.call(TOOL_PROTOCOL_OPTIONS, value)) {
    return value as ToolProtocolId;
  }
  return null;
}

export function toolProtocolMenuState(
  tool: string,
  selectedProtocol?: ToolProtocolId | null,
  configuredProtocol?: ToolProtocolId | null,
) {
  const protocols = TOOL_PROTOCOLS_BY_TOOL[tool] ?? [DEFAULT_TOOL_PROTOCOLS[tool] ?? "openai_chat"];
  const selected = selectedProtocol && protocols.includes(selectedProtocol)
    ? selectedProtocol
    : DEFAULT_TOOL_PROTOCOLS[tool] ?? protocols[0];
  const configured = configuredProtocol && protocols.includes(configuredProtocol)
    ? configuredProtocol
    : null;
  const fixed = protocols.length <= 1;
  const changed = Boolean(configured && configured !== selected);
  const suffix = changed
    ? ` · ${tr("labels.toolMenu.pendingApply")}`
    : fixed
      ? ` · ${tr("labels.toolMenu.fixed")}`
      : "";
  return {
    protocols,
    selected,
    configured,
    fixed,
    changed,
    summary: `${TOOL_PROTOCOL_OPTIONS[selected].label}${suffix}`,
  };
}

type ToolConfigStatusLike = {
  already_configured?: boolean;
  files?: string[];
  details?: Record<string, string>;
  file_statuses?: Array<{
    path: string;
    already_configured?: boolean;
  }>;
};

export type ToolPrimaryAction = "locate" | "configure_and_launch" | "launch";

export function toolPrimaryAction(
  configStatus: ToolConfigStatusLike | null | undefined,
  configurationChanged = false,
): ToolPrimaryAction {
  if (configStatus?.details?.program_located === "false") return "locate";
  if (!configStatus?.already_configured || configurationChanged) return "configure_and_launch";
  return "launch";
}

export function toolConfigurationActionKey(
  configStatus: ToolConfigStatusLike | null | undefined,
): "access.configure" | "access.reconfigure" {
  return configStatus?.already_configured ? "access.reconfigure" : "access.configure";
}

export function toolSyncsModelsOnLaunch(
  configStatus: ToolConfigStatusLike | null | undefined,
): boolean {
  const policy = configStatus?.details?.model_sync_policy as ToolModelSyncPolicy | undefined;
  return policy === "selected" || policy === "catalog";
}

export function toolConfigDetailState(
  configStatus: ToolConfigStatusLike | null | undefined,
  checking: boolean,
  configurationChanged = false,
) {
  const paths = Array.from(new Set([
    ...(configStatus?.file_statuses?.map((file) => file.path) ?? []),
    ...(configStatus?.files ?? []),
  ].filter(Boolean)));
  const programMissing = configStatus?.details?.program_located === "false";
  const externalLaunchWarning = configStatus?.details?.external_launch_warning;
  const status = configStatus === undefined
    ? checking ? tr("labels.toolMenu.checking") : tr("labels.toolMenu.waitingCheck")
    : configStatus === null
      ? tr("labels.toolMenu.checkFailed")
      : programMissing
        ? tr("labels.toolMenu.programMissing")
        : configurationChanged
          ? tr("labels.toolMenu.configurationPending")
          : configStatus?.already_configured
            ? externalLaunchWarning
              ? tr("labels.toolMenu.configuredRisk")
              : tr("labels.toolMenu.configured")
            : tr("labels.toolMenu.needsConfiguration");
  const tone: ToolConfigTone = configStatus === undefined
    ? "unknown"
    : configStatus === null
      ? "error"
      : programMissing
      ? "missing"
      : configStatus?.already_configured && !configurationChanged
        ? "ready"
        : "pending";
  return {
    status,
    tone,
    primaryPath: paths[0] ?? tr("labels.toolMenu.fileMissing"),
    fileCount: paths.length,
    externalLaunchWarning,
  };
}
