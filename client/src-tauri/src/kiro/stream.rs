//! Kiro V3 events become the same IR used by the existing HTTP/SSE adapters.
//! A metadata event is necessary, but success is emitted only after clean EOF:
//! metering and even an upstream error may follow it.
use super::event_stream::{Decoder, Frame};
use crate::protocol::ir::{
    ArtifactKind, BlockHeader, CanonicalStreamEvent as Event, ContentBlockKind, FinishDetail,
    FinishReason, ResponseMeta, ResponseStatus, ToolKind,
};
use anyhow::{Context, Result, anyhow, ensure};
use serde_json::Value;
use std::collections::BTreeMap;

struct Tool {
    index: u32,
    name: String,
    arguments: String,
    complete: bool,
}

pub(crate) struct StreamDecoder {
    frames: Decoder,
    model: String,
    response_id: String,
    started: bool,
    next_index: u32,
    text: Option<u32>,
    reasoning: Option<u32>,
    tools: BTreeMap<String, Tool>,
    terminal: bool,
    stop_reason: Option<String>,
    bytes: usize,
    credits: Option<f64>,
}

impl StreamDecoder {
    pub(crate) fn new(model: &str) -> Self {
        Self {
            frames: Decoder::default(),
            model: model.into(),
            response_id: format!("resp_{}", uuid::Uuid::new_v4().simple()),
            started: false,
            next_index: 0,
            text: None,
            reasoning: None,
            tools: BTreeMap::new(),
            terminal: false,
            stop_reason: None,
            bytes: 0,
            credits: None,
        }
    }

    pub(crate) fn credits(&self) -> Option<f64> {
        self.credits
    }

    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<Vec<Event>> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .filter(|n| *n <= crate::protocol::stream::MAX_STREAM_BYTES)
            .ok_or_else(|| anyhow!("Kiro response exceeded the stream size limit"))?;
        let mut events = Vec::new();
        for frame in self.frames.push(bytes)? {
            self.frame(frame, &mut events)?;
        }
        Ok(events)
    }

    fn start(&mut self, events: &mut Vec<Event>) {
        if !self.started {
            self.started = true;
            events.push(Event::ResponseStart(ResponseMeta {
                id: Some(self.response_id.clone()),
                model: Some(self.model.clone()),
                status: ResponseStatus::InProgress,
                provider_event_id: None,
            }));
        }
    }

    fn block(&mut self, kind: ContentBlockKind, events: &mut Vec<Event>) -> u32 {
        self.start(events);
        let index = self.next_index;
        self.next_index += 1;
        events.push(Event::BlockStart {
            index,
            block: BlockHeader {
                kind,
                source_item_id: None,
                call_id: None,
                name: None,
                tool_kind: None,
                tool_namespace: None,
                artifacts: Vec::new(),
            },
        });
        index
    }

    fn close_text_blocks(&mut self, events: &mut Vec<Event>) {
        for index in [self.text.take(), self.reasoning.take()]
            .into_iter()
            .flatten()
        {
            events.push(Event::BlockDone { index, block: None });
        }
    }

    fn frame(&mut self, frame: Frame, events: &mut Vec<Event>) -> Result<()> {
        let kind = frame.headers[":message-type"].as_str();
        if kind != "event" {
            let code = frame
                .headers
                .get(":exception-type")
                .or_else(|| frame.headers.get(":error-code"))
                .map(String::as_str)
                .unwrap_or("upstream_stream_error");
            let value = serde_json::from_slice::<Value>(&frame.payload).ok();
            let detail = value
                .as_ref()
                .map(|v| super::error_detail(v, &[]))
                .unwrap_or_else(|| {
                    crate::upstream_failure::safe_excerpt(
                        frame
                            .headers
                            .get(":error-message")
                            .map(String::as_str)
                            .unwrap_or("Kiro upstream stream failed"),
                    )
                });
            return Err(StreamFailure {
                code: code.into(),
                detail,
            }
            .into());
        }
        let name = frame
            .headers
            .get(":event-type")
            .map(String::as_str)
            .unwrap_or("");
        // Unknown events are opaque extension data, not successful termination.
        if !matches!(
            name,
            "initial-response"
                | "assistantResponseEvent"
                | "reasoningContentEvent"
                | "toolUseEvent"
                | "metadataEvent"
                | "meteringEvent"
        ) {
            return Ok(());
        }
        let value: Value =
            serde_json::from_slice(&frame.payload).context("invalid Kiro event JSON")?;
        ensure!(value.is_object(), "Kiro event payload must be an object");
        match name {
            "initial-response" => self.start(events),
            "metadataEvent" => {
                ensure!(!self.terminal, "duplicate Kiro terminal event");
                self.terminal = true;
                self.stop_reason = optional_string(&value, "stopReason")?.map(str::to_string);
            }
            "meteringEvent" => {
                let unit = optional_string(&value, "unit")?.unwrap_or("");
                if unit.eq_ignore_ascii_case("credit") || unit.eq_ignore_ascii_case("credits") {
                    let usage = value["usage"]
                        .as_f64()
                        .filter(|n| n.is_finite() && *n >= 0.0)
                        .ok_or_else(|| anyhow!("invalid Kiro credit metering"))?;
                    let total = self.credits.unwrap_or(0.0) + usage;
                    ensure!(total.is_finite(), "Kiro credit metering is out of range");
                    self.credits = Some(total);
                }
            }
            "assistantResponseEvent" => {
                ensure!(!self.terminal, "Kiro text arrived after termination");
                let text = optional_string(&value, "content")?.unwrap_or("");
                if !text.is_empty() {
                    if self.text.is_none() {
                        self.close_text_blocks(events);
                        self.text = Some(self.block(ContentBlockKind::Text, events));
                    }
                    events.push(Event::TextDelta {
                        index: self.text.unwrap(),
                        text: text.into(),
                    });
                }
            }
            "reasoningContentEvent" => {
                ensure!(!self.terminal, "Kiro reasoning arrived after termination");
                if self.reasoning.is_none() {
                    self.close_text_blocks(events);
                    self.reasoning = Some(self.block(ContentBlockKind::Reasoning, events));
                }
                let index = self.reasoning.unwrap();
                let text = optional_string(&value, "text")?.unwrap_or("");
                let signature = optional_string(&value, "signature")?;
                let redacted = optional_string(&value, "redactedContent")?;
                // GPT's signature is opaque reasoning; its accompanying text is
                // only a placeholder, not a plaintext reasoning transcript.
                let encrypted = self.model.to_ascii_lowercase().starts_with("gpt-");
                if !encrypted && !text.is_empty() {
                    events.push(Event::ReasoningDelta {
                        index,
                        text: text.into(),
                    });
                }
                if let Some(data) = signature.filter(|s| !s.is_empty()) {
                    events.push(Event::ArtifactDelta {
                        index,
                        kind: if encrypted {
                            ArtifactKind::OpenAiEncryptedReasoning
                        } else {
                            ArtifactKind::AnthropicThinkingSignature
                        },
                        data: data.into(),
                    });
                }
                if let Some(data) = redacted.filter(|s| !s.is_empty()) {
                    events.push(Event::ArtifactDelta {
                        index,
                        kind: if encrypted {
                            ArtifactKind::OpenAiEncryptedReasoning
                        } else {
                            ArtifactKind::AnthropicRedactedThinking
                        },
                        data: data.into(),
                    });
                }
            }
            "toolUseEvent" => {
                ensure!(!self.terminal, "Kiro tool call arrived after termination");
                let id = required_string(&value, "toolUseId")?;
                let name = required_string(&value, "name")?;
                let input = optional_string(&value, "input")?.unwrap_or("");
                let stop = match value.get("stop") {
                    None => false,
                    Some(Value::Bool(value)) => *value,
                    Some(_) => anyhow::bail!("Kiro tool stop flag must be boolean"),
                };
                if !self.tools.contains_key(id) {
                    self.close_text_blocks(events);
                    self.start(events);
                    let index = self.next_index;
                    self.next_index += 1;
                    self.tools.insert(
                        id.into(),
                        Tool {
                            index,
                            name: name.into(),
                            arguments: String::new(),
                            complete: false,
                        },
                    );
                    events.push(Event::BlockStart {
                        index,
                        block: BlockHeader {
                            kind: ContentBlockKind::ToolCall,
                            tool_kind: Some(ToolKind::Function),
                            tool_namespace: None,
                            source_item_id: None,
                            call_id: Some(id.into()),
                            name: Some(name.into()),
                            artifacts: Vec::new(),
                        },
                    });
                }
                let tool = self.tools.get_mut(id).expect("inserted above");
                ensure!(
                    !tool.complete && tool.name == name,
                    "Kiro tool identity changed or repeated after completion"
                );
                tool.arguments.push_str(input);
                if !input.is_empty() {
                    events.push(Event::ToolArgumentsDelta {
                        index: tool.index,
                        data: input.into(),
                    });
                }
                if stop {
                    // An absent input is a legitimate empty-parameter call.
                    if tool.arguments.is_empty() {
                        tool.arguments.push_str("{}");
                        events.push(Event::ToolArgumentsDelta {
                            index: tool.index,
                            data: "{}".into(),
                        });
                    }
                    let arguments: Value = serde_json::from_str(&tool.arguments)
                        .context("Kiro tool arguments are incomplete or invalid JSON")?;
                    ensure!(
                        arguments.is_object(),
                        "Kiro tool arguments must be an object"
                    );
                    tool.complete = true;
                    tool.arguments.clear();
                    events.push(Event::BlockDone {
                        index: tool.index,
                        block: None,
                    });
                }
            }
            _ => unreachable!(),
        }
        Ok(())
    }

    pub(crate) fn finish(&mut self) -> Result<Vec<Event>> {
        self.frames.finish()?;
        ensure!(
            self.terminal,
            "Kiro stream ended without a terminal metadata event"
        );
        ensure!(
            self.tools.values().all(|t| t.complete),
            "Kiro stream ended with an incomplete tool call"
        );
        let mut events = Vec::new();
        self.start(&mut events);
        self.close_text_blocks(&mut events);
        events.push(Event::ResponseDone(FinishDetail {
            reason: if !self.tools.is_empty() {
                FinishReason::ToolCalls
            } else if matches!(
                self.stop_reason.as_deref(),
                Some("MAX_TOKENS" | "max_tokens" | "MAX_OUTPUT_TOKENS")
            ) {
                FinishReason::Length
            } else {
                FinishReason::Stop
            },
            original_reason: self.stop_reason.clone(),
            incomplete_details: None,
        }));
        Ok(events)
    }
}

fn optional_string<'a>(value: &'a Value, field: &str) -> Result<Option<&'a str>> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        _ => anyhow::bail!("Kiro event {field} must be a string"),
    }
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    optional_string(value, field)?
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("Kiro event is missing {field}"))
}

#[derive(Debug)]
pub(crate) struct StreamFailure {
    pub(crate) code: String,
    detail: String,
}
impl std::fmt::Display for StreamFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Kiro stream {}: {}", self.code, self.detail)
    }
}
impl std::error::Error for StreamFailure {}
