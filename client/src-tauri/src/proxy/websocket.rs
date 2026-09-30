const RESPONSES_WEBSOCKET_FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(30);
const RESPONSES_WEBSOCKET_MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
const RESPONSES_WEBSOCKET_BETA: &str = "responses_websockets=2026-02-06";
const DUPLEX_WEBSOCKET_FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(30);
const DUPLEX_WEBSOCKET_MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
const WEBSOCKET_UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const WEBSOCKET_CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Default)]
struct UpstreamWebsocketCounts {
    total: usize,
    active_total: usize,
    by_route: HashMap<String, usize>,
    active_by_route: HashMap<String, usize>,
}

fn upstream_websocket_counts() -> &'static std::sync::Mutex<UpstreamWebsocketCounts> {
    static COUNTS: std::sync::OnceLock<std::sync::Mutex<UpstreamWebsocketCounts>> =
        std::sync::OnceLock::new();
    COUNTS.get_or_init(|| std::sync::Mutex::new(UpstreamWebsocketCounts::default()))
}

fn websocket_request_source(headers: &warp::http::HeaderMap) -> String {
    for name in ["originator", "user-agent"] {
        let Some(value) = headers.get(name).and_then(|value| value.to_str().ok()) else {
            continue;
        };
        let candidate = if name == "user-agent" {
            value
                .trim()
                .split(|character: char| character.is_ascii_whitespace() || character == '(')
                .next()
                .unwrap_or_default()
        } else {
            value.trim()
        };
        if !candidate.is_empty()
            && candidate.len() <= 120
            && candidate.chars().all(|character| {
                character.is_ascii_alphanumeric()
                    || character.is_ascii_whitespace()
                    || "._:/-".contains(character)
            })
        {
            return candidate
                .split_whitespace()
                .collect::<Vec<_>>()
                .join("_");
        }
    }
    "unknown".to_string()
}

struct UpstreamWebsocketLease {
    id: String,
    route_key: String,
    operation: String,
    channel_id: String,
    request_source: String,
    opened_at: std::time::Instant,
    turns: u64,
    active_turns: usize,
    close_reason: &'static str,
    closed_age_ms: Option<u64>,
}

impl UpstreamWebsocketLease {
    fn open(
        operation: impl Into<String>,
        channel_id: impl Into<String>,
        request_source: impl Into<String>,
    ) -> Self {
        let operation = operation.into();
        let channel_id = channel_id.into();
        let request_source = request_source.into();
        let route_key = format!("{operation}:{channel_id}");
        let (route_open, total_open) = {
            let mut counts = upstream_websocket_counts()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            counts.total = counts.total.saturating_add(1);
            let route_open = {
                let value = counts.by_route.entry(route_key.clone()).or_default();
                *value = value.saturating_add(1);
                *value
            };
            (route_open, counts.total)
        };
        let lease = Self {
            id: format!("uws-{:032x}", rand::random::<u128>()),
            route_key,
            operation,
            channel_id,
            request_source,
            opened_at: std::time::Instant::now(),
            turns: 0,
            active_turns: 0,
            close_reason: "scope_ended",
            closed_age_ms: None,
        };
        log::debug!(
            "[const-api][upstream-ws] opened connection_id={} operation={} channel_id={} request_source={} route_open={} total_open={}",
            lease.id,
            lease.operation,
            lease.channel_id,
            lease.request_source,
            route_open,
            total_open,
        );
        lease
    }

    fn mark_turn(&mut self) {
        if self.closed_age_ms.is_some() {
            return;
        }
        self.turns = self.turns.saturating_add(1);
        self.active_turns = self.active_turns.saturating_add(1);
        let mut counts = upstream_websocket_counts()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        counts.active_total = counts.active_total.saturating_add(1);
        let value = counts
            .active_by_route
            .entry(self.route_key.clone())
            .or_default();
        *value = value.saturating_add(1);
    }

    fn finish_turn(&mut self) {
        if self.active_turns == 0 {
            return;
        }
        self.active_turns = self.active_turns.saturating_sub(1);
        let mut counts = upstream_websocket_counts()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        counts.active_total = counts.active_total.saturating_sub(1);
        let mut remove = false;
        if let Some(value) = counts.active_by_route.get_mut(&self.route_key) {
            *value = value.saturating_sub(1);
            remove = *value == 0;
        }
        if remove {
            counts.active_by_route.remove(&self.route_key);
        }
    }

    fn set_close_reason(&mut self, reason: &'static str) {
        if self.closed_age_ms.is_none() {
            self.close_reason = reason;
        }
    }

    fn attach(&self, payload: &mut serde_json::Map<String, serde_json::Value>) {
        attach_upstream_websocket_count_snapshot(payload, &self.route_key);
        if payload.get("failure_stage").and_then(serde_json::Value::as_str) == Some("upstream_connect") {
            // A failed replacement dial has no new lease. Do not attribute it
            // to the previous connection's normal pool-retirement reason.
            payload.insert("request_source".into(), self.request_source.clone().into());
            return;
        }
        payload.insert("upstream_connection_id".to_string(), self.id.clone().into());
        payload.insert(
            "upstream_connection_age_ms".to_string(),
            serde_json::json!(self.opened_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64),
        );
        payload.insert(
            "upstream_connection_turns".to_string(),
            serde_json::json!(self.turns),
        );
        if let Some(age_ms) = self.closed_age_ms {
            payload.insert("upstream_connection_closed".to_string(), true.into());
            payload.insert("upstream_connection_closed_age_ms".to_string(), age_ms.into());
            payload.insert("upstream_close_reason".to_string(), self.close_reason.into());
        }
        payload.insert(
            "request_source".to_string(),
            self.request_source.clone().into(),
        );
    }

    fn log_terminal(&self, metadata: &ResponsesWebsocketTerminalMetadata) {
        if (200..300).contains(&metadata.status) {
            return;
        }
        log::warn!(
            "[const-api][upstream-ws] terminal connection_id={} operation={} channel_id={} request_source={} status={} event_type={} error_type={} error_code={} error_param={} provider_request_id={} retry_after_seconds={} turns={} age_ms={}",
            self.id,
            self.operation,
            self.channel_id,
            self.request_source,
            metadata.status,
            metadata.event_type,
            metadata.error_type.as_deref().unwrap_or("-"),
            metadata.error_code.as_deref().unwrap_or("-"),
            metadata.error_param.as_deref().unwrap_or("-"),
            metadata.provider_request_id.as_deref().unwrap_or("-"),
            metadata
                .retry_after_seconds
                .map(|value| value.to_string())
                .as_deref()
                .unwrap_or("-"),
            self.turns,
            self.opened_at.elapsed().as_millis(),
        );
    }
}

impl UpstreamWebsocketLease {
    fn close(&mut self) {
        if self.closed_age_ms.is_some() {
            return;
        }
        self.closed_age_ms = Some(duration_millis_u64(self.opened_at.elapsed()));
        while self.active_turns > 0 {
            self.finish_turn();
        }
        let (route_open, total_open) = {
            let mut counts = upstream_websocket_counts()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            counts.total = counts.total.saturating_sub(1);
            let mut remove = false;
            let route_open = if let Some(value) = counts.by_route.get_mut(&self.route_key) {
                *value = value.saturating_sub(1);
                remove = *value == 0;
                *value
            } else {
                0
            };
            if remove {
                counts.by_route.remove(&self.route_key);
            }
            (route_open, counts.total)
        };
        log::info!(
            "[const-api][upstream-ws] closed connection_id={} operation={} channel_id={} request_source={} reason={} turns={} age_ms={} route_open={} total_open={}",
            self.id,
            self.operation,
            self.channel_id,
            self.request_source,
            self.close_reason,
            self.turns,
            self.opened_at.elapsed().as_millis(),
            route_open,
            total_open,
        );
    }
}

impl Drop for UpstreamWebsocketLease {
    fn drop(&mut self) {
        self.close();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResponsesWebsocketTerminalMetadata {
    status: u16,
    event_type: String,
    error_type: Option<String>,
    error_code: Option<String>,
    error_param: Option<String>,
    provider_request_id: Option<String>,
    retry_after_seconds: Option<u64>,
}

fn bounded_websocket_diagnostic_value(value: Option<&serde_json::Value>) -> Option<String> {
    value
        .and_then(serde_json::Value::as_str)
        .and_then(bounded_websocket_diagnostic_string)
}

fn bounded_websocket_diagnostic_string(value: &str) -> Option<String> {
    let value = value.trim();
    if !value.is_empty()
        && value.len() <= 256
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._:-/".contains(character))
    {
        Some(value.to_string())
    } else {
        None
    }
}

fn websocket_json_header_value<'a>(
    event: &'a serde_json::Value,
    names: &[&str],
) -> Option<&'a serde_json::Value> {
    let headers = event.get("headers")?.as_object()?;
    headers.iter().find_map(|(name, value)| {
        names
            .iter()
            .any(|candidate| name.eq_ignore_ascii_case(candidate))
            .then_some(value)
    })
}

fn websocket_retry_after_seconds(value: Option<&serde_json::Value>) -> Option<u64> {
    let value = value?;
    value
        .as_u64()
        .or_else(|| {
            value
                .as_f64()
                .filter(|value| value.is_finite() && *value > 0.0)
                .map(|value| value.ceil() as u64)
        })
        .or_else(|| {
            value
                .as_str()
                .and_then(|value| value.trim().parse::<f64>().ok())
                .filter(|value| value.is_finite() && *value > 0.0)
                .map(|value| value.ceil() as u64)
        })
}

fn responses_websocket_connect_error_metadata(
    error: &anyhow::Error,
) -> ResponsesWebsocketTerminalMetadata {
    let mut metadata = ResponsesWebsocketTerminalMetadata {
        status: responses_websocket_error_status(error).unwrap_or(502),
        event_type: "websocket.handshake_error".to_string(),
        error_type: None,
        error_code: None,
        error_param: None,
        provider_request_id: None,
        retry_after_seconds: None,
    };
    for source in error.chain() {
        let Some(tokio_tungstenite::tungstenite::Error::Http(response)) = source
            .downcast_ref::<tokio_tungstenite::tungstenite::Error>()
        else {
            continue;
        };
        if let Some(value) = response
            .body()
            .as_ref()
            .and_then(|body| serde_json::from_slice::<serde_json::Value>(body).ok())
        {
            let provider_error = value
                .get("error")
                .or_else(|| value.pointer("/body/error"))
                .unwrap_or(&value);
            metadata.error_type = bounded_websocket_diagnostic_value(provider_error.get("type"));
            metadata.error_code = bounded_websocket_diagnostic_value(provider_error.get("code"));
            metadata.error_param = bounded_websocket_diagnostic_value(provider_error.get("param"));
            metadata.provider_request_id = bounded_websocket_diagnostic_value(
                value
                    .get("request_id")
                    .or_else(|| value.pointer("/body/request_id"))
                    .or_else(|| provider_error.get("request_id")),
            );
            metadata.retry_after_seconds = websocket_retry_after_seconds(
                value
                    .get("retry_after_seconds")
                    .or_else(|| value.pointer("/body/retry_after_seconds"))
                    .or_else(|| provider_error.get("retry_after_seconds")),
            );
        }
        if metadata.provider_request_id.is_none() {
            metadata.provider_request_id = ["x-request-id", "request-id", "cf-ray"]
                .into_iter()
                .find_map(|name| {
                    response
                        .headers()
                        .get(name)
                        .and_then(|value| value.to_str().ok())
                        .and_then(bounded_websocket_diagnostic_string)
                });
        }
        if metadata.retry_after_seconds.is_none() {
            metadata.retry_after_seconds = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<f64>().ok())
                .filter(|value| value.is_finite() && *value > 0.0)
                .map(|value| value.ceil() as u64);
        }
        break;
    }
    metadata
}

fn responses_websocket_connect_error_kind(
    metadata: &ResponsesWebsocketTerminalMetadata,
) -> String {
    metadata
        .error_code
        .as_ref()
        .or(metadata.error_type.as_ref())
        .cloned()
        .unwrap_or_else(|| match metadata.status {
            401 => "upstream_websocket_unauthorized".to_string(),
            403 => "upstream_websocket_forbidden".to_string(),
            404 | 405 | 426 => "upstream_websocket_unsupported".to_string(),
            429 => "upstream_websocket_rate_limited".to_string(),
            _ => "upstream_websocket_connect_failed".to_string(),
        })
}

fn attach_upstream_websocket_count_snapshot(
    payload: &mut serde_json::Map<String, serde_json::Value>,
    route_key: &str,
) {
    let (route_open, route_active, total_open, total_active) = {
        let counts = upstream_websocket_counts()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (
            counts.by_route.get(route_key).copied().unwrap_or_default(),
            counts
                .active_by_route
                .get(route_key)
                .copied()
                .unwrap_or_default(),
            counts.total,
            counts.active_total,
        )
    };
    for (key, value) in [
        ("upstream_open_connections", route_open),
        ("upstream_active_turns", route_active),
        ("upstream_process_open_connections", total_open),
        ("upstream_process_active_turns", total_active),
    ] {
        payload.insert(key.to_string(), serde_json::json!(value));
    }
}

fn responses_websocket_terminal_metadata_from_value(
    event: &serde_json::Value,
) -> Option<ResponsesWebsocketTerminalMetadata> {
    let event_type = event.get("type").and_then(serde_json::Value::as_str)?;
    let default_status = match event_type {
        "response.completed" | "response.incomplete" => 200,
        "response.cancelled" => 499,
        "response.failed" | "error" => 502,
        _ => return None,
    };
    let error = event
        .get("error")
        .or_else(|| event.pointer("/body/error"))
        .or_else(|| event.pointer("/response/error"))
        .or_else(|| event.pointer("/response/status_details/error"));
    let error_code = bounded_websocket_diagnostic_value(
        error
            .and_then(|value| value.get("code"))
            .or_else(|| event.get("code")),
    );
    let error_type = bounded_websocket_diagnostic_value(
        error
            .and_then(|value| value.get("type"))
            .or_else(|| event.get("error_type")),
    );
    let error_param = bounded_websocket_diagnostic_value(
        error
            .and_then(|value| value.get("param"))
            .or_else(|| event.get("param")),
    );
    let status = event
        .get("status")
        .or_else(|| event.get("status_code"))
        .or_else(|| event.pointer("/body/status"))
        .or_else(|| event.pointer("/body/status_code"))
        .or_else(|| error.and_then(|value| value.get("status")))
        .or_else(|| error.and_then(|value| value.get("status_code")))
        .and_then(serde_json::Value::as_u64)
        .and_then(|status| u16::try_from(status).ok())
        .unwrap_or_else(|| match error_code.as_deref() {
            Some("rate_limit_exceeded" | "slow_down") => 429,
            Some("server_is_overloaded" | "websocket_connection_limit_reached") => 503,
            Some("context_length_exceeded" | "invalid_request_error" | "previous_response_not_found") => 400,
            _ => default_status,
        });
    let provider_request_id = bounded_websocket_diagnostic_value(
        event
            .get("request_id")
            .or_else(|| event.get("requestId"))
            .or_else(|| event.pointer("/body/request_id"))
            .or_else(|| error.and_then(|value| value.get("request_id")))
            .or_else(|| event.pointer("/response/request_id"))
            .or_else(|| {
                websocket_json_header_value(
                    event,
                    &["x-request-id", "request-id", "cf-ray"],
                )
            }),
    );
    let retry_after_seconds = websocket_retry_after_seconds(
        event
            .get("retry_after_seconds")
            .or_else(|| event.pointer("/body/retry_after_seconds"))
            .or_else(|| error.and_then(|value| value.get("retry_after_seconds")))
            .or_else(|| websocket_json_header_value(event, &["retry-after"])),
    );
    Some(ResponsesWebsocketTerminalMetadata {
        status,
        event_type: event_type.to_string(),
        error_type,
        error_code,
        error_param,
        provider_request_id,
        retry_after_seconds,
    })
}

fn responses_websocket_terminal_metadata(
    text: &str,
) -> Option<ResponsesWebsocketTerminalMetadata> {
    let event = serde_json::from_str::<serde_json::Value>(text).ok()?;
    responses_websocket_terminal_metadata_from_value(&event)
}

fn attach_responses_websocket_terminal_metadata(
    payload: &mut serde_json::Map<String, serde_json::Value>,
    metadata: &ResponsesWebsocketTerminalMetadata,
) {
    payload.insert(
        "provider_event_type".to_string(),
        serde_json::Value::String(metadata.event_type.clone()),
    );
    if let Some(value) = metadata.error_type.as_ref() {
        payload.insert("error_type".to_string(), value.clone().into());
    }
    if let Some(value) = metadata.error_code.as_ref() {
        payload.insert("error_code".to_string(), value.clone().into());
        if value == "previous_response_not_found" {
            payload
                .entry("failure_stage".to_string())
                .or_insert_with(|| "continuation_reference".into());
        }
    }
    if let Some(value) = metadata.error_param.as_ref() {
        payload.insert("error_param".to_string(), value.clone().into());
    }
    if let Some(value) = metadata.provider_request_id.as_ref() {
        payload.insert("provider_request_id".to_string(), value.clone().into());
    }
    if let Some(value) = metadata.retry_after_seconds {
        payload.insert("retry_after_seconds".to_string(), value.into());
    }
    if !(200..300).contains(&metadata.status) {
        payload.insert(
            "error_kind".to_string(),
            metadata
                .error_code
                .as_ref()
                .or(metadata.error_type.as_ref())
                .cloned()
                .unwrap_or_else(|| metadata.event_type.clone())
                .into(),
        );
        payload
            .entry("failure_scope".to_string())
            .or_insert_with(|| "request".into());
    }
}

fn attach_responses_websocket_failure_observation(
    payload: &mut serde_json::Map<String, serde_json::Value>,
    text: &str,
    model: Option<&str>,
) {
    if let Some(mut failure) = crate::upstream_failure::observe(200, text, &[], model) {
        if failure.retry_after_seconds.is_none() {
            failure.retry_after_seconds = payload.get("retry_after_seconds").and_then(serde_json::Value::as_i64);
            if failure.retry_after_seconds.is_some() { failure.retry_source = "provider_event".into(); }
        }
        failure.attach(payload);
    }
}

fn supplier_responses_websocket_connect_error_payload(
    error: &anyhow::Error,
    channel_id: &str,
    request_source: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let metadata = responses_websocket_connect_error_metadata(error);
    let error_kind = responses_websocket_connect_error_kind(&metadata);
    let mut payload = serde_json::json!({
        "status": metadata.status,
        "turn_terminal": true,
        "failure_scope": if metadata.status == 401 { "channel" } else { "operation" },
        "error_kind": error_kind.clone(),
        "error_code": error_kind.clone(),
        "message": "Responses WebSocket upstream handshake failed",
        "request_source": request_source,
    })
    .as_object()
    .cloned()
    .unwrap_or_default();
    attach_responses_websocket_terminal_metadata(&mut payload, &metadata);
    payload.insert(
        "failure_scope".to_string(),
        if metadata.status == 401 {
            "channel".into()
        } else {
            "operation".into()
        },
    );
    payload.insert("error_kind".to_string(), error_kind.clone().into());
    payload.insert("error_code".to_string(), error_kind.into());
    attach_upstream_websocket_count_snapshot(
        &mut payload,
        &format!("openai_responses:{channel_id}"),
    );
    payload
}

async fn bounded_websocket_connect<F, T, E>(
    connect: F,
    timeout: Duration,
) -> Option<std::result::Result<T, E>>
where
    F: std::future::Future<Output = std::result::Result<T, E>>,
{
    tokio::time::timeout(timeout, connect).await.ok()
}

type ResponsesUpstreamSocket = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

// Boxing the native WebSocket would add a heap allocation to every duplex
// session; the enum is short-lived and kept on the owning task's stack.
#[allow(clippy::large_enum_variant)]
enum ResponsesRelayConnection {
    WebSocket(ResponsesUpstreamSocket),
    Platform(crate::supplier::PlatformDuplexSession),
}

enum ResponsesRelaySender {
    WebSocket(
        futures_util::stream::SplitSink<
            ResponsesUpstreamSocket,
            tokio_tungstenite::tungstenite::Message,
        >,
    ),
    Platform(crate::supplier::PlatformDuplexSender),
}

enum ResponsesRelayReceiver {
    WebSocket(futures_util::stream::SplitStream<ResponsesUpstreamSocket>),
    Platform(crate::supplier::PlatformDuplexReceiver),
}

impl ResponsesRelayConnection {
    fn split(self) -> (ResponsesRelaySender, ResponsesRelayReceiver) {
        match self {
            Self::WebSocket(socket) => {
                let (sender, receiver) = socket.split();
                (
                    ResponsesRelaySender::WebSocket(sender),
                    ResponsesRelayReceiver::WebSocket(receiver),
                )
            }
            Self::Platform(session) => {
                let (sender, receiver) = session.split();
                (
                    ResponsesRelaySender::Platform(sender),
                    ResponsesRelayReceiver::Platform(receiver),
                )
            }
        }
    }
}

impl ResponsesRelaySender {
    async fn send(&mut self, message: tokio_tungstenite::tungstenite::Message) -> Result<()> {
        match self {
            Self::WebSocket(sender) => sender.send(message).await.map_err(Into::into),
            Self::Platform(sender) => {
                use crate::supplier::PlatformDuplexFrame;
                let frame = match message {
                    tokio_tungstenite::tungstenite::Message::Text(text) => {
                        PlatformDuplexFrame::Text(text.to_string())
                    }
                    tokio_tungstenite::tungstenite::Message::Binary(bytes) => {
                        PlatformDuplexFrame::Binary(bytes.to_vec())
                    }
                    tokio_tungstenite::tungstenite::Message::Close(frame) => {
                        let (code, reason) = frame
                            .map(|frame| (u16::from(frame.code), frame.reason.to_string()))
                            .unwrap_or((1000, String::new()));
                        PlatformDuplexFrame::Close { code, reason }
                    }
                    tokio_tungstenite::tungstenite::Message::Ping(_)
                    | tokio_tungstenite::tungstenite::Message::Pong(_) => return Ok(()),
                    tokio_tungstenite::tungstenite::Message::Frame(_) => {
                        return Err(anyhow!("raw WebSocket frames cannot enter a logical duplex session"));
                    }
                };
                sender.send(frame).await
            }
        }
    }

    async fn close(&mut self) -> Result<()> {
        match self {
            Self::WebSocket(sender) => sender.close().await.map_err(Into::into),
            Self::Platform(sender) => {
                sender
                    .send(crate::supplier::PlatformDuplexFrame::Close {
                        code: 1000,
                        reason: String::new(),
                    })
                    .await
            }
        }
    }

    async fn close_bounded(&mut self) {
        let _ = tokio::time::timeout(WEBSOCKET_CLOSE_TIMEOUT, self.close()).await;
    }
}

async fn close_upstream_websocket_sender(
    sender: &mut futures_util::stream::SplitSink<
        ResponsesUpstreamSocket,
        tokio_tungstenite::tungstenite::Message,
    >,
) {
    let _ = tokio::time::timeout(WEBSOCKET_CLOSE_TIMEOUT, sender.close()).await;
}

async fn close_client_websocket_sender(
    sender: &mut futures_util::stream::SplitSink<warp::ws::WebSocket, warp::ws::Message>,
) {
    let _ = tokio::time::timeout(WEBSOCKET_CLOSE_TIMEOUT, sender.close()).await;
}

impl ResponsesRelayReceiver {
    async fn next(
        &mut self,
    ) -> Option<Result<tokio_tungstenite::tungstenite::Message>> {
        match self {
            Self::WebSocket(receiver) => receiver.next().await.map(|message| message.map_err(Into::into)),
            Self::Platform(receiver) => receiver.next().await.map(|frame| {
                use crate::supplier::PlatformDuplexFrame;
                match frame? {
                    PlatformDuplexFrame::Text(text) => Ok(
                        tokio_tungstenite::tungstenite::Message::Text(text.into()),
                    ),
                    PlatformDuplexFrame::Binary(bytes) => Ok(
                        tokio_tungstenite::tungstenite::Message::Binary(bytes.into()),
                    ),
                    PlatformDuplexFrame::Close { code, reason } => Ok(
                        tokio_tungstenite::tungstenite::Message::Close(Some(
                            tokio_tungstenite::tungstenite::protocol::CloseFrame {
                                code: code.into(),
                                reason: reason.into(),
                            },
                        )),
                    ),
                }
            }),
        }
    }
}

#[derive(Clone)]
enum ResponsesWebsocketNativeRoute {
    HttpSurface {
        channel: Box<ChannelConfig>,
        target: crate::channel_surface::ChannelSurfaceTarget,
    },
    OpenAiSubscription {
        channel: Box<ChannelConfig>,
    },
}

enum ResponsesWebsocketLocalRoute {
    Native(ResponsesWebsocketNativeRoute),
    HttpBridge(&'static str),
    Unavailable,
}

enum ResponsesWebsocketRoute {
    Native(ResponsesWebsocketNativeRoute),
    Platform,
    HttpBridge(&'static str),
}

impl ResponsesWebsocketNativeRoute {
    fn channel(&self) -> &ChannelConfig {
        match self {
            Self::HttpSurface { channel, .. } | Self::OpenAiSubscription { channel } => channel,
        }
    }

    fn subscription_config(&self) -> Option<SupplierConfig> {
        match self {
            Self::OpenAiSubscription { channel } => Some(supplier_from_channel(channel)),
            Self::HttpSurface { .. } => None,
        }
    }
}

struct NativeSubscriptionWebsocketRequest {
    config: SupplierConfig,
    request_body: String,
    model: String,
    started_at: std::time::Instant,
    raw_sse: String,
    upstream_send_ms: Option<u64>,
    first_sse_event_ms: Option<u64>,
    first_meaningful_event_ms: Option<u64>,
    upstream_usage: Option<SupplierUpstreamUsageAccumulator>,
    terminal_metadata: Option<ResponsesWebsocketTerminalMetadata>,
    safety_guard: Option<SubscriptionSafetyGuard>,
    billable: bool,
    local_model_observation: Option<LocalModelOutcomeGuard>,
}

impl NativeSubscriptionWebsocketRequest {
    fn start_at(
        config: &SupplierConfig,
        request_body: &str,
        started_at: std::time::Instant,
    ) -> std::result::Result<Self, serde_json::Map<String, serde_json::Value>> {
        let value = serde_json::from_str::<serde_json::Value>(request_body).unwrap_or_default();
        let billable = value.get("generate").and_then(serde_json::Value::as_bool) != Some(false);
        let safety_guard = if billable {
            Some(subscription_safety_enter(config, request_body)?)
        } else {
            None
        };
        let requested_model = value
            .get("model")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let channel = channel_from_supplier("subscription-runtime".to_string(), config);
        let model = channel_upstream_model_for_request(&channel, Some(requested_model));
        Ok(Self {
            config: config.clone(),
            request_body: request_body.to_string(),
            model,
            started_at,
            raw_sse: String::new(),
            upstream_send_ms: None,
            first_sse_event_ms: None,
            first_meaningful_event_ms: None,
            upstream_usage: Some(SupplierUpstreamUsageAccumulator::new("openai_responses")),
            terminal_metadata: None,
            safety_guard,
            billable,
            local_model_observation: None,
        })
    }

    fn mark_sent(&mut self) {
        crate::supplier::availability::note_activity(&channel_from_supplier(self.config.channel_id.clone(), &self.config));
        self.upstream_send_ms = Some(
            self.started_at
                .elapsed()
                .as_millis()
                .min(u128::from(u64::MAX)) as u64,
        );
    }

    fn observe(&mut self, text: &str) -> Option<u16> {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(text) else {
            return None;
        };
        let elapsed_ms = self
            .started_at
            .elapsed()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        self.first_sse_event_ms.get_or_insert(elapsed_ms);
        if self.first_meaningful_event_ms.is_none()
            && responses_sse_event_is_meaningful(text)
        {
            self.first_meaningful_event_ms = Some(elapsed_ms);
        }
        if self.raw_sse.len().saturating_add(text.len()).saturating_add(32)
            <= crate::protocol::stream::MAX_STREAM_BYTES
        {
            let event_name = event
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("message");
            self.raw_sse.push_str("event: ");
            self.raw_sse.push_str(event_name);
            self.raw_sse.push_str("\ndata: ");
            self.raw_sse.push_str(text);
            self.raw_sse.push_str("\n\n");
        }
        if let Some(usage) = self.upstream_usage.as_mut() {
            usage.push(text.as_bytes());
            usage.push(b"\n");
        }
        let terminal = responses_websocket_terminal_metadata_from_value(&event);
        if let Some(terminal) = terminal.as_ref() {
            self.terminal_metadata = Some(terminal.clone());
        }
        terminal.map(|terminal| terminal.status)
    }

    fn finalize(
        mut self,
        status: u16,
        failure_scope: Option<&str>,
        shared: Option<&ProxyShared>,
        channel: &ChannelConfig,
    ) {
        if !self.billable {
            if let Some(shared) = shared {
                if (200..300).contains(&status) {
                    shared.set_local_channel_ready(&channel.id, true);
                } else if failure_scope == Some("channel") {
                    shared.set_local_channel_ready(&channel.id, false);
                }
            }
            return;
        }
        let mut payload = http_response_payload(status, "text/event-stream", self.raw_sse.clone());
        payload.insert("stream".to_string(), serde_json::Value::Bool(true));
        payload.insert(
            "upstream_stream".to_string(),
            serde_json::Value::Bool(true),
        );
        payload.insert(
            "upstream_model".to_string(),
            serde_json::Value::String(self.model.clone()),
        );
        payload.insert(
            "inbound_request_bytes".to_string(),
            serde_json::json!(self.request_body.len()),
        );
        payload.insert(
            "upstream_request_bytes".to_string(),
            serde_json::json!(self.request_body.len()),
        );
        payload.insert(
            "response_wire_bytes".to_string(),
            serde_json::json!(self.raw_sse.len()),
        );
        payload.insert(
            "upstream_response_bytes".to_string(),
            serde_json::json!(self.raw_sse.len()),
        );
        payload.insert(
            "upstream_send_ms".to_string(),
            serde_json::json!(self.upstream_send_ms),
        );
        payload.insert(
            "first_body_chunk_ms".to_string(),
            serde_json::json!(self.first_sse_event_ms),
        );
        payload.insert(
            "first_sse_event_ms".to_string(),
            serde_json::json!(self.first_sse_event_ms),
        );
        payload.insert(
            "first_meaningful_event_ms".to_string(),
            serde_json::json!(self.first_meaningful_event_ms),
        );
        payload.insert(
            "routing_latency_ms".to_string(),
            serde_json::json!(
                self.first_meaningful_event_ms
                    .or(self.first_sse_event_ms)
            ),
        );
        if let Some(scope) = failure_scope {
            payload.insert(
                "failure_scope".to_string(),
                serde_json::Value::String(scope.to_string()),
            );
        }
        if let Some(metadata) = self.terminal_metadata.as_ref() {
            attach_responses_websocket_terminal_metadata(&mut payload, metadata);
        }
        if status == 499 {
            payload.insert(
                "error_kind".to_string(),
                serde_json::Value::String("downstream_cancelled".to_string()),
            );
        } else if !(200..300).contains(&status) {
            insert_subscription_route_model_failure_evidence(
                &mut payload,
                status,
                &self.raw_sse,
                Some(&self.model),
            );
        }
        let (upstream_usage, terminal_failure) = self
            .upstream_usage
            .take()
            .map(|usage| usage.finish_observed(Some(&self.model)))
            .unwrap_or_default();
        let observed_status = if status == 499 { status } else {
            terminal_failure.as_ref().map(|failure| failure.effective_status()).unwrap_or(status)
        };
        if status != 499 {
            if let Some(failure) = terminal_failure { failure.attach(&mut payload); }
        }
        attach_upstream_usage(&mut payload, upstream_usage.as_ref());
        let elapsed = self.started_at.elapsed();
        let _ = append_subscription_audit(
            &self.config,
            "/v1/responses",
            &self.request_body,
            &payload,
            elapsed,
        );
        self.safety_guard.take();

        let status_code = StatusCode::from_u16(observed_status).unwrap_or(StatusCode::BAD_GATEWAY);
        if status_code.is_success() {
            if let Some(observation) = &self.local_model_observation {
                observation.success();
            }
        }
        let evidence = local_model_failure_evidence_from_payload(&payload);
        if let Some(shared) = shared {
            let model_scoped = shared.record_local_model_response(
                &channel.id,
                &self.model,
                status_code,
                &warp::http::HeaderMap::new(),
                evidence.as_ref(),
            );
            let channel_scoped = payload
                .get("failure_scope")
                .and_then(serde_json::Value::as_str)
                == Some("channel");
            if let Some(ready) =
                local_channel_readiness_observation(status_code, model_scoped, channel_scoped)
            {
                shared.set_local_channel_ready(&channel.id, ready);
            }
        }
    }

    fn upstream_usage_snapshot(&self) -> Option<SupplierUpstreamUsage> {
        self.upstream_usage.clone().and_then(|usage| usage.finish())
    }
}

fn select_responses_websocket_local_route(
    shared: &ProxyShared,
    client_headers: &warp::http::HeaderMap,
    first_body: &[u8],
) -> ResponsesWebsocketLocalRoute {
    let config = shared.config_snapshot();
    let skip_local = client_headers
        .get(SKIP_LOCAL_SHORT_CIRCUIT_HEADER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| matches!(value, "1" | "true" | "yes"));
    if skip_local {
        return ResponsesWebsocketLocalRoute::Unavailable;
    }
    let force_local = client_headers
        .get(USE_LOCAL_SHORT_CIRCUIT_HEADER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| matches!(value, "1" | "true" | "yes"));
    let platform_authenticated = shared
        .platform_api_key
        .lock()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    if !force_local && platform_authenticated && !config.prefer_local_supply {
        return ResponsesWebsocketLocalRoute::Unavailable;
    }

    let ready_channel_ids = shared.ready_local_channel_ids();
    let Some(http_route) = crate::surface::resolve_api_route("POST", "/v1/responses", false)
    else {
        return ResponsesWebsocketLocalRoute::HttpBridge("unsupported_local_surface");
    };
    let owned = match select_resource_owned_local_channel(
        &shared.local_resource_owners,
        &config,
        &http_route,
        "/v1/responses",
        first_body,
        &ready_channel_ids,
    ) {
        Ok(selection) => selection.map(|selection| selection.channel.clone()),
        // The normal HTTP router owns the structured affinity error path. Do
        // not silently switch a stateful request to another native channel.
        Err(_) => {
            return ResponsesWebsocketLocalRoute::HttpBridge(
                "local_resource_owner_constraint",
            );
        }
    };
    let Ok(model_quota_routes) = shared.local_model_quota_routes.lock() else {
        return ResponsesWebsocketLocalRoute::HttpBridge("local_route_state_unavailable");
    };
    let exact = owned.or_else(|| {
        select_ready_local_channel_with_model_quota_and_match(
            &config,
            &warp::http::Method::POST,
            "/v1/responses",
            first_body,
            &ready_channel_ids,
            &model_quota_routes,
            now_unix(),
            ModelMatchMode::ExactOnly,
        )
        .ok()
        .flatten()
        .map(|selection| selection.channel.clone())
    });

    let Some(selection) = exact else {
        let compatible = select_ready_local_channel_with_model_quota_and_match(
            &config,
            &warp::http::Method::POST,
            "/v1/responses",
            first_body,
            &ready_channel_ids,
            &model_quota_routes,
            now_unix(),
            ModelMatchMode::CompatibleOnly,
        )
        .ok()
        .flatten();
        return if compatible.is_some() {
            ResponsesWebsocketLocalRoute::HttpBridge("local_compatible_route")
        } else {
            ResponsesWebsocketLocalRoute::Unavailable
        };
    };

    let Ok(execution_kind) =
        crate::channel_executor::execution_kind_for_source(selection.source_driver())
    else {
        return ResponsesWebsocketLocalRoute::HttpBridge("local_route_requires_http");
    };
    match execution_kind {
        crate::source_driver::ExecutionKind::OpenAiSubscription
            if crate::codex_identity::codex_request_is_native(Some(client_headers)) =>
        {
            ResponsesWebsocketLocalRoute::Native(
                ResponsesWebsocketNativeRoute::OpenAiSubscription {
                    channel: Box::new(selection),
                },
            )
        }
        crate::source_driver::ExecutionKind::HttpSurface => {
            if crate::detection::responses_features::operation_support(&selection.detection_checks, "openai.responses_websocket") == Some(false) {
                return ResponsesWebsocketLocalRoute::HttpBridge("local_websocket_unsupported");
            }
            let Some(target) = crate::channel_surface::channel_surface_target(
                &selection,
                crate::surface::ApiSurface::OpenAi,
                Some(crate::protocol::kind::ProtocolKind::OpenAiResponses),
                true,
            ) else {
                return ResponsesWebsocketLocalRoute::HttpBridge("local_http_route");
            };
            ResponsesWebsocketLocalRoute::Native(ResponsesWebsocketNativeRoute::HttpSurface {
                channel: Box::new(selection),
                target,
            })
        }
        _ => ResponsesWebsocketLocalRoute::HttpBridge("local_route_requires_http"),
    }
}

fn platform_responses_websocket_allowed(
    shared: &ProxyShared,
    client_headers: &warp::http::HeaderMap,
) -> bool {
    let force_local = client_headers
        .get(USE_LOCAL_SHORT_CIRCUIT_HEADER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| matches!(value, "1" | "true" | "yes"));
    if force_local {
        return false;
    }
    shared
        .platform_api_key
        .lock()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

fn select_responses_websocket_route(
    shared: &ProxyShared,
    client_headers: &warp::http::HeaderMap,
    first_body: &[u8],
) -> ResponsesWebsocketRoute {
    match select_responses_websocket_local_route(shared, client_headers, first_body) {
        ResponsesWebsocketLocalRoute::Native(route) => ResponsesWebsocketRoute::Native(route),
        ResponsesWebsocketLocalRoute::HttpBridge(reason) => {
            ResponsesWebsocketRoute::HttpBridge(reason)
        }
        ResponsesWebsocketLocalRoute::Unavailable
            if platform_responses_websocket_allowed(shared, client_headers) =>
        {
            ResponsesWebsocketRoute::Platform
        }
        ResponsesWebsocketLocalRoute::Unavailable => {
            ResponsesWebsocketRoute::HttpBridge("platform_duplex_unavailable")
        }
    }
}

async fn connect_platform_responses_websocket(
    shared: &ProxyShared,
    client_headers: &warp::http::HeaderMap,
) -> Result<(crate::supplier::PlatformDuplexSession, Endpoint, String)> {
    connect_platform_duplex(shared, client_headers, None).await
}

async fn connect_platform_duplex(
    shared: &ProxyShared,
    client_headers: &warp::http::HeaderMap,
    native: Option<(&str, &str)>,
) -> Result<(crate::supplier::PlatformDuplexSession, Endpoint, String)> {
    connect_platform_duplex_with_call(shared, client_headers, native, None).await
}

async fn connect_platform_duplex_with_call(
    shared: &ProxyShared,
    client_headers: &warp::http::HeaderMap,
    native: Option<(&str, &str)>,
    call_body: Option<&[u8]>,
) -> Result<(crate::supplier::PlatformDuplexSession, Endpoint, String)> {
    let config = shared.config_snapshot();
    let endpoints: Vec<_> = config
        .endpoints
        .iter()
        .filter(|endpoint| endpoint.enabled && !endpoint.base_url.trim().is_empty())
        .cloned()
        .collect();
    if endpoints.is_empty() {
        return Err(anyhow!("no enabled platform endpoint"));
    }
    let platform_api_key = shared
        .platform_api_key
        .lock()
        .map(|value| value.trim().to_string())
        .unwrap_or_default();
    if platform_api_key.is_empty() {
        return Err(anyhow!("platform credential is unavailable"));
    }
    let start = *shared.active_index.lock().await % endpoints.len();
    let mut last_error = None;
    for offset in 0..endpoints.len() {
        let index = (start + offset) % endpoints.len();
        let endpoint = &endpoints[index];
        let headers = match platform_request_headers(
            client_headers,
            &config,
            &platform_api_key,
            PlatformModelRouteMode::Configured,
            MODEL_REQUEST_MAX_UPSTREAM_ATTEMPTS,
        ) {
            Ok(headers) => headers,
            Err(error) => {
                last_error = Some(error);
                continue;
            }
        };
        let opened = if let Some((path, query)) = native {
            crate::supplier::open_platform_native_session(
                &shared.platform_duplex_pool, endpoint, shared.platform_access_token.clone(),
                shared.account_refresh_notify.clone(), headers, path, query, call_body,
            ).await
        } else {
            crate::supplier::open_platform_responses_duplex(
            &shared.platform_duplex_pool,
            endpoint,
            shared.platform_access_token.clone(),
            shared.account_refresh_notify.clone(),
            headers,
        )
            .await
        };
        match opened {
            Ok((session, transport)) => {
                *shared.active_index.lock().await = index;
                return Ok((session, endpoint.clone(), transport));
            }
            Err(error)
                if crate::supplier::platform_duplex_error_is_safe_to_replay(&error) =>
            {
                last_error = Some(error);
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("all platform duplex endpoints failed")))
}

fn responses_websocket_allows_http_bridge(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|event| {
            (event.get("type").and_then(serde_json::Value::as_str) == Some("error"))
                .then(|| {
                    event
                        .get("safe_to_retry_http_bridge")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
                })
        })
        .unwrap_or(false)
}

async fn connect_responses_websocket_request(
    request: tokio_tungstenite::tungstenite::http::Request<()>,
) -> Result<ResponsesUpstreamSocket> {
    let websocket_config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(RESPONSES_WEBSOCKET_MAX_MESSAGE_BYTES))
        .max_frame_size(Some(RESPONSES_WEBSOCKET_MAX_MESSAGE_BYTES));
    match bounded_websocket_connect(
        tokio_tungstenite::connect_async_with_config(request, Some(websocket_config), false),
        WEBSOCKET_UPSTREAM_CONNECT_TIMEOUT,
    )
    .await
    {
        Some(Ok((socket, _))) => Ok(socket),
        Some(Err(error)) if matches!(error, tokio_tungstenite::tungstenite::Error::Io(_) | tokio_tungstenite::tungstenite::Error::Tls(_)) => {
            let cause = responses_ws_io_cause(&error);
            Err(anyhow!(error).context(ResponsesWsConnectFailure { cause, timeout_ms: 0 }))
        }
        // HTTP rejection / protocol errors retain their original status and
        // classification; they are not evidence of a transient network fault.
        Some(Err(error)) => Err(anyhow!(error)),
        None => Err(anyhow!(ResponsesWsConnectFailure {
            cause: "connect_timeout",
            timeout_ms: duration_millis_u64(WEBSOCKET_UPSTREAM_CONNECT_TIMEOUT),
        })),
    }
}

async fn connect_responses_websocket_native_route(
    client: &Client,
    route: &ResponsesWebsocketNativeRoute,
    client_headers: &warp::http::HeaderMap,
) -> Result<ResponsesUpstreamSocket> {
    match route {
        ResponsesWebsocketNativeRoute::HttpSurface { channel, target } => {
            let request = responses_websocket_upstream_request(channel, target, client_headers)?;
            connect_responses_websocket_request(request).await
        }
        ResponsesWebsocketNativeRoute::OpenAiSubscription { channel } => {
            let (token, credential, _) =
                ensure_openai_subscription_access_token(client, channel).await?;
            let request = openai_subscription_websocket_request(
                &codex_responses_url(),
                &token,
                &credential,
                client_headers,
            )?;
            match connect_responses_websocket_request(request).await {
                Ok(socket) => Ok(socket),
                Err(error) if responses_websocket_error_status(&error) == Some(401) => {
                    let (token, credential, _) =
                        ensure_openai_subscription_access_token_with_options(
                            client,
                            channel,
                            Some(&token),
                            OPENAI_OAUTH_TOKEN_URL,
                        )
                        .await?;
                    let request = openai_subscription_websocket_request(
                        &codex_responses_url(),
                        &token,
                        &credential,
                        client_headers,
                    )?;
                    connect_responses_websocket_request(request).await
                }
                Err(error) => Err(error),
            }
        }
    }
}

async fn connect_openai_subscription_live(
    client: &Client,
    channel: &ChannelConfig,
    call_id: Option<&str>,
    raw_query: &str,
    client_headers: &warp::http::HeaderMap,
) -> Result<ResponsesUpstreamSocket> {
    let (token, credential, _) = ensure_openai_subscription_access_token(client, channel).await?;
    let request = openai_subscription_live_websocket_request(
        call_id,
        raw_query,
        &token,
        &credential,
        client_headers,
    )?;
    match connect_responses_websocket_request(request).await {
        Ok(socket) => Ok(socket),
        Err(error) if responses_websocket_error_status(&error) == Some(401) => {
            let (token, credential, _) = ensure_openai_subscription_access_token_with_options(
                client,
                channel,
                Some(&token),
                OPENAI_OAUTH_TOKEN_URL,
            )
            .await?;
            let request = openai_subscription_live_websocket_request(
                call_id,
                raw_query,
                &token,
                &credential,
                client_headers,
            )?;
            connect_responses_websocket_request(request).await
        }
        Err(error) => Err(error),
    }
}

fn responses_websocket_error_status(error: &anyhow::Error) -> Option<u16> {
    error.chain().find_map(|source| {
        source
            .downcast_ref::<tokio_tungstenite::tungstenite::Error>()
            .and_then(|error| match error {
                tokio_tungstenite::tungstenite::Error::Http(response) => {
                    Some(response.status().as_u16())
                }
                _ => None,
            })
    })
}

fn responses_websocket_terminal_status(text: &str) -> Option<u16> {
    responses_websocket_terminal_metadata(text).map(|metadata| metadata.status)
}

fn responses_websocket_supplier_route(
    config: &SupplierConfig,
) -> Result<(ResponsesWebsocketNativeRoute, ChannelConfig)> {
    let channel_id = if config.channel_id.trim().is_empty() {
        config.node_id.clone()
    } else {
        config.channel_id.clone()
    };
    let channel = channel_from_supplier(channel_id, config);
    let route = match crate::channel_executor::execution_kind_for_source(config.source_driver)? {
        crate::source_driver::ExecutionKind::OpenAiSubscription => {
            ResponsesWebsocketNativeRoute::OpenAiSubscription {
                channel: Box::new(channel.clone()),
            }
        }
        crate::source_driver::ExecutionKind::HttpSurface => {
            let target = crate::channel_surface::channel_surface_target(
                &channel,
                crate::surface::ApiSurface::OpenAi,
                Some(crate::protocol::kind::ProtocolKind::OpenAiResponses),
                true,
            )
            .ok_or_else(|| anyhow!("channel does not declare OpenAI Responses WebSocket"))?;
            ResponsesWebsocketNativeRoute::HttpSurface {
                channel: Box::new(channel.clone()),
                target,
            }
        }
        _ => return Err(anyhow!("supplier driver does not support Responses WebSocket")),
    };
    Ok((route, channel))
}

const SUPPLIER_DUPLEX_TURN_ID_FIELD: &str = "_const_duplex_turn_id";
const SUPPLIER_DUPLEX_TURN_SEQUENCE_FIELD: &str = "_const_duplex_turn_seq";

fn attach_supplier_duplex_turn(
    payload: &mut serde_json::Map<String, serde_json::Value>,
    turn_id: Option<&str>,
    sequence: &mut u64,
) {
    let Some(turn_id) = turn_id.map(str::trim).filter(|value| !value.is_empty()) else {
        return;
    };
    *sequence = sequence.saturating_add(1);
    payload.insert(
        SUPPLIER_DUPLEX_TURN_ID_FIELD.to_string(),
        serde_json::Value::String(turn_id.to_string()),
    );
    payload.insert(
        SUPPLIER_DUPLEX_TURN_SEQUENCE_FIELD.to_string(),
        serde_json::Value::from(*sequence),
    );
}

fn supplier_duplex_terminal_error_payload(
    message: &str,
    status: u16,
) -> serde_json::Map<String, serde_json::Value> {
    let data = serde_json::json!({
        "type": "error",
        "status": status,
        "error": {
            "type": "upstream_error",
            "code": "upstream_websocket_interrupted",
            "message": message,
        }
    })
    .to_string();
    serde_json::json!({
        "frame_type": "text",
        "data": data,
        "turn_terminal": true,
        "status": status,
        "failure_scope": "channel",
        "error_kind": "upstream_websocket_interrupted",
        "error_type": "transport_error",
        "error_code": "upstream_websocket_interrupted",
        "message": message,
    })
    .as_object()
    .cloned()
    .unwrap_or_default()
}

fn supplier_responses_send_error_payload(
    error: &anyhow::Error,
) -> serde_json::Map<String, serde_json::Value> {
    if let Some(payload) = responses_websocket_connect_error_payload(error) {
        return payload;
    }
    if responses_ws_pool_error_requires_full_replay(error) {
        // Codex recognizes this code and retries with the complete input. The
        // frame has not been handed to the upstream writer; a missing local
        // replay is not evidence that the subscription channel is unhealthy.
        let message = "Previous response is no longer available and its replay context is missing. Retry with complete input without previous_response_id.";
        let data = serde_json::json!({
            "type": "error", "status": 400,
            "error": {"type": "invalid_request_error", "code": "previous_response_not_found", "param": "previous_response_id", "message": message}
        }).to_string();
        return serde_json::json!({
            "frame_type": "text", "data": data, "turn_terminal": true, "status": 400,
            "failure_scope": "request", "error_kind": "request_error",
            "error_type": "invalid_request_error", "error_code": "previous_response_not_found",
            "failure_stage": "continuation_replay", "request_sent": false,
            "message": message
        }).as_object().cloned().unwrap_or_default();
    }
    let mut payload = supplier_duplex_terminal_error_payload(
        "Responses WebSocket upstream send failed",
        502,
    );
    attach_responses_transport_failure(&mut payload, "upstream_send", "write_error");
    payload
}

enum SupplierResponsesUpstreamSender {
    Dedicated {
        sender: futures_util::stream::SplitSink<
            ResponsesUpstreamSocket,
            tokio_tungstenite::tungstenite::Message,
        >,
        diagnostics: UpstreamWebsocketLease,
    },
    Pooled(ResponsesWsPoolSession),
}

enum SupplierResponsesUpstreamReceiver {
    Dedicated(futures_util::stream::SplitStream<ResponsesUpstreamSocket>),
    Pooled(Option<ResponsesWsPoolTurn>),
}

impl SupplierResponsesUpstreamSender {
    // Interrupt the current turn on its existing socket/lane. Never acquire a
    // new connection or replay a control frame after a transport failure.
    async fn interrupt(
        &mut self,
        receiver: &mut SupplierResponsesUpstreamReceiver,
        text: &str,
    ) -> Result<()> {
        match (self, receiver) {
            (Self::Dedicated { sender, .. }, SupplierResponsesUpstreamReceiver::Dedicated(_)) => {
                sender
                    .send(tokio_tungstenite::tungstenite::Message::Text(text.to_owned().into()))
                    .await
                    .map_err(Into::into)
            }
            (Self::Pooled(_), SupplierResponsesUpstreamReceiver::Pooled(Some(turn))) => {
                turn.interrupt(text).await
            }
            _ => Err(anyhow!("Responses WebSocket has no active turn to interrupt")),
        }
    }

    fn attach(&self, payload: &mut serde_json::Map<String, serde_json::Value>) {
        match self {
            Self::Dedicated { diagnostics, .. } => diagnostics.attach(payload),
            Self::Pooled(session) => session.attach(payload),
        }
    }

    fn log_terminal(&self, metadata: &ResponsesWebsocketTerminalMetadata) {
        match self {
            Self::Dedicated { diagnostics, .. } => diagnostics.log_terminal(metadata),
            Self::Pooled(session) => session.log_terminal(metadata),
        }
    }

    fn set_close_reason(&mut self, reason: &'static str) {
        if let Self::Dedicated { diagnostics, .. } = self {
            diagnostics.set_close_reason(reason);
        }
    }

    async fn send_text(
        &mut self,
        receiver: &mut SupplierResponsesUpstreamReceiver,
        text: &str,
    ) -> Result<()> {
        match (self, receiver) {
            (
                Self::Dedicated {
                    sender,
                    diagnostics,
                },
                SupplierResponsesUpstreamReceiver::Dedicated(_),
            ) => {
                diagnostics.mark_turn();
                sender
                    .send(tokio_tungstenite::tungstenite::Message::Text(
                        text.to_string().into(),
                    ))
                    .await
                    .map_err(Into::into)
            }
            (
                Self::Pooled(session),
                SupplierResponsesUpstreamReceiver::Pooled(turn),
            ) => {
                if turn.is_some() {
                    return Err(anyhow!("Responses WebSocket pooled turn already in progress"));
                }
                *turn = Some(session.start_turn(text).await?);
                Ok(())
            }
            _ => Err(anyhow!("Responses WebSocket upstream state is inconsistent")),
        }
    }

    /// Returns a replacement body only when a store-disabled subscription
    /// continuation was safely replayed on a new physical connection. The
    /// recovery branch is reachable solely for an affinity loss detected
    /// before the current frame was handed to the pool writer.
    async fn send_text_with_subscription_replay(
        &mut self,
        receiver: &mut SupplierResponsesUpstreamReceiver,
        subscription_config: Option<&SupplierConfig>,
        text: &str,
    ) -> Result<Option<String>> {
        match self.send_text(receiver, text).await {
            Ok(()) => Ok(None),
            Err(error)
                if subscription_config.is_some()
                    && responses_ws_pool_error_requires_full_replay(&error) =>
            {
                let config = subscription_config.expect("guarded subscription config");
                let Some(replay) = prepare_codex_websocket_subscription_replay(config, text)? else {
                    return Err(error.context(
                        "Responses WebSocket continuation replay context is unavailable",
                    ));
                };
                validate_response_create_frame(&replay)
                    .map_err(|(_, message)| anyhow!(message))?;
                self.send_text(receiver, &replay)
                    .await
                    .context("Responses WebSocket continuation replay send failed")?;
                if let Self::Pooled(session) = self {
                    session.mark_continuation_replay();
                }
                log::info!(
                    "[const-api][upstream-ws] continuation_replayed channel_id={} retry=1 safety=pre_send_only",
                    config.channel_id,
                );
                Ok(Some(replay))
            }
            Err(error) => Err(error),
        }
    }

    // A reference rejected before any response frame is not an ambiguous network
    // failure. Reuse the bounded, validated history and leave normal turns alone.
    // The returned body has no previous_response_id, so it cannot be retried here again.
    async fn replay_rejected_subscription_continuation(
        &mut self,
        receiver: &mut SupplierResponsesUpstreamReceiver,
        config: &SupplierConfig,
        request_body: &str,
        metadata: Option<&ResponsesWebsocketTerminalMetadata>,
        response_started: bool,
    ) -> Result<Option<String>> {
        if response_started
            || !metadata.is_some_and(|event| {
                event.event_type == "error"
                    && event.status == 400
                    && event.error_code.as_deref() == Some("previous_response_not_found")
            })
        {
            return Ok(None);
        }
        let Some(replay) = prepare_codex_websocket_subscription_replay(config, request_body)? else {
            return Ok(None);
        };
        validate_response_create_frame(&replay).map_err(|(_, message)| anyhow!(message))?;
        self.finish_turn(receiver);
        self.send_text(receiver, &replay)
            .await
            .context("Responses WebSocket rejected continuation replay send failed")?;
        if let Self::Pooled(session) = self {
            session.mark_continuation_replay();
        }
        log::info!(
            "[const-api][upstream-ws] continuation_replayed channel_id={} retry=1 safety=explicit_reference_rejection",
            config.channel_id,
        );
        Ok(Some(replay))
    }

    fn finish_turn(&mut self, receiver: &mut SupplierResponsesUpstreamReceiver) {
        match (self, receiver) {
            (
                Self::Dedicated { diagnostics, .. },
                SupplierResponsesUpstreamReceiver::Dedicated(_),
            ) => diagnostics.finish_turn(),
            (Self::Pooled(_), SupplierResponsesUpstreamReceiver::Pooled(turn)) => {
                if let Some(turn) = turn.take() {
                    turn.finish();
                }
            }
            _ => {}
        }
    }

    async fn send_pong(
        &mut self,
        bytes: tokio_tungstenite::tungstenite::Bytes,
    ) -> Result<()> {
        match self {
            Self::Dedicated { sender, .. } => sender
                .send(tokio_tungstenite::tungstenite::Message::Pong(bytes))
                .await
                .map_err(Into::into),
            // The pooled physical connection actor consumes control frames.
            Self::Pooled(_) => Ok(()),
        }
    }

    async fn close(
        &mut self,
        receiver: &mut SupplierResponsesUpstreamReceiver,
        reason: &'static str,
    ) {
        match (self, receiver) {
            (
                Self::Dedicated { sender, .. },
                SupplierResponsesUpstreamReceiver::Dedicated(_),
            ) => close_upstream_websocket_sender(sender).await,
            (Self::Pooled(_), SupplierResponsesUpstreamReceiver::Pooled(turn)) => {
                if let Some(turn) = turn.take() {
                    turn.cancel(reason);
                }
            }
            _ => {}
        }
    }
}

impl SupplierResponsesUpstreamReceiver {
    async fn next(&mut self) -> Option<Result<tokio_tungstenite::tungstenite::Message>> {
        match self {
            Self::Dedicated(receiver) => receiver.next().await.map(|result| result.map_err(Into::into)),
            Self::Pooled(Some(turn)) => turn.next().await,
            Self::Pooled(None) => std::future::pending().await,
        }
    }
}

async fn open_supplier_responses_upstream(
    client: &Client,
    route: &ResponsesWebsocketNativeRoute,
    channel: &ChannelConfig,
    client_headers: &warp::http::HeaderMap,
    request_source: &str,
    pool: ResponsesWsPool,
) -> Result<(SupplierResponsesUpstreamSender, SupplierResponsesUpstreamReceiver)> {
    let pool_supported = matches!(route, ResponsesWebsocketNativeRoute::OpenAiSubscription { .. })
        || responses_websocket_route_is_official_openai_api(route);
    if pool_supported && channel.subscription.responses_ws_pool_enabled
    {
        let session = pool
            .prepare_session(
                client,
                route,
                channel,
                client_headers,
                request_source,
            )
            .await?;
        return Ok((
            SupplierResponsesUpstreamSender::Pooled(session),
            SupplierResponsesUpstreamReceiver::Pooled(None),
        ));
    }

    let socket = connect_responses_websocket_native_route(client, route, client_headers).await?;
    let diagnostics = UpstreamWebsocketLease::open(
        "openai_responses",
        channel.id.clone(),
        request_source.to_string(),
    );
    let (sender, receiver) = socket.split();
    Ok((
        SupplierResponsesUpstreamSender::Dedicated {
            sender,
            diagnostics,
        },
        SupplierResponsesUpstreamReceiver::Dedicated(receiver),
    ))
}

pub(crate) async fn run_supplier_responses_duplex(
    client: &Client,
    config: &SupplierConfig,
    open: SupplierMessage,
    mut commands: tokio::sync::mpsc::Receiver<SupplierMessage>,
    outbound: &SupplierOutbound,
    responses_ws_pool: ResponsesWsPool,
) -> Result<()> {
    let session_kind = open
        .payload
        .get("session_kind")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if session_kind == "native_realtime" {
        return run_supplier_native_duplex(client, config, open, commands, outbound).await;
    }
    if session_kind == "native_realtime_call" {
        return run_supplier_voice_call(client, config, open, commands, outbound).await;
    }
    if session_kind != "openai_responses" {
        return Err(anyhow!("unsupported supplier duplex session kind"));
    }
    let wire = crate::surface_wire::SurfaceEnvelope::from_payload(&open.payload)?;
    if wire.surface != Some(crate::surface::ApiSurface::OpenAi)
        || wire.operation != Some(crate::surface::ApiOperation::ResponsesWebSocket)
    {
        return Err(anyhow!("invalid Responses WebSocket surface envelope"));
    }
    let client_headers = wire.header_map()?;
    let (route, channel) = responses_websocket_supplier_route(config)?;
    let safety_identifier = open
        .payload
        .get("_const_safety_identifier")
        .and_then(serde_json::Value::as_str)
        .filter(|_| responses_websocket_route_is_official_openai_api(&route))
        .map(str::to_string);
    let request_source = websocket_request_source(&client_headers);
    let (mut upstream_sender, mut upstream_receiver) = match open_supplier_responses_upstream(
        client,
        &route,
        &channel,
        &client_headers,
        &request_source,
        responses_ws_pool,
    )
    .await
    {
        Ok(upstream) => upstream,
        Err(error) => {
            let payload = supplier_responses_websocket_connect_error_payload(
                &error,
                &channel.id,
                &request_source,
            );
            log::warn!(
                "[const-api][upstream-ws] handshake_failed operation=openai_responses channel_id={} request_source={} status={} error_kind={} provider_request_id={} upstream_open_connections={} upstream_active_turns={}",
                channel.id,
                request_source,
                payload
                    .get("status")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(502),
                payload
                    .get("error_kind")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("upstream_websocket_connect_failed"),
                payload
                    .get("provider_request_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("-"),
                payload
                    .get("upstream_open_connections")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or_default(),
                payload
                    .get("upstream_active_turns")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or_default(),
            );
            send_supplier_message(
                outbound,
                SupplierMessage {
                    id: open.id.clone(),
                    kind: "duplex_error".to_string(),
                    payload,
                },
            )
            .await?;
            return Ok(());
        }
    };
    let mut opened_payload = serde_json::json!({
        "session_kind": session_kind,
        "transport": "websocket"
    })
    .as_object()
    .cloned()
    .unwrap_or_default();
    upstream_sender.attach(&mut opened_payload);
    send_supplier_message(
        outbound,
        SupplierMessage {
            id: open.id.clone(),
            kind: "duplex_opened".to_string(),
            payload: opened_payload,
        },
    )
    .await?;

    let subscription_config = route.subscription_config();
    let mut active_request: Option<NativeSubscriptionWebsocketRequest> = None;
    let mut active_continuation: Option<CodexHttpContinuationCollector> = None;
    let continuation_pin = CodexWebsocketContinuationPin::new();
    let mut active_update_guard: Option<crate::update_activity::UpdateActivityGuard> = None;
    let mut active_turn_id: Option<String> = None;
    let mut active_turn_sequence = 0u64;
    let mut active_response_started = false;
    let mut turn_active = false;

    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else {
                    upstream_sender
                        .close(&mut upstream_receiver, "downstream_closed")
                        .await;
                    upstream_sender.set_close_reason("downstream_closed");
                    return Ok(());
                };
                match command.kind.as_str() {
                    "duplex_client_frame" => {
                        if command.payload.get("frame_type").and_then(serde_json::Value::as_str) != Some("text") {
                            return Err(anyhow!("Responses WebSocket currently accepts text frames only"));
                        }
                        let text = command
                            .payload
                            .get("data")
                            .and_then(serde_json::Value::as_str)
                            .ok_or_else(|| anyhow!("supplier duplex text frame is missing data"))?;
                        if turn_active && is_responses_interrupt_frame(text) {
                            upstream_sender.interrupt(&mut upstream_receiver, text).await?;
                            continue;
                        }
                        if turn_active {
                            return Err(anyhow!("supplier duplex turn already in progress"));
                        }
                        let outbound_text = safety_identifier.as_deref().map_or_else(
                            || text.to_string(),
                            |identifier| String::from_utf8_lossy(
                                &crate::channel_executor::openai_body_with_safety_identifier(
                                    text.as_bytes(),
                                    identifier,
                                ),
                            ).into_owned(),
                        );
                        let outbound_text = crate::openrouter::request_body(Some(route.channel()), &outbound_text).into_owned();
                        validate_response_create_frame(&outbound_text)
                            .map_err(|(_, message)| anyhow!(message))?;
                        let request_started_at = std::time::Instant::now();
                        active_turn_id = command
                            .payload
                            .get(SUPPLIER_DUPLEX_TURN_ID_FIELD)
                            .and_then(serde_json::Value::as_str)
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .map(str::to_string);
                        active_turn_sequence = 0;
                        active_response_started = false;
                        let mut pending = match subscription_config.as_ref() {
                            Some(subscription) => Some(
                                NativeSubscriptionWebsocketRequest::start_at(
                                    subscription,
                                    &outbound_text,
                                    request_started_at,
                                )
                                .map_err(|payload| anyhow!(serde_json::Value::Object(payload).to_string()))?,
                            ),
                            None => None,
                        };
                        let Some(next_update_guard) =
                            crate::update_activity::try_begin_supplier_request(&open.id)
                        else {
                            return Err(anyhow!("client update is starting; retry in 2 seconds"));
                        };
                        // Pin/capture the parent before attempting a reconnect,
                        // including when this logical session was just opened.
                        let mut pending_continuation = subscription_config.as_ref().map(|subscription| {
                            CodexHttpContinuationCollector::new_for_pinned_websocket(
                                subscription,
                                &outbound_text,
                                &continuation_pin,
                            )
                        });
                        let replay_body = match upstream_sender
                            .send_text_with_subscription_replay(
                                &mut upstream_receiver,
                                subscription_config.as_ref(),
                                &outbound_text,
                            )
                            .await
                        {
                            Ok(replay_body) => replay_body,
                            Err(error) => {
                                log::warn!(
                                    "[const-api][upstream-ws] request_send_failed channel_id={} error={error:#}",
                                    channel.id,
                                );
                                let mut payload = supplier_responses_send_error_payload(&error);
                                let status = payload.get("status")
                                    .and_then(serde_json::Value::as_u64).unwrap_or(502) as u16;
                                let scope = payload.get("failure_scope")
                                    .and_then(serde_json::Value::as_str).unwrap_or("channel");
                                if let Some(mut request) = pending {
                                    if let Some(data) = payload.get("data").and_then(serde_json::Value::as_str) {
                                        request.observe(data);
                                    }
                                    request.finalize(status, Some(scope), None, &channel);
                                }
                                upstream_sender.attach(&mut payload);
                                attach_supplier_duplex_turn(
                                    &mut payload,
                                    active_turn_id.as_deref(),
                                    &mut active_turn_sequence,
                                );
                                send_supplier_message(outbound, SupplierMessage {
                                    id: open.id.clone(),
                                    kind: "duplex_server_frame".to_string(),
                                    payload,
                                }).await?;
                                send_supplier_message(outbound, SupplierMessage {
                                    id: open.id.clone(),
                                    kind: "duplex_close".to_string(),
                                    payload: serde_json::Map::new(),
                                }).await?;
                                drop(active_update_guard.take());
                                upstream_sender.set_close_reason("upstream_send_failed");
                                return Ok(());
                            }
                        };
                        if let Some(replay) = replay_body.as_deref() {
                            pending_continuation = subscription_config.as_ref().map(|subscription| {
                                CodexHttpContinuationCollector::new_for_pinned_websocket(
                                    subscription,
                                    replay,
                                    &continuation_pin,
                                )
                            });
                        }
                        let actual_outbound_text = replay_body.unwrap_or(outbound_text);
                        if let Some(request) = pending.as_mut() {
                            request.request_body = actual_outbound_text.clone();
                        }
                        active_update_guard = Some(next_update_guard);
                        if let Some(request) = pending.as_mut() {
                            request.mark_sent();
                        }
                        active_continuation = pending_continuation;
                        active_request = pending;
                        turn_active = true;
                    }
                    "duplex_close" => {
                        if let Some(request) = active_request.take() {
                            request.finalize(499, Some("request"), None, &channel);
                        }
                        if active_turn_id.is_some() {
                            let mut payload = supplier_duplex_terminal_error_payload(
                                "downstream request canceled",
                                499,
                            );
                            upstream_sender.attach(&mut payload);
                            payload.insert(
                                "failure_scope".to_string(),
                                serde_json::Value::String("request".to_string()),
                            );
                            attach_supplier_duplex_turn(
                                &mut payload,
                                active_turn_id.as_deref(),
                                &mut active_turn_sequence,
                            );
                            send_supplier_message(outbound, SupplierMessage {
                                id: open.id.clone(),
                                kind: "duplex_server_frame".to_string(),
                                payload,
                            }).await?;
                        }
                        upstream_sender
                            .close(&mut upstream_receiver, "downstream_close")
                            .await;
                        send_supplier_message(outbound, SupplierMessage {
                            id: open.id.clone(),
                            kind: "duplex_close".to_string(),
                            payload: serde_json::Map::new(),
                        }).await?;
                        upstream_sender.set_close_reason("downstream_close");
                        return Ok(());
                    }
                    _ => return Err(anyhow!("unsupported supplier duplex command")),
                }
            }
            upstream = upstream_receiver.next() => {
                let Some(upstream) = upstream else {
                    if let Some(request) = active_request.take() {
                        request.finalize(502, Some("channel"), None, &channel);
                    }
                    if active_turn_id.is_some() {
                        let mut payload = supplier_duplex_terminal_error_payload(
                            "Responses WebSocket upstream closed unexpectedly",
                            502,
                        );
                        upstream_sender.set_close_reason("upstream_eof");
                        upstream_sender.attach(&mut payload);
                        payload.entry("upstream_close_reason".to_string()).or_insert("upstream_eof".into());
                        attach_responses_transport_failure(&mut payload, "upstream_read", "read_error");
                        attach_supplier_duplex_turn(
                            &mut payload,
                            active_turn_id.as_deref(),
                            &mut active_turn_sequence,
                        );
                        send_supplier_message(outbound, SupplierMessage {
                            id: open.id.clone(),
                            kind: "duplex_server_frame".to_string(),
                            payload,
                        }).await?;
                        send_supplier_message(outbound, SupplierMessage {
                            id: open.id.clone(),
                            kind: "duplex_close".to_string(),
                            payload: serde_json::Map::new(),
                        }).await?;
                        drop(active_update_guard.take());
                        upstream_sender.set_close_reason("upstream_eof");
                        return Ok(());
                    }
                    upstream_sender.set_close_reason("upstream_eof");
                    return Err(anyhow!("Responses WebSocket upstream closed unexpectedly"));
                };
                let upstream = match upstream {
                    Ok(upstream) => upstream,
                    Err(error) if active_turn_id.is_some() => {
                        if let Some(request) = active_request.take() {
                            request.finalize(502, Some("channel"), None, &channel);
                        }
                        let message = format!("{error:#}");
                        let mut payload = supplier_duplex_terminal_error_payload(&message, 502);
                        upstream_sender.set_close_reason("upstream_read_error");
                        upstream_sender.attach(&mut payload);
                        payload.entry("upstream_close_reason".to_string()).or_insert("upstream_read_error".into());
                        let cause = if payload.get("upstream_close_reason").and_then(serde_json::Value::as_str) == Some("pool_event_queue_full") {
                            "backpressure"
                        } else { "read_error" };
                        attach_responses_transport_failure(&mut payload, "upstream_read", cause);
                        attach_supplier_duplex_turn(
                            &mut payload,
                            active_turn_id.as_deref(),
                            &mut active_turn_sequence,
                        );
                        send_supplier_message(outbound, SupplierMessage {
                            id: open.id.clone(),
                            kind: "duplex_server_frame".to_string(),
                            payload,
                        }).await?;
                        send_supplier_message(outbound, SupplierMessage {
                            id: open.id.clone(),
                            kind: "duplex_close".to_string(),
                            payload: serde_json::Map::new(),
                        }).await?;
                        drop(active_update_guard.take());
                        upstream_sender.set_close_reason("upstream_read_error");
                        return Ok(());
                    }
                    Err(error) => {
                        upstream_sender.set_close_reason("upstream_read_error");
                        return Err(error.into());
                    }
                };
                match upstream {
                    tokio_tungstenite::tungstenite::Message::Text(text) => {
                        let mut text = text.to_string();
                        let mut terminal_metadata = responses_websocket_terminal_metadata(&text);
                        let mut replay_send_failure = None;
                        if let (Some(subscription), Some(request)) =
                            (subscription_config.as_ref(), active_request.as_ref())
                        {
                            match upstream_sender.replay_rejected_subscription_continuation(
                                &mut upstream_receiver, subscription, &request.request_body,
                                terminal_metadata.as_ref(), active_response_started,
                            ).await {
                                Ok(Some(replay)) => {
                                    active_continuation = Some(CodexHttpContinuationCollector::new_for_pinned_websocket(
                                        subscription, &replay, &continuation_pin,
                                    ));
                                    if let Some(request) = active_request.as_mut() {
                                        request.request_body = replay;
                                    }
                                    continue;
                                }
                                Ok(None) => {}
                                Err(error) => {
                                    // Do not expose the earlier, recoverable rejection when
                                    // the actual final failure was sending its replacement.
                                    let payload = supplier_responses_send_error_payload(&error);
                                    text = payload.get("data").and_then(serde_json::Value::as_str)
                                        .unwrap_or_default().to_string();
                                    terminal_metadata = responses_websocket_terminal_metadata(&text);
                                    replay_send_failure = Some(payload);
                                }
                            }
                        }
                        if subscription_config.is_some() {
                            crate::detection::observe_openai_subscription_response_event(
                                &channel,
                                &text,
                            );
                        }
                        if let (Some(subscription), Some(continuation)) =
                            (subscription_config.as_ref(), active_continuation.as_mut())
                        {
                            let _ = continuation.push_websocket_text(subscription, &text);
                        }
                        let terminal_status = active_request
                            .as_mut()
                            .and_then(|request| request.observe(&text))
                            .or_else(|| responses_websocket_terminal_status(&text));
                        let usage = terminal_status
                            .and_then(|_| active_request.as_ref())
                            .and_then(NativeSubscriptionWebsocketRequest::upstream_usage_snapshot);
                        let mut payload = serde_json::json!({
                            "frame_type": "text",
                            "data": text,
                            "turn_terminal": terminal_status.is_some(),
                            "status": terminal_status.unwrap_or_default()
                        })
                        .as_object()
                        .cloned()
                        .unwrap_or_default();
                        let close_after_terminal = replay_send_failure.is_some();
                        if let Some(failure) = replay_send_failure {
                            payload.extend(failure);
                        }
                        if let Some(metadata) = terminal_metadata.as_ref() {
                            attach_responses_websocket_terminal_metadata(&mut payload, metadata);
                            upstream_sender.log_terminal(metadata);
                        }
                        if terminal_status.is_some() {
                            attach_responses_websocket_failure_observation(
                                &mut payload, &text, active_request.as_ref().map(|request| request.model.as_str()),
                            );
                            upstream_sender.attach(&mut payload);
                        }
                        attach_upstream_usage(&mut payload, usage.as_ref());
                        attach_supplier_duplex_turn(
                            &mut payload,
                            active_turn_id.as_deref(),
                            &mut active_turn_sequence,
                        );
                        send_supplier_message(outbound, SupplierMessage {
                            id: open.id.clone(),
                            kind: "duplex_server_frame".to_string(),
                            payload,
                        }).await?;
                        active_response_started = true;
                        if let Some(status) = terminal_status {
                            active_continuation = None;
                            if let Some(request) = active_request.take() {
                                request.finalize(
                                    status,
                                    (status == 499).then_some("request"),
                                    None,
                                    &channel,
                                );
                            }
                            upstream_sender.finish_turn(&mut upstream_receiver);
                            turn_active = false;
                            active_turn_id = None;
                            drop(active_update_guard.take());
                            if close_after_terminal {
                                upstream_sender.close(&mut upstream_receiver, "continuation_replay_send_failed").await;
                                send_supplier_message(outbound, SupplierMessage {
                                    id: open.id.clone(), kind: "duplex_close".to_string(),
                                    payload: serde_json::Map::new(),
                                }).await?;
                                return Ok(());
                            }
                        }
                    }
                    tokio_tungstenite::tungstenite::Message::Binary(bytes) => {
                        active_response_started = true;
                        let mut payload = serde_json::json!({
                            "frame_type": "binary",
                            "encoding": "base64",
                            "data": base64::engine::general_purpose::STANDARD.encode(bytes),
                            "turn_terminal": false
                        })
                        .as_object()
                        .cloned()
                        .unwrap_or_default();
                        attach_supplier_duplex_turn(
                            &mut payload,
                            active_turn_id.as_deref(),
                            &mut active_turn_sequence,
                        );
                        send_supplier_message(outbound, SupplierMessage {
                            id: open.id.clone(),
                            kind: "duplex_server_frame".to_string(),
                            payload,
                        }).await?;
                    }
                    tokio_tungstenite::tungstenite::Message::Ping(bytes) => {
                        upstream_sender.send_pong(bytes).await?;
                    }
                    tokio_tungstenite::tungstenite::Message::Pong(_) => {}
                    tokio_tungstenite::tungstenite::Message::Close(frame) => {
                        if let Some(request) = active_request.take() {
                            request.finalize(502, Some("channel"), None, &channel);
                        }
                        let (code, reason) = frame
                            .map(|frame| (u16::from(frame.code), frame.reason.to_string()))
                            .unwrap_or((1000, String::new()));
                        if active_turn_id.is_some() {
                            let message = if reason.trim().is_empty() {
                                "Responses WebSocket upstream closed unexpectedly".to_string()
                            } else {
                                reason.clone()
                            };
                            let mut payload = supplier_duplex_terminal_error_payload(&message, 502);
                            payload.insert("upstream_close_code".to_string(), code.into());
                            upstream_sender.attach(&mut payload);
                            payload.insert("upstream_close_reason".into(), "upstream_close_frame".into());
                            attach_responses_transport_failure(&mut payload, "upstream_read", "peer_close");
                            attach_supplier_duplex_turn(
                                &mut payload,
                                active_turn_id.as_deref(),
                                &mut active_turn_sequence,
                            );
                            send_supplier_message(outbound, SupplierMessage {
                                id: open.id.clone(),
                                kind: "duplex_server_frame".to_string(),
                                payload,
                            }).await?;
                            drop(active_update_guard.take());
                        }
                        send_supplier_message(outbound, SupplierMessage {
                            id: open.id.clone(),
                            kind: "duplex_close".to_string(),
                            payload: serde_json::json!({"code": code, "reason": reason})
                                .as_object().cloned().unwrap_or_default(),
                        }).await?;
                        // Receiving a Close queues the peer reply inside
                        // tungstenite. Flush it before dropping the split sink
                        // so the upstream also observes a complete handshake.
                        upstream_sender
                            .close(&mut upstream_receiver, "upstream_close_frame")
                            .await;
                        upstream_sender.set_close_reason("upstream_close_frame");
                        return Ok(());
                    }
                    tokio_tungstenite::tungstenite::Message::Frame(_) => {}
                }
            }
        }
    }
}

fn responses_websocket_route_is_official_openai_api(
    route: &ResponsesWebsocketNativeRoute,
) -> bool {
    let ResponsesWebsocketNativeRoute::HttpSurface { channel, target } = route else {
        return false;
    };
    channel.source_driver() == crate::source_driver::SourceDriverId::OpenAiApi
        && reqwest::Url::parse(&target.base_url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_string))
            .is_some_and(|host| host.eq_ignore_ascii_case("api.openai.com"))
}

pub(crate) async fn proxy_websocket_request(
    path: warp::path::FullPath,
    raw_query: Option<String>,
    headers: warp::http::HeaderMap,
    websocket: warp::ws::Ws,
    shared: Arc<ProxyShared>,
) -> Result<warp::reply::Response, Infallible> {
    let path = path.as_str();
    let raw_query = raw_query.unwrap_or_default();
    let Some(route) = crate::surface::resolve_api_route("GET", path, true) else {
        return Ok(crate::surface::surface_from_path(path)
            .and_then(|surface| {
                local_surface_error_response(
                    surface,
                    StatusCode::NOT_FOUND,
                    "websocket surface route not found",
                )
                .ok()
            })
            .unwrap_or_else(|| error_response(StatusCode::NOT_FOUND, "not found")));
    };
    if !matches!(
        route.operation,
        crate::surface::ApiOperation::ResponsesWebSocket
            | crate::surface::ApiOperation::RealtimeWebSocket
            | crate::surface::ApiOperation::RealtimeLiveConnect
            | crate::surface::ApiOperation::RealtimeLiveWebSocket
            | crate::surface::ApiOperation::RealtimeTranslationWebSocket
            | crate::surface::ApiOperation::LiveWebSocket
    ) {
        return Ok(local_surface_error_response(
            route.surface,
            StatusCode::NOT_IMPLEMENTED,
            "websocket operation is not implemented",
        )
        .unwrap_or_else(|_| {
            error_response(
                StatusCode::NOT_IMPLEMENTED,
                "websocket operation is not implemented",
            )
        }));
    }
    let config = shared.config_snapshot();
    if !local_proxy_request_authorized(path, &raw_query, &headers, &config) {
        return Ok(local_surface_error_response(
            route.surface,
            StatusCode::UNAUTHORIZED,
            "unauthorized",
        )
        .unwrap_or_else(|_| error_response(StatusCode::UNAUTHORIZED, "unauthorized")));
    }
    let Some(activity_guard) = crate::update_activity::try_begin_proxy_request() else {
        let mut response = local_surface_error_response(
            route.surface,
            StatusCode::SERVICE_UNAVAILABLE,
            "client update is starting; retry in 2 seconds",
        )
        .unwrap_or_else(|_| {
            error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "client update is starting; retry in 2 seconds",
            )
        });
        response
            .headers_mut()
            .insert("retry-after", warp::http::HeaderValue::from_static("2"));
        return Ok(response);
    };
    let raw_query = strip_local_auth_query(path, &raw_query, &headers, &config);
    let operation = route.operation;
    let websocket_path = path.to_string();
    let websocket = websocket
        .max_message_size(DUPLEX_WEBSOCKET_MAX_MESSAGE_BYTES)
        .max_frame_size(DUPLEX_WEBSOCKET_MAX_MESSAGE_BYTES);
    Ok(websocket
        .on_upgrade(move |socket| async move {
            match operation {
                crate::surface::ApiOperation::ResponsesWebSocket => {
                    run_responses_websocket_proxy(socket, headers, shared, activity_guard).await;
                }
                crate::surface::ApiOperation::RealtimeWebSocket => {
                    run_openai_realtime_websocket_proxy(
                        socket,
                        raw_query,
                        headers,
                        shared,
                        activity_guard,
                    )
                    .await;
                }
                crate::surface::ApiOperation::RealtimeLiveConnect
                | crate::surface::ApiOperation::RealtimeLiveWebSocket => {
                    run_openai_live_websocket_proxy(
                        socket,
                        websocket_path,
                        raw_query,
                        headers,
                        shared,
                        activity_guard,
                    )
                    .await;
                }
                crate::surface::ApiOperation::RealtimeTranslationWebSocket => {
                    run_openai_realtime_translation_websocket_proxy(
                        socket,
                        raw_query,
                        headers,
                        shared,
                        activity_guard,
                    )
                    .await;
                }
                crate::surface::ApiOperation::LiveWebSocket => {
                    run_gemini_live_websocket_proxy(
                        socket,
                        raw_query,
                        headers,
                        shared,
                        activity_guard,
                    )
                    .await;
                }
                _ => {}
            }
        })
        .into_response())
}

async fn run_responses_websocket_proxy(
    client_socket: warp::ws::WebSocket,
    client_headers: warp::http::HeaderMap,
    shared: Arc<ProxyShared>,
    first_activity_guard: crate::update_activity::UpdateActivityGuard,
) {
    let (mut client_sender, mut client_receiver) = client_socket.split();
    let first = match tokio::select! {
        first = tokio::time::timeout(
            RESPONSES_WEBSOCKET_FIRST_FRAME_TIMEOUT,
            client_receiver.next(),
        ) => first,
        _ = crate::update_activity::wait_for_update_drain() => {
            let _ = client_sender
                .send(warp::ws::Message::close_with(1012u16, "client update"))
                .await;
            return;
        }
    } {
        Ok(Some(Ok(message))) => message,
        Ok(Some(Err(_))) | Ok(None) => return,
        Err(_) => {
            let _ = send_responses_websocket_error(
                &mut client_sender,
                "first_frame_timeout",
                "the first response.create frame was not received within 30 seconds",
            )
            .await;
            return;
        }
    };
    let first_text = match client_response_create_text(&first) {
        Ok(text) => text.to_string(),
        Err((code, message)) => {
            let _ = send_responses_websocket_error(&mut client_sender, code, message).await;
            return;
        }
    };
    let first_value = match validate_response_create_frame(&first_text) {
        Ok(value) => value,
        Err((code, message)) => {
            let _ = send_responses_websocket_error(&mut client_sender, code, message).await;
            return;
        }
    };
    let first_request_started_at = std::time::Instant::now();
    let mut active_activity_guard = Some(first_activity_guard);
    let (native_route, platform_route) =
        match select_responses_websocket_route(&shared, &client_headers, first_text.as_bytes()) {
            ResponsesWebsocketRoute::Native(route) => (Some(route), false),
            ResponsesWebsocketRoute::Platform => (None, true),
            ResponsesWebsocketRoute::HttpBridge(reason) => {
                log::debug!(
                    "[const-api][websocket] using HTTP bridge request_source={} reason={}",
                    websocket_request_source(&client_headers),
                    reason,
                );
                run_responses_websocket_http_bridge(
                    client_sender,
                    client_receiver,
                    client_headers,
                    shared,
                    first_value,
                    active_activity_guard.take(),
                )
                .await;
                return;
            }
        };
    let connect_started_at = std::time::Instant::now();
    let (upstream_connection, channel, subscription_config, route_label, first_frame_sent) =
        if let Some(native_route) = native_route {
            let channel = native_route.channel().clone();
            let native_transport = match &native_route {
                ResponsesWebsocketNativeRoute::HttpSurface { .. } => "http_surface_websocket",
                ResponsesWebsocketNativeRoute::OpenAiSubscription { .. } => {
                    "openai_subscription_websocket"
                }
            };
            let connection = match connect_responses_websocket_native_route(
                &shared.client,
                &native_route,
                &client_headers,
            )
            .await
            {
                Ok(connection) => connection,
                Err(error) => {
                    // No inference frame has been sent yet, so falling back to
                    // the existing HTTP router is replay-safe.
                    let metadata = responses_websocket_connect_error_metadata(&error);
                    let error_kind = responses_websocket_connect_error_kind(&metadata);
                    let mut counts = serde_json::Map::new();
                    attach_upstream_websocket_count_snapshot(
                        &mut counts,
                        &format!("openai_responses:{}", channel.id),
                    );
                    log::warn!(
                        "[const-api][websocket] native connect failed; using HTTP bridge channel_id={} request_source={} status={} error_kind={} provider_request_id={} upstream_open_connections={} upstream_active_turns={}",
                        channel.id,
                        websocket_request_source(&client_headers),
                        metadata.status,
                        error_kind,
                        metadata.provider_request_id.as_deref().unwrap_or("-"),
                        counts
                            .get("upstream_open_connections")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or_default(),
                        counts
                            .get("upstream_active_turns")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or_default(),
                    );
                    run_responses_websocket_http_bridge(
                        client_sender,
                        client_receiver,
                        client_headers,
                        shared,
                        first_value,
                        active_activity_guard.take(),
                    )
                    .await;
                    return;
                }
            };
            let subscription_config = native_route.subscription_config();
            (
                ResponsesRelayConnection::WebSocket(connection),
                Some(channel.clone()),
                subscription_config,
                format!("channel_id={} transport={native_transport}", channel.id),
                false,
            )
        } else {
            let (connection, endpoint, transport) = match connect_platform_responses_websocket(
                &shared,
                &client_headers,
            )
            .await
            {
                    Ok(connection) => connection,
                    Err(error)
                        if crate::supplier::platform_duplex_error_is_safe_to_replay(
                            &error,
                        ) =>
                    {
                        log::warn!(
                            "[const-api][websocket] platform duplex was unavailable before inference; using HTTP bridge error={error}"
                        );
                        run_responses_websocket_http_bridge(
                            client_sender,
                            client_receiver,
                            client_headers,
                            shared,
                            first_value,
                            active_activity_guard.take(),
                        )
                        .await;
                        return;
                    }
                    Err(error) => {
                        log::warn!(
                            "[const-api][websocket] platform duplex outcome is ambiguous; refusing replay error={error}"
                        );
                        let _ = send_responses_websocket_status_error(
                            &mut client_sender,
                            502,
                            "platform_duplex_interrupted",
                            "the platform duplex connection was interrupted after the request may have started",
                        )
                        .await;
                        return;
                    }
                };
            (
                ResponsesRelayConnection::Platform(connection),
                None,
                None,
                format!("endpoint={} transport={} logical=platform_duplex", endpoint.name, transport),
                false,
            )
        };
    log::debug!(
        "[const-api][websocket] native connected {} duration_ms={}",
        route_label,
        connect_started_at.elapsed().as_millis(),
    );
    let mut upstream_diagnostics = channel.as_ref().map(|channel| {
        UpstreamWebsocketLease::open(
            "openai_responses",
            channel.id.clone(),
            websocket_request_source(&client_headers),
        )
    });
    if let Some(diagnostics) = upstream_diagnostics.as_mut() {
        diagnostics.mark_turn();
    }
    let mut active_subscription_request = match subscription_config.as_ref() {
        Some(config) => match NativeSubscriptionWebsocketRequest::start_at(
            config,
            &first_text,
            first_request_started_at,
        ) {
            Ok(mut request) => {
                request.local_model_observation = channel.as_ref().map(|channel| {
                    LocalModelOutcomeGuard::new(shared.clone(), &channel.id, &request.model)
                });
                Some(request)
            }
            Err(_) => {
                run_responses_websocket_http_bridge(
                    client_sender,
                    client_receiver,
                    client_headers,
                    shared,
                    first_value,
                    active_activity_guard.take(),
                )
                .await;
                return;
            }
        },
        None => None,
    };
    let (mut upstream_sender, mut upstream_receiver) = upstream_connection.split();
    if !first_frame_sent
        && upstream_sender
        .send(tokio_tungstenite::tungstenite::Message::Text(
            responses_websocket_wire_body(channel.as_ref(), &first_text).into_owned().into(),
        ))
        .await
        .is_err()
    {
        if let (Some(request), Some(channel)) =
            (active_subscription_request.take(), channel.as_ref())
        {
            request.finalize(502, Some("channel"), Some(&shared), channel);
        }
        let _ = send_responses_websocket_error(
            &mut client_sender,
            "upstream_connection_failed",
            "the first response.create frame could not be sent upstream",
        )
        .await;
        if let Some(diagnostics) = upstream_diagnostics.as_mut() {
            diagnostics.set_close_reason("first_frame_send_failed");
        }
        upstream_sender.close_bounded().await;
        return;
    }
    if let Some(request) = active_subscription_request.as_mut() {
        request.mark_sent();
    }

    let mut active_model = first_value
        .get("model")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    let mut turn_active = true;
    let mut platform_first_response_pending = platform_route;
    let mut bridge_handoff = None;
    loop {
        tokio::select! {
            _ = crate::update_activity::wait_for_update_drain(), if !turn_active => {
                let _ = client_sender
                    .send(warp::ws::Message::close_with(1012u16, "client update"))
                    .await;
                let _ = upstream_sender
                    .send(tokio_tungstenite::tungstenite::Message::Close(Some(
                        tokio_tungstenite::tungstenite::protocol::CloseFrame {
                            code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Restart,
                            reason: "client update".into(),
                        },
                    )))
                    .await;
                if let Some(diagnostics) = upstream_diagnostics.as_mut() {
                    diagnostics.set_close_reason("client_update");
                }
                break;
            }
            client_message = client_receiver.next() => {
                let Some(client_message) = client_message else {
                    if let (Some(request), Some(channel)) =
                        (active_subscription_request.take(), channel.as_ref())
                    {
                        request.finalize(499, Some("request"), Some(&shared), channel);
                    }
                    if let Some(diagnostics) = upstream_diagnostics.as_mut() {
                        diagnostics.set_close_reason("downstream_closed");
                    }
                    break;
                };
                let Ok(client_message) = client_message else {
                    if let (Some(request), Some(channel)) =
                        (active_subscription_request.take(), channel.as_ref())
                    {
                        request.finalize(499, Some("request"), Some(&shared), channel);
                    }
                    if let Some(diagnostics) = upstream_diagnostics.as_mut() {
                        diagnostics.set_close_reason("downstream_read_error");
                    }
                    break;
                };
                if client_message.is_text() || client_message.is_binary() {
                    if turn_active
                        && let Ok(text) = client_response_create_text(&client_message)
                        && is_responses_interrupt_frame(text)
                    {
                        if upstream_sender
                            .send(tokio_tungstenite::tungstenite::Message::Text(text.into()))
                            .await
                            .is_err()
                        {
                            if let Some(diagnostics) = upstream_diagnostics.as_mut() {
                                diagnostics.set_close_reason("upstream_send_failed");
                            }
                            break;
                        }
                        // Keep the active request and its usage until the
                        // upstream's terminal response, just like Codex itself.
                        continue;
                    }
                    if turn_active {
                        if send_responses_websocket_error(
                            &mut client_sender,
                            "request_in_progress",
                            "only one response.create request may run at a time",
                        )
                        .await
                        .is_err()
                        {
                            if let Some(diagnostics) = upstream_diagnostics.as_mut() {
                                diagnostics.set_close_reason("downstream_send_failed");
                            }
                            break;
                        }
                        continue;
                    }
                    let request_started_at = std::time::Instant::now();
                    let text = match client_response_create_text(&client_message) {
                        Ok(text) => text,
                        Err((code, message)) => {
                            if send_responses_websocket_error(&mut client_sender, code, message).await.is_err() {
                                if let Some(diagnostics) = upstream_diagnostics.as_mut() {
                                    diagnostics.set_close_reason("downstream_send_failed");
                                }
                                break;
                            }
                            continue;
                        }
                    };
                    let value = match validate_response_create_frame(text) {
                        Ok(value) => value,
                        Err((code, message)) => {
                            if send_responses_websocket_error(&mut client_sender, code, message).await.is_err() {
                                if let Some(diagnostics) = upstream_diagnostics.as_mut() {
                                    diagnostics.set_close_reason("downstream_send_failed");
                                }
                                break;
                            }
                            continue;
                        }
                    };
                    if let Some(model) = value
                        .get("model")
                        .and_then(serde_json::Value::as_str)
                        .map(str::trim)
                        .filter(|model| !model.is_empty())
                    {
                        if channel
                            .as_ref()
                            .is_some_and(|channel| !channel_supports_model(channel, model))
                        {
                            if send_responses_websocket_error(
                                &mut client_sender,
                                "resource_owner_conflict",
                                "a persistent Responses WebSocket cannot switch to a model outside its bound channel",
                            )
                            .await
                            .is_err()
                            {
                                if let Some(diagnostics) = upstream_diagnostics.as_mut() {
                                    diagnostics.set_close_reason("downstream_send_failed");
                                }
                                break;
                            }
                            continue;
                        }
                        active_model = model.to_string();
                    }
                    if let (Some(channel), Some(previous)) = (channel.as_ref(), value
                        .get("previous_response_id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::trim)
                        .filter(|id| !id.is_empty())
                    ) {
                        if shared
                            .local_resource_owners
                            .get(crate::surface::ApiSurface::OpenAi, "response", previous)
                            .is_some_and(|owner| owner.channel_id != channel.id)
                        {
                            if send_responses_websocket_error(
                                &mut client_sender,
                                "resource_owner_conflict",
                                "previous_response_id belongs to another channel",
                            )
                            .await
                            .is_err()
                            {
                                if let Some(diagnostics) = upstream_diagnostics.as_mut() {
                                    diagnostics.set_close_reason("downstream_send_failed");
                                }
                                break;
                            }
                            continue;
                        }
                    }
                    let mut pending_subscription_request = match subscription_config.as_ref() {
                        Some(config) => match NativeSubscriptionWebsocketRequest::start_at(config, text, request_started_at) {
                            Ok(mut request) => {
                                request.local_model_observation = channel.as_ref().map(|channel| LocalModelOutcomeGuard::new(shared.clone(), &channel.id, &request.model));
                                Some(request)
                            }
                            Err(_) => {
                                bridge_handoff = Some(value);
                                if let Some(diagnostics) = upstream_diagnostics.as_mut() {
                                    diagnostics.set_close_reason("http_bridge_handoff");
                                }
                                break;
                            }
                        },
                        None => None,
                    };
                    let Some(next_activity_guard) = crate::update_activity::try_begin_proxy_request() else {
                        let _ = client_sender
                            .send(warp::ws::Message::close_with(1012u16, "client update"))
                            .await;
                        if let Some(diagnostics) = upstream_diagnostics.as_mut() {
                            diagnostics.set_close_reason("client_update");
                        }
                        break;
                    };
                    if let Some(diagnostics) = upstream_diagnostics.as_mut() {
                        diagnostics.mark_turn();
                    }
                    if upstream_sender
                        .send(tokio_tungstenite::tungstenite::Message::Text(responses_websocket_wire_body(channel.as_ref(), text).into_owned().into()))
                        .await
                        .is_err()
                    {
                        if let (Some(request), Some(channel)) =
                            (pending_subscription_request, channel.as_ref())
                        {
                            request.finalize(502, Some("channel"), Some(&shared), channel);
                        }
                        if let Some(diagnostics) = upstream_diagnostics.as_mut() {
                            diagnostics.set_close_reason("upstream_send_failed");
                        }
                        break;
                    }
                    active_activity_guard = Some(next_activity_guard);
                    if let Some(request) = pending_subscription_request.as_mut() {
                        request.mark_sent();
                    }
                    active_subscription_request = pending_subscription_request;
                    turn_active = true;
                } else if client_message.is_ping() {
                    if upstream_sender
                        .send(tokio_tungstenite::tungstenite::Message::Ping(
                            client_message.as_bytes().to_vec().into(),
                        ))
                        .await
                        .is_err()
                    {
                        if let Some(diagnostics) = upstream_diagnostics.as_mut() {
                            diagnostics.set_close_reason("upstream_send_failed");
                        }
                        break;
                    }
                } else if client_message.is_pong() {
                    if upstream_sender
                        .send(tokio_tungstenite::tungstenite::Message::Pong(
                            client_message.as_bytes().to_vec().into(),
                        ))
                        .await
                        .is_err()
                    {
                        if let Some(diagnostics) = upstream_diagnostics.as_mut() {
                            diagnostics.set_close_reason("upstream_send_failed");
                        }
                        break;
                    }
                } else if client_message.is_close() {
                    if let (Some(request), Some(channel)) =
                        (active_subscription_request.take(), channel.as_ref())
                    {
                        request.finalize(499, Some("request"), Some(&shared), channel);
                    }
                    let upstream_close = client_message.close_frame().map(|frame| {
                        tokio_tungstenite::tungstenite::protocol::CloseFrame {
                            code: frame.0.into(),
                            reason: frame.1.to_string().into(),
                        }
                    });
                    // Warp/tokio-tungstenite queues the peer's close reply while
                    // reading the frame. Closing the sink flushes that queued
                    // reply; dropping the split sink here would reset TCP before
                    // the WebSocket close handshake reaches the caller.
                    close_client_websocket_sender(&mut client_sender).await;
                    let _ = upstream_sender
                        .send(tokio_tungstenite::tungstenite::Message::Close(
                            upstream_close,
                        ))
                        .await;
                    if let Some(diagnostics) = upstream_diagnostics.as_mut() {
                        diagnostics.set_close_reason("downstream_close");
                    }
                    break;
                }
            }
            upstream_message = upstream_receiver.next() => {
                let Some(upstream_message) = upstream_message else {
                    if let (Some(request), Some(channel)) =
                        (active_subscription_request.take(), channel.as_ref())
                    {
                        request.finalize(502, Some("channel"), Some(&shared), channel);
                    }
                    close_client_websocket_sender(&mut client_sender).await;
                    if let Some(diagnostics) = upstream_diagnostics.as_mut() {
                        diagnostics.set_close_reason("upstream_eof");
                    }
                    break;
                };
                let Ok(upstream_message) = upstream_message else {
                    if let (Some(request), Some(channel)) =
                        (active_subscription_request.take(), channel.as_ref())
                    {
                        request.finalize(502, Some("channel"), Some(&shared), channel);
                    }
                    close_client_websocket_sender(&mut client_sender).await;
                    if let Some(diagnostics) = upstream_diagnostics.as_mut() {
                        diagnostics.set_close_reason("upstream_read_error");
                    }
                    break;
                };
                let (client_message, terminal_status) = match upstream_message {
                    tokio_tungstenite::tungstenite::Message::Text(text) => {
                        let text = text.to_string();
                        let terminal_metadata = responses_websocket_terminal_metadata(&text);
                        if platform_first_response_pending
                            && responses_websocket_allows_http_bridge(&text)
                        {
                            bridge_handoff = Some(first_value.clone());
                            if let Some(diagnostics) = upstream_diagnostics.as_mut() {
                                diagnostics.set_close_reason("http_bridge_handoff");
                            }
                            break;
                        }
                        platform_first_response_pending = false;
                        if subscription_config.is_some() {
                            if let Some(channel) = channel.as_ref() {
                                crate::detection::observe_openai_subscription_response_event(
                                    channel,
                                    &text,
                                );
                            }
                        }
                        let terminal_status = active_subscription_request
                            .as_mut()
                            .and_then(|request| request.observe(&text))
                            .or_else(|| responses_websocket_terminal_status(&text));
                        if let (Some(metadata), Some(diagnostics)) =
                            (terminal_metadata.as_ref(), upstream_diagnostics.as_ref())
                        {
                            diagnostics.log_terminal(metadata);
                        }
                        if let Some(channel) = channel.as_ref() {
                            let owners = responses_websocket_resource_owners(
                                &text,
                                &channel.id,
                                &active_model,
                            );
                            if !owners.is_empty() {
                                if let Err(error) = shared.local_resource_owners.bind_many(owners).await {
                                    log::warn!(
                                        "[const-api][resource-owner] websocket bind failed code=local_resource_owner_websocket_bind_failed error={error}"
                                    );
                                }
                            }
                        }
                        (warp::ws::Message::text(text), terminal_status)
                    }
                    tokio_tungstenite::tungstenite::Message::Binary(bytes) => {
                        platform_first_response_pending = false;
                        (warp::ws::Message::binary(bytes), None)
                    }
                    tokio_tungstenite::tungstenite::Message::Ping(bytes) => {
                        (warp::ws::Message::ping(bytes), None)
                    }
                    tokio_tungstenite::tungstenite::Message::Pong(bytes) => {
                        (warp::ws::Message::pong(bytes), None)
                    }
                    tokio_tungstenite::tungstenite::Message::Close(frame) => {
                        if let (Some(request), Some(channel)) =
                            (active_subscription_request.take(), channel.as_ref())
                        {
                            request.finalize(502, Some("channel"), Some(&shared), channel);
                        }
                        let message = frame
                            .map(|frame| {
                                warp::ws::Message::close_with(
                                    u16::from(frame.code),
                                    frame.reason.to_string(),
                                )
                            })
                            .unwrap_or_else(warp::ws::Message::close);
                        let _ = client_sender.send(message).await;
                        upstream_sender.close_bounded().await;
                        if let Some(diagnostics) = upstream_diagnostics.as_mut() {
                            diagnostics.set_close_reason("upstream_close_frame");
                        }
                        break;
                    }
                    tokio_tungstenite::tungstenite::Message::Frame(_) => continue,
                };
                if client_sender.send(client_message).await.is_err() {
                    if let (Some(request), Some(channel)) =
                        (active_subscription_request.take(), channel.as_ref())
                    {
                        request.finalize(499, Some("request"), Some(&shared), channel);
                    }
                    if let Some(diagnostics) = upstream_diagnostics.as_mut() {
                        diagnostics.set_close_reason("downstream_send_failed");
                    }
                    break;
                }
                if let Some(status) = terminal_status {
                    if let Some(diagnostics) = upstream_diagnostics.as_mut() {
                        diagnostics.finish_turn();
                    }
                    turn_active = false;
                    if let (Some(request), Some(channel)) =
                        (active_subscription_request.take(), channel.as_ref())
                    {
                        request.finalize(
                            status,
                            (status == 499).then_some("request"),
                            Some(&shared),
                            channel,
                        );
                    }
                    drop(active_activity_guard.take());
                }
            }
        }
    }
    upstream_sender.close_bounded().await;
    if let Some(first_value) = bridge_handoff {
        run_responses_websocket_http_bridge(
            client_sender,
            client_receiver,
            client_headers,
            shared,
            first_value,
            active_activity_guard.take(),
        )
        .await;
    }
}

#[derive(Clone, Copy)]
enum NativeDuplexKind {
    OpenAiRealtime,
    OpenAiLiveConnect,
    OpenAiLive,
    OpenAiRealtimeTranslation,
    GeminiLive,
}

impl NativeDuplexKind {
    const fn surface(self) -> crate::surface::ApiSurface {
        match self {
            Self::OpenAiRealtime
            | Self::OpenAiLiveConnect
            | Self::OpenAiLive
            | Self::OpenAiRealtimeTranslation => {
                crate::surface::ApiSurface::OpenAi
            }
            Self::GeminiLive => crate::surface::ApiSurface::Gemini,
        }
    }

    const fn operation(self) -> crate::surface::ApiOperation {
        match self {
            Self::OpenAiRealtime => crate::surface::ApiOperation::RealtimeWebSocket,
            Self::OpenAiLiveConnect => crate::surface::ApiOperation::RealtimeLiveConnect,
            Self::OpenAiLive => crate::surface::ApiOperation::RealtimeLiveWebSocket,
            Self::OpenAiRealtimeTranslation => {
                crate::surface::ApiOperation::RealtimeTranslationWebSocket
            }
            Self::GeminiLive => crate::surface::ApiOperation::LiveWebSocket,
        }
    }

    const fn route_path(self) -> &'static str {
        match self {
            Self::OpenAiRealtime => "/v1/realtime",
            Self::OpenAiLiveConnect => "/v1/live",
            Self::OpenAiLive => "/v1/live/rtc_placeholder",
            Self::OpenAiRealtimeTranslation => "/v1/realtime/translations",
            Self::GeminiLive => {
                "/gemini/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent"
            }
        }
    }

    const fn openai_call_resource_type(self) -> Option<&'static str> {
        match self {
            Self::OpenAiRealtime => Some("realtime_call"),
            Self::OpenAiLiveConnect => None,
            Self::OpenAiLive => Some("realtime_call"),
            Self::OpenAiRealtimeTranslation => Some("realtime_translation_call"),
            Self::GeminiLive => None,
        }
    }
}

async fn run_openai_realtime_websocket_proxy(
    client_socket: warp::ws::WebSocket,
    raw_query: String,
    client_headers: warp::http::HeaderMap,
    shared: Arc<ProxyShared>,
    activity_guard: crate::update_activity::UpdateActivityGuard,
) {
    run_openai_native_duplex_websocket_proxy(
        client_socket,
        raw_query,
        client_headers,
        shared,
        activity_guard,
        NativeDuplexKind::OpenAiRealtime,
    )
    .await;
}

async fn run_openai_realtime_translation_websocket_proxy(
    client_socket: warp::ws::WebSocket,
    raw_query: String,
    client_headers: warp::http::HeaderMap,
    shared: Arc<ProxyShared>,
    activity_guard: crate::update_activity::UpdateActivityGuard,
) {
    run_openai_native_duplex_websocket_proxy(
        client_socket,
        raw_query,
        client_headers,
        shared,
        activity_guard,
        NativeDuplexKind::OpenAiRealtimeTranslation,
    )
    .await;
}

async fn run_openai_live_websocket_proxy(
    client_socket: warp::ws::WebSocket,
    path: String,
    raw_query: String,
    client_headers: warp::http::HeaderMap,
    shared: Arc<ProxyShared>,
    _activity_guard: crate::update_activity::UpdateActivityGuard,
) {
    let standalone = path == "/v1/live";
    let kind = if standalone {
        NativeDuplexKind::OpenAiLiveConnect
    } else {
        NativeDuplexKind::OpenAiLive
    };
    let (mut client_sender, mut client_receiver) = client_socket.split();
    let request_source = websocket_request_source(&client_headers);
    let reference = local_native_resource_reference(
        crate::surface::ApiSurface::OpenAi,
        &path,
    );
    if let Some(id) = reference.as_ref().map(|reference| reference.resource_id.as_str())
        .filter(|id| id.starts_with(PLATFORM_VOICE_CALL_PREFIX)) {
        relay_platform_voice_sideband(id, true, &client_headers, &shared, client_sender, client_receiver).await;
        return;
    }
    if !standalone && reference.is_none() {
        let _ = send_native_duplex_error(
            &mut client_sender,
            kind,
            "invalid_call_id",
            "the Codex realtime sideband path does not contain a valid call id",
        )
        .await;
        return;
    };
    let owner_channel_id = reference.as_ref().and_then(|reference| {
        shared.local_resource_owners.get(
            crate::surface::ApiSurface::OpenAi,
            "realtime_call",
            &reference.resource_id,
        )
        .map(|owner| owner.channel_id)
    });
    if !standalone && owner_channel_id.is_none() {
        let _ = send_native_duplex_error(
            &mut client_sender,
            kind,
            "resource_owner_not_found",
            "the realtime call owner is unknown; the sideband cannot be rebalanced",
        )
        .await;
        return;
    };
    let model = if standalone {
        websocket_query_parameter(&raw_query, "model")
            .filter(|model| !model.trim().is_empty())
            .unwrap_or_else(|| OPENAI_SUBSCRIPTION_REALTIME_MODEL.to_string())
    } else {
        String::new()
    };
    let local = select_native_duplex_channel(
        &shared,
        kind,
        &path,
        &model,
        owner_channel_id.as_deref(),
    );
    if standalone && should_use_platform_native(&shared, &client_headers, local.is_ok()) {
        match connect_platform_duplex(&shared, &client_headers, Some((&path, &raw_query))).await {
            Ok((session, _, _)) => {
                if !relay_platform_native(&mut client_sender, &mut client_receiver, session,
                    local.is_ok() && native_local_fallback_allowed(&client_headers)).await {
                    return;
                }
            }
            Err(error) if local.is_ok() && native_local_fallback_allowed(&client_headers) && crate::supplier::platform_duplex_error_is_safe_to_replay(&error) => {}
            Err(_) => {
                let _ = send_native_duplex_error(&mut client_sender, kind, "platform_realtime_unavailable", "the platform realtime session could not be opened").await;
                return;
            }
        }
    }
    let channel = match local {
        Ok(channel) => channel,
        Err((code, message)) => {
            let _ = send_native_duplex_error(&mut client_sender, kind, code, message).await;
            return;
        }
    };
    let execution = crate::channel_executor::execution_kind_for_source(channel.source_driver());
    let upstream_socket = if execution
        .is_ok_and(|execution| execution == crate::source_driver::ExecutionKind::OpenAiSubscription)
    {
        match connect_openai_subscription_live(
            &shared.client,
            &channel,
            reference
                .as_ref()
                .map(|reference| reference.resource_id.as_str()),
            &raw_query,
            &client_headers,
        )
        .await
        {
            Ok(socket) => Some(socket),
            Err(error) => {
                let metadata = responses_websocket_connect_error_metadata(&error);
                log::warn!(
                    "[const-api][websocket] upstream connect failed code=upstream_connection_failed operation={} channel_id={} request_source={} status={} error_kind={} provider_request_id={}",
                    kind.operation().as_str(),
                    channel.id,
                    request_source,
                    metadata.status,
                    responses_websocket_connect_error_kind(&metadata),
                    metadata.provider_request_id.as_deref().unwrap_or("-"),
                );
                let _ = send_native_duplex_error(
                    &mut client_sender,
                    kind,
                    "upstream_connection_failed",
                    "the OpenAI subscription Live session could not be reached",
                )
                .await;
                None
            }
        }
    } else {
        match native_duplex_upstream_request(
            &channel,
            kind,
            &path,
            &raw_query,
            &client_headers,
        ) {
            Ok(request) => {
                connect_native_duplex_upstream(
                    request,
                    &mut client_sender,
                    kind,
                    &channel.id,
                    &request_source,
                )
                .await
            }
            Err(_) => {
                let _ = send_native_duplex_error(
                    &mut client_sender,
                    kind,
                    "upstream_configuration_invalid",
                    "the realtime call owner has an invalid sideband target",
                )
                .await;
                None
            }
        }
    };
    let Some(upstream_socket) = upstream_socket else {
        return;
    };
    relay_native_duplex(
        client_sender,
        client_receiver,
        upstream_socket,
        None,
        kind,
        channel,
        model,
        shared,
        request_source,
    )
    .await;
}

async fn run_openai_native_duplex_websocket_proxy(
    client_socket: warp::ws::WebSocket,
    raw_query: String,
    client_headers: warp::http::HeaderMap,
    shared: Arc<ProxyShared>,
    _activity_guard: crate::update_activity::UpdateActivityGuard,
    kind: NativeDuplexKind,
) {
    let (mut client_sender, mut client_receiver) = client_socket.split();
    let request_source = websocket_request_source(&client_headers);
    let model = websocket_query_parameter(&raw_query, "model")
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
        .to_string();
    let call_id = websocket_query_parameter(&raw_query, "call_id")
        .filter(|value| !value.trim().is_empty());
    if let Some(id) = call_id.as_deref().filter(|id| id.starts_with(PLATFORM_VOICE_CALL_PREFIX)) {
        relay_platform_voice_sideband(id, false, &client_headers, &shared, client_sender, client_receiver).await;
        return;
    }
    let Some(call_resource_type) = kind.openai_call_resource_type() else {
        let _ = send_native_duplex_error(
            &mut client_sender,
            kind,
            "internal_route_mismatch",
            "the selected duplex handler does not match the OpenAI Realtime route",
        )
        .await;
        return;
    };
    let owner_channel_id = call_id.as_deref().and_then(|id| {
        shared
            .local_resource_owners
            .get(crate::surface::ApiSurface::OpenAi, call_resource_type, id)
            .map(|owner| owner.channel_id)
    });
    if call_id.is_some() && owner_channel_id.is_none() {
        let _ = send_native_duplex_error(
            &mut client_sender,
            kind,
            "resource_owner_not_found",
            "the realtime call owner is unknown; create a new call before attaching a sideband",
        )
        .await;
        return;
    }
    let local = select_native_duplex_channel(
        &shared,
        kind,
        kind.route_path(),
        &model,
        owner_channel_id.as_deref(),
    );
    if matches!(kind, NativeDuplexKind::OpenAiRealtime) && call_id.is_none()
        && should_use_platform_native(&shared, &client_headers, local.is_ok())
    {
        match connect_platform_duplex(&shared, &client_headers, Some((kind.route_path(), &raw_query))).await {
            Ok((session, _, _)) => {
                if !relay_platform_native(&mut client_sender, &mut client_receiver, session,
                    local.is_ok() && native_local_fallback_allowed(&client_headers)).await {
                    return;
                }
            }
            Err(error) if local.is_ok() && native_local_fallback_allowed(&client_headers) && crate::supplier::platform_duplex_error_is_safe_to_replay(&error) => {}
            Err(_) => {
                let _ = send_native_duplex_error(&mut client_sender, kind, "platform_realtime_unavailable", "the platform realtime session could not be opened").await;
                return;
            }
        }
    }
    let channel = match local {
        Ok(channel) => channel,
        Err((code, message)) => {
            let _ = send_native_duplex_error(
                &mut client_sender,
                kind,
                code,
                message,
            )
            .await;
            return;
        }
    };
    let upstream_request = match native_duplex_upstream_request(
        &channel,
        kind,
        kind.route_path(),
        &raw_query,
        &client_headers,
    ) {
        Ok(request) => request,
        Err(_) => {
            let _ = send_native_duplex_error(
                &mut client_sender,
                kind,
                "upstream_configuration_invalid",
                "the selected channel has an invalid OpenAI Realtime WebSocket target",
            )
            .await;
            return;
        }
    };
    let Some(upstream_socket) = connect_native_duplex_upstream(
        upstream_request,
        &mut client_sender,
        kind,
        &channel.id,
        &request_source,
    )
    .await
    else {
        return;
    };
    relay_native_duplex(
        client_sender,
        client_receiver,
        upstream_socket,
        None,
        kind,
        channel,
        model,
        shared,
        request_source,
    )
    .await;
}

async fn run_gemini_live_websocket_proxy(
    client_socket: warp::ws::WebSocket,
    raw_query: String,
    client_headers: warp::http::HeaderMap,
    shared: Arc<ProxyShared>,
    _activity_guard: crate::update_activity::UpdateActivityGuard,
) {
    let (mut client_sender, mut client_receiver) = client_socket.split();
    let request_source = websocket_request_source(&client_headers);
    let first = match tokio::time::timeout(
        DUPLEX_WEBSOCKET_FIRST_FRAME_TIMEOUT,
        client_receiver.next(),
    )
    .await
    {
        Ok(Some(Ok(message))) => message,
        Ok(Some(Err(_))) | Ok(None) => return,
        Err(_) => {
            let _ = send_native_duplex_error(
                &mut client_sender,
                NativeDuplexKind::GeminiLive,
                "first_frame_timeout",
                "the first Live API setup frame was not received within 30 seconds",
            )
            .await;
            return;
        }
    };
    let (model, resumption_handle) = match parse_gemini_live_setup(&first) {
        Ok(setup) => setup,
        Err((code, message)) => {
            let _ = send_native_duplex_error(
                &mut client_sender,
                NativeDuplexKind::GeminiLive,
                code,
                message,
            )
            .await;
            return;
        }
    };
    let owner_channel_id = resumption_handle.as_deref().and_then(|handle| {
        shared
            .local_resource_owners
            .get(
                crate::surface::ApiSurface::Gemini,
                "live_session",
                handle,
            )
            .map(|owner| owner.channel_id)
    });
    if resumption_handle.is_some() && owner_channel_id.is_none() {
        let _ = send_native_duplex_error(
            &mut client_sender,
            NativeDuplexKind::GeminiLive,
            "resource_owner_not_found",
            "the Live session owner is unknown; start a new session without a resumption handle",
        )
        .await;
        return;
    }
    let local = select_native_duplex_channel(
        &shared,
        NativeDuplexKind::GeminiLive,
        NativeDuplexKind::GeminiLive.route_path(),
        &model,
        owner_channel_id.as_deref(),
    );
    if resumption_handle.is_none() && should_use_platform_native(&shared, &client_headers, local.is_ok()) {
        let mut query_url = reqwest::Url::parse("http://localhost/").ok();
        if let Some(url) = query_url.as_mut() {
            let query = remove_websocket_query_parameter(&raw_query, "key");
            url.set_query((!query.is_empty()).then_some(query.as_str()));
            url.query_pairs_mut().append_pair("model", &model);
            match connect_platform_duplex(&shared, &client_headers, Some((NativeDuplexKind::GeminiLive.route_path(), url.query().unwrap_or_default()))).await {
                Ok((session, _, _)) => {
                    if !relay_platform_native_with_first(&mut client_sender, &mut client_receiver, session, Some(&first),
                        local.is_ok() && native_local_fallback_allowed(&client_headers)).await {
                        return;
                    }
                }
                Err(error) if local.is_ok() && native_local_fallback_allowed(&client_headers) && crate::supplier::platform_duplex_error_is_safe_to_replay(&error) => {}
                Err(_) => {
                    let _ = send_native_duplex_error(&mut client_sender, NativeDuplexKind::GeminiLive, "platform_realtime_unavailable", "the platform Live session could not be opened").await;
                    return;
                }
            }
        }
    }
    let channel = match local {
        Ok(channel) => channel,
        Err((code, message)) => {
            let _ = send_native_duplex_error(
                &mut client_sender,
                NativeDuplexKind::GeminiLive,
                code,
                message,
            )
            .await;
            return;
        }
    };
    let upstream_request = match native_duplex_upstream_request(
        &channel,
        NativeDuplexKind::GeminiLive,
        NativeDuplexKind::GeminiLive.route_path(),
        &raw_query,
        &client_headers,
    ) {
        Ok(request) => request,
        Err(_) => {
            let _ = send_native_duplex_error(
                &mut client_sender,
                NativeDuplexKind::GeminiLive,
                "upstream_configuration_invalid",
                "the selected channel has an invalid Gemini Live WebSocket target",
            )
            .await;
            return;
        }
    };
    let Some(upstream_socket) = connect_native_duplex_upstream(
        upstream_request,
        &mut client_sender,
        NativeDuplexKind::GeminiLive,
        &channel.id,
        &request_source,
    )
    .await
    else {
        return;
    };
    relay_native_duplex(
        client_sender,
        client_receiver,
        upstream_socket,
        Some(warp_message_to_tungstenite(first)),
        NativeDuplexKind::GeminiLive,
        channel,
        model,
        shared,
        request_source,
    )
    .await;
}

fn parse_gemini_live_setup(
    message: &warp::ws::Message,
) -> std::result::Result<(String, Option<String>), (&'static str, &'static str)> {
    let text = if message.is_text() || message.is_binary() {
        std::str::from_utf8(message.as_bytes()).map_err(|_| {
            (
                "invalid_websocket_event",
                "the Live API setup frame must contain UTF-8 JSON",
            )
        })?
    } else {
        return Err((
            "invalid_websocket_event",
            "the first Live API frame must be setup JSON",
        ));
    };
    let value = serde_json::from_str::<serde_json::Value>(text).map_err(|_| {
        (
            "invalid_websocket_event",
            "the Live API setup frame must contain valid JSON",
        )
    })?;
    let setup = value.get("setup").and_then(serde_json::Value::as_object).ok_or((
        "invalid_websocket_event",
        "the first Live API frame must contain exactly one setup message",
    ))?;
    if value.as_object().is_none_or(|object| object.len() != 1) {
        return Err((
            "invalid_websocket_event",
            "the first Live API frame must contain exactly one setup message",
        ));
    }
    let model = setup
        .get("model")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or((
            "model_required",
            "the Live API setup message must include a model",
        ))?
        .strip_prefix("models/")
        .unwrap_or_else(|| {
            setup
                .get("model")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .trim()
        })
        .to_string();
    let resumption_handle = setup
        .get("sessionResumption")
        .or_else(|| setup.get("session_resumption"))
        .and_then(|value| value.get("handle"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    Ok((model, resumption_handle))
}

fn select_native_duplex_channel(
    shared: &ProxyShared,
    kind: NativeDuplexKind,
    route_path: &str,
    model: &str,
    owner_channel_id: Option<&str>,
) -> std::result::Result<ChannelConfig, (&'static str, &'static str)> {
    let config = shared.config_snapshot();
    let ready_channel_ids = shared.ready_local_channel_ids();
    let route = crate::surface::resolve_api_route(
        "GET",
        route_path,
        true,
    )
    .ok_or((
        "native_duplex_route_unavailable",
        "the native duplex route is not available in this client build",
    ))?;

    if let Some(owner_channel_id) = owner_channel_id {
        let selected = local_channels(&config).find(|channel| {
            if channel.id != owner_channel_id
                || !ready_channel_ids.contains(&channel.id)
                || (!model.is_empty() && !channel_supports_model(channel, model))
            {
                return false;
            }
            match crate::channel_executor::execution_kind_for_source(channel.source_driver()) {
                Ok(crate::source_driver::ExecutionKind::OpenAiSubscription)
                    if matches!(kind, NativeDuplexKind::OpenAiLiveConnect | NativeDuplexKind::OpenAiLive) =>
                {
                    true
                }
                Ok(crate::source_driver::ExecutionKind::HttpSurface) => {
                    crate::channel_surface::channel_surface_target(
                        channel,
                        route.surface,
                        route.protocol,
                        true,
                    )
                    .is_some_and(|target| target.surface == route.surface)
                }
                _ => false,
            }
        });
        return selected.cloned().ok_or((
            "resource_owner_unavailable",
            "the channel that owns this resumable session is unavailable",
        ));
    }

    let selection_body = if model.is_empty() {
        Vec::new()
    } else {
        serde_json::to_vec(&serde_json::json!({"model": model})).unwrap_or_default()
    };
    if matches!(kind, NativeDuplexKind::OpenAiLiveConnect) {
        // GPT-Live entitlement is separate from the subscription's Responses
        // model list. Its absence from that list does not prove Voice is denied.
        return select_openai_realtime_call_channel_where(&config, model, |channel| {
            ready_channel_ids.contains(&channel.id)
        })
        .cloned()
        .ok_or((
            "no_local_api_channel",
            "no ready channel can execute this Codex Live session",
        ));
    }
    match select_direct_http_surface_channel_where(
        &config,
        kind.surface(),
        route_path,
        &selection_body,
        true,
        |channel| ready_channel_ids.contains(&channel.id),
    ) {
        Some(channel) => Ok(channel.clone()),
        None => Err((
            "no_local_api_channel",
            "no ready verified native API-credential channel can execute this WebSocket operation",
        )),
    }
}

fn native_duplex_upstream_request(
    channel: &ChannelConfig,
    kind: NativeDuplexKind,
    route_path: &str,
    raw_query: &str,
    client_headers: &warp::http::HeaderMap,
) -> Result<tokio_tungstenite::tungstenite::http::Request<()>> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let route = crate::surface::resolve_api_route(
        "GET",
        route_path,
        true,
    )
    .ok_or_else(|| anyhow!("native duplex route is unavailable in this client build"))?;
    let target = crate::channel_surface::channel_surface_target(
        channel,
        route.surface,
        route.protocol,
        true,
    )
    .ok_or_else(|| anyhow!("channel has no verified native WebSocket surface"))?;
    let http_url = match kind {
        NativeDuplexKind::OpenAiRealtime
        | NativeDuplexKind::OpenAiLiveConnect
        | NativeDuplexKind::OpenAiLive
        | NativeDuplexKind::OpenAiRealtimeTranslation => {
            crate::supplier::join_upstream_url(&target.base_url, route_path)
        }
        NativeDuplexKind::GeminiLive => {
            let base = reqwest::Url::parse(target.base_url.trim())?;
            let host = base
                .host_str()
                .ok_or_else(|| anyhow!("Gemini Live upstream has no host"))?;
            let mut authority = host.to_string();
            if let Some(port) = base.port() {
                authority.push(':');
                authority.push_str(&port.to_string());
            }
            format!(
                "{}://{authority}/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent",
                base.scheme()
            )
        }
    };
    let mut url = reqwest::Url::parse(&http_url)?;
    let websocket_scheme = match url.scheme() {
        "http" => "ws",
        "https" => "wss",
        _ => return Err(anyhow!("native WebSocket requires an HTTP(S) API base URL")),
    };
    url.set_scheme(websocket_scheme)
        .map_err(|_| anyhow!("cannot convert API base URL to WebSocket"))?;
    let query = match kind {
        NativeDuplexKind::OpenAiRealtime
        | NativeDuplexKind::OpenAiLiveConnect
        | NativeDuplexKind::OpenAiLive
        | NativeDuplexKind::OpenAiRealtimeTranslation => {
            raw_query.to_string()
        }
        NativeDuplexKind::GeminiLive => remove_websocket_query_parameter(raw_query, "key"),
    };
    url.set_query((!query.is_empty()).then_some(query.as_str()));
    if matches!(kind, NativeDuplexKind::GeminiLive)
        && target.auth_scheme == "x_goog_api_key"
        && !channel.upstream_api_key.trim().is_empty()
    {
        url.query_pairs_mut()
            .append_pair("key", channel.upstream_api_key.trim());
    }
    let mut request = url.as_str().into_client_request()?;
    copy_safe_websocket_headers(request.headers_mut(), client_headers);
    if !(matches!(kind, NativeDuplexKind::GeminiLive)
        && target.auth_scheme == "x_goog_api_key")
    {
        let mut auth_headers = reqwest::header::HeaderMap::new();
        crate::channel_executor::apply_upstream_auth(
            &mut auth_headers,
            channel,
            &target,
            true,
        )?;
        for (name, value) in auth_headers {
            if let Some(name) = name {
                request.headers_mut().insert(name, value);
            }
        }
    }
    Ok(request)
}

fn copy_safe_websocket_headers(
    destination: &mut tokio_tungstenite::tungstenite::http::HeaderMap,
    source: &warp::http::HeaderMap,
) {
    for (name, value) in source {
        let lower = name.as_str().to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "authorization"
                | "host"
                | "connection"
                | "upgrade"
                | "proxy-authorization"
                | "api-key"
                | "x-api-key"
                | "x-goog-api-key"
                | "cookie"
                | "sec-websocket-key"
                | "sec-websocket-version"
                | "sec-websocket-extensions"
                | "sec-websocket-protocol"
        ) || lower.starts_with("x-const-")
        {
            continue;
        }
        destination.append(name, value.clone());
    }
}

async fn connect_native_duplex_upstream(
    request: tokio_tungstenite::tungstenite::http::Request<()>,
    client_sender: &mut futures_util::stream::SplitSink<warp::ws::WebSocket, warp::ws::Message>,
    kind: NativeDuplexKind,
    channel_id: &str,
    request_source: &str,
) -> Option<
    tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
> {
    let websocket_config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(DUPLEX_WEBSOCKET_MAX_MESSAGE_BYTES))
        .max_frame_size(Some(DUPLEX_WEBSOCKET_MAX_MESSAGE_BYTES));
    match bounded_websocket_connect(
        tokio_tungstenite::connect_async_with_config(request, Some(websocket_config), false),
        WEBSOCKET_UPSTREAM_CONNECT_TIMEOUT,
    )
    .await
    {
        Some(Ok((connection, _))) => Some(connection),
        result => {
            let (code, message, status, error_kind, provider_request_id) = match result {
                None => (
                    "upstream_connection_timeout",
                    "the selected WebSocket upstream did not connect within 20 seconds",
                    504,
                    "upstream_websocket_connect_timeout".to_string(),
                    None,
                ),
                Some(Err(error)) => {
                    let metadata = responses_websocket_connect_error_metadata(&anyhow!(error));
                    let error_kind = responses_websocket_connect_error_kind(&metadata).to_string();
                    (
                        "upstream_connection_failed",
                        "the selected WebSocket upstream could not be reached",
                        metadata.status,
                        error_kind,
                        metadata.provider_request_id,
                    )
                }
                Some(Ok(_)) => unreachable!("successful WebSocket connection handled above"),
            };
            log::warn!(
                "[const-api][websocket] upstream connect failed code={} operation={} channel_id={} request_source={} status={} error_kind={} provider_request_id={}",
                code,
                kind.operation().as_str(),
                channel_id,
                request_source,
                status,
                error_kind,
                provider_request_id.as_deref().unwrap_or("-"),
            );
            let _ = send_native_duplex_error(client_sender, kind, code, message).await;
            None
        }
    }
}

async fn relay_native_duplex(
    mut client_sender: futures_util::stream::SplitSink<warp::ws::WebSocket, warp::ws::Message>,
    mut client_receiver: futures_util::stream::SplitStream<warp::ws::WebSocket>,
    upstream_socket: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    first_message: Option<tokio_tungstenite::tungstenite::Message>,
    kind: NativeDuplexKind,
    channel: ChannelConfig,
    route_model: String,
    shared: Arc<ProxyShared>,
    request_source: String,
) {
    let mut upstream_diagnostics = UpstreamWebsocketLease::open(
        kind.operation().as_str(),
        channel.id.clone(),
        request_source,
    );
    upstream_diagnostics.mark_turn();
    let (mut upstream_sender, mut upstream_receiver) = upstream_socket.split();
    if let Some(first_message) = first_message {
        if upstream_sender.send(first_message).await.is_err() {
            let _ = send_native_duplex_error(
                &mut client_sender,
                kind,
                "upstream_connection_failed",
                "the first WebSocket frame could not be sent upstream",
            )
            .await;
            upstream_diagnostics.set_close_reason("first_frame_send_failed");
            close_upstream_websocket_sender(&mut upstream_sender).await;
            return;
        }
    }

    loop {
        tokio::select! {
            _ = crate::update_activity::wait_for_update_drain() => {
                let _ = client_sender
                    .send(warp::ws::Message::close_with(1012u16, "client update"))
                    .await;
                let _ = upstream_sender
                    .send(tokio_tungstenite::tungstenite::Message::Close(Some(
                        tokio_tungstenite::tungstenite::protocol::CloseFrame {
                            code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Restart,
                            reason: "client update".into(),
                        },
                    )))
                    .await;
                upstream_diagnostics.set_close_reason("client_update");
                break;
            }
            client_message = client_receiver.next() => {
                let Some(client_message) = client_message else {
                    close_upstream_websocket_sender(&mut upstream_sender).await;
                    upstream_diagnostics.set_close_reason("downstream_closed");
                    break;
                };
                let Ok(client_message) = client_message else {
                    close_upstream_websocket_sender(&mut upstream_sender).await;
                    upstream_diagnostics.set_close_reason("downstream_read_error");
                    break;
                };
                let closes = client_message.is_close();
                if upstream_sender
                    .send(warp_message_to_tungstenite(client_message))
                    .await
                    .is_err()
                {
                    upstream_diagnostics.set_close_reason("upstream_send_failed");
                    break;
                }
                if closes {
                    close_client_websocket_sender(&mut client_sender).await;
                    upstream_diagnostics.set_close_reason("downstream_close");
                    break;
                }
            }
            upstream_message = upstream_receiver.next() => {
                let Some(upstream_message) = upstream_message else {
                    close_client_websocket_sender(&mut client_sender).await;
                    upstream_diagnostics.set_close_reason("upstream_eof");
                    break;
                };
                let Ok(upstream_message) = upstream_message else {
                    close_client_websocket_sender(&mut client_sender).await;
                    upstream_diagnostics.set_close_reason("upstream_read_error");
                    break;
                };
                if let tokio_tungstenite::tungstenite::Message::Text(text) = &upstream_message {
                    if matches!(kind, NativeDuplexKind::OpenAiLiveConnect | NativeDuplexKind::OpenAiLive)
                        && crate::channel_executor::execution_kind_for_source(
                            channel.source_driver(),
                        )
                        .is_ok_and(|execution| {
                            execution == crate::source_driver::ExecutionKind::OpenAiSubscription
                        })
                    {
                        crate::detection::observe_openai_subscription_response_event(
                            &channel,
                            text.as_ref(),
                        );
                    }
                    let owners = native_duplex_resource_owners(
                        kind,
                        text.as_ref(),
                        &channel.id,
                        &route_model,
                    );
                    if !owners.is_empty() {
                        if let Err(error) = shared.local_resource_owners.bind_many(owners).await {
                            log::warn!(
                                "[const-api][resource-owner] duplex bind failed code=local_resource_owner_duplex_bind_failed operation={} error={error}",
                                kind.operation().as_str()
                            );
                        }
                    }
                }
                let closes = upstream_message.is_close();
                if let Some(client_message) = tungstenite_message_to_warp(upstream_message) {
                    if client_sender.send(client_message).await.is_err() {
                        close_upstream_websocket_sender(&mut upstream_sender).await;
                        upstream_diagnostics.set_close_reason("downstream_send_failed");
                        break;
                    }
                }
                if closes {
                    close_upstream_websocket_sender(&mut upstream_sender).await;
                    upstream_diagnostics.set_close_reason("upstream_close_frame");
                    break;
                }
            }
        }
    }
    close_upstream_websocket_sender(&mut upstream_sender).await;
}

fn warp_message_to_tungstenite(
    message: warp::ws::Message,
) -> tokio_tungstenite::tungstenite::Message {
    if message.is_text() {
        tokio_tungstenite::tungstenite::Message::Text(
            message.to_str().unwrap_or_default().to_string().into(),
        )
    } else if message.is_binary() {
        tokio_tungstenite::tungstenite::Message::Binary(message.as_bytes().to_vec().into())
    } else if message.is_ping() {
        tokio_tungstenite::tungstenite::Message::Ping(message.as_bytes().to_vec().into())
    } else if message.is_pong() {
        tokio_tungstenite::tungstenite::Message::Pong(message.as_bytes().to_vec().into())
    } else {
        tokio_tungstenite::tungstenite::Message::Close(message.close_frame().map(|frame| {
            tokio_tungstenite::tungstenite::protocol::CloseFrame {
                code: frame.0.into(),
                reason: frame.1.to_string().into(),
            }
        }))
    }
}

fn tungstenite_message_to_warp(
    message: tokio_tungstenite::tungstenite::Message,
) -> Option<warp::ws::Message> {
    match message {
        tokio_tungstenite::tungstenite::Message::Text(text) => {
            Some(warp::ws::Message::text(text.to_string()))
        }
        tokio_tungstenite::tungstenite::Message::Binary(bytes) => {
            Some(warp::ws::Message::binary(bytes))
        }
        tokio_tungstenite::tungstenite::Message::Ping(bytes) => {
            Some(warp::ws::Message::ping(bytes))
        }
        tokio_tungstenite::tungstenite::Message::Pong(bytes) => {
            Some(warp::ws::Message::pong(bytes))
        }
        tokio_tungstenite::tungstenite::Message::Close(frame) => Some(
            frame
                .map(|frame| {
                    warp::ws::Message::close_with(
                        u16::from(frame.code),
                        frame.reason.to_string(),
                    )
                })
                .unwrap_or_else(warp::ws::Message::close),
        ),
        tokio_tungstenite::tungstenite::Message::Frame(_) => None,
    }
}

fn native_duplex_resource_owners(
    kind: NativeDuplexKind,
    text: &str,
    channel_id: &str,
    route_model: &str,
) -> Vec<LocalResourceOwner> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let resources = match kind {
        NativeDuplexKind::OpenAiRealtime
        | NativeDuplexKind::OpenAiLiveConnect
        | NativeDuplexKind::OpenAiLive
        | NativeDuplexKind::OpenAiRealtimeTranslation => value
            .pointer("/session/id")
            .and_then(local_resource_id_from_json)
            .map(|id| {
                let resource_type = if matches!(kind, NativeDuplexKind::OpenAiRealtimeTranslation)
                {
                    "realtime_translation_session"
                } else {
                    "realtime_session"
                };
                vec![(resource_type, id)]
            })
            .unwrap_or_default(),
        NativeDuplexKind::GeminiLive => value
            .pointer("/sessionResumptionUpdate/newHandle")
            .or_else(|| value.pointer("/session_resumption_update/new_handle"))
            .and_then(local_resource_id_from_json)
            .map(|id| vec![("live_session", id)])
            .unwrap_or_default(),
    };
    resources
        .into_iter()
        .filter_map(|(resource_type, resource_id)| {
            local_resource_owner_from_identity(
                kind.surface(),
                resource_type,
                resource_id,
                channel_id,
                route_model,
            )
        })
        .collect()
}

async fn send_native_duplex_error(
    sender: &mut futures_util::stream::SplitSink<warp::ws::WebSocket, warp::ws::Message>,
    kind: NativeDuplexKind,
    code: &str,
    message: &str,
) -> std::result::Result<(), warp::Error> {
    let value = match kind {
        NativeDuplexKind::OpenAiRealtime
        | NativeDuplexKind::OpenAiLiveConnect
        | NativeDuplexKind::OpenAiLive
        | NativeDuplexKind::OpenAiRealtimeTranslation => serde_json::json!({
            "type": "error",
            "error": {
                "type": "invalid_request_error",
                "code": code,
                "message": message
            }
        }),
        NativeDuplexKind::GeminiLive => serde_json::json!({
            "error": {
                "code": 400,
                "status": "INVALID_ARGUMENT",
                "message": message,
                "details": [{"reason": code}]
            }
        }),
    };
    sender
        .send(warp::ws::Message::text(value.to_string()))
        .await
}

fn websocket_query_parameter(raw_query: &str, name: &str) -> Option<String> {
    reqwest::Url::parse(&format!("http://localhost/?{raw_query}"))
        .ok()?
        .query_pairs()
        .find_map(|(candidate, value)| (candidate == name).then(|| value.into_owned()))
}

fn remove_websocket_query_parameter(raw_query: &str, name: &str) -> String {
    raw_query
        .split('&')
        .filter(|segment| segment.split_once('=').map_or(*segment, |pair| pair.0) != name)
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("&")
}

fn client_response_create_text(
    message: &warp::ws::Message,
) -> std::result::Result<&str, (&'static str, &'static str)> {
    if message.is_text() {
        return message.to_str().map_err(|_| {
            (
                "invalid_websocket_event",
                "response.create frames must contain UTF-8 JSON",
            )
        });
    }
    if message.is_binary() {
        return std::str::from_utf8(message.as_bytes()).map_err(|_| {
            (
                "invalid_websocket_event",
                "response.create frames must contain UTF-8 JSON",
            )
        });
    }
    Err((
        "invalid_websocket_event",
        "the first WebSocket frame must be response.create JSON",
    ))
}

fn is_responses_interrupt_frame(text: &str) -> bool {
    #[derive(serde::Deserialize)]
    struct Event<'a> {
        #[serde(rename = "type")]
        kind: &'a str,
    }
    serde_json::from_str::<Event<'_>>(text)
        .is_ok_and(|event| event.kind == "response.interrupt")
}

fn validate_response_create_frame(
    text: &str,
) -> std::result::Result<serde_json::Value, (&'static str, &'static str)> {
    let value = serde_json::from_str::<serde_json::Value>(text).map_err(|_| {
        (
            "invalid_websocket_event",
            "response.create frames must contain valid JSON",
        )
    })?;
    if value.get("type").and_then(serde_json::Value::as_str) != Some("response.create") {
        return Err((
            "invalid_websocket_event",
            "the client event type must be response.create",
        ));
    }
    if value
        .get("stream")
        .is_some_and(|stream| stream.as_bool() != Some(true))
    {
        return Err((
            "transport_field_not_supported",
            "Responses WebSocket requests must stream",
        ));
    }
    if value
        .get("background")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        return Err((
            "transport_field_not_supported",
            "background is not supported in WebSocket mode",
        ));
    }
    Ok(value)
}

#[derive(Default)]
struct ResponsesWebsocketBridgeState {
    history: Vec<serde_json::Value>,
    last_response_id: Option<String>,
}

enum ResponsesWebsocketBridgeRequest {
    Warmup { response_id: String },
    Http {
        body: bytes::Bytes,
        submitted_input: Vec<serde_json::Value>,
        reusable_history: bool,
    },
}

impl ResponsesWebsocketBridgeState {
    fn prepare(
        &mut self,
        mut frame: serde_json::Value,
    ) -> std::result::Result<ResponsesWebsocketBridgeRequest, (&'static str, &'static str)> {
        let input = responses_websocket_input_items(frame.get("input"))?;
        if frame.get("generate").and_then(serde_json::Value::as_bool) == Some(false) {
            let response_id = format!("resp_const_warmup_{:032x}", rand::random::<u128>());
            self.history = input;
            self.last_response_id = Some(response_id.clone());
            return Ok(ResponsesWebsocketBridgeRequest::Warmup { response_id });
        }

        let object = frame.as_object_mut().ok_or((
            "invalid_websocket_event",
            "response.create frames must contain a JSON object",
        ))?;
        object.remove("type");
        object.remove("generate");
        object.remove("stream_id");
        object.insert("stream".to_string(), serde_json::Value::Bool(true));

        let previous = object
            .get("previous_response_id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        // A server-side conversation may contain tool calls we have never seen.
        // Do not validate or later replay its partial local history as complete.
        let has_conversation = object
            .get("conversation")
            .is_some_and(|value| !value.is_null());
        let expands_previous = !has_conversation
            && previous.is_some()
            && previous == self.last_response_id.as_deref();
        let self_contained = !has_conversation && (previous.is_none() || expands_previous);
        let reusable_history = !has_conversation
            && (previous.is_none()
                || previous == self.last_response_id.as_deref().filter(|_| !self.history.is_empty()));
        let submitted_input = if expands_previous {
            object.remove("previous_response_id");
            let mut merged = self.history.clone();
            merged.extend(input);
            merged
        } else {
            input
        };
        if self_contained && responses_websocket_has_orphaned_tool_output(&submitted_input) {
            return Err((
                "invalid_websocket_continuation",
                "the Responses WebSocket HTTP bridge cannot send a tool output without its preceding tool call",
            ));
        }
        object.insert(
            "input".to_string(),
            serde_json::Value::Array(submitted_input.clone()),
        );
        let body = serde_json::to_vec(&frame).map_err(|_| {
            (
                "invalid_websocket_event",
                "response.create could not be encoded for the HTTP fallback",
            )
        })?;
        Ok(ResponsesWebsocketBridgeRequest::Http {
            body: bytes::Bytes::from(body),
            submitted_input,
            reusable_history,
        })
    }

    fn commit(
        &mut self,
        submitted_input: Vec<serde_json::Value>,
        reusable_history: bool,
        capture: ResponsesWebsocketBridgeCapture,
    ) {
        if !reusable_history || !capture.completed {
            self.history.clear();
            self.last_response_id = None;
            return;
        }
        let Some(response_id) = capture.response_id.clone() else {
            self.history.clear();
            self.last_response_id = None;
            return;
        };
        self.history = submitted_input;
        self.history.extend(capture.into_output());
        self.last_response_id = Some(response_id);
    }
}

fn responses_websocket_has_orphaned_tool_output(input: &[serde_json::Value]) -> bool {
    let mut calls = std::collections::HashSet::new();
    for item in input {
        let (family, is_call) = match item.get("type").and_then(serde_json::Value::as_str) {
            Some("function_call" | "local_shell_call") => ("function", true),
            Some("function_call_output") => ("function", false),
            Some("custom_tool_call") => ("custom", true),
            Some("custom_tool_call_output") => ("custom", false),
            Some("shell_call") => ("shell", true),
            Some("shell_call_output") => ("shell", false),
            Some("computer_call") => ("computer", true),
            Some("computer_call_output") => ("computer", false),
            Some("apply_patch_call") => ("apply_patch", true),
            Some("apply_patch_call_output") => ("apply_patch", false),
            Some("tool_search_call") => ("tool_search", true),
            Some("tool_search_output")
                if item.get("execution").and_then(serde_json::Value::as_str) != Some("server") =>
            {
                ("tool_search", false)
            }
            // Future extension items belong to the upstream protocol, not the
            // Codex-specific continuation validator used by subscription replay.
            _ => continue,
        };
        let call_id = item
            .get("call_id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        match (is_call, call_id) {
            (true, Some(call_id)) => {
                calls.insert((family, call_id));
            }
            (false, Some(call_id)) if !calls.contains(&(family, call_id)) => return true,
            (false, None) => {
                // Retain the existing legacy named-function-output allowance.
                if family != "function"
                    || !item
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|name| !name.trim().is_empty())
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

fn responses_websocket_input_items(
    input: Option<&serde_json::Value>,
) -> std::result::Result<Vec<serde_json::Value>, (&'static str, &'static str)> {
    match input {
        None | Some(serde_json::Value::Null) => Ok(Vec::new()),
        Some(serde_json::Value::Array(items)) => Ok(items.clone()),
        Some(serde_json::Value::String(text)) => Ok(vec![serde_json::json!({
            "role": "user",
            "content": text,
        })]),
        Some(_) => Err((
            "invalid_websocket_event",
            "Responses WebSocket input must be a string or an array",
        )),
    }
}

fn responses_websocket_bridge_output_items_match(
    left: &serde_json::Value,
    right: &serde_json::Value,
) -> bool {
    if left == right {
        return true;
    }
    if left.get("type").and_then(serde_json::Value::as_str)
        != right.get("type").and_then(serde_json::Value::as_str)
    {
        return false;
    }
    ["id", "call_id"].into_iter().any(|field| {
        let left = left
            .get(field)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let right = right
            .get(field)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        left.is_some() && left == right
    })
}

#[derive(Default)]
struct ResponsesWebsocketBridgeCapture {
    response_id: Option<String>,
    indexed_output: std::collections::BTreeMap<u64, serde_json::Value>,
    unindexed_output: Vec<serde_json::Value>,
    completed: bool,
    terminal: bool,
}

impl ResponsesWebsocketBridgeCapture {
    fn observe(&mut self, event: &serde_json::Value) {
        let event_type = event.get("type").and_then(serde_json::Value::as_str);
        self.terminal |= matches!(
            event_type,
            Some(
                "response.completed"
                    | "response.incomplete"
                    | "response.failed"
                    | "response.cancelled"
                    | "error"
            )
        );
        match event_type {
            Some("response.created") => {
                if let Some(response_id) = responses_websocket_bridge_response_id(event) {
                    self.response_id = Some(response_id.to_string());
                }
            }
            Some("response.output_item.done") => {
                if let Some(item) = event.get("item").cloned() {
                    if let Some(index) = event
                        .get("output_index")
                        .and_then(serde_json::Value::as_u64)
                    {
                        self.indexed_output.insert(index, item);
                    } else if !self.unindexed_output.iter().any(|existing| {
                        responses_websocket_bridge_output_items_match(existing, &item)
                    }) {
                        self.unindexed_output.push(item);
                    }
                }
            }
            Some("response.completed") => {
                self.completed = true;
                if let Some(response_id) = responses_websocket_bridge_response_id(event) {
                    self.response_id = Some(response_id.to_string());
                }
                if let Some(output) = event
                    .pointer("/response/output")
                    .and_then(serde_json::Value::as_array)
                    .filter(|output| !output.is_empty())
                {
                    for (index, item) in output.iter().cloned().enumerate() {
                        if let Ok(index) = u64::try_from(index) {
                            self.indexed_output.insert(index, item);
                        } else if !self.unindexed_output.iter().any(|existing| {
                            responses_websocket_bridge_output_items_match(existing, &item)
                        }) {
                            self.unindexed_output.push(item);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn into_output(self) -> Vec<serde_json::Value> {
        let mut output = self.indexed_output.into_values().collect::<Vec<_>>();
        for item in self.unindexed_output {
            if !output.iter().any(|existing| {
                responses_websocket_bridge_output_items_match(existing, &item)
            }) {
                output.push(item);
            }
        }
        output
    }
}

fn responses_websocket_bridge_response_id(event: &serde_json::Value) -> Option<&str> {
    event
        .pointer("/response/id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

async fn run_responses_websocket_http_bridge(
    mut client_sender: futures_util::stream::SplitSink<warp::ws::WebSocket, warp::ws::Message>,
    mut client_receiver: futures_util::stream::SplitStream<warp::ws::WebSocket>,
    client_headers: warp::http::HeaderMap,
    shared: Arc<ProxyShared>,
    first_value: serde_json::Value,
    first_activity_guard: Option<crate::update_activity::UpdateActivityGuard>,
) {
    let mut bridge = ResponsesWebsocketBridgeState::default();
    let mut next_request = Some(first_value);
    let mut first_activity_guard = first_activity_guard;
    loop {
        let value = if let Some(value) = next_request.take() {
            value
        } else {
            let message = tokio::select! {
                _ = crate::update_activity::wait_for_update_drain() => {
                    let _ = client_sender
                        .send(warp::ws::Message::close_with(1012u16, "client update"))
                        .await;
                    return;
                }
                message = client_receiver.next() => message,
            };
            let Some(Ok(message)) = message else {
                return;
            };
            if message.is_ping() {
                if client_sender
                    .send(warp::ws::Message::pong(message.as_bytes().to_vec()))
                    .await
                    .is_err()
                {
                    return;
                }
                continue;
            }
            if message.is_pong() {
                continue;
            }
            if message.is_close() {
                close_client_websocket_sender(&mut client_sender).await;
                return;
            }
            let text = match client_response_create_text(&message) {
                Ok(text) => text,
                Err((code, message)) => {
                    if send_responses_websocket_error(&mut client_sender, code, message)
                        .await
                        .is_err()
                    {
                        return;
                    }
                    continue;
                }
            };
            match validate_response_create_frame(text) {
                Ok(value) => value,
                Err((code, message)) => {
                    if send_responses_websocket_error(&mut client_sender, code, message)
                        .await
                        .is_err()
                    {
                        return;
                    }
                    continue;
                }
            }
        };

        let _turn_activity_guard = match first_activity_guard.take() {
            Some(activity_guard) => activity_guard,
            None => {
                let Some(activity_guard) = crate::update_activity::try_begin_proxy_request() else {
                    let _ = client_sender
                        .send(warp::ws::Message::close_with(1012u16, "client update"))
                        .await;
                    return;
                };
                activity_guard
            }
        };

        let request = match bridge.prepare(value) {
            Ok(request) => request,
            Err((code, message)) => {
                log::warn!(
                    "[const-api][websocket] HTTP bridge rejected response.create request_source={} code={} reason={}",
                    websocket_request_source(&client_headers),
                    code,
                    message,
                );
                if send_responses_websocket_error(&mut client_sender, code, message)
                    .await
                    .is_err()
                {
                    return;
                }
                continue;
            }
        };
        match request {
            ResponsesWebsocketBridgeRequest::Warmup { response_id } => {
                for event in [
                    serde_json::json!({
                        "type": "response.created",
                        "response": {"id": response_id.clone(), "status": "in_progress", "output": []}
                    }),
                    serde_json::json!({
                        "type": "response.completed",
                        "response": {"id": response_id, "status": "completed", "output": []}
                    }),
                ] {
                    if client_sender
                        .send(warp::ws::Message::text(event.to_string()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }
            ResponsesWebsocketBridgeRequest::Http {
                body,
                submitted_input,
                reusable_history,
            } => {
                let config = shared.config_snapshot();
                let response = forward_with_failover_with_query_presence(
                    warp::http::Method::POST,
                    "/v1/responses",
                    "",
                    false,
                    responses_websocket_bridge_headers(&client_headers),
                    body,
                    &config,
                    &shared,
                    true,
                )
                .await;
                let response = match response {
                    Ok(response) => response,
                    Err(error) => {
                        if send_responses_websocket_status_error(
                            &mut client_sender,
                            StatusCode::BAD_GATEWAY.as_u16(),
                            "http_bridge_failed",
                            &error.to_string(),
                        )
                        .await
                        .is_err()
                        {
                            return;
                        }
                        continue;
                    }
                };
                let Some((capture, pending_request)) = forward_responses_websocket_bridge_response(
                    response,
                    &mut client_sender,
                    &mut client_receiver,
                )
                .await
                else {
                    return;
                };
                bridge.commit(submitted_input, reusable_history, capture);
                next_request = pending_request;
            }
        }
    }
}

fn responses_websocket_bridge_headers(
    client_headers: &warp::http::HeaderMap,
) -> warp::http::HeaderMap {
    let mut headers = client_headers.clone();
    for name in [
        "connection",
        "upgrade",
        "sec-websocket-key",
        "sec-websocket-version",
        "sec-websocket-extensions",
        "sec-websocket-protocol",
        "openai-beta",
        "content-length",
    ] {
        headers.remove(name);
    }
    headers.insert(
        reqwest::header::ACCEPT,
        reqwest::header::HeaderValue::from_static("text/event-stream"),
    );
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    headers
}

async fn forward_responses_websocket_bridge_response(
    response: warp::reply::Response,
    client_sender: &mut futures_util::stream::SplitSink<warp::ws::WebSocket, warp::ws::Message>,
    client_receiver: &mut futures_util::stream::SplitStream<warp::ws::WebSocket>,
) -> Option<(ResponsesWebsocketBridgeCapture, Option<serde_json::Value>)> {
    let status = response.status();
    if !status.is_success() {
        let body = response.into_body().collect().await.ok()?.to_bytes();
        let message = serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|value| {
                value
                    .pointer("/error/message")
                    .or_else(|| value.get("detail"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| String::from_utf8_lossy(&body).to_string());
        send_responses_websocket_status_error(
            client_sender,
            status.as_u16(),
            "http_bridge_upstream_error",
            &message,
        )
        .await
        .ok()?;
        return Some((ResponsesWebsocketBridgeCapture::default(), None));
    }

    let mut stream = response.into_body().into_data_stream();
    let mut decoder = crate::protocol::stream::SseDecoder::default();
    let mut capture = ResponsesWebsocketBridgeCapture::default();
    let mut saw_event = false;
    let mut buffered = Vec::new();
    loop {
        let next = tokio::select! {
            message = client_receiver.next() => {
                match message {
                    Some(Ok(message)) if message.is_ping() => {
                        if client_sender
                            .send(warp::ws::Message::pong(message.as_bytes().to_vec()))
                            .await
                            .is_err()
                        {
                            return None;
                        }
                        continue;
                    }
                    Some(Ok(message)) if message.is_pong() => continue,
                    Some(Ok(message)) if message.is_close() => return None,
                    None | Some(Err(_)) => return None,
                    Some(Ok(_)) if !capture.terminal => {
                        if send_responses_websocket_error(
                            client_sender,
                            "request_in_progress",
                            "only one bridged response.create request may run at a time",
                        )
                        .await
                        .is_err()
                        {
                            return None;
                        }
                        continue;
                    }
                    Some(Ok(message)) => {
                        let text = match client_response_create_text(&message) {
                            Ok(text) => text,
                            Err((code, message)) => {
                                if send_responses_websocket_error(client_sender, code, message)
                                    .await
                                    .is_err()
                                {
                                    return None;
                                }
                                continue;
                            }
                        };
                        let value = match validate_response_create_frame(text) {
                            Ok(value) => value,
                            Err((code, message)) => {
                                if send_responses_websocket_error(client_sender, code, message)
                                    .await
                                    .is_err()
                                {
                                    return None;
                                }
                                continue;
                            }
                        };
                        return Some((capture, Some(value)));
                    }
                }
            }
            chunk = stream.next() => chunk,
        };
        let Some(chunk) = next else {
            break;
        };
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                let _ = send_responses_websocket_status_error(
                    client_sender,
                    StatusCode::BAD_GATEWAY.as_u16(),
                    "http_bridge_stream_failed",
                    &error.to_string(),
                )
                .await;
                return Some((ResponsesWebsocketBridgeCapture::default(), None));
            }
        };
        if !saw_event && buffered.len().saturating_add(chunk.len()) <= RESPONSES_WEBSOCKET_MAX_MESSAGE_BYTES {
            buffered.extend_from_slice(&chunk);
        } else if !saw_event {
            let _ = send_responses_websocket_status_error(
                client_sender,
                StatusCode::BAD_GATEWAY.as_u16(),
                "http_bridge_response_too_large",
                "the buffered HTTP response exceeded the WebSocket message limit",
            )
            .await;
            return Some((ResponsesWebsocketBridgeCapture::default(), None));
        }
        let frames = match decoder.push(&chunk) {
            Ok(frames) => frames,
            Err(error) => {
                let _ = send_responses_websocket_status_error(
                    client_sender,
                    StatusCode::BAD_GATEWAY.as_u16(),
                    error.code(),
                    &error.to_string(),
                )
                .await;
                return Some((ResponsesWebsocketBridgeCapture::default(), None));
            }
        };
        if !frames.is_empty() {
            saw_event = true;
            buffered.clear();
        }
        for frame in frames {
            if !forward_responses_websocket_sse_frame(frame, client_sender, &mut capture).await {
                return None;
            }
        }
    }
    let tail = match decoder.finish() {
        Ok(frames) => frames,
        Err(error) => {
            let _ = send_responses_websocket_status_error(
                client_sender,
                StatusCode::BAD_GATEWAY.as_u16(),
                error.code(),
                &error.to_string(),
            )
            .await;
            return Some((ResponsesWebsocketBridgeCapture::default(), None));
        }
    };
    if !tail.is_empty() {
        saw_event = true;
    }
    for frame in tail {
        if !forward_responses_websocket_sse_frame(frame, client_sender, &mut capture).await {
            return None;
        }
    }
    if !saw_event && !buffered.is_empty() {
        let mut value = serde_json::from_slice::<serde_json::Value>(&buffered).ok()?;
        let response_id = value
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("resp_const_bridge_{:032x}", rand::random::<u128>()));
        if let Some(object) = value.as_object_mut() {
            object
                .entry("id".to_string())
                .or_insert_with(|| serde_json::Value::String(response_id.clone()));
        }
        for event in [
            serde_json::json!({"type":"response.created","response":{"id":response_id.clone()}}),
            serde_json::json!({"type":"response.completed","response":value}),
        ] {
            capture.observe(&event);
            if client_sender
                .send(warp::ws::Message::text(event.to_string()))
                .await
                .is_err()
            {
                return None;
            }
        }
    }
    Some((capture, None))
}

async fn forward_responses_websocket_sse_frame(
    frame: crate::protocol::stream::SseFrame,
    client_sender: &mut futures_util::stream::SplitSink<warp::ws::WebSocket, warp::ws::Message>,
    capture: &mut ResponsesWebsocketBridgeCapture,
) -> bool {
    if frame.data.trim().is_empty() || frame.data.trim() == "[DONE]" {
        return true;
    }
    if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&frame.data) {
        if value.get("type").is_none() {
            if let (Some(object), Some(event_type)) = (
                value.as_object_mut(),
                frame.event.as_deref().map(str::trim).filter(|value| !value.is_empty()),
            ) {
                object.insert(
                    "type".to_string(),
                    serde_json::Value::String(event_type.to_string()),
                );
            }
        }
        capture.observe(&value);
    }
    client_sender
        .send(warp::ws::Message::text(frame.data))
        .await
        .is_ok()
}

async fn send_responses_websocket_error(
    sender: &mut futures_util::stream::SplitSink<warp::ws::WebSocket, warp::ws::Message>,
    code: &str,
    message: &str,
) -> std::result::Result<(), warp::Error> {
    send_responses_websocket_status_error(sender, 400, code, message).await
}

async fn send_responses_websocket_status_error(
    sender: &mut futures_util::stream::SplitSink<warp::ws::WebSocket, warp::ws::Message>,
    status: u16,
    code: &str,
    message: &str,
) -> std::result::Result<(), warp::Error> {
    sender
        .send(warp::ws::Message::text(
            serde_json::json!({
                "type": "error",
                "status": status,
                "error": {
                    "type": "invalid_request_error",
                    "code": code,
                    "message": message
                }
            })
            .to_string(),
        ))
        .await
}

fn responses_websocket_wire_body<'a>(
    channel: Option<&ChannelConfig>,
    body: &'a str,
) -> std::borrow::Cow<'a, str> {
    use std::borrow::Cow;
    let Some(channel) = channel else {
        return Cow::Borrowed(body);
    };
    let Some(model) = crate::supplier::top_level_json_string(body.as_bytes(), "model") else {
        return Cow::Borrowed(body);
    };
    let upstream = channel_upstream_model_for_request(channel, Some(&model));
    let upstream = crate::openrouter::upstream_model(channel, &upstream);
    if upstream == model {
        return Cow::Borrowed(body);
    }
    // Only the top-level model changes: continuation IDs, tool state and cache
    // prefixes retain their original bytes on every turn of a reused socket.
    Cow::Owned(crate::supplier::rewrite_supplier_body_model(body, &upstream))
}

fn responses_websocket_url(http_url: &str) -> Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(http_url)?;
    let websocket_scheme = match url.scheme() {
        "http" => "ws",
        "https" => "wss",
        _ => return Err(anyhow!("Responses WebSocket requires an HTTP(S) URL")),
    };
    url.set_scheme(websocket_scheme)
        .map_err(|_| anyhow!("cannot convert Responses URL to WebSocket"))?;
    Ok(url)
}

pub(crate) fn responses_websocket_upstream_request(
    channel: &ChannelConfig,
    target: &crate::channel_surface::ChannelSurfaceTarget,
    client_headers: &warp::http::HeaderMap,
) -> Result<tokio_tungstenite::tungstenite::http::Request<()>> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let http_url = crate::supplier::join_upstream_url(&target.base_url, "/v1/responses");
    let url = responses_websocket_url(&http_url)?;
    let mut request = url.as_str().into_client_request()?;
    for (name, value) in client_headers {
        let lower = name.as_str().to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "authorization"
                | "host"
                | "connection"
                | "upgrade"
                | "proxy-authorization"
                | "api-key"
                | "x-api-key"
                | "x-goog-api-key"
                | "cookie"
                | "sec-websocket-key"
                | "sec-websocket-version"
                | "sec-websocket-extensions"
                | "sec-websocket-protocol"
        ) || lower.starts_with("x-const-")
        {
            continue;
        }
        request.headers_mut().append(name, value.clone());
    }
    crate::channel_user_agent::apply_to_headers(channel, request.headers_mut());
    let mut auth_headers = reqwest::header::HeaderMap::new();
    crate::channel_executor::apply_upstream_auth(&mut auth_headers, channel, target, true)?;
    for (name, value) in auth_headers {
        if let Some(name) = name {
            request.headers_mut().insert(name, value);
        }
    }
    if channel.source_driver() == crate::source_driver::SourceDriverId::OpenAiApi
        && !request.headers().contains_key("openai-beta")
    {
        request.headers_mut().insert(
            "openai-beta",
            reqwest::header::HeaderValue::from_static(RESPONSES_WEBSOCKET_BETA),
        );
    }
    Ok(request)
}

fn openai_subscription_websocket_request(
    responses_url: &str,
    token: &str,
    credential: &serde_json::Value,
    client_headers: &warp::http::HeaderMap,
) -> Result<tokio_tungstenite::tungstenite::http::Request<()>> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let url = responses_websocket_url(responses_url)?;
    let mut request = url.as_str().into_client_request()?;
    let identity = active_codex_identity();
    let context_headers = codex_model_request_context_headers(Some(client_headers), &identity);
    for (name, value) in context_headers {
        if let Some(name) = name {
            request.headers_mut().insert(name, value);
        }
    }
    request.headers_mut().insert(
        reqwest::header::AUTHORIZATION,
        reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))?,
    );
    let beta = client_headers
        .get("openai-beta")
        .filter(|value| value.to_str().is_ok())
        .cloned()
        .unwrap_or_else(|| {
            reqwest::header::HeaderValue::from_static(RESPONSES_WEBSOCKET_BETA)
        });
    request.headers_mut().insert("openai-beta", beta);
    if let Some(account_id) = json_string(credential, &["account_id"])
        .or_else(|| json_string(credential, &["accountID"]))
        .or_else(|| json_string(credential, &["chatgpt_account_id"]))
        .filter(|value| !value.trim().is_empty())
    {
        request.headers_mut().insert(
            "chatgpt-account-id",
            reqwest::header::HeaderValue::from_str(&account_id)?,
        );
    }
    Ok(request)
}

fn openai_subscription_live_websocket_request(
    call_id: Option<&str>,
    raw_query: &str,
    token: &str,
    credential: &serde_json::Value,
    client_headers: &warp::http::HeaderMap,
) -> Result<tokio_tungstenite::tungstenite::http::Request<()>> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let mut url = reqwest::Url::parse(OPENAI_REALTIME_LIVE_BASE_URL)?;
    if let Some(call_id) = call_id {
        if call_id.trim().is_empty() || matches!(call_id, "." | "..") || call_id.contains('/') {
            return Err(anyhow!("invalid Codex realtime call id"));
        }
        url.path_segments_mut()
            .map_err(|_| anyhow!("invalid OpenAI realtime sideband base URL"))?
            .pop_if_empty()
            .push(call_id);
    }
    url.set_query((!raw_query.is_empty()).then_some(raw_query));
    if call_id.is_none() && websocket_query_parameter(raw_query, "model").is_none() {
        url.query_pairs_mut()
            .append_pair("model", OPENAI_SUBSCRIPTION_REALTIME_MODEL);
    }
    url.set_scheme("wss")
        .map_err(|_| anyhow!("cannot convert OpenAI realtime URL to WebSocket"))?;
    let mut request = url.as_str().into_client_request()?;
    let context_headers = codex_realtime_request_context_headers(client_headers);
    for (name, value) in context_headers {
        if let Some(name) = name {
            request.headers_mut().insert(name, value);
        }
    }
    request.headers_mut().insert(
        reqwest::header::AUTHORIZATION,
        reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))?,
    );
    if let Some(account_id) = json_string(credential, &["account_id"])
        .or_else(|| json_string(credential, &["accountID"]))
        .or_else(|| json_string(credential, &["chatgpt_account_id"]))
        .filter(|value| !value.trim().is_empty())
    {
        request.headers_mut().insert(
            "chatgpt-account-id",
            reqwest::header::HeaderValue::from_str(&account_id)?,
        );
    }
    Ok(request)
}

fn responses_websocket_resource_owners(
    text: &str,
    channel_id: &str,
    route_model: &str,
) -> Vec<LocalResourceOwner> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let mut resources = Vec::new();
    if let Some(id) = value
        .pointer("/response/id")
        .and_then(local_resource_id_from_json)
    {
        resources.push(("response", id));
    }
    if let Some(id) = value
        .pointer("/response/conversation")
        .and_then(local_resource_id_from_json)
    {
        resources.push(("conversation", id));
    }
    resources
        .into_iter()
        .filter_map(|(resource_type, resource_id)| {
            local_resource_owner_from_identity(
                crate::surface::ApiSurface::OpenAi,
                resource_type,
                resource_id,
                channel_id,
                route_model,
            )
        })
        .collect()
}

#[cfg(test)]
mod responses_websocket_tests {
    use super::*;

    #[test]
    fn interrupt_is_a_control_event_not_a_create_request() {
        let frame = r#"{"type":"response.interrupt","response_id":"resp-1","mode":"discard_partial_items","future":true}"#;
        assert!(is_responses_interrupt_frame(frame));
        assert!(validate_response_create_frame(frame).is_err(), "first frame must still be create");
        assert!(!is_responses_interrupt_frame(r#"{"type":"response.create","input":[]}"#));
        assert!(!is_responses_interrupt_frame("not JSON"));
    }

    #[tokio::test]
    async fn websocket_upstream_connects_have_a_hard_deadline() {
        let pending = std::future::pending::<std::result::Result<(), ()>>();
        let started = std::time::Instant::now();
        let result = bounded_websocket_connect(pending, Duration::from_millis(5)).await;
        assert!(result.is_none());
        assert!(started.elapsed() < Duration::from_secs(1));

        let connected = bounded_websocket_connect(
            std::future::ready(Ok::<_, ()>("connected")),
            Duration::from_secs(1),
        )
        .await;
        assert_eq!(connected, Some(Ok("connected")));
    }

    #[tokio::test]
    async fn subscription_websocket_mock_preserves_response_create_frames() {
        const FRAME: &str = r#"{ "type":"response.create","model":"gpt-test","input":[{"type":"additional_tools","role":"developer","tools":[{"type":"future_tool","opaque":true}]}],"stream":true,"future":{"keep":9007199254740993123456789} }"#;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("mock listener");
        let address = listener.local_addr().expect("mock address");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("mock accept");
            let mut socket = tokio_tungstenite::accept_async(stream)
                .await
                .expect("mock WebSocket upgrade");
            let message = socket
                .next()
                .await
                .expect("request frame")
                .expect("valid request frame");
            assert_eq!(
                message.into_text().expect("text frame"),
                FRAME
            );
            socket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    r#"{"type":"response.completed","response":{"id":"resp-mock"}}"#
                        .into(),
                ))
                .await
                .expect("mock response");
        });

        let request = openai_subscription_websocket_request(
            &format!("http://{address}/backend-api/codex/responses"),
            "subscription-token",
            &serde_json::json!({}),
            &warp::http::HeaderMap::new(),
        )
        .expect("mock request");
        let (mut socket, _) = tokio_tungstenite::connect_async(request)
            .await
            .expect("mock connect");
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                FRAME.into(),
            ))
            .await
            .expect("send request");
        let response = socket
            .next()
            .await
            .expect("response frame")
            .expect("valid response frame");
        assert!(response
            .into_text()
            .expect("text response")
            .contains("resp-mock"));
        server.await.expect("mock server");
    }

    #[test]
    fn websocket_frame_validation_preserves_unknown_fields_and_rejects_http_flags() {
        let raw = r#"{"type":"response.create","model":"gpt-test","input":[],"future":{"x":1}}"#;
        let value = validate_response_create_frame(raw).expect("valid response.create");
        assert_eq!(value["future"]["x"], 1);
        assert!(validate_response_create_frame(
            r#"{"type":"response.create","model":"gpt-test","input":[],"stream":true}"#
        )
        .is_ok());
        assert!(validate_response_create_frame(
            r#"{"type":"response.create","model":"gpt-test","input":[],"stream":false}"#
        )
        .is_err());
        assert!(validate_response_create_frame(
            r#"{"type":"response.create","model":"gpt-test","background":false}"#
        )
        .is_ok());
    }

    #[test]
    fn native_subscription_websocket_terminal_events_preserve_health_semantics() {
        let config = default_supplier_config();
        let request = || {
            NativeSubscriptionWebsocketRequest::start_at(
                &config,
                r#"{"type":"response.create","model":"gpt-test","input":[],"stream":true}"#,
                std::time::Instant::now(),
            )
            .expect("request state")
        };

        let mut completed = request();
        assert_eq!(
            completed.observe(r#"{"type":"response.completed","response":{"id":"resp-1"}}"#),
            Some(200)
        );

        let mut incomplete = request();
        assert_eq!(
            incomplete.observe(
                r#"{"type":"response.incomplete","response":{"id":"resp-2","status":"incomplete"}}"#
            ),
            Some(200)
        );

        let mut cancelled = request();
        assert_eq!(
            cancelled.observe(r#"{"type":"response.cancelled","response":{"id":"resp-3"}}"#),
            Some(499)
        );

        let mut failed = request();
        assert_eq!(
            failed.observe(r#"{"type":"response.failed","response":{"id":"resp-4"}}"#),
            Some(502)
        );
    }

    #[test]
    fn websocket_terminal_errors_preserve_provider_diagnostics_without_raw_text() {
        let terminal = responses_websocket_terminal_metadata(
            r#"{"type":"error","request_id":"req_test_123456789","retry_after_seconds":2.2,"error":{"type":"server_error","code":"server_is_overloaded","param":"model"}}"#,
        )
        .expect("terminal metadata");
        assert_eq!(terminal.status, 503);
        assert_eq!(terminal.event_type, "error");
        assert_eq!(terminal.error_type.as_deref(), Some("server_error"));
        assert_eq!(terminal.error_code.as_deref(), Some("server_is_overloaded"));
        assert_eq!(terminal.error_param.as_deref(), Some("model"));
        assert_eq!(
            terminal.provider_request_id.as_deref(),
            Some("req_test_123456789")
        );
        assert_eq!(terminal.retry_after_seconds, Some(3));

        let mut payload = serde_json::Map::new();
        attach_responses_websocket_terminal_metadata(&mut payload, &terminal);
        assert_eq!(payload["error_kind"], "server_is_overloaded");
        assert_eq!(payload["failure_scope"], "request");
        assert_eq!(payload["provider_event_type"], "error");
        assert!(payload.get("message").is_none());
    }

    #[test]
    fn websocket_terminal_observation_keeps_wire_bytes_and_retry_hint() {
        let text = r#"{"type":"response.failed","response":{"error":{"type":"server_error","code":"rate_limit_exceeded","message":"slow down"}}}"#;
        let mut payload = serde_json::json!({"data":text,"retry_after_seconds":3}).as_object().unwrap().clone();
        attach_responses_websocket_failure_observation(&mut payload, text, Some("gpt-5.6-luna"));
        assert_eq!(payload["data"], text);
        assert_eq!(payload["failure_observation_v1"]["cause"], "rate_limited");
        assert_eq!(payload["failure_observation_v1"]["retry_after_seconds"], 3);
        assert_eq!(payload["failure_scope"], "request");
        assert_eq!(payload["retry_hint_source"], "provider_event");
        assert_eq!(payload["model_failure_soft"], true);
    }

    #[test]
    fn missing_replay_uses_codex_full_request_recovery_without_channel_penalty() {
        let error = anyhow::Error::new(ResponsesWsContinuationConnectionLost)
            .context("Responses WebSocket continuation replay context is unavailable");
        let payload = supplier_responses_send_error_payload(&error);
        assert_eq!(payload["status"], 400);
        assert_eq!(payload["failure_scope"], "request");
        assert_eq!(payload["request_sent"], false);
        assert_eq!(payload["failure_stage"], "continuation_replay");
        let wire: serde_json::Value = serde_json::from_str(payload["data"].as_str().unwrap()).unwrap();
        assert_eq!(wire["error"]["code"], "previous_response_not_found");
        assert_eq!(wire["error"]["param"], "previous_response_id");
        // A failed write can be ambiguous; never turn it into an automatic
        // full-request retry merely because its message resembles a cache miss.
        let network = supplier_responses_send_error_payload(&anyhow!("connection reset without closing handshake"));
        assert_eq!(network["status"], 502);
        assert_eq!(network["failure_scope"], "channel");
        assert!(network.get("request_sent").is_none());
    }

    #[test]
    fn closed_physical_websocket_does_not_count_while_session_keeps_diagnostics() {
        let route = "pool:test-closed-snapshot";
        let mut lease = UpstreamWebsocketLease::open("pool", "test-closed-snapshot", "codex");
        lease.mark_turn();
        lease.set_close_reason("pool_retired_idle");
        lease.close();
        lease.set_close_reason("later_session_error");
        lease.mark_turn(); // a stale session cannot reopen physical counters
        let mut payload = serde_json::Map::new();
        lease.attach(&mut payload);
        assert_eq!(payload["upstream_connection_closed"], true);
        assert_eq!(payload["upstream_close_reason"], "pool_retired_idle");
        let counts = upstream_websocket_counts().lock().unwrap();
        assert_eq!(counts.by_route.get(route).copied().unwrap_or(0), 0);
        drop(counts);
        lease.close(); // also safe when Drop runs after the socket actor
    }

    #[test]
    fn websocket_slow_down_and_context_errors_are_not_generic_502s() {
        let slow_down = responses_websocket_terminal_metadata(
            r#"{"type":"error","error":{"code":"slow_down"}}"#,
        )
        .expect("slow down terminal");
        assert_eq!(slow_down.status, 429);

        let context = responses_websocket_terminal_metadata(
            r#"{"type":"error","error":{"code":"context_length_exceeded"}}"#,
        )
        .expect("context terminal");
        assert_eq!(context.status, 400);
    }

    #[test]
    fn websocket_wrapped_error_headers_preserve_request_id_and_retry_after() {
        let terminal = responses_websocket_terminal_metadata(
            r#"{"type":"error","status_code":429,"headers":{"X-Request-Id":"req_header_123","Retry-After":"1.2"},"error":{"type":"rate_limit_error","code":"slow_down"}}"#,
        )
        .expect("wrapped terminal metadata");
        assert_eq!(terminal.status, 429);
        assert_eq!(terminal.provider_request_id.as_deref(), Some("req_header_123"));
        assert_eq!(terminal.retry_after_seconds, Some(2));
    }

    #[test]
    fn websocket_wrapped_body_error_preserves_connection_limit_diagnostics() {
        let terminal = responses_websocket_terminal_metadata(
            r#"{"type":"error","status":429,"body":{"request_id":"req_body_123","error":{"type":"server_error","code":"websocket_connection_limit_reached"}},"headers":{"Retry-After":"1"}}"#,
        )
        .expect("wrapped body terminal metadata");
        assert_eq!(terminal.status, 429);
        assert_eq!(terminal.error_type.as_deref(), Some("server_error"));
        assert_eq!(
            terminal.error_code.as_deref(),
            Some("websocket_connection_limit_reached")
        );
        assert_eq!(terminal.provider_request_id.as_deref(), Some("req_body_123"));
        assert_eq!(terminal.retry_after_seconds, Some(1));
    }

    #[test]
    fn websocket_handshake_error_preserves_safe_http_diagnostics() {
        let response = tokio_tungstenite::tungstenite::http::Response::builder()
            .status(429)
            .header("x-request-id", "req_handshake_123")
            .header("retry-after", "3")
            .body(Some(
                br#"{"error":{"type":"rate_limit_error","code":"rate_limit_exceeded","message":"not persisted"}}"#
                    .to_vec(),
            ))
            .expect("handshake response");
        let error = anyhow!(tokio_tungstenite::tungstenite::Error::Http(Box::new(
            response
        )));
        let metadata = responses_websocket_connect_error_metadata(&error);
        assert_eq!(metadata.status, 429);
        assert_eq!(metadata.error_type.as_deref(), Some("rate_limit_error"));
        assert_eq!(metadata.error_code.as_deref(), Some("rate_limit_exceeded"));
        assert_eq!(metadata.provider_request_id.as_deref(), Some("req_handshake_123"));
        assert_eq!(metadata.retry_after_seconds, Some(3));
    }

    #[test]
    fn websocket_connection_counts_distinguish_idle_from_active_turns() {
        let route_key = "openai_responses:test-counts-channel";
        let mut lease = UpstreamWebsocketLease::open(
            "openai_responses",
            "test-counts-channel",
            "test_source",
        );
        let mut payload = serde_json::Map::new();
        lease.attach(&mut payload);
        assert_eq!(payload["upstream_open_connections"], 1);
        assert_eq!(payload["upstream_active_turns"], 0);

        lease.mark_turn();
        lease.attach(&mut payload);
        assert_eq!(payload["upstream_active_turns"], 1);
        lease.finish_turn();
        lease.attach(&mut payload);
        assert_eq!(payload["upstream_active_turns"], 0);
        drop(lease);

        let mut closed = serde_json::Map::new();
        attach_upstream_websocket_count_snapshot(&mut closed, route_key);
        assert_eq!(closed["upstream_open_connections"], 0);
    }

    #[test]
    fn http_bridge_can_accept_the_next_turn_after_a_terminal_event() {
        let mut capture = ResponsesWebsocketBridgeCapture::default();
        capture.observe(&serde_json::json!({
            "type": "response.completed",
            "response": {"id": "resp-1", "output": []}
        }));
        assert!(capture.terminal);
        assert!(capture.completed);
        assert_eq!(capture.response_id.as_deref(), Some("resp-1"));
    }

    #[test]
    fn http_bridge_keeps_the_created_id_when_completed_is_minimal() {
        let mut capture = ResponsesWebsocketBridgeCapture::default();
        capture.observe(&serde_json::json!({
            "type": "response.created",
            "response": {"id": "resp-created"}
        }));
        capture.observe(&serde_json::json!({
            "type": "response.completed",
            "response": {"status": "completed", "output": []}
        }));

        assert!(capture.completed);
        assert_eq!(capture.response_id.as_deref(), Some("resp-created"));
    }

    #[test]
    fn websocket_resource_capture_finds_response_and_conversation() {
        let owners = responses_websocket_resource_owners(
            r#"{"type":"response.created","response":{"id":"resp_ws","conversation":{"id":"conv_ws"}}}"#,
            "channel-ws",
            "gpt-test",
        );
        assert_eq!(owners.len(), 2);
        assert!(owners.iter().all(|owner| owner.channel_id == "channel-ws"));
    }

    #[test]
    fn websocket_http_bridge_expands_warmup_and_follow_up_history() {
        assert_eq!(
            responses_websocket_input_items(Some(&serde_json::json!("hello"))).unwrap(),
            vec![serde_json::json!({"role":"user","content":"hello"})]
        );
        let mut state = ResponsesWebsocketBridgeState::default();
        let warmup = state
            .prepare(serde_json::json!({
                "type": "response.create",
                "model": "gpt-test",
                "input": [{"role":"user","content":"initial"}],
                "stream": true,
                "generate": false
            }))
            .expect("warmup request");
        let response_id = match warmup {
            ResponsesWebsocketBridgeRequest::Warmup { response_id } => response_id,
            _ => panic!("expected warmup"),
        };

        let first = state
            .prepare(serde_json::json!({
                "type": "response.create",
                "model": "gpt-test",
                "previous_response_id": response_id,
                "input": [{"role":"user","content":"question"}],
                "stream": true
            }))
            .expect("first bridged request");
        let (first_body, submitted, reusable) = match first {
            ResponsesWebsocketBridgeRequest::Http {
                body,
                submitted_input,
                reusable_history,
            } => (body, submitted_input, reusable_history),
            _ => panic!("expected HTTP bridge"),
        };
        let first_body: serde_json::Value = serde_json::from_slice(&first_body).unwrap();
        assert!(first_body.get("type").is_none());
        assert!(first_body.get("generate").is_none());
        assert!(first_body.get("previous_response_id").is_none());
        assert_eq!(first_body["input"].as_array().unwrap().len(), 2);
        assert!(reusable);

        let mut capture = ResponsesWebsocketBridgeCapture::default();
        capture.observe(&serde_json::json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                    "type":"message",
                    "role":"assistant",
                    "content":[{"type":"output_text","text":"answer"}]
            }
        }));
        capture.observe(&serde_json::json!({
            "type": "response.completed",
            "response": {"id": "resp-real-1", "output": []}
        }));
        state.commit(submitted, reusable, capture);
        let follow_up = state
            .prepare(serde_json::json!({
                "type": "response.create",
                "model": "gpt-test",
                "previous_response_id": "resp-real-1",
                "input": [{"role":"user","content":"follow up"}],
                "stream": true
            }))
            .expect("follow-up request");
        let ResponsesWebsocketBridgeRequest::Http { body, .. } = follow_up else {
            panic!("expected HTTP bridge")
        };
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["input"].as_array().unwrap().len(), 4);
        assert!(body.get("previous_response_id").is_none());
    }

    #[test]
    fn websocket_http_bridge_preserves_custom_tool_call_when_terminal_output_is_empty() {
        let mut state = ResponsesWebsocketBridgeState::default();
        let first = state
            .prepare(serde_json::json!({
                "type": "response.create",
                "model": "gpt-test",
                "input": [{"role":"user","content":"inspect the workspace"}],
                "stream": true
            }))
            .expect("first bridged request");
        let (submitted, reusable) = match first {
            ResponsesWebsocketBridgeRequest::Http {
                submitted_input,
                reusable_history,
                ..
            } => (submitted_input, reusable_history),
            _ => panic!("expected HTTP bridge"),
        };

        let mut capture = ResponsesWebsocketBridgeCapture::default();
        capture.observe(&serde_json::json!({
            "type": "response.output_item.done",
            "item": {
                "type": "custom_tool_call",
                "call_id": "call-bridge",
                "name": "exec",
                "input": "text('ok')"
            }
        }));
        capture.observe(&serde_json::json!({
            "type": "response.completed",
            "response": {"id": "resp-tool", "output": []}
        }));
        state.commit(submitted, reusable, capture);

        let follow_up = state
            .prepare(serde_json::json!({
                "type": "response.create",
                "model": "gpt-test",
                "previous_response_id": "resp-tool",
                "input": [{
                    "type": "custom_tool_call_output",
                    "call_id": "call-bridge",
                    "output": "ok"
                }],
                "stream": true
            }))
            .expect("paired custom tool continuation");
        let ResponsesWebsocketBridgeRequest::Http { body, .. } = follow_up else {
            panic!("expected HTTP bridge")
        };
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let input = body["input"].as_array().expect("expanded input");
        assert_eq!(input.len(), 3);
        assert_eq!(input[1]["type"], "custom_tool_call");
        assert_eq!(input[1]["call_id"], "call-bridge");
        assert_eq!(input[2]["type"], "custom_tool_call_output");
        assert_eq!(input[2]["call_id"], "call-bridge");
    }

    #[test]
    fn websocket_http_bridge_accepts_standard_tool_pairs_without_rewriting_them() {
        for (call_type, output_type) in [
            ("function_call", "function_call_output"),
            ("local_shell_call", "function_call_output"),
            ("custom_tool_call", "custom_tool_call_output"),
            ("shell_call", "shell_call_output"),
            ("computer_call", "computer_call_output"),
            ("apply_patch_call", "apply_patch_call_output"),
            ("tool_search_call", "tool_search_output"),
        ] {
            let input = serde_json::json!([
                {"type": call_type, "call_id": "call-standard", "action": {"commands": ["pwd"]}},
                {"type": output_type, "call_id": "call-standard", "output": [{"stdout": "ok"}]}
            ]);
            let mut state = ResponsesWebsocketBridgeState::default();
            let request = state
                .prepare(serde_json::json!({
                    "type": "response.create", "model": "gpt-test", "input": input
                }))
                .expect("standard Responses tool pair");
            let ResponsesWebsocketBridgeRequest::Http { body, .. } = request else {
                panic!("expected HTTP bridge");
            };
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["input"], input, "{call_type}");
            let items = input.as_array().unwrap();
            assert!(responses_websocket_has_orphaned_tool_output(&items[1..]));
            assert!(responses_websocket_has_orphaned_tool_output(&[
                items[1].clone(),
                items[0].clone()
            ]));
        }
        assert!(responses_websocket_has_orphaned_tool_output(&[
            serde_json::json!({"type":"shell_call","call_id":"same-id"}),
            serde_json::json!({"type":"computer_call_output","call_id":"same-id"})
        ]));
    }

    #[test]
    fn websocket_http_bridge_defers_unknown_tool_extensions_to_upstream() {
        let mut state = ResponsesWebsocketBridgeState::default();
        assert!(state
            .prepare(serde_json::json!({
                "type": "response.create", "model": "gpt-test", "input": [
                    {"type": "future_call_output", "call_id": "call-future", "output": "ok"},
                    {"type": "tool_search_output", "execution": "server", "tools": []}
                ]
            }))
            .is_ok());
    }

    #[test]
    fn websocket_http_bridge_leaves_server_side_tool_context_to_upstream() {
        for context in [
            serde_json::json!({"conversation": "conv-existing"}),
            serde_json::json!({"conversation": {"id": "conv-existing"}}),
            serde_json::json!({"previous_response_id": "resp-unseen"}),
        ] {
            let mut frame = serde_json::json!({
                "type": "response.create", "model": "gpt-test", "input": [{
                    "type": "shell_call_output", "call_id": "call-server", "output": []
                }]
            });
            frame
                .as_object_mut()
                .unwrap()
                .extend(context.as_object().unwrap().clone());
            let mut state = ResponsesWebsocketBridgeState::default();
            let request = state.prepare(frame).expect("upstream-owned history");
            let ResponsesWebsocketBridgeRequest::Http {
                body,
                reusable_history,
                ..
            } = request else {
                panic!("expected HTTP bridge");
            };
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert!(
                !reusable_history,
                "partial server history must not be locally replayed"
            );
            for (key, value) in context.as_object().unwrap() {
                assert_eq!(&body[key], value);
            }
        }
    }

    #[test]
    fn websocket_http_bridge_rejects_an_orphaned_self_contained_tool_output() {
        let mut state = ResponsesWebsocketBridgeState {
            history: vec![serde_json::json!({"role":"user","content":"question"})],
            last_response_id: Some("resp-missing-call".to_string()),
        };
        let result = state.prepare(serde_json::json!({
                "type": "response.create",
                "model": "gpt-test",
                "previous_response_id": "resp-missing-call",
                "input": [{
                    "type": "custom_tool_call_output",
                    "call_id": "call-missing",
                    "output": "result"
                }],
                "stream": true
            }));
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("orphaned tool output must fail before the upstream request"),
        };
        assert_eq!(error.0, "invalid_websocket_continuation");
    }

    #[test]
    fn websocket_route_uses_platform_when_local_priority_has_no_ready_route() {
        let mut config = default_config();
        config.account_device_api_key = "platform-device-key".to_string();
        config.prefer_local_supply = true;
        config.channels.clear();
        let shared = ProxyShared::from_config(normalize_config(config), Client::new());

        assert!(matches!(
            select_responses_websocket_route(
                &shared,
                &warp::http::HeaderMap::new(),
                br#"{"model":"gpt-test","input":[]}"#,
            ),
            ResponsesWebsocketRoute::Platform
        ));
    }

    #[test]
    fn websocket_route_keeps_http_bridge_for_a_convertible_local_channel() {
        let mut channel =
            channel_from_supplier("local-claude".to_string(), &default_supplier_config());
        channel.set_source_driver(crate::source_driver::SourceDriverId::ClaudeSubscription);
        channel.surface_bindings = crate::source_driver_surface_bindings(
            crate::source_driver::SourceDriverId::ClaudeSubscription,
            "",
        );
        channel.enabled = true;
        channel.models = vec!["gpt-test".to_string()];
        channel.public_model = "gpt-test".to_string();
        channel.upstream_model = "claude-test".to_string();

        let mut config = default_config();
        config.account_device_api_key = "platform-device-key".to_string();
        config.prefer_local_supply = true;
        config.channels = vec![channel];
        let shared = ProxyShared::from_config(normalize_config(config), Client::new());

        match select_responses_websocket_route(
            &shared,
            &warp::http::HeaderMap::new(),
            br#"{"model":"gpt-test","input":[]}"#,
        ) {
            ResponsesWebsocketRoute::HttpBridge("local_route_requires_http") => {}
            ResponsesWebsocketRoute::HttpBridge(reason) => {
                panic!("unexpected HTTP bridge reason: {reason}")
            }
            ResponsesWebsocketRoute::Platform => panic!("expected a local HTTP bridge route"),
            ResponsesWebsocketRoute::Native(_) => panic!("expected a converted local route"),
        }
    }

    #[test]
    fn websocket_route_keeps_a_native_local_subscription_when_available() {
        let mut channel =
            channel_from_supplier("local-openai".to_string(), &default_supplier_config());
        channel.set_source_driver(crate::source_driver::SourceDriverId::OpenAiSubscription);
        channel.surface_bindings = crate::source_driver_surface_bindings(
            crate::source_driver::SourceDriverId::OpenAiSubscription,
            "",
        );
        channel.enabled = true;
        channel.models = vec!["gpt-test".to_string()];
        channel.public_model = "gpt-test".to_string();
        channel.upstream_model = "gpt-test".to_string();

        let mut config = default_config();
        config.account_device_api_key = "platform-device-key".to_string();
        config.prefer_local_supply = true;
        config.channels = vec![channel];
        let shared = ProxyShared::from_config(normalize_config(config), Client::new());
        let mut headers = warp::http::HeaderMap::new();
        headers.insert("originator", "codex_vscode".parse().unwrap());
        headers.insert("user-agent", "codex_vscode/1.2.3".parse().unwrap());

        match select_responses_websocket_route(
            &shared,
            &headers,
            br#"{"model":"gpt-test","input":[]}"#,
        ) {
            ResponsesWebsocketRoute::Native(
                ResponsesWebsocketNativeRoute::OpenAiSubscription { .. },
            ) => {}
            ResponsesWebsocketRoute::Native(_) => panic!("expected an OpenAI subscription route"),
            ResponsesWebsocketRoute::HttpBridge(reason) => {
                panic!("unexpected HTTP bridge route: {reason}")
            }
            ResponsesWebsocketRoute::Platform => panic!("expected a native local route"),
        }
    }

    #[test]
    fn websocket_route_uses_platform_when_platform_priority_has_a_local_route() {
        let mut channel =
            channel_from_supplier("local-openai".to_string(), &default_supplier_config());
        channel.set_source_driver(crate::source_driver::SourceDriverId::OpenAiSubscription);
        channel.surface_bindings = crate::source_driver_surface_bindings(
            crate::source_driver::SourceDriverId::OpenAiSubscription,
            "",
        );
        channel.enabled = true;
        channel.models = vec!["gpt-test".to_string()];
        channel.public_model = "gpt-test".to_string();
        channel.upstream_model = "gpt-test".to_string();

        let mut config = default_config();
        config.account_device_api_key = "platform-device-key".to_string();
        config.prefer_local_supply = false;
        config.channels = vec![channel];
        let shared = ProxyShared::from_config(normalize_config(config), Client::new());
        let mut headers = warp::http::HeaderMap::new();
        headers.insert("originator", "codex_vscode".parse().unwrap());
        headers.insert("user-agent", "codex_vscode/1.2.3".parse().unwrap());

        assert!(matches!(
            select_responses_websocket_route(
                &shared,
                &headers,
                br#"{"model":"gpt-test","input":[]}"#,
            ),
            ResponsesWebsocketRoute::Platform
        ));
    }

    #[test]
    fn websocket_route_honors_skip_local_when_a_local_route_exists() {
        let mut channel =
            channel_from_supplier("local-openai".to_string(), &default_supplier_config());
        channel.set_source_driver(crate::source_driver::SourceDriverId::OpenAiSubscription);
        channel.surface_bindings = crate::source_driver_surface_bindings(
            crate::source_driver::SourceDriverId::OpenAiSubscription,
            "",
        );
        channel.enabled = true;
        channel.models = vec!["gpt-test".to_string()];
        channel.public_model = "gpt-test".to_string();
        channel.upstream_model = "gpt-test".to_string();

        let mut config = default_config();
        config.account_device_api_key = "platform-device-key".to_string();
        config.prefer_local_supply = true;
        config.channels = vec![channel];
        let shared = ProxyShared::from_config(normalize_config(config), Client::new());
        let mut headers = warp::http::HeaderMap::new();
        headers.insert(SKIP_LOCAL_SHORT_CIRCUIT_HEADER, "true".parse().unwrap());

        assert!(matches!(
            select_responses_websocket_route(
                &shared,
                &headers,
                br#"{"model":"gpt-test","input":[]}"#,
            ),
            ResponsesWebsocketRoute::Platform
        ));
    }

    #[test]
    fn websocket_route_uses_platform_when_the_configured_local_route_is_not_ready() {
        let mut channel =
            channel_from_supplier("local-openai".to_string(), &default_supplier_config());
        channel.set_source_driver(crate::source_driver::SourceDriverId::OpenAiSubscription);
        channel.surface_bindings = crate::source_driver_surface_bindings(
            crate::source_driver::SourceDriverId::OpenAiSubscription,
            "",
        );
        channel.enabled = true;
        channel.models = vec!["gpt-test".to_string()];
        channel.public_model = "gpt-test".to_string();
        channel.upstream_model = "gpt-test".to_string();

        let mut config = default_config();
        config.account_device_api_key = "platform-device-key".to_string();
        config.prefer_local_supply = true;
        config.channels = vec![channel.clone()];
        let shared = ProxyShared::from_config(normalize_config(config), Client::new());
        shared
            .local_channel_readiness
            .lock()
            .expect("readiness")
            .insert(channel.id, false);

        assert!(matches!(
            select_responses_websocket_route(
                &shared,
                &warp::http::HeaderMap::new(),
                br#"{"model":"gpt-test","input":[]}"#,
            ),
            ResponsesWebsocketRoute::Platform
        ));
    }

    #[test]
    fn websocket_route_keeps_http_bridge_without_platform_credentials_or_local_routes() {
        let mut config = default_config();
        config.account_device_api_key.clear();
        config.prefer_local_supply = true;
        config.channels.clear();
        let shared = ProxyShared::from_config(normalize_config(config), Client::new());

        assert!(matches!(
            select_responses_websocket_route(
                &shared,
                &warp::http::HeaderMap::new(),
                br#"{"model":"gpt-test","input":[]}"#,
            ),
            ResponsesWebsocketRoute::HttpBridge("platform_duplex_unavailable")
        ));
    }

    #[test]
    fn websocket_route_does_not_escape_an_explicit_force_local_request() {
        let mut config = default_config();
        config.account_device_api_key = "platform-device-key".to_string();
        config.channels.clear();
        let shared = ProxyShared::from_config(normalize_config(config), Client::new());
        let mut headers = warp::http::HeaderMap::new();
        headers.insert(USE_LOCAL_SHORT_CIRCUIT_HEADER, "true".parse().unwrap());

        assert!(matches!(
            select_responses_websocket_route(
                &shared,
                &headers,
                br#"{"model":"gpt-test","input":[]}"#,
            ),
            ResponsesWebsocketRoute::HttpBridge("platform_duplex_unavailable")
        ));
    }

    #[test]
    fn openai_subscription_websocket_request_uses_native_codex_identity() {
        let mut headers = warp::http::HeaderMap::new();
        headers.insert("originator", "codex_vscode".parse().unwrap());
        headers.insert("user-agent", "codex_vscode/1.2.3".parse().unwrap());
        headers.insert("session-id", "session-1".parse().unwrap());
        headers.insert(
            "openai-beta",
            "responses_websockets=2026-02-06".parse().unwrap(),
        );
        let request = openai_subscription_websocket_request(
            "http://127.0.0.1:18080/backend-api/codex/responses",
            "subscription-token",
            &serde_json::json!({"account_id":"account-1"}),
            &headers,
        )
        .expect("subscription WebSocket request");

        assert_eq!(request.uri().scheme_str(), Some("ws"));
        assert_eq!(request.headers()["authorization"], "Bearer subscription-token");
        assert_eq!(request.headers()["chatgpt-account-id"], "account-1");
        assert_eq!(request.headers()["originator"], "codex_vscode");
        assert_eq!(request.headers()["user-agent"], "codex_vscode/1.2.3");
        assert_eq!(request.headers()["version"], "1.2.3");
        assert_eq!(request.headers()["session-id"], "session-1");
        assert_eq!(
            request.headers()["openai-beta"],
            "responses_websockets=2026-02-06"
        );
        for version in ["0.120.0", "0.154.0", "0.200.0"] {
            let user_agent = format!("codex_vscode/{version}");
            headers.insert("user-agent", user_agent.parse().unwrap());
            headers.insert("version", version.parse().unwrap());
            headers.insert("openai-beta", "responses=future-v9".parse().unwrap());
            let request = openai_subscription_websocket_request(
                "http://127.0.0.1:18080/backend-api/codex/responses",
                "subscription-token", &serde_json::json!({}), &headers,
            ).unwrap();
            assert_eq!(request.headers()["user-agent"], user_agent);
            assert_eq!(request.headers()["version"], version);
            assert_eq!(request.headers()["openai-beta"], "responses=future-v9");
        }
    }

    #[test]
    fn http_surface_websocket_request_applies_the_channel_user_agent_override() {
        let mut channel =
            channel_from_supplier("http-surface".to_string(), &default_supplier_config());
        channel.v2.user_agent_profile = crate::channel_user_agent::PROFILE_OPENCODE.into();
        let target = crate::channel_surface::preferred_channel_surface_target(&channel)
            .expect("HTTP surface target");
        let mut headers = warp::http::HeaderMap::new();
        headers.insert("user-agent", "caller/7".parse().unwrap());

        let request = responses_websocket_upstream_request(&channel, &target, &headers)
            .expect("HTTP surface WebSocket request");

        assert_eq!(request.headers()["user-agent"], "opencode/1.18.31");
    }

    #[test]
    fn openai_subscription_live_sideband_uses_codex_path_and_oauth_identity() {
        let mut headers = warp::http::HeaderMap::new();
        headers.insert("originator", "codex_vscode".parse().unwrap());
        headers.insert("user-agent", "codex_vscode/1.2.3".parse().unwrap());
        headers.insert("x-oai-attestation", "attestation".parse().unwrap());
        let request = openai_subscription_live_websocket_request(
            Some("rtc_live_1"),
            "",
            "subscription-token",
            &serde_json::json!({"account_id":"account-1"}),
            &headers,
        )
        .expect("subscription Live sideband request");

        assert_eq!(request.uri().scheme_str(), Some("wss"));
        assert_eq!(request.uri().path(), "/v1/live/rtc_live_1");
        assert_eq!(request.headers()["authorization"], "Bearer subscription-token");
        assert_eq!(request.headers()["chatgpt-account-id"], "account-1");
        assert_eq!(request.headers()["openai-alpha"], "quicksilver=v2");
        assert_eq!(request.headers()["originator"], "codex_vscode");
        assert_eq!(request.headers()["x-oai-attestation"], "attestation");
        assert!(!request.headers().contains_key("openai-beta"));
    }

    #[test]
    fn openai_subscription_live_standalone_preserves_the_model_and_query() {
        let request = openai_subscription_live_websocket_request(
            None,
            "model=gpt-live-future-codex&future=1",
            "subscription-token",
            &serde_json::json!({"account_id":"account-1"}),
            &warp::http::HeaderMap::new(),
        ).expect("standalone Live request");
        assert_eq!(request.uri().path(), "/v1/live");
        assert_eq!(request.uri().query(), Some("model=gpt-live-future-codex&future=1"));
        assert_eq!(request.headers()["authorization"], "Bearer subscription-token");
        assert_eq!(request.headers()["openai-alpha"], "quicksilver=v2");
        let default = openai_subscription_live_websocket_request(
            None, "", "token", &serde_json::json!({}), &warp::http::HeaderMap::new(),
        ).expect("default Live request");
        assert_eq!(default.uri().query(), Some("model=gpt-live-1-codex"));
    }

    /// Opens and immediately closes a Voice socket. No audio, model request,
    /// token refresh, or credential write is performed.
    #[tokio::test]
    #[ignore = "requires CONST_API_LIVE_OPENAI_CREDENTIAL; real subscription Voice handshake only"]
    async fn live_codex_subscription_voice_handshake() -> Result<()> {
        let path = std::env::var("CONST_API_LIVE_OPENAI_CREDENTIAL")?;
        let credential: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
        let token = credential["access_token"].as_str()
            .ok_or_else(|| anyhow!("credential has no access token"))?;
        let request = openai_subscription_live_websocket_request(
            None, "", token, &credential, &warp::http::HeaderMap::new(),
        )?;
        let mut socket = match connect_responses_websocket_request(request).await {
            Ok(socket) => socket,
            Err(error) => return Err(anyhow!(
                "Voice handshake failed, HTTP status={:?}", responses_websocket_error_status(&error),
            )),
        };
        eprintln!("Codex Voice standalone WebSocket: HTTP 101; no audio or generation sent");
        let _ = tokio::time::timeout(WEBSOCKET_CLOSE_TIMEOUT, socket.close(None)).await;
        Ok(())
    }

    #[tokio::test]
    async fn codex_live_sideband_reuses_the_subscription_call_owner() {
        let mut config = default_config();
        config.endpoints.clear();
        let mut channel = channel_from_supplier(
            "live-subscription".to_string(),
            &default_supplier_config(),
        );
        channel.set_source_driver(crate::source_driver::SourceDriverId::OpenAiSubscription);
        channel.enabled = true;
        config.channels = vec![channel.clone()];
        let shared = ProxyShared::from_config(normalize_config(config), Client::new());
        shared
            .local_channel_readiness
            .lock()
            .expect("readiness")
            .insert(channel.id.clone(), true);
        shared
            .local_resource_owners
            .bind_many(vec![LocalResourceOwner {
                key: LocalResourceOwnerKey {
                    surface: crate::surface::ApiSurface::OpenAi,
                    resource_type: "realtime_call".to_string(),
                    resource_id: "rtc_owned".to_string(),
                },
                channel_id: channel.id.clone(),
                route_model: "gpt-live-1-codex".to_string(),
                updated_at_unix: now_unix(),
            }])
            .await
            .expect("bind Live owner");

        let selected = select_native_duplex_channel(
            &shared,
            NativeDuplexKind::OpenAiLive,
            "/v1/live/rtc_owned",
            "",
            Some(&channel.id),
        )
        .expect("select subscription owner");
        assert_eq!(selected.id, channel.id);
        assert_eq!(
            crate::channel_executor::execution_kind_for_source(selected.source_driver())
                .expect("execution kind"),
            crate::source_driver::ExecutionKind::OpenAiSubscription
        );
    }
}
