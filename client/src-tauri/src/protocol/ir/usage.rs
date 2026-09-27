use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

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
    use super::output_tokens_with_reasoning;

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
}
