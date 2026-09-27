use serde_json::json;

use crate::protocol::adapters::{AdapterContext, ProviderDialect};
use crate::protocol::capability::CapabilityProfile;
use crate::protocol::conversion::{
    ConversionPolicy, IssuerIdentity, canonicalize_schema, plan_conversion,
};
use crate::protocol::ir::{
    CanonicalRequestV2, ContentBlock, FunctionTool, TextBlock, ToolDefinition, Turn, TurnRole,
};
use crate::protocol::kind::ProtocolKind;
use crate::protocol::{decode_request, encode_request};

fn context(protocol: ProtocolKind) -> AdapterContext {
    AdapterContext {
        dialect: ProviderDialect::Compatible {
            provider: protocol.as_str().to_string(),
        },
        issuer: IssuerIdentity {
            provider: protocol.as_str().to_string(),
            endpoint_fingerprint: "roundtrip".to_string(),
            account_fingerprint: Some("account".to_string()),
            model_family: Some("roundtrip-model".to_string()),
        },
    }
}

#[test]
fn bounded_text_and_function_schema_roundtrips_are_stable_for_every_adapter() {
    for protocol in [
        ProtocolKind::OpenAiResponses,
        ProtocolKind::OpenAiChat,
        ProtocolKind::AnthropicMessages,
        ProtocolKind::GeminiNative,
    ] {
        for text in ["", "hello", "工具调用"] {
            let mut request = CanonicalRequestV2::new("roundtrip-model");
            request.turns.push(Turn {
                id: None,
                role: TurnRole::User,
                blocks: vec![ContentBlock::Text(TextBlock::new(text))],
                status: None,
            });
            request.tools.push(ToolDefinition::Function(FunctionTool {
                name: "lookup".to_string(),
                description: Some("Lookup a value".to_string()),
                input_schema: json!({
                    "type":"object",
                    "properties":{"key":{"type":"string"}},
                    "required":["key"]
                }),
                strict: None,
                cache_policy: None,
                defer_loading: None,
                allowed_callers: Vec::new(),
                input_examples: Vec::new(),
                eager_input_streaming: None,
            }));
            let context = context(protocol);
            let plan = plan_conversion(
                &request,
                protocol,
                protocol,
                &context.issuer,
                &context.issuer,
                &CapabilityProfile::default(),
                ConversionPolicy::Production,
            )
            .unwrap();

            let encoded = encode_request(protocol, &request, &plan, &context).unwrap();
            let decoded = decode_request(protocol, &encoded, &context).unwrap();

            assert_eq!(decoded.model, request.model);
            assert_eq!(decoded.turns.len(), 1);
            assert_eq!(decoded.turns[0].role, TurnRole::User);
            let ContentBlock::Text(decoded_text) = &decoded.turns[0].blocks[0] else {
                panic!("text block");
            };
            assert_eq!(decoded_text.text, text);
            let ToolDefinition::Function(tool) = &decoded.tools[0] else {
                panic!("function tool");
            };
            assert_eq!(tool.name, "lookup");
            assert_eq!(tool.input_schema["required"][0], "key");
        }
    }
}

#[test]
fn bounded_schema_normalization_is_idempotent() {
    for schema in [
        json!(true),
        json!({"type":"object"}),
        json!({
            "description":"",
            "required":["z", "a", "a"],
            "properties":{
                "z":{"type":["string", "null"]},
                "a":{"type":"integer", "title":""}
            },
            "type":"object"
        }),
        json!({
            "type":"array",
            "items":{"oneOf":[{"type":"string"},{"type":"number"}]}
        }),
    ] {
        let once = canonicalize_schema(&schema).expect("first normalization");
        let twice = canonicalize_schema(&once).expect("second normalization");
        assert_eq!(once, twice);
    }
}

#[test]
fn responses_required_tools_encode_as_gemini_any() {
    let source = context(ProtocolKind::OpenAiResponses);
    let target = context(ProtocolKind::GeminiNative);
    let request = decode_request(
        ProtocolKind::OpenAiResponses,
        &json!({
            "model":"gemini-test",
            "input":"Use a tool.",
            "tools":[
                {
                    "type":"function",
                    "name":"lookup",
                    "parameters":{"type":"object","properties":{}}
                },
                {
                    "type":"function",
                    "name":"search",
                    "parameters":{"type":"object","properties":{}}
                }
            ],
            "tool_choice":"required"
        }),
        &source,
    )
    .expect("decode Responses request");
    let plan = plan_conversion(
        &request,
        ProtocolKind::OpenAiResponses,
        ProtocolKind::GeminiNative,
        &source.issuer,
        &target.issuer,
        &CapabilityProfile::default(),
        ConversionPolicy::Production,
    )
    .expect("plan Responses to Gemini conversion");

    let encoded = encode_request(ProtocolKind::GeminiNative, &request, &plan, &target)
        .expect("encode Gemini request");
    assert_eq!(
        encoded.pointer("/toolConfig/functionCallingConfig/mode"),
        Some(&json!("ANY"))
    );
    assert!(
        encoded
            .pointer("/toolConfig/functionCallingConfig/allowedFunctionNames")
            .is_none(),
        "Gemini ANY without a name list requires a call selected from the complete declaration set"
    );
}
