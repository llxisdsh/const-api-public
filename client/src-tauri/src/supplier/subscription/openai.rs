fn is_codex_responses_lite_rejection(attempt: &OpenAiCompatResponse) -> bool {
    let Some(failure) = attempt.buffered_failure.as_ref() else {
        return false;
    };
    if failure.status != 400 {
        return false;
    }
    let lower = failure.raw.to_ascii_lowercase();
    lower.contains("x-openai-internal-codex-responses-lite")
        && (lower.contains("not supported") || lower.contains("unsupported"))
}

struct OpenAiLiteFallbackRequest<'a> {
    client: &'a Client,
    config: &'a SupplierConfig,
    channel: &'a ChannelConfig,
    url: &'a str,
    token: String,
    credential: serde_json::Value,
    endpoint_body: &'a str,
    token_url: &'a str,
    source_protocol: &'a str,
    target_protocol: &'a str,
    model: &'a str,
    provider: &'a str,
    cache_identity: Option<&'a str>,
    request_headers: Option<&'a reqwest::header::HeaderMap>,
}

async fn retry_openai_without_responses_lite_if_needed(
    attempt: OpenAiCompatResponse,
    fallback: OpenAiLiteFallbackRequest<'_>,
) -> Result<OpenAiCompatResponse> {
    if fallback.provider == "grok" || !is_codex_responses_lite_rejection(&attempt) {
        return Ok(attempt);
    }

    crate::detection::observe_openai_subscription_responses_lite_rejection(
        fallback.config,
        fallback.model,
    );
    let prepared = prepare_codex_subscription_endpoint_request_with_lite_override(
        fallback.config,
        fallback.endpoint_body,
        false,
        fallback.model,
        fallback.cache_identity,
        fallback.request_headers,
        Some(false),
    )?;
    let mut retry = send_openai_with_failure_capture(
        fallback.client,
        fallback.channel,
        fallback.url,
        fallback.token,
        fallback.credential,
        prepared.body,
        fallback.token_url,
        fallback.source_protocol,
        fallback.target_protocol,
        fallback.model,
        fallback.provider,
        fallback.request_headers,
    )
    .await?;
    let retry_succeeded = retry
        .response
        .as_ref()
        .is_some_and(|response| response.status().is_success());
    // The first 400 is recovered protocol evidence, not an unrecovered final
    // error. Replace its provider_request_rejected fault with one retry fact;
    // keep only faults produced while preparing or executing the final attempt.
    let mut faults = prepared.faults;
    faults.push(ImprovementFault::endpoint_retry(
        "codex_responses_lite_rejected_fallback",
        "openai",
        "chatgpt_codex_subscription",
        fallback.source_protocol,
        fallback.target_protocol,
        fallback.model,
        "$",
        400,
        retry_succeeded,
        "The model rejected the Responses Lite dialect before execution; CONST API retried once with the standard Responses dialect",
    ));
    faults.extend(retry.faults);
    retry.faults = faults;
    Ok(retry)
}

#[cfg(test)]
pub(crate) async fn forward_openai_subscription_stream_request(
    client: &Client,
    config: &SupplierConfig,
    msg_id: &str,
    path: &str,
    body: &str,
    outbound: &SupplierOutbound,
    upstream_override: Option<&str>,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>> {
    let cache_identity = subscription_body_cache_identity(body);
    forward_openai_subscription_stream_request_with_capacity(
        client,
        config,
        msg_id,
        path,
        body,
        outbound,
        upstream_override,
        None,
        cache_identity.as_deref(),
        None,
    )
    .await
}

pub(crate) async fn forward_openai_subscription_stream_request_with_capacity(
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
    let provider = subscription_provider_from_config(config);
    let grok = provider == "grok";
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
            "OpenAI account subscription could not identify the inbound protocol",
        );
        let _ = append_subscription_wire_audit(config, path, body, &payload, started_at.elapsed());
        return Ok(Some(payload));
    }
    let channel = channel_from_supplier("subscription-runtime".to_string(), config);
    let model = channel_upstream_model_for_request(
        &channel,
        requested_model_from_body_or_path(body, path).as_deref(),
    );
    let conversion_body = json_body_with_stream(body, false).unwrap_or_else(|_| body.to_string());
    let conversion = upstream_request_for_api_format_with_profiles_report(
        "openai_responses",
        path,
        &conversion_body,
        &model,
        &config.capability_profiles,
    )
    .map_err(request_conversion_failure)?;
    let mut improvement_faults = conversion.faults;
    let upstream_body = conversion.body;
    let inbound_protocol = conversion.inbound_protocol;
    let target_protocol = conversion.target_protocol;
    debug_assert_eq!(target_protocol, "openai_responses");
    let endpoint_body = json_body_with_stream(&upstream_body, true)?;
    let upstream_body = if grok {
        endpoint_body.clone()
    } else {
        let prepared = prepare_codex_subscription_endpoint_request(
            config,
            &endpoint_body,
            false,
            &model,
            cache_identity,
            request_headers,
        )?;
        improvement_faults.extend(prepared.faults);
        prepared.body
    };
    let (token, credential, _) = if grok {
        ensure_grok_subscription_access_token(client, &channel).await?
    } else {
        ensure_openai_subscription_access_token(client, &channel).await?
    };
    let url = upstream_override.map(str::to_string).unwrap_or_else(|| {
        if grok {
            GROK_RESPONSES_URL.to_string()
        } else {
            codex_responses_url()
        }
    });
    let upstream_send_started = std::time::Instant::now();
    let attempt = send_openai_with_failure_capture(
        client,
        &channel,
        &url,
        token.clone(),
        credential.clone(),
        upstream_body,
        if grok {
            GROK_OAUTH_TOKEN_URL
        } else {
            OPENAI_OAUTH_TOKEN_URL
        },
        &inbound_protocol,
        &target_protocol,
        &model,
        &provider,
        request_headers,
    )
    .await?;
    let attempt = retry_openai_without_responses_lite_if_needed(
        attempt,
        OpenAiLiteFallbackRequest {
            client,
            config,
            channel: &channel,
            url: &url,
            token,
            credential,
            endpoint_body: &endpoint_body,
            token_url: if grok {
                GROK_OAUTH_TOKEN_URL
            } else {
                OPENAI_OAUTH_TOKEN_URL
            },
            source_protocol: &inbound_protocol,
            target_protocol: &target_protocol,
            model: &model,
            provider: &provider,
            cache_identity,
            request_headers,
        },
    )
    .await?;
    let upstream_send_ms = elapsed_millis_u64(upstream_send_started);
    improvement_faults.extend(attempt.faults);
    let upstream_body_for_log = attempt.request_body;
    if let Some(failure) = attempt.buffered_failure {
        let mut payload = subscription_wire_response_payload_with_header_map(
            failure.status,
            &failure.content_type,
            failure.raw.clone(),
            &failure.headers,
            false,
        );
        attach_improvement_faults(&mut payload, &improvement_faults);
        let returned_body = subscription_wire_body_text(&payload);
        insert_subscription_exchange_metrics(
            &mut payload,
            body,
            &upstream_body_for_log,
            &failure.raw,
            &returned_body,
        );
        let _ = append_supplier_raw_exchange_log(
            config,
            &provider,
            path,
            &url,
            body,
            &upstream_body_for_log,
            failure.status,
            &failure.content_type,
            &failure.raw,
            &failure.content_type,
            &returned_body,
            None,
        );
        let _ = append_subscription_wire_audit(config, path, body, &payload, started_at.elapsed());
        return Ok(Some(payload));
    }
    let Some(resp) = attempt.response else {
        return Err(anyhow!(
            "OpenAI compatibility response contained neither a response nor a captured failure"
        ));
    };
    let status = resp.status().as_u16();
    let content_type = response_content_type(&resp);
    if !(200..300).contains(&status) {
        let headers = resp.headers().clone();
        let raw = resp.text().await?;
        let mut payload = subscription_wire_response_payload_with_header_map(
            status,
            &content_type,
            raw.clone(),
            &headers,
            false,
        );
        attach_improvement_faults(&mut payload, &improvement_faults);
        let returned_body = subscription_wire_body_text(&payload);
        insert_subscription_exchange_metrics(
            &mut payload,
            body,
            &upstream_body_for_log,
            &raw,
            &returned_body,
        );
        let _ = append_supplier_raw_exchange_log(
            config,
            &provider,
            path,
            &url,
            body,
            &upstream_body_for_log,
            status,
            &content_type,
            &raw,
            &content_type,
            &returned_body,
            None,
        );
        let _ = append_subscription_wire_audit(config, path, body, &payload, started_at.elapsed());
        return Ok(Some(payload));
    }
    let stream_headers = subscription_stream_headers(resp.headers());
    let mut started = false;
    let mut stream = SupplierByteStreamBatcher::new(resp.bytes_stream());
    let mut converter =
        inbound_sse_stream_converter(&inbound_protocol, "openai_responses", &model)?;
    let mut upstream_utf8 = crate::protocol::stream::Utf8ChunkDecoder::new();
    let mut converted_utf8 = crate::protocol::stream::Utf8ChunkDecoder::new();
    let mut grok_ping_filter = grok.then(GrokResponsesPingFilter::default);
    let mut terminal_raw = String::new();
    let mut converted_raw = SubscriptionOutputCapture::new(raw_debug_logs_enabled());
    let mut timed_out = false;
    let mut usable_output_produced = false;
    let mut upstream_usage = SupplierUpstreamUsageAccumulator::new("openai_responses");
    let mut continuation =
        (!grok).then(|| CodexHttpContinuationCollector::new(&upstream_body_for_log));
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
        if stream_debug.first_body_chunk_ms.is_none() {
            stream_debug.first_body_chunk_ms = Some(elapsed_millis_u64(body_read_started));
        }
        stream_debug.upstream_chunk_count += batch.source_chunks;
        stream_debug.upstream_response_bytes += chunk.len();
        stream_debug.largest_upstream_chunk_bytes = stream_debug
            .largest_upstream_chunk_bytes
            .max(batch.largest_source_chunk);
        if let Some(decoder) = timing_sse_decoder.as_mut() {
            if let Ok(frames) = decoder.push(&chunk) {
                for frame in frames {
                    let elapsed = elapsed_millis_u64(upstream_send_started);
                    stream_debug.first_sse_event_ms.get_or_insert(elapsed);
                    if responses_sse_event_is_meaningful(&frame.data) {
                        stream_debug.first_meaningful_event_ms = Some(elapsed);
                        timing_sse_decoder = None;
                        break;
                    }
                }
            }
        }
        upstream_usage.push(&chunk);
        terminal_raw.push_str(&upstream_utf8.push(&chunk)?);
        if let Some(continuation) = continuation.as_mut() {
            let _ = continuation.push(config, &chunk);
        }
        let (converter_input, has_payload) = if let Some(filter) = grok_ping_filter.as_mut() {
            filter.push(&chunk)
        } else {
            (chunk.to_vec(), true)
        };
        if converter_input.is_empty() {
            continue;
        }
        let converted = converter.push(&converter_input);
        let conversion_failed = converter.failure().is_some();
        if conversion_failed && !started {
            break;
        }
        if converted.is_empty() {
            continue;
        }
        let text = converted_utf8.push(&converted)?;
        if text.is_empty() {
            continue;
        }
        if !conversion_failed && has_payload {
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
    if let Some(continuation) = continuation.as_mut() {
        let _ = continuation.finish(config);
    }
    if let Some(filter) = grok_ping_filter.as_mut() {
        let (filtered_tail, has_payload) = filter.finish();
        if !filtered_tail.is_empty() {
            let converted = converter.push(&filtered_tail);
            let conversion_failed = converter.failure().is_some();
            if !converted.is_empty() {
                let text = converted_utf8.push(&converted)?;
                if !text.is_empty() {
                    if !conversion_failed && has_payload {
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
                }
            }
        }
    }
    let tail = converter.finish();
    let conversion_failed = converter.failure().is_some();
    let mut tail_text = String::new();
    if !conversion_failed || started {
        tail_text = converted_utf8.push(&tail)?;
        tail_text.push_str(&converted_utf8.finish()?);
    }
    if !tail_text.is_empty() {
        if !conversion_failed {
            usable_output_produced = true;
        }
        converted_raw.push_str(&tail_text);
        send_subscription_stream_chunk(
            outbound,
            msg_id,
            status,
            &stream_headers,
            &mut started,
            &improvement_faults,
            tail_text.as_bytes(),
        )
        .await?;
    }
    let execution_error =
        subscription_stream_execution_error(&converter, timed_out, started, usable_output_produced);
    improvement_faults.extend(improvement_faults_from_stream_notices(
        converter.take_notices(),
        "openai_responses",
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
        &upstream_body_for_log,
        &terminal_raw,
        converted_raw.len(),
    );
    insert_subscription_stream_debug_metrics(&mut payload, &stream_debug);
    let _ = append_supplier_raw_exchange_log(
        config,
        &provider,
        path,
        &url,
        body,
        &upstream_body_for_log,
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

#[cfg(test)]
pub(crate) async fn forward_openai_subscription_buffered_request(
    client: &Client,
    config: &SupplierConfig,
    path: &str,
    body: &str,
    stream_response: bool,
    upstream_override: Option<&str>,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    let cache_identity = subscription_body_cache_identity(body);
    forward_openai_subscription_buffered_request_with_capacity(
        client,
        config,
        path,
        body,
        stream_response,
        upstream_override,
        None,
        cache_identity.as_deref(),
        None,
    )
    .await
}

pub(crate) async fn forward_openai_subscription_buffered_request_with_capacity(
    client: &Client,
    config: &SupplierConfig,
    path: &str,
    body: &str,
    stream_response: bool,
    upstream_override: Option<&str>,
    capacity: Option<SubscriptionRequestCapacity>,
    cache_identity: Option<&str>,
    request_headers: Option<&reqwest::header::HeaderMap>,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    let started_at = std::time::Instant::now();
    let provider = subscription_provider_from_config(config);
    let grok = provider == "grok";
    let _safety_guard = match subscription_safety_enter_with_capacity(
        config,
        body,
        capacity.unwrap_or_else(|| SubscriptionRequestCapacity::for_path(path)),
    ) {
        Ok(guard) => guard,
        Err(payload) => {
            let _ =
                append_subscription_wire_audit(config, path, body, &payload, started_at.elapsed());
            return Ok(payload);
        }
    };
    if subscription_inbound_protocol(path).is_none() {
        let payload = subscription_wire_error_response(
            501,
            "subscription_stream_not_supported",
            "OpenAI account subscription could not identify the inbound protocol",
        );
        let _ = append_subscription_wire_audit(config, path, body, &payload, started_at.elapsed());
        return Ok(payload);
    }
    let channel = channel_from_supplier("subscription-runtime".to_string(), config);
    let model = channel_upstream_model_for_request(
        &channel,
        requested_model_from_body_or_path(body, path).as_deref(),
    );
    let compact = !grok && is_openai_responses_compact_path(path);
    let conversion_body = if compact {
        json_body_without_stream(body).unwrap_or_else(|_| body.to_string())
    } else {
        json_body_with_stream(body, false).unwrap_or_else(|_| body.to_string())
    };
    let conversion = upstream_request_for_api_format_with_profiles_report(
        "openai_responses",
        path,
        &conversion_body,
        &model,
        &config.capability_profiles,
    )
    .map_err(request_conversion_failure)?;
    let mut improvement_faults = conversion.faults;
    let upstream_body = conversion.body;
    let inbound_protocol = conversion.inbound_protocol;
    let target_protocol = conversion.target_protocol;
    debug_assert_eq!(target_protocol, "openai_responses");
    let endpoint_body = if compact {
        json_body_without_stream(&upstream_body)?
    } else {
        json_body_with_stream(&upstream_body, true)?
    };
    let upstream_body = if grok {
        endpoint_body.clone()
    } else {
        let prepared = prepare_codex_subscription_endpoint_request(
            config,
            &endpoint_body,
            compact,
            &model,
            cache_identity,
            request_headers,
        )?;
        improvement_faults.extend(prepared.faults);
        prepared.body
    };
    let (token, credential, _) = if grok {
        ensure_grok_subscription_access_token(client, &channel).await?
    } else {
        ensure_openai_subscription_access_token(client, &channel).await?
    };
    let base_url = upstream_override.map(str::to_string).unwrap_or_else(|| {
        if grok {
            GROK_RESPONSES_URL.to_string()
        } else {
            codex_responses_url()
        }
    });
    let url = openai_subscription_operation_url(base_url, compact);
    let attempt = send_openai_with_failure_capture(
        client,
        &channel,
        &url,
        token.clone(),
        credential.clone(),
        upstream_body,
        if grok {
            GROK_OAUTH_TOKEN_URL
        } else {
            OPENAI_OAUTH_TOKEN_URL
        },
        &inbound_protocol,
        &target_protocol,
        &model,
        &provider,
        request_headers,
    )
    .await?;
    let attempt = retry_openai_without_responses_lite_if_needed(
        attempt,
        OpenAiLiteFallbackRequest {
            client,
            config,
            channel: &channel,
            url: &url,
            token,
            credential,
            endpoint_body: &endpoint_body,
            token_url: if grok {
                GROK_OAUTH_TOKEN_URL
            } else {
                OPENAI_OAUTH_TOKEN_URL
            },
            source_protocol: &inbound_protocol,
            target_protocol: &target_protocol,
            model: &model,
            provider: &provider,
            cache_identity,
            request_headers,
        },
    )
    .await?;
    improvement_faults.extend(attempt.faults);
    let upstream_body_for_log = attempt.request_body;
    if let Some(failure) = attempt.buffered_failure {
        let mut payload = subscription_wire_response_payload_with_header_map(
            failure.status,
            &failure.content_type,
            failure.raw.clone(),
            &failure.headers,
            false,
        );
        attach_improvement_faults(&mut payload, &improvement_faults);
        let returned_body = subscription_wire_body_text(&payload);
        insert_subscription_exchange_metrics(
            &mut payload,
            body,
            &upstream_body_for_log,
            &failure.raw,
            &returned_body,
        );
        let _ = append_supplier_raw_exchange_log(
            config,
            &provider,
            path,
            &url,
            body,
            &upstream_body_for_log,
            failure.status,
            &failure.content_type,
            &failure.raw,
            &failure.content_type,
            &returned_body,
            None,
        );
        let _ = append_subscription_wire_audit(config, path, body, &payload, started_at.elapsed());
        return Ok(payload);
    }
    let Some(resp) = attempt.response else {
        return Err(anyhow!(
            "OpenAI compatibility response contained neither a response nor a captured failure"
        ));
    };
    let status = resp.status().as_u16();
    let content_type = response_content_type(&resp);
    let response_headers = resp.headers().clone();
    if !(200..300).contains(&status) {
        let raw = resp.text().await?;
        let mut payload = subscription_wire_response_payload_with_header_map(
            status,
            &content_type,
            raw.clone(),
            &response_headers,
            false,
        );
        attach_improvement_faults(&mut payload, &improvement_faults);
        let returned_body = subscription_wire_body_text(&payload);
        insert_subscription_exchange_metrics(
            &mut payload,
            body,
            &upstream_body_for_log,
            &raw,
            &returned_body,
        );
        let _ = append_supplier_raw_exchange_log(
            config,
            &provider,
            path,
            &url,
            body,
            &upstream_body_for_log,
            status,
            &content_type,
            &returned_body,
            &content_type,
            &returned_body,
            None,
        );
        let _ = append_subscription_wire_audit(config, path, body, &payload, started_at.elapsed());
        return Ok(payload);
    }

    let raw = crate::sse_buffer::read_sse_body_with_limits(resp).await?;
    let upstream_usage = upstream_usage_from_response("openai_responses", raw.as_bytes());
    if !content_type.contains("event-stream") && !looks_like_sse_body(&raw) {
        if !grok {
            let _ =
                capture_codex_http_subscription_continuation(config, &upstream_body_for_log, &raw);
        }
        let mut payload = subscription_wire_response_payload_with_header_map(
            status,
            &content_type,
            raw.clone(),
            &response_headers,
            false,
        );
        attach_improvement_faults(&mut payload, &improvement_faults);
        attach_upstream_usage(&mut payload, upstream_usage.as_ref());
        let returned_body = subscription_wire_body_text(&payload);
        insert_subscription_exchange_metrics(
            &mut payload,
            body,
            &upstream_body_for_log,
            &raw,
            &returned_body,
        );
        let _ = append_supplier_raw_exchange_log(
            config,
            &provider,
            path,
            &url,
            body,
            &upstream_body_for_log,
            status,
            &content_type,
            &raw,
            &content_type,
            &returned_body,
            None,
        );
        let _ = append_subscription_wire_audit(config, path, body, &payload, started_at.elapsed());
        return Ok(payload);
    }
    if let Some(mut payload) = subscription_terminal_wire_error_payload(&raw) {
        attach_improvement_faults(&mut payload, &improvement_faults);
        attach_upstream_usage(&mut payload, upstream_usage.as_ref());
        let returned_body = subscription_wire_body_text(&payload);
        insert_subscription_exchange_metrics(
            &mut payload,
            body,
            &upstream_body_for_log,
            &raw,
            &returned_body,
        );
        let _ = append_supplier_raw_exchange_log(
            config,
            &provider,
            path,
            &url,
            body,
            &upstream_body_for_log,
            status,
            &content_type,
            &raw,
            &content_type,
            &returned_body,
            None,
        );
        let _ = append_subscription_wire_audit(config, path, body, &payload, started_at.elapsed());
        return Ok(payload);
    }
    if !grok {
        let _ = capture_codex_http_subscription_continuation(config, &upstream_body_for_log, &raw);
    }
    let response_raw = if grok {
        filter_grok_responses_sse(&raw)
    } else {
        raw.clone()
    };
    let response_body = if stream_response {
        if inbound_protocol == target_protocol {
            response_raw.clone()
        } else {
            let converted = canonical_stream_events_from_target_sse_with_report(
                &response_raw,
                &inbound_protocol,
                &target_protocol,
                &model,
            )?;
            improvement_faults.extend(converted.faults);
            sse_from_canonical_stream_events(&converted.events, &inbound_protocol, &model)?
        }
    } else if inbound_protocol == target_protocol && target_protocol == "openai_responses" {
        match openai_responses_completed_body_from_sse(&response_raw) {
            Ok(body) => body,
            Err(error) => {
                eprintln!(
                    "[const-api][protocol] native Responses stream lacks a complete response body; rebuilding from events: {error}"
                );
                let converted = sse_body_from_target_as_inbound_with_report(
                    &response_raw,
                    &inbound_protocol,
                    &target_protocol,
                    &model,
                )?;
                improvement_faults.extend(converted.faults);
                converted.body
            }
        }
    } else {
        let converted = sse_body_from_target_as_inbound_with_report(
            &response_raw,
            &inbound_protocol,
            &target_protocol,
            &model,
        )?;
        improvement_faults.extend(converted.faults);
        converted.body
    };
    let response_content_type = if stream_response {
        "text/event-stream"
    } else {
        "application/json"
    };
    let returned_body = response_body.clone();
    let mut payload = subscription_wire_response_payload_with_header_map(
        status,
        response_content_type,
        response_body,
        &response_headers,
        true,
    );
    attach_improvement_faults(&mut payload, &improvement_faults);
    attach_upstream_usage(&mut payload, upstream_usage.as_ref());
    insert_subscription_exchange_metrics(
        &mut payload,
        body,
        &upstream_body_for_log,
        &raw,
        &returned_body,
    );
    let _ = append_supplier_raw_exchange_log(
        config,
        &provider,
        path,
        &url,
        body,
        &upstream_body_for_log,
        status,
        &content_type,
        &raw,
        response_content_type,
        &subscription_wire_body_text(&payload),
        None,
    );
    let _ = append_subscription_wire_audit(config, path, body, &payload, started_at.elapsed());
    Ok(payload)
}

#[cfg(test)]
mod openai_subscription_recovery_tests {
    use super::*;

    fn failure(status: u16, raw: &str) -> OpenAiCompatResponse {
        OpenAiCompatResponse {
            response: None,
            buffered_failure: Some(BufferedUpstreamFailure {
                status,
                content_type: "application/json".to_string(),
                headers: reqwest::header::HeaderMap::new(),
                raw: raw.to_string(),
            }),
            request_body: "{}".to_string(),
            faults: Vec::new(),
        }
    }

    #[test]
    fn recognizes_only_the_pre_execution_responses_lite_rejection() {
        assert!(is_codex_responses_lite_rejection(&failure(
            400,
            r#"{"error":{"message":"This model is not supported when using X-OpenAI-Internal-Codex-Responses-Lite."}}"#,
        )));
        assert!(!is_codex_responses_lite_rejection(&failure(
            400,
            r#"{"error":{"message":"Unsupported parameter: temperature"}}"#,
        )));
        assert!(!is_codex_responses_lite_rejection(&failure(
            429,
            "X-OpenAI-Internal-Codex-Responses-Lite is not supported",
        )));
    }
}
