use std::collections::HashMap;

use serde_json::{Map, Value, json};

use crate::protocol::adapters::{
    AdapterContext, AdapterError, ProtocolAdapter, affinity_matches_context,
    collect_nested_unknown_fields, continuation_artifact_affinity, ensure_encoding_plan,
    link_tool_result_names, protocol_affinity, response_status_from_finish,
};
use crate::protocol::anthropic_dialect::anthropic_model_dialect;
use crate::protocol::capability::Feature;
use crate::protocol::continuation::{
    ContinuationCarrier, ContinuationOwner, decode_carrier, encode_carrier, is_portable_artifact,
    previous_part_entries, reasoning_artifacts_are_encrypted, reasoning_entries, tool_call_entries,
};
use crate::protocol::conversion::{
    ArtifactDisposition, ConversionPlan, FeatureDisposition, canonicalize_schema,
    canonicalize_tool_schema,
};
use crate::protocol::ir::*;
use crate::protocol::kind::ProtocolKind;

pub(crate) struct AnthropicMessagesAdapter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageDisposition {
    Turn(TurnRole),
    Instruction(InstructionRole),
    SystemReminder,
    ToolResult,
}

impl ProtocolAdapter for AnthropicMessagesAdapter {
    fn decode_request(
        &self,
        body: &Value,
        context: &AdapterContext,
    ) -> Result<CanonicalRequestV2, AdapterError> {
        let model = required_string(body.get("model"), "$.model", "model_missing")?;
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                AdapterError::new(
                    "messages_missing",
                    "$.messages",
                    "messages must be an array",
                )
            })?;
        let mut request = CanonicalRequestV2::new(model);
        request.metadata.source_protocol = Some(ProtocolKind::AnthropicMessages);
        request.metadata.user_id = body
            .pointer("/metadata/user_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        request.instructions = decode_system(body.get("system"), context, model)?;
        let mut call_names = HashMap::new();
        let mut compatibility_extensions = Vec::new();
        let mut pending_tool_call_ids = Vec::new();
        let mut pending_system_reminders = Vec::new();
        for (index, message) in messages.iter().enumerate() {
            let (disposition, preserve_role_extension) = message_disposition(message);
            if preserve_role_extension {
                compatibility_extensions.push(message_role_extension(index, message));
            }
            let content_path = format!("$.messages[{index}].content");
            let content = message.get("content").unwrap_or(&Value::Null);
            let blocks = match disposition {
                MessageDisposition::ToolResult => decode_compat_tool_result(
                    message,
                    content,
                    context,
                    model,
                    &content_path,
                    &mut call_names,
                )?,
                _ => decode_blocks(content, context, model, &content_path, &mut call_names)?,
            };
            let message_id = message
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string);
            if disposition == MessageDisposition::SystemReminder {
                let reminder = Turn {
                    id: message_id,
                    role: TurnRole::User,
                    blocks: system_reminder_blocks(blocks),
                    status: None,
                };
                if pending_tool_call_ids.is_empty() {
                    request.turns.push(reminder);
                } else {
                    pending_system_reminders.push(reminder);
                }
                continue;
            }
            if let MessageDisposition::Instruction(role) = disposition {
                request.instructions.push(Instruction { role, blocks });
                continue;
            }
            let role = match disposition {
                MessageDisposition::Turn(role) => role,
                MessageDisposition::ToolResult => TurnRole::User,
                MessageDisposition::Instruction(_) | MessageDisposition::SystemReminder => continue,
            };
            let mut turn = Turn {
                id: message_id,
                role,
                blocks,
                status: None,
            };

            if role == TurnRole::User && !pending_system_reminders.is_empty() {
                let blocks = std::mem::take(&mut turn.blocks);
                match split_complete_tool_results(blocks, &pending_tool_call_ids) {
                    Ok((tool_results, remaining)) => {
                        request.turns.push(Turn {
                            id: turn.id.take(),
                            role: TurnRole::User,
                            blocks: tool_results,
                            status: turn.status,
                        });
                        request.turns.append(&mut pending_system_reminders);
                        if !remaining.is_empty() {
                            turn.blocks = remaining;
                            request.turns.push(turn);
                        }
                        pending_tool_call_ids.clear();
                        continue;
                    }
                    Err(blocks) => turn.blocks = blocks,
                }
                request.turns.append(&mut pending_system_reminders);
            } else if role != TurnRole::User && !pending_system_reminders.is_empty() {
                request.turns.append(&mut pending_system_reminders);
            }

            update_pending_tool_calls(&mut pending_tool_call_ids, &turn);
            request.turns.push(turn);
        }
        request.turns.append(&mut pending_system_reminders);
        request.tools = decode_tools(body.get("tools"))?;
        request.tool_choice = decode_tool_choice(body.get("tool_choice"))?;
        request.reasoning = decode_thinking(body);
        request.response_format = decode_response_format(body)?;
        request.generation = decode_generation(body);
        request.stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
        request.extensions = decode_extensions(body, context, model);
        request.extensions.extend(compatibility_extensions);
        request
            .extensions
            .extend(decode_nested_extensions(body, context, model));
        link_tool_result_names(&mut request.turns);
        Ok(request)
    }

    fn encode_request(
        &self,
        request: &CanonicalRequestV2,
        plan: &ConversionPlan,
        _context: &AdapterContext,
    ) -> Result<Value, AdapterError> {
        ensure_encoding_plan(plan, ProtocolKind::AnthropicMessages)?;
        let mut body = json!({
            "model":request.model,
            "max_tokens":request.generation.max_output_tokens.unwrap_or(128),
            "messages":encode_turns(&request.turns, plan)?,
            "stream":request.stream,
        });
        if !request.instructions.is_empty() {
            body["system"] = Value::Array(encode_instructions(&request.instructions, plan)?);
        }
        if !request.tools.is_empty() {
            body["tools"] = encode_tools(&request.tools)?;
        }
        if !matches!(request.tool_choice, ToolChoice::Auto) {
            body["tool_choice"] = encode_tool_choice(&request.tool_choice, &request.generation)?;
        }
        encode_thinking(&mut body, &request.reasoning, &request.model);
        encode_response_format(&mut body, &request.response_format)?;
        encode_generation(&mut body, &request.generation, &request.model);
        if let Some(user_id) = &request.metadata.user_id {
            body["metadata"] = json!({"user_id":user_id});
        }
        encode_extensions(&mut body, &request.extensions, plan);
        Ok(body)
    }

    fn decode_response(
        &self,
        body: &Value,
        context: &AdapterContext,
    ) -> Result<CanonicalResponseV2, AdapterError> {
        if body.get("type").and_then(Value::as_str) == Some("error") {
            return Ok(decode_error_response(body, context));
        }
        let content = body
            .get("content")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                AdapterError::new(
                    "response_content_invalid",
                    "$.content",
                    "response content must be an array",
                )
            })?;
        let model = body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mut call_names = HashMap::new();
        let blocks = decode_blocks(
            &Value::Array(content.clone()),
            context,
            model,
            "$.content",
            &mut call_names,
        )?;
        let stop_reason = body.get("stop_reason").and_then(Value::as_str);
        let finish_reason = if stop_reason.is_none()
            && blocks
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolCall(_)))
        {
            FinishReason::ToolCalls
        } else {
            decode_stop_reason(stop_reason)
        };
        Ok(CanonicalResponseV2 {
            id: body.get("id").and_then(Value::as_str).map(str::to_string),
            model: body
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string),
            blocks,
            status: response_status_from_finish(&finish_reason),
            finish: FinishDetail {
                reason: finish_reason,
                original_reason: stop_reason.map(str::to_string),
                incomplete_details: body
                    .get("stop_sequence")
                    .filter(|value| !value.is_null())
                    .map(|value| json!({"stop_sequence":value})),
            },
            usage: decode_usage(body.get("usage")),
            error: None,
            extensions: decode_response_extensions(body, context, model),
        })
    }

    fn encode_response(
        &self,
        response: &CanonicalResponseV2,
        plan: &ConversionPlan,
        context: &AdapterContext,
    ) -> Result<Value, AdapterError> {
        ensure_encoding_plan(plan, ProtocolKind::AnthropicMessages)?;
        if let Some(error) = &response.error {
            return Ok(json!({
                "type":"error",
                "error":{
                    "type":error.provider_code.as_ref().unwrap_or(&error.code),
                    "message":error.message,
                }
            }));
        }
        Ok(json!({
            "id":response.id.clone().unwrap_or_else(||"msg_const_api".to_string()),
            "type":"message",
            "role":"assistant",
            "model":response.model.clone().unwrap_or_default(),
            "content":encode_response_blocks(
                &response.blocks,
                context,
                response.model.as_deref().unwrap_or(""),
            )?,
            "stop_reason":encode_stop_reason(&response.finish),
            "stop_sequence":response.finish.incomplete_details.as_ref().and_then(|details| details.get("stop_sequence")).cloned(),
            "usage":encode_usage(&response.usage),
        }))
    }
}

fn message_disposition(message: &Value) -> (MessageDisposition, bool) {
    let raw_role = message.get("role").and_then(Value::as_str);
    let normalized = raw_role.map(str::trim).map(str::to_ascii_lowercase);
    let disposition = match normalized.as_deref() {
        Some("assistant") => MessageDisposition::Turn(TurnRole::Assistant),
        Some("user") => MessageDisposition::Turn(TurnRole::User),
        Some("system") => MessageDisposition::SystemReminder,
        Some("developer") => MessageDisposition::Instruction(InstructionRole::Developer),
        Some("tool" | "function") => MessageDisposition::ToolResult,
        _ => MessageDisposition::Turn(infer_message_role(message.get("content"))),
    };
    let preserve_role_extension = !matches!(raw_role, Some("assistant" | "user" | "system"));
    (disposition, preserve_role_extension)
}

fn system_reminder_blocks(blocks: Vec<ContentBlock>) -> Vec<ContentBlock> {
    blocks
        .into_iter()
        .map(|block| match block {
            ContentBlock::Text(mut text) => {
                text.text = format!("<system-reminder>\n{}\n</system-reminder>", text.text);
                ContentBlock::Text(text)
            }
            other => other,
        })
        .collect()
}

fn split_complete_tool_results(
    blocks: Vec<ContentBlock>,
    pending_tool_call_ids: &[String],
) -> Result<(Vec<ContentBlock>, Vec<ContentBlock>), Vec<ContentBlock>> {
    if pending_tool_call_ids.is_empty() {
        return Err(blocks);
    }

    let mut results = HashMap::new();
    for (index, block) in blocks.iter().enumerate() {
        if let ContentBlock::ToolResult(result) = block {
            if results.insert(result.call_id.clone(), index).is_some() {
                return Err(blocks);
            }
        }
    }
    if results.len() != pending_tool_call_ids.len()
        || pending_tool_call_ids
            .iter()
            .any(|call_id| !results.contains_key(call_id))
    {
        return Err(blocks);
    }

    let mut moved_results = HashMap::with_capacity(results.len());
    let mut remaining = Vec::new();
    for block in blocks {
        match block {
            ContentBlock::ToolResult(result) => {
                moved_results.insert(result.call_id.clone(), ContentBlock::ToolResult(result));
            }
            other => remaining.push(other),
        }
    }
    let mut ordered = Vec::with_capacity(pending_tool_call_ids.len());
    for call_id in pending_tool_call_ids {
        if let Some(block) = moved_results.remove(call_id) {
            ordered.push(block);
        }
    }
    if ordered.len() != pending_tool_call_ids.len() || !moved_results.is_empty() {
        remaining.extend(ordered);
        remaining.extend(moved_results.into_values());
        return Err(remaining);
    }
    Ok((ordered, remaining))
}

fn update_pending_tool_calls(pending: &mut Vec<String>, turn: &Turn) {
    match turn.role {
        TurnRole::Assistant => {
            pending.clear();
            pending.extend(turn.blocks.iter().filter_map(|block| match block {
                ContentBlock::ToolCall(call) => Some(call.id.clone()),
                _ => None,
            }));
        }
        TurnRole::User => {
            for block in &turn.blocks {
                if let ContentBlock::ToolResult(result) = block {
                    pending.retain(|call_id| call_id != &result.call_id);
                }
            }
        }
    }
}

fn infer_message_role(content: Option<&Value>) -> TurnRole {
    let Some(items) = content.and_then(Value::as_array) else {
        return TurnRole::User;
    };
    if items.iter().any(|item| {
        matches!(
            item.get("type").and_then(Value::as_str),
            Some(
                "tool_result"
                    | "mcp_tool_result"
                    | "web_search_tool_result"
                    | "web_fetch_tool_result"
                    | "code_execution_tool_result"
                    | "bash_code_execution_tool_result"
                    | "text_editor_code_execution_tool_result"
                    | "tool_search_tool_result"
            )
        )
    }) {
        return TurnRole::User;
    }
    if items.iter().any(|item| {
        matches!(
            item.get("type").and_then(Value::as_str),
            Some(
                "tool_use" | "server_tool_use" | "mcp_tool_use" | "thinking" | "redacted_thinking"
            )
        )
    }) {
        return TurnRole::Assistant;
    }
    TurnRole::User
}

fn message_role_extension(index: usize, message: &Value) -> ProviderExtension {
    ProviderExtension {
        namespace: "anthropic_messages".to_string(),
        name: ProviderExtension::nested_name(&format!("$.messages[{index}].role")),
        value: message.get("role").cloned().unwrap_or(Value::Null),
        affinity: protocol_affinity(ProtocolKind::AnthropicMessages),
        criticality: ExtensionCriticality::Advisory,
    }
}

fn decode_compat_tool_result(
    message: &Value,
    content: &Value,
    context: &AdapterContext,
    model: &str,
    path: &str,
    call_names: &mut HashMap<String, String>,
) -> Result<Vec<ContentBlock>, AdapterError> {
    let blocks = decode_blocks(content, context, model, path, call_names)?;
    let Some(call_id) = message
        .get("tool_use_id")
        .or_else(|| message.get("tool_call_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(blocks);
    };
    Ok(vec![ContentBlock::ToolResult(ToolResult {
        call_id: call_id.to_string(),
        name: message
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| call_names.get(call_id).cloned()),
        content: blocks,
        status: ItemStatus::Completed,
        is_error: false,
        metadata: BlockMetadata::default(),
    })])
}

fn required_string<'a>(
    value: Option<&'a Value>,
    path: &str,
    code: &'static str,
) -> Result<&'a str, AdapterError> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AdapterError::new(code, path, "a non-empty string is required"))
}

fn decode_system(
    value: Option<&Value>,
    context: &AdapterContext,
    model: &str,
) -> Result<Vec<Instruction>, AdapterError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    if let Some(text) = value.as_str() {
        return Ok(vec![Instruction {
            role: InstructionRole::System,
            blocks: vec![ContentBlock::Text(TextBlock::new(text))],
        }]);
    }
    let items = value.as_array().ok_or_else(|| {
        AdapterError::new(
            "system_invalid",
            "$.system",
            "system must be a string or array",
        )
    })?;
    let mut calls = HashMap::new();
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            Ok(Instruction {
                role: InstructionRole::System,
                blocks: decode_blocks(
                    &Value::Array(vec![item.clone()]),
                    context,
                    model,
                    &format!("$.system[{index}]"),
                    &mut calls,
                )?,
            })
        })
        .collect()
}

fn decode_blocks(
    value: &Value,
    context: &AdapterContext,
    model: &str,
    path: &str,
    call_names: &mut HashMap<String, String>,
) -> Result<Vec<ContentBlock>, AdapterError> {
    if let Some(text) = value.as_str() {
        return Ok(vec![ContentBlock::Text(TextBlock::new(text))]);
    }
    let items = value.as_array().ok_or_else(|| {
        AdapterError::new("content_invalid", path, "content must be a string or array")
    })?;
    let mut blocks = Vec::new();
    let mut pending_tool_artifacts = HashMap::<String, Vec<OpaqueArtifact>>::new();
    for (index, item) in items.iter().enumerate() {
        let block_path = format!("{path}[{index}]");
        let metadata = block_metadata(item);
        match item.get("type").and_then(Value::as_str).unwrap_or("text") {
            "text" => blocks.push(ContentBlock::Text(TextBlock {
                text: item
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                metadata: BlockMetadata {
                    annotations: item
                        .get("citations")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default(),
                    ..metadata
                },
            })),
            "image" => blocks.push(ContentBlock::Image(decode_media(item, metadata, "image")?)),
            "document" => match decode_document(item, metadata) {
                Ok(document) => blocks.push(ContentBlock::File(document)),
                Err(error) if error.code() == "unsupported_media_source" => {
                    blocks.push(protocol_block(item))
                }
                Err(error) => return Err(error),
            },
            "thinking" => {
                let signature = item.get("signature").cloned().ok_or_else(|| {
                    AdapterError::new(
                        "anthropic_thinking_signature_missing",
                        format!("{block_path}.signature"),
                        "signed thinking block requires signature",
                    )
                })?;
                match decode_carrier(&signature).map_err(|error| {
                    AdapterError::new(
                        "continuation_carrier_invalid",
                        format!("{block_path}.signature"),
                        error.message(),
                    )
                })? {
                    Some(carrier) => {
                        validate_anthropic_carrier_adjacency(&carrier, items, index, &block_path)?;
                        let (reasoning_artifacts, previous_artifacts) =
                            distribute_anthropic_carrier(carrier, &mut pending_tool_artifacts);
                        if !reasoning_artifacts.is_empty() {
                            let encrypted = reasoning_artifacts_are_encrypted(&reasoning_artifacts);
                            blocks.push(ContentBlock::Reasoning(ReasoningBlock {
                                text: item
                                    .get("thinking")
                                    .and_then(Value::as_str)
                                    .map(str::to_string),
                                summary: Vec::new(),
                                artifacts: reasoning_artifacts,
                                encrypted,
                                metadata,
                            }));
                        }
                        blocks.extend(
                            previous_artifacts
                                .into_iter()
                                .map(ContentBlock::ProviderArtifact),
                        );
                    }
                    None => blocks.push(ContentBlock::Reasoning(ReasoningBlock {
                        text: item
                            .get("thinking")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        summary: Vec::new(),
                        artifacts: vec![OpaqueArtifact {
                            kind: ArtifactKind::AnthropicThinkingSignature,
                            payload: signature,
                            affinity: continuation_artifact_affinity(
                                context,
                                model,
                                ProtocolKind::AnthropicMessages,
                            ),
                            replay: ReplayPolicy::Required,
                            criticality: ArtifactCriticality::Required,
                        }],
                        encrypted: false,
                        metadata,
                    })),
                }
            }
            "redacted_thinking" => blocks.push(ContentBlock::Reasoning(ReasoningBlock {
                text: None,
                summary: Vec::new(),
                artifacts: vec![OpaqueArtifact {
                    kind: ArtifactKind::AnthropicRedactedThinking,
                    payload: item.get("data").cloned().unwrap_or(Value::Null),
                    affinity: continuation_artifact_affinity(
                        context,
                        model,
                        ProtocolKind::AnthropicMessages,
                    ),
                    replay: ReplayPolicy::Required,
                    criticality: ArtifactCriticality::Required,
                }],
                encrypted: true,
                metadata,
            })),
            "tool_use" => {
                let id = required_string(
                    item.get("id"),
                    &format!("{block_path}.id"),
                    "tool_call_id_missing",
                )?;
                let name = required_string(
                    item.get("name"),
                    &format!("{block_path}.name"),
                    "tool_name_missing",
                )?;
                call_names.insert(id.to_string(), name.to_string());
                let call = ToolCall {
                    id: id.to_string(),
                    source_item_id: None,
                    kind: ToolKind::Function,
                    name: name.to_string(),
                    arguments: Some(item.get("input").cloned().unwrap_or_else(|| json!({}))),
                    raw_arguments: None,
                    status: ItemStatus::Completed,
                    artifacts: pending_tool_artifacts.remove(id).unwrap_or_default(),
                    metadata,
                };
                blocks.push(ContentBlock::ToolCall(call));
            }
            "server_tool_use" | "mcp_tool_use" => {
                let call = decode_hosted_tool_call(item, &block_path, metadata)?;
                call_names.insert(call.id.clone(), call.name.clone());
                blocks.push(ContentBlock::ToolCall(call));
            }
            "tool_result" => {
                let call_id = required_string(
                    item.get("tool_use_id"),
                    &format!("{block_path}.tool_use_id"),
                    "tool_call_id_missing",
                )?;
                blocks.push(ContentBlock::ToolResult(ToolResult {
                    call_id: call_id.to_string(),
                    name: call_names.get(call_id).cloned(),
                    content: decode_blocks(
                        item.get("content").unwrap_or(&Value::Null),
                        context,
                        model,
                        &format!("{block_path}.content"),
                        call_names,
                    )?,
                    status: ItemStatus::Completed,
                    is_error: item
                        .get("is_error")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    metadata,
                }));
            }
            "mcp_tool_result"
            | "web_search_tool_result"
            | "web_fetch_tool_result"
            | "code_execution_tool_result"
            | "bash_code_execution_tool_result"
            | "text_editor_code_execution_tool_result"
            | "tool_search_tool_result" => {
                blocks.push(ContentBlock::ToolResult(decode_hosted_tool_result(
                    item,
                    &block_path,
                    call_names,
                    metadata,
                )?));
            }
            "search_result" => blocks.push(decode_search_result(item, metadata)),
            // Compaction blocks are opaque continuation state. Their encrypted payload must
            // round-trip byte-for-byte to Anthropic and must never become ordinary visible text.
            "compaction" => blocks.push(protocol_block(item)),
            _ => blocks.push(protocol_block(item)),
        }
    }
    if let Some(call_id) = pending_tool_artifacts.keys().next() {
        return Err(AdapterError::new(
            "continuation_carrier_binding_invalid",
            path,
            format!("continuation carrier has no adjacent tool_use {call_id}"),
        ));
    }
    Ok(blocks)
}

fn validate_anthropic_carrier_adjacency(
    carrier: &ContinuationCarrier,
    items: &[Value],
    index: usize,
    path: &str,
) -> Result<(), AdapterError> {
    let tool_call_ids = carrier
        .entries
        .iter()
        .filter_map(|entry| match &entry.owner {
            ContinuationOwner::ToolCall { call_id } => Some(call_id.as_str()),
            ContinuationOwner::Reasoning | ContinuationOwner::PreviousPart => None,
        })
        .collect::<Vec<_>>();
    if tool_call_ids.is_empty() {
        return Ok(());
    }
    let next_call_id = items
        .get(index + 1)
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("tool_use"))
        .and_then(|item| item.get("id"))
        .and_then(Value::as_str);
    if tool_call_ids
        .iter()
        .all(|call_id| Some(*call_id) == next_call_id)
    {
        return Ok(());
    }
    Err(AdapterError::new(
        "continuation_carrier_binding_invalid",
        path,
        "tool continuation carrier must immediately precede its matching tool_use block",
    ))
}

fn distribute_anthropic_carrier(
    carrier: ContinuationCarrier,
    pending_tool_artifacts: &mut HashMap<String, Vec<OpaqueArtifact>>,
) -> (Vec<OpaqueArtifact>, Vec<OpaqueArtifact>) {
    let mut reasoning = Vec::new();
    let mut previous = Vec::new();
    for entry in carrier.entries {
        match entry.owner {
            ContinuationOwner::Reasoning => reasoning.push(entry.artifact),
            ContinuationOwner::ToolCall { call_id } => pending_tool_artifacts
                .entry(call_id)
                .or_default()
                .push(entry.artifact),
            ContinuationOwner::PreviousPart => previous.push(entry.artifact),
        }
    }
    (reasoning, previous)
}

fn protocol_block(value: &Value) -> ContentBlock {
    ContentBlock::ProviderArtifact(protocol_artifact(value))
}

fn protocol_artifact(value: &Value) -> OpaqueArtifact {
    OpaqueArtifact {
        kind: ArtifactKind::ProviderSpecific,
        payload: value.clone(),
        affinity: protocol_affinity(ProtocolKind::AnthropicMessages),
        replay: ReplayPolicy::ExactWhenCompatible,
        criticality: ArtifactCriticality::Supplemental,
    }
}

fn decode_hosted_tool_call(
    item: &Value,
    path: &str,
    metadata: BlockMetadata,
) -> Result<ToolCall, AdapterError> {
    let id = required_string(
        item.get("id"),
        &format!("{path}.id"),
        "tool_call_id_missing",
    )?;
    let name = required_string(
        item.get("name"),
        &format!("{path}.name"),
        "tool_name_missing",
    )?;
    Ok(ToolCall {
        id: id.to_string(),
        source_item_id: None,
        kind: ToolKind::Hosted,
        name: name.to_string(),
        arguments: Some(item.get("input").cloned().unwrap_or_else(|| json!({}))),
        raw_arguments: None,
        status: ItemStatus::Completed,
        artifacts: vec![protocol_artifact(item)],
        metadata,
    })
}

fn decode_hosted_tool_result(
    item: &Value,
    path: &str,
    call_names: &HashMap<String, String>,
    metadata: BlockMetadata,
) -> Result<ToolResult, AdapterError> {
    let call_id = required_string(
        item.get("tool_use_id"),
        &format!("{path}.tool_use_id"),
        "tool_call_id_missing",
    )?;
    let content = item.get("content").unwrap_or(&Value::Null);
    let mut blocks = hosted_result_text_blocks(content);
    blocks.push(protocol_block(item));
    let is_error = item
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| contains_error_result(content));
    Ok(ToolResult {
        call_id: call_id.to_string(),
        name: call_names.get(call_id).cloned(),
        content: blocks,
        status: ItemStatus::Completed,
        is_error,
        metadata,
    })
}

fn hosted_result_text_blocks(value: &Value) -> Vec<ContentBlock> {
    let values = value.as_array().map(Vec::as_slice).unwrap_or_else(|| {
        if value.is_null() {
            &[]
        } else {
            std::slice::from_ref(value)
        }
    });
    values
        .iter()
        .filter_map(|value| {
            if let Some(text) = value.get("text").and_then(Value::as_str) {
                return Some(ContentBlock::Text(TextBlock::new(text)));
            }
            if value.get("type").and_then(Value::as_str) == Some("web_search_result") {
                let title = value
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let url = value.get("url").and_then(Value::as_str).unwrap_or_default();
                return Some(ContentBlock::Text(TextBlock {
                    text: [title, url]
                        .into_iter()
                        .filter(|part| !part.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n"),
                    metadata: BlockMetadata {
                        annotations: vec![value.clone()],
                        ..Default::default()
                    },
                }));
            }
            (!value.is_null()).then(|| {
                ContentBlock::Text(TextBlock::new(
                    serde_json::to_string(value).unwrap_or_default(),
                ))
            })
        })
        .collect()
}

fn contains_error_result(value: &Value) -> bool {
    let is_error = |value: &Value| {
        value
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind.ends_with("_error"))
            || value.get("error_code").is_some()
    };
    value
        .as_array()
        .map(|values| values.iter().any(is_error))
        .unwrap_or_else(|| is_error(value))
}

fn decode_search_result(item: &Value, mut metadata: BlockMetadata) -> ContentBlock {
    metadata.annotations.push(item.clone());
    let text = item
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    ContentBlock::Text(TextBlock { text, metadata })
}

fn block_metadata(value: &Value) -> BlockMetadata {
    BlockMetadata {
        cache_policy: decode_cache_policy(value.get("cache_control")),
        ..Default::default()
    }
}

fn decode_cache_policy(value: Option<&Value>) -> Option<CachePolicy> {
    let value = value?;
    match value.get("type").and_then(Value::as_str) {
        Some("ephemeral") if value.get("ttl").and_then(Value::as_str) == Some("1h") => {
            Some(CachePolicy::Ephemeral1h)
        }
        Some("ephemeral") => Some(CachePolicy::Ephemeral5m),
        Some("persistent") => Some(CachePolicy::Persistent),
        Some("no_store") => Some(CachePolicy::NoStore),
        _ => None,
    }
}

fn encode_cache_policy(policy: &Option<CachePolicy>) -> Option<Value> {
    match policy {
        Some(CachePolicy::Ephemeral5m) => Some(json!({"type":"ephemeral"})),
        Some(CachePolicy::Ephemeral1h) => Some(json!({"type":"ephemeral","ttl":"1h"})),
        Some(CachePolicy::Persistent) => Some(json!({"type":"persistent"})),
        Some(CachePolicy::NoStore) => Some(json!({"type":"no_store"})),
        None => None,
    }
}

fn decode_media(
    item: &Value,
    metadata: BlockMetadata,
    kind: &str,
) -> Result<MediaBlock, AdapterError> {
    let source = item.get("source").ok_or_else(|| {
        AdapterError::new(
            "media_source_missing",
            "$.content.source",
            "source is required",
        )
    })?;
    let source = match source.get("type").and_then(Value::as_str) {
        Some("base64") => MediaSource::InlineBase64 {
            media_type: required_string(
                source.get("media_type"),
                "$.content.source.media_type",
                "media_type_missing",
            )?
            .to_string(),
            data: required_string(
                source.get("data"),
                "$.content.source.data",
                "media_data_missing",
            )?
            .to_string(),
        },
        Some("url") => MediaSource::RemoteUrl {
            url: required_string(
                source.get("url"),
                "$.content.source.url",
                "media_url_missing",
            )?
            .to_string(),
        },
        Some("file") => MediaSource::ProviderFileId {
            provider: "anthropic".to_string(),
            id: required_string(
                source.get("file_id"),
                "$.content.source.file_id",
                "file_id_missing",
            )?
            .to_string(),
            media_type: None,
        },
        _ => {
            return Err(AdapterError::new(
                "unsupported_media_source",
                "$.content.source.type",
                format!("unsupported Anthropic {kind} source"),
            ));
        }
    };
    Ok(MediaBlock {
        source,
        detail: None,
        metadata,
    })
}

fn decode_document(item: &Value, metadata: BlockMetadata) -> Result<FileBlock, AdapterError> {
    let media = decode_media(item, metadata.clone(), "document")?;
    Ok(FileBlock {
        source: media.source,
        filename: item
            .get("title")
            .or_else(|| item.get("filename"))
            .and_then(Value::as_str)
            .map(str::to_string),
        metadata,
    })
}

fn decode_tools(value: Option<&Value>) -> Result<Vec<ToolDefinition>, AdapterError> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(index, tool)| {
            if tool
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| !matches!(kind, "custom" | "function"))
            {
                let kind = tool
                    .get("type")
                    .and_then(Value::as_str)
                    .map(anthropic_hosted_tool_kind)
                    .unwrap_or(HostedToolKind::ProviderSpecific);
                return Ok(ToolDefinition::Hosted(HostedTool {
                    kind,
                    provider: Some("anthropic".to_string()),
                    name: tool.get("name").and_then(Value::as_str).map(str::to_string),
                    config: tool.clone(),
                }));
            }
            let empty = Value::Object(Map::new());
            let schema = canonicalize_tool_schema(tool.get("input_schema").unwrap_or(&empty))
                .map_err(|error| {
                    AdapterError::new(
                        error.code(),
                        format!("$.tools[{index}].input_schema"),
                        error.to_string(),
                    )
                })?;
            Ok(ToolDefinition::Function(FunctionTool {
                name: required_string(tool.get("name"), "$.tools.name", "tool_name_missing")?
                    .to_string(),
                description: tool
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                input_schema: schema,
                strict: tool.get("strict").and_then(Value::as_bool),
                cache_policy: decode_cache_policy(tool.get("cache_control")),
                defer_loading: tool.get("defer_loading").and_then(Value::as_bool),
                allowed_callers: tool
                    .get("allowed_callers")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect(),
                input_examples: tool
                    .get("input_examples")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
                eager_input_streaming: tool.get("eager_input_streaming").and_then(Value::as_bool),
            }))
        })
        .collect()
}

fn anthropic_hosted_tool_kind(kind: &str) -> HostedToolKind {
    if kind.starts_with("web_search") || kind.starts_with("web_fetch") {
        HostedToolKind::WebSearch
    } else if kind.starts_with("code_execution")
        || kind.starts_with("bash")
        || kind.starts_with("text_editor")
    {
        HostedToolKind::CodeExecution
    } else if kind.starts_with("computer") {
        HostedToolKind::ComputerUse
    } else if kind.starts_with("mcp") {
        HostedToolKind::Mcp
    } else {
        HostedToolKind::ProviderSpecific
    }
}

fn decode_tool_choice(value: Option<&Value>) -> Result<ToolChoice, AdapterError> {
    let Some(value) = value else {
        return Ok(ToolChoice::Auto);
    };
    Ok(
        match value.get("type").and_then(Value::as_str).unwrap_or("auto") {
            "auto" => ToolChoice::Auto,
            "any" => ToolChoice::Required,
            "none" => ToolChoice::None,
            "tool" => ToolChoice::Named {
                name: required_string(
                    value.get("name"),
                    "$.tool_choice.name",
                    "tool_name_missing",
                )?
                .to_string(),
            },
            kind => {
                return Err(AdapterError::new(
                    "unsupported_tool_choice",
                    "$.tool_choice.type",
                    format!("unsupported Anthropic tool choice {kind}"),
                ));
            }
        },
    )
}

fn decode_thinking(body: &Value) -> ReasoningConfig {
    let thinking = body.get("thinking");
    let thinking_type = thinking
        .and_then(|value| value.get("type"))
        .and_then(Value::as_str);
    let mode = match thinking_type {
        Some("adaptive") => ReasoningMode::Adaptive,
        Some("enabled") => ReasoningMode::Enabled,
        _ => ReasoningMode::Disabled,
    };
    let effort = body
        .pointer("/output_config/effort")
        .and_then(Value::as_str)
        .and_then(ReasoningEffort::parse)
        .or_else(|| (thinking_type == Some("disabled")).then_some(ReasoningEffort::None));
    ReasoningConfig {
        mode,
        effort,
        token_budget: thinking
            .and_then(|value| value.get("budget_tokens"))
            .and_then(Value::as_u64),
        summary: ReasoningSummaryMode::None,
        provider_extensions: Vec::new(),
    }
}

fn decode_response_format(body: &Value) -> Result<ResponseFormat, AdapterError> {
    let Some((format, path)) = body
        .pointer("/output_config/format")
        .map(|format| (format, "$.output_config.format"))
        .or_else(|| {
            body.get("output_format")
                .map(|format| (format, "$.output_format"))
        })
    else {
        return Ok(ResponseFormat::Text);
    };
    match format.get("type").and_then(Value::as_str).unwrap_or("text") {
        "text" => Ok(ResponseFormat::Text),
        "json_schema" => {
            let empty = Value::Object(Map::new());
            let schema =
                canonicalize_schema(format.get("schema").unwrap_or(&empty)).map_err(|error| {
                    AdapterError::new(error.code(), format!("{path}.schema"), error.to_string())
                })?;
            Ok(ResponseFormat::JsonSchema {
                name: format
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("response")
                    .to_string(),
                description: format
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                schema,
                strict: Some(true),
            })
        }
        kind => Err(AdapterError::new(
            "unsupported_response_format",
            format!("{path}.type"),
            format!("unsupported Anthropic output format {kind}"),
        )),
    }
}

fn decode_generation(body: &Value) -> GenerationConfig {
    GenerationConfig {
        max_output_tokens: body.get("max_tokens").and_then(Value::as_u64),
        temperature: body.get("temperature").and_then(Value::as_f64),
        top_p: body.get("top_p").and_then(Value::as_f64),
        top_k: body.get("top_k").and_then(Value::as_u64),
        stop: body
            .get("stop_sequences")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        service_tier: body
            .get("service_tier")
            .and_then(Value::as_str)
            .map(str::to_string),
        parallel_tool_calls: body
            .pointer("/tool_choice/disable_parallel_tool_use")
            .and_then(Value::as_bool)
            .map(|disabled| !disabled),
        ..Default::default()
    }
}

fn decode_extensions(
    body: &Value,
    _context: &AdapterContext,
    _model: &str,
) -> Vec<ProviderExtension> {
    let recognized = [
        "model",
        "system",
        "messages",
        "tools",
        "tool_choice",
        "thinking",
        "output_config",
        "output_format",
        "max_tokens",
        "temperature",
        "top_p",
        "top_k",
        "stop_sequences",
        "service_tier",
        "stream",
        "metadata",
    ];
    body.as_object()
        .into_iter()
        .flat_map(|object| object.iter())
        .filter(|(name, _)| !recognized.contains(&name.as_str()))
        .map(|(name, value)| ProviderExtension {
            namespace: "anthropic_messages".to_string(),
            name: name.clone(),
            value: value.clone(),
            affinity: protocol_affinity(ProtocolKind::AnthropicMessages),
            criticality: ExtensionCriticality::Advisory,
        })
        .collect()
}

fn decode_response_extensions(
    body: &Value,
    context: &AdapterContext,
    model: &str,
) -> Vec<ProviderExtension> {
    let recognized = [
        "id",
        "type",
        "role",
        "model",
        "content",
        "stop_reason",
        "stop_sequence",
        "usage",
        "error",
        "request_id",
    ];
    let mut extensions = body
        .as_object()
        .into_iter()
        .flat_map(|object| object.iter())
        .filter(|(name, _)| !recognized.contains(&name.as_str()))
        .map(|(name, value)| ProviderExtension {
            namespace: "anthropic_messages".to_string(),
            name: name.clone(),
            value: value.clone(),
            affinity: protocol_affinity(ProtocolKind::AnthropicMessages),
            criticality: ExtensionCriticality::Advisory,
        })
        .collect::<Vec<_>>();
    scan_anthropic_blocks(
        &mut extensions,
        body.get("content"),
        "$.content",
        context,
        model,
    );
    scan_anthropic_usage_extensions(
        &mut extensions,
        body.get("usage"),
        "$.usage",
        context,
        model,
    );
    collect_nested_unknown_fields(
        &mut extensions,
        "anthropic_messages",
        body.get("error"),
        &["type", "message", "retryable", "details"],
        "$.error",
        context,
        model,
    );
    extensions
}

fn scan_anthropic_usage_extensions(
    extensions: &mut Vec<ProviderExtension>,
    value: Option<&Value>,
    path: &str,
    context: &AdapterContext,
    model: &str,
) {
    collect_nested_unknown_fields(
        extensions,
        "anthropic_messages",
        value,
        &[
            "input_tokens",
            "output_tokens",
            "cache_read_input_tokens",
            "cache_creation_input_tokens",
            "cache_creation",
            "server_tool_use",
        ],
        path,
        context,
        model,
    );
    collect_nested_unknown_fields(
        extensions,
        "anthropic_messages",
        value.and_then(|usage| usage.get("cache_creation")),
        &["ephemeral_5m_input_tokens", "ephemeral_1h_input_tokens"],
        &format!("{path}.cache_creation"),
        context,
        model,
    );
    collect_nested_unknown_fields(
        extensions,
        "anthropic_messages",
        value.and_then(|usage| usage.get("server_tool_use")),
        &[],
        &format!("{path}.server_tool_use"),
        context,
        model,
    );
}

fn decode_nested_extensions(
    body: &Value,
    context: &AdapterContext,
    model: &str,
) -> Vec<ProviderExtension> {
    let mut extensions = Vec::new();
    scan_anthropic_blocks(
        &mut extensions,
        body.get("system"),
        "$.system",
        context,
        model,
    );
    for (message_index, message) in body
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let message_path = format!("$.messages[{message_index}]");
        collect_nested_unknown_fields(
            &mut extensions,
            "anthropic_messages",
            Some(message),
            &["id", "role", "content"],
            &message_path,
            context,
            model,
        );
        scan_anthropic_blocks(
            &mut extensions,
            message.get("content"),
            &format!("{message_path}.content"),
            context,
            model,
        );
    }
    for (tool_index, tool) in body
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let tool_path = format!("$.tools[{tool_index}]");
        if tool
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| !matches!(kind, "custom" | "function"))
        {
            // Hosted tools retain their complete, versioned provider configuration in the IR.
            continue;
        }
        collect_nested_unknown_fields(
            &mut extensions,
            "anthropic_messages",
            Some(tool),
            &[
                "name",
                "description",
                "input_schema",
                "strict",
                "cache_control",
                "type",
                "defer_loading",
                "allowed_callers",
                "input_examples",
                "eager_input_streaming",
            ],
            &tool_path,
            context,
            model,
        );
        scan_cache_control(
            &mut extensions,
            tool.get("cache_control"),
            &format!("{tool_path}.cache_control"),
            context,
            model,
        );
    }
    collect_nested_unknown_fields(
        &mut extensions,
        "anthropic_messages",
        body.get("tool_choice"),
        &["type", "name", "disable_parallel_tool_use"],
        "$.tool_choice",
        context,
        model,
    );
    collect_nested_unknown_fields(
        &mut extensions,
        "anthropic_messages",
        body.get("thinking"),
        &["type", "budget_tokens"],
        "$.thinking",
        context,
        model,
    );
    collect_nested_unknown_fields(
        &mut extensions,
        "anthropic_messages",
        body.get("output_config"),
        &["effort", "format"],
        "$.output_config",
        context,
        model,
    );
    collect_nested_unknown_fields(
        &mut extensions,
        "anthropic_messages",
        body.pointer("/output_config/format"),
        &["type", "name", "description", "schema"],
        "$.output_config.format",
        context,
        model,
    );
    collect_nested_unknown_fields(
        &mut extensions,
        "anthropic_messages",
        body.get("output_format"),
        &["type", "name", "description", "schema"],
        "$.output_format",
        context,
        model,
    );
    collect_nested_unknown_fields(
        &mut extensions,
        "anthropic_messages",
        body.get("metadata"),
        &["user_id"],
        "$.metadata",
        context,
        model,
    );
    extensions
}

fn scan_anthropic_blocks(
    extensions: &mut Vec<ProviderExtension>,
    value: Option<&Value>,
    path: &str,
    context: &AdapterContext,
    model: &str,
) {
    let Some(blocks) = value.and_then(Value::as_array) else {
        return;
    };
    for (index, block) in blocks.iter().enumerate() {
        let block_path = format!("{path}[{index}]");
        let recognized: &[&str] = match block.get("type").and_then(Value::as_str) {
            Some("text") | None => &["type", "text", "citations", "cache_control"],
            Some("image") => &["type", "source", "cache_control"],
            Some("document") => &["type", "source", "title", "filename", "cache_control"],
            Some("thinking") => &["type", "thinking", "signature"],
            Some("redacted_thinking") => &["type", "data"],
            Some("tool_use") => &["type", "id", "name", "input", "cache_control"],
            Some("server_tool_use") => &["type", "id", "name", "input", "caller", "cache_control"],
            Some("mcp_tool_use") => &[
                "type",
                "id",
                "name",
                "input",
                "server_name",
                "cache_control",
            ],
            Some("tool_result") => &[
                "type",
                "tool_use_id",
                "content",
                "is_error",
                "cache_control",
            ],
            Some(
                "mcp_tool_result"
                | "web_search_tool_result"
                | "web_fetch_tool_result"
                | "code_execution_tool_result"
                | "bash_code_execution_tool_result"
                | "text_editor_code_execution_tool_result"
                | "tool_search_tool_result",
            ) => &[
                "type",
                "tool_use_id",
                "content",
                "caller",
                "is_error",
                "cache_control",
            ],
            Some("search_result") => &[
                "type",
                "content",
                "source",
                "title",
                "citations",
                "cache_control",
            ],
            Some("compaction") => &["type", "content", "encrypted_content", "cache_control"],
            Some(_) => &["type"],
        };
        collect_nested_unknown_fields(
            extensions,
            "anthropic_messages",
            Some(block),
            recognized,
            &block_path,
            context,
            model,
        );
        scan_cache_control(
            extensions,
            block.get("cache_control"),
            &format!("{block_path}.cache_control"),
            context,
            model,
        );
        collect_nested_unknown_fields(
            extensions,
            "anthropic_messages",
            block.get("source"),
            &["type", "media_type", "data", "url", "file_id"],
            &format!("{block_path}.source"),
            context,
            model,
        );
        if block.get("type").and_then(Value::as_str) == Some("tool_result") {
            scan_anthropic_blocks(
                extensions,
                block.get("content"),
                &format!("{block_path}.content"),
                context,
                model,
            );
        }
    }
}

fn scan_cache_control(
    extensions: &mut Vec<ProviderExtension>,
    value: Option<&Value>,
    path: &str,
    context: &AdapterContext,
    model: &str,
) {
    collect_nested_unknown_fields(
        extensions,
        "anthropic_messages",
        value,
        &["type", "ttl"],
        path,
        context,
        model,
    );
}

fn encode_instructions(
    instructions: &[Instruction],
    plan: &ConversionPlan,
) -> Result<Vec<Value>, AdapterError> {
    let mut output = Vec::new();
    for (index, instruction) in instructions.iter().enumerate() {
        if instruction.role == InstructionRole::Developer
            && !plan.feature_actions().iter().any(|action| {
                action.path == format!("instructions[{index}].role")
                    && action.action == FeatureDisposition::CoalesceRoleEnvelope
            })
        {
            return Err(AdapterError::new(
                "encoder_plan_violation",
                format!("instructions[{index}].role"),
                "plan does not authorize developer-to-system coalescing",
            ));
        }
        output.extend(encode_blocks(
            &instruction.blocks,
            plan,
            &format!("instructions[{index}].blocks"),
        )?);
    }
    Ok(output)
}

fn encode_turns(turns: &[Turn], plan: &ConversionPlan) -> Result<Vec<Value>, AdapterError> {
    let mut output: Vec<Value> = Vec::new();
    for (index, turn) in turns.iter().enumerate() {
        let role = anthropic_role(turn.role);
        let content = encode_blocks(&turn.blocks, plan, &format!("turns[{index}].blocks"))?;
        if let Some(previous) = output.last_mut().filter(|message| message["role"] == role) {
            if let Some(previous_content) = previous["content"].as_array_mut() {
                previous_content.extend(content);
            } else {
                let existing = previous
                    .get_mut("content")
                    .map(Value::take)
                    .unwrap_or(Value::Null);
                let mut merged = match existing {
                    Value::String(text) if !text.is_empty() => {
                        vec![json!({"type":"text","text":text})]
                    }
                    Value::Array(values) => values,
                    Value::Null => Vec::new(),
                    value => vec![value],
                };
                merged.extend(content);
                previous["content"] = Value::Array(merged);
            }
        } else {
            let adjacent_same_role = turns
                .get(index + 1)
                .is_some_and(|next| anthropic_role(next.role) == role);
            let content = if !adjacent_same_role {
                simple_text_content(&turn.blocks).unwrap_or(Value::Array(content))
            } else {
                Value::Array(content)
            };
            output.push(json!({"role":role,"content":content}));
        }
    }
    Ok(output)
}

fn anthropic_role(role: TurnRole) -> &'static str {
    if role == TurnRole::Assistant {
        "assistant"
    } else {
        "user"
    }
}

fn simple_text_content(blocks: &[ContentBlock]) -> Option<Value> {
    let [ContentBlock::Text(text)] = blocks else {
        return None;
    };
    (text.metadata == BlockMetadata::default()).then(|| Value::String(text.text.clone()))
}

fn encode_blocks(
    blocks: &[ContentBlock],
    plan: &ConversionPlan,
    base_path: &str,
) -> Result<Vec<Value>, AdapterError> {
    let mut output = Vec::new();
    for (index, block) in blocks.iter().enumerate() {
        let path = format!("{base_path}[{index}]");
        if matches!(block, ContentBlock::Reasoning(_))
            && plan.omits_feature_at(Feature::Reasoning, &path)
        {
            continue;
        }
        let mut value = match block {
            ContentBlock::Text(text) => {
                json!({"type":"text","text":text.text,"citations":text.metadata.annotations})
            }
            ContentBlock::Image(media) => encode_media("image", &media.source)?,
            ContentBlock::File(file) => {
                let mut value = encode_media("document", &file.source)?;
                if let Some(filename) = &file.filename {
                    value["title"] = json!(filename);
                }
                value
            }
            ContentBlock::Reasoning(reasoning) => encode_reasoning(reasoning, plan, &path)?,
            ContentBlock::ToolCall(call) => json!({
                "type":"tool_use","id":call.id,"name":call.name,
                "input":call.arguments.clone().unwrap_or_else(||json!({})),
            }),
            ContentBlock::ToolResult(result) => json!({
                "type":"tool_result","tool_use_id":result.call_id,
                "content":encode_blocks(&result.content, plan, &format!("{path}.content"))?,
                "is_error":result.is_error,
            }),
            ContentBlock::ProviderArtifact(artifact) if artifact_is_preserved(plan, &path) => {
                artifact.payload.clone()
            }
            ContentBlock::ProviderArtifact(_) => continue,
            _ => {
                return Err(AdapterError::new(
                    "encoder_plan_violation",
                    path,
                    "unsupported Anthropic block mapping",
                ));
            }
        };
        let policy = match block {
            ContentBlock::Text(value) => &value.metadata.cache_policy,
            ContentBlock::Image(value)
            | ContentBlock::Audio(value)
            | ContentBlock::Video(value) => &value.metadata.cache_policy,
            ContentBlock::File(value) => &value.metadata.cache_policy,
            ContentBlock::Reasoning(value) => &value.metadata.cache_policy,
            ContentBlock::ToolCall(value) => &value.metadata.cache_policy,
            ContentBlock::ToolResult(value) => &value.metadata.cache_policy,
            _ => &None,
        };
        if let Some(cache) = encode_cache_policy(policy) {
            value["cache_control"] = cache;
        }
        output.push(value);
    }
    Ok(output)
}

fn encode_reasoning(
    reasoning: &ReasoningBlock,
    plan: &ConversionPlan,
    path: &str,
) -> Result<Value, AdapterError> {
    if reasoning.encrypted {
        let artifact = reasoning
            .artifacts
            .iter()
            .enumerate()
            .find(|(index, artifact)| {
                artifact.kind == ArtifactKind::AnthropicRedactedThinking
                    && artifact_is_preserved(plan, &format!("{path}.artifacts[{index}]"))
            })
            .map(|(_, artifact)| artifact)
            .ok_or_else(|| {
                AdapterError::new(
                    "anthropic_redacted_thinking_missing",
                    path,
                    "redacted thinking payload is required",
                )
            })?;
        return Ok(json!({"type":"redacted_thinking","data":artifact.payload}));
    }
    let artifact = reasoning
        .artifacts
        .iter()
        .enumerate()
        .find(|(index, artifact)| {
            artifact.kind == ArtifactKind::AnthropicThinkingSignature
                && artifact_is_preserved(plan, &format!("{path}.artifacts[{index}]"))
        })
        .map(|(_, artifact)| artifact)
        .ok_or_else(|| {
            AdapterError::new(
                "anthropic_thinking_signature_missing",
                path,
                "thinking signature is required",
            )
        })?;
    Ok(json!({
        "type":"thinking",
        "thinking":reasoning.text.clone().unwrap_or_default(),
        "signature":artifact.payload,
    }))
}

fn encode_response_blocks(
    blocks: &[ContentBlock],
    context: &AdapterContext,
    model: &str,
) -> Result<Vec<Value>, AdapterError> {
    let mut output = Vec::new();
    for block in blocks {
        match block {
            ContentBlock::Text(text) => output.push(json!({
                "type":"text","text":text.text,"citations":text.metadata.annotations,
            })),
            ContentBlock::Refusal(refusal) => {
                output.push(json!({"type":"text","text":refusal.text}))
            }
            ContentBlock::ToolCall(call) => {
                if let Some(carrier) = encode_carrier(tool_call_entries(&call.id, &call.artifacts))
                {
                    output.push(json!({
                        "type":"thinking",
                        "thinking":"",
                        "signature":carrier,
                    }));
                }
                output.push(json!({
                    "type":"tool_use","id":call.id,"name":call.name,
                    "input":call.arguments.clone().unwrap_or_else(||json!({})),
                }));
            }
            ContentBlock::Reasoning(reasoning) => {
                let kind = if reasoning.encrypted {
                    ArtifactKind::AnthropicRedactedThinking
                } else {
                    ArtifactKind::AnthropicThinkingSignature
                };
                let artifact = reasoning.artifacts.iter().find(|artifact| {
                    artifact.kind == kind
                        && affinity_matches_context(
                            &artifact.affinity,
                            context,
                            model,
                            ProtocolKind::AnthropicMessages,
                        )
                });
                if let Some(artifact) = artifact {
                    output.push(if reasoning.encrypted {
                        json!({"type":"redacted_thinking","data":artifact.payload})
                    } else {
                        json!({
                            "type":"thinking",
                            "thinking":reasoning.text.clone().unwrap_or_default(),
                            "signature":artifact.payload,
                        })
                    });
                } else if let Some(carrier) =
                    encode_carrier(reasoning_entries(&reasoning.artifacts))
                {
                    output.push(json!({
                        "type":"thinking",
                        "thinking":reasoning.text.clone().unwrap_or_default(),
                        "signature":carrier,
                    }));
                } else if let Some(text) = reasoning.text.as_deref().filter(|text| !text.is_empty())
                {
                    // A foreign response has no issuer-valid Anthropic signature. The planner
                    // reports this approximation; return the readable reasoning as ordinary text
                    // instead of failing the complete response.
                    output.push(json!({"type":"text","text":text}));
                }
            }
            ContentBlock::ProviderArtifact(artifact)
                if affinity_matches_context(
                    &artifact.affinity,
                    context,
                    model,
                    ProtocolKind::AnthropicMessages,
                ) =>
            {
                output.push(artifact.payload.clone());
            }
            ContentBlock::ProviderArtifact(artifact) if is_portable_artifact(&artifact.kind) => {
                if let Some(carrier) =
                    encode_carrier(previous_part_entries(std::slice::from_ref(artifact)))
                {
                    output.push(json!({
                        "type":"thinking",
                        "thinking":"",
                        "signature":carrier,
                    }));
                }
            }
            ContentBlock::ProviderArtifact(_) => {}
            // Unsupported foreign response blocks are reported by the response conversion plan.
            // Keep the remaining Anthropic response usable rather than failing it wholesale.
            _ => {}
        }
    }
    if output.is_empty() {
        output.push(json!({"type":"text","text":""}));
    }
    Ok(output)
}

fn artifact_is_preserved(plan: &ConversionPlan, path: &str) -> bool {
    plan.artifact_actions()
        .iter()
        .any(|action| action.path == path && action.action == ArtifactDisposition::Preserve)
}

fn encode_media(kind: &str, source: &MediaSource) -> Result<Value, AdapterError> {
    let source = match source {
        MediaSource::InlineBase64 { media_type, data } => json!({
            "type":"base64","media_type":media_type,"data":data,
        }),
        MediaSource::RemoteUrl { url } => json!({"type":"url","url":url}),
        MediaSource::ProviderFileId { id, .. } => json!({"type":"file","file_id":id}),
    };
    Ok(json!({"type":kind,"source":source}))
}

fn encode_tools(tools: &[ToolDefinition]) -> Result<Value, AdapterError> {
    tools
        .iter()
        .map(|tool| match tool {
            ToolDefinition::Function(tool) => {
                let mut value = json!({
                    "name":tool.name,"description":tool.description,"input_schema":tool.input_schema,
                });
                if let Some(cache) = encode_cache_policy(&tool.cache_policy) {
                    value["cache_control"] = cache;
                }
                if let Some(defer_loading) = tool.defer_loading {
                    value["defer_loading"] = json!(defer_loading);
                }
                if !tool.allowed_callers.is_empty() {
                    value["allowed_callers"] = json!(tool.allowed_callers);
                }
                if !tool.input_examples.is_empty() {
                    value["input_examples"] = json!(tool.input_examples);
                }
                if let Some(eager_input_streaming) = tool.eager_input_streaming {
                    value["eager_input_streaming"] = json!(eager_input_streaming);
                }
                Ok(value)
            }
            ToolDefinition::Hosted(tool) if tool.provider.as_deref() == Some("anthropic") => {
                Ok(tool.config.clone())
            }
            _ => Err(AdapterError::new(
                "encoder_plan_violation",
                "$.tools",
                "Anthropic cannot encode this foreign tool definition safely",
            )),
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Value::Array)
}

fn encode_tool_choice(
    choice: &ToolChoice,
    generation: &GenerationConfig,
) -> Result<Value, AdapterError> {
    let mut value = match choice {
        ToolChoice::Auto => json!({"type":"auto"}),
        ToolChoice::None => json!({"type":"none"}),
        ToolChoice::Required => json!({"type":"any"}),
        ToolChoice::Named { name } => json!({"type":"tool","name":name}),
        ToolChoice::Allowed { .. } => {
            return Err(AdapterError::new(
                "encoder_plan_violation",
                "$.tool_choice",
                "Anthropic has no allowed-tools subset",
            ));
        }
    };
    if let Some(parallel) = generation.parallel_tool_calls {
        value["disable_parallel_tool_use"] = json!(!parallel);
    }
    Ok(value)
}

fn encode_thinking(body: &mut Value, reasoning: &ReasoningConfig, model: &str) {
    let dialect = anthropic_model_dialect(model);
    if reasoning.is_explicitly_disabled() {
        // Fable 5 and Mythos 5 keep thinking enabled and reject an explicit
        // disabled command. Omitting the unsupported command keeps the model
        // usable; native Anthropic requests remain byte-for-byte untouched.
        if !dialect.thinking_always_on {
            body["thinking"] = json!({"type":"disabled"});
        }
        return;
    }
    match reasoning.mode {
        ReasoningMode::Adaptive => body["thinking"] = json!({"type":"adaptive"}),
        ReasoningMode::Enabled => match reasoning.token_budget {
            Some(_) if dialect.adaptive_thinking_only => {
                body["thinking"] = json!({"type":"adaptive"})
            }
            Some(budget_tokens) => {
                body["thinking"] = json!({
                    "type":"enabled","budget_tokens":budget_tokens,
                })
            }
            // Enabled thinking without a numeric budget is not valid on the
            // Anthropic wire. Preserve the caller's intent with Anthropic's
            // adaptive mode instead of emitting budget_tokens:null.
            None => body["thinking"] = json!({"type":"adaptive"}),
        },
        ReasoningMode::Disabled => {}
        ReasoningMode::Automatic => body["thinking"] = json!({"type":"adaptive"}),
    }
    if let Some(effort) = reasoning.effort {
        // Anthropic's documented effort alphabet is
        // low/medium/high/xhigh/max. Cross-protocol values outside that
        // alphabet use the nearest semantic endpoint; native Anthropic
        // requests bypass re-encoding and therefore remain untouched. Model
        // support is a narrower, metadata-driven concern: do not silently
        // demote xhigh/max merely because some Claude models reject them.
        if let Some(name) = effort.anthropic_name() {
            body["output_config"]["effort"] = json!(name);
        }
    }
}

fn encode_response_format(body: &mut Value, format: &ResponseFormat) -> Result<(), AdapterError> {
    match format {
        ResponseFormat::Text => {}
        ResponseFormat::JsonSchema {
            name,
            description,
            schema,
            ..
        } => {
            body["output_config"]["format"] = json!({
                "type":"json_schema","name":name,"description":description,"schema":schema,
            });
        }
        ResponseFormat::JsonObject => {
            return Err(AdapterError::new(
                "encoder_plan_violation",
                "$.output_config.format",
                "Anthropic requires a JSON schema",
            ));
        }
    }
    Ok(())
}

fn encode_generation(body: &mut Value, generation: &GenerationConfig, model: &str) {
    // Claude 4.7+ removed configurable sampling. Omit cross-protocol controls
    // that the target generation cannot honor instead of sending a stable 400.
    // Same-protocol Anthropic requests bypass this encoder and remain intact.
    if !anthropic_model_dialect(model).adaptive_thinking_only {
        if let Some(value) = generation.temperature {
            body["temperature"] = json!(value);
        }
        if let Some(value) = generation.top_p {
            body["top_p"] = json!(value);
        }
        if let Some(value) = generation.top_k {
            body["top_k"] = json!(value);
        }
    }
    if !generation.stop.is_empty() {
        body["stop_sequences"] = json!(generation.stop);
    }
    if let Some(value) = &generation.service_tier {
        body["service_tier"] = json!(value);
    }
}

fn encode_extensions(body: &mut Value, extensions: &[ProviderExtension], plan: &ConversionPlan) {
    for (index, extension) in extensions.iter().enumerate() {
        if extension.namespace == "anthropic_messages"
            && extension.nested_path().is_none()
            && plan.feature_actions().iter().any(|action| {
                action.path == format!("extensions[{index}]")
                    && matches!(
                        action.action,
                        crate::protocol::conversion::FeatureDisposition::Preserve
                            | crate::protocol::conversion::FeatureDisposition::Encode
                    )
            })
        {
            body[&extension.name] = extension.value.clone();
        }
    }
}

fn decode_stop_reason(reason: Option<&str>) -> FinishReason {
    match reason {
        Some("tool_use") => FinishReason::ToolCalls,
        Some("max_tokens") => FinishReason::Length,
        Some("refusal") => FinishReason::Refusal,
        Some("pause_turn") => FinishReason::PauseTurn,
        _ => FinishReason::Stop,
    }
}

fn encode_stop_reason(finish: &FinishDetail) -> &str {
    if let Some(reason) = finish.original_reason.as_deref().filter(|reason| {
        matches!(
            *reason,
            "end_turn"
                | "tool_use"
                | "max_tokens"
                | "stop_sequence"
                | "refusal"
                | "pause_turn"
                | "compaction"
        )
    }) {
        return reason;
    }
    match finish.reason {
        FinishReason::ToolCalls => "tool_use",
        FinishReason::PauseTurn => "pause_turn",
        FinishReason::Length => "max_tokens",
        FinishReason::Refusal | FinishReason::ContentFilter => "refusal",
        FinishReason::Error => "error",
        FinishReason::Cancelled => "stop_sequence",
        FinishReason::Stop | FinishReason::Unknown => "end_turn",
    }
}

fn decode_error_response(body: &Value, context: &AdapterContext) -> CanonicalResponseV2 {
    let error = body.get("error").unwrap_or(&Value::Null);
    CanonicalResponseV2 {
        id: body
            .get("request_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        model: None,
        blocks: Vec::new(),
        status: ResponseStatus::Failed,
        finish: FinishDetail {
            reason: FinishReason::Error,
            original_reason: error
                .get("type")
                .and_then(Value::as_str)
                .map(str::to_string),
            incomplete_details: None,
        },
        usage: Usage::default(),
        error: Some(CanonicalError {
            code: "provider_error".to_string(),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            retryable: error
                .get("retryable")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            provider_code: error
                .get("type")
                .and_then(Value::as_str)
                .map(str::to_string),
            details: error.get("details").cloned(),
        }),
        extensions: decode_response_extensions(body, context, ""),
    }
}

fn reported(value: Option<&Value>) -> UsageValue {
    value
        .and_then(Value::as_i64)
        .and_then(|value| UsageValue::reported(value).ok())
        .unwrap_or_default()
}

fn decode_usage(value: Option<&Value>) -> Usage {
    let Some(value) = value else {
        return Usage::default();
    };
    let input = reported(value.get("input_tokens"));
    let output = reported(value.get("output_tokens"));
    let total = match (input.value, output.value) {
        (Some(input), Some(output)) => {
            UsageValue::reported((input + output) as i64).unwrap_or_default()
        }
        _ => UsageValue::default(),
    };
    let server_tools = value
        .get("server_tool_use")
        .and_then(Value::as_object)
        .map(|object| object.values().filter_map(Value::as_i64).sum::<i64>());
    let cache_creation = reported(value.get("cache_creation_input_tokens"));
    Usage {
        input_tokens: input,
        output_tokens: output,
        total_tokens: total,
        cache_read_tokens: reported(value.get("cache_read_input_tokens")),
        cache_write_tokens: cache_creation.clone(),
        cache_creation_tokens: cache_creation,
        cache_expiry_5m_tokens: reported(
            value.pointer("/cache_creation/ephemeral_5m_input_tokens"),
        ),
        cache_expiry_1h_tokens: reported(
            value.pointer("/cache_creation/ephemeral_1h_input_tokens"),
        ),
        reasoning_tokens: reported(value.pointer("/output_tokens_details/thinking_tokens")),
        server_tool_calls: server_tools
            .and_then(|value| UsageValue::reported(value).ok())
            .unwrap_or_default(),
        ..Default::default()
    }
}

fn encode_usage(usage: &Usage) -> Value {
    let cache_creation = usage
        .cache_creation_tokens
        .value
        .or(usage.cache_write_tokens.value);
    json!({
        "input_tokens":usage.input_tokens.value,
        "output_tokens":usage.output_tokens.value,
        "output_tokens_details":{"thinking_tokens":usage.reasoning_tokens.value},
        "cache_read_input_tokens":usage.cache_read_tokens.value,
        "cache_creation_input_tokens":cache_creation,
        "cache_creation":{
            "ephemeral_5m_input_tokens":usage.cache_expiry_5m_tokens.value,
            "ephemeral_1h_input_tokens":usage.cache_expiry_1h_tokens.value,
        },
        "server_tool_use":{"requests":usage.server_tool_calls.value},
    })
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::AnthropicMessagesAdapter;
    use crate::protocol::adapters::{
        AdapterContext, OpenAiResponsesAdapter, ProtocolAdapter, ProviderDialect,
    };
    use crate::protocol::capability::CapabilityProfile;
    use crate::protocol::conversion::{
        ArtifactDisposition, ConversionPlan, ConversionPolicy, IssuerIdentity, plan_conversion,
    };
    use crate::protocol::ir::{
        ArtifactAffinity, ArtifactKind, CachePolicy, ContentBlock, FinishReason, InstructionRole,
        ReasoningConfig, ReasoningEffort, ReasoningMode, ReasoningSummaryMode, ResponseFormat,
        ToolChoice, ToolDefinition, TurnRole,
    };
    use crate::protocol::kind::ProtocolKind;

    fn fixture() -> Value {
        serde_json::from_str(include_str!(
            "../../../tests/fixtures/protocol-v2/anthropic-messages/advanced.json"
        ))
        .unwrap()
    }

    fn context(account: &str) -> AdapterContext {
        AdapterContext {
            dialect: ProviderDialect::Compatible {
                provider: "anthropic".to_string(),
            },
            issuer: IssuerIdentity {
                provider: "anthropic".to_string(),
                endpoint_fingerprint: "api.anthropic.com".to_string(),
                account_fingerprint: Some(account.to_string()),
                model_family: Some("claude-test".to_string()),
            },
        }
    }

    fn responses_context(account: &str) -> AdapterContext {
        AdapterContext {
            dialect: ProviderDialect::Compatible {
                provider: "openai".to_string(),
            },
            issuer: IssuerIdentity {
                provider: "openai".to_string(),
                endpoint_fingerprint: "api.openai.com".to_string(),
                account_fingerprint: Some(account.to_string()),
                model_family: Some("gpt-5.6-sol".to_string()),
            },
        }
    }

    fn encode_as_responses(body: &Value) -> Value {
        let source_context = context("account-1");
        let target_context = responses_context("account-2");
        let mut request = AnthropicMessagesAdapter
            .decode_request(body, &source_context)
            .unwrap();
        request.model = "gpt-5.6-sol".to_string();
        let plan = plan_conversion(
            &request,
            ProtocolKind::AnthropicMessages,
            ProtocolKind::OpenAiResponses,
            &source_context.issuer,
            &target_context.issuer,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap();
        OpenAiResponsesAdapter
            .encode_request(&request, &plan, &target_context)
            .unwrap()
    }

    #[test]
    fn enabled_thinking_without_budget_falls_back_to_adaptive_wire_mode() {
        let mut body = serde_json::json!({});
        super::encode_thinking(
            &mut body,
            &ReasoningConfig {
                mode: ReasoningMode::Enabled,
                effort: None,
                token_budget: None,
                summary: ReasoningSummaryMode::None,
                provider_extensions: Vec::new(),
            },
            "claude-opus-5",
        );

        assert_eq!(body["thinking"]["type"], "adaptive");
        assert!(body["thinking"].get("budget_tokens").is_none());
    }

    #[test]
    fn adaptive_only_models_do_not_receive_fixed_thinking_budget() {
        let mut body = serde_json::json!({});
        super::encode_thinking(
            &mut body,
            &ReasoningConfig {
                mode: ReasoningMode::Enabled,
                effort: Some(ReasoningEffort::High),
                token_budget: Some(4096),
                summary: ReasoningSummaryMode::None,
                provider_extensions: Vec::new(),
            },
            "claude-opus-5",
        );
        assert_eq!(body["thinking"], serde_json::json!({"type":"adaptive"}));
        assert_eq!(body["output_config"]["effort"], "high");
        assert!(body.pointer("/thinking/budget_tokens").is_none());
    }

    #[test]
    fn fable_omits_unsupported_disabled_thinking_command() {
        let mut body = serde_json::json!({});
        super::encode_thinking(
            &mut body,
            &ReasoningConfig {
                mode: ReasoningMode::Disabled,
                effort: Some(ReasoningEffort::None),
                token_budget: None,
                summary: ReasoningSummaryMode::None,
                provider_extensions: Vec::new(),
            },
            "claude-fable-5",
        );
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn legacy_models_keep_fixed_budget_wire_shape() {
        let mut body = serde_json::json!({});
        super::encode_thinking(
            &mut body,
            &ReasoningConfig {
                mode: ReasoningMode::Enabled,
                effort: None,
                token_budget: Some(4096),
                summary: ReasoningSummaryMode::None,
                provider_extensions: Vec::new(),
            },
            "claude-opus-4-5",
        );
        assert_eq!(
            body["thinking"],
            serde_json::json!({"type":"enabled","budget_tokens":4096})
        );
    }

    #[test]
    fn adaptive_only_models_omit_removed_sampling_controls() {
        let mut body = serde_json::json!({});
        super::encode_generation(
            &mut body,
            &crate::protocol::ir::GenerationConfig {
                temperature: Some(0.2),
                top_p: Some(0.8),
                top_k: Some(20),
                ..Default::default()
            },
            "claude-sonnet-5",
        );
        assert!(body.get("temperature").is_none());
        assert!(body.get("top_p").is_none());
        assert!(body.get("top_k").is_none());
    }

    #[test]
    fn legacy_models_keep_sampling_controls() {
        let mut body = serde_json::json!({});
        super::encode_generation(
            &mut body,
            &crate::protocol::ir::GenerationConfig {
                temperature: Some(0.2),
                top_p: Some(0.8),
                top_k: Some(20),
                ..Default::default()
            },
            "claude-sonnet-4-5",
        );
        assert_eq!(body["temperature"], 0.2);
        assert_eq!(body["top_p"], 0.8);
        assert_eq!(body["top_k"], 20);
    }

    #[test]
    fn decodes_signed_thinking_cache_media_tools_and_usage_in_order() {
        let adapter = AnthropicMessagesAdapter;
        let fixture = fixture();
        let request = adapter
            .decode_request(&fixture["request"], &context("account-1"))
            .unwrap();

        assert_eq!(request.instructions.len(), 2);
        let ContentBlock::Text(system) = &request.instructions[1].blocks[0] else {
            panic!("system text");
        };
        assert_eq!(system.metadata.cache_policy, Some(CachePolicy::Ephemeral1h));
        assert_eq!(request.turns.len(), 3);
        assert!(matches!(request.turns[0].blocks[1], ContentBlock::Image(_)));
        assert!(matches!(request.turns[0].blocks[2], ContentBlock::File(_)));
        let ContentBlock::Reasoning(thinking) = &request.turns[1].blocks[0] else {
            panic!("thinking block");
        };
        assert_eq!(thinking.text.as_deref(), Some("reasoning"));
        assert_eq!(
            thinking.artifacts[0].kind,
            ArtifactKind::AnthropicThinkingSignature
        );
        assert_eq!(thinking.artifacts[0].payload, "sig-exact");
        assert!(matches!(
            thinking.artifacts[0].affinity,
            ArtifactAffinity::ExactIssuer { .. }
        ));
        let ContentBlock::Reasoning(redacted) = &request.turns[1].blocks[1] else {
            panic!("redacted thinking block");
        };
        assert_eq!(
            redacted.artifacts[0].kind,
            ArtifactKind::AnthropicRedactedThinking
        );
        assert!(matches!(
            request.turns[2].blocks[0],
            ContentBlock::ToolResult(_)
        ));
        assert!(matches!(request.tool_choice, ToolChoice::Named { .. }));
        assert!(matches!(request.reasoning.mode, ReasoningMode::Adaptive));
        assert!(matches!(
            request.response_format,
            ResponseFormat::JsonSchema { .. }
        ));

        let response = adapter
            .decode_response(&fixture["response"], &context("account-1"))
            .unwrap();
        assert_eq!(response.blocks.len(), 4);
        assert_eq!(response.usage.cache_read_tokens.value, Some(8));
        assert_eq!(response.usage.cache_creation_tokens.value, Some(4));
        assert_eq!(response.usage.cache_write_tokens.value, Some(4));
        assert_eq!(response.usage.cache_expiry_5m_tokens.value, Some(1));
        assert_eq!(response.usage.cache_expiry_1h_tokens.value, Some(3));
        assert_eq!(response.usage.server_tool_calls.value, Some(2));
    }

    #[test]
    fn approximates_compatible_message_roles_instead_of_rejecting_the_request() {
        let adapter = AnthropicMessagesAdapter;
        let request = adapter
            .decode_request(
                &serde_json::json!({
                    "model": "claude-public",
                    "max_tokens": 256,
                    "messages": [
                        {"role": "system", "content": "system from message history"},
                        {"role": "developer", "content": "developer instruction"},
                        {"role": "assistant", "content": [{
                            "type": "tool_use",
                            "id": "call-1",
                            "name": "read_file",
                            "input": {"path": "README.md"}
                        }]},
                        {
                            "role": "tool",
                            "tool_call_id": "call-1",
                            "content": "file contents"
                        },
                        {"role": "future_assistant", "content": [{
                            "type": "tool_use",
                            "id": "call-2",
                            "name": "search",
                            "input": {"query": "const api"}
                        }]},
                        {"content": "role omitted by a compatible client"}
                    ]
                }),
                &context("account-1"),
            )
            .expect("compatible roles should remain executable");

        assert_eq!(request.instructions.len(), 1);
        assert_eq!(request.instructions[0].role, InstructionRole::Developer);
        assert_eq!(request.turns.len(), 5);
        assert_eq!(request.turns[0].role, TurnRole::User);
        let ContentBlock::Text(reminder) = &request.turns[0].blocks[0] else {
            panic!("message-level system should become an in-band reminder");
        };
        assert_eq!(
            reminder.text,
            "<system-reminder>\nsystem from message history\n</system-reminder>"
        );
        assert_eq!(request.turns[1].role, TurnRole::Assistant);
        assert_eq!(request.turns[2].role, TurnRole::User);
        assert_eq!(request.turns[3].role, TurnRole::Assistant);
        assert_eq!(request.turns[4].role, TurnRole::User);
        let ContentBlock::ToolResult(result) = &request.turns[2].blocks[0] else {
            panic!("compatible tool role should become a tool result");
        };
        assert_eq!(result.call_id, "call-1");
        assert_eq!(result.name.as_deref(), Some("read_file"));
        assert_eq!(
            request
                .extensions
                .iter()
                .filter(|extension| extension
                    .nested_path()
                    .is_some_and(|path| path.ends_with(".role")))
                .count(),
            4
        );
        assert!(request.extensions.iter().any(|extension| {
            extension.nested_path() == Some("$.messages[4].role")
                && extension.value == "future_assistant"
        }));
        assert!(request.extensions.iter().any(|extension| {
            extension.nested_path() == Some("$.messages[5].role") && extension.value.is_null()
        }));
    }

    #[test]
    fn message_level_system_keeps_a_growing_responses_prefix_stable() {
        let first = serde_json::json!({
            "model": "claude-public",
            "max_tokens": 256,
            "system": "stable top-level instruction",
            "messages": [
                {"role": "user", "content": "review the repository"},
                {"role": "assistant", "content": "first answer"},
                {"role": "system", "content": "token count: 100"},
                {"role": "user", "content": "continue"}
            ]
        });
        let mut second = first.clone();
        second["messages"].as_array_mut().unwrap().extend([
            serde_json::json!({"role": "assistant", "content": "second answer"}),
            serde_json::json!({"role": "system", "content": "token count: 200"}),
            serde_json::json!({"role": "user", "content": "continue again"}),
        ]);

        let first = encode_as_responses(&first);
        let second = encode_as_responses(&second);
        let first_input = first["input"].as_array().unwrap();
        let second_input = second["input"].as_array().unwrap();

        assert!(second_input.len() > first_input.len());
        assert_eq!(first_input, &second_input[..first_input.len()]);
        assert_eq!(first_input[0]["role"], "system");
        assert_eq!(first_input[3]["role"], "user");
        assert_eq!(
            first_input[3]["content"][0]["text"],
            "<system-reminder>\ntoken count: 100\n</system-reminder>"
        );
    }

    #[test]
    fn message_level_system_does_not_break_parallel_tool_result_adjacency() {
        let body = serde_json::json!({
            "model": "claude-public",
            "max_tokens": 256,
            "messages": [
                {"role": "user", "content": "inspect both files"},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "call-1", "name": "read_file", "input": {"path": "a.rs"}},
                    {"type": "tool_use", "id": "call-2", "name": "read_file", "input": {"path": "b.rs"}}
                ]},
                {"role": "system", "content": "tool execution completed"},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "call-2", "content": "b"},
                    {"type": "text", "text": "summarize the result"},
                    {"type": "tool_result", "tool_use_id": "call-1", "content": "a"}
                ]}
            ]
        });

        let encoded = encode_as_responses(&body);
        let input = encoded["input"].as_array().unwrap();
        let call_1 = input
            .iter()
            .position(|item| item["type"] == "function_call" && item["call_id"] == "call-1")
            .unwrap();
        let call_2 = input
            .iter()
            .position(|item| item["type"] == "function_call" && item["call_id"] == "call-2")
            .unwrap();
        let result_1 = input
            .iter()
            .position(|item| item["type"] == "function_call_output" && item["call_id"] == "call-1")
            .unwrap();
        let result_2 = input
            .iter()
            .position(|item| item["type"] == "function_call_output" && item["call_id"] == "call-2")
            .unwrap();
        let reminder = input
            .iter()
            .position(|item| {
                item["type"] == "message"
                    && item["content"][0]["text"]
                        == "<system-reminder>\ntool execution completed\n</system-reminder>"
            })
            .unwrap();

        assert!(call_1 < call_2);
        assert!(call_2 < result_1);
        assert!(result_1 < result_2);
        assert!(result_2 < reminder);
    }

    #[test]
    fn incomplete_tool_results_are_not_guessed_during_system_reordering() {
        let source_context = context("account-1");
        let target_context = responses_context("account-2");
        let body = serde_json::json!({
            "model": "claude-public",
            "max_tokens": 256,
            "messages": [
                {"role": "user", "content": "inspect both files"},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "call-1", "name": "read_file", "input": {"path": "a.rs"}},
                    {"type": "tool_use", "id": "call-2", "name": "read_file", "input": {"path": "b.rs"}}
                ]},
                {"role": "system", "content": "one result is still pending"},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "call-1", "content": "a"}
                ]}
            ]
        });
        let mut request = AnthropicMessagesAdapter
            .decode_request(&body, &source_context)
            .unwrap();
        request.model = "gpt-5.6-sol".to_string();

        assert_eq!(request.turns[2].role, TurnRole::User);
        let ContentBlock::Text(reminder) = &request.turns[2].blocks[0] else {
            panic!("ambiguous input must preserve chronological reminder order");
        };
        assert!(reminder.text.contains("one result is still pending"));

        let error = crate::protocol::convert_non_stream(
            ProtocolKind::AnthropicMessages,
            ProtocolKind::OpenAiResponses,
            &body,
            &source_context,
            &target_context,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .expect_err("an incomplete parallel result set must remain blocked");
        assert_eq!(error.code(), "tool_call_unanswered");
        assert!(error.report().unwrap().issues.iter().any(|issue| {
            matches!(
                issue.code.as_str(),
                "tool_call_unanswered" | "tool_result_orphaned"
            )
        }));
    }

    #[test]
    fn reencodes_exact_signed_blocks_and_rejects_missing_signature() {
        let adapter = AnthropicMessagesAdapter;
        let fixture = fixture();
        let request = adapter
            .decode_request(&fixture["request"], &context("account-1"))
            .unwrap();
        let plan = plan_conversion(
            &request,
            ProtocolKind::AnthropicMessages,
            ProtocolKind::AnthropicMessages,
            &context("account-1").issuer,
            &context("account-1").issuer,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap();
        let encoded = adapter
            .encode_request(&request, &plan, &context("account-1"))
            .unwrap();
        assert_eq!(
            encoded["messages"][1]["content"][0]["signature"],
            "sig-exact"
        );
        assert_eq!(
            encoded["messages"][1]["content"][1]["data"],
            "redacted-exact"
        );
        assert_eq!(encoded["system"][1]["cache_control"]["ttl"], "1h");

        let mut missing = request.clone();
        let ContentBlock::Reasoning(thinking) = &mut missing.turns[1].blocks[0] else {
            panic!("thinking block");
        };
        thinking.artifacts.clear();
        let error = adapter
            .encode_request(&missing, &plan, &context("account-1"))
            .unwrap_err();
        assert_eq!(error.code(), "anthropic_thinking_signature_missing");
    }

    #[test]
    fn filters_signed_thinking_for_another_account() {
        let adapter = AnthropicMessagesAdapter;
        let request = adapter
            .decode_request(&fixture()["request"], &context("account-1"))
            .unwrap();
        let plan = plan_conversion(
            &request,
            ProtocolKind::AnthropicMessages,
            ProtocolKind::AnthropicMessages,
            &context("account-1").issuer,
            &context("account-2").issuer,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap();

        assert!(plan.is_executable());
        assert!(
            plan.report()
                .issues
                .iter()
                .any(|issue| issue.code == "reasoning_history_omitted")
        );
        let filtered_reasoning_artifacts = plan
            .artifact_actions()
            .iter()
            .filter(|action| {
                matches!(
                    action.kind,
                    ArtifactKind::AnthropicThinkingSignature
                        | ArtifactKind::AnthropicRedactedThinking
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(filtered_reasoning_artifacts.len(), 2);
        assert!(
            filtered_reasoning_artifacts
                .iter()
                .all(|action| action.action == ArtifactDisposition::Filter)
        );

        let encoded = adapter
            .encode_request(&request, &plan, &context("account-2"))
            .unwrap();
        let assistant_content = encoded["messages"][1]["content"].as_array().unwrap();
        assert!(
            assistant_content
                .iter()
                .any(|block| { block.get("type").and_then(Value::as_str) == Some("tool_use") })
        );
        assert!(!assistant_content.iter().any(|block| {
            matches!(
                block.get("type").and_then(Value::as_str),
                Some("thinking" | "redacted_thinking")
            )
        }));
    }

    #[test]
    fn reencodes_response_signatures_citations_usage_and_provider_errors() {
        let adapter = AnthropicMessagesAdapter;
        let fixture = fixture();
        let request = adapter
            .decode_request(&fixture["request"], &context("account-1"))
            .unwrap();
        let plan = plan_conversion(
            &request,
            ProtocolKind::AnthropicMessages,
            ProtocolKind::AnthropicMessages,
            &context("account-1").issuer,
            &context("account-1").issuer,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap();
        let response = adapter
            .decode_response(&fixture["response"], &context("account-1"))
            .unwrap();
        let encoded = adapter
            .encode_response(&response, &plan, &context("account-1"))
            .unwrap();
        assert_eq!(encoded["content"][0]["signature"], "response-sig");
        assert_eq!(encoded["content"][1]["data"], "response-redacted");
        assert_eq!(
            encoded["content"][2]["citations"][0]["type"],
            "web_search_result_location"
        );
        assert_eq!(
            encoded["usage"]["cache_creation"]["ephemeral_1h_input_tokens"],
            3
        );

        let error = adapter
            .decode_response(
                &serde_json::json!({
                    "type":"error",
                    "request_id":"req-1",
                    "error":{"type":"rate_limit_error","message":"slow down"}
                }),
                &context("account-1"),
            )
            .unwrap();
        assert_eq!(error.status, crate::protocol::ir::ResponseStatus::Failed);
        assert_eq!(
            error.error.unwrap().provider_code.as_deref(),
            Some("rate_limit_error")
        );
    }

    #[test]
    fn roundtrips_function_tool_execution_hints_and_versioned_hosted_tools() {
        let adapter = AnthropicMessagesAdapter;
        let request = adapter
            .decode_request(
                &serde_json::json!({
                    "model":"claude-test",
                    "max_tokens":64,
                    "messages":[{"role":"user","content":"search"}],
                    "tools":[
                        {
                            "name":"lookup",
                            "description":"lookup a record",
                            "input_schema":{"type":"object","properties":{"id":{"type":"string"}}},
                            "defer_loading":true,
                            "allowed_callers":["direct","code_execution_20250825"],
                            "input_examples":[{"id":"example"}],
                            "eager_input_streaming":true,
                            "cache_control":{"type":"ephemeral","ttl":"1h"}
                        },
                        {
                            "type":"web_search_20250305",
                            "name":"web_search",
                            "max_uses":3,
                            "allowed_domains":["example.com"]
                        }
                    ]
                }),
                &context("account-1"),
            )
            .unwrap();

        let ToolDefinition::Function(function) = &request.tools[0] else {
            panic!("function tool");
        };
        assert_eq!(function.defer_loading, Some(true));
        assert_eq!(
            function.allowed_callers,
            ["direct", "code_execution_20250825"]
        );
        assert_eq!(
            function.input_examples,
            [serde_json::json!({"id":"example"})]
        );
        assert_eq!(function.eager_input_streaming, Some(true));

        let ToolDefinition::Hosted(hosted) = &request.tools[1] else {
            panic!("hosted tool");
        };
        assert_eq!(hosted.provider.as_deref(), Some("anthropic"));
        assert_eq!(hosted.config["type"], "web_search_20250305");
        assert_eq!(hosted.config["allowed_domains"][0], "example.com");

        let encoded = adapter
            .encode_request(
                &request,
                &ConversionPlan::native_passthrough(ProtocolKind::AnthropicMessages),
                &context("account-1"),
            )
            .unwrap();
        assert_eq!(encoded["tools"][0]["defer_loading"], true);
        assert_eq!(
            encoded["tools"][0]["allowed_callers"][1],
            "code_execution_20250825"
        );
        assert_eq!(encoded["tools"][0]["input_examples"][0]["id"], "example");
        assert_eq!(encoded["tools"][0]["eager_input_streaming"], true);
        assert_eq!(encoded["tools"][1], hosted.config);
    }

    #[test]
    fn preserves_pause_turn_as_in_progress_in_buffered_roundtrip() {
        let adapter = AnthropicMessagesAdapter;
        let response = adapter
            .decode_response(
                &serde_json::json!({
                    "id":"msg_pause",
                    "type":"message",
                    "role":"assistant",
                    "model":"claude-test",
                    "content":[{"type":"text","text":"working"}],
                    "stop_reason":"pause_turn",
                    "usage":{"input_tokens":8,"output_tokens":2}
                }),
                &context("account-1"),
            )
            .unwrap();

        assert_eq!(response.finish.reason, FinishReason::PauseTurn);
        assert_eq!(
            response.status,
            crate::protocol::ir::ResponseStatus::InProgress
        );
        let encoded = adapter
            .encode_response(
                &response,
                &ConversionPlan::native_passthrough(ProtocolKind::AnthropicMessages),
                &context("account-1"),
            )
            .unwrap();
        assert_eq!(encoded["stop_reason"], "pause_turn");
    }

    #[test]
    fn preserves_compaction_block_and_encrypted_state_in_native_roundtrip() {
        let adapter = AnthropicMessagesAdapter;
        let block = serde_json::json!({
            "type": "compaction",
            "content": "summary of the previous conversation",
            "encrypted_content": "opaque-provider-state",
            "cache_control": {"type": "ephemeral"}
        });
        let response = adapter
            .decode_response(
                &serde_json::json!({
                    "id":"msg_compaction",
                    "type":"message",
                    "role":"assistant",
                    "model":"claude-test",
                    "content":[block.clone()],
                    "stop_reason":"compaction",
                    "usage":{"input_tokens":50001,"output_tokens":120}
                }),
                &context("account-1"),
            )
            .unwrap();
        let ContentBlock::ProviderArtifact(artifact) = &response.blocks[0] else {
            panic!("compaction must remain an opaque provider artifact");
        };
        assert_eq!(artifact.payload, block);

        let encoded = adapter
            .encode_response(
                &response,
                &ConversionPlan::native_passthrough(ProtocolKind::AnthropicMessages),
                &context("account-1"),
            )
            .unwrap();
        assert_eq!(encoded["content"][0], block);
        assert_eq!(encoded["stop_reason"], "compaction");
    }
}
