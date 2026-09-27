// Typed context survives anyhow wrapping during continuation replay. Never
// infer a pre-send failure from provider text or a retired pool connection.
#[derive(Debug)]
struct ResponsesWsConnectFailure {
    cause: &'static str,
    timeout_ms: u64,
}

impl std::fmt::Display for ResponsesWsConnectFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.timeout_ms > 0 {
            write!(
                f,
                "Responses WebSocket upstream did not connect within {} seconds",
                self.timeout_ms / 1000
            )
        } else {
            write!(
                f,
                "Responses WebSocket upstream connection failed ({})",
                self.cause
            )
        }
    }
}

impl std::error::Error for ResponsesWsConnectFailure {}

fn responses_ws_io_cause(error: &tokio_tungstenite::tungstenite::Error) -> &'static str {
    use tokio_tungstenite::tungstenite::Error;
    match error {
        Error::Tls(_) => "tls_error",
        Error::Io(error) => match error.kind() {
            std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted => {
                "connection_reset"
            }
            std::io::ErrorKind::ConnectionRefused => "connection_refused",
            _ => "connect_error",
        },
        _ => "connect_error",
    }
}

pub(crate) fn responses_websocket_connect_error_payload(
    error: &anyhow::Error,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let failure = error.downcast_ref::<ResponsesWsConnectFailure>()?;
    let mut payload = supplier_duplex_terminal_error_payload(&failure.to_string(), 502);
    payload.insert("failure_stage".into(), "upstream_connect".into());
    payload.insert("transport_cause".into(), failure.cause.into());
    payload.insert("request_sent".into(), false.into());
    if failure.timeout_ms > 0 {
        payload.insert("timeout_ms".into(), failure.timeout_ms.into());
    }
    attach_responses_transport_error_to_wire(&mut payload);
    Some(payload)
}

// Both native WS consumers and HTTP/SSE bridges receive the same additive
// diagnostics. Keep the existing type/code and never change retry safety here.
fn attach_responses_transport_error_to_wire(
    payload: &mut serde_json::Map<String, serde_json::Value>,
) {
    let Some(data) = payload.get("data").and_then(serde_json::Value::as_str) else {
        return;
    };
    let Ok(mut event) = serde_json::from_str::<serde_json::Value>(data) else {
        return;
    };
    let Some(error) = event
        .get_mut("error")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    for key in [
        "failure_stage",
        "transport_cause",
        "timeout_ms",
        "request_sent",
        "upstream_close_code",
    ] {
        if let Some(value) = payload.get(key) {
            error.insert(key.into(), value.clone());
        }
    }
    payload.insert("data".into(), event.to_string().into());
}

fn attach_responses_transport_failure(
    payload: &mut serde_json::Map<String, serde_json::Value>,
    stage: &'static str,
    cause: &'static str,
) {
    payload.insert("failure_stage".into(), stage.into());
    payload.insert("transport_cause".into(), cause.into());
    // Bytes may already have reached the upstream. In particular, an I/O error
    // is NOT proof that a response.create is safe to submit a second time.
    attach_responses_transport_error_to_wire(payload);
}

#[cfg(test)]
mod websocket_transport_failure_tests {
    use super::*;

    #[test]
    fn connect_timeout_survives_replay_context_and_is_not_a_retired_socket_failure() {
        let error = anyhow!(ResponsesWsConnectFailure {
            cause: "connect_timeout",
            timeout_ms: 20_000
        })
        .context("Responses WebSocket continuation replay send failed");
        let mut payload = supplier_responses_send_error_payload(&error);
        let mut old = UpstreamWebsocketLease::open("pool", "test-connect-evidence", "codex");
        old.set_close_reason("pool_burst_idle");
        old.close();
        old.attach(&mut payload);
        assert_eq!(payload["failure_stage"], "upstream_connect");
        assert_eq!(payload["transport_cause"], "connect_timeout");
        assert_eq!(payload["timeout_ms"], 20_000);
        assert_eq!(payload["request_sent"], false);
        assert!(!payload.contains_key("upstream_connection_id"));
        assert!(!payload.contains_key("upstream_close_reason"));
        let wire: serde_json::Value =
            serde_json::from_str(payload["data"].as_str().unwrap()).unwrap();
        assert_eq!(wire["error"]["code"], "upstream_websocket_interrupted");
        assert_eq!(wire["error"]["transport_cause"], "connect_timeout");
        assert_eq!(wire["error"]["request_sent"], false);
    }

    #[test]
    fn connect_failure_diagnostics_do_not_echo_source_credentials() {
        let error = anyhow!(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "sk-secret private URL"
        ))
        .context(ResponsesWsConnectFailure {
            cause: "connection_reset",
            timeout_ms: 0,
        });
        let payload = responses_websocket_connect_error_payload(&error).unwrap();
        assert_eq!(payload["transport_cause"], "connection_reset");
        assert!(!serde_json::to_string(&payload)
            .unwrap()
            .contains("sk-secret"));
        assert!(error.downcast_ref::<std::io::Error>().is_some());
        // A same-text error is not typed pre-send evidence.
        assert!(responses_websocket_connect_error_payload(&anyhow!(
            "Responses WebSocket upstream did not connect within 20 seconds"
        ))
        .is_none());
    }

    #[test]
    fn ambiguous_write_keeps_legacy_code_and_does_not_claim_safe_replay() {
        let payload = supplier_responses_send_error_payload(&anyhow!(
            "writer disconnected after partial send"
        ));
        assert_eq!(payload["failure_stage"], "upstream_send");
        assert!(!payload.contains_key("request_sent"));
        assert!(!payload.contains_key("safe_to_retry_other_channel"));
        let wire: serde_json::Value =
            serde_json::from_str(payload["data"].as_str().unwrap()).unwrap();
        assert_eq!(wire["error"]["transport_cause"], "write_error");
    }

    #[tokio::test]
    async fn handshake_auth_rejection_is_not_softened_to_a_network_failure() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer).await.unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        });
        let request = format!("ws://{address}/responses")
            .into_client_request()
            .unwrap();
        let error = connect_responses_websocket_request(request)
            .await
            .expect_err("must reject");
        server.await.unwrap();
        assert!(responses_websocket_connect_error_payload(&error).is_none());
        assert_eq!(
            responses_websocket_connect_error_metadata(&error).status,
            401
        );
    }
}
