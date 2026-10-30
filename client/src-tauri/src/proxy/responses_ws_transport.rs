//! Keep the official Codex fork at the transport boundary. Pooling and relays
//! continue to use registry Tungstenite messages; public API sockets keep their
//! existing connector. Subscription Responses and Live share the fork connector,
//! with protocol-specific compression. Payload buffers move without copying.

use anyhow::{Result, anyhow};
use codex_tokio_tungstenite as codex_ws;
use codex_tungstenite as codex;
use futures_util::{Sink, Stream};
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::net::TcpStream;
use tokio_tungstenite::{self as registry_ws, tungstenite as registry};

type RegistrySocket = registry_ws::WebSocketStream<registry_ws::MaybeTlsStream<TcpStream>>;
type CodexSocket =
    codex_ws::WebSocketStream<codex_ws::MaybeTlsStream<crate::openai_transport::dialer::Socket>>;

pub(crate) struct CodexConnection {
    socket: CodexSocket,
    handshake_headers: http::HeaderMap,
    metadata: Option<serde_json::Map<String, serde_json::Value>>,
    pending: Option<registry::Message>,
}

impl std::fmt::Debug for CodexConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexConnection").finish_non_exhaustive()
    }
}

impl CodexConnection {
    fn project_handshake(&mut self, message: &registry::Message) -> Option<registry::Message> {
        let headers = self.metadata.as_mut()?;
        let registry::Message::Text(text) = message else {
            return None;
        };
        let event: serde_json::Value = serde_json::from_str(text).ok()?;
        let kind = event.get("type")?.as_str()?;
        if kind == "response.metadata" {
            // A newer upstream event wins over opening headers, including its
            // turn-state. Never replay the opening state at each new turn.
            if let Some(fresh) = event.get("headers").and_then(serde_json::Value::as_object) {
                headers.retain(|name, _| !fresh.keys().any(|key| key.eq_ignore_ascii_case(name)));
            }
            return None;
        }
        // Do not publish sticky turn-state for a rejected previous_response_id.
        // The owning relay can still recover before any semantic response starts.
        if !kind.starts_with("response.")
            || matches!(kind, "response.failed" | "response.incomplete")
        {
            return None;
        }
        let headers = self.metadata.take()?;
        if headers.is_empty() {
            return None;
        }
        let mut metadata = serde_json::json!({"type":"response.metadata", "headers":headers});
        if let Some(stream_id) = event.get("stream_id") {
            metadata["stream_id"] = stream_id.clone();
        }
        Some(registry::Message::Text(metadata.to_string().into()))
    }
}

#[derive(Debug)]
pub(crate) enum ResponsesUpstreamSocket {
    Registry(RegistrySocket),
    ProxiedRegistry(
        registry_ws::WebSocketStream<
            registry_ws::MaybeTlsStream<crate::openai_transport::dialer::Socket>,
        >,
    ),
    // Compression state is much larger than a registry socket. Allocate once
    // per connection, never per message, without spawning an extra relay task.
    Codex(Box<CodexConnection>),
}

impl From<RegistrySocket> for ResponsesUpstreamSocket {
    fn from(socket: RegistrySocket) -> Self {
        Self::Registry(socket)
    }
}

impl ResponsesUpstreamSocket {
    pub(crate) fn take_handshake_headers(&mut self) -> Option<http::HeaderMap> {
        match self {
            Self::Codex(connection) => Some(std::mem::take(&mut connection.handshake_headers)),
            _ => None,
        }
    }
    pub(crate) async fn close(
        &mut self,
        frame: Option<registry::protocol::CloseFrame>,
    ) -> Result<()> {
        match self {
            Self::Registry(socket) => socket.close(frame).await.map_err(Into::into),
            Self::ProxiedRegistry(socket) => socket.close(frame).await.map_err(Into::into),
            Self::Codex(connection) => connection
                .socket
                .close(frame.map(to_codex_close))
                .await
                .map_err(codex_error),
        }
    }
}

pub(super) fn codex_config(limit: usize, compression: bool) -> codex::protocol::WebSocketConfig {
    let mut config = codex::protocol::WebSocketConfig::default()
        .max_message_size(Some(limit))
        .max_frame_size(Some(limit));
    config.extensions.permessage_deflate = compression.then(Default::default);
    config
}

// This is CONST's subscription boundary, not Codex's application-level
// connector (which also owns its network policy and OS proxy routes).
pub(super) struct CodexSubscriptionWebSocketConnector {
    tls: Arc<rustls::ClientConfig>,
}

impl CodexSubscriptionWebSocketConnector {
    pub(super) fn new(tls: Arc<rustls::ClientConfig>) -> Self {
        Self { tls }
    }

    pub(super) async fn connect(
        &self,
        mut request: http::Request<()>,
        limit: usize,
        compression: bool,
        responses_metadata: bool,
    ) -> Result<ResponsesUpstreamSocket> {
        let options = request
            .extensions_mut()
            .remove::<crate::openai_transport::TransportOptions>()
            .unwrap_or_default();
        let cookies = request
            .extensions_mut()
            .remove::<crate::openai_transport::WebSocketCookies>();
        if let Some(cookies) = &cookies {
            cookies.apply(request.headers_mut());
        }
        let result = super::bounded_websocket_connect(
            async {
                let stream = crate::openai_transport::dialer::connect(
                    request.uri(),
                    options.use_system_proxy,
                    self.tls.clone(),
                )
                .await
                .map_err(dial_error)?;
                let result = codex_ws::client_async_tls_with_config(
                    request,
                    stream,
                    Some(codex_config(limit, compression)),
                    Some(codex_ws::Connector::Rustls(Arc::clone(&self.tls))),
                )
                .await;
                // Rejections can also set routing/challenge cookies. Capture
                // them before preserving the original error for auth refresh.
                if let Some(cookies) = &cookies {
                    match &result {
                        Ok((_, response)) => cookies.store(response.headers()),
                        Err(codex::Error::Http(response)) => cookies.store(response.headers()),
                        _ => {}
                    }
                }
                let (socket, response) = result.map_err(codex_error)?;
                let handshake_headers = response.into_parts().0.headers;
                let metadata = responses_metadata.then(|| {
                    ["x-codex-turn-state", "openai-model", "x-reasoning-included"]
                        .into_iter()
                        .filter_map(|name| {
                            handshake_headers.get(name)?.to_str().ok().map(|value| {
                                (name.to_string(), serde_json::Value::String(value.into()))
                            })
                        })
                        .collect()
                });
                Ok(ResponsesUpstreamSocket::Codex(Box::new(CodexConnection {
                    socket,
                    handshake_headers,
                    metadata,
                    pending: None,
                })))
            },
            super::WEBSOCKET_UPSTREAM_CONNECT_TIMEOUT,
        )
        .await;
        super::finish_responses_websocket_connect(result)
    }
}

// Preserve HTTP rejection bodies/headers and I/O/TLS causes for the existing
// auth refresh, retry classification and diagnostics. Never log a proxy URL
// from a parser error: it may contain credentials.
fn codex_error(error: codex::Error) -> anyhow::Error {
    match error {
        codex::Error::Http(response) => registry::Error::Http(response).into(),
        codex::Error::Io(error) => registry::Error::Io(error).into(),
        codex::Error::Tls(codex::error::TlsError::Rustls(error)) => {
            registry::Error::Tls(registry::error::TlsError::Rustls(error)).into()
        }
        codex::Error::Tls(codex::error::TlsError::InvalidDnsName) => {
            registry::Error::Tls(registry::error::TlsError::InvalidDnsName).into()
        }
        codex::Error::Url(codex::error::UrlError::InvalidProxyConfig(_)) => {
            anyhow!("invalid OpenAI subscription WebSocket proxy configuration")
        }
        error => error.into(),
    }
}

fn dial_error(error: anyhow::Error) -> anyhow::Error {
    match error.downcast::<codex::Error>() {
        Ok(error) => codex_error(error),
        Err(error) => match error.downcast::<std::io::Error>() {
            Ok(error) => registry::Error::Io(error).into(),
            Err(error) => error,
        },
    }
}

fn to_codex_close(frame: registry::protocol::CloseFrame) -> codex::protocol::CloseFrame {
    codex::protocol::CloseFrame {
        code: u16::from(frame.code).into(),
        reason: frame.reason.to_string().into(),
    }
}

fn to_codex(message: registry::Message) -> Result<codex::Message> {
    Ok(match message {
        registry::Message::Text(text) => codex::Message::Text(bytes::Bytes::from(text).try_into()?),
        registry::Message::Binary(bytes) => codex::Message::Binary(bytes),
        registry::Message::Ping(bytes) => codex::Message::Ping(bytes),
        registry::Message::Pong(bytes) => codex::Message::Pong(bytes),
        registry::Message::Close(frame) => codex::Message::Close(frame.map(to_codex_close)),
        registry::Message::Frame(_) => {
            return Err(anyhow!(
                "raw frames cannot enter the Codex Responses transport"
            ));
        }
    })
}

fn from_codex(message: codex::Message) -> Result<registry::Message> {
    Ok(match message {
        codex::Message::Text(text) => registry::Message::Text(bytes::Bytes::from(text).try_into()?),
        codex::Message::Binary(bytes) => registry::Message::Binary(bytes),
        codex::Message::Ping(bytes) => registry::Message::Ping(bytes),
        codex::Message::Pong(bytes) => registry::Message::Pong(bytes),
        codex::Message::Close(frame) => {
            registry::Message::Close(frame.map(|frame| registry::protocol::CloseFrame {
                code: u16::from(frame.code).into(),
                reason: frame.reason.to_string().into(),
            }))
        }
        codex::Message::Frame(_) => {
            return Err(anyhow!(
                "unexpected raw frame from the Codex Responses transport"
            ));
        }
    })
}

impl Stream for ResponsesUpstreamSocket {
    type Item = Result<registry::Message>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.get_mut() {
            Self::Registry(socket) => Pin::new(socket)
                .poll_next(cx)
                .map(|item| item.map(|result| result.map_err(Into::into))),
            Self::ProxiedRegistry(socket) => Pin::new(socket)
                .poll_next(cx)
                .map(|item| item.map(|result| result.map_err(Into::into))),
            Self::Codex(connection) => {
                if let Some(pending) = connection.pending.take() {
                    return Poll::Ready(Some(Ok(pending)));
                }
                match Pin::new(&mut connection.socket).poll_next(cx) {
                    Poll::Ready(Some(Ok(message))) => {
                        let message = match from_codex(message) {
                            Ok(message) => message,
                            Err(error) => return Poll::Ready(Some(Err(error))),
                        };
                        if let Some(metadata) = connection.project_handshake(&message) {
                            connection.pending = Some(message);
                            Poll::Ready(Some(Ok(metadata)))
                        } else {
                            Poll::Ready(Some(Ok(message)))
                        }
                    }
                    other => other.map(|item| {
                        item.map(|result| result.map_err(codex_error).and_then(from_codex))
                    }),
                }
            }
        }
    }
}

impl Sink<registry::Message> for ResponsesUpstreamSocket {
    type Error = anyhow::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        match self.get_mut() {
            Self::Registry(socket) => Pin::new(socket).poll_ready(cx).map_err(Into::into),
            Self::ProxiedRegistry(socket) => Pin::new(socket).poll_ready(cx).map_err(Into::into),
            Self::Codex(connection) => Pin::new(&mut connection.socket)
                .poll_ready(cx)
                .map_err(codex_error),
        }
    }

    fn start_send(self: Pin<&mut Self>, item: registry::Message) -> Result<()> {
        match self.get_mut() {
            Self::Registry(socket) => Pin::new(socket).start_send(item).map_err(Into::into),
            Self::ProxiedRegistry(socket) => Pin::new(socket).start_send(item).map_err(Into::into),
            Self::Codex(connection) => Pin::new(&mut connection.socket)
                .start_send(to_codex(item)?)
                .map_err(codex_error),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        match self.get_mut() {
            Self::Registry(socket) => Pin::new(socket).poll_flush(cx).map_err(Into::into),
            Self::ProxiedRegistry(socket) => Pin::new(socket).poll_flush(cx).map_err(Into::into),
            Self::Codex(connection) => Pin::new(&mut connection.socket)
                .poll_flush(cx)
                .map_err(codex_error),
        }
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        match self.get_mut() {
            Self::Registry(socket) => Pin::new(socket).poll_close(cx).map_err(Into::into),
            Self::ProxiedRegistry(socket) => Pin::new(socket).poll_close(cx).map_err(Into::into),
            Self::Codex(connection) => Pin::new(&mut connection.socket)
                .poll_close(cx)
                .map_err(codex_error),
        }
    }
}

#[cfg(test)]
#[path = "responses_ws_transport_tests.rs"]
mod tests;
