pub(crate) async fn test_channel_upstream_inner(
    channel: ChannelConfig,
    prompt: String,
) -> Result<ChannelUpstreamTestResult, String> {
    let channel = normalize_probe_channel(channel);
    let primary_model = pick_primary_model(&channel.models, &channel);
    match test_channel_upstream_with_model_inner(channel.clone(), None, prompt.clone()).await {
        Ok(result) => Ok(result),
        Err(primary_error)
            if channel_is_claude_subscription(&channel)
                && claude_probe_requires_newer_client(&primary_error) =>
        {
            let Some(fallback_model) = fallback_probe_model(&channel, &primary_model) else {
                return Err(primary_error);
            };
            log::warn!(
                "[const-api][supplier] Claude automatic channel probe model requires a newer client profile; retrying once with a conservative model failed_model={} fallback_model={}",
                primary_model,
                fallback_model
            );
            test_channel_upstream_with_model_inner(
                channel,
                Some(fallback_model.clone()),
                prompt,
            )
            .await
            .map_err(|fallback_error| {
                format!(
                    "{primary_error}; fallback probe with model {fallback_model} also failed: {fallback_error}"
                )
            })
        }
        Err(error) => Err(error),
    }
}

fn channel_is_claude_subscription(channel: &ChannelConfig) -> bool {
    channel.source_driver() == crate::source_driver::SourceDriverId::ClaudeSubscription
        || (channel.kind.trim() == "subscription_adapter"
            && platform_or_default(&channel.subscription.platform) == "claude")
}

fn claude_probe_requires_newer_client(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.contains("claude code")
        && [
            "does not support this model",
            "minimum claude code version",
            "please update claude code",
            "run 'claude update'",
            "run \"claude update\"",
        ]
        .iter()
        .any(|marker| error.contains(marker))
}

fn fallback_probe_model(channel: &ChannelConfig, failed_model: &str) -> Option<String> {
    let candidates = channel
        .models
        .iter()
        .filter(|model| !model.trim().eq_ignore_ascii_case(failed_model.trim()))
        .cloned()
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return None;
    }
    let fallback = pick_primary_model(&candidates, channel);
    (!fallback.trim().is_empty()).then_some(fallback)
}

pub(crate) async fn test_channel_upstream_model_inner(
    channel: ChannelConfig,
    model: String,
    prompt: String,
) -> Result<ChannelUpstreamTestResult, String> {
    test_channel_upstream_with_model_inner(channel, Some(model), prompt).await
}

async fn test_channel_upstream_with_model_inner(
    channel: ChannelConfig,
    requested_model: Option<String>,
    prompt: String,
) -> Result<ChannelUpstreamTestResult, String> {
    test_channel_upstream_attempt(channel, requested_model, prompt).await.map_err(|error| error.to_string())
}

#[derive(Debug)]
pub(crate) struct ProbeError {
    pub(crate) failure: Option<crate::upstream_failure::Failure>,
    pub(crate) message: String,
    pub(crate) deferred: bool,
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.message) }
}
impl std::error::Error for ProbeError {}
impl From<String> for ProbeError {
    fn from(message: String) -> Self {
        let deferred = message.contains("probe attempt budget") || message.contains("limit reached locally") || message.contains("quota reserve");
        Self { failure: None, message, deferred }
    }
}
impl ProbeError {
    fn response(status: u16, raw: &str, headers: &[(&str, &str)], model: &str) -> Self {
        let failure = crate::upstream_failure::observe(status, raw, headers, Some(model));
        let message = failure.as_ref().map(|failure| {
            format!("upstream returned HTTP {status}: {} [cause={}, rule={}]", failure.message, failure.cause, failure.rule_id)
        }).unwrap_or_else(|| format!("upstream returned HTTP {status}"));
        Self { failure, message, deferred: false }
    }
}

pub(crate) fn channel_probe_quota_reserved(channel: &ChannelConfig, windows: &[QuotaWindow]) -> Result<bool> {
    if channel.kind.trim() != "subscription_adapter" && !crate::coding_plan::has_quota(channel.source_driver()) { return Ok(false); }
    let supplier = supplier_from_channel_unfiltered(channel);
    let mut state = subscription_safety_state_for_channel(&supplier)?;
    if !windows.is_empty() {
        apply_official_quota_windows_to_state(&mut state, windows);
        save_subscription_safety_state(state.clone())?;
    }
    Ok(quota_reserve_decision_for_state(&state, channel.quota_reserve_percent(), now_unix())
        .is_some_and(|decision| decision.blocked))
}

fn check_probe_completion(raw: &str) -> Result<(), ProbeError> {
    let shape = response_shape_summary(raw);
    if matches!(shape.finish_reason.as_str(), "length" | "max_tokens" | "incomplete" | "MAX_TOKENS")
        || !extract_any_content(raw).is_some_and(|text| !text.trim().is_empty()) && shape.tool_call_count == 0 {
        return Err(ProbeError { failure: None, message: "inconclusive: upstream returned no complete usable output".into(), deferred: false });
    }
    Ok(())
}

async fn test_channel_upstream_attempt(
    channel: ChannelConfig,
    requested_model: Option<String>,
    prompt: String,
) -> Result<ChannelUpstreamTestResult, ProbeError> {
    let channel = normalize_probe_channel(channel);
    let requested_model = requested_model
        .map(|model| exact_probe_model(&channel, &model))
        .transpose()?;
    if channel.kind.trim() == "subscription_adapter" {
        return test_subscription_channel_upstream(&channel, requested_model.as_deref(), &prompt)
            .await
            .map_err(|err| match err.downcast::<ProbeError>() { Ok(error) => error, Err(error) => error.to_string().into() });
    }
    let model = requested_model
        .unwrap_or_else(|| pick_primary_model(&channel.models, &channel));
    if model.trim().is_empty() {
        return Err("upstream model is required".to_string().into());
    }
    if channel.upstream_base_url.trim().is_empty() {
        return Err("upstream base url is required".to_string().into());
    }

    let client = long_http_client().clone();
    let started_at = std::time::Instant::now();
    let (url, body, auth_header) =
        test_request_for_channel(&channel, &model, &prompt).map_err(|err| err.to_string())?;
    let mut req = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body);
    req = crate::channel_user_agent::apply_to_request(&channel, req);
    if !channel.upstream_api_key.trim().is_empty() {
        req = req
            .header(
                "Authorization",
                format!("Bearer {}", channel.upstream_api_key.trim()),
            )
            .header("X-API-Key", channel.upstream_api_key.trim());
        if is_azure_openai_channel(&channel) {
            req = req.header("api-key", channel.upstream_api_key.trim());
        }
        if auth_header == "anthropic" {
            req = req
                .header("x-api-key", channel.upstream_api_key.trim())
                .header("anthropic-version", "2023-06-01");
        }
        if auth_header == "gemini" {
            req = req.header("x-goog-api-key", channel.upstream_api_key.trim());
        }
    }
    let resp = crate::upstream_transport::send(req)
        .await
        .map_err(|err| {
            if matches!(err, crate::upstream_transport::UpstreamTransportError::ProbeBudgetExceeded) {
                return ProbeError { failure: None, message: err.to_string(), deferred: true };
            }
            let cause = if err.is_timeout() { "timeout" } else { "network_error" };
            ProbeError { failure: Some(crate::upstream_failure::Failure { version: 1, cause: cause.into(), scope: "request".into(), rule_id: "probe.transport.v1".into(), ..Default::default() }), message: format!("request upstream failed: {err}"), deferred: err.is_ambiguous() }
        })?;
    let status = resp.status().as_u16();
    let upstream_http_version =
        crate::upstream_transport::http_version_transport(resp.version()).to_string();
    let headers = resp.headers().iter().filter_map(|(name, value)| value.to_str().ok().map(|value| (name.as_str().to_string(), value.to_string()))).collect::<Vec<_>>();
    let raw = resp.text().await.map_err(|err| err.to_string())?;
    let header_refs = headers.iter().map(|(name, value)| (name.as_str(), value.as_str())).collect::<Vec<_>>();
    if !(200..300).contains(&status) || crate::upstream_failure::observe(status, &raw, &header_refs, Some(&model)).is_some() {
        return Err(ProbeError::response(status, &raw, &header_refs, &model));
    }
    check_probe_completion(&raw)?;
    let content = protocol_safe_response_content(&raw);
    let (input_tokens, output_tokens) = extract_usage_tokens(&raw);
    Ok(ChannelUpstreamTestResult {
        http_status: status,
        latency_ms: started_at.elapsed().as_millis(),
        upstream_http_version,
        model,
        content,
        raw,
        input_tokens,
        output_tokens,
    })
}

pub(crate) async fn test_subscription_channel_upstream(
    channel: &ChannelConfig,
    requested_model: Option<&str>,
    prompt: &str,
) -> Result<ChannelUpstreamTestResult> {
    let platform = channel.subscription.platform.trim();
    let started_at = std::time::Instant::now();
    let client = long_http_client().clone();
    if requested_model.is_none() && channel.models.is_empty() {
        return Err(anyhow!(
            "subscription model catalog is empty; refresh the channel before testing"
        ));
    }
    let models = channel.models.clone();
    let model = requested_model
        .map(str::to_string)
        .unwrap_or_else(|| pick_primary_model(&models, channel));
    let prompt = normalized_health_prompt(prompt);
    let supplier = supplier_from_channel_unfiltered(channel);
    let (path, body) = subscription_probe_request(platform, &model, &prompt)?;
    let (payload, upstream_http_version) = crate::upstream_transport::observe_http_version(
        forward_subscription_supplier_request(&client, &supplier, &path, &body, false, None),
    )
    .await;
    let wire = crate::surface_wire::SurfaceEnvelope::from_payload(&payload)
        .context("subscription probe returned an invalid surface response")?;
    let status = wire.status.unwrap_or_default();
    let headers = wire.header_map().unwrap_or_default();
    let header_refs = headers.iter().filter_map(|(name, value)| value.to_str().ok()
        .map(|value| (name.as_str(), value))).collect::<Vec<_>>();
    let raw = wire
        .body
        .utf8()
        .context("subscription probe returned a non-text response")?
        .to_string();
    if !(200..300).contains(&status) {
        let mut error = ProbeError::response(status, &raw, &header_refs, &model);
        if payload.get("upstream_attempted").and_then(serde_json::Value::as_bool) == Some(false) || payload.contains_key("failure_stage") {
            error.deferred = true; error.failure = None;
        }
        error.message = subscription_probe_failure_message(&payload, &wire, status, &raw, &model);
        return Err(error.into());
    }
    if crate::upstream_failure::observe(status, &raw, &header_refs, Some(&model)).is_some() {
        return Err(ProbeError::response(status, &raw, &header_refs, &model).into());
    }
    if let Some(payload) = subscription_sse_terminal_error_payload(&raw) {
        if let Ok(wire) = crate::surface_wire::SurfaceEnvelope::from_payload(&payload) {
            return Err(ProbeError::response(wire.status.unwrap_or(502), wire.body.utf8().unwrap_or(""), &header_refs, &model).into());
        }
    }
    check_probe_completion(&raw)?;
    let content = protocol_safe_response_content(&raw);
    let (input_tokens, output_tokens) = extract_usage_tokens(&raw);
    Ok(ChannelUpstreamTestResult {
        http_status: status,
        latency_ms: started_at.elapsed().as_millis(),
        upstream_http_version: upstream_http_version
            .map(crate::upstream_transport::http_version_transport)
            .unwrap_or_default()
            .to_string(),
        model,
        content,
        raw,
        input_tokens,
        output_tokens,
    })
}

pub(crate) fn normalized_health_prompt(prompt: &str) -> String {
    let trimmed = prompt.trim();
    if !trimmed.is_empty() {
        return trimmed.to_string();
    }
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.subsec_nanos() % 100_000)
        .unwrap_or(0);
    format!("Reply with one short sentence that includes check-{suffix}.")
}

pub(crate) fn subscription_probe_request(
    platform: &str,
    model: &str,
    prompt: &str,
) -> Result<(String, String)> {
    match platform {
        "openai" | "codex" => Ok((
            "/v1/responses".to_string(),
            serde_json::json!({
                "model": model,
                "input": [{
                    "role": "user",
                    "content": [{ "type": "input_text", "text": prompt }]
                }],
                "store": false,
                "stream": false
            })
            .to_string(),
        )),
        "grok" => Ok((
            "/v1/responses".to_string(),
            serde_json::json!({
                "model": model,
                "input": [{
                    "role": "user",
                    "content": [{ "type": "input_text", "text": prompt }]
                }],
                "stream": false
            })
            .to_string(),
        )),
        "antigravity" => Ok((
            "/v1/chat/completions".to_string(),
            serde_json::json!({
                "model": model,
                "messages": [{ "role": "user", "content": prompt }],
                "stream": false
            })
            .to_string(),
        )),
        "claude" => Ok((
            "/v1/messages".to_string(),
            serde_json::json!({
                "model": model,
                "max_tokens": 64,
                "messages": [{ "role": "user", "content": prompt }]
            })
            .to_string(),
        )),
        _ => Err(anyhow!("unsupported subscription provider: {platform}")),
    }
}

fn exact_probe_model(channel: &ChannelConfig, requested_model: &str) -> Result<String, String> {
    let requested_model = requested_model.trim();
    if requested_model.is_empty() {
        return Err("upstream model is required".to_string());
    }
    Ok(channel
        .models
        .iter()
        .find(|model| model.trim().eq_ignore_ascii_case(requested_model))
        .cloned()
        .unwrap_or_else(|| requested_model.to_string()))
}

#[cfg(test)]
mod channel_probe_tests {
    use super::*;

    #[test]
    fn completed_responses_can_begin_with_reasoning_but_truncation_is_inconclusive() {
        let complete = r#"{"object":"response","status":"completed","output":[{"type":"reasoning","summary":[]},{"type":"message","content":[{"type":"output_text","text":"OK"}]}]}"#;
        assert!(check_probe_completion(complete).is_ok());
        assert_eq!(extract_any_content(complete).as_deref(), Some("OK"));
        assert!(check_probe_completion(r#"{"choices":[{"finish_reason":"length","message":{"content":null,"reasoning":"unfinished"}}]}"#).is_err());
    }

    #[test]
    fn explicit_probe_uses_the_selected_model_instead_of_the_catalog_default() {
        let mut supplier = default_supplier_config();
        supplier.subscription.platform = "antigravity".to_string();
        supplier.models = vec!["claude-opus-4-6".to_string(), "gemini-2.5-pro".to_string()];
        supplier.upstream_model = "claude-opus-4-6".to_string();
        supplier.public_model = "claude-opus-4-6".to_string();
        let channel = normalize_probe_channel(channel_from_supplier(
            "antigravity-probe".to_string(),
            &supplier,
        ));

        assert_eq!(
            exact_probe_model(&channel, "gemini-2.5-pro").unwrap(),
            "gemini-2.5-pro"
        );
    }

    #[test]
    fn subscription_probe_uses_the_configured_default_model() {
        let mut supplier = default_supplier_config();
        supplier.subscription.platform = "claude".to_string();
        supplier.upstream_model = "claude-opus-5".to_string();
        supplier.public_model = "claude-opus-5".to_string();
        let mut channel = channel_from_supplier("claude-probe".to_string(), &supplier);
        channel.set_source_driver(crate::source_driver::SourceDriverId::ClaudeSubscription);
        let models = vec![
            "claude-opus-5".to_string(),
            "claude-sonnet-5".to_string(),
            "claude-sonnet-4-5-20250929".to_string(),
        ];

        assert_eq!(
            pick_primary_model(&models, &channel),
            "claude-opus-5"
        );
    }

    #[test]
    fn claude_probe_without_a_default_prefers_a_conservative_model() {
        let mut supplier = default_supplier_config();
        supplier.subscription.platform = "claude".to_string();
        supplier.upstream_model.clear();
        supplier.public_model.clear();
        let mut channel = channel_from_supplier("claude-probe".to_string(), &supplier);
        channel.set_source_driver(crate::source_driver::SourceDriverId::ClaudeSubscription);
        let models = vec![
            "claude-fable-5-1".to_string(),
            "claude-opus-5".to_string(),
            "claude-haiku-4-5-20251001".to_string(),
            "claude-sonnet-4-5-20250929".to_string(),
        ];

        assert_eq!(
            pick_primary_model(&models, &channel),
            "claude-haiku-4-5-20251001"
        );
    }

    #[test]
    fn antigravity_probe_without_a_default_prefers_flash_over_pro() {
        let mut supplier = default_supplier_config();
        supplier.subscription.platform = "antigravity".to_string();
        supplier.upstream_model.clear();
        supplier.public_model.clear();
        let mut channel = channel_from_supplier("antigravity-probe".to_string(), &supplier);
        channel.set_source_driver(crate::source_driver::SourceDriverId::GeminiSubscription);
        let models = vec![
            "gemini-3-pro".to_string(),
            "gemini-3-flash".to_string(),
            "claude-opus-4-6".to_string(),
        ];

        assert_eq!(
            pick_primary_model(&models, &channel),
            "gemini-3-flash"
        );
    }

    #[test]
    fn openai_api_probe_without_a_default_prefers_a_small_text_model() {
        let mut channel = channel_from_supplier("openai-api-probe".to_string(), &default_supplier_config());
        channel.set_source_driver(crate::source_driver::SourceDriverId::OpenAiApi);
        channel.upstream_model.clear();
        channel.public_model.clear();
        let models = vec![
            "gpt-image-2".to_string(),
            "gpt-5.6-terra".to_string(),
            "gpt-5.6-luna".to_string(),
        ];

        assert_eq!(pick_primary_model(&models, &channel), "gpt-5.6-luna");
    }

    #[test]
    fn groq_api_probe_without_a_default_prefers_an_instant_text_model() {
        let mut channel = channel_from_supplier("groq-api-probe".to_string(), &default_supplier_config());
        channel.set_source_driver(crate::source_driver::SourceDriverId::GroqApi);
        channel.upstream_model.clear();
        channel.public_model.clear();
        let models = vec![
            "whisper-large-v3".to_string(),
            "llama-3.3-70b-versatile".to_string(),
            "llama-3.1-8b-instant".to_string(),
        ];

        assert_eq!(
            pick_primary_model(&models, &channel),
            "llama-3.1-8b-instant"
        );
    }

    #[test]
    fn claude_version_rejection_detection_is_narrow() {
        assert!(claude_probe_requires_newer_client(
            "Claude Code 2.1.220 does not support this model; version 2.1.251 or newer is required. Run 'claude update', or update the Claude desktop app, then try again."
        ));
        assert!(!claude_probe_requires_newer_client(
            "subscription probe returned HTTP 400: thinking.enabled.budget_tokens must be an integer"
        ));
        assert!(!claude_probe_requires_newer_client(
            "subscription probe returned HTTP 429: Usage credits are required for this model."
        ));
    }

    #[test]
    fn claude_version_rejection_fallback_excludes_the_failed_default() {
        let mut supplier = default_supplier_config();
        supplier.subscription.platform = "claude".to_string();
        supplier.upstream_model = "claude-opus-5".to_string();
        supplier.public_model = "claude-opus-5".to_string();
        let mut channel = channel_from_supplier("claude-probe".to_string(), &supplier);
        channel.set_source_driver(crate::source_driver::SourceDriverId::ClaudeSubscription);
        channel.models = vec![
            "claude-opus-5".to_string(),
            "claude-sonnet-4-5-20250929".to_string(),
            "claude-haiku-4-5-20251001".to_string(),
        ];

        assert_eq!(
            fallback_probe_model(&channel, "claude-opus-5"),
            Some("claude-haiku-4-5-20251001".to_string())
        );
        assert_eq!(
            fallback_probe_model(
                &ChannelConfig {
                    models: vec!["claude-opus-5".to_string()],
                    ..channel
                },
                "claude-opus-5"
            ),
            None
        );
    }

    #[test]
    fn subscription_probe_error_retains_structured_rate_limit_evidence() {
        let raw = r#"{"type":"error","error":{"type":"rate_limit_error","message":"Error"},"request_id":"req_123"}"#;
        let mut payload = serde_json::Map::new();
        payload.insert("error_kind".to_string(), "rate_limited".into());
        payload.insert("retry_after_seconds".to_string(), 30.into());
        let wire = crate::surface_wire::SurfaceEnvelope::default();

        assert_eq!(
            subscription_probe_failure_message(
                &payload,
                &wire,
                429,
                raw,
                "claude-sonnet-4-5-20250929"
            ),
            "subscription probe returned HTTP 429 for model claude-sonnet-4-5-20250929: Error [type=rate_limit_error, kind=rate_limited, request_id=req_123, retry_after=30s]"
        );
    }
}

fn subscription_probe_failure_message(
    payload: &serde_json::Map<String, serde_json::Value>,
    wire: &crate::surface_wire::SurfaceEnvelope,
    status: u16,
    raw: &str,
    model: &str,
) -> String {
    let value = serde_json::from_str::<serde_json::Value>(raw).unwrap_or_default();
    let error = value.get("error").unwrap_or(&value);
    let content = error
        .get("message")
        .and_then(serde_json::Value::as_str)
        .or_else(|| value.get("message").and_then(serde_json::Value::as_str))
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .map(str::to_string)
        .or_else(|| extract_any_content(raw))
        .unwrap_or_else(|| "upstream request failed".to_string());
    let error_type = error
        .get("type")
        .and_then(serde_json::Value::as_str)
        .or_else(|| value.get("type").and_then(serde_json::Value::as_str));
    let error_kind = payload
        .get("error_kind")
        .and_then(serde_json::Value::as_str);
    let request_id = value
        .get("request_id")
        .and_then(serde_json::Value::as_str)
        .or_else(|| error.get("request_id").and_then(serde_json::Value::as_str))
        .or_else(|| wire.header_utf8("request-id").ok().flatten())
        .or_else(|| wire.header_utf8("x-request-id").ok().flatten());
    let retry_after = payload
        .get("retry_after_seconds")
        .and_then(serde_json::Value::as_i64)
        .or_else(|| {
            wire.header_utf8("retry-after")
                .ok()
                .flatten()
                .and_then(|value| value.trim().parse::<i64>().ok())
        });
    let mut details = Vec::new();
    if let Some(value) = error_type.filter(|value| !value.trim().is_empty()) {
        details.push(format!("type={value}"));
    }
    if let Some(value) = error_kind.filter(|value| !value.trim().is_empty()) {
        details.push(format!("kind={value}"));
    }
    if let Some(value) = request_id.filter(|value| !value.trim().is_empty()) {
        details.push(format!("request_id={value}"));
    }
    if let Some(value) = retry_after.filter(|value| *value > 0) {
        details.push(format!("retry_after={value}s"));
    }
    let suffix = if details.is_empty() {
        String::new()
    } else {
        format!(" [{}]", details.join(", "))
    };
    format!("subscription probe returned HTTP {status} for model {model}: {content}{suffix}")
}
