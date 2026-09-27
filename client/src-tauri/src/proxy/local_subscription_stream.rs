struct LocalOpenAiSubscriptionStreamState {
    stream: BoxStream<'static, Result<bytes::Bytes, reqwest::Error>>,
    converter: InboundSseStreamConverter,
    upstream_utf8: crate::protocol::stream::Utf8ChunkDecoder,
    converted_utf8: crate::protocol::stream::Utf8ChunkDecoder,
    _safety_guard: Option<SubscriptionSafetyGuard>,
    config: SupplierConfig,
    inbound_path: String,
    inbound_body: String,
    upstream_url: String,
    upstream_body: String,
    status: u16,
    upstream_content_type: String,
    inbound_protocol: String,
    model: String,
    terminal_raw: String,
    converted_raw: SubscriptionOutputCapture,
    improvement_faults: Vec<ImprovementFault>,
    continuation: CodexHttpContinuationCollector,
    continuation_cached: bool,
    started_at: std::time::Instant,
    timing_id: u64,
    upstream_chunks: u64,
    upstream_bytes: usize,
    converted_chunks: u64,
    converted_bytes: usize,
    upstream_usage: Option<SupplierUpstreamUsageAccumulator>,
    timing_sse_decoder: Option<crate::protocol::stream::SseDecoder>,
    first_sse_event_ms: Option<u64>,
    first_meaningful_event_ms: Option<u64>,
    last_upstream_chunk_at: Option<std::time::Instant>,
    last_text_delta_at: Option<std::time::Instant>,
    last_client_emit_at: Option<std::time::Instant>,
    terminal_detected_at: Option<std::time::Instant>,
    done_ready_at: Option<std::time::Instant>,
    audit_finalized: bool,
    // Diagnostic only: do not change retry, billing or wire payload classification.
    stream_error_kind: Option<&'static str>,
    finished: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LocalOpenAiStreamFinalization {
    UpstreamFinished,
    DownstreamDropped,
}

impl LocalOpenAiStreamFinalization {
    fn as_str(self) -> &'static str {
        match self {
            Self::UpstreamFinished => "upstream_finished",
            Self::DownstreamDropped => "downstream_dropped",
        }
    }
}

fn local_stream_summary_level(error_kind: &str, audit_ok: bool) -> log::Level {
    if !audit_ok {
        log::Level::Error
    } else if !error_kind.is_empty() && error_kind != "downstream_cancelled" {
        log::Level::Warn
    } else {
        log::Level::Info
    }
}

#[cfg(test)]
mod local_stream_summary_tests {
    use super::*;

    #[test]
    fn request_summary_keeps_failures_and_distinguishes_cancellation() {
        assert_eq!(local_stream_summary_level("", true), log::Level::Info);
        assert_eq!(
            local_stream_summary_level("downstream_cancelled", true),
            log::Level::Info
        );
        for kind in [
            "upstream_stream_read_failed",
            "stream_inactivity_timeout",
            "upstream_stream_incomplete",
            "stream_utf8_invalid",
            "upstream_error",
            "future_error",
        ] {
            assert_eq!(local_stream_summary_level(kind, true), log::Level::Warn);
        }
        assert_eq!(local_stream_summary_level("", false), log::Level::Error);
        assert_eq!(
            local_stream_summary_level("downstream_cancelled", false),
            log::Level::Error
        );
    }
}

impl LocalOpenAiSubscriptionStreamState {
    fn terminal_seen(&self) -> bool {
        self.continuation.progress().terminal_seen
    }

    fn finalize_audit(&mut self, finalization: LocalOpenAiStreamFinalization) {
        if self.audit_finalized {
            return;
        }
        self.audit_finalized = true;

        let terminal_seen = self.terminal_seen();
        let terminal_error = subscription_sse_terminal_error_payload(&self.terminal_raw);
        let downstream_cancelled = finalization == LocalOpenAiStreamFinalization::DownstreamDropped
            && !terminal_seen
            && terminal_error.is_none();
        let diagnostic_error = self.stream_error_kind.map(str::to_string).or_else(|| {
            terminal_error.as_ref().map(|payload| {
                payload
                    .get("error_kind")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("upstream_error")
                    .to_string()
            })
        });
        self.improvement_faults
            .extend(improvement_faults_from_stream_notices(
                self.converter.take_notices(),
                "openai_responses",
                &self.inbound_protocol,
                &self.model,
            ));

        let mut payload = terminal_error.unwrap_or_else(|| {
            if downstream_cancelled {
                subscription_error_response(
                    499,
                    "downstream_cancelled",
                    "local client closed the response stream before an upstream terminal event",
                )
            } else {
                http_response_payload(self.status, "text/event-stream", String::new())
            }
        });
        if downstream_cancelled {
            payload.insert(
                "error_kind".to_string(),
                serde_json::Value::String("downstream_cancelled".to_string()),
            );
            // A caller cancellation is request-scoped. It must not degrade the
            // subscription account or channel health.
            payload.insert(
                "failure_scope".to_string(),
                serde_json::Value::String("request".to_string()),
            );
        }
        payload.insert(
            "stream_finalization".to_string(),
            serde_json::Value::String(finalization.as_str().to_string()),
        );
        payload.insert(
            "upstream_model".to_string(),
            serde_json::Value::String(self.model.clone()),
        );
        payload.insert("stream".to_string(), serde_json::Value::Bool(true));
        payload.insert("upstream_stream".to_string(), serde_json::Value::Bool(true));
        payload.insert(
            "inbound_request_bytes".to_string(),
            serde_json::json!(self.inbound_body.len()),
        );
        payload.insert(
            "upstream_request_bytes".to_string(),
            serde_json::json!(self.upstream_body.len()),
        );
        payload.insert(
            "response_wire_bytes".to_string(),
            serde_json::json!(self.terminal_raw.len()),
        );
        payload.insert(
            "upstream_response_bytes".to_string(),
            serde_json::json!(self.terminal_raw.len()),
        );
        payload.insert(
            "response_body_bytes".to_string(),
            serde_json::json!(self.converted_raw.len()),
        );
        payload.insert(
            "returned_response_bytes".to_string(),
            serde_json::json!(self.converted_raw.len()),
        );
        payload.insert(
            "upstream_chunk_count".to_string(),
            serde_json::json!(self.upstream_chunks),
        );
        payload.insert(
            "first_sse_event_ms".to_string(),
            serde_json::json!(self.first_sse_event_ms),
        );
        payload.insert(
            "first_meaningful_event_ms".to_string(),
            serde_json::json!(self.first_meaningful_event_ms),
        );
        payload.insert(
            "routing_latency_ms".to_string(),
            serde_json::json!(self.first_meaningful_event_ms.or(self.first_sse_event_ms)),
        );
        let (input_tokens, output_tokens) = extract_usage_tokens(&self.terminal_raw);
        payload.insert("input_tokens".to_string(), serde_json::json!(input_tokens));
        payload.insert(
            "output_tokens".to_string(),
            serde_json::json!(output_tokens),
        );
        let upstream_usage = self
            .upstream_usage
            .take()
            .and_then(SupplierUpstreamUsageAccumulator::finish);
        attach_upstream_usage(&mut payload, upstream_usage.as_ref());
        attach_improvement_faults(&mut payload, &self.improvement_faults);

        let raw_log_started_at = std::time::Instant::now();
        let raw_log_result = append_supplier_raw_exchange_log(
            &self.config,
            "openai",
            &self.inbound_path,
            &self.upstream_url,
            &self.inbound_body,
            &self.upstream_body,
            self.status,
            &self.upstream_content_type,
            &self.terminal_raw,
            "text/event-stream",
            self.converted_raw.as_str(),
            None,
        );
        log::debug!(
            "[const-api][stream-timing] id={} phase=raw_exchange_log_complete elapsed_ms={} duration_ms={} ok={} finalization={} terminal_seen={} raw_response_bytes={} converted_response_bytes={}",
            self.timing_id,
            self.started_at.elapsed().as_millis(),
            raw_log_started_at.elapsed().as_millis(),
            raw_log_result.is_ok(),
            finalization.as_str(),
            terminal_seen,
            self.terminal_raw.len(),
            self.converted_raw.len()
        );
        let wire_audit_started_at = std::time::Instant::now();
        let wire_audit_result = append_subscription_wire_audit(
            &self.config,
            &self.inbound_path,
            &self.inbound_body,
            &payload,
            self.started_at.elapsed(),
        );
        log::debug!(
            "[const-api][stream-timing] id={} phase=wire_audit_complete elapsed_ms={} duration_ms={} ok={} finalization={} terminal_seen={} downstream_cancelled={}",
            self.timing_id,
            self.started_at.elapsed().as_millis(),
            wire_audit_started_at.elapsed().as_millis(),
            wire_audit_result.is_ok(),
            finalization.as_str(),
            terminal_seen,
            downstream_cancelled
        );
        let error_kind = diagnostic_error
            .as_deref()
            .unwrap_or(if downstream_cancelled {
                "downstream_cancelled"
            } else if !terminal_seen {
                "upstream_stream_incomplete"
            } else if self.status >= 400 {
                "upstream_http_error"
            } else {
                ""
            });
        let audit_ok = raw_log_result.is_ok() && wire_audit_result.is_ok();
        log::log!(
            local_stream_summary_level(error_kind, audit_ok),
            "[const-api][request] event=local_subscription_stream_end id={} channel={} model={} protocol={} path={} upstream_status={} error_kind={} duration_ms={} first_event_ms={:?} upstream_bytes={} downstream_bytes={} terminal_seen={} continuation_cached={} audit_ok={}",
            self.timing_id, self.config.channel_id, self.model, self.inbound_protocol,
            self.inbound_path, self.status, if error_kind.is_empty() { "-" } else { error_kind },
            self.started_at.elapsed().as_millis(), self.first_meaningful_event_ms.or(self.first_sse_event_ms),
            self.upstream_bytes, self.converted_bytes, terminal_seen, self.continuation_cached, audit_ok
        );
        if let Err(error) = raw_log_result {
            log::error!(
                "[const-api][request] id={} raw_audit_write_failed error={error:#}",
                self.timing_id
            );
        }
        if let Err(error) = wire_audit_result {
            log::error!(
                "[const-api][request] id={} usage_audit_write_failed error={error:#}",
                self.timing_id
            );
        }
    }
}

impl Drop for LocalOpenAiSubscriptionStreamState {
    fn drop(&mut self) {
        if self.audit_finalized {
            return;
        }
        log::debug!(
            "[const-api][stream-timing] id={} phase=downstream_stream_dropped elapsed_ms={} terminal_seen={} upstream_chunks={} upstream_bytes={} converted_chunks={} converted_bytes={}",
            self.timing_id,
            self.started_at.elapsed().as_millis(),
            self.terminal_seen(),
            self.upstream_chunks,
            self.upstream_bytes,
            self.converted_chunks,
            self.converted_bytes
        );
        self.finalize_audit(LocalOpenAiStreamFinalization::DownstreamDropped);
    }
}

static LOCAL_OPENAI_STREAM_TIMING_SEQUENCE: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

fn optional_elapsed_ms(instant: Option<std::time::Instant>) -> i128 {
    instant
        .map(|value| i128::try_from(value.elapsed().as_millis()).unwrap_or(i128::MAX))
        .unwrap_or(-1)
}

fn contains_chat_done(bytes: &[u8]) -> bool {
    const CHAT_DONE: &[u8] = b"data: [DONE]";
    bytes
        .windows(CHAT_DONE.len())
        .any(|window| window == CHAT_DONE)
}

#[cfg(test)]
pub(crate) async fn forward_openai_subscription_local_stream_response(
    client: &Client,
    config: &SupplierConfig,
    path: &str,
    body: &str,
    request_headers: &warp::http::HeaderMap,
    upstream_override: Option<&str>,
) -> Result<warp::reply::Response> {
    let cache_identity = subscription_body_cache_identity(body);
    forward_openai_subscription_local_stream_response_with_cache_identity(
        client,
        config,
        path,
        body,
        request_headers,
        cache_identity.as_deref(),
        upstream_override,
    )
    .await
}

async fn forward_openai_subscription_local_stream_response_with_cache_identity(
    client: &Client,
    config: &SupplierConfig,
    path: &str,
    body: &str,
    request_headers: &warp::http::HeaderMap,
    cache_identity: Option<&str>,
    upstream_override: Option<&str>,
) -> Result<warp::reply::Response> {
    let started_at = std::time::Instant::now();
    let safety_guard = match subscription_safety_enter(config, body) {
        Ok(guard) => guard,
        Err(payload) => {
            let _ =
                append_subscription_wire_audit(config, path, body, &payload, started_at.elapsed());
            return local_subscription_payload_response(payload);
        }
    };
    if subscription_inbound_protocol(path).is_none() {
        let payload = subscription_error_response(
            501,
            "subscription_stream_not_supported",
            "OpenAI account subscription could not identify the inbound protocol",
        );
        let _ = append_subscription_wire_audit(config, path, body, &payload, started_at.elapsed());
        return local_subscription_payload_response(payload);
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
    )?;
    let mut improvement_faults = conversion.faults;
    let upstream_body = conversion.body;
    let inbound_protocol = conversion.inbound_protocol;
    let target_protocol = conversion.target_protocol;
    debug_assert_eq!(target_protocol, "openai_responses");
    let upstream_body = json_body_with_stream(&upstream_body, true)?;
    let prepared = prepare_codex_subscription_endpoint_request(
        config,
        &upstream_body,
        false,
        &model,
        cache_identity,
        Some(request_headers),
    )?;
    improvement_faults.extend(prepared.faults);
    let upstream_body = prepared.body;
    let (token, credential, _) = ensure_openai_subscription_access_token(client, &channel).await?;
    let upstream_url = upstream_override
        .map(str::to_string)
        .unwrap_or_else(codex_responses_url);
    let resp = send_openai_subscription_response_request(
        client,
        &channel,
        &upstream_url,
        token,
        credential,
        upstream_body.clone(),
        OPENAI_OAUTH_TOKEN_URL,
        "openai",
        Some(request_headers),
    )
    .await?;
    let status = resp.status().as_u16();
    let content_type = response_content_type(&resp);
    let upstream_http_version = resp.version();
    if !(200..300).contains(&status) {
        let mut response_headers = subscription_response_headers(resp.headers());
        let raw = resp.bytes().await?;
        let client_status = local_subscription_status_for_client(status, request_headers);
        if !response_headers.contains_key(reqwest::header::CONTENT_TYPE) {
            response_headers.insert(
                reqwest::header::CONTENT_TYPE,
                content_type.parse().unwrap_or_else(|_| {
                    reqwest::header::HeaderValue::from_static("application/json")
                }),
            );
        }
        if let Ok(value) = reqwest::header::HeaderValue::from_str(&status.to_string()) {
            response_headers.insert("x-const-api-upstream-status", value);
        }
        let raw_text = String::from_utf8_lossy(&raw);
        let mut payload =
            subscription_http_response_payload(status, &content_type, raw_text.to_string());
        insert_subscription_route_model_failure_evidence(
            &mut payload,
            status,
            raw_text.as_ref(),
            Some(&model),
        );
        attach_improvement_faults(&mut payload, &improvement_faults);
        let _ = append_subscription_wire_audit(config, path, body, &payload, started_at.elapsed());
        let mut response = response_from_body(
            warp::http::StatusCode::from_u16(client_status)?,
            &response_headers,
            raw,
        )?;
        attach_local_model_failure_evidence(&mut response, &payload);
        return Ok(response);
    }
    // This executor is reached only for an inbound streaming request. Codex can
    // omit or mislabel Content-Type while still returning an SSE body, so the
    // request contract—not that response header—must select incremental reads.
    // The tunneled subscription executor follows the same rule.
    let headers = subscription_stream_headers(resp.headers());
    let converter = inbound_sse_stream_converter(&inbound_protocol, "openai_responses", &model)?;
    let timing_id =
        LOCAL_OPENAI_STREAM_TIMING_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    log::debug!(
        "[const-api][stream-timing] id={timing_id} phase=stream_open elapsed_ms={} path={} inbound_protocol={} model={} upstream_http_version={upstream_http_version:?}",
        started_at.elapsed().as_millis(),
        path,
        inbound_protocol,
        model
    );
    let continuation = CodexHttpContinuationCollector::new(&upstream_body);
    let state = LocalOpenAiSubscriptionStreamState {
        stream: Box::pin(resp.bytes_stream()),
        converter,
        upstream_utf8: crate::protocol::stream::Utf8ChunkDecoder::new(),
        converted_utf8: crate::protocol::stream::Utf8ChunkDecoder::new(),
        _safety_guard: Some(safety_guard),
        config: config.clone(),
        inbound_path: path.to_string(),
        inbound_body: body.to_string(),
        upstream_url,
        upstream_body,
        status,
        upstream_content_type: content_type,
        inbound_protocol,
        model,
        terminal_raw: String::new(),
        converted_raw: SubscriptionOutputCapture::new(raw_debug_logs_enabled()),
        improvement_faults,
        continuation,
        continuation_cached: false,
        started_at,
        timing_id,
        upstream_chunks: 0,
        upstream_bytes: 0,
        converted_chunks: 0,
        converted_bytes: 0,
        upstream_usage: Some(SupplierUpstreamUsageAccumulator::new("openai_responses")),
        timing_sse_decoder: Some(crate::protocol::stream::SseDecoder::default()),
        first_sse_event_ms: None,
        first_meaningful_event_ms: None,
        last_upstream_chunk_at: None,
        last_text_delta_at: None,
        last_client_emit_at: None,
        terminal_detected_at: None,
        done_ready_at: None,
        audit_finalized: false,
        stream_error_kind: None,
        finished: false,
    };
    let stream = futures_util::stream::unfold(state, |mut state| async move {
        if state.finished {
            log::debug!(
                "[const-api][stream-timing] id={} phase=http_stream_closed elapsed_ms={} after_done_ms={} upstream_chunks={} upstream_bytes={} converted_chunks={} converted_bytes={}",
                state.timing_id,
                state.started_at.elapsed().as_millis(),
                optional_elapsed_ms(state.done_ready_at),
                state.upstream_chunks,
                state.upstream_bytes,
                state.converted_chunks,
                state.converted_bytes
            );
            return None;
        }
        let mut terminal_chunk = bytes::Bytes::new();
        loop {
            let next = match tokio::time::timeout(
                Duration::from_secs(crate::protocol::stream::STREAM_INACTIVITY_TIMEOUT_SECS),
                state.stream.next(),
            )
            .await
            {
                Ok(next) => next,
                Err(_) => {
                    state.stream_error_kind = Some("stream_inactivity_timeout");
                    log::debug!(
                        "[const-api][stream-timing] id={} phase=upstream_inactivity_timeout elapsed_ms={} after_last_upstream_ms={} after_last_text_delta_ms={} after_last_client_emit_ms={}",
                        state.timing_id,
                        state.started_at.elapsed().as_millis(),
                        optional_elapsed_ms(state.last_upstream_chunk_at),
                        optional_elapsed_ms(state.last_text_delta_at),
                        optional_elapsed_ms(state.last_client_emit_at)
                    );
                    terminal_chunk = state.converter.fail(
                        "stream_inactivity_timeout",
                        "upstream stream produced no event before the inactivity deadline",
                    );
                    break;
                }
            };
            let Some(chunk) = next else {
                log::debug!(
                    "[const-api][stream-timing] id={} phase=upstream_eof elapsed_ms={} after_last_upstream_ms={} after_last_text_delta_ms={} after_last_client_emit_ms={} upstream_chunks={} upstream_bytes={}",
                    state.timing_id,
                    state.started_at.elapsed().as_millis(),
                    optional_elapsed_ms(state.last_upstream_chunk_at),
                    optional_elapsed_ms(state.last_text_delta_at),
                    optional_elapsed_ms(state.last_client_emit_at),
                    state.upstream_chunks,
                    state.upstream_bytes
                );
                break;
            };
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(err) => {
                    state.stream_error_kind = Some("upstream_stream_read_failed");
                    return Some((Err(err), state));
                }
            };
            state.upstream_chunks = state.upstream_chunks.saturating_add(1);
            state.upstream_bytes = state.upstream_bytes.saturating_add(chunk.len());
            if let Some(usage) = state.upstream_usage.as_mut() {
                usage.push(&chunk);
            }
            state.last_upstream_chunk_at = Some(std::time::Instant::now());
            if state.upstream_chunks == 1 {
                log::debug!(
                    "[const-api][stream-timing] id={} phase=first_body_chunk elapsed_ms={} bytes={}",
                    state.timing_id,
                    state.started_at.elapsed().as_millis(),
                    chunk.len()
                );
            }
            let mut saw_meaningful_event = false;
            if let Some(decoder) = state.timing_sse_decoder.as_mut() {
                if let Ok(frames) = decoder.push(&chunk) {
                    for frame in frames {
                        if state.first_sse_event_ms.is_none() {
                            let elapsed_ms = state
                                .started_at
                                .elapsed()
                                .as_millis()
                                .min(u128::from(u64::MAX))
                                as u64;
                            state.first_sse_event_ms = Some(elapsed_ms);
                            log::debug!(
                                "[const-api][stream-timing] id={} phase=first_sse_event elapsed_ms={}",
                                state.timing_id,
                                elapsed_ms
                            );
                        }
                        if responses_sse_event_is_meaningful(&frame.data) {
                            let elapsed_ms = state
                                .started_at
                                .elapsed()
                                .as_millis()
                                .min(u128::from(u64::MAX))
                                as u64;
                            state.first_meaningful_event_ms = Some(elapsed_ms);
                            saw_meaningful_event = true;
                            log::debug!(
                                "[const-api][stream-timing] id={} phase=first_meaningful_event elapsed_ms={}",
                                state.timing_id,
                                elapsed_ms
                            );
                            break;
                        }
                    }
                }
            }
            if saw_meaningful_event {
                state.timing_sse_decoder = None;
            }
            if let Ok(text) = state.upstream_utf8.push(&chunk) {
                if text.contains("response.output_text.delta") {
                    state.last_text_delta_at = Some(std::time::Instant::now());
                }
                state.terminal_raw.push_str(&text);
            }
            let continuation_started_at = std::time::Instant::now();
            let previous_continuation = state.continuation.progress();
            let continuation = state.continuation.push(&state.config, &chunk);
            state.continuation_cached = continuation.cached;
            if !previous_continuation.terminal_seen && continuation.terminal_seen {
                state.terminal_detected_at = Some(std::time::Instant::now());
                log::debug!(
                    "[const-api][stream-timing] id={} phase=upstream_terminal_detected elapsed_ms={} after_last_text_delta_ms={} after_last_client_emit_ms={} upstream_chunks={} upstream_bytes={} accumulated_bytes={}",
                    state.timing_id,
                    state.started_at.elapsed().as_millis(),
                    optional_elapsed_ms(state.last_text_delta_at),
                    optional_elapsed_ms(state.last_client_emit_at),
                    state.upstream_chunks,
                    state.upstream_bytes,
                    state.terminal_raw.len()
                );
                log::debug!(
                    "[const-api][stream-timing] id={} phase=continuation_capture_complete elapsed_ms={} duration_ms={} cached={} request_bytes={} response_bytes={}",
                    state.timing_id,
                    state.started_at.elapsed().as_millis(),
                    continuation_started_at.elapsed().as_millis(),
                    state.continuation_cached,
                    state.upstream_body.len(),
                    state.terminal_raw.len()
                );
            }
            let conversion_started_at = std::time::Instant::now();
            let converted = state.converter.push(&chunk);
            let conversion_duration_ms = conversion_started_at.elapsed().as_millis();
            if converted.is_empty() {
                continue;
            }
            match state.converted_utf8.push(&converted) {
                Ok(text) => state.converted_raw.push_str(&text),
                Err(_) => {
                    state.stream_error_kind = Some("stream_utf8_invalid");
                    terminal_chunk = state.converter.fail(
                        "stream_utf8_invalid",
                        "converted stream contained invalid UTF-8",
                    );
                    break;
                }
            }
            state.converted_chunks = state.converted_chunks.saturating_add(1);
            state.converted_bytes = state.converted_bytes.saturating_add(converted.len());
            if contains_chat_done(&converted) {
                state.done_ready_at = Some(std::time::Instant::now());
                log::debug!(
                    "[const-api][stream-timing] id={} phase=client_done_ready elapsed_ms={} conversion_ms={} after_terminal_ms={} after_last_text_delta_ms={} after_last_client_emit_ms={} converted_chunk_bytes={} continuation_cached={}",
                    state.timing_id,
                    state.started_at.elapsed().as_millis(),
                    conversion_duration_ms,
                    optional_elapsed_ms(state.terminal_detected_at),
                    optional_elapsed_ms(state.last_text_delta_at),
                    optional_elapsed_ms(state.last_client_emit_at),
                    converted.len(),
                    state.continuation_cached
                );
            }
            state.last_client_emit_at = Some(std::time::Instant::now());
            return Some((Ok(converted), state));
        }
        let converter_finish_started_at = std::time::Instant::now();
        let tail = if terminal_chunk.is_empty() {
            state.converter.finish()
        } else {
            terminal_chunk
        };
        log::debug!(
            "[const-api][stream-timing] id={} phase=converter_finish_complete elapsed_ms={} duration_ms={} tail_bytes={}",
            state.timing_id,
            state.started_at.elapsed().as_millis(),
            converter_finish_started_at.elapsed().as_millis(),
            tail.len()
        );
        if let Ok(text) = state.upstream_utf8.finish() {
            state.terminal_raw.push_str(&text);
        }
        let continuation_started_at = std::time::Instant::now();
        let previous_continuation = state.continuation.progress();
        let continuation = state.continuation.finish(&state.config);
        state.continuation_cached = continuation.cached;
        if !previous_continuation.terminal_seen && continuation.terminal_seen {
            state.terminal_detected_at = Some(std::time::Instant::now());
            log::debug!(
                "[const-api][stream-timing] id={} phase=eof_continuation_capture_complete elapsed_ms={} duration_ms={} cached={} request_bytes={} response_bytes={}",
                state.timing_id,
                state.started_at.elapsed().as_millis(),
                continuation_started_at.elapsed().as_millis(),
                state.continuation_cached,
                state.upstream_body.len(),
                state.terminal_raw.len()
            );
        }
        state.finished = true;
        if !tail.is_empty() {
            if let Ok(text) = state.converted_utf8.push(&tail) {
                state.converted_raw.push_str(&text);
            }
        }
        if let Ok(text) = state.converted_utf8.finish() {
            state.converted_raw.push_str(&text);
        }
        if !tail.is_empty() {
            state.converted_chunks = state.converted_chunks.saturating_add(1);
            state.converted_bytes = state.converted_bytes.saturating_add(tail.len());
            if contains_chat_done(&tail) {
                state.done_ready_at = Some(std::time::Instant::now());
                log::debug!(
                    "[const-api][stream-timing] id={} phase=client_done_ready_from_tail elapsed_ms={} after_terminal_ms={} after_last_text_delta_ms={} after_last_client_emit_ms={} tail_bytes={} continuation_cached={}",
                    state.timing_id,
                    state.started_at.elapsed().as_millis(),
                    optional_elapsed_ms(state.terminal_detected_at),
                    optional_elapsed_ms(state.last_text_delta_at),
                    optional_elapsed_ms(state.last_client_emit_at),
                    tail.len(),
                    state.continuation_cached
                );
            }
            state.last_client_emit_at = Some(std::time::Instant::now());
        }
        state.finalize_audit(LocalOpenAiStreamFinalization::UpstreamFinished);
        if tail.is_empty() {
            log::debug!(
                "[const-api][stream-timing] id={} phase=http_stream_closed elapsed_ms={} after_done_ms={} upstream_chunks={} upstream_bytes={} converted_chunks={} converted_bytes={}",
                state.timing_id,
                state.started_at.elapsed().as_millis(),
                optional_elapsed_ms(state.done_ready_at),
                state.upstream_chunks,
                state.upstream_bytes,
                state.converted_chunks,
                state.converted_bytes
            );
            None
        } else {
            Some((Ok(tail), state))
        }
    });
    response_from_stream(warp::http::StatusCode::from_u16(status)?, &headers, stream)
}

fn local_subscription_payload_response(
    payload: serde_json::Map<String, serde_json::Value>,
) -> Result<warp::reply::Response> {
    let wire = crate::surface_wire::SurfaceEnvelope::from_payload(&payload)?;
    let status = wire.status.unwrap_or(502);
    let mut headers = wire.header_map()?;
    if !headers.contains_key(reqwest::header::CONTENT_TYPE) {
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
    }
    let mut response = response_from_body(
        warp::http::StatusCode::from_u16(status)?,
        &headers,
        bytes::Bytes::from(wire.body.decode()?),
    )?;
    attach_local_model_failure_evidence(&mut response, &payload);
    Ok(response)
}

#[cfg(test)]
pub(crate) async fn forward_non_openai_subscription_local_stream_response(
    client: &Client,
    config: &SupplierConfig,
    path: &str,
    body: &str,
    upstream_override: Option<&str>,
    request_headers: Option<reqwest::header::HeaderMap>,
) -> Result<warp::reply::Response> {
    let cache_identity = subscription_body_cache_identity(body);
    forward_non_openai_subscription_local_stream_response_with_cache_identity(
        client,
        config,
        path,
        body,
        upstream_override,
        request_headers,
        cache_identity.as_deref(),
    )
    .await
}

async fn forward_non_openai_subscription_local_stream_response_with_cache_identity(
    client: &Client,
    config: &SupplierConfig,
    path: &str,
    body: &str,
    upstream_override: Option<&str>,
    request_headers: Option<reqwest::header::HeaderMap>,
    cache_identity: Option<&str>,
) -> Result<warp::reply::Response> {
    let provider = subscription_provider_from_config(config);
    let (outbound, mut inbound) = tokio::sync::mpsc::channel::<SupplierMessage>(64);
    let task_client = client.clone();
    let task_config = config.clone();
    let task_path = path.to_string();
    let task_body = body.to_string();
    let task_upstream_override = upstream_override.map(str::to_string);
    let task_request_headers = request_headers;
    let task_cache_identity = cache_identity.map(str::to_string);
    crate::spawn_logged("local subscription stream task", async move {
        let panic_outbound = outbound.clone();
        let guarded =
            crate::catch_runtime_panic("local subscription stream execution", async move {
                let result = match provider.as_str() {
                    "claude" | "anthropic" => {
                        forward_claude_subscription_stream_request_with_capacity(
                            &task_client,
                            &task_config,
                            "local-subscription-stream",
                            &task_path,
                            &task_body,
                            &outbound,
                            task_upstream_override.as_deref(),
                            None,
                            task_cache_identity.as_deref(),
                            task_request_headers.as_ref(),
                        )
                        .await
                    }
                    "antigravity" => {
                        forward_antigravity_subscription_stream_request_with_capacity(
                            &task_client,
                            &task_config,
                            "local-subscription-stream",
                            &task_path,
                            &task_body,
                            &outbound,
                            task_upstream_override.as_deref(),
                            None,
                            task_cache_identity.as_deref(),
                        )
                        .await
                    }
                    "grok" => {
                        forward_openai_subscription_stream_request_with_capacity(
                            &task_client,
                            &task_config,
                            "local-subscription-stream",
                            &task_path,
                            &task_body,
                            &outbound,
                            task_upstream_override.as_deref(),
                            None,
                            task_cache_identity.as_deref(),
                            task_request_headers.as_ref(),
                        )
                        .await
                    }
                    _ => Err(anyhow!(
                "subscription provider does not expose a local streaming executor: {provider}"
            )),
                };
                match result {
                    Ok(Some(payload)) => {
                        let _ = outbound
                            .send(SupplierMessage {
                                id: "local-subscription-stream".to_string(),
                                kind: "local_payload".to_string(),
                                payload,
                            })
                            .await;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        let _ = outbound
                            .send(SupplierMessage {
                                id: "local-subscription-stream".to_string(),
                                kind: "local_error".to_string(),
                                payload: serde_json::json!({"message": error.to_string()})
                                    .as_object()
                                    .cloned()
                                    .unwrap_or_default(),
                            })
                            .await;
                    }
                }
            })
            .await;
        if let Err(recovered) = guarded {
            let _ = panic_outbound
                .send(SupplierMessage {
                    id: "local-subscription-stream".to_string(),
                    kind: "local_error".to_string(),
                    payload: serde_json::json!({
                        "message": format!("local subscription stream panicked: {}", recovered.message)
                    })
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
                })
                .await;
        }
    });

    let first = inbound
        .recv()
        .await
        .ok_or_else(|| anyhow!("subscription stream ended before response start"))?;
    if first.kind == "local_payload" {
        return subscription_payload_as_local_stream_response(config, path, body, first.payload);
    }
    if first.kind == "local_error" {
        let message = first
            .payload
            .get("message")
            .and_then(|value| value.as_str())
            .unwrap_or("subscription stream failed");
        return Err(anyhow!(message.to_string()));
    }
    if first.kind != "stream_start" {
        return Err(anyhow!(
            "subscription stream returned {} before stream_start",
            first.kind
        ));
    }

    let start_wire = crate::surface_wire::SurfaceEnvelope::from_payload(&first.payload)?;
    let status = start_wire.status.unwrap_or(200);
    let mut headers = start_wire.header_map()?;
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("text/event-stream"),
    );
    let stream = futures_util::stream::unfold((inbound, false), |(mut inbound, done)| async move {
        if done {
            return None;
        }
        while let Some(message) = inbound.recv().await {
            match message.kind.as_str() {
                "stream_chunk" => {
                    let chunk =
                        crate::surface_wire::SurfaceEnvelope::from_payload(&message.payload)
                            .and_then(|wire| wire.body.decode())
                            .map_err(|error| {
                                std::io::Error::new(std::io::ErrorKind::InvalidData, error)
                            });
                    match chunk {
                        Ok(chunk) if !chunk.is_empty() => {
                            return Some((
                                Ok::<bytes::Bytes, std::io::Error>(bytes::Bytes::from(chunk)),
                                (inbound, false),
                            ));
                        }
                        Ok(_) => {}
                        Err(error) => return Some((Err(error), (inbound, true))),
                    }
                }
                "stream_end" => return None,
                "local_error" => {
                    let message = message
                        .payload
                        .get("message")
                        .and_then(|value| value.as_str())
                        .unwrap_or("subscription stream failed");
                    let chunk = format!(
                        "event: error\ndata: {}\n\n",
                        serde_json::json!({
                            "type": "error",
                            "error": {
                                "type": "subscription_stream_error",
                                "message": message
                            }
                        })
                    );
                    return Some((
                        Ok::<bytes::Bytes, std::io::Error>(bytes::Bytes::from(chunk)),
                        (inbound, true),
                    ));
                }
                _ => {}
            }
        }
        None
    });
    response_from_stream(warp::http::StatusCode::from_u16(status)?, &headers, stream)
}

fn subscription_payload_as_local_stream_response(
    _config: &SupplierConfig,
    _path: &str,
    _body: &str,
    payload: serde_json::Map<String, serde_json::Value>,
) -> Result<warp::reply::Response> {
    let status = crate::surface_wire::SurfaceEnvelope::from_payload(&payload)?
        .status
        .unwrap_or(502);
    if !(200..300).contains(&status) {
        return local_subscription_payload_response(payload);
    }

    // A successful buffered fallback cannot be synthesized into native SSE without decoding it.
    // Return the executor's original response instead of routing a same-protocol payload via IR.
    local_subscription_payload_response(payload)
}

fn local_subscription_status_for_client(status: u16, headers: &warp::http::HeaderMap) -> u16 {
    if status == 429 && infer_tool_from_headers(headers) == "codex" {
        return warp::http::StatusCode::CONFLICT.as_u16();
    }
    status
}
