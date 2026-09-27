// Negotiated transport framing only; logical request IDs, replay and business
// message ordering are unchanged. Only heartbeats may overtake a partial message.
const SUPPLIER_MESSAGE_CHUNKS_FEATURE: &str = "message_chunks_v1";
const SUPPLIER_MESSAGE_CHUNK_TYPE: &str = "transport_chunk";
const SUPPLIER_MESSAGE_CHUNK_BYTES: usize = 64 * 1024;
const SUPPLIER_MESSAGE_CHUNK_THRESHOLD: usize = 256 * 1024;

struct SupplierHeartbeatWritten {
    id: String,
    response_timeout: Duration,
}

#[derive(Default)]
struct SupplierLegacyWriteDrain(Option<Instant>);

impl SupplierLegacyWriteDrain {
    fn record(&mut self, wire_bytes: usize) {
        // A legacy unsplit frame may remain in TCP/QUIC buffers after write()
        // returns. Bound its drain allowance by bytes, not a blanket heartbeat
        // relaxation. Negotiated chunks are far below this size threshold.
        if wire_bytes <= 1024 * 1024 {
            return;
        }
        let now = Instant::now();
        let drain = Duration::from_millis((wire_bytes as u64).saturating_mul(1000) / (256 * 1024));
        let until = self.0.unwrap_or(now).max(now) + drain;
        self.0 = Some(until.min(now + Duration::from_secs(120)));
    }

    fn written(&mut self, id: String) -> SupplierHeartbeatWritten {
        // Business traffic can now resolve a heartbeat before its pong arrives.
        // Keep the absolute drain deadline until it expires, rather than consume
        // it with the first probe while legacy bytes may still be queued.
        let drain = self
            .0
            .map(|until| until.saturating_duration_since(Instant::now()))
            .unwrap_or_default();
        SupplierHeartbeatWritten {
            id,
            response_timeout: supplier_heartbeat_response_timeout() + drain,
        }
    }
}

#[derive(Default)]
struct SupplierPendingTransportOutput(Option<SupplierMessage>);

#[derive(Default)]
struct SupplierPendingTransportControl {
    // Only the latest token matters before a refresh can be queued. Keeping
    // these two slots separate from responses bounds memory during backpressure.
    auth_refresh: Option<SupplierMessage>,
    encryption_confirmation: Option<SupplierMessage>,
}

impl SupplierPendingTransportControl {
    fn is_pending(&self) -> bool {
        self.auth_refresh.is_some() || self.encryption_confirmation.is_some()
    }

    fn next(&mut self) -> Option<SupplierMessage> {
        self.encryption_confirmation
            .take()
            .or_else(|| self.auth_refresh.take())
    }
}

impl Drop for SupplierPendingTransportOutput {
    fn drop(&mut self) {
        if let Some(message) = self.0.take() {
            crate::update_activity::supplier_output_send_failed_or_delivered_without_replay(
                &message.id,
            );
        }
    }
}

fn send_reserved_supplier_message(
    permit: mpsc::OwnedPermit<SupplierMessage>,
    message: SupplierMessage,
) {
    let pending = crate::update_activity::track_supplier_output_before_send();
    let id = message.id.clone();
    permit.send(message);
    pending.transfer(&id);
}

struct SupplierOutgoingPart {
    raw: Vec<u8>,
    kind: String,
    heartbeat_id: Option<String>,
}

struct SupplierTransportSendQueue {
    data: mpsc::Receiver<SupplierMessage>,
    heartbeats: mpsc::Receiver<SupplierMessage>,
    current: Option<(String, SupplierMessageParts)>,
}

impl SupplierTransportSendQueue {
    async fn next(
        &mut self,
        codec: &SupplierTransportCodec,
    ) -> Result<Option<SupplierOutgoingPart>> {
        loop {
            if self
                .current
                .as_ref()
                .is_some_and(|(_, parts)| parts.offset >= parts.raw.len())
            {
                self.current = None;
            }
            if self.current.is_none() {
                if let Ok(message) = self.data.try_recv() {
                    self.current = Some((message.kind.clone(), codec.prepare_message(message)?));
                }
            }
            // Authentication and encryption confirmation are ordering barriers.
            // Start the registration before the first heartbeat as legacy peers
            // expect, then allow heartbeats between its remaining fragments.
            if let Some((kind, parts)) = &mut self.current {
                if parts.offset == 0
                    && matches!(
                        kind.as_str(),
                        "authenticate" | "auth_refresh" | "payload_encryption_confirm" | "register"
                    )
                {
                    if let Some(raw) = parts.next()? {
                        return Ok(Some(SupplierOutgoingPart {
                            raw,
                            kind: kind.clone(),
                            heartbeat_id: None,
                        }));
                    }
                }
            }
            if let Ok(message) = self.heartbeats.try_recv() {
                return Ok(Some(SupplierOutgoingPart {
                    heartbeat_id: (message.kind == "ping").then_some(message.id.clone()),
                    raw: serde_json::to_vec(&message)?,
                    kind: message.kind,
                }));
            }
            if let Some((kind, parts)) = &mut self.current {
                if let Some(raw) = parts.next()? {
                    return Ok(Some(SupplierOutgoingPart {
                        raw,
                        kind: kind.clone(),
                        heartbeat_id: None,
                    }));
                }
                self.current = None;
            }
            let message = tokio::select! {
                biased;
                Some(message) = self.heartbeats.recv() => {
                    return Ok(Some(SupplierOutgoingPart { heartbeat_id:(message.kind == "ping").then_some(message.id.clone()), raw:serde_json::to_vec(&message)?, kind:message.kind }));
                }
                message = self.data.recv() => message,
            };
            let Some(message) = message else {
                return Ok(None);
            };
            self.current = Some((message.kind.clone(), codec.prepare_message(message)?));
        }
    }
}

struct SupplierMessageParts {
    raw: Vec<u8>,
    offset: usize,
    chunked: bool,
}

impl SupplierMessageParts {
    fn new(raw: Vec<u8>, enabled: bool) -> Result<Self> {
        if raw.len() > SUPPLIER_TRANSPORT_COMPRESSION_MAX_RAW_LEN {
            return Err(anyhow!("supplier message exceeds max size"));
        }
        let chunked = enabled && raw.len() > SUPPLIER_MESSAGE_CHUNK_THRESHOLD;
        Ok(Self {
            raw,
            offset: 0,
            chunked,
        })
    }

    fn next(&mut self) -> Result<Option<Vec<u8>>> {
        if self.offset >= self.raw.len() {
            return Ok(None);
        }
        if !self.chunked {
            self.offset = self.raw.len();
            return Ok(Some(std::mem::take(&mut self.raw)));
        }
        let mut end = (self.offset + SUPPLIER_MESSAGE_CHUNK_BYTES).min(self.raw.len());
        // The logical wire is UTF-8 JSON already. String fragments avoid the
        // extra base64 expansion for large bodies which are themselves base64.
        while end < self.raw.len() && self.raw[end] & 0xc0 == 0x80 {
            end -= 1;
        }
        let data = std::str::from_utf8(&self.raw[self.offset..end])?;
        let part = serde_json::to_vec(&serde_json::json!({
            "id":"", "type":SUPPLIER_MESSAGE_CHUNK_TYPE,
            "payload":{"offset":self.offset,"total":self.raw.len(),
                "data":data}
        }))?;
        self.offset = end;
        Ok(Some(part))
    }
}

struct SupplierPartialMessage {
    total: usize,
    bytes: Vec<u8>,
    started: Instant,
}

#[derive(Default)]
struct SupplierMessageAssembler(Option<SupplierPartialMessage>);

impl SupplierMessageAssembler {
    fn accept(
        &mut self,
        message: SupplierMessage,
        enabled: bool,
    ) -> Result<Option<SupplierMessage>> {
        if self
            .0
            .as_ref()
            .is_some_and(|partial| partial.started.elapsed() > Duration::from_secs(300))
        {
            return Err(anyhow!("supplier incomplete message expired"));
        }
        if message.kind != SUPPLIER_MESSAGE_CHUNK_TYPE {
            if self.0.is_some() && !matches!(message.kind.as_str(), "ping" | "pong") {
                return Err(anyhow!(
                    "supplier data message interleaved with incomplete chunks"
                ));
            }
            return Ok(Some(message));
        }
        if !enabled {
            return Err(anyhow!("supplier message chunks were not negotiated"));
        }
        let offset = message
            .payload
            .get("offset")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| anyhow!("invalid supplier chunk offset"))?;
        let total = message
            .payload
            .get("total")
            .and_then(serde_json::Value::as_u64)
            .filter(|n| *n > 0 && *n <= SUPPLIER_TRANSPORT_COMPRESSION_MAX_RAW_LEN as u64)
            .ok_or_else(|| anyhow!("invalid supplier chunk total"))? as usize;
        let data = message
            .payload
            .get("data")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty() && s.len() <= SUPPLIER_MESSAGE_CHUNK_BYTES)
            .ok_or_else(|| anyhow!("invalid supplier chunk data size"))?;
        let bytes = data.as_bytes();
        if self.0.is_none() {
            if offset != 0 {
                return Err(anyhow!("supplier chunk starts at nonzero offset"));
            }
            self.0 = Some(SupplierPartialMessage {
                total,
                bytes: Vec::new(),
                started: Instant::now(),
            });
        }
        let Some(partial) = self.0.as_mut() else {
            return Err(anyhow!("missing supplier chunk state"));
        };
        if total != partial.total
            || offset != partial.bytes.len() as u64
            || bytes.len() > total.saturating_sub(partial.bytes.len())
        {
            return Err(anyhow!("supplier chunk order, size or lifetime is invalid"));
        }
        let required = partial.bytes.len() + bytes.len();
        if required > partial.bytes.capacity() {
            let capacity = partial
                .bytes
                .capacity()
                .saturating_mul(2)
                .max(required)
                .min(total);
            partial.bytes.reserve_exact(capacity - partial.bytes.len());
        }
        partial.bytes.extend_from_slice(bytes);
        if partial.bytes.len() < total {
            return Ok(None);
        }
        let Some(partial) = self.0.take() else {
            return Err(anyhow!("missing supplier completed chunks"));
        };
        let message: SupplierMessage = serde_json::from_slice(&partial.bytes)
            .context("invalid reassembled supplier message")?;
        if message.kind == SUPPLIER_MESSAGE_CHUNK_TYPE {
            return Err(anyhow!("nested supplier message chunks are forbidden"));
        }
        Ok(Some(message))
    }

    fn finish(&self) -> Result<()> {
        if self.0.is_some() {
            return Err(anyhow!(
                "supplier connection closed with an incomplete message"
            ));
        }
        Ok(())
    }
}

/// Serialize/compress outside the encryption-state lock. Large sends must not
/// prevent the receive loop from decrypting heartbeats on the same connection.
struct SupplierPreparedPayload {
    raw_len: usize,
    content: Vec<u8>,
    compressed: bool,
}

impl SupplierPreparedPayload {
    fn new(raw: Vec<u8>, compression: bool) -> Result<Self> {
        if raw.is_empty() || raw.len() > SUPPLIER_TRANSPORT_COMPRESSION_MAX_RAW_LEN {
            return Err(anyhow!("supplier message exceeds max size"));
        }
        let raw_len = raw.len();
        if compression && raw_len >= SUPPLIER_TRANSPORT_COMPRESSION_MIN_BYTES {
            let content = zstd::bulk::compress(&raw, 5)?;
            if content.len() + SUPPLIER_TRANSPORT_FRAME_HEADER_BYTES < raw_len {
                return Ok(Self {
                    raw_len,
                    content,
                    compressed: true,
                });
            }
        }
        Ok(Self {
            raw_len,
            content: raw,
            compressed: false,
        })
    }

    fn plaintext(self) -> (Vec<u8>, bool) {
        if !self.compressed {
            return (self.content, false);
        }
        let mut frame =
            Vec::with_capacity(SUPPLIER_TRANSPORT_FRAME_HEADER_BYTES + self.content.len());
        frame.extend_from_slice(&SUPPLIER_TRANSPORT_FRAME_MAGIC);
        frame.extend_from_slice(&(self.raw_len as u32).to_be_bytes());
        frame.extend_from_slice(&(self.content.len() as u32).to_be_bytes());
        frame.extend_from_slice(&self.content);
        (frame, true)
    }
}

#[cfg(test)]
mod message_chunk_tests {
    use super::*;

    fn codec_pair(encrypted: bool) -> (SupplierTransportCodec, SupplierTransportCodec) {
        let sender = SupplierTransportCodec::default();
        let receiver = SupplierTransportCodec::default();
        for codec in [&sender, &receiver] {
            codec
                .frame_compression_enabled
                .store(true, Ordering::Relaxed);
            codec.message_chunks_enabled.store(true, Ordering::Relaxed);
        }
        if encrypted {
            sender.payload_encryption.lock().unwrap().state = Some(
                SupplierPayloadEncryptionState::new(
                    [1; 32],
                    [2; 32],
                    "chunk-test".into(),
                    SupplierPayloadEncryptionPhase::Active,
                )
                .unwrap(),
            );
            receiver.payload_encryption.lock().unwrap().state = Some(
                SupplierPayloadEncryptionState::new(
                    [2; 32],
                    [1; 32],
                    "chunk-test".into(),
                    SupplierPayloadEncryptionPhase::Active,
                )
                .unwrap(),
            );
        }
        (sender, receiver)
    }

    #[tokio::test]
    async fn websocket_and_quic_chunks_preserve_encryption_sequence_and_utf8() {
        for encrypted in [false, true] {
            for quic in [false, true] {
                let (sender, receiver) = codec_pair(encrypted);
                let original = large_message();
                let expected = serde_json::to_vec(&original).unwrap();
                let mut parts = sender.prepare_message(original).unwrap();
                let mut frames = Vec::new();
                let mut pongs = 0;
                while let Some(raw) = parts.next().unwrap() {
                    let (wire, binary) = sender.encode_raw(raw).unwrap();
                    assert!(wire.len() < 128 * 1024);
                    if encrypted {
                        assert!(wire.starts_with(&SUPPLIER_PAYLOAD_ENCRYPTION_FRAME_MAGIC));
                    }
                    frames.push((wire, binary));
                    pongs += 1;
                    frames.push(
                        sender
                            .encode(SupplierMessage {
                                id: format!("pong-{pongs}"),
                                kind: "pong".into(),
                                payload: Default::default(),
                            })
                            .unwrap(),
                    );
                }
                let mut decoded = Vec::new();
                if quic {
                    let (reader, mut writer) = tokio::io::duplex(257);
                    let sending = tokio::spawn(async move {
                        for (mut wire, binary) in frames {
                            if !binary {
                                wire.push(b'\n');
                            }
                            for bytes in wire.chunks(23) {
                                writer.write_all(bytes).await.unwrap();
                            }
                        }
                        writer.shutdown().await.unwrap();
                    });
                    let mut reader = BufReader::with_capacity(71, reader);
                    while let Some(text) = receiver.read_quic(&mut reader).await.unwrap() {
                        decoded.push(text);
                    }
                    sending.await.unwrap();
                } else {
                    for (wire, binary) in frames {
                        let frame = if binary {
                            WsMessage::Binary(wire.into())
                        } else {
                            WsMessage::Text(String::from_utf8(wire).unwrap().into())
                        };
                        if let Some(text) = receiver.decode_websocket(frame).unwrap() {
                            decoded.push(text);
                        }
                    }
                }
                assert_eq!(decoded.len(), pongs + 1);
                assert!(decoded.first().unwrap().contains("pong-1"));
                let complete: Vec<SupplierMessage> = decoded
                    .iter()
                    .map(|text| serde_json::from_str(text).unwrap())
                    .filter(|message: &SupplierMessage| message.kind != "pong")
                    .collect();
                assert_eq!(complete.len(), 1);
                assert_eq!(serde_json::to_vec(&complete[0]).unwrap(), expected);
            }
        }
    }

    #[tokio::test]
    async fn incomplete_quic_message_is_rejected_and_reconnect_starts_clean() {
        let mut parts =
            SupplierMessageParts::new(serde_json::to_vec(&large_message()).unwrap(), true).unwrap();
        let first = parts.next().unwrap().unwrap();
        let (_, old) = codec_pair(false);
        let mut wire = first;
        wire.push(b'\n');
        let error = old.read_quic(&mut &wire[..]).await.unwrap_err();
        assert!(error.to_string().contains("incomplete message"));
        let (_, fresh) = codec_pair(false);
        let message = large_message();
        assert!(fresh
            .decode_websocket(WsMessage::Text(
                serde_json::to_string(&message).unwrap().into()
            ))
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn heartbeat_never_overtakes_encryption_confirmation_or_registration_start() {
        let codec = SupplierTransportCodec::default();
        codec.message_chunks_enabled.store(true, Ordering::Relaxed);
        let (data, rx) = mpsc::channel(3);
        let (control, control_rx) = mpsc::channel(1);
        let mut queue = SupplierTransportSendQueue {
            data: rx,
            heartbeats: control_rx,
            current: None,
        };
        data.send(SupplierMessage {
            id: "confirm".into(),
            kind: SUPPLIER_PAYLOAD_ENCRYPTION_CONFIRM.into(),
            payload: Default::default(),
        })
        .await
        .unwrap();
        let mut registration = large_message();
        registration.kind = "register".into();
        data.send(registration).await.unwrap();
        enqueue_supplier_heartbeat(&control).unwrap();
        assert_eq!(
            queue.next(&codec).await.unwrap().unwrap().kind,
            SUPPLIER_PAYLOAD_ENCRYPTION_CONFIRM
        );
        assert_eq!(queue.next(&codec).await.unwrap().unwrap().kind, "register");
        assert_eq!(queue.next(&codec).await.unwrap().unwrap().kind, "ping");
        assert_eq!(queue.next(&codec).await.unwrap().unwrap().kind, "register");
    }

    fn large_message() -> SupplierMessage {
        SupplierMessage {
            id: "large-request".into(),
            kind: "http_response".into(),
            payload: serde_json::json!({"body":"含 UTF-8 🦀 payload ".repeat(30000)})
                .as_object()
                .unwrap()
                .clone(),
        }
    }

    #[test]
    fn chunks_roundtrip_atomically_with_interleaved_pong_and_bounded_frames() {
        let message = large_message();
        let expected = serde_json::to_vec(&message).unwrap();
        let mut parts = SupplierMessageParts::new(expected.clone(), true).unwrap();
        let mut assembler = SupplierMessageAssembler::default();
        let mut count = 0;
        let mut result = None;
        while let Some(raw) = parts.next().unwrap() {
            assert!(raw.len() < 90 * 1024);
            count += 1;
            let part: SupplierMessage = serde_json::from_slice(&raw).unwrap();
            let complete = assembler.accept(part, true).unwrap();
            if let Some(partial) = &assembler.0 {
                assert!(partial.bytes.capacity() <= partial.total);
            }
            if complete.is_some() {
                assert!(result.is_none());
                result = complete;
            } else {
                let pong = SupplierMessage {
                    id: "heartbeat".into(),
                    kind: "pong".into(),
                    payload: Default::default(),
                };
                assert_eq!(assembler.accept(pong, true).unwrap().unwrap().kind, "pong");
            }
        }
        assert!(count > 4);
        assert_eq!(serde_json::to_vec(&result.unwrap()).unwrap(), expected);
        assert!(assembler.0.is_none());
    }

    #[test]
    fn chunks_reject_unnegotiated_oversized_out_of_order_nested_and_interleaved_data() {
        let part = |offset, total, data: &[u8]| {
            SupplierMessage {id:String::new(),kind:SUPPLIER_MESSAGE_CHUNK_TYPE.into(),
            payload:serde_json::json!({"offset":offset,"total":total,"data":std::str::from_utf8(data).unwrap()}).as_object().unwrap().clone()}
        };
        assert!(SupplierMessageAssembler::default()
            .accept(part(0, 10, b"hello"), false)
            .is_err());
        assert!(SupplierMessageAssembler::default()
            .accept(part(1, 10, b"hello"), true)
            .is_err());
        assert!(SupplierMessageAssembler::default()
            .accept(part(0, usize::MAX, b"hello"), true)
            .is_err());
        assert!(SupplierMessageAssembler::default()
            .accept(part(0, 1, b"hello"), true)
            .is_err());
        let mut assembler = SupplierMessageAssembler::default();
        assert!(assembler
            .accept(part(0, 10, b"hello"), true)
            .unwrap()
            .is_none());
        assert!(assembler.accept(part(0, 10, b"hello"), true).is_err());
        assert!(assembler.accept(large_message(), true).is_err());
        let nested = serde_json::to_vec(&part(0, 2, b"{}")).unwrap();
        assert!(SupplierMessageAssembler::default()
            .accept(part(0, nested.len(), &nested), true)
            .is_err());
        assembler.0.as_mut().unwrap().started = Instant::now() - Duration::from_secs(301);
        assert!(assembler.accept(part(5, 10, b"hello"), true).is_err());
    }

    #[test]
    fn legacy_peer_keeps_the_original_message_and_small_packets_are_not_chunked() {
        let raw = serde_json::to_vec(&large_message()).unwrap();
        let mut legacy = SupplierMessageParts::new(raw.clone(), false).unwrap();
        assert_eq!(legacy.next().unwrap(), Some(raw));
        assert!(legacy.next().unwrap().is_none());
        let raw = b"{\"id\":\"small\",\"type\":\"ping\",\"payload\":{}}".to_vec();
        assert_eq!(
            SupplierMessageParts::new(raw.clone(), true)
                .unwrap()
                .next()
                .unwrap(),
            Some(raw)
        );
    }

    #[tokio::test]
    async fn writer_services_heartbeat_between_chunks_without_reordering_data_messages() {
        let codec = SupplierTransportCodec::default();
        codec.message_chunks_enabled.store(true, Ordering::Relaxed);
        let (data, rx) = mpsc::channel(2);
        let (control, control_rx) = mpsc::channel(1);
        let mut queue = SupplierTransportSendQueue {
            data: rx,
            heartbeats: control_rx,
            current: None,
        };
        data.send(large_message()).await.unwrap();
        let first = queue.next(&codec).await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_slice::<SupplierMessage>(&first.raw)
                .unwrap()
                .kind,
            SUPPLIER_MESSAGE_CHUNK_TYPE
        );
        let id = enqueue_supplier_heartbeat(&control).unwrap().unwrap();
        data.send(SupplierMessage {
            id: "after".into(),
            kind: "stream_end".into(),
            payload: Default::default(),
        })
        .await
        .unwrap();
        let heartbeat = queue.next(&codec).await.unwrap().unwrap();
        assert_eq!(heartbeat.heartbeat_id, Some(id));
        let mut assembled = SupplierMessageAssembler::default();
        assembled
            .accept(serde_json::from_slice(&first.raw).unwrap(), true)
            .unwrap();
        let mut completed = false;
        loop {
            let part = queue.next(&codec).await.unwrap().unwrap();
            let msg: SupplierMessage = serde_json::from_slice(&part.raw).unwrap();
            if msg.id == "after" {
                assert!(completed);
                break;
            }
            if let Some(msg) = assembled.accept(msg, true).unwrap() {
                assert_eq!(msg.id, "large-request");
                completed = true;
            }
        }
    }

    #[test]
    fn heartbeat_write_receipt_does_not_rearm_a_pong_already_received() {
        let mut heartbeat = SupplierHeartbeat::default();
        heartbeat.queued("ping-1".into());
        assert!(heartbeat.timeout_detail().contains("not written"));
        assert!(!heartbeat.written("old", supplier_heartbeat_response_timeout()));
        assert!(heartbeat.written("ping-1", supplier_heartbeat_response_timeout()));
        assert!(heartbeat.timeout_detail().contains(&format!(
            "within {} seconds after heartbeat write",
            supplier_heartbeat_response_timeout().as_secs()
        )));
        assert!(!heartbeat.written("ping-1", supplier_heartbeat_response_timeout()));
        let activity = SupplierConnectionActivity::default();
        activity.received(false);
        heartbeat.observe(&activity);
        assert!(!heartbeat.pending());
        assert!(!heartbeat.written("ping-1", supplier_heartbeat_response_timeout()));
        heartbeat.queued("ping-2".into());
        activity.received(false);
        heartbeat.observe(&activity);
        assert!(!heartbeat.written("ping-2", supplier_heartbeat_response_timeout()));
    }

    #[test]
    fn heartbeat_drain_allowance_is_only_for_large_legacy_frames_and_is_bounded() {
        let mut drain = SupplierLegacyWriteDrain::default();
        let response_timeout = supplier_heartbeat_response_timeout();
        drain.record(128 * 1024);
        assert_eq!(
            drain.written("ping".into()).response_timeout,
            response_timeout
        );
        drain.record(10 * 1024 * 1024);
        let timeout = drain.written("ping".into()).response_timeout;
        assert!(
            timeout > response_timeout + Duration::from_secs(37)
                && timeout <= response_timeout + Duration::from_secs(40)
        );
        assert!(drain.written("next-ping".into()).response_timeout
            > response_timeout + Duration::from_secs(35),
            "business progress must not consume the remaining legacy drain allowance");
        drain.record(64 * 1024 * 1024);
        assert!(
            drain.written("ping".into()).response_timeout
                <= response_timeout + Duration::from_secs(120)
        );
        drain.0 = Some(Instant::now() - Duration::from_secs(1));
        assert_eq!(
            drain.written("ping".into()).response_timeout,
            response_timeout
        );
    }

    #[tokio::test]
    async fn valid_fragments_refresh_liveness_before_the_whole_message_arrives() {
        for quic in [false, true] {
            let (sender, receiver) = codec_pair(true);
            let receiver = Arc::new(receiver);
            let mut parts = sender.prepare_message(large_message()).unwrap();
            let (wire, _) = sender.encode_raw(parts.next().unwrap().unwrap()).unwrap();
            let mut heartbeat = SupplierHeartbeat::default();
            heartbeat.queued("waiting".into());
            if quic {
                let (reader, mut writer) = tokio::io::duplex(wire.len() + 1);
                writer.write_all(&wire).await.unwrap();
                let codec = receiver.clone();
                let task = tokio::spawn(async move { codec.read_quic(&mut BufReader::new(reader)).await });
                for _ in 0..100 {
                    if receiver.activity.received.load(Ordering::Relaxed) { break; }
                    tokio::task::yield_now().await;
                }
                assert!(!task.is_finished(), "the next fragment has not arrived");
                assert!(heartbeat.observe(&receiver.activity));
                task.abort();
                let _ = task.await;
            } else {
                assert!(receiver.decode_websocket(WsMessage::Binary(wire.into())).unwrap().is_none());
                assert!(heartbeat.observe(&receiver.activity));
            }
            assert!(!heartbeat.pending());
        }
    }
}
