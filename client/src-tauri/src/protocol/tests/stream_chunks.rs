use crate::protocol::convert_non_stream;
use crate::protocol::ir::{
    ArtifactAffinity, ArtifactKind, BlockMetadata, CanonicalError, CanonicalResponseV2,
    CanonicalStreamEvent, ContentBlock, FinishDetail, FinishReason, MediaBlock, MediaSource,
    ResponseStatus, TextBlock, Usage,
};
use crate::protocol::kind::ProtocolKind;
use crate::protocol::stream::{
    CanonicalAccumulator, MAX_STREAM_BYTES, StreamParser, StreamRenderer, Utf8ChunkDecoder,
    decode_sse_frames, events_from_response_with_notices, stream_converter,
    stream_rendering_notices,
};

const PROTOCOLS: [ProtocolKind; 4] = [
    ProtocolKind::OpenAiResponses,
    ProtocolKind::OpenAiChat,
    ProtocolKind::AnthropicMessages,
    ProtocolKind::GeminiNative,
];

fn stream_fixture(protocol: ProtocolKind) -> Vec<u8> {
    let bytes: &[u8] = match protocol {
        ProtocolKind::OpenAiResponses => {
            include_bytes!("../../../tests/fixtures/protocol-v2/openai-responses/basic-tools.sse")
        }
        ProtocolKind::OpenAiChat => {
            include_bytes!("../../../tests/fixtures/protocol-v2/openai-chat/basic-tools.sse")
        }
        ProtocolKind::AnthropicMessages => {
            include_bytes!("../../../tests/fixtures/protocol-v2/anthropic-messages/basic-tools.sse")
        }
        ProtocolKind::GeminiNative => {
            include_bytes!("../../../tests/fixtures/protocol-v2/gemini-native/basic-tools.sse")
        }
    };
    bytes.to_vec()
}
fn parse_chunks(protocol: ProtocolKind, input: &[u8], cuts: &[usize]) -> Vec<CanonicalStreamEvent> {
    let mut parser = StreamParser::new(protocol, "source-model");
    let mut events = Vec::new();
    let mut start = 0;
    for &end in cuts {
        events.extend(parser.push(&input[start..end]).expect("stream chunk"));
        start = end;
    }
    events.extend(parser.push(&input[start..]).expect("stream tail"));
    events.extend(parser.finish().expect("stream finish"));
    events
}

fn every_boundary(length: usize) -> Vec<usize> {
    (1..length).collect()
}

fn randomized_boundaries(length: usize) -> Vec<usize> {
    let mut state = 0x9e37_79b9_u32;
    let mut offset = 0usize;
    let mut cuts = Vec::new();
    while offset < length {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        offset = (offset + 1 + (state as usize % 17)).min(length);
        if offset < length {
            cuts.push(offset);
        }
    }
    cuts
}

fn final_response(events: &[CanonicalStreamEvent]) -> CanonicalResponseV2 {
    let mut accumulator = CanonicalAccumulator::new();
    for event in events {
        accumulator.push(event).expect("accumulate event");
    }
    accumulator.finish().expect("final response")
}

fn response_text(events: &[CanonicalStreamEvent]) -> String {
    final_response(events)
        .blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn all_protocol_streams_are_independent_of_utf8_and_sse_chunk_boundaries() {
    for protocol in PROTOCOLS {
        let input = stream_fixture(protocol);
        let expected = parse_chunks(protocol, &input, &[]);
        assert_eq!(response_text(&expected), "Hello 世界", "{protocol}");
        assert_eq!(
            parse_chunks(protocol, &input, &every_boundary(input.len())),
            expected,
            "{protocol} every boundary"
        );
        assert_eq!(
            parse_chunks(protocol, &input, &randomized_boundaries(input.len())),
            expected,
            "{protocol} randomized boundaries"
        );
    }
}

#[test]
fn multiline_sse_data_crlf_and_duplicate_terminal_produce_one_done_event() {
    let input = concat!(
        "data: {\"id\":\"chat-1\",\n",
        "data: \"model\":\"source-model\",\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\r\n\r\n",
        "data: [DONE]\r\n\r\n",
        "data: [DONE]\r\n\r\n"
    );
    let events = parse_chunks(
        ProtocolKind::OpenAiChat,
        input.as_bytes(),
        &every_boundary(input.len()),
    );
    assert_eq!(response_text(&events), "ok");
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, CanonicalStreamEvent::ResponseDone(_)))
            .count(),
        1
    );
}

#[test]
fn four_by_four_stream_rendering_roundtrips_final_text_and_terminal_reason() {
    for source in PROTOCOLS {
        let source_events = parse_chunks(source, &stream_fixture(source), &[]);
        for target in PROTOCOLS {
            let mut renderer = StreamRenderer::new(target, "target-model");
            let mut wire = Vec::new();
            for event in &source_events {
                wire.extend_from_slice(&renderer.push(event).expect("render event"));
            }
            wire.extend_from_slice(&renderer.finish().expect("render finish"));
            let target_events = parse_chunks(target, &wire, &randomized_boundaries(wire.len()));
            assert_eq!(
                response_text(&target_events),
                "Hello 世界",
                "{source} -> {target}"
            );
            assert!(target_events.iter().any(|event| matches!(
                event,
                CanonicalStreamEvent::ResponseDone(done) if done.reason == FinishReason::Stop
            )));
        }
    }
}

#[test]
fn responses_renderer_emits_complete_content_lifecycle_and_final_output() {
    let source = parse_chunks(
        ProtocolKind::OpenAiChat,
        concat!(
            "data: {\"id\":\"chat-1\",\"model\":\"source\",\"choices\":[{\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chat-1\",\"model\":\"source\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        )
        .as_bytes(),
        &[],
    );
    let mut renderer = StreamRenderer::new(ProtocolKind::OpenAiResponses, "target");
    let mut wire = Vec::new();
    for event in &source {
        wire.extend_from_slice(&renderer.push(event).expect("render responses event"));
    }
    wire.extend_from_slice(&renderer.finish().expect("finish responses stream"));
    let wire = String::from_utf8(wire).expect("utf8 responses stream");

    for event in [
        "response.output_item.added",
        "response.content_part.added",
        "response.output_text.delta",
        "response.output_text.done",
        "response.content_part.done",
        "response.output_item.done",
        "response.completed",
    ] {
        assert!(
            wire.contains(&format!("event: {event}\n")),
            "missing {event}"
        );
    }
    assert!(wire.contains("\"output\":[{\"content\":[{\"annotations\":[],\"text\":\"hello\",\"type\":\"output_text\"}]"));
    assert_eq!(
        response_text(&parse_chunks(
            ProtocolKind::OpenAiResponses,
            wire.as_bytes(),
            &randomized_boundaries(wire.len()),
        )),
        "hello"
    );
}

#[test]
fn malformed_json_is_a_stable_stream_error() {
    let mut parser = StreamParser::new(ProtocolKind::OpenAiChat, "model");
    let error = parser
        .push(b"data: {not-json}\n\n")
        .expect_err("malformed stream event");
    assert_eq!(error.code(), "stream_event_json_invalid");
}

#[test]
fn unknown_stream_metadata_and_semantic_payloads_do_not_abort_cross_protocol_streams() {
    let cases = [
        (
            ProtocolKind::OpenAiChat,
            "data: {\"choices\":[{\"delta\":{\"future_content\":\"important\"}}]}\n\n",
        ),
        (
            ProtocolKind::OpenAiResponses,
            "event: response.future.delta\ndata: {\"type\":\"response.future.delta\",\"delta\":\"important\"}\n\n",
        ),
        (
            ProtocolKind::AnthropicMessages,
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"future_delta\",\"value\":\"important\"}}\n\n",
        ),
        (
            ProtocolKind::GeminiNative,
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"futurePart\":\"important\"}]}}]}\n\n",
        ),
    ];

    for (protocol, input) in cases {
        let mut parser = StreamParser::new(protocol, "model");
        parser
            .push(input.as_bytes())
            .unwrap_or_else(|error| panic!("{protocol} aborted on future stream data: {error}"));
    }

    let mut parser = StreamParser::new(ProtocolKind::OpenAiResponses, "model");
    parser
        .push(
            b"event: rate_limits.updated\ndata: {\"type\":\"rate_limits.updated\",\"remaining\":10}\n\n"
        )
        .expect("metadata event");
}

#[test]
fn tool_call_identity_and_partial_arguments_roundtrip_across_every_target() {
    let raw = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-tools\",\"model\":\"claude\",\"usage\":{\"input_tokens\":7,\"output_tokens\":0}}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"read_file\",\"input\":{}}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"README.md\\\"}\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":3}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n"
    );
    let source_events = parse_chunks(
        ProtocolKind::AnthropicMessages,
        raw.as_bytes(),
        &randomized_boundaries(raw.len()),
    );

    for target in PROTOCOLS {
        let mut renderer = StreamRenderer::new(target, "target-model");
        let mut wire = Vec::new();
        for event in &source_events {
            wire.extend_from_slice(&renderer.push(event).unwrap());
        }
        wire.extend_from_slice(&renderer.finish().unwrap());
        let target_events = parse_chunks(target, &wire, &every_boundary(wire.len()));
        let response = final_response(&target_events);
        let call = response
            .blocks
            .iter()
            .find_map(|block| match block {
                ContentBlock::ToolCall(call) => Some(call),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{target} tool call"));
        assert_eq!(call.id, "toolu_1", "{target}");
        assert_eq!(call.name, "read_file", "{target}");
        assert_eq!(
            call.arguments.as_ref().unwrap()["path"],
            "README.md",
            "{target}"
        );
        assert_eq!(response.finish.reason, FinishReason::ToolCalls, "{target}");
        assert_eq!(response.usage.input_tokens.value, Some(7), "{target}");
        assert_eq!(response.usage.output_tokens.value, Some(3), "{target}");
    }
}

#[test]
fn anthropic_reasoning_signature_stays_ordered_and_uses_validated_cross_protocol_carrier() {
    let raw = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-thinking\",\"model\":\"claude\"}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"Check first.\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig-1\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n"
    );
    let source = parse_chunks(ProtocolKind::AnthropicMessages, raw.as_bytes(), &[]);
    for target in PROTOCOLS {
        let mut renderer = StreamRenderer::new(target, "target-model");
        let mut wire = Vec::new();
        for event in &source {
            wire.extend_from_slice(&renderer.push(event).unwrap());
        }
        wire.extend_from_slice(&renderer.finish().unwrap());
        let wire_text = String::from_utf8_lossy(&wire);
        let response = final_response(&parse_chunks(target, &wire, &[]));
        let reasoning = response
            .blocks
            .iter()
            .find_map(|block| match block {
                ContentBlock::Reasoning(reasoning) => Some(reasoning),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{target} reasoning"));
        assert_eq!(reasoning.text.as_deref(), Some("Check first."), "{target}");
        if target == ProtocolKind::AnthropicMessages {
            assert!(reasoning.artifacts.iter().any(|artifact| {
                artifact.kind == ArtifactKind::AnthropicThinkingSignature
                    && artifact.payload == "sig-1"
            }));
        } else {
            assert!(wire_text.contains("const-api-continuation-v1:"), "{target}");
            assert!(!wire_text.contains("sig-1"), "{target}");
        }
    }
}

#[test]
fn gemini_empty_text_delta_does_not_create_an_anthropic_history_block() {
    let raw = concat!(
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"\"}]}}]}\n\n",
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"id\":\"call-1\",\"name\":\"read_file\",\"args\":{\"path\":\"README.md\"}}}]}}]}\n\n",
        "data: {\"candidates\":[{\"finishReason\":\"STOP\"}]}\n\n"
    );
    let events = parse_chunks(
        ProtocolKind::GeminiNative,
        raw.as_bytes(),
        &every_boundary(raw.len()),
    );
    let mut renderer = StreamRenderer::new(ProtocolKind::AnthropicMessages, "claude-opus-4-6");
    let mut wire = Vec::new();
    for event in &events {
        wire.extend_from_slice(&renderer.push(event).unwrap());
    }
    wire.extend_from_slice(&renderer.finish().unwrap());
    let blocks = decode_sse_frames(&wire)
        .unwrap()
        .into_iter()
        .filter(|(event, _)| event.as_deref() == Some("content_block_start"))
        .map(|(_, data)| {
            serde_json::from_str::<serde_json::Value>(&data).unwrap()["content_block"].clone()
        })
        .collect::<Vec<_>>();
    assert_eq!(blocks.len(), 1, "{blocks:?}");
    assert_eq!(blocks[0]["type"], "tool_use");
    assert_eq!(blocks[0]["id"], "call-1");
    assert!(events.iter().any(|event| matches!(event,
        CanonicalStreamEvent::ResponseDone(done) if done.reason == FinishReason::ToolCalls
    )));
}

#[test]
fn gemini_empty_text_handling_preserves_signatures_and_whitespace() {
    for part in [
        serde_json::json!({"text": " "}),
        serde_json::json!({"text": "", "thoughtSignature": "text-signature"}),
        serde_json::json!({"text": "", "thought": true, "thoughtSignature": "thinking-signature"}),
    ] {
        let raw = format!(
            "data: {}\n\ndata: {{\"candidates\":[{{\"finishReason\":\"STOP\"}}]}}\n\n",
            serde_json::json!({"candidates": [{"content": {"parts": [part.clone()]}}]})
        );
        let events = parse_chunks(ProtocolKind::GeminiNative, raw.as_bytes(), &[]);
        if let Some(signature) = part
            .get("thoughtSignature")
            .and_then(serde_json::Value::as_str)
        {
            assert!(events.iter().any(|event| matches!(event,
                CanonicalStreamEvent::ArtifactDelta { kind: ArtifactKind::GeminiThoughtSignature, data, .. }
                    if data == signature
            )), "{part}");
        } else {
            assert!(events.iter().any(|event| matches!(event,
                CanonicalStreamEvent::TextDelta { text, .. } if text == " "
            )));
        }
    }
}

#[test]
fn streamed_gemini_tool_signature_survives_responses_client_continuation() {
    let raw = concat!(
        "data: {\"modelVersion\":\"gemini-3.6-flash\",\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"thoughtSignature\":\"real-gemini-signature\",\"functionCall\":{\"id\":\"call-stream\",\"name\":\"glob\",\"args\":{\"pattern\":\"**/*.rs\"}}}]}}]}\n\n",
        "data: {\"modelVersion\":\"gemini-3.6-flash\",\"candidates\":[{\"finishReason\":\"STOP\"}]}\n\n"
    );
    let source_events = parse_chunks(ProtocolKind::GeminiNative, raw.as_bytes(), &[]);
    let mut renderer = StreamRenderer::new(ProtocolKind::OpenAiResponses, "client-model");
    let mut wire = Vec::new();
    for event in &source_events {
        wire.extend_from_slice(&renderer.push(event).unwrap());
    }
    wire.extend_from_slice(&renderer.finish().unwrap());

    let mut input = decode_sse_frames(&wire)
        .unwrap()
        .into_iter()
        .filter(|(event, _)| event.as_deref() == Some("response.output_item.done"))
        .map(|(_, data)| serde_json::from_str::<serde_json::Value>(&data).unwrap()["item"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        input.len(),
        2,
        "carrier item must precede the function call"
    );
    assert_eq!(input[0]["type"], "reasoning");
    assert_eq!(input[1]["type"], "function_call");
    assert!(
        input[0]["encrypted_content"]
            .as_str()
            .is_some_and(|value| value.starts_with("const-api-continuation-v1:"))
    );
    input.insert(0, json!({"role":"user","content":"Run glob."}));
    input.push(json!({
        "type":"function_call_output","call_id":"call-stream","output":"src/main.rs"
    }));

    let client_context = AdapterContext {
        dialect: ProviderDialect::Compatible {
            provider: "openai".to_string(),
        },
        issuer: IssuerIdentity {
            provider: "openai".to_string(),
            endpoint_fingerprint: "client-endpoint".to_string(),
            account_fingerprint: Some("client-account".to_string()),
            model_family: Some("client-model".to_string()),
        },
    };
    let gemini_context = AdapterContext {
        dialect: ProviderDialect::Compatible {
            provider: "google".to_string(),
        },
        issuer: IssuerIdentity {
            provider: "google".to_string(),
            endpoint_fingerprint: "google-endpoint".to_string(),
            account_fingerprint: None,
            model_family: Some("gemini-3.6-flash".to_string()),
        },
    };
    let converted = convert_non_stream(
        ProtocolKind::OpenAiResponses,
        ProtocolKind::GeminiNative,
        &json!({"model":"client-model","input":input}),
        &client_context,
        &gemini_context,
        &CapabilityProfile::default(),
        ConversionPolicy::Production,
    )
    .unwrap();
    let function_part = converted.body["contents"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|content| content["parts"].as_array().into_iter().flatten())
        .find(|part| part.get("functionCall").is_some())
        .unwrap();
    assert_eq!(function_part["thoughtSignature"], "real-gemini-signature");
    let function_response = converted.body["contents"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|content| content["parts"].as_array().into_iter().flatten())
        .find_map(|part| part.get("functionResponse"))
        .unwrap();
    assert_eq!(function_response["name"], "glob");
    assert!(
        !serde_json::to_string(&converted.body)
            .unwrap()
            .contains("const-api-continuation-v1:")
    );
}

#[test]
fn streamed_gemini_text_signature_survives_responses_client_continuation() {
    let raw = concat!(
        "data: {\"modelVersion\":\"gemini-3.6-flash\",\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"answer\",\"thoughtSignature\":\"real-text-signature\"}]}}]}\n\n",
        "data: {\"modelVersion\":\"gemini-3.6-flash\",\"candidates\":[{\"finishReason\":\"STOP\"}]}\n\n"
    );
    let source_events = parse_chunks(ProtocolKind::GeminiNative, raw.as_bytes(), &[]);
    assert!(source_events.iter().any(|event| matches!(
        event,
        CanonicalStreamEvent::ArtifactDelta {
            kind: ArtifactKind::GeminiThoughtSignature,
            data,
            ..
        } if data == "real-text-signature"
    )));

    let mut renderer = StreamRenderer::new(ProtocolKind::OpenAiResponses, "client-model");
    let mut wire = Vec::new();
    for event in &source_events {
        wire.extend_from_slice(&renderer.push(event).unwrap());
    }
    wire.extend_from_slice(&renderer.finish().unwrap());
    let mut input = decode_sse_frames(&wire)
        .unwrap()
        .into_iter()
        .filter(|(event, _)| event.as_deref() == Some("response.output_item.done"))
        .map(|(_, data)| serde_json::from_str::<serde_json::Value>(&data).unwrap()["item"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        input.len(),
        2,
        "text item and its carrier must both survive"
    );
    assert_eq!(input[0]["type"], "message");
    assert_eq!(input[1]["type"], "reasoning");
    input.push(json!({"role":"user","content":"continue"}));

    let client_context = AdapterContext {
        dialect: ProviderDialect::Compatible {
            provider: "openai".to_string(),
        },
        issuer: IssuerIdentity {
            provider: "openai".to_string(),
            endpoint_fingerprint: "client-endpoint".to_string(),
            account_fingerprint: Some("client-account".to_string()),
            model_family: Some("client-model".to_string()),
        },
    };
    let gemini_context = AdapterContext {
        dialect: ProviderDialect::Compatible {
            provider: "google".to_string(),
        },
        issuer: IssuerIdentity {
            provider: "google".to_string(),
            endpoint_fingerprint: "google-endpoint".to_string(),
            account_fingerprint: None,
            model_family: Some("gemini-3.6-flash".to_string()),
        },
    };
    let converted = convert_non_stream(
        ProtocolKind::OpenAiResponses,
        ProtocolKind::GeminiNative,
        &json!({"model":"client-model","input":input}),
        &client_context,
        &gemini_context,
        &CapabilityProfile::default(),
        ConversionPolicy::Production,
    )
    .unwrap();
    let text_part = converted.body["contents"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|content| content["parts"].as_array().into_iter().flatten())
        .find(|part| part["text"] == "answer")
        .unwrap();
    assert_eq!(text_part["thoughtSignature"], "real-text-signature");

    let mut accumulator = CanonicalAccumulator::new();
    for event in &source_events {
        accumulator.push(event).unwrap();
    }
    let buffered = accumulator.finish().unwrap();
    assert!(matches!(buffered.blocks[0], ContentBlock::Text(_)));
    let ContentBlock::ProviderArtifact(artifact) = &buffered.blocks[1] else {
        panic!("text signature must remain adjacent after SSE buffering");
    };
    assert_eq!(artifact.payload, "real-text-signature");
    assert!(matches!(
        &artifact.affinity,
        ArtifactAffinity::ProtocolModel { protocol, model }
            if *protocol == ProtocolKind::GeminiNative && model == "gemini-3.6-flash"
    ));
}

#[test]
fn provider_stream_error_never_turns_into_a_synthetic_success() {
    let raw = concat!(
        "event: error\n",
        "data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"busy\"}}\n\n"
    );
    let source = parse_chunks(ProtocolKind::AnthropicMessages, raw.as_bytes(), &[]);
    assert!(
        source
            .iter()
            .any(|event| matches!(event, CanonicalStreamEvent::Error(_)))
    );
    assert!(!source.iter().any(|event| matches!(
        event,
        CanonicalStreamEvent::ResponseDone(done) if done.reason == FinishReason::Stop
    )));

    for target in PROTOCOLS {
        let mut renderer = StreamRenderer::new(target, "target-model");
        let mut wire = Vec::new();
        for event in &source {
            wire.extend_from_slice(&renderer.push(event).unwrap());
        }
        wire.extend_from_slice(&renderer.finish().unwrap());
        let target_events = parse_chunks(target, &wire, &[]);
        assert!(
            target_events
                .iter()
                .any(|event| matches!(event, CanonicalStreamEvent::Error(_))),
            "{target}"
        );
        assert!(
            !target_events.iter().any(|event| matches!(
                event,
                CanonicalStreamEvent::ResponseDone(done) if done.reason == FinishReason::Stop
            )),
            "{target}"
        );
    }
}

#[test]
fn responses_failed_preserves_nested_provider_errors_and_metadata() {
    let error = serde_json::json!({
        "code": "rate_limit_exceeded",
        "message": "Temporary limit. Please try again in 1s.",
        "retry_after_seconds": 1,
        "future_details": {"resumable": true}
    });
    for event in [
        serde_json::json!({"type": "response.failed", "response": {"error": error}}),
        serde_json::json!({"type": "response.failed", "response": {"status_details": {"error": error}}}),
        serde_json::json!({"type": "error", "error": error}),
    ] {
        let wire = format!("data: {event}\n\n");
        let source = parse_chunks(
            ProtocolKind::OpenAiResponses,
            wire.as_bytes(),
            &every_boundary(wire.len()),
        );
        let response = final_response(&source);
        let actual = response.error.as_ref().expect("terminal error");
        assert_eq!(actual.code, "rate_limit_exceeded");
        assert_eq!(actual.provider_code.as_deref(), Some("rate_limit_exceeded"));
        assert_eq!(actual.message, error["message"]);
        assert_eq!(actual.details.as_ref(), Some(&error));
        // Parsing a stream error does not authorize replaying a started request.
        assert!(!actual.retryable);
        assert!(!source.iter().any(|event| matches!(
            event,
            CanonicalStreamEvent::ResponseDone(done) if done.reason == FinishReason::Stop
        )));
    }
}

#[test]
fn partial_sse_event_and_canonical_accumulator_are_bounded() {
    let mut parser = StreamParser::new(ProtocolKind::OpenAiChat, "model");
    let oversized = vec![b'x'; MAX_STREAM_BYTES + 1];
    assert_eq!(
        parser.push(&oversized).unwrap_err().code(),
        "stream_event_too_large"
    );

    let mut accumulator = CanonicalAccumulator::new();
    let event = CanonicalStreamEvent::TextDelta {
        index: 0,
        text: "x".repeat(MAX_STREAM_BYTES + 1),
    };
    assert_eq!(
        accumulator.push(&event).unwrap_err().code(),
        "stream_buffer_exceeded"
    );
}

#[test]
fn unrecognized_semantic_stream_payloads_are_reportable_without_stopping_output() {
    let mut parser = StreamParser::new(ProtocolKind::OpenAiChat, "model");
    let raw = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"hello\",\"future_semantic\":{\"x\":1}}}]}\n\n",
        "data: [DONE]\n\n"
    );
    let mut events = parser.push(raw.as_bytes()).unwrap();
    events.extend(parser.finish().unwrap());
    let notices = parser.take_notices();

    assert!(events.iter().any(
        |event| matches!(event, CanonicalStreamEvent::TextDelta { text, .. } if text == "hello")
    ));
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].code, "stream_payload_approximated");
    assert!(notices[0].summary.contains("future_semantic"));
}

#[test]
fn complete_response_to_sse_omits_only_unrepresentable_blocks_and_reports_them() {
    let response = CanonicalResponseV2 {
        id: Some("response-1".to_string()),
        model: Some("model".to_string()),
        blocks: vec![
            ContentBlock::Text(TextBlock::new("still available")),
            ContentBlock::Image(MediaBlock {
                source: MediaSource::RemoteUrl {
                    url: "https://example.invalid/image.png".to_string(),
                },
                detail: None,
                metadata: BlockMetadata::default(),
            }),
        ],
        status: ResponseStatus::Completed,
        finish: FinishDetail::stop(),
        usage: Usage::default(),
        error: None,
        extensions: Vec::new(),
    };

    let (events, notices) =
        events_from_response_with_notices(&response).expect("best-effort SSE adaptation");

    assert!(events.iter().any(
        |event| matches!(event, CanonicalStreamEvent::TextDelta { text, .. } if text == "still available")
    ));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, CanonicalStreamEvent::ResponseDone(_)))
    );
    assert!(notices.iter().any(|notice| {
        notice.code == "response_block_omitted_in_stream_adaptation" && notice.path == "$.blocks[1]"
    }));
}

#[test]
fn target_stream_limitations_are_reported_without_suppressing_events() {
    let events = vec![CanonicalStreamEvent::ReasoningSummaryDelta {
        index: 0,
        part: 0,
        text: "summary".to_string(),
    }];

    let notices = stream_rendering_notices(&events, ProtocolKind::OpenAiChat);

    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].code, "stream_reasoning_summary_approximated");
    assert_eq!(notices[0].path, "$.stream.blocks[0].summary[0]");
}

#[test]
fn complete_json_error_requested_as_sse_remains_a_terminal_error() {
    let response = CanonicalResponseV2 {
        id: Some("response-error".to_string()),
        model: Some("model".to_string()),
        blocks: Vec::new(),
        status: ResponseStatus::Failed,
        finish: FinishDetail {
            reason: FinishReason::Error,
            original_reason: Some("service_unavailable".to_string()),
            incomplete_details: None,
        },
        usage: Usage::default(),
        error: Some(CanonicalError {
            code: "service_unavailable".to_string(),
            message: "temporarily unavailable".to_string(),
            retryable: true,
            provider_code: Some("upstream_503".to_string()),
            details: None,
        }),
        extensions: Vec::new(),
    };

    let (events, _) = events_from_response_with_notices(&response).expect("error event adaptation");

    assert!(events.iter().any(|event| matches!(
        event,
        CanonicalStreamEvent::Error(error) if error.code == "service_unavailable"
    )));
    assert!(!events.iter().any(|event| matches!(
        event,
        CanonicalStreamEvent::ResponseDone(done) if done.reason == FinishReason::Stop
    )));
}

#[test]
fn supplier_utf8_bridge_keeps_split_code_points_without_lossy_replacement() {
    let input = "data: 世界\n\n".as_bytes();
    for boundary in 0..=input.len() {
        let mut decoder = Utf8ChunkDecoder::new();
        let mut output = decoder.push(&input[..boundary]).unwrap();
        output.push_str(&decoder.push(&input[boundary..]).unwrap());
        output.push_str(&decoder.finish().unwrap());
        assert_eq!(output, "data: 世界\n\n", "boundary {boundary}");
        assert!(!output.contains('\u{fffd}'));
    }
}

#[test]
fn stream_converter_failure_is_structured_and_never_emits_success_afterward() {
    for target in PROTOCOLS {
        let mut converter = stream_converter(ProtocolKind::OpenAiChat, target, "model");
        let wire = converter.fail("stream_inactivity_timeout", "upstream stalled");
        let events = parse_chunks(target, &wire, &[]);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, CanonicalStreamEvent::Error(error)
            if error.code == "stream_inactivity_timeout")),
            "{target}"
        );
        assert!(
            events.iter().any(
                |event| matches!(event, CanonicalStreamEvent::ResponseDone(done)
            if done.reason == FinishReason::Error)
            ),
            "{target}"
        );
        assert!(
            !events.iter().any(
                |event| matches!(event, CanonicalStreamEvent::ResponseDone(done)
            if done.reason == FinishReason::Stop)
            ),
            "{target}"
        );
        assert!(converter.finish().is_empty(), "{target}");
        assert_eq!(
            converter.failure().map(|error| error.code()),
            Some("stream_inactivity_timeout"),
            "{target}"
        );
    }
}

#[test]
fn malformed_stream_conversion_is_terminal_and_observable() {
    let mut converter = stream_converter(
        ProtocolKind::OpenAiChat,
        ProtocolKind::OpenAiResponses,
        "model",
    );
    let failure_wire = converter.push(b"data: {not-json}\n\n");
    assert!(!failure_wire.is_empty());
    assert!(converter.failure().is_some());
    assert!(converter.push(b"data: [DONE]\n\n").is_empty());
    assert!(converter.finish().is_empty());
}
use serde_json::json;

use crate::protocol::adapters::{AdapterContext, ProviderDialect};
use crate::protocol::capability::CapabilityProfile;
use crate::protocol::conversion::{ConversionPolicy, IssuerIdentity};
