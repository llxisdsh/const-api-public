use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;
#[cfg(test)]
use serde_json::json;
#[cfg(test)]
use sha2::{Digest, Sha256};

#[cfg(test)]
use super::ArtifactAffinity;
use super::{IrError, OpaqueArtifact, ReasoningBlock, ToolCall, ToolResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ItemStatus {
    InProgress,
    Completed,
    Incomplete,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub(crate) struct BlockMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tool_namespace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) status: Option<ItemStatus>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) annotations: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cache_policy: Option<CachePolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) prompt_cache_breakpoint: Option<PromptCacheBreakpointMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_location: Option<SourceLocation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PromptCacheBreakpointMode {
    Explicit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CachePolicy {
    Ephemeral5m,
    Ephemeral1h,
    Persistent,
    NoStore,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SourceLocation {
    pub(crate) protocol_path: Option<String>,
    pub(crate) item_index: Option<u32>,
    pub(crate) content_index: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct TextBlock {
    pub(crate) text: String,
    #[serde(default)]
    pub(crate) metadata: BlockMetadata,
}

impl TextBlock {
    pub(crate) fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            metadata: BlockMetadata::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct RefusalBlock {
    pub(crate) text: String,
    #[serde(default)]
    pub(crate) metadata: BlockMetadata,
}

impl RefusalBlock {
    pub(crate) fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            metadata: BlockMetadata::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub(crate) enum MediaSource {
    InlineBase64 {
        media_type: String,
        data: String,
    },
    RemoteUrl {
        url: String,
    },
    ProviderFileId {
        provider: String,
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media_type: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MediaDetail {
    Auto,
    Low,
    High,
    Original,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct MediaBlock {
    pub(crate) source: MediaSource,
    pub(crate) detail: Option<MediaDetail>,
    #[serde(default)]
    pub(crate) metadata: BlockMetadata,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct FileBlock {
    pub(crate) source: MediaSource,
    pub(crate) filename: Option<String>,
    #[serde(default)]
    pub(crate) metadata: BlockMetadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ContentBlockKind {
    Text,
    Reasoning,
    Refusal,
    Image,
    Audio,
    Video,
    File,
    ToolCall,
    ToolResult,
    ProviderArtifact,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ContentBlock {
    Text(TextBlock),
    Reasoning(ReasoningBlock),
    Refusal(RefusalBlock),
    Image(MediaBlock),
    Audio(MediaBlock),
    Video(MediaBlock),
    File(FileBlock),
    ToolCall(ToolCall),
    ToolResult(ToolResult),
    ProviderArtifact(OpaqueArtifact),
}

pub(super) fn validate_blocks(blocks: &[ContentBlock], path: &str) -> Result<(), IrError> {
    let mut call_ids = HashSet::new();
    validate_blocks_with_calls(blocks, path, &mut call_ids)
}

fn validate_blocks_with_calls(
    blocks: &[ContentBlock],
    path: &str,
    call_ids: &mut HashSet<String>,
) -> Result<(), IrError> {
    for (index, block) in blocks.iter().enumerate() {
        let block_path = format!("{path}.blocks[{index}]");
        match block {
            ContentBlock::Image(media)
            | ContentBlock::Audio(media)
            | ContentBlock::Video(media) => validate_media_source(&media.source, &block_path)?,
            ContentBlock::File(file) => validate_media_source(&file.source, &block_path)?,
            ContentBlock::ToolCall(call) => {
                call.validate(&block_path)?;
                if !call_ids.insert(call.id.clone()) {
                    return Err(IrError::new(
                        "duplicate_tool_call_id",
                        block_path,
                        format!("tool call ID {} is duplicated", call.id),
                    ));
                }
            }
            ContentBlock::ToolResult(result) => {
                if result.call_id.trim().is_empty() {
                    return Err(IrError::new(
                        "invalid_tool_call_id",
                        block_path.clone(),
                        "tool result call ID is empty",
                    ));
                }
                validate_blocks_with_calls(&result.content, &block_path, call_ids)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_media_source(source: &MediaSource, path: &str) -> Result<(), IrError> {
    if let MediaSource::InlineBase64 { media_type, .. } = source {
        let mut parts = media_type.split('/');
        let valid = parts.next().is_some_and(valid_media_token)
            && parts.next().is_some_and(valid_media_token)
            && parts.next().is_none();
        if !valid {
            return Err(IrError::new(
                "invalid_media_type",
                path,
                format!("invalid media type {media_type:?}"),
            ));
        }
    }
    Ok(())
}

fn valid_media_token(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#' | b'$' | b'&' | b'^' | b'_' | b'.' | b'+' | b'-'
                )
        })
}

#[cfg(test)]
pub(super) fn redacted_blocks(blocks: &[ContentBlock]) -> Vec<Value> {
    blocks.iter().map(redacted_block).collect()
}

#[cfg(test)]
fn redacted_block(block: &ContentBlock) -> Value {
    match block {
        ContentBlock::Text(value) => json!({
            "kind": "text",
            "chars": value.text.chars().count(),
            "bytes": value.text.len(),
        }),
        ContentBlock::Reasoning(value) => json!({
            "kind": "reasoning",
            "text_chars": value.text.as_ref().map(|text| text.chars().count()).unwrap_or(0),
            "summary": value.summary.iter().map(|part| json!({
                "chars": part.text.chars().count(),
                "bytes": part.text.len(),
            })).collect::<Vec<_>>(),
            "artifacts": value.artifacts.iter().map(redacted_artifact).collect::<Vec<_>>(),
            "encrypted": value.encrypted,
        }),
        ContentBlock::Refusal(value) => json!({
            "kind": "refusal",
            "chars": value.text.chars().count(),
            "bytes": value.text.len(),
        }),
        ContentBlock::Image(value) => redacted_media("image", &value.source),
        ContentBlock::Audio(value) => redacted_media("audio", &value.source),
        ContentBlock::Video(value) => redacted_media("video", &value.source),
        ContentBlock::File(value) => redacted_media("file", &value.source),
        ContentBlock::ToolCall(call) => {
            let argument_bytes = call
                .arguments
                .as_ref()
                .and_then(|value| serde_json::to_vec(value).ok())
                .unwrap_or_else(|| call.raw_arguments.clone().unwrap_or_default().into_bytes());
            json!({
                "kind": "tool_call",
                "id": call.id,
                "tool_kind": call.kind,
                "name": call.name,
                "argument_bytes": argument_bytes.len(),
                "argument_sha256": hex::encode(Sha256::digest(&argument_bytes)),
                "status": call.status,
                "artifacts": call.artifacts.iter().map(redacted_artifact).collect::<Vec<_>>(),
            })
        }
        ContentBlock::ToolResult(result) => json!({
            "kind": "tool_result",
            "call_id": result.call_id,
            "name": result.name,
            "status": result.status,
            "is_error": result.is_error,
            "content": redacted_blocks(&result.content),
        }),
        ContentBlock::ProviderArtifact(artifact) => redacted_artifact(artifact),
    }
}

#[cfg(test)]
fn redacted_media(kind: &str, source: &MediaSource) -> Value {
    match source {
        MediaSource::InlineBase64 { media_type, data } => json!({
            "kind": kind,
            "source_kind": "inline_base64",
            "media_type": media_type,
            "byte_estimate": base64_byte_estimate(data),
        }),
        MediaSource::RemoteUrl { .. } => json!({
            "kind": kind,
            "source_kind": "remote_url",
            "redacted": true,
        }),
        MediaSource::ProviderFileId { provider, .. } => json!({
            "kind": kind,
            "source_kind": "provider_file_id",
            "provider": provider,
            "redacted": true,
        }),
    }
}

#[cfg(test)]
fn base64_byte_estimate(data: &str) -> usize {
    let meaningful = data
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .count();
    let padding = data
        .trim_end()
        .bytes()
        .rev()
        .take_while(|byte| *byte == b'=')
        .count();
    (meaningful.saturating_mul(3) / 4).saturating_sub(padding)
}

#[cfg(test)]
fn redacted_artifact(artifact: &OpaqueArtifact) -> Value {
    json!({
        "kind": artifact.kind,
        "affinity": redacted_affinity(&artifact.affinity),
        "replay": artifact.replay,
        "criticality": artifact.criticality,
        "redacted": true,
    })
}

#[cfg(test)]
fn redacted_affinity(affinity: &ArtifactAffinity) -> Value {
    serde_json::to_value(affinity).unwrap_or_else(|_| json!({"scope":"unknown"}))
}
