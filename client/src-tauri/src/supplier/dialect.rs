pub(crate) const IMPROVEMENT_FAULTS_FIELD: &str = "_const_improvement_faults";

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub(crate) struct ImprovementFault {
    category: String,
    code: String,
    provider: String,
    endpoint: String,
    source_protocol: String,
    target_protocol: String,
    model: String,
    field_path: String,
    http_status: u16,
    summary: String,
    resolution: String,
    retry_attempted: bool,
    retry_succeeded: bool,
}

impl ImprovementFault {
    pub(crate) fn conversion_warning(
        code: &str,
        source_protocol: &str,
        target_protocol: &str,
        model: &str,
        field_path: &str,
        summary: &str,
    ) -> Self {
        // The conversion planner only emits provider_extension_omitted for
        // extensions explicitly marked advisory by their source adapter. Keep
        // those observations queryable, but do not present them as actionable
        // compatibility warnings alongside real semantic omissions.
        let resolution = if code.trim() == "provider_extension_omitted" {
            "advisory_extension_omitted"
        } else {
            "conversion_completed_with_warning"
        };
        Self {
            category: "protocol_conversion".to_string(),
            code: truncate_fault_text(code, 100),
            provider: String::new(),
            endpoint: String::new(),
            source_protocol: source_protocol.to_string(),
            target_protocol: target_protocol.to_string(),
            model: model.to_string(),
            field_path: truncate_fault_text(field_path, 300),
            http_status: 0,
            summary: truncate_fault_text(summary, 500),
            resolution: resolution.to_string(),
            retry_attempted: false,
            retry_succeeded: false,
        }
    }

    pub(crate) fn endpoint_adjustment(
        code: &str,
        provider: &str,
        endpoint: &str,
        source_protocol: &str,
        target_protocol: &str,
        model: &str,
        field_path: &str,
        summary: &str,
    ) -> Self {
        Self {
            category: "provider_dialect".to_string(),
            code: truncate_fault_text(code, 100),
            provider: provider.to_string(),
            endpoint: sanitize_fault_endpoint(endpoint),
            source_protocol: source_protocol.to_string(),
            target_protocol: target_protocol.to_string(),
            model: model.to_string(),
            field_path: truncate_fault_text(field_path, 300),
            http_status: 0,
            summary: truncate_fault_text(summary, 500),
            resolution: "request_adjusted_for_endpoint".to_string(),
            retry_attempted: false,
            retry_succeeded: false,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn endpoint_retry(
        code: &str,
        provider: &str,
        endpoint: &str,
        source_protocol: &str,
        target_protocol: &str,
        model: &str,
        field_path: &str,
        http_status: u16,
        succeeded: bool,
        summary: &str,
    ) -> Self {
        Self {
            category: "provider_dialect".to_string(),
            code: truncate_fault_text(code, 100),
            provider: provider.to_string(),
            endpoint: sanitize_fault_endpoint(endpoint),
            source_protocol: source_protocol.to_string(),
            target_protocol: target_protocol.to_string(),
            model: model.to_string(),
            field_path: truncate_fault_text(field_path, 300),
            http_status,
            summary: truncate_fault_text(summary, 500),
            resolution: if succeeded {
                "retry_succeeded".to_string()
            } else {
                "retry_failed".to_string()
            },
            retry_attempted: true,
            retry_succeeded: succeeded,
        }
    }

    pub(crate) fn execution_error(summary: &str) -> Option<Self> {
        if !is_program_improvable_error(summary) {
            return None;
        }
        Some(Self {
            category: "client_execution".to_string(),
            code: "program_improvable_execution_error".to_string(),
            provider: String::new(),
            endpoint: String::new(),
            source_protocol: String::new(),
            target_protocol: String::new(),
            model: String::new(),
            field_path: String::new(),
            http_status: 0,
            summary: truncate_fault_text(summary, 500),
            resolution: "not_recovered".to_string(),
            retry_attempted: false,
            retry_succeeded: false,
        })
    }

    pub(crate) fn unrecovered_provider_rejection(
        provider: &str,
        endpoint: &str,
        source_protocol: &str,
        target_protocol: &str,
        model: &str,
        status: u16,
        raw_error: &str,
    ) -> Option<Self> {
        if status != 400 {
            return None;
        }
        let summary = extract_error_summary(raw_error);
        if !is_program_improvable_error(&summary) {
            return None;
        }
        let field_path = rejected_parameter_path(&summary)
            .or_else(|| model_unsupported_parameter_path(&summary))
            .or_else(|| structured_rejected_parameter_path(raw_error))
            .or_else(|| google_unknown_parameter_path(&summary))
            .unwrap_or_default();
        Some(Self {
            category: "provider_dialect".to_string(),
            code: "provider_request_rejected".to_string(),
            provider: provider.to_string(),
            endpoint: sanitize_fault_endpoint(endpoint),
            source_protocol: source_protocol.to_string(),
            target_protocol: target_protocol.to_string(),
            model: model.to_string(),
            field_path,
            http_status: status,
            summary: truncate_fault_text(&summary, 500),
            resolution: "not_recovered".to_string(),
            retry_attempted: false,
            retry_succeeded: false,
        })
    }
}

pub(crate) fn attach_improvement_faults(
    payload: &mut serde_json::Map<String, serde_json::Value>,
    faults: &[ImprovementFault],
) {
    if faults.is_empty() {
        return;
    }
    let Ok(value) = serde_json::to_value(faults) else {
        return;
    };
    payload.insert(IMPROVEMENT_FAULTS_FIELD.to_string(), value);
}

fn extract_error_summary(raw_error: &str) -> String {
    let parsed = serde_json::from_str::<serde_json::Value>(raw_error).ok();
    if let Some(message) = parsed
        .as_ref()
        .and_then(|value| value.get("detail"))
        .and_then(serde_json::Value::as_array)
        .and_then(|details| details.first())
        .and_then(|detail| detail.get("msg"))
        .and_then(serde_json::Value::as_str)
    {
        return message.to_string();
    }
    let candidates = [
        parsed
            .as_ref()
            .and_then(|value| value.pointer("/error/message")),
        parsed.as_ref().and_then(|value| value.get("detail")),
        parsed.as_ref().and_then(|value| value.get("message")),
        parsed
            .as_ref()
            .and_then(|value| value.pointer("/error/detail")),
    ];
    candidates
        .into_iter()
        .flatten()
        .find_map(serde_json::Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| raw_error.trim().to_string())
}

fn rejected_parameter_path(summary: &str) -> Option<String> {
    let trimmed = summary.trim();
    for prefix in [
        "Unsupported parameter:",
        "Unknown parameter:",
        "Unrecognized request argument supplied:",
    ] {
        if let Some(value) = trimmed.strip_prefix(prefix) {
            return normalize_rejected_path(value);
        }
    }
    let lower = trimmed.to_ascii_lowercase();
    for prefix in [
        "unsupported parameter:",
        "unknown parameter:",
        "unrecognized request argument supplied:",
    ] {
        if let Some(index) = lower.find(prefix) {
            return normalize_rejected_path(&trimmed[index + prefix.len()..]);
        }
    }
    None
}

/// OpenAI model-specific rejections use a different sentence than the older
/// `Unsupported parameter: <field>` form. This extracts the field only for
/// diagnostics; provider errors never authorize mutating and resending a request.
fn model_unsupported_parameter_path(summary: &str) -> Option<String> {
    let trimmed = summary.trim().trim_end_matches('.').trim();
    let lower = trimmed.to_ascii_lowercase();
    let marker = [
        " is not supported on this model",
        " is not supported with this model",
        " is not supported for this model",
    ]
    .into_iter()
    .find_map(|marker| lower.find(marker).map(|index| (marker, index)))?;
    let mut candidate = trimmed[..marker.1].trim();
    for prefix in ["Unsupported parameter:", "Unknown parameter:", "Parameter:"] {
        if candidate
            .get(..prefix.len())
            .is_some_and(|value| value.eq_ignore_ascii_case(prefix))
        {
            candidate = candidate[prefix.len()..].trim();
            break;
        }
    }
    normalize_rejected_path(candidate)
}

fn normalize_rejected_path(value: &str) -> Option<String> {
    let value = value
        .trim()
        .trim_matches(|character| matches!(character, '\'' | '"' | '`'))
        .trim_end_matches('.')
        .trim()
        .trim_matches(|character| matches!(character, '\'' | '"' | '`'))
        .trim();
    if value.is_empty() || value.len() > 300 || value.chars().any(char::is_whitespace) {
        return None;
    }
    Some(value.trim_start_matches("$.").to_string())
}

fn structured_rejected_parameter_path(raw_error: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(raw_error).ok()?;
    if let Some(parameter) = value
        .pointer("/error/param")
        .and_then(serde_json::Value::as_str)
    {
        let message = value
            .pointer("/error/message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        if message.contains("unsupported") || message.contains("unknown") {
            return normalize_rejected_path(parameter);
        }
    }
    let details = value.get("detail")?.as_array()?;
    for detail in details {
        if detail.get("type").and_then(serde_json::Value::as_str) != Some("extra_forbidden") {
            continue;
        }
        let mut path = String::new();
        for segment in detail.get("loc")?.as_array()? {
            if segment.as_str() == Some("body") {
                continue;
            }
            if let Some(key) = segment.as_str() {
                if !path.is_empty() {
                    path.push('.');
                }
                path.push_str(key);
            } else if let Some(index) = segment.as_u64() {
                path.push_str(&format!("[{index}]"));
            }
        }
        if let Some(path) = normalize_rejected_path(&path) {
            return Some(path);
        }
    }
    None
}

fn google_unknown_parameter_path(summary: &str) -> Option<String> {
    let marker = "Unknown name \"";
    let start = summary.find(marker)? + marker.len();
    let end = summary[start..].find('"')? + start;
    let field = normalize_rejected_path(&summary[start..end])?;
    let remainder = &summary[end + 1..];
    let parent = remainder
        .find(" at '")
        .and_then(|start| {
            let value = &remainder[start + 5..];
            value.find('\'').map(|end| &value[..end])
        })
        .and_then(normalize_rejected_path);
    match parent {
        Some(parent) if !parent.is_empty() && parent != "request" => {
            Some(format!("{parent}.{field}"))
        }
        Some(parent) if parent == "request" => Some(format!("request.{field}")),
        _ => Some(field),
    }
}

fn is_program_improvable_error(summary: &str) -> bool {
    let lower = summary.to_ascii_lowercase();
    if [
        "timeout",
        "timed out",
        "connection",
        "connect error",
        "dns",
        "network",
        "transport closed",
        "canceled",
        "cancelled",
        "broken pipe",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        return false;
    }
    [
        "protocol",
        "conversion",
        "unsupported",
        "not supported",
        "unknown parameter",
        "invalid parameter",
        "invalid json",
        "extra inputs are not permitted",
        "cannot find field",
        "unknown field",
        "missing required parameter",
        "must be set",
        "capability",
        "surface",
        "message role",
        "schema",
        "serialize",
        "deserialize",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn truncate_fault_text(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

fn sanitize_fault_endpoint(endpoint: &str) -> String {
    if let Ok(mut url) = reqwest::Url::parse(endpoint) {
        url.set_query(None);
        url.set_fragment(None);
        let _ = url.set_username("");
        let _ = url.set_password(None);
        return truncate_fault_text(url.as_str(), 300);
    }
    let endpoint = endpoint.split_once('?').map_or(endpoint, |(path, _)| path);
    let endpoint = endpoint.split_once('#').map_or(endpoint, |(path, _)| path);
    truncate_fault_text(endpoint, 300)
}

#[cfg(test)]
mod dialect_tests {
    use super::*;

    #[test]
    fn provider_rejections_are_diagnostic_only() {
        let fault = ImprovementFault::unrecovered_provider_rejection(
            "openai",
            "https://example.test/v1/responses?secret=redacted",
            "openai_responses",
            "openai_responses",
            "gpt-5.6-sol",
            400,
            r#"{"error":{"message":"prompt_cache_retention is not supported on this model"}}"#,
        )
        .expect("known provider rejection is reportable");

        assert_eq!(fault.field_path, "prompt_cache_retention");
        assert_eq!(fault.resolution, "not_recovered");
        assert!(!fault.retry_attempted);
        assert!(!fault.retry_succeeded);
        assert!(!fault.endpoint.contains("secret"));
    }

    #[test]
    fn network_failures_are_not_improvement_faults() {
        assert!(
            ImprovementFault::execution_error("protocol conversion unsupported role").is_some()
        );
        assert!(ImprovementFault::execution_error("connection timed out").is_none());
    }

    #[test]
    fn advisory_extension_omissions_remain_visible_without_becoming_warnings() {
        let fault = ImprovementFault::conversion_warning(
            "provider_extension_omitted",
            "openai_chat",
            "gemini_native",
            "gemini-test",
            "$.messages[0].traceId",
            "advisory client metadata omitted",
        );

        assert_eq!(fault.resolution, "advisory_extension_omitted");
    }

    #[test]
    fn endpoint_retry_records_one_final_recovery_fact() {
        let recovered = ImprovementFault::endpoint_retry(
            "codex_responses_lite_rejected_fallback",
            "openai",
            "chatgpt_codex_subscription",
            "openai_responses",
            "openai_responses",
            "gpt-test",
            "$",
            400,
            true,
            "retried with the standard dialect",
        );
        assert_eq!(recovered.resolution, "retry_succeeded");
        assert!(recovered.retry_attempted);
        assert!(recovered.retry_succeeded);
        assert_eq!(recovered.http_status, 400);

        let failed = ImprovementFault::endpoint_retry(
            "codex_responses_lite_rejected_fallback",
            "openai",
            "chatgpt_codex_subscription",
            "openai_responses",
            "openai_responses",
            "gpt-test",
            "$",
            400,
            false,
            "retry failed",
        );
        assert_eq!(failed.resolution, "retry_failed");
        assert!(failed.retry_attempted);
        assert!(!failed.retry_succeeded);
    }
}
