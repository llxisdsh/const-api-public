use std::collections::{BTreeMap, HashMap};

use serde_json::Value;

use crate::protocol::ir::{
    ArtifactAffinity, ArtifactCriticality, ArtifactKind, BlockHeader, CanonicalError,
    CanonicalStreamEvent, ContentBlockKind, FinishDetail, FinishReason, OpaqueArtifact,
    ReplayPolicy, ResponseMeta, ResponseStatus, Usage, UsageProvenance, UsageValue,
};
use crate::protocol::kind::ProtocolKind;

use super::sse::{SseDecoder, SseFrame};
use super::{StreamConversionNotice, StreamError};

pub(crate) struct StreamParser {
    protocol: ProtocolKind,
    model: String,
    decoder: SseDecoder,
    started: bool,
    done: bool,
    next_index: u32,
    blocks: HashMap<String, u32>,
    open_blocks: BTreeMap<u32, ContentBlockKind>,
    tool_headers: HashMap<u32, BlockHeader>,
    notices: Vec<StreamConversionNotice>,
}

impl StreamParser {
    pub(crate) fn new(protocol: ProtocolKind, model: impl Into<String>) -> Self {
        Self {
            protocol,
            model: model.into(),
            decoder: SseDecoder::default(),
            started: false,
            done: false,
            next_index: 0,
            blocks: HashMap::new(),
            open_blocks: BTreeMap::new(),
            tool_headers: HashMap::new(),
            notices: Vec::new(),
        }
    }

    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<Vec<CanonicalStreamEvent>, StreamError> {
        let frames = self.decoder.push(bytes)?;
        self.parse_frames(frames)
    }

    pub(crate) fn finish(&mut self) -> Result<Vec<CanonicalStreamEvent>, StreamError> {
        let frames = self.decoder.finish()?;
        self.parse_frames(frames)
    }

    pub(crate) fn take_notices(&mut self) -> Vec<StreamConversionNotice> {
        std::mem::take(&mut self.notices)
    }

    fn parse_frames(
        &mut self,
        frames: Vec<SseFrame>,
    ) -> Result<Vec<CanonicalStreamEvent>, StreamError> {
        let mut events = Vec::new();
        for frame in frames {
            if self.done {
                continue;
            }
            if frame.data.trim() == "[DONE]" {
                let reason = if self.tool_headers.is_empty() {
                    FinishReason::Stop
                } else {
                    FinishReason::ToolCalls
                };
                self.finish_events(reason, None, Usage::default(), &mut events);
                continue;
            }
            let value = serde_json::from_str::<Value>(&frame.data).map_err(|error| {
                StreamError::new(
                    "stream_event_json_invalid",
                    format!("invalid SSE JSON payload: {error}"),
                )
            })?;
            if let Some((envelope, error)) = crate::upstream_failure::error_object(&value) {
                // Preserve precise skin error types before a protocol projection loses
                // its outer envelope (Responses) or choices error (Chat).
                let mut error = error.clone();
                let canonical = crate::openrouter::canonical_error_type(envelope, &error);
                if !canonical.is_empty() {
                    if let Some(object) = error.as_object_mut() {
                        object.insert("error_type".into(), canonical.into());
                    }
                }
                self.provider_error(&error, &mut events);
                continue;
            }
            if let Some(notice) =
                unknown_semantic_payload_notice(self.protocol, frame.event.as_deref(), &value)
            {
                if !self.notices.iter().any(|existing| {
                    existing.path == notice.path && existing.summary == notice.summary
                }) {
                    self.notices.push(notice);
                }
            }
            match self.protocol {
                ProtocolKind::OpenAiChat => self.parse_chat(&value, &mut events),
                ProtocolKind::OpenAiResponses => {
                    self.parse_responses(frame.event.as_deref(), &value, &mut events)
                }
                ProtocolKind::AnthropicMessages => self.parse_anthropic(&value, &mut events),
                ProtocolKind::GeminiNative => self.parse_gemini(&value, &mut events),
            }
        }
        Ok(events)
    }

    fn ensure_start(
        &mut self,
        id: Option<String>,
        model: Option<String>,
        provider_event_id: Option<String>,
        events: &mut Vec<CanonicalStreamEvent>,
    ) {
        if let Some(model) = model.filter(|model| !model.trim().is_empty()) {
            self.model = model;
        }
        if self.started {
            return;
        }
        self.started = true;
        events.push(CanonicalStreamEvent::ResponseStart(ResponseMeta {
            id,
            model: Some(self.model.clone()),
            status: ResponseStatus::InProgress,
            provider_event_id,
        }));
    }

    fn ensure_block(
        &mut self,
        key: impl Into<String>,
        header: BlockHeader,
        events: &mut Vec<CanonicalStreamEvent>,
    ) -> u32 {
        let key = key.into();
        if let Some(index) = self.blocks.get(&key).copied() {
            if header.kind == ContentBlockKind::ToolCall && !header.artifacts.is_empty() {
                if let Some(existing_header) = self.tool_headers.get_mut(&index) {
                    for artifact in header.artifacts {
                        if existing_header.artifacts.iter().any(|existing| {
                            existing.kind == artifact.kind && existing.payload == artifact.payload
                        }) {
                            continue;
                        }
                        if let Some(data) = artifact.payload.as_str() {
                            events.push(CanonicalStreamEvent::ArtifactDelta {
                                index,
                                kind: artifact.kind.clone(),
                                data: data.to_string(),
                            });
                            existing_header.artifacts.push(artifact);
                        } else {
                            self.notices.push(StreamConversionNotice {
                                code: "response_artifact_omitted_in_stream_adaptation",
                                path: format!("$.stream.blocks[{index}].artifacts"),
                                summary: "A late non-text tool-call artifact could not be represented by the canonical SSE bridge".to_string(),
                            });
                        }
                    }
                }
            }
            return index;
        }
        let index = self.next_index;
        self.next_index += 1;
        self.blocks.insert(key, index);
        self.open_blocks.insert(index, header.kind);
        if header.kind == ContentBlockKind::ToolCall {
            self.tool_headers.insert(index, header.clone());
        }
        events.push(CanonicalStreamEvent::BlockStart {
            index,
            block: header,
        });
        index
    }

    fn close_block(&mut self, index: u32, events: &mut Vec<CanonicalStreamEvent>) {
        if self.open_blocks.remove(&index).is_some() {
            events.push(CanonicalStreamEvent::BlockDone { index, block: None });
        }
    }

    fn finish_events(
        &mut self,
        reason: FinishReason,
        original_reason: Option<String>,
        usage: Usage,
        events: &mut Vec<CanonicalStreamEvent>,
    ) {
        if self.done {
            return;
        }
        self.ensure_start(None, None, None, events);
        let open = self.open_blocks.keys().copied().collect::<Vec<_>>();
        for index in open {
            self.close_block(index, events);
        }
        if usage != Usage::default() {
            events.push(CanonicalStreamEvent::UsageUpdate(Box::new(usage)));
        }
        let status = status_from_finish(reason);
        events.push(CanonicalStreamEvent::StatusUpdate(status));
        events.push(CanonicalStreamEvent::ResponseDone(FinishDetail {
            reason,
            original_reason,
            incomplete_details: None,
        }));
        self.done = true;
    }

    fn parse_chat(&mut self, value: &Value, events: &mut Vec<CanonicalStreamEvent>) {
        if let Some(error) = value.get("error") {
            self.provider_error(error, events);
            return;
        }
        self.ensure_start(
            value.get("id").and_then(Value::as_str).map(str::to_string),
            value
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string),
            None,
            events,
        );
        let Some(choice) = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
        else {
            return;
        };
        let delta = choice.get("delta").unwrap_or(&Value::Null);
        if let Some(text) = delta
            .get("content")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            let index = self.ensure_block("chat:text", text_header(), events);
            events.push(CanonicalStreamEvent::TextDelta {
                index,
                text: text.to_string(),
            });
        }
        if let Some(text) = delta
            .get("reasoning_content")
            .or_else(|| delta.get("reasoning"))
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            let index = self.ensure_block("chat:reasoning", reasoning_header(), events);
            events.push(CanonicalStreamEvent::ReasoningDelta {
                index,
                text: text.to_string(),
            });
        }
        if let Some(text) = delta
            .get("refusal")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            let index = self.ensure_block("chat:refusal", refusal_header(), events);
            events.push(CanonicalStreamEvent::RefusalDelta {
                index,
                text: text.to_string(),
            });
        }
        let shared_gemini_signature = delta
            .pointer("/extra_fields/google/thought_signature")
            .or_else(|| delta.pointer("/extra_fields/google/thoughtSignature"));
        for (tool_position, tool) in delta
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let wire_index = tool
                .get("index")
                .and_then(Value::as_u64)
                .unwrap_or(tool_position as u64) as u32;
            let function = tool.get("function").unwrap_or(&Value::Null);
            let key = format!("chat:tool:{wire_index}");
            let existing = self
                .blocks
                .get(&key)
                .and_then(|index| self.tool_headers.get(index));
            let mut header = tool_header(
                tool.get("id")
                    .and_then(Value::as_str)
                    .or_else(|| existing.and_then(|header| header.call_id.as_deref()))
                    .unwrap_or("call_stream"),
                function
                    .get("name")
                    .and_then(Value::as_str)
                    .or_else(|| existing.and_then(|header| header.name.as_deref()))
                    .unwrap_or("tool"),
                None,
            );
            let signature = tool
                .pointer("/extra_content/google/thought_signature")
                .or_else(|| tool.pointer("/extra_content/google/thoughtSignature"))
                .or_else(|| {
                    (tool_position == 0)
                        .then_some(shared_gemini_signature)
                        .flatten()
                });
            if let Some(signature) = signature.cloned() {
                header
                    .artifacts
                    .push(gemini_signature_artifact(signature, true, &self.model));
            }
            let index = self.ensure_block(key, header, events);
            if let Some(data) = function
                .get("arguments")
                .and_then(Value::as_str)
                .filter(|data| !data.is_empty())
            {
                events.push(CanonicalStreamEvent::ToolArgumentsDelta {
                    index,
                    data: data.to_string(),
                });
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish_events(
                finish_from_chat(reason),
                Some(reason.to_string()),
                usage_from_chat(value.get("usage")),
                events,
            );
        }
    }

    fn parse_responses(
        &mut self,
        event_name: Option<&str>,
        value: &Value,
        events: &mut Vec<CanonicalStreamEvent>,
    ) {
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .or(event_name)
            .unwrap_or_default();
        if kind == "error" || kind == "response.failed" {
            let error = value
                .get("error")
                .filter(|error| error.is_object())
                .or_else(|| {
                    value
                        .pointer("/response/error")
                        .filter(|error| error.is_object())
                })
                .or_else(|| value.pointer("/response/status_details/error"))
                .unwrap_or(value);
            self.provider_error(error, events);
            return;
        }
        if let Some(response) = value.get("response") {
            self.ensure_start(
                response
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                response
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                value
                    .get("event_id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                events,
            );
        } else {
            self.ensure_start(None, None, None, events);
        }
        match kind {
            "response.output_text.delta" => {
                if let Some(text) = value.get("delta").and_then(Value::as_str) {
                    let wire_index = value
                        .get("output_index")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let index = self.ensure_block(
                        format!("responses:text:{wire_index}"),
                        text_header(),
                        events,
                    );
                    events.push(CanonicalStreamEvent::TextDelta {
                        index,
                        text: text.to_string(),
                    });
                }
            }
            "response.refusal.delta" => {
                if let Some(text) = value.get("delta").and_then(Value::as_str) {
                    let index = self.ensure_block("responses:refusal", refusal_header(), events);
                    events.push(CanonicalStreamEvent::RefusalDelta {
                        index,
                        text: text.to_string(),
                    });
                }
            }
            "response.reasoning_text.delta" => {
                if let Some(text) = value.get("delta").and_then(Value::as_str) {
                    let index =
                        self.ensure_block("responses:reasoning", reasoning_header(), events);
                    events.push(CanonicalStreamEvent::ReasoningDelta {
                        index,
                        text: text.to_string(),
                    });
                }
            }
            "response.reasoning_summary_text.delta" => {
                if let Some(text) = value.get("delta").and_then(Value::as_str) {
                    let index =
                        self.ensure_block("responses:reasoning", reasoning_header(), events);
                    events.push(CanonicalStreamEvent::ReasoningSummaryDelta {
                        index,
                        part: value
                            .get("summary_index")
                            .and_then(Value::as_u64)
                            .unwrap_or(0) as u32,
                        text: text.to_string(),
                    });
                }
            }
            "response.output_item.added" => {
                let item = value.get("item").unwrap_or(&Value::Null);
                if matches!(
                    item.get("type").and_then(Value::as_str),
                    Some("function_call" | "custom_tool_call")
                ) {
                    let wire_index = value
                        .get("output_index")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    self.ensure_block(
                        format!("responses:tool:{wire_index}"),
                        responses_tool_header(item),
                        events,
                    );
                }
            }
            "response.function_call_arguments.delta" | "response.custom_tool_call_input.delta" => {
                let wire_index = value
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let index = self.ensure_block(
                    format!("responses:tool:{wire_index}"),
                    tool_header(
                        value
                            .get("call_id")
                            .or_else(|| value.get("item_id"))
                            .and_then(Value::as_str)
                            .unwrap_or("call_stream"),
                        value.get("name").and_then(Value::as_str).unwrap_or("tool"),
                        value
                            .get("item_id")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    ),
                    events,
                );
                if let Some(data) = value.get("delta").and_then(Value::as_str) {
                    events.push(CanonicalStreamEvent::ToolArgumentsDelta {
                        index,
                        data: data.to_string(),
                    });
                }
            }
            "response.completed" | "response.incomplete" | "response.cancelled" => {
                let response = value.get("response").unwrap_or(&Value::Null);
                self.parse_responses_completed_output(response, events);
                let status = response
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or(kind.strip_prefix("response.").unwrap_or("completed"));
                let reason = match status {
                    "incomplete" => FinishReason::Length,
                    "cancelled" => FinishReason::Cancelled,
                    "failed" => FinishReason::Error,
                    _ if !self.tool_headers.is_empty() => FinishReason::ToolCalls,
                    _ => FinishReason::Stop,
                };
                self.finish_events(
                    reason,
                    Some(status.to_string()),
                    usage_from_responses(response.get("usage")),
                    events,
                );
            }
            _ => {}
        }
    }

    fn parse_responses_completed_output(
        &mut self,
        response: &Value,
        events: &mut Vec<CanonicalStreamEvent>,
    ) {
        for (wire_index, item) in response
            .get("output")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            match item.get("type").and_then(Value::as_str) {
                Some("message") => {
                    let key = format!("responses:text:{wire_index}");
                    if self.blocks.contains_key(&key) {
                        continue;
                    }
                    let text = item
                        .get("content")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|part| {
                            part.get("text")
                                .or_else(|| part.get("output_text"))
                                .and_then(Value::as_str)
                        })
                        .collect::<String>();
                    if !text.is_empty() {
                        let index = self.ensure_block(key, text_header(), events);
                        events.push(CanonicalStreamEvent::TextDelta { index, text });
                    }
                }
                Some("reasoning") => {
                    let key = "responses:reasoning".to_string();
                    let existed = self.blocks.contains_key(&key);
                    let index = self.ensure_block(key, reasoning_header(), events);
                    if !existed {
                        let text = item
                            .get("content")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .filter_map(|part| part.get("text").and_then(Value::as_str))
                            .collect::<String>();
                        if !text.is_empty() {
                            events.push(CanonicalStreamEvent::ReasoningDelta { index, text });
                        }
                        for (part, summary) in item
                            .get("summary")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .filter_map(|part| part.get("text").and_then(Value::as_str))
                            .enumerate()
                        {
                            events.push(CanonicalStreamEvent::ReasoningSummaryDelta {
                                index,
                                part: part as u32,
                                text: summary.to_string(),
                            });
                        }
                    }
                    if let Some(encrypted) = item.get("encrypted_content") {
                        if let Some(data) = encrypted.as_str() {
                            events.push(CanonicalStreamEvent::ArtifactDelta {
                                index,
                                kind: ArtifactKind::OpenAiEncryptedReasoning,
                                data: data.to_string(),
                            });
                        } else if !encrypted.is_null() {
                            self.notices.push(StreamConversionNotice {
                                code: "response_artifact_omitted_in_stream_adaptation",
                                path: format!(
                                    "$.response.output[{wire_index}].encrypted_content"
                                ),
                                summary: "Non-text encrypted reasoning could not be represented by the canonical SSE bridge".to_string(),
                            });
                        }
                    }
                }
                Some("function_call" | "custom_tool_call") => {
                    let key = format!("responses:tool:{wire_index}");
                    if self.blocks.contains_key(&key) {
                        continue;
                    }
                    let index = self.ensure_block(key, responses_tool_header(item), events);
                    let arguments =
                        item.get("arguments")
                            .or_else(|| item.get("input"))
                            .map(|value| match value {
                                Value::String(text) => text.clone(),
                                value => serde_json::to_string(value)
                                    .unwrap_or_else(|_| "{}".to_string()),
                            })
                            .unwrap_or_else(|| "{}".to_string());
                    events.push(CanonicalStreamEvent::ToolArgumentsDelta {
                        index,
                        data: arguments,
                    });
                }
                _ => {}
            }
        }
    }

    fn parse_anthropic(&mut self, value: &Value, events: &mut Vec<CanonicalStreamEvent>) {
        match value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "error" => self.provider_error(value.get("error").unwrap_or(value), events),
            "message_start" => {
                let message = value.get("message").unwrap_or(&Value::Null);
                let usage = usage_from_anthropic(message.get("usage"), None);
                if usage != Usage::default() {
                    events.push(CanonicalStreamEvent::UsageUpdate(Box::new(usage)));
                }
                self.ensure_start(
                    message
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    message
                        .get("model")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    None,
                    events,
                );
            }
            "content_block_start" => {
                self.ensure_start(None, None, None, events);
                let wire_index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
                let block = value.get("content_block").unwrap_or(&Value::Null);
                let (key, header) = match block.get("type").and_then(Value::as_str) {
                    Some("thinking") => (format!("anthropic:{wire_index}"), reasoning_header()),
                    Some("redacted_thinking") => {
                        let mut header = reasoning_header();
                        if let Some(data) =
                            block.get("data").filter(|data| !data.is_null()).cloned()
                        {
                            header.artifacts.push(anthropic_artifact(
                                ArtifactKind::AnthropicRedactedThinking,
                                data,
                                &self.model,
                            ));
                        }
                        (format!("anthropic:{wire_index}"), header)
                    }
                    Some("tool_use") => (
                        format!("anthropic:{wire_index}"),
                        tool_header(
                            block
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or("call_stream"),
                            block.get("name").and_then(Value::as_str).unwrap_or("tool"),
                            None,
                        ),
                    ),
                    _ => (format!("anthropic:{wire_index}"), text_header()),
                };
                let index = self.ensure_block(key, header, events);
                if let Some(text) = block
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|v| !v.is_empty())
                {
                    events.push(CanonicalStreamEvent::TextDelta {
                        index,
                        text: text.to_string(),
                    });
                }
                if let Some(thinking) = block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .filter(|v| !v.is_empty())
                {
                    events.push(CanonicalStreamEvent::ReasoningDelta {
                        index,
                        text: thinking.to_string(),
                    });
                }
                if block.get("type").and_then(Value::as_str) == Some("thinking") {
                    if let Some(signature) = block
                        .get("signature")
                        .and_then(Value::as_str)
                        .filter(|signature| !signature.is_empty())
                    {
                        events.push(CanonicalStreamEvent::ArtifactDelta {
                            index,
                            kind: ArtifactKind::AnthropicThinkingSignature,
                            data: signature.to_string(),
                        });
                    }
                }
            }
            "content_block_delta" => {
                let wire_index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
                let key = format!("anthropic:{wire_index}");
                let delta = value.get("delta").unwrap_or(&Value::Null);
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        let index = self.ensure_block(key, text_header(), events);
                        if let Some(text) = delta.get("text").and_then(Value::as_str) {
                            events.push(CanonicalStreamEvent::TextDelta {
                                index,
                                text: text.to_string(),
                            });
                        }
                    }
                    Some("thinking_delta") => {
                        let index = self.ensure_block(key, reasoning_header(), events);
                        if let Some(text) = delta.get("thinking").and_then(Value::as_str) {
                            events.push(CanonicalStreamEvent::ReasoningDelta {
                                index,
                                text: text.to_string(),
                            });
                        }
                    }
                    Some("signature_delta") => {
                        let index = self.ensure_block(key, reasoning_header(), events);
                        if let Some(data) = delta.get("signature").and_then(Value::as_str) {
                            events.push(CanonicalStreamEvent::ArtifactDelta {
                                index,
                                kind: ArtifactKind::AnthropicThinkingSignature,
                                data: data.to_string(),
                            });
                        }
                    }
                    Some("input_json_delta") => {
                        let index = self.ensure_block(
                            key,
                            tool_header("call_stream", "tool", None),
                            events,
                        );
                        if let Some(data) = delta.get("partial_json").and_then(Value::as_str) {
                            events.push(CanonicalStreamEvent::ToolArgumentsDelta {
                                index,
                                data: data.to_string(),
                            });
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let wire_index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
                if let Some(index) = self.blocks.get(&format!("anthropic:{wire_index}")).copied() {
                    self.close_block(index, events);
                }
            }
            "message_delta" => {
                let original = value.pointer("/delta/stop_reason").and_then(Value::as_str);
                self.finish_events(
                    finish_from_anthropic(original),
                    original.map(str::to_string),
                    usage_from_anthropic(None, value.get("usage")),
                    events,
                );
            }
            "message_stop" => self.finish_events(
                if self.tool_headers.is_empty() {
                    FinishReason::Stop
                } else {
                    FinishReason::ToolCalls
                },
                None,
                Usage::default(),
                events,
            ),
            _ => {}
        }
    }

    fn parse_gemini(&mut self, value: &Value, events: &mut Vec<CanonicalStreamEvent>) {
        let response = value.get("response").unwrap_or(value);
        if let Some(error) = response.get("error") {
            self.provider_error(error, events);
            return;
        }
        self.ensure_start(
            response
                .get("responseId")
                .and_then(Value::as_str)
                .map(str::to_string),
            response
                .get("modelVersion")
                .and_then(Value::as_str)
                .map(str::to_string),
            None,
            events,
        );
        let Some(candidate) = response
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|candidates| candidates.first())
        else {
            return;
        };
        for (part_index, part) in candidate
            .pointer("/content/parts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let signature = part
                .get("thoughtSignature")
                .or_else(|| part.get("thought_signature"))
                .cloned();
            if part.get("thought").and_then(Value::as_bool) == Some(true) {
                let index = self.ensure_block("gemini:reasoning", reasoning_header(), events);
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    events.push(CanonicalStreamEvent::ReasoningDelta {
                        index,
                        text: text.to_string(),
                    });
                }
                if let Some(data) = signature.as_ref().and_then(Value::as_str) {
                    events.push(CanonicalStreamEvent::ArtifactDelta {
                        index,
                        kind: ArtifactKind::GeminiThoughtSignature,
                        data: data.to_string(),
                    });
                }
                continue;
            }
            if let Some(call) = part
                .get("functionCall")
                .or_else(|| part.get("function_call"))
            {
                let key = format!("gemini:tool:{part_index}");
                let mut header = tool_header(
                    call.get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("call_gemini_stream"),
                    call.get("name").and_then(Value::as_str).unwrap_or("tool"),
                    None,
                );
                if let Some(signature) = signature.clone() {
                    header
                        .artifacts
                        .push(gemini_signature_artifact(signature, true, &self.model));
                }
                let index = self.ensure_block(key, header, events);
                let arguments = call
                    .get("args")
                    .or_else(|| call.get("arguments"))
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                events.push(CanonicalStreamEvent::ToolArgumentsDelta {
                    index,
                    data: serde_json::to_string(&arguments).unwrap_or_else(|_| "{}".to_string()),
                });
            } else if let Some(text) = part.get("text").and_then(Value::as_str) {
                // Empty deltas are placeholders, not assistant history. Gemini
                // may also stream a signature with empty text; keep that state.
                if text.is_empty() && signature.is_none() {
                    continue;
                }
                let index = self.ensure_block("gemini:text", text_header(), events);
                events.push(CanonicalStreamEvent::TextDelta {
                    index,
                    text: text.to_string(),
                });
                if let Some(data) = signature.as_ref().and_then(Value::as_str) {
                    events.push(CanonicalStreamEvent::ArtifactDelta {
                        index,
                        kind: ArtifactKind::GeminiThoughtSignature,
                        data: data.to_string(),
                    });
                }
            }
        }
        if let Some(original) = candidate.get("finishReason").and_then(Value::as_str) {
            self.finish_events(
                finish_from_gemini(original, !self.tool_headers.is_empty()),
                Some(original.to_string()),
                usage_from_gemini(response.get("usageMetadata")),
                events,
            );
        }
    }

    fn provider_error(&mut self, value: &Value, events: &mut Vec<CanonicalStreamEvent>) {
        self.ensure_start(None, None, None, events);
        events.push(CanonicalStreamEvent::Error(CanonicalError {
            code: ["type", "status", "code"]
                .into_iter()
                .find_map(|key| value.get(key).and_then(Value::as_str))
                .unwrap_or("provider_error")
                .to_string(),
            message: value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("upstream stream error")
                .to_string(),
            retryable: false,
            provider_code: value.get("code").map(|code| {
                code.as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| code.to_string())
            }),
            details: Some(value.clone()),
        }));
        self.finish_events(
            FinishReason::Error,
            Some("provider_error".to_string()),
            Usage::default(),
            events,
        );
    }
}

fn unknown_semantic_payload_notice(
    protocol: ProtocolKind,
    event_name: Option<&str>,
    value: &Value,
) -> Option<StreamConversionNotice> {
    let unsupported = match protocol {
        ProtocolKind::OpenAiChat => value
            .pointer("/choices/0/delta")
            .and_then(Value::as_object)
            .and_then(|delta| {
                delta.keys().find(|name| {
                    !matches!(
                        name.as_str(),
                        "role"
                            | "content"
                            | "reasoning_content"
                            | "reasoning"
                            | "refusal"
                            | "tool_calls"
                    ) && delta
                        .get(name.as_str())
                        .is_some_and(|value| !value.is_null())
                })
            })
            .map(|name| format!("OpenAI Chat delta field {name}")),
        ProtocolKind::OpenAiResponses => {
            let kind = value
                .get("type")
                .and_then(Value::as_str)
                .or(event_name)
                .unwrap_or_default();
            let known = matches!(
                kind,
                "error"
                    | "response.created"
                    | "response.in_progress"
                    | "response.output_item.added"
                    | "response.output_item.done"
                    | "response.content_part.added"
                    | "response.content_part.done"
                    | "response.output_text.delta"
                    | "response.output_text.done"
                    | "response.refusal.delta"
                    | "response.refusal.done"
                    | "response.reasoning_text.delta"
                    | "response.reasoning_text.done"
                    | "response.reasoning_summary_part.added"
                    | "response.reasoning_summary_part.done"
                    | "response.reasoning_summary_text.delta"
                    | "response.reasoning_summary_text.done"
                    | "response.function_call_arguments.delta"
                    | "response.function_call_arguments.done"
                    | "response.custom_tool_call_input.delta"
                    | "response.custom_tool_call_input.done"
                    | "response.completed"
                    | "response.incomplete"
                    | "response.cancelled"
                    | "response.failed"
            );
            (!known
                && has_semantic_member(
                    value,
                    &["delta", "item", "part", "content", "output", "response"],
                ))
            .then(|| format!("OpenAI Responses event {kind}"))
        }
        ProtocolKind::AnthropicMessages => {
            let kind = value
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if kind == "content_block_delta" {
                let delta_kind = value.pointer("/delta/type").and_then(Value::as_str);
                (!matches!(
                    delta_kind,
                    Some("text_delta" | "thinking_delta" | "signature_delta" | "input_json_delta")
                ))
                .then(|| {
                    format!(
                        "Anthropic content delta {}",
                        delta_kind.unwrap_or("<missing>")
                    )
                })
            } else if kind == "content_block_start" {
                let block_kind = value.pointer("/content_block/type").and_then(Value::as_str);
                (!matches!(
                    block_kind,
                    Some("text" | "thinking" | "redacted_thinking" | "tool_use")
                ))
                .then(|| {
                    format!(
                        "Anthropic content block {}",
                        block_kind.unwrap_or("<missing>")
                    )
                })
            } else {
                let known = matches!(
                    kind,
                    "error"
                        | "message_start"
                        | "content_block_stop"
                        | "message_delta"
                        | "message_stop"
                        | "ping"
                );
                (!known
                    && has_semantic_member(
                        value,
                        &["delta", "content_block", "message", "content"],
                    ))
                .then(|| format!("Anthropic event {kind}"))
            }
        }
        ProtocolKind::GeminiNative => value
            .pointer("/response/candidates/0/content/parts")
            .or_else(|| value.pointer("/candidates/0/content/parts"))
            .and_then(Value::as_array)
            .and_then(|parts| {
                parts.iter().find(|part| {
                    part.as_object().is_some_and(|part| {
                        !part.contains_key("text")
                            && !part.contains_key("functionCall")
                            && !part.contains_key("function_call")
                            && part.get("thought").and_then(Value::as_bool) != Some(true)
                    })
                })
            })
            .map(|_| "Gemini response part".to_string()),
    };

    let kind = unsupported?;
    let event = event_name
        .or_else(|| value.get("type").and_then(Value::as_str))
        .unwrap_or("<none>");
    eprintln!(
        "[const-api][protocol] omitted unknown stream payload during adaptation protocol={} event={} detail={}",
        protocol.as_str(),
        event,
        kind
    );
    Some(StreamConversionNotice {
        code: "stream_payload_approximated",
        path: format!("$.stream[{event}]"),
        summary: format!(
            "Unrecognized {kind} could not be represented during stream adaptation and was omitted so the response could continue"
        ),
    })
}

fn has_semantic_member(value: &Value, names: &[&str]) -> bool {
    names
        .iter()
        .any(|name| value.get(*name).is_some_and(|value| !value.is_null()))
}

fn text_header() -> BlockHeader {
    BlockHeader {
        tool_kind: None,
        tool_namespace: None,
        kind: ContentBlockKind::Text,
        source_item_id: None,
        call_id: None,
        name: None,
        artifacts: Vec::new(),
    }
}

fn reasoning_header() -> BlockHeader {
    BlockHeader {
        tool_kind: None,
        tool_namespace: None,
        kind: ContentBlockKind::Reasoning,
        source_item_id: None,
        call_id: None,
        name: None,
        artifacts: Vec::new(),
    }
}

fn refusal_header() -> BlockHeader {
    BlockHeader {
        tool_kind: None,
        tool_namespace: None,
        kind: ContentBlockKind::Refusal,
        source_item_id: None,
        call_id: None,
        name: None,
        artifacts: Vec::new(),
    }
}

fn tool_header(id: &str, name: &str, source_item_id: Option<String>) -> BlockHeader {
    BlockHeader {
        tool_kind: None,
        tool_namespace: None,
        kind: ContentBlockKind::ToolCall,
        source_item_id,
        call_id: Some(id.to_string()),
        name: Some(name.to_string()),
        artifacts: Vec::new(),
    }
}

fn responses_tool_header(item: &Value) -> BlockHeader {
    let mut header = tool_header(
        item.get("call_id")
            .or_else(|| item.get("id"))
            .and_then(Value::as_str)
            .unwrap_or("call_stream"),
        item.get("name").and_then(Value::as_str).unwrap_or("tool"),
        item.get("id").and_then(Value::as_str).map(str::to_string),
    );
    header.tool_kind = Some(
        if item.get("type").and_then(Value::as_str) == Some("custom_tool_call") {
            crate::protocol::ir::ToolKind::Custom
        } else {
            crate::protocol::ir::ToolKind::Function
        },
    );
    header.tool_namespace = item
        .get("namespace")
        .and_then(Value::as_str)
        .map(str::to_string);
    header
}

fn gemini_signature_artifact(payload: Value, required: bool, model: &str) -> OpaqueArtifact {
    OpaqueArtifact {
        kind: ArtifactKind::GeminiThoughtSignature,
        payload,
        affinity: ArtifactAffinity::ProtocolModel {
            protocol: ProtocolKind::GeminiNative,
            model: model.to_string(),
        },
        replay: if required {
            ReplayPolicy::Required
        } else {
            ReplayPolicy::ExactWhenCompatible
        },
        criticality: if required {
            ArtifactCriticality::Required
        } else {
            ArtifactCriticality::Supplemental
        },
    }
}

fn anthropic_artifact(kind: ArtifactKind, payload: Value, model: &str) -> OpaqueArtifact {
    OpaqueArtifact {
        kind,
        payload,
        affinity: ArtifactAffinity::ProtocolModel {
            protocol: ProtocolKind::AnthropicMessages,
            model: model.to_string(),
        },
        replay: ReplayPolicy::Required,
        criticality: ArtifactCriticality::Required,
    }
}

fn reported(value: Option<u64>) -> UsageValue {
    UsageValue {
        value,
        source: if value.is_some() {
            UsageProvenance::Reported
        } else {
            UsageProvenance::Unavailable
        },
    }
}

fn usage_from_chat(value: Option<&Value>) -> Usage {
    let value = value.unwrap_or(&Value::Null);
    Usage {
        input_tokens: reported(value.get("prompt_tokens").and_then(Value::as_u64)),
        output_tokens: reported(value.get("completion_tokens").and_then(Value::as_u64)),
        total_tokens: reported(value.get("total_tokens").and_then(Value::as_u64)),
        cache_read_tokens: reported(
            value
                .pointer("/prompt_tokens_details/cached_tokens")
                .and_then(Value::as_u64),
        ),
        cache_write_tokens: reported(
            value
                .pointer("/prompt_tokens_details/cache_write_tokens")
                .and_then(Value::as_u64),
        ),
        reasoning_tokens: reported(
            value
                .pointer("/completion_tokens_details/reasoning_tokens")
                .and_then(Value::as_u64),
        ),
        ..Usage::default()
    }
    .with_inclusive_reasoning_output()
}

fn usage_from_responses(value: Option<&Value>) -> Usage {
    let value = value.unwrap_or(&Value::Null);
    Usage {
        input_tokens: reported(
            value
                .get("input_tokens")
                .or_else(|| value.get("prompt_tokens"))
                .and_then(Value::as_u64),
        ),
        output_tokens: reported(
            value
                .get("output_tokens")
                .or_else(|| value.get("completion_tokens"))
                .and_then(Value::as_u64),
        ),
        total_tokens: reported(value.get("total_tokens").and_then(Value::as_u64)),
        cache_read_tokens: reported(
            value
                .pointer("/input_tokens_details/cached_tokens")
                .or_else(|| value.pointer("/prompt_tokens_details/cached_tokens"))
                .and_then(Value::as_u64),
        ),
        cache_write_tokens: reported(
            value
                .pointer("/input_tokens_details/cache_write_tokens")
                .or_else(|| value.pointer("/prompt_tokens_details/cache_write_tokens"))
                .and_then(Value::as_u64),
        ),
        reasoning_tokens: reported(
            value
                .pointer("/output_tokens_details/reasoning_tokens")
                .or_else(|| value.pointer("/completion_tokens_details/reasoning_tokens"))
                .and_then(Value::as_u64),
        ),
        ..Usage::default()
    }
    .with_inclusive_reasoning_output()
}

fn usage_from_anthropic(start: Option<&Value>, delta: Option<&Value>) -> Usage {
    let start = start.unwrap_or(&Value::Null);
    let delta = delta.unwrap_or(&Value::Null);
    let input = start.get("input_tokens").and_then(Value::as_u64);
    let output = delta
        .get("output_tokens")
        .or_else(|| start.get("output_tokens"))
        .and_then(Value::as_u64);
    let cache_creation = reported(
        start
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64),
    );
    Usage {
        input_tokens: reported(input),
        output_tokens: reported(output),
        total_tokens: reported(input.zip(output).map(|(a, b)| a + b)),
        cache_read_tokens: reported(start.get("cache_read_input_tokens").and_then(Value::as_u64)),
        cache_write_tokens: cache_creation.clone(),
        cache_creation_tokens: cache_creation,
        reasoning_tokens: reported(
            delta
                .pointer("/output_tokens_details/thinking_tokens")
                .or_else(|| start.pointer("/output_tokens_details/thinking_tokens"))
                .and_then(Value::as_u64),
        ),
        ..Usage::default()
    }
}

fn usage_from_gemini(value: Option<&Value>) -> Usage {
    let value = value.unwrap_or(&Value::Null);
    Usage {
        input_tokens: reported(value.get("promptTokenCount").and_then(Value::as_u64)),
        output_tokens: reported(value.get("candidatesTokenCount").and_then(Value::as_u64)),
        total_tokens: reported(value.get("totalTokenCount").and_then(Value::as_u64)),
        cache_read_tokens: reported(value.get("cachedContentTokenCount").and_then(Value::as_u64)),
        reasoning_tokens: reported(value.get("thoughtsTokenCount").and_then(Value::as_u64)),
        ..Usage::default()
    }
    .with_separate_reasoning_output()
}

fn finish_from_chat(reason: &str) -> FinishReason {
    match reason {
        "tool_calls" | "function_call" => FinishReason::ToolCalls,
        "length" => FinishReason::Length,
        "content_filter" => FinishReason::ContentFilter,
        _ => FinishReason::Stop,
    }
}

fn finish_from_anthropic(reason: Option<&str>) -> FinishReason {
    match reason {
        Some("tool_use") => FinishReason::ToolCalls,
        Some("max_tokens") => FinishReason::Length,
        Some("refusal") => FinishReason::Refusal,
        Some("pause_turn") => FinishReason::PauseTurn,
        _ => FinishReason::Stop,
    }
}

fn finish_from_gemini(reason: &str, has_tools: bool) -> FinishReason {
    match reason {
        "MAX_TOKENS" => FinishReason::Length,
        "SAFETY" | "RECITATION" | "PROHIBITED_CONTENT" => FinishReason::ContentFilter,
        "FUNCTION_CALL" => FinishReason::ToolCalls,
        "STOP" if has_tools => FinishReason::ToolCalls,
        "STOP" => FinishReason::Stop,
        _ => FinishReason::Unknown,
    }
}

fn status_from_finish(reason: FinishReason) -> ResponseStatus {
    match reason {
        FinishReason::Stop | FinishReason::ToolCalls => ResponseStatus::Completed,
        FinishReason::PauseTurn => ResponseStatus::InProgress,
        FinishReason::Length => ResponseStatus::Incomplete,
        FinishReason::ContentFilter | FinishReason::Refusal => ResponseStatus::Refused,
        FinishReason::Error => ResponseStatus::Failed,
        FinishReason::Cancelled => ResponseStatus::Cancelled,
        FinishReason::Unknown => ResponseStatus::InProgress,
    }
}

#[cfg(test)]
mod usage_tests {
    use super::*;

    #[test]
    fn chat_stream_without_tool_indices_keeps_parallel_calls_and_terminal_reason() {
        let raw = concat!(
            "data: {\"id\":\"chatcmpl-tools\",\"model\":\"model\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[",
            "{\"id\":\"call_a\",\"type\":\"function\",\"function\":{\"name\":\"read_a\",\"arguments\":\"{}\"}},",
            "{\"id\":\"call_b\",\"type\":\"function\",\"function\":{\"name\":\"read_b\",\"arguments\":\"{}\"}}",
            "]},\"finish_reason\":null}]}\n\n",
            "data: [DONE]\n\n"
        );
        let mut parser = StreamParser::new(ProtocolKind::OpenAiChat, "model");
        let events = parser.push(raw.as_bytes()).unwrap();
        let calls = events
            .iter()
            .filter_map(|event| match event {
                CanonicalStreamEvent::BlockStart { index, block }
                    if block.kind == ContentBlockKind::ToolCall =>
                {
                    Some((*index, block.call_id.as_deref(), block.name.as_deref()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            calls,
            vec![
                (0, Some("call_a"), Some("read_a")),
                (1, Some("call_b"), Some("read_b"))
            ]
        );
        assert!(events.iter().any(|event| matches!(
            event,
            CanonicalStreamEvent::ResponseDone(FinishDetail {
                reason: FinishReason::ToolCalls,
                ..
            })
        )));
    }

    #[test]
    fn openai_stream_usage_preserves_cache_write_tokens() {
        let chat = usage_from_chat(Some(&serde_json::json!({
            "prompt_tokens": 20,
            "completion_tokens": 2,
            "prompt_tokens_details": {
                "cached_tokens": 8,
                "cache_write_tokens": 6
            }
        })));
        assert_eq!(chat.cache_read_tokens.value, Some(8));
        assert_eq!(chat.cache_write_tokens.value, Some(6));

        let responses = usage_from_responses(Some(&serde_json::json!({
            "input_tokens": 30,
            "output_tokens": 3,
            "input_tokens_details": {
                "cached_tokens": 12,
                "cache_write_tokens": 9
            }
        })));
        assert_eq!(responses.cache_read_tokens.value, Some(12));
        assert_eq!(responses.cache_write_tokens.value, Some(9));
    }
}
