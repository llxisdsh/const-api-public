use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
#[cfg(test)]
use serde_json::json;

use crate::protocol::kind::ProtocolKind;

use super::ArtifactAffinity;
#[cfg(test)]
use super::redacted_blocks;
use super::{ContentBlock, IrError, ItemStatus, ToolChoice, ToolDefinition, validate_blocks};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InstructionRole {
    System,
    Developer,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Instruction {
    pub(crate) role: InstructionRole,
    #[serde(default)]
    pub(crate) blocks: Vec<ContentBlock>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TurnRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Turn {
    pub(crate) id: Option<String>,
    pub(crate) role: TurnRole,
    #[serde(default)]
    pub(crate) blocks: Vec<ContentBlock>,
    pub(crate) status: Option<ItemStatus>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub(crate) struct RequestMetadata {
    pub(crate) source_protocol: Option<ProtocolKind>,
    pub(crate) request_id: Option<String>,
    pub(crate) user_id: Option<String>,
    pub(crate) store: Option<bool>,
    pub(crate) session_id: Option<String>,
    #[serde(default)]
    pub(crate) prompt_cache: PromptCacheConfig,
    // Only present for Responses requests with input additional_tools. The
    // effective tools are in request.tools; native encoding keeps the original
    // top-level list and replays the input declarations at their original turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) responses_tool_placement: Option<ResponsesToolPlacement>,
    #[serde(default)]
    pub(crate) labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub(crate) struct ResponsesToolPlacement {
    pub(crate) top_level_tools: Vec<Value>,
    pub(crate) input_positions: Vec<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PromptCacheMode {
    Implicit,
    Explicit,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub(crate) struct PromptCacheConfig {
    pub(crate) key: Option<String>,
    pub(crate) mode: Option<PromptCacheMode>,
    pub(crate) ttl: Option<String>,
    pub(crate) legacy_retention: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExtensionCriticality {
    Advisory,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ProviderExtension {
    pub(crate) namespace: String,
    pub(crate) name: String,
    pub(crate) value: Value,
    pub(crate) affinity: ArtifactAffinity,
    pub(crate) criticality: ExtensionCriticality,
}

impl ProviderExtension {
    const NESTED_PATH_PREFIX: &'static str = "@nested:";

    pub(crate) fn nested_path(&self) -> Option<&str> {
        self.name.strip_prefix(Self::NESTED_PATH_PREFIX)
    }

    pub(crate) fn nested_name(path: &str) -> String {
        format!("{}{path}", Self::NESTED_PATH_PREFIX)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ResponseFormat {
    #[default]
    Text,
    JsonObject,
    JsonSchema {
        name: String,
        description: Option<String>,
        schema: Value,
        strict: Option<bool>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReasoningMode {
    Disabled,
    Enabled,
    Automatic,
    Adaptive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReasoningEffort {
    // This is the portable cross-protocol vocabulary. Native conversions keep
    // the original wire body, including unknown future strings; only a
    // documented semantic value is admitted here for protocol bridging.
    None,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
    Ultra,
}

impl ReasoningEffort {
    /// Parse the portable reasoning vocabulary used by compatibility APIs.
    ///
    /// Provider-specific automatic modes (for example `auto` and `tiered`)
    /// intentionally stay outside this enum: they describe model selection,
    /// not a concrete reasoning depth.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" => Some(Self::None),
            "minimal" | "extra-low" | "extra_low" => Some(Self::Minimal),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" | "x_high" | "x-high" | "extra-high" | "extra_high" => Some(Self::XHigh),
            "max" => Some(Self::Max),
            "ultra" => Some(Self::Ultra),
            _ => None,
        }
    }

    pub(crate) const fn openai_name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
            Self::Ultra => "ultra",
        }
    }

    pub(crate) const fn anthropic_name(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Minimal | Self::Low => Some("low"),
            Self::Medium => Some("medium"),
            Self::High => Some("high"),
            Self::XHigh => Some("xhigh"),
            Self::Max | Self::Ultra => Some("max"),
        }
    }

    pub(crate) const fn gemini_level(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Minimal => Some("MINIMAL"),
            Self::Low => Some("LOW"),
            Self::Medium => Some("MEDIUM"),
            Self::High | Self::XHigh | Self::Max | Self::Ultra => Some("HIGH"),
        }
    }

    /// Coarse tier used only when a provider publishes low/medium/high model
    /// routes instead of accepting the full request-time effort vocabulary.
    pub(crate) const fn route_tier(self) -> &'static str {
        match self {
            Self::None | Self::Minimal | Self::Low => "low",
            Self::Medium => "medium",
            Self::High | Self::XHigh | Self::Max | Self::Ultra => "high",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReasoningSummaryMode {
    None,
    Auto,
    Concise,
    Detailed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ReasoningConfig {
    pub(crate) mode: ReasoningMode,
    pub(crate) effort: Option<ReasoningEffort>,
    pub(crate) token_budget: Option<u64>,
    pub(crate) summary: ReasoningSummaryMode,
    #[serde(default)]
    pub(crate) provider_extensions: Vec<ProviderExtension>,
}

impl ReasoningConfig {
    /// Whether the caller explicitly requested that reasoning be disabled.
    ///
    /// `ReasoningMode::Disabled` with no effort is also the neutral default for
    /// protocols that did not send reasoning settings. Keeping the explicit
    /// marker here prevents adapters from turning that absence into a provider
    /// command such as Gemini's `thinkingBudget: 0`.
    pub(crate) fn is_explicitly_disabled(&self) -> bool {
        self.effort == Some(ReasoningEffort::None)
    }
}

impl Default for ReasoningConfig {
    fn default() -> Self {
        Self {
            mode: ReasoningMode::Disabled,
            effort: None,
            token_budget: None,
            summary: ReasoningSummaryMode::None,
            provider_extensions: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OutputModality {
    Text,
    Image,
    Audio,
    Video,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OutputVerbosity {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct AudioOutputConfig {
    pub(crate) voice: Option<String>,
    pub(crate) format: Option<String>,
    pub(crate) sample_rate_hz: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub(crate) struct GenerationConfig {
    pub(crate) max_output_tokens: Option<u64>,
    pub(crate) temperature: Option<f64>,
    pub(crate) top_p: Option<f64>,
    pub(crate) top_k: Option<u64>,
    pub(crate) frequency_penalty: Option<f64>,
    pub(crate) presence_penalty: Option<f64>,
    pub(crate) seed: Option<i64>,
    #[serde(default)]
    pub(crate) stop: Vec<String>,
    pub(crate) service_tier: Option<String>,
    pub(crate) latency_mode: Option<String>,
    pub(crate) verbosity: Option<OutputVerbosity>,
    #[serde(default)]
    pub(crate) modalities: Vec<OutputModality>,
    pub(crate) audio: Option<AudioOutputConfig>,
    pub(crate) parallel_tool_calls: Option<bool>,
}

impl GenerationConfig {
    fn validate(&self) -> Result<(), IrError> {
        for (name, value) in [
            ("temperature", self.temperature),
            ("top_p", self.top_p),
            ("frequency_penalty", self.frequency_penalty),
            ("presence_penalty", self.presence_penalty),
        ] {
            if value.is_some_and(|value| !value.is_finite()) {
                return Err(IrError::new(
                    "non_finite_generation_value",
                    format!("generation.{name}"),
                    "generation values must be finite",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct CanonicalRequestV2 {
    pub(crate) model: String,
    #[serde(default)]
    pub(crate) instructions: Vec<Instruction>,
    #[serde(default)]
    pub(crate) turns: Vec<Turn>,
    #[serde(default)]
    pub(crate) tools: Vec<ToolDefinition>,
    pub(crate) tool_choice: ToolChoice,
    pub(crate) response_format: ResponseFormat,
    pub(crate) reasoning: ReasoningConfig,
    pub(crate) generation: GenerationConfig,
    pub(crate) stream: bool,
    pub(crate) metadata: RequestMetadata,
    #[serde(default)]
    pub(crate) extensions: Vec<ProviderExtension>,
}

impl CanonicalRequestV2 {
    pub(crate) fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            instructions: Vec::new(),
            turns: Vec::new(),
            tools: Vec::new(),
            tool_choice: ToolChoice::default(),
            response_format: ResponseFormat::default(),
            reasoning: ReasoningConfig::default(),
            generation: GenerationConfig::default(),
            stream: false,
            metadata: RequestMetadata::default(),
            extensions: Vec::new(),
        }
    }

    pub(crate) fn validate(&self) -> Result<(), IrError> {
        for (index, instruction) in self.instructions.iter().enumerate() {
            validate_blocks(&instruction.blocks, &format!("instructions[{index}]"))?;
        }
        for (index, turn) in self.turns.iter().enumerate() {
            validate_blocks(&turn.blocks, &format!("turns[{index}]"))?;
        }
        for tool in &self.tools {
            tool.validate()?;
        }
        match &self.tool_choice {
            ToolChoice::Named { name } if name.trim().is_empty() => {
                return Err(IrError::new(
                    "invalid_tool_name",
                    "tool_choice.name",
                    "named tool choice is empty",
                ));
            }
            ToolChoice::Allowed { names, .. }
                if names.iter().any(|name| name.trim().is_empty()) =>
            {
                return Err(IrError::new(
                    "invalid_tool_name",
                    "tool_choice.names",
                    "allowed tool name is empty",
                ));
            }
            _ => {}
        }
        self.generation.validate()
    }
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub(crate) struct RedactedRequestView(Value);

#[cfg(test)]
impl From<&CanonicalRequestV2> for RedactedRequestView {
    fn from(request: &CanonicalRequestV2) -> Self {
        Self(json!({
            "model": request.model,
            "instructions": request.instructions.iter().map(|instruction| json!({
                "role": instruction.role,
                "blocks": redacted_blocks(&instruction.blocks),
            })).collect::<Vec<_>>(),
            "turns": request.turns.iter().map(|turn| json!({
                "role": turn.role,
                "status": turn.status,
                "blocks": redacted_blocks(&turn.blocks),
            })).collect::<Vec<_>>(),
            "tools": request.tools.iter().map(redacted_tool).collect::<Vec<_>>(),
            "tool_choice": request.tool_choice,
            "response_format": request.response_format,
            "reasoning": {
                "mode": request.reasoning.mode,
                "effort": request.reasoning.effort,
                "token_budget": request.reasoning.token_budget,
                "summary": request.reasoning.summary,
            },
            "generation": request.generation,
            "stream": request.stream,
            "prompt_cache": {
                "has_key": request.metadata.prompt_cache.key.is_some(),
                "mode": request.metadata.prompt_cache.mode,
                "ttl": request.metadata.prompt_cache.ttl,
                "legacy_retention": request.metadata.prompt_cache.legacy_retention,
            },
            "extensions": request.extensions.iter().map(|extension| json!({
                "namespace": extension.namespace,
                "name": extension.name,
                "affinity": extension.affinity,
                "criticality": extension.criticality,
                "redacted": true,
            })).collect::<Vec<_>>(),
        }))
    }
}

#[cfg(test)]
fn redacted_tool(tool: &ToolDefinition) -> Value {
    match tool {
        ToolDefinition::Function(value) => json!({
            "type": "function",
            "name": value.name,
            "description_chars": value.description.as_ref().map(|text| text.chars().count()).unwrap_or(0),
            "schema_bytes": serde_json::to_vec(&value.input_schema).map(|bytes| bytes.len()).unwrap_or(0),
            "strict": value.strict,
            "defer_loading": value.defer_loading,
            "allowed_callers": value.allowed_callers,
            "input_example_count": value.input_examples.len(),
            "eager_input_streaming": value.eager_input_streaming,
        }),
        ToolDefinition::Custom(value) => json!({
            "type": "custom",
            "name": value.name,
            "grammar_bytes": serde_json::to_vec(&value.grammar).map(|bytes| bytes.len()).unwrap_or(0),
        }),
        ToolDefinition::Namespace(value) => json!({
            "type": "namespace",
            "namespace": value.namespace,
            "tool_count": value.tools.len(),
        }),
        ToolDefinition::Hosted(value) => json!({
            "type": "hosted",
            "kind": value.kind,
            "provider": value.provider,
            "name": value.name,
            "config_redacted": true,
        }),
    }
}
