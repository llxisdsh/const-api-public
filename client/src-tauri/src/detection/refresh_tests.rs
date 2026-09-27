use super::*;
use serde_json::json;
use std::sync::{Arc, Mutex};
use warp::Filter;

fn channel_for(driver: crate::source_driver::SourceDriverId, base: &str) -> ChannelConfig {
    let mut channel = channel_from_supplier("refresh-check".into(), &default_supplier_config());
    channel.set_source_driver(driver);
    channel.surface_bindings = crate::source_driver_surface_bindings(driver, base);
    channel.models = vec!["a-first-model".into(), "selected-model".into()];
    channel.upstream_model = "selected-model".into();
    channel.public_model = "selected-model".into();
    channel
}

#[tokio::test]
async fn optional_catalog_failure_keeps_old_models_and_evidence_without_generation() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let routes = warp::method().and(warp::path::full()).map(
        move |method: warp::http::Method, path: warp::path::FullPath| {
            captured.lock().unwrap().push(method.to_string());
            let failure = path.as_str().starts_with("/missing/");
            warp::reply::with_status(
                warp::reply::json(&json!({"data":[{"id":"new-model"}]})),
                if failure {
                    warp::http::StatusCode::NOT_FOUND
                } else {
                    warp::http::StatusCode::OK
                },
            )
        },
    );
    let (addr, server) = crate::bind_ephemeral!(routes, ([127, 0, 0, 1], 0));
    let task = tokio::spawn(server);
    let mut channel = channel_for(
        crate::source_driver::SourceDriverId::CustomEndpoint,
        &format!("http://{addr}"),
    );
    let mut secondary = channel.surface_bindings[0].clone();
    secondary.surface = crate::surface::ApiSurface::Anthropic;
    secondary.base_url = format!("http://{addr}/missing");
    secondary.protocols[0].protocol = "anthropic_messages".into();
    channel.surface_bindings.push(secondary);
    let evidence = ChannelCapabilityProfile {
        protocol: "anthropic_messages".into(),
        model_pattern: "selected-model".into(),
        catalog_metadata: true,
        context_tokens: Some(64000),
        ..Default::default()
    };
    channel.capability_profiles.push(evidence.clone());
    let result = refresh_channel(channel).await.unwrap();
    task.abort();
    assert!(result.models.iter().any(|m| m == "new-model"));
    assert!(result.models.iter().any(|m| m == "selected-model"));
    assert!(result.model_capability_evidence.contains(&evidence));
    assert_eq!(result.warnings.len(), 1);
    assert!(result.warnings[0].contains("previous models retained"));
    assert!(
        requests
            .lock()
            .unwrap()
            .iter()
            .all(|method| method == "GET")
    );
}

#[test]
fn check_projections_preserve_the_same_default_used_by_the_text_test() {
    for driver in crate::source_driver::ALL_SOURCE_DRIVER_IDS {
        let channel = channel_for(driver, "http://127.0.0.1:9");
        let expected = pick_primary_model(&channel.models, &channel);
        let contexts = surface_probe_contexts(channel, SurfaceProbeMode::Creation).unwrap();
        for context in contexts {
            let projection = probe_channel_from_context(&context).unwrap();
            assert_eq!(
                pick_primary_model(&projection.models, &projection),
                expected,
                "{driver:?}"
            );
        }
    }
}

#[tokio::test]
async fn information_refresh_for_every_http_driver_never_requests_generation() {
    use crate::source_driver::{ALL_SOURCE_DRIVER_IDS, ExecutionKind, SourceDriverId};
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let routes = warp::method().and(warp::path::full()).map(move |method: warp::http::Method, path: warp::path::FullPath| {
        captured.lock().unwrap().push((method.to_string(), path.as_str().to_string()));
        warp::reply::json(&json!({
            "data":[{"id":"selected-model","context_length":64000,"supported_parameters":["tools"]}],
            "models":[{"name":"models/selected-model","inputTokenLimit":64000}]
        }))
    });
    let (addr, server) = crate::bind_ephemeral!(routes, ([127, 0, 0, 1], 0));
    let task = tokio::spawn(server);
    for driver in ALL_SOURCE_DRIVER_IDS {
        if crate::source_driver::source_driver(driver)
            .unwrap()
            .execution_kind()
            != ExecutionKind::HttpSurface
        {
            continue;
        }
        for verified in [false, true] {
            requests.lock().unwrap().clear();
            let mut channel = channel_for(driver, &format!("http://{addr}"));
            if verified {
                for surface in &mut channel.surface_bindings {
                    surface.verification = SurfaceVerification {
                        state: "verified".into(),
                        checked_at_unix: 42,
                        summary: "original check".into(),
                    };
                    for protocol in &mut surface.protocols {
                        protocol.verification = ProtocolVerification {
                            state: "verified".into(),
                            checked_at_unix: 43,
                            summary: "original execution check".into(),
                        };
                    }
                }
            }
            let protocol = channel.v2.default_target.protocol.as_str().to_string();
            let live = ChannelCapabilityProfile {
                protocol: protocol.clone(),
                model_pattern: "selected-model".into(),
                custom_tool: true,
                verification_state: "verified".into(),
                verified_at_unix: 41,
                ..Default::default()
            };
            channel.capability_profiles = vec![
                live.clone(),
                ChannelCapabilityProfile {
                    protocol: protocol.clone(),
                    model_pattern: "selected-model".into(),
                    catalog_metadata: true,
                    context_tokens: Some(128000),
                    verification_state: "declared".into(),
                    ..Default::default()
                },
            ];
            channel.detection_checks = vec![ChannelDetectionCheck {
                name: "responses_operation".into(),
                status: "unsupported".into(),
                protocol: protocol.clone(),
                capability: "openai.responses_compact".into(),
                checked_at_unix: 40,
                message: "checked earlier".into(),
            }];
            let result = refresh_channel(channel.clone())
                .await
                .unwrap_or_else(|err| panic!("{driver:?}: {err:#}"));
            assert_eq!(result.models, ["selected-model"], "{driver:?}");
            assert!(
                result
                    .model_capability_evidence
                    .iter()
                    .any(|e| e.model_pattern == live.model_pattern
                        && e.custom_tool
                        && e.verified_at_unix == 41),
                "{driver:?}"
            );
            assert_eq!(result.detection_evidence.len(), 1, "{driver:?}");
            assert!(
                result
                    .model_capability_evidence
                    .iter()
                    .all(|e| !e.catalog_metadata || e.context_tokens != Some(128000)),
                "{driver:?}"
            );
            for surface in &result.surface_results {
                let saved = channel
                    .surface_bindings
                    .iter()
                    .find(|s| s.surface == surface.surface)
                    .unwrap();
                assert_eq!(
                    surface.surface_verification.checked_at_unix,
                    saved.verification.checked_at_unix
                );
                let saved_protocol = saved
                    .protocols
                    .iter()
                    .find(|p| p.protocol == surface.protocol.as_str())
                    .unwrap();
                assert_eq!(
                    surface.protocol_verification.state,
                    saved_protocol.verification.state
                );
                assert_eq!(
                    surface.protocol_verification.checked_at_unix,
                    saved_protocol.verification.checked_at_unix
                );
            }
            let requests = requests.lock().unwrap();
            assert!(!requests.is_empty(), "{driver:?}");
            assert!(
                requests.iter().all(|(method, path)| method == "GET"
                    && (path.ends_with("/models")
                        || path.ends_with("/models/user")
                        || path == "/api/tags")),
                "{driver:?}: {requests:?}"
            );
            if driver == SourceDriverId::Openrouter {
                assert_eq!(
                    requests.len(),
                    1,
                    "OpenRouter surfaces must share one account catalog fetch"
                );
                assert!(requests[0].1.ends_with("/models/user"));
            }
        }
    }
    task.abort();
}

#[tokio::test]
async fn information_refresh_errors_do_not_fall_back_to_generation_or_clear_saved_data() {
    for status in [401, 429, 503] {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let routes = warp::method().map(move |method: warp::http::Method| {
            captured.lock().unwrap().push(method.to_string());
            warp::reply::with_status(
                warp::reply::json(&json!({"error":{"message":"unavailable"}})),
                warp::http::StatusCode::from_u16(status).unwrap(),
            )
        });
        let (addr, server) = crate::bind_ephemeral!(routes, ([127, 0, 0, 1], 0));
        let task = tokio::spawn(server);
        let channel = channel_for(
            crate::source_driver::SourceDriverId::Openrouter,
            &format!("http://{addr}"),
        );
        assert!(refresh_channel(channel.clone()).await.is_err());
        assert_eq!(channel.models.len(), 2);
        assert_eq!(requests.lock().unwrap().as_slice(), ["GET"]);
        task.abort();
    }
}

#[tokio::test]
async fn information_refresh_accepts_an_authoritative_empty_openrouter_account_catalog() {
    let routes = warp::get()
        .and(warp::path!("v1" / "models" / "user"))
        .map(|| warp::reply::json(&json!({"data":[]})));
    let (addr, server) = crate::bind_ephemeral!(routes, ([127, 0, 0, 1], 0));
    let task = tokio::spawn(server);
    let channel = channel_for(
        crate::source_driver::SourceDriverId::Openrouter,
        &format!("http://{addr}"),
    );
    let result = refresh_channel(channel).await.unwrap();
    assert!(result.models.is_empty());
    task.abort();
}
