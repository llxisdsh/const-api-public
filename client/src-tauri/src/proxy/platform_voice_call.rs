// HTTP SDP bootstrap and the following sideband belong to one live platform
// session. These IDs are local capabilities, never provider resource IDs.
const PLATFORM_VOICE_CALL_PREFIX: &str = "rtc_const_";
const PLATFORM_VOICE_READY: &str = "const.realtime.call.ready";

struct PlatformVoiceCall {
    owner: ring::digest::Digest,
    native_live: bool,
    events: Mutex<Option<crate::output_buffer::OutputReceiver<crate::supplier::PlatformDuplexFrame>>>,
    commands: tokio::sync::mpsc::Sender<crate::supplier::PlatformDuplexFrame>,
    stopped: tokio::sync::watch::Sender<bool>,
    attached: std::sync::atomic::AtomicBool,
}

fn voice_call_owner(headers: &warp::http::HeaderMap) -> ring::digest::Digest {
    let credential = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().strip_prefix("Bearer "))
        .or_else(|| {
            ["x-api-key", "api-key", "x-goog-api-key"]
                .into_iter()
                .find_map(|key| headers.get(key).and_then(|value| value.to_str().ok()))
        })
        .unwrap_or_default()
        .trim();
    ring::digest::digest(&ring::digest::SHA256, credential.as_bytes())
}

fn is_platform_voice_call(method: &warp::http::Method, path: &str) -> bool {
    method == warp::http::Method::POST && matches!(path, "/v1/live" | "/v1/realtime/calls")
}

async fn try_platform_voice_hangup(
    method: &warp::http::Method,
    path: &str,
    headers: &warp::http::HeaderMap,
    shared: &ProxyShared,
) -> Option<warp::reply::Response> {
    if method != warp::http::Method::POST {
        return None;
    }
    let id = path
        .strip_prefix("/v1/realtime/calls/")?
        .strip_suffix("/hangup")?;
    if !id.starts_with(PLATFORM_VOICE_CALL_PREFIX) || id.contains('/') {
        return None;
    }
    let mut calls = shared.platform_voice_calls.lock().await;
    let Some(call) = calls
        .get(id)
        .filter(|call| call.owner.as_ref() == voice_call_owner(headers).as_ref())
    else {
        return Some(error_response(
            StatusCode::NOT_FOUND,
            "voice call expired or not owned by this caller",
        ));
    };
    // Synthetic IDs belong to this registry, not a provider account. End the
    // whole media/sideband session without routing or exposing an upstream ID.
    call.stopped.send_replace(true);
    calls.remove(id);
    Some(warp::reply::with_status("", StatusCode::OK).into_response())
}

async fn try_platform_voice_call(
    path: &str,
    raw_query: &str,
    headers: &warp::http::HeaderMap,
    body: &bytes::Bytes,
    config: &ClientConfig,
    shared: &Arc<ProxyShared>,
) -> Result<Option<warp::reply::Response>> {
    let ready = shared.ready_local_channel_ids();
    let model = openai_realtime_call_model_hint(body);
    let local_ready = shared
        .local_model_quota_routes
        .lock()
        .ok()
        .is_some_and(|quotas| {
            select_openai_realtime_call_channel_where(config, &model, |channel| {
                ready.contains(&channel.id)
                    && local_model_quota_route_available(&quotas, channel, &model, now_unix(), false)
            })
            .is_some()
        });
    let explicit_local = headers
        .get(USE_LOCAL_SHORT_CIRCUIT_HEADER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| matches!(value, "1" | "true" | "yes"));
    if (explicit_local && local_ready) || !should_use_platform_native(shared, headers, local_ready)
    {
        return Ok(None);
    }
    let mut payload = match parse_openai_realtime_call_payload(headers, body.clone()).await {
        Ok(payload) => payload,
        Err(error) => {
            return Ok(Some(error_response(
                StatusCode::BAD_REQUEST,
                &error.to_string(),
            )))
        }
    };
    let peer = crate::realtime_media::MediaPeer::new(true).await?;
    let answer = match peer
        .answer(payload["sdp"].as_str().unwrap_or_default())
        .await
    {
        Ok(answer) => answer,
        Err(_) => {
            return Ok(Some(error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "platform voice could not negotiate this audio offer",
            )))
        }
    };
    // The supplier creates its own upstream peer. Never expose its ICE/DTLS
    // credentials to the caller, or allow media to bypass platform metering.
    payload["sdp"] = serde_json::Value::Null;
    let body = serde_json::to_vec(&payload)?;
    let opened =
        connect_platform_duplex_with_call(shared, headers, Some((path, raw_query)), Some(&body))
            .await;
    let (session, endpoint, transport) = match opened {
        Ok(opened) => opened,
        Err(error)
            if local_ready
                && native_local_fallback_allowed(headers)
                && crate::supplier::platform_duplex_error_is_safe_to_replay(&error) =>
        {
            // An older/unavailable platform rejected before dispatch. Keep
            // the existing local call path usable during staged updates.
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let (sender, mut receiver) = session.split();
    let first = tokio::time::timeout(Duration::from_secs(150), receiver.next()).await;
    let ready = match first {
        Ok(Some(Ok(crate::supplier::PlatformDuplexFrame::Text(text)))) => {
            let event = serde_json::from_str::<serde_json::Value>(&text).unwrap_or_default();
            if event["type"].as_str() == Some(PLATFORM_VOICE_READY) {
                true
            } else {
                let status = event["status"]
                    .as_u64()
                    .and_then(|status| u16::try_from(status).ok())
                    .and_then(|status| StatusCode::from_u16(status).ok())
                    .unwrap_or(StatusCode::BAD_GATEWAY);
                let _ = sender
                    .send(crate::supplier::PlatformDuplexFrame::Close {
                        code: 1000,
                        reason: "voice bootstrap rejected".into(),
                    })
                    .await;
                if local_ready
                    && native_local_fallback_allowed(headers)
                    && platform_native_error_allows_fallback(&text)
                {
                    return Ok(None);
                }
                return Ok(Some(
                    warp::reply::with_status(warp::reply::json(&event), status).into_response(),
                ));
            }
        }
        _ => false,
    };
    if !ready {
        let _ = sender
            .send(crate::supplier::PlatformDuplexFrame::Close {
                code: 1000,
                reason: "voice bootstrap timed out".into(),
            })
            .await;
        return Ok(Some(error_response(
            StatusCode::GATEWAY_TIMEOUT,
            "platform voice could not start; please retry",
        )));
    }
    let id = format!(
        "{PLATFORM_VOICE_CALL_PREFIX}{:032x}",
        rand::random::<u128>()
    );
    let Some(activity) = crate::update_activity::try_begin_supplier_request(&id) else {
        let _ = sender
            .send(crate::supplier::PlatformDuplexFrame::Close {
                code: 1000,
                reason: "client update starting".into(),
            })
            .await;
        return Ok(Some(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "client update starting; retry shortly",
        )));
    };
    let (events, event_rx) = crate::output_buffer::channel(Default::default());
    let (commands, command_rx) = tokio::sync::mpsc::channel(16);
    let call = Arc::new(PlatformVoiceCall {
        owner: voice_call_owner(headers),
        native_live: path == "/v1/live",
        events: Mutex::new(Some(event_rx)),
        commands,
        stopped: tokio::sync::watch::channel(false).0,
        attached: std::sync::atomic::AtomicBool::new(false),
    });
    {
        let mut calls = shared.platform_voice_calls.lock().await;
        if calls.len() >= 32 {
            let _ = sender
                .send(crate::supplier::PlatformDuplexFrame::Close {
                    code: 1000,
                    reason: "too many local voice calls".into(),
                })
                .await;
            return Ok(Some(error_response(
                StatusCode::TOO_MANY_REQUESTS,
                "too many active voice calls",
            )));
        }
        calls.insert(id.clone(), call.clone());
    }
    log::info!(
        "[const-api][realtime] platform call opened endpoint={} transport={} request_source={}",
        endpoint.name,
        transport,
        websocket_request_source(headers)
    );
    let registry = shared.platform_voice_calls.clone();
    let cleanup_id = id.clone();
    crate::spawn_logged("platform voice call", async move {
        let _activity = activity;
        relay_platform_voice_media(peer, sender, receiver, events, command_rx, &call).await;
        call.stopped.send_replace(true);
        registry.lock().await.remove(&cleanup_id);
    });
    Ok(Some(
        warp::reply::with_header(
            warp::reply::with_header(
                warp::reply::with_status(answer, StatusCode::CREATED),
                "Content-Type",
                "application/sdp",
            ),
            "Location",
            format!("{path}/{id}"),
        )
        .into_response(),
    ))
}

async fn relay_platform_voice_media(
    mut peer: crate::realtime_media::MediaPeer,
    sender: crate::supplier::PlatformDuplexSender,
    mut receiver: crate::supplier::PlatformDuplexReceiver,
    events: crate::output_buffer::OutputSender<crate::supplier::PlatformDuplexFrame>,
    mut commands: tokio::sync::mpsc::Receiver<crate::supplier::PlatformDuplexFrame>,
    call: &PlatformVoiceCall,
) {
    use crate::supplier::PlatformDuplexFrame as Frame;
    let mut stopped = call.stopped.subscribe();
    let mut peer_stopped = peer.stopped();
    let connect_deadline = tokio::time::sleep(Duration::from_secs(30));
    tokio::pin!(connect_deadline);
    let lifetime = tokio::time::sleep(Duration::from_secs(3600));
    tokio::pin!(lifetime);
    let relay = async {
        let mut pending_text: Option<String> = None;
        loop {
            tokio::select! {
                _ = &mut connect_deadline, if !peer.is_connected() || (call.native_live && !call.attached.load(std::sync::atomic::Ordering::Relaxed)) => break,
                bytes = peer.received.recv() => {
                    let Some(bytes) = bytes else { break };
                    if sender.send(Frame::Binary(bytes)).await.is_err() { break; }
                }
                command = commands.recv() => {
                    let Some(command) = command else { break };
                    if matches!(command, Frame::Close { .. }) { break; }
                    if sender.send(command).await.is_err() { break; }
                }
                credit = events.reserve(pending_text.as_ref().map_or(0, String::capacity)), if pending_text.is_some() => {
                    let Some(credit) = credit else { break };
                    if let Some(text) = pending_text.take() {
                        if !events.send_reserved(Frame::Text(text), credit) { break; }
                    }
                }
                incoming = receiver.next(), if pending_text.is_none() => {
                    match incoming {
                        Some(Ok(Frame::Binary(bytes))) => { if peer.send(&bytes).await.is_err() { break; } }
                        Some(Ok(Frame::Text(text))) => {
                            // Retain sideband output during delayed attachment
                            // or a slow tool; a 64-event burst is not call failure.
                            pending_text = Some(text);
                        }
                        _ => break,
                    }
                }
            }
        }
    };
    // Keep cancellation polled even while a socket/queue write is pending.
    tokio::select! {
        _ = relay => {}
        _ = crate::realtime_media::wait_stopped(&mut stopped) => {}
        _ = crate::realtime_media::wait_stopped(&mut peer_stopped) => {}
        _ = &mut lifetime => {}
        _ = crate::update_activity::wait_for_update_drain() => {}
    }
    // Closing this peer cuts audio even if the tool keeps its RTC object open.
    drop(peer);
    let _ = tokio::time::timeout(
        Duration::from_secs(2),
        sender.send(Frame::Close {
            code: 1000,
            reason: "voice ended".into(),
        }),
    )
    .await;
}

async fn relay_platform_voice_sideband(
    id: &str,
    native_live: bool,
    headers: &warp::http::HeaderMap,
    shared: &ProxyShared,
    mut sender: futures_util::stream::SplitSink<warp::ws::WebSocket, warp::ws::Message>,
    mut receiver: futures_util::stream::SplitStream<warp::ws::WebSocket>,
) {
    use crate::supplier::PlatformDuplexFrame as Frame;
    let call = shared.platform_voice_calls.lock().await.get(id).cloned();
    let Some(call) = call.filter(|call| {
        call.native_live == native_live && call.owner.as_ref() == voice_call_owner(headers).as_ref()
    }) else {
        let _ = sender.send(warp::ws::Message::text(r#"{"type":"error","error":{"code":"resource_owner_not_found","message":"voice call expired; start a new call"}}"#)).await;
        return;
    };
    let Some(mut events) = call.events.lock().await.take() else {
        let _ = sender.send(warp::ws::Message::text(r#"{"type":"error","error":{"code":"resource_owner_busy","message":"voice sideband is already attached"}}"#)).await;
        return;
    };
    call.attached
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let mut stopped = call.stopped.subscribe();
    let relay = async {
        loop {
            tokio::select! {
                event = events.recv() => {
                    let Some(Frame::Text(text)) = event else { break };
                    if sender.send(warp::ws::Message::text(text)).await.is_err() { break; }
                }
                message = receiver.next() => {
                    let Some(Ok(message)) = message else { break };
                    if message.is_close() { break; }
                    // The sideband is JSON, not an alternative path for RTP.
                    if message.is_text() && call.commands.send(Frame::Text(message.to_str().unwrap_or_default().into())).await.is_err() { break; }
                }
            }
        }
    };
    tokio::select! {
        _ = relay => {}
        _ = crate::realtime_media::wait_stopped(&mut stopped) => {}
    }
    call.stopped.send_replace(true);
    close_client_websocket_sender(&mut sender).await;
}
