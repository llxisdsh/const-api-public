use crate::config::*;
use crate::model::*;
use crate::upstream_transport::AdaptiveRequestBuilderExt;
use crate::*;
use anyhow::{Context, Result, anyhow};
use base64::{
    Engine as _,
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
};
use rand::TryRng;
use reqwest::{
    Client, Url,
    header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT},
};
use sha2::Digest;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

mod capability_check;
mod check_deadline;
#[cfg(test)]
mod protocol_check_tests;
#[cfg(test)]
mod quota_tests;
#[cfg(test)]
mod refresh_tests;
pub(crate) mod responses_features;
mod surface_families;
mod tool_probe;
use surface_families::*;
mod quota_refresh;
pub(crate) use quota_refresh::quota_response_json;
use quota_refresh::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SurfaceProbeMode {
    Creation,
    Snapshot,
}

#[derive(Debug, Clone)]
pub(crate) struct SurfaceProbeContext {
    pub(crate) channel_id: String,
    pub(crate) source_driver: crate::source_driver::SourceDriverId,
    pub(crate) execution_kind: crate::source_driver::ExecutionKind,
    pub(crate) credential_ref: String,
    pub(crate) binding: ChannelSurfaceBinding,
    pub(crate) discovery: DiscoveryProfile,
    pub(crate) user_agent_profile: String,
    pub(crate) mode: SurfaceProbeMode,
    pub(crate) is_default_target: bool,
    pub(crate) run_advanced: bool,
    pub(crate) models: ModelSnapshot,
    pub(crate) default_model: String,
    pub(crate) subscription: Option<SubscriptionConfigV2>,
}

#[derive(Debug, Clone)]
struct ProbeObservation {
    models: Vec<String>,
    native_protocol_verified: bool,
    quota_status: String,
    remaining_ratio: f64,
    quota_windows: Vec<QuotaWindow>,
    checks: Vec<String>,
    warnings: Vec<String>,
    capability_profiles: Vec<ChannelCapabilityProfile>,
    detection_checks: Vec<ChannelDetectionCheck>,
}

type SubscriptionRefreshLock = std::sync::Arc<tokio::sync::Mutex<()>>;

static SUBSCRIPTION_REFRESH_LOCKS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<PathBuf, SubscriptionRefreshLock>>,
> = std::sync::OnceLock::new();
static SUBSCRIPTION_REFRESH_TOKEN_LOCKS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>>,
> = std::sync::OnceLock::new();
const CLAUDE_REFRESH_RESULT_CACHE_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Clone)]
struct ClaudeRefreshResultCacheEntry {
    token_material: serde_json::Value,
    refreshed_at: Instant,
}

static CLAUDE_REFRESH_RESULT_CACHE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, ClaudeRefreshResultCacheEntry>>,
> = std::sync::OnceLock::new();
const OPENAI_CREDENTIAL_READ_CACHE_TTL: Duration = Duration::from_secs(1);

#[derive(Clone)]
struct OpenAiCredentialReadCacheEntry {
    credential: serde_json::Value,
    loaded_at: Instant,
}

static OPENAI_CREDENTIAL_READ_CACHE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<PathBuf, OpenAiCredentialReadCacheEntry>>,
> = std::sync::OnceLock::new();
const SUBSCRIPTION_OAUTH_HTTP_TIMEOUT: Duration = Duration::from_secs(30);
// Periodic channel snapshots run every 30 seconds, but subscription control-plane
// model discovery follows these independent cadences. Usage queries have their
// own shared cache in quota_refresh and never perform paid model interactions.
const OPENAI_MODEL_CATALOG_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);
const CLAUDE_MODEL_CATALOG_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);
const GROK_MODEL_CATALOG_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubscriptionCatalogPolicy {
    Fresh,
    Cached,
}

fn subscription_refresh_due(
    policy: SubscriptionCatalogPolicy,
    refresh_after: Option<Instant>,
    now: Instant,
) -> bool {
    policy == SubscriptionCatalogPolicy::Fresh
        || refresh_after.is_none_or(|refresh_after| now >= refresh_after)
}

#[derive(Clone)]
struct OpenAiSubscriptionCatalogCacheEntry {
    models: Vec<crate::model_catalog::ModelObservation>,
    model_refresh_after: Instant,
    model_etag: Option<String>,
}

type OpenAiSubscriptionCatalogCacheSlot =
    std::sync::Arc<tokio::sync::Mutex<Option<OpenAiSubscriptionCatalogCacheEntry>>>;

static OPENAI_SUBSCRIPTION_CATALOG_CACHE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, OpenAiSubscriptionCatalogCacheSlot>>,
> = std::sync::OnceLock::new();

// A concrete provider rejection is stronger evidence than a possibly stale
// model-manifest flag. Keep the negative observation for this client process;
// the first affected request performs one safe dialect fallback, while later
// requests avoid paying for the rejected probe again.
static OPENAI_RESPONSES_LITE_REJECTIONS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashSet<String>>,
> = std::sync::OnceLock::new();

#[derive(Clone)]
struct ClaudeSubscriptionCatalogCacheEntry {
    models: Vec<crate::model_catalog::ModelObservation>,
    model_refresh_after: Instant,
}

type ClaudeSubscriptionCatalogCacheSlot =
    std::sync::Arc<tokio::sync::Mutex<Option<ClaudeSubscriptionCatalogCacheEntry>>>;

static CLAUDE_SUBSCRIPTION_CATALOG_CACHE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, ClaudeSubscriptionCatalogCacheSlot>>,
> = std::sync::OnceLock::new();

#[derive(Clone)]
struct GrokSubscriptionCatalogCacheEntry {
    models: Vec<crate::model_catalog::ModelObservation>,
    model_refresh_after: Instant,
}

type GrokSubscriptionCatalogCacheSlot =
    std::sync::Arc<tokio::sync::Mutex<Option<GrokSubscriptionCatalogCacheEntry>>>;

static GROK_SUBSCRIPTION_CATALOG_CACHE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, GrokSubscriptionCatalogCacheSlot>>,
> = std::sync::OnceLock::new();

static OPENAI_MODEL_ETAG_HINTS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, (String, Instant)>>,
> = std::sync::OnceLock::new();

fn subscription_refresh_lock(path: &Path) -> Result<SubscriptionRefreshLock> {
    let mut locks = SUBSCRIPTION_REFRESH_LOCKS
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .map_err(|_| anyhow!("subscription refresh lock registry poisoned"))?;
    Ok(locks
        .entry(path.to_path_buf())
        .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
        .clone())
}

fn subscription_refresh_token_key(provider: &str, refresh_token: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(provider.trim().as_bytes());
    digest.update(b"\0");
    digest.update(refresh_token.trim().as_bytes());
    hex::encode(digest.finalize())
}

fn subscription_refresh_token_lock(key: &str) -> SubscriptionRefreshLock {
    let mut locks = SUBSCRIPTION_REFRESH_TOKEN_LOCKS
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(key).and_then(std::sync::Weak::upgrade) {
        return lock;
    }
    let lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));
    locks.insert(key.to_string(), std::sync::Arc::downgrade(&lock));
    lock
}

fn cached_claude_refresh_result(key: &str) -> Option<serde_json::Value> {
    let mut cache = CLAUDE_REFRESH_RESULT_CACHE
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.retain(|_, entry| entry.refreshed_at.elapsed() < CLAUDE_REFRESH_RESULT_CACHE_TTL);
    cache.get(key).map(|entry| entry.token_material.clone())
}

fn remember_claude_refresh_result(key: String, token_material: serde_json::Value) {
    let mut cache = CLAUDE_REFRESH_RESULT_CACHE
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.retain(|_, entry| entry.refreshed_at.elapsed() < CLAUDE_REFRESH_RESULT_CACHE_TTL);
    cache.insert(
        key,
        ClaudeRefreshResultCacheEntry {
            token_material,
            refreshed_at: Instant::now(),
        },
    );
}

fn read_subscription_credential(path: &Path) -> Result<serde_json::Value> {
    serde_json::from_slice(
        &fs::read(path)
            .with_context(|| format!("read credential file {}", path.to_string_lossy()))?,
    )
    .map_err(Into::into)
}

fn read_openai_subscription_credential_cached(path: &Path) -> Result<serde_json::Value> {
    let mut cache = OPENAI_CREDENTIAL_READ_CACHE
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .map_err(|_| anyhow!("OpenAI credential read cache poisoned"))?;
    if let Some(entry) = cache.get(path) {
        if entry.loaded_at.elapsed() < OPENAI_CREDENTIAL_READ_CACHE_TTL {
            return Ok(entry.credential.clone());
        }
    }
    let credential = read_subscription_credential(path)?;
    cache.insert(
        path.to_path_buf(),
        OpenAiCredentialReadCacheEntry {
            credential: credential.clone(),
            loaded_at: Instant::now(),
        },
    );
    Ok(credential)
}

fn remember_openai_subscription_credential(path: &Path, credential: &serde_json::Value) {
    if let Ok(mut cache) = OPENAI_CREDENTIAL_READ_CACHE
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
    {
        cache.insert(
            path.to_path_buf(),
            OpenAiCredentialReadCacheEntry {
                credential: credential.clone(),
                loaded_at: Instant::now(),
            },
        );
    }
}

pub(crate) fn write_subscription_credential_atomically(
    path: &Path,
    credential: &serde_json::Value,
) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("credential path {} has no parent", path.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create credential directory {}", parent.display()))?;
    let payload = serde_json::to_vec_pretty(credential)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("create credential temp file beside {}", path.display()))?;
    temp.write_all(&payload)
        .with_context(|| format!("write credential temp file for {}", path.display()))?;
    temp.write_all(b"\n")?;
    temp.flush()
        .with_context(|| format!("flush credential temp file for {}", path.display()))?;
    temp.as_file()
        .sync_all()
        .with_context(|| format!("sync credential temp file for {}", path.display()))?;
    temp.persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("replace credential {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("protect credential {}", path.display()))?;
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .with_context(|| format!("sync credential directory {}", parent.display()))?;
    }
    remember_openai_subscription_credential(path, credential);
    Ok(())
}

pub(crate) fn surface_probe_contexts(
    channel: ChannelConfig,
    mode: SurfaceProbeMode,
) -> Result<Vec<SurfaceProbeContext>> {
    let channel = normalize_probe_channel(channel);
    let driver = crate::source_driver::source_driver(channel.source_driver())?;
    let execution_kind = driver.execution_kind();
    let bindings = crate::channel_surface::normalize_channel_surface_bindings(&channel);
    if bindings.is_empty() {
        return Err(anyhow!("channel has no API surface bindings to probe"));
    }
    let is_subscription = !matches!(
        execution_kind,
        crate::source_driver::ExecutionKind::HttpSurface
    );
    let subscription = is_subscription.then(|| channel.subscription.clone().into());
    let default_target = channel.v2.default_target.clone();
    let mut contexts = Vec::new();
    for binding in bindings {
        for protocol in &binding.protocols {
            let parsed = crate::protocol::kind::ProtocolKind::parse(&protocol.protocol)?;
            let is_default_target =
                binding.surface == default_target.surface && parsed == default_target.protocol;
            let mut pair_binding = binding.clone();
            pair_binding.protocols = vec![protocol.clone()];
            // This is a one-protocol probe projection, not the user's binding.
            pair_binding.protocols[0].preferred = true;
            contexts.push(SurfaceProbeContext {
                channel_id: channel.id.clone(),
                source_driver: channel.source_driver(),
                execution_kind,
                credential_ref: channel.v2.credential_ref.clone(),
                binding: pair_binding,
                discovery: channel.v2.discovery.clone(),
                user_agent_profile: channel.v2.user_agent_profile.clone(),
                mode,
                is_default_target,
                run_advanced: mode == SurfaceProbeMode::Creation
                    && is_default_target
                    && driver.creation.advanced_conversational
                        == crate::source_driver::AdvancedVerificationPolicy::DriverPreferredOnly,
                models: channel.models.clone(),
                default_model: channel.default_model_from(&channel.models),
                subscription: subscription.clone(),
            });
        }
    }
    Ok(contexts)
}

pub(crate) fn with_channel_check_deadline<F: std::future::Future>(
    duration: Duration,
    future: F,
) -> impl std::future::Future<Output = F::Output> {
    check_deadline::scope(duration, future)
}

pub(crate) fn channel_check_deadline() -> Option<tokio::time::Instant> {
    check_deadline::current()
}

pub(crate) async fn detect_channel(channel: ChannelConfig) -> Result<ChannelDetectionResult> {
    // Discovery and inference remain separate entry points. Full checks first collect
    // the current directory, then choose representative targets independently of an
    // expensive, unavailable, or automatic configured default.
    let mut metadata = check_deadline::run(refresh_channel(channel.clone())).await?;
    let mut inspected = channel.clone();
    crate::supplier::merge_channel_detection_result(&mut inspected, &metadata);
    if crate::supplier::channel_probe_quota_reserved(&inspected, &metadata.quota_windows)? {
        metadata
            .checks
            .push("live interaction skipped because quota reserve is active".into());
        return Ok(metadata);
    }
    let report = Box::pin(crate::supplier::availability::check(&inspected)).await;
    let mut result = if crate::supplier::availability::is_automatic_recheck() {
        // An already verified route needs only the due basic representative, not
        // repeated protocol/tool probes or unrelated successful model groups.
        metadata
    } else if report.groups.is_empty() {
        // No text representatives: retain the existing dedicated operation check.
        detect_channel_protocols(inspected).await?
    } else if let Some(test) = report
        .test
        .as_ref()
        .filter(|test| crate::supplier::availability::allows(&inspected, &test.model))
    {
        inspected.upstream_model = test.model.clone();
        inspected.public_model = test.model.clone();
        let (protocols, attempts) =
            crate::upstream_transport::with_probe_budget(12, detect_channel_protocols(inspected))
                .await;
        match protocols {
            Ok(mut result) => {
                result.checks.push(format!(
                    "protocol/capability inference attempts: {attempts}"
                ));
                result
            }
            Err(error) => {
                // Feature/default-protocol failure must not discard the other groups.
                let message = error.to_string();
                metadata.warnings.push(format!(
                    "protocol verification incomplete: {}",
                    crate::upstream_failure::safe_excerpt(&message)
                ));
                metadata
            }
        }
    } else {
        metadata
    };
    // Metadata refresh carries the previous complete evidence snapshot. Replace
    // checked groups, otherwise partial/automatic checks would show duplicate rows.
    result.detection_evidence.retain(|existing| {
        existing.name != "availability_group"
            || !report
                .groups
                .iter()
                .any(|group| existing.capability == group.group)
    });
    for group in &report.groups {
        let message = format!(
            "{}: {} — {} ({} attempts){}{}",
            group.group,
            group.representative,
            group.status,
            group.attempts,
            if group.message.is_empty() { "" } else { ": " },
            group.message
        );
        result.checks.push(message.clone());
        result
            .detection_evidence
            .push(crate::model::ChannelDetectionCheck {
                name: "availability_group".into(),
                status: group.status.clone(),
                checked_at_unix: group.checked_at,
                capability: group.group.clone(),
                message,
                ..Default::default()
            });
    }
    result.availability = Some(report);
    Ok(result)
}

async fn detect_channel_protocols(channel: ChannelConfig) -> Result<ChannelDetectionResult> {
    check_deadline::scope(
        Duration::from_secs(120),
        detect_channel_protocols_inner(channel),
    )
    .await
}

async fn detect_channel_protocols_inner(channel: ChannelConfig) -> Result<ChannelDetectionResult> {
    let contexts = surface_probe_contexts(channel.clone(), SurfaceProbeMode::Creation)?;
    let mut results = Vec::with_capacity(contexts.len());
    let mut inspected_surfaces = std::collections::HashSet::new();
    for context in contexts {
        let inspect_operation_families = inspected_surfaces.insert(context.binding.surface);
        match Box::pin(detect_surface(
            context.clone(),
            inspect_operation_families,
            &channel.capability_profiles,
        ))
        .await
        {
            Ok(result) => results.push(result),
            Err(error) => {
                let mut result = rejected_surface_probe_result(&context, &error)?;
                if check_deadline::incomplete(&error) {
                    let mut detail = format!("{error:#}");
                    for secret in [
                        &channel.upstream_api_key,
                        &channel.v2.credential_ref,
                        &context.credential_ref,
                    ] {
                        if !secret.is_empty() {
                            detail = detail.replace(secret, "[redacted]");
                        }
                    }
                    result.result.warnings = vec![format!(
                        "{} verification incomplete: {}",
                        result.result.protocol.as_str(),
                        crate::upstream_failure::safe_excerpt(&detail),
                    )];
                    // No negative protocol evidence was obtained. Keep the saved
                    // state for this surface, including unverified state on new channels.
                    result.result.surface_verification = context.binding.verification.clone();
                    result.result.protocol_verification =
                        context.binding.protocols[0].verification.clone();
                    result.result.models = context.models.clone();
                    result.result.model_capability_evidence = channel
                        .capability_profiles
                        .iter()
                        .filter(|profile| profile.protocol == result.result.protocol.as_str())
                        .cloned()
                        .collect();
                }
                results.push(result);
            }
        }
    }
    merge_surface_detection_results(results, false)
}

async fn detect_surface(
    context: SurfaceProbeContext,
    inspect_operation_families: bool,
    previous_profiles: &[ChannelCapabilityProfile],
) -> Result<SurfaceDetectionOutcome> {
    let driver = crate::source_driver::source_driver(context.source_driver)?;
    if driver.execution_kind() != context.execution_kind {
        return Err(anyhow!(
            "surface probe execution kind does not match source driver {:?}",
            context.source_driver
        ));
    }
    if context.discovery.strategy.trim().is_empty() {
        return Err(anyhow!("surface probe discovery policy is required"));
    }
    let mut projection = probe_channel_from_context(&context)?;
    // Preserve only this protocol's saved evidence. Feature probes use it only
    // when their own step was incomplete, never as proof for a different model.
    projection.capability_profiles = previous_profiles
        .iter()
        .filter(|profile| profile.protocol == context.binding.protocols[0].protocol)
        .cloned()
        .collect();
    let detection = detect_channel_single(
        projection.clone(),
        context.mode == SurfaceProbeMode::Creation && context.run_advanced,
        if context.mode == SurfaceProbeMode::Snapshot {
            SubscriptionCatalogPolicy::Cached
        } else {
            SubscriptionCatalogPolicy::Fresh
        },
    );
    let mut observation =
        if context.execution_kind == crate::source_driver::ExecutionKind::HttpSurface {
            detection.await?
        } else {
            check_deadline::run(detection).await?
        };
    // Secondary Responses surfaces need their own Codex evidence even when Chat
    // is the default. Never run paid feature checks during periodic snapshots.
    if context.mode == SurfaceProbeMode::Creation
        && context.execution_kind == crate::source_driver::ExecutionKind::HttpSurface
        && probe_protocol(&context)? == crate::protocol::kind::ProtocolKind::OpenAiResponses
        && observation.native_protocol_verified
    {
        // The basic request just succeeded. A stale saved rejection must not
        // prevent feature verification during this same recovery check.
        for binding in &mut projection.surface_bindings {
            for protocol in &mut binding.protocols {
                protocol.verification.state = "verified".into();
            }
        }
        crate::openrouter::prepare_probe_channel(&mut projection);
        responses_features::enrich(short_http_client(), &projection, &mut observation).await;
    }
    if context.mode == SurfaceProbeMode::Creation && inspect_operation_families {
        match check_deadline::observe(surface_operation_family_evidence(
            short_http_client(),
            &context,
        ))
        .await
        {
            Ok(checks) => observation.detection_checks.extend(checks),
            Err(error) => observation.warnings.push(error.to_string()),
        }
    }
    surface_probe_result_from_observation(&context, observation)
}

async fn detect_channel_single(
    channel: ChannelConfig,
    run_advanced: bool,
    subscription_catalog_policy: SubscriptionCatalogPolicy,
) -> Result<ProbeObservation> {
    let mut channel = normalize_probe_channel(channel);
    crate::openrouter::prepare_probe_channel(&mut channel);
    let client = short_http_client().clone();
    let mut checks = Vec::new();
    let mut warnings = Vec::new();

    if channel.kind.trim() == "subscription_adapter" {
        let platform = channel.subscription.platform.trim();
        let refreshed;
        let mut quota_status = "unknown".to_string();
        let mut remaining_ratio = 0.0;
        let mut quota_windows = Vec::new();
        let (models, model_observations) = if platform == "openai"
            && !channel.subscription.credential_ref.trim().is_empty()
        {
            let (observations, did_refresh, windows) =
                fetch_openai_subscription_catalog_with_policy(
                    &client,
                    &channel,
                    subscription_catalog_policy,
                )
                .await?;
            refreshed = did_refresh;
            checks.push("models: OpenAI account subscription catalog ok".to_string());
            quota_windows = windows;
            if let Some((ratio, status)) = quota_snapshot_from_windows(&quota_windows) {
                remaining_ratio = ratio;
                quota_status = status;
                checks.push(format!(
                    "quota: status={}, remaining_ratio={:.3}",
                    quota_status, remaining_ratio
                ));
                checks.push(format!("quota_windows: {}", quota_windows.len()));
            }
            (model_ids(observations.clone()), observations)
        } else if platform == "antigravity"
            && !channel.subscription.credential_ref.trim().is_empty()
        {
            let health = check_antigravity_subscription_with_policy(
                &client,
                &channel,
                subscription_catalog_policy,
            )
            .await?;
            refreshed = health.refreshed;
            quota_status = health.quota_status.clone();
            remaining_ratio = health.remaining_ratio;
            quota_windows = health.quota_windows.clone();
            warnings.extend(health.warnings.clone());
            checks.push(format!("credential: {}", health.summary));
            checks.push(format!("oauth_type: {}", health.oauth_type));
            if !health.project_id.trim().is_empty() {
                checks.push(format!("project_id: {}", health.project_id));
            }
            if !health.tier_id.trim().is_empty() {
                checks.push(format!("tier_id: {}", health.tier_id));
            }
            checks.push(format!(
                "quota: status={}, remaining_ratio={:.3}",
                health.quota_status, health.remaining_ratio
            ));
            if !health.quota_windows.is_empty() {
                checks.push(format!("quota_windows: {}", health.quota_windows.len()));
            }
            checks.push(format!(
                "models: Antigravity catalog loaded ({} models)",
                health.models.len()
            ));
            let observations = health.model_observations;
            (health.models, observations)
        } else if platform == "claude" && !channel.subscription.credential_ref.trim().is_empty() {
            let (observations, did_refresh, windows, claude_warnings) =
                fetch_claude_subscription_catalog_with_policy(
                    &client,
                    &channel,
                    subscription_catalog_policy,
                )
                .await?;
            refreshed = did_refresh;
            checks.push("models: Claude account catalog ok".to_string());
            warnings.extend(claude_warnings);
            quota_windows = windows;
            if let Some((ratio, status)) = quota_snapshot_from_windows(&quota_windows) {
                remaining_ratio = ratio;
                quota_status = status;
            }
            if !quota_windows.is_empty() {
                checks.push(format!(
                    "quota: Claude OAuth usage loaded ({} windows)",
                    quota_windows.len()
                ));
            }
            (model_ids(observations.clone()), observations)
        } else if platform == "grok" && !channel.subscription.credential_ref.trim().is_empty() {
            let (observations, catalog_refreshed, grok_warnings) =
                fetch_grok_subscription_catalog_with_policy(
                    &client,
                    &channel,
                    subscription_catalog_policy,
                )
                .await?;
            warnings.extend(grok_warnings);
            let models = model_ids(observations.clone());
            checks.push(format!(
                "models: Grok account catalog loaded ({} models)",
                models.len()
            ));
            let mut quota_refreshed = false;
            match refresh_subscription_quota(&channel, subscription_catalog_policy, async {
                let (token, _credential, did_refresh) =
                    ensure_grok_subscription_access_token(&client, &channel).await?;
                quota_refreshed = did_refresh;
                fetch_grok_subscription_quota_windows(&client, &token).await
            })
            .await
            {
                Ok(windows) => {
                    quota_windows = windows;
                    if let Some((ratio, status)) = quota_snapshot_from_windows(&quota_windows) {
                        remaining_ratio = ratio;
                        quota_status = status;
                    }
                    checks.push(format!(
                        "quota: Grok billing loaded ({} windows)",
                        quota_windows.len()
                    ));
                }
                Err(error) => warnings.push(format!(
                    "Grok billing unavailable; inference probe will decide availability: {error}"
                )),
            }
            refreshed = catalog_refreshed || quota_refreshed;
            (models, observations)
        } else if platform == "gemini" {
            return Err(anyhow!(
                "Gemini Code Assist consumer subscriptions are no longer supported; remove the old channel and connect Antigravity."
            ));
        } else {
            return Err(anyhow!("subscription credential reference is required"));
        };
        if models.is_empty() {
            return Err(anyhow!("subscription model catalog returned no models"));
        }
        checks.push("subscription adapter credential and model catalog verified".to_string());
        if refreshed {
            checks.push("credential: access token refreshed locally".to_string());
        }
        checks.push(format!(
            "local concurrency limit={}",
            channel.max_concurrency()
        ));
        if channel.subscription.credential_ref.trim().is_empty() {
            warnings
                .push("credential reference is empty; concrete executor is not ready".to_string());
        }
        let native_protocol = match platform_or_default(platform) {
            "openai" => "openai_responses",
            "claude" => "anthropic_messages",
            "antigravity" => "gemini_native",
            "grok" => "openai_responses",
            _ => "",
        };
        let capability_profiles = if native_protocol.is_empty() {
            Vec::new()
        } else {
            let mut profile = basic_protocol_capability(native_protocol, "declared");
            profile.capability_layer = "driver".to_string();
            profile.release_status = "supported".to_string();
            profile.stream_sse = true;
            profile.tool_calls = true;
            profile.tool_choice = true;
            apply_subscription_driver_capabilities(native_protocol, &mut profile);
            let mut profiles = vec![profile.clone()];
            profiles.extend(model_modality_capability_profiles(
                &profile,
                &model_observations,
                matches!(native_protocol, "openai_responses" | "gemini_native"),
            ));
            let model_profile_count = profiles.len().saturating_sub(1);
            if model_profile_count > 0 {
                checks.push(format!(
                    "media capabilities: {model_profile_count} model-scoped catalog profiles"
                ));
            }
            profiles
        };
        return Ok(ProbeObservation {
            models,
            native_protocol_verified: false,
            quota_status,
            remaining_ratio,
            quota_windows,
            checks,
            warnings,
            capability_profiles,
            detection_checks: Vec::new(),
        });
    }

    if channel.upstream_base_url.trim().is_empty() {
        return Err(anyhow!("upstream base url is required"));
    }

    let provider_hint = channel_provider_hint(&channel);
    match provider_hint.as_str() {
        "anthropic" => {
            detect_anthropic_channel(&client, &channel, checks, warnings, run_advanced).await
        }
        "gemini" => detect_gemini_channel(&client, &channel, checks, warnings, run_advanced).await,
        "local_model" => {
            detect_local_model_channel(&client, &channel, checks, warnings, run_advanced).await
        }
        _ => detect_openai_family_channel(&client, &channel, checks, warnings, run_advanced).await,
    }
}

#[derive(Debug)]
struct SurfaceDetectionOutcome {
    result: SurfaceProbeResult,
    quota_status: String,
    remaining_ratio: f64,
    quota_windows: Vec<QuotaWindow>,
}

fn probe_protocol(context: &SurfaceProbeContext) -> Result<crate::protocol::kind::ProtocolKind> {
    if context.binding.protocols.len() != 1 {
        return Err(anyhow!(
            "surface probe binding must contain exactly one protocol"
        ));
    }
    Ok(crate::protocol::kind::ProtocolKind::parse(
        &context.binding.protocols[0].protocol,
    )?)
}

fn probe_channel_from_context(context: &SurfaceProbeContext) -> Result<ChannelConfig> {
    let protocol = probe_protocol(context)?;
    let driver = crate::source_driver::source_driver(context.source_driver)?;
    let mut channel = channel_from_supplier(context.channel_id.clone(), &default_supplier_config());
    channel.v2 = channel_v2_contract_for_source(context.source_driver);
    channel.v2.credential_ref = context.credential_ref.clone();
    channel.v2.default_target = ChannelTarget {
        surface: context.binding.surface,
        protocol,
    };
    channel.v2.discovery = context.discovery.clone();
    channel.v2.user_agent_profile = context.user_agent_profile.clone();
    channel.surface_bindings = vec![context.binding.clone()];
    channel.models = context.models.clone();
    channel.upstream_model = context.default_model.clone();
    channel.public_model = context.default_model.clone();
    if let Some(subscription) = context.subscription.clone() {
        channel.subscription = subscription.into();
    }
    channel.subscription.credential_ref = context.credential_ref.clone();
    channel.name = driver.title.clone();
    Ok(project_channel_legacy_fields(channel))
}

fn surface_probe_result_from_observation(
    context: &SurfaceProbeContext,
    mut observation: ProbeObservation,
) -> Result<SurfaceDetectionOutcome> {
    if context.source_driver == crate::source_driver::SourceDriverId::Openrouter {
        crate::openrouter::normalize_catalog_projection(
            &mut observation.models,
            &mut observation.capability_profiles,
        );
    }
    let protocol = probe_protocol(context)?;
    let protocol_name = protocol.as_str();
    let mut evidence = observation
        .capability_profiles
        .into_iter()
        .filter(|profile| profile.protocol == protocol_name)
        .collect::<Vec<_>>();
    if evidence.is_empty() {
        evidence.push(basic_protocol_capability(protocol_name, "verified"));
    }
    let checked_at_unix = now_unix();
    let protocol_verification = if observation.native_protocol_verified {
        ProtocolVerification {
            state: "verified".to_string(),
            checked_at_unix,
            summary: "native protocol execution verified".to_string(),
        }
    } else {
        context.binding.protocols[0].verification.clone()
    };
    let surface_summary =
        if observation.detection_checks.iter().any(|check| {
            check.name == "surface_operation_family" && check.status == "driver_contract"
        }) {
            "model catalog verified; official operation contract declared"
        } else if observation
            .detection_checks
            .iter()
            .any(|check| check.name == "surface_operation_family")
        {
            "model catalog verified; operation families checked independently"
        } else {
            "model catalog verified"
        };
    Ok(SurfaceDetectionOutcome {
        result: SurfaceProbeResult {
            surface: context.binding.surface,
            protocol,
            surface_verification: SurfaceVerification {
                state: "verified".to_string(),
                checked_at_unix,
                summary: surface_summary.to_string(),
            },
            protocol_verification,
            models: dedupe_models(observation.models),
            model_capability_evidence: evidence,
            detection_evidence: observation.detection_checks,
            checks: observation.checks,
            warnings: observation.warnings,
        },
        quota_status: observation.quota_status,
        remaining_ratio: observation.remaining_ratio,
        quota_windows: observation.quota_windows,
    })
}

fn rejected_surface_probe_result(
    context: &SurfaceProbeContext,
    error: &anyhow::Error,
) -> Result<SurfaceDetectionOutcome> {
    let protocol = probe_protocol(context)?;
    let message = error.to_string();
    let checked_at_unix = now_unix();
    Ok(SurfaceDetectionOutcome {
        result: SurfaceProbeResult {
            surface: context.binding.surface,
            protocol,
            surface_verification: SurfaceVerification {
                state: "rejected".to_string(),
                checked_at_unix,
                summary: message.clone(),
            },
            protocol_verification: ProtocolVerification {
                state: "rejected".to_string(),
                checked_at_unix,
                summary: message.clone(),
            },
            models: Vec::new(),
            model_capability_evidence: vec![basic_protocol_capability(
                protocol.as_str(),
                "rejected",
            )],
            detection_evidence: Vec::new(),
            checks: Vec::new(),
            warnings: vec![message],
        },
        quota_status: "unknown".to_string(),
        remaining_ratio: 0.0,
        quota_windows: Vec::new(),
    })
}

fn merge_surface_detection_results(
    results: Vec<SurfaceDetectionOutcome>,
    allow_empty_catalog: bool,
) -> Result<ChannelDetectionResult> {
    if results.is_empty() {
        return Err(anyhow!("channel has no API surface protocol to verify"));
    }
    let mut models = Vec::new();
    let mut model_keys = std::collections::HashSet::new();
    let mut surface_results = Vec::new();
    let mut model_capability_evidence = Vec::new();
    let mut detection_evidence = Vec::new();
    let mut checks = Vec::new();
    let mut warnings = Vec::new();
    let mut quota_status = "unknown".to_string();
    let mut remaining_ratio = 0.0;
    let mut quota_windows = Vec::new();
    for outcome in results {
        for model in &outcome.result.models {
            if model_keys.insert(normalize_model_name(model)) {
                models.push(model.clone());
            }
        }
        model_capability_evidence.extend(outcome.result.model_capability_evidence.clone());
        detection_evidence.extend(outcome.result.detection_evidence.clone());
        checks.extend(outcome.result.checks.clone());
        warnings.extend(outcome.result.warnings.clone());
        if quota_status == "unknown" && outcome.quota_status != "unknown" {
            quota_status = outcome.quota_status;
            remaining_ratio = outcome.remaining_ratio;
        }
        if quota_windows.is_empty() && !outcome.quota_windows.is_empty() {
            quota_windows = outcome.quota_windows;
        }
        surface_results.push(outcome.result);
    }
    if models.is_empty() && !allow_empty_catalog {
        return Err(anyhow!("API surface verification returned no models"));
    }
    Ok(ChannelDetectionResult {
        availability: None,
        models,
        surface_results,
        model_capability_evidence,
        detection_evidence,
        quota_status,
        remaining_ratio,
        quota_windows,
        checks,
        warnings,
    })
}

pub(crate) async fn snapshot_channel(channel: ChannelConfig) -> Result<ChannelDetectionResult> {
    catalog_snapshot(channel, SubscriptionCatalogPolicy::Cached, true).await
}

/// Information refresh is discovery only. Never enter detect_surface or an
/// interactive check here, even when there is no previous verification.
pub(crate) async fn refresh_channel(channel: ChannelConfig) -> Result<ChannelDetectionResult> {
    let mut preserved = channel.clone();
    let mut result = catalog_snapshot(channel, SubscriptionCatalogPolicy::Fresh, false).await?;
    // The renderer consumes a complete evidence snapshot. Keep live checks and
    // endpoint evidence; replace only catalog claims for the refreshed protocols.
    preserved.capability_profiles.retain(|profile| {
        !profile.catalog_metadata
            || !result
                .surface_results
                .iter()
                .any(|surface| surface.protocol.as_str() == profile.protocol)
    });
    crate::supplier::merge_channel_detection_result(&mut preserved, &result);
    result.model_capability_evidence = preserved.capability_profiles;
    result.detection_evidence = preserved.detection_checks;
    Ok(result)
}

async fn catalog_snapshot(
    channel: ChannelConfig,
    subscription_policy: SubscriptionCatalogPolicy,
    verified_only: bool,
) -> Result<ChannelDetectionResult> {
    let quota_channel =
        crate::coding_plan::has_quota(channel.source_driver()).then(|| channel.clone());
    crate::coding_plan::validate_key(&channel)?;
    let allow_empty_catalog =
        channel.source_driver() == crate::source_driver::SourceDriverId::Openrouter;
    let contexts = surface_probe_contexts(channel, SurfaceProbeMode::Snapshot)?
        .into_iter()
        .filter(|context| !verified_only || snapshot_context_is_verified(context))
        .collect::<Vec<_>>();
    if contexts.is_empty() {
        return Err(anyhow!("periodic snapshot has no verified surface binding"));
    }

    let client = short_http_client().clone();
    let mut catalog_cache: Vec<(
        crate::surface::ApiSurface,
        String,
        Vec<crate::model_catalog::ModelObservation>,
    )> = Vec::new();
    let mut results = Vec::with_capacity(contexts.len());
    let mut partial_models = Vec::new();
    let mut partial_warnings = Vec::new();
    for context in contexts {
        let outcome = async {
            if context.execution_kind == crate::source_driver::ExecutionKind::HttpSurface {
                let (cache_surface, cache_url) = crate::openrouter::catalog_cache_key(
                    context.source_driver,
                    if crate::coding_gateway::shares_catalog(context.source_driver) {
                        // OpenCode's native dialects share a single model directory.
                        crate::surface::ApiSurface::OpenAi
                    } else {
                        context.binding.surface
                    },
                    &context.binding.base_url,
                );
                let cache_url = if crate::coding_gateway::shares_catalog(context.source_driver) {
                    crate::coding_gateway::catalog_url(context.source_driver, &cache_url)?
                        .to_string()
                } else {
                    cache_url
                };
                let cached = catalog_cache
                    .iter()
                    .find(|(surface, base_url, _)| {
                        *surface == cache_surface && base_url == &cache_url
                    })
                    .map(|(_, _, models)| models.clone());
                let models = if let Some(models) = cached {
                    models
                } else {
                    let models = fetch_surface_catalog(&client, &context).await?;
                    catalog_cache.push((cache_surface, cache_url, models.clone()));
                    models
                };
                snapshot_surface_result(&context, models)
            } else {
                let projection = probe_channel_from_context(&context)?;
                let observation =
                    detect_channel_single(projection, false, subscription_policy).await?;
                surface_probe_result_from_observation(&context, observation)
            }
        }
        .await;
        match outcome {
            Ok(outcome) => results.push(outcome),
            Err(error) if !verified_only && !context.is_default_target => {
                // An optional protocol's catalog failure is not a failed model
                // execution. Retain the old list/evidence instead of deleting
                // models whose absence could not be established by this refresh.
                partial_models.extend(context.models.iter().cloned());
                partial_warnings.push(format!(
                    "{} catalog was not refreshed; previous models retained: {error:#}",
                    context.binding.surface.as_str()
                ));
            }
            Err(error) => return Err(error),
        }
    }
    let mut result = merge_surface_detection_results(results, allow_empty_catalog)?;
    result.models.extend(partial_models);
    result.models = dedupe_models(result.models);
    result.warnings.extend(partial_warnings);
    if let Some(quota_channel) = quota_channel {
        result.quota_windows = refresh_subscription_quota(
            &quota_channel,
            subscription_policy,
            crate::coding_plan::fetch_quota(&client, &quota_channel),
        )
        .await?;
        if result.quota_windows.is_empty() {
            result.warnings.push(
                "Subscription quota is unavailable; no remaining balance has been assumed".into(),
            );
        } else {
            let supplier = crate::supplier_from_channel(&quota_channel);
            let mut state = crate::supplier::subscription_safety_state_for_channel(&supplier)?;
            crate::supplier::apply_official_quota_windows_to_state(
                &mut state,
                &result.quota_windows,
            );
            crate::supplier::save_subscription_safety_state(state)?;
            let snapshot = crate::supplier::subscription_quota_snapshot(&supplier);
            result.quota_status = snapshot.status;
            result.remaining_ratio = snapshot.remaining_ratio;
        }
    }
    Ok(result)
}

async fn fetch_surface_catalog(
    client: &Client,
    context: &SurfaceProbeContext,
) -> Result<Vec<crate::model_catalog::ModelObservation>> {
    let projection = probe_channel_from_context(context)?;
    if context.source_driver == crate::source_driver::SourceDriverId::Openrouter {
        return crate::openrouter::fetch_channel_catalog(client, &projection).await;
    }
    let models = match context.binding.surface {
        crate::surface::ApiSurface::Anthropic => {
            fetch_anthropic_catalog(
                client,
                &projection,
                &context.binding.base_url,
                &context.credential_ref,
            )
            .await
        }
        crate::surface::ApiSurface::Gemini => {
            fetch_gemini_native_catalog(
                client,
                &projection,
                &context.binding.base_url,
                &context.credential_ref,
            )
            .await
        }
        crate::surface::ApiSurface::OpenAi => match fetch_openai_compatible_catalog(
            client,
            &projection,
            &context.binding.base_url,
            &context.credential_ref,
        )
        .await
        {
            Ok(models) => Ok(models),
            Err(openai_err) if should_try_ollama_tags(&projection) => {
                fetch_ollama_tags_models(client, &projection, &context.binding.base_url)
                    .await
                    .with_context(|| {
                        format!("OpenAI-compatible model catalog failed first: {openai_err}")
                    })
                    .map(|models| {
                        models
                            .into_iter()
                            .map(|id| crate::model_catalog::ModelObservation {
                                id,
                                ..Default::default()
                            })
                            .collect()
                    })
            }
            Err(error) => Err(error),
        },
    }
    .with_context(|| {
        format!(
            "{} surface model catalog failed",
            context.binding.surface.as_str()
        )
    })?;
    if models.is_empty() {
        return Err(anyhow!("model catalog returned no models"));
    }
    Ok(crate::model_catalog::stable_dedupe_models(models))
}

fn snapshot_surface_result(
    context: &SurfaceProbeContext,
    observations: Vec<crate::model_catalog::ModelObservation>,
) -> Result<SurfaceDetectionOutcome> {
    let protocol = probe_protocol(context)?;
    let binding_protocol = &context.binding.protocols[0];
    let mut evidence = model_modality_capability_profiles(
        &basic_protocol_capability(protocol.as_str(), "declared"),
        &observations,
        false,
    );
    let mut models = model_ids(observations);
    if context.source_driver == crate::source_driver::SourceDriverId::Openrouter {
        crate::openrouter::normalize_catalog_projection(&mut models, &mut evidence);
    }
    Ok(SurfaceDetectionOutcome {
        result: SurfaceProbeResult {
            surface: context.binding.surface,
            protocol,
            surface_verification: context.binding.verification.clone(),
            protocol_verification: binding_protocol.verification.clone(),
            models,
            model_capability_evidence: evidence,
            detection_evidence: Vec::new(),
            checks: vec![format!(
                "snapshot: {} catalog refreshed without generation probe",
                context.binding.surface.as_str()
            )],
            warnings: Vec::new(),
        },
        quota_status: "unknown".to_string(),
        remaining_ratio: 0.0,
        quota_windows: Vec::new(),
    })
}

fn snapshot_context_is_verified(context: &SurfaceProbeContext) -> bool {
    context.binding.verification.state.trim() == "verified"
        || context
            .binding
            .protocols
            .iter()
            .any(|protocol| protocol.verification.state.trim() == "verified")
}

async fn detect_openai_family_channel(
    client: &Client,
    channel: &ChannelConfig,
    mut checks: Vec<String>,
    mut warnings: Vec<String>,
    run_advanced: bool,
) -> Result<ProbeObservation> {
    let catalog = check_deadline::run(async {
        if channel.source_driver() == crate::source_driver::SourceDriverId::Openrouter {
            crate::openrouter::fetch_channel_catalog(client, channel).await
        } else {
            fetch_openai_compatible_catalog(
                client,
                channel,
                &channel.upstream_base_url,
                &channel.upstream_api_key,
            )
            .await
        }
    })
    .await;
    let model_observations = match catalog {
        Ok(models) => models,
        Err(openai_err) if should_try_ollama_tags(channel) => check_deadline::run(
            fetch_ollama_tags_models(client, channel, &channel.upstream_base_url),
        )
        .await
        .with_context(|| format!("OpenAI-compatible probe failed first: {openai_err}"))?
        .into_iter()
        .map(|id| crate::model_catalog::ModelObservation {
            id,
            ..Default::default()
        })
        .collect(),
        Err(err) => return Err(err),
    };
    checks.push("models: OpenAI-compatible /v1/models ok".to_string());
    let models = model_ids(model_observations.clone());
    let primary_model = pick_primary_model(&models, channel);
    let primary_model =
        crate::openrouter::protocol_probe_model(channel, &model_observations, primary_model)?;
    let primary_model =
        crate::coding_gateway::probe_model(channel, &model_observations, primary_model)?;
    if let Some(protocols) = configured_source_protocol_contract(channel) {
        checks.push(format!(
            "protocols: {} source contract declared ({})",
            openai_family_source_label(channel),
            protocols.join(", ")
        ));
        let contract_protocols = protocols;
        let mut capability_profiles = contract_protocols
            .iter()
            .map(|protocol| basic_protocol_capability(protocol, "declared"))
            .collect::<Vec<_>>();
        let api_format = preferred_detected_protocol(channel, &contract_protocols);
        let primary_protocol = protocol_from_api_format(&api_format);
        for protocol in &contract_protocols {
            let probe = match protocol.as_str() {
                "openai_responses" => {
                    check_deadline::run(probe_openai_responses(client, channel, &primary_model))
                        .await
                }
                "openai_chat" => {
                    check_deadline::run(probe_openai_chat(client, channel, &primary_model)).await
                }
                _ => Err(anyhow!("unsupported OpenAI-family protocol: {protocol}")),
            };
            match probe {
                Ok(()) => {
                    mark_protocol_verified(&mut capability_profiles, protocol);
                    checks.push(format!(
                        "protocol: {} execution ok",
                        protocol_display_name(protocol)
                    ));
                }
                Err(err) if protocol == &primary_protocol => {
                    return Err(err).with_context(|| {
                        format!(
                            "{} primary protocol verification failed",
                            protocol_display_name(protocol)
                        )
                    });
                }
                Err(err) => {
                    mark_protocol_rejected(&mut capability_profiles, protocol);
                    warnings.push(format!(
                        "{} secondary protocol verification failed: {err}",
                        protocol_display_name(protocol)
                    ));
                }
            }
        }
        if should_run_feature_probes(channel, run_advanced) {
            enrich_openai_capability_profiles(
                client,
                channel,
                &primary_model,
                &model_observations,
                &mut capability_profiles,
                &mut checks,
                &mut warnings,
            )
            .await;
        }
        apply_official_api_file_contract(channel, &mut capability_profiles);
        append_catalog_profiles(&mut capability_profiles, &model_observations);
        return Ok(ProbeObservation {
            models,
            native_protocol_verified: true,
            quota_status: "unknown".to_string(),
            remaining_ratio: 0.0,
            quota_windows: Vec::new(),
            checks,
            warnings,
            capability_profiles,
            detection_checks: Vec::new(),
        });
    }

    let requested_protocols =
        requested_probe_protocols(channel, &["openai_responses", "openai_chat"]);
    let responses_probe = if requested_protocols
        .iter()
        .any(|protocol| protocol == "openai_responses")
    {
        Some(check_deadline::run(probe_openai_responses(client, channel, &primary_model)).await)
    } else {
        None
    };
    let chat_probe = if requested_protocols
        .iter()
        .any(|protocol| protocol == "openai_chat")
    {
        Some(check_deadline::run(probe_openai_chat(client, channel, &primary_model)).await)
    } else {
        None
    };
    let mut supported_protocols = Vec::new();
    let mut capability_profiles = Vec::new();
    if let Some(probe) = responses_probe.as_ref() {
        if probe.is_ok() {
            checks.push("protocol: OpenAI Responses /v1/responses ok".to_string());
            supported_protocols.push("openai_responses".to_string());
            capability_profiles.push(basic_protocol_capability("openai_responses", "verified"));
        } else {
            warnings.push(format!(
                "Responses probe failed: {}",
                probe
                    .as_ref()
                    .err()
                    .map(|err| err.to_string())
                    .unwrap_or_else(|| "unknown error".to_string())
            ));
        }
    }
    if let Some(probe) = chat_probe.as_ref() {
        if probe.is_ok() {
            checks.push("protocol: OpenAI Chat Completions /v1/chat/completions ok".to_string());
            supported_protocols.push("openai_chat".to_string());
            capability_profiles.push(basic_protocol_capability("openai_chat", "verified"));
        } else {
            warnings.push(format!(
                "Chat Completions probe failed: {}",
                probe
                    .as_ref()
                    .err()
                    .map(|err| err.to_string())
                    .unwrap_or_else(|| "unknown error".to_string())
            ));
        }
    }
    if supported_protocols.is_empty() {
        let responses_error = probe_error_summary(responses_probe.as_ref());
        let chat_error = probe_error_summary(chat_probe.as_ref());
        return Err(protocol_probe_failure(
            format!(
                "no supported OpenAI-compatible protocol detected; Responses: {responses_error}; Chat Completions: {chat_error}"
            ),
            [responses_probe, chat_probe],
        ));
    }
    let missing_protocols = requested_protocols
        .iter()
        .filter(|required| {
            !supported_protocols
                .iter()
                .any(|protocol| protocol == *required)
        })
        .cloned()
        .collect::<Vec<_>>();
    if !missing_protocols.is_empty() {
        return Err(protocol_probe_failure(
            format!(
                "selected protocol verification failed; requested: {}; missing: {}",
                requested_protocols.join(", "),
                missing_protocols.join(", ")
            ),
            [responses_probe, chat_probe],
        ));
    }
    if should_run_feature_probes(channel, run_advanced) {
        enrich_openai_capability_profiles(
            client,
            channel,
            &primary_model,
            &model_observations,
            &mut capability_profiles,
            &mut checks,
            &mut warnings,
        )
        .await;
    }
    apply_official_api_file_contract(channel, &mut capability_profiles);
    append_catalog_profiles(&mut capability_profiles, &model_observations);
    Ok(ProbeObservation {
        models,
        native_protocol_verified: true,
        quota_status: "unknown".to_string(),
        remaining_ratio: 0.0,
        quota_windows: Vec::new(),
        checks,
        warnings,
        capability_profiles,
        detection_checks: Vec::new(),
    })
}

async fn detect_anthropic_channel(
    client: &Client,
    channel: &ChannelConfig,
    mut checks: Vec<String>,
    mut warnings: Vec<String>,
    run_advanced: bool,
) -> Result<ProbeObservation> {
    let model_observations = check_deadline::run(async {
        if channel.source_driver() == crate::source_driver::SourceDriverId::Openrouter {
            crate::openrouter::fetch_channel_catalog(client, channel).await
        } else {
            fetch_anthropic_catalog(
                client,
                channel,
                &channel.upstream_base_url,
                &channel.upstream_api_key,
            )
            .await
        }
    })
    .await
    .context("Anthropic model catalog failed")?;
    let models = model_ids(model_observations.clone());
    let primary_model = pick_primary_model(&models, channel);
    let primary_model =
        crate::openrouter::protocol_probe_model(channel, &model_observations, primary_model)?;
    let primary_model =
        crate::coding_gateway::probe_model(channel, &model_observations, primary_model)?;
    if configured_source_protocol_contract(channel).is_some() {
        checks.push(
            "protocols: Anthropic official source contract declared (anthropic_messages)"
                .to_string(),
        );
        check_deadline::run(probe_anthropic_messages(client, channel, &primary_model))
            .await
            .context("Anthropic Messages primary protocol verification failed")?;
        checks.push("protocol: primary Anthropic Messages execution ok".to_string());
        let mut capability_profiles =
            vec![basic_protocol_capability("anthropic_messages", "verified")];
        if should_run_feature_probes(channel, run_advanced) {
            enrich_anthropic_capability_profiles(
                client,
                channel,
                &primary_model,
                &model_observations,
                &mut capability_profiles,
                &mut checks,
                &mut warnings,
            )
            .await;
        }
        apply_official_api_file_contract(channel, &mut capability_profiles);
        append_catalog_profiles(&mut capability_profiles, &model_observations);
        return Ok(ProbeObservation {
            models,
            native_protocol_verified: true,
            quota_status: "unknown".to_string(),
            remaining_ratio: 0.0,
            quota_windows: Vec::new(),
            checks,
            warnings,
            capability_profiles,
            detection_checks: Vec::new(),
        });
    }
    check_deadline::run(probe_anthropic_messages(client, channel, &primary_model))
        .await
        .context("Anthropic Messages probe failed")?;
    checks.push("protocol: Anthropic Messages /v1/messages ok".to_string());
    let mut capability_profiles = vec![basic_protocol_capability("anthropic_messages", "verified")];
    if should_run_feature_probes(channel, run_advanced) {
        enrich_anthropic_capability_profiles(
            client,
            channel,
            &primary_model,
            &model_observations,
            &mut capability_profiles,
            &mut checks,
            &mut warnings,
        )
        .await;
    }
    apply_official_api_file_contract(channel, &mut capability_profiles);
    append_catalog_profiles(&mut capability_profiles, &model_observations);
    Ok(ProbeObservation {
        models,
        native_protocol_verified: true,
        quota_status: "unknown".to_string(),
        remaining_ratio: 0.0,
        quota_windows: Vec::new(),
        checks,
        warnings,
        capability_profiles,
        detection_checks: Vec::new(),
    })
}

async fn detect_gemini_channel(
    client: &Client,
    channel: &ChannelConfig,
    mut checks: Vec<String>,
    mut warnings: Vec<String>,
    run_advanced: bool,
) -> Result<ProbeObservation> {
    let model_observations = check_deadline::run(fetch_gemini_native_catalog(
        client,
        channel,
        &channel.upstream_base_url,
        &channel.upstream_api_key,
    ))
    .await
    .context("Gemini native model catalog failed")?;
    let models = model_ids(model_observations.clone());
    let primary_model = crate::coding_gateway::probe_model(
        channel,
        &model_observations,
        pick_primary_model(&models, channel),
    )?;
    if let Some(protocols) = configured_source_protocol_contract(channel) {
        checks.push(format!(
            "protocols: Gemini official source contract declared ({})",
            protocols.join(", ")
        ));
        let contract_protocols = protocols;
        let api_format = preferred_detected_protocol(channel, &contract_protocols);
        let primary_protocol = protocol_from_api_format(&api_format);
        let mut capability_profiles = contract_protocols
            .iter()
            .map(|protocol| basic_protocol_capability(protocol, "declared"))
            .collect::<Vec<_>>();
        for protocol in &contract_protocols {
            let probe = match protocol.as_str() {
                "gemini_native" => {
                    check_deadline::run(probe_gemini_native(client, channel, &primary_model)).await
                }
                "openai_chat" => {
                    let openai_base = gemini_openai_base(&channel.upstream_base_url);
                    check_deadline::run(probe_openai_chat_with_base(
                        client,
                        channel,
                        &openai_base,
                        &channel.upstream_api_key,
                        &primary_model,
                    ))
                    .await
                }
                _ => Err(anyhow!("unsupported Gemini protocol: {protocol}")),
            };
            match probe {
                Ok(()) => {
                    mark_protocol_verified(&mut capability_profiles, protocol);
                    checks.push(format!(
                        "protocol: {} execution ok",
                        protocol_display_name(protocol)
                    ));
                }
                Err(err) if protocol == &primary_protocol => {
                    return Err(err).with_context(|| {
                        format!(
                            "{} primary protocol verification failed",
                            protocol_display_name(protocol)
                        )
                    });
                }
                Err(err) => {
                    mark_protocol_rejected(&mut capability_profiles, protocol);
                    warnings.push(format!(
                        "{} secondary protocol verification failed: {err}",
                        protocol_display_name(protocol)
                    ));
                }
            }
        }
        if should_run_feature_probes(channel, run_advanced) {
            enrich_gemini_native_capability_profiles(
                client,
                channel,
                &primary_model,
                &mut capability_profiles,
                &mut checks,
                &mut warnings,
            )
            .await;
        }
        apply_official_api_file_contract(channel, &mut capability_profiles);
        append_catalog_profiles(&mut capability_profiles, &model_observations);
        return Ok(ProbeObservation {
            models,
            native_protocol_verified: true,
            quota_status: "unknown".to_string(),
            remaining_ratio: 0.0,
            quota_windows: Vec::new(),
            checks,
            warnings,
            capability_profiles,
            detection_checks: Vec::new(),
        });
    }
    let requested_protocols = requested_probe_protocols(channel, &["gemini_native", "openai_chat"]);
    let native_probe = if requested_protocols
        .iter()
        .any(|value| value == "gemini_native")
    {
        Some(check_deadline::run(probe_gemini_native(client, channel, &primary_model)).await)
    } else {
        None
    };
    let openai_base = gemini_openai_base(&channel.upstream_base_url);
    let chat_probe = if requested_protocols
        .iter()
        .any(|value| value == "openai_chat")
    {
        Some(
            check_deadline::run(probe_openai_chat_with_base(
                client,
                channel,
                &openai_base,
                &channel.upstream_api_key,
                &primary_model,
            ))
            .await,
        )
    } else {
        None
    };
    let mut supported_protocols = Vec::new();
    let mut capability_profiles = Vec::new();
    if let Some(probe) = native_probe.as_ref() {
        if probe.is_ok() {
            checks.push("protocol: Gemini native generateContent ok".to_string());
            supported_protocols.push("gemini_native".to_string());
            capability_profiles.push(basic_protocol_capability("gemini_native", "verified"));
        } else {
            warnings.push(format!(
                "Gemini native probe failed: {}",
                probe
                    .as_ref()
                    .err()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "unknown error".to_string())
            ));
        }
    }
    if let Some(probe) = chat_probe.as_ref() {
        if probe.is_ok() {
            checks.push("protocol: Gemini OpenAI Chat compatibility ok".to_string());
            supported_protocols.push("openai_chat".to_string());
            capability_profiles.push(basic_protocol_capability("openai_chat", "verified"));
        } else {
            warnings.push(format!(
                "Gemini OpenAI Chat probe failed: {}",
                probe
                    .as_ref()
                    .err()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "unknown error".to_string())
            ));
        }
    }
    if requested_protocols.is_empty() {
        return Err(anyhow!(
            "select at least one Gemini native or OpenAI Chat protocol before detection"
        ));
    }
    let missing_protocols = requested_protocols
        .iter()
        .filter(|required| {
            !supported_protocols
                .iter()
                .any(|protocol| protocol == *required)
        })
        .cloned()
        .collect::<Vec<_>>();
    if !missing_protocols.is_empty() {
        return Err(protocol_probe_failure(
            format!(
                "Gemini official protocol contract failed; missing direct protocols: {}",
                missing_protocols.join(", ")
            ),
            [native_probe, chat_probe],
        ));
    }
    if should_run_feature_probes(channel, run_advanced) {
        enrich_gemini_native_capability_profiles(
            client,
            channel,
            &primary_model,
            &mut capability_profiles,
            &mut checks,
            &mut warnings,
        )
        .await;
    }
    apply_official_api_file_contract(channel, &mut capability_profiles);
    append_catalog_profiles(&mut capability_profiles, &model_observations);
    Ok(ProbeObservation {
        models,
        native_protocol_verified: true,
        quota_status: "unknown".to_string(),
        remaining_ratio: 0.0,
        quota_windows: Vec::new(),
        checks,
        warnings,
        capability_profiles,
        detection_checks: Vec::new(),
    })
}

fn known_source_protocol_contract(channel: &ChannelConfig) -> Option<Vec<&'static str>> {
    let id = channel.source_driver();
    if crate::source_driver::source_driver(id).is_ok_and(|driver| driver.advanced) {
        return None;
    }
    crate::source_driver::source_driver(id)
        .ok()
        .map(|driver| driver.protocols())
}

fn configured_source_protocol_contract(channel: &ChannelConfig) -> Option<Vec<String>> {
    let contract = known_source_protocol_contract(channel)?;
    let configured = requested_probe_protocols(channel, &contract);
    if configured.is_empty() {
        Some(contract.iter().map(|value| (*value).to_string()).collect())
    } else {
        Some(configured)
    }
}

fn requested_probe_protocols(channel: &ChannelConfig, allowed: &[&str]) -> Vec<String> {
    let requested =
        normalize_supported_protocols(channel.supported_protocols.clone(), &channel.api_format);
    let mut protocols = requested
        .into_iter()
        .filter(|protocol| allowed.contains(&protocol.as_str()))
        .collect::<Vec<_>>();
    if protocols.is_empty() && allowed.contains(&channel.api_format.as_str()) {
        protocols.push(channel.api_format.clone());
    }
    protocols
}

fn preferred_detected_protocol(channel: &ChannelConfig, supported: &[String]) -> String {
    let preferred_protocol = protocol_from_api_format(&channel.api_format);
    if supported
        .iter()
        .any(|protocol| protocol == &preferred_protocol)
    {
        return channel.api_format.clone();
    }
    supported
        .first()
        .cloned()
        .unwrap_or_else(|| channel.api_format.clone())
}

fn probe_error_summary(probe: Option<&Result<()>>) -> String {
    match probe {
        Some(Ok(())) => "ok".to_string(),
        Some(Err(err)) => err.to_string(),
        None => "not selected".to_string(),
    }
}

fn protocol_probe_failure(
    summary: String,
    probes: impl IntoIterator<Item = Option<Result<()>>>,
) -> anyhow::Error {
    // Keep the typed cause through the human-readable multi-protocol summary.
    // Incomplete verification is not an upstream protocol rejection.
    let mut failures = probes
        .into_iter()
        .flatten()
        .filter_map(Result::err)
        .collect::<Vec<_>>();
    let incomplete = failures.iter().position(check_deadline::incomplete);
    if failures.is_empty() {
        anyhow!(summary)
    } else {
        failures.remove(incomplete.unwrap_or(0)).context(summary)
    }
}

fn openai_family_source_label(channel: &ChannelConfig) -> &'static str {
    if is_azure_openai_channel(channel) {
        "Azure OpenAI"
    } else if channel.kind.trim().eq_ignore_ascii_case("openai")
        || channel
            .upstream_base_url
            .trim()
            .to_ascii_lowercase()
            .contains("api.openai.com")
    {
        "OpenAI official"
    } else {
        "OpenAI-compatible"
    }
}

fn basic_protocol_capability(protocol: &str, verification_state: &str) -> ChannelCapabilityProfile {
    ChannelCapabilityProfile {
        protocol: protocol.to_string(),
        capability_layer: "model_wire".to_string(),
        non_stream_json: true,
        stream_sse: true,
        stream_sse_unsupported: false,
        tool_calls: false,
        tool_choice: false,
        parallel_tool_calls: false,
        json_schema: false,
        reasoning: false,
        thinking: false,
        vision: false,
        cache_control: false,
        hosted_tools: Vec::new(),
        custom_tool: false,
        verification_state: verification_state.to_string(),
        verified_at_unix: now_unix(),
        ..Default::default()
    }
}

fn model_observations_from_tool_catalog(
    models: &[String],
    source: &str,
) -> Vec<crate::model_catalog::ModelObservation> {
    crate::tool_model_metadata::tool_models_from_ids(models)
        .into_iter()
        .map(|model| crate::model_catalog::ModelObservation {
            context_tokens: None,
            output_tokens: None,
            supported_parameters: None,
            supported_endpoints: None,
            probe_cost_nanos: None,
            id: model.id,
            priority: None,
            source: source.to_string(),
            etag: None,
            use_responses_lite: None,
            input_modalities: model.input_modalities,
            output_modalities: model.output_modalities,
        })
        .collect()
}

fn model_observations_from_saved_channel(
    channel: &ChannelConfig,
) -> Vec<crate::model_catalog::ModelObservation> {
    let fallback =
        model_observations_from_tool_catalog(&channel.models, "persisted_channel_catalog");
    channel
        .models
        .iter()
        .map(|id| {
            let mut observation = fallback
                .iter()
                .find(|model| model.id == crate::config::public_model_name(id))
                .cloned()
                .unwrap_or_default();
            observation.id = id.clone();
            for profile in &channel.capability_profiles {
                if profile.catalog_metadata
                    && profile.model_pattern.eq_ignore_ascii_case(id)
                    && profile.verification_state != "rejected"
                    && !matches!(profile.release_status.as_str(), "suspended" | "prepared")
                {
                    observation.context_tokens = crate::model_catalog::conservative_limit(
                        observation.context_tokens,
                        profile.context_tokens,
                    );
                    observation.output_tokens = crate::model_catalog::conservative_limit(
                        observation.output_tokens,
                        profile.output_tokens,
                    );
                    for (name, supported) in [
                        ("image", profile.vision || profile.image_input),
                        ("audio", profile.audio_input),
                        ("video", profile.video_input),
                        ("file", profile.file_input),
                    ] {
                        if supported
                            && !observation
                                .input_modalities
                                .iter()
                                .any(|value| value == name)
                        {
                            observation.input_modalities.push(name.into());
                        }
                    }
                    for (name, supported) in [
                        ("image", profile.image_output),
                        ("audio", profile.audio_output),
                        ("video", profile.video_output),
                        ("file", profile.file_output),
                    ] {
                        if supported
                            && !observation
                                .output_modalities
                                .iter()
                                .any(|value| value == name)
                        {
                            observation.output_modalities.push(name.into());
                        }
                    }
                }
            }
            observation
        })
        .collect()
}

fn antigravity_model_observations(
    value: &serde_json::Value,
) -> Vec<crate::model_catalog::ModelObservation> {
    let routes = crate::antigravity_models::model_routes_from_catalog(value);
    let data = value
        .get("models")
        .and_then(serde_json::Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(id, metadata)| {
            let wire = crate::antigravity_models::callable_upstream_model_id(id);
            let route = routes
                .iter()
                .find(|route| route.upstream_model.eq_ignore_ascii_case(&wire))?;
            let mut model = metadata.clone();
            if !model.is_object() {
                return None;
            }
            model["id"] = serde_json::json!(route.canonical_model);
            Some(model)
        })
        .collect::<Vec<_>>();
    crate::model_catalog::model_observations_from_value(
        ModelCatalogKind::OpenAI,
        &serde_json::json!({"data": data}),
        "antigravity_catalog",
    )
}

fn apply_subscription_driver_capabilities(
    native_protocol: &str,
    profile: &mut ChannelCapabilityProfile,
) {
    match native_protocol {
        "openai_responses" => {
            profile.parallel_tool_calls = true;
            profile.json_schema = true;
            profile.reasoning = true;
        }
        "anthropic_messages" => {
            profile.thinking = true;
            profile.cache_control = true;
        }
        "gemini_native" => {
            profile.json_schema = true;
            profile.thinking = true;
        }
        _ => {}
    }
}

fn model_modality_capability_profiles(
    base: &ChannelCapabilityProfile,
    models: &[crate::model_catalog::ModelObservation],
    experimental_media: bool,
) -> Vec<ChannelCapabilityProfile> {
    models
        .iter()
        .filter(|model| {
            !model.input_modalities.is_empty()
                || !model.output_modalities.is_empty()
                || model.context_tokens.is_some()
                || model.output_tokens.is_some()
                || model.supported_parameters.is_some()
                || model.supported_endpoints.is_some()
        })
        .map(|model| {
            let has_input = |candidate: &str| {
                model
                    .input_modalities
                    .iter()
                    .any(|value| value.eq_ignore_ascii_case(candidate))
            };
            let has_output = |candidate: &str| {
                model
                    .output_modalities
                    .iter()
                    .any(|value| value.eq_ignore_ascii_case(candidate))
            };
            let mut profile = ChannelCapabilityProfile {
                protocol: base.protocol.clone(),
                context_tokens: model.context_tokens,
                probe_cost_nanos: model.probe_cost_nanos,
                catalog_text_output: (!model.output_modalities.is_empty())
                    .then(|| model.output_modalities.iter().any(|m| m == "text")),
                catalog_source_model: Some(model.id.clone()),
                output_tokens: model.output_tokens,
                catalog_metadata: true,
                catalog_protocol_supported: crate::coding_gateway::catalog_supports_protocol(
                    model,
                    &base.protocol,
                ),
                capability_layer: "model_wire".to_string(),
                // Modalities come from the model catalog. A successful text protocol probe
                // must not upgrade every catalog modality to live semantic evidence.
                verification_state: "declared".to_string(),
                verified_at_unix: now_unix(),
                ..Default::default()
            };
            profile.model_pattern = model.id.clone();
            crate::openrouter::apply_catalog_capabilities(&mut profile, model);
            // Catalog modality lists are positive declarations, not exhaustive rejection
            // contracts. In particular, Codex currently serializes only text/image/audio in
            // InputModality while Responses also accepts input_file. Treating an omitted catalog
            // tag as a negative used to manufacture exact-model rejections for working routes.
            // Explicit driver contracts and live probes remain able to publish authoritative
            // negatives through input/output_modalities_authoritative.
            profile.input_modalities_authoritative = false;
            profile.output_modalities_authoritative = false;
            profile.vision = has_input("image");
            profile.image_input = has_input("image");
            profile.audio_input = has_input("audio");
            profile.video_input = has_input("video");
            profile.file_input = has_input("file") || has_input("pdf");
            profile.image_output = has_output("image");
            profile.audio_output = has_output("audio");
            profile.video_output = has_output("video");
            profile.file_output = has_output("file");
            profile.release_status = if experimental_media
                && (profile.audio_input
                    || profile.audio_output
                    || profile.video_input
                    || profile.video_output)
            {
                "experimental"
            } else {
                "supported"
            }
            .to_string();
            profile
        })
        .collect()
}

fn append_catalog_profiles(
    profiles: &mut Vec<ChannelCapabilityProfile>,
    models: &[crate::model_catalog::ModelObservation],
) {
    let protocols = profiles
        .iter()
        .map(|profile| profile.protocol.clone())
        .collect::<std::collections::BTreeSet<_>>();
    for protocol in protocols {
        profiles.extend(model_modality_capability_profiles(
            &basic_protocol_capability(&protocol, "declared"),
            models,
            false,
        ));
    }
}

fn apply_official_api_file_contract(
    channel: &ChannelConfig,
    profiles: &mut Vec<ChannelCapabilityProfile>,
) {
    use crate::source_driver::SourceDriverId;

    let protocol = match channel.v2.source_driver {
        SourceDriverId::OpenAiApi => "openai_responses",
        SourceDriverId::AnthropicApi => "anthropic_messages",
        SourceDriverId::GeminiApi => "gemini_native",
        _ => return,
    };
    if profiles.iter().any(|profile| {
        profile.protocol == protocol
            && profile.model_pattern.trim().is_empty()
            && profile.file_input
            && crate::model::capability_layer_is_routable(&profile.capability_layer)
    }) {
        return;
    }
    // Keep the file contract separate from the profile used by the basic text probe.
    // This is provider metadata until a dedicated semantic file probe succeeds.
    profiles.push(ChannelCapabilityProfile {
        protocol: protocol.to_string(),
        capability_layer: "model_wire".to_string(),
        release_status: "supported".to_string(),
        file_input: true,
        verification_state: "declared".to_string(),
        verified_at_unix: now_unix(),
        ..Default::default()
    });
}

fn mark_protocol_verified(profiles: &mut [ChannelCapabilityProfile], protocol: &str) {
    if let Some(profile) = profiles
        .iter_mut()
        .find(|profile| profile.protocol == protocol)
    {
        profile.verification_state = "verified".to_string();
        profile.verified_at_unix = now_unix();
    }
}

fn mark_protocol_rejected(profiles: &mut [ChannelCapabilityProfile], protocol: &str) {
    if let Some(profile) = profiles
        .iter_mut()
        .find(|profile| profile.protocol == protocol)
    {
        profile.verification_state = "rejected".to_string();
        profile.verified_at_unix = now_unix();
    }
}

fn protocol_display_name(protocol: &str) -> &'static str {
    match protocol {
        "openai_responses" => "OpenAI Responses",
        "openai_chat" => "OpenAI Chat Completions",
        "anthropic_messages" => "Anthropic Messages",
        "gemini_native" => "Gemini native",
        _ => "unknown protocol",
    }
}

fn should_run_feature_probes(channel: &ChannelConfig, run_advanced: bool) -> bool {
    if !run_advanced || channel.kind.trim() == "subscription_adapter" {
        return false;
    }
    crate::source_driver::source_driver(channel.source_driver())
        .ok()
        .map(|driver| {
            driver.creation.advanced_conversational
                == crate::source_driver::AdvancedVerificationPolicy::DriverPreferredOnly
        })
        .unwrap_or(true)
}

async fn enrich_openai_capability_profiles(
    client: &Client,
    channel: &ChannelConfig,
    model: &str,
    catalog: &[crate::model_catalog::ModelObservation],
    capability_profiles: &mut Vec<ChannelCapabilityProfile>,
    checks: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    let primary_protocol = protocol_from_api_format(&channel.api_format);
    let named_tool_choice = capability_check::probe_uses_named_tool_choice(channel, catalog, model);
    if primary_protocol == "openai_responses" {
        if let Some(responses) = capability_profiles
            .iter_mut()
            .find(|profile| profile.protocol == "openai_responses")
        {
            match check_deadline::run(probe_openai_responses_stream(client, channel, model)).await {
                Ok(()) => {
                    responses.stream_sse = true;
                    responses.stream_sse_unsupported = false;
                    checks.push("capability: OpenAI Responses stream_sse ok".to_string());
                }
                Err(err) if stream_probe_explicitly_unsupported(&err) => {
                    responses.stream_sse = false;
                    responses.stream_sse_unsupported = true;
                    warnings.push(format!(
                        "OpenAI Responses explicitly rejected streaming: {err}"
                    ));
                }
                Err(err) => {
                    capability_check::retain_incomplete(
                        channel,
                        model,
                        responses,
                        capability_check::Feature::Stream,
                        &err,
                    );
                    warnings.push(format!(
                        "OpenAI Responses stream probe was inconclusive: {err}"
                    ));
                }
            }
            match check_deadline::run(tool_probe::run(
                client,
                channel,
                model,
                crate::protocol::kind::ProtocolKind::OpenAiResponses,
                named_tool_choice,
            ))
            .await
            {
                Ok(()) => {
                    responses.tool_calls = true;
                    responses.tool_choice = named_tool_choice;
                    checks.push("capability: OpenAI Responses tool_calls ok".to_string());
                }
                Err(err) => {
                    capability_check::retain_incomplete(
                        channel,
                        model,
                        responses,
                        capability_check::Feature::Tools,
                        &err,
                    );
                    warnings.push(format!("OpenAI Responses tool call not confirmed: {err}"))
                }
            }
            match check_deadline::run(probe_openai_responses_json_schema(client, channel, model))
                .await
            {
                Ok(()) => {
                    responses.json_schema = true;
                    checks.push("capability: OpenAI Responses json_schema ok".to_string());
                }
                Err(err) => {
                    capability_check::retain_incomplete(
                        channel,
                        model,
                        responses,
                        capability_check::Feature::JsonSchema,
                        &err,
                    );
                    warnings.push(format!("OpenAI Responses JSON schema probe failed: {err}"))
                }
            }
        }
        capability_check::scope_probed_model_features(channel, model, capability_profiles);
        return;
    }
    if primary_protocol != "openai_chat" {
        return;
    }
    if let Some(chat) = capability_profiles
        .iter_mut()
        .find(|profile| profile.protocol == "openai_chat")
    {
        match check_deadline::run(probe_openai_chat_stream(client, channel, model)).await {
            Ok(()) => {
                chat.stream_sse = true;
                chat.stream_sse_unsupported = false;
                checks.push("capability: OpenAI Chat stream_sse ok".to_string());
            }
            Err(err) if stream_probe_explicitly_unsupported(&err) => {
                chat.stream_sse = false;
                chat.stream_sse_unsupported = true;
                warnings.push(format!("OpenAI Chat explicitly rejected streaming: {err}"));
            }
            Err(err) => {
                capability_check::retain_incomplete(
                    channel,
                    model,
                    chat,
                    capability_check::Feature::Stream,
                    &err,
                );
                warnings.push(format!("OpenAI Chat stream probe was inconclusive: {err}"));
            }
        }
        match check_deadline::run(tool_probe::run(
            client,
            channel,
            model,
            crate::protocol::kind::ProtocolKind::OpenAiChat,
            named_tool_choice,
        ))
        .await
        {
            Ok(()) => {
                chat.tool_calls = true;
                chat.tool_choice = named_tool_choice;
                checks.push("capability: OpenAI Chat tool_calls ok".to_string());
            }
            Err(err) => {
                capability_check::retain_incomplete(
                    channel,
                    model,
                    chat,
                    capability_check::Feature::Tools,
                    &err,
                );
                warnings.push(format!("OpenAI Chat tool call not confirmed: {err}"));
            }
        }
        match check_deadline::run(probe_openai_chat_json_schema(
            client,
            channel,
            &channel.upstream_base_url,
            &channel.upstream_api_key,
            model,
        ))
        .await
        {
            Ok(()) => {
                chat.json_schema = true;
                checks.push("capability: OpenAI Chat json_schema ok".to_string());
            }
            Err(err) => {
                capability_check::retain_incomplete(
                    channel,
                    model,
                    chat,
                    capability_check::Feature::JsonSchema,
                    &err,
                );
                warnings.push(format!("OpenAI Chat JSON schema probe failed: {err}"));
            }
        }
    }
    capability_check::scope_probed_model_features(channel, model, capability_profiles);
}

async fn enrich_anthropic_capability_profiles(
    client: &Client,
    channel: &ChannelConfig,
    model: &str,
    catalog: &[crate::model_catalog::ModelObservation],
    capability_profiles: &mut Vec<ChannelCapabilityProfile>,
    checks: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    let named_tool_choice = capability_check::probe_uses_named_tool_choice(channel, catalog, model);
    if let Some(messages) = capability_profiles
        .iter_mut()
        .find(|profile| profile.protocol == "anthropic_messages")
    {
        match check_deadline::run(probe_anthropic_messages_stream(client, channel, model)).await {
            Ok(()) => {
                messages.stream_sse = true;
                messages.stream_sse_unsupported = false;
                checks.push("capability: Anthropic Messages stream_sse ok".to_string());
            }
            Err(err) if stream_probe_explicitly_unsupported(&err) => {
                messages.stream_sse = false;
                messages.stream_sse_unsupported = true;
                warnings.push(format!(
                    "Anthropic Messages explicitly rejected streaming: {err}"
                ));
            }
            Err(err) => {
                capability_check::retain_incomplete(
                    channel,
                    model,
                    messages,
                    capability_check::Feature::Stream,
                    &err,
                );
                warnings.push(format!(
                    "Anthropic Messages stream probe was inconclusive: {err}"
                ));
            }
        }
        match check_deadline::run(tool_probe::run(
            client,
            channel,
            model,
            crate::protocol::kind::ProtocolKind::AnthropicMessages,
            named_tool_choice,
        ))
        .await
        {
            Ok(()) => {
                messages.tool_calls = true;
                messages.tool_choice = named_tool_choice;
                checks.push("capability: Anthropic Messages tool_calls ok".to_string());
            }
            Err(err) => {
                capability_check::retain_incomplete(
                    channel,
                    model,
                    messages,
                    capability_check::Feature::Tools,
                    &err,
                );
                warnings.push(format!("Anthropic Messages tool call not confirmed: {err}"));
            }
        }
        match check_deadline::run(probe_anthropic_messages_cache_control(
            client, channel, model,
        ))
        .await
        {
            Ok(()) => {
                messages.cache_control = true;
                checks.push("capability: Anthropic Messages cache_control ok".to_string());
            }
            Err(err) => {
                capability_check::retain_incomplete(
                    channel,
                    model,
                    messages,
                    capability_check::Feature::CacheControl,
                    &err,
                );
                warnings.push(format!(
                    "Anthropic Messages cache_control probe failed: {err}"
                ));
            }
        }
    }
    capability_check::scope_probed_model_features(channel, model, capability_profiles);
}

async fn enrich_gemini_native_capability_profiles(
    client: &Client,
    channel: &ChannelConfig,
    model: &str,
    capability_profiles: &mut Vec<ChannelCapabilityProfile>,
    checks: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    if protocol_from_api_format(&channel.api_format) != "gemini_native" {
        return;
    }
    if let Some(gemini) = capability_profiles
        .iter_mut()
        .find(|profile| profile.protocol == "gemini_native")
    {
        match check_deadline::run(probe_gemini_native_stream(client, channel, model)).await {
            Ok(()) => {
                gemini.stream_sse = true;
                gemini.stream_sse_unsupported = false;
                checks.push("capability: Gemini native stream_sse ok".to_string());
            }
            Err(err) if stream_probe_explicitly_unsupported(&err) => {
                gemini.stream_sse = false;
                gemini.stream_sse_unsupported = true;
                warnings.push(format!(
                    "Gemini native explicitly rejected streaming: {err}"
                ));
            }
            Err(err) => {
                capability_check::retain_incomplete(
                    channel,
                    model,
                    gemini,
                    capability_check::Feature::Stream,
                    &err,
                );
                warnings.push(format!(
                    "Gemini native stream probe was inconclusive: {err}"
                ));
            }
        }
        match check_deadline::run(tool_probe::run(
            client,
            channel,
            model,
            crate::protocol::kind::ProtocolKind::GeminiNative,
            true,
        ))
        .await
        {
            Ok(()) => {
                gemini.tool_calls = true;
                gemini.tool_choice = true;
                checks.push("capability: Gemini native function_call ok".to_string());
            }
            Err(err) => {
                capability_check::retain_incomplete(
                    channel,
                    model,
                    gemini,
                    capability_check::Feature::Tools,
                    &err,
                );
                warnings.push(format!("Gemini native tool call not confirmed: {err}"));
            }
        }
        match check_deadline::run(probe_gemini_native_json_schema(client, channel, model)).await {
            Ok(()) => {
                gemini.json_schema = true;
                checks.push("capability: Gemini native json_schema ok".to_string());
            }
            Err(err) => {
                capability_check::retain_incomplete(
                    channel,
                    model,
                    gemini,
                    capability_check::Feature::JsonSchema,
                    &err,
                );
                warnings.push(format!("Gemini native JSON schema probe failed: {err}"));
            }
        }
    }
    capability_check::scope_probed_model_features(channel, model, capability_profiles);
}

fn stream_probe_explicitly_unsupported(error: &anyhow::Error) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    if message.contains("405 method not allowed") || message.contains("501 not implemented") {
        return true;
    }
    [
        "streaming is not supported",
        "streaming not supported",
        "stream is not supported",
        "stream not supported",
        "does not support streaming",
        "doesn't support streaming",
        "unsupported streaming",
        "unsupported stream",
        "unknown parameter: stream",
        "unrecognized request argument supplied: stream",
    ]
    .iter()
    .any(|marker| message.contains(marker))
}

#[cfg(test)]
mod stream_probe_tests {
    use super::{
        ProbeObservation, SurfaceProbeContext, SurfaceProbeMode, apply_official_api_file_contract,
        apply_subscription_driver_capabilities, basic_protocol_capability, channel_provider_hint,
        classify_surface_operation_probe_status, known_source_protocol_contract,
        model_modality_capability_profiles, normalize_probe_channel, operation_family_probes,
        preferred_detected_protocol, requested_probe_protocols,
        should_probe_custom_surface_families, source_has_authoritative_surface_contract,
        stream_probe_explicitly_unsupported, supplement_codex_manifest_modalities,
        surface_probe_result_from_observation,
    };
    use crate::{channel_from_supplier, default_supplier_config};

    #[test]
    fn saved_catalog_restores_capacity_and_modalities_without_relabeling_static_limits() {
        let mut channel = channel_from_supplier("saved-catalog".into(), &default_supplier_config());
        channel.models = vec!["Vendor/unknown-model".into(), "claude-sonnet-4-6".into()];
        channel.capability_profiles = vec![crate::model::ChannelCapabilityProfile {
            model_pattern: "Vendor/unknown-model".into(),
            catalog_metadata: true,
            context_tokens: Some(1000000),
            image_input: true,
            audio_output: true,
            verification_state: "declared".into(),
            ..Default::default()
        }];
        let observations = super::model_observations_from_saved_channel(&channel);
        assert_eq!(observations[0].id, "Vendor/unknown-model");
        assert_eq!(observations[0].context_tokens, Some(1000000));
        assert!(
            observations[0]
                .input_modalities
                .iter()
                .any(|value| value == "image")
        );
        assert!(
            observations[0]
                .output_modalities
                .iter()
                .any(|value| value == "audio")
        );
        assert_eq!(observations[1].context_tokens, None);
        channel.capability_profiles[0].verification_state = "rejected".into();
        assert_eq!(
            super::model_observations_from_saved_channel(&channel)[0].context_tokens,
            None
        );
    }

    fn explicit_test_channel(base_url: &str) -> crate::model::ChannelConfig {
        let mut channel = channel_from_supplier("test".to_string(), &default_supplier_config());
        let source_driver = crate::source_driver::SourceDriverId::CustomEndpoint;
        channel.set_source_driver(source_driver);
        channel.upstream_base_url = base_url.to_string();
        channel.surface_bindings = crate::source_driver_surface_bindings(source_driver, base_url);
        channel
    }

    #[test]
    fn model_catalog_modalities_become_exact_experimental_media_profiles() {
        let base = basic_protocol_capability("openai_responses", "declared");
        let profiles = model_modality_capability_profiles(
            &base,
            &[crate::model_catalog::ModelObservation {
                context_tokens: None,
                output_tokens: None,
                supported_parameters: None,
                supported_endpoints: None,
                probe_cost_nanos: None,
                id: "gpt-audio-preview".to_string(),
                priority: Some(0),
                source: "codex".to_string(),
                etag: Some("catalog-v1".to_string()),
                use_responses_lite: None,
                input_modalities: vec![
                    "text".to_string(),
                    "image".to_string(),
                    "audio".to_string(),
                ],
                output_modalities: vec!["text".to_string()],
            }],
            true,
        );

        assert_eq!(profiles.len(), 1);
        let profile = &profiles[0];
        assert_eq!(profile.model_pattern, "gpt-audio-preview");
        assert_eq!(profile.capability_layer, "model_wire");
        assert!(!profile.input_modalities_authoritative);
        assert!(!profile.output_modalities_authoritative);
        assert!(profile.vision);
        assert!(profile.audio_input);
        assert!(!profile.video_input);
        assert!(!profile.audio_output);
        assert_eq!(profile.release_status, "experimental");
    }

    #[test]
    fn catalog_omissions_do_not_become_authoritative_media_rejections() {
        let base = basic_protocol_capability("openai_responses", "declared");
        let profiles = model_modality_capability_profiles(
            &base,
            &[crate::model_catalog::ModelObservation {
                context_tokens: None,
                output_tokens: None,
                supported_parameters: None,
                supported_endpoints: None,
                probe_cost_nanos: None,
                id: "gpt-5.6-sol".to_string(),
                priority: None,
                source: "openai_subscription".to_string(),
                etag: None,
                use_responses_lite: Some(true),
                input_modalities: vec!["text".to_string(), "image".to_string()],
                output_modalities: vec!["text".to_string()],
            }],
            true,
        );
        assert_eq!(profiles.len(), 1);
        let profile = &profiles[0];
        assert!(profile.image_input);
        assert!(!profile.file_input);
        assert!(!profile.audio_input);
        assert!(!profile.video_input);
        assert!(!profile.input_modalities_authoritative);
        assert!(!profile.output_modalities_authoritative);
    }

    #[test]
    fn subscription_driver_does_not_promote_wire_support_to_all_model_media() {
        let mut profile = basic_protocol_capability("openai_responses", "declared");
        apply_subscription_driver_capabilities("openai_responses", &mut profile);

        assert!(profile.parallel_tool_calls);
        assert!(profile.json_schema);
        assert!(profile.reasoning);
        assert!(!profile.vision);
        assert!(!profile.image_input);
        assert!(!profile.file_input);
        assert!(!profile.audio_input);
        assert!(!profile.video_input);

        let mut anthropic = basic_protocol_capability("anthropic_messages", "declared");
        apply_subscription_driver_capabilities("anthropic_messages", &mut anthropic);
        assert!(anthropic.thinking);
        assert!(anthropic.cache_control);
        assert!(!anthropic.vision);
        assert!(!anthropic.image_input);
        assert!(!anthropic.file_input);

        let mut gemini = basic_protocol_capability("gemini_native", "declared");
        apply_subscription_driver_capabilities("gemini_native", &mut gemini);
        assert!(gemini.json_schema);
        assert!(gemini.thinking);
        assert!(!gemini.vision);
        assert!(!gemini.image_input);
        assert!(!gemini.file_input);
    }

    #[test]
    fn codex_manifest_supplement_keeps_media_evidence_model_scoped() {
        let mut models = vec![
            crate::model_catalog::ModelObservation {
                context_tokens: None,
                output_tokens: None,
                supported_parameters: None,
                supported_endpoints: None,
                probe_cost_nanos: None,
                id: "gpt-5.6-sol".to_string(),
                priority: None,
                source: "codex_manifest".to_string(),
                etag: None,
                use_responses_lite: Some(true),
                input_modalities: vec!["text".to_string(), "image".to_string()],
                output_modalities: vec!["text".to_string()],
            },
            crate::model_catalog::ModelObservation {
                context_tokens: None,
                output_tokens: None,
                supported_parameters: None,
                supported_endpoints: None,
                probe_cost_nanos: None,
                id: "future-codex-model".to_string(),
                priority: None,
                source: "codex_manifest".to_string(),
                etag: None,
                use_responses_lite: None,
                input_modalities: vec!["text".to_string()],
                output_modalities: vec!["text".to_string()],
            },
        ];

        supplement_codex_manifest_modalities(&mut models);

        assert!(
            models[0]
                .input_modalities
                .iter()
                .any(|modality| modality == "image")
        );
        assert!(
            models[0]
                .input_modalities
                .iter()
                .any(|modality| modality == "pdf")
        );
        assert!(
            !models[0]
                .input_modalities
                .iter()
                .any(|modality| matches!(modality.as_str(), "audio" | "video"))
        );
        assert_eq!(models[1].input_modalities, ["text"]);

        let base = basic_protocol_capability("openai_responses", "declared");
        let profiles = model_modality_capability_profiles(&base, &models, true);
        let known = profiles
            .iter()
            .find(|profile| profile.model_pattern == "gpt-5.6-sol")
            .expect("known Codex model profile");
        assert!(known.image_input);
        assert!(known.file_input);
        assert!(!known.input_modalities_authoritative);
    }

    #[test]
    fn official_api_file_contract_does_not_promote_subscription_or_compatible_drivers() {
        let mut channel = explicit_test_channel("https://api.openai.com/v1");
        channel.set_source_driver(crate::source_driver::SourceDriverId::OpenAiApi);
        let mut profiles = vec![
            basic_protocol_capability("openai_responses", "verified"),
            basic_protocol_capability("openai_chat", "verified"),
        ];
        apply_official_api_file_contract(&channel, &mut profiles);
        let file_profile = profiles
            .iter()
            .find(|profile| profile.protocol == "openai_responses" && profile.file_input)
            .expect("official Responses file contract");
        assert_eq!(file_profile.verification_state, "declared");
        assert!(
            profiles
                .iter()
                .filter(|profile| profile.protocol == "openai_responses")
                .any(|profile| profile.verification_state == "verified" && !profile.file_input),
            "the text probe must remain separate from declared file support"
        );
        assert!(!profiles[1].file_input);

        channel.set_source_driver(crate::source_driver::SourceDriverId::OpenAiSubscription);
        profiles.retain(|profile| !profile.file_input);
        apply_official_api_file_contract(&channel, &mut profiles);
        assert!(
            profiles.iter().all(|profile| !profile.file_input),
            "subscription input_file needs independent driver evidence"
        );
    }

    #[test]
    fn verified_model_capability_does_not_promote_native_protocol_verification() {
        let mut channel = explicit_test_channel("https://gateway.example/v1");
        channel.id = "capability-isolation".to_string();
        let mut binding = channel.surface_bindings[0].clone();
        binding.protocols.truncate(1);
        binding.verification.state = "declared".to_string();
        binding.protocols[0].verification.state = "declared".to_string();
        let protocol = binding.protocols[0].protocol.clone();
        let source_driver = channel.source_driver();
        let context = SurfaceProbeContext {
            channel_id: channel.id.clone(),
            source_driver,
            execution_kind: crate::source_driver::source_driver(source_driver)
                .expect("source driver")
                .execution_kind(),
            credential_ref: channel.v2.credential_ref.clone(),
            binding,
            discovery: channel.v2.discovery.clone(),
            user_agent_profile: channel.v2.user_agent_profile.clone(),
            mode: SurfaceProbeMode::Creation,
            is_default_target: true,
            run_advanced: true,
            models: vec!["model-a".to_string()],
            default_model: "model-a".to_string(),
            subscription: None,
        };
        let result = surface_probe_result_from_observation(
            &context,
            ProbeObservation {
                models: vec!["model-a".to_string()],
                native_protocol_verified: false,
                quota_status: String::new(),
                remaining_ratio: 0.0,
                quota_windows: Vec::new(),
                checks: Vec::new(),
                warnings: Vec::new(),
                capability_profiles: vec![crate::model::ChannelCapabilityProfile {
                    protocol,
                    verification_state: "verified".to_string(),
                    ..Default::default()
                }],
                detection_checks: Vec::new(),
            },
        )
        .expect("surface result");

        assert_eq!(result.result.protocol_verification.state, "declared");
        assert_eq!(
            result.result.model_capability_evidence[0].verification_state,
            "verified"
        );
    }

    #[test]
    fn only_explicit_stream_rejections_disable_streaming() {
        for message in [
            "405 Method Not Allowed",
            "501 Not Implemented",
            "streaming is not supported for this model",
            "unknown parameter: stream",
        ] {
            assert!(stream_probe_explicitly_unsupported(&anyhow::anyhow!(
                message
            )));
        }
        for message in [
            "request timed out",
            "connection reset by peer",
            "401 Unauthorized",
            "429 Too Many Requests",
            "invalid prompt",
        ] {
            assert!(!stream_probe_explicitly_unsupported(&anyhow::anyhow!(
                message
            )));
        }
    }

    #[test]
    fn operation_family_probe_failures_do_not_invent_negative_capability() {
        assert_eq!(classify_surface_operation_probe_status(200), "verified");
        assert_eq!(classify_surface_operation_probe_status(404), "unsupported");
        assert_eq!(classify_surface_operation_probe_status(405), "unknown");
        assert_eq!(classify_surface_operation_probe_status(501), "unsupported");
        assert_eq!(
            classify_surface_operation_probe_status(401),
            "credential_limited"
        );
        for status in [400, 408, 409, 422, 429, 500, 502, 503, 504] {
            assert_eq!(
                classify_surface_operation_probe_status(status),
                "unknown",
                "HTTP {status} must retain prior evidence"
            );
        }
    }

    #[test]
    fn operation_family_probes_are_read_only_and_bounded() {
        for surface in [
            crate::surface::ApiSurface::OpenAi,
            crate::surface::ApiSurface::Anthropic,
            crate::surface::ApiSurface::Gemini,
        ] {
            let probes = operation_family_probes(surface);
            assert!(!probes.is_empty());
            for probe in probes {
                assert!(probe.path.starts_with('/'));
                assert!(
                    probe.path.contains("limit=1") || probe.path.contains("pageSize=1"),
                    "family probe must request at most one item: {}",
                    probe.path
                );
                assert!(!probe.capability.ends_with(".management"));
            }
        }
    }

    #[test]
    fn only_fixed_official_drivers_get_authoritative_surface_contracts() {
        use crate::source_driver::SourceDriverId;
        use crate::surface::ApiSurface;

        assert!(source_has_authoritative_surface_contract(
            SourceDriverId::OpenAiApi,
            ApiSurface::OpenAi
        ));
        assert!(source_has_authoritative_surface_contract(
            SourceDriverId::AnthropicApi,
            ApiSurface::Anthropic
        ));
        assert!(source_has_authoritative_surface_contract(
            SourceDriverId::GeminiApi,
            ApiSurface::Gemini
        ));
        assert!(!source_has_authoritative_surface_contract(
            SourceDriverId::GeminiApi,
            ApiSurface::OpenAi
        ));
        assert!(!source_has_authoritative_surface_contract(
            SourceDriverId::CustomEndpoint,
            ApiSurface::OpenAi
        ));
        assert!(!source_has_authoritative_surface_contract(
            SourceDriverId::Openrouter,
            ApiSurface::OpenAi
        ));
        assert!(should_probe_custom_surface_families(
            SourceDriverId::CustomEndpoint
        ));
        assert!(should_probe_custom_surface_families(
            SourceDriverId::Openrouter
        ));
        assert!(!should_probe_custom_surface_families(
            SourceDriverId::BedrockMantle
        ));
    }

    #[test]
    fn official_sources_use_stable_protocol_contracts() {
        let mut channel = channel_from_supplier("test".to_string(), &default_supplier_config());

        channel.set_source_driver(crate::source_driver::SourceDriverId::OpenAiApi);
        channel.kind = "openai".to_string();
        channel.upstream_base_url = "https://api.openai.com/v1".to_string();
        assert_eq!(
            known_source_protocol_contract(&channel),
            Some(vec!["openai_responses", "openai_chat"])
        );

        channel.set_source_driver(crate::source_driver::SourceDriverId::AnthropicApi);
        channel.kind = "anthropic".to_string();
        channel.upstream_base_url = "https://api.anthropic.com".to_string();
        assert_eq!(
            known_source_protocol_contract(&channel),
            Some(vec!["anthropic_messages"])
        );

        channel.set_source_driver(crate::source_driver::SourceDriverId::GeminiApi);
        channel.kind = "gemini".to_string();
        channel.upstream_base_url = "https://generativelanguage.googleapis.com".to_string();
        assert_eq!(
            known_source_protocol_contract(&channel),
            Some(vec!["gemini_native", "openai_chat"])
        );

        channel.set_source_driver(crate::source_driver::SourceDriverId::BedrockMantle);
        channel.kind = "aws_bedrock".to_string();
        channel.upstream_base_url = "https://bedrock-mantle.us-east-1.api.aws/v1".to_string();
        channel.api_format = "openai_responses".to_string();
        assert_eq!(
            known_source_protocol_contract(&channel),
            Some(vec![
                "openai_responses",
                "openai_chat",
                "anthropic_messages"
            ])
        );
    }

    #[test]
    fn custom_endpoint_does_not_inherit_an_official_contract_from_its_host() {
        let mut channel = channel_from_supplier("test".to_string(), &default_supplier_config());
        channel.kind = "custom_endpoint".to_string();
        channel.upstream_base_url = "https://api.openai.com/v1".to_string();
        channel.api_format = "openai_chat".to_string();

        assert_eq!(known_source_protocol_contract(&channel), None);
    }

    #[test]
    fn gemini_openai_preference_keeps_its_special_base_format() {
        let mut channel = channel_from_supplier("test".to_string(), &default_supplier_config());
        channel.kind = "gemini".to_string();
        channel.api_format = "gemini_openai".to_string();

        assert_eq!(
            preferred_detected_protocol(
                &channel,
                &["gemini_native".to_string(), "openai_chat".to_string()]
            ),
            "gemini_openai"
        );
    }

    #[test]
    fn compatible_sources_probe_only_selected_protocols() {
        let mut channel = explicit_test_channel("https://gateway.example/v1");
        channel.kind = "custom_endpoint".to_string();
        channel.api_format = "openai_chat".to_string();
        channel.supported_protocols = vec!["openai_chat".to_string()];

        assert_eq!(known_source_protocol_contract(&channel), None);
        assert_eq!(
            requested_probe_protocols(&channel, &["openai_responses", "openai_chat"]),
            vec!["openai_chat"]
        );

        channel.api_format = "openai_responses".to_string();
        channel.supported_protocols = vec!["openai_responses".to_string()];
        assert_eq!(
            requested_probe_protocols(&channel, &["openai_responses", "openai_chat"]),
            vec!["openai_responses"]
        );

        channel.api_format = "anthropic_messages".to_string();
        channel.supported_protocols = vec!["anthropic_messages".to_string()];
        channel.v2.default_target = crate::model::ChannelTarget {
            surface: crate::surface::ApiSurface::Anthropic,
            protocol: crate::protocol::kind::ProtocolKind::AnthropicMessages,
        };
        channel.surface_bindings[0].surface = crate::surface::ApiSurface::Anthropic;
        channel.surface_bindings[0].protocols[0].protocol = "anthropic_messages".to_string();
        assert_eq!(
            channel_provider_hint(&normalize_probe_channel(channel.clone())),
            "anthropic"
        );

        channel.api_format = "gemini_native".to_string();
        channel.supported_protocols = vec!["gemini_native".to_string()];
        channel.v2.default_target = crate::model::ChannelTarget {
            surface: crate::surface::ApiSurface::Gemini,
            protocol: crate::protocol::kind::ProtocolKind::GeminiNative,
        };
        channel.surface_bindings[0].surface = crate::surface::ApiSurface::Gemini;
        channel.surface_bindings[0].protocols[0].protocol = "gemini_native".to_string();
        assert_eq!(
            channel_provider_hint(&normalize_probe_channel(channel)),
            "gemini"
        );
    }

    #[test]
    fn openrouter_provider_hint_follows_the_projected_surface() {
        let mut channel = explicit_test_channel("https://openrouter.ai/api");
        channel.set_source_driver(crate::source_driver::SourceDriverId::Openrouter);
        channel.v2.default_target = crate::model::ChannelTarget {
            surface: crate::surface::ApiSurface::Anthropic,
            protocol: crate::protocol::kind::ProtocolKind::AnthropicMessages,
        };
        channel.surface_bindings[0].surface = crate::surface::ApiSurface::Anthropic;
        channel.surface_bindings[0].protocols[0].protocol = "anthropic_messages".to_string();

        assert_eq!(
            channel_provider_hint(&normalize_probe_channel(channel)),
            "anthropic"
        );
    }
}

async fn detect_local_model_channel(
    client: &Client,
    channel: &ChannelConfig,
    checks: Vec<String>,
    warnings: Vec<String>,
    run_advanced: bool,
) -> Result<ProbeObservation> {
    detect_openai_family_channel(client, channel, checks, warnings, run_advanced).await
}

pub(crate) async fn fetch_openai_compatible_catalog(
    client: &Client,
    channel: &ChannelConfig,
    base_url: &str,
    api_key: &str,
) -> Result<Vec<crate::model_catalog::ModelObservation>> {
    crate::coding_plan::validate_key(channel)?;
    let url = crate::coding_gateway::catalog_url(channel.source_driver(), base_url)?;
    let mut headers = HeaderMap::new();
    crate::channel_user_agent::apply_to_headers(channel, &mut headers);
    if !api_key.trim().is_empty() {
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", api_key.trim()))?,
        );
        headers.insert("x-api-key", HeaderValue::from_str(api_key.trim())?);
        if is_azure_openai_base_url(base_url) {
            headers.insert("api-key", HeaderValue::from_str(api_key.trim())?);
        }
    }
    let kind = match channel.source_driver() {
        crate::source_driver::SourceDriverId::OllamaCloud => ModelCatalogKind::Ollama,
        crate::source_driver::SourceDriverId::CommandCode => ModelCatalogKind::CommandCode,
        _ => ModelCatalogKind::OpenAI,
    };
    fetch_model_catalog(client, url, headers, kind).await
}

fn openai_subscription_catalog_cache_key(channel: &ChannelConfig) -> Result<String> {
    let credential_path = subscription_credential_path(channel)?;
    Ok(format!(
        "{}\u{0}{}",
        channel.id.trim(),
        credential_path.to_string_lossy()
    ))
}

fn openai_subscription_catalog_cache_slot(key: &str) -> Result<OpenAiSubscriptionCatalogCacheSlot> {
    let mut slots = OPENAI_SUBSCRIPTION_CATALOG_CACHE
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .map_err(|_| anyhow!("OpenAI subscription catalog cache registry poisoned"))?;
    Ok(slots
        .entry(key.to_string())
        .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(None)))
        .clone())
}

/// Returns the Codex transport dialect for one concrete subscription model without
/// waiting on control-plane I/O. A live manifest observation wins; the bundled
/// fallback only covers model IDs shipped with this client release.
pub(crate) fn openai_subscription_responses_lite_hint(
    config: &SupplierConfig,
    model: &str,
) -> Option<bool> {
    let channel_id = if config.channel_id.trim().is_empty() {
        "subscription-runtime".to_string()
    } else {
        config.channel_id.clone()
    };
    let channel = channel_from_supplier(channel_id, config);
    let rejection_key = openai_responses_lite_rejection_key(&channel, model);
    if rejection_key.is_some_and(|key| {
        OPENAI_RESPONSES_LITE_REJECTIONS
            .get()
            .and_then(|rejections| rejections.lock().ok())
            .is_some_and(|rejections| rejections.contains(&key))
    }) {
        return Some(false);
    }
    let live = openai_subscription_catalog_cache_key(&channel)
        .ok()
        .and_then(|key| {
            OPENAI_SUBSCRIPTION_CATALOG_CACHE
                .get()
                .and_then(|slots| slots.lock().ok())
                .and_then(|slots| slots.get(&key).cloned())
        })
        .and_then(|slot| {
            let cached = slot.try_lock().ok()?;
            cached.as_ref().and_then(|entry| {
                entry
                    .models
                    .iter()
                    .find(|entry| entry.id.eq_ignore_ascii_case(model.trim()))
                    .and_then(|entry| entry.use_responses_lite)
            })
        });
    live.or_else(|| bundled_codex_responses_lite_hint(model))
}

pub(crate) fn observe_openai_subscription_responses_lite_rejection(
    config: &SupplierConfig,
    model: &str,
) {
    let channel_id = if config.channel_id.trim().is_empty() {
        "subscription-runtime".to_string()
    } else {
        config.channel_id.clone()
    };
    let channel = channel_from_supplier(channel_id, config);
    let Some(key) = openai_responses_lite_rejection_key(&channel, model) else {
        return;
    };
    if let Ok(mut rejections) = OPENAI_RESPONSES_LITE_REJECTIONS
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
        .lock()
    {
        rejections.insert(key);
    }
}

fn openai_responses_lite_rejection_key(channel: &ChannelConfig, model: &str) -> Option<String> {
    let model = model.trim().to_ascii_lowercase();
    if model.is_empty() {
        return None;
    }
    openai_subscription_catalog_cache_key(channel)
        .ok()
        .map(|key| format!("{key}\u{0}{model}"))
}

fn bundled_codex_responses_lite_hint(model: &str) -> Option<bool> {
    match model.trim().to_ascii_lowercase().as_str() {
        "gpt-5.6-sol"
        | "gpt-5.6-terra"
        | "gpt-5.6-luna"
        | "gpt-daybreak-blue-latest"
        | "gpt-daybreak-red-latest"
        | "codex-auto-review" => Some(true),
        "gpt-5.5" | "gpt-5.4" | "gpt-5.4-mini" | "gpt-5.2" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
#[test]
fn provider_rejection_overrides_the_manifest_lite_hint_for_the_process() {
    let mut config = crate::default_supplier_config();
    config.channel_id = "responses-lite-rejection-test".to_string();
    config.credential_ref = "C:/const-api-test/responses-lite.json".to_string();
    config.subscription.credential_ref = config.credential_ref.clone();

    assert_eq!(
        openai_subscription_responses_lite_hint(&config, "gpt-5.6-sol"),
        Some(true)
    );
    observe_openai_subscription_responses_lite_rejection(&config, "gpt-5.6-sol");
    assert_eq!(
        openai_subscription_responses_lite_hint(&config, "gpt-5.6-sol"),
        Some(false)
    );
}

fn openai_model_etag_hint(key: &str) -> Option<(String, Instant)> {
    OPENAI_MODEL_ETAG_HINTS
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .ok()
        .and_then(|hints| hints.get(key).cloned())
}

fn clear_openai_model_etag_hint_observed_no_later_than(key: &str, cutoff: Instant) {
    if let Ok(mut hints) = OPENAI_MODEL_ETAG_HINTS
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
    {
        if hints
            .get(key)
            .is_some_and(|(_, observed_at)| *observed_at <= cutoff)
        {
            hints.remove(key);
        }
    }
}

fn subscription_catalog_error_allows_stale(error: &anyhow::Error) -> bool {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<reqwest::Error>())
        .is_some_and(|error| {
            error.status().map_or_else(
                || {
                    error.is_connect()
                        || error.is_timeout()
                        || error.is_request()
                        || error.is_body()
                },
                |status| {
                    status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS
                },
            )
        })
}

pub(crate) fn observe_openai_subscription_response_headers(
    channel: &ChannelConfig,
    headers: &HeaderMap,
) {
    let Ok(key) = openai_subscription_catalog_cache_key(channel) else {
        return;
    };
    let now = Instant::now();
    let model_etag = headers
        .get("x-models-etag")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= 1024)
        .map(str::to_string);
    if let Some(model_etag) = model_etag.as_ref() {
        if let Ok(mut hints) = OPENAI_MODEL_ETAG_HINTS
            .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
            .lock()
        {
            hints.insert(key.clone(), (model_etag.clone(), now));
        }
    }

    let borrowed = headers
        .iter()
        .filter_map(|(name, value)| value.to_str().ok().map(|value| (name.as_str(), value)))
        .collect::<Vec<_>>();
    let quota_windows = crate::supplier::parse_codex_quota_windows(&borrowed);
    observe_subscription_quota(channel, &quota_windows);
    let slot = OPENAI_SUBSCRIPTION_CATALOG_CACHE
        .get()
        .and_then(|slots| slots.lock().ok())
        .and_then(|slots| slots.get(&key).cloned());
    let Some(slot) = slot else {
        return;
    };
    let Ok(mut cached) = slot.try_lock() else {
        return;
    };
    let Some(cached) = cached.as_mut() else {
        return;
    };
    if let Some(model_etag) = model_etag {
        if cached.model_etag.as_deref() == Some(model_etag.as_str()) {
            cached.model_refresh_after = now + OPENAI_MODEL_CATALOG_REFRESH_INTERVAL;
        } else {
            cached.model_refresh_after = now;
        }
    }
}

pub(crate) fn observe_openai_subscription_response_event(channel: &ChannelConfig, text: &str) {
    let Ok(event) = serde_json::from_str::<serde_json::Value>(text) else {
        return;
    };
    if event.get("type").and_then(serde_json::Value::as_str) == Some("codex.rate_limits") {
        observe_subscription_quota(channel, &openai_quota_windows_from_event(&event));
        return;
    }
    if !matches!(
        event.get("type").and_then(serde_json::Value::as_str),
        Some("response.metadata" | "codex.response.metadata")
    ) {
        return;
    }
    let Some(headers) = event.get("headers").and_then(serde_json::Value::as_object) else {
        return;
    };
    let mut observed = HeaderMap::new();
    for (name, value) in headers {
        if !name.eq_ignore_ascii_case("x-models-etag")
            && !name.to_ascii_lowercase().starts_with("x-codex-")
        {
            continue;
        }
        if let (Ok(name), Some(value)) = (
            reqwest::header::HeaderName::from_bytes(name.as_bytes()),
            value.as_str(),
        ) {
            if let Ok(value) = HeaderValue::from_str(value) {
                observed.insert(name, value);
            }
        }
    }
    observe_openai_subscription_response_headers(channel, &observed);
}

pub(crate) fn observe_claude_subscription_response_headers(
    channel: &ChannelConfig,
    headers: &HeaderMap,
) {
    let borrowed = headers
        .iter()
        .filter_map(|(name, value)| value.to_str().ok().map(|value| (name.as_str(), value)))
        .collect::<Vec<_>>();
    let quota_windows = crate::supplier::parse_claude_quota_windows(&borrowed);
    observe_subscription_quota(channel, &quota_windows);
}

async fn fetch_openai_subscription_catalog_with_policy(
    client: &Client,
    channel: &ChannelConfig,
    policy: SubscriptionCatalogPolicy,
) -> Result<(
    Vec<crate::model_catalog::ModelObservation>,
    bool,
    Vec<QuotaWindow>,
)> {
    let key = openai_subscription_catalog_cache_key(channel)?;
    let mut quota_refreshed = false;
    let quota_windows = refresh_subscription_quota(channel, policy, async {
        let (token, credential, refreshed) =
            ensure_openai_subscription_access_token(client, channel).await?;
        quota_refreshed = refreshed;
        fetch_openai_subscription_quota_windows(client, &token, &credential).await
    })
    .await?;
    let slot = openai_subscription_catalog_cache_slot(&key)?;
    let mut cached = slot.lock().await;
    let now = Instant::now();
    if policy == SubscriptionCatalogPolicy::Cached && cached.is_none() && !channel.models.is_empty()
    {
        let mut models = model_observations_from_saved_channel(channel);
        supplement_codex_manifest_modalities(&mut models);
        *cached = Some(OpenAiSubscriptionCatalogCacheEntry {
            models,
            model_refresh_after: now + OPENAI_MODEL_CATALOG_REFRESH_INTERVAL,
            model_etag: None,
        });
    }
    if let (Some(entry), Some((hint, observed_at))) =
        (cached.as_mut(), openai_model_etag_hint(&key))
    {
        if entry.model_etag.as_deref() == Some(hint.as_str()) {
            entry.model_refresh_after = entry
                .model_refresh_after
                .max(observed_at + OPENAI_MODEL_CATALOG_REFRESH_INTERVAL);
        } else {
            entry.model_refresh_after = now;
        }
    }

    let refresh_models = subscription_refresh_due(
        policy,
        cached.as_ref().map(|entry| entry.model_refresh_after),
        now,
    );
    if !refresh_models {
        let Some(entry) = cached.as_ref() else {
            return Err(anyhow!("OpenAI subscription catalog cache is unavailable"));
        };
        return Ok((entry.models.clone(), quota_refreshed, quota_windows));
    }

    let (token, credential, refreshed) = ensure_openai_subscription_access_token(client, channel)
        .await
        .context("prepare OpenAI subscription credential")?;
    let account_id = json_string(&credential, &["account_id"])
        .or_else(|| json_string(&credential, &["accountID"]))
        .filter(|value| !value.trim().is_empty());

    if refresh_models {
        let fetched = fetch_codex_manifest_model_observations_with_resolver(
            client,
            codex_identity_resolver(),
            CODEX_MODELS_URL,
            &token,
            account_id.as_deref(),
        )
        .await;
        match fetched {
            Ok(mut models) => {
                supplement_codex_manifest_modalities(&mut models);
                let model_etag = models.iter().find_map(|model| model.etag.clone());
                match cached.as_mut() {
                    Some(entry) => {
                        entry.models = models;
                        entry.model_refresh_after = now + OPENAI_MODEL_CATALOG_REFRESH_INTERVAL;
                        entry.model_etag = model_etag;
                    }
                    None => {
                        *cached = Some(OpenAiSubscriptionCatalogCacheEntry {
                            models,
                            model_refresh_after: now + OPENAI_MODEL_CATALOG_REFRESH_INTERVAL,
                            model_etag,
                        });
                    }
                }
                // Consume only ETag evidence that existed when this fetch started.
                // A real response can publish a newer ETag while the cache lock is
                // held; retaining that later evidence schedules exactly one follow-up
                // refresh instead of losing it or polling on every 30-second snapshot.
                clear_openai_model_etag_hint_observed_no_later_than(&key, now);
            }
            Err(error)
                if policy == SubscriptionCatalogPolicy::Cached
                    && subscription_catalog_error_allows_stale(&error)
                    && cached.is_some() =>
            {
                let Some(entry) = cached.as_mut() else {
                    return Err(error);
                };
                entry.model_refresh_after = now + OPENAI_MODEL_CATALOG_REFRESH_INTERVAL;
                log::warn!(
                    "[const-api][codex-catalog] using stale model catalog after refresh error: {error:#}"
                );
            }
            Err(error) => return Err(error),
        }
    }

    let entry = cached
        .as_ref()
        .ok_or_else(|| anyhow!("OpenAI subscription catalog refresh produced no cache entry"))?;
    Ok((
        entry.models.clone(),
        refreshed || quota_refreshed,
        quota_windows,
    ))
}

#[cfg(test)]
mod openai_subscription_catalog_cache_tests {
    use super::*;

    #[test]
    fn websocket_metadata_updates_the_model_catalog_etag_hint() {
        let mut channel = crate::channel_from_supplier(
            format!("ws-etag-channel-{}", rand::random::<u64>()),
            &crate::default_supplier_config(),
        );
        channel.set_source_driver(crate::source_driver::SourceDriverId::OpenAiSubscription);
        channel.subscription.credential_ref =
            format!("ws-etag-credential-{}.json", rand::random::<u64>());
        let key = openai_subscription_catalog_cache_key(&channel).expect("cache key");

        observe_openai_subscription_response_event(
            &channel,
            r#"{"type":"codex.response.metadata","headers":{"X-Models-Etag":"models-v150"}}"#,
        );

        assert_eq!(
            openai_model_etag_hint(&key).map(|(etag, _)| etag),
            Some("models-v150".to_string())
        );
        clear_openai_model_etag_hint_observed_no_later_than(&key, Instant::now());
    }

    #[test]
    fn websocket_non_metadata_events_do_not_update_the_model_catalog_hint() {
        let mut channel = crate::channel_from_supplier(
            format!("ws-etag-ignore-channel-{}", rand::random::<u64>()),
            &crate::default_supplier_config(),
        );
        channel.set_source_driver(crate::source_driver::SourceDriverId::OpenAiSubscription);
        channel.subscription.credential_ref =
            format!("ws-etag-ignore-credential-{}.json", rand::random::<u64>());
        let key = openai_subscription_catalog_cache_key(&channel).expect("cache key");

        observe_openai_subscription_response_event(
            &channel,
            r#"{"type":"response.completed","headers":{"x-models-etag":"ignored"}}"#,
        );

        assert!(openai_model_etag_hint(&key).is_none());
    }

    #[test]
    fn catalog_refresh_consumes_only_etag_evidence_it_started_with() {
        let key = format!("etag-hint-test-{}", rand::random::<u64>());
        let started_at = Instant::now();
        let observed_during_fetch = started_at + Duration::from_secs(1);
        {
            let mut hints = OPENAI_MODEL_ETAG_HINTS
                .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
                .lock()
                .expect("ETag hints lock");
            hints.insert(
                key.clone(),
                ("models-v2".to_string(), observed_during_fetch),
            );
        }

        clear_openai_model_etag_hint_observed_no_later_than(&key, started_at);
        assert_eq!(
            openai_model_etag_hint(&key),
            Some(("models-v2".to_string(), observed_during_fetch))
        );

        clear_openai_model_etag_hint_observed_no_later_than(&key, observed_during_fetch);
        assert!(openai_model_etag_hint(&key).is_none());
    }

    #[tokio::test]
    async fn periodic_snapshot_seeds_cache_from_persisted_models_without_network() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing_credential = dir.path().join("missing.json");
        let mut channel = crate::channel_from_supplier(
            "cached-openai-subscription".to_string(),
            &crate::default_supplier_config(),
        );
        channel.set_source_driver(crate::source_driver::SourceDriverId::OpenAiSubscription);
        channel.subscription.credential_ref = missing_credential.to_string_lossy().to_string();
        channel.models = vec!["gpt-cached".to_string()];

        let (models, refreshed, quota) = fetch_openai_subscription_catalog_with_policy(
            &Client::new(),
            &channel,
            SubscriptionCatalogPolicy::Cached,
        )
        .await
        .expect("cached snapshot");

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gpt-cached");
        assert!(!refreshed);
        assert!(quota.is_empty());
        assert!(!missing_credential.exists());
    }
}

fn supplement_codex_manifest_modalities(models: &mut [crate::model_catalog::ModelObservation]) {
    let ids = models
        .iter()
        .map(|model| model.id.clone())
        .collect::<Vec<_>>();
    let catalog = crate::tool_model_metadata::tool_models_from_ids(&ids)
        .into_iter()
        .map(|model| (model.id.to_ascii_lowercase(), model))
        .collect::<std::collections::HashMap<_, _>>();

    for observation in models {
        let Some(metadata) = catalog.get(&observation.id.to_ascii_lowercase()) else {
            continue;
        };
        let manifest_omitted_modalities = observation.input_modalities.is_empty();
        for modality in &metadata.input_modalities {
            // The current Codex manifest enum can express text/image/audio, but not file/pdf.
            // Supplement that missing vocabulary from the packaged model catalog. For legacy
            // manifests that omit the field entirely, Codex itself defaults to text+image, so the
            // packaged image declaration is also a valid compatibility fallback.
            let supplement = matches!(modality.as_str(), "file" | "pdf")
                || (manifest_omitted_modalities && modality == "image");
            if supplement
                && !observation
                    .input_modalities
                    .iter()
                    .any(|existing| existing.eq_ignore_ascii_case(modality))
            {
                observation.input_modalities.push(modality.clone());
            }
        }
    }
}

#[cfg(test)]
pub(crate) async fn fetch_codex_manifest_models_with_resolver(
    client: &Client,
    resolver: &CodexIdentityResolver,
    models_url: &str,
    token: &str,
    account_id: Option<&str>,
) -> Result<Vec<String>> {
    Ok(model_ids(
        fetch_codex_manifest_model_observations_with_resolver(
            client, resolver, models_url, token, account_id,
        )
        .await?,
    ))
}

pub(crate) async fn fetch_codex_manifest_model_observations_with_resolver(
    client: &Client,
    resolver: &CodexIdentityResolver,
    models_url: &str,
    token: &str,
    account_id: Option<&str>,
) -> Result<Vec<crate::model_catalog::ModelObservation>> {
    let active = resolver.active();
    fetch_codex_manifest_model_observations_for_identity(
        client, models_url, token, account_id, &active,
    )
    .await
}

async fn fetch_codex_manifest_model_observations_for_identity(
    client: &Client,
    models_url: &str,
    token: &str,
    account_id: Option<&str>,
    identity: &CodexClientIdentity,
) -> Result<Vec<crate::model_catalog::ModelObservation>> {
    let mut url = reqwest::Url::parse(models_url)?;
    apply_codex_client_version_query(&mut url, identity);
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {token}"))?,
    );
    headers.extend(codex_model_identity_headers(identity));
    if let Some(account_id) = account_id {
        headers.insert("chatgpt-account-id", HeaderValue::from_str(&account_id)?);
    }
    fetch_model_catalog(client, url, headers, ModelCatalogKind::Codex).await
}

fn claude_subscription_catalog_cache_key(channel: &ChannelConfig) -> Result<String> {
    let credential_path = subscription_credential_path(channel)?;
    Ok(format!(
        "{}\u{0}{}",
        channel.id.trim(),
        credential_path.to_string_lossy()
    ))
}

fn claude_subscription_catalog_cache_slot(key: &str) -> Result<ClaudeSubscriptionCatalogCacheSlot> {
    let mut slots = CLAUDE_SUBSCRIPTION_CATALOG_CACHE
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .map_err(|_| anyhow!("Claude subscription catalog cache registry poisoned"))?;
    Ok(slots
        .entry(key.to_string())
        .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(None)))
        .clone())
}

async fn fetch_claude_subscription_catalog_with_policy(
    client: &Client,
    channel: &ChannelConfig,
    policy: SubscriptionCatalogPolicy,
) -> Result<(
    Vec<crate::model_catalog::ModelObservation>,
    bool,
    Vec<QuotaWindow>,
    Vec<String>,
)> {
    let key = claude_subscription_catalog_cache_key(channel)?;
    let mut quota_refreshed = false;
    let quota_windows = refresh_subscription_quota(channel, policy, async {
        let (token, _, refreshed) =
            ensure_claude_subscription_access_token(client, channel).await?;
        quota_refreshed = refreshed;
        fetch_claude_subscription_quota_windows(client, &token).await
    })
    .await?;
    let slot = claude_subscription_catalog_cache_slot(&key)?;
    let mut cached = slot.lock().await;
    let now = Instant::now();
    if policy == SubscriptionCatalogPolicy::Cached && cached.is_none() && !channel.models.is_empty()
    {
        *cached = Some(ClaudeSubscriptionCatalogCacheEntry {
            models: model_observations_from_saved_channel(channel),
            model_refresh_after: now + CLAUDE_MODEL_CATALOG_REFRESH_INTERVAL,
        });
    }
    // Model discovery is control-plane traffic, not a paid inference request.
    // Keep a simple five-minute cadence and preserve the last usable catalog
    // across transient refresh failures.
    let refresh_models = subscription_refresh_due(
        policy,
        cached.as_ref().map(|entry| entry.model_refresh_after),
        now,
    );
    if !refresh_models {
        let Some(entry) = cached.as_ref() else {
            return Err(anyhow!("Claude subscription catalog cache is unavailable"));
        };
        return Ok((
            entry.models.clone(),
            quota_refreshed,
            quota_windows,
            Vec::new(),
        ));
    }

    let (token, _credential, refreshed) = ensure_claude_subscription_access_token(client, channel)
        .await
        .context("prepare Claude subscription credential")?;
    let mut warnings = Vec::new();

    if refresh_models {
        match fetch_claude_subscription_models_with_token(client, &token).await {
            Ok(models) if !models.is_empty() => match cached.as_mut() {
                Some(entry) => {
                    entry.models = models;
                    entry.model_refresh_after = now + CLAUDE_MODEL_CATALOG_REFRESH_INTERVAL;
                }
                None => {
                    *cached = Some(ClaudeSubscriptionCatalogCacheEntry {
                        models,
                        model_refresh_after: now + CLAUDE_MODEL_CATALOG_REFRESH_INTERVAL,
                    });
                }
            },
            Ok(_) => {
                return Err(anyhow!(
                    "Claude subscription model catalog returned no models"
                ));
            }
            Err(error)
                if policy == SubscriptionCatalogPolicy::Cached
                    && subscription_catalog_error_allows_stale(&error)
                    && cached.is_some() =>
            {
                let Some(entry) = cached.as_mut() else {
                    return Err(error);
                };
                entry.model_refresh_after = now + CLAUDE_MODEL_CATALOG_REFRESH_INTERVAL;
                warnings.push(format!(
                    "Claude model catalog refresh unavailable; using the last catalog: {error}"
                ));
                log::warn!(
                    "[const-api][claude-catalog] preserving cached models after refresh error: {error:#}"
                );
            }
            Err(error) => return Err(error),
        }
    }

    let entry = cached
        .as_ref()
        .ok_or_else(|| anyhow!("Claude subscription catalog refresh produced no cache entry"))?;
    Ok((
        entry.models.clone(),
        refreshed || quota_refreshed,
        quota_windows,
        warnings,
    ))
}

async fn fetch_claude_subscription_models_with_token(
    client: &Client,
    token: &str,
) -> Result<Vec<crate::model_catalog::ModelObservation>> {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {token}"))?,
    );
    headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
    headers.insert(
        "anthropic-beta",
        HeaderValue::from_static(CLAUDE_CATALOG_BETA_HEADER),
    );
    headers.insert(USER_AGENT, HeaderValue::from_static(CLAUDE_CODE_USER_AGENT));
    headers.insert("x-app", HeaderValue::from_static("cli"));
    headers.insert("x-stainless-lang", HeaderValue::from_static("js"));
    headers.insert(
        "x-stainless-package-version",
        HeaderValue::from_static(CLAUDE_CODE_STAINLESS_PACKAGE_VERSION),
    );
    headers.insert(
        "x-stainless-os",
        HeaderValue::from_static(claude_code_stainless_os()),
    );
    headers.insert(
        "x-stainless-arch",
        HeaderValue::from_static(claude_code_stainless_arch()),
    );
    headers.insert("x-stainless-runtime", HeaderValue::from_static("node"));
    headers.insert(
        "x-stainless-runtime-version",
        HeaderValue::from_static(CLAUDE_CODE_STAINLESS_RUNTIME_VERSION),
    );
    headers.insert("x-stainless-retry-count", HeaderValue::from_static("0"));
    headers.insert("x-stainless-timeout", HeaderValue::from_static("600"));
    headers.insert(
        "anthropic-dangerous-direct-browser-access",
        HeaderValue::from_static(CLAUDE_DIRECT_BROWSER_ACCESS),
    );
    let url = Url::parse("https://api.anthropic.com/v1/models")?;
    fetch_model_catalog(client, url, headers, ModelCatalogKind::Anthropic).await
}

pub(crate) async fn fetch_claude_subscription_quota_windows(
    client: &Client,
    token: &str,
) -> Result<Vec<QuotaWindow>> {
    let response = client
        .get(CLAUDE_OAUTH_USAGE_URL)
        .header("Accept", "application/json, text/plain, */*")
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", token.trim()))
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("User-Agent", CLAUDE_CODE_CONTROL_USER_AGENT)
        .send_adaptive()
        .await?;
    let value = quota_response_json(response, "Claude OAuth usage").await?;
    Ok(claude_quota_windows_from_usage(&value))
}

#[cfg(test)]
mod claude_subscription_catalog_cache_tests {
    use super::*;

    #[tokio::test]
    async fn periodic_snapshot_seeds_cache_from_persisted_models_without_network() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing_credential = dir.path().join("missing.json");
        let mut channel = crate::channel_from_supplier(
            "cached-claude-subscription".to_string(),
            &crate::default_supplier_config(),
        );
        channel.set_source_driver(crate::source_driver::SourceDriverId::ClaudeSubscription);
        channel.subscription.credential_ref = missing_credential.to_string_lossy().to_string();
        channel.models = vec!["claude-cached".to_string()];

        let (models, refreshed, quota, warnings) = fetch_claude_subscription_catalog_with_policy(
            &Client::new(),
            &channel,
            SubscriptionCatalogPolicy::Cached,
        )
        .await
        .expect("cached snapshot");

        assert_eq!(
            crate::model_catalog::model_ids(models),
            vec!["claude-cached"]
        );
        assert!(!refreshed);
        assert!(quota.is_empty());
        assert!(warnings.is_empty());
        assert!(!missing_credential.exists());
    }
}

pub(crate) fn claude_quota_windows_from_usage(value: &serde_json::Value) -> Vec<QuotaWindow> {
    let checked_at_unix = now_unix();
    let Some(fields) = value.as_object() else {
        return Vec::new();
    };
    fields
        .iter()
        .filter_map(|(field, item)| {
            if item.get("is_enabled").and_then(serde_json::Value::as_bool) == Some(false) {
                return None;
            }
            // OAuth usage is a percentage (1 means 1%), unlike unified response
            // headers which carry a fraction. Never infer units from the value.
            let used_percent = numeric_json_value(item.get("utilization"))
                .filter(|value| value.is_finite())?
                .max(0.0);
            let window = match field.as_str() {
                "five_hour" => "5h",
                "seven_day" => "weekly",
                "seven_day_sonnet" => "weekly_sonnet",
                "seven_day_overage_included" => "weekly_overage_included",
                other => other,
            };
            Some(QuotaWindow {
                source: "anthropic_oauth_usage".to_string(),
                window: window.to_string(),
                remaining_ratio: (1.0 - used_percent / 100.0).clamp(0.0, 1.0),
                used_percent,
                reset_at_unix: json_string(item, &["resets_at"])
                    .as_deref()
                    .and_then(parse_token_expiry_text)
                    .unwrap_or_default(),
                checked_at_unix,
                // Additional usage buckets are informational, not exact model IDs.
                model: None,
                token_type: None,
            })
        })
        .collect()
}

fn grok_subscription_catalog_cache_key(channel: &ChannelConfig) -> Result<String> {
    let credential_path = subscription_credential_path(channel)?;
    Ok(format!(
        "{}\u{0}{}",
        channel.id.trim(),
        credential_path.to_string_lossy()
    ))
}

fn grok_subscription_catalog_cache_slot(key: &str) -> Result<GrokSubscriptionCatalogCacheSlot> {
    let mut slots = GROK_SUBSCRIPTION_CATALOG_CACHE
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .map_err(|_| anyhow!("Grok subscription catalog cache registry poisoned"))?;
    Ok(slots
        .entry(key.to_string())
        .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(None)))
        .clone())
}

async fn fetch_grok_subscription_catalog_with_policy(
    client: &Client,
    channel: &ChannelConfig,
    policy: SubscriptionCatalogPolicy,
) -> Result<(
    Vec<crate::model_catalog::ModelObservation>,
    bool,
    Vec<String>,
)> {
    let key = grok_subscription_catalog_cache_key(channel)?;
    let slot = grok_subscription_catalog_cache_slot(&key)?;
    let mut cached = slot.lock().await;
    let now = Instant::now();
    let refresh_models = subscription_refresh_due(
        policy,
        cached.as_ref().map(|entry| entry.model_refresh_after),
        now,
    );
    if !refresh_models {
        let Some(entry) = cached.as_ref() else {
            return Err(anyhow!("Grok subscription catalog cache is unavailable"));
        };
        return Ok((entry.models.clone(), false, Vec::new()));
    }

    let (token, _credential, refreshed) = ensure_grok_subscription_access_token(client, channel)
        .await
        .context("prepare Grok subscription credential")?;
    let mut warnings = Vec::new();
    match fetch_grok_subscription_catalog(client, &token).await {
        Ok(models) if !models.is_empty() => match cached.as_mut() {
            Some(entry) => {
                entry.models = models;
                entry.model_refresh_after = now + GROK_MODEL_CATALOG_REFRESH_INTERVAL;
            }
            None => {
                *cached = Some(GrokSubscriptionCatalogCacheEntry {
                    models,
                    model_refresh_after: now + GROK_MODEL_CATALOG_REFRESH_INTERVAL,
                });
            }
        },
        Ok(_) => {
            return Err(anyhow!(
                "Grok subscription model catalog returned no text models"
            ));
        }
        Err(error)
            if policy == SubscriptionCatalogPolicy::Cached
                && subscription_catalog_error_allows_stale(&error)
                && cached.is_some() =>
        {
            let Some(entry) = cached.as_mut() else {
                return Err(error);
            };
            entry.model_refresh_after = now + GROK_MODEL_CATALOG_REFRESH_INTERVAL;
            warnings.push(format!(
                "Grok model catalog refresh unavailable; using the last account catalog: {error}"
            ));
            log::warn!(
                "[const-api][grok-catalog] preserving cached models after transient refresh error: {error:#}"
            );
        }
        Err(error) => return Err(error),
    }

    let entry = cached
        .as_ref()
        .ok_or_else(|| anyhow!("Grok subscription catalog refresh produced no cache entry"))?;
    Ok((entry.models.clone(), refreshed, warnings))
}

#[cfg(test)]
mod grok_subscription_catalog_cache_tests {
    use super::*;

    fn grok_channel_with_saved_models(credential_path: &Path) -> ChannelConfig {
        let mut channel = crate::channel_from_supplier(
            format!("cached-grok-subscription-{}", rand::random::<u64>()),
            &crate::default_supplier_config(),
        );
        channel.set_source_driver(crate::source_driver::SourceDriverId::GrokSubscription);
        channel.subscription.credential_ref = credential_path.to_string_lossy().to_string();
        channel.models = vec!["grok-account-model".to_string()];
        channel
    }

    #[tokio::test]
    async fn periodic_snapshot_reuses_only_the_runtime_account_cache() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing_credential = dir.path().join("missing.json");
        let channel = grok_channel_with_saved_models(&missing_credential);
        let key = grok_subscription_catalog_cache_key(&channel).expect("cache key");
        let slot = grok_subscription_catalog_cache_slot(&key).expect("cache slot");
        let now = Instant::now();
        *slot.lock().await = Some(GrokSubscriptionCatalogCacheEntry {
            models: model_observations_from_tool_catalog(
                &["grok-account-model".to_string()],
                "grok_live_catalog",
            ),
            model_refresh_after: now + GROK_MODEL_CATALOG_REFRESH_INTERVAL,
        });

        let (models, refreshed, warnings) = fetch_grok_subscription_catalog_with_policy(
            &Client::new(),
            &channel,
            SubscriptionCatalogPolicy::Cached,
        )
        .await
        .expect("cached snapshot");

        assert_eq!(
            crate::model_catalog::model_ids(models),
            vec!["grok-account-model"]
        );
        assert!(!refreshed);
        assert!(warnings.is_empty());
        assert!(!missing_credential.exists());
    }

    #[tokio::test]
    async fn periodic_snapshot_does_not_treat_saved_models_as_account_entitlement() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing_credential = dir.path().join("missing.json");
        let channel = grok_channel_with_saved_models(&missing_credential);

        let error = fetch_grok_subscription_catalog_with_policy(
            &Client::new(),
            &channel,
            SubscriptionCatalogPolicy::Cached,
        )
        .await
        .expect_err("saved models are not account entitlement evidence");

        assert!(
            error
                .to_string()
                .contains("prepare Grok subscription credential")
        );
    }

    #[tokio::test]
    async fn explicit_refresh_bypasses_the_runtime_account_cache() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing_credential = dir.path().join("missing.json");
        let channel = grok_channel_with_saved_models(&missing_credential);
        let key = grok_subscription_catalog_cache_key(&channel).expect("cache key");
        let slot = grok_subscription_catalog_cache_slot(&key).expect("cache slot");
        let now = Instant::now();
        *slot.lock().await = Some(GrokSubscriptionCatalogCacheEntry {
            models: model_observations_from_tool_catalog(
                &["grok-account-model".to_string()],
                "grok_live_catalog",
            ),
            model_refresh_after: now + GROK_MODEL_CATALOG_REFRESH_INTERVAL,
        });

        let error = fetch_grok_subscription_catalog_with_policy(
            &Client::new(),
            &channel,
            SubscriptionCatalogPolicy::Fresh,
        )
        .await
        .expect_err("fresh catalog requires the account endpoint");

        assert!(
            error
                .to_string()
                .contains("prepare Grok subscription credential")
        );
    }
}

pub(crate) async fn fetch_grok_subscription_catalog(
    client: &Client,
    token: &str,
) -> Result<Vec<crate::model_catalog::ModelObservation>> {
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", token.trim()))?,
    );
    headers.insert("accept", HeaderValue::from_static("application/json"));
    headers.insert("x-xai-token-auth", HeaderValue::from_static("xai-grok-cli"));
    headers.insert(
        "x-grok-client-version",
        HeaderValue::from_static(GROK_CLI_VERSION),
    );
    headers.insert(
        "x-grok-client-mode",
        HeaderValue::from_static("interactive"),
    );
    headers.insert("user-agent", HeaderValue::from_static(GROK_CLI_USER_AGENT));
    let models = fetch_model_catalog(
        client,
        Url::parse(GROK_MODELS_URL)?,
        headers,
        ModelCatalogKind::Grok,
    )
    .await?;
    Ok(models
        .into_iter()
        .filter(|model| !model.id.to_ascii_lowercase().starts_with("grok-imagine"))
        .collect())
}

pub(crate) async fn fetch_grok_subscription_quota_windows(
    client: &Client,
    token: &str,
) -> Result<Vec<QuotaWindow>> {
    let weekly = fetch_grok_billing_payload(client, token, GROK_BILLING_WEEKLY_URL).await;
    let monthly = fetch_grok_billing_payload(client, token, GROK_BILLING_MONTHLY_URL).await;
    let (weekly, monthly) = match (weekly, monthly) {
        (Err(weekly), Err(monthly)) => return Err(combined_quota_error(weekly, monthly)),
        results => results,
    };
    let checked_at_unix = now_unix();
    let mut windows = Vec::new();
    if let Ok(value) = weekly {
        append_grok_billing_window(&mut windows, &value, "weekly", checked_at_unix, true);
    }
    if let Ok(value) = monthly {
        append_grok_billing_window(&mut windows, &value, "monthly", checked_at_unix, false);
    }
    Ok(windows)
}

async fn fetch_grok_billing_payload(
    client: &Client,
    token: &str,
    url: &str,
) -> Result<serde_json::Value> {
    let response = client
        .get(url)
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", token.trim()))
        .header("X-XAI-Token-Auth", "xai-grok-cli")
        .header("x-grok-client-version", GROK_CLI_VERSION)
        .header("User-Agent", GROK_BILLING_USER_AGENT)
        .send_adaptive()
        .await?;
    let value = quota_response_json(response, "Grok billing").await?;
    Ok(value)
}

fn append_grok_billing_window(
    out: &mut Vec<QuotaWindow>,
    value: &serde_json::Value,
    window: &str,
    checked_at_unix: i64,
    weekly: bool,
) {
    let config = value.get("config").unwrap_or(value);
    let used_percent = if weekly {
        numeric_json_value(config.get("creditUsagePercent"))
    } else {
        let used = grok_billing_cent_value(config.get("used"));
        let limit = grok_billing_cent_value(config.get("monthlyLimit"));
        used.zip(limit)
            .filter(|(used, limit)| used.is_finite() && limit.is_finite())
            .and_then(|(used, limit)| (limit > 0.0).then_some(used / limit * 100.0))
    };
    let Some(used_percent) = used_percent
        .filter(|value| value.is_finite())
        .map(|value| value.max(0.0))
    else {
        return;
    };
    let reset_at_unix = [
        "/config/currentPeriod/end",
        "/config/billingPeriodEnd",
        "/currentPeriod/end",
        "/billingPeriodEnd",
    ]
    .into_iter()
    .find_map(|path| {
        config
            .pointer(path.trim_start_matches("/config"))
            .or_else(|| value.pointer(path))
            .and_then(serde_json::Value::as_str)
            .and_then(parse_token_expiry_text)
    })
    .unwrap_or_default();
    out.push(QuotaWindow {
        source: "grok_billing".to_string(),
        window: window.to_string(),
        remaining_ratio: (1.0 - used_percent / 100.0).clamp(0.0, 1.0),
        used_percent,
        reset_at_unix,
        checked_at_unix,
        model: None,
        token_type: None,
    });
}

fn grok_billing_cent_value(value: Option<&serde_json::Value>) -> Option<f64> {
    let value = value?;
    if let Some(object) = value.as_object() {
        return numeric_json_value(object.get("val"));
    }
    numeric_json_value(Some(value))
}

pub(crate) async fn fetch_openai_subscription_quota_windows(
    client: &Client,
    token: &str,
    credential: &serde_json::Value,
) -> Result<Vec<QuotaWindow>> {
    let identity = active_codex_identity();
    let mut req = apply_codex_control_identity_headers(
        client
            .get(CODEX_USAGE_URL)
            .header("Accept", "application/json")
            .header("Authorization", format!("Bearer {token}")),
        &identity,
    );
    if let Some(account_id) = json_string(credential, &["account_id"])
        .or_else(|| json_string(credential, &["accountID"]))
        .filter(|value| !value.trim().is_empty())
    {
        req = req.header("Chatgpt-Account-Id", account_id);
    }
    let resp = req.send_adaptive().await?;
    let value = quota_response_json(resp, "Codex usage").await?;
    Ok(openai_quota_windows_from_usage(&value))
}

pub(crate) fn openai_quota_windows_from_usage(value: &serde_json::Value) -> Vec<QuotaWindow> {
    let checked_at_unix = now_unix();
    let mut windows = Vec::new();
    if let Some(rate_limit) = value.get("rate_limit") {
        append_openai_rate_limit_windows(&mut windows, rate_limit, "codex", checked_at_unix);
    }
    if let Some(items) = value
        .get("additional_rate_limits")
        .and_then(|value| value.as_array())
    {
        for item in items {
            let label = json_string(item, &["metered_feature"])
                .or_else(|| json_string(item, &["limit_name"]))
                .unwrap_or_else(|| "additional".to_string());
            if let Some(rate_limit) = item.get("rate_limit") {
                append_openai_rate_limit_windows(&mut windows, rate_limit, &label, checked_at_unix);
            }
        }
    }
    windows
}

fn openai_quota_windows_from_event(event: &serde_json::Value) -> Vec<QuotaWindow> {
    let Some(limits) = event.get("rate_limits") else {
        return Vec::new();
    };
    let label = event
        .get("metered_limit_name")
        .or_else(|| event.get("limit_name"))
        .and_then(serde_json::Value::as_str)
        .filter(|label| !label.is_empty())
        .unwrap_or("codex");
    let mut normalized = serde_json::Map::new();
    for (name, target) in [
        ("primary", "primary_window"),
        ("secondary", "secondary_window"),
    ] {
        if let Some(window) = limits.get(name).and_then(serde_json::Value::as_object) {
            let mut window = window.clone();
            if let Some(minutes) = window
                .get("window_minutes")
                .and_then(serde_json::Value::as_i64)
            {
                window.insert(
                    "limit_window_seconds".into(),
                    serde_json::json!(minutes.saturating_mul(60)),
                );
            }
            normalized.insert(target.into(), serde_json::Value::Object(window));
        }
    }
    let mut windows = Vec::new();
    append_openai_rate_limit_windows(
        &mut windows,
        &serde_json::Value::Object(normalized),
        label,
        now_unix(),
    );
    for window in &mut windows {
        window.source = "codex_rate_limits".into();
    }
    windows
}

fn append_openai_rate_limit_windows(
    out: &mut Vec<QuotaWindow>,
    rate_limit: &serde_json::Value,
    label: &str,
    checked_at_unix: i64,
) {
    for key in ["primary_window", "secondary_window"] {
        let Some(window) = rate_limit.get(key).filter(|value| !value.is_null()) else {
            continue;
        };
        let Some(used_percent) =
            numeric_json_value(window.get("used_percent")).filter(|value| value.is_finite())
        else {
            continue;
        };
        let window_seconds = numeric_json_value(window.get("limit_window_seconds"))
            .filter(|value| value.is_finite())
            .unwrap_or_default() as i64;
        let window_name = match window_seconds {
            18_000 => "5h".to_string(),
            604_800 => "weekly".to_string(),
            seconds if seconds > 0 => format!("{seconds}s"),
            _ => key.trim_end_matches("_window").to_string(),
        };
        let reset_at_unix = numeric_json_value(window.get("reset_at"))
            .filter(|value| value.is_finite() && *value > 0.0)
            .map(|value| value as i64)
            .or_else(|| {
                numeric_json_value(window.get("reset_after_seconds"))
                    .filter(|value| value.is_finite() && *value >= 0.0)
                    .map(|seconds| checked_at_unix.saturating_add(seconds as i64))
            })
            .unwrap_or_default();
        let used_percent = used_percent.max(0.0);
        out.push(QuotaWindow {
            source: "openai_wham_usage".to_string(),
            window: if label == "codex" {
                window_name
            } else {
                format!("{label}:{window_name}")
            },
            remaining_ratio: (1.0 - used_percent / 100.0).clamp(0.0, 1.0),
            used_percent,
            reset_at_unix,
            checked_at_unix,
            model: None,
            token_type: Some(label.to_string()),
        });
    }
}

pub(crate) async fn ensure_openai_subscription_access_token(
    client: &Client,
    channel: &ChannelConfig,
) -> Result<(String, serde_json::Value, bool)> {
    ensure_openai_subscription_access_token_with_options(
        client,
        channel,
        None,
        OPENAI_OAUTH_TOKEN_URL,
    )
    .await
}

pub(crate) async fn ensure_openai_subscription_access_token_with_options(
    client: &Client,
    channel: &ChannelConfig,
    rejected_access_token: Option<&str>,
    token_url: &str,
) -> Result<(String, serde_json::Value, bool)> {
    let force_refresh = rejected_access_token.is_some();
    let path = subscription_credential_path(channel)?;
    let mut credential = if force_refresh {
        read_subscription_credential(&path)?
    } else {
        read_openai_subscription_credential_cached(&path)?
    };
    ensure_subscription_token_provider(&mut credential, "openai");

    let original_access_token =
        json_string(&credential, &["access_token"]).filter(|token| !token.trim().is_empty());
    if let Some(access_token) = original_access_token.clone() {
        if !force_refresh && !credential_needs_refresh(&credential) {
            return Ok((access_token, credential, false));
        }
    }

    let refresh_lock = subscription_refresh_lock(&path)?;
    let _refresh_guard = refresh_lock.lock().await;
    let mut latest = read_subscription_credential(&path)?;
    ensure_subscription_token_provider(&mut latest, "openai");
    remember_openai_subscription_credential(&path, &latest);
    if let Some(access_token) =
        json_string(&latest, &["access_token"]).filter(|token| !token.trim().is_empty())
    {
        // A delayed 401 may arrive after another request already rotated the token.
        // Compare with the token actually rejected, not a new disk snapshot.
        let refreshed_by_peer = rejected_access_token.or(original_access_token.as_deref())
            != Some(access_token.as_str());
        if !credential_needs_refresh(&latest) && (!force_refresh || refreshed_by_peer) {
            return Ok((access_token, latest, refreshed_by_peer));
        }
    }
    credential = latest;

    let refresh_token = json_string(&credential, &["refresh_token"])
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| anyhow!("OpenAI subscription credential is missing refresh_token"))?;
    let refresh_request = serde_json::json!({
        "client_id": OPENAI_OAUTH_CLIENT_ID,
        "grant_type": "refresh_token",
        "refresh_token": refresh_token.clone(),
    });
    let identity = active_codex_identity();
    let resp = apply_codex_control_identity_headers(
        client
            .post(token_url)
            .timeout(SUBSCRIPTION_OAUTH_HTTP_TIMEOUT)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&refresh_request),
        &identity,
    )
    .send_adaptive()
    .await?;
    let status = resp.status();
    let refreshed = resp.json::<serde_json::Value>().await;
    if !status.is_success() {
        // An empty or HTML error body must not hide the upstream HTTP status.
        // Successful responses still require valid JSON before saving credentials.
        let error = SubscriptionOAuthError::from_response(
            "openai",
            status.as_u16(),
            refreshed.as_ref().unwrap_or(&serde_json::Value::Null),
        );
        log::warn!(
            "[const-api][subscription-auth] provider=openai stage=refresh status={} code={}",
            status.as_u16(),
            error.code(),
        );
        return Err(error.into());
    }
    let mut refreshed = refreshed?;
    if json_string(&refreshed, &["refresh_token"])
        .map(|token| token.trim().is_empty())
        .unwrap_or(true)
    {
        if let Some(object) = refreshed.as_object_mut() {
            object.insert(
                "refresh_token".to_string(),
                serde_json::Value::String(refresh_token),
            );
        }
    }
    copy_token_metadata(&credential, &mut refreshed);
    ensure_subscription_token_provider(&mut refreshed, "openai");
    normalize_token_expiry(&mut refreshed);
    let access_token = json_string(&refreshed, &["access_token"])
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| anyhow!("OpenAI OAuth refresh response did not include access_token"))?;
    write_subscription_credential_atomically(&path, &refreshed)?;
    Ok((access_token, refreshed, true))
}

async fn check_antigravity_subscription_with_policy(
    client: &Client,
    channel: &ChannelConfig,
    policy: SubscriptionCatalogPolicy,
) -> Result<AntigravitySubscriptionHealth> {
    check_antigravity_subscription_with_endpoints_and_policy(
        client,
        channel,
        ANTIGRAVITY_API_BASE_URL,
        ANTIGRAVITY_DAILY_API_BASE_URL,
        ANTIGRAVITY_OAUTH_TOKEN_URL,
        policy,
    )
    .await
}

#[cfg(test)]
pub(crate) async fn check_antigravity_subscription_with_urls(
    client: &Client,
    channel: &ChannelConfig,
    api_base_url: &str,
    token_url: &str,
) -> Result<AntigravitySubscriptionHealth> {
    check_antigravity_subscription_with_endpoints(
        client,
        channel,
        api_base_url,
        api_base_url,
        token_url,
    )
    .await
}

#[cfg(test)]
pub(crate) async fn check_antigravity_subscription_with_endpoints(
    client: &Client,
    channel: &ChannelConfig,
    api_base_url: &str,
    daily_api_base_url: &str,
    token_url: &str,
) -> Result<AntigravitySubscriptionHealth> {
    check_antigravity_subscription_with_endpoints_and_policy(
        client,
        channel,
        api_base_url,
        daily_api_base_url,
        token_url,
        SubscriptionCatalogPolicy::Fresh,
    )
    .await
}

async fn check_antigravity_subscription_with_endpoints_and_policy(
    client: &Client,
    channel: &ChannelConfig,
    api_base_url: &str,
    daily_api_base_url: &str,
    token_url: &str,
    policy: SubscriptionCatalogPolicy,
) -> Result<AntigravitySubscriptionHealth> {
    let (access_token, credential, refreshed) =
        ensure_antigravity_subscription_access_token_with_options(
            client, channel, false, token_url,
        )
        .await
        .context("prepare Antigravity subscription credential")?;
    let oauth_type = json_string(&credential, &["oauth_type"])
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "antigravity".to_string());
    if oauth_type != "antigravity" {
        return Err(anyhow!(
            "Legacy Gemini Code Assist credentials cannot be used with Antigravity; remove the old channel and authorize Antigravity again."
        ));
    }

    let load_body = serde_json::json!({
        "metadata": {
            "ideType": "ANTIGRAVITY"
        }
    });
    let (load_status, load_value, load_refreshed, _) = send_antigravity_operation_json(
        client,
        channel,
        &[api_base_url, daily_api_base_url],
        "loadCodeAssist",
        access_token.clone(),
        &load_body,
        token_url,
        false,
    )
    .await?;
    if !load_status.is_success() {
        return Err(anyhow!(
            "Antigravity loadCodeAssist returned {load_status}: {load_value}"
        ));
    }

    let mut refreshed_any = refreshed || load_refreshed;
    let mut active_access_token = if load_refreshed {
        ensure_antigravity_subscription_access_token_with_options(client, channel, false, token_url)
            .await?
            .0
    } else {
        access_token
    };
    let stored_project = json_string(&credential, &["project_id"]).unwrap_or_default();
    let stored_tier = json_string(&credential, &["tier_id"]).unwrap_or_default();
    let detected_tier =
        extract_antigravity_tier_id(&load_value).unwrap_or_else(|| stored_tier.clone());
    let mut detected_project =
        extract_antigravity_project_id(&load_value).unwrap_or(stored_project);

    if detected_project.trim().is_empty() {
        let tier_id = antigravity_default_tier_id(&load_value, &detected_tier);
        let (project, onboard_refreshed) = onboard_antigravity_user(
            client,
            channel,
            daily_api_base_url,
            token_url,
            active_access_token.clone(),
            &tier_id,
        )
        .await?;
        detected_project = project;
        refreshed_any |= onboard_refreshed;
        if onboard_refreshed {
            active_access_token = ensure_antigravity_subscription_access_token_with_options(
                client, channel, false, token_url,
            )
            .await?
            .0;
        }
    }
    if detected_project.trim().is_empty() {
        return Err(anyhow!(
            "Antigravity did not provide a cloudaicompanionProject after onboarding"
        ));
    }

    persist_antigravity_subscription_metadata(channel, &detected_project, &detected_tier)
        .await
        .context("persist detected Antigravity subscription metadata")?;

    // Antigravity resolves the account project from the OAuth token for this RPC.
    // Sending the project here is not part of the current request schema and can
    // make stricter deployments reject the otherwise valid catalog request.
    let catalog_body = serde_json::json!({});
    let (catalog_value, models, catalog_refreshed, _) = fetch_antigravity_model_catalog(
        client,
        channel,
        &[daily_api_base_url, api_base_url],
        active_access_token.clone(),
        &catalog_body,
        token_url,
    )
    .await?;
    refreshed_any |= catalog_refreshed;
    let model_routes = crate::antigravity_models::model_routes_from_catalog(&catalog_value);
    persist_antigravity_subscription_model_routes(channel, &model_routes)
        .await
        .context("persist detected Antigravity subscription model routes")?;
    if catalog_refreshed {
        active_access_token = ensure_antigravity_subscription_access_token_with_options(
            client, channel, false, token_url,
        )
        .await?
        .0;
    }

    let warnings = Vec::new();
    let catalog_quota = antigravity_quota_windows_from_model_catalog(&catalog_value);
    let quota_body = serde_json::json!({"project": detected_project});
    let live_quota = refresh_subscription_quota(channel, policy, async {
        let (status, value, quota_refreshed, quota_url) = send_antigravity_operation_json(
            client,
            channel,
            &[api_base_url, daily_api_base_url],
            "retrieveUserQuota",
            active_access_token,
            &quota_body,
            token_url,
            false,
        )
        .await?;
        refreshed_any |= quota_refreshed;
        if !status.is_success() {
            return Err(anyhow!(
                "Antigravity retrieveUserQuota returned {status} from {quota_url}: {value}"
            ));
        }
        Ok(antigravity_quota_windows_from_retrieve_user_quota(&value))
    })
    .await?;
    let quota_windows = merge_antigravity_quota_windows(catalog_quota, live_quota, &models);

    let load_quota = antigravity_quota_from_load_code_assist(&load_value)
        .unwrap_or((0.0, "unknown".to_string()));
    let (remaining_ratio, quota_status) =
        quota_snapshot_from_windows(&quota_windows).unwrap_or(load_quota);
    let summary = format!(
        "Antigravity reachable, project_id={}, models={}",
        detected_project,
        models.len()
    );
    Ok(AntigravitySubscriptionHealth {
        model_observations: antigravity_model_observations(&catalog_value),
        oauth_type,
        project_id: detected_project,
        tier_id: detected_tier,
        summary,
        models,
        remaining_ratio,
        quota_status,
        quota_windows,
        warnings,
        refreshed: refreshed_any,
    })
}

pub(crate) fn antigravity_default_tier_id(
    value: &serde_json::Value,
    detected_tier: &str,
) -> String {
    value
        .get("allowedTiers")
        .and_then(serde_json::Value::as_array)
        .and_then(|tiers| {
            tiers
                .iter()
                .find(|tier| {
                    tier.get("isDefault").and_then(serde_json::Value::as_bool) == Some(true)
                })
                .and_then(|tier| json_string(tier, &["id"]))
        })
        .or_else(|| (!detected_tier.trim().is_empty()).then(|| detected_tier.trim().to_string()))
        .unwrap_or_else(|| "free-tier".to_string())
}

pub(crate) fn antigravity_models_from_catalog(value: &serde_json::Value) -> Vec<String> {
    crate::antigravity_models::public_models_from_catalog(value)
}

pub(crate) fn antigravity_quota_windows_from_model_catalog(
    value: &serde_json::Value,
) -> Vec<QuotaWindow> {
    let checked_at_unix = now_unix();
    let windows = value
        .get("models")
        .and_then(serde_json::Value::as_object)
        .map(|models| {
            models
                .iter()
                .filter_map(|(model, metadata)| {
                    if metadata
                        .get("isInternal")
                        .and_then(serde_json::Value::as_bool)
                        == Some(true)
                    {
                        return None;
                    }
                    let quota = metadata.get("quotaInfo")?;
                    let remaining_ratio =
                        numeric_json_value(quota.get("remainingFraction"))?.clamp(0.0, 1.0);
                    Some(QuotaWindow {
                        source: "antigravity_fetch_available_models".to_string(),
                        window: model.clone(),
                        remaining_ratio,
                        used_percent: ((1.0 - remaining_ratio) * 100.0).clamp(0.0, 100.0),
                        reset_at_unix: quota
                            .get("resetTime")
                            .and_then(serde_json::Value::as_str)
                            .and_then(parse_gemini_reset_time_unix)
                            .unwrap_or_default(),
                        checked_at_unix,
                        model: Some(model.clone()),
                        token_type: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    crate::antigravity_models::canonicalize_quota_windows(windows)
}

pub(crate) fn merge_antigravity_quota_windows(
    catalog: Vec<QuotaWindow>,
    live: Vec<QuotaWindow>,
    callable_models: &[String],
) -> Vec<QuotaWindow> {
    let callable_models = callable_models
        .iter()
        .map(|model| model.trim().to_ascii_lowercase())
        .filter(|model| !model.is_empty())
        .collect::<std::collections::HashSet<_>>();
    let filter_callable = |window: &QuotaWindow| {
        window
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(|model| callable_models.contains(&model.to_ascii_lowercase()))
            .unwrap_or(false)
    };
    let catalog = catalog
        .into_iter()
        .filter(&filter_callable)
        .collect::<Vec<_>>();
    let live = live
        .into_iter()
        .filter(&filter_callable)
        .collect::<Vec<_>>();
    if live.is_empty() {
        return catalog;
    }
    // Catalog quota is a fresh observation even when the dedicated usage RPC
    // is cached. A cached RPC result must not overwrite newer catalog values.
    let live = live
        .into_iter()
        .filter(|window| {
            !catalog.iter().any(|entry| {
                entry.model == window.model && entry.checked_at_unix > window.checked_at_unix
            })
        })
        .collect::<Vec<_>>();
    let live_models = live
        .iter()
        .filter_map(|window| window.model.as_deref())
        .map(|model| model.to_ascii_lowercase())
        .collect::<std::collections::HashSet<_>>();
    let merged = catalog
        .into_iter()
        .filter(|window| {
            window
                .model
                .as_deref()
                .map(|model| !live_models.contains(&model.to_ascii_lowercase()))
                .unwrap_or(true)
        })
        .chain(live)
        .collect();
    crate::antigravity_models::canonicalize_quota_windows(merged)
}

fn antigravity_should_fallback(status: reqwest::StatusCode) -> bool {
    matches!(
        status,
        reqwest::StatusCode::NOT_FOUND
            | reqwest::StatusCode::REQUEST_TIMEOUT
            | reqwest::StatusCode::TOO_MANY_REQUESTS
    ) || status.is_server_error()
}

async fn send_antigravity_json_once(
    client: &Client,
    url: &str,
    access_token: &str,
    body: &serde_json::Value,
    control_plane: bool,
) -> Result<(reqwest::StatusCode, serde_json::Value)> {
    let user_agent = if control_plane {
        antigravity_control_plane_user_agent()
    } else {
        antigravity_user_agent()
    };
    let mut request = client
        .post(url)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Accept", "*/*")
        .header("Content-Type", "application/json")
        .header("User-Agent", user_agent)
        .json(body);
    if control_plane {
        request = request.header("X-Goog-Api-Client", ANTIGRAVITY_GOOG_API_CLIENT);
    }
    let response = request.send_adaptive().await?;
    let status = response.status();
    if url.ends_with(":retrieveUserQuota") && (status.as_u16() == 429 || status.as_u16() == 503) {
        // Keep this RPC's Retry-After without changing OAuth refresh or the
        // status-based handling of other Antigravity operations.
        return Ok((
            status,
            quota_response_json(response, "Antigravity retrieveUserQuota").await?,
        ));
    }
    let bytes = response.bytes().await?;
    let value = serde_json::from_slice::<serde_json::Value>(&bytes).unwrap_or_else(|_| {
        serde_json::json!({
            "raw": String::from_utf8_lossy(&bytes).trim()
        })
    });
    Ok((status, value))
}

pub(crate) async fn send_antigravity_operation_json(
    client: &Client,
    channel: &ChannelConfig,
    base_urls: &[&str],
    operation: &str,
    access_token: String,
    body: &serde_json::Value,
    token_url: &str,
    control_plane: bool,
) -> Result<(reqwest::StatusCode, serde_json::Value, bool, String)> {
    let mut active_token = access_token;
    let mut refreshed = false;
    let mut seen = std::collections::HashSet::new();
    let mut last_error = None;
    for base_url in base_urls {
        let base_url = base_url.trim().trim_end_matches('/');
        if base_url.is_empty() || !seen.insert(base_url.to_string()) {
            continue;
        }
        let url = format!("{base_url}/v1internal:{operation}");
        let attempt =
            send_antigravity_json_once(client, &url, &active_token, body, control_plane).await;
        let (mut status, mut value) = match attempt {
            Ok(response) => response,
            Err(error) if crate::upstream_transport::is_ambiguous_transport_error(&error) => {
                return Err(error);
            }
            Err(error) => {
                last_error =
                    Some(error.context(format!("Antigravity {operation} request to {url} failed")));
                continue;
            }
        };
        if status == reqwest::StatusCode::UNAUTHORIZED {
            let (fresh_token, _, _) = ensure_antigravity_subscription_access_token_with_options(
                client, channel, true, token_url,
            )
            .await?;
            active_token = fresh_token;
            refreshed = true;
            (status, value) =
                send_antigravity_json_once(client, &url, &active_token, body, control_plane)
                    .await?;
        }
        if antigravity_should_fallback(status) {
            last_error = Some(anyhow!(
                "Antigravity {operation} returned {status} from {url}: {value}"
            ));
            continue;
        }
        return Ok((status, value, refreshed, url));
    }
    Err(last_error
        .unwrap_or_else(|| anyhow!("Antigravity {operation} has no configured API endpoint")))
}

async fn fetch_antigravity_model_catalog(
    client: &Client,
    channel: &ChannelConfig,
    base_urls: &[&str],
    mut access_token: String,
    body: &serde_json::Value,
    token_url: &str,
) -> Result<(serde_json::Value, Vec<String>, bool, String)> {
    let mut refreshed_any = false;
    let mut last_error = None;
    let mut seen = std::collections::HashSet::new();
    for base_url in base_urls {
        let base_url = base_url.trim().trim_end_matches('/');
        if base_url.is_empty() || !seen.insert(base_url.to_string()) {
            continue;
        }
        match send_antigravity_operation_json(
            client,
            channel,
            &[base_url],
            "fetchAvailableModels",
            access_token.clone(),
            body,
            token_url,
            false,
        )
        .await
        {
            Ok((status, value, refreshed, url)) => {
                refreshed_any |= refreshed;
                if refreshed {
                    access_token = ensure_antigravity_subscription_access_token_with_options(
                        client, channel, false, token_url,
                    )
                    .await?
                    .0;
                }
                if !status.is_success() {
                    last_error = Some(anyhow!(
                        "Antigravity fetchAvailableModels returned {status} from {url}: {value}"
                    ));
                    continue;
                }
                let models = antigravity_models_from_catalog(&value);
                if models.is_empty() {
                    last_error = Some(anyhow!(
                        "Antigravity fetchAvailableModels returned no callable models from {url}"
                    ));
                    continue;
                }
                return Ok((value, models, refreshed_any, url));
            }
            Err(error) if crate::upstream_transport::is_ambiguous_transport_error(&error) => {
                return Err(error);
            }
            Err(error) => {
                last_error = Some(error);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| {
        anyhow!("Antigravity fetchAvailableModels has no configured API endpoint")
    }))
}

async fn onboard_antigravity_user(
    client: &Client,
    channel: &ChannelConfig,
    daily_api_base_url: &str,
    token_url: &str,
    mut access_token: String,
    tier_id: &str,
) -> Result<(String, bool)> {
    let body = serde_json::json!({
        "tier_id": tier_id,
        "metadata": {
            "ide_type": "ANTIGRAVITY",
            "ide_version": antigravity_user_agent_version(),
            "ide_name": "antigravity"
        }
    });
    let mut refreshed_any = false;
    for attempt in 0..5 {
        let (status, value, refreshed, _) = send_antigravity_operation_json(
            client,
            channel,
            &[daily_api_base_url],
            "onboardUser",
            access_token.clone(),
            &body,
            token_url,
            true,
        )
        .await?;
        refreshed_any |= refreshed;
        if refreshed {
            access_token = ensure_antigravity_subscription_access_token_with_options(
                client, channel, false, token_url,
            )
            .await?
            .0;
        }
        if !status.is_success() {
            return Err(anyhow!(
                "Antigravity onboardUser returned {status}: {value}"
            ));
        }
        if let Some(project) = extract_antigravity_project_id(&value) {
            return Ok((project, refreshed_any));
        }
        if value.get("done").and_then(serde_json::Value::as_bool) == Some(true) {
            return Err(anyhow!(
                "Antigravity onboardUser completed without cloudaicompanionProject"
            ));
        }
        if attempt < 4 {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }
    Err(anyhow!(
        "Antigravity onboardUser did not finish after 5 checks"
    ))
}

pub(crate) async fn persist_antigravity_subscription_metadata(
    channel: &ChannelConfig,
    project_id: &str,
    tier_id: &str,
) -> Result<bool> {
    let project_id = project_id.trim();
    let tier_id = tier_id.trim();
    if project_id.is_empty() && tier_id.is_empty() {
        return Ok(false);
    }
    let path = subscription_credential_path(channel)?;
    let refresh_lock = subscription_refresh_lock(&path)?;
    let _refresh_guard = refresh_lock.lock().await;
    let latest = read_subscription_credential(&path)?;
    let mut credential = normalize_antigravity_credential_value(&latest)
        .ok_or_else(|| anyhow!("unsupported Antigravity subscription credential format"))?;
    let mut changed = false;
    if !project_id.is_empty()
        && json_string(&credential, &["project_id"]).as_deref() != Some(project_id)
    {
        credential["project_id"] = serde_json::Value::String(project_id.to_string());
        changed = true;
    }
    if !tier_id.is_empty() && json_string(&credential, &["tier_id"]).as_deref() != Some(tier_id) {
        credential["tier_id"] = serde_json::Value::String(tier_id.to_string());
        changed = true;
    }
    if changed {
        write_subscription_credential_atomically(&path, &credential)?;
    }
    Ok(changed)
}

pub(crate) async fn persist_antigravity_subscription_model_routes(
    channel: &ChannelConfig,
    routes: &[crate::antigravity_models::AntigravityModelRoute],
) -> Result<bool> {
    if routes.is_empty() {
        return Ok(false);
    }
    let path = subscription_credential_path(channel)?;
    let refresh_lock = subscription_refresh_lock(&path)?;
    let _refresh_guard = refresh_lock.lock().await;
    let latest = read_subscription_credential(&path)?;
    let mut credential = normalize_antigravity_credential_value(&latest)
        .ok_or_else(|| anyhow!("unsupported Antigravity subscription credential format"))?;
    let routes = crate::antigravity_models::model_routes_json(routes)?;
    if credential.get(crate::antigravity_models::ANTIGRAVITY_MODEL_ROUTES_KEY) == Some(&routes) {
        return Ok(false);
    }
    credential[crate::antigravity_models::ANTIGRAVITY_MODEL_ROUTES_KEY] = routes;
    write_subscription_credential_atomically(&path, &credential)?;
    Ok(true)
}

pub(crate) async fn ensure_antigravity_subscription_access_token(
    client: &Client,
    channel: &ChannelConfig,
) -> Result<(String, serde_json::Value, bool)> {
    ensure_antigravity_subscription_access_token_with_options(
        client,
        channel,
        false,
        ANTIGRAVITY_OAUTH_TOKEN_URL,
    )
    .await
}

pub(crate) async fn force_refresh_antigravity_subscription_access_token(
    client: &Client,
    channel: &ChannelConfig,
) -> Result<(String, serde_json::Value, bool)> {
    ensure_antigravity_subscription_access_token_with_options(
        client,
        channel,
        true,
        ANTIGRAVITY_OAUTH_TOKEN_URL,
    )
    .await
}

pub(crate) async fn ensure_antigravity_subscription_access_token_with_options(
    client: &Client,
    channel: &ChannelConfig,
    force_refresh: bool,
    token_url: &str,
) -> Result<(String, serde_json::Value, bool)> {
    let path = subscription_credential_path(channel)?;
    let mut credential = read_subscription_credential(&path)?;
    credential = normalize_antigravity_credential_value(&credential).ok_or_else(|| {
        anyhow!("Legacy Gemini Code Assist credentials cannot be used with Antigravity; authorize Antigravity again.")
    })?;

    let original_access_token =
        json_string(&credential, &["access_token"]).filter(|token| !token.trim().is_empty());
    if let Some(access_token) = original_access_token.clone() {
        if !force_refresh && !credential_needs_refresh(&credential) {
            return Ok((access_token, credential, false));
        }
    }

    let refresh_lock = subscription_refresh_lock(&path)?;
    let _refresh_guard = refresh_lock.lock().await;
    let mut latest = read_subscription_credential(&path)?;
    latest = normalize_antigravity_credential_value(&latest).ok_or_else(|| {
        anyhow!("Legacy Gemini Code Assist credentials cannot be used with Antigravity; authorize Antigravity again.")
    })?;
    if let Some(access_token) =
        json_string(&latest, &["access_token"]).filter(|token| !token.trim().is_empty())
    {
        let refreshed_by_peer = original_access_token.as_deref() != Some(access_token.as_str());
        if !credential_needs_refresh(&latest) && (!force_refresh || refreshed_by_peer) {
            return Ok((access_token, latest, refreshed_by_peer));
        }
    }
    credential = latest;

    let refresh_token = json_string(&credential, &["refresh_token"])
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| anyhow!("Antigravity credential is missing refresh_token"))?;
    let mut form = std::collections::HashMap::new();
    form.insert("grant_type", "refresh_token");
    form.insert("refresh_token", refresh_token.as_str());
    crate::local_policy::require_antigravity_oauth_app()?;
    form.insert("client_id", ANTIGRAVITY_OAUTH_CLIENT_ID);
    form.insert("client_secret", ANTIGRAVITY_OAUTH_CLIENT_SECRET);

    let response = client
        .post(token_url)
        .timeout(SUBSCRIPTION_OAUTH_HTTP_TIMEOUT)
        .form(&form)
        .send_adaptive()
        .await?;
    let status = response.status();
    let mut refreshed = response.json::<serde_json::Value>().await?;
    if !status.is_success() {
        return Err(anyhow!(
            "Antigravity OAuth refresh returned {status}: {refreshed}"
        ));
    }
    if json_string(&refreshed, &["refresh_token"])
        .map(|token| token.trim().is_empty())
        .unwrap_or(true)
    {
        if let Some(object) = refreshed.as_object_mut() {
            object.insert(
                "refresh_token".to_string(),
                serde_json::Value::String(refresh_token),
            );
        }
    }
    copy_token_metadata(&credential, &mut refreshed);
    ensure_subscription_token_provider(&mut refreshed, "antigravity");
    ensure_antigravity_token_defaults(&mut refreshed);
    normalize_token_expiry(&mut refreshed);
    let access_token = json_string(&refreshed, &["access_token"])
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| {
            anyhow!("Antigravity OAuth refresh response did not include access_token")
        })?;
    write_subscription_credential_atomically(&path, &refreshed)?;
    Ok((access_token, refreshed, true))
}

pub(crate) async fn ensure_claude_subscription_access_token(
    client: &Client,
    channel: &ChannelConfig,
) -> Result<(String, serde_json::Value, bool)> {
    ensure_claude_subscription_access_token_with_options(
        client,
        channel,
        false,
        CLAUDE_OAUTH_TOKEN_URL,
    )
    .await
}

pub(crate) fn claude_oauth_refresh_body(refresh_token: &str) -> serde_json::Value {
    serde_json::json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "client_id": CLAUDE_OAUTH_CLIENT_ID,
        "scope": CLAUDE_AI_OAUTH_SCOPE,
    })
}

pub(crate) async fn ensure_claude_subscription_access_token_with_options(
    client: &Client,
    channel: &ChannelConfig,
    force_refresh: bool,
    token_url: &str,
) -> Result<(String, serde_json::Value, bool)> {
    let path = subscription_credential_path(channel)?;
    let mut credential = read_subscription_credential(&path)?;
    ensure_subscription_token_provider(&mut credential, "claude");

    let original_access_token =
        json_string(&credential, &["access_token"]).filter(|token| !token.trim().is_empty());
    if let Some(access_token) = original_access_token.clone() {
        if !force_refresh && !credential_needs_refresh(&credential) {
            return Ok((access_token, credential, false));
        }
    }

    let refresh_lock = subscription_refresh_lock(&path)?;
    let _refresh_guard = refresh_lock.lock().await;
    let mut latest = read_subscription_credential(&path)?;
    ensure_subscription_token_provider(&mut latest, "claude");
    if let Some(access_token) =
        json_string(&latest, &["access_token"]).filter(|token| !token.trim().is_empty())
    {
        let refreshed_by_peer = original_access_token.as_deref() != Some(access_token.as_str());
        if !credential_needs_refresh(&latest) && (!force_refresh || refreshed_by_peer) {
            return Ok((access_token, latest, refreshed_by_peer));
        }
    }
    credential = latest;

    let refresh_token = json_string(&credential, &["refresh_token"])
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| anyhow!("Claude subscription credential is missing refresh_token"))?;
    let refresh_key = subscription_refresh_token_key("claude", &refresh_token);
    let token_refresh_lock = subscription_refresh_token_lock(&refresh_key);
    let _token_refresh_guard = token_refresh_lock.lock().await;

    // Two locally imported channels may refer to distinct credential files but
    // contain the same rotating refresh token. Reuse the first successful
    // rotation instead of sending the now-invalid old token a second time.
    if let Some(mut refreshed) = cached_claude_refresh_result(&refresh_key) {
        copy_token_metadata(&credential, &mut refreshed);
        ensure_subscription_token_provider(&mut refreshed, "claude");
        normalize_token_expiry(&mut refreshed);
        let access_token = json_string(&refreshed, &["access_token"])
            .filter(|token| !token.trim().is_empty())
            .ok_or_else(|| anyhow!("cached Claude OAuth refresh did not include access_token"))?;
        write_subscription_credential_atomically(&path, &refreshed)?;
        return Ok((access_token, refreshed, true));
    }
    let body = claude_oauth_refresh_body(&refresh_token);
    let resp = client
        .post(token_url)
        .timeout(SUBSCRIPTION_OAUTH_HTTP_TIMEOUT)
        .header("Accept", "application/json, text/plain, */*")
        .header("Content-Type", "application/json")
        .header("User-Agent", CLAUDE_OAUTH_AXIOS_USER_AGENT)
        .json(&body)
        .send_adaptive()
        .await?;
    let status = resp.status();
    let mut refreshed = resp.json::<serde_json::Value>().await?;
    if !status.is_success() {
        return Err(
            SubscriptionOAuthError::from_response("claude", status.as_u16(), &refreshed).into(),
        );
    }
    if json_string(&refreshed, &["refresh_token"])
        .map(|token| token.trim().is_empty())
        .unwrap_or(true)
    {
        if let Some(object) = refreshed.as_object_mut() {
            object.insert(
                "refresh_token".to_string(),
                serde_json::Value::String(refresh_token),
            );
        }
    }
    ensure_subscription_token_provider(&mut refreshed, "claude");
    normalize_token_expiry(&mut refreshed);
    let access_token = json_string(&refreshed, &["access_token"])
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| anyhow!("Claude OAuth refresh response did not include access_token"))?;
    let token_material = refreshed.clone();
    copy_token_metadata(&credential, &mut refreshed);
    write_subscription_credential_atomically(&path, &refreshed)?;
    remember_claude_refresh_result(refresh_key, token_material);
    Ok((access_token, refreshed, true))
}

pub(crate) async fn ensure_grok_subscription_access_token(
    client: &Client,
    channel: &ChannelConfig,
) -> Result<(String, serde_json::Value, bool)> {
    ensure_grok_subscription_access_token_with_options(client, channel, false, GROK_OAUTH_TOKEN_URL)
        .await
}

pub(crate) async fn ensure_grok_subscription_access_token_with_options(
    client: &Client,
    channel: &ChannelConfig,
    force_refresh: bool,
    token_url: &str,
) -> Result<(String, serde_json::Value, bool)> {
    let path = subscription_credential_path(channel)?;
    let mut credential = read_subscription_credential(&path)?;
    ensure_subscription_token_provider(&mut credential, "grok");

    let original_access_token =
        json_string(&credential, &["access_token"]).filter(|token| !token.trim().is_empty());
    if let Some(access_token) = original_access_token.clone() {
        if !force_refresh && !credential_needs_refresh(&credential) {
            return Ok((access_token, credential, false));
        }
    }

    let refresh_lock = subscription_refresh_lock(&path)?;
    let _refresh_guard = refresh_lock.lock().await;
    let mut latest = read_subscription_credential(&path)?;
    ensure_subscription_token_provider(&mut latest, "grok");
    if let Some(access_token) =
        json_string(&latest, &["access_token"]).filter(|token| !token.trim().is_empty())
    {
        let refreshed_by_peer = original_access_token.as_deref() != Some(access_token.as_str());
        if !credential_needs_refresh(&latest) && (!force_refresh || refreshed_by_peer) {
            return Ok((access_token, latest, refreshed_by_peer));
        }
    }
    credential = latest;

    let refresh_token = json_string(&credential, &["refresh_token"])
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| anyhow!("Grok subscription credential is missing refresh_token"))?;
    let client_id = json_string(&credential, &["client_id"])
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| GROK_OAUTH_CLIENT_ID.to_string());
    let mut form = std::collections::HashMap::new();
    form.insert("grant_type", "refresh_token");
    form.insert("client_id", client_id.as_str());
    form.insert("refresh_token", refresh_token.as_str());

    let response = client
        .post(token_url)
        .timeout(SUBSCRIPTION_OAUTH_HTTP_TIMEOUT)
        .header("User-Agent", "const-api-grok-oauth/1.0")
        .form(&form)
        .send_adaptive()
        .await?;
    let status = response.status();
    let mut refreshed = response.json::<serde_json::Value>().await?;
    if !status.is_success() {
        return Err(anyhow!("Grok OAuth refresh returned {status}: {refreshed}"));
    }
    if json_string(&refreshed, &["refresh_token"])
        .map(|token| token.trim().is_empty())
        .unwrap_or(true)
    {
        if let Some(object) = refreshed.as_object_mut() {
            object.insert(
                "refresh_token".to_string(),
                serde_json::Value::String(refresh_token),
            );
        }
    }
    copy_token_metadata(&credential, &mut refreshed);
    ensure_subscription_token_provider(&mut refreshed, "grok");
    if let Some(object) = refreshed.as_object_mut() {
        object.insert(
            "client_id".to_string(),
            serde_json::Value::String(client_id),
        );
    }
    normalize_token_expiry(&mut refreshed);
    let access_token = json_string(&refreshed, &["access_token"])
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| anyhow!("Grok OAuth refresh response did not include access_token"))?;
    write_subscription_credential_atomically(&path, &refreshed)?;
    Ok((access_token, refreshed, true))
}

pub(crate) fn extract_antigravity_project_id(value: &serde_json::Value) -> Option<String> {
    [
        value.get("cloudaicompanionProject"),
        value.pointer("/response/cloudaicompanionProject"),
    ]
    .into_iter()
    .flatten()
    .find_map(|project| {
        project
            .as_str()
            .map(ToString::to_string)
            .or_else(|| json_string(project, &["id"]))
            .or_else(|| json_string(project, &["projectId"]))
    })
}

pub(crate) fn extract_antigravity_tier_id(value: &serde_json::Value) -> Option<String> {
    value
        .get("paidTier")
        .and_then(|tier| json_string(tier, &["id"]))
        .or_else(|| {
            value
                .get("currentTier")
                .and_then(|tier| json_string(tier, &["id"]))
        })
        .or_else(|| {
            value
                .get("allowedTiers")
                .and_then(|tiers| tiers.as_array())
                .and_then(|tiers| {
                    tiers
                        .iter()
                        .find(|tier| tier.get("isDefault").and_then(|v| v.as_bool()) == Some(true))
                        .and_then(|tier| json_string(tier, &["id"]))
                        .or_else(|| tiers.iter().find_map(|tier| json_string(tier, &["id"])))
                })
        })
}

pub(crate) fn antigravity_quota_from_load_code_assist(
    value: &serde_json::Value,
) -> Option<(f64, String)> {
    let credits = value
        .pointer("/paidTier/availableCredits")
        .or_else(|| value.pointer("/currentTier/availableCredits"))
        .or_else(|| value.get("availableCredits"))?;
    let credits = credits.as_array()?;
    let mut best_ratio: Option<f64> = None;
    for credit in credits {
        let Some(amount) = numeric_json_value(
            credit
                .get("creditAmount")
                .or_else(|| credit.get("remaining"))
                .or_else(|| credit.get("available")),
        ) else {
            continue;
        };
        let min = numeric_json_value(
            credit
                .get("minimumCreditAmountForUsage")
                .or_else(|| credit.get("minimum"))
                .or_else(|| credit.get("limit")),
        )
        .unwrap_or(0.0);
        let ratio = if min > 0.0 {
            (amount / min).clamp(0.0, 1.0)
        } else if amount > 0.0 {
            1.0
        } else {
            0.0
        };
        best_ratio = Some(
            best_ratio
                .map(|current| current.min(ratio))
                .unwrap_or(ratio),
        );
    }
    let ratio = best_ratio?;
    let status = if ratio <= 0.0 {
        "exhausted"
    } else if ratio <= 0.2 {
        "low"
    } else {
        "available"
    };
    Some((ratio, status.to_string()))
}

pub(crate) fn antigravity_quota_windows_from_retrieve_user_quota(
    value: &serde_json::Value,
) -> Vec<QuotaWindow> {
    let checked_at_unix = now_unix();
    let windows = value
        .get("buckets")
        .and_then(|buckets| buckets.as_array())
        .map(|buckets| {
            buckets
                .iter()
                .filter_map(|bucket| {
                    let remaining_ratio = numeric_json_value(bucket.get("remainingFraction"))
                        .or_else(|| {
                            numeric_json_value(bucket.get("remainingAmount"))
                                .map(|amount| if amount > 0.0 { 1.0 } else { 0.0 })
                        })?
                        .clamp(0.0, 1.0);
                    let token_type = json_string(bucket, &["tokenType"])
                        .filter(|value| !value.trim().is_empty());
                    let model =
                        json_string(bucket, &["modelId"]).filter(|value| !value.trim().is_empty());
                    let window = match (token_type.as_deref(), model.as_deref()) {
                        (Some(token_type), Some(model)) => format!("{token_type}:{model}"),
                        (Some(token_type), None) => token_type.to_string(),
                        (None, Some(model)) => model.to_string(),
                        (None, None) => "bucket".to_string(),
                    };
                    Some(QuotaWindow {
                        source: "antigravity_retrieve_user_quota".to_string(),
                        window,
                        remaining_ratio,
                        used_percent: ((1.0 - remaining_ratio) * 100.0).clamp(0.0, 100.0),
                        reset_at_unix: bucket
                            .get("resetTime")
                            .and_then(|value| value.as_str())
                            .and_then(parse_gemini_reset_time_unix)
                            .unwrap_or_default(),
                        checked_at_unix,
                        model,
                        token_type,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    crate::antigravity_models::canonicalize_quota_windows(windows)
}

fn parse_gemini_reset_time_unix(value: &str) -> Option<i64> {
    let value = value.trim();
    value
        .parse::<i64>()
        .ok()
        .filter(|value| *value > 0)
        .or_else(|| parse_rfc3339_z_unix(value))
}

fn parse_rfc3339_z_unix(value: &str) -> Option<i64> {
    let value = value.strip_suffix('Z')?;
    let mut parts = value.split('T');
    let date = parts.next()?;
    let time = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let mut date_parts = date.split('-');
    let year = date_parts.next()?.parse::<i64>().ok()?;
    let month = date_parts.next()?.parse::<i64>().ok()?;
    let day = date_parts.next()?.parse::<i64>().ok()?;
    if date_parts.next().is_some() {
        return None;
    }
    let time = time.split('.').next().unwrap_or(time);
    let mut time_parts = time.split(':');
    let hour = time_parts.next()?.parse::<i64>().ok()?;
    let minute = time_parts.next()?.parse::<i64>().ok()?;
    let second = time_parts.next()?.parse::<i64>().ok()?;
    if time_parts.next().is_some()
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=60).contains(&second)
    {
        return None;
    }
    let days = days_from_civil(year, month, day)?;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let year = year - if month <= 2 { 1 } else { 0 };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let month_prime = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * month_prime + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

fn quota_snapshot_from_windows(windows: &[QuotaWindow]) -> Option<(f64, String)> {
    crate::supplier::scoped_quota_summary(windows)
        .map(|summary| (summary.remaining_ratio, summary.status))
}

pub(crate) fn numeric_json_value(value: Option<&serde_json::Value>) -> Option<f64> {
    match value? {
        serde_json::Value::Number(number) => number.as_f64(),
        serde_json::Value::String(text) => text.trim().parse::<f64>().ok(),
        _ => None,
    }
}

pub(crate) fn subscription_credential_path(channel: &ChannelConfig) -> Result<PathBuf> {
    let source_path = PathBuf::from(channel.subscription.credential_ref.trim());
    if source_path.as_os_str().is_empty() {
        return Err(anyhow!("subscription credential reference is empty"));
    }
    if subscription_credential_is_managed_in(&source_path, &primary_subscription_credential_dir()) {
        return Ok(source_path);
    }
    let cli_proxy_dir = home_dir().join(".cli-proxy-api");
    if !path_is_within_directory(&source_path, &cli_proxy_dir) {
        // Explicit legacy paths remain caller-owned. New imports are always copied into the
        // managed store, while known scanned tool paths are migrated below without write-back.
        return Ok(source_path);
    }
    subscription_credential_path_in_dir(channel, &primary_subscription_credential_dir())
}

pub(crate) fn subscription_credential_path_in_dir(
    channel: &ChannelConfig,
    managed_dir: &Path,
) -> Result<PathBuf> {
    let path = channel.subscription.credential_ref.trim();
    if path.is_empty() {
        return Err(anyhow!("subscription credential reference is empty"));
    }
    let source_path = PathBuf::from(path);
    if subscription_credential_is_managed_in(&source_path, managed_dir) {
        return Ok(source_path);
    }

    let provider = crate::source_driver::source_driver(channel.source_driver())
        .ok()
        .and_then(|driver| driver.subscription_provider.as_deref())
        .and_then(normalize_subscription_provider)
        .or_else(|| normalize_subscription_provider(&channel.subscription.platform))
        .ok_or_else(|| anyhow!("subscription credential provider is unknown"))?;
    let storage_key = managed_import_storage_key(&source_path);
    let managed_path = managed_dir.join(format!(
        "{}-{}.json",
        provider,
        sanitize_filename_part(&storage_key)
    ));
    if managed_path.is_file() {
        return Ok(managed_path);
    }

    let candidate = subscription_credential_candidate_from_path(&source_path)
        .ok_or_else(|| anyhow!("unsupported subscription credential file"))?;
    if candidate.provider != provider {
        return Err(anyhow!(
            "credential provider mismatch: expected {}, got {}",
            provider,
            candidate.provider
        ));
    }
    let token = read_subscription_credential(&source_path)?;
    save_subscription_token_to_managed_dir(&provider, token, managed_dir, Some(storage_key))
}

pub(crate) fn credential_needs_refresh(value: &serde_json::Value) -> bool {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let Some(expiry) = token_expiry_unix(value) else {
        return false;
    };
    expiry <= now + 300
}

pub(crate) fn token_expiry_unix(value: &serde_json::Value) -> Option<i64> {
    json_string(value, &["expires_at_unix"])
        .or_else(|| json_string(value, &["expired"]))
        .or_else(|| json_string(value, &["expires_at"]))
        .or_else(|| json_string(value, &["expiresAt"]))
        .or_else(|| json_string(value, &["expiry_date"]))
        .and_then(|value| parse_token_expiry_text(&value))
}

fn parse_token_expiry_text(value: &str) -> Option<i64> {
    let value = value.trim();
    if let Ok(raw) = value.parse::<i64>() {
        if raw <= 0 {
            return None;
        }
        return Some(if raw > 10_000_000_000 {
            raw / 1000
        } else {
            raw
        });
    }
    time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .ok()
        .map(|value| value.unix_timestamp())
}

pub(crate) fn copy_token_metadata(from: &serde_json::Value, to: &mut serde_json::Value) {
    const REFRESH_RESPONSE_FIELDS: &[&str] = &[
        "access_token",
        "accessToken",
        "refresh_token",
        "refreshToken",
        "expires_in",
        "expires_at_unix",
        "expired",
        "expires_at",
        "expiresAt",
        "expiry_date",
        "token",
    ];
    let Some(source) = from.as_object() else {
        return;
    };
    let Some(object) = to.as_object_mut() else {
        return;
    };
    for (key, value) in source {
        if !object.contains_key(key) && !REFRESH_RESPONSE_FIELDS.contains(&key.as_str()) {
            object.insert(key.clone(), value.clone());
        }
    }
}

include!("detection/probes.rs");

include!("detection/subscription_credentials.rs");
