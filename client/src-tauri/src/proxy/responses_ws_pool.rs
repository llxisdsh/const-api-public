use std::sync::atomic::Ordering;

const RESPONSES_WS_POOL_LANES_PER_CONNECTION: usize = 8;
const RESPONSES_WS_POOL_MAX_TARGET_CONNECTIONS: usize = 3;
const RESPONSES_WS_POOL_MAX_CONNECTIONS: usize = 4;
// No event-count cutoff: token deltas can arrive in large, cheap bursts.
// Allocate on demand and apply socket backpressure at this shared byte budget,
// rather than closing all lanes when one consumer is temporarily slow.
const RESPONSES_WS_POOL_EVENT_BUFFER_BYTES: usize = crate::output_buffer::OUTPUT_BUFFER_BYTES;
const RESPONSES_WS_POOL_WRITE_QUEUE: usize = 64;
const RESPONSES_WS_POOL_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(not(test))]
const RESPONSES_WS_POOL_EXCLUSIVE_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(test)]
const RESPONSES_WS_POOL_EXCLUSIVE_IDLE_TIMEOUT: Duration = Duration::from_millis(100);
const RESPONSES_WS_POOL_DRAIN_AGE: Duration = Duration::from_secs(50 * 60);
const RESPONSES_WS_POOL_HARD_AGE: Duration = Duration::from_secs(60 * 60);

const RESPONSES_WS_CAPABILITY_PROBING: u8 = 0;
const RESPONSES_WS_CAPABILITY_MULTIPLEX: u8 = 1;
const RESPONSES_WS_CAPABILITY_EXCLUSIVE: u8 = 2;

const RESPONSES_WS_PUBLIC_SHARED_HANDSHAKE_HEADERS: &[&str] = &[
    "originator",
    "user-agent",
    "version",
    "openai-beta",
    "openai-organization",
    "openai-project",
    "x-responsesapi-include-timing-metrics",
];

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum ResponsesWsPoolProtocolMode {
    ConfirmedMultiplex,
    ProbeSubscriptionMultiplex,
    Exclusive,
}

#[derive(Clone, Default)]
pub(crate) struct ResponsesWsPool {
    inner: Arc<ResponsesWsPoolInner>,
}

#[derive(Default)]
struct ResponsesWsPoolInner {
    buckets: tokio::sync::Mutex<HashMap<ResponsesWsPoolKey, Arc<ResponsesWsPoolBucket>>>,
    capabilities:
        tokio::sync::Mutex<HashMap<ResponsesWsCapabilityKey, Arc<ResponsesWsCapabilityState>>>,
    shutting_down: std::sync::atomic::AtomicBool,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct ResponsesWsPoolKey {
    channel_id: String,
    credential_ref: String,
    endpoint: String,
    handshake_fingerprint: String,
    protocol_mode: ResponsesWsPoolProtocolMode,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct ResponsesWsCapabilityKey {
    channel_id: String,
    credential_ref: String,
    endpoint: String,
    beta: String,
    protocol_mode: ResponsesWsPoolProtocolMode,
}

struct ResponsesWsCapabilityState {
    value: std::sync::atomic::AtomicU8,
    notify: tokio::sync::Notify,
    probe_lock: tokio::sync::Mutex<()>,
}

#[derive(Clone)]
struct ResponsesWsConnectContext {
    client: Client,
    route: ResponsesWebsocketNativeRoute,
    headers: warp::http::HeaderMap,
    channel_id: String,
    request_source: String,
}

struct ResponsesWsPoolBucket {
    state: tokio::sync::Mutex<ResponsesWsPoolBucketState>,
    notify: tokio::sync::Notify,
    capability: Arc<ResponsesWsCapabilityState>,
    target_connections: std::sync::atomic::AtomicUsize,
    hard_connections: std::sync::atomic::AtomicUsize,
    exclusive_connections: std::sync::atomic::AtomicUsize,
    open_connections: std::sync::atomic::AtomicUsize,
    context: ResponsesWsConnectContext,
}

#[derive(Default)]
struct ResponsesWsPoolBucketState {
    connections: Vec<Arc<ResponsesWsPooledConnection>>,
    connecting: usize,
}

struct ResponsesWsPooledConnection {
    id: String,
    bucket: std::sync::Weak<ResponsesWsPoolBucket>,
    writer: tokio::sync::mpsc::Sender<ResponsesWsConnectionCommand>,
    event_budget: Arc<tokio::sync::Semaphore>,
    slots: std::sync::Mutex<Vec<ResponsesWsLaneSlot>>,
    diagnostics: std::sync::Mutex<UpstreamWebsocketLease>,
    dial_ms: u64,
    draining: std::sync::atomic::AtomicBool,
    broken: std::sync::atomic::AtomicBool,
    turns_started: std::sync::atomic::AtomicU64,
    session_pins: std::sync::atomic::AtomicUsize,
    idle_generation: std::sync::atomic::AtomicU64,
    burst: bool,
}

struct ResponsesWsLaneSlot {
    generation: u64,
    sender: Option<tokio::sync::mpsc::UnboundedSender<ResponsesWsQueuedEvent>>,
    turn_active: bool,
    expects_stream_id: bool,
}

struct ResponsesWsQueuedEvent {
    message: std::result::Result<tokio_tungstenite::tungstenite::Message, String>,
    // Released when consumed or dropped. Terminal transport errors bypass the
    // budget so a full data queue can never hide why the connection ended.
    _budget: Option<tokio::sync::OwnedSemaphorePermit>,
}

fn responses_ws_event_cost(message: &tokio_tungstenite::tungstenite::Message) -> u32 {
    // Include queue overhead so tiny frames cannot create unbounded allocations.
    // One already-decoded frame may exceed the budget; allow it through alone,
    // subject to the WebSocket decoder's existing message-size limit.
    message.len().saturating_add(std::mem::size_of::<ResponsesWsQueuedEvent>() + 32)
        .min(RESPONSES_WS_POOL_EVENT_BUFFER_BYTES) as u32
}

enum ResponsesWsConnectionCommand {
    Frame(
        tokio_tungstenite::tungstenite::Message,
        tokio::sync::oneshot::Sender<std::result::Result<(), String>>,
    ),
    Close(&'static str),
}

struct ResponsesWsLaneLease {
    connection: Arc<ResponsesWsPooledConnection>,
    lane: usize,
    generation: u64,
    receiver: tokio::sync::mpsc::UnboundedReceiver<ResponsesWsQueuedEvent>,
    released: bool,
}

pub(crate) struct ResponsesWsPoolSession {
    bucket: Arc<ResponsesWsPoolBucket>,
    preferred: Option<Arc<ResponsesWsPooledConnection>>,
    preferred_lane: Option<usize>,
    connection_reused: bool,
    acquire_ms: u64,
    continuation_replays: u64,
}

pub(crate) struct ResponsesWsPoolTurn {
    lease: Option<ResponsesWsLaneLease>,
    stream_id: Option<String>,
}

#[derive(Debug)]
struct ResponsesWsContinuationConnectionLost;

impl std::fmt::Display for ResponsesWsContinuationConnectionLost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(
            "Responses WebSocket continuation connection is no longer available",
        )
    }
}

impl std::error::Error for ResponsesWsContinuationConnectionLost {}

fn responses_ws_pool_error_requires_full_replay(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<ResponsesWsContinuationConnectionLost>()
        .is_some()
}

impl ResponsesWsPool {
    async fn prepare_session(
        &self,
        client: &Client,
        route: &ResponsesWebsocketNativeRoute,
        channel: &ChannelConfig,
        client_headers: &warp::http::HeaderMap,
        request_source: &str,
    ) -> Result<ResponsesWsPoolSession> {
        if self.inner.shutting_down.load(Ordering::Acquire) {
            return Err(anyhow!("Responses WebSocket pool is shutting down"));
        }
        let shared_headers = responses_ws_pool_shared_headers(route, client_headers);
        let protocol_mode = responses_ws_pool_protocol_mode(route, channel);
        let endpoint = responses_ws_pool_endpoint(route)?;
        let credential_ref = channel.v2.credential_ref.clone();
        let beta = shared_headers
            .get("openai-beta")
            .and_then(|value| value.to_str().ok())
            .unwrap_or(RESPONSES_WEBSOCKET_BETA)
            .to_string();
        let capability = {
            let mut capabilities = self.inner.capabilities.lock().await;
            capabilities
                .entry(ResponsesWsCapabilityKey {
                    channel_id: channel.id.clone(),
                    credential_ref: credential_ref.clone(),
                    endpoint: endpoint.clone(),
                    beta,
                    protocol_mode,
                })
                .or_insert_with(|| {
                    Arc::new(ResponsesWsCapabilityState {
                        value: std::sync::atomic::AtomicU8::new(match protocol_mode {
                            ResponsesWsPoolProtocolMode::ConfirmedMultiplex => {
                                RESPONSES_WS_CAPABILITY_MULTIPLEX
                            }
                            ResponsesWsPoolProtocolMode::ProbeSubscriptionMultiplex => {
                                RESPONSES_WS_CAPABILITY_PROBING
                            }
                            ResponsesWsPoolProtocolMode::Exclusive => {
                                RESPONSES_WS_CAPABILITY_EXCLUSIVE
                            }
                        }),
                        notify: tokio::sync::Notify::new(),
                        probe_lock: tokio::sync::Mutex::new(()),
                    })
                })
                .clone()
        };
        let key = ResponsesWsPoolKey {
            channel_id: channel.id.clone(),
            credential_ref,
            endpoint,
            handshake_fingerprint: responses_ws_pool_header_fingerprint(&shared_headers),
            protocol_mode,
        };
        let (target_connections, hard_connections) =
            responses_ws_pool_dimensions(channel.max_concurrency());
        let exclusive_connections = responses_ws_pool_exclusive_limit(
            channel.max_concurrency(),
        );
        let bucket = {
            let mut buckets = self.inner.buckets.lock().await;
            buckets.retain(|_, bucket| {
                bucket.open_connections.load(Ordering::Acquire) != 0
                    || Arc::strong_count(bucket) > 1
            });
            buckets
                .entry(key)
                .or_insert_with(|| {
                    log::info!(
                        "[const-api][upstream-ws-pool] configured channel_id={} pool=true protocol_mode={:?} lanes_per_connection={} target_connections={} hard_connections={}",
                        channel.id,
                        protocol_mode,
                        RESPONSES_WS_POOL_LANES_PER_CONNECTION,
                        target_connections,
                        hard_connections,
                    );
                    Arc::new(ResponsesWsPoolBucket {
                        state: tokio::sync::Mutex::new(ResponsesWsPoolBucketState::default()),
                        notify: tokio::sync::Notify::new(),
                        capability: capability.clone(),
                        target_connections: std::sync::atomic::AtomicUsize::new(
                            target_connections,
                        ),
                        hard_connections: std::sync::atomic::AtomicUsize::new(hard_connections),
                        exclusive_connections: std::sync::atomic::AtomicUsize::new(
                            exclusive_connections,
                        ),
                        open_connections: std::sync::atomic::AtomicUsize::new(0),
                        context: ResponsesWsConnectContext {
                            client: client.clone(),
                            route: route.clone(),
                            headers: shared_headers.clone(),
                            channel_id: channel.id.clone(),
                            request_source: request_source.to_string(),
                        },
                    })
                })
                .clone()
        };
        bucket
            .target_connections
            .store(target_connections, Ordering::Release);
        bucket
            .hard_connections
            .store(hard_connections, Ordering::Release);
        bucket
            .exclusive_connections
            .store(exclusive_connections, Ordering::Release);

        // Establish or reuse a physical connection now so duplex_opened keeps
        // its existing meaning. The lane is immediately returned; idle logical
        // sessions do not consume multiplex capacity.
        let acquire_started = std::time::Instant::now();
        let mut lease = bucket.acquire(None, None, false).await?;
        let preferred = lease.connection.clone();
        let preferred_lane = Some(lease.lane);
        let connection_reused = !lease.connection_was_created();
        lease.release_idle();
        preferred.pin_session();
        Ok(ResponsesWsPoolSession {
            bucket,
            preferred: Some(preferred),
            preferred_lane,
            connection_reused,
            acquire_ms: duration_millis_u64(acquire_started.elapsed()),
            continuation_replays: 0,
        })
    }

    pub(crate) fn shutdown(&self) {
        if self.inner.shutting_down.swap(true, Ordering::AcqRel) {
            return;
        }
        let inner = self.inner.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let buckets = {
                    let mut buckets = inner.buckets.lock().await;
                    std::mem::take(&mut *buckets)
                };
                inner.capabilities.lock().await.clear();
                for bucket in buckets.into_values() {
                    bucket.close_all("pool_shutdown").await;
                }
            });
        }
    }
}

impl ResponsesWsPoolSession {
    pub(crate) async fn start_turn(&mut self, text: &str) -> Result<ResponsesWsPoolTurn> {
        let acquire_started = std::time::Instant::now();
        let require_affinity = serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|value| {
                value
                    .get("previous_response_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
            })
            .is_some();
        if !require_affinity
            && self.bucket.capability.value.load(Ordering::Acquire)
                == RESPONSES_WS_CAPABILITY_PROBING
        {
            self.probe_stream_id_capability(text).await?;
        }
        let preferred = self.preferred.as_ref();
        let mut lease = self
            .bucket
            .acquire(preferred, self.preferred_lane, require_affinity)
            .await?;
        let stream_id = (self.bucket.capability.value.load(Ordering::Acquire)
            == RESPONSES_WS_CAPABILITY_MULTIPLEX)
            .then(|| lease.stream_id());
        let outbound = if let Some(stream_id) = &stream_id {
            responses_ws_pool_add_stream_id(text, stream_id.clone())?
        } else {
            text.to_string()
        };
        lease.start_turn(outbound).await?;
        self.connection_reused = !lease.connection_was_created();
        self.acquire_ms = duration_millis_u64(acquire_started.elapsed());
        self.preferred_lane = Some(lease.lane);
        self.replace_preferred(lease.connection.clone());
        Ok(ResponsesWsPoolTurn {
            lease: Some(lease),
            stream_id,
        })
    }

    async fn probe_stream_id_capability(&mut self, text: &str) -> Result<()> {
        let capability = self.bucket.capability.clone();
        let _probe_guard = capability.probe_lock.lock().await;
        if self.bucket.capability.value.load(Ordering::Acquire)
            != RESPONSES_WS_CAPABILITY_PROBING
        {
            return Ok(());
        }
        let preferred = self.preferred.as_ref();
        let mut lease = match self
            .bucket
            .acquire(preferred, self.preferred_lane, false)
            .await
        {
            Ok(lease) => lease,
            Err(error) => {
                self.bucket
                    .fallback_stream_id_capability("probe_acquire_failed", &error);
                return Ok(());
            }
        };
        self.preferred_lane = Some(lease.lane);
        let probe = match responses_ws_pool_probe_request(text)
            .and_then(|probe| responses_ws_pool_add_stream_id(&probe, lease.stream_id()))
        {
            Ok(probe) => probe,
            Err(error) => {
                self.bucket
                    .fallback_stream_id_capability("probe_encode_failed", &error);
                lease.cancel("pool_capability_probe_encode_failed");
                return Ok(());
            }
        };
        if let Err(error) = lease.start_turn(probe).await {
            self.bucket
                .fallback_stream_id_capability("probe_send_failed", &error);
            lease.cancel("pool_capability_probe_send_failed");
            return Ok(());
        }
        self.replace_preferred(lease.connection.clone());
        let result = tokio::time::timeout(RESPONSES_WS_POOL_WAIT_TIMEOUT, async {
            loop {
                let message = lease
                    .receiver
                    .recv()
                    .await
                    .ok_or_else(|| anyhow!("Responses WebSocket capability probe ended early"))?
                    .message
                    .map_err(anyhow::Error::msg)?;
                match message {
                    tokio_tungstenite::tungstenite::Message::Text(text) => {
                        let Some(status) = responses_websocket_terminal_status(text.as_ref()) else {
                            continue;
                        };
                        return Ok(status);
                    }
                    tokio_tungstenite::tungstenite::Message::Close(_) => {
                        return Err(anyhow!(
                            "Responses WebSocket closed during stream_id capability probe"
                        ));
                    }
                    _ => {}
                }
            }
        })
        .await;
        match result {
            Ok(Ok(_status)) => {
                if self.bucket.capability.value.load(Ordering::Acquire)
                    == RESPONSES_WS_CAPABILITY_PROBING
                {
                    self.bucket.fallback_stream_id_capability(
                        "probe_returned_no_capability_evidence",
                        &anyhow!("probe terminal event did not establish stream_id capability"),
                    );
                }
                lease.finish();
            }
            Ok(Err(error)) => {
                self.bucket
                    .fallback_stream_id_capability("probe_read_failed", &error);
                lease.cancel("pool_capability_probe_read_failed");
            }
            Err(_) => {
                let error = anyhow!("Responses WebSocket stream_id capability probe timed out");
                self.bucket
                    .fallback_stream_id_capability("probe_timeout", &error);
                lease.cancel("pool_capability_probe_timeout");
            }
        }
        Ok(())
    }

    fn replace_preferred(&mut self, connection: Arc<ResponsesWsPooledConnection>) {
        if self
            .preferred
            .as_ref()
            .is_some_and(|preferred| Arc::ptr_eq(preferred, &connection))
        {
            return;
        }
        connection.pin_session();
        if let Some(previous) = self.preferred.replace(connection) {
            previous.unpin_session();
        }
    }

    fn mark_continuation_replay(&mut self) {
        self.continuation_replays = self.continuation_replays.saturating_add(1);
    }

    pub(crate) fn attach(&self, payload: &mut serde_json::Map<String, serde_json::Value>) {
        if let Some(connection) = self.preferred.as_ref() {
            connection.attach(payload);
        }
        payload.insert("upstream_ws_pool".to_string(), true.into());
        if payload.get("failure_stage").and_then(serde_json::Value::as_str) == Some("upstream_connect") {
            return; // Acquire/reuse timings belong to the prior successful turn.
        }
        payload.insert(
            "upstream_ws_connection_reused".to_string(),
            self.connection_reused.into(),
        );
        payload.insert("upstream_ws_pool_acquire_ms".to_string(), self.acquire_ms.into());
        payload.insert(
            "upstream_ws_continuation_replays".to_string(),
            self.continuation_replays.into(),
        );
        payload.insert(
            "upstream_ws_pool_mode".to_string(),
            self.bucket.capability_label().into(),
        );
        payload.insert(
            "upstream_ws_pool_target_connections".to_string(),
            (self.bucket.target_connections.load(Ordering::Acquire) as u64).into(),
        );
        payload.insert(
            "upstream_ws_pool_hard_connections".to_string(),
            (self.bucket.hard_connections.load(Ordering::Acquire) as u64).into(),
        );
        payload.insert(
            "upstream_ws_pool_exclusive_connections".to_string(),
            (self.bucket.exclusive_connections.load(Ordering::Acquire) as u64).into(),
        );
    }

    fn log_terminal(&self, metadata: &ResponsesWebsocketTerminalMetadata) {
        if let Some(connection) = self.preferred.as_ref() {
            connection.log_terminal(metadata);
        }
    }
}

impl Drop for ResponsesWsPoolSession {
    fn drop(&mut self) {
        if let Some(connection) = self.preferred.take() {
            connection.unpin_session();
        }
    }
}

impl ResponsesWsPoolTurn {
    async fn interrupt(&self, text: &str) -> Result<()> {
        let lease = self
            .lease
            .as_ref()
            .ok_or_else(|| anyhow!("Responses WebSocket pooled turn is finished"))?;
        let text = if let Some(stream_id) = &self.stream_id {
            responses_ws_pool_add_stream_id(text, stream_id.clone())?
        } else {
            text.to_owned()
        };
        lease.send_frame(text).await
    }

    pub(crate) async fn next(
        &mut self,
    ) -> Option<Result<tokio_tungstenite::tungstenite::Message>> {
        let lease = self.lease.as_mut()?;
        lease.receiver.recv().await.map(|event| event.message.map_err(anyhow::Error::msg))
    }

    pub(crate) fn finish(mut self) {
        if let Some(mut lease) = self.lease.take() {
            lease.finish();
        }
    }

    pub(crate) fn cancel(mut self, reason: &'static str) {
        if let Some(mut lease) = self.lease.take() {
            lease.cancel(reason);
        }
    }
}

impl ResponsesWsPoolBucket {
    fn capability_label(&self) -> &'static str {
        match self.capability.value.load(Ordering::Acquire) {
            RESPONSES_WS_CAPABILITY_PROBING => "probing",
            RESPONSES_WS_CAPABILITY_MULTIPLEX => "multiplex",
            _ => "exclusive_pool",
        }
    }

    fn observe_stream_id_capability(&self, stream_id_present: bool) {
        let next = if stream_id_present {
            RESPONSES_WS_CAPABILITY_MULTIPLEX
        } else {
            RESPONSES_WS_CAPABILITY_EXCLUSIVE
        };
        if self
            .capability
            .value
            .compare_exchange(
                RESPONSES_WS_CAPABILITY_PROBING,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            log::info!(
                "[const-api][upstream-ws-pool] capability channel_id={} mode={}",
                self.context.channel_id,
                self.capability_label(),
            );
            self.capability.notify.notify_waiters();
            self.notify.notify_waiters();
        }
    }

    fn fallback_stream_id_capability(&self, reason: &'static str, error: &anyhow::Error) {
        if self
            .capability
            .value
            .compare_exchange(
                RESPONSES_WS_CAPABILITY_PROBING,
                RESPONSES_WS_CAPABILITY_EXCLUSIVE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            log::warn!(
                "[const-api][upstream-ws-pool] capability_probe_fallback channel_id={} mode=exclusive_pool reason={} error={error:#}",
                self.context.channel_id,
                reason,
            );
            self.capability.notify.notify_waiters();
            self.notify.notify_waiters();
        }
    }

    async fn acquire(
        self: &Arc<Self>,
        preferred: Option<&Arc<ResponsesWsPooledConnection>>,
        preferred_lane: Option<usize>,
        require_affinity: bool,
    ) -> Result<ResponsesWsLaneLease> {
        let deadline = tokio::time::Instant::now() + RESPONSES_WS_POOL_WAIT_TIMEOUT;
        loop {
            let notified = self.notify.notified();
            let capability_notified = self.capability.notify.notified();
            let mut create = false;
            let mut burst = false;
            {
                let mut state = self.state.lock().await;
                state.connections.retain(|connection| !connection.is_broken());
                if let Some(preferred) = preferred {
                    if !preferred.is_broken() {
                        if let Some(lease) = preferred_lane
                            .and_then(|lane| preferred.try_allocate_lane(lane, require_affinity))
                        {
                            return Ok(lease);
                        }
                        if let Some(lease) = preferred.try_allocate(require_affinity) {
                            return Ok(lease);
                        }
                    } else if require_affinity {
                        return Err(ResponsesWsContinuationConnectionLost.into());
                    }
                }
                if !require_affinity {
                    for connection in &state.connections {
                        if preferred.is_some_and(|preferred| Arc::ptr_eq(preferred, connection)) {
                            continue;
                        }
                        if let Some(lease) = connection.try_allocate(false) {
                            return Ok(lease);
                        }
                    }
                    let capability = self.capability.value.load(Ordering::Acquire);
                    let connection_limit = if capability == RESPONSES_WS_CAPABILITY_PROBING {
                        1
                    } else if capability == RESPONSES_WS_CAPABILITY_EXCLUSIVE {
                        self.exclusive_connections.load(Ordering::Acquire)
                    } else {
                        self.hard_connections.load(Ordering::Acquire)
                    };
                    let current = state.connections.len().saturating_add(state.connecting);
                    if current < connection_limit {
                        let target = self.target_connections.load(Ordering::Acquire);
                        burst = current >= target;
                        state.connecting = state.connecting.saturating_add(1);
                        create = true;
                    }
                }
            }

            if create {
                let dial_started = std::time::Instant::now();
                let connected = connect_responses_websocket_native_route(
                    &self.context.client,
                    &self.context.route,
                    &self.context.headers,
                )
                .await;
                let mut state = self.state.lock().await;
                state.connecting = state.connecting.saturating_sub(1);
                match connected {
                    Ok(socket) => {
                        let connection = ResponsesWsPooledConnection::spawn(
                            socket,
                            self,
                            &self.context,
                            burst,
                            duration_millis_u64(dial_started.elapsed()),
                        );
                        let mut lease = connection
                            .try_allocate(false)
                            .ok_or_else(|| anyhow!("new Responses WebSocket has no free lane"))?;
                        lease.mark_connection_created();
                        state.connections.push(connection);
                        self.notify.notify_waiters();
                        return Ok(lease);
                    }
                    Err(error) => {
                        self.notify.notify_waiters();
                        return Err(error);
                    }
                }
            }

            if tokio::time::timeout_at(deadline, async {
                tokio::select! {
                    _ = notified => {}
                    _ = capability_notified => {}
                }
            })
            .await
            .is_err()
            {
                return Err(anyhow!(
                    "Responses WebSocket pool did not provide capacity within 30 seconds"
                ));
            }
        }
    }

    async fn close_all(&self, reason: &'static str) {
        let connections = {
            let mut state = self.state.lock().await;
            std::mem::take(&mut state.connections)
        };
        for connection in connections {
            connection.request_close(reason);
        }
        self.notify.notify_waiters();
    }

    async fn remove_connection(&self, id: &str) {
        let mut state = self.state.lock().await;
        state.connections.retain(|connection| connection.id != id);
        self.notify.notify_waiters();
    }
}

impl ResponsesWsPooledConnection {
    fn spawn(
        socket: ResponsesUpstreamSocket,
        bucket: &Arc<ResponsesWsPoolBucket>,
        context: &ResponsesWsConnectContext,
        burst: bool,
        dial_ms: u64,
    ) -> Arc<Self> {
        bucket.open_connections.fetch_add(1, Ordering::AcqRel);
        let diagnostics = UpstreamWebsocketLease::open(
            "openai_responses_pool",
            context.channel_id.clone(),
            context.request_source.clone(),
        );
        let id = diagnostics.id.clone();
        let (writer, writer_rx) = tokio::sync::mpsc::channel(RESPONSES_WS_POOL_WRITE_QUEUE);
        let connection = Arc::new(Self {
            id,
            bucket: Arc::downgrade(bucket),
            writer,
            event_budget: Arc::new(tokio::sync::Semaphore::new(RESPONSES_WS_POOL_EVENT_BUFFER_BYTES)),
            slots: std::sync::Mutex::new(
                (0..RESPONSES_WS_POOL_LANES_PER_CONNECTION)
                    .map(|_| ResponsesWsLaneSlot {
                        generation: 0,
                        sender: None,
                        turn_active: false,
                        expects_stream_id: false,
                    })
                    .collect(),
            ),
            diagnostics: std::sync::Mutex::new(diagnostics),
            dial_ms,
            draining: std::sync::atomic::AtomicBool::new(false),
            broken: std::sync::atomic::AtomicBool::new(false),
            turns_started: std::sync::atomic::AtomicU64::new(0),
            session_pins: std::sync::atomic::AtomicUsize::new(0),
            idle_generation: std::sync::atomic::AtomicU64::new(0),
            burst,
        });
        let task_connection = connection.clone();
        tokio::spawn(async move {
            task_connection.run(socket, writer_rx).await;
        });
        connection
    }

    fn capacity(&self) -> usize {
        self.bucket.upgrade().map_or(1, |bucket| {
            if bucket.capability.value.load(Ordering::Acquire)
                == RESPONSES_WS_CAPABILITY_MULTIPLEX
            {
                RESPONSES_WS_POOL_LANES_PER_CONNECTION
            } else {
                1
            }
        })
    }

    fn try_allocate(self: &Arc<Self>, allow_draining: bool) -> Option<ResponsesWsLaneLease> {
        if self.is_broken() || (!allow_draining && self.draining.load(Ordering::Acquire)) {
            return None;
        }
        let capacity = self.capacity();
        let mut slots = self
            .slots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (lane, slot) = slots
            .iter_mut()
            .take(capacity)
            .enumerate()
            .find(|(_, slot)| slot.sender.is_none())?;
        slot.generation = slot.generation.saturating_add(1);
        slot.turn_active = false;
        slot.expects_stream_id = false;
        let generation = slot.generation;
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        slot.sender = Some(sender);
        self.idle_generation.fetch_add(1, Ordering::AcqRel);
        Some(ResponsesWsLaneLease {
            connection: self.clone(),
            lane,
            generation,
            receiver,
            released: false,
        })
    }

    fn try_allocate_lane(
        self: &Arc<Self>,
        lane: usize,
        allow_draining: bool,
    ) -> Option<ResponsesWsLaneLease> {
        if self.is_broken()
            || (!allow_draining && self.draining.load(Ordering::Acquire))
            || lane >= self.capacity()
        {
            return None;
        }
        let mut slots = self
            .slots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let slot = slots.get_mut(lane)?;
        if slot.sender.is_some() {
            return None;
        }
        slot.generation = slot.generation.saturating_add(1);
        slot.turn_active = false;
        slot.expects_stream_id = false;
        let generation = slot.generation;
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        slot.sender = Some(sender);
        self.idle_generation.fetch_add(1, Ordering::AcqRel);
        Some(ResponsesWsLaneLease {
            connection: self.clone(),
            lane,
            generation,
            receiver,
            released: false,
        })
    }

    fn is_broken(&self) -> bool {
        self.broken.load(Ordering::Acquire)
    }

    fn occupied(&self) -> usize {
        self.slots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|slot| slot.sender.is_some())
            .count()
    }

    fn start_turn(
        &self,
        lane: usize,
        generation: u64,
        expects_stream_id: bool,
    ) -> Result<()> {
        let mut slots = self
            .slots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let slot = slots
            .get_mut(lane)
            .filter(|slot| slot.generation == generation && slot.sender.is_some())
            .ok_or_else(|| anyhow!("Responses WebSocket lane is no longer available"))?;
        if slot.turn_active {
            return Err(anyhow!("Responses WebSocket lane already has an active response"));
        }
        slot.turn_active = true;
        slot.expects_stream_id = expects_stream_id;
        drop(slots);
        self.turns_started.fetch_add(1, Ordering::Relaxed);
        self.diagnostics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .mark_turn();
        Ok(())
    }

    fn complete_turn(&self, lane: usize, generation: u64) {
        let finished = {
            let mut slots = self
                .slots
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            slots
                .get_mut(lane)
                .filter(|slot| slot.generation == generation && slot.turn_active)
                .is_some_and(|slot| {
                    slot.turn_active = false;
                    slot.expects_stream_id = false;
                    true
                })
        };
        if finished {
            self.diagnostics
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .finish_turn();
        }
    }

    fn release_lane(self: &Arc<Self>, lane: usize, generation: u64) -> bool {
        let active = {
            let mut slots = self
                .slots
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(slot) = slots
                .get_mut(lane)
                .filter(|slot| slot.generation == generation)
            else {
                return false;
            };
            let active = slot.turn_active;
            slot.turn_active = false;
            slot.expects_stream_id = false;
            slot.sender = None;
            active
        };
        if active {
            self.diagnostics
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .finish_turn();
        }
        if let Some(bucket) = self.bucket.upgrade() {
            bucket.notify.notify_waiters();
        }
        let burst_complete = self.burst && self.turns_started.load(Ordering::Acquire) > 0;
        if (self.draining.load(Ordering::Acquire) || burst_complete) && self.occupied() == 0 {
            self.request_close(if burst_complete { "pool_burst_idle" } else { "pool_drained" });
        } else if self.occupied() == 0 {
            self.schedule_exclusive_idle_close();
        }
        active
    }

    fn pin_session(&self) {
        self.session_pins.fetch_add(1, Ordering::AcqRel);
        self.idle_generation.fetch_add(1, Ordering::AcqRel);
    }

    fn unpin_session(self: &Arc<Self>) {
        let previous = self
            .session_pins
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |pins| {
                Some(pins.saturating_sub(1))
            })
            .unwrap_or_default();
        if previous == 1 && self.occupied() == 0 {
            self.schedule_exclusive_idle_close();
        }
    }

    fn schedule_exclusive_idle_close(self: &Arc<Self>) {
        let Some(bucket) = self.bucket.upgrade() else {
            return;
        };
        if bucket.capability.value.load(Ordering::Acquire)
            == RESPONSES_WS_CAPABILITY_MULTIPLEX
            || self.session_pins.load(Ordering::Acquire) != 0
            || self.occupied() != 0
            || self.is_broken()
        {
            return;
        }
        let generation = self.idle_generation.fetch_add(1, Ordering::AcqRel) + 1;
        let connection = Arc::downgrade(self);
        tokio::spawn(async move {
            tokio::time::sleep(RESPONSES_WS_POOL_EXCLUSIVE_IDLE_TIMEOUT).await;
            let Some(connection) = connection.upgrade() else {
                return;
            };
            let still_idle = connection.idle_generation.load(Ordering::Acquire) == generation
                && connection.session_pins.load(Ordering::Acquire) == 0
                && connection.occupied() == 0;
            let still_exclusive = connection.bucket.upgrade().is_some_and(|bucket| {
                bucket.capability.value.load(Ordering::Acquire)
                    != RESPONSES_WS_CAPABILITY_MULTIPLEX
            });
            if still_idle && still_exclusive {
                connection.request_close("pool_exclusive_idle_timeout");
            }
        });
    }

    fn request_close(&self, reason: &'static str) {
        if !self.broken.swap(true, Ordering::AcqRel) {
            self.set_close_reason(reason);
            if self
                .writer
                .try_send(ResponsesWsConnectionCommand::Close(reason))
                .is_err()
            {
                let writer = self.writer.clone();
                tokio::spawn(async move {
                    let _ = writer.send(ResponsesWsConnectionCommand::Close(reason)).await;
                });
            }
            if let Some(bucket) = self.bucket.upgrade() {
                bucket.notify.notify_waiters();
            }
        }
    }

    fn set_close_reason(&self, reason: &'static str) {
        self.diagnostics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .set_close_reason(reason);
    }

    fn record_closed(&self, reason: &'static str) {
        self.broken.store(true, Ordering::Release);
        let mut diagnostics = self.diagnostics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        diagnostics.set_close_reason(reason);
        diagnostics.close();
    }

    fn attach(&self, payload: &mut serde_json::Map<String, serde_json::Value>) {
        self.diagnostics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .attach(payload);
        if payload.get("failure_stage").and_then(serde_json::Value::as_str) == Some("upstream_connect") {
            return; // No new socket was established; do not attach the old dial time.
        }
        payload.insert(
            "upstream_connection_draining".to_string(),
            self.draining.load(Ordering::Acquire).into(),
        );
        payload.insert("upstream_connection_dial_ms".to_string(), self.dial_ms.into());
    }

    fn log_terminal(&self, metadata: &ResponsesWebsocketTerminalMetadata) {
        self.diagnostics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .log_terminal(metadata);
    }

    async fn run(
        self: Arc<Self>,
        mut socket: ResponsesUpstreamSocket,
        mut writer_rx: tokio::sync::mpsc::Receiver<ResponsesWsConnectionCommand>,
    ) {
        let mut retire = Box::pin(tokio::time::sleep(RESPONSES_WS_POOL_DRAIN_AGE));
        let mut hard = Box::pin(tokio::time::sleep(RESPONSES_WS_POOL_HARD_AGE));
        let mut retired = false;
        let mut pending_event = None;
        let close_reason = loop {
            tokio::select! {
                permit = async {
                    self.event_budget.clone().acquire_many_owned(
                        pending_event.as_ref().map_or(0, responses_ws_event_cost),
                    ).await
                }, if pending_event.is_some() => {
                    let permit = match permit {
                        Ok(permit) => permit,
                        Err(_) => break "pool_event_budget_closed",
                    };
                    match pending_event.take() {
                        Some(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                            if let Err(error) = self.route_text(text.to_string(), Some(permit)).await {
                                log::warn!(
                                    "[const-api][upstream-ws-pool] route_failed connection_id={} error={error:#}",
                                    self.id,
                                );
                                break "pool_route_failed";
                            }
                        }
                        Some(message) => self.route_exclusive(Ok(message), false, Some(permit)).await,
                        None => {}
                    }
                }
                command = writer_rx.recv() => {
                    match command {
                        Some(ResponsesWsConnectionCommand::Frame(message, sent)) => {
                            match socket.send(message).await {
                                Ok(()) => {
                                    let _ = sent.send(Ok(()));
                                }
                                Err(error) => {
                                    self.record_closed("upstream_send_failed");
                                    log::warn!(
                                        "[const-api][upstream-ws-pool] write_failed connection_id={} error={error}",
                                        self.id,
                                    );
                                    let _ = sent.send(Err(error.to_string()));
                                    break "upstream_send_failed";
                                }
                            }
                        }
                        Some(ResponsesWsConnectionCommand::Close(reason)) => {
                            let _ = tokio::time::timeout(
                                WEBSOCKET_CLOSE_TIMEOUT,
                                socket.close(None),
                            ).await;
                            break reason;
                        }
                        None => {
                            break "pool_writer_closed";
                        }
                    }
                }
                // While data is backpressured, still service writes, explicit
                // cancellation and connection lifetime timers. There is no task
                // per event and no reordering or loss of buffered frames.
                upstream = socket.next(), if pending_event.is_none() => {
                    match upstream {
                        Some(Ok(message @ tokio_tungstenite::tungstenite::Message::Text(_))) => {
                            pending_event = Some(message);
                        }
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Ping(bytes))) => {
                            if socket
                                .send(tokio_tungstenite::tungstenite::Message::Pong(bytes))
                                .await
                                .is_err()
                            {
                                break "upstream_pong_failed";
                            }
                        }
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Pong(_))) => {}
                        Some(Ok(message @ tokio_tungstenite::tungstenite::Message::Close(_))) => {
                            self.record_closed("upstream_close_frame");
                            if let tokio_tungstenite::tungstenite::Message::Close(frame) = &message {
                                log::info!(
                                    "[const-api][upstream-ws-pool] peer_close connection_id={} close_code={}",
                                    self.id,
                                    frame.as_ref().map(|frame| u16::from(frame.code)).unwrap_or(1005),
                                );
                            }
                            self.broadcast(Ok(message)).await;
                            break "upstream_close_frame";
                        }
                        Some(Ok(message @ tokio_tungstenite::tungstenite::Message::Binary(_))) => {
                            if self.capacity() == 1 {
                                pending_event = Some(message);
                            } else {
                                break "unroutable_binary_frame";
                            }
                        }
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Frame(_))) => {}
                        Some(Err(error)) => {
                            self.record_closed("upstream_read_error");
                            log::warn!(
                                "[const-api][upstream-ws-pool] read_failed connection_id={} error={error}",
                                self.id,
                            );
                            self.broadcast(Err(error.to_string())).await;
                            break "upstream_read_error";
                        }
                        None => {
                            self.record_closed("upstream_eof");
                            self.broadcast(Err(
                                "Responses WebSocket upstream closed unexpectedly".to_string()
                            )).await;
                            break "upstream_eof";
                        }
                    }
                }
                _ = &mut retire, if !retired => {
                    retired = true;
                    self.draining.store(true, Ordering::Release);
                    if let Some(bucket) = self.bucket.upgrade() {
                        bucket.notify.notify_waiters();
                    }
                    if self.occupied() == 0 {
                        break "pool_retired_idle";
                    }
                }
                _ = &mut hard => {
                    break "pool_hard_lifetime";
                }
            }
        };
        // One decoded frame may be waiting for credits when the transport is
        // stopped. Deliver it before the error instead of discarding received
        // output. This drains existing memory, not an unbounded bypass.
        match pending_event {
            Some(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                let _ = self.route_text(text.to_string(), None).await;
            }
            Some(message) => self.route_exclusive(Ok(message), false, None).await,
            None => {}
        }
        self.record_closed(close_reason);
        self.fail_all(format!(
            "Responses WebSocket connection ended: {close_reason}"
        ));
        if let Some(bucket) = self.bucket.upgrade() {
            bucket.remove_connection(&self.id).await;
            bucket.open_connections.fetch_sub(1, Ordering::AcqRel);
        }
    }

    async fn route_text(
        &self,
        text: String,
        permit: Option<tokio::sync::OwnedSemaphorePermit>,
    ) -> Result<()> {
        let parsed = serde_json::from_str::<serde_json::Value>(&text).ok();
        let event_type = parsed
            .as_ref()
            .and_then(|value| value.get("type"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let stream_id = parsed
            .as_ref()
            .and_then(|value| value.get("stream_id"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let response_scoped = event_type == "error" || event_type.starts_with("response.");
        let Some(bucket) = self.bucket.upgrade() else {
            return Err(anyhow!("Responses WebSocket pool bucket was dropped"));
        };
        if bucket.capability.value.load(Ordering::Acquire) == RESPONSES_WS_CAPABILITY_PROBING
            && response_scoped
            && self.probing_stream_active()
        {
            bucket.observe_stream_id_capability(stream_id.is_some());
            if stream_id.is_none() {
                // Do not reuse a connection after an unsupported capability
                // probe. A private endpoint may finish the probe response and
                // then reset that socket; the replacement must start clean and
                // omit stream_id from its first request.
                self.draining.store(true, Ordering::Release);
                bucket.notify.notify_waiters();
            }
        }
        let capability = bucket.capability.value.load(Ordering::Acquire);
        let terminal = responses_websocket_terminal_status(&text).is_some();
        let message = tokio_tungstenite::tungstenite::Message::Text(
            responses_ws_pool_remove_stream_id(parsed, text).into(),
        );
        if capability == RESPONSES_WS_CAPABILITY_MULTIPLEX {
            if let Some(lane) = stream_id
                .as_deref()
                .and_then(responses_ws_pool_lane_from_stream_id)
            {
                self.route_lane(lane, Ok(message), terminal, permit).await;
                return Ok(());
            }
            if response_scoped {
                self.broadcast(Err(
                    "Responses WebSocket response event omitted stream_id".to_string(),
                ))
                .await;
                return Err(anyhow!(
                    "Responses WebSocket response event omitted stream_id"
                ));
            }
            return Ok(());
        }
        self.route_exclusive(Ok(message), terminal, permit).await;
        Ok(())
    }

    async fn route_exclusive(
        &self,
        message: std::result::Result<tokio_tungstenite::tungstenite::Message, String>,
        terminal: bool,
        permit: Option<tokio::sync::OwnedSemaphorePermit>,
    ) {
        let lane = {
            self.slots
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .iter()
                .position(|slot| slot.sender.is_some())
        };
        if let Some(lane) = lane {
            self.route_lane(lane, message, terminal, permit).await;
        }
    }

    fn probing_stream_active(&self) -> bool {
        self.slots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .any(|slot| slot.turn_active && slot.expects_stream_id)
    }

    async fn route_lane(
        &self,
        lane: usize,
        message: std::result::Result<tokio_tungstenite::tungstenite::Message, String>,
        terminal: bool,
        permit: Option<tokio::sync::OwnedSemaphorePermit>,
    ) {
        let target = {
            let slots = self
                .slots
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            slots
                .get(lane)
                .and_then(|slot| slot.sender.clone().map(|sender| (slot.generation, sender)))
        };
        let Some((generation, sender)) = target else {
            return;
        };
        if terminal {
            self.complete_turn(lane, generation);
        }
        if sender
            .send(ResponsesWsQueuedEvent { message, _budget: permit })
            .is_err()
            && !terminal
        {
            self.request_close("pool_event_consumer_dropped");
        }
    }

    async fn broadcast(
        &self,
        message: std::result::Result<tokio_tungstenite::tungstenite::Message, String>,
    ) {
        let targets = {
            self.slots
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .iter()
                .filter_map(|slot| slot.sender.clone())
                .collect::<Vec<_>>()
        };
        for target in targets {
            let _ = target.send(ResponsesWsQueuedEvent { message: message.clone(), _budget: None });
        }
    }

    fn fail_all(&self, message: String) {
        let (targets, active_turns) = {
            let mut slots = self
                .slots
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut active_turns = 0usize;
            let targets = slots
                .iter_mut()
                .filter_map(|slot| {
                    if slot.turn_active {
                        active_turns = active_turns.saturating_add(1);
                    }
                    slot.turn_active = false;
                    slot.expects_stream_id = false;
                    slot.sender.take()
                })
                .collect::<Vec<_>>();
            (targets, active_turns)
        };
        let mut diagnostics = self
            .diagnostics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for _ in 0..active_turns {
            diagnostics.finish_turn();
        }
        drop(diagnostics);
        for target in targets {
            let _ = target.send(ResponsesWsQueuedEvent { message: Err(message.clone()), _budget: None });
        }
    }
}

impl ResponsesWsLaneLease {
    fn stream_id(&self) -> String {
        format!("const-lane-{:02}", self.lane)
    }

    fn connection_was_created(&self) -> bool {
        // The high bit is an internal, allocation-local marker and is cleared
        // before the generation is compared with a lane slot.
        self.generation & (1u64 << 63) != 0
    }

    fn mark_connection_created(&mut self) {
        self.generation |= 1u64 << 63;
    }

    fn slot_generation(&self) -> u64 {
        self.generation & !(1u64 << 63)
    }

    async fn start_turn(&mut self, text: String) -> Result<()> {
        let generation = self.slot_generation();
        let expects_stream_id = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|value| value.get("stream_id").cloned())
            .and_then(|value| value.as_str().map(str::to_string))
            .is_some();
        self.connection
            .start_turn(self.lane, generation, expects_stream_id)?;
        self.send_frame(text).await
    }

    async fn send_frame(&self, text: String) -> Result<()> {
        let (sent_tx, sent_rx) = tokio::sync::oneshot::channel();
        self.connection
            .writer
            .send(ResponsesWsConnectionCommand::Frame(
                tokio_tungstenite::tungstenite::Message::Text(text.into()),
                sent_tx,
            ))
            .await
            .map_err(|_| anyhow!("Responses WebSocket pool writer is closed"))?;
        sent_rx
            .await
            .map_err(|_| anyhow!("Responses WebSocket pool writer stopped before send"))?
            .map_err(anyhow::Error::msg)
    }

    fn finish(&mut self) {
        if self.released {
            return;
        }
        let generation = self.slot_generation();
        self.connection.complete_turn(self.lane, generation);
        self.connection.release_lane(self.lane, generation);
        self.released = true;
    }

    fn release_idle(&mut self) {
        if self.released {
            return;
        }
        self.connection
            .release_lane(self.lane, self.slot_generation());
        self.released = true;
    }

    fn cancel(&mut self, reason: &'static str) {
        if self.released {
            return;
        }
        self.connection.request_close(reason);
        self.connection
            .release_lane(self.lane, self.slot_generation());
        self.released = true;
    }
}

impl Drop for ResponsesWsLaneLease {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let active = self
            .connection
            .release_lane(self.lane, self.slot_generation());
        self.released = true;
        if active {
            // The Responses WebSocket contract does not document a per-stream
            // cancel event. Closing the physical connection is the only safe
            // way to avoid an orphaned billable response.
            self.connection.request_close("pool_orphaned_active_turn");
        }
    }
}

fn responses_ws_pool_dimensions(max_concurrency: u32) -> (usize, usize) {
    let logical = usize::try_from(max_concurrency.max(1)).unwrap_or(usize::MAX);
    let target = logical
        .saturating_add(RESPONSES_WS_POOL_LANES_PER_CONNECTION - 1)
        / RESPONSES_WS_POOL_LANES_PER_CONNECTION;
    let target = target.clamp(1, RESPONSES_WS_POOL_MAX_TARGET_CONNECTIONS);
    let hard = target
        .saturating_add(1)
        .clamp(2, RESPONSES_WS_POOL_MAX_CONNECTIONS);
    (target, hard)
}

fn responses_ws_pool_exclusive_limit(max_concurrency: u32) -> usize {
    let configured = if max_concurrency == 0 {
        crate::config::DEFAULT_SUPPLIER_MAX_CONCURRENCY
    } else {
        max_concurrency
    };
    usize::try_from(configured).unwrap_or(usize::MAX).max(1)
}

fn duration_millis_u64(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

fn responses_ws_pool_protocol_mode(
    route: &ResponsesWebsocketNativeRoute,
    channel: &ChannelConfig,
) -> ResponsesWsPoolProtocolMode {
    if responses_websocket_route_is_official_openai_api(route) {
        return ResponsesWsPoolProtocolMode::ConfirmedMultiplex;
    }
    if matches!(route, ResponsesWebsocketNativeRoute::OpenAiSubscription { .. })
        && channel
            .subscription
            .responses_ws_subscription_multiplex_probe_enabled
    {
        return ResponsesWsPoolProtocolMode::ProbeSubscriptionMultiplex;
    }
    ResponsesWsPoolProtocolMode::Exclusive
}

fn responses_ws_pool_endpoint(route: &ResponsesWebsocketNativeRoute) -> Result<String> {
    let http_url = match route {
        ResponsesWebsocketNativeRoute::HttpSurface { target, .. } => {
            crate::supplier::join_upstream_url(&target.base_url, "/v1/responses")
        }
        ResponsesWebsocketNativeRoute::OpenAiSubscription { .. } => codex_responses_url(),
    };
    Ok(responses_websocket_url(&http_url)?.to_string())
}

fn responses_ws_pool_shared_headers(
    route: &ResponsesWebsocketNativeRoute,
    headers: &warp::http::HeaderMap,
) -> warp::http::HeaderMap {
    if responses_websocket_route_is_official_openai_api(route) {
        let mut shared = warp::http::HeaderMap::new();
        for name in RESPONSES_WS_PUBLIC_SHARED_HANDSHAKE_HEADERS {
            for value in headers.get_all(*name).iter() {
                if let Ok(name) = warp::http::header::HeaderName::from_bytes(name.as_bytes()) {
                    shared.append(name, value.clone());
                }
            }
        }
        return shared;
    }
    let mut shared = codex_model_request_context_headers(Some(headers), &active_codex_identity());
    if let Some(beta) = headers.get("openai-beta") {
        shared.insert("openai-beta", beta.clone());
    }
    shared
}

fn responses_ws_pool_header_fingerprint(headers: &warp::http::HeaderMap) -> String {
    use sha2::Digest as _;
    let mut hasher = sha2::Sha256::new();
    let mut names = headers
        .keys()
        .map(|name| name.as_str().to_ascii_lowercase())
        .collect::<Vec<_>>();
    names.sort_unstable();
    names.dedup();
    for name in names {
        hasher.update(name.as_bytes());
        hasher.update([0]);
        for value in headers.get_all(name.as_str()).iter() {
            hasher.update(value.as_bytes());
            hasher.update([0xfe]);
        }
        hasher.update([0xff]);
    }
    hex::encode(hasher.finalize())
}

fn responses_ws_pool_add_stream_id(text: &str, stream_id: String) -> Result<String> {
    let mut value = serde_json::from_str::<serde_json::Value>(text)?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| anyhow!("Responses WebSocket request must be a JSON object"))?;
    object.insert("stream_id".to_string(), stream_id.into());
    serde_json::to_string(&value).map_err(Into::into)
}

fn responses_ws_pool_probe_request(text: &str) -> Result<String> {
    let value = serde_json::from_str::<serde_json::Value>(text)?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("Responses WebSocket request must be a JSON object"))?;
    let mut probe = serde_json::Map::new();
    probe.insert(
        "type".to_string(),
        object
            .get("type")
            .cloned()
            .unwrap_or_else(|| "response.create".into()),
    );
    if let Some(model) = object.get("model") {
        probe.insert("model".to_string(), model.clone());
    }
    // This request only establishes protocol capability. Never duplicate the
    // caller's prompt, instructions, tools, metadata, or continuation state in
    // the internal warmup request.
    probe.insert("input".to_string(), serde_json::Value::Array(Vec::new()));
    probe.insert("tools".to_string(), serde_json::Value::Array(Vec::new()));
    probe.insert("store".to_string(), false.into());
    probe.insert("generate".to_string(), false.into());
    serde_json::to_string(&probe).map_err(Into::into)
}

fn responses_ws_pool_remove_stream_id(
    parsed: Option<serde_json::Value>,
    original: String,
) -> String {
    let Some(mut value) = parsed else {
        return original;
    };
    let Some(object) = value.as_object_mut() else {
        return original;
    };
    if object.remove("stream_id").is_none() {
        return original;
    }
    serde_json::to_string(&value).unwrap_or(original)
}

fn responses_ws_pool_lane_from_stream_id(stream_id: &str) -> Option<usize> {
    let lane = stream_id.strip_prefix("const-lane-")?.parse::<usize>().ok()?;
    (lane < RESPONSES_WS_POOL_LANES_PER_CONNECTION).then_some(lane)
}

#[cfg(test)]
mod responses_ws_pool_tests {
    use super::*;

    async fn buffered_test_connection() -> (
        Arc<ResponsesWsPoolBucket>,
        Arc<ResponsesWsPooledConnection>,
        tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    ) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("listen");
        let address = listener.local_addr().expect("address");
        let (client, server) = tokio::join!(
            tokio_tungstenite::connect_async(format!("ws://{address}")),
            async {
                let (stream, _) = listener.accept().await.expect("accept");
                tokio_tungstenite::accept_async(stream)
                    .await
                    .expect("upgrade")
            },
        );
        let (socket, _) = client.expect("connect");
        let channel = channel_from_supplier("buffer-test".to_string(), &default_supplier_config());
        let context = ResponsesWsConnectContext {
            client: Client::new(),
            route: ResponsesWebsocketNativeRoute::OpenAiSubscription {
                channel: Box::new(channel.clone()),
            },
            headers: warp::http::HeaderMap::new(),
            channel_id: channel.id,
            request_source: "test".to_string(),
        };
        let bucket = Arc::new(ResponsesWsPoolBucket {
            state: tokio::sync::Mutex::new(ResponsesWsPoolBucketState::default()),
            notify: tokio::sync::Notify::new(),
            capability: Arc::new(ResponsesWsCapabilityState {
                value: std::sync::atomic::AtomicU8::new(RESPONSES_WS_CAPABILITY_EXCLUSIVE),
                notify: tokio::sync::Notify::new(),
                probe_lock: tokio::sync::Mutex::new(()),
            }),
            target_connections: std::sync::atomic::AtomicUsize::new(1),
            hard_connections: std::sync::atomic::AtomicUsize::new(2),
            exclusive_connections: std::sync::atomic::AtomicUsize::new(2),
            open_connections: std::sync::atomic::AtomicUsize::new(0),
            context: context.clone(),
        });
        let connection = ResponsesWsPooledConnection::spawn(socket, &bucket, &context, false, 0);
        bucket
            .state
            .lock()
            .await
            .connections
            .push(connection.clone());
        (bucket, connection, server)
    }

    async fn wait_buffered(lease: &ResponsesWsLaneLease, count: usize) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while lease.receiver.len() < count {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("buffered events");
    }

    #[tokio::test]
    async fn interrupt_stays_on_the_active_pool_lane_without_starting_a_turn() {
        use tokio_tungstenite::tungstenite::Message;
        const INTERRUPT: &str = r#"{ "type":"response.interrupt","response_id":"resp-1","mode":"discard_partial_items","future":true }"#;
        for multiplex in [false, true] {
            let (_bucket, connection, mut server) = buffered_test_connection().await;
            let mut lease = connection.try_allocate(false).expect("lane");
            let stream_id = multiplex.then(|| lease.stream_id());
            let create = r#"{"type":"response.create","input":[]}"#;
            let body = stream_id.as_ref().map_or_else(
                || create.to_owned(),
                |id| responses_ws_pool_add_stream_id(create, id.clone()).expect("stream id"),
            );
            lease.start_turn(body).await.expect("start");
            server.next().await.expect("create").expect("frame");
            let mut turn = ResponsesWsPoolTurn { lease: Some(lease), stream_id: stream_id.clone() };
            turn.interrupt(INTERRUPT).await.expect("interrupt");
            let received = server.next().await.expect("control").expect("frame").into_text().expect("text");
            if let Some(id) = &stream_id {
                let mut expected: serde_json::Value = serde_json::from_str(INTERRUPT).expect("JSON");
                expected["stream_id"] = id.clone().into();
                assert_eq!(serde_json::from_str::<serde_json::Value>(&received).expect("JSON"), expected);
            } else {
                assert_eq!(received, INTERRUPT);
            }
            assert_eq!(connection.turns_started.load(Ordering::Relaxed), 1);
            assert!(connection.slots.lock().expect("slots")[0].turn_active);
            let mut terminal = serde_json::json!({"type":"response.completed","response":{"id":"resp-1","usage":{"output_tokens":2}}});
            if let Some(id) = stream_id { terminal["stream_id"] = id.into(); }
            server.send(Message::Text(terminal.to_string().into())).await.expect("terminal");
            let message = turn.next().await.expect("event").expect("frame").into_text().expect("text");
            assert!(message.contains("output_tokens"));
            turn.finish();
            assert!(connection.try_allocate(false).is_some(), "lane is reusable after terminal");
            connection.request_close("test_finished");
        }
    }

    #[tokio::test]
    async fn buffer_backpressure_preserves_order_and_services_writes() {
        use tokio_tungstenite::tungstenite::Message;
        let (_bucket, connection, mut server) = buffered_test_connection().await;
        let mut lease = connection.try_allocate(false).expect("lane");
        lease
            .start_turn(r#"{"type":"response.create","input":[]}"#.to_string())
            .await
            .expect("start");
        server.next().await.expect("request").expect("frame");
        let messages: Vec<_> = (0..3)
            .map(|i| {
                Message::Text(
                    format!(r#"{{"type":"response.output_text.delta","delta":"{i}"}}"#).into(),
                )
            })
            .collect();
        let cost = responses_ws_event_cost(&messages[0]);
        // Leave room for exactly two events, without a 32 MiB fixture.
        let reserved = connection
            .event_budget
            .clone()
            .acquire_many_owned(RESPONSES_WS_POOL_EVENT_BUFFER_BYTES as u32 - 2 * cost)
            .await
            .expect("reserve test capacity");
        for message in &messages {
            server.send(message.clone()).await.expect("upstream event");
        }
        wait_buffered(&lease, 2).await;
        let (sent, ack) = tokio::sync::oneshot::channel();
        connection
            .writer
            .send(ResponsesWsConnectionCommand::Frame(
                Message::Ping(vec![1].into()),
                sent,
            ))
            .await
            .expect("queue ping");
        tokio::time::timeout(Duration::from_secs(3), ack)
            .await
            .expect("writer stays live")
            .expect("ack")
            .expect("sent");
        assert!(server.next().await.expect("ping").expect("frame").is_ping());
        assert!(!connection.is_broken());
        assert_eq!(lease.receiver.len(), 2);
        let mut turn = ResponsesWsPoolTurn { lease: Some(lease), stream_id: None };
        assert_eq!(
            turn.next().await.expect("event").expect("frame"),
            messages[0]
        );
        wait_buffered(turn.lease.as_ref().expect("lease"), 2).await;
        for message in &messages[1..] {
            assert_eq!(turn.next().await.expect("event").expect("frame"), *message);
        }
        assert_eq!(
            connection.event_budget.available_permits(),
            2 * cost as usize
        );
        drop(reserved);
        turn.cancel("test_complete");
    }

    #[tokio::test]
    async fn buffer_shutdown_delivers_error_even_when_byte_budget_is_full() {
        use tokio_tungstenite::tungstenite::Message;
        let (_bucket, connection, mut server) = buffered_test_connection().await;
        let mut lease = connection.try_allocate(false).expect("lane");
        lease
            .start_turn(r#"{"type":"response.create","input":[]}"#.to_string())
            .await
            .expect("start");
        server.next().await.expect("request").expect("frame");
        let frame = Message::Text(r#"{"type":"response.output_text.delta","delta":"one"}"#.into());
        let cost = responses_ws_event_cost(&frame);
        let reserved = connection
            .event_budget
            .clone()
            .acquire_many_owned(RESPONSES_WS_POOL_EVENT_BUFFER_BYTES as u32 - cost)
            .await
            .expect("reserve test capacity");
        server.send(frame.clone()).await.expect("first event");
        wait_buffered(&lease, 1).await;
        server.send(frame.clone()).await.expect("pending event");
        tokio::time::timeout(Duration::from_secs(3), async {
            // Connection + queued credit + reserved test credit + pending wait.
            while Arc::strong_count(&connection.event_budget) < 4 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("second event is read and waiting for credits");
        connection.request_close("test_shutdown");
        let mut turn = ResponsesWsPoolTurn { lease: Some(lease), stream_id: None };
        // Drain the already-read pending frame ahead of the final error, even
        // though no byte credits remain at shutdown.
        wait_buffered(turn.lease.as_ref().expect("lease"), 3).await;
        assert_eq!(turn.next().await.expect("event").expect("frame"), frame);
        assert_eq!(turn.next().await.expect("event").expect("frame"), frame);
        let error = turn
            .next()
            .await
            .expect("terminal error")
            .expect_err("not a clean EOF");
        assert!(error.to_string().contains("test_shutdown"));
        turn.finish();
        drop(reserved);
        assert_eq!(
            connection.event_budget.available_permits(),
            RESPONSES_WS_POOL_EVENT_BUFFER_BYTES
        );
    }

    #[test]
    fn buffer_accounts_for_tiny_and_oversized_events() {
        use tokio_tungstenite::tungstenite::Message;
        assert!(responses_ws_event_cost(&Message::Text("".into())) > 0);
        let frame = Message::Binary(vec![0; RESPONSES_WS_POOL_EVENT_BUFFER_BYTES + 1].into());
        assert_eq!(
            responses_ws_event_cost(&frame) as usize,
            RESPONSES_WS_POOL_EVENT_BUFFER_BYTES
        );
    }

    #[test]
    fn pool_dimensions_bound_subscription_concurrency() {
        assert_eq!(responses_ws_pool_dimensions(1), (1, 2));
        assert_eq!(responses_ws_pool_dimensions(8), (1, 2));
        assert_eq!(responses_ws_pool_dimensions(9), (2, 3));
        assert_eq!(responses_ws_pool_dimensions(20), (3, 4));
        assert_eq!(responses_ws_pool_dimensions(10_000), (3, 4));
        assert_eq!(responses_ws_pool_exclusive_limit(20), 20);
        assert_eq!(
            responses_ws_pool_exclusive_limit(0),
            crate::config::DEFAULT_SUPPLIER_MAX_CONCURRENCY as usize
        );
    }

    #[test]
    fn only_typed_pre_send_affinity_loss_is_replayable() {
        let classified: anyhow::Error = ResponsesWsContinuationConnectionLost.into();
        assert!(responses_ws_pool_error_requires_full_replay(&classified));

        let same_text = anyhow!(
            "Responses WebSocket continuation connection is no longer available"
        );
        assert!(!responses_ws_pool_error_requires_full_replay(&same_text));
        assert!(!responses_ws_pool_error_requires_full_replay(&anyhow!(
            "Responses WebSocket pool writer stopped before send"
        )));
    }

    #[test]
    fn stream_ids_are_internal_and_lane_bounded() {
        let outbound = responses_ws_pool_add_stream_id(
            r#"{"type":"response.create","model":"gpt-test","input":[]}"#,
            "const-lane-07".to_string(),
        )
        .expect("add stream id");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&outbound).expect("outbound JSON")
                ["stream_id"],
            "const-lane-07"
        );
        let inbound = responses_ws_pool_remove_stream_id(
            serde_json::from_str::<serde_json::Value>(
                r#"{"type":"response.completed","stream_id":"const-lane-07"}"#,
            )
            .ok(),
            String::new(),
        );
        assert!(
            serde_json::from_str::<serde_json::Value>(&inbound).expect("inbound JSON")
                ["stream_id"]
                .is_null()
        );
        assert_eq!(
            responses_ws_pool_lane_from_stream_id("const-lane-07"),
            Some(7)
        );
        assert_eq!(
            responses_ws_pool_lane_from_stream_id("const-lane-08"),
            None
        );

        let probe = responses_ws_pool_probe_request(
            r#"{"type":"response.create","model":"gpt-test","input":"private prompt","instructions":"private instructions","previous_response_id":"resp_private","client_metadata":{"secret":true}}"#,
        )
        .expect("capability probe");
        let probe = serde_json::from_str::<serde_json::Value>(&probe).expect("probe JSON");
        assert_eq!(probe["model"], "gpt-test");
        assert_eq!(probe["generate"], false);
        assert_eq!(probe["store"], false);
        assert_eq!(probe["input"], serde_json::json!([]));
        assert!(probe.get("instructions").is_none());
        assert!(probe.get("previous_response_id").is_none());
        assert!(probe.get("client_metadata").is_none());
    }

    #[test]
    fn pooled_handshake_preserves_compatibility_headers() {
        let supplier = default_supplier_config();
        let channel = channel_from_supplier("private-handshake-test".to_string(), &supplier);
        let route = ResponsesWebsocketNativeRoute::OpenAiSubscription {
            channel: Box::new(channel.clone()),
        };
        let mut input = warp::http::HeaderMap::new();
        input.insert("originator", "codex_vscode".parse().expect("originator"));
        input.insert(
            "user-agent",
            "codex_vscode/1.2.3".parse().expect("user agent"),
        );
        input.insert("session-id", "session-secret".parse().expect("session"));
        input.insert(
            "x-client-request-id",
            "request-secret".parse().expect("request"),
        );
        input.insert(
            "x-codex-routing-hint",
            "sticky-owner".parse().expect("routing"),
        );
        input.insert(
            "x-responsesapi-include-timing-metrics",
            "true".parse().expect("timing metrics"),
        );
        let shared = responses_ws_pool_shared_headers(&route, &input);
        assert_eq!(
            responses_ws_pool_protocol_mode(&route, &channel),
            ResponsesWsPoolProtocolMode::Exclusive
        );
        assert_eq!(shared["originator"], "codex_vscode");
        assert_eq!(shared["x-codex-routing-hint"], "sticky-owner");
        assert_eq!(shared["session-id"], "session-secret");
        assert_eq!(shared["x-client-request-id"], "request-secret");
        assert_eq!(shared["x-responsesapi-include-timing-metrics"], "true");

        let baseline = responses_ws_pool_header_fingerprint(&shared);
        let mut changed = shared.clone();
        changed.insert(
            "x-responsesapi-include-timing-metrics",
            "false".parse().expect("changed timing metrics"),
        );
        assert_ne!(baseline, responses_ws_pool_header_fingerprint(&changed));
    }

    #[test]
    fn official_api_pool_mode_is_multiplex_and_omits_session_handshake_identity() {
        let supplier = default_supplier_config();
        let mut channel = channel_from_supplier("public-api-pool-test".to_string(), &supplier);
        channel.set_source_driver(crate::source_driver::SourceDriverId::OpenAiApi);
        let target = crate::channel_surface::ChannelSurfaceTarget {
            surface: crate::surface::ApiSurface::OpenAi,
            base_url: "https://api.openai.com".to_string(),
            endpoint_profile: "openai".to_string(),
            auth_scheme: "api_key".to_string(),
            protocol: crate::protocol::kind::ProtocolKind::OpenAiResponses,
        };
        let route = ResponsesWebsocketNativeRoute::HttpSurface {
            channel: Box::new(channel.clone()),
            target,
        };
        let mut input = warp::http::HeaderMap::new();
        input.insert("user-agent", "codex-test/1.0".parse().expect("user agent"));
        input.insert("session-id", "session-a".parse().expect("session"));
        input.insert("thread-id", "thread-a".parse().expect("thread"));
        input.insert(
            "openai-project",
            "project-a".parse().expect("OpenAI project"),
        );

        let shared = responses_ws_pool_shared_headers(&route, &input);
        assert_eq!(
            responses_ws_pool_protocol_mode(&route, &channel),
            ResponsesWsPoolProtocolMode::ConfirmedMultiplex
        );
        assert_eq!(
            responses_ws_pool_endpoint(&route).expect("pool endpoint"),
            "wss://api.openai.com/v1/responses"
        );
        assert_eq!(shared["user-agent"], "codex-test/1.0");
        assert_eq!(shared["openai-project"], "project-a");
        assert!(!shared.contains_key("session-id"));
        assert!(!shared.contains_key("thread-id"));
    }

    #[tokio::test]
    async fn exclusive_idle_close_is_invalidated_by_session_reuse() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("mock listener");
        let address = listener.local_addr().expect("mock address");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("mock accept");
            let mut socket = tokio_tungstenite::accept_async(stream)
                .await
                .expect("mock WebSocket upgrade");
            while let Some(frame) = socket.next().await {
                match frame.expect("mock frame") {
                    tokio_tungstenite::tungstenite::Message::Close(_) => {
                        let _ = socket.close(None).await;
                        break;
                    }
                    _ => {}
                }
            }
        });

        let (socket, _) = tokio_tungstenite::connect_async(format!("ws://{address}"))
            .await
            .expect("client connect");
        let supplier = default_supplier_config();
        let channel = channel_from_supplier("exclusive-idle-test".to_string(), &supplier);
        let context = ResponsesWsConnectContext {
            client: Client::new(),
            route: ResponsesWebsocketNativeRoute::OpenAiSubscription {
                channel: Box::new(channel.clone()),
            },
            headers: warp::http::HeaderMap::new(),
            channel_id: channel.id,
            request_source: "test".to_string(),
        };
        let bucket = Arc::new(ResponsesWsPoolBucket {
            state: tokio::sync::Mutex::new(ResponsesWsPoolBucketState::default()),
            notify: tokio::sync::Notify::new(),
            capability: Arc::new(ResponsesWsCapabilityState {
                value: std::sync::atomic::AtomicU8::new(RESPONSES_WS_CAPABILITY_EXCLUSIVE),
                notify: tokio::sync::Notify::new(),
                probe_lock: tokio::sync::Mutex::new(()),
            }),
            target_connections: std::sync::atomic::AtomicUsize::new(1),
            hard_connections: std::sync::atomic::AtomicUsize::new(2),
            exclusive_connections: std::sync::atomic::AtomicUsize::new(20),
            open_connections: std::sync::atomic::AtomicUsize::new(0),
            context: context.clone(),
        });
        let connection =
            ResponsesWsPooledConnection::spawn(socket, &bucket, &context, false, 0);
        bucket.state.lock().await.connections.push(connection.clone());

        connection.pin_session();
        connection.unpin_session();
        tokio::time::sleep(Duration::from_millis(40)).await;
        connection.pin_session();
        tokio::time::sleep(Duration::from_millis(90)).await;
        assert!(!connection.is_broken(), "stale idle timer closed reused socket");

        connection.unpin_session();
        tokio::time::sleep(Duration::from_millis(140)).await;
        assert!(connection.is_broken(), "exclusive idle socket was retained");
        server.await.expect("mock server");
    }

    #[tokio::test]
    async fn one_physical_connection_routes_interleaved_stream_ids() {
        const BURST: usize = 4096;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("mock listener");
        let address = listener.local_addr().expect("mock address");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("mock accept");
            let mut socket = tokio_tungstenite::accept_async(stream)
                .await
                .expect("mock WebSocket upgrade");
            let first = socket
                .next()
                .await
                .expect("first request")
                .expect("first request frame")
                .into_text()
                .expect("first request text");
            let second = socket
                .next()
                .await
                .expect("second request")
                .expect("second request frame")
                .into_text()
                .expect("second request text");
            let first_stream = serde_json::from_str::<serde_json::Value>(&first)
                .expect("first JSON")["stream_id"]
                .as_str()
                .expect("first stream id")
                .to_string();
            let second_stream = serde_json::from_str::<serde_json::Value>(&second)
                .expect("second JSON")["stream_id"]
                .as_str()
                .expect("second stream id")
                .to_string();
            // Keep the first consumer idle while a burst larger than the old
            // 128-event limit arrives; the second lane must still complete.
            for sequence in 0..BURST {
                socket.send(tokio_tungstenite::tungstenite::Message::Text(
                    serde_json::json!({"type":"response.output_text.delta", "stream_id":first_stream, "sequence":sequence}).to_string().into()
                )).await.expect("burst frame");
            }
            for event in [
                serde_json::json!({
                    "type": "response.output_text.delta",
                    "stream_id": second_stream,
                    "delta": "second"
                }),
                serde_json::json!({
                    "type": "response.completed",
                    "stream_id": second_stream,
                    "response": {"id": "resp-second"}
                }),
                serde_json::json!({
                    "type": "response.output_text.delta",
                    "stream_id": first_stream,
                    "delta": "first"
                }),
                serde_json::json!({
                    "type": "response.completed",
                    "stream_id": first_stream,
                    "response": {"id": "resp-first"}
                }),
            ] {
                socket
                    .send(tokio_tungstenite::tungstenite::Message::Text(
                        event.to_string().into(),
                    ))
                    .await
                .expect("mock response");
            }
            while let Some(Ok(message)) = socket.next().await {
                if message.is_close() { break; }
            }
        });

        let (socket, _) = tokio_tungstenite::connect_async(format!("ws://{address}"))
            .await
            .expect("client connect");
        let supplier = default_supplier_config();
        let channel = channel_from_supplier("pool-test".to_string(), &supplier);
        let context = ResponsesWsConnectContext {
            client: Client::new(),
            route: ResponsesWebsocketNativeRoute::OpenAiSubscription {
                channel: Box::new(channel.clone()),
            },
            headers: warp::http::HeaderMap::new(),
            channel_id: channel.id,
            request_source: "test".to_string(),
        };
        let bucket = Arc::new(ResponsesWsPoolBucket {
            state: tokio::sync::Mutex::new(ResponsesWsPoolBucketState::default()),
            notify: tokio::sync::Notify::new(),
            capability: Arc::new(ResponsesWsCapabilityState {
                value: std::sync::atomic::AtomicU8::new(
                    RESPONSES_WS_CAPABILITY_MULTIPLEX,
                ),
                notify: tokio::sync::Notify::new(),
                probe_lock: tokio::sync::Mutex::new(()),
            }),
            target_connections: std::sync::atomic::AtomicUsize::new(1),
            hard_connections: std::sync::atomic::AtomicUsize::new(2),
            exclusive_connections: std::sync::atomic::AtomicUsize::new(20),
            open_connections: std::sync::atomic::AtomicUsize::new(0),
            context: context.clone(),
        });
        let connection =
            ResponsesWsPooledConnection::spawn(socket, &bucket, &context, false, 0);
        bucket.state.lock().await.connections.push(connection.clone());
        let mut first = connection.try_allocate(false).expect("first lane");
        let mut second = connection.try_allocate(false).expect("second lane");
        first
            .start_turn(
                responses_ws_pool_add_stream_id(
                    r#"{"type":"response.create","model":"first","input":[]}"#,
                    first.stream_id(),
                )
                .expect("first request"),
            )
            .await
            .expect("send first");
        second
            .start_turn(
                responses_ws_pool_add_stream_id(
                    r#"{"type":"response.create","model":"second","input":[]}"#,
                    second.stream_id(),
                )
                .expect("second request"),
            )
            .await
            .expect("send second");

        let second_delta = second
            .receiver
            .recv()
            .await
            .expect("second delta")
            .message
            .expect("second delta frame")
            .into_text()
            .expect("second delta text");
        assert!(second_delta.contains("second"));
        assert!(!second_delta.contains("stream_id"));
        let second_terminal = second
            .receiver
            .recv()
            .await
            .expect("second terminal")
            .message
            .expect("second terminal frame")
            .into_text()
            .expect("second terminal text");
        assert!(second_terminal.contains("resp-second"));

        assert!(!connection.is_broken(), "burst closed the shared connection");
        for sequence in 0..BURST {
            let event = first.receiver.recv().await.expect("buffered event");
            let text = event.message.expect("buffered frame").into_text().expect("text");
            let value: serde_json::Value = serde_json::from_str(&text).expect("JSON");
            assert_eq!(value["sequence"], sequence);
        }

        let first_delta = first
            .receiver
            .recv()
            .await
            .expect("first delta")
            .message
            .expect("first delta frame")
            .into_text()
            .expect("first delta text");
        assert!(first_delta.contains("first"));
        let first_terminal = first
            .receiver
            .recv()
            .await
            .expect("first terminal")
            .message
            .expect("first terminal frame")
            .into_text()
            .expect("first terminal text");
        assert!(first_terminal.contains("resp-first"));

        first.finish();
        second.finish();
        assert_eq!(connection.occupied(), 0);
        connection.request_close("test_complete");
        server.await.expect("mock server");
    }

    #[tokio::test]
    async fn unsupported_stream_id_retries_on_clean_exclusive_pool_connection() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("mock listener");
        let address = listener.local_addr().expect("mock address");
        let server = tokio::spawn(async move {
            let (probe_stream, _) = listener.accept().await.expect("probe accept");
            let mut probe_socket = tokio_tungstenite::accept_async(probe_stream)
                .await
                .expect("probe WebSocket upgrade");
            let probe = probe_socket
                .next()
                .await
                .expect("probe request")
                .expect("probe frame")
                .into_text()
                .expect("probe text");
            let probe = serde_json::from_str::<serde_json::Value>(&probe).expect("probe JSON");
            assert_eq!(probe["generate"], false);
            assert_eq!(probe["stream_id"], "const-lane-00");
            probe_socket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    serde_json::json!({
                        "type": "error",
                        "error": {
                            "type": "invalid_request_error",
                            "message": "Unsupported parameter: stream_id"
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .expect("probe rejection");
            let _ = probe_socket.close(None).await;

            let (clean_stream, _) = listener.accept().await.expect("clean accept");
            let mut clean_socket = tokio_tungstenite::accept_async(clean_stream)
                .await
                .expect("clean WebSocket upgrade");
            let first = clean_socket
                .next()
                .await
                .expect("first actual request")
                .expect("first actual frame")
                .into_text()
                .expect("first actual text");
            let first = serde_json::from_str::<serde_json::Value>(&first).expect("first JSON");
            assert!(first.get("stream_id").is_none());
            assert!(first.get("generate").is_none());
            for event in [
                serde_json::json!({
                    "type": "response.created",
                    "response": {"id": "resp-clean-1"}
                }),
                serde_json::json!({
                    "type": "response.completed",
                    "response": {"id": "resp-clean-1"}
                }),
            ] {
                clean_socket
                    .send(tokio_tungstenite::tungstenite::Message::Text(
                        event.to_string().into(),
                    ))
                    .await
                    .expect("first actual response");
            }

            let continuation = clean_socket
                .next()
                .await
                .expect("continuation request")
                .expect("continuation frame")
                .into_text()
                .expect("continuation text");
            let continuation = serde_json::from_str::<serde_json::Value>(&continuation)
                .expect("continuation JSON");
            assert!(continuation.get("stream_id").is_none());
            assert_eq!(continuation["previous_response_id"], "resp-clean-1");
            clean_socket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    serde_json::json!({
                        "type": "response.completed",
                        "response": {"id": "resp-clean-2"}
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .expect("continuation response");
        });

        let supplier = default_supplier_config();
        let channel = channel_from_supplier("fallback-test".to_string(), &supplier);
        let target = crate::channel_surface::ChannelSurfaceTarget {
            surface: crate::surface::ApiSurface::OpenAi,
            base_url: format!("http://{address}"),
            endpoint_profile: "openai".to_string(),
            auth_scheme: "none".to_string(),
            protocol: crate::protocol::kind::ProtocolKind::OpenAiResponses,
        };
        let route = ResponsesWebsocketNativeRoute::HttpSurface {
            channel: Box::new(channel.clone()),
            target,
        };
        let context = ResponsesWsConnectContext {
            client: Client::new(),
            route,
            headers: warp::http::HeaderMap::new(),
            channel_id: channel.id,
            request_source: "test".to_string(),
        };
        let capability = Arc::new(ResponsesWsCapabilityState {
            value: std::sync::atomic::AtomicU8::new(RESPONSES_WS_CAPABILITY_PROBING),
            notify: tokio::sync::Notify::new(),
            probe_lock: tokio::sync::Mutex::new(()),
        });
        let bucket = Arc::new(ResponsesWsPoolBucket {
            state: tokio::sync::Mutex::new(ResponsesWsPoolBucketState::default()),
            notify: tokio::sync::Notify::new(),
            capability,
            target_connections: std::sync::atomic::AtomicUsize::new(1),
            hard_connections: std::sync::atomic::AtomicUsize::new(2),
            exclusive_connections: std::sync::atomic::AtomicUsize::new(20),
            open_connections: std::sync::atomic::AtomicUsize::new(0),
            context: context.clone(),
        });
        let (initial_socket, _) =
            tokio_tungstenite::connect_async(format!("ws://{address}/v1/responses"))
                .await
                .expect("initial client connect");
        let initial =
            ResponsesWsPooledConnection::spawn(initial_socket, &bucket, &context, false, 0);
        bucket.state.lock().await.connections.push(initial.clone());
        initial.pin_session();
        let mut first_session = ResponsesWsPoolSession {
            bucket: bucket.clone(),
            preferred: Some(initial),
            preferred_lane: Some(0),
            connection_reused: false,
            acquire_ms: 0,
            continuation_replays: 0,
        };
        let first_request = serde_json::json!({
            "type": "response.create",
            "model": "gpt-test",
            "input": [{"role": "user", "content": "first"}],
            "store": false,
            "stream": true
        })
        .to_string();
        let (first_status, first_response_id) = live_terminal_status(
            first_session.start_turn(&first_request).await.expect("first turn"),
        )
        .await
        .expect("first terminal");
        assert_eq!(first_status, 200);
        assert_eq!(first_response_id.as_deref(), Some("resp-clean-1"));
        assert_eq!(
            bucket.capability.value.load(Ordering::Acquire),
            RESPONSES_WS_CAPABILITY_EXCLUSIVE
        );

        let second_preferred = first_session.preferred.clone();
        second_preferred
            .as_ref()
            .expect("second preferred connection")
            .pin_session();
        let mut second_session = ResponsesWsPoolSession {
            bucket: bucket.clone(),
            preferred: second_preferred,
            preferred_lane: Some(0),
            connection_reused: true,
            acquire_ms: 0,
            continuation_replays: 0,
        };
        let continuation = serde_json::json!({
            "type": "response.create",
            "model": "gpt-test",
            "previous_response_id": "resp-clean-1",
            "input": [{"role": "user", "content": "continue"}],
            "store": false,
            "stream": true
        })
        .to_string();
        let (second_status, second_response_id) = live_terminal_status(
            second_session
                .start_turn(&continuation)
                .await
                .expect("continuation turn"),
        )
        .await
        .expect("continuation terminal");
        assert_eq!(second_status, 200);
        assert_eq!(second_response_id.as_deref(), Some("resp-clean-2"));
        assert!(Arc::ptr_eq(
            first_session.preferred.as_ref().expect("first connection"),
            second_session
                .preferred
                .as_ref()
                .expect("second connection")
        ));
        server.await.expect("mock server");
    }

    #[tokio::test]
    async fn pre_send_affinity_loss_replays_complete_subscription_context_once() {
        check_subscription_continuation_replay(false).await;
    }

    #[tokio::test]
    async fn rejected_reference_replays_complete_subscription_context_once() {
        check_subscription_continuation_replay(true).await;
    }

    async fn check_subscription_continuation_replay(reject_reference: bool) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("mock listener");
        let address = listener.local_addr().expect("mock address");
        let (seen_tx, mut seen_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let server = tokio::spawn(async move {
            let mut connected = None;
            for connection_index in 0..2 {
                let mut socket = match connected.take() {
                    Some(socket) => socket,
                    None => {
                        let (stream, _) = listener.accept().await.expect("mock accept");
                        tokio_tungstenite::accept_async(stream).await.expect("mock WebSocket upgrade")
                    }
                };
                let request = socket
                    .next()
                    .await
                    .expect("mock request")
                    .expect("mock request frame")
                    .into_text()
                    .expect("mock request text")
                    .to_string();
                seen_tx.send(request).expect("capture request");
                if reject_reference && connection_index == 1 {
                    socket.send(tokio_tungstenite::tungstenite::Message::Text(
                        serde_json::json!({"type": "error", "status": 400, "error": {
                            "type": "invalid_request_error", "code": "previous_response_not_found",
                            "param": "previous_response_id", "message": "previous response expired"
                        }}).to_string().into()
                    )).await.unwrap();
                    let replay = socket.next().await.unwrap().unwrap().into_text().unwrap().to_string();
                    seen_tx.send(replay).unwrap();
                }
                let events = if connection_index == 0 {
                    vec![
                        serde_json::json!({
                            "type": "response.created",
                            "response": {"id": "resp_pool_replay_parent"}
                        }),
                        serde_json::json!({
                            "type": "response.output_item.done",
                            "output_index": 0,
                            "item": {
                                "id": "fc_pool_replay_parent",
                                "type": "function_call",
                                "call_id": "call_pool_replay_parent",
                                "name": "read_file",
                                "arguments": "{}"
                            }
                        }),
                        serde_json::json!({
                            "type": "response.completed",
                            "response": {"id": "resp_pool_replay_parent", "output": []}
                        }),
                    ]
                } else {
                    vec![
                        serde_json::json!({
                            "type": "response.output_item.done",
                            "output_index": 0,
                            "item": {
                                "type": "message",
                                "role": "assistant",
                                "content": [{"type": "output_text", "text": "recovered"}]
                            }
                        }),
                        serde_json::json!({
                            "type": "response.completed",
                            "response": {"id": "resp_pool_replay_child", "output": []}
                        }),
                    ]
                };
                for event in events {
                    socket
                        .send(tokio_tungstenite::tungstenite::Message::Text(
                            event.to_string().into(),
                        ))
                        .await
                        .expect("mock response");
                }
                if reject_reference && connection_index == 0 {
                    connected = Some(socket);
                } else {
                    let _ = socket.close(None).await;
                }
            }
        });

        let mut supplier = default_supplier_config();
        supplier.channel_id = format!("pool-replay-integration-{reject_reference}");
        supplier.credential_ref = "pool-replay-account".to_string();
        supplier.subscription.credential_ref = supplier.credential_ref.clone();
        let mut channel = channel_from_supplier(supplier.channel_id.clone(), &supplier);
        channel.subscription.responses_ws_pool_enabled = true;
        channel
            .subscription
            .responses_ws_subscription_multiplex_probe_enabled = false;
        let target = crate::channel_surface::ChannelSurfaceTarget {
            surface: crate::surface::ApiSurface::OpenAi,
            base_url: format!("http://{address}"),
            endpoint_profile: "openai".to_string(),
            auth_scheme: "none".to_string(),
            protocol: crate::protocol::kind::ProtocolKind::OpenAiResponses,
        };
        let route = ResponsesWebsocketNativeRoute::HttpSurface {
            channel: Box::new(channel.clone()),
            target,
        };
        let pool = ResponsesWsPool::default();
        let session = pool
            .prepare_session(
                &Client::new(),
                &route,
                &channel,
                &warp::http::HeaderMap::new(),
                "replay-test",
            )
            .await
            .expect("prepare pooled session");
        let first_connection_id = session
            .preferred
            .as_ref()
            .expect("first preferred connection")
            .id
            .clone();
        let mut sender = SupplierResponsesUpstreamSender::Pooled(session);
        let mut receiver = SupplierResponsesUpstreamReceiver::Pooled(None);
        let continuation_pin = CodexWebsocketContinuationPin::new();
        let first_request = serde_json::json!({
            "type": "response.create",
            "model": "gpt-test",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "read"}]
            }],
            "store": false,
            "prompt_cache_key": "pool-replay-stable-cache-key",
            "stream": true
        })
        .to_string();
        assert!(sender
            .send_text_with_subscription_replay(
                &mut receiver,
                Some(&supplier),
                &first_request,
            )
            .await
            .expect("send first turn")
            .is_none());
        let mut first_collector = CodexHttpContinuationCollector::new_for_pinned_websocket(
            &supplier, &first_request, &continuation_pin,
        );
        let (first_status, first_response_id) = collect_test_subscription_turn(
            &mut sender,
            &mut receiver,
            &supplier,
            &mut first_collector,
        )
        .await;
        assert_eq!(first_status, 200);
        assert_eq!(first_response_id.as_deref(), Some("resp_pool_replay_parent"));
        assert!(first_collector.progress().cached);

        if !reject_reference {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let broken = match &sender {
                    SupplierResponsesUpstreamSender::Pooled(session) => session
                        .preferred
                        .as_ref()
                        .is_some_and(|connection| connection.is_broken()),
                    SupplierResponsesUpstreamSender::Dedicated { .. } => false,
                };
                if broken {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("first physical connection should close");
        }

        let continuation = serde_json::json!({
            "type": "response.create",
            "model": "gpt-test",
            "previous_response_id": "resp_pool_replay_parent",
            "input": [{
                "type": "function_call_output",
                "call_id": "call_pool_replay_parent",
                "output": "contents"
            }],
            "store": false,
            "stream": true
        })
        .to_string();
        let replay = sender
            .send_text_with_subscription_replay(
                &mut receiver,
                Some(&supplier),
                &continuation,
            )
            .await
            .expect("send continuation");
        let replay = if reject_reference {
            assert!(replay.is_none(), "a healthy affine connection should receive the normal delta first");
            let rejection = receiver.next().await.unwrap().unwrap().into_text().unwrap();
            let metadata = responses_websocket_terminal_metadata(&rejection).unwrap();
            // Even a reference error must not restart a turn already visible to the caller.
            assert!(sender.replay_rejected_subscription_continuation(
                &mut receiver, &supplier, &continuation, Some(&metadata), true,
            ).await.unwrap().is_none());
            let mut unrelated = metadata.clone();
            unrelated.error_code = Some("rate_limit_exceeded".into());
            assert!(sender.replay_rejected_subscription_continuation(
                &mut receiver, &supplier, &continuation, Some(&unrelated), false,
            ).await.unwrap().is_none());
            unrelated = metadata.clone();
            unrelated.status = 502;
            assert!(sender.replay_rejected_subscription_continuation(
                &mut receiver, &supplier, &continuation, Some(&unrelated), false,
            ).await.unwrap().is_none());
            unrelated = metadata.clone();
            unrelated.event_type = "response.failed".into();
            assert!(sender.replay_rejected_subscription_continuation(
                &mut receiver, &supplier, &continuation, Some(&unrelated), false,
            ).await.unwrap().is_none());
            let mut unavailable: serde_json::Value = serde_json::from_str(&continuation).unwrap();
            unavailable["previous_response_id"] = "resp_uncached_reference".into();
            assert!(sender.replay_rejected_subscription_continuation(
                &mut receiver, &supplier, &unavailable.to_string(), Some(&metadata), false,
            ).await.unwrap().is_none());
            let recovered = sender.replay_rejected_subscription_continuation(
                &mut receiver, &supplier, &continuation, Some(&metadata), false,
            ).await.unwrap().expect("explicit rejection should recover from complete history");
            // The full body has no reference; a second rejection must reach the caller.
            assert!(sender.replay_rejected_subscription_continuation(
                &mut receiver, &supplier, &recovered, Some(&metadata), false,
            ).await.unwrap().is_none());
            recovered
        } else {
            replay.expect("affinity loss should use a full replay")
        };
        let replay_json: serde_json::Value =
            serde_json::from_str(&replay).expect("replay request JSON");
        assert!(replay_json.get("previous_response_id").is_none());
        assert_eq!(replay_json["input"].as_array().map(Vec::len), Some(3));
        let mut second_collector = CodexHttpContinuationCollector::new_for_pinned_websocket(
            &supplier, &replay, &continuation_pin,
        );
        let (second_status, second_response_id) = collect_test_subscription_turn(
            &mut sender,
            &mut receiver,
            &supplier,
            &mut second_collector,
        )
        .await;
        assert_eq!(second_status, 200);
        assert_eq!(second_response_id.as_deref(), Some("resp_pool_replay_child"));

        let (second_connection_id, replay_count) = match &sender {
            SupplierResponsesUpstreamSender::Pooled(session) => (
                session
                    .preferred
                    .as_ref()
                    .expect("replacement connection")
                    .id
                    .clone(),
                session.continuation_replays,
            ),
            SupplierResponsesUpstreamSender::Dedicated { .. } => {
                panic!("test unexpectedly used a dedicated connection")
            }
        };
        assert_eq!(first_connection_id == second_connection_id, reject_reference);
        assert_eq!(replay_count, 1);
        let first_seen: serde_json::Value = serde_json::from_str(
            &seen_rx.recv().await.expect("first captured request"),
        )
        .expect("first captured JSON");
        if reject_reference {
            let delta: serde_json::Value = serde_json::from_str(&seen_rx.recv().await.unwrap()).unwrap();
            assert_eq!(delta["previous_response_id"], "resp_pool_replay_parent");
            assert_eq!(delta["input"].as_array().unwrap().len(), 1);
        }
        let second_seen: serde_json::Value = serde_json::from_str(
            &seen_rx.recv().await.expect("second captured request"),
        )
        .expect("second captured JSON");
        assert_eq!(first_seen["input"].as_array().map(Vec::len), Some(1));
        assert!(second_seen.get("previous_response_id").is_none());
        assert_eq!(second_seen["input"].as_array().map(Vec::len), Some(3));
        assert_eq!(first_seen["prompt_cache_key"], "pool-replay-stable-cache-key");
        assert_eq!(second_seen["prompt_cache_key"], first_seen["prompt_cache_key"]);
        assert_eq!(second_seen["input"][0], first_seen["input"][0]);
        assert_eq!(second_seen["input"][1]["call_id"], "call_pool_replay_parent");
        assert_eq!(second_seen["input"][2]["call_id"], "call_pool_replay_parent");
        assert!(seen_rx.try_recv().is_err(), "request was replayed more than once");

        pool.shutdown();
        server.await.expect("mock server");
    }

    async fn collect_test_subscription_turn(
        sender: &mut SupplierResponsesUpstreamSender,
        receiver: &mut SupplierResponsesUpstreamReceiver,
        config: &SupplierConfig,
        collector: &mut CodexHttpContinuationCollector,
    ) -> (u16, Option<String>) {
        loop {
            let message = receiver
                .next()
                .await
                .expect("test turn ended before terminal event")
                .expect("test upstream frame");
            let tokio_tungstenite::tungstenite::Message::Text(text) = message else {
                continue;
            };
            let _ = collector.push_websocket_text(config, text.as_ref());
            let parsed = serde_json::from_str::<serde_json::Value>(text.as_ref())
                .expect("test response JSON");
            let response_id = parsed
                .pointer("/response/id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            if let Some(status) = responses_websocket_terminal_status(text.as_ref()) {
                sender.finish_turn(receiver);
                return (status, response_id);
            }
        }
    }

    async fn live_supplier_terminal_status(
        sender: &mut SupplierResponsesUpstreamSender,
        receiver: &mut SupplierResponsesUpstreamReceiver,
        config: &SupplierConfig,
        collector: &mut CodexHttpContinuationCollector,
    ) -> Result<(u16, Option<String>)> {
        tokio::time::timeout(Duration::from_secs(90), async {
            let mut response_id = None;
            loop {
                let message = receiver
                    .next()
                    .await
                    .context("live supplier WebSocket ended before a terminal event")??;
                match message {
                    tokio_tungstenite::tungstenite::Message::Text(text) => {
                        let _ = collector.push_websocket_text(config, text.as_ref());
                        let parsed =
                            serde_json::from_str::<serde_json::Value>(text.as_ref()).ok();
                        if response_id.is_none() {
                            response_id = parsed.as_ref().and_then(|value| {
                                value
                                    .pointer("/response/id")
                                    .and_then(serde_json::Value::as_str)
                                    .map(str::to_string)
                            });
                        }
                        if let Some(status) = responses_websocket_terminal_status(text.as_ref()) {
                            sender.finish_turn(receiver);
                            if !(200..300).contains(&status) {
                                let message = parsed
                                    .as_ref()
                                    .and_then(|value| value.pointer("/error/message"))
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or("unknown upstream error");
                                anyhow::bail!(
                                    "live supplier Responses WebSocket failed status={} message={}",
                                    status,
                                    message.chars().take(300).collect::<String>(),
                                );
                            }
                            return Ok((status, response_id));
                        }
                    }
                    tokio_tungstenite::tungstenite::Message::Close(frame) => {
                        anyhow::bail!(
                            "live supplier Responses WebSocket closed before completion: {}",
                            frame.map(|frame| frame.reason.to_string()).unwrap_or_default()
                        );
                    }
                    _ => {}
                }
            }
        })
        .await
        .context("live supplier Responses WebSocket turn exceeded 90 seconds")?
    }

    async fn live_terminal_status(
        mut turn: ResponsesWsPoolTurn,
    ) -> Result<(u16, Option<String>)> {
        tokio::time::timeout(Duration::from_secs(90), async move {
            let mut response_id = None;
            loop {
                let message = turn
                    .next()
                    .await
                    .context("live Responses WebSocket ended before a terminal event")??;
                match message {
                    tokio_tungstenite::tungstenite::Message::Text(text) => {
                        let parsed = serde_json::from_str::<serde_json::Value>(text.as_ref()).ok();
                        if response_id.is_none() {
                            response_id = parsed.as_ref().and_then(|value| {
                                value
                                    .pointer("/response/id")
                                    .and_then(serde_json::Value::as_str)
                                    .map(str::to_string)
                            });
                        }
                        if let Some(status) = responses_websocket_terminal_status(text.as_ref()) {
                            if !(200..300).contains(&status) {
                                let metadata = responses_websocket_terminal_metadata(text.as_ref());
                                let error_message = parsed
                                    .as_ref()
                                    .and_then(|value| value.pointer("/error/message"))
                                    .and_then(serde_json::Value::as_str)
                                    .map(|message| {
                                        message
                                            .chars()
                                            .take(300)
                                            .map(|character| {
                                                if character.is_ascii_control() {
                                                    ' '
                                                } else {
                                                    character
                                                }
                                            })
                                            .collect::<String>()
                                    })
                                    .unwrap_or_else(|| "-".to_string());
                                anyhow::bail!(
                                    "live Responses WebSocket terminal status={} error_type={} error_code={} error_param={} message={}",
                                    status,
                                    metadata
                                        .as_ref()
                                        .and_then(|metadata| metadata.error_type.as_deref())
                                        .unwrap_or("-"),
                                    metadata
                                        .as_ref()
                                        .and_then(|metadata| metadata.error_code.as_deref())
                                        .unwrap_or("-"),
                                    metadata
                                        .as_ref()
                                        .and_then(|metadata| metadata.error_param.as_deref())
                                        .unwrap_or("-"),
                                    error_message,
                                );
                            }
                            turn.finish();
                            return Ok((status, response_id));
                        }
                    }
                    tokio_tungstenite::tungstenite::Message::Close(frame) => {
                        anyhow::bail!(
                            "live Responses WebSocket closed before completion: {}",
                            frame
                                .map(|frame| frame.reason.to_string())
                                .unwrap_or_default()
                        );
                    }
                    _ => {}
                }
            }
        })
        .await
        .context("live Responses WebSocket turn exceeded 90 seconds")?
    }

    #[derive(Debug)]
    struct LiveDirectTerminalEvidence {
        status: u16,
        saw_stream_id: bool,
        error_type: Option<String>,
        error_code: Option<String>,
        error_param: Option<String>,
        error_message: Option<String>,
    }

    fn live_openai_subscription_channel_and_model() -> Result<Option<(ChannelConfig, String)>> {
        let Some(config_path) = std::env::var_os("CONST_API_LIVE_OPENAI_WS_CONFIG_PATH") else {
            eprintln!(
                "live WebSocket evidence skipped: CONST_API_LIVE_OPENAI_WS_CONFIG_PATH is not set"
            );
            return Ok(None);
        };
        let config_path = std::path::PathBuf::from(config_path);
        let config = crate::config::load_config_from_path(&config_path)?;
        let channel = config
            .channels
            .into_iter()
            .find(|channel| {
                channel.enabled
                    && channel.source_driver()
                        == crate::source_driver::SourceDriverId::OpenAiSubscription
            })
            .context("no enabled OpenAI subscription channel in the development config")?;
        let model = std::env::var("CONST_API_LIVE_OPENAI_WS_MODEL")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .or_else(|| {
                (!channel.upstream_model.trim().is_empty())
                    .then(|| channel.upstream_model.trim().to_string())
            })
            .or_else(|| channel.models.first().cloned())
            .context("OpenAI subscription model is unavailable")?;
        if !channel.models.iter().any(|candidate| candidate == &model) {
            anyhow::bail!("selected model is not declared by the OpenAI subscription channel");
        }
        Ok(Some((channel, model)))
    }

    fn live_context_headers(
        label: &str,
        installation_id: &str,
    ) -> Result<(warp::http::HeaderMap, String, String)> {
        let identity = active_codex_identity();
        let mut headers = codex_model_identity_headers(&identity);
        let nonce = format!("{:032x}", rand::random::<u128>());
        let session_id = format!("live-ws-{label}-{nonce}");
        let thread_id = format!("live-thread-{label}-{nonce}");
        headers.insert("session-id", session_id.parse()?);
        headers.insert("thread-id", thread_id.parse()?);
        headers.insert("x-client-request-id", thread_id.parse()?);
        headers.insert("x-codex-installation-id", installation_id.parse()?);
        headers.insert(
            "x-codex-window-id",
            format!("{thread_id}:0").parse()?,
        );
        headers.insert(
            "openai-beta",
            reqwest::header::HeaderValue::from_static(RESPONSES_WEBSOCKET_BETA),
        );
        Ok((headers, session_id, thread_id))
    }

    fn live_warmup_request(
        model: &str,
        session_id: &str,
        thread_id: &str,
        stream_id: Option<&str>,
    ) -> String {
        let mut request = serde_json::json!({
            "type": "response.create",
            "model": model,
            "input": [],
            "tools": [],
            "store": false,
            "generate": false,
            "client_metadata": {
                "session_id": session_id,
                "thread_id": thread_id
            }
        });
        if let Some(stream_id) = stream_id {
            request["stream_id"] = stream_id.into();
        }
        request.to_string()
    }

    async fn live_direct_terminal_evidence(
        socket: &mut ResponsesUpstreamSocket,
        request: String,
        expected_stream_id: Option<&str>,
    ) -> Result<LiveDirectTerminalEvidence> {
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                request.into(),
            ))
            .await?;
        tokio::time::timeout(Duration::from_secs(90), async {
            let mut saw_stream_id = false;
            loop {
                let frame = socket
                    .next()
                    .await
                    .context("live direct WebSocket ended before a terminal event")??;
                match frame {
                    tokio_tungstenite::tungstenite::Message::Text(text) => {
                        let parsed = serde_json::from_str::<serde_json::Value>(text.as_ref()).ok();
                        if let Some(expected_stream_id) = expected_stream_id {
                            saw_stream_id |= parsed
                                .as_ref()
                                .and_then(|value| value.get("stream_id"))
                                .and_then(serde_json::Value::as_str)
                                == Some(expected_stream_id);
                        }
                        let Some(metadata) =
                            responses_websocket_terminal_metadata(text.as_ref())
                        else {
                            continue;
                        };
                        let error_message = parsed
                            .as_ref()
                            .and_then(|value| value.pointer("/error/message"))
                            .and_then(serde_json::Value::as_str)
                            .map(|message| message.chars().take(300).collect::<String>());
                        return Ok(LiveDirectTerminalEvidence {
                            status: metadata.status,
                            saw_stream_id,
                            error_type: metadata.error_type,
                            error_code: metadata.error_code,
                            error_param: metadata.error_param,
                            error_message,
                        });
                    }
                    tokio_tungstenite::tungstenite::Message::Close(frame) => {
                        anyhow::bail!(
                            "live direct WebSocket closed before completion: {}",
                            frame.map(|frame| frame.reason.to_string()).unwrap_or_default()
                        );
                    }
                    _ => {}
                }
            }
        })
        .await
        .context("live direct WebSocket request exceeded 90 seconds")?
    }

    fn live_terminal_evidence_json(evidence: &LiveDirectTerminalEvidence) -> serde_json::Value {
        serde_json::json!({
            "status": evidence.status,
            "saw_stream_id": evidence.saw_stream_id,
            "error_type": evidence.error_type,
            "error_code": evidence.error_code,
            "error_param": evidence.error_param,
            "error_message": evidence.error_message
        })
    }

    async fn live_subscription_socket_with_beta_mode(
        client: &Client,
        channel: &ChannelConfig,
        headers: &warp::http::HeaderMap,
        responses_url: &str,
        include_beta: bool,
    ) -> Result<ResponsesUpstreamSocket> {
        let (token, credential, _) = ensure_openai_subscription_access_token(client, channel).await?;
        let mut request =
            openai_subscription_websocket_request(responses_url, &token, &credential, headers)?;
        if !include_beta {
            request.headers_mut().remove("openai-beta");
        }
        connect_responses_websocket_request(request).await
    }

    #[tokio::test]
    #[ignore = "requires an explicit development config and performs bounded non-generating capability probes"]
    async fn live_openai_subscription_stream_id_endpoint_matrix() -> Result<()> {
        let Some((channel, model)) = live_openai_subscription_channel_and_model()? else {
            return Ok(());
        };
        let installation_id = format!("{:064x}", rand::random::<u128>());
        let (headers, session_id, thread_id) =
            live_context_headers("capability", &installation_id)?;
        let client = Client::new();
        let route = ResponsesWebsocketNativeRoute::OpenAiSubscription {
            channel: Box::new(channel.clone()),
        };

        let mut private_beta =
            connect_responses_websocket_native_route(&client, &route, &headers).await?;
        let private_beta_evidence = live_direct_terminal_evidence(
            &mut private_beta,
            live_warmup_request(
                &model,
                &session_id,
                &thread_id,
                Some("const-capability-beta"),
            ),
            Some("const-capability-beta"),
        )
        .await?;
        let _ = private_beta.close(None).await;

        let private_without_beta = live_subscription_socket_with_beta_mode(
            &client,
            &channel,
            &headers,
            &codex_responses_url(),
            false,
        )
        .await;
        let private_without_beta_json = match private_without_beta {
            Ok(mut socket) => {
                let evidence = live_direct_terminal_evidence(
                    &mut socket,
                    live_warmup_request(
                        &model,
                        &session_id,
                        &thread_id,
                        Some("const-capability-no-beta"),
                    ),
                    Some("const-capability-no-beta"),
                )
                .await?;
                let _ = socket.close(None).await;
                serde_json::json!({
                    "handshake_status": 101,
                    "terminal": live_terminal_evidence_json(&evidence)
                })
            }
            Err(error) => serde_json::json!({
                "handshake_status": responses_websocket_error_status(&error),
                "terminal": null
            }),
        };

        let public_api = live_subscription_socket_with_beta_mode(
            &client,
            &channel,
            &headers,
            "https://api.openai.com/v1/responses",
            false,
        )
        .await;
        let public_api_json = match public_api {
            Ok(mut socket) => {
                let evidence = live_direct_terminal_evidence(
                    &mut socket,
                    live_warmup_request(
                        &model,
                        &session_id,
                        &thread_id,
                        Some("const-capability-public"),
                    ),
                    Some("const-capability-public"),
                )
                .await?;
                let _ = socket.close(None).await;
                serde_json::json!({
                    "handshake_status": 101,
                    "terminal": live_terminal_evidence_json(&evidence)
                })
            }
            Err(error) => serde_json::json!({
                "handshake_status": responses_websocket_error_status(&error),
                "terminal": null
            }),
        };

        let report = serde_json::json!({
            "private_codex_current_beta": {
                "handshake_status": 101,
                "terminal": live_terminal_evidence_json(&private_beta_evidence)
            },
            "private_codex_without_beta": private_without_beta_json,
            "public_api_with_subscription_oauth": public_api_json
        });
        println!("WS_STREAM_ID_ENDPOINT_EVIDENCE={report}");

        let private_supported = (200..300).contains(&private_beta_evidence.status)
            && private_beta_evidence.saw_stream_id;
        let parser_rejected_stream_id = private_beta_evidence.error_param.as_deref()
            == Some("stream_id")
            || private_beta_evidence
                .error_message
                .as_deref()
                .is_some_and(|message| message.contains("stream_id"));
        if !private_supported && !parser_rejected_stream_id {
            anyhow::bail!(
                "private Codex endpoint produced neither stream_id support nor a stream_id parser rejection: {report}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires an explicit development config and compares six real reconnects with ctx_pool reuse"]
    async fn live_openai_subscription_ctx_pool_reconnect_count() -> Result<()> {
        const LOGICAL_RECONNECTS: usize = 6;
        let Some((mut channel, model)) = live_openai_subscription_channel_and_model()? else {
            return Ok(());
        };
        channel.subscription.responses_ws_pool_enabled = true;
        channel
            .subscription
            .responses_ws_subscription_multiplex_probe_enabled = false;
        let route = ResponsesWebsocketNativeRoute::OpenAiSubscription {
            channel: Box::new(channel.clone()),
        };
        let installation_id = format!("{:064x}", rand::random::<u128>());
        let (headers, session_id, thread_id) =
            live_context_headers("reconnect", &installation_id)?;
        let client = Client::new();

        let baseline_started = std::time::Instant::now();
        let mut baseline_handshake_ms_total = 0u128;
        for _ in 0..LOGICAL_RECONNECTS {
            let handshake_started = std::time::Instant::now();
            let mut socket =
                connect_responses_websocket_native_route(&client, &route, &headers).await?;
            baseline_handshake_ms_total = baseline_handshake_ms_total
                .saturating_add(handshake_started.elapsed().as_millis());
            let evidence = live_direct_terminal_evidence(
                &mut socket,
                live_warmup_request(&model, &session_id, &thread_id, None),
                None,
            )
            .await?;
            if !(200..300).contains(&evidence.status) {
                anyhow::bail!("baseline reconnect warmup failed: {evidence:?}");
            }
            let _ = socket.close(None).await;
        }
        let baseline_elapsed_ms = baseline_started.elapsed().as_millis();

        let pool = ResponsesWsPool::default();
        let pooled_started = std::time::Instant::now();
        let mut pooled_prepare_ms_total = 0u128;
        let mut pooled_connection_dials = HashMap::new();
        for _ in 0..LOGICAL_RECONNECTS {
            let prepare_started = std::time::Instant::now();
            let mut session = pool
                .prepare_session(&client, &route, &channel, &headers, "live-reconnect")
                .await?;
            pooled_prepare_ms_total = pooled_prepare_ms_total
                .saturating_add(prepare_started.elapsed().as_millis());
            let connection = session
                .preferred
                .as_ref()
                .context("pooled reconnect has no preferred connection")?;
            pooled_connection_dials.insert(connection.id.clone(), connection.dial_ms);
            let (status, _) = live_terminal_status(
                session
                    .start_turn(&live_warmup_request(
                        &model,
                        &session_id,
                        &thread_id,
                        None,
                    ))
                    .await?,
            )
            .await?;
            if !(200..300).contains(&status) {
                anyhow::bail!("pooled reconnect warmup failed with status {status}");
            }
            drop(session);
        }
        let pooled_elapsed_ms = pooled_started.elapsed().as_millis();
        let pooled_physical_wss = pooled_connection_dials.len();
        let pooled_handshake_ms_total = pooled_connection_dials
            .into_values()
            .map(u128::from)
            .sum::<u128>();
        let report = serde_json::json!({
            "logical_reconnects": LOGICAL_RECONNECTS,
            "handshake_context": "identical",
            "baseline_physical_wss": LOGICAL_RECONNECTS,
            "baseline_handshake_ms_total": baseline_handshake_ms_total,
            "baseline_elapsed_ms": baseline_elapsed_ms,
            "pooled_physical_wss": pooled_physical_wss,
            "pooled_handshake_ms_total": pooled_handshake_ms_total,
            "pooled_prepare_ms_total": pooled_prepare_ms_total,
            "pooled_elapsed_ms": pooled_elapsed_ms,
            "requests_generate_model_output": false
        });
        println!("WS_CTX_POOL_RECONNECT_EVIDENCE={report}");
        if pooled_physical_wss != 1 {
            anyhow::bail!(
                "ctx_pool did not reuse exactly one physical WSS for identical reconnects: {report}"
            );
        }
        pool.shutdown();
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires an explicit development config, performs two bounded subscription responses, and forces one between-turn reconnect"]
    async fn live_openai_subscription_continuation_replay_after_disconnect() -> Result<()> {
        let Some((mut channel, model)) = live_openai_subscription_channel_and_model()? else {
            return Ok(());
        };
        channel.subscription.responses_ws_pool_enabled = true;
        channel
            .subscription
            .responses_ws_subscription_multiplex_probe_enabled = false;
        let route = ResponsesWebsocketNativeRoute::OpenAiSubscription {
            channel: Box::new(channel.clone()),
        };
        let installation_id = format!("{:064x}", rand::random::<u128>());
        let (headers, session_id, thread_id) =
            live_context_headers("continuation-replay", &installation_id)?;
        let subscription = supplier_from_channel(&channel);
        let pool = ResponsesWsPool::default();
        let session = pool
            .prepare_session(
                &Client::new(),
                &route,
                &channel,
                &headers,
                "live-continuation-replay",
            )
            .await?;
        let first_connection_id = session
            .preferred
            .as_ref()
            .context("first live continuation connection is unavailable")?
            .id
            .clone();
        let mut sender = SupplierResponsesUpstreamSender::Pooled(session);
        let mut receiver = SupplierResponsesUpstreamReceiver::Pooled(None);
        let request = |label: &str, previous_response_id: Option<&str>| {
            let mut request = serde_json::json!({
                "type": "response.create",
                "model": model,
                "instructions": "Reply with exactly one word: OK.",
                "input": [{
                    "type": "message",
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": format!("CONST API reconnect acceptance {label}")
                    }]
                }],
                "tools": [],
                "tool_choice": "auto",
                "parallel_tool_calls": false,
                "reasoning": {"effort": "low", "summary": "auto"},
                "include": ["reasoning.encrypted_content"],
                "prompt_cache_key": session_id,
                "client_metadata": {
                    "session_id": session_id,
                    "thread_id": thread_id,
                    "x-codex-installation-id": installation_id,
                    "x-codex-window-id": format!("{thread_id}:0")
                },
                "store": false,
                "stream": true
            });
            if let Some(previous_response_id) = previous_response_id {
                request["previous_response_id"] = previous_response_id.into();
            }
            request.to_string()
        };

        let first_request = request("first", None);
        if sender
            .send_text_with_subscription_replay(
                &mut receiver,
                Some(&subscription),
                &first_request,
            )
            .await?
            .is_some()
        {
            anyhow::bail!("first live turn unexpectedly used continuation replay");
        }
        let mut first_collector =
            CodexHttpContinuationCollector::new_for_websocket(&subscription, &first_request);
        let (first_status, previous_response_id) = live_supplier_terminal_status(
            &mut sender,
            &mut receiver,
            &subscription,
            &mut first_collector,
        )
        .await?;
        let previous_response_id =
            previous_response_id.context("first live turn did not return a response id")?;
        if !first_collector.progress().cached {
            anyhow::bail!("first live turn did not capture complete replay context");
        }

        match &sender {
            SupplierResponsesUpstreamSender::Pooled(session) => session
                .preferred
                .as_ref()
                .context("live preferred connection disappeared before forced close")?
                .request_close("live_test_forced_between_turn_disconnect"),
            SupplierResponsesUpstreamSender::Dedicated { .. } => {
                anyhow::bail!("live continuation test unexpectedly used a dedicated connection")
            }
        }

        let continuation = request("second", Some(&previous_response_id));
        let replay = sender
            .send_text_with_subscription_replay(
                &mut receiver,
                Some(&subscription),
                &continuation,
            )
            .await?
            .context("forced affinity loss did not produce a safe full replay")?;
        let replay_json: serde_json::Value = serde_json::from_str(&replay)?;
        let replay_items = replay_json
            .get("input")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len)
            .unwrap_or_default();
        if replay_json.get("previous_response_id").is_some() || replay_items < 3 {
            anyhow::bail!(
                "live replay was not self-contained previous_response_id_present={} input_items={}",
                replay_json.get("previous_response_id").is_some(),
                replay_items,
            );
        }
        let mut second_collector =
            CodexHttpContinuationCollector::new_for_websocket(&subscription, &replay);
        let (second_status, _) = live_supplier_terminal_status(
            &mut sender,
            &mut receiver,
            &subscription,
            &mut second_collector,
        )
        .await?;
        let (second_connection_id, replay_count) = match &sender {
            SupplierResponsesUpstreamSender::Pooled(session) => (
                session
                    .preferred
                    .as_ref()
                    .context("replacement live connection is unavailable")?
                    .id
                    .clone(),
                session.continuation_replays,
            ),
            SupplierResponsesUpstreamSender::Dedicated { .. } => unreachable!(),
        };
        if first_connection_id == second_connection_id || replay_count != 1 {
            anyhow::bail!(
                "live continuation recovery failed connection_changed={} replay_count={}",
                first_connection_id != second_connection_id,
                replay_count,
            );
        }
        let report = serde_json::json!({
            "forced_failure_phase": "between_turns_before_next_send",
            "physical_connections": 2,
            "connection_changed": true,
            "continuation_replays": replay_count,
            "previous_response_id_removed": true,
            "full_replay_input_items": replay_items,
            "request_statuses": [first_status, second_status]
        });
        println!("WS_CONTINUATION_REPLAY_EVIDENCE={report}");
        pool.shutdown();
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires an explicit development config and verifies distinct handshake contexts stay separate"]
    async fn live_openai_subscription_distinct_context_isolation() -> Result<()> {
        const LOGICAL_SESSIONS: usize = 6;
        let Some((mut channel, model)) = live_openai_subscription_channel_and_model()? else {
            return Ok(());
        };
        channel.subscription.responses_ws_pool_enabled = true;
        channel
            .subscription
            .responses_ws_subscription_multiplex_probe_enabled = false;
        let route = ResponsesWebsocketNativeRoute::OpenAiSubscription {
            channel: Box::new(channel.clone()),
        };
        let installation_id = format!("{:064x}", rand::random::<u128>());
        let mut contexts = Vec::with_capacity(LOGICAL_SESSIONS);
        for index in 0..LOGICAL_SESSIONS {
            contexts.push(live_context_headers(
                &format!("count-{index}"),
                &installation_id,
            )?);
        }
        let client = Client::new();

        let baseline_started = std::time::Instant::now();
        let mut baseline_sockets = Vec::with_capacity(LOGICAL_SESSIONS);
        for (headers, _, _) in &contexts {
            baseline_sockets.push(
                connect_responses_websocket_native_route(&client, &route, headers).await?,
            );
        }
        let baseline_handshake_ms = baseline_started.elapsed().as_millis();
        for (socket, (_, session_id, thread_id)) in
            baseline_sockets.iter_mut().zip(contexts.iter())
        {
            let evidence = live_direct_terminal_evidence(
                socket,
                live_warmup_request(&model, session_id, thread_id, None),
                None,
            )
            .await?;
            if !(200..300).contains(&evidence.status) {
                anyhow::bail!("baseline warmup failed: {evidence:?}");
            }
        }
        for socket in &mut baseline_sockets {
            let _ = socket.close(None).await;
        }

        let pool = ResponsesWsPool::default();
        let pooled_started = std::time::Instant::now();
        let mut sessions = Vec::with_capacity(LOGICAL_SESSIONS);
        for (headers, _, _) in &contexts {
            sessions.push(
                pool.prepare_session(&client, &route, &channel, headers, "live-count")
                    .await?,
            );
        }
        let pooled_prepare_ms = pooled_started.elapsed().as_millis();
        let pooled_connection_ids = sessions
            .iter()
            .filter_map(|session| {
                session
                    .preferred
                    .as_ref()
                    .map(|connection| connection.id.clone())
            })
            .collect::<HashSet<_>>();
        for (session, (_, session_id, thread_id)) in sessions.iter_mut().zip(contexts.iter()) {
            let request = live_warmup_request(&model, session_id, thread_id, None);
            let (status, _) = live_terminal_status(session.start_turn(&request).await?).await?;
            if !(200..300).contains(&status) {
                anyhow::bail!("pooled warmup failed with status {status}");
            }
        }
        let pooled_handshake_ms = sessions
            .iter()
            .filter_map(|session| session.preferred.as_ref())
            .map(|connection| (connection.id.clone(), connection.dial_ms))
            .collect::<HashMap<_, _>>()
            .into_values()
            .map(u128::from)
            .sum::<u128>();
        let report = serde_json::json!({
            "logical_sessions": LOGICAL_SESSIONS,
            "baseline_physical_wss": LOGICAL_SESSIONS,
            "baseline_handshake_elapsed_ms": baseline_handshake_ms,
            "pooled_physical_wss": pooled_connection_ids.len(),
            "pooled_handshake_ms_total": pooled_handshake_ms,
            "pooled_prepare_elapsed_ms": pooled_prepare_ms,
            "requests_generate_model_output": false
        });
        println!("WS_DISTINCT_CONTEXT_ISOLATION_EVIDENCE={report}");
        if pooled_connection_ids.len() != LOGICAL_SESSIONS {
            anyhow::bail!(
                "distinct handshake contexts were unexpectedly merged: {report}"
            );
        }
        pool.shutdown();
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires an explicit development config and performs three bounded subscription requests"]
    async fn live_openai_subscription_pool_capability() -> Result<()> {
        let Some(config_path) = std::env::var_os("CONST_API_LIVE_OPENAI_WS_CONFIG_PATH") else {
            eprintln!(
                "live WebSocket pool test skipped: CONST_API_LIVE_OPENAI_WS_CONFIG_PATH is not set"
            );
            return Ok(());
        };
        let config_path = std::path::PathBuf::from(config_path);
        let config = crate::config::load_config_from_path(&config_path)?;
        let mut channel = config
            .channels
            .into_iter()
            .find(|channel| {
                channel.enabled
                    && channel.source_driver()
                        == crate::source_driver::SourceDriverId::OpenAiSubscription
            })
            .context("no enabled OpenAI subscription channel in the development config")?;
        let model = std::env::var("CONST_API_LIVE_OPENAI_WS_MODEL")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .or_else(|| {
                (!channel.upstream_model.trim().is_empty())
                    .then(|| channel.upstream_model.trim().to_string())
            })
            .or_else(|| channel.models.first().cloned())
            .context("OpenAI subscription model is unavailable")?;
        if !channel.models.iter().any(|candidate| candidate == &model) {
            anyhow::bail!("selected model is not declared by the OpenAI subscription channel");
        }
        channel.subscription.responses_ws_pool_enabled = true;
        channel
            .subscription
            .responses_ws_subscription_multiplex_probe_enabled = true;
        let route = ResponsesWebsocketNativeRoute::OpenAiSubscription {
            channel: Box::new(channel.clone()),
        };
        let mut headers = codex_model_identity_headers(&active_codex_identity());
        let session_id = format!("live-ws-{:032x}", rand::random::<u128>());
        let thread_id = format!("live-thread-{:032x}", rand::random::<u128>());
        let installation_id = format!("{:064x}", rand::random::<u128>());
        headers.insert("session-id", session_id.parse()?);
        headers.insert("thread-id", thread_id.parse()?);
        headers.insert("x-client-request-id", thread_id.parse()?);
        headers.insert("x-codex-installation-id", installation_id.parse()?);
        headers.insert(
            "x-codex-window-id",
            format!("{thread_id}:0").parse()?,
        );
        headers.insert(
            "openai-beta",
            reqwest::header::HeaderValue::from_static(RESPONSES_WEBSOCKET_BETA),
        );
        let client = Client::new();
        let pool = ResponsesWsPool::default();
        let started = std::time::Instant::now();
        let mut first = pool
            .prepare_session(&client, &route, &channel, &headers, "live-test")
            .await?;
        let request = |label: &str, previous_response_id: Option<&str>| {
            let mut value = serde_json::json!({
                "type": "response.create",
                "model": model,
                "instructions": "Reply exactly OK.",
                "input": [{
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": format!("CONST API WebSocket pool acceptance {label}. Reply exactly OK.")
                    }]
                }],
                "tools": [],
                "tool_choice": "auto",
                "parallel_tool_calls": false,
                "reasoning": {"effort": "low", "summary": "auto"},
                "include": [],
                "prompt_cache_key": session_id,
                "client_metadata": {
                    "session_id": session_id,
                    "thread_id": thread_id,
                    "x-codex-installation-id": installation_id,
                    "x-codex-window-id": format!("{thread_id}:0")
                },
                "store": false,
                "stream": true
            });
            if let Some(previous_response_id) = previous_response_id {
                value["previous_response_id"] = previous_response_id.into();
            }
            value.to_string()
        };

        // start_turn performs one non-generating capability probe before it
        // submits this first actual request.
        let first_request = request("lane-a", None);
        let first_turn = first.start_turn(&first_request).await?;
        let capability = first.bucket.capability.value.load(Ordering::Acquire);
        let (first_status, second_status, second) =
            if capability == RESPONSES_WS_CAPABILITY_MULTIPLEX {
                let mut second = pool
                    .prepare_session(&client, &route, &channel, &headers, "live-test")
                    .await?;
                let second_request = request("lane-b", None);
                let second_turn = second.start_turn(&second_request).await?;
                let (first_status, second_status) = tokio::join!(
                    live_terminal_status(first_turn),
                    live_terminal_status(second_turn)
                );
                (first_status?.0, second_status?.0, second)
            } else {
                // The private subscription endpoint may accept but not echo
                // stream_id. In that case prove ctx_pool reuse sequentially;
                // concurrent multiplex would be unsafe without a routing key.
                let (first_status, previous_response_id) =
                    live_terminal_status(first_turn).await?;
                let previous_response_id = previous_response_id.with_context(|| {
                    format!(
                        "exclusive pool response did not include a response id status={first_status}"
                    )
                })?;
                let mut second = pool
                    .prepare_session(&client, &route, &channel, &headers, "live-test")
                    .await?;
                let second_request = request("continuation", Some(&previous_response_id));
                let (second_status, _) =
                    live_terminal_status(second.start_turn(&second_request).await?).await?;
                (first_status, second_status, second)
            };
        let first_connection = first
            .preferred
            .as_ref()
            .map(|connection| connection.id.clone())
            .context("first pooled connection is unavailable")?;
        let second_connection = second
            .preferred
            .as_ref()
            .map(|connection| connection.id.clone())
            .context("second pooled connection is unavailable")?;
        if first_connection != second_connection {
            anyhow::bail!("live sessions did not reuse one physical WebSocket");
        }
        if !(200..300).contains(&first_status) || !(200..300).contains(&second_status)
        {
            anyhow::bail!(
                "live pool returned non-success status request_a={first_status} request_b={second_status}"
            );
        }
        let connection_count = first.bucket.state.lock().await.connections.len();
        if connection_count != 1 {
            anyhow::bail!(
                "live multiplex used {connection_count} physical connections instead of one"
            );
        }
        eprintln!(
            "live WebSocket pool accepted: mode={} physical_connections=1 probe_requests=1 actual_requests=2 request_statuses={}/{} elapsed_ms={}",
            first.bucket.capability_label(),
            first_status,
            second_status,
            started.elapsed().as_millis(),
        );
        pool.shutdown();
        Ok(())
    }
}
