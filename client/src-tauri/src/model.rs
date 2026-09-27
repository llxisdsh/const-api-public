use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex as StdMutex},
};
use tokio::sync::{Mutex, mpsc, oneshot};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct CodexClientIdentity {
    pub(crate) version: String,
    pub(crate) user_agent: String,
    pub(crate) originator: String,
    pub(crate) checked_at_unix: i64,
    pub(crate) source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Endpoint {
    #[serde(default)]
    pub(crate) server_id: String,
    pub(crate) name: String,
    pub(crate) base_url: String,
    #[serde(default)]
    pub(crate) supplier_ws_url: String,
    #[serde(default)]
    pub(crate) supplier_quic_url: String,
    pub(crate) enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub(crate) struct ModelCompatibilityGroupConfig {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) label: String,
    #[serde(default)]
    pub(crate) description: String,
    #[serde(default)]
    pub(crate) aliases: Vec<String>,
    #[serde(default)]
    pub(crate) models: Vec<String>,
    #[serde(default)]
    pub(crate) match_models: Vec<String>,
    #[serde(default)]
    pub(crate) disabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub(crate) struct ModelCompatibilityCatalogModel {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) display_name: String,
    #[serde(default)]
    pub(crate) vendor: String,
    #[serde(default)]
    pub(crate) family: String,
    #[serde(default)]
    pub(crate) context_tokens: u64,
    #[serde(default)]
    pub(crate) output_tokens: u64,
    #[serde(default)]
    pub(crate) input_modalities: Vec<String>,
    #[serde(default)]
    pub(crate) output_modalities: Vec<String>,
    #[serde(default)]
    pub(crate) reasoning: bool,
    #[serde(default)]
    pub(crate) tool_call: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub(crate) struct LocalModelCompatibilityProfile {
    pub(crate) platform_id: String,
    pub(crate) user_id: String,
    pub(crate) base_release_id: String,
    #[serde(default)]
    pub(crate) model_version_id: String,
    #[serde(default)]
    pub(crate) compatibility_version_id: String,
    #[serde(default)]
    pub(crate) revision: i64,
    #[serde(default)]
    pub(crate) model_groups: Vec<ModelCompatibilityGroupConfig>,
    #[serde(default)]
    pub(crate) template_model_groups: Vec<ModelCompatibilityGroupConfig>,
    #[serde(default)]
    pub(crate) catalog_models: Vec<ModelCompatibilityCatalogModel>,
    #[serde(default)]
    pub(crate) sync_status: String,
    #[serde(default)]
    pub(crate) customized: bool,
    #[serde(default)]
    pub(crate) pending_reset: bool,
    #[serde(default)]
    pub(crate) updated_at_unix_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub(crate) struct LanShareConfig {
    #[serde(default)]
    pub(crate) shared_models: Vec<String>,
    #[serde(default)]
    pub(crate) preferred_connect_ip: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub(crate) struct LanShareMember {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) api_key: String,
    #[serde(default = "crate::default_true")]
    pub(crate) enabled: bool,
    #[serde(default)]
    pub(crate) weekly_token_limit: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ClientConfig {
    #[serde(default)]
    pub(crate) account_home_server_id: String,
    #[serde(default)]
    pub(crate) account_home_base_url: String,
    #[serde(default)]
    pub(crate) config_version: u32,
    #[serde(default)]
    pub(crate) client_id: String,
    #[serde(default)]
    pub(crate) platform_id: String,
    #[serde(default)]
    pub(crate) account_platform_id: String,
    #[serde(default)]
    pub(crate) account_user_id: String,
    #[serde(default)]
    pub(crate) account_email: String,
    #[serde(default)]
    pub(crate) account_device_api_key: String,
    pub(crate) listen: String,
    #[serde(default)]
    pub(crate) allow_lan_access: bool,
    #[serde(default)]
    pub(crate) lan_share: LanShareConfig,
    #[serde(default = "crate::default_true")]
    pub(crate) proxy_auto_start: bool,
    #[serde(default = "crate::default_true")]
    pub(crate) automatic_updates: bool,
    #[serde(default)]
    pub(crate) supplier_auto_start: bool,
    pub(crate) api_key: String,
    #[serde(default)]
    pub(crate) registry_sources: Vec<String>,
    #[serde(default)]
    pub(crate) registry_signature_secret: String,
    #[serde(default)]
    pub(crate) registry_public_keys: Vec<String>,
    #[serde(default)]
    pub(crate) registry_version: i64,
    #[serde(default = "crate::default_development_endpoint")]
    pub(crate) development_endpoint: String,
    #[serde(default = "crate::default_allow_model_equivalence")]
    pub(crate) allow_model_equivalence: bool,
    #[serde(default)]
    pub(crate) prefer_local_supply: bool,
    // Legacy serialized field, normalized to true. Platform source priority
    // now determines when custom routes are used; it is no longer a preference.
    #[serde(default)]
    pub(crate) allow_unverified_platform_routes: bool,
    #[serde(default = "crate::default_model_aliases")]
    pub(crate) model_aliases: HashMap<String, String>,
    #[serde(default = "crate::default_model_compatibility_profiles")]
    pub(crate) model_compatibility_profiles: HashMap<String, LocalModelCompatibilityProfile>,
    pub(crate) endpoints: Vec<Endpoint>,
    #[serde(default)]
    pub(crate) channels: Vec<ChannelConfig>,
    #[serde(skip, default = "crate::default_supplier_config")]
    pub(crate) supplier: SupplierConfig,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ClientConfigV4Read {
    #[serde(default)]
    pub(crate) account_home_server_id: String,
    #[serde(default)]
    pub(crate) account_home_base_url: String,
    #[serde(default)]
    pub(crate) config_version: u32,
    #[serde(default)]
    pub(crate) client_id: String,
    #[serde(default)]
    pub(crate) platform_id: String,
    #[serde(default)]
    pub(crate) account_platform_id: String,
    #[serde(default)]
    pub(crate) account_user_id: String,
    #[serde(default)]
    pub(crate) account_email: String,
    #[serde(default)]
    pub(crate) account_device_api_key: String,
    pub(crate) listen: String,
    #[serde(default)]
    pub(crate) allow_lan_access: bool,
    #[serde(default)]
    pub(crate) lan_share: LanShareConfig,
    #[serde(default = "crate::default_true")]
    pub(crate) proxy_auto_start: bool,
    #[serde(default = "crate::default_true")]
    pub(crate) automatic_updates: bool,
    #[serde(default)]
    pub(crate) supplier_auto_start: bool,
    pub(crate) api_key: String,
    #[serde(default)]
    pub(crate) registry_sources: Vec<String>,
    #[serde(default)]
    pub(crate) registry_signature_secret: String,
    #[serde(default)]
    pub(crate) registry_public_keys: Vec<String>,
    #[serde(default)]
    pub(crate) registry_version: i64,
    #[serde(default = "crate::default_allow_model_equivalence")]
    pub(crate) allow_model_equivalence: bool,
    #[serde(default)]
    pub(crate) prefer_local_supply: bool,
    #[serde(default)]
    pub(crate) allow_unverified_platform_routes: bool,
    #[serde(default = "crate::default_model_aliases")]
    pub(crate) model_aliases: HashMap<String, String>,
    pub(crate) endpoints: Vec<Endpoint>,
    #[serde(default)]
    pub(crate) channels: Vec<ChannelConfigV4Read>,
    #[serde(default)]
    pub(crate) supplier: Option<SupplierConfigV4Read>,
}

#[derive(Debug, Clone)]
pub(crate) struct SupplierConfig {
    pub(crate) source_driver: crate::source_driver::SourceDriverId,
    pub(crate) credential_ref: String,
    pub(crate) executor: ChannelExecutorLocator,
    pub(crate) default_target: ChannelTarget,
    pub(crate) discovery: DiscoveryProfile,
    pub(crate) user_agent_profile: String,
    pub(crate) channel_id: String,
    pub(crate) enabled: bool,
    pub(crate) kind: String,
    pub(crate) api_format: String,
    pub(crate) node_id: String,
    pub(crate) name: String,
    pub(crate) server_ws_url: String,
    pub(crate) server_quic_url: String,
    pub(crate) upstream_base_url: String,
    pub(crate) upstream_api_key: String,
    pub(crate) public_model: String,
    pub(crate) upstream_model: String,
    pub(crate) models: Vec<String>,
    pub(crate) supported_protocols: Vec<String>,
    pub(crate) capability_profiles: Vec<ChannelCapabilityProfile>,
    pub(crate) detection_checks: Vec<ChannelDetectionCheck>,
    pub(crate) surface_bindings: Vec<ChannelSurfaceBinding>,
    pub(crate) price_ratio: f64,
    pub(crate) max_concurrency: u32,
    pub(crate) quota_reserve_percent: u32,
    pub(crate) subscription: SubscriptionAdapterConfig,
    pub(crate) registration_removed: bool,
    pub(crate) registration_health_status: Option<String>,
    pub(crate) registration_quota_status: Option<String>,
}

impl SupplierConfig {
    pub(crate) fn max_concurrency(&self) -> u32 {
        self.max_concurrency
    }

    pub(crate) fn quota_reserve_percent(&self) -> u32 {
        self.quota_reserve_percent.min(100)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SubscriptionAdapterConfig {
    #[serde(default = "crate::default_subscription_platform")]
    pub(crate) platform: String,
    #[serde(default)]
    pub(crate) account_label: String,
    #[serde(default)]
    pub(crate) credential_ref: String,
    // Compatibility mirrors for configs written before capacity became
    // channel-wide. Runtime decisions use the channel/supplier root fields.
    #[serde(default = "crate::default_subscription_max_concurrency")]
    pub(crate) max_concurrency: u32,
    #[serde(default = "crate::default_responses_ws_pool_enabled")]
    pub(crate) responses_ws_pool_enabled: bool,
    #[serde(default = "crate::default_responses_ws_subscription_multiplex_probe_enabled")]
    pub(crate) responses_ws_subscription_multiplex_probe_enabled: bool,
    #[serde(default = "crate::default_subscription_rpm_limit")]
    pub(crate) rpm_limit: u32,
    #[serde(default)]
    pub(crate) quota_reserve_percent: u32,
    #[serde(default = "crate::default_subscription_daily_request_limit")]
    pub(crate) daily_request_limit: u32,
    #[serde(default)]
    pub(crate) cooldown_until_unix: i64,
    #[serde(default)]
    pub(crate) risk_note: String,
    #[serde(default)]
    pub(crate) audit_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SubscriptionSafetyState {
    pub(crate) channel_id: String,
    pub(crate) provider: String,
    #[serde(default = "crate::default_subscription_safety_active_state")]
    pub(crate) state: String,
    #[serde(default)]
    pub(crate) last_error_kind: String,
    #[serde(default)]
    pub(crate) last_error_at_unix: i64,
    #[serde(default)]
    pub(crate) last_status: u16,
    #[serde(default)]
    pub(crate) cooldown_until_unix: i64,
    #[serde(default)]
    pub(crate) daily_used: u32,
    #[serde(default)]
    pub(crate) daily_limit: u32,
    #[serde(default)]
    pub(crate) rpm_limit: u32,
    #[serde(default)]
    pub(crate) current_concurrency: u32,
    #[serde(default)]
    pub(crate) max_concurrency: u32,
    #[serde(default)]
    pub(crate) remaining_ratio: f64,
    #[serde(default)]
    pub(crate) quota_source: String,
    #[serde(default)]
    pub(crate) quota_window: String,
    #[serde(default)]
    pub(crate) quota_used_percent: f64,
    #[serde(default)]
    pub(crate) quota_reset_at_unix: i64,
    #[serde(default)]
    pub(crate) quota_checked_at_unix: i64,
    #[serde(default)]
    pub(crate) quota_windows: Vec<QuotaWindow>,
    #[serde(default = "crate::default_subscription_safety_success_ewma")]
    pub(crate) success_ewma: f64,
    #[serde(default)]
    pub(crate) latency_ewma_ms: f64,
    #[serde(default)]
    pub(crate) consecutive_401: u32,
    #[serde(default)]
    pub(crate) consecutive_403: u32,
    #[serde(default)]
    pub(crate) consecutive_429: u32,
    #[serde(default)]
    pub(crate) consecutive_5xx: u32,
    #[serde(default)]
    pub(crate) estimated_input_tokens: u32,
    #[serde(default)]
    pub(crate) estimated_output_tokens: u32,
    #[serde(default)]
    pub(crate) rpm_window_unix: Vec<i64>,
}

#[derive(Debug, Clone)]
pub(crate) struct ChannelConfig {
    pub(crate) v2: ChannelConfigV2Contract,
    pub(crate) id: String,
    pub(crate) enabled: bool,
    pub(crate) share_enabled: bool,
    pub(crate) kind: String,
    pub(crate) api_format: String,
    pub(crate) node_id: String,
    pub(crate) name: String,
    pub(crate) server_ws_url: String,
    pub(crate) server_quic_url: String,
    pub(crate) upstream_base_url: String,
    pub(crate) upstream_api_key: String,
    pub(crate) public_model: String,
    pub(crate) upstream_model: String,
    pub(crate) models: Vec<String>,
    pub(crate) supported_protocols: Vec<String>,
    pub(crate) capability_profiles: Vec<ChannelCapabilityProfile>,
    pub(crate) detection_checks: Vec<ChannelDetectionCheck>,
    pub(crate) surface_bindings: Vec<ChannelSurfaceBinding>,
    pub(crate) price_ratio: f64,
    pub(crate) subscription: SubscriptionAdapterConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ChannelConfigV2Contract {
    #[serde(default)]
    pub(crate) created_at_unix_ms: i64,
    pub(crate) source_driver: crate::source_driver::SourceDriverId,
    pub(crate) credential_ref: String,
    pub(crate) executor: ChannelExecutorLocator,
    pub(crate) default_target: ChannelTarget,
    pub(crate) discovery: DiscoveryProfile,
    #[serde(default)]
    pub(crate) user_agent_profile: String,
    #[serde(default = "crate::default_channel_max_concurrency")]
    pub(crate) max_concurrency: u32,
    #[serde(default)]
    pub(crate) quota_reserve_percent: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ChannelExecutorLocator {
    HttpSurface,
    RetainedSubscription { provider: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ChannelTarget {
    pub(crate) surface: crate::surface::ApiSurface,
    pub(crate) protocol: crate::protocol::kind::ProtocolKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct DiscoveryProfile {
    pub(crate) strategy: String,
    pub(crate) catalog: bool,
    pub(crate) quota: bool,
    pub(crate) metadata: bool,
}

pub(crate) type ModelSnapshot = Vec<String>;

#[derive(Debug, Deserialize)]
pub(crate) struct SupplierConfigV4Read {
    #[serde(default)]
    pub(crate) source_driver: Option<crate::source_driver::SourceDriverId>,
    #[serde(default)]
    pub(crate) credential_ref: String,
    #[serde(default)]
    pub(crate) executor: Option<ChannelExecutorLocator>,
    #[serde(default)]
    pub(crate) default_target: Option<ChannelTarget>,
    #[serde(default)]
    pub(crate) discovery: Option<DiscoveryProfile>,
    #[serde(default)]
    pub(crate) user_agent_profile: String,
    #[serde(default)]
    pub(crate) channel_id: String,
    pub(crate) enabled: bool,
    #[serde(default = "crate::default_channel_kind")]
    pub(crate) kind: String,
    #[serde(default = "crate::default_channel_api_format")]
    pub(crate) api_format: String,
    pub(crate) node_id: String,
    pub(crate) name: String,
    pub(crate) server_ws_url: String,
    #[serde(default)]
    pub(crate) server_quic_url: String,
    pub(crate) upstream_base_url: String,
    pub(crate) upstream_api_key: String,
    pub(crate) public_model: String,
    pub(crate) upstream_model: String,
    #[serde(default)]
    pub(crate) models: Vec<String>,
    #[serde(default)]
    pub(crate) supported_protocols: Vec<String>,
    #[serde(default)]
    pub(crate) capability_profiles: Vec<ChannelCapabilityProfile>,
    #[serde(default)]
    pub(crate) detection_checks: Vec<ChannelDetectionCheck>,
    #[serde(default, alias = "surfaces")]
    pub(crate) surface_bindings: Vec<ChannelSurfaceBinding>,
    #[serde(default = "crate::default_price_ratio")]
    pub(crate) price_ratio: f64,
    #[serde(default)]
    pub(crate) max_concurrency: Option<u32>,
    #[serde(default)]
    pub(crate) quota_reserve_percent: Option<u32>,
    #[serde(default = "crate::default_subscription_adapter_config")]
    pub(crate) subscription: SubscriptionAdapterConfig,
}

#[derive(Serialize)]
struct SupplierConfigWire<'a> {
    source_driver: crate::source_driver::SourceDriverId,
    credential_ref: &'a str,
    executor: &'a ChannelExecutorLocator,
    default_target: &'a ChannelTarget,
    discovery: &'a DiscoveryProfile,
    #[serde(skip_serializing_if = "str::is_empty")]
    user_agent_profile: &'a str,
    channel_id: &'a str,
    enabled: bool,
    kind: &'a str,
    api_format: &'a str,
    node_id: &'a str,
    name: &'a str,
    server_ws_url: &'a str,
    server_quic_url: &'a str,
    upstream_base_url: &'a str,
    upstream_api_key: &'a str,
    public_model: &'a str,
    upstream_model: &'a str,
    models: &'a [String],
    supported_protocols: &'a [String],
    capability_profiles: &'a [ChannelCapabilityProfile],
    detection_checks: &'a [ChannelDetectionCheck],
    surface_bindings: &'a [ChannelSurfaceBinding],
    price_ratio: f64,
    max_concurrency: u32,
    quota_reserve_percent: u32,
    subscription: SubscriptionAdapterConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ChannelConfigV4Read {
    #[serde(default)]
    pub(crate) source_driver: Option<crate::source_driver::SourceDriverId>,
    #[serde(default)]
    pub(crate) credential_ref: String,
    #[serde(default)]
    pub(crate) default_target: Option<ChannelTarget>,
    #[serde(default)]
    pub(crate) discovery: Option<DiscoveryProfile>,
    #[serde(default)]
    pub(crate) user_agent_profile: String,
    pub(crate) id: String,
    pub(crate) enabled: bool,
    #[serde(default)]
    pub(crate) share_enabled: bool,
    #[serde(default = "crate::default_channel_kind")]
    pub(crate) kind: String,
    #[serde(default = "crate::default_channel_api_format")]
    pub(crate) api_format: String,
    pub(crate) node_id: String,
    pub(crate) name: String,
    pub(crate) server_ws_url: String,
    #[serde(default)]
    pub(crate) server_quic_url: String,
    pub(crate) upstream_base_url: String,
    pub(crate) upstream_api_key: String,
    pub(crate) public_model: String,
    pub(crate) upstream_model: String,
    #[serde(default)]
    pub(crate) models: Vec<String>,
    #[serde(default)]
    pub(crate) supported_protocols: Vec<String>,
    #[serde(default)]
    pub(crate) capability_profiles: Vec<ChannelCapabilityProfile>,
    #[serde(default)]
    pub(crate) detection_checks: Vec<ChannelDetectionCheck>,
    #[serde(default, alias = "surfaces")]
    pub(crate) surface_bindings: Vec<ChannelSurfaceBinding>,
    #[serde(default = "crate::default_price_ratio")]
    pub(crate) price_ratio: f64,
    #[serde(default)]
    pub(crate) max_concurrency: Option<u32>,
    #[serde(default)]
    pub(crate) quota_reserve_percent: Option<u32>,
    #[serde(default = "crate::default_subscription_adapter_config")]
    pub(crate) subscription: SubscriptionAdapterConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ChannelConfigV2 {
    pub(crate) id: String,
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) created_at_unix_ms: i64,
    pub(crate) source_driver: crate::source_driver::SourceDriverId,
    pub(crate) enabled: bool,
    pub(crate) share_enabled: bool,
    pub(crate) credential_ref: String,
    pub(crate) executor: ChannelExecutorLocator,
    pub(crate) surfaces: Vec<ChannelSurfaceBinding>,
    pub(crate) default_target: ChannelTarget,
    pub(crate) discovery: DiscoveryProfile,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) user_agent_profile: String,
    #[serde(default)]
    pub(crate) default_model: String,
    pub(crate) models: ModelSnapshot,
    pub(crate) price_ratio: f64,
    pub(crate) node_id: String,
    pub(crate) server_ws_url: String,
    pub(crate) server_quic_url: String,
    // Channel-wide supply limits. Optional on read so pre-v0.1.52 configs can
    // fall back to the former subscription-scoped compatibility fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) max_concurrency: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) quota_reserve_percent: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) subscription: Option<SubscriptionConfigV2>,
    #[serde(default, rename = "model_capability_evidence")]
    pub(crate) capability_profiles: Vec<ChannelCapabilityProfile>,
    #[serde(default, rename = "detection_evidence")]
    pub(crate) detection_checks: Vec<ChannelDetectionCheck>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SubscriptionConfigV2 {
    pub(crate) platform: String,
    pub(crate) account_label: String,
    pub(crate) max_concurrency: u32,
    #[serde(default = "crate::default_responses_ws_pool_enabled")]
    pub(crate) responses_ws_pool_enabled: bool,
    #[serde(default = "crate::default_responses_ws_subscription_multiplex_probe_enabled")]
    pub(crate) responses_ws_subscription_multiplex_probe_enabled: bool,
    pub(crate) rpm_limit: u32,
    #[serde(default)]
    pub(crate) quota_reserve_percent: u32,
    pub(crate) daily_request_limit: u32,
    pub(crate) cooldown_until_unix: i64,
    pub(crate) risk_note: String,
    pub(crate) audit_enabled: bool,
}

impl ChannelConfig {
    pub(crate) fn source_driver(&self) -> crate::source_driver::SourceDriverId {
        self.v2.source_driver
    }

    pub(crate) fn max_concurrency(&self) -> u32 {
        self.v2.max_concurrency
    }

    pub(crate) fn quota_reserve_percent(&self) -> u32 {
        self.v2.quota_reserve_percent.min(100)
    }

    pub(crate) fn default_model_from(&self, models: &[String]) -> String {
        configured_default_model(&self.upstream_model, &self.public_model, models)
    }

    #[cfg(test)]
    pub(crate) fn set_source_driver(
        &mut self,
        source_driver: crate::source_driver::SourceDriverId,
    ) {
        let created_at_unix_ms = self.v2.created_at_unix_ms;
        let credential_ref = std::mem::take(&mut self.v2.credential_ref);
        let max_concurrency = self.v2.max_concurrency;
        let quota_reserve_percent = self.v2.quota_reserve_percent;
        self.v2 = crate::channel_v2_contract_for_source(source_driver);
        self.v2.created_at_unix_ms = created_at_unix_ms;
        self.v2.credential_ref = credential_ref;
        self.v2.max_concurrency = max_concurrency;
        self.v2.quota_reserve_percent = quota_reserve_percent;
    }
}

impl<'a> From<&'a SupplierConfig> for SupplierConfigWire<'a> {
    fn from(value: &'a SupplierConfig) -> Self {
        let mut subscription = value.subscription.clone();
        subscription.max_concurrency = value.max_concurrency;
        subscription.quota_reserve_percent = value.quota_reserve_percent.min(100);
        Self {
            source_driver: value.source_driver,
            credential_ref: &value.credential_ref,
            executor: &value.executor,
            default_target: &value.default_target,
            discovery: &value.discovery,
            user_agent_profile: &value.user_agent_profile,
            channel_id: &value.channel_id,
            enabled: value.enabled,
            kind: &value.kind,
            api_format: &value.api_format,
            node_id: &value.node_id,
            name: &value.name,
            server_ws_url: &value.server_ws_url,
            server_quic_url: &value.server_quic_url,
            upstream_base_url: &value.upstream_base_url,
            upstream_api_key: &value.upstream_api_key,
            public_model: &value.public_model,
            upstream_model: &value.upstream_model,
            models: &value.models,
            supported_protocols: &value.supported_protocols,
            capability_profiles: &value.capability_profiles,
            detection_checks: &value.detection_checks,
            surface_bindings: &value.surface_bindings,
            price_ratio: value.price_ratio,
            max_concurrency: value.max_concurrency,
            quota_reserve_percent: value.quota_reserve_percent.min(100),
            subscription,
        }
    }
}

impl From<SupplierConfigV4Read> for SupplierConfig {
    fn from(value: SupplierConfigV4Read) -> Self {
        let source_driver = value.source_driver.unwrap_or_else(|| {
            crate::infer_source_driver_at_load_boundary(
                &value.kind,
                &value.name,
                &value.upstream_base_url,
                &value.subscription.platform,
            )
        });
        let driver = crate::source_driver::source_driver(source_driver).ok();
        let executor = value.executor.unwrap_or_else(|| {
            driver
                .map(|driver| driver.executor.clone())
                .unwrap_or(ChannelExecutorLocator::HttpSurface)
        });
        let credential_ref = [
            value.credential_ref.as_str(),
            value.upstream_api_key.as_str(),
            value.subscription.credential_ref.as_str(),
        ]
        .into_iter()
        .find(|candidate| !candidate.trim().is_empty())
        .unwrap_or_default()
        .to_string();
        let default_target = value.default_target.unwrap_or_else(|| {
            driver
                .map(|driver| driver.default_target.clone())
                .unwrap_or(ChannelTarget {
                    surface: crate::surface::ApiSurface::OpenAi,
                    protocol: crate::protocol::kind::ProtocolKind::OpenAiChat,
                })
        });
        let discovery = value.discovery.unwrap_or_else(|| {
            driver
                .map(|driver| driver.discovery.clone())
                .unwrap_or(DiscoveryProfile {
                    strategy: "custom".to_string(),
                    catalog: true,
                    quota: false,
                    metadata: true,
                })
        });
        let max_concurrency = value
            .max_concurrency
            .unwrap_or(value.subscription.max_concurrency);
        let quota_reserve_percent = value
            .quota_reserve_percent
            .unwrap_or(value.subscription.quota_reserve_percent)
            .min(100);
        let mut subscription = value.subscription;
        subscription.max_concurrency = max_concurrency;
        subscription.quota_reserve_percent = quota_reserve_percent;
        Self {
            source_driver,
            credential_ref,
            executor,
            default_target,
            discovery,
            user_agent_profile: value.user_agent_profile,
            channel_id: value.channel_id,
            enabled: value.enabled,
            kind: value.kind,
            api_format: value.api_format,
            node_id: value.node_id,
            name: value.name,
            server_ws_url: value.server_ws_url,
            server_quic_url: value.server_quic_url,
            upstream_base_url: value.upstream_base_url,
            upstream_api_key: value.upstream_api_key,
            public_model: value.public_model,
            upstream_model: value.upstream_model,
            models: value.models,
            supported_protocols: value.supported_protocols,
            capability_profiles: value.capability_profiles,
            detection_checks: value.detection_checks,
            surface_bindings: value.surface_bindings,
            price_ratio: value.price_ratio,
            max_concurrency,
            quota_reserve_percent,
            subscription,
            registration_removed: false,
            registration_health_status: None,
            registration_quota_status: None,
        }
    }
}

impl From<ChannelConfig> for ChannelConfigV2 {
    fn from(mut value: ChannelConfig) -> Self {
        let is_subscription = matches!(
            value.v2.source_driver,
            crate::source_driver::SourceDriverId::OpenAiSubscription
                | crate::source_driver::SourceDriverId::ClaudeSubscription
                | crate::source_driver::SourceDriverId::GeminiSubscription
                | crate::source_driver::SourceDriverId::GrokSubscription
        );
        value.models = crate::config::dedupe_models(value.models);
        let default_model =
            configured_default_model(&value.upstream_model, &value.public_model, &value.models);
        crate::config::sort_models_by_short_name(&mut value.models);
        let created_at_unix_ms = value.v2.created_at_unix_ms;
        Self {
            id: value.id,
            name: value.name,
            created_at_unix_ms,
            source_driver: value.v2.source_driver,
            enabled: value.enabled,
            share_enabled: value.share_enabled,
            credential_ref: value.v2.credential_ref,
            executor: value.v2.executor,
            surfaces: value.surface_bindings,
            default_target: value.v2.default_target,
            discovery: value.v2.discovery,
            user_agent_profile: value.v2.user_agent_profile,
            default_model,
            models: value.models,
            price_ratio: value.price_ratio,
            node_id: value.node_id,
            server_ws_url: value.server_ws_url,
            server_quic_url: value.server_quic_url,
            max_concurrency: Some(value.v2.max_concurrency),
            quota_reserve_percent: Some(value.v2.quota_reserve_percent.min(100)),
            capability_profiles: value.capability_profiles,
            detection_checks: value.detection_checks,
            subscription: is_subscription.then(|| {
                value.subscription.max_concurrency = value.v2.max_concurrency;
                value.subscription.quota_reserve_percent = value.v2.quota_reserve_percent.min(100);
                value.subscription.into()
            }),
        }
    }
}

impl From<ChannelConfigV2> for ChannelConfig {
    fn from(value: ChannelConfigV2) -> Self {
        let driver = crate::source_driver::source_driver(value.source_driver).ok();
        let kind = driver
            .map(|driver| driver.kind.clone())
            .unwrap_or_else(|| "custom_endpoint".to_string());
        let api_format = value.default_target.protocol.as_str().to_string();
        let upstream_base_url = value
            .surfaces
            .iter()
            .find(|binding| binding.surface == value.default_target.surface)
            .map(|binding| binding.base_url.clone())
            .unwrap_or_default();
        let supported_protocols = value
            .surfaces
            .iter()
            .flat_map(|binding| binding.protocols.iter())
            .map(|binding| binding.protocol.clone())
            .fold(Vec::new(), |mut values, protocol| {
                if !values.contains(&protocol) {
                    values.push(protocol);
                }
                values
            });
        let primary_model = observed_default_model(&value.default_model, &value.models);
        let max_concurrency = value
            .max_concurrency
            .or_else(|| value.subscription.as_ref().map(|item| item.max_concurrency))
            .unwrap_or_else(crate::default_subscription_max_concurrency);
        let quota_reserve_percent = value
            .quota_reserve_percent
            .or_else(|| {
                value
                    .subscription
                    .as_ref()
                    .map(|item| item.quota_reserve_percent)
            })
            .unwrap_or(0)
            .min(100);
        let mut subscription = value
            .subscription
            .map(Into::into)
            .unwrap_or_else(crate::default_subscription_adapter_config);
        subscription.credential_ref = value.credential_ref.clone();
        subscription.max_concurrency = max_concurrency;
        subscription.quota_reserve_percent = quota_reserve_percent;
        let v2 = ChannelConfigV2Contract {
            created_at_unix_ms: value.created_at_unix_ms,
            source_driver: value.source_driver,
            credential_ref: value.credential_ref.clone(),
            executor: value.executor,
            default_target: value.default_target,
            discovery: value.discovery,
            user_agent_profile: value.user_agent_profile,
            max_concurrency,
            quota_reserve_percent,
        };
        Self {
            v2,
            id: value.id,
            enabled: value.enabled,
            share_enabled: value.share_enabled,
            kind,
            api_format,
            node_id: value.node_id,
            name: value.name,
            server_ws_url: value.server_ws_url,
            server_quic_url: value.server_quic_url,
            upstream_base_url,
            upstream_api_key: value.credential_ref,
            public_model: primary_model.clone(),
            upstream_model: primary_model,
            models: value.models,
            supported_protocols,
            capability_profiles: value.capability_profiles,
            detection_checks: value.detection_checks,
            surface_bindings: value.surfaces,
            price_ratio: value.price_ratio,
            subscription,
        }
    }
}

fn configured_default_model(upstream: &str, public: &str, models: &[String]) -> String {
    let configured = [upstream, public]
        .into_iter()
        .find(|value| !value.trim().is_empty())
        .unwrap_or_default();
    observed_default_model(configured, models)
}

fn observed_default_model(configured: &str, models: &[String]) -> String {
    let configured = configured.trim();
    if !configured.is_empty() {
        if let Some(observed) = models
            .iter()
            .find(|model| model.trim().eq_ignore_ascii_case(configured))
        {
            return observed.clone();
        }
        if let Some(observed) = models.iter().find(|model| {
            crate::config::short_model_name(model)
                .eq_ignore_ascii_case(crate::config::short_model_name(configured))
        }) {
            return observed.clone();
        }
    }
    models
        .first()
        .cloned()
        .unwrap_or_else(|| configured.to_string())
}

impl From<SubscriptionAdapterConfig> for SubscriptionConfigV2 {
    fn from(value: SubscriptionAdapterConfig) -> Self {
        Self {
            platform: value.platform,
            account_label: value.account_label,
            max_concurrency: value.max_concurrency,
            responses_ws_pool_enabled: value.responses_ws_pool_enabled,
            responses_ws_subscription_multiplex_probe_enabled: value
                .responses_ws_subscription_multiplex_probe_enabled,
            rpm_limit: 0,
            quota_reserve_percent: value.quota_reserve_percent.min(100),
            daily_request_limit: 0,
            cooldown_until_unix: value.cooldown_until_unix,
            risk_note: String::new(),
            audit_enabled: false,
        }
    }
}

impl From<SubscriptionConfigV2> for SubscriptionAdapterConfig {
    fn from(value: SubscriptionConfigV2) -> Self {
        Self {
            platform: value.platform,
            account_label: value.account_label,
            credential_ref: String::new(),
            max_concurrency: value.max_concurrency,
            responses_ws_pool_enabled: value.responses_ws_pool_enabled,
            responses_ws_subscription_multiplex_probe_enabled: value
                .responses_ws_subscription_multiplex_probe_enabled,
            rpm_limit: 0,
            quota_reserve_percent: value.quota_reserve_percent.min(100),
            daily_request_limit: 0,
            cooldown_until_unix: value.cooldown_until_unix,
            risk_note: String::new(),
            audit_enabled: false,
        }
    }
}

impl Serialize for SupplierConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        SupplierConfigWire::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SupplierConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        SupplierConfigV4Read::deserialize(deserializer).map(Into::into)
    }
}

impl Serialize for ChannelConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        ChannelConfigV2::from(self.clone()).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ChannelConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let mut channel = ChannelConfig::from(ChannelConfigV2::deserialize(deserializer)?);
        crate::coding_gateway::upgrade_legacy_zen_surfaces(&mut channel);
        crate::channel_surface::repair_channel_surface_contract(&mut channel);
        crate::channel_surface::validate_channel_v2(&channel).map_err(serde::de::Error::custom)?;
        crate::channel_surface::validate_channel_source_policy(&channel)
            .map_err(serde::de::Error::custom)?;
        Ok(channel)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub(crate) struct ChannelSurfaceBinding {
    pub(crate) surface: crate::surface::ApiSurface,
    pub(crate) base_url: String,
    pub(crate) endpoint_profile: String,
    pub(crate) auth_scheme: String,
    #[serde(default)]
    pub(crate) protocols: Vec<ChannelProtocolBinding>,
    #[serde(default)]
    pub(crate) operation_overrides: Vec<OperationEndpointOverride>,
    #[serde(default)]
    pub(crate) verification: SurfaceVerification,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub(crate) struct ChannelProtocolBinding {
    pub(crate) protocol: String,
    #[serde(default)]
    pub(crate) preferred: bool,
    #[serde(default)]
    pub(crate) verification: ProtocolVerification,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub(crate) struct OperationEndpointOverride {
    pub(crate) operation: String,
    pub(crate) method: String,
    pub(crate) url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub(crate) struct SurfaceVerification {
    pub(crate) state: String,
    pub(crate) checked_at_unix: i64,
    pub(crate) summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub(crate) struct ProtocolVerification {
    pub(crate) state: String,
    pub(crate) checked_at_unix: i64,
    pub(crate) summary: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProxyStatus {
    pub(crate) running: bool,
    pub(crate) listen: String,
    pub(crate) active_endpoint: Option<String>,
    pub(crate) platform_connection_state: String,
    pub(crate) platform_transport: String,
    pub(crate) platform_transport_error: String,
    pub(crate) platform_server_version: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SupplierStatus {
    pub(crate) running: bool,
    pub(crate) starting: bool,
    pub(crate) node_id: String,
    pub(crate) server_ws_url: String,
    pub(crate) server_quic_url: String,
    pub(crate) transport_preference: String,
    pub(crate) connected_transport: String,
    pub(crate) active_transport: String,
    pub(crate) last_transport_error: String,
    #[serde(default)]
    pub(crate) nodes: Vec<String>,
    #[serde(default)]
    pub(crate) channels: Vec<SupplierChannelHealth>,
    #[serde(default)]
    pub(crate) last_error: String,
    #[serde(default)]
    pub(crate) route_statuses: Vec<SupplierNodeRouteStatus>,
    #[serde(default)]
    pub(crate) channel_transports: Vec<SupplierChannelTransportStatus>,
    // Local UI observations only; omitted by status responses without a config snapshot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) availability_groups:
        Option<std::collections::BTreeMap<String, Vec<crate::supplier::availability::GroupResult>>>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub(crate) struct SupplierChannelTransportStatus {
    pub(crate) channel_id: String,
    pub(crate) supplier_unit_id: String,
    pub(crate) connected_transport: String,
    pub(crate) active_transport: String,
    pub(crate) last_transport_error: String,
    pub(crate) last_register_ack_unix: i64,
    pub(crate) platform_registered: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PricingFallbackInfo {
    pub(crate) model: String,
    pub(crate) provider: String,
    pub(crate) billing_model: String,
    pub(crate) pricing_rule_id: String,
    pub(crate) confidence: String,
    pub(crate) price_version_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct SupplierNodeRouteStatus {
    pub(crate) node_id: String,
    pub(crate) channel_id: String,
    pub(crate) supplier_unit_id: String,
    pub(crate) state: String,
    pub(crate) reason: String,
    pub(crate) message: String,
    #[serde(default)]
    pub(crate) protocol: String,
    #[serde(default)]
    pub(crate) target_protocol: String,
    #[serde(default)]
    pub(crate) model: String,
    #[serde(default)]
    pub(crate) feature: String,
    #[serde(default)]
    pub(crate) issue_code: String,
    #[serde(default)]
    pub(crate) accepted_models: Vec<String>,
    #[serde(default)]
    pub(crate) unsupported_models: Vec<String>,
    #[serde(default)]
    pub(crate) fallback_priced_models: Vec<PricingFallbackInfo>,
    #[serde(default)]
    pub(crate) catalog_release_id: String,
    pub(crate) cooldown_until_unix: i64,
    pub(crate) updated_at_unix: i64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SupplierChannelHealth {
    pub(crate) channel_id: String,
    pub(crate) name: String,
    pub(crate) status: String,
    pub(crate) model: String,
    pub(crate) model_count: usize,
    pub(crate) latency_ms: u128,
    #[serde(default)]
    pub(crate) upstream_http_version: String,
    pub(crate) credential_status: String,
    pub(crate) quota_status: String,
    pub(crate) remaining_ratio: f64,
    pub(crate) daily_used: u32,
    pub(crate) daily_limit: u32,
    pub(crate) quota_checked_at_unix: i64,
    #[serde(default)]
    pub(crate) quota_windows: Vec<QuotaWindow>,
    pub(crate) message: String,
    pub(crate) checked_at_unix: i64,
    #[serde(default)]
    pub(crate) safety_state: String,
    #[serde(default)]
    pub(crate) last_error_kind: String,
    #[serde(default)]
    pub(crate) cooldown_until_unix: i64,
    #[serde(default)]
    pub(crate) success_ewma: f64,
    #[serde(default)]
    pub(crate) latency_ewma_ms: f64,
    #[serde(default)]
    pub(crate) current_concurrency: u32,
    #[serde(default)]
    pub(crate) max_concurrency: u32,
    #[serde(default)]
    pub(crate) rpm_limit: u32,
    #[serde(default)]
    pub(crate) rpm_remaining: u32,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SupplierChannelValidationResult {
    pub(crate) channel: ChannelConfig,
    pub(crate) health: SupplierChannelHealth,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ToolApplyResult {
    pub(crate) tool: String,
    pub(crate) files: Vec<String>,
    pub(crate) backups: Vec<String>,
    #[serde(default)]
    pub(crate) file_statuses: Vec<ToolFileStatus>,
    #[serde(default)]
    pub(crate) already_configured: bool,
    #[serde(default)]
    pub(crate) details: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct ToolProgramCandidate {
    pub(crate) path: String,
    pub(crate) label: String,
    pub(crate) kind: String,
    pub(crate) exists: bool,
    #[serde(default)]
    pub(crate) modified_at_unix: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) version: Option<String>,
    #[serde(default)]
    pub(crate) selected: bool,
    #[serde(default)]
    pub(crate) launchable: bool,
    #[serde(default)]
    pub(crate) recommended: bool,
    #[serde(default)]
    pub(crate) status: String,
    #[serde(default)]
    pub(crate) reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ToolProgramLocationResult {
    pub(crate) tool: String,
    pub(crate) selected_path: String,
    pub(crate) candidates: Vec<ToolProgramCandidate>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ToolFileStatus {
    pub(crate) path: String,
    pub(crate) exists_before: bool,
    pub(crate) changed: bool,
    pub(crate) already_configured: bool,
    pub(crate) before_sha256: String,
    pub(crate) after_sha256: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub(crate) struct CodexSessionScanResult {
    pub(crate) provider_counts: HashMap<String, usize>,
    pub(crate) files: Vec<CodexSessionFileSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CodexSessionFileSummary {
    pub(crate) path: String,
    pub(crate) provider: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub(crate) struct CodexSessionMigrationResult {
    pub(crate) migrated_files: usize,
    pub(crate) skipped_files: usize,
    pub(crate) migrated_sqlite_rows: usize,
    pub(crate) skipped_invalid_sqlite_rows: usize,
    #[serde(default)]
    pub(crate) target_provider: String,
    pub(crate) backups: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) sqlite_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LocalProxyTestResult {
    pub(crate) http_status: u16,
    pub(crate) latency_ms: u128,
    pub(crate) model: String,
    pub(crate) protocol: String,
    pub(crate) path: String,
    pub(crate) route: String,
    pub(crate) skip_local: bool,
    pub(crate) content: String,
    pub(crate) raw: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProtocolDebugResult {
    pub(crate) target_type: String,
    pub(crate) target_id: String,
    pub(crate) inbound_protocol: String,
    pub(crate) target_protocol: String,
    pub(crate) requested_model: String,
    pub(crate) upstream_model: String,
    pub(crate) conversion_level: String,
    pub(crate) path: String,
    pub(crate) request_headers: serde_json::Value,
    pub(crate) request_body: serde_json::Value,
    pub(crate) unsupported_fields: Vec<String>,
    pub(crate) lossy_warnings: Vec<String>,
    pub(crate) executed: bool,
    pub(crate) http_status: Option<u16>,
    pub(crate) latency_ms: Option<u128>,
    pub(crate) content_type: String,
    pub(crate) content: String,
    pub(crate) raw: String,
    pub(crate) metrics: ProtocolDebugMetrics,
    pub(crate) error_layer: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProtocolDebugMetrics {
    pub(crate) request_body_bytes: usize,
    pub(crate) response_raw_bytes: usize,
    pub(crate) response_content_bytes: usize,
    pub(crate) response_content_chars: usize,
    pub(crate) response_raw_lines: usize,
    pub(crate) response_sse_events: usize,
    pub(crate) response_sse_done: bool,
    pub(crate) response_sse_last_event_type: String,
    pub(crate) response_kind: String,
    pub(crate) tool_call_count: usize,
    pub(crate) finish_reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ChannelUpstreamTestResult {
    pub(crate) http_status: u16,
    pub(crate) latency_ms: u128,
    pub(crate) upstream_http_version: String,
    pub(crate) model: String,
    pub(crate) content: String,
    pub(crate) raw: String,
    pub(crate) input_tokens: i64,
    pub(crate) output_tokens: i64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SurfaceProbeResult {
    pub(crate) surface: crate::surface::ApiSurface,
    pub(crate) protocol: crate::protocol::kind::ProtocolKind,
    pub(crate) surface_verification: SurfaceVerification,
    pub(crate) protocol_verification: ProtocolVerification,
    pub(crate) models: Vec<String>,
    pub(crate) model_capability_evidence: Vec<ChannelCapabilityProfile>,
    pub(crate) detection_evidence: Vec<ChannelDetectionCheck>,
    pub(crate) checks: Vec<String>,
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ChannelDetectionResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) availability: Option<crate::supplier::availability::Report>,
    pub(crate) models: ModelSnapshot,
    pub(crate) surface_results: Vec<SurfaceProbeResult>,
    pub(crate) model_capability_evidence: Vec<ChannelCapabilityProfile>,
    pub(crate) detection_evidence: Vec<ChannelDetectionCheck>,
    #[serde(default)]
    pub(crate) quota_status: String,
    #[serde(default)]
    pub(crate) remaining_ratio: f64,
    #[serde(default)]
    pub(crate) quota_windows: Vec<QuotaWindow>,
    pub(crate) checks: Vec<String>,
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub(crate) struct ChannelCapabilityProfile {
    pub(crate) protocol: String,
    /// Advisory catalog metadata used only to choose a small availability check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) probe_cost_nanos: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) catalog_text_output: Option<bool>,
    /// Original catalog identity, retained before public short-name normalization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) catalog_source_model: Option<String>,
    /// Numeric limits from this channel's live model catalog, not the bundled catalog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) context_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) catalog_metadata: bool,
    /// Explicit per-model endpoint support; absence retains the prior contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) catalog_protocol_supported: Option<bool>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) model_pattern: String,
    #[serde(default, skip_serializing_if = "capability_layer_is_default")]
    pub(crate) capability_layer: String,
    #[serde(default)]
    pub(crate) input_modalities_authoritative: bool,
    #[serde(default)]
    pub(crate) output_modalities_authoritative: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) release_status: String,
    #[serde(default)]
    pub(crate) non_stream_json: bool,
    #[serde(default)]
    pub(crate) stream_sse: bool,
    #[serde(default)]
    pub(crate) stream_sse_unsupported: bool,
    #[serde(default)]
    pub(crate) tool_calls: bool,
    #[serde(default)]
    pub(crate) tool_choice: bool,
    #[serde(default)]
    pub(crate) parallel_tool_calls: bool,
    #[serde(default)]
    pub(crate) json_schema: bool,
    #[serde(default)]
    pub(crate) reasoning: bool,
    #[serde(default)]
    pub(crate) thinking: bool,
    #[serde(default)]
    pub(crate) vision: bool,
    #[serde(default)]
    pub(crate) image_input: bool,
    #[serde(default)]
    pub(crate) image_output: bool,
    #[serde(default)]
    pub(crate) audio_input: bool,
    #[serde(default)]
    pub(crate) audio_output: bool,
    #[serde(default)]
    pub(crate) video_input: bool,
    #[serde(default)]
    pub(crate) video_output: bool,
    #[serde(default)]
    pub(crate) file_input: bool,
    #[serde(default)]
    pub(crate) file_output: bool,
    #[serde(default)]
    pub(crate) cache_control: bool,
    #[serde(default)]
    pub(crate) hosted_tools: Vec<String>,
    #[serde(default)]
    pub(crate) custom_tool: bool,
    #[serde(default)]
    pub(crate) verification_state: String,
    #[serde(default)]
    pub(crate) verified_at_unix: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct SupplierCapabilityEvidence {
    pub(crate) feature: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) token_limit: Option<u64>,
    pub(crate) state: String,
    pub(crate) source: String,
    #[serde(default, skip_serializing_if = "capability_layer_is_default")]
    pub(crate) capability_layer: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) model_pattern: Option<String>,
    pub(crate) protocol: String,
    pub(crate) observed_at_unix: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) target_protocol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) conversion_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) release_status: Option<String>,
}

pub(crate) fn normalized_capability_layer(layer: &str) -> &'static str {
    match layer.trim().to_ascii_lowercase().as_str() {
        "driver" => "driver",
        "client_host" => "client_host",
        "product_service" => "product_service",
        _ => "model_wire",
    }
}

fn capability_layer_is_default(layer: &String) -> bool {
    normalized_capability_layer(layer) == "model_wire"
}

pub(crate) fn capability_layer_is_routable(layer: &str) -> bool {
    matches!(normalized_capability_layer(layer), "model_wire" | "driver")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct SupplierConversionReachability {
    pub(crate) source_protocol: String,
    pub(crate) target_protocol: String,
    pub(crate) state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub(crate) struct ChannelDetectionCheck {
    pub(crate) name: String,
    pub(crate) status: String,
    #[serde(default)]
    pub(crate) checked_at_unix: i64,
    #[serde(default)]
    pub(crate) protocol: String,
    #[serde(default)]
    pub(crate) capability: String,
    #[serde(default)]
    pub(crate) message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct EndpointRegistryManifest {
    pub(crate) version: i64,
    #[serde(default)]
    pub(crate) platform_id: String,
    pub(crate) expires_at: String,
    #[serde(default)]
    pub(crate) endpoints: Vec<EndpointRegistryNode>,
    #[serde(default)]
    pub(crate) signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct EndpointRegistryNode {
    #[serde(default)]
    pub(crate) server_id: String,
    pub(crate) id: String,
    pub(crate) api: String,
    #[serde(default)]
    pub(crate) ws: String,
    #[serde(default)]
    pub(crate) quic: String,
    #[serde(default)]
    pub(crate) quic_cert_sha256: String,
    #[serde(default)]
    pub(crate) region: String,
    #[serde(default)]
    pub(crate) weight: i32,
    #[serde(default)]
    pub(crate) min_client_version: String,
    #[serde(default)]
    pub(crate) preferred_from_client_version: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct EndpointDiscoveryResult {
    pub(crate) source: String,
    pub(crate) version: i64,
    pub(crate) platform_id: String,
    pub(crate) endpoints: Vec<Endpoint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) refresh_warning: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SubscriptionAdapterInspection {
    pub(crate) status: String,
    pub(crate) safe_to_share: bool,
    pub(crate) checks: Vec<String>,
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SubscriptionCredentialCandidate {
    pub(crate) provider: String,
    pub(crate) label: String,
    pub(crate) email: String,
    pub(crate) path: String,
    pub(crate) expires_at: String,
    pub(crate) has_access_token: bool,
    pub(crate) has_refresh_token: bool,
    pub(crate) status: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SubscriptionOAuthSession {
    pub(crate) provider: String,
    pub(crate) auth_url: String,
    pub(crate) state: String,
    pub(crate) code_verifier: String,
    pub(crate) redirect_uri: String,
    pub(crate) callback_hint: String,
    pub(crate) auto_callback: bool,
}

pub(crate) struct AntigravitySubscriptionHealth {
    pub(crate) model_observations: Vec<crate::model_catalog::ModelObservation>,
    pub(crate) oauth_type: String,
    pub(crate) project_id: String,
    pub(crate) tier_id: String,
    pub(crate) summary: String,
    pub(crate) models: Vec<String>,
    pub(crate) remaining_ratio: f64,
    pub(crate) quota_status: String,
    pub(crate) quota_windows: Vec<QuotaWindow>,
    pub(crate) warnings: Vec<String>,
    pub(crate) refreshed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct QuotaWindow {
    pub(crate) source: String,
    pub(crate) window: String,
    #[serde(default)]
    pub(crate) remaining_ratio: f64,
    #[serde(default)]
    pub(crate) used_percent: f64,
    #[serde(default)]
    pub(crate) reset_at_unix: i64,
    #[serde(default)]
    pub(crate) checked_at_unix: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) token_type: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct SubscriptionQuotaSnapshot {
    pub(crate) status: String,
    pub(crate) remaining_ratio: f64,
    pub(crate) daily_used: u32,
    pub(crate) daily_limit: u32,
    pub(crate) checked_at_unix: i64,
    pub(crate) quota_source: String,
    pub(crate) quota_window: String,
    pub(crate) quota_used_percent: f64,
    pub(crate) quota_reset_at_unix: i64,
    pub(crate) quota_windows: Vec<QuotaWindow>,
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) config_path: PathBuf,
    pub(crate) proxy: Arc<Mutex<ProxyRuntime>>,
    pub(crate) proxy_config: Arc<StdMutex<ClientConfig>>,
    pub(crate) local_channel_readiness: Arc<StdMutex<HashMap<String, bool>>>,
    pub(crate) local_model_quota_routes:
        Arc<StdMutex<HashMap<String, crate::supplier::LocalModelQuotaRouteState>>>,
    pub(crate) supplier: Arc<Mutex<SupplierRuntime>>,
    pub(crate) account: Arc<Mutex<crate::account::AccountRuntime>>,
    pub(crate) model_compatibility_http_cache:
        Arc<StdMutex<crate::model_compatibility::ModelCompatibilityHttpCache>>,
    pub(crate) account_refresh_notify: Arc<tokio::sync::Notify>,
    pub(crate) platform_api_key: Arc<StdMutex<String>>,
    pub(crate) platform_transport: Arc<crate::platform_transport::PlatformHttpTransport>,
    pub(crate) subscription_oauth_callbacks:
        crate::subscription_oauth_callback::SubscriptionOAuthCallbackManager,
}

pub(crate) struct ProxyRuntime {
    pub(crate) running: bool,
    pub(crate) listen: String,
    pub(crate) active_endpoint: Option<String>,
    pub(crate) platform_transport_state:
        Arc<StdMutex<crate::platform_transport::PlatformTransportSnapshot>>,
    pub(crate) shutdown: Option<oneshot::Sender<()>>,
    pub(crate) server_task: Option<tokio::task::JoinHandle<()>>,
}

pub(crate) struct SupplierRuntime {
    pub(crate) running: bool,
    pub(crate) starting: bool,
    pub(crate) node_id: String,
    pub(crate) server_ws_url: String,
    pub(crate) server_quic_url: String,
    pub(crate) transport_preference: String,
    pub(crate) transport_state: Arc<StdMutex<SupplierTransportSnapshot>>,
    pub(crate) nodes: Vec<String>,
    pub(crate) channels: Vec<SupplierChannelHealth>,
    pub(crate) last_error: String,
    pub(crate) channel_agents: HashMap<String, SupplierAgentHandle>,
}

pub(crate) struct SupplierAgentHandle {
    pub(crate) client_id: String,
    pub(crate) channel_ids: Vec<String>,
    pub(crate) node_ids: Vec<String>,
    pub(crate) suppliers: Vec<SupplierConfig>,
    pub(crate) agent_shutdown: oneshot::Sender<()>,
    pub(crate) monitor_shutdown: oneshot::Sender<()>,
    pub(crate) register_update: mpsc::Sender<SupplierRegistrationUpdate>,
}

#[derive(Debug, Clone)]
pub(crate) struct SupplierRegistrationUpdate {
    pub(crate) suppliers: Vec<SupplierConfig>,
    pub(crate) force: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SupplierTransportSnapshot {
    pub(crate) nodes: std::collections::HashMap<String, SupplierTransportNodeSnapshot>,
    pub(crate) model_admission_statuses: std::collections::HashMap<String, SupplierNodeRouteStatus>,
    pub(crate) channel_health: std::collections::HashMap<String, SupplierChannelHealth>,
    pub(crate) last_transport_error: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SupplierTransportNodeSnapshot {
    pub(crate) connected_transport: String,
    pub(crate) connected_changed_at_unix: i64,
    pub(crate) active_transport: String,
    pub(crate) last_transport_error: String,
    pub(crate) changed_at_unix: i64,
    pub(crate) last_register_ack_unix: i64,
    pub(crate) route_status: Option<SupplierNodeRouteStatus>,
}

impl SupplierTransportSnapshot {
    pub(crate) fn aggregate(&self) -> (String, String) {
        let active: Vec<(&str, i64)> = self
            .nodes
            .values()
            .map(|node| (node.active_transport.as_str(), node.changed_at_unix))
            .filter(|(transport, _)| !transport.is_empty())
            .collect();
        let active_transport = aggregate_supplier_transports(active);
        (active_transport, self.last_transport_error.clone())
    }

    pub(crate) fn connected_transport(&self) -> String {
        aggregate_supplier_transports(
            self.nodes
                .values()
                .map(|node| {
                    (
                        node.connected_transport.as_str(),
                        node.connected_changed_at_unix,
                    )
                })
                .filter(|(transport, _)| !transport.is_empty())
                .collect(),
        )
    }

    pub(crate) fn route_statuses(&self) -> Vec<SupplierNodeRouteStatus> {
        let mut keys = self.nodes.keys().cloned().collect::<Vec<_>>();
        for key in self.model_admission_statuses.keys() {
            if !keys.contains(key) {
                keys.push(key.clone());
            }
        }
        let mut statuses = keys
            .into_iter()
            .filter_map(|key| {
                let operational = self
                    .nodes
                    .get(&key)
                    .and_then(|node| node.route_status.as_ref());
                let admission = self.model_admission_statuses.get(&key);
                match operational {
                    Some(status) if status.state != "ready" => Some(status.clone()),
                    Some(status) => admission.cloned().or_else(|| Some(status.clone())),
                    None => admission.cloned(),
                }
            })
            .collect::<Vec<_>>();
        statuses.sort_by(|a, b| a.channel_id.cmp(&b.channel_id));
        statuses
    }
}

fn aggregate_supplier_transports(mut transports: Vec<(&str, i64)>) -> String {
    transports.sort_by_key(|(_, changed_at_unix)| *changed_at_unix);
    if transports.is_empty() {
        return String::new();
    }
    if transports
        .iter()
        .all(|(transport, _)| *transport == transports[0].0)
    {
        return transports[0].0.to_string();
    }
    if let Some((transport, _)) = transports
        .iter()
        .rev()
        .find(|(transport, _)| *transport == "quic" || *transport == "websocket")
    {
        return (*transport).to_string();
    }
    if transports
        .iter()
        .any(|(transport, _)| *transport == "reconnecting")
    {
        return "reconnecting".to_string();
    }
    "starting".to_string()
}
