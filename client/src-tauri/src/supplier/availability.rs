//! Representative checks and their admission projection. The source catalog is never edited.
use super::{ChannelConfig, ChannelUpstreamTestResult, ProbeError, test_channel_upstream_attempt};
use futures_util::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    sync::{Mutex, OnceLock},
    time::Duration,
};

const GROUPS: [&str; 4] = [
    "anthropic_closed",
    "google_closed",
    "openai_closed",
    "other",
];
const CHECK_PROMPT: &str = "Reply with the single word OK.";
const RECHECK_SECONDS: i64 = 30 * 60;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct GroupResult {
    pub group: String,
    pub representative: String,
    pub upstream_model: String,
    pub status: String,
    pub cause: String,
    pub rule_id: String,
    pub message: String,
    pub attempts: usize,
    pub checked_at: i64,
    pub next_probe_at: i64,
    pub scope_inferred: bool,
    pub automatic_confirmed: bool,
}

impl GroupResult {
    fn applies_to_catalog(&self, channel: &ChannelConfig) -> bool {
        match crate::config::resolve_model_name(&channel.models, &self.representative) {
            Some(model) => {
                catalog_identity(channel, model)
                    == canonical_catalog_source(channel, &self.upstream_model)
            }
            // Suspension belongs to the group, not to the sampled model. Catalog
            // retirement is not recovery; a later check selects a current sample.
            None => self.status == "suspended",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct Report {
    pub groups: Vec<GroupResult>,
    pub attempts: usize,
    #[serde(skip)]
    pub test: Option<ChannelUpstreamTestResult>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Snapshot {
    fingerprint: String,
    revision: u64,
    groups: BTreeMap<String, GroupResult>,
    model_groups: BTreeMap<String, String>,
    admission_pending: bool,
    // One-time compatibility with subscriptions checked before grouped admission.
    // Do not reuse account history after an endpoint/credential identity change.
    legacy_admission_checked: bool,
    automatic_window_at: i64,
    automatic_attempts: usize,
    #[serde(skip)]
    last_demand_at: i64,
}

impl Snapshot {
    fn allows(&self, model: &str) -> bool {
        if self.admission_pending {
            return false;
        }
        let name = crate::config::short_model_name(model).to_ascii_lowercase();
        let group = self
            .model_groups
            .get(&name)
            .map(String::as_str)
            .unwrap_or_else(|| group(model));
        !self
            .groups
            .get("other")
            .is_some_and(|g| g.status == "suspended")
            && !self
                .groups
                .get(group)
                .is_some_and(|g| g.status == "suspended")
    }

    fn merge(&mut self, groups: &[GroupResult]) {
        for result in groups {
            // A cancelled/limited/inconclusive check cannot erase an earlier conclusion.
            if matches!(result.status.as_str(), "available" | "suspended") {
                self.groups.insert(result.group.clone(), result.clone());
            } else if !self.groups.contains_key(&result.group) {
                self.groups.insert(result.group.clone(), result.clone());
            } else if let Some(old) = self.groups.get_mut(&result.group) {
                old.next_probe_at = old.next_probe_at.max(result.next_probe_at);
            }
        }
        self.revision = self.revision.saturating_add(1);
    }

    fn merge_check(
        &mut self,
        channel: &ChannelConfig,
        groups: &[GroupResult],
        legacy_admitted: bool,
    ) {
        // A directory GET is not admission evidence. A saved execution check is:
        // legacy verified channels must not lose supply because a new check could
        // not run. This also repairs snapshots saved by the earlier fail-closed bug.
        let previously_admitted = (!self.admission_pending && self.revision > 0)
            || (!self.legacy_admission_checked && legacy_admitted)
            || channel.surface_bindings.iter().any(|binding| {
                binding
                    .protocols
                    .iter()
                    .any(|protocol| protocol.verification.state.trim() == "verified")
            })
            || channel.capability_profiles.iter().any(|profile| {
                !profile.catalog_metadata
                    && profile.non_stream_json
                    && profile.verification_state.trim() == "verified"
            });
        self.merge(groups);
        self.legacy_admission_checked = true;
        self.admission_pending =
            !previously_admitted && !self.groups.values().any(|g| g.status == "available");
        // Suspended groups still gate their own scope in allows(), even when the
        // channel had previously been admitted. Only a successful check clears them.
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Store {
    version: u8,
    channels: BTreeMap<String, Snapshot>,
    #[serde(skip)]
    checking: HashSet<String>,
}

fn decode(bytes: &[u8]) -> Option<Store> {
    if bytes.len() > 2 * 1024 * 1024 {
        return None;
    }
    let mut store: Store = serde_json::from_slice(bytes).ok()?;
    if store.version != 1 || store.channels.len() > 256 {
        return None;
    }
    for snapshot in store.channels.values_mut() {
        // Future groups/states are observations, not new hard gates.
        snapshot.groups.retain(|key, value| {
            GROUPS.contains(&key.as_str())
                && value.representative.len() <= 256
                && value.message.len() <= 1024
        });
        if !snapshot.groups.values().any(|g| {
            matches!(
                g.status.as_str(),
                "available" | "suspended" | "inconclusive" | "deferred"
            )
        }) {
            snapshot.admission_pending = false;
        }
    }
    Some(store)
}

fn store() -> &'static Mutex<Store> {
    static STATE: OnceLock<Mutex<Store>> = OnceLock::new();
    STATE.get_or_init(|| {
        let initial = Store {
            version: 1,
            ..Default::default()
        };
        #[cfg(not(test))]
        let initial = {
            let path = crate::config::client_data_root().join("channel-availability.json");
            match std::fs::read(&path) {
                Ok(bytes) => decode(&bytes).unwrap_or_else(|| {
                    log::warn!(
                        "channel_availability_snapshot_unreadable; retaining legacy admission"
                    );
                    initial
                }),
                Err(_) => initial,
            }
        };
        Mutex::new(initial)
    })
}

fn persist() {
    #[cfg(not(test))]
    {
        use std::io::Write;
        // Serialize writers, then take the latest committed state. A slow earlier
        // channel must never overwrite a newer channel's completed snapshot.
        static WRITER: Mutex<()> = Mutex::new(());
        let _writer = WRITER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let latest = store()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let save = || -> anyhow::Result<()> {
            let root = crate::config::client_data_root();
            std::fs::create_dir_all(&root)?;
            let mut file = tempfile::NamedTempFile::new_in(&root)?;
            serde_json::to_writer(&mut file, &latest)?;
            file.flush()?;
            file.as_file().sync_all()?;
            file.persist(root.join("channel-availability.json"))?;
            Ok(())
        };
        if let Err(error) = save() {
            log::warn!("channel_availability_snapshot_save_failed: {error}");
        }
    }
}

// This is deliberately independent of access tokens, display names, prices and model sorting.
// It is never logged or exported. Endpoints, credential binding and execution policy invalidate it.
fn fingerprint(channel: &ChannelConfig) -> String {
    let bindings = channel
        .surface_bindings
        .iter()
        .map(|b| (&b.surface, &b.base_url))
        .collect::<Vec<_>>();
    let bytes = serde_json::to_vec(&(
        &channel.v2.source_driver,
        &channel.v2.credential_ref,
        &channel.v2.executor,
        &channel.v2.default_target,
        &bindings,
        &channel.upstream_api_key,
    ))
    .unwrap_or_default();
    let mut hash = Sha256::new();
    hash.update(bytes);
    // An empty override preserves existing channels' persisted fingerprints.
    hash.update(channel.v2.user_agent_profile.as_bytes());
    hex::encode(hash.finalize())
}

fn legacy_subscription_success(
    channel: &ChannelConfig,
    state: Option<&crate::model::SubscriptionSafetyState>,
) -> bool {
    channel.kind == "subscription_adapter"
        && !channel.id.is_empty()
        && state.is_some_and(|state| {
            state.channel_id == channel.id
                && state.provider
                    == super::subscription_provider_from_config(&crate::supplier_from_channel(
                        channel,
                    ))
                && (200..300).contains(&state.last_status)
        })
}

fn legacy_subscription_admitted(channel: &ChannelConfig, revision: &str) -> bool {
    if channel.kind != "subscription_adapter" {
        return false;
    }
    let eligible = store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .channels
        .get(&channel.id)
        .is_none_or(|snapshot| {
            snapshot.fingerprint == revision && !snapshot.legacy_admission_checked
        });
    if !eligible {
        return false;
    }
    // Read the existing cached observation, without hydrating it into a different
    // provider or synthesizing a default active state. Never add I/O to routing.
    let saved = super::cached_subscription_safety_state(&channel.id)
        .ok()
        .flatten();
    legacy_subscription_success(channel, saved.as_ref())
}

/// Bounded name rules, not API protocol or execution-provider identity. Unknown developers
/// remain other; open weights from OpenAI/Google do not enter their closed-model group.
pub(crate) fn group(model: &str) -> &'static str {
    let full = model.trim().to_ascii_lowercase();
    let name = crate::config::short_model_name(&full);
    if let Some((vendor, _)) = full.rsplit_once('/') {
        if !matches!(vendor, "anthropic" | "google" | "openai" | "models") {
            return "other";
        }
    }
    if name.starts_with("claude-") {
        "anthropic_closed"
    } else if name.starts_with("gemini-") {
        "google_closed"
    } else if !name.starts_with("gpt-oss")
        && (name.starts_with("gpt-")
            || name.starts_with("chatgpt-")
            || name.starts_with("codex-")
            || name.as_bytes().first() == Some(&b'o')
                && name.as_bytes().get(1).is_some_and(u8::is_ascii_digit))
    {
        "openai_closed"
    } else {
        "other"
    }
}

fn candidate_rank(model: &str) -> (u8, u8, String) {
    let name = model.to_ascii_lowercase();
    let unstable = name.contains(":free")
        || name.contains("preview")
        || name.contains("experimental")
        || name.contains("thinking")
        || name.contains("reasoning");
    let cheap = [
        "haiku",
        "flash-lite",
        "flash_lite",
        "luna",
        "nano",
        "mini",
        "instant",
        "8b",
        "27b",
    ];
    let rank = if cheap.iter().any(|s| name.contains(s)) {
        0
    } else if [
        "flash", "sonnet", "sol", "qwen", "llama", "mistral", "gemma",
    ]
    .iter()
    .any(|s| name.contains(s))
    {
        1
    } else if name.contains("opus") || name.contains("fable") || name.contains("pro") {
        3
    } else {
        2
    };
    (u8::from(unstable), rank, name)
}

fn representatives(channel: &ChannelConfig) -> Vec<(String, String)> {
    let mut groups: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let metadata = channel
        .capability_profiles
        .iter()
        .filter(|p| p.catalog_metadata)
        .map(|p| (p.model_pattern.to_ascii_lowercase(), p))
        .collect::<BTreeMap<_, _>>();
    for model in &channel.models {
        let lower = model.to_ascii_lowercase();
        let short = crate::config::short_model_name(&lower);
        if matches!(short, "auto" | "router" | "bodybuilder")
            || lower == "openrouter/free"
            || short.ends_with(":batch")
        {
            continue;
        }
        if [
            "embedding",
            "whisper",
            "tts",
            "image",
            "dall-e",
            "video",
            "moderation",
            "rerank",
        ]
        .iter()
        .any(|s| short.contains(s))
        {
            continue;
        }
        if metadata
            .get(&lower)
            .is_some_and(|p| p.catalog_text_output == Some(false))
        {
            continue;
        }
        groups
            .entry(group(&catalog_identity(channel, model)))
            .or_default()
            .push(model.clone());
    }
    groups
        .into_iter()
        .filter_map(|(group, mut models)| {
            models.sort_by_key(|model| {
                let (unstable, fallback, name) = candidate_rank(model);
                let cost = metadata.get(&name).and_then(|p| p.probe_cost_nanos);
                (unstable, cost.unwrap_or(u64::MAX), fallback, name)
            });
            models
                .into_iter()
                .next()
                .map(|model| (group.to_string(), model))
        })
        .collect()
}

pub(crate) fn allows(channel: &ChannelConfig, model: &str) -> bool {
    let mut state = store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(snapshot) = state
        .channels
        .get_mut(&channel.id)
        .filter(|s| s.fingerprint == fingerprint(channel))
    else {
        return true;
    };
    let allowed = snapshot.allows(model);
    if !allowed {
        snapshot.last_demand_at = crate::now_unix();
    }
    allowed
}

pub(crate) fn effective_models(channel: &ChannelConfig, models: Vec<String>) -> Vec<String> {
    let state = store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(snapshot) = state
        .channels
        .get(&channel.id)
        .filter(|s| s.fingerprint == fingerprint(channel))
    else {
        return models;
    };
    models
        .into_iter()
        .filter(|model| snapshot.allows(model))
        .collect()
}

// An active channel can re-evaluate a paused sibling group after its cooldown.
// Catalog reads, heartbeats and our own checks are not business demand.
pub(crate) fn note_activity(channel: &ChannelConfig) {
    if crate::upstream_transport::probe_budget_active() {
        return;
    }
    let mut state = store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(snapshot) = state
        .channels
        .get_mut(&channel.id)
        .filter(|s| s.fingerprint == fingerprint(channel))
    {
        snapshot.last_demand_at = crate::now_unix();
    }
}

fn catalog_identity(channel: &ChannelConfig, model: &str) -> String {
    let source = channel
        .capability_profiles
        .iter()
        .find(|p| p.catalog_metadata && p.model_pattern.eq_ignore_ascii_case(model))
        .and_then(|p| p.catalog_source_model.clone())
        .unwrap_or_else(|| crate::openrouter::upstream_model(channel, model));
    canonical_catalog_source(channel, &source)
}

fn canonical_catalog_source(channel: &ChannelConfig, source: &str) -> String {
    // Compatibility with snapshots written before refresh preserved source IDs.
    // Only reconstruct known pathless OpenRouter aliases. A real namespace change
    // remains a different identity and must still invalidate the old conclusion.
    if channel.source_driver() == crate::source_driver::SourceDriverId::Openrouter
        && !source.contains('/')
    {
        crate::openrouter::upstream_model(channel, source)
    } else {
        source.to_string()
    }
}

/// Read the effective conclusions, not an incomplete later attempt or stale config copy.
/// Removed samples retain group suspensions; changed identities invalidate conclusions.
pub(crate) fn groups_for_channel(channel: &ChannelConfig) -> Vec<GroupResult> {
    let identity = fingerprint(channel);
    let state = store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state
        .channels
        .get(&channel.id)
        .filter(|snapshot| snapshot.fingerprint == identity)
        .map(|snapshot| {
            snapshot
                .groups
                .values()
                .filter(|result| result.applies_to_catalog(channel))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

// Catalog reconciliation is off the per-request path. Changed upstream mappings
// do not inherit old hard gates; ordinary token refresh does not invalidate them.
pub(crate) fn reconcile_catalog(channel: &ChannelConfig) {
    let mut state = store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(snapshot) = state
        .channels
        .get_mut(&channel.id)
        .filter(|s| s.fingerprint == fingerprint(channel))
    else {
        return;
    };
    let before = snapshot.groups.len();
    snapshot.groups.retain(|_, g| g.applies_to_catalog(channel));
    let changed = before != snapshot.groups.len();
    if changed {
        snapshot.admission_pending = false;
    }
    drop(state);
    if changed {
        persist();
    }
}

pub(crate) fn admission_pending(channel: &ChannelConfig) -> bool {
    let state = store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state
        .channels
        .get(&channel.id)
        .filter(|s| s.fingerprint == fingerprint(channel))
        .is_some_and(|s| s.admission_pending)
}

pub(crate) fn has_check_snapshot(channel: &ChannelConfig) -> bool {
    let identity = fingerprint(channel);
    store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .channels
        .get(&channel.id)
        .is_some_and(|snapshot| {
            snapshot.fingerprint == identity
                && snapshot.revision > 0
                && snapshot.groups.values().any(|result| {
                    matches!(
                        result.status.as_str(),
                        "available" | "suspended" | "inconclusive" | "deferred"
                    ) && result.applies_to_catalog(channel)
                })
        })
}

pub(crate) fn recheck_due(channel: &ChannelConfig, now: i64) -> bool {
    let state = store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    !state.checking.contains(&channel.id)
        && state
            .channels
            .get(&channel.id)
            .filter(|s| s.fingerprint == fingerprint(channel))
            .is_some_and(|s| {
                s.groups.values().any(|g| {
                    g.status == "suspended"
                        && !g.automatic_confirmed
                        && g.next_probe_at <= now
                        && s.last_demand_at >= g.checked_at
                }) && (s.automatic_window_at <= now - 3600 || s.automatic_attempts < 8)
            })
}

// Existing offline full recovery shares the same hourly envelope as due-group
// recovery. Reserve before awaiting, so cancellation/restarts cannot multiply spend.
pub(crate) fn reserve_offline_recovery_budget(channel: &ChannelConfig) -> usize {
    let mut state = store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if state.channels.len() >= 256 && !state.channels.contains_key(&channel.id) {
        return 0;
    }
    let snapshot = state.channels.entry(channel.id.clone()).or_default();
    let revision = fingerprint(channel);
    if snapshot.fingerprint != revision {
        *snapshot = Snapshot {
            legacy_admission_checked: !snapshot.fingerprint.is_empty(),
            fingerprint: revision,
            ..Default::default()
        };
    }
    let now = crate::now_unix();
    if snapshot.automatic_window_at <= now - 3600 {
        snapshot.automatic_window_at = now;
        snapshot.automatic_attempts = 0;
    }
    let reserved = 8_usize.saturating_sub(snapshot.automatic_attempts);
    snapshot.automatic_attempts += reserved;
    drop(state);
    if reserved > 0 {
        persist();
    }
    reserved
}

tokio::task_local! { static AUTOMATIC_RECHECK: bool; }
pub(crate) fn is_automatic_recheck() -> bool {
    AUTOMATIC_RECHECK.try_with(|v| *v).unwrap_or(false)
}
pub(crate) async fn automatic_recheck<F: std::future::Future>(future: F) -> F::Output {
    AUTOMATIC_RECHECK.scope(true, future).await
}

struct CheckGuard(String);
impl Drop for CheckGuard {
    fn drop(&mut self) {
        store()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .checking
            .remove(&self.0);
    }
}

pub(crate) async fn check(channel: &ChannelConfig) -> Report {
    let mut targets = representatives(channel);
    if targets.is_empty() {
        return Report::default();
    }
    let revision = fingerprint(channel);
    let legacy_admitted = legacy_subscription_admitted(channel, &revision);
    {
        let mut state = store()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.checking.insert(channel.id.clone()) {
            return Report {
                groups: targets
                    .into_iter()
                    .map(|(group, representative)| GroupResult {
                        group,
                        representative,
                        status: "deferred".into(),
                        message: "channel check already running".into(),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            };
        }
        if is_automatic_recheck() {
            if let Some(snapshot) = state
                .channels
                .get_mut(&channel.id)
                .filter(|s| s.fingerprint == fingerprint(channel))
            {
                let now = crate::now_unix();
                targets.retain(|(group, _)| {
                    snapshot.groups.get(group).is_some_and(|g| {
                        g.status == "suspended"
                            && !g.automatic_confirmed
                            && g.next_probe_at <= now
                            && snapshot.last_demand_at >= g.checked_at
                    })
                });
                if snapshot.automatic_window_at <= now - 3600 {
                    snapshot.automatic_window_at = now;
                    snapshot.automatic_attempts = 0;
                }
                targets.truncate(8_usize.saturating_sub(snapshot.automatic_attempts) / 2);
                snapshot.automatic_attempts += targets.len() * 2;
            } else {
                targets.clear();
            }
        }
        // Reserve the automatic recheck window before awaiting. Cancellation cannot
        // turn an expired suspended group into a 30-second paid polling loop.
        if let Some(snapshot) = state.channels.get_mut(&channel.id) {
            for group in snapshot.groups.values_mut().filter(|g| {
                g.status == "suspended" && targets.iter().any(|(group, _)| *group == g.group)
            }) {
                group.next_probe_at = group.next_probe_at.max(crate::now_unix() + RECHECK_SECONDS);
            }
        }
        drop(state);
        persist();
    }
    let _guard = CheckGuard(channel.id.clone());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    let deadline = crate::detection::channel_check_deadline()
        .map(|parent| deadline.min(parent))
        .unwrap_or(deadline);
    let parallel = if channel.max_concurrency() == 0 {
        4
    } else {
        (channel.max_concurrency() as usize).min(4)
    };
    let mut results = stream::iter(targets.into_iter().map(|(group, model)| async move {
        // Scoped task-local accounting includes transport and adapter retries; it does not
        // affect ordinary traffic or credential/catalog POSTs.
        let ((result, test), used) = crate::upstream_transport::with_probe_budget(
            2,
            check_group(channel, group, model, deadline),
        )
        .await;
        (
            GroupResult {
                attempts: used,
                ..result
            },
            test,
        )
    }))
    .buffer_unordered(parallel)
    .collect::<Vec<_>>()
    .await;
    results.sort_by(|a, b| a.0.group.cmp(&b.0.group));
    let report = Report {
        attempts: results.iter().map(|r| r.0.attempts).sum(),
        test: results.iter().find_map(|r| r.1.clone()),
        groups: results.into_iter().map(|r| r.0).collect(),
    };
    let mut state = store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if state.channels.len() < 256 || state.channels.contains_key(&channel.id) {
        let snapshot = state.channels.entry(channel.id.clone()).or_default();
        if snapshot.fingerprint != revision {
            *snapshot = Snapshot {
                fingerprint: revision,
                ..Default::default()
            };
        }
        snapshot.model_groups = channel
            .models
            .iter()
            .map(|model| {
                (
                    crate::config::short_model_name(model).to_ascii_lowercase(),
                    group(&catalog_identity(channel, model)).into(),
                )
            })
            .collect();
        snapshot.merge_check(channel, &report.groups, legacy_admitted);
        drop(state);
        persist();
    }
    report
}

async fn check_group(
    channel: &ChannelConfig,
    group: String,
    model: String,
    deadline: tokio::time::Instant,
) -> (GroupResult, Option<ChannelUpstreamTestResult>) {
    let now = crate::now_unix();
    let mut result = GroupResult {
        group,
        upstream_model: catalog_identity(channel, &model),
        representative: crate::config::short_model_name(&model).to_ascii_lowercase(),
        status: "deferred".into(),
        checked_at: now,
        next_probe_at: now + RECHECK_SECONDS,
        ..Default::default()
    };
    let mut valid_failures = 0;
    static SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
    let Ok(Ok(_slot)) = tokio::time::timeout_at(deadline, SLOTS.acquire()).await else {
        result.message = "waiting for local check capacity; no upstream failure".into();
        return (result, None);
    };
    for _ in 0..2 {
        let attempt_deadline = deadline.min(tokio::time::Instant::now() + Duration::from_secs(22));
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        let attempt = tokio::time::timeout_at(
            attempt_deadline,
            test_channel_upstream_attempt(
                channel.clone(),
                Some(model.clone()),
                CHECK_PROMPT.into(),
            ),
        )
        .await;
        let error = match attempt {
            Ok(Ok(test)) => {
                result.status = "available".into();
                result.next_probe_at = 0;
                return (result, Some(test));
            }
            Ok(Err(error)) => error,
            Err(_) => ProbeError {
                failure: None,
                message: "check deadline reached; availability not established".into(),
                deferred: true,
            },
        };
        let mut message = error.message.clone();
        for secret in [&channel.upstream_api_key, &channel.v2.credential_ref] {
            if !secret.is_empty() {
                message = message.replace(secret, "[redacted]");
            }
        }
        result.message = crate::upstream_failure::safe_excerpt(&message);
        let Some(failure) = error.failure.as_ref().filter(|_| !error.deferred) else {
            if !error.deferred {
                result.status = "inconclusive".into();
            }
            break;
        };
        result.cause = failure.cause.clone();
        result.rule_id = failure.rule_id.clone();
        if !failure.group_failure() {
            result.status = "inconclusive".into();
            break;
        }
        valid_failures += 1;
        if valid_failures == 2 {
            result.status = "suspended".into();
            result.scope_inferred = true;
            result.automatic_confirmed = is_automatic_recheck()
                && matches!(
                    failure.cause.as_str(),
                    "location_unsupported" | "provider_eligibility_restricted"
                );
            break;
        }
        let delay = failure.retry_after_seconds.unwrap_or(0).max(0);
        result.next_probe_at = result.next_probe_at.max(now.saturating_add(delay));
        // Respect upstream not-before without spending the whole interactive deadline asleep.
        if delay > 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis((delay as u64 * 1000).max(250))).await;
    }
    (result, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_user_agent_preserves_the_legacy_check_fingerprint() {
        let channel = crate::default_config().channels.remove(0);
        let bindings = channel
            .surface_bindings
            .iter()
            .map(|binding| (&binding.surface, &binding.base_url))
            .collect::<Vec<_>>();
        let legacy_bytes = serde_json::to_vec(&(
            &channel.v2.source_driver,
            &channel.v2.credential_ref,
            &channel.v2.executor,
            &channel.v2.default_target,
            &bindings,
            &channel.upstream_api_key,
        ))
        .unwrap();
        assert_eq!(
            fingerprint(&channel),
            hex::encode(Sha256::digest(legacy_bytes))
        );
    }

    #[test]
    fn cached_check_requires_an_observation_for_the_current_identity_and_catalog() {
        let mut channel = crate::default_config().channels.remove(0);
        channel.id = "cached-check-snapshot-identity".into();
        channel.models = vec!["qwen-fixture".into()];
        let mut snapshot = Snapshot {
            fingerprint: fingerprint(&channel),
            revision: 1,
            ..Default::default()
        };
        store()
            .lock()
            .unwrap()
            .channels
            .insert(channel.id.clone(), snapshot.clone());
        assert!(
            !has_check_snapshot(&channel),
            "a budget reservation is not a check"
        );
        snapshot.merge(&[GroupResult {
            group: "other".into(),
            representative: "qwen-fixture".into(),
            upstream_model: "qwen-fixture".into(),
            status: "inconclusive".into(),
            ..Default::default()
        }]);
        store()
            .lock()
            .unwrap()
            .channels
            .insert(channel.id.clone(), snapshot);
        assert!(has_check_snapshot(&channel));
        let mut changed = channel.clone();
        changed.v2.credential_ref = "new-credential".into();
        assert!(!has_check_snapshot(&changed));
        changed = channel.clone();
        changed.v2.user_agent_profile = crate::channel_user_agent::PROFILE_OPENCODE.into();
        assert!(!has_check_snapshot(&changed));
        assert!(has_check_snapshot(&channel));
        changed = channel.clone();
        changed.models = vec!["unrelated-model".into()];
        assert!(!has_check_snapshot(&changed));
        reconcile_catalog(&changed);
        assert!(!has_check_snapshot(&changed));
        store().lock().unwrap().channels.remove(&channel.id);
    }

    #[tokio::test]
    async fn cached_activation_and_hot_registration_keep_admission_gates() {
        for pending in [false, true] {
            let mut channel = crate::default_config().channels.remove(0);
            channel.id = format!("cached-activation-admission-{pending}");
            channel.enabled = true;
            channel.models = vec!["qwen-fixture".into()];
            channel.surface_bindings[0].verification = crate::model::SurfaceVerification {
                state: "verified".into(),
                checked_at_unix: 1,
                summary: "old verification".into(),
            };
            let mut snapshot = Snapshot {
                fingerprint: fingerprint(&channel),
                admission_pending: pending,
                ..Default::default()
            };
            snapshot.merge(&[GroupResult {
                group: "other".into(),
                representative: "qwen-fixture".into(),
                upstream_model: "qwen-fixture".into(),
                status: if pending { "inconclusive" } else { "suspended" }.into(),
                ..Default::default()
            }]);
            store()
                .lock()
                .unwrap()
                .channels
                .insert(channel.id.clone(), snapshot);
            let (outcome, attempts) = crate::upstream_transport::with_probe_budget(
                0,
                super::super::validate_supplier_channel(
                    channel.clone(),
                    super::super::SupplierCheckMode::Activation,
                ),
            )
            .await;
            let (_, mut health) = outcome.unwrap();
            assert_eq!(attempts, 0);
            assert_eq!(health.status, "unavailable");
            assert_eq!(health.model_count, 0);
            // A prior failed/unverified admission must not be paid for again on restart.
            channel.surface_bindings[0].verification = Default::default();
            assert!(has_check_snapshot(&channel));
            let (again, attempts) = crate::upstream_transport::with_probe_budget(
                0,
                super::super::validate_supplier_channel(
                    channel.clone(),
                    super::super::SupplierCheckMode::Activation,
                ),
            )
            .await;
            assert_eq!(again.unwrap().1.status, "unavailable");
            assert_eq!(attempts, 0);
            // Even an older healthy runtime observation cannot override a model gate.
            health.status = "available".into();
            let mut cfg = crate::default_config();
            cfg.channels = vec![channel.clone()];
            let payload = super::super::supplier_registration_snapshot(
                &cfg,
                &[channel.id.clone()],
                &[health],
            );
            assert!(payload[0].models.is_empty());
            assert_eq!(
                payload[0].registration_health_status.as_deref(),
                Some("blocked")
            );
            store().lock().unwrap().channels.remove(&channel.id);
        }
    }

    #[test]
    fn legacy_subscription_admission_requires_real_matching_success_and_upgrades_once() {
        for source in [
            crate::source_driver::SourceDriverId::OpenAiSubscription,
            crate::source_driver::SourceDriverId::ClaudeSubscription,
            crate::source_driver::SourceDriverId::GeminiSubscription,
            crate::source_driver::SourceDriverId::GrokSubscription,
        ] {
            let mut channel = crate::default_config().channels.remove(0);
            channel.set_source_driver(source);
            channel.id = format!("legacy-admission-{source:?}");
            channel.capability_profiles.clear();
            channel.surface_bindings = crate::source_driver_surface_bindings(source, "");
            channel = crate::project_channel_legacy_fields(channel);
            let config = crate::supplier_from_channel(&channel);
            let mut saved = super::super::default_subscription_safety_state(&config);
            let deferred = GroupResult {
                group: "openai_closed".into(),
                status: "deferred".into(),
                ..Default::default()
            };
            assert!(!legacy_subscription_success(&channel, None));
            assert!(
                !legacy_subscription_success(&channel, Some(&saved)),
                "default active state is not proof"
            );
            saved.last_status = 200;
            let admitted = legacy_subscription_success(&channel, Some(&saved));
            assert!(admitted, "{source:?}");
            // Reproduce a snapshot saved by the earlier false-negative bug.
            let mut snapshot = Snapshot {
                admission_pending: true,
                revision: 1,
                ..Default::default()
            };
            snapshot.merge_check(&channel, &[deferred.clone()], admitted);
            assert!(snapshot.allows("gpt-luna"));
            snapshot.merge_check(
                &channel,
                &[GroupResult {
                    group: "other".into(),
                    status: "suspended".into(),
                    ..Default::default()
                }],
                admitted,
            );
            assert!(!snapshot.allows("gpt-luna"), "a real suspension still wins");
            let mut new_identity = Snapshot {
                legacy_admission_checked: true,
                ..Default::default()
            };
            new_identity.merge_check(&channel, &[deferred], admitted);
            assert!(
                new_identity.admission_pending,
                "do not replay legacy success after identity changes"
            );
            saved.provider = "different-provider".into();
            assert!(!legacy_subscription_success(&channel, Some(&saved)));
            saved.provider = super::super::subscription_provider_from_config(&config);
            saved.channel_id = "different-channel".into();
            assert!(!legacy_subscription_success(&channel, Some(&saved)));
            saved.channel_id = channel.id.clone();
            saved.last_status = 429;
            assert!(!legacy_subscription_success(&channel, Some(&saved)));
        }
    }

    #[test]
    fn legacy_admission_marker_survives_serialization_and_identity_reset() {
        let mut channel = crate::default_config().channels.remove(0);
        channel.id = "legacy-admission-marker".into();
        store().lock().unwrap().channels.insert(
            channel.id.clone(),
            Snapshot {
                fingerprint: "old-identity".into(),
                ..Default::default()
            },
        );
        reserve_offline_recovery_budget(&channel);
        let mut state = store().lock().unwrap();
        let snapshot = state.channels.remove(&channel.id).unwrap();
        assert!(snapshot.legacy_admission_checked);
        let mut isolated = Store {
            version: 1,
            ..Default::default()
        };
        isolated.channels.insert(channel.id.clone(), snapshot);
        let decoded = decode(&serde_json::to_vec(&isolated).unwrap()).unwrap();
        assert!(decoded.channels[&channel.id].legacy_admission_checked);
        assert!(
            !serde_json::from_str::<Snapshot>("{}")
                .unwrap()
                .legacy_admission_checked
        );
    }

    #[tokio::test]
    async fn zero_attempt_check_preserves_legacy_execution_but_not_new_admission() {
        for verified in [false, true] {
            let mut channel = crate::default_config().channels.remove(0);
            channel.id = format!("availability-zero-budget-{verified}");
            channel.models = vec!["gpt-luna".into()];
            channel.public_model = channel.models[0].clone();
            channel.upstream_model = channel.public_model.clone();
            channel.capability_profiles.clear();
            for binding in &mut channel.surface_bindings {
                binding.verification.state = "verified".into(); // catalog only
                for protocol in &mut binding.protocols {
                    protocol.verification.state =
                        if verified { "verified" } else { "unknown" }.into();
                }
            }
            assert!(allows(&channel, "gpt-luna"));
            let (report, attempts) =
                crate::upstream_transport::with_probe_budget(0, check(&channel)).await;
            assert_eq!(attempts, 0);
            assert_eq!(report.attempts, 0);
            assert!(
                report
                    .groups
                    .iter()
                    .all(|g| matches!(g.status.as_str(), "inconclusive" | "deferred")),
                "{report:?}"
            );
            assert_eq!(
                effective_models(&channel, channel.models.clone()).len(),
                usize::from(verified)
            );
            store().lock().unwrap().channels.remove(&channel.id);
        }
    }

    #[test]
    fn inconclusive_checks_preserve_admission_and_effective_failure_scope() {
        let mut channel = crate::default_config().channels.remove(0);
        channel.capability_profiles.clear();
        channel.surface_bindings.clear();
        let mut snapshot = Snapshot::default();
        let result = |group: &str, status: &str| GroupResult {
            group: group.into(),
            status: status.into(),
            ..Default::default()
        };
        snapshot.merge_check(&channel, &[result("openai_closed", "deferred")], false);
        assert!(
            !snapshot.allows("gpt-luna"),
            "new channels still await admission"
        );
        snapshot.merge_check(&channel, &[result("openai_closed", "available")], false);
        snapshot.merge_check(&channel, &[result("anthropic_closed", "suspended")], false);
        snapshot.merge_check(
            &channel,
            &[
                result("openai_closed", "inconclusive"),
                result("anthropic_closed", "deferred"),
            ],
            false,
        );
        assert!(snapshot.allows("gpt-luna"));
        assert!(!snapshot.allows("claude-haiku-4-5"));
        snapshot.merge_check(&channel, &[result("other", "suspended")], false);
        assert!(
            !snapshot.allows("gpt-luna"),
            "confirmed other-group failure still blocks the channel"
        );

        channel
            .capability_profiles
            .push(crate::model::ChannelCapabilityProfile {
                non_stream_json: true,
                verification_state: "verified".into(),
                ..Default::default()
            });
        let mut poisoned = Snapshot {
            admission_pending: true,
            ..Default::default()
        };
        poisoned.merge_check(&channel, &[result("openai_closed", "deferred")], false);
        assert!(
            poisoned.allows("gpt-luna"),
            "repair a previously persisted inconclusive-only snapshot"
        );
    }

    #[test]
    fn tool_catalog_excludes_suspended_local_groups_without_hiding_other_routes() {
        let mut config = crate::default_config();
        let mut channel = config.channels.remove(0);
        channel.id = "availability-tool-catalog".into();
        channel.enabled = true;
        channel.models = vec!["claude-fable-5".into(), "qwen-27b".into()];
        channel.public_model = "claude-fable-5".into();
        channel.upstream_model = channel.public_model.clone();
        store().lock().unwrap().channels.insert(
            channel.id.clone(),
            Snapshot {
                fingerprint: fingerprint(&channel),
                groups: BTreeMap::from([(
                    "anthropic_closed".into(),
                    GroupResult {
                        group: "anthropic_closed".into(),
                        status: "suspended".into(),
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            },
        );
        config.channels = vec![channel.clone()];
        let mut ready = HashSet::from([channel.id.clone()]);
        let ids = |config: &crate::model::ClientConfig, ready: &HashSet<String>| {
            let entries = crate::proxy::local_model_entries(config, ready);
            crate::tool_model_metadata::tool_models_from_response(
                &serde_json::json!({"data":entries}),
            )
            .into_iter()
            .map(|model| model.id)
            .collect::<Vec<_>>()
        };
        assert_eq!(ids(&config, &ready), ["qwen-27b"]);
        assert_eq!(
            config.channels[0].models, channel.models,
            "the source catalog remains intact"
        );

        let mut healthy = channel.clone();
        healthy.id = "availability-tool-catalog-healthy".into();
        ready.insert(healthy.id.clone());
        config.channels.push(healthy);
        assert_eq!(ids(&config, &ready), ["claude-fable-5", "qwen-27b"]);

        config.channels.pop();
        store()
            .lock()
            .unwrap()
            .channels
            .get_mut(&channel.id)
            .unwrap()
            .groups
            .get_mut("anthropic_closed")
            .unwrap()
            .status = "available".into();
        assert_eq!(
            ids(&config, &ready),
            ["claude-fable-5", "qwen-27b"],
            "recovery is visible on the next catalog read"
        );
        store().lock().unwrap().channels.remove(&channel.id);
    }

    #[test]
    fn ui_groups_read_effective_snapshot_without_config_evidence() {
        let mut channel = crate::default_config().channels.remove(0);
        channel.id = "availability-ui-snapshot".into();
        channel.models = vec!["gemini-flash-lite".into()];
        channel.detection_checks.clear();
        assert!(groups_for_channel(&channel).is_empty());
        let result = GroupResult {
            group: "google_closed".into(),
            representative: channel.models[0].clone(),
            upstream_model: catalog_identity(&channel, &channel.models[0]),
            status: "suspended".into(),
            checked_at: 123,
            attempts: 2,
            message: "User location is not supported".into(),
            ..Default::default()
        };
        let mut snapshot = Snapshot {
            fingerprint: fingerprint(&channel),
            ..Default::default()
        };
        snapshot.merge(&[result.clone()]);
        snapshot.merge(&[GroupResult {
            status: "inconclusive".into(),
            checked_at: 456,
            ..result
        }]);
        store()
            .lock()
            .unwrap()
            .channels
            .insert(channel.id.clone(), snapshot);
        let groups = groups_for_channel(&channel);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].status, "suspended");
        assert_eq!(groups[0].checked_at, 123);
        assert_eq!(groups[0].attempts, 2);
        let exported = serde_json::to_value(&groups).unwrap();
        assert!(exported[0].get("fingerprint").is_none());
        assert!(
            channel.detection_checks.is_empty(),
            "UI read must not mutate config"
        );
        channel.name = "renamed".into();
        channel.price_ratio = 0.5;
        assert_eq!(groups_for_channel(&channel).len(), 1);
        channel.v2.credential_ref.push_str("changed");
        assert!(groups_for_channel(&channel).is_empty());
        store().lock().unwrap().channels.remove(&channel.id);
    }

    #[tokio::test]
    async fn openrouter_refresh_and_snapshot_preserve_all_group_identities_and_gates() {
        use warp::Filter;
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = calls.clone();
        let ids = [
            "google/gemini-2.5-flash-lite",
            "google/gemini-3.8-flash",
            "anthropic/claude-3-haiku",
            "openai/gpt-5-nano",
            "mistralai/mistral-nemo",
        ];
        let route = warp::method().map(move |method: warp::http::Method| {
            assert_eq!(
                method,
                warp::http::Method::GET,
                "refresh must not run paid probes"
            );
            let refresh = seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let models = ids.map(|id| {
                // Catalog turnover must not heal the previously suspended family.
                let id = if refresh > 0 && id == "anthropic/claude-3-haiku" {
                    "anthropic/claude-sonnet-5.5"
                } else {
                    id
                };
                serde_json::json!({
                "id":id, "context_length":1000000, "supported_parameters":["tools"],
                "architecture":{"input_modalities":["text"],"output_modalities":["text"]}
                })
            });
            warp::reply::json(&serde_json::json!({"data": models}))
        });
        let (addr, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
        let server = tokio::spawn(server);
        let mut channel = crate::default_config().channels.remove(0);
        channel.id = "availability-openrouter-refresh-roundtrip".into();
        channel.set_source_driver(crate::source_driver::SourceDriverId::Openrouter);
        channel.surface_bindings = crate::source_driver_surface_bindings(
            channel.source_driver(),
            &format!("http://{addr}"),
        );
        for surface in &mut channel.surface_bindings {
            surface.verification.state = "verified".into();
            for protocol in &mut surface.protocols {
                protocol.verification.state = "verified".into();
            }
        }
        channel.models = ids.map(str::to_string).to_vec();
        channel.public_model = ids[0].into();
        channel.upstream_model = ids[0].into();
        channel.capability_profiles.clear();
        let results = [ids[0], ids[2], ids[3], ids[4]].map(|source| GroupResult {
            group: group(source).into(),
            representative: crate::config::short_model_name(source).into(),
            upstream_model: source.into(),
            status: if group(source) == "other" {
                "available"
            } else {
                "suspended"
            }
            .into(),
            checked_at: 42,
            attempts: 2,
            ..Default::default()
        });
        let mut snapshot = Snapshot {
            fingerprint: fingerprint(&channel),
            ..Default::default()
        };
        snapshot.merge(&results);
        store()
            .lock()
            .unwrap()
            .channels
            .insert(channel.id.clone(), snapshot);
        for periodic in [false, true, false] {
            let report = if periodic {
                crate::detection::snapshot_channel(channel.clone()).await
            } else {
                crate::detection::refresh_channel(channel.clone()).await
            }
            .unwrap();
            let google: Vec<_> = report
                .model_capability_evidence
                .iter()
                .filter(|p| p.catalog_metadata && p.model_pattern == "gemini-2.5-flash-lite")
                .collect();
            assert_eq!(
                google.len(),
                3,
                "all native protocol catalogs retain the same source"
            );
            for profile in google {
                assert_eq!(profile.catalog_source_model.as_deref(), Some(ids[0]));
                assert_eq!(profile.context_tokens, Some(1000000));
            }
            crate::supplier::merge_channel_detection_result(&mut channel, &report);
            crate::openrouter::normalize_channel(&mut channel);
            reconcile_catalog(&channel);
            assert_eq!(
                groups_for_channel(&channel).len(),
                4,
                "group conclusions must survive representative retirement"
            );
            let permitted = effective_models(&channel, channel.models.clone());
            assert_eq!(
                permitted.len(),
                1,
                "closed-provider restrictions must survive refresh"
            );
            assert!(permitted[0].ends_with("mistral-nemo"));
            let config = crate::supplier_from_channel(&channel);
            let agent = crate::supplier::SupplierAgentConfig {
                client_id: "roundtrip-client".into(),
                session_id: "roundtrip-session".into(),
                suppliers: vec![config.clone()],
                server_ws_url: "ws://127.0.0.1:1".into(),
                server_quic_url: String::new(),
                access_token: std::sync::Arc::new(std::sync::Mutex::new(String::new())),
                account_refresh_notify: std::sync::Arc::new(tokio::sync::Notify::new()),
            };
            let payload = crate::supplier::supplier_register_payload(&agent, &config);
            assert_eq!(
                payload["models"],
                serde_json::json!(permitted),
                "platform registration uses the same gates"
            );
        }
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::Relaxed),
            3,
            "one free catalog GET per refresh"
        );
        server.abort();
        store().lock().unwrap().channels.remove(&channel.id);
    }

    #[test]
    fn suspended_group_survives_catalog_gap_and_restart_until_successful_check() {
        let mut channel = crate::default_config().channels.remove(0);
        channel.id = "availability-retired-representative".into();
        channel.models = vec!["claude-3-haiku".into()];
        let result = GroupResult {
            group: "anthropic_closed".into(),
            representative: channel.models[0].clone(),
            upstream_model: catalog_identity(&channel, &channel.models[0]),
            status: "suspended".into(),
            checked_at: 42,
            ..Default::default()
        };
        let mut snapshot = Snapshot {
            fingerprint: fingerprint(&channel),
            ..Default::default()
        };
        snapshot.merge(&[result]);
        store()
            .lock()
            .unwrap()
            .channels
            .insert(channel.id.clone(), snapshot);
        channel.models.clear();
        reconcile_catalog(&channel);
        channel.models = vec!["claude-sonnet-5.5".into()];
        reconcile_catalog(&channel);
        assert!(has_check_snapshot(&channel));
        assert_eq!(
            groups_for_channel(&channel)[0].representative,
            "claude-3-haiku"
        );
        assert!(effective_models(&channel, channel.models.clone()).is_empty());
        assert_eq!(representatives(&channel)[0].1, "claude-sonnet-5.5");
        let mut state = store().lock().unwrap();
        let saved = serde_json::to_vec(&*state).unwrap();
        let mut restored = decode(&saved)
            .unwrap()
            .channels
            .remove(&channel.id)
            .unwrap();
        assert!(!restored.allows("claude-sonnet-5.5"));
        restored.merge(&[GroupResult {
            group: "anthropic_closed".into(),
            representative: channel.models[0].clone(),
            upstream_model: catalog_identity(&channel, &channel.models[0]),
            status: "available".into(),
            ..Default::default()
        }]);
        assert!(restored.allows("claude-sonnet-5.5"));
        state.channels.remove(&channel.id);
    }

    #[test]
    fn openrouter_legacy_short_source_identity_is_compatible_but_namespace_changes_are_not() {
        let mut channel = crate::default_config().channels.remove(0);
        channel.id = "availability-legacy-source".into();
        channel.set_source_driver(crate::source_driver::SourceDriverId::Openrouter);
        channel.models = vec!["gemini-2.5-flash-lite".into()];
        channel.capability_profiles = vec![crate::model::ChannelCapabilityProfile {
            model_pattern: channel.models[0].clone(),
            catalog_metadata: true,
            catalog_source_model: Some(channel.models[0].clone()),
            ..Default::default()
        }];
        for saved_source in ["gemini-2.5-flash-lite", "google/gemini-2.5-flash-lite"] {
            let result = GroupResult {
                group: "google_closed".into(),
                representative: channel.models[0].clone(),
                upstream_model: saved_source.into(),
                status: "suspended".into(),
                ..Default::default()
            };
            let mut snapshot = Snapshot {
                fingerprint: fingerprint(&channel),
                ..Default::default()
            };
            snapshot.merge(&[result]);
            store()
                .lock()
                .unwrap()
                .channels
                .insert(channel.id.clone(), snapshot);
            reconcile_catalog(&channel);
            assert_eq!(groups_for_channel(&channel).len(), 1);
            assert!(effective_models(&channel, channel.models.clone()).is_empty());
        }
        channel.capability_profiles[0].catalog_source_model =
            Some("different/gemini-2.5-flash-lite".into());
        reconcile_catalog(&channel);
        assert!(groups_for_channel(&channel).is_empty());
        store().lock().unwrap().channels.remove(&channel.id);
    }

    #[test]
    fn ui_groups_reject_removed_representatives_and_changed_upstream_mappings() {
        let mut channel = crate::default_config().channels.remove(0);
        channel.id = "availability-ui-catalog".into();
        channel.models = vec!["qwen-mini".into()];
        let result = GroupResult {
            group: "other".into(),
            representative: channel.models[0].clone(),
            upstream_model: catalog_identity(&channel, &channel.models[0]),
            status: "available".into(),
            ..Default::default()
        };
        let mut snapshot = Snapshot {
            fingerprint: fingerprint(&channel),
            ..Default::default()
        };
        snapshot.merge(&[result]);
        store()
            .lock()
            .unwrap()
            .channels
            .insert(channel.id.clone(), snapshot);
        assert_eq!(groups_for_channel(&channel).len(), 1);
        channel.models.clear();
        assert!(groups_for_channel(&channel).is_empty());
        channel.models = vec!["qwen-mini".into()];
        channel
            .capability_profiles
            .push(crate::model::ChannelCapabilityProfile {
                model_pattern: "qwen-mini".into(),
                catalog_metadata: true,
                catalog_source_model: Some("different/qwen-mini".into()),
                ..Default::default()
            });
        assert!(groups_for_channel(&channel).is_empty());
        store().lock().unwrap().channels.remove(&channel.id);
    }

    #[test]
    fn group_identity_is_bounded_and_open_weights_are_other() {
        for model in [
            "google/gemma-4",
            "openai/gpt-oss-120b",
            "unknown/gpt-8",
            "openrouter/auto",
            "qwen3.8-27b",
        ] {
            assert_eq!(group(model), "other");
        }
        assert_eq!(group("CLAUDE-new-model"), "anthropic_closed");
        assert_eq!(group("gemini-future:free"), "google_closed");
        assert_eq!(group("openai/gpt-6-mini"), "openai_closed");
        assert_eq!(group("codex-future-model"), "openai_closed");
    }
    #[test]
    fn admission_retains_source_and_does_not_resurrect_defaults() {
        let mut snapshot = Snapshot::default();
        snapshot.merge(&[GroupResult {
            group: "anthropic_closed".into(),
            status: "suspended".into(),
            ..Default::default()
        }]);
        assert!(!snapshot.allows("claude-opus-future"));
        assert!(snapshot.allows("qwen-future"));
        snapshot.merge(&[GroupResult {
            group: "anthropic_closed".into(),
            status: "deferred".into(),
            ..Default::default()
        }]);
        assert!(!snapshot.allows("claude-opus-future"));
        snapshot.merge(&[GroupResult {
            group: "other".into(),
            status: "suspended".into(),
            ..Default::default()
        }]);
        assert!(!snapshot.allows("gpt-next"));
        snapshot.merge(&[GroupResult {
            group: "other".into(),
            status: "available".into(),
            ..Default::default()
        }]);
        assert!(snapshot.allows("gpt-next"));
        assert!(!snapshot.allows("claude-opus-future"));
    }
    #[test]
    fn stored_evidence_tolerates_future_fields_but_not_future_versions() {
        assert!(decode(br#"{"version":2}"#).is_none());
        assert!(decode(b"broken").is_none());
        let state = decode(br#"{"version":1,"future":true,"channels":{"x":{"groups":{"other":{"status":"new-state"}}}}}"#).unwrap();
        assert!(state.channels["x"].allows("model"));
        let state = decode(br#"{"version":1,"channels":{"x":{"admission_pending":true,"groups":{"other":{"status":"future-state"}}}}}"#).unwrap();
        assert!(state.channels["x"].allows("model"));
    }

    #[test]
    fn offline_recovery_budget_is_shared_and_reserved_before_execution() {
        let mut channel = crate::default_config().channels.remove(0);
        channel.id = "availability-offline-budget".into();
        assert_eq!(reserve_offline_recovery_budget(&channel), 8);
        assert_eq!(reserve_offline_recovery_budget(&channel), 0);
        assert!(allows(&channel, "gpt-luna"));
        let mut state = store().lock().unwrap();
        state
            .channels
            .get_mut(&channel.id)
            .unwrap()
            .automatic_window_at -= 3601;
        drop(state);
        assert_eq!(reserve_offline_recovery_budget(&channel), 8);
    }
    #[test]
    fn representative_prefers_cheap_regular_models_not_the_default_or_auto() {
        let mut channel = crate::default_config().channels.remove(0);
        channel.models = [
            "claude-opus-5",
            "claude-haiku-4-5",
            "gemini-pro",
            "gemini-flash-lite",
            "gpt-large",
            "gpt-mini",
            "auto",
            "openrouter/free",
            "qwen-mini:free",
            "qwen-27b",
            "text-embedding-3",
        ]
        .map(str::to_string)
        .to_vec();
        let selected = representatives(&channel);
        assert_eq!(selected.len(), 4);
        assert!(selected.iter().any(|(_, m)| m == "qwen-27b"));
        assert!(selected.iter().any(|(_, m)| m == "claude-haiku-4-5"));
    }

    #[tokio::test]
    async fn parallel_group_checks_retry_once_project_atomically_and_recover() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        use warp::Filter;
        let calls = Arc::new(AtomicUsize::new(0));
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mode = Arc::new(AtomicUsize::new(0));
        let (captured_calls, captured_active, captured_peak, captured_mode) =
            (calls.clone(), active.clone(), peak.clone(), mode.clone());
        let routes = warp::post().and(warp::body::json()).and_then(move |body: serde_json::Value| {
            let (calls, active, peak, mode) = (captured_calls.clone(), captured_active.clone(), captured_peak.clone(), captured_mode.clone());
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                let in_flight = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(in_flight, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(20)).await;
                active.fetch_sub(1, Ordering::SeqCst);
                let model = body["model"].as_str().unwrap_or("");
                let other = group(model) == "other";
                let mode = mode.load(Ordering::SeqCst);
                let (status, value) = if mode == 0 && !other {
                    (403, serde_json::json!({"error":{"code":403,"message":"The request is prohibited due to a violation of provider Terms Of Service."}}))
                } else if mode == 2 && other {
                    (503, serde_json::json!({"error":{"type":"overloaded_error"}}))
                } else if mode == 3 && other {
                    (429, serde_json::json!({"error":{"code":"usage_not_included","message":"Usage credits are required for this model."}}))
                } else {
                    (200, serde_json::json!({"choices":[{"finish_reason":"stop","message":{"content":"OK"}}],"usage":{"prompt_tokens":8,"completion_tokens":1}}))
                };
                Ok::<_, std::convert::Infallible>(warp::reply::with_status(warp::reply::json(&value), warp::http::StatusCode::from_u16(status).unwrap()))
            }
        });
        let (addr, server) = crate::bind_ephemeral!(routes, ([127, 0, 0, 1], 0));
        let server = tokio::spawn(server);
        let mut channel = crate::channel_from_supplier(
            "availability-mock".into(),
            &crate::default_supplier_config(),
        );
        channel.set_source_driver(crate::source_driver::SourceDriverId::CustomEndpoint);
        channel.surface_bindings = crate::source_driver_surface_bindings(
            channel.source_driver(),
            &format!("http://{addr}"),
        );
        channel.models = [
            "claude-haiku-future",
            "claude-opus-future",
            "gemini-flash-lite",
            "gpt-luna",
            "qwen-27b",
        ]
        .map(str::to_string)
        .to_vec();
        channel.public_model = "claude-opus-future".into();
        channel.upstream_model = channel.public_model.clone();
        channel.v2.max_concurrency = 2;
        let source = channel.models.clone();
        let report = check(&channel).await;
        assert_eq!(report.attempts, 7, "{report:?}");
        assert_eq!(calls.load(Ordering::SeqCst), 7);
        assert_eq!(peak.load(Ordering::SeqCst), 2);
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert_eq!(
            report
                .groups
                .iter()
                .filter(|g| g.status == "suspended")
                .count(),
            3
        );
        assert_eq!(effective_models(&channel, source.clone()), ["qwen-27b"]);
        let supplier = crate::supplier_from_channel(&channel);
        assert_eq!(supplier.models, ["qwen-27b"]);
        assert_eq!(supplier.public_model, "qwen-27b");
        assert_eq!(channel.models, source);
        assert!(
            !recheck_due(&channel, crate::now_unix() + 1801),
            "idle channels do not trigger paid checks"
        );
        crate::upstream_transport::with_probe_budget(0, async {
            note_activity(&channel);
        })
        .await;
        assert!(
            !recheck_due(&channel, crate::now_unix() + 1801),
            "checks are not business demand"
        );
        note_activity(&channel);
        assert!(
            recheck_due(&channel, crate::now_unix() + 1801),
            "active remote supply can recover sibling groups"
        );
        assert!(!allows(&channel, "claude-opus-future"));
        assert!(recheck_due(&channel, crate::now_unix() + 1801));
        {
            let mut state = store().lock().unwrap();
            for group in state
                .channels
                .get_mut(&channel.id)
                .unwrap()
                .groups
                .values_mut()
            {
                if group.status == "suspended" {
                    group.next_probe_at = 0;
                }
            }
        }
        let confirmed = automatic_recheck(check(&channel)).await;
        assert_eq!(confirmed.attempts, 6);
        assert!(
            confirmed
                .groups
                .iter()
                .all(|group| group.automatic_confirmed)
        );
        note_activity(&channel);
        assert!(
            !recheck_due(&channel, crate::now_unix() + 7201),
            "unchanged eligibility restrictions do not trigger repeated paid challenges"
        );
        mode.store(1, Ordering::SeqCst);
        let report = check(&channel).await;
        assert_eq!(report.attempts, 4);
        assert_eq!(effective_models(&channel, source.clone()), source);
        mode.store(2, Ordering::SeqCst);
        let report = check(&channel).await;
        assert_eq!(report.attempts, 5);
        assert!(effective_models(&channel, source.clone()).is_empty());
        let supplier = crate::supplier_from_channel(&channel);
        assert!(supplier.public_model.is_empty() && supplier.upstream_model.is_empty());
        mode.store(3, Ordering::SeqCst);
        let report = check(&channel).await;
        assert_eq!(
            report
                .groups
                .iter()
                .find(|g| g.group == "other")
                .unwrap()
                .status,
            "inconclusive"
        );
        assert!(
            effective_models(&channel, source.clone()).is_empty(),
            "a narrow quota result cannot erase an older outage"
        );
        mode.store(1, Ordering::SeqCst);
        check(&channel).await;
        assert_eq!(effective_models(&channel, source.clone()), source);
        server.abort();
    }

    #[tokio::test]
    async fn cancellation_releases_single_flight_without_writing_failure() {
        let mut channel = crate::channel_from_supplier(
            "availability-cancel".into(),
            &crate::default_supplier_config(),
        );
        channel.models = vec!["gpt-luna".into()];
        {
            store().lock().unwrap().checking.insert(channel.id.clone());
        }
        let guard = CheckGuard(channel.id.clone());
        let report = check(&channel).await;
        assert_eq!(report.groups[0].status, "deferred");
        assert_eq!(report.attempts, 0);
        drop(guard);
        assert!(!store().lock().unwrap().checking.contains(&channel.id));
        assert!(allows(&channel, "gpt-luna"));
    }
}
