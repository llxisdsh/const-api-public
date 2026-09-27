#[cfg(test)]
pub(crate) fn responses_sse_to_chat_sse(raw: &str, model: &str) -> String {
    convert_complete_sse_text(raw, "openai_responses", "openai_chat", model).unwrap_or_default()
}

#[cfg(test)]
pub(crate) fn responses_sse_to_chat_body(raw: &str, model: &str) -> Result<String> {
    sse_body_from_target_as_inbound(raw, "openai_chat", "openai_responses", model)
}

#[cfg(test)]
fn convert_complete_sse_text(
    raw: &str,
    source: &str,
    target: &str,
    model: &str,
) -> Result<String> {
    let events = canonical_stream_events_from_target_sse(raw, source, model)?;
    sse_from_canonical_stream_events(&events, target, model)
}

#[cfg(test)]
pub(crate) fn chat_sse_to_responses_sse(raw: &str, model: &str) -> String {
    convert_complete_sse_text(raw, "openai_chat", "openai_responses", model).unwrap_or_default()
}

#[cfg(test)]
pub(crate) fn find_sse_event_boundary(buffer: &str) -> Option<(usize, usize)> {
    crate::protocol::stream::find_sse_event_boundary(buffer)
}

#[cfg(test)]
pub(crate) fn gemini_native_body_to_antigravity_body(
    body: &str,
    upstream_model: &str,
    project_id: &str,
) -> Result<String> {
    gemini_native_body_to_antigravity_body_with_cache_identity(
        body,
        upstream_model,
        project_id,
        None,
    )
}

pub(crate) fn gemini_native_body_to_antigravity_body_with_cache_identity(
    body: &str,
    upstream_model: &str,
    project_id: &str,
    cache_identity: Option<&str>,
) -> Result<String> {
    // Antigravity keeps the complete native Gemini request under `request`.
    let mut value: serde_json::Value = serde_json::from_str(body)?;
    let session_id = value
        .get("sessionId")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .is_none()
        .then(|| antigravity_stable_session_id(&value, cache_identity));
    if let Some(request) = value.as_object_mut() {
        // Gemini selects buffered versus SSE generation through the RPC action.
        // Antigravity's internal request schema rejects an OpenAI-style body flag.
        request.remove("stream");
        request.remove("safetySettings");
        normalize_antigravity_function_schemas(request);
        if let Some(session_id) = session_id {
            request.insert(
                "sessionId".to_string(),
                serde_json::Value::String(session_id),
            );
        }
    }
    let request_type = if upstream_model.to_ascii_lowercase().contains("image") {
        "image_gen"
    } else {
        "agent"
    };
    let mut payload = serde_json::json!({
        "model": upstream_model,
        "requestId": antigravity_request_id(),
        "userAgent": "antigravity",
        "requestType": request_type,
        "request": value
    });
    if !project_id.trim().is_empty() {
        payload["project"] = serde_json::json!(project_id.trim());
    }
    serde_json::to_string(&payload).map_err(Into::into)
}

fn normalize_antigravity_function_schemas(
    request: &mut serde_json::Map<String, serde_json::Value>,
) {
    let Some(tools) = request
        .get_mut("tools")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    for declaration in tools.iter_mut().filter_map(|tool| {
        tool.get_mut("functionDeclarations")
            .and_then(serde_json::Value::as_array_mut)
    }).flatten() {
        let Some(declaration) = declaration.as_object_mut() else {
            continue;
        };
        if !declaration.contains_key("parameters") {
            if let Some(schema) = declaration.remove("parametersJsonSchema") {
                declaration.insert("parameters".to_string(), schema);
            } else if let Some(schema) = declaration.remove("parameters_json_schema") {
                declaration.insert("parameters".to_string(), schema);
            }
        }
    }
}

fn antigravity_stable_session_id(
    request: &serde_json::Value,
    cache_identity: Option<&str>,
) -> String {
    use sha2::{Digest, Sha256};

    let explicit = cache_identity
        .map(str::trim)
        .filter(|value| {
            !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
        })
        .map(str::as_bytes);
    let derived;
    let seed = if let Some(explicit) = explicit {
        explicit
    } else if let Some(first_user) = request
        .get("contents")
        .and_then(serde_json::Value::as_array)
        .and_then(|contents| {
            contents.iter().find(|content| {
                content
                    .get("role")
                    .and_then(serde_json::Value::as_str)
                    .is_none_or(|role| role.eq_ignore_ascii_case("user"))
            })
        })
        .and_then(|content| content.get("parts"))
    {
        derived = serde_json::to_vec(first_user).unwrap_or_default();
        derived.as_slice()
    } else {
        return format!(
            "-{}",
            rand::random::<u64>() & 0x7fff_ffff_ffff_ffff
        );
    };
    let digest = Sha256::digest(seed);
    let Some(prefix) = digest.as_slice().first_chunk::<8>() else {
        return format!("-{}", rand::random::<u64>() & 0x7fff_ffff_ffff_ffff);
    };
    let value = u64::from_be_bytes(*prefix) & 0x7fff_ffff_ffff_ffff;
    format!("-{}", value.max(1))
}

fn antigravity_request_id() -> String {
    let value = rand::random::<u128>();
    format!(
        "agent-{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        (value >> 96) as u32,
        (value >> 80) as u16,
        (value >> 64) as u16,
        (value >> 48) as u16,
        value & 0xffff_ffff_ffff,
    )
}

/// Removes the Antigravity `response` envelope while preserving native Gemini SSE bytes.
pub(crate) struct AntigravitySseConverter {
    buffer: String,
    done: bool,
    canonical_model: Option<String>,
}

impl AntigravitySseConverter {
    pub(crate) fn new() -> Self {
        Self {
            buffer: String::new(),
            done: false,
            canonical_model: None,
        }
    }

    pub(crate) fn with_canonical_model(canonical_model: &str) -> Self {
        Self {
            canonical_model: Some(canonical_model.trim().to_string()),
            ..Self::new()
        }
    }

    pub(crate) fn push(&mut self, chunk: &str) -> String {
        if self.done {
            return String::new();
        }
        self.buffer.push_str(chunk);
        let mut output = String::new();
        while let Some((index, separator_len)) =
            crate::protocol::stream::find_sse_event_boundary(&self.buffer)
        {
            let event = self.buffer[..index].to_string();
            self.buffer = self.buffer[index + separator_len..].to_string();
            output.push_str(&self.convert_event(&event));
        }
        output
    }

    pub(crate) fn finish(&mut self) -> String {
        if self.done || self.buffer.trim().is_empty() {
            return String::new();
        }
        let event = std::mem::take(&mut self.buffer);
        self.convert_event(&event)
    }

    fn convert_event(&mut self, event: &str) -> String {
        let data = event
            .lines()
            .filter_map(|line| {
                line.trim_end_matches('\r')
                    .strip_prefix("data:")
                    .map(str::trim_start)
            })
            .collect::<Vec<_>>()
            .join("\n");
        if data.trim() == "[DONE]" {
            self.done = true;
            return "data: [DONE]\n\n".to_string();
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&data) else {
            return String::new();
        };
        let mut response = value.get("response").cloned().unwrap_or(value);
        if let Some(canonical_model) = self.canonical_model.as_deref() {
            crate::antigravity_models::canonicalize_native_response_value(
                &mut response,
                canonical_model,
            );
        }
        format!("data: {response}\n\n")
    }
}
