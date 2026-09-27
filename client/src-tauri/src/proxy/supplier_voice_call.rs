// Provider credentials and SDP never leave this supplier. The platform carries
// encoded audio, data-channel messages and the existing JSON sideband only.
#[derive(Debug)]
struct VoiceCallSetupError {
    stage: &'static str,
    status: u16,
    code: String,
}
impl std::fmt::Display for VoiceCallSetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "voice {} failed (HTTP {}, provider_code={})",
            self.stage, self.status, self.code
        )
    }
}
impl std::error::Error for VoiceCallSetupError {}

async fn open_supplier_voice_call(
    client: &Client,
    channel: &ChannelConfig,
    wire: &crate::surface_wire::SurfaceEnvelope,
) -> Result<(crate::realtime_media::MediaPeer, ResponsesUpstreamSocket)> {
    let native_live = wire.operation == Some(crate::surface::ApiOperation::RealtimeLiveCallCreate);
    if wire.surface != Some(crate::surface::ApiSurface::OpenAi)
        || (!native_live
            && wire.operation != Some(crate::surface::ApiOperation::RealtimeCallsCreate))
    {
        return Err(anyhow!("invalid platform voice call operation"));
    }
    let execution = crate::channel_executor::execution_kind_for_source(channel.source_driver())?;
    let subscription = execution == crate::source_driver::ExecutionKind::OpenAiSubscription;
    if !subscription && execution != crate::source_driver::ExecutionKind::HttpSurface {
        return Err(anyhow!(
            "this channel does not implement OpenAI voice calls"
        ));
    }
    let model = wire
        .selected_upstream_model
        .as_deref()
        .or(wire.requested_model.as_deref())
        .filter(|model| !model.trim().is_empty())
        .ok_or_else(|| anyhow!("voice call requires a model"))?;
    let mut body: serde_json::Value = serde_json::from_slice(&wire.body.decode()?)?;
    if !body
        .get("session")
        .is_some_and(serde_json::Value::is_object)
    {
        return Err(anyhow!("voice call requires a session object"));
    }
    let peer = crate::realtime_media::MediaPeer::new(false).await?;
    body["sdp"] = peer.offer().await?.into();
    body["session"]["model"] = model.into();
    let mut headers = wire.header_map()?;
    headers.remove(reqwest::header::CONTENT_LENGTH);
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    let response = if subscription {
        let payload = normalize_openai_subscription_realtime_call_payload(
            &headers,
            bytes::Bytes::from(serde_json::to_vec(&body)?),
            native_live,
        )
        .await?;
        if payload.upstream_model != model {
            return Err(anyhow!(
                "subscription voice model does not match the platform billing model"
            ));
        }
        send_openai_subscription_realtime_call(
            client,
            channel,
            &codex_realtime_calls_url()?,
            &headers,
            payload,
        )
        .await?
    } else {
        let target = crate::channel_surface::channel_surface_target(
            channel,
            crate::surface::ApiSurface::OpenAi,
            None,
            true,
        )
        .filter(|target| target.surface == crate::surface::ApiSurface::OpenAi)
        .ok_or_else(|| anyhow!("channel has no OpenAI voice surface"))?;
        let path = if native_live {
            "/v1/live"
        } else {
            "/v1/realtime/calls"
        };
        let url = crate::surface_wire::merge_raw_query(
            &crate::supplier::join_upstream_url(&target.base_url, path),
            &wire.raw_query,
            wire.has_query,
        );
        let mut upstream_headers = reqwest::header::HeaderMap::new();
        copy_safe_websocket_headers(&mut upstream_headers, &headers);
        upstream_headers.remove(reqwest::header::CONTENT_TYPE);
        crate::channel_executor::apply_upstream_auth(
            &mut upstream_headers,
            channel,
            &target,
            true,
        )?;
        // Both public Realtime and Codex's API /live endpoint use multipart;
        // only the subscription backend above uses the JSON bootstrap shape.
        let request = {
            let boundary = format!("const-voice-{:032x}", rand::random::<u128>());
            let multipart = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"sdp\"\r\nContent-Type: application/sdp\r\n\r\n{}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"session\"\r\nContent-Type: application/json\r\n\r\n{}\r\n--{boundary}--\r\n",
                body["sdp"].as_str().unwrap_or_default(), serde_json::to_string(&body["session"])?);
            client
                .post(url)
                .headers(upstream_headers)
                .header(
                    reqwest::header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(multipart)
        };
        crate::upstream_transport::send(request).await?
    };
    let status = response.status();
    let call_id = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|location| location.trim_end_matches('/').rsplit('/').next())
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 256
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        })
        .map(str::to_string);
    if !status.is_success() {
        // Never log SDP, bearer tokens or a provider response containing them.
        let mut stream = response.bytes_stream();
        let mut error_body = Vec::new();
        while let Some(Ok(chunk)) = stream.next().await {
            if error_body.len() + chunk.len() > 32 * 1024 {
                break;
            }
            error_body.extend_from_slice(&chunk);
        }
        let parsed = serde_json::from_slice::<serde_json::Value>(&error_body).unwrap_or_default();
        let code = parsed
            .pointer("/error/code")
            .or_else(|| parsed.get("code"))
            .or_else(|| parsed.pointer("/error/type"))
            .and_then(serde_json::Value::as_str)
            .filter(|code| {
                code.len() <= 128
                    && code.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
                    })
            })
            .unwrap_or("unclassified")
            .to_string();
        #[cfg(test)]
        if std::env::var_os("CONST_API_LIVE_VOICE_CONFIG").is_some() {
            let message = parsed
                .pointer("/error/message")
                .or_else(|| parsed.get("detail"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("non-JSON provider rejection");
            std::eprintln!(
                "Voice provider rejection: {}",
                message.chars().take(400).collect::<String>()
            );
        }
        return Err(VoiceCallSetupError {
            stage: "bootstrap",
            status: status.as_u16(),
            code,
        }
        .into());
    }
    let call_id = call_id.ok_or_else(|| anyhow!("voice provider omitted its call Location"))?;
    if response
        .content_length()
        .is_some_and(|length| length > OPENAI_REALTIME_SDP_MAX_BYTES as u64)
    {
        return Err(anyhow!("voice provider SDP is too large"));
    }
    let mut stream = response.bytes_stream();
    let mut answer = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if answer.len() + chunk.len() > OPENAI_REALTIME_SDP_MAX_BYTES {
            return Err(anyhow!("voice provider SDP is too large"));
        }
        answer.extend_from_slice(&chunk);
    }
    let answer = std::str::from_utf8(&answer).context("voice provider SDP is not UTF-8")?;
    peer.accept_answer(answer)
        .await
        .context("voice SDP negotiation failed")?;
    let socket = if subscription {
        connect_openai_subscription_live(client, channel, Some(&call_id), "", &headers)
            .await
            .map_err(|error| VoiceCallSetupError {
                stage: "sideband",
                status: responses_websocket_error_status(&error).unwrap_or(502),
                code: "handshake_failed".into(),
            })?
    } else {
        let (kind, path, query) = if native_live {
            (
                NativeDuplexKind::OpenAiLive,
                format!("/v1/live/{call_id}"),
                String::new(),
            )
        } else {
            (
                NativeDuplexKind::OpenAiRealtime,
                "/v1/realtime".into(),
                format!("call_id={call_id}"),
            )
        };
        let request = native_duplex_upstream_request(channel, kind, &path, &query, &headers)?;
        connect_responses_websocket_request(request)
            .await
            .map_err(|error| VoiceCallSetupError {
                stage: "sideband",
                status: responses_websocket_error_status(&error).unwrap_or(502),
                code: "handshake_failed".into(),
            })?
    };
    peer.wait_connected()
        .await
        .context("voice media ICE/DTLS connection failed")?;
    Ok((peer, socket))
}

async fn run_supplier_voice_call(
    client: &Client,
    config: &SupplierConfig,
    open: SupplierMessage,
    mut commands: tokio::sync::mpsc::Receiver<SupplierMessage>,
    outbound: &SupplierOutbound,
) -> Result<()> {
    use crate::supplier::PlatformDuplexFrame as Frame;
    let wire = crate::surface_wire::SurfaceEnvelope::from_payload(&open.payload)?;
    // A call can start upstream work during bootstrap. Wait for the platform's
    // first lease, sent only after it reserves the session's concurrency slot.
    let start = tokio::time::timeout(Duration::from_secs(10), commands.recv()).await;
    if !matches!(start, Ok(Some(ref command)) if command.kind == "duplex_client_frame"
        && command.payload.get("control").and_then(serde_json::Value::as_str) == Some("media_lease"))
    {
        return Ok(());
    }
    let channel = channel_from_supplier(config.channel_id.clone(), config);
    let subscription =
        config.source_driver == crate::source_driver::SourceDriverId::OpenAiSubscription;
    let _activity = crate::update_activity::try_begin_supplier_request(&open.id)
        .ok_or_else(|| anyhow!("client update is starting; retry in 2 seconds"))?;
    let _safety = if subscription {
        match subscription_safety_enter(
            config,
            &serde_json::json!({"model":wire.selected_upstream_model}).to_string(),
        ) {
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
    let bootstrap = tokio::time::timeout(
        Duration::from_secs(40),
        open_supplier_voice_call(client, &channel, &wire),
    );
    let opened = tokio::select! {
        result = bootstrap => result.map_err(|_| anyhow!("voice call bootstrap timed out")).and_then(|result| result),
        _ = commands.recv() => return Ok(()),
        _ = crate::update_activity::wait_for_update_drain() => return Ok(()),
    };
    let (mut peer, socket) = match opened {
        Ok(opened) => opened,
        Err(error) => {
            let rejection = error.downcast_ref::<VoiceCallSetupError>();
            let status = rejection.map(|error| error.status).unwrap_or(502);
            let stage = rejection
                .map(|error| error.stage)
                .unwrap_or("media_or_transport");
            let code = rejection
                .map(|error| error.code.as_str())
                .unwrap_or("connection_failed");
            log::warn!("[const-api][realtime] supplier call failed channel_id={} status={} stage={} provider_code={}", channel.id, status, stage, code);
            send_native_supplier_message(outbound, SupplierMessage { id:open.id, kind:"duplex_error".into(),
                payload: serde_json::json!({"status":status, "failure_scope":"operation", "upstream_attempted":true,
                    "message": format!("voice {stage} failed (HTTP {status}, provider_code={code})"), "error_kind":"realtime_call_failed"}).as_object().cloned().unwrap_or_default(),
            }).await?;
            return Ok(());
        }
    };
    send_native_supplier_message(
        outbound,
        SupplierMessage {
            id: open.id.clone(),
            kind: "duplex_opened".into(),
            payload: serde_json::Map::new(),
        },
    )
    .await?;
    send_native_supplier_message(
        outbound,
        SupplierMessage {
            id: open.id.clone(),
            kind: "duplex_server_frame".into(),
            payload: crate::supplier::platform_frame_payload(Frame::Text(
                serde_json::json!({"type":PLATFORM_VOICE_READY}).to_string(),
            )),
        },
    )
    .await?;
    let (mut sender, mut receiver) = socket.split();
    let mut stopped = peer.stopped();
    let lease = tokio::time::sleep(Duration::from_secs(20));
    let lifetime = tokio::time::sleep(Duration::from_secs(3600));
    tokio::pin!(lease, lifetime);
    loop {
        let frame = tokio::select! {
            _ = crate::realtime_media::wait_stopped(&mut stopped) => break,
            _ = &mut lease => break,
            _ = &mut lifetime => break,
            _ = crate::update_activity::wait_for_update_drain() => break,
            command = commands.recv() => {
                let Some(command) = command else { break };
                if command.kind == "duplex_close" { break; }
                if command.kind != "duplex_client_frame" { continue; }
                lease.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(20));
                if command.payload.get("control").and_then(serde_json::Value::as_str) == Some("media_lease") { continue; }
                match crate::supplier::platform_server_frame(&command.payload)? {
                    Frame::Binary(bytes) => { if peer.send(&bytes).await.is_err() { break; } }
                    Frame::Text(text) => { if sender.send(tokio_tungstenite::tungstenite::Message::Text(text.into())).await.is_err() { break; } }
                    Frame::Close { .. } => break,
                }
                continue;
            }
            bytes = peer.received.recv() => { let Some(bytes) = bytes else { break }; Frame::Binary(bytes) }
            message = receiver.next() => {
                match message {
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => Frame::Text(text.to_string()),
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => continue,
                }
            }
        };
        if send_native_supplier_message(
            outbound,
            SupplierMessage {
                id: open.id.clone(),
                kind: "duplex_server_frame".into(),
                payload: crate::supplier::platform_frame_payload(frame),
            },
        )
        .await
        .is_err()
        {
            break;
        }
    }
    drop(peer);
    close_upstream_websocket_sender(&mut sender).await;
    send_native_supplier_message(
        outbound,
        SupplierMessage {
            id: open.id,
            kind: "duplex_close".into(),
            payload: serde_json::Map::new(),
        },
    )
    .await
}
