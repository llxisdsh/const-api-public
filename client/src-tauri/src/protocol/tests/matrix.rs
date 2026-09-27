use serde_json::{Value, json};

use crate::protocol::adapters::{AdapterContext, ProviderDialect};
use crate::protocol::capability::{CapabilityProfile, Feature, protocol_wire_supports};
use crate::protocol::conversion::{ConversionLevel, ConversionPolicy, IssuerIdentity};
use crate::protocol::ir::{
    ArtifactKind, BlockMetadata, CachePolicy, CanonicalRequestV2, ContentBlock, FileBlock,
    FinishReason, FunctionTool, ItemStatus, MediaBlock, MediaSource, OutputVerbosity,
    ReasoningBlock, ReasoningEffort, ReasoningMode, RefusalBlock, ResponseFormat, ResponseStatus,
    TextBlock, ToolCall, ToolDefinition, ToolResult, Turn, TurnRole,
};
use crate::protocol::kind::ProtocolKind;
use crate::protocol::{
    ProtocolFacadeError, convert_non_stream, convert_response_non_stream, decode_request,
    decode_response, encode_request, encode_response, plan_request,
};

use super::fixtures::load_fixture;

const PROTOCOLS: [ProtocolKind; 4] = [
    ProtocolKind::OpenAiResponses,
    ProtocolKind::OpenAiChat,
    ProtocolKind::AnthropicMessages,
    ProtocolKind::GeminiNative,
];

fn additional_tools_body(tools: Value) -> Value {
    json!({
        "model":"source-model", "stream":true,
        "input":[
            {"type":"additional_tools", "id":"at_stable", "role":"developer", "tools":tools},
            {"role":"user", "content":"Use a tool."}
        ]
    })
}

fn responses_function(name: &str) -> Value {
    json!({"type":"function", "name":name, "parameters":{
        "type":"object", "properties":{"query":{"type":"string"}}, "required":["query"]
    }})
}

#[test]
fn responses_additional_tools_reach_all_conversion_targets_without_empty_messages() {
    let mut body = additional_tools_body(json!([responses_function("search")]));
    body["tools"] = json!([responses_function("first")]);
    body["input"].as_array_mut().unwrap().insert(1, json!({
        "type":"additional_tools", "role":"developer", "tools":[responses_function("search"), responses_function("last")]
    }));
    for target in PROTOCOLS {
        let converted = converted_request_body(ProtocolKind::OpenAiResponses, target, &body);
        let decoded = decode_request(
            target,
            &converted,
            &target_context(ProtocolKind::OpenAiResponses, target),
        )
        .unwrap();
        let names = decoded
            .tools
            .iter()
            .map(|tool| match tool {
                ToolDefinition::Function(tool) => tool.name.as_str(),
                _ => panic!("function expected"),
            })
            .collect::<Vec<_>>();
        assert_eq!(names, ["first", "search", "last"], "{target}: {converted}");
        if target != ProtocolKind::OpenAiResponses {
            assert_eq!(decoded.turns.len(), 1, "{target}: {converted}");
            assert_eq!(decoded.turns[0].role, TurnRole::User);
        }
        // Gemini selects streaming in the URL, not in the JSON request body.
        if target != ProtocolKind::GeminiNative {
            assert!(decoded.stream);
        }
    }
}

#[test]
fn responses_additional_tools_native_position_and_top_level_are_preserved() {
    let mut body = additional_tools_body(json!([responses_function("search")]));
    body["tools"] = json!([responses_function("first")]);
    body["input"].as_array_mut().unwrap().insert(
        1,
        json!({"role":"developer","content":"Stable instructions"}),
    );
    let converted = converted_request_body(
        ProtocolKind::OpenAiResponses,
        ProtocolKind::OpenAiResponses,
        &body,
    );
    assert_eq!(converted, body);

    // Exercise the IR encoder as well as the normal raw passthrough path.
    let context = context(
        ProtocolKind::OpenAiResponses,
        "source-model",
        "source-account",
    );
    let request = decode_request(ProtocolKind::OpenAiResponses, &body, &context).unwrap();
    let request: CanonicalRequestV2 =
        serde_json::from_value(serde_json::to_value(request).unwrap()).unwrap();
    let plan = plan_request(
        &request,
        ProtocolKind::OpenAiResponses,
        ProtocolKind::OpenAiResponses,
        &context,
        &context,
        &CapabilityProfile::default(),
        ConversionPolicy::Production,
    )
    .unwrap();
    let encoded = encode_request(ProtocolKind::OpenAiResponses, &request, &plan, &context).unwrap();
    assert_eq!(encoded["input"][0], body["input"][0]);
    assert_eq!(encoded["tools"], body["tools"]);

    body.as_object_mut().unwrap().remove("tools");
    let request = decode_request(ProtocolKind::OpenAiResponses, &body, &context).unwrap();
    let request: CanonicalRequestV2 =
        serde_json::from_value(serde_json::to_value(request).unwrap()).unwrap();
    let plan = plan_request(
        &request,
        ProtocolKind::OpenAiResponses,
        ProtocolKind::OpenAiResponses,
        &context,
        &context,
        &CapabilityProfile::default(),
        ConversionPolicy::Production,
    )
    .unwrap();
    let encoded = encode_request(ProtocolKind::OpenAiResponses, &request, &plan, &context).unwrap();
    assert!(encoded.get("tools").is_none());
    assert_eq!(encoded["input"][0], body["input"][0]);
}

#[test]
fn responses_additional_tools_invalid_and_conflicting_definitions_are_explicit_errors() {
    let base = additional_tools_body(json!([responses_function("search")]));
    for (body, expected) in [
        (
            {
                let mut body = base.clone();
                body["input"][0]["role"] = json!("user");
                body
            },
            "additional_tools_invalid",
        ),
        (
            {
                let mut body = base.clone();
                body["input"][0]["tools"] = json!({});
                body
            },
            "additional_tools_invalid",
        ),
        (
            {
                let mut body = base.clone();
                body["tools"] = json!([{"type":"custom","name":"search"}]);
                body
            },
            "tool_definition_conflict",
        ),
    ] {
        let error = convert_non_stream(
            ProtocolKind::OpenAiResponses,
            ProtocolKind::OpenAiChat,
            &body,
            &context(
                ProtocolKind::OpenAiResponses,
                "source-model",
                "source-account",
            ),
            &target_context(ProtocolKind::OpenAiResponses, ProtocolKind::OpenAiChat),
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap_err();
        assert_eq!(error.code(), expected);
    }
}

#[test]
fn responses_namespace_custom_tools_keep_type_grammar_and_bridge_targets() {
    let grammar = json!({"type":"grammar","syntax":"lark","definition":"start: /.+/"});
    let body = additional_tools_body(json!([{
        "type":"namespace", "name":"functions", "tools":[
            responses_function("search"), {"type":"custom","name":"apply_patch","format":grammar}
        ]
    }]));
    let context = context(
        ProtocolKind::OpenAiResponses,
        "source-model",
        "source-account",
    );
    let mut request = decode_request(ProtocolKind::OpenAiResponses, &body, &context).unwrap();
    let ToolDefinition::Namespace(namespace) = &request.tools[0] else {
        panic!("namespace");
    };
    let crate::protocol::ir::NamespaceToolDefinition::Custom(custom) = &namespace.tools[1] else {
        panic!("custom tool");
    };
    assert_eq!(custom.grammar, grammar);
    assert!(
        crate::protocol::capability::extract_required_features(&request)
            .iter()
            .any(|usage| usage.feature == Feature::CustomTools)
    );

    // Force top-level IR encoding to check the actual nested wire types too.
    request.metadata.responses_tool_placement = None;
    request.turns.remove(0);
    let plan = plan_request(
        &request,
        ProtocolKind::OpenAiResponses,
        ProtocolKind::OpenAiResponses,
        &context,
        &context,
        &CapabilityProfile::default(),
        ConversionPolicy::Production,
    )
    .unwrap();
    let encoded = encode_request(ProtocolKind::OpenAiResponses, &request, &plan, &context).unwrap();
    assert_eq!(encoded["tools"][0]["tools"][0]["type"], "function");
    assert_eq!(encoded["tools"][0]["tools"][1]["type"], "custom");
    assert_eq!(encoded["tools"][0]["tools"][1]["format"], grammar);
    for target in PROTOCOLS
        .into_iter()
        .filter(|target| *target != ProtocolKind::OpenAiResponses)
    {
        let converted = convert_non_stream(
            ProtocolKind::OpenAiResponses,
            target,
            &body,
            &context,
            &target_context(ProtocolKind::OpenAiResponses, target),
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap();
        assert!(
            converted
                .body
                .to_string()
                .contains("ns9_functions_apply_patch")
        );
        assert!(converted.body.to_string().contains("start: /.+/"));
        assert!(!converted.plan.tool_mapping.is_empty());
    }
}

#[test]
fn responses_additional_tools_merge_namespace_children_in_stable_order() {
    let mut body = additional_tools_body(
        json!([{"type":"namespace","name":"functions","tools":[responses_function("b"),responses_function("a")]}]),
    );
    body["tools"] =
        json!([{"type":"namespace","name":"functions","tools":[responses_function("a")]}]);
    let context = context(
        ProtocolKind::OpenAiResponses,
        "source-model",
        "source-account",
    );
    let request = decode_request(ProtocolKind::OpenAiResponses, &body, &context).unwrap();
    let ToolDefinition::Namespace(namespace) = &request.tools[0] else {
        panic!("namespace");
    };
    assert_eq!(
        namespace
            .tools
            .iter()
            .map(|tool| tool.name())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
    let restored: CanonicalRequestV2 =
        serde_json::from_value(serde_json::to_value(&request).unwrap()).unwrap();
    assert_eq!(restored, request);
}

#[test]
fn responses_custom_history_is_not_misreported_as_invalid_json() {
    let body = json!({"model":"source-model","input":[
        {"type":"custom_tool_call","call_id":"call_patch","name":"apply_patch","input":"*** Begin Patch\n*** End Patch"},
        {"type":"custom_tool_call_output","call_id":"call_patch","output":"Done"}
    ]});
    let converted = convert_non_stream(
        ProtocolKind::OpenAiResponses,
        ProtocolKind::OpenAiChat,
        &body,
        &context(
            ProtocolKind::OpenAiResponses,
            "source-model",
            "source-account",
        ),
        &target_context(ProtocolKind::OpenAiResponses, ProtocolKind::OpenAiChat),
        &CapabilityProfile::default(),
        ConversionPolicy::Production,
    )
    .unwrap();
    let arguments = converted.body["messages"][0]["tool_calls"][0]["function"]["arguments"]
        .as_str()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(arguments).unwrap()["input"],
        "*** Begin Patch\n*** End Patch"
    );
}

fn fixture(protocol: ProtocolKind) -> super::fixtures::FixtureCase {
    load_fixture(
        match protocol {
            ProtocolKind::OpenAiResponses => "openai-responses",
            ProtocolKind::OpenAiChat => "openai-chat",
            ProtocolKind::AnthropicMessages => "anthropic-messages",
            ProtocolKind::GeminiNative => "gemini-native",
        },
        "basic-tools",
    )
}

fn context(protocol: ProtocolKind, model: &str, account: &str) -> AdapterContext {
    let provider = match protocol {
        ProtocolKind::OpenAiResponses | ProtocolKind::OpenAiChat => "openai",
        ProtocolKind::AnthropicMessages => "anthropic",
        ProtocolKind::GeminiNative => "google",
    };
    AdapterContext {
        dialect: ProviderDialect::Compatible {
            provider: provider.to_string(),
        },
        issuer: IssuerIdentity {
            provider: provider.to_string(),
            endpoint_fingerprint: format!("{provider}-endpoint"),
            account_fingerprint: Some(account.to_string()),
            model_family: Some(model.to_string()),
        },
    }
}

fn visible_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            ContentBlock::Refusal(refusal) => Some(refusal.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn text_request() -> CanonicalRequestV2 {
    let mut request = CanonicalRequestV2::new("source-model");
    request.turns.push(Turn {
        id: None,
        role: TurnRole::User,
        blocks: vec![ContentBlock::Text(TextBlock::new("Describe the input."))],
        status: None,
    });
    request
}

fn source_body(protocol: ProtocolKind, request: &CanonicalRequestV2) -> Value {
    let source_context = context(protocol, "source-model", "source-account");
    let plan = plan_request(
        request,
        protocol,
        protocol,
        &source_context,
        &source_context,
        &CapabilityProfile::default(),
        ConversionPolicy::Production,
    )
    .expect("native source plan");
    encode_request(protocol, request, &plan, &source_context).expect("source request encoding")
}

fn target_context(source: ProtocolKind, target: ProtocolKind) -> AdapterContext {
    if source == target {
        context(target, "source-model", "source-account")
    } else {
        context(target, "target-model", "target-account")
    }
}

fn converted_request_body(source: ProtocolKind, target: ProtocolKind, body: &Value) -> Value {
    convert_non_stream(
        source,
        target,
        body,
        &context(source, "source-model", "source-account"),
        &target_context(source, target),
        &CapabilityProfile::default(),
        ConversionPolicy::Production,
    )
    .unwrap_or_else(|error| panic!("{source} -> {target} failed: {error}"))
    .body
}

fn assert_unsupported_feature(
    error: &ProtocolFacadeError,
    feature: Feature,
    source: ProtocolKind,
    target: ProtocolKind,
) {
    assert_eq!(error.code(), "conversion_blocked", "{source} -> {target}");
    assert!(
        error
            .report()
            .is_some_and(|report| report.issues.iter().any(|issue| {
                issue.code == "unsupported_required_feature" && issue.feature == Some(feature)
            })),
        "{source} -> {target} should report unsupported {feature:?}: {error}"
    );
}

#[test]
fn reasoning_usage_crosses_all_response_pairs_without_changing_native_bodies() {
    for source in PROTOCOLS {
        for reasoning in [None, Some(0), Some(94)] {
            let mut body = fixture(source).response;
            let separate_output = 103 - reasoning.unwrap_or(0);
            let (field, usage) = match source {
                ProtocolKind::OpenAiChat => (
                    "usage",
                    json!({
                        "prompt_tokens":32, "completion_tokens":separate_output, "total_tokens":135,
                        "completion_tokens_details":{"reasoning_tokens":reasoning}
                    }),
                ),
                ProtocolKind::OpenAiResponses => (
                    "usage",
                    json!({
                        "input_tokens":32, "output_tokens":103, "total_tokens":135,
                        "output_tokens_details":{"reasoning_tokens":reasoning}
                    }),
                ),
                ProtocolKind::AnthropicMessages => (
                    "usage",
                    json!({
                        "input_tokens":32, "output_tokens":103,
                        "output_tokens_details":{"thinking_tokens":reasoning}
                    }),
                ),
                ProtocolKind::GeminiNative => (
                    "usageMetadata",
                    json!({
                        "promptTokenCount":32, "candidatesTokenCount":separate_output,
                        "totalTokenCount":135, "thoughtsTokenCount":reasoning
                    }),
                ),
            };
            body[field] = usage;
            for target in PROTOCOLS {
                let target_context = target_context(source, target);
                let converted = convert_response_non_stream(
                    source,
                    target,
                    &body,
                    &context(source, "source-model", "source-account"),
                    &target_context,
                )
                .unwrap();
                if source == target {
                    assert_eq!(converted.body, body, "native body must remain unchanged");
                }
                let decoded = decode_response(target, &converted.body, &target_context).unwrap();
                assert_eq!(
                    decoded.usage.output_tokens.value,
                    Some(103),
                    "{source} -> {target}"
                );
                assert_eq!(
                    decoded.usage.reasoning_tokens.value,
                    reasoning.map(|value| value as u64),
                    "{source} -> {target}"
                );
            }
        }
    }
}

#[test]
fn request_and_response_text_tools_status_and_usage_cross_all_sixteen_pairs() {
    for source in PROTOCOLS {
        for target in PROTOCOLS {
            let source_fixture = fixture(source);
            let source_context = context(source, "source-model", "source-account");
            let target_model = if source == target {
                "source-model"
            } else {
                "target-model"
            };
            let target_account = if source == target {
                "source-account"
            } else {
                "target-account"
            };
            let target_context = context(target, target_model, target_account);
            let converted = convert_non_stream(
                source,
                target,
                &source_fixture.request,
                &source_context,
                &target_context,
                &CapabilityProfile::default(),
                ConversionPolicy::Production,
            )
            .unwrap_or_else(|error| panic!("{source} -> {target} request failed: {error}"));
            let decoded = decode_request(target, &converted.body, &target_context)
                .unwrap_or_else(|error| panic!("{source} -> {target} decode failed: {error}"));

            assert!(decoded.instructions.iter().any(|instruction| {
                visible_text(&instruction.blocks) == "Use tools carefully."
            }));
            assert!(decoded.turns.iter().any(|turn| {
                turn.role == TurnRole::User && visible_text(&turn.blocks) == "Read README.md"
            }));
            let call = decoded
                .turns
                .iter()
                .flat_map(|turn| turn.blocks.iter())
                .find_map(|block| match block {
                    ContentBlock::ToolCall(call) => Some(call),
                    _ => None,
                })
                .expect("tool call");
            assert_eq!(call.name, "read_file");
            assert_eq!(call.arguments.as_ref().unwrap()["path"], "README.md");
            assert!(decoded.turns.iter().flat_map(|turn| turn.blocks.iter()).any(
                |block| matches!(block, ContentBlock::ToolResult(result) if result.call_id == call.id)
            ));

            let mut response = decode_response(source, &source_fixture.response, &source_context)
                .unwrap_or_else(|error| panic!("{source} response decode failed: {error}"));
            response.model = Some(target_model.to_string());
            let encoded = encode_response(target, &response, &converted.plan, &target_context)
                .unwrap_or_else(|error| panic!("{source} -> {target} response failed: {error}"));
            let decoded_response = decode_response(target, &encoded, &target_context)
                .unwrap_or_else(|error| panic!("{target} response decode failed: {error}"));
            assert_eq!(visible_text(&decoded_response.blocks), "Reading");
            assert!(decoded_response.blocks.iter().any(
                |block| matches!(block, ContentBlock::ToolCall(call) if call.name == "read_file")
            ));
            assert_eq!(
                decoded_response.finish.reason, response.finish.reason,
                "{source} -> {target} finish reason"
            );
            assert_eq!(
                decoded_response.usage.input_tokens,
                response.usage.input_tokens
            );
            assert_eq!(
                decoded_response.usage.output_tokens,
                response.usage.output_tokens
            );
            assert_eq!(
                decoded_response.usage.total_tokens,
                response.usage.total_tokens
            );

            if source == target {
                assert_eq!(converted.plan.level(), ConversionLevel::Native);
            }
        }
    }
}

#[test]
fn native_conversion_validates_but_preserves_unknown_wire_fields() {
    for protocol in PROTOCOLS {
        let mut body = fixture(protocol).request;
        body["x_const_api_unknown"] = Value::String("keep-me".to_string());
        let context = context(protocol, "source-model", "source-account");

        let converted = convert_non_stream(
            protocol,
            protocol,
            &body,
            &context,
            &context,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap();

        assert_eq!(converted.body["x_const_api_unknown"], "keep-me");
        assert_eq!(converted.plan.level(), ConversionLevel::Native);
    }
}

#[test]
fn hermes_chat_reasoning_maps_to_all_other_protocols() {
    let source = ProtocolKind::OpenAiChat;
    let source_context = context(source, "source-model", "source-account");
    let body = serde_json::json!({
        "model": "source-model",
        "messages": [{"role": "user", "content": "hello"}],
        "reasoning": {"enabled": true, "effort": "high"}
    });

    for target in [
        ProtocolKind::OpenAiResponses,
        ProtocolKind::AnthropicMessages,
        ProtocolKind::GeminiNative,
    ] {
        let converted = convert_non_stream(
            source,
            target,
            &body,
            &source_context,
            &target_context(source, target),
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap_or_else(|error| panic!("Chat -> {target} failed: {error}"));

        assert!(
            converted
                .plan
                .report()
                .issues
                .iter()
                .all(|issue| issue.code != "provider_extension_omitted")
        );
        match target {
            ProtocolKind::OpenAiResponses => {
                assert_eq!(
                    converted.body.pointer("/reasoning/effort"),
                    Some(&serde_json::json!("high"))
                );
            }
            ProtocolKind::AnthropicMessages => {
                assert_eq!(
                    converted.body.pointer("/thinking/type"),
                    Some(&serde_json::json!("adaptive"))
                );
                assert_eq!(
                    converted.body.pointer("/output_config/effort"),
                    Some(&serde_json::json!("high"))
                );
            }
            ProtocolKind::GeminiNative => {
                assert_eq!(
                    converted
                        .body
                        .pointer("/generationConfig/thinkingConfig/thinkingLevel"),
                    Some(&serde_json::json!("HIGH"))
                );
            }
            ProtocolKind::OpenAiChat => unreachable!(),
        }
    }
}

#[test]
fn unknown_chat_reasoning_member_reports_its_real_extension_name() {
    let source = ProtocolKind::OpenAiChat;
    let target = ProtocolKind::OpenAiResponses;
    let source_context = context(source, "source-model", "source-account");
    let body = serde_json::json!({
        "model": "source-model",
        "messages": [{"role": "user", "content": "hello"}],
        "reasoning": {
            "enabled": true,
            "effort": "high",
            "future_reasoning_option": "diagnose-me"
        }
    });

    let converted = convert_non_stream(
        source,
        target,
        &body,
        &source_context,
        &target_context(source, target),
        &CapabilityProfile::default(),
        ConversionPolicy::Production,
    )
    .expect("advisory reasoning extension should be omitted without blocking");
    let issue = converted
        .plan
        .report()
        .issues
        .iter()
        .find(|issue| issue.code == "provider_extension_omitted")
        .expect("extension warning");

    assert_eq!(issue.path, "$.reasoning.future_reasoning_option");
    assert!(
        issue
            .summary
            .contains("openai_chat.future_reasoning_option")
    );
}

#[test]
fn opencode_stream_usage_option_is_satisfied_without_hiding_other_options() {
    let source = ProtocolKind::OpenAiChat;
    let target = ProtocolKind::OpenAiResponses;
    let source_context = context(source, "source-model", "source-account");
    let mut body = serde_json::json!({
        "model": "source-model",
        "messages": [{"role": "user", "content": "hello"}],
        "stream": true,
        "stream_options": {"include_usage": true}
    });

    let converted = convert_non_stream(
        source,
        target,
        &body,
        &source_context,
        &target_context(source, target),
        &CapabilityProfile::default(),
        ConversionPolicy::Production,
    )
    .expect("OpenCode usage request is fulfilled by the local Chat stream renderer");
    assert!(
        converted
            .plan
            .report()
            .issues
            .iter()
            .all(|issue| issue.code != "provider_extension_omitted")
    );

    body["stream_options"]["include_obfuscation"] = serde_json::json!(false);
    let converted = convert_non_stream(
        source,
        target,
        &body,
        &source_context,
        &target_context(source, target),
        &CapabilityProfile::default(),
        ConversionPolicy::Production,
    )
    .expect("unsupported advisory stream options remain non-blocking");
    let warning = converted
        .plan
        .report()
        .issues
        .iter()
        .find(|issue| issue.code == "provider_extension_omitted")
        .expect("unsupported stream option warning");
    assert_eq!(warning.path, "$.stream_options");
    assert!(warning.summary.contains("openai_chat.stream_options"));
}

#[test]
fn same_protocol_cross_issuer_preserves_deep_wire_fields_and_rewrites_model() {
    for protocol in PROTOCOLS {
        let mut body = fixture(protocol).request;
        let future_path = match protocol {
            ProtocolKind::OpenAiResponses => {
                body["input"][0]["future_metadata"] = serde_json::json!({"trace":{"depth":2}});
                "/input/0/future_metadata/trace/depth"
            }
            ProtocolKind::OpenAiChat => {
                body["messages"][1]["future_metadata"] = serde_json::json!({"trace":{"depth":2}});
                "/messages/1/future_metadata/trace/depth"
            }
            ProtocolKind::AnthropicMessages => {
                body["messages"][0]["future_metadata"] = serde_json::json!({"trace":{"depth":2}});
                "/messages/0/future_metadata/trace/depth"
            }
            ProtocolKind::GeminiNative => {
                body["contents"][0]["futureMetadata"] = serde_json::json!({"trace":{"depth":2}});
                "/contents/0/futureMetadata/trace/depth"
            }
        };
        let source_context = context(protocol, "source-model", "source-account");
        let target_context = context(protocol, "target-model", "target-account");

        let converted = convert_non_stream(
            protocol,
            protocol,
            &body,
            &source_context,
            &target_context,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap();

        assert_eq!(
            converted.body.pointer(future_path),
            Some(&serde_json::json!(2))
        );
        if protocol == ProtocolKind::GeminiNative {
            assert!(converted.body.get("model").is_none());
        } else {
            assert_eq!(converted.body["model"], "target-model");
        }
    }
}

#[test]
fn same_protocol_passthrough_does_not_require_request_or_response_ir_decoding() {
    for protocol in PROTOCOLS {
        let invalid_for_adapter = Value::Null;
        let source_context = context(protocol, "source-model", "source-account");
        let target_context = context(protocol, "target-model", "target-account");

        assert!(decode_request(protocol, &invalid_for_adapter, &source_context).is_err());
        let request = convert_non_stream(
            protocol,
            protocol,
            &invalid_for_adapter,
            &source_context,
            &target_context,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .expect("same-protocol request must bypass the IR adapter");
        assert_eq!(request.body, invalid_for_adapter);
        assert_eq!(request.plan.level(), ConversionLevel::Native);

        assert!(decode_response(protocol, &invalid_for_adapter, &source_context).is_err());
        let response = convert_response_non_stream(
            protocol,
            protocol,
            &invalid_for_adapter,
            &source_context,
            &target_context,
        )
        .expect("same-protocol response must bypass the IR adapter");
        assert_eq!(response.body, invalid_for_adapter);
        assert_eq!(response.plan.level(), ConversionLevel::Native);
    }
}

#[test]
fn cross_protocol_response_unknown_fields_are_reported_before_omission() {
    let source = ProtocolKind::OpenAiResponses;
    let target = ProtocolKind::AnthropicMessages;
    let source_context = context(source, "source-model", "source-account");
    let target_context = context(target, "target-model", "target-account");
    let mut body = fixture(source).response;
    body["future_response_field"] = serde_json::json!({"semantic": true});

    let converted =
        convert_response_non_stream(source, target, &body, &source_context, &target_context)
            .expect("production response conversion remains availability-first");

    assert_eq!(converted.plan.level(), ConversionLevel::Lossy);
    assert!(converted.plan.report().issues.iter().any(|issue| {
        issue.code == "provider_extension_omitted"
            && issue.path == "$.future_response_field"
            && issue.severity == crate::protocol::conversion::IssueSeverity::Warning
    }));
    assert!(converted.body.get("future_response_field").is_none());
}

#[test]
fn cross_protocol_response_nested_unknown_fields_are_reported_at_their_source_path() {
    let source = ProtocolKind::OpenAiChat;
    let target = ProtocolKind::AnthropicMessages;
    let source_context = context(source, "source-model", "source-account");
    let target_context = context(target, "target-model", "target-account");
    let mut body = fixture(source).response;
    body["choices"][0]["message"]["future_semantic"] =
        serde_json::json!({"media_policy": "preserve"});

    let converted =
        convert_response_non_stream(source, target, &body, &source_context, &target_context)
            .expect("nested response metadata must not discard the usable response");

    assert!(converted.plan.report().issues.iter().any(|issue| {
        issue.code == "provider_extension_omitted"
            && issue.path == "$.choices[0].message.future_semantic"
            && issue.severity == crate::protocol::conversion::IssueSeverity::Warning
    }));
    assert!(converted.body.to_string().contains("assistant"));
}

#[test]
fn cross_protocol_unknown_response_items_are_omitted_with_an_artifact_warning() {
    let source = ProtocolKind::OpenAiResponses;
    let target = ProtocolKind::AnthropicMessages;
    let source_context = context(source, "source-model", "source-account");
    let target_context = context(target, "target-model", "target-account");
    let mut body = fixture(source).response;
    body["output"]
        .as_array_mut()
        .expect("Responses fixture output")
        .push(serde_json::json!({
            "type": "future_media_result",
            "payload": {"url": "provider://opaque"}
        }));

    let converted =
        convert_response_non_stream(source, target, &body, &source_context, &target_context)
            .expect("unknown response item must not replace the remaining response");

    assert!(converted.plan.report().issues.iter().any(|issue| {
        issue.code == "response_artifact_approximated"
            && issue.path.starts_with("$.blocks[")
            && issue.severity == crate::protocol::conversion::IssueSeverity::Warning
    }));
    assert!(!converted.body.to_string().contains("future_media_result"));
}

#[test]
fn cross_protocol_error_envelopes_keep_the_error_and_report_unknown_metadata() {
    let source = ProtocolKind::OpenAiChat;
    let target = ProtocolKind::AnthropicMessages;
    let source_context = context(source, "source-model", "source-account");
    let target_context = context(target, "target-model", "target-account");
    let body = serde_json::json!({
        "error": {
            "message": "temporarily unavailable",
            "type": "service_unavailable",
            "code": "upstream_503",
            "retryable": true,
            "details": {"region": "test"},
            "future_diagnostic": {"trace": "opaque"}
        }
    });

    let converted =
        convert_response_non_stream(source, target, &body, &source_context, &target_context)
            .expect("structured provider errors should remain usable cross-protocol");

    assert_eq!(converted.body["type"], "error");
    assert_eq!(
        converted.body["error"]["message"],
        "temporarily unavailable"
    );
    assert!(converted.plan.report().issues.iter().any(|issue| {
        issue.code == "provider_extension_omitted" && issue.path == "$.error.future_diagnostic"
    }));
    assert!(
        converted
            .plan
            .report()
            .issues
            .iter()
            .any(|issue| issue.code == "response_error_metadata_approximated")
    );
}

#[test]
fn unknown_semantic_blocks_are_preserved_natively_and_logged_when_omitted_cross_protocol() {
    for source in PROTOCOLS {
        let mut body = fixture(source).request;
        match source {
            ProtocolKind::OpenAiResponses => {
                body["input"][0]["content"][0] =
                    serde_json::json!({"type":"future_input","payload":{"x":1}});
            }
            ProtocolKind::OpenAiChat => {
                body["messages"][1]["content"] = serde_json::json!([{
                    "type":"future_content","payload":{"x":1}
                }]);
            }
            ProtocolKind::AnthropicMessages => {
                body["messages"][0]["content"][0] = serde_json::json!({
                    "type":"future_content","payload":{"x":1}
                });
            }
            ProtocolKind::GeminiNative => {
                body["contents"][0]["parts"][0] = serde_json::json!({"futurePart":{"x":1}});
            }
        }
        let source_context = context(source, "source-model", "source-account");
        let native_target = context(source, "target-model", "target-account");
        let native = convert_non_stream(
            source,
            source,
            &body,
            &source_context,
            &native_target,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap();
        assert!(native.body.to_string().contains("\"x\":1"));

        let target = match source {
            ProtocolKind::OpenAiChat => ProtocolKind::AnthropicMessages,
            _ => ProtocolKind::OpenAiChat,
        };
        let converted = convert_non_stream(
            source,
            target,
            &body,
            &source_context,
            &context(target, "target-model", "target-account"),
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap();
        assert!(
            converted
                .plan
                .report()
                .issues
                .iter()
                .any(|issue| issue.code == "artifact_filtered")
        );
    }
}

#[test]
fn anthropic_deprecated_output_format_and_max_effort_decode_without_loss() {
    let mut body = fixture(ProtocolKind::AnthropicMessages).request;
    body["output_format"] = serde_json::json!({
        "type":"json_schema",
        "name":"answer",
        "schema":{
            "type":"object",
            "properties":{"answer":{"type":"string"}},
            "required":["answer"]
        }
    });
    body["output_config"] = serde_json::json!({"effort":"max"});
    let source_context = context(
        ProtocolKind::AnthropicMessages,
        "source-model",
        "source-account",
    );

    let decoded = decode_request(ProtocolKind::AnthropicMessages, &body, &source_context).unwrap();
    assert_eq!(decoded.reasoning.effort, Some(ReasoningEffort::Max));
    assert!(matches!(
        decoded.response_format,
        ResponseFormat::JsonSchema { ref name, .. } if name == "answer"
    ));
}

#[test]
fn openai_responses_preserves_max_reasoning_effort() {
    let mut body = fixture(ProtocolKind::OpenAiResponses).request;
    body["reasoning"] = serde_json::json!({"effort": "max"});
    let source_context = context(
        ProtocolKind::OpenAiResponses,
        "source-model",
        "source-account",
    );

    let decoded = decode_request(ProtocolKind::OpenAiResponses, &body, &source_context).unwrap();
    assert_eq!(decoded.reasoning.effort, Some(ReasoningEffort::Max));
}

#[test]
fn openai_protocols_encode_max_and_ultra_without_collapsing_them() {
    for (effort, wire_value) in [
        (ReasoningEffort::Max, "max"),
        (ReasoningEffort::Ultra, "ultra"),
    ] {
        let mut request = text_request();
        request.reasoning.mode = ReasoningMode::Enabled;
        request.reasoning.effort = Some(effort);

        let responses = source_body(ProtocolKind::OpenAiResponses, &request);
        assert_eq!(
            responses.pointer("/reasoning/effort"),
            Some(&serde_json::json!(wire_value))
        );
        let chat = source_body(ProtocolKind::OpenAiChat, &request);
        assert_eq!(
            chat.get("reasoning_effort"),
            Some(&serde_json::json!(wire_value))
        );
    }
}

#[test]
fn cross_protocol_reasoning_efforts_use_the_target_protocol_vocabulary() {
    let mut openai = fixture(ProtocolKind::OpenAiChat).request;
    openai["reasoning_effort"] = serde_json::json!("minimal");
    let anthropic = converted_request_body(
        ProtocolKind::OpenAiChat,
        ProtocolKind::AnthropicMessages,
        &openai,
    );
    assert_eq!(
        anthropic.pointer("/output_config/effort"),
        Some(&serde_json::json!("low"))
    );

    openai["reasoning_effort"] = serde_json::json!("ultra");
    let anthropic = converted_request_body(
        ProtocolKind::OpenAiChat,
        ProtocolKind::AnthropicMessages,
        &openai,
    );
    assert_eq!(
        anthropic.pointer("/output_config/effort"),
        Some(&serde_json::json!("max"))
    );
    let gemini = converted_request_body(
        ProtocolKind::OpenAiChat,
        ProtocolKind::GeminiNative,
        &openai,
    );
    assert_eq!(
        gemini.pointer("/generationConfig/thinkingConfig/thinkingLevel"),
        Some(&serde_json::json!("HIGH"))
    );

    let mut anthropic = fixture(ProtocolKind::AnthropicMessages).request;
    anthropic["thinking"] = serde_json::json!({"type": "adaptive"});
    anthropic["output_config"] = serde_json::json!({"effort": "max"});
    let chat = converted_request_body(
        ProtocolKind::AnthropicMessages,
        ProtocolKind::OpenAiChat,
        &anthropic,
    );
    assert_eq!(
        chat.get("reasoning_effort"),
        Some(&serde_json::json!("max"))
    );
    let responses = converted_request_body(
        ProtocolKind::AnthropicMessages,
        ProtocolKind::OpenAiResponses,
        &anthropic,
    );
    assert_eq!(
        responses.pointer("/reasoning/effort"),
        Some(&serde_json::json!("max"))
    );
    let gemini = converted_request_body(
        ProtocolKind::AnthropicMessages,
        ProtocolKind::GeminiNative,
        &anthropic,
    );
    assert_eq!(
        gemini.pointer("/generationConfig/thinkingConfig/thinkingLevel"),
        Some(&serde_json::json!("HIGH"))
    );

    let mut gemini = fixture(ProtocolKind::GeminiNative).request;
    gemini["generationConfig"]["thinkingConfig"] = serde_json::json!({"thinkingLevel": "HIGH"});
    let chat = converted_request_body(
        ProtocolKind::GeminiNative,
        ProtocolKind::OpenAiChat,
        &gemini,
    );
    assert_eq!(
        chat.get("reasoning_effort"),
        Some(&serde_json::json!("high"))
    );
}

#[test]
fn explicit_reasoning_disable_survives_cross_protocol_conversion() {
    let mut responses = fixture(ProtocolKind::OpenAiResponses).request;
    responses["reasoning"] = serde_json::json!({"effort": "none"});

    let chat = converted_request_body(
        ProtocolKind::OpenAiResponses,
        ProtocolKind::OpenAiChat,
        &responses,
    );
    assert_eq!(
        chat.get("reasoning_effort"),
        Some(&serde_json::json!("none"))
    );
    let anthropic = converted_request_body(
        ProtocolKind::OpenAiResponses,
        ProtocolKind::AnthropicMessages,
        &responses,
    );
    assert_eq!(
        anthropic.pointer("/thinking/type"),
        Some(&serde_json::json!("disabled"))
    );
    assert!(anthropic.pointer("/output_config/effort").is_none());
    let gemini = converted_request_body(
        ProtocolKind::OpenAiResponses,
        ProtocolKind::GeminiNative,
        &responses,
    );
    assert_eq!(
        gemini.pointer("/generationConfig/thinkingConfig/thinkingBudget"),
        Some(&serde_json::json!(0))
    );
    assert!(
        gemini
            .pointer("/generationConfig/thinkingConfig/thinkingLevel")
            .is_none()
    );

    let mut anthropic = fixture(ProtocolKind::AnthropicMessages).request;
    anthropic["thinking"] = serde_json::json!({"type": "disabled"});
    anthropic
        .as_object_mut()
        .expect("Anthropic request object")
        .remove("output_config");
    let responses = converted_request_body(
        ProtocolKind::AnthropicMessages,
        ProtocolKind::OpenAiResponses,
        &anthropic,
    );
    assert_eq!(
        responses.pointer("/reasoning/effort"),
        Some(&serde_json::json!("none"))
    );

    let mut gemini = fixture(ProtocolKind::GeminiNative).request;
    gemini["generationConfig"]["thinkingConfig"] = serde_json::json!({"thinkingBudget": 0});
    let chat = converted_request_body(
        ProtocolKind::GeminiNative,
        ProtocolKind::OpenAiChat,
        &gemini,
    );
    assert_eq!(
        chat.get("reasoning_effort"),
        Some(&serde_json::json!("none"))
    );
}

#[test]
fn missing_chat_reasoning_uses_gemini_model_default_but_explicit_disable_does_not() {
    let mut chat = fixture(ProtocolKind::OpenAiChat).request;
    let gemini =
        converted_request_body(ProtocolKind::OpenAiChat, ProtocolKind::GeminiNative, &chat);
    assert!(gemini.pointer("/generationConfig/thinkingConfig").is_none());

    chat["reasoning"] = serde_json::json!({"enabled": false});
    let gemini =
        converted_request_body(ProtocolKind::OpenAiChat, ProtocolKind::GeminiNative, &chat);
    assert_eq!(
        gemini.pointer("/generationConfig/thinkingConfig/thinkingBudget"),
        Some(&serde_json::json!(0))
    );
}

#[test]
fn anthropic_hosted_tool_calls_and_results_keep_portable_semantics_across_protocols() {
    let body = serde_json::json!({
        "model": "claude-source",
        "max_tokens": 128,
        "messages": [
            {"role":"user","content":"Find the release notes."},
            {"role":"assistant","content":[
                {
                    "type":"server_tool_use",
                    "id":"srv_1",
                    "name":"web_search",
                    "input":{"query":"CONST API release notes"},
                    "caller":{"type":"direct"}
                },
                {
                    "type":"web_search_tool_result",
                    "tool_use_id":"srv_1",
                    "content":[{
                        "type":"web_search_result",
                        "title":"Release notes",
                        "url":"https://example.test/releases",
                        "encrypted_content":"provider-bound"
                    }]
                }
            ]}
        ]
    });
    let source = ProtocolKind::AnthropicMessages;
    let source_context = context(source, "claude-source", "source-account");

    for target in PROTOCOLS {
        let target_context = context(target, "target-model", "target-account");
        let converted = convert_non_stream(
            source,
            target,
            &body,
            &source_context,
            &target_context,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap_or_else(|error| panic!("Anthropic -> {target}: {error}"));
        let decoded = decode_request(target, &converted.body, &target_context)
            .unwrap_or_else(|error| panic!("decode Anthropic -> {target}: {error}"));
        let blocks = decoded
            .turns
            .iter()
            .flat_map(|turn| turn.blocks.iter())
            .collect::<Vec<_>>();

        assert!(
            blocks.iter().any(|block| matches!(
                block,
                ContentBlock::ToolCall(call)
                    if call.id == "srv_1"
                        && call.name == "web_search"
                        && call.arguments.as_ref().and_then(|value| value.get("query"))
                            == Some(&serde_json::json!("CONST API release notes"))
            )),
            "Anthropic -> {target} lost the hosted tool call"
        );
        assert!(
            blocks.iter().any(|block| matches!(
                block,
                ContentBlock::ToolResult(result)
                    if result.call_id == "srv_1"
                        && visible_text(&result.content).contains("Release notes")
                        && visible_text(&result.content).contains("https://example.test/releases")
            )),
            "Anthropic -> {target} lost the hosted tool result"
        );

        if target != source {
            assert!(
                converted
                    .plan
                    .report()
                    .issues
                    .iter()
                    .any(|issue| issue.code == "artifact_filtered")
            );
        }
    }
}

#[test]
fn structured_output_schema_crosses_every_protocol_pair_without_semantic_loss() {
    let mut request = text_request();
    request.response_format = ResponseFormat::JsonSchema {
        name: "answer".to_string(),
        description: Some("A short answer".to_string()),
        schema: serde_json::json!({
            "type":"object",
            "properties":{
                "steps":{
                    "type":"array",
                    "items":{
                        "type":"object",
                        "properties":{
                            "action":{"type":"string","enum":["read","write"]},
                            "payload":{
                                "oneOf":[
                                    {
                                        "type":"object",
                                        "properties":{"path":{"type":"string","minLength":1}},
                                        "required":["path"],
                                        "additionalProperties":false
                                    },
                                    {"type":"array","items":{"type":"integer","minimum":0}}
                                ]
                            }
                        },
                        "required":["action","payload"],
                        "additionalProperties":false
                    }
                }
            },
            "required":["steps"],
            "additionalProperties":false
        }),
        strict: Some(true),
    };

    for source in PROTOCOLS {
        let body = source_body(source, &request);
        let source_context = context(source, "source-model", "source-account");
        for target in PROTOCOLS {
            let target_context = target_context(source, target);
            let converted = convert_non_stream(
                source,
                target,
                &body,
                &source_context,
                &target_context,
                &CapabilityProfile::default(),
                ConversionPolicy::Production,
            )
            .unwrap_or_else(|error| panic!("{source} -> {target}: {error}"));
            let decoded = decode_request(target, &converted.body, &target_context).unwrap();
            let ResponseFormat::JsonSchema {
                name,
                schema,
                strict,
                ..
            } = decoded.response_format
            else {
                panic!("{source} -> {target} lost JSON schema");
            };
            if source == ProtocolKind::GeminiNative || target == ProtocolKind::GeminiNative {
                assert_eq!(name, "gemini_response", "{source} -> {target}");
            } else {
                assert_eq!(name, "answer", "{source} -> {target}");
            }
            assert_eq!(schema["required"][0], "steps", "{source} -> {target}");
            assert_eq!(
                schema.pointer("/properties/steps/items/properties/action/enum/1"),
                Some(&serde_json::json!("write")),
                "{source} -> {target}"
            );
            assert_eq!(
                schema.pointer(
                    "/properties/steps/items/properties/payload/oneOf/0/properties/path/minLength"
                ),
                Some(&serde_json::json!(1)),
                "{source} -> {target}"
            );
            assert_eq!(
                schema.pointer("/properties/steps/items/properties/payload/oneOf/1/items/minimum"),
                Some(&serde_json::json!(0)),
                "{source} -> {target}"
            );
            // Gemini enforces responseJsonSchema directly and has no separate wire-level
            // name/strict flags to reconstruct on decode.
            if target == ProtocolKind::GeminiNative {
                assert_eq!(strict, None, "{source} -> {target}");
            } else if source != ProtocolKind::GeminiNative {
                assert_eq!(strict, Some(true), "{source} -> {target}");
            }
        }
    }
}

#[test]
fn deep_tool_schemas_arguments_and_results_cross_all_sixteen_pairs() {
    let schema = serde_json::json!({
        "type":"object",
        "properties":{
            "task":{
                "type":"object",
                "properties":{
                    "steps":{
                        "type":"array",
                        "items":{
                            "type":"object",
                            "properties":{
                                "action":{"type":"string"},
                                "payload":{
                                    "oneOf":[
                                        {"type":"object","additionalProperties":{"type":"string"}},
                                        {"type":"array","items":{"type":"integer"}}
                                    ]
                                }
                            },
                            "required":["action","payload"]
                        }
                    }
                },
                "required":["steps"]
            }
        },
        "required":["task"],
        "additionalProperties":false
    });
    let arguments = serde_json::json!({
        "task":{
            "steps":[
                {"action":"read","payload":{"path":"README.md","encoding":"utf-8"}},
                {"action":"write","payload":[1,2,3]}
            ]
        }
    });
    let mut request = text_request();
    request.tools.push(ToolDefinition::Function(FunctionTool {
        name: "run_steps".to_string(),
        description: Some("Execute nested steps".to_string()),
        input_schema: schema,
        strict: Some(true),
        cache_policy: None,
        defer_loading: None,
        allowed_callers: Vec::new(),
        input_examples: Vec::new(),
        eager_input_streaming: None,
    }));
    request.turns.push(Turn {
        id: None,
        role: TurnRole::Assistant,
        blocks: vec![ContentBlock::ToolCall(ToolCall::function(
            "call-deep",
            "run_steps",
            arguments.clone(),
        ))],
        status: Some(ItemStatus::Completed),
    });
    request.turns.push(Turn {
        id: None,
        role: TurnRole::User,
        blocks: vec![ContentBlock::ToolResult(ToolResult {
            call_id: "call-deep".to_string(),
            name: Some("run_steps".to_string()),
            content: vec![ContentBlock::Text(TextBlock::new(
                r#"{"ok":true,"written":[1,2,3]}"#,
            ))],
            status: ItemStatus::Completed,
            is_error: false,
            metadata: BlockMetadata::default(),
        })],
        status: Some(ItemStatus::Completed),
    });

    for source in PROTOCOLS {
        let body = source_body(source, &request);
        let source_context = context(source, "source-model", "source-account");
        for target in PROTOCOLS {
            let target_context = target_context(source, target);
            let converted = convert_non_stream(
                source,
                target,
                &body,
                &source_context,
                &target_context,
                &CapabilityProfile::default(),
                ConversionPolicy::Production,
            )
            .unwrap_or_else(|error| panic!("{source} -> {target}: {error}"));
            let decoded = decode_request(target, &converted.body, &target_context).unwrap();
            let ToolDefinition::Function(tool) = &decoded.tools[0] else {
                panic!("{source} -> {target} lost function tool");
            };
            assert_eq!(
                tool.input_schema.pointer(
                    "/properties/task/properties/steps/items/properties/payload/oneOf/0/additionalProperties/type"
                ),
                Some(&serde_json::json!("string")),
                "{source} -> {target}"
            );
            let call = decoded
                .turns
                .iter()
                .flat_map(|turn| &turn.blocks)
                .find_map(|block| match block {
                    ContentBlock::ToolCall(call) => Some(call),
                    _ => None,
                })
                .expect("deep tool call");
            assert_eq!(
                call.arguments.as_ref(),
                Some(&arguments),
                "{source} -> {target}"
            );
            assert!(decoded.turns.iter().flat_map(|turn| &turn.blocks).any(|block| {
                matches!(block, ContentBlock::ToolResult(result) if visible_text(&result.content).contains("written"))
            }));
        }
    }
}

#[test]
fn nested_future_fields_are_native_passthrough_and_cross_protocol_diagnostics() {
    let source = ProtocolKind::OpenAiChat;
    let mut body = fixture(source).request;
    body["messages"][1]["content"] = serde_json::json!([{
        "type":"text",
        "text":"Read README.md",
        "future_metadata":{"trace":{"levels":[{"depth":1},{"depth":2}]}}
    }]);
    let source_context = context(source, "source-model", "source-account");
    let native = convert_non_stream(
        source,
        source,
        &body,
        &source_context,
        &source_context,
        &CapabilityProfile::default(),
        ConversionPolicy::Production,
    )
    .unwrap();
    assert_eq!(
        native
            .body
            .pointer("/messages/1/content/0/future_metadata/trace/levels/1/depth"),
        Some(&serde_json::json!(2))
    );

    let decoded_source = decode_request(source, &body, &source_context).unwrap();
    let extension = decoded_source
        .extensions
        .iter()
        .find(|extension| {
            extension.nested_path() == Some("$.messages[1].content[0].future_metadata")
        })
        .expect("nested future field");
    assert_eq!(extension.value["trace"]["levels"][1]["depth"], 2);

    let target = ProtocolKind::AnthropicMessages;
    let target_context = target_context(source, target);
    let converted = convert_non_stream(
        source,
        target,
        &body,
        &source_context,
        &target_context,
        &CapabilityProfile::default(),
        ConversionPolicy::Production,
    )
    .unwrap();
    assert_eq!(converted.plan.level(), ConversionLevel::Lossy);
    assert!(converted.plan.report().issues.iter().any(|issue| {
        issue.code == "provider_extension_omitted"
            && issue.path == "$.messages[1].content[0].future_metadata"
    }));
    assert!(
        !serde_json::to_string(&converted.body)
            .unwrap()
            .contains("future_metadata")
    );
}

#[test]
fn gemini_advisory_extensions_are_omitted_but_context_references_still_block() {
    let source = ProtocolKind::GeminiNative;
    let target = ProtocolKind::OpenAiChat;
    let source_context = context(source, "source-model", "source-account");
    let target_context = target_context(source, target);
    let mut body = fixture(source).request;
    body["safetySettings"] = serde_json::json!([{
        "category":"HARM_CATEGORY_DANGEROUS_CONTENT",
        "threshold":"BLOCK_ONLY_HIGH"
    }]);
    body["store"] = serde_json::json!(true);
    body["generationConfig"]["thinkingConfig"] = serde_json::json!({
        "thinkingBudget": 256,
        "futureThinkingOption": {"mode":"balanced"}
    });

    let converted = convert_non_stream(
        source,
        target,
        &body,
        &source_context,
        &target_context,
        &CapabilityProfile::default(),
        ConversionPolicy::Production,
    )
    .expect("advisory Gemini extensions should not make a request unusable");
    assert_eq!(converted.plan.level(), ConversionLevel::Lossy);
    assert!(
        converted
            .plan
            .report()
            .issues
            .iter()
            .filter(|issue| issue.code == "provider_extension_omitted")
            .count()
            >= 3
    );

    body["cachedContent"] = serde_json::json!("cachedContents/context-1");
    let converted = convert_non_stream(
        source,
        target,
        &body,
        &source_context,
        &target_context,
        &CapabilityProfile::default(),
        ConversionPolicy::Production,
    )
    .expect("production prioritizes an available cross-protocol request");
    assert_eq!(converted.plan.level(), ConversionLevel::Lossy);
    assert!(
        converted
            .plan
            .report()
            .issues
            .iter()
            .any(|issue| issue.code == "provider_extension_approximated"
                && issue.path == "$.cachedContent")
    );
}

#[test]
fn output_verbosity_maps_between_openai_protocols_and_is_safely_omitted_elsewhere() {
    let sources = [ProtocolKind::OpenAiChat, ProtocolKind::OpenAiResponses];
    let mut request = text_request();
    request.generation.verbosity = Some(OutputVerbosity::High);

    for source in sources {
        let body = source_body(source, &request);
        let source_context = context(source, "source-model", "source-account");
        for target in PROTOCOLS {
            let target_context = target_context(source, target);
            let converted = convert_non_stream(
                source,
                target,
                &body,
                &source_context,
                &target_context,
                &CapabilityProfile::default(),
                ConversionPolicy::Production,
            )
            .unwrap_or_else(|error| panic!("{source} -> {target}: {error}"));
            let decoded = decode_request(target, &converted.body, &target_context).unwrap();
            if matches!(
                target,
                ProtocolKind::OpenAiChat | ProtocolKind::OpenAiResponses
            ) {
                assert_eq!(
                    decoded.generation.verbosity,
                    Some(OutputVerbosity::High),
                    "{source} -> {target}"
                );
            } else {
                assert_eq!(decoded.generation.verbosity, None, "{source} -> {target}");
                assert!(converted.plan.report().issues.iter().any(|issue| {
                    issue.code == "unsupported_feature_omitted"
                        && issue.feature == Some(Feature::OutputVerbosity)
                }));
            }
        }
    }
}

#[test]
fn prompt_cache_options_map_between_openai_protocols_and_are_safely_omitted_elsewhere() {
    let source = ProtocolKind::OpenAiResponses;
    let mut request = text_request();
    request.metadata.prompt_cache.key = Some("session-cache-key".to_string());
    let body = source_body(source, &request);
    let source_context = context(source, "source-model", "source-account");

    for target in PROTOCOLS {
        let target_context = target_context(source, target);
        let converted = convert_non_stream(
            source,
            target,
            &body,
            &source_context,
            &target_context,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap_or_else(|error| panic!("{source} -> {target}: {error}"));
        let decoded = decode_request(target, &converted.body, &target_context).unwrap();
        if matches!(
            target,
            ProtocolKind::OpenAiChat | ProtocolKind::OpenAiResponses
        ) {
            assert_eq!(
                decoded.metadata.prompt_cache.key.as_deref(),
                Some("session-cache-key"),
                "{source} -> {target}"
            );
        } else {
            assert_eq!(
                decoded.metadata.prompt_cache.key, None,
                "{source} -> {target}"
            );
            assert!(converted.plan.report().issues.iter().any(|issue| {
                issue.code == "unsupported_feature_omitted"
                    && issue.feature == Some(Feature::PromptCacheOptions)
            }));
        }
    }
}

#[test]
fn gemini_thought_signature_maps_through_openai_compatibility_shape() {
    let source = ProtocolKind::OpenAiChat;
    let target = ProtocolKind::GeminiNative;
    let mut body = fixture(source).request;
    body["messages"][2]["tool_calls"][0]["extra_content"] =
        serde_json::json!({"google":{"thought_signature":"signature-1"}});
    let google_context = AdapterContext {
        dialect: ProviderDialect::Compatible {
            provider: "google".to_string(),
        },
        issuer: IssuerIdentity {
            provider: "google".to_string(),
            endpoint_fingerprint: "google-endpoint".to_string(),
            account_fingerprint: Some("google-account".to_string()),
            model_family: Some("source-model".to_string()),
        },
    };
    let converted = convert_non_stream(
        source,
        target,
        &body,
        &google_context,
        &google_context,
        &CapabilityProfile::default(),
        ConversionPolicy::Production,
    )
    .unwrap();
    assert!(
        serde_json::to_string(&converted.body)
            .unwrap()
            .contains("signature-1")
    );
    let decoded = decode_request(target, &converted.body, &google_context).unwrap();
    assert!(decoded.turns.iter().flat_map(|turn| &turn.blocks).any(|block| {
        matches!(block, ContentBlock::ToolCall(call) if call.artifacts.iter().any(|artifact| artifact.kind == ArtifactKind::GeminiThoughtSignature && artifact.payload == "signature-1"))
    }));
}

fn continuation_source_response(protocol: ProtocolKind) -> Value {
    match protocol {
        ProtocolKind::OpenAiResponses => json!({
            "id":"resp-state","object":"response","status":"completed","model":"source-model",
            "output":[
                {"type":"reasoning","id":"rs-state","status":"completed","summary":[],"encrypted_content":"openai-state"},
                {"type":"function_call","id":"fc-state","status":"completed","call_id":"call-state","name":"read_file","arguments":"{\"path\":\"README.md\"}"}
            ],
            "usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}
        }),
        ProtocolKind::AnthropicMessages => json!({
            "id":"msg-state","type":"message","role":"assistant","model":"source-model",
            "content":[
                {"type":"thinking","thinking":"check the file","signature":"anthropic-state"},
                {"type":"tool_use","id":"call-state","name":"read_file","input":{"path":"README.md"}}
            ],
            "stop_reason":"tool_use","usage":{"input_tokens":1,"output_tokens":1}
        }),
        ProtocolKind::GeminiNative => json!({
            "responseId":"gemini-state","modelVersion":"source-model",
            "candidates":[{"content":{"role":"model","parts":[{
                "thoughtSignature":"gemini-state",
                "functionCall":{"id":"call-state","name":"read_file","args":{"path":"README.md"}}
            }]},"finishReason":"STOP"}],
            "usageMetadata":{"promptTokenCount":1,"candidatesTokenCount":1,"totalTokenCount":2}
        }),
        ProtocolKind::OpenAiChat => unreachable!("Chat is a carrier target, not an opaque source"),
    }
}

fn next_request_from_converted_response(protocol: ProtocolKind, response: &Value) -> Value {
    match protocol {
        ProtocolKind::OpenAiResponses => {
            let mut input = response["output"]
                .as_array()
                .cloned()
                .expect("Responses output");
            input.push(json!({
                "type":"function_call_output","call_id":"call-state","output":"file contents"
            }));
            json!({"model":"client-model","input":input})
        }
        ProtocolKind::OpenAiChat => json!({
            "model":"client-model",
            "messages":[
                response["choices"][0]["message"].clone(),
                {"role":"tool","tool_call_id":"call-state","name":"read_file","content":"file contents"}
            ]
        }),
        ProtocolKind::AnthropicMessages => json!({
            "model":"client-model","max_tokens":64,
            "messages":[
                {"role":"assistant","content":response["content"].clone()},
                {"role":"user","content":[{
                    "type":"tool_result","tool_use_id":"call-state","content":"file contents"
                }]}
            ]
        }),
        ProtocolKind::GeminiNative => json!({
            "model":"client-model",
            "contents":[
                {"role":"model","parts":response["candidates"][0]["content"]["parts"].clone()},
                {"role":"user","parts":[{"functionResponse":{
                    "id":"call-state","name":"read_file","response":{"output":"file contents"}
                }}]}
            ]
        }),
    }
}

fn assert_original_continuation_reaches_origin(protocol: ProtocolKind, request: &Value) {
    match protocol {
        ProtocolKind::OpenAiResponses => {
            assert!(
                request["input"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|item| item["type"] == "reasoning"
                        && item["encrypted_content"] == "openai-state")
            )
        }
        ProtocolKind::AnthropicMessages => assert!(request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|message| message["content"].as_array().into_iter().flatten())
            .any(|block| block["type"] == "thinking" && block["signature"] == "anthropic-state")),
        ProtocolKind::GeminiNative => assert!(
            request["contents"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|content| content["parts"].as_array().into_iter().flatten())
                .any(|part| part.get("functionCall").is_some()
                    && part["thoughtSignature"] == "gemini-state")
        ),
        ProtocolKind::OpenAiChat => unreachable!(),
    }
    assert!(
        !serde_json::to_string(request)
            .unwrap()
            .contains("const-api-continuation-v1:")
    );
}

#[test]
fn opaque_continuation_round_trips_through_every_foreign_client_protocol() {
    for origin in [
        ProtocolKind::OpenAiResponses,
        ProtocolKind::AnthropicMessages,
        ProtocolKind::GeminiNative,
    ] {
        let origin_context = context(origin, "source-model", "source-account");
        for client in PROTOCOLS.into_iter().filter(|client| *client != origin) {
            let client_context = context(client, "client-model", "client-account");
            let response = convert_response_non_stream(
                origin,
                client,
                &continuation_source_response(origin),
                &origin_context,
                &client_context,
            )
            .unwrap_or_else(|error| panic!("{origin} -> {client} response: {error}"));
            let next_request = next_request_from_converted_response(client, &response.body);
            let restored = convert_non_stream(
                client,
                origin,
                &next_request,
                &client_context,
                &origin_context,
                &CapabilityProfile::default(),
                ConversionPolicy::Production,
            )
            .unwrap_or_else(|error| panic!("{client} -> {origin} continuation: {error}"));
            assert_original_continuation_reaches_origin(origin, &restored.body);
        }
    }
}

#[test]
fn anthropic_redacted_thinking_round_trips_through_every_foreign_client_protocol() {
    let origin = ProtocolKind::AnthropicMessages;
    let origin_context = context(origin, "source-model", "source-account");
    let source = json!({
        "id":"msg-redacted","type":"message","role":"assistant","model":"source-model",
        "content":[
            {"type":"redacted_thinking","data":"anthropic-redacted-state"},
            {"type":"tool_use","id":"call-state","name":"read_file","input":{"path":"README.md"}}
        ],
        "stop_reason":"tool_use","usage":{"input_tokens":1,"output_tokens":1}
    });

    for client in PROTOCOLS.into_iter().filter(|client| *client != origin) {
        let client_context = context(client, "client-model", "client-account");
        let response =
            convert_response_non_stream(origin, client, &source, &origin_context, &client_context)
                .unwrap_or_else(|error| panic!("Anthropic redacted -> {client} response: {error}"));
        let next_request = next_request_from_converted_response(client, &response.body);
        let restored = convert_non_stream(
            client,
            origin,
            &next_request,
            &client_context,
            &origin_context,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap_or_else(|error| panic!("{client} -> Anthropic redacted continuation: {error}"));
        assert!(
            restored.body["messages"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|message| message["content"].as_array().into_iter().flatten())
                .any(|block| block["type"] == "redacted_thinking"
                    && block["data"] == "anthropic-redacted-state"),
            "{client}: {}",
            restored.body
        );
        assert!(
            !serde_json::to_string(&restored.body)
                .unwrap()
                .contains("const-api-continuation-v1:")
        );
    }
}

#[test]
fn response_bridge_continuation_is_bound_to_protocol_and_upstream_model() {
    let source_context = AdapterContext::runtime(
        "response:gemini_native",
        "response-upstream",
        None,
        "source-model",
    );
    let client_context = context(
        ProtocolKind::OpenAiResponses,
        "client-model",
        "client-account",
    );
    let response = convert_response_non_stream(
        ProtocolKind::GeminiNative,
        ProtocolKind::OpenAiResponses,
        &continuation_source_response(ProtocolKind::GeminiNative),
        &source_context,
        &client_context,
    )
    .unwrap();
    let next_request =
        next_request_from_converted_response(ProtocolKind::OpenAiResponses, &response.body);
    let responses_target = AdapterContext::runtime(
        "channel:openai_responses",
        "runtime-target:openai_responses",
        None,
        "gpt-target",
    );
    let rerouted = convert_non_stream(
        ProtocolKind::OpenAiResponses,
        ProtocolKind::OpenAiResponses,
        &next_request,
        &client_context,
        &responses_target,
        &CapabilityProfile::default(),
        ConversionPolicy::Production,
    )
    .unwrap();
    let rerouted_text = serde_json::to_string(&rerouted.body).unwrap();
    assert!(!rerouted_text.contains("const-api-continuation-v1:"));
    assert!(rerouted_text.contains("call-state"));

    let same_model_context = AdapterContext::runtime(
        "channel:gemini_native",
        "runtime-target:gemini_native",
        None,
        "source-model",
    );
    let restored = convert_non_stream(
        ProtocolKind::OpenAiResponses,
        ProtocolKind::GeminiNative,
        &next_request,
        &client_context,
        &same_model_context,
        &CapabilityProfile::default(),
        ConversionPolicy::Production,
    )
    .unwrap();
    assert_original_continuation_reaches_origin(ProtocolKind::GeminiNative, &restored.body);

    let switched_model_context = AdapterContext::runtime(
        "channel:gemini_native",
        "runtime-target:gemini_native",
        None,
        "different-model",
    );
    let switched = convert_non_stream(
        ProtocolKind::OpenAiResponses,
        ProtocolKind::GeminiNative,
        &next_request,
        &client_context,
        &switched_model_context,
        &CapabilityProfile::default(),
        ConversionPolicy::Production,
    )
    .unwrap();
    let function_part = switched.body["contents"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|content| content["parts"].as_array().into_iter().flatten())
        .find(|part| part.get("functionCall").is_some())
        .unwrap();
    assert_eq!(
        function_part["thoughtSignature"],
        "skip_thought_signature_validator"
    );
    assert_ne!(function_part["thoughtSignature"], "gemini-state");
}

#[test]
fn gemini_text_signature_round_trips_through_every_foreign_client_protocol() {
    let gemini_context = context(
        ProtocolKind::GeminiNative,
        "gemini-3.6-flash",
        "google-account",
    );
    let source = json!({
        "responseId":"gemini-text-state","modelVersion":"gemini-3.6-flash",
        "candidates":[{"content":{"role":"model","parts":[{
            "text":"answer","thoughtSignature":"gemini-text-signature"
        }]} ,"finishReason":"STOP"}],
        "usageMetadata":{"promptTokenCount":1,"candidatesTokenCount":1,"totalTokenCount":2}
    });

    for client in [
        ProtocolKind::OpenAiResponses,
        ProtocolKind::OpenAiChat,
        ProtocolKind::AnthropicMessages,
    ] {
        let client_context = context(client, "client-model", "client-account");
        let response = convert_response_non_stream(
            ProtocolKind::GeminiNative,
            client,
            &source,
            &gemini_context,
            &client_context,
        )
        .unwrap_or_else(|error| panic!("Gemini -> {client} response: {error}"));
        let next_request = match client {
            ProtocolKind::OpenAiResponses => json!({
                "model":"client-model",
                "input":response.body["output"].as_array().unwrap().iter().cloned()
                    .chain(std::iter::once(json!({"role":"user","content":"continue"})))
                    .collect::<Vec<_>>()
            }),
            ProtocolKind::OpenAiChat => json!({
                "model":"client-model",
                "messages":[
                    response.body["choices"][0]["message"].clone(),
                    {"role":"user","content":"continue"}
                ]
            }),
            ProtocolKind::AnthropicMessages => json!({
                "model":"client-model","max_tokens":64,
                "messages":[
                    {"role":"assistant","content":response.body["content"].clone()},
                    {"role":"user","content":"continue"}
                ]
            }),
            ProtocolKind::GeminiNative => unreachable!(),
        };
        let restored = convert_non_stream(
            client,
            ProtocolKind::GeminiNative,
            &next_request,
            &client_context,
            &gemini_context,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap_or_else(|error| panic!("{client} -> Gemini continuation: {error}"));
        let text_part = restored.body["contents"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|content| content["parts"].as_array().into_iter().flatten())
            .find(|part| part["text"] == "answer")
            .unwrap_or_else(|| panic!("{client} restored text part"));
        assert_eq!(
            text_part["thoughtSignature"], "gemini-text-signature",
            "{client}"
        );
        assert!(
            !serde_json::to_string(&restored.body)
                .unwrap()
                .contains("const-api-continuation-v1:"),
            "{client} carrier leaked upstream"
        );
    }
}

#[test]
fn gemini_external_tool_history_gets_one_bypass_signature_per_parallel_step() {
    let body = json!({
        "model":"client-model",
        "input":[
            {"role":"user","content":"Run both tools."},
            {"type":"function_call","call_id":"call-a","name":"first","arguments":"{}"},
            {"type":"function_call","call_id":"call-b","name":"second","arguments":"{}"},
            {"type":"function_call_output","call_id":"call-a","output":"a"},
            {"type":"function_call_output","call_id":"call-b","output":"b"}
        ]
    });
    let converted = convert_non_stream(
        ProtocolKind::OpenAiResponses,
        ProtocolKind::GeminiNative,
        &body,
        &context(
            ProtocolKind::OpenAiResponses,
            "client-model",
            "client-account",
        ),
        &context(
            ProtocolKind::GeminiNative,
            "gemini-3.6-flash",
            "target-account",
        ),
        &CapabilityProfile::default(),
        ConversionPolicy::Production,
    )
    .unwrap();
    let calls = converted.body["contents"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|content| content["parts"].as_array().into_iter().flatten())
        .filter(|part| part.get("functionCall").is_some())
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[0]["thoughtSignature"],
        "skip_thought_signature_validator"
    );
    assert!(calls[1].get("thoughtSignature").is_none());
    let results = converted.body["contents"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|content| content["parts"].as_array().into_iter().flatten())
        .filter_map(|part| part.get("functionResponse"))
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["name"], "first");
    assert_eq!(results[1]["name"], "second");
}

#[test]
fn media_matrix_preserves_supported_classes_and_blocks_unsupported_targets() {
    let cases = [
        (
            Feature::ImageInput,
            ContentBlock::Image(MediaBlock {
                source: MediaSource::InlineBase64 {
                    media_type: "image/png".to_string(),
                    data: "AA==".to_string(),
                },
                detail: None,
                metadata: BlockMetadata::default(),
            }),
            vec![
                ProtocolKind::OpenAiResponses,
                ProtocolKind::OpenAiChat,
                ProtocolKind::AnthropicMessages,
                ProtocolKind::GeminiNative,
            ],
        ),
        (
            Feature::AudioInput,
            ContentBlock::Audio(MediaBlock {
                source: MediaSource::InlineBase64 {
                    media_type: "audio/wav".to_string(),
                    data: "AA==".to_string(),
                },
                detail: None,
                metadata: BlockMetadata::default(),
            }),
            vec![ProtocolKind::OpenAiChat, ProtocolKind::GeminiNative],
        ),
        (
            Feature::VideoInput,
            ContentBlock::Video(MediaBlock {
                source: MediaSource::InlineBase64 {
                    media_type: "video/mp4".to_string(),
                    data: "AA==".to_string(),
                },
                detail: None,
                metadata: BlockMetadata::default(),
            }),
            vec![ProtocolKind::GeminiNative],
        ),
        (
            Feature::FileInput,
            ContentBlock::File(FileBlock {
                source: MediaSource::InlineBase64 {
                    media_type: "application/pdf".to_string(),
                    data: "AA==".to_string(),
                },
                filename: Some("sample.pdf".to_string()),
                metadata: BlockMetadata::default(),
            }),
            vec![
                ProtocolKind::OpenAiResponses,
                ProtocolKind::AnthropicMessages,
                ProtocolKind::GeminiNative,
            ],
        ),
    ];

    for (feature, block, sources) in cases {
        for source in sources {
            let mut request = text_request();
            request.turns[0].blocks.push(block.clone());
            let body = source_body(source, &request);
            let source_context = context(source, "source-model", "source-account");
            for target in PROTOCOLS {
                let target_context = target_context(source, target);
                let result = convert_non_stream(
                    source,
                    target,
                    &body,
                    &source_context,
                    &target_context,
                    &CapabilityProfile::default(),
                    ConversionPolicy::Production,
                );
                if protocol_wire_supports(target, feature) {
                    let converted =
                        result.unwrap_or_else(|error| panic!("{source} -> {target}: {error}"));
                    let decoded = decode_request(target, &converted.body, &target_context).unwrap();
                    assert!(
                        decoded.turns[0]
                            .blocks
                            .iter()
                            .any(|candidate| std::mem::discriminant(candidate)
                                == std::mem::discriminant(&block)),
                        "{source} -> {target} lost {feature:?}"
                    );
                } else {
                    assert_unsupported_feature(
                        &result.expect_err("unsupported media must be blocked"),
                        feature,
                        source,
                        target,
                    );
                }
            }
        }
    }
}

#[test]
fn anthropic_cache_control_is_preserved_or_safely_omitted() {
    let source = ProtocolKind::AnthropicMessages;
    let mut request = text_request();
    let ContentBlock::Text(text) = &mut request.turns[0].blocks[0] else {
        unreachable!()
    };
    text.metadata.cache_policy = Some(CachePolicy::Ephemeral5m);
    let body = source_body(source, &request);
    let source_context = context(source, "source-model", "source-account");

    for target in PROTOCOLS {
        let target_context = target_context(source, target);
        let result = convert_non_stream(
            source,
            target,
            &body,
            &source_context,
            &target_context,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        );
        if protocol_wire_supports(target, Feature::CacheControl) {
            let converted = result.unwrap_or_else(|error| panic!("{target}: {error}"));
            let decoded = decode_request(target, &converted.body, &target_context).unwrap();
            let ContentBlock::Text(text) = &decoded.turns[0].blocks[0] else {
                panic!("{target} text block");
            };
            assert_eq!(
                text.metadata.cache_policy,
                Some(CachePolicy::Ephemeral5m),
                "Anthropic -> {target}"
            );
        } else {
            let converted = result.unwrap_or_else(|error| panic!("{target}: {error}"));
            let decoded = decode_request(target, &converted.body, &target_context).unwrap();
            let ContentBlock::Text(text) = &decoded.turns[0].blocks[0] else {
                panic!("{target} text block");
            };
            assert_eq!(text.metadata.cache_policy, None, "Anthropic -> {target}");
            assert!(converted.plan.report().issues.iter().any(|issue| {
                issue.code == "unsupported_feature_omitted"
                    && issue.feature == Some(Feature::CacheControl)
                    && issue.summary.contains("CacheControl")
                    && issue.summary.contains(target.as_str())
            }));
        }
    }
}

#[test]
fn refusal_and_content_filter_status_cross_every_response_target() {
    for source in PROTOCOLS {
        let source_context = context(source, "source-model", "source-account");
        for target in PROTOCOLS {
            let target_context = target_context(source, target);
            let request = convert_non_stream(
                source,
                target,
                &fixture(source).request,
                &source_context,
                &target_context,
                &CapabilityProfile::default(),
                ConversionPolicy::Production,
            )
            .unwrap();
            let mut response =
                decode_response(source, &fixture(source).response, &source_context).unwrap();
            response.blocks = vec![ContentBlock::Refusal(RefusalBlock::new("Request blocked"))];
            response.status = ResponseStatus::Refused;
            response.finish.reason = FinishReason::ContentFilter;
            response.finish.original_reason = Some("policy".to_string());
            let encoded = encode_response(target, &response, &request.plan, &target_context)
                .unwrap_or_else(|error| panic!("{source} -> {target}: {error}"));
            let decoded = decode_response(target, &encoded, &target_context).unwrap();
            assert_eq!(
                decoded.status,
                ResponseStatus::Refused,
                "{source} -> {target}"
            );
            assert!(
                matches!(
                    decoded.finish.reason,
                    FinishReason::ContentFilter | FinishReason::Refusal
                ),
                "{source} -> {target}: {:?}",
                decoded.finish.reason
            );
            assert_eq!(
                visible_text(&decoded.blocks),
                "Request blocked",
                "{source} -> {target}"
            );
        }
    }
}

#[test]
fn unsigned_reasoning_history_is_omitted_across_protocols_without_losing_visible_content() {
    let source = ProtocolKind::OpenAiChat;
    let mut request = text_request();
    request.turns.push(Turn {
        id: None,
        role: TurnRole::Assistant,
        blocks: vec![
            ContentBlock::Reasoning(ReasoningBlock {
                text: Some("Check the constraints.".to_string()),
                summary: Vec::new(),
                artifacts: Vec::new(),
                encrypted: false,
                metadata: BlockMetadata::default(),
            }),
            ContentBlock::Text(TextBlock::new("Visible answer.")),
        ],
        status: None,
    });
    let body = source_body(source, &request);
    let source_context = context(source, "source-model", "source-account");

    for target in PROTOCOLS {
        let target_context = target_context(source, target);
        let result = convert_non_stream(
            source,
            target,
            &body,
            &source_context,
            &target_context,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        );
        let converted = result.unwrap_or_else(|error| panic!("{source} -> {target}: {error}"));
        let decoded = decode_request(target, &converted.body, &target_context).unwrap();
        assert!(
            visible_text(
                &decoded
                    .turns
                    .iter()
                    .flat_map(|turn| turn.blocks.clone())
                    .collect::<Vec<_>>()
            )
            .contains("Visible answer.")
        );
        if target == source {
            assert!(decoded.turns.iter().flat_map(|turn| &turn.blocks).any(|block| {
                matches!(block, ContentBlock::Reasoning(reasoning) if reasoning.text.as_deref() == Some("Check the constraints."))
            }));
        } else {
            assert!(
                !decoded
                    .turns
                    .iter()
                    .flat_map(|turn| &turn.blocks)
                    .any(|block| matches!(block, ContentBlock::Reasoning(_)))
            );
            assert!(converted.plan.report().issues.iter().any(|issue| {
                issue.code == "reasoning_history_omitted"
                    && issue.feature == Some(Feature::Reasoning)
            }));
        }
    }
}

#[test]
fn runtime_conversion_rejects_an_orphaned_tool_result_before_encoding() {
    let source = ProtocolKind::OpenAiChat;
    let target = ProtocolKind::AnthropicMessages;
    let body = serde_json::json!({
        "model": "source-model",
        "messages": [{
            "role": "tool",
            "tool_call_id": "missing-call",
            "content": "orphaned result"
        }]
    });
    let error = convert_non_stream(
        source,
        target,
        &body,
        &context(source, "source-model", "source-account"),
        &context(target, "target-model", "target-account"),
        &CapabilityProfile::default(),
        ConversionPolicy::Production,
    )
    .expect_err("orphaned tool results must not reach the target adapter");

    assert_eq!(error.code(), "tool_result_orphaned");
    assert!(error.report().is_some_and(|report| {
        report
            .issues
            .iter()
            .any(|issue| issue.code == "tool_result_orphaned")
    }));
}

#[test]
fn runtime_conversion_reports_allowlisted_transcript_repairs() {
    let source = ProtocolKind::OpenAiChat;
    let target = ProtocolKind::AnthropicMessages;
    let body = serde_json::json!({
        "model": "source-model",
        "messages": [
            {"role": "user", "content": "first"},
            {"role": "user", "content": "second"}
        ]
    });
    let converted = convert_non_stream(
        source,
        target,
        &body,
        &context(source, "source-model", "source-account"),
        &context(target, "target-model", "target-account"),
        &CapabilityProfile::default(),
        ConversionPolicy::Production,
    )
    .expect("adjacent user messages have a deterministic Anthropic representation");

    assert_eq!(converted.plan.level(), ConversionLevel::Compatible);
    assert_eq!(converted.plan.repairs().len(), 1);
    assert_eq!(converted.plan.repairs()[0].code, "adjacent_roles_coalesced");
    assert_eq!(converted.plan.report().repairs, converted.plan.repairs());
    assert_eq!(converted.body["messages"].as_array().unwrap().len(), 1);
}
