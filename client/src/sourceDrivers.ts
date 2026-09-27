import sourceDriverManifestJson from "../source-drivers.json";
import { DEFAULT_SUPPLIER_MAX_CONCURRENCY } from "./pricingPolicy";
import { reconcileDetectedDefaultModel } from "./supplierEditorState";

export type SourceDriverCategory =
  | "subscription"
  | "official_api"
  | "cloud_platform"
  | "local_runtime"
  | "compatible_gateway";
export type SourceDriverProtocol = "openai_responses" | "openai_chat" | "anthropic_messages" | "gemini_native";
export type SourceDriverSurface = "open_ai" | "anthropic" | "gemini";
// `gemini_subscription` is a persisted compatibility key; its implementation is Antigravity.
export type SourceDriverExecutionKind =
  | "http_surface"
  | "open_ai_subscription"
  | "claude_subscription"
  | "gemini_subscription"
  | "grok_subscription";
export type SourceDriverExecutorLocator =
  | { readonly type: "http_surface" }
  | { readonly type: "retained_subscription"; readonly provider: "openai" | "claude" | "antigravity" | "grok" };
export type SourceDriverTarget = {
  readonly surface: SourceDriverSurface;
  readonly protocol: SourceDriverProtocol;
};
export type SubscriptionResponseMode = "buffered" | "sse";
export type SubscriptionConversionLevel = "c0" | "c1" | "c2" | "c3";
export type SubscriptionUsageConfidence = "reported" | "reported_or_unknown" | "estimated" | "unknown";
export type SubscriptionEvidenceLevel = "declared" | "component_verified" | "mock_verified" | "live_verified";
export type SubscriptionConversionContract = {
  readonly request: SubscriptionConversionLevel;
  readonly response: SubscriptionConversionLevel;
  readonly stream: SubscriptionConversionLevel;
  readonly error: SubscriptionConversionLevel;
};
export type SubscriptionOperationCapability = {
  readonly modes: readonly SubscriptionResponseMode[];
  readonly conversion: SubscriptionConversionContract;
  readonly usageConfidence: SubscriptionUsageConfidence;
  readonly evidence: SubscriptionEvidenceLevel;
  readonly defaultEnabled: boolean;
};
export type SubscriptionSpecialOperation = {
  readonly surface: SourceDriverSurface;
  readonly operation: string;
  readonly capability: SubscriptionOperationCapability;
};
export type SubscriptionDriverContract = {
  readonly version: number;
  readonly acceptedIngressProtocols: readonly SourceDriverProtocol[];
  readonly generation: SubscriptionOperationCapability;
  readonly specialOperations: readonly SubscriptionSpecialOperation[];
};
export type SourceDriverDiscovery = {
  readonly strategy: string;
  readonly catalog: boolean;
  readonly quota: boolean;
  readonly metadata: boolean;
};
export type SourceDriverIconKey =
  | "openai"
  | "anthropic"
  | "gemini"
  | "azure"
  | "bedrock"
  | "ollama"
  | "lmstudio"
  | "vllm"
  | "openrouter"
  | "opencode"
  | "cline"
  | "omniroute"
  | "custom"
  | "network"
  | "claude"
  | "xai"
  | "mistral"
  | "deepseek"
  | "dashscope"
  | "moonshot"
  | "zhipu"
  | "minimax"
  | "stepfun"
  | "groq"
  | "together"
  | "fireworks"
  | "perplexity"
  | "huggingface"
  | "nvidia"
  | "siliconflow"
  | "volcengine"
  | "baidu"
  | "tencent";
export type SourceDriverId =
  | "openai_subscription"
  // Persisted compatibility key for the Antigravity subscription driver.
  | "gemini_subscription"
  | "claude_subscription"
  | "grok_subscription"
  | "openai_api"
  | "anthropic_api"
  | "gemini_api"
  | "xai_api"
  | "mistral_api"
  | "deepseek_api"
  | "dashscope_api"
  | "moonshot_api"
  | "zhipu_api"
  | "minimax_api"
  | "stepfun_api"
  | "azure_openai"
  | "bedrock_mantle"
  | "groq_api"
  | "together_api"
  | "fireworks_api"
  | "perplexity_api"
  | "huggingface_api"
  | "nvidia_api"
  | "siliconflow_api"
  | "volcengine_ark_api"
  | "baidu_qianfan_api"
  | "tencent_hunyuan_api"
  | "ollama"
  | "lm_studio"
  | "vllm"
  | "lan_share"
  | "openrouter"
  | "opencode_go"
  | "opencode_zen"
  | "kilo_gateway"
  | "cline_api"
  | "command_code"
  | "kimi_code"
  | "glm_coding_plan"
  | "minimax_token_plan"
  | "ollama_cloud"
  | "omniroute"
  | "custom_endpoint";

type DetectionPolicy = {
  readonly creation: "guided" | "on_add" | "on_save";
  readonly basicVerification: "surface_preferred";
  readonly advancedConversational: "none" | "driver_preferred_only";
  readonly periodic: {
    readonly catalog: boolean;
    readonly quota: boolean;
    readonly metadata: boolean;
    readonly conversationalProbe: false;
  };
};

export type SourceDriverSurfaceBinding = {
  readonly surface: SourceDriverSurface;
  readonly baseUrl?: string;
  readonly endpointProfile: string;
  readonly authScheme: string;
  readonly protocols: readonly SourceDriverProtocol[];
  readonly preferredProtocol: SourceDriverProtocol;
};

export type SourceDriver = {
  readonly id: SourceDriverId;
  readonly category: SourceDriverCategory;
  readonly order: number;
  readonly title: string;
  readonly description: string;
  readonly iconKey: SourceDriverIconKey;
  readonly actionLabel: string;
  readonly homepageUrl?: string;
  readonly advanced: boolean;
  readonly fixedConnection: boolean;
  readonly executionKind: SourceDriverExecutionKind;
  readonly executor: SourceDriverExecutorLocator;
  readonly defaultTarget: SourceDriverTarget;
  readonly discovery: SourceDriverDiscovery;
  readonly protocolBindings: ReadonlyArray<{ readonly protocol: SourceDriverProtocol; readonly preferred: boolean }>;
  readonly surfaceBindings: readonly SourceDriverSurfaceBinding[];
  readonly subscriptionProvider?: "openai" | "claude" | "antigravity" | "grok";
  readonly subscriptionContract?: SubscriptionDriverContract;
  readonly detection: DetectionPolicy;
  // Temporary renderer compatibility projections. V2 fields above remain authoritative.
  readonly kind: string;
  readonly baseUrl: string;
  readonly apiFormat: SourceDriverProtocol;
  readonly supportedProtocols: readonly SourceDriverProtocol[];
};

export type SourceDriverInitialSurface = {
  surface: SourceDriverSurface;
  base_url: string;
  endpoint_profile: string;
  auth_scheme: string;
  protocols: Array<{
    protocol: SourceDriverProtocol;
    preferred: boolean;
    verification: SourceDriverInitialVerification;
  }>;
  operation_overrides: Array<{ operation: string; method: string; url: string }>;
  verification: SourceDriverInitialVerification;
};

type SourceDriverInitialVerification = {
  state: "declared";
  checked_at_unix: 0;
  summary: "";
};

export type SourceDriverInitialDefaults = {
  sourceDriver: SourceDriverId;
  credentialRef: string;
  executionKind: SourceDriverExecutionKind;
  executor: SourceDriverExecutorLocator;
  surfaces: SourceDriverInitialSurface[];
  defaultTarget: SourceDriverTarget;
  discovery: SourceDriverDiscovery;
  models: string[];
  subscription?: { platform: "openai" | "claude" | "antigravity" | "grok" };
  // Temporary renderer compatibility projections derived from the V2 fields above.
  kind: string;
  baseUrl: string;
  apiFormat: SourceDriverProtocol;
  supportedProtocols: SourceDriverProtocol[];
  surfaceBindings: SourceDriverInitialSurface[];
};

export type ChannelVerificationV5 = {
  state: string;
  checked_at_unix: number;
  summary: string;
};

export type ChannelProtocolBindingV5 = {
  protocol: SourceDriverProtocol;
  preferred: boolean;
  verification: ChannelVerificationV5;
};

export type ChannelOperationOverrideV5 = {
  operation: string;
  method: string;
  url: string;
};

export type ChannelSurfaceBindingV5 = {
  surface: SourceDriverSurface;
  base_url: string;
  endpoint_profile: string;
  auth_scheme: string;
  protocols: ChannelProtocolBindingV5[];
  operation_overrides: ChannelOperationOverrideV5[];
  verification: ChannelVerificationV5;
};

export type ChannelModelCapabilityEvidenceV5 = {
  protocol: string;
  model_pattern?: string;
  catalog_metadata?: boolean;
  context_tokens?: number;
  output_tokens?: number;
  release_status?: string;
  verification_state?: string;
  verified_at_unix?: number;
  [key: string]: unknown;
};

export type ChannelDetectionEvidenceV5 = {
  name: string;
  status: string;
  checked_at_unix?: number;
  protocol?: string;
  capability?: string;
  message?: string;
};

export type ChannelSubscriptionV5 = {
  platform: "openai" | "claude" | "antigravity" | "grok";
  account_label: string;
  max_concurrency: number;
  responses_ws_pool_enabled: boolean;
  responses_ws_subscription_multiplex_probe_enabled: boolean;
  rpm_limit: number;
  quota_reserve_percent: number;
  daily_request_limit: number;
  cooldown_until_unix: number;
  risk_note: string;
  audit_enabled: boolean;
};

export type ChannelConfigV5 = {
  id: string;
  name: string;
  created_at_unix_ms: number;
  source_driver: SourceDriverId;
  enabled: boolean;
  share_enabled: boolean;
  credential_ref: string;
  executor: SourceDriverExecutorLocator;
  surfaces: ChannelSurfaceBindingV5[];
  default_target: SourceDriverTarget;
  discovery: SourceDriverDiscovery;
  user_agent_profile?: string;
  default_model: string;
  models: string[];
  price_ratio: number;
  node_id: string;
  server_ws_url: string;
  server_quic_url: string;
  /** Channel-wide supply capacity. Kept outside subscription because API channels use it too. */
  max_concurrency: number;
  /** Channel-wide quota headroom. Kept outside subscription because API channels use it too. */
  quota_reserve_percent: number;
  subscription?: ChannelSubscriptionV5;
  model_capability_evidence: ChannelModelCapabilityEvidenceV5[];
  detection_evidence: ChannelDetectionEvidenceV5[];
};

export type SourceDriverChannelV5Input = {
  id: string;
  name: string;
  createdAtUnixMs?: number;
  credentialRef?: string;
  nodeId?: string;
  serverWsUrl?: string;
  serverQuicUrl?: string;
};

export type SurfaceProbeResultV2 = {
  surface: SourceDriverSurface;
  protocol: SourceDriverProtocol;
  surface_verification: ChannelVerificationV5;
  protocol_verification: ChannelVerificationV5;
  models: string[];
  model_capability_evidence: ChannelModelCapabilityEvidenceV5[];
  detection_evidence: ChannelDetectionEvidenceV5[];
  checks: string[];
  warnings: string[];
};

export type AvailabilityGroupResult = {
  group: string;
  representative: string;
  upstream_model: string;
  status: string;
  cause: string;
  rule_id: string;
  message: string;
  attempts: number;
  checked_at: number;
  next_probe_at: number;
  scope_inferred: boolean;
};

export type ChannelDetectionResultV2 = {
  availability?: { attempts: number; groups: AvailabilityGroupResult[] };
  models: string[];
  surface_results: SurfaceProbeResultV2[];
  model_capability_evidence: ChannelModelCapabilityEvidenceV5[];
  detection_evidence: ChannelDetectionEvidenceV5[];
  quota_status: string;
  remaining_ratio: number;
  quota_windows: unknown[];
  checks: string[];
  warnings: string[];
};

type RawManifest = {
  version: number;
  drivers: RawDriver[];
};

type RawDriver = {
  id: string;
  category: string;
  order: number;
  title: string;
  description: string;
  icon_key: string;
  action_label: string;
  homepage_url?: string;
  advanced?: boolean;
  fixed_connection?: boolean;
  execution_kind: string;
  executor: { type: string; provider?: string };
  default_target: { surface: string; protocol: string };
  discovery: { strategy: string; catalog: boolean; quota: boolean; metadata: boolean };
  kind: string;
  subscription_provider?: string;
  subscription_contract?: RawSubscriptionDriverContract;
  protocols: Array<{ protocol: string; preferred?: boolean }>;
  surfaces: Array<{
    surface: string;
    base_url?: string;
    endpoint_profile: string;
    auth_scheme: string;
    protocols: string[];
    preferred_protocol: string;
  }>;
  creation: {
    trigger: string;
    basic_verification: string;
    advanced_conversational: string;
  };
  periodic: {
    catalog: boolean;
    quota: boolean;
    metadata: boolean;
    conversational_probe: boolean;
  };
};

type RawSubscriptionDriverContract = {
  version: number;
  accepted_ingress_protocols: string[];
  generation: RawSubscriptionOperationCapability;
  special_operations: Array<{
    surface: string;
    operation: string;
    capability: RawSubscriptionOperationCapability;
  }>;
};

type RawSubscriptionOperationCapability = {
  modes: string[];
  conversion: {
    request: string;
    response: string;
    stream: string;
    error: string;
  };
  usage_confidence: string;
  evidence: string;
  default_enabled: boolean;
};

const DRIVER_IDS = new Set<SourceDriverId>([
  "openai_subscription", "gemini_subscription", "claude_subscription", "grok_subscription", "openai_api",
  "anthropic_api", "gemini_api", "xai_api", "mistral_api", "deepseek_api",
  "dashscope_api", "moonshot_api", "zhipu_api", "minimax_api", "stepfun_api",
  "azure_openai", "bedrock_mantle", "groq_api", "together_api", "fireworks_api",
  "perplexity_api", "huggingface_api", "nvidia_api", "siliconflow_api",
  "volcengine_ark_api", "baidu_qianfan_api", "tencent_hunyuan_api", "ollama",
  "lm_studio", "vllm", "lan_share", "openrouter", "omniroute", "custom_endpoint",
  "opencode_go", "opencode_zen", "kilo_gateway", "cline_api",
  "command_code", "kimi_code", "glm_coding_plan", "minimax_token_plan", "ollama_cloud",
]);
const CATEGORIES = new Set<SourceDriverCategory>([
  "subscription",
  "official_api",
  "cloud_platform",
  "local_runtime",
  "compatible_gateway",
]);
const CATEGORY_ORDER: Record<SourceDriverCategory, number> = {
  subscription: 0,
  official_api: 1,
  cloud_platform: 2,
  local_runtime: 3,
  compatible_gateway: 4,
};
const API_ACCESS_CATEGORY_ORDER: Record<Exclude<SourceDriverCategory, "subscription">, number> = {
  official_api: 0,
  cloud_platform: 1,
  compatible_gateway: 2,
  local_runtime: 3,
};
const PROTOCOLS = new Set<SourceDriverProtocol>(["openai_responses", "openai_chat", "anthropic_messages", "gemini_native"]);
const SURFACES = new Set<SourceDriverSurface>(["open_ai", "anthropic", "gemini"]);
const ICONS = new Set<SourceDriverIconKey>([
  "openai", "anthropic", "gemini", "azure", "bedrock", "ollama", "lmstudio",
  "vllm", "openrouter", "opencode", "cline", "omniroute", "custom", "network", "claude", "xai", "mistral",
  "deepseek", "dashscope", "moonshot", "zhipu", "minimax", "stepfun", "groq",
  "together", "fireworks", "perplexity", "huggingface", "nvidia",
  "siliconflow", "volcengine", "baidu", "tencent",
]);
const CREATION_TRIGGERS = new Set<DetectionPolicy["creation"]>(["guided", "on_add", "on_save"]);
const BASIC_VERIFICATION_POLICIES = new Set<DetectionPolicy["basicVerification"]>(["surface_preferred"]);
const ADVANCED_VERIFICATION_POLICIES = new Set<DetectionPolicy["advancedConversational"]>(["none", "driver_preferred_only"]);
const SUBSCRIPTION_RESPONSE_MODES = new Set<SubscriptionResponseMode>(["buffered", "sse"]);
const SUBSCRIPTION_CONVERSION_LEVELS = new Set<SubscriptionConversionLevel>(["c0", "c1", "c2", "c3"]);
const SUBSCRIPTION_USAGE_CONFIDENCE = new Set<SubscriptionUsageConfidence>([
  "reported", "reported_or_unknown", "estimated", "unknown",
]);
const SUBSCRIPTION_EVIDENCE_LEVELS = new Set<SubscriptionEvidenceLevel>([
  "declared", "component_verified", "mock_verified", "live_verified",
]);

const manifest = parseSourceDriverManifest(sourceDriverManifestJson as unknown);
export const SOURCE_DRIVER_MANIFEST_VERSION = manifest.version;
export const SOURCE_DRIVERS = manifest.drivers;

export function parseSourceDriverManifest(value: unknown): { version: number; drivers: readonly SourceDriver[] } {
  const raw = parseManifest(value);
  return {
    version: raw.version,
    drivers: deepFreeze(
      raw.drivers
        .map(toSourceDriver)
        .sort((left, right) => CATEGORY_ORDER[left.category] - CATEGORY_ORDER[right.category] || left.order - right.order),
    ),
  };
}

export function sourceDriverById(id: SourceDriverId): SourceDriver {
  const driver = SOURCE_DRIVERS.find((candidate) => candidate.id === id);
  if (!driver) throw new Error(`Unknown source driver: ${id}`);
  return driver;
}

export function sourceDriversForCategory(category: SourceDriverCategory): readonly SourceDriver[] {
  return SOURCE_DRIVERS.filter((driver) => driver.category === category);
}

export function sourceDriversForApiAccess(): readonly SourceDriver[] {
  return SOURCE_DRIVERS
    .filter((driver) => driver.category !== "subscription"
      && driver.category !== "local_runtime"
      && driver.id !== "custom_endpoint"
      && driver.id !== "omniroute"
      && driver.id !== "lan_share")
    .sort((left, right) => {
      return API_ACCESS_CATEGORY_ORDER[left.category as Exclude<SourceDriverCategory, "subscription">]
        - API_ACCESS_CATEGORY_ORDER[right.category as Exclude<SourceDriverCategory, "subscription">]
        || left.order - right.order;
    });
}

export function sourceDriversForLanSharing(): readonly SourceDriver[] {
  return SOURCE_DRIVERS.filter((driver) => driver.id === "lan_share");
}

export function sourceDriversForCustom(): readonly SourceDriver[] {
  return SOURCE_DRIVERS.filter((driver) => driver.id === "custom_endpoint");
}

export function sourceDriverInitialChannelDefaults(driver: SourceDriver): SourceDriverInitialDefaults {
  const surfaces = driver.surfaceBindings.map((binding) => ({
    surface: binding.surface,
    base_url: binding.baseUrl ?? "",
    endpoint_profile: binding.endpointProfile,
    auth_scheme: binding.authScheme,
    protocols: binding.protocols.map((protocol) => ({
      protocol,
      preferred: protocol === binding.preferredProtocol,
      verification: declaredVerification(),
    })),
    operation_overrides: [],
    verification: declaredVerification(),
  }));
  return {
    sourceDriver: driver.id,
    credentialRef: "",
    executionKind: driver.executionKind,
    executor: { ...driver.executor },
    surfaces,
    defaultTarget: { ...driver.defaultTarget },
    discovery: { ...driver.discovery },
    models: [],
    ...(driver.subscriptionProvider ? { subscription: { platform: driver.subscriptionProvider } } : {}),
    kind: driver.kind,
    baseUrl: driver.baseUrl,
    apiFormat: driver.apiFormat,
    supportedProtocols: [...driver.supportedProtocols],
    surfaceBindings: surfaces.map(cloneInitialSurface),
  };
}

export function sourceDriverInitialChannelV5(
  driver: SourceDriver,
  input: SourceDriverChannelV5Input,
): ChannelConfigV5 {
  const defaults = sourceDriverInitialChannelDefaults(driver);
  const channel: ChannelConfigV5 = {
    id: input.id,
    name: input.name,
    created_at_unix_ms: input.createdAtUnixMs ?? 0,
    source_driver: defaults.sourceDriver,
    enabled: false,
    share_enabled: false,
    credential_ref: input.credentialRef ?? "",
    executor: { ...defaults.executor },
    surfaces: defaults.surfaces.map(cloneInitialSurface),
    default_target: { ...defaults.defaultTarget },
    discovery: { ...defaults.discovery },
    user_agent_profile: "",
    default_model: "",
    models: [],
    price_ratio: 1,
    node_id: input.nodeId ?? "",
    server_ws_url: input.serverWsUrl ?? "",
    server_quic_url: input.serverQuicUrl ?? "",
    max_concurrency: DEFAULT_SUPPLIER_MAX_CONCURRENCY,
    quota_reserve_percent: 0,
    model_capability_evidence: [],
    detection_evidence: [],
    ...(defaults.subscription ? {
      subscription: {
        platform: defaults.subscription.platform,
        account_label: "",
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
    } : {}),
  };
  return normalizeChannelV5(channel);
}

export function normalizeChannelV5<T extends ChannelConfigV5>(channel: T): T {
  const normalized = structuredClone(channel);
  // V5 keeps share_enabled on the wire for older clients, but enabled is the
  // only product switch. LAN share channels are intentionally local-only.
  normalized.share_enabled = normalized.enabled && normalized.source_driver !== "lan_share";
  const userAgentProfile = (normalized.user_agent_profile ?? "").trim().toLowerCase();
  if (userAgentProfile) normalized.user_agent_profile = userAgentProfile;
  else delete normalized.user_agent_profile;
  const legacySubscription = normalized.subscription;
  const rawMaxConcurrency = Number(
    normalized.max_concurrency ?? legacySubscription?.max_concurrency ?? DEFAULT_SUPPLIER_MAX_CONCURRENCY,
  );
  normalized.max_concurrency = Number.isFinite(rawMaxConcurrency)
    ? Math.min(0xffff_ffff, Math.max(0, Math.trunc(rawMaxConcurrency)))
    : DEFAULT_SUPPLIER_MAX_CONCURRENCY;
  const rawQuotaReservePercent = Number(
    normalized.quota_reserve_percent ?? legacySubscription?.quota_reserve_percent ?? 0,
  );
  normalized.quota_reserve_percent = Number.isFinite(rawQuotaReservePercent)
    ? Math.min(100, Math.max(0, Math.trunc(rawQuotaReservePercent)))
    : 0;
  if (normalized.subscription) {
    // Keep the former subscription fields as a compatibility mirror for clients
    // that predate the channel-wide capacity fields.
    normalized.subscription.max_concurrency = normalized.max_concurrency;
    normalized.subscription.quota_reserve_percent = normalized.quota_reserve_percent;
    normalized.subscription.responses_ws_pool_enabled =
      normalized.subscription.responses_ws_pool_enabled ?? true;
    normalized.subscription.responses_ws_subscription_multiplex_probe_enabled =
      normalized.subscription.responses_ws_subscription_multiplex_probe_enabled ?? false;
    normalized.subscription.rpm_limit = 0;
    normalized.subscription.quota_reserve_percent = Math.min(
      100,
      Math.max(0, Number(normalized.subscription.quota_reserve_percent) || 0),
    );
    normalized.subscription.daily_request_limit = 0;
    normalized.subscription.risk_note = "";
    normalized.subscription.audit_enabled = false;
  }
  normalized.id = normalized.id.trim();
  normalized.name = normalized.name.trim();
  const createdAtUnixMs = Number(normalized.created_at_unix_ms ?? 0);
  normalized.created_at_unix_ms = Number.isSafeInteger(createdAtUnixMs) && createdAtUnixMs > 0
    ? createdAtUnixMs
    : 0;
  normalized.credential_ref = normalized.credential_ref.trim();
  normalized.node_id = normalized.node_id.trim();
  normalized.server_ws_url = normalized.server_ws_url.trim();
  normalized.server_quic_url = normalized.server_quic_url.trim();
  normalized.models = [...normalized.models];
  const configuredDefault = (normalized.default_model ?? "").trim();
  normalized.default_model = normalized.models.find(
    (model) => model.trim().toLowerCase() === configuredDefault.toLowerCase(),
  ) ?? normalized.models[0] ?? configuredDefault;
  normalized.model_capability_evidence = structuredClone(normalized.model_capability_evidence ?? []);
  normalized.detection_evidence = structuredClone(normalized.detection_evidence ?? []);
  normalized.surfaces = normalized.surfaces.map((surface) => ({
    ...surface,
    base_url: normalizeBaseUrl(surface.base_url),
    endpoint_profile: surface.endpoint_profile.trim(),
    auth_scheme: surface.auth_scheme.trim(),
    protocols: surface.protocols.map((binding) => ({
      ...binding,
      verification: normalizeVerification(binding.verification),
    })),
    operation_overrides: surface.operation_overrides.map((override) => ({
      operation: override.operation.trim(),
      method: override.method.trim().toUpperCase(),
      url: normalizeBaseUrl(override.url),
    })),
    verification: normalizeVerification(surface.verification),
  }));
  if (normalized.source_driver === "lan_share") {
    const openAiBase = normalized.surfaces.find((surface) => surface.surface === "open_ai")?.base_url;
    if (openAiBase) {
      try {
        const root = new URL(openAiBase);
        root.pathname = root.pathname.replace(/\/(?:v1|anthropic|gemini)\/?$/i, "").replace(/\/$/, "");
        root.search = "";
        root.hash = "";
        normalized.surfaces = normalized.surfaces.map((surface) => {
          const next = new URL(root.toString());
          const suffix = surface.surface === "open_ai"
            ? "v1"
            : surface.surface === "anthropic" ? "anthropic" : "gemini";
          next.pathname = `${root.pathname.replace(/\/$/, "")}/${suffix}`;
          return { ...surface, base_url: normalizeBaseUrl(next.toString()) };
        });
      } catch {
        // Validation below reports the original malformed URL without hiding it.
      }
    }
  }
  // The original fixed Zen preset predates its Gemini surface. Add only that
  // declaration; validation below still rejects altered URLs/protocols/policies.
  if (normalized.source_driver === "opencode_zen" && normalized.surfaces.length === 2
    && !normalized.surfaces.some((surface) => surface.surface === "gemini")) {
    const gemini = sourceDriverInitialChannelDefaults(sourceDriverById("opencode_zen"))
      .surfaces.find((surface) => surface.surface === "gemini")!;
    normalized.surfaces.push(gemini);
  }
  validateChannelV5(normalized);
  return normalized;
}

export function validateChannelV5(channel: ChannelConfigV5): void {
  if (!channel.id?.trim() || !channel.name?.trim()) throw new Error("V5 channel id and name are required");
  if (!Number.isSafeInteger(channel.created_at_unix_ms) || channel.created_at_unix_ms < 0) {
    throw new Error("V5 channel creation time must be a non-negative integer");
  }
  const driver = sourceDriverById(channel.source_driver);
  if (!sameExecutor(channel.executor, driver.executor)) {
    throw new Error("V5 channel executor must match immutable source_driver");
  }
  if (!channel.discovery?.strategy?.trim()) throw new Error("V5 channel discovery strategy is required");
  if (!Array.isArray(channel.surfaces) || channel.surfaces.length === 0) {
    throw new Error("V5 channel must declare at least one surface binding");
  }

  const seenSurfaces = new Set<SourceDriverSurface>();
  for (const surface of channel.surfaces) {
    if (!SURFACES.has(surface.surface) || seenSurfaces.has(surface.surface)) {
      throw new Error(`V5 channel has an invalid or duplicate surface: ${surface.surface}`);
    }
    seenSurfaces.add(surface.surface);
    if (driver.executionKind === "http_surface") {
      parseStrictHttpBaseUrl(surface.base_url, "surface base URL");
    } else if (surface.base_url.trim()) {
      throw new Error("retained subscription surface must not declare a base URL");
    }
    if (!surface.endpoint_profile?.trim() || !surface.auth_scheme?.trim()) {
      throw new Error(`${surface.surface} surface endpoint and auth policy are required`);
    }
    if (!Array.isArray(surface.protocols) || surface.protocols.length === 0) {
      throw new Error(`${surface.surface} surface must declare protocols`);
    }
    const seenProtocols = new Set<SourceDriverProtocol>();
    let preferredCount = 0;
    for (const binding of surface.protocols) {
      if (!PROTOCOLS.has(binding.protocol)
        || surfaceForProtocol(binding.protocol) !== surface.surface
        || seenProtocols.has(binding.protocol)) {
        throw new Error(`${surface.surface} surface has an invalid protocol binding`);
      }
      seenProtocols.add(binding.protocol);
      if (binding.preferred) preferredCount += 1;
    }
    if (preferredCount !== 1) {
      throw new Error(`${surface.surface} surface must declare exactly one preferred protocol`);
    }
    if (surface.operation_overrides.length > 0 && !driver.advanced) {
      throw new Error("operation endpoint overrides require an advanced source driver");
    }
    const base = driver.executionKind === "http_surface"
      ? parseStrictHttpBaseUrl(surface.base_url, "surface base URL")
      : undefined;
    const seenOverrides = new Set<string>();
    for (const override of surface.operation_overrides) {
      if (!override.operation || !override.method || !override.url) {
        throw new Error("operation endpoint override is incomplete");
      }
      const key = `${override.method.toUpperCase()} ${override.operation.toLowerCase()}`;
      if (seenOverrides.has(key)) throw new Error("duplicate operation endpoint override");
      seenOverrides.add(key);
      const overrideUrl = parseStrictHttpBaseUrl(override.url, "operation endpoint override URL");
      if (!base || overrideUrl.origin !== base.origin) {
        throw new Error("operation endpoint override cannot replace the configured authority");
      }
    }
  }

  if (!channel.surfaces.some((surface) => surface.surface === channel.default_target.surface
    && surface.protocols.some((binding) => binding.protocol === channel.default_target.protocol))) {
    throw new Error("V5 channel default_target must reference a declared surface/protocol binding");
  }
  if (driver.fixedConnection) {
    const matchesPreset = channel.default_target.surface === driver.defaultTarget.surface
      && channel.default_target.protocol === driver.defaultTarget.protocol
      && channel.discovery.strategy === driver.discovery.strategy
      && channel.discovery.catalog === driver.discovery.catalog
      && channel.discovery.quota === driver.discovery.quota
      && channel.discovery.metadata === driver.discovery.metadata
      && channel.surfaces.length === driver.surfaceBindings.length
      && channel.surfaces.every((surface) => {
        const preset = driver.surfaceBindings.find((item) => item.surface === surface.surface);
        return preset
          && normalizeBaseUrl(new URL(surface.base_url).href) === normalizeBaseUrl(new URL(preset.baseUrl!).href)
          && surface.endpoint_profile === preset.endpointProfile
          && surface.auth_scheme === preset.authScheme
          && surface.operation_overrides.length === 0
          && surface.protocols.length === preset.protocols.length
          && surface.protocols.every((binding) => preset.protocols.includes(binding.protocol)
            && binding.preferred === (binding.protocol === preset.preferredProtocol));
      });
    if (!matchesPreset) {
      throw new Error(`${driver.title} uses fixed official endpoints and protocols. Use Custom API Endpoint for other connections.`);
    }
  }
}

export function serializeChannelV5(channel: ChannelConfigV5): ChannelConfigV5 {
  const normalized = normalizeChannelV5(channel);
  const driver = sourceDriverById(normalized.source_driver);
  return {
    id: normalized.id,
    name: normalized.name,
    created_at_unix_ms: normalized.created_at_unix_ms,
    source_driver: normalized.source_driver,
    enabled: normalized.enabled,
    share_enabled: normalized.enabled && normalized.source_driver !== "lan_share",
    credential_ref: normalized.credential_ref,
    executor: structuredClone(normalized.executor),
    surfaces: structuredClone(normalized.surfaces),
    default_target: structuredClone(normalized.default_target),
    discovery: structuredClone(normalized.discovery),
    ...(normalized.user_agent_profile ? { user_agent_profile: normalized.user_agent_profile } : {}),
    default_model: normalized.default_model,
    models: [...normalized.models],
    price_ratio: normalized.price_ratio,
    node_id: normalized.node_id,
    server_ws_url: normalized.server_ws_url,
    server_quic_url: normalized.server_quic_url,
    max_concurrency: normalized.max_concurrency,
    quota_reserve_percent: normalized.quota_reserve_percent,
    ...(driver.category === "subscription" && normalized.subscription
      ? { subscription: structuredClone(normalized.subscription) }
      : {}),
    model_capability_evidence: structuredClone(normalized.model_capability_evidence),
    detection_evidence: structuredClone(normalized.detection_evidence),
  };
}

export function serializeClientConfigV5<
  T extends { config_version: number; channels: ChannelConfigV5[]; supplier?: unknown },
>(config: T): Omit<T, "config_version" | "channels" | "supplier"> & {
  config_version: 5;
  channels: ChannelConfigV5[];
} {
  const { config_version: _legacyVersion, channels, supplier: _legacySupplier, ...rest } = config;
  return {
    ...rest,
    config_version: 5,
    channels: channels.map(serializeChannelV5),
  };
}

export function reloadChannelV5(value: unknown): ChannelConfigV5 {
  if (!value || typeof value !== "object") throw new Error("V5 channel must be an object");
  return normalizeChannelV5(structuredClone(value) as ChannelConfigV5);
}

export function applyChannelDetectionV2<T extends ChannelConfigV5>(
  channel: T,
  detection: ChannelDetectionResultV2,
): T & {
  quota_status: string;
  remaining_ratio: number;
  quota_windows: unknown[];
  detection_checks: string[];
  detection_warnings: string[];
} {
  const normalized = normalizeChannelV5(channel);
  const surfaces = normalized.surfaces.map((surface) => {
    const pairResults = detection.surface_results.filter((result) => result.surface === surface.surface);
    if (pairResults.length === 0) return surface;
    return {
      ...surface,
      verification: normalizeVerification(pairResults[pairResults.length - 1].surface_verification),
      protocols: surface.protocols.map((binding) => {
        const result = pairResults.find((candidate) => candidate.protocol === binding.protocol);
        return result ? {
          ...binding,
          verification: normalizeVerification(result.protocol_verification),
        } : binding;
      }),
    };
  });
  const reconciledModels = reconcileDetectedDefaultModel(
    normalized.default_model,
    detection.models,
  );
  return {
    ...channel,
    ...normalized,
    surfaces,
    default_model: reconciledModels.defaultModel,
    models: reconciledModels.models,
    model_capability_evidence: structuredClone(detection.model_capability_evidence ?? []),
    detection_evidence: structuredClone(detection.detection_evidence ?? []),
    quota_status: detection.quota_status ?? "",
    remaining_ratio: detection.remaining_ratio ?? 0,
    quota_windows: structuredClone(detection.quota_windows ?? []),
    detection_checks: [...(detection.checks ?? [])],
    detection_warnings: [...(detection.warnings ?? [])],
  };
}

function normalizeVerification(value: ChannelVerificationV5 | undefined): ChannelVerificationV5 {
  return {
    state: value?.state?.trim() || "declared",
    checked_at_unix: Number.isFinite(value?.checked_at_unix) ? Number(value?.checked_at_unix) : 0,
    summary: value?.summary?.trim() || "",
  };
}

function sameExecutor(left: SourceDriverExecutorLocator, right: SourceDriverExecutorLocator): boolean {
  return left.type === right.type
    && (left.type === "http_surface" || right.type === "http_surface" || left.provider === right.provider);
}

function normalizeBaseUrl(value: string): string {
  const trimmed = value.trim();
  if (!trimmed) return "";
  return trimmed.replace(/\/+$/, "");
}

function parseStrictHttpBaseUrl(value: string, label: string): URL {
  const trimmed = value.trim();
  if (trimmed.includes("\\")) throw new Error(`${label} contains an invalid authority separator`);
  let url: URL;
  try {
    url = new URL(trimmed);
  } catch {
    throw new Error(`${label} is invalid`);
  }
  if ((url.protocol !== "http:" && url.protocol !== "https:")
    || !url.hostname
    || url.username
    || url.password
    || url.search
    || url.hash) {
    throw new Error(`${label} must use a strict HTTP authority without credentials, query, or fragment`);
  }
  return url;
}

function parseManifest(value: unknown): RawManifest {
  if (!value || typeof value !== "object") throw new Error("Source driver manifest must be an object");
  const candidate = value as Partial<RawManifest>;
  if (!Number.isInteger(candidate.version) || Number(candidate.version) <= 0 || !Array.isArray(candidate.drivers)) {
    throw new Error("Source driver manifest has an invalid version or driver list");
  }
  const ids = new Set<string>();
  for (const driver of candidate.drivers) validateRawDriver(driver, ids);
  if (ids.size !== DRIVER_IDS.size || [...DRIVER_IDS].some((id) => !ids.has(id))) {
    throw new Error("Source driver manifest must contain every source driver exactly once");
  }
  return candidate as RawManifest;
}

function validateRawDriver(driver: RawDriver, ids: Set<string>) {
  if (!DRIVER_IDS.has(driver.id as SourceDriverId) || ids.has(driver.id)) throw new Error(`Invalid or duplicate source driver: ${driver.id}`);
  ids.add(driver.id);
  if (!CATEGORIES.has(driver.category as SourceDriverCategory) || !ICONS.has(driver.icon_key as SourceDriverIconKey)) {
    throw new Error(`Source driver ${driver.id} has invalid category or icon`);
  }
  if (!Number.isInteger(driver.order) || driver.order <= 0
    || !driver.title?.trim() || !driver.description?.trim() || !driver.action_label?.trim() || !driver.kind?.trim()) {
    throw new Error(`Source driver ${driver.id} has incomplete metadata`);
  }
  if (driver.homepage_url !== undefined && !isStrictHttpsUrl(driver.homepage_url)) {
    throw new Error(`Source driver ${driver.id} has an invalid homepage URL`);
  }
  if (!CREATION_TRIGGERS.has(driver.creation?.trigger as DetectionPolicy["creation"])
    || !BASIC_VERIFICATION_POLICIES.has(driver.creation?.basic_verification as DetectionPolicy["basicVerification"])
    || !ADVANCED_VERIFICATION_POLICIES.has(driver.creation?.advanced_conversational as DetectionPolicy["advancedConversational"])) {
    throw new Error(`Source driver ${driver.id} has an invalid creation policy`);
  }
  if (driver.periodic?.conversational_probe !== false) {
    throw new Error(`Source driver ${driver.id} must not run periodic conversational probes`);
  }
  if (!driver.periodic.catalog && !driver.periodic.quota && !driver.periodic.metadata) {
    throw new Error(`Source driver ${driver.id} has no periodic metadata policy`);
  }
  const expectedExecution = expectedExecutionKind(driver.id as SourceDriverId);
  if (driver.execution_kind !== expectedExecution || !validExecutor(driver, expectedExecution)) {
    throw new Error(`Source driver ${driver.id} has an invalid executor contract`);
  }
  if (!driver.discovery?.strategy?.trim()
    || (!driver.discovery.catalog && !driver.discovery.quota && !driver.discovery.metadata)) {
    throw new Error(`Source driver ${driver.id} has an invalid discovery policy`);
  }
  const protocols = driver.protocols.map((binding) => binding.protocol);
  if (protocols.length === 0
    || new Set(protocols).size !== protocols.length
    || protocols.some((protocol) => !PROTOCOLS.has(protocol as SourceDriverProtocol))) {
    throw new Error(`Source driver ${driver.id} has invalid protocols`);
  }
  if (driver.protocols.filter((binding) => binding.preferred).length !== 1) {
    throw new Error(`Source driver ${driver.id} must have exactly one default protocol`);
  }
  const surfaceIds = driver.surfaces.map((binding) => binding.surface);
  if (surfaceIds.length === 0
    || new Set(surfaceIds).size !== surfaceIds.length
    || surfaceIds.some((surface) => !SURFACES.has(surface as SourceDriverSurface))) {
    throw new Error(`Source driver ${driver.id} has invalid surfaces`);
  }
  for (const surface of driver.surfaces) {
    const surfaceProtocols = new Set(surface.protocols);
    if (!surface.endpoint_profile?.trim() || !surface.auth_scheme?.trim()
      || surfaceProtocols.size !== surface.protocols.length
      || !surface.protocols.includes(surface.preferred_protocol)
      || surface.protocols.some((protocol) =>
        !protocols.includes(protocol) || surfaceForProtocol(protocol as SourceDriverProtocol) !== surface.surface)) {
      throw new Error(`Source driver ${driver.id} has an invalid surface binding`);
    }
    if (expectedExecution === "http_surface") {
      if (!surface.base_url || !isStrictHttpBaseUrl(surface.base_url)) {
        throw new Error(`Source driver ${driver.id} has an invalid HTTP surface base URL`);
      }
    } else if (surface.base_url !== undefined) {
      throw new Error(`Source driver ${driver.id} subscription surface cannot declare a base URL`);
    }
  }
  if (!driver.surfaces.some((surface) =>
    surface.surface === driver.default_target?.surface
      && surface.protocols.includes(driver.default_target?.protocol))) {
    throw new Error(`Source driver ${driver.id} has a dangling default target`);
  }
  validateSubscriptionContract(driver, expectedExecution);
}

function validateSubscriptionContract(
  driver: RawDriver,
  executionKind: SourceDriverExecutionKind,
): void {
  if (executionKind === "http_surface") {
    if (driver.subscription_contract !== undefined) {
      throw new Error(`Source driver ${driver.id} HTTP executor cannot declare a subscription contract`);
    }
    return;
  }

  const contract = driver.subscription_contract;
  if (!contract || !Number.isInteger(contract.version) || contract.version <= 0) {
    throw new Error(`Source driver ${driver.id} requires a versioned subscription contract`);
  }
  if (!Array.isArray(contract.accepted_ingress_protocols)
    || contract.accepted_ingress_protocols.length === 0
    || new Set(contract.accepted_ingress_protocols).size !== contract.accepted_ingress_protocols.length
    || contract.accepted_ingress_protocols.some((protocol) => !PROTOCOLS.has(protocol as SourceDriverProtocol))) {
    throw new Error(`Source driver ${driver.id} has invalid subscription ingress protocols`);
  }
  if (driver.protocols.some((binding) =>
    !contract.accepted_ingress_protocols.includes(binding.protocol))) {
    throw new Error(`Source driver ${driver.id} subscription contract omits a native protocol`);
  }
  validateSubscriptionCapability(driver.id, "generation", contract.generation, true);
  if (!contract.generation.modes.includes("buffered") || !contract.generation.modes.includes("sse")) {
    throw new Error(`Source driver ${driver.id} generation contract must support buffered and SSE modes`);
  }

  const seen = new Set<string>();
  for (const special of contract.special_operations ?? []) {
    const operation = special.operation?.trim();
    if (!SURFACES.has(special.surface as SourceDriverSurface)
      || !operation
      || !subscriptionOperationBelongsToSurface(special.surface, operation)) {
      throw new Error(`Source driver ${driver.id} has an invalid subscription special operation`);
    }
    const key = `${special.surface}:${operation}`;
    if (seen.has(key)) {
      throw new Error(`Source driver ${driver.id} has a duplicate subscription special operation`);
    }
    seen.add(key);
    validateSubscriptionCapability(driver.id, operation, special.capability, false);
  }
}

function validateSubscriptionCapability(
  driverId: string,
  operation: string,
  capability: RawSubscriptionOperationCapability,
  requireEnabled: boolean,
): void {
  if (!capability
    || !Array.isArray(capability.modes)
    || capability.modes.length === 0
    || new Set(capability.modes).size !== capability.modes.length
    || capability.modes.some((mode) => !SUBSCRIPTION_RESPONSE_MODES.has(mode as SubscriptionResponseMode))) {
    throw new Error(`Source driver ${driverId} operation ${operation} has invalid response modes`);
  }
  const conversion = capability.conversion;
  if (!conversion
    || Object.values(conversion).some((level) =>
      !SUBSCRIPTION_CONVERSION_LEVELS.has(level as SubscriptionConversionLevel))) {
    throw new Error(`Source driver ${driverId} operation ${operation} has invalid conversion levels`);
  }
  const hasSse = capability.modes.includes("sse");
  if (hasSse === (conversion.stream === "c3")) {
    throw new Error(`Source driver ${driverId} operation ${operation} has inconsistent SSE conversion`);
  }
  if (conversion.request === "c3" || conversion.response === "c3" || conversion.error === "c3") {
    throw new Error(`Source driver ${driverId} operation ${operation} declares an unusable conversion`);
  }
  if (!SUBSCRIPTION_USAGE_CONFIDENCE.has(capability.usage_confidence as SubscriptionUsageConfidence)
    || !SUBSCRIPTION_EVIDENCE_LEVELS.has(capability.evidence as SubscriptionEvidenceLevel)
    || typeof capability.default_enabled !== "boolean") {
    throw new Error(`Source driver ${driverId} operation ${operation} has invalid evidence or usage policy`);
  }
  if (requireEnabled && !capability.default_enabled) {
    throw new Error(`Source driver ${driverId} generation contract must be enabled`);
  }
  if (capability.default_enabled
    && ([conversion.request, conversion.response, conversion.error].some((level) => level === "c2")
      || (hasSse && conversion.stream === "c2"))) {
    throw new Error(`Source driver ${driverId} operation ${operation} cannot default-enable c2/c3 conversion`);
  }
}

function subscriptionOperationBelongsToSurface(surface: string, operation: string): boolean {
  if (operation === "responses_compact"
    || operation === "realtime_calls_create"
    || operation === "realtime_live_call_create") {
    return surface === "open_ai";
  }
  if (operation === "count_tokens") return surface === "anthropic" || surface === "gemini";
  if (operation === "embed_content") return surface === "gemini";
  return false;
}

function toSourceDriver(raw: RawDriver): SourceDriver {
  const preferred = raw.protocols.find((binding) => binding.preferred)?.protocol as SourceDriverProtocol;
  const preferredSurface = raw.surfaces.find((binding) => binding.protocols.includes(preferred));
  return {
    id: raw.id as SourceDriverId,
    category: raw.category as SourceDriverCategory,
    order: raw.order,
    title: raw.title,
    description: raw.description,
    iconKey: raw.icon_key as SourceDriverIconKey,
    actionLabel: raw.action_label,
    homepageUrl: raw.homepage_url?.trim() || undefined,
    advanced: Boolean(raw.advanced),
    fixedConnection: Boolean(raw.fixed_connection),
    executionKind: raw.execution_kind as SourceDriverExecutionKind,
    executor: raw.executor as SourceDriverExecutorLocator,
    defaultTarget: {
      surface: raw.default_target.surface as SourceDriverSurface,
      protocol: raw.default_target.protocol as SourceDriverProtocol,
    },
    discovery: { ...raw.discovery },
    protocolBindings: raw.protocols.map((binding) => ({
      protocol: binding.protocol as SourceDriverProtocol,
      preferred: Boolean(binding.preferred),
    })),
    kind: raw.kind,
    baseUrl: preferredSurface?.base_url ?? "",
    apiFormat: preferred,
    supportedProtocols: raw.protocols.map((binding) => binding.protocol as SourceDriverProtocol),
    surfaceBindings: raw.surfaces.map((binding) => ({
      surface: binding.surface as SourceDriverSurface,
      baseUrl: binding.base_url,
      endpointProfile: binding.endpoint_profile,
      authScheme: binding.auth_scheme,
      protocols: binding.protocols as SourceDriverProtocol[],
      preferredProtocol: binding.preferred_protocol as SourceDriverProtocol,
    })),
    subscriptionProvider: raw.subscription_provider as SourceDriver["subscriptionProvider"],
    subscriptionContract: raw.subscription_contract
      ? toSubscriptionDriverContract(raw.subscription_contract)
      : undefined,
    detection: {
      creation: raw.creation.trigger as DetectionPolicy["creation"],
      basicVerification: raw.creation.basic_verification as DetectionPolicy["basicVerification"],
      advancedConversational: raw.creation.advanced_conversational as DetectionPolicy["advancedConversational"],
      periodic: {
        catalog: raw.periodic.catalog,
        quota: raw.periodic.quota,
        metadata: raw.periodic.metadata,
        conversationalProbe: false,
      },
    },
  };
}

function toSubscriptionDriverContract(raw: RawSubscriptionDriverContract): SubscriptionDriverContract {
  return {
    version: raw.version,
    acceptedIngressProtocols: raw.accepted_ingress_protocols as SourceDriverProtocol[],
    generation: toSubscriptionOperationCapability(raw.generation),
    specialOperations: raw.special_operations.map((special) => ({
      surface: special.surface as SourceDriverSurface,
      operation: special.operation,
      capability: toSubscriptionOperationCapability(special.capability),
    })),
  };
}

function toSubscriptionOperationCapability(
  raw: RawSubscriptionOperationCapability,
): SubscriptionOperationCapability {
  return {
    modes: raw.modes as SubscriptionResponseMode[],
    conversion: raw.conversion as SubscriptionConversionContract,
    usageConfidence: raw.usage_confidence as SubscriptionUsageConfidence,
    evidence: raw.evidence as SubscriptionEvidenceLevel,
    defaultEnabled: raw.default_enabled,
  };
}

function expectedExecutionKind(id: SourceDriverId): SourceDriverExecutionKind {
  if (id === "openai_subscription") return "open_ai_subscription";
  if (id === "claude_subscription") return "claude_subscription";
  if (id === "gemini_subscription") return "gemini_subscription";
  if (id === "grok_subscription") return "grok_subscription";
  return "http_surface";
}

function validExecutor(driver: RawDriver, executionKind: SourceDriverExecutionKind): boolean {
  if (executionKind === "http_surface") {
    return driver.executor?.type === "http_surface"
      && driver.executor.provider === undefined
      && driver.subscription_provider === undefined;
  }
  const provider = executionKind === "open_ai_subscription"
    ? "openai"
    : executionKind === "claude_subscription"
      ? "claude"
      : executionKind === "gemini_subscription" ? "antigravity" : "grok";
  return driver.executor?.type === "retained_subscription"
    && driver.executor.provider === provider
    && driver.subscription_provider === provider;
}

function surfaceForProtocol(protocol: SourceDriverProtocol): SourceDriverSurface {
  if (protocol === "anthropic_messages") return "anthropic";
  if (protocol === "gemini_native") return "gemini";
  return "open_ai";
}

function isStrictHttpBaseUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return (url.protocol === "http:" || url.protocol === "https:")
      && Boolean(url.hostname)
      && !url.username
      && !url.password
      && !url.search
      && !url.hash;
  } catch {
    return false;
  }
}

function isStrictHttpsUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return url.protocol === "https:"
      && Boolean(url.hostname)
      && !url.username
      && !url.password
      && !url.search
      && !url.hash;
  } catch {
    return false;
  }
}

function cloneInitialSurface(surface: SourceDriverInitialSurface): SourceDriverInitialSurface {
  return {
    ...surface,
    protocols: surface.protocols.map((protocol) => ({
      ...protocol,
      verification: { ...protocol.verification },
    })),
    operation_overrides: surface.operation_overrides.map((override) => ({ ...override })),
    verification: { ...surface.verification },
  };
}

function declaredVerification(): SourceDriverInitialVerification {
  return { state: "declared", checked_at_unix: 0, summary: "" };
}

function deepFreeze<T>(value: T): T {
  if (value && typeof value === "object" && !Object.isFrozen(value)) {
    Object.freeze(value);
    for (const property of Object.values(value as Record<string, unknown>)) deepFreeze(property);
  }
  return value;
}
