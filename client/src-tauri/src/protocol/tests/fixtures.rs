use serde::Deserialize;
use serde_json::Value;

use crate::protocol::kind::ProtocolKind;
use crate::protocol::{adapters::AdapterContext, ir::ContentBlock};

#[derive(Debug, Deserialize)]
pub(super) struct FixtureCase {
    pub(super) protocol: ProtocolKind,
    pub(super) path: String,
    pub(super) request: Value,
    pub(super) response: Value,
    pub(super) expected: SemanticExpectation,
}

#[derive(Debug, Deserialize)]
pub(super) struct SemanticExpectation {
    request_message_count: usize,
    system_text: String,
    user_text: String,
    tool_name: String,
    tool_call_id: String,
    tool_argument_path: String,
    tool_result_text: String,
    response_text: String,
    finish_reason: String,
    input_tokens: i64,
    output_tokens: i64,
    total_tokens: i64,
    #[serde(default)]
    unsupported: Vec<String>,
}

pub(super) fn load_fixture(directory: &str, name: &str) -> FixtureCase {
    let raw = match (directory, name) {
        ("openai-chat", "basic-tools") => {
            include_str!("../../../tests/fixtures/protocol-v2/openai-chat/basic-tools.json")
        }
        ("openai-responses", "basic-tools") => {
            include_str!("../../../tests/fixtures/protocol-v2/openai-responses/basic-tools.json")
        }
        ("anthropic-messages", "basic-tools") => {
            include_str!("../../../tests/fixtures/protocol-v2/anthropic-messages/basic-tools.json")
        }
        ("gemini-native", "basic-tools") => {
            include_str!("../../../tests/fixtures/protocol-v2/gemini-native/basic-tools.json")
        }
        _ => panic!("unknown protocol fixture: {directory}/{name}"),
    };
    serde_json::from_str(raw).expect("valid protocol fixture")
}

#[test]
fn loads_basic_tool_fixtures_for_every_core_protocol() {
    for (protocol, directory) in [
        ("openai_chat", "openai-chat"),
        ("openai_responses", "openai-responses"),
        ("anthropic_messages", "anthropic-messages"),
        ("gemini_native", "gemini-native"),
    ] {
        let fixture = load_fixture(directory, "basic-tools");
        assert_eq!(fixture.protocol.as_str(), protocol);
        assert!(!fixture.path.is_empty());
        assert!(fixture.request.is_object());
        assert!(fixture.response.is_object());
        assert!(!fixture.expected.tool_name.is_empty());
    }
}

#[test]
fn current_converters_preserve_basic_text_tool_and_usage_semantics() {
    for directory in [
        "openai-chat",
        "openai-responses",
        "anthropic-messages",
        "gemini-native",
    ] {
        let fixture = load_fixture(directory, "basic-tools");
        let context = AdapterContext::legacy_bridge("fixture-model");
        let canonical =
            crate::protocol::decode_request(fixture.protocol, &fixture.request, &context)
                .expect("v2 request converter");

        assert_eq!(
            canonical.instructions.len() + canonical.turns.len(),
            fixture.expected.request_message_count,
            "{} message count",
            fixture.protocol
        );
        assert_eq!(
            canonical
                .instructions
                .iter()
                .find(|instruction| {
                    instruction.role == crate::protocol::ir::InstructionRole::System
                })
                .and_then(|instruction| first_text(&instruction.blocks))
                .unwrap_or_default(),
            fixture.expected.system_text,
            "{} system text",
            fixture.protocol
        );
        assert_eq!(
            canonical
                .turns
                .iter()
                .find(|turn| turn.role == crate::protocol::ir::TurnRole::User)
                .and_then(|turn| first_text(&turn.blocks))
                .unwrap_or_default(),
            fixture.expected.user_text,
            "{} user text",
            fixture.protocol
        );

        let request_call = canonical
            .turns
            .iter()
            .flat_map(|turn| turn.blocks.iter())
            .find_map(|block| match block {
                ContentBlock::ToolCall(call) => Some(call),
                _ => None,
            })
            .expect("request tool call");
        assert_eq!(request_call.name, fixture.expected.tool_name);
        assert_eq!(request_call.id, fixture.expected.tool_call_id);
        assert_eq!(
            request_call.arguments.as_ref().expect("tool arguments")["path"],
            fixture.expected.tool_argument_path
        );
        assert_eq!(
            canonical
                .turns
                .iter()
                .flat_map(|turn| turn.blocks.iter())
                .find_map(|block| match block {
                    ContentBlock::ToolResult(result) => first_text(&result.content),
                    _ => None,
                })
                .unwrap_or_default(),
            fixture.expected.tool_result_text,
            "{} tool result",
            fixture.protocol
        );

        let response =
            crate::protocol::decode_response(fixture.protocol, &fixture.response, &context)
                .expect("v2 response converter");
        assert_eq!(
            first_text(&response.blocks),
            Some(fixture.expected.response_text.as_str())
        );
        assert_eq!(
            finish_reason_name(response.finish.reason),
            fixture.expected.finish_reason
        );
        assert_eq!(
            response.usage.input_tokens.value,
            Some(fixture.expected.input_tokens as u64)
        );
        assert_eq!(
            response.usage.output_tokens.value,
            Some(fixture.expected.output_tokens as u64)
        );
        assert_eq!(
            response.usage.total_tokens.value,
            Some(fixture.expected.total_tokens as u64)
        );
        assert_eq!(
            response.blocks.iter().find_map(|block| match block {
                ContentBlock::ToolCall(call) => Some(call.name.as_str()),
                _ => None,
            }),
            Some(fixture.expected.tool_name.as_str())
        );

        assert!(
            !fixture.expected.unsupported.is_empty(),
            "fixtures must record current advanced-semantic gaps"
        );
    }
}

fn first_text(blocks: &[ContentBlock]) -> Option<&str> {
    blocks.iter().find_map(|block| match block {
        ContentBlock::Text(text) => Some(text.text.as_str()),
        _ => None,
    })
}

fn finish_reason_name(reason: crate::protocol::ir::FinishReason) -> &'static str {
    match reason {
        crate::protocol::ir::FinishReason::Stop => "stop",
        crate::protocol::ir::FinishReason::Length => "length",
        crate::protocol::ir::FinishReason::ToolCalls => "tool_calls",
        crate::protocol::ir::FinishReason::PauseTurn => "pause_turn",
        crate::protocol::ir::FinishReason::ContentFilter => "content_filter",
        crate::protocol::ir::FinishReason::Refusal => "refusal",
        crate::protocol::ir::FinishReason::Error => "error",
        crate::protocol::ir::FinishReason::Cancelled => "cancelled",
        crate::protocol::ir::FinishReason::Unknown => "unknown",
    }
}
