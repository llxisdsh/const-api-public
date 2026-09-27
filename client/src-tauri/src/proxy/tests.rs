#[cfg(test)]
#[allow(clippy::result_large_err)] // tungstenite's server callback owns the external error type.
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use warp::http::HeaderMap;
    use warp::Filter;

    fn local_model_channel(models: &[&str], upstream_base_url: &str) -> ChannelConfig {
        let first = models.first().copied().unwrap_or_default().to_string();
        let source_driver = crate::source_driver::SourceDriverId::CustomEndpoint;
        ChannelConfig {
            v2: crate::channel_v2_contract_for_source(source_driver),
            id: "local-models".to_string(),
            enabled: true,
            share_enabled: true,
            kind: default_channel_kind(),
            api_format: default_channel_api_format(),
            node_id: "node-local-models".to_string(),
            name: "Local Models".to_string(),
            server_ws_url: String::new(),
            server_quic_url: String::new(),
            upstream_base_url: upstream_base_url.to_string(),
            upstream_api_key: String::new(),
            public_model: first.clone(),
            upstream_model: first,
            models: models.iter().map(|model| (*model).to_string()).collect(),
            supported_protocols: vec!["openai_chat".to_string()],
            capability_profiles: Vec::new(),
            detection_checks: Vec::new(),
            surface_bindings: crate::source_driver_surface_bindings(
                source_driver,
                upstream_base_url,
            ),
            price_ratio: 1.0,
            subscription: default_subscription_adapter_config(),
        }
    }

    fn openai_subscription_channel(models: &[&str]) -> ChannelConfig {
        let mut channel = local_model_channel(models, "local://subscription/openai");
        channel.id = "openai-account-subscription".to_string();
        channel.name = "OpenAI Account Subscription".to_string();
        channel.set_source_driver(crate::source_driver::SourceDriverId::OpenAiSubscription);
        channel.upstream_base_url = "local://subscription/openai".to_string();
        channel.api_format = "openai_responses".to_string();
        channel.supported_protocols = vec!["openai_responses".to_string()];
        channel.surface_bindings = crate::source_driver_surface_bindings(
            crate::source_driver::SourceDriverId::OpenAiSubscription,
            "",
        );
        channel
    }

    fn openai_api_channel(id: &str, model: &str, upstream_base_url: &str) -> ChannelConfig {
        let mut channel = local_model_channel(&[model], upstream_base_url);
        channel.id = id.to_string();
        channel.name = id.to_string();
        channel.node_id = format!("node-{id}");
        channel.set_source_driver(crate::source_driver::SourceDriverId::OpenAiApi);
        channel.api_format = "openai_responses".to_string();
        channel.supported_protocols = vec![
            "openai_responses".to_string(),
            "openai_chat".to_string(),
        ];
        channel.surface_bindings = crate::source_driver_surface_bindings(
            crate::source_driver::SourceDriverId::OpenAiApi,
            upstream_base_url,
        );
        for binding in &mut channel.surface_bindings {
            binding.verification.state = "verified".to_string();
            for protocol in &mut binding.protocols {
                protocol.verification.state = "verified".to_string();
            }
        }
        channel
    }

    fn gemini_api_channel(id: &str, model: &str, upstream_base_url: &str) -> ChannelConfig {
        let mut channel = local_model_channel(&[model], upstream_base_url);
        channel.id = id.to_string();
        channel.name = id.to_string();
        channel.node_id = format!("node-{id}");
        channel.set_source_driver(crate::source_driver::SourceDriverId::GeminiApi);
        channel.api_format = "gemini_native".to_string();
        channel.supported_protocols = vec!["gemini_native".to_string()];
        channel.surface_bindings = crate::source_driver_surface_bindings(
            crate::source_driver::SourceDriverId::GeminiApi,
            upstream_base_url,
        );
        for binding in &mut channel.surface_bindings {
            binding.verification.state = "verified".to_string();
            for protocol in &mut binding.protocols {
                protocol.verification.state = "verified".to_string();
            }
        }
        channel
    }

    fn configure_model_compatibility_pair(
        config: &mut ClientConfig,
        requested_model: &str,
        compatible_model: &str,
    ) {
        config.account_platform_id = "platform-1".to_string();
        config.account_user_id = "user-1".to_string();
        config.allow_model_equivalence = true;
        config.model_compatibility_profiles.insert(
            model_compatibility_profile_key("platform-1", "user-1").expect("profile key"),
            LocalModelCompatibilityProfile {
                platform_id: "platform-1".to_string(),
                user_id: "user-1".to_string(),
                base_release_id: "release-1".to_string(),
                model_version_id: "models-1".to_string(),
                compatibility_version_id: "compat-1".to_string(),
                revision: 1,
                model_groups: vec![ModelCompatibilityGroupConfig {
                    id: "test-compatible".to_string(),
                    label: "Test compatible".to_string(),
                    description: String::new(),
                    aliases: Vec::new(),
                    models: vec![compatible_model.to_string()],
                    match_models: vec![
                        requested_model.to_string(),
                        compatible_model.to_string(),
                    ],
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
    }

    async fn forward_chat_json(
        shared: &Arc<ProxyShared>,
        model: &str,
        cache_identity: &str,
    ) -> serde_json::Value {
        let response = forward_with_failover(
            warp::http::Method::POST,
            "/v1/chat/completions",
            "",
            HeaderMap::new(),
            bytes::Bytes::from(
                serde_json::to_vec(&serde_json::json!({
                    "model": model,
                    "prompt_cache_key": cache_identity,
                    "messages": []
                }))
                .expect("chat request json"),
            ),
            shared,
        )
        .await
        .expect("chat response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = crate::test_body_bytes(response.into_body())
            .await
            .expect("chat response body");
        serde_json::from_slice(&body).expect("chat response json")
    }

    async fn assert_generation_route_preference(
        prefer_local: bool,
        expected_source: &str,
        expected_platform_calls: usize,
        expected_local_calls: usize,
    ) {
        let platform_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let platform_calls_filter = platform_calls.clone();
        let platform = warp::method().and(warp::path::full()).map(
            move |method: warp::http::Method, path: warp::path::FullPath| {
                if method == warp::http::Method::POST && path.as_str() == "/v1/chat/completions" {
                    platform_calls_filter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                warp::reply::json(&serde_json::json!({
                    "id": "platform",
                    "source": "platform",
                    "choices": []
                }))
            },
        );
        let (platform_addr, platform_server) =
            crate::bind_ephemeral!(platform, ([127, 0, 0, 1], 0));
        let platform_task = tokio::spawn(platform_server);

        let local_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let local_calls_filter = local_calls.clone();
        let local = warp::method().and(warp::path::full()).map(
            move |method: warp::http::Method, path: warp::path::FullPath| {
                if method == warp::http::Method::POST && path.as_str() == "/v1/chat/completions" {
                    local_calls_filter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                warp::reply::json(&serde_json::json!({
                    "id": "local",
                    "source": "local",
                    "choices": []
                }))
            },
        );
        let (local_addr, local_server) = crate::bind_ephemeral!(local, ([127, 0, 0, 1], 0));
        let local_task = tokio::spawn(local_server);

        let mut cfg = default_config();
        cfg.account_device_api_key = "sk-platform-device".to_string();
        cfg.prefer_local_supply = prefer_local;
        cfg.endpoints = vec![Endpoint {
            server_id: String::new(),
            name: "platform".to_string(),
            base_url: format!("http://{platform_addr}"),
            supplier_ws_url: String::new(),
            supplier_quic_url: String::new(),
            enabled: true,
        }];
        cfg.channels = vec![local_model_channel(
            &["shared-model"],
            &format!("http://{local_addr}/v1"),
        )];
        let cfg = normalize_config(cfg);
        let readiness = Arc::new(StdMutex::new(HashMap::from([(
            cfg.channels[0].id.clone(),
            true,
        )])));
        let platform_api_key = Arc::new(StdMutex::new("sk-platform-device".to_string()));
        let shared = Arc::new(ProxyShared::with_platform_api_key(
            cfg.clone(),
            Client::new(),
            platform_api_key,
            Arc::new(StdMutex::new(cfg)),
            readiness,
        ));

        let response = forward_with_failover(
            warp::http::Method::POST,
            "/v1/chat/completions",
            "",
            HeaderMap::new(),
            bytes::Bytes::from_static(br#"{"model":"SHARED-MODEL","messages":[]}"#),
            &shared,
        )
        .await
        .expect("preferred route response");
        let body = crate::test_body_bytes(response.into_body())
            .await
            .expect("preferred route body");
        let payload: serde_json::Value =
            serde_json::from_slice(&body).expect("preferred route json");

        assert_eq!(payload["source"], expected_source);
        assert_eq!(
            platform_calls.load(std::sync::atomic::Ordering::SeqCst),
            expected_platform_calls
        );
        assert_eq!(
            local_calls.load(std::sync::atomic::Ordering::SeqCst),
            expected_local_calls
        );

        platform_task.abort();
        local_task.abort();
    }

    #[test]
    fn platform_restriction_allows_local_stage_and_queues_one_notice() {
        crate::client_toasts::clear_client_toast_notices();
        let response = warp::reply::with_header(
            warp::reply::with_header(
                warp::reply::with_header(
                    warp::reply::with_status("restricted", StatusCode::FORBIDDEN),
                    PLATFORM_MODEL_FALLBACK_SAFE_HEADER,
                    "1",
                ),
                "X-Const-Error-Code",
                "platform_access_restricted",
            ),
            "X-Const-Platform-Access-Until",
            "2026-08-30T12:10:00Z",
        )
        .into_response();
        if !matches!(
            classify_platform_model_response(response),
            ModelRouteAttempt::LocalOnlyResponse(_)
        ) {
            panic!("platform restriction was not limited to the local fallback stage");
        }
        let notices = crate::client_toasts::drain_client_toast_notices_inner();
        assert_eq!(notices.len(), 1);
        let notice = serde_json::to_value(&notices[0]).expect("serialize notice");
        assert_eq!(notice["code"], "platform_access_restricted");

		let mixed_version = warp::reply::with_header(
			warp::reply::with_status("blocked", StatusCode::BAD_GATEWAY),
			PLATFORM_ERROR_CODE_HEADER,
			"cyber_policy",
		)
		.into_response();
		assert!(matches!(
			classify_platform_model_response(mixed_version),
			ModelRouteAttempt::LocalOnlyResponse(_)
		));
        crate::client_toasts::clear_client_toast_notices();
    }

    include!("tests/surfaces.rs");
    include!("tests/voice_routing.rs");
    include!("tests/platform_voice.rs");

    include!("tests/routing.rs");
    include!("tests/model_discovery.rs");

    include!("tests/protocol_conversion.rs");

}
