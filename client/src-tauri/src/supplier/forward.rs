struct ResolvedSupplierWireRoute {
    method: reqwest::Method,
    canonical_path: String,
    request_target: String,
    route: crate::surface::ApiRoute,
}

fn resolve_supplier_wire_route(
    wire: &crate::surface_wire::SurfaceEnvelope,
) -> Result<ResolvedSupplierWireRoute> {
    let method = if wire.method.trim().is_empty() {
        reqwest::Method::POST
    } else {
        reqwest::Method::from_bytes(wire.method.as_bytes())?
    };
    let canonical_path = match wire.canonical_path() {
        path if path.is_empty() => "/v1/chat/completions".to_string(),
        path => path,
    };
    let route = crate::surface::resolve_api_route(method.as_str(), &canonical_path, false)
        .ok_or_else(|| anyhow!("request path is outside the configured API surfaces"))?;

    if let Some(declared) = wire.surface {
        if declared != route.surface {
            return Err(anyhow!(
                "declared surface {} does not match resolved {} surface for {} {}",
                declared.as_str(),
                route.surface.as_str(),
                method.as_str(),
                canonical_path
            ));
        }
    }
    if let Some(declared) = wire.operation {
        if declared != route.operation {
            return Err(anyhow!(
                "declared operation {} does not match resolved {} operation for {} {}",
                declared.as_str(),
                route.operation.as_str(),
                method.as_str(),
                canonical_path
            ));
        }
    }
    if let Some(declared) = wire.protocol.as_deref() {
        let resolved = route.protocol.map(|protocol| protocol.as_str());
        if Some(declared) != resolved {
            return Err(anyhow!(
                "declared protocol {} does not match resolved {} protocol for {} {}",
                declared,
                resolved.unwrap_or("none"),
                method.as_str(),
                canonical_path
            ));
        }
    }

    let mut request_target = canonical_path.clone();
    if wire.has_query || !wire.raw_query.is_empty() {
        request_target.push('?');
        request_target.push_str(&wire.raw_query);
    }
    Ok(ResolvedSupplierWireRoute {
        method,
        canonical_path,
        request_target,
        route,
    })
}

fn channel_http_response_payload(
    status: u16,
    headers: &reqwest::header::HeaderMap,
    body: &[u8],
    faults: &[ImprovementFault],
    upstream_usage: Option<&SupplierUpstreamUsage>,
    diagnostics: Option<&crate::openrouter::ResponseDiagnostics>,
    upstream_model: Option<&str>,
    original_error_body: Option<&[u8]>,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    let mut payload = crate::surface_wire::http_response_payload(status, headers, body)?;
    attach_improvement_faults(&mut payload, faults);
    attach_upstream_usage(&mut payload, upstream_usage);
    attach_channel_failure_metadata(&mut payload, status, headers, body);
    let raw = original_error_body.unwrap_or(body);
    if let Ok(raw) = std::str::from_utf8(raw) {
        let borrowed = headers.iter().filter_map(|(name, value)| value.to_str().ok().map(|v| (name.as_str(), v))).collect::<Vec<_>>();
        if let Some(failure) = crate::upstream_failure::observe(status, raw, &borrowed, upstream_model) {
            if let Some(delay) = failure.retry_after_seconds {
                payload.insert("retry_after_seconds".into(), delay.into());
                payload.insert("retry_hint_source".into(), failure.retry_source.clone().into());
            }
            insert_subscription_route_model_failure_evidence(&mut payload, status, raw, upstream_model);
        }
    }
    if let Some(diagnostics) = diagnostics { diagnostics.attach(&mut payload); }
    Ok(payload)
}

fn safe_channel_failure_identifier(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 192
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
        })
    {
        return None;
    }
    Some(value.to_string())
}

fn channel_failure_json_identifier(
    value: &serde_json::Value,
    keys: &[&str],
) -> Option<String> {
    let nested = value.get("error").and_then(serde_json::Value::as_object);
    for object in nested.into_iter().chain(value.as_object()) {
        for key in keys {
            if let Some(identifier) = object
                .get(*key)
                .and_then(serde_json::Value::as_str)
                .and_then(safe_channel_failure_identifier)
            {
                return Some(identifier);
            }
        }
    }
    None
}

fn channel_failure_header_identifier(
    headers: &reqwest::header::HeaderMap,
) -> Option<String> {
    [
        "x-request-id",
        "request-id",
        "x-goog-request-id",
        "x-amzn-requestid",
        "anthropic-request-id",
        "cf-ray",
    ]
        .into_iter()
        .find_map(|name| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .and_then(safe_channel_failure_identifier)
        })
}

fn channel_failure_body_retry_after_seconds(value: &serde_json::Value) -> Option<i64> {
    let nested = value.get("error").and_then(serde_json::Value::as_object);
    nested
        .into_iter()
        .chain(value.as_object())
        .find_map(|object| {
            ["retry_after_seconds", "retry_after"]
                .into_iter()
                .find_map(|key| {
                    let value = object.get(key)?;
                    let seconds = value
                        .as_f64()
                        .or_else(|| value.as_str()?.trim().parse::<f64>().ok())?;
                    seconds.is_finite().then(|| seconds.ceil() as i64)
                })
        })
        .filter(|seconds| *seconds > 0)
}

fn channel_failure_is_explicitly_retryable(error_type: &str, error_code: &str) -> bool {
    [error_type, error_code].into_iter().any(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "upstream_unavailable"
                | "server_is_overloaded"
                | "server_overloaded"
                | "overloaded_error"
                | "service_unavailable"
                | "temporarily_unavailable"
                | "rate_limit_error"
                | "rate_limited"
                | "rate_limit_exceeded"
                | "resource_exhausted"
                | "unavailable"
        )
    })
}

fn attach_channel_failure_metadata(
    payload: &mut serde_json::Map<String, serde_json::Value>,
    status: u16,
    headers: &reqwest::header::HeaderMap,
    body: &[u8],
) {
    if (200..300).contains(&status) {
        return;
    }
    let parsed = serde_json::from_slice::<serde_json::Value>(body).ok();
    let error_type = parsed
        .as_ref()
        .and_then(|value| channel_failure_json_identifier(value, &["type"]));
    let error_code = parsed
        .as_ref()
        .and_then(|value| channel_failure_json_identifier(value, &["code", "status"]));
    let provider_request_id = channel_failure_header_identifier(headers).or_else(|| {
        parsed.as_ref().and_then(|value| {
            channel_failure_json_identifier(
                value,
                &["provider_request_id", "request_id", "requestId"],
            )
        })
    });
    if let Some(value) = error_type.as_deref() {
        payload.insert("error_type".to_string(), value.into());
    }
    if let Some(value) = error_code.as_deref() {
        payload.insert("error_code".to_string(), value.into());
    }
    if let Some(value) = provider_request_id.as_deref() {
        payload.insert("provider_request_id".to_string(), value.into());
    }
    let borrowed_headers = headers
        .iter()
        .filter_map(|(name, value)| value.to_str().ok().map(|value| (name.as_str(), value)))
        .collect::<Vec<_>>();
    if let Some(retry_after) = crate::supplier::parse_subscription_retry_after(&borrowed_headers)
        .or_else(|| {
            parsed
                .as_ref()
                .and_then(channel_failure_body_retry_after_seconds)
        })
        .filter(|value| *value > 0)
    {
        payload.insert("retry_after_seconds".to_string(), retry_after.into());
    }
    let error_kind = error_code.as_deref().or(error_type.as_deref());
    if let Some(value) = error_kind {
        payload
            .entry("error_kind".to_string())
            .or_insert_with(|| value.into());
    }
    if channel_failure_is_explicitly_retryable(
        error_type.as_deref().unwrap_or_default(),
        error_code.as_deref().unwrap_or_default(),
    ) {
        // This response is fully buffered and no client-visible output has
        // started. Keep the evidence request-local so a transient provider
        // overload does not evict an otherwise healthy custom channel.
        payload.insert("failure_scope".to_string(), "request".into());
        payload.insert("safe_to_retry_other_channel".to_string(), true.into());
        payload.insert("safe_to_retry_same_channel".to_string(), false.into());
    }
}

fn channel_stream_start_payload(
    status: u16,
    headers: &reqwest::header::HeaderMap,
    faults: &[ImprovementFault],
) -> Result<serde_json::Map<String, serde_json::Value>> {
    let mut payload = crate::surface_wire::stream_start_payload(status, headers)?;
    attach_improvement_faults(&mut payload, faults);
    Ok(payload)
}

async fn send_supplier_execution_failure(
    outbound: &SupplierOutbound,
    request_id: &str,
    error: &anyhow::Error,
) -> Result<()> {
    send_supplier_message(
        outbound,
        SupplierMessage {
            id: request_id.to_string(),
            kind: "error".to_string(),
            payload: supplier_execution_error_payload(error),
        },
    )
    .await
}

// Shared by local short-circuit and platform forwarding. Preserve context intent
// before replacing the public model with the provider's exact wire identity.
pub(crate) fn rewrite_retained_subscription_body_model(
    provider: &str,
    path: &str,
    body: &str,
    selected_model: Option<&str>,
) -> Option<String> {
    let mut selected_model = selected_model?.to_string();
    if provider == "claude" && requested_model_from_body_or_path(body, path)
        .is_some_and(|model| crate::config::without_context_hint(&model) != model.trim()) {
        selected_model = format!("{}[1m]", crate::config::without_context_hint(&selected_model));
    }
    Some(rewrite_supplier_body_model(body, &selected_model))
}

pub(crate) async fn forward_supplier_request(
    client: &Client,
    config: &SupplierConfig,
    msg: &SupplierMessage,
    stream_requested: bool,
    outbound: &SupplierOutbound,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>> {
    let wire =
        crate::surface_wire::SurfaceEnvelope::from_payload(&msg.payload).map_err(|error| {
            platform_execution_failure("request_surface_decode", "invalid_surface_wire", error)
        })?;
    let resolved = resolve_supplier_wire_route(&wire).map_err(|error| {
        platform_execution_failure("request_surface_validate", "invalid_surface_route", error)
    })?;
    let request_capacity = SubscriptionRequestCapacity::from_supplier_payload(
        &msg.payload,
        resolved.route.operation,
    );
    let target_upstream_model = wire
        .selected_upstream_model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let selected_target_protocol = wire
        .selected_target_protocol
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let channel = channel_from_supplier(config.channel_id.clone(), config);
    if matches!(resolved.route.operation.as_str(), "openai.chat_completions" | "openai.responses" | "anthropic.messages" | "gemini.generate_content" | "gemini.stream_generate_content") {
        availability::note_activity(&channel);
    }
    if let Some(model) = target_upstream_model {
        if !availability::allows(&channel, model) {
            return Err(execution_failure("local_admission", "platform", "channel_group_paused",
                false, false, false, false,
                recommendation("deny", "deny", "none", "ineligible", "other_channel"),
                "channel representative check paused this model group; refresh route selection"));
        }
    }
    if let crate::channel_executor::ChannelExecutionBackend::RetainedSubscription { provider } =
        crate::channel_executor::channel_execution_backend(&channel, resolved.route.operation)?
    {
        let retained_target = match provider {
            "openai" | "codex" => "openai_responses",
            "grok" => "openai_responses",
            "antigravity" => "gemini_native",
            "claude" => "anthropic_messages",
            _ => "",
        };
        if selected_target_protocol.is_some_and(|selected| selected != retained_target) {
            return Err(platform_execution_failure(
                "request_surface_validate",
                "selected_target_protocol_mismatch",
                anyhow!(
                    "selected target protocol is incompatible with retained {provider} execution"
                ),
            ));
        }
        let original_body = wire.body.utf8().map_err(|error| {
            platform_execution_failure("request_surface_decode", "invalid_surface_body", error)
        })?;
        let request_headers = wire.header_map().map_err(|error| {
            platform_execution_failure(
                "request_surface_decode",
                "invalid_surface_headers",
                error,
            )
        })?;
        let routed_body = rewrite_retained_subscription_body_model(
            provider, &resolved.request_target, original_body, target_upstream_model,
        );
        let body = routed_body.as_deref().unwrap_or(original_body);
        let provider = provider.to_string();
        let upstream_override = retained_subscription_upstream_override(&provider);
        let force_buffered =
            subscription_operation_requires_buffered(&provider, &resolved.request_target);
        crate::channel_executor::admit_retained_subscription_operation(
            &channel,
            &resolved.route,
            stream_requested && !force_buffered,
        )?;
        if stream_requested && !force_buffered {
            if provider == "openai" || provider == "codex" || provider == "grok" {
                return forward_openai_subscription_stream_request_with_capacity(
                    client,
                    config,
                    msg.id.as_str(),
                    &resolved.request_target,
                    body,
                    outbound,
                    upstream_override.as_deref(),
                    Some(request_capacity),
                    wire.cache_identity.as_deref(),
                    Some(&request_headers),
                )
                .await;
            }
            if provider == "antigravity" {
                return forward_antigravity_subscription_stream_request_with_capacity(
                    client,
                    config,
                    msg.id.as_str(),
                    &resolved.request_target,
                    body,
                    outbound,
                    upstream_override.as_deref(),
                    Some(request_capacity),
                    wire.cache_identity.as_deref(),
                )
                .await;
            }
            if provider == "claude" {
                return forward_claude_subscription_stream_request_with_capacity(
                    client,
                    config,
                    msg.id.as_str(),
                    &resolved.request_target,
                    body,
                    outbound,
                    upstream_override.as_deref(),
                    Some(request_capacity),
                    wire.cache_identity.as_deref(),
                    Some(&request_headers),
                )
                .await;
            }
        }
        return Ok(Some(
            forward_subscription_supplier_request_with_capacity(
                client,
                config,
                &resolved.request_target,
                body,
                stream_requested,
                upstream_override.as_deref(),
                Some(request_capacity),
                wire.cache_identity.as_deref(),
                Some(&request_headers),
            )
            .await,
        ));
    }
    let body = bytes::Bytes::from(wire.body.decode().map_err(|error| {
        platform_execution_failure("request_surface_decode", "invalid_surface_body", error)
    })?);
    let execution = crate::channel_executor::execute_channel_request(
        client,
        &channel,
        crate::channel_executor::ChannelRequest {
            method: resolved.method,
            path: resolved.canonical_path,
            raw_query: wire.raw_query.clone(),
            has_query: wire.has_query,
            headers: wire.header_map().map_err(|error| {
                platform_execution_failure(
                    "request_surface_decode",
                    "invalid_surface_headers",
                    error,
                )
            })?,
            body,
            stream_requested: Some(stream_requested),
            selected_upstream_model: target_upstream_model.map(str::to_string),
            selected_target_protocol: selected_target_protocol.map(str::to_string),
            cache_identity: wire.cache_identity.clone(),
            safety_identifier: msg
                .payload
                .get("_const_safety_identifier")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        },
    )
    .await?;
    let diagnostics = crate::openrouter::response_diagnostics(&execution.response);
    let upstream_model = execution.upstream_model;
    let target_protocol = execution.target_protocol;
    let inbound_protocol = execution
        .inbound_protocol
        .unwrap_or_else(|| target_protocol.clone());
    let opaque = execution.opaque;
    let native_passthrough = execution.native_passthrough || opaque;
    let mut improvement_faults = execution.improvement_faults;
    let tool_mapping = execution.tool_mapping;
    let resp = execution.response;
    let status = resp.status().as_u16();
    let response_headers = resp.headers().clone();
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let is_event_stream = content_type
        .as_deref()
        .is_some_and(|value| value.contains("event-stream"));
    if !(200..300).contains(&status) {
        let body = resp.bytes().await?;
        let upstream_usage = upstream_usage_from_body(&target_protocol, &body);
        let cross_protocol_json_error = !native_passthrough
            && inbound_protocol != target_protocol
            && !is_event_stream
            && content_type
                .as_deref()
                .is_some_and(|value| value.to_ascii_lowercase().contains("json"));
        if cross_protocol_json_error {
            if let Ok(raw) = std::str::from_utf8(&body) {
                match response_body_from_target_as_inbound_with_report_with_tools(
                    raw,
                    &inbound_protocol,
                    &target_protocol,
                    &upstream_model,
                    &tool_mapping,
                ) {
                    Ok(converted) => {
                        improvement_faults.extend(converted.faults);
                        let mut headers = response_headers.clone();
                        headers.remove(reqwest::header::CONTENT_LENGTH);
                        headers.remove(reqwest::header::CONTENT_ENCODING);
                        headers.insert(
                            reqwest::header::CONTENT_TYPE,
                            reqwest::header::HeaderValue::from_static("application/json"),
                        );
                        return Ok(Some(channel_http_response_payload(
                            status,
                            &headers,
                            converted.body.as_bytes(),
                            &improvement_faults,
                            upstream_usage.as_ref(),
                            diagnostics.as_ref(),
                            Some(&upstream_model),
                            Some(&body),
                        )?));
                    }
                    Err(error) => {
                        // Error conversion must never hide the provider's real failure. Preserve
                        // the original body and make the missing mapping observable instead.
                        eprintln!(
                            "[const-api][protocol] cross-protocol error response kept in upstream shape source={} target={} error={error}",
                            target_protocol, inbound_protocol
                        );
                        improvement_faults.push(ImprovementFault::conversion_warning(
                            "error_response_passthrough",
                            &target_protocol,
                            &inbound_protocol,
                            &upstream_model,
                            "$.error",
                            "The upstream error envelope had no safe target-protocol mapping; the original error body was preserved",
                        ));
                    }
                }
            }
        }
        return Ok(Some(channel_http_response_payload(
            status,
            &response_headers,
            &body,
            &improvement_faults,
            upstream_usage.as_ref(),
            diagnostics.as_ref(),
            Some(&upstream_model),
            None,
        )?));
    }
    if native_passthrough && stream_requested && is_event_stream {
        let mut stream = SupplierByteStreamBatcher::new(resp.bytes_stream());
        let mut upstream_usage = SupplierUpstreamUsageAccumulator::new(&target_protocol);
        let mut started = false;
        loop {
            let chunk = match stream.next().await? {
                SupplierStreamRead::Chunk(batch) => batch.bytes,
                SupplierStreamRead::End => {
                    if !started {
                        send_supplier_message(
                            outbound,
                            SupplierMessage {
                                id: msg.id.clone(),
                                kind: "stream_start".to_string(),
                                payload: channel_stream_start_payload(
                                    status,
                                    &response_headers,
                                    &improvement_faults,
                                )?,
                            },
                        )
                        .await?;
                    }
                    break;
                }
                SupplierStreamRead::InactivityTimeout => {
                    return Err(anyhow!("upstream stream inactivity timeout"));
                }
            };
            upstream_usage.push(&chunk);
            if !started {
                send_supplier_message(
                    outbound,
                    SupplierMessage {
                        id: msg.id.clone(),
                        kind: "stream_start".to_string(),
                        payload: channel_stream_start_payload(
                            status,
                            &response_headers,
                            &improvement_faults,
                        )?,
                    },
                )
                .await?;
                started = true;
            }
            let message = SupplierMessage {
                id: msg.id.clone(),
                kind: "stream_chunk".to_string(),
                payload: crate::surface_wire::stream_chunk_payload(chunk.as_slice())?,
            };
            send_supplier_message(outbound, message).await?;
        }
        send_supplier_message(
            outbound,
            SupplierMessage {
                id: msg.id.clone(),
                kind: "stream_end".to_string(),
                payload: {
                    let mut payload = serde_json::Map::new();
                    let (usage, failure) = upstream_usage.finish_observed(Some(&upstream_model));
                    attach_upstream_usage(&mut payload, usage.as_ref());
                    if let Some(failure) = failure { failure.attach(&mut payload); }
                    if let Some(diagnostics) = &diagnostics { diagnostics.attach(&mut payload); }
                    payload
                },
            },
        )
        .await?;
        return Ok(None);
    }
    if native_passthrough && !is_event_stream {
        let body = resp.bytes().await?;
        let upstream_usage = upstream_usage_from_body(&target_protocol, &body);
        return Ok(Some(channel_http_response_payload(
            status,
            &response_headers,
            &body,
            &improvement_faults,
            upstream_usage.as_ref(),
            diagnostics.as_ref(),
            Some(&upstream_model),
            None,
        )?));
    }
    if !stream_requested && is_event_stream {
        let raw = crate::sse_buffer::read_sse_body_with_limits(resp).await?;
        let mut upstream_usage = SupplierUpstreamUsageAccumulator::new(&target_protocol);
        upstream_usage.push(raw.as_bytes());
        let upstream_usage = upstream_usage.finish();
        let converted = sse_body_from_target_as_inbound_with_report_with_tools(
            &raw,
            &inbound_protocol,
            &target_protocol,
            &upstream_model,
            &tool_mapping,
        )
        .map_err(|error| response_conversion_failure(false, false, error))?;
        improvement_faults.extend(converted.faults);
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        return Ok(Some(channel_http_response_payload(
            status,
            &headers,
            converted.body.as_bytes(),
            &improvement_faults,
            upstream_usage.as_ref(),
            diagnostics.as_ref(),
            Some(&upstream_model),
            None,
        )?));
    }
    if stream_requested && !is_event_stream {
        let raw = resp.text().await?;
        let upstream_usage = upstream_usage_from_body(&target_protocol, raw.as_bytes());
        let converted = sse_body_from_target_body_as_inbound_with_report_with_tools(
            &raw,
            &inbound_protocol,
            &target_protocol,
            &upstream_model,
            &tool_mapping,
        )
        .map_err(|error| response_conversion_failure(false, false, error))?;
        improvement_faults.extend(converted.faults);
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("text/event-stream"),
        );
        let start = SupplierMessage {
            id: msg.id.clone(),
            kind: "stream_start".to_string(),
            payload: channel_stream_start_payload(status, &headers, &improvement_faults)?,
        };
        send_supplier_message(outbound, start).await?;
        let chunk = SupplierMessage {
            id: msg.id.clone(),
            kind: "stream_chunk".to_string(),
            payload: crate::surface_wire::stream_chunk_payload(converted.body.as_bytes())?,
        };
        send_supplier_message(outbound, chunk).await?;
        let end = SupplierMessage {
            id: msg.id.clone(),
            kind: "stream_end".to_string(),
            payload: {
                let mut payload = serde_json::Map::new();
                attach_upstream_usage(&mut payload, upstream_usage.as_ref());
                if let Some(failure) = crate::upstream_failure::observe(status, &raw, &[], Some(&upstream_model)) { failure.attach(&mut payload); }
                if let Some(diagnostics) = &diagnostics { diagnostics.attach(&mut payload); }
                payload
            },
        };
        send_supplier_message(outbound, end).await?;
        return Ok(None);
    }
    if stream_requested {
        let mut stream_headers = reqwest::header::HeaderMap::new();
        stream_headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("text/event-stream"),
        );
        let mut stream = SupplierByteStreamBatcher::new(resp.bytes_stream());
        let mut converter =
            inbound_sse_stream_converter(&inbound_protocol, &target_protocol, &upstream_model)?.with_tool_mapping(tool_mapping);
        let mut upstream_usage = SupplierUpstreamUsageAccumulator::new(&target_protocol);
        let mut utf8 = crate::protocol::stream::Utf8ChunkDecoder::new();
        let mut started = false;
        loop {
            let chunk = match stream.next().await? {
                SupplierStreamRead::Chunk(batch) => batch.bytes,
                SupplierStreamRead::End => break,
                SupplierStreamRead::InactivityTimeout => {
                    let usable_output_produced = started;
                    if !started {
                        return Err(upstream_stream_failure(
                            false,
                            false,
                            "upstream stream inactivity timeout",
                        ));
                    }
                    let converted = converter.fail(
                        "stream_inactivity_timeout",
                        "upstream stream produced no event before the inactivity deadline",
                    );
                    let text = utf8.push(&converted)?;
                    if !text.is_empty() {
                        let message = SupplierMessage {
                            id: msg.id.clone(),
                            kind: "stream_chunk".to_string(),
                            payload: crate::surface_wire::stream_chunk_payload(text.as_bytes())?,
                        };
                        send_supplier_message(outbound, message).await?;
                    }
                    let error = upstream_stream_failure(
                        true,
                        usable_output_produced,
                        "upstream stream inactivity timeout",
                    );
                    send_supplier_execution_failure(outbound, &msg.id, &error).await?;
                    return Ok(None);
                }
            };
            upstream_usage.push(&chunk);
            let usable_output_before_chunk = started;
            let converted = converter.push(&chunk);
            let conversion_failure = converter.failure();
            if let Some(failure) = conversion_failure.as_deref() {
                if !started {
                    return Err(response_conversion_failure(false, false, failure));
                }
            }
            if converted.is_empty() {
                if let Some(failure) = conversion_failure.as_deref() {
                    let error = response_conversion_failure(true, true, failure);
                    send_supplier_execution_failure(outbound, &msg.id, &error).await?;
                    return Ok(None);
                }
                continue;
            }
            let text = utf8.push(&converted)?;
            if text.is_empty() {
                continue;
            }
            if !started {
                send_supplier_message(
                    outbound,
                    SupplierMessage {
                        id: msg.id.clone(),
                        kind: "stream_start".to_string(),
                        payload: channel_stream_start_payload(
                            status,
                            &stream_headers,
                            &improvement_faults,
                        )?,
                    },
                )
                .await?;
                started = true;
            }
            let request_id = msg.id.clone();
            let stream_msg = SupplierMessage {
                id: msg.id.clone(),
                kind: "stream_chunk".to_string(),
                payload: crate::surface_wire::stream_chunk_payload(text.as_bytes())?,
            };
            send_supplier_message(outbound, stream_msg).await?;
            if let Some(failure) = conversion_failure.as_deref() {
                let error = response_conversion_failure(true, usable_output_before_chunk, failure);
                send_supplier_execution_failure(outbound, &request_id, &error).await?;
                return Ok(None);
            }
        }
        let tail = converter.finish();
        let finish_failure = converter.failure();
        if let Some(failure) = finish_failure.as_deref() {
            if !started {
                return Err(response_conversion_failure(false, false, failure));
            }
        }
        let mut tail = utf8.push(&tail)?;
        tail.push_str(&utf8.finish()?);
        if !tail.is_empty() {
            if !started {
                send_supplier_message(
                    outbound,
                    SupplierMessage {
                        id: msg.id.clone(),
                        kind: "stream_start".to_string(),
                        payload: channel_stream_start_payload(
                            status,
                            &stream_headers,
                            &improvement_faults,
                        )?,
                    },
                )
                .await?;
                started = true;
            }
            let msg = SupplierMessage {
                id: msg.id.clone(),
                kind: "stream_chunk".to_string(),
                payload: crate::surface_wire::stream_chunk_payload(tail.as_bytes())?,
            };
            send_supplier_message(outbound, msg).await?;
        }
        if let Some(failure) = finish_failure.as_deref() {
            let error = response_conversion_failure(true, true, failure);
            send_supplier_execution_failure(outbound, &msg.id, &error).await?;
            return Ok(None);
        }
        let response_faults = improvement_faults_from_stream_notices(
            converter.take_notices(),
            &target_protocol,
            &inbound_protocol,
            &upstream_model,
        );
        if !started {
            send_supplier_message(
                outbound,
                SupplierMessage {
                    id: msg.id.clone(),
                    kind: "stream_start".to_string(),
                    payload: channel_stream_start_payload(
                        status,
                        &stream_headers,
                        &improvement_faults,
                    )?,
                },
            )
            .await?;
        }
        let end = SupplierMessage {
            id: msg.id.clone(),
            kind: "stream_end".to_string(),
            payload: {
                let mut payload = serde_json::Map::new();
                attach_improvement_faults(&mut payload, &response_faults);
                let (upstream_usage, failure) = upstream_usage.finish_observed(Some(&upstream_model));
                attach_upstream_usage(&mut payload, upstream_usage.as_ref());
                if let Some(failure) = failure { failure.attach(&mut payload); }
                if let Some(diagnostics) = &diagnostics { diagnostics.attach(&mut payload); }
                payload
            },
        };
        send_supplier_message(outbound, end).await?;
        return Ok(None);
    }
    let body = resp.text().await?;
    let upstream_usage = upstream_usage_from_body(&target_protocol, body.as_bytes());
    let converted = response_body_from_target_as_inbound_with_report_with_tools(
        &body,
        &inbound_protocol,
        &target_protocol,
        &upstream_model,
        &tool_mapping,
    )
    .map_err(|error| response_conversion_failure(false, false, error))?;
    improvement_faults.extend(converted.faults);
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    Ok(Some(channel_http_response_payload(
        status,
        &headers,
        converted.body.as_bytes(),
        &improvement_faults,
        upstream_usage.as_ref(),
        diagnostics.as_ref(),
        Some(&upstream_model),
        None,
    )?))
}

#[cfg(test)]
fn retained_subscription_upstream_override(provider: &str) -> Option<String> {
    RETAINED_SUBSCRIPTION_UPSTREAM_OVERRIDES
        .get_or_init(|| StdMutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&provider.trim().to_ascii_lowercase())
        .cloned()
}

#[cfg(test)]
static RETAINED_SUBSCRIPTION_UPSTREAM_OVERRIDES: OnceLock<StdMutex<HashMap<String, String>>> =
    OnceLock::new();

#[cfg(test)]
fn set_retained_subscription_upstream_override(provider: &str, endpoint: Option<&str>) {
    let mut overrides = RETAINED_SUBSCRIPTION_UPSTREAM_OVERRIDES
        .get_or_init(|| StdMutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let provider = provider.trim().to_ascii_lowercase();
    match endpoint {
        Some(endpoint) => {
            overrides.insert(provider, endpoint.to_string());
        }
        None => {
            overrides.remove(&provider);
        }
    }
}

#[cfg(not(test))]
fn retained_subscription_upstream_override(_provider: &str) -> Option<String> {
    None
}

pub(crate) fn supplier_target_stream_requested(
    config: &SupplierConfig,
    inbound_protocol: &str,
    target_protocol: &str,
    inbound_stream_requested: bool,
) -> bool {
    if crate::protocol::kind::ProtocolKind::parse(inbound_protocol).is_err() {
        return inbound_stream_requested;
    }
    let Ok(target_protocol) = crate::protocol::kind::ProtocolKind::parse(target_protocol) else {
        return inbound_stream_requested;
    };
    let profile = config
        .capability_profiles
        .iter()
        .find(|profile| {
            crate::protocol::kind::ProtocolKind::parse(&profile.protocol).ok()
                == Some(target_protocol)
                && matches!(profile.verification_state.as_str(), "verified" | "declared")
        })
        .or_else(|| {
            config.capability_profiles.iter().find(|profile| {
                crate::protocol::kind::ProtocolKind::parse(&profile.protocol).ok()
                    == Some(target_protocol)
            })
        });
    let Some(profile) = profile else {
        return inbound_stream_requested;
    };
    if inbound_stream_requested && profile.stream_sse_unsupported {
        false
    } else if !inbound_stream_requested
        && profile.verification_state == "verified"
        && !profile.non_stream_json
        && profile.stream_sse
    {
        true
    } else {
        inbound_stream_requested
    }
}

#[cfg(test)]
mod opaque_subscription_tests {
    use super::*;
    use crate::model::{
        ChannelProtocolBinding, ChannelSurfaceBinding, ProtocolVerification, SurfaceVerification,
    };
    use crate::surface::{ApiOperation, ApiSurface};
    use crate::surface_wire::{SurfaceEnvelope, WireBody, WireHeader};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn local_and_platform_subscription_body_rewrite_preserves_context_intent() {
        for stream in [false, true] {
            let body = format!(r#"{{ "model": "claude-sonnet-4-6[1m]", "stream": {stream}, "messages": [{{"role":"user","content":[{{"type":"text","text":"stable cache prefix","cache_control":{{"type":"ephemeral"}}}}]}}] }}"#);
            let routed = rewrite_retained_subscription_body_model(
                "claude", "/anthropic/v1/messages", &body, Some("claude-opus-4-6"),
            ).unwrap();
            assert_eq!(routed, body.replacen("claude-sonnet-4-6[1m]", "claude-opus-4-6[1m]", 1));
            let unrelated = rewrite_retained_subscription_body_model("openai", "/v1/responses", &body, Some("gpt-test")).unwrap();
            assert_eq!(unrelated, body.replacen("claude-sonnet-4-6[1m]", "gpt-test", 1));
        }
    }

    #[test]
    fn buffered_custom_channel_upstream_unavailable_is_safe_to_fail_over() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-request-id", "req-provider-123".parse().unwrap());
        headers.insert("retry-after", "3".parse().unwrap());
        let payload = channel_http_response_payload(
            400,
            &headers,
            br#"{"error":{"type":"server_error","code":"upstream_unavailable"}}"#,
            &[],
            None,
            None,
            Some("model-a"),
            None,
        )
        .expect("channel error payload");

        assert_eq!(payload["error_type"], "server_error");
        assert_eq!(payload["error_code"], "upstream_unavailable");
        assert_eq!(payload["provider_request_id"], "req-provider-123");
        assert_eq!(payload["retry_after_seconds"], 3);
        assert_eq!(payload["failure_scope"], "request");
        assert_eq!(payload["safe_to_retry_other_channel"], true);
        assert_eq!(payload["safe_to_retry_same_channel"], false);
    }

    #[test]
    fn ordinary_custom_channel_bad_request_is_not_declared_retryable() {
        let payload = channel_http_response_payload(
            400,
            &reqwest::header::HeaderMap::new(),
            br#"{"error":{"type":"invalid_request_error","code":"invalid_value","retry_after_seconds":1.2}}"#,
            &[],
            None,
            None,
            Some("model-a"),
            None,
        )
        .expect("channel error payload");

        assert_eq!(payload["error_type"], "invalid_request_error");
        assert_eq!(payload["error_code"], "invalid_value");
        assert_eq!(payload["retry_after_seconds"], 2);
        assert_eq!(payload["safe_to_retry_other_channel"], false);
        assert_eq!(payload["failure_scope"], "request");
    }

    #[tokio::test]
    async fn subscription_opaque_request_is_rejected_by_same_surface_guard() {
        let config =
            typed_subscription_config(crate::source_driver::SourceDriverId::OpenAiSubscription);

        let wire = SurfaceEnvelope {
            surface: Some(ApiSurface::OpenAi),
            operation: Some(ApiOperation::Opaque),
            method: "PATCH".to_string(),
            path: "/v1/future-operation".to_string(),
            raw_query: "mode=raw".to_string(),
            has_query: true,
            headers: vec![WireHeader {
                name: "x-opaque-marker".to_string(),
                value: WireBody::from_bytes(b"preserve-me"),
            }],
            body: WireBody::from_bytes(br#"{"model":"client-model","opaque":true}"#),
            selected_upstream_model: Some("configured-model".to_string()),
            ..SurfaceEnvelope::default()
        };
        let payload = crate::surface_wire::wire_payload(wire).expect("opaque envelope");
        let message = SupplierMessage {
            id: "opaque-subscription".to_string(),
            kind: "request".to_string(),
            payload,
        };
        let (outbound, _outbound_rx) = mpsc::channel(1);

        let error = forward_supplier_request(&Client::new(), &config, &message, false, &outbound)
            .await
            .expect_err("opaque subscription must not enter the specialized executor");

        assert_eq!(
            error.to_string(),
            "channel has no verified matching openai surface for opaque operation"
        );
    }

    fn typed_subscription_config(
        source_driver: crate::source_driver::SourceDriverId,
    ) -> SupplierConfig {
        let mut config = default_supplier_config();
        let contract = crate::channel_v2_contract_for_source(source_driver);
        config.source_driver = source_driver;
        config.executor = contract.executor;
        config.default_target = contract.default_target;
        config.discovery = contract.discovery;
        config.surface_bindings = crate::source_driver_surface_bindings(source_driver, "");
        config.kind = "custom_endpoint".to_string();
        config.api_format = "legacy-conflict".to_string();
        config.upstream_base_url = "https://legacy-conflict.invalid/v1".to_string();
        config.subscription.platform = "test-unknown".to_string();
        config
    }

    fn supplier_message(id: &str, wire: SurfaceEnvelope) -> SupplierMessage {
        SupplierMessage {
            id: id.to_string(),
            kind: "request".to_string(),
            payload: crate::surface_wire::wire_payload(wire).expect("surface envelope"),
        }
    }

    fn configure_verified_openai_http_surface(config: &mut SupplierConfig, base_url: &str) {
        config.surface_bindings = vec![ChannelSurfaceBinding {
            surface: ApiSurface::OpenAi,
            base_url: base_url.to_string(),
            endpoint_profile: "openai_compatible".to_string(),
            auth_scheme: "none".to_string(),
            protocols: vec![ChannelProtocolBinding {
                protocol: "openai_chat".to_string(),
                preferred: true,
                verification: ProtocolVerification {
                    state: "verified".to_string(),
                    checked_at_unix: 1,
                    summary: String::new(),
                },
            }],
            operation_overrides: Vec::new(),
            verification: SurfaceVerification {
                state: "verified".to_string(),
                checked_at_unix: 1,
                summary: String::new(),
            },
        }];
    }

    async fn read_complete_http_request(
        stream: &mut tokio::net::TcpStream,
        label: &str,
    ) -> Vec<u8> {
        let mut request = Vec::new();
        let mut expected_len = None;
        loop {
            let mut chunk = [0_u8; 1024];
            let read = stream.read(&mut chunk).await.expect("read request");
            assert!(read > 0, "{label} request closed before body");
            request.extend_from_slice(&chunk[..read]);
            if expected_len.is_none() {
                if let Some(header_end) =
                    request.windows(4).position(|window| window == b"\r\n\r\n")
                {
                    let content_length = String::from_utf8_lossy(&request[..header_end])
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    expected_len = Some(header_end + 4 + content_length);
                }
            }
            if expected_len.is_some_and(|expected| request.len() >= expected) {
                return request;
            }
        }
    }

    #[tokio::test]
    async fn buffered_cross_protocol_response_conversion_failure_never_returns_raw_success() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind conversion upstream");
        let address = listener.local_addr().expect("conversion upstream address");
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept conversion request");
            let _ = read_complete_http_request(&mut stream, "conversion").await;
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 8\r\nConnection: close\r\n\r\nnot-json",
                )
                .await
                .expect("write malformed conversion response");
        });

        let mut config = default_supplier_config();
        config.upstream_base_url = format!("http://{address}");
        config.api_format = "openai_chat".to_string();
        let base_url = config.upstream_base_url.clone();
        configure_verified_openai_http_surface(&mut config, &base_url);
        let message = supplier_message(
            "buffered-conversion-failure",
            SurfaceEnvelope {
                surface: Some(ApiSurface::OpenAi),
                operation: Some(ApiOperation::Responses),
                protocol: Some("openai_responses".to_string()),
                method: "POST".to_string(),
                path: "/v1/responses".to_string(),
                body: WireBody::from_bytes(br#"{"model":"code-cheap","input":"hello"}"#),
                stream: Some(false),
                ..SurfaceEnvelope::default()
            },
        );
        let (outbound, mut outbound_rx) = mpsc::channel(4);

        let error = forward_supplier_request(&Client::new(), &config, &message, false, &outbound)
            .await
            .expect_err("invalid target response must fail conversion");
        let evidence = execution_evidence_for_error(&error);
        assert_eq!(evidence.stage, "response_protocol_convert");
        assert_eq!(evidence.error_code, "response_conversion_failed");
        assert!(evidence.upstream_attempted);
        assert!(evidence.upstream_response_received);
        assert_eq!(evidence.recommendation.consumer_charge, "deny");
        assert_eq!(evidence.recommendation.supplier_penalty, "review");
        assert!(outbound_rx.try_recv().is_err());
        upstream_task.await.expect("conversion upstream task");
    }

    #[tokio::test]
    async fn subscription_missing_operation_on_unknown_path_uses_same_surface_guard() {
        let config =
            typed_subscription_config(crate::source_driver::SourceDriverId::OpenAiSubscription);
        let message = supplier_message(
            "opaque-missing-operation",
            SurfaceEnvelope {
                surface: Some(ApiSurface::OpenAi),
                method: "PATCH".to_string(),
                path: "/v1/future-operation%2Fpart".to_string(),
                raw_query: "mode=raw%2Fvalue".to_string(),
                has_query: true,
                body: WireBody::from_bytes(br#"{"opaque":true}"#),
                ..SurfaceEnvelope::default()
            },
        );
        let (outbound, _outbound_rx) = mpsc::channel(1);

        let error = forward_supplier_request(&Client::new(), &config, &message, false, &outbound)
            .await
            .expect_err("missing operation must not enter the subscription executor");

        assert_eq!(
            error.to_string(),
            "channel has no verified matching openai surface for opaque operation"
        );
    }

    #[tokio::test]
    async fn subscription_forged_known_operation_on_unknown_path_is_rejected_before_provider() {
        let config =
            typed_subscription_config(crate::source_driver::SourceDriverId::OpenAiSubscription);
        let message = supplier_message(
            "opaque-forged-operation",
            SurfaceEnvelope {
                surface: Some(ApiSurface::OpenAi),
                operation: Some(ApiOperation::ChatCompletions),
                protocol: Some("openai_chat".to_string()),
                method: "PATCH".to_string(),
                path: "/v1/future-operation".to_string(),
                body: WireBody::from_bytes(br#"{"opaque":true}"#),
                ..SurfaceEnvelope::default()
            },
        );
        let (outbound, _outbound_rx) = mpsc::channel(1);

        let error = forward_supplier_request(&Client::new(), &config, &message, false, &outbound)
            .await
            .expect_err("forged known operation must be rejected before provider dispatch");

        assert!(
            error
                .to_string()
                .contains("declared operation chat_completions does not match resolved opaque"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn subscription_valid_known_operation_preserves_escaped_path_and_bare_query() {
        let config =
            typed_subscription_config(crate::source_driver::SourceDriverId::GeminiSubscription);
        let wire = SurfaceEnvelope {
            surface: Some(ApiSurface::Gemini),
            operation: Some(ApiOperation::GenerateContent),
            protocol: Some("gemini_native".to_string()),
            method: "POST".to_string(),
            path: "/v1beta/models/model%2Fpart:generateContent".to_string(),
            has_query: true,
            body: WireBody::from_bytes(br#"{}"#),
            ..SurfaceEnvelope::default()
        };
        let resolved = resolve_supplier_wire_route(&wire).expect("resolve valid known route");
        assert_eq!(
            resolved.canonical_path,
            "/gemini/v1beta/models/model%2Fpart:generateContent"
        );
        assert_eq!(
            resolved.request_target,
            "/gemini/v1beta/models/model%2Fpart:generateContent?"
        );
        assert_eq!(resolved.route.surface, ApiSurface::Gemini);
        assert_eq!(resolved.route.operation, ApiOperation::GenerateContent);
        assert_eq!(
            resolved.route.protocol,
            Some(crate::protocol::kind::ProtocolKind::GeminiNative)
        );
        let message = supplier_message("known-operation", wire);
        let (outbound, _outbound_rx) = mpsc::channel(1);

        let payload = forward_supplier_request(&Client::new(), &config, &message, false, &outbound)
            .await
            .expect("valid known operation")
            .expect("known operation uses buffered subscription executor");

        let response = SurfaceEnvelope::from_payload(&payload).expect("typed error response");
        assert_eq!(response.status, Some(502));
        assert!(response
            .body
            .utf8()
            .is_ok_and(|body| body.contains("credential reference is empty")));
    }

    #[tokio::test]
    async fn opaque_stream_flag_without_content_type_preserves_raw_buffered_response() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind opaque upstream");
        let address = listener.local_addr().expect("opaque upstream address");
        let response_body = [0_u8, 255, 16, 128];
        let mut upstream_task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept opaque request");
            let _ = read_complete_http_request(&mut stream, "opaque").await;
            let mut response =
                b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\n".to_vec();
            response.extend_from_slice(&response_body);
            stream
                .write_all(&response)
                .await
                .expect("write opaque response");
        });

        let mut config = default_supplier_config();
        config.upstream_base_url = format!("http://{address}");
        config.api_format = "openai_chat".to_string();
        let base_url = config.upstream_base_url.clone();
        configure_verified_openai_http_surface(&mut config, &base_url);
        let wire = SurfaceEnvelope {
            surface: Some(ApiSurface::OpenAi),
            operation: Some(ApiOperation::Opaque),
            method: "POST".to_string(),
            path: "/v1/future-stream".to_string(),
            stream: Some(true),
            ..SurfaceEnvelope::default()
        };
        let payload = crate::surface_wire::wire_payload(wire).expect("opaque envelope");
        let message = SupplierMessage {
            id: "opaque-missing-content-type".to_string(),
            kind: "request".to_string(),
            payload,
        };
        let (outbound, mut outbound_rx) = mpsc::channel(4);

        let payload = forward_supplier_request(&Client::new(), &config, &message, true, &outbound)
            .await
            .expect("forward opaque response")
            .expect("opaque response remains buffered");
        let response = SurfaceEnvelope::from_payload(&payload).expect("decode opaque response");

        assert_eq!(response.status, Some(200));
        assert_eq!(response.body.decode().unwrap(), response_body);
        assert!(!response.header_map().unwrap().contains_key("content-type"));
        assert!(outbound_rx.try_recv().is_err());
        match tokio::time::timeout(std::time::Duration::from_secs(2), &mut upstream_task).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => panic!("opaque upstream task failed: {error}"),
            Err(_) => {
                upstream_task.abort();
                let _ = upstream_task.await;
                panic!("opaque upstream task did not finish");
            }
        }
    }

    #[tokio::test]
    async fn known_native_stream_flag_without_content_type_preserves_raw_buffered_response() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind native upstream");
        let address = listener.local_addr().expect("native upstream address");
        let response_body = [0_u8, 255, 16, 128];
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept native request");
            let _ = read_complete_http_request(&mut stream, "native").await;
            let mut response =
                b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\n".to_vec();
            response.extend_from_slice(&response_body);
            stream
                .write_all(&response)
                .await
                .expect("write native response");
        });

        let mut config = default_supplier_config();
        config.upstream_base_url = format!("http://{address}");
        config.api_format = "openai_chat".to_string();
        let base_url = config.upstream_base_url.clone();
        configure_verified_openai_http_surface(&mut config, &base_url);
        let wire = SurfaceEnvelope {
            surface: Some(ApiSurface::OpenAi),
            operation: Some(ApiOperation::ChatCompletions),
            protocol: Some("openai_chat".to_string()),
            method: "POST".to_string(),
            path: "/v1/chat/completions".to_string(),
            stream: Some(true),
            body: WireBody::from_bytes(br#"{"model":"code-cheap","messages":[],"stream":true}"#),
            ..SurfaceEnvelope::default()
        };
        let message = supplier_message("native-missing-content-type", wire);
        let (outbound, mut outbound_rx) = mpsc::channel(4);

        let payload = forward_supplier_request(&Client::new(), &config, &message, true, &outbound)
            .await
            .expect("forward native response")
            .expect("native response remains buffered");
        let response = SurfaceEnvelope::from_payload(&payload).expect("decode native response");

        assert_eq!(response.status, Some(200));
        assert_eq!(response.body.decode().unwrap(), response_body);
        assert!(!response.header_map().unwrap().contains_key("content-type"));
        assert!(outbound_rx.try_recv().is_err());
        upstream_task.await.expect("native upstream task");
    }

    #[tokio::test]
    async fn native_stream_start_waits_for_first_body_byte() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind stream upstream");
        let address = listener.local_addr().expect("stream upstream address");
        let (release_body_tx, release_body_rx) = tokio::sync::oneshot::channel();
        let (headers_sent_tx, headers_sent_rx) = tokio::sync::oneshot::channel();
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept stream request");
            let _ = read_complete_http_request(&mut stream, "stream").await;
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 10\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("write stream headers");
            headers_sent_tx.send(()).expect("signal stream headers");
            release_body_rx.await.expect("release stream body");
            stream
                .write_all(b"data: hi\n\n")
                .await
                .expect("write stream body");
        });

        let mut config = default_supplier_config();
        config.upstream_base_url = format!("http://{address}");
        config.api_format = "openai_chat".to_string();
        let base_url = config.upstream_base_url.clone();
        configure_verified_openai_http_surface(&mut config, &base_url);
        let message = supplier_message(
            "delayed-stream",
            SurfaceEnvelope {
                surface: Some(ApiSurface::OpenAi),
                operation: Some(ApiOperation::ChatCompletions),
                protocol: Some("openai_chat".to_string()),
                method: "POST".to_string(),
                path: "/v1/chat/completions".to_string(),
                stream: Some(true),
                body: WireBody::from_bytes(
                    br#"{"model":"code-cheap","messages":[],"stream":true}"#,
                ),
                ..SurfaceEnvelope::default()
            },
        );
        let (outbound, mut outbound_rx) = mpsc::channel(4);
        let forward_task = tokio::spawn(async move {
            forward_supplier_request(&Client::new(), &config, &message, true, &outbound).await
        });

        headers_sent_rx.await.expect("wait for stream headers");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(150), outbound_rx.recv())
                .await
                .is_err(),
            "stream_start was emitted before the first body byte"
        );
        release_body_tx.send(()).expect("release body");

        for expected_kind in ["stream_start", "stream_chunk", "stream_end"] {
            let message =
                tokio::time::timeout(std::time::Duration::from_secs(2), outbound_rx.recv())
                    .await
                    .expect("stream message timeout")
                    .expect("stream message channel closed");
            assert_eq!(message.kind, expected_kind);
        }
        assert!(forward_task
            .await
            .expect("forward stream task")
            .expect("forward stream")
            .is_none());
        upstream_task.await.expect("stream upstream task");
    }

    #[tokio::test]
    async fn final_server_error_preserves_status_repeated_headers_and_binary_body() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind error upstream");
        let address = listener.local_addr().expect("error upstream address");
        let response_body = [0_u8, 255, 16, 128];
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept error request");
            let _ = read_complete_http_request(&mut stream, "error").await;
            let mut response = b"HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/octet-stream\r\nSet-Cookie: a=1\r\nSet-Cookie: b=2\r\nX-Repeated: first\r\nX-Repeated: second\r\nContent-Length: 4\r\nConnection: close\r\n\r\n".to_vec();
            response.extend_from_slice(&response_body);
            stream
                .write_all(&response)
                .await
                .expect("write error response");
        });

        let mut config = default_supplier_config();
        config.upstream_base_url = format!("http://{address}");
        config.api_format = "openai_chat".to_string();
        let base_url = config.upstream_base_url.clone();
        configure_verified_openai_http_surface(&mut config, &base_url);
        let message = supplier_message(
            "final-error",
            SurfaceEnvelope {
                surface: Some(ApiSurface::OpenAi),
                operation: Some(ApiOperation::ChatCompletions),
                protocol: Some("openai_chat".to_string()),
                method: "POST".to_string(),
                path: "/v1/chat/completions".to_string(),
                stream: Some(true),
                body: WireBody::from_bytes(
                    br#"{"model":"code-cheap","messages":[],"stream":true}"#,
                ),
                ..SurfaceEnvelope::default()
            },
        );
        let (outbound, mut outbound_rx) = mpsc::channel(4);

        let payload = forward_supplier_request(&Client::new(), &config, &message, true, &outbound)
            .await
            .expect("forward final error")
            .expect("final error is buffered");
        let response = SurfaceEnvelope::from_payload(&payload).expect("decode final error");
        let headers = response.header_map().expect("error headers");

        assert_eq!(response.status, Some(503));
        assert_eq!(response.body.decode().unwrap(), response_body);
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
        assert!(outbound_rx.try_recv().is_err());
        upstream_task.await.expect("error upstream task");
    }
}
