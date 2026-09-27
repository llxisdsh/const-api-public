use std::collections::{BTreeMap, HashMap, HashSet};

use bytes::Bytes;
use serde_json::{Value, json};

use crate::protocol::continuation::{
    encode_carrier, previous_part_entries, reasoning_entries, stream_artifact, tool_call_entries,
};
use crate::protocol::ir::{
    ArtifactKind, BlockHeader, CanonicalError, CanonicalStreamEvent, ContentBlockKind,
    FinishDetail, FinishReason, ResponseMeta, ToolKind, Usage,
};
use crate::protocol::kind::ProtocolKind;

use super::{StreamError, merge_usage};

pub(crate) struct StreamRenderer {
    protocol: ProtocolKind,
    model: String,
    response_id: String,
    started: bool,
    done: bool,
    failed: bool,
    blocks: HashMap<u32, BlockHeader>,
    chat_tool_indices: HashMap<u32, u32>,
    next_chat_tool_index: u32,
    tool_arguments: BTreeMap<u32, String>,
    block_text: BTreeMap<u32, String>,
    block_artifacts: BTreeMap<u32, Vec<(ArtifactKind, Value)>>,
    done_blocks: HashSet<u32>,
    response_output: BTreeMap<u32, Value>,
    wire_indices: HashMap<u32, u32>,
    next_wire_index: u32,
    usage: Usage,
}

impl StreamRenderer {
    pub(crate) fn new(protocol: ProtocolKind, model: impl Into<String>) -> Self {
        Self {
            protocol,
            model: model.into(),
            response_id: "resp_const_api_stream".to_string(),
            started: false,
            done: false,
            failed: false,
            blocks: HashMap::new(),
            chat_tool_indices: HashMap::new(),
            next_chat_tool_index: 0,
            tool_arguments: BTreeMap::new(),
            block_text: BTreeMap::new(),
            block_artifacts: BTreeMap::new(),
            done_blocks: HashSet::new(),
            response_output: BTreeMap::new(),
            wire_indices: HashMap::new(),
            next_wire_index: 0,
            usage: Usage::default(),
        }
    }

    pub(crate) fn push(&mut self, event: &CanonicalStreamEvent) -> Result<Bytes, StreamError> {
        if self.done {
            return Ok(Bytes::new());
        }
        let mut output = Vec::new();
        match event {
            CanonicalStreamEvent::ResponseStart(meta) => self.start(meta, &mut output),
            CanonicalStreamEvent::BlockStart { index, block } => {
                self.ensure_started(&mut output);
                self.blocks.insert(*index, block.clone());
                self.block_start(*index, block, &mut output);
            }
            CanonicalStreamEvent::TextDelta { index, text } => {
                self.ensure_block(*index, ContentBlockKind::Text, &mut output);
                self.text_delta(*index, text, &mut output);
            }
            CanonicalStreamEvent::ReasoningDelta { index, text } => {
                self.ensure_block(*index, ContentBlockKind::Reasoning, &mut output);
                self.reasoning_delta(*index, text, &mut output);
            }
            CanonicalStreamEvent::ReasoningSummaryDelta { index, part, text } => {
                self.ensure_block(*index, ContentBlockKind::Reasoning, &mut output);
                self.reasoning_summary_delta(*index, *part, text, &mut output);
            }
            CanonicalStreamEvent::ArtifactDelta { index, kind, data } => {
                if !self.blocks.contains_key(index) {
                    self.ensure_block(*index, ContentBlockKind::Reasoning, &mut output);
                }
                self.artifact_delta(*index, kind, data, &mut output);
            }
            CanonicalStreamEvent::ToolArgumentsDelta { index, data } => {
                self.ensure_block(*index, ContentBlockKind::ToolCall, &mut output);
                self.tool_arguments
                    .entry(*index)
                    .or_default()
                    .push_str(data);
                self.tool_delta(*index, data, &mut output);
            }
            CanonicalStreamEvent::RefusalDelta { index, text } => {
                self.ensure_block(*index, ContentBlockKind::Refusal, &mut output);
                self.refusal_delta(*index, text, &mut output);
            }
            CanonicalStreamEvent::BlockDone { index, .. } => self.block_done(*index, &mut output),
            CanonicalStreamEvent::UsageUpdate(usage) => merge_usage(&mut self.usage, usage),
            CanonicalStreamEvent::StatusUpdate(_) => {}
            CanonicalStreamEvent::Error(error) => {
                self.ensure_started(&mut output);
                self.render_error(error, &mut output);
                self.failed = true;
            }
            CanonicalStreamEvent::ResponseDone(finish) => self.response_done(finish, &mut output),
        }
        Ok(Bytes::from(output))
    }

    pub(crate) fn finish(&mut self) -> Result<Bytes, StreamError> {
        if self.done {
            Ok(Bytes::new())
        } else {
            Err(StreamError::new(
                "stream_incomplete",
                "renderer finished before a canonical terminal event",
            ))
        }
    }

    fn start(&mut self, meta: &ResponseMeta, output: &mut Vec<u8>) {
        if let Some(id) = &meta.id {
            self.response_id = id.clone();
        }
        if let Some(model) = &meta.model {
            self.model = model.clone();
        }
        self.ensure_started(output);
    }

    fn ensure_started(&mut self, output: &mut Vec<u8>) {
        if self.started {
            return;
        }
        self.started = true;
        match self.protocol {
            ProtocolKind::OpenAiChat => data_event(
                output,
                json!({
                    "id":chat_id(&self.response_id),"object":"chat.completion.chunk","model":self.model,
                    "choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":Value::Null}]
                }),
            ),
            ProtocolKind::OpenAiResponses => named_event(
                output,
                "response.created",
                json!({
                    "type":"response.created",
                    "response":self.responses_response("in_progress")
                }),
            ),
            ProtocolKind::AnthropicMessages => named_event(
                output,
                "message_start",
                json!({
                    "type":"message_start",
                    "message":{
                        "id":anthropic_id(&self.response_id),"type":"message","role":"assistant",
                        "model":self.model,"content":[],"stop_reason":Value::Null,
                        "stop_sequence":Value::Null,
                        "usage":{"input_tokens":usage_value(&self.usage.input_tokens),"output_tokens":0}
                    }
                }),
            ),
            ProtocolKind::GeminiNative => {}
        }
    }

    fn ensure_block(&mut self, index: u32, kind: ContentBlockKind, output: &mut Vec<u8>) {
        if self.blocks.contains_key(&index) {
            return;
        }
        let header = BlockHeader {
            tool_kind: None,
            tool_namespace: None,
            kind,
            source_item_id: None,
            call_id: (kind == ContentBlockKind::ToolCall).then(|| format!("call_{index}")),
            name: (kind == ContentBlockKind::ToolCall).then(|| "tool".to_string()),
            artifacts: Vec::new(),
        };
        self.blocks.insert(index, header.clone());
        self.block_start(index, &header, output);
    }

    fn block_start(&mut self, index: u32, block: &BlockHeader, output: &mut Vec<u8>) {
        if !block.artifacts.is_empty() {
            self.block_artifacts.insert(
                index,
                block
                    .artifacts
                    .iter()
                    .map(|artifact| (artifact.kind.clone(), artifact.payload.clone()))
                    .collect(),
            );
        }
        self.emit_tool_continuation_carrier(index, block, output);
        let wire_index = self.allocate_wire_index(index);
        match self.protocol {
            ProtocolKind::OpenAiChat if block.kind == ContentBlockKind::ToolCall => {
                let tool_index = self.chat_tool_index(index);
                let signature = chat_tool_signature(block);
                let mut tool_call = json!({
                    "index":tool_index,"id":block.call_id,"type":"function",
                    "function":{"name":block.name,"arguments":""}
                });
                let mut delta = json!({"tool_calls":[tool_call.clone()]});
                if let Some(signature) = signature {
                    tool_call["extra_content"] =
                        json!({"google":{"thought_signature":signature.clone()}});
                    delta["tool_calls"] = json!([tool_call]);
                    delta["extra_fields"] = json!({"google":{"thought_signature":signature}});
                }
                if let Some(carrier) = encode_carrier(tool_call_entries(
                    block.call_id.as_deref().unwrap_or("call_stream"),
                    &block.artifacts,
                )) {
                    // Keep the native per-tool compatibility field for generic clients, while
                    // CONST-aware clients retain the model-bound continuation envelope here.
                    delta["extra_fields"]["const_api"]["continuation"] = json!(carrier);
                }
                data_event(
                    output,
                    json!({
                        "id":chat_id(&self.response_id),"object":"chat.completion.chunk","model":self.model,
                        "choices":[{"index":0,"delta":delta,"finish_reason":Value::Null}]
                    }),
                )
            }
            ProtocolKind::OpenAiResponses => {
                let item_id = response_item_id(index, block);
                let mut item = match block.kind {
                    ContentBlockKind::ToolCall => json!({
                        "id":item_id,
                        "type":"function_call","status":"in_progress","call_id":block.call_id,
                        "name":block.name,"arguments":""
                    }),
                    ContentBlockKind::Reasoning => json!({
                        "id":item_id,
                        "type":"reasoning","status":"in_progress","summary":[]
                    }),
                    _ => json!({
                        "id":item_id,
                        "type":"message","status":"in_progress","role":"assistant","content":[]
                    }),
                };
                if block.kind == ContentBlockKind::ToolCall {
                    if let Some(namespace) = &block.tool_namespace {
                        item["namespace"] = json!(namespace);
                    }
                    if block.tool_kind == Some(ToolKind::Custom) {
                        item["type"] = json!("custom_tool_call");
                        item.as_object_mut()
                            .map(|object| object.remove("arguments"));
                        item["input"] = json!("");
                    }
                }
                named_event(
                    output,
                    "response.output_item.added",
                    json!({"type":"response.output_item.added","output_index":wire_index,"item":item}),
                );
                let part = match block.kind {
                    ContentBlockKind::Text => Some(json!({
                        "type":"output_text","text":"","annotations":[]
                    })),
                    ContentBlockKind::Refusal => Some(json!({
                        "type":"refusal","refusal":""
                    })),
                    _ => None,
                };
                if let Some(part) = part {
                    named_event(
                        output,
                        "response.content_part.added",
                        json!({
                            "type":"response.content_part.added","output_index":wire_index,
                            "content_index":0,"item_id":response_item_id(index, block),"part":part
                        }),
                    );
                }
            }
            ProtocolKind::AnthropicMessages => {
                let content_block = match block.kind {
                    ContentBlockKind::ToolCall => json!({
                        "type":"tool_use","id":block.call_id,"name":block.name,"input":{}
                    }),
                    ContentBlockKind::Reasoning => block
                        .artifacts
                        .iter()
                        .find(|artifact| artifact.kind == ArtifactKind::AnthropicRedactedThinking)
                        .map(|artifact| json!({"type":"redacted_thinking","data":artifact.payload}))
                        .unwrap_or_else(|| json!({"type":"thinking","thinking":"","signature":""})),
                    ContentBlockKind::Refusal | ContentBlockKind::Text => {
                        json!({"type":"text","text":""})
                    }
                    _ => return,
                };
                named_event(
                    output,
                    "content_block_start",
                    json!({"type":"content_block_start","index":wire_index,"content_block":content_block}),
                );
            }
            ProtocolKind::GeminiNative | ProtocolKind::OpenAiChat => {}
        }
    }

    fn allocate_wire_index(&mut self, canonical_index: u32) -> u32 {
        if !matches!(
            self.protocol,
            ProtocolKind::OpenAiResponses | ProtocolKind::AnthropicMessages
        ) {
            self.wire_indices.insert(canonical_index, canonical_index);
            return canonical_index;
        }
        let wire_index = self.next_wire_index;
        self.next_wire_index = self.next_wire_index.saturating_add(1);
        self.wire_indices.insert(canonical_index, wire_index);
        wire_index
    }

    fn wire_index(&self, canonical_index: u32) -> u32 {
        self.wire_indices
            .get(&canonical_index)
            .copied()
            .unwrap_or(canonical_index)
    }

    fn next_synthetic_wire_index(&mut self) -> u32 {
        let index = self.next_wire_index;
        self.next_wire_index = self.next_wire_index.saturating_add(1);
        index
    }

    fn emit_tool_continuation_carrier(
        &mut self,
        canonical_index: u32,
        block: &BlockHeader,
        output: &mut Vec<u8>,
    ) {
        if block.kind != ContentBlockKind::ToolCall
            || !matches!(
                self.protocol,
                ProtocolKind::OpenAiResponses | ProtocolKind::AnthropicMessages
            )
        {
            return;
        }
        let call_id = block.call_id.as_deref().unwrap_or("call_stream");
        let Some(carrier) = encode_carrier(tool_call_entries(call_id, &block.artifacts)) else {
            return;
        };
        let wire_index = self.next_synthetic_wire_index();
        match self.protocol {
            ProtocolKind::OpenAiResponses => {
                let item = json!({
                    "id":format!("rs_const_carrier_{canonical_index}"),
                    "type":"reasoning",
                    "status":"completed",
                    "summary":[],
                    "encrypted_content":carrier,
                });
                named_event(
                    output,
                    "response.output_item.added",
                    json!({"type":"response.output_item.added","output_index":wire_index,"item":item}),
                );
                named_event(
                    output,
                    "response.output_item.done",
                    json!({"type":"response.output_item.done","output_index":wire_index,"item":item}),
                );
                self.response_output.insert(wire_index, item);
            }
            ProtocolKind::AnthropicMessages => {
                named_event(
                    output,
                    "content_block_start",
                    json!({
                        "type":"content_block_start",
                        "index":wire_index,
                        "content_block":{"type":"thinking","thinking":"","signature":""}
                    }),
                );
                named_event(
                    output,
                    "content_block_delta",
                    json!({
                        "type":"content_block_delta",
                        "index":wire_index,
                        "delta":{"type":"signature_delta","signature":carrier}
                    }),
                );
                named_event(
                    output,
                    "content_block_stop",
                    json!({"type":"content_block_stop","index":wire_index}),
                );
            }
            _ => {}
        }
    }

    fn text_delta(&mut self, index: u32, text: &str, output: &mut Vec<u8>) {
        self.block_text.entry(index).or_default().push_str(text);
        let wire_index = self.wire_index(index);
        match self.protocol {
            ProtocolKind::OpenAiChat => data_event(
                output,
                json!({
                    "id":chat_id(&self.response_id),"object":"chat.completion.chunk","model":self.model,
                    "choices":[{"index":0,"delta":{"content":text},"finish_reason":Value::Null}]
                }),
            ),
            ProtocolKind::OpenAiResponses => named_event(
                output,
                "response.output_text.delta",
                json!({
                    "type":"response.output_text.delta","output_index":wire_index,"content_index":0,
                    "item_id":self.responses_item_id(index),"delta":text
                }),
            ),
            ProtocolKind::AnthropicMessages => named_event(
                output,
                "content_block_delta",
                json!({"type":"content_block_delta","index":wire_index,"delta":{"type":"text_delta","text":text}}),
            ),
            ProtocolKind::GeminiNative => data_event(
                output,
                json!({"modelVersion":self.model,"candidates":[{"content":{"role":"model","parts":[{"text":text}]}}]}),
            ),
        }
    }

    fn reasoning_delta(&mut self, index: u32, text: &str, output: &mut Vec<u8>) {
        self.block_text.entry(index).or_default().push_str(text);
        let wire_index = self.wire_index(index);
        if self.protocol == ProtocolKind::AnthropicMessages
            && self.blocks.get(&index).is_some_and(|block| {
                block
                    .artifacts
                    .iter()
                    .any(|artifact| artifact.kind == ArtifactKind::AnthropicRedactedThinking)
            })
        {
            return;
        }
        match self.protocol {
            ProtocolKind::OpenAiChat => data_event(
                output,
                json!({
                    "id":chat_id(&self.response_id),"object":"chat.completion.chunk","model":self.model,
                    "choices":[{"index":0,"delta":{"reasoning_content":text},"finish_reason":Value::Null}]
                }),
            ),
            ProtocolKind::OpenAiResponses => named_event(
                output,
                "response.reasoning_text.delta",
                json!({"type":"response.reasoning_text.delta","output_index":wire_index,"delta":text}),
            ),
            ProtocolKind::AnthropicMessages => named_event(
                output,
                "content_block_delta",
                json!({"type":"content_block_delta","index":wire_index,"delta":{"type":"thinking_delta","thinking":text}}),
            ),
            ProtocolKind::GeminiNative => data_event(
                output,
                json!({"modelVersion":self.model,"candidates":[{"content":{"role":"model","parts":[{"thought":true,"text":text}]}}]}),
            ),
        }
    }

    fn reasoning_summary_delta(&self, index: u32, part: u32, text: &str, output: &mut Vec<u8>) {
        if self.protocol == ProtocolKind::OpenAiResponses {
            let wire_index = self.wire_index(index);
            named_event(
                output,
                "response.reasoning_summary_text.delta",
                json!({
                    "type":"response.reasoning_summary_text.delta","output_index":wire_index,
                    "summary_index":part,"delta":text
                }),
            );
        }
    }

    fn artifact_delta(
        &mut self,
        index: u32,
        kind: &ArtifactKind,
        data: &str,
        output: &mut Vec<u8>,
    ) {
        append_artifact_delta(&mut self.block_artifacts, index, kind, data);
        let owner = self.blocks.get(&index).map(|header| header.kind);
        let wire_index = self.wire_index(index);
        match self.protocol {
            ProtocolKind::AnthropicMessages
                if matches!(kind, ArtifactKind::AnthropicThinkingSignature) =>
            {
                named_event(
                    output,
                    "content_block_delta",
                    json!({"type":"content_block_delta","index":wire_index,"delta":{"type":"signature_delta","signature":data}}),
                )
            }
            ProtocolKind::GeminiNative
                if matches!(kind, ArtifactKind::GeminiThoughtSignature)
                    && owner != Some(ContentBlockKind::ToolCall) =>
            {
                let part = if owner == Some(ContentBlockKind::Reasoning) {
                    json!({"thought":true,"thoughtSignature":data})
                } else {
                    json!({"text":"","thoughtSignature":data})
                };
                data_event(
                    output,
                    json!({"modelVersion":self.model,"candidates":[{"content":{"role":"model","parts":[part]}}]}),
                )
            }
            ProtocolKind::OpenAiChat
                if matches!(kind, ArtifactKind::GeminiThoughtSignature)
                    && owner == Some(ContentBlockKind::ToolCall) =>
            {
                let tool_index = self.chat_tool_index(index);
                data_event(
                    output,
                    json!({
                        "id":chat_id(&self.response_id),"object":"chat.completion.chunk","model":self.model,
                        "choices":[{"index":0,"delta":{
                            "tool_calls":[{"index":tool_index,"extra_content":{"google":{"thought_signature":data}}}],
                            "extra_fields":{"google":{"thought_signature":data}}
                        },"finish_reason":Value::Null}]
                    }),
                )
            }
            _ => {}
        }
    }

    fn tool_delta(&mut self, index: u32, data: &str, output: &mut Vec<u8>) {
        let wire_index = self.wire_index(index);
        match self.protocol {
            ProtocolKind::OpenAiChat => {
                let tool_index = self.chat_tool_index(index);
                data_event(
                    output,
                    json!({
                        "id":chat_id(&self.response_id),"object":"chat.completion.chunk","model":self.model,
                        "choices":[{"index":0,"delta":{"tool_calls":[{
                            "index":tool_index,"function":{"arguments":data}
                        }]},"finish_reason":Value::Null}]
                    }),
                )
            }
            ProtocolKind::OpenAiResponses => {
                let header = self.blocks.get(&index);
                let event =
                    if header.is_some_and(|header| header.tool_kind == Some(ToolKind::Custom)) {
                        "response.custom_tool_call_input.delta"
                    } else {
                        "response.function_call_arguments.delta"
                    };
                named_event(
                    output,
                    event,
                    json!({
                        "type":event,"output_index":wire_index,
                        "item_id":header.and_then(|header| header.source_item_id.clone()),
                        "call_id":header.and_then(|header| header.call_id.clone()),"delta":data
                    }),
                )
            }
            ProtocolKind::AnthropicMessages => named_event(
                output,
                "content_block_delta",
                json!({"type":"content_block_delta","index":wire_index,"delta":{"type":"input_json_delta","partial_json":data}}),
            ),
            ProtocolKind::GeminiNative => {}
        }
    }

    fn chat_tool_index(&mut self, canonical_index: u32) -> u32 {
        if let Some(index) = self.chat_tool_indices.get(&canonical_index) {
            return *index;
        }
        let index = self.next_chat_tool_index;
        self.next_chat_tool_index += 1;
        self.chat_tool_indices.insert(canonical_index, index);
        index
    }

    fn refusal_delta(&mut self, index: u32, text: &str, output: &mut Vec<u8>) {
        self.block_text.entry(index).or_default().push_str(text);
        let wire_index = self.wire_index(index);
        match self.protocol {
            ProtocolKind::OpenAiChat => data_event(
                output,
                json!({
                    "id":chat_id(&self.response_id),"object":"chat.completion.chunk","model":self.model,
                    "choices":[{"index":0,"delta":{"refusal":text},"finish_reason":Value::Null}]
                }),
            ),
            ProtocolKind::OpenAiResponses => named_event(
                output,
                "response.refusal.delta",
                json!({
                    "type":"response.refusal.delta","output_index":wire_index,"content_index":0,
                    "item_id":self.responses_item_id(index),"delta":text
                }),
            ),
            ProtocolKind::AnthropicMessages | ProtocolKind::GeminiNative => {
                self.text_delta(index, text, output)
            }
        }
    }

    fn block_done(&mut self, index: u32, output: &mut Vec<u8>) {
        if !self.done_blocks.insert(index) {
            return;
        }
        let wire_index = self.wire_index(index);
        if self.protocol == ProtocolKind::GeminiNative {
            self.flush_gemini_reasoning_carrier(index, output);
            self.flush_gemini_tool(index, output);
            self.flush_previous_part_carrier(index, output);
        } else if self.protocol == ProtocolKind::AnthropicMessages {
            self.flush_anthropic_reasoning_carrier(index, wire_index, output);
            named_event(
                output,
                "content_block_stop",
                json!({"type":"content_block_stop","index":wire_index}),
            );
            self.flush_previous_part_carrier(index, output);
        } else if self.protocol == ProtocolKind::OpenAiChat {
            self.flush_chat_reasoning_carrier(index, output);
            self.flush_previous_part_carrier(index, output);
        } else if self.protocol == ProtocolKind::OpenAiResponses {
            let Some(header) = self.blocks.get(&index).cloned() else {
                return;
            };
            let item_id = response_item_id(index, &header);
            let text = self.block_text.get(&index).cloned().unwrap_or_default();
            let item = match header.kind {
                ContentBlockKind::ToolCall => {
                    let arguments = self.tool_arguments.get(&index).cloned().unwrap_or_default();
                    let custom = header.tool_kind == Some(ToolKind::Custom);
                    let event = if custom {
                        "response.custom_tool_call_input.done"
                    } else {
                        "response.function_call_arguments.done"
                    };
                    let field = if custom { "input" } else { "arguments" };
                    named_event(
                        output,
                        event,
                        json!({
                            "type":event,"output_index":wire_index,
                            "item_id":item_id,"call_id":header.call_id,field:arguments
                        }),
                    );
                    let mut item = json!({
                        "id":item_id,"type":if custom {"custom_tool_call"} else {"function_call"},"status":"completed",
                        "call_id":header.call_id,"name":header.name,field:arguments
                    });
                    if let Some(namespace) = &header.tool_namespace {
                        item["namespace"] = json!(namespace);
                    }
                    item
                }
                ContentBlockKind::Text => {
                    let part = json!({"type":"output_text","text":text,"annotations":[]});
                    named_event(
                        output,
                        "response.output_text.done",
                        json!({
                            "type":"response.output_text.done","output_index":wire_index,
                            "content_index":0,"item_id":item_id,"text":text
                        }),
                    );
                    named_event(
                        output,
                        "response.content_part.done",
                        json!({
                            "type":"response.content_part.done","output_index":wire_index,
                            "content_index":0,"item_id":item_id,"part":part
                        }),
                    );
                    json!({
                        "id":item_id,"type":"message","status":"completed",
                        "role":"assistant","content":[part]
                    })
                }
                ContentBlockKind::Refusal => {
                    let part = json!({"type":"refusal","refusal":text});
                    named_event(
                        output,
                        "response.refusal.done",
                        json!({
                            "type":"response.refusal.done","output_index":wire_index,
                            "content_index":0,"item_id":item_id,"refusal":text
                        }),
                    );
                    named_event(
                        output,
                        "response.content_part.done",
                        json!({
                            "type":"response.content_part.done","output_index":wire_index,
                            "content_index":0,"item_id":item_id,"part":part
                        }),
                    );
                    json!({
                        "id":item_id,"type":"message","status":"completed",
                        "role":"assistant","content":[part]
                    })
                }
                ContentBlockKind::Reasoning => {
                    let mut item = json!({
                        "id":item_id,"type":"reasoning","status":"completed",
                        "summary":if text.is_empty() { Vec::<Value>::new() } else {
                            vec![json!({"type":"summary_text","text":text})]
                        }
                    });
                    let artifacts = self.stream_artifacts(index, ContentBlockKind::Reasoning);
                    let native = artifacts
                        .iter()
                        .find(|artifact| artifact.kind == ArtifactKind::OpenAiEncryptedReasoning);
                    if artifacts.len() == 1 {
                        if let Some(native) = native {
                            item["encrypted_content"] = native.payload.clone();
                        } else if let Some(carrier) = encode_carrier(reasoning_entries(&artifacts))
                        {
                            item["encrypted_content"] = json!(carrier);
                        }
                    } else if let Some(carrier) = encode_carrier(reasoning_entries(&artifacts)) {
                        item["encrypted_content"] = json!(carrier);
                    }
                    item
                }
                _ => json!({
                    "id":item_id,"type":"message","status":"completed",
                    "role":"assistant","content":[]
                }),
            };
            named_event(
                output,
                "response.output_item.done",
                json!({"type":"response.output_item.done","output_index":wire_index,"item":item}),
            );
            self.response_output.insert(wire_index, item);
            self.flush_previous_part_carrier(index, output);
        }
    }

    fn responses_item_id(&self, index: u32) -> String {
        self.blocks
            .get(&index)
            .map(|header| response_item_id(index, header))
            .unwrap_or_else(|| format!("msg_{index}"))
    }

    fn flush_gemini_tool(&mut self, index: u32, output: &mut Vec<u8>) {
        let Some(header) = self.blocks.get(&index) else {
            return;
        };
        if header.kind != ContentBlockKind::ToolCall {
            return;
        }
        let raw = self.tool_arguments.remove(&index).unwrap_or_default();
        let args = serde_json::from_str::<Value>(&raw).unwrap_or_else(|_| json!({}));
        let mut part = json!({"functionCall":{
            "id":header.call_id,"name":header.name,"args":args
        }});
        let artifacts = self.stream_artifacts(index, ContentBlockKind::ToolCall);
        let native = artifacts
            .iter()
            .find(|artifact| artifact.kind == ArtifactKind::GeminiThoughtSignature);
        if artifacts.len() == 1 {
            if let Some(native) = native {
                part["thoughtSignature"] = native.payload.clone();
            } else if let Some(carrier) = encode_carrier(tool_call_entries(
                header.call_id.as_deref().unwrap_or("call_stream"),
                &artifacts,
            )) {
                part["thoughtSignature"] = json!(carrier);
            }
        } else if let Some(carrier) = encode_carrier(tool_call_entries(
            header.call_id.as_deref().unwrap_or("call_stream"),
            &artifacts,
        )) {
            part["thoughtSignature"] = json!(carrier);
        }
        data_event(
            output,
            json!({
                "modelVersion":self.model,
                "candidates":[{"content":{"role":"model","parts":[part]}}]
            }),
        );
    }

    fn stream_artifacts(
        &self,
        index: u32,
        owner: ContentBlockKind,
    ) -> Vec<crate::protocol::ir::OpaqueArtifact> {
        self.block_artifacts
            .get(&index)
            .into_iter()
            .flatten()
            .map(|(kind, payload)| {
                stream_artifact(kind.clone(), payload.clone(), owner, Some(&self.model))
            })
            .collect()
    }

    fn flush_anthropic_reasoning_carrier(&self, index: u32, wire_index: u32, output: &mut Vec<u8>) {
        if self.blocks.get(&index).map(|block| block.kind) != Some(ContentBlockKind::Reasoning) {
            return;
        }
        let artifacts = self.stream_artifacts(index, ContentBlockKind::Reasoning);
        if artifacts.iter().any(|artifact| {
            matches!(
                artifact.kind,
                ArtifactKind::AnthropicThinkingSignature | ArtifactKind::AnthropicRedactedThinking
            )
        }) {
            return;
        }
        if let Some(carrier) = encode_carrier(reasoning_entries(&artifacts)) {
            named_event(
                output,
                "content_block_delta",
                json!({
                    "type":"content_block_delta",
                    "index":wire_index,
                    "delta":{"type":"signature_delta","signature":carrier}
                }),
            );
        }
    }

    fn flush_gemini_reasoning_carrier(&self, index: u32, output: &mut Vec<u8>) {
        if self.blocks.get(&index).map(|block| block.kind) != Some(ContentBlockKind::Reasoning) {
            return;
        }
        let artifacts = self.stream_artifacts(index, ContentBlockKind::Reasoning);
        if artifacts
            .iter()
            .any(|artifact| artifact.kind == ArtifactKind::GeminiThoughtSignature)
        {
            return;
        }
        if let Some(carrier) = encode_carrier(reasoning_entries(&artifacts)) {
            data_event(
                output,
                json!({
                    "modelVersion":self.model,
                    "candidates":[{"content":{"role":"model","parts":[{
                        "thought":true,"text":"","thoughtSignature":carrier
                    }]}}]
                }),
            );
        }
    }

    fn flush_chat_reasoning_carrier(&self, index: u32, output: &mut Vec<u8>) {
        if self.blocks.get(&index).map(|block| block.kind) != Some(ContentBlockKind::Reasoning) {
            return;
        }
        let artifacts = self.stream_artifacts(index, ContentBlockKind::Reasoning);
        if let Some(carrier) = encode_carrier(reasoning_entries(&artifacts)) {
            data_event(
                output,
                json!({
                    "id":chat_id(&self.response_id),"object":"chat.completion.chunk","model":self.model,
                    "choices":[{"index":0,"delta":{
                        "extra_fields":{"google":{"thought_signature":carrier}}
                    },"finish_reason":Value::Null}]
                }),
            );
        }
    }

    fn flush_previous_part_carrier(&mut self, index: u32, output: &mut Vec<u8>) {
        let Some(owner) = self.blocks.get(&index).map(|block| block.kind) else {
            return;
        };
        if matches!(
            owner,
            ContentBlockKind::Reasoning | ContentBlockKind::ToolCall
        ) {
            return;
        }
        let artifacts = self.stream_artifacts(index, owner);
        if artifacts.is_empty()
            || (self.protocol == ProtocolKind::GeminiNative
                && artifacts
                    .iter()
                    .any(|artifact| artifact.kind == ArtifactKind::GeminiThoughtSignature))
        {
            return;
        }
        let Some(carrier) = encode_carrier(previous_part_entries(&artifacts)) else {
            return;
        };
        match self.protocol {
            ProtocolKind::OpenAiResponses => {
                let wire_index = self.next_synthetic_wire_index();
                let item = json!({
                    "id":format!("rs_const_part_carrier_{index}"),
                    "type":"reasoning","status":"completed","summary":[],
                    "encrypted_content":carrier,
                });
                named_event(
                    output,
                    "response.output_item.added",
                    json!({"type":"response.output_item.added","output_index":wire_index,"item":item}),
                );
                named_event(
                    output,
                    "response.output_item.done",
                    json!({"type":"response.output_item.done","output_index":wire_index,"item":item}),
                );
                self.response_output.insert(wire_index, item);
            }
            ProtocolKind::AnthropicMessages => {
                let wire_index = self.next_synthetic_wire_index();
                named_event(
                    output,
                    "content_block_start",
                    json!({
                        "type":"content_block_start","index":wire_index,
                        "content_block":{"type":"thinking","thinking":"","signature":""}
                    }),
                );
                named_event(
                    output,
                    "content_block_delta",
                    json!({
                        "type":"content_block_delta","index":wire_index,
                        "delta":{"type":"signature_delta","signature":carrier}
                    }),
                );
                named_event(
                    output,
                    "content_block_stop",
                    json!({"type":"content_block_stop","index":wire_index}),
                );
            }
            ProtocolKind::OpenAiChat => data_event(
                output,
                json!({
                    "id":chat_id(&self.response_id),"object":"chat.completion.chunk","model":self.model,
                    "choices":[{"index":0,"delta":{
                        "extra_fields":{"google":{"thought_signature":carrier}}
                    },"finish_reason":Value::Null}]
                }),
            ),
            ProtocolKind::GeminiNative => data_event(
                output,
                json!({
                    "modelVersion":self.model,
                    "candidates":[{"content":{"role":"model","parts":[{
                        "text":"","thoughtSignature":carrier
                    }]}}]
                }),
            ),
        }
    }

    fn render_error(&self, error: &CanonicalError, output: &mut Vec<u8>) {
        let payload = json!({
            "error":{"type":error.code,"message":error.message,"code":error.provider_code}
        });
        match self.protocol {
            ProtocolKind::OpenAiResponses => named_event(output, "error", payload),
            ProtocolKind::AnthropicMessages => named_event(
                output,
                "error",
                json!({"type":"error","error":{"type":error.code,"message":error.message}}),
            ),
            ProtocolKind::OpenAiChat | ProtocolKind::GeminiNative => data_event(output, payload),
        }
    }

    fn response_done(&mut self, finish: &FinishDetail, output: &mut Vec<u8>) {
        if self.done {
            return;
        }
        self.ensure_started(output);
        if self.failed || finish.reason == FinishReason::Error {
            self.done = true;
            return;
        }
        if self.protocol == ProtocolKind::GeminiNative {
            for index in self.tool_arguments.keys().copied().collect::<Vec<_>>() {
                self.flush_gemini_tool(index, output);
            }
        }
        if self.protocol == ProtocolKind::OpenAiResponses {
            for index in self.blocks.keys().copied().collect::<Vec<_>>() {
                self.block_done(index, output);
            }
        }
        match self.protocol {
            ProtocolKind::OpenAiChat => {
                data_event(
                    output,
                    json!({
                        "id":chat_id(&self.response_id),"object":"chat.completion.chunk","model":self.model,
                        "choices":[{"index":0,"delta":{},"finish_reason":chat_finish(finish.reason)}],
                        "usage":chat_usage(&self.usage)
                    }),
                );
                output.extend_from_slice(b"data: [DONE]\n\n");
            }
            ProtocolKind::OpenAiResponses => named_event(
                output,
                "response.completed",
                json!({
                    "type":"response.completed",
                    "response":self.responses_response(match finish.reason {
                        FinishReason::Length => "incomplete",
                        FinishReason::Cancelled => "cancelled",
                        FinishReason::Refusal | FinishReason::ContentFilter => "refused",
                        _ => "completed",
                    })
                }),
            ),
            ProtocolKind::AnthropicMessages => {
                named_event(
                    output,
                    "message_delta",
                    json!({
                        "type":"message_delta","delta":{"stop_reason":anthropic_finish(finish.reason),"stop_sequence":Value::Null},
                        "usage":{
                            "output_tokens":usage_value(&self.usage.output_tokens),
                            "output_tokens_details":{"thinking_tokens":self.usage.reasoning_tokens.value}
                        }
                    }),
                );
                named_event(output, "message_stop", json!({"type":"message_stop"}));
            }
            ProtocolKind::GeminiNative => data_event(
                output,
                json!({
                    "modelVersion":self.model,
                    "candidates":[{"finishReason":gemini_finish(finish.reason)}],
                    "usageMetadata":gemini_usage(&self.usage)
                }),
            ),
        }
        self.done = true;
    }

    fn responses_response(&self, status: &str) -> Value {
        let output = self.response_output.values().cloned().collect::<Vec<_>>();
        json!({
            "id":responses_id(&self.response_id),"object":"response","created_at":0,
            "status":status,"model":self.model,"output":output,"usage":responses_usage(&self.usage)
        })
    }
}

fn chat_tool_signature(block: &BlockHeader) -> Option<Value> {
    let native = block
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == ArtifactKind::GeminiThoughtSignature);
    if block.artifacts.len() == 1 && native.is_some() {
        return native.map(|artifact| artifact.payload.clone());
    }
    let call_id = block.call_id.as_deref().unwrap_or("call_stream");
    encode_carrier(tool_call_entries(call_id, &block.artifacts)).map(Value::String)
}

fn append_artifact_delta(
    artifacts: &mut BTreeMap<u32, Vec<(ArtifactKind, Value)>>,
    index: u32,
    kind: &ArtifactKind,
    data: &str,
) {
    let values = artifacts.entry(index).or_default();
    if let Some((_, payload)) = values.iter_mut().find(|(existing, _)| existing == kind) {
        if let Some(existing) = payload.as_str() {
            *payload = Value::String(format!("{existing}{data}"));
        }
        return;
    }
    values.push((kind.clone(), Value::String(data.to_string())));
}

fn response_item_id(index: u32, header: &BlockHeader) -> String {
    header
        .source_item_id
        .clone()
        .unwrap_or_else(|| match header.kind {
            ContentBlockKind::ToolCall => format!("fc_{index}"),
            ContentBlockKind::Reasoning => format!("rs_{index}"),
            _ => format!("msg_{index}"),
        })
}

fn named_event(output: &mut Vec<u8>, event: &str, value: Value) {
    output.extend_from_slice(format!("event: {event}\n").as_bytes());
    data_event(output, value);
}

fn data_event(output: &mut Vec<u8>, value: Value) {
    output.extend_from_slice(b"data: ");
    output.extend_from_slice(
        serde_json::to_string(&value)
            .unwrap_or_else(|_| "{}".to_string())
            .as_bytes(),
    );
    output.extend_from_slice(b"\n\n");
}

fn usage_value(value: &crate::protocol::ir::UsageValue) -> u64 {
    value.value.unwrap_or(0)
}

fn chat_usage(usage: &Usage) -> Value {
    json!({
        "prompt_tokens":usage_value(&usage.input_tokens),
        "completion_tokens":usage_value(&usage.output_tokens),
        "total_tokens":usage_value(&usage.total_tokens),
        "completion_tokens_details":{"reasoning_tokens":usage.reasoning_tokens.value}
    })
}

fn responses_usage(usage: &Usage) -> Value {
    json!({
        "input_tokens":usage_value(&usage.input_tokens),
        "output_tokens":usage_value(&usage.output_tokens),
        "total_tokens":usage_value(&usage.total_tokens),
        "input_tokens_details":{
            "cached_tokens":usage.cache_read_tokens.value,
            "cache_write_tokens":usage.cache_write_tokens.value
        },
        "output_tokens_details":{"reasoning_tokens":usage.reasoning_tokens.value}
    })
}

fn gemini_usage(usage: &Usage) -> Value {
    json!({
        "promptTokenCount":usage_value(&usage.input_tokens),
        "candidatesTokenCount":usage_value(&usage.non_reasoning_output()),
        "totalTokenCount":usage_value(&usage.total_tokens),
        "thoughtsTokenCount":usage.reasoning_tokens.value
    })
}

fn chat_finish(reason: FinishReason) -> &'static str {
    match reason {
        FinishReason::ToolCalls => "tool_calls",
        FinishReason::PauseTurn => "stop",
        FinishReason::Length => "length",
        FinishReason::ContentFilter | FinishReason::Refusal => "content_filter",
        _ => "stop",
    }
}

fn anthropic_finish(reason: FinishReason) -> &'static str {
    match reason {
        FinishReason::ToolCalls => "tool_use",
        FinishReason::PauseTurn => "pause_turn",
        FinishReason::Length => "max_tokens",
        FinishReason::Refusal | FinishReason::ContentFilter => "refusal",
        _ => "end_turn",
    }
}

fn gemini_finish(reason: FinishReason) -> &'static str {
    match reason {
        FinishReason::Length => "MAX_TOKENS",
        FinishReason::PauseTurn => "FINISH_REASON_UNSPECIFIED",
        FinishReason::Refusal | FinishReason::ContentFilter => "SAFETY",
        _ => "STOP",
    }
}

fn chat_id(id: &str) -> String {
    if id.starts_with("chatcmpl_") {
        id.to_string()
    } else {
        format!("chatcmpl_{id}")
    }
}

fn responses_id(id: &str) -> String {
    if id.starts_with("resp_") {
        id.to_string()
    } else {
        format!("resp_{id}")
    }
}

fn anthropic_id(id: &str) -> String {
    if id.starts_with("msg_") {
        id.to_string()
    } else {
        format!("msg_{id}")
    }
}
