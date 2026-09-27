pub(crate) fn supplier_upstream_model_for_request(
    config: &SupplierConfig,
    requested_model: Option<&str>,
) -> String {
    let requested = requested_model
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let upstream_model = config.upstream_model.trim();
    if let Some(requested) = requested {
        let requested_key = normalize_model_name(requested);
        let public_key = normalize_model_name(&config.public_model);
        let upstream_key = normalize_model_name(upstream_model);
        if let Some(model) = crate::config::resolve_model_name(&config.models, requested)
        {
            return model.trim().to_string();
        }
        if !upstream_model.is_empty() && requested_key == upstream_key {
            return upstream_model.to_string();
        }
        if !upstream_model.is_empty() && requested_key == public_key {
            return upstream_model.to_string();
        }
        if !upstream_model.is_empty()
            && (crate::config::model_name_matches(upstream_model, requested)
                || crate::config::model_name_matches(&config.public_model, requested))
        {
            return upstream_model.to_string();
        }
        return requested.to_string();
    }
    if !upstream_model.is_empty() {
        return upstream_model.to_string();
    }
    config
        .models
        .first()
        .map(|model| model.trim().to_string())
        .unwrap_or_else(|| requested.unwrap_or_default().to_string())
}

pub(crate) fn supplier_upstream_path_for_format(
    api_format: &str,
    path: &str,
    upstream_model: &str,
) -> String {
    if api_format == "gemini_openai" && path.starts_with("/v1beta/") {
        return "/v1/chat/completions".to_string();
    }
    supplier_upstream_path(path, upstream_model)
}

pub(crate) fn supplier_upstream_path(path: &str, upstream_model: &str) -> String {
    if upstream_model.trim().is_empty() {
        return path.to_string();
    }
    let Some(model) = gemini_model_from_path(path) else {
        return path.to_string();
    };
    let start = if path.starts_with("/gemini/") {
        "/gemini/v1beta/models/".len()
    } else {
        "/v1beta/models/".len()
    };
    format!(
        "{}{}{}",
        &path[..start],
        upstream_model.trim(),
        &path[start + model.len()..]
    )
}

// A model may contain colons (:free, :extended). Only a terminal API action
// separates it from the operation; query values are never part of the model.
pub(crate) fn gemini_model_from_path(path: &str) -> Option<&str> {
    let path = path.split_once('?').map_or(path, |(path, _)| path);
    let rest = path
        .strip_prefix("/gemini/v1beta/models/")
        .or_else(|| path.strip_prefix("/v1beta/models/"))?;
    let (model, action) = rest.rsplit_once(':')?;
    (!model.is_empty()
        && matches!(
            action,
            "generateContent" | "streamGenerateContent" | "countTokens" | "embedContent" | "batchEmbedContents"
        ))
    .then_some(model)
}

pub(crate) fn requested_model_from_body_or_path(body: &str, path: &str) -> Option<String> {
    top_level_json_string(body.as_bytes(), "model").or_else(|| {
        let path = path.split_once('?').map_or(path, |(path, _)| path);
        if let Some(model) = path.strip_prefix("/v1/models/") {
            return (!model.is_empty() && !model.contains('/'))
                .then(|| model.to_string());
        }
        gemini_model_from_path(path).map(str::to_string)
    })
}

pub(crate) fn join_upstream_url(base_url: &str, path: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    // DeepSeek publishes the OpenAI SDK base at the origin rather than a /v1
    // version root. Our internal OpenAI routes are /v1/*, so make them
    // provider-relative without changing the public Base URL shown to users.
    if (base.eq_ignore_ascii_case("https://api.deepseek.com")
        // Kilo's documented gateway root is unversioned.
        || base.eq_ignore_ascii_case("https://api.kilo.ai/api/gateway")) && path.starts_with("/v1/") {
        return format!("{}{}", base, path.trim_start_matches("/v1"));
    }
    if base.ends_with("/v1beta/openai") && path.starts_with("/v1/") {
        return format!("{}{}", base, path.trim_start_matches("/v1"));
    }
    // OpenAI-compatible providers publish different version roots (for
    // example /v2, /api/v3 and /api/paas/v4). The internal route remains
    // /v1/*, but it is relative to the provider's declared version root.
    let terminal_segment = base.rsplit('/').next().unwrap_or_default();
    let terminal_version = terminal_segment.strip_prefix('v').is_some_and(|version| {
        !version.is_empty() && version.chars().all(|ch| ch.is_ascii_digit())
    });
    if terminal_version && path.starts_with("/v1/") {
        return format!("{}{}", base, path.trim_start_matches("/v1"));
    }
    format!("{base}{path}")
}

pub(crate) fn bedrock_anthropic_base(base_url: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    if base.ends_with("/anthropic") {
        return base.to_string();
    }
    let root = base
        .strip_suffix("/v1")
        .unwrap_or(base)
        .trim_end_matches('/');
    format!("{root}/anthropic")
}

pub(crate) fn rewrite_supplier_body_model(body: &str, upstream_model: &str) -> String {
    if upstream_model.trim().is_empty() {
        return body.to_string();
    }
    let Ok(model) = serde_json::to_string(upstream_model.trim()) else {
        return body.to_string();
    };
    rewrite_top_level_json_field(body, "model", &model, false).unwrap_or_else(|_| body.to_string())
}

fn top_level_json_raw_token<'a>(body: &'a [u8], field: &str) -> Option<&'a str> {
    use serde_json::value::RawValue;

    let body = std::str::from_utf8(body).ok()?;
    serde_json::from_str::<&RawValue>(body).ok()?;
    let scan = scan_top_level_object(body).ok()?;
    let entry = scan.fields.iter().rev().find(|entry| entry.name == field)?;
    Some(&body[entry.value_start..entry.value_end])
}

pub(crate) fn top_level_json_bool(body: &[u8], field: &str) -> Option<bool> {
    match top_level_json_raw_token(body, field)? {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

pub(crate) fn top_level_json_string(body: &[u8], field: &str) -> Option<String> {
    serde_json::from_str(top_level_json_raw_token(body, field)?).ok()
}

pub(crate) fn rewrite_top_level_json_field(
    body: &str,
    field: &str,
    replacement_json: &str,
    insert_when_missing: bool,
) -> Result<String> {
    use serde_json::value::RawValue;

    serde_json::from_str::<Box<RawValue>>(replacement_json)
        .context("replacement must be valid JSON")?;
    serde_json::from_str::<Box<RawValue>>(body).context("body must be valid JSON")?;

    let scan = scan_top_level_object(body)?;
    let mut spans = scan
        .fields
        .iter()
        .filter(|entry| {
            entry.name == field && &body[entry.value_start..entry.value_end] != replacement_json
        })
        .map(|entry| (entry.value_start, entry.value_end))
        .collect::<Vec<_>>();
    if spans.is_empty() && scan.fields.iter().any(|entry| entry.name == field) {
        return Ok(body.to_string());
    }
    if spans.is_empty() && !insert_when_missing {
        return Ok(body.to_string());
    }

    if spans.is_empty() {
        let insert_at = body[..scan.close_brace]
            .trim_end_matches(char::is_whitespace)
            .len();
        let separator = if scan.fields.is_empty() { "" } else { "," };
        let insertion = format!(
            "{separator}{}:{replacement_json}",
            serde_json::to_string(field)?
        );
        let mut output = String::with_capacity(body.len() + insertion.len());
        output.push_str(&body[..insert_at]);
        output.push_str(&insertion);
        output.push_str(&body[insert_at..]);
        return Ok(output);
    }

    spans.sort_unstable_by_key(|span| span.0);
    let mut output = String::with_capacity(body.len() + replacement_json.len());
    let mut cursor = 0;
    for (start, end) in spans {
        output.push_str(&body[cursor..start]);
        output.push_str(replacement_json);
        cursor = end;
    }
    output.push_str(&body[cursor..]);
    Ok(output)
}

struct TopLevelObjectScan {
    fields: Vec<TopLevelField>,
    close_brace: usize,
}

struct TopLevelField {
    name: String,
    value_start: usize,
    value_end: usize,
}

fn scan_top_level_object(body: &str) -> Result<TopLevelObjectScan> {
    let bytes = body.as_bytes();
    let mut cursor = skip_json_whitespace(bytes, 0);
    if bytes.get(cursor) != Some(&b'{') {
        return Err(anyhow!("body must be a JSON object"));
    }
    cursor += 1;
    let mut fields = Vec::new();
    loop {
        cursor = skip_json_whitespace(bytes, cursor);
        if bytes.get(cursor) == Some(&b'}') {
            return Ok(TopLevelObjectScan {
                fields,
                close_brace: cursor,
            });
        }
        let key_start = cursor;
        let key_end = scan_json_string(bytes, key_start)?;
        let name: String = serde_json::from_str(&body[key_start..key_end])?;
        cursor = skip_json_whitespace(bytes, key_end);
        if bytes.get(cursor) != Some(&b':') {
            return Err(anyhow!("expected ':' after JSON object key"));
        }
        cursor = skip_json_whitespace(bytes, cursor + 1);
        let value_start = cursor;
        let value_end = scan_json_value(bytes, value_start)?;
        fields.push(TopLevelField {
            name,
            value_start,
            value_end,
        });
        cursor = skip_json_whitespace(bytes, value_end);
        match bytes.get(cursor) {
            Some(b',') => cursor += 1,
            Some(b'}') => {
                return Ok(TopLevelObjectScan {
                    fields,
                    close_brace: cursor,
                });
            }
            _ => return Err(anyhow!("expected ',' or '}}' after JSON object value")),
        }
    }
}

fn skip_json_whitespace(bytes: &[u8], mut cursor: usize) -> usize {
    while matches!(bytes.get(cursor), Some(b' ' | b'\n' | b'\r' | b'\t')) {
        cursor += 1;
    }
    cursor
}

fn scan_json_string(bytes: &[u8], start: usize) -> Result<usize> {
    if bytes.get(start) != Some(&b'"') {
        return Err(anyhow!("expected JSON string"));
    }
    let mut cursor = start + 1;
    let mut escaped = false;
    while let Some(byte) = bytes.get(cursor).copied() {
        if escaped {
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else if byte == b'"' {
            return Ok(cursor + 1);
        }
        cursor += 1;
    }
    Err(anyhow!("unterminated JSON string"))
}

fn scan_json_value(bytes: &[u8], start: usize) -> Result<usize> {
    match bytes.get(start) {
        Some(b'"') => scan_json_string(bytes, start),
        Some(b'{' | b'[') => scan_json_container(bytes, start),
        Some(_) => {
            let mut cursor = start;
            while let Some(byte) = bytes.get(cursor) {
                if matches!(byte, b',' | b'}' | b']' | b' ' | b'\n' | b'\r' | b'\t') {
                    break;
                }
                cursor += 1;
            }
            if cursor == start {
                Err(anyhow!("expected JSON value"))
            } else {
                Ok(cursor)
            }
        }
        None => Err(anyhow!("expected JSON value")),
    }
}

fn scan_json_container(bytes: &[u8], start: usize) -> Result<usize> {
    let mut stack = vec![bytes[start]];
    let mut cursor = start + 1;
    let mut in_string = false;
    let mut escaped = false;
    while let Some(byte) = bytes.get(cursor).copied() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            cursor += 1;
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' | b'[' => stack.push(byte),
            b'}' => {
                if stack.pop() != Some(b'{') {
                    return Err(anyhow!("mismatched JSON object delimiter"));
                }
            }
            b']' => {
                if stack.pop() != Some(b'[') {
                    return Err(anyhow!("mismatched JSON array delimiter"));
                }
            }
            _ => {}
        }
        cursor += 1;
        if stack.is_empty() {
            return Ok(cursor);
        }
    }
    Err(anyhow!("unterminated JSON container"))
}

// This is a Claude Code OAuth compatibility fallback, not a protocol-wide generation default.
// Normal Anthropic requests and cross-protocol adapters preserve or derive their own output limit.
// Keep it scoped to `ensure_claude_messages_body`: applying it to arbitrary Claude-compatible
// providers can exceed older models' output limits.
const CLAUDE_CODE_OAUTH_FALLBACK_MAX_TOKENS: u64 = 128_000;
const CLAUDE_CODE_FINGERPRINT_SALT: &[u8] = b"59cf53e54c78";
const CLAUDE_CODE_COMPAT_PROMPT: &str = "You are an interactive agent that helps users with software engineering tasks. Work in the current project, understand the existing code before editing, preserve user changes, and use the available tools when appropriate. Be careful with security-sensitive or destructive actions. Prefer concise and accurate responses, explain important decisions and verification results, include useful file locations when discussing code, and never claim actions that you did not perform.";

/// Applies only the required Anthropic Messages defaults at the retained subscription boundary.
///
/// Do not turn this into an allowlist: native Claude beta fields must remain transparent.
#[cfg(test)]
pub(crate) fn ensure_claude_messages_body(body: &str, upstream_model: &str) -> Result<String> {
    Ok(ensure_claude_messages_body_with_faults(body, upstream_model)?.body)
}

/// Claude Code OAuth accepts the dated 4.5 routes used by the official client.
///
/// Keep this mapping at the subscription boundary: normal Anthropic API-key
/// channels should continue to pass the caller's model ID through unchanged.
pub(crate) fn normalize_claude_subscription_model(model: &str) -> String {
    match model.trim() {
        "claude-sonnet-4-5" => "claude-sonnet-4-5-20250929".to_string(),
        "claude-opus-4-5" => "claude-opus-4-5-20251101".to_string(),
        "claude-haiku-4-5" => "claude-haiku-4-5-20251001".to_string(),
        model => model.to_string(),
    }
}

pub(crate) fn normalize_claude_subscription_count_tokens_body(body: &str) -> Result<String> {
    let mut value: serde_json::Value = serde_json::from_str(body)?;
    if let Some(model) = value
        .get("model")
        .and_then(serde_json::Value::as_str)
        .map(normalize_claude_subscription_model)
    {
        value["model"] = serde_json::Value::String(model);
    }
    // Anthropic's count_tokens operation accepts request-input fields only.
    // This helper is scoped to that operation; never leak the Messages generation
    // default into the token-counting request.
    if let Some(object) = value.as_object_mut() {
        object.remove("max_tokens");
    }
    Ok(serde_json::to_string(&value)?)
}

fn first_claude_user_text(value: &serde_json::Value) -> String {
    value
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .and_then(|messages| {
            messages.iter().find_map(|message| {
                if message.get("role").and_then(serde_json::Value::as_str) != Some("user") {
                    return None;
                }
                match message.get("content") {
                    Some(serde_json::Value::String(text)) => Some(text.clone()),
                    Some(serde_json::Value::Array(blocks)) => blocks.iter().find_map(|block| {
                        (block.get("type").and_then(serde_json::Value::as_str) == Some("text"))
                            .then(|| block.get("text").and_then(serde_json::Value::as_str))
                            .flatten()
                            .map(str::to_string)
                    }),
                    _ => None,
                }
            })
        })
        .unwrap_or_default()
}

fn claude_code_build_fingerprint(value: &serde_json::Value) -> String {
    let first_text = first_claude_user_text(value);
    let text = first_text.as_bytes();
    let mut input = Vec::with_capacity(
        CLAUDE_CODE_FINGERPRINT_SALT.len() + 3 + CLAUDE_CODE_VERSION.len(),
    );
    input.extend_from_slice(CLAUDE_CODE_FINGERPRINT_SALT);
    for index in [4_usize, 7, 20] {
        input.push(text.get(index).copied().unwrap_or(b'0'));
    }
    input.extend_from_slice(CLAUDE_CODE_VERSION.as_bytes());
    hex::encode(Sha256::digest(&input))[..3].to_string()
}

fn stable_claude_code_uuid(seed: &str) -> String {
    let mut bytes: [u8; 32] = Sha256::digest(seed.as_bytes()).into();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{}-{}-{}-{}-{}",
        hex::encode(&bytes[0..4]),
        hex::encode(&bytes[4..6]),
        hex::encode(&bytes[6..8]),
        hex::encode(&bytes[8..10]),
        hex::encode(&bytes[10..16])
    )
}

fn claude_code_system_is_native(value: &serde_json::Value) -> bool {
    value
        .get("system")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|blocks| {
            blocks.iter().any(|block| {
                block
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|text| {
                        text.starts_with("x-anthropic-billing-header:")
                            && text.contains("cc_entrypoint=")
                    })
            })
        })
}

fn claude_system_text_and_cache_control(
    value: &serde_json::Value,
) -> (String, Option<serde_json::Value>) {
    match value.get("system") {
        Some(serde_json::Value::String(text)) => (text.trim().to_string(), None),
        Some(serde_json::Value::Array(blocks)) => {
            let mut text = Vec::new();
            let mut cache_control = None;
            for block in blocks {
                if block.get("type").and_then(serde_json::Value::as_str) != Some("text") {
                    continue;
                }
                if let Some(part) = block
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                {
                    text.push(part.to_string());
                }
                if let Some(value) = block.get("cache_control") {
                    cache_control = Some(value.clone());
                }
            }
            (text.join("\n\n"), cache_control)
        }
        _ => (String::new(), None),
    }
}

fn prepend_claude_system_instructions_to_messages(
    value: &mut serde_json::Value,
    original_system: String,
    cache_control: Option<serde_json::Value>,
) {
    let original_system = original_system.trim();
    if original_system.is_empty()
        || original_system == CLAUDE_CODE_IDENTITY_PROMPT
        || original_system.starts_with("x-anthropic-billing-header:")
    {
        return;
    }
    let mut instruction = serde_json::json!({
        "type": "text",
        "text": format!("[System Instructions]\n{original_system}")
    });
    if let Some(cache_control) = cache_control {
        instruction["cache_control"] = cache_control;
    }
    let mut prefix = vec![
        serde_json::json!({"role": "user", "content": [instruction]}),
        serde_json::json!({
            "role": "assistant",
            "content": [{"type": "text", "text": "Understood. I will follow these instructions."}]
        }),
    ];
    if let Some(messages) = value
        .get_mut("messages")
        .and_then(serde_json::Value::as_array_mut)
    {
        prefix.append(messages);
        *messages = prefix;
    }
}

fn ensure_claude_code_metadata(
    value: &mut serde_json::Value,
    identity_seed: &str,
    cache_identity: Option<&str>,
) -> Result<()> {
    let serialized = serde_json::to_string(value)?;
    if claude_code_session_id_from_body(&serialized).is_some() {
        return Ok(());
    }
    let original_user_id = value
        .pointer("/metadata/user_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let device_id = hex::encode(Sha256::digest(
        format!("const-api-claude-device-v1\0{identity_seed}").as_bytes(),
    ));
    let session_seed = normalized_subscription_cache_identity(cache_identity)
        .map(|cache_identity| format!("cache:{cache_identity}"))
        .unwrap_or_else(|| first_claude_user_text(value));
    let session_id = stable_claude_code_uuid(&format!(
        "const-api-claude-session-v1\0{identity_seed}\0{original_user_id}\0{session_seed}"
    ));
    if !value.get("metadata").is_some_and(serde_json::Value::is_object) {
        value["metadata"] = serde_json::json!({});
    }
    value["metadata"]["user_id"] = serde_json::Value::String(
        serde_json::json!({
            "device_id": device_id,
            "account_uuid": "",
            "session_id": session_id
        })
        .to_string(),
    );
    Ok(())
}

/// Wraps a generic Anthropic Messages body as a current Claude Code OAuth request.
///
/// Claude OAuth credentials are scoped to the first-party client. Catalog and quota reads can
/// succeed while a generic inference body is classified as third-party usage. Keep the rewrite
/// here, at the retained subscription boundary, so probes and real requests share one contract.
/// Bodies that already carry a Claude Code billing block are preserved.
#[cfg(test)]
pub(crate) fn prepare_claude_subscription_oauth_body(
    body: &str,
    identity_seed: &str,
) -> Result<String> {
    prepare_claude_subscription_oauth_body_with_cache_identity(body, identity_seed, None)
}

pub(crate) fn prepare_claude_subscription_oauth_body_with_cache_identity(
    body: &str,
    identity_seed: &str,
    cache_identity: Option<&str>,
) -> Result<String> {
    let mut value: serde_json::Value = serde_json::from_str(body)?;
    // A genuine Claude Code request can contain session/classifier system blocks before or after
    // its billing block. Preserve the whole body instead of re-wrapping or replacing metadata.
    if claude_code_system_is_native(&value) {
        return Ok(body.to_string());
    }
    // Derive the session from the caller's original first user turn. The compatibility rewrite
    // can prepend a migrated system instruction, which must not collapse unrelated conversations
    // that happen to share the same system prompt onto one session identity.
    ensure_claude_code_metadata(&mut value, identity_seed, cache_identity)?;
    // A cache control in system/messages is later in Anthropic's tools -> system -> messages
    // prefix order and already includes the compatibility prompt. A tools-only control does not,
    // so add the compatibility boundary when the four-breakpoint budget still permits it.
    let cache_compatibility_prompt =
        claude_cache_controls(&value).should_cache_compatibility_prompt();
    let (original_system, cache_control) = claude_system_text_and_cache_control(&value);
    let fingerprint = claude_code_build_fingerprint(&value);
    let mut compatibility_prompt = serde_json::json!({
        "type": "text",
        "text": CLAUDE_CODE_COMPAT_PROMPT
    });
    if cache_compatibility_prompt {
        compatibility_prompt["cache_control"] =
            serde_json::json!({"type": "ephemeral", "ttl": "5m"});
    }
    value["system"] = serde_json::json!([
        {
            "type": "text",
            "text": format!(
                "x-anthropic-billing-header: cc_version={CLAUDE_CODE_VERSION}.{fingerprint}; cc_entrypoint={CLAUDE_CODE_ENTRYPOINT};"
            )
        },
        {"type": "text", "text": CLAUDE_CODE_IDENTITY_PROMPT},
        compatibility_prompt
    ]);
    prepend_claude_system_instructions_to_messages(&mut value, original_system, cache_control);
    Ok(serde_json::to_string(&value)?)
}

pub(crate) struct ReportedClaudeSubscriptionAdjustment {
    pub(crate) body: String,
    pub(crate) faults: Vec<ImprovementFault>,
}

#[derive(Clone, Copy, Default)]
struct ClaudeCacheControls {
    automatic: bool,
    tools: usize,
    after_tools: usize,
}

impl ClaudeCacheControls {
    fn explicit(self) -> usize {
        self.tools + self.after_tools
    }

    fn any(self) -> bool {
        self.automatic || self.explicit() > 0
    }

    fn should_cache_compatibility_prompt(self) -> bool {
        !self.automatic && self.after_tools == 0 && self.explicit() < 4
    }
}

fn claude_content_cache_control_count(value: &serde_json::Value) -> usize {
    value
        .as_array()
        .into_iter()
        .flatten()
        .map(|block| {
            usize::from(block.get("cache_control").is_some())
                + block
                    .get("content")
                    .map(claude_content_cache_control_count)
                    .unwrap_or_default()
        })
        .sum()
}

fn claude_cache_controls(value: &serde_json::Value) -> ClaudeCacheControls {
    let tools = value
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter(|tool| tool.get("cache_control").is_some())
                .count()
        })
        .unwrap_or_default();
    let system = value
        .get("system")
        .map(claude_content_cache_control_count)
        .unwrap_or_default();
    let messages = value
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .map(|messages| {
            messages
                .iter()
                .map(|message| {
                    usize::from(message.get("cache_control").is_some())
                        + message
                            .get("content")
                            .map(claude_content_cache_control_count)
                            .unwrap_or_default()
                })
                .sum::<usize>()
        })
        .unwrap_or_default();
    ClaudeCacheControls {
        automatic: value.get("cache_control").is_some(),
        tools,
        after_tools: system + messages,
    }
}

fn contains_claude_cache_control(value: &serde_json::Value) -> bool {
    claude_cache_controls(value).any()
}

fn ephemeral_claude_cache_control() -> serde_json::Value {
    serde_json::json!({"type": "ephemeral"})
}

/// Adds conservative Anthropic cache breakpoints only when the caller supplied none.
///
/// Anthropic evaluates prompt prefixes in tools -> system -> messages order. The stable
/// boundaries are the final non-deferred tool, the final system block, and the latest
/// cacheable conversation turn. Existing controls, including the top-level automatic cache
/// policy, remain entirely caller-owned.
fn inject_missing_claude_cache_breakpoints(value: &mut serde_json::Value) -> usize {
    if contains_claude_cache_control(value) {
        return 0;
    }

    let mut injected = 0;
    if let Some(tools) = value
        .get_mut("tools")
        .and_then(serde_json::Value::as_array_mut)
    {
        if let Some(tool) = tools.iter_mut().rev().find(|tool| {
            !tool
                .get("defer_loading")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        }) {
            if let Some(tool) = tool.as_object_mut() {
                tool.insert(
                    "cache_control".to_string(),
                    ephemeral_claude_cache_control(),
                );
                injected += 1;
            }
        }
    }

    if let Some(system) = value.get_mut("system") {
        match system {
            serde_json::Value::String(text) if !text.is_empty() => {
                *system = serde_json::json!([{
                    "type": "text",
                    "text": text,
                    "cache_control": ephemeral_claude_cache_control()
                }]);
                injected += 1;
            }
            serde_json::Value::Array(blocks) => {
                if let Some(block) = blocks.last_mut().and_then(serde_json::Value::as_object_mut) {
                    block.insert(
                        "cache_control".to_string(),
                        ephemeral_claude_cache_control(),
                    );
                    injected += 1;
                }
            }
            _ => {}
        }
    }

    if let Some(messages) = value
        .get_mut("messages")
        .and_then(serde_json::Value::as_array_mut)
    {
        let target = messages
            .iter()
            .enumerate()
            .rev()
            .find(|(_, message)| claude_message_is_cacheable(message))
            .map(|(index, _)| index);
        if let Some(target) = target {
            if let Some(content) = messages[target].get_mut("content") {
                match content {
                    serde_json::Value::String(text) => {
                        *content = serde_json::json!([{
                            "type": "text",
                            "text": text,
                            "cache_control": ephemeral_claude_cache_control()
                        }]);
                        injected += 1;
                    }
                    serde_json::Value::Array(blocks) => {
                        if let Some(block) =
                            blocks.last_mut().and_then(serde_json::Value::as_object_mut)
                        {
                            block.insert(
                                "cache_control".to_string(),
                                ephemeral_claude_cache_control(),
                            );
                            injected += 1;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    injected
}

fn claude_message_is_cacheable(message: &serde_json::Value) -> bool {
    let role = message
        .get("role")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if !matches!(role, "user" | "assistant") {
        return false;
    }
    match message.get("content") {
        Some(serde_json::Value::String(_)) => true,
        Some(serde_json::Value::Array(blocks)) if !blocks.is_empty() => {
            role != "assistant"
                || !blocks.last().is_some_and(|block| {
                    matches!(
                        block.get("type").and_then(serde_json::Value::as_str),
                        Some("thinking" | "redacted_thinking")
                    )
                })
        }
        _ => false,
    }
}

pub(crate) fn anthropic_body_with_automatic_cache(body: &str) -> Result<String> {
    let mut value: serde_json::Value = serde_json::from_str(body)?;
    if contains_claude_cache_control(&value) {
        return Ok(body.to_string());
    }
    let Some(object) = value.as_object_mut() else {
        return Ok(body.to_string());
    };
    object.insert(
        "cache_control".to_string(),
        ephemeral_claude_cache_control(),
    );
    serde_json::to_string(&value).map_err(Into::into)
}

fn normalize_claude_cacheable_message_content(value: &mut serde_json::Value) {
    let Some(messages) = value
        .get_mut("messages")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    for message in messages {
        let Some(content) = message.get_mut("content") else {
            continue;
        };
        if let serde_json::Value::String(text) = content {
            *content = serde_json::json!([{"type": "text", "text": text}]);
        }
        let Some(blocks) = content.as_array_mut() else {
            continue;
        };
        for block in blocks {
            let Some(block) = block.as_object_mut() else {
                continue;
            };
            if block
                .get("citations")
                .and_then(serde_json::Value::as_array)
                .is_some_and(Vec::is_empty)
            {
                block.remove("citations");
            }
        }
    }
}

pub(crate) fn ensure_claude_messages_body_with_faults(
    body: &str,
    upstream_model: &str,
) -> Result<ReportedClaudeSubscriptionAdjustment> {
    let mut value: serde_json::Value = serde_json::from_str(body)?;
    // The generic protocol converter has already selected the upstream model. A native Claude
    // Code body must otherwise remain byte-stable: its defaults, cache markers, tool settings,
    // metadata and system blocks belong to the originating client.
    if claude_code_system_is_native(&value) {
        let body = if upstream_model.trim().is_empty() {
            body.to_string()
        } else {
            rewrite_supplier_body_model(
                body,
                &normalize_claude_subscription_model(upstream_model),
            )
        };
        return Ok(ReportedClaudeSubscriptionAdjustment {
            body,
            faults: Vec::new(),
        });
    }
    let mut faults = Vec::new();
    if !upstream_model.trim().is_empty() {
        value["model"] =
            serde_json::Value::String(normalize_claude_subscription_model(upstream_model));
    }
    if value.get("max_tokens").is_none() {
        value["max_tokens"] = serde_json::json!(CLAUDE_CODE_OAUTH_FALLBACK_MAX_TOKENS);
    }
    if value.get("tools").is_none() {
        value["tools"] = serde_json::json!([]);
    }
    let tools_are_empty = value
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .is_none_or(Vec::is_empty);
    if tools_are_empty {
        let removed = value
            .as_object_mut()
            .and_then(|object| object.remove("tool_choice"))
            .is_some();
        if removed {
            let summary = "Claude account subscription cannot apply tool_choice without any tool definitions; tool_choice was omitted so the request could continue";
            log::debug!(
                "[const-api][subscription] endpoint adjustment endpoint=claude_messages code=claude_tool_choice_omitted_without_tools path=$.tool_choice"
            );
            faults.push(ImprovementFault::endpoint_adjustment(
                "claude_tool_choice_omitted_without_tools",
                "anthropic",
                "claude_account_subscription",
                "anthropic_messages",
                "anthropic_messages",
                upstream_model,
                "$.tool_choice",
                summary,
            ));
        }
    }
    let thinking_enabled = matches!(
        value
            .get("thinking")
            .and_then(|thinking| thinking.get("type"))
            .and_then(serde_json::Value::as_str),
        Some("enabled" | "adaptive")
    );
    if thinking_enabled && value.get("context_management").is_none() {
        value["context_management"] = serde_json::json!({
            "edits": [{"type": "clear_thinking_20251015", "keep": "all"}]
        });
    }
    if value
        .get("messages")
        .and_then(|messages| messages.as_array())
        .map(|messages| messages.is_empty())
        .unwrap_or(true)
    {
        value["messages"] = serde_json::json!([{"role": "user", "content": ""}]);
    }
    if !contains_claude_cache_control(&value) {
        normalize_claude_cacheable_message_content(&mut value);
        inject_missing_claude_cache_breakpoints(&mut value);
    }
    Ok(ReportedClaudeSubscriptionAdjustment {
        body: serde_json::to_string(&value)?,
        faults,
    })
}

pub(crate) fn test_request_for_channel(
    channel: &ChannelConfig,
    model: &str,
    prompt: &str,
) -> Result<(String, serde_json::Value, &'static str)> {
    let model = crate::openrouter::upstream_model(channel, model);
    let protocol = crate::protocol::kind::ProtocolKind::parse(&channel.api_format).ok()
        .filter(|protocol| crate::coding_gateway::model_supports_protocol(channel.source_driver(), &model, *protocol))
        .or_else(|| crate::coding_gateway::model_protocol(channel.source_driver(), &model));
    match protocol.map(|protocol| protocol.as_str()).unwrap_or(channel.api_format.trim()) {
        "anthropic_messages" => Ok((
            join_upstream_url(
                &if channel.kind.trim().eq_ignore_ascii_case("aws_bedrock") {
                    bedrock_anthropic_base(&channel.upstream_base_url)
                } else {
                    channel.upstream_base_url.clone()
                },
                "/v1/messages",
            ),
            serde_json::json!({
                "model": model,
                "max_tokens": 128,
                "messages": [{"role": "user", "content": prompt}]
            }),
            "anthropic",
        )),
        "gemini_native" => Ok((
            crate::coding_gateway::gemini_url(
                channel.source_driver(),
                &channel.upstream_base_url,
                &format!("/v1beta/models/{model}:generateContent"),
            )?,
            serde_json::json!({
                "contents": [{"parts": [{"text": prompt}]}],
                "generationConfig": {"maxOutputTokens": 128}
            }),
            "gemini",
        )),
        "gemini_openai" => Ok((
            join_upstream_url(
                &gemini_openai_base(&channel.upstream_base_url),
                "/v1/chat/completions",
            ),
            serde_json::json!({
                "model": model,
                "messages": [{"role": "user", "content": prompt}]
            }),
            "openai",
        )),
        "openai_responses" => Ok((
            join_upstream_url(&channel.upstream_base_url, "/v1/responses"),
            serde_json::json!({
                "model": model,
                "input": prompt
            }),
            "openai",
        )),
        _ => Ok((
            join_upstream_url(&channel.upstream_base_url, "/v1/chat/completions"),
            serde_json::json!({
                "model": model,
                "messages": [{"role": "user", "content": prompt}]
            }),
            "openai",
        )),
    }
}

pub(crate) fn extract_any_content(raw: &str) -> Option<String> {
    extract_chat_content(raw)
        .or_else(|| extract_anthropic_content(raw))
        .or_else(|| extract_gemini_content(raw))
        .or_else(|| extract_responses_sse_content(raw))
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ResponseShapeSummary {
    pub(crate) kind: String,
    pub(crate) tool_call_count: usize,
    pub(crate) finish_reason: String,
    pub(crate) sse_event_count: usize,
    pub(crate) sse_done: bool,
    pub(crate) sse_last_event_type: String,
}

pub(crate) fn protocol_safe_response_content(raw: &str) -> String {
    if let Some(content) = extract_any_content(raw) {
        return content;
    }
    let shape = response_shape_summary(raw);
    if shape.tool_call_count > 0 {
        return format!(
            "\u{5de5}\u{5177}\u{8c03}\u{7528} {} \u{4e2a}",
            shape.tool_call_count
        );
    }
    let (input_tokens, output_tokens) = extract_usage_tokens(raw);
    if shape.finish_reason == "completed" || input_tokens > 0 || output_tokens > 0 {
        if input_tokens > 0 || output_tokens > 0 {
            return format!(
                "Request completed without displayable text (input {input_tokens}, output {output_tokens} tokens)"
            );
        }
        return "Request completed without displayable text".to_string();
    }
    raw.to_string()
}

pub(crate) fn response_shape_summary(raw: &str) -> ResponseShapeSummary {
    let mut summary = ResponseShapeSummary::default();
    if raw.trim().is_empty() {
        summary.kind = "empty".to_string();
        return summary;
    }
    let sse = response_sse_summary(raw);
    summary.sse_event_count = sse.event_count;
    summary.sse_done = sse.done;
    summary.sse_last_event_type = sse.last_event_type;
    for value in response_json_values(raw) {
        merge_response_shape_value(&mut summary, &value);
    }
    if summary.kind.is_empty() {
        summary.kind = if summary.tool_call_count > 0 {
            "tool_call".to_string()
        } else if extract_any_content(raw).is_some() {
            "text".to_string()
        } else {
            "unknown".to_string()
        };
    }
    if summary.tool_call_count > 0 && extract_any_content(raw).is_some() {
        summary.kind = "mixed".to_string();
    }
    summary
}

#[derive(Debug, Clone, Default)]
struct ResponseSseSummary {
    event_count: usize,
    done: bool,
    last_event_type: String,
}

fn response_sse_summary(raw: &str) -> ResponseSseSummary {
    let mut summary = ResponseSseSummary::default();
    let mut event_name = String::new();
    for line in raw.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            event_name.clear();
            continue;
        }
        if let Some(event) = line.strip_prefix("event:") {
            event_name = event.trim().to_string();
            continue;
        }
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() {
            continue;
        }
        if data == "[DONE]" {
            summary.done = true;
            continue;
        }
        summary.event_count += 1;
        if !event_name.is_empty() {
            summary.last_event_type = event_name.clone();
            continue;
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(data) {
            if let Some(event_type) = value
                .get("type")
                .and_then(|value| value.as_str())
                .or_else(|| value.get("object").and_then(|value| value.as_str()))
                .filter(|value| !value.trim().is_empty())
            {
                summary.last_event_type = event_type.to_string();
            }
        }
    }
    summary
}

fn response_json_values(raw: &str) -> Vec<serde_json::Value> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) {
        return vec![value];
    }
    let mut values = Vec::new();
    let mut event_name = String::new();
    for line in raw.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            event_name.clear();
            continue;
        }
        if let Some(event) = line.strip_prefix("event:") {
            event_name = event.trim().to_string();
            continue;
        }
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(data) {
            if !event_name.is_empty() {
                if let Some(object) = value.as_object_mut() {
                    object
                        .entry("type")
                        .or_insert_with(|| serde_json::json!(event_name));
                }
            }
            values.push(value);
        }
    }
    values
}

fn merge_response_shape_value(summary: &mut ResponseShapeSummary, value: &serde_json::Value) {
    if let Some(reason) = value
        .pointer("/choices/0/finish_reason")
        .and_then(|value| value.as_str())
        .or_else(|| value.get("stop_reason").and_then(|value| value.as_str()))
        .or_else(|| {
            value
                .pointer("/candidates/0/finishReason")
                .and_then(|value| value.as_str())
        })
        .or_else(|| value.get("status").and_then(|value| value.as_str()))
        .or_else(|| {
            value
                .pointer("/response/status")
                .and_then(|value| value.as_str())
        })
        .filter(|value| !value.trim().is_empty())
    {
        summary.finish_reason = reason.to_string();
    }
    if value.get("type").and_then(|value| value.as_str()) == Some("response.completed") {
        summary.finish_reason = "completed".to_string();
    }
    let tool_count = chat_tool_call_count(value)
        + response_output_tool_count(value)
        + anthropic_tool_use_count(value)
        + gemini_function_call_count(value)
        + response_item_tool_count(value);
    summary.tool_call_count = summary.tool_call_count.max(tool_count);
    if summary.tool_call_count > 0 {
        summary.kind = "tool_call".to_string();
    } else if extract_any_content(&value.to_string()).is_some() && summary.kind.is_empty() {
        summary.kind = "text".to_string();
    }
}

fn chat_tool_call_count(value: &serde_json::Value) -> usize {
    value
        .pointer("/choices/0/message/tool_calls")
        .or_else(|| value.pointer("/choices/0/delta/tool_calls"))
        .and_then(|value| value.as_array())
        .map(Vec::len)
        .unwrap_or_default()
}

fn response_output_tool_count(value: &serde_json::Value) -> usize {
    value
        .get("output")
        .and_then(|value| value.as_array())
        .or_else(|| {
            value
                .pointer("/response/output")
                .and_then(|value| value.as_array())
        })
        .map(|items| {
            items
                .iter()
                .filter(|item| {
                    item.get("type").and_then(|value| value.as_str()) == Some("function_call")
                })
                .count()
        })
        .unwrap_or_default()
}

fn anthropic_tool_use_count(value: &serde_json::Value) -> usize {
    value
        .get("content")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter(|item| {
                    item.get("type").and_then(|value| value.as_str()) == Some("tool_use")
                })
                .count()
        })
        .unwrap_or_default()
}

fn gemini_function_call_count(value: &serde_json::Value) -> usize {
    value
        .pointer("/candidates/0/content/parts")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter(|item| {
                    item.get("functionCall").is_some() || item.get("function_call").is_some()
                })
                .count()
        })
        .unwrap_or_default()
}

fn response_item_tool_count(value: &serde_json::Value) -> usize {
    if value
        .get("item")
        .and_then(|item| item.get("type"))
        .and_then(|value| value.as_str())
        == Some("function_call")
    {
        1
    } else {
        0
    }
}

pub(crate) fn extract_responses_sse_content(raw: &str) -> Option<String> {
    let mut text = String::new();
    let mut done_text: Option<String> = None;
    for line in raw.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };
        if let Some(delta) = value
            .get("delta")
            .or_else(|| value.pointer("/response/output_text/delta"))
            .or_else(|| value.pointer("/choices/0/delta/content"))
            .and_then(|item| item.as_str())
        {
            text.push_str(delta);
        }
        if let Some(done) = value
            .get("text")
            .or_else(|| value.pointer("/part/text"))
            .or_else(|| value.pointer("/choices/0/message/content"))
            .and_then(|item| item.as_str())
            .filter(|item| !item.is_empty())
        {
            done_text = Some(done.to_string());
        }
    }
    if !text.is_empty() {
        return Some(text);
    }
    done_text
}

pub(crate) fn extract_chat_content(raw: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    value
        .get("choices")
        .and_then(|choices| choices.get(0))
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(|content| content.as_str())
        .map(str::to_string)
        .or_else(|| {
            value
                .get("output")
                .and_then(|output| output.as_array())
                .and_then(|items| items.iter().filter_map(|item| item.get("content").and_then(|c| c.as_array()))
                    .flatten().find_map(|content| content.get("text").and_then(|t| t.as_str())))
                .map(str::to_string)
        })
        .or_else(|| {
            value
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(|message| message.as_str())
                .map(str::to_string)
        })
}

pub(crate) fn extract_anthropic_content(raw: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    value
        .get("content")
        .and_then(|content| content.as_array())
        .and_then(|items| {
            items.iter().find_map(|item| {
                item.get("text")
                    .and_then(|text| text.as_str())
                    .map(str::to_string)
            })
        })
        .or_else(|| {
            value
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(|message| message.as_str())
                .map(str::to_string)
        })
}

pub(crate) fn extract_gemini_content(raw: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    value
        .get("response")
        .and_then(|response| response.get("candidates"))
        .and_then(|items| items.get(0))
        .and_then(|item| item.get("content"))
        .and_then(|content| content.get("parts"))
        .and_then(|parts| parts.get(0))
        .and_then(|part| part.get("text"))
        .and_then(|text| text.as_str())
        .map(str::to_string)
        .or_else(|| {
            value
                .get("candidates")
                .and_then(|items| items.get(0))
                .and_then(|item| item.get("content"))
                .and_then(|content| content.get("parts"))
                .and_then(|parts| parts.get(0))
                .and_then(|part| part.get("text"))
                .and_then(|text| text.as_str())
                .map(str::to_string)
        })
        .or_else(|| {
            value
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(|message| message.as_str())
                .map(str::to_string)
        })
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct UsageTokenCounts {
    pub(crate) input_tokens: Option<u64>,
    pub(crate) output_tokens: Option<u64>,
}

impl UsageTokenCounts {
    pub(crate) fn merge(self, other: Self) -> Self {
        Self {
            input_tokens: merge_optional_max(self.input_tokens, other.input_tokens),
            output_tokens: merge_optional_max(self.output_tokens, other.output_tokens),
        }
    }
}

pub(crate) fn extract_usage_tokens(raw: &str) -> (i64, i64) {
    let counts = extract_usage_token_counts(raw);
    (
        counts
            .input_tokens
            .map(|value| i64::try_from(value).unwrap_or(i64::MAX))
            .unwrap_or_default(),
        counts
            .output_tokens
            .map(|value| i64::try_from(value).unwrap_or(i64::MAX))
            .unwrap_or_default(),
    )
}

pub(crate) fn extract_usage_token_counts(raw: &str) -> UsageTokenCounts {
    let parsed = if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) {
        extract_usage_token_counts_from_value(&value)
    } else {
        response_json_values(raw)
            .iter()
            .fold(UsageTokenCounts::default(), |counts, value| {
                counts.merge(extract_usage_token_counts_from_value(value))
            })
    };
    parsed.merge(extract_usage_token_counts_from_suffix(raw))
}

fn extract_usage_token_counts_from_value(value: &serde_json::Value) -> UsageTokenCounts {
    let Some(usage) = value
        .get("usage")
        .or_else(|| value.pointer("/response/usage"))
        .or_else(|| value.pointer("/message/usage"))
        .or_else(|| value.get("usageMetadata"))
        .or_else(|| value.pointer("/response/usageMetadata"))
        .or_else(|| value.pointer("/interaction/usage"))
    else {
        return UsageTokenCounts::default();
    };
    let base_input_tokens = usage
        .get("prompt_tokens")
        .or_else(|| usage.get("input_tokens"))
        .or_else(|| usage.get("promptTokenCount"))
        .or_else(|| usage.get("inputTokens"))
        .or_else(|| usage.get("total_input_tokens"))
        .and_then(json_u64);
    let cache_input_tokens = ["cache_creation_input_tokens", "cache_read_input_tokens"]
        .iter()
        .filter_map(|key| usage.get(*key).and_then(json_u64))
        .collect::<Vec<_>>();
    let input_tokens = match (base_input_tokens, cache_input_tokens.is_empty()) {
        (Some(input), _) => Some(
            cache_input_tokens
                .iter()
                .fold(input, |total, value| total.saturating_add(*value)),
        ),
        (None, false) => Some(
            cache_input_tokens
                .iter()
                .fold(0_u64, |total, value| total.saturating_add(*value)),
        ),
        (None, true) => None,
    };
    let reported_output_tokens = usage
        .get("completion_tokens")
        .or_else(|| usage.get("output_tokens"))
        .or_else(|| usage.get("candidatesTokenCount"))
        .or_else(|| usage.get("responseTokenCount"))
        .or_else(|| usage.get("outputTokens"))
        .or_else(|| usage.get("total_output_tokens"))
        .and_then(json_u64);
    let output_from_total = usage
        .get("total_tokens")
        .or_else(|| usage.get("totalTokenCount"))
        .and_then(json_u64)
        .and_then(|total| base_input_tokens.map(|input| total.saturating_sub(input)));
    let reasoning_tokens = usage
        .get("thoughtsTokenCount")
        .or_else(|| usage.get("total_thought_tokens"))
        .and_then(json_u64);
    let direct_output = match (reported_output_tokens, reasoning_tokens) {
        (Some(output), Some(reasoning)) => Some(output.saturating_add(reasoning)),
        (Some(output), None) => Some(output),
        (None, Some(reasoning)) => Some(reasoning),
        (None, None) => None,
    };
    UsageTokenCounts {
        input_tokens,
        output_tokens: merge_optional_max(direct_output, output_from_total),
    }
}

fn merge_optional_max(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn extract_usage_token_counts_from_suffix(raw: &str) -> UsageTokenCounts {
    let mut counts = UsageTokenCounts::default();
    for field in ["usage", "usageMetadata"] {
        let needle = format!("\"{field}\"");
        for (index, _) in raw.match_indices(&needle) {
            let mut remaining = raw[index + needle.len()..].trim_start();
            let Some(after_colon) = remaining.strip_prefix(':') else {
                continue;
            };
            remaining = after_colon.trim_start();
            if !remaining.starts_with('{') {
                continue;
            }
            let Some(Ok(usage)) = serde_json::Deserializer::from_str(remaining)
                .into_iter::<serde_json::Value>()
                .next()
            else {
                continue;
            };
            let mut wrapper = serde_json::Map::new();
            wrapper.insert(field.to_string(), usage);
            counts = counts.merge(extract_usage_token_counts_from_value(
                &serde_json::Value::Object(wrapper),
            ));
        }
    }
    counts
}

fn json_u64(value: &serde_json::Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|value| u64::try_from(value).ok()))
        .or_else(|| value.as_str().and_then(|value| value.parse::<u64>().ok()))
}

#[cfg(test)]
mod top_level_json_tests {
    use super::*;

    #[test]
    fn gemini_action_parsing_and_rewriting_preserve_model_variants_and_queries() {
        for prefix in ["/gemini/v1beta/models/", "/v1beta/models/"] {
            for action in ["generateContent", "streamGenerateContent", "countTokens", "embedContent", "batchEmbedContents"] {
                for model in ["model", "model:free", "model:extended", "claude-model[1m]"] {
                    let path = format!("{prefix}{model}:{action}?alt=sse&tag=a:b");
                    assert_eq!(requested_model_from_body_or_path("{}", &path), Some(model.into()));
                    let wire = format!("Vendor/{model}");
                    assert_eq!(supplier_upstream_path(&path, &wire), format!("{prefix}{wire}:{action}?alt=sse&tag=a:b"));
                }
            }
        }
        assert_eq!(requested_model_from_body_or_path("{}", "/v1/models/model:free"), Some("model:free".into()));
        assert_eq!(supplier_upstream_path("/v1/chat/completions", "Vendor/model"), "/v1/chat/completions");
    }

    #[test]
    fn anthropic_api_automatic_cache_is_added_once_without_rewriting_caller_controls() {
        let plain = r#"{"model":"claude-sonnet","messages":[{"role":"user","content":"hello"}]}"#;
        let cached = anthropic_body_with_automatic_cache(plain).unwrap();
        let cached: serde_json::Value = serde_json::from_str(&cached).unwrap();
        assert_eq!(cached["cache_control"]["type"], "ephemeral");

        let caller = r#"{"model":"claude-sonnet","cache_control":{"type":"ephemeral","ttl":"1h"},"messages":[{"role":"user","content":"hello"}]}"#;
        assert_eq!(anthropic_body_with_automatic_cache(caller).unwrap(), caller);
    }

    #[test]
    fn usage_tokens_cover_openai_anthropic_gemini_and_numeric_strings() {
        assert_eq!(
            extract_usage_tokens(r#"{"usage":{"input_tokens":11,"output_tokens":7}}"#),
            (11, 7)
        );
        assert_eq!(
            extract_usage_tokens(
                r#"{"usage":{"input_tokens":10,"cache_creation_input_tokens":2,"cache_read_input_tokens":3,"output_tokens":4}}"#
            ),
            (15, 4)
        );
        assert_eq!(
            extract_usage_tokens(
                r#"{"usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":4,"thoughtsTokenCount":2,"totalTokenCount":9}}"#
            ),
            (3, 6)
        );
        assert_eq!(
            extract_usage_tokens(
                r#"{"response":{"usage":{"inputTokens":"5","outputTokens":"6"}}}"#
            ),
            (5, 6)
        );
    }

    #[test]
    fn usage_token_counts_preserve_explicit_zero_and_missing_fields() {
        assert_eq!(
            extract_usage_token_counts(r#"{"usage":{"prompt_tokens":12,"completion_tokens":0}}"#),
            UsageTokenCounts {
                input_tokens: Some(12),
                output_tokens: Some(0),
            }
        );
        assert_eq!(
            extract_usage_token_counts(r#"{"usage":{"prompt_tokens":12}}"#),
            UsageTokenCounts {
                input_tokens: Some(12),
                output_tokens: None,
            }
        );
    }

    #[test]
    fn usage_token_counts_cover_anthropic_stream_and_gemini_interactions() {
        let mut accumulator = SupplierUpstreamUsageAccumulator::new("anthropic_messages");
        accumulator.push(
            b"data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":21,\"output_tokens\":0}}}\n\n",
        );
        accumulator.push(
            b"data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":8}}\n\n",
        );
        assert_eq!(
            accumulator.finish().unwrap().lan_share_token_counts(),
            UsageTokenCounts {
                input_tokens: Some(21),
                output_tokens: Some(8),
            }
        );

        assert_eq!(
            extract_usage_token_counts(
                r#"{"interaction":{"usage":{"total_input_tokens":34,"total_output_tokens":5,"total_thought_tokens":3}}}"#
            ),
            UsageTokenCounts {
                input_tokens: Some(34),
                output_tokens: Some(8),
            }
        );
    }

    #[test]
    fn usage_token_counts_parse_a_usage_object_from_a_truncated_large_json_tail() {
        let raw = format!(
            "{}\"usage\":{{\"prompt_tokens\":17,\"completion_tokens\":2}}}}",
            "x".repeat(600 * 1024)
        );
        let tail = &raw[raw.len() - 512 * 1024..];

        assert_eq!(
            extract_usage_token_counts(tail),
            UsageTokenCounts {
                input_tokens: Some(17),
                output_tokens: Some(2),
            }
        );
    }

    #[test]
    fn protocol_safe_response_content_summarizes_completed_response_without_text() {
        let raw =
            r#"{"status":"completed","output":[],"usage":{"input_tokens":14,"output_tokens":6}}"#;

        assert_eq!(
            protocol_safe_response_content(raw),
            "Request completed without displayable text (input 14, output 6 tokens)"
        );
        assert!(!protocol_safe_response_content(raw).contains("\"output\""));
    }

    #[test]
    fn top_level_json_bool_ignores_nested_unknown_fields_without_mutating_input() {
        let body = br#"{
  "future": {"stream": false, "nested": [1, {"keep": true}]},
  "stream" : true,
  "unknownAfter": {"bytes": "stay exactly here"}
}"#;
        let original = body.to_vec();

        assert_eq!(top_level_json_bool(body, "stream"), Some(true));
        assert_eq!(body.as_slice(), original.as_slice());
        assert_eq!(
            top_level_json_bool(br#"{"future":{"stream":true}}"#, "stream"),
            None
        );
        assert_eq!(
            top_level_json_bool(br#"{"stream":false}"#, "stream"),
            Some(false)
        );
        assert_eq!(top_level_json_bool(br#"{"stream":"true"}"#, "stream"), None);
        assert_eq!(
            top_level_json_string(
                br#"{"future":{"model":"nested"},"model":"top-level"}"#,
                "model"
            ),
            Some("top-level".to_string())
        );
    }
}
