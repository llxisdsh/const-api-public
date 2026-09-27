import { modelDisplayName } from "./modelPresentation";
import type {
  ApiSurface,
  AppMetaState,
  ChannelCapabilityProfile,
  ChannelConfig,
  ChannelDetectionResult,
  ChannelSurfaceBinding,
  ChannelUpstreamTestResult,
  ClientConfig,
  ClientConfigV5Wire,
  DebugProtocol,
  DebugTargetOption,
  DebugTargetType,
  Endpoint,
  ModelAliasPreviewRow,
  ModelCatalogVersionInfo,
  ModelCompatibilityGroup,
  PlatformProvider,
  PlatformSupplierNode,
  ProtocolVerification,
  ProtocolDebugResult,
  ProxyStatus,
  RouteCandidate,
  RouteDecision,
  RouteNodeSnapshot,
  SubscriptionAdapterConfig,
  SubscriptionProvider,
  SupplierChannelHealth,
  SupplierConfig,
  SupplierNodeRouteStatus,
  SupplierStatus,
  SurfaceVerification,
  Toast,
  UpdateState
} from "./appTypes";
import type { DiagnosticIssue } from "./diagnostics";
import { currentAppLanguage, tr } from "./i18n";
import { DEFAULT_SUPPLIER_MAX_CONCURRENCY, MAX_PRICE_RATIO } from "./pricingPolicy";
import {
  DEFAULT_DEVELOPMENT_ENDPOINT,
  DEFAULT_LOCAL_PROXY_LISTEN,
} from "./runtimeProfile";
import {
  applyChannelDetectionV2,
  normalizeChannelV5,
  reloadChannelV5,
  sourceDriverById,
  sourceDriverInitialChannelV5,
  type ChannelConfigV5,
  type SourceDriverId,
} from "./sourceDrivers";
import { reconcileDetectedDefaultModel } from "./supplierEditorState";

export const emptyConfig: ClientConfig = {
  config_version: 1,
  platform_id: "",
  account_platform_id: "",
  account_user_id: "",
  account_email: "",
  listen: DEFAULT_LOCAL_PROXY_LISTEN,
  allow_lan_access: false,
  lan_share: {
    shared_models: [],
    preferred_connect_ip: "",
  },
  proxy_auto_start: true,
  automatic_updates: true,
  supplier_auto_start: false,
  api_key: "",
  registry_sources: [],
  registry_signature_secret: "",
  registry_public_keys: [],
  registry_version: 0,
  development_endpoint: DEFAULT_DEVELOPMENT_ENDPOINT,
  allow_model_equivalence: true,
  prefer_local_supply: true,
  allow_unverified_platform_routes: true,
  model_aliases: {},
  endpoints: [],
  channels: [],
  supplier: {
    user_agent_profile: "",
    enabled: false,
    kind: "openai_compatible",
    api_format: "openai_chat",
    node_id: "local-supplier",
    name: "Local Supplier",
    server_ws_url: "",
    server_quic_url: "",
    upstream_base_url: "http://127.0.0.1:8317",
    upstream_api_key: "",
    public_model: "",
    upstream_model: "",
    models: [],
    supported_protocols: ["openai_chat"],
    capability_profiles: [],
    detection_checks: [],
    surface_bindings: [],
    price_ratio: 1,
    subscription: {
      platform: "claude",
      account_label: "",
      credential_ref: "",
      max_concurrency: DEFAULT_SUPPLIER_MAX_CONCURRENCY,
      responses_ws_pool_enabled: true,
      responses_ws_subscription_multiplex_probe_enabled: false,
      rpm_limit: 0,
      quota_reserve_percent: 0,
      daily_request_limit: 0,
      cooldown_until_unix: 0,
      risk_note: "",
      audit_enabled: false,
    },
  },
};

export function generateLocalApiKey(): string {
  const bytes = new Uint8Array(16);
  globalThis.crypto.getRandomValues(bytes);
  const secret = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
  return `sk-api-${secret}`;
}

export function localListenPort(listen: string): string | null {
  const match = listen.trim().match(/:(\d+)$/);
  return match?.[1] ?? null;
}

export function localLoopbackApiRoot(listen: string): string | null {
  const port = localListenPort(listen);
  return port ? `http://127.0.0.1:${port}` : null;
}

export function localListenAddressForLanAccess(listen: string, enabled: boolean): string | null {
  const port = localListenPort(listen);
  return port ? `${enabled ? "0.0.0.0" : "127.0.0.1"}:${port}` : null;
}

export function modelAliasPreviewRows(aliases: Record<string, string>): ModelAliasPreviewRow[] {
  return Object.entries(aliases)
    .map(([alias, value]) => ({
      alias: alias.trim(),
      candidates: value.split("|").map((candidate) => candidate.trim()).filter(Boolean),
    }))
    .filter((row) => row.alias && row.candidates.length > 0)
    .sort((left, right) => left.alias.localeCompare(right.alias));
}

export function modelCompatibilityGroupsFromPayload(value: unknown): ModelCompatibilityGroup[] {
  if (!Array.isArray(value)) return [];
  const groups: ModelCompatibilityGroup[] = [];
  for (const item of value) {
    if (!item || typeof item !== "object" || Array.isArray(item)) continue;
    const payload = item as Record<string, unknown>;
    const id = typeof payload.id === "string" ? payload.id.trim() : "";
    const label = typeof payload.label === "string" ? payload.label.trim() : id;
    const description = typeof payload.description === "string" ? payload.description.trim() : "";
    const aliases = Array.isArray(payload.aliases)
      ? payload.aliases.filter((alias): alias is string => typeof alias === "string").map((alias) => alias.trim()).filter(Boolean)
      : [];
    const models = Array.isArray(payload.models)
      ? payload.models.filter((model): model is string => typeof model === "string").map((model) => model.trim()).filter(Boolean)
      : [];
    const matchModels = Array.isArray(payload.match_models)
      ? payload.match_models.filter((model): model is string => typeof model === "string").map((model) => model.trim()).filter(Boolean)
      : [];
    const disabled = payload.disabled === true;
    if (!id || (models.length === 0 && !disabled)) continue;
    groups.push({ id, label, description, aliases, models, match_models: matchModels, disabled });
  }
  return groups;
}

export function isSubscriptionAdapter(kind: string) {
  return kind === "subscription_adapter";
}

export function channelFieldProfile(kind: string, sourceDriverId: SourceDriverId) {
  const normalizedKind = kind.trim().toLowerCase();
  const sourceDriver = sourceDriverById(sourceDriverId);
  return {
    showSubscriptionFields: normalizedKind === "subscription_adapter",
    showBaseUrl: normalizedKind !== "subscription_adapter",
    showApiKey: normalizedKind !== "subscription_adapter" && normalizedKind !== "local_model",
    baseUrlReadOnly: sourceDriver.id !== "custom_endpoint"
      && sourceDriver.id !== "lan_share"
      && sourceDriver.category !== "local_runtime",
  };
}

export function channelPrimaryProtocolOptions(kind: string, supportedProtocols: readonly string[] = []) {
  const normalizedKind = kind.trim().toLowerCase();
  if (normalizedKind === "openai_compatible") {
    const labels: Record<string, string> = {
      openai_responses: "OpenAI Responses",
      openai_chat: "OpenAI Chat Completions",
      anthropic_messages: "Anthropic Messages",
      gemini_native: "Gemini Native",
    };
    const declaredOptions = [...new Set(supportedProtocols)]
      .filter((protocol) => labels[protocol])
      .map((value) => ({ value, label: labels[value] }));
    if (declaredOptions.length > 0) return declaredOptions;
  }
  if (["openai", "azure", "azure_openai"].includes(normalizedKind)) {
    return [
      { value: "openai_responses", label: "OpenAI Responses" },
      { value: "openai_chat", label: "OpenAI Chat Completions" },
    ];
  }
  if (normalizedKind === "anthropic") {
    return [{ value: "anthropic_messages", label: "Anthropic Messages" }];
  }
  if (normalizedKind === "gemini") {
    return [
      { value: "gemini_native", label: "Gemini Native" },
      { value: "gemini_openai", label: "Gemini OpenAI Chat" },
    ];
  }
  if (normalizedKind === "aws_bedrock") {
    return [
      { value: "openai_responses", label: "OpenAI Responses" },
      { value: "openai_chat", label: "OpenAI Chat Completions" },
      { value: "anthropic_messages", label: "Anthropic Messages" },
    ];
  }
  if (["openrouter", "local_model"].includes(normalizedKind)) {
    return [{ value: "openai_chat", label: "OpenAI Chat Completions" }];
  }
  return [
    { value: "openai_responses", label: "OpenAI Responses" },
    { value: "openai_chat", label: "OpenAI Chat Completions" },
    { value: "anthropic_messages", label: "Anthropic Messages" },
    { value: "gemini_native", label: "Gemini Native" },
  ];
}

export function sanitizeApiFormat(kind: string, apiFormat?: string) {
  if (isSubscriptionAdapter(kind)) {
    return "subscription_skeleton";
  }
  if (apiFormat === "openai_responses") return "openai_responses";
  if (apiFormat === "anthropic_messages") return "anthropic_messages";
  if (apiFormat === "gemini_native") return "gemini_native";
  if (apiFormat === "gemini_openai") return "gemini_openai";
  if (apiFormat === "openai_chat") return "openai_chat";
  return apiFormat?.trim() || "openai_chat";
}

export function pickPassthroughModel(models: string[], upstreamModel: string, publicModel: string) {
  const upstreamKey = upstreamModel.trim().toLowerCase();
  const publicKey = publicModel.trim().toLowerCase();
  if (upstreamKey) {
    const observed = models.find((model) => model.trim().toLowerCase() === upstreamKey);
    if (observed) return observed;
  }
  if (publicKey) {
    const observed = models.find((model) => model.trim().toLowerCase() === publicKey);
    if (observed) return observed;
  }
  return models[0] || upstreamModel || publicModel || "";
}

export function normalizeModelList(models: string[] = [], publicModel: string, upstreamModel: string) {
  if (models.length > 0) {
    return dedupeModels(models);
  }
  return dedupeModels([publicModel, upstreamModel]);
}

export function dedupeModels(models: string[]) {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const model of models) {
    const value = model.trim();
    const key = value.toLowerCase();
    if (!key || seen.has(key)) continue;
    seen.add(key);
    out.push(value);
  }
  return out;
}

export function codexAccountLabel(value?: string) {
  if (value === "official") return tr("labels.officialSignedIn");
  if (value === "api_key") return "API Key";
  return tr("labels.signedOut");
}

export function codexTrafficLabel(value?: string) {
  if (value === "const_api") return "CONST API";
  if (value === "openai") return tr("labels.officialTraffic");
  if (value === "other") return tr("labels.other");
  return tr("labels.unknown");
}

export function codexSessionMigrationSummary(details?: Record<string, string>) {
  if (!details) return "";
  const files = Number(details.session_migrated_files ?? 0);
  const rows = Number(details.session_migrated_sqlite_rows ?? 0);
  const sqliteError = details.session_migrated_sqlite_error;
  const targetProvider = details.session_target_provider;
  const parts: string[] = [];
  if (files > 0 || rows > 0) {
    parts.push(tr(targetProvider ? "labels.sessionMigrationTo" : "labels.sessionMigration", {
      files,
      rows,
      target: targetProvider,
    }));
  }
  if (sqliteError) {
    parts.push(tr("labels.sessionIndexFailed", { error: sqliteError }));
  }
  return parts.length > 0 ? `; ${parts.join("; ")}` : "";
}

export function channelKindLabel(kind: string) {
  if (kind === "openai") return tr("labels.channelKind.openai");
  if (kind === "openrouter") return "OpenRouter";
  if (kind === "azure_openai") return "Microsoft Foundry / Azure OpenAI";
  if (kind === "anthropic") return "Claude / Anthropic";
  if (kind === "gemini") return "Gemini";
  if (kind === "aws_bedrock") return "Amazon Bedrock";
  if (kind === "local_model") return tr("labels.channelKind.localModel");
  if (kind === "custom_endpoint") return tr("labels.channelKind.customEndpoint");
  if (kind === "subscription_adapter") return tr("labels.channelKind.subscription");
  return tr("labels.channelKind.compatible");
}

export function providerToChannelKind(provider: string) {
  if (provider === "openai") return "openai";
  if (provider === "anthropic") return "anthropic";
  if (provider === "gemini") return "gemini";
  if (provider === "antigravity") return "subscription_adapter";
  if (provider === "grok") return "subscription_adapter";
  if (provider === "ollama" || provider === "lmstudio" || provider === "vllm" || provider === "local_model") return "local_model";
  if (provider.startsWith("subscription_")) return "subscription_adapter";
  return "openai_compatible";
}

export function detectedChannelKind(currentKind: string, provider: string) {
  if (currentKind === "custom_endpoint") {
    return currentKind;
  }
  const detected = providerToChannelKind(provider);
  if (detected === "openai_compatible" && ["openrouter", "azure_openai", "aws_bedrock", "custom_endpoint"].includes(currentKind)) {
    return currentKind;
  }
  return detected;
}

export function applyChannelDetection(channel: ChannelConfig, detection: ChannelDetectionResult): ChannelConfig {
  if (detection.models.length === 0 && channel.source_driver !== "openrouter") {
    throw new Error(tr("errors.codes.empty_model_catalog"));
  }
  const merged = applyChannelDetectionV2(channelV5FromRenderer(channel), detection);
  const defaultModel = merged.default_model;
  const projected = projectRendererChannel({
    ...channel,
    ...merged,
    surface_bindings: merged.surfaces,
    capability_profiles: merged.model_capability_evidence as ChannelCapabilityProfile[],
    detection_checks: merged.detection_evidence,
    subscription: channel.subscription,
  });
  return {
    ...projected,
    kind: channel.kind,
    api_format: channel.api_format,
    supported_protocols: [...channel.supported_protocols],
    upstream_base_url: channel.upstream_base_url,
    upstream_api_key: channel.upstream_api_key,
    default_model: defaultModel,
    upstream_model: defaultModel,
    public_model: defaultModel,
  };
}

export function protocolLabel(apiFormat: string) {
  if (apiFormat === "openai_responses") return "OpenAI Responses";
  if (apiFormat === "openai_chat") return "OpenAI Chat";
  if (apiFormat === "anthropic_messages") return "Anthropic Messages";
  if (apiFormat === "gemini_native") return "Gemini Native";
  if (apiFormat === "gemini_openai") return "Gemini OpenAI Compat";
  if (apiFormat === "bedrock_converse" || apiFormat === "bedrock") return tr("labels.protocol.removedBedrock");
  if (apiFormat === "subscription_skeleton") return tr("labels.protocol.subscription");
  return apiFormat || "-";
}

export function transportLabel(transport: string) {
  if (transport === "quic") return "QUIC";
  if (transport === "websocket") return "WebSocket";
  if (transport === "http3") return "HTTP/3";
  if (transport === "http2") return "HTTP/2";
  if (transport === "http1") return "HTTP/1.1";
  if (transport === "http") return "HTTP";
  if (transport === "starting") return tr("labels.transport.checking");
  if (transport === "reconnecting") return tr("labels.transport.reconnecting");
  return "-";
}

export function supplierProtocolLatencyLabel(
  health?: Pick<SupplierChannelHealth, "upstream_http_version" | "latency_ms">,
) {
  const { primary: protocol, secondary: latency } = supplierProtocolLatencyParts(health);
  return [protocol, latency].filter((value) => value !== "-").join(" · ") || "-";
}

export function supplierProtocolLatencyParts(
  health?: Pick<SupplierChannelHealth, "upstream_http_version" | "latency_ms">,
) {
  return {
    primary: transportLabel(health?.upstream_http_version?.trim() || ""),
    secondary: typeof health?.latency_ms === "number" && health.latency_ms > 0
      ? `${Math.round(health.latency_ms)}ms`
      : "-",
  };
}

export function applySupplierUpstreamObservation(
  status: SupplierStatus,
  channelId: string,
  observation: Pick<ChannelUpstreamTestResult, "upstream_http_version" | "latency_ms">,
) {
  const version = observation.upstream_http_version?.trim() || "";
  if (!status.channels?.length) return status;
  let changed = false;
  const channels = status.channels.map((health) => {
    if (health.channel_id !== channelId) return health;
    const nextVersion = version || health.upstream_http_version || "";
    if (
      health.latency_ms === observation.latency_ms
      && (health.upstream_http_version || "") === nextVersion
    ) {
      return health;
    }
    changed = true;
    return {
      ...health,
      latency_ms: observation.latency_ms,
      upstream_http_version: nextVersion,
    };
  });
  return changed ? { ...status, channels } : status;
}

export function endpointDisplayName(endpoint?: Endpoint) {
  if (!endpoint) return tr("labels.endpoint.notConfigured");
  const name = endpoint.name?.trim();
  if (name) return name;
  const url = endpoint.base_url?.trim();
  if (!url) return tr("labels.endpoint.unnamed");
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

export function configuredEndpointName(config: ClientConfig) {
  const endpoint = config.endpoints.find((item) => item.enabled) ?? config.endpoints[0];
  return endpointDisplayName(endpoint);
}

export function endpointConnectionName(status: ProxyStatus, config: ClientConfig) {
  return status.active_endpoint?.trim() || configuredEndpointName(config);
}

export function aboutServerLabel(status: ProxyStatus, config: ClientConfig) {
  const endpoint = endpointConnectionName(status, config);
  const version = status.platform_server_version?.trim().replace(/^v/, "");
  return version ? `v${version} · ${endpoint}` : endpoint;
}

export function aboutUpdateHint(state: UpdateState, lastCheckedAt?: string) {
  if (state.status === "ready" && state.version) return tr("about.finishAfterRestart");
  if (state.status === "idle") {
    return state.checkedAt || lastCheckedAt ? "" : tr("about.clickCheck");
  }
  return updateLatestHint(state);
}

export function isEstablishedSupplierTransport(transport?: string) {
  return transport === "quic" || transport === "websocket";
}

export function buildRuntimeDiagnostics(
  proxyStatus: ProxyStatus,
  supplierStatus: SupplierStatus,
  meta: AppMetaState,
  config: ClientConfig,
  localEntryDiagnosticReady: boolean,
): Omit<DiagnosticIssue, "createdAt">[] {
  const issues: Omit<DiagnosticIssue, "createdAt">[] = [];

  if (localEntryDiagnosticReady && config.proxy_auto_start && !proxyStatus.running) {
    const localRoot = localLoopbackApiRoot(config.listen);
    issues.push({
      id: "local-entry",
      scope: "access",
      severity: "error",
      title: tr("diagnostics.localEntryStopped"),
      message: localRoot
        ? tr("diagnostics.toolEntryStopped", { root: localRoot })
        : tr("diagnostics.listenAddressInvalid", {
          address: config.listen || tr("common.notConfigured"),
        }),
      action: "start_proxy",
    });
  }

  for (const routeStatus of supplierStatus.route_statuses ?? []) {
    if (routeStatus.reason !== "model_not_supported" && routeStatus.reason !== "pricing_fallback") continue;
    const channelId = routeStatus.channel_id?.trim() || routeStatus.supplier_unit_id?.trim() || routeStatus.node_id?.trim() || "unknown";
    const channelName = config.channels.find((channel) => channel.id === routeStatus.channel_id)?.name || channelId;
    const unsupportedCount = routeStatus.unsupported_models?.length ?? 0;
    const noneAccepted = routeStatus.reason === "model_not_supported";
    issues.push({
      id: `supplier-model-catalog-${channelId}`,
      scope: "supplier",
      severity: "warning",
      title: noneAccepted
        ? tr("diagnostics.channelNoModels", { channel: channelName })
        : tr("diagnostics.channelBillingFallback", { channel: channelName }),
      message: noneAccepted
        ? tr("diagnostics.unsupportedModels", { count: unsupportedCount })
        : supplierPricingFallbackSummary(routeStatus),
      action: "open_supplier",
    });
  }

  if (meta.endpointStatus === "error" && meta.endpointLastError) {
    issues.push({
      id: "endpoint-registry",
      scope: "about",
      severity: "error",
      title: tr("diagnostics.endpointSourceFailed"),
      message: meta.endpointLastError,
      action: "refresh_registry",
    });
  }

  if (meta.updateLastError) {
    issues.push({
      id: "update-source",
      scope: "about",
      severity: "warning",
      title: tr("diagnostics.updateSourceFailed"),
      message: meta.updateLastError,
      action: "check_update",
    });
  }

  return issues;
}

export function endpointNetworkState(
  proxyStatus: ProxyStatus,
) {
  if (!proxyStatus.running) return tr("labels.network.onDemand");
  const connectedTransport = proxyStatus.platform_transport?.trim();
  if (connectedTransport) {
    const protocol = transportLabel(connectedTransport);
    if (proxyStatus.platform_transport_error && connectedTransport !== "http3") {
      return tr("labels.network.fallback", { protocol });
    }
    return tr("labels.network.connected", { protocol });
  }
  if (proxyStatus.platform_connection_state === "probing") return tr("labels.network.probing");
  if (proxyStatus.platform_connection_state === "degraded") return tr("labels.network.degraded");
  if (proxyStatus.platform_connection_state === "failed") return tr("labels.network.failed");
  return tr("labels.network.onDemand");
}

export function supplierNetworkState(status: SupplierStatus) {
  if (!status.running && !status.starting) return tr("labels.supplierNetwork.stopped");

  const activeTransport = status.active_transport?.trim();
  if (isEstablishedSupplierTransport(activeTransport)) {
    return tr("labels.supplierNetwork.connected", {
      protocol: transportLabel(activeTransport),
    });
  }

  const connectedTransport = status.connected_transport?.trim();
  if (isEstablishedSupplierTransport(connectedTransport)) {
    return tr("labels.supplierNetwork.confirming", {
      protocol: transportLabel(connectedTransport),
    });
  }

  if (activeTransport === "reconnecting" || connectedTransport === "reconnecting") {
    return tr("labels.supplierNetwork.reconnecting");
  }
  return tr("labels.supplierNetwork.connecting");
}

export function updateStatusLabel(state: UpdateState) {
  switch (state.status) {
    case "checking":
      return tr("labels.update.checking");
    case "downloading":
      return tr("labels.update.downloading");
    case "ready":
      return tr("labels.update.ready");
    case "ready_waiting_idle":
      return tr("labels.update.waiting");
    case "installing":
      return tr("labels.update.installing");
    case "error":
      return tr("labels.update.failed");
    default:
      return tr("labels.update.enabled");
  }
}

export function updateLatestHint(state: UpdateState) {
  switch (state.status) {
    case "checking":
      return tr("labels.update.confirming");
    case "downloading":
      return tr("labels.update.backgroundDownload");
    case "ready_waiting_idle":
      return state.waitingReason || tr("labels.update.waitingHint");
    case "installing":
      return tr("labels.update.applying");
    case "error":
      return state.error || tr("labels.update.recentFailure");
    default:
      return tr("labels.update.current");
  }
}

export function sourceSummary(sources: string[]) {
  if (sources.length === 0) return tr("labels.source.notConfigured");
  if (sources.length === 1) return compactSourceLabel(sources[0]);
  return tr("labels.source.count", { count: sources.length });
}

export function compactSourceLabel(source: string) {
  if (source === "local-cache") return tr("labels.source.localCache");
  if (source === "embedded") return tr("labels.source.embedded");
  if (source === "local-dev") return tr("labels.source.localDev");
  if (source === "github-release") return "GitHub";
  try {
    const url = new URL(source);
    if (url.hostname === "volces.com" || url.hostname.endsWith(".volces.com")) return "TOS";
    if (url.hostname === "github.com"
      || url.hostname === "githubusercontent.com"
      || url.hostname.endsWith(".githubusercontent.com")) {
      return "GitHub";
    }
    return url.hostname;
  } catch {
    return source || tr("labels.source.notConfigured");
  }
}

function deliverySources(selected: string | undefined, sources: string[]) {
  return [...new Set(
    [...sources, selected ?? ""]
      .map((source) => source.trim())
      .filter(Boolean),
  )];
}

function deliveryChannelLabel(count: number) {
  return count > 0
    ? tr("labels.source.channelCount", { count })
    : tr("labels.source.notConfigured");
}

function availableDeliveryChannelCount(selected: string | undefined, sources: string[]) {
  const configured = deliverySources(undefined, sources)
    .filter((source) => !["embedded", "packaged", "local-cache"].includes(source.toLowerCase()));
  if (configured.length > 0) return configured.length;
  const normalized = selected?.trim().toLowerCase();
  return normalized && !["embedded", "packaged", "local-cache"].includes(normalized) ? 1 : 0;
}

type DeliveryChannelRole = "primary" | "fallback" | "cache" | "bundled";

function selectedDeliveryChannelRole(
  selected: string | undefined,
  sources: string[],
): DeliveryChannelRole | undefined {
  const normalized = selected?.trim().toLowerCase();
  if (!normalized) return undefined;
  if (["embedded", "packaged"].includes(normalized)) return "bundled";
  if (normalized === "local-cache") return "cache";

  const configured = deliverySources(undefined, sources);
  const selectedIndex = configured.findIndex((source) => source.toLowerCase() === normalized);
  return selectedIndex <= 0 ? "primary" : "fallback";
}

function deliveryChannelRoleLabel(role: DeliveryChannelRole) {
  if (role === "primary") return tr("labels.source.primaryChannel");
  if (role === "fallback") return tr("labels.source.fallbackChannel");
  if (role === "cache") return tr("labels.source.cacheChannel");
  return tr("labels.source.bundledChannel");
}

function lastUsedSourceNote(role?: DeliveryChannelRole, state = "") {
  const selected = role
    ? tr("labels.source.lastUsedChannel", { channel: deliveryChannelRoleLabel(role) })
    : "";
  return [selected, state].filter(Boolean).join(" · ") || tr("labels.source.notChecked");
}

function modelCatalogRemoteChannelCount(info: ModelCatalogVersionInfo) {
  const explicitSources = deliverySources(undefined, info.delivery_sources ?? [])
    .filter((source) => !["embedded", "packaged", "local-cache"].includes(source.toLowerCase()));
  if (explicitSources.length > 0) return explicitSources.length;
  const selected = info.delivery_source?.trim().toLowerCase();
  if (selected && !["embedded", "packaged", "local-cache"].includes(selected)) return 1;
  return info.source.trim().toLowerCase() === "packaged" ? 0 : 1;
}

function modelCatalogActiveChannelRole(info: ModelCatalogVersionInfo): DeliveryChannelRole {
  return selectedDeliveryChannelRole(info.delivery_source, info.delivery_sources ?? [])
    ?? (info.source.trim().toLowerCase() === "packaged" ? "bundled" : "primary");
}

export function updateSourceLabel(meta: AppMetaState) {
  return deliveryChannelLabel(availableDeliveryChannelCount(meta.updateSource, meta.updateSources));
}

export function updateSourceNote(meta: AppMetaState) {
  return lastUsedSourceNote(
    selectedDeliveryChannelRole(meta.updateSource, meta.updateSources),
    meta.updateLastError ? tr("labels.source.refreshFailed") : "",
  );
}

export function endpointRegistryLabel(meta: AppMetaState, config: ClientConfig) {
  if (meta.endpointStatus === "checking") return tr("labels.source.refreshing");
  if (meta.endpointStatus === "success") {
    const version = meta.endpointVersion ? `v${meta.endpointVersion}` : tr("labels.source.refreshed");
    const count = meta.endpointCount === undefined ? "" : ` / ${tr("labels.source.endpointCount", { count: meta.endpointCount })}`;
    return `${version}${count}`;
  }
  return sourceSummary(meta.endpointSources.length > 0 ? meta.endpointSources : config.registry_sources);
}

export function endpointRegistryHint(meta: AppMetaState) {
  if (meta.endpointRefreshWarning && meta.endpointSource) {
    return `${compactSourceLabel(meta.endpointSource)} · ${tr("labels.source.remoteRecovery")}`;
  }
  if (meta.endpointSource) return compactSourceLabel(meta.endpointSource);
  if (meta.endpointLastCheckedAt) return meta.endpointLastCheckedAt;
  return tr("labels.source.scheduled");
}

export function endpointRegistrySourceLabel(meta: AppMetaState, config: ClientConfig) {
  const sources = meta.endpointSources.length > 0 ? meta.endpointSources : config.registry_sources;
  return deliveryChannelLabel(availableDeliveryChannelCount(meta.endpointSource, sources));
}

export function endpointRegistrySourceNote(meta: AppMetaState) {
  const state = meta.endpointStatus === "checking"
    ? tr("labels.source.refreshing")
    : meta.endpointStatus === "error"
      ? tr("labels.source.refreshFailed")
      : meta.endpointRefreshWarning
        ? tr("labels.source.remoteRecovery")
        : "";
  return lastUsedSourceNote(
    selectedDeliveryChannelRole(meta.endpointSource, meta.endpointSources),
    state,
  );
}

export function modelCatalogVersionLabel(info?: ModelCatalogVersionInfo) {
  return info?.model_version_id.trim() || tr("labels.source.reading");
}

export function modelCatalogVersionHint(meta: AppMetaState) {
  const info = meta.modelCatalog;
  if (!info) {
    return tr("labels.source.readingCurrent");
  }
  const compatibility = info.compatibility_version_id.trim();
  return compatibility
    ? tr("labels.source.compatibility", { version: compatibility })
    : tr("labels.source.compatibilityMissing");
}

export function modelCatalogSourceLabel(info?: ModelCatalogVersionInfo) {
  if (!info) return tr("labels.source.reading");
  return deliveryChannelLabel(modelCatalogRemoteChannelCount(info) + 1);
}

export function modelCatalogSourceNote(meta: AppMetaState) {
  if (!meta.modelCatalog) {
    return meta.modelCatalogLastError ? tr("labels.source.readFailed") : "";
  }
  return lastUsedSourceNote(
    modelCatalogActiveChannelRole(meta.modelCatalog),
    meta.modelCatalogLastError ? tr("labels.source.refreshFailed") : "",
  );
}

export function modelCatalogVersionDetails(info?: ModelCatalogVersionInfo) {
  if (!info) return "";
  return [
    tr("labels.source.modelVersion", { version: info.model_version_id }),
    tr("labels.source.catalogRelease", { version: info.release_id }),
    tr("labels.source.compatibilityVersion", { version: info.compatibility_version_id }),
    info.sequence ? tr("labels.source.catalogSequence", { sequence: info.sequence }) : "",
  ].filter(Boolean).join("\n");
}

export function routeNodeLabel(node?: RouteNodeSnapshot | null) {
  if (!node) return "-";
  if (node.name && node.node_id) return `${node.name} (${node.node_id})`;
  return node.node_id || node.name || "-";
}

export function routePlanStickyText(decision: RouteDecision) {
  if (decision.sticky_hit) {
    return tr("debugConsole.route.stickyHit", {
      node: decision.sticky_node_id ? ` ${decision.sticky_node_id}` : "",
    });
  }
  if (decision.sticky_miss_reason) {
    return tr("debugConsole.route.stickyMiss", { reason: decision.sticky_miss_reason });
  }
  if (decision.sticky_node_id) {
    return tr("debugConsole.route.stickyMiss", { reason: decision.sticky_node_id });
  }
  return tr("common.none");
}

export function routePlanCacheText(decision: RouteDecision) {
  const domain = decision.cache_domain_id?.trim() || "";
  const compactDomain = domain.length > 24
    ? `${domain.slice(0, 14)}…${domain.slice(-6)}`
    : domain;
  const idle = decision.sticky_idle_seconds
    ? tr("debugConsole.route.idleMinutes", {
      count: Math.floor(decision.sticky_idle_seconds / 60),
    })
    : "";
  return [
    decision.cache_route_reason || "",
    compactDomain,
    idle,
  ].filter(Boolean).join(" · ") || tr("debugConsole.route.notEstablished");
}

export function routeCandidateProtocolText(candidate: RouteCandidate) {
  const protocols = candidate.supported_protocols?.length ? candidate.supported_protocols.map(protocolLabel).join(" / ") : "-";
  const operations = candidate.supported_operations?.length
    ? ` · ${candidate.supported_operations.length} ops`
    : "";
  return `${candidate.protocol_match
    ? tr("debugConsole.route.matched")
    : tr("debugConsole.route.notMatched")} · ${protocols}${operations}`;
}

export function routeCandidateFilterText(candidate: RouteCandidate) {
  if (candidate.filter_reason) return candidate.filter_reason;
  if (candidate.selectable) return tr("debugConsole.route.selectable");
  if (!candidate.model_match) return "model_mismatch";
  if (!candidate.protocol_match) return "protocol_unsupported";
  return tr("debugConsole.route.notSelectable");
}

export function routeCandidateHealthText(candidate: RouteCandidate) {
  const health = candidate.health_status || "-";
  const quota = candidate.quota_status || "-";
  const safety = candidate.safety_state ? ` / ${candidate.safety_state}` : "";
  const signal = routeCandidateQuotaSignalText(candidate);
  const capacity = typeof candidate.effective_concurrency === "number"
    ? tr("labels.channel.concurrency", {
      value: `${candidate.current_concurrency ?? 0}/${candidate.effective_concurrency}`,
    })
    : "";
  return `${health} / ${quota}${safety}${signal ? ` · ${signal}` : ""}${capacity ? ` · ${capacity}` : ""}`;
}

export function routeCandidateQuotaSignalText(candidate: RouteCandidate) {
  const source = candidate.quota_source || "";
  if (!source || source === "unknown") return "";
  const window = candidate.quota_window ? ` ${candidate.quota_window}` : "";
  const used = typeof candidate.quota_used_percent === "number"
    ? ` ${candidate.quota_used_percent.toFixed(candidate.quota_used_percent % 1 === 0 ? 0 : 1)}%`
    : "";
  const reset = candidate.quota_reset_at_unix ? ` reset ${formatUnixCountdown(candidate.quota_reset_at_unix)}` : "";
  const windows = candidate.quota_windows && candidate.quota_windows.length > 1
    ? ` ${tr("debugConsole.route.windows", { count: candidate.quota_windows.length })}`
    : "";
  return `${source}${window}${used}${reset}${windows}`;
}

export function routePlanInvokeErrorText(error: unknown) {
  const message = String(error);
  if (
    message.includes("本地入口未运行或本地 API Key 无效")
    || message.includes("Local entry is not running")
  ) {
    return message;
  }
  return tr("debugConsole.route.localEntryInvalid", { error: message });
}

export function formatRouteScore(value?: number) {
  if (value === undefined || Number.isNaN(value)) return "-";
  return Math.abs(value) < 10 ? value.toFixed(3) : value.toFixed(1);
}

export function utf8ByteLength(value: string) {
  return new TextEncoder().encode(value).length;
}

export function accessTestRequestPath(protocol: string, model: string) {
  if (protocol === "openai_chat") return "/v1/chat/completions";
  if (protocol === "anthropic_messages") return "/anthropic/v1/messages";
  if (protocol === "gemini_native") return `/gemini/v1beta/models/${encodeURIComponent(model)}:generateContent`;
  return "/v1/responses";
}

type DebugRequestExperiments = {
  claudeServerSideCompaction?: boolean;
};

export function routePlanRequestBody(
  protocol: string,
  model: string,
  prompt: string,
  stream: boolean,
  experiments: DebugRequestExperiments = {},
) {
  const text = prompt.trim() || "ping";
  if (protocol === "openai_chat") {
    return { model, stream, messages: [{ role: "user", content: text }] };
  }
  if (protocol === "anthropic_messages") {
    return {
      model,
      stream,
      max_tokens: 64,
      messages: [{ role: "user", content: text }],
      ...(experiments.claudeServerSideCompaction
        ? {
            context_management: {
              edits: [{
                type: "compact_20260112",
                trigger: { type: "input_tokens", value: 50_000 },
              }],
            },
          }
        : {}),
    };
  }
  if (protocol === "gemini_native") {
    return { contents: [{ role: "user", parts: [{ text }] }] };
  }
  return { model, stream, input: text };
}

export function platformModelOptions(providers: PlatformProvider[], nodes: PlatformSupplierNode[]) {
  const nodeModels = nodes.flatMap((node) => {
    const models = node.models && node.models.length > 0 ? node.models : [node.public_model, node.upstream_model];
    return models.filter(Boolean);
  });
  const providerModels = providers
    .filter((provider) => provider.enabled)
    .flatMap((provider) => [provider.public_model, provider.model])
    .filter(Boolean);
  return dedupeModels([...nodeModels, ...providerModels]);
}

export function debugProtocolOptions(): DebugProtocol[] {
  return ["openai_responses", "openai_chat", "anthropic_messages", "gemini_native"];
}

export function normalizeSupportedProtocols(protocols: string[] = [], apiFormat = "openai_chat", kind = "openai_compatible", platform?: string): DebugProtocol[] {
  const source = protocols.length > 0
    ? protocols
    : isSubscriptionAdapter(kind)
      ? subscriptionSupportedProtocols(platform)
      : protocolsFromApiFormat(apiFormat);
  const out: DebugProtocol[] = [];
  for (const protocol of source) {
    const normalized = normalizeDebugProtocol(protocol);
    if (normalized && !out.includes(normalized)) {
      out.push(normalized);
    }
  }
  return out.length > 0 ? out : protocolsFromApiFormat(apiFormat);
}

export function normalizeDebugProtocol(protocol: string): DebugProtocol | null {
  if (protocol === "openai_responses") return "openai_responses";
  if (protocol === "openai_chat" || protocol === "gemini_openai") return "openai_chat";
  if (protocol === "anthropic_messages") return "anthropic_messages";
  if (protocol === "gemini_native") return "gemini_native";
  return null;
}

export function protocolsFromApiFormat(apiFormat: string): DebugProtocol[] {
  const normalized = normalizeDebugProtocol(apiFormat);
  return normalized ? [normalized] : [];
}

export function subscriptionSupportedProtocols(platform?: string): DebugProtocol[] {
  if (platform === "openai") return ["openai_responses"];
  if (platform === "grok") return ["openai_responses"];
  if (platform === "antigravity") return ["gemini_native"];
  if (!platform || platform === "claude") return ["anthropic_messages"];
  return [];
}

export function surfaceForProtocol(protocol: DebugProtocol): ApiSurface {
  if (protocol === "anthropic_messages") return "anthropic";
  if (protocol === "gemini_native") return "gemini";
  return "open_ai";
}

export function surfaceProtocols(surface: ApiSurface): DebugProtocol[] {
  if (surface === "open_ai") return ["openai_responses", "openai_chat"];
  if (surface === "anthropic") return ["anthropic_messages"];
  return ["gemini_native"];
}

export function surfaceOperationOptions(surface: ApiSurface) {
  if (surface === "open_ai") {
    return [
      { operation: "list_models", label: tr("supplier.advanced.modelList"), method: "GET" },
      { operation: "get_model", label: tr("supplier.advanced.modelDetails"), method: "GET" },
      { operation: "chat_completions", label: "Chat Completions", method: "POST" },
      { operation: "responses", label: "Responses", method: "POST" },
      { operation: "responses_compact", label: "Responses Compact", method: "POST" },
      { operation: "embeddings", label: "Embeddings", method: "POST" },
    ];
  }
  if (surface === "anthropic") {
    return [
      { operation: "list_models", label: tr("supplier.advanced.modelList"), method: "GET" },
      { operation: "get_model", label: tr("supplier.advanced.modelDetails"), method: "GET" },
      { operation: "messages", label: "Messages", method: "POST" },
      { operation: "count_tokens", label: "Count Tokens", method: "POST" },
    ];
  }
  return [
    { operation: "list_models", label: tr("supplier.advanced.modelList"), method: "GET" },
    { operation: "get_model", label: tr("supplier.advanced.modelDetails"), method: "GET" },
    { operation: "generate_content", label: "Generate Content", method: "POST" },
    { operation: "stream_generate_content", label: "Stream Generate Content", method: "POST" },
    { operation: "count_tokens", label: "Count Tokens", method: "POST" },
    { operation: "embed_content", label: "Embed Content", method: "POST" },
  ];
}

export function defaultSurfaceOperationUrl(baseUrl: string, surface: ApiSurface, operation: string) {
  const base = baseUrl.trim().replace(/\/$/, "");
  const path = surface === "open_ai"
    ? ({
      list_models: "/models",
      get_model: "/models/{model}",
      chat_completions: "/chat/completions",
      responses: "/responses",
      responses_compact: "/responses/compact",
      embeddings: "/embeddings",
    } as Record<string, string>)[operation]
    : surface === "anthropic"
      ? ({
        list_models: "/v1/models",
        get_model: "/v1/models/{model}",
        messages: "/v1/messages",
        count_tokens: "/v1/messages/count_tokens",
      } as Record<string, string>)[operation]
      : ({
        list_models: "/v1beta/models",
        get_model: "/v1beta/models/{model}",
        generate_content: "/v1beta/models/{model}:generateContent",
        stream_generate_content: "/v1beta/models/{model}:streamGenerateContent",
        count_tokens: "/v1beta/models/{model}:countTokens",
        embed_content: "/v1beta/models/{model}:embedContent",
      } as Record<string, string>)[operation];
  return `${base}${path ?? ""}`;
}

export function surfaceLabel(surface: ApiSurface) {
  if (surface === "open_ai") return "OpenAI";
  if (surface === "anthropic") return "Anthropic";
  return "Gemini";
}

export function surfaceVerificationLabel(verification: SurfaceVerification | undefined) {
  const state = verification?.state?.trim().toLowerCase();
  if (state === "verified") return tr("supplier.advanced.verified");
  if (state === "rejected") return tr("supplier.advanced.rejected");
  return tr("supplier.advanced.declared");
}

export function surfaceEditorRows(channel: ChannelConfig, bindings: ChannelSurfaceBinding[]) {
  if (channel.kind !== "custom_endpoint") return bindings;
  return (["open_ai", "anthropic", "gemini"] as ApiSurface[]).map((surface) => bindings.find((binding) => binding.surface === surface) ?? {
    surface,
    base_url: "",
    endpoint_profile: surfaceEndpointProfile(channel.kind, surface, channel.subscription?.platform),
    auth_scheme: surfaceAuthScheme(channel.kind, surface),
    protocols: [],
    operation_overrides: [],
    verification: { state: "declared", checked_at_unix: 0, summary: "" },
  });
}

export function surfaceEndpointProfile(kind: string, surface: ApiSurface, platform?: string) {
  const source = kind.trim().toLowerCase();
  if (source === "subscription_adapter") return `subscription_${platform || "claude"}`;
  if (source === "openai" && surface === "open_ai") return "openai";
  if (source === "anthropic" && surface === "anthropic") return "anthropic";
  if (source === "gemini" && surface === "gemini") return "google_gemini";
  if (source === "gemini" && surface === "open_ai") return "google_openai";
  if (["azure", "azure_openai"].includes(source) && surface === "open_ai") return "azure_openai";
  if (source === "aws_bedrock" && surface === "open_ai") return "bedrock_mantle_openai";
  if (source === "aws_bedrock" && surface === "anthropic") return "bedrock_mantle_anthropic";
  if (surface === "anthropic") return "anthropic_compatible";
  if (surface === "gemini") return "gemini_compatible";
  return "openai_compatible";
}

export function surfaceAuthScheme(kind: string, surface: ApiSurface) {
  const source = kind.trim().toLowerCase();
  if (source === "subscription_adapter") return "subscription";
  if (["azure", "azure_openai"].includes(source)) return "api_key";
  if (surface === "anthropic") return "x_api_key";
  if (surface === "gemini") return "x_goog_api_key";
  return "bearer";
}

export function protocolVerification(channel: ChannelConfig, protocol: DebugProtocol): ProtocolVerification {
  return (channel.surfaces ?? channel.surface_bindings ?? [])
    .flatMap((surface) => surface.protocols)
    .find((binding) => binding.protocol === protocol)
    ?.verification ?? { state: "declared", checked_at_unix: 0, summary: "" };
}

export function normalizeSurfaceBindings(channel: ChannelConfig): ChannelSurfaceBinding[] {
  return structuredClone(channel.surfaces ?? channel.surface_bindings ?? []);
}

export function debugTargetProtocolHint(
  target: DebugTargetOption | undefined,
  protocol: DebugProtocol,
  inboundProtocol: DebugProtocol,
  targetType: DebugTargetType,
) {
  if (targetType === "platform_auto") {
    return tr("debugConsole.protocolHint.platform", { protocol: protocolLabel(protocol) });
  }
  void target;
  void inboundProtocol;
  return tr("debugConsole.protocolHint.channel", { protocol: protocolLabel(protocol) });
}

export function debugRecommendedTargetProtocol(
  target: DebugTargetOption | undefined,
  inboundProtocol: DebugProtocol,
  targetType: DebugTargetType,
): DebugProtocol {
  void target;
  void targetType;
  return inboundProtocol;
}

export function debugTargetTypeLabel(targetType: DebugTargetType) {
  if (targetType === "local_channel") return tr("debugConsole.targetTypes.local");
  if (targetType === "platform_auto") return tr("debugConsole.targetTypes.platformAuto");
  return tr("debugConsole.targetTypes.platformNode");
}

export function debugEmptyTargetText(targetType: DebugTargetType, lastRefresh: number | null) {
  if (targetType === "local_channel") return tr("debugConsole.emptyTargets.local");
  if (!lastRefresh) return tr("debugConsole.emptyTargets.notLoaded");
  return tr("debugConsole.emptyTargets.generic");
}

export function formatRelativeTime(timestamp: number) {
  const seconds = Math.max(0, Math.floor((Date.now() - timestamp) / 1000));
  if (seconds < 5) return tr("debugConsole.relativeTime.justNow");
  if (seconds < 60) return tr("debugConsole.relativeTime.seconds", { count: seconds });
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return tr("debugConsole.relativeTime.minutes", { count: minutes });
  const hours = Math.floor(minutes / 60);
  return tr("debugConsole.relativeTime.hours", { count: hours });
}

export function debugTargetOptions(
  targetType: DebugTargetType,
  channels: ChannelConfig[],
  nodes: PlatformSupplierNode[],
  platformModels: string[],
): DebugTargetOption[] {
  if (targetType === "local_channel") {
    return channels.flatMap((channel) => {
      const apiFormat = sanitizeApiFormat(channel.kind, channel.api_format);
      const supportedProtocols = normalizeSupportedProtocols(channel.supported_protocols ?? [], apiFormat, channel.kind, channel.subscription?.platform);
      const defaultProtocol = supportedProtocols[0] ?? debugProtocolFromApiFormat(apiFormat);
      if (!defaultProtocol) return [];
      const models = dedupeModels(channel.models.length > 0 ? channel.models : [channel.public_model, channel.upstream_model]);
      const defaultModel = reconcileDetectedDefaultModel(
        channel.default_model || channel.upstream_model || channel.public_model,
        models,
      ).defaultModel;
      return [{
        id: channel.id,
        name: channel.name || channel.id,
        subtitle: `${channelKindLabel(channel.kind)} · ${protocolLabel(apiFormat)}`,
        models,
        defaultModel,
        defaultProtocol,
        supportedProtocols,
        status: channelIsPooled(channel)
          ? tr("debugConsole.targetStatus.pooled")
          : tr("debugConsole.targetStatus.disabled"),
      }];
    });
  }
  if (targetType === "platform_node") {
    return nodes.map((node) => {
      const supportedProtocols = node.supported_protocols && node.supported_protocols.length > 0
        ? normalizeSupportedProtocols(node.supported_protocols, "")
        : [];
      const models = dedupeModels(node.models && node.models.length > 0 ? node.models : [node.public_model, node.upstream_model]);
      const defaultModel = reconcileDetectedDefaultModel(
        node.public_model || node.upstream_model,
        models,
      ).defaultModel;
      return {
        id: node.node_id,
        name: node.name || node.node_id,
        subtitle: `${tr("debugConsole.targetTypes.platformNode")} · ${node.health_status || "unknown"} / ${node.quota_status || "unknown"}`,
        models,
        defaultModel,
        defaultProtocol: supportedProtocols[0] ?? "openai_chat",
        supportedProtocols,
        status: `${node.health_status || "unknown"} / ${node.quota_status || "unknown"}`,
      };
    });
  }
  return [{
    id: "",
    name: tr("debugConsole.targetTypes.platformAuto"),
    subtitle: tr("debugConsole.platformAutoSubtitle"),
    models: platformModels,
    defaultModel: platformModels[0] ?? "",
    defaultProtocol: "openai_chat",
    supportedProtocols: [],
    status: tr("debugConsole.targetStatus.automatic"),
  }];
}

export function debugProtocolFromApiFormat(apiFormat: string): DebugProtocol | null {
  if (apiFormat === "openai_responses") return "openai_responses";
  if (apiFormat === "openai_chat" || apiFormat === "gemini_openai") return "openai_chat";
  if (apiFormat === "anthropic_messages") return "anthropic_messages";
  if (apiFormat === "gemini_native") return "gemini_native";
  return null;
}

export function subscriptionProviderLabel(provider: SubscriptionProvider) {
  if (provider === "openai") return tr("labels.subscription.openai");
  if (provider === "antigravity") return tr("labels.subscription.antigravity");
  if (provider === "grok") return tr("labels.subscription.grok");
  return tr("labels.subscription.claude");
}

export function subscriptionWizardTitle(provider: SubscriptionProvider) {
  return tr("labels.subscription.connect", { provider: subscriptionProviderLabel(provider) });
}

export function hasBlockingSupplierError(status: SupplierStatus) {
  return Boolean(
    (status.last_error || status.last_transport_error)
      && !isEstablishedSupplierTransport(status.connected_transport)
      && !isEstablishedSupplierTransport(status.active_transport),
  );
}

export function supplierTransportPending(status: SupplierStatus) {
  return status.starting || status.active_transport === "starting" || status.active_transport === "reconnecting";
}

export function channelIsPooled(channel: ChannelConfig) {
  return channel.enabled;
}

export function channelIsPlatformShareable(_channel: ChannelConfig) {
  return false;
}

export function channelIsPlatformPooled(channel: ChannelConfig) {
  return channel.enabled && channelIsPlatformShareable(channel);
}

export function channelHasVerifiedProtocol(channel: ChannelConfig) {
  return (channel.capability_profiles ?? []).some((profile) =>
    (profile.verification_state === "verified" || profile.verification_state === "declared") &&
    Boolean(profile.protocol) &&
    Boolean(profile.non_stream_json || profile.stream_sse)
  );
}

export function capabilityStateLabel(state?: string) {
  if (state === "verified") return tr("labels.capability.verified");
  if (state === "declared") return tr("labels.capability.declared");
  if (state === "failed") return tr("labels.capability.failed");
  return tr("labels.capability.unconfirmed");
}

export function capabilityEvidenceLabel(profile: ChannelCapabilityProfile) {
  const source = capabilityStateLabel(profile.verification_state);
  const timestamp = Number(profile.verified_at_unix ?? 0);
  if (!Number.isFinite(timestamp) || timestamp <= 0) return source;
  return `${source} · ${new Date(timestamp * 1000).toLocaleString(currentAppLanguage(), { hour12: false })}`;
}

export function capabilityFeatureLabels(profile: ChannelCapabilityProfile) {
  const labels: string[] = [];
  if (profile.non_stream_json) labels.push("JSON");
  if (profile.stream_sse) labels.push("Stream");
  if (profile.tool_calls) labels.push("Tool");
  if (profile.tool_choice) labels.push("Tool Choice");
  if (profile.parallel_tool_calls) labels.push("Parallel Tool");
  if (profile.json_schema) labels.push("Schema");
  if (profile.reasoning) labels.push("Reasoning");
  if (profile.thinking) labels.push("Thinking");
  if (profile.vision || profile.image_input) labels.push("Image In");
  if (profile.image_output) labels.push("Image Out");
  if (profile.audio_input) labels.push("Audio In");
  if (profile.audio_output) labels.push("Audio Out");
  if (profile.video_input) labels.push("Video In");
  if (profile.video_output) labels.push("Video Out");
  if (profile.file_input) labels.push("File In");
  if (profile.file_output) labels.push("File Out");
  if (profile.cache_control) labels.push("Cache");
  if (profile.custom_tool) labels.push("Custom Tool");
  for (const tool of profile.hosted_tools ?? []) {
    labels.push(tool);
  }
  return labels.length > 0 ? labels : [tr("labels.capability.base")];
}

export function channelSwitchLabel(channel: ChannelConfig) {
  return channelIsPooled(channel) ? tr("labels.channel.enabled") : tr("labels.channel.disabled");
}

export function supplierChannelModelCount(
  channel: Pick<ChannelConfig, "models" | "upstream_model" | "public_model">,
  health?: Pick<SupplierChannelHealth, "model_count">,
  routeStatus?: Pick<SupplierNodeRouteStatus, "accepted_models" | "unsupported_models">,
) {
  return Math.max(
    channel.models.length,
    health?.model_count ?? 0,
    (routeStatus?.accepted_models?.length ?? 0) + (routeStatus?.unsupported_models?.length ?? 0),
    channel.upstream_model || channel.public_model ? 1 : 0,
  );
}

export function supplierRouteStatusForChannel(supplierStatus: SupplierStatus, channelId: string) {
  const normalizedChannelId = channelId.trim();
  if (!normalizedChannelId) return undefined;
  return (supplierStatus.route_statuses ?? []).find((status) => status.channel_id?.trim() === normalizedChannelId);
}

export function supplierChannelTransportForChannel(supplierStatus: SupplierStatus, channelId: string) {
  const normalizedChannelId = channelId.trim();
  if (!normalizedChannelId) return undefined;
  return (supplierStatus.channel_transports ?? [])
    .find((status) => status.channel_id?.trim() === normalizedChannelId);
}

export function supplierChannelRuntimeStarted(supplierStatus: SupplierStatus, channelId: string) {
  return supplierChannelTransportForChannel(supplierStatus, channelId) !== undefined;
}

export function normalizedRouteState(routeStatus: SupplierNodeRouteStatus | undefined) {
  return routeStatus?.state?.trim().toLowerCase() ?? "";
}

export type SupplierRouteStatusToastSnapshot = {
  state: string;
  signature: string;
};

export function supplierRouteStatusToastIdentity(routeStatus: SupplierNodeRouteStatus) {
  return routeStatus.supplier_unit_id?.trim()
    || routeStatus.node_id?.trim()
    || routeStatus.channel_id?.trim()
    || "";
}

export function supplierRouteStatusToastSnapshot(
  routeStatus: SupplierNodeRouteStatus,
): SupplierRouteStatusToastSnapshot {
  // updated_at_unix is deliberately excluded: notifications describe semantic transitions,
  // not polling, acknowledgement, or sampling time changes.
  const acceptedModels = [...(routeStatus.accepted_models ?? [])].map((model) => model.trim()).filter(Boolean).sort();
  const unsupportedModels = [...(routeStatus.unsupported_models ?? [])].map((model) => model.trim()).filter(Boolean).sort();
  const fallbackPricedModels = supplierPricingWarnings(routeStatus)
    .map((fallback) => [
      fallback.model?.trim() ?? "",
      fallback.provider?.trim() ?? "",
      fallback.billing_model?.trim() ?? "",
      fallback.price_version_id?.trim() ?? "",
    ].join(":"))
    .sort();
  const state = normalizedRouteState(routeStatus);
  return {
    state,
    signature: JSON.stringify({
      state,
      reason: routeStatus.reason?.trim() ?? "",
      message: routeStatus.message?.trim() ?? "",
      acceptedModels,
      unsupportedModels,
      fallbackPricedModels,
    }),
  };
}

export type ProtocolDebugOutcome =
  | "preview"
  | "request_failed"
  | "request_succeeded"
  | "compaction_triggered"
  | "compaction_not_triggered";

function debugRequestUsesClaudeCompaction(requestBody: unknown) {
  if (!requestBody || typeof requestBody !== "object" || Array.isArray(requestBody)) return false;
  const contextManagement = (requestBody as Record<string, unknown>).context_management;
  if (!contextManagement || typeof contextManagement !== "object" || Array.isArray(contextManagement)) return false;
  const edits = (contextManagement as Record<string, unknown>).edits;
  return Array.isArray(edits) && edits.some((edit) => (
    edit !== null
    && typeof edit === "object"
    && !Array.isArray(edit)
    && String((edit as Record<string, unknown>).type ?? "").toLowerCase() === "compact_20260112"
  ));
}

function debugResponseShowsClaudeCompaction(result: ProtocolDebugResult) {
  const response = `${result.raw ?? ""}\n${result.content ?? ""}`;
  return /"type"\s*:\s*"(?:compaction|compaction_delta|compact_20260112)"/i.test(response)
    || /"stop_reason"\s*:\s*"compaction"/i.test(response);
}

function debugResponseShowsError(result: ProtocolDebugResult) {
  if (result.metrics.response_kind === "error") return true;
  if (/^event:\s*(?:error|response\.failed)\s*$/im.test(result.raw ?? "")) return true;
  const candidates = [result.raw ?? ""];
  for (const line of (result.raw ?? "").split(/\r?\n/)) {
    const data = line.match(/^data:\s*(.+)$/)?.[1]?.trim();
    if (data && data !== "[DONE]") candidates.push(data);
  }
  return candidates.some((candidate) => {
    try {
      const value = JSON.parse(candidate) as Record<string, unknown>;
      const type = String(value.type ?? "").toLowerCase();
      return type === "error" || type === "response.failed" || value.error != null;
    } catch {
      return false;
    }
  });
}

export function protocolDebugOutcome(result: ProtocolDebugResult): ProtocolDebugOutcome {
  const status = result.http_status;
  if (
    Boolean(result.error_layer)
    || (typeof status === "number" && (status < 200 || status >= 300))
    || debugResponseShowsError(result)
  ) {
    return "request_failed";
  }
  if (!result.executed) return "preview";
  if (!debugRequestUsesClaudeCompaction(result.request_body)) return "request_succeeded";
  return debugResponseShowsClaudeCompaction(result)
    ? "compaction_triggered"
    : "compaction_not_triggered";
}

export function shouldShowSupplierRouteStatusToast(
  previous: SupplierRouteStatusToastSnapshot | undefined,
  next: SupplierRouteStatusToastSnapshot,
) {
  if (!next.state) return false;
  // Seeing an already-ready route for the first time is availability, not recovery.
  if (!previous) return next.state !== "ready";
  if (previous.signature === next.signature) return false;
  if (next.state === "ready") return previous.state !== "ready";
  return true;
}

export function supplierRouteStatusSuppressesToast(routeStatus: SupplierNodeRouteStatus | undefined) {
  return routeStatus?.reason === "model_not_supported"
    || routeStatus?.reason === "model_unsupported"
    || routeStatus?.reason === "models_partially_supported";
}

export function supplierRouteStatusBlocksReady(routeStatus: SupplierNodeRouteStatus | undefined) {
  if (supplierRouteStatusHasAcceptedModels(routeStatus)) return false;
  const state = normalizedRouteState(routeStatus);
  return state === "fallback" || state === "offline" || state === "removed" || state === "deleted" || state === "failed";
}

export function supplierRouteStatusHasAcceptedModels(routeStatus: SupplierNodeRouteStatus | undefined) {
  return routeStatus?.reason === "models_partially_supported"
    && (routeStatus.accepted_models?.length ?? 0) > 0;
}

export function supplierRouteStatusTone(routeStatus: SupplierNodeRouteStatus | undefined) {
  if (supplierRouteStatusHasAcceptedModels(routeStatus)) return "";
  const state = normalizedRouteState(routeStatus);
  if (state === "fallback" || state === "degraded") return "pending";
  if (state === "offline" || state === "removed" || state === "deleted" || state === "failed") return "bad";
  return "";
}

export function supplierRouteStatusInlineLabel(routeStatus: SupplierNodeRouteStatus | undefined) {
  const state = normalizedRouteState(routeStatus);
  if (routeStatus?.reason === "model_not_supported") return tr("supplierRoute.modelNotInPool");
  if (supplierRouteStatusHasAcceptedModels(routeStatus)) return "";
  if (routeStatus?.reason === "models_partially_supported") return tr("supplierRoute.modelNotInPool");
  if (routeStatus?.reason === "pricing_fallback") return tr("supplierRoute.pricingFallback");
  if (state === "fallback") return tr("supplierRoute.fallbackPool");
  if (state === "offline") return tr("supplierRoute.serverOffline");
  if (state === "removed" || state === "deleted") return tr("supplierRoute.serverRemoved");
  if (state === "failed") return tr("supplierRoute.serverUnavailable");
  return "";
}

export function supplierRouteReasonLabel(reason: string | undefined) {
  switch (reason?.trim()) {
    case "rate_limited":
      return tr("supplierRoute.reasons.rateLimited");
    case "auth_error":
      return tr("supplierRoute.reasons.authError");
    case "risk_blocked":
    case "risk_challenge":
      return tr("supplierRoute.reasons.riskControl");
    case "quota_exhausted":
      return tr("supplierRoute.reasons.quotaExhausted");
    case "cooldown":
      return tr("supplierRoute.reasons.cooldown");
    case "timeout":
      return tr("supplierRoute.reasons.timeout");
    case "network_error":
      return tr("supplierRoute.reasons.networkError");
    case "channel_failure":
      return tr("supplierRoute.reasons.channelFailure");
    case "temporary_failure":
      return tr("supplierRoute.reasons.temporaryFailure");
    case "unregistered":
      return tr("supplierRoute.reasons.clientOffline");
    case "removed":
      return tr("supplierRoute.reasons.channelRemoved");
    case "failed":
      return tr("supplierRoute.reasons.callFailed");
    case "model_not_supported":
      return tr("supplierRoute.reasons.catalogUnsupported");
    case "models_partially_supported":
      return tr("supplierRoute.reasons.catalogPartial");
    case "pricing_fallback":
      return tr("supplierRoute.reasons.reviewedFamilyPricing");
    default:
      return reason?.trim() ?? "";
  }
}

type SupplierTransportFailurePayload = {
  direction?: unknown;
  error_kind?: unknown;
  failure_scope?: unknown;
  message?: unknown;
  phase?: unknown;
  timeout_ms?: unknown;
  transport?: unknown;
};

function supplierTransportLabel(value: string) {
  switch (value.toLowerCase()) {
    case "quic":
      return "QUIC";
    case "websocket":
      return "WebSocket";
    default:
      return value;
  }
}

function supplierTransportPhaseLabel(value: string) {
  switch (value.toLowerCase()) {
    case "server_write":
      return tr("supplierRoute.transportPhases.serverWrite");
    case "server_set_write_deadline":
      return tr("supplierRoute.transportPhases.serverSetWriteDeadline");
    default:
      return value.split("_").join(" ");
  }
}

function supplierTransportTimeoutLabel(timeoutMs: number) {
  if (timeoutMs >= 1000) {
    const seconds = timeoutMs / 1000;
    return tr("supplierRoute.durationSeconds", {
      value: Number.isInteger(seconds) ? seconds : seconds.toFixed(1),
    });
  }
  return tr("supplierRoute.durationMilliseconds", { value: timeoutMs });
}

type SupplierStructuredRouteMessage = {
  text: string;
  structured: boolean;
};

function supplierStructuredRouteMessage(message: string): SupplierStructuredRouteMessage {
  const raw = message.trim();
  if (!raw.startsWith("{")) return { text: raw, structured: false };
  let payload: SupplierTransportFailurePayload;
  try {
    const parsed = JSON.parse(raw) as unknown;
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
      return { text: raw, structured: false };
    }
    payload = parsed as SupplierTransportFailurePayload;
  } catch {
    return { text: raw, structured: false };
  }

  const transport = typeof payload.transport === "string" ? payload.transport.trim() : "";
  const phase = typeof payload.phase === "string" ? payload.phase.trim() : "";
  const errorKind = typeof payload.error_kind === "string" ? payload.error_kind.trim() : "";
  const lowLevelMessage = typeof payload.message === "string" ? payload.message.trim() : "";
  const timeoutMs = typeof payload.timeout_ms === "number" && Number.isFinite(payload.timeout_ms)
    ? Math.max(0, payload.timeout_ms)
    : 0;

  if (transport && phase) {
    const params = {
      transport: supplierTransportLabel(transport),
      phase: supplierTransportPhaseLabel(phase),
      message: lowLevelMessage || tr("supplierRoute.noLowLevelDetail"),
    };
    if (errorKind === "timeout") {
      return { text: timeoutMs > 0
        ? tr("supplierRoute.transportTimeoutWithLimit", {
          ...params,
          timeout: supplierTransportTimeoutLabel(timeoutMs),
        })
        : tr("supplierRoute.transportTimeout", params), structured: true };
    }
    return { text: tr("supplierRoute.transportFailure", params), structured: true };
  }

  // Older servers carried only the scoped low-level message. Extract it so a
  // formal client remains readable instead of exposing the raw JSON envelope.
  if (lowLevelMessage) {
    if (lowLevelMessage.toLowerCase() === "deadline exceeded") {
      return {
        text: tr("supplierRoute.legacyDeadlineExceeded", { message: lowLevelMessage }),
        structured: true,
      };
    }
    return { text: lowLevelMessage, structured: true };
  }
  if (typeof payload.failure_scope === "string" || errorKind) {
    return { text: "", structured: true };
  }
  return { text: raw, structured: false };
}

export function supplierRouteStatusDetail(routeStatus: SupplierNodeRouteStatus | undefined) {
  const label = supplierRouteStatusInlineLabel(routeStatus);
  if (routeStatus?.reason === "models_partially_supported") {
    return tr("supplierRoute.partialModels", {
      accepted: routeStatus.accepted_models?.length ?? 0,
      unsupported: routeStatus.unsupported_models?.length ?? 0,
    });
  }
  if (!label) return "";
  if (routeStatus?.reason === "model_not_supported") {
    return tr("supplierRoute.unsupportedModels", {
      label,
      count: routeStatus.unsupported_models?.length ?? 0,
    });
  }
  if (routeStatus?.reason === "pricing_fallback") {
    return `${label}：${supplierPricingFallbackSummary(routeStatus)}`;
  }
  const reason = supplierRouteReasonLabel(routeStatus?.reason);
  const message = supplierStructuredRouteMessage(routeStatus?.message ?? "");
  if (message.structured) {
    return [label, message.text || reason].filter(Boolean).join(tr("supplierRoute.detailSeparator"));
  }
  return [label, reason, message.text].filter(Boolean).join(tr("supplierRoute.detailSeparator"));
}

export function supplierRouteStatusToastText(routeStatus: SupplierNodeRouteStatus, channelName: string) {
  if (supplierRouteStatusSuppressesToast(routeStatus)) return "";
  const state = normalizedRouteState(routeStatus);
  if (supplierRouteStatusHasAcceptedModels(routeStatus)) {
    return tr("supplierRoute.onlineWithDetail", {
      channel: channelName,
      detail: supplierRouteStatusDetail(routeStatus),
    });
  }
  if (state === "ready") return tr("supplierRoute.restored", { channel: channelName });
  const detail = supplierRouteStatusDetail(routeStatus);
  if (detail) return tr("supplierRoute.channelDetail", { channel: channelName, detail });
  return "";
}

export function supplierRouteStatusToastTone(routeStatus: SupplierNodeRouteStatus): Toast["tone"] {
  if (supplierRouteStatusHasAcceptedModels(routeStatus)) return "info";
  const state = normalizedRouteState(routeStatus);
  if (state === "ready") return "success";
  return state === "fallback" || state === "degraded" ? "info" : "error";
}

export function supplierPricingWarnings(routeStatus: SupplierNodeRouteStatus | undefined) {
  return (routeStatus?.fallback_priced_models ?? []).filter((item) => item.confidence !== "variant_fallback");
}

export function supplierPricingFallbackSummary(routeStatus: SupplierNodeRouteStatus | undefined) {
  const fallbacks = supplierPricingWarnings(routeStatus);
  if (fallbacks.length === 0) return tr("supplierRoute.pricingNoDetails");
  const details = fallbacks.slice(0, 3).map((fallback) => {
    const model = modelDisplayName(fallback.model ?? "") || tr("supplierRoute.unknownModel");
    const billingModel = modelDisplayName(fallback.billing_model ?? "") || tr("supplierRoute.unknownBillingModel");
    const version = fallback.price_version_id?.trim();
    return tr("supplierRoute.pricingPair", {
      model,
      billingModel,
      version: version ? tr("supplierRoute.versionSuffix", { version }) : "",
    });
  });
  const remaining = fallbacks.length - details.length;
  return tr("supplierRoute.pricingSummary", {
    count: fallbacks.length,
    details: details.join(tr("supplierRoute.listSeparator")),
    remaining: remaining > 0 ? tr("supplierRoute.remainingModels", { count: remaining }) : "",
  });
}

export function channelStateTone(
  channel: ChannelConfig,
  health: SupplierChannelHealth | undefined,
  supplierStatus: SupplierStatus,
  startingChannelId?: string | null,
  pendingChannelIds: string[] = [],
) {
  if (!channelIsPooled(channel)) return "muted";
  if (startingChannelId === channel.id || pendingChannelIds.includes(channel.id)) return "pending";
  const channelTransport = supplierChannelTransportForChannel(supplierStatus, channel.id);
  const transportOnline = isEstablishedSupplierTransport(channelTransport?.active_transport);
  const platformOnline = transportOnline && channelTransport?.platform_registered === true;
  const safetyState = normalizedSafetyState(health, channel);
  const routeStatus = supplierRouteStatusForChannel(supplierStatus, channel.id);
  const routeTone = supplierRouteStatusTone(routeStatus);
  if (safetyState === "cooldown" || safetyState === "auth_refreshing" || safetyState === "quota_low") return "pending";
  if (["risk_blocked", "auth_error", "quota_exhausted", "disabled"].includes(safetyState)) return "bad";
  if (routeTone && platformOnline) return routeTone;
  if (channelLocallyReady(channel, health)) return "good";
  if (supplierTransportPending(supplierStatus)) return "pending";
  if (hasBlockingSupplierError(supplierStatus)) return "bad";
  if (health?.status === "available" && platformOnline) return "good";
  if (health && health.status !== "available") return "bad";
  if (channelCanUseLocally(channel)) return "pending";
  return "bad";
}

export function channelOnlineLabel(
  channel: ChannelConfig,
  health: SupplierChannelHealth | undefined,
  supplierStatus: SupplierStatus,
  pendingChannelIds: string[] = [],
) {
  if (!channelIsPooled(channel)) return tr("labels.channel.offline");
  if (pendingChannelIds.includes(channel.id)) return tr("labels.channel.checking");
  if (supplierStatus.starting && !health) return tr("labels.channel.checking");
  const channelTransport = supplierChannelTransportForChannel(supplierStatus, channel.id);
  const transportOnline = isEstablishedSupplierTransport(channelTransport?.active_transport);
  const platformOnline = transportOnline && channelTransport?.platform_registered === true;
  const safetyState = normalizedSafetyState(health, channel);
  const routeStatus = supplierRouteStatusForChannel(supplierStatus, channel.id);
  const routeLabel = supplierRouteStatusInlineLabel(routeStatus);
  if (safetyState === "cooldown") return tr("labels.channel.cooldown");
  if (safetyState === "auth_refreshing") return tr("labels.channel.refreshing");
  if (safetyState === "quota_low") return tr("labels.channel.lowQuota");
  if (safetyState === "quota_exhausted") return tr("labels.channel.quotaExhausted");
  if (safetyState === "auth_error") return tr("labels.channel.reauthorize");
  if (safetyState === "risk_blocked") return tr("labels.channel.manualAction");
  if (routeLabel && platformOnline && supplierRouteStatusBlocksReady(routeStatus)) return routeLabel;
  if (channelLocallyReady(channel, health)) {
    return platformOnline && !supplierRouteStatusBlocksReady(routeStatus)
      ? tr("labels.channel.platformOnline")
      : tr("labels.channel.localAvailable");
  }
  if (routeLabel && platformOnline) return routeLabel;
  if (health?.status === "available" && platformOnline) return tr("labels.channel.platformOnline");
  if (health && health.status !== "available") return tr("labels.channel.offline");
  if (channelCanUseLocally(channel)) return tr("labels.channel.pendingLocalCheck");
  if (hasBlockingSupplierError(supplierStatus)) return tr("labels.channel.offline");
  if (supplierStatus.running) return tr("labels.channel.waitingRefresh");
  return tr("labels.channel.offline");
}

export function healthStatusLabel(health: SupplierChannelHealth | undefined, channel: ChannelConfig, supplierRunning = false) {
  if (!channelIsPooled(channel)) return tr("labels.channel.disabled");
  const safetyState = normalizedSafetyState(health, channel);
  if (safetyState === "cooldown") return tr("labels.channel.localCooldown");
  if (safetyState === "auth_refreshing") return tr("labels.channel.credentialRefreshing");
  if (safetyState === "quota_exhausted") return tr("labels.channel.quotaExhausted");
  if (safetyState === "auth_error") return tr("labels.channel.authorizationExpired");
  if (safetyState === "risk_blocked") return tr("labels.channel.riskBlocked");
  if (health?.status === "available") return tr("labels.channel.checkPassed");
  if (supplierRunning) return tr("labels.channel.notStarted");
  return tr("labels.channel.waitingStartupCheck");
}

export function credentialStatusLabel(status: string | undefined, channel: ChannelConfig) {
  if (channel.kind !== "subscription_adapter") return tr("labels.channel.notRequired");
  if (status === "refreshed") return tr("labels.channel.refreshed");
  if (status === "ok") return tr("labels.channel.available");
  return tr("labels.channel.waitingCheck");
}

export function quotaStatusLabel(status: string | undefined, channel: ChannelConfig) {
  if (channel.kind !== "subscription_adapter" && !channel.discovery?.quota) return tr("labels.channel.notApplicable");
  if (status === "available") return tr("labels.channel.available");
  if (status === "low") return tr("labels.channel.low");
  if (status === "exhausted") return tr("labels.channel.exhausted");
  return tr("labels.unknown");
}

export function normalizedSafetyState(health: SupplierChannelHealth | undefined, channel: ChannelConfig) {
  if (channel.kind !== "subscription_adapter") return "not_applicable";
  const state = health?.safety_state?.trim();
  if (state) return state;
  if (health?.status === "available") return "active";
  return "unknown";
}

export function safetyStatusLabel(health: SupplierChannelHealth | undefined, channel: ChannelConfig) {
  if (channel.kind !== "subscription_adapter") return tr("labels.channel.notApplicable");
  switch (normalizedSafetyState(health, channel)) {
    case "active":
      return tr("labels.channel.normal");
    case "quota_low":
      return tr("labels.channel.lowQuota");
    case "cooldown":
      return tr("labels.channel.cooldown");
    case "quota_exhausted":
      return tr("labels.channel.quotaExhausted");
    case "auth_refreshing":
      return tr("labels.channel.refreshing");
    case "auth_error":
      return tr("labels.channel.authorizationExpired");
    case "risk_blocked":
      return tr("labels.channel.riskBlocked");
    case "disabled":
      return tr("labels.channel.disabled");
    default:
      return tr("labels.unknown");
  }
}

export function safetyDetailLabel(health: SupplierChannelHealth | undefined, channel: ChannelConfig) {
  if (channel.kind !== "subscription_adapter") return tr("labels.channel.apiKeyChannel");
  if (!health) return tr("labels.channel.waitingCheck");
  const state = normalizedSafetyState(health, channel);
  if (state === "cooldown" && health.cooldown_until_unix) {
    return tr("labels.channel.recoverIn", {
      duration: formatUnixCountdown(health.cooldown_until_unix),
    });
  }
  if (state === "risk_blocked" || state === "auth_error") {
    return health.last_error_kind || tr("labels.channel.needsAttention");
  }
  const concurrency =
    typeof health.current_concurrency === "number" && typeof health.max_concurrency === "number"
      ? health.max_concurrency > 0
        ? `${health.current_concurrency}/${health.max_concurrency}`
        : tr("labels.channel.unlimited")
      : "";
  return [
    concurrency ? tr("labels.channel.concurrency", { value: concurrency }) : "",
  ].filter(Boolean).join(" · ") || tr("labels.channel.protected");
}

export function formatUnixCountdown(unixSeconds: number) {
  const seconds = Math.max(0, unixSeconds - Math.floor(Date.now() / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.ceil(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.ceil(minutes / 60);
  return `${hours}h`;
}

export function channelCanSupply(channel: ChannelConfig) {
  const model = channel.upstream_model || channel.public_model || channel.models[0] || "";
  return Boolean(model.trim() && channelCanDetect(channel));
}

export function channelCanDetect(channel: ChannelConfig) {
  return channel.kind === "subscription_adapter" || Boolean(channel.upstream_base_url.trim());
}

export function channelCanUseLocally(channel: ChannelConfig) {
  if (!channelIsPooled(channel) || !channelCanSupply(channel)) return false;
  if (channel.kind === "subscription_adapter") return true;
  return !channel.upstream_base_url.trim().toLowerCase().startsWith("local://");
}

export function channelLocallyReady(channel: ChannelConfig, health: SupplierChannelHealth | undefined) {
  if (!channelCanUseLocally(channel) || !health) return false;
  const safetyState = normalizedSafetyState(health, channel);
  const statusReady = health.status === "available" || safetyState === "quota_low";
  return statusReady && ![
    "cooldown",
    "auth_refreshing",
    "quota_exhausted",
    "auth_error",
    "risk_blocked",
    "disabled",
  ].includes(safetyState);
}

export function projectRendererChannel(channel: ChannelConfig): ChannelConfig {
  const driver = sourceDriverById(channel.source_driver);
  const surfaces = structuredClone(channel.surfaces ?? channel.surface_bindings ?? []);
  const supportedProtocols = surfaces
    .flatMap((surface) => surface.protocols)
    .filter((binding) => !["rejected", "failed", "unsupported"].includes(binding.verification.state.trim().toLowerCase()))
    .map((binding) => binding.protocol)
    .filter((protocol, index, values) => values.indexOf(protocol) === index);
  const defaultSurface = surfaces.find((surface) => surface.surface === channel.default_target.surface);
  const subscriptionPlatform = driver.subscriptionProvider ?? channel.subscription?.platform ?? "claude";
  const maxConcurrency = channel.max_concurrency
    ?? channel.subscription?.max_concurrency
    ?? DEFAULT_SUPPLIER_MAX_CONCURRENCY;
  const quotaReservePercent = channel.quota_reserve_percent
    ?? channel.subscription?.quota_reserve_percent
    ?? 0;
  const subscription = {
    ...(channel.subscription ?? defaultSubscriptionAdapter(subscriptionPlatform)),
    platform: subscriptionPlatform,
    credential_ref: channel.credential_ref,
    max_concurrency: maxConcurrency,
    quota_reserve_percent: quotaReservePercent,
  };
  const reconciled = reconcileDetectedDefaultModel(
    channel.default_model || channel.upstream_model || channel.public_model,
    channel.models,
  );
  return {
    ...channel,
    source_driver: driver.id,
    executor: structuredClone(driver.executor),
    kind: driver.kind,
    api_format: channel.default_target.protocol,
    default_model: reconciled.defaultModel,
    upstream_base_url: defaultSurface?.base_url ?? "",
    upstream_api_key: channel.credential_ref,
    supported_protocols: supportedProtocols,
    surfaces,
    surface_bindings: surfaces,
    capability_profiles: channel.model_capability_evidence as ChannelCapabilityProfile[],
    detection_checks: channel.detection_evidence,
    max_concurrency: maxConcurrency,
    quota_reserve_percent: quotaReservePercent,
    subscription,
    public_model: reconciled.defaultModel,
    upstream_model: reconciled.defaultModel,
    models: reconciled.models,
  };
}

export function rendererChannelFromV5(value: ChannelConfigV5, legacy?: Partial<ChannelConfig>): ChannelConfig {
  const channel = reloadChannelV5(value);
  const driver = sourceDriverById(channel.source_driver);
  const defaultModel = reconcileDetectedDefaultModel(
    legacy?.upstream_model || legacy?.public_model || channel.default_model,
    channel.models,
  ).defaultModel;
  const subscription = channel.subscription
    ? {
        ...channel.subscription,
        credential_ref: channel.credential_ref,
        max_concurrency: channel.max_concurrency,
        quota_reserve_percent: channel.quota_reserve_percent,
      }
    : {
        ...defaultSubscriptionAdapter(driver.subscriptionProvider ?? "claude"),
        max_concurrency: channel.max_concurrency,
        quota_reserve_percent: channel.quota_reserve_percent,
      };
  return projectRendererChannel({
    ...channel,
    kind: driver.kind,
    api_format: channel.default_target.protocol,
    upstream_base_url: "",
    upstream_api_key: channel.credential_ref,
    default_model: defaultModel,
    public_model: defaultModel,
    upstream_model: defaultModel,
    supported_protocols: [],
    capability_profiles: channel.model_capability_evidence as ChannelCapabilityProfile[],
    detection_checks: channel.detection_evidence,
    surface_bindings: channel.surfaces,
    subscription,
  });
}

export function channelV5FromRenderer(channel: ChannelConfig): ChannelConfigV5 {
  const driver = sourceDriverById(channel.source_driver);
  const reconciled = reconcileDetectedDefaultModel(
    channel.default_model || channel.upstream_model || channel.public_model,
    channel.models,
  );
  return {
    id: channel.id,
    name: channel.name,
    created_at_unix_ms: channel.created_at_unix_ms,
    source_driver: driver.id,
    enabled: channel.enabled,
    share_enabled: channel.enabled && channelIsPlatformShareable(channel),
    credential_ref: channel.credential_ref,
    executor: structuredClone(driver.executor),
    surfaces: structuredClone(channel.surfaces ?? channel.surface_bindings ?? []),
    default_target: structuredClone(channel.default_target),
    discovery: structuredClone(channel.discovery),
    user_agent_profile: channel.user_agent_profile ?? "",
    default_model: reconciled.defaultModel,
    models: reconciled.models,
    price_ratio: normalizedPriceRatio(channel.price_ratio),
    node_id: channel.node_id,
    server_ws_url: channel.server_ws_url,
    server_quic_url: channel.server_quic_url,
    max_concurrency: channel.max_concurrency,
    quota_reserve_percent: channel.quota_reserve_percent,
    ...(driver.category === "subscription" ? {
      subscription: {
        platform: driver.subscriptionProvider!,
        account_label: channel.subscription.account_label,
        max_concurrency: channel.max_concurrency,
        responses_ws_pool_enabled: channel.subscription.responses_ws_pool_enabled,
        responses_ws_subscription_multiplex_probe_enabled:
          channel.subscription.responses_ws_subscription_multiplex_probe_enabled,
        rpm_limit: 0,
        quota_reserve_percent: channel.quota_reserve_percent,
        daily_request_limit: 0,
        cooldown_until_unix: channel.subscription.cooldown_until_unix,
        risk_note: "",
        audit_enabled: false,
      },
    } : {}),
    model_capability_evidence: structuredClone(
      channel.model_capability_evidence
        ?? (channel.capability_profiles as ChannelConfigV5["model_capability_evidence"] | undefined)
        ?? [],
    ),
    detection_evidence: structuredClone(channel.detection_evidence ?? channel.detection_checks ?? []),
  };
}

export function normalizeRendererChannel(channel: ChannelConfig): ChannelConfig {
  return rendererChannelFromV5(normalizeChannelV5(channelV5FromRenderer(channel)), channel);
}

export function hydrateClientConfigV5(config: ClientConfigV5Wire): ClientConfig {
  const channels = (config.channels ?? []).map((channel) => rendererChannelFromV5(channel));
  const fallback = channels[0] ?? channelFromSupplier("channel-1", emptyConfig.supplier);
  return {
    ...emptyConfig,
    ...config,
    config_version: 5,
    channels: channels.length > 0 ? channels : [fallback],
    supplier: supplierFromChannel(fallback),
  };
}

export function channelFromSupplier(id: string, supplier: SupplierConfig): ChannelConfig {
  const sourceDriver = supplier.source_driver ?? "custom_endpoint";
  const driver = sourceDriverById(sourceDriver);
  const initial = sourceDriverInitialChannelV5(driver, {
    id,
    name: supplier.name || "Local Channel",
    createdAtUnixMs: 0,
    credentialRef: supplier.subscription?.credential_ref || supplier.upstream_api_key,
    nodeId: supplier.node_id,
    serverWsUrl: supplier.server_ws_url,
    serverQuicUrl: supplier.server_quic_url,
  });
  const configuredSurfaces = supplier.surface_bindings?.length
    ? structuredClone(supplier.surface_bindings)
    : initial.surfaces.map((surface) => surface.surface === initial.default_target.surface && supplier.upstream_base_url.trim()
      ? { ...surface, base_url: supplier.upstream_base_url }
      : surface);
  return rendererChannelFromV5(normalizeChannelV5({
    ...initial,
    user_agent_profile: supplier.user_agent_profile ?? "",
    enabled: supplier.enabled,
    share_enabled: supplier.enabled,
    surfaces: configuredSurfaces,
    models: [...(supplier.models ?? [])],
    price_ratio: normalizedPriceRatio(supplier.price_ratio),
    model_capability_evidence: structuredClone(
      (supplier.capability_profiles ?? []) as ChannelConfigV5["model_capability_evidence"],
    ),
    detection_evidence: structuredClone(supplier.detection_checks ?? []),
  }), {
    public_model: supplier.public_model,
    upstream_model: supplier.upstream_model,
  });
}

export function supplierFromChannel(channel: ChannelConfig): SupplierConfig {
  return {
    source_driver: channel.source_driver,
    user_agent_profile: channel.user_agent_profile ?? "",
    enabled: channel.enabled && channelIsPlatformShareable(channel),
    kind: channel.kind,
    api_format: channel.api_format,
    node_id: channel.node_id,
    name: channel.name,
    server_ws_url: channel.server_ws_url,
    server_quic_url: channel.server_quic_url,
    upstream_base_url: channel.upstream_base_url,
    upstream_api_key: channel.upstream_api_key,
    public_model: channel.public_model,
    upstream_model: channel.upstream_model,
    models: channel.models ?? [],
    supported_protocols: [...channel.supported_protocols],
    capability_profiles: channel.capability_profiles ?? [],
    detection_checks: channel.detection_checks ?? [],
    surface_bindings: structuredClone(channel.surfaces),
    price_ratio: normalizedPriceRatio(channel.price_ratio),
    subscription: channel.subscription,
  };
}

export { MAX_PRICE_RATIO } from "./pricingPolicy";

export function normalizedPriceRatio(value?: string | number) {
  const ratio = Number(value);
  if (!Number.isFinite(ratio)) return 1;
  return Math.min(MAX_PRICE_RATIO, Math.max(0, ratio));
}

export function priceRatioFromInput(value: string) {
  const ratio = Number(value);
  if (!Number.isFinite(ratio)) return 1;
  return Math.min(MAX_PRICE_RATIO, Math.max(0, ratio));
}

export function priceRatioToDisplay(value?: string | number) {
  return normalizedPriceRatio(value).toFixed(12).replace(/\.?0+$/, "");
}

export function supplierWsFromEndpoint(endpoint: Endpoint) {
  const registryWs = normalizeSupplierWsUrl(endpoint.supplier_ws_url ?? "");
  if (registryWs) return registryWs;
  return wsFromEndpoint(endpoint.base_url);
}

export function supplierQuicFromEndpoint(endpoint: Endpoint) {
  return endpoint.supplier_quic_url?.trim().replace(/\/$/, "") ?? "";
}

export function wsFromEndpoint(baseUrl: string) {
  try {
    const parsed = new URL(baseUrl.trim().replace(/\/$/, ""));
    if (parsed.protocol === "https:") {
      parsed.protocol = "wss:";
    } else if (parsed.protocol === "http:" && isLocalSupplierHost(parsed.hostname)) {
      parsed.protocol = "ws:";
    } else {
      return "";
    }
    parsed.pathname = `${parsed.pathname.replace(/\/$/, "")}/supplier/ws`;
    parsed.search = "";
    parsed.hash = "";
    return parsed.toString().replace(/\/$/, "");
  } catch {
    return "";
  }
}

export function normalizeSupplierWsUrl(raw: string) {
  const trimmed = raw.trim().replace(/\/$/, "");
  if (!trimmed || !supplierWebsocketUrlAllowed(trimmed)) return "";
  return trimmed;
}

export function supplierWebsocketUrlAllowed(raw: string) {
  try {
    const parsed = new URL(raw.trim());
    if (parsed.protocol === "wss:") return true;
    if (parsed.protocol === "ws:") return isLocalSupplierHost(parsed.hostname);
  } catch {
    return false;
  }
  return false;
}

export function isLocalSupplierHost(host: string) {
  const normalized = host.trim().replace(/^\[/, "").replace(/\]$/, "").toLowerCase();
  return normalized === "localhost" || normalized === "::1" || normalized === "0:0:0:0:0:0:0:1" || normalized.startsWith("127.");
}

export function defaultSubscriptionAdapter(platform: string): SubscriptionAdapterConfig {
  return {
    platform,
    account_label: "",
    credential_ref: "",
    max_concurrency: DEFAULT_SUPPLIER_MAX_CONCURRENCY,
    responses_ws_pool_enabled: true,
    responses_ws_subscription_multiplex_probe_enabled: false,
    rpm_limit: 0,
    quota_reserve_percent: 0,
    daily_request_limit: 0,
    cooldown_until_unix: 0,
    risk_note: "",
    audit_enabled: false,
  };
}
