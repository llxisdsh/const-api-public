use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::protocol::continuation::reasoning_artifacts_are_encrypted;
use crate::protocol::ir::{
    ArtifactKind, BlockHeader, BlockMetadata, CanonicalError, CanonicalResponseV2,
    CanonicalStreamEvent, ContentBlock, ContentBlockKind, FinishDetail, FinishReason, ItemStatus,
    OpaqueArtifact, ReasoningBlock, RefusalBlock, ResponseStatus, TextBlock, ToolCall, ToolKind,
    Usage,
};

use super::{MAX_BUFFERED_STREAM_DURATION_SECS, MAX_STREAM_BYTES, StreamError, merge_usage};

enum BlockBuilder {
    Text(String),
    Reasoning {
        text: String,
        summaries: BTreeMap<u32, String>,
        artifacts: Vec<OpaqueArtifact>,
    },
    Refusal(String),
    ToolCall {
        header: BlockHeader,
        arguments: String,
        artifacts: Vec<OpaqueArtifact>,
    },
    Unsupported(ContentBlockKind),
}

pub(crate) struct CanonicalAccumulator {
    started_at: Instant,
    bytes: usize,
    id: Option<String>,
    model: Option<String>,
    status: ResponseStatus,
    finish: Option<FinishDetail>,
    usage: Usage,
    error: Option<CanonicalError>,
    builders: BTreeMap<u32, BlockBuilder>,
    completed: BTreeMap<u32, ContentBlock>,
    trailing_artifacts: BTreeMap<u32, Vec<OpaqueArtifact>>,
}

impl CanonicalAccumulator {
    pub(crate) fn new() -> Self {
        Self {
            started_at: Instant::now(),
            bytes: 0,
            id: None,
            model: None,
            status: ResponseStatus::InProgress,
            finish: None,
            usage: Usage::default(),
            error: None,
            builders: BTreeMap::new(),
            completed: BTreeMap::new(),
            trailing_artifacts: BTreeMap::new(),
        }
    }

    pub(crate) fn push(&mut self, event: &CanonicalStreamEvent) -> Result<(), StreamError> {
        if self.started_at.elapsed() > Duration::from_secs(MAX_BUFFERED_STREAM_DURATION_SECS) {
            return Err(StreamError::new(
                "stream_duration_exceeded",
                format!("buffered stream exceeded {MAX_BUFFERED_STREAM_DURATION_SECS} seconds"),
            ));
        }
        self.bytes = self
            .bytes
            .checked_add(event_size(event))
            .filter(|bytes| *bytes <= MAX_STREAM_BYTES)
            .ok_or_else(|| {
                StreamError::new(
                    "stream_buffer_exceeded",
                    format!("canonical stream exceeded {MAX_STREAM_BYTES} bytes"),
                )
            })?;
        match event {
            CanonicalStreamEvent::ResponseStart(meta) => {
                if self.id.is_none() {
                    self.id = meta.id.clone();
                }
                if meta.model.is_some() {
                    self.model = meta.model.clone();
                }
                self.status = meta.status;
            }
            CanonicalStreamEvent::BlockStart { index, block } => {
                self.builders
                    .entry(*index)
                    .or_insert_with(|| builder_from_header(block));
            }
            CanonicalStreamEvent::TextDelta { index, text } => {
                let builder = self
                    .builders
                    .entry(*index)
                    .or_insert_with(|| BlockBuilder::Text(String::new()));
                match builder {
                    BlockBuilder::Text(buffer) => buffer.push_str(text),
                    _ => return Err(block_mismatch(*index, "text")),
                }
            }
            CanonicalStreamEvent::ReasoningDelta { index, text } => {
                let builder = self
                    .builders
                    .entry(*index)
                    .or_insert_with(reasoning_builder);
                match builder {
                    BlockBuilder::Reasoning { text: buffer, .. } => buffer.push_str(text),
                    _ => return Err(block_mismatch(*index, "reasoning")),
                }
            }
            CanonicalStreamEvent::ReasoningSummaryDelta { index, part, text } => {
                let builder = self
                    .builders
                    .entry(*index)
                    .or_insert_with(reasoning_builder);
                match builder {
                    BlockBuilder::Reasoning { summaries, .. } => {
                        summaries.entry(*part).or_default().push_str(text)
                    }
                    _ => return Err(block_mismatch(*index, "reasoning_summary")),
                }
            }
            CanonicalStreamEvent::ArtifactDelta { index, kind, data } => {
                let model = self.model.as_deref();
                let builder = self
                    .builders
                    .entry(*index)
                    .or_insert_with(reasoning_builder);
                match builder {
                    BlockBuilder::Reasoning { artifacts, .. } => {
                        append_artifact(artifacts, kind, data, ContentBlockKind::Reasoning, model);
                    }
                    BlockBuilder::ToolCall { artifacts, .. } => {
                        append_artifact(artifacts, kind, data, ContentBlockKind::ToolCall, model);
                    }
                    BlockBuilder::Text(_) => append_artifact(
                        self.trailing_artifacts.entry(*index).or_default(),
                        kind,
                        data,
                        ContentBlockKind::Text,
                        model,
                    ),
                    BlockBuilder::Refusal(_) => append_artifact(
                        self.trailing_artifacts.entry(*index).or_default(),
                        kind,
                        data,
                        ContentBlockKind::Refusal,
                        model,
                    ),
                    BlockBuilder::Unsupported(owner) => append_artifact(
                        self.trailing_artifacts.entry(*index).or_default(),
                        kind,
                        data,
                        *owner,
                        model,
                    ),
                }
            }
            CanonicalStreamEvent::ToolArgumentsDelta { index, data } => {
                let builder =
                    self.builders
                        .entry(*index)
                        .or_insert_with(|| BlockBuilder::ToolCall {
                            header: BlockHeader {
                                tool_kind: None,
                                tool_namespace: None,
                                kind: ContentBlockKind::ToolCall,
                                source_item_id: None,
                                call_id: Some(format!("call_{index}")),
                                name: Some("tool".to_string()),
                                artifacts: Vec::new(),
                            },
                            arguments: String::new(),
                            artifacts: Vec::new(),
                        });
                match builder {
                    BlockBuilder::ToolCall { arguments, .. } => arguments.push_str(data),
                    _ => return Err(block_mismatch(*index, "tool_arguments")),
                }
            }
            CanonicalStreamEvent::RefusalDelta { index, text } => {
                let builder = self
                    .builders
                    .entry(*index)
                    .or_insert_with(|| BlockBuilder::Refusal(String::new()));
                match builder {
                    BlockBuilder::Refusal(buffer) => buffer.push_str(text),
                    _ => return Err(block_mismatch(*index, "refusal")),
                }
            }
            CanonicalStreamEvent::BlockDone { index, block } => {
                if let Some(block) = block {
                    self.builders.remove(index);
                    self.completed.insert(*index, block.clone());
                } else if let Some(builder) = self.builders.remove(index) {
                    if let Some(block) = finish_builder(builder)? {
                        self.completed.insert(*index, block);
                    }
                }
            }
            CanonicalStreamEvent::UsageUpdate(usage) => merge_usage(&mut self.usage, usage),
            CanonicalStreamEvent::StatusUpdate(status) => self.status = *status,
            CanonicalStreamEvent::Error(error) => {
                self.error = Some(error.clone());
                self.status = ResponseStatus::Failed;
            }
            CanonicalStreamEvent::ResponseDone(finish) => {
                if self.finish.is_none() {
                    self.finish = Some(finish.clone());
                }
            }
        }
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Result<CanonicalResponseV2, StreamError> {
        let pending = std::mem::take(&mut self.builders);
        for (index, builder) in pending {
            if let Some(block) = finish_builder(builder)? {
                self.completed.insert(index, block);
            }
        }
        let finish = self.finish.ok_or_else(|| {
            StreamError::new(
                "stream_incomplete",
                "stream ended without a terminal response event",
            )
        })?;
        let mut blocks = Vec::new();
        for (index, block) in self.completed {
            blocks.push(block);
            if let Some(artifacts) = self.trailing_artifacts.remove(&index) {
                blocks.extend(artifacts.into_iter().map(ContentBlock::ProviderArtifact));
            }
        }
        let response = CanonicalResponseV2 {
            id: self.id,
            model: self.model,
            blocks,
            status: self.status,
            finish,
            usage: self.usage,
            error: self.error,
            extensions: Vec::new(),
        };
        response
            .validate()
            .map_err(|error| StreamError::new(error.code(), error.to_string()))?;
        Ok(response)
    }
}

fn builder_from_header(header: &BlockHeader) -> BlockBuilder {
    match header.kind {
        ContentBlockKind::Text => BlockBuilder::Text(String::new()),
        ContentBlockKind::Reasoning | ContentBlockKind::ProviderArtifact => {
            BlockBuilder::Reasoning {
                text: String::new(),
                summaries: BTreeMap::new(),
                artifacts: header.artifacts.clone(),
            }
        }
        ContentBlockKind::Refusal => BlockBuilder::Refusal(String::new()),
        ContentBlockKind::ToolCall => BlockBuilder::ToolCall {
            header: header.clone(),
            arguments: String::new(),
            artifacts: header.artifacts.clone(),
        },
        other => BlockBuilder::Unsupported(other),
    }
}

fn reasoning_builder() -> BlockBuilder {
    BlockBuilder::Reasoning {
        text: String::new(),
        summaries: BTreeMap::new(),
        artifacts: Vec::new(),
    }
}

fn finish_builder(builder: BlockBuilder) -> Result<Option<ContentBlock>, StreamError> {
    Ok(match builder {
        BlockBuilder::Text(text) => Some(ContentBlock::Text(TextBlock::new(text))),
        BlockBuilder::Refusal(text) => Some(ContentBlock::Refusal(RefusalBlock::new(text))),
        BlockBuilder::Reasoning {
            text,
            summaries,
            artifacts,
        } => Some(ContentBlock::Reasoning(ReasoningBlock {
            text: (!text.is_empty()).then_some(text),
            summary: summaries.into_values().map(TextBlock::new).collect(),
            encrypted: reasoning_artifacts_are_encrypted(&artifacts),
            artifacts,
            metadata: BlockMetadata::default(),
        })),
        BlockBuilder::ToolCall {
            header,
            arguments,
            artifacts,
        } => {
            let parsed = if header.tool_kind == Some(ToolKind::Custom) {
                serde_json::Value::Null
            } else if arguments.trim().is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_str(&arguments).map_err(|error| {
                    StreamError::new(
                        "stream_tool_arguments_invalid",
                        format!("tool arguments are incomplete: {error}"),
                    )
                })?
            };
            Some(ContentBlock::ToolCall(ToolCall {
                id: header.call_id.unwrap_or_else(|| "call_stream".to_string()),
                source_item_id: header.source_item_id,
                kind: header.tool_kind.unwrap_or(ToolKind::Function),
                name: header.name.unwrap_or_else(|| "tool".to_string()),
                arguments: Some(parsed),
                raw_arguments: Some(arguments),
                status: ItemStatus::Completed,
                artifacts,
                metadata: BlockMetadata {
                    tool_namespace: header.tool_namespace,
                    ..Default::default()
                },
            }))
        }
        BlockBuilder::Unsupported(kind) => {
            return Err(StreamError::new(
                "stream_block_unsupported",
                format!("cannot accumulate {kind:?} stream block"),
            ));
        }
    })
}

fn artifact(
    kind: ArtifactKind,
    payload: String,
    owner: ContentBlockKind,
    model: Option<&str>,
) -> OpaqueArtifact {
    crate::protocol::continuation::stream_artifact(
        kind,
        serde_json::Value::String(payload),
        owner,
        model,
    )
}

fn append_artifact(
    artifacts: &mut Vec<OpaqueArtifact>,
    kind: &ArtifactKind,
    data: &str,
    owner: ContentBlockKind,
    model: Option<&str>,
) {
    match artifacts.iter_mut().find(|artifact| &artifact.kind == kind) {
        Some(artifact) => match artifact.payload.as_str() {
            Some(payload) => {
                artifact.payload = serde_json::Value::String(format!("{payload}{data}"))
            }
            None => artifact.payload = serde_json::Value::String(data.to_string()),
        },
        None => artifacts.push(artifact(kind.clone(), data.to_string(), owner, model)),
    }
}

fn event_size(event: &CanonicalStreamEvent) -> usize {
    match event {
        CanonicalStreamEvent::BlockStart { block, .. } => block
            .artifacts
            .iter()
            .map(|artifact| artifact.payload.to_string().len())
            .sum(),
        CanonicalStreamEvent::TextDelta { text, .. }
        | CanonicalStreamEvent::ReasoningDelta { text, .. }
        | CanonicalStreamEvent::ReasoningSummaryDelta { text, .. }
        | CanonicalStreamEvent::RefusalDelta { text, .. } => text.len(),
        CanonicalStreamEvent::ArtifactDelta { data, .. }
        | CanonicalStreamEvent::ToolArgumentsDelta { data, .. } => data.len(),
        _ => 0,
    }
}

fn block_mismatch(index: u32, delta: &str) -> StreamError {
    StreamError::new(
        "stream_block_type_mismatch",
        format!("{delta} delta does not match block {index}"),
    )
}

#[allow(dead_code)]
fn _finish_reason_is_exhaustive(reason: FinishReason) -> FinishReason {
    reason
}
