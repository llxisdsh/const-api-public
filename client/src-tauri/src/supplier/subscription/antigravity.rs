#[cfg(test)]
pub(crate) async fn forward_antigravity_subscription_request(
    client: &Client,
    channel: &ChannelConfig,
    path: &str,
    body: &str,
    upstream_override: Option<&str>,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    forward_antigravity_subscription_request_with_cache_identity(
        client,
        channel,
        path,
        body,
        upstream_override,
        None,
    )
    .await
}

async fn forward_antigravity_subscription_request_with_cache_identity(
    client: &Client,
    channel: &ChannelConfig,
    path: &str,
    body: &str,
    upstream_override: Option<&str>,
    cache_identity: Option<&str>,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    let requested_model = channel_upstream_model_for_request(
        channel,
        requested_model_from_body_or_path(body, path).as_deref(),
    );
    let model = crate::antigravity_models::canonical_model_id(&requested_model);
    if subscription_inbound_protocol(path).is_none() {
        return Ok(subscription_wire_error_response(
            404,
            "subscription_path_not_supported",
            &format!("Antigravity subscription could not identify the inbound protocol: {path}"),
        ));
    }
    let (token, credential, _) =
        ensure_antigravity_subscription_access_token(client, channel).await?;
    let project_id = json_string(&credential, &["project_id"])
        .or_else(|| json_string(&credential, &["cloudaicompanionProject"]))
        .unwrap_or_default();
    let count_tokens = is_gemini_count_tokens_path(path);
    if project_id.is_empty() && !count_tokens {
        anyhow::bail!(
            "Antigravity credential is missing project_id; detect the channel again or re-authorize it"
        );
    }
    let conversion_body = if count_tokens {
        json_body_without_stream(body).unwrap_or_else(|_| body.to_string())
    } else {
        json_body_with_stream(body, false).unwrap_or_else(|_| body.to_string())
    };
    let conversion = upstream_request_for_api_format_with_profiles_report(
        "gemini_native",
        path,
        &conversion_body,
        &model,
        &channel.capability_profiles,
    )
    .map_err(request_conversion_failure)?;
    let mut improvement_faults = conversion.faults;
    let tool_mapping = conversion.tool_mapping;
    if is_gemini_embed_content_path(path) {
        let summary = "Antigravity account subscription does not expose a native embedContent transport; generation compatibility was attempted";
        log::debug!(
            "[const-api][subscription] endpoint adjustment endpoint=antigravity code=special_operation_approximated path={path}"
        );
        improvement_faults.push(ImprovementFault::endpoint_adjustment(
            "special_operation_approximated",
            "google",
            "antigravity_subscription",
            "gemini_native",
            "gemini_native",
            &model,
            path,
            summary,
        ));
    }
    let native_body = conversion.body;
    let inbound_protocol = conversion.inbound_protocol;
    let target_protocol = conversion.target_protocol;
    let prepared = crate::antigravity_models::prepare_subscription_request(
        &credential,
        &requested_model,
        body,
        &native_body,
    )?;
    let model = prepared.canonical_model;
    let upstream_model = prepared.upstream_model;
    let native_body = prepared.native_body;
    let upstream_body = if count_tokens {
        let mut native: serde_json::Value = serde_json::from_str(&native_body)?;
        if let Some(object) = native.as_object_mut() {
            if object.remove("safetySettings").is_some() {
                let summary = "Antigravity countTokens transport does not accept safetySettings; the field was omitted so token counting could continue";
                log::debug!(
                    "[const-api][subscription] endpoint adjustment endpoint=antigravity_count_tokens code=gemini_count_tokens_safety_settings_omitted path=$.safetySettings"
                );
                improvement_faults.push(ImprovementFault::endpoint_adjustment(
                    "gemini_count_tokens_safety_settings_omitted",
                    "google",
                    "antigravity_count_tokens",
                    "gemini_native",
                    "gemini_native",
                    &model,
                    "$.safetySettings",
                    summary,
                ));
            }
            object.remove("stream");
        }
        serde_json::json!({"request": native}).to_string()
    } else {
        gemini_native_body_to_antigravity_body_with_cache_identity(
            &native_body,
            &upstream_model,
            &project_id,
            cache_identity,
        )?
    };
    let urls = if let Some(override_url) = upstream_override {
        vec![antigravity_subscription_operation_url(
            override_url.to_string(),
            count_tokens,
        )]
    } else {
        antigravity_runtime_urls(
            None,
            if count_tokens {
                "countTokens"
            } else {
                "generateContent"
            },
        )
    };
    let (status, content_type, raw, headers, url) = send_antigravity_body_with_fallback(
        client,
        channel,
        &urls,
        "application/json",
        token.clone(),
        upstream_body.clone(),
        ANTIGRAVITY_OAUTH_TOKEN_URL,
    )
    .await?;
    if let Some(fault) = ImprovementFault::unrecovered_provider_rejection(
        "antigravity",
        &url,
        &inbound_protocol,
        &target_protocol,
        &model,
        status,
        &raw,
    ) {
        improvement_faults.push(fault);
    }
    let upstream_usage = upstream_usage_from_response("gemini_native", raw.as_bytes());
    let response_body = if (200..300).contains(&status) && count_tokens {
        raw
    } else if (200..300).contains(&status) {
        let native_raw = antigravity_body_to_native_body(&raw).unwrap_or_else(|| raw.clone());
        let native_raw =
            crate::antigravity_models::canonicalize_native_response_body(&native_raw, &model)
                .unwrap_or(native_raw);
        let converted = response_body_from_target_as_inbound_with_report_with_tools(
            &native_raw,
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
    let mut payload = subscription_wire_response_payload_with_owned_headers(
        status,
        &content_type,
        response_body,
        &headers,
        (200..300).contains(&status) && !count_tokens,
    );
    attach_improvement_faults(&mut payload, &improvement_faults);
    attach_upstream_usage(&mut payload, upstream_usage.as_ref());
    Ok(payload)
}

fn antigravity_body_to_native_body(raw: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    value
        .get("response")
        .cloned()
        .and_then(|response| serde_json::to_string(&response).ok())
}

pub(crate) async fn send_antigravity_body(
    client: &Client,
    channel: &ChannelConfig,
    url: &str,
    accept: &str,
    access_token: String,
    body: String,
    token_url: &str,
) -> Result<(u16, String, String, Vec<(String, String)>)> {
    let request = client
        .post(url)
        .header("Accept", accept)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {access_token}"))
        .header("User-Agent", antigravity_user_agent())
        .body(body.clone());
    let resp = crate::upstream_transport::send(request).await?;
    let status = resp.status().as_u16();
    let content_type = response_content_type(&resp);
    let headers = response_header_pairs(resp.headers());
    let raw = resp.text().await?;
    if status != 401 {
        return Ok((status, content_type, raw, headers));
    }

    let (fresh_token, _, _) =
        ensure_antigravity_subscription_access_token_with_options(client, channel, true, token_url)
            .await?;
    let request = client
        .post(url)
        .header("Accept", accept)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {fresh_token}"))
        .header("User-Agent", antigravity_user_agent())
        .body(body);
    let retry = crate::upstream_transport::send(request).await?;
    let retry_status = retry.status().as_u16();
    let retry_content_type = response_content_type(&retry);
    let retry_headers = response_header_pairs(retry.headers());
    let retry_raw = retry.text().await?;
    Ok((retry_status, retry_content_type, retry_raw, retry_headers))
}

pub(crate) async fn send_antigravity_body_with_fallback(
    client: &Client,
    channel: &ChannelConfig,
    urls: &[String],
    accept: &str,
    access_token: String,
    body: String,
    token_url: &str,
) -> Result<(u16, String, String, Vec<(String, String)>, String)> {
    if urls.is_empty() {
        anyhow::bail!("Antigravity runtime endpoint list is empty");
    }

    let mut last_transport_error = None;
    for (index, url) in urls.iter().enumerate() {
        match send_antigravity_body(
            client,
            channel,
            url,
            accept,
            access_token.clone(),
            body.clone(),
            token_url,
        )
        .await
        {
            Ok((status, content_type, raw, headers)) => {
                let has_fallback = index + 1 < urls.len();
                if has_fallback && antigravity_runtime_should_fallback(status) {
                    continue;
                }
                return Ok((status, content_type, raw, headers, url.clone()));
            }
            Err(error) if crate::upstream_transport::is_ambiguous_transport_error(&error) => {
                return Err(error);
            }
            Err(error) if index + 1 < urls.len() => {
                last_transport_error = Some(error);
            }
            Err(error) => return Err(error),
        }
    }

    Err(last_transport_error
        .unwrap_or_else(|| anyhow::anyhow!("all Antigravity runtime endpoints failed")))
}

pub(crate) async fn send_antigravity_stream_with_fallback(
    client: &Client,
    channel: &ChannelConfig,
    urls: &[String],
    access_token: String,
    body: String,
) -> Result<(reqwest::Response, String, String)> {
    if urls.is_empty() {
        anyhow::bail!("Antigravity runtime endpoint list is empty");
    }

    let mut active_token = access_token;
    let mut last_transport_error = None;
    for (index, url) in urls.iter().enumerate() {
        let send = |token: &str| {
            crate::upstream_transport::send(
                client
                    .post(url)
                    .header("Accept", "text/event-stream, application/json")
                    .header("Content-Type", "application/json")
                    .header("Authorization", format!("Bearer {token}"))
                    .header("User-Agent", antigravity_user_agent())
                    .body(body.clone()),
            )
        };
        let mut response = match send(&active_token).await {
            Ok(response) => response,
            Err(error) if error.is_ambiguous() => return Err(error.into()),
            Err(error) if index + 1 < urls.len() => {
                last_transport_error = Some(error.into());
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            let _ = response.text().await;
            let (fresh_token, _, _) =
                force_refresh_antigravity_subscription_access_token(client, channel).await?;
            active_token = fresh_token;
            response = match send(&active_token).await {
                Ok(response) => response,
                Err(error) if error.is_ambiguous() => return Err(error.into()),
                Err(error) if index + 1 < urls.len() => {
                    last_transport_error = Some(error.into());
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
        }
        let status = response.status().as_u16();
        if index + 1 < urls.len() && antigravity_runtime_should_fallback(status) {
            let _ = response.text().await;
            continue;
        }
        return Ok((response, active_token, url.clone()));
    }

    Err(last_transport_error
        .unwrap_or_else(|| anyhow::anyhow!("all Antigravity streaming endpoints failed")))
}

#[cfg(test)]
pub(crate) async fn forward_antigravity_subscription_stream_request(
    client: &Client,
    config: &SupplierConfig,
    msg_id: &str,
    path: &str,
    body: &str,
    outbound: &SupplierOutbound,
    upstream_override: Option<&str>,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>> {
    forward_antigravity_subscription_stream_request_with_capacity(
        client,
        config,
        msg_id,
        path,
        body,
        outbound,
        upstream_override,
        None,
        None,
    )
    .await
}

pub(crate) async fn forward_antigravity_subscription_stream_request_with_capacity(
    client: &Client,
    config: &SupplierConfig,
    msg_id: &str,
    path: &str,
    body: &str,
    outbound: &SupplierOutbound,
    upstream_override: Option<&str>,
    capacity: Option<SubscriptionRequestCapacity>,
    cache_identity: Option<&str>,
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
    let channel = channel_from_supplier("subscription-runtime".to_string(), config);
    let requested_model = channel_upstream_model_for_request(
        &channel,
        requested_model_from_body_or_path(body, path).as_deref(),
    );
    let model = crate::antigravity_models::canonical_model_id(&requested_model);
    if subscription_inbound_protocol(path).is_none() {
        let payload = subscription_wire_error_response(
            404,
            "subscription_path_not_supported",
            &format!(
                "Antigravity subscription could not identify the inbound streaming protocol: {path}"
            ),
        );
        let _ = append_subscription_wire_audit(config, path, body, &payload, started_at.elapsed());
        return Ok(Some(payload));
    }
    let (token, credential, _) =
        ensure_antigravity_subscription_access_token(client, &channel).await?;
    let project_id = json_string(&credential, &["project_id"])
        .or_else(|| json_string(&credential, &["cloudaicompanionProject"]))
        .unwrap_or_default();
    if project_id.is_empty() {
        anyhow::bail!(
            "Antigravity credential is missing project_id; detect the channel again or re-authorize it"
        );
    }
    let conversion_body = json_body_with_stream(body, false).unwrap_or_else(|_| body.to_string());
    let conversion = upstream_request_for_api_format_with_profiles_report(
        "gemini_native",
        path,
        &conversion_body,
        &model,
        &config.capability_profiles,
    )
    .map_err(request_conversion_failure)?;
    let mut improvement_faults = conversion.faults;
    let tool_mapping = conversion.tool_mapping;
    let native_body = conversion.body;
    let inbound_protocol = conversion.inbound_protocol;
    let prepared = crate::antigravity_models::prepare_subscription_request(
        &credential,
        &requested_model,
        body,
        &native_body,
    )?;
    let model = prepared.canonical_model;
    let upstream_model = prepared.upstream_model;
    let native_body = prepared.native_body;
    let upstream_body = gemini_native_body_to_antigravity_body_with_cache_identity(
        &native_body,
        &upstream_model,
        &project_id,
        cache_identity,
    )?;
    let urls = antigravity_runtime_urls(upstream_override, "streamGenerateContent");
    let upstream_send_started = std::time::Instant::now();
    let (resp, _active_token, url) = send_antigravity_stream_with_fallback(
        client,
        &channel,
        &urls,
        token,
        upstream_body.clone(),
    )
    .await?;
    let upstream_send_ms = elapsed_millis_u64(upstream_send_started);
    if resp.status() == reqwest::StatusCode::BAD_REQUEST {
        let original_headers = resp.headers().clone();
        let original_content_type = response_content_type(&resp);
        let original_raw = resp.text().await?;
        if let Some(fault) = ImprovementFault::unrecovered_provider_rejection(
            "antigravity",
            &url,
            &inbound_protocol,
            "gemini_native",
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
    if !(200..300).contains(&status) || !content_type.contains("event-stream") {
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
    let mut antigravity = AntigravitySseConverter::with_canonical_model(&model);
    let mut converter = inbound_sse_stream_converter(&inbound_protocol, "gemini_native", &model)?.with_tool_mapping(tool_mapping);
    let mut upstream_utf8 = crate::protocol::stream::Utf8ChunkDecoder::new();
    let mut converted_utf8 = crate::protocol::stream::Utf8ChunkDecoder::new();
    let mut terminal_raw = String::new();
    // This path has no raw-exchange writer; retain only the byte count.
    let mut converted_raw = SubscriptionOutputCapture::new(false);
    let mut timed_out = false;
    let mut usable_output_produced = false;
    let mut upstream_usage = SupplierUpstreamUsageAccumulator::new("gemini_native");
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
                    "gemini_native",
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
        let text = upstream_utf8.push(&chunk)?;
        terminal_raw.push_str(&text);
        let native = antigravity.push(&text);
        let converted = converter.push(native.as_bytes());
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
    let native_tail = antigravity.finish();
    let mut converted = converter.push(native_tail.as_bytes()).to_vec();
    converted.extend_from_slice(&converter.finish());
    let conversion_failed = converter.failure().is_some();
    let mut tail = String::new();
    if !conversion_failed || started {
        tail = converted_utf8.push(&converted)?;
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
        "gemini_native",
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
    // Gemini streaming is selected by the :streamGenerateContent path rather
    // than a JSON `stream` field, so the generic body-based detector cannot
    // infer either side of this exchange.
    mark_subscription_stream_exchange(&mut payload);
    insert_subscription_stream_debug_metrics(&mut payload, &stream_debug);
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
