//! OpenCode Console device authorization.
//!
//! The Console desktop flow returns an OAuth access/refresh-token pair. The
//! renderer passes the serialized token to the existing subscription import
//! path; inference refreshes the access token at the request boundary.

use reqwest::{
    Client, RequestBuilder, StatusCode,
    header::{CONTENT_TYPE, RETRY_AFTER, USER_AGENT},
};
use serde::Serialize;
use serde_json::Value;
use std::{sync::OnceLock, time::Duration};

const CLIENT_ID: &str = "opencode-cli";
const CONSOLE_ORIGIN: &str = "https://opencode.ai/console/";
const DEVICE_CODE_URL: &str = "https://opencode.ai/console/auth/device/code";
const DEVICE_TOKEN_URL: &str = "https://opencode.ai/console/auth/device/token";
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

struct ResponseBody {
    status: StatusCode,
    value: Value,
    retry_after: u64,
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
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
    })
    .map_err(|_| AuthError::new("network"))
}

async fn response(request: RequestBuilder) -> Result<ResponseBody, AuthError> {
    let response = request
        .header("Accept", "application/json")
        .header(USER_AGENT, crate::opencode_client::USER_AGENT_VALUE)
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
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(15);
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
        return Ok(ResponseBody {
            status,
            value: Value::Null,
            retry_after,
        });
    }
    let bytes = response.bytes().await.map_err(|_| AuthError {
        code: "invalid_response",
        status: Some(status.as_u16()),
    })?;
    Ok(ResponseBody {
        status,
        value: parse_response_body(&content_type, &bytes)?,
        retry_after,
    })
}

fn upstream_error(status: StatusCode, body: &Value) -> AuthError {
    let code = oauth_error_code(body)
        .map(|error| match error {
            "access_denied" => "denied",
            "expired_token" | "invalid_grant" => "expired",
            "slow_down" => "slow_down",
            _ => "upstream_error",
        })
        .unwrap_or("upstream_error");
    AuthError {
        code,
        status: Some(status.as_u16()),
    }
}

fn oauth_error_code(body: &Value) -> Option<&str> {
    body.get("error")
        .and_then(|error| {
            error.as_str().or_else(|| {
                error
                    .get("code")
                    .and_then(Value::as_str)
                    .or_else(|| error.get("type").and_then(Value::as_str))
            })
        })
        .or_else(|| body.get("error_code").and_then(Value::as_str))
        .filter(|code| {
            !code.is_empty()
                && code.len() <= 128
                && code
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.')
        })
}

fn parse_response_body(content_type: &str, bytes: &[u8]) -> Result<Value, AuthError> {
    let content_type = content_type.to_ascii_lowercase();
    if content_type.contains("json") {
        return serde_json::from_slice(bytes).map_err(|_| AuthError::new("invalid_response"));
    }
    if content_type.contains("application/x-www-form-urlencoded") {
        return serde_urlencoded::from_bytes::<std::collections::HashMap<String, String>>(bytes)
            .map(|fields| {
                Value::Object(
                    fields
                        .into_iter()
                        .map(|(key, value)| (key, Value::String(value)))
                        .collect(),
                )
            })
            .map_err(|_| AuthError::new("invalid_response"));
    }
    Err(AuthError::new("invalid_response"))
}

fn text<'a>(body: &'a Value, key: &str) -> Option<&'a str> {
    body.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

fn verification_url(body: &Value) -> Result<String, AuthError> {
    let raw = text(body, "verification_uri_complete")
        .or_else(|| text(body, "verification_uri"))
        .ok_or_else(|| AuthError::new("invalid_response"))?;
    let base =
        reqwest::Url::parse(CONSOLE_ORIGIN).map_err(|_| AuthError::new("unexpected_endpoint"))?;
    let url = base
        .join(raw)
        .map_err(|_| AuthError::new("unexpected_endpoint"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(AuthError::new("unexpected_endpoint"));
    }
    let path = url.path();
    if url.host_str() != Some("opencode.ai")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || (path != "/console" && !path.starts_with("/console/"))
    {
        return Err(AuthError::new("unexpected_endpoint"));
    }
    if url.scheme() == "http" && !cfg!(test) {
        return Err(AuthError::new("unexpected_endpoint"));
    }
    Ok(url.into())
}

async fn resolve_org_id(client: &Client, access_token: &str) -> Result<Option<String>, AuthError> {
    let response = response(
        client
            .get("https://opencode.ai/console/api/orgs")
            .bearer_auth(access_token),
    )
    .await?;
    if !response.status.is_success() {
        return Err(upstream_error(response.status, &response.value));
    }
    if !response.value.is_array() {
        return Err(AuthError::new("invalid_response"));
    }
    // Like OpenCode itself, an account without organizations sends no org header.
    Ok(first_organization_id(&response.value))
}

fn first_organization_id(body: &Value) -> Option<String> {
    let mut organizations = body
        .as_array()?
        .iter()
        .filter_map(|organization| {
            let id = text(organization, "id")?;
            let name = organization
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_lowercase();
            Some((name, id.to_string()))
        })
        .collect::<Vec<_>>();
    organizations.sort();
    organizations.into_iter().next().map(|(_, id)| id)
}

fn credential_body(mut body: Value) -> Result<Value, AuthError> {
    let access_token = text(&body, "access_token")
        .ok_or_else(|| AuthError::new("invalid_response"))?
        .to_string();
    let refresh_token = text(&body, "refresh_token")
        .ok_or_else(|| AuthError::new("invalid_response"))?
        .to_string();
    let object = body
        .as_object_mut()
        .ok_or_else(|| AuthError::new("invalid_response"))?;
    object.insert("type".into(), Value::String("opencode".into()));
    object.insert("provider".into(), Value::String("opencode".into()));
    object.insert("access_token".into(), Value::String(access_token));
    object.insert("refresh_token".into(), Value::String(refresh_token));
    Ok(body)
}

fn poll_result(status: StatusCode, body: &Value) -> Result<PollResult, AuthError> {
    match oauth_error_code(body) {
        Some("authorization_pending") => Ok(PollResult::Pending),
        Some("slow_down") => Ok(PollResult::SlowDown),
        _ => Err(upstream_error(status, body)),
    }
}

#[tauri::command]
pub(crate) async fn create_opencode_auth_session() -> Result<DeviceSession, AuthError> {
    let response = response(client()?.post(DEVICE_CODE_URL).json(&serde_json::json!({
        "client_id": CLIENT_ID,
        "supports_org_scope": true,
    })))
    .await?;
    if !response.status.is_success() {
        return Err(upstream_error(response.status, &response.value));
    }
    let body = response.value;
    let expires = body
        .get("expires_in")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| AuthError::new("invalid_response"))?
        .min(1800);
    let interval = body
        .get("interval")
        .and_then(Value::as_u64)
        .unwrap_or(5)
        .clamp(1, 60);
    let device_code = text(&body, "device_code")
        .ok_or_else(|| AuthError::new("invalid_response"))?
        .to_string();
    let user_code = text(&body, "user_code").unwrap_or_default().to_string();
    Ok(DeviceSession {
        device_code,
        user_code,
        verification_uri: verification_url(&body)?,
        expires_at_unix_ms: (crate::now_unix() + expires as i64) * 1000,
        interval_seconds: interval,
    })
}

#[tauri::command]
pub(crate) async fn poll_opencode_auth_session(
    device_code: String,
) -> Result<PollResult, AuthError> {
    if device_code.trim().is_empty() || device_code.len() > 4096 {
        return Err(AuthError::new("invalid_response"));
    }
    let http = client()?;
    let response = response(http.post(DEVICE_TOKEN_URL).json(&serde_json::json!({
        "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
        "device_code": device_code,
        "client_id": CLIENT_ID,
    })))
    .await?;
    if response.status.is_server_error() || response.status == StatusCode::TOO_MANY_REQUESTS {
        return Ok(PollResult::Retry {
            after_seconds: response.retry_after.clamp(5, 900),
        });
    }
    if !response.status.is_success() {
        return poll_result(response.status, &response.value);
    }
    let mut token = credential_body(response.value)?;
    let access_token = text(&token, "access_token")
        .expect("credential_body validates access_token")
        .to_string();
    if text(&token, "org_id").is_none()
        && let Some(org_id) = resolve_org_id(&http, &access_token).await?
    {
        token["org_id"] = serde_json::json!(org_id);
    }
    Ok(PollResult::Authorized {
        api_key: serde_json::to_string(&token).map_err(|_| AuthError::new("invalid_response"))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn error_code(status: StatusCode, body: &Value) -> Result<String, AuthError> {
        poll_result(status, body)
            .map(|_| "poll".into())
            .or_else(|error| {
                if error.status == Some(status.as_u16()) {
                    Ok(error.code.into())
                } else {
                    Err(error)
                }
            })
    }

    #[test]
    fn device_poll_distinguishes_pending_and_slow_down_from_auth_errors() {
        assert!(matches!(
            poll_result(
                StatusCode::BAD_REQUEST,
                &json!({"error": "authorization_pending"})
            ),
            Ok(PollResult::Pending)
        ));
        assert!(matches!(
            poll_result(
                StatusCode::BAD_REQUEST,
                &json!({"error": {"code": "slow_down"}})
            ),
            Ok(PollResult::SlowDown)
        ));
        assert_eq!(
            error_code(StatusCode::BAD_REQUEST, &json!({"error": "invalid_grant"})).unwrap(),
            "expired"
        );
        assert_eq!(
            error_code(
                StatusCode::UNAUTHORIZED,
                &json!({"error": {"code": "invalid_grant", "message": "SECRET liaison"}})
            )
            .unwrap(),
            "expired"
        );
    }

    #[test]
    fn oauth_form_errors_are_parsed_without_retaining_descriptions() {
        let body = concat!("error=access_", "denied&error_description=SECRET");
        let parsed = parse_response_body(
            "application/x-www-form-urlencoded; charset=UTF-8",
            body.as_bytes(),
        )
        .unwrap();
        assert_eq!(parsed["error"], "access_denied");
        assert_eq!(
            error_code(StatusCode::BAD_REQUEST, &parsed).unwrap(),
            "denied"
        );
        assert!(parse_response_body("text/plain", b"SECRET non-json").is_err());
        assert!(parse_response_body("application/json", b"SECRET not-json").is_err());
    }

    #[test]
    fn device_credentials_require_both_tokens_and_preserve_expiry_and_org() {
        let token = credential_body(json!({
            "access_token": "access-a",
            "refresh_token": "refresh-a",
            "expires_in": 3600,
            "org_id": "org-a"
        }))
        .unwrap();
        assert_eq!(token["type"], "opencode");
        assert_eq!(token["provider"], "opencode");
        assert_eq!(token["access_token"], "access-a");
        assert_eq!(token["refresh_token"], "refresh-a");
        assert_eq!(token["expires_in"], 3600);
        assert_eq!(token["org_id"], "org-a");
        assert!(
            credential_body(json!({"access_token": "access-a", "refresh_token": " "})).is_err()
        );
    }

    #[test]
    fn missing_org_selects_the_first_sorted_organization() {
        let organizations = json!([
            {"id": "org-z", "name": "Zulu"},
            {"id": "org-d", "name": "alpha"},
            {"id": "org-a", "name": "Alpha"},
            {"id": "", "name": "invalid"}
        ]);
        assert_eq!(
            first_organization_id(&organizations).as_deref(),
            Some("org-a")
        );
        assert!(first_organization_id(&json!([])).is_none());
        assert!(first_organization_id(&json!({"items": []})).is_none());
    }

    #[test]
    fn verification_urls_accept_documented_relative_forms_and_safe_hosts() {
        let root_relative = verification_url(
            &json!({"verification_uri_complete": "/console/device?user_code=ABCD"}),
        )
        .unwrap();
        let path_relative =
            verification_url(&json!({"verification_uri_complete": "device?user_code=ABCD"}))
                .unwrap();
        let fallback = verification_url(&json!({"verification_uri": "/console/device"})).unwrap();
        assert_eq!(
            root_relative,
            concat!("https://opencode.ai", "/console/device?user_code=ABCD")
        );
        assert_eq!(path_relative, root_relative);
        assert_eq!(fallback, concat!("https://opencode.ai", "/console/device"));
        let unsafe_url = concat!("java", "script:alert(1)");
        assert!(verification_url(&json!({"verification_uri_complete": unsafe_url})).is_err());
        assert!(verification_url(&json!({})).is_err());
    }
}
