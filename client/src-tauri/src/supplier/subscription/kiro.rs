// Kiro's native EventStream is decoded once for both local and platform paths.
// HTTP/SSE encoding stays in the existing canonical protocol adapters.
pub(crate) async fn forward_kiro_subscription_request(
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
        ir::{CanonicalError, CanonicalStreamEvent as Event, Usage, UsageProvenance, UsageValue},
        kind::ProtocolKind,
        stream::{CanonicalAccumulator, StreamRenderer},
    };
    let started_at = std::time::Instant::now();
    // Buffered execution is already guarded/audited by the subscription dispatcher.
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
    let channel = channel_from_supplier(config.channel_id.clone(), config);
    let protocol = subscription_inbound_protocol(path)
        .ok_or_else(|| request_conversion_failure("unsupported Kiro generation path"))?;
    if is_anthropic_count_tokens_path(path)
        || is_gemini_count_tokens_path(path)
        || is_gemini_embed_content_path(path)
        || is_openai_responses_compact_path(path)
    {
        return Err(request_conversion_failure(
            "Kiro does not expose this special operation",
        ));
    }
    let protocol = ProtocolKind::parse(&protocol)?;
    let model = channel_upstream_model_for_request(
        &channel,
        requested_model_from_body_or_path(body, path).as_deref(),
    );
    let client = crate::kiro::http_client()?;
    let (mut token, credential, _) =
        ensure_kiro_subscription_access_token(&client, &channel, false).await?;
    let profile = credential["profile_arn"]
        .as_str()
        .expect("validated Kiro profile");
    let native = crate::kiro::request::prepare(
        protocol,
        &serde_json::from_str(body).map_err(request_conversion_failure)?,
        &model,
        profile,
        cache_identity,
        channel
            .capability_profiles
            .iter()
            .find(|profile| {
                profile.catalog_metadata && profile.catalog_source_model.as_deref() == Some(&model)
            })
            .and_then(|profile| profile.kiro_request_schema.as_ref()),
    )
    .map_err(request_conversion_failure)?;
    let faults: Vec<_> = native
        .adjustments
        .iter()
        .map(|message| {
            ImprovementFault::endpoint_adjustment(
                "kiro_request_adapted",
                "kiro",
                "kiro_v3",
                protocol.as_str(),
                "kiro_v3",
                &model,
                "$",
                message,
            )
        })
        .collect();
    let url = match upstream_override {
        Some(url) => url.to_string(),
        None => format!(
            "https://runtime.{}.kiro.dev/",
            crate::kiro::profile_region(profile)?
        ),
    };
    let send = |token: &str| {
        crate::kiro::api_request(
            &client,
            &url,
            token,
            "KiroRuntimeService.GenerateAssistantResponse",
            &native.body,
        )
        .header("Accept", "application/vnd.amazon.eventstream")
    };
    let mut response =
        send_kiro_runtime_request(send(&token), outbound.map(|(sender, _)| sender)).await?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        // A concrete authentication rejection is safe to retry once. Never replay
        // an ambiguous send failure, a partial stream, or a quota rejection.
        let _rejection = crate::kiro::rejection(response, &token).await?;
        (token, _, _) = ensure_kiro_subscription_access_token(&client, &channel, true).await?;
        response =
            send_kiro_runtime_request(send(&token), outbound.map(|(sender, _)| sender)).await?;
    }
    if !response.status().is_success() {
        let headers = subscription_response_headers(response.headers());
        let error = crate::kiro::rejection(response, &token).await?;
        let raw =
            serde_json::json!({"error":{"type":"kiro_upstream_error","message":error.to_string()}})
                .to_string();
        let mut payload = subscription_wire_response_payload_with_header_map(
            error.status,
            "application/json",
            raw,
            &headers,
            true,
        );
        attach_improvement_faults(&mut payload, &faults);
        if outbound.is_some() {
            let _ =
                append_subscription_wire_audit(config, path, body, &payload, started_at.elapsed());
        }
        return Ok(Some(payload));
    }
    let headers = subscription_stream_headers(response.headers());
    let mut stream = SupplierByteStreamBatcher::new(response.bytes_stream());
    let mut decoder = crate::kiro::stream::StreamDecoder::new(&model);
    let mut mapper = native.tool_mapping.stream_mapper();
    let mut renderer = StreamRenderer::new(protocol, &model).with_estimated_token_usage();
    let mut accumulator = CanonicalAccumulator::new();
    let input = u64::from(estimate_subscription_tokens(body));
    let usage = |output: u64| Usage {
        input_tokens: UsageValue {
            value: Some(input),
            source: UsageProvenance::Derived,
        },
        output_tokens: UsageValue {
            value: Some(output),
            source: UsageProvenance::Derived,
        },
        total_tokens: UsageValue {
            value: Some(input + output),
            source: UsageProvenance::Derived,
        },
        ..Default::default()
    };
    renderer.push(&Event::UsageUpdate(Box::new(usage(0))))?;
    accumulator.push(&Event::UsageUpdate(Box::new(usage(0))))?;
    let mut started = false;
    let mut output_chars = 0_u64;
    let mut received_bytes = 0_usize;
    let mut returned_bytes = 0_usize;
    let processing: Result<()> = async {
        loop {
            let read = match outbound {
                Some((sender, _)) => tokio::select! {
                    biased;
                    _ = sender.closed() => anyhow::bail!("Kiro request cancelled by downstream"),
                    result = stream.next() => result?,
                },
                None => stream.next().await?,
            };
            let (events, finished) = match read {
                SupplierStreamRead::Chunk(batch) => {
                    received_bytes += batch.bytes.len();
                    (decoder.push(&batch.bytes)?, false)
                }
                SupplierStreamRead::End => (decoder.finish()?, true),
                SupplierStreamRead::InactivityTimeout => {
                    anyhow::bail!("Kiro stream inactivity deadline exceeded")
                }
            };
            let mut output = Vec::new();
            for event in mapper.map(&events)? {
                match &event {
                    Event::TextDelta { text, .. }
                    | Event::ReasoningDelta { text, .. }
                    | Event::RefusalDelta { text, .. } => {
                        output_chars += text.chars().count() as u64
                    }
                    Event::ToolArgumentsDelta { data, .. } => {
                        output_chars += data.chars().count() as u64
                    }
                    Event::ResponseDone(_) => {
                        let estimated =
                            Event::UsageUpdate(Box::new(usage(output_chars.div_ceil(4))));
                        if outbound.is_some() {
                            renderer.push(&estimated)?;
                        } else {
                            accumulator.push(&estimated)?;
                        }
                    }
                    _ => {}
                }
                if outbound.is_some() {
                    output.extend_from_slice(&renderer.push(&event)?);
                } else {
                    accumulator.push(&event)?;
                }
            }
            if let Some((outbound, id)) = outbound {
                returned_bytes += output.len();
                send_subscription_stream_chunk(
                    outbound,
                    id,
                    200,
                    &headers,
                    &mut started,
                    &faults,
                    &output,
                )
                .await?;
            }
            if finished {
                break;
            }
        }
        Ok(())
    }
    .await;
    if let Err(error) = processing {
        if outbound.is_some_and(|(sender, _)| sender.is_closed()) {
            // Consumer cancellation is not a provider failure; dropping the
            // response stream cancels its connection without replaying the call.
            return Err(error);
        }
        let native_error = error.downcast_ref::<crate::kiro::stream::StreamFailure>();
        let code = native_error
            .map(|error| error.code.as_str())
            .unwrap_or("upstream_stream_interrupted");
        let message = crate::upstream_failure::safe_excerpt(&error.to_string());
        let status = match code.to_ascii_lowercase().as_str() {
            "throttlingexception" | "servicequotaexceededexception" => 429,
            "accessdeniedexception" => 403,
            "validationexception" => 400,
            _ => 502,
        };
        let failure = execution_failure(
            "upstream_stream",
            "upstream_provider",
            code,
            true,
            true,
            started,
            output_chars > 0,
            recommendation("deny", "review", "none", "eligible", "other_channel"),
            &message,
        );
        let mut payload = supplier_execution_error_payload(&failure);
        payload
            .get_mut("wire")
            .expect("execution error has a wire envelope")["status"] = serde_json::json!(status);
        attach_improvement_faults(&mut payload, &faults);
        if let Some((outbound, id)) = outbound {
            let _ =
                append_subscription_wire_audit(config, path, body, &payload, started_at.elapsed());
            if started {
                let bytes = renderer.push(&Event::Error(CanonicalError {
                    code: code.to_string(),
                    message,
                    retryable: status == 429 || status >= 500,
                    provider_code: Some(code.to_string()),
                    details: None,
                }))?;
                send_subscription_stream_chunk(
                    outbound,
                    id,
                    status,
                    &headers,
                    &mut started,
                    &faults,
                    &bytes,
                )
                .await?;
                send_supplier_message(
                    outbound,
                    SupplierMessage {
                        id: id.to_string(),
                        kind: "error".into(),
                        payload,
                    },
                )
                .await?;
                return Ok(None);
            }
        }
        return Ok(Some(payload));
    }
    let mut payload = if let Some((outbound, id)) = outbound {
        renderer.finish()?;
        finish_subscription_stream(outbound, id, 200, &headers, &mut started, &faults, None)
            .await?;
        subscription_wire_response_payload(200, "text/event-stream", String::new())
    } else {
        let response = accumulator.finish()?;
        let context = AdapterContext::runtime("kiro", "kiro_v3", None, &model);
        let mut wire = crate::protocol::encode_response(
            protocol,
            &response,
            &ConversionPlan::native_passthrough(protocol),
            &context,
        )?;
        let usage_field = if protocol == ProtocolKind::GeminiNative {
            "usageMetadata"
        } else {
            "usage"
        };
        wire[usage_field]["const_api_usage_source"] = serde_json::json!("estimated");
        let raw = serde_json::to_string(&wire)?;
        returned_bytes = raw.len();
        subscription_wire_response_payload_with_header_map(
            200,
            "application/json",
            raw,
            &headers,
            true,
        )
    };
    attach_improvement_faults(&mut payload, &faults);
    payload.insert(
        "upstream_response_bytes".into(),
        serde_json::json!(received_bytes),
    );
    payload.insert(
        "returned_response_bytes".into(),
        serde_json::json!(returned_bytes),
    );
    if let Some(credits) = decoder.credits() {
        // Diagnostic only: credits have no token conversion or USD denomination.
        payload.insert("kiro_usage_credits".into(), serde_json::json!(credits));
    }
    if outbound.is_some() {
        let _ = append_subscription_wire_audit(config, path, body, &payload, started_at.elapsed());
        Ok(None)
    } else {
        Ok(Some(payload))
    }
}

async fn send_kiro_runtime_request(
    request: reqwest::RequestBuilder,
    outbound: Option<&SupplierOutbound>,
) -> Result<reqwest::Response> {
    match outbound {
        Some(sender) => tokio::select! {
            biased;
            _ = sender.closed() => anyhow::bail!("Kiro request cancelled by downstream"),
            response = crate::upstream_transport::send(request) => Ok(response?),
        },
        None => Ok(crate::upstream_transport::send(request).await?),
    }
}
