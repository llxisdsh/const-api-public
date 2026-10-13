pub(crate) async fn forward_cursor_subscription_request(
    config: &SupplierConfig,
    path: &str,
    body: &str,
    outbound: Option<(&SupplierOutbound, &str)>,
    upstream_override: Option<&str>,
    capacity: Option<SubscriptionRequestCapacity>,
    cache_identity: Option<&str>,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>> {
    use crate::protocol::{
        adapters::AdapterContext,
        conversion::ConversionPlan,
        ir::{CanonicalError, CanonicalStreamEvent as Event, Usage},
        kind::ProtocolKind,
        stream::{CanonicalAccumulator, StreamRenderer},
    };
    let started_at = std::time::Instant::now();
    let _guard = if outbound.is_some() {
        match subscription_safety_enter_with_capacity(
            config,
            body,
            capacity.unwrap_or_else(|| SubscriptionRequestCapacity::for_path(path)),
        ) {
            Ok(guard) => Some(guard),
            Err(payload) => return Ok(Some(payload)),
        }
    } else {
        None
    };
    if is_anthropic_count_tokens_path(path)
        || is_gemini_count_tokens_path(path)
        || is_gemini_embed_content_path(path)
        || is_openai_responses_compact_path(path)
    {
        return Err(request_conversion_failure(
            "Cursor does not expose this special operation",
        ));
    }
    let protocol = ProtocolKind::parse(
        &subscription_inbound_protocol(path)
            .ok_or_else(|| request_conversion_failure("unsupported Cursor generation path"))?,
    )?;
    let channel = channel_from_supplier(config.channel_id.clone(), config);
    let model = channel_upstream_model_for_request(
        &channel,
        requested_model_from_body_or_path(body, path).as_deref(),
    );
    let identity = serde_json::json!([
        channel.id,
        channel.subscription.credential_ref,
        cache_identity
            .map(str::to_string)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
    ])
    .to_string();
    let prepared = crate::cursor::request::prepare_with_models(
        protocol,
        &serde_json::from_str(body).map_err(request_conversion_failure)?,
        &model,
        &identity,
        &crate::cursor::models::source_ids(&channel),
    )
    .map_err(request_conversion_failure)?;
    let model = prepared.model.clone();
    let faults: Vec<_> = prepared
        .adjustments
        .iter()
        .map(|message| {
            ImprovementFault::endpoint_adjustment(
                "cursor_request_adapted",
                "cursor",
                "cursor_agent",
                protocol.as_str(),
                "cursor_agent",
                &model,
                "$",
                message,
            )
        })
        .collect();
    let client = crate::cursor::http_client()?;
    #[cfg(test)]
    let client = if upstream_override.is_some() {
        reqwest::Client::builder()
            .no_proxy()
            .http2_prior_knowledge()
            .build()?
    } else {
        client
    };
    let (mut token, _, _) =
        ensure_cursor_subscription_access_token(&client, &channel, false).await?;
    let start = crate::cursor::runtime::start(&client, &token, &prepared, upstream_override);
    let mut result = match outbound {
        Some((sender, _)) => tokio::select! { biased;
            _ = sender.closed() => anyhow::bail!("Cursor downstream cancelled"),
            result = start => result,
        },
        None => start.await,
    };
    if result
        .as_ref()
        .err()
        .and_then(|e| e.downcast_ref::<crate::cursor::ApiError>())
        .is_some_and(|e| e.status == 401)
    {
        // Only a pre-stream HTTP authentication rejection may be retried. Never
        // replay a timeout, a model rejection or a partially received response.
        (token, _, _) = ensure_cursor_subscription_access_token(&client, &channel, true).await?;
        let start = crate::cursor::runtime::start(&client, &token, &prepared, upstream_override);
        result = match outbound {
            Some((sender, _)) => tokio::select! { biased;
                _ = sender.closed() => anyhow::bail!("Cursor downstream cancelled"),
                result = start => result,
            },
            None => start.await,
        };
    }
    let mut events = match result {
        Ok(events) => events,
        Err(error) => {
            return cursor_failure_payload(
                config,
                path,
                body,
                &error,
                &faults,
                outbound.is_some(),
                started_at.elapsed(),
            )
            .map(Some);
        }
    };
    let mut mapper = prepared.tool_mapping.stream_mapper();
    let mut renderer = StreamRenderer::new(protocol, &model).with_estimated_token_usage();
    let mut accumulator = CanonicalAccumulator::new();
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("text/event-stream"),
    );
    let mut started = false;
    let mut pending_start = None;
    let mut output_chars = 0_u64;
    let input = u64::from(estimate_subscription_tokens(body));
    let mut reported_usage = Usage::default();
    let initial = Event::UsageUpdate(Box::new(cursor_response_usage(
        &reported_usage, input, 0, protocol,
    )?));
    renderer.push(&initial)?;
    accumulator.push(&initial)?;
    let processing: Result<()> = async {
        loop {
            let next = match outbound {
                Some((sender, _)) => tokio::select! { biased;
                    _ = sender.closed() => anyhow::bail!("Cursor downstream cancelled"),
                    value = events.recv() => value,
                },
                None => events.recv().await,
            };
            let event =
                next.ok_or_else(|| anyhow!("Cursor stream closed without a final event"))??;
            if let Event::UsageUpdate(usage) = event {
                reported_usage = *usage;
                continue;
            }
            if matches!(event, Event::ResponseStart(_)) {
                // AgentService may reject the account in its first Connect
                // frame. Do not commit HTTP 200 before any actual response.
                pending_start = Some(event);
                continue;
            }
            let done = matches!(event, Event::ResponseDone(_));
            for event in mapper.map(&[event])? {
                match &event {
                    Event::TextDelta { text, .. } | Event::ReasoningDelta { text, .. } => {
                        output_chars += text.chars().count() as u64
                    }
                    Event::ToolArgumentsDelta { data, .. } => {
                        output_chars += data.chars().count() as u64
                    }
                    _ => {}
                }
                let mut bytes = Vec::new();
                if let Some(start) = pending_start.take() {
                    if outbound.is_some() {
                        bytes.extend(renderer.push(&start)?);
                    } else {
                        accumulator.push(&start)?;
                    }
                }
                if matches!(event, Event::ResponseDone(_)) {
                    let usage = Event::UsageUpdate(Box::new(cursor_response_usage(
                        &reported_usage, input, output_chars.div_ceil(4), protocol,
                    )?));
                    if outbound.is_some() {
                        renderer.set_estimated_token_usage(
                            reported_usage.input_tokens.value.is_none()
                                || reported_usage.output_tokens.value.is_none(),
                        );
                        bytes.extend(renderer.push(&usage)?);
                    } else {
                        accumulator.push(&usage)?;
                    }
                }
                if let Some((sender, id)) = outbound {
                    bytes.extend(renderer.push(&event)?);
                    send_subscription_stream_chunk(
                        sender,
                        id,
                        200,
                        &headers,
                        &mut started,
                        &faults,
                        &bytes,
                    )
                    .await?;
                } else {
                    accumulator.push(&event)?;
                }
            }
            if done {
                return Ok(());
            }
        }
    }
    .await;
    if let Err(error) = processing {
        if outbound.is_some_and(|(sender, _)| sender.is_closed()) {
            return Err(error);
        }
        let payload = cursor_failure_payload(
            config,
            path,
            body,
            &error,
            &faults,
            outbound.is_some(),
            started_at.elapsed(),
        )?;
        if let Some((sender, id)) = outbound
            && started
        {
            let message = crate::upstream_failure::safe_excerpt(&error.to_string());
            let bytes = renderer.push(&Event::Error(CanonicalError {
                code: "cursor_upstream_error".into(),
                message,
                retryable: false,
                provider_code: error
                    .downcast_ref::<crate::cursor::ApiError>()
                    .map(|e| e.code.clone()),
                details: None,
            }))?;
            let status = error
                .downcast_ref::<crate::cursor::ApiError>()
                .map_or(502, |e| e.status);
            send_subscription_stream_chunk(
                sender,
                id,
                status,
                &headers,
                &mut started,
                &faults,
                &bytes,
            )
            .await?;
            send_supplier_message(
                sender,
                SupplierMessage {
                    id: id.into(),
                    kind: "error".into(),
                    payload,
                },
            )
            .await?;
            return Ok(None);
        }
        return Ok(Some(payload));
    }
    let upstream_usage = SupplierUpstreamUsage::cursor_reported_usage(prepared.fast, &reported_usage);
    let mut payload = if let Some((sender, id)) = outbound {
        renderer.finish()?;
        finish_subscription_stream(
            sender, id, 200, &headers, &mut started, &faults, Some(&upstream_usage),
        ).await?;
        subscription_wire_response_payload(200, "text/event-stream", String::new())
    } else {
        let response = accumulator.finish()?;
        let context = AdapterContext::runtime("cursor", "cursor_agent", None, &model);
        let mut wire = crate::protocol::encode_response(
            protocol,
            &response,
            &ConversionPlan::native_passthrough(protocol),
            &context,
        )?;
        let field = if protocol == ProtocolKind::GeminiNative {
            "usageMetadata"
        } else {
            "usage"
        };
        if reported_usage.input_tokens.value.is_none()
            || reported_usage.output_tokens.value.is_none()
        {
            wire[field]["const_api_usage_source"] = serde_json::json!("estimated");
        }
        subscription_wire_response_payload(200, "application/json", serde_json::to_string(&wire)?)
    };
    attach_improvement_faults(&mut payload, &faults);
    attach_upstream_usage(&mut payload, Some(&upstream_usage));
    if outbound.is_some() {
        let _ = append_subscription_wire_audit(config, path, body, &payload, started_at.elapsed());
        Ok(None)
    } else {
        Ok(Some(payload))
    }
}

fn cursor_response_usage(
    reported: &crate::protocol::ir::Usage,
    input_estimate: u64,
    output_estimate: u64,
    protocol: crate::protocol::kind::ProtocolKind,
) -> Result<crate::protocol::ir::Usage> {
    use crate::protocol::{
        ir::{UsageProvenance, UsageValue},
        kind::ProtocolKind,
    };
    let derived = |value| UsageValue {
        value: Some(value), source: UsageProvenance::Derived,
    };
    let mut usage = reported.clone();
    if usage.input_tokens.value.is_none() {
        usage.input_tokens = derived(input_estimate);
    }
    if usage.output_tokens.value.is_none() {
        usage.output_tokens = derived(output_estimate);
    }
    let cache = usage.cache_read_tokens.value.unwrap_or(0)
        .checked_add(usage.cache_write_tokens.value.unwrap_or(0))
        .ok_or_else(|| anyhow!("Cursor cache token total is out of range"))?;
    let full_input = usage.input_tokens.value.unwrap();
    usage.total_tokens = derived(
        full_input.checked_add(usage.output_tokens.value.unwrap())
            .ok_or_else(|| anyhow!("Cursor token total is out of range"))?,
    );
    // Anthropic reports uncached input separately; the other public protocols
    // include cached input. Billing always receives the original sidecar above.
    if protocol == ProtocolKind::AnthropicMessages && cache > 0 {
        usage.input_tokens = derived(full_input.saturating_sub(cache));
    }
    Ok(usage)
}

fn cursor_failure_payload(
    config: &SupplierConfig,
    path: &str,
    body: &str,
    error: &anyhow::Error,
    faults: &[ImprovementFault],
    audit: bool,
    elapsed: std::time::Duration,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    let native = error.downcast_ref::<crate::cursor::ApiError>();
    let status = native.map_or(502, |e| e.status);
    let code = native.map_or("cursor_upstream_error", |e| e.code.as_str());
    let message = crate::upstream_failure::safe_excerpt(&error.to_string());
    let mut payload = subscription_wire_error_response(status, code, &message);
    attach_improvement_faults(&mut payload, faults);
    if audit {
        let _ = append_subscription_wire_audit(config, path, body, &payload, elapsed);
    }
    Ok(payload)
}
