//! Identity of the official OpenCode client at OpenCode's inference API.
//!
//! The Console subscription adapter speaks for OpenCode, not for CONST. Its
//! requests carry the captured 1.19.0 User-Agent, OpenCode-shaped session and
//! turn IDs, and no `x-session-affinity`/`X-Session-Id`. Free-model requests
//! retain the previously measured streaming and `bash`/`read` declarations.
//! These declarations do not install tools or grant execution permissions.
//! Free inference also uses a small environment block and the dedicated Bun
//! TLS profile, verified against released 2.1.3/2.3.0. No tool is executed here.
//! See docs/coding-gateways.md's 2026-11-16 recheck.

use crate::{model::ChannelConfig, protocol::kind::ProtocolKind};
use anyhow::Result;
use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Mutex, OnceLock},
};

pub(crate) mod transport;

/// Captured from OpenCode 1.19.0; revalidated with the current free-tier contract.
pub(crate) const USER_AGENT_VALUE: &str =
    "opencode/1.19.0 ai-sdk/provider-utils/4.0.23 runtime/bun/1.3.14";
pub(crate) const SESSION_HEADER: &str = "x-opencode-session";
pub(crate) const SESSION_ID_HEADER: &str = "x-opencode-session-id";
const REQUEST_HEADER: &str = "x-opencode-request";
const CLIENT_HEADER: &str = "x-opencode-client";
const PROJECT_HEADER: &str = "x-opencode-project";

const SESSION_PREFIX: &str = "ses_";
const REQUEST_PREFIX: &str = "msg_";
const ID_RANDOM_LEN: usize = 14;
const ID_TIME_LEN: usize = 12;
const SESSION_CACHE_LIMIT: usize = 512;
const BASE62: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

const FREE_TOOL_DESCRIPTION: &str = "Not available in this session. Never call this tool.";
const BASH_SCHEMA: &str = r#"{"type":"object","properties":{"command":{"type":"string","description":"The command to execute"},"timeout":{"type":"integer","description":"Optional timeout in milliseconds"},"workdir":{"type":"string","description":"The working directory to run the command in"}},"required":["command"]}"#;
const READ_SCHEMA: &str = r#"{"type":"object","properties":{"filePath":{"type":"string","description":"The absolute path to the file or directory to read"},"offset":{"type":"integer","description":"The line number to start reading from (1-indexed)"},"limit":{"type":"integer","description":"The maximum number of lines to read"}},"required":["filePath"]}"#;
const FREE_TOOLS: [(&str, &str); 2] = [("bash", BASH_SCHEMA), ("read", READ_SCHEMA)];
// Fixed metadata, not a substitute for the caller's instructions or a claim
// about its filesystem. Never send a supplier machine's local working directory.
const FREE_ENVIRONMENT: &str =
    "<env>\nWorking directory: not provided by this API gateway.\nPlatform: unknown\n</env>";

struct SessionIds {
    by_seed: HashMap<String, String>,
    order: VecDeque<String>,
}

static SESSION_IDS: OnceLock<Mutex<SessionIds>> = OnceLock::new();
static ID_CLOCK: OnceLock<Mutex<(i64, u64)>> = OnceLock::new();

/// Make one Console request look like OpenCode's own. A caller's valid
/// OpenCode session is kept; otherwise the cache identity is mapped to a
/// stable OpenCode-shaped session so a conversation keeps its routing.
pub(crate) fn apply_headers(headers: &mut HeaderMap, session_seed: Option<&str>) {
    headers.insert(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE));
    headers.remove("x-session-affinity");
    headers.remove("x-session-id");

    let valid_session = |name| {
        headers
            .get(name)
            .filter(|value| {
                value.to_str().is_ok_and(|value| {
                    has_id_shape(value, SESSION_PREFIX)
                        || value.strip_prefix(SESSION_PREFIX).is_some_and(|suffix| {
                            suffix.len() == 64
                                && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
                        })
                })
            })
            .cloned()
    };
    let affinity = valid_session(SESSION_HEADER);
    let current = valid_session(SESSION_ID_HEADER);
    // Parent/child and forked conversations share routing affinity, not their
    // actual session ID. Only fill a missing identity; never collapse the pair.
    let affinity = affinity.or_else(|| current.clone()).unwrap_or_else(|| {
        HeaderValue::from_str(&session_id(session_seed))
            .expect("generated OpenCode IDs are valid headers")
    });
    headers.insert(SESSION_HEADER, affinity.clone());
    headers.insert(SESSION_ID_HEADER, current.unwrap_or(affinity));

    let existing_request = headers
        .get(REQUEST_HEADER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| has_id_shape(value, REQUEST_PREFIX));
    if !existing_request {
        headers.insert(
            REQUEST_HEADER,
            HeaderValue::from_str(&new_id(REQUEST_PREFIX, false))
                .expect("generated OpenCode IDs are valid headers"),
        );
    }
    headers.insert(CLIENT_HEADER, HeaderValue::from_static("cli"));
    headers
        .entry(PROJECT_HEADER)
        .or_insert(HeaderValue::from_static("global"));
}

/// Free models are the ones whose ID says so, the small known set whose IDs
/// do not, and catalog entries priced at zero.
pub(crate) fn is_free_model(channel: &ChannelConfig, model: &str) -> bool {
    let model = crate::config::without_context_hint(model).trim();
    let short = model
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .to_ascii_lowercase();
    if short.ends_with("-free") || matches!(short.as_str(), "big-pickle" | "grok-code") {
        return true;
    }
    channel.capability_profiles.iter().any(|profile| {
        profile.probe_cost_nanos == Some(0)
            && crate::config::model_name_matches(&profile.model_pattern, model)
    })
}

/// Apply the measured Console free-model contract: environment, stream, bash/read
/// declarations. Only append missing declarations; never rewrite a caller's
/// tools, including zero-argument and composed schemas. Tool-free callers use
/// `tool_choice: none`, so compatibility declarations cannot become calls.
/// Returns `None` when the body already complies.
pub(crate) fn apply_free_tier_body_contract(
    protocol: ProtocolKind,
    body: &[u8],
) -> Result<Option<Vec<u8>>> {
    if !matches!(
        protocol,
        ProtocolKind::OpenAiChat | ProtocolKind::OpenAiResponses | ProtocolKind::AnthropicMessages
    ) {
        return Ok(None);
    }
    let Value::Object(object) = serde_json::from_slice::<Value>(body)? else {
        return Ok(None);
    };
    let tools = match object.get("tools") {
        None | Some(Value::Null) => &[][..],
        Some(Value::Array(tools)) => tools.as_slice(),
        Some(_) => anyhow::bail!("OpenCode free-model tools must be a JSON array"),
    };
    let mut missing = Vec::new();
    for (name, schema) in FREE_TOOLS {
        if !tools
            .iter()
            .any(|tool| tool_name(tool, protocol) == Some(name))
        {
            missing.push(serde_json::to_string(&free_tier_tool(
                protocol, name, schema,
            ))?);
        }
    }
    let original = std::str::from_utf8(body)?;
    let mut adjusted = free_environment(protocol, &object, original)?;
    if object.get("stream").and_then(Value::as_bool) != Some(true) {
        adjusted =
            crate::supplier::rewrite_top_level_json_field(&adjusted, "stream", "true", true)?;
    }
    if !missing.is_empty() {
        // Keep the original tool tokens intact, not just their parsed values.
        let mut expanded = if tools.is_empty() {
            "[".to_string()
        } else {
            let raw = crate::supplier::top_level_json_raw_token(body, "tools")
                .expect("validated tools array has a raw JSON token");
            format!("{},", &raw[..raw.len() - 1])
        };
        expanded.push_str(&missing.join(","));
        expanded.push(']');
        adjusted =
            crate::supplier::rewrite_top_level_json_field(&adjusted, "tools", &expanded, true)?;
        if tools.is_empty() {
            let choice = if protocol == ProtocolKind::AnthropicMessages {
                r#"{"type":"none"}"#
            } else {
                r#""none""#
            };
            adjusted = crate::supplier::rewrite_top_level_json_field(
                &adjusted,
                "tool_choice",
                choice,
                true,
            )?;
        }
    }
    Ok((adjusted != original).then(|| adjusted.into_bytes()))
}

fn has_environment(value: &Value) -> bool {
    match value {
        Value::String(text) => {
            text.contains("<env>")
                && text.contains("Working directory:")
                && text.contains("Platform:")
                && text.contains("</env>")
        }
        Value::Array(parts) => parts
            .iter()
            .any(|part| part.get("text").is_some_and(has_environment)),
        _ => false,
    }
}

fn free_environment(
    protocol: ProtocolKind,
    object: &serde_json::Map<String, Value>,
    original: &str,
) -> Result<String> {
    let has_message_environment = |messages: &Value| {
        messages.as_array().is_some_and(|messages| {
            messages.iter().any(|message| {
                matches!(message["role"].as_str(), Some("system" | "developer"))
                    && has_environment(&message["content"])
            })
        })
    };
    let (field, replacement) = match protocol {
        ProtocolKind::OpenAiChat => {
            let messages = object
                .get("messages")
                .ok_or_else(|| anyhow::anyhow!("OpenCode free-model messages are required"))?;
            if has_message_environment(messages) {
                return Ok(original.to_string());
            }
            let messages = messages
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("OpenCode free-model messages must be an array"))?;
            let raw = crate::supplier::top_level_json_raw_token(original.as_bytes(), "messages")
                .expect("validated messages have a raw token");
            let prefix = json!({"role":"system","content":FREE_ENVIRONMENT}).to_string();
            (
                "messages",
                format!(
                    "[{prefix}{}{}",
                    if messages.is_empty() { "" } else { "," },
                    &raw[1..]
                ),
            )
        }
        ProtocolKind::OpenAiResponses => {
            if object.get("instructions").is_some_and(has_environment)
                || object.get("input").is_some_and(has_message_environment)
            {
                return Ok(original.to_string());
            }
            let instructions = match object.get("instructions") {
                None | Some(Value::Null) => FREE_ENVIRONMENT.to_string(),
                Some(Value::String(text)) => format!("{text}\n\n{FREE_ENVIRONMENT}"),
                _ => anyhow::bail!("OpenCode free-model instructions must be a string"),
            };
            ("instructions", serde_json::to_string(&instructions)?)
        }
        ProtocolKind::AnthropicMessages => {
            if object.get("system").is_some_and(has_environment) {
                return Ok(original.to_string());
            }
            let system = match object.get("system") {
                None | Some(Value::Null) => serde_json::to_string(FREE_ENVIRONMENT)?,
                Some(Value::String(text)) => {
                    serde_json::to_string(&format!("{text}\n\n{FREE_ENVIRONMENT}"))?
                }
                Some(Value::Array(parts)) => {
                    let raw =
                        crate::supplier::top_level_json_raw_token(original.as_bytes(), "system")
                            .expect("validated system has a raw token");
                    format!(
                        "{}{}{}]",
                        &raw[..raw.len() - 1],
                        if parts.is_empty() { "" } else { "," },
                        json!({"type":"text","text":FREE_ENVIRONMENT})
                    )
                }
                _ => anyhow::bail!("OpenCode free-model system must be text or content blocks"),
            };
            ("system", system)
        }
        _ => return Ok(original.to_string()),
    };
    crate::supplier::rewrite_top_level_json_field(original, field, &replacement, true)
}

fn tool_name(tool: &Value, protocol: ProtocolKind) -> Option<&str> {
    match protocol {
        ProtocolKind::OpenAiChat => tool.pointer("/function/name").and_then(Value::as_str),
        _ => tool.get("name").and_then(Value::as_str),
    }
}

fn free_tier_tool(protocol: ProtocolKind, name: &str, schema: &str) -> Value {
    let schema: Value = serde_json::from_str(schema).expect("built-in tool schemas are valid JSON");
    match protocol {
        ProtocolKind::OpenAiChat => json!({
            "type": "function",
            "function": {"name": name, "description": FREE_TOOL_DESCRIPTION, "parameters": schema}
        }),
        ProtocolKind::AnthropicMessages => json!({
            "name": name, "description": FREE_TOOL_DESCRIPTION, "input_schema": schema
        }),
        _ => json!({
            "type": "function", "name": name, "description": FREE_TOOL_DESCRIPTION, "parameters": schema
        }),
    }
}

fn session_id(seed: Option<&str>) -> String {
    let Some(seed) = seed.filter(|seed| !seed.trim().is_empty()) else {
        return new_id(SESSION_PREFIX, true);
    };
    let mut sessions = SESSION_IDS
        .get_or_init(|| {
            Mutex::new(SessionIds {
                by_seed: HashMap::new(),
                order: VecDeque::new(),
            })
        })
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(id) = sessions.by_seed.get(seed) {
        return id.clone();
    }
    let id = new_id(SESSION_PREFIX, true);
    if sessions.by_seed.len() >= SESSION_CACHE_LIMIT {
        if let Some(oldest) = sessions.order.pop_front() {
            sessions.by_seed.remove(&oldest);
        }
    }
    sessions.order.push_back(seed.to_string());
    sessions.by_seed.insert(seed.to_string(), id.clone());
    id
}

/// An identifier laid out like OpenCode's: prefix, 12 hex digits of a
/// millisecond clock plus a same-millisecond counter, then 14 random base62
/// digits. Session clocks are bit-inverted, as OpenCode sorts newest first.
fn new_id(prefix: &str, descending: bool) -> String {
    let now = crate::now_unix_millis();
    let mut clock = ID_CLOCK
        .get_or_init(|| Mutex::new((0, 0)))
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if now != clock.0 {
        *clock = (now, 0);
    }
    clock.1 += 1;
    let counter = clock.1;
    drop(clock);

    let current = (now.max(0) as u64)
        .wrapping_mul(0x1000)
        .wrapping_add(counter);
    let current = if descending { !current } else { current };
    let mut random = [0_u8; ID_RANDOM_LEN];
    for byte in &mut random {
        *byte = rand::random::<u8>();
    }
    let suffix: String = random
        .iter()
        .map(|byte| BASE62[*byte as usize % BASE62.len()] as char)
        .collect();
    format!(
        "{prefix}{:0width$x}{suffix}",
        current & 0x0000_ffff_ffff_ffff,
        width = ID_TIME_LEN
    )
}

fn has_id_shape(value: &str, prefix: &str) -> bool {
    let Some(rest) = value.strip_prefix(prefix) else {
        return false;
    };
    rest.len() == ID_TIME_LEN + ID_RANDOM_LEN
        && rest.as_bytes()[..ID_TIME_LEN]
            .iter()
            .all(u8::is_ascii_hexdigit)
        && rest.as_bytes()[ID_TIME_LEN..]
            .iter()
            .all(u8::is_ascii_alphanumeric)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel_from_supplier;

    fn headers() -> HeaderMap {
        HeaderMap::new()
    }

    #[test]
    fn identity_replaces_caller_transport_headers() {
        let mut headers = headers();
        headers.insert(USER_AGENT, HeaderValue::from_static("caller/1"));
        headers.insert("x-session-affinity", HeaderValue::from_static("affinity"));
        headers.insert("X-Session-Id", HeaderValue::from_static("session"));

        apply_headers(&mut headers, Some("conversation-a"));

        assert_eq!(headers[USER_AGENT], USER_AGENT_VALUE);
        assert!(!headers.contains_key("x-session-affinity"));
        assert!(!headers.contains_key("x-session-id"));
        let session = headers[SESSION_HEADER].to_str().unwrap();
        assert!(has_id_shape(session, SESSION_PREFIX));
        assert_eq!(headers[SESSION_ID_HEADER], session);
        assert!(has_id_shape(
            headers[REQUEST_HEADER].to_str().unwrap(),
            REQUEST_PREFIX
        ));
        assert_eq!(headers[CLIENT_HEADER], "cli");
        assert_eq!(headers[PROJECT_HEADER], "global");
    }

    #[test]
    fn conversation_session_is_stable_and_new_turns_get_new_request_ids() {
        let mut first = headers();
        apply_headers(&mut first, Some("conversation-a"));
        let session = first[SESSION_HEADER].clone();
        let request = first[REQUEST_HEADER].clone();

        let mut second = headers();
        apply_headers(&mut second, Some("conversation-a"));
        assert_eq!(second[SESSION_HEADER], session);
        assert_ne!(second[REQUEST_HEADER], request);

        let mut other = headers();
        apply_headers(&mut other, Some("conversation-b"));
        assert_ne!(other[SESSION_HEADER], session);
    }

    #[test]
    fn caller_opencode_session_is_preserved() {
        let mut headers = headers();
        let caller = format!("ses_{:012x}{}", 0x1234, "a".repeat(14));
        headers.insert(SESSION_HEADER, HeaderValue::from_str(&caller).unwrap());

        apply_headers(&mut headers, Some("conversation-a"));

        assert_eq!(headers[SESSION_HEADER], caller);
        assert_eq!(headers[SESSION_ID_HEADER], caller);
    }

    #[test]
    fn subscription_identity_opencode_keeps_parent_child_and_hashed_affinity() {
        for parent in [
            new_id(SESSION_PREFIX, true),
            format!("ses_{}", "a".repeat(64)),
        ] {
            let child = new_id(SESSION_PREFIX, true);
            let mut headers = headers();
            headers.insert(SESSION_HEADER, parent.parse().unwrap());
            headers.insert(SESSION_ID_HEADER, child.parse().unwrap());
            headers.insert("x-opencode-parent-session-id", parent.parse().unwrap());
            apply_headers(&mut headers, Some("must-not-replace-native"));
            assert_eq!(headers[SESSION_HEADER], parent);
            assert_eq!(headers[SESSION_ID_HEADER], child);
            assert_eq!(headers["x-opencode-parent-session-id"], parent);
        }
    }

    #[test]
    fn a_hashed_cache_identity_is_not_forwarded_as_a_session() {
        let mut headers = headers();
        apply_headers(
            &mut headers,
            Some("c1_0123456789abcdef0123456789abcdef01234567"),
        );
        let session = headers[SESSION_HEADER].to_str().unwrap();
        assert!(has_id_shape(session, SESSION_PREFIX));
    }

    #[test]
    fn free_models_are_detected_by_suffix_known_id_and_zero_price() {
        let mut channel = channel_from_supplier("http".into(), &crate::default_supplier_config());

        assert!(is_free_model(&channel, "deepseek-v4-flash-free"));
        assert!(is_free_model(&channel, "opencode/big-pickle"));
        assert!(!is_free_model(&channel, "claude-sonnet-5"));

        channel
            .capability_profiles
            .push(crate::model::ChannelCapabilityProfile {
                protocol: "openai_chat".into(),
                probe_cost_nanos: Some(0),
                model_pattern: "grok-code".into(),
                ..Default::default()
            });
        assert!(is_free_model(&channel, "grok-code"));
    }

    #[test]
    fn chat_contract_restores_free_tier_declarations_with_calls_disabled() {
        let body =
            br#"{"model":"big-pickle","messages":[{"role":"user","content":"hi"}],"stream":false}"#;
        let contract = apply_free_tier_body_contract(ProtocolKind::OpenAiChat, body)
            .unwrap()
            .expect("non-streaming body needs the contract");
        let body: Value = serde_json::from_slice(&contract).unwrap();
        assert_eq!(body["stream"], true);
        assert_eq!(body["tool_choice"], "none");
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        for (tool, name) in tools.iter().zip(["bash", "read"]) {
            assert_eq!(tool["function"]["name"], name);
            assert_eq!(tool["function"]["description"], FREE_TOOL_DESCRIPTION);
            assert!(
                tool["function"]["parameters"]["properties"]
                    .as_object()
                    .is_some_and(|properties| !properties.is_empty())
            );
        }
        assert!(
            apply_free_tier_body_contract(ProtocolKind::OpenAiChat, &contract)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn free_tier_tool_free_calls_are_disabled_in_each_protocol() {
        for protocol in [
            ProtocolKind::OpenAiChat,
            ProtocolKind::OpenAiResponses,
            ProtocolKind::AnthropicMessages,
        ] {
            for raw_tools in ["", ",\"tools\":[]", ",\"tools\":null"] {
                let body = format!(r#"{{"messages":[],"stream":true{raw_tools}}}"#);
                let updated = apply_free_tier_body_contract(protocol, body.as_bytes())
                    .unwrap()
                    .unwrap();
                let value: Value = serde_json::from_slice(&updated).unwrap();
                assert_eq!(value["tools"].as_array().unwrap().len(), 2);
                assert_eq!(
                    value["tool_choice"],
                    if protocol == ProtocolKind::AnthropicMessages {
                        json!({"type":"none"})
                    } else {
                        json!("none")
                    }
                );
                assert!(
                    apply_free_tier_body_contract(protocol, &updated)
                        .unwrap()
                        .is_none()
                );
            }
        }
    }

    #[test]
    fn free_tier_invalid_tools_are_not_silently_discarded() {
        assert!(
            apply_free_tier_body_contract(
                ProtocolKind::OpenAiChat,
                br#"{"stream":true,"tools":{"keep":"invalid caller input"}}"#
            )
            .is_err()
        );
    }

    #[test]
    fn responses_contract_uses_flat_tools_and_preserves_caller_tools() {
        let body = br#"{"model":"big-pickle","input":"hi","stream":true,"tools":[{"type":"function","name":"custom","parameters":{"type":"object","properties":{"x":{"type":"string"}}}}],"tool_choice":"auto"}"#;
        let updated = apply_free_tier_body_contract(ProtocolKind::OpenAiResponses, body)
            .unwrap()
            .unwrap();
        let value: Value = serde_json::from_slice(&updated).unwrap();
        let original: Value = serde_json::from_slice(body).unwrap();
        assert_eq!(value["tools"][0], original["tools"][0]);
        assert_eq!(value["tools"][1]["name"], "bash");
        assert_eq!(value["tools"][2]["name"], "read");
        assert_eq!(value["tool_choice"], "auto");
    }

    #[test]
    fn anthropic_contract_preserves_no_argument_tools() {
        let body = br#"{"model":"big-pickle","max_tokens":64,"messages":[],"tools":[{"name":"bash","input_schema":{"type":"object","properties":{}}}]}"#;
        let contract = apply_free_tier_body_contract(ProtocolKind::AnthropicMessages, body)
            .unwrap()
            .expect("stream needs the contract");
        let body: Value = serde_json::from_slice(&contract).unwrap();
        assert_eq!(body["stream"], true);
        let tools = body["tools"].as_array().unwrap();
        let bash = tools.iter().find(|tool| tool["name"] == "bash").unwrap();
        assert_eq!(
            bash["input_schema"],
            serde_json::json!({"type":"object","properties":{}})
        );
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[1]["name"], "read");
    }

    #[test]
    fn subscription_identity_opencode_preserves_tool_contract_bytes() {
        for (protocol, body) in [
            (
                ProtocolKind::OpenAiChat,
                r#"{ "messages":[], "stream":false, "tools":[{"type":"function","function":{"name":"bash","parameters":{"anyOf":[{"type":"object","properties":{}}]}}}],"tool_choice":"required" }"#,
            ),
            (
                ProtocolKind::OpenAiResponses,
                r#"{ "stream":false, "tools":[{"type":"function","name":"read","parameters":{"type":"object","properties":{}}}],"tool_choice":{"type":"function","name":"read"} }"#,
            ),
            (
                ProtocolKind::AnthropicMessages,
                r#"{ "stream":false, "tools":[{"name":"read","input_schema":{"oneOf":[{"type":"object"}]}}],"tool_choice":{"type":"any"} }"#,
            ),
        ] {
            let changed = apply_free_tier_body_contract(protocol, body.as_bytes())
                .unwrap()
                .unwrap();
            let text = std::str::from_utf8(&changed).unwrap();
            let original_tools =
                crate::supplier::top_level_json_raw_token(body.as_bytes(), "tools").unwrap();
            assert!(text.contains(&original_tools[1..original_tools.len() - 1]));
            let mut expected: Value = serde_json::from_str(body).unwrap();
            expected["stream"] = json!(true);
            match protocol {
                ProtocolKind::OpenAiChat => {
                    expected["messages"] = json!([{"role":"system","content":FREE_ENVIRONMENT}])
                }
                ProtocolKind::OpenAiResponses => expected["instructions"] = json!(FREE_ENVIRONMENT),
                ProtocolKind::AnthropicMessages => expected["system"] = json!(FREE_ENVIRONMENT),
                _ => unreachable!(),
            }
            let mut received: Value = serde_json::from_slice(&changed).unwrap();
            received["tools"].as_array_mut().unwrap().truncate(1);
            assert_eq!(received, expected);
            assert!(
                apply_free_tier_body_contract(protocol, &changed)
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn environment_preserves_history_instructions_and_cache_blocks() {
        let messages = r#"[{ "role":"system", "content":"Keep caller instructions." }, {"role":"user","content":"hello","extension":42}]"#;
        let chat = format!(r#"{{"messages":{messages}}}"#);
        let changed = apply_free_tier_body_contract(ProtocolKind::OpenAiChat, chat.as_bytes())
            .unwrap()
            .unwrap();
        assert!(
            std::str::from_utf8(&changed)
                .unwrap()
                .contains(&messages[1..messages.len() - 1])
        );
        let value: Value = serde_json::from_slice(&changed).unwrap();
        assert_eq!(value["messages"][0]["content"], FREE_ENVIRONMENT);
        assert_eq!(value["messages"].as_array().unwrap().len(), 3);

        let responses = br#"{"input":"hello","instructions":"Keep caller instructions."}"#;
        let changed = apply_free_tier_body_contract(ProtocolKind::OpenAiResponses, responses)
            .unwrap()
            .unwrap();
        let value: Value = serde_json::from_slice(&changed).unwrap();
        assert_eq!(
            value["instructions"],
            format!("Keep caller instructions.\n\n{FREE_ENVIRONMENT}")
        );
        assert_eq!(value["input"], "hello");

        let system = r#"[{ "type":"text", "text":"Keep caller instructions.", "cache_control":{"type":"ephemeral","ttl":"1h"}}]"#;
        let anthropic = format!(r#"{{"system":{system},"messages":[]}}"#);
        let changed =
            apply_free_tier_body_contract(ProtocolKind::AnthropicMessages, anthropic.as_bytes())
                .unwrap()
                .unwrap();
        assert!(
            std::str::from_utf8(&changed)
                .unwrap()
                .contains(&system[1..system.len() - 1])
        );
        let value: Value = serde_json::from_slice(&changed).unwrap();
        assert_eq!(value["system"][1]["text"], FREE_ENVIRONMENT);
        assert_eq!(value["system"].as_array().unwrap().len(), 2);
        assert!(
            apply_free_tier_body_contract(ProtocolKind::AnthropicMessages, &changed)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn caller_environment_is_retained_without_supplier_metadata() {
        let environment = "<env>\nWorking directory: /caller-project\nPlatform: linux\n</env>";
        for (protocol, body) in [
            (
                ProtocolKind::OpenAiChat,
                json!({"messages":[{"role":"developer","content":[{"type":"text","text":environment}]}]}),
            ),
            (
                ProtocolKind::OpenAiResponses,
                json!({"input":[{"role":"system","content":environment}]}),
            ),
            (
                ProtocolKind::AnthropicMessages,
                json!({"system":[{"type":"text","text":environment,"cache_control":{"type":"ephemeral"}}]}),
            ),
        ] {
            let original = body.to_string();
            let adjusted =
                free_environment(protocol, body.as_object().unwrap(), &original).unwrap();
            assert_eq!(adjusted, original);
        }
    }

    #[test]
    fn compliant_body_is_left_untouched() {
        let body = br#"{"model":"big-pickle","instructions":"<env>Working directory: caller-provided\nPlatform: linux</env>","stream":true,"tools":[{"type":"function","name":"bash","parameters":{"type":"object","properties":{"command":{"type":"string"}}}},{"type":"function","name":"read","parameters":{"type":"object","properties":{"filePath":{"type":"string"}}}}]}"#;
        assert!(
            apply_free_tier_body_contract(ProtocolKind::OpenAiResponses, body)
                .unwrap()
                .is_none()
        );
        assert!(
            apply_free_tier_body_contract(ProtocolKind::GeminiNative, body)
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn subscription_identity_opencode_http_wire_preserves_caller_tools() {
        use warp::Filter;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let route = warp::header::headers_cloned().and(warp::body::bytes())
            .map(move |headers: HeaderMap, body: bytes::Bytes| {
                tx.send((headers, body)).unwrap();
                warp::reply::with_header("data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"call-fixture\",\"function\":{\"name\":\"probe\",\"arguments\":\"{}\"}}]}}]}\n\ndata: [DONE]\n\n", "content-type", "text/event-stream")
            });
        let (address, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
        let server = tokio::spawn(server);
        let parent = new_id(SESSION_PREFIX, true);
        let child = new_id(SESSION_PREFIX, true);
        let mut headers = headers();
        headers.insert(SESSION_HEADER, parent.parse().unwrap());
        headers.insert(SESSION_ID_HEADER, child.parse().unwrap());
        apply_headers(&mut headers, None);
        let body = r#"{ "messages":[], "stream":false, "tools":[{"type":"function","function":{"name":"probe","parameters":{"type":"object","properties":{}}}}],"tool_choice":"required" }"#;
        let adjusted = apply_free_tier_body_contract(ProtocolKind::OpenAiChat, body.as_bytes())
            .unwrap()
            .unwrap();
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let response = client
            .post(format!("http://{address}/v1/chat/completions"))
            .headers(headers)
            .body(adjusted.clone())
            .send()
            .await
            .unwrap();
        assert!(response.text().await.unwrap().contains("call-fixture"));
        let (received, payload) = rx.recv().await.unwrap();
        server.abort();
        assert_eq!(received[USER_AGENT], USER_AGENT_VALUE);
        assert_eq!(received[SESSION_HEADER], parent);
        assert_eq!(received[SESSION_ID_HEADER], child);
        assert_eq!(payload.as_ref(), adjusted);
        let payload: Value = serde_json::from_slice(&payload).unwrap();
        let original: Value = serde_json::from_str(body).unwrap();
        assert_eq!(payload["tools"][0], original["tools"][0]);
        assert_eq!(payload["tools"].as_array().unwrap().len(), 3);
        assert_eq!(payload["tool_choice"], "required");
    }

    // Explicitly selected only. Never activates a channel or writes config;
    // uses the existing, unexpired Dev credential without refresh or migration.
    #[tokio::test]
    #[ignore = "uses four short free-model requests on the existing Dev OpenCode Console channel"]
    async fn live_dev_opencode_subscription_identity_smoke() -> Result<()> {
        // Explicit live-test input only. Keep crate::home_dir() isolated for
        // every runtime side effect; never point the whole test process at home.
        let root = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("user home is unavailable"))?
            .join(".const-api-dev")
            .canonicalize()?;
        let config: crate::model::ClientConfig =
            serde_json::from_slice(&std::fs::read(root.join("client.json"))?)?;
        let channel = config
            .channels
            .into_iter()
            .find(|channel| {
                channel.source_driver() == crate::source_driver::SourceDriverId::OpencodeConsole
            })
            .ok_or_else(|| anyhow::anyhow!("Dev OpenCode Console channel missing"))?;
        let path = std::path::PathBuf::from(&channel.subscription.credential_ref).canonicalize()?;
        anyhow::ensure!(path.starts_with(&root), "test only reads Dev credentials");
        let credential: Value = serde_json::from_slice(&std::fs::read(path)?)?;
        anyhow::ensure!(
            !crate::detection::credential_needs_refresh(&credential),
            "Dev credential needs refresh; test does not write credentials"
        );
        let model = "minimax-m3.1-flash-free";
        anyhow::ensure!(
            channel.models.iter().any(|id| id == model),
            "smoke model is not in Dev channel"
        );
        let client = crate::http_client::channel_client(channel.v2.use_system_proxy)?;
        for (path, body) in [
            (
                "/v1/chat/completions".to_string(),
                json!({"model":model,"messages":[{"role":"user","content":"Reply only OK."}],"max_tokens":64,"stream":false}),
            ),
            (
                "/v1/responses".to_string(),
                json!({"model":model,"input":"Reply only OK.","max_output_tokens":64,"stream":true}),
            ),
            (
                "/anthropic/v1/messages".to_string(),
                json!({"model":model,"messages":[{"role":"user","content":"Reply only OK."}],"max_tokens":64,"stream":false}),
            ),
            (
                format!("/gemini/v1beta/models/{model}:generateContent"),
                json!({"contents":[{"role":"user","parts":[{"text":"Reply only OK."}]}],"generationConfig":{"maxOutputTokens":64}}),
            ),
        ] {
            // Check again before each call: never refresh/write the user's credential.
            anyhow::ensure!(
                !crate::detection::credential_needs_refresh(&credential),
                "Dev credential needs refresh"
            );
            let execution = crate::channel_executor::execute_channel_request(
                &client,
                &channel,
                crate::channel_executor::ChannelRequest {
                    method: reqwest::Method::POST,
                    path: path.clone(),
                    raw_query: String::new(),
                    has_query: false,
                    headers: HeaderMap::new(),
                    body: body.to_string().into(),
                    stream_requested: None,
                    selected_upstream_model: Some(model.to_string()),
                    selected_target_protocol: None,
                    cache_identity: Some("dev-free-contract-smoke".into()),
                    safety_identifier: None,
                },
            )
            .await?;
            anyhow::ensure!(
                execution.upstream_model == model && execution.target_protocol == "openai_chat",
                "unexpected smoke route"
            );
            let response = execution.response;
            let status = response.status();
            let body = response.text().await?;
            let free_tier_policy = body.to_ascii_lowercase().contains("free tier");
            let error = serde_json::from_str::<Value>(&body).ok();
            let reason = error
                .as_ref()
                .and_then(|value| {
                    value
                        .pointer("/error/message")
                        .or_else(|| value.get("message"))
                })
                .and_then(Value::as_str)
                .map(crate::upstream_failure::safe_excerpt)
                .unwrap_or_default();
            println!(
                "Dev OpenCode: path={path}, model={model}, HTTP={status}, free_tier_policy={free_tier_policy}"
            );
            anyhow::ensure!(
                status.is_success(),
                "Dev OpenCode rejected the adapted free-model request (HTTP {status}, free_tier_policy={free_tier_policy}): {reason}"
            );
            anyhow::ensure!(
                body.contains("[DONE]") && body.contains("OK"),
                "OpenCode stream did not complete with the smoke reply"
            );
            for line in body.lines().filter_map(|line| line.strip_prefix("data: ")) {
                if line == "[DONE]" {
                    continue;
                }
                let event: Value = serde_json::from_str(line)?;
                for choice in event["choices"].as_array().into_iter().flatten() {
                    anyhow::ensure!(
                        choice
                            .pointer("/delta/tool_calls")
                            .and_then(Value::as_array)
                            .is_none_or(Vec::is_empty),
                        "compatibility tool calls must be disabled"
                    );
                }
            }
            println!("Dev OpenCode: completed OK, compatibility tool calls=0");
        }
        Ok(())
    }
}
