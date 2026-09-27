use crate::endpoint::PinnedRegistryKey;
use anyhow::{Context, Result, anyhow};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use futures_util::StreamExt;
use reqwest::{
    Client, Response, StatusCode, Url,
    header::{ACCEPT, ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED},
};
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

const MANIFEST_SCHEMA_VERSION: u32 = 1;
const MAX_MANIFEST_BYTES: usize = 256 << 10;
const MAX_MODEL_BYTES: u64 = 4 << 20;
const MAX_PRICING_BYTES: u64 = 16 << 20;
const MAX_TOOL_BYTES: u64 = 8 << 20;
const MANIFEST_SOURCE_TIMEOUT: Duration = Duration::from_secs(4);

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CatalogComponent {
    url: String,
    sha256: String,
    size: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CatalogComponents {
    model_catalog: CatalogComponent,
    pricing_catalog: CatalogComponent,
    tool_metadata: CatalogComponent,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CatalogManifest {
    schema_version: u32,
    sequence: i64,
    release_id: String,
    released_at: String,
    expires_at: String,
    min_client_version: String,
    min_server_version: String,
    model_version_id: String,
    compatibility_version_id: String,
    pricing_version_id: String,
    components: CatalogComponents,
    key_id: String,
    signature: String,
}

#[derive(Debug, Clone)]
pub(crate) struct CatalogToolRelease {
    pub(crate) sequence: i64,
    pub(crate) release_id: String,
    pub(crate) model_version_id: String,
    pub(crate) compatibility_version_id: String,
    pub(crate) delivery_source: String,
    pub(crate) delivery_sources: Vec<String>,
    tool_raw: Vec<u8>,
}

impl CatalogToolRelease {
    pub(crate) fn install(&self) -> Result<()> {
        crate::tool_model_metadata::install_tool_model_metadata(
            &self.tool_raw,
            &self.compatibility_version_id,
            crate::tool_model_metadata::ModelCatalogVersionInfo {
                release_id: self.release_id.clone(),
                model_version_id: self.model_version_id.clone(),
                compatibility_version_id: self.compatibility_version_id.clone(),
                sequence: Some(self.sequence),
                source: "public".to_string(),
                delivery_source: self.delivery_source.clone(),
                delivery_sources: self.delivery_sources.clone(),
            },
        )
    }
}

#[derive(Debug)]
pub(crate) struct CatalogRefresh {
    pub(crate) release: CatalogToolRelease,
    pub(crate) changed: bool,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct HttpCacheState {
    #[serde(default)]
    source_url: String,
    #[serde(default)]
    etag: String,
    #[serde(default)]
    last_modified: String,
}

struct CachePaths {
    manifest: PathBuf,
    http_state: PathBuf,
    components: PathBuf,
}

pub(crate) fn catalog_refresh_jitter(base: Duration) -> Duration {
    let span = base.as_secs() / 5;
    if span == 0 {
        return base;
    }
    Duration::from_secs(base.as_secs() - span + rand::random_range(0..=span * 2))
}

pub(crate) fn load_cached_catalog_tool(
    config_path: &Path,
    allow_expired: bool,
) -> Result<CatalogToolRelease> {
    let paths = cache_paths(config_path)?;
    let raw = fs::read(&paths.manifest)
        .with_context(|| format!("read catalog manifest cache {}", paths.manifest.display()))?;
    let keys = compiled_catalog_keys()?;
    let manifest = verify_manifest(
        &raw,
        &keys,
        packaged_catalog_minimum_sequence(),
        allow_expired,
    )?;
    let delivery_sources =
        crate::release_sources::trusted_release_sources(config_path)?.catalog_registry_urls();
    release_from_cache(&paths, manifest, delivery_sources)
}

pub(crate) async fn refresh_catalog_tool(
    client: &Client,
    config_path: &Path,
) -> Result<CatalogRefresh> {
    let release_sources = crate::release_sources::trusted_release_sources(config_path)?;
    let catalog_sources = release_sources.catalog_sources();
    let manifest_urls = catalog_sources
        .iter()
        .map(|source| source.manifest_url.clone())
        .collect::<Vec<_>>();
    let mut failures = Vec::new();
    for source in &catalog_sources {
        match refresh_catalog_tool_from_source(
            client,
            config_path,
            &source.manifest_url,
            &source.component_base_url,
            &manifest_urls,
        )
        .await
        {
            Ok(refresh) => return Ok(refresh),
            Err(error) => failures.push(format!("{}: {error:#}", source.manifest_url)),
        }
    }
    Err(anyhow!(
        "all catalog registry sources failed: {}",
        failures.join("; ")
    ))
}

async fn refresh_catalog_tool_from_source(
    client: &Client,
    config_path: &Path,
    manifest_url: &str,
    component_base_url: &str,
    delivery_sources: &[String],
) -> Result<CatalogRefresh> {
    let paths = cache_paths(config_path)?;
    let keys = compiled_catalog_keys()?;
    let cached_raw = fs::read(&paths.manifest).ok();
    let cached_manifest = cached_raw.as_deref().and_then(|raw| {
        verify_manifest(raw, &keys, packaged_catalog_minimum_sequence(), true).ok()
    });
    let minimum_sequence = cached_manifest
        .as_ref()
        .map(|manifest| manifest.sequence)
        .unwrap_or_else(packaged_catalog_minimum_sequence);
    let http_state = fs::read(&paths.http_state)
        .ok()
        .and_then(|raw| serde_json::from_slice::<HttpCacheState>(&raw).ok())
        .unwrap_or_default();

    let mut request = client
        .get(manifest_url)
        .timeout(MANIFEST_SOURCE_TIMEOUT)
        .header(ACCEPT, "application/json")
        .header("User-Agent", "const-api-client/catalog-registry");
    let same_source = http_state.source_url.trim() == manifest_url;
    if cached_manifest.is_some() && same_source && !http_state.etag.trim().is_empty() {
        request = request.header(IF_NONE_MATCH, http_state.etag.trim());
    }
    if cached_manifest.is_some() && same_source && !http_state.last_modified.trim().is_empty() {
        request = request.header(IF_MODIFIED_SINCE, http_state.last_modified.trim());
    }
    let mut response = request
        .send()
        .await
        .context("refresh catalog registry manifest")?;
    validate_final_https(response.url(), "catalog manifest")?;
    if response.status() == StatusCode::NOT_MODIFIED {
        let cached = cached_raw
            .as_deref()
            .context("catalog registry returned 304 without a cache")
            .and_then(|raw| verify_manifest(raw, &keys, minimum_sequence, false))
            .and_then(|manifest| release_from_cache(&paths, manifest, delivery_sources.to_vec()));
        if let Ok(mut release) = cached {
            release.delivery_source = manifest_url.to_string();
            return Ok(CatalogRefresh {
                release,
                changed: false,
            });
        }
        // A proxy/CDN may correctly return 304 while a local component was
        // truncated. Retry the root without validators and repair only the
        // content-addressed component that fails its hash.
        response = client
            .get(manifest_url)
            .timeout(MANIFEST_SOURCE_TIMEOUT)
            .header(ACCEPT, "application/json")
            .header("User-Agent", "const-api-client/catalog-registry")
            .send()
            .await
            .context("repair catalog registry cache")?;
        validate_final_https(response.url(), "catalog manifest")?;
    }
    if !response.status().is_success() {
        return Err(anyhow!(
            "catalog registry returned HTTP {}",
            response.status()
        ));
    }
    let etag = response
        .headers()
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .trim()
        .to_string();
    let last_modified = response
        .headers()
        .get(LAST_MODIFIED)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .trim()
        .to_string();
    let raw = read_response_limited(response, MAX_MANIFEST_BYTES as u64)
        .await
        .context("read catalog manifest")?;
    if raw.is_empty() {
        return Err(anyhow!("catalog manifest size is invalid"));
    }
    let manifest = verify_manifest(&raw, &keys, minimum_sequence, false)?;
    if cached_manifest
        .as_ref()
        .is_some_and(|cached| cached.sequence == manifest.sequence)
        && cached_raw.as_deref() != Some(raw.as_ref())
    {
        return Err(anyhow!(
            "catalog sequence {} was reused with different signed content",
            manifest.sequence
        ));
    }
    let tool_raw = load_or_fetch_component(
        client,
        &paths,
        manifest_url,
        component_base_url,
        &manifest.components.tool_metadata,
    )
    .await
    .context("load catalog tool metadata")?;
    crate::tool_model_metadata::parse_tool_model_metadata(
        &tool_raw,
        Some(&manifest.compatibility_version_id),
    )?;

    atomic_write(&paths.manifest, &raw)?;
    let state = serde_json::to_vec(&HttpCacheState {
        source_url: manifest_url.to_string(),
        etag,
        last_modified,
    })?;
    atomic_write(&paths.http_state, &state)?;
    let changed = cached_manifest.as_ref().is_none_or(|cached| {
        cached.sequence != manifest.sequence || cached.release_id != manifest.release_id
    });
    Ok(CatalogRefresh {
        release: CatalogToolRelease {
            sequence: manifest.sequence,
            release_id: manifest.release_id,
            model_version_id: manifest.model_version_id,
            compatibility_version_id: manifest.compatibility_version_id,
            delivery_source: manifest_url.to_string(),
            delivery_sources: delivery_sources.to_vec(),
            tool_raw,
        },
        changed,
    })
}

fn release_from_cache(
    paths: &CachePaths,
    manifest: CatalogManifest,
    delivery_sources: Vec<String>,
) -> Result<CatalogToolRelease> {
    let tool_raw = read_cached_component(paths, &manifest.components.tool_metadata)?;
    crate::tool_model_metadata::parse_tool_model_metadata(
        &tool_raw,
        Some(&manifest.compatibility_version_id),
    )?;
    Ok(CatalogToolRelease {
        sequence: manifest.sequence,
        release_id: manifest.release_id,
        model_version_id: manifest.model_version_id,
        compatibility_version_id: manifest.compatibility_version_id,
        delivery_source: "local-cache".to_string(),
        delivery_sources,
        tool_raw,
    })
}

async fn load_or_fetch_component(
    client: &Client,
    paths: &CachePaths,
    manifest_url: &str,
    component_base_url: &str,
    component: &CatalogComponent,
) -> Result<Vec<u8>> {
    if let Ok(raw) = read_cached_component(paths, component) {
        return Ok(raw);
    }
    let mut failures = Vec::new();
    for url in component_candidate_urls(component_base_url, manifest_url, &component.url) {
        let result = async {
            let response = client
                .get(&url)
                .timeout(MANIFEST_SOURCE_TIMEOUT)
                .header(ACCEPT, "application/json")
                .header("User-Agent", "const-api-client/catalog-registry")
                .send()
                .await?;
            validate_final_https(response.url(), "catalog component")?;
            if !response.status().is_success() {
                return Err(anyhow!(
                    "catalog component returned HTTP {}",
                    response.status()
                ));
            }
            let raw = read_response_limited(response, component.size).await?;
            verify_component(component, &raw)?;
            Ok::<Vec<u8>, anyhow::Error>(raw)
        }
        .await;
        match result {
            Ok(raw) => {
                atomic_write(&component_path(paths, component), &raw)?;
                return Ok(raw);
            }
            Err(error) => failures.push(format!("{url}: {error:#}")),
        }
    }
    Err(anyhow!(
        "all catalog component sources failed: {}",
        failures.join("; ")
    ))
}

fn component_candidate_urls(
    component_base_url: &str,
    manifest_url: &str,
    signed_url: &str,
) -> Vec<String> {
    let signed_url = signed_url.trim().to_string();
    let mut candidates = Vec::new();
    if let (Ok(base), Ok(component)) = (Url::parse(component_base_url), Url::parse(&signed_url)) {
        if let Some(filename) = component
            .path_segments()
            .and_then(|mut parts| parts.next_back())
        {
            if let Ok(mirrored) = base.join(filename) {
                let mirrored = mirrored.to_string();
                if mirrored != signed_url {
                    candidates.push(mirrored);
                }
            }
        }
    }
    if candidates.is_empty() {
        if let (Ok(mut manifest), Ok(component)) =
            (Url::parse(manifest_url), Url::parse(&signed_url))
        {
            if let Some(filename) = component
                .path_segments()
                .and_then(|mut parts| parts.next_back())
            {
                let directory = manifest
                    .path()
                    .rsplit_once('/')
                    .map(|(directory, _)| directory)
                    .unwrap_or_default();
                manifest.set_path(&format!("{directory}/{filename}"));
                manifest.set_query(None);
                manifest.set_fragment(None);
                let mirrored = manifest.to_string();
                if mirrored != signed_url {
                    candidates.push(mirrored);
                }
            }
        }
    }
    candidates.push(signed_url);
    candidates
}

fn read_cached_component(paths: &CachePaths, component: &CatalogComponent) -> Result<Vec<u8>> {
    let raw = fs::read(component_path(paths, component))?;
    verify_component(component, &raw)?;
    Ok(raw)
}

fn verify_component(component: &CatalogComponent, raw: &[u8]) -> Result<()> {
    if raw.len() as u64 != component.size {
        return Err(anyhow!("catalog component size mismatch"));
    }
    let actual = hex::encode(Sha256::digest(raw));
    if actual != component.sha256.trim().to_ascii_lowercase() {
        return Err(anyhow!("catalog component SHA-256 mismatch"));
    }
    Ok(())
}

async fn read_response_limited(response: Response, maximum: u64) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > maximum)
    {
        return Err(anyhow!("catalog response exceeds {maximum} bytes"));
    }
    let capacity = response.content_length().unwrap_or_default().min(maximum) as usize;
    let mut raw = Vec::with_capacity(capacity);
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("read catalog response body")?;
        let next_length = raw
            .len()
            .checked_add(chunk.len())
            .context("catalog response size overflow")?;
        if next_length as u64 > maximum {
            return Err(anyhow!("catalog response exceeds {maximum} bytes"));
        }
        raw.extend_from_slice(&chunk);
    }
    Ok(raw)
}

fn verify_manifest(
    raw: &[u8],
    keys: &[PinnedRegistryKey],
    minimum_sequence: i64,
    allow_expired: bool,
) -> Result<CatalogManifest> {
    if raw.is_empty() || raw.len() > MAX_MANIFEST_BYTES {
        return Err(anyhow!("catalog manifest size is invalid"));
    }
    if keys.is_empty() {
        return Err(anyhow!("catalog registry pinned key set is empty"));
    }
    let mut document =
        serde_json::from_slice::<Value>(raw).context("decode catalog registry manifest")?;
    verify_signature(&mut document, keys)?;
    let manifest =
        serde_json::from_slice::<CatalogManifest>(raw).context("decode catalog manifest")?;
    validate_manifest(&manifest, minimum_sequence, allow_expired)?;
    Ok(manifest)
}

fn verify_signature(document: &mut Value, keys: &[PinnedRegistryKey]) -> Result<()> {
    let object = document
        .as_object_mut()
        .ok_or_else(|| anyhow!("catalog manifest must be an object"))?;
    let key_id = object
        .get("key_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| valid_key_id(value))
        .ok_or_else(|| anyhow!("catalog manifest key_id is invalid"))?
        .to_string();
    let encoded = object
        .get("signature")
        .and_then(Value::as_str)
        .and_then(|value| value.strip_prefix("ed25519:"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("catalog manifest requires an Ed25519 signature"))?
        .to_string();
    let pinned = keys
        .iter()
        .find(|key| key.key_id.trim() == key_id)
        .ok_or_else(|| anyhow!("catalog manifest key_id {key_id} is not pinned"))?;
    object.insert("signature".to_string(), Value::String(String::new()));
    let canonical = serde_json::to_vec(document).context("canonicalize catalog manifest")?;
    let signature = Signature::from_slice(&BASE64.decode(encoded)?)
        .context("decode catalog manifest signature")?;
    let public_key = BASE64.decode(pinned.public_key.trim())?;
    let public_key = <[u8; 32]>::try_from(public_key.as_slice())
        .map_err(|_| anyhow!("catalog public key must be 32 bytes"))?;
    VerifyingKey::from_bytes(&public_key)?
        .verify(&canonical, &signature)
        .map_err(|_| anyhow!("catalog manifest signature mismatch"))
}

fn validate_manifest(
    manifest: &CatalogManifest,
    minimum_sequence: i64,
    allow_expired: bool,
) -> Result<()> {
    if manifest.schema_version != MANIFEST_SCHEMA_VERSION {
        return Err(anyhow!(
            "unsupported catalog manifest schema version {}",
            manifest.schema_version
        ));
    }
    if manifest.sequence <= 0 || manifest.sequence < minimum_sequence {
        return Err(anyhow!(
            "catalog rollback rejected: sequence {} is below {}",
            manifest.sequence,
            minimum_sequence
        ));
    }
    if manifest.sequence == packaged_catalog_minimum_sequence()
        && (manifest.release_id != env!("CONST_API_PACKAGED_CATALOG_RELEASE_ID")
            || manifest.model_version_id != env!("CONST_API_PACKAGED_MODEL_VERSION_ID")
            || manifest.compatibility_version_id
                != env!("CONST_API_PACKAGED_COMPATIBILITY_VERSION_ID")
            || manifest.pricing_version_id != env!("CONST_API_PACKAGED_PRICING_VERSION_ID"))
    {
        return Err(anyhow!(
            "catalog sequence {} conflicts with the packaged catalog identity",
            manifest.sequence
        ));
    }
    for (label, value) in [
        ("release_id", manifest.release_id.as_str()),
        ("model_version_id", manifest.model_version_id.as_str()),
        (
            "compatibility_version_id",
            manifest.compatibility_version_id.as_str(),
        ),
        ("pricing_version_id", manifest.pricing_version_id.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(anyhow!("catalog manifest {label} is required"));
        }
    }
    let released_at = OffsetDateTime::parse(manifest.released_at.trim(), &Rfc3339)
        .context("catalog released_at must be RFC3339")?;
    let expires_at = OffsetDateTime::parse(manifest.expires_at.trim(), &Rfc3339)
        .context("catalog expires_at must be RFC3339")?;
    if expires_at <= released_at {
        return Err(anyhow!("catalog expiry must be after release time"));
    }
    if !allow_expired && expires_at.unix_timestamp() <= current_unix_timestamp()? {
        return Err(anyhow!("catalog manifest is expired"));
    }
    let required = Version::parse(manifest.min_client_version.trim())
        .context("catalog minimum client version is invalid")?;
    let current = Version::parse(env!("CARGO_PKG_VERSION")).context("client version is invalid")?;
    if current < required {
        return Err(anyhow!(
            "catalog requires client {required}, current client is {current}"
        ));
    }
    Version::parse(manifest.min_server_version.trim())
        .context("catalog minimum server version is invalid")?;
    validate_component(
        "model_catalog",
        &manifest.components.model_catalog,
        MAX_MODEL_BYTES,
    )?;
    validate_component(
        "pricing_catalog",
        &manifest.components.pricing_catalog,
        MAX_PRICING_BYTES,
    )?;
    validate_component(
        "tool_metadata",
        &manifest.components.tool_metadata,
        MAX_TOOL_BYTES,
    )?;
    Ok(())
}

fn validate_component(label: &str, component: &CatalogComponent, maximum: u64) -> Result<()> {
    if component.size == 0 || component.size > maximum {
        return Err(anyhow!("catalog {label} size is invalid"));
    }
    let digest = component.sha256.trim();
    if digest.len() != 64 || hex::decode(digest).is_err() {
        return Err(anyhow!("catalog {label} SHA-256 is invalid"));
    }
    let url = Url::parse(component.url.trim())?;
    validate_final_https(&url, label)
}

fn validate_final_https(url: &Url, label: &str) -> Result<()> {
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(anyhow!("catalog {label} URL must be credential-free HTTPS"));
    }
    Ok(())
}

fn compiled_catalog_keys() -> Result<Vec<PinnedRegistryKey>> {
    let raw = option_env!("CONST_API_CATALOG_PUBLIC_KEYS")
        .filter(|value| !value.trim().is_empty())
        .or_else(|| option_env!("CONST_API_REGISTRY_PUBLIC_KEYS"))
        .unwrap_or("")
        .trim();
    if raw.is_empty() {
        return Err(anyhow!("catalog registry pinned key set is empty"));
    }
    let keys = serde_json::from_str::<Vec<PinnedRegistryKey>>(raw)
        .context("parse compiled catalog registry keys")?;
    let mut seen = HashSet::new();
    for key in &keys {
        let key_id = key.key_id.trim();
        let decoded = BASE64
            .decode(key.public_key.trim())
            .with_context(|| format!("decode catalog public key {key_id}"))?;
        if !valid_key_id(key_id)
            || !seen.insert(key_id.to_string())
            || decoded.len() != 32
            || BASE64.encode(decoded) != key.public_key.trim()
        {
            return Err(anyhow!("compiled catalog public key set is invalid"));
        }
    }
    if keys.is_empty() {
        return Err(anyhow!("catalog registry pinned key set is empty"));
    }
    Ok(keys)
}

fn valid_key_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn cache_paths(config_path: &Path) -> Result<CachePaths> {
    let parent = config_path
        .parent()
        .context("client config path has no parent")?;
    let root = parent.join("catalog-registry");
    Ok(CachePaths {
        manifest: root.join("manifest.json"),
        http_state: root.join("http-state.json"),
        components: root.join("components"),
    })
}

fn component_path(paths: &CachePaths, component: &CatalogComponent) -> PathBuf {
    paths.components.join(format!(
        "{}.json",
        component.sha256.trim().to_ascii_lowercase()
    ))
}

fn atomic_write(path: &Path, raw: &[u8]) -> Result<()> {
    if fs::read(path).ok().as_deref() == Some(raw) {
        return Ok(());
    }
    let parent = path.parent().context("catalog cache path has no parent")?;
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(raw)?;
    temp.flush()?;
    temp.as_file().sync_all()?;
    temp.persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("replace catalog cache {}", path.display()))?;
    Ok(())
}

fn current_unix_timestamp() -> Result<i64> {
    let value = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_secs();
    i64::try_from(value).context("system time exceeds supported range")
}

fn packaged_catalog_minimum_sequence() -> i64 {
    env!("CONST_API_PACKAGED_CATALOG_SEQUENCE")
        .parse::<i64>()
        .expect("build script must provide a positive packaged catalog sequence")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::json;

    fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&[23_u8; 32])
    }

    fn pinned_key() -> PinnedRegistryKey {
        PinnedRegistryKey {
            key_id: "catalog-test".to_string(),
            public_key: BASE64.encode(signing_key().verifying_key().as_bytes()),
        }
    }

    fn component(url: &str, raw: &[u8]) -> Value {
        json!({
            "url": url,
            "sha256": hex::encode(Sha256::digest(raw)),
            "size": raw.len(),
        })
    }

    fn signed_manifest(sequence: i64, expires_at: &str, tool_raw: &[u8]) -> Vec<u8> {
        let mut value = json!({
            "schema_version": 1,
            "sequence": sequence,
            "release_id": format!("release-{sequence}"),
            "released_at": "2026-07-25T00:00:00Z",
            "expires_at": expires_at,
            "min_client_version": "0.1.0",
            "min_server_version": "0.1.0",
            "model_version_id": format!("models-{sequence}"),
            "compatibility_version_id": "compat-v10-2026-07-27",
            "pricing_version_id": format!("pricing-{sequence}"),
            "components": {
                "model_catalog": component("https://cdn.example/models.json", b"models"),
                "pricing_catalog": component("https://cdn.example/pricing.json", b"pricing"),
                "tool_metadata": component("https://cdn.example/tools.json", tool_raw),
            },
            "key_id": "catalog-test",
            "signature": "",
        });
        let canonical = serde_json::to_vec(&value).unwrap();
        let signature = signing_key().sign(&canonical);
        value["signature"] =
            Value::String(format!("ed25519:{}", BASE64.encode(signature.to_bytes())));
        serde_json::to_vec(&value).unwrap()
    }

    #[test]
    fn refresh_jitter_stays_within_twenty_percent() {
        let base = Duration::from_secs(1_800);
        for _ in 0..100 {
            let value = catalog_refresh_jitter(base);
            assert!(value >= Duration::from_secs(1_440));
            assert!(value <= Duration::from_secs(2_160));
        }
    }

    #[test]
    fn packaged_tool_metadata_passes_strict_parser() {
        let raw = include_bytes!("../resources/tool-model-metadata.json");
        crate::tool_model_metadata::parse_tool_model_metadata(raw, None)
            .expect("packaged metadata");
    }

    #[test]
    fn packaged_catalog_sequence_is_a_positive_rollback_floor() {
        assert!(packaged_catalog_minimum_sequence() > 0);
    }

    #[test]
    fn packaged_sequence_rejects_a_different_published_identity() {
        let tool_raw = include_bytes!("../resources/tool-model-metadata.json");
        let sequence = packaged_catalog_minimum_sequence();
        let raw = signed_manifest(sequence, "2030-01-01T00:00:00Z", tool_raw);
        let error = verify_manifest(&raw, &[pinned_key()], sequence, false).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("conflicts with the packaged catalog identity")
        );
    }

    #[test]
    fn signed_manifest_rejects_tampering_and_rollback() {
        let tool_raw = include_bytes!("../resources/tool-model-metadata.json");
        let raw = signed_manifest(9, "2030-01-01T00:00:00Z", tool_raw);
        let verified = verify_manifest(&raw, &[pinned_key()], 9, false).unwrap();
        assert_eq!(verified.release_id, "release-9");

        let mut tampered = serde_json::from_slice::<Value>(&raw).unwrap();
        tampered["release_id"] = Value::String("tampered".to_string());
        assert!(
            verify_manifest(
                &serde_json::to_vec(&tampered).unwrap(),
                &[pinned_key()],
                9,
                false
            )
            .unwrap_err()
            .to_string()
            .contains("signature mismatch")
        );
        assert!(
            verify_manifest(&raw, &[pinned_key()], 10, false)
                .unwrap_err()
                .to_string()
                .contains("rollback")
        );
    }

    #[test]
    fn expired_signed_cache_is_lkg_only() {
        let tool_raw = include_bytes!("../resources/tool-model-metadata.json");
        let raw = signed_manifest(9, "2026-07-26T00:00:00Z", tool_raw);
        assert!(verify_manifest(&raw, &[pinned_key()], 0, false).is_err());
        assert!(verify_manifest(&raw, &[pinned_key()], 0, true).is_ok());
    }

    #[test]
    fn component_mirror_follows_the_verified_manifest_source() {
        let sources = crate::release_sources::bundled_release_sources()
            .unwrap()
            .catalog_sources();
        let candidates = component_candidate_urls(
            &sources[0].component_base_url,
            &sources[0].manifest_url,
            "https://github.com/llxisdsh/const-api-public/releases/download/catalog-registry/tools-deadbeef.json",
        );
        assert_eq!(
            candidates,
            vec![
                "https://const.tos-cn-shanghai.volces.com/catalog-registry/tools-deadbeef.json",
                "https://github.com/llxisdsh/const-api-public/releases/download/catalog-registry/tools-deadbeef.json",
            ]
        );
    }
}
