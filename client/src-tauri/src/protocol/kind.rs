use super::error::ProtocolError;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum ProtocolKind {
    #[serde(rename = "openai_responses")]
    OpenAiResponses,
    #[serde(rename = "openai_chat")]
    OpenAiChat,
    #[serde(rename = "anthropic_messages")]
    AnthropicMessages,
    #[serde(rename = "gemini_native")]
    GeminiNative,
}

impl ProtocolKind {
    pub(crate) fn parse(value: &str) -> Result<Self, ProtocolError> {
        match value.trim() {
            "" => Err(ProtocolError::invalid(value)),
            "responses" | "openai_responses" => Ok(Self::OpenAiResponses),
            "chat" | "openai" | "openai_chat" | "gemini_openai" => Ok(Self::OpenAiChat),
            "anthropic" | "anthropic_messages" => Ok(Self::AnthropicMessages),
            "gemini" | "gemini_native" => Ok(Self::GeminiNative),
            other => Err(ProtocolError::unsupported(other)),
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiResponses => "openai_responses",
            Self::OpenAiChat => "openai_chat",
            Self::AnthropicMessages => "anthropic_messages",
            Self::GeminiNative => "gemini_native",
        }
    }

    pub(crate) fn path(self, model: &str, stream: bool) -> String {
        match self {
            Self::OpenAiResponses => "/v1/responses".to_string(),
            Self::OpenAiChat => "/v1/chat/completions".to_string(),
            Self::AnthropicMessages => "/v1/messages".to_string(),
            Self::GeminiNative => {
                let model = model.trim().replace('/', "%2F");
                let action = if stream {
                    "streamGenerateContent?alt=sse"
                } else {
                    "generateContent"
                };
                format!("/v1beta/models/{model}:{action}")
            }
        }
    }
}

impl TryFrom<&str> for ProtocolKind {
    type Error = ProtocolError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

#[cfg(test)]
pub(crate) fn validate_channel_api_format(
    channel_kind: &str,
    api_format: &str,
) -> Result<Option<ProtocolKind>, ProtocolError> {
    if channel_kind.trim() == "subscription_adapter" {
        return Ok(None);
    }
    ProtocolKind::parse(api_format).map(Some)
}

impl fmt::Display for ProtocolKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_the_four_core_protocols_and_ui_aliases() {
        for (raw, expected) in [
            ("openai_responses", ProtocolKind::OpenAiResponses),
            ("responses", ProtocolKind::OpenAiResponses),
            ("openai_chat", ProtocolKind::OpenAiChat),
            ("chat", ProtocolKind::OpenAiChat),
            ("openai", ProtocolKind::OpenAiChat),
            ("gemini_openai", ProtocolKind::OpenAiChat),
            ("anthropic_messages", ProtocolKind::AnthropicMessages),
            ("anthropic", ProtocolKind::AnthropicMessages),
            ("gemini_native", ProtocolKind::GeminiNative),
            ("gemini", ProtocolKind::GeminiNative),
        ] {
            assert_eq!(ProtocolKind::parse(raw).unwrap(), expected, "{raw}");
        }
    }

    #[test]
    fn rejects_empty_unknown_and_converse_protocols() {
        let empty = ProtocolKind::parse("").expect_err("empty protocol");
        assert_eq!(empty.code(), "invalid_protocol");

        for raw in [
            "future_protocol",
            "bedrock",
            "bedrock_converse",
            "aws_bedrock",
        ] {
            let error = ProtocolKind::parse(raw).expect_err(raw);
            assert_eq!(error.code(), "unsupported_protocol", "{raw}");
        }
    }

    #[test]
    fn removed_converse_error_explains_mantle_reconfiguration() {
        let error = ProtocolKind::parse("bedrock_converse").unwrap_err();
        assert_eq!(
            error.action_message(),
            "该渠道使用已移除的 Bedrock Converse，请通过 Amazon Bedrock Mantle 重新配置。"
        );
    }

    #[test]
    fn channel_validation_rejects_converse_but_accepts_mantle_and_subscriptions() {
        let removed = validate_channel_api_format("aws_bedrock", "bedrock_converse")
            .expect_err("Converse channel must be invalid");
        assert_eq!(
            removed.action_message(),
            "该渠道使用已移除的 Bedrock Converse，请通过 Amazon Bedrock Mantle 重新配置。"
        );
        assert_eq!(
            validate_channel_api_format("aws_bedrock", "openai_chat").unwrap(),
            Some(ProtocolKind::OpenAiChat)
        );
        assert_eq!(
            validate_channel_api_format("subscription_adapter", "subscription_skeleton").unwrap(),
            None
        );
    }

    #[test]
    fn builds_native_paths_without_protocol_guessing() {
        assert_eq!(
            ProtocolKind::OpenAiResponses.path("gpt-5.5", false),
            "/v1/responses"
        );
        assert_eq!(
            ProtocolKind::OpenAiChat.path("gpt-5.5", true),
            "/v1/chat/completions"
        );
        assert_eq!(
            ProtocolKind::AnthropicMessages.path("claude-sonnet", false),
            "/v1/messages"
        );
        assert_eq!(
            ProtocolKind::GeminiNative.path("models/gemini-3", false),
            "/v1beta/models/models%2Fgemini-3:generateContent"
        );
        assert_eq!(
            ProtocolKind::GeminiNative.path("gemini-3", true),
            "/v1beta/models/gemini-3:streamGenerateContent?alt=sse"
        );
        assert_eq!(
            ProtocolKind::GeminiNative.path("", false),
            "/v1beta/models/:generateContent"
        );
    }

    #[test]
    fn runtime_protocol_parsing_rejects_removed_converse_without_chat_fallback() {
        for removed in ["bedrock", "bedrock_converse", "aws_bedrock"] {
            assert!(ProtocolKind::parse(removed).is_err(), "{removed} protocol");
            assert!(
                crate::normalize_supported_protocols(Vec::new(), removed).is_empty(),
                "{removed} supported protocols"
            );
            assert!(
                crate::proxy::protocol_from_api_format(removed).is_empty(),
                "{removed} runtime protocol"
            );
        }

        assert!(ProtocolKind::parse("").is_err());
        assert!(crate::proxy::protocol_from_path("/bedrock/model/x/converse").is_empty());
        assert_eq!(
            crate::normalize_supported_protocols(Vec::new(), "openai_chat"),
            vec!["openai_chat"]
        );
    }
}
