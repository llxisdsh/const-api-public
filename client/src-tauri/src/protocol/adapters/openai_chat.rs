use std::collections::HashMap;

use serde_json::{Map, Value, json};

use crate::protocol::adapters::{
    AdapterContext, AdapterError, ProtocolAdapter, ProviderDialect, collect_nested_unknown_fields,
    decode_openai_prompt_cache, decode_prompt_cache_breakpoint, encode_openai_prompt_cache,
    encode_prompt_cache_breakpoint, ensure_encoding_plan, exact_issuer_affinity,
    link_tool_result_names, protocol_affinity, response_status_from_finish,
};
use crate::protocol::capability::Feature;
use crate::protocol::continuation::{
    ContinuationCarrier, ContinuationEntry, ContinuationOwner, decode_carrier, encode_carrier,
    is_portable_artifact, previous_part_entries, reasoning_artifacts_are_encrypted,
    reasoning_entries, tool_call_entries,
};
use crate::protocol::conversion::{
    ConversionLevel, ConversionPlan, FeatureDisposition, canonicalize_schema,
    canonicalize_tool_schema, flatten_namespace_tools,
};
use crate::protocol::ir::*;
use crate::protocol::kind::ProtocolKind;

pub(crate) struct OpenAiChatAdapter;

impl ProtocolAdapter for OpenAiChatAdapter {
    fn decode_request(
        &self,
        body: &Value,
        context: &AdapterContext,
    ) -> Result<CanonicalRequestV2, AdapterError> {
        let model = body
            .get("model")
            .and_then(Value::as_str)
            .filter(|model| !model.trim().is_empty())
            .ok_or_else(|| AdapterError::new("model_missing", "$.model", "model is required"))?;
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
        request.stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
        request.metadata.source_protocol = Some(ProtocolKind::OpenAiChat);
        let mut call_names = std::collections::HashMap::new();
        for (index, message) in messages.iter().enumerate() {
            let role = message.get("role").and_then(Value::as_str).ok_or_else(|| {
                AdapterError::new(
                    "message_role_missing",
                    format!("$.messages[{index}].role"),
                    "message role is required",
                )
            })?;
            if matches!(role, "system" | "developer") {
                request.instructions.push(Instruction {
                    role: if role == "developer" {
                        InstructionRole::Developer
                    } else {
                        InstructionRole::System
                    },
                    blocks: decode_content(message.get("content"), index)?,
                });
                continue;
            }
            if role == "tool" {
                let call_id = message
                    .get("tool_call_id")
                    .or_else(|| message.get("call_id"))
                    .and_then(Value::as_str)
                    .filter(|id| !id.trim().is_empty())
                    .ok_or_else(|| {
                        AdapterError::new(
                            "tool_call_id_missing",
                            format!("$.messages[{index}].tool_call_id"),
                            "tool result requires a call ID",
                        )
                    })?;
                request.turns.push(Turn {
                    id: None,
                    role: TurnRole::User,
                    blocks: vec![ContentBlock::ToolResult(ToolResult {
                        call_id: call_id.to_string(),
                        name: message
                            .get("name")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .or_else(|| call_names.get(call_id).cloned()),
                        content: decode_content(message.get("content"), index)?,
                        status: ItemStatus::Completed,
                        is_error: false,
                        metadata: BlockMetadata::default(),
                    })],
                    status: Some(ItemStatus::Completed),
                });
                continue;
            }
            let turn_role = match role {
                "assistant" => TurnRole::Assistant,
                "user" => TurnRole::User,
                _ => {
                    return Err(AdapterError::new(
                        "unsupported_message_role",
                        format!("$.messages[{index}].role"),
                        format!("unsupported Chat role {role}"),
                    ));
                }
            };
            let mut blocks = decode_content(message.get("content"), index)?;
            if let Some(reasoning) = message
                .get("reasoning_content")
                .or_else(|| message.get("reasoning"))
                .and_then(Value::as_str)
            {
                blocks.insert(
                    0,
                    ContentBlock::Reasoning(ReasoningBlock {
                        text: Some(reasoning.to_string()),
                        summary: Vec::new(),
                        artifacts: Vec::new(),
                        encrypted: false,
                        metadata: BlockMetadata::default(),
                    }),
                );
            }
            if let Some(refusal) = message.get("refusal").and_then(Value::as_str) {
                blocks.push(ContentBlock::Refusal(RefusalBlock::new(refusal)));
            }
            let message_signature = message
                .pointer("/extra_fields/google/thought_signature")
                .or_else(|| message.pointer("/extra_fields/google/thoughtSignature"));
            let carrier = decode_chat_message_carrier(message)?;
            let (reasoning_artifacts, mut tool_artifacts, previous_part_artifacts) =
                carrier.map(distribute_chat_carrier).unwrap_or_default();
            if !reasoning_artifacts.is_empty() {
                attach_chat_reasoning_artifacts(&mut blocks, reasoning_artifacts);
            }
            attach_chat_previous_part_artifacts(&mut blocks, previous_part_artifacts);
            for (call_index, call) in message
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
            {
                let parsed = decode_tool_call(
                    call,
                    index,
                    call_index,
                    (call_index == 0).then_some(message_signature).flatten(),
                    call.get("id")
                        .or_else(|| call.get("call_id"))
                        .and_then(Value::as_str)
                        .and_then(|call_id| tool_artifacts.remove(call_id))
                        .unwrap_or_default(),
                )?;
                call_names.insert(parsed.id.clone(), parsed.name.clone());
                blocks.push(ContentBlock::ToolCall(parsed));
            }
            if let Some(call_id) = tool_artifacts.keys().next() {
                return Err(AdapterError::new(
                    "continuation_carrier_binding_invalid",
                    format!("$.messages[{index}]"),
                    format!("continuation carrier references missing tool call {call_id}"),
                ));
            }
            if let Some(annotations) = message.get("annotations").and_then(Value::as_array) {
                if let Some(metadata) = first_block_metadata_mut(&mut blocks) {
                    metadata.annotations = annotations.clone();
                }
            }
            request.turns.push(Turn {
                id: message
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                role: turn_role,
                blocks,
                status: None,
            });
        }
        request.tools = decode_tools(body.get("tools"))?;
        request.tool_choice = decode_tool_choice(body.get("tool_choice"))?;
        request.response_format = decode_response_format(body.get("response_format"))?;
        request.reasoning = decode_reasoning(body, context, model);
        request.generation = decode_generation(body);
        request.metadata.user_id = body.get("user").and_then(Value::as_str).map(str::to_string);
        request.metadata.store = body.get("store").and_then(Value::as_bool);
        request.metadata.prompt_cache = decode_openai_prompt_cache(body);
        request.extensions = decode_extensions(body, context, model);
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
        context: &AdapterContext,
    ) -> Result<Value, AdapterError> {
        ensure_encoding_plan(plan, ProtocolKind::OpenAiChat)?;
        let mut messages = Vec::new();
        for (instruction_index, instruction) in request.instructions.iter().enumerate() {
            messages.push(json!({
                "role": if instruction.role == InstructionRole::Developer { "developer" } else { "system" },
                "content": encode_content(
                    &instruction.blocks,
                    plan,
                    &format!("instructions[{instruction_index}].blocks"),
                    0,
                )?,
            }));
        }
        for (turn_index, turn) in request.turns.iter().enumerate() {
            encode_turn(turn, turn_index, plan, &mut messages)?;
        }
        let mut body = json!({
            "model": request.model,
            "messages": messages,
            "stream": request.stream,
        });
        if !request.tools.is_empty() {
            body["tools"] = encode_tools(&request.tools, plan)?;
        }
        if !matches!(request.tool_choice, ToolChoice::Auto) {
            body["tool_choice"] = encode_tool_choice(&request.tool_choice)?;
        }
        if !matches!(request.response_format, ResponseFormat::Text) {
            body["response_format"] = encode_response_format(&request.response_format)?;
        }
        encode_reasoning(&mut body, &request.reasoning, plan);
        encode_generation(&mut body, &request.generation, context);
        if let Some(user_id) = &request.metadata.user_id {
            body["user"] = json!(user_id);
        }
        if let Some(store) = request.metadata.store {
            body["store"] = json!(store);
        }
        encode_openai_prompt_cache(&mut body, &request.metadata.prompt_cache);
        for (index, extension) in request.extensions.iter().enumerate() {
            if extension.namespace == "openai_chat"
                && extension.nested_path().is_none()
                && feature_is_preserved(plan, &format!("extensions[{index}]"))
            {
                body[&extension.name] = extension.value.clone();
            }
        }
        Ok(body)
    }

    fn decode_response(
        &self,
        body: &Value,
        context: &AdapterContext,
    ) -> Result<CanonicalResponseV2, AdapterError> {
        let choice = body
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first());
        if choice.is_none() && body.get("error").is_some_and(Value::is_object) {
            return Ok(decode_chat_error_response(body, context));
        }
        let choice = choice.ok_or_else(|| {
            AdapterError::new("response_choice_missing", "$.choices", "missing choice")
        })?;
        let message = choice.get("message").ok_or_else(|| {
            AdapterError::new(
                "response_message_missing",
                "$.choices[0].message",
                "missing message",
            )
        })?;
        let model = body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mut blocks = decode_content(message.get("content"), 0)?;
        if let Some(reasoning) = message.get("reasoning_content").and_then(Value::as_str) {
            blocks.insert(
                0,
                ContentBlock::Reasoning(ReasoningBlock {
                    text: Some(reasoning.to_string()),
                    summary: Vec::new(),
                    artifacts: Vec::new(),
                    encrypted: false,
                    metadata: BlockMetadata::default(),
                }),
            );
        }
        if let Some(refusal) = message.get("refusal").and_then(Value::as_str) {
            blocks.push(ContentBlock::Refusal(RefusalBlock::new(refusal)));
        }
        let message_signature = message
            .pointer("/extra_fields/google/thought_signature")
            .or_else(|| message.pointer("/extra_fields/google/thoughtSignature"));
        let carrier = decode_chat_message_carrier(message)?;
        let (reasoning_artifacts, mut tool_artifacts, previous_part_artifacts) =
            carrier.map(distribute_chat_carrier).unwrap_or_default();
        if !reasoning_artifacts.is_empty() {
            attach_chat_reasoning_artifacts(&mut blocks, reasoning_artifacts);
        }
        attach_chat_previous_part_artifacts(&mut blocks, previous_part_artifacts);
        for (index, call) in message
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            blocks.push(ContentBlock::ToolCall(decode_tool_call(
                call,
                0,
                index,
                (index == 0).then_some(message_signature).flatten(),
                call.get("id")
                    .or_else(|| call.get("call_id"))
                    .and_then(Value::as_str)
                    .and_then(|call_id| tool_artifacts.remove(call_id))
                    .unwrap_or_default(),
            )?));
        }
        if let Some(call_id) = tool_artifacts.keys().next() {
            return Err(AdapterError::new(
                "continuation_carrier_binding_invalid",
                "$.choices[0].message",
                format!("continuation carrier references missing tool call {call_id}"),
            ));
        }
        if let Some(annotations) = message.get("annotations").and_then(Value::as_array) {
            if let Some(metadata) = first_block_metadata_mut(&mut blocks) {
                metadata.annotations = annotations.clone();
            }
        }
        let finish_reason = finish_reason(choice.get("finish_reason").and_then(Value::as_str));
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
                original_reason: choice
                    .get("finish_reason")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                incomplete_details: None,
            },
            usage: decode_usage(body.get("usage")),
            error: None,
            extensions: decode_chat_response_extensions(body, context, model),
        })
    }

    fn encode_response(
        &self,
        response: &CanonicalResponseV2,
        plan: &ConversionPlan,
        _context: &AdapterContext,
    ) -> Result<Value, AdapterError> {
        ensure_encoding_plan(plan, ProtocolKind::OpenAiChat)?;
        if let Some(error) = &response.error {
            let mut value = json!({
                "error": {
                    "message": error.message,
                    "type": error.code,
                    "code": error.provider_code.as_ref().unwrap_or(&error.code),
                    "retryable": error.retryable,
                }
            });
            if let Some(details) = &error.details {
                value["error"]["details"] = details.clone();
            }
            return Ok(value);
        }
        let mut message =
            json!({"role":"assistant","content": encode_assistant_text(&response.blocks)});
        let calls = response
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(block_index, block)| match block {
                ContentBlock::ToolCall(call) => Some(encode_tool_call(
                    call,
                    plan,
                    &format!("$.blocks[{block_index}]"),
                )),
                _ => None,
            })
            .collect::<Vec<_>>();
        if !calls.is_empty() {
            if let Some(signature) = gemini_signature_from_encoded_calls(&calls) {
                message["extra_fields"] = json!({"google":{"thought_signature":signature}});
            }
            message["tool_calls"] = Value::Array(calls);
        }
        let carrier_entries = preserved_chat_entries(&response.blocks, "$.blocks", plan);
        if chat_entries_need_carrier(&carrier_entries) {
            if let Some(carrier) = encode_carrier(carrier_entries) {
                set_chat_continuation_carrier(&mut message, carrier);
            }
        }
        if let Some(reasoning) = response.blocks.iter().find_map(|block| match block {
            ContentBlock::Reasoning(value) => value.text.clone(),
            _ => None,
        }) {
            message["reasoning_content"] = json!(reasoning);
        }
        if let Some(refusal) = response.blocks.iter().find_map(|block| match block {
            ContentBlock::Refusal(value) => Some(value.text.clone()),
            _ => None,
        }) {
            message["refusal"] = json!(refusal);
        }
        if let Some(annotations) = annotations_from_blocks(&response.blocks) {
            message["annotations"] = json!(annotations);
        }
        Ok(json!({
            "id": response.id.clone().unwrap_or_else(|| "chatcmpl_const_api".to_string()),
            "object":"chat.completion",
            "model": response.model.clone().unwrap_or_default(),
            "choices":[{"index":0,"message":message,"finish_reason":encode_finish_reason(response.finish.reason)}],
            "usage": encode_usage(&response.usage),
        }))
    }
}

fn decode_content(
    value: Option<&Value>,
    message_index: usize,
) -> Result<Vec<ContentBlock>, AdapterError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    if let Some(text) = value.as_str() {
        return Ok(vec![ContentBlock::Text(TextBlock::new(text))]);
    }
    let parts = value.as_array().ok_or_else(|| {
        AdapterError::new(
            "invalid_message_content",
            format!("$.messages[{message_index}].content"),
            "content must be string, null, or array",
        )
    })?;
    let mut blocks = Vec::new();
    for part in parts {
        match part.get("type").and_then(Value::as_str).unwrap_or("text") {
            "text" | "input_text" => {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    blocks.push(ContentBlock::Text(TextBlock {
                        text: text.to_string(),
                        metadata: metadata_from_part(part),
                    }));
                }
            }
            "image_url" | "input_image" => blocks.push(ContentBlock::Image(decode_image(part)?)),
            "input_audio" | "audio" => blocks.push(ContentBlock::Audio(decode_audio(part)?)),
            "input_file" | "file" => blocks.push(ContentBlock::File(decode_file(part)?)),
            _ => blocks.push(ContentBlock::ProviderArtifact(OpaqueArtifact {
                kind: ArtifactKind::ProviderSpecific,
                payload: part.clone(),
                affinity: protocol_affinity(ProtocolKind::OpenAiChat),
                replay: ReplayPolicy::ExactWhenCompatible,
                criticality: ArtifactCriticality::Supplemental,
            })),
        }
    }
    Ok(blocks)
}

fn metadata_from_part(part: &Value) -> BlockMetadata {
    BlockMetadata {
        annotations: part
            .get("annotations")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        prompt_cache_breakpoint: decode_prompt_cache_breakpoint(part),
        ..Default::default()
    }
}

fn decode_image(part: &Value) -> Result<MediaBlock, AdapterError> {
    let image = part.get("image_url").unwrap_or(part);
    let url = image
        .as_str()
        .or_else(|| image.get("url").and_then(Value::as_str))
        .ok_or_else(|| {
            AdapterError::new(
                "image_source_missing",
                "$.content.image_url",
                "image URL is required",
            )
        })?;
    let source = decode_data_url(url).unwrap_or_else(|| MediaSource::RemoteUrl {
        url: url.to_string(),
    });
    let detail = match image.get("detail").and_then(Value::as_str) {
        Some("low") => Some(MediaDetail::Low),
        Some("high") => Some(MediaDetail::High),
        Some("original") => Some(MediaDetail::Original),
        Some(_) => Some(MediaDetail::Auto),
        None => None,
    };
    Ok(MediaBlock {
        source,
        detail,
        metadata: metadata_from_part(part),
    })
}

fn decode_audio(part: &Value) -> Result<MediaBlock, AdapterError> {
    let audio = part.get("input_audio").unwrap_or(part);
    let data = audio.get("data").and_then(Value::as_str).ok_or_else(|| {
        AdapterError::new(
            "audio_data_missing",
            "$.content.input_audio.data",
            "audio data is required",
        )
    })?;
    let format = audio.get("format").and_then(Value::as_str).unwrap_or("wav");
    Ok(MediaBlock {
        source: MediaSource::InlineBase64 {
            media_type: format!("audio/{format}"),
            data: data.to_string(),
        },
        detail: None,
        metadata: metadata_from_part(part),
    })
}

fn decode_file(part: &Value) -> Result<FileBlock, AdapterError> {
    let file = part.get("file").unwrap_or(part);
    let source = if let Some(id) = file.get("file_id").and_then(Value::as_str) {
        MediaSource::ProviderFileId {
            provider: "openai".to_string(),
            id: id.to_string(),
            media_type: None,
        }
    } else if let Some(url) = file.get("file_url").and_then(Value::as_str) {
        MediaSource::RemoteUrl {
            url: url.to_string(),
        }
    } else {
        return Err(AdapterError::new(
            "file_source_missing",
            "$.content.file",
            "file source is required",
        ));
    };
    Ok(FileBlock {
        source,
        filename: file
            .get("filename")
            .and_then(Value::as_str)
            .map(str::to_string),
        metadata: metadata_from_part(part),
    })
}

fn decode_data_url(url: &str) -> Option<MediaSource> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let media_type = meta.strip_suffix(";base64")?;
    Some(MediaSource::InlineBase64 {
        media_type: media_type.to_string(),
        data: data.to_string(),
    })
}

fn decode_tool_call(
    value: &Value,
    message: usize,
    index: usize,
    fallback_signature: Option<&Value>,
    mut carrier_artifacts: Vec<OpaqueArtifact>,
) -> Result<ToolCall, AdapterError> {
    let function = value.get("function").ok_or_else(|| {
        AdapterError::new(
            "tool_function_missing",
            format!("$.messages[{message}].tool_calls[{index}].function"),
            "function is required",
        )
    })?;
    let name = function
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| {
            AdapterError::new(
                "tool_name_missing",
                format!("$.messages[{message}].tool_calls[{index}].function.name"),
                "tool name is required",
            )
        })?;
    let id = value
        .get("id")
        .or_else(|| value.get("call_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| {
            AdapterError::new(
                "tool_call_id_missing",
                format!("$.messages[{message}].tool_calls[{index}].id"),
                "tool call ID is required",
            )
        })?;
    let raw = function
        .get("arguments")
        .and_then(Value::as_str)
        .map(str::to_string);
    let arguments = match function.get("arguments") {
        Some(Value::String(raw)) => serde_json::from_str(raw).ok(),
        Some(value) => Some(value.clone()),
        None => Some(json!({})),
    };
    let signature = value
        .pointer("/extra_content/google/thought_signature")
        .or_else(|| value.pointer("/extra_content/google/thoughtSignature"))
        .or(fallback_signature);
    if let Some(payload) = signature {
        match decode_carrier(payload).map_err(|error| {
            AdapterError::new(
                "continuation_carrier_invalid",
                format!("$.messages[{message}].tool_calls[{index}].extra_content"),
                error.message(),
            )
        })? {
            Some(_) => {}
            None if !carrier_artifacts
                .iter()
                .any(|artifact| artifact.kind == ArtifactKind::GeminiThoughtSignature) =>
            {
                carrier_artifacts.push(OpaqueArtifact {
                    kind: ArtifactKind::GeminiThoughtSignature,
                    payload: payload.clone(),
                    // Clients that retain only Google's reviewed compatibility field still get a
                    // usable fallback. CONST's message-level carrier keeps the narrower model
                    // affinity when the client preserves it.
                    affinity: protocol_affinity(ProtocolKind::GeminiNative),
                    replay: ReplayPolicy::Required,
                    criticality: ArtifactCriticality::Required,
                })
            }
            None => {}
        }
    }
    Ok(ToolCall {
        id: id.to_string(),
        source_item_id: None,
        kind: ToolKind::Function,
        name: name.to_string(),
        arguments,
        raw_arguments: raw,
        status: ItemStatus::Completed,
        artifacts: carrier_artifacts,
        metadata: BlockMetadata::default(),
    })
}

fn decode_chat_message_carrier(
    message: &Value,
) -> Result<Option<ContinuationCarrier>, AdapterError> {
    let candidates = message
        .pointer("/extra_fields/const_api/continuation")
        .into_iter()
        .chain(
            message
                .pointer("/extra_fields/google/thought_signature")
                .or_else(|| message.pointer("/extra_fields/google/thoughtSignature"))
                .into_iter(),
        )
        .chain(
            message
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|call| {
                    call.pointer("/extra_content/google/thought_signature")
                        .or_else(|| call.pointer("/extra_content/google/thoughtSignature"))
                }),
        );
    let mut entries = Vec::<ContinuationEntry>::new();
    for value in candidates {
        let decoded = decode_carrier(value).map_err(|error| {
            AdapterError::new(
                "continuation_carrier_invalid",
                "$.message.extra_fields.google.thought_signature",
                error.message(),
            )
        })?;
        let Some(decoded) = decoded else {
            continue;
        };
        for entry in decoded.entries {
            if let Some(existing) = entries.iter().find(|existing| {
                existing.owner == entry.owner && existing.artifact.kind == entry.artifact.kind
            }) {
                if existing != &entry {
                    return Err(AdapterError::new(
                        "continuation_carrier_binding_invalid",
                        "$.message",
                        "message and tool call contain conflicting continuation state",
                    ));
                }
                continue;
            }
            entries.push(entry);
        }
    }
    if entries.is_empty() {
        return Ok(None);
    }
    if encode_carrier(entries.clone()).is_none() {
        return Err(AdapterError::new(
            "continuation_carrier_invalid",
            "$.message",
            "combined continuation state exceeds the validated carrier bounds",
        ));
    }
    Ok(Some(ContinuationCarrier { entries }))
}

fn distribute_chat_carrier(
    carrier: ContinuationCarrier,
) -> (
    Vec<OpaqueArtifact>,
    HashMap<String, Vec<OpaqueArtifact>>,
    Vec<OpaqueArtifact>,
) {
    let mut reasoning = Vec::new();
    let mut tools = HashMap::<String, Vec<OpaqueArtifact>>::new();
    let mut previous = Vec::new();
    for entry in carrier.entries {
        match entry.owner {
            ContinuationOwner::Reasoning => reasoning.push(entry.artifact),
            ContinuationOwner::ToolCall { call_id } => {
                tools.entry(call_id).or_default().push(entry.artifact)
            }
            ContinuationOwner::PreviousPart => previous.push(entry.artifact),
        }
    }
    (reasoning, tools, previous)
}

fn attach_chat_previous_part_artifacts(
    blocks: &mut Vec<ContentBlock>,
    artifacts: Vec<OpaqueArtifact>,
) {
    if artifacts.is_empty() {
        return;
    }
    let insert_at = blocks
        .iter()
        .rposition(|block| {
            !matches!(
                block,
                ContentBlock::Reasoning(_)
                    | ContentBlock::ToolCall(_)
                    | ContentBlock::ToolResult(_)
                    | ContentBlock::ProviderArtifact(_)
            )
        })
        .map_or(blocks.len(), |index| index + 1);
    blocks.splice(
        insert_at..insert_at,
        artifacts.into_iter().map(ContentBlock::ProviderArtifact),
    );
}

fn attach_chat_reasoning_artifacts(blocks: &mut Vec<ContentBlock>, artifacts: Vec<OpaqueArtifact>) {
    if let Some(ContentBlock::Reasoning(reasoning)) = blocks
        .iter_mut()
        .find(|block| matches!(block, ContentBlock::Reasoning(_)))
    {
        reasoning.artifacts.extend(artifacts);
        reasoning.encrypted = reasoning_artifacts_are_encrypted(&reasoning.artifacts);
        return;
    }
    let encrypted = reasoning_artifacts_are_encrypted(&artifacts);
    blocks.insert(
        0,
        ContentBlock::Reasoning(ReasoningBlock {
            text: None,
            summary: Vec::new(),
            artifacts,
            encrypted,
            metadata: BlockMetadata::default(),
        }),
    );
}

fn decode_tools(value: Option<&Value>) -> Result<Vec<ToolDefinition>, AdapterError> {
    let mut tools = Vec::new();
    for (index, item) in value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        if item
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("function")
            != "function"
        {
            return Err(AdapterError::new(
                "unsupported_tool_type",
                format!("$.tools[{index}]"),
                "only function tools are decoded by Chat adapter",
            ));
        }
        let function = item.get("function").unwrap_or(item);
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .ok_or_else(|| {
                AdapterError::new(
                    "tool_name_missing",
                    format!("$.tools[{index}].function.name"),
                    "tool name is required",
                )
            })?;
        let schema = canonicalize_tool_schema(function.get("parameters").unwrap_or(&json!({})))
            .map_err(|error| {
                AdapterError::new(
                    "invalid_tool_schema",
                    format!("$.tools[{index}].function.parameters"),
                    error.to_string(),
                )
            })?;
        tools.push(ToolDefinition::Function(FunctionTool {
            name: name.to_string(),
            description: function
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_string),
            input_schema: schema,
            strict: function.get("strict").and_then(Value::as_bool),
            cache_policy: None,
            defer_loading: function.get("defer_loading").and_then(Value::as_bool),
            allowed_callers: function
                .get("allowed_callers")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            input_examples: function
                .get("input_examples")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            eager_input_streaming: function
                .get("eager_input_streaming")
                .and_then(Value::as_bool),
        }));
    }
    Ok(tools)
}

fn decode_tool_choice(value: Option<&Value>) -> Result<ToolChoice, AdapterError> {
    let Some(value) = value else {
        return Ok(ToolChoice::Auto);
    };
    if let Some(mode) = value.as_str() {
        return Ok(match mode {
            "none" => ToolChoice::None,
            "required" => ToolChoice::Required,
            _ => ToolChoice::Auto,
        });
    }
    let name = value
        .pointer("/function/name")
        .or_else(|| value.get("name"))
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| {
            AdapterError::new(
                "tool_choice_name_missing",
                "$.tool_choice",
                "named tool choice requires a name",
            )
        })?;
    Ok(ToolChoice::Named {
        name: name.to_string(),
    })
}

fn decode_response_format(value: Option<&Value>) -> Result<ResponseFormat, AdapterError> {
    let Some(value) = value else {
        return Ok(ResponseFormat::Text);
    };
    match value.get("type").and_then(Value::as_str).unwrap_or("text") {
        "text" => Ok(ResponseFormat::Text),
        "json_object" => Ok(ResponseFormat::JsonObject),
        "json_schema" => {
            let spec = value.get("json_schema").ok_or_else(|| {
                AdapterError::new(
                    "json_schema_missing",
                    "$.response_format.json_schema",
                    "json_schema is required",
                )
            })?;
            let empty_schema = Value::Object(Map::new());
            let schema = canonicalize_schema(spec.get("schema").unwrap_or(&empty_schema)).map_err(
                |error| {
                    AdapterError::new(
                        error.code(),
                        "$.response_format.json_schema.schema",
                        error.to_string(),
                    )
                },
            )?;
            Ok(ResponseFormat::JsonSchema {
                name: spec
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("response")
                    .to_string(),
                description: spec
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                schema,
                strict: spec.get("strict").and_then(Value::as_bool),
            })
        }
        kind => Err(AdapterError::new(
            "unsupported_response_format",
            "$.response_format.type",
            format!("unsupported response format {kind}"),
        )),
    }
}

fn decode_generation(body: &Value) -> GenerationConfig {
    GenerationConfig {
        max_output_tokens: body
            .get("max_completion_tokens")
            .or_else(|| body.get("max_tokens"))
            .and_then(Value::as_u64),
        temperature: body.get("temperature").and_then(Value::as_f64),
        top_p: body.get("top_p").and_then(Value::as_f64),
        top_k: body.get("top_k").and_then(Value::as_u64),
        frequency_penalty: body.get("frequency_penalty").and_then(Value::as_f64),
        presence_penalty: body.get("presence_penalty").and_then(Value::as_f64),
        seed: body.get("seed").and_then(Value::as_i64),
        stop: decode_strings(body.get("stop")),
        service_tier: body
            .get("service_tier")
            .and_then(Value::as_str)
            .map(str::to_string),
        latency_mode: None,
        verbosity: match body.get("verbosity").and_then(Value::as_str) {
            Some("low") => Some(OutputVerbosity::Low),
            Some("medium") => Some(OutputVerbosity::Medium),
            Some("high") => Some(OutputVerbosity::High),
            _ => None,
        },
        modalities: body
            .get("modalities")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|v| match v.as_str() {
                Some("text") => Some(OutputModality::Text),
                Some("audio") => Some(OutputModality::Audio),
                Some("image") => Some(OutputModality::Image),
                Some("video") => Some(OutputModality::Video),
                _ => None,
            })
            .collect(),
        audio: body.get("audio").map(|a| AudioOutputConfig {
            voice: a.get("voice").and_then(Value::as_str).map(str::to_string),
            format: a.get("format").and_then(Value::as_str).map(str::to_string),
            sample_rate_hz: a
                .get("sample_rate_hz")
                .and_then(Value::as_u64)
                .and_then(|v| u32::try_from(v).ok()),
        }),
        parallel_tool_calls: body.get("parallel_tool_calls").and_then(Value::as_bool),
    }
}

fn decode_strings(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(v)) => vec![v.clone()],
        Some(Value::Array(v)) => v
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn decode_extensions(
    body: &Value,
    _context: &AdapterContext,
    _model: &str,
) -> Vec<ProviderExtension> {
    let recognized = [
        "model",
        "messages",
        "tools",
        "tool_choice",
        "parallel_tool_calls",
        "response_format",
        "reasoning",
        "reasoning_effort",
        "modalities",
        "audio",
        "stream",
        "temperature",
        "top_p",
        "top_k",
        "frequency_penalty",
        "presence_penalty",
        "seed",
        "stop",
        "max_tokens",
        "max_completion_tokens",
        "service_tier",
        "verbosity",
        "user",
        "store",
        "prompt_cache_key",
        "prompt_cache_options",
        "prompt_cache_retention",
    ];
    body.as_object()
        .into_iter()
        .flat_map(|map| map.iter())
        .filter(|(key, value)| {
            !recognized.contains(&key.as_str())
                && !(key.as_str() == "stream_options"
                    && stream_usage_option_is_satisfied_locally(body, value))
        })
        .map(|(name, value)| ProviderExtension {
            namespace: "openai_chat".to_string(),
            name: name.clone(),
            value: value.clone(),
            affinity: protocol_affinity(ProtocolKind::OpenAiChat),
            criticality: ExtensionCriticality::Advisory,
        })
        .collect()
}

fn decode_chat_response_extensions(
    body: &Value,
    context: &AdapterContext,
    model: &str,
) -> Vec<ProviderExtension> {
    let recognized = ["id", "object", "model", "choices", "usage", "error"];
    let mut extensions = body
        .as_object()
        .into_iter()
        .flat_map(|map| map.iter())
        .filter(|(name, _)| !recognized.contains(&name.as_str()))
        .map(|(name, value)| ProviderExtension {
            namespace: "openai_chat".to_string(),
            name: name.clone(),
            value: value.clone(),
            affinity: protocol_affinity(ProtocolKind::OpenAiChat),
            criticality: ExtensionCriticality::Advisory,
        })
        .collect::<Vec<_>>();

    for (choice_index, choice) in body
        .get("choices")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let choice_path = format!("$.choices[{choice_index}]");
        if choice_index > 0 {
            // CanonicalResponseV2 currently models one selected completion. Retain the complete
            // additional choice as an observable extension instead of silently discarding it.
            extensions.push(ProviderExtension {
                namespace: "openai_chat".to_string(),
                name: ProviderExtension::nested_name(&choice_path),
                value: choice.clone(),
                affinity: protocol_affinity(ProtocolKind::OpenAiChat),
                criticality: ExtensionCriticality::Advisory,
            });
            continue;
        }
        collect_nested_unknown_fields(
            &mut extensions,
            "openai_chat",
            Some(choice),
            &["index", "message", "finish_reason"],
            &choice_path,
            context,
            model,
        );
        let Some(message) = choice.get("message") else {
            continue;
        };
        let message_path = format!("{choice_path}.message");
        collect_nested_unknown_fields(
            &mut extensions,
            "openai_chat",
            Some(message),
            &[
                "role",
                "content",
                "reasoning_content",
                "refusal",
                "tool_calls",
                "annotations",
                "extra_fields",
            ],
            &message_path,
            context,
            model,
        );
        scan_chat_content_parts(
            &mut extensions,
            message.get("content"),
            &format!("{message_path}.content"),
            context,
            model,
        );
        scan_chat_tool_calls(
            &mut extensions,
            message.get("tool_calls"),
            &format!("{message_path}.tool_calls"),
            context,
            model,
        );
        scan_chat_gemini_extra_fields(
            &mut extensions,
            message.get("extra_fields"),
            &format!("{message_path}.extra_fields"),
            context,
            model,
        );
    }
    scan_chat_usage_extensions(
        &mut extensions,
        body.get("usage"),
        "$.usage",
        context,
        model,
    );
    collect_nested_unknown_fields(
        &mut extensions,
        "openai_chat",
        body.get("error"),
        &["message", "type", "code", "param", "retryable", "details"],
        "$.error",
        context,
        model,
    );
    extensions
}

fn decode_chat_error_response(body: &Value, context: &AdapterContext) -> CanonicalResponseV2 {
    let error = body.get("error").unwrap_or(&Value::Null);
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let provider_code = error
        .get("code")
        .and_then(Value::as_str)
        .map(str::to_string);
    let error_type = error
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("provider_error")
        .to_string();
    let details = error.get("details").cloned().or_else(|| {
        error
            .get("param")
            .filter(|value| !value.is_null())
            .map(|param| json!({"param": param}))
    });
    CanonicalResponseV2 {
        id: body.get("id").and_then(Value::as_str).map(str::to_string),
        model: (!model.is_empty()).then(|| model.to_string()),
        blocks: Vec::new(),
        status: ResponseStatus::Failed,
        finish: FinishDetail {
            reason: FinishReason::Error,
            original_reason: provider_code.clone().or_else(|| Some(error_type.clone())),
            incomplete_details: None,
        },
        usage: decode_usage(body.get("usage")),
        error: Some(CanonicalError {
            code: error_type,
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("OpenAI-compatible request failed")
                .to_string(),
            retryable: error
                .get("retryable")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            provider_code,
            details,
        }),
        extensions: decode_chat_response_extensions(body, context, model),
    }
}

fn stream_usage_option_is_satisfied_locally(body: &Value, value: &Value) -> bool {
    if body.get("stream").and_then(Value::as_bool) != Some(true) {
        return false;
    }
    let Some(options) = value.as_object() else {
        return false;
    };
    options.len() == 1 && options.get("include_usage").and_then(Value::as_bool) == Some(true)
}

fn scan_chat_content_parts(
    extensions: &mut Vec<ProviderExtension>,
    value: Option<&Value>,
    path: &str,
    context: &AdapterContext,
    model: &str,
) {
    for (part_index, part) in value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let part_path = format!("{path}[{part_index}]");
        collect_nested_unknown_fields(
            extensions,
            "openai_chat",
            Some(part),
            &[
                "type",
                "text",
                "annotations",
                "image_url",
                "input_image",
                "input_audio",
                "audio",
                "input_file",
                "file",
                "file_id",
                "file_url",
                "filename",
                "detail",
            ],
            &part_path,
            context,
            model,
        );
        for container in ["image_url", "input_image"] {
            collect_nested_unknown_fields(
                extensions,
                "openai_chat",
                part.get(container),
                &["url", "detail"],
                &format!("{part_path}.{container}"),
                context,
                model,
            );
        }
        for container in ["input_audio", "audio"] {
            collect_nested_unknown_fields(
                extensions,
                "openai_chat",
                part.get(container),
                &["data", "format"],
                &format!("{part_path}.{container}"),
                context,
                model,
            );
        }
        for container in ["input_file", "file"] {
            collect_nested_unknown_fields(
                extensions,
                "openai_chat",
                part.get(container),
                &["file_id", "file_url", "filename"],
                &format!("{part_path}.{container}"),
                context,
                model,
            );
        }
    }
}

fn scan_chat_tool_calls(
    extensions: &mut Vec<ProviderExtension>,
    value: Option<&Value>,
    path: &str,
    context: &AdapterContext,
    model: &str,
) {
    for (call_index, call) in value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let call_path = format!("{path}[{call_index}]");
        collect_nested_unknown_fields(
            extensions,
            "openai_chat",
            Some(call),
            &["id", "call_id", "type", "function", "extra_content"],
            &call_path,
            context,
            model,
        );
        collect_nested_unknown_fields(
            extensions,
            "openai_chat",
            call.get("function"),
            &["name", "arguments"],
            &format!("{call_path}.function"),
            context,
            model,
        );
        collect_nested_unknown_fields(
            extensions,
            "openai_chat",
            call.get("extra_content"),
            &["google"],
            &format!("{call_path}.extra_content"),
            context,
            model,
        );
        collect_nested_unknown_fields(
            extensions,
            "openai_chat",
            call.pointer("/extra_content/google"),
            &["thought_signature", "thoughtSignature"],
            &format!("{call_path}.extra_content.google"),
            context,
            model,
        );
    }
}

fn scan_chat_gemini_extra_fields(
    extensions: &mut Vec<ProviderExtension>,
    value: Option<&Value>,
    path: &str,
    context: &AdapterContext,
    model: &str,
) {
    collect_nested_unknown_fields(
        extensions,
        "openai_chat",
        value,
        &["google"],
        path,
        context,
        model,
    );
    collect_nested_unknown_fields(
        extensions,
        "openai_chat",
        value.and_then(|value| value.get("google")),
        &["thought_signature", "thoughtSignature"],
        &format!("{path}.google"),
        context,
        model,
    );
}

fn scan_chat_usage_extensions(
    extensions: &mut Vec<ProviderExtension>,
    value: Option<&Value>,
    path: &str,
    context: &AdapterContext,
    model: &str,
) {
    collect_nested_unknown_fields(
        extensions,
        "openai_chat",
        value,
        &[
            "prompt_tokens",
            "completion_tokens",
            "total_tokens",
            "prompt_tokens_details",
            "completion_tokens_details",
        ],
        path,
        context,
        model,
    );
    collect_nested_unknown_fields(
        extensions,
        "openai_chat",
        value.and_then(|usage| usage.get("prompt_tokens_details")),
        &["cached_tokens", "cache_write_tokens", "audio_tokens"],
        &format!("{path}.prompt_tokens_details"),
        context,
        model,
    );
    collect_nested_unknown_fields(
        extensions,
        "openai_chat",
        value.and_then(|usage| usage.get("completion_tokens_details")),
        &[
            "reasoning_tokens",
            "audio_tokens",
            "accepted_prediction_tokens",
            "rejected_prediction_tokens",
        ],
        &format!("{path}.completion_tokens_details"),
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
            "openai_chat",
            Some(message),
            &[
                "role",
                "content",
                "name",
                "tool_call_id",
                "call_id",
                "id",
                "reasoning_content",
                "reasoning",
                "refusal",
                "tool_calls",
                "annotations",
                "extra_fields",
            ],
            &message_path,
            context,
            model,
        );
        scan_chat_gemini_extra_fields(
            &mut extensions,
            message.get("extra_fields"),
            &format!("{message_path}.extra_fields"),
            context,
            model,
        );
        for (part_index, part) in message
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let part_path = format!("{message_path}.content[{part_index}]");
            collect_nested_unknown_fields(
                &mut extensions,
                "openai_chat",
                Some(part),
                &[
                    "type",
                    "text",
                    "annotations",
                    "image_url",
                    "input_image",
                    "input_audio",
                    "audio",
                    "input_file",
                    "file",
                    "file_id",
                    "file_url",
                    "filename",
                    "detail",
                    "prompt_cache_breakpoint",
                ],
                &part_path,
                context,
                model,
            );
            collect_nested_unknown_fields(
                &mut extensions,
                "openai_chat",
                part.get("image_url"),
                &["url", "detail"],
                &format!("{part_path}.image_url"),
                context,
                model,
            );
            for container in ["input_audio", "audio"] {
                collect_nested_unknown_fields(
                    &mut extensions,
                    "openai_chat",
                    part.get(container),
                    &["data", "format"],
                    &format!("{part_path}.{container}"),
                    context,
                    model,
                );
            }
            for container in ["input_file", "file"] {
                collect_nested_unknown_fields(
                    &mut extensions,
                    "openai_chat",
                    part.get(container),
                    &["file_id", "file_url", "filename"],
                    &format!("{part_path}.{container}"),
                    context,
                    model,
                );
            }
        }
        for (call_index, call) in message
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let call_path = format!("{message_path}.tool_calls[{call_index}]");
            collect_nested_unknown_fields(
                &mut extensions,
                "openai_chat",
                Some(call),
                &["id", "call_id", "type", "function", "extra_content"],
                &call_path,
                context,
                model,
            );
            collect_nested_unknown_fields(
                &mut extensions,
                "openai_chat",
                call.get("function"),
                &["name", "arguments"],
                &format!("{call_path}.function"),
                context,
                model,
            );
            collect_nested_unknown_fields(
                &mut extensions,
                "openai_chat",
                call.get("extra_content"),
                &["google"],
                &format!("{call_path}.extra_content"),
                context,
                model,
            );
            collect_nested_unknown_fields(
                &mut extensions,
                "openai_chat",
                call.pointer("/extra_content/google"),
                &["thought_signature", "thoughtSignature"],
                &format!("{call_path}.extra_content.google"),
                context,
                model,
            );
        }
    }
    for (tool_index, tool) in body
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let tool_path = format!("$.tools[{tool_index}]");
        collect_nested_unknown_fields(
            &mut extensions,
            "openai_chat",
            Some(tool),
            &["type", "function"],
            &tool_path,
            context,
            model,
        );
        collect_nested_unknown_fields(
            &mut extensions,
            "openai_chat",
            tool.get("function").or(Some(tool)),
            &[
                "name",
                "description",
                "parameters",
                "strict",
                "defer_loading",
                "allowed_callers",
                "input_examples",
                "eager_input_streaming",
                "type",
                "function",
            ],
            &format!("{tool_path}.function"),
            context,
            model,
        );
    }
    collect_nested_unknown_fields(
        &mut extensions,
        "openai_chat",
        body.get("tool_choice"),
        &["type", "function", "name"],
        "$.tool_choice",
        context,
        model,
    );
    collect_nested_unknown_fields(
        &mut extensions,
        "openai_chat",
        body.pointer("/tool_choice/function"),
        &["name"],
        "$.tool_choice.function",
        context,
        model,
    );
    collect_nested_unknown_fields(
        &mut extensions,
        "openai_chat",
        body.get("response_format"),
        &["type", "json_schema"],
        "$.response_format",
        context,
        model,
    );
    collect_nested_unknown_fields(
        &mut extensions,
        "openai_chat",
        body.pointer("/response_format/json_schema"),
        &["name", "description", "schema", "strict"],
        "$.response_format.json_schema",
        context,
        model,
    );
    collect_nested_unknown_fields(
        &mut extensions,
        "openai_chat",
        body.get("audio"),
        &["voice", "format", "sample_rate_hz"],
        "$.audio",
        context,
        model,
    );
    collect_nested_unknown_fields(
        &mut extensions,
        "openai_chat",
        body.get("prompt_cache_options"),
        &["mode", "ttl"],
        "$.prompt_cache_options",
        context,
        model,
    );
    extensions
}

fn first_block_metadata_mut(blocks: &mut [ContentBlock]) -> Option<&mut BlockMetadata> {
    blocks.iter_mut().find_map(|block| match block {
        ContentBlock::Text(v) => Some(&mut v.metadata),
        ContentBlock::Reasoning(v) => Some(&mut v.metadata),
        ContentBlock::Refusal(v) => Some(&mut v.metadata),
        ContentBlock::Image(v) | ContentBlock::Audio(v) | ContentBlock::Video(v) => {
            Some(&mut v.metadata)
        }
        ContentBlock::File(v) => Some(&mut v.metadata),
        ContentBlock::ToolCall(v) => Some(&mut v.metadata),
        ContentBlock::ToolResult(v) => Some(&mut v.metadata),
        ContentBlock::ProviderArtifact(_) => None,
    })
}

fn annotations_from_blocks(blocks: &[ContentBlock]) -> Option<&[Value]> {
    blocks.iter().find_map(|block| {
        let metadata = match block {
            ContentBlock::Text(value) => &value.metadata,
            ContentBlock::Reasoning(value) => &value.metadata,
            ContentBlock::Refusal(value) => &value.metadata,
            ContentBlock::Image(value)
            | ContentBlock::Audio(value)
            | ContentBlock::Video(value) => &value.metadata,
            ContentBlock::File(value) => &value.metadata,
            ContentBlock::ToolCall(value) => &value.metadata,
            ContentBlock::ToolResult(value) => &value.metadata,
            ContentBlock::ProviderArtifact(_) => return None,
        };
        (!metadata.annotations.is_empty()).then_some(metadata.annotations.as_slice())
    })
}

fn encode_turn(
    turn: &Turn,
    turn_index: usize,
    plan: &ConversionPlan,
    messages: &mut Vec<Value>,
) -> Result<(), AdapterError> {
    if turn.role == TurnRole::Assistant {
        let mut message = json!({"role":"assistant","content":encode_assistant_text(&turn.blocks)});
        let calls = turn
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(block_index, block)| match block {
                ContentBlock::ToolCall(call) => Some(encode_tool_call(
                    call,
                    plan,
                    &format!("turns[{turn_index}].blocks[{block_index}]"),
                )),
                _ => None,
            })
            .collect::<Vec<_>>();
        if !calls.is_empty() {
            if let Some(signature) = gemini_signature_from_encoded_calls(&calls) {
                message["extra_fields"] = json!({"google":{"thought_signature":signature}});
            }
            message["tool_calls"] = Value::Array(calls);
        }
        let carrier_entries =
            preserved_chat_entries(&turn.blocks, &format!("turns[{turn_index}].blocks"), plan);
        if chat_entries_need_carrier(&carrier_entries) {
            if let Some(carrier) = encode_carrier(carrier_entries) {
                set_chat_continuation_carrier(&mut message, carrier);
            }
        }
        if let Some(reasoning) = turn
            .blocks
            .iter()
            .enumerate()
            .find_map(|(block_index, block)| match block {
                ContentBlock::Reasoning(v)
                    if !plan.omits_feature_at(
                        Feature::Reasoning,
                        &format!("turns[{turn_index}].blocks[{block_index}]"),
                    ) =>
                {
                    v.text.clone()
                }
                _ => None,
            })
        {
            message["reasoning_content"] = json!(reasoning);
        }
        if let Some(refusal) = turn.blocks.iter().find_map(|block| match block {
            ContentBlock::Refusal(v) => Some(v.text.clone()),
            _ => None,
        }) {
            message["refusal"] = json!(refusal);
        }
        if let Some(annotations) = annotations_from_blocks(&turn.blocks) {
            message["annotations"] = json!(annotations);
        }
        if message.get("tool_calls").is_some() {
            if let Some(previous) = messages.last_mut().filter(|previous| {
                previous.get("role").and_then(Value::as_str) == Some("assistant")
                    && previous.get("tool_calls").is_none()
            }) {
                previous["tool_calls"] = message["tool_calls"].clone();
                if previous.get("content").is_none_or(Value::is_null)
                    && message.get("content").is_some_and(|value| !value.is_null())
                {
                    previous["content"] = message["content"].clone();
                }
                for field in [
                    "reasoning_content",
                    "refusal",
                    "annotations",
                    "extra_fields",
                ] {
                    if let Some(value) = message.get(field) {
                        previous[field] = value.clone();
                    }
                }
                return Ok(());
            }
        }
        messages.push(message);
        return Ok(());
    }
    let mut regular = Vec::new();
    let mut regular_start = 0;
    for (block_index, block) in turn.blocks.iter().enumerate() {
        match block {
            ContentBlock::ToolResult(result) => {
                flush_user_blocks(&mut regular, regular_start, turn_index, plan, messages)?;
                messages.push(json!({
                    "role":"tool",
                    "tool_call_id":result.call_id,
                    "name":result.name.clone().unwrap_or_else(||"tool".to_string()),
                    "content":encode_content(
                        &result.content,
                        plan,
                        &format!("turns[{turn_index}].blocks[{block_index}].blocks"),
                        0,
                    )?,
                }));
                regular_start = block_index + 1;
            }
            block => regular.push(block.clone()),
        }
    }
    flush_user_blocks(&mut regular, regular_start, turn_index, plan, messages)?;
    Ok(())
}

fn flush_user_blocks(
    blocks: &mut Vec<ContentBlock>,
    start_index: usize,
    turn_index: usize,
    plan: &ConversionPlan,
    messages: &mut Vec<Value>,
) -> Result<(), AdapterError> {
    if !blocks.is_empty() {
        messages.push(json!({
            "role":"user",
            "content":encode_content(
                blocks,
                plan,
                &format!("turns[{turn_index}].blocks"),
                start_index,
            )?,
        }));
        blocks.clear();
    }
    Ok(())
}

fn encode_content(
    blocks: &[ContentBlock],
    plan: &ConversionPlan,
    path: &str,
    start_index: usize,
) -> Result<Value, AdapterError> {
    if blocks.len() == 1 {
        if let ContentBlock::Text(text) = &blocks[0] {
            if text.metadata.prompt_cache_breakpoint.is_none() {
                return Ok(json!(text.text));
            }
        }
    }
    let mut parts = Vec::new();
    for (local_index, block) in blocks.iter().enumerate() {
        let block_path = format!("{path}[{}]", start_index + local_index);
        match block {
            ContentBlock::Text(v) => {
                let mut value = json!({"type":"text","text":v.text});
                encode_prompt_cache_breakpoint(&mut value, &v.metadata);
                parts.push(value);
            }
            ContentBlock::Image(v) => parts.push(encode_image(v)?),
            ContentBlock::Audio(v) => parts.push(encode_audio(v)?),
            ContentBlock::File(v) => parts.push(encode_file(v)?),
            ContentBlock::Refusal(v) => {
                let mut value = json!({"type":"refusal","refusal":v.text});
                encode_prompt_cache_breakpoint(&mut value, &v.metadata);
                parts.push(value);
            }
            ContentBlock::ProviderArtifact(artifact)
                if artifact_is_preserved(plan, &block_path) =>
            {
                parts.push(artifact.payload.clone())
            }
            ContentBlock::ProviderArtifact(_) => {}
            ContentBlock::Reasoning(_)
                if plan.omits_feature_at(Feature::Reasoning, &block_path) => {}
            ContentBlock::Reasoning(_)
            | ContentBlock::ToolCall(_)
            | ContentBlock::ToolResult(_)
            | ContentBlock::Video(_) => {
                return Err(AdapterError::new(
                    "encoder_plan_violation",
                    block_path,
                    "plan did not authorize this Chat content mapping",
                ));
            }
        }
    }
    Ok(Value::Array(parts))
}

fn artifact_is_preserved(plan: &ConversionPlan, path: &str) -> bool {
    plan.level() == ConversionLevel::Native
        || plan.artifact_actions().iter().any(|action| {
            action.path == path
                && action.action == crate::protocol::conversion::ArtifactDisposition::Preserve
        })
}

fn encode_assistant_text(blocks: &[ContentBlock]) -> Value {
    let text = blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text(v) => Some(v.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    if text.is_empty() {
        Value::Null
    } else {
        json!(text)
    }
}
fn encode_tool_call(call: &ToolCall, plan: &ConversionPlan, path: &str) -> Value {
    let mut value = json!({"id":call.id,"type":"function","function":{"name":call.name,"arguments":call.raw_arguments.clone().unwrap_or_else(||serde_json::to_string(call.arguments.as_ref().unwrap_or(&json!({}))).unwrap_or_else(|_|"{}".to_string()))}});
    let preserved = call
        .artifacts
        .iter()
        .enumerate()
        .filter(|(artifact_index, artifact)| {
            is_portable_artifact(&artifact.kind)
                && artifact_is_preserved(plan, &format!("{path}.artifacts[{artifact_index}]"))
        })
        .map(|(_, artifact)| artifact.clone())
        .collect::<Vec<_>>();
    if preserved.len() == 1 && preserved[0].kind == ArtifactKind::GeminiThoughtSignature {
        let signature = &preserved[0];
        value["extra_content"] = json!({"google":{"thought_signature":signature.payload}});
    } else if let Some(carrier) = encode_carrier(tool_call_entries(&call.id, &preserved)) {
        value["extra_content"] = json!({"google":{"thought_signature":carrier}});
    }
    value
}

fn preserved_chat_entries(
    blocks: &[ContentBlock],
    base_path: &str,
    plan: &ConversionPlan,
) -> Vec<ContinuationEntry> {
    let mut entries = Vec::new();
    for (block_index, block) in blocks.iter().enumerate() {
        match block {
            ContentBlock::Reasoning(reasoning) => {
                let artifacts = reasoning
                    .artifacts
                    .iter()
                    .enumerate()
                    .filter(|(artifact_index, artifact)| {
                        is_portable_artifact(&artifact.kind)
                            && artifact_is_preserved(
                                plan,
                                &format!("{base_path}[{block_index}].artifacts[{artifact_index}]"),
                            )
                    })
                    .map(|(_, artifact)| artifact.clone())
                    .collect::<Vec<_>>();
                entries.extend(reasoning_entries(&artifacts));
            }
            ContentBlock::ToolCall(call) => {
                let artifacts = call
                    .artifacts
                    .iter()
                    .enumerate()
                    .filter(|(artifact_index, artifact)| {
                        is_portable_artifact(&artifact.kind)
                            && artifact_is_preserved(
                                plan,
                                &format!("{base_path}[{block_index}].artifacts[{artifact_index}]"),
                            )
                    })
                    .map(|(_, artifact)| artifact.clone())
                    .collect::<Vec<_>>();
                entries.extend(tool_call_entries(&call.id, &artifacts));
            }
            ContentBlock::ProviderArtifact(artifact)
                if is_portable_artifact(&artifact.kind)
                    && artifact_is_preserved(plan, &format!("{base_path}[{block_index}]")) =>
            {
                entries.extend(previous_part_entries(std::slice::from_ref(artifact)));
            }
            _ => {}
        }
    }
    entries
}

fn chat_entries_need_carrier(entries: &[ContinuationEntry]) -> bool {
    !entries.is_empty()
}

fn set_chat_continuation_carrier(message: &mut Value, carrier: String) {
    let Some(message) = message.as_object_mut() else {
        return;
    };
    let extra_fields = message.entry("extra_fields").or_insert_with(|| json!({}));
    if !extra_fields.is_object() {
        *extra_fields = json!({});
    }
    extra_fields["const_api"]["continuation"] = json!(carrier);
}

fn gemini_signature_from_encoded_calls(calls: &[Value]) -> Option<&Value> {
    calls.iter().find_map(|call| {
        call.pointer("/extra_content/google/thought_signature")
            .or_else(|| call.pointer("/extra_content/google/thoughtSignature"))
    })
}

fn feature_is_preserved(plan: &ConversionPlan, path: &str) -> bool {
    plan.feature_actions().iter().any(|action| {
        action.path == path
            && matches!(
                action.action,
                FeatureDisposition::Preserve | FeatureDisposition::Encode
            )
    })
}

fn encode_image(media: &MediaBlock) -> Result<Value, AdapterError> {
    let url = match &media.source {
        MediaSource::InlineBase64 { media_type, data } => {
            format!("data:{media_type};base64,{data}")
        }
        MediaSource::RemoteUrl { url } => url.clone(),
        MediaSource::ProviderFileId { .. } => {
            return Err(AdapterError::new(
                "encoder_plan_violation",
                "$.content.image",
                "Chat image cannot encode provider file ID",
            ));
        }
    };
    let mut value = json!({"type":"image_url","image_url":{"url":url,"detail":media.detail.map(|d| match d {MediaDetail::Low=>"low",MediaDetail::High=>"high",MediaDetail::Original=>"original",MediaDetail::Auto=>"auto"})}});
    encode_prompt_cache_breakpoint(&mut value, &media.metadata);
    Ok(value)
}
fn encode_audio(media: &MediaBlock) -> Result<Value, AdapterError> {
    if let MediaSource::InlineBase64 { media_type, data } = &media.source {
        let mut value = json!({"type":"input_audio","input_audio":{"data":data,"format":media_type.split('/').next_back().unwrap_or("wav")}});
        encode_prompt_cache_breakpoint(&mut value, &media.metadata);
        Ok(value)
    } else {
        Err(AdapterError::new(
            "encoder_plan_violation",
            "$.content.audio",
            "Chat audio requires inline data",
        ))
    }
}
fn encode_file(file: &FileBlock) -> Result<Value, AdapterError> {
    let mut value = match &file.source {
        MediaSource::ProviderFileId { id, .. } => {
            json!({"type":"input_file","file_id":id,"filename":file.filename})
        }
        MediaSource::RemoteUrl { url } => {
            json!({"type":"input_file","file_url":url,"filename":file.filename})
        }
        _ => {
            return Err(AdapterError::new(
                "encoder_plan_violation",
                "$.content.file",
                "Chat file cannot encode this source",
            ));
        }
    };
    encode_prompt_cache_breakpoint(&mut value, &file.metadata);
    Ok(value)
}

fn encode_tools(tools: &[ToolDefinition], plan: &ConversionPlan) -> Result<Value, AdapterError> {
    let flatten_namespaces = plan.feature_actions().iter().any(|action| {
        action.feature == Feature::NamespaceTools
            && action.action == FeatureDisposition::FlattenNamespace
    });
    if flatten_namespaces {
        if tools.iter().any(|tool| {
            !matches!(
                tool,
                ToolDefinition::Function(_) | ToolDefinition::Namespace(_)
            )
        }) {
            return Err(AdapterError::new(
                "encoder_plan_violation",
                "$.tools",
                "namespace flattening cannot encode another non-function tool kind",
            ));
        }
        let flattened = flatten_namespace_tools(tools).map_err(|error| {
            AdapterError::new("encoder_plan_violation", "$.tools", error.to_string())
        })?;
        return Ok(Value::Array(
            flattened.tools.iter().map(encode_function_tool).collect(),
        ));
    }
    tools
        .iter()
        .map(|tool| match tool {
            ToolDefinition::Function(tool) => Ok(encode_function_tool(tool)),
            _ => Err(AdapterError::new(
                "encoder_plan_violation",
                "$.tools",
                "Chat adapter has no authorized mapping for this tool kind",
            )),
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Value::Array)
}

fn encode_function_tool(tool: &FunctionTool) -> Value {
    json!({
        "type":"function",
        "function":{
            "name":tool.name,
            "description":tool.description,
            "parameters":tool.input_schema,
            "strict":tool.strict,
        }
    })
}
fn encode_tool_choice(choice: &ToolChoice) -> Result<Value, AdapterError> {
    Ok(match choice {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::None => json!("none"),
        ToolChoice::Required => json!("required"),
        ToolChoice::Named { name } => json!({"type":"function","function":{"name":name}}),
        ToolChoice::Allowed { .. } => {
            return Err(AdapterError::new(
                "encoder_plan_violation",
                "$.tool_choice",
                "allowed-list choice is not Chat-compatible",
            ));
        }
    })
}
fn encode_response_format(format: &ResponseFormat) -> Result<Value, AdapterError> {
    Ok(match format {
        ResponseFormat::Text => json!({"type":"text"}),
        ResponseFormat::JsonObject => json!({"type":"json_object"}),
        ResponseFormat::JsonSchema {
            name,
            description,
            schema,
            strict,
        } => {
            json!({"type":"json_schema","json_schema":{"name":name,"description":description,"schema":schema,"strict":strict}})
        }
    })
}
fn decode_reasoning(body: &Value, context: &AdapterContext, model: &str) -> ReasoningConfig {
    let reasoning = body.get("reasoning").and_then(Value::as_object);
    let effort = body
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .or_else(|| {
            reasoning
                .and_then(|value| value.get("effort"))
                .and_then(Value::as_str)
        })
        .and_then(ReasoningEffort::parse);
    let token_budget = reasoning
        .and_then(|value| {
            value
                .get("max_tokens")
                .or_else(|| value.get("budget_tokens"))
        })
        .and_then(Value::as_u64);
    let summary = match reasoning
        .and_then(|value| {
            value
                .get("summary")
                .or_else(|| value.get("generate_summary"))
        })
        .and_then(Value::as_str)
    {
        Some("auto") => ReasoningSummaryMode::Auto,
        Some("concise") => ReasoningSummaryMode::Concise,
        Some("detailed") => ReasoningSummaryMode::Detailed,
        _ => ReasoningSummaryMode::None,
    };
    let explicitly_enabled = reasoning
        .and_then(|value| value.get("enabled"))
        .and_then(Value::as_bool);
    let explicitly_disabled =
        explicitly_enabled == Some(false) || effort == Some(ReasoningEffort::None);
    let mode = if explicitly_disabled {
        ReasoningMode::Disabled
    } else if token_budget.is_some() {
        ReasoningMode::Enabled
    } else if explicitly_enabled == Some(true)
        || effort.is_some()
        || !matches!(summary, ReasoningSummaryMode::None)
    {
        ReasoningMode::Automatic
    } else {
        ReasoningMode::Disabled
    };
    let disabled = mode == ReasoningMode::Disabled;
    let recognized = [
        "enabled",
        "effort",
        "max_tokens",
        "budget_tokens",
        "summary",
        "generate_summary",
    ];
    ReasoningConfig {
        mode,
        // Keep an explicit `none` so a cross-protocol hop can distinguish
        // "disable reasoning" from "use the target model's default".
        effort: if explicitly_disabled {
            // Preserve an explicit disable marker. A missing reasoning field is
            // also represented by Disabled mode, but must remain distinguishable
            // so a cross-protocol target can use its model default.
            Some(ReasoningEffort::None)
        } else if disabled {
            None
        } else {
            effort
        },
        token_budget: if disabled { None } else { token_budget },
        summary: if disabled {
            ReasoningSummaryMode::None
        } else {
            summary
        },
        provider_extensions: reasoning
            .into_iter()
            .flat_map(|value| value.iter())
            .filter(|(name, _)| !recognized.contains(&name.as_str()))
            .map(|(name, value)| ProviderExtension {
                namespace: "openai_chat".to_string(),
                name: name.clone(),
                value: value.clone(),
                affinity: exact_issuer_affinity(context, model),
                criticality: ExtensionCriticality::Advisory,
            })
            .collect(),
    }
}

fn encode_reasoning(body: &mut Value, reasoning: &ReasoningConfig, plan: &ConversionPlan) {
    // OpenAI-compatible endpoints use an open string field in practice. These
    // are the portable values known to the IR; native requests bypass this
    // encoder and retain unknown values byte-for-byte.
    let effort = reasoning.effort.map(ReasoningEffort::openai_name);
    if let Some(effort) = effort {
        body["reasoning_effort"] = json!(effort);
    }
    let mut extensions = Map::new();
    for (index, extension) in reasoning.provider_extensions.iter().enumerate() {
        if extension.namespace == "openai_chat"
            && feature_is_preserved(plan, &format!("reasoning.provider_extensions[{index}]"))
        {
            extensions.insert(extension.name.clone(), extension.value.clone());
        }
    }
    if !extensions.is_empty() {
        body["reasoning"] = Value::Object(extensions);
    }
}

fn encode_generation(body: &mut Value, g: &GenerationConfig, context: &AdapterContext) {
    if let Some(v) = g.max_output_tokens {
        let field = match context.dialect {
            ProviderDialect::OpenAi | ProviderDialect::AzureOpenAi => "max_completion_tokens",
            ProviderDialect::DeepSeek | ProviderDialect::Compatible { .. } => "max_tokens",
        };
        body[field] = json!(v)
    }
    if let Some(v) = g.temperature {
        body["temperature"] = json!(v)
    }
    if let Some(v) = g.top_p {
        body["top_p"] = json!(v)
    }
    if let Some(v) = g.top_k {
        body["top_k"] = json!(v)
    }
    if let Some(v) = g.frequency_penalty {
        body["frequency_penalty"] = json!(v)
    }
    if let Some(v) = g.presence_penalty {
        body["presence_penalty"] = json!(v)
    }
    if let Some(v) = g.seed {
        body["seed"] = json!(v)
    }
    if !g.stop.is_empty() {
        body["stop"] = json!(g.stop)
    }
    if let Some(v) = &g.service_tier {
        body["service_tier"] = json!(v)
    }
    if let Some(v) = g.verbosity {
        body["verbosity"] = json!(match v {
            OutputVerbosity::Low => "low",
            OutputVerbosity::Medium => "medium",
            OutputVerbosity::High => "high",
        })
    }
    if let Some(v) = g.parallel_tool_calls {
        body["parallel_tool_calls"] = json!(v)
    }
    if !g.modalities.is_empty() {
        body["modalities"] = json!(
            g.modalities
                .iter()
                .map(|m| match m {
                    OutputModality::Text => "text",
                    OutputModality::Audio => "audio",
                    OutputModality::Image => "image",
                    OutputModality::Video => "video",
                })
                .collect::<Vec<_>>()
        )
    }
    if let Some(a) = &g.audio {
        body["audio"] = json!({"voice":a.voice,"format":a.format,"sample_rate_hz":a.sample_rate_hz})
    }
}

fn finish_reason(value: Option<&str>) -> FinishReason {
    match value {
        Some("stop") => FinishReason::Stop,
        Some("length") => FinishReason::Length,
        Some("tool_calls") | Some("function_call") => FinishReason::ToolCalls,
        Some("content_filter") => FinishReason::ContentFilter,
        Some("refusal") => FinishReason::Refusal,
        Some("cancelled") => FinishReason::Cancelled,
        Some("error") => FinishReason::Error,
        _ => FinishReason::Unknown,
    }
}
fn encode_finish_reason(value: FinishReason) -> &'static str {
    match value {
        FinishReason::Stop => "stop",
        FinishReason::Length => "length",
        FinishReason::ToolCalls => "tool_calls",
        FinishReason::PauseTurn => "stop",
        FinishReason::ContentFilter => "content_filter",
        FinishReason::Refusal => "refusal",
        FinishReason::Error => "error",
        FinishReason::Cancelled => "cancelled",
        FinishReason::Unknown => "stop",
    }
}
fn reported(value: Option<&Value>) -> UsageValue {
    value
        .and_then(Value::as_i64)
        .and_then(|v| UsageValue::reported(v).ok())
        .unwrap_or_default()
}
fn decode_usage(value: Option<&Value>) -> Usage {
    let Some(v) = value else {
        return Usage::default();
    };
    Usage {
        input_tokens: reported(v.get("prompt_tokens")),
        output_tokens: reported(v.get("completion_tokens")),
        total_tokens: reported(v.get("total_tokens")),
        cache_read_tokens: reported(v.pointer("/prompt_tokens_details/cached_tokens")),
        cache_write_tokens: reported(v.pointer("/prompt_tokens_details/cache_write_tokens")),
        reasoning_tokens: reported(v.pointer("/completion_tokens_details/reasoning_tokens")),
        audio_input_units: reported(v.pointer("/prompt_tokens_details/audio_tokens")),
        audio_output_units: reported(v.pointer("/completion_tokens_details/audio_tokens")),
        accepted_prediction_tokens: reported(
            v.pointer("/completion_tokens_details/accepted_prediction_tokens"),
        ),
        rejected_prediction_tokens: reported(
            v.pointer("/completion_tokens_details/rejected_prediction_tokens"),
        ),
        ..Default::default()
    }
    .with_inclusive_reasoning_output()
}
fn encode_usage(u: &Usage) -> Value {
    let cache_write = u.cache_write_tokens.value.or(u.cache_creation_tokens.value);
    json!({"prompt_tokens":u.input_tokens.value,"completion_tokens":u.output_tokens.value,"total_tokens":u.total_tokens.value,"prompt_tokens_details":{"cached_tokens":u.cache_read_tokens.value,"cache_write_tokens":cache_write,"audio_tokens":u.audio_input_units.value},"completion_tokens_details":{"reasoning_tokens":u.reasoning_tokens.value,"audio_tokens":u.audio_output_units.value,"accepted_prediction_tokens":u.accepted_prediction_tokens.value,"rejected_prediction_tokens":u.rejected_prediction_tokens.value}})
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;
    use crate::protocol::adapters::{AdapterContext, ProtocolAdapter, ProviderDialect};
    use crate::protocol::capability::CapabilityProfile;
    use crate::protocol::conversion::{ConversionPolicy, IssuerIdentity, plan_conversion};
    use crate::protocol::ir::{ContentBlock, ResponseFormat, ToolChoice, UsageProvenance};
    use crate::protocol::kind::ProtocolKind;

    fn fixture() -> Value {
        serde_json::from_str(include_str!(
            "../../../tests/fixtures/protocol-v2/openai-chat/advanced.json"
        ))
        .unwrap()
    }

    fn context() -> AdapterContext {
        AdapterContext {
            dialect: ProviderDialect::DeepSeek,
            issuer: IssuerIdentity {
                provider: "deepseek".to_string(),
                endpoint_fingerprint: "endpoint".to_string(),
                account_fingerprint: Some("account".to_string()),
                model_family: Some("chat-test".to_string()),
            },
        }
    }

    #[test]
    fn chat_carrier_merges_parallel_tool_subsets_and_rejects_conflicts() {
        let first = crate::protocol::continuation::stream_artifact(
            ArtifactKind::GeminiThoughtSignature,
            json!("signature-a"),
            ContentBlockKind::ToolCall,
            Some("gemini-test"),
        );
        let second = crate::protocol::continuation::stream_artifact(
            ArtifactKind::GeminiThoughtSignature,
            json!("signature-b"),
            ContentBlockKind::ToolCall,
            Some("gemini-test"),
        );
        let first_carrier = encode_carrier(tool_call_entries("call-a", &[first])).unwrap();
        let second_carrier = encode_carrier(tool_call_entries("call-b", &[second])).unwrap();
        let message = json!({
            "tool_calls":[
                {"id":"call-a","extra_content":{"google":{"thought_signature":first_carrier.clone()}}},
                {"id":"call-b","extra_content":{"google":{"thought_signature":second_carrier}}}
            ]
        });
        let merged = decode_chat_message_carrier(&message).unwrap().unwrap();
        assert_eq!(merged.entries.len(), 2);

        let conflicting = crate::protocol::continuation::stream_artifact(
            ArtifactKind::GeminiThoughtSignature,
            json!("different-signature"),
            ContentBlockKind::ToolCall,
            Some("gemini-test"),
        );
        let conflicting_carrier =
            encode_carrier(tool_call_entries("call-a", &[conflicting])).unwrap();
        let message = json!({
            "extra_fields":{"google":{"thought_signature":first_carrier}},
            "tool_calls":[{
                "id":"call-a",
                "extra_content":{"google":{"thought_signature":conflicting_carrier}}
            }]
        });
        assert!(decode_chat_message_carrier(&message).is_err());
    }

    #[test]
    fn decodes_ordered_advanced_chat_request_and_response() {
        let adapter = OpenAiChatAdapter;
        let fixture = fixture();
        let request = adapter
            .decode_request(&fixture["request"], &context())
            .unwrap();
        assert_eq!(request.instructions.len(), 2);
        assert_eq!(request.turns.len(), 3);
        assert!(matches!(request.turns[0].blocks[1], ContentBlock::Image(_)));
        assert!(matches!(request.turns[0].blocks[2], ContentBlock::Audio(_)));
        assert!(matches!(
            request.turns[1].blocks[0],
            ContentBlock::Reasoning(_)
        ));
        let calls = request.turns[1]
            .blocks
            .iter()
            .filter(|block| matches!(block, ContentBlock::ToolCall(_)))
            .count();
        assert_eq!(calls, 2);
        let ContentBlock::ToolCall(incomplete) = &request.turns[1].blocks[2] else {
            panic!("second tool call");
        };
        assert!(incomplete.arguments.is_none());
        assert_eq!(incomplete.raw_arguments.as_deref(), Some("{\"id\":2"));
        assert!(matches!(request.tool_choice, ToolChoice::Named { .. }));
        assert!(matches!(
            request.response_format,
            ResponseFormat::JsonSchema { .. }
        ));
        assert_eq!(request.extensions[0].name, "x_provider_option");

        let response = adapter
            .decode_response(&fixture["response"], &context())
            .unwrap();
        assert_eq!(response.id.as_deref(), Some("chatcmpl-1"));
        assert!(
            response
                .blocks
                .iter()
                .any(|block| matches!(block, ContentBlock::Refusal(_)))
        );
        assert_eq!(response.usage.cache_read_tokens.value, Some(4));
        assert_eq!(response.usage.cache_write_tokens.value, Some(3));
        assert_eq!(response.usage.reasoning_tokens.value, Some(2));
        assert_eq!(
            response.usage.input_tokens.source,
            UsageProvenance::Reported
        );
    }

    #[test]
    fn decodes_hermes_reasoning_object_without_treating_it_as_an_extension() {
        let adapter = OpenAiChatAdapter;
        let request = adapter
            .decode_request(
                &json!({
                    "model": "gpt-test",
                    "messages": [{"role": "user", "content": "hello"}],
                    "reasoning": {
                        "enabled": true,
                        "effort": "high",
                        "max_tokens": 512,
                        "future_option": "keep-for-diagnostics"
                    }
                }),
                &context(),
            )
            .unwrap();

        assert_eq!(request.reasoning.mode, ReasoningMode::Enabled);
        assert_eq!(request.reasoning.effort, Some(ReasoningEffort::High));
        assert_eq!(request.reasoning.token_budget, Some(512));
        assert_eq!(request.reasoning.provider_extensions.len(), 1);
        assert_eq!(
            request.reasoning.provider_extensions[0].name,
            "future_option"
        );
        assert!(
            request
                .extensions
                .iter()
                .all(|extension| extension.name != "reasoning")
        );
    }

    #[test]
    fn encodes_only_with_a_matching_executable_plan() {
        let adapter = OpenAiChatAdapter;
        let fixture = fixture();
        let request = adapter
            .decode_request(&fixture["request"], &context())
            .unwrap();
        let plan = plan_conversion(
            &request,
            ProtocolKind::OpenAiChat,
            ProtocolKind::OpenAiChat,
            &context().issuer,
            &context().issuer,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap();
        let encoded = adapter.encode_request(&request, &plan, &context()).unwrap();
        assert_eq!(encoded["messages"][0]["role"], "system");
        assert_eq!(encoded["messages"][1]["role"], "developer");
        assert_eq!(encoded["tools"][0]["function"]["strict"], true);
        assert_eq!(encoded["response_format"]["type"], "json_schema");
        assert_eq!(encoded["x_provider_option"]["mode"], "future");
    }

    #[test]
    fn reencodes_advanced_response_metadata_and_usage() {
        let adapter = OpenAiChatAdapter;
        let response = adapter
            .decode_response(&fixture()["response"], &context())
            .unwrap();
        let request = CanonicalRequestV2::new("chat-test");
        let plan = plan_conversion(
            &request,
            ProtocolKind::OpenAiChat,
            ProtocolKind::OpenAiChat,
            &context().issuer,
            &context().issuer,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap();

        let encoded = adapter
            .encode_response(&response, &plan, &context())
            .unwrap();

        assert_eq!(
            encoded["choices"][0]["message"]["reasoning_content"],
            "reasoning"
        );
        assert_eq!(
            encoded["choices"][0]["message"]["refusal"],
            "no private data"
        );
        assert_eq!(
            encoded["choices"][0]["message"]["annotations"][0]["type"],
            "url_citation"
        );
        assert_eq!(
            encoded["usage"]["prompt_tokens_details"]["cached_tokens"],
            4
        );
        assert_eq!(
            encoded["usage"]["prompt_tokens_details"]["cache_write_tokens"],
            3
        );
        assert_eq!(
            encoded["usage"]["completion_tokens_details"]["reasoning_tokens"],
            2
        );
    }

    #[test]
    fn rejects_missing_model_tool_name_and_wrong_plan() {
        let adapter = OpenAiChatAdapter;
        let missing = adapter.decode_request(&serde_json::json!({"messages":[]}), &context());
        assert_eq!(missing.unwrap_err().code(), "model_missing");
        let bad_tool = adapter.decode_request(
            &serde_json::json!({"model":"m","messages":[{"role":"assistant","tool_calls":[{"id":"x","function":{"name":"","arguments":"{}"}}]}]}),
            &context(),
        );
        assert_eq!(bad_tool.unwrap_err().code(), "tool_name_missing");

        let request = adapter
            .decode_request(
                &serde_json::json!({"model":"m","messages":[{"role":"user","content":"hi"}]}),
                &context(),
            )
            .unwrap();
        let wrong_plan = plan_conversion(
            &request,
            ProtocolKind::OpenAiChat,
            ProtocolKind::AnthropicMessages,
            &context().issuer,
            &context().issuer,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap();
        let error = adapter
            .encode_request(&request, &wrong_plan, &context())
            .unwrap_err();
        assert_eq!(error.code(), "encoder_plan_violation");
    }

    #[test]
    fn decodes_user_metadata_without_creating_an_extension() {
        let request = OpenAiChatAdapter
            .decode_request(
                &serde_json::json!({
                    "model":"m",
                    "messages":[],
                    "user":"account-1"
                }),
                &context(),
            )
            .unwrap();

        assert_eq!(request.metadata.user_id.as_deref(), Some("account-1"));
        assert!(request.extensions.is_empty());
    }

    #[test]
    fn canonicalizes_response_schema_through_the_shared_converter() {
        let request = OpenAiChatAdapter
            .decode_request(
                &serde_json::json!({
                    "model":"m",
                    "messages":[],
                    "response_format":{
                        "type":"json_schema",
                        "json_schema":{
                            "name":"answer",
                            "schema":{
                                "title":"",
                                "type":"object",
                                "properties":{"ok":{"type":"boolean"}},
                                "required":["ok","ok"]
                            }
                        }
                    }
                }),
                &context(),
            )
            .unwrap();
        let ResponseFormat::JsonSchema { schema, .. } = request.response_format else {
            panic!("json schema response format");
        };

        assert!(schema.get("title").is_none());
        assert_eq!(schema["required"], serde_json::json!(["ok"]));
    }

    #[test]
    fn tool_result_without_name_uses_non_null_chat_fallback() {
        let adapter = OpenAiChatAdapter;
        let mut request = CanonicalRequestV2::new("model");
        request.turns.push(Turn {
            id: None,
            role: TurnRole::User,
            blocks: vec![ContentBlock::ToolResult(ToolResult {
                call_id: "call-1".to_string(),
                name: None,
                content: vec![ContentBlock::Text(TextBlock::new("ok"))],
                status: ItemStatus::Completed,
                is_error: false,
                metadata: BlockMetadata::default(),
            })],
            status: None,
        });
        let plan = plan_conversion(
            &request,
            ProtocolKind::OpenAiChat,
            ProtocolKind::OpenAiChat,
            &context().issuer,
            &context().issuer,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap();
        let encoded = adapter.encode_request(&request, &plan, &context()).unwrap();
        assert_eq!(encoded["messages"][0]["name"], "tool");
    }

    #[test]
    fn preserves_user_and_tool_result_block_order_when_encoding() {
        let adapter = OpenAiChatAdapter;
        let mut request = CanonicalRequestV2::new("model");
        request.turns.push(Turn {
            id: None,
            role: TurnRole::User,
            blocks: vec![
                ContentBlock::Text(TextBlock::new("before")),
                ContentBlock::ToolResult(ToolResult {
                    call_id: "call-1".to_string(),
                    name: Some("lookup".to_string()),
                    content: vec![ContentBlock::Text(TextBlock::new("result"))],
                    status: ItemStatus::Completed,
                    is_error: false,
                    metadata: BlockMetadata::default(),
                }),
                ContentBlock::Text(TextBlock::new("after")),
            ],
            status: None,
        });
        let plan = plan_conversion(
            &request,
            ProtocolKind::OpenAiChat,
            ProtocolKind::OpenAiChat,
            &context().issuer,
            &context().issuer,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap();

        let encoded = adapter.encode_request(&request, &plan, &context()).unwrap();

        assert_eq!(encoded["messages"][0]["role"], "user");
        assert_eq!(encoded["messages"][0]["content"], "before");
        assert_eq!(encoded["messages"][1]["role"], "tool");
        assert_eq!(encoded["messages"][2]["role"], "user");
        assert_eq!(encoded["messages"][2]["content"], "after");
    }

    #[test]
    fn flattens_namespaces_only_when_the_plan_authorizes_it() {
        let adapter = OpenAiChatAdapter;
        let mut request = CanonicalRequestV2::new("model");
        request.turns.push(Turn {
            id: None,
            role: TurnRole::User,
            blocks: vec![ContentBlock::Text(TextBlock::new("read"))],
            status: None,
        });
        request.tools.push(ToolDefinition::Namespace(NamespaceTool {
            namespace: "repo".to_string(),
            description: None,
            tools: vec![
                FunctionTool {
                    name: "read".to_string(),
                    description: None,
                    input_schema: serde_json::json!({"type":"object"}),
                    strict: None,
                    cache_policy: None,
                    defer_loading: None,
                    allowed_callers: Vec::new(),
                    input_examples: Vec::new(),
                    eager_input_streaming: None,
                }
                .into(),
            ],
        }));
        let plan = plan_conversion(
            &request,
            ProtocolKind::OpenAiResponses,
            ProtocolKind::OpenAiChat,
            &context().issuer,
            &context().issuer,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap();

        let encoded = adapter.encode_request(&request, &plan, &context()).unwrap();

        assert_eq!(encoded["tools"][0]["function"]["name"], "ns4_repo_read");
    }
}
