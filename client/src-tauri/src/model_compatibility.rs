use crate::config::{commit_config_update, load_config_from_path, normalize_model_name};
use crate::model::{
    AppState, ClientConfig, LocalModelCompatibilityProfile, ModelCompatibilityCatalogModel,
    ModelCompatibilityGroupConfig,
};
use anyhow::{Context, Result, anyhow};
use reqwest::{
    Method, StatusCode,
    header::{ETAG, HeaderValue, IF_NONE_MATCH},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tauri::State;

const ACCOUNT_MODEL_COMPATIBILITY_PATH: &str = "/api/account/model-compatibility";
const LOCAL_MODEL_COMPATIBILITY_PLATFORM_ID: &str = "__local__";
const LOCAL_MODEL_COMPATIBILITY_USER_ID: &str = "__device__";

#[cfg(test)]
#[path = "model_compatibility/http_cache_tests.rs"]
mod http_cache_tests;

#[derive(Default)]
pub(crate) struct ModelCompatibilityHttpCache {
    // Per-runtime, memory-only, scoped to endpoint, account, credential and
    // route policy. Never persist private HTTP responses or validators.
    active: Option<(
        CompatibilityHttpScope,
        Arc<tokio::sync::Mutex<CompatibilityHttpDocument>>,
    )>,
}

type CompatibilityHttpScope = (String, String, String, [u8; 32]);

#[derive(Default)]
struct CompatibilityHttpDocument {
    value: Option<Value>,
    etag: Option<HeaderValue>,
    checked_at: Option<Instant>,
    last_error: Option<RemoteModelCompatibilityError>,
}

impl CompatibilityHttpDocument {
    fn invalidate(&mut self) {
        self.value = None;
        self.etag = None;
        self.checked_at = None;
        self.last_error = None;
    }
}

#[derive(Debug, Clone)]
struct RemoteModelCompatibilityError {
    status: Option<u16>,
    detail: String,
}

impl std::fmt::Display for RemoteModelCompatibilityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct SaveModelCompatibilityInput {
    pub(crate) base_release_id: String,
    #[serde(default)]
    pub(crate) revision: i64,
    pub(crate) model_groups: Vec<ModelCompatibilityGroupConfig>,
    #[serde(default)]
    pub(crate) template_model_groups: Vec<ModelCompatibilityGroupConfig>,
    #[serde(default)]
    pub(crate) catalog_models: Vec<ModelCompatibilityCatalogModel>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ResetModelCompatibilityInput {
    pub(crate) base_release_id: String,
    #[serde(default)]
    pub(crate) revision: i64,
    pub(crate) template_model_groups: Vec<ModelCompatibilityGroupConfig>,
    #[serde(default)]
    pub(crate) catalog_models: Vec<ModelCompatibilityCatalogModel>,
}

#[derive(Debug, Clone, Serialize)]
struct ServerModelCompatibilityGroupInput {
    id: String,
    disabled: bool,
    models: Vec<String>,
}

pub(crate) fn model_compatibility_profile_key(platform_id: &str, user_id: &str) -> Option<String> {
    let platform_id = platform_id.trim();
    let user_id = user_id.trim();
    if platform_id.is_empty() || user_id.is_empty() {
        return None;
    }
    Some(format!("{platform_id}::{user_id}"))
}

pub(crate) fn active_model_compatibility_profile(
    config: &ClientConfig,
) -> Option<&LocalModelCompatibilityProfile> {
    let key = model_compatibility_profile_key(
        if config.account_platform_id.trim().is_empty() {
            &config.platform_id
        } else {
            &config.account_platform_id
        },
        &config.account_user_id,
    )?;
    config.model_compatibility_profiles.get(&key)
}

fn local_model_compatibility_cache_key() -> String {
    model_compatibility_profile_key(
        LOCAL_MODEL_COMPATIBILITY_PLATFORM_ID,
        LOCAL_MODEL_COMPATIBILITY_USER_ID,
    )
    .unwrap_or_else(|| {
        format!("{LOCAL_MODEL_COMPATIBILITY_PLATFORM_ID}::{LOCAL_MODEL_COMPATIBILITY_USER_ID}")
    })
}

fn is_local_model_compatibility_cache(profile: &LocalModelCompatibilityProfile) -> bool {
    profile.platform_id == LOCAL_MODEL_COMPATIBILITY_PLATFORM_ID
        && profile.user_id == LOCAL_MODEL_COMPATIBILITY_USER_ID
}

fn cached_local_model_compatibility_profile(
    config: &ClientConfig,
) -> Option<&LocalModelCompatibilityProfile> {
    config
        .model_compatibility_profiles
        .get(&local_model_compatibility_cache_key())
        .filter(|profile| !profile.model_groups.is_empty())
        .or_else(|| {
            active_model_compatibility_profile(config)
                .filter(|profile| !profile.model_groups.is_empty())
        })
        .or_else(|| {
            config
                .model_compatibility_profiles
                .values()
                .filter(|profile| !profile.model_groups.is_empty())
                .max_by_key(|profile| profile.updated_at_unix_ms)
        })
}

fn effective_local_model_compatibility_profile(
    config: &ClientConfig,
) -> LocalModelCompatibilityProfile {
    let Some(profile) = cached_local_model_compatibility_profile(config).cloned() else {
        return client_default_model_compatibility_profile();
    };
    if is_authoritative_profile_snapshot(&profile) {
        // Complete server/generated profiles are snapshots, while the active
        // signed public or same-source packaged document is always the catalog
        // authority. Carry forward only explicit user additions so a former
        // server snapshot cannot survive as a second baseline.
        return rebase_profile_onto_active_defaults(profile);
    }
    // Incomplete profiles are intentionally supported for local-only and test
    // configurations. They describe their own groups and must not be mistaken
    // for a generated template snapshot.
    profile
}

fn is_authoritative_profile_snapshot(profile: &LocalModelCompatibilityProfile) -> bool {
    if profile.template_model_groups.is_empty() || profile.catalog_models.is_empty() {
        return false;
    }
    !profile.compatibility_version_id.trim().is_empty()
        || profile.base_release_id.starts_with("client-compat-")
        || profile.base_release_id.starts_with("catalog-")
}

fn client_default_model_compatibility_profile() -> LocalModelCompatibilityProfile {
    // The generated resource is built from the server catalog. This keeps the
    // offline baseline, group order, aliases, and match-only routes on the same
    // release without maintaining a second handwritten list.
    let groups = crate::tool_model_metadata::active_tool_compatibility_groups()
        .into_iter()
        .map(|group| ModelCompatibilityGroupConfig {
            id: group.id,
            label: group.label,
            description: group.description,
            aliases: group.aliases,
            models: group.models,
            match_models: group.match_models,
            disabled: false,
        })
        .collect::<Vec<_>>();
    let mut catalog_models = crate::tool_model_metadata::active_tool_catalog_models()
        .into_iter()
        .map(|model| ModelCompatibilityCatalogModel {
            id: model.id,
            display_name: model.display_name,
            vendor: model.vendor,
            family: model.family,
            context_tokens: model.context_tokens,
            output_tokens: model.output_tokens,
            input_modalities: model.input_modalities,
            output_modalities: model.output_modalities,
            reasoning: model.reasoning,
            tool_call: model.tool_call,
        })
        .collect::<Vec<_>>();
    let mut catalog_ids = catalog_models
        .iter()
        .map(|model| normalize_model_name(&model.id))
        .collect::<HashSet<_>>();
    for model in groups.iter().flat_map(|group| group.models.iter()) {
        let key = normalize_model_name(model);
        if key.is_empty() || !catalog_ids.insert(key) {
            continue;
        }
        catalog_models.push(ModelCompatibilityCatalogModel {
            id: model.clone(),
            display_name: model.clone(),
            vendor: String::new(),
            ..Default::default()
        });
    }
    LocalModelCompatibilityProfile {
        platform_id: LOCAL_MODEL_COMPATIBILITY_PLATFORM_ID.to_string(),
        user_id: LOCAL_MODEL_COMPATIBILITY_USER_ID.to_string(),
        base_release_id: crate::tool_model_metadata::active_tool_compatibility_release_id(),
        model_version_id: active_model_catalog_version().model_version_id,
        compatibility_version_id: active_tool_compatibility_version(),
        revision: 0,
        model_groups: groups.clone(),
        template_model_groups: groups,
        catalog_models,
        sync_status: "local".to_string(),
        customized: false,
        pending_reset: false,
        updated_at_unix_ms: 0,
    }
}

fn active_tool_compatibility_version() -> String {
    crate::tool_model_metadata::active_tool_compatibility_version_id()
}

fn active_model_catalog_version() -> crate::tool_model_metadata::ModelCatalogVersionInfo {
    crate::tool_model_metadata::active_model_catalog_version()
}

fn rebase_profile_onto_active_defaults(
    profile: LocalModelCompatibilityProfile,
) -> LocalModelCompatibilityProfile {
    let mut current = client_default_model_compatibility_profile();
    if !profile.customized {
        current.platform_id = profile.platform_id;
        current.user_id = profile.user_id;
        current.revision = profile.revision;
        current.sync_status = profile.sync_status;
        current.pending_reset = profile.pending_reset;
        current.updated_at_unix_ms = profile.updated_at_unix_ms;
        return current;
    }

    // A local profile used to persist a complete snapshot. Treat every model
    // from its saved template (and every current default) as system-owned, then
    // carry forward only the remaining user additions. This gives old profiles
    // the new immutable defaults without any per-user migration.
    let system_models = profile
        .template_model_groups
        .iter()
        .chain(current.template_model_groups.iter())
        .flat_map(|group| group.models.iter().chain(group.match_models.iter()))
        .map(|model| normalize_model_name(model))
        .filter(|model| !model.is_empty())
        .collect::<HashSet<_>>();
    let mut additions_by_group = HashMap::<String, Vec<String>>::new();
    let mut seen_additions = HashSet::new();
    for group in &profile.model_groups {
        let group_key = normalize_model_name(&group.id);
        if group_key.is_empty() {
            continue;
        }
        for model in &group.models {
            let model = model.trim();
            let key = normalize_model_name(model);
            if key.is_empty() || system_models.contains(&key) || !seen_additions.insert(key) {
                continue;
            }
            additions_by_group
                .entry(group_key.clone())
                .or_default()
                .push(model.to_string());
        }
    }
    for group in &mut current.model_groups {
        if let Some(additions) = additions_by_group.remove(&normalize_model_name(&group.id)) {
            group.models.extend(additions);
        }
        group.disabled = false;
    }

    let mut catalog_models = current.catalog_models;
    let mut catalog_ids = catalog_models
        .iter()
        .map(|model| normalize_model_name(&model.id))
        .collect::<HashSet<_>>();
    for model in profile.catalog_models {
        let key = normalize_model_name(&model.id);
        if key.is_empty() || !catalog_ids.insert(key) {
            continue;
        }
        catalog_models.push(model);
    }

    current.platform_id = profile.platform_id;
    current.user_id = profile.user_id;
    current.revision = profile.revision;
    current.catalog_models = catalog_models;
    current.sync_status = profile.sync_status;
    current.customized = true;
    current.pending_reset = profile.pending_reset;
    current.updated_at_unix_ms = profile.updated_at_unix_ms;
    current
}

pub(crate) fn model_compatibility_candidates(
    config: &ClientConfig,
    requested_model: &str,
) -> Vec<String> {
    if !config.allow_model_equivalence {
        return Vec::new();
    }
    let profile = effective_local_model_compatibility_profile(config);
    let requested_key = normalize_model_name(crate::config::without_context_hint(requested_model));
    if requested_key.is_empty() {
        return Vec::new();
    }
    let assigned_group = profile.model_groups.iter().position(|group| {
        !group.disabled
            && group
                .models
                .iter()
                .any(|model| normalize_model_name(model) == requested_key)
    });
    let matched_group = assigned_group.or_else(|| {
        profile.model_groups.iter().position(|group| {
            !group.disabled
                && (normalize_model_name(&group.id) == requested_key
                    || group
                        .aliases
                        .iter()
                        .any(|alias| normalize_model_name(alias) == requested_key)
                    || group
                        .match_models
                        .iter()
                        .any(|model| normalize_model_name(model) == requested_key))
        })
    });
    if let Some(group_index) = matched_group {
        return compatibility_group_candidate_chain(&profile.model_groups, group_index)
            .into_iter()
            .filter_map(|model| {
                if normalize_model_name(&model) == requested_key {
                    return None;
                }
                Some(model)
            })
            .collect();
    }
    Vec::new()
}

fn compatibility_group_candidate_chain(
    groups: &[ModelCompatibilityGroupConfig],
    group_index: usize,
) -> Vec<String> {
    if group_index >= groups.len() {
        return Vec::new();
    }
    let mut seen = HashSet::new();
    // Profiles are ordered highest to lowest. Walk the requested group first,
    // then each preceding (higher) group; never fall downward.
    groups[..=group_index]
        .iter()
        .rev()
        .filter(|group| !group.disabled)
        .flat_map(|group| group.models.iter())
        .filter_map(|model| {
            let model = model.trim();
            let key = normalize_model_name(model);
            if key.is_empty() || !seen.insert(key) {
                return None;
            }
            Some(model.to_string())
        })
        .collect()
}

pub(crate) fn anthropic_model_routes(
    config: &ClientConfig,
    available_models: &[String],
) -> Vec<String> {
    let mut available_by_key = HashMap::new();
    let mut available_in_order = Vec::new();
    for model in available_models {
        let model = model.trim();
        let key = normalize_model_name(model);
        if key.is_empty() || available_by_key.contains_key(&key) {
            continue;
        }
        available_by_key.insert(key, model.to_string());
        available_in_order.push(model.to_string());
    }

    let mut routes = Vec::new();
    let mut seen_routes = HashSet::new();
    if config.allow_model_equivalence {
        let profile = effective_local_model_compatibility_profile(config);
        for (group_index, group) in profile.model_groups.iter().enumerate() {
            if group.disabled {
                continue;
            }
            // Claude-facing tools can only select Anthropic model names. A
            // route is therefore publishable when its own group or any higher
            // compatible group has live supply, even if that supply is backed
            // by a non-Anthropic model. Keep this projection confined to the
            // Anthropic surface; the ordinary OpenAI model catalog must remain
            // a list of actual live models for tools that persist it verbatim.
            let group_is_routable =
                compatibility_group_candidate_chain(&profile.model_groups, group_index)
                    .iter()
                    .any(|model| available_by_key.contains_key(&normalize_model_name(model)));
            if !group_is_routable {
                continue;
            }
            for route_name in group
                .models
                .iter()
                .map(|model| model.trim())
                .filter(|model| is_anthropic_gateway_model_route(model))
            {
                let route_key = normalize_model_name(route_name);
                if route_key.is_empty() || !seen_routes.insert(route_key) {
                    continue;
                }
                routes.push(route_name.to_string());
            }
        }
    } else {
        // Disabling equivalence deliberately disables compatibility groups. In
        // that mode expose only currently advertised native Anthropic routes.
        for model in available_in_order {
            if !is_anthropic_gateway_model_route(&model) {
                continue;
            }
            let route_key = normalize_model_name(&model);
            if route_key.is_empty() || !seen_routes.insert(route_key) {
                continue;
            }
            routes.push(model);
        }
    }
    routes
}

fn is_anthropic_gateway_model_route(model: &str) -> bool {
    let model = normalize_model_name(model);
    model.starts_with("claude-")
        || model.starts_with("anthropic/")
        || model.starts_with("anthropic.")
        || model.contains("/anthropic.")
        || model.contains(".anthropic.")
}

#[tauri::command]
pub(crate) async fn fetch_model_compatibility(
    state: State<'_, AppState>,
) -> std::result::Result<Value, String> {
    fetch_model_compatibility_inner(&state)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) async fn save_model_compatibility(
    state: State<'_, AppState>,
    input: SaveModelCompatibilityInput,
) -> std::result::Result<Value, String> {
    save_model_compatibility_inner(&state, input)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) async fn reset_model_compatibility(
    state: State<'_, AppState>,
    input: ResetModelCompatibilityInput,
) -> std::result::Result<Value, String> {
    reset_model_compatibility_inner(&state, input)
        .await
        .map_err(|error| error.to_string())
}

pub(crate) async fn sync_model_compatibility_for_runtime(state: &AppState) -> Result<()> {
    let Ok(config) = platform_authorized_config(state) else {
        return Ok(());
    };
    if current_account_scope(&config).is_none() {
        return Ok(());
    }
    let _ = fetch_model_compatibility_inner(state).await?;
    Ok(())
}

async fn fetch_model_compatibility_inner(state: &AppState) -> Result<Value> {
    let config = match platform_authorized_config(state) {
        Ok(config) if current_account_scope(&config).is_some() => config,
        _ => {
            let config = load_config_from_path(&state.config_path)?;
            let profile = effective_local_model_compatibility_profile(&config);
            return Ok(payload_from_profile(state, None, &profile, None));
        }
    };
    let local = active_model_compatibility_profile(&config).cloned();
    let pending_device = config
        .model_compatibility_profiles
        .get(&local_model_compatibility_cache_key())
        .filter(|profile| profile.customized && profile.sync_status == "pending")
        .cloned();

    if let Some(profile) = local
        .as_ref()
        .filter(|profile| profile.sync_status == "pending")
        .filter(|profile| {
            pending_device
                .as_ref()
                .map(|device| device.updated_at_unix_ms <= profile.updated_at_unix_ms)
                .unwrap_or(true)
        })
    {
        match sync_pending_profile(state, &config, profile).await {
            Ok(payload) => return Ok(payload),
            Err(error) if error.status == Some(409) => {
                return conflict_payload(state, &config, profile, error.detail).await;
            }
            Err(error) if remote_authorization_failed(&error) => {
                return fallback_after_remote_authorization_failure(state, Some(profile));
            }
            Err(error) => {
                return Ok(payload_from_profile(
                    state,
                    None,
                    profile,
                    Some(error.detail),
                ));
            }
        }
    }

    let remote = match request_account_model_compatibility(
        &config,
        Method::GET,
        None,
        None,
        &state.model_compatibility_http_cache,
    )
    .await
    {
        Ok(remote) => remote,
        Err(error) if remote_authorization_failed(&error) => {
            return fallback_after_remote_authorization_failure(state, local.as_ref());
        }
        Err(error) => {
            if let Some(profile) = local.as_ref() {
                return Ok(payload_from_profile(
                    state,
                    None,
                    profile,
                    Some(error.detail),
                ));
            }
            return Err(anyhow!(error.detail));
        }
    };
    if let Some(profile) = local
        .as_ref()
        .filter(|profile| profile.sync_status == "conflict")
    {
        let conflict = rebase_conflict_profile(&config, profile, &remote)?;
        persist_local_model_compatibility(state, conflict.clone())?;
        return Ok(payload_from_profile(state, Some(remote), &conflict, None));
    }
    let remote_profile = profile_from_remote(&config, &remote, "synced", false)?;
    if !remote_profile.customized {
        if let Some(pending_local) = pending_device {
            let pending = match promote_local_profile_to_account(
                &config,
                &pending_local,
                &remote_profile,
            ) {
                Ok(pending) => pending,
                Err(error) => {
                    return Ok(payload_from_profile(
                        state,
                        Some(remote),
                        &pending_local,
                        Some(format!(
                            "Local customizations remain active but were not synced because they conflict with the server template: {error}"
                        )),
                    ));
                }
            };
            persist_local_model_compatibility(state, pending.clone())?;
            return match sync_pending_profile(state, &config, &pending).await {
                Ok(payload) => Ok(payload),
                Err(error) if error.status == Some(409) => {
                    conflict_payload(state, &config, &pending, error.detail).await
                }
                Err(error) => Ok(payload_from_profile(
                    state,
                    Some(remote),
                    &pending,
                    Some(error.detail),
                )),
            };
        }
    }
    persist_local_model_compatibility(state, remote_profile.clone())?;
    Ok(payload_from_profile(
        state,
        Some(remote),
        &remote_profile,
        None,
    ))
}

async fn save_model_compatibility_inner(
    state: &AppState,
    input: SaveModelCompatibilityInput,
) -> Result<Value> {
    let authorized = platform_authorized_config(state)
        .ok()
        .filter(|config| current_account_scope(config).is_some());
    let model_groups = normalize_local_model_groups(
        input.model_groups,
        &input.template_model_groups,
        &input.catalog_models,
    )?;
    let (platform_id, user_id) = authorized
        .as_ref()
        .and_then(current_account_scope)
        .unwrap_or_else(|| {
            (
                LOCAL_MODEL_COMPATIBILITY_PLATFORM_ID.to_string(),
                LOCAL_MODEL_COMPATIBILITY_USER_ID.to_string(),
            )
        });
    let pending = LocalModelCompatibilityProfile {
        platform_id,
        user_id,
        base_release_id: input.base_release_id.trim().to_string(),
        model_version_id: active_model_catalog_version().model_version_id,
        compatibility_version_id: active_tool_compatibility_version(),
        revision: input.revision.max(0),
        model_groups,
        template_model_groups: normalize_template_groups(input.template_model_groups),
        catalog_models: normalize_catalog_models(input.catalog_models),
        sync_status: "pending".to_string(),
        customized: true,
        pending_reset: false,
        updated_at_unix_ms: unix_time_millis(),
    };
    if pending.base_release_id.is_empty() {
        return Err(anyhow!("compatibility template release is missing"));
    }
    persist_local_model_compatibility(state, pending.clone())?;
    let Some(config) = authorized else {
        return Ok(payload_from_profile(state, None, &pending, None));
    };
    match sync_pending_profile(state, &config, &pending).await {
        Ok(payload) => Ok(payload),
        Err(error) if remote_authorization_failed(&error) => {
            fallback_after_remote_authorization_failure(state, Some(&pending))
        }
        Err(error) => {
            let local = pending;
            if error.status == Some(409) {
                return conflict_payload(state, &config, &local, error.detail).await;
            }
            Ok(payload_from_profile(
                state,
                None,
                &local,
                Some(error.detail),
            ))
        }
    }
}

async fn reset_model_compatibility_inner(
    state: &AppState,
    input: ResetModelCompatibilityInput,
) -> Result<Value> {
    let authorized = platform_authorized_config(state)
        .ok()
        .filter(|config| current_account_scope(config).is_some());
    if authorized.is_none() {
        // A logged-out/local reset is a reset to the client contract, not to a
        // stale server template echoed by the renderer.
        let mut fallback = client_default_model_compatibility_profile();
        fallback.updated_at_unix_ms = unix_time_millis();
        persist_local_model_compatibility(state, fallback.clone())?;
        return Ok(payload_from_profile(state, None, &fallback, None));
    }
    let template_model_groups = normalize_template_groups(input.template_model_groups);
    if template_model_groups.is_empty() {
        return Err(anyhow!(
            "compatibility template is unavailable; refresh before resetting"
        ));
    }
    let (platform_id, user_id) = authorized
        .as_ref()
        .and_then(current_account_scope)
        .context("authorized compatibility reset has no account scope")?;
    let pending = LocalModelCompatibilityProfile {
        platform_id,
        user_id,
        base_release_id: input.base_release_id.trim().to_string(),
        model_version_id: active_model_catalog_version().model_version_id,
        compatibility_version_id: active_tool_compatibility_version(),
        revision: input.revision.max(0),
        model_groups: template_model_groups.clone(),
        template_model_groups,
        catalog_models: normalize_catalog_models(input.catalog_models),
        sync_status: "pending".to_string(),
        customized: false,
        pending_reset: true,
        updated_at_unix_ms: unix_time_millis(),
    };
    persist_local_model_compatibility(state, pending.clone())?;
    let Some(config) = authorized else {
        return Ok(payload_from_profile(state, None, &pending, None));
    };
    match sync_pending_profile(state, &config, &pending).await {
        Ok(payload) => Ok(payload),
        Err(error) if remote_authorization_failed(&error) => {
            fallback_after_remote_authorization_failure(state, Some(&pending))
        }
        Err(error) => {
            let local = pending;
            if error.status == Some(409) {
                return conflict_payload(state, &config, &local, error.detail).await;
            }
            Ok(payload_from_profile(
                state,
                None,
                &local,
                Some(error.detail),
            ))
        }
    }
}

fn remote_authorization_failed(error: &RemoteModelCompatibilityError) -> bool {
    matches!(error.status, Some(401) | Some(403))
}

fn fallback_after_remote_authorization_failure(
    state: &AppState,
    previous: Option<&LocalModelCompatibilityProfile>,
) -> Result<Value> {
    // A non-empty but rejected device key is not an active platform session.
    // Drop only the runtime authorization signal; the account refresh flow
    // remains responsible for renewing or expiring persisted credentials.
    if let Ok(mut key) = state.platform_api_key.lock() {
        key.clear();
    }

    // Tool metadata always remains on the signed public (or same-source
    // packaged LKG) component. The server is only an account preference and
    // availability source, so losing authorization cannot switch catalogs.
    let mut fallback = previous
        .cloned()
        .map(|profile| {
            if is_authoritative_profile_snapshot(&profile) {
                rebase_profile_onto_active_defaults(profile)
            } else {
                profile
            }
        })
        .unwrap_or_else(client_default_model_compatibility_profile);
    if fallback.model_groups.is_empty() || fallback.template_model_groups.is_empty() {
        fallback = client_default_model_compatibility_profile();
    }
    fallback.platform_id = LOCAL_MODEL_COMPATIBILITY_PLATFORM_ID.to_string();
    fallback.user_id = LOCAL_MODEL_COMPATIBILITY_USER_ID.to_string();
    fallback.sync_status = if fallback.customized {
        "pending".to_string()
    } else {
        "local".to_string()
    };
    fallback.pending_reset = false;
    fallback.updated_at_unix_ms = unix_time_millis();
    persist_local_model_compatibility(state, fallback.clone())?;

    // Do not echo a raw server phrase such as "Invalid API key" into this
    // editor. The local-policy state already communicates that synchronization
    // is unavailable, while model editing and routing keep using current data.
    Ok(payload_from_profile(state, None, &fallback, None))
}

async fn sync_pending_profile(
    state: &AppState,
    config: &ClientConfig,
    profile: &LocalModelCompatibilityProfile,
) -> std::result::Result<Value, RemoteModelCompatibilityError> {
    let response = if profile.pending_reset {
        request_account_model_compatibility(
            config,
            Method::DELETE,
            None,
            Some(profile.revision),
            &state.model_compatibility_http_cache,
        )
        .await?
    } else {
        let groups = profile
            .model_groups
            .iter()
            .map(|group| ServerModelCompatibilityGroupInput {
                id: group.id.clone(),
                disabled: group.disabled,
                models: group.models.clone(),
            })
            .collect::<Vec<_>>();
        request_account_model_compatibility(
            config,
            Method::PUT,
            Some(json!({
                "base_release_id": profile.base_release_id,
                "revision": profile.revision,
                "model_groups": groups,
            })),
            None,
            &state.model_compatibility_http_cache,
        )
        .await?
    };
    let synced = profile_from_remote(config, &response, "synced", false).map_err(|error| {
        RemoteModelCompatibilityError {
            status: None,
            detail: error.to_string(),
        }
    })?;
    persist_local_model_compatibility(state, synced.clone()).map_err(|error| {
        RemoteModelCompatibilityError {
            status: None,
            detail: error.to_string(),
        }
    })?;
    Ok(payload_from_profile(state, Some(response), &synced, None))
}

async fn request_account_model_compatibility(
    config: &ClientConfig,
    method: Method,
    body: Option<Value>,
    revision: Option<i64>,
    cache: &StdMutex<ModelCompatibilityHttpCache>,
) -> std::result::Result<Value, RemoteModelCompatibilityError> {
    let suffix = revision
        .map(|revision| format!("?revision={}", revision.max(0)))
        .unwrap_or_default();
    let required_platform = if config.account_platform_id.trim().is_empty() {
        config.platform_id.trim()
    } else {
        config.account_platform_id.trim()
    };
    let (_, endpoint) = crate::account::select_account_endpoint(config, Some(required_platform))
        .map_err(|error| RemoteModelCompatibilityError {
            status: None,
            detail: error.to_string(),
        })?;
    let resource_url = format!(
        "{}{}",
        endpoint.base_url.trim_end_matches('/'),
        ACCOUNT_MODEL_COMPATIBILITY_PATH,
    );
    let url = format!("{resource_url}{suffix}");
    let started_at = Instant::now();
    let scope = (
        resource_url,
        required_platform.to_string(),
        config.account_user_id.trim().to_string(),
        Sha256::digest(config.account_device_api_key.trim().as_bytes()).into(),
    );
    let slot = {
        let mut cache = cache.lock().unwrap_or_else(|error| error.into_inner());
        if cache
            .active
            .as_ref()
            .is_none_or(|(active, _)| active != &scope)
        {
            cache.active = Some((scope, Arc::new(tokio::sync::Mutex::new(Default::default()))));
        }
        // Switching accounts/endpoints never waits for the old account's HTTP
        // operation, and a late old response cannot populate the new cache.
        cache.active.as_ref().map(|(_, slot)| slot.clone())
    }
    .ok_or_else(|| RemoteModelCompatibilityError {
        status: None,
        detail: "model compatibility cache scope unavailable".to_string(),
    })?;
    let mut cache = slot.lock().await;
    let is_read = method == Method::GET;
    if !is_read {
        // Even a rejected or uncertain write may accompany a conflict/change.
        // Never satisfy subsequent reads from the pre-write document.
        cache.invalidate();
    } else if cache
        .checked_at
        .is_some_and(|checked| checked >= started_at)
    {
        if let Some(error) = &cache.last_error {
            return Err(error.clone());
        }
        if let Some(value) = &cache.value {
            return Ok(value.clone());
        }
    }
    let result = send_model_compatibility_request(config, method, body, &url, &mut cache).await;
    if is_read {
        // Coalesce overlapping failures too, avoiding a queue of identical
        // timeout/retry requests. A later, new request still revalidates.
        cache.checked_at = Some(Instant::now());
        cache.last_error = result.as_ref().err().cloned();
    }
    result
}

async fn send_model_compatibility_request(
    config: &ClientConfig,
    method: Method,
    body: Option<Value>,
    url: &str,
    cache: &mut CompatibilityHttpDocument,
) -> std::result::Result<Value, RemoteModelCompatibilityError> {
    let is_read = method == Method::GET;
    for attempt in 0..2 {
        let mut request = crate::short_http_client()
            .request(method.clone(), url)
            .bearer_auth(config.account_device_api_key.trim());
        // Request the complete fallback-capable model list from older servers too.
        request = request.header(crate::ALLOW_UNVERIFIED_PLATFORM_ROUTES_HEADER, "1");
        if is_read {
            if let Some(etag) = cache.etag.as_ref().filter(|_| cache.value.is_some()) {
                request = request.header(IF_NONE_MATCH, etag.clone());
            }
        }
        if let Some(body) = &body {
            request = request
                .header("Content-Type", "application/json")
                .json(body);
        }
        let response = request
            .send()
            .await
            .map_err(|error| RemoteModelCompatibilityError {
                status: None,
                detail: format!("model compatibility sync failed: {error}"),
            })?;
        let status = response.status();
        if is_read && status == StatusCode::NOT_MODIFIED {
            let matches = cache.etag.as_ref().is_some_and(|etag| {
                response
                    .headers()
                    .get(ETAG)
                    .is_none_or(|returned| returned == etag)
            });
            if matches {
                if let Some(value) = cache.value.as_mut() {
                    if let Some(observed) = response
                        .headers()
                        .get("x-const-availability-as-of")
                        .and_then(|value| value.to_str().ok())
                        .filter(|value| value.len() <= 128)
                    {
                        value["availability_as_of"] = Value::String(observed.to_string());
                    }
                    let value = value.clone();
                    cache.checked_at = Some(Instant::now());
                    return Ok(value);
                }
            }
            cache.invalidate();
            if attempt == 0 {
                continue;
            }
        }
        let etag = response.headers().get(ETAG).cloned();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            cache.invalidate();
        }
        let raw = response
            .text()
            .await
            .map_err(|error| RemoteModelCompatibilityError {
                status: Some(status.as_u16()),
                detail: format!("read model compatibility response: {error}"),
            })?;
        if !status.is_success() {
            let detail = serde_json::from_str::<Value>(&raw)
                .ok()
                .and_then(|value| {
                    value
                        .get("error")
                        .and_then(Value::as_str)
                        .or_else(|| value.pointer("/error/message").and_then(Value::as_str))
                        .or_else(|| value.get("message").and_then(Value::as_str))
                        .map(str::to_string)
                })
                .unwrap_or_else(|| raw.trim().chars().take(240).collect());
            return Err(RemoteModelCompatibilityError {
                status: Some(status.as_u16()),
                detail: if detail.is_empty() {
                    format!("model compatibility server returned HTTP {status}")
                } else {
                    detail
                },
            });
        }
        let value: Value =
            serde_json::from_str(&raw).map_err(|error| RemoteModelCompatibilityError {
                status: Some(status.as_u16()),
                detail: format!("invalid model compatibility response: {error}"),
            })?;
        if is_read {
            cache.invalidate();
            if value.is_object() && raw.len() <= 4 * 1024 * 1024 {
                cache.value = Some(value.clone());
                cache.etag = etag;
                cache.checked_at = Some(Instant::now());
            }
        }
        return Ok(value);
    }
    Err(RemoteModelCompatibilityError {
        status: Some(304),
        detail: "model compatibility returned 304 without a valid cache".to_string(),
    })
}

fn profile_from_remote(
    config: &ClientConfig,
    payload: &Value,
    sync_status: &str,
    pending_reset: bool,
) -> Result<LocalModelCompatibilityProfile> {
    let (platform_id, user_id) = current_account_scope(config)
        .context("model compatibility response has no active account")?;
    let model_groups = payload_array(payload, "model_groups")?;
    let template_model_groups = payload_array(payload, "template_model_groups")?;
    let catalog_models = serde_json::from_value(
        payload
            .get("catalog_models")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
    )
    .context("decode model compatibility catalog")?;
    let profile = LocalModelCompatibilityProfile {
        platform_id,
        user_id,
        base_release_id: payload
            .get("release_id")
            .and_then(Value::as_str)
            .or_else(|| payload.get("base_release_id").and_then(Value::as_str))
            .unwrap_or_default()
            .trim()
            .to_string(),
        model_version_id: payload
            .get("model_version_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string(),
        compatibility_version_id: payload
            .get("compatibility_version_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string(),
        revision: payload
            .get("revision")
            .and_then(Value::as_i64)
            .unwrap_or_default()
            .max(0),
        model_groups,
        template_model_groups,
        catalog_models,
        sync_status: sync_status.to_string(),
        customized: payload
            .get("customized")
            .and_then(Value::as_bool)
            .unwrap_or_else(|| payload.get("source").and_then(Value::as_str) == Some("user")),
        pending_reset,
        updated_at_unix_ms: unix_time_millis(),
    };
    // The server owns the user's additions and live availability, not a
    // second copy of the system catalog. Rebase every complete response onto
    // the active signed/packaged component so an older server release can
    // never replace the compatibility groups currently published to clients.
    Ok(rebase_profile_onto_active_defaults(profile))
}

fn promote_local_profile_to_account(
    config: &ClientConfig,
    local: &LocalModelCompatibilityProfile,
    remote: &LocalModelCompatibilityProfile,
) -> Result<LocalModelCompatibilityProfile> {
    let (platform_id, user_id) = current_account_scope(config)
        .context("model compatibility promotion has no active account")?;
    let catalog_models =
        merge_compatibility_catalogs(remote.catalog_models.clone(), local.catalog_models.clone());
    let model_groups = rebase_model_groups_to_template(
        &local.model_groups,
        &local.template_model_groups,
        &remote.template_model_groups,
        &catalog_models,
    )?;
    Ok(LocalModelCompatibilityProfile {
        platform_id,
        user_id,
        base_release_id: remote.base_release_id.clone(),
        model_version_id: remote.model_version_id.clone(),
        compatibility_version_id: remote.compatibility_version_id.clone(),
        revision: remote.revision,
        model_groups,
        template_model_groups: remote.template_model_groups.clone(),
        catalog_models,
        sync_status: "pending".to_string(),
        customized: true,
        pending_reset: false,
        updated_at_unix_ms: unix_time_millis(),
    })
}

async fn conflict_payload(
    state: &AppState,
    config: &ClientConfig,
    profile: &LocalModelCompatibilityProfile,
    detail: String,
) -> Result<Value> {
    let remote = request_account_model_compatibility(
        config,
        Method::GET,
        None,
        None,
        &state.model_compatibility_http_cache,
    )
    .await
    .ok();
    if let Some(payload) = remote.as_ref() {
        let synced = profile_from_remote(config, payload, "synced", false)?;
        let change_already_applied = if profile.pending_reset {
            !synced.customized
        } else {
            synced.customized
                && model_group_preferences_equal(&profile.model_groups, &synced.model_groups)
        };
        if change_already_applied {
            persist_local_model_compatibility(state, synced.clone())?;
            return Ok(payload_from_profile(state, remote, &synced, None));
        }
    }
    let mut conflict = match remote.as_ref() {
        Some(payload) => rebase_conflict_profile(config, profile, payload)?,
        None => profile.clone(),
    };
    conflict.sync_status = "conflict".to_string();
    persist_local_model_compatibility(state, conflict.clone())?;
    Ok(payload_from_profile(state, remote, &conflict, Some(detail)))
}

fn model_group_preferences_equal(
    left: &[ModelCompatibilityGroupConfig],
    right: &[ModelCompatibilityGroupConfig],
) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            normalize_model_name(&left.id) == normalize_model_name(&right.id)
                && left.disabled == right.disabled
                && left.models.len() == right.models.len()
                && left
                    .models
                    .iter()
                    .zip(&right.models)
                    .all(|(left, right)| normalize_model_name(left) == normalize_model_name(right))
        })
}

fn rebase_conflict_profile(
    config: &ClientConfig,
    profile: &LocalModelCompatibilityProfile,
    remote: &Value,
) -> Result<LocalModelCompatibilityProfile> {
    let latest = profile_from_remote(config, remote, "conflict", profile.pending_reset)?;
    let mut conflict = profile.clone();
    conflict.base_release_id = latest.base_release_id;
    conflict.model_version_id = latest.model_version_id;
    conflict.compatibility_version_id = latest.compatibility_version_id;
    conflict.revision = latest.revision;
    conflict.catalog_models = merge_compatibility_catalogs(
        latest.catalog_models.clone(),
        profile.catalog_models.clone(),
    );
    conflict.sync_status = "conflict".to_string();
    conflict.updated_at_unix_ms = unix_time_millis();
    if profile.pending_reset {
        conflict.model_groups = latest.template_model_groups.clone();
        conflict.customized = false;
    } else {
        conflict.model_groups = rebase_model_groups_to_template(
            &profile.model_groups,
            &profile.template_model_groups,
            &latest.template_model_groups,
            &conflict.catalog_models,
        )?;
    }
    conflict.template_model_groups = latest.template_model_groups;
    Ok(conflict)
}

fn payload_array(payload: &Value, key: &str) -> Result<Vec<ModelCompatibilityGroupConfig>> {
    serde_json::from_value(
        payload
            .get(key)
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
    )
    .with_context(|| format!("decode {key}"))
}

fn payload_from_profile(
    state: &AppState,
    remote: Option<Value>,
    profile: &LocalModelCompatibilityProfile,
    sync_error: Option<String>,
) -> Value {
    let mut payload = remote.filter(Value::is_object).unwrap_or_else(|| json!({}));
    let object = match &mut payload {
        Value::Object(object) => object,
        _ => return json!({}),
    };
    object.insert(
        "object".to_string(),
        Value::String("account_model_compatibility".to_string()),
    );
    object.insert("version".to_string(), Value::from(1));
    object.insert(
        "source".to_string(),
        Value::String(
            if is_local_model_compatibility_cache(profile) {
                if profile.customized {
                    "local_user"
                } else {
                    "client"
                }
            } else if profile.customized {
                "user"
            } else {
                "server"
            }
            .to_string(),
        ),
    );
    object.insert("customized".to_string(), Value::Bool(profile.customized));
    object.insert(
        "platform_sync_available".to_string(),
        Value::Bool(
            state
                .platform_api_key
                .lock()
                .map(|key| !key.trim().is_empty())
                .unwrap_or(false),
        ),
    );
    object.insert(
        "release_id".to_string(),
        Value::String(profile.base_release_id.clone()),
    );
    object.insert(
        "base_release_id".to_string(),
        Value::String(profile.base_release_id.clone()),
    );
    object.insert(
        "model_version_id".to_string(),
        Value::String(profile.model_version_id.clone()),
    );
    object.insert(
        "compatibility_version_id".to_string(),
        Value::String(profile.compatibility_version_id.clone()),
    );
    object.insert("revision".to_string(), Value::from(profile.revision));
    object.insert(
        "model_groups".to_string(),
        serde_json::to_value(&profile.model_groups).unwrap_or_else(|_| Value::Array(Vec::new())),
    );
    object.insert(
        "template_model_groups".to_string(),
        serde_json::to_value(&profile.template_model_groups)
            .unwrap_or_else(|_| Value::Array(Vec::new())),
    );
    object.insert(
        "catalog_models".to_string(),
        serde_json::to_value(catalog_models_for_payload(state, profile))
            .unwrap_or_else(|_| Value::Array(Vec::new())),
    );
    object.insert(
        "aliases".to_string(),
        serde_json::to_value(aliases_for_groups(&profile.model_groups))
            .unwrap_or_else(|_| json!({})),
    );
    object.insert(
        "sync_status".to_string(),
        Value::String(profile.sync_status.clone()),
    );
    object.insert(
        "local_available_models".to_string(),
        serde_json::to_value(local_available_model_ids(state))
            .unwrap_or_else(|_| Value::Array(Vec::new())),
    );
    object
        .entry("platform_available_models".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(error) = sync_error.filter(|error| !error.trim().is_empty()) {
        object.insert("sync_error".to_string(), Value::String(error));
    } else {
        object.remove("sync_error");
    }
    payload
}

fn persist_local_model_compatibility(
    state: &AppState,
    profile: LocalModelCompatibilityProfile,
) -> Result<()> {
    let key = model_compatibility_profile_key(&profile.platform_id, &profile.user_id)
        .context("model compatibility profile has no account scope")?;
    commit_config_update(&state.config_path, &state.proxy_config, move |config| {
        config
            .model_compatibility_profiles
            .insert(key, profile.clone());
        if !is_local_model_compatibility_cache(&profile) {
            let mut cache = profile;
            cache.platform_id = LOCAL_MODEL_COMPATIBILITY_PLATFORM_ID.to_string();
            cache.user_id = LOCAL_MODEL_COMPATIBILITY_USER_ID.to_string();
            config
                .model_compatibility_profiles
                .insert(local_model_compatibility_cache_key(), cache);
        }
        Ok(())
    })?;
    Ok(())
}

fn current_account_scope(config: &ClientConfig) -> Option<(String, String)> {
    let platform_id = if config.account_platform_id.trim().is_empty() {
        config.platform_id.trim()
    } else {
        config.account_platform_id.trim()
    };
    let user_id = config.account_user_id.trim();
    if platform_id.is_empty()
        || user_id.is_empty()
        || config.account_device_api_key.trim().is_empty()
    {
        return None;
    }
    Some((platform_id.to_string(), user_id.to_string()))
}

fn platform_authorized_config(state: &AppState) -> Result<ClientConfig> {
    let mut config = load_config_from_path(&state.config_path)?;
    let active_key = state
        .platform_api_key
        .lock()
        .map_err(|_| anyhow!("platform API key lock poisoned"))?
        .clone();
    if active_key.trim().is_empty() {
        return Err(anyhow!(
            "platform access is unavailable until legal consent reconciliation completes"
        ));
    }
    config.account_device_api_key = active_key;
    Ok(config)
}

fn normalize_local_model_groups(
    groups: Vec<ModelCompatibilityGroupConfig>,
    templates: &[ModelCompatibilityGroupConfig],
    catalog: &[ModelCompatibilityCatalogModel],
) -> Result<Vec<ModelCompatibilityGroupConfig>> {
    let template_by_id = templates
        .iter()
        .map(|group| (normalize_model_name(&group.id), group))
        .collect::<HashMap<_, _>>();
    let catalog_by_id = catalog
        .iter()
        .filter_map(|model| {
            let key = normalize_model_name(&model.id);
            (!key.is_empty()).then_some((key, model.id.trim().to_string()))
        })
        .collect::<HashMap<_, _>>();
    if template_by_id.is_empty() || catalog_by_id.is_empty() {
        return Err(anyhow!("server template or model catalog is unavailable"));
    }
    let system_models = templates
        .iter()
        .flat_map(|group| group.models.iter().chain(group.match_models.iter()))
        .map(|model| normalize_model_name(model))
        .filter(|model| !model.is_empty())
        .collect::<HashSet<_>>();
    let mut seen_groups = HashSet::new();
    let mut inputs_by_id = HashMap::new();
    for input in groups {
        let group_key = normalize_model_name(&input.id);
        if !template_by_id.contains_key(&group_key) {
            return Err(anyhow!("unknown compatibility group {}", input.id));
        }
        if !seen_groups.insert(group_key.clone()) {
            return Err(anyhow!("duplicate compatibility group {}", input.id));
        }
        inputs_by_id.insert(group_key, input);
    }

    let mut seen_models = HashMap::<String, String>::new();
    let mut out = Vec::with_capacity(templates.len());
    for template in templates {
        let group_key = normalize_model_name(&template.id);
        let input = inputs_by_id.remove(&group_key);
        let mut seen_group_models = HashSet::new();
        let mut additions = Vec::new();
        for candidate in input.map(|group| group.models).unwrap_or_default() {
            let key = normalize_model_name(&candidate);
            let Some(public_id) = catalog_by_id.get(&key) else {
                return Err(anyhow!(
                    "model {candidate} is not in the selectable compatibility catalog"
                ));
            };
            if system_models.contains(&key) || !seen_group_models.insert(key.clone()) {
                continue;
            }
            if let Some(previous) = seen_models.insert(key, template.id.clone()) {
                return Err(anyhow!(
                    "model {public_id} belongs to both {previous} and {}",
                    template.id
                ));
            }
            additions.push(public_id.clone());
        }
        let mut models = template.models.clone();
        models.extend(additions);
        out.push(ModelCompatibilityGroupConfig {
            id: template.id.clone(),
            label: template.label.clone(),
            description: template.description.clone(),
            aliases: template.aliases.clone(),
            models,
            match_models: stable_strings(template.match_models.clone()),
            disabled: false,
        });
    }
    Ok(out)
}

fn normalize_template_groups(
    groups: Vec<ModelCompatibilityGroupConfig>,
) -> Vec<ModelCompatibilityGroupConfig> {
    let mut seen = HashSet::new();
    groups
        .into_iter()
        .filter_map(|mut group| {
            group.id = group.id.trim().to_string();
            group.label = group.label.trim().to_string();
            group.description = group.description.trim().to_string();
            group.aliases = stable_strings(group.aliases);
            group.models = stable_strings(group.models);
            group.match_models = stable_strings(group.match_models);
            let key = normalize_model_name(&group.id);
            if key.is_empty() || group.models.is_empty() || !seen.insert(key) {
                return None;
            }
            Some(group)
        })
        .collect()
}

fn normalize_catalog_models(
    models: Vec<ModelCompatibilityCatalogModel>,
) -> Vec<ModelCompatibilityCatalogModel> {
    let mut seen = HashSet::new();
    models
        .into_iter()
        .filter_map(|mut model| {
            model.id = model.id.trim().to_string();
            model.display_name = model.display_name.trim().to_string();
            model.vendor = model.vendor.trim().to_string();
            model.family = model.family.trim().to_string();
            model.input_modalities = stable_strings(model.input_modalities);
            model.output_modalities = stable_strings(model.output_modalities);
            let key = normalize_model_name(&model.id);
            if key.is_empty() || !seen.insert(key) {
                return None;
            }
            Some(model)
        })
        .collect()
}

fn merge_compatibility_catalogs(
    primary: Vec<ModelCompatibilityCatalogModel>,
    secondary: Vec<ModelCompatibilityCatalogModel>,
) -> Vec<ModelCompatibilityCatalogModel> {
    normalize_catalog_models(primary.into_iter().chain(secondary).collect())
}

fn rebase_model_groups_to_template(
    groups: &[ModelCompatibilityGroupConfig],
    source_templates: &[ModelCompatibilityGroupConfig],
    target_templates: &[ModelCompatibilityGroupConfig],
    target_catalog: &[ModelCompatibilityCatalogModel],
) -> Result<Vec<ModelCompatibilityGroupConfig>> {
    let system_models = source_templates
        .iter()
        .chain(target_templates.iter())
        .flat_map(|group| group.models.iter().chain(group.match_models.iter()))
        .map(|model| normalize_model_name(model))
        .filter(|model| !model.is_empty())
        .collect::<HashSet<_>>();
    let additions = groups
        .iter()
        .map(|group| ModelCompatibilityGroupConfig {
            id: group.id.clone(),
            models: group
                .models
                .iter()
                .filter(|model| !system_models.contains(&normalize_model_name(model)))
                .cloned()
                .collect(),
            ..Default::default()
        })
        .collect();
    normalize_local_model_groups(additions, target_templates, target_catalog)
}

fn stable_strings(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter_map(|value| {
            let value = value.trim().to_string();
            let key = normalize_model_name(&value);
            if key.is_empty() || !seen.insert(key) {
                return None;
            }
            Some(value)
        })
        .collect()
}

fn aliases_for_groups(groups: &[ModelCompatibilityGroupConfig]) -> HashMap<String, String> {
    let mut aliases = HashMap::new();
    for (group_index, group) in groups.iter().enumerate() {
        if group.disabled || group.models.is_empty() {
            continue;
        }
        let candidates = compatibility_group_candidate_chain(groups, group_index).join("|");
        for key in std::iter::once(&group.id)
            .chain(group.aliases.iter())
            .chain(group.match_models.iter())
        {
            let key = normalize_model_name(key);
            if !key.is_empty() {
                aliases.insert(key, candidates.clone());
            }
        }
    }
    for (group_index, group) in groups.iter().enumerate() {
        if group.disabled || group.models.is_empty() {
            continue;
        }
        let candidates = compatibility_group_candidate_chain(groups, group_index).join("|");
        for key in &group.models {
            let key = normalize_model_name(key);
            if !key.is_empty() {
                aliases.insert(key, candidates.clone());
            }
        }
    }
    aliases
}

fn local_available_model_ids(state: &AppState) -> Vec<String> {
    let config = state
        .proxy_config
        .lock()
        .map(|config| config.clone())
        .unwrap_or_else(|_| {
            load_config_from_path(&state.config_path).unwrap_or_else(|_| crate::default_config())
        });
    let ready = state
        .local_channel_readiness
        .lock()
        .map(|readiness| {
            readiness
                .iter()
                .filter_map(|(channel_id, ready)| ready.then_some(channel_id.clone()))
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();
    crate::proxy::local_model_entries(&config, &ready)
        .into_iter()
        .filter_map(|model| model.get("id").and_then(Value::as_str).map(str::to_string))
        .collect()
}

fn configured_local_model_ids(state: &AppState) -> Vec<String> {
    let config = state
        .proxy_config
        .lock()
        .map(|config| config.clone())
        .unwrap_or_else(|_| {
            load_config_from_path(&state.config_path).unwrap_or_else(|_| crate::default_config())
        });
    let mut seen = HashSet::new();
    config
        .channels
        .iter()
        .filter(|channel| crate::proxy::channel_can_forward_locally(channel))
        .flat_map(crate::proxy::visible_channel_models)
        .filter_map(|model| {
            let model = model.trim().to_string();
            let key = normalize_model_name(&model);
            if key.is_empty() || !seen.insert(key) {
                return None;
            }
            Some(model)
        })
        .collect()
}

fn catalog_models_for_payload(
    state: &AppState,
    profile: &LocalModelCompatibilityProfile,
) -> Vec<ModelCompatibilityCatalogModel> {
    let mut models = profile.catalog_models.clone();
    let mut seen = models
        .iter()
        .map(|model| normalize_model_name(&model.id))
        .collect::<HashSet<_>>();
    for id in configured_local_model_ids(state) {
        let key = normalize_model_name(&id);
        if key.is_empty() || !seen.insert(key) {
            continue;
        }
        models.push(ModelCompatibilityCatalogModel {
            display_name: id.clone(),
            id,
            vendor: "local".to_string(),
            ..Default::default()
        });
    }
    models
}

fn unix_time_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_device_key_cannot_bypass_runtime_legal_gate() {
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("client.json");
        let mut config = crate::default_config();
        config.account_platform_id = "platform-1".to_string();
        config.account_user_id = "user-1".to_string();
        config.account_device_api_key = "saved-device-key".to_string();
        crate::write_config_to_path(&config_path, &config).unwrap();
        let state = crate::runtime::create_runtime_state(config_path, config);

        assert!(platform_authorized_config(&state).is_err());
        *state.platform_api_key.lock().unwrap() = "authorized-device-key".to_string();
        assert_eq!(
            platform_authorized_config(&state)
                .unwrap()
                .account_device_api_key,
            "authorized-device-key"
        );
    }

    #[test]
    fn rejected_account_key_falls_back_without_exposing_the_remote_error() {
        let remote_error = RemoteModelCompatibilityError {
            status: Some(401),
            detail: "Invalid API key".to_string(),
        };
        assert!(remote_authorization_failed(&remote_error));

        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("client.json");
        let config = crate::default_config();
        crate::write_config_to_path(&config_path, &config).unwrap();
        let state = crate::runtime::create_runtime_state(config_path, config);
        *state.platform_api_key.lock().unwrap() = "rejected-device-key".to_string();

        let payload = fallback_after_remote_authorization_failure(&state, None)
            .expect("local compatibility fallback");

        assert_eq!(payload["platform_sync_available"], Value::Bool(false));
        assert!(payload.get("sync_error").is_none());
        assert!(
            !payload
                .to_string()
                .to_ascii_lowercase()
                .contains("invalid api key")
        );
        assert!(state.platform_api_key.lock().unwrap().is_empty());
    }

    fn profile_config(groups: Vec<ModelCompatibilityGroupConfig>) -> ClientConfig {
        let mut config = crate::default_config();
        config.account_platform_id = "platform-1".to_string();
        config.account_user_id = "user-1".to_string();
        config.account_device_api_key = "sk-device".to_string();
        let key = model_compatibility_profile_key("platform-1", "user-1").unwrap();
        config.model_compatibility_profiles.insert(
            key,
            LocalModelCompatibilityProfile {
                platform_id: "platform-1".to_string(),
                user_id: "user-1".to_string(),
                model_groups: groups,
                ..Default::default()
            },
        );
        config
    }

    #[test]
    fn local_candidates_follow_user_order_after_exact_model() {
        let config = profile_config(vec![ModelCompatibilityGroupConfig {
            id: "frontier".to_string(),
            aliases: vec!["best".to_string()],
            models: vec!["model-b".to_string(), "model-a".to_string()],
            ..Default::default()
        }]);
        assert_eq!(
            model_compatibility_candidates(&config, "best"),
            vec!["model-b", "model-a"]
        );
        assert_eq!(
            model_compatibility_candidates(&config, "model-a"),
            vec!["model-b"]
        );
    }

    #[test]
    fn embedded_default_keeps_local_compatibility_available_before_login() {
        let mut config = crate::default_config();
        config.account_platform_id.clear();
        config.account_user_id.clear();
        config.model_compatibility_profiles.clear();

        assert_eq!(
            model_compatibility_candidates(&config, "claude-opus-4-8"),
            vec![
                "kimi-k3",
                "gpt-5.6-terra",
                "gpt-5.5",
                "muse-spark-1.3",
                "claude-mythos-5",
                "claude-fable-5",
                "gpt-6-sol",
                "gpt-5.6-sol",
                "claude-opus-5-5",
                "claude-opus-5",
                "grok-4.7",
                "grok-4.6",
                "claude-mythos-5-1",
                "claude-fable-5-1",
                "gpt-6-astra",
            ]
        );
    }

    #[test]
    fn embedded_default_only_auto_substitutes_reviewed_coding_models() {
        let profile = client_default_model_compatibility_profile();
        let ids = profile
            .model_groups
            .iter()
            .flat_map(|group| group.models.iter().cloned())
            .collect::<Vec<_>>();
        let models = crate::tool_model_metadata::tool_models_from_ids(&ids);

        assert_eq!(
            models.len(),
            ids.len(),
            "every compatibility model must have catalog metadata"
        );
        for model in &models {
            assert!(model.reasoning, "{} must support reasoning", model.id);
            assert!(model.tool_call, "{} must support tool calls", model.id);
            // Spark remains for Codex compatibility, but is officially
            // text-only. Never invent vision to satisfy the general rule.
            if model.id == "gpt-5.3-codex-spark" {
                assert_eq!(model.input_modalities, vec!["text"]);
            } else {
                assert!(
                    model
                        .input_modalities
                        .iter()
                        .any(|modality| modality == "image"),
                    "{} must support image input",
                    model.id
                );
            }
            assert!(
                model.context_tokens.is_some_and(|tokens| tokens >= 128_000),
                "{} must expose at least 128K context",
                model.id
            );
        }
        // Compatibility selects the explicit V4.1 Flash identity; the generic
        // and older V4 names remain catalog entries, not duplicate choices.
        assert!(
            ids.iter()
                .any(|candidate| candidate == "deepseek-v4.1-flash")
        );
        assert!(!ids.iter().any(|candidate| candidate == "deepseek-flash"));
        for (model_id, expected_output_tokens, expected_input_modalities) in [
            ("deepseek-v4-pro", 384_000, vec!["text"]),
            ("deepseek-v4-flash", 393_216, vec!["image", "text"]),
        ] {
            assert!(
                !ids.iter().any(|candidate| candidate == model_id),
                "legacy {model_id} must remain outside compatibility groups"
            );
            let model = crate::tool_model_metadata::tool_models_from_ids(&[model_id.to_string()])
                .into_iter()
                .next()
                .expect("catalog DeepSeek metadata");
            assert_eq!(model.context_tokens, Some(1_000_000));
            assert_eq!(model.output_tokens, Some(expected_output_tokens));
            assert_eq!(model.input_modalities, expected_input_modalities);
            assert!(model.reasoning);
            assert!(model.tool_call);
        }
        let deepseek_flash = crate::tool_model_metadata::tool_models_from_ids(&[
            "deepseek-v4-flash".to_string(),
            "deepseek-v4-pro".to_string(),
        ]);
        let flash = deepseek_flash
            .iter()
            .find(|model| model.id == "deepseek-v4-flash")
            .expect("catalog DeepSeek Flash metadata");
        assert_eq!(flash.reasoning_efforts, vec!["low", "high", "max"]);
        let pro = deepseek_flash
            .iter()
            .find(|model| model.id == "deepseek-v4-pro")
            .expect("catalog DeepSeek Pro metadata");
        assert_eq!(pro.reasoning_efforts, vec!["low", "high", "max"]);
        assert!(
            !ids.iter()
                .any(|model| model.eq_ignore_ascii_case("MiniMax-M3"))
        );
        assert!(
            profile
                .catalog_models
                .iter()
                .any(|model| model.id.eq_ignore_ascii_case("MiniMax-M3"))
        );
        let kimi_k3 = models
            .iter()
            .find(|model| model.id == "kimi-k3")
            .expect("embedded Kimi K3 metadata");
        assert_eq!(kimi_k3.context_tokens, Some(1_048_576));
        assert!(
            kimi_k3
                .input_modalities
                .iter()
                .any(|value| value == "image")
        );
        assert!(kimi_k3.reasoning);
        assert!(kimi_k3.tool_call);
        assert!(
            profile
                .catalog_models
                .iter()
                .any(|model| model.id == "glm-5.2")
        );
        assert!(
            profile
                .catalog_models
                .iter()
                .any(|model| model.id == "glm-5v-turbo")
        );
        assert!(profile.catalog_models.iter().any(|model| model.id == "hy3"));
        assert!(!ids.iter().any(|model| model.starts_with("glm-")));
    }

    #[test]
    fn embedded_default_uses_neutral_tiers_and_balances_flash_and_muse() {
        let profile = client_default_model_compatibility_profile();
        assert_eq!(
            profile
                .model_groups
                .iter()
                .map(|group| group.label.as_str())
                .collect::<Vec<_>>(),
            ["T0", "T1", "T2", "T3", "T4", "T5", "T6", "T7"]
        );
        for (model, expected_group) in [
            ("muse-spark-1.3", "elite"),
            ("gemini-3.8-flash", "advanced"),
            ("gemini-3.7-flash", "advanced"),
            ("gemini-3.6-flash", "capable"),
            ("gemini-3.5-flash", "capable"),
            ("gemini-3-flash-preview", "standard"),
            ("gemini-2.5-flash", "basic"),
        ] {
            let actual = profile
                .model_groups
                .iter()
                .find(|group| group.models.iter().any(|candidate| candidate == model))
                .map(|group| group.id.as_str());
            assert_eq!(actual, Some(expected_group), "{model}");
        }
        for model in [
            "gemini-3.5-flash-lite",
            "gemini-3.1-flash-lite",
            "gemini-2.5-flash-lite",
        ] {
            assert!(
                !profile
                    .model_groups
                    .iter()
                    .any(|group| group.models.iter().any(|candidate| candidate == model)),
                "{model} must remain opt-in"
            );
            assert!(
                profile
                    .catalog_models
                    .iter()
                    .any(|candidate| candidate.id == model),
                "{model} must remain selectable"
            );
        }
    }

    #[test]
    fn stale_local_full_snapshot_inherits_new_defaults_and_keeps_only_user_additions() {
        let old_template = vec![ModelCompatibilityGroupConfig {
            id: "apex".to_string(),
            label: "Old apex".to_string(),
            models: vec!["legacy-system-model".to_string()],
            ..Default::default()
        }];
        let merged = rebase_profile_onto_active_defaults(LocalModelCompatibilityProfile {
            platform_id: LOCAL_MODEL_COMPATIBILITY_PLATFORM_ID.to_string(),
            user_id: LOCAL_MODEL_COMPATIBILITY_USER_ID.to_string(),
            base_release_id: "client-compat-v5-2026-07-24".to_string(),
            model_groups: vec![ModelCompatibilityGroupConfig {
                id: "apex".to_string(),
                models: vec![
                    "legacy-system-model".to_string(),
                    "custom/local-gpt-6".to_string(),
                ],
                ..Default::default()
            }],
            template_model_groups: old_template,
            catalog_models: vec![ModelCompatibilityCatalogModel {
                id: "custom/local-gpt-6".to_string(),
                ..Default::default()
            }],
            customized: true,
            ..Default::default()
        });

        assert_eq!(
            merged.base_release_id,
            crate::tool_model_metadata::active_tool_compatibility_release_id()
        );
        let apex = merged
            .model_groups
            .iter()
            .find(|group| group.id == "apex")
            .expect("current apex");
        assert_eq!(
            apex.models,
            [
                "claude-mythos-5-1",
                "claude-fable-5-1",
                "gpt-6-astra",
                "custom/local-gpt-6",
            ]
        );
        assert!(!merged.model_groups.iter().any(|group| {
            group
                .models
                .iter()
                .any(|model| model == "legacy-system-model")
        }));
    }

    #[test]
    fn cached_server_snapshot_is_rebased_onto_the_active_authority() {
        let mut config = crate::default_config();
        config.model_compatibility_profiles.insert(
            local_model_compatibility_cache_key(),
            LocalModelCompatibilityProfile {
                platform_id: LOCAL_MODEL_COMPATIBILITY_PLATFORM_ID.to_string(),
                user_id: LOCAL_MODEL_COMPATIBILITY_USER_ID.to_string(),
                base_release_id: "server-catalog-old".to_string(),
                compatibility_version_id: "compat-old".to_string(),
                model_groups: vec![ModelCompatibilityGroupConfig {
                    id: "apex".to_string(),
                    models: vec![
                        "old-server-system-model".to_string(),
                        "custom/local-gpt-6".to_string(),
                    ],
                    ..Default::default()
                }],
                template_model_groups: vec![ModelCompatibilityGroupConfig {
                    id: "apex".to_string(),
                    models: vec!["old-server-system-model".to_string()],
                    ..Default::default()
                }],
                catalog_models: vec![ModelCompatibilityCatalogModel {
                    id: "custom/local-gpt-6".to_string(),
                    ..Default::default()
                }],
                customized: true,
                ..Default::default()
            },
        );

        let effective = effective_local_model_compatibility_profile(&config);
        assert_eq!(
            effective.base_release_id,
            crate::tool_model_metadata::active_tool_compatibility_release_id()
        );
        assert!(effective.model_groups.iter().any(|group| {
            group
                .models
                .iter()
                .any(|model| model == "custom/local-gpt-6")
        }));
        assert!(!effective.model_groups.iter().any(|group| {
            group
                .models
                .iter()
                .any(|model| model == "old-server-system-model")
        }));
    }

    #[test]
    fn remote_server_snapshot_contributes_preferences_but_never_replaces_catalog() {
        let mut config = crate::default_config();
        config.account_platform_id = "platform-1".to_string();
        config.account_user_id = "user-1".to_string();
        config.account_device_api_key = "sk-device".to_string();
        let payload = json!({
            "release_id": "server-catalog-old",
            "model_version_id": "models-old",
            "compatibility_version_id": "compat-old",
            "revision": 7,
            "customized": true,
            "template_model_groups": [{
                "id": "apex",
                "label": "T0",
                "models": ["old-server-system-model"]
            }],
            "model_groups": [{
                "id": "apex",
                "label": "T0",
                "models": ["old-server-system-model", "custom/local-gpt-6"]
            }],
            "catalog_models": [{"id": "custom/local-gpt-6"}],
            "platform_available_models": ["old-server-system-model"]
        });

        let profile = profile_from_remote(&config, &payload, "synced", false)
            .expect("rebase server response");
        let active = client_default_model_compatibility_profile();
        assert_eq!(profile.base_release_id, active.base_release_id);
        assert_eq!(profile.model_version_id, active.model_version_id);
        assert_eq!(
            profile.compatibility_version_id,
            active.compatibility_version_id
        );
        assert_eq!(profile.platform_id, "platform-1");
        assert_eq!(profile.user_id, "user-1");
        assert_eq!(profile.revision, 7);
        assert!(
            profile
                .model_groups
                .iter()
                .any(|group| group.models.iter().any(|model| model == "claude-opus-5"))
        );
        assert!(profile.model_groups.iter().any(|group| {
            group
                .models
                .iter()
                .any(|model| model == "custom/local-gpt-6")
        }));
        assert!(!profile.model_groups.iter().any(|group| {
            group
                .models
                .iter()
                .any(|model| model == "old-server-system-model")
        }));

        let temp = tempfile::tempdir().expect("tempdir");
        let state = crate::runtime::create_runtime_state(temp.path().join("client.json"), config);
        let projected = payload_from_profile(&state, Some(payload), &profile, None);
        assert_eq!(
            projected["release_id"].as_str(),
            Some(active.base_release_id.as_str())
        );
    }

    #[test]
    fn local_normalization_locks_system_models_and_persists_additions_after_them() {
        let templates = vec![
            ModelCompatibilityGroupConfig {
                id: "apex".to_string(),
                label: "T0".to_string(),
                models: vec!["system-a".to_string()],
                match_models: vec!["match-only".to_string()],
                ..Default::default()
            },
            ModelCompatibilityGroupConfig {
                id: "frontier".to_string(),
                label: "T1".to_string(),
                models: vec!["system-b".to_string()],
                ..Default::default()
            },
        ];
        let catalog = ["system-a", "system-b", "match-only", "custom/local-gpt-6"]
            .into_iter()
            .map(|id| ModelCompatibilityCatalogModel {
                id: id.to_string(),
                ..Default::default()
            })
            .collect::<Vec<_>>();
        let normalized = normalize_local_model_groups(
            vec![
                ModelCompatibilityGroupConfig {
                    id: "apex".to_string(),
                    models: vec![
                        "system-b".to_string(),
                        "match-only".to_string(),
                        "custom/local-gpt-6".to_string(),
                    ],
                    disabled: true,
                    ..Default::default()
                },
                ModelCompatibilityGroupConfig {
                    id: "frontier".to_string(),
                    models: vec!["system-a".to_string()],
                    disabled: true,
                    ..Default::default()
                },
            ],
            &templates,
            &catalog,
        )
        .expect("normalized compatibility additions");

        assert_eq!(normalized[0].models, ["system-a", "custom/local-gpt-6"]);
        assert_eq!(normalized[1].models, ["system-b"]);
        assert!(normalized.iter().all(|group| !group.disabled));
    }

    #[test]
    fn compatibility_catalog_includes_configured_models_even_when_not_ready() {
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("client.json");
        let mut config = crate::default_config();
        config.channels[0].enabled = false;
        config.channels[0].models = vec!["custom/local-gpt-6".to_string()];
        crate::write_config_to_path(&config_path, &config).unwrap();
        let state = crate::runtime::create_runtime_state(config_path, config);
        let profile = client_default_model_compatibility_profile();

        let catalog = catalog_models_for_payload(&state, &profile);
        assert!(
            catalog
                .iter()
                .any(|model| model.id == "custom/local-gpt-6" && model.vendor == "local")
        );
        assert!(
            !local_available_model_ids(&state)
                .iter()
                .any(|model| model == "custom/local-gpt-6")
        );
    }

    #[test]
    fn codex_spark_is_a_candidate_while_auto_review_is_match_only() {
        let mut config = crate::default_config();
        config.model_compatibility_profiles.clear();
        let profile = client_default_model_compatibility_profile();
        let standard = profile
            .model_groups
            .iter()
            .find(|group| group.id == "standard")
            .expect("T6 group");
        assert_eq!(standard.aliases, ["standard", "t6"]);
        assert!(
            standard
                .models
                .iter()
                .any(|model| model == "gpt-5.3-codex-spark")
        );
        assert!(
            standard
                .match_models
                .iter()
                .any(|model| model == "claude-sonnet-4-5-20250929")
        );
        let reviewer = profile
            .model_groups
            .iter()
            .find(|group| group.id == "expert")
            .expect("T3 group");
        assert!(reviewer.models.iter().any(|model| model == "gpt-5.6-luna"));
        assert!(
            reviewer
                .match_models
                .iter()
                .any(|model| model == "codex-auto-review")
        );

        let spark_candidates = model_compatibility_candidates(&config, "gpt-5.3-codex-spark");
        assert_eq!(
            &spark_candidates[..4],
            [
                "claude-sonnet-4-5",
                "grok-build-0.1",
                "gpt-5.4-nano",
                "grok-4.3",
            ]
        );
        assert!(
            !spark_candidates
                .iter()
                .any(|model| model == "gpt-5.3-codex-spark")
        );

        // Guardian is a specialized request identity, never a general fallback
        // candidate. Its Luna-level fallback floor excludes weaker Flash/Spark.
        let review_candidates = model_compatibility_candidates(&config, "codex-auto-review");
        assert_eq!(
            &review_candidates[..5],
            [
                "grok-4.5",
                "gpt-5.4",
                "claude-opus-4-7",
                "claude-sonnet-5",
                "gpt-6-luna",
            ]
        );
        assert!(
            !review_candidates
                .iter()
                .any(|model| model == "codex-auto-review"
                    || model.starts_with("gemini-")
                    || model == "gpt-5.3-codex-spark")
        );
        assert!(profile.model_groups.iter().all(|group| {
            !group
                .models
                .iter()
                .any(|model| model == "codex-auto-review")
        }));
    }

    #[test]
    fn anthropic_routes_use_compatible_claude_names_for_non_anthropic_models() {
        let config = crate::default_config();
        let routes = anthropic_model_routes(
            &config,
            &[
                "gpt-5.6-sol".to_string(),
                "gpt-5.6-terra".to_string(),
                "gpt-5.6-luna".to_string(),
                "gpt-5.5".to_string(),
                "gpt-5.4".to_string(),
                "gpt-5.4-mini".to_string(),
                "gpt-5.3-codex-spark".to_string(),
            ],
        );

        assert_eq!(
            routes.iter().map(String::as_str).collect::<Vec<_>>(),
            vec![
                "claude-mythos-5",
                "claude-fable-5",
                "claude-opus-5-5",
                "claude-opus-5",
                "claude-opus-4-8",
                "claude-opus-4-7",
                "claude-sonnet-5",
                "claude-opus-4-6",
                "claude-sonnet-4-6",
                "claude-opus-4-5",
                "claude-sonnet-4-5",
                "claude-haiku-4-5",
            ]
        );
    }

    #[test]
    fn anthropic_routes_publish_lower_groups_when_higher_supply_is_live() {
        let config = crate::default_config();
        assert!(anthropic_model_routes(&config, &[]).is_empty());

        let routes = anthropic_model_routes(&config, &["gpt-5.6-sol".to_string()]);
        assert_eq!(
            routes,
            vec![
                "claude-mythos-5",
                "claude-fable-5",
                "claude-opus-5-5",
                "claude-opus-5",
                "claude-opus-4-8",
                "claude-opus-4-7",
                "claude-sonnet-5",
                "claude-opus-4-6",
                "claude-sonnet-4-6",
                "claude-opus-4-5",
                "claude-sonnet-4-5",
                "claude-haiku-4-5",
            ]
        );
    }

    #[test]
    fn local_candidates_search_same_group_then_each_higher_group() {
        let config = profile_config(vec![
            ModelCompatibilityGroupConfig {
                id: "highest".to_string(),
                models: vec!["high-a".to_string(), "high-b".to_string()],
                ..Default::default()
            },
            ModelCompatibilityGroupConfig {
                id: "middle".to_string(),
                models: vec!["mid-a".to_string()],
                ..Default::default()
            },
            ModelCompatibilityGroupConfig {
                id: "lower".to_string(),
                aliases: vec!["low".to_string()],
                models: vec!["low-a".to_string(), "low-b".to_string()],
                ..Default::default()
            },
        ]);

        assert_eq!(
            model_compatibility_candidates(&config, "low-b"),
            vec!["low-a", "mid-a", "high-a", "high-b"]
        );
        assert_eq!(
            model_compatibility_candidates(&config, "low"),
            vec!["low-a", "low-b", "mid-a", "high-a", "high-b"]
        );
    }

    #[test]
    fn alias_preview_uses_the_same_upward_candidate_chain() {
        let groups = vec![
            ModelCompatibilityGroupConfig {
                id: "highest".to_string(),
                models: vec!["high-a".to_string(), "high-b".to_string()],
                ..Default::default()
            },
            ModelCompatibilityGroupConfig {
                id: "middle".to_string(),
                models: vec!["mid-a".to_string()],
                ..Default::default()
            },
            ModelCompatibilityGroupConfig {
                id: "lower".to_string(),
                aliases: vec!["low".to_string()],
                models: vec!["low-a".to_string(), "low-b".to_string()],
                ..Default::default()
            },
        ];

        let aliases = aliases_for_groups(&groups);
        assert_eq!(
            aliases.get("low").map(String::as_str),
            Some("low-a|low-b|mid-a|high-a|high-b")
        );
        assert_eq!(
            aliases.get("low-b").map(String::as_str),
            Some("low-a|low-b|mid-a|high-a|high-b")
        );
    }

    #[test]
    fn anthropic_routes_keep_native_models_without_equivalence() {
        let mut config = crate::default_config();
        config.allow_model_equivalence = false;

        assert_eq!(
            anthropic_model_routes(
                &config,
                &["gpt-5.6-sol".to_string(), "claude-sonnet-4-6".to_string(),],
            ),
            vec!["claude-sonnet-4-6"]
        );
    }

    #[test]
    fn logged_out_runtime_uses_the_latest_cached_profile_before_embedded_defaults() {
        let mut config = profile_config(vec![ModelCompatibilityGroupConfig {
            id: "frontier".to_string(),
            models: vec!["cached-model".to_string(), "requested-model".to_string()],
            ..Default::default()
        }]);
        config.account_platform_id.clear();
        config.account_user_id.clear();

        assert_eq!(
            model_compatibility_candidates(&config, "requested-model"),
            vec!["cached-model"]
        );
    }

    #[test]
    fn device_cache_wins_over_stale_account_caches() {
        let mut config = profile_config(vec![ModelCompatibilityGroupConfig {
            id: "frontier".to_string(),
            models: vec!["account-model".to_string(), "requested-model".to_string()],
            ..Default::default()
        }]);
        config.account_platform_id.clear();
        config.account_user_id.clear();
        config.model_compatibility_profiles.insert(
            local_model_compatibility_cache_key(),
            LocalModelCompatibilityProfile {
                platform_id: LOCAL_MODEL_COMPATIBILITY_PLATFORM_ID.to_string(),
                user_id: LOCAL_MODEL_COMPATIBILITY_USER_ID.to_string(),
                model_groups: vec![ModelCompatibilityGroupConfig {
                    id: "frontier".to_string(),
                    models: vec!["device-model".to_string(), "requested-model".to_string()],
                    ..Default::default()
                }],
                customized: true,
                sync_status: "pending".to_string(),
                ..Default::default()
            },
        );

        assert_eq!(
            model_compatibility_candidates(&config, "requested-model"),
            vec!["device-model"]
        );
    }

    #[test]
    fn pending_device_customization_is_rebased_to_the_account_template() {
        let config = profile_config(Vec::new());
        let local = LocalModelCompatibilityProfile {
            platform_id: LOCAL_MODEL_COMPATIBILITY_PLATFORM_ID.to_string(),
            user_id: LOCAL_MODEL_COMPATIBILITY_USER_ID.to_string(),
            model_groups: vec![ModelCompatibilityGroupConfig {
                id: "frontier".to_string(),
                models: vec![
                    "old-system-model".to_string(),
                    "custom/local-gpt-6".to_string(),
                ],
                ..Default::default()
            }],
            template_model_groups: vec![ModelCompatibilityGroupConfig {
                id: "frontier".to_string(),
                models: vec!["old-system-model".to_string()],
                ..Default::default()
            }],
            catalog_models: vec![ModelCompatibilityCatalogModel {
                id: "custom/local-gpt-6".to_string(),
                vendor: "local".to_string(),
                ..Default::default()
            }],
            customized: true,
            sync_status: "pending".to_string(),
            ..Default::default()
        };
        let remote = LocalModelCompatibilityProfile {
            base_release_id: "server-release".to_string(),
            model_version_id: "server-models-v2".to_string(),
            revision: 7,
            template_model_groups: vec![ModelCompatibilityGroupConfig {
                id: "frontier".to_string(),
                label: "Frontier".to_string(),
                models: vec!["model-a".to_string()],
                ..Default::default()
            }],
            catalog_models: vec![ModelCompatibilityCatalogModel {
                id: "model-a".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };

        let promoted =
            promote_local_profile_to_account(&config, &local, &remote).expect("promoted profile");

        assert_eq!(promoted.platform_id, "platform-1");
        assert_eq!(promoted.user_id, "user-1");
        assert_eq!(promoted.base_release_id, "server-release");
        assert_eq!(promoted.model_version_id, "server-models-v2");
        assert_eq!(promoted.revision, 7);
        assert_eq!(
            promoted.model_groups[0].models,
            vec!["model-a", "custom/local-gpt-6"]
        );
        assert!(
            !promoted.model_groups[0]
                .models
                .iter()
                .any(|model| model == "old-system-model")
        );
        assert!(
            promoted
                .catalog_models
                .iter()
                .any(|model| model.id == "custom/local-gpt-6")
        );
        assert!(promoted.customized);
        assert_eq!(promoted.sync_status, "pending");
    }

    #[test]
    fn disabled_group_does_not_produce_local_candidates() {
        let config = profile_config(vec![ModelCompatibilityGroupConfig {
            id: "frontier".to_string(),
            models: vec!["model-a".to_string()],
            disabled: true,
            ..Default::default()
        }]);
        assert!(model_compatibility_candidates(&config, "model-a").is_empty());
    }

    #[test]
    fn current_candidate_group_wins_before_an_old_template_match_fallback() {
        let config = profile_config(vec![
            ModelCompatibilityGroupConfig {
                id: "old".to_string(),
                models: vec!["model-a".to_string()],
                match_models: vec!["moved-model".to_string()],
                ..Default::default()
            },
            ModelCompatibilityGroupConfig {
                id: "new".to_string(),
                models: vec!["moved-model".to_string(), "model-b".to_string()],
                ..Default::default()
            },
        ]);
        assert_eq!(
            model_compatibility_candidates(&config, "moved-model"),
            vec!["model-b", "model-a"]
        );
    }

    #[tokio::test]
    async fn local_reset_restores_embedded_fallback_instead_of_stale_ui_template() {
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("client.json");
        let mut config = crate::default_config();
        config.model_compatibility_profiles.insert(
            local_model_compatibility_cache_key(),
            LocalModelCompatibilityProfile {
                platform_id: LOCAL_MODEL_COMPATIBILITY_PLATFORM_ID.to_string(),
                user_id: LOCAL_MODEL_COMPATIBILITY_USER_ID.to_string(),
                base_release_id: "stale-release".to_string(),
                model_groups: vec![ModelCompatibilityGroupConfig {
                    id: "stale".to_string(),
                    models: vec!["stale-model".to_string()],
                    ..Default::default()
                }],
                customized: true,
                ..Default::default()
            },
        );
        crate::write_config_to_path(&config_path, &config).unwrap();
        let state = crate::runtime::create_runtime_state(config_path.clone(), config);

        let payload = reset_model_compatibility_inner(
            &state,
            ResetModelCompatibilityInput {
                base_release_id: "renderer-stale-release".to_string(),
                revision: 99,
                template_model_groups: vec![ModelCompatibilityGroupConfig {
                    id: "renderer-stale".to_string(),
                    models: vec!["renderer-stale-model".to_string()],
                    ..Default::default()
                }],
                catalog_models: vec![ModelCompatibilityCatalogModel {
                    id: "renderer-stale-model".to_string(),
                    ..Default::default()
                }],
            },
        )
        .await
        .unwrap();

        let expected = client_default_model_compatibility_profile();
        let expected_release = crate::tool_model_metadata::active_tool_compatibility_release_id();
        assert_eq!(
            payload["base_release_id"].as_str(),
            Some(expected_release.as_str())
        );
        assert_eq!(
            payload["model_version_id"].as_str(),
            Some(expected.model_version_id.as_str())
        );
        let persisted = crate::load_config_from_path(&config_path).unwrap();
        let restored = persisted
            .model_compatibility_profiles
            .get(&local_model_compatibility_cache_key())
            .unwrap();
        assert_eq!(restored.base_release_id, expected.base_release_id);
        assert_eq!(restored.model_version_id, expected.model_version_id);
        assert_eq!(restored.model_groups, expected.model_groups);
        assert_eq!(
            restored.template_model_groups,
            expected.template_model_groups
        );
        assert_eq!(restored.catalog_models, expected.catalog_models);
        assert!(!restored.customized);
        assert_eq!(restored.sync_status, "local");
    }

    #[test]
    fn synced_policy_comparison_ignores_display_metadata_but_keeps_order() {
        let left = vec![ModelCompatibilityGroupConfig {
            id: "frontier".to_string(),
            label: "Local label".to_string(),
            models: vec!["MODEL-A".to_string(), "model-b".to_string()],
            ..Default::default()
        }];
        let mut right = left.clone();
        right[0].label = "Server label".to_string();
        right[0].models[0] = "model-a".to_string();
        assert!(model_group_preferences_equal(&left, &right));

        right[0].models.swap(0, 1);
        assert!(!model_group_preferences_equal(&left, &right));
    }
}
