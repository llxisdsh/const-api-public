// Grok Build OAuth uses the CLI proxy for text, but the public xAI host for
// Images/Videos. Media entitlement is separate: never synthesize these models
// from the CLI text catalog or a consumer plan name.
pub(crate) const GROK_MEDIA_BASE_URL: &str = "https://api.x.ai/v1";

pub(crate) fn grok_media_operation(route: &crate::surface::ApiRoute) -> Option<&'static str> {
    use crate::surface::ApiOperation;
    match route.operation {
        ApiOperation::ImagesGenerations => Some("images/generations"),
        ApiOperation::ImagesEdits => Some("images/edits"),
        ApiOperation::VideosCreate => Some("videos/generations"),
        ApiOperation::VideosGet => Some("videos"),
        ApiOperation::VideosEdit => Some("videos/edits"),
        ApiOperation::VideosExtend => Some("videos/extensions"),
        _ => None,
    }
}

pub(crate) async fn fetch_grok_media_models(
    client: &Client,
    token: &str,
    base: &str,
) -> Vec<crate::model_catalog::ModelObservation> {
    let fetch = |kind: &'static str| async move {
        let url = format!("{}/{kind}-generation-models", base.trim_end_matches('/'));
        let response = crate::upstream_transport::send(
            client
                .get(&url)
                .bearer_auth(token)
                .header(reqwest::header::ACCEPT, "application/json")
                .timeout(Duration::from_secs(30)),
        )
        .await?;
        let response = response.error_for_status()?;
        let value: serde_json::Value = response.json().await?;
        let mut models = crate::model_catalog::model_observations_from_value(
            crate::model_catalog::ModelCatalogKind::Grok,
            &value,
            &url,
        );
        for model in &mut models {
            // The dedicated catalog establishes the output modality even when
            // an older response omits modality fields. Explicit fields win.
            if model.output_modalities.is_empty() {
                model.output_modalities = vec![kind.to_string()];
            }
            if model.input_modalities.is_empty() {
                model.input_modalities = vec!["text".into(), "image".into()];
            }
        }
        Ok::<_, anyhow::Error>(models)
    };
    let (images, videos) = tokio::join!(fetch("image"), fetch("video"));
    let mut models = Vec::new();
    for (kind, result) in [("image", images), ("video", videos)] {
        match result {
            Ok(found) => models.extend(found),
            Err(error) => {
                // Free accounts can use text while both media catalogs return
                // 403. A media failure must not disable text or invent access.
                let status = error
                    .downcast_ref::<reqwest::Error>()
                    .and_then(reqwest::Error::status)
                    .map(|s| s.as_u16());
                log::debug!("[const-api][grok-catalog] media={kind} unavailable status={status:?}");
            }
        }
    }
    models
}

pub(crate) async fn execute_grok_media_request(
    client: &Client,
    channel: &ChannelConfig,
    request: crate::channel_executor::ChannelRequest,
) -> Result<crate::channel_executor::ChannelExecution> {
    let route = crate::surface::resolve_api_route(request.method.as_str(), &request.path, false)
        .ok_or_else(|| anyhow!("unsupported Grok media route"))?;
    let operation =
        grok_media_operation(&route).ok_or_else(|| anyhow!("unsupported Grok media operation"))?;
    let stream = request.stream_requested.unwrap_or(false)
        || serde_json::from_slice::<serde_json::Value>(&request.body)
            .ok()
            .and_then(|v| v.get("stream").and_then(serde_json::Value::as_bool))
            == Some(true);
    crate::channel_executor::admit_retained_subscription_operation(channel, &route, stream)?;
    let mut body = request.body;
    let model = if body.is_empty() {
        String::new()
    } else {
        // xAI's edits and video endpoints use JSON references, not Codex's
        // multipart dialect. Preserve native fields rather than silently drop
        // references when a caller uses the wrong dialect.
        let raw = std::str::from_utf8(&body)?;
        let value: serde_json::Value = serde_json::from_str(raw)
            .context("Grok media requires a JSON body with native image/video references")?;
        if !value.is_object() {
            return Err(anyhow!("Grok media request must be a JSON object"));
        }
        let model = request
            .selected_upstream_model
            .as_deref()
            .or_else(|| value.get("model").and_then(serde_json::Value::as_str))
            .unwrap_or_default()
            .to_string();
        if request.selected_upstream_model.is_some() {
            body = bytes::Bytes::from(rewrite_supplier_body_model(raw, &model));
        }
        model
    };
    let config = supplier_from_channel(channel);
    let guard = subscription_safety_enter_with_capacity(
        &config,
        "",
        SubscriptionRequestCapacity::from_flags(
            true,
            route.operation != crate::surface::ApiOperation::VideosGet,
        ),
    )
    .map_err(|payload| anyhow!(serde_json::Value::Object(payload).to_string()))?;
    let base = retained_subscription_upstream_override("grok_media")
        .unwrap_or_else(|| GROK_MEDIA_BASE_URL.to_string());
    let path = if route.operation == crate::surface::ApiOperation::VideosGet {
        request.path.trim_start_matches("/v1/").to_string()
    } else {
        operation.to_string()
    };
    let mut url = format!("{}/{path}", base.trim_end_matches('/'));
    if request.has_query {
        url.push('?');
        url.push_str(&request.raw_query);
    }
    let (token, _, _) = ensure_grok_subscription_access_token(client, channel).await?;
    let send = |token: String| {
        // Only supplier credentials are sent. Caller authorization, cookies,
        // forwarding IPs and device headers do not belong to this account.
        crate::upstream_transport::send(
            client
                .request(request.method.clone(), &url)
                .bearer_auth(token)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .header(reqwest::header::ACCEPT, "application/json")
                .body(body.clone()),
        )
    };
    let mut response = send(token).await?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        let _ = response.bytes().await;
        let (token, _, _) = ensure_grok_subscription_access_token_with_options(
            client,
            channel,
            true,
            GROK_OAUTH_TOKEN_URL,
        )
        .await?;
        response = send(token).await?;
    }
    response.headers_mut().remove(reqwest::header::SET_COOKIE);
    Ok(crate::channel_executor::ChannelExecution {
        response: guard.hold_response(response),
        inbound_protocol: None,
        target_protocol: "openai_responses".into(),
        upstream_model: model,
        inbound_stream_requested: false,
        native_passthrough: true,
        opaque: false,
        improvement_faults: Vec::new(),
        tool_mapping: Default::default(),
    })
}

fn grok_image_response_usage(body: &[u8]) -> Option<SupplierUpstreamUsage> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    if value.get("error").is_some() {
        return None;
    }
    let images = value.get("data")?.as_array()?;
    let count = images
        .iter()
        .filter(|image| {
            ["url", "b64_json"].iter().any(|key| {
                image
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|s| !s.is_empty())
            })
        })
        .count() as u64;
    if count == 0 {
        return None;
    }
    let mut usage = SupplierUpstreamUsage::from_json("openai_responses", &value);
    // Bill actual returned images, never the requested n. This is provider
    // output evidence; it does not pretend the provider reported token counts.
    usage.replace_meter("image_output_unit", "per_image", count, 1, false);
    Some(usage)
}

#[cfg(test)]
mod grok_media_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // This provider has a separate override key: no real credentials/network
    // and no collision with the existing CLI inference fixtures.
    static MEDIA_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    struct MediaOverride;
    impl Drop for MediaOverride {
        fn drop(&mut self) {
            set_retained_subscription_upstream_override("grok_media", None);
        }
    }

    async fn read_request(socket: &mut tokio::net::TcpStream) -> Vec<u8> {
        let mut request = Vec::new();
        loop {
            let mut chunk = [0_u8; 4096];
            let n = socket.read(&mut chunk).await.unwrap();
            assert_ne!(n, 0, "request ended before body");
            request.extend_from_slice(&chunk[..n]);
            if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                let size = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .map(|value| value.trim().parse::<usize>().unwrap())
                    .unwrap_or(0);
                if request.len() >= end + 4 + size {
                    return request;
                }
            }
        }
    }

    async fn respond(socket: &mut tokio::net::TcpStream, status: u16, body: &str) {
        let headers = format!(
            "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nSet-Cookie: supplier-private=1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        socket.write_all(headers.as_bytes()).await.unwrap();
        socket.write_all(body.as_bytes()).await.unwrap();
    }

    fn channel(credentials: &std::path::Path) -> ChannelConfig {
        let mut config = default_supplier_config();
        config.channel_id = "grok-media-fixture".into();
        config.enabled = true;
        config.source_driver = crate::source_driver::SourceDriverId::GrokSubscription;
        let contract = crate::channel_v2_contract_for_source(config.source_driver);
        config.executor = contract.executor;
        config.default_target = contract.default_target;
        config.discovery = contract.discovery;
        config.surface_bindings = crate::source_driver_surface_bindings(config.source_driver, "");
        config.kind = "subscription_adapter".into();
        config.api_format = "openai_responses".into();
        config.models = vec!["grok-imagine-image".into(), "grok-imagine-video".into()];
        config.upstream_model = "grok-imagine-image".into();
        config.credential_ref = credentials.to_string_lossy().into();
        config.subscription.platform = "grok".into();
        config.subscription.credential_ref = config.credential_ref.clone();
        config.subscription.audit_enabled = false;
        channel_from_supplier(config.channel_id.clone(), &config)
    }

    #[tokio::test]
    async fn grok_media_catalog_discovers_account_models_and_keeps_partial_success() {
        for video_status in [200, 403] {
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .unwrap();
            let base = format!("http://{}/v1", listener.local_addr().unwrap());
            let upstream = tokio::spawn(async move {
                for _ in 0..2 {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let request = String::from_utf8(read_request(&mut socket).await).unwrap();
                    assert!(
                        request
                            .to_ascii_lowercase()
                            .contains("authorization: bearer media-test-token")
                    );
                    if request.starts_with("GET /v1/image-generation-models ") {
                        respond(&mut socket, 200, r#"{"models":[{"id":"grok-imagine-next","output_modalities":["image"]}]}"#).await;
                    } else {
                        assert!(request.starts_with("GET /v1/video-generation-models "));
                        respond(
                            &mut socket,
                            video_status,
                            if video_status == 200 {
                                r#"{"models":[{"id":"grok-video-next"}]}"#
                            } else {
                                r#"{"error":"subscription required"}"#
                            },
                        )
                        .await;
                    }
                }
            });
            let models = fetch_grok_media_models(&Client::new(), "media-test-token", &base).await;
            upstream.await.unwrap();
            assert_eq!(models.len(), if video_status == 200 { 2 } else { 1 });
            assert_eq!(models[0].id, "grok-imagine-next");
            assert_eq!(models[0].output_modalities, ["image"]);
            if video_status == 200 {
                assert_eq!(models[1].output_modalities, ["video"]);
            }
        }
    }

    #[test]
    fn grok_media_usage_counts_returned_images_without_fabricating_tokens() {
        let usage = grok_image_response_usage(br#"{"data":[{"url":"https://example.test/a"},{"b64_json":"AA=="},{"url":""},{"error":"filtered"}],"usage":{"cost_in_usd_ticks":4}}"#).unwrap();
        let usage = serde_json::to_value(usage).unwrap();
        assert!(
            usage["usage_meters"]
                .as_array()
                .unwrap()
                .iter()
                .any(|meter| meter["meter_kind"] == "image_output_unit" && meter["quantity"] == 2)
        );
        assert!(usage["input_tokens"].is_null());
        assert!(usage["output_tokens"].is_null());
        for body in [
            br#"{"error":"denied"}"#.as_slice(),
            br#"{"data":[]}"#,
            br#"{"data":[{"url":""}]}"#,
        ] {
            assert!(grok_image_response_usage(body).is_none());
        }
    }

    #[tokio::test]
    async fn grok_media_supplier_wire_preserves_native_body_errors_and_image_meter() {
        let _lock = MEDIA_TEST_LOCK.lock().await;
        let directory = tempfile::tempdir().unwrap();
        let credential = directory.path().join("grok.json");
        fs::write(
            &credential,
            serde_json::json!({"access_token":"supplier-token","expired":now_unix()+3600})
                .to_string(),
        )
        .unwrap();
        let channel = channel(&credential);
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        set_retained_subscription_upstream_override(
            "grok_media",
            Some(&format!("http://{}/v1", listener.local_addr().unwrap())),
        );
        let _reset = MediaOverride;
        let upstream = tokio::spawn(async move {
            for status in [200, 403, 503] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let request = read_request(&mut socket).await;
                let split = request.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
                let headers = String::from_utf8_lossy(&request[..split]).to_ascii_lowercase();
                assert!(headers.starts_with("post /v1/images/generations "));
                assert!(headers.contains("authorization: bearer supplier-token"));
                for forbidden in [
                    "caller-token",
                    "forwarded",
                    "cookie:",
                    "x-xai-token-auth",
                    "x-codex-installation-id",
                ] {
                    assert!(!headers.contains(forbidden), "leaked {forbidden}");
                }
                let value: serde_json::Value =
                    serde_json::from_slice(&request[split + 4..]).unwrap();
                assert_eq!(value["model"], "grok-imagine-image");
                assert_eq!(value["extension"], serde_json::json!({"keep":true}));
                respond(
                    &mut socket,
                    status,
                    if status == 200 {
                        r#"{"data":[{"b64_json":"AA=="}],"future_field":42}"#
                    } else {
                        r#"{"error":{"message":"subscription required"}}"#
                    },
                )
                .await;
            }
        });
        for status in [200, 403, 503] {
            let wire = crate::surface_wire::SurfaceEnvelope {
                surface: Some(crate::surface::ApiSurface::OpenAi),
                operation: Some(crate::surface::ApiOperation::ImagesGenerations),
                method: "POST".into(), path: "/v1/images/generations".into(),
                headers: ["authorization", "cookie", "x-forwarded-for", "x-codex-installation-id"]
                    .map(|name| crate::surface_wire::WireHeader { name:name.into(), value:crate::surface_wire::WireBody::from_bytes(b"caller-token") }).into(),
                body: crate::surface_wire::WireBody::from_bytes(br#"{"model":"grok-imagine-image","prompt":"blue dot","n":3,"extension":{"keep":true}}"#),
                selected_upstream_model: Some("grok-imagine-image".into()), stream: Some(false),
                ..Default::default()
            };
            let msg = SupplierMessage {
                id: format!("media-{status}"),
                kind: "http_request".into(),
                output_credit: None,
                payload: crate::surface_wire::wire_payload(wire).unwrap(),
            };
            let (outbound, _inbound) = mpsc::channel(4);
            let payload = forward_supplier_request(
                &Client::new(),
                &supplier_from_channel(&channel),
                &msg,
                false,
                &outbound,
            )
            .await
            .unwrap()
            .unwrap();
            let response = crate::surface_wire::SurfaceEnvelope::from_payload(&payload).unwrap();
            assert_eq!(response.status, Some(status));
            if status == 200 {
                assert!(
                    payload["upstream_usage"]["usage_meters"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|meter| meter["meter_kind"] == "image_output_unit"
                            && meter["quantity"] == 1)
                );
            } else {
                assert!(
                    payload
                        .get("upstream_usage")
                        .is_none_or(serde_json::Value::is_null)
                );
            }
            assert!(
                !serde_json::to_string(&response.headers)
                    .unwrap()
                    .contains("supplier-private")
            );
        }
        upstream.await.unwrap();
    }

    #[tokio::test]
    async fn grok_media_cancel_releases_capacity_before_and_after_response_headers() {
        let _lock = MEDIA_TEST_LOCK.lock().await;
        let directory = tempfile::tempdir().unwrap();
        let credential = directory.path().join("grok.json");
        fs::write(
            &credential,
            serde_json::json!({"access_token":"supplier-token","expired":now_unix()+3600})
                .to_string(),
        )
        .unwrap();
        for send_headers in [false, true] {
            let mut channel = channel(&credential);
            channel.id = format!("safety-grok-media-cancel-{send_headers}");
            let id = channel.id.clone();
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .unwrap();
            set_retained_subscription_upstream_override(
                "grok_media",
                Some(&format!("http://{}/v1", listener.local_addr().unwrap())),
            );
            let _reset = MediaOverride;
            let (arrived, received) = tokio::sync::oneshot::channel();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                read_request(&mut socket).await;
                if send_headers {
                    socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 8\r\n\r\n").await.unwrap();
                }
                arrived.send(()).unwrap();
                let mut byte = [0u8; 1];
                let _ = socket.read(&mut byte).await;
            });
            let task = tokio::spawn(async move {
                crate::channel_executor::execute_channel_request(
                    &Client::new(),
                    &channel,
                    crate::channel_executor::ChannelRequest {
                        method: reqwest::Method::POST,
                        path: "/v1/images/generations".into(),
                        raw_query: String::new(),
                        has_query: false,
                        headers: Default::default(),
                        body: bytes::Bytes::from_static(
                            br#"{"model":"grok-imagine-image","prompt":"dot"}"#,
                        ),
                        stream_requested: Some(false),
                        selected_upstream_model: None,
                        selected_target_protocol: None,
                        cache_identity: None,
                        safety_identifier: None,
                    },
                )
                .await
            });
            tokio::time::timeout(Duration::from_secs(5), received)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(crate::channel_concurrency::current(&id), 1);
            if send_headers {
                drop(task.await.unwrap().unwrap());
            } else {
                task.abort();
                assert!(matches!(task.await, Err(error) if error.is_cancelled()));
            }
            assert_eq!(crate::channel_concurrency::current(&id), 0);
            server.abort();
        }
    }

    #[tokio::test]
    async fn grok_media_local_video_json_create_and_poll_reuse_channel() {
        let _lock = MEDIA_TEST_LOCK.lock().await;
        let directory = tempfile::tempdir().unwrap();
        let credential = directory.path().join("grok.json");
        fs::write(
            &credential,
            serde_json::json!({"access_token":"supplier-token","expired":now_unix()+3600})
                .to_string(),
        )
        .unwrap();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        set_retained_subscription_upstream_override(
            "grok_media",
            Some(&format!("http://{}/v1", listener.local_addr().unwrap())),
        );
        let _reset = MediaOverride;
        let upstream = tokio::spawn(async move {
            for (prefix, body) in [
                (
                    "POST /v1/videos/generations ",
                    r#"{"request_id":"video-test"}"#,
                ),
                (
                    "GET /v1/videos/video-test ",
                    r#"{"status":"done","video":{"url":"https://example.test/video.mp4","duration":5}}"#,
                ),
            ] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let request = String::from_utf8(read_request(&mut socket).await).unwrap();
                assert!(request.starts_with(prefix), "wrong media endpoint");
                respond(&mut socket, 200, body).await;
            }
        });
        let mut config = default_config();
        config.channels = vec![channel(&credential)];
        let api_key = config.api_key.clone();
        let shared = Arc::new(crate::proxy::ProxyShared::from_config(
            config,
            Client::new(),
        ));
        let routes = crate::proxy_routes(shared);
        let created = warp::test::request()
            .method("POST")
            .path("/v1/videos/generations")
            .header("authorization", format!("Bearer {api_key}"))
            .json(
                &serde_json::json!({"model":"grok-imagine-video","prompt":"blue dot","duration":5}),
            )
            .reply(&routes)
            .await;
        assert_eq!(
            created.status(),
            200,
            "{}",
            String::from_utf8_lossy(created.body())
        );
        let polled = warp::test::request()
            .method("GET")
            .path("/v1/videos/video-test")
            .header("authorization", format!("Bearer {api_key}"))
            .reply(&routes)
            .await;
        assert_eq!(
            polled.status(),
            200,
            "{}",
            String::from_utf8_lossy(polled.body())
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(polled.body()).unwrap()["video"]["duration"],
            5
        );
        upstream.await.unwrap();
    }
}
