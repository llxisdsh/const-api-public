//! Request sizing for routing, never billable usage or a tokenizer guarantee.
//! Like Codex's local budget estimate, text uses UTF-8 bytes / 4. Media and
//! opaque history remain explicitly incomplete instead of counting ciphertext.
use serde_json::Value;

#[derive(Debug, Default)]
pub(crate) struct Requirement {
    pub(crate) input_tokens: u64,
    pub(crate) output_tokens: Option<u64>,
    pub(crate) incomplete: bool,
    pub(crate) previous_response_id: Option<String>,
    current_input: Option<Value>,
}

impl Requirement {
    pub(crate) fn for_request(path: &str, body: &[u8]) -> Self {
        let path = path.split('?').next().unwrap_or(path);
        use crate::surface::ApiOperation as Op;
        if !crate::surface::resolve_api_route("POST", path, false).is_some_and(|route| {
            matches!(
                route.operation,
                Op::ChatCompletions
                    | Op::Responses
                    | Op::Messages
                    | Op::GenerateContent
                    | Op::StreamGenerateContent
            )
        }) {
            return Self::default();
        }
        Self::from_body(body)
    }

    pub(crate) fn from_body(body: &[u8]) -> Self {
        let Ok(mut value) = serde_json::from_slice::<Value>(body) else {
            return Self::default();
        };
        let mut requirement = Self::from_value(&value);
        if requirement.previous_response_id.is_some() {
            requirement.current_input = value.get_mut("input").map(Value::take);
        }
        requirement
    }

    pub(crate) fn from_value(value: &Value) -> Self {
        let mut stats = TextSize::default();
        for key in [
            "messages",
            "input",
            "instructions",
            "system",
            "contents",
            "systemInstruction",
            "system_instruction",
            "tools",
            "functions",
        ] {
            if let Some(value) = value.get(key) {
                stats.add(value);
            }
        }
        let previous_response_id = value
            .get("previous_response_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_owned);
        let incomplete = stats.incomplete
            || previous_response_id.is_some()
            || value
                .get("conversation")
                .is_some_and(|value| !value.is_null())
            || value.get("cachedContent").is_some()
            || value.get("cached_content").is_some();
        let output_tokens = if value.get("generate").and_then(Value::as_bool) == Some(false) {
            Some(0)
        } else {
            // The current Chat field takes precedence over deprecated max_tokens.
            [
                "/max_completion_tokens",
                "/max_output_tokens",
                "/max_tokens",
                "/generationConfig/maxOutputTokens",
                "/generation_config/max_output_tokens",
            ]
            .into_iter()
            .find_map(|key| {
                value
                    .pointer(key)
                    .and_then(Value::as_u64)
                    .filter(|n| *n > 0)
            })
        };
        Self {
            input_tokens: stats.tokens(),
            output_tokens,
            incomplete,
            previous_response_id,
            current_input: None,
        }
    }

    pub(crate) fn fits(&self, limits: crate::tool_model_metadata::ModelTokenLimits) -> bool {
        if self.input_tokens == 0 && self.output_tokens.is_none() {
            return true;
        }
        if limits
            .input_tokens
            .is_some_and(|limit| self.input_tokens > limit)
        {
            return false;
        }
        if self
            .output_tokens
            .zip(limits.output_tokens)
            .is_some_and(|(requested, limit)| requested > limit)
        {
            return false;
        }
        // An omitted output limit is not a request for the model's maximum.
        // Reserving that maximum rejects even tiny requests on small gateways.
        let reserve = self.output_tokens.unwrap_or(0);
        // An explicit input-only limit is not evidence of a joint window.
        !limits
            .max_context_tokens
            .or_else(|| {
                limits
                    .input_tokens
                    .is_none()
                    .then_some(limits.context_tokens)
                    .flatten()
            })
            .is_some_and(|limit| self.input_tokens.saturating_add(reserve) > limit)
    }

    pub(crate) fn fits_channel(&self, channel: &crate::model::ChannelConfig, model: &str) -> bool {
        let history = self
            .previous_response_id
            .as_deref()
            .filter(|_| {
                channel.source_driver() == crate::source_driver::SourceDriverId::OpenAiSubscription
            })
            .and_then(|parent| {
                crate::supplier::codex_continuation_capacity(
                    &crate::supplier_from_channel(channel),
                    parent,
                    self.current_input.as_ref(),
                )
            });
        let measured = Self {
            input_tokens: self
                .input_tokens
                .saturating_add(history.map_or(0, |(tokens, _)| tokens)),
            output_tokens: self.output_tokens,
            ..Self::default()
        };
        measured.fits(crate::tool_model_metadata::channel_model_token_limits(
            channel, model,
        ))
    }
}

#[derive(Default)]
struct TextSize {
    bytes: u64,
    incomplete: bool,
}

impl TextSize {
    fn add(&mut self, value: &Value) {
        match value {
            Value::String(text) => self.bytes = self.bytes.saturating_add(text.len() as u64),
            Value::Array(items) => {
                for item in items {
                    self.add(item);
                }
                self.bytes = self.bytes.saturating_add(items.len() as u64 * 16);
            }
            Value::Object(object) => {
                let kind = object.get("type").and_then(Value::as_str);
                let opaque_document = kind == Some("document")
                    && !object
                        .get("source")
                        .and_then(|source| source.get("type"))
                        .and_then(Value::as_str)
                        .is_some_and(|source| matches!(source, "text" | "content"));
                if opaque_document
                    || kind.is_some_and(|kind| {
                        matches!(
                            kind,
                            "image"
                                | "input_image"
                                | "image_url"
                                | "input_audio"
                                | "audio"
                                | "input_file"
                                | "file"
                                | "video"
                                | "redacted_thinking"
                                | "item_reference"
                        )
                    })
                    || object.contains_key("inlineData")
                    || object.contains_key("inline_data")
                    || object.contains_key("fileData")
                    || object.contains_key("file_data")
                {
                    self.incomplete = true;
                    return;
                }
                for (key, item) in object {
                    if (kind == Some("thinking") && key == "signature")
                        || matches!(
                            key.as_str(),
                            "encrypted_content"
                                | "encryptedContent"
                                | "thoughtSignature"
                                | "thought_signature"
                        )
                    {
                        self.incomplete = true;
                        continue;
                    }
                    self.bytes = self.bytes.saturating_add(key.len() as u64 + 4);
                    self.add(item);
                }
            }
            _ => self.bytes = self.bytes.saturating_add(8),
        }
    }

    fn tokens(&self) -> u64 {
        // Reserve a small margin for protocol wrappers, without presenting
        // an approximate count as the provider's actual usage.
        self.bytes.div_ceil(4).saturating_mul(11).div_ceil(10)
    }
}

pub(crate) fn history_tokens<'a>(history: impl IntoIterator<Item = &'a Value>) -> (u64, bool) {
    let mut size = TextSize::default();
    for item in history {
        size.add(item);
        size.bytes = size.bytes.saturating_add(16);
    }
    (size.tokens(), size.incomplete)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_model_metadata::ModelTokenLimits;

    #[test]
    fn context_capacity_only_applies_to_generation_including_queries() {
        let body = br#"{"input":"hello","max_tokens":4000}"#;
        for path in [
            "/v1/chat/completions",
            "/v1/responses?beta=test",
            "/anthropic/v1/messages?beta=true",
            "/gemini/v1beta/models/model:streamGenerateContent?alt=sse",
        ] {
            assert!(
                Requirement::for_request(path, body).input_tokens > 0,
                "{path}"
            );
        }
        for path in [
            "/v1/embeddings",
            "/v1/responses/compact",
            "/v1/responses/input_tokens",
            "/anthropic/v1/messages/count_tokens",
            "/gemini/v1beta/models/model:countTokens",
        ] {
            assert_eq!(
                Requirement::for_request(path, body).input_tokens,
                0,
                "{path}"
            );
        }
    }

    #[test]
    fn context_capacity_counts_tools_and_output_without_counting_media_bytes() {
        let request = Requirement::from_value(
            &serde_json::json!({"messages":[{"role":"user", "content":"x".repeat(40000)}], "tools":[{"type":"function", "function":{"description":"x".repeat(40000)}}], "max_tokens":4000}),
        );
        assert!(request.input_tokens > 20000);
        assert!(!request.fits(ModelTokenLimits {
            context_tokens: Some(20000),
            ..Default::default()
        }));
        let image = Requirement::from_value(
            &serde_json::json!({"input":[{"type":"input_image", "image_url":format!("data:image/png;base64,{}", "x".repeat(400000))}]}),
        );
        assert!(image.incomplete);
        assert!(image.input_tokens < 100);
    }

    #[test]
    fn context_capacity_keeps_document_text_but_skips_binary_and_signed_state() {
        for block in [
            serde_json::json!({"type":"document","source":{"type":"base64","media_type":"application/pdf","data":"x".repeat(400000)}}),
            serde_json::json!({"type":"document","source":{"type":"file","file_id":"file-fixture"}}),
            serde_json::json!({"type":"redacted_thinking","data":"x".repeat(400000)}),
            serde_json::json!({"type":"thinking","thinking":"visible","signature":"x".repeat(400000)}),
            serde_json::json!({"type":"item_reference","id":"msg-fixture"}),
        ] {
            let req = Requirement::from_value(
                &serde_json::json!({"messages":[{"role":"user","content":[block]}]}),
            );
            assert!(req.incomplete);
            assert!(req.input_tokens < 100);
        }
        for source in [
            serde_json::json!({"type":"text","data":"x".repeat(40000)}),
            serde_json::json!({"type":"content","content":[{"type":"text","text":"x".repeat(40000)}]}),
        ] {
            let req = Requirement::from_value(
                &serde_json::json!({"messages":[{"role":"user","content":[{"type":"document","source":source}]}]}),
            );
            assert!(!req.incomplete);
            assert!(req.input_tokens > 10000);
        }
    }

    #[test]
    fn context_capacity_preserves_input_and_joint_constraints() {
        let input_only = ModelTokenLimits {
            context_tokens: Some(1000),
            input_tokens: Some(1000),
            output_tokens: Some(100),
            ..Default::default()
        };
        let request = Requirement {
            input_tokens: 1000,
            output_tokens: Some(100),
            ..Default::default()
        };
        assert!(request.fits(input_only));
        assert!(!request.fits(ModelTokenLimits {
            max_context_tokens: Some(1000),
            ..input_only
        }));
        let limits = ModelTokenLimits {
            max_context_tokens: Some(1050000),
            input_tokens: Some(922000),
            output_tokens: Some(128000),
            ..Default::default()
        };
        let mut requirement = Requirement {
            input_tokens: 922000,
            output_tokens: Some(128000),
            ..Default::default()
        };
        assert!(requirement.fits(limits));
        requirement.input_tokens += 1;
        assert!(!requirement.fits(limits));
        requirement.input_tokens = 900000;
        requirement.output_tokens = Some(200000);
        assert!(!requirement.fits(limits));
    }
}
