use super::*;
use crate::protocol::{
    ir::CanonicalStreamEvent,
    kind::ProtocolKind,
    stream::{CanonicalAccumulator, StreamRenderer},
};
use event_stream::{Decoder, test_frame};

const PROFILE: &str = "arn:aws:codewhisperer:us-east-1:123456789012:profile/test-profile";

#[test]
fn kiro_requests_do_not_claim_a_const_api_user_agent() {
    let request = api_request(
        &Client::new(),
        "https://example.test/",
        "synthetic-access",
        "KiroControlPlaneBearerService.ListAvailableModels",
        &json!({}),
    )
    .build()
    .expect("Kiro request");

    assert!(!request.headers().contains_key("user-agent"));
}

fn test_supplier(directory: &std::path::Path) -> crate::SupplierConfig {
    let token = json!({"provider":"kiro","type":"kiro","auth_type":"social",
        "access_token":"synthetic-access","refresh_token":"synthetic-refresh",
        "profile_arn":PROFILE,"expires_at_unix":crate::now_unix()+3600});
    let mut channel =
        crate::detection::save_subscription_token_and_import_in_dir("kiro", token, directory)
            .unwrap();
    channel.enabled = true;
    channel.models = vec!["claude-haiku-4.5".into()];
    channel.public_model = "claude-haiku-4.5".into();
    channel.upstream_model = "claude-haiku-4.5".into();
    channel.subscription.audit_enabled = false;
    crate::supplier_from_channel_unfiltered(&channel)
}

#[test]
fn kiro_is_available_for_local_and_platform_supply_in_all_four_protocols() {
    let directory = tempfile::tempdir().unwrap();
    let mut supplier = test_supplier(directory.path());
    supplier.capability_profiles = vec![crate::model::ChannelCapabilityProfile {
        protocol: "anthropic_messages".into(),
        non_stream_json: true,
        stream_sse: true,
        tool_calls: true,
        verification_state: "declared".into(),
        capability_layer: "driver".into(),
        release_status: "supported".into(),
        ..Default::default()
    }];
    let channel = crate::channel_from_supplier(supplier.channel_id.clone(), &supplier);
    assert!(crate::supplier::availability::allows(
        &channel,
        "claude-haiku-4.5"
    ));
    assert_eq!(
        crate::supplier::availability::effective_models(&channel, channel.models.clone()),
        channel.models
    );
    let agent = crate::supplier::SupplierAgentConfig::single(supplier.clone());
    let payload = crate::supplier::supplier_register_payload(&agent, &supplier);
    assert_eq!(payload["models"], json!(channel.models));
    assert_eq!(payload["provider_kind"], "kiro");
    assert_eq!(payload["endpoint_kind"], "subscription");
    for operation in [
        "anthropic.messages",
        "openai.chat_completions",
        "openai.responses",
        "gemini.generate_content",
        "gemini.stream_generate_content",
    ] {
        assert!(
            payload["supported_operations"]
                .as_array()
                .unwrap()
                .contains(&json!(operation)),
            "{operation}"
        );
    }
    let evidence = payload["effective_capabilities"].as_array().unwrap();
    for protocol in [
        "anthropic_messages",
        "openai_chat",
        "openai_responses",
        "gemini_native",
    ] {
        assert!(
            evidence.iter().any(|item| item["protocol"] == protocol
                && item["feature"] == "tool_calls"
                && item["state"] == "supported"
                && item["conversion_level"] == "compatible"),
            "{protocol}"
        );
    }
    use crate::source_driver::{SourceDriverId, SubscriptionResponseMode};
    use crate::surface::{ApiOperation, ApiSurface};
    for (surface, operation, protocol) in [
        (
            ApiSurface::Anthropic,
            ApiOperation::Messages,
            ProtocolKind::AnthropicMessages,
        ),
        (
            ApiSurface::OpenAi,
            ApiOperation::ChatCompletions,
            ProtocolKind::OpenAiChat,
        ),
        (
            ApiSurface::OpenAi,
            ApiOperation::Responses,
            ProtocolKind::OpenAiResponses,
        ),
        (
            ApiSurface::Gemini,
            ApiOperation::GenerateContent,
            ProtocolKind::GeminiNative,
        ),
        (
            ApiSurface::Gemini,
            ApiOperation::StreamGenerateContent,
            ProtocolKind::GeminiNative,
        ),
    ] {
        for mode in [
            SubscriptionResponseMode::Buffered,
            SubscriptionResponseMode::Sse,
        ] {
            crate::source_driver::admit_subscription_operation(
                SourceDriverId::KiroSubscription,
                surface,
                operation,
                Some(protocol),
                mode,
            )
            .unwrap();
        }
    }
    assert!(
        crate::source_driver::admit_subscription_operation(
            SourceDriverId::KiroSubscription,
            ApiSurface::OpenAi,
            ApiOperation::Embeddings,
            None,
            SubscriptionResponseMode::Buffered,
        )
        .is_err()
    );
}

fn inbound_requests() -> Vec<(&'static str, Value)> {
    vec![
        (
            "/v1/messages",
            json!({"model":"claude-haiku-4.5","max_tokens":32,"messages":[{"role":"user","content":"hello"}]}),
        ),
        (
            "/v1/chat/completions",
            json!({"model":"claude-haiku-4.5","messages":[{"role":"user","content":"hello"}]}),
        ),
        (
            "/v1/responses",
            json!({"model":"claude-haiku-4.5","input":"hello"}),
        ),
        (
            "/v1beta/models/claude-haiku-4.5:generateContent",
            json!({"model":"claude-haiku-4.5","contents":[{"role":"user","parts":[{"text":"hello"}]}]}),
        ),
    ]
}

#[tokio::test]
async fn kiro_native_executor_serves_all_four_buffered_and_supplier_stream_envelopes() {
    use warp::Filter;
    let bytes = native_events(&[
        ("assistantResponseEvent", json!({"content":"hello world"})),
        ("metadataEvent", json!({"stopReason":"END_TURN"})),
        ("meteringEvent", json!({"unit":"credit","usage":0.001})),
    ]);
    let route = warp::post()
        .and(warp::header::<String>("x-amz-target"))
        .and(warp::header::<String>("content-type"))
        .and(warp::body::bytes())
        .map(
            move |target: String, content_type: String, body: bytes::Bytes| {
                assert_eq!(target, "KiroRuntimeService.GenerateAssistantResponse");
                assert_eq!(content_type, "application/x-amz-json-1.0");
                let body: Value = serde_json::from_slice(&body).unwrap();
                assert!(body.get("additionalModelRequestFields").is_none());
                assert_eq!(
                    body["conversationState"]["currentMessage"]["userInputMessage"]["modelId"],
                    "claude-haiku-4.5"
                );
                warp::http::Response::builder()
                    .header("content-type", "application/vnd.amazon.eventstream")
                    .header("x-amzn-requestid", "synthetic-request-id")
                    .header("set-cookie", "private=must-not-leak")
                    .body(bytes.clone())
                    .unwrap()
            },
        );
    let (address, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
    let server = tokio::spawn(server);
    let directory = tempfile::tempdir().unwrap();
    let supplier = test_supplier(directory.path());
    let url = format!("http://{address}/");
    for (path, body) in inbound_requests() {
        let body = body.to_string();
        let (payload, attempts) = crate::upstream_transport::with_probe_budget(
            1,
            crate::supplier::forward_kiro_subscription_request(
                &supplier,
                path,
                &body,
                None,
                Some(&url),
                None,
                Some("test-user-session"),
            ),
        )
        .await;
        assert_eq!(attempts, 1);
        let payload = payload.unwrap().unwrap();
        let wire = crate::surface_wire::SurfaceEnvelope::from_payload(&payload).unwrap();
        assert_eq!(wire.status, Some(200));
        let headers = wire.header_map().unwrap();
        assert_eq!(headers["content-type"], "application/json");
        assert!(!headers.contains_key("set-cookie"));
        assert_eq!(headers["x-amzn-requestid"], "synthetic-request-id");
        let body: Value = serde_json::from_str(wire.body.utf8().unwrap()).unwrap();
        let usage = body
            .get("usage")
            .or_else(|| body.get("usageMetadata"))
            .unwrap();
        assert_eq!(usage["const_api_usage_source"], "estimated");
        assert!(
            usage
                .get("input_tokens")
                .or_else(|| usage.get("prompt_tokens"))
                .or_else(|| usage.get("promptTokenCount"))
                .unwrap()
                .as_u64()
                .unwrap()
                > 0
        );
        assert_eq!(payload["kiro_usage_credits"], 0.001);
        assert!(!payload.contains_key("upstream_usage"));

        let (sender, mut receiver) = tokio::sync::mpsc::channel(128);
        assert!(
            crate::supplier::forward_kiro_subscription_request(
                &supplier,
                path,
                &body_for_path(path),
                Some((&sender, "kiro-test")),
                Some(&url),
                None,
                Some("test-user-session")
            )
            .await
            .unwrap()
            .is_none()
        );
        drop(sender);
        let mut stream = Vec::new();
        let mut ended = false;
        while let Some(message) = receiver.recv().await {
            assert_ne!(message.kind, "error");
            if message.kind == "stream_chunk" {
                stream.extend(
                    crate::surface_wire::SurfaceEnvelope::from_payload(&message.payload)
                        .unwrap()
                        .body
                        .decode()
                        .unwrap(),
                );
            }
            ended |= message.kind == "stream_end";
        }
        assert!(ended);
        let output = String::from_utf8(stream).unwrap();
        assert!(output.contains("hello world"));
        assert!(output.contains("const_api_usage_source"));
    }
    server.abort();
}

fn body_for_path(path: &str) -> String {
    inbound_requests()
        .into_iter()
        .find(|(candidate, _)| *candidate == path)
        .unwrap()
        .1
        .to_string()
}

#[tokio::test]
async fn kiro_cancellation_does_not_wait_for_headers_or_a_silent_upstream() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for started in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (connected, accepted) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 4096];
            assert!(socket.read(&mut buffer).await.unwrap() > 0);
            if started {
                let bytes =
                    native_events(&[("assistantResponseEvent", json!({"content":"begin"}))]);
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\nContent-Type: application/vnd.amazon.eventstream\r\n\r\n").await.unwrap();
                socket.write_all(&bytes).await.unwrap();
            }
            connected.send(()).unwrap();
            // Keep both the response and socket open until the test cancels the consumer.
            std::future::pending::<()>().await;
            drop(socket);
        });
        let directory = tempfile::tempdir().unwrap();
        let supplier = test_supplier(directory.path());
        let (sender, mut receiver) = tokio::sync::mpsc::channel(16);
        let execution = tokio::spawn(async move {
            crate::supplier::forward_kiro_subscription_request(
                &supplier,
                "/v1/messages",
                &body_for_path("/v1/messages"),
                Some((&sender, "cancel-test")),
                Some(&format!("http://{address}")),
                None,
                None,
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(3), accepted)
            .await
            .unwrap()
            .unwrap();
        if started {
            tokio::time::timeout(Duration::from_secs(3), async {
                while receiver.recv().await.unwrap().kind != "stream_chunk" {}
            })
            .await
            .unwrap();
        }
        drop(receiver);
        let error = tokio::time::timeout(Duration::from_secs(2), execution)
            .await
            .expect("consumer cancellation must not wait for upstream activity")
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains("cancelled by downstream"));
        server.abort();
    }
}

#[tokio::test]
async fn kiro_native_executor_preserves_rejections_and_does_not_complete_a_truncated_stream() {
    use warp::Filter;
    let partial = native_events(&[("assistantResponseEvent", json!({"content":"partial"}))]);
    let route = warp::post()
        .and(warp::path::param::<String>())
        .map(move |case: String| match case.as_str() {
            "forbidden" => warp::http::Response::builder()
                .status(403)
                .header("retry-after", "7")
                .body(br#"{"message":"Model unavailable in this region"}"#.to_vec())
                .unwrap(),
            "throttled" => warp::http::Response::builder()
                .status(429)
                .header("retry-after", "7")
                .body(br#"{"message":"Rate exceeded"}"#.to_vec())
                .unwrap(),
            _ => warp::http::Response::builder()
                .body(partial.clone())
                .unwrap(),
        });
    let (address, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
    let server = tokio::spawn(server);
    let directory = tempfile::tempdir().unwrap();
    let supplier = test_supplier(directory.path());
    let body = body_for_path("/v1/messages");
    for (case, status) in [("forbidden", 403), ("throttled", 429), ("partial", 502)] {
        let payload = crate::supplier::forward_kiro_subscription_request(
            &supplier,
            "/v1/messages",
            &body,
            None,
            Some(&format!("http://{address}/{case}")),
            None,
            None,
        )
        .await
        .unwrap()
        .unwrap();
        let wire = crate::surface_wire::SurfaceEnvelope::from_payload(&payload).unwrap();
        assert_eq!(wire.status, Some(status));
        if status != 502 {
            assert_eq!(wire.header_map().unwrap()["retry-after"], "7");
        } else {
            assert!(wire.body.utf8().unwrap().contains("terminal metadata"));
        }
    }
    let (sender, mut receiver) = tokio::sync::mpsc::channel(128);
    assert!(
        crate::supplier::forward_kiro_subscription_request(
            &supplier,
            "/v1/messages",
            &body,
            Some((&sender, "partial-test")),
            Some(&format!("http://{address}/partial")),
            None,
            None
        )
        .await
        .unwrap()
        .is_none()
    );
    drop(sender);
    let mut failed = false;
    while let Some(message) = receiver.recv().await {
        assert_ne!(message.kind, "stream_end");
        failed |= message.kind == "error";
    }
    assert!(failed);
    server.abort();
}

fn native_events(events: &[(&str, Value)]) -> Vec<u8> {
    events
        .iter()
        .flat_map(|(name, body)| test_frame("event", name, &serde_json::to_vec(body).unwrap()))
        .collect()
}

#[test]
fn native_stream_waits_for_clean_eof_and_retains_credit_metering_without_token_fabrication() {
    let bytes = native_events(&[
        ("initial-response", json!({"conversationId":"sess_test"})),
        ("assistantResponseEvent", json!({"content":"O"})),
        ("assistantResponseEvent", json!({"content":"K"})),
        ("metadataEvent", json!({"stopReason":"END_TURN"})),
        ("contextUsageEvent", json!({"contextUsagePercentage":0.125})),
        ("meteringEvent", json!({"unit":"credit","usage":0.005})),
    ]);
    let mut decoder = stream::StreamDecoder::new("claude-haiku-4.5");
    let mut events = Vec::new();
    for byte in bytes {
        events.extend(decoder.push(&[byte]).unwrap());
    }
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, CanonicalStreamEvent::ResponseDone(_)))
    );
    assert_eq!(decoder.credits(), Some(0.005));
    events.extend(decoder.finish().unwrap());
    let mut accumulated = CanonicalAccumulator::new();
    for event in &events {
        accumulated.push(event).unwrap();
    }
    let response = accumulated.finish().unwrap();
    assert!(response.usage.input_tokens.value.is_none());
    assert!(response.usage.output_tokens.value.is_none());
    for protocol in [
        ProtocolKind::OpenAiChat,
        ProtocolKind::OpenAiResponses,
        ProtocolKind::AnthropicMessages,
        ProtocolKind::GeminiNative,
    ] {
        let mut renderer =
            StreamRenderer::new(protocol, "claude-haiku-4.5").with_estimated_token_usage();
        let mut wire = Vec::new();
        for event in &events {
            wire.extend_from_slice(&renderer.push(event).unwrap());
        }
        renderer.finish().unwrap();
        for (_, data) in crate::protocol::stream::decode_sse_frames(&wire).unwrap() {
            if data == "[DONE]" {
                continue;
            }
            let value: Value = serde_json::from_str(&data).unwrap();
            for pointer in [
                "/usage",
                "/usageMetadata",
                "/message/usage",
                "/response/usage",
            ] {
                if let Some(usage) = value.pointer(pointer) {
                    assert_eq!(
                        usage["const_api_usage_source"], "estimated",
                        "{protocol:?}: {usage}"
                    );
                }
            }
        }
    }
}

#[test]
fn native_tool_fragments_preserve_ids_and_do_not_trust_end_turn_as_tool_finish() {
    let bytes = native_events(&[
        (
            "toolUseEvent",
            json!({"toolUseId":"call_one","name":"probe","input":"{\"ok\":"}),
        ),
        (
            "toolUseEvent",
            json!({"toolUseId":"call_one","name":"probe","input":"true}"}),
        ),
        (
            "toolUseEvent",
            json!({"toolUseId":"call_one","name":"probe","stop":true}),
        ),
        ("metadataEvent", json!({"stopReason":"END_TURN"})),
    ]);
    let mut decoder = stream::StreamDecoder::new("claude-haiku-4.5");
    let mut events = decoder.push(&bytes).unwrap();
    events.extend(decoder.finish().unwrap());
    let mut accumulated = CanonicalAccumulator::new();
    for event in &events {
        accumulated.push(event).unwrap();
    }
    let response = accumulated.finish().unwrap();
    assert_eq!(
        response.finish.reason,
        crate::protocol::ir::FinishReason::ToolCalls
    );
    let crate::protocol::ir::ContentBlock::ToolCall(call) = &response.blocks[0] else {
        panic!("tool missing")
    };
    assert_eq!(call.id, "call_one");
    assert_eq!(call.arguments, Some(json!({"ok":true})));
}

#[test]
fn partial_tools_missing_metadata_and_late_errors_are_failures() {
    let mut missing = stream::StreamDecoder::new("claude-haiku-4.5");
    missing
        .push(&native_events(&[(
            "assistantResponseEvent",
            json!({"content":"unfinished"}),
        )]))
        .unwrap();
    assert!(missing.finish().is_err());
    let mut partial = stream::StreamDecoder::new("claude-haiku-4.5");
    partial
        .push(&native_events(&[
            (
                "toolUseEvent",
                json!({"toolUseId":"call_one","name":"probe","input":"{"}),
            ),
            ("metadataEvent", json!({})),
        ]))
        .unwrap();
    assert!(partial.finish().is_err());
    let mut invalid = stream::StreamDecoder::new("claude-haiku-4.5");
    assert!(
        invalid
            .push(&native_events(&[(
                "toolUseEvent",
                json!({"toolUseId":"call_one","name":"probe","input":"{","stop":true})
            )]))
            .is_err()
    );
    let mut late = stream::StreamDecoder::new("claude-haiku-4.5");
    late.push(&native_events(&[("metadataEvent", json!({}))]))
        .unwrap();
    let error = late
        .push(&test_frame(
            "exception",
            "error",
            br#"{"message":"synthetic rejection"}"#,
        ))
        .unwrap_err();
    assert!(error.to_string().contains("synthetic rejection"));
}

fn tool_round(result: bool) -> Value {
    let mut messages = vec![json!({"role":"user","content":"Call probe"})];
    if result {
        messages.push(json!({"role":"assistant","content":[{"type":"tool_use","id":"call_one","name":"probe","input":{"ok":true,"images":[],"kind":"keep"}}]}));
        messages.push(json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"call_one","content":"ok=true"}]}));
    }
    json!({"model":"claude-haiku-4.5","max_tokens":32,"system":"Be brief", "messages":messages,
        "tools":[{"name":"probe","input_schema":{"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"]}}]})
}

#[test]
fn native_requests_keep_tool_payloads_and_scoped_conversation_and_continuation_ids() {
    let prepare = |body: &Value, profile: &str, session: &str| {
        request::prepare(
            ProtocolKind::AnthropicMessages,
            body,
            "claude-haiku-4.5",
            profile,
            Some(session),
            None,
        )
        .unwrap()
        .body
    };
    let first = prepare(&tool_round(false), PROFILE, "user-a-session-1");
    let resumed = prepare(&tool_round(true), PROFILE, "user-a-session-1");
    assert_eq!(
        first["conversationState"]["conversationId"],
        resumed["conversationState"]["conversationId"]
    );
    assert_eq!(
        first["conversationState"]["agentContinuationId"],
        resumed["conversationState"]["agentContinuationId"]
    );
    assert_eq!(resumed.pointer("/conversationState/currentMessage/userInputMessage/userInputMessageContext/toolResults/0/toolUseId").unwrap(), "call_one");
    assert_eq!(
        resumed
            .pointer("/conversationState/history/1/assistantResponseMessage/toolUses/0/input")
            .unwrap(),
        &json!({"ok":true,"images":[],"kind":"keep"})
    );
    assert!(first.get("additionalModelRequestFields").is_none());
    for isolated in [
        prepare(&tool_round(false), PROFILE, "user-b-session-1"),
        prepare(
            &tool_round(false),
            &PROFILE.replace("test-profile", "other-profile"),
            "user-a-session-1",
        ),
    ] {
        assert_ne!(
            first["conversationState"]["conversationId"],
            isolated["conversationState"]["conversationId"]
        );
    }
    let mut next = tool_round(false);
    next["messages"].as_array_mut().unwrap().extend([
        json!({"role":"assistant","content":"done"}),
        json!({"role":"user","content":"Next task"}),
    ]);
    let next = prepare(&next, PROFILE, "user-a-session-1");
    assert_eq!(
        first["conversationState"]["conversationId"],
        next["conversationState"]["conversationId"]
    );
    assert_ne!(
        first["conversationState"]["agentContinuationId"],
        next["conversationState"]["agentContinuationId"]
    );
}

#[test]
fn kiro_cache_prefix_survives_tool_order_and_current_response_controls() {
    let mut body = tool_round(true);
    let mut another_tool = body["tools"][0].clone();
    another_tool["name"] = json!("alpha");
    body["tools"].as_array_mut().unwrap().push(another_tool);
    let prepare = |body: &Value| {
        request::prepare(
            ProtocolKind::AnthropicMessages,
            body,
            "claude-haiku-4.5",
            PROFILE,
            Some("scoped-cache-session"),
            None,
        )
        .unwrap()
        .body
    };
    let automatic = prepare(&body);
    body["tools"].as_array_mut().unwrap().reverse();
    assert_eq!(automatic, prepare(&body));

    body["tool_choice"] = json!({"type":"any"});
    let required = prepare(&body);
    let automatic = &automatic["conversationState"];
    let required = &required["conversationState"];
    assert_eq!(automatic["history"], required["history"]);
    assert_eq!(automatic["conversationId"], required["conversationId"]);
    assert_eq!(
        automatic["agentContinuationId"],
        required["agentContinuationId"]
    );
    assert_eq!(
        automatic["currentMessage"]["userInputMessage"]["userInputMessageContext"],
        required["currentMessage"]["userInputMessage"]["userInputMessageContext"]
    );
    let current = required["currentMessage"]["userInputMessage"]["content"]
        .as_str()
        .unwrap();
    assert!(current.contains("For the next response, call an available tool."));
    assert!(
        !required["history"]
            .to_string()
            .contains("For the next response")
    );
    assert!(
        required["history"][0]["userInputMessage"]["content"]
            .as_str()
            .unwrap()
            .contains("Be brief")
    );
}

#[test]
fn responses_additional_namespace_custom_tools_use_the_existing_reversible_bridge() {
    let body = json!({"model":"claude-haiku-4.5","input":[{"type":"additional_tools","role":"developer","tools":[
        {"type":"namespace","name":"editor","tools":[{"type":"custom","name":"apply","format":{"type":"text"}}]}]},
        {"type":"message","role":"user","content":"Run the tool"}]});
    let prepared = request::prepare(
        ProtocolKind::OpenAiResponses,
        &body,
        "claude-haiku-4.5",
        PROFILE,
        Some("scoped-session"),
        None,
    )
    .unwrap();
    let specification = prepared.body.pointer("/conversationState/currentMessage/userInputMessage/userInputMessageContext/tools/0/toolSpecification").unwrap();
    assert_eq!(
        specification["inputSchema"]["json"]["properties"]["input"]["type"],
        "string"
    );
    assert!(!prepared.tool_mapping.is_empty());
}

#[test]
fn generation_controls_follow_the_account_model_schema_and_limits() {
    let schema = json!({"type":"object","properties":{
        "max_tokens":{"type":"integer","minimum":1024,"maximum":64000},
        "thinking":{"type":"object","properties":{"type":{"enum":["adaptive","between_tools"]}}},
        "output_config":{"type":"object","properties":{"effort":{"enum":["low","medium","high"]}}}
    }});
    let prepare = |body: &Value, model: &str, schema: Option<&Value>| {
        request::prepare(
            ProtocolKind::AnthropicMessages,
            body,
            model,
            PROFILE,
            None,
            schema,
        )
        .unwrap()
    };
    let mut body = tool_round(false);
    body["thinking"] = json!({"type":"disabled"});
    let old = prepare(&body, "claude-haiku-4.5", None);
    assert!(old.body.get("additionalModelRequestFields").is_none());
    assert!(!old.adjustments.is_empty());
    let modern = prepare(&body, "claude-sonnet-5.5", Some(&schema));
    assert_eq!(
        modern.body["additionalModelRequestFields"],
        json!({"max_tokens":1024})
    );
    body["max_tokens"] = json!(999999);
    body["thinking"] = json!({"type":"adaptive"});
    body["output_config"] = json!({"effort":"high"});
    let modern = prepare(&body, "claude-sonnet-5.5", Some(&schema));
    assert_eq!(
        modern.body["additionalModelRequestFields"],
        json!({"max_tokens":64000,"thinking":{"type":"adaptive"},"output_config":{"effort":"high"}})
    );

    let gpt_schema = json!({"properties":{"reasoning":{"properties":{"effort":{"enum":["low","medium","high"]}}}}});
    let gpt = request::prepare(
        ProtocolKind::OpenAiResponses,
        &json!({"model":"gpt-5.6","input":"OK","reasoning":{"effort":"high"}}),
        "gpt-5.6",
        PROFILE,
        None,
        Some(&gpt_schema),
    )
    .unwrap();
    assert_eq!(
        gpt.body["additionalModelRequestFields"],
        json!({"reasoning":{"effort":"high"}})
    );
}

#[test]
fn catalog_preserves_native_schema_objects_and_encoded_json() {
    let schema =
        json!({"properties":{"max_tokens":{"type":"integer","minimum":1024,"maximum":64000}}});
    for wire in [schema.clone(), json!(schema.to_string())] {
        let models = catalog_page(
            &json!({"models":[{"modelId":"claude-new","additionalModelRequestFieldsSchema":wire}]}),
        )
        .unwrap();
        assert_eq!(models[0].kiro_request_schema.as_ref(), Some(&schema));
    }
    assert!(
        catalog_page(
            &json!({"models":[{"modelId":"claude-new","additionalModelRequestFieldsSchema":123}]})
        )
        .is_err()
    );
}

#[test]
fn login_uses_portal_root_but_callback_includes_identity_provider() {
    let url = Url::parse(&authorization_url("test-state", "challenge").unwrap()).unwrap();
    assert_eq!(url.host_str(), Some("app.kiro.dev"));
    let query: std::collections::HashMap<_, _> = url.query_pairs().collect();
    assert_eq!(query["redirect_uri"], "http://localhost:3128");
    assert_eq!(query["redirect_from"], "kirocli");
    let callback = format!("{CALLBACK}?state=test-state&code=synthetic&login_option=google");
    assert_eq!(
        callback_provider(&callback, "test-state").unwrap(),
        "google"
    );
    for invalid in [
        callback.replace("localhost", "evil.example"),
        callback.replace("test-state", "other"),
        format!("{callback}&state=test-state"),
        callback.replace("google", "builderid"),
        "synthetic-code".to_string(),
    ] {
        assert!(callback_provider(&invalid, "test-state").is_err());
    }
}

#[test]
fn token_rotation_preserves_missing_refresh_and_profile_without_reviving_old_access() {
    let old = json!({"auth_type":"social","profile_arn":PROFILE,"refresh_token":"old-refresh","access_token":"old-access"});
    let new = normalize_token(
        json!({"accessToken":"new-access","expiresIn":3600}),
        Some(&old),
    )
    .unwrap();
    assert_eq!(new["refresh_token"], "old-refresh");
    assert_eq!(new["profile_arn"], PROFILE);
    assert_eq!(new["access_token"], "new-access");
    assert_eq!(new["auth_type"], "social");
    assert!(normalize_token(json!({"expiresIn":3600}), Some(&old)).is_err());
    assert!(normalize_token(json!({"accessToken":"new","expiresIn":0}), Some(&old)).is_err());
    let rotated = normalize_token(
        json!({"accessToken":"new","refreshToken":"rotated","expiresIn":3600}),
        Some(&old),
    )
    .unwrap();
    assert_eq!(rotated["refresh_token"], "rotated");
}

#[test]
fn profile_cannot_redirect_bearer_to_an_arbitrary_host() {
    assert_eq!(profile_region(PROFILE).unwrap(), "us-east-1");
    for region in ["us-east-1.evil.example", "us/east/1", "user@us-east-1", ""] {
        assert!(profile_region(&PROFILE.replace("us-east-1", region)).is_err());
    }
    assert!(profile_region(&PROFILE.replace("profile/test-profile", "profile/../other")).is_err());
}

#[test]
fn catalog_uses_account_wire_ids_and_declared_limits_without_static_models_or_prices() {
    let models = catalog_page(
        &json!({"models":[{"modelId":"claude-haiku-4.5", "rateMultiplier":0.4,
        "rateUnit":"Credit","tokenLimits":{"maxInputTokens":200000,"maxOutputTokens":64000}}]}),
    )
    .unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "claude-haiku-4.5");
    assert_eq!(models[0].context_tokens, Some(200000));
    assert_eq!(models[0].output_tokens, Some(64000));
    assert!(models[0].probe_cost_nanos.is_none());
    assert!(catalog_page(&json!({"models":[{"modelName":"Missing ID"}]})).is_err());
    assert!(
        catalog_page(&json!({"models":[{"modelId":"test","tokenLimits":{"maxInputTokens":-1}}]}))
            .is_err()
    );
}

fn free_quota() -> Value {
    json!({"overageConfiguration":{"overageStatus":"DISABLED"},"usageBreakdownList":[{
        "resourceType":"CREDIT","usageLimit":50,"usageLimitWithPrecision":50.0,
        "currentUsage":49,"currentUsageWithPrecision":49.75,"nextDateReset":3000,
        "freeTrialInfo":{"freeTrialStatus":"EXPIRED","freeTrialExpiry":1500,
            "usageLimit":500,"currentUsage":106.16},"bonuses":[]}]})
}

#[test]
fn expired_trial_is_not_available_and_sub_one_percent_is_not_exhausted() {
    let windows = quota_windows(&free_quota(), 2000).unwrap();
    assert_eq!(windows.len(), 1);
    assert!((windows[0].remaining_ratio - 0.005).abs() < 1e-12);
    assert!(crate::supplier::quota_window_limits_supply(&windows[0]));
    assert!(windows[0].used_percent < 100.0);
    let mut exhausted = free_quota();
    exhausted["usageBreakdownList"][0]["currentUsageWithPrecision"] = json!(50.0);
    assert_eq!(
        quota_windows(&exhausted, 2000).unwrap()[0].remaining_ratio,
        0.0
    );
}

#[test]
fn active_pools_are_additive_and_user_enabled_overage_is_not_blocked_by_included_balance() {
    let mut value = free_quota();
    value["usageBreakdownList"][0]["currentUsageWithPrecision"] = json!(50.0);
    value["usageBreakdownList"][0]["freeTrialInfo"] = json!({"freeTrialStatus":"ACTIVE",
        "freeTrialExpiry":2500,"usageLimit":100,"currentUsageWithPrecision":10.5});
    value["usageBreakdownList"][0]["bonuses"] = json!([
        {"status":"ACTIVE","usageLimit":20,"currentUsage":0,"expiresAt":4000},
        {"status":"EXPIRED","usageLimit":900,"currentUsage":0,"expiresAt":1000}]);
    let windows = quota_windows(&value, 2000).unwrap();
    assert!((windows[0].remaining_ratio - 109.5 / 170.0).abs() < 1e-12);
    assert_eq!(windows[0].reset_at_unix, 2500);
    value["overageConfiguration"]["overageStatus"] = json!("ENABLED");
    assert!(!crate::supplier::quota_window_limits_supply(
        &quota_windows(&value, 2000).unwrap()[0]
    ));
}

#[test]
fn frame_decoder_handles_every_split_and_coalesced_messages() {
    let first = test_frame("event", "assistantResponseEvent", br#"{"content":"OK"}"#);
    let second = test_frame("event", "metadataEvent", br#"{"stopReason":"END_TURN"}"#);
    let all = [first, second].concat();
    for split in 0..=all.len() {
        let mut decoder = Decoder::default();
        let mut frames = decoder.push(&all[..split]).unwrap();
        frames.extend(decoder.push(&all[split..]).unwrap());
        decoder.finish().unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].headers[":event-type"], "assistantResponseEvent");
    }
    let mut decoder = Decoder::default();
    let mut count = 0;
    for byte in all {
        count += decoder.push(&[byte]).unwrap().len();
    }
    decoder.finish().unwrap();
    assert_eq!(count, 2);
}

#[test]
fn corrupted_truncated_and_oversized_frames_are_not_successful_completion() {
    let frame = test_frame("event", "metadataEvent", b"{}");
    for changed in [0, 8, 14, frame.len() - 1] {
        let mut invalid = frame.clone();
        invalid[changed] ^= 1;
        assert!(Decoder::default().push(&invalid).is_err());
    }
    for cut in 1..frame.len() {
        let mut decoder = Decoder::default();
        decoder.push(&frame[..cut]).unwrap();
        assert!(decoder.finish().is_err());
    }
    let mut oversized = Vec::new();
    oversized.extend_from_slice(&u32::MAX.to_be_bytes());
    oversized.extend_from_slice(&0_u32.to_be_bytes());
    oversized.extend_from_slice(&crc32fast::hash(&oversized).to_be_bytes());
    assert!(Decoder::default().push(&oversized).is_err());
}

#[tokio::test]
async fn oauth_rejection_preserves_status_and_redacts_echoed_credential() {
    use warp::Filter;
    let route = warp::any().map(|| {
        warp::reply::with_status(
            warp::reply::json(
                &json!({"error":"invalid_grant","message":"rejected synthetic-refresh"}),
            ),
            warp::http::StatusCode::BAD_REQUEST,
        )
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(warp::serve(route).incoming(listener).run());
    let client = Client::builder().no_proxy().build().unwrap();
    let error = oauth_post(
        &client,
        &format!("http://{address}"),
        &json!({}),
        &["synthetic-refresh"],
    )
    .await
    .unwrap_err();
    let wire = crate::subscription_oauth_error::oauth_wire_error_from_anyhow(&error).unwrap();
    assert_eq!(wire.status, 401);
    assert!(!wire.message.contains("synthetic-refresh"));
    task.abort();
}
