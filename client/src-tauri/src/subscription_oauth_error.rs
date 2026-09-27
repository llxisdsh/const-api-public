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
