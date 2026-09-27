const SUPPLIER_PAYLOAD_ENCRYPTION_FEATURE: &str = "x25519_aes256gcm_v1";
const SUPPLIER_PAYLOAD_ENCRYPTION_OBJECT_KEY: &str = "payload_encryption";
const SUPPLIER_PAYLOAD_ENCRYPTION_CONTEXT: &[u8] = b"const-api-supplier-payload-aead-v1";
const SUPPLIER_PAYLOAD_ENCRYPTION_CONFIRM: &str = "payload_encryption_confirm";
const SUPPLIER_PAYLOAD_ENCRYPTION_FRAME_HEADER_BYTES: usize = 24;
const SUPPLIER_PAYLOAD_ENCRYPTION_COMPRESSED_FLAG: u8 = 1;
const SUPPLIER_PAYLOAD_ENCRYPTION_FRAME_MAGIC: [u8; 4] = [0, b'C', b'E', b'1'];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SupplierPayloadEncryptionPhase {
    ConfirmReady,
    ConfirmSent,
    Active,
}

struct SupplierPayloadEncryptionOffer {
    private_key: Option<ring::agreement::EphemeralPrivateKey>,
    public_key: [u8; 32],
    nonce: [u8; 32],
    expected_identity_sha256: [u8; 32],
    sent: bool,
}

impl SupplierPayloadEncryptionOffer {
    fn new(expected_identity_sha256: [u8; 32]) -> Result<Self> {
        let rng = ring::rand::SystemRandom::new();
        let private_key =
            ring::agreement::EphemeralPrivateKey::generate(&ring::agreement::X25519, &rng)
                .map_err(|_| anyhow!("generate supplier payload encryption key"))?;
        let public_key: [u8; 32] = private_key
            .compute_public_key()
            .map_err(|_| anyhow!("derive supplier payload encryption public key"))?
            .as_ref()
            .try_into()
            .map_err(|_| anyhow!("supplier payload encryption public key has invalid length"))?;
        let mut nonce = [0u8; 32];
        ring::rand::SecureRandom::fill(&rng, &mut nonce)
            .map_err(|_| anyhow!("generate supplier payload encryption nonce"))?;
        Ok(Self {
            private_key: Some(private_key),
            public_key,
            nonce,
            expected_identity_sha256,
            sent: false,
        })
    }
}

struct SupplierPayloadEncryptionState {
    send_key: ring::aead::LessSafeKey,
    receive_key: ring::aead::LessSafeKey,
    send_sequence: u64,
    receive_sequence: u64,
    handshake_id: String,
    phase: SupplierPayloadEncryptionPhase,
}

#[derive(Default)]
struct SupplierPayloadEncryption {
    offer: Option<SupplierPayloadEncryptionOffer>,
    state: Option<SupplierPayloadEncryptionState>,
}

struct SupplierPayloadKeyMaterialLength;

impl ring::hkdf::KeyType for SupplierPayloadKeyMaterialLength {
    fn len(&self) -> usize {
        64
    }
}

fn supplier_payload_encryption_transcript(
    client_public: &[u8],
    server_public: &[u8],
    client_nonce: &[u8],
    server_nonce: &[u8],
    handshake_id: &str,
) -> Vec<u8> {
    let mut transcript = Vec::with_capacity(
        SUPPLIER_PAYLOAD_ENCRYPTION_CONTEXT.len() + 1 + 32 * 4 + handshake_id.len(),
    );
    transcript.extend_from_slice(SUPPLIER_PAYLOAD_ENCRYPTION_CONTEXT);
    transcript.push(0);
    transcript.extend_from_slice(client_public);
    transcript.extend_from_slice(server_public);
    transcript.extend_from_slice(client_nonce);
    transcript.extend_from_slice(server_nonce);
    transcript.extend_from_slice(handshake_id.as_bytes());
    transcript
}

fn derive_supplier_payload_encryption_keys(
    shared_secret: &[u8],
    transcript: &[u8],
) -> Result<([u8; 32], [u8; 32])> {
    let salt_bytes = Sha256::digest(transcript);
    let salt = ring::hkdf::Salt::new(ring::hkdf::HKDF_SHA256, &salt_bytes);
    let prk = salt.extract(shared_secret);
    let info = [SUPPLIER_PAYLOAD_ENCRYPTION_CONTEXT];
    let output = prk
        .expand(&info, SupplierPayloadKeyMaterialLength)
        .map_err(|_| anyhow!("expand supplier payload encryption keys"))?;
    let mut key_material = [0u8; 64];
    output
        .fill(&mut key_material)
        .map_err(|_| anyhow!("fill supplier payload encryption keys"))?;
    let mut client_to_server = [0u8; 32];
    let mut server_to_client = [0u8; 32];
    client_to_server.copy_from_slice(&key_material[..32]);
    server_to_client.copy_from_slice(&key_material[32..]);
    key_material.fill(0);
    Ok((client_to_server, server_to_client))
}

fn supplier_payload_encryption_key(key: [u8; 32]) -> Result<ring::aead::LessSafeKey> {
    let unbound = ring::aead::UnboundKey::new(&ring::aead::AES_256_GCM, &key)
        .map_err(|_| anyhow!("create supplier payload encryption key"))?;
    Ok(ring::aead::LessSafeKey::new(unbound))
}

fn supplier_payload_encryption_nonce(sequence: u64) -> ring::aead::Nonce {
    let mut nonce = [0u8; 12];
    nonce[3] = 1;
    nonce[4..].copy_from_slice(&sequence.to_be_bytes());
    ring::aead::Nonce::assume_unique_for_key(nonce)
}

fn payload_encryption_object<'a>(
    payload: &'a serde_json::Map<String, serde_json::Value>,
) -> Option<&'a serde_json::Map<String, serde_json::Value>> {
    payload
        .get(SUPPLIER_PAYLOAD_ENCRYPTION_OBJECT_KEY)?
        .as_object()
}

fn payload_encryption_base64<const N: usize>(
    payload: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<[u8; N]> {
    let raw = payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("supplier payload encryption {key} is missing"))?;
    let decoded = BASE64
        .decode(raw)
        .with_context(|| format!("decode supplier payload encryption {key}"))?;
    decoded
        .try_into()
        .map_err(|_| anyhow!("supplier payload encryption {key} has invalid length"))
}

fn payload_encryption_base64_vec(
    payload: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Vec<u8>> {
    let raw = payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("supplier payload encryption {key} is missing"))?;
    BASE64
        .decode(raw)
        .with_context(|| format!("decode supplier payload encryption {key}"))
}

fn append_supplier_transport_feature(
    payload: &mut serde_json::Map<String, serde_json::Value>,
    feature: &str,
) {
    let features = payload
        .entry("transport_features".to_string())
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    let Some(features) = features.as_array_mut() else {
        return;
    };
    if !features.iter().any(|value| value.as_str() == Some(feature)) {
        features.push(serde_json::Value::String(feature.to_string()));
    }
}

impl SupplierPayloadEncryptionState {
    fn new(
        send_key: [u8; 32],
        receive_key: [u8; 32],
        handshake_id: String,
        phase: SupplierPayloadEncryptionPhase,
    ) -> Result<Self> {
        Ok(Self {
            send_key: supplier_payload_encryption_key(send_key)?,
            receive_key: supplier_payload_encryption_key(receive_key)?,
            send_sequence: 0,
            receive_sequence: 0,
            handshake_id,
            phase,
        })
    }

    #[cfg(test)]
    fn encode(&mut self, msg: &SupplierMessage, compression_enabled: bool) -> Result<Vec<u8>> {
        self.encode_prepared(SupplierPreparedPayload::new(
            serde_json::to_vec(msg)?,
            compression_enabled,
        )?)
    }

    fn encode_prepared(&mut self, prepared: SupplierPreparedPayload) -> Result<Vec<u8>> {
        let mut content = prepared.content;
        let flags = if prepared.compressed {
            SUPPLIER_PAYLOAD_ENCRYPTION_COMPRESSED_FLAG
        } else {
            0
        };
        if self.send_sequence == u64::MAX {
            return Err(anyhow!(
                "supplier payload encryption send sequence exhausted"
            ));
        }
        let ciphertext_len = content.len() + ring::aead::MAX_TAG_LEN;
        let mut header = vec![0u8; SUPPLIER_PAYLOAD_ENCRYPTION_FRAME_HEADER_BYTES];
        header[..4].copy_from_slice(&SUPPLIER_PAYLOAD_ENCRYPTION_FRAME_MAGIC);
        header[4] = flags;
        header[8..16].copy_from_slice(&self.send_sequence.to_be_bytes());
        header[16..20].copy_from_slice(&(prepared.raw_len as u32).to_be_bytes());
        header[20..24].copy_from_slice(&(ciphertext_len as u32).to_be_bytes());
        self.send_key
            .seal_in_place_append_tag(
                supplier_payload_encryption_nonce(self.send_sequence),
                ring::aead::Aad::from(header.as_slice()),
                &mut content,
            )
            .map_err(|_| anyhow!("encrypt supplier payload frame"))?;
        self.send_sequence += 1;
        header.extend_from_slice(&content);
        Ok(header)
    }

    fn decode(&mut self, frame: &[u8]) -> Result<SupplierMessage> {
        if frame.len() < SUPPLIER_PAYLOAD_ENCRYPTION_FRAME_HEADER_BYTES
            || frame[..4] != SUPPLIER_PAYLOAD_ENCRYPTION_FRAME_MAGIC
        {
            return Err(anyhow!("invalid encrypted supplier frame"));
        }
        let flags = frame[4];
        if flags & !SUPPLIER_PAYLOAD_ENCRYPTION_COMPRESSED_FLAG != 0 || frame[5..8] != [0, 0, 0] {
            return Err(anyhow!("encrypted supplier frame has invalid flags"));
        }
        let sequence = u64::from_be_bytes(frame[8..16].try_into()?);
        if sequence != self.receive_sequence || self.receive_sequence == u64::MAX {
            return Err(anyhow!("encrypted supplier frame sequence is invalid"));
        }
        let raw_len = u32::from_be_bytes(frame[16..20].try_into()?) as usize;
        let ciphertext_len = u32::from_be_bytes(frame[20..24].try_into()?) as usize;
        if raw_len == 0 || raw_len > SUPPLIER_TRANSPORT_COMPRESSION_MAX_RAW_LEN {
            return Err(anyhow!("encrypted supplier frame has invalid raw size"));
        }
        if ciphertext_len <= ring::aead::MAX_TAG_LEN
            || ciphertext_len != frame.len() - SUPPLIER_PAYLOAD_ENCRYPTION_FRAME_HEADER_BYTES
        {
            return Err(anyhow!("encrypted supplier frame has invalid payload size"));
        }
        let mut content = frame[SUPPLIER_PAYLOAD_ENCRYPTION_FRAME_HEADER_BYTES..].to_vec();
        let plaintext = self
            .receive_key
            .open_in_place(
                supplier_payload_encryption_nonce(sequence),
                ring::aead::Aad::from(&frame[..SUPPLIER_PAYLOAD_ENCRYPTION_FRAME_HEADER_BYTES]),
                &mut content,
            )
            .map_err(|_| anyhow!("encrypted supplier frame authentication failed"))?;
        let raw = if flags & SUPPLIER_PAYLOAD_ENCRYPTION_COMPRESSED_FLAG != 0 {
            zstd::bulk::decompress(plaintext, raw_len)?
        } else {
            plaintext.to_vec()
        };
        if raw.len() != raw_len {
            return Err(anyhow!("encrypted supplier frame size mismatch"));
        }
        let message = serde_json::from_slice(&raw)?;
        self.receive_sequence += 1;
        Ok(message)
    }
}

struct SupplierTransportCodec {
    activity: SupplierConnectionActivity,
    frame_compression_enabled: AtomicBool,
    message_chunks_enabled: AtomicBool,
    message_assembler: StdMutex<SupplierMessageAssembler>,
    payload_encryption: StdMutex<SupplierPayloadEncryption>,
}

impl Default for SupplierTransportCodec {
    fn default() -> Self {
        Self::new(None)
    }
}

impl SupplierTransportCodec {
    fn new(expected_identity_sha256: Option<[u8; 32]>) -> Self {
        let offer = expected_identity_sha256
            .and_then(|expected| SupplierPayloadEncryptionOffer::new(expected).ok());
        Self {
            activity: SupplierConnectionActivity::default(),
            frame_compression_enabled: AtomicBool::new(false),
            message_chunks_enabled: AtomicBool::new(false),
            message_assembler: StdMutex::new(SupplierMessageAssembler::default()),
            payload_encryption: StdMutex::new(SupplierPayloadEncryption { offer, state: None }),
        }
    }

    fn inject_payload_encryption_offer(&self, msg: &mut SupplierMessage) {
        if !matches!(
            msg.kind.as_str(),
            "authenticate" | "auth_refresh" | "register"
        ) {
            return;
        }
        let Ok(mut encryption) = self.payload_encryption.lock() else {
            return;
        };
        if encryption.state.is_some() {
            return;
        }
        let Some(offer) = encryption.offer.as_mut() else {
            return;
        };
        append_supplier_transport_feature(&mut msg.payload, SUPPLIER_PAYLOAD_ENCRYPTION_FEATURE);
        msg.payload.insert(
            SUPPLIER_PAYLOAD_ENCRYPTION_OBJECT_KEY.to_string(),
            serde_json::json!({
                "version": SUPPLIER_PAYLOAD_ENCRYPTION_FEATURE,
                "client_public_key": BASE64.encode(offer.public_key),
                "client_nonce": BASE64.encode(offer.nonce),
            }),
        );
        offer.sent = true;
    }

    fn accept_payload_encryption_ack(&self, message: &SupplierMessage) -> Result<()> {
        if !matches!(message.kind.as_str(), "authenticate_ack" | "register_ack") {
            return Ok(());
        }
        let Some(response) = payload_encryption_object(&message.payload) else {
            return Ok(());
        };
        if response.get("version").and_then(serde_json::Value::as_str)
            != Some(SUPPLIER_PAYLOAD_ENCRYPTION_FEATURE)
        {
            return Ok(());
        }
        let mut encryption = self
            .payload_encryption
            .lock()
            .map_err(|_| anyhow!("supplier payload encryption state is poisoned"))?;
        if encryption.state.is_some() {
            return Ok(());
        }
        let offer = encryption
            .offer
            .as_mut()
            .ok_or_else(|| anyhow!("supplier payload encryption was not offered"))?;
        if !offer.sent {
            return Err(anyhow!(
                "supplier payload encryption ack arrived before offer"
            ));
        }
        let server_public = payload_encryption_base64::<32>(response, "server_public_key")?;
        let server_nonce = payload_encryption_base64::<32>(response, "server_nonce")?;
        let certificate = payload_encryption_base64_vec(response, "identity_certificate")?;
        let signature = payload_encryption_base64_vec(response, "signature")?;
        let handshake_id = response
            .get("handshake_id")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 128)
            .ok_or_else(|| anyhow!("supplier payload encryption handshake id is invalid"))?
            .to_string();
        let identity_hash = Sha256::digest(&certificate);
        if identity_hash[..] != offer.expected_identity_sha256[..] {
            return Err(anyhow!("supplier payload encryption identity pin mismatch"));
        }
        use x509_parser::prelude::FromDer as _;
        let (_, certificate) = x509_parser::certificate::X509Certificate::from_der(&certificate)
            .map_err(|_| anyhow!("parse supplier payload encryption identity certificate"))?;
        let transcript = supplier_payload_encryption_transcript(
            &offer.public_key,
            &server_public,
            &offer.nonce,
            &server_nonce,
            &handshake_id,
        );
        ring::signature::UnparsedPublicKey::new(
            &ring::signature::ECDSA_P256_SHA256_ASN1,
            certificate.public_key().subject_public_key.data.as_ref(),
        )
        .verify(&transcript, &signature)
        .map_err(|_| anyhow!("supplier payload encryption handshake signature mismatch"))?;
        let private_key = offer
            .private_key
            .take()
            .ok_or_else(|| anyhow!("supplier payload encryption offer was already consumed"))?;
        let peer_public =
            ring::agreement::UnparsedPublicKey::new(&ring::agreement::X25519, server_public);
        let keys = ring::agreement::agree_ephemeral(private_key, &peer_public, |shared_secret| {
            derive_supplier_payload_encryption_keys(shared_secret, &transcript)
        })
        .map_err(|_| anyhow!("derive supplier payload encryption shared secret"))??;
        encryption.state = Some(SupplierPayloadEncryptionState::new(
            keys.0,
            keys.1,
            handshake_id,
            SupplierPayloadEncryptionPhase::ConfirmReady,
        )?);
        Ok(())
    }

    fn take_payload_encryption_confirmation(&self) -> Option<SupplierMessage> {
        let mut encryption = self.payload_encryption.lock().ok()?;
        let state = encryption.state.as_mut()?;
        if state.phase != SupplierPayloadEncryptionPhase::ConfirmReady {
            return None;
        }
        state.phase = SupplierPayloadEncryptionPhase::ConfirmSent;
        Some(SupplierMessage {
            id: format!("payload-encryption-{}", now_unix()),
            kind: SUPPLIER_PAYLOAD_ENCRYPTION_CONFIRM.to_string(),
            payload: serde_json::json!({"handshake_id": state.handshake_id})
                .as_object()
                .cloned()
                .unwrap_or_default(),
        })
    }

    fn payload_encryption_active(&self) -> bool {
        self.payload_encryption
            .lock()
            .ok()
            .and_then(|state| state.state.as_ref().map(|state| state.phase))
            == Some(SupplierPayloadEncryptionPhase::Active)
    }

    fn prepare_message(&self, mut msg: SupplierMessage) -> Result<SupplierMessageParts> {
        self.inject_payload_encryption_offer(&mut msg);
        SupplierMessageParts::new(
            serde_json::to_vec(&msg)?,
            self.message_chunks_enabled.load(Ordering::Relaxed),
        )
    }

    fn encode(&self, mut msg: SupplierMessage) -> Result<(Vec<u8>, bool)> {
        self.inject_payload_encryption_offer(&mut msg);
        self.encode_raw(serde_json::to_vec(&msg)?)
    }

    fn encode_raw(&self, raw: Vec<u8>) -> Result<(Vec<u8>, bool)> {
        let prepared = SupplierPreparedPayload::new(
            raw,
            self.frame_compression_enabled.load(Ordering::Relaxed),
        )?;
        {
            let mut encryption = self
                .payload_encryption
                .lock()
                .map_err(|_| anyhow!("supplier payload encryption state is poisoned"))?;
            if let Some(state) = encryption.state.as_mut() {
                if matches!(
                    state.phase,
                    SupplierPayloadEncryptionPhase::ConfirmSent
                        | SupplierPayloadEncryptionPhase::Active
                ) {
                    return Ok((state.encode_prepared(prepared)?, true));
                }
            }
        }
        Ok(prepared.plaintext())
    }

    fn normalize_message(&self, message: SupplierMessage) -> Result<String> {
        if supplier_transport_string_array(&message.payload, "transport_features")
            .iter()
            .any(|feature| feature == SUPPLIER_MESSAGE_CHUNKS_FEATURE)
        {
            self.message_chunks_enabled.store(true, Ordering::Relaxed);
        }
        if supplier_payload_accepts_frame_compression(&message.payload) {
            self.frame_compression_enabled
                .store(true, Ordering::Relaxed);
        }
        // Invalid or stale crypto data is a downgrade to the original protocol,
        // never a reason to take a working supplier connection offline.
        let _ = self.accept_payload_encryption_ack(&message);
        Ok(serde_json::to_string(&message)?)
    }

    fn decode_binary_frame(&self, frame: &[u8]) -> Result<SupplierMessage> {
        if frame.starts_with(&SUPPLIER_PAYLOAD_ENCRYPTION_FRAME_MAGIC) {
            let mut encryption = self
                .payload_encryption
                .lock()
                .map_err(|_| anyhow!("supplier payload encryption state is poisoned"))?;
            let state = encryption
                .state
                .as_mut()
                .ok_or_else(|| anyhow!("encrypted supplier frame arrived before negotiation"))?;
            if !matches!(
                state.phase,
                SupplierPayloadEncryptionPhase::ConfirmSent
                    | SupplierPayloadEncryptionPhase::Active
            ) {
                return Err(anyhow!(
                    "encrypted supplier frame arrived before confirmation"
                ));
            }
            let message = state.decode(frame)?;
            state.phase = SupplierPayloadEncryptionPhase::Active;
            return Ok(message);
        }
        if self.payload_encryption_active() {
            return Err(anyhow!(
                "plaintext supplier frame arrived after payload encryption activation"
            ));
        }
        decode_supplier_transport_frame(frame)
    }

    fn decode_websocket(&self, message: WsMessage) -> Result<Option<String>> {
        let message = match message {
            WsMessage::Text(text) => {
                if self.payload_encryption_active() {
                    return Err(anyhow!(
                        "plaintext supplier message arrived after payload encryption activation"
                    ));
                }
                serde_json::from_str(&text)?
            }
            WsMessage::Binary(frame) => self.decode_binary_frame(&frame)?,
            _ => return Ok(None),
        };
        let business = !matches!(message.kind.as_str(), "ping" | "pong");
        let message = self
            .message_assembler
            .lock()
            .map_err(|_| anyhow!("supplier message assembler is poisoned"))?
            .accept(message, self.message_chunks_enabled.load(Ordering::Relaxed))?;
        self.activity.received(business);
        message
            .map(|message| self.normalize_message(message))
            .transpose()
    }

    async fn read_quic<R>(&self, reader: &mut R) -> Result<Option<String>>
    where
        R: tokio::io::AsyncBufRead + Unpin,
    {
        loop {
            let first = match reader.read_u8().await {
                Ok(first) => first,
                Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                    self.message_assembler
                        .lock()
                        .map_err(|_| anyhow!("supplier message assembler is poisoned"))?
                        .finish()?;
                    return Ok(None);
                }
                Err(error) => return Err(error.into()),
            };
            let message = if first != SUPPLIER_TRANSPORT_FRAME_MAGIC[0] {
                if self.payload_encryption_active() {
                    return Err(anyhow!(
                        "plaintext supplier message arrived after payload encryption activation"
                    ));
                }
                let mut wire = vec![first];
                (&mut *reader)
                    .take(SUPPLIER_TRANSPORT_COMPRESSION_MAX_RAW_LEN as u64 + 1)
                    .read_until(b'\n', &mut wire)
                    .await?;
                if wire.len() > SUPPLIER_TRANSPORT_COMPRESSION_MAX_RAW_LEN + 1 {
                    return Err(anyhow!("supplier JSON frame exceeds max size"));
                }
                serde_json::from_slice(&wire)?
            } else {
                let mut magic = [0u8; 4];
                magic[0] = first;
                reader.read_exact(&mut magic[1..]).await?;
                if magic == SUPPLIER_PAYLOAD_ENCRYPTION_FRAME_MAGIC {
                    let mut header = [0u8; SUPPLIER_PAYLOAD_ENCRYPTION_FRAME_HEADER_BYTES];
                    header[..4].copy_from_slice(&magic);
                    reader.read_exact(&mut header[4..]).await?;
                    let ciphertext_len = u32::from_be_bytes(header[20..24].try_into()?) as usize;
                    if ciphertext_len == 0
                        || ciphertext_len > SUPPLIER_TRANSPORT_COMPRESSION_MAX_RAW_LEN + 64
                    {
                        return Err(anyhow!("encrypted supplier frame has invalid payload size"));
                    }
                    let mut frame = Vec::with_capacity(
                        SUPPLIER_PAYLOAD_ENCRYPTION_FRAME_HEADER_BYTES + ciphertext_len,
                    );
                    frame.extend_from_slice(&header);
                    frame.resize(
                        SUPPLIER_PAYLOAD_ENCRYPTION_FRAME_HEADER_BYTES + ciphertext_len,
                        0,
                    );
                    reader
                        .read_exact(&mut frame[SUPPLIER_PAYLOAD_ENCRYPTION_FRAME_HEADER_BYTES..])
                        .await?;
                    self.decode_binary_frame(&frame)?
                } else {
                    if magic != SUPPLIER_TRANSPORT_FRAME_MAGIC {
                        return Err(anyhow!("invalid binary supplier frame magic"));
                    }
                    if self.payload_encryption_active() {
                        return Err(anyhow!(
                            "plaintext supplier frame arrived after payload encryption activation"
                        ));
                    }
                    let mut frame_header = [0u8; SUPPLIER_TRANSPORT_FRAME_HEADER_BYTES];
                    frame_header[..4].copy_from_slice(&magic);
                    reader.read_exact(&mut frame_header[4..]).await?;
                    let compressed_len = u32::from_be_bytes([
                        frame_header[8],
                        frame_header[9],
                        frame_header[10],
                        frame_header[11],
                    ]) as usize;
                    if compressed_len == 0
                        || compressed_len > SUPPLIER_TRANSPORT_COMPRESSION_MAX_RAW_LEN
                    {
                        return Err(anyhow!(
                            "compressed supplier frame has invalid payload size"
                        ));
                    }
                    let mut frame =
                        Vec::with_capacity(SUPPLIER_TRANSPORT_FRAME_HEADER_BYTES + compressed_len);
                    frame.extend_from_slice(&frame_header);
                    frame.resize(SUPPLIER_TRANSPORT_FRAME_HEADER_BYTES + compressed_len, 0);
                    reader
                        .read_exact(&mut frame[SUPPLIER_TRANSPORT_FRAME_HEADER_BYTES..])
                        .await?;
                    self.decode_binary_frame(&frame)?
                }
            };
            let business = !matches!(message.kind.as_str(), "ping" | "pong");
            let message = self
                .message_assembler
                .lock()
                .map_err(|_| anyhow!("supplier message assembler is poisoned"))?
                .accept(message, self.message_chunks_enabled.load(Ordering::Relaxed))?;
            self.activity.received(business);
            if let Some(message) = message {
                return Ok(Some(self.normalize_message(message)?));
            }
        }
    }
}

fn supplier_payload_encryption_identity_pin(config: &SupplierAgentConfig) -> Option<[u8; 32]> {
    parse_supplier_quic_url(&config.server_quic_url)
        .ok()
        .and_then(|target| target.cert_sha256)
}

#[cfg(test)]
mod payload_encryption_tests {
    use super::*;
    use rcgen::SigningKey as _;

    fn message(kind: &str, body: &str) -> SupplierMessage {
        SupplierMessage {
            id: format!("{kind}-id"),
            kind: kind.to_string(),
            payload: serde_json::json!({"body": body})
                .as_object()
                .cloned()
                .unwrap_or_default(),
        }
    }

    #[test]
    fn supplier_payload_encryption_key_derivation_matches_protocol_vector() {
        let transcript = supplier_payload_encryption_transcript(
            &[1; 32],
            &[2; 32],
            &[3; 32],
            &[4; 32],
            "test-handshake",
        );
        let (client_to_server, server_to_client) =
            derive_supplier_payload_encryption_keys(&[5; 32], &transcript)
                .expect("derive test keys");
        let mut material = Vec::with_capacity(64);
        material.extend_from_slice(&client_to_server);
        material.extend_from_slice(&server_to_client);

        assert_eq!(
            hex::encode(material),
            "411f3f2e2693bb988980d9e1f4067db7d8aed28ed03f70e7f90e9da0437bfdba10010f687dad528115b94c29d7b9ea19b4731eb56add7ac386bfa9135ceeb2e2"
        );

        let mut state = SupplierPayloadEncryptionState::new(
            [1; 32],
            [0; 32],
            "vector".to_string(),
            SupplierPayloadEncryptionPhase::Active,
        )
        .expect("create vector state");
        let frame = state
            .encode(
                &SupplierMessage {
                    id: "vector".to_string(),
                    kind: "ping".to_string(),
                    payload: serde_json::json!({"value": "hello"})
                        .as_object()
                        .cloned()
                        .unwrap_or_default(),
                },
                false,
            )
            .expect("encode vector frame");
        assert_eq!(
            hex::encode(frame),
            "0043453100000000000000000000000000000039000000498786507b9e73c31a721780e0653664cdb98e7f8f9d71904548c0af1e5c953106325f23bc2f100e46fefcd622c41dd03d421771b9507a421a8ddbce2daf1fea6fd24083571b67a1094e"
        );
    }

    #[test]
    fn supplier_payload_encryption_handshake_frames_and_tamper_detection() {
        let identity = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
            .expect("self-signed identity");
        let identity_der = identity.cert.der().as_ref();
        let identity_pin: [u8; 32] = Sha256::digest(identity_der).into();
        let codec = SupplierTransportCodec::new(Some(identity_pin));

        let authenticate = SupplierMessage {
            id: "auth-id".to_string(),
            kind: "authenticate".to_string(),
            payload: serde_json::Map::new(),
        };
        let (register_wire, binary) = codec
            .encode(authenticate)
            .expect("encode authentication offer");
        assert!(!binary);
        let offered: SupplierMessage =
            serde_json::from_slice(&register_wire).expect("decode authentication offer");
        let offer = payload_encryption_object(&offered.payload).expect("payload encryption offer");
        assert_eq!(
            offer.get("version").and_then(serde_json::Value::as_str),
            Some(SUPPLIER_PAYLOAD_ENCRYPTION_FEATURE)
        );
        let client_public =
            payload_encryption_base64::<32>(offer, "client_public_key").expect("client public");
        let client_nonce =
            payload_encryption_base64::<32>(offer, "client_nonce").expect("client nonce");

        let rng = ring::rand::SystemRandom::new();
        let server_private =
            ring::agreement::EphemeralPrivateKey::generate(&ring::agreement::X25519, &rng)
                .expect("server private key");
        let server_public: [u8; 32] = server_private
            .compute_public_key()
            .expect("server public key")
            .as_ref()
            .try_into()
            .expect("server public key length");
        let server_nonce = [11u8; 32];
        let handshake_id = "test-handshake-id";
        let transcript = supplier_payload_encryption_transcript(
            &client_public,
            &server_public,
            &client_nonce,
            &server_nonce,
            handshake_id,
        );
        let signature = identity
            .signing_key
            .sign(&transcript)
            .expect("sign handshake");
        let peer_public =
            ring::agreement::UnparsedPublicKey::new(&ring::agreement::X25519, client_public);
        let (client_to_server, server_to_client) =
            ring::agreement::agree_ephemeral(server_private, &peer_public, |shared_secret| {
                derive_supplier_payload_encryption_keys(shared_secret, &transcript)
            })
            .expect("derive shared secret")
            .expect("derive protocol keys");

        let ack = SupplierMessage {
            id: "auth-id".to_string(),
            kind: "authenticate_ack".to_string(),
            payload: serde_json::json!({
                SUPPLIER_PAYLOAD_ENCRYPTION_OBJECT_KEY: {
                    "version": SUPPLIER_PAYLOAD_ENCRYPTION_FEATURE,
                    "handshake_id": handshake_id,
                    "server_public_key": BASE64.encode(server_public),
                    "server_nonce": BASE64.encode(server_nonce),
                    "identity_certificate": BASE64.encode(identity_der),
                    "signature": BASE64.encode(signature),
                }
            })
            .as_object()
            .cloned()
            .unwrap_or_default(),
        };
        codec
            .accept_payload_encryption_ack(&ack)
            .expect("accept signed handshake");
        assert!(!codec.payload_encryption_active());

        let confirmation = codec
            .take_payload_encryption_confirmation()
            .expect("client confirmation");
        let (confirmation_frame, binary) =
            codec.encode(confirmation).expect("encrypt confirmation");
        assert!(binary);
        assert!(confirmation_frame.starts_with(&SUPPLIER_PAYLOAD_ENCRYPTION_FRAME_MAGIC));

        let mut server_state = SupplierPayloadEncryptionState::new(
            server_to_client,
            client_to_server,
            handshake_id.to_string(),
            SupplierPayloadEncryptionPhase::Active,
        )
        .expect("server encryption state");
        let decoded_confirmation = server_state
            .decode(&confirmation_frame)
            .expect("decrypt confirmation");
        assert_eq!(
            decoded_confirmation.kind,
            SUPPLIER_PAYLOAD_ENCRYPTION_CONFIRM
        );
        assert_eq!(
            decoded_confirmation
                .payload
                .get("handshake_id")
                .and_then(serde_json::Value::as_str),
            Some(handshake_id)
        );

        let response = message("http_request", "encrypted server request");
        let response_frame = server_state
            .encode(&response, false)
            .expect("encrypt server request");
        let decoded_response = codec
            .decode_binary_frame(&response_frame)
            .expect("decrypt server request");
        assert_eq!(decoded_response.payload, response.payload);
        assert!(codec.payload_encryption_active());
        assert!(codec
            .decode_websocket(WsMessage::Text(
                serde_json::to_string(&message("http_request", "plaintext"))
                    .expect("encode plaintext")
                    .into(),
            ))
            .is_err());
        let (plaintext_frame, compressed) = encode_supplier_transport_frame(&message(
            "http_request",
            &"plaintext compressed frame ".repeat(512),
        ))
        .expect("encode plaintext binary frame");
        assert!(compressed);
        assert!(codec.decode_binary_frame(&plaintext_frame).is_err());

        codec
            .frame_compression_enabled
            .store(true, Ordering::Relaxed);
        let request = message("http_response", &"compress before encryption ".repeat(512));
        let (request_frame, binary) = codec.encode(request.clone()).expect("encrypt response");
        assert!(binary);
        assert_ne!(
            request_frame[4] & SUPPLIER_PAYLOAD_ENCRYPTION_COMPRESSED_FLAG,
            0
        );
        let mut tampered = request_frame.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(server_state.decode(&tampered).is_err());
        let decoded_request = server_state
            .decode(&request_frame)
            .expect("decrypt compressed response");
        assert_eq!(decoded_request.payload, request.payload);
        assert!(server_state.decode(&request_frame).is_err());
    }

    #[test]
    fn supplier_payload_encryption_preserves_legacy_protocol() {
        let legacy_codec = SupplierTransportCodec::default();
        let register = SupplierMessage {
            id: "legacy-register".to_string(),
            kind: "register".to_string(),
            payload: serde_json::Map::new(),
        };
        let (wire, binary) = legacy_codec
            .encode(register)
            .expect("encode legacy register");
        assert!(!binary);
        let decoded: SupplierMessage =
            serde_json::from_slice(&wire).expect("decode legacy register");
        assert!(!decoded
            .payload
            .contains_key(SUPPLIER_PAYLOAD_ENCRYPTION_OBJECT_KEY));

        let offered_codec = SupplierTransportCodec::new(Some([9; 32]));
        let authenticate = SupplierMessage {
            id: "offered-auth".to_string(),
            kind: "authenticate".to_string(),
            payload: serde_json::Map::new(),
        };
        let (_, binary) = offered_codec
            .encode(authenticate)
            .expect("encode offered authentication");
        assert!(!binary);
        let legacy_ack = SupplierMessage {
            id: "offered-auth".to_string(),
            kind: "authenticate_ack".to_string(),
            payload: serde_json::Map::new(),
        };
        offered_codec
            .normalize_message(legacy_ack)
            .expect("accept legacy ack");
        assert!(offered_codec
            .take_payload_encryption_confirmation()
            .is_none());
        let (wire, binary) = offered_codec
            .encode(message("http_response", "legacy response"))
            .expect("keep legacy framing");
        assert!(!binary);
        assert_eq!(
            serde_json::from_slice::<SupplierMessage>(&wire)
                .expect("decode legacy response")
                .payload
                .get("body")
                .and_then(serde_json::Value::as_str),
            Some("legacy response")
        );
    }
}
