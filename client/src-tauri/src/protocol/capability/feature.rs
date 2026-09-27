use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::protocol::ir::{
    CachePolicy, CanonicalRequestV2, ContentBlock, FunctionTool, HostedToolKind,
    NamespaceToolDefinition, ReasoningMode, ResponseFormat, ToolChoice, ToolDefinition, ToolKind,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Feature {
    Stream,
    DeveloperInstruction,
    JsonObject,
    JsonSchema,
    StrictSchema,
    Tools,
    ParallelTools,
    ToolChoiceNone,
    ToolChoiceRequired,
    ToolChoiceNamed,
    ToolChoiceAllowed,
    Reasoning,
    ReasoningSummary,
    EncryptedReasoning,
    AnthropicSignatureReplay,
    GeminiThoughtSignatureReplay,
    CacheControl,
    PromptCacheOptions,
    PromptCacheBreakpoint,
    ImageInput,
    AudioInput,
    AudioOutput,
    OutputVerbosity,
    ResponseStorage,
    UserIdentity,
    VideoInput,
    FileInput,
    HostedWebSearch,
    HostedFileSearch,
    HostedCodeExecution,
    HostedComputerUse,
    HostedMcp,
    HostedUrlContext,
    HostedProviderTool,
    CustomTools,
    NamespaceTools,
    ToolDeferLoading,
    ToolAllowedCallers,
    ToolInputExamples,
    ToolEagerInputStreaming,
    ProviderExtension,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FeatureUse {
    pub(crate) feature: Feature,
    pub(crate) path: String,
}

pub(crate) fn extract_required_features(request: &CanonicalRequestV2) -> Vec<FeatureUse> {
    let mut features = BTreeMap::<Feature, String>::new();
    let mut insert = |feature, path: String| {
        features.entry(feature).or_insert(path);
    };

    if request.stream {
        insert(Feature::Stream, "stream".to_string());
    }
    match &request.response_format {
        ResponseFormat::Text => {}
        ResponseFormat::JsonObject => insert(Feature::JsonObject, "response_format".to_string()),
        ResponseFormat::JsonSchema { strict, .. } => {
            insert(Feature::JsonSchema, "response_format".to_string());
            if *strict == Some(true) {
                insert(Feature::StrictSchema, "response_format.strict".to_string());
            }
        }
    }
    if !request.tools.is_empty() {
        insert(Feature::Tools, "tools".to_string());
    }
    for (index, tool) in request.tools.iter().enumerate() {
        match tool {
            ToolDefinition::Function(tool) => {
                extract_function_features(tool, &format!("tools[{index}]"), &mut insert);
            }
            ToolDefinition::Custom(_) => insert(Feature::CustomTools, format!("tools[{index}]")),
            ToolDefinition::Namespace(namespace) => {
                insert(Feature::NamespaceTools, format!("tools[{index}]"));
                for (child_index, child) in namespace.tools.iter().enumerate() {
                    let path = format!("tools[{index}].tools[{child_index}]");
                    match child {
                        NamespaceToolDefinition::Function(tool) => {
                            extract_function_features(tool, &path, &mut insert)
                        }
                        NamespaceToolDefinition::Custom(_) => insert(Feature::CustomTools, path),
                    }
                }
            }
            ToolDefinition::Hosted(tool) => insert(
                match tool.kind {
                    HostedToolKind::WebSearch => Feature::HostedWebSearch,
                    HostedToolKind::FileSearch => Feature::HostedFileSearch,
                    HostedToolKind::CodeExecution => Feature::HostedCodeExecution,
                    HostedToolKind::ComputerUse => Feature::HostedComputerUse,
                    HostedToolKind::Mcp => Feature::HostedMcp,
                    HostedToolKind::UrlContext => Feature::HostedUrlContext,
                    HostedToolKind::ProviderSpecific => Feature::HostedProviderTool,
                },
                format!("tools[{index}]"),
            ),
        }
    }
    match &request.tool_choice {
        ToolChoice::Auto => {}
        ToolChoice::None => insert(Feature::ToolChoiceNone, "tool_choice".to_string()),
        ToolChoice::Required => insert(Feature::ToolChoiceRequired, "tool_choice".to_string()),
        ToolChoice::Named { .. } => insert(Feature::ToolChoiceNamed, "tool_choice".to_string()),
        ToolChoice::Allowed { .. } => insert(Feature::ToolChoiceAllowed, "tool_choice".to_string()),
    }
    if request.generation.parallel_tool_calls == Some(true) {
        insert(
            Feature::ParallelTools,
            "generation.parallel_tool_calls".to_string(),
        );
    }
    if !matches!(request.reasoning.mode, ReasoningMode::Disabled) {
        insert(Feature::Reasoning, "reasoning".to_string());
    }
    if !matches!(
        request.reasoning.summary,
        crate::protocol::ir::ReasoningSummaryMode::None
    ) {
        insert(Feature::ReasoningSummary, "reasoning.summary".to_string());
    }
    if request.generation.audio.is_some()
        || request
            .generation
            .modalities
            .contains(&crate::protocol::ir::OutputModality::Audio)
    {
        insert(Feature::AudioOutput, "generation.audio".to_string());
    }
    if request.generation.verbosity.is_some() {
        insert(Feature::OutputVerbosity, "generation.verbosity".to_string());
    }
    if request.metadata.store.is_some() {
        insert(Feature::ResponseStorage, "metadata.store".to_string());
    }
    if request.metadata.user_id.is_some() {
        insert(Feature::UserIdentity, "metadata.user_id".to_string());
    }
    if request.metadata.prompt_cache.key.is_some()
        || request.metadata.prompt_cache.mode.is_some()
        || request.metadata.prompt_cache.ttl.is_some()
        || request.metadata.prompt_cache.legacy_retention.is_some()
    {
        insert(
            Feature::PromptCacheOptions,
            "metadata.prompt_cache".to_string(),
        );
    }
    for (index, instruction) in request.instructions.iter().enumerate() {
        if instruction.role == crate::protocol::ir::InstructionRole::Developer {
            insert(
                Feature::DeveloperInstruction,
                format!("instructions[{index}].role"),
            );
        }
        extract_block_features(
            &instruction.blocks,
            &format!("instructions[{index}]"),
            &mut insert,
        );
    }
    for (index, turn) in request.turns.iter().enumerate() {
        extract_block_features(&turn.blocks, &format!("turns[{index}]"), &mut insert);
    }
    features
        .into_iter()
        .map(|(feature, path)| FeatureUse { feature, path })
        .collect()
}

fn extract_function_features(
    tool: &FunctionTool,
    path: &str,
    insert: &mut impl FnMut(Feature, String),
) {
    if tool.strict == Some(true) {
        insert(Feature::StrictSchema, format!("{path}.strict"));
    }
    if tool.cache_policy.is_some() {
        insert(Feature::CacheControl, format!("{path}.cache_policy"));
    }
    if tool.defer_loading.is_some() {
        insert(Feature::ToolDeferLoading, format!("{path}.defer_loading"));
    }
    if !tool.allowed_callers.is_empty() {
        insert(
            Feature::ToolAllowedCallers,
            format!("{path}.allowed_callers"),
        );
    }
    if !tool.input_examples.is_empty() {
        insert(Feature::ToolInputExamples, format!("{path}.input_examples"));
    }
    if tool.eager_input_streaming.is_some() {
        insert(
            Feature::ToolEagerInputStreaming,
            format!("{path}.eager_input_streaming"),
        );
    }
}

fn extract_block_features(
    blocks: &[ContentBlock],
    path: &str,
    insert: &mut impl FnMut(Feature, String),
) {
    for (index, block) in blocks.iter().enumerate() {
        let block_path = format!("{path}.blocks[{index}]");
        match block {
            ContentBlock::Text(value) => {
                extract_cache(&value.metadata.cache_policy, &block_path, insert)
            }
            ContentBlock::Reasoning(value) => {
                insert(Feature::Reasoning, block_path.clone());
                if !value.summary.is_empty() {
                    insert(Feature::ReasoningSummary, block_path.clone());
                }
                for artifact in &value.artifacts {
                    if let Some(feature) = artifact_feature(&artifact.kind) {
                        insert(feature, block_path.clone());
                    }
                }
                extract_cache(&value.metadata.cache_policy, &block_path, insert);
            }
            ContentBlock::Refusal(value) => {
                extract_cache(&value.metadata.cache_policy, &block_path, insert)
            }
            ContentBlock::Image(value) => {
                insert(Feature::ImageInput, block_path.clone());
                extract_cache(&value.metadata.cache_policy, &block_path, insert);
            }
            ContentBlock::Audio(value) => {
                insert(Feature::AudioInput, block_path.clone());
                extract_cache(&value.metadata.cache_policy, &block_path, insert);
            }
            ContentBlock::Video(value) => {
                insert(Feature::VideoInput, block_path.clone());
                extract_cache(&value.metadata.cache_policy, &block_path, insert);
            }
            ContentBlock::File(value) => {
                insert(Feature::FileInput, block_path.clone());
                extract_cache(&value.metadata.cache_policy, &block_path, insert);
            }
            ContentBlock::ToolCall(value) => {
                insert(Feature::Tools, block_path.clone());
                if value.kind == ToolKind::Custom {
                    insert(Feature::CustomTools, block_path.clone());
                }
                for artifact in &value.artifacts {
                    if let Some(feature) = artifact_feature(&artifact.kind) {
                        insert(feature, block_path.clone());
                    }
                }
                extract_cache(&value.metadata.cache_policy, &block_path, insert);
            }
            ContentBlock::ToolResult(value) => {
                insert(Feature::Tools, block_path.clone());
                extract_cache(&value.metadata.cache_policy, &block_path, insert);
                extract_block_features(&value.content, &block_path, insert);
            }
            ContentBlock::ProviderArtifact(artifact) => {
                if let Some(feature) = artifact_feature(&artifact.kind) {
                    insert(feature, block_path.clone());
                }
            }
        }
        let metadata = match block {
            ContentBlock::Text(value) => Some(&value.metadata),
            ContentBlock::Reasoning(value) => Some(&value.metadata),
            ContentBlock::Refusal(value) => Some(&value.metadata),
            ContentBlock::Image(value)
            | ContentBlock::Audio(value)
            | ContentBlock::Video(value) => Some(&value.metadata),
            ContentBlock::File(value) => Some(&value.metadata),
            ContentBlock::ToolCall(value) => Some(&value.metadata),
            ContentBlock::ToolResult(value) => Some(&value.metadata),
            ContentBlock::ProviderArtifact(_) => None,
        };
        if let Some(metadata) = metadata {
            extract_prompt_cache_breakpoint(&metadata.prompt_cache_breakpoint, &block_path, insert);
        }
    }
}

fn extract_cache(
    cache_policy: &Option<CachePolicy>,
    path: &str,
    insert: &mut impl FnMut(Feature, String),
) {
    if cache_policy.is_some() {
        insert(Feature::CacheControl, path.to_string());
    }
}

fn extract_prompt_cache_breakpoint(
    breakpoint: &Option<crate::protocol::ir::PromptCacheBreakpointMode>,
    path: &str,
    insert: &mut impl FnMut(Feature, String),
) {
    if breakpoint.is_some() {
        insert(Feature::PromptCacheBreakpoint, path.to_string());
    }
}

fn artifact_feature(kind: &crate::protocol::ir::ArtifactKind) -> Option<Feature> {
    match kind {
        crate::protocol::ir::ArtifactKind::AnthropicThinkingSignature
        | crate::protocol::ir::ArtifactKind::AnthropicRedactedThinking => {
            Some(Feature::AnthropicSignatureReplay)
        }
        crate::protocol::ir::ArtifactKind::GeminiThoughtSignature => {
            Some(Feature::GeminiThoughtSignatureReplay)
        }
        crate::protocol::ir::ArtifactKind::OpenAiEncryptedReasoning => {
            Some(Feature::EncryptedReasoning)
        }
        crate::protocol::ir::ArtifactKind::ContinuationToken
        | crate::protocol::ir::ArtifactKind::ProviderSpecific => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Feature, extract_required_features};
    use crate::protocol::ir::{CachePolicy, CanonicalRequestV2, FunctionTool, ToolDefinition};

    #[test]
    fn extracts_cache_control_from_function_tool_definition() {
        let mut request = CanonicalRequestV2::new("claude-test");
        request.tools.push(ToolDefinition::Function(FunctionTool {
            name: "lookup".to_string(),
            description: None,
            input_schema: json!({"type":"object"}),
            strict: None,
            cache_policy: Some(CachePolicy::Ephemeral1h),
            defer_loading: None,
            allowed_callers: Vec::new(),
            input_examples: Vec::new(),
            eager_input_streaming: None,
        }));

        let features = extract_required_features(&request);

        assert!(features.iter().any(|feature| {
            feature.feature == Feature::CacheControl && feature.path == "tools[0].cache_policy"
        }));
    }
}
