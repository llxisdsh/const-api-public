//! Muse Code device sign-in mints a dedicated subscription key. Inference uses
//! the ordinary HTTP executor; DCA tokens never enter channel config or logs.
use reqwest::{Client, RequestBuilder, StatusCode};
use serde::Serialize;
use serde_json::Value;
use std::{sync::OnceLock, time::Duration};

const AUTHORIZATION_URL: &str = "https://auth.meta.com/oidc/device/authorization/";
const TOKEN_URL: &str = "https://auth.meta.com/oidc/device/token/";
const KEY_URL: &str = "https://api.meta.ai/muse-code/key";
const CLIENT_ID: &str = "1031625952748946";
const API_BASE: &str = "https://api.meta.ai/v1";
static CLIENT: OnceLock<crate::http_client::HttpClient> = OnceLock::new();

#[derive(Debug, Serialize)]
pub(crate) struct AuthError {
    code: &'static str,
    status: Option<u16>,
}

impl AuthError {
    fn new(code: &'static str) -> Self {
        Self { code, status: None }
    }
}

#[derive(Serialize)]
pub(crate) struct DeviceSession {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_at_unix_ms: i64,
    interval_seconds: u64,
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum PollResult {
    Pending,
    SlowDown,
    Retry { after_seconds: u64 },
    Authorized { api_key: String },
}

fn client() -> Result<Client, AuthError> {
    crate::http_client::cached_client(&CLIENT, || {
        Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            // A 307/308 must never forward the DCA token in the POST body.
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
    })
    .map_err(|_| AuthError::new("network"))
}

async fn json_response(request: RequestBuilder) -> Result<(StatusCode, Value, u64), AuthError> {
    let response = request
        .header("Accept", "application/json")
        .header(
            "User-Agent",
            concat!("const-api/", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await
        .map_err(|error| {
            crate::http_client::observe_error(&error);
            AuthError::new("network")
        })?;
    crate::http_client::observe_response(&response);
    let status = response.status();
    if status.is_redirection() {
        return Err(AuthError {
            code: "unexpected_endpoint",
            status: Some(status.as_u16()),
        });
    }
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(30);
    // Gateway/rate-limit failures need not contain JSON. Keep the polling loop
    // bounded by the device code lifetime, including during a network switch.
    if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
        return Ok((status, Value::Null, retry_after));
    }
    let body = response.json::<Value>().await.map_err(|_| AuthError {
        code: "invalid_response",
        status: Some(status.as_u16()),
    })?;
    Ok((status, body, retry_after))
}

fn upstream_error(status: StatusCode, body: &Value) -> AuthError {
    let error = body
        .get("error")
        .and_then(Value::as_str)
        .or_else(|| body.pointer("/error/code").and_then(Value::as_str));
    let code = match error {
        Some("access_denied") => "denied",
        Some("expired_token" | "invalid_grant") => "expired",
        Some("payment_required") => "payment_required",
        Some("unsupported_country" | "region_restricted" | "country_not_supported") => {
            "region_restricted"
        }
        _ => {
            let message = body
                .pointer("/error/message")
                .or_else(|| body.get("message"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_ascii_lowercase();
            if (message.contains("region") || message.contains("country"))
                && (message.contains("not available")
                    || message.contains("not supported")
                    || message.contains("restricted"))
            {
                "region_restricted"
            } else {
                "upstream_error"
            }
        }
    };
    // Error bodies can echo account details or credentials; return only a
    // recognized reason and HTTP status, never the raw response.
    AuthError {
        code,
        status: Some(status.as_u16()),
    }
}

#[tauri::command]
pub(crate) async fn create_muse_code_auth_session() -> Result<DeviceSession, AuthError> {
    start(&client()?, AUTHORIZATION_URL).await
}

async fn start(client: &Client, url: &str) -> Result<DeviceSession, AuthError> {
    let (status, body, _) =
        json_response(client.post(url).form(&[("client_id", CLIENT_ID)])).await?;
    if !status.is_success() {
        return Err(upstream_error(status, &body));
    }
    let text = |key| {
        body.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
    };
    let device_code = text("device_code").ok_or_else(|| AuthError::new("invalid_response"))?;
    let user_code = text("user_code").ok_or_else(|| AuthError::new("invalid_response"))?;
    let uri = text("verification_uri").ok_or_else(|| AuthError::new("invalid_response"))?;
    let url = reqwest::Url::parse(uri).map_err(|_| AuthError::new("unexpected_endpoint"))?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || !url
            .host_str()
            .is_some_and(|host| host == "meta.com" || host.ends_with(".meta.com"))
    {
        return Err(AuthError::new("unexpected_endpoint"));
    }
    let expires = body
        .get("expires_in")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .ok_or_else(|| AuthError::new("invalid_response"))?
        .min(900);
    let interval = match body.get("interval") {
        None => 5,
        Some(value) => value
            .as_u64()
            .filter(|n| *n > 0)
            .ok_or_else(|| AuthError::new("invalid_response"))?,
    };
    Ok(DeviceSession {
        device_code: device_code.to_owned(),
        user_code: user_code.to_owned(),
        verification_uri: url.into(),
        expires_at_unix_ms: (crate::now_unix() + expires as i64) * 1000,
        interval_seconds: interval.min(900),
    })
}

#[tauri::command]
pub(crate) async fn poll_muse_code_auth_session(
    device_code: String,
) -> Result<PollResult, AuthError> {
    poll(&client()?, &device_code, TOKEN_URL, KEY_URL).await
}

async fn poll(
    client: &Client,
    device_code: &str,
    token_url: &str,
    key_url: &str,
) -> Result<PollResult, AuthError> {
    if device_code.trim().is_empty() || device_code.len() > 4096 {
        return Err(AuthError::new("invalid_response"));
    }
    let response = json_response(client.post(token_url).form(&[
        ("client_id", CLIENT_ID),
        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ("device_code", device_code),
    ]))
    .await;
    let (status, body, after_seconds) = match response {
        Err(error) if error.code == "network" => {
            return Ok(PollResult::Retry { after_seconds: 10 });
        }
        other => other?,
    };
    if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
        return Ok(PollResult::Retry {
            after_seconds: after_seconds.clamp(5, 900),
        });
    }
    if !status.is_success() {
        return match body.get("error").and_then(Value::as_str) {
            Some("authorization_pending") => Ok(PollResult::Pending),
            Some("slow_down") => Ok(PollResult::SlowDown),
            _ => Err(upstream_error(status, &body)),
        };
    }
    let token = body
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| AuthError::new("invalid_response"))?;
    // No ordinary API key / environment variable fallback and no automatic
    // payment activation. A failed exchange requires explicit sign-in again.
    let (status, key, _) = json_response(
        client
            .post(key_url)
            .bearer_auth(token)
            .json(&serde_json::json!({"dca_token": token})),
    )
    .await?;
    if !status.is_success() {
        return Err(upstream_error(status, &key));
    }
    subscription_key(&key).map(|api_key| PollResult::Authorized { api_key })
}

fn subscription_key(value: &Value) -> Result<String, AuthError> {
    if value.get("require_payment").and_then(Value::as_bool) == Some(true) {
        return Err(AuthError::new("payment_required"));
    }
    if value.get("is_subs_active").and_then(Value::as_bool) == Some(false) {
        return Err(AuthError::new("subscription_inactive"));
    }
    if value.get("require_payment").and_then(Value::as_bool) != Some(false)
        || value.get("is_subs_active").and_then(Value::as_bool) != Some(true)
    {
        return Err(AuthError::new("invalid_response"));
    }
    if let Some(base) = value.get("base_url") {
        if !base
            .as_str()
            .is_some_and(|url| url.trim_end_matches('/') == API_BASE)
        {
            return Err(AuthError::new("unexpected_endpoint"));
        }
    }
    value
        .get("api_key")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| AuthError::new("invalid_response"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use warp::Filter;

    #[test]
    fn muse_code_import_requires_confirmed_subscription_and_fixed_endpoint() {
        let valid = serde_json::json!({"api_key":"dedicated-test-key", "is_subs_active":true, "require_payment":false, "base_url":API_BASE});
        assert_eq!(subscription_key(&valid).unwrap(), "dedicated-test-key");
        for (field, value, code) in [
            ("require_payment", Value::Bool(true), "payment_required"),
            (
                "is_subs_active",
                Value::Bool(false),
                "subscription_inactive",
            ),
            ("is_subs_active", Value::Null, "invalid_response"),
            ("require_payment", Value::Null, "invalid_response"),
            (
                "base_url",
                Value::String("https://unrelated.example/v1".into()),
                "unexpected_endpoint",
            ),
            ("api_key", Value::String(String::new()), "invalid_response"),
        ] {
            let mut body = valid.clone();
            body[field] = value;
            assert_eq!(subscription_key(&body).unwrap_err().code, code);
        }
        let error = upstream_error(
            StatusCode::FORBIDDEN,
            &serde_json::json!({
                "error": {"message":"This service is not available in your region; private account details"},
                "api_key":"never-print-this",
            }),
        );
        let serialized = serde_json::to_string(&error).unwrap();
        assert_eq!(error.code, "region_restricted");
        assert!(!serialized.contains("private") && !serialized.contains("never-print"));
    }

    #[tokio::test]
    async fn muse_code_device_login_mints_only_after_approval_without_inference() {
        let minted = Arc::new(AtomicUsize::new(0));
        let seen = minted.clone();
        let routes = warp::post().and(warp::path::full()).and(warp::header::headers_cloned())
            .and(warp::body::bytes()).map(move |path: warp::path::FullPath, headers: warp::http::HeaderMap, body: bytes::Bytes| {
                let (status, body) = match path.as_str() {
                    "/start" => (200, serde_json::json!({"device_code":"test-device", "user_code":"AB-CD", "verification_uri":"https://auth.meta.com/device", "expires_in":600, "interval":5})),
                    "/invalid-url" => (200, serde_json::json!({"device_code":"test-device", "user_code":"AB-CD", "verification_uri":"https://meta.com.evil.example/device", "expires_in":600})),
                    "/redirect" => (307, Value::Null),
                    "/token" => {
                        let form = std::str::from_utf8(&body).unwrap();
                        assert!(form.contains(&format!("client_id={CLIENT_ID}")));
                        assert!(form.contains("device_code=test-device"));
                        (200, serde_json::json!({"access_token":"test-dca"}))
                    }
                    "/pending" => (400, serde_json::json!({"error":"authorization_pending"})),
                    "/slow" => (400, serde_json::json!({"error":"slow_down"})),
                    "/limited" => (429, Value::Null),
                    "/denied" => (403, serde_json::json!({"error":"access_denied"})),
                    "/key" => {
                        seen.fetch_add(1, Ordering::Relaxed);
                        assert_eq!(headers["authorization"], "Bearer test-dca");
                        let body: Value = serde_json::from_slice(&body).unwrap();
                        assert_eq!(body["dca_token"], "test-dca");
                        (200, serde_json::json!({"api_key":"test-subscription-key", "is_subs_active":true, "require_payment":false}))
                    }
                    _ => panic!("unexpected request, no inference should be sent"),
                };
                warp::reply::with_status(warp::reply::json(&body), warp::http::StatusCode::from_u16(status).unwrap())
            });
        let (address, server) = crate::bind_ephemeral!(routes, ([127, 0, 0, 1], 0));
        let server = tokio::spawn(server);
        let base = format!("http://{address}");
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        assert_eq!(
            start(&client, &format!("{base}/invalid-url"))
                .await
                .err()
                .unwrap()
                .code,
            "unexpected_endpoint"
        );
        let session = start(&client, &format!("{base}/start")).await.unwrap();
        assert_eq!(session.user_code, "AB-CD");
        let key_url = format!("{base}/key");
        assert!(matches!(
            poll(
                &client,
                &session.device_code,
                &format!("{base}/pending"),
                &key_url
            )
            .await
            .unwrap(),
            PollResult::Pending
        ));
        assert!(matches!(
            poll(
                &client,
                &session.device_code,
                &format!("{base}/slow"),
                &key_url
            )
            .await
            .unwrap(),
            PollResult::SlowDown
        ));
        assert!(matches!(
            poll(
                &client,
                &session.device_code,
                &format!("{base}/limited"),
                &key_url
            )
            .await
            .unwrap(),
            PollResult::Retry { .. }
        ));
        assert_eq!(
            poll(
                &client,
                &session.device_code,
                &format!("{base}/denied"),
                &key_url
            )
            .await
            .unwrap_err()
            .code,
            "denied"
        );
        assert_eq!(minted.load(Ordering::Relaxed), 0);
        assert_eq!(
            poll(
                &client,
                &session.device_code,
                &format!("{base}/token"),
                &format!("{base}/redirect")
            )
            .await
            .unwrap_err()
            .code,
            "unexpected_endpoint"
        );
        assert_eq!(minted.load(Ordering::Relaxed), 0);
        let result = poll(
            &client,
            &session.device_code,
            &format!("{base}/token"),
            &key_url,
        )
        .await
        .unwrap();
        assert!(
            matches!(result, PollResult::Authorized { api_key } if api_key == "test-subscription-key")
        );
        assert_eq!(minted.load(Ordering::Relaxed), 1);
        server.abort();
    }
}
