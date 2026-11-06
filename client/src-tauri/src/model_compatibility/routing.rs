use super::*;
use crate::model::Endpoint;
use crate::proxy::PlatformModelRouteMode;
use std::time::Duration;

// Compile-time rollback: disable both inventory exclusion and demand-driven
// refresh. The existing compatibility sync and live platform routing remain.
const ENABLE_PLATFORM_MODEL_INVENTORY_FILTER: bool = true;

// The server's shared inventory may be five seconds old. Using the request
// start (not response arrival) and a 25-second local age bounds exclusion by
// a 30-second observation window even when the network is slow.
pub(crate) const ROUTING_INVENTORY_MAX_AGE: Duration = Duration::from_secs(25);

#[derive(Deserialize)]
pub(super) struct PlatformRoutingModels {
    schema_version: u32,
    models: HashSet<String>,
    identity_aliases: HashMap<String, String>,
    #[serde(skip)]
    compatible: HashMap<String, String>,
}

impl PlatformRoutingModels {
    pub(super) fn from_payload(payload: &Value) -> Option<Self> {
        let mut inventory: Self =
            serde_json::from_value(payload.get("routing_model_inventory")?.clone()).ok()?;
        if inventory.schema_version != 1 {
            return None;
        }
        inventory.compatible = serde_json::from_value(payload.get("aliases")?.clone()).ok()?;
        // Match using this server's identity rules, not a potentially newer
        // signed client catalog. Literal advertised names also remain valid.
        inventory.models = inventory
            .models
            .iter()
            .flat_map(|model| {
                let name = normalize_model_name(crate::config::without_context_hint(model));
                let public = crate::config::public_model_name(&name);
                [
                    // An unqualified identity candidate can also address a
                    // supplier's qualified spelling through modelname.Matches.
                    crate::config::public_model_name(inventory.identity(&public)),
                    public,
                    crate::config::public_model_name(inventory.identity(&name)),
                ]
            })
            .collect();
        Some(inventory)
    }

    fn identity<'a>(&'a self, name: &'a str) -> &'a str {
        self.identity_aliases
            .get(name)
            .map(String::as_str)
            .unwrap_or(name)
    }

    fn absent(&self, model: &str, mode: PlatformModelRouteMode) -> bool {
        let requested = normalize_model_name(crate::config::without_context_hint(model));
        let identity = self.identity(&requested);
        let model = crate::config::public_model_name(model);
        let identity_name = crate::config::public_model_name(identity);
        match mode {
            PlatformModelRouteMode::ExactOnly => {
                !self.models.contains(&model) && !self.models.contains(&identity_name)
            }
            PlatformModelRouteMode::CompatibleOnly => {
                if self.models.is_empty() {
                    return true;
                }
                // Match both keys used by Go modelAliasKeys (schema v1).
                // The family key may add candidates even when the literal key
                // exists. Unknown keys remain the authority's decision.
                let family = if identity == "default" || identity.starts_with("default ") {
                    Some("default")
                } else {
                    ["fable", "opus", "sonnet", "haiku", "claude"]
                        .into_iter()
                        .find(|part| identity.contains(part))
                };
                let candidates = self
                    .compatible
                    .get(identity)
                    .into_iter()
                    .chain(family.and_then(|key| self.compatible.get(key)));
                let mut known = false;
                for candidates in candidates {
                    known = true;
                    if candidates.split(['|', '\n', '\t']).any(|candidate| {
                        let candidate =
                            normalize_model_name(crate::config::without_context_hint(candidate));
                        let candidate_identity = self.identity(&candidate);
                        candidate_identity != identity
                            && (self
                                .models
                                .contains(&crate::config::public_model_name(&candidate))
                                || self.models.contains(&crate::config::public_model_name(
                                    candidate_identity,
                                )))
                    }) {
                        return false;
                    }
                }
                known
            }
            PlatformModelRouteMode::Configured => false,
        }
    }
}

// This is a read of the existing account model HTTP cache, not a second
// negative-result cache. Never wait for its network refresh on the hot path.
pub(crate) fn cached_platform_model_absent(
    cache: &StdMutex<ModelCompatibilityHttpCache>,
    config: &ClientConfig,
    endpoint: &Endpoint,
    platform_key: &str,
    model: &str,
    mode: PlatformModelRouteMode,
) -> bool {
    if !ENABLE_PLATFORM_MODEL_INVENTORY_FILTER {
        return false;
    }
    let Ok(mut cache) = cache.lock() else {
        return false;
    };
    let Some((scope, slot)) = cache.active.as_ref() else {
        cache.last_routing_demand = Some(Instant::now());
        return false;
    };
    let platform = if config.account_platform_id.trim().is_empty() {
        config.platform_id.trim()
    } else {
        config.account_platform_id.trim()
    };
    let resource_url = format!(
        "{}{}",
        endpoint.base_url.trim_end_matches('/'),
        ACCOUNT_MODEL_COMPATIBILITY_PATH
    );
    let key_hash: [u8; 32] = Sha256::digest(platform_key.trim().as_bytes()).into();
    if scope.0 != resource_url
        || scope.1 != platform
        || scope.2 != config.account_user_id.trim()
        || scope.3 != key_hash
    {
        cache.last_routing_demand = Some(Instant::now());
        return false;
    }
    let slot = slot.clone();
    let Ok(document) = slot.try_lock() else {
        return false;
    };
    let absent = document
        .routing
        .as_ref()
        .is_some_and(|inventory| inventory.absent(model, mode));
    if absent {
        // Ordinary successful-model traffic gains nothing from extra polling.
        // Only exclusions (including stale ones) keep the inventory hot.
        cache.last_routing_demand = Some(Instant::now());
    }
    absent
        && document.last_error.is_none()
        && document
            .validated_at
            .is_some_and(|at| at.elapsed() < ROUTING_INVENTORY_MAX_AGE)
}

pub(crate) fn routing_inventory_refresh_due(cache: &StdMutex<ModelCompatibilityHttpCache>) -> bool {
    if !ENABLE_PLATFORM_MODEL_INVENTORY_FILTER {
        return false;
    }
    let Ok(cache) = cache.lock() else {
        return false;
    };
    let Some(demand) = cache.last_routing_demand else {
        return false;
    };
    let Some((_, slot)) = &cache.active else {
        return true;
    };
    let Ok(document) = slot.try_lock() else {
        return false;
    };
    // Older servers do not provide authoritative presence. Keep their original
    // sync frequency. Idle clients stop fast polling after the last demand.
    if document.value.is_some() && document.routing.is_none() {
        return false;
    }
    // A failed/slow refresh does not queue another refresh per model request.
    document
        .checked_at
        .is_none_or(|at| demand > at && at.elapsed() >= Duration::from_secs(20))
}

pub(crate) async fn refresh_routing_inventory_for_runtime(state: &AppState) -> Result<()> {
    let Ok(config) = platform_authorized_config(state) else {
        return Ok(());
    };
    if current_account_scope(&config).is_none() {
        return Ok(());
    }
    // Refresh only the shared HTTP document. Full preference synchronization
    // retains its existing cadence and does not rewrite config every 20 seconds.
    request_account_model_compatibility(
        &config,
        Method::GET,
        None,
        None,
        &state.model_compatibility_http_cache,
    )
    .await
    .map_err(|error| anyhow!(error.detail))?;
    Ok(())
}

pub(super) fn merge_inventory_delta(
    document: &CompatibilityHttpDocument,
    delta: &Value,
    etag: Option<&HeaderValue>,
) -> Option<Value> {
    let policy = |tag: &HeaderValue| -> Option<String> {
        let tag = tag
            .to_str()
            .ok()?
            .trim_start_matches("W/")
            .trim_matches('"');
        if !tag.starts_with("compatibility-inventory-v1-") {
            return None;
        }
        tag.rsplit_once('-').map(|(policy, _)| policy.to_string())
    };
    if policy(document.etag.as_ref()?)? != policy(etag?)? {
        return None;
    }
    let mut value = document.value.clone()?;
    serde_json::from_value::<Vec<String>>(delta.get("platform_available_models")?.clone()).ok()?;
    for key in ["platform_available_models", "availability_as_of"] {
        value[key] = delta.get(key)?.clone();
    }
    let inventory = delta.get("routing_model_inventory")?;
    if inventory.get("schema_version")?.as_u64()? != 1 {
        return None;
    }
    // Identity aliases belong to the unchanged policy, not this supply delta.
    value["routing_model_inventory"]["models"] = inventory.get("models")?.clone();
    PlatformRoutingModels::from_payload(&value)?;
    Some(value)
}
