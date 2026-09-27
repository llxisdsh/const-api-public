const PLATFORM_DUPLEX_FEATURE: &str = "platform_duplex_v1";
const PLATFORM_DUPLEX_RESUME_FEATURE: &str = "platform_duplex_resume_v1";
const PLATFORM_DUPLEX_OPEN_TIMEOUT: Duration = Duration::from_secs(20);
const PLATFORM_DUPLEX_WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const PLATFORM_WIRE_CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
const PLATFORM_DUPLEX_RESUME_WINDOW: Duration = Duration::from_secs(120);
const PLATFORM_DUPLEX_MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
const PLATFORM_DUPLEX_TURN_ID_FIELD: &str = "_const_duplex_turn_id";
const PLATFORM_DUPLEX_TURN_SEQUENCE_FIELD: &str = "_const_duplex_turn_seq";
const PLATFORM_DUPLEX_SEQUENCE_FIELD: &str = "_const_platform_seq";

pub(crate) fn native_realtime_no_replay(message: &SupplierMessage) -> bool {
    message.kind.starts_with("duplex_")
        && message
            .payload
            .get("_const_realtime_no_replay")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
}

#[derive(Debug)]
pub(crate) struct PlatformDuplexOpenError {
    message: String,
    safe_to_replay: bool,
}

impl std::fmt::Display for PlatformDuplexOpenError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PlatformDuplexOpenError {}

pub(crate) fn platform_duplex_error_is_safe_to_replay(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<PlatformDuplexOpenError>()
        .is_some_and(|error| error.safe_to_replay)
}

fn safe_platform_duplex_open_error(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(PlatformDuplexOpenError {
        message: message.into(),
        safe_to_replay: true,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PlatformDuplexFrame {
    Text(String),
    Binary(Vec<u8>),
    Close { code: u16, reason: String },
}

struct PlatformDuplexCommand {
    frame: PlatformDuplexFrame,
    completed: oneshot::Sender<std::result::Result<(), String>>,
}

pub(crate) struct PlatformDuplexSender {
    outbound: mpsc::Sender<PlatformDuplexCommand>,
}

impl PlatformDuplexSender {
    pub(crate) async fn send(&self, frame: PlatformDuplexFrame) -> Result<()> {
        let (completed, result) = oneshot::channel();
        self.outbound
            .send(PlatformDuplexCommand { frame, completed })
            .await
            .map_err(|_| anyhow!("platform duplex transport is closed"))?;
        result
            .await
            .map_err(|_| anyhow!("platform duplex transport stopped before the frame was written"))?
            .map_err(anyhow::Error::msg)
    }
}

pub(crate) struct PlatformDuplexReceiver {
    inbound: mpsc::Receiver<Result<PlatformDuplexFrame, String>>,
}

impl PlatformDuplexReceiver {
    pub(crate) async fn next(&mut self) -> Option<Result<PlatformDuplexFrame>> {
        self.inbound
            .recv()
            .await
            .map(|frame| frame.map_err(anyhow::Error::msg))
    }
}

pub(crate) struct PlatformDuplexSession {
    sender: PlatformDuplexSender,
    receiver: PlatformDuplexReceiver,
}

impl PlatformDuplexSession {
    pub(crate) fn split(self) -> (PlatformDuplexSender, PlatformDuplexReceiver) {
        (self.sender, self.receiver)
    }
}

#[derive(Clone, Default)]
pub(crate) struct PlatformDuplexPool {
    connections: Arc<tokio::sync::Mutex<HashMap<String, Arc<PlatformPhysicalConnection>>>>,
    connect_locks: Arc<
        tokio::sync::Mutex<
            HashMap<String, Arc<tokio::sync::Mutex<()>>>,
        >,
    >,
}

struct PlatformPhysicalConnection {
    commands: mpsc::Sender<PlatformPhysicalCommand>,
    transport: String,
    closed: Arc<AtomicBool>,
    authenticated_token: Arc<StdMutex<String>>,
    resume_supported: bool,
    output_budget: crate::output_buffer::OutputBudget,
}

enum PlatformPhysicalCommand {
    Open {
        message: SupplierMessage,
        events: crate::output_buffer::OutputSender<std::result::Result<SupplierMessage, String>>,
        completed: oneshot::Sender<std::result::Result<(), String>>,
    },
    Send {
        message: SupplierMessage,
        completed: oneshot::Sender<std::result::Result<(), String>>,
    },
    Close {
        session_id: String,
    },
}

struct PlatformWireCommand {
    message: SupplierMessage,
    completed: oneshot::Sender<std::result::Result<(), String>>,
}

struct PlatformWireConnection {
    codec: Arc<SupplierTransportCodec>,
    outbound: mpsc::Sender<PlatformWireCommand>,
    inbound: mpsc::Receiver<std::result::Result<SupplierMessage, String>>,
    writer: tokio::task::JoinHandle<()>,
    reader: tokio::task::JoinHandle<()>,
    _quic_endpoint: Option<quinn::Endpoint>,
    resume_supported: bool,
}

impl PlatformWireConnection {
    async fn send(&self, message: SupplierMessage) -> Result<()> {
        let kind = message.kind.clone();
        tokio::time::timeout(PLATFORM_DUPLEX_WRITE_TIMEOUT, async {
            let (completed, result) = oneshot::channel();
            self.outbound
                .send(PlatformWireCommand { message, completed })
                .await
                .map_err(|_| anyhow!("platform long connection is closed"))?;
            result
                .await
                .map_err(|_| anyhow!("platform long-connection writer stopped"))?
                .map_err(anyhow::Error::msg)
        })
        .await
        .map_err(|_| anyhow!("platform long-connection write timed out"))??;
        self.codec.activity.written(&kind);
        Ok(())
    }

    async fn stop(self) {
        let PlatformWireConnection {
            codec: _,
            outbound,
            inbound,
            mut writer,
            mut reader,
            _quic_endpoint,
            resume_supported: _,
        } = self;
        drop(inbound);
        drop(outbound);

        // Give WebSocket a chance to flush its close frame and QUIC a chance to
        // finish its send stream. A broken peer must not keep the shared
        // physical-connection task alive forever, so both halves remain bounded.
        if tokio::time::timeout(PLATFORM_WIRE_CLOSE_TIMEOUT, &mut writer)
            .await
            .is_err()
        {
            writer.abort();
            let _ = writer.await;
        }
        if tokio::time::timeout(PLATFORM_WIRE_CLOSE_TIMEOUT, &mut reader)
            .await
            .is_err()
        {
            reader.abort();
            let _ = reader.await;
        }
    }
}

fn platform_auth_message(token: &str, kind: &str) -> SupplierMessage {
    SupplierMessage {
        id: format!("platform-auth-{:032x}", rand::random::<u128>()),
        kind: kind.to_string(),
        payload: serde_json::json!({
            "access_token": token,
            "device_id": machine_client_id(),
            "client_id": machine_client_id(),
            "transport_features": [
                SUPPLIER_TRANSPORT_FRAME_COMPRESSION_FEATURE,
                PLATFORM_DUPLEX_FEATURE,
                PLATFORM_DUPLEX_RESUME_FEATURE
            ],
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    }
}

fn platform_auth_ack(message: &SupplierMessage) -> Result<bool> {
    if message.kind != "authenticate_ack" {
        return Err(anyhow!("unexpected platform authentication response"));
    }
    if !message
        .payload
        .get("ok")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return Err(anyhow!("platform authentication was rejected"));
    }
    if !supplier_transport_string_array(&message.payload, "transport_features")
        .iter()
        .any(|feature| feature == PLATFORM_DUPLEX_FEATURE)
    {
        return Err(safe_platform_duplex_open_error(
            "the selected platform server does not support long-connection duplex sessions",
        ));
    }
    Ok(supplier_transport_string_array(&message.payload, "transport_features")
        .iter()
        .any(|feature| feature == PLATFORM_DUPLEX_RESUME_FEATURE))
}

fn platform_request_header_payload(
    headers: &reqwest::header::HeaderMap,
) -> serde_json::Map<String, serde_json::Value> {
    let mut values = serde_json::Map::new();
    for name in headers.keys() {
        let items = headers
            .get_all(name)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .map(|value| serde_json::Value::String(value.to_string()))
            .collect::<Vec<_>>();
        if !items.is_empty() {
            values.insert(name.as_str().to_string(), serde_json::Value::Array(items));
        }
    }
    values
}

fn platform_duplex_open_message(
    session_id: &str,
    headers: &reqwest::header::HeaderMap,
) -> SupplierMessage {
    SupplierMessage {
        id: session_id.to_string(),
        kind: "platform_duplex_open".to_string(),
        payload: serde_json::json!({
            "session_kind": "openai_responses",
            "request": {
                "method": "GET",
                "path": "/v1/responses",
                "raw_query": "",
                "headers": platform_request_header_payload(headers),
            }
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    }
}

fn platform_duplex_resume_message(
    session_id: &str,
    turn_id: Option<&str>,
    after_turn_sequence: u64,
    after_platform_sequence: u64,
) -> SupplierMessage {
    let mut payload = serde_json::json!({
        "after_turn_sequence": after_turn_sequence,
        "after_platform_sequence": after_platform_sequence,
    })
    .as_object()
    .cloned()
    .unwrap_or_default();
    if let Some(turn_id) = turn_id.map(str::trim).filter(|value| !value.is_empty()) {
        payload.insert(
            "turn_id".to_string(),
            serde_json::Value::String(turn_id.to_string()),
        );
    }
    SupplierMessage {
        id: session_id.to_string(),
        kind: "platform_duplex_resume".to_string(),
        payload,
    }
}

fn platform_duplex_ack_message(session_id: &str, sequence: u64) -> SupplierMessage {
    SupplierMessage {
        id: session_id.to_string(),
        kind: "platform_duplex_ack".to_string(),
        payload: serde_json::json!({"sequence": sequence})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    }
}

fn platform_duplex_frame_turn_id(frame: &PlatformDuplexFrame) -> Option<String> {
    let PlatformDuplexFrame::Text(text) = frame else {
        return None;
    };
    let value = serde_json::from_str::<serde_json::Value>(text).ok()?;
    if value.get("type").and_then(serde_json::Value::as_str) != Some("response.create") {
        return None;
    }
    if value.get("generate").and_then(serde_json::Value::as_bool) == Some(false) {
        return None;
    }
    Some(format!("duplex-turn-{:032x}", rand::random::<u128>()))
}

pub(crate) fn platform_frame_payload(
    frame: PlatformDuplexFrame,
) -> serde_json::Map<String, serde_json::Value> {
    match frame {
        PlatformDuplexFrame::Text(data) => serde_json::json!({
            "frame_type": "text",
            "data": data,
        }),
        PlatformDuplexFrame::Binary(data) => serde_json::json!({
            "frame_type": "binary",
            "encoding": "base64",
            "data": BASE64.encode(data),
        }),
        PlatformDuplexFrame::Close { code, reason } => serde_json::json!({
            "frame_type": "close",
            "code": code,
            "reason": reason,
        }),
    }
    .as_object()
    .cloned()
    .unwrap_or_default()
}

pub(crate) fn platform_server_frame(
    payload: &serde_json::Map<String, serde_json::Value>,
) -> Result<PlatformDuplexFrame> {
    let frame_type = payload_string(payload, "frame_type");
    let data = payload_string(payload, "data");
    match frame_type.as_str() {
        "text" => Ok(PlatformDuplexFrame::Text(data)),
        "binary" if payload_string(payload, "encoding") == "base64" => {
            Ok(PlatformDuplexFrame::Binary(BASE64.decode(data)?))
        }
        "close" => Ok(PlatformDuplexFrame::Close {
            code: payload
                .get("code")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(1000) as u16,
            reason: payload_string(payload, "reason"),
        }),
        _ => Err(anyhow!("unsupported platform duplex server frame")),
    }
}

fn platform_error_frame(payload: &serde_json::Map<String, serde_json::Value>) -> PlatformDuplexFrame {
    let mut event = serde_json::Map::new();
    event.insert("type".to_string(), serde_json::Value::String("error".to_string()));
    if let Some(value) = payload.get("safe_to_retry_http_bridge") {
        event.insert("safe_to_retry_http_bridge".to_string(), value.clone());
    }
    event.insert(
        "error".to_string(),
        payload.get("error").cloned().unwrap_or_else(|| {
            serde_json::json!({
                "type": "const_api_error",
                "code": "platform_duplex_error",
                "message": payload_string(payload, "message"),
            })
        }),
    );
    PlatformDuplexFrame::Text(serde_json::Value::Object(event).to_string())
}

async fn platform_ws_send(
    websocket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    codec: &SupplierTransportCodec,
    message: SupplierMessage,
) -> Result<()> {
    let (wire, compressed) = codec.encode(message)?;
    if compressed {
        websocket.send(WsMessage::Binary(wire.into())).await?;
    } else {
        websocket
            .send(WsMessage::Text(String::from_utf8(wire)?.into()))
            .await?;
    }
    Ok(())
}

async fn platform_ws_read(
    websocket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    codec: &SupplierTransportCodec,
) -> Result<SupplierMessage> {
    loop {
        let message = websocket
            .next()
            .await
            .ok_or_else(|| anyhow!("platform WebSocket closed"))??;
        if let WsMessage::Ping(body) = &message {
            websocket.send(WsMessage::Pong(body.clone())).await?;
            continue;
        }
        if message.is_close() {
            return Err(anyhow!("platform WebSocket closed during handshake"));
        }
        let Some(text) = codec.decode_websocket(message)? else {
            continue;
        };
        return Ok(serde_json::from_str(&text)?);
    }
}

async fn connect_platform_websocket_wire(
    ws_url: &str,
    identity_pin: Option<[u8; 32]>,
    token: &str,
) -> Result<PlatformWireConnection> {
    let websocket_config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(PLATFORM_DUPLEX_MAX_MESSAGE_BYTES))
        .max_frame_size(Some(PLATFORM_DUPLEX_MAX_MESSAGE_BYTES));
    let (mut websocket, _) = tokio::time::timeout(
        supplier_connect_timeout(),
        tokio_tungstenite::connect_async_with_config(ws_url, Some(websocket_config), false),
    )
    .await
    .map_err(|_| anyhow!("platform WebSocket connect timed out"))??;
    let codec = Arc::new(SupplierTransportCodec::new(identity_pin));
    platform_ws_send(&mut websocket, &codec, platform_auth_message(token, "authenticate")).await?;
    let auth = tokio::time::timeout(PLATFORM_DUPLEX_OPEN_TIMEOUT, platform_ws_read(&mut websocket, &codec))
        .await
        .map_err(|_| anyhow!("platform WebSocket authentication timed out"))??;
    let resume_supported = platform_auth_ack(&auth)?;
    if let Some(confirmation) = codec.take_payload_encryption_confirmation() {
        platform_ws_send(&mut websocket, &codec, confirmation).await?;
    }

    let (mut writer, mut reader) = websocket.split();
    let (outbound, mut outbound_rx) = mpsc::channel::<PlatformWireCommand>(32);
    let (inbound_tx, inbound) = mpsc::channel(32);
    let writer_codec = codec.clone();
    let writer_errors = inbound_tx.clone();
    let writer_task = crate::spawn_logged("platform websocket writer", async move {
        while let Some(command) = outbound_rx.recv().await {
            let result = (|| writer_codec.encode(command.message))()
                .and_then(|(wire, compressed)| {
                    if compressed {
                        Ok(WsMessage::Binary(wire.into()))
                    } else {
                        Ok(WsMessage::Text(String::from_utf8(wire)?.into()))
                    }
                });
            let result = match result {
                Ok(message) => writer.send(message).await.map_err(anyhow::Error::from),
                Err(error) => Err(error),
            };
            let detail = result.as_ref().err().map(|error| format!("{error:#}"));
            let _ = command.completed.send(result.map(|_| ()).map_err(|error| format!("{error:#}")));
            if let Some(detail) = detail {
                let _ = writer_errors.send(Err(detail)).await;
                return;
            }
        }
        let _ = writer.close().await;
    });
    let reader_codec = codec.clone();
    let reader_task = crate::spawn_logged("platform websocket reader", async move {
        while let Some(message) = reader.next().await {
            let message = match message {
                Ok(message) => message,
                Err(error) => {
                    let _ = inbound_tx.send(Err(format!("{error:#}"))).await;
                    return;
                }
            };
            if message.is_close() {
                let _ = inbound_tx.send(Err("platform WebSocket closed".to_string())).await;
                return;
            }
            let text = match reader_codec.decode_websocket(message) {
                Ok(Some(text)) => text,
                Ok(None) => continue,
                Err(error) => {
                    let _ = inbound_tx.send(Err(format!("{error:#}"))).await;
                    return;
                }
            };
            match serde_json::from_str::<SupplierMessage>(&text) {
                Ok(message) => {
                    if inbound_tx.send(Ok(message)).await.is_err() {
                        return;
                    }
                }
                Err(error) => {
                    let _ = inbound_tx.send(Err(format!("{error:#}"))).await;
                    return;
                }
            }
        }
    });
    Ok(PlatformWireConnection {
        codec,
        outbound,
        inbound,
        writer: writer_task,
        reader: reader_task,
        _quic_endpoint: None,
        resume_supported,
    })
}

async fn platform_quic_send(
    send: &mut quinn::SendStream,
    codec: &SupplierTransportCodec,
    message: SupplierMessage,
) -> Result<()> {
    let (mut wire, compressed) = codec.encode(message)?;
    if !compressed {
        wire.push(b'\n');
    }
    send.write_all(&wire).await?;
    send.flush().await?;
    Ok(())
}

async fn platform_quic_read(
    reader: &mut BufReader<quinn::RecvStream>,
    codec: &SupplierTransportCodec,
) -> Result<SupplierMessage> {
    let text = codec
        .read_quic(reader)
        .await?
        .ok_or_else(|| anyhow!("platform QUIC stream closed"))?;
    Ok(serde_json::from_str(&text)?)
}

async fn connect_platform_quic_wire(
    quic_url: &str,
    token: &str,
) -> Result<PlatformWireConnection> {
    let identity_pin = parse_supplier_quic_url(quic_url)
        .ok()
        .and_then(|target| target.cert_sha256);
    let (endpoint, connection) = tokio::time::timeout(
        supplier_connect_timeout(),
        connect_supplier_quic_endpoint(quic_url),
    )
    .await
    .map_err(|_| anyhow!("platform QUIC connect timed out"))??;
    let (mut send, recv) = tokio::time::timeout(supplier_connect_timeout(), connection.open_bi())
        .await
        .map_err(|_| anyhow!("platform QUIC stream open timed out"))??;
    let mut reader = BufReader::new(recv);
    let codec = Arc::new(SupplierTransportCodec::new(identity_pin));
    platform_quic_send(&mut send, &codec, platform_auth_message(token, "authenticate")).await?;
    let auth = tokio::time::timeout(
        PLATFORM_DUPLEX_OPEN_TIMEOUT,
        platform_quic_read(&mut reader, &codec),
    )
    .await
    .map_err(|_| anyhow!("platform QUIC authentication timed out"))??;
    let resume_supported = platform_auth_ack(&auth)?;
    if let Some(confirmation) = codec.take_payload_encryption_confirmation() {
        platform_quic_send(&mut send, &codec, confirmation).await?;
    }

    let (outbound, mut outbound_rx) = mpsc::channel::<PlatformWireCommand>(32);
    let (inbound_tx, inbound) = mpsc::channel(32);
    let writer_codec = codec.clone();
    let writer_errors = inbound_tx.clone();
    let writer_task = crate::spawn_logged("platform quic writer", async move {
        while let Some(command) = outbound_rx.recv().await {
            let result = platform_quic_send(&mut send, &writer_codec, command.message).await;
            let detail = result.as_ref().err().map(|error| format!("{error:#}"));
            let _ = command.completed.send(result.map_err(|error| format!("{error:#}")));
            if let Some(detail) = detail {
                let _ = writer_errors.send(Err(detail)).await;
                return;
            }
        }
        let _ = send.finish();
    });
    let reader_codec = codec.clone();
    let reader_task = crate::spawn_logged("platform quic reader", async move {
        loop {
            let result = match reader_codec.read_quic(&mut reader).await {
                Ok(Some(text)) => serde_json::from_str::<SupplierMessage>(&text)
                    .map_err(|error| format!("{error:#}")),
                Ok(None) => Err("platform QUIC stream closed".to_string()),
                Err(error) => Err(format!("{error:#}")),
            };
            let terminal = result.is_err();
            if inbound_tx.send(result).await.is_err() || terminal {
                break;
            }
        }
    });
    Ok(PlatformWireConnection {
        codec,
        outbound,
        inbound,
        writer: writer_task,
        reader: reader_task,
        _quic_endpoint: Some(endpoint),
        resume_supported,
    })
}

async fn open_platform_wire(
    endpoint: &Endpoint,
    token: &str,
) -> Result<(PlatformWireConnection, String)> {
    let candidates = supplier_transport_candidates_for_urls(
        &endpoint.supplier_quic_url,
        &endpoint.supplier_ws_url,
    );
    if candidates.is_empty() {
        return Err(safe_platform_duplex_open_error(
            "the selected platform endpoint has no QUIC or WebSocket tunnel",
        ));
    }
    let identity_pin = parse_supplier_quic_url(&endpoint.supplier_quic_url)
        .ok()
        .and_then(|target| target.cert_sha256);
    let mut failures = Vec::new();
    for candidate in candidates {
        let result = match candidate.transport.as_str() {
            "quic" => connect_platform_quic_wire(&candidate.url, token).await,
            "websocket" => {
                connect_platform_websocket_wire(&candidate.url, identity_pin, token).await
            }
            _ => continue,
        };
        match result {
            Ok(connection) => return Ok((connection, candidate.transport)),
            Err(error) if platform_duplex_error_is_safe_to_replay(&error) => return Err(error),
            Err(error) => failures.push(format!("{}: {error:#}", candidate.transport)),
        }
    }
    Err(safe_platform_duplex_open_error(format!(
        "platform long connection could not be established: {}",
        failures.join("; ")
    )))
}

fn platform_endpoint_connection_key(endpoint: &Endpoint) -> String {
    format!(
        "{}\u{1f}{}",
        endpoint.supplier_quic_url.trim().trim_end_matches('/'),
        endpoint.supplier_ws_url.trim().trim_end_matches('/')
    )
}

fn platform_duplex_error_message(payload: &serde_json::Map<String, serde_json::Value>) -> String {
    payload
        .get("error")
        .and_then(serde_json::Value::as_object)
        .and_then(|error| error.get("message"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .or_else(|| {
            payload
                .get("error_kind")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|message| !message.is_empty())
        })
        .unwrap_or("platform duplex session could not be opened")
        .to_string()
}

fn platform_physical_connection(
    wire: PlatformWireConnection,
    transport: String,
    token: String,
    access_token: Arc<StdMutex<String>>,
    account_refresh_notify: Arc<Notify>,
) -> Arc<PlatformPhysicalConnection> {
    let (commands, command_rx) = mpsc::channel(64);
    let closed = Arc::new(AtomicBool::new(false));
    let authenticated_token = Arc::new(StdMutex::new(token.clone()));
    let resume_supported = wire.resume_supported;
    let connection = Arc::new(PlatformPhysicalConnection {
        commands,
        transport,
        closed: closed.clone(),
        authenticated_token: authenticated_token.clone(),
        resume_supported,
        output_budget: Default::default(),
    });
    crate::spawn_logged(
        "platform duplex physical connection",
        run_platform_physical_connection(
            wire,
            command_rx,
            closed,
            authenticated_token,
            token,
            access_token,
            account_refresh_notify,
        ),
    );
    connection
}

// Size retained JSON without reserializing/copying its text or binary payload.
// Include container overhead, including unknown fields from newer servers.
fn platform_output_heap_bytes(message: &SupplierMessage) -> usize {
    fn object_bytes(object: &serde_json::Map<String, serde_json::Value>) -> usize {
        object.iter().fold(0usize, |total, (key, value)| {
            total
                .saturating_add(key.capacity())
                .saturating_add(size_of::<serde_json::Value>() + size_of::<String>() + 64)
                .saturating_add(value_bytes(value))
        })
    }
    fn value_bytes(value: &serde_json::Value) -> usize {
        match value {
            serde_json::Value::String(text) => text.capacity(),
            serde_json::Value::Array(values) => values.iter().fold(
                values
                    .capacity()
                    .saturating_mul(size_of::<serde_json::Value>()),
                |total, value| total.saturating_add(value_bytes(value)),
            ),
            serde_json::Value::Object(object) => object_bytes(object),
            _ => 0,
        }
    }
    message
        .id
        .capacity()
        .saturating_add(message.kind.capacity())
        .saturating_add(object_bytes(&message.payload))
}

async fn run_platform_physical_connection(
    mut wire: PlatformWireConnection,
    mut commands: mpsc::Receiver<PlatformPhysicalCommand>,
    closed: Arc<AtomicBool>,
    authenticated_token: Arc<StdMutex<String>>,
    mut last_token: String,
    access_token: Arc<StdMutex<String>>,
    account_refresh_notify: Arc<Notify>,
) {
    let mut sessions = HashMap::<
        String,
        crate::output_buffer::OutputSender<std::result::Result<SupplierMessage, String>>,
    >::new();
    let mut pending_output: Option<(
        crate::output_buffer::OutputSender<std::result::Result<SupplierMessage, String>>,
        SupplierMessage,
        usize,
    )> = None;
    let mut heartbeat_tick = tokio::time::interval(supplier_heartbeat_interval());
    heartbeat_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    heartbeat_tick.tick().await;
    let mut token_tick = tokio::time::interval(Duration::from_secs(5));
    token_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    token_tick.tick().await;
    let pong_timeout = tokio::time::sleep(supplier_heartbeat_response_timeout());
    tokio::pin!(pong_timeout);
    let mut heartbeat = SupplierHeartbeat::default();
    let mut pending_refresh: Option<(String, String)> = None;
    let mut terminal_error = "platform long connection closed".to_string();

    loop {
        if pending_output.as_ref().is_some_and(|(_, message, _)| !sessions.contains_key(&message.id)) {
            // Explicit local session cancellation, not a slow consumer.
            pending_output = None;
        }
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else {
                    break;
                };
                match command {
                    PlatformPhysicalCommand::Open { message, events, completed } => {
                        if sessions.contains_key(&message.id) {
                            let _ = completed.send(Err("platform duplex session id is already active".to_string()));
                            continue;
                        }
                        let session_id = message.id.clone();
                        sessions.insert(session_id.clone(), events);
                        let result = wire.send(message).await.map_err(|error| format!("{error:#}"));
                        if result.is_err() {
                            sessions.remove(&session_id);
                        }
                        let failed = result.is_err();
                        let _ = completed.send(result);
                        if failed {
                            terminal_error = "platform long-connection write failed".to_string();
                            break;
                        }
                    }
                    PlatformPhysicalCommand::Send { message, completed } => {
                        let close = message.kind == "platform_duplex_close";
                        let session_id = message.id.clone();
                        let result = wire.send(message).await.map_err(|error| format!("{error:#}"));
                        let failed = result.is_err();
                        let _ = completed.send(result);
                        if close {
                            sessions.remove(&session_id);
                        }
                        if failed {
                            terminal_error = "platform long-connection write failed".to_string();
                            break;
                        }
                    }
                    PlatformPhysicalCommand::Close { session_id } => {
                        sessions.remove(&session_id);
                        let _ = wire.send(SupplierMessage {
                            id: session_id,
                            kind: "platform_duplex_close".to_string(),
                            payload: serde_json::Map::new(),
                        }).await;
                    }
                }
            }
            credit = async {
                let Some((events, _, bytes)) = pending_output.as_ref() else {
                    return futures_util::future::pending().await;
                };
                events.reserve(*bytes).await
            }, if pending_output.is_some() => {
                if let Some((events, message, _)) = pending_output.take() {
                    let session_id = message.id.clone();
                    let terminal = matches!(message.kind.as_str(), "platform_duplex_error" | "platform_duplex_close");
                    let sent = credit.is_some_and(|credit| events.send_reserved(Ok(message), credit));
                    if terminal || !sent {
                        sessions.remove(&session_id);
                    }
                    if !sent {
                        let _ = wire.send(SupplierMessage {
                            id: session_id,
                            kind: "platform_duplex_close".to_string(),
                            payload: serde_json::Map::new(),
                        }).await;
                    }
                    // Local backpressure is not a missed remote heartbeat.
                    if heartbeat.pending() {
                        pong_timeout.as_mut().reset(tokio::time::Instant::now() + supplier_heartbeat_response_timeout());
                    }
                }
            }
            incoming = wire.inbound.recv(), if pending_output.is_none() => {
                let Some(incoming) = incoming else {
                    break;
                };
                let message = match incoming {
                    Ok(message) => message,
                    Err(error) => {
                        terminal_error = error;
                        break;
                    }
                };
                heartbeat.observe(&wire.codec.activity);
                match message.kind.as_str() {
                    "server_restarting" => {
                        terminal_error = "platform server is restarting".to_string();
                        break;
                    }
                    "pong" => {
                        continue;
                    }
                    "ping" => {
                        if let Err(error) = wire.send(SupplierMessage {
                            id: message.id,
                            kind: "pong".to_string(),
                            payload: serde_json::Map::new(),
                        }).await {
                            terminal_error = format!("{error:#}");
                            break;
                        }
                        continue;
                    }
                    "authenticate_ack" => {
                        if pending_refresh.as_ref().is_some_and(|(id, _)| id == &message.id) {
                            if message.payload.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
                                if let Some((_, token)) = pending_refresh.take() {
                                    last_token = token.clone();
                                    if let Ok(mut authenticated) = authenticated_token.lock() {
                                        *authenticated = token;
                                    }
                                }
                            } else {
                                account_refresh_notify.notify_one();
                                terminal_error = "platform authentication refresh was rejected".to_string();
                                break;
                            }
                        }
                        continue;
                    }
                    _ => {}
                }

                let session_id = message.id.clone();
                let Some(events) = sessions.get(&session_id).cloned() else {
                    continue;
                };
                let bytes = platform_output_heap_bytes(&message);
                pending_output = Some((events, message, bytes));
            }
            _ = heartbeat_tick.tick(), if !heartbeat.pending() && pending_output.is_none() => {
                heartbeat.observe(&wire.codec.activity);
                if !heartbeat.needs_probe(&wire.codec.activity) {
                    continue;
                }
                let message = supplier_heartbeat_message();
                let id = message.id.clone();
                heartbeat.queued(id.clone());
                if let Err(error) = wire.send(message).await {
                    terminal_error = format!("{error:#}");
                    break;
                }
                heartbeat.written(&id, supplier_heartbeat_response_timeout());
                pong_timeout.as_mut().reset(
                    tokio::time::Instant::now() + supplier_heartbeat_response_timeout(),
                );
            }
            _ = token_tick.tick() => {
                heartbeat.observe(&wire.codec.activity);
                if pending_refresh.is_some() {
                    continue;
                }
                let token = access_token.lock()
                    .map(|token| token.trim().to_string())
                    .unwrap_or_default();
                if token == last_token {
                    continue;
                }
                if token.is_empty() {
                    terminal_error = "platform account authentication was cleared".to_string();
                    break;
                }
                let message = platform_auth_message(&token, "auth_refresh");
                let id = message.id.clone();
                if let Err(error) = wire.send(message).await {
                    terminal_error = format!("{error:#}");
                    break;
                }
                pending_refresh = Some((id, token));
            }
            _ = &mut pong_timeout, if heartbeat.pending() && pending_output.is_none() => {
                if heartbeat.observe(&wire.codec.activity) {
                    continue;
                }
                terminal_error = format!("platform long-connection heartbeat timed out: {}", heartbeat.timeout_detail());
                break;
            }
        }
    }

    closed.store(true, Ordering::Release);
    wire.reader.abort();
    wire.inbound.close();
    // Preserve the already-read frame ahead of the terminal error, even when
    // shutdown interrupts a wait for buffer credits. No new upstream execution.
    if let Some((events, message, _)) = pending_output {
        if sessions.contains_key(&message.id) {
            events.finish(Ok(message));
        }
    }
    // Also keep frames already decoded by the bounded wire reader if a write
    // failure ended the actor first. These were already in client memory.
    while let Ok(incoming) = wire.inbound.try_recv() {
        if let Ok(message) = incoming {
            if let Some(events) = sessions.get(&message.id) {
                events.finish(Ok(message));
            }
        }
    }
    for (_, events) in sessions.drain() {
        events.finish(Err(terminal_error.clone()));
    }
    wire.stop().await;
}

impl PlatformPhysicalConnection {
    fn usable_with_token(&self, token: &str) -> bool {
        !self.closed.load(Ordering::Acquire)
            && self
                .authenticated_token
                .lock()
                .map(|current| current.as_str() == token)
                .unwrap_or(false)
    }

    async fn open(
        &self,
        message: SupplierMessage,
    ) -> Result<(
        crate::output_buffer::OutputReceiver<std::result::Result<SupplierMessage, String>>,
        bool,
    )> {
        if self.closed.load(Ordering::Acquire) {
            return Err(safe_platform_duplex_open_error(
                "platform long connection is closed",
            ));
        }
        let (events, mut event_rx) = crate::output_buffer::channel(self.output_budget.clone());
        let (completed, written) = oneshot::channel();
        self.commands
            .send(PlatformPhysicalCommand::Open {
                message,
                events,
                completed,
            })
            .await
            .map_err(|_| safe_platform_duplex_open_error("platform long connection is closed"))?;
        written
            .await
            .map_err(|_| {
                safe_platform_duplex_open_error("platform long-connection writer stopped")
            })?
            .map_err(safe_platform_duplex_open_error)?;

        loop {
            let response = tokio::time::timeout(PLATFORM_DUPLEX_OPEN_TIMEOUT, event_rx.recv())
                .await
                .map_err(|_| safe_platform_duplex_open_error("platform duplex open timed out"))?
                .ok_or_else(|| {
                    safe_platform_duplex_open_error(
                        "platform long connection closed during duplex open",
                    )
                })?
                .map_err(safe_platform_duplex_open_error)?;
            match response.kind.as_str() {
                "platform_duplex_opened" => {
                    let persisted = response
                        .payload
                        .get("persisted")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    return Ok((event_rx, persisted));
                }
                "platform_duplex_error" => {
                    return Err(safe_platform_duplex_open_error(
                        platform_duplex_error_message(&response.payload),
                    ))
                }
                "platform_duplex_close" => {
                    return Err(safe_platform_duplex_open_error(
                        "platform closed the duplex session during open",
                    ))
                }
                _ => continue,
            }
        }
    }

    async fn send(&self, message: SupplierMessage) -> Result<()> {
        let (completed, result) = oneshot::channel();
        self.commands
            .send(PlatformPhysicalCommand::Send { message, completed })
            .await
            .map_err(|_| anyhow!("platform long connection is closed"))?;
        result
            .await
            .map_err(|_| anyhow!("platform long-connection writer stopped"))?
            .map_err(anyhow::Error::msg)
    }
}

impl PlatformDuplexPool {
    async fn connection(
        &self,
        endpoint: &Endpoint,
        access_token: Arc<StdMutex<String>>,
        account_refresh_notify: Arc<Notify>,
    ) -> Result<Arc<PlatformPhysicalConnection>> {
        let token = access_token
            .lock()
            .map(|token| token.trim().to_string())
            .unwrap_or_default();
        if token.is_empty() {
            return Err(safe_platform_duplex_open_error(
                "platform account authentication is unavailable",
            ));
        }
        let key = platform_endpoint_connection_key(endpoint);
        if let Some(connection) = self.connections.lock().await.get(&key).cloned() {
            if connection.usable_with_token(&token) {
                return Ok(connection);
            }
        }
        let connect_lock = {
            let mut locks = self.connect_locks.lock().await;
            locks
                .entry(key.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _connect_guard = connect_lock.lock().await;
        if let Some(connection) = self.connections.lock().await.get(&key).cloned() {
            if connection.usable_with_token(&token) {
                return Ok(connection);
            }
        }
        let (wire, transport) = open_platform_wire(endpoint, &token).await?;
        let connection = platform_physical_connection(
            wire,
            transport,
            token,
            access_token,
            account_refresh_notify,
        );
        self.connections
            .lock()
            .await
            .insert(key, connection.clone());
        Ok(connection)
    }
}

async fn reconnect_platform_logical_session(
    pool: &PlatformDuplexPool,
    endpoint: &Endpoint,
    access_token: Arc<StdMutex<String>>,
    account_refresh_notify: Arc<Notify>,
    headers: &reqwest::header::HeaderMap,
    session_id: &str,
    active_turn_id: Option<&str>,
    after_turn_sequence: u64,
    after_platform_sequence: u64,
) -> Result<(
    Arc<PlatformPhysicalConnection>,
    crate::output_buffer::OutputReceiver<std::result::Result<SupplierMessage, String>>,
    bool,
)> {
    let deadline = tokio::time::Instant::now() + PLATFORM_DUPLEX_RESUME_WINDOW;
    let mut retry_delay = Duration::from_millis(250);
    loop {
        let last_error = match pool
            .connection(
                endpoint,
                access_token.clone(),
                account_refresh_notify.clone(),
            )
            .await
        {
            Ok(physical) => {
                if let Some(turn_id) = active_turn_id {
                    if !physical.resume_supported {
                        return Err(anyhow!(
                            "the platform server cannot resume an in-flight duplex response"
                        ));
                    }
                    let resume = platform_duplex_resume_message(
                        session_id,
                        Some(turn_id),
                        after_turn_sequence,
                        after_platform_sequence,
                    );
                    match physical.open(resume).await {
                        Ok((events, persisted)) => return Ok((physical, events, persisted)),
                        Err(error) => format!("{error:#}"),
                    }
                } else {
                    if physical.resume_supported {
                        let resume = platform_duplex_resume_message(
                            session_id,
                            None,
                            0,
                            after_platform_sequence,
                        );
                        if let Ok((events, persisted)) = physical.open(resume).await {
                            return Ok((physical, events, persisted));
                        }
                    }
                    match physical
                        .open(platform_duplex_open_message(session_id, headers))
                        .await
                    {
                        Ok((events, persisted)) => return Ok((physical, events, persisted)),
                        Err(error) => format!("{error:#}"),
                    }
                }
            }
            Err(error) => format!("{error:#}"),
        };
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Err(anyhow!(
                "platform duplex response recovery expired: {last_error}"
            ));
        }
        // A fleet of clients reconnecting at a fixed 250 ms cadence can keep a
        // replacement server overloaded. Fast exponential backoff with jitter
        // preserves low single-client recovery latency without synchronizing a
        // reconnect storm.
        let jitter = Duration::from_millis(rand::random::<u64>() % 126);
        tokio::time::sleep(std::cmp::min(retry_delay + jitter, deadline - now)).await;
        retry_delay = std::cmp::min(retry_delay.saturating_mul(2), Duration::from_secs(2));
    }
}

pub(crate) async fn open_platform_responses_duplex(
    pool: &PlatformDuplexPool,
    endpoint: &Endpoint,
    access_token: Arc<StdMutex<String>>,
    account_refresh_notify: Arc<Notify>,
    headers: reqwest::header::HeaderMap,
) -> Result<(PlatformDuplexSession, String)> {
    let mut physical = pool
        .connection(
            endpoint,
            access_token.clone(),
            account_refresh_notify.clone(),
        )
        .await?;
    let session_id = format!("platform-session-{:032x}", rand::random::<u128>());
    let open = platform_duplex_open_message(&session_id, &headers);
    let (mut events, _) = match physical.open(open).await {
        Ok(events) => events,
        Err(error) => {
            let _ = physical
                .commands
                .send(PlatformPhysicalCommand::Close {
                    session_id: session_id.clone(),
                })
                .await;
            return Err(error);
        }
    };
    let transport = physical.transport.clone();
    let pool = pool.clone();
    let endpoint = endpoint.clone();
    let reconnect_access_token = access_token;
    let reconnect_notify = account_refresh_notify;
    let (commands, mut command_rx) = mpsc::channel::<PlatformDuplexCommand>(16);
    // One frame of slack keeps the ACK close to actual local consumption while
    // still decoupling the tunnel reader from a brief scheduler delay.
    let (frames, frame_rx) = mpsc::channel(1);
    crate::spawn_logged("platform duplex logical session", async move {
        let mut active_turn_id: Option<String> = None;
        let mut last_turn_sequence = 0u64;
        let mut last_platform_sequence = 0u64;
        loop {
            tokio::select! {
                command = command_rx.recv() => {
                    let Some(command) = command else {
                        let _ = physical.commands.send(PlatformPhysicalCommand::Close {
                            session_id: session_id.clone(),
                        }).await;
                        break;
                    };
                    let close = matches!(command.frame, PlatformDuplexFrame::Close { .. });
                    let mut payload = platform_frame_payload(command.frame.clone());
                    if !close {
                        if let Some(turn_id) = platform_duplex_frame_turn_id(&command.frame) {
                            if active_turn_id.is_some() {
                                let _ = command.completed.send(Err(
                                    "a platform duplex turn is already active".to_string()
                                ));
                                continue;
                            }
                            payload.insert(
                                PLATFORM_DUPLEX_TURN_ID_FIELD.to_string(),
                                serde_json::Value::String(turn_id.clone()),
                            );
                            active_turn_id = Some(turn_id);
                            last_turn_sequence = 0;
                        }
                    }
                    let kind = if close {
                        "platform_duplex_close"
                    } else {
                        "platform_duplex_client_frame"
                    };
                    let result = physical.send(SupplierMessage {
                        id: session_id.clone(),
                        kind: kind.to_string(),
                        payload,
                    }).await;
                    if result.is_err() && active_turn_id.is_some() && physical.resume_supported {
                        match reconnect_platform_logical_session(
                            &pool,
                            &endpoint,
                            reconnect_access_token.clone(),
                            reconnect_notify.clone(),
                            &headers,
                            &session_id,
                            active_turn_id.as_deref(),
                            last_turn_sequence,
                            last_platform_sequence,
                        ).await {
                            Ok((next_physical, next_events, _persisted)) => {
                                physical = next_physical;
                                events = next_events;
                                let _ = command.completed.send(Ok(()));
                                continue;
                            }
                            Err(error) => {
                                let detail = format!("{error:#}");
                                let _ = command.completed.send(Err(detail.clone()));
                                let _ = frames.send(Err(detail)).await;
                                break;
                            }
                        }
                    }
                    let failed = result.is_err();
                    let _ = command.completed.send(result.map_err(|error| format!("{error:#}")));
                    if failed || close {
                        break;
                    }
                }
                incoming = events.recv() => {
                    let incoming = match incoming {
                        Some(Ok(message)) => Some(message),
                        Some(Err(_)) | None => None,
                    };
                    let Some(message) = incoming else {
                        match reconnect_platform_logical_session(
                            &pool,
                            &endpoint,
                            reconnect_access_token.clone(),
                            reconnect_notify.clone(),
                            &headers,
                            &session_id,
                            active_turn_id.as_deref(),
                            last_turn_sequence,
                            last_platform_sequence,
                        ).await {
                            Ok((next_physical, next_events, _persisted)) => {
                                physical = next_physical;
                                events = next_events;
                                continue;
                            }
                            Err(error) => {
                                let _ = frames.send(Err(format!("{error:#}"))).await;
                                break;
                            }
                        }
                    };
                    if message.kind == "platform_duplex_close" {
                        match reconnect_platform_logical_session(
                            &pool,
                            &endpoint,
                            reconnect_access_token.clone(),
                            reconnect_notify.clone(),
                            &headers,
                            &session_id,
                            active_turn_id.as_deref(),
                            last_turn_sequence,
                            last_platform_sequence,
                        ).await {
                            Ok((next_physical, next_events, _persisted)) => {
                                physical = next_physical;
                                events = next_events;
                                continue;
                            }
                            Err(error) => {
                                let _ = frames.send(Err(format!("{error:#}"))).await;
                                break;
                            }
                        }
                    }
                    let platform_sequence = message
                        .payload
                        .get(PLATFORM_DUPLEX_SEQUENCE_FIELD)
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or_default();
                    if physical.resume_supported {
                        if platform_sequence == 0 {
                            let _ = frames.send(Err(
                                "platform duplex response is missing its recovery sequence".to_string()
                            )).await;
                            break;
                        }
                        if platform_sequence <= last_platform_sequence {
                            let _ = physical
                                .send(platform_duplex_ack_message(&session_id, platform_sequence))
                                .await;
                            continue;
                        }
                        if platform_sequence != last_platform_sequence.saturating_add(1) {
                            let _ = frames.send(Err(
                                "platform duplex response sequence has a gap".to_string()
                            )).await;
                            break;
                        }
                    }
                    if message.kind == "platform_duplex_server_frame" {
                        if let Some(turn_id) = active_turn_id.as_deref() {
                            let response_turn_id = payload_string(&message.payload, PLATFORM_DUPLEX_TURN_ID_FIELD);
                            let turn_sequence = message
                                .payload
                                .get(PLATFORM_DUPLEX_TURN_SEQUENCE_FIELD)
                                .and_then(serde_json::Value::as_u64)
                                .unwrap_or_default();
                            if response_turn_id != turn_id || turn_sequence == 0 {
                                let _ = frames.send(Err(
                                    "platform duplex response does not match the active turn".to_string()
                                )).await;
                                break;
                            }
                            if turn_sequence <= last_turn_sequence {
                                if platform_sequence > 0 {
                                    last_platform_sequence = platform_sequence;
                                    let _ = physical
                                        .send(platform_duplex_ack_message(&session_id, platform_sequence))
                                        .await;
                                }
                                continue;
                            }
                            if turn_sequence != last_turn_sequence.saturating_add(1) {
                                let _ = frames.send(Err(
                                    "platform duplex turn response sequence has a gap".to_string()
                                )).await;
                                break;
                            }
                            last_turn_sequence = turn_sequence;
                        }
                    }
                    let terminal_turn = message.kind == "platform_duplex_server_frame"
                        && message
                            .payload
                            .get("turn_terminal")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false);
                    let frame = match message.kind.as_str() {
                        "platform_duplex_server_frame" => platform_server_frame(&message.payload)
                            .map_err(|error| format!("{error:#}")),
                        "platform_duplex_error" => Ok(platform_error_frame(&message.payload)),
                        _ => continue,
                    };
                    if frames.send(frame).await.is_err() {
                        break;
                    }
                    // Delivery into the local proxy queue is the recovery
                    // boundary. Update terminal/error state before attempting
                    // the transport ACK so a lost ACK cannot make the client
                    // resume an already completed model turn forever.
                    if terminal_turn {
                        active_turn_id = None;
                    }
                    let terminal_error = message.kind == "platform_duplex_error";
                    if platform_sequence > 0 {
                        last_platform_sequence = platform_sequence;
                        if physical
                            .send(platform_duplex_ack_message(&session_id, platform_sequence))
                            .await
                            .is_err()
                        {
                            continue;
                        }
                    }
                    if terminal_error {
                        break;
                    }
                }
            }
        }
    });
    Ok((
        PlatformDuplexSession {
            sender: PlatformDuplexSender { outbound: commands },
            receiver: PlatformDuplexReceiver { inbound: frame_rx },
        },
        transport,
    ))
}

/// Native media uses the same QUIC-first physical pool, but never resumes or
/// replays frames: stale audio must not be spoken (or billed) a second time.
#[cfg(test)]
pub(crate) async fn open_platform_native_duplex(
    pool: &PlatformDuplexPool,
    endpoint: &Endpoint,
    access_token: Arc<StdMutex<String>>,
    account_refresh_notify: Arc<Notify>,
    headers: reqwest::header::HeaderMap,
    path: &str,
    raw_query: &str,
) -> Result<(PlatformDuplexSession, String)> {
    open_platform_native_session(pool, endpoint, access_token, account_refresh_notify, headers, path, raw_query, None).await
}

pub(crate) async fn open_platform_native_session(
    pool: &PlatformDuplexPool,
    endpoint: &Endpoint,
    access_token: Arc<StdMutex<String>>,
    account_refresh_notify: Arc<Notify>,
    headers: reqwest::header::HeaderMap,
    path: &str,
    raw_query: &str,
    call_body: Option<&[u8]>,
) -> Result<(PlatformDuplexSession, String)> {
    let physical = pool
        .connection(endpoint, access_token, account_refresh_notify)
        .await?;
    let session_id = format!("platform-media-{:032x}", rand::random::<u128>());
    let mut open = platform_duplex_open_message(&session_id, &headers);
    open.payload
        .insert("session_kind".into(), if call_body.is_some() { "native_realtime_call" } else { "native_realtime" }.into());
    if let Some(request) = open
        .payload
        .get_mut("request")
        .and_then(serde_json::Value::as_object_mut)
    {
        request.insert("path".into(), path.into());
        request.insert("raw_query".into(), raw_query.into());
        if let Some(body) = call_body {
            request.insert("method".into(), "POST".into());
            request.insert("body".into(), std::str::from_utf8(body)?.into());
        }
    }
    let (mut events, _) = match physical.open(open).await {
        Ok(events) => events,
        Err(error) => {
            let _ = physical
                .commands
                .send(PlatformPhysicalCommand::Close { session_id })
                .await;
            return Err(error);
        }
    };
    let transport = physical.transport.clone();
    let (commands, mut command_rx) = mpsc::channel::<PlatformDuplexCommand>(16);
    let (frames, frame_rx) = mpsc::channel(1);
    crate::spawn_logged("platform native media session", async move {
        // Poll both directions independently. A full downstream queue must not
        // block the acknowledgement that the caller is awaiting for an upload.
        let upload = async {
            while let Some(command) = command_rx.recv().await {
                let close = matches!(command.frame, PlatformDuplexFrame::Close { .. });
                let result = physical
                    .send(SupplierMessage {
                        id: session_id.clone(),
                        kind: if close {
                            "platform_duplex_close"
                        } else {
                            "platform_duplex_client_frame"
                        }.into(),
                        payload: platform_frame_payload(command.frame),
                    }).await;
                let failed = result.is_err();
                let _ = command.completed.send(result.map_err(|error| format!("{error:#}")));
                if failed || close {
                    break;
                }
            }
        };
        let download = async {
            loop {
                let message = match events.recv().await {
                    Some(Ok(message)) => message,
                    Some(Err(error)) => {
                        let _ = frames.send(Err(error)).await;
                        break;
                    }
                    None => break,
                };
                let frame = match message.kind.as_str() {
                    "platform_duplex_server_frame" => platform_server_frame(&message.payload)
                        .map_err(|error| format!("{error:#}")),
                    "platform_duplex_error" => Ok(platform_error_frame(&message.payload)),
                    "platform_duplex_close" => break,
                    _ => continue,
                };
                if frames.send(frame).await.is_err() || message.kind == "platform_duplex_error" {
                    break;
                }
            }
        };
        tokio::select! {
            _ = upload => {}
            _ = download => {}
            _ = frames.closed() => {}
        }
        let _ = physical
            .commands
            .send(PlatformPhysicalCommand::Close { session_id })
            .await;
    });
    Ok((
        PlatformDuplexSession {
            sender: PlatformDuplexSender { outbound: commands },
            receiver: PlatformDuplexReceiver { inbound: frame_rx },
        },
        transport,
    ))
}

#[cfg(test)]
mod platform_duplex_tests {
    use super::*;

    fn buffered_mock_physical() -> (
        Arc<PlatformPhysicalConnection>,
        mpsc::Sender<Result<SupplierMessage, String>>,
        mpsc::UnboundedReceiver<SupplierMessage>,
    ) {
        let (outbound, mut writes) = mpsc::channel::<PlatformWireCommand>(16);
        let (inbound, received) = mpsc::channel(32);
        let (observed, observations) = mpsc::unbounded_channel();
        let replies = inbound.clone();
        let writer = tokio::spawn(async move {
            while let Some(command) = writes.recv().await {
                let message = command.message;
                if message.kind == "platform_duplex_open" {
                    replies
                        .send(Ok(SupplierMessage {
                            id: message.id.clone(),
                            kind: "platform_duplex_opened".into(),
                            payload: serde_json::Map::new(),
                        }))
                        .await
                        .unwrap();
                }
                let _ = observed.send(message);
                let _ = command.completed.send(Ok(()));
            }
        });
        let physical = platform_physical_connection(
            PlatformWireConnection {
                codec: Arc::new(SupplierTransportCodec::new(None)),
                outbound,
                inbound: received,
                writer,
                reader: tokio::spawn(async {}),
                _quic_endpoint: None,
                resume_supported: false,
            },
            "mock".into(),
            "access".into(),
            Arc::new(StdMutex::new("access".into())),
            Arc::new(Notify::new()),
        );
        (physical, inbound, observations)
    }

    fn buffered_mock_frame(id: &str, text: String) -> SupplierMessage {
        SupplierMessage {
            id: id.into(),
            kind: "platform_duplex_server_frame".into(),
            payload: platform_frame_payload(PlatformDuplexFrame::Text(text)),
        }
    }

    #[tokio::test]
    async fn slow_usage_client_retains_burst_and_terminal_without_blocking_other_sessions() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let (physical, incoming, _writes) = buffered_mock_physical();
            let (mut slow, _) = physical
                .open(platform_duplex_open_message("slow", &Default::default()))
                .await
                .unwrap();
            let (mut fast, _) = physical
                .open(platform_duplex_open_message("fast", &Default::default()))
                .await
                .unwrap();
            for sequence in 0..4096 {
                incoming
                    .send(Ok(buffered_mock_frame("slow", sequence.to_string())))
                    .await
                    .unwrap();
            }
            incoming
                .send(Ok(buffered_mock_frame("fast", "fast response".into())))
                .await
                .unwrap();
            let message = fast.recv().await.unwrap().unwrap();
            assert_eq!(
                platform_server_frame(&message.payload).unwrap(),
                PlatformDuplexFrame::Text("fast response".into())
            );
            assert_eq!(slow.len(), 4096, "retain burst beyond old 256-event cutoff");
            incoming.send(Err("wire interrupted".into())).await.unwrap();
            while !physical.closed.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
            for sequence in 0..4096 {
                let message = slow.recv().await.unwrap().unwrap();
                assert_eq!(
                    platform_server_frame(&message.payload).unwrap(),
                    PlatformDuplexFrame::Text(sequence.to_string())
                );
            }
            assert_eq!(slow.recv().await.unwrap().unwrap_err(), "wire interrupted");
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn full_usage_buffer_still_services_upload_and_cancellation() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let (mut physical, incoming, mut writes) = buffered_mock_physical();
            Arc::get_mut(&mut physical).unwrap().output_budget =
                crate::output_buffer::OutputBudget::with_limit(256);
            let (mut output, _) = physical
                .open(platform_duplex_open_message("slow", &Default::default()))
                .await
                .unwrap();
            assert_eq!(writes.recv().await.unwrap().kind, "platform_duplex_open");
            // Each message takes the whole budget. Second is already read but
            // waiting for local consumption; no consumer failure or extra dial.
            incoming
                .send(Ok(buffered_mock_frame("slow", "a".repeat(256))))
                .await
                .unwrap();
            incoming
                .send(Ok(buffered_mock_frame("slow", "b".repeat(256))))
                .await
                .unwrap();
            while output.is_empty() || incoming.capacity() < 32 {
                tokio::task::yield_now().await;
            }
            physical
                .send(SupplierMessage {
                    id: "slow".into(),
                    kind: "platform_duplex_client_frame".into(),
                    payload: Default::default(),
                })
                .await
                .unwrap();
            assert_eq!(
                writes.recv().await.unwrap().kind,
                "platform_duplex_client_frame"
            );
            assert!(!physical.closed.load(Ordering::Acquire));
            assert_eq!(
                platform_server_frame(&output.recv().await.unwrap().unwrap().payload).unwrap(),
                PlatformDuplexFrame::Text("a".repeat(256))
            );
            assert_eq!(
                platform_server_frame(&output.recv().await.unwrap().unwrap().payload).unwrap(),
                PlatformDuplexFrame::Text("b".repeat(256))
            );
            incoming
                .send(Ok(buffered_mock_frame("slow", "c".repeat(256))))
                .await
                .unwrap();
            incoming
                .send(Ok(buffered_mock_frame("slow", "d".repeat(256))))
                .await
                .unwrap();
            physical
                .send(SupplierMessage {
                    id: "slow".into(),
                    kind: "platform_duplex_close".into(),
                    payload: Default::default(),
                })
                .await
                .unwrap();
            assert_eq!(writes.recv().await.unwrap().kind, "platform_duplex_close");
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn usage_shutdown_drains_already_received_output_before_error() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let (mut physical, incoming, _writes) = buffered_mock_physical();
            Arc::get_mut(&mut physical).unwrap().output_budget =
                crate::output_buffer::OutputBudget::with_limit(256);
            let (mut output, _) = physical
                .open(platform_duplex_open_message("slow", &Default::default()))
                .await.unwrap();
            for text in ["a", "b"] {
                incoming.send(Ok(buffered_mock_frame("slow", text.repeat(256))))
                    .await.unwrap();
            }
            while output.is_empty() || incoming.capacity() < 32 {
                tokio::task::yield_now().await;
            }
            drop(physical);
            for text in ["a", "b"] {
                assert_eq!(
                    platform_server_frame(&output.recv().await.unwrap().unwrap().payload).unwrap(),
                    PlatformDuplexFrame::Text(text.repeat(256)),
                );
            }
            assert_eq!(output.recv().await.unwrap().unwrap_err(), "platform long connection closed");
            assert!(output.recv().await.is_none());
        }).await.unwrap();
    }

    #[tokio::test]
    async fn native_media_uses_existing_tunnel_and_preserves_frames_without_replay() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let auth = read_mock_message(&mut socket).await;
            socket.send(WsMessage::Text(serde_json::json!({
                "id":auth.id, "type":"authenticate_ack", "payload":{
                    "ok":true, "transport_features":[PLATFORM_DUPLEX_FEATURE, PLATFORM_DUPLEX_RESUME_FEATURE]
                }
            }).to_string().into())).await.unwrap();
            let open = read_mock_message(&mut socket).await;
            assert_eq!(open.payload["session_kind"], "native_realtime");
            assert_eq!(open.payload["request"]["path"], "/v1/live");
            assert_eq!(
                open.payload["request"]["raw_query"],
                "model=gpt-live-1-codex&future=yes"
            );
            socket
                .send(WsMessage::Text(
                    serde_json::json!({
                        "id":open.id, "type":"platform_duplex_opened", "payload":{"ok":true}
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            for _ in 0..2 {
                let frame = read_mock_message(&mut socket).await;
                assert_eq!(frame.kind, "platform_duplex_client_frame");
                assert!(frame.payload.get("turn_id").is_none());
                socket.send(WsMessage::Text(serde_json::json!({
                    "id":frame.id, "type":"platform_duplex_server_frame", "payload":frame.payload
                }).to_string().into())).await.unwrap();
            }
            // A network loss ends media; it must not open another connection to replay.
            let _ = socket.close(None).await;
            assert!(
                tokio::time::timeout(Duration::from_millis(150), listener.accept())
                    .await
                    .is_err()
            );
        });
        let endpoint = Endpoint {
            server_id: String::new(),
            name: "mock".into(),
            base_url: format!("http://{address}"),
            supplier_ws_url: format!("ws://{address}/supplier/ws"),
            supplier_quic_url: String::new(),
            enabled: true,
        };
        let (session, transport) = open_platform_native_duplex(
            &PlatformDuplexPool::default(),
            &endpoint,
            Arc::new(StdMutex::new("access".into())),
            Arc::new(Notify::new()),
            reqwest::header::HeaderMap::new(),
            "/v1/live",
            "model=gpt-live-1-codex&future=yes",
        )
        .await
        .unwrap();
        assert_eq!(transport, "websocket");
        let (sender, mut receiver) = session.split();
        for frame in [
            PlatformDuplexFrame::Text(
                r#"{ "type":"input_audio.append","audio":"AA==","future":true }"#.into(),
            ),
            PlatformDuplexFrame::Binary(vec![0, 255, 1, 128]),
        ] {
            sender.send(frame.clone()).await.unwrap();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(5), receiver.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap(),
                frame
            );
        }
        let terminal = tokio::time::timeout(Duration::from_secs(5), receiver.next())
            .await
            .unwrap();
        assert!(terminal.is_none() || terminal.is_some_and(|frame| frame.is_err()));
        server.await.unwrap();
    }

    #[test]
    fn native_realtime_frames_bypass_only_their_own_supplier_replay() {
        for kind in [
            "duplex_opened",
            "duplex_server_frame",
            "duplex_error",
            "duplex_close",
        ] {
            let mut message = SupplierMessage {
                id: "voice".into(),
                kind: kind.into(),
                payload: serde_json::Map::new(),
            };
            assert!(!native_realtime_no_replay(&message));
            message
                .payload
                .insert("_const_realtime_no_replay".into(), true.into());
            assert!(native_realtime_no_replay(&message));
            message.kind = "response_chunk".into();
            assert!(!native_realtime_no_replay(&message));
        }
    }

    #[tokio::test]
    async fn native_media_upload_and_close_progress_with_full_download_queue() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (burst_sent, burst_received) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let auth = read_mock_message(&mut socket).await;
            socket.send(WsMessage::Text(serde_json::json!({
                "id":auth.id,"type":"authenticate_ack","payload":{
                    "ok":true,"transport_features":[PLATFORM_DUPLEX_FEATURE]
                }
            }).to_string().into())).await.unwrap();
            let open = read_mock_message(&mut socket).await;
            socket.send(WsMessage::Text(serde_json::json!({
                "id":open.id,"type":"platform_duplex_opened","payload":{"ok":true}
            }).to_string().into())).await.unwrap();
            for sequence in 0..8 {
                socket.send(WsMessage::Text(serde_json::json!({
                    "id":open.id,"type":"platform_duplex_server_frame",
                    "payload":{"frame_type":"text","data":sequence.to_string()}
                }).to_string().into())).await.unwrap();
            }
            burst_sent.send(()).unwrap();
            let upload = read_mock_message(&mut socket).await;
            assert_eq!(upload.kind, "platform_duplex_client_frame");
            assert_eq!(upload.payload["data"], "upload while download is full");
            assert_eq!(read_mock_message(&mut socket).await.kind, "platform_duplex_close");
        });
        let endpoint = Endpoint {
            server_id: String::new(),
            name: "backpressure".into(), base_url: format!("http://{address}"),
            supplier_ws_url: format!("ws://{address}/supplier/ws"),
            supplier_quic_url: String::new(), enabled: true,
        };
        let (session, _) = open_platform_native_duplex(
            &PlatformDuplexPool::default(), &endpoint,
            Arc::new(StdMutex::new("access".into())), Arc::new(Notify::new()),
            reqwest::header::HeaderMap::new(), "/v1/live", "model=gpt-live-1-codex",
        ).await.unwrap();
        let (sender, receiver) = session.split();
        burst_received.await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while receiver.inbound.is_empty() { tokio::task::yield_now().await; }
        }).await.unwrap();
        // Deliberately never drain the response queue. Writes and cancellation
        // must still finish; a larger buffer only hides this circular wait.
        tokio::time::timeout(Duration::from_secs(2), sender.send(PlatformDuplexFrame::Text(
            "upload while download is full".into(),
        ))).await.expect("upload deadlocked behind download").unwrap();
        tokio::time::timeout(Duration::from_secs(2), sender.send(PlatformDuplexFrame::Close {
            code: 1000, reason: "done".into(),
        })).await.expect("close deadlocked behind download").unwrap();
        tokio::time::timeout(Duration::from_secs(2), server).await.unwrap().unwrap();
    }

    async fn read_mock_message(
        socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    ) -> SupplierMessage {
        loop {
            let message = socket
                .next()
                .await
                .expect("mock platform message")
                .expect("mock platform frame");
            match message {
                WsMessage::Text(text) => {
                    return serde_json::from_str(&text).expect("mock platform JSON")
                }
                WsMessage::Binary(bytes) => {
                    return decode_supplier_transport_frame(&bytes)
                        .expect("mock compressed platform frame")
                }
                WsMessage::Ping(bytes) => {
                    socket
                        .send(WsMessage::Pong(bytes))
                        .await
                        .expect("mock platform pong");
                }
                _ => {}
            }
        }
    }

    #[tokio::test]
    async fn platform_websocket_multiplexes_logical_sessions_without_supplier_registration() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock platform websocket");
        let address = listener.local_addr().expect("mock platform address");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept platform websocket");
            let mut socket = tokio_tungstenite::accept_async(stream)
                .await
                .expect("upgrade platform websocket");
            let auth = read_mock_message(&mut socket).await;
            assert_eq!(auth.kind, "authenticate");
            socket
                .send(WsMessage::Text(
                    serde_json::json!({
                        "id": auth.id,
                        "type": "authenticate_ack",
                        "payload": {
                            "ok": true,
                            "user_id": "user-1",
                            "transport_features": [PLATFORM_DUPLEX_FEATURE, SUPPLIER_TRANSPORT_FRAME_COMPRESSION_FEATURE]
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .expect("send authentication ack");

            let open = read_mock_message(&mut socket).await;
            assert_eq!(open.kind, "platform_duplex_open");
            assert!(open.payload.get("channels").is_none());
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
                .expect("send platform duplex opened");

            let frame = read_mock_message(&mut socket).await;
            assert_eq!(frame.kind, "platform_duplex_client_frame");
            assert_eq!(payload_string(&frame.payload, "data"), "request");
            socket
                .send(WsMessage::Text(
                    serde_json::json!({
                        "id": frame.id,
                        "type": "platform_duplex_server_frame",
                        "payload": {"frame_type": "text", "data": "response"}
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .expect("send platform server frame");

            let second_open = read_mock_message(&mut socket).await;
            assert_eq!(second_open.kind, "platform_duplex_open");
            assert_ne!(second_open.id, frame.id);
            socket
                .send(WsMessage::Text(
                    serde_json::json!({
                        "id": second_open.id,
                        "type": "platform_duplex_opened",
                        "payload": {"ok": true}
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .expect("send second platform duplex opened");
            let second_frame = read_mock_message(&mut socket).await;
            assert_eq!(second_frame.kind, "platform_duplex_client_frame");
            assert_eq!(payload_string(&second_frame.payload, "data"), "second");
            socket
                .send(WsMessage::Text(
                    serde_json::json!({
                        "id": second_frame.id,
                        "type": "platform_duplex_server_frame",
                        "payload": {"frame_type": "text", "data": "second-response"}
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .expect("send second platform server frame");
        });

        let endpoint = Endpoint {
            server_id: String::new(),
            name: "mock".to_string(),
            base_url: format!("http://{address}"),
            supplier_ws_url: format!("ws://{address}/supplier/ws"),
            supplier_quic_url: String::new(),
            enabled: true,
        };
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_static("Bearer sk-test"),
        );
        let pool = PlatformDuplexPool::default();
        let access_token = Arc::new(StdMutex::new("access-token".to_string()));
        let refresh_notify = Arc::new(Notify::new());
        let (session, transport) = open_platform_responses_duplex(
            &pool,
            &endpoint,
            access_token.clone(),
            refresh_notify.clone(),
            headers.clone(),
        )
        .await
        .expect("open platform duplex");
        assert_eq!(transport, "websocket");
        let (sender, mut receiver) = session.split();
        sender
            .send(PlatformDuplexFrame::Text("request".to_string()))
            .await
            .expect("send platform logical frame");
        assert_eq!(
            receiver
                .next()
                .await
                .expect("platform response")
                .expect("valid platform response"),
            PlatformDuplexFrame::Text("response".to_string())
        );

        let (second_session, second_transport) = open_platform_responses_duplex(
            &pool,
            &endpoint,
            access_token,
            refresh_notify,
            headers,
        )
        .await
        .expect("open second logical session on the shared physical connection");
        assert_eq!(second_transport, "websocket");
        let (second_sender, mut second_receiver) = second_session.split();
        second_sender
            .send(PlatformDuplexFrame::Text("second".to_string()))
            .await
            .expect("send second logical frame");
        assert_eq!(
            second_receiver
                .next()
                .await
                .expect("second platform response")
                .expect("valid second platform response"),
            PlatformDuplexFrame::Text("second-response".to_string())
        );
        server.await.expect("mock platform task");
    }

    #[tokio::test]
    async fn platform_websocket_resumes_response_without_replaying_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind resumable platform websocket");
        let address = listener.local_addr().expect("resumable platform address");
        let server = tokio::spawn(async move {
            let (first_stream, _) = listener.accept().await.expect("accept first connection");
            let mut first = tokio_tungstenite::accept_async(first_stream)
                .await
                .expect("upgrade first connection");
            let auth = read_mock_message(&mut first).await;
            first
                .send(WsMessage::Text(
                    serde_json::json!({
                        "id": auth.id,
                        "type": "authenticate_ack",
                        "payload": {
                            "ok": true,
                            "user_id": "user-1",
                            "transport_features": [PLATFORM_DUPLEX_FEATURE, PLATFORM_DUPLEX_RESUME_FEATURE]
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .expect("ack first authentication");
            let open = read_mock_message(&mut first).await;
            first
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
                .expect("ack logical open");
            let request = read_mock_message(&mut first).await;
            assert_eq!(request.kind, "platform_duplex_client_frame");
            let turn_id = payload_string(&request.payload, PLATFORM_DUPLEX_TURN_ID_FIELD);
            assert!(!turn_id.is_empty());
            first
                .send(WsMessage::Text(
                    serde_json::json!({
                        "id": open.id,
                        "type": "platform_duplex_server_frame",
                        "payload": {
                            "frame_type": "text",
                            "data": "{\"type\":\"response.created\"}",
                            "turn_terminal": false,
                            "_const_platform_seq": 1,
                            "_const_duplex_turn_id": turn_id,
                            "_const_duplex_turn_seq": 1
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .expect("send first response frame");
            let ack = read_mock_message(&mut first).await;
            assert_eq!(ack.kind, "platform_duplex_ack");
            assert_eq!(ack.payload.get("sequence").and_then(serde_json::Value::as_u64), Some(1));
            first.close(None).await.expect("close first physical connection");

            let (second_stream, _) = listener.accept().await.expect("accept replacement connection");
            let mut second = tokio_tungstenite::accept_async(second_stream)
                .await
                .expect("upgrade replacement connection");
            let second_auth = read_mock_message(&mut second).await;
            second
                .send(WsMessage::Text(
                    serde_json::json!({
                        "id": second_auth.id,
                        "type": "authenticate_ack",
                        "payload": {
                            "ok": true,
                            "user_id": "user-1",
                            "transport_features": [PLATFORM_DUPLEX_FEATURE, PLATFORM_DUPLEX_RESUME_FEATURE]
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .expect("ack replacement authentication");
            let resume = read_mock_message(&mut second).await;
            assert_eq!(resume.kind, "platform_duplex_resume");
            assert_eq!(payload_string(&resume.payload, "turn_id"), turn_id);
            assert_eq!(resume.payload.get("after_turn_sequence").and_then(serde_json::Value::as_u64), Some(1));
            assert_eq!(resume.payload.get("after_platform_sequence").and_then(serde_json::Value::as_u64), Some(1));
            second
                .send(WsMessage::Text(
                    serde_json::json!({
                        "id": open.id,
                        "type": "platform_duplex_opened",
                        "payload": {"ok": true, "resumed": true, "persisted": true}
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .expect("ack persisted resume");
            second
                .send(WsMessage::Text(
                    serde_json::json!({
                        "id": open.id,
                        "type": "platform_duplex_server_frame",
                        "payload": {
                            "frame_type": "text",
                            "data": "{\"type\":\"response.completed\"}",
                            "turn_terminal": true,
                            "status": 200,
                            "_const_platform_seq": 2,
                            "_const_duplex_turn_id": turn_id,
                            "_const_duplex_turn_seq": 2
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .expect("send resumed terminal frame");
            let terminal_ack = read_mock_message(&mut second).await;
            assert_eq!(terminal_ack.kind, "platform_duplex_ack");
            assert_eq!(terminal_ack.payload.get("sequence").and_then(serde_json::Value::as_u64), Some(2));
        });

        let endpoint = Endpoint {
            server_id: String::new(),
            name: "mock-resume".to_string(),
            base_url: format!("http://{address}"),
            supplier_ws_url: format!("ws://{address}/supplier/ws"),
            supplier_quic_url: String::new(),
            enabled: true,
        };
        let pool = PlatformDuplexPool::default();
        let (session, _) = open_platform_responses_duplex(
            &pool,
            &endpoint,
            Arc::new(StdMutex::new("access-token".to_string())),
            Arc::new(Notify::new()),
            reqwest::header::HeaderMap::new(),
        )
        .await
        .expect("open resumable platform duplex");
        let (sender, mut receiver) = session.split();
        sender
            .send(PlatformDuplexFrame::Text(
                r#"{"type":"response.create","model":"gpt-test","input":"hello"}"#.to_string(),
            ))
            .await
            .expect("send one model request");
        assert_eq!(
            receiver.next().await.expect("created frame").expect("valid created frame"),
            PlatformDuplexFrame::Text(r#"{"type":"response.created"}"#.to_string())
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), receiver.next())
                .await
                .expect("resumed response timeout")
                .expect("completed frame")
                .expect("valid completed frame"),
            PlatformDuplexFrame::Text(r#"{"type":"response.completed"}"#.to_string())
        );
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("mock resume server timeout")
            .expect("mock resume server task");
    }

    #[test]
    fn legacy_server_without_duplex_feature_is_replay_safe() {
        let message: SupplierMessage = serde_json::from_value(serde_json::json!({
                "id": "auth",
                "type": "authenticate_ack",
                "payload": {"ok": true}
            }))
            .expect("legacy authentication response");
        let error = platform_auth_ack(&message)
            .expect_err("legacy server must not claim platform duplex");
        assert!(platform_duplex_error_is_safe_to_replay(&error));
    }
}
