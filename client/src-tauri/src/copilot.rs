//! Copilot account authentication; inference keeps the shared native HTTP path.
//! The GitHub credential is restricted to GitHub auth/account endpoints. Only the short
//! service token is sent to the account-specific Copilot API origin.
use crate::{
    model::ChannelConfig, source_driver::SourceDriverId,
    upstream_transport::AdaptiveRequestBuilderExt,
};
use anyhow::{Context, Result, anyhow, ensure};
use reqwest::{
    Client, RequestBuilder, Url,
    header::{AUTHORIZATION, HeaderMap, HeaderValue},
};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

const CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
const TOKEN_URL: &str = "https://api.github.com/copilot_internal/v2/token";
const API_BASE: &str = "https://api.githubcopilot.com";
// GitHub account endpoints and the Copilot inference API have separate versions.
const ACCOUNT_API_VERSION: &str = "2025-04-01";
static CLIENT: OnceLock<crate::http_client::HttpClient> = OnceLock::new();
type TokenSlot = Arc<tokio::sync::Mutex<TokenCache>>;
static TOKENS: OnceLock<Mutex<HashMap<String, TokenSlot>>> = OnceLock::new();

#[derive(Clone)]
struct ServiceToken {
    token: String,
    origin: Url,
    issued_at: Instant,
    refresh_at: Instant,
}

#[derive(Default)]
struct TokenCache {
    value: Option<ServiceToken>,
    refresh: crate::detection::subscription_refresh::RefreshState,
}

fn client() -> Result<Client> {
    Ok(crate::http_client::cached_client(&CLIENT, || {
        Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
    })?)
}

fn slot(credential: &str) -> Result<TokenSlot> {
    let key = hex::encode(Sha256::digest(credential.as_bytes()));
    Ok(TOKENS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| anyhow!("Copilot token cache poisoned"))?
        .entry(key)
        .or_default()
        .clone())
}

fn api_origin(value: &str) -> Result<Url> {
    let url = Url::parse(value).context("invalid Copilot API origin")?;
    ensure!(
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && matches!(url.path(), "" | "/")
            && url.query().is_none()
            && url.fragment().is_none()
            && url.host_str().is_some_and(|host| matches!(
                host,
                "api.githubcopilot.com"
                    | "api.individual.githubcopilot.com"
                    | "api.business.githubcopilot.com"
                    | "api.enterprise.githubcopilot.com"
            )),
        "Copilot returned an unsupported API origin; update the channel adapter"
    );
    Ok(url)
}

fn service_token(value: &Value, now: Instant) -> Result<ServiceToken> {
    let token = value["token"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow!("Copilot token response is missing its service token"))?;
    HeaderValue::from_str(&format!("Bearer {token}")).context("invalid Copilot service token")?;
    // refresh_in is relative and does not depend on the machine's wall clock.
    let lifetime = value["refresh_in"]
        .as_u64()
        .filter(|n| *n > 0)
        .or_else(|| {
            let expiry = value["expires_at"].as_i64()?;
            u64::try_from(expiry - crate::now_unix())
                .ok()
                .filter(|n| *n > 0)
        })
        .ok_or_else(|| anyhow!("Copilot token response has no usable expiry"))?;
    let origin = api_origin(
        value
            .pointer("/endpoints/api")
            .and_then(Value::as_str)
            .unwrap_or(API_BASE),
    )?;
    let lifetime = lifetime.min(24 * 60 * 60);
    Ok(ServiceToken {
        token: token.into(),
        origin,
        issued_at: now,
        refresh_at: now + Duration::from_secs(lifetime.saturating_sub(30).max(1)),
    })
}

fn client_headers(headers: &mut HeaderMap) {
    headers.insert(
        "user-agent",
        HeaderValue::from_static(concat!("const-api/", env!("CARGO_PKG_VERSION"))),
    );
    // Keep this pair aligned with the same stable VS Code release.
    headers.insert("editor-version", HeaderValue::from_static("vscode/1.142.0"));
    headers.insert(
        "editor-plugin-version",
        HeaderValue::from_static("copilot-chat/0.70.0"),
    );
    headers.insert(
        "copilot-integration-id",
        HeaderValue::from_static("vscode-chat"),
    );
}

async fn exchange_at(client: &Client, url: &str, credential: &str) -> Result<ServiceToken> {
    let mut headers = HeaderMap::new();
    client_headers(&mut headers);
    headers.insert(
        "x-github-api-version",
        HeaderValue::from_static(ACCOUNT_API_VERSION),
    );
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("token {credential}"))?,
    );
    headers.insert("accept", HeaderValue::from_static("application/json"));
    let response = client.get(url).headers(headers).send_adaptive().await?;
    let status = response.status();
    let parsed = response.json::<Value>().await;
    if !status.is_success() {
        let value = parsed.unwrap_or(Value::Null);
        let detail = value
            .pointer("/error_details/message")
            .or_else(|| value.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("Copilot token exchange was rejected");
        return Err(
            crate::subscription_oauth_error::SubscriptionOAuthError::from_response(
                "GitHub Copilot",
                status.as_u16(),
                &serde_json::json!({"error_description":
                crate::upstream_failure::safe_excerpt(&detail.replace(credential, "[redacted]"))}),
            )
            .into(),
        );
    }
    let value = parsed.context("Copilot token endpoint returned invalid JSON")?;
    service_token(&value, Instant::now())
}

async fn token(credential: &str) -> Result<ServiceToken> {
    token_at(credential, &client()?, TOKEN_URL).await
}

async fn token_at(credential: &str, client: &Client, url: &str) -> Result<ServiceToken> {
    let credential = credential.trim();
    ensure!(
        !credential.is_empty(),
        "Sign in to GitHub Copilot before checking this channel"
    );
    let started = Instant::now();
    let cache = slot(credential)?;
    let mut cache = cache.lock().await;
    if let Some(token) = &cache.value {
        if token.refresh_at > started {
            return Ok(token.clone());
        }
    }
    let token = cache
        .refresh
        .run(started, exchange_at(client, url, credential))
        .await?;
    cache.value = Some(token.clone());
    Ok(token)
}

/// Restrict subscription credentials to the operations this adapter implements.
fn wire_path(path: &str) -> Result<&str> {
    match path {
        "/models" | "/v1/models" => Ok("/models"),
        "/chat/completions" | "/v1/chat/completions" => Ok("/chat/completions"),
        "/responses" | "/v1/responses" => Ok("/responses"),
        "/messages" | "/v1/messages" => Ok("/v1/messages"),
        _ => Err(anyhow!(
            "Copilot does not expose this API operation: {path}"
        )),
    }
}

fn is_tool_continuation(value: &Value) -> bool {
    if let Some(last) = value["messages"].as_array().and_then(|items| items.last()) {
        return last["role"] == "tool"
            || (last["role"] == "user"
                && last["content"].as_array().is_some_and(|parts| {
                    !parts.is_empty() && parts.iter().all(|part| part["type"] == "tool_result")
                }));
    }
    value["input"]
        .as_array()
        .and_then(|items| items.last())
        .is_some_and(|last| {
            matches!(
                last["type"].as_str(),
                Some("function_call_output" | "custom_tool_call_output")
            )
        })
}

fn authorize_request(request: &mut reqwest::Request, token: &ServiceToken) -> Result<()> {
    // The configured endpoint is fixed; no caller-controlled origin is accepted.
    api_origin(&format!(
        "{}://{}",
        request.url().scheme(),
        request.url().authority()
    ))?;
    let path = wire_path(request.url().path())?;
    let messages = path == "/v1/messages";
    let chat = path == "/chat/completions";
    let mut url = token.origin.join(path)?;
    url.set_query(request.url().query());
    *request.url_mut() = url;
    let body = request
        .body()
        .and_then(reqwest::Body::as_bytes)
        .and_then(|body| serde_json::from_slice::<Value>(body).ok());
    let continuation = body.as_ref().is_some_and(is_tool_continuation);
    let chat_image = chat
        && body.as_ref().is_some_and(|body| {
            body["messages"].as_array().is_some_and(|messages| {
                messages.iter().any(|message| {
                    message["content"]
                        .as_array()
                        .is_some_and(|parts| parts.iter().any(|part| part["type"] == "image_url"))
                })
            })
        });
    let headers = request.headers_mut();
    for name in [
        "authorization",
        "x-api-key",
        "api-key",
        "cookie",
        "x-initiator",
        "x-request-id",
    ] {
        headers.remove(name);
    }
    client_headers(headers);
    headers.insert(
        "x-github-api-version",
        HeaderValue::from_static("2026-01-09"),
    );
    if chat_image {
        headers.insert("copilot-vision-request", HeaderValue::from_static("true"));
    }
    if messages {
        headers
            .entry("anthropic-version")
            .or_insert(HeaderValue::from_static("2023-06-01"));
    }
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", token.token))?,
    );
    headers.insert(
        "openai-intent",
        HeaderValue::from_static("conversation-edits"),
    );
    headers.insert(
        "x-initiator",
        HeaderValue::from_static(if continuation { "agent" } else { "user" }),
    );
    headers.insert(
        "x-request-id",
        HeaderValue::from_str(&format!("{:032x}", rand::random::<u128>()))?,
    );
    Ok(())
}

pub(crate) async fn prepare_request(
    channel: &ChannelConfig,
    request: RequestBuilder,
) -> Result<RequestBuilder> {
    Ok(prepare_request_with_generation(channel, request).await?.0)
}

pub(crate) async fn prepare_request_with_generation(
    channel: &ChannelConfig,
    request: RequestBuilder,
) -> Result<(RequestBuilder, Option<Instant>)> {
    if channel.source_driver() != SourceDriverId::CopilotSubscription {
        return Ok((request, None));
    }
    let (client, request) = request.build_split();
    let mut request = request?;
    wire_path(request.url().path())?;
    let token = token(&channel.v2.credential_ref).await?;
    authorize_request(&mut request, &token)?;
    Ok((
        RequestBuilder::from_parts(client, request),
        Some(token.issued_at),
    ))
}

pub(crate) async fn observe_response(
    channel: &ChannelConfig,
    status: reqwest::StatusCode,
    generation: Option<Instant>,
) {
    if generation.is_some() && status == reqwest::StatusCode::UNAUTHORIZED {
        if let Ok(cache) = slot(channel.v2.credential_ref.trim()) {
            let mut cache = cache.lock().await;
            if cache.value.as_ref().map(|token| token.issued_at) == generation {
                cache.value = None;
            }
        }
    }
}

pub(crate) async fn catalog(
    client: &Client,
    channel: &ChannelConfig,
) -> Result<Vec<crate::model_catalog::ModelObservation>> {
    let (request, generation) =
        prepare_request_with_generation(channel, client.get(format!("{API_BASE}/models"))).await?;
    let response = request.send_adaptive().await?;
    let status = response.status();
    observe_response(channel, status, generation).await;
    let response = response
        .error_for_status()
        .context("Copilot model catalog rejected the account")?;
    let value: Value = response
        .json()
        .await
        .context("invalid Copilot model catalog")?;
    let models = crate::model_catalog::model_observations_from_value(
        crate::model_catalog::ModelCatalogKind::Copilot,
        &value,
        "github_copilot",
    );
    ensure!(
        !models.is_empty(),
        "Copilot account returned no enabled chat models"
    );
    Ok(models)
}

pub(crate) async fn quota(channel: &ChannelConfig) -> Result<Vec<crate::model::QuotaWindow>> {
    let response = client()?
        .get("https://api.github.com/copilot_internal/user")
        .header(
            AUTHORIZATION,
            format!("token {}", channel.v2.credential_ref.trim()),
        )
        .header("accept", "application/json")
        .header("x-github-api-version", ACCOUNT_API_VERSION)
        .header(
            "user-agent",
            concat!("const-api/", env!("CARGO_PKG_VERSION")),
        )
        .send_adaptive()
        .await?;
    let value = crate::detection::quota_response_json(response, "Copilot quota").await?;
    parse_quota(&value, crate::now_unix())
}

fn parse_quota(value: &Value, now: i64) -> Result<Vec<crate::model::QuotaWindow>> {
    let snapshots = value["quota_snapshots"]
        .as_object()
        .ok_or_else(|| anyhow!("Copilot quota response has no recognized quota snapshots"))?;
    let reset = value["quota_reset_date"]
        .as_str()
        .and_then(|text| {
            time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339)
                .ok()
                .map(|date| date.unix_timestamp())
        })
        .unwrap_or(0);
    let mut windows = Vec::new();
    for (name, snapshot) in snapshots {
        if snapshot["unlimited"].as_bool() == Some(true) {
            continue;
        }
        let ratio = snapshot["percent_remaining"]
            .as_f64()
            .map(|percent| percent / 100.0)
            .or_else(|| Some(snapshot["remaining"].as_f64()? / snapshot["entitlement"].as_f64()?));
        if let Some(ratio) = ratio.filter(|ratio| ratio.is_finite() && *ratio >= 0.0) {
            windows.push(crate::model::QuotaWindow {
                source: "copilot_usage".into(),
                window: format!("{name}:monthly"),
                remaining_ratio: ratio.clamp(0.0, 1.0),
                used_percent: (1.0 - ratio.clamp(0.0, 1.0)) * 100.0,
                reset_at_unix: reset,
                checked_at_unix: now,
                model: None,
                token_type: None,
            });
        }
    }
    // Buckets are advisory: premium exhaustion must not disable included models
    // or permitted overage. Do not reinterpret these changing units as token fees.
    Ok(windows)
}

#[derive(Debug, Serialize)]
pub(crate) struct AuthError {
    code: &'static str,
    status: Option<u16>,
}

#[derive(Serialize)]
pub(crate) struct DeviceSession {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_at_unix_ms: i64,
    interval_seconds: u64,
}

#[derive(Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum PollResult {
    Pending,
    SlowDown,
    Retry { after_seconds: u64 },
    Authorized { api_key: String },
}

fn auth_error(code: &'static str) -> AuthError {
    AuthError { code, status: None }
}

async fn auth_json(request: RequestBuilder) -> std::result::Result<(u16, Value), AuthError> {
    let response = request
        .header("accept", "application/json")
        .header(
            "user-agent",
            concat!("const-api/", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await
        .map_err(|error| {
            crate::http_client::observe_error(&error);
            auth_error("network")
        })?;
    let status = response.status().as_u16();
    if status == 429 || status >= 500 {
        return Ok((status, Value::Null));
    }
    let value = response.json().await.map_err(|_| AuthError {
        code: "invalid_response",
        status: Some(status),
    })?;
    Ok((status, value))
}

#[tauri::command]
pub(crate) async fn create_copilot_auth_session() -> std::result::Result<DeviceSession, AuthError> {
    let client = client().map_err(|_| auth_error("network"))?;
    let (status, value) = auth_json(
        client
            .post("https://github.com/login/device/code")
            .form(&[("client_id", CLIENT_ID), ("scope", "read:user user:email")]),
    )
    .await?;
    if status != 200 {
        return Err(AuthError {
            code: "upstream_error",
            status: Some(status),
        });
    }
    let field = |name| {
        value[name]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .ok_or_else(|| auth_error("invalid_response"))
    };
    let verification_uri = field("verification_uri")?;
    if verification_uri != "https://github.com/login/device" {
        return Err(auth_error("invalid_response"));
    }
    let expires = value["expires_in"]
        .as_u64()
        .filter(|v| *v > 0 && *v <= 1800)
        .ok_or_else(|| auth_error("invalid_response"))?;
    Ok(DeviceSession {
        device_code: field("device_code")?,
        user_code: field("user_code")?,
        verification_uri,
        expires_at_unix_ms: crate::now_unix() * 1000 + expires as i64 * 1000,
        interval_seconds: value["interval"].as_u64().unwrap_or(5).clamp(5, 60),
    })
}

fn poll_result(status: u16, value: &Value) -> std::result::Result<PollResult, AuthError> {
    if status == 429 || status >= 500 {
        return Ok(PollResult::Retry { after_seconds: 15 });
    }
    if status != 200 {
        return Err(AuthError {
            code: "upstream_error",
            status: Some(status),
        });
    }
    match value["error"].as_str() {
        Some("authorization_pending") => Ok(PollResult::Pending),
        Some("slow_down") => Ok(PollResult::SlowDown),
        Some("expired_token") => Err(auth_error("expired")),
        Some("access_denied") => Err(auth_error("denied")),
        Some(_) => Err(auth_error("upstream_error")),
        None => value["access_token"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|key| PollResult::Authorized {
                api_key: key.to_string(),
            })
            .ok_or_else(|| auth_error("invalid_response")),
    }
}

#[tauri::command]
pub(crate) async fn poll_copilot_auth_session(
    device_code: String,
) -> std::result::Result<PollResult, AuthError> {
    if device_code.trim().is_empty() || device_code.len() > 2048 {
        return Err(auth_error("invalid_response"));
    }
    let client = client().map_err(|_| auth_error("network"))?;
    let (status, value) = auth_json(
        client
            .post("https://github.com/login/oauth/access_token")
            .form(&[
                ("client_id", CLIENT_ID),
                ("device_code", device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ]),
    )
    .await?;
    poll_result(status, &value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use warp::Filter;

    fn test_token(origin: &str) -> ServiceToken {
        ServiceToken {
            token: "short-service-secret".into(),
            origin: Url::parse(origin).unwrap(),
            issued_at: Instant::now(),
            refresh_at: Instant::now() + Duration::from_secs(300),
        }
    }

    #[test]
    fn token_origin_and_relative_expiry_are_validated_without_decoding_the_token() {
        let now = Instant::now();
        let valid = json!({"token":"opaque-hmac;not.a.jwt", "refresh_in":1800,
            "expires_at":1, "endpoints":{"api":"https://api.individual.githubcopilot.com"}});
        let parsed = service_token(&valid, now).unwrap();
        assert_eq!(parsed.refresh_at.duration_since(now).as_secs(), 1770);
        assert_eq!(parsed.token, "opaque-hmac;not.a.jwt");
        for invalid in [
            "http://api.githubcopilot.com",
            "https://api.githubcopilot.com.evil.test",
            "https://user@api.githubcopilot.com",
            "https://api.githubcopilot.com/private",
            "https://api.githubcopilot.com?credential=x",
            "https://localhost",
        ] {
            assert!(api_origin(invalid).is_err(), "{invalid}");
        }
        assert!(service_token(&json!({"token":"abc"}), now).is_err());
    }

    #[test]
    fn authorization_preserves_body_and_query_but_never_forwards_github_credentials() {
        let body = r#"{"model":"claude-sonnet-5.5","messages":[{"role":"user","content":"hello"}],"unknown_future_field":true}"#;
        let mut request = Client::new()
            .post(format!("{API_BASE}/v1/messages?beta=true"))
            .header("authorization", "Bearer long-github-secret")
            .header("x-api-key", "long-github-secret")
            .header("cookie", "private=value")
            .header("x-initiator", "agent")
            .header("anthropic-beta", "future-beta")
            .header("x-github-api-version", "2025-04-01")
            .body(body)
            .build()
            .unwrap();
        authorize_request(
            &mut request,
            &test_token("https://api.individual.githubcopilot.com"),
        )
        .unwrap();
        assert_eq!(
            request.url().as_str(),
            "https://api.individual.githubcopilot.com/v1/messages?beta=true"
        );
        assert_eq!(request.body().unwrap().as_bytes().unwrap(), body.as_bytes());
        assert_eq!(
            request.headers()["authorization"],
            "Bearer short-service-secret"
        );
        assert_eq!(request.headers()["x-initiator"], "user");
        assert_eq!(request.headers()["anthropic-beta"], "future-beta");
        assert_eq!(request.headers()["editor-version"], "vscode/1.142.0");
        assert_eq!(
            request.headers()["editor-plugin-version"],
            "copilot-chat/0.70.0"
        );
        assert_eq!(request.headers()["copilot-integration-id"], "vscode-chat");
        assert_eq!(request.headers()["x-github-api-version"], "2026-01-09");
        assert!(!request.headers().contains_key("x-api-key"));
        assert!(!request.headers().contains_key("cookie"));
        assert!(wire_path("/v1/files").is_err());
    }

    #[test]
    fn initiator_is_based_on_the_current_turn_not_old_tool_messages() {
        for (body, expected) in [
            (
                json!({"messages":[{"role":"tool","content":"done"},{"role":"user","content":"new question"}]}),
                false,
            ),
            (json!({"messages":[{"role":"tool","content":"done"}]}), true),
            (
                json!({"messages":[{"role":"user","content":[{"type":"tool_result","tool_use_id":"a","content":"ok"}]}]}),
                true,
            ),
            (
                json!({"messages":[{"role":"user","content":[{"type":"tool_result"},{"type":"text","text":"new question"}]}]}),
                false,
            ),
            (
                json!({"input":[{"type":"function_call_output","call_id":"a","output":"ok"}]}),
                true,
            ),
            (
                json!({"input":[{"type":"function_call_output"},{"role":"user","content":"new question"}]}),
                false,
            ),
        ] {
            assert_eq!(is_tool_continuation(&body), expected);
        }
    }

    #[test]
    fn device_poll_obeys_oauth_errors_even_when_http_is_successful() {
        assert!(matches!(
            poll_result(200, &json!({"error":"authorization_pending"})),
            Ok(PollResult::Pending)
        ));
        assert!(matches!(
            poll_result(200, &json!({"error":"slow_down"})),
            Ok(PollResult::SlowDown)
        ));
        assert_eq!(
            poll_result(200, &json!({"error":"access_denied"}))
                .err()
                .unwrap()
                .code,
            "denied"
        );
        assert!(matches!(
            poll_result(502, &Value::Null),
            Ok(PollResult::Retry { .. })
        ));
        assert!(poll_result(200, &json!({})).is_err());
    }

    #[test]
    fn model_catalog_respects_explicit_protocols_and_does_not_import_byok() {
        let models = crate::model_catalog::model_observations_from_value(
            crate::model_catalog::ModelCatalogKind::Copilot,
            &json!({"data":[
                {"id":"claude-future", "supported_endpoints":["/v1/messages"], "model_picker_enabled":false,
                    "capabilities":{"type":"chat","limits":{"max_context_window_tokens":1000000,"max_output_tokens":8192},
                    "supports":{"vision":true,"tool_calls":true}}},
                {"id":"not-a-claude-name","supported_endpoints":["/responses"],"capabilities":{"type":"chat"}},
                {"id":"custom-claude", "custom_model":{"key_name":"my-key","owner_name":"org"}},
                {"id":"legacy-custom-claude", "custom_model":true},
                {"id":"disabled-claude", "policy":{"state":"disabled"}},
                {"id":"embedding", "capabilities":{"type":"embeddings"}}
            ]}),
            "copilot",
        );
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].context_tokens, Some(1000000));
        assert_eq!(models[0].output_tokens, Some(8192));
        assert_eq!(models[0].input_modalities, ["text", "image"]);
        assert_eq!(
            crate::coding_gateway::catalog_supports_protocol(&models[0], "anthropic_messages"),
            Some(true)
        );
        assert_eq!(
            crate::coding_gateway::catalog_supports_protocol(&models[0], "openai_chat"),
            Some(false)
        );
    }

    #[test]
    fn quota_is_not_invented_or_allowed_to_disable_other_buckets() {
        assert!(parse_quota(&json!({}), 1).is_err());
        let windows = parse_quota(
            &json!({"quota_snapshots":{
                "chat":{"unlimited":true}, "missing":{},
                "premium_interactions":{"percent_remaining":0,"overage_permitted":true}
            }}),
            1,
        )
        .unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].remaining_ratio, 0.0);
        assert!(!crate::supplier::quota_window_limits_supply(&windows[0]));
    }

    #[tokio::test]
    async fn catalog_refresh_only_gets_models_with_the_service_token() {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let route = warp::method()
            .and(warp::path::full())
            .and(warp::header::headers_cloned())
            .map(move |method: warp::http::Method, path: warp::path::FullPath, headers: HeaderMap| {
                assert_eq!(method, warp::http::Method::GET);
                assert_eq!(path.as_str(), "/models");
                assert_eq!(headers["authorization"], "Bearer short-service-secret");
                assert_eq!(headers["x-github-api-version"], "2026-01-09");
                assert!(!headers.contains_key("x-api-key"));
                seen.fetch_add(1, Ordering::SeqCst);
                warp::reply::json(&json!({"data":[
                    {"id":"claude-test","supported_endpoints":["/v1/messages"],"capabilities":{"type":"chat"}},
                    {"id":"disabled","policy":{"state":"disabled"}},
                    {"id":"byok","custom_model":{"key_name":"private-key"}}
                ]}))
            });
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(warp::serve(route).incoming(listener).run());
        let mut channel =
            crate::coding_gateway::tests::channel(SourceDriverId::CopilotSubscription, API_BASE);
        let key = format!("github-test-{}", rand::random::<u64>());
        channel.v2.credential_ref = key.clone();
        channel.upstream_api_key = key.clone();
        slot(&key).unwrap().lock().await.value = Some(test_token(&origin));
        let models = catalog(&Client::new(), &channel).await.unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "claude-test");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn model_checks_use_service_auth_and_catalog_dialects_with_bounded_attempts() {
        let seen = Arc::new(AtomicUsize::new(0));
        let calls = seen.clone();
        let route = warp::post()
            .and(warp::path::full())
            .and(warp::header::headers_cloned())
            .and(warp::body::json())
            .map(move |path: warp::path::FullPath, headers: HeaderMap, value: Value| {
                assert_eq!(headers["authorization"], "Bearer short-service-secret");
                assert!(!headers.contains_key("x-api-key"));
                assert_eq!(headers["x-initiator"], "user");
                calls.fetch_add(1, Ordering::SeqCst);
                let response = match value["model"].as_str().unwrap() {
                    "chat-test" => {
                        assert_eq!(path.as_str(), "/chat/completions");
                        assert!(value["messages"].is_array());
                        json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"OK"}}]})
                    }
                    "claude-test" => {
                        assert_eq!(path.as_str(), "/v1/messages");
                        assert_eq!(headers["anthropic-version"], "2023-06-01");
                        assert!(value["max_tokens"].is_number());
                        json!({"type":"message","role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"OK"}]})
                    }
                    "responses-test" => {
                        assert_eq!(path.as_str(), "/responses");
                        assert_eq!(value["input"], "Reply OK.");
                        json!({"object":"response","status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"OK"}]}]})
                    }
                    model => panic!("unexpected model: {model}"),
                };
                warp::reply::json(&response)
            });
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(warp::serve(route).incoming(listener).run());
        let mut channel =
            crate::coding_gateway::tests::channel(SourceDriverId::CopilotSubscription, API_BASE);
        let key = format!("github-probe-test-{}", rand::random::<u64>());
        channel.v2.credential_ref = key.clone();
        channel.upstream_api_key = key.clone();
        slot(&key).unwrap().lock().await.value = Some(test_token(&origin));
        for (model, supported) in [
            ("chat-test", "openai_chat"),
            ("claude-test", "anthropic_messages"),
            ("responses-test", "openai_responses"),
        ] {
            channel.models = vec![model.into()];
            channel.capability_profiles = ["openai_chat", "anthropic_messages", "openai_responses"]
                .into_iter()
                .map(|protocol| crate::model::ChannelCapabilityProfile {
                    protocol: protocol.into(),
                    model_pattern: model.into(),
                    catalog_metadata: true,
                    catalog_protocol_supported: Some(protocol == supported),
                    ..Default::default()
                })
                .collect();
            let (result, attempts) = crate::upstream_transport::with_probe_budget(
                1,
                crate::supplier::test_channel_upstream_model_inner(
                    channel.clone(),
                    model.into(),
                    "Reply OK.".into(),
                ),
            )
            .await;
            let result = result.unwrap();
            assert_eq!(result.http_status, 200);
            assert_eq!(result.content, "OK");
            assert_eq!(attempts, 1);
        }
        let (result, attempts) = crate::upstream_transport::with_probe_budget(
            0,
            crate::supplier::test_channel_upstream_model_inner(
                channel,
                "responses-test".into(),
                "Reply OK.".into(),
            ),
        )
        .await;
        assert!(result.unwrap_err().contains("probe attempt budget"));
        assert_eq!(attempts, 0);
        assert_eq!(seen.load(Ordering::SeqCst), 3);
        server.abort();
    }

    #[tokio::test]
    async fn rejected_model_check_expires_service_token_without_replaying_inference() {
        let seen = Arc::new(AtomicUsize::new(0));
        let calls = seen.clone();
        let route = warp::post().map(move || {
            calls.fetch_add(1, Ordering::SeqCst);
            warp::reply::with_status(
                "service token expired",
                warp::http::StatusCode::UNAUTHORIZED,
            )
        });
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(warp::serve(route).incoming(listener).run());
        let mut channel =
            crate::coding_gateway::tests::channel(SourceDriverId::CopilotSubscription, API_BASE);
        let key = format!("github-probe-401-test-{}", rand::random::<u64>());
        channel.v2.credential_ref = key.clone();
        channel.upstream_api_key = key.clone();
        slot(&key).unwrap().lock().await.value = Some(test_token(&origin));
        let error = crate::supplier::test_channel_upstream_model_inner(
            channel,
            "chat-test".into(),
            "OK".into(),
        )
        .await
        .unwrap_err();
        assert!(error.contains("HTTP 401"));
        assert!(!error.contains(&key));
        assert!(slot(&key).unwrap().lock().await.value.is_none());
        assert_eq!(seen.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn concurrent_requests_share_one_token_exchange() {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let route =
            warp::get()
                .and(warp::header::headers_cloned())
                .map(move |headers: HeaderMap| {
                    assert!(
                        headers["authorization"]
                            .to_str()
                            .unwrap()
                            .starts_with("token github-test-")
                    );
                    assert_eq!(headers["editor-version"], "vscode/1.142.0");
                    assert_eq!(headers["editor-plugin-version"], "copilot-chat/0.70.0");
                    assert_eq!(headers["x-github-api-version"], "2025-04-01");
                    seen.fetch_add(1, Ordering::SeqCst);
                    warp::reply::json(
                        &json!({"token":"short", "refresh_in":1800, "endpoints":{"api":API_BASE}}),
                    )
                });
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let url = format!("http://{}/token", listener.local_addr().unwrap());
        let server = tokio::spawn(warp::serve(route).incoming(listener).run());
        let key = format!("github-test-{}", rand::random::<u64>());
        let client = Client::new();
        let (a, b, c) = tokio::join!(
            token_at(&key, &client, &url),
            token_at(&key, &client, &url),
            token_at(&key, &client, &url)
        );
        assert!(a.is_ok() && b.is_ok() && c.is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        slot(&key)
            .unwrap()
            .lock()
            .await
            .value
            .as_mut()
            .unwrap()
            .refresh_at = Instant::now();
        token_at(&key, &client, &url).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        server.abort();
    }

    #[tokio::test]
    async fn token_rejections_keep_status_and_redact_credentials() {
        let credential = "github-test-private-credential";
        let route = warp::get()
            .and(warp::path::param::<u16>())
            .map(move |status| {
                let body = if status == 503 {
                    "temporarily unavailable".to_string()
                } else {
                    json!({"error_details":{"message":format!("rejected {credential}")}})
                        .to_string()
                };
                warp::reply::with_status(body, warp::http::StatusCode::from_u16(status).unwrap())
            });
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(warp::serve(route).incoming(listener).run());
        for (status, kind) in [
            (401, "subscription_credential_expired"),
            (403, "subscription_oauth_permission_denied"),
            (429, "subscription_oauth_rate_limited"),
            (503, "subscription_oauth_unavailable"),
        ] {
            let error = exchange_at(&Client::new(), &format!("{origin}/{status}"), credential)
                .await
                .err()
                .unwrap();
            let wire =
                crate::subscription_oauth_error::oauth_wire_error_from_anyhow(&error).unwrap();
            assert_eq!(wire.status, status);
            assert_eq!(wire.error_type, kind);
            assert!(!wire.message.contains(credential));
        }
        server.abort();
    }

    #[tokio::test]
    async fn failed_token_exchange_is_shared_and_the_next_attempt_can_recover() {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let route = warp::get().and_then(move || {
            let attempt = seen.fetch_add(1, Ordering::SeqCst);
            async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                let (value, status) = if attempt == 0 {
                    (
                        json!({"message":"temporarily unavailable"}),
                        warp::http::StatusCode::SERVICE_UNAVAILABLE,
                    )
                } else {
                    (
                        json!({"token":"short", "refresh_in":1800}),
                        warp::http::StatusCode::OK,
                    )
                };
                Ok::<_, std::convert::Infallible>(warp::reply::with_status(
                    warp::reply::json(&value),
                    status,
                ))
            }
        });
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let url = format!("http://{}/token", listener.local_addr().unwrap());
        let server = tokio::spawn(warp::serve(route).incoming(listener).run());
        let key = format!("github-test-{}", rand::random::<u64>());
        let client = Client::new();
        let (a, b, c) = tokio::join!(
            token_at(&key, &client, &url),
            token_at(&key, &client, &url),
            token_at(&key, &client, &url)
        );
        for result in [a, b, c] {
            let error = result.err().unwrap();
            let wire =
                crate::subscription_oauth_error::oauth_wire_error_from_anyhow(&error).unwrap();
            assert_eq!(wire.status, 503);
            assert_eq!(wire.error_type, "subscription_oauth_unavailable");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        token_at(&key, &client, &url).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        server.abort();
    }

    #[tokio::test]
    async fn old_unauthorized_response_does_not_evict_a_refreshed_token() {
        let mut channel =
            crate::coding_gateway::tests::channel(SourceDriverId::CopilotSubscription, API_BASE);
        channel.v2.credential_ref = format!("github-test-{}", rand::random::<u64>());
        let old = test_token(API_BASE);
        let mut current = old.clone();
        current.issued_at = old.issued_at + Duration::from_millis(1);
        let generation = current.issued_at;
        let cache = slot(&channel.v2.credential_ref).unwrap();
        cache.lock().await.value = Some(current);
        observe_response(
            &channel,
            reqwest::StatusCode::UNAUTHORIZED,
            Some(old.issued_at),
        )
        .await;
        assert_eq!(
            cache.lock().await.value.as_ref().map(|t| t.issued_at),
            Some(generation)
        );
        observe_response(
            &channel,
            reqwest::StatusCode::UNAUTHORIZED,
            Some(generation),
        )
        .await;
        assert!(cache.lock().await.value.is_none());
    }

    #[test]
    fn chat_vision_header_follows_payload_without_claiming_other_wire_modes() {
        for (path, body, vision) in [
            (
                "/chat/completions",
                json!({"messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,test"}}]}]}),
                true,
            ),
            (
                "/chat/completions",
                json!({"messages":[{"role":"user","content":"hello"}]}),
                false,
            ),
            (
                "/v1/messages",
                json!({"messages":[{"role":"user","content":[{"type":"image","source":{"type":"base64","data":"test"}}]}]}),
                false,
            ),
        ] {
            let mut request = Client::new()
                .post(format!("{API_BASE}{path}"))
                .json(&body)
                .build()
                .unwrap();
            authorize_request(&mut request, &test_token(API_BASE)).unwrap();
            assert_eq!(
                request.headers().contains_key("copilot-vision-request"),
                vision
            );
            assert_eq!(
                serde_json::from_slice::<Value>(request.body().unwrap().as_bytes().unwrap())
                    .unwrap(),
                body
            );
        }
        assert!(wire_path("/v1/files").is_err());
        assert!(wire_path("/v1/realtime").is_err());
    }

    #[tokio::test]
    async fn shared_executor_preserves_claude_sse_tools_and_tool_continuation() {
        use crate::channel_executor::{ChannelRequest, execute_channel_request};
        let seen = Arc::new(AtomicUsize::new(0));
        let calls = seen.clone();
        let body = "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call_1\",\"name\":\"read\",\"input\":{}}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        let route = warp::post()
            .and(warp::path::full())
            .and(warp::header::headers_cloned())
            .and(warp::body::json())
            .map(
                move |path: warp::path::FullPath, headers: HeaderMap, value: Value| {
                    let turn = calls.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(path.as_str(), "/v1/messages");
                    assert_eq!(headers["authorization"], "Bearer short-service-secret");
                    assert!(!headers.contains_key("x-api-key"));
                    assert_eq!(
                        headers["x-initiator"],
                        if turn == 0 { "user" } else { "agent" }
                    );
                    assert_eq!(value["future_protocol_field"], "keep");
                    warp::reply::with_header(body, "content-type", "text/event-stream")
                },
            );
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(warp::serve(route).incoming(listener).run());
        let mut channel =
            crate::coding_gateway::tests::channel(SourceDriverId::CopilotSubscription, API_BASE);
        let key = format!("github-test-{}", rand::random::<u64>());
        channel.v2.credential_ref = key.clone();
        channel.upstream_api_key = key.clone();
        channel
            .capability_profiles
            .push(crate::model::ChannelCapabilityProfile {
                protocol: "anthropic_messages".into(),
                model_pattern: "claude-test".into(),
                catalog_metadata: true,
                catalog_protocol_supported: Some(true),
                ..Default::default()
            });
        slot(&key).unwrap().lock().await.value = Some(test_token(&origin));
        for content in [
            json!("hello"),
            json!([{"type":"tool_result","tool_use_id":"call_1","content":"ok"}]),
        ] {
            let request = ChannelRequest { method:reqwest::Method::POST, path:"/anthropic/v1/messages".into(), raw_query:String::new(),
                has_query:false, headers:HeaderMap::new(), body:serde_json::to_vec(&json!({"model":"claude-test", "stream":true,
                    "messages":[{"role":"user","content":content}], "future_protocol_field":"keep"})).unwrap().into(),
                stream_requested:Some(true), selected_upstream_model:Some("claude-test".into()), selected_target_protocol:None,
                cache_identity:None, safety_identifier:None };
            let response = execute_channel_request(&Client::new(), &channel, request)
                .await
                .unwrap();
            assert!(response.native_passthrough);
            assert_eq!(response.response.text().await.unwrap(), body);
        }
        assert_eq!(seen.load(Ordering::SeqCst), 2);
        server.abort();
    }
}
