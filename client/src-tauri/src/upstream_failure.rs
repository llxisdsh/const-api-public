//! Read-only failure observation shared by probes and live HTTP/SSE/WS paths.
//! Only protocol error envelopes are inspected; model output is never searched.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub(crate) const MAX_OBSERVATION_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub(crate) struct Failure {
    pub(crate) version: u8,
    pub(crate) cause: String,
    pub(crate) scope: String,
    pub(crate) rule_id: String,
    pub(crate) status: u16,
    pub(crate) code: Option<Value>,
    pub(crate) error_type: String,
    pub(crate) reason: String,
    pub(crate) domain: String,
    pub(crate) param: String,
    pub(crate) model: String,
    pub(crate) retry_after_seconds: Option<i64>,
    pub(crate) retry_source: String,
    #[serde(skip)]
    pub(crate) message: String,
}

impl Failure {
    pub(crate) fn effective_status(&self) -> u16 {
        if self.status >= 400 {
            return self.status;
        }
        match self.cause.as_str() {
            "rate_limited" | "model_capacity" | "model_quota" | "model_entitlement"
            | "account_quota" => 429,
            "auth_error" => 401,
            "permission_denied"
            | "provider_auth_error"
            | "provider_eligibility_restricted"
            | "location_unsupported"
            | "content_policy"
            | "cyber_policy" => 403,
            "request_invalid" | "continuation_invalid" | "model_unavailable" => 400,
            _ => 502,
        }
    }
    pub(crate) fn legacy_kind(&self) -> &str {
        match self.cause.as_str() {
            "model_capacity" => "provider_overload",
            "model_quota" | "model_entitlement" | "account_quota" => "quota_exhausted",
            "provider_eligibility_restricted" | "provider_auth_error" => "permission_denied",
            "legal_restricted" => "permission_denied",
            "model_unavailable" | "request_invalid" | "continuation_invalid" => "request_error",
            "unknown" => "",
            other => other,
        }
    }

    pub(crate) fn group_failure(&self) -> bool {
        matches!(
            self.cause.as_str(),
            "provider_eligibility_restricted"
                | "location_unsupported"
                | "permission_denied"
                | "provider_overload"
                | "network_error"
                | "timeout"
        )
    }

    pub(crate) fn safe_to_retry(&self) -> bool {
        !matches!(
            self.cause.as_str(),
            "request_invalid"
                | "continuation_invalid"
                | "content_policy"
                | "cyber_policy"
                | "risk_challenge"
                | "legal_restricted"
        )
    }

    pub(crate) fn attach(&self, payload: &mut Map<String, Value>) {
        if self.version != 1 {
            return;
        }
        // Additive map fields are ignored by legacy peers; legacy scope remains valid.
        if let Ok(value) = serde_json::to_value(self) {
            payload.insert("failure_observation_v1".into(), value);
        }
        if self.status < 400 {
            payload.insert("upstream_semantic_failure".into(), true.into());
        }
        let kind = self.legacy_kind();
        if !kind.is_empty() {
            payload.insert("error_kind".into(), kind.into());
        }
        payload.insert("safe_to_retry_same_channel".into(), false.into());
        payload.insert(
            "safe_to_retry_other_channel".into(),
            self.safe_to_retry().into(),
        );
        payload.insert("failure_scope".into(), self.scope.clone().into());
        payload.remove("failure_model");
        payload.remove("model_failure_soft");
        if self.scope == "model" && !self.model.is_empty() {
            payload.insert("failure_model".into(), self.model.clone().into());
        } else if self.scope == "request"
            && !self.model.is_empty()
            && matches!(
                self.cause.as_str(),
                "rate_limited"
                    | "provider_overload"
                    | "permission_denied"
                    | "provider_auth_error"
                    | "timeout"
                    | "network_error"
            )
        {
            payload.insert("failure_model".into(), self.model.clone().into());
            payload.insert("model_failure_soft".into(), true.into());
        }
        if let Some(delay) = self.retry_after_seconds {
            payload.insert("retry_after_seconds".into(), delay.into());
            payload.insert("retry_hint_source".into(), self.retry_source.clone().into());
        }
    }
}

fn text(value: Option<&Value>) -> &str {
    value.and_then(Value::as_str).unwrap_or("")
}
fn lower(value: Option<&Value>) -> String {
    text(value)
        .trim()
        .chars()
        .take(128)
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Fixed, protocol-owned paths, including successful HTTP responses with a failed body.
pub(crate) fn error_object(value: &Value) -> Option<(&Value, &Value)> {
    if let Some(error) = value.get("error").filter(|v| v.is_object()) {
        return Some((value, error));
    }
    if let Some(response) = value.get("response") {
        if let Some(error) = response.get("error").filter(|v| v.is_object()) {
            return Some((response, error));
        }
    }
    value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| {
            choices.iter().find_map(|choice| {
                (text(choice.get("finish_reason")) == "error")
                    .then(|| {
                        choice
                            .get("error")
                            .filter(|v| v.is_object())
                            .map(|e| (choice, e))
                    })
                    .flatten()
            })
        })
}

pub(crate) fn observe(
    status: u16,
    body: &str,
    headers: &[(&str, &str)],
    model: Option<&str>,
) -> Option<Failure> {
    let value = (body.len() <= MAX_OBSERVATION_BYTES)
        .then(|| serde_json::from_str::<Value>(body).ok())
        .flatten();
    let mut failure = match value.as_ref() {
        Some(value) => observe_value(status, value, model),
        None if status >= 400 => {
            let message = if body.len() <= MAX_OBSERVATION_BYTES {
                body.trim()
            } else {
                ""
            };
            let (cause, scope, rule) = scoped_message(status, &message.to_ascii_lowercase(), model);
            let scope = if scope == "model" && model.is_none_or(str::is_empty) {
                "request"
            } else {
                scope
            };
            Some(Failure {
                version: 1,
                cause: cause.into(),
                scope: scope.into(),
                rule_id: rule.into(),
                model: model.unwrap_or("").to_string(),
                message: safe_excerpt(message),
                status,
                ..Default::default()
            })
        }
        _ => None,
    }?;
    if let Some(delay) = retry_after(headers, crate::now_unix()) {
        failure.retry_after_seconds = Some(delay);
        failure.retry_source = "header".into();
    }
    Some(failure)
}

pub(crate) fn observe_value(status: u16, value: &Value, model: Option<&str>) -> Option<Failure> {
    let pair = error_object(value);
    if status < 400 && pair.is_none() {
        return None;
    }
    let (envelope, error) = pair.unwrap_or((value, value));
    // SSE/WS can carry a numeric upstream status inside an error while the
    // enclosing transport is successful. Keep the wire status, classify the error.
    let classification_status = if status < 400 {
        error
            .get("code")
            .or_else(|| error.get("status"))
            .and_then(Value::as_u64)
            .filter(|code| (400..600).contains(code))
            .map(|code| code as u16)
            .unwrap_or(status)
    } else {
        status
    };
    let canonical = crate::openrouter::canonical_error_type(envelope, error);
    let code = error
        .get("code")
        .filter(|v| v.is_string() || v.is_number() || v.is_null())
        .map(|v| {
            v.as_str()
                .map(|s| Value::String(safe_excerpt(s)))
                .unwrap_or_else(|| v.clone())
        });
    let raw_type = lower(error.get("type"));
    let code_text = lower(code.as_ref());
    let rpc_status = lower(error.get("status"));
    let message = text(error.get("message").or_else(|| value.get("detail")));
    let message_lower = message.trim().to_ascii_lowercase();
    let mut failure = Failure {
        version: 1,
        cause: "unknown".into(),
        scope: "request".into(),
        rule_id: "unknown.v1".into(),
        status,
        code,
        error_type: if canonical.is_empty() {
            raw_type.clone()
        } else {
            canonical.clone()
        },
        param: text(error.get("param")).chars().take(128).collect(),
        message: safe_excerpt(message),
        ..Default::default()
    };
    let mut detail_code = lower(error.pointer("/details/error_code"));
    if let Some(details) = error.get("details").and_then(Value::as_array) {
        for detail in details.iter().take(16) {
            let ty = text(detail.get("@type"));
            if ty.ends_with("/google.rpc.ErrorInfo") {
                failure.reason = text(detail.get("reason")).chars().take(128).collect();
                failure.domain = text(detail.get("domain")).chars().take(128).collect();
                failure.model = text(detail.pointer("/metadata/model"))
                    .chars()
                    .take(192)
                    .collect();
                detail_code = failure.reason.to_ascii_lowercase();
            } else if ty.ends_with("/google.rpc.RetryInfo") {
                if let Some(delay) = duration_seconds(text(detail.get("retryDelay"))) {
                    failure.retry_after_seconds = Some(delay);
                    failure.retry_source = "structured_body".into();
                }
            }
        }
    }
    if failure.retry_after_seconds.is_none() {
        failure.retry_after_seconds = error
            .get("resets_in_seconds")
            .and_then(Value::as_i64)
            .filter(|v| *v >= 0)
            .or_else(|| {
                error
                    .get("resets_at")
                    .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
                    .map(|v| v.saturating_sub(crate::now_unix()).max(0))
            });
        if failure.retry_after_seconds.is_some() {
            failure.retry_source = "structured_body".into();
        }
    }
    // Canonical skin types are more precise than lossy native codes. Specific detail codes win.
    let kind = if !detail_code.is_empty() {
        detail_code.as_str()
    } else if !canonical.is_empty() {
        canonical.as_str()
    } else if !code_text.is_empty() {
        code_text.as_str()
    } else if !raw_type.is_empty() {
        raw_type.as_str()
    } else {
        rpc_status.as_str()
    };
    let (cause, scope, rule) = match kind {
        "cyber_policy" => ("cyber_policy", "request", "policy.cyber.v1"),
        "content_policy_violation" | "image_content_policy_violation" | "refusal" => {
            ("content_policy", "request", "policy.content.v1")
        }
        "enforced_spend_limit_reached"
        | "insufficient_quota"
        | "credit_balance_exhausted"
        | "organization_spend_limit_exceeded"
        | "project_spend_limit_exceeded"
        | "organization_usage_limit_exceeded"
        | "usage_limit_reached"
        | "payment_required" => ("account_quota", "channel", "quota.account.v1"),
        "model_capacity_exhausted" => ("model_capacity", "model", "capacity.model.v1"),
        "usage_not_included" => ("model_entitlement", "model", "quota.entitlement.v1"),
        "model_not_found"
        | "model_not_supported"
        | "unsupported_model"
        | "model_access_denied"
        | "model_not_allowed"
        | "unknown_model" => ("model_unavailable", "model", "model.unavailable.v1"),
        "rate_limit_exceeded"
        | "rate_limit_error"
        | "rate_limit_error_reached"
        | "rate_limited"
        | "slow_down" => ("rate_limited", "request", "rate.structured.v1"),
        "invalid_api_key" | "authentication_error" | "unauthenticated" | "authentication" => {
            if !canonical.is_empty() {
                ("provider_auth_error", "request", "auth.provider.v1")
            } else {
                ("auth_error", "channel", "auth.credential.v1")
            }
        }
        "provider_overloaded"
        | "overloaded_error"
        | "server_error"
        | "server"
        | "internal"
        | "upstream_unavailable"
        | "provider_unavailable"
        | "api_error"
        | "unavailable" => ("provider_overload", "request", "availability.structured.v1"),
        "timeout" | "timeout_error" => ("timeout", "request", "availability.timeout.v1"),
        "invalid_request_error" => scoped_message(
            if classification_status < 400 {
                400
            } else {
                classification_status
            },
            &message_lower,
            model,
        ),
        "permission_error" | "permission_denied" => scoped_message(
            if classification_status < 400 {
                403
            } else {
                classification_status
            },
            &message_lower,
            model,
        ),
        "invalid_value"
        | "invalid_request"
        | "invalid_argument"
        | "context_length_exceeded"
        | "invalid_prompt" => ("request_invalid", "request", "request.structured.v1"),
        _ => scoped_message(classification_status, &message_lower, model),
    };
    failure.cause = cause.into();
    failure.scope = scope.into();
    failure.rule_id = rule.into();
    if failure.scope == "model" {
        let actual = model.unwrap_or("").trim();
        if !failure.model.is_empty()
            && !actual.is_empty()
            && crate::config::normalize_model_name(&failure.model)
                != crate::config::normalize_model_name(actual)
        {
            failure.scope = "request".into();
        } else if failure.model.is_empty() {
            failure.model = actual.to_string();
        }
        if failure.model.is_empty() {
            failure.scope = "request".into();
        }
    }
    if failure.model.is_empty() {
        failure.model = model.unwrap_or("").to_string();
    }
    Some(failure)
}

/// Completion evidence only; unknown extensions remain transparent, without healing health.
pub(crate) fn completed_json(value: &Value) -> bool {
    if error_object(value).is_some() {
        return false;
    }
    if matches!(text(value.get("status")), "completed" | "incomplete") {
        return true;
    }
    if value.get("type").and_then(Value::as_str) == Some("message")
        && value.get("stop_reason").is_some_and(Value::is_string)
    {
        return true;
    }
    value
        .get("choices")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            !items.is_empty()
                && items.iter().all(|item| {
                    matches!(
                        text(item.get("finish_reason")),
                        "stop" | "length" | "tool_calls" | "function_call"
                    )
                })
        })
        || value
            .get("candidates")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                !items.is_empty()
                    && items
                        .iter()
                        .all(|item| matches!(text(item.get("finishReason")), "STOP" | "MAX_TOKENS"))
            })
}

fn scoped_message(
    status: u16,
    message: &str,
    model: Option<&str>,
) -> (&'static str, &'static str, &'static str) {
    if status == 403
        && (message.starts_with("captcha verification required")
            || message.starts_with("请求触发风控")
            || message.starts_with("账号异常"))
    {
        return ("risk_challenge", "channel", "safety.challenge.v1");
    }
    if matches!(status, 400 | 403)
        && (message.starts_with("user location is not supported")
            || message.starts_with("api use is not supported in your location"))
    {
        return ("location_unsupported", "model", "location.explicit.v1");
    }
    if status == 403
        && message == "the request is prohibited due to a violation of provider terms of service."
    {
        return (
            "provider_eligibility_restricted",
            "model",
            "eligibility.provider_terms.v1",
        );
    }
    if matches!(status, 400 | 404 | 422)
        && (message.starts_with("no tool call found for ")
            || message.starts_with("no tool output found for ")
            || message.starts_with("previous response")
            || message.starts_with("item with id"))
    {
        return ("continuation_invalid", "request", "request.history.v1");
    }
    if matches!(status, 400 | 403 | 404 | 422)
        && model.is_some_and(|m| !m.is_empty() && message.contains(&m.to_ascii_lowercase()))
        && (message.contains("model is not supported")
            || message.contains("not a valid model id")
            || message.contains("model does not exist")
            || message.contains("model was not found"))
    {
        return ("model_unavailable", "model", "model.named.v1");
    }
    if matches!(status, 400 | 429)
        && (message.starts_with("you have reached your specified api usage limits")
            || message.starts_with("you have reached your specified workspace api usage limits"))
    {
        return ("account_quota", "channel", "quota.claude_spend.v1");
    }
    if status == 429 && message.contains("usage credits are required for this model") {
        return ("model_entitlement", "model", "quota.model_credits.v1");
    }
    if matches!(status, 429 | 503)
        && message.starts_with("no capacity available for model ")
        && message.ends_with(" on the server")
    {
        return ("model_capacity", "model", "capacity.named.v1");
    }
    match status {
        401 => ("auth_error", "channel", "http.unauthorized.v1"),
        402 => ("account_quota", "channel", "http.payment_required.v1"),
        403 => ("permission_denied", "request", "http.forbidden.v1"),
        429 => ("rate_limited", "request", "http.rate_unknown.v1"),
        451 => ("legal_restricted", "request", "http.legal_unknown.v1"),
        500..=599 => ("provider_overload", "request", "http.unavailable.v1"),
        400 | 404 | 409 | 413 | 422 => ("request_invalid", "request", "http.request.v1"),
        _ => ("unknown", "request", "unknown.v1"),
    }
}

pub(crate) fn safe_excerpt(message: &str) -> String {
    let mut out = String::new();
    let mut hide_next = false;
    for word in message.split_whitespace() {
        let lower = word.to_ascii_lowercase();
        let secret = hide_next
            || lower.starts_with("sk-")
            || lower.starts_with("eyj")
            || word.contains('@')
            || [
                "api_key=",
                "api-key=",
                "access_token=",
                "refresh_token=",
                "authorization=",
                "cookie=",
                "turn_state=",
            ]
            .iter()
            .any(|marker| lower.contains(marker))
            || word.len() > 96;
        hide_next = matches!(
            lower.as_str(),
            "bearer" | "authorization:" | "api_key:" | "cookie:"
        );
        let part = if secret { "[redacted]" } else { word };
        if out.len() + part.len() + 1 > 512 {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(part);
    }
    out
}

fn duration_seconds(value: &str) -> Option<i64> {
    let seconds = value.strip_suffix('s')?.parse::<f64>().ok()?;
    (seconds.is_finite() && (0.0..=31_536_000.0).contains(&seconds)).then(|| seconds.ceil() as i64)
}

pub(crate) fn retry_after(headers: &[(&str, &str)], now: i64) -> Option<i64> {
    headers.iter().find_map(|(name, value)| {
        if !name.eq_ignore_ascii_case("retry-after") {
            return None;
        }
        if let Ok(seconds) = value.trim().parse::<i64>() {
            return (0..=31_536_000).contains(&seconds).then_some(seconds);
        }
        let date = time::OffsetDateTime::parse(
            value.trim(),
            &time::format_description::well_known::Rfc2822,
        )
        .ok()?;
        Some(
            date.unix_timestamp()
                .saturating_sub(now)
                .clamp(0, 31_536_000),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn failure(status: u16, raw: &str) -> Failure {
        observe(status, raw, &[], Some("model-a")).unwrap()
    }
    #[test]
    fn structured_rate_capacity_and_quota_are_distinct() {
        assert_eq!(
            failure(429, r#"{"error":{"type":"rate_limit_exceeded"}}"#).cause,
            "rate_limited"
        );
        assert_eq!(
            failure(429, r#"{"error":{"type":"insufficient_quota"}}"#).scope,
            "channel"
        );
        let capacity = failure(
            429,
            r#"{"error":{"status":"RESOURCE_EXHAUSTED","details":[{"@type":"type.googleapis.com/google.rpc.RetryInfo","retryDelay":"0.25s"},{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"MODEL_CAPACITY_EXHAUSTED","domain":"googleapis.com","metadata":{"model":"model-a"}}]}}"#,
        );
        assert_eq!(capacity.cause, "model_capacity");
        assert_eq!(capacity.scope, "model");
        assert_eq!(capacity.retry_after_seconds, Some(1));
    }
    #[test]
    fn errors_are_not_searched_inside_assistant_content() {
        assert!(
            observe(
                200,
                r#"{"choices":[{"message":{"content":"quota exhausted, risk, error"}}]}"#,
                &[],
                None
            )
            .is_none()
        );
        assert_eq!(failure(403, r#"{"error":{"message":"a normal error","metadata":{"prompt":"captcha quota risk"}}}"#).cause, "permission_denied");
    }
    #[test]
    fn observed_live_shapes_and_future_fields_are_tolerated() {
        let detail = observe(400, r#"{"detail":"The 'model-a' model is not supported when using Codex with a ChatGPT account."}"#, &[], Some("model-a")).unwrap();
        assert_eq!(detail.cause, "model_unavailable");
        let future = failure(
            400,
            r#"{"error":{"code":null,"type":"future_error","unknown":{"a":1}},"detail":[{"msg":"unknown"}]}"#,
        );
        assert_eq!(future.scope, "request");
        assert_eq!(future.error_type, "future_error");
        assert_eq!(failure(429, r#"{"error":{"type":"rate_limit_error","details":{"error_code":"enforced_spend_limit_reached"}}}"#).cause, "account_quota");
    }
    #[test]
    fn terminal_skin_errors_override_lossy_native_codes() {
        assert_eq!(failure(200, r#"{"type":"response.failed","response":{"error_type":"authentication","error":{"code":"server_error"}}}"#).cause, "provider_auth_error");
        assert_eq!(failure(200, r#"{"choices":[{"finish_reason":"error","error":{"code":502,"metadata":{"error_type":"provider_unavailable"}}}]}"#).cause, "provider_overload");
    }
    #[test]
    fn retry_after_has_units_and_zero_semantics() {
        assert_eq!(retry_after(&[("Retry-After", "0")], 1), Some(0));
        assert_eq!(retry_after(&[("Retry-After", "-1")], 1), None);
        assert_eq!(
            retry_after(
                &[("Retry-After", "Wed, 21 Oct 2015 07:28:00 GMT")],
                1445412470
            ),
            Some(10)
        );
        assert_eq!(retry_after(&[("x-ratelimit-reset-tokens", "30")], 1), None);
    }

    #[test]
    fn excerpts_redact_inline_credentials_and_attach_replaces_stale_scope() {
        let excerpt = safe_excerpt(
            "HTTP 403 https://host/?api_key=private access_token=private Bearer private user@example.com",
        );
        assert!(!excerpt.contains("private"));
        assert!(!excerpt.contains("user@example.com"));
        let mut payload = serde_json::json!({"failure_scope":"model","failure_model":"wrong","model_failure_soft":false}).as_object().unwrap().clone();
        failure(429, r#"{"error":{"code":"rate_limit_exceeded"}}"#).attach(&mut payload);
        assert_eq!(payload["failure_scope"], "request");
        assert_eq!(payload["failure_model"], "model-a");
        assert_eq!(payload["model_failure_soft"], true);
        failure(400, r#"{"error":{"code":"invalid_value"}}"#).attach(&mut payload);
        assert!(!payload.contains_key("failure_model"));
        assert!(!payload.contains_key("model_failure_soft"));
    }

    #[test]
    fn streamed_request_errors_and_numeric_status_keep_their_real_cause() {
        for status in [200, 400] {
            let error = failure(
                status,
                r#"{"error":{"type":"invalid_request_error","code":null,"message":"No tool call found for custom tool call output with call_id call_example."}}"#,
            );
            assert_eq!(error.cause, "continuation_invalid");
            assert!(!error.safe_to_retry());
        }
        let error = failure(
            200,
            r#"{"choices":[{"finish_reason":"error","error":{"code":403,"message":"The request is prohibited due to a violation of provider Terms Of Service."}}]}"#,
        );
        assert_eq!(error.status, 200);
        assert_eq!(error.cause, "provider_eligibility_restricted");
        assert_eq!(error.scope, "model");
    }
}
