//! Edition boundary, applied when loading or saving configuration.
pub(crate) fn require_antigravity_oauth_app() -> anyhow::Result<()> {
    anyhow::ensure!(
        !crate::ANTIGRAVITY_OAUTH_CLIENT_ID.is_empty()
            && !crate::ANTIGRAVITY_OAUTH_CLIENT_SECRET.is_empty(),
        "This local build does not include Antigravity OAuth application credentials. Browser sign-in and token refresh require CONST_LOCAL_ANTIGRAVITY_CLIENT_ID and CONST_LOCAL_ANTIGRAVITY_CLIENT_SECRET at build time; see SOURCE.md."
    );
    Ok(())
}

pub(crate) fn enforce(config: &mut crate::model::ClientConfig) {
    config.endpoints.clear();
    config.registry_sources.clear();
    config.registry_public_keys.clear();
    config.registry_signature_secret.clear();
    config.platform_id.clear();
    config.account_platform_id.clear();
    config.account_home_server_id.clear();
    config.account_home_base_url.clear();
    config.account_user_id.clear();
    config.account_email.clear();
    config.account_device_api_key.clear();
    config.automatic_updates = false;
    config.supplier_auto_start = false;
    config.prefer_local_supply = true;
    config.supplier.server_ws_url.clear();
    config.supplier.server_quic_url.clear();
    for channel in &mut config.channels {
        channel.share_enabled = false;
        channel.server_ws_url.clear();
        channel.server_quic_url.clear();
    }
}

#[cfg(test)]
mod tests {
    use http_body_util::BodyExt;
    use warp::Filter;

    #[tokio::test]
    async fn missing_antigravity_oauth_app_fails_before_authorization_or_exchange() {
        let configured = !crate::ANTIGRAVITY_OAUTH_CLIENT_ID.is_empty()
            && !crate::ANTIGRAVITY_OAUTH_CLIENT_SECRET.is_empty();
        assert_eq!(super::require_antigravity_oauth_app().is_ok(), configured);
        if !configured {
            let error = crate::create_subscription_oauth_session_data("antigravity").unwrap_err();
            assert!(error.to_string().contains("SOURCE.md"));
            let error = crate::exchange_antigravity_oauth_code("unused", "unused", "").await.unwrap_err();
            assert!(error.to_string().contains("SOURCE.md"));
        }
    }

    #[tokio::test]
    async fn hosted_transports_reject_even_explicit_endpoints_and_credentials() {
        use std::sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}};
        use crate::supplier::{SupplierAgentConfig, SupplierRunOutcome};
        let requests = Arc::new(AtomicUsize::new(0));
        let observed = requests.clone();
        let mock = warp::any().map(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            "unexpected hosted request"
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(warp::serve(mock).incoming(listener).run());
        let endpoint = crate::model::Endpoint {
            server_id: "mock-hosted".into(), name: "Mock hosted".into(), enabled: true,
            base_url: format!("http://{addr}"),
            supplier_ws_url: format!("ws://{addr}/supplier"),
            supplier_quic_url: String::new(),
        };
        let mut config = crate::config::default_config();
        // Bypass the normal config sanitizer deliberately: network entry points must refuse too.
        config.endpoints = vec![endpoint.clone()];
        config.account_device_api_key = "test-copied-device-key".into();
        let client = reqwest::Client::new();
        let transport = Arc::new(crate::platform_transport::PlatformHttpTransport::new(
            client.clone(), Arc::new(Mutex::new(Default::default())),
        ));
        transport.start_probe(endpoint.clone());
        assert!(transport.refresh_health_if_due(&endpoint, std::time::Duration::ZERO).await.is_err());
        assert!(transport.get(&endpoint, "/api/account/me", Default::default()).await.is_err());
        assert!(crate::proxy::forward_once(
            warp::http::Method::POST, "/v1/responses", "", false, Default::default(),
            bytes::Bytes::from_static(b"{}"), &client, &config, &endpoint, "test-copied-api-key",
        ).await.is_err());
        let mut agent = SupplierAgentConfig {
            client_id: "test-local".into(), session_id: "test-session".into(),
            suppliers: vec![config.supplier.clone()],
            server_ws_url: endpoint.supplier_ws_url.clone(), server_quic_url: String::new(),
            access_token: Arc::new(Mutex::new("test-copied-access-token".into())),
            account_refresh_notify: Arc::new(tokio::sync::Notify::new()),
        };
        let (_shutdown, mut shutdown) = tokio::sync::oneshot::channel();
        let (_updates, mut updates) = tokio::sync::mpsc::channel(1);
        let state = Arc::new(Mutex::new(Default::default()));
        assert_eq!(crate::supplier::run_supplier_websocket_connection(
            &client, &mut agent, &endpoint.supplier_ws_url, &mut shutdown, &mut updates, &state,
        ).await, SupplierRunOutcome::Shutdown);
        assert!(crate::supplier::connect_supplier_quic_endpoint("quic://127.0.0.1:1").await.is_err());
        assert!(crate::account::account_register().await.is_err());
        assert!(crate::account::AccountRuntime::new().access_token().is_none());
        tokio::task::yield_now().await;
        assert_eq!(requests.load(Ordering::SeqCst), 0);
        server.abort();
    }

    #[test]
    fn persisted_platform_settings_cannot_enable_the_local_edition() {
        let mut config = crate::config::default_config();
        config.account_device_api_key = "test-platform-key".into();
        config.account_home_base_url = "https://example.invalid".into();
        config.automatic_updates = true;
        config.supplier_auto_start = true;
        config.registry_sources = vec!["https://example.invalid/registry.json".into()];
        config.endpoints.push(crate::model::Endpoint {
            server_id: "fixture".into(),
            name: "fixture".into(),
            enabled: true,
            base_url: "https://example.invalid".into(),
            supplier_ws_url: "wss://example.invalid".into(),
            supplier_quic_url: String::new(),
        });
        let mut channel = crate::coding_gateway::tests::channel(
            crate::source_driver::SourceDriverId::OpencodeGo,
            "https://opencode.ai/zen/go/v1",
        );
        channel.share_enabled = true;
        channel.server_ws_url = "wss://example.invalid".into();
        config.channels = vec![channel];
        // Test the real persisted-config boundary, not just the policy helper.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("client.json");
        std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
        let config = crate::config::load_config_from_path(&path).unwrap();
        assert!(config.account_device_api_key.is_empty());
        assert!(config.account_home_base_url.is_empty());
        assert!(!config.automatic_updates && !config.supplier_auto_start);
        assert!(config.channels.iter().all(|channel| !channel.share_enabled));
        assert!(
            config
                .channels
                .iter()
                .all(|channel| channel.server_ws_url.is_empty())
        );
        assert_eq!(config.channels.len(), 1);
        assert!(config.endpoints.is_empty());
        assert!(config.registry_sources.is_empty());
        assert!(
            crate::account::AccountRuntime::new()
                .access_token()
                .is_none()
        );
        assert!(crate::config::client_data_root().ends_with(".const-api-local"));
    }

    #[tokio::test]
    async fn local_protocol_forwarding_preserves_tools_and_cache_without_an_account() {
        use crate::source_driver::SourceDriverId;
        let upstream = warp::post().and(warp::path::full()).and(warp::body::json())
            .map(|path: warp::path::FullPath, body: serde_json::Value| {
                let response = if path.as_str().ends_with("/messages") {
                    serde_json::json!({"id":"msg_local","type":"message","role":"assistant","model":body["model"],"content":[{"type":"tool_use","id":"call_local","name":"lookup","input":{"query":"ok"}}],"stop_reason":"tool_use","usage":{"input_tokens":20,"cache_read_input_tokens":100,"output_tokens":3}})
                } else if path.as_str().ends_with(":generateContent") {
                    serde_json::json!({"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"lookup","args":{"query":"ok"}}}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":120,"cachedContentTokenCount":100,"candidatesTokenCount":3,"totalTokenCount":123}})
                } else if path.as_str().ends_with("/responses") {
                    serde_json::json!({"id":"resp_local","object":"response","status":"completed","model":body["model"],"output":[{"type":"function_call","id":"fc_local","call_id":"call_local","name":"lookup","arguments":"{\"query\":\"ok\"}"}],"usage":{"input_tokens":120,"input_tokens_details":{"cached_tokens":100},"output_tokens":3}})
                } else {
                    serde_json::json!({"id":"chat_local","object":"chat.completion","model":body["model"],"choices":[{"index":0,"message":{"role":"assistant","tool_calls":[{"id":"call_local","type":"function","function":{"name":"lookup","arguments":"{\"query\":\"ok\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":120,"prompt_tokens_details":{"cached_tokens":100},"completion_tokens":3}})
                };
                warp::reply::json(&response)
            });
        let (address, server) = crate::bind_ephemeral!(upstream, ([127, 0, 0, 1], 0));
        let server = tokio::spawn(server);
        for (driver, model) in [
            (SourceDriverId::OpencodeGo, "gpt-5.6-luna"),
            (SourceDriverId::OpencodeGo, "qwen3.8-max"),
            (SourceDriverId::OpencodeZen, "minimax-m3"),
            (SourceDriverId::OpencodeZen, "gemini-3.8-flash"),
        ] {
            let mut channel =
                crate::coding_gateway::tests::channel(driver, &format!("http://{address}/v1"));
            channel.models = vec![model.into()];
            channel.upstream_model = model.into();
            channel.public_model = model.into();
            let mut config = crate::config::default_config();
            config.channels = vec![channel.clone()];
            super::enforce(&mut config);
            assert!(!crate::source_driver::channel_is_platform_shareable(
                &channel
            ));
            let body = serde_json::json!({"model":model,"input":"hello","tools":[{"type":"function","name":"lookup","parameters":{"type":"object","properties":{"query":{"type":"string"}}}}]}).to_string();
            let response = crate::proxy::forward_local_channel_once(
                warp::http::Method::POST,
                "/v1/responses",
                "",
                warp::http::HeaderMap::new(),
                body.into(),
                &reqwest::Client::new(),
                &config,
                &channel,
            )
            .await
            .unwrap();
            assert_eq!(response.status(), 200, "{model}");
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert!(
                value["output"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|item| item["type"] == "function_call" && item["name"] == "lookup"),
                "{model}: {value}"
            );
            assert_eq!(
                value["usage"]["input_tokens_details"]["cached_tokens"], 100,
                "{model}: {value}"
            );
        }
        server.abort();
    }
}
