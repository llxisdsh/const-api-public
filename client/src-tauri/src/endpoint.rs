use crate::{
    config::{normalize_supplier_ws_url, supplier_quic_from_endpoint, supplier_ws_from_endpoint},
    model::*,
};
use anyhow::{Context, Result, anyhow};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use reqwest::{Client, Url};
use semver::Version;
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::HashSet,
    fs,
    io::Write,
    net::{IpAddr, Ipv6Addr},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

const ENDPOINT_REGISTRY_CACHE_FILE: &str = "endpoint-registry-cache.json";
const BUNDLED_ENDPOINT_REGISTRY: &[u8] = include_bytes!("../../../shared/endpoints.json");
const ENDPOINT_SOURCE_TIMEOUT: Duration = Duration::from_secs(5);
const ENDPOINT_ACTIVATION_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_DEVELOPMENT_SERVER_PORT: u16 = 8080;
#[cfg(debug_assertions)]
const DEVELOPMENT_SERVER_PORT_ENV: &str = "CONST_API_DEV_SERVER_PORT";

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PinnedRegistryKey {
    pub(crate) key_id: String,
    pub(crate) public_key: String,
}

#[derive(Debug, Clone)]
pub(crate) struct VerifiedEndpointManifest {
    pub(crate) manifest: EndpointRegistryManifest,
    raw: Vec<u8>,
}

impl VerifiedEndpointManifest {
    pub(crate) fn version(&self) -> i64 {
        self.manifest.version
    }
}

#[cfg(any(debug_assertions, test))]
fn parse_development_server_port(value: &str) -> Option<u16> {
    value.trim().parse::<u16>().ok().filter(|port| *port != 0)
}

fn development_server_port() -> u16 {
    #[cfg(debug_assertions)]
    {
        let Ok(value) = std::env::var(DEVELOPMENT_SERVER_PORT_ENV) else {
            return DEFAULT_DEVELOPMENT_SERVER_PORT;
        };
        if value.trim().is_empty() {
            return DEFAULT_DEVELOPMENT_SERVER_PORT;
        }
        return parse_development_server_port(&value).unwrap_or_else(|| {
            eprintln!(
                "[const-api][endpoint] ignoring invalid {DEVELOPMENT_SERVER_PORT_ENV}={value:?}; expected a port from 1 to 65535"
            );
            DEFAULT_DEVELOPMENT_SERVER_PORT
        });
    }
    #[cfg(not(debug_assertions))]
    DEFAULT_DEVELOPMENT_SERVER_PORT
}

fn local_dev_endpoint_for_port(port: u16) -> Endpoint {
    Endpoint {
        server_id: String::new(),
        name: crate::config::DEVELOPMENT_ENDPOINT_LOCAL.to_string(),
        base_url: format!("http://127.0.0.1:{port}"),
        supplier_ws_url: format!("ws://127.0.0.1:{port}/supplier/ws"),
        supplier_quic_url: format!("quic://127.0.0.1:{port}/supplier"),
        enabled: false,
    }
}

pub(crate) fn local_dev_endpoint() -> Endpoint {
    local_dev_endpoint_for_port(development_server_port())
}

pub(crate) fn merge_discovered_endpoints(_: Vec<Endpoint>) -> Vec<Endpoint> { Vec::new() }

fn merge_discovered_endpoints_for_mode(
    discovered: Vec<Endpoint>,
    development_mode: bool,
) -> Vec<Endpoint> {
    let fallback = local_dev_endpoint();
    let fallback_key = endpoint_api_key(&fallback.base_url);
    let mut seen = HashSet::new();
    let mut merged = Vec::with_capacity(discovered.len() + usize::from(development_mode));

    for endpoint in discovered {
        let key = endpoint_api_key(&endpoint.base_url);
        if key == fallback_key || !seen.insert(key) {
            continue;
        }
        merged.push(endpoint);
    }
    if development_mode {
        merged.push(fallback);
    }
    merged
}

fn endpoint_api_key(raw: &str) -> String {
    let trimmed = raw.trim();
    let Ok(mut url) = Url::parse(trimmed) else {
        return trimmed.to_string();
    };
    let path = url.path().trim_end_matches('/').to_string();
    url.set_path(if path.is_empty() { "/" } else { &path });
    url.set_fragment(None);
    url.to_string().trim_end_matches('/').to_string()
}

pub(crate) async fn fetch_endpoint_manifest(
    client: &Client,
    source: &str,
    cache_path: &Path,
    minimum_version: i64,
) -> Result<VerifiedEndpointManifest> {
    // Registry documents are small. Bound each mirror independently so an
    // unreachable regional source cannot delay the next trusted fallback.
    let keys = compiled_pinned_registry_keys()?;
    crate::conditional_document::public_document_cache()
        .fetch(
            &format!("endpoint:{source}"),
            client.get(source).timeout(ENDPOINT_SOURCE_TIMEOUT),
            None,
            |raw, _| {
                validate_and_store_endpoint_manifest_at(
                    raw,
                    source,
                    &keys,
                    minimum_version,
                    current_unix_timestamp()?,
                    env!("CARGO_PKG_VERSION"),
                    cache_path,
                )
            },
        )
        .await
}

fn parse_remote_endpoint_manifest_at(
    raw: &[u8],
    source: &str,
    public_keys: &[PinnedRegistryKey],
    minimum_version: i64,
    now_unix: i64,
    client_version: &str,
) -> Result<VerifiedEndpointManifest> {
    if public_keys.is_empty() {
        return Err(anyhow!("remote endpoint registry pinned key set is empty"));
    }
    let mut document = serde_json::from_slice::<Value>(raw)
        .with_context(|| format!("invalid endpoint registry JSON from {source}"))?;
    verify_remote_manifest_signature(&mut document, public_keys)
        .map_err(|error| anyhow!("invalid endpoint registry signature from {source}: {error}"))?;
    let manifest = serde_json::from_slice::<EndpointRegistryManifest>(raw)
        .with_context(|| format!("invalid endpoint registry JSON from {source}"))?;
    validate_endpoint_manifest(&manifest, minimum_version, now_unix, client_version)
        .map_err(|error| anyhow!("invalid endpoint registry contents from {source}: {error}"))?;
    Ok(VerifiedEndpointManifest {
        manifest,
        raw: raw.to_vec(),
    })
}

fn validate_and_store_endpoint_manifest_at(
    raw: &[u8],
    source: &str,
    public_keys: &[PinnedRegistryKey],
    minimum_version: i64,
    now_unix: i64,
    client_version: &str,
    cache_path: &Path,
) -> Result<VerifiedEndpointManifest> {
    let verified = parse_remote_endpoint_manifest_at(
        raw,
        source,
        public_keys,
        minimum_version,
        now_unix,
        client_version,
    )?;
    write_verified_endpoint_cache(cache_path, &verified.raw)?;
    Ok(verified)
}

fn load_cached_endpoint_manifest_at(
    cache_path: &Path,
    public_keys: &[PinnedRegistryKey],
    now_unix: i64,
    client_version: &str,
) -> Result<VerifiedEndpointManifest> {
    let raw = fs::read(cache_path)
        .with_context(|| format!("read endpoint registry cache {}", cache_path.display()))?;
    parse_remote_endpoint_manifest_at(
        &raw,
        "local-cache",
        public_keys,
        0,
        now_unix,
        client_version,
    )
}

pub(crate) fn load_cached_endpoint_manifest(cache_path: &Path) -> Result<VerifiedEndpointManifest> {
    let keys = compiled_pinned_registry_keys()?;
    load_cached_endpoint_manifest_at(
        cache_path,
        &keys,
        current_unix_timestamp()?,
        env!("CARGO_PKG_VERSION"),
    )
}

fn parse_bundled_endpoint_manifest_at(
    raw: &[u8],
    now_unix: i64,
    client_version: &str,
) -> Result<VerifiedEndpointManifest> {
    // This is the exact unsigned publication source compiled into the application,
    // so it is protected by the application artifact rather than registry signing.
    // Remote updates still require an Ed25519 signature before replacing it.
    let manifest = serde_json::from_slice::<EndpointRegistryManifest>(raw)
        .context("invalid bundled endpoint registry JSON")?;
    validate_endpoint_manifest(&manifest, 0, now_unix, client_version)
        .context("invalid bundled endpoint registry contents")?;
    Ok(VerifiedEndpointManifest {
        manifest,
        raw: raw.to_vec(),
    })
}

pub(crate) fn load_bundled_endpoint_manifest() -> Result<VerifiedEndpointManifest> {
    parse_bundled_endpoint_manifest_at(
        BUNDLED_ENDPOINT_REGISTRY,
        current_unix_timestamp()?,
        env!("CARGO_PKG_VERSION"),
    )
}

pub(crate) fn bundled_endpoint_discovery() -> Result<EndpointDiscoveryResult> { Ok(EndpointDiscoveryResult { source: "local_only".into(), version: 0, platform_id: String::new(), endpoints: Vec::new(), refresh_warning: None }) }

pub(crate) fn endpoint_registry_cache_path(config_path: &Path) -> PathBuf {
    config_path.with_file_name(ENDPOINT_REGISTRY_CACHE_FILE)
}

pub(crate) fn endpoint_registry_cache_snapshot(cache_path: &Path) -> Option<Vec<u8>> {
    fs::read(cache_path).ok()
}

pub(crate) fn restore_endpoint_registry_cache(
    cache_path: &Path,
    previous: Option<&[u8]>,
) -> Result<()> {
    if let Some(previous) = previous {
        return write_verified_endpoint_cache(cache_path, previous);
    }
    match fs::remove_file(cache_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("remove unactivated endpoint cache {}", cache_path.display())),
    }
}

pub(crate) fn fresh_endpoint_http_client() -> Result<Client> {
    Client::builder()
        .connect_timeout(ENDPOINT_ACTIVATION_PROBE_TIMEOUT)
        .timeout(ENDPOINT_ACTIVATION_PROBE_TIMEOUT)
        .retry(reqwest::retry::never())
        .build()
        .context("build endpoint activation client")
}

pub(crate) async fn prioritize_reachable_endpoint(
    discovery: &mut EndpointDiscoveryResult,
) -> Result<()> {
    let client = fresh_endpoint_http_client()?;
    prioritize_reachable_endpoint_with_client(discovery, &client).await
}

async fn prioritize_reachable_endpoint_with_client(
    discovery: &mut EndpointDiscoveryResult,
    client: &Client,
) -> Result<()> {
    let mut failures = Vec::new();
    for index in 0..discovery.endpoints.len() {
        let endpoint = &discovery.endpoints[index];
        if !endpoint.enabled || endpoint.base_url.trim().is_empty() {
            continue;
        }
        let health_url = format!("{}/livez", endpoint.base_url.trim_end_matches('/'));
        match client.get(&health_url).send().await {
            Ok(response) if response.status().is_success() => {
                if index != 0 {
                    discovery.endpoints.swap(0, index);
                }
                return Ok(());
            }
            Ok(response) => failures.push(format!(
                "{} returned HTTP {}",
                endpoint.name,
                response.status()
            )),
            Err(error) => failures.push(format!("{}: {}", endpoint.name, error)),
        }
    }
    Err(anyhow!(
        "no discovered endpoint passed activation probe: {}",
        failures.join("; ")
    ))
}

pub(crate) fn endpoint_discovery_from_manifest(
    source: impl Into<String>,
    manifest: EndpointRegistryManifest,
) -> Result<EndpointDiscoveryResult> {
    let source = source.into();
    let discovered = endpoints_from_manifest(&manifest);
    if discovered.is_empty() {
        return Err(anyhow!("{source} returned no endpoints"));
    }
    Ok(EndpointDiscoveryResult {
        source,
        version: manifest.version,
        platform_id: manifest.platform_id,
        endpoints: merge_discovered_endpoints(discovered),
        refresh_warning: None,
    })
}

pub(crate) fn endpoint_discovery_from_verified_manifest(
    source: impl Into<String>,
    verified: &VerifiedEndpointManifest,
) -> Result<EndpointDiscoveryResult> {
    endpoint_discovery_from_manifest(source, verified.manifest.clone())
}

pub(crate) fn fallback_endpoint_discovery(
    cache: Option<&VerifiedEndpointManifest>,
    bundled: Option<&VerifiedEndpointManifest>,
    remote_error: impl Into<String>,
) -> Result<EndpointDiscoveryResult> {
    let remote_error = remote_error.into();
    let preferred = match (cache, bundled) {
        (Some(cache), Some(bundled)) if cache.version() >= bundled.version() => {
            Some(("local-cache", cache))
        }
        (Some(_), Some(bundled)) => Some(("embedded", bundled)),
        (Some(cache), None) => Some(("local-cache", cache)),
        (None, Some(bundled)) => Some(("embedded", bundled)),
        (None, None) => None,
    };
    if let Some((source, manifest)) = preferred {
        let mut result = endpoint_discovery_from_verified_manifest(source, manifest)?;
        result.refresh_warning = Some(remote_error);
        return Ok(result);
    }
    Err(anyhow!(
        "endpoint registry unavailable: {remote_error}; no valid signed cache or bundled snapshot"
    ))
}

pub(crate) fn trusted_endpoint_discovery(_: &Path) -> Result<EndpointDiscoveryResult> { Ok(EndpointDiscoveryResult { source: "local_only".into(), version: 0, platform_id: String::new(), endpoints: Vec::new(), refresh_warning: None }) }

fn apply_development_endpoint_preference(
    endpoints: &mut [Endpoint],
    preference: &str,
    development_mode: bool,
) {
    let preference = crate::config::normalize_development_endpoint(preference);
    if !development_mode {
        for endpoint in endpoints {
            endpoint.enabled = endpoint.name != "local-dev";
        }
        return;
    }

    let selected = (preference != crate::config::DEVELOPMENT_ENDPOINT_AUTO
        && endpoints.iter().any(|endpoint| endpoint.name == preference))
    .then_some(preference.as_str());
    for endpoint in endpoints {
        endpoint.enabled = match selected {
            Some(selected) => endpoint.name == selected,
            None => endpoint.name != "local-dev",
        };
    }
}

pub(crate) fn apply_endpoint_discovery_to_config(config: &mut ClientConfig, _: &EndpointDiscoveryResult) { crate::local_policy::enforce(config); }

// One selection boundary serves account HTTP, proxy HTTP/WS/QUIC and supplier
// transport alike. Disabled foreign shards must never receive account secrets.
pub(crate) fn apply_account_home_endpoint_filter(config: &mut ClientConfig) {
    if config.account_home_server_id.is_empty() && config.account_home_base_url.is_empty() {
        return;
    }
    if config.account_home_server_id.is_empty()
        && let Some(endpoint) = config.endpoints.iter().find(|endpoint| {
            endpoint_api_key(&endpoint.base_url) == endpoint_api_key(&config.account_home_base_url)
        })
    {
        config.account_home_server_id = endpoint.server_id.clone();
    }
    // Unlabelled entries retain the old registry contract: aliases of the
    // original single database. New independent shards must declare server_id.
    let legacy_home = config.endpoints.iter().any(|endpoint| {
        endpoint.server_id.is_empty()
            && endpoint_api_key(&endpoint.base_url)
                == endpoint_api_key(&config.account_home_base_url)
    });
    for endpoint in &mut config.endpoints {
        let same_home =
            if !config.account_home_server_id.is_empty() && !endpoint.server_id.is_empty() {
                config.account_home_server_id == endpoint.server_id
            } else {
                endpoint.server_id.is_empty() && legacy_home
            };
        endpoint.enabled &= same_home;
    }
}

pub(crate) fn apply_trusted_endpoint_state(config: &mut ClientConfig, _: &Path) { crate::local_policy::enforce(config); }

fn endpoint_preferred_for_client(node: &EndpointRegistryNode, client_version: &Version) -> bool {
    let preferred_from = node.preferred_from_client_version.trim();
    !preferred_from.is_empty()
        && Version::parse(preferred_from).is_ok_and(|minimum| client_version >= &minimum)
}

fn endpoints_from_manifest_for_client(
    manifest: &EndpointRegistryManifest,
    client_version: &Version,
) -> Vec<Endpoint> {
    let mut nodes = manifest.endpoints.clone();
    nodes.sort_by(|a, b| {
        let region_cmp = a.region.cmp(&b.region);
        if region_cmp == std::cmp::Ordering::Equal {
            endpoint_preferred_for_client(b, client_version)
                .cmp(&endpoint_preferred_for_client(a, client_version))
                .then_with(|| b.weight.cmp(&a.weight))
                .then_with(|| a.id.cmp(&b.id))
        } else {
            region_cmp
        }
    });
    nodes
        .into_iter()
        .filter(|node| !node.id.trim().is_empty() && !node.api.trim().is_empty())
        .map(|node| Endpoint {
            server_id: node.server_id.clone(),
            name: node.id.trim().to_string(),
            base_url: node.api.trim().trim_end_matches('/').to_string(),
            supplier_ws_url: normalize_supplier_ws_url(&node.ws).unwrap_or_default(),
            supplier_quic_url: quic_url_with_certificate_pin(
                node.quic.trim().trim_end_matches('/'),
                &node.quic_cert_sha256,
            ),
            enabled: true,
        })
        .collect()
}

pub(crate) fn endpoints_from_manifest(manifest: &EndpointRegistryManifest) -> Vec<Endpoint> {
    let client_version = Version::parse(env!("CARGO_PKG_VERSION"))
        .expect("package version must be valid semantic version syntax");
    endpoints_from_manifest_for_client(manifest, &client_version)
}

fn quic_url_with_certificate_pin(raw: &str, fingerprint: &str) -> String {
    if raw.is_empty() || fingerprint.trim().is_empty() {
        return raw.to_string();
    }
    let normalized = fingerprint
        .trim()
        .strip_prefix("sha256:")
        .unwrap_or(fingerprint.trim())
        .replace(':', "");
    if normalized.len() != 64 || hex::decode(&normalized).is_err() {
        return raw.to_string();
    }
    let Ok(mut url) = Url::parse(raw) else {
        return raw.to_string();
    };
    url.query_pairs_mut()
        .append_pair("cert_sha256", &format!("sha256:{normalized}"));
    url.to_string()
}

fn compiled_pinned_registry_keys() -> Result<Vec<PinnedRegistryKey>> {
    let raw = option_env!("CONST_API_REGISTRY_PUBLIC_KEYS")
        .unwrap_or("")
        .trim();
    if raw.is_empty() {
        return Err(anyhow!("remote endpoint registry pinned key set is empty"));
    }
    let keys = serde_json::from_str::<Vec<PinnedRegistryKey>>(raw)
        .context("parse compiled endpoint registry pinned key set")?;
    if keys.is_empty() {
        return Err(anyhow!("remote endpoint registry pinned key set is empty"));
    }
    let mut seen = HashSet::new();
    for key in &keys {
        let key_id = key.key_id.trim();
        if !valid_key_id(key_id) {
            return Err(anyhow!("invalid compiled endpoint registry key id"));
        }
        if !seen.insert(key_id) {
            return Err(anyhow!(
                "duplicate compiled endpoint registry key id {key_id}"
            ));
        }
        let raw_key = BASE64
            .decode(key.public_key.trim())
            .with_context(|| format!("decode compiled endpoint registry key {key_id}"))?;
        if raw_key.len() != 32 {
            return Err(anyhow!(
                "compiled endpoint registry key {key_id} must be 32 bytes"
            ));
        }
    }
    Ok(keys)
}

fn verify_remote_manifest_signature(
    document: &mut Value,
    public_keys: &[PinnedRegistryKey],
) -> Result<()> {
    let object = document
        .as_object_mut()
        .ok_or_else(|| anyhow!("endpoint registry manifest must be a JSON object"))?;
    let key_id = object
        .get("key_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|key_id| valid_key_id(key_id))
        .ok_or_else(|| anyhow!("missing or invalid registry key id"))?
        .to_string();
    let encoded_signature = object
        .get("signature")
        .and_then(Value::as_str)
        .and_then(|signature| signature.strip_prefix("ed25519:"))
        .map(str::trim)
        .filter(|signature| !signature.is_empty())
        .ok_or_else(|| anyhow!("remote endpoint registry requires an ed25519 signature"))?
        .to_string();
    let pinned = public_keys
        .iter()
        .find(|key| key.key_id.trim() == key_id)
        .ok_or_else(|| anyhow!("unknown registry key id {key_id}"))?;

    object.insert("signature".to_string(), Value::String(String::new()));
    let canonical = serde_json::to_vec(document).context("canonicalize endpoint registry")?;
    let signature_bytes = BASE64
        .decode(encoded_signature)
        .context("decode endpoint registry signature")?;
    let signature = Signature::from_slice(&signature_bytes)
        .context("invalid endpoint registry ed25519 signature")?;
    let public_key_bytes = BASE64
        .decode(pinned.public_key.trim())
        .with_context(|| format!("decode endpoint registry key {key_id}"))?;
    let public_key_bytes = <[u8; 32]>::try_from(public_key_bytes.as_slice())
        .map_err(|_| anyhow!("endpoint registry key {key_id} must be 32 bytes"))?;
    let public_key = VerifyingKey::from_bytes(&public_key_bytes)
        .with_context(|| format!("invalid endpoint registry key {key_id}"))?;
    public_key
        .verify(&canonical, &signature)
        .map_err(|_| anyhow!("endpoint registry signature mismatch for key id {key_id}"))
}

fn validate_endpoint_manifest(
    manifest: &EndpointRegistryManifest,
    minimum_version: i64,
    now_unix: i64,
    client_version: &str,
) -> Result<()> {
    if manifest.version <= 0 {
        return Err(anyhow!("endpoint registry version must be positive"));
    }
    if manifest.version < minimum_version {
        return Err(anyhow!(
            "endpoint registry rollback rejected: version {} is below cached version {minimum_version}",
            manifest.version
        ));
    }
    if manifest.platform_id.trim().is_empty() {
        return Err(anyhow!("endpoint registry platform_id is required"));
    }
    let expires_at = OffsetDateTime::parse(manifest.expires_at.trim(), &Rfc3339)
        .context("endpoint registry expires_at must be RFC3339")?;
    if expires_at.unix_timestamp() <= now_unix {
        return Err(anyhow!("endpoint registry manifest is expired"));
    }
    let client_version = Version::parse(client_version).context("invalid client version")?;
    if manifest.endpoints.is_empty() {
        return Err(anyhow!(
            "endpoint registry must contain at least one endpoint"
        ));
    }

    let mut endpoint_ids = HashSet::new();
    for (index, endpoint) in manifest.endpoints.iter().enumerate() {
        let id = endpoint.id.trim();
        if id.is_empty() {
            return Err(anyhow!("endpoint {index} id is required"));
        }
        if !endpoint_ids.insert(id) {
            return Err(anyhow!("duplicate endpoint registry id {id}"));
        }
        if endpoint.region.trim().is_empty() {
            return Err(anyhow!("endpoint {id} region is required"));
        }
        if endpoint.weight < 0 {
            return Err(anyhow!("endpoint {id} weight cannot be negative"));
        }
        let minimum_client = Version::parse(endpoint.min_client_version.trim())
            .with_context(|| format!("endpoint {id} has invalid min_client_version"))?;
        if minimum_client > client_version {
            return Err(anyhow!(
                "endpoint {id} requires client {minimum_client}, current client is {client_version}"
            ));
        }
        let preferred_from = endpoint.preferred_from_client_version.trim();
        if !preferred_from.is_empty() {
            Version::parse(preferred_from).with_context(|| {
                format!("endpoint {id} has invalid preferred_from_client_version")
            })?;
        }

        validate_registry_url(&endpoint.api, &format!("endpoint {id} api"), "https")?;
        if !endpoint.ws.trim().is_empty() {
            validate_registry_url(&endpoint.ws, &format!("endpoint {id} websocket"), "wss")?;
        }
        if !endpoint.quic.trim().is_empty() {
            let loopback =
                validate_registry_url(&endpoint.quic, &format!("endpoint {id} QUIC"), "quic")?;
            if !loopback && normalized_sha256_pin(&endpoint.quic_cert_sha256).is_none() {
                return Err(anyhow!(
                    "endpoint {id} remote QUIC requires a valid SHA-256 pin"
                ));
            }
        }
    }
    Ok(())
}

fn validate_registry_url(raw: &str, label: &str, remote_scheme: &str) -> Result<bool> {
    let url = Url::parse(raw.trim()).with_context(|| format!("{label} must be a valid URL"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(anyhow!("{label} cannot contain URL credentials"));
    }
    if url.fragment().is_some() {
        return Err(anyhow!("{label} cannot contain a fragment"));
    }
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("{label} must contain a valid host"))?;
    let loopback = validate_registry_host(host, label)?;
    if loopback {
        return Err(anyhow!(
            "{label} cannot use a loopback host in the published endpoint registry"
        ));
    }
    if url.scheme() != remote_scheme {
        return Err(anyhow!("{label} must use {remote_scheme}"));
    }
    Ok(false)
}

fn validate_registry_host(host: &str, label: &str) -> Result<bool> {
    let host = host.trim();
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host)
        .to_ascii_lowercase();
    if host == "localhost" {
        return Ok(true);
    }
    if let Ok(address) = host.parse::<IpAddr>() {
        if address == IpAddr::from([127, 0, 0, 1]) || address == IpAddr::V6(Ipv6Addr::LOCALHOST) {
            return Ok(true);
        }
        if address.is_unspecified() || address.is_multicast() {
            return Err(anyhow!("{label} has an invalid host"));
        }
        return Ok(false);
    }
    if host.len() > 253 || !host.contains('.') {
        return Err(anyhow!("{label} has an invalid host"));
    }
    for component in host.split('.') {
        if component.is_empty()
            || component.len() > 63
            || !component
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || !component
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            || !component
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric)
        {
            return Err(anyhow!("{label} has an invalid host"));
        }
    }
    Ok(false)
}

fn normalized_sha256_pin(raw: &str) -> Option<String> {
    let normalized = raw
        .trim()
        .strip_prefix("sha256:")
        .unwrap_or(raw.trim())
        .replace(':', "");
    (normalized.len() == 64 && hex::decode(&normalized).is_ok())
        .then(|| normalized.to_ascii_lowercase())
}

fn valid_key_id(key_id: &str) -> bool {
    !key_id.is_empty()
        && key_id.len() <= 64
        && key_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn write_verified_endpoint_cache(cache_path: &Path, raw: &[u8]) -> Result<()> {
    let parent = cache_path
        .parent()
        .ok_or_else(|| anyhow!("endpoint registry cache path has no parent"))?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "create endpoint registry cache directory {}",
            parent.display()
        )
    })?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).with_context(|| {
        format!(
            "create endpoint registry cache temp file beside {}",
            cache_path.display()
        )
    })?;
    temp.write_all(raw)
        .with_context(|| format!("write endpoint registry cache {}", cache_path.display()))?;
    temp.flush()?;
    temp.as_file().sync_all()?;
    temp.persist(cache_path)
        .map_err(|error| error.error)
        .with_context(|| format!("replace endpoint registry cache {}", cache_path.display()))?;
    Ok(())
}

fn current_unix_timestamp() -> Result<i64> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs();
    i64::try_from(seconds).context("system clock exceeds supported timestamp range")
}
