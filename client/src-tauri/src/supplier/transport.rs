impl SupplierRuntime {
    pub(crate) fn status(&self) -> SupplierStatus {
        let status_checked_at_unix = now_unix();
        let transport = self
            .transport_state
            .lock()
            .map(|state| {
                let aggregate = state.aggregate();
                let mut channel_transports = self
                    .channel_agents
                    .values()
                    .flat_map(|handle| {
                        handle.suppliers.iter().map(|supplier| {
                            let unit_id = supplier_unit_id(&handle.client_id, &supplier.channel_id);
                            let node = state.nodes.get(&unit_id);
                            let last_register_ack_unix = node
                                .map(|item| item.last_register_ack_unix)
                                .unwrap_or_default();
                            SupplierChannelTransportStatus {
                                channel_id: supplier.channel_id.clone(),
                                supplier_unit_id: unit_id,
                                connected_transport: node
                                    .map(|item| item.connected_transport.clone())
                                    .unwrap_or_default(),
                                active_transport: node
                                    .map(|item| item.active_transport.clone())
                                    .unwrap_or_default(),
                                last_transport_error: node
                                    .map(|item| item.last_transport_error.clone())
                                    .unwrap_or_default(),
                                last_register_ack_unix,
                                platform_registered: supplier_registration_is_current(
                                    last_register_ack_unix,
                                    status_checked_at_unix,
                                ),
                            }
                        })
                    })
                    .collect::<Vec<_>>();
                channel_transports.sort_by(|a, b| a.channel_id.cmp(&b.channel_id));
                (
                    state.connected_transport(),
                    aggregate.0,
                    aggregate.1,
                    state.route_statuses(),
                    channel_transports,
                    state.channel_health.values().cloned().collect::<Vec<_>>(),
                )
            })
            .unwrap_or_default();
        let mut channels = self.channels.clone();
        for health in transport.5 {
            if let Some(existing) = channels
                .iter_mut()
                .find(|item| item.channel_id == health.channel_id)
            {
                *existing = health;
            } else {
                channels.push(health);
            }
        }
        channels.sort_by(|a, b| a.channel_id.cmp(&b.channel_id));
        SupplierStatus {
            running: self.running,
            starting: self.starting,
            node_id: self.node_id.clone(),
            server_ws_url: self.server_ws_url.clone(),
            server_quic_url: self.server_quic_url.clone(),
            transport_preference: self.transport_preference.clone(),
            connected_transport: transport.0,
            active_transport: transport.1,
            last_transport_error: transport.2,
            nodes: self.nodes.clone(),
            channels,
            last_error: self.last_error.clone(),
            route_statuses: transport.3,
            channel_transports: transport.4,
            availability_groups: None,
        }
    }
}

const SUPPLIER_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
// A control reply may wait behind an ordinary fifteen-second business write.
// Keep a fast liveness probe, with five seconds of scheduling/network margin.
const SUPPLIER_HEARTBEAT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(20);
const SUPPLIER_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
// QUIC keeps this transport-level idle deadline as a final safety net. The
// application ping/pong deadline above detects a dead control tunnel sooner.
const SUPPLIER_TRANSPORT_IDLE_TIMEOUT: Duration = Duration::from_secs(100);
const SUPPLIER_RECONNECT_INTERVAL: Duration = Duration::from_secs(2);
const SUPPLIER_TRANSPORT_CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
const SUPPLIER_TRANSPORT_ERROR_MAX_CHARS: usize = 768;

fn supplier_transport_write_timeout(wire_bytes: usize) -> Duration {
    // Same conservative wire-size budget as the server, including old peers
    // without compression. A stuck writer still has a finite lifetime.
    let extra_ms = wire_bytes.saturating_sub(1 << 20).saturating_mul(1000) / (256 << 10);
    Duration::from_millis(15_000 + (extra_ms as u64).min(105_000))
}

#[derive(Default)]
struct SupplierConnectionActivity {
    received: AtomicBool,
    business_received: AtomicBool,
    business_written: AtomicBool,
}

impl SupplierConnectionActivity {
    fn received(&self, business: bool) {
        self.received.store(true, Ordering::Relaxed);
        if business {
            self.business_received.store(true, Ordering::Relaxed);
        }
    }

    fn written(&self, kind: &str) {
        if !matches!(kind, "ping" | "pong") {
            self.business_written.store(true, Ordering::Relaxed);
        }
    }

    fn take_received(&self) -> bool {
        self.received.swap(false, Ordering::Relaxed)
    }

    fn needs_probe(&self) -> bool {
        // Consume both flags even when only one direction was active. A
        // successful local write alone never proves that the peer is alive.
        let received = self.business_received.swap(false, Ordering::Relaxed);
        let written = self.business_written.swap(false, Ordering::Relaxed);
        !received || !written
    }
}

#[derive(Default)]
struct SupplierHeartbeat {
    id: Option<String>,
    written: bool,
    response_timeout: Duration,
    checked: bool,
}

impl SupplierHeartbeat {
    fn needs_probe(&mut self, activity: &SupplierConnectionActivity) -> bool {
        let idle = activity.needs_probe();
        // Preserve the initial connection check, including heartbeat progress
        // during large registrations. Later ticks can reuse business traffic.
        let initial = !std::mem::replace(&mut self.checked, true);
        initial || idle
    }

    fn pending(&self) -> bool {
        self.id.is_some()
    }

    fn queued(&mut self, id: String) {
        self.id = Some(id);
        self.written = false;
        self.response_timeout = supplier_heartbeat_response_timeout();
    }

    fn written(&mut self, id: &str, response_timeout: Duration) -> bool {
        if self.id.as_deref() != Some(id) || self.written {
            return false;
        }
        self.written = true;
        self.response_timeout = response_timeout;
        true
    }

    fn observe(&mut self, activity: &SupplierConnectionActivity) -> bool {
        if !activity.take_received() {
            return false;
        }
        // Valid business frames (including validated partial messages) are
        // liveness evidence too. Do not require a particular pong while data
        // is still arriving, and do not parse a large payload a second time.
        self.id = None;
        self.written = false;
        true
    }

    fn timeout_detail(&self) -> String {
        if self.written {
            format!(
                "no supplier message received within {} seconds after heartbeat write",
                self.response_timeout.as_secs()
            )
        } else {
            format!(
                "heartbeat was not written within {} seconds; supplier send queue stalled",
                SUPPLIER_TRANSPORT_IDLE_TIMEOUT.as_secs()
            )
        }
    }
}

fn enqueue_supplier_heartbeat(outbound: &SupplierOutbound) -> Result<Option<String>> {
    let message = supplier_heartbeat_message();
    let id = message.id.clone();
    let pending = crate::update_activity::track_supplier_output_before_send();
    match outbound.try_send(message) {
        Ok(()) => {
            pending.transfer(&id);
            Ok(Some(id))
        }
        Err(mpsc::error::TrySendError::Full(_)) => Ok(None),
        Err(mpsc::error::TrySendError::Closed(_)) => Err(anyhow!("supplier transport closed")),
    }
}

enum SupplierQuicReadResult {
    Message(String),
    Closed,
    Failed(String),
}

struct SupplierQuicReadResume(Option<oneshot::Sender<()>>);

impl Drop for SupplierQuicReadResume {
    fn drop(&mut self) {
        if let Some(resume) = self.0.take() {
            let _ = resume.send(());
        }
    }
}

struct SupplierQuicReadEvent {
    result: SupplierQuicReadResult,
    // Reading the next frame waits until the current event has been handled.
    // Besides keeping ordering exact, this lets the authentication handler send
    // its encryption confirmation before another encrypted frame is decoded.
    _resume: SupplierQuicReadResume,
}

async fn run_supplier_quic_reader<R>(
    transport_codec: Arc<SupplierTransportCodec>,
    mut reader: R,
    events: mpsc::Sender<SupplierQuicReadEvent>,
) where
    R: tokio::io::AsyncBufRead + Unpin,
{
    loop {
        let result = match transport_codec.read_quic(&mut reader).await {
            Ok(Some(text)) => SupplierQuicReadResult::Message(text),
            Ok(None) => SupplierQuicReadResult::Closed,
            Err(error) => SupplierQuicReadResult::Failed(compact_supplier_transport_error(
                format!("{error:#}"),
            )),
        };
        let resumable = matches!(result, SupplierQuicReadResult::Message(_));
        let (resume, resumed) = if resumable {
            let (resume, resumed) = oneshot::channel();
            (Some(resume), Some(resumed))
        } else {
            (None, None)
        };
        if events
            .send(SupplierQuicReadEvent {
                result,
                _resume: SupplierQuicReadResume(resume),
            })
            .await
            .is_err()
            || !resumable
        {
            return;
        }
        let Some(resumed) = resumed else {
            return;
        };
        if resumed.await.is_err() {
            return;
        }
    }
}

fn compact_supplier_transport_error(error: impl std::fmt::Display) -> String {
    let normalized = error
        .to_string()
        .replace(['\r', '\n', '\t'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut compact = normalized
        .chars()
        .take(SUPPLIER_TRANSPORT_ERROR_MAX_CHARS)
        .collect::<String>();
    if normalized.chars().count() > SUPPLIER_TRANSPORT_ERROR_MAX_CHARS {
        compact.push_str("...");
    }
    if compact.is_empty() {
        "unknown transport error".to_string()
    } else {
        compact
    }
}

fn supplier_transport_failure_message(
    transport: &str,
    phase: &str,
    error: impl std::fmt::Display,
) -> String {
    format!(
        "{transport} {phase} failed: {}",
        compact_supplier_transport_error(error)
    )
}

fn supplier_websocket_close_detail(message: &WsMessage) -> Option<String> {
    match message {
        WsMessage::Close(Some(frame)) => Some(format!(
            "peer closed websocket code={:?} reason={}",
            frame.code, frame.reason
        )),
        WsMessage::Close(None) => Some("peer closed websocket without a close reason".to_string()),
        _ => None,
    }
}

fn supplier_quic_close_detail(connection: &quinn::Connection, fallback: &str) -> String {
    connection
        .close_reason()
        .map(compact_supplier_transport_error)
        .unwrap_or_else(|| fallback.to_string())
}

fn report_supplier_transport_failure(
    transport_state: &Arc<StdMutex<SupplierTransportSnapshot>>,
    config: &SupplierAgentConfig,
    transport: &str,
    phase: &str,
    error: impl std::fmt::Display,
) {
    let message = supplier_transport_failure_message(transport, phase, error);
    log::warn!(
        "[const-api][supplier] transport failure transport={} phase={} channels={} detail={}",
        transport,
        phase,
        config.suppliers.len(),
        message
    );
    update_supplier_agent_connected_transport_state(transport_state, config, "reconnecting");
    update_supplier_agent_transport_state(transport_state, config, "reconnecting", &message);
}

async fn finish_supplier_transport_failure(
    outbound: SupplierOutbound,
    writer: tokio::task::JoinHandle<()>,
    transport_state: &Arc<StdMutex<SupplierTransportSnapshot>>,
    config: &SupplierAgentConfig,
    transport: &str,
    phase: &str,
    detail: String,
) -> SupplierRunOutcome {
    close_supplier_transport_writer(outbound, writer).await;
    report_supplier_transport_failure(transport_state, config, transport, phase, detail);
    SupplierRunOutcome::Disconnected
}

async fn close_supplier_transport_writer(
    outbound: SupplierOutbound,
    mut writer: tokio::task::JoinHandle<()>,
) {
    drop(outbound);
    if tokio::time::timeout(SUPPLIER_TRANSPORT_CLOSE_TIMEOUT, &mut writer)
        .await
        .is_err()
    {
        writer.abort();
        let _ = writer.await;
    }
}

pub(crate) fn supplier_heartbeat_interval() -> Duration {
    SUPPLIER_HEARTBEAT_INTERVAL
}

pub(crate) fn supplier_heartbeat_response_timeout() -> Duration {
    SUPPLIER_HEARTBEAT_RESPONSE_TIMEOUT
}

pub(crate) fn supplier_connect_timeout() -> Duration {
    SUPPLIER_CONNECT_TIMEOUT
}

pub(crate) fn supplier_transport_idle_timeout() -> Duration {
    SUPPLIER_TRANSPORT_IDLE_TIMEOUT
}

pub(crate) fn supplier_reconnect_interval() -> Duration {
    SUPPLIER_RECONNECT_INTERVAL - Duration::from_millis(500)
        + Duration::from_millis(rand::random::<u64>() % 1001)
}

pub(crate) fn supplier_registration_is_current(last_register_ack_unix: i64, now_unix: i64) -> bool {
    // Registration acknowledgement belongs to the current transport connection. The
    // connection heartbeat owns link health; periodic channel refresh owns channel health.
    last_register_ack_unix > 0 && last_register_ack_unix <= now_unix
}

fn supplier_heartbeat_message() -> SupplierMessage {
    SupplierMessage {
        id: format!("h{:x}", now_unix()),
        kind: "ping".to_string(),
        payload: serde_json::Map::new(),
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct SupplierMessage {
    pub(crate) id: String,
    #[serde(rename = "type")]
    pub(crate) kind: String,
    #[serde(default)]
    pub(crate) payload: serde_json::Map<String, serde_json::Value>,
}

impl serde::Serialize for SupplierMessage {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        // Compact only empty heartbeats. Preserve the exact envelope contract
        // for business/control messages consumed by older server versions.
        let compact = self.payload.is_empty() && matches!(self.kind.as_str(), "ping" | "pong");
        let mut wire = serializer.serialize_struct("SupplierMessage", if compact { 2 } else { 3 })?;
        wire.serialize_field("id", &self.id)?;
        wire.serialize_field("type", &self.kind)?;
        if !compact {
            wire.serialize_field("payload", &self.payload)?;
        }
        wire.end()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SupplierTransportCandidate {
    pub(crate) transport: String,
    pub(crate) url: String,
}

#[derive(Debug, Clone)]
pub(crate) struct SupplierAgentConfig {
    pub(crate) client_id: String,
    pub(crate) session_id: String,
    pub(crate) suppliers: Vec<SupplierConfig>,
    pub(crate) server_ws_url: String,
    pub(crate) server_quic_url: String,
    pub(crate) access_token: Arc<StdMutex<String>>,
    pub(crate) account_refresh_notify: Arc<Notify>,
}

impl SupplierAgentConfig {
    fn with_suppliers(&self, suppliers: Vec<SupplierConfig>) -> Self {
        Self {
            client_id: self.client_id.clone(),
            session_id: self.session_id.clone(),
            suppliers,
            server_ws_url: self.server_ws_url.clone(),
            server_quic_url: self.server_quic_url.clone(),
            access_token: self.access_token.clone(),
            account_refresh_notify: self.account_refresh_notify.clone(),
        }
    }

    #[cfg(test)]
    pub(crate) fn single(supplier: SupplierConfig) -> Self {
        supplier_agent_groups(
            machine_client_id(),
            vec![supplier],
            Arc::new(StdMutex::new(String::new())),
            Arc::new(Notify::new()),
        )
        .into_iter()
        .next()
        .expect("single supplier agent")
    }

    pub(crate) fn primary(&self) -> &SupplierConfig {
        self.suppliers
            .first()
            .expect("supplier agent must have at least one supplier")
    }

    pub(crate) fn channel_ids(&self) -> Vec<String> {
        self.suppliers
            .iter()
            .map(|supplier| supplier.channel_id.clone())
            .collect()
    }

    pub(crate) fn supplier_unit_ids(&self) -> Vec<String> {
        self.suppliers
            .iter()
            .map(|supplier| supplier_unit_id(&self.client_id, &supplier.channel_id))
            .collect()
    }

    pub(crate) fn endpoint_key(&self) -> String {
        format!("{}|{}", self.server_quic_url, self.server_ws_url)
    }

    pub(crate) fn supplier_for_target(
        &self,
        channel_id: &str,
        supplier_unit_id_value: &str,
    ) -> &SupplierConfig {
        let channel_id = channel_id.trim();
        let supplier_unit_id_value = supplier_unit_id_value.trim();
        self.suppliers
            .iter()
            .find(|supplier| {
                (!channel_id.is_empty() && supplier.channel_id == channel_id)
                    || (!supplier_unit_id_value.is_empty()
                        && supplier_unit_id(&self.client_id, &supplier.channel_id)
                            == supplier_unit_id_value)
            })
            .unwrap_or_else(|| self.primary())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SupplierQuicUrl {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) server_name: String,
    pub(crate) cert_sha256: Option<[u8; 32]>,
}

pub(crate) type SupplierOutbound = mpsc::Sender<SupplierMessage>;

pub(crate) const SUPPLIER_STREAM_SEQUENCE_FIELD: &str = "_const_stream_seq";
pub(crate) const SUPPLIER_STREAM_EXECUTION_ID_FIELD: &str = "_const_execution_id";
const SUPPLIER_STREAM_ACK_FIELD: &str = "seq";
const SUPPLIER_STREAM_PROGRESS_FIELD: &str = "received_seq";
const SUPPLIER_TUNNEL_RESUME_WINDOW: Duration = Duration::from_secs(120);
const SUPPLIER_REPLAY_RETRY_INTERVAL: Duration = Duration::from_secs(30);
const SUPPLIER_REPLAY_RETAINED_FRAMES: usize = 256;
// The server dispatches at most 64 queued responses per logical request. Keep
// replay (including the first send) below that limit until delivery progress
// or a durable ACK replenishes the window.
const SUPPLIER_REPLAY_REQUEST_WINDOW: usize = 24;

#[derive(Debug, Clone)]
struct SupplierReplayMessage {
    request_id: String,
    sequence: u64,
    terminal: bool,
    queued_at: Instant,
    sent: bool,
    credited: bool,
    message: SupplierMessage,
}

#[derive(Debug, Default)]
pub(crate) struct SupplierReplayState {
    next_sequence: HashMap<String, u64>,
    execution_ids: HashMap<String, String>,
    acknowledged_sequence: HashMap<String, u64>,
    unacknowledged: VecDeque<SupplierReplayMessage>,
    outstanding: HashMap<String, usize>,
    flow_control_enabled: bool,
    next_unsent: usize,
    blocked: bool,
    last_replay_progress: Option<Instant>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct SupplierReplayMaintenance {
    pub(crate) retried: bool,
    pub(crate) expired_requests: usize,
    pub(crate) expired: Vec<SupplierReplayExpiry>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SupplierReplayExpiry {
    pub(crate) request_id: String,
    pub(crate) execution_id: String,
    pub(crate) turn_id: String,
    pub(crate) first_sequence: u64,
    pub(crate) last_sequence: u64,
    pub(crate) frames: usize,
    pub(crate) oldest_age_ms: u64,
    pub(crate) terminal: bool,
}

fn supplier_replay_diagnostic_id(value: &str) -> &str {
    if !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        value
    } else {
        "-"
    }
}

impl SupplierReplayState {
    pub(crate) fn enqueue(&mut self, mut message: SupplierMessage) {
        let request_id = message.id.clone();
        let execution_id = self
            .execution_ids
            .entry(message.id.clone())
            .or_insert_with(new_supplier_execution_id)
            .clone();
        let sequence = self
            .next_sequence
            .entry(message.id.clone())
            .and_modify(|value| *value = value.saturating_add(1))
            .or_insert(1);
        message.payload.insert(
            SUPPLIER_STREAM_SEQUENCE_FIELD.to_string(),
            serde_json::Value::from(*sequence),
        );
        message.payload.insert(
            SUPPLIER_STREAM_EXECUTION_ID_FIELD.to_string(),
            serde_json::Value::String(execution_id),
        );
        self.unacknowledged.push_back(SupplierReplayMessage {
            request_id: message.id.clone(),
            sequence: *sequence,
            terminal: matches!(
                message.kind.as_str(),
                "http_response" | "stream_end" | "error" | "duplex_close" | "duplex_error"
            ),
            queued_at: Instant::now(),
            sent: false,
            credited: false,
            message,
        });
        crate::update_activity::supplier_output_enqueued_for_replay(&request_id);
    }

    pub(crate) fn reset_for_connection(&mut self) {
        for item in &mut self.unacknowledged {
            item.sent = false;
            item.credited = false;
        }
        self.outstanding.clear();
        self.next_unsent = 0;
        self.blocked = false;
        self.last_replay_progress = None;
    }

    fn has_unsent(&self) -> bool {
        self.unacknowledged.iter().skip(self.next_unsent).any(|item| {
            !item.sent
                && (!self.flow_control_enabled
                    || self.outstanding.get(&item.request_id).copied().unwrap_or(0)
                        < SUPPLIER_REPLAY_REQUEST_WINDOW)
        })
    }

    pub(crate) fn next_unsent(&mut self) -> Option<SupplierMessage> {
        for index in self.next_unsent..self.unacknowledged.len() {
            let item = &mut self.unacknowledged[index];
            if item.sent {
                continue;
            }
            if self.flow_control_enabled {
                let outstanding = self.outstanding.entry(item.request_id.clone()).or_default();
                if *outstanding >= SUPPLIER_REPLAY_REQUEST_WINDOW {
                    self.blocked = true;
                    continue;
                }
                *outstanding += 1;
            }
            item.sent = true;
            self.next_unsent = index + 1;
            let message = item.message.clone();
            if !self.has_unsent() {
                self.last_replay_progress = Some(Instant::now());
            }
            return Some(message);
        }
        None
    }

    pub(crate) fn acknowledge(
        &mut self,
        request_id: &str,
        sequence: u64,
        execution_id: Option<&str>,
    ) {
        if sequence == 0 {
            return;
        }
        if execution_id.is_some_and(|execution_id| {
            self.execution_ids
                .get(request_id)
                .is_none_or(|current| current != execution_id)
        }) {
            return;
        }
        let Some(issued) = self.next_sequence.get(request_id).copied() else {
            return;
        };
        // Keep the cursor scoped to this execution, including legacy ACKs
        // without an execution ID. Never let an oversized ACK cover frames
        // that have not been generated yet.
        let sequence = sequence.min(issued);
        match self.acknowledged_sequence.get_mut(request_id) {
            Some(acknowledged) if sequence <= *acknowledged => return,
            Some(acknowledged) => *acknowledged = sequence,
            None => {
                self.acknowledged_sequence.insert(request_id.to_owned(), sequence);
            }
        }
        let mut removed_before_cursor = 0usize;
        let mut removed = 0usize;
        let mut removed_sent = 0usize;
        let mut terminal_acknowledged = false;
        let next_unsent = self.next_unsent;
        let mut index = 0usize;
        self.unacknowledged.retain(|item| {
            let was_sent = index < next_unsent;
            index += 1;
            if item.request_id == request_id && item.sequence <= sequence {
                removed += 1;
                removed_sent += usize::from(item.sent && !item.credited);
                crate::update_activity::supplier_replay_message_removed(&item.request_id);
                if was_sent {
                    removed_before_cursor += 1;
                }
                terminal_acknowledged |= item.terminal;
                false
            } else {
                true
            }
        });
        if let Some(outstanding) = self.outstanding.get_mut(request_id) {
            *outstanding = outstanding.saturating_sub(removed_sent);
            if *outstanding == 0 {
                self.outstanding.remove(request_id);
            }
        }
        self.next_unsent = if self.blocked { 0 } else { self.next_unsent.saturating_sub(removed_before_cursor) };
        self.blocked = false;
        if removed > 0 {
            self.last_replay_progress = (!self.unacknowledged.is_empty()).then(Instant::now);
            // Retain a small reusable allocation, but do not keep a reconnect
            // backlog's peak capacity for the lifetime of the supplier client.
            let capacity = self.unacknowledged.capacity();
            let length = self.unacknowledged.len();
            if capacity > SUPPLIER_REPLAY_RETAINED_FRAMES && length < capacity / 4 {
                self.unacknowledged.shrink_to((length * 2).max(SUPPLIER_REPLAY_RETAINED_FRAMES));
            }
        }
        if terminal_acknowledged
            && !self
                .unacknowledged
                .iter()
                .any(|item| item.request_id == request_id)
        {
            self.next_sequence.remove(request_id);
            self.execution_ids.remove(request_id);
            self.acknowledged_sequence.remove(request_id);
        }
    }

    fn receive_progress(&mut self, request_id: &str, sequence: u64, execution_id: Option<&str>) {
        if !self.flow_control_enabled || sequence == 0 || execution_id.is_some_and(|execution_id| {
            self.execution_ids.get(request_id).is_none_or(|current| current != execution_id)
        }) {
            return;
        }
        let Some(issued) = self.next_sequence.get(request_id).copied() else {
            return;
        };
        let sequence = sequence.min(issued);
        let mut credited = 0;
        for item in &mut self.unacknowledged {
            if item.request_id == request_id && item.sequence <= sequence && item.sent && !item.credited {
                item.credited = true;
                credited += 1;
            }
        }
        if credited == 0 {
            return;
        }
        if let Some(outstanding) = self.outstanding.get_mut(request_id) {
            *outstanding = outstanding.saturating_sub(credited);
            if *outstanding == 0 {
                self.outstanding.remove(request_id);
            }
        }
        if self.blocked {
            self.next_unsent = 0;
            self.blocked = false;
        }
        self.last_replay_progress = Some(Instant::now());
    }

    pub(crate) fn cancel_execution(&mut self, request_id: &str, execution_id: &str) -> bool {
        if self
            .execution_ids
            .get(request_id)
            .is_none_or(|current| current != execution_id)
        {
            return false;
        }
        let mut removed_before_cursor = 0usize;
        let mut retained = VecDeque::with_capacity(self.unacknowledged.len());
        for (index, item) in self.unacknowledged.drain(..).enumerate() {
            if item.request_id == request_id {
                crate::update_activity::supplier_replay_message_removed(&item.request_id);
                if index < self.next_unsent {
                    removed_before_cursor += 1;
                }
            } else {
                retained.push_back(item);
            }
        }
        self.unacknowledged = retained;
        self.next_unsent = if self.blocked { 0 } else { self.next_unsent.saturating_sub(removed_before_cursor) };
        self.blocked = false;
        self.outstanding.remove(request_id);
        self.next_sequence.remove(request_id);
        self.execution_ids.remove(request_id);
        self.acknowledged_sequence.remove(request_id);
        self.last_replay_progress = (!self.unacknowledged.is_empty()).then(Instant::now);
        true
    }

    pub(crate) fn maintain_unacknowledged(&mut self, now: Instant) -> SupplierReplayMaintenance {
        let expired_request_ids = self
            .unacknowledged
            .iter()
            .map(|item| item.request_id.clone())
            .collect::<HashSet<_>>();
        let expired_request_ids = expired_request_ids
            .into_iter()
            .filter(|request_id| {
                // Never age out a frame that has not yet been handed to the
                // transport writer. The recovery window applies only after
                // every queued frame for this logical request was sent once;
                // otherwise a slow socket could turn replay cleanup into an
                // SSE data-loss path.
                let all_frames_sent = self
                    .unacknowledged
                    .iter()
                    .filter(|item| item.request_id == *request_id)
                    .all(|item| item.sent);
                if !all_frames_sent {
                    return false;
                }
                let terminal_expired = self.unacknowledged.iter().any(|item| {
                    item.request_id == *request_id
                        && item.terminal
                        && now.saturating_duration_since(item.queued_at)
                            >= SUPPLIER_TUNNEL_RESUME_WINDOW
                });
                let abandoned_expired =
                    !crate::update_activity::supplier_request_is_producing(request_id)
                        && self
                            .unacknowledged
                            .iter()
                            .filter(|item| item.request_id == *request_id)
                            .all(|item| {
                                now.saturating_duration_since(item.queued_at)
                                    >= SUPPLIER_TUNNEL_RESUME_WINDOW
                            });
                terminal_expired || abandoned_expired
            })
            .collect::<HashSet<_>>();
        let mut maintenance = SupplierReplayMaintenance {
            expired_requests: expired_request_ids.len(),
            ..SupplierReplayMaintenance::default()
        };
        if !expired_request_ids.is_empty() {
            // Snapshot only correlation IDs and counters before dropping replay.
            // Never copy the response text or arbitrary payload into diagnostics.
            for request_id in &expired_request_ids {
                let frames = self
                    .unacknowledged
                    .iter()
                    .filter(|item| &item.request_id == request_id)
                    .collect::<Vec<_>>();
                if let (Some(first), Some(last)) = (frames.first(), frames.last()) {
                    let turn_id = frames
                        .iter()
                        .rev()
                        .find_map(|item| {
                            item.message
                                .payload
                                .get("_const_duplex_turn_id")
                                .and_then(serde_json::Value::as_str)
                        })
                        .unwrap_or_default();
                    maintenance.expired.push(SupplierReplayExpiry {
                        request_id: supplier_replay_diagnostic_id(request_id).to_string(),
                        execution_id: supplier_replay_diagnostic_id(
                            self.execution_ids
                                .get(request_id)
                                .map(String::as_str)
                                .unwrap_or_default(),
                        )
                        .to_string(),
                        turn_id: supplier_replay_diagnostic_id(turn_id).to_string(),
                        first_sequence: first.sequence,
                        last_sequence: last.sequence,
                        frames: frames.len(),
                        oldest_age_ms: now
                            .saturating_duration_since(first.queued_at)
                            .as_millis()
                            .min(u64::MAX as u128) as u64,
                        terminal: frames.iter().any(|item| item.terminal),
                    });
                }
            }
            let mut removed_before_cursor = 0usize;
            let mut retained = VecDeque::with_capacity(self.unacknowledged.len());
            for (index, item) in self.unacknowledged.drain(..).enumerate() {
                if expired_request_ids.contains(&item.request_id) {
                    crate::update_activity::supplier_replay_message_removed(&item.request_id);
                    if index < self.next_unsent {
                        removed_before_cursor += 1;
                    }
                } else {
                    retained.push_back(item);
                }
            }
            self.unacknowledged = retained;
            self.next_unsent = if self.blocked { 0 } else { self.next_unsent.saturating_sub(removed_before_cursor) };
            self.blocked = false;
            for request_id in expired_request_ids {
                self.outstanding.remove(&request_id);
                self.next_sequence.remove(&request_id);
                self.execution_ids.remove(&request_id);
                self.acknowledged_sequence.remove(&request_id);
            }
            self.last_replay_progress = (!self.unacknowledged.is_empty()).then_some(now);
        }

        if self.unacknowledged.is_empty() || self.has_unsent() {
            return maintenance;
        }
        let Some(last_progress) = self.last_replay_progress else {
            self.last_replay_progress = Some(now);
            return maintenance;
        };
        if now.saturating_duration_since(last_progress) >= SUPPLIER_REPLAY_RETRY_INTERVAL {
            // Stream sequences and execution IDs make this exact replay
            // idempotent. Retrying on the live connection heals a lost ACK or
            // a short server-store outage without redispatching the model call.
            self.reset_for_connection();
            self.last_replay_progress = Some(now);
            maintenance.retried = true;
        }
        maintenance
    }

    #[cfg(test)]
    pub(crate) fn set_last_replay_progress_for_test(&mut self, last_progress: Instant) {
        self.last_replay_progress = Some(last_progress);
    }

    #[cfg(test)]
    pub(crate) fn age_replay_for_test(&mut self, request_id: &str, queued_at: Instant) {
        for item in self
            .unacknowledged
            .iter_mut()
            .filter(|item| item.request_id == request_id)
        {
            item.queued_at = queued_at;
        }
    }

    #[cfg(test)]
    pub(crate) fn pending_replay_count_for_test(&self) -> usize {
        self.unacknowledged.len()
    }

    #[cfg(test)]
    pub(crate) fn has_execution_for_test(&self, request_id: &str) -> bool {
        self.execution_ids.contains_key(request_id)
    }

    fn clear(&mut self) {
        for item in &self.unacknowledged {
            crate::update_activity::supplier_replay_message_removed(&item.request_id);
        }
        self.next_sequence.clear();
        self.execution_ids.clear();
        self.acknowledged_sequence.clear();
        self.unacknowledged.clear();
        self.outstanding.clear();
        self.next_unsent = 0;
        self.blocked = false;
        self.last_replay_progress = None;
    }
}

fn maintain_supplier_replay(replay: &mut SupplierReplayState) {
    let maintenance = replay.maintain_unacknowledged(Instant::now());
    if maintenance.expired_requests > 0 {
        log::warn!(
            "[const-api][supplier] expired {} orphaned replay request(s) after {}s without acknowledgement",
            maintenance.expired_requests,
            SUPPLIER_TUNNEL_RESUME_WINDOW.as_secs(),
        );
        for expired in &maintenance.expired {
            log::warn!(
                "[const-api][supplier] replay_expired request_id={} execution_id={} turn_id={} first_unacked_sequence={} last_unacked_sequence={} frames={} oldest_age_ms={} terminal={} action=discard_unacknowledged_response",
                expired.request_id, expired.execution_id, expired.turn_id,
                expired.first_sequence, expired.last_sequence, expired.frames,
                expired.oldest_age_ms, expired.terminal,
            );
        }
    } else if maintenance.retried {
        log::debug!(
            "[const-api][supplier] retrying unacknowledged response frames on the live transport"
        );
    }
}

fn new_supplier_execution_id() -> String {
    format!("execution-{:032x}", rand::random::<u128>())
}

impl Drop for SupplierReplayState {
    fn drop(&mut self) {
        // Keep update activity ownership tied to the replay buffer itself.
        // An unexpected transport-task exit must not leave a logical request
        // permanently busy and block a later background update.
        self.clear();
    }
}

fn expire_supplier_tunnel_resume_window(
    disconnected_since: &mut Option<Instant>,
    request_tasks: &SupplierRequestTasks,
    replay: &mut SupplierReplayState,
) {
    let Some(since) = *disconnected_since else {
        return;
    };
    if since.elapsed() < SUPPLIER_TUNNEL_RESUME_WINDOW {
        return;
    }
    request_tasks.abort_all();
    replay.clear();
    *disconnected_since = Some(Instant::now());
}

#[cfg(test)]
pub(crate) fn supplier_transport_candidates(
    config: &SupplierConfig,
) -> Vec<SupplierTransportCandidate> {
    supplier_transport_candidates_for_urls(
        &supplier_effective_quic_url(config),
        &config.server_ws_url,
    )
}

pub(crate) fn supplier_agent_transport_candidates(
    config: &SupplierAgentConfig,
) -> Vec<SupplierTransportCandidate> {
    supplier_transport_candidates_for_urls(&config.server_quic_url, &config.server_ws_url)
}

pub(crate) fn supplier_transport_candidates_for_urls(
    server_quic_url: &str,
    server_ws_url: &str,
) -> Vec<SupplierTransportCandidate> {
    let mut out = Vec::new();
    if !server_quic_url.trim().is_empty() {
        out.push(SupplierTransportCandidate {
            transport: "quic".to_string(),
            url: server_quic_url.trim().trim_end_matches('/').to_string(),
        });
    }
    if let Some(server_ws_url) = normalize_supplier_ws_url(server_ws_url) {
        out.push(SupplierTransportCandidate {
            transport: "websocket".to_string(),
            url: server_ws_url,
        });
    }
    out
}

pub(crate) fn supplier_effective_quic_url(config: &SupplierConfig) -> String {
    let explicit = config.server_quic_url.trim().trim_end_matches('/');
    if !explicit.is_empty() {
        return explicit.to_string();
    }
    supplier_quic_url_from_websocket(&config.server_ws_url).unwrap_or_default()
}

pub(crate) fn supplier_quic_url_from_websocket(ws_url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(ws_url.trim()).ok()?;
    if parsed.scheme() != "ws" && parsed.scheme() != "wss" {
        return None;
    }
    let host = parsed.host_str()?.trim();
    if host.is_empty() {
        return None;
    }
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    let port = parsed.port_or_known_default().unwrap_or(443);
    let port_suffix = if port == 443 {
        String::new()
    } else {
        format!(":{port}")
    };
    Some(format!("quic://{host}{port_suffix}/supplier"))
}

pub(crate) fn supplier_agent_groups(
    client_id: String,
    suppliers: Vec<SupplierConfig>,
    access_token: Arc<StdMutex<String>>,
    account_refresh_notify: Arc<Notify>,
) -> Vec<SupplierAgentConfig> {
    let mut groups: Vec<SupplierAgentConfig> = Vec::new();
    for mut supplier in suppliers {
        let server_quic_url = supplier_effective_quic_url(&supplier);
        let server_ws_url = supplier
            .server_ws_url
            .trim()
            .trim_end_matches('/')
            .to_string();
        let server_ws_url = normalize_supplier_ws_url(&server_ws_url).unwrap_or_default();
        supplier.server_ws_url = server_ws_url.clone();
        if let Some(group) = groups.iter_mut().find(|group| {
            group.server_ws_url == server_ws_url && group.server_quic_url == server_quic_url
        }) {
            group.suppliers.push(supplier);
        } else {
            let endpoint_key = format!("{}|{}", server_quic_url, server_ws_url);
            groups.push(SupplierAgentConfig {
                client_id: client_id.clone(),
                session_id: new_supplier_session_id(&client_id, &endpoint_key),
                suppliers: vec![supplier],
                server_ws_url,
                server_quic_url,
                access_token: access_token.clone(),
                account_refresh_notify: account_refresh_notify.clone(),
            });
        }
    }
    groups
}

pub(crate) fn new_supplier_session_id(client_id: &str, endpoint_key: &str) -> String {
    generated_id_from_seed(
        &format!(
            "{}:{}:{}",
            client_id.trim(),
            endpoint_key.trim(),
            now_unix()
        ),
        "session",
    )
}

pub(crate) fn parse_supplier_quic_url(raw: &str) -> Result<SupplierQuicUrl> {
    let parsed = reqwest::Url::parse(raw.trim()).context("invalid supplier QUIC URL")?;
    if parsed.scheme() != "quic" {
        return Err(anyhow!("supplier QUIC URL must use quic://"));
    }
    let host = parsed
        .host_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("supplier QUIC URL is missing host"))?;
    let port = parsed.port().unwrap_or(443);
    let server_name = if host
        .parse::<IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
    {
        "localhost".to_string()
    } else {
        host.clone()
    };
    let cert_sha256 = parsed
        .query_pairs()
        .find(|(key, _)| key == "cert_sha256")
        .map(|(_, value)| parse_quic_cert_sha256(&value))
        .transpose()?;
    Ok(SupplierQuicUrl {
        host,
        port,
        server_name,
        cert_sha256,
    })
}

fn parse_quic_cert_sha256(raw: &str) -> Result<[u8; 32]> {
    let normalized = raw
        .trim()
        .strip_prefix("sha256:")
        .unwrap_or(raw.trim())
        .replace(':', "");
    let decoded = hex::decode(normalized).context("invalid QUIC certificate SHA-256 pin")?;
    decoded
        .try_into()
        .map_err(|_| anyhow!("QUIC certificate SHA-256 pin must contain 32 bytes"))
}

pub(crate) fn supplier_payload_accepts_frame_compression(
    payload: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    supplier_transport_string_array(payload, "transport_features")
        .iter()
        .any(|feature| feature == SUPPLIER_TRANSPORT_FRAME_COMPRESSION_FEATURE)
}

pub(crate) fn supplier_transport_string_array(
    payload: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Vec<String> {
    payload
        .get(key)
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

// zstd_frame_v2 wraps the complete JSON message as magic + two big-endian
// lengths + zstd bytes. The raw JSON is returned unless the final frame wins.
#[cfg(test)]
pub(crate) fn encode_supplier_transport_frame(msg: &SupplierMessage) -> Result<(Vec<u8>, bool)> {
    let raw = serde_json::to_vec(msg)?;
    if raw.len() < SUPPLIER_TRANSPORT_COMPRESSION_MIN_BYTES {
        return Ok((raw, false));
    }
    if raw.len() > SUPPLIER_TRANSPORT_COMPRESSION_MAX_RAW_LEN {
        return Err(anyhow!("supplier message exceeds max size"));
    }
    let compressed = zstd::bulk::compress(&raw, 5)?;
    if compressed.len() + SUPPLIER_TRANSPORT_FRAME_HEADER_BYTES >= raw.len() {
        return Ok((raw, false));
    }
    let mut frame = Vec::with_capacity(SUPPLIER_TRANSPORT_FRAME_HEADER_BYTES + compressed.len());
    frame.extend_from_slice(&SUPPLIER_TRANSPORT_FRAME_MAGIC);
    frame.extend_from_slice(&(raw.len() as u32).to_be_bytes());
    frame.extend_from_slice(&(compressed.len() as u32).to_be_bytes());
    frame.extend_from_slice(&compressed);
    Ok((frame, true))
}

pub(crate) fn decode_supplier_transport_frame(frame: &[u8]) -> Result<SupplierMessage> {
    if frame.len() < SUPPLIER_TRANSPORT_FRAME_HEADER_BYTES
        || frame[..4] != SUPPLIER_TRANSPORT_FRAME_MAGIC
    {
        return Err(anyhow!("invalid compressed supplier frame"));
    }
    let raw_len = u32::from_be_bytes([frame[4], frame[5], frame[6], frame[7]]) as usize;
    let compressed_len = u32::from_be_bytes([frame[8], frame[9], frame[10], frame[11]]) as usize;
    if raw_len == 0 || raw_len > SUPPLIER_TRANSPORT_COMPRESSION_MAX_RAW_LEN {
        return Err(anyhow!("compressed supplier frame has invalid raw size"));
    }
    if compressed_len == 0 || compressed_len != frame.len() - SUPPLIER_TRANSPORT_FRAME_HEADER_BYTES
    {
        return Err(anyhow!(
            "compressed supplier frame has invalid payload size"
        ));
    }
    let raw = zstd::bulk::decompress(&frame[SUPPLIER_TRANSPORT_FRAME_HEADER_BYTES..], raw_len)?;
    if raw.len() != raw_len {
        return Err(anyhow!("compressed supplier frame size mismatch"));
    }
    Ok(serde_json::from_slice(&raw)?)
}

pub(crate) async fn send_supplier_message(
    outbound: &SupplierOutbound,
    msg: SupplierMessage,
) -> Result<()> {
    let pending_supplier_output = crate::update_activity::track_supplier_output_before_send();
    let request_id = msg.id.clone();
    let result = outbound
        .send(msg)
        .await
        .map_err(|_| anyhow!("supplier transport closed"));
    if result.is_ok() {
        pending_supplier_output.transfer(&request_id);
    }
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SupplierRunOutcome {
    Shutdown,
    Disconnected,
}

fn supplier_access_token(config: &SupplierAgentConfig) -> String {
    config
        .access_token
        .lock()
        .map(|token| token.clone())
        .unwrap_or_default()
}

fn supplier_auth_message(config: &SupplierAgentConfig, kind: &str, token: &str) -> SupplierMessage {
    SupplierMessage {
        id: format!("auth-{}", now_unix()),
        kind: kind.to_string(),
        payload: serde_json::json!({
            "access_token": token,
            "device_id": config.client_id,
            "client_id": config.client_id,
            // Negotiate on the small authentication exchange, before the first
            // potentially multi-megabyte registration (also after reconnect).
            "transport_features": [SUPPLIER_TRANSPORT_FRAME_COMPRESSION_FEATURE, SUPPLIER_MESSAGE_CHUNKS_FEATURE],
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    }
}

fn supplier_auth_ack(text: &str) -> Result<bool> {
    let message: SupplierMessage =
        serde_json::from_str(text).context("invalid supplier auth ack")?;
    if message.kind != "authenticate_ack" {
        return Err(anyhow!("unexpected supplier authentication response"));
    }
    Ok(message
        .payload
        .get("ok")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false))
}

pub(crate) fn supplier_register_ack_stream_resume(text: &str) -> Option<bool> {
    let message: SupplierMessage = serde_json::from_str(text).ok()?;
    if message.kind != "register_ack"
        || !message
            .payload
            .get("ok")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    {
        return None;
    }
    Some(
        supplier_transport_string_array(&message.payload, "transport_features")
            .iter()
            .any(|feature| feature == SUPPLIER_TRANSPORT_STREAM_RESUME_FEATURE),
    )
}

pub(crate) fn supplier_register_ack_stream_progress(text: &str) -> Option<bool> {
    let message: SupplierMessage = serde_json::from_str(text).ok()?;
    (message.kind == "register_ack" && message.payload.get("ok")?.as_bool() == Some(true))
        .then(|| {
            supplier_transport_string_array(&message.payload, "transport_features")
                .iter()
                .any(|feature| feature == SUPPLIER_TRANSPORT_STREAM_PROGRESS_FEATURE)
        })
}

fn apply_supplier_stream_resume_negotiation(
    text: &str,
    disconnected_since: &mut Option<Instant>,
    request_tasks: &SupplierRequestTasks,
    replay: &mut SupplierReplayState,
) -> Option<bool> {
    let enabled = supplier_register_ack_stream_resume(text)?;
    replay.flow_control_enabled = enabled && supplier_register_ack_stream_progress(text) == Some(true);
    if !enabled {
        // An older server cannot correlate outputs from a request that belonged
        // to the previous physical connection. Stop those tasks instead of
        // silently emitting orphaned output on the replacement connection.
        if disconnected_since.is_some() {
            request_tasks.abort_all();
        }
        replay.clear();
    }
    *disconnected_since = None;
    Some(enabled)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SupplierRegistrationChannelResult {
    pub(crate) channel_id: String,
    pub(crate) supplier_unit_id: String,
    pub(crate) accepted_models: Vec<String>,
    pub(crate) unsupported_models: Vec<String>,
    pub(crate) fallback_priced_models: Vec<PricingFallbackInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
// Status updates are transient. Boxing the large variant would add allocation
// to the supplier control path without reducing retained memory.
#[allow(clippy::large_enum_variant)]
pub(crate) enum SupplierInboundOutcome {
    None,
    Pong,
    AuthenticationAck {
        accepted: bool,
    },
    AuthenticationRequired,
    RegisterAck {
        registered_count: usize,
        catalog_release_id: String,
        channel_results: Vec<SupplierRegistrationChannelResult>,
    },
    RegistrationRejected {
        reason: String,
        message: String,
    },
    NodeStatusUpdate(SupplierNodeRouteStatus),
    StreamAck {
        request_id: String,
        sequence: u64,
        received_sequence: u64,
        execution_id: Option<String>,
    },
    RequestCancel {
        request_id: String,
        execution_id: String,
    },
    ServerRestarting {
        retry_after_ms: u64,
    },
}

pub(crate) fn handle_supplier_inbound_outcome(
    outcome: SupplierInboundOutcome,
    transport_state: &Arc<StdMutex<SupplierTransportSnapshot>>,
    config: &SupplierAgentConfig,
    active_transport: &str,
) {
    match outcome {
        SupplierInboundOutcome::RegisterAck {
            registered_count,
            catalog_release_id,
            channel_results,
        } => {
            update_supplier_agent_transport_state(transport_state, config, active_transport, "");
            if channel_results.is_empty() && registered_count > 0 {
                for supplier_unit_id_value in config.supplier_unit_ids() {
                    let was_pending =
                        mark_supplier_register_ack(transport_state, &supplier_unit_id_value);
                    let cleared_lifecycle_state = clear_supplier_registration_lifecycle_route_state(
                        transport_state,
                        &supplier_unit_id_value,
                    );
                    if was_pending || cleared_lifecycle_state {
                        log::info!(
                            "[const-api][supplier] channel registration acknowledged supplier_unit_id={}",
                            supplier_unit_id_value
                        );
                    }
                }
            }
            for result in channel_results {
                let supplier_unit_id_value = if result.supplier_unit_id.trim().is_empty() {
                    supplier_unit_id(&config.client_id, &result.channel_id)
                } else {
                    result.supplier_unit_id.clone()
                };
                let was_pending =
                    mark_supplier_register_ack(transport_state, &supplier_unit_id_value);
                let cleared_lifecycle_state = clear_supplier_registration_lifecycle_route_state(
                    transport_state,
                    &supplier_unit_id_value,
                );
                if was_pending || cleared_lifecycle_state {
                    log::info!(
                        "[const-api][supplier] channel registration acknowledged channel_id={} supplier_unit_id={}",
                        result.channel_id,
                        supplier_unit_id_value
                    );
                }
                if result.accepted_models.is_empty() {
                    update_supplier_model_admission_route_state(
                        transport_state,
                        SupplierNodeRouteStatus {
                            channel_id: result.channel_id,
                            supplier_unit_id: supplier_unit_id_value,
                            state: "degraded".to_string(),
                            reason: "model_not_supported".to_string(),
                            message: format!(
                                "server catalog rejected {} observed model(s)",
                                result.unsupported_models.len()
                            ),
                            accepted_models: result.accepted_models,
                            unsupported_models: result.unsupported_models,
                            fallback_priced_models: result.fallback_priced_models,
                            catalog_release_id: catalog_release_id.clone(),
                            ..Default::default()
                        },
                    );
                } else if !result.unsupported_models.is_empty() {
                    update_supplier_model_admission_route_state(
                        transport_state,
                        SupplierNodeRouteStatus {
                            channel_id: result.channel_id,
                            supplier_unit_id: supplier_unit_id_value,
                            state: "degraded".to_string(),
                            reason: "models_partially_supported".to_string(),
                            message: format!(
                                "server catalog accepted {} and rejected {} observed model(s)",
                                result.accepted_models.len(),
                                result.unsupported_models.len()
                            ),
                            accepted_models: result.accepted_models,
                            unsupported_models: result.unsupported_models,
                            fallback_priced_models: result.fallback_priced_models,
                            catalog_release_id: catalog_release_id.clone(),
                            ..Default::default()
                        },
                    );
                } else if !result.fallback_priced_models.is_empty() {
                    update_supplier_model_admission_route_state(
                        transport_state,
                        SupplierNodeRouteStatus {
                            channel_id: result.channel_id,
                            supplier_unit_id: supplier_unit_id_value,
                            state: "degraded".to_string(),
                            reason: "pricing_fallback".to_string(),
                            message: format!(
                                "{} observed model(s) use reviewed fallback pricing",
                                result.fallback_priced_models.len()
                            ),
                            accepted_models: result.accepted_models,
                            fallback_priced_models: result.fallback_priced_models,
                            catalog_release_id: catalog_release_id.clone(),
                            ..Default::default()
                        },
                    );
                } else {
                    clear_supplier_model_admission_route_state(
                        transport_state,
                        &supplier_unit_id_value,
                    );
                }
            }
        }
        SupplierInboundOutcome::RegistrationRejected { reason, message } => {
            log::warn!(
                "[const-api][supplier] channel registration rejected reason={} message={}",
                reason,
                message
            );
        }
        SupplierInboundOutcome::NodeStatusUpdate(mut status) => {
            // Older servers reported catalog admission as an operational fallback. A channel can
            // be registered while the active catalog currently accepts none of its models, so
            // keep that condition in the non-blocking admission-status domain.
            if status.reason == "model_unsupported" {
                status.state = "degraded".to_string();
                status.reason = "model_not_supported".to_string();
            }
            if matches!(
                status.reason.as_str(),
                "model_not_supported" | "models_partially_supported" | "pricing_fallback"
            ) {
                update_supplier_model_admission_route_state(transport_state, status);
            } else {
                if status.state == "ready" && status.reason == "catalog_updated" {
                    let key = if status.supplier_unit_id.trim().is_empty() {
                        status.node_id.clone()
                    } else {
                        status.supplier_unit_id.clone()
                    };
                    clear_supplier_model_admission_route_state(transport_state, &key);
                }
                update_supplier_route_state(transport_state, status);
            }
        }
        SupplierInboundOutcome::AuthenticationAck { .. }
        | SupplierInboundOutcome::AuthenticationRequired
        | SupplierInboundOutcome::StreamAck { .. }
        | SupplierInboundOutcome::RequestCancel { .. }
        | SupplierInboundOutcome::ServerRestarting { .. }
        | SupplierInboundOutcome::Pong
        | SupplierInboundOutcome::None => {}
    }
}

fn mark_supplier_register_ack(
    transport_state: &Arc<StdMutex<SupplierTransportSnapshot>>,
    supplier_unit_id: &str,
) -> bool {
    let supplier_unit_id = supplier_unit_id.trim();
    if supplier_unit_id.is_empty() {
        return false;
    }
    if let Ok(mut snapshot) = transport_state.lock() {
        let node = snapshot
            .nodes
            .entry(supplier_unit_id.to_string())
            .or_default();
        let was_pending = node.last_register_ack_unix <= 0;
        node.last_register_ack_unix = now_unix();
        return was_pending;
    }
    false
}

pub(crate) fn handle_supplier_connection_outcome(
    outcome: SupplierInboundOutcome,
    config: &SupplierAgentConfig,
    registrations: &SupplierRegistrationDispatcher,
    transport_state: &Arc<StdMutex<SupplierTransportSnapshot>>,
    active_transport: &str,
    registration_authorized: &mut bool,
) {
    match outcome {
        SupplierInboundOutcome::AuthenticationAck { accepted } => {
            *registration_authorized = accepted;
            if accepted {
                let _ = registrations.publish(config.clone(), Duration::ZERO, true);
            } else if !supplier_access_token(config).is_empty() {
                config.account_refresh_notify.notify_one();
            }
        }
        SupplierInboundOutcome::AuthenticationRequired => {
            *registration_authorized = false;
            // The preceding register was rejected rather than accepted. It must not
            // suppress the identical registration sent after authentication succeeds.
            registrations.invalidate();
            if !supplier_access_token(config).is_empty() {
                config.account_refresh_notify.notify_one();
            }
        }
        SupplierInboundOutcome::RegisterAck { .. } => {
            *registration_authorized = true;
            handle_supplier_inbound_outcome(outcome, transport_state, config, active_transport);
        }
        SupplierInboundOutcome::RegistrationRejected { .. } => {
            // Keep the healthy transport connected, but allow the next monitor/recovery tick to
            // retry the identical registration instead of suppressing it via signature dedup.
            registrations.invalidate();
            handle_supplier_inbound_outcome(outcome, transport_state, config, active_transport);
        }
        other => {
            handle_supplier_inbound_outcome(other, transport_state, config, active_transport);
        }
    }
}

pub(crate) async fn run_supplier_agent(
    mut config: SupplierAgentConfig,
    mut shutdown: oneshot::Receiver<()>,
    mut register_update: mpsc::Receiver<SupplierRegistrationUpdate>,
    transport_state: Arc<StdMutex<SupplierTransportSnapshot>>,
) { log::error!("Hosted supply is unavailable in the local edition"); }

#[cfg(test)]
#[allow(dead_code)]
pub(crate) async fn run_supplier_websocket_connection(
    client: &Client,
    config: &mut SupplierAgentConfig,
    ws_url: &str,
    shutdown: &mut oneshot::Receiver<()>,
    register_update: &mut mpsc::Receiver<SupplierRegistrationUpdate>,
    transport_state: &Arc<StdMutex<SupplierTransportSnapshot>>,
) -> SupplierRunOutcome {
    let (request_outbound, mut request_outbound_rx) = mpsc::channel::<SupplierMessage>(256);
    let request_tasks = SupplierRequestTasks::default();
    let _request_task_guard = SupplierRequestTaskGuard::new(request_tasks.clone());
    let mut replay = SupplierReplayState::default();
    let mut disconnected_since = None;
    run_supplier_websocket_connection_with_runtime(
        client,
        config,
        ws_url,
        shutdown,
        register_update,
        transport_state,
        &request_outbound,
        &mut request_outbound_rx,
        &request_tasks,
        &mut replay,
        &mut disconnected_since,
    )
    .await
}

async fn run_supplier_websocket_connection_with_runtime(
    client: &Client,
    config: &mut SupplierAgentConfig,
    ws_url: &str,
    shutdown: &mut oneshot::Receiver<()>,
    register_update: &mut mpsc::Receiver<SupplierRegistrationUpdate>,
    transport_state: &Arc<StdMutex<SupplierTransportSnapshot>>,
    request_outbound: &SupplierOutbound,
    request_outbound_rx: &mut mpsc::Receiver<SupplierMessage>,
    request_tasks: &SupplierRequestTasks,
    replay: &mut SupplierReplayState,
    disconnected_since: &mut Option<Instant>,
) -> SupplierRunOutcome { SupplierRunOutcome::Shutdown }

#[cfg(test)]
pub(crate) async fn run_supplier_quic_connection(
    client: &Client,
    config: &mut SupplierAgentConfig,
    quic_url: &str,
    shutdown: &mut oneshot::Receiver<()>,
    register_update: &mut mpsc::Receiver<SupplierRegistrationUpdate>,
    transport_state: &Arc<StdMutex<SupplierTransportSnapshot>>,
) -> SupplierRunOutcome {
    let (request_outbound, mut request_outbound_rx) = mpsc::channel::<SupplierMessage>(256);
    let request_tasks = SupplierRequestTasks::default();
    let _request_task_guard = SupplierRequestTaskGuard::new(request_tasks.clone());
    let mut replay = SupplierReplayState::default();
    let mut disconnected_since = None;
    run_supplier_quic_connection_with_runtime(
        client,
        config,
        quic_url,
        shutdown,
        register_update,
        transport_state,
        &request_outbound,
        &mut request_outbound_rx,
        &request_tasks,
        &mut replay,
        &mut disconnected_since,
    )
    .await
}

async fn run_supplier_quic_connection_with_runtime(
    client: &Client,
    config: &mut SupplierAgentConfig,
    quic_url: &str,
    shutdown: &mut oneshot::Receiver<()>,
    register_update: &mut mpsc::Receiver<SupplierRegistrationUpdate>,
    transport_state: &Arc<StdMutex<SupplierTransportSnapshot>>,
    request_outbound: &SupplierOutbound,
    request_outbound_rx: &mut mpsc::Receiver<SupplierMessage>,
    request_tasks: &SupplierRequestTasks,
    replay: &mut SupplierReplayState,
    disconnected_since: &mut Option<Instant>,
) -> SupplierRunOutcome { SupplierRunOutcome::Shutdown }

pub(crate) fn supplier_register_min_interval_for_update(
    updated_suppliers: &[SupplierConfig],
) -> Duration {
    if updated_suppliers.is_empty() {
        Duration::ZERO
    } else {
        Duration::from_secs(2)
    }
}

pub(crate) fn apply_supplier_registration_update(
    current: &mut SupplierAgentConfig,
    update: SupplierRegistrationUpdate,
) -> (SupplierAgentConfig, Duration) {
    let min_interval = supplier_register_min_interval_for_update(&update.suppliers);
    let register_config = current.with_suppliers(update.suppliers.clone());
    merge_supplier_registration_update(&mut current.suppliers, update.suppliers);
    (register_config, min_interval)
}

pub(crate) fn merge_supplier_registration_update(
    current: &mut Vec<SupplierConfig>,
    updated: Vec<SupplierConfig>,
) {
    if updated.is_empty() {
        current.clear();
        return;
    }
    for supplier in updated {
        current.retain(|item| item.channel_id != supplier.channel_id);
        if !supplier.registration_removed {
            current.push(supplier);
        }
    }
}

pub(crate) async fn connect_supplier_quic_endpoint(
    quic_url: &str,
) -> Result<(quinn::Endpoint, quinn::Connection)> { anyhow::bail!("Hosted supply is unavailable in the local edition") }

pub(crate) async fn handle_supplier_inbound_text_guarded(
    client: &Client,
    config: &SupplierAgentConfig,
    outbound: &SupplierOutbound,
    request_outbound: &SupplierOutbound,
    request_tasks: &SupplierRequestTasks,
    text: &str,
    scope: &'static str,
) -> SupplierInboundOutcome {
    match catch_runtime_panic(
        scope,
        handle_supplier_inbound_text_with_tasks(
            client,
            config,
            outbound,
            request_outbound,
            Some(request_tasks),
            text,
        ),
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(_) => SupplierInboundOutcome::None,
    }
}

#[cfg(test)]
pub(crate) async fn handle_supplier_inbound_text(
    client: &Client,
    config: &SupplierAgentConfig,
    outbound: &SupplierOutbound,
    text: &str,
) -> SupplierInboundOutcome {
    handle_supplier_inbound_text_with_tasks(client, config, outbound, outbound, None, text).await
}

async fn handle_supplier_inbound_text_with_tasks(
    client: &Client,
    config: &SupplierAgentConfig,
    outbound: &SupplierOutbound,
    request_outbound: &SupplierOutbound,
    request_tasks: Option<&SupplierRequestTasks>,
    text: &str,
) -> SupplierInboundOutcome {
    if let Ok(message) = serde_json::from_str::<SupplierMessage>(text) {
        if message.kind == "authenticate_ack" {
            return SupplierInboundOutcome::AuthenticationAck {
                accepted: message
                    .payload
                    .get("ok")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false),
            };
        }
        if message.kind == "register_ack" {
            if message
                .payload
                .get("ok")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
            {
                return SupplierInboundOutcome::RegisterAck {
                    registered_count: message
                        .payload
                        .get("registered_count")
                        .and_then(|value| value.as_u64())
                        .unwrap_or_default() as usize,
                    catalog_release_id: payload_string(&message.payload, "catalog_release_id"),
                    channel_results: supplier_registration_channel_results(&message.payload),
                };
            }
            if payload_string(&message.payload, "reason") == "auth_required" {
                return SupplierInboundOutcome::AuthenticationRequired;
            }
            let reason = payload_string(&message.payload, "reason");
            return SupplierInboundOutcome::RegistrationRejected {
                reason: if reason.is_empty() {
                    "registration_rejected".to_string()
                } else {
                    reason
                },
                message: payload_string(&message.payload, "message"),
            };
        }
        if message.kind == "node_status_update" {
            return SupplierInboundOutcome::NodeStatusUpdate(
                supplier_node_route_status_from_payload(&message.payload),
            );
        }
        if message.kind == "pong" {
            return SupplierInboundOutcome::Pong;
        }
        if message.kind == "stream_ack" {
            return SupplierInboundOutcome::StreamAck {
                request_id: message.id,
                sequence: message
                    .payload
                    .get(SUPPLIER_STREAM_ACK_FIELD)
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or_default(),
                received_sequence: message
                    .payload
                    .get(SUPPLIER_STREAM_PROGRESS_FIELD)
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or_default(),
                execution_id: message
                    .payload
                    .get(SUPPLIER_STREAM_EXECUTION_ID_FIELD)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
            };
        }
        if message.kind == "server_restarting" {
            return SupplierInboundOutcome::ServerRestarting {
                retry_after_ms: message
                    .payload
                    .get("retry_after_ms")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(2_000),
            };
        }
        if message.kind == "ping" {
            let pong = SupplierMessage {
                id: message.id,
                kind: "pong".to_string(),
                payload: serde_json::Map::new(),
            };
            let _ = send_supplier_message(outbound, pong).await;
            return SupplierInboundOutcome::None;
        }
        if message.kind == "request_cancel" {
            if let Some(execution_id) = message
                .payload
                .get(SUPPLIER_STREAM_EXECUTION_ID_FIELD)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                return SupplierInboundOutcome::RequestCancel {
                    request_id: message.id,
                    execution_id: execution_id.to_string(),
                };
            }
            // Legacy and ordinary requester cancellation is request-scoped.
            // Keep its behavior unchanged for old servers and old clients.
            if let Some(tasks) = request_tasks {
                tasks.cancel(&message.id);
            }
            return SupplierInboundOutcome::None;
        }
        if message.kind == "duplex_client_frame" || message.kind == "duplex_close" {
            let delivered = match request_tasks {
                Some(tasks) => tasks.send_duplex(message.clone()).await,
                None => false,
            };
            if !delivered {
                let response = SupplierMessage {
                    id: message.id,
                    kind: "duplex_error".to_string(),
                    payload: serde_json::json!({
                        "error_kind": "duplex_session_not_found",
                        "failure_scope": "request",
                        "error": {
                            "type": "duplex_session_not_found",
                            "code": "duplex_session_not_found",
                            "message": "supplier duplex session is not active"
                        }
                    })
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
                };
                let _ = send_supplier_message(request_outbound, response).await;
            }
            return SupplierInboundOutcome::None;
        }
        if message.kind == "duplex_open" {
            let target_channel_id = message
                .payload
                .get("target_channel_id")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let target_supplier_unit_id = message
                .payload
                .get("target_supplier_unit_id")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let supplier = config
                .supplier_for_target(target_channel_id, target_supplier_unit_id)
                .clone();
            if let Some(tasks) = request_tasks {
                tasks.spawn_duplex(client.clone(), supplier, message, request_outbound.clone());
            }
            return SupplierInboundOutcome::None;
        }
        if message.kind == "http_request" {
            let target_channel_id = message
                .payload
                .get("target_channel_id")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let target_supplier_unit_id = message
                .payload
                .get("target_supplier_unit_id")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let supplier = config
                .supplier_for_target(target_channel_id, target_supplier_unit_id)
                .clone();
            if let Some(tasks) = request_tasks {
                tasks.spawn(client.clone(), supplier, message, request_outbound.clone());
            } else {
                execute_supplier_request_and_report(client, &supplier, message, request_outbound)
                    .await;
            }
        }
    }
    SupplierInboundOutcome::None
}

fn supplier_node_route_status_from_payload(
    payload: &serde_json::Map<String, serde_json::Value>,
) -> SupplierNodeRouteStatus {
    SupplierNodeRouteStatus {
        node_id: payload_string(payload, "node_id"),
        channel_id: payload_string(payload, "channel_id"),
        supplier_unit_id: payload_string(payload, "supplier_unit_id"),
        state: payload_string(payload, "state"),
        reason: payload_string(payload, "reason"),
        message: payload_string(payload, "message"),
        protocol: payload_string(payload, "protocol"),
        target_protocol: payload_string(payload, "target_protocol"),
        model: payload_string(payload, "model"),
        feature: payload_string(payload, "feature"),
        issue_code: payload_string(payload, "issue_code"),
        accepted_models: payload_string_array(payload, "accepted_models"),
        unsupported_models: payload_string_array(payload, "unsupported_models"),
        fallback_priced_models: payload_pricing_fallbacks(payload, "fallback_priced_models"),
        catalog_release_id: payload_string(payload, "catalog_release_id"),
        cooldown_until_unix: payload_i64(payload, "cooldown_until_unix"),
        updated_at_unix: payload_i64(payload, "updated_at_unix"),
    }
}

fn supplier_registration_channel_results(
    payload: &serde_json::Map<String, serde_json::Value>,
) -> Vec<SupplierRegistrationChannelResult> {
    payload
        .get("channel_results")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_object())
        .map(|result| SupplierRegistrationChannelResult {
            channel_id: payload_string(result, "channel_id"),
            supplier_unit_id: payload_string(result, "supplier_unit_id"),
            accepted_models: payload_string_array(result, "accepted_models"),
            unsupported_models: payload_string_array(result, "unsupported_models"),
            fallback_priced_models: payload_pricing_fallbacks(result, "fallback_priced_models"),
        })
        .collect()
}

fn payload_pricing_fallbacks(
    payload: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Vec<PricingFallbackInfo> {
    payload
        .get(key)
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_object())
        .map(|value| PricingFallbackInfo {
            model: payload_string(value, "model"),
            provider: payload_string(value, "provider"),
            billing_model: payload_string(value, "billing_model"),
            pricing_rule_id: payload_string(value, "pricing_rule_id"),
            confidence: payload_string(value, "confidence"),
            price_version_id: payload_string(value, "price_version_id"),
        })
        .filter(|value| !value.model.is_empty())
        .collect()
}

fn payload_string_array(
    payload: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Vec<String> {
    payload
        .get(key)
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn payload_string(payload: &serde_json::Map<String, serde_json::Value>, key: &str) -> String {
    payload
        .get(key)
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn payload_i64(payload: &serde_json::Map<String, serde_json::Value>, key: &str) -> i64 {
    payload
        .get(key)
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_u64().and_then(|raw| i64::try_from(raw).ok()))
        })
        .unwrap_or_default()
}

#[derive(Debug)]
pub(crate) struct NoCertificateVerification;

impl rustls::client::danger::ServerCertVerifier for NoCertificateVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls_pki_types::CertificateDer<'_>,
        _intermediates: &[rustls_pki_types::CertificateDer<'_>],
        _server_name: &rustls_pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls_pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls_pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls_pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
        ]
    }
}

#[derive(Debug)]
pub(crate) struct PinnedCertificateVerification {
    pub(crate) cert_sha256: [u8; 32],
}

impl rustls::client::danger::ServerCertVerifier for PinnedCertificateVerification {
    fn verify_server_cert(
        &self,
        end_entity: &rustls_pki_types::CertificateDer<'_>,
        _intermediates: &[rustls_pki_types::CertificateDer<'_>],
        _server_name: &rustls_pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls_pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let actual: [u8; 32] = Sha256::digest(end_entity.as_ref()).into();
        if actual != self.cert_sha256 {
            return Err(rustls::Error::General(
                "supplier QUIC certificate pin mismatch".to_string(),
            ));
        }
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls_pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls_pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

pub(crate) fn supplier_quic_allows_insecure_cert(target: &SupplierQuicUrl) -> bool {
    if target.host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    target
        .host
        .parse::<IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

pub(crate) fn rustls_client_config_for_supplier_quic(
    target: &SupplierQuicUrl,
) -> rustls::ClientConfig {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut crypto = if supplier_quic_allows_insecure_cert(target) {
        rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoCertificateVerification))
            .with_no_client_auth()
    } else if let Some(cert_sha256) = target.cert_sha256 {
        rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinnedCertificateVerification {
                cert_sha256,
            }))
            .with_no_client_auth()
    } else {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth()
    };
    crypto.alpn_protocols = vec![SUPPLIER_QUIC_ALPN.to_vec()];
    crypto
}

pub(crate) fn quic_client_config_for_target(
    target: &SupplierQuicUrl,
) -> Result<quinn::ClientConfig> {
    let crypto = rustls_client_config_for_supplier_quic(target);
    let quic_crypto = quinn::crypto::rustls::QuicClientConfig::try_from(crypto)?;
    let mut config = quinn::ClientConfig::new(Arc::new(quic_crypto));
    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(Some(supplier_transport_idle_timeout().try_into()?));
    config.transport_config(Arc::new(transport));
    Ok(config)
}

#[cfg(test)]
mod recovery_tests {
    use super::*;

    fn replay_test_message(id: &str, kind: &str) -> SupplierMessage {
        SupplierMessage { id: id.into(), kind: kind.into(), payload: Default::default() }
    }

    #[test]
    fn replay_duplicate_ack_keeps_storage_and_does_not_postpone_recovery() {
        let mut replay = SupplierReplayState::default();
        replay.flow_control_enabled = true;
        for _ in 0..4096 {
            replay.enqueue(replay_test_message("ack-storm", "stream_chunk"));
        }
        while replay.next_unsent().is_some() {}
        replay.acknowledge("ack-storm", 1, None);
        assert_eq!(
            replay.next_unsent().unwrap().payload[SUPPLIER_STREAM_SEQUENCE_FIELD],
            SUPPLIER_REPLAY_REQUEST_WINDOW + 1
        );
        assert!(replay.next_unsent().is_none());
        let storage = replay.unacknowledged.as_slices().0.as_ptr();
        let capacity = replay.unacknowledged.capacity();
        let now = Instant::now();
        let progress = now - SUPPLIER_REPLAY_RETRY_INTERVAL;
        replay.last_replay_progress = Some(progress);
        for _ in 0..5000 {
            replay.acknowledge("ack-storm", 1, None);
            assert_eq!(replay.unacknowledged.as_slices().0.as_ptr(), storage);
        }
        assert_eq!(replay.unacknowledged.capacity(), capacity);
        assert_eq!(replay.unacknowledged.len(), 4095);
        assert_eq!(replay.last_replay_progress, Some(progress));
        assert!(replay.maintain_unacknowledged(now).retried);
        assert_eq!(replay.next_unsent().unwrap().payload[SUPPLIER_STREAM_SEQUENCE_FIELD], 2);
        replay.reset_for_connection();
        replay.acknowledge("ack-storm", 1, None);
        assert_eq!(replay.next_unsent().unwrap().payload[SUPPLIER_STREAM_SEQUENCE_FIELD], 2);
        replay.clear();
    }

    #[test]
    fn replay_ack_preserves_interleaved_unsent_frames_and_execution_identity() {
        let mut replay = SupplierReplayState::default();
        for id in ["ack-a", "ack-b", "ack-a", "ack-b"] {
            replay.enqueue(replay_test_message(id, "stream_chunk"));
        }
        for _ in 0..3 { replay.next_unsent().unwrap(); }
        replay.acknowledge("ack-a", 1, None);
        assert_eq!(replay.next_unsent, 2);
        let before = replay.unacknowledged.len();
        replay.acknowledge("ack-a", 2, Some("another-execution"));
        replay.acknowledge("unknown-request", u64::MAX, None);
        assert_eq!(replay.unacknowledged.len(), before);
        assert_eq!(replay.acknowledged_sequence.len(), 1);
        replay.acknowledge("ack-b", 1, None);
        let unsent = replay.next_unsent().unwrap();
        assert_eq!(unsent.id, "ack-b");
        assert_eq!(unsent.payload[SUPPLIER_STREAM_SEQUENCE_FIELD], 2);
        assert!(replay.next_unsent().is_none());
        replay.reset_for_connection();
        let ids: Vec<_> = std::iter::from_fn(|| replay.next_unsent()).map(|m| m.id).collect();
        assert_eq!(ids, ["ack-a", "ack-b"]);

        let old_execution = replay.execution_ids["ack-a"].clone();
        replay.enqueue(replay_test_message("ack-a", "stream_end"));
        replay.acknowledge("ack-a", 3, Some(&old_execution));
        assert!(!replay.acknowledged_sequence.contains_key("ack-a"));
        replay.enqueue(replay_test_message("ack-a", "stream_chunk"));
        replay.acknowledge("ack-a", 3, Some(&old_execution));
        assert!(replay.unacknowledged.iter().any(|m| m.request_id == "ack-a" && m.sequence == 1));
        replay.acknowledge("ack-a", 1, None);
        assert!(!replay.unacknowledged.iter().any(|m| m.request_id == "ack-a"));
        replay.clear();
    }

    #[test]
    fn replay_ack_cursor_never_covers_future_frames_and_is_cleaned_up() {
        for cleanup in ["cancel", "expire", "clear"] {
            let id = format!("ack-cleanup-{cleanup}");
            let mut replay = SupplierReplayState::default();
            replay.enqueue(replay_test_message(&id, "stream_chunk"));
            replay.acknowledge(&id, u64::MAX, None);
            assert_eq!(replay.acknowledged_sequence[&id], 1);
            replay.enqueue(replay_test_message(&id, "stream_chunk"));
            replay.acknowledge(&id, 2, None);
            assert!(replay.unacknowledged.is_empty(), "an old oversized ACK must not hide new ACKs");
            replay.enqueue(replay_test_message(&id, "stream_chunk"));
            match cleanup {
                "cancel" => {
                    let execution = replay.execution_ids[&id].clone();
                    assert!(replay.cancel_execution(&id, &execution));
                }
                "expire" => {
                    while replay.next_unsent().is_some() {}
                    let now = Instant::now();
                    replay.age_replay_for_test(&id, now - SUPPLIER_TUNNEL_RESUME_WINDOW);
                    assert_eq!(replay.maintain_unacknowledged(now).expired_requests, 1);
                }
                _ => replay.clear(),
            }
            assert!(replay.acknowledged_sequence.is_empty());
            replay.acknowledge(&id, 99, None);
            assert!(replay.acknowledged_sequence.is_empty());
        }
    }

    #[test]
    fn replay_ack_releases_peak_backlog_capacity_without_reordering() {
        let mut replay = SupplierReplayState::default();
        replay.flow_control_enabled = true;
        for _ in 0..8192 {
            replay.enqueue(replay_test_message("ack-shrink", "stream_chunk"));
        }
        while replay.next_unsent().is_some() {}
        replay.acknowledge("ack-shrink", 8092, None);
        assert_eq!(replay.unacknowledged.len(), 100);
        assert_eq!(replay.next_unsent, 0);
        assert!(replay.unacknowledged.capacity() <= SUPPLIER_REPLAY_RETAINED_FRAMES);
        replay.reset_for_connection();
        for sequence in 8093..=8192 {
            if !replay.has_unsent() {
                replay.acknowledge("ack-shrink", sequence - 1, None);
            }
            assert_eq!(replay.next_unsent().unwrap().payload[SUPPLIER_STREAM_SEQUENCE_FIELD], sequence);
        }
        replay.acknowledge("ack-shrink", 8192, None);
        replay.enqueue(replay_test_message("ack-shrink", "stream_end"));
        assert_eq!(replay.next_unsent().unwrap().kind, "stream_end");
        replay.acknowledge("ack-shrink", 8193, None);
        assert!(replay.unacknowledged.is_empty());
        assert!(replay.acknowledged_sequence.is_empty());
        assert!(replay.unacknowledged.capacity() <= SUPPLIER_REPLAY_RETAINED_FRAMES);
    }

    #[test]
    fn replay_window_bounds_each_request_without_blocking_other_requests() {
        let mut replay = SupplierReplayState::default();
        replay.flow_control_enabled = true;
        for _ in 0..80 {
            replay.enqueue(replay_test_message("slow", "stream_chunk"));
        }
        for _ in 0..3 {
            replay.enqueue(replay_test_message("fast", "stream_chunk"));
        }
        for sequence in 1..=SUPPLIER_REPLAY_REQUEST_WINDOW {
            let message = replay.next_unsent().unwrap();
            assert_eq!(message.id, "slow");
            assert_eq!(message.payload[SUPPLIER_STREAM_SEQUENCE_FIELD], sequence);
        }
        for sequence in 1..=3 {
            let message = replay.next_unsent().unwrap();
            assert_eq!(message.id, "fast");
            assert_eq!(message.payload[SUPPLIER_STREAM_SEQUENCE_FIELD], sequence);
        }
        assert!(!replay.has_unsent());
        replay.acknowledge("slow", 12, None);
        for sequence in (SUPPLIER_REPLAY_REQUEST_WINDOW + 1)..=(SUPPLIER_REPLAY_REQUEST_WINDOW + 12) {
            let message = replay.next_unsent().unwrap();
            assert_eq!(message.id, "slow");
            assert_eq!(message.payload[SUPPLIER_STREAM_SEQUENCE_FIELD], sequence);
        }
        assert!(!replay.has_unsent());
        replay.clear();
    }

    #[test]
    fn replay_progress_reopens_window_without_discarding_undurable_frames() {
        let mut replay = SupplierReplayState::default();
        replay.flow_control_enabled = true;
        for _ in 0..48 {
            replay.enqueue(replay_test_message("progress", "stream_chunk"));
        }
        for _ in 0..SUPPLIER_REPLAY_REQUEST_WINDOW {
            replay.next_unsent().unwrap();
        }
        assert!(!replay.has_unsent());
        replay.receive_progress("progress", 16, None);
        assert_eq!(replay.unacknowledged.len(), 48);
        assert_eq!(replay.outstanding["progress"], SUPPLIER_REPLAY_REQUEST_WINDOW - 16);
        for sequence in 25..=40 {
            assert_eq!(replay.next_unsent().unwrap().payload[SUPPLIER_STREAM_SEQUENCE_FIELD], sequence);
        }
        assert!(!replay.has_unsent());
        replay.acknowledge("progress", 16, None);
        assert_eq!(replay.unacknowledged.len(), 32);
        assert_eq!(replay.outstanding["progress"], SUPPLIER_REPLAY_REQUEST_WINDOW);
        replay.receive_progress("progress", 32, None);
        assert_eq!(replay.next_unsent().unwrap().payload[SUPPLIER_STREAM_SEQUENCE_FIELD], 41);
    }

    #[test]
    fn old_server_does_not_enable_flow_window() {
        let mut replay = SupplierReplayState::default();
        for _ in 0..80 {
            replay.enqueue(replay_test_message("legacy", "stream_chunk"));
        }
        for _ in 0..80 {
            replay.next_unsent().unwrap();
        }
        assert!(!replay.has_unsent());
    }

    #[tokio::test]
    async fn quic_reader_preserves_fragmented_mixed_frames_under_load() {
        let (reader, mut writer) = tokio::io::duplex(4 * 1024);
        let (events_tx, mut events_rx) = mpsc::channel(1);
        let reader_task = tokio::spawn(run_supplier_quic_reader(
            Arc::new(SupplierTransportCodec::default()),
            BufReader::new(reader),
            events_tx,
        ));

        let mut wire = Vec::new();
        let mut expected = Vec::new();
        let mut compressed = 0;
        for index in 0..512 {
            let body = if index % 2 == 0 {
                format!("small-{index}")
            } else {
                format!("large-{index}-{}", "fragmented-payload-".repeat(256))
            };
            let message = SupplierMessage {
                id: format!("request-{index}"),
                kind: "http_request".to_string(),
                payload: serde_json::json!({"body": body})
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
            };
            let (mut frame, binary) =
                encode_supplier_transport_frame(&message).expect("encode mixed QUIC frame");
            if binary {
                compressed += 1;
            } else {
                frame.push(b'\n');
            }
            wire.extend_from_slice(&frame);
            expected.push(message);
        }
        assert!(compressed > 0 && compressed < expected.len());

        let writer_task = tokio::spawn(async move {
            let mut offset = 0;
            while offset < wire.len() {
                let chunk_len = ((offset % 31) + 1).min(wire.len() - offset);
                writer
                    .write_all(&wire[offset..offset + chunk_len])
                    .await
                    .expect("write fragmented QUIC bytes");
                offset += chunk_len;
                tokio::task::yield_now().await;
            }
            writer
                .shutdown()
                .await
                .expect("close fragmented QUIC writer");
        });

        for expected_message in expected {
            let event = tokio::time::timeout(Duration::from_secs(5), events_rx.recv())
                .await
                .expect("QUIC reader event timed out")
                .expect("QUIC reader stopped before all frames");
            // Hold the acknowledgement while unrelated main-loop work wins.
            // The reader must keep ownership of the partial frame and resume at
            // the next frame boundary afterwards.
            tokio::task::yield_now().await;
            let SupplierQuicReadEvent { result, _resume } = event;
            let SupplierQuicReadResult::Message(text) = result else {
                panic!("QUIC reader ended before all mixed frames");
            };
            let actual: SupplierMessage =
                serde_json::from_str(&text).expect("decode complete QUIC message");
            assert_eq!(actual.id, expected_message.id);
            assert_eq!(actual.kind, expected_message.kind);
            assert_eq!(actual.payload, expected_message.payload);
        }

        let terminal = tokio::time::timeout(Duration::from_secs(5), events_rx.recv())
            .await
            .expect("QUIC terminal event timed out")
            .expect("QUIC reader omitted terminal event");
        assert!(matches!(terminal.result, SupplierQuicReadResult::Closed));
        writer_task.await.expect("fragmented QUIC writer task");
        reader_task.await.expect("QUIC reader task");
    }

    #[test]
    fn heartbeat_accepts_valid_business_and_legacy_control_messages() {
        for text in [
            r#"{"id":"old-heartbeat","type":"pong","payload":{}}"#,
            r#"{"id":"h1","type":"pong"}"#,
            r#"{"id":"h1","type":"register_ack","payload":{"ok":false,"reason":"auth_required"}}"#,
            r#"{"id":"request-1","type":"stream_ack","payload":{"sequence":1}}"#,
        ] {
            let codec = SupplierTransportCodec::default();
            let mut heartbeat = SupplierHeartbeat::default();
            heartbeat.queued("h1".into());
            assert!(codec.decode_websocket(WsMessage::Text(text.into())).unwrap().is_some());
            assert!(heartbeat.observe(&codec.activity));
            assert!(!heartbeat.pending());
            assert!(!heartbeat.written("h1", supplier_heartbeat_response_timeout()));
        }
        let codec = SupplierTransportCodec::default();
        let mut heartbeat = SupplierHeartbeat::default();
        heartbeat.queued("h1".into());
        assert!(codec.decode_websocket(WsMessage::Text("not JSON".into())).is_err());
        assert!(!heartbeat.observe(&codec.activity));
        assert!(heartbeat.pending());
    }

    #[test]
    fn heartbeat_idle_ticks_require_two_way_business_progress() {
        let activity = SupplierConnectionActivity::default();
        let mut heartbeat = SupplierHeartbeat::default();
        assert!(heartbeat.needs_probe(&activity));
        activity.received(true);
        activity.written("stream_chunk");
        assert!(!heartbeat.needs_probe(&activity));
        assert!(heartbeat.needs_probe(&activity));
        activity.received(true);
        assert!(heartbeat.needs_probe(&activity), "receive-only traffic still needs a ping");
        activity.written("stream_chunk");
        assert!(heartbeat.needs_probe(&activity), "local writes do not prove peer liveness");
        activity.received(false);
        activity.written("ping");
        assert!(heartbeat.needs_probe(&activity), "idle ping/pong must not halve the heartbeat frequency");
    }

    #[test]
    fn heartbeat_packet_and_timing_keep_the_existing_contract() {
        let message = supplier_heartbeat_message();
        let wire = serde_json::to_string(&message).unwrap();
        assert!(wire.len() < 40);
        assert!(!wire.contains("payload"));
        let decoded: SupplierMessage = serde_json::from_str(&wire).unwrap();
        assert_eq!(decoded.id, message.id);
        assert_eq!(decoded.kind, "ping");
        assert!(decoded.payload.is_empty());
        let mut business = decoded;
        business.kind = "request_cancel".into();
        assert!(serde_json::to_value(business).unwrap()["payload"].is_object());
        assert_eq!(supplier_heartbeat_interval(), Duration::from_secs(30));
        assert_eq!(supplier_heartbeat_response_timeout(), supplier_transport_write_timeout(64) + Duration::from_secs(5));
        assert!(supplier_heartbeat_response_timeout() < supplier_transport_idle_timeout());
        for _ in 0..100 {
            let delay = supplier_reconnect_interval();
            assert!((Duration::from_millis(1500)..=Duration::from_millis(2500)).contains(&delay));
        }
    }

    #[test]
    fn supplier_transport_failure_keeps_compact_underlying_error() {
        let message = supplier_transport_failure_message(
            "websocket",
            "read",
            "IO error: connection reset by peer\r\nVPN path changed",
        );

        assert_eq!(
            message,
            "websocket read failed: IO error: connection reset by peer VPN path changed"
        );
    }
}
