//! Request-only adaptation; no global conversation/history store. A supplied
//! cache identity is already caller-scoped by CONST's local/platform ingress.
use crate::protocol::{
    adapters::AdapterContext,
    conversion::{
        CompatibleAction, TargetRules, ToolWireMap, bridge_request_tools,
        validate_and_normalize_transcript,
    },
    ir::{
        ArtifactKind, CanonicalRequestV2, ContentBlock, ExtensionCriticality, InstructionRole,
        MediaSource, ReasoningMode, ResponseFormat, ToolChoice, ToolDefinition, ToolKind, TurnRole,
    },
    kind::ProtocolKind,
};
use anyhow::{Result, anyhow, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub(crate) struct PreparedRequest {
    pub(crate) body: Value,
    pub(crate) tool_mapping: ToolWireMap,
    pub(crate) adjustments: Vec<String>,
}

pub(crate) fn prepare(
    protocol: ProtocolKind,
    body: &Value,
    model: &str,
    profile: &str,
    cache_identity: Option<&str>,
    model_schema: Option<&Value>,
) -> Result<PreparedRequest> {
    super::profile_region(profile)?;
    let context = AdapterContext::runtime(
        "kiro",
        "kiro_v3",
        Some(hex::encode(Sha256::digest(profile))),
        model,
    );
    let mut request = crate::protocol::decode_request(protocol, body, &context)?;
    request.model = model.into();
    let tool_mapping = bridge_request_tools(&mut request)?;
    if protocol == ProtocolKind::OpenAiResponses {
        // The Responses adapter already validates/promotes these declarations.
        // Their original placement is only needed for Responses passthrough.
        for turn in &mut request.turns {
            turn.blocks.retain(|block| {
                !matches!(block, ContentBlock::ProviderArtifact(artifact)
                if artifact.payload.get("type").and_then(Value::as_str) == Some("additional_tools"))
            });
        }
        request.turns.retain(|turn| !turn.blocks.is_empty());
    }
    let mut rules = TargetRules::new(protocol, ProtocolKind::AnthropicMessages)
        .require_alternating_roles()
        .group_parallel_tool_results();
    rules.allow(CompatibleAction::CoalesceRoleEnvelope);
    validate_and_normalize_transcript(&mut request, &rules)?;
    request.validate()?;
    ensure!(
        request
            .generation
            .modalities
            .iter()
            .all(|m| *m == crate::protocol::ir::OutputModality::Text)
            && request.generation.audio.is_none(),
        "capability_mismatch: Kiro does not expose media generation"
    );
    ensure!(
        request
            .turns
            .last()
            .is_some_and(|t| t.role == TurnRole::User),
        "capability_mismatch: Kiro requires a final user message or tool result"
    );
    for extension in &request.extensions {
        ensure!(
            extension.criticality != ExtensionCriticality::Critical,
            "capability_mismatch: Kiro cannot express required request extension {}",
            extension.name
        );
    }
    let mut adjustments = Vec::new();
    if request.metadata.prompt_cache.ttl.is_some()
        || request.metadata.prompt_cache.legacy_retention.is_some()
    {
        adjustments.push(
            "Kiro controls prompt-cache retention; explicit retention settings are not forwarded"
                .into(),
        );
    }
    let mut instructions = Vec::new();
    for instruction in &request.instructions {
        let text = text_blocks(&instruction.blocks)?;
        if !text.is_empty() {
            let role = match instruction.role {
                InstructionRole::System => "System",
                InstructionRole::Developer => "Developer",
            };
            instructions.push(format!(
                "[{role} instructions]\n{text}\n[End {role} instructions]"
            ));
        }
    }
    if !instructions.is_empty() {
        adjustments
            .push("Kiro carries system/developer instructions in the first user message".into());
    }
    let mut current_instructions = Vec::new();
    let mut tools = Vec::new();
    for tool in &request.tools {
        let ToolDefinition::Function(tool) = tool else {
            anyhow::bail!("capability_mismatch: Kiro cannot execute hosted/provider tools");
        };
        ensure!(
            tool.name.len() <= 64,
            "capability_mismatch: Kiro tool name exceeds 64 bytes"
        );
        tools.push(json!({"toolSpecification":{"name":tool.name,
            "description":tool.description.as_deref().unwrap_or(""),"inputSchema":{"json":tool.input_schema}}}));
        if tool.cache_policy.is_some() {
            adjustments.push(
                "Kiro controls prompt caching; explicit cache breakpoints are not forwarded".into(),
            );
        }
    }
    match &request.tool_choice {
        ToolChoice::Auto => {}
        ToolChoice::None => tools.clear(),
        ToolChoice::Named { name } => {
            tools.retain(|t| t["toolSpecification"]["name"].as_str() == Some(name));
            ensure!(!tools.is_empty(), "named Kiro tool is not declared");
            current_instructions.push(format!("For the next response, call the tool {name}."));
            adjustments.push("Kiro named tool choice is enforced by the available tool set and instructions, not constrained sampling".into());
        }
        ToolChoice::Required => {
            ensure!(!tools.is_empty(), "required Kiro tool list is empty");
            current_instructions.push("For the next response, call an available tool.".into());
            adjustments.push(
                "Kiro required tool choice is instruction-based, not constrained sampling".into(),
            );
        }
        ToolChoice::Allowed { mode, names } => {
            tools.retain(|t| {
                t["toolSpecification"]["name"]
                    .as_str()
                    .is_some_and(|n| names.iter().any(|v| v == n))
            });
            ensure!(!tools.is_empty(), "allowed Kiro tool list is empty");
            if *mode == crate::protocol::ir::AllowedMode::Required {
                current_instructions.push("For the next response, call an available tool.".into());
                adjustments.push(
                    "Kiro required tool choice is instruction-based, not constrained sampling"
                        .into(),
                );
            }
        }
    }
    match &request.response_format {
        ResponseFormat::Text => {}
        ResponseFormat::JsonObject => {
            current_instructions
                .push("Return only one valid JSON object, without markdown fences.".into());
            adjustments
                .push("Kiro JSON output is instruction-based, not constrained sampling".into());
        }
        ResponseFormat::JsonSchema { schema, strict, .. } => {
            ensure!(
                *strict != Some(true),
                "capability_mismatch: Kiro does not expose strict JSON-schema sampling"
            );
            current_instructions.push(format!(
                "Return only JSON matching this schema, without markdown fences:\n{schema}"
            ));
            adjustments
                .push("Kiro JSON schema is instruction-based, not constrained sampling".into());
        }
    }
    // These definitions become a native prompt prefix. Caller enumeration
    // order must not churn that prefix when the available tools are unchanged.
    tools.sort_by(|a, b| {
        a["toolSpecification"]["name"]
            .as_str()
            .cmp(&b["toolSpecification"]["name"].as_str())
    });

    let conversation = cache_identity
        .filter(|v| !v.is_empty())
        .map(|identity| stable_id(&["conversation", profile, model, identity]))
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let conversation = format!("sess_{conversation}");
    // A tool-result round belongs to the preceding real user turn. Hash the
    // transcript prefix, not the changing result or the latest tool-call ID.
    let user_index = request
        .turns
        .iter()
        .rposition(|t| {
            t.role == TurnRole::User
                && !t
                    .blocks
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolResult(_)))
        })
        .ok_or_else(|| anyhow!("Kiro tool continuation is missing its user turn"))?;
    let prefix = serde_json::to_string(&request.turns[..=user_index])?;
    let continuation = stable_id(&["continuation", &conversation, &prefix]);
    let mut history = Vec::new();
    let mut first_user = true;
    for (index, turn) in request.turns.iter().enumerate() {
        let value = match turn.role {
            TurnRole::User => {
                let mut content = Vec::new();
                let mut images = Vec::new();
                let mut results = Vec::new();
                if first_user {
                    content.extend(instructions.iter().cloned());
                    first_user = false;
                }
                for block in &turn.blocks {
                    match block {
                        ContentBlock::Text(t) => content.push(t.text.clone()),
                        ContentBlock::Image(image) => images.push(image_value(&image.source)?),
                        ContentBlock::ToolResult(result) => {
                            let mut result_text = Vec::new();
                            for block in &result.content {
                                match block {
                                    ContentBlock::Text(t) => {
                                        result_text.push(json!({"text":t.text}))
                                    }
                                    ContentBlock::Image(image) => {
                                        images.push(image_value(&image.source)?);
                                        result_text.push(json!({"text":format!("[image {} attached to this message for tool result {}]", images.len(), result.call_id)}));
                                    }
                                    _ => anyhow::bail!(
                                        "capability_mismatch: Kiro tool result contains unsupported media or artifacts"
                                    ),
                                }
                            }
                            if result_text.is_empty() {
                                result_text.push(json!({"text":""}));
                            }
                            results.push(json!({"toolUseId":result.call_id,"content":result_text,
                                "status":if result.is_error {"error"} else {"success"}}));
                        }
                        _ => anyhow::bail!(
                            "capability_mismatch: Kiro user message contains unsupported media or artifacts"
                        ),
                    }
                }
                // Output/tool-choice controls apply only to this response.
                // Rewriting the first historical turn would invalidate the
                // entire reusable prefix on every change of these controls.
                if index + 1 == request.turns.len() {
                    content.extend(current_instructions.iter().cloned());
                }
                if images.len() > 1 {
                    content.insert(
                        0,
                        format!(
                            "[{} attached images follow in numbered order.]",
                            images.len()
                        ),
                    );
                }
                let mut user =
                    json!({"content":content.join("\n\n"),"modelId":model,"origin":"AI_EDITOR"});
                if !images.is_empty() {
                    user["images"] = json!(images);
                }
                if !results.is_empty() {
                    user["userInputMessageContext"] = json!({"toolResults":results});
                }
                json!({"userInputMessage":user})
            }
            TurnRole::Assistant => {
                let mut texts = Vec::new();
                let mut calls = Vec::new();
                let mut reasoning = None;
                for block in &turn.blocks {
                    match block {
                        ContentBlock::Text(t) => texts.push(t.text.clone()),
                        ContentBlock::Refusal(t) => texts.push(t.text.clone()),
                        ContentBlock::ToolCall(call) => {
                            ensure!(
                                call.kind == ToolKind::Function,
                                "capability_mismatch: Kiro history requires JSON function tools"
                            );
                            let input = call
                                .arguments
                                .clone()
                                .or_else(|| {
                                    call.raw_arguments
                                        .as_deref()
                                        .and_then(|raw| serde_json::from_str(raw).ok())
                                })
                                .ok_or_else(|| {
                                    anyhow!("Kiro history contains incomplete tool arguments")
                                })?;
                            calls.push(json!({"toolUseId":call.id,"name":call.name,"input":input}));
                        }
                        ContentBlock::Reasoning(r) => {
                            ensure!(
                                reasoning.is_none(),
                                "capability_mismatch: Kiro accepts one reasoning envelope per assistant message"
                            );
                            for artifact in &r.artifacts {
                                let native_protocol = if model.starts_with("claude-") {
                                    ProtocolKind::AnthropicMessages
                                } else {
                                    ProtocolKind::OpenAiResponses
                                };
                                ensure!(
                                    crate::protocol::conversion::affinity_permits(
                                        &artifact.affinity,
                                        native_protocol,
                                        &context.issuer,
                                        model,
                                    ) && artifact.replay
                                        != crate::protocol::ir::ReplayPolicy::Never,
                                    "capability_mismatch: Kiro cannot replay another issuer or model's reasoning state"
                                );
                                match artifact.kind {
                                    ArtifactKind::AnthropicThinkingSignature => {
                                        reasoning = Some(json!({"reasoningText":{
                                        "text":r.text.as_deref().unwrap_or(""),"signature":artifact.payload}}))
                                    }
                                    ArtifactKind::AnthropicRedactedThinking
                                    | ArtifactKind::OpenAiEncryptedReasoning => {
                                        reasoning =
                                            Some(json!({"redactedContent":artifact.payload}));
                                    }
                                    _ => anyhow::bail!(
                                        "capability_mismatch: Kiro cannot replay this reasoning artifact"
                                    ),
                                }
                            }
                            if reasoning.is_none() && r.text.as_ref().is_some_and(|t| !t.is_empty())
                            {
                                adjustments.push("Unsigned reasoning history is not replayed as assistant output".into());
                            }
                        }
                        _ => anyhow::bail!(
                            "capability_mismatch: unsupported Kiro assistant history block"
                        ),
                    }
                }
                let mut assistant = json!({"content":texts.join("\n\n")});
                if !calls.is_empty() {
                    assistant["toolUses"] = json!(calls);
                }
                if let Some(reasoning) = reasoning {
                    assistant["reasoningContent"] = reasoning;
                }
                json!({"assistantResponseMessage":assistant})
            }
        };
        history.push(value);
    }
    let mut current = history.pop().expect("final user turn checked above");
    if !tools.is_empty() {
        let user = &mut current["userInputMessage"];
        if user.get("userInputMessageContext").is_none() {
            user["userInputMessageContext"] = json!({});
        }
        user["userInputMessageContext"]["tools"] = json!(tools);
    }
    let mut body = json!({"profileArn":profile,"agentMode":"vibe","conversationState":{
        "conversationId":conversation,"rootConversationId":conversation,"agentContinuationId":continuation,
        "agentTaskType":"vibe","chatTriggerType":"MANUAL","currentMessage":current}});
    if !history.is_empty() {
        body["conversationState"]["history"] = json!(history);
    }
    let additional = generation_fields(&request, model_schema, &mut adjustments);
    if !additional
        .as_object()
        .expect("generation object")
        .is_empty()
    {
        body["additionalModelRequestFields"] = additional;
    }
    adjustments.sort();
    adjustments.dedup();
    Ok(PreparedRequest {
        body,
        tool_mapping,
        adjustments,
    })
}

fn stable_id(parts: &[&str]) -> String {
    let digest = Sha256::digest(parts.join("\0").as_bytes());
    uuid::Uuid::from_bytes(digest[..16].try_into().expect("SHA256 has 32 bytes")).to_string()
}

fn text_blocks(blocks: &[ContentBlock]) -> Result<String> {
    blocks
        .iter()
        .map(|block| match block {
            ContentBlock::Text(t) => Ok(t.text.as_str()),
            _ => Err(anyhow!(
                "capability_mismatch: Kiro instructions must be text"
            )),
        })
        .collect::<Result<Vec<_>>>()
        .map(|v| v.join("\n\n"))
}

fn image_value(source: &MediaSource) -> Result<Value> {
    let MediaSource::InlineBase64 { media_type, data } = source else {
        anyhow::bail!(
            "capability_mismatch: Kiro requires inline images; remote URLs and provider file IDs are not fetched"
        );
    };
    let format = match media_type.as_str() {
        "image/jpeg" => "jpeg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => anyhow::bail!("capability_mismatch: unsupported Kiro image format"),
    };
    Ok(json!({"format":format,"source":{"bytes":data}}))
}

fn generation_fields(
    request: &CanonicalRequestV2,
    schema: Option<&Value>,
    adjustments: &mut Vec<String>,
) -> Value {
    let mut fields = json!({});
    let claude = request.model.starts_with("claude-");
    if claude {
        if let Some(limit) = request.generation.max_output_tokens {
            fields["max_tokens"] = json!(limit);
        }
        if request.reasoning.is_explicitly_disabled() {
            fields["thinking"] = json!({"type":"disabled"});
        } else if matches!(
            request.reasoning.mode,
            ReasoningMode::Enabled | ReasoningMode::Adaptive | ReasoningMode::Automatic
        ) {
            fields["thinking"] = json!({"type":"adaptive"});
            if request.reasoning.token_budget.is_some() {
                adjustments.push(
                    "Kiro uses adaptive thinking instead of a fixed reasoning token budget".into(),
                );
            }
        }
        if let Some(effort) = request
            .reasoning
            .effort
            .filter(|_| !request.reasoning.is_explicitly_disabled())
        {
            if let Some(effort) = effort.anthropic_name() {
                fields["output_config"] = json!({"effort":effort});
            }
        }
    } else {
        if request.model.starts_with("gpt-") {
            if let Some(effort) = request.reasoning.effort {
                fields["reasoning"] = json!({"effort":effort.openai_name()});
            }
        }
        if request.generation.max_output_tokens.is_some() {
            adjustments.push("Kiro's non-Claude transport does not expose a verified output-token limit; the account model limit applies".into());
        }
    }
    if request.generation.temperature.is_some()
        || request.generation.top_p.is_some()
        || request.generation.top_k.is_some()
        || request.generation.seed.is_some()
        || request.generation.frequency_penalty.is_some()
        || request.generation.presence_penalty.is_some()
        || !request.generation.stop.is_empty()
    {
        adjustments
            .push("Kiro does not expose these sampling controls; provider defaults apply".into());
    }
    if request.generation.service_tier.is_some()
        || request.generation.latency_mode.is_some()
        || request.generation.verbosity.is_some()
        || request.generation.parallel_tool_calls.is_some()
    {
        adjustments.push("Kiro does not expose the requested service, verbosity or tool-parallelism controls; provider defaults apply".into());
    }
    // Optional controls are a Kiro model contract, not an Anthropic-family
    // guarantee. Older catalogs/models without a schema use provider defaults.
    retain_supported_fields(&mut fields, schema, "", adjustments);
    fields
}

fn retain_supported_fields(
    fields: &mut Value,
    schema: Option<&Value>,
    prefix: &str,
    adjustments: &mut Vec<String>,
) {
    fields.as_object_mut().expect("generation object").retain(|key, value| {
        let path = format!("{prefix}{key}");
        let property = schema.and_then(|s| s.get("properties")).and_then(|p| p.get(key));
        let Some(property) = property else {
            adjustments.push(format!("Kiro model does not declare {path}; provider defaults apply"));
            return false;
        };
        if value.is_object() {
            retain_supported_fields(value, Some(property), &format!("{path}."), adjustments);
            return !value.as_object().expect("generation object").is_empty();
        }
        if property.get("enum").and_then(Value::as_array).is_some_and(|values| !values.contains(value)) {
            adjustments.push(format!("Kiro model does not support the requested {path}; provider defaults apply"));
            return false;
        }
        if let Some(number) = value.as_u64() {
            let bounded = number
                .max(property.get("minimum").and_then(Value::as_u64).unwrap_or(0))
                .min(property.get("maximum").and_then(Value::as_u64).unwrap_or(u64::MAX));
            if number != bounded {
                adjustments.push(format!("Kiro {path} adjusted from {number} to the model's supported limit {bounded}"));
                *value = json!(bounded);
            }
        }
        true
    });
}
