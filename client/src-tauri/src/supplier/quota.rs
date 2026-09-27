#[derive(Debug, Clone)]
pub(crate) struct OfficialQuotaSignal {
    source: String,
    window: String,
    used_percent: f64,
    reset_at_unix: i64,
    checked_at_unix: i64,
}

impl OfficialQuotaSignal {
    fn remaining_ratio(&self) -> f64 {
        (1.0 - self.used_percent.clamp(0.0, 100.0) / 100.0).clamp(0.0, 1.0)
    }

    fn to_quota_window(&self) -> QuotaWindow {
        QuotaWindow {
            source: self.source.clone(),
            window: self.window.clone(),
            remaining_ratio: self.remaining_ratio(),
            used_percent: self.used_percent.max(0.0),
            reset_at_unix: self.reset_at_unix,
            checked_at_unix: self.checked_at_unix,
            model: None,
            token_type: None,
        }
    }
}

pub(crate) fn subscription_http_response_payload(
    status: u16,
    content_type: &str,
    body: String,
) -> serde_json::Map<String, serde_json::Value> {
    subscription_http_response_payload_with_headers(status, content_type, body, &[])
}

pub(crate) fn subscription_http_response_payload_with_headers(
    status: u16,
    content_type: &str,
    body: String,
    headers: &[(&str, &str)],
) -> serde_json::Map<String, serde_json::Value> {
    let mut payload = http_response_payload(status, content_type, body.clone());
    let mut windows = parse_codex_quota_windows(headers);
    windows.extend(parse_claude_quota_windows(headers));
    if !windows.is_empty() {
        insert_official_quota_windows(&mut payload, &windows);
    }
    let (error_kind, body_retry_after) = classify_subscription_failure(status, &body);
    if !payload.contains_key("quota_source") {
        if let Some(signal) = parse_subscription_error_quota_signal(&body) {
            windows.push(signal.to_quota_window());
            insert_official_quota_windows(&mut payload, &windows);
        }
    }
    insert_subscription_policy_metadata(&mut payload, &error_kind, headers);
    if (200..300).contains(&status) {
        return payload;
    }
    let header_retry_after = parse_subscription_retry_after(headers);
    let retry_after = header_retry_after.unwrap_or(body_retry_after);
    if !error_kind.is_empty() && error_kind != "cyber_policy" {
        payload.insert(
            "error_kind".to_string(),
            serde_json::Value::String(error_kind),
        );
        payload.insert(
            "safe_to_retry_other_channel".to_string(),
            serde_json::Value::Bool(true),
        );
        payload.insert(
            "safe_to_retry_same_channel".to_string(),
            serde_json::Value::Bool(false),
        );
    }
    if retry_after > 0 || header_retry_after == Some(0) {
        payload.insert(
            "retry_after_seconds".to_string(),
            serde_json::json!(retry_after),
        );
        payload.insert("retry_hint_source".into(), if header_retry_after.is_some() { "header" }
            else if crate::upstream_failure::observe(status, &body, &[], None)
                .is_some_and(|f| f.retry_source == "structured_body") { "structured_body" }
            else { "heuristic" }.into());
    }
    insert_subscription_model_failure_evidence(&mut payload, status, &body);
    payload
}

fn insert_subscription_policy_metadata(
    payload: &mut serde_json::Map<String, serde_json::Value>,
    error_kind: &str,
    headers: &[(&str, &str)],
) {
    if error_kind != "cyber_policy" {
        return;
    }
    payload.insert("error_kind".to_string(), error_kind.into());
    payload.insert("failure_scope".to_string(), "request".into());
    payload.insert("safe_to_retry_other_channel".to_string(), false.into());
    payload.insert("safe_to_retry_same_channel".to_string(), false.into());
    let provider_request_id = headers
        .iter()
        .find(|(name, _)| {
            name.eq_ignore_ascii_case("x-request-id") || name.eq_ignore_ascii_case("request-id")
        })
        .map(|(_, value)| value.trim())
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    payload.insert(
        "_const_policy_signal".to_string(),
        serde_json::json!({
            "version": 1,
            "code": "cyber_policy",
            "category": "cyber",
            "strength": "explicit_abuse",
            "scope": "request",
            "provider_request_id": provider_request_id,
            "retry": "none"
        }),
    );
}

pub(crate) fn insert_subscription_model_failure_evidence(
    payload: &mut serde_json::Map<String, serde_json::Value>,
    status: u16,
    body: &str,
) {
    insert_subscription_route_model_failure_evidence(payload, status, body, None);
}

pub(crate) fn insert_subscription_route_model_failure_evidence(
    payload: &mut serde_json::Map<String, serde_json::Value>,
    status: u16,
    body: &str,
    upstream_model: Option<&str>,
) {
    insert_subscription_route_model_failure_evidence_legacy(payload, status, body, upstream_model);
    if payload.get("upstream_attempted").and_then(serde_json::Value::as_bool) == Some(false)
        || payload.contains_key("failure_stage")
        || payload.get("provider_error_reason").and_then(serde_json::Value::as_str) == Some("REALTIME_ENTITLEMENT_DENIED") {
        return;
    }
    let Some(mut observed) = crate::upstream_failure::observe(status, body, &[], upstream_model) else { return; };
    if observed.cause == "unknown" { return; }
    // Official, matching quota windows may refine quota scope; a rate signal is not a depleted account.
    if matches!(observed.cause.as_str(), "account_quota" | "model_quota") || observed.rule_id == "http.rate_unknown.v1" {
        if payload_has_global_quota_evidence(payload, status) {
            observed.scope = "channel".into(); observed.cause = "account_quota".into();
        } else if let Some(model) = upstream_model.filter(|model| payload_has_exact_model_quota_evidence(payload, status, model)) {
            observed.scope = "model".into(); observed.model = model.into(); observed.cause = "model_quota".into();
        }
    }
    // Preserve a provider/header wait already captured by the HTTP boundary.
    if let Some(seconds) = payload.get("retry_after_seconds").and_then(serde_json::Value::as_i64) {
        observed.retry_after_seconds = Some(seconds);
        observed.retry_source = payload.get("retry_hint_source").and_then(serde_json::Value::as_str).unwrap_or("heuristic").into();
    }
    if observed.retry_after_seconds.is_none() && matches!(observed.cause.as_str(), "location_unsupported" | "provider_eligibility_restricted" | "account_quota" | "model_entitlement") {
        observed.retry_after_seconds = Some(1800); observed.retry_source = "heuristic".into();
    }
    observed.attach(payload);
}

fn insert_subscription_route_model_failure_evidence_legacy(
    payload: &mut serde_json::Map<String, serde_json::Value>,
    status: u16,
    body: &str,
    upstream_model: Option<&str>,
) {
    if status == 403 && body.to_ascii_lowercase().contains("voice session access denied") {
        payload.insert("failure_scope".to_string(), "request".into());
        payload.remove("failure_model");
        payload.insert(
            "provider_error_reason".to_string(),
            "REALTIME_ENTITLEMENT_DENIED".into(),
        );
        return;
    }
    if subscription_location_unsupported(status, body) {
        payload.insert("failure_scope".to_string(), "channel".into());
        payload.remove("failure_model");
        payload.insert(
            "provider_error_reason".to_string(),
            "USER_LOCATION_UNSUPPORTED".into(),
        );
        return;
    }
    if status == 402 {
        payload.insert("failure_scope".to_string(), "channel".into());
        payload.remove("failure_model");
        payload.insert(
            "provider_error_reason".to_string(),
            "GLOBAL_QUOTA_LIMIT".into(),
        );
        return;
    }
    if let Some(scope) = payload
        .get("failure_scope")
        .and_then(serde_json::Value::as_str)
    {
        if scope.eq_ignore_ascii_case("operation")
            || scope.eq_ignore_ascii_case("channel")
            || (scope.eq_ignore_ascii_case("model")
                && payload
                    .get("failure_model")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|model| !model.trim().is_empty()))
        {
            return;
        }
    }
    if let Some(model) = model_capacity_failure_model(status, body) {
        insert_model_failure_evidence(payload, &model, "MODEL_CAPACITY_EXHAUSTED");
        return;
    }
    let upstream_model = upstream_model
        .map(str::trim)
        .filter(|model| !model.is_empty());
    if payload_has_global_quota_evidence(payload, status) {
        payload.insert("failure_scope".to_string(), "channel".into());
        payload.remove("failure_model");
        payload.insert(
            "provider_error_reason".to_string(),
            "GLOBAL_QUOTA_LIMIT".into(),
        );
        return;
    }
    if let Some(model) =
        upstream_model.filter(|model| provider_model_unavailable(status, body, model))
    {
        insert_model_failure_evidence(payload, model, "MODEL_UNAVAILABLE");
        return;
    }
    if let Some(model) = upstream_model
        .filter(|model| payload_has_exact_model_quota_evidence(payload, status, model))
    {
        insert_model_failure_evidence(payload, model, "MODEL_QUOTA_LIMIT");
        return;
    }
    // Weak availability evidence lowers only this route's preference. The
    // caller still checks the operation before applying model health; resource
    // lookup errors and metadata helpers cannot condemn model execution.
    let kind = payload
        .get("error_kind")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    // A bare 404 can reference a missing response/item/file, not a bad model.
    // Explicit model-not-found evidence was handled above.
    if (matches!(status, 403 | 429) || (status >= 500 && status != 501))
        && matches!(
            kind,
            "" | "permission_denied" | "rate_limited" | "quota_exhausted" | "provider_overload"
        )
        && payload
            .get("upstream_attempted")
            .and_then(serde_json::Value::as_bool)
            != Some(false)
        && !payload.contains_key("failure_stage")
    {
        if let Some(model) = upstream_model {
            insert_model_failure_evidence(payload, model, "MODEL_ROUTE_UNAVAILABLE");
            // Old servers do not understand soft penalties and would turn an
            // ambiguous 429 into a long hard model ban. Keep their request-local
            // behavior; new servers and local routing understand the extension.
            payload.insert("failure_scope".to_string(), "request".into());
            payload.insert("model_failure_soft".to_string(), true.into());
            return;
        }
        payload.insert("failure_scope".to_string(), "request".into());
        payload.remove("failure_model");
    }
    if status == 404 {
        payload.insert("failure_scope".to_string(), "request".into());
        payload.remove("failure_model");
    }
}

fn insert_model_failure_evidence(
    payload: &mut serde_json::Map<String, serde_json::Value>,
    model: &str,
    reason: &str,
) {
    payload.insert("failure_scope".to_string(), "model".into());
    payload.insert("failure_model".to_string(), model.to_string().into());
    payload.insert(
        "provider_error_reason".to_string(),
        reason.to_string().into(),
    );
}

fn provider_model_unavailable(status: u16, body: &str, upstream_model: &str) -> bool {
    if !matches!(status, 400 | 403 | 404 | 422 | 429 | 503) {
        return false;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    let error = value.get("error").unwrap_or(&value);
    let code = [
        error.get("code"),
        error.get("type"),
        value.get("code"),
        value.get("status"),
    ]
    .into_iter()
    .flatten()
    .filter_map(serde_json::Value::as_str)
    .map(str::trim)
    .find(|value| !value.is_empty())
    .unwrap_or_default();
    if matches!(
        code.to_ascii_lowercase().as_str(),
        "model_not_found"
            | "model_not_supported"
            | "unsupported_model"
            | "unknown_model"
            | "model_access_denied"
            | "model_not_allowed"
    ) {
        return true;
    }
    let message = error
        .get("message")
        .or_else(|| value.get("message"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let model = upstream_model.to_ascii_lowercase();
    message.contains(&model)
        && message.contains("model")
        && (message.contains("not found")
            || message.contains("does not exist")
            || message.contains("not supported"))
}

fn payload_has_exact_model_quota_evidence(
    payload: &serde_json::Map<String, serde_json::Value>,
    status: u16,
    upstream_model: &str,
) -> bool {
    let kind = payload
        .get("error_kind")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if status != 429
        && !kind.eq_ignore_ascii_case("quota_exhausted")
        && !kind.eq_ignore_ascii_case("rate_limited")
    {
        return false;
    }
    let Some(windows) = payload
        .get("quota_windows")
        .and_then(serde_json::Value::as_array)
        .filter(|windows| !windows.is_empty())
    else {
        return false;
    };
    let mut found_model = false;
    for window in windows {
        let Some(model) = window
            .get("model")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|model| !model.is_empty())
        else {
            continue;
        };
        if normalize_model_name(model) == normalize_model_name(upstream_model)
            && payload_quota_window_exhausted(window)
        {
            found_model = true;
        }
    }
    found_model
}

fn payload_has_global_quota_evidence(
    payload: &serde_json::Map<String, serde_json::Value>,
    status: u16,
) -> bool {
    let kind = payload
        .get("error_kind")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if status != 429
        && !kind.eq_ignore_ascii_case("quota_exhausted")
        && !kind.eq_ignore_ascii_case("rate_limited")
    {
        return false;
    }
    payload
        .get("quota_windows")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|windows| {
            windows.iter().any(|window| {
                window
                    .get("model")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .is_none_or(str::is_empty)
                    && payload_quota_window_exhausted(window)
            })
        })
}

fn payload_quota_window_exhausted(window: &serde_json::Value) -> bool {
    let remaining = window.get("remaining_ratio").and_then(serde_json::Value::as_f64);
    let used = window.get("used_percent").and_then(serde_json::Value::as_f64);
    if remaining.is_none() && used.is_none() { return false; }
    // Older payloads omit source/window fields. Retain their explicit quota
    // evidence, without interpreting a missing numeric value as zero balance.
    let signal = QuotaWindow {
        source: window.get("source").and_then(serde_json::Value::as_str).unwrap_or_default().into(),
        window: window.get("window").and_then(serde_json::Value::as_str).unwrap_or_default().into(),
        remaining_ratio: remaining.unwrap_or_else(|| (1.0 - used.unwrap_or_default() / 100.0).clamp(0.0, 1.0)),
        used_percent: used.unwrap_or_else(|| (1.0 - remaining.unwrap_or(1.0)) * 100.0),
        model: window.get("model").and_then(serde_json::Value::as_str).map(str::to_string),
        token_type: window.get("token_type").and_then(serde_json::Value::as_str).map(str::to_string),
        ..Default::default()
    };
    let legacy_unscoped = signal.source.trim().is_empty() && signal.window.trim().is_empty()
        && signal.token_type.as_deref().is_none_or(|bucket| bucket.trim().is_empty());
    signal.remaining_ratio.is_finite() && signal.used_percent.is_finite()
        && signal.remaining_ratio >= 0.0 && (legacy_unscoped || quota_window_limits_supply(&signal))
        && quota_window_is_exhausted(&signal)
}

fn model_capacity_failure_model(status: u16, body: &str) -> Option<String> {
    if status != 429 && status != 503 {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let error = value.get("error").unwrap_or(&value);
    let details = error.get("details").and_then(serde_json::Value::as_array);
    let structured_model = details.and_then(|details| {
        details.iter().find_map(|detail| {
            let reason = detail.get("reason").and_then(serde_json::Value::as_str)?;
            if !reason.eq_ignore_ascii_case("MODEL_CAPACITY_EXHAUSTED") {
                return None;
            }
            detail
                .get("metadata")
                .and_then(|metadata| metadata.get("model"))
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|model| !model.is_empty())
                .map(str::to_string)
        })
    });
    if structured_model.is_some() {
        return structured_model;
    }
    let message = error
        .get("message")
        .and_then(serde_json::Value::as_str)?
        .trim();
    message
        .strip_prefix("No capacity available for model ")
        .and_then(|value| value.strip_suffix(" on the server"))
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string)
}

/// Merge observations without treating a partial response as a complete account
/// snapshot. Never stamp cached data with the time it was read.
pub(crate) fn merge_official_quota_windows(
    current: &mut Vec<QuotaWindow>,
    observed: &[QuotaWindow],
) {
    for window in observed.iter().filter(|window| {
        !window.source.is_empty()
            && !window.window.is_empty()
            && window.used_percent.is_finite()
            && window.remaining_ratio.is_finite()
    }) {
        // Antigravity's catalog and usage RPC describe the same per-model quota
        // with different bucket names. Do not keep the older source alongside
        // a newer one (the stale lower balance would keep winning the summary).
        let antigravity = |value: &QuotaWindow| {
            matches!(
                value.source.as_str(),
                "antigravity_fetch_available_models" | "antigravity_retrieve_user_quota"
            )
        };
        if antigravity(window) && window.model.is_some() {
            if current.iter().any(|old| {
                antigravity(old)
                    && old.model == window.model
                    && old.source != window.source
                    && old.checked_at_unix > window.checked_at_unix
            }) {
                continue;
            }
            current.retain(|old| {
                !(antigravity(old)
                    && old.model == window.model
                    && old.source != window.source
                    && old.checked_at_unix <= window.checked_at_unix)
            });
        }
        // Codex headers predate token_type; its default bucket is also "codex".
        let bucket = |window: &QuotaWindow| {
            window
                .token_type
                .as_deref()
                .filter(|value| !value.is_empty() && *value != "codex")
                .unwrap_or("")
                .to_string()
        };
        if let Some(previous) = current.iter_mut().find(|previous| {
            previous.window == window.window
                && previous.model == window.model
                && bucket(previous) == bucket(window)
        }) {
            if window.checked_at_unix >= previous.checked_at_unix {
                *previous = window.clone();
            }
        } else {
            current.push(window.clone());
        }
    }
}

pub(crate) fn insert_official_quota_windows(
    payload: &mut serde_json::Map<String, serde_json::Value>,
    windows: &[QuotaWindow],
) {
    // Preserve auxiliary windows for display even when they cannot gate supply.
    payload.insert("quota_windows".to_string(), serde_json::json!(windows));
    let Some(summary) = channel_quota_summary(windows) else {
        return;
    };
    let representative = &summary.representative;
    payload.insert(
        "quota_source".to_string(),
        serde_json::Value::String(summary_quota_source(&representative.source)),
    );
    payload.insert(
        "quota_window".to_string(),
        serde_json::Value::String(representative.window.clone()),
    );
    payload.insert(
        "quota_used_percent".to_string(),
        serde_json::json!(representative.used_percent),
    );
    payload.insert(
        "quota_reset_at_unix".to_string(),
        serde_json::json!(representative.reset_at_unix),
    );
    payload.insert(
        "quota_checked_at_unix".to_string(),
        serde_json::json!(representative.checked_at_unix),
    );
}

pub(crate) fn response_header_pairs(headers: &reqwest::header::HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_string(), value.to_string()))
        })
        .collect()
}

#[derive(Default)]
struct CodexQuotaHeaderGroup {
    label: String,
    used_percent: Option<f64>,
    reset_after_seconds: Option<i64>,
    reset_at_unix: Option<i64>,
    window_minutes: Option<i64>,
}

pub(crate) fn parse_codex_quota_windows(headers: &[(&str, &str)]) -> Vec<QuotaWindow> {
    let mut groups: std::collections::BTreeMap<String, CodexQuotaHeaderGroup> =
        std::collections::BTreeMap::new();
    for (name, value) in headers {
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        let Some(rest) = name.strip_prefix("x-codex-") else {
            continue;
        };
        let Some((label, suffix)) = rest.split_once('-') else {
            continue;
        };
        let group = groups
            .entry(label.to_string())
            .or_insert_with(|| CodexQuotaHeaderGroup {
                label: label.to_string(),
                ..Default::default()
            });
        match suffix {
            "used-percent" => group.used_percent = value.parse::<f64>().ok(),
            "reset-after-seconds" => group.reset_after_seconds = value.parse::<i64>().ok(),
            "reset-at" => group.reset_at_unix = value.parse::<i64>().ok(),
            "window-minutes" => group.window_minutes = value.parse::<i64>().ok(),
            _ => {}
        }
    }
    groups
        .into_values()
        .filter_map(codex_quota_signal_from_group)
        .map(|signal| QuotaWindow {
            source: "codex_header".to_string(),
            ..signal.to_quota_window()
        })
        .collect()
}

#[derive(Default)]
struct ClaudeUnifiedQuotaHeaderGroup {
    window: String,
    utilization: Option<f64>,
    reset_at_unix: Option<i64>,
}

pub(crate) fn parse_claude_quota_windows(headers: &[(&str, &str)]) -> Vec<QuotaWindow> {
    let mut unified: std::collections::BTreeMap<String, ClaudeUnifiedQuotaHeaderGroup> =
        std::collections::BTreeMap::new();
    for (name, value) in headers {
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        if let Some(rest) = name.strip_prefix("anthropic-ratelimit-unified-") {
            let Some((window, suffix)) = rest.split_once('-') else {
                continue;
            };
            if !matches!(window, "5h" | "7d" | "7d_oi") {
                continue;
            }
            let group = unified.entry(window.to_string()).or_insert_with(|| {
                ClaudeUnifiedQuotaHeaderGroup {
                    window: claude_quota_window_name(window),
                    ..Default::default()
                }
            });
            match suffix {
                "utilization" => group.utilization = value.parse::<f64>().ok(),
                "reset" | "reset-at" | "resets-at" => {
                    group.reset_at_unix = parse_quota_reset_unix(value)
                }
                _ => {}
            }
            continue;
        }
    }
    let checked_at_unix = now_unix();
    unified
        .into_values()
        .filter_map(|group| {
            let used_percent = group.utilization.map(|value| value * 100.0)
                .filter(|value| value.is_finite())?.max(0.0);
            Some(QuotaWindow {
                source: "anthropic_header".to_string(),
                window: group.window,
                remaining_ratio: (1.0 - used_percent / 100.0).clamp(0.0, 1.0),
                used_percent,
                reset_at_unix: group.reset_at_unix.unwrap_or_default(),
                checked_at_unix,
                model: None,
                token_type: None,
            })
        })
        .collect()
}

pub(crate) fn parse_grok_quota_windows(headers: &[(&str, &str)]) -> Vec<QuotaWindow> {
    let header = |name: &str| {
        headers
            .iter()
            .find(|(candidate, _)| candidate.trim().eq_ignore_ascii_case(name))
            .map(|(_, value)| value.trim())
    };
    let checked_at_unix = now_unix();
    ["requests", "tokens"]
        .into_iter()
        .filter_map(|dimension| {
            let limit = header(&format!("x-ratelimit-limit-{dimension}"))?
                .parse::<f64>()
                .ok()?;
            let remaining = header(&format!("x-ratelimit-remaining-{dimension}"))?
                .parse::<f64>()
                .ok()?;
            if !limit.is_finite() || limit <= 0.0 || !remaining.is_finite() {
                return None;
            }
            let remaining_ratio = (remaining / limit).clamp(0.0, 1.0);
            let reset_at_unix = header(&format!("x-ratelimit-reset-{dimension}"))
                .and_then(parse_grok_quota_reset_unix)
                .unwrap_or_default();
            Some(QuotaWindow {
                source: "grok_header".to_string(),
                window: dimension.to_string(),
                remaining_ratio,
                used_percent: (1.0 - remaining_ratio) * 100.0,
                reset_at_unix,
                checked_at_unix,
                model: None,
                token_type: Some(dimension.to_string()),
            })
        })
        .collect()
}

fn parse_grok_quota_reset_unix(value: &str) -> Option<i64> {
    let value = value.trim();
    if let Ok(raw) = value.parse::<i64>() {
        return Some(if raw > 1_000_000_000_000 {
            raw / 1000
        } else {
            raw
        })
        .filter(|value| *value > 0);
    }
    time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .ok()
        .map(|value| value.unix_timestamp())
}

fn parse_quota_reset_unix(value: &str) -> Option<i64> {
    parse_grok_quota_reset_unix(value)
}

fn claude_quota_window_name(window: &str) -> String {
    match window {
        "7d" => "weekly".to_string(),
        "7d_oi" => "weekly_overage_included".to_string(),
        other => other.to_string(),
    }
}

pub(crate) fn summary_quota_source(source: &str) -> String {
    match source {
        "codex_header"
        | "codex_rate_limits"
        | "openai_wham_usage"
        | "anthropic_header"
        | "anthropic_oauth_usage"
        | "grok_header"
        | "grok_billing"
        | "kimi_coding_usage"
        | "glm_coding_usage"
        | "minimax_token_usage"
        | "gemini_retrieve_user_quota"
        | "antigravity_retrieve_user_quota"
        | "antigravity_fetch_available_models" => "official_header".to_string(),
        other => other.to_string(),
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ScopedQuotaSummary {
    pub(crate) remaining_ratio: f64,
    pub(crate) status: String,
    pub(crate) representative: QuotaWindow,
}

fn is_antigravity_quota_source(source: &str) -> bool {
    matches!(source, "antigravity_retrieve_user_quota" | "antigravity_fetch_available_models")
}

pub(crate) fn quota_window_period_seconds(window: &str) -> Option<u64> {
    let normalized = window.rsplit(':').next()?.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "hourly" => return Some(3600),
        "daily" => return Some(86400),
        "weekly" => return Some(604800),
        _ => {}
    }
    let split = normalized.find(|ch: char| !ch.is_ascii_digit())?;
    let amount = normalized[..split].parse::<u64>().ok().filter(|value| *value > 0)?;
    let factor = match &normalized[split..] {
        "s" => 1, "m" => 60, "h" => 3600, "d" => 86400, "w" => 604800,
        _ => return None,
    };
    amount.checked_mul(factor)
}

// Only primary account windows gate supply; model/feature/overage buckets do not.
// Unknown-period account windows can still gate supply even though
// supplierQuota.tsx displays them in its collapsed section.
// Antigravity intentionally retains its existing per-model quota semantics.
pub(crate) fn quota_window_limits_supply(window: &QuotaWindow) -> bool {
    if is_antigravity_quota_source(&window.source) {
        return true;
    }
    let standard_bucket = |bucket: &str| matches!(bucket.trim().to_ascii_lowercase().as_str(),
        "" | "codex" | "default" | "global" | "all");
    let model_scoped = window.model.as_deref().is_some_and(|model| !model.trim().is_empty());
    let account_source = matches!(window.source.as_str(), "anthropic_oauth_usage" | "anthropic_header"
        | "openai_wham_usage" | "codex_header" | "codex_rate_limits" | "grok_billing" | "grok_header");
    if (model_scoped && account_source) || !standard_bucket(window.token_type.as_deref().unwrap_or_default()) {
        return false;
    }
    let (bucket, period) = window.window.rsplit_once(':').unwrap_or(("", &window.window));
    if !standard_bucket(bucket) {
        return false;
    }
    if window.source == "official_error" {
        return true;
    }
    if !account_source && model_scoped && period.trim().is_empty() {
        // Preserve legacy/custom exact-model quota reports with no period.
        return true;
    }
    // Grok's monthly on-demand spending cap is not the subscription balance.
    if window.source == "grok_billing" && period.eq_ignore_ascii_case("monthly") {
        return false;
    }
    quota_window_period_seconds(period).is_some()
        || matches!(period.trim().to_ascii_lowercase().as_str(), "primary" | "secondary" | "monthly")
}

fn quota_window_is_exhausted(window: &QuotaWindow) -> bool {
    let remaining = quota_window_effective_ratio(window);
    if is_antigravity_quota_source(&window.source) {
        quota_remaining_is_exhausted(remaining)
    } else {
        remaining <= 0.0
    }
}

fn quota_window_effective_ratio(window: &QuotaWindow) -> f64 {
    if window.used_percent >= 100.0 {
        0.0
    } else {
        window.remaining_ratio.clamp(0.0, 1.0)
    }
}

fn lower_quota_window(current: Option<QuotaWindow>, candidate: &QuotaWindow) -> QuotaWindow {
    match current {
        Some(current)
            if quota_window_effective_ratio(&current)
                <= quota_window_effective_ratio(candidate) =>
        {
            current
        }
        _ => candidate.clone(),
    }
}

pub(crate) fn scoped_quota_summary(windows: &[QuotaWindow]) -> Option<ScopedQuotaSummary> {
    let mut global: Option<QuotaWindow> = None;
    let mut models = std::collections::HashMap::<String, QuotaWindow>::new();
    for window in windows
        .iter()
        .filter(|window| !window.source.trim().is_empty() && !window.window.trim().is_empty()
            && window.remaining_ratio.is_finite() && window.used_percent.is_finite()
            && quota_window_limits_supply(window))
    {
        let model = window
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty());
        if let Some(model) = model {
            let key = model.to_ascii_lowercase();
            let tightest = lower_quota_window(models.remove(&key), window);
            models.insert(key, tightest);
        } else {
            global = Some(lower_quota_window(global, window));
        }
    }

    let best_model = models.into_values().max_by(|a, b| {
        quota_window_effective_ratio(a)
            .partial_cmp(&quota_window_effective_ratio(b))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let representative = match (global, best_model) {
        (Some(global), Some(model))
            if quota_window_effective_ratio(&global) <= quota_window_effective_ratio(&model) =>
        {
            global
        }
        (Some(_), Some(model)) => model,
        (Some(global), None) => global,
        (None, Some(model)) => model,
        (None, None) => return None,
    };
    let remaining_ratio = quota_window_effective_ratio(&representative);
    let status = if quota_window_is_exhausted(&representative) {
        "exhausted"
    } else if remaining_ratio < 0.2 {
        "low"
    } else {
        "available"
    };
    Some(ScopedQuotaSummary {
        remaining_ratio,
        status: status.to_string(),
        representative,
    })
}

pub(crate) fn quota_remaining_is_exhausted(remaining_ratio: f64) -> bool {
    remaining_ratio <= 0.050_000_001
}

// Model windows may be only a subset of the channel's models. Preserve them
// for exact-model admission, but never infer a global outage from that subset.
pub(crate) fn channel_quota_summary(windows: &[QuotaWindow]) -> Option<ScopedQuotaSummary> {
    let global: Vec<_> = windows
        .iter()
        .filter(|window| {
            quota_window_limits_supply(window) && window
                .model
                .as_deref()
                .is_none_or(|model| model.trim().is_empty())
        })
        .cloned()
        .collect();
    if !global.is_empty() {
        return scoped_quota_summary(&global);
    }
    let mut summary = scoped_quota_summary(windows)?;
    if summary.status == "exhausted" {
        summary.status = "unknown".to_string();
    }
    Some(summary)
}

pub(crate) fn model_quota_route_key(channel_id: &str, model: &str) -> String {
    format!(
        "{}\u{0}{}",
        channel_id.trim(),
        normalize_model_name(model.trim())
    )
}

const LOCAL_MODEL_RUNTIME_PROBE_LEASE_SECONDS: i64 = 5 * 60;
static LOCAL_MODEL_FAILURE_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub(crate) fn local_model_failure_generation() -> u64 {
    LOCAL_MODEL_FAILURE_SEQUENCE.load(std::sync::atomic::Ordering::Relaxed)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct LocalModelQuotaRouteState {
    // Official quota observations and runtime execution health are kept
    // separately. A fresh quota snapshot proves that quota exists; it does not
    // prove that the provider can currently execute this particular model.
    pub(crate) cooldown_until_unix: i64,
    pub(crate) failed_at_unix: i64,
    pub(crate) runtime_cooldown_until_unix: i64,
    pub(crate) runtime_failed_at_unix: i64,
    pub(crate) runtime_failure_count: u32,
    pub(crate) runtime_failure_sequence: u64,
    pub(crate) runtime_soft_failure: bool,
    // A timestamp lease prevents a cancelled half-open request from leaving
    // this route blocked forever. Normal completion releases it immediately.
    pub(crate) runtime_probe_until_unix: i64,
}

impl LocalModelQuotaRouteState {
    pub(crate) fn available_at(self, now: i64) -> bool {
        (self.cooldown_until_unix <= 0 || self.cooldown_until_unix <= now)
            && (self.runtime_cooldown_until_unix <= 0
                || self.runtime_cooldown_until_unix <= now)
            && (self.runtime_probe_until_unix <= 0 || self.runtime_probe_until_unix <= now)
    }

    pub(crate) fn claim_runtime_probe(&mut self, now: i64) -> bool {
        if !self.available_at(now) && !self.available_as_last_resort(now) {
            return false;
        }
        if self.runtime_failure_count > 0 {
            self.runtime_probe_until_unix =
                now.saturating_add(LOCAL_MODEL_RUNTIME_PROBE_LEASE_SECONDS);
        }
        true
    }

    pub(crate) fn release_runtime_probe(&mut self) {
        self.runtime_probe_until_unix = 0;
    }

    pub(crate) fn available_as_last_resort(self, now: i64) -> bool {
        self.runtime_soft_failure
            && self.cooldown_until_unix <= now
            && self.runtime_probe_until_unix <= now
    }

    pub(crate) fn record_runtime_failure(&mut self, now: i64, retry_after_seconds: i64, soft: bool) {
        let hard_until = if self.runtime_soft_failure {
            0
        } else {
            self.runtime_cooldown_until_unix
        };
        self.runtime_failure_sequence =
            LOCAL_MODEL_FAILURE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.runtime_failure_count = self.runtime_failure_count.saturating_add(1);
        self.runtime_failed_at_unix = now;
        self.runtime_probe_until_unix = 0;
        // Only ambiguous availability failures without an upstream retry hint
        // may be used as a last resort. Never bypass an existing hard limit.
        self.runtime_soft_failure = soft && retry_after_seconds <= 0 && hard_until <= now;
        let shift = self.runtime_failure_count.saturating_sub(1).min(3);
        let cooldown = (60_i64 << shift).min(5 * 60).max(retry_after_seconds);
        self.runtime_cooldown_until_unix = now.saturating_add(cooldown).max(hard_until);
    }

    pub(crate) fn record_success(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn record_success_observed(&mut self, sequence: u64) -> bool {
        if self.runtime_failure_sequence != sequence {
            return false;
        }
        self.record_success();
        true
    }

    pub(crate) fn has_runtime_failure(self) -> bool {
        self.runtime_failure_count > 0 || self.runtime_cooldown_until_unix > 0
    }
}

fn codex_quota_signal_from_group(group: CodexQuotaHeaderGroup) -> Option<OfficialQuotaSignal> {
    let used_percent = group.used_percent.filter(|value| value.is_finite())?;
    let checked_at_unix = now_unix();
    let reset_at_unix = group
        .reset_at_unix
        .filter(|value| *value > 0)
        .or_else(|| {
            group
                .reset_after_seconds
                .filter(|value| *value > 0)
                .map(|seconds| checked_at_unix.saturating_add(seconds))
        })
        .unwrap_or_default();
    Some(OfficialQuotaSignal {
        source: "official_header".to_string(),
        window: if matches!(group.label.as_str(), "primary" | "secondary") {
            codex_quota_window_name(&group.label, group.window_minutes)
        } else {
            format!("{}:{}", group.label, codex_quota_window_name(&group.label, group.window_minutes))
        },
        used_percent: used_percent.max(0.0),
        reset_at_unix,
        checked_at_unix,
    })
}

fn codex_quota_window_name(label: &str, window_minutes: Option<i64>) -> String {
    match window_minutes {
        Some(300) => "5h".to_string(),
        Some(10080) => "weekly".to_string(),
        Some(minutes) if minutes > 0 => format!("{minutes}m"),
        _ => label.to_string(),
    }
}

pub(crate) fn parse_subscription_error_quota_signal(body: &str) -> Option<OfficialQuotaSignal> {
    let value = serde_json::from_str::<serde_json::Value>(body).ok()?;
    let error_type = subscription_error_type(&value);
    if error_type != "usage_limit_reached" {
        return None;
    }
    let checked_at_unix = now_unix();
    let reset_after = subscription_error_reset_after_seconds(&value, checked_at_unix)
        .filter(|seconds| *seconds > 0)?;
    Some(OfficialQuotaSignal {
        source: "official_error".to_string(),
        window: "unknown".to_string(),
        used_percent: 100.0,
        reset_at_unix: checked_at_unix + reset_after,
        checked_at_unix,
    })
}

fn subscription_error_type(value: &serde_json::Value) -> String {
    value
        .pointer("/error/type")
        .or_else(|| value.pointer("/error/code"))
        .or_else(|| value.get("type"))
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn subscription_error_reset_after_seconds(value: &serde_json::Value, now: i64) -> Option<i64> {
    value
        .pointer("/error/resets_in_seconds")
        .or_else(|| value.get("resets_in_seconds"))
        .and_then(|value| value.as_i64())
        .or_else(|| {
            value
                .pointer("/error/resets_at")
                .or_else(|| value.get("resets_at"))
                .and_then(|value| {
                    if let Some(unix) = value.as_i64() {
                        Some(unix.saturating_sub(now).max(0))
                    } else {
                        value
                            .as_str()
                            .and_then(|raw| raw.trim().parse::<i64>().ok())
                            .map(|unix| unix.saturating_sub(now).max(0))
                    }
                })
        })
}

pub(crate) fn parse_subscription_retry_after(headers: &[(&str, &str)]) -> Option<i64> {
    crate::upstream_failure::retry_after(headers, now_unix())
}

pub(crate) fn subscription_sse_terminal_error_payload(
    raw: &str,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    // Bound each event, not the complete stream. A long successful prefix must
    // not hide a small terminal error. Avoid copying the entire accumulated SSE.
    let mut body = String::new();
    let mut oversized = false;
    for line in raw.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if !oversized {
                if let Some(observed) = crate::upstream_failure::observe(200, &body, &[], None) {
                    return Some(subscription_http_response_payload(
                        observed.effective_status(), "text/event-stream", body,
                    ));
                }
            }
            body.clear();
            oversized = false;
        } else if let Some(data) = line.strip_prefix("data:").filter(|_| !oversized) {
            let data = data.trim_start();
            if body.len().saturating_add(data.len()).saturating_add(1)
                > crate::upstream_failure::MAX_OBSERVATION_BYTES
            {
                body.clear();
                oversized = true;
                continue;
            }
            if !body.is_empty() { body.push('\n'); }
            body.push_str(data);
        }
    }
    None
}

pub(crate) fn classify_subscription_failure(status: u16, body: &str) -> (String, i64) {
    let Some(failure) = crate::upstream_failure::observe(status, body, &[], None) else { return (String::new(), 0); };
    let fallback = if matches!(failure.cause.as_str(), "account_quota" | "location_unsupported" | "provider_eligibility_restricted") { 1800 } else { 0 };
    (failure.legacy_kind().to_string(), failure.retry_after_seconds.unwrap_or(fallback))
}

fn subscription_location_unsupported(status: u16, body: &str) -> bool {
    if status == 451 {
        return true;
    }
    if !matches!(status, 400 | 403) {
        return false;
    }
    let lower = body.to_ascii_lowercase();
    lower.contains("user location is not supported")
        || lower.contains("location is not supported for the api")
        || lower.contains("api use is not supported in your location")
}

#[cfg(test)]
mod tests {
    use super::{
        classify_subscription_failure, parse_claude_quota_windows,
        subscription_http_response_payload, subscription_http_response_payload_with_headers,
        LocalModelQuotaRouteState,
    };

    #[test]
    fn terminal_sse_error_survives_large_prefix_and_oversized_events() {
        let error = "event: error\ndata: {\"error\":{\"type\":\"overloaded_error\",\"message\":\"overloaded\"}}\n\n";
        let expected = super::subscription_sse_terminal_error_payload(error).unwrap();
        let prefix = format!("data: {{\"type\":\"response.output_text.delta\",\"delta\":\"{}\"}}\n\n", "x".repeat(2048));
        for raw in [
            format!("{}{error}", prefix.repeat(150)),
            format!("data: {}\n\n{error}", "x".repeat(crate::upstream_failure::MAX_OBSERVATION_BYTES + 1)),
            format!("{}{error}", prefix.repeat(150)).replace('\n', "\r\n"),
        ] {
            assert!(raw.len() > crate::upstream_failure::MAX_OBSERVATION_BYTES);
            assert_eq!(super::subscription_sse_terminal_error_payload(&raw), Some(expected.clone()));
        }
        assert!(super::subscription_sse_terminal_error_payload(&prefix.repeat(150)).is_none());
        let raw = format!("data: {}\n{error}", "x".repeat(crate::upstream_failure::MAX_OBSERVATION_BYTES + 1));
        assert!(super::subscription_sse_terminal_error_payload(&raw).is_none(), "do not parse a fragment of an oversized event");
    }

    #[test]
    fn legacy_quota_payload_retains_explicit_exhaustion_without_promoting_extras() {
        use serde_json::json;
        for window in [json!({"remaining_ratio":0}), json!({"used_percent":100}), json!({"used_percent":120}), json!({"remaining_ratio":0,"model":"gpt-luna"})] {
            assert!(super::payload_quota_window_exhausted(&window), "{window}");
        }
        for window in [json!({}), json!({"remaining_ratio":0.5}), json!({"remaining_ratio":-1}),
            json!({"remaining_ratio":0,"token_type":"image"}),
            json!({"source":"anthropic_oauth_usage","window":"weekly_sonnet","remaining_ratio":0}),
            json!({"source":"grok_billing","window":"monthly","remaining_ratio":0})] {
            assert!(!super::payload_quota_window_exhausted(&window), "{window}");
        }
        let payload = json!({"quota_windows":[{"remaining_ratio":0}]});
        assert!(super::payload_has_global_quota_evidence(payload.as_object().unwrap(), 429));
        assert!(!super::payload_has_global_quota_evidence(payload.as_object().unwrap(), 200));
    }

    #[test]
    fn header_quota_preserves_overage_but_never_negative_remaining() {
        let claude = parse_claude_quota_windows(&[("anthropic-ratelimit-unified-5h-utilization", "1.2")]);
        let openai = super::parse_codex_quota_windows(&[("x-codex-primary-used-percent", "120")]);
        for windows in [claude, openai] {
            assert_eq!(windows.len(), 1);
            assert_eq!(windows[0].used_percent, 120.0);
            assert_eq!(windows[0].remaining_ratio, 0.0);
            assert_eq!(super::channel_quota_summary(&windows).unwrap().status, "exhausted");
        }
        assert!(parse_claude_quota_windows(&[("anthropic-ratelimit-unified-5h-utilization", "1e308")]).is_empty());
    }

    #[test]
    fn quota_period_grammar_matches_server_and_colon_buckets_remain_auxiliary() {
        for period in ["5h", "01h", "300m", "codex:5H", " 7d ", "weekly", "18446744073709551615s"] {
            assert!(super::quota_window_limits_supply(&crate::model::QuotaWindow {
                source: "codex_header".into(), window: period.into(), ..Default::default()
            }), "{period}");
        }
        for period in ["0h", "+1h", "1.5h", "5 h", "1ms", "1h2m", "18446744073709551615w", "18446744073709551616s", "feature:preview:5h"] {
            assert!(!super::quota_window_limits_supply(&crate::model::QuotaWindow {
                source: "codex_header".into(), window: period.into(), ..Default::default()
            }), "{period}");
        }
        assert!(reqwest::header::HeaderName::from_bytes(b"x-codex-feature:preview-used-percent").is_err());
        assert_eq!(super::quota_window_period_seconds("feature:preview:5h"), Some(18000));
    }

    #[test]
    fn model_health_quota_evidence_requires_exhaustion_and_correct_scope() {
        let body = r#"{"error":{"message":"temporarily unavailable"}}"#;
        let mut payload = subscription_http_response_payload(429, "application/json", body.into());
        payload.insert(
            "quota_windows".into(),
            serde_json::json!([
                {"window":"weekly", "remaining_ratio":0.8},
                {"model":"model-a", "remaining_ratio":0.0}
            ]),
        );
        super::insert_subscription_route_model_failure_evidence(
            &mut payload,
            429,
            body,
            Some("model-a"),
        );
        assert_eq!(payload["failure_scope"], "model");
        assert_eq!(payload["provider_error_reason"], "MODEL_QUOTA_LIMIT");
        let mut available =
            subscription_http_response_payload(429, "application/json", body.into());
        available.insert("quota_windows".into(), serde_json::json!([
            {"window":"weekly", "remaining_ratio":0.8}, {"model":"model-a", "remaining_ratio":null}
        ]));
        super::insert_subscription_route_model_failure_evidence(
            &mut available,
            429,
            body,
            Some("model-a"),
        );
        assert_eq!(available["model_failure_soft"], true);
        assert_eq!(available["failure_scope"], "request", "old servers must not hard-ban weak evidence");
    }

    #[test]
    fn model_health_partial_quota_summary_does_not_close_channel() {
        let windows: Vec<super::QuotaWindow> = serde_json::from_value(serde_json::json!([
            {"source":"official_header", "window":"weekly", "remaining_ratio":0.8, "used_percent":20, "reset_at_unix":0, "checked_at_unix":1},
            {"source":"official_header", "window":"weekly_model", "model":"model-a", "remaining_ratio":0, "used_percent":100, "reset_at_unix":0, "checked_at_unix":1}
        ])).unwrap();
        let summary = super::channel_quota_summary(&windows).unwrap();
        assert_eq!(summary.status, "available");
        assert_eq!(summary.remaining_ratio, 0.8);
        assert!(super::channel_quota_summary(&windows[1..]).is_none());
        assert!(super::scoped_quota_summary(&windows[1..]).is_none());
    }

    #[test]
    fn model_health_risk_errors_are_not_reclassified_as_soft_model_failures() {
        let body = r#"{"error":{"message":"captcha verification required"}}"#;
        let mut payload = subscription_http_response_payload(403, "application/json", body.into());
        super::insert_subscription_route_model_failure_evidence(
            &mut payload,
            403,
            body,
            Some("model-a"),
        );
        assert_eq!(payload["error_kind"], "risk_challenge");
        assert_ne!(
            payload
                .get("failure_scope")
                .and_then(serde_json::Value::as_str),
            Some("model")
        );
    }

    #[test]
    fn model_health_success_generation_and_explicit_retry_after_are_preserved() {
        let mut state = LocalModelQuotaRouteState::default();
        state.record_runtime_failure(1000, 1800, false);
        assert!(!state.available_at(1301));
        let old_sequence = state.runtime_failure_sequence;
        state.record_runtime_failure(1001, 60, false);
        assert!(!state.record_success_observed(old_sequence));
        assert!(state.record_success_observed(state.runtime_failure_sequence));
        assert_eq!(state, LocalModelQuotaRouteState::default());
    }

    #[test]
    fn unsupported_location_requires_actual_model_and_does_not_close_all_groups() {
        let mut payload = subscription_http_response_payload(
            400,
            "application/json",
            r#"{"error":{"message":"User location is not supported for the API use."}}"#
                .to_string(),
        );

        assert_eq!(payload["error_kind"], "location_unsupported");
        assert_eq!(payload["failure_scope"], "request");
        assert_eq!(payload["safe_to_retry_other_channel"], true);
        assert_eq!(payload["safe_to_retry_same_channel"], false);
        assert_eq!(payload["retry_after_seconds"], 30 * 60);
        super::insert_subscription_route_model_failure_evidence(&mut payload, 400,
            r#"{"error":{"message":"User location is not supported for the API use."}}"#, Some("gemini-flash"));
        assert_eq!(payload["failure_scope"], "model");
        assert_eq!(payload["failure_model"], "gemini-flash");
    }

    #[test]
    fn payment_required_is_channel_quota_and_retryable_elsewhere() {
        let payload = subscription_http_response_payload(
            402,
            "application/json",
            r#"{"error":{"type":"insufficient_balance","message":"balance is exhausted"}}"#
                .to_string(),
        );

        assert_eq!(payload["error_kind"], "quota_exhausted");
        assert_eq!(payload["failure_scope"], "channel");
        assert_eq!(payload["safe_to_retry_other_channel"], true);
        assert_eq!(payload["safe_to_retry_same_channel"], false);
        assert_eq!(payload["retry_after_seconds"], 30 * 60);
    }

    #[test]
    fn voice_entitlement_denial_is_request_scoped() {
        let payload = subscription_http_response_payload(
            403,
            "application/json",
            r#"{"error":{"message":"Voice session access denied.","code":"forbidden"}}"#
                .to_string(),
        );

        assert_eq!(payload["error_kind"], "permission_denied");
        assert_eq!(payload["failure_scope"], "request");
        assert_eq!(payload["provider_error_reason"], "REALTIME_ENTITLEMENT_DENIED");
        assert_eq!(payload["safe_to_retry_other_channel"], true);
        assert_eq!(payload["safe_to_retry_same_channel"], false);
    }

    #[test]
    fn legal_rejection_is_not_evidence_of_account_location() {
        let payload = subscription_http_response_payload(
            451,
            "application/json",
            r#"{"error":{"message":"Unavailable for legal reasons"}}"#.to_string(),
        );

        assert_eq!(payload["error_kind"], "permission_denied");
        assert_eq!(payload["failure_scope"], "request");
        assert_eq!(payload["safe_to_retry_other_channel"], false);
    }

    #[test]
    fn cyber_policy_is_request_scoped_and_never_retryable() {
        let payload = subscription_http_response_payload_with_headers(
            502,
            "application/json",
            r#"{"error":{"type":"invalid_request","code":"cyber_policy","message":"blocked"}}"#.to_string(),
            &[("x-request-id", "provider-request-1")],
        );
        assert_eq!(payload["error_kind"], "cyber_policy");
        assert_eq!(payload["failure_scope"], "request");
        assert_eq!(payload["safe_to_retry_other_channel"], false);
        assert_eq!(payload["safe_to_retry_same_channel"], false);
        assert_eq!(payload["_const_policy_signal"]["code"], "cyber_policy");
        assert_eq!(
            payload["_const_policy_signal"]["provider_request_id"],
            "provider-request-1"
        );
    }

    #[test]
    fn chinese_risk_messages_are_classified_without_mojibake() {
        assert_eq!(
            classify_subscription_failure(403, "请求触发风控").0,
            "risk_challenge"
        );
        assert_eq!(
            classify_subscription_failure(403, "账号异常，请稍后再试").0,
            "risk_challenge"
        );
    }

    #[test]
    fn cyber_policy_code_outranks_transport_status() {
        for status in [400, 502] {
            assert_eq!(
                classify_subscription_failure(
                    status,
                    r#"{"error":{"type":"invalid_request","code":"cyber_policy","message":"blocked"}}"#,
                )
                .0,
                "cyber_policy"
            );
        }
    }

    #[test]
    fn claude_overage_window_is_auxiliary_not_an_invented_model() {
        let windows = parse_claude_quota_windows(&[
            (
                "anthropic-ratelimit-unified-7d_oi-utilization",
                "0.75",
            ),
            ("anthropic-ratelimit-unified-7d_oi-reset", "1893456000"),
        ]);

        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].window, "weekly_overage_included");
        assert_eq!(windows[0].model, None);
        assert!(super::channel_quota_summary(&windows).is_none());
        assert!((windows[0].remaining_ratio - 0.25).abs() < 0.001);
    }

    #[test]
    fn expired_model_failure_allows_only_one_half_open_probe() {
        let now = 1_000;
        let mut state = LocalModelQuotaRouteState::default();
        state.record_runtime_failure(now, 60, false);
        assert!(!state.available_at(now + 59));

        assert!(state.claim_runtime_probe(now + 60));
        assert!(!state.available_at(now + 60));
        assert!(!state.claim_runtime_probe(now + 60));

        let mut abandoned = state;
        assert!(abandoned.claim_runtime_probe(
            now + 60 + super::LOCAL_MODEL_RUNTIME_PROBE_LEASE_SECONDS
        ));

        state.release_runtime_probe();
        assert!(state.claim_runtime_probe(now + 60));
        state.record_success();
        assert!(state.available_at(now + 60));
        assert_eq!(state.runtime_failure_count, 0);
    }
}

pub(crate) fn subscription_error_response(
    status: u16,
    error_type: &str,
    message: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let body = serde_json::json!({
        "error": {
            "message": message,
            "type": error_type
        }
    });
    http_response_payload(status, "application/json", body.to_string())
}

pub(crate) fn subscription_structured_error_response(
    status: u16,
    error_kind: &str,
    message: &str,
    retry_after_seconds: i64,
    cooldown_until_unix: i64,
) -> serde_json::Map<String, serde_json::Value> {
    let mut payload = subscription_error_response(status, error_kind, message);
    payload.insert(
        "error_kind".to_string(),
        serde_json::Value::String(error_kind.to_string()),
    );
    if retry_after_seconds > 0 {
        payload.insert(
            "retry_after_seconds".to_string(),
            serde_json::json!(retry_after_seconds),
        );
    }
    if cooldown_until_unix > 0 {
        payload.insert(
            "cooldown_until_unix".to_string(),
            serde_json::json!(cooldown_until_unix),
        );
    }
    payload.insert(
        "safe_to_retry_other_channel".to_string(),
        serde_json::Value::Bool(true),
    );
    payload.insert(
        "safe_to_retry_same_channel".to_string(),
        serde_json::Value::Bool(false),
    );
    payload.insert(
        crate::surface_wire::LOGICAL_REQUEST_STARTED_FIELD.to_string(),
        serde_json::Value::Bool(false),
    );
    payload
}
