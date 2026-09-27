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
