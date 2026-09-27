//! Responses extensions are independent of the basic generation protocol.
use super::*;
use crate::channel_surface::ChannelSurfaceTarget;
use crate::protocol::kind::ProtocolKind;
use futures_util::StreamExt;
use serde_json::{Value, json};

const CHECK: &str = "responses_operation";
const PROTOCOL: &str = "openai_responses";

pub(crate) fn operation_support(checks: &[ChannelDetectionCheck], operation: &str) -> Option<bool> {
    checks
        .iter()
        .filter(|check| check.name == CHECK && check.capability == operation)
        .max_by_key(|check| check.checked_at_unix)
        .and_then(|check| match check.status.as_str() {
            "verified" => Some(true),
            "unsupported" => Some(false),
            _ => None,
        })
}

pub(crate) fn channel_allows_path(channel: &ChannelConfig, path: &str) -> bool {
    let compact = crate::surface::resolve_api_route("POST", path, false)
        .is_some_and(|route| route.operation == crate::surface::ApiOperation::ResponsesCompact);
    !compact
        || operation_support(&channel.detection_checks, "openai.responses_compact") != Some(false)
}

fn evidence(capability: &str, status: &str, message: String) -> ChannelDetectionCheck {
    ChannelDetectionCheck {
        name: CHECK.into(),
        status: status.into(),
        checked_at_unix: now_unix(),
        protocol: PROTOCOL.into(),
        capability: capability.into(),
        message,
    }
}

fn failure_detail(channel: &ChannelConfig, error: &anyhow::Error) -> String {
    let mut detail = format!("{error:#}");
    for secret in [&channel.upstream_api_key, &channel.v2.credential_ref] {
        if !secret.is_empty() {
            detail = detail.replace(secret, "[redacted]");
        }
    }
    crate::upstream_failure::safe_excerpt(&detail)
}

pub(super) async fn enrich(
    client: &Client,
    channel: &ChannelConfig,
    observation: &mut ProbeObservation,
) {
    let Some(target) =
        crate::channel_surface::channel_protocol_target(channel, ProtocolKind::OpenAiResponses)
    else {
        return;
    };
    let models = observation
        .models
        .iter()
        .filter(|model| {
            crate::coding_gateway::catalog_protocol_support(
                &observation.capability_profiles,
                model,
                ProtocolKind::OpenAiResponses,
            )
            .unwrap_or_else(|| {
                crate::coding_gateway::model_supports_protocol(
                    channel.source_driver(),
                    model,
                    ProtocolKind::OpenAiResponses,
                )
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    if crate::coding_gateway::is_gateway(channel.source_driver()) && models.is_empty() {
        return;
    }
    let model = pick_primary_model(&models, channel);
    let result = check_deadline::step(
        Duration::from_secs(45),
        custom_tool_roundtrip(client, channel, &target, &model),
    )
    .await;
    let mut custom = ChannelCapabilityProfile {
        protocol: PROTOCOL.into(),
        model_pattern: model.clone(),
        verification_state: "verified".into(),
        verified_at_unix: now_unix(),
        ..Default::default()
    };
    match result {
        Ok(()) => {
            custom.custom_tool = true;
            observation.detection_checks.push(evidence(
                "custom_tool",
                "verified",
                format!("{model}: streaming grammar custom tool call and stateless output replay verified"),
            ));
        }
        Err(error) => {
            capability_check::retain_incomplete(
                channel,
                &model,
                &mut custom,
                capability_check::Feature::CustomTool,
                &error,
            );
            let detail = failure_detail(channel, &error);
            observation.detection_checks.push(evidence(
                "custom_tool",
                "unknown",
                format!("{model}: {detail}; basic Responses remains available"),
            ));
        }
    }
    if custom.custom_tool {
        // Tool/schema and custom-tool checks can describe the same exact model.
        // Keep one profile so merging the latter cannot erase the former.
        if let Some(profile) = observation.capability_profiles.iter_mut().find(|profile| {
            profile.protocol == PROTOCOL
                && profile.model_pattern == model
                && !profile.catalog_metadata
        }) {
            profile.custom_tool = true;
        } else {
            observation.capability_profiles.push(custom);
        }
    }
    // Invalid model/input prevent generation. Validation errors are not proof of
    // support; only an unambiguous missing endpoint disables a native operation.
    for (operation, path) in [
        ("openai.responses_compact", "/v1/responses/compact"),
        (
            "openai.responses_input_tokens",
            "/v1/responses/input_tokens",
        ),
    ] {
        let result = check_deadline::step(Duration::from_secs(6), async {
            let request = post(
                client,
                channel,
                &target,
                path,
                &json!({"model":null,"input":null}),
            )?;
            read_response(request.send_adaptive().await?).await
        })
        .await;
        let (status, message) = match result {
            Ok((code, body)) => (
                endpoint_status(code, &body),
                format!("invalid-input endpoint probe HTTP {code}"),
            ),
            Err(error) => (
                "unknown",
                format!(
                    "endpoint probe incomplete: {}",
                    failure_detail(channel, &error)
                ),
            ),
        };
        observation
            .detection_checks
            .push(evidence(operation, status, message));
    }
    let request =
        crate::proxy::responses_websocket_upstream_request(channel, &target, &HeaderMap::new());
    let (state, detail) = match request {
        Ok(request) => match check_deadline::step(Duration::from_secs(6), async {
            tokio_tungstenite::connect_async(request)
                .await
                .map_err(anyhow::Error::from)
        })
        .await
        {
            Ok((mut socket, _)) => {
                // A handshake proves transport, not custom tools or session replay.
                // Never send response.create; close/drop the probe connection.
                let _ = tokio::time::timeout(Duration::from_secs(1), socket.close(None)).await;
                (
                    "verified",
                    "WebSocket handshake HTTP 101; probe connection closed".to_string(),
                )
            }
            Err(error) => {
                if let Some(tokio_tungstenite::tungstenite::Error::Http(response)) =
                    error.downcast_ref()
                {
                    (
                        websocket_probe_status(response.status().as_u16()),
                        format!("WebSocket handshake HTTP {}", response.status().as_u16()),
                    )
                } else {
                    (
                        "unknown",
                        format!(
                            "WebSocket handshake failed: {}",
                            failure_detail(channel, &error)
                        ),
                    )
                }
            }
        },
        Err(_) => ("unknown", "WebSocket probe request unavailable".into()),
    };
    observation.detection_checks.push(evidence(
        "openai.responses_websocket",
        state,
        format!("{detail}; no generation requested"),
    ));
}

fn endpoint_status(code: u16, body: &[u8]) -> &'static str {
    if matches!(code, 401 | 403 | 429) {
        return "credential_limited";
    }
    let text = String::from_utf8_lossy(body).trim().to_ascii_lowercase();
    if matches!(code, 405 | 501) {
        return "unsupported";
    }
    if code == 404 {
        // A model/account-specific 404 must not disable an entire endpoint.
        let generic_json = serde_json::from_slice::<Value>(body)
            .ok()
            .is_some_and(|value| {
                let error = value.get("error").unwrap_or(&value);
                matches!(
                    error
                        .get("message")
                        .and_then(Value::as_str)
                        .map(|m| m.to_ascii_lowercase())
                        .as_deref(),
                    Some("not found" | "404 not found" | "404: not found" | "not found.")
                ) && error.get("param").is_none_or(Value::is_null)
                    && error
                        .get("code")
                        .is_none_or(|code| code == 404 || code == "404" || code == "not_found")
            });
        if text.is_empty()
            || matches!(
                text.as_str(),
                "not found" | "404 not found" | "404 page not found"
            )
            || generic_json
        {
            return "unsupported";
        }
    }
    "unknown"
}

fn websocket_probe_status(code: u16) -> &'static str {
    // The production Responses handshake contains no model or response ID.
    // A 404 here cannot be a missing requested model: this native WS route is
    // unavailable. Only connect_async success (not a bare 101) proves support.
    match code {
        404 | 405 | 501 => "unsupported",
        401 | 403 | 429 => "credential_limited",
        _ => "unknown",
    }
}

fn post(
    client: &Client,
    channel: &ChannelConfig,
    target: &ChannelSurfaceTarget,
    path: &str,
    body: &Value,
) -> Result<reqwest::RequestBuilder> {
    let route = crate::surface::resolve_api_route("POST", path, false)
        .ok_or_else(|| anyhow!("probe route unavailable"))?;
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let url = crate::channel_executor::resolve_target_url(
        channel,
        target,
        &route,
        &reqwest::Method::POST,
        path,
        model,
    )?;
    let mut headers = HeaderMap::new();
    crate::channel_user_agent::apply_to_headers(channel, &mut headers);
    crate::channel_executor::apply_upstream_auth(&mut headers, channel, target, true)?;
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    let text = serde_json::to_string(body)?;
    Ok(client
        .post(url)
        .headers(headers)
        .body(crate::openrouter::request_body(Some(channel), &text).into_owned()))
}

async fn read_response(response: reqwest::Response) -> Result<(u16, Vec<u8>)> {
    let code = response.status().as_u16();
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if body.len() + chunk.len() > 256 * 1024 {
            return Err(anyhow!("probe response too large"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok((code, body))
}

async fn completed_response(request: reqwest::RequestBuilder) -> Result<Value> {
    let (status, body) = read_response(request.send_adaptive().await?).await?;
    if !(200..300).contains(&status) {
        // Do not persist raw upstream bodies, which may echo credentials/prompts.
        return Err(anyhow!("custom tool probe HTTP {status}"));
    }
    let mut completed = None;
    for (event, data) in crate::protocol::stream::decode_sse_frames(&body)? {
        if data.trim() == "[DONE]" {
            continue;
        }
        let value: Value = serde_json::from_str(&data)?;
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .or(event.as_deref());
        if matches!(
            kind,
            Some("error" | "response.failed" | "response.incomplete")
        ) {
            return Err(anyhow!("custom tool probe stream failed or was incomplete"));
        }
        if kind == Some("response.completed") {
            completed = value.get("response").cloned();
        }
    }
    let value =
        completed.ok_or_else(|| anyhow!("custom tool probe returned no completed SSE response"))?;
    if value.get("status").and_then(Value::as_str) != Some("completed") {
        return Err(anyhow!("custom tool probe did not complete"));
    }
    Ok(value)
}

async fn custom_tool_roundtrip(
    client: &Client,
    channel: &ChannelConfig,
    target: &ChannelSurfaceTarget,
    model: &str,
) -> Result<()> {
    let mut body = json!({
        "model": model, "store": false, "stream": true, "include": ["reasoning.encrypted_content"],
        "input": [{"role":"user","content":"Call const_probe with the exact input ok. After its result, reply ok."}],
        "tools": [{"type":"custom","name":"const_probe","description":"A capability check. Input must be ok.","format":{"type":"grammar","syntax":"lark","definition":"start: \"ok\""}}],
        "tool_choice": {"type":"custom","name":"const_probe"}, "max_output_tokens": 256
    });
    let first = completed_response(post(client, channel, target, "/v1/responses", &body)?).await?;
    let output = first
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("custom tool probe missing output"))?;
    let calls: Vec<_> = output
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("custom_tool_call"))
        .collect();
    if calls.len() != 1
        || calls[0].get("name").and_then(Value::as_str) != Some("const_probe")
        || calls[0].get("input").and_then(Value::as_str).map(str::trim) != Some("ok")
    {
        return Err(anyhow!(
            "custom tool probe returned no valid const_probe call"
        ));
    }
    let call_id = calls[0]
        .get("call_id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| anyhow!("custom tool probe missing call_id"))?;
    let input = body["input"]
        .as_array_mut()
        .ok_or_else(|| anyhow!("probe input unavailable"))?;
    input.extend(output.iter().cloned()); // Includes opaque reasoning/encrypted items.
    input.push(json!({"type":"custom_tool_call_output","call_id":call_id,"output":"ok"}));
    body["tool_choice"] = json!("none");
    let second = completed_response(post(client, channel, target, "/v1/responses", &body)?).await?;
    let finished = second
        .get("output")
        .and_then(Value::as_array)
        .is_some_and(|output| {
            output.iter().any(|item| {
                item["type"] == "message"
                    && item["role"] == "assistant"
                    && item["content"].as_array().is_some_and(|content| {
                        content.iter().any(|part| {
                            part["type"] == "output_text"
                                && part["text"]
                                    .as_str()
                                    .is_some_and(|text| !text.trim().is_empty())
                        })
                    })
            })
        });
    if !finished {
        return Err(anyhow!(
            "custom tool result replay returned no assistant message"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_error_details_retain_the_cause_without_credentials() {
        let mut channel = channel("http://127.0.0.1:1");
        channel.upstream_api_key = "private-api-key".into();
        channel.v2.credential_ref = "private-credential-reference".into();
        let error =
            anyhow!("connection timed out for private-api-key private-credential-reference")
                .context("endpoint request failed");
        let detail = failure_detail(&channel, &error);
        assert!(detail.contains("endpoint request failed: connection timed out"));
        assert!(!detail.contains("private-api-key"));
        assert!(!detail.contains("private-credential-reference"));
    }

    fn channel(base: &str) -> ChannelConfig {
        let mut channel = crate::channel_from_supplier("probe".into(), &default_supplier_config());
        channel.set_source_driver(crate::source_driver::SourceDriverId::Openrouter);
        channel.v2.credential_ref = "test-secret".into();
        channel.surface_bindings =
            crate::source_driver_surface_bindings(channel.source_driver(), base);
        channel.v2.default_target.protocol = ProtocolKind::OpenAiResponses;
        channel.models = vec!["gpt-5.6-luna".into()];
        project_channel_legacy_fields(channel)
    }

    fn observation() -> ProbeObservation {
        ProbeObservation {
            models: vec!["gpt-5.6-luna".into()],
            native_protocol_verified: true,
            quota_status: "unknown".into(),
            remaining_ratio: 0.0,
            quota_windows: vec![],
            checks: vec![],
            warnings: vec![],
            capability_profiles: vec![],
            detection_checks: vec![],
        }
    }

    #[tokio::test]
    async fn command_code_responses_probe_uses_catalog_instead_of_the_default_model() {
        use warp::Filter;
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let received = seen.clone();
        let route = warp::post()
            .and(warp::path::full())
            .and(warp::body::json())
            .map(move |path: warp::path::FullPath, body: Value| {
                if path.as_str() != "/v1/responses" {
                    return warp::reply::with_status(String::new(), warp::http::StatusCode::NOT_FOUND);
                }
                received.lock().unwrap().push(body["model"].as_str().unwrap().into());
                let output = if body["tool_choice"] == "none" {
                    json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]}])
                } else {
                    json!([{"type":"custom_tool_call","call_id":"call_probe","name":"const_probe","input":"ok"}])
                };
                warp::reply::with_status(
                    format!("data: {}\n\n", json!({"type":"response.completed","response":{"status":"completed","output":output}})),
                    warp::http::StatusCode::OK,
                )
            });
        let (addr, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
        let server = tokio::spawn(server);
        let mut channel = crate::coding_gateway::tests::channel(
            crate::source_driver::SourceDriverId::CommandCode,
            &format!("http://{addr}/v1"),
        );
        let catalog = [
            ("claude-fixture", "/messages"),
            ("chat-only", "/chat/completions"),
            ("responses-model", "/responses"),
        ]
        .map(|(model, endpoint)| crate::model_catalog::ModelObservation {
            id: model.into(),
            supported_endpoints: Some(vec![endpoint.into()]),
            ..Default::default()
        });
        let mut saved = observation();
        saved.models = catalog.iter().map(|model| model.id.clone()).collect();
        saved.capability_profiles = vec![basic_protocol_capability(PROTOCOL, "verified")];
        append_catalog_profiles(&mut saved.capability_profiles, &catalog);
        let client = Client::new();
        for default in ["claude-fixture", "chat-only"] {
            channel.upstream_model = default.into();
            let mut result = saved.clone();
            enrich(&client, &channel, &mut result).await;
            assert!(result.capability_profiles.iter().any(|profile| {
                profile.model_pattern == "responses-model" && profile.custom_tool
            }));
        }
        assert_eq!(*seen.lock().unwrap(), vec!["responses-model"; 4]);

        // A catalog with no Responses candidate must not retry the incompatible default.
        saved.models.truncate(2);
        enrich(&client, &channel, &mut saved).await;
        assert_eq!(seen.lock().unwrap().len(), 4);
        assert!(saved.detection_checks.is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn expired_check_preserves_custom_tool_and_completed_model_features_without_requests() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        use warp::Filter;
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let routes = warp::any().map(move || {
            seen.fetch_add(1, Ordering::SeqCst);
            warp::reply::json(&json!({}))
        });
        let (addr, server) = crate::bind_ephemeral!(routes, ([127, 0, 0, 1], 0));
        let server = tokio::spawn(server);
        let mut channel = channel(&format!("http://{addr}"));
        let mut result = observation();
        let mut saved = ChannelCapabilityProfile {
            protocol: PROTOCOL.into(),
            model_pattern: "gpt-5.6-luna".into(),
            tool_calls: true,
            json_schema: true,
            custom_tool: true,
            verification_state: "verified".into(),
            verified_at_unix: 123,
            ..Default::default()
        };
        channel.capability_profiles.push(saved.clone());
        saved.custom_tool = false;
        result.capability_profiles.push(saved);
        check_deadline::scope(
            Duration::ZERO,
            enrich(short_http_client(), &channel, &mut result),
        )
        .await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(result.capability_profiles.len(), 1);
        let profile = &result.capability_profiles[0];
        assert!(profile.tool_calls && profile.json_schema && profile.custom_tool);
        assert!(
            result
                .detection_checks
                .iter()
                .any(|check| check.capability == "custom_tool"
                    && check.message.contains("deadline reached"))
        );
        server.abort();
    }

    #[tokio::test]
    async fn custom_tool_requires_complete_roundtrip_and_preserves_opaque_output() {
        use warp::Filter;
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let received = seen.clone();
        let route = warp::post().and(warp::path::full()).and(warp::header::optional::<String>("authorization"))
            .and(warp::body::json()).map(move |path: warp::path::FullPath, auth: Option<String>, value: Value| {
                assert_eq!(path.as_str(), "/custom/responses");
                assert_eq!(auth.as_deref(), Some("Bearer test-secret"));
                assert_eq!(value["model"], "gpt-5.6-luna");
                assert_eq!(value["store"], false);
                assert_eq!(value["stream"], true);
                assert_eq!(value["tools"][0]["format"]["syntax"], "lark");
                let mut values = received.lock().unwrap();
                values.push(value);
                let response = if values.len() == 1 {
                    json!({"status":"completed","output":[
                        {"type":"reasoning","encrypted_content":"opaque-exact-state","id":"rs_probe"},
                        {"type":"custom_tool_call","call_id":"call_probe","name":"const_probe","input":"ok"}
                    ]})
                } else {
                    json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]}]})
                };
                warp::reply::with_header(format!("event: response.completed\r\ndata: {}\r\n\r\n", json!({"type":"response.completed","response":response})), "content-type", "text/event-stream")
            });
        let (addr, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
        let server = tokio::spawn(server);
        let mut channel = channel(&format!("http://{addr}"));
        channel.set_source_driver(crate::source_driver::SourceDriverId::CustomEndpoint);
        channel.v2.default_target.protocol = ProtocolKind::OpenAiResponses;
        channel.surface_bindings[0].operation_overrides = serde_json::from_value(json!([
            {"operation":"responses","method":"POST","url":format!("http://{addr}/custom/responses")}
        ])).unwrap();
        let target = crate::channel_surface::channel_protocol_target(
            &channel,
            ProtocolKind::OpenAiResponses,
        )
        .unwrap();
        custom_tool_roundtrip(&Client::new(), &channel, &target, "gpt-5.6-luna")
            .await
            .unwrap();
        let values = seen.lock().unwrap();
        assert_eq!(values.len(), 2);
        assert_eq!(
            values[1]["input"][1]["encrypted_content"],
            "opaque-exact-state"
        );
        assert_eq!(values[1]["input"][2]["call_id"], "call_probe");
        assert_eq!(values[1]["input"][3]["type"], "custom_tool_call_output");
        assert_eq!(values[1]["input"][3]["call_id"], "call_probe");
        assert_eq!(values[1]["tool_choice"], "none");
        assert!(values[1].get("previous_response_id").is_none());
        server.abort();
    }

    #[tokio::test]
    async fn basic_responses_success_never_becomes_custom_tool_success() {
        use warp::Filter;
        let route = warp::path::full().map(|path: warp::path::FullPath| {
            if path.as_str() == "/v1/responses" {
                warp::reply::with_status(
                    warp::reply::json(&json!({"status":"completed","output":[]})),
                    warp::http::StatusCode::OK,
                )
            } else {
                warp::reply::with_status(
                    warp::reply::json(&json!({"error":{"code":404,"message":"Not Found"}})),
                    warp::http::StatusCode::NOT_FOUND,
                )
            }
        });
        let (addr, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
        let server = tokio::spawn(server);
        let channel = channel(&format!("http://{addr}"));
        let mut result = observation();
        enrich(&Client::new(), &channel, &mut result).await;
        assert!(
            result
                .capability_profiles
                .iter()
                .all(|profile| !profile.custom_tool)
        );
        assert_eq!(
            operation_support(&result.detection_checks, "custom_tool"),
            None
        );
        assert_eq!(
            operation_support(&result.detection_checks, "openai.responses_compact"),
            Some(false)
        );
        assert_eq!(
            operation_support(&result.detection_checks, "openai.responses_input_tokens"),
            Some(false)
        );
        assert!(result.native_protocol_verified);
        server.abort();
    }

    #[tokio::test]
    async fn openrouter_snapshot_uses_account_catalog_without_inference_or_anthropic_aliases() {
        use warp::Filter;
        let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = requests.clone();
        let route = warp::get().and(warp::path::full()).and(warp::header::optional::<String>("anthropic-version"))
            .map(move |path: warp::path::FullPath, version: Option<String>| {
                assert_eq!(path.as_str(), "/v1/models/user");
                assert!(version.is_none());
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                warp::reply::json(&json!({"data":[{"id":"openai/gpt-5.6-luna"},{"id":"openai/gpt-5.6-luna:batch"},{"id":"qwen/qwen3.8-27b"}]}))
            });
        let (addr, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
        let server = tokio::spawn(server);
        let mut channel = channel(&format!("http://{addr}"));
        for binding in &mut channel.surface_bindings {
            binding.verification.state = "verified".into();
        }
        let result = snapshot_channel(channel).await.unwrap();
        assert_eq!(result.models, vec!["gpt-5.6-luna", "qwen3.8-27b"]);
        assert!(result.detection_evidence.is_empty());
        assert!(result.model_capability_evidence.is_empty());
        assert!(requests.load(std::sync::atomic::Ordering::SeqCst) > 0);
        server.abort();
    }

    #[tokio::test]
    async fn secondary_responses_is_probed_and_websocket_probe_is_closed_without_generation() {
        use warp::Filter;
        let (closed, wait_closed) = tokio::sync::oneshot::channel();
        let closed = std::sync::Arc::new(std::sync::Mutex::new(Some(closed)));
        let websocket =
            warp::path!("v1" / "responses")
                .and(warp::ws())
                .map(move |ws: warp::ws::Ws| {
                    let closed = closed.clone();
                    ws.on_upgrade(move |mut socket| async move {
                        let message = socket.next().await.unwrap().unwrap();
                        assert!(
                            message.is_close(),
                            "handshake probe must never send a generation frame"
                        );
                        if let Some(closed) = closed.lock().unwrap().take() {
                            let _ = closed.send(());
                        }
                    })
                });
        let http = warp::path::full().map(|path: warp::path::FullPath| {
            let (body, status) = if path.as_str() == "/v1/models/user" {
                (json!({"data":[{"id":"openai/gpt-5.6-luna"}]}), 200)
            } else if matches!(
                path.as_str(),
                "/v1/responses" | "/v1/chat/completions" | "/v1/messages"
            ) {
                (
                    json!({"id":"basic-only","status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"OK"}]}],"choices":[{"finish_reason":"stop","message":{"content":"OK"}}]}),
                    200,
                )
            } else {
                (json!({"error":{"code":404,"message":"Not Found"}}), 404)
            };
            warp::reply::with_status(
                warp::reply::json(&body),
                warp::http::StatusCode::from_u16(status).unwrap(),
            )
        });
        let (addr, server) = crate::bind_ephemeral!(websocket.or(http), ([127, 0, 0, 1], 0));
        let server = tokio::spawn(server);
        let mut channel = channel(&format!("http://{addr}"));
        channel.v2.default_target.protocol = ProtocolKind::OpenAiChat;
        for binding in &mut channel.surface_bindings {
            for protocol in &mut binding.protocols {
                if protocol.protocol == "openai_responses" {
                    protocol.verification.state = "rejected".into();
                }
            }
        }
        let result = detect_channel(channel).await.unwrap();
        assert!(
            result
                .detection_evidence
                .iter()
                .any(|check| check.name == CHECK && check.capability == "custom_tool")
        );
        assert_eq!(
            operation_support(&result.detection_evidence, "openai.responses_websocket"),
            Some(true)
        );
        tokio::time::timeout(Duration::from_secs(2), wait_closed)
            .await
            .unwrap()
            .unwrap();
        server.abort();
    }

    #[test]
    fn endpoint_absence_is_not_a_model_or_auth_failure() {
        for body in [
            b"Not Found".as_slice(),
            br#"{"error":{"code":404,"message":"Not Found"}}"#,
        ] {
            assert_eq!(endpoint_status(404, body), "unsupported");
        }
        for (code, body) in [
            (
                404,
                br#"{"error":{"code":"model_not_found","message":"Not Found"}}"#.as_slice(),
            ),
            (400, b"Invalid input"),
            (200, b"{}"),
            (500, b"Not Found"),
        ] {
            assert_eq!(endpoint_status(code, body), "unknown");
        }
        assert_eq!(endpoint_status(429, b""), "credential_limited");
        assert_eq!(websocket_probe_status(404), "unsupported");
        assert_eq!(websocket_probe_status(429), "credential_limited");
        assert_eq!(websocket_probe_status(101), "unknown");
    }

    #[tokio::test]
    #[ignore = "spends a small amount of the configured dev OpenRouter quota"]
    async fn live_dev_openrouter_responses_extension_probe() {
        let raw = std::fs::read(crate::home_dir().join(".const-api-dev/client.json")).unwrap();
        let config: ClientConfig = serde_json::from_slice(&raw).unwrap();
        let channel = config
            .channels
            .into_iter()
            .find(|c| {
                c.source_driver() == crate::source_driver::SourceDriverId::Openrouter && c.enabled
            })
            .unwrap();
        let mut result = observation();
        result.models = vec!["qwen/qwen3.8-27b".into()];
        enrich(short_http_client(), &channel, &mut result).await;
        for check in result.detection_checks {
            println!("{}: {} ({})", check.capability, check.status, check.message);
        }
    }

    #[test]
    fn newest_probe_replaces_old_rejection_without_assuming_support() {
        let mut checks = vec![evidence(
            "openai.responses_compact",
            "unsupported",
            String::new(),
        )];
        checks[0].checked_at_unix = 1;
        assert_eq!(
            operation_support(&checks, "openai.responses_compact"),
            Some(false)
        );
        checks.push(evidence(
            "openai.responses_compact",
            "unknown",
            String::new(),
        ));
        assert_eq!(operation_support(&checks, "openai.responses_compact"), None);
        assert_eq!(
            operation_support(&checks, "openai.responses_websocket"),
            None
        );
    }
}
