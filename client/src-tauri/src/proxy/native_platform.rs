// Native media and Responses share the physical platform tunnel. Media has no
// response replay/HTTP fallback after execution starts: tools own reconnection.
fn should_use_platform_native(
    shared: &ProxyShared,
    headers: &warp::http::HeaderMap,
    local_ready: bool,
) -> bool {
    platform_responses_websocket_allowed(shared, headers)
        && (!native_local_fallback_allowed(headers)
            || !shared.config_snapshot().prefer_local_supply
            || !local_ready)
}

fn native_local_fallback_allowed(headers: &warp::http::HeaderMap) -> bool {
    !headers
        .get(SKIP_LOCAL_SHORT_CIRCUIT_HEADER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| matches!(value, "1" | "true" | "yes"))
}

fn platform_native_error_allows_fallback(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text).is_ok_and(|event| {
        event["type"].as_str() == Some("error")
            && event["safe_to_retry_http_bridge"].as_bool() == Some(true)
    })
}

async fn relay_platform_native(
    client_sender: &mut futures_util::stream::SplitSink<warp::ws::WebSocket, warp::ws::Message>,
    client_receiver: &mut futures_util::stream::SplitStream<warp::ws::WebSocket>,
    session: crate::supplier::PlatformDuplexSession,
    allow_local_fallback: bool,
) -> bool {
    relay_platform_native_with_first(
        client_sender,
        client_receiver,
        session,
        None,
        allow_local_fallback,
    )
    .await
}

// true returns the still-open tool socket to its existing local route. This is
// allowed only for an explicit pre-dispatch rejection before any upstream frame.
async fn relay_platform_native_with_first(
    client_sender: &mut futures_util::stream::SplitSink<warp::ws::WebSocket, warp::ws::Message>,
    client_receiver: &mut futures_util::stream::SplitStream<warp::ws::WebSocket>,
    session: crate::supplier::PlatformDuplexSession,
    first: Option<&warp::ws::Message>,
    allow_local_fallback: bool,
) -> bool {
    use crate::supplier::PlatformDuplexFrame as Frame;
    let (sender, mut receiver) = session.split();
    if let Some(first) = first {
        let frame = if first.is_binary() {
            Frame::Binary(first.as_bytes().to_vec())
        } else {
            Frame::Text(first.to_str().unwrap_or_default().to_string())
        };
        if sender.send(frame).await.is_err() {
            return false;
        }
    }
    let relay = async {
        let mut received_upstream = false;
        let mut can_fallback = allow_local_fallback;
        loop {
            tokio::select! {
                message = client_receiver.next() => {
                    let Some(Ok(message)) = message else { break };
                    let frame = if message.is_text() {
                        Frame::Text(message.to_str().unwrap_or_default().to_string())
                    } else if message.is_binary() {
                        Frame::Binary(message.as_bytes().to_vec())
                    } else if message.is_close() { break } else { continue };
                    // These frames are not retained for replay. Once consumed,
                    // leave retries to the tool rather than lose its initial input.
                    can_fallback = false;
                    if sender.send(frame).await.is_err() { break; }
                }
                frame = receiver.next() => {
                    let Some(Ok(frame)) = frame else { break };
                    if !received_upstream && can_fallback
                        && matches!(&frame, Frame::Text(text) if platform_native_error_allows_fallback(text))
                    {
                        return true;
                    }
                    received_upstream = true;
                    let message = match frame {
                        Frame::Text(text) => warp::ws::Message::text(text),
                        Frame::Binary(bytes) => warp::ws::Message::binary(bytes),
                        Frame::Close { .. } => break,
                    };
                    if client_sender.send(message).await.is_err() { break; }
                }
            }
        }
        false
    };
    let fallback = tokio::select! {
        result = relay => result,
        _ = crate::update_activity::wait_for_update_drain() => false,
    };
    let _ = tokio::time::timeout(
        Duration::from_secs(2),
        sender.send(Frame::Close {
            code: 1000,
            reason: "media session ended".into(),
        }),
    )
    .await;
    if !fallback {
        close_client_websocket_sender(client_sender).await;
    }
    fallback
}

async fn run_supplier_native_duplex(
    client: &Client,
    config: &SupplierConfig,
    open: SupplierMessage,
    mut commands: tokio::sync::mpsc::Receiver<SupplierMessage>,
    outbound: &SupplierOutbound,
) -> Result<()> {
    use crate::supplier::PlatformDuplexFrame as Frame;
    let wire = crate::surface_wire::SurfaceEnvelope::from_payload(&open.payload)?;
    let kind = match (wire.surface, wire.operation) {
        (
            Some(crate::surface::ApiSurface::OpenAi),
            Some(crate::surface::ApiOperation::RealtimeWebSocket),
        ) => NativeDuplexKind::OpenAiRealtime,
        (
            Some(crate::surface::ApiSurface::OpenAi),
            Some(crate::surface::ApiOperation::RealtimeLiveConnect),
        ) => NativeDuplexKind::OpenAiLiveConnect,
        (
            Some(crate::surface::ApiSurface::Gemini),
            Some(crate::surface::ApiOperation::LiveWebSocket),
        ) => NativeDuplexKind::GeminiLive,
        _ => return Err(anyhow!("unsupported native realtime surface envelope")),
    };
    let channel = channel_from_supplier(config.channel_id.clone(), config);
    let execution = crate::channel_executor::execution_kind_for_source(config.source_driver)?;
    let subscription = execution == crate::source_driver::ExecutionKind::OpenAiSubscription;
    if execution != crate::source_driver::ExecutionKind::HttpSurface
        && !(subscription && matches!(kind, NativeDuplexKind::OpenAiLiveConnect))
    {
        return Err(anyhow!(
            "this subscription does not implement the requested native realtime operation"
        ));
    }
    let model = wire
        .selected_upstream_model
        .as_deref()
        .filter(|model| !model.trim().is_empty())
        .or(wire.requested_model.as_deref())
        .unwrap_or_default();
    if model.is_empty() {
        return Err(anyhow!("native realtime requires a model"));
    }
    let _safety = if subscription {
        match subscription_safety_enter(config, &serde_json::json!({"model":model}).to_string()) {
            Ok(guard) => Some(guard),
            Err(payload) => {
                send_native_supplier_message(
                    outbound,
                    SupplierMessage {
                        id: open.id,
                        kind: "duplex_error".into(),
                        payload,
                    },
                )
                .await?;
                return Ok(());
            }
        }
    } else {
        None
    };
    let _activity = crate::update_activity::try_begin_supplier_request(&open.id)
        .ok_or_else(|| anyhow!("client update is starting; retry in 2 seconds"))?;
    let headers = wire.header_map()?;
    let mut query = reqwest::Url::parse("http://localhost/")?;
    query.set_query((!wire.raw_query.is_empty()).then_some(wire.raw_query.as_str()));
    let pairs: Vec<_> = query
        .query_pairs()
        .filter(|(name, _)| name != "model" && name != "call_id" && name != "key")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    query.set_query(None);
    query.query_pairs_mut().extend_pairs(pairs);
    if !matches!(kind, NativeDuplexKind::GeminiLive) {
        query.query_pairs_mut().append_pair("model", model);
    }
    let query = query.query().unwrap_or_default();
    let socket = if subscription {
        connect_openai_subscription_live(client, &channel, None, query, &headers).await
    } else {
        let request =
            native_duplex_upstream_request(&channel, kind, kind.route_path(), query, &headers)?;
        connect_responses_websocket_request(request).await
    };
    let socket = match socket {
        Ok(socket) => socket,
        Err(error) => {
            let mut payload = supplier_responses_websocket_connect_error_payload(
                &error,
                &channel.id,
                &websocket_request_source(&headers),
            );
            payload.insert("failure_scope".into(), "operation".into());
            payload.insert(
                "message".into(),
                format!(
                    "{} upstream WebSocket handshake failed",
                    kind.operation().as_str()
                )
                .into(),
            );
            send_native_supplier_message(
                outbound,
                SupplierMessage {
                    id: open.id,
                    kind: "duplex_error".into(),
                    payload,
                },
            )
            .await?;
            return Ok(());
        }
    };
    let mut diagnostics = UpstreamWebsocketLease::open(
        kind.operation().as_str(),
        channel.id.clone(),
        websocket_request_source(&headers),
    );
    diagnostics.mark_turn();
    send_native_supplier_message(
        outbound,
        SupplierMessage {
            id: open.id.clone(),
            kind: "duplex_opened".into(),
            payload: serde_json::Map::new(),
        },
    )
    .await?;
    let (mut sender, mut receiver) = socket.split();
    // Enforce independently at the supplier too, including after server loss.
    let lifetime = tokio::time::sleep(Duration::from_secs(3600));
    tokio::pin!(lifetime);
    let lease = tokio::time::sleep(Duration::from_secs(20));
    tokio::pin!(lease);
    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { break };
                if command.kind == "duplex_close" { break; }
                if command.kind != "duplex_client_frame" { continue; }
                lease.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(20));
                if command.payload.get("control").and_then(serde_json::Value::as_str) == Some("media_lease") { continue; }
                let message = match crate::supplier::platform_server_frame(&command.payload)? {
                    Frame::Text(text) => tokio_tungstenite::tungstenite::Message::Text(text.into()),
                    Frame::Binary(bytes) => tokio_tungstenite::tungstenite::Message::Binary(bytes.into()),
                    Frame::Close { .. } => break,
                };
                if sender.send(message).await.is_err() { break; }
            }
            message = receiver.next() => {
                let Some(Ok(message)) = message else { break };
                let frame = match message {
                    tokio_tungstenite::tungstenite::Message::Text(text) => {
                        if subscription { crate::detection::observe_openai_subscription_response_event(&channel, text.as_ref()); }
                        Frame::Text(text.to_string())
                    }
                    tokio_tungstenite::tungstenite::Message::Binary(bytes) => Frame::Binary(bytes.to_vec()),
                    tokio_tungstenite::tungstenite::Message::Close(_) => break,
                    _ => continue,
                };
                // No turn_terminal: VAD can start another response at any time.
                if send_native_supplier_message(outbound, SupplierMessage { id:open.id.clone(), kind:"duplex_server_frame".into(), payload:crate::supplier::platform_frame_payload(frame) }).await.is_err() { break; }
            }
            _ = &mut lifetime => break,
            _ = &mut lease => break,
            _ = crate::update_activity::wait_for_update_drain() => break,
        }
    }
    close_upstream_websocket_sender(&mut sender).await;
    send_native_supplier_message(
        outbound,
        SupplierMessage {
            id: open.id,
            kind: "duplex_close".into(),
            payload: serde_json::Map::new(),
        },
    )
    .await?;
    Ok(())
}

async fn send_native_supplier_message(
    outbound: &SupplierOutbound,
    mut message: SupplierMessage,
) -> Result<()> {
    message
        .payload
        .insert("_const_realtime_no_replay".into(), true.into());
    send_supplier_message(outbound, message).await
}
