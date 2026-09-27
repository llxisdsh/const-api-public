import { tr } from "./i18n";

type EndpointConnection = {
  server_ws_url: string;
  server_quic_url: string;
};

type EndpointDiscoveryMerge<TEndpoint> = {
  platform_id: string;
  version: number;
  endpoints: TEndpoint[];
  serverWsUrl: string;
  serverQuicUrl: string;
};

type DetectionVerification = {
  state?: string;
  checked_at_unix?: number;
  summary?: string;
};

type DetectionSurface = {
  surface: string;
  base_url: string;
  endpoint_profile: string;
  auth_scheme: string;
  verification?: DetectionVerification;
  protocols: Array<{
    protocol: string;
    preferred: boolean;
    verification?: DetectionVerification;
  }>;
  operation_overrides: Array<{
    operation: string;
    method: string;
    url: string;
  }>;
};

type DetectionInput = {
  source_driver: string;
  credential_ref: string;
  user_agent_profile?: string;
  executor: unknown;
  default_target: unknown;
  discovery: unknown;
  surfaces: DetectionSurface[];
};

export function reconcileDetectedDefaultModel(
  configuredDefault: string,
  detectedModels: string[],
  detectedDefault = "",
) {
  const seen = new Set<string>();
  const models = detectedModels.flatMap((model) => {
    const value = model.trim();
    const key = value.toLowerCase();
    if (!key || seen.has(key)) return [];
    seen.add(key);
    return [value];
  });
  const findObserved = (candidate: string) => {
    const key = candidate.trim().toLowerCase();
    return key ? models.find((model) => model.toLowerCase() === key) ?? "" : "";
  };
  return {
    models,
    defaultModel: findObserved(configuredDefault)
      || findObserved(detectedDefault)
      || models[0]
      || "",
  };
}

export function resolveDebugModelSelection(
  selectedModel: string,
  configuredDefault: string,
  models: string[],
) {
  return selectedModel.trim()
    || reconcileDetectedDefaultModel(configuredDefault, models).defaultModel;
}

function detectionInput(channel: DetectionInput) {
  return {
    source_driver: channel.source_driver,
    credential_ref: channel.credential_ref,
    user_agent_profile: (channel.user_agent_profile ?? "").trim().toLowerCase(),
    executor: channel.executor,
    default_target: channel.default_target,
    discovery: channel.discovery,
    surfaces: channel.surfaces.map((surface) => ({
      surface: surface.surface,
      base_url: surface.base_url,
      endpoint_profile: surface.endpoint_profile,
      auth_scheme: surface.auth_scheme,
      protocols: surface.protocols.map((binding) => ({
        protocol: binding.protocol,
        preferred: binding.preferred,
      })),
      operation_overrides: surface.operation_overrides.map((override) => ({
        operation: override.operation,
        method: override.method,
        url: override.url,
      })),
    })),
  };
}

export function channelDetectionInputChanged(
  saved: DetectionInput | undefined,
  current: DetectionInput,
) {
  if (!saved) return true;
  return JSON.stringify(detectionInput(saved)) !== JSON.stringify(detectionInput(current));
}

export function channelRuntimeNeedsActivation(
  saved: (DetectionInput & EndpointConnection & { enabled: boolean; node_id: string }) | undefined,
  current: DetectionInput & EndpointConnection & { node_id: string },
) {
  return !saved || !saved.enabled
    || saved.node_id !== current.node_id
    || saved.server_ws_url !== current.server_ws_url
    || saved.server_quic_url !== current.server_quic_url
    || channelDetectionInputChanged(saved, current);
}

type EditorSettings = DetectionInput & {
  id: string;
  name: string;
  enabled: boolean;
  share_enabled: boolean;
  default_model: string;
  price_ratio: number;
  node_id: string;
  server_ws_url: string;
  server_quic_url: string;
  max_concurrency?: number;
  quota_reserve_percent?: number;
  subscription?: unknown;
};

function editorSettings(channel: EditorSettings) {
  return {
    id: channel.id,
    name: channel.name,
    enabled: channel.enabled,
    share_enabled: channel.enabled,
    default_model: channel.default_model,
    price_ratio: channel.price_ratio,
    node_id: channel.node_id,
    server_ws_url: channel.server_ws_url,
    server_quic_url: channel.server_quic_url,
    max_concurrency: channel.max_concurrency,
    quota_reserve_percent: channel.quota_reserve_percent,
    subscription: channel.subscription,
    ...detectionInput(channel),
  };
}

export function channelEditorSettingsChanged(
  saved: EditorSettings | undefined,
  current: EditorSettings,
) {
  if (!saved) return true;
  return JSON.stringify(editorSettings(saved)) !== JSON.stringify(editorSettings(current));
}

export function diagnosticToastEventKey(issue: {
  id: string;
  title?: string;
  message: string;
  createdAt: number;
}) {
  return `${issue.id}:${issue.createdAt}:${issue.message}`;
}

export function mergeEndpointDiscoveryConfig<
  TEndpoint,
  TChannel extends EndpointConnection,
  TSupplier extends EndpointConnection,
  TConfig extends {
    platform_id: string;
    registry_version: number;
    endpoints: TEndpoint[];
    channels: TChannel[];
    supplier: TSupplier;
  },
>(current: TConfig, discovery: EndpointDiscoveryMerge<TEndpoint>): TConfig {
  return {
    ...current,
    platform_id: discovery.platform_id,
    registry_version: discovery.version,
    endpoints: discovery.endpoints,
    supplier: {
      ...current.supplier,
      server_ws_url: discovery.serverWsUrl,
      server_quic_url: discovery.serverQuicUrl,
    },
    channels: current.channels.map((channel) => ({
      ...channel,
      server_ws_url: discovery.serverWsUrl,
      server_quic_url: discovery.serverQuicUrl,
    })),
  };
}

export function cancelSupplierEditor<
  TChannel extends { id: string },
  TConfig extends { channels: TChannel[] },
>(selectedChannelId: string, savedConfig: TConfig) {
  const selectedChannelWasSaved = savedConfig.channels.some((channel) => channel.id === selectedChannelId);
  return {
    config: savedConfig,
    closeDetail: !selectedChannelWasSaved,
    selectedChannelId: selectedChannelWasSaved
      ? selectedChannelId
      : savedConfig.channels[0]?.id ?? "",
  };
}

export function supplierDeleteConfirmation(channel: string) {
  return {
    title: tr("supplier.deleteConfirm.title", { channel }),
    message: tr("supplier.deleteConfirm.message"),
    confirmText: tr("supplier.deleteConfirm.confirm"),
    tone: "danger" as const,
  };
}

export function supplierDeletedNotice(channel: string, stoppedRuntime: boolean) {
  return tr(stoppedRuntime
    ? "supplier.messages.stoppedAndDeleted"
    : "supplier.messages.deleted", { channel });
}

export function finishSupplierDeletion<
  TChannel extends { id: string; models?: string[] },
>(remainingChannels: TChannel[]) {
  const nextChannel = remainingChannels[0];
  return {
    closeDetail: true,
    selectedChannelId: nextChannel?.id ?? "",
    models: nextChannel?.models ?? [],
  };
}

export type SupplierLifecyclePresentationInput = {
  saved: boolean;
  enabledForSupply: boolean;
  busy: boolean;
  checking?: boolean;
  detectionReady?: boolean;
  localStatus?: string;
  accountState: "signed_out" | "refreshing" | "signed_in" | "expired";
  runtimeRunning: boolean;
  connectedTransport?: string;
  activeTransport?: string;
  routeState?: string;
  routeLabel?: string;
};

export type SupplierLifecyclePresentation = {
  configLabel: string;
  localLabel: string;
  localTone: "good" | "pending" | "bad" | "muted";
  platformLabel: string;
  platformTone: "good" | "pending" | "bad" | "muted";
  actionLabel: string;
};

function transportEstablished(transport?: string) {
  return transport === "quic" || transport === "websocket";
}

export function supplierLifecyclePresentation(
  input: SupplierLifecyclePresentationInput,
): SupplierLifecyclePresentation {
  const localAvailable = input.localStatus === "available";
  const routeState = input.routeState?.trim().toLowerCase() ?? "";
  const routeBlocked = ["fallback", "offline", "removed", "deleted", "failed"].includes(routeState);
  const registered = transportEstablished(input.activeTransport);

  let localLabel = tr("labels.channel.detectionPending");
  let localTone: SupplierLifecyclePresentation["localTone"] = "muted";
  if (input.busy && (input.checking ?? true)) {
    localLabel = tr("labels.channel.detectionRunning");
    localTone = "pending";
  } else if (input.localStatus && !localAvailable) {
    localLabel = tr("labels.channel.detectionInvalid");
    localTone = "bad";
  } else if (input.detectionReady || localAvailable) {
    localLabel = tr("labels.channel.detectionPassed");
    localTone = "good";
  }

  let platformLabel = tr("labels.channel.supplyDisabled");
  let platformTone: SupplierLifecyclePresentation["platformTone"] = "muted";
  if (input.enabledForSupply) {
    if (registered && routeBlocked) {
      platformLabel = tr("labels.channel.supplyUnavailable", {
        status: input.routeLabel || tr("common.unavailable"),
      });
      platformTone = routeState === "fallback" ? "pending" : "bad";
    } else if (registered) {
      platformLabel = input.routeLabel
        ? tr("labels.channel.supplyOnlineStatus", { status: input.routeLabel })
        : tr("labels.channel.supplyOnline");
      platformTone = input.routeLabel ? "pending" : "good";
    } else if (!localAvailable) {
      platformLabel = tr("labels.channel.supplyWaitingDetection");
      platformTone = input.busy ? "pending" : "muted";
    } else if (input.accountState !== "signed_in") {
      platformLabel = tr("labels.channel.supplyWaitingSignIn");
      platformTone = "pending";
    } else if (transportEstablished(input.connectedTransport)) {
      platformLabel = tr("labels.channel.supplyRegistering");
      platformTone = "pending";
    } else if (input.runtimeRunning || input.busy) {
      platformLabel = tr("labels.channel.supplyConnecting");
      platformTone = "pending";
    } else {
      platformLabel = tr("labels.channel.supplyDisconnected");
      platformTone = "bad";
    }
  }

  return {
    configLabel: input.saved
      ? tr("labels.channel.configSaved")
      : tr("labels.channel.configUnsaved"),
    localLabel,
    localTone,
    platformLabel,
    platformTone,
    actionLabel: input.busy
      ? tr("supplier.actions.detecting")
      : registered && input.enabledForSupply
        ? tr("supplier.actions.detectAndRefresh")
        : input.enabledForSupply
          ? tr("supplier.actions.detectAndConnect")
          : tr("supplier.actions.detectAndEnable"),
  };
}
