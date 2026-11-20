#[tokio::test]
async fn openrouter_account_refresh_reaches_compact_media_discovery_with_skill_disabled() {
    use serde_json::json;
    let image = json!({"input_modalities":["text","image"],"output_modalities":["image"]});
    let catalog = json!({"data":[
        {"id":"qwen/qwen-image-3","architecture":image,"endpoints":["/api/v1/images"]},
        {"id":"minimax/minimax-h3-image","architecture":image,"endpoints":["/api/v1/images"]},
        {"id":"amazon/titan-embed-image-v1","architecture":{"input_modalities":["image"],"output_modalities":["embeddings"]}},
        {"id":"recraft/example:free","architecture":{"input_modalities":["text"],"output_modalities":["image"]},"endpoints":["/api/v1/images"]},
        {"id":"openrouter/auto","architecture":image,"endpoints":["/api/v1/chat/completions"]},
        {"id":"alibaba/wan-3.0","architecture":{"output_modalities":["video"]}},
        {"id":"heygen/heygen-voice-1-turbo:free","architecture":{"output_modalities":["speech"]}},
        {"id":"example/audio-chat","architecture":{"output_modalities":["text","audio"]}},
        {"id":"google/lyria-3-clip-preview","architecture":{"output_modalities":["text","audio"]},"endpoints":["/api/v1/chat/completions"]},
        {"id":"example/video-reader","architecture":{"input_modalities":["video"],"output_modalities":["text"]}}
    ]});
    let route = warp::get()
        .and(warp::path!("v1" / "models" / "user"))
        .map(move || warp::reply::json(&catalog))
        .or(warp::get().and(warp::path!("v1" / "images" / "models")).map(move || warp::reply::json(&json!({"data":[
            {"id":"qwen/qwen-image-3","architecture":image,"supported_parameters":{"input_references":{"min":0,"max":3}}},
            {"id":"minimax/minimax-h3-image","architecture":image,"supported_parameters":{"input_references":{"min":1,"max":2}}},
            {"id":"openrouter/auto","architecture":image,"supported_parameters":{"input_references":{"min":0,"max":2}}}
        ]}))));
    let (addr, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
    let server = tokio::spawn(server);
    let mut channel = crate::channel_from_supplier("router".into(), &crate::default_supplier_config());
    channel.set_source_driver(crate::source_driver::SourceDriverId::Openrouter);
    channel.upstream_base_url = format!("http://{addr}/v1");
    channel.upstream_api_key = "fixture".into();
    channel.enabled = true;
    channel.surface_bindings = crate::source_driver_surface_bindings(channel.source_driver(), &channel.upstream_base_url);
    for binding in &mut channel.surface_bindings {
        binding.verification.state = "verified".into();
    }
    let refreshed = crate::detection::refresh_channel(channel.clone()).await.unwrap();
    crate::supplier::merge_channel_detection_result(&mut channel, &refreshed);
    let selected = channel_upstream_model_for_request(&channel, Some("qwen-image-3"));
    assert_eq!(crate::openrouter::upstream_model(&channel, &selected), "qwen/qwen-image-3");
    let supply = crate::supplier::SupplierAgentConfig::single(crate::config::supplier_from_channel(&channel));
    let registration = crate::supplier::supplier_register_message(&supply);
    let registered_ops = registration.payload["channels"][0]["supported_operations"].as_array().unwrap();
    assert!(registered_ops.contains(&json!(crate::image_models::GENERATE)));
    assert!(registered_ops.contains(&json!(crate::image_models::EDIT)));
    let evidence = registration.payload["channels"][0]["native_capabilities"].as_array().unwrap();
    let advertises = |suffix: &str, op: &str| evidence.iter().any(|e|
        e["source"] == "openrouter_images" && e["state"] == "supported"
        && e["model_pattern"].as_str().is_some_and(|id| id.rsplit('/').next() == Some(suffix))
        && e["feature"] == format!("operation:{op}"));
    assert!(advertises("qwen-image-3", crate::image_models::GENERATE));
    assert!(advertises("minimax-h3-image", crate::image_models::EDIT));
    assert!(!advertises("minimax-h3-image", crate::image_models::GENERATE));
    assert!(!advertises("auto", crate::image_models::EDIT));
    let ready = [channel.id.clone()].into_iter().collect();
    let mut config = crate::default_config();
    config.configure_image_skill = false;
    config.channels = vec![channel];
    let entries = local_model_entries(&config, &ready);
    let resp = local_models_response(&config, crate::surface::ApiSurface::OpenAi, &entries, true).unwrap();
    let body = crate::test_body_bytes(resp.into_body()).await.unwrap();
    let catalog = serde_json::from_slice(&body).unwrap();
    let found = crate::media_cli::discovery(&catalog).unwrap();
    assert_eq!(found["video"], json!(["wan-3.0"]));
    assert_eq!(found["speech"], json!(["heygen-voice-1-turbo:free"]));
    assert_eq!(found["native_operations"][crate::media_models::CHAT_MUSIC], json!(["lyria-3-clip-preview"]));
    assert!(found["declared_outputs"]["audio"].as_array().unwrap().contains(&json!("audio-chat")));
    let images: std::collections::BTreeMap<_, _> = crate::image_models::catalog_models(&catalog).unwrap()
        .map(|m| (m.id, (m.operations.generate, m.operations.edit))).collect();
    assert_eq!(images, [("example:free",(true,false)),("minimax-h3-image",(false,true)),("qwen-image-3",(true,true))].into_iter().collect());
    server.abort();
}

#[tokio::test]
async fn native_image_discovery_keeps_market_rates_scoped_to_market_operations_with_skill_off() {
    use crate::image_models::{EDIT, GENERATE};
    let mut config = default_config();
    config.configure_image_skill = false;
    let pricing = serde_json::json!({
        "currency":"USD","price_version_id":"test",
        "base_rates":[{"meter_kind":"image_output_unit","unit":"per_image","price_usd":0.05}],
        "offer_ratios":{(GENERATE):{"min_ratio":0.5,"max_ratio":1.0}}
    });
    let platform = serde_json::json!({"data":[{"id":"canvas","const_api":{
        "image_operations":[GENERATE],"platform_image_pricing":pricing
    }}]});
    let mut local = [serde_json::json!({"id":"canvas","const_api":{"image_operations":[EDIT]}})];
    crate::model_discovery::mark_directory_source(&mut local[0], "local");
    for prefer_local in [true, false] {
        let response = merge_local_models_response(
            warp::reply::json(&platform).into_response(), &config,
            crate::surface::ApiSurface::OpenAi, &local, prefer_local, true,
        ).await.unwrap();
        let bytes = crate::test_body_bytes(response.into_body()).await.unwrap();
        let payload = serde_json::from_slice(&bytes).unwrap();
        let model = crate::image_models::catalog_models(&payload).unwrap().next().unwrap();
        assert_eq!(model.id, "canvas");
        assert!(model.operations.generate && model.operations.edit);
        assert_eq!(model.platform_pricing, Some(&pricing));
        assert_eq!(payload["data"][0]["const_api"]["operations_by_source"], serde_json::json!({
            "local":[EDIT], "platform":[GENERATE]
        }));
    }
}

#[tokio::test]
async fn native_media_directory_is_available_with_no_skill_or_local_supplier() {
    let mut config = default_config();
    config.configure_image_skill = false;
    config.channels.clear();
    let platform = serde_json::json!({"data":[{"id":"studio-voice","const_api":{
        "media_operations":[crate::media_models::SPEECH],"speech_stream_format":"sse"
    }}]});
    for prefer_local in [true, false] {
        let response = merge_local_models_response(warp::reply::json(&platform).into_response(), &config,
            crate::surface::ApiSurface::OpenAi, &[], prefer_local, true).await.unwrap();
        let bytes = crate::test_body_bytes(response.into_body()).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(payload["data"][0]["const_api"]["media_operations"], serde_json::json!([crate::media_models::SPEECH]));
        assert_eq!(payload["data"][0]["const_api"]["speech_stream_format"], "sse");
    }
}

#[tokio::test]
async fn model_discovery_rules_survive_local_merge_on_all_surfaces() {
    let mut config = default_config();
    config.allow_model_equivalence = false;
    for surface in [
        crate::surface::ApiSurface::OpenAi,
        crate::surface::ApiSurface::Anthropic,
        crate::surface::ApiSurface::Gemini,
    ] {
        for prefer_local in [true, false] {
            let key = if surface == crate::surface::ApiSurface::Gemini {
                "models"
            } else {
                "data"
            };
            let entry = |id: &str| {
                if key == "models" {
                    serde_json::json!({"name":format!("models/{id}")})
                } else {
                    serde_json::json!({"id":id})
                }
            };
            let mut payload = serde_json::json!({key: [entry("claude-aaa"), entry("claude-future"), entry("claude-retired")]});
            payload[crate::model_discovery::POLICY_FIELD] = serde_json::json!({"schema_version":1,"rules":[
                {"pattern":"claude-future","priority":0,"hidden":false},
                {"pattern":"claude-retired","priority":0,"hidden":true}
            ]});
            let response = warp::reply::json(&payload).into_response();
            let local = vec![
                serde_json::json!({"id":"claude-retired"}),
                serde_json::json!({"id":"claude-local"}),
                serde_json::json!({"id":"claude-future","const_api":{
                    "capability_states":{"tool_calls":false},
                    "native_capabilities":[{"feature":"tool_calls","state":"supported"}]
                }}),
            ];
            let response =
                merge_local_models_response(response, &config, surface, &local, prefer_local, true)
                    .await
                    .unwrap();
            let body = crate::test_body_bytes(response.into_body()).await.unwrap();
            let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let models = payload[key].as_array().unwrap();
            assert_eq!(payload[crate::model_discovery::POLICY_SOURCE_FIELD], "platform");
            assert_eq!(
                payload[crate::model_discovery::POLICY_FIELD]["rules"][0]["priority"],
                2
            );
            assert_eq!(
                models
                    .iter()
                    .map(|model| model_id_for_surface(surface, model).unwrap())
                    .collect::<Vec<_>>(),
                ["claude-future", "claude-aaa", "claude-local"]
            );
            assert_eq!(
                models[0]["const_api"]["capability_states"]["tool_calls"],
                true
            );
            assert!(models[0]["const_api"].get("native_capabilities").is_none());
            if surface == crate::surface::ApiSurface::Anthropic {
                assert_eq!(payload["first_id"], "claude-future");
                assert_eq!(payload["last_id"], "claude-local");
            }
        }
    }
}

#[tokio::test]
async fn local_model_sorting_does_not_invent_admin_recommendations() {
    let config = default_config();
    let response = local_models_response(&config, crate::surface::ApiSurface::OpenAi,
        &[serde_json::json!({"id":"gpt-test"})], true).unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&crate::test_body_bytes(response.into_body()).await.unwrap()).unwrap();
    assert!(crate::model_discovery::ModelPresentation::from_payload(&payload).is_some());
    assert!(crate::model_discovery::ModelPresentation::recommendations_from_payload(&payload).unwrap().is_none());
    assert_eq!(payload["data"][0]["id"], "gpt-test");
    let legacy = warp::reply::json(&serde_json::json!({"data":[{"id":"gpt-test"}]})).into_response();
    let response = merge_local_models_response(legacy, &config, crate::surface::ApiSurface::OpenAi, &[], false, true).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&crate::test_body_bytes(response.into_body()).await.unwrap()).unwrap();
    assert!(crate::model_discovery::ModelPresentation::recommendations_from_payload(&payload).unwrap().is_none());
    // A cached/remerged local fallback must not become a platform recommendation.
    let response = merge_local_models_response(warp::reply::json(&payload).into_response(), &config, crate::surface::ApiSurface::OpenAi, &[], false, true).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&crate::test_body_bytes(response.into_body()).await.unwrap()).unwrap();
    assert!(crate::model_discovery::ModelPresentation::recommendations_from_payload(&payload).unwrap().is_none());
}

#[tokio::test]
async fn malformed_recommendations_remain_distinct_from_missing_rules_after_merge() {
    let payload = serde_json::json!({"data":[{"id":"gpt-test"}], "const_api_model_presentation":{"schema_version":99,"rules":[]}});
    let response = merge_local_models_response(warp::reply::json(&payload).into_response(), &default_config(), crate::surface::ApiSurface::OpenAi, &[], false, true).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&crate::test_body_bytes(response.into_body()).await.unwrap()).unwrap();
    assert_eq!(crate::model_discovery::ModelPresentation::recommendations_from_payload(&payload).unwrap_err(), "TOOL_MODEL_RECOMMENDATIONS_INVALID");
}

#[tokio::test]
async fn model_discovery_cache_reuses_reads_but_keeps_local_state_live_and_full_view_separate() {
    let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = hits.clone();
    let platform = warp::path!("v1" / "models")
        .and(warp::get())
        .and(warp::header::optional::<String>(
            crate::model_discovery::CATALOG_HEADER,
        ))
        .map(move |view: Option<String>| {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            assert!(matches!(view.as_deref(), Some("compact-v1" | "full-v1")));
            // An old server ignores negotiation and returns its full evidence.
            warp::reply::json(&serde_json::json!({"data":[{"id":"gpt-future","const_api":{
                "native_capabilities":[{"feature":"tool_calls","state":"supported"}]
            }}]}))
        });
    let (address, server) = crate::bind_ephemeral!(platform, ([127, 0, 0, 1], 0));
    let server = tokio::spawn(server);
    let shared = model_catalog_test_shared(address, Duration::from_secs(2));
    for i in 0..3 {
        let response = forward_with_failover(
            warp::http::Method::GET,
            "/v1/models",
            "",
            HeaderMap::new(),
            bytes::Bytes::new(),
            &shared,
        )
        .await
        .unwrap();
        assert_eq!(
            response.headers().get(MODEL_CATALOG_CACHE_HEADER).is_some(),
            i > 0
        );
        let body = crate::test_body_bytes(response.into_body()).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["data"][0]["id"], "gpt-future");
        assert!(payload["data"][0]["const_api"]
            .get("native_capabilities")
            .is_none());
        if i == 1 {
            for ready in shared.local_channel_readiness.lock().unwrap().values_mut() {
                *ready = false;
            }
        }
        if i == 2 {
            assert_eq!(payload["data"].as_array().unwrap().len(), 1);
        }
    }
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    let mut headers = HeaderMap::new();
    headers.insert(
        crate::model_discovery::CATALOG_HEADER,
        warp::http::HeaderValue::from_static("full-v1"),
    );
    let response = forward_with_failover(
        warp::http::Method::GET,
        "/v1/models",
        "",
        headers,
        bytes::Bytes::new(),
        &shared,
    )
    .await
    .unwrap();
    let body = crate::test_body_bytes(response.into_body()).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(payload["data"][0]["const_api"]["native_capabilities"].is_array());
    let mut headers = HeaderMap::new();
    headers.insert(
        "cache-control",
        warp::http::HeaderValue::from_static("no-cache"),
    );
    forward_with_failover(
        warp::http::Method::GET,
        "/v1/models",
        "",
        headers,
        bytes::Bytes::new(),
        &shared,
    )
    .await
    .unwrap();
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 3);
    server.abort();
}

#[test]
fn model_discovery_empty_snapshot_replaces_stale_and_cache_is_bounded() {
    let mut config = default_config();
    config.account_device_api_key = "sk-test-device".into();
    let shared = ProxyShared::from_config(config, Client::new());
    let surface = crate::surface::ApiSurface::OpenAi;
    for i in 0..40 {
        shared.remember_model_catalog_snapshot(
            surface,
            &format!("page={i}"),
            "sk-test-device",
            &bytes::Bytes::from_static(br#"{"data":[{"id":"old"}]}"#),
        );
    }
    assert!(shared.model_catalog_snapshots.lock().unwrap().len() <= 16);
    shared.remember_model_catalog_snapshot(
        surface,
        "page=39",
        "sk-test-device",
        &bytes::Bytes::from_static(br#"{"data":[]}"#),
    );
    let snapshots = shared.model_catalog_snapshots.lock().unwrap();
    let snapshot = snapshots
        .get(&ModelCatalogCacheKey {
            surface,
            raw_query: "page=39".into(),
        })
        .unwrap();
    assert_eq!(snapshot.body.as_ref(), br#"{"data":[]}"#);
}
