#[tokio::test]
async fn voice_standalone_live_preserves_native_frames_and_query() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let upstream = warp::path!("v1" / "live")
        .and(warp::query::raw())
        .and(warp::header::exact(
            "authorization",
            "Bearer voice-upstream",
        ))
        .and(warp::ws())
        .map(|query: String, ws: warp::ws::Ws| {
            assert_eq!(query, "model=gpt-live-test&future=x%20y");
            ws.on_upgrade(|socket| async move {
                let (mut tx, mut rx) = socket.split();
                while let Some(Ok(frame)) = rx.next().await {
                    if frame.is_close() || tx.send(frame).await.is_err() {
                        break;
                    }
                }
            })
        });
    let (address, server) = crate::bind_ephemeral!(upstream, ([127, 0, 0, 1], 0));
    let upstream_task = tokio::spawn(server);
    let mut config = default_config();
    config.api_key = "voice-local".to_string();
    config.account_device_api_key.clear();
    let mut channel = openai_api_channel("voice", "gpt-live-test", &format!("http://{address}/v1"));
    channel.upstream_api_key = "voice-upstream".to_string();
    config.channels = vec![channel];
    let shared = Arc::new(ProxyShared::from_config(
        normalize_config(config),
        Client::new(),
    ));
    let (proxy_address, server) =
        crate::bind_ephemeral!(crate::proxy_routes(shared), ([127, 0, 0, 1], 0));
    let proxy_task = tokio::spawn(server);
    let mut request = format!("ws://{proxy_address}/v1/live?model=gpt-live-test&future=x%20y")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("authorization", "Bearer voice-local".parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    for frame in [
        tokio_tungstenite::tungstenite::Message::Text(
            r#"{ "type":"input_audio.append", "audio":"AA==", "future":true }"#.into(),
        ),
        tokio_tungstenite::tungstenite::Message::Binary(vec![0, 255, 1, 128].into()),
    ] {
        socket.send(frame.clone()).await.unwrap();
        let echoed = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(echoed, frame);
    }
    let _ = socket.close(None).await;
    proxy_task.abort();
    upstream_task.abort();
}

#[tokio::test]
async fn voice_supplier_native_executor_keeps_media_and_consumes_only_lease_control() {
    use crate::supplier::PlatformDuplexFrame as Frame;
    let captured = Arc::new(StdMutex::new(Vec::new()));
    let capture = captured.clone();
    let upstream = warp::path!("v1" / "realtime")
        .and(warp::query::raw())
        .and(warp::header::exact("authorization", "Bearer voice-key"))
        .and(warp::ws())
        .map(move |query: String, ws: warp::ws::Ws| {
            assert!(query.contains("model=gpt-realtime-test"));
            assert!(query.contains("future=yes"));
            let capture = capture.clone();
            ws.on_upgrade(move |socket| async move {
                let (mut tx, mut rx) = socket.split();
                while let Some(Ok(frame)) = rx.next().await {
                    if frame.is_close() {
                        break;
                    }
                    capture.lock().unwrap().push(frame.as_bytes().to_vec());
                    if tx.send(frame).await.is_err() {
                        break;
                    }
                }
            })
        });
    let (address, server) = crate::bind_ephemeral!(upstream, ([127, 0, 0, 1], 0));
    let server = tokio::spawn(server);
    let mut channel = openai_api_channel(
        "voice",
        "gpt-realtime-test",
        &format!("http://{address}/v1"),
    );
    channel.upstream_api_key = "voice-key".into();
    channel.v2.credential_ref = "voice-key".into();
    let config = crate::supplier_from_channel(&channel);
    let (commands, receiver) = tokio::sync::mpsc::channel(8);
    let (outbound, mut events) = tokio::sync::mpsc::channel(8);
    let open = SupplierMessage {id:"voice-native-supplier-test".into(),kind:"duplex_open".into(),payload:serde_json::json!({
        "session_kind":"native_realtime", "wire":{
            "version":2,"surface":"openai","operation":crate::surface::ApiOperation::RealtimeWebSocket,
            "method":"GET","escaped_relative_path":"/v1/realtime","raw_query":"future=yes",
            "requested_model":"gpt-realtime-test","selected_upstream_model":"gpt-realtime-test"
        }
    }).as_object().unwrap().clone()};
    let task = tokio::spawn(async move {
        run_supplier_native_duplex(&Client::new(), &config, open, receiver, &outbound).await
    });
    let opened = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(opened.kind, "duplex_opened", "{opened:?}");
    commands
        .send(SupplierMessage {
            id: opened.id.clone(),
            kind: "duplex_client_frame".into(),
            payload: serde_json::json!({"control":"media_lease"})
                .as_object()
                .unwrap()
                .clone(),
        })
        .await
        .unwrap();
    for frame in [
        Frame::Text(r#"{ "type":"input_audio_buffer.append","audio":"AA==","future":1 }"#.into()),
        Frame::Binary(vec![0, 255, 128, 1]),
    ] {
        commands
            .send(SupplierMessage {
                id: opened.id.clone(),
                kind: "duplex_client_frame".into(),
                payload: crate::supplier::platform_frame_payload(frame.clone()),
            })
            .await
            .unwrap();
        let echoed = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(echoed.kind, "duplex_server_frame");
        assert!(crate::supplier::native_realtime_no_replay(&echoed));
        assert_eq!(
            crate::supplier::platform_server_frame(&echoed.payload).unwrap(),
            frame
        );
        assert!(echoed.payload.get("turn_terminal").is_none());
    }
    commands
        .send(SupplierMessage {
            id: opened.id,
            kind: "duplex_close".into(),
            payload: serde_json::Map::new(),
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        captured.lock().unwrap().len(),
        2,
        "lease control leaked upstream"
    );
    for _ in 0..4 {
        crate::update_activity::supplier_output_send_failed_or_delivered_without_replay(
            "voice-native-supplier-test",
        );
    }
    server.abort();
}

#[tokio::test]
async fn voice_unknown_owners_never_select_another_channel() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let mut config = default_config();
    config.api_key = "voice-owner-local".to_string();
    config.account_device_api_key.clear();
    config.channels = vec![openai_api_channel(
        "wrong-owner",
        "gpt-realtime-test",
        "http://127.0.0.1:9/v1",
    )];
    let shared = Arc::new(ProxyShared::from_config(
        normalize_config(config),
        Client::new(),
    ));
    let (address, server) =
        crate::bind_ephemeral!(crate::proxy_routes(shared), ([127, 0, 0, 1], 0));
    let task = tokio::spawn(server);
    for (path, setup) in [
        ("/v1/live/rtc_unknown", None),
        ("/v1/realtime?call_id=rtc_unknown", None),
        ("/v1/realtime/translations?call_id=rtc_unknown", None),
        (
            "/gemini/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent",
            Some(
                r#"{"setup":{"model":"models/live-test","sessionResumption":{"handle":"unknown"}}}"#,
            ),
        ),
    ] {
        let mut request = format!("ws://{address}{path}")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("authorization", "Bearer voice-owner-local".parse().unwrap());
        let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        if let Some(setup) = setup {
            socket
                .send(tokio_tungstenite::tungstenite::Message::Text(setup.into()))
                .await
                .unwrap();
        }
        let frame = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap();
        assert!(
            frame.contains("resource_owner_not_found"),
            "{path}: {frame}"
        );
    }
    task.abort();
}

#[tokio::test]
async fn voice_audio_upload_routes_by_model_and_keeps_binary_form() {
    let captured = Arc::new(StdMutex::new(Vec::new()));
    let capture = captured.clone();
    let upstream = warp::post()
        .and(warp::body::bytes())
        .map(move |body: bytes::Bytes| {
            capture.lock().unwrap().push(body);
            warp::reply::json(&serde_json::json!({"text":"ok"}))
        });
    let (address, server) = crate::bind_ephemeral!(upstream, ([127, 0, 0, 1], 0));
    let task = tokio::spawn(server);
    let mut config = default_config();
    config.api_key = "audio-local".to_string();
    config.account_device_api_key.clear();
    config.channels = vec![
        openai_api_channel("wrong-first", "wrong-model", "http://127.0.0.1:9/v1"),
        openai_api_channel("audio", "whisper-test", &format!("http://{address}/v1")),
    ];
    let shared = Arc::new(ProxyShared::from_config(
        normalize_config(config),
        Client::new(),
    ));
    let body = b"--audio-test\r\nContent-Disposition: form-data; name=\"file\"; filename=\"test.wav\"\r\nContent-Type: audio/wav\r\n\r\nRIFF\0\xff\r\n--audio-test\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nwhisper-test\r\n--audio-test--\r\n";
    for path in ["/v1/audio/transcriptions", "/v1/audio/translations"] {
        let response = warp::test::request()
            .method("POST")
            .path(path)
            .header("authorization", "Bearer audio-local")
            .header("content-type", "multipart/form-data; boundary=audio-test")
            .body(body.as_slice())
            .reply(&crate::proxy_routes(shared.clone()))
            .await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{path}: {:?}",
            response.body()
        );
    }
    let captured = captured.lock().unwrap();
    assert_eq!(captured.len(), 2);
    assert!(captured.iter().all(|received| received.as_ref() == body));
    task.abort();
}
