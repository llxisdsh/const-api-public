mod content;
mod reasoning;
mod request;
mod response;
mod stream;
mod tools;
mod usage;

pub(crate) use content::*;
pub(crate) use reasoning::*;
pub(crate) use request::*;
pub(crate) use response::*;
pub(crate) use stream::*;
pub(crate) use tools::*;
pub(crate) use usage::*;

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IrError {
    code: &'static str,
    path: String,
    message: String,
}

impl IrError {
    pub(crate) fn new(
        code: &'static str,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            path: path.into(),
            message: message.into(),
        }
    }

    pub(crate) const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for IrError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            write!(formatter, "{}: {}", self.code, self.message)
        } else {
            write!(
                formatter,
                "{} at {}: {}",
                self.code, self.path, self.message
            )
        }
    }
}

impl std::error::Error for IrError {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn ordered_request() -> CanonicalRequestV2 {
        let signature = OpaqueArtifact {
            kind: ArtifactKind::AnthropicThinkingSignature,
            payload: json!("signed-secret"),
            affinity: ArtifactAffinity::ExactIssuer {
                provider: "anthropic".to_string(),
                endpoint_fingerprint: "endpoint-sha256".to_string(),
                account_fingerprint: Some("account-sha256".to_string()),
                model: Some("claude-test".to_string()),
            },
            replay: ReplayPolicy::ExactWhenCompatible,
            criticality: ArtifactCriticality::Required,
        };
        let image = ContentBlock::Image(MediaBlock {
            source: MediaSource::InlineBase64 {
                media_type: "image/png".to_string(),
                data: "aGVsbG8=".to_string(),
            },
            detail: Some(MediaDetail::Auto),
            metadata: BlockMetadata::default(),
        });

        let mut request = CanonicalRequestV2::new("claude-test");
        request.instructions.push(Instruction {
            role: InstructionRole::Developer,
            blocks: vec![ContentBlock::Text(TextBlock::new("keep tools ordered"))],
        });
        request.turns.push(Turn {
            id: Some("turn-1".to_string()),
            role: TurnRole::Assistant,
            blocks: vec![
                ContentBlock::Text(TextBlock::new("working")),
                ContentBlock::Reasoning(ReasoningBlock {
                    text: None,
                    summary: vec![TextBlock::new("checked both calls")],
                    artifacts: vec![signature],
                    encrypted: false,
                    metadata: BlockMetadata::default(),
                }),
                ContentBlock::ToolCall(ToolCall::function(
                    "call-a",
                    "read_file",
                    json!({"path":"a.txt"}),
                )),
                ContentBlock::ToolCall(ToolCall::function(
                    "call-b",
                    "read_file",
                    json!({"path":"b.txt"}),
                )),
                ContentBlock::ToolResult(ToolResult {
                    call_id: "call-a".to_string(),
                    name: Some("read_file".to_string()),
                    content: vec![image],
                    status: ItemStatus::Completed,
                    is_error: false,
                    metadata: BlockMetadata::default(),
                }),
                ContentBlock::Refusal(RefusalBlock::new("cannot expose private data")),
            ],
            status: Some(ItemStatus::Completed),
        });
        request
    }

    #[test]
    fn ordered_ir_serialization_preserves_blocks_affinity_usage_and_status() {
        let request = ordered_request();
        request.validate().expect("valid request");

        let value = serde_json::to_value(&request).expect("serialize request");
        let block_types = value["turns"][0]["blocks"]
            .as_array()
            .expect("blocks")
            .iter()
            .map(|block| block["type"].as_str().unwrap_or_default())
            .collect::<Vec<_>>();
        assert_eq!(
            block_types,
            [
                "text",
                "reasoning",
                "tool_call",
                "tool_call",
                "tool_result",
                "refusal"
            ]
        );
        assert_eq!(
            value["turns"][0]["blocks"][1]["artifacts"][0]["affinity"]["scope"],
            "exact_issuer"
        );
        assert_eq!(
            serde_json::to_value(ArtifactKind::OpenAiEncryptedReasoning).unwrap(),
            "openai_encrypted_reasoning"
        );

        let mut usage = Usage::default();
        usage.input_tokens = UsageValue::reported(120).unwrap();
        usage.output_tokens = UsageValue::derived(30).unwrap();
        usage.total_tokens = UsageValue::reported(150).unwrap();
        let response = CanonicalResponseV2 {
            id: Some("resp-1".to_string()),
            model: Some("claude-test".to_string()),
            blocks: request.turns[0].blocks.clone(),
            status: ResponseStatus::Completed,
            finish: FinishDetail::stop(),
            usage,
            error: None,
            extensions: Vec::new(),
        };
        response.validate().expect("valid response");
        let response_value = serde_json::to_value(response).expect("serialize response");
        assert_eq!(response_value["status"], "completed");
        assert_eq!(
            response_value["usage"]["input_tokens"]["source"],
            "reported"
        );
        assert_eq!(
            response_value["usage"]["output_tokens"]["source"],
            "derived"
        );
    }

    #[test]
    fn validation_rejects_empty_names_duplicate_calls_negative_usage_bad_media_and_nan() {
        let empty_name = ToolDefinition::Function(FunctionTool {
            name: "  ".to_string(),
            description: None,
            input_schema: json!({"type":"object"}),
            strict: Some(true),
            cache_policy: None,
            defer_loading: None,
            allowed_callers: Vec::new(),
            input_examples: Vec::new(),
            eager_input_streaming: None,
        });
        assert_eq!(
            empty_name.validate().unwrap_err().code(),
            "invalid_tool_name"
        );

        let mut duplicate = ordered_request();
        duplicate.turns[0]
            .blocks
            .push(ContentBlock::ToolCall(ToolCall::function(
                "call-a",
                "other",
                json!({}),
            )));
        assert_eq!(
            duplicate.validate().unwrap_err().code(),
            "duplicate_tool_call_id"
        );

        let duplicate_response = CanonicalResponseV2 {
            id: None,
            model: Some("model".to_string()),
            blocks: vec![
                ContentBlock::ToolCall(ToolCall::function("same", "first", json!({}))),
                ContentBlock::ToolCall(ToolCall::function("same", "second", json!({}))),
            ],
            status: ResponseStatus::Completed,
            finish: FinishDetail::stop(),
            usage: Usage::default(),
            error: None,
            extensions: Vec::new(),
        };
        assert_eq!(
            duplicate_response.validate().unwrap_err().code(),
            "duplicate_tool_call_id"
        );

        assert_eq!(
            UsageValue::reported(-1).unwrap_err().code(),
            "negative_usage"
        );

        let mut bad_media = CanonicalRequestV2::new("model");
        bad_media.turns.push(Turn {
            id: None,
            role: TurnRole::User,
            blocks: vec![ContentBlock::Image(MediaBlock {
                source: MediaSource::InlineBase64 {
                    media_type: "not a media type".to_string(),
                    data: "AA==".to_string(),
                },
                detail: None,
                metadata: BlockMetadata::default(),
            })],
            status: None,
        });
        assert_eq!(
            bad_media.validate().unwrap_err().code(),
            "invalid_media_type"
        );

        let mut bad_generation = CanonicalRequestV2::new("model");
        bad_generation.generation.temperature = Some(f64::NAN);
        assert_eq!(
            bad_generation.validate().unwrap_err().code(),
            "non_finite_generation_value"
        );
    }

    #[test]
    fn redacted_views_hide_content_artifacts_media_and_tool_arguments() {
        let request = ordered_request();
        let redacted = RedactedRequestView::from(&request);
        let value = serde_json::to_value(redacted).expect("serialize redacted request");
        let blocks = value["turns"][0]["blocks"].as_array().expect("blocks");

        assert_eq!(blocks[0]["kind"], "text");
        assert!(blocks[0].get("text").is_none());
        assert_eq!(blocks[1]["artifacts"][0]["redacted"], true);
        assert!(blocks[1]["artifacts"][0].get("payload").is_none());
        assert_eq!(blocks[2]["argument_bytes"], 16);
        assert_eq!(blocks[2]["argument_sha256"].as_str().unwrap().len(), 64);
        assert_eq!(blocks[4]["content"][0]["source_kind"], "inline_base64");
        assert_eq!(blocks[4]["content"][0]["byte_estimate"], 5);
        assert!(
            serde_json::to_string(&value)
                .unwrap()
                .find("signed-secret")
                .is_none()
        );

        let response = CanonicalResponseV2 {
            id: Some("response-secret".to_string()),
            model: Some("model".to_string()),
            blocks: request.turns[0].blocks.clone(),
            status: ResponseStatus::Completed,
            finish: FinishDetail::stop(),
            usage: Usage::default(),
            error: None,
            extensions: Vec::new(),
        };
        let response_value = serde_json::to_value(RedactedResponseView::from(&response)).unwrap();
        assert!(response_value.get("id").is_none());
        assert_eq!(response_value["status"], "completed");
    }

    #[test]
    fn stream_events_serialize_one_ordered_block_lifecycle() {
        let events = vec![
            CanonicalStreamEvent::ResponseStart(ResponseMeta {
                id: Some("resp-1".to_string()),
                model: Some("model".to_string()),
                status: ResponseStatus::InProgress,
                provider_event_id: Some("event-1".to_string()),
            }),
            CanonicalStreamEvent::BlockStart {
                index: 0,
                block: BlockHeader {
                    tool_kind: None,
                    tool_namespace: None,
                    kind: ContentBlockKind::Text,
                    source_item_id: Some("item-1".to_string()),
                    call_id: None,
                    name: None,
                    artifacts: Vec::new(),
                },
            },
            CanonicalStreamEvent::TextDelta {
                index: 0,
                text: "hello".to_string(),
            },
            CanonicalStreamEvent::BlockDone {
                index: 0,
                block: Some(ContentBlock::Text(TextBlock::new("hello"))),
            },
            CanonicalStreamEvent::ResponseDone(FinishDetail::stop()),
        ];
        let value = serde_json::to_value(events).expect("serialize stream events");
        let kinds = value
            .as_array()
            .unwrap()
            .iter()
            .map(|event| event["event"].as_str().unwrap_or_default())
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            [
                "response_start",
                "block_start",
                "text_delta",
                "block_done",
                "response_done"
            ]
        );
    }
}
