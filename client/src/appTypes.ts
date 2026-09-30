import type {
  AvailabilityGroupResult,
  ChannelConfigV5,
  ChannelDetectionResultV2,
  ChannelProtocolBindingV5,
  ChannelSurfaceBindingV5,
  ChannelVerificationV5,
  SourceDriverId,
  SourceDriverSurface,
} from "./sourceDrivers";

export type Endpoint = {
  server_id?: string;
  name: string;
  base_url: string;
  supplier_ws_url?: string;
  supplier_quic_url?: string;
  enabled: boolean;
};

export type LanShareConfig = {
  shared_models: string[];
  preferred_connect_ip: string;
};

export type LanShareMember = {
  id: string;
  name: string;
  api_key: string;
  enabled: boolean;
  weekly_token_limit: number;
};

export type LanShareUsageSnapshot = {
  week_started_at_unix: number;
  resets_at_unix: number;
  input_tokens: number;
  output_tokens: number;
  used_tokens: number;
  requests: number;
};

export type LanShareMemberStatus = LanShareMember & LanShareUsageSnapshot & {
  remaining_tokens: number;
  exhausted: boolean;
};

export type LanShareConnectAddress = {
  interface_name: string;
  ip: string;
  url: string;
  is_primary: boolean;
};

export type LanShareHostStatus = {
  allow_lan_access: boolean;
  listen: string;
  connect_url: string;
  connect_addresses: LanShareConnectAddress[];
  shared_models: string[];
  available_models: string[];
  input_weight: number;
  output_weight: number;
  members: LanShareMemberStatus[];
};

export type LanShareSelfStatus = LanShareUsageSnapshot & {
  name: string;
  models: string[];
  enabled: boolean;
  weekly_token_limit: number;
  remaining_tokens: number;
  exhausted: boolean;
  input_weight: number;
  output_weight: number;
};

export type CustomBrandIcon = "anythingllm" | "azure" | "bedrock" | "claude" | "claude-science" | "cline" | "codex" | "copilot" | "deepseek-harness" | "gemini" | "goose" | "grok" | "hermes" | "kimi" | "mimocode" | "minimax-code" | "mistral-vibe" | "omniroute" | "open-design" | "open-interpreter" | "openclaw" | "opencode" | "openscience" | "pi" | "qwen" | "raven" | "reasonix" | "trae" | "trae-work" | "vibe-trading" | "vscode" | "workbuddy" | "zcode";

export type CopyField =
  | "base-url"
  | "api-key"
  | "supplier-credential-reference"
  | "tool-config-path"
  | "guide-root-url"
  | "guide-openai-url"
  | "guide-openai-chat-url"
  | "guide-openai-models-url"
  | "guide-anthropic-url"
  | "guide-gemini-url"
  | "guide-model-id"
  | "guide-models";

export type ClientConfig = {
  config_version: number;
  client_id?: string;
  platform_id: string;
  account_platform_id: string;
  account_home_server_id?: string;
  account_home_base_url?: string;
  account_user_id: string;
  account_email: string;
  listen: string;
  allow_lan_access: boolean;
  lan_share: LanShareConfig;
  proxy_auto_start: boolean;
  automatic_updates: boolean;
  supplier_auto_start: boolean;
  api_key: string;
  registry_sources: string[];
  registry_signature_secret: string;
  registry_public_keys: string[];
  registry_version: number;
  development_endpoint: string;
  allow_model_equivalence: boolean;
  prefer_local_supply: boolean;
  /** Legacy persisted field; runtime routing now always permits custom fallback. */
  allow_unverified_platform_routes: boolean;
  model_aliases: Record<string, string>;
  endpoints: Endpoint[];
  channels: ChannelConfig[];
  supplier: SupplierConfig;
};

export type ClientConfigV5Wire = Omit<ClientConfig, "channels" | "supplier"> & {
  channels: ChannelConfigV5[];
};

export type SupplierConfig = {
  source_driver?: SourceDriverId;
  user_agent_profile?: string;
  enabled: boolean;
  kind: string;
  api_format: string;
  node_id: string;
  name: string;
  server_ws_url: string;
  server_quic_url: string;
  upstream_base_url: string;
  upstream_api_key: string;
  public_model: string;
  upstream_model: string;
  models: string[];
  supported_protocols: string[];
  capability_profiles?: ChannelCapabilityProfile[];
  detection_checks?: ChannelDetectionCheck[];
  surface_bindings?: ChannelSurfaceBinding[];
  price_ratio: number;
  subscription?: SubscriptionAdapterConfig;
};

export type ChannelConfig = Omit<ChannelConfigV5, "subscription"> & {
  enabled: boolean;
  share_enabled: boolean;
  kind: string;
  api_format: string;
  node_id: string;
  name: string;
  server_ws_url: string;
  server_quic_url: string;
  upstream_base_url: string;
  upstream_api_key: string;
  public_model: string;
  upstream_model: string;
  models: string[];
  supported_protocols: string[];
  capability_profiles?: ChannelCapabilityProfile[];
  detection_checks?: ChannelDetectionCheck[];
  surface_bindings: ChannelSurfaceBinding[];
  price_ratio: number;
  subscription: SubscriptionAdapterConfig;
};

export type SubscriptionAdapterConfig = {
  platform: string;
  account_label: string;
  credential_ref: string;
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

export type SubscriptionProvider = "openai" | "claude" | "antigravity" | "grok";

export type SubscriptionOAuthSession = {
  provider: string;
  auth_url: string;
  state: string;
  code_verifier: string;
  redirect_uri: string;
  callback_hint: string;
  auto_callback: boolean;
};

export type ProxyStatus = {
  running: boolean;
  listen: string;
  active_endpoint?: string | null;
  platform_connection_state?: string;
  platform_transport?: string;
  platform_transport_error?: string;
  platform_server_version?: string;
};

export type SupplierStatus = {
  running: boolean;
  starting?: boolean;
  node_id: string;
  server_ws_url: string;
  server_quic_url: string;
  transport_preference: string;
  connected_transport: string;
  active_transport: string;
  last_transport_error?: string;
  nodes?: string[];
  channels?: SupplierChannelHealth[];
  last_error?: string;
  route_statuses?: SupplierNodeRouteStatus[];
  channel_transports?: SupplierChannelTransportStatus[];
  availability_groups?: Record<string, AvailabilityGroupResult[]>;
};

export type SupplierChannelTransportStatus = {
  channel_id: string;
  supplier_unit_id: string;
  connected_transport: string;
  active_transport: string;
  last_transport_error: string;
  last_register_ack_unix?: number;
  platform_registered?: boolean;
};

export type SupplierNodeRouteStatus = {
  node_id?: string;
  channel_id?: string;
  supplier_unit_id?: string;
  state?: string;
  reason?: string;
  message?: string;
  accepted_models?: string[];
  unsupported_models?: string[];
  fallback_priced_models?: PricingFallbackInfo[];
  catalog_release_id?: string;
  cooldown_until_unix?: number;
  updated_at_unix?: number;
};

export type ApiSurface = SourceDriverSurface;

export type ProtocolVerification = ChannelVerificationV5;

export type ChannelProtocolBinding = ChannelProtocolBindingV5;

export type OperationEndpointOverride = {
  operation: string;
  method: string;
  url: string;
};

export type SurfaceVerification = ProtocolVerification;

export type ChannelSurfaceBinding = ChannelSurfaceBindingV5;

export type PricingFallbackInfo = {
  model?: string;
  provider?: string;
  billing_model?: string;
  pricing_rule_id?: string;
  confidence?: string;
  price_version_id?: string;
};

export type QuotaWindow = {
  source?: string;
  window?: string;
  remaining_ratio?: number;
  used_percent?: number;
  reset_at_unix?: number;
  checked_at_unix?: number;
  model?: string;
  token_type?: string;
};

export type SupplierChannelHealth = {
  channel_id: string;
  name: string;
  status: string;
  model: string;
  model_count: number;
  latency_ms: number;
  upstream_http_version?: string;
  credential_status: string;
  quota_status: string;
  remaining_ratio?: number;
  daily_used?: number;
  daily_limit?: number;
  quota_checked_at_unix?: number;
  quota_windows?: QuotaWindow[];
  safety_state?: string;
  last_error_kind?: string;
  cooldown_until_unix?: number;
  success_ewma?: number;
  latency_ewma_ms?: number;
  current_concurrency?: number;
  max_concurrency?: number;
  rpm_limit?: number;
  rpm_remaining?: number;
  message: string;
  checked_at_unix: number;
};

export type SupplierChannelValidationResult = {
  channel: ChannelConfigV5;
  health: SupplierChannelHealth;
};

export type PlatformProvider = {
  id: string;
  name: string;
  model: string;
  public_model: string;
  enabled: boolean;
};

export type PlatformSupplierNode = {
  node_id: string;
  name: string;
  public_model: string;
  upstream_model: string;
  models?: string[];
  supported_protocols?: string[];
  supported_operations?: string[];
  health_status?: string;
  quota_status?: string;
};

export type ToolApplyResult = {
  tool: string;
  files: string[];
  backups: string[];
  file_statuses?: ToolFileStatus[];
  already_configured?: boolean;
  details?: Record<string, string>;
};

export type ToolConfigOperationResponse = {
  status: "confirmation_required" | "manual_close_required" | "completed" | "committed_not_started";
  confirmation_required: boolean;
  manual_close_required: boolean;
  running: boolean;
  confirmation_token?: string | null;
  message?: string | null;
  result?: ToolApplyResult | null;
};

export type ToolConfigOperationTicket = {
  operation_id: string;
};

export type ToolRemoveMode = "restore_pre_const" | "native_route";

export type ToolRemoveDecision = ToolRemoveMode | "cancel";

export type ToolRemoveDialogState = {
  tool: string;
  toolTitle: string;
  nativeOption?: {
    title: string;
    description: string;
  };
};

export type ToolConfigOperationStatus = {
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

export type ToolConfigProgressControl = {
  waitForUpdate: (
    operationId: string,
    afterVersion: number,
    waitMs: number,
  ) => Promise<ToolConfigOperationStatus>;
};

export type ToolOperationProgress = {
  stage: string;
  completed?: number | null;
  total?: number | null;
  stageRatio?: number | null;
  ratio?: number | null;
};

export type ToolProgramCandidate = {
  path: string;
  label: string;
  kind: string;
  edition?: "desktop" | "cli";
  exists: boolean;
  modified_at_unix?: number;
  version?: string;
  selected?: boolean;
  launchable?: boolean;
  recommended?: boolean;
  status?: string;
  reason?: string;
};

export type ToolProgramLocationResult = {
  tool: string;
  selected_path: string;
  candidates: ToolProgramCandidate[];
};

export type ToolFileStatus = {
  path: string;
  exists_before: boolean;
  changed: boolean;
  already_configured: boolean;
  before_sha256: string;
  after_sha256: string;
};

export type CodexSessionScanResult = {
  provider_counts: Record<string, number>;
  files: { path: string; provider: string }[];
};

export type DebugTargetType = "local_channel" | "platform_auto" | "platform_node";

export type DebugProtocol = "openai_responses" | "openai_chat" | "anthropic_messages" | "gemini_native";

export type DebugTargetOption = {
  id: string;
  name: string;
  subtitle: string;
  models: string[];
  defaultModel: string;
  defaultProtocol: DebugProtocol;
  supportedProtocols: DebugProtocol[];
  status: string;
};

export type RouteCandidate = {
  node_id: string;
  channel_id?: string;
  supplier_unit_id?: string;
  public_model?: string;
  cache_domain_id?: string;
  supported_protocols?: string[];
  supported_operations?: string[];
  model_match: boolean;
  protocol_match: boolean;
  selectable: boolean;
  filter_reason?: string;
  score: number;
  score_breakdown?: Record<string, number>;
  health_status?: string;
  quota_status?: string;
  quota_source?: string;
  quota_window?: string;
  quota_used_percent?: number;
  quota_reset_at_unix?: number;
  quota_windows?: QuotaWindow[];
  safety_state?: string;
  rpm_remaining?: number;
  rpm_limit?: number;
  cooldown_until_unix?: number;
  current_concurrency?: number;
  reported_current_concurrency?: number;
  max_concurrency?: number;
  learned_concurrency?: number;
  effective_concurrency?: number;
};

export type RouteNodeSnapshot = {
  node_id: string;
  channel_id?: string;
  supplier_unit_id?: string;
  name?: string;
  public_model?: string;
  upstream_model?: string;
  cache_domain_id?: string;
  models?: string[];
  supported_protocols?: string[];
  supported_operations?: string[];
  health_status?: string;
  quota_status?: string;
  quota_source?: string;
  quota_window?: string;
  quota_used_percent?: number;
  quota_reset_at_unix?: number;
  quota_windows?: QuotaWindow[];
  safety_state?: string;
  current_concurrency?: number;
  reported_current_concurrency?: number;
  max_concurrency?: number;
  learned_concurrency?: number;
  effective_concurrency?: number;
};

export type RouteDecision = {
  model?: string;
  requested_protocols?: string[];
  required_operation?: string;
  selected: boolean;
  selected_node?: RouteNodeSnapshot | null;
  selected_protocol?: string;
  selected_upstream_model?: string;
  max_price_ratio?: number;
  best_price_ratio?: number;
  selected_price_ratio?: number;
  price_selection_reason?: string;
  conversion_level?: string;
  selection_reason?: string;
  cache_route_reason?: string;
  cache_domain_id?: string;
  sticky_hit: boolean;
  sticky_node_id?: string;
  sticky_miss_reason?: string;
  sticky_idle_seconds?: number;
  sticky_weight?: number;
  challenger_weight?: number;
  rebalance_threshold?: number;
  candidates: RouteCandidate[];
};

export type RoutePlanState = {
  status: "idle" | "loading" | "success" | "error" | "unavailable";
  decision?: RouteDecision;
  error?: string;
  requestedModel?: string;
  requestedProtocol?: DebugProtocol;
  requestedPath?: string;
  refreshedAt?: number;
};

export type TestResult = {
  status: "idle" | "running" | "success" | "error";
  httpStatus?: number;
  latencyMs?: number;
  upstreamHttpVersion?: string;
  model?: string;
  protocol?: string;
  path?: string;
  route?: string;
  skipLocal?: boolean;
  inputTokens?: number;
  outputTokens?: number;
  content?: string;
  raw?: string;
  error?: string;
};

export type LocalProxyTestResult = {
  http_status: number;
  latency_ms: number;
  model: string;
  protocol: string;
  path: string;
  route: string;
  skip_local: boolean;
  content: string;
  raw: string;
};

export type ProtocolDebugResult = {
  target_type: string;
  target_id: string;
  inbound_protocol: string;
  target_protocol: string;
  requested_model: string;
  upstream_model: string;
  conversion_level: string;
  path: string;
  request_headers: Record<string, string>;
  request_body: unknown;
  unsupported_fields: string[];
  lossy_warnings: string[];
  executed: boolean;
  http_status?: number;
  latency_ms?: number;
  content_type: string;
  content: string;
  raw: string;
  metrics: ProtocolDebugMetrics;
  error_layer: string;
};

export type ProtocolDebugMetrics = {
  request_body_bytes: number;
  response_raw_bytes: number;
  response_content_bytes: number;
  response_content_chars: number;
  response_raw_lines: number;
  response_sse_events: number;
  response_sse_done: boolean;
  response_sse_last_event_type: string;
  response_kind: string;
  tool_call_count: number;
  finish_reason: string;
};

export type ChannelUpstreamTestResult = LocalProxyTestResult & {
  upstream_http_version?: string;
  input_tokens: number;
  output_tokens: number;
};

export type ChannelCapabilityProfile = {
  protocol: string;
  model_pattern?: string;
  capability_layer?: "model_wire" | "driver" | "client_host" | "product_service" | string;
  input_modalities_authoritative?: boolean;
  output_modalities_authoritative?: boolean;
  release_status?: "prepared" | "experimental" | "supported" | "suspended" | string;
  non_stream_json?: boolean;
  stream_sse?: boolean;
  stream_sse_unsupported?: boolean;
  tool_calls?: boolean;
  tool_choice?: boolean;
  parallel_tool_calls?: boolean;
  json_schema?: boolean;
  reasoning?: boolean;
  thinking?: boolean;
  vision?: boolean;
  image_input?: boolean;
  image_output?: boolean;
  audio_input?: boolean;
  audio_output?: boolean;
  video_input?: boolean;
  video_output?: boolean;
  file_input?: boolean;
  file_output?: boolean;
  cache_control?: boolean;
  hosted_tools?: string[];
  custom_tool?: boolean;
  verification_state?: string;
  verified_at_unix?: number;
};

export type ChannelDetectionCheck = {
  name: string;
  status: string;
  checked_at_unix?: number;
  protocol?: string;
  capability?: string;
  message?: string;
};

export type ChannelDetectionResult = ChannelDetectionResultV2;

export type EndpointDiscoveryResult = {
  source: string;
  version: number;
  platform_id: string;
  endpoints: Endpoint[];
  refresh_warning?: string;
};

export type Toast = {
  text: string;
  tone: "info" | "success" | "error";
};

export type ConfirmDialogState = {
  title: string;
  message: string;
  confirmText?: string;
  cancelText?: string;
  tone?: "default" | "danger";
  mode?: "confirm" | "notice";
};

export type ChannelDuplicateCandidate = {
  channel_id: string;
  name: string;
};

export type ChannelDuplicateDecision =
  | { action: "open_existing"; channelId: string }
  | { action: "create_new" }
  | { action: "cancel" };

export type ChannelDuplicateDialogState = {
  draftName: string;
  candidates: ChannelDuplicateCandidate[];
};

export type AppLogEntry = {
  id: number;
  time: string;
  text: string;
  tone: Toast["tone"];
};

export type UpdateState = {
  status: "idle" | "checking" | "downloading" | "ready" | "ready_waiting_idle" | "installing" | "error";
  version?: string;
  error?: string;
  waitingReason?: string;
  checkedAt?: string;
  showDiagnostic?: boolean;
};

export type UpdateInstallReadiness = {
  idle: boolean;
  gate_acquired: boolean;
  draining: boolean;
  active_logical_requests: number;
  active_proxy_requests: number;
  active_supplier_requests: number;
  pending_supplier_outbound_messages: number;
  unacknowledged_supplier_replay_messages: number;
  reason: string;
};

export type NativeUpdateSourceStatus = {
  sourceUrl: string;
  status: string;
  latencyMs?: number | null;
  version?: string | null;
  error?: string | null;
};

export type NativeUpdateStatus = {
  status: UpdateState["status"];
  version?: string | null;
  error?: string | null;
  checkedAtUnixMs: number;
  sourceUrl?: string | null;
  sources: NativeUpdateSourceStatus[];
  preferredSourceUrl?: string | null;
  automatic: boolean;
  readiness: UpdateInstallReadiness;
};

export type ModelCatalogVersionInfo = {
  release_id: string;
  model_version_id: string;
  compatibility_version_id: string;
  sequence?: number | null;
  source: "packaged" | "public" | "server" | string;
  delivery_source?: string;
  delivery_sources?: string[];
};

export type AppMetaState = {
  currentVersion: string;
  updateSources: string[];
  updateSource?: string;
  endpointSources: string[];
  updateLastCheckedAt?: string;
  updateLastError?: string;
  endpointStatus: "idle" | "checking" | "success" | "error";
  endpointSource?: string;
  endpointVersion?: number;
  endpointCount?: number;
  endpointLastCheckedAt?: string;
  endpointLastError?: string;
  endpointRefreshWarning?: string;
  modelCatalog?: ModelCatalogVersionInfo;
  modelCatalogLastError?: string;
};

export type ReleaseSourceStatus = {
  sequence: number;
  deliverySource: string;
  bootstrapSources: string[];
  sourceIds: string[];
  endpointSources: string[];
  catalogSources: string[];
  catalogComponentSources: string[];
  updateSources: string[];
  serverUpdateSources: string[];
};

export type BrandIconData = {
  title: string;
  path: string;
  hex: string;
};

export type ModelAliasPreviewRow = {
  alias: string;
  candidates: string[];
};

export type ModelCompatibilityGroup = {
  id: string;
  label: string;
  description?: string;
  aliases?: string[];
  models: string[];
  match_models?: string[];
  disabled?: boolean;
};

export type ModelCompatibilityCatalogModel = {
  id: string;
  display_name?: string;
  vendor?: string;
  family?: string;
  context_tokens?: number;
  output_tokens?: number;
  input_modalities?: string[];
  output_modalities?: string[];
  reasoning?: boolean;
  tool_call?: boolean;
};

export type ModelCompatibilityPolicyState = {
  status: "idle" | "loading" | "ready" | "error";
  aliases: Record<string, string>;
  modelGroups: ModelCompatibilityGroup[];
  templateModelGroups: ModelCompatibilityGroup[];
  catalogModels: ModelCompatibilityCatalogModel[];
  platformAvailableModels: string[];
  localAvailableModels: string[];
  source?: string;
  customized?: boolean;
  platformSyncAvailable?: boolean;
  baseReleaseId?: string;
  revision?: number;
  templateChanged?: boolean;
  syncStatus?: "synced" | "pending" | "conflict" | "local";
  syncError?: string;
  error?: string;
};
