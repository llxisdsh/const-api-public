use super::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

type CatalogSlot = Arc<tokio::sync::Mutex<Option<Catalog>>>;
static CATALOGS: OnceLock<Mutex<HashMap<String, CatalogSlot>>> = OnceLock::new();
const CATALOG_INTERVAL: Duration = Duration::from_secs(5 * 60);

struct Catalog {
    models: Vec<crate::model_catalog::ModelObservation>,
    refresh_after: Instant,
}

pub(crate) async fn ensure_kiro_subscription_access_token(
    client: &Client,
    channel: &ChannelConfig,
    force_refresh: bool,
) -> Result<(String, serde_json::Value, bool)> {
    let started = Instant::now();
    let path = subscription_credential_path(channel)?;
    let credential = read_subscription_credential(&path)?;
    crate::kiro::validate_credential(&credential)?;
    let original = json_string(&credential, &["access_token"]).filter(|v| !v.is_empty());
    if let Some(token) = original.as_ref()
        && !force_refresh
        && !credential_needs_refresh(&credential)
    {
        return Ok((token.clone(), credential, false));
    }
    let lock = subscription_refresh_lock(&path)?;
    let mut guard = lock.lock().await;
    guard
        .run(started, async {
            let latest = read_subscription_credential(&path)?;
            crate::kiro::validate_credential(&latest)?;
            if let Some(token) = json_string(&latest, &["access_token"]).filter(|v| !v.is_empty()) {
                let peer_refreshed = original.as_deref() != Some(token.as_str());
                if !credential_needs_refresh(&latest) && (!force_refresh || peer_refreshed) {
                    return Ok((token, latest, peer_refreshed));
                }
            }
            let refresh = latest["refresh_token"]
                .as_str()
                .expect("validated Kiro refresh token");
            let key = subscription_refresh_token_key("kiro", refresh);
            let token_lock = subscription_refresh_token_lock(&key);
            let mut token_guard = token_lock.lock().await;
            token_guard
                .run(started, async {
                    // A forced refresh must not reuse the access token that was
                    // just rejected when the provider keeps its refresh token stable.
                    let cached = cached_subscription_refresh_result(&key).filter(|value| {
                        !force_refresh || value["access_token"] != latest["access_token"]
                    });
                    let mut refreshed = match cached {
                        Some(value) => value,
                        None => {
                            let value = crate::kiro::refresh_token(client, &latest).await?;
                            // Retain a successful rotation even if the subsequent local write fails.
                            remember_subscription_refresh_result(key, value.clone());
                            value
                        }
                    };
                    copy_token_metadata(&latest, &mut refreshed);
                    let token = refreshed["access_token"]
                        .as_str()
                        .ok_or_else(|| anyhow!("Kiro refresh response is missing access_token"))?
                        .to_string();
                    write_subscription_credential_atomically(&path, &refreshed)?;
                    Ok((token, refreshed, true))
                })
                .await
        })
        .await
}

fn allows_stale(error: &anyhow::Error) -> bool {
    subscription_catalog_error_allows_stale(error)
        || error
            .chain()
            .find_map(|cause| cause.downcast_ref::<crate::kiro::ApiError>())
            .is_some_and(|error| error.status == 429 || error.status >= 500)
}

pub(super) async fn catalog(
    channel: &ChannelConfig,
    policy: SubscriptionCatalogPolicy,
) -> Result<(
    Vec<crate::model_catalog::ModelObservation>,
    bool,
    Vec<String>,
)> {
    let key = format!("kiro\0{}", subscription_catalog_cache_identity(channel)?);
    let slot = CATALOGS
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| anyhow!("Kiro catalog cache poisoned"))?
        .entry(key)
        .or_default()
        .clone();
    let mut cached = slot.lock().await;
    if policy == SubscriptionCatalogPolicy::Cached
        && let Some(value) = cached.as_ref()
        && Instant::now() < value.refresh_after
    {
        return Ok((value.models.clone(), false, Vec::new()));
    }
    let client = crate::kiro::http_client()?;
    let mut refreshed = false;
    let result = async {
        let (mut token, mut credential, did_refresh) =
            ensure_kiro_subscription_access_token(&client, channel, false).await?;
        refreshed = did_refresh;
        let mut result =
            crate::kiro::catalog(&client, &token, credential["profile_arn"].as_str().unwrap())
                .await;
        if result
            .as_ref()
            .err()
            .and_then(|e| e.downcast_ref::<crate::kiro::ApiError>())
            .is_some_and(|error| error.status == 401)
        {
            (token, credential, _) =
                ensure_kiro_subscription_access_token(&client, channel, true).await?;
            refreshed = true;
            result =
                crate::kiro::catalog(&client, &token, credential["profile_arn"].as_str().unwrap())
                    .await;
        }
        result
    }
    .await;
    match result {
        Ok(models) => {
            *cached = Some(Catalog {
                models: models.clone(),
                refresh_after: Instant::now() + CATALOG_INTERVAL,
            });
            Ok((models, refreshed, Vec::new()))
        }
        Err(error) if policy == SubscriptionCatalogPolicy::Cached && allows_stale(&error) => {
            if let Some(value) = cached.as_mut() {
                value.refresh_after = Instant::now() + Duration::from_secs(60);
                let warning = format!(
                    "Kiro model refresh unavailable; preserving the last account catalog: {error}"
                );
                log::warn!("[const-api][kiro-catalog] channel={} {warning}", channel.id);
                Ok((value.models.clone(), refreshed, vec![warning]))
            } else {
                Err(error)
            }
        }
        Err(error) => Err(error),
    }
}

pub(super) async fn quota(
    channel: &ChannelConfig,
    policy: SubscriptionCatalogPolicy,
) -> Result<Vec<QuotaWindow>> {
    refresh_subscription_quota(channel, policy, async {
        let client = crate::kiro::http_client()?;
        let (token, credential, _) =
            ensure_kiro_subscription_access_token(&client, channel, false).await?;
        crate::kiro::quota(&client, &token, credential["profile_arn"].as_str().unwrap()).await
    })
    .await
}
