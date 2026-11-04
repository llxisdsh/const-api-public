use std::{error::Error, fmt};

#[derive(Debug, Clone)]
pub(crate) struct SubscriptionOAuthError {
    provider: String,
    upstream_status: u16,
    code: String,
    description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubscriptionOAuthWireError {
    pub(crate) status: u16,
    pub(crate) error_type: &'static str,
    pub(crate) message: String,
}

impl SubscriptionOAuthError {
    pub(crate) fn from_response(
        provider: &str,
        upstream_status: u16,
        value: &serde_json::Value,
    ) -> Self {
        let error = value.get("error").unwrap_or(value);
        let code = error
            .as_str()
            .or_else(|| error.get("code").and_then(serde_json::Value::as_str))
            .or_else(|| error.get("type").and_then(serde_json::Value::as_str))
            .unwrap_or("oauth_error")
            .trim()
            .to_string();
        let code = if code.len() <= 128
            && !code.is_empty()
            && code
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        {
            code
        } else {
            "oauth_error".to_string()
        };
        let description = value
            .get("error_description")
            .and_then(serde_json::Value::as_str)
            .or_else(|| error.get("message").and_then(serde_json::Value::as_str))
            .or_else(|| value.get("message").and_then(serde_json::Value::as_str))
            .unwrap_or_default()
            .trim()
            .to_string();
        Self {
            provider: provider.trim().to_string(),
            upstream_status,
            code,
            description,
        }
    }

    pub(crate) fn code(&self) -> &str {
        &self.code
    }

    fn credential_is_terminal(&self) -> bool {
        let code = self.code.to_ascii_lowercase();
        let description = self.description.to_ascii_lowercase();
        self.upstream_status == 401
            || matches!(
                code.as_str(),
                "invalid_grant"
                    | "invalid_token"
                    | "refresh_token_expired"
                    | "refresh_token_reused"
                    | "refresh_token_invalidated"
            )
            || description.contains("refresh token expired")
            || description.contains("refresh token has expired")
            || description.contains("refresh token revoked")
            || description.contains("refresh token has been revoked")
    }

    pub(crate) fn wire_error(&self) -> SubscriptionOAuthWireError {
        if self.credential_is_terminal() {
            return SubscriptionOAuthWireError {
                status: 401,
                error_type: "subscription_credential_expired",
                message: format!(
                    "{} subscription credential expired or was revoked; reconnect the account subscription ({})",
                    display_provider(&self.provider),
                    self.detail()
                ),
            };
        }
        let (status, error_type) = match self.upstream_status {
            403 => (403, "subscription_oauth_permission_denied"),
            429 => (429, "subscription_oauth_rate_limited"),
            status if status >= 500 => (status, "subscription_oauth_unavailable"),
            _ => (502, "subscription_oauth_error"),
        };
        SubscriptionOAuthWireError {
            status,
            error_type,
            message: format!(
                "{} OAuth refresh failed: {}",
                display_provider(&self.provider),
                self.detail()
            ),
        }
    }

    fn detail(&self) -> String {
        if self.description.is_empty() {
            self.code.clone()
        } else {
            format!("{}: {}", self.code, self.description)
        }
    }
}

impl fmt::Display for SubscriptionOAuthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} OAuth refresh returned HTTP {}: {}",
            display_provider(&self.provider),
            self.upstream_status,
            self.detail()
        )
    }
}

impl Error for SubscriptionOAuthError {}

pub(crate) fn oauth_wire_error_from_anyhow(
    error: &anyhow::Error,
) -> Option<SubscriptionOAuthWireError> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<SubscriptionOAuthError>())
        .map(SubscriptionOAuthError::wire_error)
}

const TOKEN_EXCHANGE_ERROR_BODY_LIMIT: usize = 16 * 1024;

/// Code exchange happens before a channel exists. Keep its rejection distinct
/// from refresh/credential-expiry handling, and never include the callback or tokens.
pub(crate) async fn token_exchange_response(
    provider: &str,
    mut response: reqwest::Response,
    secrets: &[&str],
) -> anyhow::Result<serde_json::Value> {
    let status = response.status();
    if status.is_success() {
        return response.json().await.map_err(|_| {
            anyhow::anyhow!(
                "{} OAuth token exchange returned invalid JSON",
                display_provider(provider)
            )
        });
    }

    let request_id = ["x-request-id", "x-openai-request-id", "cf-ray"]
        .iter()
        .find_map(|name| {
            response
                .headers()
                .get(*name)
                .and_then(|value| value.to_str().ok())
        })
        .map(|value| token_exchange_diagnostic(value, secrets));
    let challenge = response
        .headers()
        .get("cf-mitigated")
        .is_some_and(|value| value == "challenge");
    let html = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("text/html"));
    let mut body = Vec::new();
    let detail = loop {
        match response.chunk().await {
            Ok(Some(chunk)) if chunk.len() <= TOKEN_EXCHANGE_ERROR_BODY_LIMIT - body.len() => {
                body.extend_from_slice(&chunk);
            }
            // Do not parse a truncated response: an echoed secret could be cut
            // into a prefix that would evade redaction.
            Ok(Some(_)) => break "error response exceeded diagnostic size limit".to_string(),
            Err(_) => break "error response body could not be read".to_string(),
            Ok(None) => {
                break if challenge {
                    "Cloudflare challenge (cf-mitigated: challenge)".to_string()
                } else if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body) {
                    let error =
                        SubscriptionOAuthError::from_response(provider, status.as_u16(), &value);
                    token_exchange_diagnostic(&error.detail(), secrets)
                } else if html {
                    "HTML response instead of an OAuth JSON error".to_string()
                } else {
                    "non-JSON or empty error response".to_string()
                };
            }
        }
    };
    let request_id = request_id
        .map(|id| format!(" (request id: {id})"))
        .unwrap_or_default();
    // Only selected, redacted fields are surfaced. Raw bodies can contain login
    // state or credentials even when the token endpoint returns an error.
    Err(anyhow::anyhow!(
        "{} OAuth token exchange returned HTTP {status}: {detail}{request_id}",
        display_provider(provider),
    ))
}

fn token_exchange_diagnostic(text: &str, secrets: &[&str]) -> String {
    let mut text = text.to_string();
    for secret in secrets.iter().copied().filter(|secret| !secret.is_empty()) {
        text = text.replace(secret, "[redacted]");
        text = text.replace(urlencoding::encode(secret).as_ref(), "[redacted]");
    }
    crate::upstream_failure::safe_excerpt(&text)
}

fn display_provider(provider: &str) -> String {
    let mut chars = provider.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => "Subscription".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exchange_response(status: u16, body: impl Into<reqwest::Body>) -> reqwest::Response {
        http::Response::builder()
            .status(status)
            .body(body.into())
            .unwrap()
            .into()
    }

    #[tokio::test]
    async fn token_exchange_success_preserves_token_fields() {
        let value = serde_json::json!({
            "access_token": "test-access", "refresh_token": "test-refresh", "expires_in": 3600,
        });
        let token = token_exchange_response(
            "openai",
            exchange_response(200, value.to_string()),
            &["test-code"],
        )
        .await
        .unwrap();
        assert_eq!(token, value);
    }

    #[tokio::test]
    async fn token_exchange_rejection_keeps_reason_and_redacts_echoed_secrets() {
        let code = "ac.test+/=code";
        let verifier = "test-verifier";
        let mut response = exchange_response(
            403,
            serde_json::json!({
                "error": {
                    "code": "access_denied",
                    "message": format!("Denied {code} {} {verifier}", urlencoding::encode(code)),
                },
                "access_token": "returned-secret-must-not-appear",
            })
            .to_string(),
        );
        response
            .headers_mut()
            .insert("x-request-id", "req-test-403".parse().unwrap());
        let error = token_exchange_response("openai", response, &[code, verifier])
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("HTTP 403 Forbidden"));
        assert!(error.contains("access_denied: Denied"));
        assert!(error.contains("req-test-403"));
        assert!(error.contains("[redacted]"));
        for secret in [
            code,
            verifier,
            urlencoding::encode(code).as_ref(),
            "returned-secret-must-not-appear",
        ] {
            assert!(!error.contains(secret));
        }
        assert!(!error.contains("refresh"));
    }

    #[tokio::test]
    async fn token_exchange_standard_oauth_error_preserves_grant_failure() {
        let response = exchange_response(
            400,
            serde_json::json!({
                "error": "invalid_grant", "error_description": "Authorization code expired",
            })
            .to_string(),
        );
        let error = token_exchange_response("openai", response, &[])
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("HTTP 400 Bad Request"));
        assert!(error.contains("invalid_grant: Authorization code expired"));
        assert!(!error.contains("refresh"));
    }

    #[tokio::test]
    async fn token_exchange_html_is_distinct_from_an_explicit_cloudflare_challenge() {
        for challenge in [false, true] {
            let mut response = exchange_response(403, "<html>private-login-state</html>");
            response
                .headers_mut()
                .insert("content-type", "text/html; charset=utf-8".parse().unwrap());
            response
                .headers_mut()
                .insert("cf-ray", "ray-test".parse().unwrap());
            if challenge {
                response
                    .headers_mut()
                    .insert("cf-mitigated", "challenge".parse().unwrap());
            }
            let error = token_exchange_response("openai", response, &[])
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains("HTTP 403 Forbidden"));
            assert!(error.contains("ray-test"));
            assert!(!error.contains("private-login-state"));
            assert_eq!(error.contains("Cloudflare challenge"), challenge);
            assert_eq!(error.contains("HTML response"), !challenge);
        }
    }

    #[tokio::test]
    async fn token_exchange_unusable_bodies_retain_status_without_raw_content() {
        for (body, expected) in [
            ("private non-json response".to_string(), "non-JSON or empty"),
            (
                format!("{}private", "x".repeat(TOKEN_EXCHANGE_ERROR_BODY_LIMIT)),
                "size limit",
            ),
        ] {
            let error = token_exchange_response("openai", exchange_response(403, body), &[])
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains("HTTP 403 Forbidden"));
            assert!(error.contains(expected));
            assert!(!error.contains("private"));
        }
        let body =
            reqwest::Body::wrap_stream(futures_util::stream::iter([Err::<bytes::Bytes, _>(
                std::io::Error::other("private transport detail"),
            )]));
        let error = token_exchange_response("openai", exchange_response(403, body), &[])
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("HTTP 403 Forbidden"));
        assert!(error.contains("body could not be read"));
        assert!(!error.contains("private"));
    }

    #[tokio::test]
    async fn token_exchange_request_id_is_redacted_before_shortening() {
        let secret = "private-secret";
        let mut response = exchange_response(403, "{}");
        response
            .headers_mut()
            .insert("x-request-id", secret.parse().unwrap());
        let error = token_exchange_response("openai", response, &[secret])
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("request id: [redacted]"));
        assert!(!error.contains(secret));
    }

    #[tokio::test]
    async fn token_exchange_invalid_success_does_not_expose_response() {
        let error =
            token_exchange_response("openai", exchange_response(200, "private response"), &[])
                .await
                .unwrap_err()
                .to_string();
        assert!(error.contains("invalid JSON"));
        assert!(!error.contains("private"));
    }

    #[test]
    fn expired_refresh_token_becomes_a_credential_error() {
        let error = SubscriptionOAuthError::from_response(
            "claude",
            400,
            &serde_json::json!({
                "error": "invalid_grant",
                "error_description": "Refresh token expired"
            }),
        );
        let wire = error.wire_error();
        assert_eq!(wire.status, 401);
        assert_eq!(wire.error_type, "subscription_credential_expired");
        assert!(wire.message.contains("reconnect"));
    }

    #[test]
    fn provider_outage_remains_retryable_provider_evidence() {
        let error = SubscriptionOAuthError::from_response(
            "claude",
            503,
            &serde_json::json!({"error": {"message": "temporarily unavailable"}}),
        );
        let wire = error.wire_error();
        assert_eq!(wire.status, 503);
        assert_eq!(wire.error_type, "subscription_oauth_unavailable");
    }
}
