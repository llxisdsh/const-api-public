use super::*;
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use warp::{Filter, Reply};

#[tokio::test]
async fn coding_gateway_zen_gemini_feature_checks_are_model_scoped() {
    let route = warp::post()
        .and(warp::path!("zen" / "v1" / "models" / String))
        .and(warp::body::json())
        .map(|model: String, body: serde_json::Value| {
            if model.ends_with(":streamGenerateContent") {
                return warp::reply::with_header(
                    "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"ok\"}]},\"finishReason\":\"STOP\"}]}\n\n",
                    "content-type", "text/event-stream",
                ).into_response();
            }
            assert_eq!(model, "gemini-3.8-flash:generateContent");
            let part = if body.get("tools").is_some() {
                json!({"functionCall":{"name":"const_probe","args":{"ok":true}}})
            } else {
                json!({"text":"{\"ok\":true}"})
            };
            warp::reply::json(&json!({
                "candidates":[{"content":{"role":"model","parts":[part]},"finishReason":"STOP"}]
            })).into_response()
        });
    let (addr, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
    let server = tokio::spawn(server);
    let mut channel = crate::coding_gateway::tests::channel(
        crate::source_driver::SourceDriverId::OpencodeZen,
        &format!("http://{addr}/zen/v1"),
    );
    channel.api_format = "gemini_native".into();
    let mut profiles = vec![basic_protocol_capability("gemini_native", "verified")];
    let mut warnings = Vec::new();
    enrich_gemini_native_capability_profiles(
        &Client::new(),
        &channel,
        "gemini-3.8-flash",
        &mut profiles,
        &mut Vec::new(),
        &mut warnings,
    )
    .await;
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(!profiles[0].tool_calls && !profiles[0].json_schema);
    let verified = profiles
        .iter()
        .find(|profile| profile.model_pattern == "gemini-3.8-flash")
        .unwrap();
    assert!(verified.tool_calls && verified.tool_choice && verified.json_schema);
    server.abort();
}

#[tokio::test]
async fn regression_incomplete_features_preserve_prior_verified_capabilities() {
    let catalog = warp::path!("v1" / "models")
        .map(|| warp::reply::json(&json!({"data":[{"id":"claude-test"}]})));
    let messages = warp::path!("v1" / "messages")
        .and(warp::body::json())
        .and_then(|body: serde_json::Value| async move {
            if body["stream"] == true {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Ok::<_, std::convert::Infallible>(anthropic_reply(&body))
        });
    let (addr, server) = crate::bind_ephemeral!(catalog.or(messages), ([127, 0, 0, 1], 0));
    let server = tokio::spawn(server);
    let mut channel = anthropic_channel(&format!("http://{addr}"), "audit-prior-features");
    let mut saved = basic_protocol_capability("anthropic_messages", "verified");
    saved.tool_calls = true;
    saved.tool_choice = true;
    saved.cache_control = true;
    saved.verified_at_unix = 123;
    channel.capability_profiles = vec![saved];
    let result = check_deadline::scope(
        Duration::from_millis(500),
        detect_channel_protocols(channel.clone()),
    )
    .await
    .unwrap();
    server.abort();
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("deadline reached"))
    );
    crate::supplier::merge_channel_detection_result(&mut channel, &result);
    let after = channel
        .capability_profiles
        .iter()
        .find(|profile| {
            !profile.catalog_metadata
                && profile.protocol == "anthropic_messages"
                && profile.model_pattern.is_empty()
        })
        .unwrap();
    println!(
        "after incomplete recheck: verified={}, tools={}, choice={}, cache={}",
        after.verification_state, after.tool_calls, after.tool_choice, after.cache_control
    );
    assert!(
        after.tool_calls && after.tool_choice && after.cache_control,
        "deadline must not erase already verified capabilities when no negative evidence was obtained"
    );
}

#[tokio::test]
async fn regression_native_request_timeout_preserves_primary_protocol() {
    let routes = warp::method().and(warp::path::full()).and_then(
        |method: warp::http::Method, path: warp::path::FullPath| async move {
            if path.as_str().ends_with("/chat/completions") && method == warp::http::Method::POST {
                tokio::time::sleep(Duration::from_secs(32)).await;
            }
            let body = if path.as_str().ends_with("/models") {
                json!({"data":[{"id":"selected-model"}]})
            } else {
                json!({"type":"message","content":[{"type":"text","text":"OK"}],"stop_reason":"end_turn"})
            };
            Ok::<_, std::convert::Infallible>(warp::reply::json(&body))
        });
    let (addr, server) = crate::bind_ephemeral!(routes, ([127, 0, 0, 1], 0));
    let server = tokio::spawn(server);
    let mut channel =
        channel_from_supplier("audit-native-timeout".into(), &default_supplier_config());
    channel.set_source_driver(crate::source_driver::SourceDriverId::CustomEndpoint);
    let base = format!("http://{addr}");
    let mut chat = crate::source_driver_surface_bindings(channel.source_driver(), &base).remove(0);
    chat.protocols.retain(|p| p.protocol == "openai_chat");
    chat.protocols[0].verification = ProtocolVerification {
        state: "verified".into(),
        checked_at_unix: 123,
        summary: "previous successful request".into(),
    };
    let anthropic = crate::source_driver_surface_bindings(
        crate::source_driver::SourceDriverId::AnthropicApi,
        &base,
    )
    .remove(0);
    channel.surface_bindings = vec![anthropic, chat];
    channel.v2.default_target.surface = crate::surface::ApiSurface::OpenAi;
    channel.v2.default_target.protocol = crate::protocol::kind::ProtocolKind::OpenAiChat;
    channel.models = vec!["selected-model".into()];
    channel.public_model = "selected-model".into();
    channel.upstream_model = channel.public_model.clone();
    let result = detect_channel_protocols(channel).await.unwrap();
    server.abort();
    let chat = result
        .surface_results
        .iter()
        .find(|r| r.protocol.as_str() == "openai_chat")
        .unwrap();
    println!(
        "native timeout: protocol={}, checked_at={}, warnings={:?}",
        chat.protocol_verification.state,
        chat.protocol_verification.checked_at_unix,
        result.warnings
    );
    assert_eq!(
        chat.protocol_verification.state, "verified",
        "a transient reqwest timeout must not reject a previously verified primary protocol"
    );
}

fn anthropic_channel(base: &str, id: &str) -> ChannelConfig {
    let mut channel = channel_from_supplier(id.into(), &default_supplier_config());
    channel.set_source_driver(crate::source_driver::SourceDriverId::AnthropicApi);
    channel.surface_bindings = crate::source_driver_surface_bindings(channel.source_driver(), base);
    channel.models = vec!["claude-test".into()];
    channel.upstream_model = "claude-test".into();
    channel.public_model = channel.upstream_model.clone();
    channel
}

fn anthropic_reply(body: &serde_json::Value) -> warp::reply::Response {
    if body["stream"] == true {
        return warp::reply::with_header(
            "event: message_start\ndata: {\"type\":\"message_start\"}\n\n",
            "content-type",
            "text/event-stream",
        )
        .into_response();
    }
    if body.get("tools").is_some() {
        return warp::reply::json(&json!({
            "type":"message", "content":[{"type":"tool_use","id":"probe","name":"const_probe","input":{"ok":true}}],
            "stop_reason":"tool_use"
        })).into_response();
    }
    warp::reply::json(&json!({
        "type":"message", "content":[{"type":"text","text":"OK"}], "stop_reason":"end_turn",
        "usage":{"input_tokens":1,"output_tokens":1,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}
    })).into_response()
}

#[tokio::test]
async fn decisive_protocol_rejection_remains_rejected_while_another_protocol_succeeds() {
    let routes = warp::method().and(warp::path::full()).map(
        |method: warp::http::Method, path: warp::path::FullPath| {
            let rejected = method == warp::http::Method::POST && path.as_str().ends_with("/chat/completions");
            let body = if path.as_str().ends_with("/models") {
                json!({"data":[{"id":"selected-model"}]})
            } else if rejected {
                json!({"error":{"message":"chat completions protocol is not supported"}})
            } else {
                json!({"type":"message","content":[{"type":"text","text":"OK"}],"stop_reason":"end_turn"})
            };
            warp::reply::with_status(warp::reply::json(&body), if rejected {
                warp::http::StatusCode::BAD_REQUEST
            } else { warp::http::StatusCode::OK })
        });
    let (addr, server) = crate::bind_ephemeral!(routes, ([127, 0, 0, 1], 0));
    let server = tokio::spawn(server);
    let mut channel = anthropic_channel(&format!("http://{addr}"), "explicit-rejection-check");
    channel.set_source_driver(crate::source_driver::SourceDriverId::CustomEndpoint);
    let mut chat =
        crate::source_driver_surface_bindings(channel.source_driver(), &format!("http://{addr}"))
            .remove(0);
    chat.protocols.retain(|p| p.protocol == "openai_chat");
    chat.protocols[0].verification.state = "verified".into();
    channel.surface_bindings.push(chat);
    channel.v2.default_target.surface = crate::surface::ApiSurface::OpenAi;
    channel.v2.default_target.protocol = crate::protocol::kind::ProtocolKind::OpenAiChat;
    let result = detect_channel_protocols(channel).await.unwrap();
    server.abort();
    let chat = result
        .surface_results
        .iter()
        .find(|r| r.protocol.as_str() == "openai_chat")
        .unwrap();
    assert_eq!(chat.protocol_verification.state, "rejected");
    assert!(
        result
            .surface_results
            .iter()
            .any(|r| r.protocol.as_str() == "anthropic_messages"
                && r.protocol_verification.state == "verified")
    );
}

#[test]
fn composed_check_future_stays_bounded_on_windows_worker_stacks() {
    let channel = anthropic_channel("http://127.0.0.1:9", "check-future-size");
    let check = detect_channel(channel.clone());
    let validation = crate::supplier::validate_supplier_channel(
        channel,
        crate::supplier::SupplierCheckMode::FullInteractive,
    );
    for size in [
        std::mem::size_of_val(&check),
        std::mem::size_of_val(&validation),
    ] {
        assert!(size < 256 * 1024, "composed check future is {size} bytes");
    }
}

#[tokio::test]
async fn healthy_serial_capability_check_can_complete_after_ten_seconds() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let catalog = warp::path!("v1" / "models")
        .map(|| warp::reply::json(&json!({"data":[{"id":"claude-test"}]})));
    let messages = warp::path!("v1" / "messages")
        .and(warp::body::json())
        .and_then(move |body: serde_json::Value| {
            let seen = seen.clone();
            async move {
                seen.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_secs(3)).await;
                Ok::<_, std::convert::Infallible>(anthropic_reply(&body))
            }
        });
    let (addr, server) = crate::bind_ephemeral!(catalog.or(messages), ([127, 0, 0, 1], 0));
    let server = tokio::spawn(server);
    let start = Instant::now();
    let result = detect_channel(anthropic_channel(
        &format!("http://{addr}"),
        "slow-healthy-capabilities",
    ))
    .await
    .unwrap();
    server.abort();
    let evidence = result
        .model_capability_evidence
        .iter()
        .find(|p| p.protocol == "anthropic_messages")
        .unwrap();
    assert_eq!(evidence.verification_state, "verified");
    assert!(evidence.tool_calls && evidence.cache_control && evidence.stream_sse);
    assert!(start.elapsed() >= Duration::from_secs(12));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        5,
        "one group request and four capability requests, no timeout retries"
    );
}

#[tokio::test]
async fn zero_budget_validation_keeps_legacy_channel_health_available() {
    let generations = Arc::new(AtomicUsize::new(0));
    let captured = generations.clone();
    let routes = warp::method().map(move |method: warp::http::Method| {
        if method == warp::http::Method::POST {
            captured.fetch_add(1, Ordering::SeqCst);
        }
        warp::reply::json(&json!({"data":[{"id":"claude-test"}]}))
    });
    let (addr, server) = crate::bind_ephemeral!(routes, ([127, 0, 0, 1], 0));
    let server = tokio::spawn(server);
    for verified in [false, true] {
        let mut channel = anthropic_channel(
            &format!("http://{addr}"),
            &format!("validation-zero-budget-{verified}"),
        );
        channel.capability_profiles.clear();
        for binding in &mut channel.surface_bindings {
            for protocol in &mut binding.protocols {
                protocol.verification.state = if verified { "verified" } else { "unknown" }.into();
            }
        }
        let (result, attempts) = crate::upstream_transport::with_probe_budget(
            0,
            crate::supplier::validate_supplier_channel(
                channel,
                crate::supplier::SupplierCheckMode::FullInteractive,
            ),
        )
        .await;
        let (_, health) = result.unwrap();
        assert_eq!(attempts, 0);
        assert_eq!(health.model_count, usize::from(verified));
        assert_eq!(
            health.status,
            if verified { "available" } else { "unavailable" }
        );
    }
    assert_eq!(generations.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn deadline_keeps_completed_features_inside_the_current_surface() {
    let catalog = warp::path!("v1" / "models")
        .map(|| warp::reply::json(&json!({"data":[{"id":"claude-test"}]})));
    let messages = warp::path!("v1" / "messages")
        .and(warp::body::json())
        .and_then(|body: serde_json::Value| async move {
            if body.to_string().contains("cache_control") {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Ok::<_, std::convert::Infallible>(anthropic_reply(&body))
        });
    let (addr, server) = crate::bind_ephemeral!(catalog.or(messages), ([127, 0, 0, 1], 0));
    let server = tokio::spawn(server);
    let result = check_deadline::scope(
        Duration::from_millis(500),
        detect_channel_protocols(anthropic_channel(
            &format!("http://{addr}"),
            "partial-features",
        )),
    )
    .await
    .unwrap();
    server.abort();
    let evidence = result
        .model_capability_evidence
        .iter()
        .find(|p| p.protocol == "anthropic_messages")
        .unwrap();
    assert_eq!(evidence.verification_state, "verified");
    assert!(evidence.stream_sse && evidence.tool_calls);
    assert!(!evidence.cache_control);
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("deadline reached"))
    );
}

#[tokio::test]
async fn later_default_protocol_timeout_retains_earlier_success_and_saved_state() {
    let requests = Arc::new(AtomicUsize::new(0));
    let captured = requests.clone();
    let routes = warp::method().and(warp::path::full()).map(move |method: warp::http::Method, path: warp::path::FullPath| {
        captured.fetch_add(1, Ordering::SeqCst);
        (method, path)
    }).and_then(|(method, path): (warp::http::Method, warp::path::FullPath)| async move {
        if path.as_str().ends_with("/messages") && method == warp::http::Method::POST {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        let body = if path.as_str().ends_with("/models") {
            json!({"data":[{"id":"selected-model"}]})
        } else {
            json!({"choices":[{"message":{"content":"OK"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}})
        };
        Ok::<_, std::convert::Infallible>(warp::reply::json(&body))
    });
    let (addr, server) = crate::bind_ephemeral!(routes, ([127, 0, 0, 1], 0));
    let server = tokio::spawn(server);
    let mut channel = channel_from_supplier("partial-protocols".into(), &default_supplier_config());
    channel.set_source_driver(crate::source_driver::SourceDriverId::CustomEndpoint);
    let base = format!("http://{addr}");
    channel.surface_bindings =
        crate::source_driver_surface_bindings(channel.source_driver(), &base);
    channel.surface_bindings.truncate(1);
    channel.surface_bindings[0]
        .protocols
        .retain(|p| p.protocol == "openai_chat");
    let mut anthropic = crate::source_driver_surface_bindings(
        crate::source_driver::SourceDriverId::AnthropicApi,
        &base,
    )
    .remove(0);
    anthropic.protocols[0].verification = ProtocolVerification {
        state: "verified".into(),
        checked_at_unix: 123,
        summary: "previous execution".into(),
    };
    channel.surface_bindings.push(anthropic);
    channel.v2.default_target.surface = crate::surface::ApiSurface::Anthropic;
    channel.v2.default_target.protocol = crate::protocol::kind::ProtocolKind::AnthropicMessages;
    channel.models = vec!["selected-model".into()];
    channel.public_model = "selected-model".into();
    channel.upstream_model = channel.public_model.clone();
    let result = check_deadline::scope(
        Duration::from_millis(500),
        detect_channel_protocols(channel),
    )
    .await
    .unwrap();
    let after = requests.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        requests.load(Ordering::SeqCst),
        after,
        "no detached paid probes after return"
    );
    server.abort();
    assert_eq!(result.surface_results.len(), 2);
    let chat = result
        .surface_results
        .iter()
        .find(|r| r.protocol.as_str() == "openai_chat")
        .unwrap();
    assert_eq!(chat.protocol_verification.state, "verified");
    let anthropic = result
        .surface_results
        .iter()
        .find(|r| r.protocol.as_str() == "anthropic_messages")
        .unwrap();
    assert_eq!(anthropic.protocol_verification.checked_at_unix, 123);
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("deadline reached"))
    );
}
