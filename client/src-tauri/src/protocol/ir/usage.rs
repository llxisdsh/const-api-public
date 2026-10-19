use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::protocol::kind::ProtocolKind;

use super::IrError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UsageProvenance {
    Reported,
    Derived,
    #[default]
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub(crate) struct UsageValue {
    pub(crate) value: Option<u64>,
    pub(crate) source: UsageProvenance,
}

impl UsageValue {
    pub(crate) fn reported(value: i64) -> Result<Self, IrError> {
        Self::from_i64(value, UsageProvenance::Reported)
    }

    #[cfg(test)]
    pub(crate) fn derived(value: i64) -> Result<Self, IrError> {
        Self::from_i64(value, UsageProvenance::Derived)
    }

    fn from_i64(value: i64, source: UsageProvenance) -> Result<Self, IrError> {
        let value = u64::try_from(value).map_err(|_| {
            IrError::new("negative_usage", "usage", "usage values cannot be negative")
        })?;
        Ok(Self {
            value: Some(value),
            source,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub(crate) struct Usage {
    pub(crate) input_tokens: UsageValue,
    // Inclusive of reasoning; provider adapters translate their wire accounting.
    pub(crate) output_tokens: UsageValue,
    pub(crate) total_tokens: UsageValue,
    pub(crate) cache_read_tokens: UsageValue,
    pub(crate) cache_write_tokens: UsageValue,
    pub(crate) cache_creation_tokens: UsageValue,
    pub(crate) cache_expiry_5m_tokens: UsageValue,
    pub(crate) cache_expiry_1h_tokens: UsageValue,
    pub(crate) reasoning_tokens: UsageValue,
    pub(crate) text_input_units: UsageValue,
    pub(crate) text_output_units: UsageValue,
    pub(crate) image_input_units: UsageValue,
    pub(crate) image_output_units: UsageValue,
    pub(crate) audio_input_units: UsageValue,
    pub(crate) audio_output_units: UsageValue,
    pub(crate) video_input_units: UsageValue,
    pub(crate) video_output_units: UsageValue,
    pub(crate) accepted_prediction_tokens: UsageValue,
    pub(crate) rejected_prediction_tokens: UsageValue,
    pub(crate) server_tool_calls: UsageValue,
    #[serde(default)]
    pub(crate) provider_billable_units: BTreeMap<String, UsageValue>,
}

// Some OpenAI-compatible providers report reasoning separately. Only fold it
// into output when the reported total proves that it has not been included.
pub(crate) fn output_tokens_with_reasoning(
    input: Option<u64>,
    output: Option<u64>,
    total: Option<u64>,
    reasoning: Option<u64>,
) -> Option<u64> {
    let output = output?;
    let (Some(input), Some(total), Some(reasoning)) = (input, total, reasoning) else {
        return Some(output);
    };
    if reasoning > 0
        && input
            .checked_add(output)
            .and_then(|tokens| tokens.checked_add(reasoning))
            == Some(total)
    {
        return output.checked_add(reasoning);
    }
    Some(output)
}

impl Usage {
    /// Translate the input-token convention carried by the canonical usage into
    /// the target protocol's wire convention. OpenAI-compatible and Gemini
    /// protocols include cache tokens in their total input count, while
    /// Anthropic reports uncached input separately from cache reads/writes.
    pub(crate) fn translated_for_protocol(
        &self,
        source: ProtocolKind,
        target: ProtocolKind,
    ) -> Self {
        if source == target {
            return self.clone();
        }

        let source_includes_cache = source != ProtocolKind::AnthropicMessages;
        let target_includes_cache = target != ProtocolKind::AnthropicMessages;
        if source_includes_cache == target_includes_cache {
            return self.clone();
        }

        let cache_tokens = self
            .cache_read_tokens
            .value
            .unwrap_or_default()
            .saturating_add(
                self.cache_write_tokens
                    .value
                    .or(self.cache_creation_tokens.value)
                    .unwrap_or_default(),
            );
        if cache_tokens == 0 {
            return self.clone();
        }

        let mut translated = self.clone();
        let adjust = |value: &UsageValue| {
            value.value.map(|value| {
                if target_includes_cache {
                    value.saturating_add(cache_tokens)
                } else {
                    value.saturating_sub(cache_tokens)
                }
            })
        };
        if let Some(value) = adjust(&self.input_tokens) {
            translated.input_tokens = UsageValue {
                value: Some(value),
                source: UsageProvenance::Derived,
            };
        }
        if let Some(value) = adjust(&self.total_tokens) {
            translated.total_tokens = UsageValue {
                value: Some(value),
                source: UsageProvenance::Derived,
            };
        }
        translated
    }

    pub(crate) fn with_separate_reasoning_output(mut self) -> Self {
        if let (Some(output), Some(reasoning)) =
            (self.output_tokens.value, self.reasoning_tokens.value)
        {
            self.output_tokens.value = Some(output.saturating_add(reasoning));
            self.output_tokens.source = UsageProvenance::Derived;
        }
        self
    }

    pub(crate) fn non_reasoning_output(&self) -> UsageValue {
        UsageValue {
            value: self
                .output_tokens
                .value
                .map(|output| output.saturating_sub(self.reasoning_tokens.value.unwrap_or(0))),
            source: self.output_tokens.source,
        }
    }

    pub(crate) fn with_inclusive_reasoning_output(mut self) -> Self {
        let output = output_tokens_with_reasoning(
            self.input_tokens.value,
            self.output_tokens.value,
            self.total_tokens.value,
            self.reasoning_tokens.value,
        );
        if output != self.output_tokens.value {
            self.output_tokens = UsageValue {
                value: output,
                source: UsageProvenance::Derived,
            };
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{Usage, UsageProvenance, UsageValue, output_tokens_with_reasoning};
    use crate::protocol::kind::ProtocolKind;

    #[test]
    fn reasoning_totals_require_complete_consistent_evidence() {
        assert_eq!(
            output_tokens_with_reasoning(Some(32), Some(9), Some(135), Some(94)),
            Some(103)
        );
        assert_eq!(
            output_tokens_with_reasoning(Some(32), Some(103), Some(135), Some(94)),
            Some(103)
        );
        assert_eq!(
            output_tokens_with_reasoning(Some(32), Some(9), None, Some(94)),
            Some(9)
        );
        assert_eq!(
            output_tokens_with_reasoning(Some(32), Some(9), Some(135), None),
            Some(9)
        );
        assert_eq!(
            output_tokens_with_reasoning(Some(32), None, Some(135), Some(94)),
            None
        );
        assert_eq!(
            output_tokens_with_reasoning(Some(0), Some(0), Some(94), Some(94)),
            Some(94)
        );
        assert_eq!(
            output_tokens_with_reasoning(Some(u64::MAX), Some(9), Some(8), Some(1)),
            Some(9)
        );
    }

    #[test]
    fn cache_input_semantics_are_translated_between_anthropic_and_public_protocols() {
        let usage = Usage {
            input_tokens: UsageValue::reported(100).unwrap(),
            output_tokens: UsageValue::reported(20).unwrap(),
            total_tokens: UsageValue::reported(120).unwrap(),
            cache_read_tokens: UsageValue::reported(30).unwrap(),
            cache_write_tokens: UsageValue::reported(10).unwrap(),
            ..Usage::default()
        };

        let anthropic = usage.translated_for_protocol(
            ProtocolKind::OpenAiResponses,
            ProtocolKind::AnthropicMessages,
        );
        assert_eq!(anthropic.input_tokens.value, Some(60));
        assert_eq!(anthropic.total_tokens.value, Some(80));
        assert_eq!(anthropic.input_tokens.source, UsageProvenance::Derived);

        let openai = anthropic.translated_for_protocol(
            ProtocolKind::AnthropicMessages,
            ProtocolKind::OpenAiResponses,
        );
        assert_eq!(openai.input_tokens.value, Some(100));
        assert_eq!(openai.total_tokens.value, Some(120));
        assert_eq!(openai.total_tokens.source, UsageProvenance::Derived);
    }
}
