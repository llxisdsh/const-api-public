    #[test]
    fn local_priority_tiers_balance_equal_accounts_without_overriding_fidelity() {
        let mut config = default_config();
        config.channels = (0..3).map(|i| {
            let mut channel = local_model_channel(&["model-a"], "https://priority.test/v1");
            channel.id = format!("priority-selection-{i}");
            channel.v2.local_priority = Some(if i == 2 { 2 } else { 1 });
            channel
        }).collect();
        let peers = || local_priority_candidates(&config, config.channels.iter().map(|c| (c, 0, 0)))
            .iter().map(|c| c.id.clone()).collect::<Vec<_>>();
        assert_eq!(peers(), vec!["priority-selection-0", "priority-selection-1"]);
        let active = crate::channel_concurrency::Guard::acquire(&config.channels[0].id, 0).unwrap();
        assert_eq!(peers(), vec!["priority-selection-1"]);
        let other_active = crate::channel_concurrency::Guard::acquire(&config.channels[1].id, 0).unwrap();
        assert_eq!(peers(), vec!["priority-selection-0", "priority-selection-1"], "an idle backup cannot override an available higher tier");
        let selected = select_local_channel_route_where(&config, "/v1/chat/completions", br#"{"model":"model-a","messages":[]}"#, |c| c.v2.local_priority == Some(2)).unwrap();
        assert_eq!(selected.channel.id, "priority-selection-2");
        for ranks in [(1, 0), (0, 1)] {
            let best = select_local_priority_channel(&config, config.channels.iter().enumerate()
                .map(|(i, c)| if i == 2 { (c, 0, 0) } else { (c, ranks.0, ranks.1) })).unwrap();
            assert_eq!(best.id, "priority-selection-2", "capability and protocol fidelity precede tiers");
        }
        let before = local_model_route_preference_context(&config, false, true);
        config.channels[2].v2.local_priority = Some(1);
        let after = local_model_route_preference_context(&config, false, true);
        assert_ne!(before.policy_fingerprint, after.policy_fingerprint);
        drop((active, other_active));
    }

    #[tokio::test]
    async fn local_priority_http_balances_new_sessions_keeps_affinity_and_falls_back_at_capacity() {
        let upstream = warp::path!(String / "v1" / "chat" / "completions")
            .map(|account: String| warp::reply::json(&serde_json::json!({"account":account,"choices":[]})));
        let (addr, server) = crate::bind_ephemeral!(upstream, ([127, 0, 0, 1], 0));
        let task = tokio::spawn(server);
        let mut config = default_config();
        config.endpoints.clear();
        config.account_device_api_key.clear();
        config.prefer_local_supply = true;
        config.allow_model_equivalence = false;
        config.channels = ["a", "b", "backup"].iter().map(|name| {
            let mut channel = local_model_channel(&["model-a"], &format!("http://{addr}/{name}/v1"));
            channel.id = format!("priority-http-{name}");
            channel.v2.max_concurrency = 2;
            channel.v2.local_priority = Some(if *name == "backup" { 2 } else { 1 });
            channel
        }).collect();
        let shared = Arc::new(ProxyShared::from_config(normalize_config(config), Client::new()));
        async fn request(shared: &Arc<ProxyShared>, session: &str) -> String {
            let mut headers = HeaderMap::new();
            headers.insert("x-conversation-id", session.parse().unwrap());
            let response = forward_with_failover(warp::http::Method::POST, "/v1/chat/completions", "", headers,
                bytes::Bytes::from_static(br#"{"model":"model-a","messages":[]}"#), shared).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = crate::test_body_bytes(response.into_body()).await.unwrap();
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["account"].as_str().unwrap().to_string()
        }
        let a_busy = crate::channel_concurrency::Guard::acquire("priority-http-a", 2).unwrap();
        assert_eq!(request(&shared, "session-b").await, "b", "new session uses the less busy peer");
        drop(a_busy);
        let b_busy = crate::channel_concurrency::Guard::acquire("priority-http-b", 2).unwrap();
        assert_eq!(request(&shared, "session-b").await, "b", "existing session remains on its account");
        assert_eq!(request(&shared, "session-a").await, "a", "another session can use the idle peer");
        let a1 = crate::channel_concurrency::Guard::acquire("priority-http-a", 2).unwrap();
        let a2 = crate::channel_concurrency::Guard::acquire("priority-http-a", 2).unwrap();
        let b2 = crate::channel_concurrency::Guard::acquire("priority-http-b", 2).unwrap();
        assert_eq!(request(&shared, "session-backup").await, "backup", "full first tier falls through without spending upstream attempts");
        drop((a1, a2, b_busy, b2));
        assert_ne!(request(&shared, "session-recovered").await, "backup", "new sessions return to the recovered first tier");
        task.abort();
    }

    #[tokio::test]
    async fn local_priority_one_shot_request_uses_headroom_before_a_full_channel() {
        let upstream = warp::path!(String / "v1" / "embeddings")
            .map(|account: String| warp::reply::json(&serde_json::json!({"account":account,"data":[]})));
        let (addr, server) = crate::bind_ephemeral!(upstream, ([127, 0, 0, 1], 0));
        let task = tokio::spawn(server);
        let mut config = default_config();
        config.endpoints.clear();
        config.account_device_api_key.clear();
        config.prefer_local_supply = true;
        config.channels = [("small", 1, 1), ("large", 20, 1), ("backup", 20, 2)].iter().map(|(name, limit, tier)| {
            let mut channel = local_model_channel(&["embed-a"], &format!("http://{addr}/{name}/v1"));
            channel.id = format!("one-shot-{name}");
            channel.v2.max_concurrency = *limit;
            channel.v2.local_priority = Some(*tier);
            channel
        }).collect();
        let shared = Arc::new(ProxyShared::from_config(normalize_config(config), Client::new()));
        async fn embed(shared: &Arc<ProxyShared>) -> (StatusCode, String) {
            let response = forward_with_failover(warp::http::Method::POST, "/v1/embeddings", "", HeaderMap::new(),
                bytes::Bytes::from_static(br#"{"model":"embed-a","input":"hi"}"#), shared).await.unwrap();
            let status = response.status();
            let body = crate::test_body_bytes(response.into_body()).await.unwrap();
            let account = serde_json::from_slice::<serde_json::Value>(&body).ok()
                .and_then(|value| value["account"].as_str().map(str::to_string)).unwrap_or_default();
            (status, account)
        }
        // A full small channel loses to a busier peer that still has headroom.
        let small = crate::channel_concurrency::Guard::acquire("one-shot-small", 1).unwrap();
        let large = [(); 2].map(|()| crate::channel_concurrency::Guard::acquire("one-shot-large", 20).unwrap());
        assert_eq!(embed(&shared).await, (StatusCode::OK, "large".into()));
        // A full first tier falls through to the next tier instead of returning 429.
        let large_rest = (2..20).map(|_| crate::channel_concurrency::Guard::acquire("one-shot-large", 20).unwrap()).collect::<Vec<_>>();
        assert_eq!(embed(&shared).await, (StatusCode::OK, "backup".into()));
        drop((small, large, large_rest));
        task.abort();
    }

    #[test]
    fn malformed_optional_model_metadata_cannot_panic_during_capacity_merge() {
        for malformed in [serde_json::json!("unexpected"), serde_json::json!([1]), serde_json::json!(true), serde_json::Value::Null] {
            let mut preferred = serde_json::json!({"id":"unknown-context-model","const_api":malformed});
            let additional = serde_json::json!({"id":"unknown-context-model","const_api":{"context_tokens":1000000}});
            merge_model_entry_metadata(&mut preferred, &additional);
            assert!(preferred["const_api"].is_object());
            assert!(preferred["const_api"]["context_tokens"].is_null());
            assert_eq!(preferred["const_api"]["supports_1m"], false);
        }
    }

    #[test]
    fn image_generation_selects_account_subscription_without_exposing_it_to_other_opaque_routes() {
        let mut config = default_config();
        config.channels = vec![openai_subscription_channel(&["gpt-5.6-sol"])];
        let ready = HashSet::from(["openai-account-subscription".to_string()]);
        let body = br#"{
            "model":"gpt-image-2",
            "prompt":"draw a geometric test card",
            "size":"auto",
            "quality":"auto"
        }"#;

        let image = select_ready_local_channel(
            &config,
            &warp::http::Method::POST,
            "/v1/images/generations",
            body,
            &ready,
        )
        .expect("image route selection")
        .expect("subscription image channel");
        assert_eq!(image.channel.id, "openai-account-subscription");
        assert_eq!(image.route_model, "gpt-image-2");

        let edit = select_ready_local_channel(
            &config,
            &warp::http::Method::POST,
            "/v1/images/edits",
            body,
            &ready,
        )
        .expect("image edit route selection")
        .expect("subscription image edit channel");
        assert_eq!(edit.channel.id, "openai-account-subscription");
        assert_eq!(edit.route_model, "gpt-image-2");

        let now = now_unix();
        let image_quota = HashMap::from([(
            crate::supplier::model_quota_route_key("openai-account-subscription", "gpt-image-2"),
            crate::supplier::LocalModelQuotaRouteState {
                cooldown_until_unix: now + 60,
                failed_at_unix: now,
                ..Default::default()
            },
        )]);
        let blocked = select_ready_local_channel_with_model_quota(
            &config,
            &warp::http::Method::POST,
            "/v1/images/generations",
            body,
            &ready,
            &image_quota,
            now,
        )
        .expect("model-scoped image quota selection");
        assert!(blocked.is_none());

        let opaque = select_ready_local_channel(
            &config,
            &warp::http::Method::POST,
            "/v1/files/raw",
            body,
            &ready,
        )
        .expect("opaque route selection");
        assert!(opaque.is_none());
    }

    #[test]
    fn model_quota_cooldown_skips_only_that_channel_model_route() {
        let mut first = local_model_channel(&["model-a", "model-b"], "https://first.test/v1");
        first.id = "first".to_string();
        let mut second = local_model_channel(&["model-a", "model-b"], "https://second.test/v1");
        second.id = "second".to_string();
        let mut config = default_config();
        config.channels = vec![first, second];
        let ready = HashSet::from(["first".to_string(), "second".to_string()]);
        let now = now_unix();
        let routes = HashMap::from([
            (
                crate::supplier::model_quota_route_key("first", "model-a"),
                crate::supplier::LocalModelQuotaRouteState {
                    cooldown_until_unix: now + 60,
                    failed_at_unix: now,
                    ..Default::default()
                },
            ),
            (
                crate::supplier::model_quota_route_key("first", "model-b"),
                crate::supplier::LocalModelQuotaRouteState::default(),
            ),
            (
                crate::supplier::model_quota_route_key("second", "model-a"),
                crate::supplier::LocalModelQuotaRouteState::default(),
            ),
        ]);

        let model_a = select_ready_local_channel_with_model_quota(
            &config,
            &warp::http::Method::POST,
            "/v1/chat/completions",
            br#"{"model":"model-a","messages":[]}"#,
            &ready,
            &routes,
            now,
        )
        .expect("model-a route")
        .expect("model-a fallback channel");
        assert_eq!(model_a.channel.id, "second");

        let model_b = select_ready_local_channel_with_model_quota(
            &config,
            &warp::http::Method::POST,
            "/v1/chat/completions",
            br#"{"model":"model-b","messages":[]}"#,
            &ready,
            &routes,
            now,
        )
        .expect("model-b route")
        .expect("model-b remains available");
        assert_eq!(model_b.channel.id, "first");
    }

    #[test]
    fn local_soft_model_failure_keeps_last_resort_without_overriding_healthy_priority() {
        let mut first = local_model_channel(&["model-a", "model-b"], "https://first.test/v1");
        first.id = "first".to_string();
        let mut second = first.clone();
        second.id = "second".to_string();
        let mut config = default_config();
        config.channels = vec![first, second];
        let ready = HashSet::from(["first".to_string(), "second".to_string()]);
        let now = now_unix();
        let key = crate::supplier::model_quota_route_key("first", "model-a");
        let mut failure = crate::supplier::LocalModelQuotaRouteState::default();
        failure.record_runtime_failure(now, 0, true);
        let mut routes = HashMap::from([(key.clone(), failure)]);
        let select = |cfg: &ClientConfig, routes: &HashMap<String, crate::supplier::LocalModelQuotaRouteState>| {
            select_ready_local_channel_with_model_quota(
                cfg, &warp::http::Method::POST, "/v1/chat/completions",
                br#"{"model":"model-a","messages":[]}"#, &ready, routes, now,
            ).unwrap().map(|selected| selected.channel.id.clone())
        };
        assert_eq!(select(&config, &routes).as_deref(), Some("second"));
        config.channels.truncate(1);
        assert_eq!(select(&config, &routes).as_deref(), Some("first"), "do not turn a soft failure into no route");
        assert!(routes.get_mut(&key).unwrap().claim_runtime_probe(now));
        assert!(select(&config, &routes).is_none(), "only one recovery attempt can be in flight");
        routes.get_mut(&key).unwrap().record_success();
        assert_eq!(select(&config, &routes).as_deref(), Some("first"));
        routes.get_mut(&key).unwrap().record_runtime_failure(now, 1800, true);
        assert!(select(&config, &routes).is_none(), "an explicit retry hint must still be respected");
    }

    #[test]
    fn image_generation_prefers_ready_account_subscription_over_openai_api() {
        let direct =
            verified_openai_channel("openai-api", &["gpt-image-2"], "https://api.openai.com/v1");
        let mut config = default_config();
        config.channels = vec![openai_subscription_channel(&["gpt-5.6-sol"]), direct];
        let ready = HashSet::from([
            "openai-account-subscription".to_string(),
            "openai-api".to_string(),
        ]);

        let selected = select_ready_local_channel(
            &config,
            &warp::http::Method::POST,
            "/v1/images/generations",
            br#"{"model":"gpt-image-2","prompt":"draw a tiny test card"}"#,
            &ready,
        )
        .expect("image route selection")
        .expect("account subscription image channel");

        assert_eq!(selected.channel.id, "openai-account-subscription");
        assert_eq!(selected.route_model, "gpt-image-2");
        assert_eq!(
            crate::channel_executor::execution_kind_for_source(selected.channel.source_driver())
                .expect("subscription execution kind"),
            crate::source_driver::ExecutionKind::OpenAiSubscription
        );
    }

    #[test]
    fn image_generation_falls_back_to_ready_openai_api_when_subscription_is_not_ready() {
        let direct =
            verified_openai_channel("openai-api", &["gpt-image-2"], "https://api.openai.com/v1");
        let mut config = default_config();
        config.channels = vec![openai_subscription_channel(&["gpt-5.6-sol"]), direct];
        let ready = HashSet::from(["openai-api".to_string()]);

        let selected = select_ready_local_channel(
            &config,
            &warp::http::Method::POST,
            "/v1/images/generations",
            br#"{"model":"gpt-image-2","prompt":"draw a tiny test card"}"#,
            &ready,
        )
        .expect("image route selection")
        .expect("fallback OpenAI API image channel");

        assert_eq!(selected.channel.id, "openai-api");
        assert_eq!(selected.route_model, "gpt-image-2");
        assert_eq!(
            crate::channel_executor::execution_kind_for_source(selected.channel.source_driver())
                .expect("direct execution kind"),
            crate::source_driver::ExecutionKind::HttpSurface
        );
    }

    #[test]
    fn embeddings_selects_matching_direct_http_surface_not_account_subscription() {
        let mut direct = verified_openai_channel(
            "direct-openai",
            &["text-embedding-3-large"],
            "https://api.test/v1",
        );
        direct.upstream_model = "text-embedding-3-large".to_string();
        let mut config = default_config();
        config.channels = vec![
            openai_subscription_channel(&["text-embedding-3-large"]),
            direct,
        ];
        let ready = HashSet::from([
            "openai-account-subscription".to_string(),
            "direct-openai".to_string(),
        ]);

        let selected = select_ready_local_channel(
            &config,
            &warp::http::Method::POST,
            "/v1/embeddings",
            br#"{"model":"text-embedding-3-large","input":"embed me"}"#,
            &ready,
        )
        .expect("embedding route selection")
        .expect("direct embedding channel");

        assert_eq!(selected.channel.id, "direct-openai");
        assert_eq!(
            crate::channel_executor::execution_kind_for_source(selected.channel.source_driver())
                .expect("direct execution kind"),
            crate::source_driver::ExecutionKind::HttpSurface
        );
    }

    #[test]
    fn subscription_image_urls_follow_codex_native_images_endpoints() {
        assert_eq!(
            codex_image_url_from_responses_url(
                "https://chatgpt.com/backend-api/codex/responses",
                "images/generations",
            ),
            "https://chatgpt.com/backend-api/codex/images/generations"
        );
        assert_eq!(
            codex_image_url_from_responses_url(
                "https://example.test/backend-api/codex/responses?client_version=1.2.3",
                "images/edits",
            ),
            "https://example.test/backend-api/codex/images/edits?client_version=1.2.3"
        );
    }

    #[test]
    fn subscription_image_request_preserves_native_body_and_codex_headers() {
        let body = bytes::Bytes::from_static(
            br#"{"model":"gpt-image-2","prompt":"draw a test card","quality":"low"}"#,
        );
        let mut inbound_headers = HeaderMap::new();
        inbound_headers.insert(
            "content-type",
            "application/json; charset=utf-8"
                .parse()
                .expect("content type"),
        );
        inbound_headers.insert("originator", "Codex Desktop".parse().expect("originator"));
        inbound_headers.insert(
            "user-agent",
            "Codex Desktop/0.999.0 (Windows 11; x86_64) vscode"
                .parse()
                .expect("user agent"),
        );
        inbound_headers.insert(
            "x-codex-turn-metadata",
            r#"{"turn_id":"turn-test"}"#.parse().expect("turn metadata"),
        );
        inbound_headers.insert(
            "x-client-request-id",
            "request-test".parse().expect("client request id"),
        );
        let request = openai_subscription_image_request_builder(
            &Client::new(),
            "https://chatgpt.com/backend-api/codex/images/generations",
            "test-access-token",
            &serde_json::json!({"account_id":"account-test"}),
            &inbound_headers,
            OpenAiSubscriptionImagePayload {
                body: body.clone(),
                content_type: "application/json; charset=utf-8".to_string(),
            },
        )
        .build()
        .expect("native image request");

        assert_eq!(
            request.url().as_str(),
            "https://chatgpt.com/backend-api/codex/images/generations"
        );
        assert_eq!(
            request
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok()),
            Some("Bearer test-access-token")
        );
        assert_eq!(
            request
                .headers()
                .get("chatgpt-account-id")
                .and_then(|v| v.to_str().ok()),
            Some("account-test")
        );
        assert_eq!(
            request
                .headers()
                .get("originator")
                .and_then(|v| v.to_str().ok()),
            Some(active_codex_identity().originator.as_str())
        );
        assert_eq!(
            request
                .headers()
                .get("user-agent")
                .and_then(|v| v.to_str().ok()),
            Some(active_codex_identity().user_agent.as_str())
        );
        assert_eq!(
            request
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("application/json; charset=utf-8")
        );
        assert_eq!(
            request
                .headers()
                .get("version")
                .and_then(|value| value.to_str().ok()),
            Some(active_codex_identity().version.as_str())
        );
        assert_eq!(
            request
                .headers()
                .get("x-codex-turn-metadata")
                .and_then(|v| v.to_str().ok()),
            Some(r#"{"turn_id":"turn-test"}"#)
        );
        assert_eq!(
            request
                .headers()
                .get("x-client-request-id")
                .and_then(|v| v.to_str().ok()),
            Some("request-test")
        );
        assert!(request.headers().get("openai-beta").is_none());
        assert_eq!(
            request.body().and_then(reqwest::Body::as_bytes),
            Some(body.as_ref())
        );
    }
    #[tokio::test]
    async fn subscription_identity_codex_http_wire_is_shared_across_text_image_and_voice() {
        use warp::Filter;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let route = warp::header::headers_cloned().and(warp::body::bytes())
            .map(move |headers: reqwest::header::HeaderMap, body: bytes::Bytes| {
                tx.send((headers, body)).unwrap();
                "{}"
            });
        let (address, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
        let server = tokio::spawn(server);
        let client = Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().unwrap();
        let identity = active_codex_identity();
        let credential = serde_json::json!({"account_id":"fixture-account"});
        let mut headers = warp::http::HeaderMap::new();
        headers.insert("originator", "codex_vscode".parse().unwrap());
        headers.insert("user-agent", "codex_vscode/9.9.9".parse().unwrap());
        headers.insert("version", "9.9.9".parse().unwrap());
        headers.insert("session-id", "fixture-session".parse().unwrap());
        headers.insert("x-codex-turn-metadata", r#"{"turn_id":"fixture-turn"}"#.parse().unwrap());
        headers.insert("openai-beta", "future-feature=v1".parse().unwrap());
        headers.insert("openai-alpha", "quicksilver=v1".parse().unwrap());
        headers.insert("authorization", "Bearer must-not-forward".parse().unwrap());
        headers.insert("cookie", "must-not-forward".parse().unwrap());
        let body = r#"{ "input":"fixture", "tools":[{"type":"function","name":"probe","parameters":{"type":"object","properties":{}}}],"tool_choice":"required" }"#;
        let url = format!("http://{address}/test");
        let requests = [
            crate::supplier::openai_subscription_response_request(&client, &url, "fixture-token", &credential, body.into(), Some(&headers)).unwrap(),
            openai_subscription_image_request_builder(&client, &url, "fixture-token", &credential, &headers,
                OpenAiSubscriptionImagePayload { body: body.into(), content_type:"application/json".into() }),
            openai_subscription_realtime_call_request(&client, &url, "fixture-token", &credential, &headers,
                OpenAiSubscriptionRealtimeCallPayload { body: body.into(), content_type:"application/json", upstream_model:"fixture".into() }),
        ];
        for (index, request) in requests.into_iter().enumerate() {
            assert!(request.send().await.unwrap().status().is_success());
            let (received, payload) = rx.recv().await.unwrap();
            for (name, expected) in [("originator", &identity.originator), ("user-agent", &identity.user_agent), ("version", &identity.version)] {
                assert_eq!(received[name], *expected);
            }
            assert_eq!(received["authorization"], "Bearer fixture-token");
            assert_eq!(received["chatgpt-account-id"], "fixture-account");
            assert_eq!(received["session-id"], "fixture-session");
            assert_eq!(received["x-codex-turn-metadata"], headers["x-codex-turn-metadata"]);
            assert!(!received.contains_key("cookie"));
            assert!(received["openai-beta"].to_str().unwrap().contains("future-feature=v1"));
            if index == 2 { assert_eq!(received["openai-alpha"], "quicksilver=v1"); }
            let payload = if index == 0 {
                zstd::stream::decode_all(std::io::Cursor::new(payload)).unwrap()
            } else { payload.to_vec() };
            assert_eq!(payload, body.as_bytes());
        }
        server.abort();
    }

    #[tokio::test]
    async fn subscription_images_preserve_json_and_normalize_public_multipart_edits() {
        let boundary = "const-api-image-boundary";
        let multipart_body = bytes::Bytes::from_static(
            concat!(
                "--const-api-image-boundary\r\n",
                "Content-Disposition: form-data; name=\"model\"\r\n\r\n",
                "gpt-image-2\r\n",
                "--const-api-image-boundary\r\n",
                "Content-Disposition: form-data; name=\"prompt\"\r\n\r\n",
                "add a tiny mark\r\n",
                "--const-api-image-boundary\r\n",
                "Content-Disposition: form-data; name=\"n\"\r\n\r\n",
                "1\r\n",
                "--const-api-image-boundary\r\n",
                "Content-Disposition: form-data; name=\"stream\"\r\n\r\n",
                "true\r\n",
                "--const-api-image-boundary\r\n",
                "Content-Disposition: form-data; name=\"client_trace\"\r\n\r\n",
                "preserve-me\r\n",
                "--const-api-image-boundary\r\n",
                "Content-Disposition: form-data; name=\"mask[image_url]\"\r\n\r\n",
                "data:image/png;base64,bWFzaw==\r\n",
                "--const-api-image-boundary\r\n",
                "Content-Disposition: form-data; name=\"image[]\"; filename=\"tiny.png\"\r\n",
                "Content-Type: image/png\r\n\r\n",
                "tiny-png\r\n",
                "--const-api-image-boundary--\r\n"
            )
            .as_bytes(),
        );
        let mut headers = HeaderMap::new();
        headers.insert(
            "content-type",
            format!("multipart/form-data; boundary={boundary}")
                .parse()
                .expect("multipart content type"),
        );

        let normalized =
            normalize_openai_subscription_image_payload("images/edits", &headers, multipart_body)
                .await
                .expect("normalize image edit multipart");
        assert_eq!(normalized.content_type, "application/json");
        let value: serde_json::Value =
            serde_json::from_slice(&normalized.body).expect("normalized image edit JSON");
        assert_eq!(value["model"], "gpt-image-2");
        assert_eq!(value["prompt"], "add a tiny mark");
        assert_eq!(value["n"], 1);
        assert_eq!(value["stream"], true);
        assert_eq!(value["client_trace"], "preserve-me");
        assert_eq!(
            value
                .pointer("/mask/image_url")
                .and_then(serde_json::Value::as_str),
            Some("data:image/png;base64,bWFzaw==")
        );
        let expected_image = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(b"tiny-png")
        );
        assert_eq!(
            value
                .pointer("/images/0/image_url")
                .and_then(serde_json::Value::as_str),
            Some(expected_image.as_str())
        );

        let native = bytes::Bytes::from_static(
            br#"{"model":"gpt-image-2","prompt":"keep exact JSON spacing"}"#,
        );
        let normalized = normalize_openai_subscription_image_payload(
            "images/generations",
            &HeaderMap::new(),
            native.clone(),
        )
        .await
        .expect("preserve native image JSON");
        assert_eq!(normalized.body, native);
        assert_eq!(normalized.content_type, "application/json");
    }

    #[test]
    fn one_model_or_optional_feature_failure_does_not_take_the_channel_offline() {
        assert!(!local_response_marks_channel_unready_for_request(
            "POST",
            "/v1/images/generations",
            StatusCode::SERVICE_UNAVAILABLE,
        ));
        assert!(!local_response_marks_channel_unready_for_request(
            "POST",
            "/v1/images/edits",
            StatusCode::SERVICE_UNAVAILABLE,
        ));
        assert!(!local_response_marks_channel_unready_for_request(
            "POST",
            "/v1/responses",
            StatusCode::SERVICE_UNAVAILABLE,
        ));
        assert!(local_response_marks_channel_unready_for_request(
            "POST",
            "/v1/images/generations",
            StatusCode::UNAUTHORIZED,
        ));
        assert!(!local_response_marks_channel_unready_for_request(
            "POST",
            "/v1/responses",
            StatusCode::NOT_IMPLEMENTED,
        ));
        assert!(local_response_allows_request_retry(
            "POST",
            "/v1/responses",
            StatusCode::SERVICE_UNAVAILABLE,
        ));
        assert!(local_response_allows_request_retry(
            "POST",
            "/anthropic/v1/messages",
            StatusCode::NOT_FOUND,
        ));
        assert!(!local_response_allows_request_retry(
            "GET",
            "/v1/files/file-1",
            StatusCode::NOT_FOUND,
        ));
        assert_eq!(
            local_channel_readiness_observation(
                StatusCode::SERVICE_UNAVAILABLE,
                false,
                false,
            ),
            None,
            "a request-local failure must not overwrite existing channel readiness"
        );
        assert_eq!(
            local_channel_readiness_observation(StatusCode::SERVICE_UNAVAILABLE, true, false),
            None,
            "a model-local failure must not overwrite channel readiness"
        );
        assert_eq!(
            local_channel_readiness_observation(StatusCode::SERVICE_UNAVAILABLE, false, true),
            Some(false)
        );
        assert_eq!(
            local_channel_readiness_observation(StatusCode::OK, false, false),
            Some(true),
            "a successful real request is direct channel-health evidence"
        );
    }

    #[tokio::test]
    async fn verified_local_subscription_opaque_request_preserves_wire_shape() {
        let upstream = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind opaque upstream");
        let upstream_addr = upstream.local_addr().expect("opaque upstream address");
        let (captured_tx, captured_rx) = tokio::sync::oneshot::channel();
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.expect("opaque upstream request");
            let mut request = Vec::new();
            let header_end = loop {
                let mut chunk = [0_u8; 1024];
                let read = stream
                    .read(&mut chunk)
                    .await
                    .expect("read opaque upstream request");
                assert!(read > 0, "opaque upstream closed before headers");
                request.extend_from_slice(&chunk[..read]);
                if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    break index + 4;
                }
            };
            let headers = std::str::from_utf8(&request[..header_end])
                .expect("opaque upstream headers")
                .to_string();
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().expect("content length"))
                })
                .unwrap_or(0);
            while request.len() < header_end + content_length {
                let mut chunk = [0_u8; 1024];
                let read = stream
                    .read(&mut chunk)
                    .await
                    .expect("read opaque upstream body");
                assert!(read > 0, "opaque upstream closed before body");
                request.extend_from_slice(&chunk[..read]);
            }
            let request_line = headers.lines().next().expect("opaque request line");
            captured_tx
                .send((
                    request_line.to_string(),
                    headers,
                    request[header_end..header_end + content_length].to_vec(),
                ))
                .expect("send opaque capture");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: 2\r\nConnection: close\r\n\r\n\x00\xff",
                )
                .await
                .expect("write opaque upstream response");
        });

        let base_url = format!("http://{upstream_addr}");
        let mut channel = local_model_channel(&["opaque-local-model"], &base_url);
        channel.kind = "subscription_adapter".to_string();
        channel.api_format = "subscription_skeleton".to_string();
        channel.subscription.platform = "openai".to_string();
        channel.surface_bindings = vec![ChannelSurfaceBinding {
            surface: crate::surface::ApiSurface::OpenAi,
            base_url,
            endpoint_profile: "openai_compatible".to_string(),
            auth_scheme: "none".to_string(),
            protocols: vec![ChannelProtocolBinding {
                protocol: "openai_chat".to_string(),
                preferred: true,
                verification: ProtocolVerification {
                    state: "verified".to_string(),
                    checked_at_unix: 1,
                    summary: String::new(),
                },
            }],
            operation_overrides: Vec::new(),
            verification: SurfaceVerification {
                state: "verified".to_string(),
                checked_at_unix: 1,
                summary: String::new(),
            },
        }];

        let mut headers = HeaderMap::new();
        headers.insert("x-opaque-marker", "preserve-me".parse().expect("header"));
        let request_body = bytes::Bytes::from_static(b"\xff\x00opaque-body");
        let response = forward_local_channel_once(
            warp::http::Method::PATCH,
            "/v1/future-operation",
            "mode=raw%2Fvalue",
            headers,
            request_body.clone(),
            &Client::new(),
            &default_config(),
            &channel,
        )
        .await
        .expect("verified opaque subscription forward");

        let (request_line, captured_headers, captured_body) =
            captured_rx.await.expect("captured opaque request");
        upstream_task.await.expect("opaque upstream task");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            request_line,
            "PATCH /v1/future-operation?mode=raw%2Fvalue HTTP/1.1"
        );
        assert!(captured_headers
            .to_ascii_lowercase()
            .contains("x-opaque-marker: preserve-me"));
        assert_eq!(captured_body, request_body);
    }

    #[test]
    fn local_selection_requires_a_ready_channel() {
        let mut cfg = default_config();
        cfg.channels = vec![local_model_channel(
            &["Local-Ready-Model"],
            "http://127.0.0.1:9/v1",
        )];
        let body = br#"{"model":"local-ready-model","messages":[]}"#;
        let mut ready_channel_ids = HashSet::new();

        assert!(select_ready_local_channel(
            &cfg,
            &warp::http::Method::POST,
            "/v1/chat/completions",
            body,
            &ready_channel_ids,
        )
        .unwrap()
        .is_none());
        assert!(local_model_entries(&cfg, &ready_channel_ids).is_empty());

        ready_channel_ids.insert("local-models".to_string());
        // A legacy local-only config must remain usable while load-time
        // normalization mirrors the retired sharing field.
        cfg.channels[0].share_enabled = false;
        cfg.channels[0].capability_profiles = vec![ChannelCapabilityProfile {
            protocol: "openai_chat".to_string(),
            reasoning: true,
            vision: true,
            tool_calls: true,
            verification_state: "verified".to_string(),
            ..Default::default()
        }];
        let entries = local_model_entries(&cfg, &ready_channel_ids);
        assert!(select_ready_local_channel(
            &cfg,
            &warp::http::Method::POST,
            "/v1/chat/completions",
            body,
            &ready_channel_ids,
        )
        .unwrap()
        .is_some());
        assert_eq!(entries[0]["id"], "Local-Ready-Model");
        assert_eq!(entries[0]["const_api"]["reasoning"], true);
        assert_eq!(entries[0]["const_api"]["vision"], true);
        assert_eq!(entries[0]["const_api"]["tool_calls"], true);
    }

    #[tokio::test]
    async fn ready_local_route_wins_when_local_is_preferred() {
        assert_generation_route_preference(true, "local", 0, 1).await;
    }

    #[tokio::test]
    async fn platform_route_wins_when_platform_is_preferred_and_available() {
        assert_generation_route_preference(false, "platform", 1, 0).await;
    }

    #[tokio::test]
    async fn online_exact_route_precedes_ready_local_compatibility_route() {
        let platform_headers = Arc::new(StdMutex::new(Vec::<(String, Option<String>)>::new()));
        let platform_headers_filter = platform_headers.clone();
        let platform = warp::method()
            .and(warp::path::full())
            .and(warp::header::optional::<String>(
                PLATFORM_MODEL_ROUTE_MODE_HEADER,
            ))
            .and(warp::header::optional::<String>(
                "x-const-api-allow-model-equivalence",
            ))
            .map(
                move |method: warp::http::Method,
                      path: warp::path::FullPath,
                      mode: Option<String>,
                      equivalence: Option<String>| {
                    if method == warp::http::Method::POST
                        && path.as_str() == "/v1/chat/completions"
                    {
                        platform_headers_filter
                            .lock()
                            .expect("platform headers")
                            .push((mode.unwrap_or_default(), equivalence));
                    }
                    warp::reply::json(&serde_json::json!({
                        "id": "platform-exact",
                        "source": "platform-exact",
                        "choices": []
                    }))
                },
            );
        let (platform_addr, platform_server) =
            crate::bind_ephemeral!(platform, ([127, 0, 0, 1], 0));
        let platform_task = tokio::spawn(platform_server);

        let local_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let local_calls_filter = local_calls.clone();
        let local = warp::any().map(move || {
            local_calls_filter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            warp::reply::json(&serde_json::json!({
                "id": "local-compatible",
                "source": "local-compatible",
                "choices": []
            }))
        });
        let (local_addr, local_server) =
            crate::bind_ephemeral!(local, ([127, 0, 0, 1], 0));
        let local_task = tokio::spawn(local_server);

        let mut config = default_config();
        config.account_device_api_key = "sk-platform-device".to_string();
        config.prefer_local_supply = true;
        config.endpoints = vec![Endpoint {
            server_id: String::new(),
            name: "platform".to_string(),
            base_url: format!("http://{platform_addr}"),
            supplier_ws_url: String::new(),
            supplier_quic_url: String::new(),
            enabled: true,
        }];
        config.channels = vec![local_model_channel(
            &["model-b"],
            &format!("http://{local_addr}/v1"),
        )];
        configure_model_compatibility_pair(&mut config, "model-a", "model-b");
        let shared = Arc::new(ProxyShared::from_config(
            normalize_config(config),
            Client::new(),
        ));

        let response = forward_with_failover(
            warp::http::Method::POST,
            "/v1/chat/completions",
            "",
            HeaderMap::new(),
            bytes::Bytes::from_static(br#"{"model":"model-a","messages":[]}"#),
            &shared,
        )
        .await
        .expect("platform exact response");
        let body = crate::test_body_bytes(response.into_body())
            .await
            .expect("platform body");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).expect("platform json")["source"],
            "platform-exact"
        );
        assert_eq!(
            platform_headers.lock().expect("platform headers").as_slice(),
            [("exact-only".to_string(), None)]
        );
        assert_eq!(
            local_calls.load(std::sync::atomic::Ordering::SeqCst),
            0
        );

        platform_task.abort();
        local_task.abort();
    }

    #[tokio::test]
    async fn local_route_preference_respects_fidelity_and_supply_order_after_login() {
        let platform_requests = Arc::new(StdMutex::new(Vec::<(String, String)>::new()));
        let platform_requests_filter = platform_requests.clone();
        let platform = warp::method()
            .and(warp::path::full())
            .and(warp::header::optional::<String>(
                PLATFORM_MODEL_ROUTE_MODE_HEADER,
            ))
            .and(warp::body::bytes())
            .map(
                move |method: warp::http::Method,
                      path: warp::path::FullPath,
                      mode: Option<String>,
                      body: bytes::Bytes| {
                    if method != warp::http::Method::POST
                        || path.as_str() != "/v1/chat/completions"
                    {
                        return warp::reply::with_status("ok", StatusCode::OK).into_response();
                    }
                    let model = serde_json::from_slice::<serde_json::Value>(&body)
                        .ok()
                        .and_then(|value| {
                            value
                                .get("model")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_string)
                        })
                        .unwrap_or_default();
                    let mode = mode.unwrap_or_default();
                    platform_requests_filter
                        .lock()
                        .expect("platform requests")
                        .push((model.clone(), mode.clone()));
                    if model == "model-fallback" {
                        let response = warp::reply::with_status(
                            warp::reply::json(&serde_json::json!({
                                "error": {"message": "no exact platform route"}
                            })),
                            StatusCode::SERVICE_UNAVAILABLE,
                        );
                        let response = warp::reply::with_header(
                            response,
                            PLATFORM_MODEL_ROUTE_MODE_APPLIED_HEADER,
                            mode,
                        );
                        let response = warp::reply::with_header(
                            response,
                            PLATFORM_UPSTREAM_ATTEMPTS_USED_HEADER,
                            "0",
                        );
                        return warp::reply::with_header(
                            response,
                            PLATFORM_MODEL_FALLBACK_SAFE_HEADER,
                            "1",
                        )
                        .into_response();
                    }
                    warp::reply::json(&serde_json::json!({
                        "id": "platform-exact",
                        "source": format!("platform:{model}"),
                        "choices": []
                    }))
                    .into_response()
                },
            );
        let (platform_addr, platform_server) =
            crate::bind_ephemeral!(platform, ([127, 0, 0, 1], 0));
        let platform_task = tokio::spawn(platform_server);

        let local_models = Arc::new(StdMutex::new(Vec::<String>::new()));
        let local_models_filter = local_models.clone();
        let local = warp::path::full().and(warp::body::bytes()).map(
            move |path: warp::path::FullPath, body: bytes::Bytes| {
                if path.as_str() == "/healthz" {
                    return warp::reply::with_status("ok", StatusCode::OK).into_response();
                }
                let model = serde_json::from_slice::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("model")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                    })
                    .unwrap_or_default();
                local_models_filter
                    .lock()
                    .expect("local models")
                    .push(model);
                warp::reply::json(&serde_json::json!({
                    "id": "local",
                    "source": "local",
                    "choices": []
                }))
                .into_response()
            },
        );
        let (local_addr, local_server) =
            crate::bind_ephemeral!(local, ([127, 0, 0, 1], 0));
        let local_task = tokio::spawn(local_server);

        let mut config = default_config();
        config.account_device_api_key.clear();
        config.prefer_local_supply = true;
        config.endpoints = vec![Endpoint {
            server_id: String::new(),
            name: "platform".to_string(),
            base_url: format!("http://{platform_addr}"),
            supplier_ws_url: String::new(),
            supplier_quic_url: String::new(),
            enabled: true,
        }];
        config.channels = vec![local_model_channel(
            &["model-b"],
            &format!("http://{local_addr}/v1"),
        )];
        configure_model_compatibility_pair(&mut config, "model-a", "model-b");
        let profile_key = model_compatibility_profile_key("platform-1", "user-1")
            .expect("compatibility profile key");
        config
            .model_compatibility_profiles
            .get_mut(&profile_key)
            .expect("compatibility profile")
            .model_groups[0]
            .match_models
            .push("model-fallback".to_string());
        let config = normalize_config(config);
        let readiness = Arc::new(StdMutex::new(HashMap::from([(
            config.channels[0].id.clone(),
            true,
        )])));
        let platform_api_key = Arc::new(StdMutex::new(String::new()));
        let live_config = Arc::new(StdMutex::new(config.clone()));
        let shared = Arc::new(ProxyShared::with_platform_api_key(
            config,
            Client::new(),
            platform_api_key.clone(),
            live_config.clone(),
            readiness,
        ));

        for model in ["model-a", "model-b", "model-fallback"] {
            assert_eq!(
                forward_chat_json(&shared, model, "same-session").await["source"],
                "local"
            );
        }
        assert!(
            platform_requests
                .lock()
                .expect("platform requests")
                .is_empty()
        );

        *platform_api_key.lock().expect("platform key") = "device-key".to_string();
        assert_eq!(
            forward_chat_json(&shared, "model-a", "same-session").await["source"],
            "platform:model-a"
        );

        live_config
            .lock()
            .expect("live config")
            .prefer_local_supply = false;
        assert_eq!(
            forward_chat_json(&shared, "model-b", "same-session").await["source"],
            "platform:model-b"
        );

        live_config
            .lock()
            .expect("live config")
            .prefer_local_supply = true;
        assert_eq!(
            forward_chat_json(&shared, "model-fallback", "same-session").await["source"],
            "local"
        );
        assert_eq!(
            forward_chat_json(&shared, "model-fallback", "same-session").await["source"],
            "local"
        );

        assert_eq!(
            platform_requests
                .lock()
                .expect("platform requests")
                .as_slice(),
            [
                ("model-a".to_string(), "exact-only".to_string()),
                ("model-b".to_string(), "exact-only".to_string()),
                ("model-fallback".to_string(), "exact-only".to_string()),
                ("model-fallback".to_string(), "exact-only".to_string()),
            ]
        );
        assert_eq!(
            local_models.lock().expect("local models").as_slice(),
            ["model-b", "model-b", "model-b", "model-b", "model-b"]
        );

        platform_task.abort();
        local_task.abort();
    }

    #[tokio::test]
    async fn local_route_sticky_compatibility_yields_to_recovered_exact_routes() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let platform_exact_available = Arc::new(AtomicBool::new(false));
        let platform_available_filter = platform_exact_available.clone();
        let platform = warp::any().map(move || {
            if platform_available_filter.load(Ordering::SeqCst) {
                return warp::reply::json(&serde_json::json!({
                    "source": "platform-exact", "choices": []
                }))
                .into_response();
            }
            let response = warp::reply::with_status(
                warp::reply::json(&serde_json::json!({"error": {"message": "no exact route"}})),
                StatusCode::SERVICE_UNAVAILABLE,
            );
            let response = warp::reply::with_header(
                response,
                PLATFORM_MODEL_ROUTE_MODE_APPLIED_HEADER,
                "exact-only",
            );
            let response = warp::reply::with_header(
                response,
                PLATFORM_UPSTREAM_ATTEMPTS_USED_HEADER,
                "0",
            );
            warp::reply::with_header(response, PLATFORM_MODEL_FALLBACK_SAFE_HEADER, "1")
                .into_response()
        });
        let (platform_addr, platform_server) = crate::bind_ephemeral!(platform, ([127, 0, 0, 1], 0));
        let platform_task = tokio::spawn(platform_server);

        let fail_local_exact = Arc::new(AtomicBool::new(false));
        let fail_local_filter = fail_local_exact.clone();
        let local = warp::body::json().map(move |body: serde_json::Value| {
            let model = body["model"].as_str().expect("model");
            if model == "model-a" && fail_local_filter.load(Ordering::SeqCst) {
                return warp::reply::with_status(
                    warp::reply::json(&serde_json::json!({"error": {"message": "temporary failure"}})),
                    StatusCode::SERVICE_UNAVAILABLE,
                )
                .into_response();
            }
            warp::reply::json(&serde_json::json!({
                "source": format!("local:{model}"), "choices": []
            }))
            .into_response()
        });
        let (local_addr, local_server) = crate::bind_ephemeral!(local, ([127, 0, 0, 1], 0));
        let local_task = tokio::spawn(local_server);
        let challenger = warp::any().map(|| {
            warp::reply::json(&serde_json::json!({
                "source": "challenger", "choices": []
            }))
        });
        let (challenger_addr, challenger_server) =
            crate::bind_ephemeral!(challenger, ([127, 0, 0, 1], 0));
        let challenger_task = tokio::spawn(challenger_server);

        for prefer_local in [true, false] {
            platform_exact_available.store(false, Ordering::SeqCst);
            fail_local_exact.store(false, Ordering::SeqCst);
            let mut config = default_config();
            config.prefer_local_supply = prefer_local;
            config.account_device_api_key = "test-device-key".to_string();
            config.endpoints = vec![Endpoint {
                server_id: String::new(),
                name: "platform".to_string(),
                base_url: format!("http://{platform_addr}"),
                supplier_ws_url: String::new(),
                supplier_quic_url: String::new(),
                enabled: true,
            }];
            config.channels = [
                ("sticky-challenger", "model-b", challenger_addr),
                ("sticky-primary", "model-b", local_addr),
                ("sticky-exact", "model-a", local_addr),
            ]
            .into_iter()
            .map(|(id, model, addr)| {
                let mut channel = local_model_channel(&[model], &format!("http://{addr}/v1"));
                channel.id = id.to_string();
                channel.node_id = format!("node-{id}");
                channel
            })
            .collect();
            configure_model_compatibility_pair(&mut config, "model-a", "model-b");
            let config = normalize_config(config);
            let readiness = Arc::new(StdMutex::new(HashMap::from([(
                "sticky-primary".to_string(),
                true,
            )])));
            let shared = Arc::new(ProxyShared::with_platform_api_key(
                config.clone(),
                Client::new(),
                Arc::new(StdMutex::new("test-device-key".to_string())),
                Arc::new(StdMutex::new(config)),
                readiness.clone(),
            ));
            assert_eq!(
                forward_chat_json(&shared, "model-a", "same-session").await["source"],
                "local:model-b"
            );

            // A newly ready, earlier-listed compatible channel must not steal
            // the established cache route within the compatibility stage.
            readiness.lock().unwrap().insert("sticky-challenger".to_string(), true);
            assert_eq!(
                forward_chat_json(&shared, "model-a", "same-session").await["source"],
                "local:model-b"
            );

            platform_exact_available.store(true, Ordering::SeqCst);
            assert_eq!(
                forward_chat_json(&shared, "model-a", "same-session").await["source"],
                "platform-exact"
            );

            platform_exact_available.store(false, Ordering::SeqCst);
            readiness.lock().unwrap().insert("sticky-challenger".to_string(), false);
            assert_eq!(
                forward_chat_json(&shared, "model-a", "same-session").await["source"],
                "local:model-b"
            );

            // Readiness changes without editing the configuration must still
            // restore exact-model priority over an existing compatible route.
            readiness.lock().unwrap().insert("sticky-exact".to_string(), true);
            assert_eq!(
                forward_chat_json(&shared, "model-a", "same-session").await["source"],
                "local:model-a"
            );

            // Exact-first does not disable safe fallback when that route fails.
            fail_local_exact.store(true, Ordering::SeqCst);
            assert_eq!(
                forward_chat_json(&shared, "model-a", "same-session").await["source"],
                "local:model-b"
            );
        }
        platform_task.abort();
        local_task.abort();
        challenger_task.abort();
    }

    #[tokio::test]
    async fn online_exact_insufficient_balance_falls_back_to_local_compatibility() {
        crate::client_toasts::clear_client_toast_notices();
        let platform_modes = Arc::new(StdMutex::new(Vec::<String>::new()));
        let platform_modes_filter = platform_modes.clone();
        let platform = warp::method()
            .and(warp::path::full())
            .and(warp::header::optional::<String>(
                PLATFORM_MODEL_ROUTE_MODE_HEADER,
            ))
            .map(
                move |method: warp::http::Method,
                      path: warp::path::FullPath,
                      mode: Option<String>| {
                    if method != warp::http::Method::POST
                        || path.as_str() != "/v1/chat/completions"
                    {
                        return warp::reply::with_status("ok", StatusCode::OK).into_response();
                    }
                    let mode = mode.unwrap_or_default();
                    platform_modes_filter
                        .lock()
                        .expect("platform modes")
                        .push(mode.clone());
                    let response = warp::reply::with_status(
                        warp::reply::json(&serde_json::json!({
                            "error": {
                                "message": "insufficient_balance: balance must be positive before a paid request",
                                "type": "invalid_request_error"
                            }
                        })),
                        StatusCode::PAYMENT_REQUIRED,
                    );
                    let response = warp::reply::with_header(
                        response,
                        PLATFORM_MODEL_ROUTE_MODE_APPLIED_HEADER,
                        mode,
                    );
                    let response = warp::reply::with_header(
                        response,
                        "X-Const-Error-Code",
                        "insufficient_balance",
                    );
                    let response = warp::reply::with_header(
                        response,
                        "X-Const-Suggested-Action",
                        "open_funds_topup",
                    );
                    let response = warp::reply::with_header(
                        response,
                        "X-Const-Topup-Available",
                        "true",
                    );
                    warp::reply::with_header(response, "X-Request-Id", "request-balance-1")
                        .into_response()
                },
            );
        let (platform_addr, platform_server) =
            crate::bind_ephemeral!(platform, ([127, 0, 0, 1], 0));
        let platform_task = tokio::spawn(platform_server);

        let local_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let local_calls_filter = local_calls.clone();
        let local = warp::path::full().map(move |path: warp::path::FullPath| {
            if path.as_str() != "/healthz" {
                local_calls_filter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            warp::reply::json(&serde_json::json!({
                "id": "local-compatible",
                "source": "local-compatible",
                "choices": []
            }))
        });
        let (local_addr, local_server) =
            crate::bind_ephemeral!(local, ([127, 0, 0, 1], 0));
        let local_task = tokio::spawn(local_server);

        let mut config = default_config();
        config.account_device_api_key = "sk-platform-device".to_string();
        config.prefer_local_supply = true;
        config.endpoints = vec![Endpoint {
            server_id: String::new(),
            name: "platform".to_string(),
            base_url: format!("http://{platform_addr}"),
            supplier_ws_url: String::new(),
            supplier_quic_url: String::new(),
            enabled: true,
        }];
        config.channels = vec![local_model_channel(
            &["model-b"],
            &format!("http://{local_addr}/v1"),
        )];
        configure_model_compatibility_pair(&mut config, "model-a", "model-b");
        let shared = Arc::new(ProxyShared::from_config(
            normalize_config(config),
            Client::new(),
        ));

        let response = forward_with_failover(
            warp::http::Method::POST,
            "/v1/chat/completions",
            "",
            HeaderMap::new(),
            bytes::Bytes::from_static(br#"{"model":"model-a","messages":[]}"#),
            &shared,
        )
        .await
        .expect("local compatibility response");
        let body = crate::test_body_bytes(response.into_body())
            .await
            .expect("local compatibility body");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).expect("local json")["source"],
            "local-compatible"
        );
        assert_eq!(
            platform_modes.lock().expect("platform modes").as_slice(),
            ["exact-only"]
        );
        assert_eq!(
            local_calls.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert!(crate::client_toasts::drain_client_toast_notices_inner().is_empty());

        crate::client_toasts::clear_client_toast_notices();

        let final_balance_response = warp::reply::with_header(
            warp::reply::with_header(
                warp::reply::with_header(
                    warp::reply::with_header(
                        warp::reply::with_status("balance required", StatusCode::PAYMENT_REQUIRED),
                        "X-Const-Error-Code",
                        "insufficient_balance",
                    ),
                    "X-Const-Suggested-Action",
                    "open_funds_topup",
                ),
                "X-Const-Topup-Available",
                "true",
            ),
            "X-Request-Id",
            "request-balance-final",
        )
        .into_response();
        crate::client_toasts::record_platform_toast(&final_balance_response);
        let toast_notices = crate::client_toasts::drain_client_toast_notices_inner();
        assert_eq!(toast_notices.len(), 1);
        let matching = serde_json::to_value(&toast_notices[0]).expect("serialize notice");
        assert_eq!(matching["code"], "insufficient_balance");
        assert_eq!(matching["context"], "request_blocked");
        assert!(matching["dedupeKey"]
            .as_str()
            .is_some_and(|key| key.starts_with("insufficient_balance:")));

        platform_task.abort();
        local_task.abort();
    }

    #[tokio::test]
    async fn final_failure_follows_actual_fallback_order_for_every_model_routing_switch() {
        crate::client_toasts::clear_client_toast_notices();
        let call_order = Arc::new(StdMutex::new(Vec::<String>::new()));
        let platform_order = call_order.clone();
        let platform = warp::method()
            .and(warp::path::full())
            .and(warp::header::optional::<String>(
                PLATFORM_MODEL_ROUTE_MODE_HEADER,
            ))
            .map(
                move |method: warp::http::Method,
                      path: warp::path::FullPath,
                      mode: Option<String>| {
                    if method != warp::http::Method::POST
                        || path.as_str() != "/v1/chat/completions"
                    {
                        return warp::reply::with_status("ok", StatusCode::OK).into_response();
                    }
                    platform_order
                        .lock()
                        .expect("call order")
                        .push(format!("platform:{}", mode.as_deref().unwrap_or("configured")));
                    let response = warp::reply::with_status(
                        warp::reply::json(&serde_json::json!({
                            "error": {
                                "type": "invalid_request_error",
                                "message": "platform-balance-final"
                            }
                        })),
                        StatusCode::PAYMENT_REQUIRED,
                    );
                    let response = warp::reply::with_header(
                        response,
                        PLATFORM_ERROR_CODE_HEADER,
                        "insufficient_balance",
                    );
                    if let Some(mode) = mode {
                        return warp::reply::with_header(
                            response,
                            PLATFORM_MODEL_ROUTE_MODE_APPLIED_HEADER,
                            mode,
                        )
                        .into_response();
                    }
                    response.into_response()
                },
            );
        let (platform_addr, platform_server) =
            crate::bind_ephemeral!(platform, ([127, 0, 0, 1], 0));
        let platform_task = tokio::spawn(platform_server);

        let local_order = call_order.clone();
        let local_successes_remaining = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let local_successes_filter = local_successes_remaining.clone();
        let local = warp::method()
            .and(warp::path::full())
            .and(warp::body::bytes())
            .map(move |method: warp::http::Method,
                       path: warp::path::FullPath,
                       body: bytes::Bytes| {
                if method == warp::http::Method::POST
                    && path.as_str() == "/v1/chat/completions"
                {
                    let model = serde_json::from_slice::<serde_json::Value>(&body)
                        .ok()
                        .and_then(|value| {
                            value
                                .get("model")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_string)
                        })
                        .unwrap_or_default();
                    local_order
                        .lock()
                        .expect("call order")
                        .push(format!("local:{model}"));
                    if local_successes_filter.swap(0, std::sync::atomic::Ordering::SeqCst) > 0 {
                        return warp::reply::json(&serde_json::json!({
                            "id": "local-success",
                            "source": "local-compatible"
                        }))
                        .into_response();
                    }
                    return warp::reply::with_status(
                        warp::reply::json(&serde_json::json!({
                            "error": {
                                "type": "concurrency_full",
                                "message": "local-capacity-final"
                            }
                        })),
                        StatusCode::TOO_MANY_REQUESTS,
                    )
                    .into_response();
                }
                warp::reply::with_status("ok", StatusCode::OK).into_response()
            });
        let (local_addr, local_server) =
            crate::bind_ephemeral!(local, ([127, 0, 0, 1], 0));
        let local_task = tokio::spawn(local_server);

        for allow_equivalence in [false, true] {
            for prefer_local in [false, true] {
                call_order.lock().expect("call order").clear();
                let mut config = default_config();
                config.account_device_api_key = "sk-platform-device".to_string();
                config.prefer_local_supply = prefer_local;
                config.allow_model_equivalence = allow_equivalence;
                config.endpoints = vec![Endpoint {
                    server_id: String::new(),
                    name: "platform".to_string(),
                    base_url: format!("http://{platform_addr}"),
                    supplier_ws_url: String::new(),
                    supplier_quic_url: String::new(),
                    enabled: true,
                }];
                config.channels = vec![local_model_channel(
                    &["model-a"],
                    &format!("http://{local_addr}/v1"),
                )];
                if allow_equivalence {
                    configure_model_compatibility_pair(&mut config, "model-a", "model-b");
                }
                let shared = Arc::new(ProxyShared::from_config(
                    normalize_config(config),
                    Client::new(),
                ));

                let response = forward_with_failover(
                    warp::http::Method::POST,
                    "/v1/chat/completions",
                    "",
                    HeaderMap::new(),
                    bytes::Bytes::from_static(br#"{"model":"model-a","messages":[]}"#),
                    &shared,
                )
                .await
                .expect("final routed failure");
                let status = response.status();
                let body = crate::test_body_bytes(response.into_body())
                    .await
                    .expect("failure body");
                let body = String::from_utf8_lossy(&body);

                if prefer_local {
                    assert_eq!(status, StatusCode::PAYMENT_REQUIRED);
                    assert!(body.contains("platform-balance-final"));
                } else {
                    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
                    assert!(body.contains("local-capacity-final"));
                }
                let expected_platform = "platform:exact-only";
                let expected = if prefer_local {
                    vec!["local:model-a".to_string(), expected_platform.to_string()]
                } else {
                    vec![expected_platform.to_string(), "local:model-a".to_string()]
                };
                assert_eq!(*call_order.lock().expect("call order"), expected);
            }
        }

        for prefer_local in [false, true] {
            call_order.lock().expect("call order").clear();
            let mut config = default_config();
            config.account_device_api_key = "sk-platform-device".to_string();
            config.prefer_local_supply = prefer_local;
            config.endpoints = vec![Endpoint {
                server_id: String::new(),
                name: "platform".to_string(),
                base_url: format!("http://{platform_addr}"),
                supplier_ws_url: String::new(),
                supplier_quic_url: String::new(),
                enabled: true,
            }];
            config.channels.clear();
            configure_model_compatibility_pair(&mut config, "model-a", "model-b");
            let shared = Arc::new(ProxyShared::from_config(
                normalize_config(config),
                Client::new(),
            ));

            let response = forward_with_failover(
                warp::http::Method::POST,
                "/v1/chat/completions",
                "",
                HeaderMap::new(),
                bytes::Bytes::from_static(br#"{"model":"model-a","messages":[]}"#),
                &shared,
            )
            .await
            .expect("platform balance without a local route");
            assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);
            let body = crate::test_body_bytes(response.into_body())
                .await
                .expect("platform failure body");
            assert!(String::from_utf8_lossy(&body).contains("platform-balance-final"));
            assert_eq!(
                *call_order.lock().expect("call order"),
                vec!["platform:exact-only".to_string()]
            );
        }

        call_order.lock().expect("call order").clear();
        local_successes_remaining.store(1, std::sync::atomic::Ordering::SeqCst);
        let mut config = default_config();
        config.account_device_api_key = "sk-platform-device".to_string();
        config.prefer_local_supply = false;
        config.endpoints = vec![Endpoint {
            server_id: String::new(),
            name: "platform".to_string(),
            base_url: format!("http://{platform_addr}"),
            supplier_ws_url: String::new(),
            supplier_quic_url: String::new(),
            enabled: true,
        }];
        config.channels = vec![local_model_channel(
            &["model-b"],
            &format!("http://{local_addr}/v1"),
        )];
        configure_model_compatibility_pair(&mut config, "model-a", "model-b");
        let shared = Arc::new(ProxyShared::from_config(
            normalize_config(config),
            Client::new(),
        ));
        let sticky_body = bytes::Bytes::from_static(
            br#"{"model":"model-a","prompt_cache_key":"sticky-route","messages":[]}"#,
        );

        let response = forward_with_failover(
            warp::http::Method::POST,
            "/v1/chat/completions",
            "",
            HeaderMap::new(),
            sticky_body.clone(),
            &shared,
        )
        .await
        .expect("initial compatible local success");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            *call_order.lock().expect("call order"),
            vec![
                "platform:exact-only".to_string(),
                "local:model-b".to_string()
            ]
        );

        call_order.lock().expect("call order").clear();
        let response = forward_with_failover(
            warp::http::Method::POST,
            "/v1/chat/completions",
            "",
            HeaderMap::new(),
            sticky_body,
            &shared,
        )
        .await
        .expect("sticky local capacity failure");
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let body = crate::test_body_bytes(response.into_body())
            .await
            .expect("sticky local failure body");
        assert!(String::from_utf8_lossy(&body).contains("local-capacity-final"));
        assert_eq!(
            *call_order.lock().expect("call order"),
            vec![
                "platform:exact-only".to_string(),
                "local:model-b".to_string()
            ]
        );

        platform_task.abort();
        local_task.abort();
        crate::client_toasts::clear_client_toast_notices();
    }

    #[tokio::test]
    async fn local_concurrency_full_skips_the_same_account_and_preserves_platform_fallback_budget() {
        let platform_modes = Arc::new(StdMutex::new(Vec::<String>::new()));
        let platform_modes_filter = platform_modes.clone();
        let platform = warp::method()
            .and(warp::path::full())
            .and(warp::header::optional::<String>(
                PLATFORM_MODEL_ROUTE_MODE_HEADER,
            ))
            .map(move |method: warp::http::Method,
                       path: warp::path::FullPath,
                       mode: Option<String>| {
                if method != warp::http::Method::POST || path.as_str() != "/v1/responses" {
                    return warp::reply::with_status("ok", StatusCode::OK).into_response();
                }
                let mode = mode.unwrap_or_default();
                platform_modes_filter
                    .lock()
                    .expect("platform modes")
                    .push(mode.clone());
                if mode == "compatible-only" {
                    return warp::reply::json(&serde_json::json!({
                        "id": "platform-compatible",
                        "source": "platform-compatible"
                    }))
                    .into_response();
                }
                let response = warp::reply::with_status(
                    warp::reply::json(&serde_json::json!({
                        "error": {"message": "no exact platform route"}
                    })),
                    StatusCode::SERVICE_UNAVAILABLE,
                );
                let response = warp::reply::with_header(
                    response,
                    PLATFORM_MODEL_ROUTE_MODE_APPLIED_HEADER,
                    mode,
                );
                let response = warp::reply::with_header(
                    response,
                    PLATFORM_MODEL_FALLBACK_SAFE_HEADER,
                    "1",
                );
                warp::reply::with_header(response, PLATFORM_UPSTREAM_ATTEMPTS_USED_HEADER, "1")
                    .into_response()
            });
        let (platform_addr, platform_server) =
            crate::bind_ephemeral!(platform, ([127, 0, 0, 1], 0));
        let platform_task = tokio::spawn(platform_server);

        let request_body =
            br#"{"model":"model-a","input":"hello","store":false}"#;
        let mut channels = Vec::new();
        let mut held_capacity = Vec::new();
        for index in 0..4 {
            let mut channel = openai_subscription_channel(&["model-a", "model-b"]);
            channel.id = format!("safety-proxy-capacity-fallback-{index}");
            channel.node_id = format!("node-{}", channel.id);
            channel.v2.max_concurrency = 1;
            channel.subscription.rpm_limit = 0;
            channel.v2.quota_reserve_percent = 0;
            held_capacity.push(subscription_safety_enter(
                &supplier_from_channel(&channel),
                std::str::from_utf8(request_body).expect("request body"),
            ).expect("reserve each local subscription slot"));
            channels.push(channel);
        }

        let mut config = default_config();
        config.account_device_api_key = "sk-platform-device".to_string();
        config.prefer_local_supply = true;
        config.endpoints = vec![Endpoint {
            server_id: String::new(),
            name: "platform".to_string(),
            base_url: format!("http://{platform_addr}"),
            supplier_ws_url: String::new(),
            supplier_quic_url: String::new(),
            enabled: true,
        }];
        config.channels = channels;
        configure_model_compatibility_pair(&mut config, "model-a", "model-b");
        let shared = Arc::new(ProxyShared::from_config(
            normalize_config(config),
            Client::new(),
        ));

        let response = forward_with_failover(
            warp::http::Method::POST,
            "/v1/responses",
            "",
            HeaderMap::new(),
            bytes::Bytes::from_static(request_body),
            &shared,
        )
        .await
        .expect("platform compatibility fallback");
        assert_eq!(response.status(), StatusCode::OK);
        let body = crate::test_body_bytes(response.into_body())
            .await
            .expect("platform response body");
        let body: serde_json::Value =
            serde_json::from_slice(&body).expect("platform response JSON");
        assert_eq!(body["source"], "platform-compatible");
        assert_eq!(
            platform_modes.lock().expect("platform modes").as_slice(),
            ["exact-only", "compatible-only"]
        );

        platform_task.abort();
    }

    #[tokio::test]
    async fn local_api_capacity_rejections_reach_the_fourth_channel_without_harming_health() {
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let received = requests.clone();
        let upstream = warp::any().map(move || {
            received.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            warp::reply::json(&serde_json::json!({"source": "fourth-channel"}))
        });
        let (address, server) = crate::bind_ephemeral!(upstream, ([127, 0, 0, 1], 0));
        let task = tokio::spawn(server);
        let mut config = default_config();
        config.account_device_api_key.clear();
        config.endpoints.clear();
        config.prefer_local_supply = true;
        config.channels.clear();
        let mut held = Vec::new();
        for index in 0..4 {
            let mut channel = local_model_channel(&["model-a"], &format!("http://{address}/v1"));
            channel.id = format!("api-capacity-fallback-{index}");
            channel.v2.max_concurrency = 1;
            if index < 3 {
                held.push(crate::channel_concurrency::Guard::acquire(&channel.id, 1).unwrap());
            }
            config.channels.push(channel);
        }
        let shared = Arc::new(ProxyShared::from_config(normalize_config(config), Client::new()));
        let response = forward_with_failover(
            warp::http::Method::POST, "/v1/chat/completions", "", HeaderMap::new(),
            bytes::Bytes::from_static(br#"{"model":"model-a","messages":[]}"#), &shared,
        ).await.expect("capacity fallback");
        assert_eq!(response.status(), StatusCode::OK);
        let body = crate::test_body_bytes(response.into_body()).await.unwrap();
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&body).unwrap()["source"], "fourth-channel");
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 1);
        let ready = shared.ready_local_channel_ids();
        for index in 0..3 {
            assert!(ready.contains(&format!("api-capacity-fallback-{index}")), "busy is not unhealthy");
        }
        assert_eq!(crate::channel_concurrency::current("api-capacity-fallback-3"), 0);
        let _last_slot = crate::channel_concurrency::Guard::acquire("api-capacity-fallback-3", 1).unwrap();
        let error = tokio::time::timeout(std::time::Duration::from_secs(2), forward_with_failover(
            warp::http::Method::POST, "/v1/chat/completions", "", HeaderMap::new(),
            bytes::Bytes::from_static(br#"{"model":"model-a","messages":[]}"#), &shared,
        )).await.expect("an all-full pool must terminate").unwrap_err();
        assert_eq!(proxy_execution_error_response(&error).status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 1, "all-full pool must not dispatch");
        task.abort();
    }

    #[test]
    fn local_capacity_budget_exemption_requires_pre_dispatch_proof() {
        let local = crate::supplier::subscription_structured_error_response(
            429, "concurrency_full", "local capacity full", 1, 0,
        );
        for upstream_attempted in [None, Some(false), Some(true)] {
            let mut payload = local.clone();
            if let Some(attempted) = upstream_attempted {
                payload.get_mut(crate::supplier::EXECUTION_EVIDENCE_FIELD).unwrap()["upstream_attempted"] = attempted.into();
            } else {
                payload.remove(crate::supplier::EXECUTION_EVIDENCE_FIELD);
            }
            let mut response = error_response(StatusCode::TOO_MANY_REQUESTS, "full");
            attach_local_model_failure_evidence(&mut response, &payload);
            assert_eq!(response.extensions().get::<LocalAdmissionFailure>(), Some(&LocalAdmissionFailure::ConcurrencyFull {
                before_dispatch: upstream_attempted == Some(false),
            }));
        }
        let error = crate::supplier::channel_concurrency_failure();
        assert_eq!(local_execution_error_retry_scope(&error, false, "/v1/responses"), LocalRouteRetryScope::AdmissionCapacity);
        assert_eq!(local_execution_error_retry_scope(&error, true, "/v1/responses"), LocalRouteRetryScope::None);
    }

    #[tokio::test]
    async fn cyber_policy_falls_back_locally_without_reentering_platform() {
        let platform_modes = Arc::new(StdMutex::new(Vec::<String>::new()));
        let platform_modes_filter = platform_modes.clone();
        let platform = warp::method()
            .and(warp::path::full())
            .and(warp::header::optional::<String>(
                PLATFORM_MODEL_ROUTE_MODE_HEADER,
            ))
            .map(
                move |method: warp::http::Method,
                      path: warp::path::FullPath,
                      mode: Option<String>| {
                    if method != warp::http::Method::POST
                        || path.as_str() != "/v1/chat/completions"
                    {
                        return warp::reply::with_status("ok", StatusCode::OK).into_response();
                    }
                    let mode = mode.unwrap_or_default();
                    platform_modes_filter
                        .lock()
                        .expect("platform modes")
                        .push(mode.clone());
                    let response = warp::reply::with_status(
                        warp::reply::json(&serde_json::json!({
                            "error": {"code": "cyber_policy", "message": "blocked"}
                        })),
                        StatusCode::BAD_GATEWAY,
                    );
                    let response = warp::reply::with_header(
                        response,
                        PLATFORM_MODEL_ROUTE_MODE_APPLIED_HEADER,
                        mode,
                    );
                    let response = warp::reply::with_header(
                        response,
                        PLATFORM_MODEL_FALLBACK_SCOPE_HEADER,
                        PLATFORM_MODEL_FALLBACK_SCOPE_LOCAL_ONLY,
                    );
                    warp::reply::with_header(response, PLATFORM_ERROR_CODE_HEADER, "cyber_policy")
                        .into_response()
                },
            );
        let (platform_addr, platform_server) =
            crate::bind_ephemeral!(platform, ([127, 0, 0, 1], 0));
        let platform_task = tokio::spawn(platform_server);

        let local_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let local_calls_filter = local_calls.clone();
        let local = warp::path::full().map(move |path: warp::path::FullPath| {
            if path.as_str() != "/healthz" {
                local_calls_filter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            warp::reply::json(&serde_json::json!({
                "id": "local-policy-fallback",
                "source": "local-policy-fallback",
                "choices": []
            }))
        });
        let (local_addr, local_server) =
            crate::bind_ephemeral!(local, ([127, 0, 0, 1], 0));
        let local_task = tokio::spawn(local_server);

        let mut config = default_config();
        config.account_device_api_key = "sk-platform-device".to_string();
        config.prefer_local_supply = false;
        config.endpoints = vec![
            Endpoint {
                server_id: String::new(),
                name: "platform-a".to_string(),
                base_url: format!("http://{platform_addr}"),
                supplier_ws_url: String::new(),
                supplier_quic_url: String::new(),
                enabled: true,
            },
            Endpoint {
                server_id: String::new(),
                name: "platform-b".to_string(),
                base_url: format!("http://{platform_addr}"),
                supplier_ws_url: String::new(),
                supplier_quic_url: String::new(),
                enabled: true,
            },
        ];
        config.channels = vec![local_model_channel(
            &["model-b"],
            &format!("http://{local_addr}/v1"),
        )];
        configure_model_compatibility_pair(&mut config, "model-a", "model-b");
        let shared = Arc::new(ProxyShared::from_config(
            normalize_config(config.clone()),
            Client::new(),
        ));

        let response = forward_with_failover(
            warp::http::Method::POST,
            "/v1/chat/completions",
            "",
            HeaderMap::new(),
            bytes::Bytes::from_static(br#"{"model":"model-a","messages":[]}"#),
            &shared,
        )
        .await
        .expect("local-only fallback response");
        let body = crate::test_body_bytes(response.into_body())
            .await
            .expect("local-only fallback body");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).expect("local JSON")["source"],
            "local-policy-fallback"
        );
        assert_eq!(
            platform_modes.lock().expect("platform modes").as_slice(),
            ["exact-only"]
        );
        assert_eq!(local_calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        config.channels.clear();
        let no_local = Arc::new(ProxyShared::from_config(normalize_config(config), Client::new()));
        let response = forward_with_failover(
            warp::http::Method::POST,
            "/v1/chat/completions",
            "",
            HeaderMap::new(),
            bytes::Bytes::from_static(br#"{"model":"model-a","messages":[]}"#),
            &no_local,
        )
        .await
        .expect("original policy response");
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = crate::test_body_bytes(response.into_body())
            .await
            .expect("policy response body");
        assert!(String::from_utf8_lossy(&body).contains("cyber_policy"));
        assert_eq!(
            platform_modes.lock().expect("platform modes").as_slice(),
            ["exact-only", "exact-only"]
        );

        platform_task.abort();
        local_task.abort();
    }

    #[tokio::test]
    async fn claude_never_receives_intermediate_balance_error_when_local_compatibility_succeeds() {
        crate::client_toasts::clear_client_toast_notices();
        let platform_modes = Arc::new(StdMutex::new(Vec::<String>::new()));
        let platform_modes_filter = platform_modes.clone();
        let platform = warp::method()
            .and(warp::path::full())
            .and(warp::header::optional::<String>(
                PLATFORM_MODEL_ROUTE_MODE_HEADER,
            ))
            .map(
                move |method: warp::http::Method,
                      path: warp::path::FullPath,
                      mode: Option<String>| {
                    if method == warp::http::Method::POST
                        && path.as_str() == "/anthropic/v1/messages"
                    {
                        let mode = mode.unwrap_or_default();
                        platform_modes_filter
                            .lock()
                            .expect("platform modes")
                            .push(mode.clone());
                        let response = warp::reply::with_header(
                            warp::reply::with_status(
                                warp::reply::json(&serde_json::json!({
                                    "error": {
                                        "type": "invalid_request_error",
                                        "message": "insufficient_balance"
                                    }
                                })),
                                StatusCode::PAYMENT_REQUIRED,
                            ),
                            PLATFORM_MODEL_ROUTE_MODE_APPLIED_HEADER,
                            mode,
                        );
                        return warp::reply::with_header(
                            response,
                            "X-Const-Error-Code",
                            "insufficient_balance",
                        )
                        .into_response();
                    }
                    warp::reply::with_status("not found", StatusCode::NOT_FOUND).into_response()
                },
            );
        let (platform_addr, platform_server) =
            crate::bind_ephemeral!(platform, ([127, 0, 0, 1], 0));
        let platform_task = tokio::spawn(platform_server);

        let local_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let local_calls_filter = local_calls.clone();
        let local = warp::path::full().map(move |path: warp::path::FullPath| {
            if path.as_str() == "/v1/chat/completions" {
                local_calls_filter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return warp::reply::json(&serde_json::json!({
                    "id": "chatcmpl-local",
                    "model": "gpt-local",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "local answer"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 8, "completion_tokens": 2, "total_tokens": 10}
                }))
                .into_response();
            }
            warp::reply::with_status("not found", StatusCode::NOT_FOUND).into_response()
        });
        let (local_addr, local_server) =
            crate::bind_ephemeral!(local, ([127, 0, 0, 1], 0));
        let local_task = tokio::spawn(local_server);

        for prefer_local in [false, true] {
            let mut config = default_config();
            config.account_device_api_key = "sk-platform-device".to_string();
            config.prefer_local_supply = prefer_local;
            config.endpoints = vec![Endpoint {
                server_id: String::new(),
                name: "platform".to_string(),
                base_url: format!("http://{platform_addr}"),
                supplier_ws_url: String::new(),
                supplier_quic_url: String::new(),
                enabled: true,
            }];
            config.channels = vec![local_model_channel(
                &["gpt-local"],
                &format!("http://{local_addr}/v1"),
            )];
            configure_model_compatibility_pair(&mut config, "claude-opus-5", "gpt-local");
            let shared = Arc::new(ProxyShared::from_config(
                normalize_config(config),
                Client::new(),
            ));

            let response = forward_with_failover(
                warp::http::Method::POST,
                "/anthropic/v1/messages",
                "beta=true",
                HeaderMap::new(),
                bytes::Bytes::from_static(
                    br#"{"model":"claude-opus-5","max_tokens":64,"messages":[{"role":"user","content":"hello"}]}"#,
                ),
                &shared,
            )
            .await
            .expect("Claude local compatibility response");
            assert_eq!(response.status(), StatusCode::OK);
            let body = crate::test_body_bytes(response.into_body())
                .await
                .expect("Claude response body");
            let body: serde_json::Value = serde_json::from_slice(&body).expect("Claude JSON");
            assert_eq!(body["type"], "message");
            assert_eq!(body["content"][0]["text"], "local answer");
            assert!(crate::client_toasts::drain_client_toast_notices_inner().is_empty());
        }

        assert_eq!(local_calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(
            platform_modes.lock().expect("platform modes").as_slice(),
            ["exact-only", "exact-only"]
        );

        platform_task.abort();
        local_task.abort();
    }

    #[tokio::test]
    async fn explicit_unsafe_platform_failure_is_not_replayed_on_another_route() {
        let first_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let first_calls_filter = first_calls.clone();
        let first_platform = warp::method().and(warp::path::full()).map(
            move |method: warp::http::Method, path: warp::path::FullPath| {
                if method == warp::http::Method::POST
                    && path.as_str() == "/v1/chat/completions"
                {
                    first_calls_filter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                warp::reply::with_header(
                    warp::reply::with_status(
                        "upstream outcome is ambiguous",
                        StatusCode::SERVICE_UNAVAILABLE,
                    ),
                    PLATFORM_MODEL_ROUTE_MODE_APPLIED_HEADER,
                    "exact-only",
                )
            },
        );
        let (first_addr, first_server) =
            crate::bind_ephemeral!(first_platform, ([127, 0, 0, 1], 0));
        let first_task = tokio::spawn(first_server);

        let second_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let second_calls_filter = second_calls.clone();
        let second_platform = warp::method().and(warp::path::full()).map(
            move |method: warp::http::Method, path: warp::path::FullPath| {
                if method == warp::http::Method::POST
                    && path.as_str() == "/v1/chat/completions"
                {
                    second_calls_filter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                warp::reply::json(&serde_json::json!({"source": "second-platform"}))
            },
        );
        let (second_addr, second_server) =
            crate::bind_ephemeral!(second_platform, ([127, 0, 0, 1], 0));
        let second_task = tokio::spawn(second_server);

        let local_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let local_calls_filter = local_calls.clone();
        let local = warp::method().and(warp::path::full()).map(
            move |method: warp::http::Method, path: warp::path::FullPath| {
                if method == warp::http::Method::POST
                    && path.as_str() == "/v1/chat/completions"
                {
                    local_calls_filter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                warp::reply::json(&serde_json::json!({"source": "local-compatible"}))
            },
        );
        let (local_addr, local_server) =
            crate::bind_ephemeral!(local, ([127, 0, 0, 1], 0));
        let local_task = tokio::spawn(local_server);

        let mut config = default_config();
        config.account_device_api_key = "sk-platform-device".to_string();
        config.prefer_local_supply = true;
        config.endpoints = vec![
            Endpoint {
                server_id: String::new(),
                name: "first-platform".to_string(),
                base_url: format!("http://{first_addr}"),
                supplier_ws_url: String::new(),
                supplier_quic_url: String::new(),
                enabled: true,
            },
            Endpoint {
                server_id: String::new(),
                name: "second-platform".to_string(),
                base_url: format!("http://{second_addr}"),
                supplier_ws_url: String::new(),
                supplier_quic_url: String::new(),
                enabled: true,
            },
        ];
        config.channels = vec![local_model_channel(
            &["model-b"],
            &format!("http://{local_addr}/v1"),
        )];
        configure_model_compatibility_pair(&mut config, "model-a", "model-b");
        let shared = Arc::new(ProxyShared::from_config(
            normalize_config(config),
            Client::new(),
        ));

        let response = forward_with_failover(
            warp::http::Method::POST,
            "/v1/chat/completions",
            "",
            HeaderMap::new(),
            bytes::Bytes::from_static(br#"{"model":"model-a","messages":[]}"#),
            &shared,
        )
        .await
        .expect("explicit unsafe platform response");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(first_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(second_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(local_calls.load(std::sync::atomic::Ordering::SeqCst), 0);

        first_task.abort();
        second_task.abort();
        local_task.abort();
    }

    #[tokio::test]
    async fn local_preference_routes_exact_scopes_before_compatibility_scopes() {
        let sequence = Arc::new(StdMutex::new(Vec::<String>::new()));
        let platform_sequence = sequence.clone();
        let platform = warp::method()
            .and(warp::path::full())
            .and(warp::header::optional::<String>(
                PLATFORM_MODEL_ROUTE_MODE_HEADER,
            ))
            .and(warp::header::optional::<String>(
                "x-const-api-allow-model-equivalence",
            ))
            .and(warp::header::optional::<String>(
                PLATFORM_UPSTREAM_ATTEMPT_BUDGET_HEADER,
            ))
            .map(
                move |method: warp::http::Method,
                      path: warp::path::FullPath,
                      mode: Option<String>,
                      equivalence: Option<String>,
                      budget: Option<String>| {
                    if method != warp::http::Method::POST
                        || path.as_str() != "/v1/chat/completions"
                    {
                        return warp::reply::with_status("ok", StatusCode::OK).into_response();
                    }
                    let mode = mode.unwrap_or_default();
                    platform_sequence
                        .lock()
                        .expect("route sequence")
                        .push(format!(
                            "platform:{mode}:{}:budget={}",
                            equivalence.as_deref().unwrap_or("off"),
                            budget.as_deref().unwrap_or("missing")
                        ));
                    if mode == "exact-only" {
                        let response = warp::reply::with_status(
                            warp::reply::json(&serde_json::json!({
                                "error": {"message": "no exact platform route"}
                            })),
                            StatusCode::SERVICE_UNAVAILABLE,
                        );
                        let response = warp::reply::with_header(
                            response,
                            PLATFORM_MODEL_ROUTE_MODE_APPLIED_HEADER,
                            "exact-only",
                        );
                        let response = warp::reply::with_header(
                            response,
                            PLATFORM_UPSTREAM_ATTEMPTS_USED_HEADER,
                            "0",
                        );
                        return warp::reply::with_header(
                            response,
                            PLATFORM_MODEL_FALLBACK_SAFE_HEADER,
                            "1",
                        )
                        .into_response();
                    }
                    warp::reply::with_header(
                        warp::reply::json(&serde_json::json!({
                            "id": "platform-compatible",
                            "source": "platform-compatible",
                            "choices": []
                        })),
                        PLATFORM_MODEL_ROUTE_MODE_APPLIED_HEADER,
                        "compatible-only",
                    )
                    .into_response()
                },
            );
        let (platform_addr, platform_server) =
            crate::bind_ephemeral!(platform, ([127, 0, 0, 1], 0));
        let platform_task = tokio::spawn(platform_server);

        let exact_sequence = sequence.clone();
        let exact_local = warp::path::full().map(move |path: warp::path::FullPath| {
            if path.as_str() != "/healthz" {
                exact_sequence
                    .lock()
                    .expect("route sequence")
                    .push("local:exact".to_string());
            }
            warp::reply::with_status("exact local unavailable", StatusCode::SERVICE_UNAVAILABLE)
        });
        let (exact_addr, exact_server) =
            crate::bind_ephemeral!(exact_local, ([127, 0, 0, 1], 0));
        let exact_task = tokio::spawn(exact_server);

        let compatible_sequence = sequence.clone();
        let compatible_local = warp::path::full().map(move |path: warp::path::FullPath| {
            if path.as_str() != "/healthz" {
                compatible_sequence
                    .lock()
                    .expect("route sequence")
                    .push("local:compatible".to_string());
            }
            warp::reply::with_status(
                "compatible local unavailable",
                StatusCode::SERVICE_UNAVAILABLE,
            )
        });
        let (compatible_addr, compatible_server) =
            crate::bind_ephemeral!(compatible_local, ([127, 0, 0, 1], 0));
        let compatible_task = tokio::spawn(compatible_server);

        let mut exact_channel = local_model_channel(
            &["model-a"],
            &format!("http://{exact_addr}/v1"),
        );
        exact_channel.id = "local-exact".to_string();
        exact_channel.node_id = "node-local-exact".to_string();
        let mut compatible_channel = local_model_channel(
            &["model-b"],
            &format!("http://{compatible_addr}/v1"),
        );
        compatible_channel.id = "local-compatible".to_string();
        compatible_channel.node_id = "node-local-compatible".to_string();

        let mut config = default_config();
        config.account_device_api_key = "sk-platform-device".to_string();
        config.prefer_local_supply = true;
        config.endpoints = vec![Endpoint {
            server_id: String::new(),
            name: "platform".to_string(),
            base_url: format!("http://{platform_addr}"),
            supplier_ws_url: String::new(),
            supplier_quic_url: String::new(),
            enabled: true,
        }];
        config.channels = vec![exact_channel, compatible_channel];
        configure_model_compatibility_pair(&mut config, "model-a", "model-b");
        let shared = Arc::new(ProxyShared::from_config(
            normalize_config(config),
            Client::new(),
        ));

        let response = forward_with_failover(
            warp::http::Method::POST,
            "/v1/chat/completions",
            "",
            HeaderMap::new(),
            bytes::Bytes::from_static(br#"{"model":"model-a","messages":[]}"#),
            &shared,
        )
        .await
        .expect("compatible platform response");
        let body = crate::test_body_bytes(response.into_body())
            .await
            .expect("compatible platform body");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).expect("platform json")["source"],
            "platform-compatible"
        );
        assert_eq!(
            sequence.lock().expect("route sequence").as_slice(),
            [
                "local:exact",
                "platform:exact-only:off:budget=2",
                "local:compatible",
                "platform:compatible-only:1:budget=1",
            ]
        );

        platform_task.abort();
        exact_task.abort();
        compatible_task.abort();
    }

    #[tokio::test]
    async fn local_provider_failure_degrades_only_failed_model_and_keeps_successful_route() {
        let sequence = Arc::new(StdMutex::new(Vec::<String>::new()));
        let sequence_filter = sequence.clone();
        let upstream = warp::path::full()
            .and(warp::body::bytes())
            .map(move |path: warp::path::FullPath, body: bytes::Bytes| {
                if path.as_str() == "/healthz" {
                    return warp::reply::with_status("ok", StatusCode::OK).into_response();
                }
                let model = serde_json::from_slice::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|value| value.get("model").and_then(|model| model.as_str()).map(str::to_string))
                    .unwrap_or_default();
                sequence_filter
                    .lock()
                    .expect("model sequence")
                    .push(model.clone());
                if model == "model-b" {
                    return warp::reply::with_status(
                        warp::reply::json(&serde_json::json!({
                            "error": {
                                "code": 503,
                                "message": "Service temporarily unavailable",
                                "status": "UNAVAILABLE"
                            }
                        })),
                        StatusCode::SERVICE_UNAVAILABLE,
                    )
                    .into_response();
                }
                warp::reply::json(&serde_json::json!({
                    "id": "fallback-success",
                    "source": model,
                    "choices": []
                }))
                .into_response()
            });
        let (upstream_addr, upstream_server) =
            crate::bind_ephemeral!(upstream, ([127, 0, 0, 1], 0));
        let upstream_task = tokio::spawn(upstream_server);

        let mut config = default_config();
        config.account_device_api_key.clear();
        config.prefer_local_supply = true;
        config.endpoints.clear();
        config.channels = vec![local_model_channel(
            &["model-b", "model-c"],
            &format!("http://{upstream_addr}/v1"),
        )];
        configure_model_compatibility_pair(&mut config, "model-a", "model-b");
        let profile_key = model_compatibility_profile_key("platform-1", "user-1")
            .expect("compatibility profile key");
        let group = &mut config
            .model_compatibility_profiles
            .get_mut(&profile_key)
            .expect("compatibility profile")
            .model_groups[0];
        group.models.push("model-c".to_string());
        group.match_models.push("model-c".to_string());
        let config = normalize_config(config);
        assert_eq!(
            crate::model_compatibility::model_compatibility_candidates(&config, "model-a"),
            ["model-b", "model-c"]
        );
        let shared = Arc::new(ProxyShared::from_config(config, Client::new()));
        let request_body = bytes::Bytes::from_static(
            br#"{"model":"model-a","prompt_cache_key":"same-session","messages":[]}"#,
        );

        for _ in 0..2 {
            let response = forward_with_failover(
                warp::http::Method::POST,
                "/v1/chat/completions",
                "",
                HeaderMap::new(),
                request_body.clone(),
                &shared,
            )
            .await
            .expect("local compatible response");
            assert_eq!(response.status(), StatusCode::OK);
        }

        assert_eq!(
            sequence.lock().expect("model sequence").as_slice(),
            ["model-b", "model-c", "model-c"]
        );
        let failed_key = crate::supplier::model_quota_route_key("local-models", "model-b");
        {
            let routes = shared.local_model_quota_routes.lock().expect("model health");
            assert_eq!(routes[&failed_key].runtime_failure_count, 1);
            let successful_key = crate::supplier::model_quota_route_key("local-models", "model-c");
            assert!(!routes.get(&successful_key).is_some_and(|route| route.has_runtime_failure()),
                "one model's 503 must not degrade another model on the same channel");
        }
        assert_eq!(
            shared
                .local_channel_readiness
                .lock()
                .expect("channel readiness")
                .get("local-models"),
            Some(&true)
        );

        upstream_task.abort();
    }

    #[tokio::test]
    async fn local_model_failover_never_exceeds_three_upstream_dispatches() {
        let sequence = Arc::new(StdMutex::new(Vec::<String>::new()));
        let sequence_filter = sequence.clone();
        let upstream = warp::body::bytes().map(move |body: bytes::Bytes| {
            let model = serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|value| value.get("model").and_then(|model| model.as_str()).map(str::to_string))
                .unwrap_or_default();
            sequence_filter
                .lock()
                .expect("model sequence")
                .push(model.clone());
            warp::reply::with_status(
                warp::reply::json(&serde_json::json!({
                    "error": {
                        "code": 503,
                        "message": format!("No capacity available for model {model} on the server"),
                        "details": [{
                            "reason": "MODEL_CAPACITY_EXHAUSTED",
                            "metadata": {"model": model}
                        }]
                    }
                })),
                StatusCode::SERVICE_UNAVAILABLE,
            )
        });
        let (upstream_addr, upstream_server) =
            crate::bind_ephemeral!(upstream, ([127, 0, 0, 1], 0));
        let upstream_task = tokio::spawn(upstream_server);

        let models = ["model-b", "model-c", "model-d", "model-e"];
        let mut config = default_config();
        config.account_device_api_key.clear();
        config.prefer_local_supply = true;
        config.endpoints.clear();
        config.channels = vec![local_model_channel(
            &models,
            &format!("http://{upstream_addr}/v1"),
        )];
        configure_model_compatibility_pair(&mut config, "model-a", "model-b");
        let profile_key = model_compatibility_profile_key("platform-1", "user-1")
            .expect("compatibility profile key");
        let group = &mut config
            .model_compatibility_profiles
            .get_mut(&profile_key)
            .expect("compatibility profile")
            .model_groups[0];
        group.models = models.iter().map(|model| (*model).to_string()).collect();
        group.match_models = std::iter::once("model-a".to_string())
            .chain(models.iter().map(|model| (*model).to_string()))
            .collect();
        let shared = Arc::new(ProxyShared::from_config(
            normalize_config(config),
            Client::new(),
        ));

        let response = forward_with_failover(
            warp::http::Method::POST,
            "/v1/chat/completions",
            "",
            HeaderMap::new(),
            bytes::Bytes::from_static(br#"{"model":"model-a","messages":[]}"#),
            &shared,
        )
        .await
        .expect("final provider failure");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            sequence.lock().expect("model sequence").as_slice(),
            ["model-b", "model-c", "model-d"]
        );
        let health = shared
            .local_model_quota_routes
            .lock()
            .expect("model health");
        for model in ["model-b", "model-c", "model-d"] {
            let key = crate::supplier::model_quota_route_key("local-models", model);
            let state = health
                .get(&key)
                .copied()
                .expect("structured model failure health");
            assert!(state.runtime_failure_count > 0);
            assert!(!state.available_at(now_unix()));
        }
        assert!(
            !health.contains_key(&crate::supplier::model_quota_route_key(
                "local-models",
                "model-e"
            )),
            "the untried model must remain healthy"
        );

        upstream_task.abort();
    }

    #[tokio::test]
    async fn local_and_platform_model_metadata_are_merged_without_changing_local_identity() {
        let local = serde_json::json!({
            "id": "Shared-Model",
            "object": "model",
            "owned_by": "local",
            "const_api": {
                "source": "local_supplier",
                "supported_protocols": ["openai_responses"],
                "reasoning": false
            }
        });
        let platform = serde_json::json!({
            "id": "shared-model",
            "object": "model",
            "owned_by": "platform",
            "const_api": {
                "supported_protocols": ["openai_chat"],
                "reasoning": true,
                "context_tokens": 123456,
                "effective_capabilities": [{"feature":"vision","state":"supported"}]
            }
        });
        let config = default_config();
        let response =
            local_models_response(&config, crate::surface::ApiSurface::OpenAi, &[platform], false)
                .expect("platform response");
        let response = merge_local_models_response(
            response,
            &config,
            crate::surface::ApiSurface::OpenAi,
            &[local],
            true,
            false,
        )
        .await
        .expect("merged response");
        let body = crate::test_body_bytes(response.into_body())
            .await
            .expect("merged body");
        let payload: serde_json::Value = serde_json::from_slice(&body).expect("merged JSON");
        let models = payload["data"].as_array().expect("models");

        assert_eq!(models.len(), 1);
        assert_eq!(models[0]["id"], "shared-model");
        assert_eq!(models[0]["owned_by"], "local");
        assert_eq!(models[0]["const_api"]["reasoning"], true);
        assert!(models[0]["const_api"]["context_tokens"].is_null());
        assert_eq!(models[0]["const_api"]["supports_1m"], false);
        assert_eq!(
            models[0]["const_api"]["supported_protocols"],
            serde_json::json!(["openai_responses", "openai_chat"])
        );
        assert_eq!(
            models[0]["const_api"]["effective_capabilities"][0]["feature"],
            "vision"
        );
    }

    #[tokio::test]
    async fn shared_model_catalog_normalizes_local_platform_and_native_surfaces() {
        let mut config = default_config();
        config.allow_model_equivalence = false;
        let local = serde_json::json!({"id": "local/CLAUDE-SONNET-4-6", "owned_by": "local",
            "const_api": {"context_tokens": 1234}});
        for surface in [crate::surface::ApiSurface::OpenAi, crate::surface::ApiSurface::Anthropic, crate::surface::ApiSurface::Gemini] {
            for prefer_local in [false, true] {
                let entries = if surface == crate::surface::ApiSurface::Gemini {
                    serde_json::json!({"models": [
                        {"name": "models/vendor/CLAUDE-SONNET-4-6", "const_api": {"context_tokens": 4321}},
                        {"name": "models/other/claude-sonnet-4-6", "const_api": {"context_tokens": 9999}},
                        {"name": "models/vendor/Claude-Haiku-4-5:free", "inputTokenLimit": 2048}
                    ]})
                } else {
                    serde_json::json!({"data": [
                        {"id": "vendor/CLAUDE-SONNET-4-6", "const_api": {"context_tokens": 4321}},
                        {"id": "other/claude-sonnet-4-6", "const_api": {"context_tokens": 9999}},
                        {"id": "vendor/Claude-Haiku-4-5:free"}, null, {"id": "vendor/"}
                    ]})
                };
                let response = warp::reply::json(&entries).into_response();
                let response = merge_local_models_response(response, &config, surface, std::slice::from_ref(&local), prefer_local, false).await.unwrap();
                let bytes = crate::test_body_bytes(response.into_body()).await.unwrap();
                let payload: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                let collection = if surface == crate::surface::ApiSurface::Gemini { "models" } else { "data" };
                let models = payload[collection].as_array().unwrap();
                let ids = models.iter().filter_map(|entry| model_id_for_surface(surface, entry)).collect::<Vec<_>>();
                assert_eq!(ids, ["claude-haiku-4-5:free", "claude-sonnet-4-6"]);
                assert_eq!(models[1]["const_api"]["context_tokens"], 1234);
                if surface == crate::surface::ApiSurface::Gemini {
                    assert_eq!(models[0]["inputTokenLimit"], 2048);
                }
            }
        }
        assert_eq!(local["id"], "local/CLAUDE-SONNET-4-6");
    }

    #[test]
    fn short_model_routes_resolve_original_ids_without_equivalence_or_path_guessing() {
        let mut config = default_config();
        config.allow_model_equivalence = false;
        let channel = local_model_channel(&["Vendor/MiXeD:free", "Second/MiXeD:free"], "http://127.0.0.1:9/v1");
        assert!(channel_supports_model(&channel, "mixed:free"));
        assert!(!channel_supports_model(&channel, "Third/mixed:free"));
        assert!(!channel_supports_model(&channel, "mixed"));
        for requested in ["mixed:free", "Vendor/MiXeD:free"] {
            assert_eq!(channel_upstream_model_for_request(&channel, Some(requested)), "Vendor/MiXeD:free");
            assert_eq!(supplier_upstream_model_for_request(&supplier_from_channel(&channel), Some(requested)), "Vendor/MiXeD:free");
        }
        config.channels = vec![channel];
        let selected = select_local_channel_route_where(&config, "/v1/chat/completions", br#"{"model":"mixed:free","messages":[]}"#, |_| true).unwrap();
        assert!(!selected.substituted);
        assert_eq!(selected.channel.id, "local-models");
    }

    #[tokio::test]
    async fn platform_only_anthropic_catalog_is_rewritten_to_compatible_routes() {
        let config = default_config();
        let platform = serde_json::json!({
            "id": "gpt-5.6-terra",
            "object": "model",
            "owned_by": "platform"
        });
        let response =
            local_models_response(&config, crate::surface::ApiSurface::OpenAi, &[platform], false)
                .expect("platform response");
        let response = merge_local_models_response(
            response,
            &config,
            crate::surface::ApiSurface::Anthropic,
            &[],
            false,
            false,
        )
        .await
        .expect("rewritten response");
        let body = crate::test_body_bytes(response.into_body())
            .await
            .expect("rewritten body");
        let payload: serde_json::Value = serde_json::from_slice(&body).expect("rewritten JSON");
        let models = payload["data"].as_array().expect("models");
        let route = models
            .iter()
            .find(|model| model["id"] == "claude-opus-4-8")
            .expect("opus route");

        assert_eq!(route["display_name"], "claude-opus-4-8");
        assert!(route.get("anthropic_family_tier").is_none());
        assert!(models
            .iter()
            .all(|model| model["id"].as_str().is_some_and(|id| id.contains("claude"))));
        assert_eq!(payload["first_id"], models[0]["id"]);
        assert_eq!(payload["last_id"], models.last().expect("last model")["id"]);
        assert_eq!(payload["has_more"], false);
    }

    #[tokio::test]
    async fn local_model_details_use_the_requested_native_surface_shape() {
        let model = serde_json::json!({
            "id": "Claude-Sonnet-4-6",
            "object": "model",
            "owned_by": "local",
            "const_api": { "source": "local_supplier" }
        });
        let tests = [
            ("/v1/models/claude-sonnet-4-6", "id", "claude-sonnet-4-6"),
            (
                "/anthropic/v1/models/claude-sonnet-4-6",
                "display_name",
                "claude-sonnet-4-6",
            ),
            (
                "/gemini/v1beta/models/claude-sonnet-4-6",
                "name",
                "models/claude-sonnet-4-6",
            ),
        ];
        let config = default_config();
        for (path, field, expected) in tests {
            let route = crate::surface::resolve_api_route("GET", path, false).unwrap();
            let response =
                local_model_control_response(&config, &route, std::slice::from_ref(&model), false)
                    .expect("model detail response");
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            let body = crate::test_body_bytes(response.into_body())
                .await
                .expect("detail body");
            let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(payload[field], expected, "{path}: {payload}");
        }
    }

    #[tokio::test]
    async fn anthropic_model_catalog_exposes_compatible_routes_for_gpt_models() {
        let config = default_config();
        let model = serde_json::json!({
            "id": "gpt-5.6-sol",
            "object": "model",
            "owned_by": "local",
            "const_api": { "source": "local_supplier" }
        });

        let response = local_models_response(
            &config,
            crate::surface::ApiSurface::Anthropic,
            std::slice::from_ref(&model),
            false,
        )
        .expect("Anthropic model list");
        let body = crate::test_body_bytes(response.into_body())
            .await
            .expect("model list body");
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();

        let models = payload["data"].as_array().expect("models");
        let fable = models
            .iter()
            .find(|model| model["id"] == "claude-fable-5")
            .expect("fable route");

        assert_eq!(fable["display_name"], "claude-fable-5");
        assert_eq!(fable["const_api"]["source"], "local_supplier");
        assert!(fable.get("anthropic_family_tier").is_none());
        assert!(fable.get("is_family_default").is_none());
        assert!(fable.get("supports_1m").is_none());
        assert!(fable.get("max_input_tokens").is_none());
        let haiku = models
            .iter()
            .find(|model| model["id"] == "claude-haiku-4-5")
            .expect("haiku route backed by higher compatible supply");
        assert_eq!(haiku["const_api"]["source"], "local_supplier");
        let actual_routes = models
            .iter()
            .filter_map(|model| model["id"].as_str())
            .collect::<Vec<_>>();
        let mut expected_routes = crate::model_compatibility::anthropic_model_routes(
            &config,
            &["gpt-5.6-sol".to_string()],
        );
        expected_routes.sort();
        assert_eq!(
            actual_routes,
            expected_routes.iter().map(String::as_str).collect::<Vec<_>>()
        );
        assert!(!payload.to_string().contains("\"id\":\"gpt-5.6-sol\""));
    }

    #[tokio::test]
    async fn openai_model_catalog_keeps_only_actual_live_models_with_equivalence_enabled() {
        let config = default_config();
        assert!(config.allow_model_equivalence);
        let model = serde_json::json!({
            "id": "gpt-5.6-sol",
            "object": "model",
            "owned_by": "local"
        });

        let response = local_models_response(
            &config,
            crate::surface::ApiSurface::OpenAi,
            std::slice::from_ref(&model),
            false,
        )
        .expect("OpenAI model list");
        let body = crate::test_body_bytes(response.into_body())
            .await
            .expect("model list body");
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let models = payload["data"].as_array().expect("models");

        assert_eq!(models.len(), 1);
        assert_eq!(models[0]["id"], "gpt-5.6-sol");
        assert!(!payload.to_string().contains("claude-haiku-4-5"));
    }

    #[tokio::test]
    async fn anthropic_alias_metadata_comes_from_the_first_available_route_candidate() {
        let config = default_config();
        let models = [
            serde_json::json!({
                "id": "gpt-5.6-terra",
                "object": "model",
                "const_api": { "vision": false, "source": "frontier" }
            }),
            serde_json::json!({
                "id": "claude-fable-5",
                "object": "model",
                "const_api": { "vision": true, "source": "apex" }
            }),
        ];

        let response =
            local_models_response(&config, crate::surface::ApiSurface::Anthropic, &models, false)
                .expect("Anthropic model list");
        let body = crate::test_body_bytes(response.into_body())
            .await
            .expect("model list body");
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let opus = payload["data"]
            .as_array()
            .expect("models")
            .iter()
            .find(|model| model["id"] == "claude-opus-4-8")
            .expect("frontier Claude route");

        assert_eq!(opus["const_api"]["source"], "frontier");
        assert_eq!(opus["const_api"]["vision"], false);
    }

    #[tokio::test]
    async fn local_missing_model_uses_native_error_shape() {
        let route = crate::surface::resolve_api_route("GET", "/anthropic/v1/models/missing", false)
            .unwrap();
        let response = local_model_control_response(&default_config(), &route, &[], false)
            .expect("missing model response");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(response.headers().contains_key("x-request-id"));
        let body = crate::test_body_bytes(response.into_body())
            .await
            .expect("error body");
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["type"], "error");
        assert_eq!(payload["error"]["type"], "not_found_error");
    }

    #[test]
    fn local_selection_prefers_native_protocol_before_manual_order() {
        let mut responses = local_model_channel(&["shared-model"], "http://127.0.0.1:9/v1");
        responses.id = "responses".to_string();
        responses.v2.default_target.protocol = crate::protocol::kind::ProtocolKind::OpenAiResponses;
        responses.surface_bindings[0].protocols = vec![ChannelProtocolBinding {
            protocol: "openai_responses".to_string(),
            preferred: true,
            verification: ProtocolVerification::default(),
        }];
        responses.api_format = "openai_responses".to_string();
        responses.supported_protocols = vec!["openai_responses".to_string()];
        responses.capability_profiles = vec![ChannelCapabilityProfile {
            protocol: "openai_responses".to_string(),
            non_stream_json: true,
            verification_state: "verified".to_string(),
            ..Default::default()
        }];
        let mut chat = local_model_channel(&["shared-model"], "http://127.0.0.1:10/v1");
        chat.id = "chat".to_string();
        chat.capability_profiles = vec![ChannelCapabilityProfile {
            protocol: "openai_chat".to_string(),
            non_stream_json: true,
            verification_state: "verified".to_string(),
            ..Default::default()
        }];
        let mut cfg = default_config();
        cfg.channels = vec![responses, chat];

        let selected = select_local_channel(
            &cfg,
            "/v1/chat/completions",
            br#"{"model":"shared-model","messages":[]}"#,
        )
        .expect("native protocol channel");

        assert_eq!(selected.id, "chat");
    }

    #[test]
    fn local_protocol_rank_uses_actual_default_target_not_an_unused_surface() {
        let mut channel = local_model_channel(&["shared-model"], "http://127.0.0.1:9/v1");
        let mut anthropic = channel.surface_bindings[0].clone();
        anthropic.surface = crate::surface::ApiSurface::Anthropic;
        anthropic.base_url = "http://127.0.0.1:10".to_string();
        anthropic.endpoint_profile = "anthropic".to_string();
        anthropic.protocols = vec![ChannelProtocolBinding {
            protocol: "anthropic_messages".to_string(),
            preferred: true,
            verification: ProtocolVerification::default(),
        }];
        channel.surface_bindings.push(anthropic);
        channel.v2.default_target = crate::model::ChannelTarget {
            surface: crate::surface::ApiSurface::Anthropic,
            protocol: crate::protocol::kind::ProtocolKind::AnthropicMessages,
        };

        // An OpenAI Chat binding exists, but unmatched Responses actually uses
        // the Anthropic default, so this is cross-surface conversion, not rank 1.
        assert_eq!(
            local_channel_protocol_fidelity_rank(&channel, "openai_responses", "shared-model"),
            Some(2)
        );
        assert_eq!(local_channel_protocol_fidelity_rank(&channel, "openai_chat", "shared-model"), Some(0));
        channel.v2.default_target = crate::model::ChannelTarget {
            surface: crate::surface::ApiSurface::OpenAi,
            protocol: crate::protocol::kind::ProtocolKind::OpenAiChat,
        };
        assert_eq!(
            local_channel_protocol_fidelity_rank(&channel, "openai_responses", "shared-model"),
            Some(1)
        );
        // A model-specific gateway dialect overrides the channel-wide default.
        channel.set_source_driver(crate::source_driver::SourceDriverId::OpencodeZen);
        assert_eq!(local_channel_protocol_fidelity_rank(&channel, "anthropic_messages", "claude-test"), Some(0));
        assert_eq!(local_channel_protocol_fidelity_rank(&channel, "openai_chat", "claude-test"), Some(2));
    }

    #[test]
    fn local_preflight_failures_retry_without_penalty_but_never_replay_started_or_owned_requests() {
        use crate::channel_executor::surface_operation_not_supported_error;
        let path = "/v1/chat/completions";
        for error in [
            surface_operation_not_supported_error("Responses binding rejected"),
            crate::supplier::request_conversion_failure("unsupported required feature"),
        ] {
            assert_eq!(local_execution_error_retry_scope(&error, false, path), LocalRouteRetryScope::Preflight);
            assert_eq!(local_execution_error_retry_scope(&error, true, path), LocalRouteRetryScope::None);
        }
        for error in [
            crate::channel_executor::ambiguous_transport_error("outcome unknown"),
            crate::channel_executor::response_conversion_error("invalid upstream body"),
            crate::supplier::response_conversion_failure(false, false, "invalid upstream body"),
            crate::supplier::upstream_stream_failure(true, true, "stream interrupted"),
        ] {
            assert_eq!(local_execution_error_retry_scope(&error, false, path), LocalRouteRetryScope::None);
        }
    }

    #[test]
    fn local_selection_skips_a_catalog_model_with_a_rejected_native_binding() {
        use crate::source_driver::SourceDriverId;
        for state in ["rejected", "failed", "unsupported", "missing"] {
            let mut denied = crate::coding_gateway::tests::channel(SourceDriverId::CopilotSubscription, "http://127.0.0.1:9");
            denied.id = "denied-first".into();
            denied.enabled = true;
            denied.models = vec!["gpt-6-luna".into()];
            denied.capability_profiles = ["openai_chat", "openai_responses"].into_iter().map(|protocol| ChannelCapabilityProfile {
                protocol: protocol.into(), model_pattern: "gpt-6-luna".into(), catalog_metadata: true,
                catalog_protocol_supported: Some(protocol == "openai_responses"), ..Default::default()
            }).collect();
            for binding in &mut denied.surface_bindings {
                if state == "missing" {
                    binding.protocols.retain(|p| p.protocol != "openai_responses");
                } else {
                    for protocol in &mut binding.protocols {
                        if protocol.protocol == "openai_responses" {
                            protocol.verification.state = state.into();
                        }
                    }
                }
            }
            let mut available = local_model_channel(&["gpt-6-luna"], "http://127.0.0.1:10/v1");
            available.id = "available-second".into();
            let mut config = default_config();
            config.channels = vec![denied, available];
            let selected = select_local_channel_route_where(&config, "/v1/chat/completions",
                br#"{"model":"gpt-6-luna","messages":[]}"#, |_| true).unwrap();
            assert_eq!(selected.channel.id, "available-second", "{state}");
            // A rejected optional Responses binding must not exclude a model
            // whose catalog also explicitly permits the still-usable Chat API.
            config.channels[0].capability_profiles[0].catalog_protocol_supported = Some(true);
            let selected = select_local_channel_route_where(&config, "/v1/chat/completions",
                br#"{"model":"gpt-6-luna","messages":[]}"#, |_| true).unwrap();
            assert_eq!(selected.channel.id, "denied-first", "Chat remains executable: {state}");
        }
    }

    #[tokio::test]
    async fn local_model_refusal_retries_another_channel_without_disabling_other_models() {
        for (status, code, can_retry) in [
            (StatusCode::BAD_REQUEST, "model_not_supported", true),
            (StatusCode::NOT_FOUND, "model_not_found", true),
            (StatusCode::BAD_REQUEST, "invalid_argument", false),
        ] {
            let calls = Arc::new(StdMutex::new(Vec::<String>::new()));
            let recorded = calls.clone();
            let upstream = warp::path::param::<String>()
                .and(warp::body::json())
                .map(move |channel: String, body: serde_json::Value| {
                    let model = body["model"].as_str().unwrap().to_string();
                    recorded.lock().unwrap().push(format!("{channel}:{model}"));
                    if channel == "first" && model == "model-a" {
                        return warp::reply::with_status(
                            warp::reply::json(&serde_json::json!({"error": {
                                "code": code, "message": "request rejected"
                            }})),
                            status,
                        ).into_response();
                    }
                    warp::reply::json(&serde_json::json!({
                        "id": "success", "source": channel, "choices": []
                    })).into_response()
                });
            let (address, server) = crate::bind_ephemeral!(upstream, ([127, 0, 0, 1], 0));
            let server = tokio::spawn(server);
            for (prefer_local, allow_equivalence) in [(true, true), (true, false), (false, true), (false, false)] {
                calls.lock().unwrap().clear();
                let mut config = default_config();
                config.account_device_api_key.clear();
                config.endpoints.clear();
                config.prefer_local_supply = prefer_local;
                config.allow_model_equivalence = allow_equivalence;
                config.channels = ["first", "second"].into_iter().map(|id| {
                    let mut channel = local_model_channel(
                        &["model-a", "model-b"], &format!("http://{address}/{id}/v1"),
                    );
                    channel.id = id.into();
                    channel
                }).collect();
                let shared = Arc::new(ProxyShared::from_config(normalize_config(config), Client::new()));
                // Distinct cache identities ensure the second request is routed
                // by health, not by the first request's successful-channel affinity.
                for index in 0..2 {
                    let response = forward_with_failover(
                        warp::http::Method::POST, "/v1/chat/completions", "", HeaderMap::new(),
                        bytes::Bytes::from(serde_json::json!({
                            "model":"model-a", "messages":[], "prompt_cache_key":format!("request-{index}")
                        }).to_string()), &shared,
                    ).await.unwrap();
                    assert_eq!(response.status(), if can_retry { StatusCode::OK } else { status },
                        "{code}, prefer_local={prefer_local}");
                    let _ = crate::test_body_bytes(response.into_body()).await.unwrap();
                }
                assert_eq!(forward_chat_json(&shared, "model-b", "other-model").await["source"], "first");
                let expected = if can_retry {
                    vec!["first:model-a", "second:model-a", "second:model-a", "first:model-b"]
                } else {
                    vec!["first:model-a", "first:model-a", "first:model-b"]
                };
                assert_eq!(*calls.lock().unwrap(), expected, "{code}, prefer_local={prefer_local}");
                let health = shared.local_model_quota_routes.lock().unwrap();
                let failed_key = crate::supplier::model_quota_route_key("first", "model-a");
                assert_eq!(health.get(&failed_key).is_some_and(|state| state.has_runtime_failure()), can_retry);
                assert!(!health.get(&crate::supplier::model_quota_route_key("first", "model-b"))
                    .is_some_and(|state| state.has_runtime_failure()));
                assert_eq!(shared.local_channel_readiness.lock().unwrap().get("first"), Some(&true));
            }
            server.abort();
        }
    }

    #[tokio::test]
    async fn local_explicitly_unsafe_failure_never_replays_on_another_channel() {
        let calls = Arc::new(StdMutex::new(Vec::<String>::new()));
        let recorded = calls.clone();
        let upstream = warp::path::param::<String>().map(move |channel: String| {
            recorded.lock().unwrap().push(channel);
            warp::reply::with_status(
                warp::reply::json(&serde_json::json!({"error": {
                    "message": "captcha verification required"
                }})),
                StatusCode::FORBIDDEN,
            )
        });
        let (address, server) = crate::bind_ephemeral!(upstream, ([127, 0, 0, 1], 0));
        let server = tokio::spawn(server);
        for allow_equivalence in [false, true] {
            for (path, body) in [
                ("/v1/chat/completions", r#"{"model":"model-a","messages":[]}"#),
                ("/gemini/v1beta/models/model-a:generateContent", r#"{"contents":[{"role":"user","parts":[{"text":"hello"}]}]}"#),
            ] {
                calls.lock().unwrap().clear();
                let mut config = default_config();
                config.account_device_api_key.clear();
                config.endpoints.clear();
                config.allow_model_equivalence = allow_equivalence;
                config.channels = ["first", "second"].into_iter().map(|id| {
                    let mut channel = local_model_channel(&["model-a"], &format!("http://{address}/{id}/v1"));
                    channel.id = id.into();
                    channel
                }).collect();
                let shared = Arc::new(ProxyShared::from_config(normalize_config(config), Client::new()));
                let response = forward_with_failover(
                    warp::http::Method::POST, path, "", HeaderMap::new(),
                    bytes::Bytes::from_static(body.as_bytes()), &shared,
                ).await.unwrap();
                assert_eq!(response.status(), StatusCode::FORBIDDEN);
                assert!(response.extensions().get::<LocalRetryForbidden>().is_some());
                assert_eq!(*calls.lock().unwrap(), ["first"], "{path}, equivalence={allow_equivalence}");
            }
        }
        server.abort();
    }

    #[tokio::test]
    async fn local_conversion_failure_falls_through_without_sending_or_disabling_the_channel() {
        let calls = Arc::new(StdMutex::new(Vec::<String>::new()));
        let recorded = calls.clone();
        let upstream = warp::path::param::<String>().map(move |channel: String| {
            recorded.lock().unwrap().push(channel.clone());
            warp::reply::json(&serde_json::json!({
                "id": "converted-success", "object": "response", "model": "model-a",
                "status": "completed", "output": [{"type":"message", "role":"assistant",
                    "content":[{"type":"output_text","text":"OK"}]}]
            }))
        });
        let (address, server) = crate::bind_ephemeral!(upstream, ([127, 0, 0, 1], 0));
        let server = tokio::spawn(server);
        let mut config = default_config();
        config.account_device_api_key.clear();
        config.endpoints.clear();
        config.prefer_local_supply = true;
        config.channels = [("text-only-0", false), ("text-only-1", false),
            ("text-only-2", false), ("text-only-3", false), ("audio", true)]
            .into_iter().map(|(id, audio_input)| {
            let mut channel = openai_api_channel(id, "model-a", &format!("http://{address}/{id}/v1"));
            channel.capability_profiles = vec![ChannelCapabilityProfile {
                protocol: "openai_responses".into(), model_pattern: "model-a".into(),
                verification_state: "verified".into(), input_modalities_authoritative: true,
                verified_at_unix: now_unix(), non_stream_json: true, audio_input, ..Default::default()
            }];
            channel
        }).collect();
        let config = normalize_config(config);
        // Responses audio is an evidence-gated extension: four channels cannot
        // encode it. These preflight failures must not consume the three actual
        // upstream attempts before the last channel's supported dialect.
        let path = "/gemini/v1beta/models/model-a:generateContent";
        let body = bytes::Bytes::from_static(br#"{"contents":[{"role":"user","parts":[{"inlineData":{"mimeType":"audio/wav","data":"AAAA"}}]}]}"#);
        for channel in &config.channels {
            let conversion = upstream_request_for_api_format_with_profiles_report(
                "openai_responses", path, std::str::from_utf8(&body).unwrap(), "model-a",
                &channel.capability_profiles,
            );
            assert_eq!(conversion.is_ok(), channel.id == "audio", "{}", channel.id);
        }
        assert_eq!(select_local_channel(&config, path, &body).unwrap().id, "text-only-0");
        let shared = Arc::new(ProxyShared::from_config(config, Client::new()));
        let response = forward_with_failover(
            warp::http::Method::POST, path, "", HeaderMap::new(), body, &shared,
        ).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = crate::test_body_bytes(response.into_body()).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["candidates"][0]["content"]["parts"][0]["text"], "OK");
        assert_eq!(*calls.lock().unwrap(), ["audio"]);
        for index in 0..4 {
            let id = format!("text-only-{index}");
            assert_ne!(shared.local_channel_readiness.lock().unwrap().get(&id), Some(&false));
            assert!(!shared.local_model_quota_routes.lock().unwrap()
                .get(&crate::supplier::model_quota_route_key(&id, "model-a"))
                .is_some_and(|state| state.has_runtime_failure()));
        }
        server.abort();
    }

    #[tokio::test]
    async fn local_response_conversion_failure_never_dispatches_another_channel() {
        let calls = Arc::new(StdMutex::new(Vec::<String>::new()));
        let recorded = calls.clone();
        let upstream = warp::path::param::<String>().map(move |channel: String| {
            recorded.lock().unwrap().push(channel);
            warp::reply::with_header("invalid upstream JSON", "content-type", "application/json")
        });
        let (address, server) = crate::bind_ephemeral!(upstream, ([127, 0, 0, 1], 0));
        let server = tokio::spawn(server);
        let mut config = default_config();
        config.account_device_api_key.clear();
        config.endpoints.clear();
        config.channels = ["first", "second"].into_iter().map(|id| {
            let mut channel = local_model_channel(&["model-a"], &format!("http://{address}/{id}/v1"));
            channel.id = id.into();
            channel
        }).collect();
        let shared = Arc::new(ProxyShared::from_config(normalize_config(config), Client::new()));
        let error = forward_with_failover(
            warp::http::Method::POST, "/gemini/v1beta/models/model-a:generateContent", "", HeaderMap::new(),
            bytes::Bytes::from_static(br#"{"contents":[{"role":"user","parts":[{"text":"hello"}]}]}"#), &shared,
        ).await.unwrap_err();
        let evidence = crate::supplier::execution_evidence_for_error(&error);
        assert!(evidence.upstream_attempted && evidence.upstream_response_received);
        assert_eq!(evidence.error_code, "response_conversion_failed");
        assert_eq!(*calls.lock().unwrap(), ["first"]);
        assert_ne!(shared.local_channel_readiness.lock().unwrap().get("first"), Some(&false));
        server.abort();
    }

    #[tokio::test]
    async fn local_owner_bound_model_refusal_is_returned_once_without_rebalancing() {
        let calls = Arc::new(StdMutex::new(Vec::<String>::new()));
        let recorded = calls.clone();
        let upstream = warp::path::param::<String>().map(move |channel: String| {
            recorded.lock().unwrap().push(channel);
            warp::reply::with_status(
                warp::reply::json(&serde_json::json!({"error":{"code":"model_not_supported"}})),
                StatusCode::BAD_REQUEST,
            )
        });
        let (address, server) = crate::bind_ephemeral!(upstream, ([127, 0, 0, 1], 0));
        let server = tokio::spawn(server);
        let mut config = default_config();
        config.account_device_api_key.clear();
        config.endpoints.clear();
        config.channels = ["first", "second"].into_iter().map(|id| {
            openai_api_channel(id, "model-a", &format!("http://{address}/{id}/v1"))
        }).collect();
        let shared = Arc::new(ProxyShared::from_config(normalize_config(config), Client::new()));
        shared.local_resource_owners.bind_many(vec![LocalResourceOwner {
            key: LocalResourceOwnerKey {
                surface: crate::surface::ApiSurface::OpenAi,
                resource_type: "response".into(), resource_id: "resp_owned".into(),
            },
            channel_id: "first".into(), route_model: "model-a".into(), updated_at_unix: now_unix(),
        }]).await.unwrap();
        let response = forward_with_failover(
            warp::http::Method::POST, "/v1/responses", "", HeaderMap::new(),
            bytes::Bytes::from_static(br#"{"model":"model-a","previous_response_id":"resp_owned","input":"continue"}"#), &shared,
        ).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(*calls.lock().unwrap(), ["first"]);
        server.abort();
    }

    #[tokio::test]
    async fn local_rejected_bindings_do_not_consume_attempts_before_a_working_candidate() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = calls.clone();
        let upstream = warp::any().map(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            warp::reply::json(&serde_json::json!({"id":"working-candidate", "choices":[]}))
        });
        let (address, server) = crate::bind_ephemeral!(upstream, ([127, 0, 0, 1], 0));
        let server = tokio::spawn(server);
        for prefer_local in [true, false] {
            let mut config = default_config();
            config.account_device_api_key.clear();
            config.endpoints.clear();
            config.prefer_local_supply = prefer_local;
            config.channels = (0..5).map(|index| {
                let mut channel = crate::coding_gateway::tests::channel(
                    crate::source_driver::SourceDriverId::OpencodeZen, &format!("http://{address}"),
                );
                channel.id = format!("rejected-{index}");
                channel.enabled = true;
                channel.models = vec!["gpt-test".into()];
                for binding in &mut channel.surface_bindings {
                    for protocol in &mut binding.protocols {
                        if protocol.protocol == "openai_responses" {
                            protocol.verification.state = "rejected".into();
                        }
                    }
                }
                channel
            }).collect();
            config.channels.push(local_model_channel(&["gpt-test"], &format!("http://{address}/v1")));
            let shared = Arc::new(ProxyShared::from_config(normalize_config(config), Client::new()));
            let response = forward_with_failover(warp::http::Method::POST, "/v1/chat/completions", "",
                HeaderMap::new(), bytes::Bytes::from_static(br#"{"model":"gpt-test","messages":[]}"#), &shared,
            ).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK, "prefer_local={prefer_local}");
            let body = crate::test_body_bytes(response.into_body()).await.unwrap();
            assert_eq!(serde_json::from_slice::<serde_json::Value>(&body).unwrap()["id"], "working-candidate");
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        server.abort();
    }

    #[test]
    fn short_model_projection_keeps_only_evidence_for_the_original_model() {
        let original = serde_json::json!({"id": "First/MiXeD:free", "const_api": {
            "effective_capabilities": [
                {"model_pattern": "First/*", "feature": "reasoning", "state": "supported"},
                {"model_pattern": "Other/*", "feature": "vision", "state": "supported"}
            ]
        }});
        let projected = render_model_for_surface(crate::surface::ApiSurface::OpenAi, &original);
        assert_eq!(projected["const_api"]["effective_capabilities"], serde_json::json!([
            {"model_pattern": "mixed:free", "feature": "reasoning", "state": "supported"}
        ]));
        assert_eq!(original["const_api"]["effective_capabilities"][0]["model_pattern"], "First/*");
        let models = crate::tool_model_metadata::tool_models_from_response(&serde_json::json!({"data": [projected]}));
        assert!(models[0].reasoning);
        assert!(!models[0].attachment);
    }

    #[test]
    fn local_priority_selects_first_equally_suitable_same_model_channel() {
        let mut first = local_model_channel(&["shared-model"], "http://127.0.0.1:9/v1");
        first.id = "first".to_string();
        let mut second = local_model_channel(&["shared-model"], "http://127.0.0.1:10/v1");
        second.id = "second".to_string();
        let mut cfg = default_config();
        cfg.channels = vec![first, second];
        let body = br#"{"model":"shared-model","messages":[]}"#;

        for allow_unverified in [false, true] {
            cfg.allow_unverified_platform_routes = allow_unverified;
            assert_eq!(
                select_local_channel(&cfg, "/v1/chat/completions", body)
                    .expect("first priority channel")
                    .id,
                "first"
            );
        }

        cfg.channels.swap(0, 1);
        assert_eq!(
            select_local_channel(&cfg, "/v1/chat/completions", body)
                .expect("reordered priority channel")
                .id,
            "second"
        );
    }

    #[test]
    fn local_selection_uses_protocol_conversion_when_no_native_channel_exists() {
        let mut responses = local_model_channel(&["shared-model"], "http://127.0.0.1:9/v1");
        responses.v2.default_target.protocol = crate::protocol::kind::ProtocolKind::OpenAiResponses;
        responses.surface_bindings[0].protocols = vec![ChannelProtocolBinding {
            protocol: "openai_responses".to_string(),
            preferred: true,
            verification: ProtocolVerification::default(),
        }];
        responses.api_format = "openai_responses".to_string();
        responses.supported_protocols = vec!["openai_responses".to_string()];
        responses.capability_profiles = vec![ChannelCapabilityProfile {
            protocol: "openai_responses".to_string(),
            non_stream_json: true,
            verification_state: "verified".to_string(),
            ..Default::default()
        }];
        let mut cfg = default_config();
        cfg.channels = vec![responses];

        let selected = select_local_channel(
            &cfg,
            "/v1/chat/completions",
            br#"{"model":"shared-model","messages":[]}"#,
        )
        .expect("convertible responses channel");

        assert_eq!(selected.api_format, "openai_responses");
    }

    #[test]
    fn local_selection_prefers_positive_file_evidence_but_keeps_unknown_fallbacks() {
        let responses_channel = |id: &str, file_input: bool, authoritative: bool| {
            let mut channel =
                local_model_channel(&["shared-model"], "http://127.0.0.1:9/v1");
            channel.id = id.to_string();
            channel.v2.default_target.protocol =
                crate::protocol::kind::ProtocolKind::OpenAiResponses;
            channel.surface_bindings[0].protocols = vec![ChannelProtocolBinding {
                protocol: "openai_responses".to_string(),
                preferred: true,
                verification: ProtocolVerification::default(),
            }];
            channel.api_format = "openai_responses".to_string();
            channel.supported_protocols = vec!["openai_responses".to_string()];
            channel.capability_profiles = vec![ChannelCapabilityProfile {
                protocol: "openai_responses".to_string(),
                capability_layer: "driver".to_string(),
                non_stream_json: true,
                file_input,
                input_modalities_authoritative: authoritative,
                verification_state: "verified".to_string(),
                ..Default::default()
            }];
            channel
        };
        let mut cfg = default_config();
        cfg.channels = vec![
            responses_channel("unknown", false, false),
            responses_channel("file-capable", true, true),
        ];
        let body = br#"{
            "model":"shared-model",
            "input":[{"role":"user","content":[{"type":"input_file","file_id":"file_123"}]}]
        }"#;

        let selected =
            select_local_channel_route_where(&cfg, "/v1/responses", body, |_| true)
                .expect("file-capable route");

        assert_eq!(selected.channel.id, "file-capable");

        cfg.channels = vec![responses_channel("unknown", false, false)];
        let fallback =
            select_local_channel_route_where(&cfg, "/v1/responses", body, |_| true)
                .expect("unknown evidence remains routable");
        assert_eq!(fallback.channel.id, "unknown");

        let mut confirmed_unsupported =
            responses_channel("confirmed-unsupported", false, true);
        confirmed_unsupported.capability_profiles[0].model_pattern =
            "shared-model".to_string();
        confirmed_unsupported.capability_profiles[0].verified_at_unix = now_unix();
        cfg.channels = vec![
            confirmed_unsupported,
            responses_channel("unknown", false, false),
        ];
        let fallback =
            select_local_channel_route_where(&cfg, "/v1/responses", body, |_| true)
                .expect("confirmed negative is skipped while an unknown path exists");
        assert_eq!(fallback.channel.id, "unknown");
    }

    #[tokio::test]
    async fn local_subscription_buffered_stream_fallback_preserves_native_json() {
        let mut config = default_supplier_config();
        config.kind = "subscription_adapter".to_string();
        config.api_format = "subscription_skeleton".to_string();
        config.upstream_model = "claude-test".to_string();
        config.models = vec!["claude-test".to_string()];
        config.subscription.platform = "claude".to_string();
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        let body = serde_json::json!({
            "id": "chatcmpl_local",
            "object": "chat.completion",
            "model": "claude-test",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "local stream"},
                "finish_reason": "stop"
            }]
        })
        .to_string();
        let payload = crate::surface_wire::http_response_payload(200, &headers, body.as_bytes())
            .expect("typed local subscription response");

        let response = subscription_payload_as_local_stream_response(
            &config,
            "/v1/chat/completions",
            r#"{"model":"claude-test","stream":true,"messages":[]}"#,
            payload,
        )
        .expect("native response");
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
        let body = crate::test_body_bytes(response.into_body())
            .await
            .expect("stream body");
        let body = String::from_utf8_lossy(&body);
        assert!(body.contains("chat.completion"), "{body}");
        assert!(body.contains("local stream"), "{body}");
        assert!(!body.contains("[DONE]"), "{body}");
    }

    #[test]
    fn response_shape_matrix_covers_all_protocol_pairs() {
        let fixtures = [
            (
                "openai_chat",
                include_str!("../../../tests/fixtures/protocol-v2/openai-chat/basic-tools.json"),
                include_str!("../../../tests/fixtures/protocol-v2/openai-chat/basic-tools.sse"),
            ),
            (
                "openai_responses",
                include_str!("../../../tests/fixtures/protocol-v2/openai-responses/basic-tools.json"),
                include_str!("../../../tests/fixtures/protocol-v2/openai-responses/basic-tools.sse"),
            ),
            (
                "anthropic_messages",
                include_str!(
                    "../../../tests/fixtures/protocol-v2/anthropic-messages/basic-tools.json"
                ),
                include_str!("../../../tests/fixtures/protocol-v2/anthropic-messages/basic-tools.sse"),
            ),
            (
                "gemini_native",
                include_str!("../../../tests/fixtures/protocol-v2/gemini-native/basic-tools.json"),
                include_str!("../../../tests/fixtures/protocol-v2/gemini-native/basic-tools.sse"),
            ),
        ];
        let inbound_protocols = [
            "openai_chat",
            "openai_responses",
            "anthropic_messages",
            "gemini_native",
        ];

        for (upstream_protocol, fixture, upstream_sse) in fixtures {
            let fixture: serde_json::Value = serde_json::from_str(fixture).expect("fixture");
            let upstream_json = serde_json::to_string(&fixture["response"]).expect("response");
            for inbound_protocol in inbound_protocols {
                let native = inbound_protocol == upstream_protocol;
                let json_to_json = response_body_from_target_as_inbound(
                    &upstream_json,
                    inbound_protocol,
                    upstream_protocol,
                    "source-model",
                )
                .unwrap_or_else(|error| {
                    panic!(
                        "non-stream -> non-stream {upstream_protocol} -> {inbound_protocol}: {error}"
                    )
                });
                serde_json::from_str::<serde_json::Value>(&json_to_json).unwrap_or_else(|error| {
                    panic!("non-stream JSON {upstream_protocol} -> {inbound_protocol}: {error}")
                });

                let json_to_stream = sse_body_from_target_body_as_inbound(
                    &upstream_json,
                    inbound_protocol,
                    upstream_protocol,
                    "source-model",
                )
                .unwrap_or_else(|error| {
                    panic!(
                        "non-stream -> stream {upstream_protocol} -> {inbound_protocol}: {error}"
                    )
                });
                assert!(!json_to_stream.is_empty());
                canonical_stream_events_from_target_sse(
                    &json_to_stream,
                    inbound_protocol,
                    "source-model",
                )
                .unwrap_or_else(|error| {
                    panic!("rendered stream {upstream_protocol} -> {inbound_protocol}: {error}")
                });

                let stream_to_json = sse_body_from_target_as_inbound(
                    upstream_sse,
                    inbound_protocol,
                    upstream_protocol,
                    "source-model",
                )
                .unwrap_or_else(|error| {
                    panic!(
                        "stream -> non-stream {upstream_protocol} -> {inbound_protocol}: {error}"
                    )
                });
                serde_json::from_str::<serde_json::Value>(&stream_to_json).unwrap_or_else(
                    |error| {
                        panic!("buffered JSON {upstream_protocol} -> {inbound_protocol}: {error}")
                    },
                );

                if native {
                    let mut converter = inbound_sse_stream_converter(
                        inbound_protocol,
                        upstream_protocol,
                        "source-model",
                    )
                    .unwrap();
                    assert_eq!(
                        converter.push(upstream_sse.as_bytes()),
                        bytes::Bytes::copy_from_slice(upstream_sse.as_bytes())
                    );
                    assert!(converter.finish().is_empty());
                    continue;
                }

                let events = canonical_stream_events_from_target_sse(
                    upstream_sse,
                    upstream_protocol,
                    "source-model",
                )
                .unwrap_or_else(|error| panic!("parse {upstream_protocol} stream: {error}"));
                let stream_to_stream =
                    sse_from_canonical_stream_events(&events, inbound_protocol, "source-model")
                        .unwrap_or_else(|error| {
                            panic!(
                        "stream -> stream {upstream_protocol} -> {inbound_protocol}: {error}"
                    )
                        });
                assert!(!stream_to_stream.is_empty());
                canonical_stream_events_from_target_sse(
                    &stream_to_stream,
                    inbound_protocol,
                    "source-model",
                )
                .unwrap_or_else(|error| {
                    panic!("converted stream {upstream_protocol} -> {inbound_protocol}: {error}")
                });
            }
        }
    }

    #[test]
    fn local_proxy_auth_requires_configured_key_for_api_paths() {
        let mut cfg = default_config();
        cfg.api_key = "sk-local-test".to_string();
        let headers = HeaderMap::new();

        assert!(!local_proxy_request_authorized(
            "/v1/models",
            "",
            &headers,
            &cfg
        ));
        assert!(!local_proxy_request_authorized(
            "/api/me", "", &headers, &cfg
        ));
        assert!(local_proxy_request_authorized(
            "/healthz", "", &headers, &cfg
        ));

        let mut bearer_headers = HeaderMap::new();
        bearer_headers.insert(
            "authorization",
            "Bearer sk-local-test".parse().expect("header"),
        );
        assert!(local_proxy_request_authorized(
            "/v1/models",
            "",
            &bearer_headers,
            &cfg
        ));

        let mut api_key_headers = HeaderMap::new();
        api_key_headers.insert("x-api-key", "sk-local-test".parse().expect("header"));
        assert!(local_proxy_request_authorized(
            "/api/me",
            "",
            &api_key_headers,
            &cfg
        ));

        let mut gemini_headers = HeaderMap::new();
        gemini_headers.insert("x-goog-api-key", "sk-local-test".parse().expect("header"));
        assert!(local_proxy_request_authorized(
            "/gemini/v1beta/models",
            "",
            &gemini_headers,
            &cfg
        ));
        assert!(local_proxy_request_authorized(
            "/gemini/v1beta/models",
            "alt=sse&key=sk-local-test",
            &HeaderMap::new(),
            &cfg
        ));
        assert!(!local_proxy_request_authorized(
            "/api/me",
            "key=sk-local-test",
            &HeaderMap::new(),
            &cfg
        ));

        cfg.allow_lan_access = true;
        assert!(local_proxy_request_authorized(
            "/v1/models",
            "",
            &bearer_headers,
            &cfg
        ));
        assert!(!local_proxy_request_authorized(
            "/v1/models",
            "",
            &HeaderMap::new(),
            &cfg
        ));

        cfg.api_key = String::new();
        assert!(!local_proxy_request_authorized(
            "/v1/models",
            "",
            &api_key_headers,
            &cfg
        ));
    }

    #[test]
    fn lan_share_member_key_requires_lan_access_and_only_resolves_on_restricted_http() {
        let mut cfg = default_config();
        cfg.api_key = "sk-owner".to_string();
        let members = vec![LanShareMember {
            id: "member-a".to_string(),
            name: "Office A".to_string(),
            api_key: "cst-lan-member-a".to_string(),
            enabled: true,
            weekly_token_limit: 50_000,
        }];
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            "Bearer cst-lan-member-a".parse().expect("member header"),
        );

        assert!(local_proxy_request_identity_with_members(
            "/v1/models",
            "",
            &headers,
            &cfg,
            &members,
        )
        .is_none());

        cfg.allow_lan_access = true;
        let identity = local_proxy_request_identity_with_members(
            "/v1/models",
            "",
            &headers,
            &cfg,
            &members,
        )
            .expect("LAN member identity");
        let LocalProxyRequestIdentity::LanShareMember { member, .. } = identity else {
            panic!("member key must not resolve as the owner");
        };
        assert_eq!(member.id, "member-a");
        assert!(!local_proxy_request_authorized(
            "/v1/models",
            "",
            &headers,
            &cfg,
        ));
    }

    #[tokio::test]
    async fn models_endpoint_merges_ready_local_catalog_when_platform_is_preferred() {
        let platform_models = warp::path!("v1" / "models").map(|| {
            warp::reply::json(&serde_json::json!({
                "object": "list",
                "data": [
                    {"id": "platform-live-model", "object": "model", "owned_by": "platform"}
                ]
            }))
        });
        let (platform_addr, platform_server) =
            crate::bind_ephemeral!(platform_models, ([127, 0, 0, 1], 0));
        tokio::spawn(platform_server);

        let mut cfg = default_config();
        cfg.listen = "127.0.0.1:0".to_string();
        cfg.api_key = "sk-local-models".to_string();
        cfg.account_device_api_key = "sk-platform-device".to_string();
        cfg.prefer_local_supply = false;
        cfg.endpoints = vec![Endpoint {
            server_id: String::new(),
            name: "platform".to_string(),
            base_url: format!("http://{platform_addr}"),
            supplier_ws_url: String::new(),
            supplier_quic_url: String::new(),
            enabled: true,
        }];
        cfg.channels = vec![local_model_channel(
            &["local-live-model", "PLATFORM-LIVE-MODEL"],
            "http://127.0.0.1:9/v1",
        )];
        let shared = Arc::new(ProxyShared::from_config(
            normalize_config(cfg),
            Client::new(),
        ));
        let routes = warp::any()
            .and(warp::method())
            .and(warp::path::full())
            .and(optional_raw_query())
            .and(warp::header::headers_cloned())
            .and(warp::body::bytes())
            .and(with_shared(shared))
            .and_then(proxy_request);
        let (proxy_addr, proxy_server) = crate::bind_ephemeral!(routes, ([127, 0, 0, 1], 0));
        tokio::spawn(proxy_server);

        let value: serde_json::Value = Client::new()
            .get(format!("http://{proxy_addr}/v1/models"))
            .bearer_auth("sk-local-models")
            .send()
            .await
            .expect("models response")
            .json()
            .await
            .expect("models json");
        let ids: Vec<_> = value["data"]
            .as_array()
            .expect("model list")
            .iter()
            .filter_map(|item| item["id"].as_str())
            .collect();

        assert_eq!(ids, vec!["local-live-model", "platform-live-model"]);
    }

    fn model_catalog_test_shared(
        platform_addr: std::net::SocketAddr,
        upstream_timeout: Duration,
    ) -> Arc<ProxyShared> {
        let mut cfg = default_config();
        cfg.listen = "127.0.0.1:0".to_string();
        cfg.api_key = "sk-local-models".to_string();
        cfg.account_device_api_key = "sk-platform-device".to_string();
        cfg.prefer_local_supply = false;
        cfg.endpoints = vec![Endpoint {
            server_id: String::new(),
            name: "platform".to_string(),
            base_url: format!("http://{platform_addr}"),
            supplier_ws_url: String::new(),
            supplier_quic_url: String::new(),
            enabled: true,
        }];
        cfg.channels = vec![local_model_channel(
            &["claude-sonnet-4-6"],
            "http://127.0.0.1:9/v1",
        )];
        let mut shared = ProxyShared::from_config(normalize_config(cfg), Client::new());
        shared.model_catalog_upstream_timeout = upstream_timeout;
        Arc::new(shared)
    }

    #[tokio::test]
    async fn model_catalog_timeout_uses_stale_cache_on_every_surface() {
        let slow = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let slow_filter = slow.clone();
        let platform = warp::path::full().and_then(move |path: warp::path::FullPath| {
            let slow = slow_filter.clone();
            async move {
                if slow.load(std::sync::atomic::Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                let payload = match path.as_str() {
                    "/gemini/v1beta/models" => serde_json::json!({
                        "models": [{
                            "id": "platform-only-model",
                            "name": "models/platform-only-model"
                        }]
                    }),
                    _ => serde_json::json!({
                        "object": "list",
                        "data": [{
                            "id": "platform-only-model",
                            "object": "model",
                            "owned_by": "platform"
                        }]
                    }),
                };
                Ok::<_, Infallible>(warp::reply::json(&payload))
            }
        });
        let (platform_addr, platform_server) =
            crate::bind_ephemeral!(platform, ([127, 0, 0, 1], 0));
        tokio::spawn(platform_server);
        let shared = model_catalog_test_shared(platform_addr, Duration::from_millis(50));
        let cases = [
            (crate::surface::ApiSurface::OpenAi, "/v1/models"),
            (
                crate::surface::ApiSurface::Anthropic,
                "/anthropic/v1/models",
            ),
            (
                crate::surface::ApiSurface::Gemini,
                "/gemini/v1beta/models",
            ),
        ];

        for (surface, path) in cases {
            let response = forward_with_failover(
                warp::http::Method::GET,
                path,
                "page=1",
                HeaderMap::new(),
                bytes::Bytes::new(),
                &shared,
            )
            .await
            .unwrap_or_else(|error| panic!("warm {path}: {error}"));
            assert!(response.headers().get(MODEL_CATALOG_CACHE_HEADER).is_none());
            let body = crate::test_body_bytes(response.into_body())
                .await
                .expect("warm model catalog body");
            assert!(model_catalog_body_has_models(surface, &body), "{path}");
        }

        slow.store(true, std::sync::atomic::Ordering::SeqCst);
        for cached in shared.model_catalog_snapshots.lock().unwrap().values_mut() {
            cached.received_at -= MODEL_CATALOG_FRESH_TTL;
        }
        for (surface, path) in cases {
            let response = tokio::time::timeout(
                Duration::from_secs(1),
                forward_with_failover(
                    warp::http::Method::GET,
                    path,
                    "page=1",
                    HeaderMap::new(),
                    bytes::Bytes::new(),
                    &shared,
                ),
            )
            .await
            .unwrap_or_else(|_| panic!("stale fallback exceeded deadline for {path}"))
            .unwrap_or_else(|error| panic!("stale {path}: {error}"));
            assert_eq!(
                response
                    .headers()
                    .get(MODEL_CATALOG_CACHE_HEADER)
                    .and_then(|value| value.to_str().ok()),
                Some("stale"),
                "{path}"
            );
            let body = crate::test_body_bytes(response.into_body())
                .await
                .expect("stale model catalog body");
            assert!(model_catalog_body_has_models(surface, &body), "{path}");
            assert!(String::from_utf8_lossy(&body).contains("local_supplier"), "{path}");
        }
    }

    #[tokio::test]
    async fn cold_model_catalog_timeout_uses_ready_local_models_on_every_surface() {
        let platform = warp::path::full().and_then(|_: warp::path::FullPath| async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            Ok::<_, Infallible>(warp::reply::json(&serde_json::json!({
                "object": "list",
                "data": [{"id": "too-late"}]
            })))
        });
        let (platform_addr, platform_server) =
            crate::bind_ephemeral!(platform, ([127, 0, 0, 1], 0));
        tokio::spawn(platform_server);
        let shared = model_catalog_test_shared(platform_addr, Duration::from_millis(50));

        for (surface, path) in [
            (crate::surface::ApiSurface::OpenAi, "/v1/models"),
            (
                crate::surface::ApiSurface::Anthropic,
                "/anthropic/v1/models",
            ),
            (
                crate::surface::ApiSurface::Gemini,
                "/gemini/v1beta/models",
            ),
        ] {
            let response = tokio::time::timeout(
                Duration::from_secs(1),
                forward_with_failover(
                    warp::http::Method::GET,
                    path,
                    "",
                    HeaderMap::new(),
                    bytes::Bytes::new(),
                    &shared,
                ),
            )
            .await
            .unwrap_or_else(|_| panic!("cold fallback exceeded deadline for {path}"))
            .unwrap_or_else(|error| panic!("cold {path}: {error}"));
            assert!(response.headers().get(MODEL_CATALOG_CACHE_HEADER).is_none());
            let body = crate::test_body_bytes(response.into_body())
                .await
                .expect("cold model catalog body");
            assert!(model_catalog_body_has_models(surface, &body), "{path}");
            assert!(String::from_utf8_lossy(&body).contains("local_supplier"), "{path}");
        }
    }

    #[tokio::test]
    async fn model_catalog_deadline_does_not_limit_generation_requests() {
        let platform = warp::path::full().and_then(|_: warp::path::FullPath| async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            Ok::<_, Infallible>(warp::reply::json(&serde_json::json!({
                "id": "generation-response",
                "choices": []
            })))
        });
        let (platform_addr, platform_server) =
            crate::bind_ephemeral!(platform, ([127, 0, 0, 1], 0));
        tokio::spawn(platform_server);
        let mut cfg = default_config();
        cfg.account_device_api_key = "sk-platform-device".to_string();
        cfg.allow_model_equivalence = false;
        cfg.endpoints = vec![Endpoint {
            server_id: String::new(),
            name: "platform".to_string(),
            base_url: format!("http://{platform_addr}"),
            supplier_ws_url: String::new(),
            supplier_quic_url: String::new(),
            enabled: true,
        }];
        cfg.channels.clear();
        let mut shared = ProxyShared::from_config(normalize_config(cfg), Client::new());
        shared.model_catalog_upstream_timeout = Duration::from_millis(20);
        let shared = Arc::new(shared);

        let response = tokio::time::timeout(
            Duration::from_secs(1),
            forward_with_failover(
                warp::http::Method::POST,
                "/v1/chat/completions",
                "",
                HeaderMap::new(),
                bytes::Bytes::from_static(br#"{"model":"gpt-test","messages":[]}"#),
                &shared,
            ),
        )
        .await
        .expect("generation request completed within its normal timeout")
        .expect("generation response");

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn model_catalog_auth_rejection_clears_the_matching_cache() {
        let platform = warp::path::full().map(|_: warp::path::FullPath| {
            warp::reply::with_status(
                warp::reply::json(&serde_json::json!({
                    "error": {"message": "unauthorized"}
                })),
                StatusCode::UNAUTHORIZED,
            )
        });
        let (platform_addr, platform_server) =
            crate::bind_ephemeral!(platform, ([127, 0, 0, 1], 0));
        tokio::spawn(platform_server);
        let shared = model_catalog_test_shared(platform_addr, Duration::from_millis(50));
        let cached_body = bytes::Bytes::from_static(
            br#"{"object":"list","data":[{"id":"cached-model"}]}"#,
        );
        shared.remember_model_catalog_snapshot(
            crate::surface::ApiSurface::OpenAi,
            "",
            "sk-platform-device",
            &cached_body,
        );
        shared.remember_model_catalog_snapshot(
            crate::surface::ApiSurface::OpenAi,
            "page=2",
            "sk-platform-device",
            &cached_body,
        );
        assert!(shared
            .cached_model_catalog_response(crate::surface::ApiSurface::OpenAi, "")
            .expect("seeded cache lookup")
            .is_some());
        assert!(shared
            .cached_model_catalog_response(crate::surface::ApiSurface::OpenAi, "page=2")
        .expect("seeded query cache lookup")
            .is_some());

        for cached in shared.model_catalog_snapshots.lock().unwrap().values_mut() {
            cached.received_at -= MODEL_CATALOG_FRESH_TTL;
        }

        forward_with_failover(
            warp::http::Method::GET,
            "/v1/models",
            "",
            HeaderMap::new(),
            bytes::Bytes::new(),
            &shared,
        )
        .await
        .expect("local response after platform auth rejection");

        assert!(shared
            .cached_model_catalog_response(crate::surface::ApiSurface::OpenAi, "")
            .expect("post-auth-rejection cache lookup")
            .is_none());
        assert!(shared
            .cached_model_catalog_response(crate::surface::ApiSurface::OpenAi, "page=2")
            .expect("post-auth-rejection query cache lookup")
            .is_some());
    }

    #[test]
    fn model_catalog_cache_does_not_cross_platform_credentials() {
        let mut cfg = default_config();
        cfg.account_device_api_key = "sk-platform-old".to_string();
        let shared = ProxyShared::from_config(normalize_config(cfg), Client::new());
        let body = bytes::Bytes::from_static(
            br#"{"object":"list","data":[{"id":"cached-model"}]}"#,
        );
        shared.remember_model_catalog_snapshot(
            crate::surface::ApiSurface::OpenAi,
            "page=1",
            "sk-platform-old",
            &body,
        );
        assert!(shared
            .cached_model_catalog_response(crate::surface::ApiSurface::OpenAi, "page=1")
            .expect("old-key cache lookup")
            .is_some());

        *shared.platform_api_key.lock().expect("platform key lock") =
            "sk-platform-new".to_string();
        assert!(shared
            .cached_model_catalog_response(crate::surface::ApiSurface::OpenAi, "page=1")
            .expect("new-key cache lookup")
            .is_none());

        *shared.platform_api_key.lock().expect("platform key lock") =
            "sk-platform-old".to_string();
        shared.clear_model_catalog_snapshot(
            crate::surface::ApiSurface::OpenAi,
            "page=1",
            "sk-platform-new",
        );
        assert!(shared
            .cached_model_catalog_response(crate::surface::ApiSurface::OpenAi, "page=1")
            .expect("mismatched clear lookup")
            .is_some());
        shared.clear_model_catalog_snapshot(
            crate::surface::ApiSurface::OpenAi,
            "page=1",
            "sk-platform-old",
        );
        assert!(shared
            .cached_model_catalog_response(crate::surface::ApiSurface::OpenAi, "page=1")
            .expect("cleared cache lookup")
            .is_none());
    }

    #[test]
    fn model_catalog_cache_does_not_cross_query() {
        let mut cfg = default_config();
        cfg.account_device_api_key = "sk-platform".to_string();
        let shared = ProxyShared::from_config(normalize_config(cfg), Client::new());
        let body = bytes::Bytes::from_static(
            br#"{"object":"list","data":[{"id":"verified-model"}]}"#,
        );
        shared.remember_model_catalog_snapshot(
            crate::surface::ApiSurface::OpenAi,
            "",
            "sk-platform",
            &body,
        );

        assert!(shared
            .cached_model_catalog_response(crate::surface::ApiSurface::OpenAi, "")
            .expect("matching query cache lookup")
            .is_some());
        assert!(shared
            .cached_model_catalog_response(crate::surface::ApiSurface::OpenAi, "page=2")
            .expect("different query cache lookup")
            .is_none());
    }

    #[test]
    fn local_gemini_query_auth_removes_only_the_exact_used_segment() {
        let mut cfg = default_config();
        cfg.api_key = "local-proxy-key".to_string();
        let raw = "tag=one&k%65y=local-proxy-key&key=semantic&escape=%2f&empty=&bare";
        assert_eq!(
            strip_local_auth_query(
                "/gemini/v1beta/models/gemini:generateContent",
                raw,
                &HeaderMap::new(),
                &cfg,
            ),
            "tag=one&key=semantic&escape=%2f&empty=&bare"
        );
    }

    #[test]
    fn local_gemini_query_auth_removes_every_exact_matching_segment_only() {
        let mut cfg = default_config();
        cfg.api_key = "local-proxy-key".to_string();
        let raw = concat!(
            "key=local-proxy-key&tag=one&k%65y=local-proxy-key&",
            "key=semantic&Key=local-proxy-key&key=local-proxy-key%2F&empty=&bare"
        );
        assert_eq!(
            strip_local_auth_query(
                "/gemini/v1beta/models/gemini:generateContent",
                raw,
                &HeaderMap::new(),
                &cfg,
            ),
            "tag=one&key=semantic&Key=local-proxy-key&key=local-proxy-key%2F&empty=&bare"
        );
    }

    #[test]
    fn valid_header_presentation_preserves_matching_query_presentations() {
        let mut cfg = default_config();
        cfg.api_key = "local-proxy-key".to_string();
        let mut headers = HeaderMap::new();
        headers.insert("x-goog-api-key", "local-proxy-key".parse().unwrap());
        let raw = "key=local-proxy-key&k%65y=local-proxy-key&key=semantic";

        assert_eq!(
            strip_local_auth_query(
                "/gemini/v1beta/models/gemini:generateContent",
                raw,
                &headers,
                &cfg,
            ),
            raw
        );
    }

    #[test]
    fn invalid_header_does_not_hide_valid_query_presentation() {
        let mut cfg = default_config();
        cfg.api_key = "local-proxy-key".to_string();
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer invalid-key".parse().unwrap());
        let raw = "key=local-proxy-key&tag=one&k%65y=local-proxy-key&key=semantic";

        assert_eq!(
            strip_local_auth_query(
                "/gemini/v1beta/models/gemini:generateContent",
                raw,
                &headers,
                &cfg,
            ),
            "tag=one&key=semantic"
        );
    }

    #[test]
    fn local_header_auth_preserves_semantic_key_query_bytes_on_every_surface() {
        let mut cfg = default_config();
        cfg.api_key = "local-proxy-key".to_string();
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer local-proxy-key".parse().unwrap());
        let raw = "key=semantic&k%65y=second&tag=&bare";
        for path in [
            "/v1/future",
            "/anthropic/v1/future",
            "/gemini/v1beta/future",
        ] {
            assert_eq!(strip_local_auth_query(path, raw, &headers, &cfg), raw);
        }
    }

    #[tokio::test]
    async fn models_endpoint_uses_local_catalog_without_platform_login_or_connection() {
        let mut cfg = default_config();
        cfg.listen = "127.0.0.1:0".to_string();
        cfg.account_device_api_key.clear();
        cfg.prefer_local_supply = false;
        cfg.endpoints = vec![Endpoint {
            server_id: String::new(),
            name: "offline-platform".to_string(),
            base_url: "http://127.0.0.1:9".to_string(),
            supplier_ws_url: String::new(),
            supplier_quic_url: String::new(),
            enabled: true,
        }];
        cfg.channels = vec![local_model_channel(
            &["Local-Model-A", "local-model-a", "local-model-b"],
            "http://127.0.0.1:11434/v1",
        )];
        let shared = Arc::new(ProxyShared::from_config(
            normalize_config(cfg),
            Client::new(),
        ));

        let response = forward_with_failover(
            warp::http::Method::GET,
            "/v1/models",
            "",
            HeaderMap::new(),
            bytes::Bytes::new(),
            &shared,
        )
        .await
        .expect("local model response");
        let body = crate::test_body_bytes(response.into_body())
            .await
            .expect("models body");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("models json");
        let ids: Vec<_> = value["data"]
            .as_array()
            .expect("model list")
            .iter()
            .filter_map(|item| item["id"].as_str())
            .collect();

        assert_eq!(ids, vec!["local-model-a", "local-model-b"]);
    }

    #[tokio::test]
    async fn persisted_user_model_compatibility_substitutes_a_ready_local_model_offline() {
        let response_body = br#"{"id":"local","choices":[]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            response_body.len()
        )
        .into_bytes()
        .into_iter()
        .chain(response_body.iter().copied())
        .collect();
        let (upstream_base_url, captured, capture_task) = capture_one_http_request(response).await;

        let mut cfg = default_config();
        cfg.account_platform_id = "platform-1".to_string();
        cfg.account_user_id = "user-1".to_string();
        cfg.account_device_api_key.clear();
        cfg.prefer_local_supply = true;
        cfg.allow_model_equivalence = true;
        cfg.endpoints.clear();
        cfg.channels = vec![verified_openai_channel(
            "compatible-local",
            &["model-b"],
            &format!("{upstream_base_url}/v1"),
        )];
        cfg.model_compatibility_profiles.insert(
            model_compatibility_profile_key("platform-1", "user-1").expect("profile key"),
            LocalModelCompatibilityProfile {
                platform_id: "platform-1".to_string(),
                user_id: "user-1".to_string(),
                base_release_id: "release-1".to_string(),
                model_version_id: "models-1".to_string(),
                compatibility_version_id: "compat-1".to_string(),
                revision: 1,
                model_groups: vec![ModelCompatibilityGroupConfig {
                    id: "compatible".to_string(),
                    label: "Compatible".to_string(),
                    description: String::new(),
                    aliases: vec!["model-family".to_string()],
                    models: vec!["model-b".to_string()],
                    match_models: vec!["model-a".to_string(), "model-b".to_string()],
                    disabled: false,
                }],
                template_model_groups: Vec::new(),
                catalog_models: Vec::new(),
                sync_status: "synced".to_string(),
                customized: true,
                pending_reset: false,
                updated_at_unix_ms: 1,
            },
        );
        let shared = Arc::new(ProxyShared::from_config(
            normalize_config(cfg),
            Client::new(),
        ));

        let response = forward_with_failover(
            warp::http::Method::POST,
            "/v1/chat/completions",
            "",
            HeaderMap::new(),
            bytes::Bytes::from_static(br#"{"model":"model-a","messages":[]}"#),
            &shared,
        )
        .await
        .expect("compatible local response");
        assert_eq!(response.status(), StatusCode::OK);

        let captured = captured.await.expect("captured local request");
        let body = decoded_raw_http_body(&captured.headers, &captured.body);
        let payload: serde_json::Value =
            serde_json::from_slice(&body).expect("captured local request body");
        assert_eq!(payload["model"], "model-b");
        capture_task.await.expect("capture task");
    }

    #[tokio::test]
    async fn logged_out_fidelity_skips_platform_and_reaches_local_compatibility() {
        let platform_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let platform_calls_filter = platform_calls.clone();
        let platform = warp::any().map(move || {
            platform_calls_filter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            warp::reply::with_status(
                warp::reply::json(&serde_json::json!({
                    "error": {"message": "invalid api key"}
                })),
                StatusCode::UNAUTHORIZED,
            )
        });
        let (platform_addr, platform_server) =
            crate::bind_ephemeral!(platform, ([127, 0, 0, 1], 0));
        let platform_task = tokio::spawn(platform_server);

        let local_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let local_calls_filter = local_calls.clone();
        let local = warp::any().map(move || {
            local_calls_filter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            warp::reply::json(&serde_json::json!({
                "id": "local-compatible",
                "source": "local-compatible",
                "choices": []
            }))
        });
        let (local_addr, local_server) =
            crate::bind_ephemeral!(local, ([127, 0, 0, 1], 0));
        let local_task = tokio::spawn(local_server);

        let mut cfg = default_config();
        cfg.account_device_api_key.clear();
        cfg.prefer_local_supply = true;
        cfg.endpoints = vec![Endpoint {
            server_id: String::new(),
            name: "configured-but-logged-out-platform".to_string(),
            base_url: format!("http://{platform_addr}"),
            supplier_ws_url: String::new(),
            supplier_quic_url: String::new(),
            enabled: true,
        }];
        cfg.channels = vec![verified_openai_channel(
            "local-compatible",
            &["model-b"],
            &format!("http://{local_addr}/v1"),
        )];
        configure_model_compatibility_pair(&mut cfg, "model-a", "model-b");
        let shared = Arc::new(ProxyShared::from_config(
            normalize_config(cfg),
            Client::new(),
        ));

        let response = forward_with_failover(
            warp::http::Method::POST,
            "/v1/chat/completions",
            "",
            HeaderMap::new(),
            bytes::Bytes::from_static(br#"{"model":"model-a","messages":[]}"#),
            &shared,
        )
        .await
        .expect("local compatible response");
        let body = crate::test_body_bytes(response.into_body())
            .await
            .expect("local compatible body");
        let payload: serde_json::Value =
            serde_json::from_slice(&body).expect("local compatible json");

        assert_eq!(payload["source"], "local-compatible");
        assert_eq!(
            platform_calls.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(local_calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        platform_task.abort();
        local_task.abort();
    }

    #[test]
    fn local_route_does_not_guess_compatible_models_without_an_active_profile() {
        let mut cfg = default_config();
        cfg.allow_model_equivalence = true;
        cfg.account_platform_id.clear();
        cfg.account_user_id.clear();
        cfg.channels = vec![local_model_channel(
            &["actual-local-model"],
            "http://127.0.0.1:9/v1",
        )];

        assert!(select_local_channel_route_where(
            &cfg,
            "/v1/chat/completions",
            br#"{"model":"unknown-compatible-alias","messages":[]}"#,
            |_| true,
        )
        .is_none());
    }

    #[test]
    fn logged_out_local_route_uses_embedded_compatibility_defaults() {
        let mut cfg = default_config();
        cfg.account_platform_id.clear();
        cfg.account_user_id.clear();
        cfg.account_device_api_key.clear();
        cfg.allow_model_equivalence = true;
        cfg.model_compatibility_profiles.clear();
        cfg.channels = vec![local_model_channel(
            &["gpt-5.6-terra"],
            "http://127.0.0.1:9/v1",
        )];

        let selected = select_local_channel_route_where(
            &cfg,
            "/anthropic/v1/messages",
            br#"{"model":"claude-opus-4-8","messages":[]}"#,
            |_| true,
        )
        .expect("embedded compatibility route");

        assert_eq!(selected.route_model, "gpt-5.6-terra");
        assert!(selected.substituted);
    }

    #[test]
    fn lower_group_request_uses_an_available_higher_group_route() {
        let mut cfg = default_config();
        cfg.account_platform_id.clear();
        cfg.account_user_id.clear();
        cfg.account_device_api_key.clear();
        cfg.allow_model_equivalence = true;
        cfg.model_compatibility_profiles.clear();
        cfg.channels = vec![local_model_channel(
            &["gpt-5.6-sol"],
            "http://127.0.0.1:9/v1",
        )];

        let selected = select_local_channel_route_where(
            &cfg,
            "/anthropic/v1/messages",
            br#"{"model":"claude-haiku-4-5","max_tokens":1,"messages":[{"role":"user","content":"."}]}"#,
            |_| true,
        )
        .expect("higher compatibility route");

        assert_eq!(selected.route_model, "gpt-5.6-sol");
        assert!(selected.substituted);
    }

    #[test]
    fn ordinary_anthropic_request_uses_the_same_upward_search() {
        let mut cfg = default_config();
        cfg.account_platform_id.clear();
        cfg.account_user_id.clear();
        cfg.account_device_api_key.clear();
        cfg.allow_model_equivalence = true;
        cfg.model_compatibility_profiles.clear();
        cfg.channels = vec![local_model_channel(
            &["gpt-5.6-sol"],
            "http://127.0.0.1:9/v1",
        )];

        let selected = select_local_channel_route_where(
            &cfg,
            "/anthropic/v1/messages",
            br#"{"model":"claude-haiku-4-5","max_tokens":2,"messages":[{"role":"user","content":"."}]}"#,
            |_| true,
        )
        .expect("ordinary higher compatibility route");
        assert_eq!(selected.route_model, "gpt-5.6-sol");
        assert!(selected.substituted);
    }

    #[tokio::test]
    async fn local_failure_is_not_replaced_by_platform_failure_when_logged_out() {
        let local = warp::any().map(|| {
            warp::reply::with_status(
                warp::reply::json(&serde_json::json!({
                    "error": {"message": "local upstream failed"}
                })),
                StatusCode::BAD_GATEWAY,
            )
        });
        let (local_addr, local_server) = crate::bind_ephemeral!(local, ([127, 0, 0, 1], 0));
        tokio::spawn(local_server);

        let mut cfg = default_config();
        cfg.account_device_api_key.clear();
        cfg.prefer_local_supply = false;
        cfg.endpoints = vec![Endpoint {
            server_id: String::new(),
            name: "offline-platform".to_string(),
            base_url: "http://127.0.0.1:9".to_string(),
            supplier_ws_url: String::new(),
            supplier_quic_url: String::new(),
            enabled: true,
        }];
        let mut local_channel =
            local_model_channel(&["local-model"], &format!("http://{local_addr}/v1"));
        local_channel.capability_profiles = vec![ChannelCapabilityProfile {
            protocol: "openai_chat".to_string(),
            non_stream_json: true,
            verification_state: "verified".to_string(),
            ..Default::default()
        }];
        cfg.channels = vec![local_channel];
        let shared = Arc::new(ProxyShared::from_config(
            normalize_config(cfg),
            Client::new(),
        ));

        let response = forward_with_failover(
            warp::http::Method::POST,
            "/v1/chat/completions",
            "",
            HeaderMap::new(),
            bytes::Bytes::from_static(br#"{"model":"LOCAL-MODEL","messages":[]}"#),
            &shared,
        )
        .await
        .expect("local error response");
        let status = response.status();
        let body = crate::test_body_bytes(response.into_body())
            .await
            .expect("local error body");

        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(String::from_utf8_lossy(&body).contains("local upstream failed"));
    }

    #[tokio::test]
    async fn platform_failure_falls_back_to_ready_local_channel_when_local_is_not_preferred() {
        let platform_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let platform_calls_filter = platform_calls.clone();
        let platform = warp::path::full().map(move |path: warp::path::FullPath| {
            if path.as_str() == "/healthz" {
                return warp::reply::with_status("ok", StatusCode::OK);
            }
            platform_calls_filter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            warp::reply::with_status("platform unavailable", StatusCode::BAD_GATEWAY)
        });
        let (platform_addr, platform_server) =
            crate::bind_ephemeral!(platform, ([127, 0, 0, 1], 0));
        tokio::spawn(platform_server);

        let local_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let local_calls_filter = local_calls.clone();
        let local = warp::any().map(move || {
            local_calls_filter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            warp::reply::json(&serde_json::json!({
                "id": "local",
                "choices": [{"message": {"role": "assistant", "content": "local fallback"}}]
            }))
        });
        let (local_addr, local_server) = crate::bind_ephemeral!(local, ([127, 0, 0, 1], 0));
        tokio::spawn(local_server);

        let mut cfg = default_config();
        cfg.account_device_api_key = "sk-platform-device".to_string();
        cfg.prefer_local_supply = false;
        cfg.endpoints = vec![Endpoint {
            server_id: String::new(),
            name: "platform".to_string(),
            base_url: format!("http://{platform_addr}"),
            supplier_ws_url: String::new(),
            supplier_quic_url: String::new(),
            enabled: true,
        }];
        cfg.channels = vec![local_model_channel(
            &["local-model"],
            &format!("http://{local_addr}/v1"),
        )];
        let shared = Arc::new(ProxyShared::from_config(
            normalize_config(cfg),
            Client::new(),
        ));

        let response = forward_with_failover(
            warp::http::Method::POST,
            "/v1/chat/completions",
            "",
            HeaderMap::new(),
            bytes::Bytes::from_static(br#"{"model":"LOCAL-MODEL","messages":[]}"#),
            &shared,
        )
        .await
        .expect("local fallback response");
        let body = crate::test_body_bytes(response.into_body())
            .await
            .expect("local fallback body");

        assert!(String::from_utf8_lossy(&body).contains("local fallback"));
        assert_eq!(platform_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(local_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn forward_once_negotiates_compressed_platform_responses() {
        let seen_encoding = Arc::new(std::sync::Mutex::new(None::<String>));
        let seen_encoding_filter = seen_encoding.clone();
        let platform_models = warp::path!("v1" / "models")
            .and(warp::header::optional::<String>("accept-encoding"))
            .map(move |encoding: Option<String>| {
                *seen_encoding_filter.lock().expect("lock") = encoding;
                warp::reply::json(&serde_json::json!({
                    "object": "list",
                    "data": []
                }))
            });
        let (platform_addr, platform_server) =
            crate::bind_ephemeral!(platform_models, ([127, 0, 0, 1], 0));
        tokio::spawn(platform_server);

        let mut cfg = default_config();
        cfg.api_key = "sk-local-models".to_string();
        let endpoint = Endpoint {
            server_id: String::new(),
            name: "platform".to_string(),
            base_url: format!("http://{platform_addr}"),
            supplier_ws_url: String::new(),
            supplier_quic_url: String::new(),
            enabled: true,
        };
        let mut headers = HeaderMap::new();
        headers.insert("accept-encoding", "gzip, deflate".parse().expect("header"));

        let response = forward_once(
            warp::http::Method::GET,
            "/v1/models",
            "",
            false,
            headers,
            bytes::Bytes::new(),
            &Client::new(),
            &cfg,
            &endpoint,
            "",
        )
        .await
        .expect("forward");

        assert_eq!(response.status(), StatusCode::OK);
        let encoding = seen_encoding
            .lock()
            .expect("lock")
            .clone()
            .unwrap_or_default();
        assert!(encoding.contains("zstd"), "accept-encoding={encoding:?}");
        assert!(encoding.contains("gzip"), "accept-encoding={encoding:?}");
        assert!(!encoding.contains("identity"), "accept-encoding={encoding:?}");
    }

    #[tokio::test]
    async fn forward_once_owns_and_clamps_the_platform_attempt_budget() {
        let seen_budget = Arc::new(std::sync::Mutex::new(None::<String>));
        let seen_budget_filter = seen_budget.clone();
        let platform_models = warp::path!("v1" / "models")
            .and(warp::header::optional::<String>(
                "x-const-api-upstream-attempt-budget",
            ))
            .map(move |budget: Option<String>| {
                *seen_budget_filter.lock().expect("lock") = budget;
                warp::reply::json(&serde_json::json!({
                    "object": "list",
                    "data": []
                }))
            });
        let (platform_addr, platform_server) =
            crate::bind_ephemeral!(platform_models, ([127, 0, 0, 1], 0));
        tokio::spawn(platform_server);

        let cfg = default_config();
        let endpoint = Endpoint {
            server_id: String::new(),
            name: "platform".to_string(),
            base_url: format!("http://{platform_addr}"),
            supplier_ws_url: String::new(),
            supplier_quic_url: String::new(),
            enabled: true,
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-const-api-upstream-attempt-budget",
            "99".parse().expect("caller budget"),
        );

        let response = forward_once(
            warp::http::Method::GET,
            "/v1/models",
            "",
            false,
            headers,
            bytes::Bytes::new(),
            &Client::new(),
            &cfg,
            &endpoint,
            "",
        )
        .await
        .expect("forward");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            seen_budget.lock().expect("lock").as_deref(),
            Some("3")
        );
    }

    #[test]
    fn platform_request_compression_uses_zstd_for_large_json() {
        let raw = serde_json::json!({
            "model": "gpt-test",
            "messages": [{"role": "user", "content": "compressible request content ".repeat(512)}]
        })
        .to_string();
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );

        let compressed = compress_platform_request_body(
            &mut headers,
            bytes::Bytes::copy_from_slice(raw.as_bytes()),
        )
        .expect("compress request");

        assert_eq!(
            headers
                .get(reqwest::header::CONTENT_ENCODING)
                .and_then(|value| value.to_str().ok()),
            Some("zstd")
        );
        assert!(compressed.len() < raw.len());
        let decoded = zstd::bulk::decompress(&compressed, raw.len()).expect("decompress request");
        assert_eq!(decoded, raw.as_bytes());
    }

    #[test]
    fn platform_request_compression_skips_small_json() {
        let raw = bytes::Bytes::from_static(br#"{"model":"gpt-test"}"#);
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );

        let forwarded = compress_platform_request_body(&mut headers, raw.clone())
            .expect("inspect request");

        assert_eq!(forwarded, raw);
        assert!(headers.get(reqwest::header::CONTENT_ENCODING).is_none());
    }

    #[tokio::test]
    async fn forward_once_sends_large_json_as_zstd() {
        let seen_request = Arc::new(std::sync::Mutex::new(None::<(String, bytes::Bytes)>));
        let seen_request_filter = seen_request.clone();
        let platform = warp::path!("v1" / "responses")
            .and(warp::header::optional::<String>("content-encoding"))
            .and(warp::body::bytes())
            .map(move |encoding: Option<String>, body: bytes::Bytes| {
                *seen_request_filter.lock().expect("lock") =
                    Some((encoding.unwrap_or_default(), body));
                warp::reply::json(&serde_json::json!({"ok": true}))
            });
        let (platform_addr, platform_server) =
            crate::bind_ephemeral!(platform, ([127, 0, 0, 1], 0));
        tokio::spawn(platform_server);

        let raw = serde_json::json!({
            "model": "gpt-test",
            "input": "large compressible platform request ".repeat(512)
        })
        .to_string();
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/json".parse().expect("header"));
        let cfg = default_config();
        let endpoint = Endpoint {
            server_id: String::new(),
            name: "platform".to_string(),
            base_url: format!("http://{platform_addr}"),
            supplier_ws_url: String::new(),
            supplier_quic_url: String::new(),
            enabled: true,
        };

        let response = forward_once(
            warp::http::Method::POST,
            "/v1/responses",
            "",
            false,
            headers,
            bytes::Bytes::copy_from_slice(raw.as_bytes()),
            &Client::new(),
            &cfg,
            &endpoint,
            "",
        )
        .await
        .expect("forward");
        assert_eq!(response.status(), StatusCode::OK);

        let (encoding, compressed) = seen_request
            .lock()
            .expect("lock")
            .clone()
            .expect("captured request");
        assert_eq!(encoding, "zstd");
        let decoded = zstd::bulk::decompress(&compressed, raw.len()).expect("decompress request");
        assert_eq!(decoded, raw.as_bytes());
    }

    #[tokio::test]
    async fn forward_once_auto_decompresses_zstd_response() {
        let raw = bytes::Bytes::from(
            serde_json::json!({"data": "compressed platform response ".repeat(512)}).to_string(),
        );
        let compressed = bytes::Bytes::from(
            zstd::bulk::compress(&raw, 5).expect("compress platform response"),
        );
        let platform = warp::path!("v1" / "models").map(move || {
            let chunk = compressed.clone();
            let stream = futures_util::stream::once(async move {
                Ok::<bytes::Bytes, Infallible>(chunk)
            });
            let mut response = crate::test_stream_response(stream);
            response.headers_mut().insert(
                "content-type",
                "application/json".parse().expect("content type"),
            );
            response.headers_mut().insert(
                "content-encoding",
                "zstd".parse().expect("content encoding"),
            );
            response
        });
        let (platform_addr, platform_server) =
            crate::bind_ephemeral!(platform, ([127, 0, 0, 1], 0));
        tokio::spawn(platform_server);

        let cfg = default_config();
        let endpoint = Endpoint {
            server_id: String::new(),
            name: "platform".to_string(),
            base_url: format!("http://{platform_addr}"),
            supplier_ws_url: String::new(),
            supplier_quic_url: String::new(),
            enabled: true,
        };
        let response = forward_once(
            warp::http::Method::GET,
            "/v1/models",
            "",
            false,
            HeaderMap::new(),
            bytes::Bytes::new(),
            &Client::new(),
            &cfg,
            &endpoint,
            "",
        )
        .await
        .expect("forward");

        assert!(response.headers().get("content-encoding").is_none());
        let body = crate::test_body_bytes(response.into_body())
            .await
            .expect("response body");
        assert_eq!(body, raw);
    }

    #[tokio::test]
    async fn forward_once_streams_requested_response_without_buffering_for_json_content_type() {
        let platform_stream = warp::path!("v1" / "responses").map(|| {
            let stream = futures_util::stream::unfold(0, |state| async move {
                match state {
                    0 => Some((
                        Ok::<bytes::Bytes, Infallible>(bytes::Bytes::from_static(
                            b"data: first\n\n",
                        )),
                        1,
                    )),
                    1 => {
                        tokio::time::sleep(Duration::from_millis(800)).await;
                        Some((
                            Ok::<bytes::Bytes, Infallible>(bytes::Bytes::from_static(
                                b"data: second\n\n",
                            )),
                            2,
                        ))
                    }
                    _ => None,
                }
            });
            warp::http::Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/json")
                .body(crate::test_stream_response(stream).into_body())
                .expect("stream response")
        });
        let (platform_addr, platform_server) =
            crate::bind_ephemeral!(platform_stream, ([127, 0, 0, 1], 0));
        tokio::spawn(platform_server);

        let mut cfg = default_config();
        cfg.api_key = "sk-local-models".to_string();
        let endpoint = Endpoint {
            server_id: String::new(),
            name: "platform".to_string(),
            base_url: format!("http://{platform_addr}"),
            supplier_ws_url: String::new(),
            supplier_quic_url: String::new(),
            enabled: true,
        };

        let response = tokio::time::timeout(
            Duration::from_millis(400),
            forward_once(
                warp::http::Method::POST,
                "/v1/responses",
                "",
                false,
                HeaderMap::new(),
                bytes::Bytes::from_static(br#"{"model":"gpt-test","stream":true}"#),
                &Client::new(),
                &cfg,
                &endpoint,
                "",
            ),
        )
        .await
        .expect("streaming response should return before the tail chunk")
        .expect("forward");

        let mut body = response.into_body().into_data_stream();
        let first = body
            .next()
            .await
            .expect("first chunk")
            .expect("first chunk ok");
        assert_eq!(first, bytes::Bytes::from_static(b"data: first\n\n"));
    }

    #[tokio::test]
    async fn response_from_target_as_inbound_streams_native_sse_without_buffering() {
        let upstream_stream = warp::path!("v1" / "responses").map(|| {
            let stream = futures_util::stream::unfold(0, |state| async move {
                match state {
                    0 => Some((
                        Ok::<bytes::Bytes, Infallible>(bytes::Bytes::from_static(
                            b"event: response.output_text.delta\ndata: {\"delta\":\"first\"}\n\n",
                        )),
                        1,
                    )),
                    1 => {
                        tokio::time::sleep(Duration::from_millis(800)).await;
                        Some((
                            Ok::<bytes::Bytes, Infallible>(bytes::Bytes::from_static(
                                b"event: response.completed\ndata: {}\n\n",
                            )),
                            2,
                        ))
                    }
                    _ => None,
                }
            });
            warp::http::Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/event-stream")
                .body(crate::test_stream_response(stream).into_body())
                .expect("stream response")
        });
        let (upstream_addr, upstream_server) =
            crate::bind_ephemeral!(upstream_stream, ([127, 0, 0, 1], 0));
        tokio::spawn(upstream_server);
        let upstream = Client::new()
            .post(format!("http://{upstream_addr}/v1/responses"))
            .send()
            .await
            .expect("upstream response");

        let response = tokio::time::timeout(
            Duration::from_millis(400),
            response_from_target_as_inbound(
                upstream,
                "openai_responses",
                "openai_responses",
                "gpt-test",
                true,
                Default::default(),
            ),
        )
        .await
        .expect("local channel stream should return before the tail chunk")
        .expect("response");

        let mut body = response.into_body().into_data_stream();
        let first = body
            .next()
            .await
            .expect("first chunk")
            .expect("first chunk ok");
        assert_eq!(
            first,
            bytes::Bytes::from_static(
                b"event: response.output_text.delta\ndata: {\"delta\":\"first\"}\n\n"
            )
        );
    }

    #[tokio::test]
    async fn response_from_target_as_inbound_attaches_exact_model_failure_evidence() {
        let upstream_error = warp::path!("v1" / "responses").map(|| {
            warp::reply::with_status(
                warp::reply::json(&serde_json::json!({
                    "error": {
                        "code": "model_not_found",
                        "message": "requested model is unavailable"
                    }
                })),
                StatusCode::NOT_FOUND,
            )
        });
        let (upstream_addr, upstream_server) =
            crate::bind_ephemeral!(upstream_error, ([127, 0, 0, 1], 0));
        tokio::spawn(upstream_server);
        let upstream = Client::new()
            .post(format!("http://{upstream_addr}/v1/responses"))
            .send()
            .await
            .expect("upstream response");

        let response = response_from_target_as_inbound(
            upstream,
            "anthropic_messages",
            "openai_responses",
            "gpt-missing",
            false,
            Default::default(),
        )
        .await
        .expect("response");
        let evidence = response
            .extensions()
            .get::<LocalModelRouteFailureEvidence>()
            .expect("model failure evidence");
        assert_eq!(evidence.failure_model, "gpt-missing");
    }

    #[tokio::test]
    async fn native_passthrough_attaches_exact_model_failure_evidence() {
        let upstream_error = warp::path!("v1" / "responses").map(|| {
            warp::reply::with_status(
                warp::reply::json(&serde_json::json!({
                    "error": {
                        "type": "model_not_supported",
                        "message": "model is not supported"
                    }
                })),
                StatusCode::NOT_FOUND,
            )
        });
        let (upstream_addr, upstream_server) =
            crate::bind_ephemeral!(upstream_error, ([127, 0, 0, 1], 0));
        tokio::spawn(upstream_server);
        let upstream = Client::new()
            .post(format!("http://{upstream_addr}/v1/responses"))
            .send()
            .await
            .expect("upstream response");

        let response = response_from_reqwest_with_upstream_model(
            upstream,
            false,
            Some("gpt-native-missing"),
        )
        .await
        .expect("response");
        let evidence = response
            .extensions()
            .get::<LocalModelRouteFailureEvidence>()
            .expect("model failure evidence");
        assert_eq!(evidence.failure_model, "gpt-native-missing");
    }

    #[tokio::test]
    async fn response_from_target_as_inbound_bridges_native_json_to_requested_stream() {
        let upstream_json = warp::path!("v1" / "responses").map(|| {
            warp::reply::with_header(
                serde_json::json!({
                    "id": "resp_1",
                    "object": "response",
                    "status": "completed",
                    "model": "gpt-test",
                    "output": [{
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "json hello"}]
                    }]
                })
                .to_string(),
                "content-type",
                "application/json",
            )
        });
        let (upstream_addr, upstream_server) =
            crate::bind_ephemeral!(upstream_json, ([127, 0, 0, 1], 0));
        tokio::spawn(upstream_server);
        let upstream = Client::new()
            .post(format!("http://{upstream_addr}/v1/responses"))
            .send()
            .await
            .expect("upstream response");

        let response = response_from_target_as_inbound(
            upstream,
            "openai_responses",
            "openai_responses",
            "gpt-test",
            true,
            Default::default(),
        )
        .await
        .expect("response");
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = crate::test_body_bytes(response.into_body())
            .await
            .expect("body");
        let body = String::from_utf8_lossy(&body);

        assert!(content_type.contains("text/event-stream"));
        assert!(body.contains("response.output_text.delta"), "{body}");
        assert!(body.contains("json hello"), "{body}");
        assert!(body.contains("response.completed"), "{body}");
    }

    #[tokio::test]
    async fn response_from_target_as_inbound_streams_chat_sse_to_gemini_sse() {
        let upstream_stream = warp::path!("v1" / "chat" / "completions").map(|| {
            let stream = futures_util::stream::iter(vec![Ok::<bytes::Bytes, Infallible>(
                bytes::Bytes::from_static(
                    b"data: {\"id\":\"chatcmpl_1\",\"model\":\"gpt-test\",\"choices\":[{\"delta\":{\"content\":\"hello gemini\"},\"finish_reason\":null}]}\n\n\
data: {\"id\":\"chatcmpl_1\",\"model\":\"gpt-test\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
data: [DONE]\n\n",
                ),
            )]);
            warp::http::Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/event-stream")
                .body(crate::test_stream_response(stream).into_body())
                .expect("stream response")
        });
        let (upstream_addr, upstream_server) =
            crate::bind_ephemeral!(upstream_stream, ([127, 0, 0, 1], 0));
        tokio::spawn(upstream_server);
        let upstream = Client::new()
            .post(format!("http://{upstream_addr}/v1/chat/completions"))
            .send()
            .await
            .expect("upstream response");

        let response = response_from_target_as_inbound(
            upstream,
            "gemini_native",
            "openai_chat",
            "gemini-test",
            true,
            Default::default(),
        )
        .await
        .expect("response");
        let body = crate::test_body_bytes(response.into_body())
            .await
            .expect("body");
        let body = String::from_utf8_lossy(&body);

        assert!(body.contains("\"candidates\""), "{body}");
        assert!(body.contains("hello gemini"), "{body}");
        assert!(!body.contains("chat.completion.chunk"), "{body}");
    }

    #[test]
    fn response_from_body_preserves_content_encoding_when_body_is_still_encoded() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            "application/json".parse().expect("content type"),
        );
        headers.insert(
            reqwest::header::CONTENT_ENCODING,
            "gzip".parse().expect("content encoding"),
        );
        headers.insert(
            reqwest::header::CONTENT_LENGTH,
            "1234".parse().expect("content length"),
        );

        let response = response_from_body(
            StatusCode::OK,
            &headers,
            bytes::Bytes::from_static(&[0x1f, 0x8b, 0x08, 0x00]),
        )
        .expect("response");

        assert_eq!(
            response
                .headers()
                .get("content-encoding")
                .and_then(|value| value.to_str().ok()),
            Some("gzip")
        );
        assert!(
            response.headers().get("content-length").is_none(),
            "content-length is unsafe after proxy body rebuilding"
        );
    }

    #[test]
    fn raw_proxy_debug_headers_keep_credentials() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer secret".parse().expect("auth"));
        headers.insert("x-api-key", "sk-secret".parse().expect("api key"));
        headers.insert("cookie", "session=secret".parse().expect("cookie"));

        let raw = raw_proxy_headers(&headers);

        assert_eq!(raw["authorization"], "Bearer secret");
        assert_eq!(raw["x-api-key"], "sk-secret");
        assert_eq!(raw["cookie"], "session=secret");
    }

    #[test]
    fn raw_proxy_debug_headers_redact_credentials() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer secret".parse().expect("auth"));
        headers.insert("x-api-key", "sk-secret".parse().expect("api key"));
        headers.insert("cookie", "session=secret".parse().expect("cookie"));
        headers.insert(
            "content-type",
            "application/json".parse().expect("content type"),
        );

        let sanitized = sanitized_proxy_headers(&headers);

        assert_eq!(sanitized["authorization"], "<redacted>");
        assert_eq!(sanitized["x-api-key"], "<redacted>");
        assert_eq!(sanitized["cookie"], "<redacted>");
        assert_eq!(sanitized["content-type"], "application/json");
    }

    #[test]
    fn raw_proxy_logger_rebuilds_response_body_without_legacy_file() {
        let _guard = crate::tool_config::test_home_lock()
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let previous = crate::tool_config::set_test_home_override(Some(dir.path().to_path_buf()));
        let response = warp::http::Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/event-stream")
            .body(bytes::Bytes::from_static(b"data: {\"ok\":true}\n\ndata: [DONE]\n\n").into())
            .expect("response");

        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        let (status, content_type, body) = runtime.block_on(async {
            let rebuilt = append_raw_proxy_exchange_log_and_rebuild_response(
                &warp::http::Method::POST,
                "/v1/chat/completions",
                "",
                &HeaderMap::new(),
                br#"{"model":"gpt-test"}"#,
                response,
            )
            .await;
            let status = rebuilt.status();
            let content_type = rebuilt
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_string();
            let body = crate::test_body_bytes(rebuilt.into_body())
                .await
                .expect("body");
            (status, content_type, body)
        });

        assert_eq!(status, StatusCode::OK);
        assert!(content_type.contains("text/event-stream"));
        assert_eq!(
            String::from_utf8_lossy(&body),
            "data: {\"ok\":true}\n\ndata: [DONE]\n\n"
        );
        assert!(!dir
            .path()
            .join(".const-api")
            .join("debug")
            .join("raw-traffic.jsonl")
            .exists());
        crate::tool_config::set_test_home_override(previous);
    }

    #[test]
    fn recent_request_status_stays_in_memory_for_the_active_client_scope() {
        let headers = warp::http::HeaderMap::new();
        write_local_proxy_recent_request_status(
            &warp::http::Method::POST,
            "/v1/responses",
            &headers,
            br#"{"model":"gpt-memory"}"#,
            StatusCode::OK,
        )
        .expect("record recent request");
        let status = local_proxy_recent_request_status().expect("recent request status");
        assert_eq!(status["path"], "/v1/responses");
        assert_eq!(status["model"], "gpt-memory");
    }

    #[test]
    fn runtime_platform_key_is_independent_and_updateable_without_proxy_restart() {
        let mut cfg = default_config();
        cfg.api_key = "local-fixed-key".to_string();
        cfg.account_device_api_key.clear();
        let shared = ProxyShared::from_config(cfg, Client::new());

        assert!(shared.platform_api_key.lock().expect("key lock").is_empty());

        *shared.platform_api_key.lock().expect("key lock") = "platform-device-key".to_string();
        assert_eq!(
            shared.platform_api_key.lock().expect("key lock").as_str(),
            "platform-device-key"
        );
    }

    #[test]
    fn local_surface_manifest_publishes_only_the_three_canonical_mounts() {
        let manifest = crate::surface::surface_manifest();
        assert_eq!(manifest["version"], 3);
        assert_eq!(
            manifest["surfaces"]
                .as_array()
                .unwrap()
                .iter()
                .map(|surface| surface["mount"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["/v1", "/anthropic", "/gemini"]
        );
        assert_eq!(
            manifest["surfaces"][0]["protocols"],
            serde_json::json!(["openai_responses", "openai_chat"])
        );
        assert!(is_local_proxy_api_path("/anthropic/v1/messages"));
        assert!(is_local_proxy_api_path(
            "/gemini/v1beta/models/gemini-test:generateContent"
        ));
        assert!(!is_local_proxy_api_path("/v1beta/models/gemini-test"));
    }

    #[tokio::test]
    async fn local_root_explains_standard_surfaces_without_auth_or_backend_secrets() {
        let shared = Arc::new(ProxyShared::from_config(default_config(), Client::new()));
        let html = warp::test::request()
            .method("GET")
            .path("/")
            .reply(&crate::proxy_routes(shared.clone()))
            .await;
        assert_eq!(html.status(), StatusCode::OK);
        assert!(html.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/html"));
        assert_eq!(html.headers()["cache-control"], "no-store");
        assert!(html.headers().contains_key("content-security-policy"));
        let html_body = String::from_utf8_lossy(html.body());
        for expected in ["/v1", "/anthropic", "/gemini", "/.well-known/const-api"] {
            assert!(html_body.contains(expected), "{expected}");
        }
        assert!(!html_body.contains("token"));
        assert!(!html_body.contains("cookie"));

        let json = warp::test::request()
            .method("GET")
            .path("/")
            .header("accept", "application/json")
            .reply(&crate::proxy_routes(shared.clone()))
            .await;
        assert_eq!(json.status(), StatusCode::OK);
        let document: serde_json::Value = serde_json::from_slice(json.body()).unwrap();
        assert_eq!(document["name"], "CONST API");
        assert_eq!(document["surfaces"][0]["base_path"], "/v1");
        assert_eq!(
            document["execution_backends"][1]["id"],
            "subscription_driver"
        );
        assert_eq!(document["execution_backends"][1]["public_protocol"], false);
        assert_eq!(document["management_api"]["exposed"], false);

        let method = warp::test::request()
            .method("POST")
            .path("/")
            .reply(&crate::proxy_routes(shared))
            .await;
        assert_eq!(method.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(method.headers()["allow"], "GET");
    }

    #[tokio::test]
    async fn management_paths_fail_closed_before_authentication_or_forwarding() {
        let shared = Arc::new(ProxyShared::from_config(default_config(), Client::new()));
        for path in [
            "/v1/organization/audit_logs",
            "/v1/organization/projects/project_1",
            "/v1/projects/project_1/api_keys",
        ] {
            let response = warp::test::request()
                .method("GET")
                .path(path)
                .reply(&crate::proxy_routes(shared.clone()))
                .await;
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
            assert_eq!(
                response.headers()["x-const-error-code"],
                "management_surface_not_exposed",
                "{path}"
            );
            let payload: serde_json::Value = serde_json::from_slice(response.body()).unwrap();
            assert!(
                payload["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("independent management endpoint"),
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn platform_only_model_catalog_normalizes_live_and_cached_responses() {
        let offline = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let offline_filter = offline.clone();
        let platform = warp::path::full().map(move |path: warp::path::FullPath| {
            if offline_filter.load(std::sync::atomic::Ordering::SeqCst) {
                return warp::reply::with_status("unavailable", StatusCode::SERVICE_UNAVAILABLE).into_response();
            }
            let payload = if path.as_str().contains("gemini") {
                serde_json::json!({"models": [
                    {"name": "models/Vendor/Claude-Sonnet-4-6"},
                    {"name": "models/Other/claude-sonnet-4-6"}
                ]})
            } else {
                serde_json::json!({"data": [
                    {"id": "Vendor/Claude-Sonnet-4-6"},
                    {"id": "Other/claude-sonnet-4-6"}
                ]})
            };
            warp::reply::json(&payload).into_response()
        });
        let (address, server) = crate::bind_ephemeral!(platform, ([127, 0, 0, 1], 0));
        let server_task = tokio::spawn(server);
        let mut config = default_config();
        config.account_device_api_key = "sk-platform".into();
        config.prefer_local_supply = false;
        config.allow_model_equivalence = false;
        config.channels.clear();
        config.supplier.enabled = false;
        config.endpoints = vec![Endpoint {
            server_id: String::new(),
            name: "platform".into(), base_url: format!("http://{address}"),
            supplier_ws_url: String::new(), supplier_quic_url: String::new(), enabled: true,
        }];
        let shared = Arc::new(ProxyShared::from_config(config, Client::new()));
        for cached in [false, true] {
            offline.store(cached, std::sync::atomic::Ordering::SeqCst);
            for (surface, path, key) in [
                (crate::surface::ApiSurface::OpenAi, "/v1/models", "data"),
                (crate::surface::ApiSurface::Anthropic, "/anthropic/v1/models", "data"),
                (crate::surface::ApiSurface::Gemini, "/gemini/v1beta/models", "models"),
            ] {
                let response = forward_with_failover(warp::http::Method::GET, path, "", HeaderMap::new(), bytes::Bytes::new(), &shared).await.unwrap();
                assert_eq!(response.headers().contains_key(MODEL_CATALOG_CACHE_HEADER), cached);
                let body = crate::test_body_bytes(response.into_body()).await.unwrap();
                let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let ids = payload[key].as_array().unwrap().iter()
                    .filter_map(|entry| model_id_for_surface(surface, entry)).collect::<Vec<_>>();
                assert_eq!(ids, ["claude-sonnet-4-6"], "{path}, cached={cached}");
            }
        }
        server_task.abort();
    }

    #[test]
    fn realtime_call_local_route_follows_channel_priority_across_backend_kinds() {
        let subscription = openai_subscription_channel(&["gpt-realtime-1.5"]);
        let api = openai_api_channel(
            "openai-api-realtime",
            "gpt-realtime-2.1",
            "https://api.openai.com",
        );
        let mut config = default_config();
        config.channels = vec![subscription.clone(), api.clone()];

        let selected = select_openai_realtime_call_channel_where(&config, "", |_| true)
            .expect("subscription route");
        assert_eq!(selected.id, subscription.id);

        let selected = select_openai_realtime_call_channel_where(&config, "gpt-realtime-2.1", |_| true)
            .expect("native public Realtime route");
        assert_eq!(selected.id, api.id, "a public model must not be silently mapped to private GPT-Live");

        config.channels.reverse();
        let selected = select_openai_realtime_call_channel_where(&config, "", |_| true)
            .expect("API route");
        assert_eq!(selected.id, api.id);

        let route = crate::surface::resolve_api_route(
            "POST",
            "/v1/realtime/calls",
            false,
        )
        .expect("Realtime calls route");
        assert_eq!(route.operation, crate::surface::ApiOperation::RealtimeCallsCreate);
        assert!(crate::surface::api_operation_requires_local_backend(
            route.operation
        ));
        assert!(!local_response_marks_channel_unready_for_request(
            "POST",
            "/v1/realtime/calls",
            StatusCode::FORBIDDEN,
        ));
        let live = crate::surface::resolve_api_route("POST", "/v1/live", false)
            .expect("Codex Live call route");
        assert_eq!(
            live.operation,
            crate::surface::ApiOperation::RealtimeLiveCallCreate
        );
        assert!(crate::surface::api_operation_requires_local_backend(
            live.operation
        ));
        assert!(!local_response_marks_channel_unready_for_request(
            "POST",
            "/v1/live",
            StatusCode::FORBIDDEN,
        ));
        assert!(local_response_marks_channel_unready_for_request(
            "POST",
            "/v1/responses",
            StatusCode::FORBIDDEN,
        ));
    }
