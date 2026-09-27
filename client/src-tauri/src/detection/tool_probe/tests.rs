use super::*;
use std::sync::{Arc, Mutex};
use warp::Filter;

fn protocols() -> [ProtocolKind; 4] {
    [
        ProtocolKind::OpenAiChat,
        ProtocolKind::OpenAiResponses,
        ProtocolKind::AnthropicMessages,
        ProtocolKind::GeminiNative,
    ]
}

fn complete(protocol: ProtocolKind) -> Value {
    match protocol {
        ProtocolKind::OpenAiChat => {
            json!({"choices":[{"finish_reason":"tool_calls","message":{"tool_calls":[{"type":"function","function":{"name":"const_probe","arguments":"{\"ok\":true}"}}]}}]})
        }
        ProtocolKind::OpenAiResponses => {
            json!({"status":"completed","output":[{"type":"reasoning"},{"type":"function_call","name":"const_probe","arguments":"{\"ok\":true}"}]})
        }
        ProtocolKind::AnthropicMessages => {
            json!({"stop_reason":"tool_use","content":[{"type":"text","text":""},{"type":"tool_use","name":"const_probe","input":{"ok":true}}]})
        }
        ProtocolKind::GeminiNative => {
            json!({"candidates":[{"finishReason":"STOP","content":{"parts":[{"text":""},{"functionCall":{"name":"const_probe","args":{"ok":true}}}]}}]})
        }
    }
}

fn truncated(protocol: ProtocolKind) -> Value {
    let mut value = match protocol {
        ProtocolKind::OpenAiChat => {
            json!({"choices":[{"finish_reason":"length","message":{"content":null,"reasoning":"PRIVATE_REASONING"}}]})
        }
        ProtocolKind::OpenAiResponses => {
            json!({"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}})
        }
        ProtocolKind::AnthropicMessages => json!({"stop_reason":"max_tokens"}),
        ProtocolKind::GeminiNative => json!({"candidates":[{"finishReason":"MAX_TOKENS"}]}),
    };
    value["id"] = json!("probe-request-id");
    value
}

async fn mock_probe(
    protocol: ProtocolKind,
    responses: Vec<(u16, Value)>,
) -> (Result<()>, Vec<Value>) {
    mock_probe_with_tool_choice(protocol, responses, true).await
}

async fn mock_probe_with_tool_choice(
    protocol: ProtocolKind,
    responses: Vec<(u16, Value)>,
    named_tool_choice: bool,
) -> (Result<()>, Vec<Value>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let route = warp::post()
        .and(warp::body::json())
        .map(move |body: Value| {
            let mut requests = captured.lock().unwrap();
            let index = requests.len().min(responses.len() - 1);
            requests.push(body);
            let (status, value) = &responses[index];
            warp::reply::with_status(
                warp::reply::json(value),
                warp::http::StatusCode::from_u16(*status).unwrap(),
            )
        });
    let (addr, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
    let task = tokio::spawn(server);
    let mut channel = channel_from_supplier("tool-probe".into(), &default_supplier_config());
    channel.upstream_base_url = format!("http://{addr}");
    let result = run(
        short_http_client(),
        &channel,
        "configured-model",
        protocol,
        named_tool_choice,
    )
    .await;
    task.abort();
    let requests = requests.lock().unwrap().clone();
    (result, requests)
}

#[tokio::test]
async fn each_protocol_retries_only_truncation_once_with_a_bounded_budget() {
    for protocol in protocols() {
        let (result, requests) = mock_probe(
            protocol,
            vec![(200, truncated(protocol)), (200, complete(protocol))],
        )
        .await;
        result.unwrap();
        assert_eq!(requests.len(), 2, "{protocol:?}");
        let budgets = requests
            .iter()
            .map(|body| {
                body.get("max_tokens")
                    .or_else(|| body.get("max_output_tokens"))
                    .or_else(|| body.pointer("/generationConfig/maxOutputTokens"))
                    .and_then(Value::as_u64)
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(budgets, [256, 1024]);
        for body in requests {
            if protocol != ProtocolKind::GeminiNative {
                assert_eq!(body["model"], "configured-model");
            }
        }
    }
}

#[tokio::test]
async fn success_needs_only_one_request_for_each_protocol() {
    for protocol in protocols() {
        let (result, requests) = mock_probe(protocol, vec![(200, complete(protocol))]).await;
        result.unwrap();
        assert_eq!(requests.len(), 1);
    }
}

#[tokio::test]
async fn exhausted_budget_stays_unknown_and_does_not_echo_reasoning() {
    for protocol in protocols() {
        let (result, requests) = mock_probe(protocol, vec![(200, truncated(protocol))]).await;
        let error = result.unwrap_err().to_string();
        assert_eq!(requests.len(), 2);
        for expected in [
            "configured-model",
            "probe-request-id",
            "output token budget exhausted",
            "output_budget=1024",
            "attempts=2",
            "unconfirmed",
        ] {
            assert!(error.contains(expected), "{error}");
        }
        assert!(!error.contains("PRIVATE_REASONING"));
    }
}

#[tokio::test]
async fn http_errors_and_missing_calls_are_not_retried() {
    for status in [400, 401, 403, 429, 500, 503] {
        let (result, requests) = mock_probe(
            ProtocolKind::OpenAiChat,
            vec![(status, json!({"error":{"message":"not available"}}))],
        )
        .await;
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains(&format!("HTTP {status}"))
        );
        assert_eq!(requests.len(), 1);
    }
    let (result, requests) = mock_probe(
        ProtocolKind::OpenAiChat,
        vec![(
            200,
            json!({"choices":[{"finish_reason":"stop","message":{"content":"no tool"}}]}),
        )],
    )
    .await;
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("no complete const_probe")
    );
    assert_eq!(requests.len(), 1);
}

#[test]
fn incomplete_or_wrong_arguments_do_not_verify_tool_support() {
    for args in [
        None,
        Some(json!("{")),
        Some(json!({"ok":false})),
        Some(json!({"ok":true,"unexpected":true})),
    ] {
        assert!(!arguments_ok(args.as_ref()));
    }
    assert!(arguments_ok(Some(&json!("{\"ok\":true}"))));
    assert!(arguments_ok(Some(&json!({"ok":true}))));
}

#[test]
fn errors_redact_secrets_before_shortening_details() {
    let mut channel = channel_from_supplier("redaction".into(), &default_supplier_config());
    channel.upstream_api_key = "private-key".repeat(50);
    let detail = response_summary(
        &channel,
        "model",
        401,
        &json!({"error":{"message":format!("rejected {}",channel.upstream_api_key)}}),
    );
    assert!(detail.contains("[redacted]"));
    assert!(!detail.contains("private-key"));
}

#[tokio::test]
async fn model_without_named_tool_choice_can_still_prove_function_calls() {
    for protocol in [
        ProtocolKind::OpenAiChat,
        ProtocolKind::OpenAiResponses,
        ProtocolKind::AnthropicMessages,
    ] {
        let (result, requests) =
            mock_probe_with_tool_choice(protocol, vec![(200, complete(protocol))], false).await;
        result.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].get("tool_choice").is_none());
        assert!(requests[0].get("tools").is_some());
    }
}

#[tokio::test]
async fn failed_or_filtered_response_does_not_prove_function_calls() {
    for status in ["failed", "incomplete", "cancelled", "in_progress"] {
        let mut response = complete(ProtocolKind::OpenAiResponses);
        response["status"] = json!(status);
        let (result, requests) =
            mock_probe(ProtocolKind::OpenAiResponses, vec![(200, response)]).await;
        assert!(result.unwrap_err().to_string().contains("unconfirmed"));
        assert_eq!(requests.len(), 1);
    }
}
