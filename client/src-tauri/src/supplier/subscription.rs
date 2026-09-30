const CLAUDE_RESPONSE_ACCEPT_ENCODING: &str = "gzip, deflate, br, zstd";

pub(crate) async fn forward_subscription_supplier_request(
    client: &Client,
    config: &SupplierConfig,
    path: &str,
    body: &str,
    stream_requested: bool,
    upstream_override: Option<&str>,
) -> serde_json::Map<String, serde_json::Value> {
    forward_subscription_supplier_request_with_headers(
        client,
        config,
        path,
        body,
        stream_requested,
        upstream_override,
        None,
    )
    .await
}

pub(crate) async fn forward_subscription_supplier_request_with_headers(
    client: &Client,
    config: &SupplierConfig,
    path: &str,
    body: &str,
    stream_requested: bool,
    upstream_override: Option<&str>,
    request_headers: Option<&reqwest::header::HeaderMap>,
) -> serde_json::Map<String, serde_json::Value> {
    let cache_identity = subscription_body_cache_identity(body);
    forward_subscription_supplier_request_with_capacity(
        client,
        config,
        path,
        body,
        stream_requested,
        upstream_override,
        None,
        cache_identity.as_deref(),
        request_headers,
    )
    .await
}

pub(crate) fn subscription_body_cache_identity(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let metadata = value
        .get("client_metadata")
        .and_then(serde_json::Value::as_object);
    let candidate = metadata
        .and_then(|metadata| metadata.get("session_id"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            metadata
                .and_then(|metadata| metadata.get("thread_id"))
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| value.get("session_id").and_then(serde_json::Value::as_str))
        .or_else(|| value.get("sessionId").and_then(serde_json::Value::as_str))
        .or_else(|| {
            value
                .get("conversation_id")
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| {
            value
                .get("prompt_cache_key")
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| {
            let conversation = value.get("conversation")?;
            conversation
                .as_str()
                .or_else(|| conversation.get("id").and_then(serde_json::Value::as_str))
        })
        .map(str::to_string)
        .or_else(|| claude_code_session_id_from_body(body));
    normalized_subscription_cache_identity(candidate.as_deref()).map(str::to_string)
}

pub(crate) async fn forward_subscription_supplier_request_with_capacity(
    client: &Client,
    config: &SupplierConfig,
    path: &str,
    body: &str,
    stream_requested: bool,
    upstream_override: Option<&str>,
    capacity: Option<SubscriptionRequestCapacity>,
    cache_identity: Option<&str>,
    request_headers: Option<&reqwest::header::HeaderMap>,
) -> serde_json::Map<String, serde_json::Value> {
    let started_at = std::time::Instant::now();
    let provider = subscription_provider_from_config(config);
    let force_buffered = subscription_operation_requires_buffered(&provider, path);
    let responses_subscription = matches!(provider.as_str(), "openai" | "codex" | "grok");
    let audit_recorded_by_executor =
        (!stream_requested || force_buffered) && responses_subscription;
    let _safety_guard = if responses_subscription {
        None
    } else {
        match subscription_safety_enter_with_capacity(
            config,
            body,
            capacity.unwrap_or_else(|| SubscriptionRequestCapacity::for_path(path)),
        ) {
            Ok(guard) => Some(guard),
            Err(payload) => {
                let _ = append_subscription_wire_audit(
                    config,
                    path,
                    body,
                    &payload,
                    started_at.elapsed(),
                );
                return payload;
            }
        }
    };
    let result = async {
        if stream_requested && !force_buffered {
            return Ok(subscription_wire_error_response(
                501,
                "subscription_stream_not_supported",
                "subscription executor does not support this streaming path yet; disable stream for now",
            ));
        }
        let channel = channel_from_supplier("subscription-runtime".to_string(), config);
        match provider.as_str() {
            "openai" | "codex" | "grok" => {
                forward_openai_subscription_buffered_request_with_capacity(
                    client,
                    config,
                    path,
                    body,
                    false,
                    upstream_override,
                    capacity,
                    cache_identity,
                    request_headers,
                )
                .await
            }
            "antigravity" => {
                forward_antigravity_subscription_request_with_cache_identity(
                    client,
                    &channel,
                    path,
                    body,
                    upstream_override,
                    cache_identity,
                )
                    .await
            }
            "claude" => {
                forward_claude_subscription_request_with_headers(
                    client,
                    &channel,
                    path,
                    body,
                    upstream_override,
                    cache_identity,
                    request_headers,
                )
                .await
            }
            _ => Ok(subscription_wire_error_response(
                400,
                "subscription_provider_unknown",
                &format!("unknown subscription provider: {provider}"),
            )),
        }
    }
    .await;

    let payload = result.unwrap_or_else(|err: anyhow::Error| {
        if let Some(error) = oauth_wire_error_from_anyhow(&err) {
            subscription_wire_error_response(error.status, error.error_type, &error.message)
        } else if crate::upstream_transport::is_ambiguous_transport_error(&err) {
            supplier_execution_error_payload(&err)
        } else {
            subscription_wire_error_response(502, "subscription_executor_error", &err.to_string())
        }
    });
    if !audit_recorded_by_executor {
        let _ = append_subscription_wire_audit(config, path, body, &payload, started_at.elapsed());
    }
    payload
}

#[derive(Debug, Default, Clone)]
pub(crate) struct SubscriptionStreamDebug {
    upstream_send_ms: u64,
    first_body_chunk_ms: Option<u64>,
    first_sse_event_ms: Option<u64>,
    first_meaningful_event_ms: Option<u64>,
    body_read_ms: u64,
    upstream_chunk_count: usize,
    upstream_response_bytes: usize,
    largest_upstream_chunk_bytes: usize,
}

impl SubscriptionStreamDebug {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "upstream_send_ms": self.upstream_send_ms,
            "first_body_chunk_ms": self.first_body_chunk_ms,
            "first_sse_event_ms": self.first_sse_event_ms,
            "first_meaningful_event_ms": self.first_meaningful_event_ms,
            "routing_latency_ms": self.routing_latency_ms(),
            "body_read_ms": self.body_read_ms,
            "upstream_chunk_count": self.upstream_chunk_count,
            "upstream_response_bytes": self.upstream_response_bytes,
            "largest_upstream_chunk_bytes": self.largest_upstream_chunk_bytes,
        })
    }

    fn routing_latency_ms(&self) -> Option<u64> {
        self.first_meaningful_event_ms
            .or(self.first_sse_event_ms)
            .or_else(|| {
                self.first_body_chunk_ms
                    .map(|first_chunk| self.upstream_send_ms.saturating_add(first_chunk))
            })
    }

    fn observe_body_chunk(
        &mut self,
        body_read_started: std::time::Instant,
        source_chunks: usize,
        largest_source_chunk: usize,
        response_bytes: usize,
    ) {
        self.first_body_chunk_ms
            .get_or_insert_with(|| elapsed_millis_u64(body_read_started));
        self.upstream_chunk_count = self.upstream_chunk_count.saturating_add(source_chunks);
        self.upstream_response_bytes = self
            .upstream_response_bytes
            .saturating_add(response_bytes);
        self.largest_upstream_chunk_bytes = self
            .largest_upstream_chunk_bytes
            .max(largest_source_chunk);
    }

    fn observe_sse_frames(
        &mut self,
        protocol: &str,
        request_started: std::time::Instant,
        frames: &[crate::protocol::stream::SseFrame],
    ) -> bool {
        let elapsed = elapsed_millis_u64(request_started);
        for frame in frames {
            self.first_sse_event_ms.get_or_insert(elapsed);
            if self.first_meaningful_event_ms.is_none()
                && subscription_sse_event_is_meaningful(protocol, &frame.data)
            {
                self.first_meaningful_event_ms = Some(elapsed);
                return true;
            }
        }
        false
    }
}

pub(crate) fn responses_sse_event_is_meaningful(data: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
        return false;
    };
    value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|kind| {
            !matches!(
                kind,
                "response.created" | "response.queued" | "response.in_progress"
            )
        })
}

fn subscription_sse_event_is_meaningful(protocol: &str, data: &str) -> bool {
    match protocol.trim().to_ascii_lowercase().as_str() {
        "openai_responses" => responses_sse_event_is_meaningful(data),
        "openai_chat" => openai_chat_sse_event_is_meaningful(data),
        "anthropic_messages" => anthropic_sse_event_is_meaningful(data),
        "gemini_native" => gemini_sse_event_is_meaningful(data),
        _ => !data.trim().is_empty() && data.trim() != "[DONE]",
    }
}

fn openai_chat_sse_event_is_meaningful(data: &str) -> bool {
    if data.trim() == "[DONE]" {
        return false;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
        return false;
    };
    if value.get("error").is_some() {
        return true;
    }
    value
        .get("choices")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|choices| {
            choices.iter().any(|choice| {
                choice.get("finish_reason").is_some_and(|value| !value.is_null())
                    || choice
                        .get("delta")
                        .or_else(|| choice.get("message"))
                        .is_some_and(openai_chat_delta_is_meaningful)
            })
        })
}

fn openai_chat_delta_is_meaningful(delta: &serde_json::Value) -> bool {
    ["content", "reasoning_content", "reasoning"]
        .into_iter()
        .any(|field| {
            delta
                .get(field)
                .and_then(serde_json::Value::as_str)
                .is_some_and(|value| !value.is_empty())
        })
        || ["tool_calls", "function_call"]
            .into_iter()
            .any(|field| delta.get(field).is_some_and(json_value_is_non_empty))
}

fn anthropic_sse_event_is_meaningful(data: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
        return false;
    };
    match value.get("type").and_then(serde_json::Value::as_str) {
        Some("error" | "message_stop") => true,
        Some("message_delta") => value
            .pointer("/delta/stop_reason")
            .is_some_and(|value| !value.is_null()),
        Some("content_block_start") => value
            .get("content_block")
            .is_some_and(anthropic_content_block_is_meaningful),
        Some("content_block_delta") => value
            .get("delta")
            .is_some_and(anthropic_delta_is_meaningful),
        _ => false,
    }
}

fn anthropic_content_block_is_meaningful(block: &serde_json::Value) -> bool {
    match block.get("type").and_then(serde_json::Value::as_str) {
        Some("text") => block
            .get("text")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| !value.is_empty()),
        Some("thinking") => block
            .get("thinking")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| !value.is_empty()),
        Some("tool_use" | "server_tool_use" | "mcp_tool_use") => true,
        Some(_) => true,
        None => false,
    }
}

fn anthropic_delta_is_meaningful(delta: &serde_json::Value) -> bool {
    ["text", "thinking", "partial_json"]
        .into_iter()
        .any(|field| {
            delta
                .get(field)
                .and_then(serde_json::Value::as_str)
                .is_some_and(|value| !value.is_empty())
        })
        || delta
            .get("citation")
            .is_some_and(json_value_is_non_empty)
}

fn gemini_sse_event_is_meaningful(data: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
        return false;
    };
    let response = value.get("response").unwrap_or(&value);
    if response.get("error").is_some()
        || response
            .get("promptFeedback")
            .is_some_and(json_value_is_non_empty)
    {
        return true;
    }
    response
        .get("candidates")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|candidates| {
            candidates.iter().any(|candidate| {
                candidate
                    .get("finishReason")
                    .is_some_and(|value| !value.is_null())
                    || candidate
                        .pointer("/content/parts")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|parts| parts.iter().any(gemini_part_is_meaningful))
            })
        })
}

fn gemini_part_is_meaningful(part: &serde_json::Value) -> bool {
    ["text", "thought"]
        .into_iter()
        .any(|field| {
            part.get(field)
                .and_then(serde_json::Value::as_str)
                .is_some_and(|value| !value.is_empty())
        })
        || [
            "functionCall",
            "functionResponse",
            "inlineData",
            "fileData",
            "executableCode",
            "codeExecutionResult",
        ]
        .into_iter()
        .any(|field| part.get(field).is_some_and(json_value_is_non_empty))
}

fn json_value_is_non_empty(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(_) | serde_json::Value::Number(_) => true,
        serde_json::Value::String(value) => !value.is_empty(),
        serde_json::Value::Array(values) => !values.is_empty(),
        serde_json::Value::Object(values) => !values.is_empty(),
    }
}

fn elapsed_millis_u64(started_at: std::time::Instant) -> u64 {
    started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

fn subscription_wire_response_payload(
    status: u16,
    content_type: &str,
    body: String,
) -> serde_json::Map<String, serde_json::Value> {
    subscription_wire_response_payload_with_owned_headers(status, content_type, body, &[], true)
}

fn subscription_wire_response_payload_with_header_map(
    status: u16,
    content_type: &str,
    body: String,
    headers: &reqwest::header::HeaderMap,
    body_rebuilt: bool,
) -> serde_json::Map<String, serde_json::Value> {
    let ordered = crate::surface_wire::header_pairs(headers)
        .into_iter()
        .map(|header| (header.name, header.value))
        .collect::<Vec<_>>();
    let quota_headers = response_header_pairs(headers);
    subscription_wire_response_payload_from_parts(
        status,
        content_type,
        body,
        ordered,
        &quota_headers,
        body_rebuilt,
    )
}

fn subscription_wire_response_payload_with_owned_headers(
    status: u16,
    content_type: &str,
    body: String,
    headers: &[(String, String)],
    body_rebuilt: bool,
) -> serde_json::Map<String, serde_json::Value> {
    let ordered = headers
        .iter()
        .map(|(name, value)| {
            (
                name.clone(),
                crate::surface_wire::WireBody::from_bytes(value.as_bytes()),
            )
        })
        .collect::<Vec<_>>();
    subscription_wire_response_payload_from_parts(
        status,
        content_type,
        body,
        ordered,
        headers,
        body_rebuilt,
    )
}

fn subscription_wire_response_payload_from_parts(
    status: u16,
    content_type: &str,
    body: String,
    headers: Vec<(String, crate::surface_wire::WireBody)>,
    quota_headers: &[(String, String)],
    body_rebuilt: bool,
) -> serde_json::Map<String, serde_json::Value> {
    let mut wire_headers = headers
        .into_iter()
        .filter(|(name, _)| {
            !body_rebuilt
                || (!name.eq_ignore_ascii_case("content-length")
                    && !name.eq_ignore_ascii_case("content-encoding")
                    && !name.eq_ignore_ascii_case("content-type"))
        })
        .map(|(name, value)| crate::surface_wire::WireHeader { name, value })
        .collect::<Vec<_>>();
    if !content_type.is_empty()
        && !wire_headers
            .iter()
            .any(|header| header.name.eq_ignore_ascii_case("content-type"))
    {
        wire_headers.push(crate::surface_wire::WireHeader {
            name: "content-type".to_string(),
            value: crate::surface_wire::WireBody::from_bytes(content_type.as_bytes()),
        });
    }
    let envelope = crate::surface_wire::SurfaceEnvelope {
        status: Some(status),
        headers: wire_headers,
        body: crate::surface_wire::WireBody::from_bytes(body.as_bytes()),
        ..crate::surface_wire::SurfaceEnvelope::default()
    };
    let mut payload = crate::surface_wire::wire_payload(envelope).unwrap_or_else(|error| {
        surface_wire_serialization_error_payload(&error)
    });
    insert_subscription_wire_metadata(&mut payload, status, &body, quota_headers);
    payload
}

fn insert_subscription_wire_metadata(
    payload: &mut serde_json::Map<String, serde_json::Value>,
    status: u16,
    body: &str,
    headers: &[(String, String)],
) {
    let borrowed = headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect::<Vec<_>>();
    let mut windows = parse_codex_quota_windows(&borrowed);
    windows.extend(parse_claude_quota_windows(&borrowed));
    windows.extend(parse_grok_quota_windows(&borrowed));
    if !windows.is_empty() {
        insert_official_quota_windows(payload, &windows);
    }
    let (error_kind, body_retry_after) = classify_subscription_failure(status, body);
    if !payload.contains_key("quota_source") {
        if let Some(signal) = parse_subscription_error_quota_signal(body) {
            insert_official_quota_windows(payload, &[signal.to_quota_window()]);
        }
    }
    insert_subscription_policy_metadata(payload, &error_kind, &borrowed);
    if (200..300).contains(&status) {
        return;
    }
    insert_subscription_model_failure_evidence(payload, status, body);
    let retry_after = parse_subscription_retry_after(&borrowed).unwrap_or(body_retry_after);
    if !error_kind.is_empty() && error_kind != "cyber_policy" {
        payload.insert("error_kind".to_string(), error_kind.into());
        payload.insert("safe_to_retry_other_channel".to_string(), true.into());
        payload.insert("safe_to_retry_same_channel".to_string(), false.into());
    }
    if retry_after > 0 {
        payload.insert("retry_after_seconds".to_string(), retry_after.into());
    }
}

fn subscription_wire_error_response(
    status: u16,
    error_type: &str,
    message: &str,
) -> serde_json::Map<String, serde_json::Value> {
    subscription_wire_response_payload(
        status,
        "application/json",
        serde_json::json!({
            "error": {
                "message": message,
                "type": error_type,
            }
        })
        .to_string(),
    )
}

fn subscription_terminal_wire_error_payload(
    raw: &str,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let mut last_event = String::new();
    for line in raw.lines() {
        if let Some(event) = line.strip_prefix("event:") {
            last_event = event.trim().to_ascii_lowercase();
            continue;
        }
        if !last_event.contains("error") {
            continue;
        }
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let body = data.trim();
        if body.is_empty() || body == "[DONE]" {
            continue;
        }
        let lower = body.to_ascii_lowercase();
        let status = if lower.contains("usage_limit")
            || lower.contains("resource_exhausted")
            || lower.contains("rate")
            || lower.contains("quota")
        {
            429
        } else if lower.contains("auth") || lower.contains("unauthorized") {
            401
        } else if lower.contains("captcha") || lower.contains("verification") {
            403
        } else {
            500
        };
        return Some(subscription_wire_response_payload(
            status,
            "text/event-stream",
            body.to_string(),
        ));
    }
    None
}

fn subscription_wire_body_text(payload: &serde_json::Map<String, serde_json::Value>) -> String {
    crate::surface_wire::SurfaceEnvelope::from_payload(payload)
        .ok()
        .and_then(|wire| wire.body.utf8().ok().map(str::to_string))
        .unwrap_or_default()
}

pub(crate) fn subscription_response_headers(
    headers: &reqwest::header::HeaderMap,
) -> reqwest::header::HeaderMap {
    let mut output = headers.clone();
    crate::channel_executor::strip_http_response_headers(&mut output);
    output.remove(reqwest::header::CONTENT_LENGTH);
    output.remove(reqwest::header::CONTENT_ENCODING);
    // Retained subscription cookies belong to the supplier's provider session.
    // The gateway does not maintain a caller cookie jar, so forwarding them
    // cannot improve continuity and would expose account-scoped material.
    output.remove(reqwest::header::SET_COOKIE);
    output
}

pub(crate) fn subscription_stream_headers(
    headers: &reqwest::header::HeaderMap,
) -> reqwest::header::HeaderMap {
    let mut output = subscription_response_headers(headers);
    output.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("text/event-stream"),
    );
    output
}

async fn send_subscription_stream_chunk(
    outbound: &SupplierOutbound,
    msg_id: &str,
    status: u16,
    headers: &reqwest::header::HeaderMap,
    started: &mut bool,
    faults: &[ImprovementFault],
    chunk: &[u8],
) -> Result<()> {
    if chunk.is_empty() {
        return Ok(());
    }
    if !*started {
        let mut payload = crate::surface_wire::stream_start_payload(status, headers)?;
        attach_improvement_faults(&mut payload, faults);
        send_supplier_message(
            outbound,
            SupplierMessage {
                id: msg_id.to_string(),
                kind: "stream_start".to_string(),
                payload,
            },
        )
        .await?;
        *started = true;
    }
    send_supplier_message(
        outbound,
        SupplierMessage {
            id: msg_id.to_string(),
            kind: "stream_chunk".to_string(),
            payload: crate::surface_wire::stream_chunk_payload(chunk)?,
        },
    )
    .await
}

async fn finish_subscription_stream(
    outbound: &SupplierOutbound,
    msg_id: &str,
    status: u16,
    headers: &reqwest::header::HeaderMap,
    started: &mut bool,
    faults: &[ImprovementFault],
    upstream_usage: Option<&SupplierUpstreamUsage>,
) -> Result<()> {
    if !*started {
        let mut payload = crate::surface_wire::stream_start_payload(status, headers)?;
        attach_improvement_faults(&mut payload, faults);
        send_supplier_message(
            outbound,
            SupplierMessage {
                id: msg_id.to_string(),
                kind: "stream_start".to_string(),
                payload,
            },
        )
        .await?;
        *started = true;
    }
    let mut payload = subscription_stream_end_payload()?;
    attach_upstream_usage(&mut payload, upstream_usage);
    send_supplier_message(
        outbound,
        SupplierMessage {
            id: msg_id.to_string(),
            kind: "stream_end".to_string(),
            payload,
        },
    )
    .await
}

fn subscription_stream_end_payload() -> Result<serde_json::Map<String, serde_json::Value>> {
    crate::surface_wire::wire_payload(crate::surface_wire::SurfaceEnvelope {
        stream: Some(true),
        ..crate::surface_wire::SurfaceEnvelope::default()
    })
}

fn subscription_stream_execution_error(
    converter: &InboundSseStreamConverter,
    timed_out: bool,
    response_started: bool,
    usable_output_produced: bool,
) -> Option<anyhow::Error> {
    if timed_out {
        return Some(upstream_stream_failure(
            response_started,
            usable_output_produced,
            "upstream stream inactivity timeout",
        ));
    }
    converter.failure().map(|failure| {
        response_conversion_failure(response_started, usable_output_produced, failure)
    })
}

pub(crate) fn append_subscription_wire_audit(
    config: &SupplierConfig,
    path: &str,
    request_body: &str,
    response_payload: &serde_json::Map<String, serde_json::Value>,
    elapsed: Duration,
) -> Result<()> {
    append_subscription_audit(config, path, request_body, response_payload, elapsed)
}

pub(crate) fn subscription_inbound_protocol(path: &str) -> Option<String> {
    let canonical_path = path.split_once('?').map_or(path, |(path, _)| path);
    let protocol = protocol_from_path(canonical_path);
    (!protocol.is_empty()).then_some(protocol)
}

pub(crate) fn is_openai_responses_compact_path(path: &str) -> bool {
    path.split('?')
        .next()
        .is_some_and(|path| path.ends_with("/responses/compact"))
}

fn is_anthropic_count_tokens_path(path: &str) -> bool {
    path.split('?')
        .next()
        .is_some_and(|path| path.ends_with("/messages/count_tokens"))
}

fn is_gemini_count_tokens_path(path: &str) -> bool {
    path.split('?')
        .next()
        .is_some_and(|path| path.ends_with(":countTokens"))
}

fn is_gemini_embed_content_path(path: &str) -> bool {
    path.split('?')
        .next()
        .is_some_and(|path| path.ends_with(":embedContent"))
}

pub(crate) fn subscription_operation_requires_buffered(provider: &str, path: &str) -> bool {
    match provider {
        "openai" | "codex" => is_openai_responses_compact_path(path),
        "claude" => is_anthropic_count_tokens_path(path),
        "antigravity" => is_gemini_count_tokens_path(path),
        _ => false,
    }
}

fn json_body_without_stream(body: &str) -> Result<String> {
    let mut value: serde_json::Value = serde_json::from_str(body)?;
    if value.get("stream").is_none() {
        return Ok(body.to_string());
    }
    if let Some(object) = value.as_object_mut() {
        object.remove("stream");
    }
    serde_json::to_string(&value).map_err(Into::into)
}

fn openai_subscription_operation_url(responses_url: String, compact: bool) -> String {
    if !compact {
        return responses_url;
    }
    let trimmed = responses_url.trim_end_matches('/');
    if trimmed.ends_with("/responses/compact") {
        trimmed.to_string()
    } else if trimmed.ends_with("/responses") {
        format!("{trimmed}/compact")
    } else {
        format!("{trimmed}/responses/compact")
    }
}

fn claude_subscription_operation_url(messages_url: String, count_tokens: bool) -> String {
    if !count_tokens {
        return messages_url;
    }
    let (base, query) = messages_url
        .split_once('?')
        .map_or((messages_url.as_str(), ""), |(base, query)| (base, query));
    let base = base.trim_end_matches('/');
    let base = if base.ends_with("/messages/count_tokens") {
        base.to_string()
    } else if base.ends_with("/messages") {
        format!("{base}/count_tokens")
    } else {
        format!("{base}/v1/messages/count_tokens")
    };
    if query.is_empty() {
        format!("{base}?beta=true")
    } else if query.split('&').any(|part| part == "beta=true") {
        format!("{base}?{query}")
    } else {
        format!("{base}?{query}&beta=true")
    }
}

fn antigravity_subscription_operation_url(generation_url: String, count_tokens: bool) -> String {
    if !count_tokens {
        return generation_url;
    }
    let (base, query) = generation_url
        .split_once('?')
        .map_or((generation_url.as_str(), ""), |(base, query)| (base, query));
    let base = base.trim_end_matches('/');
    let base = if let Some((prefix, action)) = base.rsplit_once(':') {
        if action == "generateContent"
            || action == "streamGenerateContent"
            || action == "countTokens"
        {
            format!("{prefix}:countTokens")
        } else {
            format!("{base}/v1internal:countTokens")
        }
    } else {
        format!("{base}/v1internal:countTokens")
    };
    if query.is_empty() {
        base
    } else {
        format!("{base}?{query}")
    }
}

fn antigravity_runtime_urls(upstream_override: Option<&str>, operation: &str) -> Vec<String> {
    if let Some(url) = upstream_override
        .map(str::trim)
        .filter(|url| !url.is_empty())
    {
        return vec![url.to_string()];
    }
    [ANTIGRAVITY_DAILY_API_BASE_URL, ANTIGRAVITY_API_BASE_URL]
        .into_iter()
        .map(|base| {
            let suffix = if operation == "streamGenerateContent" {
                "?alt=sse"
            } else {
                ""
            };
            format!("{base}/v1internal:{operation}{suffix}")
        })
        .collect()
}

fn antigravity_runtime_should_fallback(status: u16) -> bool {
    matches!(status, 404 | 408 | 429) || status >= 500
}

const GROK_RESPONSES_PING_COMMENT: &[u8] = b": ping\n\n";
const GROK_RESPONSES_PING_FRAME_MAX_LINES: usize = 16;
const GROK_RESPONSES_PING_FRAME_MAX_BYTES: usize = 16 * 1024;

/// Grok's subscription gateway can inject non-standard `event: ping` frames into an
/// otherwise OpenAI Responses stream. Strict Responses clients reject unknown event
/// names, so keep the frame as an SSE comment at this subscription boundary. Only a
/// possible ping frame is buffered, and both its line count and byte size are bounded.
#[derive(Default)]
struct GrokResponsesPingFilter {
    partial_line: Vec<u8>,
    ping_frame: Vec<u8>,
    ping_frame_lines: usize,
    in_passthrough_frame: bool,
}

impl GrokResponsesPingFilter {
    fn push(&mut self, bytes: &[u8]) -> (Vec<u8>, bool) {
        self.partial_line.extend_from_slice(bytes);
        let mut output = Vec::with_capacity(bytes.len());
        let mut has_payload = false;
        while let Some(newline) = self.partial_line.iter().position(|byte| *byte == b'\n') {
            let line = self.partial_line.drain(..=newline).collect::<Vec<_>>();
            self.push_line(&line, &mut output, &mut has_payload);
        }
        (output, has_payload)
    }

    fn finish(&mut self) -> (Vec<u8>, bool) {
        let mut output = Vec::new();
        let mut has_payload = false;
        if !self.partial_line.is_empty() {
            let line = std::mem::take(&mut self.partial_line);
            self.push_line(&line, &mut output, &mut has_payload);
        }
        if !self.ping_frame.is_empty() {
            self.finish_ping_frame(None, &mut output, &mut has_payload);
        }
        self.in_passthrough_frame = false;
        (output, has_payload)
    }

    fn push_line(&mut self, line: &[u8], output: &mut Vec<u8>, has_payload: &mut bool) {
        let trimmed = trim_sse_line_ending(line);
        let blank = trimmed.is_empty();

        if self.in_passthrough_frame {
            output.extend_from_slice(line);
            *has_payload |= !blank && !trimmed.starts_with(b":");
            if blank {
                self.in_passthrough_frame = false;
            }
            return;
        }

        if !self.ping_frame.is_empty() {
            if blank {
                self.finish_ping_frame(Some(line), output, has_payload);
                return;
            }
            let can_buffer = grok_ping_frame_extension(trimmed)
                && self.ping_frame_lines < GROK_RESPONSES_PING_FRAME_MAX_LINES
                && self.ping_frame.len() + line.len() <= GROK_RESPONSES_PING_FRAME_MAX_BYTES;
            if can_buffer {
                self.ping_frame.extend_from_slice(line);
                self.ping_frame_lines += 1;
                return;
            }

            output.extend_from_slice(&std::mem::take(&mut self.ping_frame));
            self.ping_frame_lines = 0;
            output.extend_from_slice(line);
            *has_payload = true;
            self.in_passthrough_frame = true;
            return;
        }

        if !blank && sse_field_value(trimmed, b"event") == Some(b"ping".as_slice()) {
            self.ping_frame.extend_from_slice(line);
            self.ping_frame_lines = 1;
            return;
        }

        output.extend_from_slice(line);
        *has_payload |= !blank && !trimmed.starts_with(b":");
        self.in_passthrough_frame = !blank;
    }

    fn finish_ping_frame(
        &mut self,
        blank_line: Option<&[u8]>,
        output: &mut Vec<u8>,
        has_payload: &mut bool,
    ) {
        if grok_ping_frame_is_filterable(&self.ping_frame) {
            output.extend_from_slice(GROK_RESPONSES_PING_COMMENT);
        } else {
            output.extend_from_slice(&self.ping_frame);
            if let Some(blank_line) = blank_line {
                output.extend_from_slice(blank_line);
            }
            *has_payload = true;
        }
        self.ping_frame.clear();
        self.ping_frame_lines = 0;
    }
}

fn trim_sse_line_ending(mut line: &[u8]) -> &[u8] {
    if line.last() == Some(&b'\n') {
        line = &line[..line.len() - 1];
    }
    if line.last() == Some(&b'\r') {
        line = &line[..line.len() - 1];
    }
    line
}

fn sse_field_value<'a>(line: &'a [u8], field: &[u8]) -> Option<&'a [u8]> {
    let colon = line.iter().position(|byte| *byte == b':')?;
    if &line[..colon] != field {
        return None;
    }
    let value = &line[colon + 1..];
    Some(value.strip_prefix(b" ").unwrap_or(value))
}

fn grok_ping_frame_extension(line: &[u8]) -> bool {
    line.starts_with(b":") || sse_field_value(line, b"data").is_some()
}

fn grok_ping_frame_is_filterable(frame: &[u8]) -> bool {
    let mut data = Vec::new();
    for line in frame.split_inclusive(|byte| *byte == b'\n').skip(1) {
        let line = trim_sse_line_ending(line);
        if let Some(value) = sse_field_value(line, b"data") {
            if !data.is_empty() {
                data.push(b'\n');
            }
            data.extend_from_slice(value);
        }
    }
    if data.is_empty() {
        return true;
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&data) else {
        return true;
    };
    value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .map(|kind| kind == "ping")
        .unwrap_or(true)
}

fn filter_grok_responses_sse(raw: &str) -> String {
    let mut filter = GrokResponsesPingFilter::default();
    let (mut output, _) = filter.push(raw.as_bytes());
    let (tail, _) = filter.finish();
    output.extend_from_slice(&tail);
    String::from_utf8(output).unwrap_or_else(|error| {
        String::from_utf8_lossy(&error.into_bytes()).into_owned()
    })
}

fn normalized_subscription_cache_identity(value: Option<&str>) -> Option<&str> {
    let value = value.map(str::trim).filter(|value| !value.is_empty())?;
    if value.len() > 256 || value.chars().any(char::is_control) {
        return None;
    }
    Some(value)
}

fn derived_openai_subscription_cache_identity(value: &serde_json::Value) -> Option<String> {
    let object = value.as_object()?;
    let input = object.get("input")?;
    let first_user = match input {
        serde_json::Value::String(_) => input,
        serde_json::Value::Array(items) => items.iter().find(|item| {
            item.as_object().is_some_and(|item| {
                item.get("role")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|role| role.eq_ignore_ascii_case("user"))
            })
        })?,
        _ => return None,
    };
    let root = serde_json::json!({
        "version": "const-local-cache-identity-v1",
        "model": object.get("model"),
        "instructions": object.get("instructions"),
        "tools": object.get("tools"),
        "first_user": first_user,
    });
    let digest = Sha256::digest(serde_json::to_vec(&root).ok()?);
    Some(format!("c1_{}", hex::encode(&digest[..20])))
}

fn openai_body_with_cache_identity_policy(
    body: &str,
    cache_identity: Option<&str>,
    derive_when_missing: bool,
) -> Result<String> {
    let mut value: serde_json::Value = serde_json::from_str(body)?;
    let Some(object) = value.as_object() else {
        return Ok(body.to_string());
    };
    match object.get("prompt_cache_key") {
        Some(serde_json::Value::String(existing)) if !existing.trim().is_empty() => {
            return Ok(body.to_string());
        }
        Some(value) if !value.is_null() && !value.is_string() => {
            return Ok(body.to_string());
        }
        _ => {}
    }
    let cache_identity = normalized_subscription_cache_identity(cache_identity)
        .map(str::to_string)
        .or_else(|| derive_when_missing.then(|| derived_openai_subscription_cache_identity(&value)).flatten());
    let Some(cache_identity) = cache_identity else {
        return Ok(body.to_string());
    };
    let Some(object) = value.as_object_mut() else {
        return Ok(body.to_string());
    };
    object.insert(
        "prompt_cache_key".to_string(),
        serde_json::Value::String(cache_identity),
    );
    serde_json::to_string(&value).map_err(Into::into)
}

pub(crate) fn openai_body_with_cache_identity(
    body: &str,
    cache_identity: Option<&str>,
) -> Result<String> {
    openai_body_with_cache_identity_policy(body, cache_identity, false)
}

pub(crate) fn openai_subscription_body_with_cache_identity(
    body: &str,
    cache_identity: Option<&str>,
) -> Result<String> {
    openai_body_with_cache_identity_policy(body, cache_identity, true)
}

include!("subscription/openai.rs");

pub(crate) fn looks_like_sse_body(raw: &str) -> bool {
    let text = raw.trim_start();
    text.starts_with("event:") || text.starts_with("data:") || text.contains("\ndata:")
}

include!("subscription/claude.rs");

include!("subscription/antigravity.rs");

pub(crate) fn subscription_provider_from_config(config: &SupplierConfig) -> String {
    crate::channel_executor::execution_kind_for_source(config.source_driver)
        .ok()
        .and_then(crate::channel_executor::subscription_provider_for_execution_kind)
        .unwrap_or_default()
        .to_string()
}

pub(crate) fn response_content_type(resp: &reqwest::Response) -> String {
    resp.headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string()
}

pub(crate) fn codex_responses_url() -> String {
    codex_responses_url_override()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| CODEX_RESPONSES_URL.to_string())
}

pub(crate) fn http_response_payload(
    status: u16,
    content_type: &str,
    body: String,
) -> serde_json::Map<String, serde_json::Value> {
    let mut headers = reqwest::header::HeaderMap::new();
    if let Ok(content_type) = reqwest::header::HeaderValue::from_str(content_type) {
        headers.insert(reqwest::header::CONTENT_TYPE, content_type);
    }
    crate::surface_wire::http_response_payload(status, &headers, body.as_bytes())
        .unwrap_or_else(|error| surface_wire_serialization_error_payload(&error))
}

fn surface_wire_serialization_error_payload(
    error: &anyhow::Error,
) -> serde_json::Map<String, serde_json::Value> {
    log::error!("surface wire response serialization failed: {error:#}");
    let body = serde_json::json!({
        "error": {
            "type": "client_serialization_error",
            "message": "CONST API client could not serialize the upstream response"
        }
    })
    .to_string();
    let mut payload = serde_json::Map::new();
    payload.insert(
        "wire".to_string(),
        serde_json::json!({
            "version": 2,
            "headers": [{
                "name": "content-type",
                "value": {"encoding": "utf8", "data": "application/json"}
            }],
            "body": {"encoding": "utf8", "data": body},
            "status": 500
        }),
    );
    payload
}

fn openai_responses_completed_body_from_sse(raw: &str) -> Result<String> {
    let mut completed = None;
    let mut completed_items = std::collections::BTreeMap::<u64, serde_json::Value>::new();
    let mut text_parts =
        std::collections::BTreeMap::<(u64, u64), (Option<String>, String, bool)>::new();
    for (event, data) in crate::protocol::stream::decode_sse_frames(raw.as_bytes())? {
        if data.trim().is_empty() || data.trim() == "[DONE]" {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(&data)?;
        let kind = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .or(event.as_deref())
            .unwrap_or_default();
        match kind {
            "response.output_item.done" => {
                let index = value
                    .get("output_index")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(completed_items.len() as u64);
                if let Some(item) = value.get("item").cloned() {
                    completed_items.insert(index, item);
                }
            }
            "response.output_text.delta" | "response.output_text.done" => {
                let output_index = value
                    .get("output_index")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                let content_index = value
                    .get("content_index")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                let item_id = value
                    .get("item_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                let part = text_parts.entry((output_index, content_index)).or_insert((
                    item_id.clone(),
                    String::new(),
                    false,
                ));
                if part.0.is_none() {
                    part.0 = item_id;
                }
                if kind == "response.output_text.done" {
                    if let Some(text) = value.get("text").and_then(serde_json::Value::as_str) {
                        part.1 = text.to_string();
                        part.2 = true;
                    }
                } else if !part.2 {
                    if let Some(delta) = value.get("delta").and_then(serde_json::Value::as_str) {
                        part.1.push_str(delta);
                    }
                }
            }
            "response.completed" => {
                completed = value.get("response").cloned();
            }
            _ => {}
        }
    }
    let mut response = completed.ok_or_else(|| {
        anyhow!("OpenAI Responses stream ended without response.completed.response")
    })?;
    let output_missing = response
        .get("output")
        .and_then(serde_json::Value::as_array)
        .map(Vec::is_empty)
        .unwrap_or(true);
    if !output_missing {
        if let Some(output) = response
            .get_mut("output")
            .and_then(serde_json::Value::as_array_mut)
        {
            for (output_index, item) in output.iter_mut().enumerate() {
                let has_id = item
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|id| !id.trim().is_empty());
                if has_id {
                    continue;
                }
                let Some(item_id) = completed_items
                    .get(&(output_index as u64))
                    .and_then(|completed| completed.get("id"))
                    .and_then(serde_json::Value::as_str)
                    .filter(|id| !id.trim().is_empty())
                else {
                    continue;
                };
                if let Some(item) = item.as_object_mut() {
                    item.insert(
                        "id".to_string(),
                        serde_json::Value::String(item_id.to_string()),
                    );
                }
            }
        }
    }
    if output_missing {
        for (output_index, item) in &mut completed_items {
            let message_content_missing = item
                .get("content")
                .and_then(serde_json::Value::as_array)
                .map(Vec::is_empty)
                .unwrap_or(false);
            if item.get("type").and_then(serde_json::Value::as_str) == Some("message")
                && message_content_missing
            {
                let content = text_parts
                    .iter()
                    .filter(|((candidate, _), _)| candidate == output_index)
                    .map(|(_, (_, text, _))| {
                        serde_json::json!({
                            "type": "output_text",
                            "text": text,
                            "annotations": []
                        })
                    })
                    .collect::<Vec<_>>();
                if !content.is_empty() {
                    if let Some(item) = item.as_object_mut() {
                        item.insert("content".to_string(), serde_json::Value::Array(content));
                    }
                }
            }
        }
        let text_output_indexes = text_parts
            .keys()
            .map(|(output_index, _)| *output_index)
            .collect::<std::collections::BTreeSet<_>>();
        for output_index in text_output_indexes {
            if completed_items.contains_key(&output_index) {
                continue;
            }
            let parts = text_parts
                .iter()
                .filter(|((candidate, _), _)| *candidate == output_index)
                .collect::<Vec<_>>();
            let content = parts
                .iter()
                .map(|(_, (_, text, _))| {
                    serde_json::json!({
                        "type": "output_text",
                        "text": text,
                        "annotations": []
                    })
                })
                .collect::<Vec<_>>();
            let mut item = serde_json::json!({
                "type": "message",
                "status": "completed",
                "role": "assistant",
                "content": content
            });
            if let Some(item_id) = parts
                .iter()
                .find_map(|(_, (item_id, _, _))| item_id.as_deref())
            {
                if let Some(item) = item.as_object_mut() {
                    item.insert(
                        "id".to_string(),
                        serde_json::Value::String(item_id.to_string()),
                    );
                }
            }
            completed_items.insert(output_index, item);
        }
        if !completed_items.is_empty() {
            response
                .as_object_mut()
                .context("response.completed.response must be an object")?
                .insert(
                    "output".to_string(),
                    serde_json::Value::Array(completed_items.into_values().collect()),
                );
        }
    }
    serde_json::to_string(&response).map_err(Into::into)
}

#[cfg(test)]
mod buffered_response_tests {
    use super::*;

    fn decoded_request_body(request: &reqwest::Request) -> Vec<u8> {
        let encoded = request
            .body()
            .and_then(reqwest::Body::as_bytes)
            .expect("in-memory request body");
        zstd::stream::decode_all(std::io::Cursor::new(encoded)).expect("zstd request body")
    }

    fn json_field_count(value: &serde_json::Value, field: &str) -> usize {
        match value {
            serde_json::Value::Array(values) => values
                .iter()
                .map(|value| json_field_count(value, field))
                .sum(),
            serde_json::Value::Object(values) => {
                usize::from(values.contains_key(field))
                    + values
                        .values()
                        .map(|value| json_field_count(value, field))
                        .sum::<usize>()
            }
            _ => 0,
        }
    }

    fn continuation_test_config(scope: &str) -> SupplierConfig {
        let mut config = default_supplier_config();
        config.channel_id = format!("continuation-{scope}");
        config.credential_ref = format!("memory://continuation/{scope}");
        config.subscription.credential_ref = config.credential_ref.clone();
        config
    }

    #[test]
    fn responses_latency_probe_skips_setup_events_and_accepts_tool_output() {
        assert!(!responses_sse_event_is_meaningful(
            r#"{"type":"response.created","response":{"id":"resp-1"}}"#
        ));
        assert!(responses_sse_event_is_meaningful(
            r#"{"type":"response.output_item.added","item":{"type":"function_call"}}"#
        ));
        assert!(responses_sse_event_is_meaningful(
            r#"{"type":"response.output_text.delta","delta":"hello"}"#
        ));
    }

    #[test]
    fn provider_latency_probe_skips_setup_and_usage_only_events() {
        assert!(!subscription_sse_event_is_meaningful(
            "openai_chat",
            r#"{"choices":[{"delta":{"role":"assistant"}}]}"#,
        ));
        assert!(subscription_sse_event_is_meaningful(
            "openai_chat",
            r#"{"choices":[{"delta":{"content":"hello"}}]}"#,
        ));

        assert!(!subscription_sse_event_is_meaningful(
            "anthropic_messages",
            r#"{"type":"message_start","message":{"id":"msg_1"}}"#,
        ));
        assert!(subscription_sse_event_is_meaningful(
            "anthropic_messages",
            r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"hello"}}"#,
        ));
        assert!(subscription_sse_event_is_meaningful(
            "anthropic_messages",
            r#"{"type":"content_block_start","content_block":{"type":"tool_use","name":"read_file"}}"#,
        ));

        assert!(!subscription_sse_event_is_meaningful(
            "gemini_native",
            r#"{"response":{"usageMetadata":{"promptTokenCount":10}}}"#,
        ));
        assert!(subscription_sse_event_is_meaningful(
            "gemini_native",
            r#"{"response":{"candidates":[{"content":{"parts":[{"text":"hello"}]}}]}}"#,
        ));
        assert!(subscription_sse_event_is_meaningful(
            "gemini_native",
            r#"{"response":{"candidates":[{"content":{"parts":[{"functionCall":{"name":"read_file"}}]}}]}}"#,
        ));
    }

    #[test]
    fn stream_debug_routes_on_meaningful_event_before_fallback_chunk() {
        let debug = SubscriptionStreamDebug {
            upstream_send_ms: 35,
            first_body_chunk_ms: Some(45),
            first_sse_event_ms: Some(90),
            first_meaningful_event_ms: Some(120),
            ..Default::default()
        };
        assert_eq!(debug.routing_latency_ms(), Some(120));

        let fallback = SubscriptionStreamDebug {
            upstream_send_ms: 35,
            first_body_chunk_ms: Some(45),
            ..Default::default()
        };
        assert_eq!(fallback.routing_latency_ms(), Some(80));
    }

    #[test]
    fn codex_http_continuation_expands_streamed_tool_context() {
        let config = continuation_test_config("streamed-tool-context");
        let first_request = serde_json::json!({
            "model": "gpt-5.6-sol",
            "prompt_cache_key": "c1_streamed_tool_context",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "inspect"}]
            }],
            "stream": true,
            "store": false
        })
        .to_string();
        let first_response = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_matrix_stream\"}}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"rs_1\",\"type\":\"reasoning\",\"encrypted_content\":\"opaque\",\"summary\":[]}}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":1,\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"status\":\"completed\",\"call_id\":\"call_read\",\"name\":\"read_file\",\"arguments\":\"{}\"}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_matrix_stream\",\"status\":\"completed\",\"output\":[]}}\n\n"
        );
        assert!(capture_codex_http_subscription_continuation(
            &config,
            &first_request,
            first_response
        ));

        let follow_up = serde_json::json!({
            "model": "gpt-5.6-sol",
            "previous_response_id": "resp_matrix_stream",
            "input": [{
                "type": "function_call_output",
                "call_id": "call_read",
                "output": "ok"
            }],
            "stream": true
        })
        .to_string();
        let repaired = prepare_codex_http_subscription_request(&config, &follow_up)
            .expect("continuation repair");
        let repaired: serde_json::Value = serde_json::from_str(&repaired).expect("repaired json");
        assert!(repaired.get("previous_response_id").is_none());
        assert_eq!(
            repaired["prompt_cache_key"],
            "c1_streamed_tool_context",
            "continuation replay must retain the first turn's upstream cache domain"
        );
        assert!(
            repaired.get("store").is_none(),
            "continuation recovery must not apply subscription endpoint policy"
        );
        let input = repaired["input"].as_array().expect("input array");
        assert_eq!(input.len(), 4);
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[1]["type"], "reasoning");
        assert_eq!(
            input[1]["id"], "rs_1",
            "continuation recovery preserves provider data until the final endpoint boundary"
        );
        assert_eq!(input[2]["type"], "function_call");
        assert_eq!(input[2]["call_id"], "call_read");
        assert_eq!(input[3]["type"], "function_call_output");
        assert_eq!(input[3]["call_id"], "call_read");
    }

    #[test]
    fn codex_http_continuation_cache_miss_starts_valid_new_turn() {
        for stream in [false, true] {
            let config = continuation_test_config(&format!("cache-miss-{stream}"));
            let body = serde_json::json!({
                "model": "gpt-5.6-sol",
                "previous_response_id": "resp_not_cached",
                "input": [{
                    "type": "function_call_output",
                    "call_id": "call_missing",
                    "output": "ok"
                }],
                "stream": stream,
                "store": false
            })
            .to_string();
            let prepared = prepare_codex_http_subscription_request(&config, &body)
                .expect("cache miss degrades to new turn");
            let prepared: serde_json::Value =
                serde_json::from_str(&prepared).expect("prepared json");
            assert_eq!(prepared["stream"], stream);
            assert!(prepared.get("previous_response_id").is_none());
            assert_eq!(prepared["input"].as_array().map(Vec::len), Some(1));
            assert_eq!(prepared["input"][0]["type"], "message");
            assert_eq!(prepared["input"][0]["role"], "user");
        }
    }

    #[test]
    fn codex_http_continuation_cache_miss_drops_only_orphan_tools() {
        let config = continuation_test_config("cache-miss-with-message");
        let body = serde_json::json!({
            "model": "gpt-5.6-sol",
            "previous_response_id": "resp_from_another_process",
            "input": [
                {
                    "type": "function_call_output",
                    "call_id": "call_orphan",
                    "output": "unlinked result"
                },
                {
                    "type": "function_call",
                    "call_id": "call_paired",
                    "name": "read_file",
                    "arguments": "{}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_paired",
                    "output": "paired result"
                },
                {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "continue"}]
                }
            ],
            "stream": true
        })
        .to_string();
        let prepared = prepare_codex_http_subscription_request(&config, &body)
            .expect("cache miss transcript repair");
        let prepared: serde_json::Value = serde_json::from_str(&prepared).expect("prepared json");
        assert!(prepared.get("previous_response_id").is_none());
        let input = prepared["input"].as_array().expect("input array");
        assert_eq!(input.len(), 3);
        assert_eq!(input[0]["type"], "function_call");
        assert_eq!(input[1]["type"], "function_call_output");
        assert_eq!(input[2]["type"], "message");
        assert!(input.iter().all(|item| {
            item.get("call_id").and_then(serde_json::Value::as_str) != Some("call_orphan")
        }));
    }

    #[test]
    fn codex_http_continuation_store_survives_process_memory_loss() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("continuations.sqlite3");
        let history = vec![serde_json::json!({
            "type": "function_call",
            "call_id": "call_persisted",
            "name": "read_file",
            "arguments": "{}"
        })];
        let state = CodexHttpContinuationState {
            history: Arc::new(history.clone()),
            cache_identity: Some("c1_persisted".to_string()),
        };
        persist_codex_http_continuation_to_path(
            &path,
            "hashed-scope-and-response",
            &state,
            1_000_000,
        )
        .expect("persist continuation");

        let loaded =
            load_codex_http_continuation_from_path(&path, "hashed-scope-and-response", 1_000_001)
                .expect("load continuation")
                .expect("persisted continuation present");
        assert_eq!(loaded, state);
    }

    fn assert_only_v2_wire(payload: &serde_json::Map<String, serde_json::Value>) {
        for legacy in ["status", "headers", "body", "start", "chunk"] {
            assert!(
                !payload.contains_key(legacy),
                "legacy field {legacy} leaked"
            );
        }
        crate::surface_wire::SurfaceEnvelope::from_payload(payload)
            .expect("typed v2 subscription payload");
    }

    #[test]
    fn subscription_stream_timeout_and_conversion_failure_produce_accounting_evidence() {
        let mut passthrough =
            inbound_sse_stream_converter("openai_responses", "openai_responses", "model").unwrap();
        let _ = passthrough.fail("stream_inactivity_timeout", "stalled");
        let timeout = subscription_stream_execution_error(&passthrough, true, false, false)
            .expect("passthrough timeout evidence");
        let timeout_evidence = execution_evidence_for_error(&timeout);
        assert_eq!(timeout_evidence.stage, "upstream_stream");
        assert_eq!(timeout_evidence.recommendation.consumer_charge, "deny");

        let mut canonical =
            inbound_sse_stream_converter("openai_responses", "openai_chat", "model").unwrap();
        let _ = canonical.push(b"data: {not-json}\n\n");
        let conversion = subscription_stream_execution_error(&canonical, false, false, false)
            .expect("conversion failure evidence");
        let conversion_evidence = execution_evidence_for_error(&conversion);
        assert_eq!(conversion_evidence.stage, "response_protocol_convert");
        assert_eq!(
            conversion_evidence.recommendation.supplier_penalty,
            "review"
        );
    }

    #[test]
    fn subscription_buffered_response_is_v2_wire_with_ordered_headers_only() {
        let payload = subscription_wire_response_payload_with_owned_headers(
            200,
            "application/json",
            "{\"ok\":true}".to_string(),
            &[
                ("set-cookie".to_string(), "a=1".to_string()),
                ("set-cookie".to_string(), "b=2".to_string()),
                ("x-repeated".to_string(), "first".to_string()),
                ("x-repeated".to_string(), "second".to_string()),
            ],
            false,
        );

        assert_only_v2_wire(&payload);
        let wire = crate::surface_wire::SurfaceEnvelope::from_payload(&payload)
            .expect("typed v2 subscription response");
        let headers = wire.header_map().expect("response headers");
        assert_eq!(wire.status, Some(200));
        assert_eq!(wire.body.decode().unwrap(), br#"{"ok":true}"#);
        assert_eq!(
            headers
                .get_all("set-cookie")
                .iter()
                .map(|value| value.to_str().unwrap())
                .collect::<Vec<_>>(),
            ["a=1", "b=2"]
        );
        assert_eq!(
            headers
                .get_all("x-repeated")
                .iter()
                .map(|value| value.to_str().unwrap())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
    }

    #[tokio::test]
    async fn subscription_stream_lifecycle_uses_v2_wire_for_start_chunk_and_end() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.append("set-cookie", "a=1".parse().unwrap());
        headers.append("set-cookie", "b=2".parse().unwrap());
        headers.insert("content-type", "application/json".parse().unwrap());
        headers.insert("content-length", "999".parse().unwrap());
        headers.insert("x-codex-turn-state", "turn-state-1".parse().unwrap());
        headers.insert("x-models-etag", "models-v2".parse().unwrap());
        let headers = subscription_stream_headers(&headers);
        let (outbound, mut inbound) = mpsc::channel(4);
        let mut started = false;
        let faults = vec![ImprovementFault::conversion_warning(
            "provider_extension_omitted",
            "anthropic_messages",
            "openai_responses",
            "gpt-5",
            "$.stream_options",
            "optional extension omitted",
        )];
        let usage = SupplierUpstreamUsage {
            source_protocol: "openai_responses".to_string(),
            input_tokens: Some(20),
            output_tokens: Some(3),
            cache_read_tokens: Some(8),
            input_tokens_include_cache_read: true,
            input_tokens_include_cache_write: true,
            ..Default::default()
        };

        send_subscription_stream_chunk(
            &outbound,
            "stream-wire",
            200,
            &headers,
            &mut started,
            &faults,
            b"data: hello\n\n",
        )
        .await
        .unwrap();
        finish_subscription_stream(
            &outbound,
            "stream-wire",
            200,
            &headers,
            &mut started,
            &faults,
            Some(&usage),
        )
        .await
        .unwrap();

        let start = inbound.recv().await.unwrap();
        let chunk = inbound.recv().await.unwrap();
        let end = inbound.recv().await.unwrap();
        assert_eq!(start.kind, "stream_start");
        assert_eq!(chunk.kind, "stream_chunk");
        assert_eq!(end.kind, "stream_end");
        for message in [&start, &chunk, &end] {
            assert_only_v2_wire(&message.payload);
        }
        let start_wire =
            crate::surface_wire::SurfaceEnvelope::from_payload(&start.payload).unwrap();
        assert_eq!(
            start.payload[IMPROVEMENT_FAULTS_FIELD][0]["code"],
            "provider_extension_omitted"
        );
        assert!(!chunk.payload.contains_key(IMPROVEMENT_FAULTS_FIELD));
        assert!(!end.payload.contains_key(IMPROVEMENT_FAULTS_FIELD));
        assert_eq!(
            end.payload[SUPPLIER_UPSTREAM_USAGE_FIELD]["cache_read_tokens"],
            8
        );
        let start_headers = start_wire.header_map().unwrap();
        assert_eq!(
            start_headers.get("content-type").unwrap(),
            "text/event-stream"
        );
        assert!(!start_headers.contains_key("content-length"));
        assert_eq!(start_headers["x-codex-turn-state"], "turn-state-1");
        assert_eq!(start_headers["x-models-etag"], "models-v2");
        assert!(!start_headers.contains_key("set-cookie"));
        let chunk_wire =
            crate::surface_wire::SurfaceEnvelope::from_payload(&chunk.payload).unwrap();
        assert_eq!(chunk_wire.body.decode().unwrap(), b"data: hello\n\n");
    }

    #[test]
    fn subscription_terminal_error_is_typed_v2_wire() {
        let payload = subscription_terminal_wire_error_payload(
            "event: error\ndata: {\"error\":{\"type\":\"usage_limit_reached\"}}\n\n",
        )
        .expect("terminal error");
        assert_only_v2_wire(&payload);
        let wire = crate::surface_wire::SurfaceEnvelope::from_payload(&payload).unwrap();
        assert_eq!(wire.status, Some(429));
        assert!(wire.body.utf8().unwrap().contains("usage_limit_reached"));
    }

    #[test]
    fn subscription_provider_selection_comes_only_from_typed_source_driver() {
        let cases = [
            (
                crate::source_driver::SourceDriverId::OpenAiSubscription,
                "openai",
            ),
            (
                crate::source_driver::SourceDriverId::ClaudeSubscription,
                "claude",
            ),
            (
                crate::source_driver::SourceDriverId::GeminiSubscription,
                "antigravity",
            ),
            (
                crate::source_driver::SourceDriverId::GrokSubscription,
                "grok",
            ),
        ];

        for (source_driver, expected) in cases {
            let mut config = default_supplier_config();
            config.source_driver = source_driver;
            config.subscription.platform = "legacy-conflict".to_string();
            config.upstream_base_url = "local://subscription/legacy-conflict".to_string();

            assert_eq!(subscription_provider_from_config(&config), expected);
        }

        let mut http = default_supplier_config();
        http.subscription.platform = "openai".to_string();
        http.upstream_base_url = "local://subscription/openai".to_string();
        assert_eq!(subscription_provider_from_config(&http), "");
    }

    #[tokio::test]
    async fn rust_retained_subscription_wire_fixture_producer() {
        let Ok(output_path) = std::env::var("CONST_API_SURFACE_WIRE_FIXTURE_OUT") else {
            return;
        };
        let mut records = Vec::new();

        for source_driver in [
            crate::source_driver::SourceDriverId::OpenAiSubscription,
            crate::source_driver::SourceDriverId::ClaudeSubscription,
            crate::source_driver::SourceDriverId::GeminiSubscription,
            crate::source_driver::SourceDriverId::GrokSubscription,
        ] {
            let mut config = default_supplier_config();
            config.source_driver = source_driver;
            config.subscription.platform = "legacy-conflict".to_string();
            let provider = subscription_provider_from_config(&config);
            let buffered_body = format!(r#"{{"provider":"{provider}","buffered":true}}"#);
            let buffered = subscription_wire_response_payload_with_owned_headers(
                200,
                "application/json",
                buffered_body.clone(),
                &[
                    (
                        "content-length".to_string(),
                        buffered_body.len().to_string(),
                    ),
                    ("x-repeated".to_string(), "first".to_string()),
                    ("x-repeated".to_string(), "second".to_string()),
                ],
                false,
            );
            records.push(serde_json::json!({
                "provider": provider,
                "kind": "http_response",
                "payload": buffered,
            }));

            let mut headers = reqwest::header::HeaderMap::new();
            headers.append("x-repeated", "first".parse().unwrap());
            headers.append("x-repeated", "second".parse().unwrap());
            headers.insert("content-type", "application/json".parse().unwrap());
            headers.insert("content-length", "999".parse().unwrap());
            let headers = subscription_stream_headers(&headers);
            let (outbound, mut inbound) = mpsc::channel(3);
            let mut started = false;
            send_subscription_stream_chunk(
                &outbound,
                &provider,
                200,
                &headers,
                &mut started,
                &[],
                format!("data: {provider}\n\n").as_bytes(),
            )
            .await
            .unwrap();
            finish_subscription_stream(
                &outbound,
                &provider,
                200,
                &headers,
                &mut started,
                &[],
                None,
            )
            .await
            .unwrap();
            for _ in 0..3 {
                let message = inbound.recv().await.unwrap();
                records.push(serde_json::json!({
                    "provider": provider,
                    "kind": message.kind,
                    "payload": message.payload,
                }));
            }

            records.push(serde_json::json!({
                "provider": provider,
                "kind": "error",
                "payload": subscription_wire_error_response(
                    429,
                    "quota_exhausted",
                    &format!("{provider} retained subscription quota exhausted"),
                ),
            }));
        }

        std::fs::write(output_path, serde_json::to_vec(&records).unwrap()).unwrap();
    }

    #[test]
    fn local_subscription_cache_identity_uses_explicit_conversation_fields() {
        for body in [
            r#"{"prompt_cache_key":"cache-1"}"#,
            r#"{"client_metadata":{"session_id":"cache-1"}}"#,
            r#"{"client_metadata":{"thread_id":"cache-1"}}"#,
            r#"{"conversation":{"id":"cache-1"}}"#,
        ] {
            assert_eq!(
                subscription_body_cache_identity(body).as_deref(),
                Some("cache-1")
            );
        }
        assert_eq!(
            subscription_body_cache_identity(r#"{"prompt_cache_key":"bad\nidentity"}"#),
            None
        );
    }

    #[test]
    fn openai_subscription_cache_identity_is_injected_without_overriding_the_caller() {
        let injected = openai_subscription_body_with_cache_identity(
            r#"{"model":"gpt-5.6-sol","input":"hello"}"#,
            Some("c1_stable"),
        )
        .unwrap();
        let injected: serde_json::Value = serde_json::from_str(&injected).unwrap();
        assert_eq!(injected["prompt_cache_key"], "c1_stable");

        let caller = r#"{"model":"gpt-5.6-sol","prompt_cache_key":"caller-key"}"#;
        assert_eq!(
            openai_subscription_body_with_cache_identity(caller, Some("gateway-key")).unwrap(),
            caller
        );
        assert_eq!(
            openai_subscription_body_with_cache_identity(caller, Some("bad\nidentity")).unwrap(),
            caller
        );
    }

    #[test]
    fn openai_subscription_derives_a_stable_local_identity_for_growing_conversations() {
        let first = r#"{
            "model":"gpt-5.6-sol",
            "instructions":"stable system",
            "tools":[{"type":"function","name":"lookup"}],
            "input":[{"role":"user","content":"first question"}]
        }"#;
        let grown = r#"{
            "model":"gpt-5.6-sol",
            "instructions":"stable system",
            "tools":[{"type":"function","name":"lookup"}],
            "input":[
                {"role":"user","content":"first question"},
                {"role":"assistant","content":"answer"},
                {"role":"user","content":"follow up"}
            ]
        }"#;
        let first: serde_json::Value = serde_json::from_str(
            &openai_subscription_body_with_cache_identity(first, None).unwrap(),
        )
        .unwrap();
        let grown: serde_json::Value = serde_json::from_str(
            &openai_subscription_body_with_cache_identity(grown, None).unwrap(),
        )
        .unwrap();

        assert_eq!(first["prompt_cache_key"], grown["prompt_cache_key"]);
        assert!(first["prompt_cache_key"]
            .as_str()
            .is_some_and(|value| value.starts_with("c1_") && value.len() == 43));
    }

    #[test]
    fn codex_response_request_matches_current_session_identity_contract() {
        let body = serde_json::json!({
            "model": "gpt-5.6-sol",
            "prompt_cache_key": "cache-key-override",
            "client_metadata": {
                "session_id": "session-1",
                "thread_id": "thread-1",
                "x-codex-installation-id": "installation-1",
                "x-codex-window-id": "window-1",
                "x-codex-turn-metadata": "{\"request_kind\":\"turn\"}",
                "x-codex-parent-thread-id": "parent-1",
                "x-openai-subagent": "collab_spawn",
                "x-untrusted-metadata": "must-not-be-forwarded"
            }
        })
        .to_string();
        let request = openai_subscription_response_request(
            &Client::new(),
            "https://example.test/backend-api/codex/responses",
            "token",
            &serde_json::json!({"account_id": "account-1"}),
            body.clone(),
            None,
        )
        .expect("Codex request builder")
        .build()
        .expect("Codex response request");
        let headers = request.headers();

        assert_eq!(headers.get("session-id").unwrap(), "session-1");
        assert_eq!(headers.get("thread-id").unwrap(), "thread-1");
        assert_eq!(headers.get("x-client-request-id").unwrap(), "thread-1");
        assert_eq!(headers.get("x-codex-window-id").unwrap(), "window-1");
        assert_eq!(
            headers.get("x-codex-turn-metadata").unwrap(),
            "{\"request_kind\":\"turn\"}"
        );
        assert_eq!(headers.get("x-codex-parent-thread-id").unwrap(), "parent-1");
        assert_eq!(headers.get("x-openai-subagent").unwrap(), "collab_spawn");
        assert_eq!(headers.get("chatgpt-account-id").unwrap(), "account-1");
        assert!(!headers.contains_key("openai-beta"));
        assert!(!headers.contains_key("x-codex-installation-id"));
        assert!(!headers.contains_key("x-untrusted-metadata"));
        assert_eq!(headers.get("content-encoding").unwrap(), "zstd");
        assert_eq!(decoded_request_body(&request), body.as_bytes());
    }

    #[test]
    fn generated_responses_lite_body_adds_the_required_codex_header() {
        let body = serde_json::json!({
            "model": "gpt-5.6-luna",
            "instructions": "",
            "input": [{"type": "additional_tools", "role": "developer", "tools": []}],
            "stream": true,
            "store": false
        })
        .to_string();
        let request = openai_subscription_response_request(
            &Client::new(),
            "https://example.test/backend-api/codex/responses",
            "token",
            &serde_json::json!({}),
            body,
            None,
        )
        .expect("Codex request builder")
        .build()
        .expect("Codex Responses Lite request");

        assert_eq!(
            request
                .headers()
                .get("x-openai-internal-codex-responses-lite")
                .unwrap(),
            "true"
        );
    }

    #[test]
    fn codex_response_request_does_not_confuse_cache_key_with_session_identity() {
        let body = r#"{"model":"gpt-5.6-sol","prompt_cache_key":"stable-cache-key"}"#.to_string();
        let request = openai_subscription_response_request(
            &Client::new(),
            "https://example.test/backend-api/codex/responses",
            "token",
            &serde_json::json!({}),
            body.clone(),
            None,
        )
        .expect("Codex request builder")
        .build()
        .expect("Codex response request");

        assert!(!request.headers().contains_key("session-id"));
        assert!(!request.headers().contains_key("thread-id"));
        assert!(!request.headers().contains_key("x-client-request-id"));
        assert_eq!(request.headers()["content-encoding"], "zstd");
        assert_eq!(decoded_request_body(&request), body.as_bytes());
    }

    #[test]
    fn codex_response_request_preserves_complete_caller_identity_and_turn_state() {
        let mut inbound = reqwest::header::HeaderMap::new();
        inbound.insert("originator", "codex_vscode".parse().unwrap());
        inbound.insert("user-agent", "codex_vscode/1.2.3".parse().unwrap());
        inbound.insert("x-codex-turn-state", "turn-state-1".parse().unwrap());
        inbound.insert("x-oai-attestation", "attestation-1".parse().unwrap());
        inbound.insert("x-not-codex", "drop-me".parse().unwrap());

        let request = openai_subscription_response_request(
            &Client::new(),
            "https://example.test/backend-api/codex/responses",
            "token",
            &serde_json::json!({}),
            r#"{"model":"gpt-5.6-sol","input":"hi"}"#.to_string(),
            Some(&inbound),
        )
        .expect("Codex request builder")
        .build()
        .expect("Codex response request");

        assert_eq!(request.headers()["originator"], "codex_vscode");
        assert_eq!(request.headers()["user-agent"], "codex_vscode/1.2.3");
        assert_eq!(request.headers()["version"], "1.2.3");
        assert_eq!(request.headers()["x-codex-turn-state"], "turn-state-1");
        assert_eq!(request.headers()["x-oai-attestation"], "attestation-1");
        assert!(!request.headers().contains_key("x-not-codex"));
    }

    #[test]
    fn codex_response_request_skips_invalid_identity_headers() {
        let body = serde_json::json!({
            "prompt_cache_key": "cache-key\nheader-injection",
            "client_metadata": {
                "thread_id": "thread\nheader-injection"
            }
        })
        .to_string();
        let request = openai_subscription_response_request(
            &Client::new(),
            "https://example.test/backend-api/codex/responses",
            "token",
            &serde_json::json!({}),
            body,
            None,
        )
        .expect("Codex request builder")
        .build()
        .expect("Codex response request");

        assert!(!request.headers().contains_key("session-id"));
        assert!(!request.headers().contains_key("thread-id"));
        assert!(!request.headers().contains_key("x-client-request-id"));
    }

    #[test]
    fn claude_request_preserves_cache_contract_and_current_session_identity() {
        let session_id = "123e4567-e89b-12d3-a456-426614174000";
        let body = serde_json::json!({
            "model": "claude-sonnet-5",
            "cache_control": {"type": "ephemeral"},
            "system": [{
                "type": "text",
                "text": "stable system prefix",
                "cache_control": {"type": "ephemeral", "ttl": "1h"}
            }],
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "continue"}]
            }],
            "metadata": {
                "user_id": serde_json::json!({
                    "device_id": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                    "account_uuid": "",
                    "session_id": session_id
                }).to_string()
            }
        })
        .to_string();
        let request = claude_subscription_request(
            &Client::new(),
            "https://api.anthropic.test/v1/messages",
            "token",
            body.clone(),
        )
        .build()
        .expect("Claude request");

        assert_eq!(
            request.headers().get("x-claude-code-session-id").unwrap(),
            session_id
        );
        assert_eq!(
            request.headers().get("user-agent").unwrap(),
            CLAUDE_CODE_USER_AGENT
        );
        assert_eq!(
            request.headers().get("x-stainless-retry-count").unwrap(),
            "0"
        );
        assert_eq!(
            request.headers().get("x-stainless-package-version").unwrap(),
            CLAUDE_CODE_STAINLESS_PACKAGE_VERSION
        );
        assert_eq!(
            request.headers().get("x-stainless-os").unwrap(),
            claude_code_stainless_os()
        );
        assert_eq!(
            request.headers().get("x-stainless-arch").unwrap(),
            claude_code_stainless_arch()
        );
        assert_eq!(
            request.headers().get("x-stainless-runtime-version").unwrap(),
            CLAUDE_CODE_STAINLESS_RUNTIME_VERSION
        );
        assert_eq!(request.headers().get("x-stainless-timeout").unwrap(), "600");
        assert_eq!(
            request
                .headers()
                .get("anthropic-dangerous-direct-browser-access")
                .unwrap(),
            CLAUDE_DIRECT_BROWSER_ACCESS
        );
        assert_eq!(request.headers().get("accept").unwrap(), "application/json");
        assert!(request
            .headers()
            .get("x-client-request-id")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.len() == 36));
        assert!(request
            .headers()
            .get("anthropic-beta")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.contains("prompt-caching-scope-2026-01-05")
                && value.contains("extended-cache-ttl-2025-04-11")));
        assert_eq!(
            request.body().and_then(reqwest::Body::as_bytes),
            Some(body.as_bytes())
        );
    }

    #[test]
    fn non_claude_tools_keep_long_context_beta_without_borrowing_their_identity() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "user-agent",
            reqwest::header::HeaderValue::from_static("another-tool"),
        );
        headers.append("anthropic-beta", "compact-2026-01-12".parse().unwrap());
        headers.append(
            "anthropic-beta",
            crate::channel_executor::ANTHROPIC_1M_BETA.parse().unwrap(),
        );
        let request = claude_subscription_request_with_headers(
            &Client::new(),
            "https://api.anthropic.test/v1/messages",
            "test-token",
            r#"{"model":"claude-sonnet-4-6","messages":[]}"#.into(),
            Some(&headers),
        )
        .build()
        .unwrap();
        let beta = request.headers()["anthropic-beta"].to_str().unwrap();
        assert_eq!(
            beta.matches(crate::channel_executor::ANTHROPIC_1M_BETA)
                .count(),
            1
        );
        assert!(beta.contains("oauth-2025-04-20"));
        assert_ne!(request.headers()["user-agent"], "another-tool");
    }

    #[test]
    fn claude_beta_opt_ins_merge_repeated_headers_without_changing_identity_or_body() {
        for native in [false, true] {
            for endpoint in ["messages", "messages/count_tokens"] {
                let mut headers = reqwest::header::HeaderMap::new();
                headers.insert(
                    "user-agent",
                    if native {
                        "claude-cli/2.1.272 (external, cli)"
                    } else {
                        "another-tool"
                    }
                    .parse()
                    .unwrap(),
                );
                headers.insert(
                    "authorization",
                    "Bearer caller-key-must-not-be-forwarded".parse().unwrap(),
                );
                headers.append(
                    "anthropic-beta",
                    "per-turn-control-2026-07-01, oauth-2025-04-20"
                        .parse()
                        .unwrap(),
                );
                headers.append(
                    "anthropic-beta",
                    "PER-TURN-CONTROL-2026-07-01,context-1m-2025-08-07"
                        .parse()
                        .unwrap(),
                );
                let body = serde_json::json!({
                    "model":"claude-sonnet-4-6",
                    "system":[{"type":"text","text":if native { "x-anthropic-billing-header: cc_version=2.1.272; cc_entrypoint=cli; cch=abcde;" } else { "stable prefix" },"cache_control":{"type":"ephemeral","ttl":"1h"}}],
                    "messages":[{"role":"user","content":"hello"}],
                    "context_management":{"edits":[{"type":"compact_20260112"}]}
                }).to_string();
                let request = claude_subscription_request_with_headers(
                    &Client::new(),
                    &format!("https://api.anthropic.test/v1/{endpoint}"),
                    "upstream-test-token",
                    body.clone(),
                    Some(&headers),
                )
                .build()
                .unwrap();
                let betas = request.headers()["anthropic-beta"]
                    .to_str()
                    .unwrap()
                    .split(',')
                    .collect::<Vec<_>>();
                for beta in [
                    "per-turn-control-2026-07-01",
                    "oauth-2025-04-20",
                    "context-1m-2025-08-07",
                    CLAUDE_SERVER_SIDE_COMPACTION_BETA,
                ] {
                    assert_eq!(
                        betas
                            .iter()
                            .filter(|value| value.eq_ignore_ascii_case(beta))
                            .count(),
                        1,
                        "{native} {endpoint}: {betas:?}"
                    );
                }
                if !native && endpoint.ends_with("count_tokens") {
                    assert!(betas.contains(&"token-counting-2024-11-01"));
                }
                assert_eq!(
                    request.headers()["authorization"],
                    "Bearer upstream-test-token"
                );
                assert_eq!(request.body().unwrap().as_bytes().unwrap(), body.as_bytes());
                if native {
                    assert_eq!(request.headers()["user-agent"], headers["user-agent"]);
                    assert!(!request.headers().contains_key("x-stainless-runtime"));
                } else {
                    assert_eq!(request.headers()["user-agent"], CLAUDE_CODE_USER_AGENT);
                }
            }
        }
    }

    #[test]
    fn native_claude_request_preserves_real_client_identity_headers() {
        let body = serde_json::json!({
            "model": "claude-sonnet-4-6",
            "system": [{
                "type": "text",
                "text": "x-anthropic-billing-header: cc_version=2.1.241; cc_entrypoint=cli; cch=abcde;"
            }],
            "messages": [{"role": "user", "content": "hello"}],
            "stream": true
        })
        .to_string();
        let mut inbound = reqwest::header::HeaderMap::new();
        inbound.insert(
            reqwest::header::ACCEPT,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        inbound.insert(
            reqwest::header::ACCEPT_ENCODING,
            reqwest::header::HeaderValue::from_static(CLAUDE_RESPONSE_ACCEPT_ENCODING),
        );
        inbound.insert(
            reqwest::header::USER_AGENT,
            reqwest::header::HeaderValue::from_static("claude-cli/2.1.241 (external, cli)"),
        );
        inbound.insert("x-app", reqwest::header::HeaderValue::from_static("cli"));
        inbound.insert(
            "anthropic-dangerous-direct-browser-access",
            reqwest::header::HeaderValue::from_static("true"),
        );
        inbound.insert(
            "x-stainless-os",
            reqwest::header::HeaderValue::from_static("Windows"),
        );
        inbound.insert(
            "x-stainless-arch",
            reqwest::header::HeaderValue::from_static("x64"),
        );
        inbound.insert(
            "x-auth-nonce",
            reqwest::header::HeaderValue::from_static("native-nonce"),
        );
        inbound.insert(
            "anthropic-beta",
            reqwest::header::HeaderValue::from_static(
                "claude-code-20250219,interleaved-thinking-2025-05-14",
            ),
        );

        let request = claude_subscription_request_with_headers(
            &Client::new(),
            "https://api.anthropic.test/v1/messages",
            "token",
            body,
            Some(&inbound),
        )
        .build()
        .expect("native Claude request");

        assert_eq!(
            request.headers().get(reqwest::header::USER_AGENT).unwrap(),
            "claude-cli/2.1.241 (external, cli)"
        );
        assert_eq!(request.headers().get("x-stainless-os").unwrap(), "Windows");
        assert_eq!(request.headers().get("x-stainless-arch").unwrap(), "x64");
        assert_eq!(request.headers().get("accept").unwrap(), "application/json");
        assert_eq!(
            request.headers().get("x-auth-nonce").unwrap(),
            "native-nonce"
        );
        assert!(!request
            .headers()
            .contains_key("x-stainless-package-version"));
        assert!(!request.headers().contains_key("x-stainless-runtime"));
        assert!(!request.headers().contains_key("x-client-request-id"));
        assert!(!request.headers().contains_key("connection"));
        assert_eq!(
            request.headers().get("accept-encoding").unwrap(),
            CLAUDE_RESPONSE_ACCEPT_ENCODING
        );
        assert_eq!(
            request
                .headers()
                .get("anthropic-dangerous-direct-browser-access")
                .unwrap(),
            "true"
        );
        assert!(request
            .headers()
            .get("anthropic-beta")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value
                == "claude-code-20250219,interleaved-thinking-2025-05-14,oauth-2025-04-20"));
    }

    #[test]
    fn native_subscription_old_and_future_versions_preserve_protocol_payloads() {
        let client = Client::new();
        for version in ["0.120.0", "0.154.0", "0.200.0"] {
            let body = r#"{ "model":"gpt-test", "stream":true, "store":false,
                "input":[{"type":"future_input_v9","future":{"nested":[1,null,true]}},
                {"type":"additional_tools","role":"developer","tools":[{"type":"namespace","name":"future","tools":[{"type":"future_tool_v9","opaque":"keep"}]}]}],
                "future_top":{"keep":true} }"#;
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert("originator", "codex_cli_rs".parse().unwrap());
            let user_agent = format!("codex_cli_rs/{version} (Windows; x86_64)");
            headers.insert("user-agent", user_agent.parse().unwrap());
            headers.insert("version", version.parse().unwrap());
            headers.insert("openai-beta", "tools=future-v9".parse().unwrap());
            let conversion = upstream_request_for_api_format_with_profiles_report("openai_responses", "/v1/responses", body, "gpt-test", &[]).unwrap();
            assert!(conversion.tool_mapping.is_empty());
            assert_eq!(conversion.body, body);
            let prepared = prepare_codex_subscription_endpoint_request(&default_supplier_config(), &conversion.body, false, "gpt-test", None, Some(&headers)).unwrap();
            assert_eq!(prepared.body, body);
            let request = openai_subscription_response_request(&client, "https://example.test/responses", "mock-supplier-token", &serde_json::json!({}), prepared.body, Some(&headers)).unwrap().build().unwrap();
            let encoded = request.body().unwrap().as_bytes().unwrap();
            assert_eq!(zstd::stream::decode_all(std::io::Cursor::new(encoded)).unwrap(), body.as_bytes());
            assert_eq!(request.headers()["version"], version);
            assert_eq!(request.headers()["user-agent"], user_agent);
            assert_eq!(request.headers()["openai-beta"], "tools=future-v9");
        }
        for version in ["2.1.220", "2.1.285", "3.0.0"] {
            let model = "claude-sonnet-5-5";
            let body = serde_json::json!({"model":model, "stream":true,
                "system":[{"type":"text","text":format!("x-anthropic-billing-header: cc_version={version}; cc_entrypoint=cli;")}],
                "messages":[{"role":"user","content":[{"type":"future_content_v9","opaque":{"keep":true}}]}],
                "thinking":{"type":"between_tools"},
                "tools":[{"type":"browser_toolset_20260801"},{"type":"future_tool_v9","name":"future"}],"future_top":{"keep":true}
            }).to_string();
            let mut headers = reqwest::header::HeaderMap::new();
            let user_agent = format!("claude-cli/{version} (external, cli)");
            headers.insert("user-agent", user_agent.parse().unwrap());
            headers.insert("anthropic-beta", "future-feature-2099-01-01".parse().unwrap());
            let conversion = upstream_request_for_api_format_with_profiles_report("anthropic_messages", "/v1/messages", &body, model, &[]).unwrap();
            assert!(conversion.tool_mapping.is_empty());
            assert_eq!(conversion.body, body);
            let adjusted = ensure_claude_messages_body_with_faults(&conversion.body, model).unwrap();
            let prepared = prepare_claude_subscription_oauth_body(&adjusted.body, "mock-credential-reference").unwrap();
            assert_eq!(prepared, body);
            let request = claude_subscription_request_with_headers(&client, "https://example.test/v1/messages", "mock-supplier-token", prepared, Some(&headers)).build().unwrap();
            assert_eq!(request.body().unwrap().as_bytes().unwrap(), body.as_bytes());
            assert_eq!(request.headers()["user-agent"], user_agent);
            assert!(request.headers()["anthropic-beta"].to_str().unwrap().contains("future-feature-2099-01-01"));
        }
    }

    #[test]
    fn claude_compaction_control_and_replay_add_the_required_beta() {
        let bodies = [
            serde_json::json!({
                "model": "claude-sonnet-4-6",
                "messages": [{"role": "user", "content": "compact this conversation"}],
                "context_management": {
                    "edits": [{
                        "type": "compact_20260112",
                        "trigger": {"type": "input_tokens", "value": 50000},
                        "pause_after_compaction": true
                    }]
                }
            }),
            serde_json::json!({
                "model": "claude-sonnet-4-6",
                "messages": [
                    {"role": "user", "content": "long prior request"},
                    {"role": "assistant", "content": [{
                        "type": "compaction",
                        "content": "summary",
                        "encrypted_content": "opaque-state"
                    }]},
                    {"role": "user", "content": "continue"}
                ]
            }),
        ];

        for body in bodies {
            let request = claude_subscription_request(
                &Client::new(),
                "https://api.anthropic.test/v1/messages",
                "token",
                body.to_string(),
            )
            .build()
            .expect("Claude compaction request");
            let beta = request
                .headers()
                .get("anthropic-beta")
                .and_then(|value| value.to_str().ok())
                .expect("anthropic-beta");
            assert!(beta.split(',').any(|value| value == CLAUDE_SERVER_SIDE_COMPACTION_BETA));
            assert_eq!(
                beta.split(',')
                    .filter(|value| value.eq_ignore_ascii_case(CLAUDE_SERVER_SIDE_COMPACTION_BETA))
                    .count(),
                1
            );
        }
    }

    #[test]
    fn native_claude_compaction_keeps_identity_and_appends_required_beta() {
        let body = serde_json::json!({
            "model": "claude-sonnet-4-6",
            "system": [{
                "type": "text",
                "text": "x-anthropic-billing-header: cc_version=2.1.241; cc_entrypoint=cli; cch=abcde;"
            }],
            "messages": [{"role": "user", "content": "compact"}],
            "context_management": {"edits": [{"type": "compact_20260112"}]}
        })
        .to_string();
        let mut inbound = reqwest::header::HeaderMap::new();
        inbound.insert(
            reqwest::header::USER_AGENT,
            reqwest::header::HeaderValue::from_static("claude-cli/2.1.241 (external, cli)"),
        );
        inbound.insert(
            "anthropic-beta",
            reqwest::header::HeaderValue::from_static("claude-code-20250219"),
        );

        let request = claude_subscription_request_with_headers(
            &Client::new(),
            "https://api.anthropic.test/v1/messages",
            "token",
            body,
            Some(&inbound),
        )
        .build()
        .expect("native Claude compaction request");
        assert_eq!(
            request.headers().get(reqwest::header::USER_AGENT).unwrap(),
            "claude-cli/2.1.241 (external, cli)"
        );
        assert_eq!(
            request
                .headers()
                .get("anthropic-beta")
                .and_then(|value| value.to_str().ok()),
            Some("claude-code-20250219,oauth-2025-04-20,compact-2026-01-12")
        );
    }

    #[test]
    fn claude_count_tokens_request_uses_the_token_counting_contract() {
        let request = claude_subscription_request(
            &Client::new(),
            "https://api.anthropic.test/v1/messages/count_tokens?beta=true",
            "token",
            serde_json::json!({
                "model": "claude-sonnet-4-6",
                "messages": [{"role": "user", "content": "hello"}]
            })
            .to_string(),
        )
        .build()
        .expect("Claude count_tokens request");

        assert_eq!(
            request
                .headers()
                .get("anthropic-beta")
                .and_then(|value| value.to_str().ok()),
            Some(CLAUDE_COUNT_TOKENS_BETA_HEADER)
        );
        assert!(CLAUDE_COUNT_TOKENS_BETA_HEADER.contains("context-management-2025-06-27"));
        assert!(CLAUDE_COUNT_TOKENS_BETA_HEADER.contains("token-counting-2024-11-01"));
        assert!(!CLAUDE_COUNT_TOKENS_BETA_HEADER.contains(CLAUDE_SERVER_SIDE_COMPACTION_BETA));
        assert_eq!(request.headers().get("accept").unwrap(), "application/json");
        assert_eq!(
            request.headers().get("accept-encoding").unwrap(),
            CLAUDE_RESPONSE_ACCEPT_ENCODING
        );

        let compaction = claude_subscription_request(
            &Client::new(),
            "https://api.anthropic.test/v1/messages/count_tokens?beta=true",
            "token",
            serde_json::json!({
                "model": "claude-sonnet-4-6",
                "messages": [{"role": "assistant", "content": [{
                    "type": "compaction",
                    "content": "summary"
                }]}],
                "context_management": {"edits": [{"type": "compact_20260112"}]}
            })
            .to_string(),
        )
        .build()
        .expect("Claude compaction count_tokens request");
        let beta = compaction
            .headers()
            .get("anthropic-beta")
            .and_then(|value| value.to_str().ok())
            .expect("count_tokens beta");
        assert!(beta.contains("token-counting-2024-11-01"));
        assert!(beta.contains(CLAUDE_SERVER_SIDE_COMPACTION_BETA));
    }

    #[test]
    fn claude_stream_request_negotiates_official_response_compression() {
        let request = claude_subscription_request(
            &Client::new(),
            "https://api.anthropic.test/v1/messages",
            "token",
            serde_json::json!({
                "model": "claude-sonnet-4-6",
                "messages": [{"role": "user", "content": "hello"}],
                "stream": true
            })
            .to_string(),
        )
        .build()
        .expect("Claude streaming request");

        assert_eq!(
            request.headers().get("accept").unwrap(),
            "text/event-stream"
        );
        assert_eq!(
            request.headers().get("accept-encoding").unwrap(),
            CLAUDE_RESPONSE_ACCEPT_ENCODING
        );
        assert_eq!(
            request.headers().get("x-stainless-helper-method").unwrap(),
            "stream"
        );
    }

    #[tokio::test]
    async fn claude_zstd_sse_is_decoded_before_the_response_ends() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("zstd SSE listener");
        let address = listener.local_addr().expect("zstd SSE address");
        let expected = b"event: message_start\ndata: {\"type\":\"message_start\"}\n\n";
        let compressed =
            zstd::stream::encode_all(std::io::Cursor::new(expected), 3).expect("zstd SSE frame");
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("zstd SSE accept");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = socket.read(&mut buffer).await.expect("read Claude request");
                assert!(read > 0, "Claude request ended before its headers");
                request.extend_from_slice(&buffer[..read]);
            }
            let request = String::from_utf8_lossy(&request).to_ascii_lowercase();
            assert!(request.contains(&format!(
                "accept-encoding: {}",
                CLAUDE_RESPONSE_ACCEPT_ENCODING
            )));

            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Encoding: zstd\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("write zstd SSE headers");
            socket
                .write_all(format!("{:X}\r\n", compressed.len()).as_bytes())
                .await
                .expect("write zstd chunk length");
            socket
                .write_all(&compressed)
                .await
                .expect("write zstd SSE frame");
            socket
                .write_all(b"\r\n")
                .await
                .expect("finish zstd SSE chunk");
            socket.flush().await.expect("flush zstd SSE frame");

            let _ = finish_rx.await;
            socket
                .write_all(b"0\r\n\r\n")
                .await
                .expect("finish zstd SSE response");
            socket.flush().await.expect("flush zstd SSE response end");
            socket.shutdown().await.expect("shutdown zstd SSE response");
        });

        let response = claude_subscription_request(
            &Client::new(),
            &format!("http://{address}/v1/messages?beta=true"),
            "token",
            serde_json::json!({
                "model": "claude-sonnet-4-6",
                "messages": [{"role": "user", "content": "hello"}],
                "stream": true
            })
            .to_string(),
        )
        .send()
        .await
        .expect("Claude zstd SSE response");
        let mut stream = response.bytes_stream();
        let first = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
            .await
            .expect("zstd SSE first event before response end")
            .expect("zstd SSE body chunk")
            .expect("decoded zstd SSE chunk");
        assert_eq!(first.as_ref(), expected);
        finish_tx.send(()).expect("release zstd SSE response");
        let mut trailing = Vec::new();
        while let Some(chunk) = stream.next().await {
            trailing.extend_from_slice(&chunk.expect("decoded trailing zstd SSE chunk"));
        }
        assert!(trailing.is_empty());
        server.await.expect("zstd SSE server");
    }

    #[test]
    fn claude_fast_mode_beta_is_only_added_when_requested() {
        let request = claude_subscription_request(
            &Client::new(),
            "https://api.anthropic.test/v1/messages",
            "token",
            serde_json::json!({
                "model": "claude-sonnet-4-6",
                "messages": [{"role": "user", "content": "hello"}],
                "speed": "fast"
            })
            .to_string(),
        )
        .build()
        .expect("Claude fast request");

        assert!(request
            .headers()
            .get("anthropic-beta")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.contains("fast-mode-2026-02-01")));
    }

    #[test]
    fn claude_oauth_body_wrapper_is_idempotent_and_preserves_system_semantics() {
        let generic = serde_json::json!({
            "model": "claude-sonnet-4-6",
            "system": [{
                "type": "text",
                "text": "Only answer about this repository.",
                "cache_control": {"type": "ephemeral", "ttl": "1h"}
            }],
            "messages": [{"role": "user", "content": "review the code"}],
            "metadata": {"user_id": "application-user-7"},
            "stream": false
        })
        .to_string();

        let wrapped = prepare_claude_subscription_oauth_body(&generic, "credential-a")
            .expect("wrapped Claude OAuth body");
        let wrapped_again = prepare_claude_subscription_oauth_body(&wrapped, "credential-a")
            .expect("idempotent Claude OAuth body");
        let value: serde_json::Value = serde_json::from_str(&wrapped).unwrap();

        assert_eq!(wrapped_again, wrapped);
        assert!(value["system"][0]["text"]
            .as_str()
            .is_some_and(|text| text.starts_with(&format!(
                "x-anthropic-billing-header: cc_version={CLAUDE_CODE_VERSION}."
            ))));
        assert_eq!(value["system"][1]["text"], CLAUDE_CODE_IDENTITY_PROMPT);
        assert_eq!(
            value["messages"][0]["content"][0]["text"],
            "[System Instructions]\nOnly answer about this repository."
        );
        assert_eq!(
            value["messages"][0]["content"][0]["cache_control"]["ttl"],
            "1h"
        );
        assert!(value["system"][2].get("cache_control").is_none());
        assert_eq!(json_field_count(&value, "cache_control"), 1);
        assert_eq!(value["messages"][2]["content"], "review the code");
        let session_id = claude_code_session_id_from_body(&wrapped).expect("generated session");
        let other = prepare_claude_subscription_oauth_body(
            &generic.replace("review the code", "fix the tests"),
            "credential-a",
        )
        .expect("second Claude OAuth conversation");
        assert_ne!(
            claude_code_session_id_from_body(&other).as_deref(),
            Some(session_id.as_str())
        );
    }

    #[test]
    fn claude_oauth_body_wrapper_preserves_compaction_control_and_replay_state() {
        let generic = serde_json::json!({
            "model": "claude-sonnet-4-6",
            "messages": [
                {"role": "assistant", "content": [{
                    "type": "compaction",
                    "content": "summary",
                    "encrypted_content": "opaque-provider-state"
                }]},
                {"role": "user", "content": "continue"}
            ],
            "context_management": {
                "edits": [{
                    "type": "compact_20260112",
                    "trigger": {"type": "input_tokens", "value": 50000},
                    "pause_after_compaction": true
                }]
            }
        })
        .to_string();

        let wrapped = prepare_claude_subscription_oauth_body(&generic, "credential-a")
            .expect("wrapped Claude compaction body");
        let value: serde_json::Value = serde_json::from_str(&wrapped).unwrap();

        assert_eq!(
            value.pointer("/context_management/edits/0/type"),
            Some(&serde_json::json!("compact_20260112"))
        );
        assert_eq!(
            value.pointer("/context_management/edits/0/pause_after_compaction"),
            Some(&serde_json::json!(true))
        );
        let replay = value["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|message| {
                message["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
            })
            .find(|block| block["type"] == "compaction")
            .expect("compaction replay block");
        assert_eq!(replay["content"], "summary");
        assert_eq!(replay["encrypted_content"], "opaque-provider-state");
    }

    #[test]
    fn claude_oauth_body_wrapper_respects_the_four_breakpoint_budget() {
        let generic = serde_json::json!({
            "model": "claude-sonnet-4-6",
            "system": "stable system",
            "tools": [
                {"name": "read_1", "input_schema": {"type": "object"}, "cache_control": {"type": "ephemeral"}},
                {"name": "read_2", "input_schema": {"type": "object"}, "cache_control": {"type": "ephemeral"}},
                {"name": "read_3", "input_schema": {"type": "object"}, "cache_control": {"type": "ephemeral"}},
                {"name": "read_4", "input_schema": {"type": "object"}, "cache_control": {"type": "ephemeral"}}
            ],
            "messages": [{"role": "user", "content": "latest"}]
        })
        .to_string();

        let wrapped = prepare_claude_subscription_oauth_body(&generic, "credential-a")
            .expect("wrapped Claude OAuth body");
        let value: serde_json::Value = serde_json::from_str(&wrapped).unwrap();

        assert_eq!(json_field_count(&value, "cache_control"), 4);
        assert!(value["system"][2].get("cache_control").is_none());
        assert_eq!(
            value["messages"][0]["content"][0]["text"],
            "[System Instructions]\nstable system"
        );
    }

    #[test]
    fn claude_oauth_body_wrapper_adds_one_cache_boundary_when_missing() {
        let generic = serde_json::json!({
            "model": "claude-sonnet-4-6",
            "messages": [{"role": "user", "content": "hello"}]
        })
        .to_string();

        let wrapped = prepare_claude_subscription_oauth_body(&generic, "credential-a")
            .expect("wrapped Claude OAuth body");
        let value: serde_json::Value = serde_json::from_str(&wrapped).unwrap();

        assert_eq!(json_field_count(&value, "cache_control"), 1);
        assert_eq!(value["system"][2]["cache_control"]["ttl"], "5m");

        let tool_only = serde_json::json!({
            "model": "claude-sonnet-4-6",
            "tools": [{
                "name": "read",
                "input_schema": {"type": "object"},
                "cache_control": {"type": "ephemeral"}
            }],
            "messages": [{"role": "user", "content": "hello"}]
        })
        .to_string();
        let tool_only = prepare_claude_subscription_oauth_body(&tool_only, "credential-a")
            .expect("wrapped tools-only Claude OAuth body");
        let tool_only: serde_json::Value = serde_json::from_str(&tool_only).unwrap();
        assert_eq!(json_field_count(&tool_only, "cache_control"), 2);
        assert_eq!(tool_only["system"][2]["cache_control"]["ttl"], "5m");
    }

    #[test]
    fn claude_cache_identity_is_stable_across_routes_without_changing_device_identity() {
        let request = |messages: serde_json::Value, cache_identity: &str| {
            prepare_claude_subscription_oauth_body_with_cache_identity(
                &serde_json::json!({
                    "model": "claude-sonnet-4-6",
                    "messages": messages,
                })
                .to_string(),
                "credential-a",
                Some(cache_identity),
            )
            .expect("Claude OAuth cache identity")
        };
        let first = request(
            serde_json::json!([{"role": "user", "content": "same first question"}]),
            "c1_route_a",
        );
        let grown = request(
            serde_json::json!([
                {"role": "user", "content": "same first question"},
                {"role": "assistant", "content": "answer"},
                {"role": "user", "content": "follow up"}
            ]),
            "c1_route_a",
        );
        let other = request(
            serde_json::json!([{"role": "user", "content": "same first question"}]),
            "c1_route_b",
        );
        let metadata = |body: &str| {
            let value: serde_json::Value = serde_json::from_str(body).unwrap();
            serde_json::from_str::<serde_json::Value>(
                value["metadata"]["user_id"].as_str().unwrap(),
            )
            .unwrap()
        };
        let first = metadata(&first);
        let grown = metadata(&grown);
        let other = metadata(&other);

        assert_eq!(first["session_id"], grown["session_id"]);
        assert_ne!(first["session_id"], other["session_id"]);
        assert_eq!(first["device_id"], grown["device_id"]);
        assert_eq!(first["device_id"], other["device_id"]);
    }

    #[test]
    fn claude_oauth_body_wrapper_preserves_native_billing_block_in_any_position() {
        let native_body = r#"{
  "model": "claude-sonnet-4-6",
  "system": [
    {"type": "text", "text": "session classifier context"},
    {"type": "text", "text": "x-anthropic-billing-header: cc_version=2.1.220.abc; cc_entrypoint=cli;"},
    {"type": "text", "text": "native instructions"}
  ],
  "messages": [{"role": "user", "content": "review"}],
  "metadata": {"user_id": "native-metadata"}
}"#;
        let native: serde_json::Value = serde_json::from_str(native_body).unwrap();

        let wrapped =
            prepare_claude_subscription_oauth_body(native_body, "C:/credentials/claude.json")
                .expect("native Claude body");

        assert_eq!(wrapped, native_body);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&wrapped).unwrap(),
            native
        );

        let adjusted = ensure_claude_messages_body_with_faults(native_body, "claude-sonnet-4-6")
            .expect("native Claude adjustment");
        assert_eq!(adjusted.body, native_body);
        assert!(adjusted.faults.is_empty());
    }

    #[test]
    fn claude_subscription_does_not_invent_sampling_defaults() {
        let adjusted = ensure_claude_messages_body_with_faults(
            r#"{"model":"claude-opus-5","messages":[{"role":"user","content":"hello"}]}"#,
            "claude-opus-5",
        )
        .expect("non-native Claude subscription body");
        let body: serde_json::Value = serde_json::from_str(&adjusted.body).unwrap();
        assert!(body.get("temperature").is_none());
        assert_eq!(body["max_tokens"], CLAUDE_CODE_OAUTH_FALLBACK_MAX_TOKENS);
    }

    #[test]
    fn grok_request_uses_the_official_cli_identity() {
        let request = grok_subscription_response_request(
            &Client::new(),
            "https://cli-chat-proxy.grok.com/v1/responses",
            "token",
            r#"{"model":"grok-4.5","input":"hello","stream":true}"#.to_string(),
        )
        .expect("valid Grok request")
        .build()
        .expect("Grok Responses request");

        assert_eq!(
            request.headers().get("authorization").unwrap(),
            "Bearer token"
        );
        assert_eq!(
            request.headers().get("x-xai-token-auth").unwrap(),
            "xai-grok-cli"
        );
        assert_eq!(
            request.headers().get("x-grok-client-version").unwrap(),
            GROK_CLI_VERSION
        );
        assert_eq!(
            request.headers().get("x-grok-client-mode").unwrap(),
            "interactive"
        );
        assert_eq!(
            request.headers().get("x-grok-model-override").unwrap(),
            "grok-4.5"
        );
        assert_eq!(
            request.headers().get("user-agent").unwrap(),
            GROK_CLI_USER_AGENT
        );
    }

    #[test]
    fn grok_request_rejects_a_missing_model_before_send() {
        let error = grok_subscription_response_request(
            &Client::new(),
            "https://cli-chat-proxy.grok.com/v1/responses",
            "token",
            r#"{"input":"hello","stream":true}"#.to_string(),
        )
        .err()
        .expect("missing model must fail");

        assert!(error.to_string().contains("missing a model"));
    }

    #[test]
    fn grok_ping_filter_rewrites_fragmented_vendor_pings_and_preserves_responses_events() {
        let mut filter = GrokResponsesPingFilter::default();
        let mut output = Vec::new();
        let mut has_payload = false;
        for chunk in [
            "event: pi",
            "ng\r\ndata: {\"type\":\"ping\",\"cost\":\"1.25\"}\r\n\r",
            "\nevent: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n",
        ] {
            let (filtered, payload) = filter.push(chunk.as_bytes());
            output.extend_from_slice(&filtered);
            has_payload |= payload;
        }
        let (tail, payload) = filter.finish();
        output.extend_from_slice(&tail);
        has_payload |= payload;
        let output = String::from_utf8(output).unwrap();

        assert!(output.starts_with(": ping\n\n"), "{output}");
        assert!(!output.contains("event: ping"), "{output}");
        assert!(output.contains("event: response.completed\n"), "{output}");
        assert!(has_payload);
    }

    #[test]
    fn grok_ping_filter_preserves_conflicting_event_payloads() {
        let raw = concat!(
            "event: ping\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"keep\"}\n\n"
        );
        assert_eq!(filter_grok_responses_sse(raw), raw);
    }

    #[test]
    fn grok_ping_filter_replays_oversized_candidates_instead_of_buffering_them() {
        let oversized = "x".repeat(GROK_RESPONSES_PING_FRAME_MAX_BYTES);
        let raw = format!("event: ping\ndata: {oversized}\n\n");
        assert_eq!(filter_grok_responses_sse(&raw), raw);
    }

    #[test]
    fn claude_session_identity_accepts_strict_legacy_and_rejects_ambiguous_user_ids() {
        let session_id = "123e4567-e89b-12d3-a456-426614174000";
        let legacy = format!(
            "user_{}_account__session_{session_id}",
            "a1b2c3d4e5f6".repeat(5) + "a1b2"
        );
        let legacy_body = serde_json::json!({"metadata": {"user_id": legacy}}).to_string();
        assert_eq!(
            claude_code_session_id_from_body(&legacy_body).as_deref(),
            Some(session_id)
        );

        for user_id in [
            "ordinary-application-user",
            "session_123e4567-e89b-12d3-a456-426614174000",
            r#"{"session_id":"123e4567-e89b-12d3-a456-426614174000"}"#,
            r#"{"device_id":"device","session_id":"bad
header"}"#,
        ] {
            let body = serde_json::json!({"metadata": {"user_id": user_id}}).to_string();
            let request = claude_subscription_request(
                &Client::new(),
                "https://api.anthropic.test/v1/messages",
                "token",
                body,
            )
            .build()
            .expect("Claude request without ambiguous session header");
            assert!(
                !request.headers().contains_key("x-claude-code-session-id"),
                "{user_id}"
            );
        }
    }

    #[test]
    fn native_openai_responses_sse_extracts_the_original_completed_response() {
        let raw = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"x_future\":{\"keep\":true}}}\n\n"
        );

        let body = openai_responses_completed_body_from_sse(raw).expect("completed response");
        let value: serde_json::Value = serde_json::from_str(&body).expect("response json");
        assert_eq!(value["id"], "resp_1");
        assert_eq!(value["x_future"]["keep"], true);
        assert_eq!(value["output"][0]["type"], "message");
        assert_eq!(value["output"][0]["content"][0]["text"], "hello");
    }

    #[test]
    fn native_openai_responses_sse_restores_completed_items_when_terminal_output_is_empty() {
        let raw = concat!(
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"rs_1\",\"type\":\"reasoning\",\"encrypted_content\":\"opaque\",\"summary\":[]}}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":1,\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"status\":\"completed\",\"call_id\":\"call_1\",\"name\":\"lookup_weather\",\"arguments\":\"{\\\"city\\\":\\\"Shanghai\\\"}\"}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_2\",\"status\":\"completed\",\"output\":[],\"x_future\":{\"keep\":true}}}\n\n"
        );

        let body = openai_responses_completed_body_from_sse(raw).expect("completed response");
        let value: serde_json::Value = serde_json::from_str(&body).expect("response json");
        assert_eq!(value["x_future"]["keep"], true);
        assert_eq!(value["output"][0]["type"], "reasoning");
        assert_eq!(value["output"][1]["type"], "function_call");
        assert_eq!(value["output"][1]["name"], "lookup_weather");
    }

    #[test]
    fn native_openai_responses_sse_hydrates_only_missing_terminal_item_ids() {
        let raw = concat!(
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"fc_from_stream\",\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"weather\",\"arguments\":\"{}\"}}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":1,\"item\":{\"id\":\"fc_stream_existing\",\"type\":\"function_call\",\"call_id\":\"call_2\",\"name\":\"other\",\"arguments\":\"{}\"}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_3\",\"status\":\"completed\",\"output\":[{\"id\":null,\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"terminal-weather\",\"arguments\":\"{}\"},{\"id\":\"fc_terminal_existing\",\"type\":\"function_call\",\"call_id\":\"call_2\",\"name\":\"terminal-other\",\"arguments\":\"{}\"}]}}\n\n"
        );

        let body = openai_responses_completed_body_from_sse(raw).expect("completed response");
        let value: serde_json::Value = serde_json::from_str(&body).expect("response json");
        assert_eq!(value["output"][0]["id"], "fc_from_stream");
        assert_eq!(value["output"][0]["name"], "terminal-weather");
        assert_eq!(value["output"][1]["id"], "fc_terminal_existing");
        assert_eq!(value["output"][1]["name"], "terminal-other");
    }

    #[test]
    fn native_openai_responses_sse_requires_a_completed_response_body() {
        let error = openai_responses_completed_body_from_sse(
            "event: response.completed\ndata: {\"type\":\"response.completed\"}\n\n",
        )
        .expect_err("missing response payload");
        assert!(error.to_string().contains("response.completed.response"));
    }

    #[test]
    fn compact_operation_derives_the_non_stream_endpoint_without_duplication() {
        assert!(is_openai_responses_compact_path(
            "/v1/responses/compact?beta=true"
        ));
        assert_eq!(
            openai_subscription_operation_url(
                "https://chatgpt.com/backend-api/codex/responses".to_string(),
                true,
            ),
            "https://chatgpt.com/backend-api/codex/responses/compact"
        );
        assert_eq!(
            openai_subscription_operation_url(
                "https://chatgpt.com/backend-api/codex/responses/compact".to_string(),
                true,
            ),
            "https://chatgpt.com/backend-api/codex/responses/compact"
        );
        assert_eq!(
            claude_subscription_operation_url(
                "https://api.anthropic.com/v1/messages?beta=true".to_string(),
                true,
            ),
            "https://api.anthropic.com/v1/messages/count_tokens?beta=true"
        );
        assert_eq!(
            antigravity_subscription_operation_url(
                "https://cloudcode-pa.googleapis.com/v1internal:generateContent".to_string(),
                true,
            ),
            "https://cloudcode-pa.googleapis.com/v1internal:countTokens"
        );
    }

    #[test]
    fn native_gemini_response_unwrap_preserves_unknown_nested_fields() {
        let raw = serde_json::json!({
            "response": {
                "candidates": [{
                    "content": {"parts": [{
                        "text": "hello",
                        "futurePart": {"depth": 2}
                    }]}
                }],
                "futureTop": {"keep": true}
            },
            "transportMetadata": {"requestId": "transport-only"}
        })
        .to_string();

        let native = antigravity_body_to_native_body(&raw).expect("native response");
        let value: serde_json::Value = serde_json::from_str(&native).expect("response json");
        assert_eq!(
            value.pointer("/candidates/0/content/parts/0/futurePart/depth"),
            Some(&serde_json::json!(2))
        );
        assert_eq!(value["futureTop"]["keep"], true);
        assert!(value.get("transportMetadata").is_none());
    }
}

#[cfg(any(test, debug_assertions))]
fn codex_responses_url_override() -> Option<String> {
    #[cfg(test)]
    if let Ok(override_url) = CODEX_RESPONSES_URL_TEST_OVERRIDE
        .get_or_init(|| StdMutex::new(None))
        .lock()
    {
        if override_url.is_some() {
            return override_url.clone();
        }
    }
    std::env::var("CONST_API_CODEX_RESPONSES_URL").ok()
}

#[cfg(test)]
static CODEX_RESPONSES_URL_TEST_OVERRIDE: OnceLock<StdMutex<Option<String>>> = OnceLock::new();

#[cfg(test)]
pub(crate) fn set_codex_responses_url_override_for_tests(override_url: Option<String>) {
    *CODEX_RESPONSES_URL_TEST_OVERRIDE
        .get_or_init(|| StdMutex::new(None))
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = override_url;
}

#[cfg(not(any(test, debug_assertions)))]
fn codex_responses_url_override() -> Option<String> {
    None
}
