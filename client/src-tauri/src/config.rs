use crate::home_dir;
use crate::{endpoint::merge_discovered_endpoints, model::*};
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex as StdMutex, OnceLock},
};

pub(crate) const CURRENT_CLIENT_CONFIG_VERSION: u32 = 5;
pub(crate) const DEFAULT_LOCAL_PROXY_LISTEN: &str = "127.0.0.1:38787";
pub(crate) const DEFAULT_DEVELOPMENT_PROXY_LISTEN: &str = "127.0.0.1:38788";
#[cfg(debug_assertions)]
const CLIENT_PROFILE_ENV: &str = "CONST_API_CLIENT_PROFILE";
#[cfg(debug_assertions)]
const CLIENT_DATA_DIR_ENV: &str = "CONST_API_CLIENT_DATA_DIR";
#[cfg(debug_assertions)]
const CLIENT_CONFIG_PATH_ENV: &str = "CONST_API_CLIENT_CONFIG_PATH";
const LEGACY_PLATFORM_DEVICE_KEY_PREFIX: &str = "sk-ca_";
const LEGACY_PRICE_RATIO_SCALE: f64 = 1_000_000.0;
const LEGACY_PRICE_RATIO_THRESHOLD: f64 = 1_000.0;
const MAX_PRICE_RATIO: f64 = 3.0;
pub(crate) const DEFAULT_SUPPLIER_MAX_CONCURRENCY: u32 = 20;
pub(crate) const DEVELOPMENT_ENDPOINT_AUTO: &str = "auto";
pub(crate) const DEVELOPMENT_ENDPOINT_LOCAL: &str = "local-dev";
const LEGACY_DOMESTIC_ENDPOINT_REGISTRY: &str =
    "https://const.tos-cn-shanghai.volces.com/registry/endpoints.json";
const LEGACY_PUBLISHED_ENDPOINT_REGISTRY: &str = "https://github.com/llxisdsh/const-api-public/releases/download/endpoint-registry/endpoints.json";
static CONFIG_COMMIT_LOCK: OnceLock<StdMutex<()>> = OnceLock::new();
const LEGACY_PRIVATE_ENDPOINT_REGISTRY: &str =
    "https://raw.githubusercontent.com/llxisdsh/const-api/main/registry/endpoints.json";
const LEGACY_LOCAL_ENDPOINT_REGISTRY: &str = "http://127.0.0.1:8080/registry/endpoints.json";

pub(crate) fn generate_scoped_api_key(scope: &str) -> String {
    debug_assert_eq!(scope.len(), 3);
    debug_assert!(scope.bytes().all(|byte| byte.is_ascii_lowercase()));
    format!("sk-{scope}-{:032x}", rand::random::<u128>())
}

pub(crate) fn generate_local_api_key() -> String {
    generate_scoped_api_key("api")
}

#[cfg(debug_assertions)]
fn development_profile_value(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "dev" | "development"
        )
    })
}

pub(crate) fn development_profile_active() -> bool {
    #[cfg(debug_assertions)]
    {
        return development_profile_value(std::env::var(CLIENT_PROFILE_ENV).ok().as_deref());
    }
    #[cfg(not(debug_assertions))]
    false
}

fn client_data_root_from(home: PathBuf, override_path: Option<PathBuf>) -> PathBuf {
    override_path.unwrap_or_else(|| home.join(".const-api"))
}

pub(crate) fn client_data_root() -> PathBuf { home_dir().join(".const-api-local") }

fn config_path_from(data_root: PathBuf, override_path: Option<PathBuf>) -> PathBuf {
    override_path.unwrap_or_else(|| data_root.join("client.json"))
}

pub(crate) fn default_config_path() -> PathBuf { client_data_root().join("client.json") }

fn local_proxy_listen_for_profile(development: bool) -> &'static str {
    if development {
        DEFAULT_DEVELOPMENT_PROXY_LISTEN
    } else {
        DEFAULT_LOCAL_PROXY_LISTEN
    }
}

pub(crate) fn default_local_proxy_listen() -> &'static str { "127.0.0.1:38789" }

pub(crate) fn application_display_name() -> &'static str { "CONST API Local" }

fn default_endpoint_registry_sources() -> Vec<String> { Vec::new() }

pub(crate) fn default_config() -> ClientConfig { let mut exported_config = (|| {
    let supplier = default_supplier_config();
    let channel = channel_from_supplier("channel-1".to_string(), &supplier);
    let mut config = ClientConfig {
        config_version: CURRENT_CLIENT_CONFIG_VERSION,
        client_id: machine_client_id(),
        platform_id: String::new(),
        account_platform_id: String::new(),
        account_home_server_id: String::new(),
        account_home_base_url: String::new(),
        account_user_id: String::new(),
        account_email: String::new(),
        account_device_api_key: String::new(),
        listen: default_local_proxy_listen().to_string(),
        allow_lan_access: false,
        lan_share: LanShareConfig::default(),
        proxy_auto_start: true,
        automatic_updates: true,
        supplier_auto_start: false,
        api_key: generate_local_api_key(),
        registry_sources: default_endpoint_registry_sources(),
        registry_signature_secret: String::new(),
        registry_public_keys: Vec::new(),
        registry_version: 0,
        development_endpoint: default_development_endpoint(),
        allow_model_equivalence: default_allow_model_equivalence(),
        prefer_local_supply: true,
        allow_unverified_platform_routes: true,
        model_aliases: default_model_aliases(),
        model_compatibility_profiles: default_model_compatibility_profiles(),
        endpoints: Vec::new(),
        channels: vec![channel],
        supplier,
    };
    match crate::endpoint::bundled_endpoint_discovery() {
        Ok(discovery) => {
            crate::endpoint::apply_endpoint_discovery_to_config(&mut config, &discovery)
        }
        Err(error) => {
            log::error!("[const-api][endpoint] bundled endpoint snapshot is unavailable: {error:#}")
        }
    }
    config
})(); crate::local_policy::enforce(&mut exported_config); exported_config }

pub(crate) fn default_supplier_config() -> SupplierConfig {
    let source_driver = crate::source_driver::SourceDriverId::CustomEndpoint;
    let v2 = channel_v2_contract_for_source(source_driver);
    let upstream_base_url = "http://127.0.0.1:8317".to_string();
    let surface_bindings = source_driver_surface_bindings(source_driver, &upstream_base_url);
    SupplierConfig {
        source_driver,
        credential_ref: String::new(),
        executor: v2.executor,
        default_target: v2.default_target,
        discovery: v2.discovery,
        user_agent_profile: String::new(),
        channel_id: "channel-1".to_string(),
        enabled: false,
        kind: default_channel_kind(),
        api_format: default_channel_api_format(),
        node_id: "local-supplier".to_string(),
        name: "Local Supplier".to_string(),
        server_ws_url: String::new(),
        server_quic_url: String::new(),
        upstream_base_url,
        upstream_api_key: String::new(),
        public_model: String::new(),
        upstream_model: String::new(),
        models: Vec::new(),
        supported_protocols: vec!["openai_chat".to_string()],
        capability_profiles: Vec::new(),
        detection_checks: Vec::new(),
        surface_bindings,
        price_ratio: default_price_ratio(),
        max_concurrency: default_channel_max_concurrency(),
        quota_reserve_percent: 0,
        subscription: default_subscription_adapter_config(),
        registration_removed: false,
        registration_health_status: None,
        registration_quota_status: None,
    }
}

pub(crate) fn default_price_ratio() -> f64 {
    1.0
}

pub(crate) fn default_development_endpoint() -> String {
    development_endpoint_for_profile(development_profile_active()).to_string()
}

fn development_endpoint_for_profile(development: bool) -> &'static str {
    if development {
        DEVELOPMENT_ENDPOINT_LOCAL
    } else {
        DEVELOPMENT_ENDPOINT_AUTO
    }
}

pub(crate) fn normalize_development_endpoint(value: &str) -> String {
    let value = value.trim();
    if value.eq_ignore_ascii_case(DEVELOPMENT_ENDPOINT_AUTO) {
        return DEVELOPMENT_ENDPOINT_AUTO.to_string();
    }
    if !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return value.to_string();
    }
    DEVELOPMENT_ENDPOINT_AUTO.to_string()
}

pub(crate) fn default_channel_kind() -> String {
    "openai_compatible".to_string()
}

pub(crate) fn default_channel_api_format() -> String {
    "openai_chat".to_string()
}

pub(crate) fn default_subscription_platform() -> String {
    "claude".to_string()
}

pub(crate) fn default_channel_max_concurrency() -> u32 {
    DEFAULT_SUPPLIER_MAX_CONCURRENCY
}

// Kept for deserializing the legacy subscription-scoped compatibility field.
pub(crate) fn default_subscription_max_concurrency() -> u32 {
    default_channel_max_concurrency()
}

pub(crate) fn default_responses_ws_pool_enabled() -> bool {
    true
}

pub(crate) fn default_responses_ws_subscription_multiplex_probe_enabled() -> bool {
    false
}

pub(crate) fn default_subscription_rpm_limit() -> u32 {
    0
}

pub(crate) fn default_subscription_daily_request_limit() -> u32 {
    0
}

pub(crate) fn default_subscription_adapter_config() -> SubscriptionAdapterConfig {
    SubscriptionAdapterConfig {
        platform: default_subscription_platform(),
        account_label: String::new(),
        credential_ref: String::new(),
        max_concurrency: default_subscription_max_concurrency(),
        responses_ws_pool_enabled: default_responses_ws_pool_enabled(),
        responses_ws_subscription_multiplex_probe_enabled:
            default_responses_ws_subscription_multiplex_probe_enabled(),
        rpm_limit: default_subscription_rpm_limit(),
        quota_reserve_percent: 0,
        daily_request_limit: default_subscription_daily_request_limit(),
        cooldown_until_unix: 0,
        risk_note: String::new(),
        audit_enabled: false,
    }
}

#[cfg(test)]
pub(crate) fn default_channel_v2_contract() -> ChannelConfigV2Contract {
    channel_v2_contract_for_source(crate::source_driver::SourceDriverId::CustomEndpoint)
}

pub(crate) fn channel_v2_contract_for_source(
    source_driver: crate::source_driver::SourceDriverId,
) -> ChannelConfigV2Contract {
    let driver = crate::source_driver::source_driver(source_driver).ok();
    let default_target = driver
        .map(|driver| driver.default_target.clone())
        .unwrap_or(ChannelTarget {
            surface: crate::surface::ApiSurface::OpenAi,
            protocol: crate::protocol::kind::ProtocolKind::OpenAiChat,
        });
    let discovery = driver
        .map(|driver| driver.discovery.clone())
        .unwrap_or(DiscoveryProfile {
            strategy: "custom".to_string(),
            catalog: true,
            quota: false,
            metadata: true,
        });
    ChannelConfigV2Contract {
        created_at_unix_ms: 0,
        source_driver,
        credential_ref: String::new(),
        executor: driver
            .map(|driver| driver.executor.clone())
            .unwrap_or(ChannelExecutorLocator::HttpSurface),
        default_target,
        discovery,
        user_agent_profile: String::new(),
        max_concurrency: default_channel_max_concurrency(),
        quota_reserve_percent: 0,
    }
}

pub(crate) fn default_subscription_safety_active_state() -> String {
    "active".to_string()
}

pub(crate) fn default_subscription_safety_success_ewma() -> f64 {
    1.0
}

pub(crate) fn default_true() -> bool {
    true
}

pub(crate) fn default_allow_model_equivalence() -> bool {
    true
}

pub(crate) fn default_model_aliases() -> std::collections::HashMap<String, String> {
    std::collections::HashMap::new()
}

pub(crate) fn default_model_compatibility_profiles()
-> std::collections::HashMap<String, LocalModelCompatibilityProfile> {
    std::collections::HashMap::new()
}

pub(crate) fn load_config_from_path(path: &PathBuf) -> Result<ClientConfig> {
    let raw = fs::read(path).with_context(|| format!("read config {}", path.display()))?;
    // Ignore channel catalogs and capability evidence in this first pass,
    // rather than building a second complete JSON tree just to read a version.
    #[derive(serde::Deserialize)]
    struct Metadata {
        #[serde(default)]
        config_version: serde_json::Value,
    }
    let version = serde_json::from_slice::<Metadata>(&raw)
        .map_err(|error| anyhow::anyhow!("decode config metadata {}: {error}", path.display()))?
        .config_version
        .as_u64()
        .unwrap_or_default() as u32;
    let needs_migration = version < CURRENT_CLIENT_CONFIG_VERSION;
    let mut cfg = if needs_migration {
        migrate_client_config_v4(serde_json::from_slice::<ClientConfigV4Read>(&raw).map_err(
            |error| {
                anyhow::anyhow!(
                    "decode legacy config v{version} {}: {error}",
                    path.display()
                )
            },
        )?)
    } else if version == CURRENT_CLIENT_CONFIG_VERSION {
        serde_json::from_slice::<ClientConfig>(&raw).map_err(|error| {
            anyhow::anyhow!("decode config v{version} {}: {error}", path.display())
        })?
    } else {
        return Err(anyhow::anyhow!(
            "unsupported client config version {version}"
        ));
    };
    if cfg.listen.trim().is_empty() {
        cfg.listen = default_local_proxy_listen().to_string();
    }
    if cfg.registry_sources.is_empty() {
        cfg.registry_sources = default_config().registry_sources;
    }
    let mut cfg = normalize_config(cfg);
    if let Ok(release_sources) = crate::release_sources::trusted_release_sources(path) {
        cfg.registry_sources = release_sources.endpoint_registry_urls();
    }
    crate::endpoint::apply_trusted_endpoint_state(&mut cfg, path);
    if needs_migration {
        backup_config(path, &format!("v{version}"))?;
        write_config_to_path(path, &cfg)?;
    }
    Ok(cfg)
}

pub(crate) fn load_config_from_path_with_recovery(path: &PathBuf) -> Result<ClientConfig> {
    match load_config_from_path(path) {
        Ok(config) => Ok(config),
        Err(load_error) => {
            eprintln!(
                "[const-api][config] strict load failed; attempting field recovery: {load_error:#}"
            );
            backup_config(path, "recovery")?;
            let raw = fs::read(path)
                .with_context(|| format!("read config for recovery {}", path.display()))?;
            let recovered = serde_json::from_slice::<serde_json::Value>(&raw)
                .ok()
                .and_then(|value| recover_config_value(value).ok())
                .unwrap_or_else(|| {
                    eprintln!(
                        "[const-api][config] config JSON cannot be recovered; using safe defaults"
                    );
                    default_config()
                });
            write_config_to_path(path, &recovered).with_context(|| {
                format!("persist recovered config after strict load failure: {load_error:#}")
            })
        }
    }
}

fn backup_config(path: &PathBuf, label: &str) -> Result<PathBuf> {
    let mut sequence = 0_u32;
    let backup = loop {
        let suffix = if sequence == 0 {
            format!(".{label}.bak")
        } else {
            format!(".{label}.{sequence}.bak")
        };
        let candidate = PathBuf::from(format!("{}{suffix}", path.display()));
        if !candidate.exists() {
            break candidate;
        }
        sequence = sequence.saturating_add(1);
    };
    fs::copy(path, &backup).with_context(|| {
        format!(
            "backup config {} before migration or recovery to {}",
            path.display(),
            backup.display()
        )
    })?;
    Ok(backup)
}

fn recover_config_value(value: serde_json::Value) -> Result<ClientConfig> {
    let user = value
        .as_object()
        .context("recover config root must be a JSON object")?;
    let version = user
        .get("config_version")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_default() as u32;
    let channel_values = user
        .get("channels")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let supplier_value = user.get("supplier").cloned();

    let defaults = default_config();
    let default_channels = defaults.channels.clone();
    let mut accepted = serde_json::to_value(defaults)?
        .as_object()
        .cloned()
        .context("default config must serialize as a JSON object")?;
    accepted.insert(
        "config_version".to_string(),
        serde_json::json!(CURRENT_CLIENT_CONFIG_VERSION),
    );
    accepted.insert("channels".to_string(), serde_json::json!([]));

    for (field, field_value) in user {
        if matches!(field.as_str(), "config_version" | "channels" | "supplier") {
            continue;
        }
        let previous = accepted.insert(field.clone(), field_value.clone());
        if serde_json::from_value::<ClientConfig>(serde_json::Value::Object(accepted.clone()))
            .is_err()
        {
            eprintln!(
                "[const-api][config] ignored invalid top-level field during recovery: {field}"
            );
            match previous {
                Some(previous) => {
                    accepted.insert(field.clone(), previous);
                }
                None => {
                    accepted.remove(field);
                }
            }
        }
    }

    let mut recovered: ClientConfig = serde_json::from_value(serde_json::Value::Object(accepted))?;
    let mut channels = Vec::new();
    for (index, channel_value) in channel_values.into_iter().enumerate() {
        let channel = if version < CURRENT_CLIENT_CONFIG_VERSION {
            serde_json::from_value::<ChannelConfigV4Read>(channel_value)
                .map(|channel| migrate_channel_v4_at_load_boundary(channel, index))
        } else {
            serde_json::from_value::<crate::model::ChannelConfigV2>(channel_value).map(Into::into)
        };
        match channel {
            Ok(channel) => {
                let channel = normalize_channel(channel, index, None, None);
                match crate::channel_surface::validate_channel_v2(&channel)
                    .and_then(|()| crate::channel_surface::validate_channel_source_policy(&channel))
                {
                    Ok(()) => channels.push(channel),
                    Err(error) => eprintln!(
                        "[const-api][config] ignored invalid channel {} during recovery: {error:#}",
                        channel.id
                    ),
                }
            }
            Err(error) => eprintln!(
                "[const-api][config] ignored unreadable channel at index {index} during recovery: {error}"
            ),
        }
    }
    if channels.is_empty() && version < CURRENT_CLIENT_CONFIG_VERSION {
        if let Some(supplier_value) = supplier_value {
            if let Ok(supplier) = serde_json::from_value::<SupplierConfigV4Read>(supplier_value) {
                let channel = migrate_supplier_v4_channel_at_load_boundary("channel-1", &supplier);
                if crate::channel_surface::validate_channel_v2(&channel)
                    .and_then(|()| crate::channel_surface::validate_channel_source_policy(&channel))
                    .is_ok()
                {
                    channels.push(channel);
                }
            }
        }
    }
    recovered.channels = if channels.is_empty() {
        eprintln!("[const-api][config] no channel could be recovered; using the default channel");
        default_channels
    } else {
        channels
    };
    Ok(normalize_config(recovered))
}

fn migrate_client_config_v4(value: ClientConfigV4Read) -> ClientConfig {
    let supplier_channel = value
        .supplier
        .as_ref()
        .map(|supplier| migrate_supplier_v4_channel_at_load_boundary("channel-1", supplier));
    let supplier = value
        .supplier
        .map(SupplierConfig::from)
        .unwrap_or_else(default_supplier_config);
    let channels = if value.channels.is_empty() {
        vec![
            supplier_channel
                .unwrap_or_else(|| channel_from_supplier("channel-1".to_string(), &supplier)),
        ]
    } else {
        value
            .channels
            .into_iter()
            .enumerate()
            .map(|(index, channel)| migrate_channel_v4_at_load_boundary(channel, index))
            .collect()
    };
    ClientConfig {
        config_version: value.config_version,
        client_id: value.client_id,
        platform_id: value.platform_id,
        account_platform_id: value.account_platform_id,
        account_home_server_id: value.account_home_server_id,
        account_home_base_url: value.account_home_base_url,
        account_user_id: value.account_user_id,
        account_email: value.account_email,
        account_device_api_key: value.account_device_api_key,
        listen: value.listen,
        allow_lan_access: value.allow_lan_access,
        lan_share: value.lan_share,
        proxy_auto_start: value.proxy_auto_start,
        automatic_updates: value.automatic_updates,
        supplier_auto_start: value.supplier_auto_start,
        api_key: value.api_key,
        registry_sources: value.registry_sources,
        registry_signature_secret: value.registry_signature_secret,
        registry_public_keys: value.registry_public_keys,
        registry_version: value.registry_version,
        development_endpoint: default_development_endpoint(),
        allow_model_equivalence: value.allow_model_equivalence,
        prefer_local_supply: value.prefer_local_supply,
        allow_unverified_platform_routes: value.allow_unverified_platform_routes,
        model_aliases: value.model_aliases,
        model_compatibility_profiles: default_model_compatibility_profiles(),
        endpoints: value.endpoints,
        channels,
        supplier,
    }
}

pub(crate) fn write_config_to_path(path: &PathBuf, cfg: &ClientConfig) -> Result<ClientConfig> {
    let parent = path
        .parent()
        .with_context(|| format!("config path {} has no parent", path.display()))?;
    fs::create_dir_all(parent)?;
    let mut normalized = normalize_config(cfg.clone());
    crate::endpoint::apply_trusted_endpoint_state(&mut normalized, path);
    for channel in &normalized.channels {
        crate::channel_surface::validate_channel_v2(channel)
            .with_context(|| format!("validate channel {}", channel.id))?;
        crate::channel_surface::validate_channel_source_policy(channel)
            .with_context(|| format!("validate channel {} source policy", channel.id))?;
    }
    let raw = serde_json::to_vec_pretty(&normalized)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("create config temp file beside {}", path.display()))?;
    temp.write_all(&raw)
        .with_context(|| format!("write config temp file for {}", path.display()))?;
    temp.write_all(b"\n")?;
    temp.flush()?;
    temp.as_file().sync_all()?;
    temp.persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("replace config {}", path.display()))?;
    Ok(normalized)
}

/// Applies one short, in-process configuration transaction.
///
/// Network discovery must finish before entering this function. The live proxy
/// configuration is swapped only after the atomic disk write. A dedicated
/// commit lock serializes writers without holding up proxy requests during
/// filesystem sync.
pub(crate) fn commit_config_update<T>(
    path: &PathBuf,
    live_config: &Arc<StdMutex<ClientConfig>>,
    update: impl FnOnce(&mut ClientConfig) -> Result<T>,
) -> Result<(ClientConfig, T)> {
    let _commit = CONFIG_COMMIT_LOCK
        .get_or_init(|| StdMutex::new(()))
        .lock()
        .map_err(|_| anyhow::anyhow!("config commit lock poisoned"))?;
    let mut next = load_config_from_path(path)?;
    let result = update(&mut next)?;
    let authoritative = write_config_to_path(path, &next)?;
    let mut live = live_config
        .lock()
        .map_err(|_| anyhow::anyhow!("proxy live config lock poisoned"))?;
    *live = authoritative.clone();
    Ok((authoritative, result))
}

/// Reloads an externally edited configuration without allowing it to race a
/// runtime commit. Normal UI reads can use this as a cheap consistency barrier.
pub(crate) fn reload_config_authoritatively(
    path: &PathBuf,
    live_config: &Arc<StdMutex<ClientConfig>>,
) -> Result<ClientConfig> {
    let _commit = CONFIG_COMMIT_LOCK
        .get_or_init(|| StdMutex::new(()))
        .lock()
        .map_err(|_| anyhow::anyhow!("config commit lock poisoned"))?;
    let authoritative = load_config_from_path(path)?;
    let mut live = live_config
        .lock()
        .map_err(|_| anyhow::anyhow!("proxy live config lock poisoned"))?;
    *live = authoritative.clone();
    Ok(authoritative)
}

pub(crate) fn channel_configs_match(left: &ChannelConfig, right: &ChannelConfig) -> bool {
    match (serde_json::to_value(left), serde_json::to_value(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

/// Connection identity only: policy edits and observations do not invalidate checks.
pub(crate) fn channel_detection_inputs_match(left: &ChannelConfig, right: &ChannelConfig) -> bool {
    let contract = |channel: &ChannelConfig| {
        let mut value = channel.v2.clone();
        value.created_at_unix_ms = 0;
        value.max_concurrency = 0;
        value.quota_reserve_percent = 0;
        value
    };
    let bindings = |channel: &ChannelConfig| {
        let mut values = channel.surface_bindings.clone();
        for binding in &mut values {
            binding.verification = SurfaceVerification::default();
            for protocol in &mut binding.protocols {
                protocol.verification = ProtocolVerification::default();
            }
        }
        values
    };
    contract(left) == contract(right) && bindings(left) == bindings(right)
}

/// An editor may predate a background sample. Keep observations it did not edit,
/// but never transfer evidence across changed credentials/endpoints/protocols.
pub(crate) fn preserve_unedited_channel_observations(
    incoming: &mut ChannelConfig,
    baseline: &ChannelConfig,
    current: &ChannelConfig,
) {
    if !channel_detection_inputs_match(incoming, baseline)
        || !channel_detection_inputs_match(current, baseline)
    {
        return;
    }
    if incoming.models == baseline.models {
        incoming.models = current.models.clone();
    }
    if incoming.capability_profiles == baseline.capability_profiles {
        incoming.capability_profiles = current.capability_profiles.clone();
    }
    if incoming.detection_checks == baseline.detection_checks {
        incoming.detection_checks = current.detection_checks.clone();
    }
    for ((incoming, baseline), current) in incoming
        .surface_bindings
        .iter_mut()
        .zip(&baseline.surface_bindings)
        .zip(&current.surface_bindings)
    {
        if incoming.verification == baseline.verification {
            incoming.verification = current.verification.clone();
        }
        for ((incoming, baseline), current) in incoming
            .protocols
            .iter_mut()
            .zip(&baseline.protocols)
            .zip(&current.protocols)
        {
            if incoming.verification == baseline.verification {
                incoming.verification = current.verification.clone();
            }
        }
    }
}

pub(crate) fn normalize_config(mut cfg: ClientConfig) -> ClientConfig { let mut exported_config = (|| {
    // Retain the wire field for old clients, but the retired opt-in must not
    // suppress custom fallback after loading or saving an older configuration.
    cfg.allow_unverified_platform_routes = true;
    let original_version = cfg.config_version;
    if cfg.api_key.trim().is_empty() {
        cfg.api_key = generate_local_api_key();
    }
    if original_version < CURRENT_CLIENT_CONFIG_VERSION {
        if cfg
            .api_key
            .trim()
            .starts_with(LEGACY_PLATFORM_DEVICE_KEY_PREFIX)
        {
            cfg.api_key = generate_local_api_key();
        }
        if original_version < 4 {
            cfg.supplier.price_ratio = migrate_legacy_price_ratio(cfg.supplier.price_ratio);
            for channel in &mut cfg.channels {
                channel.price_ratio = migrate_legacy_price_ratio(channel.price_ratio);
            }
            cfg.prefer_local_supply = true;
        }
        cfg.config_version = CURRENT_CLIENT_CONFIG_VERSION;
    }
    if cfg.client_id.trim().is_empty() {
        cfg.client_id = machine_client_id();
    } else {
        cfg.client_id = normalize_identity(&cfg.client_id, "client");
    }
    if cfg.listen.trim().is_empty() {
        cfg.listen = default_local_proxy_listen().to_string();
    }
    if cfg.registry_sources.is_empty() {
        cfg.registry_sources = default_config().registry_sources;
    }
    cfg.development_endpoint = normalize_development_endpoint(&cfg.development_endpoint);
    cfg.model_aliases.clear();
    cfg.model_compatibility_profiles = cfg
        .model_compatibility_profiles
        .into_iter()
        .filter_map(|(key, mut profile)| {
            let key = key.trim().to_string();
            profile.platform_id = profile.platform_id.trim().to_string();
            profile.user_id = profile.user_id.trim().to_string();
            profile.base_release_id = profile.base_release_id.trim().to_string();
            profile.sync_status = profile.sync_status.trim().to_string();
            if key.is_empty() || profile.platform_id.is_empty() || profile.user_id.is_empty() {
                return None;
            }
            Some((key, profile))
        })
        .collect();
    let mut registry_sources = Vec::new();
    for source in cfg.registry_sources {
        let source = source.trim();
        if source.is_empty() || source == LEGACY_LOCAL_ENDPOINT_REGISTRY {
            continue;
        }
        let source = if source == LEGACY_PRIVATE_ENDPOINT_REGISTRY {
            LEGACY_PUBLISHED_ENDPOINT_REGISTRY
        } else {
            source
        };
        if !registry_sources.iter().any(|existing| existing == source) {
            registry_sources.push(source.to_string());
        }
    }
    // Versions before 0.1.24 stored GitHub as the only built-in source.
    // Upgrade that exact legacy default to the mirrored policy while leaving
    // explicit custom source lists untouched.
    let legacy_mirrored_default = registry_sources.len() == 2
        && registry_sources[0] == LEGACY_DOMESTIC_ENDPOINT_REGISTRY
        && registry_sources[1] == LEGACY_PUBLISHED_ENDPOINT_REGISTRY;
    if legacy_mirrored_default
        || (registry_sources.len() == 1
            && registry_sources[0] == LEGACY_PUBLISHED_ENDPOINT_REGISTRY)
    {
        registry_sources = default_endpoint_registry_sources();
    }
    cfg.registry_sources = registry_sources;
    cfg.registry_public_keys = cfg
        .registry_public_keys
        .into_iter()
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
        .collect();
    cfg.endpoints = merge_discovered_endpoints(cfg.endpoints);
    let fallback_ws = derive_supplier_ws_url(&cfg);
    let fallback_quic = derive_supplier_quic_url(&cfg);
    cfg.channels = cfg
        .channels
        .into_iter()
        .enumerate()
        .map(|(idx, channel)| {
            normalize_channel(
                channel,
                idx,
                fallback_ws.as_deref(),
                fallback_quic.as_deref(),
            )
        })
        .collect();
    cfg.lan_share.shared_models = dedupe_models(cfg.lan_share.shared_models);
    cfg.lan_share.preferred_connect_ip = cfg.lan_share.preferred_connect_ip.trim().to_string();
    // Ordinary channels retain the single-switch contract. A LAN-share
    // channel is the one explicit local-only exception and can never start the
    // platform supplier lifecycle.
    cfg.supplier_auto_start = cfg.channels.iter().any(|channel| {
        channel.enabled && crate::source_driver::channel_is_platform_shareable(channel)
    });
    // Channel order is the user-controlled priority for local routing. Health,
    // capability, and model filters may skip a channel, but normalization must
    // never silently reorder the remaining candidates.
    if let Some(compatibility_channel) = cfg
        .channels
        .iter()
        .find(|channel| crate::source_driver::channel_is_platform_shareable(channel))
        .or_else(|| cfg.channels.first())
    {
        cfg.supplier = supplier_from_channel(compatibility_channel);
    } else {
        cfg.supplier = default_supplier_config();
    }
    cfg
})(); crate::local_policy::enforce(&mut exported_config); exported_config }

fn migrate_legacy_price_ratio(value: f64) -> f64 {
    if value.is_finite() && value >= LEGACY_PRICE_RATIO_THRESHOLD {
        value / LEGACY_PRICE_RATIO_SCALE
    } else {
        value
    }
}

pub(crate) fn channel_from_supplier(id: String, supplier: &SupplierConfig) -> ChannelConfig {
    let mut subscription = supplier.subscription.clone();
    subscription.credential_ref = supplier.credential_ref.clone();
    ChannelConfig {
        v2: ChannelConfigV2Contract {
            created_at_unix_ms: 0,
            source_driver: supplier.source_driver,
            credential_ref: supplier.credential_ref.clone(),
            executor: supplier.executor.clone(),
            default_target: supplier.default_target.clone(),
            discovery: supplier.discovery.clone(),
            user_agent_profile: supplier.user_agent_profile.clone(),
            max_concurrency: supplier.max_concurrency,
            quota_reserve_percent: supplier.quota_reserve_percent.min(100),
        },
        id,
        enabled: supplier.enabled,
        share_enabled: supplier.enabled,
        kind: supplier.kind.clone(),
        api_format: supplier.api_format.clone(),
        node_id: supplier.node_id.clone(),
        name: supplier.name.clone(),
        server_ws_url: supplier.server_ws_url.clone(),
        server_quic_url: supplier.server_quic_url.clone(),
        upstream_base_url: supplier.upstream_base_url.clone(),
        upstream_api_key: supplier.upstream_api_key.clone(),
        public_model: supplier.public_model.clone(),
        upstream_model: supplier.upstream_model.clone(),
        models: supplier.models.clone(),
        supported_protocols: supplier.supported_protocols.clone(),
        capability_profiles: supplier.capability_profiles.clone(),
        detection_checks: supplier.detection_checks.clone(),
        surface_bindings: supplier.surface_bindings.clone(),
        price_ratio: supplier.price_ratio,
        subscription,
    }
}

fn migrate_supplier_v4_channel_at_load_boundary(
    id: &str,
    value: &SupplierConfigV4Read,
) -> ChannelConfig {
    migrate_channel_v4_at_load_boundary(
        ChannelConfigV4Read {
            source_driver: value.source_driver,
            credential_ref: value.credential_ref.clone(),
            default_target: value.default_target.clone(),
            discovery: value.discovery.clone(),
            user_agent_profile: value.user_agent_profile.clone(),
            id: id.to_string(),
            enabled: value.enabled,
            share_enabled: value.enabled,
            kind: value.kind.clone(),
            api_format: value.api_format.clone(),
            node_id: value.node_id.clone(),
            name: value.name.clone(),
            server_ws_url: value.server_ws_url.clone(),
            server_quic_url: value.server_quic_url.clone(),
            upstream_base_url: value.upstream_base_url.clone(),
            upstream_api_key: value.upstream_api_key.clone(),
            public_model: value.public_model.clone(),
            upstream_model: value.upstream_model.clone(),
            models: value.models.clone(),
            supported_protocols: value.supported_protocols.clone(),
            capability_profiles: value.capability_profiles.clone(),
            detection_checks: value.detection_checks.clone(),
            surface_bindings: value.surface_bindings.clone(),
            price_ratio: value.price_ratio,
            max_concurrency: value.max_concurrency,
            quota_reserve_percent: value.quota_reserve_percent,
            subscription: value.subscription.clone(),
        },
        0,
    )
}

fn migrate_channel_v4_at_load_boundary(value: ChannelConfigV4Read, _index: usize) -> ChannelConfig {
    let source_driver = value.source_driver.unwrap_or_else(|| {
        infer_source_driver_at_load_boundary(
            &value.kind,
            &value.name,
            &value.upstream_base_url,
            &value.subscription.platform,
        )
    });
    let mut v2 = channel_v2_contract_for_source(source_driver);
    v2.max_concurrency = value
        .max_concurrency
        .unwrap_or(value.subscription.max_concurrency);
    v2.quota_reserve_percent = value
        .quota_reserve_percent
        .unwrap_or(value.subscription.quota_reserve_percent)
        .min(100);
    v2.credential_ref = [
        value.credential_ref.as_str(),
        value.upstream_api_key.as_str(),
        value.subscription.credential_ref.as_str(),
    ]
    .into_iter()
    .find(|candidate| !candidate.trim().is_empty())
    .unwrap_or_default()
    .to_string();
    if let Some(discovery) = value.discovery {
        v2.discovery = discovery;
    }
    v2.user_agent_profile = value.user_agent_profile.trim().to_ascii_lowercase();

    let retained_subscription = matches!(
        v2.executor,
        ChannelExecutorLocator::RetainedSubscription { .. }
    );
    let has_existing_bindings = !value.surface_bindings.is_empty();
    let mut surface_bindings = if retained_subscription {
        // V4 projected the retained subscription's internal upstream URL as if it
        // were a configurable HTTP surface. V5 owns that transport in the retained
        // executor, so carrying the legacy URL forward creates an invalid hybrid.
        source_driver_surface_bindings(source_driver, "")
    } else {
        value.surface_bindings
    };
    if surface_bindings.is_empty() {
        surface_bindings = legacy_surface_bindings(
            source_driver,
            &value.upstream_base_url,
            &value.api_format,
            &value.supported_protocols,
        );
    }
    if surface_bindings.is_empty() {
        surface_bindings = source_driver_surface_bindings(source_driver, &value.upstream_base_url);
    }

    if !retained_subscription {
        v2.default_target = value.default_target.unwrap_or_else(|| {
            if has_existing_bindings {
                preferred_binding_target(&surface_bindings)
            } else {
                legacy_protocol_from_api_format(&value.api_format).and_then(|protocol| {
                    surface_bindings
                        .iter()
                        .find(|binding| {
                            binding
                                .protocols
                                .iter()
                                .any(|candidate| candidate.protocol == protocol.as_str())
                        })
                        .map(|binding| ChannelTarget {
                            surface: binding.surface,
                            protocol,
                        })
                })
            }
            .unwrap_or_else(|| v2.default_target.clone())
        });
    }

    let models = normalize_model_list(value.models, &value.public_model, &value.upstream_model);
    let mut subscription = value.subscription;
    subscription.credential_ref = v2.credential_ref.clone();
    subscription.max_concurrency = v2.max_concurrency;
    subscription.quota_reserve_percent = v2.quota_reserve_percent;

    ChannelConfig {
        v2,
        id: value.id,
        enabled: value.enabled,
        share_enabled: value.share_enabled,
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
        models,
        supported_protocols: value.supported_protocols,
        capability_profiles: value.capability_profiles,
        detection_checks: value.detection_checks,
        surface_bindings,
        price_ratio: value.price_ratio,
        subscription,
    }
}

fn preferred_binding_target(bindings: &[ChannelSurfaceBinding]) -> Option<ChannelTarget> {
    bindings.iter().find_map(|binding| {
        binding
            .protocols
            .iter()
            .find(|protocol| protocol.preferred)
            .or_else(|| binding.protocols.first())
            .and_then(|protocol| {
                crate::protocol::kind::ProtocolKind::parse(&protocol.protocol)
                    .ok()
                    .map(|protocol| ChannelTarget {
                        surface: binding.surface,
                        protocol,
                    })
            })
    })
}

fn legacy_surface_bindings(
    source_driver: crate::source_driver::SourceDriverId,
    base_url: &str,
    api_format: &str,
    supported_protocols: &[String],
) -> Vec<ChannelSurfaceBinding> {
    if base_url.trim().is_empty() {
        return Vec::new();
    }
    let mut protocols = normalize_supported_protocols(supported_protocols.to_vec(), api_format);
    let preferred = legacy_protocol_from_api_format(api_format);
    if let Some(preferred) = preferred {
        let preferred = preferred.as_str().to_string();
        if !protocols.iter().any(|protocol| protocol == &preferred) {
            protocols.push(preferred);
        }
    }
    let mut bindings: Vec<ChannelSurfaceBinding> = Vec::new();
    for protocol in protocols {
        let Ok(parsed) = crate::protocol::kind::ProtocolKind::parse(&protocol) else {
            continue;
        };
        let surface = match parsed {
            crate::protocol::kind::ProtocolKind::OpenAiChat
            | crate::protocol::kind::ProtocolKind::OpenAiResponses => {
                crate::surface::ApiSurface::OpenAi
            }
            crate::protocol::kind::ProtocolKind::AnthropicMessages => {
                crate::surface::ApiSurface::Anthropic
            }
            crate::protocol::kind::ProtocolKind::GeminiNative => crate::surface::ApiSurface::Gemini,
        };
        let index = if let Some(index) = bindings.iter().position(|item| item.surface == surface) {
            index
        } else {
            let (endpoint_profile, auth_scheme) = driver_surface_policy(source_driver, surface);
            bindings.push(ChannelSurfaceBinding {
                surface,
                base_url: base_url.trim().trim_end_matches('/').to_string(),
                endpoint_profile,
                auth_scheme,
                protocols: Vec::new(),
                operation_overrides: Vec::new(),
                verification: SurfaceVerification::default(),
            });
            bindings.len().saturating_sub(1)
        };
        if let Some(binding) = bindings.get_mut(index) {
            binding.protocols.push(ChannelProtocolBinding {
                protocol: parsed.as_str().to_string(),
                preferred: preferred == Some(parsed),
                verification: ProtocolVerification::default(),
            });
        }
    }
    for binding in &mut bindings {
        if !binding.protocols.iter().any(|protocol| protocol.preferred) {
            if let Some(protocol) = binding.protocols.first_mut() {
                protocol.preferred = true;
            }
        }
    }
    bindings
}

fn legacy_protocol_from_api_format(
    api_format: &str,
) -> Option<crate::protocol::kind::ProtocolKind> {
    normalize_supported_protocols(Vec::new(), api_format)
        .into_iter()
        .next()
        .and_then(|protocol| crate::protocol::kind::ProtocolKind::parse(&protocol).ok())
}

pub(crate) fn source_driver_surface_bindings(
    source_driver: crate::source_driver::SourceDriverId,
    legacy_base_url: &str,
) -> Vec<ChannelSurfaceBinding> {
    crate::source_driver::source_driver(source_driver)
        .map(|driver| {
            let retained_subscription =
                driver.execution_kind() != crate::source_driver::ExecutionKind::HttpSurface;
            driver
                .surfaces
                .iter()
                .map(|surface| ChannelSurfaceBinding {
                    surface: surface.surface,
                    base_url: if retained_subscription {
                        String::new()
                    } else if legacy_base_url.trim().is_empty() {
                        surface.base_url.clone().unwrap_or_default()
                    } else {
                        legacy_base_url.trim().trim_end_matches('/').to_string()
                    },
                    endpoint_profile: surface.endpoint_profile.clone(),
                    auth_scheme: surface.auth_scheme.clone(),
                    protocols: surface
                        .protocols
                        .iter()
                        .map(|protocol| ChannelProtocolBinding {
                            protocol: protocol.clone(),
                            preferred: protocol == &surface.preferred_protocol,
                            verification: ProtocolVerification::default(),
                        })
                        .collect(),
                    operation_overrides: Vec::new(),
                    verification: SurfaceVerification::default(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn driver_surface_policy(
    source_driver: crate::source_driver::SourceDriverId,
    surface: crate::surface::ApiSurface,
) -> (String, String) {
    crate::source_driver::source_driver(source_driver)
        .ok()
        .and_then(|driver| {
            driver
                .surfaces
                .iter()
                .find(|candidate| candidate.surface == surface)
        })
        .map(|surface| {
            (
                surface.endpoint_profile.clone(),
                surface.auth_scheme.clone(),
            )
        })
        .unwrap_or_else(|| match surface {
            crate::surface::ApiSurface::OpenAi => {
                ("openai_compatible".to_string(), "bearer".to_string())
            }
            crate::surface::ApiSurface::Anthropic => {
                ("anthropic".to_string(), "x_api_key".to_string())
            }
            crate::surface::ApiSurface::Gemini => {
                ("google_gemini".to_string(), "x_goog_api_key".to_string())
            }
        })
}

pub(crate) fn supplier_from_channel(channel: &ChannelConfig) -> SupplierConfig {
    let mut supplier = supplier_from_channel_unfiltered(channel);
    supplier.models = crate::supplier::availability::effective_models(channel, supplier.models);
    if supplier.models.is_empty() && !channel.models.is_empty() {
        supplier.public_model.clear();
        supplier.upstream_model.clear();
        supplier.registration_health_status = Some("blocked".into());
    } else if !supplier.models.is_empty() {
        supplier.public_model = channel.default_model_from(&supplier.models);
        supplier.upstream_model = supplier.public_model.clone();
    }
    supplier
}

// Exact checks must be able to verify a suspended target without advertising it.
pub(crate) fn supplier_from_channel_unfiltered(channel: &ChannelConfig) -> SupplierConfig {
    let driver = crate::source_driver::source_driver(channel.source_driver()).ok();
    let primary_model = channel.default_model_from(&channel.models);
    let mut subscription = channel.subscription.clone();
    subscription.credential_ref = channel.v2.credential_ref.clone();
    subscription.max_concurrency = channel.v2.max_concurrency;
    subscription.quota_reserve_percent = channel.v2.quota_reserve_percent.min(100);
    SupplierConfig {
        source_driver: channel.source_driver(),
        credential_ref: channel.v2.credential_ref.clone(),
        executor: channel.v2.executor.clone(),
        default_target: channel.v2.default_target.clone(),
        discovery: channel.v2.discovery.clone(),
        user_agent_profile: channel.v2.user_agent_profile.clone(),
        channel_id: channel.id.clone(),
        enabled: channel.enabled && crate::source_driver::channel_is_platform_shareable(channel),
        kind: driver
            .map(|driver| driver.kind.clone())
            .unwrap_or_else(|| channel.kind.clone()),
        api_format: channel.v2.default_target.protocol.as_str().to_string(),
        node_id: channel.node_id.clone(),
        name: channel.name.clone(),
        server_ws_url: channel.server_ws_url.clone(),
        server_quic_url: channel.server_quic_url.clone(),
        upstream_base_url: channel
            .surface_bindings
            .iter()
            .find(|binding| binding.surface == channel.v2.default_target.surface)
            .map(|binding| binding.base_url.clone())
            .unwrap_or_default(),
        upstream_api_key: channel.v2.credential_ref.clone(),
        public_model: primary_model.clone(),
        upstream_model: primary_model,
        models: channel.models.clone(),
        supported_protocols: crate::channel_surface::protocols_from_surface_bindings(channel),
        capability_profiles: channel.capability_profiles.clone(),
        detection_checks: channel.detection_checks.clone(),
        surface_bindings: channel.surface_bindings.clone(),
        price_ratio: channel.price_ratio,
        max_concurrency: channel.v2.max_concurrency,
        quota_reserve_percent: channel.v2.quota_reserve_percent.min(100),
        subscription,
        registration_removed: false,
        registration_health_status: None,
        registration_quota_status: None,
    }
}

pub(crate) fn supplier_from_channel_for_node(channel: &ChannelConfig) -> SupplierConfig {
    let mut supplier = supplier_from_channel(channel);
    supplier.enabled =
        channel.enabled && crate::source_driver::channel_is_platform_shareable(channel);
    supplier
}

pub(crate) fn normalize_channel(
    mut channel: ChannelConfig,
    index: usize,
    fallback_ws_url: Option<&str>,
    fallback_quic_url: Option<&str>,
) -> ChannelConfig {
    crate::coding_gateway::upgrade_legacy_zen_surfaces(&mut channel);
    crate::channel_surface::repair_channel_surface_contract(&mut channel);
    crate::openrouter::normalize_channel(&mut channel);
    // `enabled` remains canonical. `share_enabled` is a compatibility mirror,
    // except for the dedicated LAN-share driver which is always local-only.
    channel.share_enabled =
        channel.enabled && crate::source_driver::channel_is_platform_shareable(&channel);
    channel.models = dedupe_models(std::mem::take(&mut channel.models));
    channel.v2.user_agent_profile = channel.v2.user_agent_profile.trim().to_ascii_lowercase();
    if channel.id.trim().is_empty() {
        channel.id = format!("channel-{}", index + 1);
    } else {
        channel.id = channel.id.trim().to_string();
    }
    let source_driver = channel.source_driver();
    let subscription_driver = matches!(
        source_driver,
        crate::source_driver::SourceDriverId::OpenAiSubscription
            | crate::source_driver::SourceDriverId::ClaudeSubscription
            | crate::source_driver::SourceDriverId::GeminiSubscription
            | crate::source_driver::SourceDriverId::GrokSubscription
    );
    if subscription_driver {
        if source_driver == crate::source_driver::SourceDriverId::OpenAiSubscription {
            // Remove the recognizable artifacts produced by the former OpenAI-only policy: an
            // exact all-media rejection and a client-host file declaration. Neither describes
            // the semantic capability of the upstream model.
            channel.capability_profiles.retain(|profile| {
                let protocol = profile.protocol.trim() == "openai_responses";
                let layer = crate::model::normalized_capability_layer(&profile.capability_layer);
                let legacy_rejection = protocol
                    && layer == "driver"
                    && !profile.model_pattern.trim().is_empty()
                    && profile.verification_state.trim() == "driver_contract"
                    && profile.input_modalities_authoritative
                    && !(profile.vision
                        || profile.image_input
                        || profile.audio_input
                        || profile.video_input
                        || profile.file_input);
                let legacy_client_file = protocol
                    && layer == "client_host"
                    && profile.model_pattern.trim().is_empty()
                    && profile.file_input
                    && profile.verification_state.trim() == "declared";
                !legacy_rejection && !legacy_client_file
            });
        }

        // A subscription driver profile describes the protocol adapter, not every model behind
        // it. Strip old model-agnostic media claims uniformly for OpenAI, Claude, Gemini and Grok;
        // semantic media evidence must be model-scoped, so exact model profiles stay untouched.
        for profile in channel.capability_profiles.iter_mut().filter(|profile| {
            profile.model_pattern.trim().is_empty()
                && crate::model::normalized_capability_layer(&profile.capability_layer) == "driver"
        }) {
            profile.input_modalities_authoritative = false;
            profile.output_modalities_authoritative = false;
            profile.vision = false;
            profile.image_input = false;
            profile.image_output = false;
            profile.audio_input = false;
            profile.audio_output = false;
            profile.video_input = false;
            profile.video_output = false;
            profile.file_input = false;
            profile.file_output = false;
        }
    }
    for profile in &mut channel.capability_profiles {
        profile.model_pattern = profile.model_pattern.trim().to_string();
        profile.release_status =
            match profile.release_status.trim().to_ascii_lowercase().as_str() {
                "prepared" => "prepared",
                "experimental" => "experimental",
                "suspended" => "suspended",
                _ => "supported",
            }
            .to_string();
        // Older profiles used `false` for both "not probed" and "unsupported".
        // Absence of evidence must remain availability-first; only the explicit
        // marker introduced with the tri-state semantics may disable streaming.
        if !profile.stream_sse && !profile.stream_sse_unsupported {
            profile.stream_sse = true;
        }
    }
    if channel.node_id.trim().is_empty() || channel.node_id == "local-supplier" {
        channel.node_id = generated_channel_node_id(&channel.id);
    }
    if channel.name.trim().is_empty() {
        channel.name = if index == 0 {
            "Local Channel".to_string()
        } else {
            format!("Local Channel {}", index + 1)
        };
    }
    if channel.server_ws_url.trim().is_empty()
        || channel.server_ws_url == "ws://127.0.0.1:8080/supplier/ws"
    {
        if let Some(ws_url) = fallback_ws_url {
            channel.server_ws_url = ws_url.to_string();
        }
    }
    if channel.server_quic_url.trim().is_empty() {
        if let Some(quic_url) = fallback_quic_url {
            channel.server_quic_url = quic_url.to_string();
        }
    }
    if !channel.price_ratio.is_finite() {
        channel.price_ratio = default_price_ratio();
    } else if channel.price_ratio < 0.0 {
        channel.price_ratio = 0.0;
    } else if channel.price_ratio > MAX_PRICE_RATIO {
        channel.price_ratio = MAX_PRICE_RATIO;
    }
    channel.v2.quota_reserve_percent = channel.v2.quota_reserve_percent.min(100);
    // Old clients still read these values from subscription channels. Keep a
    // compatibility mirror, while runtime decisions use channel-wide fields.
    channel.subscription.max_concurrency = channel.v2.max_concurrency;
    channel.subscription.quota_reserve_percent = channel.v2.quota_reserve_percent;
    channel.subscription.rpm_limit = 0;
    channel.subscription.daily_request_limit = 0;
    channel.subscription.risk_note.clear();
    channel.subscription.audit_enabled = false;
    channel
}

pub(crate) fn project_channel_legacy_fields(mut channel: ChannelConfig) -> ChannelConfig {
    crate::openrouter::normalize_channel(&mut channel);
    channel.models = dedupe_models(std::mem::take(&mut channel.models));
    let default_model = channel.default_model_from(&channel.models);
    sort_models_by_short_name(&mut channel.models);
    let Ok(driver) = crate::source_driver::source_driver(channel.source_driver()) else {
        return channel;
    };
    channel.kind = driver.kind.clone();
    if let Some(provider) = driver.subscription_provider.as_deref() {
        channel.subscription.platform = provider.to_string();
    }
    channel.api_format = channel.v2.default_target.protocol.as_str().to_string();
    channel.upstream_base_url = channel
        .surface_bindings
        .iter()
        .find(|binding| binding.surface == channel.v2.default_target.surface)
        .map(|binding| binding.base_url.clone())
        .unwrap_or_default();
    channel.upstream_api_key = channel.v2.credential_ref.clone();
    channel.subscription.credential_ref = channel.v2.credential_ref.clone();
    channel.public_model = default_model.clone();
    channel.upstream_model = default_model;
    channel.supported_protocols = crate::channel_surface::protocols_from_surface_bindings(&channel);
    channel
}

// Temporary compatibility boundary for persisted channels created before source_driver existed.
pub(crate) fn infer_source_driver_at_load_boundary(
    kind: &str,
    name: &str,
    base_url: &str,
    subscription_provider: &str,
) -> crate::source_driver::SourceDriverId {
    use crate::source_driver::SourceDriverId;

    if kind.trim().eq_ignore_ascii_case("subscription_adapter") {
        return match subscription_provider.trim().to_ascii_lowercase().as_str() {
            "openai" | "codex" | "chatgpt" => SourceDriverId::OpenAiSubscription,
            "gemini" | "google" | "antigravity" => SourceDriverId::GeminiSubscription,
            "grok" | "xai" => SourceDriverId::GrokSubscription,
            _ => SourceDriverId::ClaudeSubscription,
        };
    }
    match kind.trim().to_ascii_lowercase().as_str() {
        "openai" => SourceDriverId::OpenAiApi,
        "anthropic" => SourceDriverId::AnthropicApi,
        "gemini" => SourceDriverId::GeminiApi,
        "azure" | "azure_openai" => SourceDriverId::AzureOpenAi,
        "aws_bedrock" => SourceDriverId::BedrockMantle,
        "openrouter" => SourceDriverId::Openrouter,
        "custom_endpoint" => SourceDriverId::CustomEndpoint,
        "local_model" => {
            let hint = format!("{name} {base_url}").to_ascii_lowercase();
            if hint.contains("ollama") || hint.contains("11434") {
                SourceDriverId::Ollama
            } else if hint.contains("lm studio")
                || hint.contains("lmstudio")
                || hint.contains("1234")
            {
                SourceDriverId::LmStudio
            } else if hint.contains("vllm") || hint.contains("8000") {
                SourceDriverId::Vllm
            } else {
                SourceDriverId::CustomEndpoint
            }
        }
        _ => SourceDriverId::CustomEndpoint,
    }
}

pub(crate) fn normalize_model_list(
    models: Vec<String>,
    public_model: &str,
    upstream_model: &str,
) -> Vec<String> {
    if !models.is_empty() {
        return dedupe_models(models);
    }
    dedupe_models(vec![public_model.to_string(), upstream_model.to_string()])
}

pub(crate) fn dedupe_models(models: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for model in models {
        let key = normalize_model_name(short_model_name(&model));
        if key.is_empty() || seen.contains(&key) {
            continue;
        }
        seen.insert(key);
        out.push(model);
    }
    out
}

pub(crate) fn short_model_name(model: &str) -> &str {
    let model = model.trim();
    model
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(model)
}

pub(crate) fn sort_models_by_short_name(models: &mut [String]) {
    models.sort_by_cached_key(|model| normalize_model_name(short_model_name(model)));
}

// Public catalog/tool IDs are not upstream wire IDs. Keep channel.models intact
// so a public short name can be resolved back to its first advertised spelling.
pub(crate) fn public_model_name(model: &str) -> String {
    without_context_hint(model)
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .trim()
        .to_lowercase()
}

pub(crate) fn model_name_matches(candidate: &str, requested: &str) -> bool {
    // Provider IDs are overwhelmingly ASCII. Avoid allocating lowercase copies
    // for every candidate on the request path; retain Unicode compatibility.
    if candidate.is_ascii() && requested.is_ascii() {
        let requested = without_context_hint(requested);
        let candidate = without_context_hint(candidate);
        return !requested.is_empty()
            && (candidate.eq_ignore_ascii_case(requested)
                || (!requested.contains('/')
                    && short_model_name(candidate)
                        .trim()
                        .eq_ignore_ascii_case(requested))
                || crate::tool_model_metadata::same_model_identity(
                    if requested.contains('/') {
                        candidate
                    } else {
                        short_model_name(candidate).trim()
                    },
                    requested,
                ));
    }
    let requested = normalize_model_name(without_context_hint(requested));
    !requested.is_empty()
        && (normalize_model_name(without_context_hint(candidate)) == requested
            || (!requested.contains('/') && public_model_name(candidate) == requested)
            || crate::tool_model_metadata::same_model_identity(
                if requested.contains('/') {
                    candidate
                } else {
                    short_model_name(candidate)
                },
                &requested,
            ))
}

// Preserve exact upstream IDs, including literal context variants, before
// falling back to a tool-only context alias. Equal matches keep source order.
pub(crate) fn resolve_model_name<'a>(models: &'a [String], requested: &str) -> Option<&'a str> {
    let requested = requested.trim();
    if requested.is_empty() {
        return None;
    }
    let short = !requested.contains('/');
    let mut alias = None;
    for model in models {
        let model = model.trim();
        let candidate = if short {
            short_model_name(model).trim()
        } else {
            model
        };
        let literal_matches = if candidate.is_ascii() && requested.is_ascii() {
            candidate.eq_ignore_ascii_case(requested)
        } else {
            normalize_model_name(candidate) == normalize_model_name(requested)
        };
        if literal_matches {
            return Some(model);
        }
        if alias.is_none() && model_name_matches(model, requested) {
            alias = Some(model);
        }
    }
    alias
}

pub(crate) fn without_context_hint(model: &str) -> &str {
    let model = model.trim();
    if model
        .get(model.len().saturating_sub(4)..)
        .is_some_and(|tail| tail.eq_ignore_ascii_case("[1m]"))
    {
        model[..model.len() - 4].trim_end()
    } else {
        model
    }
}

pub(crate) fn normalize_model_name(model: &str) -> String {
    model.trim().to_lowercase()
}

pub(crate) fn normalize_supported_protocols(
    protocols: Vec<String>,
    api_format: &str,
) -> Vec<String> {
    let mut candidates = protocols;
    if candidates.is_empty() {
        candidates = protocols_from_api_format(api_format);
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for protocol in candidates {
        let protocol = match protocol.trim() {
            "openai_responses" => "openai_responses",
            "openai_chat" | "gemini_openai" => "openai_chat",
            "anthropic_messages" => "anthropic_messages",
            "gemini_native" => "gemini_native",
            _ => continue,
        };
        if seen.insert(protocol.to_string()) {
            out.push(protocol.to_string());
        }
    }
    if out.is_empty() {
        out = protocols_from_api_format(api_format);
    }
    out
}

pub(crate) fn protocols_from_api_format(api_format: &str) -> Vec<String> {
    match api_format.trim() {
        "openai_responses" => vec!["openai_responses".to_string()],
        "openai_chat" | "gemini_openai" => vec!["openai_chat".to_string()],
        "anthropic_messages" => vec!["anthropic_messages".to_string()],
        "gemini_native" => vec!["gemini_native".to_string()],
        "openai_subscription" => vec!["openai_responses".to_string()],
        "gemini_subscription" => vec!["gemini_native".to_string()],
        "claude_subscription" => vec!["anthropic_messages".to_string()],
        _ => Vec::new(),
    }
}

pub(crate) fn is_azure_openai_base_url(base_url: &str) -> bool {
    let lower = base_url.trim().to_ascii_lowercase();
    lower.contains(".openai.azure.com") || lower.contains(".services.ai.azure.com")
}

pub(crate) fn is_azure_openai_channel(channel: &ChannelConfig) -> bool {
    matches!(channel.kind.trim(), "azure_openai" | "azure")
        || is_azure_openai_base_url(&channel.upstream_base_url)
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)] // Identity helpers below are shared by production and tests.
mod tests {
    use super::*;

    #[test]
    fn model_aliases_match_both_directions_and_keep_advertised_wire_ids() {
        let mut channel = default_config().channels.remove(0);
        channel.set_source_driver(crate::source_driver::SourceDriverId::CustomEndpoint);
        for (requested, wire) in [
            ("claude-opus-5.5", "claude-opus-5-5"),
            ("claude-opus-5-5", "anthropic/claude-opus-5.5"),
            ("claude-sonnet-5.5", "claude-sonnet-5-5"),
            ("anthropic/claude-sonnet-5.5", "claude-sonnet-5-5"),
            ("CLAUDE-SONNET-5.5[1m]", "claude-sonnet-5-5[1M]"),
            ("claude-mythos-5.1", "claude-mythos-5-1"),
        ] {
            channel.models = vec![wire.into()];
            assert!(crate::proxy::channel_supports_model(&channel, requested));
            assert_eq!(resolve_model_name(&channel.models, requested), Some(wire));
            assert_eq!(
                crate::proxy::channel_upstream_model_for_request(&channel, Some(requested)),
                wire
            );
            assert_eq!(
                crate::supplier::supplier_upstream_model_for_request(
                    &crate::supplier_from_channel(&channel),
                    Some(requested)
                ),
                wire
            );
            for enabled in [false, true] {
                let mut config = default_config();
                config.allow_model_equivalence = enabled;
                let alternatives =
                    crate::model_compatibility::model_compatibility_candidates(&config, requested);
                assert!(enabled || alternatives.is_empty());
                assert!(!alternatives.iter().any(|model| {
                    crate::tool_model_metadata::same_model_identity(model, requested)
                }));
            }
        }
        let names = vec!["claude-opus-5-5".into(), "claude-opus-5.5".into()];
        assert_eq!(
            resolve_model_name(&names, "claude-opus-5.5"),
            Some("claude-opus-5.5")
        );
        for distinct in [
            "claude-opus-5.5:free",
            "claude-opus-5.5-pro",
            "unknown/claude-opus-5.5",
            "claude-opus-5.6",
        ] {
            assert!(
                !model_name_matches("claude-opus-5-5", distinct),
                "{distinct}"
            );
        }
    }

    #[test]
    fn exact_context_variant_resolves_before_base_alias_on_both_execution_paths() {
        let mut channel = default_config().channels.remove(0);
        channel.set_source_driver(crate::source_driver::SourceDriverId::CustomEndpoint);
        channel.models = vec![
            "First/Mixed".into(),
            "First/Mixed[1M]".into(),
            "Second/Mixed".into(),
            "First/Mixed:free".into(),
        ];
        for (requested, expected) in [
            ("mixed", "First/Mixed"),
            ("mixed[1m]", "First/Mixed[1M]"),
            ("first/mixed[1m]", "First/Mixed[1M]"),
            ("second/mixed", "Second/Mixed"),
            ("mixed:free", "First/Mixed:free"),
        ] {
            assert_eq!(
                resolve_model_name(&channel.models, requested),
                Some(expected)
            );
            assert_eq!(
                crate::proxy::channel_upstream_model_for_request(&channel, Some(requested)),
                expected
            );
            assert_eq!(
                crate::supplier::supplier_upstream_model_for_request(
                    &crate::supplier_from_channel(&channel),
                    Some(requested)
                ),
                expected
            );
        }
        assert_eq!(resolve_model_name(&channel.models, "other/mixed"), None);
        assert_eq!(resolve_model_name(&channel.models, "mixed:extended"), None);
        assert_eq!(
            resolve_model_name(&channel.models[..1], "mixed[1m]"),
            Some("First/Mixed")
        );
        assert_eq!(
            resolve_model_name(&["Vendor/ Ä".into(), "Vendor/ Ä[1M]".into()], "ä[1m]"),
            Some("Vendor/ Ä[1M]")
        );
    }

    #[test]
    fn new_local_configs_receive_distinct_compact_api_keys() {
        let first = default_config().api_key;
        let second = default_config().api_key;

        assert_ne!(first, second);
        for key in [first, second] {
            assert_eq!(key.len(), 39);
            assert!(key.starts_with("sk-api-"));
            assert!(key[7..].bytes().all(|byte| byte.is_ascii_hexdigit()));
        }

        let mut blank = default_config();
        blank.api_key = "  ".to_string();
        let recovered = normalize_config(blank).api_key;
        assert!(recovered.starts_with("sk-api-"));
        assert_eq!(recovered.len(), 39);
    }

    #[test]
    fn development_profile_requires_an_explicit_debug_profile() {
        for value in [Some("dev"), Some("development"), Some(" Development ")] {
            assert!(development_profile_value(value));
        }
        for value in [None, Some(""), Some("production"), Some("debug")] {
            assert!(!development_profile_value(value));
        }
    }

    #[test]
    fn client_paths_and_port_are_profile_scoped_without_changing_production_defaults() {
        let home = PathBuf::from("user-home");
        let development_root = PathBuf::from("development-root");

        assert_eq!(
            client_data_root_from(home.clone(), None),
            home.join(".const-api")
        );
        assert_eq!(
            client_data_root_from(home, Some(development_root.clone())),
            development_root
        );
        assert_eq!(
            config_path_from(PathBuf::from("production-root"), None),
            PathBuf::from("production-root").join("client.json")
        );
        assert_eq!(
            config_path_from(
                PathBuf::from("ignored-root"),
                Some(PathBuf::from("explicit.json"))
            ),
            PathBuf::from("explicit.json")
        );
        assert_eq!(local_proxy_listen_for_profile(false), "127.0.0.1:38787");
        assert_eq!(local_proxy_listen_for_profile(true), "127.0.0.1:38788");
        assert_eq!(development_endpoint_for_profile(false), "auto");
        assert_eq!(development_endpoint_for_profile(true), "local-dev");
    }

    #[test]
    fn legacy_config_without_native_update_preference_keeps_updates_enabled() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("client.json");
        let mut value = serde_json::to_value(default_config()).expect("serialize config");
        value
            .as_object_mut()
            .expect("config object")
            .remove("automatic_updates");
        write_json(&path, &value);

        let loaded = load_config_from_path(&path).expect("load legacy config");
        assert!(loaded.automatic_updates);

        let mut disabled = loaded;
        disabled.automatic_updates = false;
        write_config_to_path(&path, &disabled).expect("persist native preference");
        assert!(
            !load_config_from_path(&path)
                .expect("reload native preference")
                .automatic_updates
        );
    }

    fn write_json(path: &std::path::Path, value: &serde_json::Value) {
        std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
    }

    fn persisted_json(path: &std::path::Path) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    fn legacy_v4_fixture() -> serde_json::Value {
        let cfg = default_config();
        let channel = &cfg.channels[0];
        let mut value = serde_json::to_value(&cfg).expect("serialize common config fields");
        value["config_version"] = serde_json::json!(4);
        value["channels"] = serde_json::json!([{
            "source_driver": channel.source_driver(),
            "id": channel.id,
            "enabled": channel.enabled,
            "share_enabled": channel.share_enabled,
            "kind": channel.kind,
            "api_format": channel.api_format,
            "node_id": channel.node_id,
            "name": channel.name,
            "server_ws_url": channel.server_ws_url,
            "server_quic_url": channel.server_quic_url,
            "upstream_base_url": channel.upstream_base_url,
            "upstream_api_key": channel.upstream_api_key,
            "public_model": channel.public_model,
            "upstream_model": channel.upstream_model,
            "models": channel.models,
            "supported_protocols": channel.supported_protocols,
            "capability_profiles": channel.capability_profiles,
            "detection_checks": channel.detection_checks,
            "surface_bindings": channel.surface_bindings,
            "price_ratio": channel.price_ratio,
            "subscription": channel.subscription,
        }]);
        value["supplier"] =
            serde_json::to_value(default_supplier_config()).expect("serialize V4 supplier");
        value
    }

    #[test]
    fn v5_writer_emits_only_v2_channel_truth_and_no_supplier() {
        let value = serde_json::to_value(default_config()).expect("serialize V5 config");
        let channel = value["channels"][0]
            .as_object()
            .expect("serialized V5 channel");

        assert_eq!(value["config_version"], 5);
        assert!(value.get("supplier").is_none());
        for required in [
            "source_driver",
            "credential_ref",
            "executor",
            "surfaces",
            "default_target",
            "discovery",
            "models",
        ] {
            assert!(
                channel.contains_key(required),
                "missing V5 field {required}"
            );
        }
        for legacy in [
            "kind",
            "api_format",
            "upstream_base_url",
            "upstream_api_key",
            "public_model",
            "upstream_model",
            "supported_protocols",
            "surface_bindings",
        ] {
            assert!(
                !channel.contains_key(legacy),
                "V5 writer revived legacy field {legacy}"
            );
        }
    }

    #[test]
    fn v5_load_accepts_and_rewrites_buggy_openai_surface_name() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("client.json");
        let mut value = serde_json::to_value(default_config()).expect("serialize V5 config");
        value["channels"][0]["surfaces"][0]["surface"] = serde_json::json!("openai");
        value["channels"][0]["default_target"]["surface"] = serde_json::json!("openai");
        write_json(&path, &value);

        let loaded = load_config_from_path(&path).expect("load buggy OpenAI surface name");
        write_config_to_path(&path, &loaded).expect("rewrite V5 config");

        let repaired = persisted_json(&path);
        assert_eq!(repaired["channels"][0]["surfaces"][0]["surface"], "open_ai");
        assert_eq!(
            repaired["channels"][0]["default_target"]["surface"],
            "open_ai"
        );
    }

    #[test]
    fn v5_load_repairs_preference_drift_without_dropping_the_channel() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("client.json");
        let mut value = serde_json::to_value(default_config()).expect("serialize V5 config");
        value["channels"][0]["surfaces"][0]["protocols"][0]["preferred"] = serde_json::json!(false);
        value["channels"][0]["default_target"] = serde_json::json!({
            "surface": "anthropic",
            "protocol": "anthropic_messages"
        });
        write_json(&path, &value);

        let loaded = load_config_from_path(&path).expect("repair V5 channel drift");
        let channel = &loaded.channels[0];

        assert_eq!(channel.surface_bindings.len(), 1);
        assert_eq!(
            channel.surface_bindings[0]
                .protocols
                .iter()
                .filter(|protocol| protocol.preferred)
                .count(),
            1
        );
        assert_eq!(
            channel.v2.default_target.surface,
            crate::surface::ApiSurface::OpenAi
        );
        assert_eq!(
            channel.api_format,
            channel.v2.default_target.protocol.as_str()
        );
        assert_eq!(
            channel.upstream_base_url,
            channel.surface_bindings[0].base_url
        );
        crate::channel_surface::validate_channel_v2(channel).expect("valid repaired channel");
    }

    #[test]
    fn v4_custom_projection_migrates_to_independent_surface_bindings() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("client.json");
        let mut value = legacy_v4_fixture();
        value["channels"][0]["source_driver"] = serde_json::json!("custom_endpoint");
        value["channels"][0]["kind"] = serde_json::json!("custom_endpoint");
        value["channels"][0]["api_format"] = serde_json::json!("anthropic_messages");
        value["channels"][0]["upstream_base_url"] =
            serde_json::json!("https://gateway.example.test/api/");
        value["channels"][0]["supported_protocols"] = serde_json::json!([
            "openai_chat",
            "anthropic_messages",
            "anthropic_messages",
            "gemini_native"
        ]);
        value["channels"][0]["surface_bindings"] = serde_json::json!([]);
        write_json(&path, &value);

        let loaded = load_config_from_path(&path).expect("migrate legacy custom channel");
        let channel = &loaded.channels[0];

        assert_eq!(loaded.config_version, CURRENT_CLIENT_CONFIG_VERSION);
        assert_eq!(
            channel.v2.default_target.surface,
            crate::surface::ApiSurface::Anthropic
        );
        assert_eq!(
            channel.v2.default_target.protocol,
            crate::protocol::kind::ProtocolKind::AnthropicMessages
        );
        assert_eq!(channel.surface_bindings.len(), 3);
        for binding in &channel.surface_bindings {
            assert_eq!(binding.base_url, "https://gateway.example.test/api");
            assert_eq!(
                binding
                    .protocols
                    .iter()
                    .filter(|protocol| protocol.preferred)
                    .count(),
                1
            );
        }
        assert_eq!(channel.api_format, "anthropic_messages");
        crate::channel_surface::validate_channel_v2(channel).expect("valid migrated channel");

        let persisted = persisted_json(&path);
        assert_eq!(
            persisted["channels"][0]["surfaces"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert!(persisted["channels"][0].get("api_format").is_none());
    }

    #[test]
    fn v5_requires_all_v2_channel_identity_and_target_fields() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("client.json");
        for required in [
            "source_driver",
            "credential_ref",
            "executor",
            "surfaces",
            "default_target",
        ] {
            let mut value = serde_json::to_value(default_config()).expect("serialize config");
            value["config_version"] = serde_json::json!(5);
            value["channels"][0]
                .as_object_mut()
                .unwrap()
                .remove(required);
            write_json(&path, &value);

            let error = load_config_from_path(&path).unwrap_err();
            assert!(
                error.to_string().contains(required),
                "missing V5 field {required} should be rejected: {error:#}"
            );
        }
    }

    #[test]
    fn recovery_preserves_valid_fields_and_defaults_only_the_invalid_field() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("client.json");
        let mut value = serde_json::to_value(default_config()).expect("serialize config");
        value["account_email"] = serde_json::json!("kept@example.test");
        value["allow_lan_access"] = serde_json::json!("not-a-boolean");
        write_json(&path, &value);

        let recovered = load_config_from_path_with_recovery(&path).expect("recover invalid field");

        assert_eq!(recovered.account_email, "kept@example.test");
        assert!(!recovered.allow_lan_access);
        assert_eq!(recovered.channels.len(), default_config().channels.len());
        assert!(PathBuf::from(format!("{}.recovery.bak", path.display())).exists());
        assert_eq!(persisted_json(&path)["config_version"], 5);
    }

    #[test]
    fn recovery_keeps_valid_channels_when_another_channel_is_unreadable() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("client.json");
        let mut value = serde_json::to_value(default_config()).expect("serialize config");
        let mut valid = value["channels"][0].clone();
        valid["id"] = serde_json::json!("channel-valid");
        let mut invalid = valid.clone();
        invalid.as_object_mut().unwrap().remove("source_driver");
        value["channels"] = serde_json::json!([invalid, valid]);
        write_json(&path, &value);

        let recovered = load_config_from_path_with_recovery(&path).expect("recover valid channel");

        assert_eq!(recovered.channels.len(), 1);
        assert_eq!(recovered.channels[0].id, "channel-valid");
    }

    #[test]
    fn v4_channels_win_over_supplier_and_supplier_seeds_only_when_empty() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("client.json");
        let mut value = legacy_v4_fixture();
        value["channels"][0]["name"] = serde_json::json!("Channel Wins");
        value["supplier"]["name"] = serde_json::json!("Supplier Seed");
        write_json(&path, &value);

        let loaded = load_config_from_path(&path).expect("migrate V4 channels");
        assert_eq!(loaded.channels.len(), 1);
        assert_eq!(loaded.channels[0].name, "Channel Wins");

        value["channels"] = serde_json::json!([]);
        write_json(&path, &value);
        let seeded = load_config_from_path(&path).expect("migrate V4 supplier");
        assert_eq!(seeded.channels.len(), 1);
        assert_eq!(seeded.channels[0].name, "Supplier Seed");
    }

    #[test]
    fn v4_subscription_migration_discards_obsolete_http_surface_base_url() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("client.json");
        let mut value = legacy_v4_fixture();
        let channel = value["channels"][0].as_object_mut().unwrap();
        channel.remove("source_driver");
        channel.remove("surface_bindings");
        channel.insert(
            "kind".to_string(),
            serde_json::json!("subscription_adapter"),
        );
        channel.insert(
            "api_format".to_string(),
            serde_json::json!("openai_responses"),
        );
        channel.insert(
            "upstream_base_url".to_string(),
            serde_json::json!("https://chatgpt.com/backend-api/codex"),
        );
        channel["subscription"]["platform"] = serde_json::json!("openai");
        write_json(&path, &value);

        let loaded = load_config_from_path(&path).expect("migrate V4 subscription");

        assert_eq!(
            loaded.channels[0].source_driver(),
            crate::source_driver::SourceDriverId::OpenAiSubscription
        );
        assert!(
            loaded.channels[0]
                .surface_bindings
                .iter()
                .all(|binding| binding.base_url.is_empty())
        );
        assert_eq!(persisted_json(&path)["config_version"], 5);
    }

    #[test]
    fn v4_binding_then_legacy_then_driver_default_priority_is_stable() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("client.json");
        let mut value = legacy_v4_fixture();
        {
            let channel = value["channels"][0].as_object_mut().unwrap();
            channel.remove("source_driver");
            channel.insert("kind".to_string(), serde_json::json!("openai"));
            channel.insert(
                "api_format".to_string(),
                serde_json::json!("openai_responses"),
            );
            channel.insert(
                "upstream_base_url".to_string(),
                serde_json::json!("https://legacy.example/v1"),
            );
            channel.insert(
                "surface_bindings".to_string(),
                serde_json::json!([{
                    "surface": "open_ai",
                    "base_url": "https://binding.example/v1",
                    "endpoint_profile": "openai_compatible",
                    "auth_scheme": "bearer",
                    "protocols": [{
                        "protocol": "openai_chat",
                        "preferred": true,
                        "verification": {"state": "", "checked_at_unix": 0, "summary": ""}
                    }],
                    "operation_overrides": [],
                    "verification": {"state": "", "checked_at_unix": 0, "summary": ""}
                }]),
            );
        }
        write_json(&path, &value);

        let loaded = load_config_from_path(&path).expect("migrate V4 conflict");
        assert_eq!(
            loaded.channels[0].surface_bindings[0].base_url,
            "https://binding.example/v1"
        );
        assert_eq!(
            loaded.channels[0].surface_bindings[0].protocols[0].protocol,
            "openai_chat"
        );
        assert_eq!(
            loaded.channels[0].v2.default_target.protocol,
            crate::protocol::kind::ProtocolKind::OpenAiChat
        );

        value["channels"][0]
            .as_object_mut()
            .unwrap()
            .remove("surface_bindings");
        write_json(&path, &value);
        let legacy = load_config_from_path(&path).expect("migrate legacy fields");
        assert_eq!(
            legacy.channels[0].surface_bindings[0].base_url,
            "https://legacy.example/v1"
        );
        assert_eq!(
            legacy.channels[0].surface_bindings[0]
                .protocols
                .iter()
                .find(|protocol| protocol.preferred)
                .map(|protocol| protocol.protocol.as_str()),
            Some("openai_responses")
        );
        assert_eq!(
            legacy.channels[0].v2.default_target.protocol,
            crate::protocol::kind::ProtocolKind::OpenAiResponses
        );
    }

    #[test]
    fn v4_valid_api_format_is_added_when_stale_supported_protocols_omit_it() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("client.json");
        let mut value = legacy_v4_fixture();
        let channel = value["channels"][0].as_object_mut().unwrap();
        channel.insert("source_driver".to_string(), serde_json::json!("openai_api"));
        channel.insert(
            "api_format".to_string(),
            serde_json::json!("openai_responses"),
        );
        channel.insert(
            "supported_protocols".to_string(),
            serde_json::json!(["openai_chat"]),
        );
        channel.remove("surface_bindings");
        write_json(&path, &value);

        let loaded = load_config_from_path(&path).expect("migrate stale V4 protocols");
        let binding = &loaded.channels[0].surface_bindings[0];

        assert_eq!(
            binding
                .protocols
                .iter()
                .map(|protocol| protocol.protocol.as_str())
                .collect::<Vec<_>>(),
            vec!["openai_chat", "openai_responses"]
        );
        assert_eq!(
            binding
                .protocols
                .iter()
                .find(|protocol| protocol.preferred)
                .map(|protocol| protocol.protocol.as_str()),
            Some("openai_responses")
        );
        assert_eq!(
            loaded.channels[0].v2.default_target.protocol,
            crate::protocol::kind::ProtocolKind::OpenAiResponses
        );
    }

    #[test]
    fn model_dedup_preserves_first_exact_spelling_and_order() {
        let models = dedupe_models(vec![
            "  GPT-Exact  ".to_string(),
            "gpt-exact".to_string(),
            "Model-B".to_string(),
            "MODEL-B".to_string(),
            "模型-C".to_string(),
        ]);

        assert_eq!(models, vec!["  GPT-Exact  ", "Model-B", "模型-C"]);
    }

    #[test]
    fn model_short_name_dedup_keeps_first_wire_id_and_variants() {
        let models = dedupe_models(vec![
            "qwen/qwen3.8-27b".into(),
            "qwen3.8-27b".into(),
            "a/model".into(),
            "b/MODEL".into(),
            "a/model:free".into(),
            "a/model[1m]".into(),
            "a/model:batch".into(),
        ]);
        assert_eq!(
            models,
            vec![
                "qwen/qwen3.8-27b",
                "a/model",
                "a/model:free",
                "a/model[1m]",
                "a/model:batch"
            ]
        );
        let mut channel =
            crate::channel_from_supplier("short-names".into(), &default_supplier_config());
        channel.upstream_model = "b/MODEL".into();
        assert_eq!(channel.default_model_from(&models), "a/model");
        assert_eq!(
            crate::proxy::channel_upstream_model_for_request(&channel, Some(&models[0])),
            "qwen/qwen3.8-27b"
        );
    }

    #[test]
    fn saved_channel_models_sort_short_names_but_keep_full_case_sensitive_ids() {
        let mut channel =
            crate::channel_from_supplier("short-names".into(), &default_supplier_config());
        channel.models = vec![
            "z/Qwen3.8-27B".into(),
            "first/Beta".into(),
            "second/beta".into(),
            "v/alpha".into(),
        ];
        channel.upstream_model = "second/beta".into();
        let stored = crate::model::ChannelConfigV2::from(channel);
        assert_eq!(
            stored.models,
            vec!["v/alpha", "first/Beta", "z/Qwen3.8-27B"]
        );
        assert_eq!(stored.default_model, "first/Beta");
        let restored: ChannelConfig = stored.into();
        assert_eq!(restored.upstream_model, "first/Beta");
        assert_eq!(
            crate::proxy::channel_upstream_model_for_request(&restored, Some("z/Qwen3.8-27B")),
            "z/Qwen3.8-27B"
        );
    }

    #[test]
    fn normalization_preserves_user_local_channel_priority() {
        let mut cfg = default_config();
        let mut first = cfg.channels[0].clone();
        first.id = "first-disabled-z".to_string();
        first.name = "Z first".to_string();
        first.enabled = false;
        let mut second = first.clone();
        second.id = "second-enabled-a".to_string();
        second.name = "A second".to_string();
        second.enabled = true;
        cfg.channels = vec![first, second];

        let normalized = normalize_config(cfg);

        assert_eq!(
            normalized
                .channels
                .iter()
                .map(|channel| channel.id.as_str())
                .collect::<Vec<_>>(),
            vec!["first-disabled-z", "second-enabled-a"]
        );
    }

    #[test]
    fn legacy_runtime_projections_cannot_change_v5_truth_and_follow_its_surface_projection() {
        let mut channel = default_config().channels.remove(0);
        channel.surface_bindings[0]
            .protocols
            .push(ChannelProtocolBinding {
                protocol: "openai_responses".to_string(),
                preferred: false,
                verification: ProtocolVerification::default(),
            });
        let binding = channel.surface_bindings.clone();
        let default_target = channel.v2.default_target.clone();

        channel.kind = "anthropic".to_string();
        channel.api_format = "anthropic_messages".to_string();
        channel.upstream_base_url = "https://legacy.invalid".to_string();
        channel.upstream_api_key = "legacy-key".to_string();
        channel.supported_protocols = vec!["anthropic_messages".to_string()];
        channel.public_model = "legacy-model".to_string();
        channel.upstream_model = "legacy-model".to_string();
        channel.subscription.credential_ref = "legacy-subscription-key".to_string();
        channel.v2.credential_ref = "credential://v2".to_string();
        let unrelated_legacy_fields = (
            channel.kind.clone(),
            channel.upstream_api_key.clone(),
            channel.public_model.clone(),
            channel.upstream_model.clone(),
            channel.subscription.credential_ref.clone(),
        );
        let normalized = normalize_channel(channel, 0, None, None);

        assert_eq!(normalized.surface_bindings, binding);
        assert_eq!(normalized.v2.default_target, default_target);
        assert_eq!(normalized.v2.credential_ref, "credential://v2");
        assert_eq!(
            normalized.api_format,
            normalized.v2.default_target.protocol.as_str()
        );
        assert_eq!(
            normalized.upstream_base_url,
            normalized.surface_bindings[0].base_url
        );
        assert_eq!(
            normalized.supported_protocols,
            vec!["openai_chat".to_string(), "openai_responses".to_string()]
        );
        assert_eq!(
            (
                normalized.kind,
                normalized.upstream_api_key,
                normalized.public_model,
                normalized.upstream_model,
                normalized.subscription.credential_ref,
            ),
            unrelated_legacy_fields
        );
    }

    #[test]
    fn second_v5_save_is_identical_and_does_not_revive_legacy_fields() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("client.json");
        let cfg = default_config();

        write_config_to_path(&path, &cfg).expect("first V5 save");
        let first = persisted_json(&path);
        let loaded = load_config_from_path(&path).expect("reload V5");
        write_config_to_path(&path, &loaded).expect("second V5 save");
        let second = persisted_json(&path);

        assert_eq!(second, first);
        assert!(second.get("supplier").is_none());
        assert!(second["channels"][0].get("kind").is_none());
        assert!(second["channels"][0].get("api_format").is_none());
    }

    #[test]
    fn every_manifest_driver_constructs_valid_v5_and_round_trips_stably() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("client.json");
        let mut cfg = default_config();
        cfg.channels = crate::source_driver::ALL_SOURCE_DRIVER_IDS
            .into_iter()
            .enumerate()
            .map(|(index, source_driver)| {
                let driver = crate::source_driver::source_driver(source_driver).unwrap();
                let mut channel =
                    channel_from_supplier(format!("manifest-{index}"), &default_supplier_config());
                channel.v2 = channel_v2_contract_for_source(source_driver);
                channel.v2.credential_ref =
                    if source_driver == crate::source_driver::SourceDriverId::MinimaxTokenPlan {
                        "sk-cp-test-key".to_string()
                    } else {
                        format!("credential://{}", index + 1)
                    };
                channel.surface_bindings = source_driver_surface_bindings(source_driver, "");
                channel.models = vec![format!("model-{index}")];
                if let Some(provider) = driver.subscription_provider.as_deref() {
                    channel.subscription.platform = provider.to_string();
                }
                crate::channel_surface::validate_channel_v2(&channel)
                    .unwrap_or_else(|error| panic!("driver {:?}: {error:#}", source_driver));
                channel
            })
            .collect();

        write_config_to_path(&path, &cfg).expect("write all manifest drivers");
        let first = persisted_json(&path);
        assert_eq!(
            first["channels"].as_array().unwrap().len(),
            crate::source_driver::ALL_SOURCE_DRIVER_IDS.len()
        );
        for channel in first["channels"].as_array().unwrap() {
            assert!(channel.get("executor").is_some());
            assert!(channel.get("kind").is_none());
            assert!(channel.get("supported_protocols").is_none());
        }

        let loaded = load_config_from_path(&path).expect("reload all manifest drivers");
        write_config_to_path(&path, &loaded).expect("rewrite all manifest drivers");
        assert_eq!(persisted_json(&path), first);
    }

    fn assert_v5_write_rejected(mut cfg: ClientConfig, expected: &str) {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("client.json");
        cfg.config_version = CURRENT_CLIENT_CONFIG_VERSION;
        let error = write_config_to_path(&path, &cfg).unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message
                .to_ascii_lowercase()
                .contains(&expected.to_ascii_lowercase()),
            "expected {expected:?}, got {error:#}"
        );
    }

    #[test]
    fn v5_write_rejects_duplicate_surfaces_and_repairs_duplicate_protocols() {
        let mut duplicate_surface = default_config();
        let binding = duplicate_surface.channels[0].surface_bindings[0].clone();
        duplicate_surface.channels[0].surface_bindings.push(binding);
        assert_v5_write_rejected(duplicate_surface, "duplicate surface");

        let mut duplicate_protocol = default_config();
        let mut protocol = duplicate_protocol.channels[0].surface_bindings[0].protocols[0].clone();
        protocol.preferred = false;
        duplicate_protocol.channels[0].surface_bindings[0]
            .protocols
            .push(protocol);
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("client.json");
        let written = write_config_to_path(&path, &duplicate_protocol)
            .expect("repair duplicate protocol binding");
        assert_eq!(written.channels[0].surface_bindings[0].protocols.len(), 1);
    }

    #[test]
    fn v5_write_rejects_empty_url_and_repairs_preferred_contracts() {
        let mut empty_url = default_config();
        empty_url.channels[0].surface_bindings[0].base_url.clear();
        assert_v5_write_rejected(empty_url, "base URL");

        let mut missing_preferred = default_config();
        missing_preferred.channels[0].surface_bindings[0].protocols[0].preferred = false;
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("missing-preferred.json");
        let missing = write_config_to_path(&path, &missing_preferred)
            .expect("repair missing preferred protocol");
        assert_eq!(
            missing.channels[0].surface_bindings[0]
                .protocols
                .iter()
                .filter(|protocol| protocol.preferred)
                .count(),
            1
        );

        let mut duplicate_preferred = default_config();
        duplicate_preferred.channels[0].surface_bindings[0]
            .protocols
            .push(ChannelProtocolBinding {
                protocol: "openai_responses".to_string(),
                preferred: true,
                verification: ProtocolVerification::default(),
            });
        let path = temp.path().join("duplicate-preferred.json");
        let duplicate = write_config_to_path(&path, &duplicate_preferred)
            .expect("repair duplicate preferred protocols");
        let protocols = &duplicate.channels[0].surface_bindings[0].protocols;
        assert!(
            protocols
                .iter()
                .any(|protocol| protocol.protocol == "openai_chat" && protocol.preferred)
        );
        assert!(
            protocols
                .iter()
                .any(|protocol| protocol.protocol == "openai_responses" && !protocol.preferred)
        );
    }

    #[test]
    fn v5_write_rejects_invalid_protocol_and_repairs_dangling_default_target() {
        let mut invalid_protocol = default_config();
        invalid_protocol.channels[0].surface_bindings[0].protocols[0].protocol =
            "not_a_protocol".to_string();
        assert_v5_write_rejected(invalid_protocol, "invalid protocol");

        let mut dangling = default_config();
        dangling.channels[0].v2.default_target = ChannelTarget {
            surface: crate::surface::ApiSurface::Anthropic,
            protocol: crate::protocol::kind::ProtocolKind::AnthropicMessages,
        };
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("client.json");
        let written =
            write_config_to_path(&path, &dangling).expect("repair dangling default target");
        assert_eq!(
            written.channels[0].v2.default_target,
            ChannelTarget {
                surface: crate::surface::ApiSurface::OpenAi,
                protocol: crate::protocol::kind::ProtocolKind::OpenAiChat,
            }
        );
    }

    #[test]
    fn surface_preferred_and_channel_default_target_round_trip_independently() {
        let mut cfg = default_config();
        let channel = &mut cfg.channels[0];
        channel.surface_bindings[0]
            .protocols
            .push(ChannelProtocolBinding {
                protocol: "openai_responses".to_string(),
                preferred: false,
                verification: ProtocolVerification::default(),
            });
        channel.v2.default_target.protocol = crate::protocol::kind::ProtocolKind::OpenAiResponses;
        channel.models = vec![
            "GPT-Exact".to_string(),
            "gpt-exact".to_string(),
            "模型-A".to_string(),
        ];
        let original_preferred = channel.surface_bindings[0].protocols.clone();
        let original_target = channel.v2.default_target.clone();
        let original_models = dedupe_models(channel.models.clone());
        let encoded = serde_json::to_vec(&cfg).expect("serialize V5");
        let decoded: ClientConfig = serde_json::from_slice(&encoded).expect("deserialize V5");

        assert_eq!(
            decoded.channels[0].surface_bindings[0].protocols,
            original_preferred
        );
        assert_eq!(decoded.channels[0].v2.default_target, original_target);
        assert_eq!(decoded.channels[0].models, original_models);
    }

    #[test]
    fn v5_round_trip_preserves_default_model_independently_from_catalog_order() {
        let mut cfg = default_config();
        cfg.channels[0].models = vec!["a-first-model".to_string(), "chosen-model".to_string()];
        cfg.channels[0].upstream_model = "chosen-model".to_string();
        cfg.channels[0].public_model = "chosen-model".to_string();

        let encoded = serde_json::to_vec(&cfg).expect("serialize V5");
        let decoded: ClientConfig = serde_json::from_slice(&encoded).expect("deserialize V5");

        assert_eq!(decoded.channels[0].upstream_model, "chosen-model");
        assert_eq!(decoded.channels[0].public_model, "chosen-model");
        assert_eq!(decoded.channels[0].models[0], "a-first-model");
    }

    #[test]
    fn v5_round_trip_preserves_the_optional_user_agent_profile() {
        let mut cfg = default_config();
        cfg.channels[0].v2.user_agent_profile = "opencode".to_string();

        let encoded = serde_json::to_vec(&cfg).expect("serialize V5");
        let decoded: ClientConfig = serde_json::from_slice(&encoded).expect("deserialize V5");

        assert_eq!(decoded.channels[0].v2.user_agent_profile, "opencode");
        assert_eq!(
            supplier_from_channel(&decoded.channels[0]).user_agent_profile,
            "opencode"
        );
    }

    #[test]
    fn channel_projection_and_supplier_use_the_configured_default_model() {
        let mut channel = default_config().channels.remove(0);
        channel.models = vec!["a-first-model".to_string(), "chosen-model".to_string()];
        channel.upstream_model = "chosen-model".to_string();
        channel.public_model = "chosen-model".to_string();

        let projected = project_channel_legacy_fields(channel);
        let supplier = supplier_from_channel(&projected);

        assert_eq!(projected.upstream_model, "chosen-model");
        assert_eq!(projected.public_model, "chosen-model");
        assert_eq!(supplier.upstream_model, "chosen-model");
        assert_eq!(supplier.public_model, "chosen-model");
        assert_eq!(supplier.models[0], "a-first-model");
    }

    #[test]
    fn default_channels_use_safe_shared_limits() {
        let cfg = default_config();
        let channel = cfg.channels.first().expect("default channel");

        assert!(!cfg.allow_lan_access);
        assert_eq!(channel.max_concurrency(), DEFAULT_SUPPLIER_MAX_CONCURRENCY);
        assert!(channel.subscription.responses_ws_pool_enabled);
        assert!(
            !channel
                .subscription
                .responses_ws_subscription_multiplex_probe_enabled
        );
        assert_eq!(channel.subscription.rpm_limit, 0);
        assert_eq!(channel.quota_reserve_percent(), 0);
        assert_eq!(channel.subscription.daily_request_limit, 0);
    }

    #[test]
    fn development_endpoint_preference_accepts_ids_and_rejects_urls() {
        assert_eq!(normalize_development_endpoint(" cn-primary "), "cn-primary");
        assert_eq!(normalize_development_endpoint("LOCAL_dev"), "LOCAL_dev");
        assert_eq!(
            normalize_development_endpoint("https://api.example.com"),
            DEVELOPMENT_ENDPOINT_AUTO
        );
        assert_eq!(
            normalize_development_endpoint(""),
            DEVELOPMENT_ENDPOINT_AUTO
        );
    }

    #[test]
    fn normalization_collapses_legacy_channel_switches_to_enabled() {
        let mut cfg = default_config();
        cfg.channels[0].enabled = true;
        cfg.channels[0].share_enabled = false;
        cfg.supplier_auto_start = false;

        let normalized = normalize_config(cfg);

        assert!(normalized.channels[0].enabled);
        assert!(normalized.channels[0].share_enabled);
        assert!(normalized.supplier_auto_start);
        assert!(normalized.supplier.enabled);

        let mut cfg = normalized;
        cfg.channels[0].enabled = false;
        cfg.channels[0].share_enabled = true;
        cfg.supplier_auto_start = true;

        let normalized = normalize_config(cfg);

        assert!(!normalized.channels[0].enabled);
        assert!(!normalized.channels[0].share_enabled);
        assert!(!normalized.supplier_auto_start);
        assert!(!normalized.supplier.enabled);
    }

    #[test]
    fn normalization_keeps_lan_share_enabled_but_out_of_platform_supply() {
        let mut cfg = default_config();
        let channel = &mut cfg.channels[0];
        channel.enabled = true;
        channel.share_enabled = true;
        channel.set_source_driver(crate::source_driver::SourceDriverId::LanShare);
        channel.kind = "lan_share".to_string();
        channel.upstream_base_url = "http://192.168.1.20:38787/v1".to_string();
        channel.surface_bindings = crate::source_driver_surface_bindings(
            crate::source_driver::SourceDriverId::LanShare,
            &channel.upstream_base_url,
        );
        cfg.supplier_auto_start = true;

        let normalized = normalize_config(cfg);

        assert!(normalized.channels[0].enabled);
        assert!(!normalized.channels[0].share_enabled);
        assert!(!normalized.supplier_auto_start);
        assert!(!normalized.supplier.enabled);
    }

    #[test]
    fn normalize_channel_preserves_active_api_limits_and_retires_legacy_controls() {
        let mut channel =
            channel_from_supplier("channel-explicit".to_string(), &default_supplier_config());
        channel.kind = "openai_compatible".to_string();
        channel.v2.max_concurrency = 8;
        channel.subscription.responses_ws_pool_enabled = false;
        channel
            .subscription
            .responses_ws_subscription_multiplex_probe_enabled = false;
        channel.subscription.rpm_limit = 120;
        channel.v2.quota_reserve_percent = 125;
        channel.subscription.daily_request_limit = 5000;
        channel.subscription.risk_note = "legacy risk note".to_string();
        channel.subscription.audit_enabled = true;

        let channel = normalize_channel(channel, 0, None, None);

        assert_eq!(channel.max_concurrency(), 8);
        assert!(!channel.subscription.responses_ws_pool_enabled);
        assert!(
            !channel
                .subscription
                .responses_ws_subscription_multiplex_probe_enabled
        );
        assert_eq!(channel.subscription.rpm_limit, 0);
        assert_eq!(channel.quota_reserve_percent(), 100);
        assert_eq!(channel.subscription.daily_request_limit, 0);
        assert!(channel.subscription.risk_note.is_empty());
        assert!(!channel.subscription.audit_enabled);

        let encoded = serde_json::to_value(&channel).expect("serialize API channel limits");
        assert_eq!(encoded["max_concurrency"], 8);
        assert_eq!(encoded["quota_reserve_percent"], 100);
        assert!(encoded.get("subscription").is_none());
        let decoded: ChannelConfig =
            serde_json::from_value(encoded).expect("reload API channel limits");
        assert_eq!(decoded.max_concurrency(), 8);
        assert_eq!(decoded.quota_reserve_percent(), 100);
    }

    #[test]
    fn v5_reads_legacy_nested_limits_when_channel_fields_are_missing() {
        let channel = default_config().channels.remove(0);
        let mut encoded = serde_json::to_value(&channel).expect("serialize channel");
        let object = encoded.as_object_mut().expect("channel object");
        object.remove("max_concurrency");
        object.remove("quota_reserve_percent");
        let mut legacy_subscription = serde_json::to_value(default_subscription_adapter_config())
            .expect("serialize legacy subscription limits");
        legacy_subscription["max_concurrency"] = serde_json::json!(6);
        legacy_subscription["quota_reserve_percent"] = serde_json::json!(12);
        object.insert("subscription".to_string(), legacy_subscription);

        let decoded: ChannelConfig =
            serde_json::from_value(encoded).expect("reload legacy channel limits");

        assert_eq!(decoded.max_concurrency(), 6);
        assert_eq!(decoded.quota_reserve_percent(), 12);
        assert_eq!(decoded.subscription.max_concurrency, 6);
        assert_eq!(decoded.subscription.quota_reserve_percent, 12);
    }

    #[test]
    fn normalization_preserves_distinct_local_source_driver_identities() {
        let cases = [
            crate::source_driver::SourceDriverId::Ollama,
            crate::source_driver::SourceDriverId::LmStudio,
            crate::source_driver::SourceDriverId::Vllm,
        ];

        for (index, source_driver) in cases.into_iter().enumerate() {
            let mut channel =
                channel_from_supplier(format!("local-{index}"), &default_supplier_config());
            channel.kind = "local_model".to_string();
            channel.set_source_driver(source_driver);
            channel.surface_bindings =
                source_driver_surface_bindings(source_driver, &channel.upstream_base_url);

            let normalized = normalize_channel(channel, index, None, None);
            let encoded = serde_json::to_string(&normalized).expect("serialize channel");
            let encoded_value: serde_json::Value =
                serde_json::from_str(&encoded).expect("channel JSON value");
            let decoded: ChannelConfig =
                serde_json::from_str(&encoded).expect("deserialize channel");
            let supplier = supplier_from_channel(&normalized);
            let supplier_value = serde_json::to_value(&supplier).expect("serialize supplier");
            let expected_executor = crate::source_driver::source_driver(source_driver)
                .unwrap()
                .executor
                .clone();

            assert_eq!(decoded.source_driver(), source_driver);
            assert_eq!(
                encoded_value.get("source_driver"),
                Some(&serde_json::to_value(source_driver).unwrap())
            );
            assert!(encoded_value["subscription"].get("source_driver").is_none());
            assert_eq!(
                supplier_value.get("source_driver"),
                Some(&serde_json::to_value(source_driver).unwrap())
            );
            assert_eq!(supplier.source_driver, source_driver);
            assert_eq!(supplier.executor, expected_executor);
            assert_eq!(
                supplier_value["executor"],
                serde_json::to_value(expected_executor).unwrap()
            );
        }
    }

    #[test]
    fn supplier_projection_keeps_source_driver_and_executor_immutable_from_legacy_truth() {
        let mut channel =
            channel_from_supplier("immutable-driver".to_string(), &default_supplier_config());
        channel.set_source_driver(crate::source_driver::SourceDriverId::OpenAiSubscription);
        channel.kind = "anthropic".to_string();
        channel.api_format = "anthropic_messages".to_string();
        channel.upstream_base_url = "https://legacy.example.test/v1".to_string();
        channel.subscription.platform = "claude".to_string();

        let supplier = supplier_from_channel(&channel);

        assert_eq!(
            supplier.source_driver,
            crate::source_driver::SourceDriverId::OpenAiSubscription
        );
        assert_eq!(
            supplier.executor,
            ChannelExecutorLocator::RetainedSubscription {
                provider: "openai".to_string(),
            }
        );
    }

    #[test]
    fn runtime_supplier_projection_does_not_reconstruct_v2_truth_from_legacy_fields() {
        let mut supplier = default_supplier_config();
        supplier.source_driver = crate::source_driver::SourceDriverId::OpenAiSubscription;
        supplier.executor = ChannelExecutorLocator::RetainedSubscription {
            provider: "openai".to_string(),
        };
        supplier.credential_ref = "credential-v5".to_string();
        supplier.default_target = ChannelTarget {
            surface: crate::surface::ApiSurface::OpenAi,
            protocol: crate::protocol::kind::ProtocolKind::OpenAiResponses,
        };
        supplier.discovery.strategy = "explicit-v5".to_string();
        supplier.surface_bindings.clear();
        supplier.kind = "anthropic".to_string();
        supplier.api_format = "anthropic_messages".to_string();
        supplier.supported_protocols = vec!["anthropic_messages".to_string()];
        supplier.subscription.platform = "claude".to_string();

        let channel = channel_from_supplier("runtime-strict".to_string(), &supplier);

        assert_eq!(
            channel.source_driver(),
            crate::source_driver::SourceDriverId::OpenAiSubscription
        );
        assert_eq!(channel.v2.executor, supplier.executor);
        assert_eq!(channel.v2.credential_ref, "credential-v5");
        assert_eq!(channel.v2.default_target, supplier.default_target);
        assert_eq!(channel.v2.discovery.strategy, "explicit-v5");
        assert!(channel.surface_bindings.is_empty());
    }

    #[test]
    fn generic_normalization_does_not_infer_source_driver_from_legacy_fields() {
        let mut cfg = default_config();
        cfg.channels[0].kind = "openai".to_string();
        cfg.channels[0].upstream_base_url = "https://api.openai.com/v1".to_string();

        let normalized = normalize_config(cfg);

        assert_eq!(
            normalized.channels[0].source_driver(),
            crate::source_driver::SourceDriverId::CustomEndpoint
        );
    }

    #[test]
    fn normalize_channel_preserves_channel_limits_and_retires_subscription_only_controls() {
        let mut channel =
            channel_from_supplier("channel-sub".to_string(), &default_supplier_config());
        channel.kind = "subscription_adapter".to_string();
        channel.v2.max_concurrency = 1;
        channel.subscription.rpm_limit = 6;
        channel.v2.quota_reserve_percent = 25;
        channel.subscription.daily_request_limit = 200;

        let channel = normalize_channel(channel, 0, None, None);

        assert_eq!(channel.max_concurrency(), 1);
        assert_eq!(channel.subscription.rpm_limit, 0);
        assert_eq!(channel.quota_reserve_percent(), 25);
        assert_eq!(channel.subscription.daily_request_limit, 0);
    }

    #[test]
    fn normalize_channel_treats_legacy_missing_stream_evidence_as_supported() {
        let mut channel =
            channel_from_supplier("channel-stream".to_string(), &default_supplier_config());
        channel.capability_profiles = vec![ChannelCapabilityProfile {
            protocol: "openai_chat".to_string(),
            stream_sse: false,
            stream_sse_unsupported: false,
            ..Default::default()
        }];

        let normalized = normalize_channel(channel.clone(), 0, None, None);
        assert!(normalized.capability_profiles[0].stream_sse);
        assert!(!normalized.capability_profiles[0].stream_sse_unsupported);

        channel.capability_profiles[0].stream_sse_unsupported = true;
        let explicitly_unsupported = normalize_channel(channel, 0, None, None);
        assert!(!explicitly_unsupported.capability_profiles[0].stream_sse);
        assert!(explicitly_unsupported.capability_profiles[0].stream_sse_unsupported);
    }

    #[test]
    fn normalize_channel_removes_global_media_claims_from_every_subscription_driver() {
        use crate::source_driver::SourceDriverId;

        for (index, (source_driver, protocol)) in [
            (SourceDriverId::OpenAiSubscription, "openai_responses"),
            (SourceDriverId::ClaudeSubscription, "anthropic_messages"),
            (SourceDriverId::GeminiSubscription, "gemini_native"),
            (SourceDriverId::GrokSubscription, "openai_responses"),
        ]
        .into_iter()
        .enumerate()
        {
            let mut channel = channel_from_supplier(
                format!("subscription-media-{index}"),
                &default_supplier_config(),
            );
            channel.set_source_driver(source_driver);
            channel.capability_profiles = vec![
                ChannelCapabilityProfile {
                    protocol: protocol.to_string(),
                    capability_layer: "driver".to_string(),
                    input_modalities_authoritative: true,
                    output_modalities_authoritative: true,
                    vision: true,
                    image_input: true,
                    image_output: true,
                    audio_input: true,
                    audio_output: true,
                    video_input: true,
                    video_output: true,
                    file_input: true,
                    file_output: true,
                    verification_state: "declared".to_string(),
                    ..Default::default()
                },
                ChannelCapabilityProfile {
                    protocol: protocol.to_string(),
                    capability_layer: "model_wire".to_string(),
                    model_pattern: "exact-model".to_string(),
                    image_input: true,
                    file_input: true,
                    verification_state: "declared".to_string(),
                    ..Default::default()
                },
            ];
            if source_driver == SourceDriverId::OpenAiSubscription {
                channel.capability_profiles.extend([
                    ChannelCapabilityProfile {
                        protocol: "openai_responses".to_string(),
                        capability_layer: "driver".to_string(),
                        model_pattern: "legacy-rejection".to_string(),
                        input_modalities_authoritative: true,
                        verification_state: "driver_contract".to_string(),
                        ..Default::default()
                    },
                    ChannelCapabilityProfile {
                        protocol: "openai_responses".to_string(),
                        capability_layer: "client_host".to_string(),
                        file_input: true,
                        verification_state: "declared".to_string(),
                        ..Default::default()
                    },
                ]);
            }

            let normalized = normalize_channel(channel, index, None, None);
            let global = normalized
                .capability_profiles
                .iter()
                .find(|profile| {
                    profile.model_pattern.is_empty()
                        && crate::model::normalized_capability_layer(&profile.capability_layer)
                            == "driver"
                })
                .expect("global driver profile");
            assert!(!global.input_modalities_authoritative);
            assert!(!global.output_modalities_authoritative);
            assert!(
                !(global.vision
                    || global.image_input
                    || global.image_output
                    || global.audio_input
                    || global.audio_output
                    || global.video_input
                    || global.video_output
                    || global.file_input
                    || global.file_output)
            );

            let exact = normalized
                .capability_profiles
                .iter()
                .find(|profile| profile.model_pattern == "exact-model")
                .expect("exact model evidence is preserved");
            assert!(exact.image_input);
            assert!(exact.file_input);
            assert!(
                !normalized
                    .capability_profiles
                    .iter()
                    .any(|profile| profile.model_pattern == "legacy-rejection")
            );
        }
    }

    #[test]
    fn normalize_config_migrates_scaled_price_ratios_once() {
        let mut cfg = default_config();
        cfg.config_version = 3;
        cfg.supplier.price_ratio = 1_000_000.0;
        cfg.channels[0].price_ratio = 100_000.0;

        let cfg = normalize_config(cfg);

        assert_eq!(cfg.config_version, CURRENT_CLIENT_CONFIG_VERSION);
        assert!((cfg.channels[0].price_ratio - 0.1).abs() < f64::EPSILON);
        assert!((cfg.supplier.price_ratio - 0.1).abs() < f64::EPSILON);
    }

    #[test]
    fn normalize_config_preserves_decimal_price_ratios() {
        let mut cfg = default_config();
        cfg.config_version = 3;
        cfg.supplier.price_ratio = 0.001;
        cfg.channels[0].price_ratio = 0.001;

        let cfg = normalize_config(cfg);

        assert!((cfg.channels[0].price_ratio - 0.001).abs() < f64::EPSILON);
        assert!((cfg.supplier.price_ratio - 0.001).abs() < f64::EPSILON);
    }

    #[test]
    fn normalize_config_preserves_free_and_clamps_negative_price_ratios() {
        let mut free = default_config();
        free.channels[0].price_ratio = 0.0;
        let free = normalize_config(free);
        assert_eq!(free.channels[0].price_ratio, 0.0);
        assert_eq!(free.supplier.price_ratio, 0.0);

        let mut negative = default_config();
        negative.channels[0].price_ratio = -0.5;
        let negative = normalize_config(negative);
        assert_eq!(negative.channels[0].price_ratio, 0.0);
        assert_eq!(negative.supplier.price_ratio, 0.0);

        let mut invalid = default_config();
        invalid.channels[0].price_ratio = f64::NAN;
        let invalid = normalize_config(invalid);
        assert_eq!(invalid.channels[0].price_ratio, default_price_ratio());
        assert_eq!(invalid.supplier.price_ratio, default_price_ratio());

        let mut excessive = default_config();
        excessive.channels[0].price_ratio = 100.0;
        let excessive = normalize_config(excessive);
        assert_eq!(excessive.channels[0].price_ratio, MAX_PRICE_RATIO);
        assert_eq!(excessive.supplier.price_ratio, MAX_PRICE_RATIO);
    }

    #[test]
    fn load_config_preserves_an_existing_legacy_proxy_port() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("client.json");
        let mut cfg = default_config();
        cfg.listen = "127.0.0.1:8787".to_string();
        write_json(&path, &serde_json::to_value(cfg).expect("serialize config"));

        let loaded = load_config_from_path(&path).expect("load config");
        assert_eq!(loaded.listen, "127.0.0.1:8787");
    }

    #[test]
    fn load_config_persists_price_ratio_migration() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("client.json");
        let mut value = legacy_v4_fixture();
        value["config_version"] = serde_json::json!(3);
        value["channels"][0]["price_ratio"] = serde_json::json!(1_000_000.0);
        write_json(&path, &value);

        let loaded = load_config_from_path(&path).expect("load config");
        let persisted: ClientConfig =
            serde_json::from_slice(&std::fs::read(&path).expect("read persisted config"))
                .expect("decode persisted config");

        assert_eq!(loaded.config_version, CURRENT_CLIENT_CONFIG_VERSION);
        assert!((loaded.channels[0].price_ratio - 1.0).abs() < f64::EPSILON);
        assert_eq!(persisted.config_version, CURRENT_CLIENT_CONFIG_VERSION);
        assert!((persisted.channels[0].price_ratio - 1.0).abs() < f64::EPSILON);
    }
}

pub(crate) fn generated_channel_node_id(channel_id: &str) -> String {
    let seed = if development_profile_active() {
        format!(
            "{}:{}:development:{}",
            home_dir().display(),
            std::env::consts::OS,
            channel_id.trim()
        )
    } else {
        format!(
            "{}:{}:{}",
            home_dir().display(),
            std::env::consts::OS,
            channel_id.trim()
        )
    };
    generated_id_from_seed(&seed, "node")
}

pub(crate) fn machine_client_id() -> String { format!("local-{:032x}", rand::random::<u128>()) }

#[cfg(any(test, debug_assertions))]
fn client_id_override() -> Option<String> {
    std::env::var("CONST_API_CLIENT_ID").ok()
}

#[cfg(not(any(test, debug_assertions)))]
fn client_id_override() -> Option<String> {
    None
}

pub(crate) fn supplier_unit_id(client_id: &str, channel_id: &str) -> String {
    let seed = format!("{}:{}", client_id.trim(), channel_id.trim());
    generated_id_from_seed(&seed, "supplier")
}

fn normalize_identity(raw: &str, prefix: &str) -> String {
    let mut out = raw
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out = out.trim_matches('-').to_string();
    if out.is_empty() {
        return format!("{prefix}-");
    }
    if out.starts_with(&format!("{prefix}-")) {
        out
    } else {
        format!("{prefix}-{out}")
    }
}

pub(crate) fn generated_id_from_seed(seed: &str, prefix: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in seed.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{prefix}-{hash:016x}")
}

pub(crate) fn derive_supplier_ws_url(cfg: &ClientConfig) -> Option<String> {
    let endpoint = cfg
        .endpoints
        .iter()
        .find(|endpoint| endpoint.enabled && !endpoint.base_url.trim().is_empty())?;
    supplier_ws_from_endpoint(endpoint)
}

pub(crate) fn derive_supplier_quic_url(cfg: &ClientConfig) -> Option<String> {
    let endpoint = cfg
        .endpoints
        .iter()
        .find(|endpoint| endpoint.enabled && !endpoint.base_url.trim().is_empty())?;
    supplier_quic_from_endpoint(endpoint)
}

pub(crate) fn supplier_ws_from_endpoint(endpoint: &Endpoint) -> Option<String> {
    if let Some(ws_url) = normalize_supplier_ws_url(&endpoint.supplier_ws_url) {
        return Some(ws_url);
    }
    derive_ws_from_base_url(endpoint.base_url.trim().trim_end_matches('/'))
}

pub(crate) fn supplier_quic_from_endpoint(endpoint: &Endpoint) -> Option<String> {
    if endpoint.supplier_quic_url.trim().is_empty() {
        return None;
    }
    Some(
        endpoint
            .supplier_quic_url
            .trim()
            .trim_end_matches('/')
            .to_string(),
    )
}

pub(crate) fn derive_ws_from_base_url(base_url: &str) -> Option<String> {
    let base = base_url.trim().trim_end_matches('/');
    let parsed = reqwest::Url::parse(base).ok()?;
    let scheme = match parsed.scheme() {
        "https" => "wss",
        "http"
            if parsed
                .host_str()
                .map(is_local_supplier_host)
                .unwrap_or(false) =>
        {
            "ws"
        }
        _ => return None,
    };
    let host = parsed.host_str()?.trim();
    if host.is_empty() {
        return None;
    }
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    let port = parsed
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    let base_path = parsed.path().trim_end_matches('/');
    let path = if base_path.is_empty() || base_path == "/" {
        "/supplier/ws".to_string()
    } else {
        format!("{base_path}/supplier/ws")
    };
    Some(format!("{scheme}://{host}{port}{path}"))
}

pub(crate) fn normalize_supplier_ws_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() || !supplier_websocket_url_allowed(trimmed) {
        return None;
    }
    Some(trimmed.to_string())
}

pub(crate) fn supplier_websocket_url_allowed(raw: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(raw.trim()) else {
        return false;
    };
    match parsed.scheme() {
        "wss" => true,
        "ws" => parsed
            .host_str()
            .map(is_local_supplier_host)
            .unwrap_or(false),
        _ => false,
    }
}

pub(crate) fn is_local_supplier_host(host: &str) -> bool {
    let host = host.trim().trim_matches(['[', ']']);
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

pub(crate) fn is_current_local_proxy(base_url: &str, listen: &str) -> bool {
    let Ok(target) = reqwest::Url::parse(base_url.trim()) else {
        return false;
    };
    let Ok(local) = reqwest::Url::parse(&format!("http://{}", listen.trim())) else {
        return false;
    };
    let Some(target_host) = target.host_str().map(normalize_loopback_host) else {
        return false;
    };
    let Some(local_host) = local.host_str().map(normalize_loopback_host) else {
        return false;
    };
    target_host == local_host && target.port_or_known_default() == local.port_or_known_default()
}

pub(crate) fn normalize_loopback_host(host: &str) -> String {
    match host {
        "localhost" | "127.0.0.1" | "::1" => "loopback".to_string(),
        value => value.to_string(),
    }
}
