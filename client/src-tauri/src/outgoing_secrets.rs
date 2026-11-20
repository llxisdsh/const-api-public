//! Consumer-side protection of prompt text. Supplier credentials, opaque media
//! and signed reasoning are outside the prompt boundary. No reverse map exists.
use crate::surface::ApiOperation;
use bytes::{BufMut, Bytes};
use regex::Regex;
use reqwest::header::{CONTENT_ENCODING, CONTENT_LENGTH, HeaderMap};
use ring::hmac;
use serde_json::Value;
use std::{borrow::Cow, io::Read, sync::LazyLock};

static SECRETS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?-u:\b)(?:sk-(?:api|lan)-[0-9a-f]{32}|sk-ca_[A-Za-z0-9_-]{43}|",
        r"sk-(?:proj-|svcacct-|ant-(?:api|oat)[0-9]{2}-)[A-Za-z0-9_-]{20,}|sk-[A-Za-z0-9]{32,}|",
        r"gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{40,}|glpat-[A-Za-z0-9_-]{20,}|",
        r"AIza[A-Za-z0-9_-]{35}|hf_[A-Za-z0-9]{30,}|gsk_[A-Za-z0-9]{40,}|xai-[A-Za-z0-9]{40,}|",
        r"npm_[A-Za-z0-9]{36}|pypi-[A-Za-z0-9_-]{50,}|(?:sk|rk)_live_[A-Za-z0-9]{20,})|",
        r"-----BEGIN (?:RSA |EC |DSA |OPENSSH |ENCRYPTED )?PRIVATE KEY-----",
        r"(?:[\r\n \tA-Za-z0-9+/=:,-]|\\[rn])+?-----END (?:RSA |EC |DSA |OPENSSH |ENCRYPTED )?PRIVATE KEY-----"
    )).expect("static credential patterns")
});

// A contact explicitly labelled in prose or a structured tool result.
// Unlabelled package names, Git remotes and author addresses are common in code.
const EMAIL: &str = r"[A-Za-z0-9._%+-]+@(?:[A-Za-z0-9-]+\.)+[A-Za-z]{2,63}\b";
fn contact_email_field(key: &str) -> bool {
    matches!(
        key,
        "email"
            | "email_address"
            | "contact_email"
            | "emailAddress"
            | "contactEmail"
            | "邮箱"
            | "联系邮箱"
            | "电子邮箱"
    )
}
static EMAIL_VALUE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"\A{EMAIL}\z")).unwrap());
static LABELLED_EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r#"(?P<label>(?:(?i:\b(?:contact[ _-]?email|e-?mail(?:[ _-]address)?))|(?:联系|电子)?邮箱)["']?[ \t]*[=:：][ \t]*["']?)(?P<address>{EMAIL})"#)).unwrap()
});
static JSON_STRING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""(?:[^"\\]|\\.)*""#).expect("JSON string token"));

struct Mask {
    session_key: hmac::Key,
    replacements: usize,
}

impl Mask {
    fn placeholder(&mut self, kind: &str, value: &str) -> String {
        self.replacements += 1;
        let digest = hmac::sign(&self.session_key, value.as_bytes());
        format!("[CONST_{kind}_{}]", hex::encode(&digest.as_ref()[..12]))
    }

    fn text(&mut self, text: &mut String) {
        if text
            .strip_prefix("data:")
            .and_then(|uri| uri.split_once(','))
            .is_some_and(|(meta, bytes)| {
                meta.ends_with(";base64")
                    && bytes.bytes().all(|b| {
                        b.is_ascii_alphanumeric()
                            || matches!(b, b'+' | b'/' | b'=' | b'-' | b'_' | b'\r' | b'\n')
                    })
            })
        {
            return;
        }
        let result = SECRETS.replace_all(text, |m: &regex::Captures<'_>| {
            let token = m.get(0).unwrap();
            let identifier_byte = |b: &u8| b.is_ascii_alphanumeric() || matches!(*b, b'_' | b'-');
            if !token.as_str().starts_with("-----")
                && (text.as_bytes()[..token.start()]
                    .last()
                    .is_some_and(identifier_byte)
                    || text
                        .as_bytes()
                        .get(token.end())
                        .is_some_and(identifier_byte))
            {
                return token.as_str().to_owned();
            }
            self.placeholder("SECRET", token.as_str())
        });
        if let Cow::Owned(replaced) = result {
            *text = replaced;
        }
        if text.contains('@') {
            let result = LABELLED_EMAIL.replace_all(text, |m: &regex::Captures<'_>| {
                format!(
                    "{}{}",
                    &m["label"],
                    self.placeholder("EMAIL", &m["address"])
                )
            });
            if let Cow::Owned(replaced) = result {
                *text = replaced;
            }
        }
    }

    // Tool outputs can contain arbitrary structured user data. Only their
    // values are text; changing property names would change the tool contract.
    fn data(&mut self, value: &mut Value) {
        match value {
            Value::String(text) => self.text(text),
            Value::Array(items) => items.iter_mut().for_each(|v| self.data(v)),
            Value::Object(map) => {
                for (key, value) in map {
                    if contact_email_field(key)
                        && let Value::String(address) = value
                        && EMAIL_VALUE.is_match(address)
                    {
                        *address = self.placeholder("EMAIL", address);
                    } else {
                        self.data(value);
                    }
                }
            }
            _ => {}
        }
    }

    // Chat and Responses encode function arguments as JSON inside a string.
    // Only rewrite changed string tokens. Re-encoding the entire argument would
    // change whitespace, duplicate keys and numbers outside serde_json's integer range.
    fn tool_data(&mut self, value: &mut Value) {
        let Value::String(text) = value else {
            return self.data(value);
        };
        if !text.trim_start().starts_with(['{', '['])
            || serde_json::from_str::<&serde_json::value::RawValue>(text).is_err()
        {
            return self.text(text);
        }
        let mut result = String::new();
        let mut copied = 0;
        let mut contact = false;
        for token in JSON_STRING.find_iter(text) {
            let Ok(mut decoded) = serde_json::from_str::<String>(token.as_str()) else {
                contact = false;
                continue;
            };
            if let Some(after_colon) = text[token.end()..].trim_start().strip_prefix(':') {
                contact =
                    contact_email_field(&decoded) && after_colon.trim_start().starts_with('"');
                continue;
            }
            let before = self.replacements;
            if contact && EMAIL_VALUE.is_match(&decoded) {
                decoded = self.placeholder("EMAIL", &decoded);
            } else {
                self.text(&mut decoded);
            }
            contact = false;
            if self.replacements != before {
                result.push_str(&text[copied..token.start()]);
                result.push_str(&Value::String(decoded).to_string());
                copied = token.end();
            }
        }
        if copied > 0 {
            result.push_str(&text[copied..]);
            *text = result;
        }
    }

    // Visit tool declarations, leaving grammars, schema keys, names and enums intact.
    fn tool_descriptions(&mut self, value: &mut Value) {
        match value {
            Value::Array(items) => items
                .iter_mut()
                .for_each(|item| self.tool_descriptions(item)),
            Value::Object(map) => {
                if let Some(Value::String(description)) = map.get_mut("description") {
                    self.text(description);
                }
                for field in [
                    "function",
                    "functionDeclarations",
                    "function_declarations",
                    "tools",
                ] {
                    if let Some(definition) = map.get_mut(field) {
                        self.tool_descriptions(definition);
                    }
                }
            }
            _ => {}
        }
    }

    fn content(&mut self, value: &mut Value, in_tool_result: bool) {
        match value {
            Value::String(_) if in_tool_result => self.tool_data(value),
            Value::String(text) => self.text(text),
            Value::Array(items) => items
                .iter_mut()
                .for_each(|v| self.content(v, in_tool_result)),
            Value::Object(map) => {
                let kind = map.get("type").and_then(Value::as_str).unwrap_or_default();
                if matches!(kind, "thinking" | "redacted_thinking" | "reasoning")
                    || map.get("thought").and_then(Value::as_bool) == Some(true)
                    || ["thoughtSignature", "thought_signature"]
                        .iter()
                        .any(|field| {
                            map.get(*field)
                                .and_then(Value::as_str)
                                .is_some_and(|s| !s.is_empty())
                        })
                {
                    return;
                }
                let is_tool = kind.ends_with("tool_use") || kind.ends_with("_call");
                let tool_result = kind == "tool_result"
                    || matches!(
                        map.get("role").and_then(Value::as_str),
                        Some("tool" | "function")
                    );
                if matches!(kind, "additional_tools" | "tool_search_output")
                    && let Some(tools) = map.get_mut("tools")
                {
                    self.tool_descriptions(tools);
                }
                if is_tool {
                    for field in ["input", "action", "operation"] {
                        if let Some(data) = map.get_mut(field) {
                            self.data(data);
                        }
                    }
                }
                for field in [
                    "text",
                    "content",
                    "parts",
                    "tool_calls",
                    "function_call",
                    "function",
                    "tool_result",
                ] {
                    if let Some(v) = map.get_mut(field) {
                        self.content(v, in_tool_result || (field == "content" && tool_result));
                    }
                }
                if let Some(arguments) = map.get_mut("arguments") {
                    self.tool_data(arguments);
                }
                if let Some(output) = map.get_mut("output") {
                    if let Value::Array(items) = output {
                        for item in items {
                            match item.get("type").and_then(Value::as_str) {
                                Some(
                                    "text" | "input_text" | "output_text" | "input_image"
                                    | "input_file",
                                ) => self.content(item, true),
                                _ => self.data(item),
                            }
                        }
                    } else {
                        self.tool_data(output);
                    }
                }
                for (field, data) in [
                    ("functionCall", "args"),
                    ("functionResponse", "response"),
                    ("function_call", "args"),
                    ("function_response", "response"),
                    ("executableCode", "code"),
                    ("executable_code", "code"),
                    ("codeExecutionResult", "output"),
                    ("code_execution_result", "output"),
                ] {
                    if let Some(v) = map.get_mut(field).and_then(|v| v.get_mut(data)) {
                        self.data(v);
                    }
                }
            }
            _ => {}
        }
    }
}

enum PromptEnvelope<'a> {
    Direct,
    GeminiCountTokens,
    ResponsesSocket(&'a mut Option<String>),
}

fn protected_json(
    raw: &[u8],
    key: &str,
    headers: &HeaderMap,
    envelope: PromptEnvelope<'_>,
) -> Result<Option<Vec<u8>>, &'static str> {
    let text = std::str::from_utf8(raw).map_err(|_| "prompt protection requires UTF-8 JSON")?;
    // JSON can escape a token prefix as Unicode or a slash inside PEM data.
    let has_hint = text.contains("\\u")
        || text.contains("-----BEGIN")
        || text.contains('@')
        || SECRETS.is_match(text);
    // A first WS turn can carry the only session ID even when its text is clean.
    let header_id = crate::supplier::subscription_header_cache_identity(headers);
    let is_socket = matches!(envelope, PromptEnvelope::ResponsesSocket(_));
    let is_gemini_count = matches!(envelope, PromptEnvelope::GeminiCountTokens);
    let may_set_session = is_socket
        && header_id.is_none()
        && [
            "session",
            "thread_id",
            "conversation",
            "prompt_cache_key",
            "metadata",
            "\\u",
        ]
        .iter()
        .any(|hint| text.contains(hint));
    if !has_hint && !may_set_session {
        return Ok(None);
    }
    let mut request: Value = serde_json::from_slice(raw).map_err(|_| "invalid prompt JSON")?;
    let explicit = header_id
        .or_else(|| crate::supplier::subscription_body_cache_identity_from_value(&request));
    let session = match envelope {
        PromptEnvelope::ResponsesSocket(previous) => {
            if let Some(current) = explicit {
                *previous = Some(current);
            }
            previous.as_deref().unwrap_or_default()
        }
        _ => explicit.as_deref().unwrap_or_default(),
    };
    if !has_hint {
        return Ok(None);
    }
    let root = hmac::Key::new(hmac::HMAC_SHA256, key.as_bytes());
    let mut ctx = hmac::Context::with_key(&root);
    ctx.update(b"const-prompt-protection-v1\0");
    ctx.update(session.as_bytes());
    let mut mask = Mask {
        session_key: hmac::Key::new(hmac::HMAC_SHA256, ctx.sign().as_ref()),
        replacements: 0,
    };
    let mut fields = |payload: &mut Value| {
        for field in ["tools", "functions"] {
            if let Some(definitions) = payload.get_mut(field) {
                mask.tool_descriptions(definitions);
            }
        }
        for field in [
            "system",
            "instructions",
            "systemInstruction",
            "system_instruction",
            "messages",
            "input",
            "contents",
            "items",
        ] {
            if let Some(v) = payload.get_mut(field) {
                mask.content(v, false);
            }
        }
    };
    fields(&mut request);
    if is_gemini_count {
        for field in ["generateContentRequest", "generate_content_request"] {
            if let Some(generation) = request.get_mut(field) {
                fields(generation);
            }
        }
    }
    if is_socket
        && request.get("type").and_then(Value::as_str) == Some("response.create")
        && let Some(response) = request.get_mut("response")
    {
        fields(response);
    }
    if mask.replacements > 0 {
        encode_protected_prompt(&request, crate::MODEL_MESSAGE_MAX_BYTES).map(Some)
    } else {
        Ok(None)
    }
}

fn encode_protected_prompt(value: &Value, limit: usize) -> Result<Vec<u8>, &'static str> {
    // A placeholder can be longer than the original address. Bound the encoded
    // message before either HTTP or WebSocket hands it to the selected route.
    let mut bytes = Vec::new();
    serde_json::to_writer((&mut bytes).limit(limit).writer(), value)
        .map_err(|_| "protected prompt exceeds the local gateway limit")?;
    Ok(bytes)
}

pub(crate) fn protect_http(
    method: &str,
    path: &str,
    headers: &mut HeaderMap,
    body: Bytes,
    key: &str,
) -> Result<Bytes, &'static str> {
    use ApiOperation::*;
    let Some(route) = crate::surface::resolve_api_route(method, path, false) else {
        return Ok(body);
    };
    if !matches!(
        route.operation,
        ChatCompletions
            | Responses
            | ResponsesCompact
            | ResponsesInputTokens
            | Messages
            | CountTokens
            | GenerateContent
            | StreamGenerateContent
            | ConversationItemsCreate
            | ConversationsCreate
            | CachedContentsCreate
    ) {
        return Ok(body);
    }
    let limit = crate::MODEL_MESSAGE_MAX_BYTES;
    if body.len() > limit {
        return Err("prompt exceeds the local gateway limit");
    }
    let mut decoded = Vec::new();
    let mut enc = headers.get_all(CONTENT_ENCODING).iter();
    let encoding = enc
        .next()
        .map(|v| v.to_str())
        .transpose()
        .map_err(|_| "invalid prompt content encoding")?
        .unwrap_or("identity")
        .trim();
    if enc.next().is_some() {
        return Err("unsupported layered prompt content encoding");
    }
    let raw = if encoding.eq_ignore_ascii_case("identity") {
        body.as_ref()
    } else {
        let reader: Box<dyn Read + '_> = if encoding.eq_ignore_ascii_case("gzip") {
            Box::new(flate2::read::MultiGzDecoder::new(body.as_ref()))
        } else if encoding.eq_ignore_ascii_case("zstd") {
            Box::new(
                zstd::stream::read::Decoder::new(body.as_ref())
                    .map_err(|_| "invalid zstd prompt")?,
            )
        } else {
            return Err("unsupported prompt content encoding");
        };
        reader
            .take(limit as u64 + 1)
            .read_to_end(&mut decoded)
            .map_err(|_| "cannot decode protected prompt")?;
        if decoded.len() > limit {
            return Err("decoded prompt exceeds the local gateway limit");
        }
        decoded.as_slice()
    };
    let envelope =
        if route.surface == crate::surface::ApiSurface::Gemini && route.operation == CountTokens {
            PromptEnvelope::GeminiCountTokens
        } else {
            PromptEnvelope::Direct
        };
    let Some(masked) = protected_json(raw, key, headers, envelope)? else {
        return Ok(body);
    };
    headers.remove(CONTENT_ENCODING);
    headers.remove(CONTENT_LENGTH);
    Ok(masked.into())
}

pub(crate) fn protect_ws(
    message: warp::ws::Message,
    key: &str,
    headers: &HeaderMap,
    session: &mut Option<String>,
) -> warp::ws::Message {
    if !message.is_text() && !message.is_binary() {
        return message;
    }
    match protected_json(
        message.as_bytes(),
        key,
        headers,
        PromptEnvelope::ResponsesSocket(session),
    ) {
        Ok(None) => message,
        Ok(Some(masked)) if message.is_binary() => warp::ws::Message::binary(masked),
        Ok(Some(masked)) => {
            warp::ws::Message::text(String::from_utf8(masked).expect("JSON serializer emits UTF-8"))
        }
        // The existing response.create validator reports a protocol error. A
        // frame we could not inspect must never be forwarded in its original form.
        Err(_) => warp::ws::Message::text("{}"),
    }
}

#[cfg(test)]
pub(crate) const SYNTHETIC_KEY: &str = concat!("sk-", "proj-", "SYNTHETICFIXTURE0123456789");

#[cfg(test)]
mod tests {
    use super::SYNTHETIC_KEY as CANARY;
    use super::*;
    use serde_json::json;

    #[test]
    fn masks_known_prompt_fields_consistently_on_every_protocol_and_ws_turn() {
        let mut headers = HeaderMap::new();
        headers.insert("session-id", "conversation-a".parse().unwrap());
        let mut expected = String::new();
        for (path, payload) in [
            (
                "/v1/chat/completions",
                json!({"messages":[{"role":"tool","content":CANARY}]}),
            ),
            (
                "/v1/responses",
                json!({"input":[{"type":"function_call_output","call_id":"call-1","output": CANARY}]}),
            ),
            (
                "/v1/responses/compact",
                json!({"input":[{"role":"user","content":[{"type":"input_text","text":CANARY}]}]}),
            ),
            (
                "/anthropic/v1/messages/count_tokens",
                json!({"system":CANARY,"messages":[]}),
            ),
            (
                "/anthropic/v1/messages",
                json!({"messages":[{"role":"user","content":[{"type":"tool_result","content":CANARY}]}]}),
            ),
            (
                "/gemini/v1beta/models/gemini-test:generateContent",
                json!({"contents":[{"parts":[{"functionResponse":{"name":"show","response":{"nested":[CANARY]}}}]}]}),
            ),
            (
                "/gemini/v1beta/models/gemini-test:countTokens",
                json!({"generateContentRequest":{"model":"models/gemini-test","contents":[{"parts":[{"text":CANARY}]}]}}),
            ),
            (
                "/gemini/v1beta/models/gemini-test:countTokens",
                json!({"generate_content_request":{"system_instruction":{"parts":[{"text":CANARY}]}}}),
            ),
        ] {
            let body = protect_http(
                "POST",
                path,
                &mut headers,
                payload.to_string().into(),
                "caller",
            )
            .unwrap();
            let result = std::str::from_utf8(&body).unwrap();
            assert!(!result.contains(CANARY), "{path}");
            let start = result.find("[CONST_SECRET_").unwrap();
            let end = start + result[start..].find(']').unwrap() + 1;
            if expected.is_empty() {
                expected = result[start..end].to_string();
            }
            assert_eq!(&result[start..end], expected);
        }
        let mut socket_session = None;
        for event in [
            json!({"type":"response.create","input":CANARY}),
            json!({"type":"response.create","previous_response_id":"resp-1","input":[{"type":"message","content":CANARY}]}),
            json!({"type":"response.create","response":{"input":CANARY},"input":CANARY}),
        ] {
            let raw = event.to_string();
            let masked = protect_ws(
                warp::ws::Message::binary(raw.as_bytes().to_vec()),
                "caller",
                &headers,
                &mut socket_session,
            );
            assert!(masked.is_binary());
            let masked = std::str::from_utf8(masked.as_bytes()).unwrap();
            assert!(!masked.contains(CANARY) && masked.contains(&expected));
        }
        let event = json!({"input": CANARY}).to_string();
        headers.insert("session-id", "conversation-b".parse().unwrap());
        let other = protected_json(event.as_bytes(), "caller", &headers, PromptEnvelope::Direct)
            .unwrap()
            .unwrap();
        assert!(!std::str::from_utf8(&other).unwrap().contains(&expected));
        headers.insert("session-id", "conversation-a".parse().unwrap());
        let other = protected_json(
            event.as_bytes(),
            "other-lan-member",
            &headers,
            PromptEnvelope::Direct,
        )
        .unwrap()
        .unwrap();
        assert!(!std::str::from_utf8(&other).unwrap().contains(&expected));
    }

    #[test]
    fn masks_channel_credentials_and_labelled_contacts_without_changing_protocol_structure() {
        let local_key = crate::config::generate_local_api_key();
        let email = "alex+ops@example.org";
        let contact = format!("联系邮箱：{email}");
        let headers = HeaderMap::new();
        let body = json!({
            "tools":[{"type":"custom","name":"tool-name","description":local_key,"format":{"grammar":local_key}}],
            "input":[
                {"type":"local_shell_call","id":local_key,"action":{"command":["echo",local_key]}},
                {"type":"apply_patch_call","operation":{"diff":contact}},
                {"type":"mcp_tool_use","input":{"contactEmail":email,"signature":local_key}},
                {"type":"function_call_output","call_id":"call-1","output":[
                    {"type":"input_file","file_data":local_key},{"type":"input_text","text":local_key}]},
                {"role":"user","content":format!("git remote: git@example.org; normal text {email}")},
                {"type":"additional_tools","tools":[{"name":"fixture","description":local_key}]}
            ]
        });
        let protected = protected_json(
            &serde_json::to_vec(&body).unwrap(),
            "caller",
            &headers,
            PromptEnvelope::Direct,
        )
        .unwrap()
        .unwrap();
        let value: Value = serde_json::from_slice(&protected).unwrap();
        let masked = value["tools"][0]["description"].as_str().unwrap();
        assert!(masked.starts_with("[CONST_SECRET_"));
        assert_eq!(value["tools"][0]["format"], body["tools"][0]["format"]);
        assert_eq!(value["input"][0]["id"], local_key);
        assert_eq!(value["input"][0]["action"]["command"][1], masked);
        assert_eq!(value["input"][2]["input"]["signature"], masked);
        let masked_contact = value["input"][2]["input"]["contactEmail"].as_str().unwrap();
        assert!(masked_contact.starts_with("[CONST_EMAIL_"));
        assert_eq!(
            value["input"][1]["operation"]["diff"],
            format!("联系邮箱：{masked_contact}")
        );
        assert_eq!(
            value["input"][3]["output"][0],
            body["input"][3]["output"][0]
        );
        assert_eq!(value["input"][3]["output"][1]["text"], masked);
        assert_eq!(value["input"][4], body["input"][4]);
        assert_eq!(value["input"][5]["tools"][0]["description"], masked);

        let signed = json!({"functionCall":{"name":"test","args":{"key":local_key}},"thoughtSignature":"signature"});
        let gemini = json!({"tools":[{"functionDeclarations":[{"name":"test","description":local_key}]}],
            "contents":[{"parts":[signed,{"functionResponse":{"name":"test","response":{"contact_email":email}}}]}]});
        let protected = protected_json(
            &serde_json::to_vec(&gemini).unwrap(),
            "caller",
            &headers,
            PromptEnvelope::Direct,
        )
        .unwrap()
        .unwrap();
        let parts: Value = serde_json::from_slice(&protected).unwrap();
        assert_eq!(parts["contents"][0]["parts"][0], signed);
        assert_eq!(
            parts["contents"][0]["parts"][1]["functionResponse"]["response"]["contact_email"],
            masked_contact
        );
        assert_eq!(
            parts["tools"][0]["functionDeclarations"][0]["description"],
            masked
        );
    }

    #[test]
    fn protected_output_obeys_the_message_limit_after_substitution() {
        let value = json!({"input":"[CONST_EMAIL_fixture]"});
        let expected = serde_json::to_vec(&value).unwrap();
        assert_eq!(
            encode_protected_prompt(&value, expected.len()).unwrap(),
            expected
        );
        assert!(encode_protected_prompt(&value, expected.len() - 1).is_err());
    }

    #[test]
    fn encoded_tool_values_match_plain_text_and_preserve_unmodified_arguments() {
        let encoded = format!(
            r#"{{"n": 98765432101234567890, "key":"{}","contactEmail":"a\u0040example.org"}}"#,
            CANARY.replace("sk-", r"\u0073k-")
        );
        let untouched = r#"{  "n": 98765432101234567890, "key": "unchanged"  }"#;
        let payload = json!({
            "messages": [
                {"role":"assistant","tool_calls":[{"function":{"name":"f","arguments":encoded}},
                    {"function":{"name":"g","arguments":untouched}}]},
                {"role":"tool","content":encoded},
                {"role":"tool","content":[{"type":"text","text":encoded}]},
                {"role":"user","content":[{"type":"tool_result","content":[{"type":"text","text":encoded}]}]}],
            "input":[{"type":"function_call","arguments":encoded},
                {"type":"function_call_output","output":encoded},
                {"type":"function_call_output","output":[{"type":"input_text","text":encoded}]}],
            "contents":[{"parts":[{"executableCode":{"code":CANARY}},
                {"codeExecutionResult":{"output":CANARY}}]}],
            "instructions":CANARY
        });
        let raw = serde_json::to_vec(&payload).unwrap();
        let bytes = protected_json(&raw, "fixture", &HeaderMap::new(), PromptEnvelope::Direct)
            .unwrap()
            .unwrap();
        let masked: Value = serde_json::from_slice(&bytes).unwrap();
        let expected = &masked["instructions"];
        assert_ne!(expected, CANARY);
        for pointer in [
            "/messages/0/tool_calls/0/function/arguments",
            "/messages/1/content",
            "/messages/2/content/0/text",
            "/messages/3/content/0/content/0/text",
            "/input/0/arguments",
            "/input/1/output",
            "/input/2/output/0/text",
        ] {
            let argument = masked.pointer(pointer).unwrap().as_str().unwrap();
            assert!(argument.starts_with(r#"{"n": 98765432101234567890, "#));
            let decoded: Value = serde_json::from_str(argument).unwrap();
            assert_eq!(&decoded["key"], expected);
            assert!(
                decoded["contactEmail"]
                    .as_str()
                    .unwrap()
                    .starts_with("[CONST_EMAIL_")
            );
        }
        assert_eq!(
            masked["messages"][0]["tool_calls"][1]["function"]["arguments"],
            untouched
        );
        assert_eq!(
            &masked["contents"][0]["parts"][0]["executableCode"]["code"],
            expected
        );
        assert_eq!(
            &masked["contents"][0]["parts"][1]["codeExecutionResult"]["output"],
            expected
        );
    }

    #[test]
    fn credential_rules_mask_complete_tokens_and_keep_longer_unrelated_identifiers() {
        for (prefix, size) in [
            ("sk-lan-", 32),
            ("sk-ca_", 43),
            ("glpat-", 20),
            ("hf_", 30),
            ("gsk_", 40),
            ("xai-", 40),
            ("npm_", 36),
            ("pypi-", 50),
            ("rk_live_", 24),
        ] {
            let token = format!("{prefix}{}", "a".repeat(size));
            let raw = json!({"input":format!("use {token}.")}).to_string();
            let changed = protected_json(
                raw.as_bytes(),
                "caller",
                &HeaderMap::new(),
                PromptEnvelope::Direct,
            )
            .unwrap()
            .unwrap();
            assert!(
                !std::str::from_utf8(&changed).unwrap().contains(&token),
                "{prefix}"
            );
        }
        let longer_identifier = format!("{}z", crate::config::generate_local_api_key());
        let raw = json!({"input":longer_identifier}).to_string();
        assert!(
            protected_json(
                raw.as_bytes(),
                "caller",
                &HeaderMap::new(),
                PromptEnvelope::Direct
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn websocket_continuations_reuse_the_scope_from_a_clean_first_turn() {
        let headers = HeaderMap::new();
        let mut remembered = None;
        let first =
            json!({"type":"response.create","prompt_cache_key":"turn-series","input":"hello"});
        let _ = protect_ws(
            warp::ws::Message::text(first.to_string()),
            "caller",
            &headers,
            &mut remembered,
        );
        let next = json!({"type":"response.create","previous_response_id":"resp-1","input":CANARY});
        let msg = protect_ws(
            warp::ws::Message::text(next.to_string()),
            "caller",
            &headers,
            &mut remembered,
        );
        let result: Value = serde_json::from_slice(msg.as_bytes()).unwrap();
        let http = json!({"input":CANARY,"prompt_cache_key":"turn-series"}).to_string();
        let expected: Value = serde_json::from_slice(
            &protected_json(http.as_bytes(), "caller", &headers, PromptEnvelope::Direct)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(result["input"], expected["input"]);
        assert_ne!(result["input"], CANARY);
        assert_eq!(result["previous_response_id"], "resp-1");
    }

    #[test]
    fn protects_pem_and_compressed_text_without_modifying_metadata_or_plain_requests() {
        let pem = format!(
            "-----BEGIN {kind}-----\nQUJD/RA=\n-----END {kind}-----",
            kind = "PRIVATE KEY"
        );
        let escaped = json!({"input": &pem}).to_string().replace('/', r"\/");
        let masked_pem = protected_json(
            escaped.as_bytes(),
            "caller",
            &HeaderMap::new(),
            PromptEnvelope::Direct,
        )
        .unwrap()
        .unwrap();
        let masked_pem: Value = serde_json::from_slice(&masked_pem).unwrap();
        assert!(
            masked_pem["input"]
                .as_str()
                .unwrap()
                .starts_with("[CONST_SECRET_")
        );
        let raw = json!({"model":"original", "metadata":{"id":CANARY}, "input":[{"role":"user","content":[
            {"type":"input_text","text":format!("密钥{CANARY}\n{pem}")},
            {"type":"input_image","image_url":"data:image/png;base64,ABCD"}
        ]}]}).to_string();
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_ENCODING, "zstd".parse().unwrap());
        let zipped = zstd::stream::encode_all(raw.as_bytes(), 1).unwrap();
        let masked = protect_http(
            "POST",
            "/v1/responses",
            &mut headers,
            zipped.into(),
            "caller",
        )
        .unwrap();
        let value: Value = serde_json::from_slice(&masked).unwrap();
        let text = value
            .pointer("/input/0/content/0/text")
            .unwrap()
            .as_str()
            .unwrap();
        assert_eq!(text.matches("[CONST_SECRET_").count(), 2);
        assert_eq!(value["metadata"]["id"], CANARY);
        assert_eq!(
            value.pointer("/input/0/content/1/image_url").unwrap(),
            "data:image/png;base64,ABCD"
        );
        assert!(!headers.contains_key(CONTENT_ENCODING));
        let original = Bytes::from_static(br#"{ "input": "hello", "unknown": 1 }"#);
        let unchanged = protect_http(
            "POST",
            "/v1/responses",
            &mut headers,
            original.clone(),
            "caller",
        )
        .unwrap();
        assert_eq!(original.as_ptr(), unchanged.as_ptr());
        headers.insert(CONTENT_ENCODING, "uninspectable".parse().unwrap());
        assert!(protect_http("POST", "/v1/responses", &mut headers, original, "caller").is_err());
        let invalid = protect_ws(
            warp::ws::Message::text(format!("{{{CANARY}")),
            "caller",
            &headers,
            &mut None,
        );
        assert_eq!(invalid.to_str().unwrap(), "{}");
    }
}
