async fn voice_test_physical_platform(
    listener: tokio::net::TcpListener,
    config: SupplierConfig,
) -> Result<()> {
    use tokio_tungstenite::tungstenite::Message as Ws;
    let (stream, _) = listener.accept().await?;
    let mut socket = tokio_tungstenite::accept_async(stream).await?;
    let auth: SupplierMessage =
        serde_json::from_str(socket.next().await.context("auth missing")??.to_text()?)?;
    socket
        .send(Ws::Text(
            serde_json::json!({"id":auth.id,"type":"authenticate_ack","payload":{
                "ok":true,"transport_features":["platform_duplex_v1"]
            }})
            .to_string()
            .into(),
        ))
        .await?;
    let open: SupplierMessage =
        serde_json::from_str(socket.next().await.context("open missing")??.to_text()?)?;
    assert_eq!(open.payload["session_kind"], "native_realtime_call");
    assert_eq!(open.payload["request"]["method"], "POST");
    let call_body = open.payload["request"]["body"]
        .as_str()
        .context("call body missing")?;
    let body: serde_json::Value = serde_json::from_str(call_body)?;
    assert!(
        body["sdp"].is_null(),
        "consumer ICE credentials crossed the platform"
    );
    let path = open.payload["request"]["path"]
        .as_str()
        .context("call path missing")?;
    let operation = if path == "/v1/live" {
        "realtime_live_call_create"
    } else {
        "realtime_calls_create"
    };
    let supplier_open = SupplierMessage { id:open.id.clone(), kind:"duplex_open".into(),
        payload: serde_json::json!({"session_kind":"native_realtime_call","wire":{
            "version":2,"surface":"openai","operation":operation,"method":"POST",
            "escaped_relative_path":path,"headers":[],"body":{"encoding":"utf8","data":call_body},
            "requested_model":body["session"]["model"],"selected_upstream_model":body["session"]["model"]
        }}).as_object().unwrap().clone(),
    };
    socket
        .send(Ws::Text(
            serde_json::json!({"id":open.id,"type":"platform_duplex_opened","payload":{"ok":true}})
                .to_string()
                .into(),
        ))
        .await?;
    let (commands, rx) = tokio::sync::mpsc::channel(32);
    let (outbound, mut events) = tokio::sync::mpsc::channel(128);
    let task = tokio::spawn(async move {
        run_supplier_voice_call(&Client::new(), &config, supplier_open, rx, &outbound).await
    });
    commands
        .send(SupplierMessage {
            id: open.id.clone(),
            kind: "duplex_client_frame".into(),
            payload: serde_json::json!({"control":"media_lease"})
                .as_object()
                .unwrap()
                .clone(),
        })
        .await?;
    loop {
        tokio::select! {
            event = events.recv() => {
                let Some(mut event) = event else { break };
                if event.kind == "duplex_opened" { continue; }
                assert!(crate::supplier::native_realtime_no_replay(&event));
                event.kind = match event.kind.as_str() {
                    "duplex_server_frame" => "platform_duplex_server_frame",
                    "duplex_error" => "platform_duplex_error",
                    _ => "platform_duplex_close",
                }.into();
                if socket.send(Ws::Text(serde_json::to_string(&event)?.into())).await.is_err() { break; }
            }
            incoming = socket.next() => {
                let Some(Ok(Ws::Text(text))) = incoming else { break };
                let mut message: SupplierMessage = serde_json::from_str(&text)?;
                if message.kind == "platform_duplex_close" { break; }
                if message.kind != "platform_duplex_client_frame" { continue; }
                message.kind = "duplex_client_frame".into();
                commands.send(message).await?;
            }
        }
    }
    drop(commands);
    tokio::time::timeout(Duration::from_secs(5), task).await???;
    Ok(())
}

#[tokio::test]
async fn voice_platform_call_reaches_remote_api_without_local_channel() -> Result<()> {
    voice_platform_call_case(false).await
}

#[tokio::test]
async fn voice_platform_standard_api_call_preserves_public_session_and_sideband() -> Result<()> {
    voice_platform_call_case(true).await
}

async fn voice_platform_call_case(public: bool) -> Result<()> {
    use crate::realtime_media::{frame, MediaPeer, DATA_TEXT};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let model = if public {
        "gpt-realtime-2.1"
    } else {
        "gpt-live-1-codex"
    };
    let call_path = if public {
        "/v1/realtime/calls"
    } else {
        "/v1/live"
    };
    let provider_peer = Arc::new(Mutex::new(None::<MediaPeer>));
    let captured = provider_peer.clone();
    let bootstrap = warp::post()
        .and(warp::path::full())
        .and(warp::header::exact(
            "authorization",
            "Bearer voice-upstream",
        ))
        .and(warp::header::headers_cloned())
        .and(warp::body::bytes())
        .and_then(
            move |path: warp::path::FullPath,
                  headers: warp::http::HeaderMap,
                  body: bytes::Bytes| {
                let captured = captured.clone();
                async move {
                    assert_eq!(path.as_str(), call_path);
                    assert!(headers["content-type"]
                        .to_str()
                        .unwrap()
                        .starts_with("multipart/form-data;"));
                    let body = parse_openai_realtime_call_payload(&headers, body)
                        .await
                        .unwrap();
                    assert_eq!(body["session"]["model"], model);
                    assert_eq!(body["session"]["future"], true);
                    let peer = MediaPeer::new(true).await.unwrap();
                    let answer = peer.answer(body["sdp"].as_str().unwrap()).await.unwrap();
                    *captured.lock().await = Some(peer);
                    Ok::<_, Infallible>(warp::reply::with_header(
                        warp::reply::with_status(answer, StatusCode::CREATED),
                        "Location",
                        format!("{call_path}/rtc_provider"),
                    ))
                }
            },
        );
    let sideband = warp::get()
        .and(warp::path::full())
        .and(warp::query::raw().or(warp::any().map(String::new)).unify())
        .and(warp::header::exact(
            "authorization",
            "Bearer voice-upstream",
        ))
        .and(warp::ws())
        .map(
            move |path: warp::path::FullPath, query: String, ws: warp::ws::Ws| {
                assert_eq!(
                    path.as_str(),
                    if public {
                        "/v1/realtime"
                    } else {
                        "/v1/live/rtc_provider"
                    }
                );
                if public {
                    assert_eq!(query, "call_id=rtc_provider");
                }
                ws.on_upgrade(move |mut socket| async move {
                    let _ = socket
                        .send(warp::ws::Message::text(
                            serde_json::json!({"type":"session.updated","session":{"model":model}})
                                .to_string(),
                        ))
                        .await;
                    for sequence in 0..256 {
                        if socket.send(warp::ws::Message::text(
                            serde_json::json!({"type":"transcript.delta", "sequence":sequence}).to_string(),
                        )).await.is_err() { return; }
                    }
                    while let Some(Ok(message)) = socket.next().await {
                        if message.is_close() || socket.send(message).await.is_err() {
                            break;
                        }
                    }
                })
            },
        );
    let (address, server) = crate::bind_ephemeral!(bootstrap.or(sideband), ([127, 0, 0, 1], 0));
    let provider_task = tokio::spawn(server);
    let mut channel = openai_api_channel("remote-voice", model, &format!("http://{address}/v1"));
    channel.upstream_api_key = "voice-upstream".into();
    channel.v2.credential_ref = "voice-upstream".into();
    let supplier = crate::supplier_from_channel(&channel);
    let platform = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let platform_address = platform.local_addr()?;
    let platform_task = tokio::spawn(voice_test_physical_platform(platform, supplier));
    let mut config = default_config();
    config.api_key = "voice-tool".into();
    config.account_device_api_key = "voice-platform".into();
    config.prefer_local_supply = true; // no local channels: must still use platform
    config.channels.clear();
    config.endpoints = vec![Endpoint {
        server_id: String::new(),
        name: "voice-test".into(),
        base_url: format!("http://{platform_address}"),
        supplier_ws_url: format!("ws://{platform_address}/supplier/ws"),
        supplier_quic_url: String::new(),
        enabled: true,
    }];
    let shared = Arc::new(ProxyShared::from_config(
        normalize_config(config),
        Client::new(),
    ));
    *shared.platform_access_token.lock().unwrap() = "voice-access".into();
    let (proxy_address, server) =
        crate::bind_ephemeral!(crate::proxy_routes(shared.clone()), ([127, 0, 0, 1], 0));
    let proxy_task = tokio::spawn(server);
    let mut tool = MediaPeer::new(true).await?;
    let offer = tool.offer().await?;
    let response = Client::new()
        .post(format!("http://{proxy_address}{call_path}"))
        .bearer_auth("voice-tool")
        .json(&serde_json::json!({"sdp":offer,"session":{"model":model,"future":true}}))
        .send()
        .await?;
    if response.status() != StatusCode::CREATED {
        panic!(
            "platform voice bootstrap returned {}: {}",
            response.status(),
            response.text().await?
        );
    }
    let location = response.headers()["location"].to_str()?.to_string();
    assert!(location.starts_with(&format!("{call_path}/rtc_const_")));
    let answer = response.text().await?;
    tool.accept_answer(&answer).await?;
    let call_id = location.rsplit('/').next().unwrap();
    let local_call = shared.platform_voice_calls.lock().await.get(call_id).unwrap().clone();
    // Delay the tool sideband attachment until more than the old 64-event
    // limit is already received. None of those transcript events may disappear.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if local_call.events.lock().await.as_ref().unwrap().len() >= 257 { break; }
            tokio::task::yield_now().await;
        }
    }).await?;
    let sideband_path = if public {
        format!(
            "/v1/realtime?call_id={}",
            location.rsplit('/').next().unwrap()
        )
    } else {
        location
    };
    let mut request = format!("ws://{proxy_address}{sideband_path}").into_client_request()?;
    request
        .headers_mut()
        .insert("authorization", "Bearer voice-tool".parse()?);
    let (mut control, _) = tokio_tungstenite::connect_async(request).await?;
    let started = tokio::time::timeout(Duration::from_secs(5), control.next())
        .await?
        .context("sideband missing")??;
    assert!(started.to_text()?.contains("session.updated"));
    for sequence in 0..256 {
        let event = tokio::time::timeout(Duration::from_secs(5), control.next())
            .await?.context("buffered transcript missing")??;
        let event: serde_json::Value = serde_json::from_str(event.to_text()?)?;
        assert_eq!(event["sequence"], sequence);
    }
    let outgoing = frame(DATA_TEXT, br#"{"type":"future.input","unchanged":true}"#);
    tool.send(&outgoing).await?;
    let mut provider = provider_peer
        .lock()
        .await
        .take()
        .context("provider peer missing")?;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), provider.received.recv())
            .await?
            .unwrap(),
        outgoing
    );
    let incoming = frame(DATA_TEXT, br#"{"type":"future.output","unchanged":true}"#);
    provider.send(&incoming).await?;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), tool.received.recv())
            .await?
            .unwrap(),
        incoming
    );
    // Exercise the full four-peer audio path in both directions, not just the
    // control handshake. Neither edge may transcode or renumber audio packets.
    voice_assert_rtp(&tool, &mut provider.received, 17).await?;
    voice_assert_rtp(&provider, &mut tool.received, 41).await?;
    if public {
        // Keep the tool peer and sideband open: the HTTP hangup itself must
        // terminate the platform session, not just wait for those to disappear.
        let id = sideband_path.split("call_id=").nth(1).unwrap();
        let response = Client::new()
            .post(format!(
                "http://{proxy_address}/v1/realtime/calls/{id}/hangup"
            ))
            .bearer_auth("voice-tool")
            .send()
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
    } else {
        let _ = control.close(None).await;
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while !shared.platform_voice_calls.lock().await.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    tokio::time::timeout(Duration::from_secs(5), platform_task).await???;
    proxy_task.abort();
    provider_task.abort();
    Ok(())
}

#[tokio::test]
async fn voice_hangup_rejects_other_owners_and_preserves_provider_routes() {
    let shared = ProxyShared::from_config(default_config(), Client::new());
    let mut owner = warp::http::HeaderMap::new();
    owner.insert("authorization", "Bearer owner".parse().unwrap());
    let mut stranger = warp::http::HeaderMap::new();
    stranger.insert("authorization", "Bearer stranger".parse().unwrap());
    let (commands, _commands) = tokio::sync::mpsc::channel(1);
    let (_events, events) = crate::output_buffer::channel(Default::default());
    let call = Arc::new(PlatformVoiceCall {
        owner: voice_call_owner(&owner),
        native_live: false,
        events: Mutex::new(Some(events)),
        commands,
        stopped: tokio::sync::watch::channel(false).0,
        attached: std::sync::atomic::AtomicBool::new(false),
    });
    shared
        .platform_voice_calls
        .lock()
        .await
        .insert("rtc_const_owned".into(), call.clone());
    let path = "/v1/realtime/calls/rtc_const_owned/hangup";
    let rejected = try_platform_voice_hangup(&warp::http::Method::POST, path, &stranger, &shared)
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::NOT_FOUND);
    assert!(!*call.stopped.borrow());
    assert!(try_platform_voice_hangup(
        &warp::http::Method::POST,
        "/v1/realtime/calls/rtc_provider_id/hangup",
        &owner,
        &shared
    )
    .await
    .is_none());
    let ended = try_platform_voice_hangup(&warp::http::Method::POST, path, &owner, &shared)
        .await
        .unwrap();
    assert_eq!(ended.status(), StatusCode::OK);
    assert!(*call.stopped.borrow());
    assert!(shared.platform_voice_calls.lock().await.is_empty());
    assert_eq!(
        try_platform_voice_hangup(&warp::http::Method::POST, path, &owner, &shared)
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn voice_call_falls_back_only_on_confirmed_pre_dispatch_rejection() -> Result<()> {
    use tokio_tungstenite::tungstenite::Message as Ws;
    for (safe, skip_local, expect_fallback) in [
        (true, false, true),
        (false, false, false),
        (true, true, false),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let platform_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let auth: SupplierMessage =
                serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap())
                    .unwrap();
            socket
                .send(Ws::Text(
                    serde_json::json!({
                        "id":auth.id,"type":"authenticate_ack","payload":{
                            "ok":true,"transport_features":["platform_duplex_v1"]
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            let open: SupplierMessage =
                serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap())
                    .unwrap();
            socket
                .send(Ws::Text(
                    serde_json::json!({
                        "id":open.id,"type":"platform_duplex_opened","payload":{"ok":true}
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            let error = serde_json::json!({"type":"error","status":402,
                "error":{"code":"insufficient_balance"},"safe_to_retry_http_bridge":safe});
            socket
                .send(Ws::Text(
                    serde_json::json!({
                        "id":open.id,"type":"platform_duplex_server_frame",
                        "payload":{"frame_type":"text","data":error.to_string()}
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            let closed: SupplierMessage =
                serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap())
                    .unwrap();
            assert_eq!(closed.kind, "platform_duplex_close");
        });
        let mut config = default_config();
        config.prefer_local_supply = false;
        config.account_device_api_key = "platform-key".into();
        config.channels = vec![openai_api_channel(
            "local-voice",
            "gpt-realtime-2.1",
            "http://127.0.0.1:1/v1",
        )];
        config.endpoints = vec![Endpoint {
            server_id: String::new(),
            name: "rejecting-platform".into(),
            base_url: format!("http://{address}"),
            supplier_ws_url: format!("ws://{address}/supplier/ws"),
            supplier_quic_url: String::new(),
            enabled: true,
        }];
        let config = normalize_config(config);
        let shared = Arc::new(ProxyShared::from_config(config.clone(), Client::new()));
        shared.set_local_channel_ready("local-voice", true);
        *shared.platform_access_token.lock().unwrap() = "access".into();
        let peer = crate::realtime_media::MediaPeer::new(true).await?;
        let offer = peer.offer().await?;
        let mut headers = warp::http::HeaderMap::new();
        headers.insert("content-type", "application/json".parse()?);
        if skip_local {
            headers.insert(SKIP_LOCAL_SHORT_CIRCUIT_HEADER, "true".parse()?);
        }
        let body = bytes::Bytes::from(
            serde_json::json!({"sdp":offer,"session":{"model":"gpt-realtime-2.1"}}).to_string(),
        );
        let response =
            try_platform_voice_call("/v1/realtime/calls", "", &headers, &body, &config, &shared)
                .await?;
        assert_eq!(
            response.is_none(),
            expect_fallback,
            "safe={safe} skip={skip_local}"
        );
        if let Some(response) = response {
            assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);
        }
        assert!(shared.platform_voice_calls.lock().await.is_empty());
        tokio::time::timeout(Duration::from_secs(5), platform_task).await??;
    }
    Ok(())
}

async fn voice_assert_rtp(
    sender: &crate::realtime_media::MediaPeer,
    receiver: &mut tokio::sync::mpsc::Receiver<Vec<u8>>,
    sequence: u16,
) -> Result<()> {
    use crate::realtime_media::{frame, parse_frame, AUDIO};
    use rtc::shared::marshal::{Marshal, Unmarshal};
    let packet = rtc::rtp::Packet {
        header: rtc::rtp::header::Header {
            version: 2,
            sequence_number: sequence,
            timestamp: 960,
            ssrc: 45,
            payload_type: 111,
            ..Default::default()
        },
        payload: bytes::Bytes::from_static(&[0xf8, 0xff, 0xfe]),
    };
    sender.send(&frame(AUDIO, &packet.marshal()?)).await?;
    let received = tokio::time::timeout(Duration::from_secs(5), receiver.recv())
        .await?
        .context("voice RTP missing")?;
    let (lane, payload) = parse_frame(&received)?;
    assert_eq!(lane, AUDIO);
    let received = rtc::rtp::Packet::unmarshal(&mut bytes::BytesMut::from(payload))?;
    assert_eq!(received.payload, packet.payload);
    assert_eq!(received.header.sequence_number, sequence);
    assert_eq!(received.header.timestamp, packet.header.timestamp);
    Ok(())
}

#[test]
fn voice_preserves_native_dialect_headers_for_calls_and_sideband() {
    let mut headers = warp::http::HeaderMap::new();
    headers.insert("originator", "codex_cli_rs".parse().unwrap());
    headers.insert("user-agent", "codex_cli_rs/0.150.1".parse().unwrap());
    headers.insert("openai-alpha", "quicksilver=v1".parse().unwrap());
    headers.insert("openai-beta", "future-voice=v1".parse().unwrap());
    let copied = codex_realtime_request_context_headers(&headers);
    assert_eq!(copied["openai-alpha"], "quicksilver=v1");
    assert_eq!(copied["openai-beta"], "future-voice=v1");
    let sideband = openai_subscription_live_websocket_request(
        Some("rtc_test"),
        "",
        "test",
        &serde_json::json!({}),
        &headers,
    )
    .unwrap();
    assert_eq!(sideband.headers()["openai-alpha"], copied["openai-alpha"]);
    assert_eq!(sideband.headers()["openai-beta"], copied["openai-beta"]);
}

#[tokio::test]
#[ignore = "requires CONST_API_LIVE_VOICE_CONFIG and CONST_API_LIVE_VOICE_CHANNEL; brief real subscription call"]
async fn live_voice_subscription_media_connectivity() -> Result<()> {
    let path = std::env::var("CONST_API_LIVE_VOICE_CONFIG")?;
    let id = std::env::var("CONST_API_LIVE_VOICE_CHANNEL")?;
    let config: ClientConfig = serde_json::from_slice(&std::fs::read(path)?)?;
    let config = normalize_config(config);
    let channel = config
        .channels
        .iter()
        .find(|channel| channel.id == id)
        .context("dev voice channel missing")?;
    if channel.source_driver() != crate::source_driver::SourceDriverId::OpenAiSubscription {
        return Err(anyhow!(
            "live voice validation requires the selected dev subscription"
        ));
    }
    let wire = crate::surface_wire::SurfaceEnvelope::from_payload(serde_json::json!({"wire":{
        "version":2,"surface":"openai","operation":"realtime_live_call_create","method":"POST",
        "escaped_relative_path":"/v1/live","headers":[],"requested_model":"gpt-live-1-codex","selected_upstream_model":"gpt-live-1-codex",
        "body":{"encoding":"utf8","data":r#"{"sdp":null,"session":{"model":"gpt-live-1-codex","delegation":{"type":"client"},"instructions":"Remain silent until the user speaks."}}"#}
    }}).as_object().unwrap())?;
    let (peer, mut socket) = tokio::time::timeout(
        Duration::from_secs(45),
        open_supplier_voice_call(&Client::new(), channel, &wire),
    )
    .await??;
    peer.wait_connected().await?;
    std::eprintln!("Voice subscription: HTTP call + sideband + ICE/DTLS/data channel connected. No microphone or inference input sent.");
    let _ = socket.close(None).await;
    drop(peer);
    Ok(())
}
