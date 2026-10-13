//! Account-assigned Agent endpoints; privacy mode selects agent_url, not agentn.
use super::*;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};

struct Endpoint {
    url: String,
    refresh_at: Instant,
}
type Slot = Arc<tokio::sync::Mutex<Option<Endpoint>>>;
static ENDPOINTS: OnceLock<Mutex<HashMap<[u8; 32], Slot>>> = OnceLock::new();

pub(super) async fn agent_url(client: &Client, token: &str) -> Result<String> {
    let slot = {
        let mut cache = ENDPOINTS
            .get_or_init(Default::default)
            .lock()
            .map_err(|_| anyhow!("Cursor endpoint cache poisoned"))?;
        // Token rotation changes the key. Bound old generations without
        // evicting a lookup that is currently in flight.
        if cache.len() >= 128 {
            cache.retain(|_, slot| Arc::strong_count(slot) > 1);
        }
        cache
            .entry(Sha256::digest(token.as_bytes()).into())
            .or_default()
            .clone()
    };
    let mut endpoint = slot.lock().await;
    if let Some(value) = endpoint.as_ref()
        && value.refresh_at > Instant::now()
    {
        return Ok(value.url.clone());
    }
    let result = async {
        let response = send(
            headers(
                client.post(format!(
                    "{API}/aiserver.v1.ServerConfigService/GetServerConfig"
                )),
                token,
            )
            .header("content-type", "application/proto")
            .body(Vec::new())
            .timeout(Duration::from_secs(15)),
        )
        .await?;
        if !response.status().is_success() {
            return Err(rejection(response, &[token]).await?.into());
        }
        parse(&bounded_body(response).await?)
    }
    .await;
    match result {
        Ok(url) => {
            *endpoint = Some(Endpoint {
                url: url.clone(),
                refresh_at: Instant::now() + Duration::from_secs(3600),
            });
            Ok(url)
        }
        Err(error)
            if crate::detection::subscription_catalog_error_allows_stale(&error)
                || error
                    .downcast_ref::<ApiError>()
                    .is_some_and(|e| e.status == 429 || e.status >= 500) =>
        {
            if let Some(value) = endpoint.as_mut() {
                eprintln!(
                    "[const-api][cursor] endpoint refresh unavailable; retaining the assigned endpoint: {}",
                    crate::upstream_failure::safe_excerpt(&error.to_string())
                );
                value.refresh_at = Instant::now() + Duration::from_secs(60);
                Ok(value.url.clone())
            } else {
                Err(error)
            }
        }
        Err(error) => Err(error),
    }
}

pub(super) fn parse(raw: &[u8]) -> Result<String> {
    let fields = wire::fields(raw)?;
    let config = wire::fields(
        wire::data(&fields, 27)
            .ok_or_else(|| anyhow!("Cursor did not assign an Agent endpoint"))?,
    )?;
    let mut url =
        reqwest::Url::parse(&wire::string(&config, 1)?).context("invalid Cursor Agent endpoint")?;
    ensure!(
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none()
            && url
                .host_str()
                .is_some_and(|host| host == "api5.cursor.sh" || host.ends_with(".api5.cursor.sh")),
        "Cursor returned an unsupported Agent endpoint; update the channel adapter"
    );
    url.set_path("/agent.v1.AgentService/Run");
    Ok(url.to_string())
}
