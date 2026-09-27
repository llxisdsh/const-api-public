mod transport_regression_tests {
    use super::*;

    fn large_catalog() -> SupplierAgentConfig {
        let mut supplier = default_supplier_config();
        supplier.channel_id = "transport-large-catalog-fixture".into();
        supplier.node_id = supplier.channel_id.clone();
        supplier.models = (0..514)
            .map(|index| format!("fixture-model-{index:04}"))
            .collect();
        supplier.capability_profiles = supplier
            .models
            .iter()
            .flat_map(|model| {
                ["openai_chat", "openai_responses", "anthropic_messages"].map(|protocol| {
                    ChannelCapabilityProfile {
                        protocol: protocol.into(),
                        model_pattern: model.clone(),
                        catalog_source_model: Some(format!("fixture-provider/{model}")),
                        catalog_metadata: true,
                        context_tokens: Some(1_000_000),
                        output_tokens: Some(32768),
                        non_stream_json: true,
                        stream_sse: true,
                        tool_calls: true,
                        tool_choice: true,
                        parallel_tool_calls: true,
                        json_schema: true,
                        reasoning: true,
                        vision: true,
                        image_input: true,
                        verification_state: "verified".into(),
                        ..Default::default()
                    }
                })
            })
            .collect();
        SupplierAgentConfig {
            client_id: "transport-regression-client".into(),
            session_id: "transport-regression-session".into(),
            suppliers: vec![supplier],
            server_ws_url: String::new(),
            server_quic_url: String::new(),
            access_token: Arc::new(StdMutex::new("mock-only-token".into())),
            account_refresh_notify: Arc::new(Notify::new()),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn large_first_registration_survives_limited_uplink_and_legacy_negotiation() {
        let fixture = large_catalog();
        let raw_bytes = serde_json::to_vec(&supplier_register_message(&fixture))
            .unwrap()
            .len();
        assert!(
            raw_bytes > 4 * 1024 * 1024,
            "fixture must reproduce a real large registration: {raw_bytes}"
        );
        for (compression, chunks) in [(true, true), (true, false), (false, false)] {
            let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let server_addr = server.local_addr().unwrap();
            let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let proxy_addr = proxy.local_addr().unwrap();
            let relay = tokio::spawn(async move {
                let (front, _) = proxy.accept().await.unwrap();
                let back = tokio::net::TcpStream::connect(server_addr).await.unwrap();
                let (mut fr, mut fw) = front.into_split();
                let (mut br, mut bw) = back.into_split();
                let up = async {
                    let mut bytes = [0u8; 16 * 1024];
                    loop {
                        let count = fr.read(&mut bytes).await?;
                        if count == 0 {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(16)).await;
                        bw.write_all(&bytes[..count]).await?;
                    }
                    bw.shutdown().await
                };
                let down = async { tokio::io::copy(&mut br, &mut fw).await.map(|_| ()) };
                tokio::select! { _ = up => {}, _ = down => {} }
            });
            let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
            let mut mock = tokio::spawn(async move {
                let (tcp, _) = server.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
                let mut assembler = SupplierMessageAssembler::default();
                let mut received_register = false;
                let mut received_ping = false;
                let mut registration_wire = 0;
                let mut part_count = 0;
                let mut stop = Some(shutdown_tx);
                while let Some(Ok(frame)) = ws.next().await {
                    let binary = frame.is_binary();
                    let wire_len = frame.len();
                    let msg: SupplierMessage = match frame {
                        WsMessage::Text(raw) => serde_json::from_str(&raw).unwrap(),
                        WsMessage::Binary(raw) => decode_supplier_transport_frame(&raw).unwrap(),
                        _ => continue,
                    };
                    if matches!(msg.kind.as_str(), "register" | SUPPLIER_MESSAGE_CHUNK_TYPE) {
                        if part_count == 0 {
                            assert_eq!(
                                binary, compression,
                                "first registration must use negotiated compression"
                            );
                        }
                        if chunks {
                            assert!(wire_len < 128 * 1024);
                        }
                        registration_wire += wire_len;
                        part_count += 1;
                    }
                    let Some(msg) = assembler.accept(msg, chunks).unwrap() else {
                        continue;
                    };
                    let mut reply = SupplierMessage {
                        id: msg.id.clone(),
                        kind: String::new(),
                        payload: Default::default(),
                    };
                    match msg.kind.as_str() {
                        "authenticate" => {
                            let offered =
                                supplier_transport_string_array(&msg.payload, "transport_features");
                            assert!(offered
                                .iter()
                                .any(|feature| feature
                                    == SUPPLIER_TRANSPORT_FRAME_COMPRESSION_FEATURE));
                            assert!(offered
                                .iter()
                                .any(|feature| feature == SUPPLIER_MESSAGE_CHUNKS_FEATURE));
                            let mut features = Vec::new();
                            if compression {
                                features.push(SUPPLIER_TRANSPORT_FRAME_COMPRESSION_FEATURE);
                            }
                            if chunks {
                                features.push(SUPPLIER_MESSAGE_CHUNKS_FEATURE);
                            }
                            reply.kind = "authenticate_ack".into();
                            reply.payload =
                                serde_json::json!({"ok":true,"transport_features":features})
                                    .as_object()
                                    .unwrap()
                                    .clone();
                        }
                        "register" => {
                            assert_eq!(
                                msg.payload["channels"][0]["models"]
                                    .as_array()
                                    .unwrap()
                                    .len(),
                                514
                            );
                            received_register = true;
                            reply.kind = "register_ack".into();
                            reply.payload = serde_json::json!({"ok":true,"registered_count":1})
                                .as_object()
                                .unwrap()
                                .clone();
                        }
                        "ping" => {
                            received_ping = true;
                            reply.kind = "pong".into();
                        }
                        _ => continue,
                    }
                    if ws
                        .send(WsMessage::Text(
                            serde_json::to_string(&reply).unwrap().into(),
                        ))
                        .await
                        .is_err()
                    {
                        break;
                    }
                    if received_register && received_ping {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        if let Some(stop) = stop.take() {
                            let _ = stop.send(());
                        }
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        break;
                    }
                }
                (
                    received_register,
                    received_ping,
                    registration_wire,
                    part_count,
                )
            });
            let mut agent = fixture.clone();
            let (_updates, mut updates) = mpsc::channel(1);
            let state = Arc::new(StdMutex::new(SupplierTransportSnapshot::default()));
            let started = Instant::now();
            let result = tokio::time::timeout(
                Duration::from_secs(35),
                run_supplier_websocket_connection(
                    &Client::new(),
                    &mut agent,
                    &format!("ws://{proxy_addr}"),
                    &mut shutdown_rx,
                    &mut updates,
                    &state,
                ),
            )
            .await;
            let error = state.lock().unwrap().aggregate().1;
            std::eprintln!("limited uplink runtime: compression={compression} chunks={chunks} raw={raw_bytes} result={result:?} error={error}");
            let report = tokio::time::timeout(Duration::from_secs(3), &mut mock).await;
            mock.abort();
            relay.abort();
            let _ = relay.await;
            assert_eq!(
                result.unwrap(),
                SupplierRunOutcome::Shutdown,
                "compression={compression} chunks={chunks} {error}"
            );
            let report = report.expect("mock peer did not stop").unwrap();
            assert!(report.0 && report.1);
            assert_eq!(report.3 > 1, chunks);
            if compression {
                assert!(report.2 < raw_bytes / 4);
            }
            std::eprintln!("limited uplink: compression={compression} chunks={chunks} raw={raw_bytes} wire={} parts={} elapsed={:?}",report.2,report.3,started.elapsed());
        }
    }

    #[test]
    fn registration_queue_coalesces_latest_channel_state_and_preserves_force() {
        let mut pending = SupplierPendingRegistrations::default();
        let mut config = large_catalog();
        pending.publish(&config, Duration::from_secs(30), true);
        for n in 0..20 {
            config.suppliers[0].name = format!("latest-{n}");
            pending.publish(&config, Duration::from_secs(30), false);
        }
        assert_eq!(pending.updates.len(), 1);
        let (_, config, _, forced) = pending.updates.pop_front().unwrap();
        assert_eq!(config.suppliers[0].name, "latest-19");
        assert!(forced);
        assert_eq!(
            supplier_register_signature(&config).len(),
            64,
            "signatures must not retain multi-MiB catalogs"
        );
    }

    #[tokio::test]
    async fn forced_registration_preempts_a_throttled_background_snapshot() {
        let mut config = large_catalog().with_suppliers(vec![default_supplier_config()]);
        config.suppliers[0].channel_id = "forced-registration-preempts-throttle".into();
        let (outbound, mut rx) = mpsc::channel(4);
        let (errors, _) = mpsc::unbounded_channel();
        let dispatcher = SupplierRegistrationDispatcher::new(outbound, errors);
        dispatcher
            .publish(config.clone(), Duration::from_secs(3600), false)
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap();
        config.suppliers[0].name = "stale-background".into();
        dispatcher
            .publish(config.clone(), Duration::from_secs(3600), false)
            .unwrap();
        assert!(tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .is_err());
        config.suppliers[0].name = "manual-latest".into();
        dispatcher
            .publish(config, Duration::from_secs(3600), true)
            .unwrap();
        let result = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.payload["channels"][0]["name"], "manual-latest");
        drop(dispatcher);
        assert!(
            rx.recv().await.is_none(),
            "stale intermediate snapshot must not be sent later"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn saturated_response_queue_does_not_block_receiving_server_control() {
        saturated_response_queue_receives_server_control(false).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reconnect_waits_for_resume_negotiation_without_losing_live_duplex_frames() {
        check_output_waits_for_negotiation(false, true, "duplex_server_frame").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reconnect_waits_for_resume_negotiation_without_losing_live_http_frames() {
        check_output_waits_for_negotiation(false, true, "stream_chunk").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn legacy_server_still_receives_output_after_register_ack_without_features() {
        check_output_waits_for_negotiation(false, false, "stream_chunk").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_reconnect_waits_for_negotiation_without_losing_live_duplex_frames() {
        check_output_waits_for_negotiation(true, true, "duplex_server_frame").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_reconnect_waits_for_negotiation_without_losing_live_http_frames() {
        check_output_waits_for_negotiation(true, true, "stream_chunk").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_legacy_server_receives_output_after_register_ack_without_features() {
        check_output_waits_for_negotiation(true, false, "stream_chunk").await;
    }

    enum NegotiationListener {
        WebSocket(tokio::net::TcpListener),
        Quic(quinn::Endpoint),
    }

    enum NegotiationPeer {
        WebSocket(tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>),
        Quic {
            _connection: quinn::Connection,
            reader: tokio::io::Lines<BufReader<quinn::RecvStream>>,
            writer: quinn::SendStream,
        },
    }

    impl NegotiationListener {
        async fn bind(quic: bool) -> (Self, String) {
            if !quic {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let url = format!("ws://{}", listener.local_addr().unwrap());
                return (Self::WebSocket(listener), url);
            }
            let _ = rustls::crypto::ring::default_provider().install_default();
            let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
            let key = rustls_pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());
            let mut tls = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(vec![cert.cert.der().clone()], key.into()).unwrap();
            tls.alpn_protocols = vec![SUPPLIER_QUIC_ALPN.to_vec()];
            let config = quinn::ServerConfig::with_crypto(Arc::new(
                quinn::crypto::rustls::QuicServerConfig::try_from(tls).unwrap(),
            ));
            let endpoint = quinn::Endpoint::server(config, "127.0.0.1:0".parse().unwrap()).unwrap();
            let url = format!("quic://{}/supplier", endpoint.local_addr().unwrap());
            (Self::Quic(endpoint), url)
        }

        async fn accept(&self) -> NegotiationPeer {
            match self {
                Self::WebSocket(listener) => {
                    let (tcp, _) = listener.accept().await.unwrap();
                    NegotiationPeer::WebSocket(tokio_tungstenite::accept_async(tcp).await.unwrap())
                }
                Self::Quic(endpoint) => {
                    let connection = endpoint.accept().await.unwrap().await.unwrap();
                    let (writer, recv) = connection.accept_bi().await.unwrap();
                    NegotiationPeer::Quic {
                        _connection: connection, reader: BufReader::new(recv).lines(), writer,
                    }
                }
            }
        }
    }

    impl NegotiationPeer {
        async fn recv(&mut self) -> SupplierMessage {
            match self {
                Self::WebSocket(ws) => {
                    let raw = ws.next().await.unwrap().unwrap().into_text().unwrap();
                    serde_json::from_str(&raw).unwrap()
                }
                Self::Quic { reader, .. } => {
                    serde_json::from_str(&reader.next_line().await.unwrap().unwrap()).unwrap()
                }
            }
        }

        async fn send(&mut self, value: serde_json::Value) {
            match self {
                Self::WebSocket(ws) => {
                    ws.send(WsMessage::Text(value.to_string().into())).await.unwrap();
                }
                Self::Quic { writer, .. } => {
                    let mut raw = serde_json::to_vec(&value).unwrap();
                    raw.push(b'\n');
                    writer.write_all(&raw).await.unwrap();
                    writer.flush().await.unwrap();
                }
            }
        }
    }

    #[tokio::test]
    async fn heartbeat_slow_reply_business_progress_and_dead_peer_work_on_both_transports() {
        tokio::join!(
            check_heartbeat_reply(false, Some("pong")),
            check_heartbeat_reply(true, Some("pong")),
            check_heartbeat_reply(false, Some("stream_ack")),
            check_heartbeat_reply(true, Some("stream_ack")),
            check_heartbeat_reply(false, None),
            check_heartbeat_reply(true, None),
        );
    }

    async fn check_heartbeat_reply(quic: bool, reply: Option<&'static str>) {
        let (listener, url) = NegotiationListener::bind(quic).await;
        let (stop, mut shutdown) = oneshot::channel();
        let (finished, finished_rx) = oneshot::channel();
        let peer = tokio::spawn(async move {
            let mut connection = listener.accept().await;
            let ping = loop {
                let message = connection.recv().await;
                let kind = match message.kind.as_str() {
                    "authenticate" => "authenticate_ack",
                    // Do not race a registration ACK with the initial ping:
                    // any such valid reply correctly resolves that probe too.
                    "register" => continue,
                    "ping" => break message,
                    _ => continue,
                };
                connection.send(serde_json::json!({"id":message.id,"type":kind,
                    "payload":{"ok":true,"registered_count":1}})).await;
            };
            if let Some(kind) = reply {
                // This healthy delay used to trip the three-second deadline.
                tokio::time::sleep(Duration::from_secs(4)).await;
                connection.send(serde_json::json!({"id":ping.id,"type":kind,
                    "payload":{"sequence":1}})).await;
                connection.send(serde_json::json!({"id":"after-delay","type":"ping"})).await;
                loop {
                    let message = connection.recv().await;
                    if message.kind == "pong" && message.id == "after-delay" {
                        break;
                    }
                }
                let _ = stop.send(());
            }
            // A silent peer remains connected until the client's own detector fires.
            let _ = finished_rx.await;
        });
        let mut config = large_catalog().with_suppliers(vec![default_supplier_config()]);
        let (_updates, mut updates) = mpsc::channel(1);
        let (output, mut output_rx) = mpsc::channel(1);
        let state = Arc::new(StdMutex::new(SupplierTransportSnapshot::default()));
        let mut replay = SupplierReplayState::default();
        let mut disconnected = None;
        let started = Instant::now();
        let result = tokio::time::timeout(Duration::from_secs(25), async {
            if quic {
                run_supplier_quic_connection_with_runtime(
                    &Client::new(), &mut config, &url, &mut shutdown, &mut updates,
                    &state, &output, &mut output_rx, &SupplierRequestTasks::default(),
                    &mut replay, &mut disconnected,
                ).await
            } else {
                run_supplier_websocket_connection_with_runtime(
                    &Client::new(), &mut config, &url, &mut shutdown, &mut updates,
                    &state, &output, &mut output_rx, &SupplierRequestTasks::default(),
                    &mut replay, &mut disconnected,
                ).await
            }
        }).await;
        let _ = finished.send(());
        let outcome = result.unwrap_or_else(|_| panic!(
            "heartbeat must react before the 100-second backstop: quic={quic} reply={reply:?} state={:?}",
            state.lock().unwrap().aggregate(),
        ));
        peer.await.unwrap();
        if reply.is_some() {
            assert_eq!(outcome, SupplierRunOutcome::Shutdown);
        } else {
            assert_eq!(outcome, SupplierRunOutcome::Disconnected);
            assert!(started.elapsed() >= supplier_heartbeat_response_timeout());
            assert!(state.lock().unwrap().aggregate().1.contains(&format!(
                "within {} seconds", supplier_heartbeat_response_timeout().as_secs(),
            )));
        }
    }

    async fn check_output_waits_for_negotiation(quic: bool, resumable: bool, kind: &'static str) {
        let (listener, url) = NegotiationListener::bind(quic).await;
        let (stop, mut shutdown) = oneshot::channel();
        let (finished, finished_rx) = oneshot::channel();
        let peer = tokio::spawn(async move {
            let mut ws = listener.accept().await;
            let mut frames = Vec::new();
            let register_id = loop {
                let msg = ws.recv().await;
                let reply = match msg.kind.as_str() {
                    "authenticate" => "authenticate_ack",
                    "ping" => "pong",
                    "register" => break msg.id,
                    value if value == kind => { frames.push(msg); continue; }
                    _ => continue,
                };
                ws.send(serde_json::json!({
                    "id": msg.id, "type": reply, "payload": {"ok": true}
                })).await;
            };
            ws.send(serde_json::json!({
                "id": "during-negotiation", "type": "ping", "payload": {}
            })).await;
            let delay = tokio::time::sleep(Duration::from_millis(500));
            tokio::pin!(delay);
            let mut pong = false;
            loop {
                tokio::select! {
                    _ = &mut delay => break,
                    msg = ws.recv() => {
                        match msg.kind.as_str() {
                            value if value == kind => frames.push(msg),
                            "pong" => pong = true,
                            "ping" => {
                                ws.send(serde_json::json!({
                                    "id": msg.id, "type": "pong", "payload": {}
                                })).await;
                            }
                            _ => {}
                        }
                    }
                }
            }
            let before_ack = frames.len();
            let features = if resumable { vec![SUPPLIER_TRANSPORT_STREAM_RESUME_FEATURE] } else { vec![] };
            ws.send(serde_json::json!({
                "id": register_id, "type": "register_ack",
                "payload": {"ok": true, "registered_count": 1,
                    "transport_features": features}
            })).await;
            while frames.len() < if resumable { 2 } else { 1 } {
                let msg = ws.recv().await;
                if msg.kind == kind { frames.push(msg); }
            }
            let _ = stop.send(());
            // Keep the peer alive until the shutdown branch completes. Otherwise
            // EOF and the explicit stop race, unrelated to output negotiation.
            let _ = finished_rx.await;
            (before_ack, pong, frames)
        });
        let message = |turn_seq| SupplierMessage {
            id: format!("live-reconnected-session-{quic}-{resumable}-{kind}"), kind: kind.into(),
            payload: serde_json::json!({"data": "unchanged", "frame_type": "text",
                "_const_duplex_turn_id": "live-reconnected-turn", "_const_duplex_turn_seq": turn_seq})
                .as_object().unwrap().clone(),
        };
        let mut replay = SupplierReplayState::default();
        let previous = if resumable {
            replay.enqueue(message(71));
            Some(replay.next_unsent().unwrap())
        } else { None };
        let (output, mut output_rx) = mpsc::channel(8);
        output.send(message(72)).await.unwrap();
        let mut config = large_catalog().with_suppliers(vec![default_supplier_config()]);
        let (_updates, mut updates) = mpsc::channel(1);
        let state = Arc::new(StdMutex::new(SupplierTransportSnapshot::default()));
        let mut disconnected = resumable.then(Instant::now);
        let outcome = tokio::time::timeout(Duration::from_secs(5), async {
            if quic {
                run_supplier_quic_connection_with_runtime(
                    &Client::new(), &mut config, &url, &mut shutdown, &mut updates,
                    &state, &output, &mut output_rx, &SupplierRequestTasks::default(), &mut replay, &mut disconnected,
                ).await
            } else {
                run_supplier_websocket_connection_with_runtime(
                    &Client::new(), &mut config, &url, &mut shutdown, &mut updates,
                    &state, &output, &mut output_rx, &SupplierRequestTasks::default(), &mut replay, &mut disconnected,
                ).await
            }
        }).await;
        let _ = finished.send(());
        let outcome = outcome.unwrap();
        assert_eq!(outcome, SupplierRunOutcome::Shutdown);
        let (before_ack, pong, frames) = peer.await.unwrap();
        assert_eq!(before_ack, 0, "live output escaped before stream-resume negotiation");
        assert!(pong, "negotiation must not block heartbeats");
        for (index, frame) in frames.iter().enumerate() {
            if let Some(previous) = previous.as_ref() {
                assert_eq!(frame.payload[SUPPLIER_STREAM_SEQUENCE_FIELD], index + 1);
                assert_eq!(frame.payload["_const_duplex_turn_seq"], index + 71);
                assert_eq!(frame.payload[SUPPLIER_STREAM_EXECUTION_ID_FIELD], previous.payload[SUPPLIER_STREAM_EXECUTION_ID_FIELD]);
            } else {
                assert!(!frame.payload.contains_key(SUPPLIER_STREAM_SEQUENCE_FIELD));
                assert!(!frame.payload.contains_key(SUPPLIER_STREAM_EXECUTION_ID_FIELD));
                assert_eq!(frame.payload["_const_duplex_turn_seq"], 72);
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn saturated_response_queue_during_auth_refresh_keeps_receiving_server_control() {
        saturated_response_queue_receives_server_control(true).await;
    }

    async fn saturated_response_queue_receives_server_control(refresh_auth: bool) {
        let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = server.local_addr().unwrap();
        let (flood, ready) = oneshot::channel();
        let (stop, stopped) = oneshot::channel::<()>();
        let access_token = Arc::new(StdMutex::new("mock-only-token".to_string()));
        let peer_token = access_token.clone();
        let peer = tokio::spawn(async move {
            let (tcp, _) = server.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            while let Some(Ok(frame)) = ws.next().await {
                let WsMessage::Text(raw) = frame else {
                    continue;
                };
                let msg: SupplierMessage = serde_json::from_str(&raw).unwrap();
                let kind = match msg.kind.as_str() {
                    "authenticate" => "authenticate_ack",
                    "register" => "register_ack",
                    "ping" => "pong",
                    _ => continue,
                };
                ws.send(WsMessage::Text(serde_json::json!({"id":msg.id,"type":kind,"payload":{"ok":true,"registered_count":1}}).to_string().into())).await.unwrap();
                if msg.kind == "register" {
                    break;
                }
            }
            let _ = flood.send(());
            // Stop reading: force real socket and outbound-channel backpressure.
            tokio::time::sleep(Duration::from_millis(300)).await;
            if refresh_auth {
                *peer_token.lock().unwrap() = "mock-refreshed-token".into();
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            ws.send(WsMessage::Text(
                serde_json::json!({"id":"restart","type":"server_restarting","payload":{}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            let _ = stopped.await;
        });
        let (output, mut output_rx) = mpsc::channel(256);
        let producer_output = output.clone();
        let producer = tokio::spawn(async move {
            ready.await.unwrap();
            for index in 0..128 {
                let message = SupplierMessage {
                    id: format!("backpressure-{index}"),
                    kind: "http_response".into(),
                    payload: serde_json::json!({"body":"x".repeat(256*1024)})
                        .as_object()
                        .unwrap()
                        .clone(),
                };
                if producer_output.send(message).await.is_err() {
                    break;
                }
            }
        });
        let mut config = large_catalog().with_suppliers(vec![default_supplier_config()]);
        config.access_token = access_token;
        let (_shutdown, mut shutdown) = oneshot::channel();
        let (_updates, mut updates) = mpsc::channel(1);
        let state = Arc::new(StdMutex::new(SupplierTransportSnapshot::default()));
        let mut replay = SupplierReplayState::default();
        let mut disconnected = None;
        let started = Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(6),
            run_supplier_websocket_connection_with_runtime(
                &Client::new(),
                &mut config,
                &format!("ws://{addr}"),
                &mut shutdown,
                &mut updates,
                &state,
                &output,
                &mut output_rx,
                &SupplierRequestTasks::default(),
                &mut replay,
                &mut disconnected,
            ),
        )
        .await;
        producer.abort();
        let _ = producer.await;
        let _ = stop.send(());
        peer.await.unwrap();
        assert_eq!(result.unwrap(), SupplierRunOutcome::Disconnected);
        let error = state.lock().unwrap().aggregate().1;
        assert!(
            error.contains("server requested supplier reconnect"),
            "{error}"
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
