mod accumulator;
mod parser;
mod renderer;
mod sse;

use std::fmt;

use bytes::Bytes;

use crate::protocol::ir::{
    CanonicalError, CanonicalStreamEvent, FinishDetail, FinishReason, ResponseStatus,
};
use crate::protocol::kind::ProtocolKind;

pub(crate) use accumulator::CanonicalAccumulator;
pub(crate) use parser::StreamParser;
pub(crate) use renderer::StreamRenderer;
pub(crate) use sse::{SseDecoder, SseFrame, Utf8ChunkDecoder};

pub(crate) const MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;
// Long reasoning can legitimately produce no SSE event for several minutes.
// Keep pass-through inactivity and bounded full-buffer conversion explicit so
// neither is accidentally shortened when transport timeouts are tuned.
pub(crate) const STREAM_INACTIVITY_TIMEOUT_SECS: u64 = crate::MODEL_REQUEST_TIMEOUT.as_secs();
pub(crate) const MAX_BUFFERED_STREAM_DURATION_SECS: u64 = crate::MODEL_REQUEST_TIMEOUT.as_secs();

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StreamConversionNotice {
    pub(crate) code: &'static str,
    pub(crate) path: String,
    pub(crate) summary: String,
}

pub(crate) fn find_sse_event_boundary(buffer: &str) -> Option<(usize, usize)> {
    sse::find_boundary(buffer.as_bytes())
}

pub(crate) fn decode_sse_frames(
    bytes: &[u8],
) -> Result<Vec<(Option<String>, String)>, StreamError> {
    let mut decoder = sse::SseDecoder::default();
    let mut frames = decoder.push(bytes)?;
    frames.extend(decoder.finish()?);
    Ok(frames
        .into_iter()
        .map(|frame| (frame.event, frame.data))
        .collect())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StreamError {
    code: &'static str,
    message: String,
}

impl StreamError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(crate) const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for StreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for StreamError {}

#[cfg(test)]
mod reasoning_usage_tests {
    use super::*;

    #[test]
    fn reasoning_usage_survives_all_stream_protocol_pairs() {
        use ProtocolKind::*;
        let fixtures = [
            (
                OpenAiChat,
                "data: {\"id\":\"chatcmpl-usage\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":32,\"completion_tokens\":9,\"total_tokens\":135,\"completion_tokens_details\":{\"reasoning_tokens\":94}}}\n\ndata: [DONE]\n\n",
            ),
            (
                OpenAiResponses,
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp-usage\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":32,\"output_tokens\":103,\"total_tokens\":135,\"output_tokens_details\":{\"reasoning_tokens\":94}}}}\n\n",
            ),
            (
                AnthropicMessages,
                "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-usage\",\"usage\":{\"input_tokens\":32,\"output_tokens\":0}}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":103,\"output_tokens_details\":{\"thinking_tokens\":94}}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
            ),
            (
                GeminiNative,
                "data: {\"candidates\":[{\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":32,\"candidatesTokenCount\":9,\"thoughtsTokenCount\":94,\"totalTokenCount\":135}}\n\n",
            ),
        ];
        for (source, raw) in fixtures {
            for target in [OpenAiChat, OpenAiResponses, AnthropicMessages, GeminiNative] {
                let mut converter = stream_converter(source, target, "usage-fixture");
                let mut bytes = Vec::new();
                for chunk in raw.as_bytes().chunks(7) {
                    bytes.extend_from_slice(&converter.push(chunk));
                }
                bytes.extend_from_slice(&converter.finish());
                let mut parser = StreamParser::new(target, "usage-fixture");
                let mut events = parser.push(&bytes).unwrap();
                events.extend(parser.finish().unwrap());
                let mut usage = crate::protocol::ir::Usage::default();
                for event in events {
                    if let CanonicalStreamEvent::UsageUpdate(update) = event {
                        merge_usage(&mut usage, &update);
                    }
                }
                assert_eq!(
                    usage.output_tokens.value,
                    Some(103),
                    "{source:?} -> {target:?}: {}",
                    String::from_utf8_lossy(&bytes)
                );
                assert_eq!(
                    usage.reasoning_tokens.value,
                    Some(94),
                    "{source:?} -> {target:?}"
                );
            }
        }
    }
}

pub(super) fn merge_usage(
    current: &mut crate::protocol::ir::Usage,
    update: &crate::protocol::ir::Usage,
) {
    macro_rules! replace {
        ($field:ident) => {
            if update.$field.value.is_some() {
                current.$field = update.$field.clone();
            }
        };
    }
    replace!(input_tokens);
    replace!(output_tokens);
    replace!(total_tokens);
    replace!(cache_read_tokens);
    replace!(cache_write_tokens);
    replace!(cache_creation_tokens);
    replace!(cache_expiry_5m_tokens);
    replace!(cache_expiry_1h_tokens);
    replace!(reasoning_tokens);
    replace!(text_input_units);
    replace!(text_output_units);
    replace!(image_input_units);
    replace!(image_output_units);
    replace!(audio_input_units);
    replace!(audio_output_units);
    replace!(video_input_units);
    replace!(video_output_units);
    replace!(accepted_prediction_tokens);
    replace!(rejected_prediction_tokens);
    replace!(server_tool_calls);
    current
        .provider_billable_units
        .extend(update.provider_billable_units.clone());
}

pub(crate) struct StreamConverter {
    tool_mapper: Option<crate::protocol::conversion::ToolStreamMapper>,
    parser: StreamParser,
    renderer: StreamRenderer,
    target_protocol: ProtocolKind,
    failure: Option<StreamError>,
    notices: Vec<StreamConversionNotice>,
}

pub(crate) fn stream_converter(
    source: ProtocolKind,
    target: ProtocolKind,
    model: impl Into<String>,
) -> StreamConverter {
    let model = model.into();
    StreamConverter {
        tool_mapper: None,
        parser: StreamParser::new(source, &model),
        renderer: StreamRenderer::new(target, model),
        target_protocol: target,
        failure: None,
        notices: Vec::new(),
    }
}

pub(crate) fn events_from_response_with_notices(
    response: &crate::protocol::ir::CanonicalResponseV2,
) -> Result<(Vec<CanonicalStreamEvent>, Vec<StreamConversionNotice>), StreamError> {
    use crate::protocol::ir::{BlockHeader, ContentBlock, ContentBlockKind, ResponseMeta};

    let mut events = vec![CanonicalStreamEvent::ResponseStart(ResponseMeta {
        id: response.id.clone(),
        model: response.model.clone(),
        status: ResponseStatus::InProgress,
        provider_event_id: None,
    })];
    let mut notices = response
        .extensions
        .iter()
        .map(|extension| StreamConversionNotice {
            code: "response_extension_omitted_in_stream_adaptation",
            path: extension
                .nested_path()
                .map(str::to_string)
                .unwrap_or_else(|| format!("$.{}", extension.name)),
            summary: format!(
                "Response field {}.{} could not be represented while adapting a complete response to SSE and was omitted so the stream could continue",
                extension.namespace, extension.name
            ),
        })
        .collect::<Vec<_>>();
    let mut output_index = 0_u32;
    let mut previous_output_index = None;
    for (source_index, block) in response.blocks.iter().enumerate() {
        let source_path = format!("$.blocks[{source_index}]");
        if let ContentBlock::ProviderArtifact(artifact) = block {
            if crate::protocol::continuation::is_portable_artifact(&artifact.kind) {
                if let (Some(index), Some(data)) =
                    (previous_output_index, artifact.payload.as_str())
                {
                    let insert_at = events.len().saturating_sub(1);
                    events.insert(
                        insert_at,
                        CanonicalStreamEvent::ArtifactDelta {
                            index,
                            kind: artifact.kind.clone(),
                            data: data.to_string(),
                        },
                    );
                    continue;
                }
            }
        }
        let index = output_index;
        let Some(header) = (match block {
            ContentBlock::Text(_) => Some(BlockHeader {
                tool_kind: None,
                tool_namespace: None,
                kind: ContentBlockKind::Text,
                source_item_id: None,
                call_id: None,
                name: None,
                artifacts: Vec::new(),
            }),
            ContentBlock::Reasoning(reasoning) => Some(BlockHeader {
                tool_kind: None,
                tool_namespace: None,
                kind: ContentBlockKind::Reasoning,
                source_item_id: None,
                call_id: None,
                name: None,
                // Redacted thinking has no delta representation in Anthropic SSE: its opaque
                // payload belongs on content_block_start, so it must travel with the IR header.
                artifacts: reasoning
                    .artifacts
                    .iter()
                    .filter(|artifact| {
                        artifact.kind
                            == crate::protocol::ir::ArtifactKind::AnthropicRedactedThinking
                    })
                    .cloned()
                    .collect(),
            }),
            ContentBlock::Refusal(_) => Some(BlockHeader {
                tool_kind: None,
                tool_namespace: None,
                kind: ContentBlockKind::Refusal,
                source_item_id: None,
                call_id: None,
                name: None,
                artifacts: Vec::new(),
            }),
            ContentBlock::ToolCall(call) => Some(BlockHeader {
                tool_kind: Some(call.kind),
                tool_namespace: call.metadata.tool_namespace.clone(),
                kind: ContentBlockKind::ToolCall,
                source_item_id: call.source_item_id.clone(),
                call_id: Some(call.id.clone()),
                name: Some(call.name.clone()),
                artifacts: call.artifacts.clone(),
            }),
            _ => None,
        }) else {
            notices.push(StreamConversionNotice {
                code: "response_block_omitted_in_stream_adaptation",
                path: source_path,
                summary: "A complete response block has no canonical SSE representation; it was omitted so the remaining stream could continue".to_string(),
            });
            continue;
        };
        output_index = output_index.checked_add(1).ok_or_else(|| {
            StreamError::new("stream_block_index_overflow", "too many response blocks")
        })?;
        if block_has_annotations(block) {
            notices.push(StreamConversionNotice {
                code: "response_annotations_omitted_in_stream_adaptation",
                path: format!("{source_path}.metadata.annotations"),
                summary:
                    "Response annotations could not be represented by the canonical SSE bridge"
                        .to_string(),
            });
        }
        events.push(CanonicalStreamEvent::BlockStart {
            index,
            block: header,
        });
        match block {
            ContentBlock::Text(text) => events.push(CanonicalStreamEvent::TextDelta {
                index,
                text: text.text.clone(),
            }),
            ContentBlock::Reasoning(reasoning) => {
                if let Some(text) = &reasoning.text {
                    events.push(CanonicalStreamEvent::ReasoningDelta {
                        index,
                        text: text.clone(),
                    });
                }
                for (part, summary) in reasoning.summary.iter().enumerate() {
                    events.push(CanonicalStreamEvent::ReasoningSummaryDelta {
                        index,
                        part: part as u32,
                        text: summary.text.clone(),
                    });
                }
                for artifact in &reasoning.artifacts {
                    if artifact.kind == crate::protocol::ir::ArtifactKind::AnthropicRedactedThinking
                    {
                        continue;
                    }
                    if let Some(data) = artifact.payload.as_str() {
                        events.push(CanonicalStreamEvent::ArtifactDelta {
                            index,
                            kind: artifact.kind.clone(),
                            data: data.to_string(),
                        });
                    } else {
                        notices.push(StreamConversionNotice {
                            code: "response_artifact_omitted_in_stream_adaptation",
                            path: format!("{source_path}.artifacts"),
                            summary: "A non-text response artifact could not be represented by the canonical SSE bridge".to_string(),
                        });
                    }
                }
            }
            ContentBlock::Refusal(refusal) => events.push(CanonicalStreamEvent::RefusalDelta {
                index,
                text: refusal.text.clone(),
            }),
            ContentBlock::ToolCall(call) => {
                let data = call
                    .raw_arguments
                    .clone()
                    .or_else(|| {
                        call.arguments
                            .as_ref()
                            .and_then(|value| serde_json::to_string(value).ok())
                    })
                    .unwrap_or_else(|| "{}".to_string());
                events.push(CanonicalStreamEvent::ToolArgumentsDelta { index, data });
            }
            _ => {
                return Err(StreamError::new(
                    "response_block_kind_mismatch",
                    format!("{source_path} has no canonical SSE representation"),
                ));
            }
        }
        events.push(CanonicalStreamEvent::BlockDone {
            index,
            block: Some(block.clone()),
        });
        previous_output_index = Some(index);
    }
    if response.usage != crate::protocol::ir::Usage::default() {
        events.push(CanonicalStreamEvent::UsageUpdate(Box::new(
            response.usage.clone(),
        )));
    }
    events.push(CanonicalStreamEvent::StatusUpdate(response.status));
    if let Some(error) = &response.error {
        // A complete JSON error can be returned even when the caller requested SSE. Preserve the
        // terminal error as a canonical event so the target renderer emits its native error shape
        // instead of producing an apparently successful but empty stream.
        events.push(CanonicalStreamEvent::Error(error.clone()));
    }
    events.push(CanonicalStreamEvent::ResponseDone(response.finish.clone()));
    Ok((events, notices))
}

fn block_has_annotations(block: &crate::protocol::ir::ContentBlock) -> bool {
    use crate::protocol::ir::ContentBlock;

    let metadata = match block {
        ContentBlock::Text(value) => &value.metadata,
        ContentBlock::Reasoning(value) => &value.metadata,
        ContentBlock::Refusal(value) => &value.metadata,
        ContentBlock::Image(value) | ContentBlock::Audio(value) | ContentBlock::Video(value) => {
            &value.metadata
        }
        ContentBlock::File(value) => &value.metadata,
        ContentBlock::ToolCall(value) => &value.metadata,
        ContentBlock::ToolResult(value) => &value.metadata,
        ContentBlock::ProviderArtifact(_) => return false,
    };
    !metadata.annotations.is_empty()
}

pub(crate) fn stream_rendering_notices(
    events: &[CanonicalStreamEvent],
    target: ProtocolKind,
) -> Vec<StreamConversionNotice> {
    use std::collections::HashMap;

    let mut notices = Vec::new();
    let block_kinds = events
        .iter()
        .filter_map(|event| match event {
            CanonicalStreamEvent::BlockStart { index, block } => Some((*index, block.kind)),
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    for event in events {
        let CanonicalStreamEvent::BlockStart { index, block } = event else {
            continue;
        };
        for (artifact_index, artifact) in block.artifacts.iter().enumerate() {
            if !stream_target_supports_artifact(target, block.kind, &artifact.kind) {
                push_unique_notice(
                    &mut notices,
                    StreamConversionNotice {
                        code: "stream_artifact_approximated",
                        path: format!(
                            "$.stream.blocks[{index}].artifacts[{artifact_index}]"
                        ),
                        summary: "Target stream protocol cannot represent this provider artifact; the remaining response continues".to_string(),
                    },
                );
            }
        }
    }
    for event in events {
        let notice = match event {
            CanonicalStreamEvent::ReasoningSummaryDelta { index, part, .. }
                if target != ProtocolKind::OpenAiResponses =>
            {
                Some(StreamConversionNotice {
                    code: "stream_reasoning_summary_approximated",
                    path: format!("$.stream.blocks[{index}].summary[{part}]"),
                    summary:
                        "Target stream protocol has no reasoning-summary event; the main response continues without that summary"
                            .to_string(),
                })
            }
            CanonicalStreamEvent::ArtifactDelta { index, kind, .. }
                if !stream_target_supports_artifact(
                    target,
                    block_kinds
                        .get(index)
                        .copied()
                        .unwrap_or(crate::protocol::ir::ContentBlockKind::Reasoning),
                    kind,
                ) =>
            {
                Some(StreamConversionNotice {
                    code: "stream_artifact_approximated",
                    path: format!("$.stream.blocks[{index}].artifact"),
                    summary:
                        "Target stream protocol cannot represent this provider artifact; the remaining response continues"
                            .to_string(),
                })
            }
            CanonicalStreamEvent::RefusalDelta { index, .. }
                if matches!(
                    target,
                    ProtocolKind::AnthropicMessages | ProtocolKind::GeminiNative
                ) =>
            {
                Some(StreamConversionNotice {
                    code: "stream_refusal_approximated",
                    path: format!("$.stream.blocks[{index}]"),
                    summary:
                        "Target stream protocol represents refusal content as ordinary text"
                            .to_string(),
                })
            }
            _ => None,
        };
        if let Some(notice) = notice {
            push_unique_notice(&mut notices, notice);
        }
    }
    notices
}

fn stream_target_supports_artifact(
    target: ProtocolKind,
    owner: crate::protocol::ir::ContentBlockKind,
    kind: &crate::protocol::ir::ArtifactKind,
) -> bool {
    use crate::protocol::ir::{ArtifactKind, ContentBlockKind};

    if crate::protocol::continuation::is_portable_artifact(kind) {
        return true;
    }

    match target {
        ProtocolKind::AnthropicMessages => {
            owner == ContentBlockKind::Reasoning
                && matches!(
                    kind,
                    ArtifactKind::AnthropicThinkingSignature
                        | ArtifactKind::AnthropicRedactedThinking
                )
        }
        ProtocolKind::GeminiNative => matches!(kind, ArtifactKind::GeminiThoughtSignature),
        ProtocolKind::OpenAiChat => {
            owner == ContentBlockKind::ToolCall
                && matches!(kind, ArtifactKind::GeminiThoughtSignature)
        }
        ProtocolKind::OpenAiResponses => {
            owner == ContentBlockKind::Reasoning
                && matches!(kind, ArtifactKind::OpenAiEncryptedReasoning)
        }
    }
}

fn push_unique_notice(notices: &mut Vec<StreamConversionNotice>, notice: StreamConversionNotice) {
    if notices
        .iter()
        .any(|existing| existing.code == notice.code && existing.path == notice.path)
    {
        return;
    }
    notices.push(notice);
}

impl StreamConverter {
    pub(crate) fn with_tool_mapping(
        mut self,
        mapping: crate::protocol::conversion::ToolWireMap,
    ) -> Self {
        if !mapping.is_empty() {
            self.tool_mapper = Some(mapping.stream_mapper());
        }
        self
    }
    pub(crate) fn push(&mut self, chunk: &[u8]) -> Bytes {
        if self.failure.is_some() {
            return Bytes::new();
        }
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.push_inner(chunk))) {
            Ok(output) => output,
            Err(_) => {
                eprintln!("[const-api][protocol] recovered stream conversion panic");
                let error = StreamError::new(
                    "stream_conversion_panic",
                    "stream conversion panicked; only this request was terminated",
                );
                self.failure = Some(error.clone());
                self.render_error(error)
            }
        }
    }

    fn push_inner(&mut self, chunk: &[u8]) -> Bytes {
        match self.parser.push(chunk) {
            Ok(events) => self.render_events(&events),
            Err(error) => {
                self.failure = Some(error.clone());
                self.render_error(error)
            }
        }
    }

    pub(crate) fn finish(&mut self) -> Bytes {
        if self.failure.is_some() {
            return Bytes::new();
        }
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.finish_inner())) {
            Ok(output) => output,
            Err(_) => {
                eprintln!("[const-api][protocol] recovered stream finalization panic");
                let error = StreamError::new(
                    "stream_conversion_panic",
                    "stream finalization panicked; only this request was terminated",
                );
                self.failure = Some(error.clone());
                self.render_error(error)
            }
        }
    }

    fn finish_inner(&mut self) -> Bytes {
        match self.parser.finish() {
            Ok(events) => {
                let mut output = self.render_events(&events).to_vec();
                match self.renderer.finish() {
                    Ok(tail) => output.extend_from_slice(&tail),
                    Err(error) => {
                        self.failure = Some(error.clone());
                        output.extend_from_slice(&self.render_error(error));
                    }
                }
                Bytes::from(output)
            }
            Err(error) => {
                self.failure = Some(error.clone());
                self.render_error(error)
            }
        }
    }

    pub(crate) fn fail(&mut self, code: &'static str, message: impl Into<String>) -> Bytes {
        let error = StreamError::new(code, message);
        self.failure = Some(error.clone());
        self.render_error(error)
    }

    pub(crate) fn failure(&self) -> Option<&StreamError> {
        self.failure.as_ref()
    }

    pub(crate) fn take_notices(&mut self) -> Vec<StreamConversionNotice> {
        let mut notices = self.parser.take_notices();
        for notice in std::mem::take(&mut self.notices) {
            push_unique_notice(&mut notices, notice);
        }
        notices
    }

    fn render_events(&mut self, events: &[CanonicalStreamEvent]) -> Bytes {
        let mapped;
        let events = if let Some(mapper) = &mut self.tool_mapper {
            match mapper.map(events) {
                Ok(events) => {
                    mapped = events;
                    &mapped
                }
                Err(error) => {
                    self.failure = Some(error.clone());
                    return self.render_error(error);
                }
            }
        } else {
            events
        };
        for notice in stream_rendering_notices(events, self.target_protocol) {
            push_unique_notice(&mut self.notices, notice);
        }
        let mut output = Vec::new();
        for event in events {
            match self.renderer.push(event) {
                Ok(bytes) => output.extend_from_slice(&bytes),
                Err(error) => {
                    self.failure = Some(error.clone());
                    output.extend_from_slice(&self.render_error(error));
                    break;
                }
            }
        }
        Bytes::from(output)
    }

    fn render_error(&mut self, error: StreamError) -> Bytes {
        let events = [
            CanonicalStreamEvent::Error(CanonicalError {
                code: error.code().to_string(),
                message: error.to_string(),
                retryable: false,
                provider_code: None,
                details: None,
            }),
            CanonicalStreamEvent::StatusUpdate(ResponseStatus::Failed),
            CanonicalStreamEvent::ResponseDone(FinishDetail {
                reason: FinishReason::Error,
                original_reason: Some(error.code().to_string()),
                incomplete_details: None,
            }),
        ];
        let mut output = Vec::new();
        for event in &events {
            if let Ok(bytes) = self.renderer.push(event) {
                output.extend_from_slice(&bytes);
            }
        }
        Bytes::from(output)
    }
}
