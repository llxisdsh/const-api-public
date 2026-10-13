use super::wire as w;
use crate::protocol::{
    adapters::AdapterContext,
    conversion::{ToolWireMap, bridge_request_tools},
    ir::*,
    kind::ProtocolKind,
};
use anyhow::{Result, anyhow, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

#[derive(Clone)]
pub(crate) struct Tool {
    pub(crate) name: String,
    pub(super) definition: Vec<u8>,
    descriptor: Vec<u8>,
}
pub(crate) struct Prepared {
    pub(crate) model: String,
    pub(crate) fast: bool,
    pub(crate) tool_mapping: ToolWireMap,
    pub(crate) adjustments: Vec<String>,
    pub(crate) scope: String,
    pub(super) run: Vec<u8>,
    pub(super) blobs: HashMap<Vec<u8>, Vec<u8>>,
    pub(super) tools: Vec<Tool>,
    pub(super) results: Vec<(String, Vec<u8>)>,
}

#[cfg(test)]
pub(crate) fn prepare(
    protocol: ProtocolKind,
    body: &Value,
    model: &str,
    identity: &str,
) -> Result<Prepared> {
    prepare_with_models(protocol, body, model, identity, &[model.to_string()])
}

pub(crate) fn prepare_with_models(
    protocol: ProtocolKind,
    body: &Value,
    model: &str,
    identity: &str,
    catalog: &[String],
) -> Result<Prepared> {
    let context = AdapterContext::runtime("cursor", "cursor_agent", None, model);
    let mut request = crate::protocol::decode_request(protocol, body, &context)?;
    for field in ["service_tier", "speed"] {
        ensure!(
            body.get(field).is_none_or(|v| v.is_null() || v.is_string()),
            "{field} must be a string"
        );
    }
    let tier = body.get("service_tier").and_then(Value::as_str);
    let speed = body.get("speed").and_then(Value::as_str);
    // Cursor has no UltraFast/Flex contract. Do not disguise either as Fast.
    ensure!(
        tier.is_none_or(|value| matches!(
            value,
            "" | "auto" | "default" | "standard" | "standard_only" | "fast" | "priority"
        )),
        "capability_mismatch: Cursor does not expose the requested service tier"
    );
    ensure!(
        speed.is_none_or(|value| matches!(value, "standard" | "fast")),
        "capability_mismatch: Cursor does not expose the requested speed"
    );
    let fast = super::models::fast_variant(model, catalog)
        || speed == Some("fast")
        || matches!(tier, Some("fast" | "priority"));
    let model = super::models::resolve(catalog, model, &request.reasoning, fast)?;
    let actual_fast = super::models::fast_variant(model, catalog);
    request.model = model.into();
    // Definitions have already been promoted into request.tools by the decoder.
    request.turns.retain(|turn| {
        !matches!(turn.blocks.as_slice(), [ContentBlock::ProviderArtifact(artifact)]
            if artifact.payload.get("type").and_then(Value::as_str) == Some("additional_tools"))
    });
    let tool_mapping = bridge_request_tools(&mut request)?;
    request.validate()?;
    ensure!(
        request
            .generation
            .modalities
            .iter()
            .all(|m| *m == OutputModality::Text)
            && request.generation.audio.is_none(),
        "capability_mismatch: Cursor does not expose media generation"
    );
    ensure!(
        !request
            .extensions
            .iter()
            .any(|e| e.criticality == ExtensionCriticality::Critical),
        "capability_mismatch: Cursor cannot preserve a required provider extension"
    );
    let mut adjustments = Vec::new();
    if fast && !actual_fast {
        adjustments
            .push("Cursor Fast route is unavailable; using the standard model and tariff".into());
    }
    let mut instructions = request
        .instructions
        .iter()
        .map(|i| text(&i.blocks))
        .collect::<Result<Vec<_>>>()?;
    instructions.push("Use the provided CONST MCP tools for external actions. Local filesystem, shell and browser access are unavailable in this environment.".into());
    if let Some(limit) = request.generation.max_output_tokens {
        instructions.push(format!("Keep this response within {limit} output tokens."));
        adjustments.push("Cursor output length is instruction-based; its agent interface has no verified hard token limit".into());
    }
    if request.generation.temperature.is_some()
        || request.generation.top_p.is_some()
        || request.generation.top_k.is_some()
        || request.generation.seed.is_some()
        || request.generation.frequency_penalty.is_some()
        || request.generation.presence_penalty.is_some()
        || !request.generation.stop.is_empty()
        || request.generation.latency_mode.is_some()
        || request.generation.verbosity.is_some()
        || request.generation.parallel_tool_calls.is_some()
    {
        adjustments.push("Cursor controls sampling and agent execution; unsupported generation controls use provider defaults".into());
    }
    if request.reasoning.token_budget.is_some() {
        adjustments
            .push("Cursor uses model effort rather than a fixed reasoning token budget".into());
    }
    let mut tools = Vec::new();
    for tool in &request.tools {
        let ToolDefinition::Function(tool) = tool else {
            anyhow::bail!("capability_mismatch: Cursor cannot execute hosted/provider tools");
        };
        let schema = w::json(&tool.input_schema);
        let description = tool.description.as_deref().unwrap_or("");
        tools.push(Tool {
            name: tool.name.clone(),
            definition: [
                w::bytes(1, &tool.name),
                w::bytes(2, description),
                w::bytes(3, &schema),
                w::bytes(4, "const"),
                w::bytes(5, &tool.name),
            ]
            .concat(),
            descriptor: [
                w::bytes(1, &tool.name),
                w::bytes(3, description),
                w::bytes(4, &schema),
            ]
            .concat(),
        });
    }
    // Stable native definitions/context also keep a pending H2 tool run
    // reusable when a caller enumerates the same tool set in a different order.
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    match &request.tool_choice {
        ToolChoice::None => tools.clear(),
        ToolChoice::Auto => {}
        ToolChoice::Named { name } => {
            tools.retain(|t| t.name == *name);
            ensure!(!tools.is_empty(), "Cursor named tool is not declared");
            instructions.push(format!("Call {name} for the next response."));
            adjustments.push("Cursor named tool choice uses tool filtering and instructions, not constrained sampling".into());
        }
        ToolChoice::Required => {
            ensure!(!tools.is_empty(), "Cursor required tool list is empty");
            instructions.push("Call one of the provided tools for the next response.".into());
            adjustments.push("Cursor required tool choice is instruction-based".into());
        }
        ToolChoice::Allowed { names, mode } => {
            tools.retain(|t| names.contains(&t.name));
            ensure!(!tools.is_empty(), "Cursor allowed tool list is empty");
            if *mode == AllowedMode::Required {
                instructions.push("Call one of the provided tools for the next response.".into());
                adjustments.push("Cursor required tool choice is instruction-based".into());
            }
        }
    }
    match &request.response_format {
        ResponseFormat::Text => {}
        ResponseFormat::JsonObject => {
            instructions.push("Return only a valid JSON object without markdown fences.".into());
            adjustments.push("Cursor JSON output is instruction-based".into());
        }
        ResponseFormat::JsonSchema { schema, strict, .. } => {
            ensure!(
                *strict != Some(true),
                "capability_mismatch: Cursor has no strict JSON-schema sampling"
            );
            instructions.push(format!("Return only JSON matching this schema: {schema}"));
            adjustments.push("Cursor JSON schema is instruction-based".into());
        }
    }
    let system = instructions.join("\n\n");
    let mut blobs = HashMap::new();
    // Cursor replaces root system messages and only expands RequestContext
    // rules for an empty transcript. Keep caller instructions in a stable
    // user-rules message, including when reconstructing an existing history.
    // This is an agent-rule adaptation, not an overriding system prompt.
    let rules = json!({"role":"user", "content":[{
        "type":"text", "text":format!("<user_rules>\n{system}\n</user_rules>")
    }]});
    let mut state = vec![
        w::int(10, 1),
        w::bytes(1, store_blob(&mut blobs, serde_json::to_vec(&rules)?)?),
    ];
    let results: HashMap<_, _> = request
        .turns
        .iter()
        .flat_map(|t| &t.blocks)
        .filter_map(|b| match b {
            ContentBlock::ToolResult(r) => Some(r),
            _ => None,
        })
        .map(|r| Ok((r.call_id.clone(), mcp_result(r)?)))
        .collect::<Result<_>>()?;
    let last = request
        .turns
        .last()
        .ok_or_else(|| anyhow!("Cursor request has no messages"))?;
    let continuation = last
        .blocks
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolResult(_)));
    // A new user instruction/image accompanying a result needs a fresh native
    // run; replying only to the pending MCP call would silently lose it.
    let resume_only = last
        .blocks
        .iter()
        .all(|b| matches!(b, ContentBlock::ToolResult(_)));
    let latest_results = last
        .blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolResult(r) if resume_only => {
                Some((r.call_id.clone(), results[&r.call_id].clone()))
            }
            _ => None,
        })
        .collect();
    let call_names: HashMap<_, _> = request
        .turns
        .iter()
        .flat_map(|turn| &turn.blocks)
        .filter_map(|block| match block {
            ContentBlock::ToolCall(call) => Some((call.id.as_str(), call.name.as_str())),
            _ => None,
        })
        .collect();
    // root_prompt_messages_json is the native model transcript. The separate
    // turns index is for agent/UI history, not sufficient as model context.
    let mut history = Vec::new();
    let mut current = None;
    for (index, turn) in request.turns.iter().enumerate() {
        match turn.role {
            TurnRole::User => {
                let is_current = index + 1 == request.turns.len();
                let mut content = Vec::new();
                for block in &turn.blocks {
                    if let ContentBlock::ToolResult(result) = block {
                        if !content.is_empty() {
                            history.push(json!({"role":"user", "content":content}));
                            content.clear();
                        }
                        let name = call_names
                            .get(result.call_id.as_str())
                            .ok_or_else(|| anyhow!("Cursor tool result has no matching call"))?;
                        history.push(json!({"role":"tool", "content":[{
                            "type":"tool-result", "toolCallId":result.call_id,
                            "toolName":name, "result":text(&result.content)?, "isError":result.is_error
                        }]}));
                    } else if matches!(block, ContentBlock::ProviderArtifact(a)
                        if a.payload.get("type").and_then(Value::as_str) == Some("additional_tools"))
                    {
                        continue;
                    } else if !is_current {
                        content.push(history_content(block)?);
                    }
                }
                if !content.is_empty() {
                    history.push(json!({"role":"user", "content":content}));
                }
                if is_current
                    && turn
                        .blocks
                        .iter()
                        .any(|b| !matches!(b, ContentBlock::ToolResult(_)))
                {
                    current = Some(user_message(&turn.blocks, index, &mut blobs)?);
                }
            }
            TurnRole::Assistant => {
                let mut content = Vec::new();
                for block in &turn.blocks {
                    let part = match block {
                        ContentBlock::Text(t) => json!({"type":"text", "text":t.text}),
                        ContentBlock::Refusal(t) => json!({"type":"text", "text":t.text}),
                        ContentBlock::ToolCall(call) => {
                            ensure!(
                                call.kind == ToolKind::Function,
                                "capability_mismatch: Cursor history requires function tools"
                            );
                            let arguments = call
                                .arguments
                                .clone()
                                .or_else(|| {
                                    call.raw_arguments
                                        .as_deref()
                                        .and_then(|v| serde_json::from_str(v).ok())
                                })
                                .ok_or_else(|| {
                                    anyhow!("incomplete Cursor historical tool arguments")
                                })?;
                            ensure!(
                                arguments.is_object(),
                                "Cursor tool arguments must be an object"
                            );
                            ensure!(
                                results.contains_key(&call.id),
                                "Cursor history has an unresolved tool call"
                            );
                            json!({"type":"tool-call", "toolCallId":call.id, "toolName":call.name, "args":arguments})
                        }
                        ContentBlock::Reasoning(r) if !r.encrypted => {
                            json!({"type":"reasoning", "text":r.text.as_deref().unwrap_or("")})
                        }
                        _ => anyhow::bail!(
                            "capability_mismatch: Cursor cannot preserve this assistant artifact"
                        ),
                    };
                    content.push(part);
                }
                history.push(json!({"role":"assistant", "content":content}));
            }
        }
    }
    for message in history {
        state.push(w::bytes(
            1,
            store_blob(&mut blobs, serde_json::to_vec(&message)?)?,
        ));
    }
    let current = match current {
        Some(user) => user,
        None if continuation => {
            // Used only if the original connection is gone (restart/reroute).
            // Imported tool messages retain the original call/result pairs.
            [
                w::bytes(1, "Continue from the supplied tool results."),
                w::bytes(2, uuid::Uuid::new_v4().to_string()),
                w::bytes(3, []),
                w::int(4, 1),
            ]
            .concat()
        }
        None => anyhow::bail!("Cursor generation requires a user message or tool result"),
    };
    let action = vec![w::bytes(1, current)];
    let tool_bytes: Vec<_> = tools.iter().map(|t| w::bytes(1, &t.definition)).collect();
    let scope = hex::encode(Sha256::digest(serde_json::to_vec(&(
        identity,
        model,
        &system,
        &tool_bytes,
    ))?));
    let conversation = hex::encode(Sha256::digest(identity.as_bytes()));
    let model_details = [w::bytes(1, model), w::bytes(3, model), w::bytes(4, model)];
    let mut requested_model = vec![w::bytes(1, model)];
    if let Some(effort) = request.reasoning.effort {
        let family = model.rsplit('/').next().unwrap_or(model);
        let parameter = if family.starts_with("gpt-") {
            Some("reasoning")
        } else if family.starts_with("claude-") || family.starts_with("grok-") {
            Some("effort")
        } else {
            None
        };
        let tier_is_in_id = super::models::encoded_effort(model).is_some();
        if let Some(parameter) = parameter.filter(|_| !tier_is_in_id) {
            requested_model.push(w::message(
                3,
                &[w::bytes(1, parameter), w::bytes(2, effort.openai_name())],
            ));
        } else {
            adjustments.push("Cursor uses the selected model's reasoning configuration; no unverified parameter was added".into());
        }
    }
    let run = w::message(
        1,
        &[
            w::message(1, &state),
            w::message(2, &[w::message(1, &action)]),
            w::message(3, &model_details),
            w::message(4, &tool_bytes),
            w::bytes(5, &conversation),
            // Cursor rebuilds its system prompt; custom_system_prompt is
            // account-gated. Caller rules are in the stable model transcript.
            w::message(9, &requested_model),
            // Excluding workspace context is account/model-gated. Use the
            // ordinary Agent contract; context() still supplies no workspace,
            // and the runtime never executes tools on the supplier's machine.
            w::int(12, 0),
            w::bytes(16, &conversation),
            w::int(19, 1),
        ],
    );
    ensure!(
        run.len() <= w::MAX_FRAME,
        "Cursor request exceeds frame limit"
    );
    Ok(Prepared {
        model: super::models::public_id_in_catalog(model, catalog),
        fast: actual_fast,
        tool_mapping,
        adjustments,
        scope,
        run,
        blobs,
        tools,
        results: latest_results,
    })
}

fn store_blob(blobs: &mut HashMap<Vec<u8>, Vec<u8>>, value: Vec<u8>) -> Result<Vec<u8>> {
    ensure!(
        value.len() <= w::MAX_FRAME,
        "Cursor history blob is too large"
    );
    let id = Sha256::digest(&value).to_vec();
    blobs.insert(id.clone(), value);
    Ok(id)
}

fn text(blocks: &[ContentBlock]) -> Result<String> {
    blocks
        .iter()
        .map(|b| match b {
            ContentBlock::Text(t) => Ok(t.text.clone()),
            ContentBlock::Refusal(t) => Ok(t.text.clone()),
            _ => anyhow::bail!("capability_mismatch: Cursor expected text content"),
        })
        .collect::<Result<Vec<_>>>()
        .map(|v| v.join("\n\n"))
}

fn history_content(block: &ContentBlock) -> Result<Value> {
    match block {
        ContentBlock::Text(text) => Ok(json!({"type":"text", "text":text.text})),
        ContentBlock::Image(image) => {
            use base64::Engine;
            let MediaSource::InlineBase64 { data, media_type } = &image.source else {
                anyhow::bail!("capability_mismatch: Cursor images require inline base64 data");
            };
            let raw = base64::engine::general_purpose::STANDARD.decode(data)?;
            ensure!(raw.len() <= w::MAX_FRAME, "Cursor image is too large");
            Ok(
                json!({"type":"image", "image":{"__type":"Uint8Array", "hex":hex::encode(raw)}, "mimeType":media_type}),
            )
        }
        _ => anyhow::bail!("capability_mismatch: Cursor cannot preserve this history content"),
    }
}

fn user_message(
    blocks: &[ContentBlock],
    index: usize,
    blobs: &mut HashMap<Vec<u8>, Vec<u8>>,
) -> Result<Vec<u8>> {
    use base64::Engine;
    let mut texts = Vec::new();
    let mut images = Vec::new();
    for block in blocks {
        match block {
            ContentBlock::Text(t) => texts.push(t.text.clone()),
            ContentBlock::Image(image) => {
                let MediaSource::InlineBase64 { data, media_type } = &image.source else {
                    anyhow::bail!("capability_mismatch: Cursor images require inline base64 data");
                };
                let raw = base64::engine::general_purpose::STANDARD.decode(data)?;
                ensure!(raw.len() <= w::MAX_FRAME, "Cursor image is too large");
                let hash = Sha256::digest(&raw).to_vec();
                images.push(w::message(
                    1,
                    &[
                        w::bytes(2, hex::encode(&hash)),
                        w::bytes(7, media_type),
                        w::message(9, &[w::bytes(1, &hash), w::bytes(2, &raw)]),
                    ],
                ));
                blobs.insert(hash, raw);
            }
            ContentBlock::ToolResult(_) => {}
            ContentBlock::ProviderArtifact(a)
                if a.payload.get("type").and_then(Value::as_str) == Some("additional_tools") => {}
            _ => anyhow::bail!("capability_mismatch: Cursor cannot preserve this user artifact"),
        }
    }
    let text = texts.join("\n\n");
    let id = hex::encode(Sha256::digest(format!("{index}\0{text}")));
    Ok([
        w::bytes(1, text),
        w::bytes(2, id),
        w::message(3, &images),
        w::int(4, 1),
    ]
    .concat())
}

fn mcp_result(result: &ToolResult) -> Result<Vec<u8>> {
    Ok(w::message(
        1,
        &[
            w::message(1, &[w::message(1, &[w::bytes(1, text(&result.content)?)])]),
            w::int(2, u64::from(result.is_error)),
        ],
    ))
}

pub(super) fn context(tools: &[Tool]) -> Vec<u8> {
    let mut parts: Vec<_> = tools.iter().map(|t| w::bytes(7, &t.definition)).collect();
    if !tools.is_empty() {
        let mut descriptor = vec![w::bytes(1, "const"), w::bytes(2, "const")];
        descriptor.extend(tools.iter().map(|t| w::bytes(5, &t.descriptor)));
        // RequestContext.mcp_meta_tool_options (current CLI schema).
        parts.push(w::message(34, &[w::int(1, 1), w::message(2, &descriptor)]));
    }
    parts.concat()
}

pub(super) fn mcp_state(tools: &[Tool]) -> Vec<u8> {
    let mut server = vec![
        w::bytes(1, "const"),
        w::bytes(2, "const"),
        w::bytes(7, "ready"),
    ];
    server.extend(tools.iter().map(|t| w::bytes(5, &t.definition)));
    w::message(1, &[w::message(1, &server)])
}
