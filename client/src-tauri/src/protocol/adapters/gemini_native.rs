use std::collections::{HashMap, VecDeque};

use serde_json::{Map, Value, json};

use crate::protocol::adapters::{
    AdapterContext, AdapterError, ProtocolAdapter, affinity_matches_context,
    collect_nested_unknown_fields, continuation_artifact_affinity, ensure_encoding_plan,
    exact_issuer_affinity, link_tool_result_names, protocol_affinity, response_status_from_finish,
};
use crate::protocol::capability::Feature;
use crate::protocol::continuation::{
    ContinuationCarrier, ContinuationOwner, decode_carrier, encode_carrier, is_portable_artifact,
    previous_part_entries, reasoning_artifacts_are_encrypted, reasoning_entries, tool_call_entries,
};
use crate::protocol::conversion::{
    ArtifactDisposition, ConversionPlan, FeatureDisposition, canonicalize_schema,
    canonicalize_tool_schema, flatten_namespace_tools,
};
use crate::protocol::ir::*;
use crate::protocol::kind::ProtocolKind;

pub(crate) struct GeminiNativeAdapter;

impl ProtocolAdapter for GeminiNativeAdapter {
    fn decode_request(
        &self,
        body: &Value,
        context: &AdapterContext,
    ) -> Result<CanonicalRequestV2, AdapterError> {
        let model = body
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| context.issuer.model_family.clone())
            .filter(|model| !model.trim().is_empty())
            .ok_or_else(|| AdapterError::new("model_missing", "$.model", "model is required"))?;
        let contents = body
            .get("contents")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                AdapterError::new(
                    "contents_missing",
                    "$.contents",
                    "Gemini contents must be an array",
                )
            })?;
        let mut request = CanonicalRequestV2::new(&model);
        request.metadata.source_protocol = Some(ProtocolKind::GeminiNative);
        request.instructions =
            decode_system_instruction(body.get("systemInstruction"), context, &model)?;
        request.turns = decode_contents(contents, context, &model)?;
        request.tools = decode_tools(body.get("tools"))?;
        request.tool_choice =
            decode_tool_choice(body.pointer("/toolConfig/functionCallingConfig"))?;
        request.response_format = decode_response_format(body)?;
        request.reasoning = decode_reasoning(body, context, &model);
        request.generation = decode_generation(body);
        request.extensions = decode_request_extensions(body, context, &model);
        link_tool_result_names(&mut request.turns);
        request
            .validate()
            .map_err(|error| AdapterError::new(error.code(), "$.contents", error.to_string()))?;
        Ok(request)
    }

    fn encode_request(
        &self,
        request: &CanonicalRequestV2,
        plan: &ConversionPlan,
        context: &AdapterContext,
    ) -> Result<Value, AdapterError> {
        ensure_encoding_plan(plan, ProtocolKind::GeminiNative)?;
        request
            .validate()
            .map_err(|error| AdapterError::new(error.code(), "$", error.to_string()))?;
        let mut body = json!({
            "contents": encode_contents(&request.turns, plan)?,
        });
        if !request.instructions.is_empty() {
            body["systemInstruction"] = json!({
                "parts": encode_instruction_parts(&request.instructions, plan)?
            });
        }
        if !request.tools.is_empty() {
            body["tools"] = encode_tools(&request.tools, plan)?;
            body["toolConfig"] = json!({
                "functionCallingConfig": encode_tool_choice(&request.tool_choice)
            });
        }
        encode_generation(&mut body, request)?;
        replay_request_extensions(&mut body, &request.extensions, context, &request.model);
        Ok(body)
    }

    fn decode_response(
        &self,
        body: &Value,
        context: &AdapterContext,
    ) -> Result<CanonicalResponseV2, AdapterError> {
        if body.get("error").is_some() {
            return Ok(decode_error_response(body, context));
        }
        let model = body
            .get("modelVersion")
            .and_then(Value::as_str)
            .or(context.issuer.model_family.as_deref())
            .unwrap_or_default();
        let candidate = body
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|candidates| candidates.first());
        let Some(candidate) = candidate else {
            if let Some(feedback) = body.get("promptFeedback").filter(|feedback| {
                feedback
                    .get("blockReason")
                    .and_then(Value::as_str)
                    .is_some_and(|reason| reason != "BLOCK_REASON_UNSPECIFIED")
            }) {
                let reason = feedback
                    .get("blockReason")
                    .and_then(Value::as_str)
                    .unwrap_or("SAFETY");
                return Ok(CanonicalResponseV2 {
                    id: body
                        .get("responseId")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    model: (!model.is_empty()).then(|| model.to_string()),
                    blocks: Vec::new(),
                    status: ResponseStatus::Refused,
                    finish: FinishDetail {
                        reason: FinishReason::ContentFilter,
                        original_reason: Some(reason.to_string()),
                        incomplete_details: Some(feedback.clone()),
                    },
                    usage: decode_usage(body.get("usageMetadata"))?,
                    error: None,
                    extensions: vec![provider_extension(
                        "promptFeedback",
                        feedback.clone(),
                        context,
                        model,
                        ExtensionCriticality::Advisory,
                    )],
                });
            }
            return Err(AdapterError::new(
                "candidates_missing",
                "$.candidates",
                "Gemini response must contain a candidate",
            ));
        };
        let mut finish = decode_finish(candidate);
        let empty_parts = Vec::new();
        let parts = match candidate.pointer("/content/parts") {
            Some(parts) => parts.as_array().ok_or_else(|| {
                AdapterError::new(
                    "candidate_content_missing",
                    "$.candidates[0].content.parts",
                    "Gemini candidate parts must be an array",
                )
            })?,
            None if finish.original_reason.is_some() => &empty_parts,
            None => {
                return Err(AdapterError::new(
                    "candidate_content_missing",
                    "$.candidates[0].content.parts",
                    "Gemini candidate has neither content nor a terminal finish reason",
                ));
            }
        };
        let mut tracker = CallTracker::default();
        let blocks = decode_parts(
            parts,
            TurnRole::Assistant,
            &mut tracker,
            context,
            model,
            "response.blocks",
        )?;
        if matches!(finish.reason, FinishReason::Stop | FinishReason::Unknown)
            && blocks
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolCall(_)))
        {
            finish.reason = FinishReason::ToolCalls;
        }
        let status = response_status_from_finish(&finish.reason);
        Ok(CanonicalResponseV2 {
            id: body
                .get("responseId")
                .and_then(Value::as_str)
                .map(str::to_string),
            model: (!model.is_empty()).then(|| model.to_string()),
            blocks,
            status,
            finish,
            usage: decode_usage(body.get("usageMetadata"))?,
            error: None,
            extensions: decode_response_extensions(body, candidate, context, model),
        })
    }

    fn encode_response(
        &self,
        response: &CanonicalResponseV2,
        plan: &ConversionPlan,
        context: &AdapterContext,
    ) -> Result<Value, AdapterError> {
        ensure_encoding_plan(plan, ProtocolKind::GeminiNative)?;
        if let Some(error) = &response.error {
            return Ok(json!({
                "error": {
                    "code": error.details.as_ref().and_then(|details| details.get("http_code")).and_then(Value::as_u64).unwrap_or(500),
                    "message": error.message,
                    "status": error.provider_code.clone().unwrap_or_else(|| error.code.clone()),
                    "details": error.details,
                }
            }));
        }
        let model = response.model.as_deref().unwrap_or_default();
        let mut candidate = json!({
            "content": {
                "role": "model",
                "parts": encode_response_parts(&response.blocks, context, model)?
            },
            "finishReason": encode_finish_reason(response.finish.reason),
        });
        let mut body = json!({
            "candidates": [candidate.clone()],
            "usageMetadata": encode_usage(&response.usage),
        });
        if let Some(id) = &response.id {
            body["responseId"] = json!(id);
        }
        if !model.is_empty() {
            body["modelVersion"] = json!(model);
        }
        replay_response_extensions(
            &mut body,
            &mut candidate,
            &response.extensions,
            context,
            model,
        );
        body["candidates"][0] = candidate;
        Ok(body)
    }
}

#[derive(Default)]
struct CallTracker {
    next_id: usize,
    pending: HashMap<String, VecDeque<String>>,
}

impl CallTracker {
    fn register(&mut self, name: &str, explicit: Option<&str>) -> String {
        let id = explicit
            .filter(|id| !id.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                let id = format!("call_gemini_{}", self.next_id);
                self.next_id += 1;
                id
            });
        self.pending
            .entry(name.to_string())
            .or_default()
            .push_back(id.clone());
        id
    }

    fn resolve(&mut self, name: &str, explicit: Option<&str>) -> String {
        explicit
            .filter(|id| !id.trim().is_empty())
            .map(str::to_string)
            .or_else(|| self.pending.get_mut(name).and_then(VecDeque::pop_front))
            .unwrap_or_else(|| {
                let id = format!("call_gemini_result_{}", self.next_id);
                self.next_id += 1;
                id
            })
    }
}

fn decode_system_instruction(
    value: Option<&Value>,
    context: &AdapterContext,
    model: &str,
) -> Result<Vec<Instruction>, AdapterError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let parts = value
        .get("parts")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            AdapterError::new(
                "system_instruction_invalid",
                "$.systemInstruction.parts",
                "system instruction parts must be an array",
            )
        })?;
    let mut tracker = CallTracker::default();
    Ok(vec![Instruction {
        role: InstructionRole::System,
        blocks: decode_parts(
            parts,
            TurnRole::User,
            &mut tracker,
            context,
            model,
            "instructions[0].blocks",
        )?,
    }])
}

fn decode_contents(
    contents: &[Value],
    context: &AdapterContext,
    model: &str,
) -> Result<Vec<Turn>, AdapterError> {
    let mut tracker = CallTracker::default();
    contents
        .iter()
        .enumerate()
        .map(|(index, content)| {
            let role = match content.get("role").and_then(Value::as_str) {
                Some("model") => TurnRole::Assistant,
                Some("user") | Some("function") | None => TurnRole::User,
                Some(other) => {
                    return Err(AdapterError::new(
                        "role_invalid",
                        format!("$.contents[{index}].role"),
                        format!("unsupported Gemini role {other}"),
                    ));
                }
            };
            let parts = content
                .get("parts")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    AdapterError::new(
                        "parts_missing",
                        format!("$.contents[{index}].parts"),
                        "Gemini content parts must be an array",
                    )
                })?;
            Ok(Turn {
                id: None,
                role,
                blocks: decode_parts(
                    parts,
                    role,
                    &mut tracker,
                    context,
                    model,
                    &format!("turns[{index}].blocks"),
                )?,
                status: None,
            })
        })
        .collect()
}

fn decode_parts(
    parts: &[Value],
    role: TurnRole,
    tracker: &mut CallTracker,
    context: &AdapterContext,
    model: &str,
    base_path: &str,
) -> Result<Vec<ContentBlock>, AdapterError> {
    let mut blocks = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        let path = format!("{base_path}[{index}]");
        let signature = part
            .get("thoughtSignature")
            .or_else(|| part.get("thought_signature"))
            .cloned();
        let carrier = signature
            .as_ref()
            .map(decode_carrier)
            .transpose()
            .map_err(|error| {
                AdapterError::new("continuation_carrier_invalid", &path, error.message())
            })?
            .flatten();
        if part.get("thought").and_then(Value::as_bool) == Some(true) {
            let artifacts = match carrier {
                Some(carrier) => carrier_reasoning_artifacts(carrier, &path)?,
                None => signature
                    .map(|payload| {
                        vec![gemini_signature(
                            payload,
                            context,
                            model,
                            ArtifactCriticality::Supplemental,
                        )]
                    })
                    .unwrap_or_default(),
            };
            let encrypted = reasoning_artifacts_are_encrypted(&artifacts);
            blocks.push(ContentBlock::Reasoning(ReasoningBlock {
                text: part.get("text").and_then(Value::as_str).map(str::to_string),
                summary: Vec::new(),
                artifacts,
                encrypted,
                metadata: BlockMetadata::default(),
            }));
            push_part_extras(&mut blocks, part, &["thought", "text"], context, model);
            continue;
        }
        if let Some(call) = part
            .get("functionCall")
            .or_else(|| part.get("function_call"))
        {
            let name = required_string(
                call.get("name"),
                &format!("{path}.functionCall.name"),
                "tool_name_missing",
            )?;
            let id = tracker.register(&name, call.get("id").and_then(Value::as_str));
            let artifacts = match carrier {
                Some(carrier) => carrier_tool_artifacts(carrier, &id, &path)?,
                None => signature
                    .map(|payload| {
                        vec![gemini_signature(
                            payload,
                            context,
                            model,
                            ArtifactCriticality::Required,
                        )]
                    })
                    .unwrap_or_default(),
            };
            blocks.push(ContentBlock::ToolCall(ToolCall {
                id,
                source_item_id: None,
                kind: ToolKind::Function,
                name,
                arguments: Some(
                    call.get("args")
                        .or_else(|| call.get("arguments"))
                        .cloned()
                        .unwrap_or_else(|| json!({})),
                ),
                raw_arguments: None,
                status: ItemStatus::Completed,
                artifacts,
                metadata: BlockMetadata::default(),
            }));
            push_part_extras(
                &mut blocks,
                part,
                &["functionCall", "function_call"],
                context,
                model,
            );
            continue;
        }
        if let Some(result) = part
            .get("functionResponse")
            .or_else(|| part.get("function_response"))
        {
            let name = required_string(
                result.get("name"),
                &format!("{path}.functionResponse.name"),
                "tool_name_missing",
            )?;
            let call_id = tracker.resolve(&name, result.get("id").and_then(Value::as_str));
            let response = result.get("response").cloned().unwrap_or_else(|| json!({}));
            let is_error = response.get("error").is_some();
            blocks.push(ContentBlock::ToolResult(ToolResult {
                call_id,
                name: Some(name),
                content: vec![ContentBlock::Text(TextBlock::new(
                    serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string()),
                ))],
                status: if is_error {
                    ItemStatus::Failed
                } else {
                    ItemStatus::Completed
                },
                is_error,
                metadata: BlockMetadata::default(),
            }));
            push_part_extras(
                &mut blocks,
                part,
                &["functionResponse", "function_response"],
                context,
                model,
            );
            push_standalone_signature(&mut blocks, signature, carrier, context, model, &path)?;
            continue;
        }
        if let Some(text) = part.get("text").and_then(Value::as_str) {
            blocks.push(ContentBlock::Text(TextBlock::new(text)));
            push_part_extras(&mut blocks, part, &["text"], context, model);
            push_standalone_signature(&mut blocks, signature, carrier, context, model, &path)?;
            continue;
        }
        if let Some(data) = part.get("inlineData").or_else(|| part.get("inline_data")) {
            let media_type = required_string(
                data.get("mimeType").or_else(|| data.get("mime_type")),
                &format!("{path}.inlineData.mimeType"),
                "media_type_missing",
            )?;
            let bytes = required_string(
                data.get("data"),
                &format!("{path}.inlineData.data"),
                "media_data_missing",
            )?;
            blocks.push(media_block(
                &media_type,
                MediaSource::InlineBase64 {
                    media_type: media_type.clone(),
                    data: bytes,
                },
            ));
            push_part_extras(
                &mut blocks,
                part,
                &["inlineData", "inline_data"],
                context,
                model,
            );
            push_standalone_signature(&mut blocks, signature, carrier, context, model, &path)?;
            continue;
        }
        if let Some(data) = part.get("fileData").or_else(|| part.get("file_data")) {
            let media_type = required_string(
                data.get("mimeType").or_else(|| data.get("mime_type")),
                &format!("{path}.fileData.mimeType"),
                "media_type_missing",
            )?;
            let file_uri = required_string(
                data.get("fileUri").or_else(|| data.get("file_uri")),
                &format!("{path}.fileData.fileUri"),
                "file_uri_missing",
            )?;
            blocks.push(media_block(
                &media_type,
                MediaSource::ProviderFileId {
                    provider: "gemini".to_string(),
                    id: file_uri,
                    media_type: Some(media_type.clone()),
                },
            ));
            push_part_extras(
                &mut blocks,
                part,
                &["fileData", "file_data"],
                context,
                model,
            );
            push_standalone_signature(&mut blocks, signature, carrier, context, model, &path)?;
            continue;
        }

        let mut opaque = part.clone();
        if let Some(object) = opaque.as_object_mut() {
            object.remove("thoughtSignature");
            object.remove("thought_signature");
        }
        blocks.push(ContentBlock::ProviderArtifact(OpaqueArtifact {
            kind: ArtifactKind::ProviderSpecific,
            payload: opaque,
            affinity: protocol_affinity(ProtocolKind::GeminiNative),
            replay: ReplayPolicy::ExactWhenCompatible,
            criticality: ArtifactCriticality::Supplemental,
        }));
        if let Some(carrier) = carrier {
            return Err(AdapterError::new(
                "continuation_carrier_binding_invalid",
                path,
                format!(
                    "continuation carrier cannot bind to an unsupported Gemini part ({} entries)",
                    carrier.entries.len()
                ),
            ));
        }
        if let Some(payload) = signature {
            blocks.push(ContentBlock::ProviderArtifact(gemini_signature(
                payload,
                context,
                model,
                ArtifactCriticality::Supplemental,
            )));
        }
    }
    if role == TurnRole::Assistant {
        mark_first_function_signature_required(&mut blocks);
    }
    Ok(blocks)
}

fn push_part_extras(
    blocks: &mut Vec<ContentBlock>,
    part: &Value,
    recognized: &[&str],
    _context: &AdapterContext,
    _model: &str,
) {
    let Some(mut extras) = part.as_object().cloned() else {
        return;
    };
    for name in recognized {
        extras.remove(*name);
    }
    extras.remove("thoughtSignature");
    extras.remove("thought_signature");
    if extras.is_empty() {
        return;
    }
    blocks.push(ContentBlock::ProviderArtifact(OpaqueArtifact {
        kind: ArtifactKind::ProviderSpecific,
        payload: json!({"gemini_part_extra":extras}),
        affinity: protocol_affinity(ProtocolKind::GeminiNative),
        replay: ReplayPolicy::ExactWhenCompatible,
        criticality: ArtifactCriticality::Supplemental,
    }));
}

fn push_standalone_signature(
    blocks: &mut Vec<ContentBlock>,
    signature: Option<Value>,
    carrier: Option<ContinuationCarrier>,
    context: &AdapterContext,
    model: &str,
    path: &str,
) -> Result<(), AdapterError> {
    if let Some(carrier) = carrier {
        for entry in carrier.entries {
            if entry.owner != ContinuationOwner::PreviousPart {
                return Err(AdapterError::new(
                    "continuation_carrier_binding_invalid",
                    path,
                    "continuation carrier on an ordinary Gemini part must describe that part",
                ));
            }
            blocks.push(ContentBlock::ProviderArtifact(entry.artifact));
        }
        return Ok(());
    }
    if let Some(payload) = signature {
        blocks.push(ContentBlock::ProviderArtifact(gemini_signature(
            payload,
            context,
            model,
            ArtifactCriticality::Supplemental,
        )));
    }
    Ok(())
}

fn carrier_reasoning_artifacts(
    carrier: ContinuationCarrier,
    path: &str,
) -> Result<Vec<OpaqueArtifact>, AdapterError> {
    carrier
        .entries
        .into_iter()
        .map(|entry| match entry.owner {
            ContinuationOwner::Reasoning => Ok(entry.artifact),
            ContinuationOwner::ToolCall { .. } | ContinuationOwner::PreviousPart => {
                Err(AdapterError::new(
                    "continuation_carrier_binding_invalid",
                    path,
                    "non-reasoning continuation carrier cannot bind to a Gemini thought part",
                ))
            }
        })
        .collect()
}

fn carrier_tool_artifacts(
    carrier: ContinuationCarrier,
    call_id: &str,
    path: &str,
) -> Result<Vec<OpaqueArtifact>, AdapterError> {
    carrier
        .entries
        .into_iter()
        .map(|entry| match entry.owner {
            ContinuationOwner::ToolCall { call_id: expected } if expected == call_id => {
                Ok(entry.artifact)
            }
            _ => Err(AdapterError::new(
                "continuation_carrier_binding_invalid",
                path,
                "continuation carrier does not match this Gemini functionCall",
            )),
        })
        .collect()
}

fn mark_first_function_signature_required(blocks: &mut [ContentBlock]) {
    let Some(ContentBlock::ToolCall(call)) = blocks
        .iter_mut()
        .find(|block| matches!(block, ContentBlock::ToolCall(_)))
    else {
        return;
    };
    if let Some(artifact) = call
        .artifacts
        .iter_mut()
        .find(|artifact| artifact.kind == ArtifactKind::GeminiThoughtSignature)
    {
        artifact.criticality = ArtifactCriticality::Required;
        artifact.replay = ReplayPolicy::Required;
    }
}

fn gemini_signature(
    payload: Value,
    context: &AdapterContext,
    model: &str,
    criticality: ArtifactCriticality,
) -> OpaqueArtifact {
    OpaqueArtifact {
        kind: ArtifactKind::GeminiThoughtSignature,
        payload,
        affinity: continuation_artifact_affinity(context, model, ProtocolKind::GeminiNative),
        replay: if criticality == ArtifactCriticality::Required {
            ReplayPolicy::Required
        } else {
            ReplayPolicy::ExactWhenCompatible
        },
        criticality,
    }
}

fn media_block(media_type: &str, source: MediaSource) -> ContentBlock {
    let block = MediaBlock {
        source,
        detail: None,
        metadata: BlockMetadata::default(),
    };
    if media_type.starts_with("image/") {
        ContentBlock::Image(block)
    } else if media_type.starts_with("audio/") {
        ContentBlock::Audio(block)
    } else if media_type.starts_with("video/") {
        ContentBlock::Video(block)
    } else {
        ContentBlock::File(FileBlock {
            source: block.source,
            filename: None,
            metadata: block.metadata,
        })
    }
}

fn required_string(
    value: Option<&Value>,
    path: &str,
    code: &'static str,
) -> Result<String, AdapterError> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| AdapterError::new(code, path, "required string is missing"))
}

fn decode_tools(value: Option<&Value>) -> Result<Vec<ToolDefinition>, AdapterError> {
    let Some(tools) = value.and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut output = Vec::new();
    for (tool_index, tool) in tools.iter().enumerate() {
        let object = tool.as_object().ok_or_else(|| {
            AdapterError::new(
                "tool_invalid",
                format!("$.tools[{tool_index}]"),
                "Gemini tool must be an object",
            )
        })?;
        if let Some(declarations) = object
            .get("functionDeclarations")
            .or_else(|| object.get("function_declarations"))
            .and_then(Value::as_array)
        {
            for (declaration_index, declaration) in declarations.iter().enumerate() {
                let name = required_string(
                    declaration.get("name"),
                    &format!(
                        "$.tools[{tool_index}].functionDeclarations[{declaration_index}].name"
                    ),
                    "tool_name_missing",
                )?;
                let schema = declaration
                    .get("parametersJsonSchema")
                    .or_else(|| declaration.get("parameters_json_schema"))
                    .or_else(|| declaration.get("parameters"))
                    .cloned()
                    .unwrap_or_else(|| json!({"type":"object"}));
                let schema = canonicalize_tool_schema(&schema).map_err(|error| {
                    AdapterError::new(
                        error.code(),
                        format!("$.tools[{tool_index}].functionDeclarations[{declaration_index}]"),
                        error.to_string(),
                    )
                })?;
                output.push(ToolDefinition::Function(FunctionTool {
                    name,
                    description: declaration
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    input_schema: schema,
                    strict: None,
                    cache_policy: None,
                    defer_loading: None,
                    allowed_callers: Vec::new(),
                    input_examples: Vec::new(),
                    eager_input_streaming: None,
                }));
            }
        }
        for (name, config) in object {
            if matches!(
                name.as_str(),
                "functionDeclarations" | "function_declarations"
            ) {
                continue;
            }
            let kind = match name.as_str() {
                "googleSearch" | "google_search" => HostedToolKind::WebSearch,
                "fileSearch" | "file_search" => HostedToolKind::FileSearch,
                "codeExecution" | "code_execution" => HostedToolKind::CodeExecution,
                "computerUse" | "computer_use" => HostedToolKind::ComputerUse,
                "urlContext" | "url_context" => HostedToolKind::UrlContext,
                _ => HostedToolKind::ProviderSpecific,
            };
            output.push(ToolDefinition::Hosted(HostedTool {
                kind,
                provider: Some("gemini".to_string()),
                name: Some(name.clone()),
                config: config.clone(),
            }));
        }
    }
    Ok(output)
}

fn decode_tool_choice(value: Option<&Value>) -> Result<ToolChoice, AdapterError> {
    let Some(value) = value else {
        return Ok(ToolChoice::Auto);
    };
    let mode = value.get("mode").and_then(Value::as_str).unwrap_or("AUTO");
    let names = value
        .get("allowedFunctionNames")
        .or_else(|| value.get("allowed_function_names"))
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(match (mode, names.as_slice()) {
        ("NONE", _) => ToolChoice::None,
        ("ANY", [name]) => ToolChoice::Named { name: name.clone() },
        ("ANY", names) if !names.is_empty() => ToolChoice::Allowed {
            mode: AllowedMode::Required,
            names: names.to_vec(),
        },
        ("ANY", _) => ToolChoice::Required,
        (_, names) if !names.is_empty() => ToolChoice::Allowed {
            mode: AllowedMode::Auto,
            names: names.to_vec(),
        },
        _ => ToolChoice::Auto,
    })
}

fn decode_response_format(body: &Value) -> Result<ResponseFormat, AdapterError> {
    let config = body.get("generationConfig").unwrap_or(&Value::Null);
    let mime = config
        .get("responseMimeType")
        .or_else(|| config.get("response_mime_type"))
        .and_then(Value::as_str);
    let schema = config
        .get("responseJsonSchema")
        .or_else(|| config.get("response_json_schema"))
        .or_else(|| config.get("_responseJsonSchema"))
        .or_else(|| config.get("responseSchema"))
        .or_else(|| config.get("response_schema"));
    if let Some(schema) = schema {
        return Ok(ResponseFormat::JsonSchema {
            name: "gemini_response".to_string(),
            description: None,
            schema: canonicalize_schema(schema).map_err(|error| {
                AdapterError::new(
                    error.code(),
                    "$.generationConfig.responseJsonSchema",
                    error.to_string(),
                )
            })?,
            strict: None,
        });
    }
    Ok(if mime == Some("application/json") {
        ResponseFormat::JsonObject
    } else {
        ResponseFormat::Text
    })
}

fn decode_reasoning(body: &Value, context: &AdapterContext, model: &str) -> ReasoningConfig {
    let thinking = body.pointer("/generationConfig/thinkingConfig");
    let budget = thinking
        .and_then(|value| value.get("thinkingBudget"))
        .and_then(Value::as_u64);
    let level = thinking
        .and_then(|value| value.get("thinkingLevel"))
        .and_then(Value::as_str);
    let include = thinking
        .and_then(|value| value.get("includeThoughts"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mode = if thinking.is_none() || budget == Some(0) {
        ReasoningMode::Disabled
    } else if budget.is_some() {
        ReasoningMode::Enabled
    } else {
        ReasoningMode::Automatic
    };
    let effort = if budget == Some(0) {
        Some(ReasoningEffort::None)
    } else {
        level.and_then(ReasoningEffort::parse)
    };
    let mut provider_extensions = Vec::new();
    if let Some(thinking) = thinking {
        let mut extra = thinking.as_object().cloned().unwrap_or_default();
        extra.remove("thinkingBudget");
        extra.remove("thinkingLevel");
        extra.remove("includeThoughts");
        if !extra.is_empty() {
            provider_extensions.push(provider_extension(
                "thinking_config_extra",
                Value::Object(extra),
                context,
                model,
                ExtensionCriticality::Advisory,
            ));
        }
    }
    ReasoningConfig {
        mode,
        effort,
        token_budget: budget,
        summary: if include {
            ReasoningSummaryMode::Auto
        } else {
            ReasoningSummaryMode::None
        },
        provider_extensions,
    }
}

fn decode_generation(body: &Value) -> GenerationConfig {
    let config = body.get("generationConfig").unwrap_or(&Value::Null);
    GenerationConfig {
        max_output_tokens: config.get("maxOutputTokens").and_then(Value::as_u64),
        temperature: config.get("temperature").and_then(Value::as_f64),
        top_p: config.get("topP").and_then(Value::as_f64),
        top_k: config.get("topK").and_then(Value::as_u64),
        frequency_penalty: config.get("frequencyPenalty").and_then(Value::as_f64),
        presence_penalty: config.get("presencePenalty").and_then(Value::as_f64),
        seed: config.get("seed").and_then(Value::as_i64),
        stop: config
            .get("stopSequences")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        service_tier: body
            .get("serviceTier")
            .and_then(Value::as_str)
            .map(str::to_string),
        latency_mode: None,
        verbosity: None,
        modalities: config
            .get("responseModalities")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(|value| match value {
                "TEXT" => Some(OutputModality::Text),
                "IMAGE" => Some(OutputModality::Image),
                "AUDIO" => Some(OutputModality::Audio),
                _ => None,
            })
            .collect(),
        audio: None,
        parallel_tool_calls: None,
    }
}

fn decode_request_extensions(
    body: &Value,
    context: &AdapterContext,
    model: &str,
) -> Vec<ProviderExtension> {
    let mut extensions = Vec::new();
    let recognized = [
        "model",
        "contents",
        "systemInstruction",
        "tools",
        "toolConfig",
        "generationConfig",
        "serviceTier",
    ];
    for (name, value) in body
        .as_object()
        .into_iter()
        .flat_map(|object| object.iter())
    {
        if recognized.contains(&name.as_str()) {
            continue;
        }
        let criticality = if name == "cachedContent" {
            ExtensionCriticality::Critical
        } else {
            ExtensionCriticality::Advisory
        };
        extensions.push(provider_extension(
            name,
            value.clone(),
            context,
            model,
            criticality,
        ));
    }
    if let Some(config) = body.get("generationConfig").and_then(Value::as_object) {
        let known = [
            "maxOutputTokens",
            "temperature",
            "topP",
            "topK",
            "frequencyPenalty",
            "presencePenalty",
            "seed",
            "stopSequences",
            "responseMimeType",
            "responseJsonSchema",
            "_responseJsonSchema",
            "responseSchema",
            "responseModalities",
            "thinkingConfig",
        ];
        for (name, value) in config {
            if !known.contains(&name.as_str()) {
                extensions.push(provider_extension(
                    &ProviderExtension::nested_name(&format!("$.generationConfig.{name}")),
                    value.clone(),
                    context,
                    model,
                    ExtensionCriticality::Advisory,
                ));
            }
        }
    }
    if let Some(system) = body.get("systemInstruction") {
        scan_gemini_content(
            &mut extensions,
            system,
            "$.systemInstruction",
            context,
            model,
        );
    }
    for (content_index, content) in body
        .get("contents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        scan_gemini_content(
            &mut extensions,
            content,
            &format!("$.contents[{content_index}]"),
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
        for (declaration_index, declaration) in tool
            .get("functionDeclarations")
            .or_else(|| tool.get("function_declarations"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            collect_nested_unknown_fields(
                &mut extensions,
                "gemini_native",
                Some(declaration),
                &[
                    "name",
                    "description",
                    "parameters",
                    "parametersJsonSchema",
                    "parameters_json_schema",
                    "response",
                    "responseJsonSchema",
                    "response_json_schema",
                    "behavior",
                ],
                &format!("$.tools[{tool_index}].functionDeclarations[{declaration_index}]"),
                context,
                model,
            );
        }
    }
    collect_nested_unknown_fields(
        &mut extensions,
        "gemini_native",
        body.get("toolConfig"),
        &["functionCallingConfig", "retrievalConfig"],
        "$.toolConfig",
        context,
        model,
    );
    collect_nested_unknown_fields(
        &mut extensions,
        "gemini_native",
        body.pointer("/toolConfig/functionCallingConfig"),
        &["mode", "allowedFunctionNames"],
        "$.toolConfig.functionCallingConfig",
        context,
        model,
    );
    extensions
}

fn scan_gemini_content(
    extensions: &mut Vec<ProviderExtension>,
    content: &Value,
    path: &str,
    context: &AdapterContext,
    model: &str,
) {
    collect_nested_unknown_fields(
        extensions,
        "gemini_native",
        Some(content),
        &["role", "parts"],
        path,
        context,
        model,
    );
    for (part_index, part) in content
        .get("parts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let part_path = format!("{path}.parts[{part_index}]");
        collect_nested_unknown_fields(
            extensions,
            "gemini_native",
            Some(part),
            &[
                "text",
                "thought",
                "thoughtSignature",
                "thought_signature",
                "functionCall",
                "function_call",
                "functionResponse",
                "function_response",
                "inlineData",
                "inline_data",
                "fileData",
                "file_data",
            ],
            &part_path,
            context,
            model,
        );
        for (container, recognized) in [
            (
                "functionCall",
                &["name", "args", "arguments", "id"] as &[&str],
            ),
            ("function_call", &["name", "args", "arguments", "id"]),
            ("functionResponse", &["name", "response", "id"]),
            ("function_response", &["name", "response", "id"]),
            ("inlineData", &["mimeType", "data"]),
            ("inline_data", &["mime_type", "data"]),
            ("fileData", &["mimeType", "fileUri"]),
            ("file_data", &["mime_type", "file_uri"]),
        ] {
            collect_nested_unknown_fields(
                extensions,
                "gemini_native",
                part.get(container),
                recognized,
                &format!("{part_path}.{container}"),
                context,
                model,
            );
        }
    }
}

fn provider_extension(
    name: &str,
    value: Value,
    context: &AdapterContext,
    model: &str,
    criticality: ExtensionCriticality,
) -> ProviderExtension {
    ProviderExtension {
        namespace: "gemini_native".to_string(),
        name: name.to_string(),
        value,
        affinity: if criticality == ExtensionCriticality::Critical {
            exact_issuer_affinity(context, model)
        } else {
            protocol_affinity(ProtocolKind::GeminiNative)
        },
        criticality,
    }
}

fn decode_finish(candidate: &Value) -> FinishDetail {
    let original = candidate
        .get("finishReason")
        .and_then(Value::as_str)
        .map(str::to_string);
    let reason = match original.as_deref() {
        None | Some("") | Some("FINISH_REASON_UNSPECIFIED") => FinishReason::Unknown,
        Some("STOP") => FinishReason::Stop,
        Some("MAX_TOKENS") => FinishReason::Length,
        Some("SAFETY")
        | Some("RECITATION")
        | Some("BLOCKLIST")
        | Some("PROHIBITED_CONTENT")
        | Some("SPII")
        | Some("IMAGE_SAFETY")
        | Some("IMAGE_PROHIBITED_CONTENT")
        | Some("IMAGE_RECITATION") => FinishReason::ContentFilter,
        Some("MALFORMED_FUNCTION_CALL")
        | Some("UNEXPECTED_TOOL_CALL")
        | Some("TOO_MANY_TOOL_CALLS")
        | Some("MISSING_THOUGHT_SIGNATURE")
        | Some("MALFORMED_RESPONSE") => FinishReason::Error,
        Some(_) => FinishReason::Unknown,
    };
    FinishDetail {
        reason,
        original_reason: original,
        incomplete_details: candidate
            .get("finishMessage")
            .cloned()
            .map(|message| json!({"finish_message":message})),
    }
}

fn decode_usage(value: Option<&Value>) -> Result<Usage, AdapterError> {
    let value = value.unwrap_or(&Value::Null);
    let mut usage = Usage {
        input_tokens: usage_value(value, "promptTokenCount")?,
        output_tokens: usage_value(value, "candidatesTokenCount")?,
        total_tokens: usage_value(value, "totalTokenCount")?,
        cache_read_tokens: usage_value(value, "cachedContentTokenCount")?,
        reasoning_tokens: usage_value(value, "thoughtsTokenCount")?,
        ..Default::default()
    };
    apply_modality_usage(
        value.get("promptTokensDetails"),
        &mut usage.text_input_units,
        &mut usage.image_input_units,
        &mut usage.audio_input_units,
        &mut usage.video_input_units,
    )?;
    apply_modality_usage(
        value.get("candidatesTokensDetails"),
        &mut usage.text_output_units,
        &mut usage.image_output_units,
        &mut usage.audio_output_units,
        &mut usage.video_output_units,
    )?;
    if let Some(count) = value.get("toolUsePromptTokenCount").and_then(Value::as_i64) {
        usage.provider_billable_units.insert(
            "gemini_tool_use_prompt_tokens".to_string(),
            UsageValue::reported(count).map_err(|error| {
                AdapterError::new(
                    error.code(),
                    "$.usageMetadata.toolUsePromptTokenCount",
                    error.to_string(),
                )
            })?,
        );
    }
    Ok(usage.with_separate_reasoning_output())
}

fn usage_value(value: &Value, name: &str) -> Result<UsageValue, AdapterError> {
    let Some(raw) = value.get(name).and_then(Value::as_i64) else {
        return Ok(UsageValue::default());
    };
    UsageValue::reported(raw).map_err(|error| {
        AdapterError::new(
            error.code(),
            format!("$.usageMetadata.{name}"),
            error.to_string(),
        )
    })
}

fn apply_modality_usage(
    details: Option<&Value>,
    text: &mut UsageValue,
    image: &mut UsageValue,
    audio: &mut UsageValue,
    video: &mut UsageValue,
) -> Result<(), AdapterError> {
    for detail in details.and_then(Value::as_array).into_iter().flatten() {
        let Some(count) = detail.get("tokenCount").and_then(Value::as_i64) else {
            continue;
        };
        let value = UsageValue::reported(count).map_err(|error| {
            AdapterError::new(error.code(), "$.usageMetadata.modality", error.to_string())
        })?;
        match detail.get("modality").and_then(Value::as_str) {
            Some("TEXT") => *text = value,
            Some("IMAGE") => *image = value,
            Some("AUDIO") => *audio = value,
            Some("VIDEO") => *video = value,
            _ => {}
        }
    }
    Ok(())
}

fn decode_response_extensions(
    body: &Value,
    candidate: &Value,
    context: &AdapterContext,
    model: &str,
) -> Vec<ProviderExtension> {
    let mut extensions = Vec::new();
    let mut candidate_metadata = candidate.as_object().cloned().unwrap_or_default();
    candidate_metadata.remove("content");
    candidate_metadata.remove("finishReason");
    candidate_metadata.remove("finishMessage");
    candidate_metadata.remove("index");
    if !candidate_metadata.is_empty() {
        extensions.push(provider_extension(
            "candidate_metadata",
            Value::Object(candidate_metadata),
            context,
            model,
            ExtensionCriticality::Advisory,
        ));
    }
    for name in ["promptFeedback", "modelStatus", "createTime"] {
        if let Some(value) = body.get(name) {
            extensions.push(provider_extension(
                name,
                value.clone(),
                context,
                model,
                ExtensionCriticality::Advisory,
            ));
        }
    }
    let recognized = [
        "candidates",
        "usageMetadata",
        "modelVersion",
        "responseId",
        "promptFeedback",
        "modelStatus",
        "createTime",
        "error",
    ];
    for (name, value) in body
        .as_object()
        .into_iter()
        .flat_map(|object| object.iter())
    {
        if recognized.contains(&name.as_str()) {
            continue;
        }
        extensions.push(provider_extension(
            name,
            value.clone(),
            context,
            model,
            ExtensionCriticality::Advisory,
        ));
    }
    if let Some(candidates) = body.get("candidates").and_then(Value::as_array) {
        if candidates.len() > 1 {
            extensions.push(provider_extension(
                "additional_candidates",
                Value::Array(candidates[1..].to_vec()),
                context,
                model,
                ExtensionCriticality::Advisory,
            ));
        }
    }
    scan_gemini_response_content(
        &mut extensions,
        candidate.get("content"),
        "$.candidates[0].content",
        context,
        model,
    );
    scan_gemini_usage_extensions(
        &mut extensions,
        body.get("usageMetadata"),
        "$.usageMetadata",
        context,
        model,
    );
    collect_nested_unknown_fields(
        &mut extensions,
        "gemini_native",
        body.get("error"),
        &["code", "message", "status", "details"],
        "$.error",
        context,
        model,
    );
    extensions
}

fn scan_gemini_response_content(
    extensions: &mut Vec<ProviderExtension>,
    value: Option<&Value>,
    path: &str,
    context: &AdapterContext,
    model: &str,
) {
    collect_nested_unknown_fields(
        extensions,
        "gemini_native",
        value,
        &["role", "parts"],
        path,
        context,
        model,
    );
    for (part_index, part) in value
        .and_then(|content| content.get("parts"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let part_path = format!("{path}.parts[{part_index}]");
        // Top-level part extras are already retained as ProviderArtifact by decode_parts. Only
        // inspect nested containers here so each actual omission produces exactly one warning.
        for (container, recognized) in [
            (
                "functionCall",
                &["name", "args", "arguments", "id"] as &[&str],
            ),
            ("function_call", &["name", "args", "arguments", "id"]),
            ("functionResponse", &["name", "response", "id"]),
            ("function_response", &["name", "response", "id"]),
            ("inlineData", &["mimeType", "data"]),
            ("inline_data", &["mime_type", "data"]),
            ("fileData", &["mimeType", "fileUri"]),
            ("file_data", &["mime_type", "file_uri"]),
        ] {
            collect_nested_unknown_fields(
                extensions,
                "gemini_native",
                part.get(container),
                recognized,
                &format!("{part_path}.{container}"),
                context,
                model,
            );
        }
    }
}

fn scan_gemini_usage_extensions(
    extensions: &mut Vec<ProviderExtension>,
    value: Option<&Value>,
    path: &str,
    context: &AdapterContext,
    model: &str,
) {
    collect_nested_unknown_fields(
        extensions,
        "gemini_native",
        value,
        &[
            "promptTokenCount",
            "candidatesTokenCount",
            "totalTokenCount",
            "cachedContentTokenCount",
            "thoughtsTokenCount",
            "promptTokensDetails",
            "candidatesTokensDetails",
            "toolUsePromptTokenCount",
        ],
        path,
        context,
        model,
    );
    for details_name in ["promptTokensDetails", "candidatesTokensDetails"] {
        for (detail_index, detail) in value
            .and_then(|usage| usage.get(details_name))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let detail_path = format!("{path}.{details_name}[{detail_index}]");
            collect_nested_unknown_fields(
                extensions,
                "gemini_native",
                Some(detail),
                &["modality", "tokenCount"],
                &detail_path,
                context,
                model,
            );
            if !matches!(
                detail.get("modality").and_then(Value::as_str),
                Some("TEXT" | "IMAGE" | "AUDIO" | "VIDEO") | None
            ) {
                extensions.push(ProviderExtension {
                    namespace: "gemini_native".to_string(),
                    name: ProviderExtension::nested_name(&detail_path),
                    value: detail.clone(),
                    affinity: protocol_affinity(ProtocolKind::GeminiNative),
                    criticality: ExtensionCriticality::Advisory,
                });
            }
        }
    }
}

fn decode_error_response(body: &Value, context: &AdapterContext) -> CanonicalResponseV2 {
    let error = body.get("error").unwrap_or(&Value::Null);
    let http_code = error.get("code").and_then(Value::as_u64).unwrap_or(500);
    let provider_code = error
        .get("status")
        .and_then(Value::as_str)
        .map(str::to_string);
    CanonicalResponseV2 {
        id: None,
        model: None,
        blocks: Vec::new(),
        status: ResponseStatus::Failed,
        finish: FinishDetail {
            reason: FinishReason::Error,
            original_reason: provider_code.clone(),
            incomplete_details: None,
        },
        usage: Usage::default(),
        error: Some(CanonicalError {
            code: "gemini_error".to_string(),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Gemini request failed")
                .to_string(),
            retryable: http_code == 429 || http_code >= 500,
            provider_code,
            details: Some(json!({
                "http_code":http_code,
                "details":error.get("details"),
            })),
        }),
        extensions: decode_response_extensions(body, &Value::Null, context, ""),
    }
}

fn encode_instruction_parts(
    instructions: &[Instruction],
    plan: &ConversionPlan,
) -> Result<Vec<Value>, AdapterError> {
    let mut parts = Vec::new();
    for (instruction_index, instruction) in instructions.iter().enumerate() {
        if instruction.role == InstructionRole::Developer
            && !feature_action_is(
                plan,
                &format!("instructions[{instruction_index}].role"),
                FeatureDisposition::CoalesceRoleEnvelope,
            )
        {
            return Err(AdapterError::new(
                "encoder_plan_violation",
                format!("instructions[{instruction_index}].role"),
                "plan does not authorize developer-to-system coalescing",
            ));
        }
        parts.extend(encode_blocks(
            &instruction.blocks,
            plan,
            &format!("instructions[{instruction_index}].blocks"),
        )?);
    }
    Ok(parts)
}

fn encode_contents(turns: &[Turn], plan: &ConversionPlan) -> Result<Vec<Value>, AdapterError> {
    let mut contents: Vec<Value> = Vec::new();
    for (index, turn) in turns.iter().enumerate() {
        let block_path = format!("turns[{index}].blocks");
        let role = if turn.role == TurnRole::Assistant {
            "model"
        } else {
            "user"
        };

        // Responses represents parallel assistant items as adjacent top-level entries.
        // Gemini requires them to remain one model turn so its external-history bypass
        // signature is attached only once for the whole parallel tool-call step.
        if role == "model" {
            if let Some(previous) = contents.last_mut() {
                if previous.get("role").and_then(Value::as_str) == Some("model") {
                    if let Some(previous_parts) =
                        previous.get_mut("parts").and_then(Value::as_array_mut)
                    {
                        append_encoded_blocks(previous_parts, &turn.blocks, plan, &block_path)?;
                        continue;
                    }
                }
            }
        }
        let parts = encode_blocks(&turn.blocks, plan, &block_path)?;
        contents.push(json!({
            "role": role,
            "parts": parts,
        }));
    }

    for content in &mut contents {
        if content.get("role").and_then(Value::as_str) == Some("model") {
            if let Some(parts) = content.get_mut("parts").and_then(Value::as_array_mut) {
                ensure_first_function_signature(parts);
            }
        }
    }
    Ok(contents)
}

const EXTERNAL_HISTORY_THOUGHT_SIGNATURE: &str = "skip_thought_signature_validator";

fn ensure_first_function_signature(parts: &mut [Value]) {
    let Some(part) = parts
        .iter_mut()
        .find(|part| part.get("functionCall").is_some())
    else {
        return;
    };
    if part.get("thoughtSignature").is_none() {
        part["thoughtSignature"] = json!(EXTERNAL_HISTORY_THOUGHT_SIGNATURE);
    }
}

fn encode_blocks(
    blocks: &[ContentBlock],
    plan: &ConversionPlan,
    base_path: &str,
) -> Result<Vec<Value>, AdapterError> {
    let mut parts = Vec::new();
    append_encoded_blocks(&mut parts, blocks, plan, base_path)?;
    Ok(parts)
}

fn append_encoded_blocks(
    parts: &mut Vec<Value>,
    blocks: &[ContentBlock],
    plan: &ConversionPlan,
    base_path: &str,
) -> Result<(), AdapterError> {
    for (index, block) in blocks.iter().enumerate() {
        let path = format!("{base_path}[{index}]");
        if matches!(block, ContentBlock::Reasoning(_))
            && plan.omits_feature_at(Feature::Reasoning, &path)
        {
            continue;
        }
        match block {
            ContentBlock::Text(text) => parts.push(json!({"text":text.text})),
            ContentBlock::Refusal(refusal) => parts.push(json!({"text":refusal.text})),
            ContentBlock::Reasoning(reasoning) => {
                let mut part = json!({
                    "thought": true,
                    "text": reasoning.text.clone().unwrap_or_default(),
                });
                attach_planned_signature(
                    &mut part,
                    &reasoning.artifacts,
                    plan,
                    &format!("{path}.artifacts"),
                );
                parts.push(part);
            }
            ContentBlock::Image(media)
            | ContentBlock::Audio(media)
            | ContentBlock::Video(media) => parts.push(encode_media(&media.source, &path)?),
            ContentBlock::File(file) => parts.push(encode_media(&file.source, &path)?),
            ContentBlock::ToolCall(call) => {
                let function_call = json!({
                    "id":call.id,
                    "name":call.name,
                    "args":call.arguments.clone().unwrap_or_else(|| json!({})),
                });
                let mut part = json!({"functionCall":function_call});
                attach_planned_signature(
                    &mut part,
                    &call.artifacts,
                    plan,
                    &format!("{path}.artifacts"),
                );
                parts.push(part);
            }
            ContentBlock::ToolResult(result) => {
                let name = result
                    .name
                    .clone()
                    .unwrap_or_else(|| "tool_result".to_string());
                let mut response = tool_result_response(&result.content);
                if result.is_error && response.get("error").is_none() {
                    response = json!({"error":response});
                }
                parts.push(json!({
                    "functionResponse": {
                        "id": result.call_id,
                        "name": name,
                        "response": response,
                    }
                }));
            }
            ContentBlock::ProviderArtifact(artifact)
                if artifact.kind == ArtifactKind::GeminiThoughtSignature =>
            {
                if artifact_is_preserved(plan, &path) {
                    let previous = parts.last_mut().ok_or_else(|| {
                        AdapterError::new(
                            "gemini_signature_position_invalid",
                            path.clone(),
                            "thought signature has no preceding Gemini part",
                        )
                    })?;
                    previous["thoughtSignature"] = artifact.payload.clone();
                }
            }
            ContentBlock::ProviderArtifact(artifact)
                if artifact_is_preserved(plan, &path)
                    && gemini_part_extra(&artifact.payload).is_some() =>
            {
                if let Some(extra) = gemini_part_extra(&artifact.payload) {
                    merge_part_extra(
                        parts.last_mut().ok_or_else(|| {
                            AdapterError::new(
                                "gemini_part_extra_position_invalid",
                                path.clone(),
                                "Gemini part metadata has no preceding part",
                            )
                        })?,
                        extra,
                    );
                }
            }
            ContentBlock::ProviderArtifact(artifact)
                if artifact_is_preserved(plan, &path) && is_portable_artifact(&artifact.kind) =>
            {
                let previous = parts.last_mut().ok_or_else(|| {
                    AdapterError::new(
                        "continuation_carrier_binding_invalid",
                        path.clone(),
                        "continuation state has no preceding Gemini part",
                    )
                })?;
                if let Some(carrier) =
                    encode_carrier(previous_part_entries(std::slice::from_ref(artifact)))
                {
                    previous["thoughtSignature"] = json!(carrier);
                }
            }
            ContentBlock::ProviderArtifact(artifact) if artifact_is_preserved(plan, &path) => {
                parts.push(artifact.payload.clone());
            }
            ContentBlock::ProviderArtifact(_) => {}
        }
    }
    Ok(())
}

fn attach_planned_signature(
    part: &mut Value,
    artifacts: &[OpaqueArtifact],
    plan: &ConversionPlan,
    base_path: &str,
) {
    if let Some((_, artifact)) = artifacts.iter().enumerate().find(|(index, artifact)| {
        artifact.kind == ArtifactKind::GeminiThoughtSignature
            && artifact_is_preserved(plan, &format!("{base_path}[{index}]"))
    }) {
        part["thoughtSignature"] = artifact.payload.clone();
    }
}

fn artifact_is_preserved(plan: &ConversionPlan, path: &str) -> bool {
    plan.artifact_actions()
        .iter()
        .any(|action| action.path == path && action.action == ArtifactDisposition::Preserve)
}

fn feature_action_is(plan: &ConversionPlan, path: &str, disposition: FeatureDisposition) -> bool {
    plan.feature_actions()
        .iter()
        .any(|action| action.path == path && action.action == disposition)
}

fn tool_result_response(content: &[ContentBlock]) -> Value {
    if let [ContentBlock::Text(text)] = content {
        return serde_json::from_str(&text.text).unwrap_or_else(|_| json!({"content":text.text}));
    }
    let text = content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    json!({"content":text})
}

fn gemini_part_extra(payload: &Value) -> Option<&Map<String, Value>> {
    payload.get("gemini_part_extra").and_then(Value::as_object)
}

fn merge_part_extra(part: &mut Value, extra: &Map<String, Value>) {
    if let Some(part) = part.as_object_mut() {
        part.extend(extra.clone());
    }
}

fn encode_media(source: &MediaSource, path: &str) -> Result<Value, AdapterError> {
    match source {
        MediaSource::InlineBase64 { media_type, data } => Ok(json!({
            "inlineData":{"mimeType":media_type,"data":data}
        })),
        MediaSource::ProviderFileId {
            provider,
            id,
            media_type,
        } if matches!(provider.as_str(), "gemini" | "google") => Ok(json!({
            "fileData":{
                "mimeType":media_type.as_deref().unwrap_or_else(|| guess_media_type(id)),
                "fileUri":id
            }
        })),
        MediaSource::ProviderFileId { provider, .. } => Err(AdapterError::new(
            "provider_file_incompatible",
            path,
            format!("{provider} file IDs are not Gemini file URIs"),
        )),
        MediaSource::RemoteUrl { .. } => Err(AdapterError::new(
            "remote_url_not_gemini_file",
            path,
            "Gemini fileData requires a Gemini file URI, not a generic remote URL",
        )),
    }
}

fn guess_media_type(id: &str) -> &'static str {
    if id.contains("video") {
        "video/mp4"
    } else if id.contains("audio") {
        "audio/mpeg"
    } else if id.contains("image") {
        "image/png"
    } else {
        "application/octet-stream"
    }
}

fn encode_tools(tools: &[ToolDefinition], plan: &ConversionPlan) -> Result<Value, AdapterError> {
    let tools = if tools
        .iter()
        .any(|tool| matches!(tool, ToolDefinition::Namespace(_)))
    {
        if !plan
            .feature_actions()
            .iter()
            .any(|action| action.action == FeatureDisposition::FlattenNamespace)
        {
            return Err(AdapterError::new(
                "encoder_plan_violation",
                "$.tools",
                "plan does not authorize namespace flattening",
            ));
        }
        let flattened = flatten_namespace_tools(tools).map_err(|error| {
            AdapterError::new("namespace_flatten_failed", "$.tools", error.to_string())
        })?;
        let mut rewritten = flattened
            .tools
            .into_iter()
            .map(ToolDefinition::Function)
            .collect::<Vec<_>>();
        rewritten.extend(
            tools
                .iter()
                .filter(|tool| matches!(tool, ToolDefinition::Hosted(_)))
                .cloned(),
        );
        rewritten
    } else {
        tools.to_vec()
    };
    let mut declarations = Vec::new();
    let mut output = Vec::new();
    for tool in tools {
        match tool {
            ToolDefinition::Function(tool) => {
                let input_schema = if plan.report().source_protocol == ProtocolKind::GeminiNative {
                    tool.input_schema
                } else {
                    prepare_gemini_tool_schema(tool.input_schema)
                };
                declarations.push(json!({
                    "name":tool.name,
                    "description":tool.description,
                    "parametersJsonSchema":input_schema,
                }));
            }
            ToolDefinition::Hosted(tool) => {
                let name = tool.name.unwrap_or_else(|| match tool.kind {
                    HostedToolKind::WebSearch => "googleSearch".to_string(),
                    HostedToolKind::FileSearch => "fileSearch".to_string(),
                    HostedToolKind::CodeExecution => "codeExecution".to_string(),
                    HostedToolKind::ComputerUse => "computerUse".to_string(),
                    HostedToolKind::UrlContext => "urlContext".to_string(),
                    HostedToolKind::Mcp => "mcp".to_string(),
                    HostedToolKind::ProviderSpecific => "providerTool".to_string(),
                });
                output.push(json!({name:tool.config}));
            }
            ToolDefinition::Custom(_) | ToolDefinition::Namespace(_) => {
                return Err(AdapterError::new(
                    "encoder_plan_violation",
                    "$.tools",
                    "Gemini Native cannot encode this tool definition",
                ));
            }
        }
    }
    if !declarations.is_empty() {
        output.insert(0, json!({"functionDeclarations":declarations}));
    }
    Ok(Value::Array(output))
}

fn prepare_gemini_tool_schema(mut schema: Value) -> Value {
    repair_missing_gemini_array_items(&mut schema);
    schema
}

fn repair_missing_gemini_array_items(schema: &mut Value) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };

    let declares_array = match object.get("type") {
        Some(Value::String(kind)) => kind == "array",
        Some(Value::Array(kinds)) => kinds.iter().any(|kind| kind.as_str() == Some("array")),
        _ => false,
    };
    if declares_array && !object.contains_key("items") {
        object.insert("items".to_string(), json!({"type":"string"}));
    }

    for key in [
        "items",
        "additionalProperties",
        "contains",
        "not",
        "if",
        "then",
        "else",
        "propertyNames",
        "unevaluatedProperties",
        "unevaluatedItems",
        "contentSchema",
        "additionalItems",
    ] {
        if let Some(child) = object.get_mut(key) {
            repair_missing_gemini_array_items(child);
        }
    }
    for key in ["anyOf", "oneOf", "allOf", "prefixItems"] {
        if let Some(children) = object.get_mut(key).and_then(Value::as_array_mut) {
            for child in children {
                repair_missing_gemini_array_items(child);
            }
        }
    }
    for key in [
        "properties",
        "patternProperties",
        "$defs",
        "definitions",
        "dependentSchemas",
    ] {
        if let Some(children) = object.get_mut(key).and_then(Value::as_object_mut) {
            for child in children.values_mut() {
                repair_missing_gemini_array_items(child);
            }
        }
    }
}

fn encode_tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!({"mode":"AUTO"}),
        ToolChoice::None => json!({"mode":"NONE"}),
        ToolChoice::Required => json!({"mode":"ANY"}),
        ToolChoice::Named { name } => {
            json!({"mode":"ANY","allowedFunctionNames":[name]})
        }
        ToolChoice::Allowed { mode, names } => json!({
            "mode":if *mode == AllowedMode::Required { "ANY" } else { "AUTO" },
            "allowedFunctionNames":names,
        }),
    }
}

fn encode_generation(body: &mut Value, request: &CanonicalRequestV2) -> Result<(), AdapterError> {
    let generation = &request.generation;
    let mut config = Map::new();
    insert_option(&mut config, "maxOutputTokens", generation.max_output_tokens);
    insert_option(&mut config, "temperature", generation.temperature);
    insert_option(&mut config, "topP", generation.top_p);
    insert_option(&mut config, "topK", generation.top_k);
    insert_option(
        &mut config,
        "frequencyPenalty",
        generation.frequency_penalty,
    );
    insert_option(&mut config, "presencePenalty", generation.presence_penalty);
    insert_option(&mut config, "seed", generation.seed);
    if !generation.stop.is_empty() {
        config.insert("stopSequences".to_string(), json!(generation.stop));
    }
    if !generation.modalities.is_empty() {
        config.insert(
            "responseModalities".to_string(),
            Value::Array(
                generation
                    .modalities
                    .iter()
                    .map(|modality| {
                        Value::String(
                            match modality {
                                OutputModality::Text => "TEXT",
                                OutputModality::Image => "IMAGE",
                                OutputModality::Audio => "AUDIO",
                                OutputModality::Video => "VIDEO",
                            }
                            .to_string(),
                        )
                    })
                    .collect(),
            ),
        );
    }
    match &request.response_format {
        ResponseFormat::Text => {}
        ResponseFormat::JsonObject => {
            config.insert("responseMimeType".to_string(), json!("application/json"));
        }
        ResponseFormat::JsonSchema { schema, .. } => {
            config.insert("responseMimeType".to_string(), json!("application/json"));
            config.insert("responseJsonSchema".to_string(), schema.clone());
        }
    }
    let mut thinking = Map::new();
    if !matches!(request.reasoning.summary, ReasoningSummaryMode::None) {
        thinking.insert("includeThoughts".to_string(), Value::Bool(true));
    }
    if let Some(budget) = request.reasoning.token_budget {
        thinking.insert("thinkingBudget".to_string(), json!(budget));
    } else if request.reasoning.is_explicitly_disabled() {
        // Only an explicit source-protocol disable becomes budget 0. Missing
        // reasoning settings mean "use the target model default"; emitting 0
        // there breaks Gemini models that require thinking.
        thinking.insert("thinkingBudget".to_string(), json!(0));
    }
    if let Some(effort) = request.reasoning.effort {
        // Gemini's documented discrete alphabet stops at HIGH. Preserve its
        // four exact values and clamp only higher foreign-protocol values to
        // that semantic ceiling. Explicit `none` is represented by budget 0.
        // Individual Gemini models expose different subsets; selectors use
        // catalog metadata for that distinction, while direct requests remain
        // the provider's decision instead of being guessed from a model name.
        let level = effort.gemini_level();
        if let Some(level) = level {
            thinking.insert("thinkingLevel".to_string(), json!(level));
        }
    }
    for extension in &request.reasoning.provider_extensions {
        if extension.namespace == "gemini_native" && extension.name == "thinking_config_extra" {
            if let Some(extra) = extension.value.as_object() {
                thinking.extend(extra.clone());
            }
        }
    }
    if !thinking.is_empty() {
        config.insert("thinkingConfig".to_string(), Value::Object(thinking));
    }
    for extension in &request.extensions {
        if extension.namespace != "gemini_native" || extension.nested_path().is_some() {
            continue;
        }
        if let Some(name) = extension.name.strip_prefix("generationConfig.") {
            config.insert(name.to_string(), extension.value.clone());
        }
    }
    if !config.is_empty() {
        body["generationConfig"] = Value::Object(config);
    }
    if let Some(tier) = &generation.service_tier {
        body["serviceTier"] = json!(tier);
    }
    Ok(())
}

fn insert_option<T: serde::Serialize>(map: &mut Map<String, Value>, name: &str, value: Option<T>) {
    if let Some(value) = value {
        map.insert(name.to_string(), json!(value));
    }
}

fn replay_request_extensions(
    body: &mut Value,
    extensions: &[ProviderExtension],
    context: &AdapterContext,
    model: &str,
) {
    for extension in extensions {
        if extension.namespace != "gemini_native"
            || extension.nested_path().is_some()
            || extension.name.starts_with("generationConfig.")
            || !affinity_matches_context(
                &extension.affinity,
                context,
                model,
                ProtocolKind::GeminiNative,
            )
        {
            continue;
        }
        body[&extension.name] = extension.value.clone();
    }
}

fn encode_response_parts(
    blocks: &[ContentBlock],
    context: &AdapterContext,
    model: &str,
) -> Result<Vec<Value>, AdapterError> {
    let mut parts = Vec::new();
    for block in blocks {
        match block {
            ContentBlock::Text(text) => parts.push(json!({"text":text.text})),
            ContentBlock::Refusal(refusal) => parts.push(json!({"text":refusal.text})),
            ContentBlock::Reasoning(reasoning) => {
                let mut part = json!({
                    "thought":true,
                    "text":reasoning.text.clone().unwrap_or_default(),
                });
                attach_response_signature(
                    &mut part,
                    &reasoning.artifacts,
                    reasoning_entries(&reasoning.artifacts),
                    context,
                    model,
                );
                parts.push(part);
            }
            ContentBlock::Image(media)
            | ContentBlock::Audio(media)
            | ContentBlock::Video(media) => {
                parts.push(encode_media(&media.source, "response.blocks")?)
            }
            ContentBlock::File(file) => parts.push(encode_media(&file.source, "response.blocks")?),
            ContentBlock::ToolCall(call) => {
                let function_call = json!({
                    "id":call.id,
                    "name":call.name,
                    "args":call.arguments.clone().unwrap_or_else(||json!({})),
                });
                let mut part = json!({"functionCall":function_call});
                attach_response_signature(
                    &mut part,
                    &call.artifacts,
                    tool_call_entries(&call.id, &call.artifacts),
                    context,
                    model,
                );
                parts.push(part);
            }
            ContentBlock::ToolResult(result) => parts.push(json!({
                "functionResponse":{
                    "id":result.call_id,
                    "name":result.name.clone().unwrap_or_else(||"tool_result".to_string()),
                    "response":tool_result_response(&result.content),
                }
            })),
            ContentBlock::ProviderArtifact(artifact)
                if artifact.kind == ArtifactKind::GeminiThoughtSignature
                    && affinity_matches_context(
                        &artifact.affinity,
                        context,
                        model,
                        ProtocolKind::GeminiNative,
                    ) =>
            {
                let previous = parts.last_mut().ok_or_else(|| {
                    AdapterError::new(
                        "gemini_signature_position_invalid",
                        "response.blocks",
                        "thought signature has no preceding Gemini part",
                    )
                })?;
                previous["thoughtSignature"] = artifact.payload.clone();
            }
            ContentBlock::ProviderArtifact(artifact)
                if affinity_matches_context(
                    &artifact.affinity,
                    context,
                    model,
                    ProtocolKind::GeminiNative,
                ) && gemini_part_extra(&artifact.payload).is_some() =>
            {
                if let Some(extra) = gemini_part_extra(&artifact.payload) {
                    merge_part_extra(
                        parts.last_mut().ok_or_else(|| {
                            AdapterError::new(
                                "gemini_part_extra_position_invalid",
                                "response.blocks",
                                "Gemini part metadata has no preceding part",
                            )
                        })?,
                        extra,
                    );
                }
            }
            ContentBlock::ProviderArtifact(artifact) if is_portable_artifact(&artifact.kind) => {
                let previous = parts.last_mut().ok_or_else(|| {
                    AdapterError::new(
                        "continuation_carrier_binding_invalid",
                        "response.blocks",
                        "continuation state has no preceding Gemini part",
                    )
                })?;
                if let Some(carrier) =
                    encode_carrier(previous_part_entries(std::slice::from_ref(artifact)))
                {
                    previous["thoughtSignature"] = json!(carrier);
                }
            }
            ContentBlock::ProviderArtifact(artifact)
                if affinity_matches_context(
                    &artifact.affinity,
                    context,
                    model,
                    ProtocolKind::GeminiNative,
                ) =>
            {
                parts.push(artifact.payload.clone())
            }
            ContentBlock::ProviderArtifact(_) => {}
        }
    }
    if parts.is_empty() {
        parts.push(json!({"text":""}));
    }
    Ok(parts)
}

fn attach_response_signature(
    part: &mut Value,
    artifacts: &[OpaqueArtifact],
    carrier_entries: Vec<crate::protocol::continuation::ContinuationEntry>,
    context: &AdapterContext,
    model: &str,
) {
    if let Some(artifact) = artifacts.iter().find(|artifact| {
        artifact.kind == ArtifactKind::GeminiThoughtSignature
            && affinity_matches_context(
                &artifact.affinity,
                context,
                model,
                ProtocolKind::GeminiNative,
            )
    }) {
        part["thoughtSignature"] = artifact.payload.clone();
    } else if let Some(carrier) = encode_carrier(carrier_entries) {
        part["thoughtSignature"] = json!(carrier);
    }
}

fn encode_finish_reason(reason: FinishReason) -> &'static str {
    match reason {
        FinishReason::Stop | FinishReason::ToolCalls => "STOP",
        FinishReason::PauseTurn => "FINISH_REASON_UNSPECIFIED",
        FinishReason::Length => "MAX_TOKENS",
        FinishReason::ContentFilter | FinishReason::Refusal => "SAFETY",
        FinishReason::Error => "OTHER",
        FinishReason::Cancelled | FinishReason::Unknown => "FINISH_REASON_UNSPECIFIED",
    }
}

fn encode_usage(usage: &Usage) -> Value {
    let mut value = json!({});
    insert_usage(&mut value, "promptTokenCount", &usage.input_tokens);
    insert_usage(
        &mut value,
        "candidatesTokenCount",
        &usage.non_reasoning_output(),
    );
    insert_usage(&mut value, "totalTokenCount", &usage.total_tokens);
    insert_usage(
        &mut value,
        "cachedContentTokenCount",
        &usage.cache_read_tokens,
    );
    insert_usage(&mut value, "thoughtsTokenCount", &usage.reasoning_tokens);
    if let Some(tool_tokens) = usage
        .provider_billable_units
        .get("gemini_tool_use_prompt_tokens")
    {
        insert_usage(&mut value, "toolUsePromptTokenCount", tool_tokens);
    }
    let prompt = encode_modality_details([
        ("TEXT", &usage.text_input_units),
        ("IMAGE", &usage.image_input_units),
        ("AUDIO", &usage.audio_input_units),
        ("VIDEO", &usage.video_input_units),
    ]);
    if !prompt.is_empty() {
        value["promptTokensDetails"] = Value::Array(prompt);
    }
    let candidates = encode_modality_details([
        ("TEXT", &usage.text_output_units),
        ("IMAGE", &usage.image_output_units),
        ("AUDIO", &usage.audio_output_units),
        ("VIDEO", &usage.video_output_units),
    ]);
    if !candidates.is_empty() {
        value["candidatesTokensDetails"] = Value::Array(candidates);
    }
    value
}

fn insert_usage(target: &mut Value, name: &str, usage: &UsageValue) {
    if let Some(value) = usage.value {
        target[name] = json!(value);
    }
}

fn encode_modality_details<'a>(
    values: impl IntoIterator<Item = (&'static str, &'a UsageValue)>,
) -> Vec<Value> {
    values
        .into_iter()
        .filter_map(|(modality, usage)| {
            usage
                .value
                .map(|token_count| json!({"modality":modality,"tokenCount":token_count}))
        })
        .collect()
}

fn replay_response_extensions(
    body: &mut Value,
    candidate: &mut Value,
    extensions: &[ProviderExtension],
    context: &AdapterContext,
    model: &str,
) {
    for extension in extensions {
        if extension.namespace != "gemini_native"
            || !affinity_matches_context(
                &extension.affinity,
                context,
                model,
                ProtocolKind::GeminiNative,
            )
        {
            continue;
        }
        match extension.name.as_str() {
            "candidate_metadata" => {
                if let Some(values) = extension.value.as_object() {
                    for (name, value) in values {
                        candidate[name] = value.clone();
                    }
                }
            }
            "additional_candidates" => {
                if let Some(values) = extension.value.as_array() {
                    if let Some(candidates) = body
                        .get_mut("candidates")
                        .and_then(serde_json::Value::as_array_mut)
                    {
                        candidates.extend(values.clone());
                    }
                }
            }
            name => body[name] = extension.value.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::GeminiNativeAdapter;
    use crate::protocol::adapters::{AdapterContext, ProtocolAdapter, ProviderDialect};
    use crate::protocol::capability::CapabilityProfile;
    use crate::protocol::conversion::{ConversionPolicy, IssuerIdentity, plan_conversion};
    use crate::protocol::ir::{
        ArtifactAffinity, ArtifactKind, ContentBlock, FinishReason, HostedToolKind, MediaSource,
        ReasoningEffort, ReasoningMode, ResponseFormat, ResponseStatus, ToolChoice, ToolDefinition,
    };
    use crate::protocol::kind::ProtocolKind;

    fn fixture() -> Value {
        serde_json::from_str(include_str!(
            "../../../tests/fixtures/protocol-v2/gemini-native/advanced.json"
        ))
        .unwrap()
    }

    fn basic_fixture() -> Value {
        serde_json::from_str(include_str!(
            "../../../tests/fixtures/protocol-v2/gemini-native/basic-tools.json"
        ))
        .unwrap()
    }

    fn context(account: &str) -> AdapterContext {
        AdapterContext {
            dialect: ProviderDialect::Compatible {
                provider: "google".to_string(),
            },
            issuer: IssuerIdentity {
                provider: "google".to_string(),
                endpoint_fingerprint: "generativelanguage.googleapis.com".to_string(),
                account_fingerprint: Some(account.to_string()),
                model_family: Some("gemini-test".to_string()),
            },
        }
    }

    #[test]
    fn decodes_ordered_parts_media_tools_signatures_config_and_usage() {
        let adapter = GeminiNativeAdapter;
        let fixture = fixture();
        let request = adapter
            .decode_request(&fixture["request"], &context("account-1"))
            .unwrap();

        assert_eq!(request.model, "gemini-test");
        assert_eq!(request.instructions.len(), 1);
        assert_eq!(request.turns.len(), 5);
        assert!(matches!(request.turns[0].blocks[1], ContentBlock::Image(_)));
        let ContentBlock::Video(video) = &request.turns[0].blocks[2] else {
            panic!("video file part");
        };
        assert!(matches!(
            video.source,
            MediaSource::ProviderFileId { ref provider, ref id, .. }
                if provider == "gemini" && id == "files/asset-1"
        ));
        let ContentBlock::ToolCall(first_call) = &request.turns[1].blocks[0] else {
            panic!("first function call");
        };
        assert_eq!(first_call.id, "call-weather");
        assert_eq!(
            first_call.artifacts[0].kind,
            ArtifactKind::GeminiThoughtSignature
        );
        assert_eq!(first_call.artifacts[0].payload, "sig-parallel");
        assert!(matches!(
            first_call.artifacts[0].affinity,
            ArtifactAffinity::ExactIssuer { .. }
        ));
        let ContentBlock::ToolCall(second_call) = &request.turns[1].blocks[1] else {
            panic!("parallel function call");
        };
        assert!(second_call.artifacts.is_empty());
        assert!(matches!(
            request.turns[2].blocks[0],
            ContentBlock::ToolResult(_)
        ));
        assert!(matches!(
            request.turns[3].blocks[0],
            ContentBlock::Reasoning(_)
        ));
        assert_eq!(request.tools.len(), 5);
        assert!(request.tools.iter().any(|tool| matches!(
            tool,
            ToolDefinition::Hosted(tool) if tool.kind == HostedToolKind::WebSearch
        )));
        assert!(matches!(request.tool_choice, ToolChoice::Allowed { .. }));
        assert!(matches!(request.reasoning.mode, ReasoningMode::Enabled));
        assert_eq!(request.reasoning.effort, Some(ReasoningEffort::High));
        assert_eq!(request.reasoning.token_budget, Some(1024));
        assert!(matches!(
            request.response_format,
            ResponseFormat::JsonSchema { .. }
        ));

        let response = adapter
            .decode_response(&fixture["response"], &context("account-1"))
            .unwrap();
        assert_eq!(response.id.as_deref(), Some("response-1"));
        assert_eq!(response.model.as_deref(), Some("gemini-test-001"));
        assert_eq!(response.status, ResponseStatus::Completed);
        assert_eq!(response.finish.reason, FinishReason::ToolCalls);
        assert!(matches!(response.blocks[0], ContentBlock::Reasoning(_)));
        assert!(
            response
                .blocks
                .iter()
                .any(|block| matches!(block, ContentBlock::Image(_)))
        );
        let call = response
            .blocks
            .iter()
            .find_map(|block| match block {
                ContentBlock::ToolCall(call) => Some(call),
                _ => None,
            })
            .expect("response function call");
        assert_eq!(call.artifacts[0].payload, "response-call-signature");
        assert_eq!(response.usage.cache_read_tokens.value, Some(4));
        assert_eq!(response.usage.reasoning_tokens.value, Some(5));
        assert_eq!(response.usage.image_input_units.value, Some(2));
        assert_eq!(response.usage.image_output_units.value, Some(2));
    }

    #[test]
    fn reencodes_exact_request_and_response_without_moving_signatures() {
        let adapter = GeminiNativeAdapter;
        let fixture = fixture();
        let request = adapter
            .decode_request(&fixture["request"], &context("account-1"))
            .unwrap();
        let plan = plan_conversion(
            &request,
            ProtocolKind::GeminiNative,
            ProtocolKind::GeminiNative,
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
            encoded["contents"][1]["parts"][0]["thoughtSignature"],
            "sig-parallel"
        );
        assert!(
            encoded["contents"][1]["parts"][1]
                .get("thoughtSignature")
                .is_none()
        );
        assert_eq!(
            encoded["contents"][3]["parts"][0]["thoughtSignature"],
            "sig-sequential"
        );
        assert_eq!(
            encoded["contents"][3]["parts"][1]["thoughtSignature"],
            "sig-source"
        );
        assert_eq!(
            encoded["contents"][0]["parts"][2]["fileData"]["fileUri"],
            "files/asset-1"
        );
        assert_eq!(
            encoded["contents"][0]["parts"][2]["fileData"]["mimeType"],
            "video/mp4"
        );
        assert_eq!(
            encoded["contents"][0]["parts"][2]["videoMetadata"]["startOffset"],
            "1s"
        );
        assert_eq!(
            encoded["generationConfig"]["responseJsonSchema"]["type"],
            "object"
        );
        assert!(
            encoded["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool.get("googleSearch").is_some())
        );
        assert_eq!(encoded["cachedContent"], "cachedContents/context-1");

        let response = adapter
            .decode_response(&fixture["response"], &context("account-1"))
            .unwrap();
        let encoded_response = adapter
            .encode_response(&response, &plan, &context("account-1"))
            .unwrap();
        assert_eq!(
            encoded_response["candidates"][0]["content"]["parts"][0]["thoughtSignature"],
            "response-thought-signature"
        );
        assert_eq!(
            encoded_response["candidates"][0]["content"]["parts"][1]["thoughtSignature"],
            "response-text-signature"
        );
        assert_eq!(encoded_response["usageMetadata"]["thoughtsTokenCount"], 5);
        assert_eq!(
            encoded_response["candidates"][0]["groundingMetadata"]["webSearchQueries"][0],
            "weather Paris"
        );
    }

    #[test]
    fn replaces_foreign_gemini_signatures_with_external_history_bypass_and_decodes_errors() {
        let adapter = GeminiNativeAdapter;
        let fixture = fixture();
        let request = adapter
            .decode_request(&fixture["request"], &context("account-1"))
            .unwrap();
        let plan = plan_conversion(
            &request,
            ProtocolKind::GeminiNative,
            ProtocolKind::GeminiNative,
            &context("account-1").issuer,
            &context("account-2").issuer,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap();
        assert!(
            plan.report()
                .issues
                .iter()
                .any(|issue| issue.code == "artifact_filtered")
        );
        let encoded = adapter
            .encode_request(&request, &plan, &context("account-2"))
            .unwrap();
        assert_eq!(
            encoded["contents"][1]["parts"][0]["thoughtSignature"],
            "skip_thought_signature_validator"
        );

        let response = adapter
            .decode_response(&fixture["error"], &context("account-1"))
            .unwrap();
        assert_eq!(response.status, ResponseStatus::Failed);
        let error = response.error.expect("provider error");
        assert_eq!(error.provider_code.as_deref(), Some("RESOURCE_EXHAUSTED"));
        assert!(error.retryable);
    }

    #[test]
    fn generated_function_ids_are_emitted_on_both_calls_and_results() {
        let adapter = GeminiNativeAdapter;
        let request = adapter
            .decode_request(&basic_fixture()["request"], &context("account-1"))
            .unwrap();
        let plan = plan_conversion(
            &request,
            ProtocolKind::GeminiNative,
            ProtocolKind::GeminiNative,
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
            encoded["contents"][1]["parts"][0]["functionCall"]["id"],
            "call_gemini_0"
        );
        assert_eq!(
            encoded["contents"][2]["parts"][0]["functionResponse"]["id"],
            "call_gemini_0"
        );
    }

    #[test]
    fn cross_protocol_tools_repair_missing_nested_gemini_array_items() {
        let adapter = GeminiNativeAdapter;
        let mut request = adapter
            .decode_request(&basic_fixture()["request"], &context("account-1"))
            .unwrap();
        let ToolDefinition::Function(tool) = &mut request.tools[0] else {
            panic!("function tool");
        };
        tool.input_schema = serde_json::json!({
            "type":"object",
            "properties":{
                "missing":{"type":"array"},
                "defined":{"type":"array","items":{"type":"integer"}},
                "nested":{"anyOf":[{"type":"null"},{"type":"array"}]}
            }
        });
        let plan = plan_conversion(
            &request,
            ProtocolKind::OpenAiResponses,
            ProtocolKind::GeminiNative,
            &context("account-1").issuer,
            &context("account-1").issuer,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap();

        let encoded = adapter
            .encode_request(&request, &plan, &context("account-1"))
            .unwrap();
        let schema = &encoded["tools"][0]["functionDeclarations"][0]["parametersJsonSchema"];
        assert_eq!(schema["properties"]["missing"]["items"]["type"], "string");
        assert_eq!(schema["properties"]["defined"]["items"]["type"], "integer");
        assert_eq!(
            schema["properties"]["nested"]["anyOf"][1]["items"]["type"],
            "string"
        );
    }

    #[test]
    fn native_gemini_tool_schema_is_not_rewritten() {
        let adapter = GeminiNativeAdapter;
        let mut request = adapter
            .decode_request(&basic_fixture()["request"], &context("account-1"))
            .unwrap();
        let ToolDefinition::Function(tool) = &mut request.tools[0] else {
            panic!("function tool");
        };
        tool.input_schema = serde_json::json!({
            "type":"object",
            "properties":{"values":{"type":"array"}}
        });
        let plan = plan_conversion(
            &request,
            ProtocolKind::GeminiNative,
            ProtocolKind::GeminiNative,
            &context("account-1").issuer,
            &context("account-1").issuer,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap();

        let encoded = adapter
            .encode_request(&request, &plan, &context("account-1"))
            .unwrap();
        assert!(
            encoded["tools"][0]["functionDeclarations"][0]["parametersJsonSchema"]["properties"]
                ["values"]
                .get("items")
                .is_none()
        );
    }

    #[test]
    fn safety_blocked_candidate_without_content_is_a_refused_response() {
        let response = GeminiNativeAdapter
            .decode_response(
                &serde_json::json!({
                    "modelVersion":"gemini-test",
                    "candidates":[{
                        "finishReason":"SAFETY",
                        "safetyRatings":[{
                            "category":"HARM_CATEGORY_HARASSMENT",
                            "probability":"HIGH"
                        }]
                    }]
                }),
                &context("account-1"),
            )
            .unwrap();

        assert_eq!(response.status, ResponseStatus::Refused);
        assert_eq!(response.finish.reason, FinishReason::ContentFilter);
        assert!(response.blocks.is_empty());
    }

    #[test]
    fn prompt_feedback_without_candidates_is_a_refused_response() {
        let response = GeminiNativeAdapter
            .decode_response(
                &serde_json::json!({
                    "modelVersion":"gemini-test",
                    "promptFeedback":{
                        "blockReason":"SAFETY",
                        "blockReasonMessage":"blocked prompt"
                    },
                    "usageMetadata":{"promptTokenCount":3,"totalTokenCount":3}
                }),
                &context("account-1"),
            )
            .unwrap();

        assert_eq!(response.status, ResponseStatus::Refused);
        assert_eq!(response.finish.reason, FinishReason::ContentFilter);
        assert_eq!(response.usage.input_tokens.value, Some(3));
    }
}
