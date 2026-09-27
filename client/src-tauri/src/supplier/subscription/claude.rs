#[cfg(test)]
pub(crate) async fn forward_claude_subscription_request(
    client: &Client,
    channel: &ChannelConfig,
    path: &str,
    body: &str,
    upstream_override: Option<&str>,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    forward_claude_subscription_request_with_headers(
        client,
        channel,
        path,
        body,
        upstream_override,
        None,
        None,
    )
    .await
}

async fn forward_claude_subscription_request_with_headers(
    client: &Client,
    channel: &ChannelConfig,
    path: &str,
    body: &str,
    upstream_override: Option<&str>,
    cache_identity: Option<&str>,
    request_headers: Option<&reqwest::header::HeaderMap>,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    if subscription_inbound_protocol(path).is_none() {
        return Ok(subscription_wire_error_response(
            404,
            "subscription_path_not_supported",
            &format!("Claude subscription could not identify the inbound protocol: {path}"),
        ));
    }
    let model = channel_upstream_model_for_request(
        channel,
        requested_model_from_body_or_path(body, path).as_deref(),
    );
    // The current 1M native models use the base ID at standard pricing.
    // Keep caller-provided protocol headers, but do not invent an extra beta.
    let model = crate::config::without_context_hint(&model).to_string();
    let count_tokens = is_anthropic_count_tokens_path(path);
    let conversion_body = if count_tokens {
        json_body_without_stream(body).unwrap_or_else(|_| body.to_string())
    } else {
        json_body_with_stream(body, false).unwrap_or_else(|_| body.to_string())
    };
    let conversion = upstream_request_for_api_format_with_profiles_report(
        "anthropic_messages",
        path,
        &conversion_body,
        &model,
        &channel.capability_profiles,
    )
    .map_err(request_conversion_failure)?;
    let mut improvement_faults = conversion.faults;
    let tool_mapping = conversion.tool_mapping;
    let upstream_body = conversion.body;
    let inbound_protocol = conversion.inbound_protocol;
    let target_protocol = conversion.target_protocol;
    let upstream_body = if count_tokens {
        normalize_claude_subscription_count_tokens_body(&json_body_without_stream(&upstream_body)?)?
    } else {
        let dialect = ensure_claude_messages_body_with_faults(&upstream_body, &model)?;
        improvement_faults.extend(dialect.faults);
        prepare_claude_subscription_oauth_body_with_cache_identity(
            &dialect.body,
            &channel.v2.credential_ref,
            cache_identity,
        )?
    };
    let (token, _credential, _) = ensure_claude_subscription_access_token(client, channel).await?;
    let url = claude_subscription_operation_url(
        upstream_override
            .map(str::to_string)
            .unwrap_or_else(|| CLAUDE_MESSAGES_URL.to_string()),
        count_tokens,
    );
    let (status, content_type, raw, headers) = send_claude_subscription_body_with_headers(
        client,
        channel,
        &url,
        token,
        upstream_body.clone(),
        CLAUDE_OAUTH_TOKEN_URL,
        request_headers,
    )
    .await?;
    if let Some(fault) = ImprovementFault::unrecovered_provider_rejection(
        "claude",
        &url,
        &inbound_protocol,
        &target_protocol,
        &model,
        status,
        &raw,
    ) {
        improvement_faults.push(fault);
    }
    let upstream_usage = upstream_usage_from_response("anthropic_messages", raw.as_bytes());
    let response_body = if (200..300).contains(&status) && count_tokens {
        raw
    } else if (200..300).contains(&status) {
        let converted = response_body_from_target_as_inbound_with_report_with_tools(
            &raw,
            &inbound_protocol,
            &target_protocol,
            &model,
            &tool_mapping,
        )
        .map_err(|error| response_conversion_failure(false, false, error))?;
        improvement_faults.extend(converted.faults);
        converted.body
    } else {
        raw
    };
    let output_content_type = if (200..300).contains(&status) {
        "application/json"
    } else {
        &content_type
    };
    let mut payload = subscription_wire_response_payload_with_owned_headers(
        status,
        output_content_type,
        response_body,
        &headers,
        (200..300).contains(&status) && !count_tokens,
    );
    attach_improvement_faults(&mut payload, &improvement_faults);
    attach_upstream_usage(&mut payload, upstream_usage.as_ref());
    Ok(payload)
}

#[cfg(test)]
pub(crate) fn openai_subscription_response_request(
    client: &Client,
    url: &str,
    token: &str,
    credential: &serde_json::Value,
    body: String,
    request_headers: Option<&reqwest::header::HeaderMap>,
) -> Result<reqwest::RequestBuilder> {
    let encoded = encode_openai_subscription_request_body(&body)?;
    Ok(openai_subscription_response_request_with_encoded_body(
        client,
        url,
        token,
        credential,
        &body,
        encoded,
        request_headers,
    ))
}

fn openai_subscription_response_request_with_encoded_body(
    client: &Client,
    url: &str,
    token: &str,
    credential: &serde_json::Value,
    identity_body: &str,
    body: bytes::Bytes,
    request_headers: Option<&reqwest::header::HeaderMap>,
) -> reqwest::RequestBuilder {
    let identity = active_codex_identity();
    let response_headers =
        codex_response_identity_headers(identity_body, request_headers, &identity);
    let mut req = client
        .post(url)
        .header("Accept", "text/event-stream, application/json")
        .header("Content-Type", "application/json")
        .header("Content-Encoding", "zstd")
        .header("Authorization", format!("Bearer {token}"))
        .headers(response_headers)
        .body(body);
    if let Some(account_id) = json_string(credential, &["account_id"])
        .or_else(|| json_string(credential, &["accountID"]))
        .or_else(|| json_string(credential, &["chatgpt_account_id"]))
        .filter(|value| !value.trim().is_empty())
    {
        req = req.header("Chatgpt-Account-Id", account_id);
    }
    req
}

fn encode_openai_subscription_request_body(body: &str) -> Result<bytes::Bytes> {
    let started_at = std::time::Instant::now();
    let encoded = zstd::stream::encode_all(std::io::Cursor::new(body.as_bytes()), 3)
        .context("compress OpenAI subscription request body with zstd")?;
    log::debug!(
        "[const-api][request-timing] phase=request_compressed encoding=zstd duration_us={} original_bytes={} encoded_bytes={}",
        started_at.elapsed().as_micros(),
        body.len(),
        encoded.len(),
    );
    Ok(bytes::Bytes::from(encoded))
}

pub(crate) fn grok_subscription_response_request(
    client: &Client,
    url: &str,
    token: &str,
    body: String,
) -> Result<reqwest::RequestBuilder> {
    let model = serde_json::from_str::<serde_json::Value>(&body)
        .context("parse Grok Responses request body")?
        .get("model")
        .and_then(serde_json::Value::as_str)
        .filter(|model| !model.trim().is_empty())
        .ok_or_else(|| anyhow!("Grok Responses request is missing a model"))?
        .to_string();
    Ok(client
        .post(url)
        .header("Accept", "text/event-stream, application/json")
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {token}"))
        .header("X-XAI-Token-Auth", "xai-grok-cli")
        .header("x-grok-client-version", GROK_CLI_VERSION)
        .header("X-Grok-Client-Mode", "interactive")
        .header("x-grok-model-override", model)
        .header("User-Agent", GROK_CLI_USER_AGENT)
        .body(body))
}

fn codex_response_identity_headers(
    body: &str,
    request_headers: Option<&reqwest::header::HeaderMap>,
    identity: &CodexClientIdentity,
) -> reqwest::header::HeaderMap {
    let mut headers = codex_model_request_context_headers(request_headers, identity);
    let Ok(body) = serde_json::from_str::<serde_json::Value>(body) else {
        return headers;
    };
    if body_uses_codex_responses_lite(&body) {
        headers
            .entry("x-openai-internal-codex-responses-lite")
            .or_insert(reqwest::header::HeaderValue::from_static("true"));
    }
    let metadata = body
        .get("client_metadata")
        .and_then(serde_json::Value::as_object);
    let metadata_string = |key: &str| {
        metadata
            .and_then(|values| values.get(key))
            .and_then(serde_json::Value::as_str)
    };
    let session_id = metadata_string("session_id");
    let thread_id = metadata_string("thread_id");

    insert_codex_response_header_if_absent(&mut headers, "session-id", session_id);
    insert_codex_response_header_if_absent(&mut headers, "thread-id", thread_id);
    insert_codex_response_header_if_absent(&mut headers, "x-client-request-id", thread_id);
    for name in [
        "x-codex-window-id",
        "x-codex-turn-metadata",
        "x-codex-parent-thread-id",
        "x-codex-turn-state",
        "x-codex-beta-features",
        "x-codex-routing-hint",
        "x-openai-subagent",
        "x-openai-memgen-request",
        "x-openai-internal-codex-responses-lite",
        "x-responsesapi-include-timing-metrics",
    ] {
        insert_codex_response_header_if_absent(&mut headers, name, metadata_string(name));
    }
    headers
}

fn insert_codex_response_header_if_absent(
    headers: &mut reqwest::header::HeaderMap,
    name: &'static str,
    value: Option<&str>,
) {
    if headers.contains_key(name) {
        return;
    }
    let Some(value) = value.filter(|value| !value.trim().is_empty()) else {
        return;
    };
    if let Ok(value) = reqwest::header::HeaderValue::from_str(value) {
        headers.insert(reqwest::header::HeaderName::from_static(name), value);
    }
}

pub(crate) async fn send_openai_subscription_response_request(
    client: &Client,
    channel: &ChannelConfig,
    url: &str,
    token: String,
    credential: serde_json::Value,
    body: String,
    token_url: &str,
    provider: &str,
    request_headers: Option<&reqwest::header::HeaderMap>,
) -> Result<reqwest::Response> {
    let grok = provider == "grok";
    let encoded_openai_body = if grok {
        None
    } else {
        Some(encode_openai_subscription_request_body(&body)?)
    };
    let request = if grok {
        grok_subscription_response_request(client, url, &token, body.clone())?
    } else if let Some(encoded_body) = encoded_openai_body.as_ref() {
        openai_subscription_response_request_with_encoded_body(
            client,
            url,
            &token,
            &credential,
            &body,
            encoded_body.clone(),
            request_headers,
        )
    } else {
        return Err(anyhow!("OpenAI subscription request body was not encoded"));
    };
    let resp = crate::upstream_transport::send(request).await?;
    if !grok {
        crate::detection::observe_openai_subscription_response_headers(channel, resp.headers());
    }
    if resp.status() != reqwest::StatusCode::UNAUTHORIZED {
        return Ok(resp);
    }
    let _ = resp.text().await;
    let (fresh_token, fresh_credential, _) = if grok {
        ensure_grok_subscription_access_token_with_options(client, channel, true, token_url).await?
    } else {
        ensure_openai_subscription_access_token_with_options(client, channel, Some(&token), token_url)
            .await?
    };
    let request = if grok {
        grok_subscription_response_request(client, url, &fresh_token, body)?
    } else if let Some(encoded_body) = encoded_openai_body {
        openai_subscription_response_request_with_encoded_body(
            client,
            url,
            &fresh_token,
            &fresh_credential,
            &body,
            encoded_body,
            request_headers,
        )
    } else {
        return Err(anyhow!("OpenAI subscription request body was not encoded"));
    };
    let response = crate::upstream_transport::send(request).await?;
    if !grok {
        crate::detection::observe_openai_subscription_response_headers(channel, response.headers());
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            log::warn!(
                "[const-api][subscription-auth] provider=openai stage=response_after_refresh status=401 recovery=exhausted channel_id={}",
                channel.id,
            );
        }
    }
    Ok(response)
}

struct BufferedUpstreamFailure {
    status: u16,
    content_type: String,
    headers: reqwest::header::HeaderMap,
    raw: String,
}

struct OpenAiCompatResponse {
    response: Option<reqwest::Response>,
    buffered_failure: Option<BufferedUpstreamFailure>,
    request_body: String,
    faults: Vec<ImprovementFault>,
}

#[allow(clippy::too_many_arguments)]
async fn send_openai_with_failure_capture(
    client: &Client,
    channel: &ChannelConfig,
    url: &str,
    token: String,
    credential: serde_json::Value,
    body: String,
    token_url: &str,
    source_protocol: &str,
    target_protocol: &str,
    model: &str,
    provider: &str,
    request_headers: Option<&reqwest::header::HeaderMap>,
) -> Result<OpenAiCompatResponse> {
    let response = send_openai_subscription_response_request(
        client,
        channel,
        url,
        token,
        credential,
        body.clone(),
        token_url,
        provider,
        request_headers,
    )
    .await?;
    if response.status() != reqwest::StatusCode::BAD_REQUEST {
        return Ok(OpenAiCompatResponse {
            response: Some(response),
            buffered_failure: None,
            request_body: body,
            faults: Vec::new(),
        });
    }

    // A provider 400 is diagnostic evidence, not permission to rewrite and
    // resend a request. Known subscription incompatibilities must be handled
    // before the first send by the endpoint-specific adapter.
    let failure = BufferedUpstreamFailure {
        status: response.status().as_u16(),
        content_type: response_content_type(&response),
        headers: response.headers().clone(),
        raw: response.text().await?,
    };
    let faults = ImprovementFault::unrecovered_provider_rejection(
        provider,
        url,
        source_protocol,
        target_protocol,
        model,
        failure.status,
        &failure.raw,
    )
    .into_iter()
    .collect();
    Ok(OpenAiCompatResponse {
        response: None,
        buffered_failure: Some(failure),
        request_body: body,
        faults,
    })
}

#[cfg(test)]
pub(crate) async fn send_claude_subscription_body(
    client: &Client,
    channel: &ChannelConfig,
    url: &str,
    token: String,
    body: String,
    token_url: &str,
) -> Result<(u16, String, String, Vec<(String, String)>)> {
    send_claude_subscription_body_with_headers(client, channel, url, token, body, token_url, None)
        .await
}

async fn send_claude_subscription_body_with_headers(
    client: &Client,
    channel: &ChannelConfig,
    url: &str,
    token: String,
    body: String,
    token_url: &str,
    request_headers: Option<&reqwest::header::HeaderMap>,
) -> Result<(u16, String, String, Vec<(String, String)>)> {
    let resp = crate::upstream_transport::send(claude_subscription_request_with_headers(
        client,
        url,
        &token,
        body.clone(),
        request_headers,
    ))
    .await?;
    if resp.status() != reqwest::StatusCode::UNAUTHORIZED {
        crate::detection::observe_claude_subscription_response_headers(channel, resp.headers());
        let status = resp.status().as_u16();
        let content_type = response_content_type(&resp);
        let headers = response_header_pairs(resp.headers());
        let raw = resp.text().await?;
        return Ok((status, content_type, raw, headers));
    }
    let _ = resp.text().await;
    let (fresh_token, _, _) =
        ensure_claude_subscription_access_token_with_options(client, channel, true, token_url)
            .await?;
    let retry = crate::upstream_transport::send(claude_subscription_request_with_headers(
        client,
        url,
        &fresh_token,
        body,
        request_headers,
    ))
    .await?;
    crate::detection::observe_claude_subscription_response_headers(channel, retry.headers());
    let status = retry.status().as_u16();
    let content_type = response_content_type(&retry);
    let headers = response_header_pairs(retry.headers());
    let raw = retry.text().await?;
    Ok((status, content_type, raw, headers))
}

#[cfg(test)]
pub(crate) async fn forward_claude_subscription_stream_request(
    client: &Client,
    config: &SupplierConfig,
    msg_id: &str,
    path: &str,
    body: &str,
    outbound: &SupplierOutbound,
    upstream_override: Option<&str>,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>> {
    forward_claude_subscription_stream_request_with_capacity(
        client,
        config,
        msg_id,
        path,
        body,
        outbound,
        upstream_override,
        None,
        None,
        None,
    )
    .await
}

pub(crate) async fn forward_claude_subscription_stream_request_with_capacity(
    client: &Client,
    config: &SupplierConfig,
    msg_id: &str,
    path: &str,
    body: &str,
    outbound: &SupplierOutbound,
    upstream_override: Option<&str>,
    capacity: Option<SubscriptionRequestCapacity>,
    cache_identity: Option<&str>,
    request_headers: Option<&reqwest::header::HeaderMap>,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>> {
    let started_at = std::time::Instant::now();
    let _safety_guard = match subscription_safety_enter_with_capacity(
        config,
        body,
        capacity.unwrap_or_else(|| SubscriptionRequestCapacity::for_path(path)),
    ) {
        Ok(guard) => guard,
        Err(payload) => {
            let _ =
                append_subscription_wire_audit(config, path, body, &payload, started_at.elapsed());
            return Ok(Some(payload));
        }
    };
    if subscription_inbound_protocol(path).is_none() {
        let payload = subscription_wire_error_response(
            501,
            "subscription_stream_not_supported",
            "Claude subscription could not identify the inbound streaming protocol",
        );
        let _ = append_subscription_wire_audit(config, path, body, &payload, started_at.elapsed());
        return Ok(Some(payload));
    }
    let channel = channel_from_supplier("subscription-runtime".to_string(), config);
    let model = channel_upstream_model_for_request(
        &channel,
        requested_model_from_body_or_path(body, path).as_deref(),
    );
    let model = crate::config::without_context_hint(&model).to_string();
    let conversion_body = json_body_with_stream(body, false).unwrap_or_else(|_| body.to_string());
    let conversion = upstream_request_for_api_format_with_profiles_report(
        "anthropic_messages",
        path,
        &conversion_body,
        &model,
        &config.capability_profiles,
    )
    .map_err(request_conversion_failure)?;
    let mut improvement_faults = conversion.faults;
    let tool_mapping = conversion.tool_mapping;
    let upstream_body = conversion.body;
    let inbound_protocol = conversion.inbound_protocol;
    let target_protocol = conversion.target_protocol;
    let dialect = ensure_claude_messages_body_with_faults(&upstream_body, &model)?;
    improvement_faults.extend(dialect.faults);
    let oauth_body = prepare_claude_subscription_oauth_body_with_cache_identity(
        &dialect.body,
        &channel.v2.credential_ref,
        cache_identity,
    )?;
    let upstream_body = json_body_with_stream(&oauth_body, true)?;
    let (token, _, _) = ensure_claude_subscription_access_token(client, &channel).await?;
    let mut active_token = token;
    let url = upstream_override
        .map(str::to_string)
        .unwrap_or_else(|| CLAUDE_MESSAGES_URL.to_string());
    let upstream_send_started = std::time::Instant::now();
    let mut resp = crate::upstream_transport::send(claude_subscription_request_with_headers(
        client,
        &url,
        &active_token,
        upstream_body.clone(),
        request_headers,
    ))
    .await?;
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        let _ = resp.text().await;
        let (fresh_token, _, _) = ensure_claude_subscription_access_token_with_options(
            client,
            &channel,
            true,
            CLAUDE_OAUTH_TOKEN_URL,
        )
        .await?;
        active_token = fresh_token;
        resp = crate::upstream_transport::send(claude_subscription_request_with_headers(
            client,
            &url,
            &active_token,
            upstream_body.clone(),
            request_headers,
        ))
        .await?;
    }
    let upstream_send_ms = elapsed_millis_u64(upstream_send_started);
    crate::detection::observe_claude_subscription_response_headers(&channel, resp.headers());
    if resp.status() == reqwest::StatusCode::BAD_REQUEST {
        let original_headers = resp.headers().clone();
        let original_content_type = response_content_type(&resp);
        let original_raw = resp.text().await?;
        if let Some(fault) = ImprovementFault::unrecovered_provider_rejection(
            "claude",
            &url,
            &inbound_protocol,
            &target_protocol,
            &model,
            400,
            &original_raw,
        ) {
            improvement_faults.push(fault);
        }
        let mut payload = subscription_wire_response_payload_with_header_map(
            400,
            &original_content_type,
            original_raw,
            &original_headers,
            false,
        );
        attach_improvement_faults(&mut payload, &improvement_faults);
        let _ = append_subscription_wire_audit(config, path, body, &payload, started_at.elapsed());
        return Ok(Some(payload));
    }
    let status = resp.status().as_u16();
    let content_type = response_content_type(&resp);
    if !(200..300).contains(&status) {
        let headers = resp.headers().clone();
        let raw = resp.text().await?;
        let mut payload = subscription_wire_response_payload_with_header_map(
            status,
            &content_type,
            raw,
            &headers,
            false,
        );
        attach_improvement_faults(&mut payload, &improvement_faults);
        let _ = append_subscription_wire_audit(config, path, body, &payload, started_at.elapsed());
        return Ok(Some(payload));
    }
    let stream_headers = subscription_stream_headers(resp.headers());
    let mut started = false;
    let mut stream = SupplierByteStreamBatcher::new(resp.bytes_stream());
    let mut converter = inbound_sse_stream_converter(&inbound_protocol, &target_protocol, &model)?.with_tool_mapping(tool_mapping);
    let mut upstream_utf8 = crate::protocol::stream::Utf8ChunkDecoder::new();
    let mut converted_utf8 = crate::protocol::stream::Utf8ChunkDecoder::new();
    let mut terminal_raw = String::new();
    let mut converted_raw = SubscriptionOutputCapture::new(raw_debug_logs_enabled());
    let mut timed_out = false;
    let mut usable_output_produced = false;
    let mut upstream_usage = SupplierUpstreamUsageAccumulator::new("anthropic_messages");
    let body_read_started = std::time::Instant::now();
    let mut stream_debug = SubscriptionStreamDebug {
        upstream_send_ms,
        ..Default::default()
    };
    let mut timing_sse_decoder = Some(crate::protocol::stream::SseDecoder::default());
    loop {
        let batch = match stream.next().await? {
            SupplierStreamRead::Chunk(batch) => batch,
            SupplierStreamRead::End => break,
            SupplierStreamRead::InactivityTimeout => {
                timed_out = true;
                let failed = converter.fail(
                    "stream_inactivity_timeout",
                    "upstream stream produced no event before the inactivity deadline",
                );
                let text = converted_utf8.push(&failed)?;
                if started && !text.is_empty() {
                    converted_raw.push_str(&text);
                    send_subscription_stream_chunk(
                        outbound,
                        msg_id,
                        status,
                        &stream_headers,
                        &mut started,
                        &improvement_faults,
                        text.as_bytes(),
                    )
                    .await?;
                }
                break;
            }
        };
        let chunk = batch.bytes;
        stream_debug.observe_body_chunk(
            body_read_started,
            batch.source_chunks,
            batch.largest_source_chunk,
            chunk.len(),
        );
        let saw_meaningful_event = if let Some(decoder) = timing_sse_decoder.as_mut() {
            decoder.push(&chunk).ok().is_some_and(|frames| {
                stream_debug.observe_sse_frames(
                    "anthropic_messages",
                    upstream_send_started,
                    &frames,
                )
            })
        } else {
            false
        };
        if saw_meaningful_event {
            timing_sse_decoder = None;
        }
        upstream_usage.push(&chunk);
        terminal_raw.push_str(&upstream_utf8.push(&chunk)?);
        let converted = converter.push(&chunk);
        let conversion_failed = converter.failure().is_some();
        if conversion_failed && !started {
            break;
        }
        let text = converted_utf8.push(&converted)?;
        if text.is_empty() {
            continue;
        }
        if !conversion_failed {
            usable_output_produced = true;
        }
        converted_raw.push_str(&text);
        send_subscription_stream_chunk(
            outbound,
            msg_id,
            status,
            &stream_headers,
            &mut started,
            &improvement_faults,
            text.as_bytes(),
        )
        .await?;
        if conversion_failed {
            break;
        }
    }
    stream_debug.body_read_ms = elapsed_millis_u64(body_read_started);
    terminal_raw.push_str(&upstream_utf8.finish()?);
    let tail_bytes = converter.finish();
    let conversion_failed = converter.failure().is_some();
    let mut tail = String::new();
    if !conversion_failed || started {
        tail = converted_utf8.push(&tail_bytes)?;
        tail.push_str(&converted_utf8.finish()?);
    }
    if !tail.is_empty() {
        if !conversion_failed {
            usable_output_produced = true;
        }
        converted_raw.push_str(&tail);
        send_subscription_stream_chunk(
            outbound,
            msg_id,
            status,
            &stream_headers,
            &mut started,
            &improvement_faults,
            tail.as_bytes(),
        )
        .await?;
    }
    let execution_error =
        subscription_stream_execution_error(&converter, timed_out, started, usable_output_produced);
    improvement_faults.extend(improvement_faults_from_stream_notices(
        converter.take_notices(),
        &target_protocol,
        &inbound_protocol,
        &model,
    ));
    let upstream_usage = upstream_usage.finish();
    let mut terminal_error = execution_error
        .as_ref()
        .map(supplier_execution_error_payload)
        .or_else(|| subscription_terminal_wire_error_payload(&terminal_raw));
    if !started {
        if let Some(payload) = terminal_error.as_mut() {
            attach_improvement_faults(payload, &improvement_faults);
        }
    }
    let mut payload = terminal_error.clone().unwrap_or_else(|| {
        subscription_wire_response_payload(status, "text/event-stream", String::new())
    });
    attach_improvement_faults(&mut payload, &improvement_faults);
    attach_upstream_usage(&mut payload, upstream_usage.as_ref());
    insert_subscription_exchange_metrics_with_returned_bytes(
        &mut payload,
        body,
        &upstream_body,
        &terminal_raw,
        converted_raw.len(),
    );
    insert_subscription_stream_debug_metrics(&mut payload, &stream_debug);
    let _ = append_supplier_raw_exchange_log(
        config,
        "claude",
        path,
        &url,
        body,
        &upstream_body,
        status,
        &content_type,
        &terminal_raw,
        "text/event-stream",
        converted_raw.as_str(),
        Some(&stream_debug),
    );
    let _ = append_subscription_wire_audit(config, path, body, &payload, started_at.elapsed());
    if let Some(payload) = terminal_error {
        if !started {
            return Ok(Some(payload));
        }
        send_supplier_message(
            outbound,
            SupplierMessage {
                id: msg_id.to_string(),
                kind: "error".to_string(),
                payload,
            },
        )
        .await?;
    } else {
        finish_subscription_stream(
            outbound,
            msg_id,
            status,
            &stream_headers,
            &mut started,
            &improvement_faults,
            upstream_usage.as_ref(),
        )
        .await?;
    }
    Ok(None)
}

fn native_claude_code_headers(
    body: &str,
    request_headers: Option<&reqwest::header::HeaderMap>,
    fallback_beta: &str,
) -> Option<reqwest::header::HeaderMap> {
    let Some(request_headers) = request_headers else {
        return None;
    };
    let native_user_agent = request_headers
        .get(reqwest::header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim().starts_with("claude-cli/"));
    let native_body = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .is_some_and(|value| claude_code_system_is_native(&value));
    if !native_user_agent || !native_body {
        return None;
    }

    let mut output = reqwest::header::HeaderMap::new();
    for name in [
        "accept",
        "accept-encoding",
        "user-agent",
        "anthropic-dangerous-direct-browser-access",
        "anthropic-version",
        "x-app",
        "x-anthropic-additional-protection",
        "x-client-request-id",
        "x-client-app",
        "x-auth-nonce",
        "x-claude-code-session-id",
        "x-claude-remote-container-id",
        "x-claude-remote-session-id",
        "x-stainless-lang",
        "x-stainless-package-version",
        "x-stainless-os",
        "x-stainless-arch",
        "x-stainless-runtime",
        "x-stainless-runtime-version",
        "x-stainless-retry-count",
        "x-stainless-timeout",
        "x-stainless-read-timeout",
        "x-stainless-helper-method",
    ] {
        if let Some(value) = request_headers.get(name) {
            output.insert(
                reqwest::header::HeaderName::from_static(name),
                value.clone(),
            );
        }
    }

    let mut betas = caller_claude_betas(Some(request_headers));
    if !betas.is_empty() {
        push_claude_beta(&mut betas, "oauth-2025-04-20");
        if claude_request_uses_server_side_compaction(body) {
            push_claude_beta(&mut betas, CLAUDE_SERVER_SIDE_COMPACTION_BETA);
        }
        if let Ok(value) = reqwest::header::HeaderValue::from_str(&betas.join(",")) {
            output.insert("anthropic-beta", value);
        }
    }
    if !output.contains_key("anthropic-beta") {
        if let Ok(value) = reqwest::header::HeaderValue::from_str(fallback_beta) {
            output.insert("anthropic-beta", value);
        }
    }
    if !output.contains_key("anthropic-version") {
        output.insert(
            "anthropic-version",
            reqwest::header::HeaderValue::from_static("2023-06-01"),
        );
    }
    Some(output)
}

fn push_claude_beta(betas: &mut Vec<String>, beta: &str) {
    if !betas
        .iter()
        .any(|existing| existing.eq_ignore_ascii_case(beta))
    {
        betas.push(beta.to_string());
    }
}

fn caller_claude_betas(headers: Option<&reqwest::header::HeaderMap>) -> Vec<String> {
    let mut betas = Vec::new();
    if let Some(headers) = headers {
        for value in headers
            .get_all("anthropic-beta")
            .iter()
            .filter_map(|value| value.to_str().ok())
        {
            for beta in value
                .split(',')
                .map(str::trim)
                .filter(|beta| !beta.is_empty())
            {
                push_claude_beta(&mut betas, beta);
            }
        }
    }
    betas
}

pub(crate) fn claude_request_uses_server_side_compaction(body: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    let compaction_edit = value
        .pointer("/context_management/edits")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|edits| {
            edits.iter().any(|edit| {
                edit.get("type")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|kind| kind.eq_ignore_ascii_case("compact_20260112"))
            })
        });
    if compaction_edit {
        return true;
    }
    value
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|message| message.get("content").and_then(serde_json::Value::as_array))
        .flatten()
        .any(|block| {
            block
                .get("type")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|kind| kind.eq_ignore_ascii_case("compaction"))
        })
}

#[cfg(test)]
pub(crate) fn claude_subscription_request(
    client: &Client,
    url: &str,
    token: &str,
    body: String,
) -> reqwest::RequestBuilder {
    claude_subscription_request_with_headers(client, url, token, body, None)
}

fn claude_subscription_request_with_headers(
    client: &Client,
    url: &str,
    token: &str,
    body: String,
    request_headers: Option<&reqwest::header::HeaderMap>,
) -> reqwest::RequestBuilder {
    let session_id = claude_code_session_id_from_body(&body);
    let stream = !is_anthropic_count_tokens_path(url)
        && top_level_json_bool(body.as_bytes(), "stream").unwrap_or(false);
    let mut beta_header = if is_anthropic_count_tokens_path(url) {
        CLAUDE_COUNT_TOKENS_BETA_HEADER.to_string()
    } else {
        CLAUDE_CODE_BETA_HEADER.to_string()
    };
    if !is_anthropic_count_tokens_path(url)
        && top_level_json_string(body.as_bytes(), "speed")
            .is_some_and(|speed| speed.eq_ignore_ascii_case("fast"))
        && !beta_header.contains("fast-mode-2026-02-01")
    {
        beta_header.push_str(",fast-mode-2026-02-01");
    }
    if claude_request_uses_server_side_compaction(&body)
        && !beta_header.split(',').any(|beta| {
            beta.trim()
                .eq_ignore_ascii_case(CLAUDE_SERVER_SIDE_COMPACTION_BETA)
        })
    {
        beta_header.push(',');
        beta_header.push_str(CLAUDE_SERVER_SIDE_COMPACTION_BETA);
    }
    // Feature opt-ins are protocol data, not client identity. Preserve explicit
    // caller betas (including repeated header lines) without copying its UA or
    // credentials. Keep endpoint-specific defaults and deduplicate stably.
    let mut betas = beta_header
        .split(',')
        .map(str::to_string)
        .collect::<Vec<_>>();
    for beta in caller_claude_betas(request_headers) {
        push_claude_beta(&mut betas, &beta);
    }
    beta_header = betas.join(",");
    let native_headers = native_claude_code_headers(&body, request_headers, &beta_header);
    let accept = if stream {
        "text/event-stream"
    } else {
        "application/json"
    };
    let mut request = client
        .post(url)
        .header("Accept", accept)
        .header("Accept-Encoding", CLAUDE_RESPONSE_ACCEPT_ENCODING)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {token}"))
        .body(body);
    if let Some(native_headers) = native_headers {
        // Do not mix a real Claude Code identity with the compatibility
        // client's synthetic Stainless/OS/runtime fingerprint.
        return request.headers(native_headers);
    }
    request = request
        .header("anthropic-version", "2023-06-01")
        .header("anthropic-beta", beta_header)
        .header("User-Agent", CLAUDE_CODE_USER_AGENT)
        .header("X-App", "cli")
        .header("X-Stainless-Lang", "js")
        .header(
            "X-Stainless-Package-Version",
            CLAUDE_CODE_STAINLESS_PACKAGE_VERSION,
        )
        .header("X-Stainless-OS", claude_code_stainless_os())
        .header("X-Stainless-Arch", claude_code_stainless_arch())
        .header("X-Stainless-Runtime", "node")
        .header(
            "X-Stainless-Runtime-Version",
            CLAUDE_CODE_STAINLESS_RUNTIME_VERSION,
        )
        .header("X-Stainless-Retry-Count", "0")
        .header("X-Stainless-Timeout", "600")
        .header(
            "Anthropic-Dangerous-Direct-Browser-Access",
            CLAUDE_DIRECT_BROWSER_ACCESS,
        )
        .header("x-client-request-id", random_request_uuid())
        .header("Connection", "keep-alive");
    if stream {
        request = request.header("x-stainless-helper-method", "stream");
    }
    if let Some(session_id) = session_id {
        request = request.header("X-Claude-Code-Session-Id", session_id);
    }
    request
}

fn random_request_uuid() -> String {
    let mut bytes = rand::random::<u128>().to_be_bytes();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{}-{}-{}-{}-{}",
        hex::encode(&bytes[0..4]),
        hex::encode(&bytes[4..6]),
        hex::encode(&bytes[6..8]),
        hex::encode(&bytes[8..10]),
        hex::encode(&bytes[10..16])
    )
}

fn claude_code_session_id_from_body(body: &str) -> Option<String> {
    let body: serde_json::Value = serde_json::from_str(body).ok()?;
    let raw = body.pointer("/metadata/user_id")?.as_str()?.trim();
    let session_id = if raw.starts_with('{') {
        let metadata: serde_json::Value = serde_json::from_str(raw).ok()?;
        metadata
            .get("device_id")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())?;
        metadata
            .get("session_id")
            .and_then(serde_json::Value::as_str)?
            .trim()
            .to_string()
    } else {
        legacy_claude_code_session_id(raw)?.to_string()
    };
    if session_id.is_empty()
        || session_id.len() > 256
        || reqwest::header::HeaderValue::from_str(&session_id).is_err()
    {
        return None;
    }
    Some(session_id)
}

fn legacy_claude_code_session_id(raw: &str) -> Option<&str> {
    let rest = raw.strip_prefix("user_")?;
    let (device_id, rest) = rest.split_once("_account_")?;
    let (account_id, session_id) = rest.split_once("_session_")?;
    if device_id.len() != 64
        || !device_id.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !account_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
        || session_id.len() != 36
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
    {
        return None;
    }
    Some(session_id)
}
