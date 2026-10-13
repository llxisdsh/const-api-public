//! Cursor account credentials and metadata. Inference belongs to the isolated
//! Connect-RPC runtime; no SDK, CLI process or local tool execution is required.
use anyhow::{Context, Result, anyhow, ensure};
use base64::Engine;
use futures_util::StreamExt;
use reqwest::{Client, Response};
use serde_json::{Value, json};
use std::{sync::OnceLock, time::Duration};

pub(crate) mod auth;
mod endpoint;
pub(crate) mod models;
pub(crate) mod quota;
pub(crate) mod request;
pub(crate) mod runtime;
mod wire;

const API: &str = "https://api2.cursor.sh";
// Cursor Agent build, not the IDE's semver. Checked against the CLI installer.
pub(crate) const CLIENT_VERSION: &str = "cli-2026.10.01-e373342";

pub(crate) fn http_client() -> Result<Client> {
    static CLIENT: OnceLock<crate::http_client::HttpClient> = OnceLock::new();
    Ok(crate::http_client::cached_client(&CLIENT, || {
        Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
    })?)
}

pub(crate) fn headers(request: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    request
        .bearer_auth(token)
        .header("x-cursor-client-type", "cli")
        .header("x-cursor-client-version", CLIENT_VERSION)
        .header("x-ghost-mode", "true")
        .header("connect-protocol-version", "1")
        .header("user-agent", "connect-es/1.6.1")
        .header("x-request-id", uuid::Uuid::new_v4().to_string())
}

pub(crate) fn normalize_token(value: Value, previous: Option<&Value>) -> Result<Value> {
    let access = value["access_token"]
        .as_str()
        .or_else(|| value["accessToken"].as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("Cursor token is missing access_token"))?;
    let refresh = value["refresh_token"]
        .as_str()
        .or_else(|| value["refreshToken"].as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| previous.and_then(|p| p["refresh_token"].as_str()));
    reqwest::header::HeaderValue::from_str(&format!("Bearer {access}"))
        .context("invalid Cursor access token")?;
    let claims = access
        .split('.')
        .nth(1)
        .and_then(|s| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(s)
                .ok()
        })
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let mut normalized = json!({"provider":"cursor","type":"cursor","access_token":access});
    if let Some(refresh) = refresh {
        normalized["refresh_token"] = json!(refresh);
    }
    if let Some(expiry) = claims
        .as_ref()
        .and_then(|c| c["exp"].as_i64())
        .filter(|n| *n > 0)
    {
        normalized["expires_at_unix"] = json!(expiry);
    }
    if let Some(sub) = claims
        .as_ref()
        .and_then(|c| c["sub"].as_str())
        .filter(|s| !s.is_empty())
    {
        normalized["account_id"] = json!(sub);
    }
    Ok(normalized)
}

pub(crate) async fn refresh_token(client: &Client, credential: &Value) -> Result<Value> {
    let refresh = credential["refresh_token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("Cursor credential cannot refresh; sign in again"))?;
    let response = send(
        headers(
            client.post(format!("{API}/auth/exchange_user_api_key")),
            refresh,
        )
        .timeout(Duration::from_secs(30))
        .json(&json!({})),
    )
    .await?;
    if !response.status().is_success() {
        return Err(rejection(response, &[refresh]).await?.into());
    }
    let value = serde_json::from_slice(&bounded_body(response).await?)
        .context("invalid Cursor refresh response")?;
    normalize_token(value, Some(credential))
}

pub(crate) async fn catalog(
    client: &Client,
    token: &str,
) -> Result<Vec<crate::model_catalog::ModelObservation>> {
    catalog_at(
        client,
        token,
        &format!("{API}/agent.v1.AgentService/GetUsableModels"),
    )
    .await
}
async fn catalog_at(
    client: &Client,
    token: &str,
    url: &str,
) -> Result<Vec<crate::model_catalog::ModelObservation>> {
    let response = send(
        headers(client.post(url), token)
            .timeout(crate::MODEL_REQUEST_TIMEOUT)
            .header("content-type", "application/proto")
            .body(Vec::new()),
    )
    .await?;
    if !response.status().is_success() {
        return Err(rejection(response, &[token]).await?.into());
    }
    let raw = bounded_body(response).await?;
    let mut models = Vec::new();
    for (number, value) in wire::fields(&raw)? {
        if let (1, wire::ValueRef::Bytes(model)) = (number, value) {
            let fields = wire::fields(model)?;
            let id = wire::string(&fields, 1)?;
            ensure!(!id.trim().is_empty(), "Cursor catalog model has no ID");
            models.push(crate::model_catalog::ModelObservation {
                id,
                source: "cursor_account_catalog".into(),
                // GetUsableModels does not report context limits. Let the shared
                // catalog supply its fallback; never assert a guessed account limit.
                input_modalities: vec!["text".into()],
                output_modalities: vec!["text".into()],
                ..Default::default()
            });
        }
    }
    ensure!(!models.is_empty(), "Cursor account returned no models");
    Ok(crate::model_catalog::stable_dedupe_models(models))
}

pub(crate) async fn send(request: reqwest::RequestBuilder) -> Result<Response> {
    // Run is full-duplex HTTP/2; never switch protocols or replay a paid request.
    let response = request.send().await.map_err(|e| {
        crate::http_client::observe_error(&e);
        anyhow!(e.without_url())
    })?;
    crate::http_client::observe_response(&response);
    Ok(response)
}
pub(crate) async fn bounded_body(response: Response) -> Result<Vec<u8>> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| anyhow!(e.without_url()))?;
        ensure!(
            bytes.len() + chunk.len() <= wire::MAX_FRAME,
            "Cursor metadata exceeded size limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests;

#[derive(Debug)]
pub(crate) struct ApiError {
    pub(crate) status: u16,
    pub(crate) code: String,
    pub(crate) detail: String,
}
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Cursor HTTP {} [{}]: {}",
            self.status, self.code, self.detail
        )
    }
}
impl std::error::Error for ApiError {}
pub(crate) async fn rejection(response: Response, secrets: &[&str]) -> Result<ApiError> {
    let status = response.status().as_u16();
    let raw = bounded_body(response).await?;
    let value = serde_json::from_slice::<Value>(&raw).ok();
    let code = value
        .as_ref()
        .and_then(|v| v["code"].as_str())
        .unwrap_or("cursor_upstream_error")
        .to_string();
    let mut detail = value
        .as_ref()
        .and_then(|v| v["message"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| String::from_utf8_lossy(&raw).into_owned());
    for secret in secrets.iter().filter(|s| !s.is_empty()) {
        detail = detail.replace(secret, "[redacted]");
    }
    Ok(ApiError {
        status,
        code,
        detail: crate::upstream_failure::safe_excerpt(&detail),
    })
}
