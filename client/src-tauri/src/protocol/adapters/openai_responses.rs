use std::collections::HashMap;

use serde_json::{Map, Value, json};

use crate::protocol::adapters::{
    AdapterContext, AdapterError, ProtocolAdapter, affinity_matches_context,
    collect_nested_unknown_fields, continuation_artifact_affinity, decode_openai_prompt_cache,
    decode_prompt_cache_breakpoint, encode_openai_prompt_cache, encode_prompt_cache_breakpoint,
    ensure_encoding_plan, exact_issuer_affinity, link_tool_result_names, protocol_affinity,
};
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

pub(crate) struct OpenAiResponsesAdapter;

impl ProtocolAdapter for OpenAiResponsesAdapter {
    fn decode_request(
        &self,
        body: &Value,
        context: &AdapterContext,
    ) -> Result<CanonicalRequestV2, AdapterError> {
        let model = required_string(body.get("model"), "$.model", "model_missing")?;
        let mut request = CanonicalRequestV2::new(model);
        request.metadata.source_protocol = Some(ProtocolKind::OpenAiResponses);
        request.metadata.user_id = body.get("user").and_then(Value::as_str).map(str::to_string);
        request.metadata.store = body.get("store").and_then(Value::as_bool);
        request.metadata.prompt_cache = decode_openai_prompt_cache(body);
        request.stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
        request.instructions = decode_instructions(body.get("instructions"), context, model)?;
        let (input_instructions, turns) = decode_input(body.get("input"), context, model)?;
        request.instructions.extend(input_instructions);
        request.turns = turns;
        request.tools = decode_tools(body.get("tools"), "$.tools")?;
        for (index, item) in body
            .get("input")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            if item.get("type").and_then(Value::as_str) != Some("additional_tools") {
                continue;
            }
            if item.get("role").and_then(Value::as_str) != Some("developer")
                || !item.get("tools").is_some_and(Value::is_array)
            {
                return Err(AdapterError::new(
                    "additional_tools_invalid",
                    format!("$.input[{index}]"),
                    "additional_tools requires role=developer and a tools array",
                ));
            }
            request
                .metadata
                .responses_tool_placement
                .get_or_insert_with(|| ResponsesToolPlacement {
                    top_level_tools: body
                        .get("tools")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default(),
                    input_positions: Vec::new(),
                })
                .input_positions
                .push(index);
            let path = format!("$.input[{index}].tools");
            merge_additional_tools(
                &mut request.tools,
                decode_tools(item.get("tools"), &path)?,
                &path,
            )?;
        }
        request.tool_choice = decode_tool_choice(body.get("tool_choice"))?;
        request.response_format = decode_response_format(body)?;
        request.reasoning = decode_reasoning_config(body.get("reasoning"), context, model);
        request.generation = decode_generation(body);
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
        _context: &AdapterContext,
    ) -> Result<Value, AdapterError> {
        ensure_encoding_plan(plan, ProtocolKind::OpenAiResponses)?;
        let mut input = encode_instruction_items(&request.instructions, plan)?;
        input.extend(
            encode_turns(&request.turns, plan)?
                .as_array()
                .cloned()
                .unwrap_or_default(),
        );
        if let Some(placement) = &request.metadata.responses_tool_placement {
            // The IR stores developer instructions separately. Put declarations
            // back before their original input positions, including the Lite
            // prefix before the first developer instruction.
            let (declarations, mut messages): (Vec<_>, Vec<_>) =
                input.into_iter().partition(|item| {
                    item.get("type").and_then(Value::as_str) == Some("additional_tools")
                });
            if declarations.len() != placement.input_positions.len() {
                return Err(AdapterError::new(
                    "additional_tools_placement_invalid",
                    "$.input",
                    "tool declaration placement does not match the retained items",
                ));
            }
            for (position, item) in placement.input_positions.iter().zip(declarations) {
                messages.insert((*position).min(messages.len()), item);
            }
            input = messages;
        }
        let mut body = json!({
            "model": request.model,
            "input": input,
            "store": request.metadata.store.unwrap_or(false),
            "stream": request.stream,
        });
        if let Some(placement) = &request.metadata.responses_tool_placement {
            if !placement.top_level_tools.is_empty() {
                body["tools"] = json!(placement.top_level_tools);
            }
        } else if !request.tools.is_empty() {
            body["tools"] = encode_tools(&request.tools)?;
        }
        if !matches!(request.tool_choice, ToolChoice::Auto) {
            body["tool_choice"] = encode_tool_choice(&request.tool_choice, &request.tools)?;
        }
        if !matches!(request.response_format, ResponseFormat::Text)
            || request.generation.verbosity.is_some()
        {
            let mut text = json!({});
            if !matches!(request.response_format, ResponseFormat::Text) {
                text["format"] = encode_response_format(&request.response_format)?;
            }
            if let Some(verbosity) = request.generation.verbosity {
                text["verbosity"] = json!(match verbosity {
                    OutputVerbosity::Low => "low",
                    OutputVerbosity::Medium => "medium",
                    OutputVerbosity::High => "high",
                });
            }
            body["text"] = text;
        }
        encode_reasoning_config(&mut body, &request.reasoning, plan);
        encode_generation(&mut body, &request.generation);
        if let Some(user) = &request.metadata.user_id {
            body["user"] = json!(user);
        }
        encode_openai_prompt_cache(&mut body, &request.metadata.prompt_cache);
        encode_extensions(&mut body, &request.extensions, plan);
        Ok(body)
    }

    fn decode_response(
        &self,
        body: &Value,
        context: &AdapterContext,
    ) -> Result<CanonicalResponseV2, AdapterError> {
        let empty_output = Vec::new();
        let output = match body.get("output").and_then(Value::as_array) {
            Some(output) => output,
            None if body.get("error").is_some_and(Value::is_object) => &empty_output,
            None => {
                return Err(AdapterError::new(
                    "response_output_invalid",
                    "$.output",
                    "response output must be an array",
                ));
            }
        };
        let model = body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let blocks = decode_output_items(output, context, model)?;
        let status = response_status(body.get("status").and_then(Value::as_str));
        let error = decode_error(body.get("error"));
        let finish = FinishDetail {
            reason: response_finish_reason(status, &blocks, error.is_some()),
            original_reason: body
                .pointer("/incomplete_details/reason")
                .and_then(Value::as_str)
                .map(str::to_string),
            incomplete_details: body.get("incomplete_details").cloned(),
        };
        Ok(CanonicalResponseV2 {
            id: body.get("id").and_then(Value::as_str).map(str::to_string),
            model: body
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string),
            blocks,
            status,
            finish,
            usage: decode_usage(body.get("usage")),
            error,
            extensions: decode_response_extensions(body, context, model),
        })
    }

    fn encode_response(
        &self,
        response: &CanonicalResponseV2,
        plan: &ConversionPlan,
        context: &AdapterContext,
    ) -> Result<Value, AdapterError> {
        ensure_encoding_plan(plan, ProtocolKind::OpenAiResponses)?;
        let mut body = json!({
            "id": response.id.clone().unwrap_or_else(|| "resp_const_api".to_string()),
            "object": "response",
            "status": encode_response_status(response.status),
            "model": response.model.clone().unwrap_or_default(),
            "output": encode_response_blocks(
                &response.blocks,
                context,
                response.model.as_deref().unwrap_or(""),
            )?,
            "usage": encode_usage(&response.usage),
        });
        if let Some(details) = &response.finish.incomplete_details {
            body["incomplete_details"] = details.clone();
        }
        if let Some(error) = &response.error {
            body["error"] = json!({
                "code": error.provider_code.as_ref().unwrap_or(&error.code),
                "message": error.message,
                "type": error.code,
                "retryable": error.retryable,
            });
            if let Some(details) = &error.details {
                body["error"]["details"] = details.clone();
            }
        }
        for extension in &response.extensions {
            if extension.namespace == "openai_responses"
                && extension_matches_context(
                    extension,
                    context,
                    response.model.as_deref().unwrap_or(""),
                )
            {
                body[&extension.name] = extension.value.clone();
            }
        }
        Ok(body)
    }
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

fn decode_instructions(
    value: Option<&Value>,
    context: &AdapterContext,
    model: &str,
) -> Result<Vec<Instruction>, AdapterError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let blocks = if let Some(text) = value.as_str() {
        vec![ContentBlock::Text(TextBlock::new(text))]
    } else {
        decode_content(value, context, model, "$.instructions")?
    };
    Ok(vec![Instruction {
        role: InstructionRole::System,
        blocks,
    }])
}

fn decode_input(
    value: Option<&Value>,
    context: &AdapterContext,
    model: &str,
) -> Result<(Vec<Instruction>, Vec<Turn>), AdapterError> {
    let Some(value) = value else {
        return Ok((Vec::new(), Vec::new()));
    };
    if let Some(text) = value.as_str() {
        return Ok((
            Vec::new(),
            vec![Turn {
                id: None,
                role: TurnRole::User,
                blocks: vec![ContentBlock::Text(TextBlock::new(text))],
                status: None,
            }],
        ));
    }
    let items = value.as_array().ok_or_else(|| {
        AdapterError::new(
            "input_invalid",
            "$.input",
            "input must be a string or array",
        )
    })?;
    let mut instructions = Vec::new();
    let mut turns = Vec::new();
    let mut pending_tool_artifacts = HashMap::<String, Vec<OpaqueArtifact>>::new();
    for (index, item) in items.iter().enumerate() {
        let role = item.get("role").and_then(Value::as_str);
        if item
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("message")
            == "message"
            && matches!(role, Some("system" | "developer"))
        {
            instructions.push(Instruction {
                role: if role == Some("developer") {
                    InstructionRole::Developer
                } else {
                    InstructionRole::System
                },
                blocks: decode_content(
                    item.get("content").unwrap_or(&Value::Null),
                    context,
                    model,
                    &format!("$.input[{index}].content"),
                )?,
            });
        } else if let Some(carrier) =
            decode_responses_carrier_item(item, &format!("$.input[{index}].encrypted_content"))?
        {
            validate_carrier_adjacency(&carrier, items, index, "$.input")?;
            let (reasoning_artifacts, previous_artifacts) =
                distribute_carrier_entries(carrier, &mut pending_tool_artifacts);
            let mut blocks = Vec::new();
            if !reasoning_artifacts.is_empty() {
                blocks.push(ContentBlock::Reasoning(
                    decode_reasoning_item_with_artifacts(item, reasoning_artifacts),
                ));
            }
            blocks.extend(
                previous_artifacts
                    .into_iter()
                    .map(ContentBlock::ProviderArtifact),
            );
            if !blocks.is_empty() {
                turns.push(Turn {
                    id: item.get("id").and_then(Value::as_str).map(str::to_string),
                    role: TurnRole::Assistant,
                    blocks,
                    status: item_status(item.get("status").and_then(Value::as_str)),
                });
            }
        } else {
            let mut turn = decode_input_item(item, index, context, model)?;
            attach_pending_tool_artifacts(&mut turn.blocks, &mut pending_tool_artifacts);
            turns.push(turn);
        }
    }
    if let Some(call_id) = pending_tool_artifacts.keys().next() {
        return Err(AdapterError::new(
            "continuation_carrier_binding_invalid",
            "$.input",
            format!("continuation carrier has no adjacent function call {call_id}"),
        ));
    }
    Ok((instructions, turns))
}

fn decode_input_item(
    item: &Value,
    index: usize,
    context: &AdapterContext,
    model: &str,
) -> Result<Turn, AdapterError> {
    let kind = item
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("message");
    let id = item.get("id").and_then(Value::as_str).map(str::to_string);
    let status = item_status(item.get("status").and_then(Value::as_str));
    let (role, blocks) = match kind {
        "message" => {
            let role = match item.get("role").and_then(Value::as_str).unwrap_or("user") {
                "assistant" => TurnRole::Assistant,
                "user" | "system" | "developer" => TurnRole::User,
                role => {
                    return Err(AdapterError::new(
                        "unsupported_item_role",
                        format!("$.input[{index}].role"),
                        format!("unsupported Responses role {role}"),
                    ));
                }
            };
            (
                role,
                decode_content(
                    item.get("content").unwrap_or(&Value::Null),
                    context,
                    model,
                    &format!("$.input[{index}].content"),
                )?,
            )
        }
        "reasoning" => (
            TurnRole::Assistant,
            vec![ContentBlock::Reasoning(decode_reasoning_item(
                item, context, model,
            )?)],
        ),
        "function_call" | "custom_tool_call" => (
            TurnRole::Assistant,
            vec![ContentBlock::ToolCall(decode_tool_call(item, index)?)],
        ),
        "function_call_output" | "custom_tool_call_output" => (
            TurnRole::User,
            vec![ContentBlock::ToolResult(decode_tool_result(
                item, index, context, model,
            )?)],
        ),
        _ => (
            TurnRole::Assistant,
            vec![ContentBlock::ProviderArtifact(provider_artifact(
                item.clone(),
                context,
                model,
            ))],
        ),
    };
    Ok(Turn {
        id,
        role,
        blocks,
        status,
    })
}

fn decode_output_item(
    item: &Value,
    index: usize,
    context: &AdapterContext,
    model: &str,
) -> Result<Vec<ContentBlock>, AdapterError> {
    match item
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("message")
    {
        "message" => decode_content(
            item.get("content").unwrap_or(&Value::Null),
            context,
            model,
            &format!("$.output[{index}].content"),
        ),
        "reasoning" => Ok(vec![ContentBlock::Reasoning(decode_reasoning_item(
            item, context, model,
        )?)]),
        "function_call" | "custom_tool_call" => {
            Ok(vec![ContentBlock::ToolCall(decode_tool_call(item, index)?)])
        }
        "function_call_output" | "custom_tool_call_output" => Ok(vec![ContentBlock::ToolResult(
            decode_tool_result(item, index, context, model)?,
        )]),
        _ => Ok(vec![ContentBlock::ProviderArtifact(provider_artifact(
            item.clone(),
            context,
            model,
        ))]),
    }
}

fn decode_output_items(
    items: &[Value],
    context: &AdapterContext,
    model: &str,
) -> Result<Vec<ContentBlock>, AdapterError> {
    let mut blocks = Vec::new();
    let mut pending_tool_artifacts = HashMap::<String, Vec<OpaqueArtifact>>::new();
    for (index, item) in items.iter().enumerate() {
        if let Some(carrier) =
            decode_responses_carrier_item(item, &format!("$.output[{index}].encrypted_content"))?
        {
            validate_carrier_adjacency(&carrier, items, index, "$.output")?;
            let (reasoning_artifacts, previous_artifacts) =
                distribute_carrier_entries(carrier, &mut pending_tool_artifacts);
            if !reasoning_artifacts.is_empty() {
                blocks.push(ContentBlock::Reasoning(
                    decode_reasoning_item_with_artifacts(item, reasoning_artifacts),
                ));
            }
            blocks.extend(
                previous_artifacts
                    .into_iter()
                    .map(ContentBlock::ProviderArtifact),
            );
            continue;
        }
        let mut decoded = decode_output_item(item, index, context, model)?;
        attach_pending_tool_artifacts(&mut decoded, &mut pending_tool_artifacts);
        blocks.extend(decoded);
    }
    if let Some(call_id) = pending_tool_artifacts.keys().next() {
        return Err(AdapterError::new(
            "continuation_carrier_binding_invalid",
            "$.output",
            format!("continuation carrier has no adjacent function call {call_id}"),
        ));
    }
    Ok(blocks)
}

fn decode_responses_carrier_item(
    item: &Value,
    path: &str,
) -> Result<Option<ContinuationCarrier>, AdapterError> {
    if item.get("type").and_then(Value::as_str) != Some("reasoning") {
        return Ok(None);
    }
    let Some(value) = item.get("encrypted_content") else {
        return Ok(None);
    };
    decode_carrier(value)
        .map_err(|error| AdapterError::new("continuation_carrier_invalid", path, error.message()))
}

fn validate_carrier_adjacency(
    carrier: &ContinuationCarrier,
    items: &[Value],
    index: usize,
    base_path: &str,
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
        .filter(|item| {
            matches!(
                item.get("type").and_then(Value::as_str),
                Some("function_call" | "custom_tool_call")
            )
        })
        .and_then(|item| item.get("call_id").or_else(|| item.get("id")))
        .and_then(Value::as_str);
    if tool_call_ids
        .iter()
        .all(|call_id| Some(*call_id) == next_call_id)
    {
        return Ok(());
    }
    Err(AdapterError::new(
        "continuation_carrier_binding_invalid",
        format!("{base_path}[{index}]"),
        "tool continuation carrier must immediately precede its matching function call",
    ))
}

fn distribute_carrier_entries(
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

fn attach_pending_tool_artifacts(
    blocks: &mut [ContentBlock],
    pending: &mut HashMap<String, Vec<OpaqueArtifact>>,
) {
    for block in blocks {
        let ContentBlock::ToolCall(call) = block else {
            continue;
        };
        if let Some(artifacts) = pending.remove(&call.id) {
            call.artifacts.extend(artifacts);
        }
    }
}

fn decode_content(
    value: &Value,
    context: &AdapterContext,
    model: &str,
    path: &str,
) -> Result<Vec<ContentBlock>, AdapterError> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    if let Some(text) = value.as_str() {
        return Ok(vec![ContentBlock::Text(TextBlock::new(text))]);
    }
    let items = value.as_array().ok_or_else(|| {
        AdapterError::new("content_invalid", path, "content must be a string or array")
    })?;
    let mut blocks = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let metadata = block_metadata(item, index);
        match item
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("input_text")
        {
            "input_text" | "output_text" | "text" | "summary_text" => {
                let text = item.get("text").and_then(Value::as_str).unwrap_or_default();
                blocks.push(ContentBlock::Text(TextBlock {
                    text: text.to_string(),
                    metadata,
                }));
            }
            "refusal" | "output_refusal" => {
                let text = item
                    .get("refusal")
                    .or_else(|| item.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                blocks.push(ContentBlock::Refusal(RefusalBlock {
                    text: text.to_string(),
                    metadata,
                }));
            }
            "input_image" | "output_image" => {
                blocks.push(ContentBlock::Image(decode_image(item, metadata)?));
            }
            "input_file" | "output_file" => {
                blocks.push(ContentBlock::File(decode_file(item, metadata)?));
            }
            "input_audio" | "output_audio" => {
                blocks.push(ContentBlock::Audio(decode_audio(item, metadata)?));
            }
            _ => blocks.push(ContentBlock::ProviderArtifact(provider_artifact(
                item.clone(),
                context,
                model,
            ))),
        }
    }
    Ok(blocks)
}

fn block_metadata(item: &Value, index: usize) -> BlockMetadata {
    BlockMetadata {
        source_item_id: item.get("id").and_then(Value::as_str).map(str::to_string),
        status: item_status(item.get("status").and_then(Value::as_str)),
        annotations: item
            .get("annotations")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        source_location: Some(SourceLocation {
            protocol_path: None,
            item_index: None,
            content_index: Some(index as u32),
        }),
        prompt_cache_breakpoint: decode_prompt_cache_breakpoint(item),
        ..Default::default()
    }
}

fn decode_image(item: &Value, metadata: BlockMetadata) -> Result<MediaBlock, AdapterError> {
    let source = if let Some(url) = item.get("image_url").and_then(Value::as_str) {
        decode_data_url(url).unwrap_or_else(|| MediaSource::RemoteUrl {
            url: url.to_string(),
        })
    } else if let Some(id) = item.get("file_id").and_then(Value::as_str) {
        MediaSource::ProviderFileId {
            provider: "openai".to_string(),
            id: id.to_string(),
            media_type: None,
        }
    } else {
        return Err(AdapterError::new(
            "image_source_missing",
            "$.content.image",
            "image_url or file_id is required",
        ));
    };
    Ok(MediaBlock {
        source,
        detail: match item.get("detail").and_then(Value::as_str) {
            Some("low") => Some(MediaDetail::Low),
            Some("high") => Some(MediaDetail::High),
            Some("original") => Some(MediaDetail::Original),
            Some(_) => Some(MediaDetail::Auto),
            None => None,
        },
        metadata,
    })
}

fn decode_file(item: &Value, metadata: BlockMetadata) -> Result<FileBlock, AdapterError> {
    let source = if let Some(id) = item.get("file_id").and_then(Value::as_str) {
        MediaSource::ProviderFileId {
            provider: "openai".to_string(),
            id: id.to_string(),
            media_type: None,
        }
    } else if let Some(url) = item.get("file_url").and_then(Value::as_str) {
        MediaSource::RemoteUrl {
            url: url.to_string(),
        }
    } else if let Some(data) = item.get("file_data").and_then(Value::as_str) {
        MediaSource::InlineBase64 {
            media_type: item
                .get("media_type")
                .and_then(Value::as_str)
                .unwrap_or("application/octet-stream")
                .to_string(),
            data: data.to_string(),
        }
    } else {
        return Err(AdapterError::new(
            "file_source_missing",
            "$.content.file",
            "file_id, file_url, or file_data is required",
        ));
    };
    Ok(FileBlock {
        source,
        filename: item
            .get("filename")
            .and_then(Value::as_str)
            .map(str::to_string),
        metadata,
    })
}

fn decode_audio(item: &Value, metadata: BlockMetadata) -> Result<MediaBlock, AdapterError> {
    if let Some(audio_url) = item.get("audio_url").and_then(Value::as_str) {
        let source = decode_data_url(audio_url).ok_or_else(|| {
            AdapterError::new(
                "audio_source_invalid",
                "$.content.audio.audio_url",
                "Responses audio_url must be a base64 data URL",
            )
        })?;
        if !matches!(
            &source,
            MediaSource::InlineBase64 { media_type, .. }
                if media_type.to_ascii_lowercase().starts_with("audio/")
        ) {
            return Err(AdapterError::new(
                "audio_source_invalid",
                "$.content.audio.audio_url",
                "Responses audio_url must use an audio media type",
            ));
        }
        return Ok(MediaBlock {
            source,
            detail: None,
            metadata,
        });
    }
    let data = required_string(
        item.get("data"),
        "$.content.audio.data",
        "audio_data_missing",
    )?;
    let format = item.get("format").and_then(Value::as_str).unwrap_or("wav");
    Ok(MediaBlock {
        source: MediaSource::InlineBase64 {
            media_type: format!("audio/{format}"),
            data: data.to_string(),
        },
        detail: None,
        metadata,
    })
}

fn decode_data_url(url: &str) -> Option<MediaSource> {
    let rest = url.strip_prefix("data:")?;
    let (metadata, data) = rest.split_once(',')?;
    Some(MediaSource::InlineBase64 {
        media_type: metadata.strip_suffix(";base64")?.to_string(),
        data: data.to_string(),
    })
}

fn decode_reasoning_item(
    item: &Value,
    context: &AdapterContext,
    model: &str,
) -> Result<ReasoningBlock, AdapterError> {
    let summary = item
        .get("summary")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .map(TextBlock::new)
        .collect::<Vec<_>>();
    let artifacts = match item
        .get("encrypted_content")
        .filter(|value| !value.is_null())
    {
        Some(value) => match decode_carrier(value).map_err(|error| {
            AdapterError::new(
                "continuation_carrier_invalid",
                "$.reasoning.encrypted_content",
                error.message(),
            )
        })? {
            Some(carrier) => {
                if carrier
                    .entries
                    .iter()
                    .any(|entry| !matches!(entry.owner, ContinuationOwner::Reasoning))
                {
                    return Err(AdapterError::new(
                        "continuation_carrier_binding_invalid",
                        "$.reasoning.encrypted_content",
                        "tool continuation carrier was not adjacent to a function call",
                    ));
                }
                carrier
                    .entries
                    .into_iter()
                    .map(|entry| entry.artifact)
                    .collect()
            }
            None => vec![OpaqueArtifact {
                kind: ArtifactKind::OpenAiEncryptedReasoning,
                payload: value.clone(),
                affinity: continuation_artifact_affinity(
                    context,
                    model,
                    ProtocolKind::OpenAiResponses,
                ),
                replay: ReplayPolicy::ExactWhenCompatible,
                criticality: ArtifactCriticality::Supplemental,
            }],
        },
        None => Vec::new(),
    };
    let content_text = item
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("reasoning_text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<String>();
    let encrypted = reasoning_artifacts_are_encrypted(&artifacts);
    Ok(ReasoningBlock {
        // Keep accepting the legacy compatibility shape on input, but always
        // emit the documented reasoning_text content-part shape.
        text: (!content_text.is_empty())
            .then_some(content_text)
            .or_else(|| item.get("text").and_then(Value::as_str).map(str::to_string)),
        summary,
        artifacts,
        encrypted,
        metadata: BlockMetadata {
            source_item_id: item.get("id").and_then(Value::as_str).map(str::to_string),
            status: item_status(item.get("status").and_then(Value::as_str)),
            ..Default::default()
        },
    })
}

fn decode_reasoning_item_with_artifacts(
    item: &Value,
    artifacts: Vec<OpaqueArtifact>,
) -> ReasoningBlock {
    let summary = item
        .get("summary")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .map(TextBlock::new)
        .collect::<Vec<_>>();
    let content_text = item
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("reasoning_text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<String>();
    let encrypted = reasoning_artifacts_are_encrypted(&artifacts);
    ReasoningBlock {
        text: (!content_text.is_empty())
            .then_some(content_text)
            .or_else(|| item.get("text").and_then(Value::as_str).map(str::to_string)),
        summary,
        artifacts,
        encrypted,
        metadata: BlockMetadata {
            source_item_id: item.get("id").and_then(Value::as_str).map(str::to_string),
            status: item_status(item.get("status").and_then(Value::as_str)),
            ..Default::default()
        },
    }
}

fn decode_tool_call(item: &Value, index: usize) -> Result<ToolCall, AdapterError> {
    let kind = item
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("function_call");
    let name = required_string(
        item.get("name"),
        &format!("$.input[{index}].name"),
        "tool_name_missing",
    )?;
    let id = item
        .get("call_id")
        .or_else(|| item.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| {
            AdapterError::new(
                "tool_call_id_missing",
                format!("$.input[{index}].call_id"),
                "tool call ID is required",
            )
        })?;
    let raw_arguments = item
        .get(if kind == "custom_tool_call" {
            "input"
        } else {
            "arguments"
        })
        .and_then(Value::as_str)
        .map(str::to_string);
    let arguments = raw_arguments
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .or_else(|| {
            item.get("arguments")
                .filter(|value| !value.is_string())
                .cloned()
        });
    Ok(ToolCall {
        id: id.to_string(),
        source_item_id: item.get("id").and_then(Value::as_str).map(str::to_string),
        kind: if kind == "custom_tool_call" {
            ToolKind::Custom
        } else {
            ToolKind::Function
        },
        name: name.to_string(),
        arguments,
        raw_arguments,
        status: item_status(item.get("status").and_then(Value::as_str))
            .unwrap_or(ItemStatus::Completed),
        artifacts: Vec::new(),
        metadata: BlockMetadata {
            tool_namespace: item
                .get("namespace")
                .and_then(Value::as_str)
                .map(str::to_string),
            ..Default::default()
        },
    })
}

fn decode_tool_result(
    item: &Value,
    index: usize,
    context: &AdapterContext,
    model: &str,
) -> Result<ToolResult, AdapterError> {
    let call_id = required_string(
        item.get("call_id"),
        &format!("$.input[{index}].call_id"),
        "tool_call_id_missing",
    )?;
    Ok(ToolResult {
        call_id: call_id.to_string(),
        name: item.get("name").and_then(Value::as_str).map(str::to_string),
        content: decode_content(
            item.get("output").unwrap_or(&Value::Null),
            context,
            model,
            &format!("$.input[{index}].output"),
        )?,
        status: item_status(item.get("status").and_then(Value::as_str))
            .unwrap_or(ItemStatus::Completed),
        is_error: item
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        metadata: BlockMetadata {
            source_item_id: item.get("id").and_then(Value::as_str).map(str::to_string),
            ..Default::default()
        },
    })
}

fn decode_tools(value: Option<&Value>, path: &str) -> Result<Vec<ToolDefinition>, AdapterError> {
    let mut output = Vec::new();
    for (index, tool) in value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let kind = tool
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("function");
        output.push(match kind {
            "function" => {
                ToolDefinition::Function(decode_function_tool(tool, &format!("{path}[{index}]"))?)
            }
            "custom" => {
                ToolDefinition::Custom(decode_custom_tool(tool, &format!("{path}[{index}]"))?)
            }
            "namespace" => {
                let nested = tool
                    .get("tools")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .enumerate()
                    .map(|(nested_index, nested)| {
                        let path = format!("{path}[{index}].tools[{nested_index}]");
                        match nested
                            .get("type")
                            .and_then(Value::as_str)
                            .unwrap_or("function")
                        {
                            "function" => decode_function_tool(nested, &path)
                                .map(NamespaceToolDefinition::Function),
                            "custom" => decode_custom_tool(nested, &path)
                                .map(NamespaceToolDefinition::Custom),
                            _ => Err(AdapterError::new(
                                "namespace_tool_type_unsupported",
                                path,
                                "namespace tools must be function or custom definitions",
                            )),
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                ToolDefinition::Namespace(NamespaceTool {
                    namespace: required_string(
                        tool.get("name"),
                        &format!("{path}[{index}].name"),
                        "tool_name_missing",
                    )?
                    .to_string(),
                    description: tool
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    tools: nested,
                })
            }
            _ => ToolDefinition::Hosted(HostedTool {
                kind: hosted_tool_kind(kind),
                provider: Some("openai".to_string()),
                name: tool
                    .get("name")
                    .or_else(|| tool.get("server_label"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                config: tool.clone(),
            }),
        });
    }
    Ok(output)
}

fn decode_custom_tool(tool: &Value, path: &str) -> Result<CustomTool, AdapterError> {
    Ok(CustomTool {
        name: required_string(
            tool.get("name"),
            &format!("{path}.name"),
            "tool_name_missing",
        )?
        .to_string(),
        description: tool
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_string),
        grammar: tool.get("format").cloned().unwrap_or_else(|| json!({})),
    })
}

// Stable first-seen order keeps the tool prefix cacheable. Identical repeated
// declarations are harmless; contradictory definitions cannot be flattened
// without silently changing which tool the caller meant.
fn merge_additional_tools(
    tools: &mut Vec<ToolDefinition>,
    additions: Vec<ToolDefinition>,
    path: &str,
) -> Result<(), AdapterError> {
    fn key(tool: &ToolDefinition) -> String {
        match tool {
            ToolDefinition::Function(tool) => format!("tool:{}", tool.name),
            ToolDefinition::Custom(tool) => format!("tool:{}", tool.name),
            ToolDefinition::Namespace(tool) => format!("namespace:{}", tool.namespace),
            ToolDefinition::Hosted(tool) => format!(
                "hosted:{}:{}",
                tool.config["type"],
                tool.name.as_deref().unwrap_or_default()
            ),
        }
    }
    let mut indices: HashMap<String, usize> = tools
        .iter()
        .enumerate()
        .map(|(index, tool)| (key(tool), index))
        .collect();
    for (index, addition) in additions.into_iter().enumerate() {
        let name = key(&addition);
        let Some(&existing_index) = indices.get(&name) else {
            indices.insert(name, tools.len());
            tools.push(addition);
            continue;
        };
        let existing = &mut tools[existing_index];
        if *existing == addition {
            continue;
        }
        if let (ToolDefinition::Namespace(existing), ToolDefinition::Namespace(addition)) =
            (&mut *existing, &addition)
        {
            let mut children: HashMap<String, usize> = existing
                .tools
                .iter()
                .enumerate()
                .map(|(index, child)| (child.name().to_string(), index))
                .collect();
            for child in &addition.tools {
                if let Some(&child_index) = children.get(child.name()) {
                    let previous = &existing.tools[child_index];
                    if previous != child {
                        return Err(AdapterError::new(
                            "tool_definition_conflict",
                            format!("{path}[{index}]"),
                            format!(
                                "conflicting definition for {}.{}",
                                existing.namespace,
                                child.name()
                            ),
                        ));
                    }
                } else {
                    children.insert(child.name().to_string(), existing.tools.len());
                    existing.tools.push(child.clone());
                }
            }
            continue;
        }
        return Err(AdapterError::new(
            "tool_definition_conflict",
            format!("{path}[{index}]"),
            format!("conflicting definition for {name}"),
        ));
    }
    Ok(())
}

fn decode_function_tool(tool: &Value, path: &str) -> Result<FunctionTool, AdapterError> {
    let schema = canonicalize_tool_schema(
        tool.get("parameters")
            .or_else(|| tool.get("input_schema"))
            .unwrap_or(&Value::Object(Map::new())),
    )
    .map_err(|error| {
        AdapterError::new(
            error.code(),
            format!("{path}.parameters"),
            error.to_string(),
        )
    })?;
    Ok(FunctionTool {
        name: required_string(
            tool.get("name"),
            &format!("{path}.name"),
            "tool_name_missing",
        )?
        .to_string(),
        description: tool
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_string),
        input_schema: schema,
        strict: tool.get("strict").and_then(Value::as_bool),
        cache_policy: None,
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
    })
}

fn hosted_tool_kind(kind: &str) -> HostedToolKind {
    match kind {
        "web_search" | "web_search_preview" => HostedToolKind::WebSearch,
        "file_search" => HostedToolKind::FileSearch,
        "code_interpreter" | "code_execution" => HostedToolKind::CodeExecution,
        "computer_use" | "computer_use_preview" => HostedToolKind::ComputerUse,
        "mcp" => HostedToolKind::Mcp,
        _ => HostedToolKind::ProviderSpecific,
    }
}

fn decode_tool_choice(value: Option<&Value>) -> Result<ToolChoice, AdapterError> {
    let Some(value) = value else {
        return Ok(ToolChoice::Auto);
    };
    if let Some(mode) = value.as_str() {
        return Ok(match mode {
            "auto" => ToolChoice::Auto,
            "none" => ToolChoice::None,
            "required" => ToolChoice::Required,
            other => ToolChoice::Named {
                name: other.to_string(),
            },
        });
    }
    let kind = value.get("type").and_then(Value::as_str).unwrap_or("auto");
    if kind == "allowed_tools" {
        let names = value
            .get("tools")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|tool| {
                let name = tool.get("name").and_then(Value::as_str)?;
                Some(tool.get("namespace").and_then(Value::as_str).map_or_else(
                    || name.to_string(),
                    |namespace| format!("{namespace}.{name}"),
                ))
            })
            .collect();
        return Ok(ToolChoice::Allowed {
            mode: if value.get("mode").and_then(Value::as_str) == Some("required") {
                AllowedMode::Required
            } else {
                AllowedMode::Auto
            },
            names,
        });
    }
    let name = required_string(value.get("name"), "$.tool_choice.name", "tool_name_missing")?;
    Ok(ToolChoice::Named {
        name: value.get("namespace").and_then(Value::as_str).map_or_else(
            || name.to_string(),
            |namespace| format!("{namespace}.{name}"),
        ),
    })
}

fn decode_response_format(body: &Value) -> Result<ResponseFormat, AdapterError> {
    let Some(format) = body
        .pointer("/text/format")
        .or_else(|| body.get("response_format"))
    else {
        return Ok(ResponseFormat::Text);
    };
    match format.get("type").and_then(Value::as_str).unwrap_or("text") {
        "text" => Ok(ResponseFormat::Text),
        "json_object" => Ok(ResponseFormat::JsonObject),
        "json_schema" => {
            let empty_schema = Value::Object(Map::new());
            let schema = canonicalize_schema(format.get("schema").unwrap_or(&empty_schema))
                .map_err(|error| {
                    AdapterError::new(error.code(), "$.text.format.schema", error.to_string())
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
                strict: format.get("strict").and_then(Value::as_bool),
            })
        }
        kind => Err(AdapterError::new(
            "unsupported_response_format",
            "$.text.format.type",
            format!("unsupported Responses format {kind}"),
        )),
    }
}

fn decode_reasoning_config(
    value: Option<&Value>,
    context: &AdapterContext,
    model: &str,
) -> ReasoningConfig {
    let Some(value) = value else {
        return ReasoningConfig::default();
    };
    let recognized = ["effort", "summary", "generate_summary", "max_tokens"];
    let effort = value
        .get("effort")
        .and_then(Value::as_str)
        .and_then(ReasoningEffort::parse);
    let token_budget = value.get("max_tokens").and_then(Value::as_u64);
    let summary = match value
        .get("summary")
        .or_else(|| value.get("generate_summary"))
        .and_then(Value::as_str)
    {
        Some("auto") => ReasoningSummaryMode::Auto,
        Some("concise") => ReasoningSummaryMode::Concise,
        Some("detailed") => ReasoningSummaryMode::Detailed,
        _ => ReasoningSummaryMode::None,
    };
    let mode = if effort == Some(ReasoningEffort::None) {
        ReasoningMode::Disabled
    } else if token_budget.is_some() {
        ReasoningMode::Enabled
    } else if effort.is_some() || !matches!(summary, ReasoningSummaryMode::None) {
        ReasoningMode::Automatic
    } else {
        ReasoningMode::Disabled
    };
    ReasoningConfig {
        mode,
        effort,
        token_budget,
        summary,
        provider_extensions: value
            .as_object()
            .into_iter()
            .flat_map(|object| object.iter())
            .filter(|(name, _)| !recognized.contains(&name.as_str()))
            .map(|(name, value)| provider_extension(name, value, context, model, false))
            .collect(),
    }
}

fn decode_generation(body: &Value) -> GenerationConfig {
    GenerationConfig {
        max_output_tokens: body.get("max_output_tokens").and_then(Value::as_u64),
        temperature: body.get("temperature").and_then(Value::as_f64),
        top_p: body.get("top_p").and_then(Value::as_f64),
        frequency_penalty: body.get("frequency_penalty").and_then(Value::as_f64),
        presence_penalty: body.get("presence_penalty").and_then(Value::as_f64),
        seed: body.get("seed").and_then(Value::as_i64),
        verbosity: match body.pointer("/text/verbosity").and_then(Value::as_str) {
            Some("low") => Some(OutputVerbosity::Low),
            Some("medium") => Some(OutputVerbosity::Medium),
            Some("high") => Some(OutputVerbosity::High),
            _ => None,
        },
        service_tier: body
            .get("service_tier")
            .and_then(Value::as_str)
            .map(str::to_string),
        parallel_tool_calls: body.get("parallel_tool_calls").and_then(Value::as_bool),
        modalities: body
            .get("modalities")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(|kind| match kind {
                "text" => Some(OutputModality::Text),
                "audio" => Some(OutputModality::Audio),
                "image" => Some(OutputModality::Image),
                "video" => Some(OutputModality::Video),
                _ => None,
            })
            .collect(),
        audio: body.get("audio").map(|audio| AudioOutputConfig {
            voice: audio
                .get("voice")
                .and_then(Value::as_str)
                .map(str::to_string),
            format: audio
                .get("format")
                .and_then(Value::as_str)
                .map(str::to_string),
            sample_rate_hz: audio
                .get("sample_rate_hz")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok()),
        }),
        ..Default::default()
    }
}

fn decode_extensions(
    body: &Value,
    context: &AdapterContext,
    model: &str,
) -> Vec<ProviderExtension> {
    let recognized = [
        "model",
        "instructions",
        "input",
        "tools",
        "tool_choice",
        "text",
        "response_format",
        "reasoning",
        "max_output_tokens",
        "temperature",
        "top_p",
        "frequency_penalty",
        "presence_penalty",
        "seed",
        "service_tier",
        "parallel_tool_calls",
        "modalities",
        "audio",
        "store",
        "stream",
        "user",
        "prompt_cache_key",
        "prompt_cache_options",
        "prompt_cache_retention",
    ];
    body.as_object()
        .into_iter()
        .flat_map(|object| object.iter())
        .filter(|(name, _)| !recognized.contains(&name.as_str()))
        .map(|(name, value)| {
            provider_extension(name, value, context, model, name == "previous_response_id")
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
        "object",
        "status",
        "model",
        "output",
        "usage",
        "error",
        "incomplete_details",
    ];
    let mut extensions = body
        .as_object()
        .into_iter()
        .flat_map(|object| object.iter())
        .filter(|(name, _)| !recognized.contains(&name.as_str()))
        .map(|(name, value)| provider_extension(name, value, context, model, false))
        .collect::<Vec<_>>();
    extensions.extend(decode_response_nested_extensions(body, context, model));
    extensions
}

fn decode_response_nested_extensions(
    body: &Value,
    context: &AdapterContext,
    model: &str,
) -> Vec<ProviderExtension> {
    let mut extensions = Vec::new();
    for (item_index, item) in body
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let item_path = format!("$.output[{item_index}]");
        let kind = item
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("message");
        let recognized: Option<&[&str]> = match kind {
            "message" => Some(&["type", "id", "status", "role", "content"]),
            "reasoning" => Some(&[
                "type",
                "id",
                "status",
                "summary",
                "encrypted_content",
                "text",
            ]),
            "function_call" => Some(&[
                "type",
                "id",
                "status",
                "call_id",
                "name",
                "namespace",
                "arguments",
            ]),
            "custom_tool_call" => Some(&[
                "type",
                "id",
                "status",
                "call_id",
                "name",
                "namespace",
                "input",
            ]),
            "function_call_output" | "custom_tool_call_output" => {
                Some(&["type", "id", "status", "call_id", "name", "output"])
            }
            // Unknown output items are retained as ProviderArtifact and are audited by the
            // response artifact policy; scanning them again would produce duplicate warnings.
            _ => None,
        };
        let Some(recognized) = recognized else {
            continue;
        };
        collect_nested_unknown_fields(
            &mut extensions,
            "openai_responses",
            Some(item),
            recognized,
            &item_path,
            context,
            model,
        );
        if kind == "message" {
            scan_responses_content(
                &mut extensions,
                item.get("content"),
                &format!("{item_path}.content"),
                context,
                model,
            );
        } else if matches!(kind, "function_call_output" | "custom_tool_call_output") {
            scan_responses_content(
                &mut extensions,
                item.get("output"),
                &format!("{item_path}.output"),
                context,
                model,
            );
        } else if kind == "reasoning" {
            for (summary_index, summary) in item
                .get("summary")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
            {
                collect_nested_unknown_fields(
                    &mut extensions,
                    "openai_responses",
                    Some(summary),
                    &["type", "text"],
                    &format!("{item_path}.summary[{summary_index}]"),
                    context,
                    model,
                );
            }
        }
    }
    scan_responses_usage_extensions(
        &mut extensions,
        body.get("usage"),
        "$.usage",
        context,
        model,
    );
    collect_nested_unknown_fields(
        &mut extensions,
        "openai_responses",
        body.get("error"),
        &["code", "type", "message", "retryable", "details"],
        "$.error",
        context,
        model,
    );
    extensions
}

fn scan_responses_content(
    extensions: &mut Vec<ProviderExtension>,
    value: Option<&Value>,
    path: &str,
    context: &AdapterContext,
    model: &str,
) {
    for (content_index, content) in value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        collect_nested_unknown_fields(
            extensions,
            "openai_responses",
            Some(content),
            &[
                "type",
                "id",
                "status",
                "annotations",
                "text",
                "refusal",
                "image_url",
                "file_id",
                "detail",
                "file_url",
                "file_data",
                "media_type",
                "filename",
                "audio_url",
                "audio_data",
                "data",
                "format",
                "prompt_cache_breakpoint",
            ],
            &format!("{path}[{content_index}]"),
            context,
            model,
        );
    }
}

fn scan_responses_usage_extensions(
    extensions: &mut Vec<ProviderExtension>,
    value: Option<&Value>,
    path: &str,
    context: &AdapterContext,
    model: &str,
) {
    collect_nested_unknown_fields(
        extensions,
        "openai_responses",
        value,
        &[
            "input_tokens",
            "output_tokens",
            "total_tokens",
            "input_tokens_details",
            "output_tokens_details",
        ],
        path,
        context,
        model,
    );
    collect_nested_unknown_fields(
        extensions,
        "openai_responses",
        value.and_then(|usage| usage.get("input_tokens_details")),
        &["cached_tokens", "cache_write_tokens", "audio_tokens"],
        &format!("{path}.input_tokens_details"),
        context,
        model,
    );
    collect_nested_unknown_fields(
        extensions,
        "openai_responses",
        value.and_then(|usage| usage.get("output_tokens_details")),
        &["reasoning_tokens", "audio_tokens"],
        &format!("{path}.output_tokens_details"),
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
    for (item_index, item) in body
        .get("input")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let item_path = format!("$.input[{item_index}]");
        collect_nested_unknown_fields(
            &mut extensions,
            "openai_responses",
            Some(item),
            if item.get("type").and_then(Value::as_str) == Some("additional_tools") {
                &["type", "id", "role", "tools"]
            } else {
                &[
                    "type",
                    "id",
                    "status",
                    "role",
                    "content",
                    "summary",
                    "encrypted_content",
                    "call_id",
                    "name",
                    "namespace",
                    "arguments",
                    "input",
                    "output",
                ]
            },
            &item_path,
            context,
            model,
        );
        for (content_index, content) in item
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            collect_nested_unknown_fields(
                &mut extensions,
                "openai_responses",
                Some(content),
                &[
                    "type",
                    "id",
                    "status",
                    "annotations",
                    "text",
                    "refusal",
                    "image_url",
                    "file_id",
                    "detail",
                    "file_url",
                    "file_data",
                    "media_type",
                    "filename",
                    "audio_url",
                    "audio_data",
                    "format",
                    "prompt_cache_breakpoint",
                ],
                &format!("{item_path}.content[{content_index}]"),
                context,
                model,
            );
        }
    }
    let tool_lists = std::iter::once(("$.tools".to_string(), body.get("tools"))).chain(
        body.get("input")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
            .filter(|(_, item)| {
                item.get("type").and_then(Value::as_str) == Some("additional_tools")
            })
            .map(|(index, item)| (format!("$.input[{index}].tools"), item.get("tools"))),
    );
    for (tools_path, value) in tool_lists {
        for (tool_index, tool) in value
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let kind = tool
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("function");
            if matches!(kind, "function" | "custom" | "namespace") {
                collect_nested_unknown_fields(
                    &mut extensions,
                    "openai_responses",
                    Some(tool),
                    &[
                        "type",
                        "name",
                        "description",
                        "parameters",
                        "input_schema",
                        "strict",
                        "format",
                        "tools",
                        "defer_loading",
                        "allowed_callers",
                        "input_examples",
                        "eager_input_streaming",
                    ],
                    &format!("{tools_path}[{tool_index}]"),
                    context,
                    model,
                );
            }
            for (nested_index, nested) in tool
                .get("tools")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
            {
                collect_nested_unknown_fields(
                    &mut extensions,
                    "openai_responses",
                    Some(nested),
                    &[
                        "type",
                        "name",
                        "description",
                        "parameters",
                        "input_schema",
                        "strict",
                        "format",
                        "defer_loading",
                        "allowed_callers",
                        "input_examples",
                        "eager_input_streaming",
                    ],
                    &format!("{tools_path}[{tool_index}].tools[{nested_index}]"),
                    context,
                    model,
                );
            }
        }
    }
    collect_nested_unknown_fields(
        &mut extensions,
        "openai_responses",
        body.get("tool_choice"),
        &["type", "name", "mode", "tools", "server_label"],
        "$.tool_choice",
        context,
        model,
    );
    collect_nested_unknown_fields(
        &mut extensions,
        "openai_responses",
        body.get("text"),
        &["format", "verbosity"],
        "$.text",
        context,
        model,
    );
    collect_nested_unknown_fields(
        &mut extensions,
        "openai_responses",
        body.pointer("/text/format")
            .or_else(|| body.get("response_format")),
        &["type", "name", "description", "schema", "strict"],
        "$.text.format",
        context,
        model,
    );
    collect_nested_unknown_fields(
        &mut extensions,
        "openai_responses",
        body.get("reasoning"),
        &["effort", "summary", "generate_summary", "max_tokens"],
        "$.reasoning",
        context,
        model,
    );
    collect_nested_unknown_fields(
        &mut extensions,
        "openai_responses",
        body.get("audio"),
        &["voice", "format", "sample_rate_hz"],
        "$.audio",
        context,
        model,
    );
    collect_nested_unknown_fields(
        &mut extensions,
        "openai_responses",
        body.get("prompt_cache_options"),
        &["mode", "ttl"],
        "$.prompt_cache_options",
        context,
        model,
    );
    extensions
}

fn provider_extension(
    name: &str,
    value: &Value,
    context: &AdapterContext,
    model: &str,
    critical: bool,
) -> ProviderExtension {
    ProviderExtension {
        namespace: "openai_responses".to_string(),
        name: name.to_string(),
        value: value.clone(),
        affinity: if critical {
            exact_issuer_affinity(context, model)
        } else {
            protocol_affinity(ProtocolKind::OpenAiResponses)
        },
        criticality: if critical {
            ExtensionCriticality::Critical
        } else {
            ExtensionCriticality::Advisory
        },
    }
}

fn provider_artifact(value: Value, _context: &AdapterContext, _model: &str) -> OpaqueArtifact {
    OpaqueArtifact {
        kind: ArtifactKind::ProviderSpecific,
        payload: value,
        affinity: protocol_affinity(ProtocolKind::OpenAiResponses),
        replay: ReplayPolicy::ExactWhenCompatible,
        criticality: ArtifactCriticality::Supplemental,
    }
}

fn encode_instruction_items(
    instructions: &[Instruction],
    plan: &ConversionPlan,
) -> Result<Vec<Value>, AdapterError> {
    let mut items = Vec::new();
    for (instruction_index, instruction) in instructions.iter().enumerate() {
        let content = encode_content(
            &instruction.blocks,
            plan,
            &format!("instructions[{instruction_index}]"),
            false,
        )?;
        items.push(json!({
            "type":"message",
            "role":if instruction.role == InstructionRole::Developer {
                "developer"
            } else {
                "system"
            },
            "content":content,
        }));
    }
    Ok(items)
}

fn encode_turns(turns: &[Turn], plan: &ConversionPlan) -> Result<Value, AdapterError> {
    let call_kinds = turns
        .iter()
        .flat_map(|turn| turn.blocks.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolCall(call) => Some((call.id.as_str(), call.kind)),
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    let mut output = Vec::new();
    for (turn_index, turn) in turns.iter().enumerate() {
        let mut message_blocks = Vec::new();
        for (block_index, block) in turn.blocks.iter().enumerate() {
            match block {
                ContentBlock::ToolCall(call) => {
                    flush_message(turn, &mut message_blocks, &mut output, plan, turn_index)?;
                    output.push(encode_tool_call_item(call));
                }
                ContentBlock::ToolResult(result) => {
                    flush_message(turn, &mut message_blocks, &mut output, plan, turn_index)?;
                    output.push(encode_tool_result_item(
                        result,
                        call_kinds.get(result.call_id.as_str()).copied(),
                        plan,
                        turn_index,
                        block_index,
                    )?);
                }
                ContentBlock::Reasoning(reasoning) => {
                    let path = format!("turns[{turn_index}].blocks[{block_index}]");
                    if plan.omits_feature_at(Feature::Reasoning, &path) {
                        continue;
                    }
                    flush_message(turn, &mut message_blocks, &mut output, plan, turn_index)?;
                    output.push(encode_reasoning_item(reasoning, plan, &path));
                }
                ContentBlock::ProviderArtifact(artifact) => {
                    flush_message(turn, &mut message_blocks, &mut output, plan, turn_index)?;
                    if artifact_is_preserved(
                        plan,
                        &format!("turns[{turn_index}].blocks[{block_index}]"),
                    ) {
                        output.push(artifact.payload.clone());
                    }
                }
                block => message_blocks.push(block.clone()),
            }
        }
        flush_message(turn, &mut message_blocks, &mut output, plan, turn_index)?;
    }
    Ok(Value::Array(output))
}

fn flush_message(
    turn: &Turn,
    blocks: &mut Vec<ContentBlock>,
    output: &mut Vec<Value>,
    plan: &ConversionPlan,
    turn_index: usize,
) -> Result<(), AdapterError> {
    if blocks.is_empty() {
        return Ok(());
    }
    let content = encode_content(
        blocks,
        plan,
        &format!("turns[{turn_index}]"),
        turn.role == TurnRole::Assistant,
    )?;
    let mut message = json!({
        "type":"message",
        "role":if turn.role == TurnRole::Assistant { "assistant" } else { "user" },
        "content":content,
    });
    if let Some(id) = turn.id.as_deref().filter(|id| !id.trim().is_empty()) {
        message["id"] = json!(id);
    }
    if let Some(status) = turn.status {
        message["status"] = json!(encode_item_status(status));
    }
    output.push(message);
    blocks.clear();
    Ok(())
}

fn encode_content(
    blocks: &[ContentBlock],
    plan: &ConversionPlan,
    base_path: &str,
    output: bool,
) -> Result<Vec<Value>, AdapterError> {
    let mut content = Vec::new();
    for (index, block) in blocks.iter().enumerate() {
        let path = format!("{base_path}.blocks[{index}]");
        match block {
            ContentBlock::Text(text) => {
                let mut item = json!({
                    "type":if output { "output_text" } else { "input_text" },
                    "text":text.text,
                });
                if !output {
                    encode_prompt_cache_breakpoint(&mut item, &text.metadata);
                } else if text.metadata.prompt_cache_breakpoint.is_some() {
                    return Err(AdapterError::new(
                        "unsupported_prompt_cache_breakpoint",
                        path,
                        "Responses only accepts cache breakpoints on input content blocks",
                    ));
                }
                if output && !text.metadata.annotations.is_empty() {
                    item["annotations"] = json!(text.metadata.annotations);
                }
                content.push(item);
            }
            ContentBlock::Refusal(refusal) => {
                content.push(json!({"type":"refusal","refusal":refusal.text}))
            }
            ContentBlock::Image(image) => content.push(encode_image(image)?),
            ContentBlock::File(file) => content.push(encode_file(file)?),
            ContentBlock::Audio(audio) => content.push(encode_audio(audio)?),
            ContentBlock::ProviderArtifact(artifact) if artifact_is_preserved(plan, &path) => {
                content.push(artifact.payload.clone())
            }
            ContentBlock::ProviderArtifact(_) => {}
            ContentBlock::Reasoning(_) if plan.omits_feature_at(Feature::Reasoning, &path) => {}
            _ => {
                return Err(AdapterError::new(
                    "encoder_plan_violation",
                    path,
                    "plan did not authorize this Responses content mapping",
                ));
            }
        }
    }
    Ok(content)
}

fn encode_reasoning_item(reasoning: &ReasoningBlock, plan: &ConversionPlan, path: &str) -> Value {
    let mut item = json!({
        "type":"reasoning",
        "id":reasoning.metadata.source_item_id,
        "status":reasoning.metadata.status.map(encode_item_status),
        "summary":reasoning.summary.iter().map(|text| json!({"type":"summary_text","text":text.text})).collect::<Vec<_>>(),
    });
    if let Some(text) = &reasoning.text {
        item["content"] = json!([{"type":"reasoning_text","text":text}]);
    }
    for (index, artifact) in reasoning.artifacts.iter().enumerate() {
        if artifact.kind == ArtifactKind::OpenAiEncryptedReasoning
            && artifact_is_preserved(plan, &format!("{path}.artifacts[{index}]"))
        {
            item["encrypted_content"] = artifact.payload.clone();
        }
    }
    item
}

fn encode_tool_call_item(call: &ToolCall) -> Value {
    let mut item = json!({
        "type":if call.kind == ToolKind::Custom { "custom_tool_call" } else { "function_call" },
        "id":call.source_item_id,
        "status":encode_item_status(call.status),
        "call_id":call.id,
        "name":call.name,
    });
    if let Some(namespace) = &call.metadata.tool_namespace {
        item["namespace"] = json!(namespace);
    }
    let arguments = call.raw_arguments.clone().unwrap_or_else(|| {
        serde_json::to_string(
            call.arguments
                .as_ref()
                .unwrap_or(&Value::Object(Map::new())),
        )
        .unwrap_or_else(|_| "{}".to_string())
    });
    if call.kind == ToolKind::Custom {
        item["input"] = json!(arguments);
    } else {
        item["arguments"] = json!(arguments);
    }
    item
}

fn encode_tool_result_item(
    result: &ToolResult,
    call_kind: Option<ToolKind>,
    plan: &ConversionPlan,
    turn_index: usize,
    block_index: usize,
) -> Result<Value, AdapterError> {
    let output = encode_content(
        &result.content,
        plan,
        &format!("turns[{turn_index}].blocks[{block_index}].content"),
        false,
    )?;
    let output = if output.len() == 1
        && output[0].get("type").and_then(Value::as_str) == Some("input_text")
    {
        output[0].get("text").cloned().unwrap_or(Value::Null)
    } else {
        Value::Array(output)
    };
    Ok(json!({
        "type":if call_kind == Some(ToolKind::Custom) {
            "custom_tool_call_output"
        } else {
            "function_call_output"
        },
        "id":result.metadata.source_item_id,
        "status":encode_item_status(result.status),
        "call_id":result.call_id,
        "output":output,
    }))
}

fn artifact_is_preserved(plan: &ConversionPlan, path: &str) -> bool {
    plan.artifact_actions()
        .iter()
        .any(|action| action.path == path && action.action == ArtifactDisposition::Preserve)
}

fn encode_tools(tools: &[ToolDefinition]) -> Result<Value, AdapterError> {
    tools
        .iter()
        .map(|tool| match tool {
            ToolDefinition::Function(tool) => {
                let mut value = json!({
                    "type":"function","name":tool.name,"description":tool.description,
                    "parameters":tool.input_schema,
                });
                if let Some(strict) = tool.strict {
                    value["strict"] = json!(strict);
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
            ToolDefinition::Custom(tool) => Ok(json!({
                "type":"custom","name":tool.name,"description":tool.description,"format":tool.grammar,
            })),
            ToolDefinition::Namespace(namespace) => {
                let definitions = namespace.tools.iter().map(|tool| match tool {
                    NamespaceToolDefinition::Function(tool) => ToolDefinition::Function(tool.clone()),
                    NamespaceToolDefinition::Custom(tool) => ToolDefinition::Custom(tool.clone()),
                }).collect::<Vec<_>>();
                let tools = encode_tools(&definitions)?;
                Ok(json!({
                    "type":"namespace","name":namespace.namespace,
                    "description":namespace.description,"tools":tools,
                }))
            }
            ToolDefinition::Hosted(tool) => {
                let mut config = tool.config.clone();
                if !config.is_object() {
                    config = json!({"config":config});
                }
                if config.get("type").is_none() {
                    config["type"] = json!(match tool.kind {
                        HostedToolKind::WebSearch => "web_search_preview",
                        HostedToolKind::FileSearch => "file_search",
                        HostedToolKind::CodeExecution => "code_interpreter",
                        HostedToolKind::ComputerUse => "computer_use_preview",
                        HostedToolKind::Mcp => "mcp",
                        HostedToolKind::UrlContext => "url_context",
                        HostedToolKind::ProviderSpecific => "provider_tool",
                    });
                }
                Ok(config)
            }
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Value::Array)
}

fn encode_tool_choice(
    choice: &ToolChoice,
    definitions: &[ToolDefinition],
) -> Result<Value, AdapterError> {
    Ok(match choice {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::None => json!("none"),
        ToolChoice::Required => json!("required"),
        ToolChoice::Named { name } => json!({"type":"function","name":name}),
        ToolChoice::Allowed { mode, names } => json!({
            "type":"allowed_tools",
            "mode":if *mode == AllowedMode::Required { "required" } else { "auto" },
            "tools":names.iter().map(|name| json!({
                "type":tool_choice_kind(name, definitions),
                "name":name,
            })).collect::<Vec<_>>(),
        }),
    })
}

fn tool_choice_kind<'a>(name: &str, definitions: &'a [ToolDefinition]) -> &'a str {
    definitions
        .iter()
        .find_map(|definition| match definition {
            ToolDefinition::Function(tool) if tool.name == name => Some("function"),
            ToolDefinition::Custom(tool) if tool.name == name => Some("custom"),
            ToolDefinition::Namespace(tool) if tool.namespace == name => Some("namespace"),
            ToolDefinition::Hosted(tool) if tool.name.as_deref() == Some(name) => tool
                .config
                .get("type")
                .and_then(Value::as_str)
                .or(Some("provider_tool")),
            _ => None,
        })
        .unwrap_or("function")
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
        } => json!({
            "type":"json_schema","name":name,"description":description,"schema":schema,"strict":strict,
        }),
    })
}

fn encode_reasoning_config(body: &mut Value, reasoning: &ReasoningConfig, plan: &ConversionPlan) {
    if matches!(reasoning.mode, ReasoningMode::Disabled) && !reasoning.is_explicitly_disabled() {
        return;
    }
    let mut value = json!({});
    if let Some(effort) = reasoning.effort {
        value["effort"] = json!(effort.openai_name());
    }
    if let Some(max_tokens) = reasoning.token_budget {
        value["max_tokens"] = json!(max_tokens);
    }
    let summary = match reasoning.summary {
        ReasoningSummaryMode::Auto => Some("auto"),
        ReasoningSummaryMode::Concise => Some("concise"),
        ReasoningSummaryMode::Detailed => Some("detailed"),
        ReasoningSummaryMode::None => None,
    };
    if let Some(summary) = summary {
        value["summary"] = json!(summary);
    }
    for (index, extension) in reasoning.provider_extensions.iter().enumerate() {
        if feature_is_preserved(plan, &format!("reasoning.provider_extensions[{index}]")) {
            value[&extension.name] = extension.value.clone();
        }
    }
    body["reasoning"] = value;
}

fn encode_generation(body: &mut Value, generation: &GenerationConfig) {
    if let Some(value) = generation.max_output_tokens {
        body["max_output_tokens"] = json!(value);
    }
    if let Some(value) = generation.temperature {
        body["temperature"] = json!(value);
    }
    if let Some(value) = generation.top_p {
        body["top_p"] = json!(value);
    }
    if let Some(value) = generation.frequency_penalty {
        body["frequency_penalty"] = json!(value);
    }
    if let Some(value) = generation.presence_penalty {
        body["presence_penalty"] = json!(value);
    }
    if let Some(value) = generation.seed {
        body["seed"] = json!(value);
    }
    if let Some(value) = &generation.service_tier {
        body["service_tier"] = json!(value);
    }
    if let Some(value) = generation.parallel_tool_calls {
        body["parallel_tool_calls"] = json!(value);
    }
    if !generation.modalities.is_empty() {
        body["modalities"] = json!(
            generation
                .modalities
                .iter()
                .map(|value| match value {
                    OutputModality::Text => "text",
                    OutputModality::Audio => "audio",
                    OutputModality::Image => "image",
                    OutputModality::Video => "video",
                })
                .collect::<Vec<_>>()
        );
    }
    if let Some(audio) = &generation.audio {
        body["audio"] = json!({"voice":audio.voice,"format":audio.format,"sample_rate_hz":audio.sample_rate_hz});
    }
}

fn encode_extensions(body: &mut Value, extensions: &[ProviderExtension], plan: &ConversionPlan) {
    for (index, extension) in extensions.iter().enumerate() {
        if extension.namespace == "openai_responses"
            && extension.nested_path().is_none()
            && feature_is_preserved(plan, &format!("extensions[{index}]"))
        {
            body[&extension.name] = extension.value.clone();
        }
    }
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

fn encode_image(image: &MediaBlock) -> Result<Value, AdapterError> {
    let mut value = match &image.source {
        MediaSource::InlineBase64 { media_type, data } => {
            json!({"type":"input_image","image_url":format!("data:{media_type};base64,{data}")})
        }
        MediaSource::RemoteUrl { url } => json!({"type":"input_image","image_url":url}),
        MediaSource::ProviderFileId { id, .. } => json!({"type":"input_image","file_id":id}),
    };
    if let Some(detail) = image.detail {
        value["detail"] = json!(match detail {
            MediaDetail::Auto => "auto",
            MediaDetail::Low => "low",
            MediaDetail::High => "high",
            MediaDetail::Original => "original",
        });
    }
    encode_prompt_cache_breakpoint(&mut value, &image.metadata);
    Ok(value)
}

fn encode_file(file: &FileBlock) -> Result<Value, AdapterError> {
    let mut value = match &file.source {
        MediaSource::ProviderFileId { id, .. } => json!({"type":"input_file","file_id":id}),
        MediaSource::RemoteUrl { url } => json!({"type":"input_file","file_url":url}),
        MediaSource::InlineBase64 { media_type, data } => json!({
            "type":"input_file","file_data":data,"media_type":media_type,
        }),
    };
    if let Some(filename) = &file.filename {
        value["filename"] = json!(filename);
    }
    encode_prompt_cache_breakpoint(&mut value, &file.metadata);
    Ok(value)
}

fn encode_audio(audio: &MediaBlock) -> Result<Value, AdapterError> {
    if audio.metadata.prompt_cache_breakpoint.is_some() {
        return Err(AdapterError::new(
            "unsupported_prompt_cache_breakpoint",
            "$.content.audio.prompt_cache_breakpoint",
            "Responses does not accept cache breakpoints on audio blocks",
        ));
    }
    match &audio.source {
        MediaSource::InlineBase64 { media_type, data } => Ok(json!({
            "type":"input_audio",
            "audio_url":format!("data:{media_type};base64,{data}"),
        })),
        _ => Err(AdapterError::new(
            "encoder_plan_violation",
            "$.content.audio",
            "Responses audio requires inline data",
        )),
    }
}

fn encode_response_blocks(
    blocks: &[ContentBlock],
    context: &AdapterContext,
    model: &str,
) -> Result<Vec<Value>, AdapterError> {
    let mut output = Vec::new();
    let mut message_content = Vec::new();
    let flush = |content: &mut Vec<Value>, output: &mut Vec<Value>| {
        if !content.is_empty() {
            output.push(json!({
                "type":"message","role":"assistant","status":"completed",
                "content":std::mem::take(content),
            }));
        }
    };
    for (block_index, block) in blocks.iter().enumerate() {
        match block {
            ContentBlock::Text(text) => message_content.push(json!({
                "type":"output_text","text":text.text,"annotations":text.metadata.annotations,
            })),
            ContentBlock::Refusal(refusal) => {
                message_content.push(json!({"type":"refusal","refusal":refusal.text}))
            }
            ContentBlock::Reasoning(reasoning) => {
                flush(&mut message_content, &mut output);
                let mut item = json!({
                    "type":"reasoning","id":reasoning.metadata.source_item_id,
                    "status":reasoning.metadata.status.map(encode_item_status),
                    "summary":reasoning.summary.iter().map(|text| json!({"type":"summary_text","text":text.text})).collect::<Vec<_>>(),
                });
                if let Some(text) = &reasoning.text {
                    item["content"] = json!([{"type":"reasoning_text","text":text}]);
                }
                let portable = reasoning_entries(&reasoning.artifacts);
                let native = reasoning.artifacts.iter().find(|artifact| {
                    artifact.kind == ArtifactKind::OpenAiEncryptedReasoning
                        && artifact_matches_context(artifact, context, model)
                });
                if portable.len() == 1 {
                    if let Some(native) = native {
                        item["encrypted_content"] = native.payload.clone();
                    } else if let Some(carrier) = encode_carrier(portable) {
                        item["encrypted_content"] = json!(carrier);
                    }
                } else if let Some(carrier) = encode_carrier(portable) {
                    item["encrypted_content"] = json!(carrier);
                }
                output.push(item);
            }
            ContentBlock::ToolCall(call) => {
                flush(&mut message_content, &mut output);
                if let Some(carrier) = encode_carrier(tool_call_entries(&call.id, &call.artifacts))
                {
                    output.push(json!({
                        "type":"reasoning",
                        "id":format!("rs_const_carrier_{block_index}"),
                        "status":"completed",
                        "summary":[],
                        "encrypted_content":carrier,
                    }));
                }
                output.push(encode_tool_call_item(call));
            }
            ContentBlock::ProviderArtifact(artifact)
                if artifact_matches_context(artifact, context, model) =>
            {
                flush(&mut message_content, &mut output);
                output.push(artifact.payload.clone());
            }
            ContentBlock::ProviderArtifact(artifact) if is_portable_artifact(&artifact.kind) => {
                flush(&mut message_content, &mut output);
                if let Some(carrier) =
                    encode_carrier(previous_part_entries(std::slice::from_ref(artifact)))
                {
                    output.push(json!({
                        "type":"reasoning",
                        "id":format!("rs_const_part_carrier_{block_index}"),
                        "status":"completed",
                        "summary":[],
                        "encrypted_content":carrier,
                    }));
                }
            }
            ContentBlock::ProviderArtifact(_) => {}
            // Cross-protocol response conversion is availability-first. The conversion plan
            // records each unsupported block as an approximation before the encoder is called.
            // Omitting it here lets supported text/tool output continue instead of replacing the
            // whole upstream response with a local conversion error.
            _ => {}
        }
    }
    flush(&mut message_content, &mut output);
    Ok(output)
}

fn artifact_matches_context(
    artifact: &OpaqueArtifact,
    context: &AdapterContext,
    model: &str,
) -> bool {
    affinity_matches_context(
        &artifact.affinity,
        context,
        model,
        ProtocolKind::OpenAiResponses,
    )
}

fn extension_matches_context(
    extension: &ProviderExtension,
    context: &AdapterContext,
    model: &str,
) -> bool {
    artifact_matches_context(
        &OpaqueArtifact {
            kind: ArtifactKind::ProviderSpecific,
            payload: Value::Null,
            affinity: extension.affinity.clone(),
            replay: ReplayPolicy::ExactWhenCompatible,
            criticality: ArtifactCriticality::Supplemental,
        },
        context,
        model,
    )
}

fn item_status(value: Option<&str>) -> Option<ItemStatus> {
    match value {
        Some("in_progress") => Some(ItemStatus::InProgress),
        Some("completed") => Some(ItemStatus::Completed),
        Some("incomplete") => Some(ItemStatus::Incomplete),
        Some("failed") => Some(ItemStatus::Failed),
        Some("cancelled") => Some(ItemStatus::Cancelled),
        _ => None,
    }
}

fn encode_item_status(value: ItemStatus) -> &'static str {
    match value {
        ItemStatus::InProgress => "in_progress",
        ItemStatus::Completed => "completed",
        ItemStatus::Incomplete => "incomplete",
        ItemStatus::Failed => "failed",
        ItemStatus::Cancelled => "cancelled",
    }
}

fn response_status(value: Option<&str>) -> ResponseStatus {
    match value {
        Some("in_progress") => ResponseStatus::InProgress,
        Some("incomplete") => ResponseStatus::Incomplete,
        Some("failed") => ResponseStatus::Failed,
        Some("cancelled") => ResponseStatus::Cancelled,
        Some("refused") => ResponseStatus::Refused,
        _ => ResponseStatus::Completed,
    }
}

fn encode_response_status(value: ResponseStatus) -> &'static str {
    match value {
        ResponseStatus::InProgress => "in_progress",
        ResponseStatus::Completed => "completed",
        ResponseStatus::Incomplete => "incomplete",
        ResponseStatus::Failed => "failed",
        ResponseStatus::Cancelled => "cancelled",
        ResponseStatus::Refused => "refused",
    }
}

fn response_finish_reason(
    status: ResponseStatus,
    blocks: &[ContentBlock],
    has_error: bool,
) -> FinishReason {
    if has_error || status == ResponseStatus::Failed {
        return FinishReason::Error;
    }
    if status == ResponseStatus::Cancelled {
        return FinishReason::Cancelled;
    }
    if status == ResponseStatus::Refused {
        return FinishReason::Refusal;
    }
    if status == ResponseStatus::Incomplete {
        return FinishReason::Length;
    }
    if blocks
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolCall(_)))
    {
        FinishReason::ToolCalls
    } else {
        FinishReason::Stop
    }
}

fn decode_error(value: Option<&Value>) -> Option<CanonicalError> {
    let value = value.filter(|value| value.is_object())?;
    let provider_code = value
        .get("code")
        .and_then(Value::as_str)
        .map(str::to_string);
    Some(CanonicalError {
        code: value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("provider_error")
            .to_string(),
        message: value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        retryable: value
            .get("retryable")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        provider_code,
        details: value.get("details").cloned(),
    })
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
    Usage {
        input_tokens: reported(
            value
                .get("input_tokens")
                .or_else(|| value.get("prompt_tokens")),
        ),
        output_tokens: reported(
            value
                .get("output_tokens")
                .or_else(|| value.get("completion_tokens")),
        ),
        total_tokens: reported(value.get("total_tokens")),
        cache_read_tokens: reported(
            value
                .pointer("/input_tokens_details/cached_tokens")
                .or_else(|| value.pointer("/prompt_tokens_details/cached_tokens")),
        ),
        cache_write_tokens: reported(
            value
                .pointer("/input_tokens_details/cache_write_tokens")
                .or_else(|| value.pointer("/prompt_tokens_details/cache_write_tokens")),
        ),
        reasoning_tokens: reported(
            value
                .pointer("/output_tokens_details/reasoning_tokens")
                .or_else(|| value.pointer("/completion_tokens_details/reasoning_tokens")),
        ),
        audio_input_units: reported(value.pointer("/input_tokens_details/audio_tokens")),
        audio_output_units: reported(value.pointer("/output_tokens_details/audio_tokens")),
        ..Default::default()
    }
    .with_inclusive_reasoning_output()
}

fn encode_usage(usage: &Usage) -> Value {
    let cache_write = usage
        .cache_write_tokens
        .value
        .or(usage.cache_creation_tokens.value);
    json!({
        "input_tokens":usage.input_tokens.value,
        "output_tokens":usage.output_tokens.value,
        "total_tokens":usage.total_tokens.value,
        "input_tokens_details":{
            "cached_tokens":usage.cache_read_tokens.value,
            "cache_write_tokens":cache_write,
            "audio_tokens":usage.audio_input_units.value,
        },
        "output_tokens_details":{
            "reasoning_tokens":usage.reasoning_tokens.value,
            "audio_tokens":usage.audio_output_units.value,
        },
    })
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::OpenAiResponsesAdapter;
    use crate::protocol::adapters::{AdapterContext, ProtocolAdapter, ProviderDialect};
    use crate::protocol::adapters::{AnthropicMessagesAdapter, OpenAiChatAdapter};
    use crate::protocol::capability::{
        CapabilityEvidence, CapabilityProfile, EvidenceSource, Feature, SupportState,
    };
    use crate::protocol::conversion::{ConversionPolicy, IssuerIdentity, plan_conversion};
    use crate::protocol::ir::{
        ArtifactAffinity, ContentBlock, PromptCacheBreakpointMode, PromptCacheMode, ReasoningMode,
        ResponseFormat, ResponseStatus, ToolChoice, ToolDefinition,
    };
    use crate::protocol::kind::ProtocolKind;

    fn fixture() -> Value {
        serde_json::from_str(include_str!(
            "../../../tests/fixtures/protocol-v2/openai-responses/advanced.json"
        ))
        .unwrap()
    }

    fn context(account: &str) -> AdapterContext {
        AdapterContext {
            dialect: ProviderDialect::OpenAi,
            issuer: IssuerIdentity {
                provider: "openai".to_string(),
                endpoint_fingerprint: "api.openai.com".to_string(),
                account_fingerprint: Some(account.to_string()),
                model_family: Some("responses-test".to_string()),
            },
        }
    }

    #[test]
    fn effort_only_reasoning_uses_automatic_mode_without_a_null_budget() {
        let responses = OpenAiResponsesAdapter;
        let request = responses
            .decode_request(
                &serde_json::json!({
                    "model": "gpt-5",
                    "input": "hello",
                    "reasoning": {"effort": "high"}
                }),
                &context("account-1"),
            )
            .unwrap();

        assert_eq!(request.reasoning.mode, ReasoningMode::Automatic);
        assert_eq!(request.reasoning.token_budget, None);

        let plan = plan_conversion(
            &request,
            ProtocolKind::OpenAiResponses,
            ProtocolKind::AnthropicMessages,
            &context("account-1").issuer,
            &context("account-1").issuer,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap();
        let anthropic = AnthropicMessagesAdapter
            .encode_request(&request, &plan, &context("account-1"))
            .unwrap();
        assert_eq!(anthropic["thinking"]["type"], "adaptive");
        assert!(anthropic["thinking"].get("budget_tokens").is_none());
    }

    #[test]
    fn responses_audio_extension_round_trips_as_codex_audio_url() {
        let adapter = OpenAiResponsesAdapter;
        let value = serde_json::json!({
            "model": "gpt-audio-preview",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_audio",
                    "audio_url": "data:audio/wav;base64,AAAA"
                }]
            }]
        });
        let request = adapter
            .decode_request(&value, &context("account-1"))
            .unwrap();
        assert!(matches!(request.turns[0].blocks[0], ContentBlock::Audio(_)));
        let public_error = plan_conversion(
            &request,
            ProtocolKind::OpenAiResponses,
            ProtocolKind::OpenAiResponses,
            &context("account-1").issuer,
            &context("account-1").issuer,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .expect_err("public Responses audio must require provider-specific evidence");
        assert!(
            public_error
                .report()
                .issues
                .iter()
                .any(|issue| issue.code == "unsupported_required_feature")
        );
        let profile = CapabilityProfile::new(vec![CapabilityEvidence::exact_model(
            Feature::AudioInput,
            SupportState::Supported,
            EvidenceSource::ProviderMetadata,
            "gpt-audio-preview",
            ProtocolKind::OpenAiResponses,
            1,
        )]);
        let plan = plan_conversion(
            &request,
            ProtocolKind::OpenAiResponses,
            ProtocolKind::OpenAiResponses,
            &context("account-1").issuer,
            &context("account-1").issuer,
            &profile,
            ConversionPolicy::Production,
        )
        .unwrap();
        let encoded = adapter
            .encode_request(&request, &plan, &context("account-1"))
            .unwrap();
        assert_eq!(
            encoded["input"][0]["content"][0],
            serde_json::json!({
                "type": "input_audio",
                "audio_url": "data:audio/wav;base64,AAAA"
            })
        );
    }

    #[test]
    fn decodes_ordered_responses_items_tools_artifacts_and_status() {
        let adapter = OpenAiResponsesAdapter;
        let fixture = fixture();
        let request = adapter
            .decode_request(&fixture["request"], &context("account-1"))
            .unwrap();

        assert_eq!(request.instructions.len(), 1);
        assert_eq!(request.turns.len(), 6);
        assert!(matches!(request.turns[0].blocks[1], ContentBlock::Image(_)));
        assert!(matches!(request.turns[0].blocks[2], ContentBlock::File(_)));
        let ContentBlock::Reasoning(reasoning) = &request.turns[1].blocks[0] else {
            panic!("reasoning block");
        };
        assert_eq!(reasoning.summary[0].text, "checked");
        assert_eq!(reasoning.text.as_deref(), Some("internal"));
        assert!(matches!(
            reasoning.artifacts[0].affinity,
            ArtifactAffinity::ExactIssuer { .. }
        ));
        assert_eq!(request.tools.len(), 6);
        assert!(
            request
                .tools
                .iter()
                .any(|tool| matches!(tool, ToolDefinition::Custom(_)))
        );
        assert!(
            request
                .tools
                .iter()
                .any(|tool| matches!(tool, ToolDefinition::Namespace(_)))
        );
        assert!(
            request
                .tools
                .iter()
                .any(|tool| matches!(tool, ToolDefinition::Hosted(_)))
        );
        assert!(matches!(request.tool_choice, ToolChoice::Allowed { .. }));
        assert!(matches!(
            request.response_format,
            ResponseFormat::JsonSchema { .. }
        ));
        assert_eq!(request.metadata.user_id.as_deref(), Some("account-1"));
        assert_eq!(request.generation.modalities.len(), 2);
        assert_eq!(
            request.generation.audio.as_ref().unwrap().voice.as_deref(),
            Some("alloy")
        );

        let response = adapter
            .decode_response(&fixture["response"], &context("account-1"))
            .unwrap();
        assert_eq!(response.status, ResponseStatus::Incomplete);
        assert_eq!(response.blocks.len(), 4);
        assert!(matches!(response.blocks[0], ContentBlock::Reasoning(_)));
        assert!(matches!(response.blocks[1], ContentBlock::Text(_)));
        assert!(matches!(response.blocks[2], ContentBlock::Refusal(_)));
        assert!(matches!(response.blocks[3], ContentBlock::ToolCall(_)));
        assert_eq!(response.usage.cache_read_tokens.value, Some(6));
        assert_eq!(response.usage.cache_write_tokens.value, Some(5));
        assert_eq!(response.usage.reasoning_tokens.value, Some(3));
    }

    #[test]
    fn encoding_filters_encrypted_reasoning_for_a_different_issuer() {
        let adapter = OpenAiResponsesAdapter;
        let mut request = adapter
            .decode_request(&fixture()["request"], &context("account-1"))
            .unwrap();
        request.extensions.clear();
        let plan = plan_conversion(
            &request,
            ProtocolKind::OpenAiResponses,
            ProtocolKind::OpenAiResponses,
            &context("account-1").issuer,
            &context("account-2").issuer,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap();

        let encoded = adapter
            .encode_request(&request, &plan, &context("account-2"))
            .unwrap();
        assert!(!encoded.to_string().contains("opaque-reasoning"));
        assert!(
            encoded["input"]
                .as_array()
                .unwrap()
                .iter()
                .all(|item| item["type"] != "reasoning")
        );
        assert!(
            plan.report()
                .issues
                .iter()
                .any(|issue| issue.code == "reasoning_history_omitted")
        );
    }

    #[test]
    fn reencodes_same_issuer_request_and_response_without_flattening() {
        let adapter = OpenAiResponsesAdapter;
        let fixture = fixture();
        let request = adapter
            .decode_request(&fixture["request"], &context("account-1"))
            .unwrap();
        let plan = plan_conversion(
            &request,
            ProtocolKind::OpenAiResponses,
            ProtocolKind::OpenAiResponses,
            &context("account-1").issuer,
            &context("account-1").issuer,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap();

        let encoded_request = adapter
            .encode_request(&request, &plan, &context("account-1"))
            .unwrap();
        let encoded_input = encoded_request["input"].as_array().unwrap();
        let reasoning = encoded_input
            .iter()
            .find(|item| item["type"] == "reasoning")
            .unwrap();
        assert_eq!(reasoning["encrypted_content"], "opaque-reasoning");
        assert_eq!(reasoning["content"][0]["type"], "reasoning_text");
        assert_eq!(reasoning["content"][0]["text"], "internal");
        assert!(reasoning.get("text").is_none());
        assert_eq!(encoded_input[0]["role"], "system");
        assert_eq!(encoded_request["previous_response_id"], "resp-prev");
        assert_eq!(encoded_request["store"], false);
        assert_eq!(encoded_request["prompt_cache_key"], "fixture-cache");
        assert_eq!(encoded_request["prompt_cache_retention"], "24h");
        assert_eq!(
            encoded_request["tool_choice"]["tools"][0]["type"],
            "function"
        );
        assert_eq!(encoded_request["tool_choice"]["tools"][1]["type"], "custom");
        assert!(
            encoded_input
                .iter()
                .any(|item| item["type"] == "custom_tool_call_output")
        );
        assert!(
            encoded_request["text"]["format"]["schema"]
                .get("title")
                .is_none()
        );
        assert_eq!(
            encoded_request["text"]["format"]["schema"]["required"],
            serde_json::json!(["ok"])
        );

        let response = adapter
            .decode_response(&fixture["response"], &context("account-1"))
            .unwrap();
        let encoded_response = adapter
            .encode_response(&response, &plan, &context("account-1"))
            .unwrap();
        assert_eq!(encoded_response["status"], "incomplete");
        assert_eq!(
            encoded_response["output"][0]["encrypted_content"],
            "opaque-output"
        );
        assert_eq!(
            encoded_response["output"][1]["content"][0]["annotations"][0]["type"],
            "url_citation"
        );
        assert_eq!(
            encoded_response["usage"]["input_tokens_details"]["cached_tokens"],
            6
        );
        assert_eq!(
            encoded_response["usage"]["input_tokens_details"]["cache_write_tokens"],
            5
        );
    }

    #[test]
    fn preserves_failed_response_error_details() {
        let response = OpenAiResponsesAdapter
            .decode_response(
                &serde_json::json!({
                    "id":"resp-failed",
                    "model":"responses-test",
                    "status":"failed",
                    "output":[],
                    "error":{"type":"invalid_request_error","code":"bad_input","message":"bad","retryable":false}
                }),
                &context("account-1"),
            )
            .unwrap();

        assert_eq!(response.status, ResponseStatus::Failed);
        assert_eq!(
            response.error.as_ref().unwrap().provider_code.as_deref(),
            Some("bad_input")
        );
        assert_eq!(
            response.finish.reason,
            crate::protocol::ir::FinishReason::Error
        );
    }

    #[test]
    fn response_does_not_replay_encrypted_reasoning_to_another_model() {
        let adapter = OpenAiResponsesAdapter;
        let request = adapter
            .decode_request(&fixture()["request"], &context("account-1"))
            .unwrap();
        let plan = plan_conversion(
            &request,
            ProtocolKind::OpenAiResponses,
            ProtocolKind::OpenAiResponses,
            &context("account-1").issuer,
            &context("account-1").issuer,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap();
        let mut response = adapter
            .decode_response(&fixture()["response"], &context("account-1"))
            .unwrap();
        response.model = Some("another-model".to_string());

        let encoded = adapter
            .encode_response(&response, &plan, &context("account-1"))
            .unwrap();

        assert!(!encoded.to_string().contains("opaque-output"));
    }

    #[test]
    fn decodes_cancelled_terminal_status() {
        let response = OpenAiResponsesAdapter
            .decode_response(
                &serde_json::json!({
                    "id":"resp-cancelled",
                    "model":"responses-test",
                    "status":"cancelled",
                    "output":[]
                }),
                &context("account-1"),
            )
            .unwrap();

        assert_eq!(response.status, ResponseStatus::Cancelled);
        assert_eq!(
            response.finish.reason,
            crate::protocol::ir::FinishReason::Cancelled
        );
    }

    #[test]
    fn preserves_prompt_cache_options_and_breakpoints_between_openai_surfaces() {
        let responses = OpenAiResponsesAdapter;
        let chat = OpenAiChatAdapter;
        let request = responses
            .decode_request(
                &serde_json::json!({
                    "model":"gpt-5.6",
                    "prompt_cache_key":"tenant:fixture:stable-prefix",
                    "prompt_cache_options":{"mode":"explicit","ttl":"30m"},
                    "prompt_cache_retention":"24h",
                    "input":[{
                        "type":"message",
                        "role":"user",
                        "content":[{
                            "type":"input_text",
                            "text":"stable prefix",
                            "prompt_cache_breakpoint":{"mode":"explicit"}
                        }]
                    }]
                }),
                &context("account-1"),
            )
            .unwrap();

        assert_eq!(
            request.metadata.prompt_cache.mode,
            Some(PromptCacheMode::Explicit)
        );
        assert_eq!(request.metadata.prompt_cache.ttl.as_deref(), Some("30m"));
        let ContentBlock::Text(text) = &request.turns[0].blocks[0] else {
            panic!("text block");
        };
        assert_eq!(
            text.metadata.prompt_cache_breakpoint,
            Some(PromptCacheBreakpointMode::Explicit)
        );

        let to_chat = plan_conversion(
            &request,
            ProtocolKind::OpenAiResponses,
            ProtocolKind::OpenAiChat,
            &context("account-1").issuer,
            &context("account-1").issuer,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap();
        let chat_body = chat
            .encode_request(&request, &to_chat, &context("account-1"))
            .unwrap();
        assert_eq!(
            chat_body["prompt_cache_key"],
            "tenant:fixture:stable-prefix"
        );
        assert_eq!(chat_body["prompt_cache_options"]["mode"], "explicit");
        assert_eq!(chat_body["prompt_cache_options"]["ttl"], "30m");
        assert_eq!(
            chat_body["messages"][0]["content"][0]["prompt_cache_breakpoint"]["mode"],
            "explicit"
        );

        let chat_request = chat
            .decode_request(&chat_body, &context("account-1"))
            .unwrap();
        let to_responses = plan_conversion(
            &chat_request,
            ProtocolKind::OpenAiChat,
            ProtocolKind::OpenAiResponses,
            &context("account-1").issuer,
            &context("account-1").issuer,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap();
        let responses_body = responses
            .encode_request(&chat_request, &to_responses, &context("account-1"))
            .unwrap();
        assert_eq!(responses_body["prompt_cache_options"]["mode"], "explicit");
        assert_eq!(
            responses_body["input"][0]["content"][0]["prompt_cache_breakpoint"]["mode"],
            "explicit"
        );
    }

    #[test]
    fn rejects_missing_model_and_invalid_response_shape() {
        let adapter = OpenAiResponsesAdapter;
        assert_eq!(
            adapter
                .decode_request(&serde_json::json!({"input":[]}), &context("a"))
                .unwrap_err()
                .code(),
            "model_missing"
        );
        assert_eq!(
            adapter
                .decode_response(&serde_json::json!({"output":{}}), &context("a"))
                .unwrap_err()
                .code(),
            "response_output_invalid"
        );
    }
}
