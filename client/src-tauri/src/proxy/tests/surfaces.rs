    #[tokio::test]
    async fn local_response_lifecycle_uses_hard_owner_and_fails_closed_after_delete() {
        let channel_a_calls = Arc::new(StdMutex::new(Vec::<String>::new()));
        let channel_a_calls_filter = channel_a_calls.clone();
        let channel_a = warp::method().and(warp::path::full()).map(
            move |method: warp::http::Method, path: warp::path::FullPath| {
                channel_a_calls_filter
                    .lock()
                    .expect("channel A calls")
                    .push(format!("{method} {}", path.as_str()));
                warp::reply::json(&serde_json::json!({
                    "id": "resp_owned_a",
                    "object": if method == warp::http::Method::DELETE {
                        "response.deleted"
                    } else {
                        "response"
                    },
                    "model": "model-a",
                    "source": "channel-a",
                    "output": []
                }))
            },
        );
        let (channel_a_addr, channel_a_server) =
            crate::bind_ephemeral!(channel_a, ([127, 0, 0, 1], 0));
        let channel_a_task = tokio::spawn(channel_a_server);

        let channel_b_calls = Arc::new(StdMutex::new(Vec::<String>::new()));
        let channel_b_calls_filter = channel_b_calls.clone();
        let channel_b = warp::method().and(warp::path::full()).map(
            move |method: warp::http::Method, path: warp::path::FullPath| {
                channel_b_calls_filter
                    .lock()
                    .expect("channel B calls")
                    .push(format!("{method} {}", path.as_str()));
                warp::reply::json(&serde_json::json!({
                    "id": "resp_wrong_b",
                    "object": "response",
                    "model": "model-b",
                    "source": "channel-b",
                    "output": []
                }))
            },
        );
        let (channel_b_addr, channel_b_server) =
            crate::bind_ephemeral!(channel_b, ([127, 0, 0, 1], 0));
        let channel_b_task = tokio::spawn(channel_b_server);

        let mut config = default_config();
        config.account_device_api_key.clear();
        config.prefer_local_supply = true;
        config.channels = vec![
            openai_api_channel(
                "channel-a",
                "model-a",
                &format!("http://{channel_a_addr}/v1"),
            ),
            openai_api_channel(
                "channel-b",
                "model-b",
                &format!("http://{channel_b_addr}/v1"),
            ),
        ];
        let shared = Arc::new(ProxyShared::from_config(
            normalize_config(config),
            Client::new(),
        ));

        let created = forward_with_failover(
            warp::http::Method::POST,
            "/v1/responses",
            "",
            HeaderMap::new(),
            bytes::Bytes::from_static(br#"{"model":"model-a","input":"owner"}"#),
            &shared,
        )
        .await
        .expect("create response");
        assert_eq!(created.status(), StatusCode::OK);
        let created_body = crate::test_body_bytes(created.into_body())
            .await
            .expect("create body");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&created_body)
                .expect("create json")["source"],
            "channel-a"
        );

        let fetched = forward_with_failover(
            warp::http::Method::GET,
            "/v1/responses/resp_owned_a",
            "include=output",
            HeaderMap::new(),
            bytes::Bytes::new(),
            &shared,
        )
        .await
        .expect("get owned response");
        let fetched_body = crate::test_body_bytes(fetched.into_body())
            .await
            .expect("get body");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&fetched_body)
                .expect("get json")["source"],
            "channel-a"
        );

        let deleted = forward_with_failover(
            warp::http::Method::DELETE,
            "/v1/responses/resp_owned_a",
            "",
            HeaderMap::new(),
            bytes::Bytes::new(),
            &shared,
        )
        .await
        .expect("delete owned response");
        assert_eq!(deleted.status(), StatusCode::OK);
        let _ = crate::test_body_bytes(deleted.into_body()).await;

        let after_delete = forward_with_failover(
            warp::http::Method::GET,
            "/v1/responses/resp_owned_a",
            "",
            HeaderMap::new(),
            bytes::Bytes::new(),
            &shared,
        )
        .await
        .expect("fail-closed response");
        assert_eq!(after_delete.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            channel_a_calls.lock().expect("channel A calls").as_slice(),
            [
                "POST /v1/responses",
                "GET /v1/responses/resp_owned_a",
                "DELETE /v1/responses/resp_owned_a"
            ]
        );
        assert!(channel_b_calls.lock().expect("channel B calls").is_empty());

        channel_a_task.abort();
        channel_b_task.abort();
    }

    #[tokio::test]
    async fn local_conversation_lifecycle_is_native_and_owner_bound() {
        let calls = Arc::new(StdMutex::new(Vec::<String>::new()));
        let calls_filter = calls.clone();
        let upstream = warp::method()
            .and(warp::path::full())
            .and(warp::query::raw().or(warp::any().map(String::new)).unify())
            .map(
                move |method: warp::http::Method,
                      path: warp::path::FullPath,
                      query: String| {
                    calls_filter.lock().expect("calls").push(format!(
                        "{method} {}{}",
                        path.as_str(),
                        if query.is_empty() {
                            String::new()
                        } else {
                            format!("?{query}")
                        }
                    ));
                    warp::reply::json(&serde_json::json!({
                        "id": "conv_native_1",
                        "object": if method == warp::http::Method::DELETE {
                            "conversation.deleted"
                        } else {
                            "conversation"
                        },
                        "metadata": {"native": "yes"}
                    }))
                },
            );
        let (upstream_addr, upstream_server) =
            crate::bind_ephemeral!(upstream, ([127, 0, 0, 1], 0));
        let upstream_task = tokio::spawn(upstream_server);

        let mut config = default_config();
        config.account_device_api_key.clear();
        config.prefer_local_supply = true;
        config.channels = vec![openai_api_channel(
            "conversation-channel",
            "model-unused",
            &format!("http://{upstream_addr}/v1"),
        )];
        let shared = Arc::new(ProxyShared::from_config(
            normalize_config(config),
            Client::new(),
        ));

        let create = forward_with_failover(
            warp::http::Method::POST,
            "/v1/conversations",
            "",
            HeaderMap::new(),
            bytes::Bytes::from_static(br#"{"metadata":{"native":"yes"}}"#),
            &shared,
        )
        .await
        .expect("create conversation");
        assert_eq!(create.status(), StatusCode::OK);
        let _ = crate::test_body_bytes(create.into_body()).await;

        let list = forward_with_failover(
            warp::http::Method::GET,
            "/v1/conversations/conv_native_1/items",
            "limit=20&order=asc",
            HeaderMap::new(),
            bytes::Bytes::new(),
            &shared,
        )
        .await
        .expect("list conversation items");
        assert_eq!(list.status(), StatusCode::OK);
        let _ = crate::test_body_bytes(list.into_body()).await;

        let delete = forward_with_failover(
            warp::http::Method::DELETE,
            "/v1/conversations/conv_native_1",
            "",
            HeaderMap::new(),
            bytes::Bytes::new(),
            &shared,
        )
        .await
        .expect("delete conversation");
        assert_eq!(delete.status(), StatusCode::OK);
        let _ = crate::test_body_bytes(delete.into_body()).await;

        assert_eq!(
            calls.lock().expect("calls").as_slice(),
            [
                "POST /v1/conversations",
                "GET /v1/conversations/conv_native_1/items?limit=20&order=asc",
                "DELETE /v1/conversations/conv_native_1"
            ]
        );
        upstream_task.abort();
    }

    #[tokio::test]
    async fn responses_websocket_proxies_native_frames_and_binds_response_owner() {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let upstream_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("upstream websocket listener");
        let upstream_address = upstream_listener.local_addr().expect("upstream address");
        let upstream_authorized = Arc::new(StdMutex::new(false));
        let upstream_authorized_filter = upstream_authorized.clone();
        let upstream_task = tokio::spawn(async move {
            let (stream, _) = upstream_listener.accept().await.expect("upstream accept");
            let mut websocket = tokio_tungstenite::accept_hdr_async(
                stream,
                move |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                      response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                    let authorized = request
                        .headers()
                        .get("authorization")
                        .and_then(|value| value.to_str().ok())
                        == Some("Bearer sk-upstream-secret");
                    *upstream_authorized_filter.lock().expect("upstream auth") = authorized;
                    assert_eq!(request.uri().path(), "/v1/responses");
                    Ok(response)
                },
            )
                .await
                .expect("upstream websocket handshake");
            let first = websocket
                .next()
                .await
                .expect("first upstream frame")
                .expect("first upstream message")
                .into_text()
                .expect("first text");
            let first: serde_json::Value =
                serde_json::from_str(&first).expect("first response.create");
            assert_eq!(first["future"]["preserved"], true);
            websocket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    serde_json::json!({
                        "type": "response.created",
                        "response": {
                            "id": "resp_ws_bound",
                            "model": "model-ws",
                            "output": []
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .expect("send created event");
            websocket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    serde_json::json!({
                        "type": "response.completed",
                        "response": {
                            "id": "resp_ws_bound",
                            "model": "model-ws",
                            "output": []
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .expect("send completed event");
            let second = websocket
                .next()
                .await
                .expect("second upstream frame")
                .expect("second upstream message")
                .into_text()
                .expect("second text");
            let second: serde_json::Value =
                serde_json::from_str(&second).expect("second response.create");
            assert_eq!(second["previous_response_id"], "resp_ws_bound");
            websocket
                .send(tokio_tungstenite::tungstenite::Message::Close(None))
                .await
                .ok();
        });

        let mut config = default_config();
        config.api_key = "sk-local-websocket".to_string();
        config.account_device_api_key.clear();
        config.prefer_local_supply = true;
        let mut channel = openai_api_channel(
            "websocket-channel",
            "model-ws",
            &format!("http://{upstream_address}/v1"),
        );
        channel.upstream_api_key = "sk-upstream-secret".to_string();
        config.channels = vec![channel];
        let shared = Arc::new(ProxyShared::from_config(
            normalize_config(config),
            Client::new(),
        ));
        let routes = crate::proxy_routes(shared.clone());
        let (proxy_address, proxy_server) =
            crate::bind_ephemeral!(routes, ([127, 0, 0, 1], 0));
        let proxy_task = tokio::spawn(proxy_server);

        let mut request = format!("ws://{proxy_address}/v1/responses")
            .into_client_request()
            .expect("client websocket request");
        request.headers_mut().insert(
            "authorization",
            "Bearer sk-local-websocket".parse().expect("local auth"),
        );
        let (mut client, _) = tokio_tungstenite::connect_async(request)
            .await
            .expect("connect through local proxy");
        client
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::json!({
                    "type": "response.create",
                    "model": "model-ws",
                    "store": false,
                    "input": [],
                    "future": {"preserved": true}
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("first client frame");
        let created = client
            .next()
            .await
            .expect("created client frame")
            .expect("created client message")
            .into_text()
            .expect("created text");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&created)
                .expect("created json")["response"]["id"],
            "resp_ws_bound"
        );
        let owner = shared
            .local_resource_owners
            .get(
                crate::surface::ApiSurface::OpenAi,
                "response",
                "resp_ws_bound",
            )
            .expect("websocket response owner");
        assert_eq!(owner.channel_id, "websocket-channel");

        let completed = client
            .next()
            .await
            .expect("completed client frame")
            .expect("completed client message")
            .into_text()
            .expect("completed text");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&completed)
                .expect("completed json")["type"],
            "response.completed"
        );

        client
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::json!({
                    "type": "response.create",
                    "model": "model-ws",
                    "store": false,
                    "previous_response_id": "resp_ws_bound",
                    "input": []
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("continuation frame");
        upstream_task.await.expect("upstream task");
        assert!(*upstream_authorized.lock().expect("upstream auth"));
        proxy_task.abort();
    }

    #[tokio::test]
    async fn native_websocket_restores_full_model_on_every_reused_turn() {
        use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};
        for driver in [crate::source_driver::SourceDriverId::OpenAiApi, crate::source_driver::SourceDriverId::Openrouter] {
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let address = listener.local_addr().unwrap();
            let (tx, rx) = tokio::sync::oneshot::channel();
            let upstream_task = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                let mut frames = Vec::new();
                for turn in 0..2 {
                    frames.push(socket.next().await.unwrap().unwrap().into_text().unwrap().to_string());
                    socket.send(Message::Text(format!(r#"{{"type":"response.completed","response":{{"id":"resp-wire-{turn}","output":[]}}}}"#).into())).await.unwrap();
                }
                let _ = tx.send(frames);
                let _ = socket.close(None).await;
            });
            let wire = "Vendor/model-ws:free";
            let mut channel = openai_api_channel("wire-ws-channel", wire, &format!("http://{address}/v1"));
            channel.set_source_driver(driver);
            channel.upstream_api_key = "mock-upstream".into();
            let mut config = default_config();
            config.api_key = "mock-local-key".into();
            config.account_device_api_key.clear();
            config.prefer_local_supply = true;
            config.allow_model_equivalence = false;
            config.channels = vec![channel];
            let shared = Arc::new(ProxyShared::from_config(normalize_config(config), Client::new()));
            let frames = [
                r#"{ "type":"response.create", "model":"model-ws:free", "input":[{"role":"user","content":"stable cache prefix"}],"store":false,"prompt_cache_key":"keep" }"#,
                r#"{ "type":"response.create", "model":"model-ws:free", "previous_response_id":"resp-wire-0","input":[],"store":false,"metadata":{"model":"model-ws:free","opaque":"unchanged"} }"#,
            ];
            assert!(matches!(select_responses_websocket_route(&shared, &warp::http::HeaderMap::new(), frames[0].as_bytes()), ResponsesWebsocketRoute::Native(_)));
            let (proxy_address, proxy_server) = crate::bind_ephemeral!(crate::proxy_routes(shared), ([127,0,0,1],0));
            let proxy_task = tokio::spawn(proxy_server);
            let mut request = format!("ws://{proxy_address}/v1/responses").into_client_request().unwrap();
            request.headers_mut().insert("authorization", "Bearer mock-local-key".parse().unwrap());
            let (mut client, _) = tokio_tungstenite::connect_async(request).await.unwrap();
            for frame in frames {
                client.send(Message::Text(frame.into())).await.unwrap();
                let terminal = tokio::time::timeout(Duration::from_secs(5), client.next()).await.unwrap().unwrap().unwrap().into_text().unwrap();
                assert!(terminal.contains("response.completed"), "{terminal}");
            }
            let result = tokio::time::timeout(Duration::from_secs(5), rx).await;
            let _ = client.close(None).await;
            proxy_task.abort();
            upstream_task.abort();
            for (actual, original) in result.unwrap().unwrap().iter().zip(frames) {
                assert_eq!(actual, &original.replacen("model-ws:free", wire, 1), "{driver:?}: wire rewrite must preserve every other byte");
            }
        }
    }

    #[tokio::test]
    async fn responses_websocket_http_bridge_replays_a_custom_tool_call_before_its_output() {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let upstream_requests = Arc::new(StdMutex::new(Vec::<serde_json::Value>::new()));
        let upstream_requests_filter = upstream_requests.clone();
        let upstream = warp::post()
            .and(warp::path!("v1" / "responses"))
            .and(warp::body::json())
            .map(move |body: serde_json::Value| {
                let mut requests = upstream_requests_filter
                    .lock()
                    .expect("upstream requests");
                requests.push(body);
                let response = if requests.len() == 1 {
                    concat!(
                        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_bridge_tool\",\"status\":\"in_progress\",\"output\":[]}}\n\n",
                        "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"custom_tool_call\",\"call_id\":\"call_bridge_tool\",\"name\":\"exec\",\"input\":\"text('ok')\"}}\n\n",
                        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_bridge_tool\",\"status\":\"completed\",\"output\":[]}}\n\n",
                        "data: [DONE]\n\n"
                    )
                } else {
                    concat!(
                        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_bridge_final\",\"status\":\"in_progress\",\"output\":[]}}\n\n",
                        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_bridge_final\",\"status\":\"completed\",\"output\":[]}}\n\n",
                        "data: [DONE]\n\n"
                    )
                };
                warp::reply::with_header(response, "content-type", "text/event-stream")
            });
        let (upstream_address, upstream_server) =
            crate::bind_ephemeral!(upstream, ([127, 0, 0, 1], 0));
        let upstream_task = tokio::spawn(upstream_server);

        let mut config = default_config();
        config.api_key = "sk-local-websocket-bridge".to_string();
        config.account_device_api_key.clear();
        config.prefer_local_supply = true;
        config.channels = vec![openai_api_channel(
            "websocket-bridge-channel",
            "model-bridge",
            &format!("http://{upstream_address}/v1"),
        )];
        let shared = Arc::new(ProxyShared::from_config(
            normalize_config(config),
            Client::new(),
        ));
        let routes = crate::proxy_routes(shared);
        let (proxy_address, proxy_server) =
            crate::bind_ephemeral!(routes, ([127, 0, 0, 1], 0));
        let proxy_task = tokio::spawn(proxy_server);

        let mut request = format!("ws://{proxy_address}/v1/responses")
            .into_client_request()
            .expect("client websocket request");
        request.headers_mut().insert(
            "authorization",
            "Bearer sk-local-websocket-bridge"
                .parse()
                .expect("local auth"),
        );
        let (mut client, _) = tokio_tungstenite::connect_async(request)
            .await
            .expect("connect through local proxy");
        client
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::json!({
                    "type": "response.create",
                    "model": "model-bridge",
                    "store": false,
                    "stream": true,
                    "input": [{"role":"user","content":"run the tool"}]
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("first client frame");

        loop {
            let event = client
                .next()
                .await
                .expect("first response terminal frame")
                .expect("first response message")
                .into_text()
                .expect("first response text");
            if serde_json::from_str::<serde_json::Value>(&event)
                .ok()
                .and_then(|event| event.get("type").cloned())
                == Some(serde_json::json!("response.completed"))
            {
                break;
            }
        }

        client
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::json!({
                    "type": "response.create",
                    "model": "model-bridge",
                    "store": false,
                    "stream": true,
                    "previous_response_id": "resp_bridge_tool",
                    "input": [{
                        "type": "custom_tool_call_output",
                        "call_id": "call_bridge_tool",
                        "output": "ok"
                    }]
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("tool output client frame");

        loop {
            let event = client
                .next()
                .await
                .expect("second response terminal frame")
                .expect("second response message")
                .into_text()
                .expect("second response text");
            if serde_json::from_str::<serde_json::Value>(&event)
                .ok()
                .and_then(|event| event.get("type").cloned())
                == Some(serde_json::json!("response.completed"))
            {
                break;
            }
        }

        {
            let requests = upstream_requests.lock().expect("upstream requests");
            assert_eq!(requests.len(), 2);
            let continuation = &requests[1];
            assert!(continuation.get("previous_response_id").is_none());
            let input = continuation["input"]
                .as_array()
                .expect("self-contained continuation input");
            assert_eq!(input.len(), 3);
            assert_eq!(input[1]["type"], "custom_tool_call");
            assert_eq!(input[1]["call_id"], "call_bridge_tool");
            assert_eq!(input[2]["type"], "custom_tool_call_output");
            assert_eq!(input[2]["call_id"], "call_bridge_tool");
        }
        let _ = client.close(None).await;
        proxy_task.abort();
        upstream_task.abort();
    }

    #[tokio::test]
    async fn responses_websocket_platform_duplex_keeps_custom_tool_output_on_one_session() {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        use tokio_tungstenite::tungstenite::Message as WsMessage;

        let platform_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("platform listener");
        let platform_address = platform_listener.local_addr().expect("platform address");
        let platform_task = tokio::spawn(async move {
            let (stream, _) = platform_listener.accept().await.expect("platform accept");
            let mut socket = tokio_tungstenite::accept_async(stream)
                .await
                .expect("platform WebSocket upgrade");

            let auth = socket
                .next()
                .await
                .expect("platform auth frame")
                .expect("platform auth message")
                .into_text()
                .expect("platform auth text");
            let auth: SupplierMessage = serde_json::from_str(&auth).expect("platform auth JSON");
            assert_eq!(auth.kind, "authenticate");
            socket
                .send(WsMessage::Text(
                    serde_json::json!({
                        "id": auth.id,
                        "type": "authenticate_ack",
                        "payload": {
                            "ok": true,
                            "user_id": "platform-user",
                            "transport_features": ["platform_duplex_v1"]
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .expect("platform auth ack");

            let open = socket
                .next()
                .await
                .expect("platform open frame")
                .expect("platform open message")
                .into_text()
                .expect("platform open text");
            let open: SupplierMessage = serde_json::from_str(&open).expect("platform open JSON");
            assert_eq!(open.kind, "platform_duplex_open");
            socket
                .send(WsMessage::Text(
                    serde_json::json!({
                        "id": open.id,
                        "type": "platform_duplex_opened",
                        "payload": {"ok": true}
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .expect("platform open ack");

            let first = socket
                .next()
                .await
                .expect("first platform frame")
                .expect("first platform message")
                .into_text()
                .expect("first platform text");
            let first: SupplierMessage = serde_json::from_str(&first).expect("first frame JSON");
            assert_eq!(first.kind, "platform_duplex_client_frame");
            let first_turn_id = first.payload["_const_duplex_turn_id"]
                .as_str()
                .expect("first turn id")
                .to_string();
            let first_body: serde_json::Value = serde_json::from_str(
                first.payload["data"].as_str().expect("first frame data"),
            )
            .expect("first response.create JSON");
            assert_eq!(first_body["model"], "platform-model");

            for (sequence, event, terminal) in [
                (
                    1u64,
                    serde_json::json!({
                        "type": "response.created",
                        "response": {"id": "resp-platform-tool", "output": []}
                    }),
                    false,
                ),
                (
                    2,
                    serde_json::json!({
                        "type": "response.output_item.done",
                        "output_index": 0,
                        "item": {
                            "type": "custom_tool_call",
                            "call_id": "call-platform-tool",
                            "name": "exec",
                            "input": "text('ok')"
                        }
                    }),
                    false,
                ),
                (
                    3,
                    serde_json::json!({
                        "type": "response.completed",
                        "response": {
                            "id": "resp-platform-tool",
                            "status": "completed",
                            "output": []
                        }
                    }),
                    true,
                ),
            ] {
                socket
                    .send(WsMessage::Text(
                        serde_json::json!({
                            "id": open.id,
                            "type": "platform_duplex_server_frame",
                            "payload": {
                                "frame_type": "text",
                                "data": event.to_string(),
                                "turn_terminal": terminal,
                                "status": if terminal { 200 } else { 0 },
                                "_const_duplex_turn_id": first_turn_id,
                                "_const_duplex_turn_seq": sequence
                            }
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .expect("platform first-turn response");
            }

            let second = socket
                .next()
                .await
                .expect("second platform frame")
                .expect("second platform message")
                .into_text()
                .expect("second platform text");
            let second: SupplierMessage = serde_json::from_str(&second).expect("second frame JSON");
            assert_eq!(second.kind, "platform_duplex_client_frame");
            assert_eq!(second.id, open.id, "both turns must use one logical session");
            let second_turn_id = second.payload["_const_duplex_turn_id"]
                .as_str()
                .expect("second turn id");
            assert_ne!(second_turn_id, first_turn_id);
            let second_body: serde_json::Value = serde_json::from_str(
                second.payload["data"].as_str().expect("second frame data"),
            )
            .expect("second response.create JSON");
            assert_eq!(second_body["previous_response_id"], "resp-platform-tool");
            assert_eq!(second_body["input"][0]["type"], "custom_tool_call_output");
            assert_eq!(second_body["input"][0]["call_id"], "call-platform-tool");

            socket
                .send(WsMessage::Text(
                    serde_json::json!({
                        "id": open.id,
                        "type": "platform_duplex_server_frame",
                        "payload": {
                            "frame_type": "text",
                            "data": serde_json::json!({
                                "type": "response.completed",
                                "response": {
                                    "id": "resp-platform-final",
                                    "status": "completed",
                                    "output": []
                                }
                            }).to_string(),
                            "turn_terminal": true,
                            "status": 200,
                            "_const_duplex_turn_id": second_turn_id,
                            "_const_duplex_turn_seq": 1
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .expect("platform second-turn response");
        });

        let mut config = default_config();
        config.api_key = "sk-local-platform-duplex".to_string();
        config.account_device_api_key = "sk-platform-device".to_string();
        config.prefer_local_supply = true;
        config.channels.clear();
        config.endpoints = vec![Endpoint {
            server_id: String::new(),
            name: "mock-platform".to_string(),
            base_url: format!("http://{platform_address}"),
            supplier_ws_url: format!("ws://{platform_address}/supplier/ws"),
            supplier_quic_url: String::new(),
            enabled: true,
        }];
        let shared = ProxyShared::from_config(
            normalize_config(config),
            Client::new(),
        );
        *shared
            .platform_access_token
            .lock()
            .expect("platform access token") = "platform-access-token".to_string();
        let shared = Arc::new(shared);
        let routes = crate::proxy_routes(shared);
        let (proxy_address, proxy_server) =
            crate::bind_ephemeral!(routes, ([127, 0, 0, 1], 0));
        let proxy_task = tokio::spawn(proxy_server);

        let mut request = format!("ws://{proxy_address}/v1/responses")
            .into_client_request()
            .expect("client WebSocket request");
        request.headers_mut().insert(
            "authorization",
            "Bearer sk-local-platform-duplex"
                .parse()
                .expect("local authorization"),
        );
        let (mut client, _) = tokio_tungstenite::connect_async(request)
            .await
            .expect("connect through local proxy");
        client
            .send(WsMessage::Text(
                serde_json::json!({
                    "type": "response.create",
                    "model": "platform-model",
                    "store": false,
                    "stream": true,
                    "input": [{"role": "user", "content": "run the tool"}]
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("first response.create");

        let mut custom_call_seen = false;
        loop {
            let event = client
                .next()
                .await
                .expect("first turn response")
                .expect("first turn message")
                .into_text()
                .expect("first turn text");
            let event: serde_json::Value = serde_json::from_str(&event).expect("first event JSON");
            custom_call_seen |= event.get("type").and_then(serde_json::Value::as_str)
                == Some("response.output_item.done")
                && event.pointer("/item/call_id").and_then(serde_json::Value::as_str)
                    == Some("call-platform-tool");
            if event.get("type").and_then(serde_json::Value::as_str)
                == Some("response.completed")
            {
                break;
            }
        }
        assert!(custom_call_seen, "the platform response must preserve the tool call");

        client
            .send(WsMessage::Text(
                serde_json::json!({
                    "type": "response.create",
                    "model": "platform-model",
                    "store": false,
                    "stream": true,
                    "previous_response_id": "resp-platform-tool",
                    "input": [{
                        "type": "custom_tool_call_output",
                        "call_id": "call-platform-tool",
                        "output": "ok"
                    }]
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("tool result response.create");
        loop {
            let event = client
                .next()
                .await
                .expect("second turn response")
                .expect("second turn message")
                .into_text()
                .expect("second turn text");
            if serde_json::from_str::<serde_json::Value>(&event)
                .ok()
                .and_then(|event| event.get("type").cloned())
                == Some(serde_json::json!("response.completed"))
            {
                break;
            }
        }

        platform_task.await.expect("platform task");
        let _ = client.close(None).await;
        proxy_task.abort();
    }

    #[tokio::test]
    async fn openai_realtime_websocket_preserves_query_headers_and_native_tool_events() {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let upstream_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("Realtime upstream listener");
        let upstream_address = upstream_listener.local_addr().expect("upstream address");
        let handshake_verified = Arc::new(StdMutex::new(false));
        let handshake_verified_task = handshake_verified.clone();
        let upstream_task = tokio::spawn(async move {
            let (stream, _) = upstream_listener.accept().await.expect("upstream accept");
            let mut websocket = tokio_tungstenite::accept_hdr_async(
                stream,
                move |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                      response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                    let query = request.uri().query().unwrap_or_default();
                    let verified = request.uri().path() == "/v1/realtime"
                        && query.contains("model=gpt-realtime-test")
                        && query.contains("future=x%20y")
                        && request
                            .headers()
                            .get("authorization")
                            .and_then(|value| value.to_str().ok())
                            == Some("Bearer realtime-upstream-key")
                        && request
                            .headers()
                            .get("openai-safety-identifier")
                            .and_then(|value| value.to_str().ok())
                            == Some("stable-user-hash");
                    *handshake_verified_task.lock().expect("handshake state") = verified;
                    Ok(response)
                },
            )
            .await
            .expect("Realtime upstream handshake");
            websocket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    serde_json::json!({
                        "type": "session.created",
                        "session": {"id": "sess_realtime_owner", "model": "gpt-realtime-test"},
                        "future": {"preserved": true}
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .expect("session.created");
            let update = websocket
                .next()
                .await
                .expect("session update")
                .expect("session update message")
                .into_text()
                .expect("session update text");
            let update: serde_json::Value =
                serde_json::from_str(&update).expect("session update JSON");
            assert_eq!(update["session"]["tools"][0]["name"], "weather");
            assert_eq!(update["session"]["audio"]["input"]["format"]["type"], "audio/pcm");
            websocket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    serde_json::json!({
                        "type": "response.function_call_arguments.done",
                        "call_id": "call_native_1",
                        "name": "weather",
                        "arguments": "{\"city\":\"Shanghai\"}",
                        "future": {"preserved": true}
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .expect("tool event");
            websocket
                .send(tokio_tungstenite::tungstenite::Message::Close(None))
                .await
                .ok();
        });

        let mut config = default_config();
        config.api_key = "local-realtime-key".to_string();
        config.account_device_api_key.clear();
        config.prefer_local_supply = true;
        let mut selected = openai_api_channel(
            "realtime-channel",
            "gpt-realtime-test",
            &format!("http://{upstream_address}/v1"),
        );
        selected.upstream_api_key = "realtime-upstream-key".to_string();
        let mut wrong =
            openai_api_channel("wrong-realtime-channel", "other-model", "http://127.0.0.1:9/v1");
        wrong.upstream_api_key = "must-not-be-used".to_string();
        config.channels = vec![wrong, selected];
        let shared = Arc::new(ProxyShared::from_config(
            normalize_config(config),
            Client::new(),
        ));
        let routes = crate::proxy_routes(shared.clone());
        let (proxy_address, proxy_server) =
            crate::bind_ephemeral!(routes, ([127, 0, 0, 1], 0));
        let proxy_task = tokio::spawn(proxy_server);

        let mut request =
            format!("ws://{proxy_address}/v1/realtime?model=gpt-realtime-test&future=x%20y")
                .into_client_request()
                .expect("Realtime client request");
        request.headers_mut().insert(
            "authorization",
            "Bearer local-realtime-key".parse().expect("local auth"),
        );
        request.headers_mut().insert(
            "openai-safety-identifier",
            "stable-user-hash".parse().expect("safety identifier"),
        );
        let (mut client, _) = tokio_tungstenite::connect_async(request)
            .await
            .expect("connect Realtime through proxy");
        let created = client
            .next()
            .await
            .expect("session.created frame")
            .expect("session.created message")
            .into_text()
            .expect("session.created text");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&created)
                .expect("session.created JSON")["future"]["preserved"],
            true
        );
        client
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::json!({
                    "type": "session.update",
                    "session": {
                        "type": "realtime",
                        "audio": {"input": {"format": {"type": "audio/pcm", "rate": 24000}}},
                        "tools": [{
                            "type": "function",
                            "name": "weather",
                            "description": "weather",
                            "parameters": {"type": "object"}
                        }]
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("send session.update");
        let tool = client
            .next()
            .await
            .expect("tool frame")
            .expect("tool message")
            .into_text()
            .expect("tool text");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&tool).expect("tool JSON")["future"]
                ["preserved"],
            true
        );
        upstream_task.await.expect("upstream task");
        assert!(*handshake_verified.lock().expect("handshake state"));
        let owner = shared
            .local_resource_owners
            .get(
                crate::surface::ApiSurface::OpenAi,
                "realtime_session",
                "sess_realtime_owner",
            )
            .expect("Realtime session owner");
        assert_eq!(owner.channel_id, "realtime-channel");
        proxy_task.abort();
    }

    #[tokio::test]
    async fn codex_live_call_and_sideband_share_the_created_channel_owner() {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let call = warp::post()
            .and(warp::path!("v1" / "live"))
            .and(warp::path::end())
            .and(warp::header::exact(
                "authorization",
                "Bearer live-upstream-key",
            ))
            .map(|| {
                warp::reply::with_header(
                    "v=answer\r\n",
                    "location",
                    "/v1/live/rtc_owned",
                )
            });
        let sideband = warp::get()
            .and(warp::path!("v1" / "live" / "rtc_owned"))
            .and(warp::path::end())
            .and(warp::header::exact(
                "authorization",
                "Bearer live-upstream-key",
            ))
            .and(warp::ws())
            .map(|websocket: warp::ws::Ws| {
                websocket.on_upgrade(|socket| async move {
                    let (mut sender, mut receiver) = socket.split();
                    sender
                        .send(warp::ws::Message::text(
                            serde_json::json!({
                                "type": "session.started",
                                "session": {"id": "rtc_owned"}
                            })
                            .to_string(),
                        ))
                        .await
                        .expect("send session.started");
                    let frame = receiver
                        .next()
                        .await
                        .expect("client frame")
                        .expect("client frame message");
                    sender.send(frame).await.expect("echo client frame");
                })
            });
        let (upstream_address, upstream_server) =
            crate::bind_ephemeral!(call.or(sideband), ([127, 0, 0, 1], 0));
        let upstream_task = tokio::spawn(upstream_server);

        let mut config = default_config();
        config.api_key = "live-local-key".to_string();
        config.account_device_api_key.clear();
        config.prefer_local_supply = true;
        let mut channel = openai_api_channel(
            "live-owner-channel",
            "gpt-live-test",
            &format!("http://{upstream_address}/v1"),
        );
        channel.upstream_api_key = "live-upstream-key".to_string();
        config.channels = vec![channel];
        let shared = Arc::new(ProxyShared::from_config(
            normalize_config(config),
            Client::new(),
        ));
        let (proxy_address, proxy_server) =
            crate::bind_ephemeral!(crate::proxy_routes(shared.clone()), ([127, 0, 0, 1], 0));
        let proxy_task = tokio::spawn(proxy_server);

        let boundary = "codex-realtime-call-boundary";
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"sdp\"\r\nContent-Type: application/sdp\r\n\r\nv=offer\r\n\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"session\"\r\nContent-Type: application/json\r\n\r\n{{\"model\":\"gpt-realtime\",\"delegation\":{{\"type\":\"client\"}}}}\r\n--{boundary}--\r\n"
        );
        let response = Client::new()
            .post(format!("http://{proxy_address}/v1/live"))
            .header("authorization", "Bearer live-local-key")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body)
            .send()
            .await
            .expect("create Live call through proxy");
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok()),
            Some("/v1/live/rtc_owned")
        );
        assert_eq!(response.text().await.unwrap(), "v=answer\r\n");
        assert_eq!(
            shared
                .local_resource_owners
                .get(
                    crate::surface::ApiSurface::OpenAi,
                    "realtime_call",
                    "rtc_owned",
                )
                .expect("Live call owner")
                .channel_id,
            "live-owner-channel"
        );

        let mut request = format!("ws://{proxy_address}/v1/live/rtc_owned")
            .into_client_request()
            .expect("Live client request");
        request.headers_mut().insert(
            "authorization",
            "Bearer live-local-key".parse().expect("local auth"),
        );
        let (mut client, _) = tokio_tungstenite::connect_async(request)
            .await
            .expect("connect Live sideband through proxy");
        let started = client
            .next()
            .await
            .expect("session.started")
            .expect("session.started message")
            .into_text()
            .expect("session.started text");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&started).unwrap()["type"],
            "session.started"
        );
        client
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::json!({"type":"input_audio.append","audio":"AA=="})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("send Live frame");
        let echoed = client
            .next()
            .await
            .expect("echoed frame")
            .expect("echoed message")
            .into_text()
            .expect("echoed text");
        assert!(echoed.contains("input_audio.append"));

        proxy_task.abort();
        upstream_task.abort();
    }

    #[tokio::test]
    async fn openai_realtime_translation_websocket_uses_the_official_native_path() {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let upstream_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("translation upstream listener");
        let upstream_address = upstream_listener.local_addr().expect("upstream address");
        let upstream_task = tokio::spawn(async move {
            let (stream, _) = upstream_listener.accept().await.expect("upstream accept");
            let mut websocket = tokio_tungstenite::accept_hdr_async(
                stream,
                |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                 response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                    assert_eq!(request.uri().path(), "/v1/realtime/translations");
                    assert!(request
                        .uri()
                        .query()
                        .unwrap_or_default()
                        .contains("model=gpt-realtime-translate"));
                    assert_eq!(
                        request
                            .headers()
                            .get("authorization")
                            .and_then(|value| value.to_str().ok()),
                        Some("Bearer translation-upstream-key")
                    );
                    Ok(response)
                },
            )
            .await
            .expect("translation upstream handshake");
            websocket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    serde_json::json!({
                        "type": "session.created",
                        "session": {"id": "sess_translation_owner"}
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .expect("session.created");
            let event = websocket
                .next()
                .await
                .expect("translation event")
                .expect("valid translation event");
            assert_eq!(
                event.into_text().expect("text event"),
                r#"{"type":"input_audio_buffer.append","audio":"AA=="}"#
            );
        });

        let mut config = default_config();
        config.api_key = "local-translation-key".to_string();
        config.account_device_api_key.clear();
        config.prefer_local_supply = true;
        let mut channel = openai_api_channel(
            "translation-channel",
            "gpt-realtime-translate",
            &format!("http://{upstream_address}/v1"),
        );
        channel.upstream_api_key = "translation-upstream-key".to_string();
        config.channels = vec![channel];
        let shared = Arc::new(ProxyShared::from_config(
            normalize_config(config),
            Client::new(),
        ));
        let (proxy_address, proxy_server) =
            crate::bind_ephemeral!(crate::proxy_routes(shared.clone()), ([127, 0, 0, 1], 0));
        let proxy_task = tokio::spawn(proxy_server);

        let mut request = format!(
            "ws://{proxy_address}/v1/realtime/translations?model=gpt-realtime-translate"
        )
        .into_client_request()
        .expect("translation client request");
        request.headers_mut().insert(
            "authorization",
            "Bearer local-translation-key".parse().expect("local auth"),
        );
        let (mut client, _) = tokio_tungstenite::connect_async(request)
            .await
            .expect("connect translation through proxy");
        let created = client
            .next()
            .await
            .expect("session.created frame")
            .expect("session.created message");
        assert!(created.into_text().expect("created text").contains("session.created"));
        client
            .send(tokio_tungstenite::tungstenite::Message::Text(
                r#"{"type":"input_audio_buffer.append","audio":"AA=="}"#.into(),
            ))
            .await
            .expect("send translation event");

        upstream_task.await.expect("upstream task");
        let owner = shared
            .local_resource_owners
            .get(
                crate::surface::ApiSurface::OpenAi,
                "realtime_translation_session",
                "sess_translation_owner",
            )
            .expect("translation session owner");
        assert_eq!(owner.channel_id, "translation-channel");
        proxy_task.abort();
    }

    #[tokio::test]
    async fn gemini_live_websocket_keeps_auth_private_and_resumption_hard_affinity() {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let upstream_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("Gemini Live upstream listener");
        let upstream_address = upstream_listener.local_addr().expect("upstream address");
        let handshakes = Arc::new(StdMutex::new(0usize));
        let handshakes_task = handshakes.clone();
        let upstream_task = tokio::spawn(async move {
            for index in 0..2 {
                let (stream, _) = upstream_listener.accept().await.expect("upstream accept");
                let handshakes_task = handshakes_task.clone();
                let mut websocket = tokio_tungstenite::accept_hdr_async(
                    stream,
                    move |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                          response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                        assert_eq!(
                            request.uri().path(),
                            "/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent"
                        );
                        let parameters = reqwest::Url::parse(&format!(
                            "http://localhost/?{}",
                            request.uri().query().unwrap_or_default()
                        ))
                        .expect("query URL")
                        .query_pairs()
                        .map(|(name, value)| (name.into_owned(), value.into_owned()))
                        .collect::<HashMap<_, _>>();
                        assert_eq!(
                            parameters.get("key").map(String::as_str),
                            Some("gemini-upstream-key")
                        );
                        assert_eq!(parameters.get("alt").map(String::as_str), Some("json"));
                        assert!(!request
                            .uri()
                            .query()
                            .unwrap_or_default()
                            .contains("local-gemini-key"));
                        *handshakes_task.lock().expect("handshakes") += 1;
                        Ok(response)
                    },
                )
                .await
                .expect("Gemini Live upstream handshake");
                let setup = websocket
                    .next()
                    .await
                    .expect("setup frame")
                    .expect("setup message")
                    .into_text()
                    .expect("setup text");
                let setup: serde_json::Value =
                    serde_json::from_str(&setup).expect("setup JSON");
                assert_eq!(setup["setup"]["model"], "models/gemini-live-test");
                assert_eq!(setup["setup"]["future"]["preserved"], true);
                if index == 1 {
                    assert_eq!(
                        setup["setup"]["sessionResumption"]["handle"],
                        "live_resume_owner"
                    );
                }
                websocket
                    .send(tokio_tungstenite::tungstenite::Message::Text(
                        serde_json::json!({"setupComplete": {}})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .expect("setup complete");
                if index == 0 {
                    websocket
                        .send(tokio_tungstenite::tungstenite::Message::Text(
                            serde_json::json!({
                                "sessionResumptionUpdate": {
                                    "resumable": true,
                                    "newHandle": "live_resume_owner"
                                },
                                "future": {"preserved": true}
                            })
                            .to_string()
                            .into(),
                        ))
                        .await
                        .expect("resumption update");
                }
                websocket
                    .send(tokio_tungstenite::tungstenite::Message::Close(None))
                    .await
                    .ok();
            }
        });

        let mut config = default_config();
        config.api_key = "local-gemini-key".to_string();
        config.account_device_api_key.clear();
        config.prefer_local_supply = true;
        let mut channel = gemini_api_channel(
            "gemini-live-channel",
            "gemini-live-test",
            &format!("http://{upstream_address}/v1beta"),
        );
        channel.upstream_api_key = "gemini-upstream-key".to_string();
        config.channels = vec![channel.clone()];
        let shared = Arc::new(ProxyShared::from_config(
            normalize_config(config),
            Client::new(),
        ));
        let routes = crate::proxy_routes(shared.clone());
        let (proxy_address, proxy_server) =
            crate::bind_ephemeral!(routes, ([127, 0, 0, 1], 0));
        let proxy_task = tokio::spawn(proxy_server);
        let live_path =
            "/gemini/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent";

        let (mut first_client, _) = tokio_tungstenite::connect_async(
            format!(
                "ws://{proxy_address}{live_path}?key=local-gemini-key&alt=json"
            )
            .into_client_request()
            .expect("first Live request"),
        )
        .await
        .expect("first Live connection");
        first_client
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::json!({
                    "setup": {
                        "model": "models/gemini-live-test",
                        "generationConfig": {"responseModalities": ["AUDIO"]},
                        "sessionResumption": {},
                        "tools": [{"functionDeclarations": [{"name": "weather"}]}],
                        "future": {"preserved": true}
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("first setup");
        let _ = first_client.next().await.expect("setup complete");
        let resumption = first_client
            .next()
            .await
            .expect("resumption frame")
            .expect("resumption message")
            .into_text()
            .expect("resumption text");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&resumption)
                .expect("resumption JSON")["future"]["preserved"],
            true
        );
        let _ = first_client.next().await;
        assert_eq!(
            shared
                .local_resource_owners
                .get(
                    crate::surface::ApiSurface::Gemini,
                    "live_session",
                    "live_resume_owner",
                )
                .expect("Live session owner")
                .channel_id,
            "gemini-live-channel"
        );

        let mut updated = shared.config_snapshot();
        let mut wrong = gemini_api_channel(
            "wrong-live-channel",
            "gemini-live-test",
            "http://127.0.0.1:9/v1beta",
        );
        wrong.upstream_api_key = "wrong-key".to_string();
        updated.channels.push(wrong);
        *shared.live_config.lock().expect("live config") = normalize_config(updated);

        let (mut second_client, _) = tokio_tungstenite::connect_async(
            format!(
                "ws://{proxy_address}{live_path}?key=local-gemini-key&alt=json"
            )
            .into_client_request()
            .expect("second Live request"),
        )
        .await
        .expect("second Live connection");
        second_client
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::json!({
                    "setup": {
                        "model": "models/gemini-live-test",
                        "sessionResumption": {"handle": "live_resume_owner"},
                        "future": {"preserved": true}
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("resumption setup");
        let setup_complete = second_client
            .next()
            .await
            .expect("second setup complete")
            .expect("second setup message")
            .into_text()
            .expect("second setup text");
        assert!(serde_json::from_str::<serde_json::Value>(&setup_complete)
            .expect("second setup JSON")
            .get("setupComplete")
            .is_some());
        upstream_task.await.expect("upstream task");
        assert_eq!(*handshakes.lock().expect("handshakes"), 2);
        proxy_task.abort();
    }

    #[tokio::test]
    async fn file_upload_and_download_stream_incrementally_and_keep_hard_owner() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("file upstream listener");
        let upstream_address = listener.local_addr().expect("file upstream address");
        let (first_upload_chunk_tx, first_upload_chunk_rx) = tokio::sync::oneshot::channel();
        let (release_download_tail_tx, release_download_tail_rx) =
            tokio::sync::oneshot::channel();
        let captured_upload = Arc::new(StdMutex::new(Vec::<u8>::new()));
        let captured_upload_task = captured_upload.clone();
        let captured_requests = Arc::new(StdMutex::new(Vec::<String>::new()));
        let captured_requests_task = captured_requests.clone();
        let upstream_task = tokio::spawn(async move {
            let mut first_upload_chunk_tx = Some(first_upload_chunk_tx);
            let (mut upload, _) = listener.accept().await.expect("accept upload");
            let mut request = Vec::new();
            let header_end = loop {
                let mut chunk = [0_u8; 2048];
                let read = upload.read(&mut chunk).await.expect("read upload headers");
                assert!(read > 0, "upload closed before headers");
                request.extend_from_slice(&chunk[..read]);
                if let Some(index) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    break index + 4;
                }
            };
            let header_text =
                std::str::from_utf8(&request[..header_end]).expect("upload header text");
            let content_length = header_text
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().expect("upload content length"))
                })
                .expect("streaming proxy preserves content length");
            assert!(header_text.starts_with("POST /v1/files HTTP/1.1\r\n"));
            assert!(
                header_text
                    .to_ascii_lowercase()
                    .contains("authorization: bearer sk-file-upstream\r\n"),
                "upstream API authorization must replace local authorization"
            );
            assert!(!header_text.contains("sk-file-local"));
            let mut body = request[header_end..].to_vec();
            if !body.is_empty() {
                if let Some(signal) = first_upload_chunk_tx.take() {
                    let _ = signal.send(());
                }
            }
            while body.len() < content_length {
                let mut chunk = [0_u8; 2048];
                let read = upload.read(&mut chunk).await.expect("read upload body");
                assert!(read > 0, "upload closed before full body");
                let first = body.is_empty();
                body.extend_from_slice(&chunk[..read]);
                if first {
                    if let Some(signal) = first_upload_chunk_tx.take() {
                        let _ = signal.send(());
                    }
                }
            }
            *captured_upload_task.lock().expect("captured upload") = body;
            captured_requests_task
                .lock()
                .expect("captured requests")
                .push("POST /v1/files".to_string());
            let response = br#"{"id":"file_stream_owned","object":"file","bytes":11,"purpose":"assistants"}"#;
            upload
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        response.len()
                    )
                    .as_bytes(),
                )
                .await
                .expect("write upload response headers");
            upload
                .write_all(response)
                .await
                .expect("write upload response");

            let (mut download, _) = listener.accept().await.expect("accept download");
            let mut headers = Vec::new();
            while !headers.windows(4).any(|part| part == b"\r\n\r\n") {
                let mut chunk = [0_u8; 1024];
                let read = download
                    .read(&mut chunk)
                    .await
                    .expect("read download request");
                assert!(read > 0, "download closed before headers");
                headers.extend_from_slice(&chunk[..read]);
            }
            let headers = std::str::from_utf8(&headers).expect("download headers");
            assert!(headers.starts_with(
                "GET /v1/files/file_stream_owned/content?alt=media&x=1 HTTP/1.1\r\n"
            ));
            captured_requests_task
                .lock()
                .expect("captured requests")
                .push("GET /v1/files/file_stream_owned/content?alt=media&x=1".to_string());
            download
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=\"owned.bin\"\r\nContent-Length: 6\r\nConnection: close\r\n\r\nabc",
                )
                .await
                .expect("write first download bytes");
            download.flush().await.expect("flush first download bytes");
            let _ = release_download_tail_rx.await;
            download
                .write_all(b"def")
                .await
                .expect("write download tail");
        });

        let b_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let b_calls_filter = b_calls.clone();
        let channel_b = warp::any().map(move || {
            b_calls_filter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            warp::reply::json(&serde_json::json!({"id":"wrong-channel"}))
        });
        let (channel_b_address, channel_b_server) =
            crate::bind_ephemeral!(channel_b, ([127, 0, 0, 1], 0));
        let channel_b_task = tokio::spawn(channel_b_server);

        let mut config = default_config();
        config.api_key = "sk-file-local".to_string();
        config.account_device_api_key.clear();
        config.prefer_local_supply = true;
        let mut channel_a = openai_api_channel(
            "file-channel-a",
            "unused-file-model",
            &format!("http://{upstream_address}/v1"),
        );
        channel_a.upstream_api_key = "sk-file-upstream".to_string();
        config.channels = vec![channel_a.clone()];
        let shared = Arc::new(ProxyShared::from_config(
            normalize_config(config),
            Client::new(),
        ));
        let routes = crate::proxy_routes(shared.clone());
        let (proxy_address, proxy_server) =
            crate::bind_ephemeral!(routes, ([127, 0, 0, 1], 0));
        let proxy_task = tokio::spawn(proxy_server);

        let first = bytes::Bytes::from_static(b"--boundary\r\ncontent-a");
        let second = bytes::Bytes::from_static(b"\r\ncontent-b\r\n--boundary--\r\n");
        let expected = [first.as_ref(), second.as_ref()].concat();
        let expected_length = expected.len();
        let (upload_tx, upload_rx) = tokio::sync::mpsc::channel::<
            std::result::Result<bytes::Bytes, std::io::Error>,
        >(2);
        upload_tx.send(Ok(first)).await.expect("queue first chunk");
        let upload_stream =
            futures_util::stream::unfold(upload_rx, |mut receiver| async move {
                receiver.recv().await.map(|chunk| (chunk, receiver))
            });
        let upload_client = Client::new();
        let upload_task = tokio::spawn(async move {
            upload_client
                .post(format!("http://{proxy_address}/v1/files"))
                .header("authorization", "Bearer sk-file-local")
                .header("content-type", "multipart/form-data; boundary=boundary")
                .header("content-length", expected_length)
                .body(reqwest::Body::wrap_stream(upload_stream))
                .send()
                .await
                .expect("stream file through proxy")
        });
        tokio::time::timeout(Duration::from_secs(2), first_upload_chunk_rx)
            .await
            .expect("upstream must receive first upload chunk before body completion")
            .expect("first upload signal");
        upload_tx.send(Ok(second)).await.expect("queue upload tail");
        drop(upload_tx);
        let upload_response = upload_task.await.expect("upload task");
        assert_eq!(upload_response.status(), reqwest::StatusCode::OK);
        let upload_json: serde_json::Value = upload_response
            .json()
            .await
            .expect("upload response json");
        assert_eq!(upload_json["id"], "file_stream_owned");
        assert_eq!(
            captured_upload.lock().expect("captured upload").as_slice(),
            expected.as_slice()
        );
        assert_eq!(
            shared
                .local_resource_owners
                .get(
                    crate::surface::ApiSurface::OpenAi,
                    "file",
                    "file_stream_owned"
                )
                .expect("file owner")
                .channel_id,
            "file-channel-a"
        );

        let channel_b_config = openai_api_channel(
            "file-channel-b",
            "unused-file-model",
            &format!("http://{channel_b_address}/v1"),
        );
        shared
            .live_config
            .lock()
            .expect("live config")
            .channels
            .push(channel_b_config.clone());
        shared
            .local_channel_readiness
            .lock()
            .expect("readiness")
            .insert(channel_b_config.id.clone(), true);

        let download = Client::new()
            .get(format!(
                "http://{proxy_address}/v1/files/file_stream_owned/content?alt=media&x=1"
            ))
            .header("authorization", "Bearer sk-file-local")
            .send();
        let download = tokio::time::timeout(Duration::from_secs(2), download)
            .await
            .expect("binary response headers must not wait for the tail")
            .expect("download through proxy");
        assert_eq!(download.status(), reqwest::StatusCode::OK);
        assert_eq!(
            download
                .headers()
                .get("content-disposition")
                .and_then(|value| value.to_str().ok()),
            Some("attachment; filename=\"owned.bin\"")
        );
        let mut stream = download.bytes_stream();
        let first_download = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("first binary chunk must stream before tail")
            .expect("first binary item")
            .expect("first binary bytes");
        assert_eq!(first_download, bytes::Bytes::from_static(b"abc"));
        release_download_tail_tx
            .send(())
            .expect("release download tail");
        let remaining = stream
            .try_fold(Vec::new(), |mut bytes, chunk| async move {
                bytes.extend_from_slice(&chunk);
                Ok(bytes)
            })
            .await
            .expect("download remaining bytes");
        assert_eq!(remaining, b"def");
        assert_eq!(b_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(
            captured_requests.lock().expect("captured requests").as_slice(),
            [
                "POST /v1/files",
                "GET /v1/files/file_stream_owned/content?alt=media&x=1"
            ]
        );

        upstream_task.await.expect("upstream file task");
        proxy_task.abort();
        channel_b_task.abort();
    }

    #[tokio::test]
    async fn gemini_resumable_upload_url_stays_local_and_final_file_keeps_hard_owner() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("Gemini upstream listener");
        let upstream_address = listener.local_addr().expect("Gemini upstream address");
        let upstream_task = tokio::spawn(async move {
            let (mut start, _) = listener.accept().await.expect("accept upload start");
            let start_request = read_raw_http_request(&mut start).await;
            assert_eq!(start_request.method, "POST");
            assert_eq!(start_request.target, "/upload/v1beta/files");
            assert!(start_request
                .headers
                .to_ascii_lowercase()
                .contains("x-goog-api-key: gemini-upstream-key\r\n"));
            assert!(start_request
                .headers
                .to_ascii_lowercase()
                .contains("x-goog-upload-command: start\r\n"));
            let upstream_upload_url =
                format!("http://{upstream_address}/upload/v1beta/files?upload_id=private-session");
            start
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nX-Goog-Upload-URL: {upstream_upload_url}\r\nX-Goog-Upload-Status: active\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .expect("write upload start response");

            let (mut upload, _) = listener.accept().await.expect("accept upload bytes");
            let upload_request = read_raw_http_request(&mut upload).await;
            assert_eq!(upload_request.method, "POST");
            assert_eq!(
                upload_request.target,
                "/upload/v1beta/files?upload_id=private-session"
            );
            assert_eq!(upload_request.body, b"media-bytes");
            let lower_headers = upload_request.headers.to_ascii_lowercase();
            assert!(lower_headers.contains("x-goog-upload-command: upload, finalize\r\n"));
            assert!(!lower_headers.contains("authorization:"));
            assert!(!lower_headers.contains("x-goog-api-key:"));
            let final_response = br#"{"file":{"name":"files/gemini_owned","uri":"https://generativelanguage.googleapis.com/v1beta/files/gemini_owned","mimeType":"audio/mpeg","state":"ACTIVE"}}"#;
            upload
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        final_response.len()
                    )
                    .as_bytes(),
                )
                .await
                .expect("write upload response headers");
            upload
                .write_all(final_response)
                .await
                .expect("write upload response");

            let (mut metadata, _) = listener.accept().await.expect("accept file metadata");
            let metadata_request = read_raw_http_request(&mut metadata).await;
            assert_eq!(metadata_request.method, "GET");
            assert_eq!(
                metadata_request.target,
                "/v1beta/files/gemini_owned?view=FULL"
            );
            assert!(metadata_request
                .headers
                .to_ascii_lowercase()
                .contains("x-goog-api-key: gemini-upstream-key\r\n"));
            let metadata_response =
                br#"{"name":"files/gemini_owned","state":"ACTIVE","mimeType":"audio/mpeg"}"#;
            metadata
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        metadata_response.len()
                    )
                    .as_bytes(),
                )
                .await
                .expect("write metadata response headers");
            metadata
                .write_all(metadata_response)
                .await
                .expect("write metadata response");
        });

        let wrong_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let wrong_calls_filter = wrong_calls.clone();
        let wrong = warp::any().map(move || {
            wrong_calls_filter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            warp::reply::json(&serde_json::json!({"name":"files/wrong"}))
        });
        let (wrong_address, wrong_server) =
            crate::bind_ephemeral!(wrong, ([127, 0, 0, 1], 0));
        let wrong_task = tokio::spawn(wrong_server);

        let mut config = default_config();
        config.api_key = "gemini-local-key".to_string();
        config.account_device_api_key.clear();
        config.prefer_local_supply = true;
        let mut channel = gemini_api_channel(
            "gemini-file-channel",
            "gemini-test",
            &format!("http://{upstream_address}"),
        );
        channel.upstream_api_key = "gemini-upstream-key".to_string();
        config.channels = vec![channel];
        let shared = Arc::new(ProxyShared::from_config(
            normalize_config(config),
            Client::new(),
        ));
        let routes = crate::proxy_routes(shared.clone());
        let (proxy_address, proxy_server) =
            crate::bind_ephemeral!(routes, ([127, 0, 0, 1], 0));
        let proxy_task = tokio::spawn(proxy_server);

        let start = Client::new()
            .post(format!(
                "http://{proxy_address}/gemini/upload/v1beta/files"
            ))
            .header("authorization", "Bearer gemini-local-key")
            .header("content-type", "application/json")
            .header("x-goog-upload-protocol", "resumable")
            .header("x-goog-upload-command", "start")
            .body(br#"{"file":{"display_name":"tiny.mp3"}}"#.to_vec())
            .send()
            .await
            .expect("start Gemini upload");
        assert_eq!(start.status(), reqwest::StatusCode::OK);
        let local_upload_url = start
            .headers()
            .get("x-goog-upload-url")
            .and_then(|value| value.to_str().ok())
            .expect("rewritten local upload URL")
            .to_string();
        assert!(local_upload_url.starts_with(&format!(
            "http://{proxy_address}/gemini/.upload-session/"
        )));
        assert!(!local_upload_url.contains("private-session"));

        let upload = Client::new()
            .post(&local_upload_url)
            .header("authorization", "Bearer gemini-local-key")
            .header("content-type", "audio/mpeg")
            .header("x-goog-upload-offset", "0")
            .header("x-goog-upload-command", "upload, finalize")
            .body("media-bytes")
            .send()
            .await
            .expect("upload Gemini bytes");
        assert_eq!(upload.status(), reqwest::StatusCode::OK);
        let uploaded: serde_json::Value = upload.json().await.expect("Gemini upload JSON");
        assert_eq!(uploaded["file"]["name"], "files/gemini_owned");
        assert_eq!(
            shared
                .local_resource_owners
                .get(
                    crate::surface::ApiSurface::Gemini,
                    "file",
                    "gemini_owned"
                )
                .expect("Gemini file owner")
                .channel_id,
            "gemini-file-channel"
        );

        let wrong_channel = gemini_api_channel(
            "gemini-file-wrong",
            "gemini-test",
            &format!("http://{wrong_address}"),
        );
        shared
            .live_config
            .lock()
            .expect("live config")
            .channels
            .push(wrong_channel.clone());
        shared
            .local_channel_readiness
            .lock()
            .expect("readiness")
            .insert(wrong_channel.id.clone(), true);

        let metadata = Client::new()
            .get(format!(
                "http://{proxy_address}/gemini/v1beta/files/gemini_owned?view=FULL"
            ))
            .header("authorization", "Bearer gemini-local-key")
            .send()
            .await
            .expect("get Gemini file through owner");
        assert_eq!(metadata.status(), reqwest::StatusCode::OK);
        assert_eq!(wrong_calls.load(std::sync::atomic::Ordering::SeqCst), 0);

        upstream_task.await.expect("Gemini upstream task");
        proxy_task.abort();
        wrong_task.abort();
    }

    #[derive(Debug)]
    struct RawRequestCapture {
        method: String,
        target: String,
        headers: String,
        body: Vec<u8>,
    }

    async fn read_raw_http_request(stream: &mut tokio::net::TcpStream) -> RawRequestCapture {
        let mut request = Vec::new();
        let header_end = loop {
            let mut chunk = [0_u8; 1024];
            let read = stream.read(&mut chunk).await.expect("read HTTP request");
            assert!(read > 0, "HTTP capture closed before headers");
            request.extend_from_slice(&chunk[..read]);
            if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let headers = std::str::from_utf8(&request[..header_end])
            .expect("captured HTTP headers")
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
                .expect("read captured HTTP body");
            assert!(read > 0, "HTTP capture closed before body");
            request.extend_from_slice(&chunk[..read]);
        }
        let mut request_line = headers
            .lines()
            .next()
            .expect("request line")
            .split_whitespace();
        RawRequestCapture {
            method: request_line.next().expect("method").to_string(),
            target: request_line.next().expect("target").to_string(),
            headers,
            body: request[header_end..header_end + content_length].to_vec(),
        }
    }

    async fn capture_one_http_request(
        response: Vec<u8>,
    ) -> (
        String,
        tokio::sync::oneshot::Receiver<RawRequestCapture>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind HTTP capture");
        let address = listener.local_addr().expect("HTTP capture address");
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let mut sender = Some(sender);
            loop {
                let (mut stream, _) = listener.accept().await.expect("captured HTTP request");
                let captured = read_raw_http_request(&mut stream).await;
                if captured.target == "/healthz" {
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                        )
                        .await
                        .expect("write health probe response");
                    continue;
                }
                sender
                    .take()
                    .expect("business capture sender")
                    .send(captured)
                    .expect("send HTTP capture");
                stream
                    .write_all(&response)
                    .await
                    .expect("write captured HTTP response");
                return;
            }
        });
        (format!("http://{address}"), receiver, task)
    }

    fn raw_header_values<'a>(headers: &'a str, name: &str) -> Vec<&'a str> {
        headers
            .lines()
            .filter_map(|line| {
                let (candidate, value) = line.split_once(':')?;
                candidate.eq_ignore_ascii_case(name).then(|| value.trim())
            })
            .collect()
    }

    fn decoded_raw_http_body(headers: &str, body: &[u8]) -> Vec<u8> {
        if !raw_header_values(headers, "transfer-encoding")
            .iter()
            .any(|value| value.eq_ignore_ascii_case("chunked"))
        {
            return body.to_vec();
        }

        let mut decoded = Vec::new();
        let mut cursor = 0;
        loop {
            let line_end = body[cursor..]
                .windows(2)
                .position(|window| window == b"\r\n")
                .map(|offset| cursor + offset)
                .expect("chunk size line");
            let size_text = std::str::from_utf8(&body[cursor..line_end])
                .expect("chunk size text")
                .split(';')
                .next()
                .unwrap()
                .trim();
            let size = usize::from_str_radix(size_text, 16).expect("chunk size");
            cursor = line_end + 2;
            if size == 0 {
                break;
            }
            let chunk_end = cursor + size;
            decoded.extend_from_slice(&body[cursor..chunk_end]);
            assert_eq!(&body[chunk_end..chunk_end + 2], b"\r\n");
            cursor = chunk_end + 2;
        }
        decoded
    }

    fn verified_openai_channel(
        id: &str,
        models: &[&str],
        upstream_base_url: &str,
    ) -> ChannelConfig {
        let mut channel = local_model_channel(models, upstream_base_url);
        channel.id = id.to_string();
        channel.node_id = format!("node-{id}");
        channel.name = id.to_string();
        channel.surface_bindings = vec![ChannelSurfaceBinding {
            surface: crate::surface::ApiSurface::OpenAi,
            base_url: upstream_base_url.to_string(),
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
        channel
    }

    #[tokio::test]
    async fn local_proxy_distinguishes_bare_query_delimiter_from_no_query() {
        let upstream = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind upstream capture");
        let upstream_addr = upstream.local_addr().expect("upstream address");
        let (target_tx, target_rx) = tokio::sync::oneshot::channel();
        let upstream_task = tokio::spawn(async move {
            let mut targets = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = upstream.accept().await.expect("upstream request");
                let mut request = Vec::new();
                let header_end = loop {
                    let mut chunk = [0_u8; 1024];
                    let read = stream
                        .read(&mut chunk)
                        .await
                        .expect("read upstream request");
                    assert!(read > 0, "upstream connection closed before headers");
                    request.extend_from_slice(&chunk[..read]);
                    if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n")
                    {
                        break index + 4;
                    }
                };
                let headers = std::str::from_utf8(&request[..header_end])
                    .expect("request headers")
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
                    let read = stream.read(&mut chunk).await.expect("read upstream body");
                    assert!(read > 0, "upstream connection closed before body");
                    request.extend_from_slice(&chunk[..read]);
                }
                targets.push(
                    headers
                        .lines()
                        .next()
                        .and_then(|line| line.split_whitespace().nth(1))
                        .expect("request target")
                        .to_string(),
                );
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                    )
                    .await
                    .expect("write upstream response");
            }
            target_tx.send(targets).expect("send captured targets");
        });

        let mut config = default_config();
        config.listen = "127.0.0.1:0".to_string();
        config.api_key = "sk-bare-query".to_string();
        config.account_device_api_key.clear();
        config.prefer_local_supply = true;
        config.endpoints.clear();
        config.channels = vec![verified_openai_channel(
            "bare-query",
            &["bare-query-model"],
            &format!("http://{upstream_addr}"),
        )];
        let shared = Arc::new(ProxyShared::from_config(
            normalize_config(config),
            Client::new(),
        ));
        let (proxy_addr, proxy_server) =
            crate::bind_ephemeral!(crate::proxy_routes(shared), ([127, 0, 0, 1], 0));
        tokio::spawn(proxy_server);

        let body = br#"{"model":"bare-query-model","messages":[]}"#;
        for request_target in ["/v1/chat/completions?", "/v1/chat/completions"] {
            let mut client = tokio::net::TcpStream::connect(proxy_addr)
                .await
                .expect("connect proxy");
            let request = format!(
                "POST {request_target} HTTP/1.1\r\nHost: {proxy_addr}\r\nAuthorization: Bearer sk-bare-query\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                std::str::from_utf8(body).unwrap()
            );
            client
                .write_all(request.as_bytes())
                .await
                .expect("write raw proxy request");
            let mut response = Vec::new();
            client
                .read_to_end(&mut response)
                .await
                .expect("read proxy response");
        }

        let targets = target_rx.await.expect("captured upstream targets");
        upstream_task.await.expect("upstream task");
        assert_eq!(
            targets,
            vec!["/v1/chat/completions?", "/v1/chat/completions"]
        );
    }

    #[tokio::test]
    async fn platform_endpoint_preserves_bare_query_delimiter_from_ingress() {
        let body = br#"{"object":"list","data":[]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(body).unwrap()
        )
        .into_bytes();
        let (platform_base, target_rx, platform_task) = capture_one_http_request(response).await;

        let mut config = default_config();
        config.listen = "127.0.0.1:0".to_string();
        config.api_key = "sk-platform-query".to_string();
        config.account_device_api_key = "sk-platform-device".to_string();
        config.prefer_local_supply = false;
        config.channels.clear();
        config.endpoints = vec![Endpoint {
            server_id: String::new(),
            name: "platform-capture".to_string(),
            base_url: platform_base,
            supplier_ws_url: String::new(),
            supplier_quic_url: String::new(),
            enabled: true,
        }];
        let shared = Arc::new(ProxyShared::from_config(
            normalize_config(config),
            Client::new(),
        ));
        let (proxy_addr, proxy_server) =
            crate::bind_ephemeral!(crate::proxy_routes(shared), ([127, 0, 0, 1], 0));
        tokio::spawn(proxy_server);

        let mut client = tokio::net::TcpStream::connect(proxy_addr)
            .await
            .expect("connect proxy");
        let request = format!(
            "GET /v1/models? HTTP/1.1\r\nHost: {proxy_addr}\r\nAuthorization: Bearer sk-platform-query\r\nConnection: close\r\n\r\n"
        );
        client
            .write_all(request.as_bytes())
            .await
            .expect("write raw proxy request");
        let mut response = Vec::new();
        client
            .read_to_end(&mut response)
            .await
            .expect("read proxy response");

        let target = target_rx.await.expect("captured platform target").target;
        platform_task.await.expect("platform task");
        assert_eq!(target, "/v1/models?");
    }

    #[tokio::test]
    async fn platform_endpoint_splits_request_and_response_header_sanitization() {
        let response_body = br#"{"ok":true}"#;
        let mut upstream_response = format!(
            concat!(
                "HTTP/1.1 200 OK\r\n",
                "Content-Type: application/json\r\n",
                "Content-Length: {}\r\n",
                "Connection: X-Remove-Response\r\n",
                "Keep-Alive: timeout=5\r\n",
                "Proxy-Authenticate: Basic realm=upstream\r\n",
                "Proxy-Authorization: Basic upstream-secret\r\n",
                "TE: trailers\r\n",
                "Trailer: X-Trailer\r\n",
                "Proxy-Connection: keep-alive\r\n",
                "X-Remove-Response: connection-secret\r\n",
                "Authorization: Bearer upstream-secret\r\n",
                "Api-Key: upstream-secret\r\n",
                "X-Api-Key: upstream-secret\r\n",
                "X-Goog-Api-Key: upstream-secret\r\n",
                "Cookie: upstream-session=secret\r\n",
                "Set-Cookie: upstream-session=secret\r\n",
                "X-End-To-End: first\r\n",
                "X-End-To-End: second\r\n",
                "\r\n"
            ),
            response_body.len()
        )
        .into_bytes();
        upstream_response.extend_from_slice(response_body);
        let (platform_base, capture_rx, platform_task) =
            capture_one_http_request(upstream_response).await;

        let mut config = default_config();
        config.listen = "127.0.0.1:0".to_string();
        config.api_key = "sk-platform-ingress".to_string();
        config.account_device_api_key = "sk-platform-upstream".to_string();
        config.prefer_local_supply = false;
        // A legacy disabled preference must not suppress custom fallback.
        config.allow_unverified_platform_routes = false;
        config.channels.clear();
        config.endpoints = vec![Endpoint {
            server_id: String::new(),
            name: "platform-capture".to_string(),
            base_url: platform_base,
            supplier_ws_url: String::new(),
            supplier_quic_url: String::new(),
            enabled: true,
        }];
        let shared = Arc::new(ProxyShared::from_config(
            normalize_config(config),
            Client::new(),
        ));
        let (proxy_addr, proxy_server) =
            crate::bind_ephemeral!(crate::proxy_routes(shared), ([127, 0, 0, 1], 0));
        tokio::spawn(proxy_server);

        let mut client = tokio::net::TcpStream::connect(proxy_addr)
            .await
            .expect("connect platform proxy");
        let request = format!(
            concat!(
                "GET /v1/models? HTTP/1.1\r\n",
                "Host: {0}\r\n",
                "Authorization: Bearer sk-platform-ingress\r\n",
                "Connection: close, X-Remove-Request\r\n",
                "Keep-Alive: timeout=5\r\n",
                "Proxy-Authenticate: Basic realm=inbound\r\n",
                "Proxy-Authorization: Basic inbound-secret\r\n",
                "TE: trailers\r\n",
                "Trailer: X-Trailer\r\n",
                "Proxy-Connection: keep-alive\r\n",
                "X-Remove-Request: connection-secret\r\n",
                "Api-Key: inbound-secret\r\n",
                "X-Api-Key: inbound-secret\r\n",
                "X-Goog-Api-Key: inbound-secret\r\n",
                "X-Const-Api-Max-Price-Ratio: 9\r\n",
                "X-Const-Api-Allow-Unverified-Routes: 0\r\n",
                "Accept-Language: ja-JP\r\n",
                "Cookie: provider-session=inbound-secret\r\n",
                "Set-Cookie: provider-session=inbound-secret\r\n",
                "X-End-To-End: request-first\r\n",
                "X-End-To-End: request-second\r\n",
                "\r\n"
            ),
            proxy_addr
        );
        client
            .write_all(request.as_bytes())
            .await
            .expect("write platform proxy request");
        let mut response = Vec::new();
        client
            .read_to_end(&mut response)
            .await
            .expect("read platform proxy response");

        let captured = capture_rx.await.expect("captured platform request");
        platform_task.await.expect("platform capture task");
        for name in [
            "connection",
            "keep-alive",
            "proxy-authenticate",
            "proxy-authorization",
            "te",
            "trailer",
            "transfer-encoding",
            "upgrade",
            "proxy-connection",
            "x-remove-request",
            "api-key",
            "x-api-key",
            "x-goog-api-key",
        ] {
            assert!(
                raw_header_values(&captured.headers, name).is_empty(),
                "request header {name} reached the platform"
            );
        }
        assert_eq!(
            raw_header_values(&captured.headers, "authorization"),
            ["Bearer sk-platform-upstream"]
        );
        assert_eq!(
            raw_header_values(&captured.headers, "x-end-to-end"),
            ["request-first", "request-second"]
        );
        assert_eq!(
            raw_header_values(&captured.headers, MAX_PRICE_RATIO_HEADER),
            Vec::<String>::new()
        );
        assert_eq!(
            raw_header_values(
                &captured.headers,
                ALLOW_UNVERIFIED_PLATFORM_ROUTES_HEADER
            ),
            ["1"]
        );
        assert_eq!(
            raw_header_values(&captured.headers, "accept-language"),
            [crate::native_i18n::display_language_tag()]
        );

        let response_header_end = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("platform proxy response headers")
            + 4;
        let response_headers = String::from_utf8_lossy(&response[..response_header_end]);
        assert_eq!(
            raw_header_values(&response_headers, "cookie"),
            ["upstream-session=secret"]
        );
        assert_eq!(
            raw_header_values(&response_headers, "set-cookie"),
            ["upstream-session=secret"]
        );
        for name in [
            "keep-alive",
            "proxy-authenticate",
            "proxy-authorization",
            "te",
            "trailer",
            "upgrade",
            "proxy-connection",
            "x-remove-response",
            "authorization",
            "api-key",
            "x-api-key",
            "x-goog-api-key",
        ] {
            assert!(
                raw_header_values(&response_headers, name).is_empty(),
                "response header {name} reached the caller"
            );
        }
        assert_eq!(
            raw_header_values(&response_headers, "x-end-to-end"),
            ["first", "second"]
        );
    }

    #[tokio::test]
    async fn local_opaque_json_ingress_selects_verified_surface_by_shallow_model() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".to_vec();
        let (other_base, mut other_rx, other_task) =
            capture_one_http_request(response.clone()).await;
        let (selected_base, mut selected_rx, selected_task) =
            capture_one_http_request(response).await;

        let mut config = default_config();
        config.listen = "127.0.0.1:0".to_string();
        config.api_key = "sk-opaque-json".to_string();
        config.account_device_api_key.clear();
        config.prefer_local_supply = true;
        config.endpoints.clear();
        config.channels = vec![
            verified_openai_channel("other-channel", &["other-model"], &other_base),
            verified_openai_channel("selected-channel", &["opaque-target"], &selected_base),
        ];
        let shared = Arc::new(ProxyShared::from_config(
            normalize_config(config),
            Client::new(),
        ));
        let (proxy_addr, proxy_server) =
            crate::bind_ephemeral!(crate::proxy_routes(shared), ([127, 0, 0, 1], 0));
        tokio::spawn(proxy_server);

        let body = br#"{ "model" : "opaque-target", "future" : {"keep":true} }"#;
        let mut client = tokio::net::TcpStream::connect(proxy_addr)
            .await
            .expect("connect opaque JSON proxy");
        let request = format!(
            "PATCH /v1/future-operation?mode=raw%2Fvalue HTTP/1.1\r\nHost: {proxy_addr}\r\nAuthorization: Bearer sk-opaque-json\r\nContent-Type: application/vnd.future+json\r\nX-Opaque-Marker: preserve-me\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(body).unwrap()
        );
        client
            .write_all(request.as_bytes())
            .await
            .expect("write opaque JSON ingress");
        let mut response = Vec::new();
        client
            .read_to_end(&mut response)
            .await
            .expect("read opaque JSON response");

        let selected = tokio::select! {
            capture = &mut selected_rx => ("selected", capture.expect("selected capture")),
            capture = &mut other_rx => ("other", capture.expect("other capture")),
            _ = tokio::time::sleep(Duration::from_secs(2)) => panic!("opaque ingress did not reach a local channel"),
        };
        assert_eq!(selected.0, "selected");
        assert_eq!(selected.1.method, "PATCH");
        assert_eq!(selected.1.target, "/v1/future-operation?mode=raw%2Fvalue");
        assert!(selected
            .1
            .headers
            .to_ascii_lowercase()
            .contains("x-opaque-marker: preserve-me"));
        assert_eq!(selected.1.body, body);
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 200"));

        selected_task.await.expect("selected upstream task");
        other_task.abort();
    }

    #[tokio::test]
    async fn local_opaque_binary_ingress_uses_unique_verified_surface_without_default_content_type()
    {
        let response_body = [0_u8, 255, 16, 128];
        let mut raw_response =
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\n".to_vec();
        raw_response.extend_from_slice(&response_body);
        let (upstream_base, capture_rx, mut upstream_task) =
            capture_one_http_request(raw_response).await;

        let mut config = default_config();
        config.listen = "127.0.0.1:0".to_string();
        config.api_key = "sk-opaque-binary".to_string();
        config.account_device_api_key.clear();
        config.prefer_local_supply = true;
        config.endpoints.clear();
        config.channels = vec![verified_openai_channel(
            "unique-openai",
            &["unused-for-binary"],
            &upstream_base,
        )];
        let shared = Arc::new(ProxyShared::from_config(
            normalize_config(config),
            Client::new(),
        ));
        let (proxy_addr, proxy_server) =
            crate::bind_ephemeral!(crate::proxy_routes(shared), ([127, 0, 0, 1], 0));
        let proxy_task = tokio::spawn(proxy_server);

        let body = [255_u8, 0, 129, b'{', b'}'];
        let mut request = format!(
            "PATCH /v1/future-binary/raw? HTTP/1.1\r\nHost: {proxy_addr}\r\nAuthorization: Bearer sk-opaque-binary\r\nContent-Type: application/octet-stream\r\nX-Opaque-Marker: preserve-binary\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        request.extend_from_slice(&body);
        let response = match tokio::time::timeout(Duration::from_secs(5), async {
            let mut client = tokio::net::TcpStream::connect(proxy_addr)
                .await
                .expect("connect opaque binary proxy");
            client
                .write_all(&request)
                .await
                .expect("write opaque binary ingress");
            let mut response = Vec::new();
            client
                .read_to_end(&mut response)
                .await
                .expect("read opaque binary response");
            response
        })
        .await
        {
            Ok(response) => response,
            Err(_) => {
                upstream_task.abort();
                proxy_task.abort();
                let _ = upstream_task.await;
                let _ = proxy_task.await;
                panic!("timed out waiting for opaque binary proxy response");
            }
        };
        let captured = match tokio::time::timeout(Duration::from_secs(5), capture_rx).await {
            Ok(Ok(captured)) => captured,
            Ok(Err(error)) => {
                upstream_task.abort();
                proxy_task.abort();
                let _ = upstream_task.await;
                let _ = proxy_task.await;
                panic!("opaque binary capture closed: {error}");
            }
            Err(_) => {
                upstream_task.abort();
                proxy_task.abort();
                let _ = upstream_task.await;
                let _ = proxy_task.await;
                panic!("opaque binary request did not reach the selected upstream");
            }
        };
        match tokio::time::timeout(Duration::from_secs(5), &mut upstream_task).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                proxy_task.abort();
                let _ = proxy_task.await;
                panic!("opaque binary upstream task failed: {error}");
            }
            Err(_) => {
                upstream_task.abort();
                proxy_task.abort();
                let _ = upstream_task.await;
                let _ = proxy_task.await;
                panic!("opaque binary upstream task did not finish");
            }
        }
        proxy_task.abort();
        let _ = proxy_task.await;
        assert_eq!(captured.method, "PATCH");
        assert_eq!(captured.target, "/v1/future-binary/raw?");
        assert_eq!(captured.body, body);
        let response_header_end = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("proxy response headers")
            + 4;
        let response_headers = String::from_utf8_lossy(&response[..response_header_end]);
        assert!(response_headers.starts_with("HTTP/1.1 200"));
        assert!(!response_headers
            .to_ascii_lowercase()
            .contains("content-type:"));
        assert_eq!(
            decoded_raw_http_body(&response_headers, &response[response_header_end..]),
            response_body
        );
    }

    #[test]
    fn local_opaque_ingress_uses_configured_priority_for_unknown_family_evidence() {
        let mut config = default_config();
        config.listen = "127.0.0.1:0".to_string();
        config.api_key = "sk-opaque-ambiguous".to_string();
        config.account_device_api_key.clear();
        config.prefer_local_supply = true;
        config.endpoints.clear();
        config.channels = vec![
            verified_openai_channel("first-openai", &["first-model"], "http://127.0.0.1:9"),
            verified_openai_channel("second-openai", &["second-model"], "http://127.0.0.1:10"),
        ];
        let config = normalize_config(config);
        let selected = select_direct_http_surface_channel_where(
            &config,
            crate::surface::ApiSurface::OpenAi,
            "/v1/future-binary/raw",
            &[255_u8, 0, 128],
            true,
            |_| true,
        )
        .expect("first channel");
        assert_eq!(selected.id, "first-openai");
    }

    #[test]
    fn local_native_resource_prefers_verified_family_over_unknown_channel() {
        let mut config = default_config();
        config.endpoints.clear();
        let first = verified_openai_channel(
            "unknown-files",
            &["gpt-test"],
            "http://127.0.0.1:9",
        );
        let mut verified = verified_openai_channel(
            "verified-files",
            &["gpt-test"],
            "http://127.0.0.1:10",
        );
        verified.detection_checks.push(ChannelDetectionCheck {
            name: "surface_operation_family".to_string(),
            status: "verified".to_string(),
            checked_at_unix: 42,
            protocol: "openai_responses".to_string(),
            capability: "openai.files_uploads".to_string(),
            message: "HTTP 200".to_string(),
        });
        config.channels = vec![first, verified];
        let config = normalize_config(config);

        let selected = select_direct_http_surface_channel_where(
            &config,
            crate::surface::ApiSurface::OpenAi,
            "/v1/files",
            b"",
            true,
            |_| true,
        )
        .expect("verified channel");
        assert_eq!(selected.id, "verified-files");
    }

    #[test]
    fn native_model_path_selects_the_channel_that_exposes_that_model() {
        let mut config = default_config();
        config.endpoints.clear();
        config.channels = vec![
            verified_openai_channel(
                "first-model-channel",
                &["first-model"],
                "http://127.0.0.1:9",
            ),
            verified_openai_channel(
                "second-model-channel",
                &["second-model"],
                "http://127.0.0.1:10",
            ),
        ];
        let config = normalize_config(config);
        let selected = select_direct_http_surface_channel_where(
            &config,
            crate::surface::ApiSurface::OpenAi,
            "/v1/models/second-model",
            b"",
            true,
            |_| true,
        )
        .expect("model channel");
        assert_eq!(selected.id, "second-model-channel");
    }

    #[tokio::test]
    async fn local_subscription_opaque_request_uses_same_surface_guard_before_provider() {
        let mut channel =
            local_model_channel(&["opaque-local-model"], "local://subscription/openai");
        channel.kind = "subscription_adapter".to_string();
        channel.api_format = "subscription_skeleton".to_string();
        channel.subscription.platform = "openai".to_string();

        let mut headers = HeaderMap::new();
        headers.insert("x-opaque-marker", "preserve-me".parse().expect("header"));
        let config = default_config();

        let error = forward_local_channel_once(
            warp::http::Method::PATCH,
            "/v1/future-operation",
            "mode=raw%2Fvalue",
            headers,
            bytes::Bytes::from_static(b"\xff\x00opaque-body"),
            &Client::new(),
            &config,
            &channel,
        )
        .await
        .expect_err("opaque subscription must not enter the specialized provider executor");

        assert_eq!(
            error.to_string(),
            "channel has no verified matching openai surface for opaque operation"
        );
    }
