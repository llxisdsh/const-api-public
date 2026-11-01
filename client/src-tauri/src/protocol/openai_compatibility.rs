//! Request-dependent restrictions of the official OpenAI API, not gateway/model guesses.
//! https://developers.openai.com/api/docs/guides/latest-model#gpt-6-astra-update-api-and-model-parameters

use serde_json::Value;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ToolRequest {
    pub(crate) has_tools: bool,
    pub(crate) reasoning_disabled: bool,
}

impl ToolRequest {
    pub(crate) fn from_body(body: &[u8]) -> Self {
        let Ok(body) = serde_json::from_slice::<Value>(body) else {
            // The request decoder owns malformed JSON; it is not evidence for conversion.
            return Self::default();
        };
        Self {
            has_tools: nonempty_array(&body["tools"])
                || nonempty_array(&body["functions"])
                || ["messages", "input", "contents"].iter().any(|key| {
                    body[*key]
                        .as_array()
                        .is_some_and(|items| items.iter().any(tool_history))
                }),
            reasoning_disabled: body["reasoning_effort"] == "none"
                || body["reasoning"]["effort"] == "none",
        }
    }

    pub(crate) fn requires_responses(self, model: &str) -> bool {
        self.has_tools
            && (["gpt-6-astra", "gpt-6.1-sol", "gpt-6.1-terra"]
                .iter()
                .any(|base| model_or_snapshot(model, base))
                || (!self.reasoning_disabled
                    && ["gpt-6-sol", "gpt-6-luna"]
                        .iter()
                        .any(|base| model_or_snapshot(model, base))))
    }
}

fn nonempty_array(value: &Value) -> bool {
    value.as_array().is_some_and(|items| !items.is_empty())
}

fn tool_history(item: &Value) -> bool {
    matches!(item["role"].as_str(), Some("tool" | "function"))
        || nonempty_array(&item["tool_calls"])
        || item["function_call"].is_object()
        || matches!(
            item["type"].as_str(),
            Some(
                "function_call"
                    | "function_call_output"
                    | "custom_tool_call"
                    | "custom_tool_call_output"
                    | "tool_use"
                    | "tool_result"
            )
        )
        || item["functionCall"].is_object()
        || item["functionResponse"].is_object()
        || ["content", "parts"].iter().any(|key| {
            item[*key]
                .as_array()
                .is_some_and(|items| items.iter().any(tool_history))
        })
}

fn model_or_snapshot(model: &str, base: &str) -> bool {
    model == base
        || model.strip_prefix(base).is_some_and(|suffix| {
            let bytes = suffix.as_bytes();
            bytes.len() == 11
                && bytes[0] == b'-'
                && bytes[5] == b'-'
                && bytes[8] == b'-'
                && bytes
                    .iter()
                    .enumerate()
                    .all(|(i, b)| matches!(i, 0 | 5 | 8) || b.is_ascii_digit())
        })
}

/// Legacy Chat clients still emit max_tokens, even with thinking disabled. The
/// reasoning-model families require max_completion_tokens, with the same limit.
pub(crate) fn normalize_chat_token_limit(
    body: &[u8],
    model: &str,
) -> anyhow::Result<Option<Vec<u8>>> {
    let reasoning_model = ["gpt-5", "gpt-6", "o1", "o3", "o4"].iter().any(|base| {
        model == *base
            || model
                .strip_prefix(base)
                .is_some_and(|suffix| suffix.starts_with('-') || suffix.starts_with('.'))
    });
    if !reasoning_model
        || !body
            .windows(b"max_tokens".len())
            .any(|part| part == b"max_tokens")
    {
        return Ok(None);
    }
    let mut value: Value = serde_json::from_slice(body)?;
    let Some(object) = value.as_object_mut() else {
        return Ok(None);
    };
    let Some(limit) = object.remove("max_tokens") else {
        return Ok(None);
    };
    // Match the Chat decoder: the modern field wins if both were supplied.
    object.entry("max_completion_tokens").or_insert(limit);
    Ok(Some(serde_json::to_vec(&value)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_openai_tool_contract_matches_shared_cases() {
        let cases: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/protocol-v2/openai-tool-routing.json"
        ))
        .unwrap();
        for case in cases.as_array().unwrap() {
            let request = ToolRequest::from_body(&serde_json::to_vec(&case["body"]).unwrap());
            assert_eq!(
                request.has_tools,
                case["tools"].as_bool().unwrap(),
                "{}",
                case["name"]
            );
            assert_eq!(
                request.reasoning_disabled,
                case["reasoning_disabled"].as_bool().unwrap(),
                "{}",
                case["name"]
            );
            assert_eq!(
                request.requires_responses(case["model"].as_str().unwrap()),
                case["responses"].as_bool().unwrap(),
                "{}",
                case["name"]
            );
        }
    }

    #[test]
    fn official_openai_legacy_tool_conversion_never_silently_drops_functions() {
        for body in [
            serde_json::json!({"model":"gpt-6-astra","messages":[{"role":"user","content":"review"}],"functions":[{"name":"read_file","parameters":{"type":"object"}}]}),
            serde_json::json!({"model":"gpt-6-astra","messages":[{"role":"assistant","function_call":{"name":"read_file","arguments":"{}"}}]}),
        ] {
            assert!(
                crate::proxy::upstream_request_for_api_format(
                    "openai_responses",
                    "/v1/chat/completions",
                    &body.to_string(),
                    "gpt-6-astra"
                )
                .is_err()
            );
            let (_, native, _, _) = crate::proxy::upstream_request_for_api_format(
                "openai_chat",
                "/v1/chat/completions",
                &body.to_string(),
                "gpt-6-astra",
            )
            .unwrap();
            assert_eq!(serde_json::from_str::<Value>(&native).unwrap(), body);
        }
    }

    #[test]
    fn official_openai_chat_limit_preserves_budget_and_unknown_fields() {
        for model in ["gpt-6.1-sol", "gpt-6-astra", "gpt-6-luna", "gpt-5.5", "o3"] {
            let body = br#"{"max_tokens":8192,"reasoning_effort":"none","vendor_extra":{"keep":true},"messages":[]}"#;
            let normalized: Value =
                serde_json::from_slice(&normalize_chat_token_limit(body, model).unwrap().unwrap())
                    .unwrap();
            assert_eq!(normalized["max_completion_tokens"], 8192);
            assert!(normalized.get("max_tokens").is_none());
            assert_eq!(normalized["reasoning_effort"], "none");
            assert_eq!(normalized["vendor_extra"]["keep"], true);
        }
        let both = br#"{"max_tokens":100,"max_completion_tokens":200}"#;
        let normalized: Value = serde_json::from_slice(
            &normalize_chat_token_limit(both, "gpt-6.1-sol")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(normalized, serde_json::json!({"max_completion_tokens":200}));
        assert!(
            normalize_chat_token_limit(both, "gpt-4o")
                .unwrap()
                .is_none()
        );
        assert!(
            normalize_chat_token_limit(
                br#"{ "messages": [], "max_completion_tokens":100 }"#,
                "gpt-6.1-sol"
            )
            .unwrap()
            .is_none()
        );
    }
}
