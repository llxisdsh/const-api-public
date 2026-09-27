// Outcome observation never changes response bytes, retry permission or billing.
// Unknown stream extensions remain transparent and are not treated as success.
struct LocalModelOutcomeGuard {
    shared: Arc<ProxyShared>,
    channel_id: String,
    model: String,
    route_key: String,
    sequence: u64,
    probe_until: i64,
}

impl LocalModelOutcomeGuard {
    fn new(shared: Arc<ProxyShared>, channel_id: &str, model: &str) -> Self {
        let route_key = model_quota_route_key(channel_id, model);
        let state = shared
            .local_model_quota_routes
            .lock()
            .ok()
            .and_then(|routes| routes.get(&route_key).copied())
            .unwrap_or_default();
        Self {
            shared,
            channel_id: channel_id.to_string(),
            model: model.to_string(),
            route_key,
            sequence: state.runtime_failure_sequence,
            probe_until: state.runtime_probe_until_unix,
        }
    }

    fn success(&self) {
        if let Ok(mut routes) = self.shared.local_model_quota_routes.lock() {
            if let Some(state) = routes.get_mut(&self.route_key) {
                state.record_success_observed(self.sequence);
            }
        }
    }

    fn failure(&self, error: crate::protocol::ir::CanonicalError) {
        let details = error.details.unwrap_or_else(|| {
            serde_json::json!({
                "type": error.code, "message": error.message,
            })
        });
        let explicit_status = details
            .get("status")
            .or_else(|| details.get("code"))
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u16::try_from(value).ok())
            .filter(|status| (400..600).contains(status));
        let body = serde_json::json!({"error": details}).to_string();
        let Some(failure) = crate::upstream_failure::observe(explicit_status.unwrap_or(200), &body, &[], Some(&self.model)) else { return; };
        if failure.cause == "unknown" { return; }
        let status = failure.effective_status();
        let mut payload =
            subscription_http_response_payload(status, "application/json", body.clone());
        insert_subscription_route_model_failure_evidence(
            &mut payload,
            status,
            &body,
            Some(&self.model),
        );
        let evidence = local_model_failure_evidence_from_payload(&payload);
        let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
        let model_scoped = self.shared.record_local_model_response(
            &self.channel_id,
            &self.model,
            status,
            &warp::http::HeaderMap::new(),
            evidence.as_ref(),
        );
        if !model_scoped
            && payload
                .get("failure_scope")
                .and_then(serde_json::Value::as_str)
                == Some("channel")
        {
            self.shared.set_local_channel_ready(&self.channel_id, false);
        }
    }
}

impl Drop for LocalModelOutcomeGuard {
    fn drop(&mut self) {
        if let Ok(mut routes) = self.shared.local_model_quota_routes.lock() {
            if let Some(state) = routes.get_mut(&self.route_key) {
                // An older in-flight result must not release a newer probe.
                if state.runtime_failure_sequence == self.sequence
                    && state.runtime_probe_until_unix == self.probe_until
                {
                    state.release_runtime_probe();
                }
            }
        }
    }
}

fn observe_local_model_response(
    response: warp::reply::Response,
    observation: LocalModelOutcomeGuard,
    protocol: Option<crate::protocol::kind::ProtocolKind>,
) -> warp::reply::Response {
    let is_event_stream = response_headers_are_event_stream(response.headers());
    let observe_json = !is_event_stream && protocol.is_some() && response.headers()
        .get("content-type").and_then(|h| h.to_str().ok()).is_some_and(|h| h.contains("json"));
    let (parts, body) = response.into_parts();
    let parser = protocol
        .filter(|_| is_event_stream)
        .map(|protocol| crate::protocol::stream::StreamParser::new(protocol, &observation.model));
    let stream = futures_util::stream::unfold(
        (
            body.into_data_stream(),
            parser,
            observation,
            !is_event_stream && !observe_json,
            observe_json.then(Vec::<u8>::new),
        ),
        |(mut stream, mut parser, observation, mut recover_on_eof, mut json)| async move {
            let chunk = stream.next().await;
            if matches!(&chunk, Some(Err(_))) {
                recover_on_eof = false;
                parser = None;
                json = None;
            }
            if let (Some(buffer), Some(Ok(bytes))) = (&mut json, &chunk) {
                if buffer.len().saturating_add(bytes.len()) <= crate::upstream_failure::MAX_OBSERVATION_BYTES {
                    buffer.extend_from_slice(bytes);
                } else { json = None; }
            }
            if chunk.is_none() {
                if let Some(buffer) = json.take() {
                    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&buffer) {
                        if let Some((envelope, error)) = crate::upstream_failure::error_object(&value) {
                            let mut detail = error.clone();
                            let canonical = crate::openrouter::canonical_error_type(envelope, error);
                            if !canonical.is_empty() { if let Some(object) = detail.as_object_mut() { object.insert("error_type".into(), canonical.into()); } }
                            observation.failure(crate::protocol::ir::CanonicalError {
                                code: "provider_error".into(), message: String::new(), retryable: false, provider_code: None, details: Some(detail),
                            });
                        } else if crate::upstream_failure::completed_json(&value) { observation.success(); }
                    }
                }
            }
            // Buffered JSON and binary media must also finish successfully;
            // response headers alone cannot prove a complete model operation.
            if chunk.is_none() && recover_on_eof {
                observation.success();
            }
            let events = match (parser.as_mut(), &chunk) {
                (Some(parser), Some(Ok(bytes))) => Some(parser.push(bytes)),
                (Some(parser), None) => Some(parser.finish()),
                _ => None,
            };
            if let Some(events) = events {
                match events {
                    Ok(events) => {
                        use crate::protocol::ir::{CanonicalStreamEvent, FinishReason};
                        for event in events {
                            match event {
                                CanonicalStreamEvent::Error(error) => {
                                    observation.failure(error);
                                    parser = None;
                                    break;
                                }
                                CanonicalStreamEvent::ResponseDone(finish) => {
                                    if !matches!(
                                        finish.reason,
                                        FinishReason::Error
                                            | FinishReason::Cancelled
                                            | FinishReason::Unknown
                                    ) {
                                        observation.success();
                                    }
                                    parser = None;
                                    break;
                                }
                                _ => {}
                            }
                        }
                    }
                    // Observation is best-effort, not a new protocol validator.
                    Err(_) => parser = None,
                }
            }
            chunk.map(|chunk| (chunk, (stream, parser, observation, recover_on_eof, json)))
        },
    );
    let body = warp::reply::stream(ThreadSafeStream::new(stream))
        .into_response()
        .into_body();
    warp::http::Response::from_parts(parts, body)
}

#[cfg(test)]
mod model_health_tests {
    use super::*;
    use crate::protocol::kind::ProtocolKind;

    fn degraded_model() -> Arc<ProxyShared> {
        let shared = Arc::new(ProxyShared::from_config(default_config(), Client::new()));
        let mut state = LocalModelQuotaRouteState::default();
        state.record_runtime_failure(now_unix(), 60, false);
        state.runtime_cooldown_until_unix = 0;
        shared
            .local_model_quota_routes
            .lock()
            .unwrap()
            .insert(model_quota_route_key("channel", "model-a"), state);
        shared
    }

    fn failures(shared: &ProxyShared) -> u32 {
        shared.local_model_quota_routes.lock().unwrap()
            [&model_quota_route_key("channel", "model-a")]
            .runtime_failure_count
    }

    fn stream_response(raw: &'static str) -> warp::reply::Response {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("content-type", "text/event-stream".parse().unwrap());
        let chunks = raw
            .as_bytes()
            .chunks(7)
            .map(|part| Ok::<_, Infallible>(bytes::Bytes::copy_from_slice(part)))
            .collect::<Vec<_>>();
        response_from_stream(StatusCode::OK, &headers, futures_util::stream::iter(chunks)).unwrap()
    }

    #[tokio::test]
    async fn json_semantics_preserve_bytes_and_only_completed_success_heals() {
        for (raw, expected) in [
            (r#"{"choices":[{"finish_reason":"stop","message":{"content":"OK"}}]}"#, 0),
            (r#"{"error":{"code":"rate_limit_exceeded","message":"slow down"}}"#, 2),
            (r#"{"future":{"text":"OK"}}"#, 1),
            (r#"{"choices":[{"finish_reason":"stop","message":{"content":"error quota risk"}}]}"#, 0),
        ] {
            let shared = degraded_model();
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert("content-type", "application/json".parse().unwrap());
            let chunks = raw.as_bytes().chunks(3).map(|part| Ok::<_, Infallible>(bytes::Bytes::copy_from_slice(part))).collect::<Vec<_>>();
            let response = response_from_stream(StatusCode::OK, &headers, futures_util::stream::iter(chunks)).unwrap();
            let response = observe_local_model_response(response, LocalModelOutcomeGuard::new(shared.clone(), "channel", "model-a"), Some(ProtocolKind::OpenAiChat));
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(bytes.as_ref(), raw.as_bytes()); assert_eq!(failures(&shared), expected, "{raw}");
        }
    }

    #[tokio::test]
    async fn model_health_stream_success_is_observed_after_terminal_without_changing_bytes() {
        for (protocol, raw) in [
            (
                ProtocolKind::OpenAiResponses,
                "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}\n\n",
            ),
            (
                ProtocolKind::OpenAiChat,
                "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
            ),
            (
                ProtocolKind::AnthropicMessages,
                "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
            ),
            (
                ProtocolKind::GeminiNative,
                "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"ok\"}]},\"finishReason\":\"STOP\"}]}\n\n",
            ),
        ] {
            let shared = degraded_model();
            let response = observe_local_model_response(
                stream_response(raw),
                LocalModelOutcomeGuard::new(shared.clone(), "channel", "model-a"),
                Some(protocol),
            );
            assert_eq!(
                failures(&shared),
                1,
                "200 headers alone cannot recover {protocol}"
            );
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(bytes.as_ref(), raw.as_bytes());
            assert_eq!(
                failures(&shared),
                0,
                "completed {protocol} should recover its exact model"
            );
        }
    }

    #[tokio::test]
    async fn model_health_stream_error_unknown_and_cancel_do_not_invent_success() {
        for (protocol, raw, expected) in [
            (
                ProtocolKind::AnthropicMessages,
                "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"overloaded\"}}\n\n",
                2,
            ),
            (
                ProtocolKind::AnthropicMessages,
                "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"invalid input\"}}\n\n",
                1,
            ),
            (
                ProtocolKind::OpenAiResponses,
                "data: {\"type\":\"future.event\"}\n\n",
                1,
            ),
        ] {
            let shared = degraded_model();
            let response = observe_local_model_response(
                stream_response(raw),
                LocalModelOutcomeGuard::new(shared.clone(), "channel", "model-a"),
                Some(protocol),
            );
            assert_eq!(
                response
                    .into_body()
                    .collect()
                    .await
                    .unwrap()
                    .to_bytes()
                    .as_ref(),
                raw.as_bytes()
            );
            assert_eq!(failures(&shared), expected);
        }
        let shared = degraded_model();
        shared
            .local_model_quota_routes
            .lock()
            .unwrap()
            .get_mut(&model_quota_route_key("channel", "model-a"))
            .unwrap()
            .claim_runtime_probe(now_unix());
        let response = observe_local_model_response(
            stream_response("data: {}\n\n"),
            LocalModelOutcomeGuard::new(shared.clone(), "channel", "model-a"),
            Some(ProtocolKind::OpenAiResponses),
        );
        drop(response);
        let state = shared.local_model_quota_routes.lock().unwrap()
            [&model_quota_route_key("channel", "model-a")];
        assert_eq!(state.runtime_failure_count, 1);
        assert_eq!(
            state.runtime_probe_until_unix, 0,
            "cancel must release the recovery lease"
        );
    }

    #[tokio::test]
    async fn model_health_non_sse_response_recovers_only_after_body_completes() {
        for broken in [false, true] {
            let shared = degraded_model();
            let mut chunks = vec![Ok(bytes::Bytes::from_static(b"audio-or-json-body"))];
            if broken {
                chunks.push(Err(std::io::Error::other("connection interrupted")));
            }
            let response = response_from_stream(
                StatusCode::OK,
                &reqwest::header::HeaderMap::new(),
                futures_util::stream::iter(chunks),
            )
            .unwrap();
            let response = observe_local_model_response(
                response,
                LocalModelOutcomeGuard::new(shared.clone(), "channel", "model-a"),
                None,
            );
            assert_eq!(failures(&shared), 1);
            let body = response.into_body().collect().await;
            if broken {
                assert!(body.is_err());
                assert_eq!(failures(&shared), 1);
            } else {
                assert_eq!(body.unwrap().to_bytes().as_ref(), b"audio-or-json-body");
                assert_eq!(failures(&shared), 0);
            }
        }
    }

    #[tokio::test]
    async fn model_health_old_success_and_other_model_cannot_erase_failure() {
        let shared = degraded_model();
        let old = LocalModelOutcomeGuard::new(shared.clone(), "channel", "model-a");
        shared
            .local_model_quota_routes
            .lock()
            .unwrap()
            .get_mut(&model_quota_route_key("channel", "model-a"))
            .unwrap()
            .record_runtime_failure(now_unix(), 60, false);
        old.success();
        {
            let mut routes = shared.local_model_quota_routes.lock().unwrap();
            let state = routes
                .get_mut(&model_quota_route_key("channel", "model-a"))
                .unwrap();
            state.runtime_cooldown_until_unix = 0;
            assert!(state.claim_runtime_probe(now_unix()));
        }
        drop(old);
        assert!(
            shared.local_model_quota_routes.lock().unwrap()
                [&model_quota_route_key("channel", "model-a")]
                .runtime_probe_until_unix
                > 0,
            "an old result must not release a newer recovery request"
        );
        LocalModelOutcomeGuard::new(shared.clone(), "channel", "model-b").success();
        assert_eq!(failures(&shared), 2);
        LocalModelOutcomeGuard::new(shared.clone(), "channel", "model-a").success();
        assert_eq!(failures(&shared), 0);
        for op in [
            crate::surface::ApiOperation::GetModel,
            crate::surface::ApiOperation::CountTokens,
            crate::surface::ApiOperation::ResponsesInputTokens,
        ] {
            assert!(!local_operation_proves_model_health(op));
        }
    }

    #[test]
    fn model_health_missing_resource_is_not_model_outage_and_weak_wire_stays_compatible() {
        for body in [
            r#"{"error":{"code":"item_not_found","message":"Item rs_old is not persisted"}}"#,
            r#"{"error":{"code":"previous_response_not_found","message":"not found"}}"#,
        ] {
            let mut payload =
                subscription_http_response_payload(404, "application/json", body.into());
            insert_subscription_route_model_failure_evidence(
                &mut payload,
                404,
                body,
                Some("model-a"),
            );
            assert_eq!(payload["failure_scope"], "request");
            assert!(local_model_failure_evidence_from_payload(&payload).is_none());
        }
        let body = r#"{"error":{"message":"temporarily unavailable"}}"#;
        let mut payload = subscription_http_response_payload(503, "application/json", body.into());
        insert_subscription_route_model_failure_evidence(&mut payload, 503, body, Some("model-a"));
        assert_eq!(
            payload["failure_scope"], "request",
            "old servers must retain request-local handling"
        );
        let evidence = local_model_failure_evidence_from_payload(&payload).unwrap();
        assert!(evidence.soft);
        assert_eq!(evidence.failure_model, "model-a");
    }
}
