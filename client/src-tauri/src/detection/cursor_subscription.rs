use super::*;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};

pub(crate) async fn ensure_cursor_subscription_access_token(
    client: &Client,
    channel: &ChannelConfig,
    force_refresh: bool,
) -> Result<(String, serde_json::Value, bool)> {
    let started = Instant::now();
    let path = subscription_credential_path(channel)?;
    let original = read_subscription_credential(&path)?;
    if let Some(token) = json_string(&original, &["access_token"]).filter(|s| !s.is_empty())
        && !force_refresh
        && !credential_needs_refresh(&original)
    {
        return Ok((token, original, false));
    }
    let lock = subscription_refresh_lock(&path)?;
    let mut guard = lock.lock().await;
    guard
        .run(started, async {
            let latest = read_subscription_credential(&path)?;
            if let Some(token) = json_string(&latest, &["access_token"]).filter(|s| !s.is_empty()) {
                let changed = latest["access_token"] != original["access_token"];
                if !credential_needs_refresh(&latest) && (!force_refresh || changed) {
                    return Ok((token, latest, changed));
                }
            }
            let refresh = latest["refresh_token"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow!("Cursor credential requires a new sign-in"))?;
            let key = subscription_refresh_token_key("cursor", refresh);
            let token_lock = subscription_refresh_token_lock(&key);
            let mut token_guard = token_lock.lock().await;
            token_guard
                .run(started, async {
                    let cached = cached_subscription_refresh_result(&key)
                        .filter(|v| !force_refresh || v["access_token"] != latest["access_token"]);
                    let mut value = match cached {
                        Some(value) => value,
                        None => {
                            let value = crate::cursor::refresh_token(client, &latest).await?;
                            remember_subscription_refresh_result(key, value.clone());
                            value
                        }
                    };
                    copy_token_metadata(&latest, &mut value);
                    let token = value["access_token"]
                        .as_str()
                        .expect("normalized Cursor credential")
                        .to_string();
                    write_subscription_credential_atomically(&path, &value)?;
                    Ok((token, value, true))
                })
                .await
        })
        .await
}

struct Catalog {
    models: Vec<crate::model_catalog::ModelObservation>,
    refresh_after: Instant,
}
type Catalogs = Mutex<HashMap<String, Arc<tokio::sync::Mutex<Option<Catalog>>>>>;
static CATALOGS: OnceLock<Catalogs> = OnceLock::new();

pub(super) async fn quota(
    channel: &ChannelConfig,
    policy: SubscriptionCatalogPolicy,
) -> Result<Vec<QuotaWindow>> {
    refresh_subscription_quota(channel, policy, async {
        let client = crate::cursor::http_client()?;
        let (mut token, _, _) =
            ensure_cursor_subscription_access_token(&client, channel, false).await?;
        let mut result = crate::cursor::quota::fetch(&client, &token).await;
        if result
            .as_ref()
            .err()
            .and_then(|e| e.downcast_ref::<crate::cursor::ApiError>())
            .is_some_and(|e| e.status == 401)
        {
            (token, _, _) = ensure_cursor_subscription_access_token(&client, channel, true).await?;
            result = crate::cursor::quota::fetch(&client, &token).await;
        }
        result
    })
    .await
}

pub(super) async fn catalog(
    channel: &ChannelConfig,
    policy: SubscriptionCatalogPolicy,
) -> Result<(
    Vec<crate::model_catalog::ModelObservation>,
    bool,
    Vec<String>,
)> {
    let key = format!(
        "{}\0{}",
        channel.id,
        subscription_credential_path(channel)?.display()
    );
    let slot = CATALOGS
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| anyhow!("Cursor catalog cache poisoned"))?
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
    let client = crate::cursor::http_client()?;
    let mut refreshed = false;
    let result = async {
        let (mut token, _, did_refresh) =
            ensure_cursor_subscription_access_token(&client, channel, false).await?;
        refreshed = did_refresh;
        let mut result = crate::cursor::catalog(&client, &token).await;
        if result
            .as_ref()
            .err()
            .and_then(|e| e.downcast_ref::<crate::cursor::ApiError>())
            .is_some_and(|e| e.status == 401)
        {
            (token, _, _) = ensure_cursor_subscription_access_token(&client, channel, true).await?;
            refreshed = true;
            result = crate::cursor::catalog(&client, &token).await;
        }
        result
    }
    .await;
    match result {
        Ok(models) => {
            *cached = Some(Catalog {
                models: models.clone(),
                refresh_after: Instant::now() + Duration::from_secs(300),
            });
            Ok((models, refreshed, Vec::new()))
        }
        Err(error)
            if policy == SubscriptionCatalogPolicy::Cached
                && (subscription_catalog_error_allows_stale(&error)
                    || error
                        .downcast_ref::<crate::cursor::ApiError>()
                        .is_some_and(|e| e.status == 429 || e.status >= 500)) =>
        {
            if let Some(value) = cached.as_mut() {
                value.refresh_after = Instant::now() + Duration::from_secs(60);
                Ok((
                    value.models.clone(),
                    refreshed,
                    vec![format!(
                        "Cursor model refresh unavailable; keeping the last account catalog: {error}"
                    )],
                ))
            } else {
                Err(error)
            }
        }
        Err(error) => Err(error),
    }
}
