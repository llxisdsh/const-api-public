//! Bounded semantic tool checks. Information refresh must never call this module.
use super::*;
use crate::protocol::kind::ProtocolKind;
use serde_json::{Value, json};

const OUTPUT_BUDGETS: [u32; 2] = [256, 1024];

pub(super) async fn run(
    client: &Client,
    channel: &ChannelConfig,
    model: &str,
    protocol: ProtocolKind,
    named_tool_choice: bool,
) -> Result<()> {
    for (attempt, budget) in OUTPUT_BUDGETS.into_iter().enumerate() {
        let mut body = request_body(protocol, model, budget);
        if !named_tool_choice {
            if let Some(body) = body.as_object_mut() {
                body.remove("tool_choice");
            }
        }
        let request = match protocol {
            ProtocolKind::OpenAiChat | ProtocolKind::OpenAiResponses => {
                let path = if protocol == ProtocolKind::OpenAiChat {
                    "/v1/chat/completions"
                } else {
                    "/v1/responses"
                };
                openai_request_with_base(
                    client.post(join_upstream_url(&channel.upstream_base_url, path)),
                    channel,
                    &channel.upstream_api_key,
                    &channel.upstream_base_url,
                )
                .json(&body)
            }
            ProtocolKind::AnthropicMessages => anthropic_messages_request(client, channel, &body),
            ProtocolKind::GeminiNative => gemini_native_request(
                client,
                channel,
                &format!("/v1beta/models/{model}:generateContent"),
                &body,
            )?,
        };
        let mut response = request.send_adaptive().await?;
        let status = response.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            anyhow::ensure!(
                bytes.len().saturating_add(chunk.len()) <= 64 * 1024,
                "tool probe response too large (model={model})"
            );
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes).with_context(|| {
            format!("tool probe returned non-JSON data (model={model}, HTTP {status})")
        })?;
        let detail = response_summary(channel, model, status.as_u16(), &value);
        if !status.is_success() || value.get("error").is_some_and(|error| !error.is_null()) {
            return Err(anyhow!("{detail}; tool capability remains unconfirmed"));
        }
        // Even a partial tool block can have the correct name. Require complete
        // arguments and a non-truncated response before claiming semantic proof.
        let truncated = output_truncated(protocol, &value);
        let terminal_success = protocol != ProtocolKind::OpenAiResponses
            || value
                .get("status")
                .and_then(Value::as_str)
                .is_none_or(|status| status == "completed");
        if !truncated && terminal_success && has_probe_call(protocol, &value) {
            return Ok(());
        }
        if truncated && attempt + 1 < OUTPUT_BUDGETS.len() {
            continue;
        }
        return Err(anyhow!(
            "{detail}; {}; output_budget={budget}, attempts={}; tool capability remains unconfirmed",
            if truncated {
                "output token budget exhausted"
            } else {
                "no complete const_probe(ok=true) call returned"
            },
            attempt + 1,
        ));
    }
    Err(anyhow!("tool capability remains unconfirmed"))
}

fn request_body(protocol: ProtocolKind, model: &str, budget: u32) -> Value {
    let prompt = "Call const_probe with ok=true.";
    let schema = json!({"type":"object", "properties":{"ok":{"type":"boolean"}}, "required":["ok"], "additionalProperties":false});
    match protocol {
        ProtocolKind::OpenAiChat => json!({
            "model":model, "messages":[{"role":"user","content":prompt}],
            "tools":[{"type":"function","function":{"name":"const_probe","parameters":schema}}],
            "tool_choice":{"type":"function","function":{"name":"const_probe"}}, "max_tokens":budget
        }),
        ProtocolKind::OpenAiResponses => json!({
            "model":model, "input":prompt,
            "tools":[{"type":"function","name":"const_probe","description":"Return probe arguments.","parameters":schema,"strict":true}],
            "tool_choice":{"type":"function","name":"const_probe"}, "max_output_tokens":budget
        }),
        ProtocolKind::AnthropicMessages => json!({
            "model":model, "messages":[{"role":"user","content":prompt}],
            "tools":[{"name":"const_probe","description":"Return probe arguments.","input_schema":schema}],
            "tool_choice":{"type":"tool","name":"const_probe"}, "max_tokens":budget
        }),
        ProtocolKind::GeminiNative => json!({
            "contents":[{"parts":[{"text":prompt}]}],
            "tools":[{"functionDeclarations":[{"name":"const_probe","parameters":{
                "type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"]
            }}]}],
            "toolConfig":{"functionCallingConfig":{"mode":"ANY","allowedFunctionNames":["const_probe"]}},
            "generationConfig":{"maxOutputTokens":budget}
        }),
    }
}

fn arguments_ok(value: Option<&Value>) -> bool {
    let parsed = value
        .and_then(Value::as_str)
        .and_then(|text| serde_json::from_str::<Value>(text).ok());
    parsed
        .as_ref()
        .or(value)
        .is_some_and(|args| args == &json!({"ok":true}))
}

fn has_probe_call(protocol: ProtocolKind, value: &Value) -> bool {
    let calls = match protocol {
        ProtocolKind::OpenAiChat => value.pointer("/choices/0/message/tool_calls"),
        ProtocolKind::OpenAiResponses => value.get("output"),
        ProtocolKind::AnthropicMessages => value.get("content"),
        ProtocolKind::GeminiNative => value.pointer("/candidates/0/content/parts"),
    };
    calls.and_then(Value::as_array).is_some_and(|calls| {
        calls.iter().any(|call| {
            let (function, arguments) = match protocol {
                ProtocolKind::OpenAiChat => (call.get("function"), "arguments"),
                ProtocolKind::OpenAiResponses
                    if call.get("type").and_then(Value::as_str) == Some("function_call") =>
                {
                    (Some(call), "arguments")
                }
                ProtocolKind::AnthropicMessages
                    if call.get("type").and_then(Value::as_str) == Some("tool_use") =>
                {
                    (Some(call), "input")
                }
                ProtocolKind::GeminiNative => (call.get("functionCall"), "args"),
                _ => (None, ""),
            };
            function.is_some_and(|function| {
                function.get("name").and_then(Value::as_str) == Some("const_probe")
                    && arguments_ok(function.get(arguments))
            })
        })
    })
}

fn output_truncated(protocol: ProtocolKind, value: &Value) -> bool {
    match protocol {
        ProtocolKind::OpenAiChat => {
            value
                .pointer("/choices/0/finish_reason")
                .and_then(Value::as_str)
                == Some("length")
        }
        ProtocolKind::OpenAiResponses => {
            value
                .pointer("/incomplete_details/reason")
                .and_then(Value::as_str)
                == Some("max_output_tokens")
        }
        ProtocolKind::AnthropicMessages => {
            value.get("stop_reason").and_then(Value::as_str) == Some("max_tokens")
        }
        ProtocolKind::GeminiNative => {
            value
                .pointer("/candidates/0/finishReason")
                .and_then(Value::as_str)
                == Some("MAX_TOKENS")
        }
    }
}

fn response_summary(channel: &ChannelConfig, model: &str, status: u16, value: &Value) -> String {
    let mut summary = format!("model={model}, HTTP {status}");
    for (label, pointers) in [
        ("request_id", &["/request_id", "/id"][..]),
        ("provider", &["/provider"][..]),
        ("status", &["/status"][..]),
        (
            "finish_reason",
            &[
                "/choices/0/finish_reason",
                "/stop_reason",
                "/candidates/0/finishReason",
                "/incomplete_details/reason",
            ][..],
        ),
        (
            "error",
            &["/error/message", "/error/type", "/error/code"][..],
        ),
    ] {
        if let Some(text) = pointers
            .iter()
            .find_map(|pointer| value.pointer(pointer).and_then(Value::as_str))
        {
            let mut text = text.to_string();
            for secret in [&channel.upstream_api_key, &channel.v2.credential_ref] {
                if !secret.is_empty() {
                    text = text.replace(secret, "[redacted]");
                }
            }
            summary.push_str(&format!(
                ", {label}={}",
                text.chars().take(256).collect::<String>()
            ));
        }
    }
    // Keep useful identifiers and failure reasons, never raw prompts/reasoning.
    for secret in [&channel.upstream_api_key, &channel.v2.credential_ref] {
        if !secret.is_empty() {
            summary = summary.replace(secret, "[redacted]");
        }
    }
    summary.replace(['\r', '\n'], " ")
}

#[cfg(test)]
mod tests;
