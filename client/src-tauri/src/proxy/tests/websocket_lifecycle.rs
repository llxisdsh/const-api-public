async fn lifecycle_proxy(
    config: ClientConfig,
) -> (AppState, std::net::SocketAddr, Arc<ProxyShared>) {
    let state = crate::runtime::create_runtime_state(PathBuf::new(), config.clone());
    let shared = Arc::new(ProxyShared::from_config(config, Client::new()));
    let (shutdown, stopped) = tokio::sync::oneshot::channel();
    let (address, server) =
        crate::try_bind_proxy_server(shared.clone(), "127.0.0.1:0".parse().unwrap(), stopped)
            .await
            .unwrap();
    {
        let mut runtime = state.proxy.lock().await;
        runtime.running = true;
        runtime.shutdown = Some(shutdown);
        runtime.server_task = Some(tokio::spawn(server));
    }
    (state, address, shared)
}

async fn lifecycle_socket(
    address: std::net::SocketAddr,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut request = format!("ws://{address}/v1/responses")
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "authorization",
        "Bearer lifecycle-test-key".parse().unwrap(),
    );
    tokio_tungstenite::connect_async(request).await.unwrap().0
}

fn lifecycle_config() -> ClientConfig {
    let mut config = default_config();
    config.api_key = "lifecycle-test-key".into();
    config.account_device_api_key.clear();
    config.endpoints.clear();
    config.channels.clear();
    config.prefer_local_supply = true;
    config.allow_model_equivalence = false;
    config
}

#[tokio::test]
async fn websocket_lifecycle_status_reads_active_requests_instead_of_health_snapshot() {
    let mut config = lifecycle_config();
    let channel = openai_api_channel(
        "lifecycle-status",
        "lifecycle-model",
        "http://127.0.0.1:1/v1",
    );
    config.channels.push(channel.clone());
    let state = crate::runtime::create_runtime_state(PathBuf::new(), config);
    let mut health = crate::supplier::failed_supplier_channel_health(
        &channel,
        "fixture health snapshot".into(),
        0,
        123,
    );
    health.current_concurrency = 99;
    let mut runtime = state.supplier.lock().await;
    runtime.channels.push(health.clone());
    runtime
        .transport_state
        .lock()
        .unwrap()
        .channel_health
        .insert(channel.id.clone(), health);
    assert_eq!(runtime.status().channels[0].current_concurrency, 0);
    let request = crate::channel_concurrency::Guard::acquire(&channel.id, 1).unwrap();
    assert_eq!(runtime.status().channels[0].current_concurrency, 1);
    assert_eq!(
        runtime.status().channels[0].checked_at_unix,
        123,
        "no health probe is needed"
    );
    drop(request);
    assert_eq!(runtime.status().channels[0].current_concurrency, 0);
}

#[test]
fn websocket_lifecycle_subscription_counts_turns_and_releases_on_completion_or_cancel() {
    let _home_guard = crate::tool_config::test_home_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let home = tempfile::tempdir().unwrap();
    let previous = crate::tool_config::set_test_home_override(Some(home.path().to_path_buf()));
    crate::supplier::reset_subscription_safety_runtime_for_tests();
    let result = std::panic::catch_unwind(|| {
        let mut channel = openai_subscription_channel(&["lifecycle-model"]);
        channel.id = "safety-ws-lifecycle-turn-count".into();
        channel.v2.max_concurrency = 1;
        let config = supplier_from_channel(&channel);
        let start = |frame| {
            NativeSubscriptionWebsocketRequest::start_at(&config, frame, std::time::Instant::now())
                .unwrap()
        };
        let current = || crate::supplier::subscription_current_concurrency(&channel.id);
        assert_eq!(current(), 0);
        let warmup = start(
            r#"{"type":"response.create","model":"lifecycle-model","input":[],"generate":false}"#,
        );
        assert_eq!(current(), 0, "warmup is not an executing model turn");
        drop(warmup);
        let frame = r#"{"type":"response.create","model":"lifecycle-model","input":[]}"#;
        for (event, status) in [
            (
                r#"{"type":"response.completed","response":{"id":"resp-count","status":"completed"}}"#,
                200,
            ),
            (
                r#"{"type":"response.cancelled","response":{"id":"resp-count","status":"cancelled"}}"#,
                499,
            ),
            (
                r#"{"type":"response.failed","response":{"id":"resp-count","status":"failed"}}"#,
                502,
            ),
        ] {
            let mut request = start(frame);
            assert_eq!(current(), 1);
            assert_eq!(request.observe(event), Some(status));
            request.finalize(status, (status == 499).then_some("request"), None, &channel);
            assert_eq!(current(), 0, "a terminal turn must release its slot");
        }
        let request = start(frame);
        assert_eq!(current(), 1);
        drop(request);
        assert_eq!(current(), 0, "disconnect/cancel must release its slot");
    });
    crate::tool_config::set_test_home_override(previous);
    crate::supplier::reset_subscription_safety_runtime_for_tests();
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
}

#[tokio::test]
async fn websocket_lifecycle_proxy_stop_closes_idle_socket_and_restart_has_new_admission() {
    let config = lifecycle_config();
    let (state, address, shared) = lifecycle_proxy(config.clone()).await;
    let mut socket = lifecycle_socket(address).await;
    crate::stop_proxy_inner(&state).await.unwrap();
    let message = tokio::time::timeout(Duration::from_secs(3), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        message.is_close(),
        "idle old WS must not outlive Stop: {message:?}"
    );
    assert!(*shared.websocket_shutdown.borrow());
    let (restarted, address, fresh) = lifecycle_proxy(config).await;
    let mut socket = lifecycle_socket(address).await;
    assert!(
        !*fresh.websocket_shutdown.borrow(),
        "old stop must not cancel the new listener"
    );
    crate::stop_proxy_inner(&restarted).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(3), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .is_close()
    );
}

#[tokio::test]
async fn websocket_lifecycle_proxy_stop_drains_turn_and_forbids_next_native_and_http_bridge() {
    use tokio_tungstenite::tungstenite::Message as Ws;
    for native in [true, false] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_address = listener.local_addr().unwrap();
        let (started, started_rx) = tokio::sync::oneshot::channel();
        let (finish, finish_rx) = tokio::sync::oneshot::channel();
        let upstream = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let done = r#"{"type":"response.completed","response":{"id":"resp-drained","status":"completed","output":[]}}"#;
            if native {
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                assert!(socket.next().await.unwrap().unwrap().is_text());
                started.send(()).unwrap();
                finish_rx.await.unwrap();
                socket.send(Ws::Text(done.into())).await.unwrap();
                let next = tokio::time::timeout(Duration::from_secs(3), socket.next())
                    .await
                    .unwrap();
                assert!(
                    matches!(next, Some(Ok(Ws::Close(_))) | None),
                    "a second call reached a stopped proxy: {next:?}"
                );
            } else {
                let mut request = Vec::new();
                let mut buf = [0; 2048];
                while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                    let count = stream.read(&mut buf).await.unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&buf[..count]);
                }
                started.send(()).unwrap();
                finish_rx.await.unwrap();
                let body = format!("data: {done}\n\ndata: [DONE]\n\n");
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
        });
        let mut config = lifecycle_config();
        let mut channel = openai_api_channel(
            &format!("lifecycle-{native}"),
            "lifecycle-model",
            &format!("http://{upstream_address}/v1"),
        );
        if native {
            channel.detection_checks.push(ChannelDetectionCheck {
                name: "responses_operation".into(),
                capability: "openai.responses_websocket".into(),
                status: "verified".into(),
                ..Default::default()
            });
        }
        let channel_id = channel.id.clone();
        config.channels.push(channel);
        let (state, address, _) = lifecycle_proxy(config).await;
        let mut socket = lifecycle_socket(address).await;
        assert_eq!(
            crate::channel_concurrency::current(&channel_id),
            0,
            "idle WS is not an active call"
        );
        socket
            .send(Ws::Text(
                r#"{"type":"response.create","model":"lifecycle-model","input":[],"stream":true}"#
                    .into(),
            ))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), started_rx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(crate::channel_concurrency::current(&channel_id), 1);
        crate::stop_proxy_inner(&state).await.unwrap();
        assert_eq!(
            crate::channel_concurrency::current(&channel_id),
            1,
            "Stop must preserve the admitted turn"
        );
        finish.send(()).unwrap();
        let mut completed = false;
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(Ok(message)) = socket.next().await {
                if message.is_close() {
                    break;
                }
                if message
                    .to_text()
                    .is_ok_and(|text| text.contains("response.completed"))
                {
                    completed = true;
                    // Race a new turn against shutdown: it must never reach upstream.
                    let _ = socket
                        .send(Ws::Text(
                            r#"{"type":"response.create","model":"lifecycle-model","input":[]}"#
                                .into(),
                        ))
                        .await;
                }
            }
        })
        .await
        .unwrap();
        assert!(
            completed,
            "the response was cut off by Stop, native={native}"
        );
        upstream.await.unwrap();
        assert_eq!(crate::channel_concurrency::current(&channel_id), 0);
    }
}
