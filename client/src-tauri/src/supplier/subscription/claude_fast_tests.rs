use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn claude_fast_policy_only_changes_an_explicit_fast_request() {
    let ordinary = r#"{ "model": "claude-future", "messages": [], "service_tier":"auto" }"#;
    for enabled in [false, true] {
        assert_eq!(
            claude_subscription_fast_policy(ordinary.to_string(), enabled).unwrap(),
            ordinary
        );
    }
    let mut original = serde_json::json!({
        "model": "claude-future", "speed": "fast", "stream": true,
        "tools": [{"name":"test","input_schema":{"type":"object"}}],
        "messages": [{"role":"user","content":[{"type":"text","text":"test","cache_control":{"type":"ephemeral"}}]}],
        "future_field": {"keep":true}
    });
    let body = original.to_string();
    assert_eq!(
        claude_subscription_fast_policy(body.clone(), true).unwrap(),
        body
    );
    let adjusted = claude_subscription_fast_policy(body, false).unwrap();
    original["speed"] = serde_json::json!("standard");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&adjusted).unwrap(),
        original
    );
}

#[test]
fn claude_fast_recovery_requires_an_explicit_feature_rejection() {
    assert!(claude_fast_explicitly_unsupported(
        r#"{"error":{"type":"invalid_request_error","message":"This model does not support fast mode"}}"#
    ));
    assert!(claude_fast_explicitly_unsupported(
        r#"{"error":{"type":"invalid_request_error","param":"speed","code":"unsupported_value"}}"#
    ));
    for error in [
        r#"{"error":{"type":"invalid_request_error","message":"invalid tools"}}"#,
        r#"{"error":{"type":"invalid_request_error","message":"Fast mode rate limit exceeded"}}"#,
        r#"{"error":{"type":"permission_error","message":"fast mode is not supported"}}"#,
        r#"{"error":{"type":"invalid_request_error","message":"Insufficient credits for fast mode"}}"#,
        "upstream timeout",
    ] {
        assert!(
            !claude_fast_explicitly_unsupported(error),
            "must not replay: {error}"
        );
    }
}

// Local HTTP fixtures exercise the common sender used by JSON and SSE paths.
async fn mock_claude_fast_responses(
    enabled: bool,
    replies: Vec<(u16, &'static str, &'static str)>,
) -> (u16, String) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let url = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for (status, reply, speed) in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4096];
            let header_end = loop {
                let read = socket.read(&mut buffer).await.unwrap();
                assert!(read > 0);
                request.extend_from_slice(&buffer[..read]);
                if let Some(pos) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    break pos + 4;
                }
            };
            let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
            let length: usize = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            while request.len() < header_end + length {
                let read = socket.read(&mut buffer).await.unwrap();
                assert!(read > 0);
                request.extend_from_slice(&buffer[..read]);
            }
            let body: serde_json::Value =
                serde_json::from_slice(&request[header_end..header_end + length]).unwrap();
            assert_eq!(body["speed"], speed);
            assert_eq!(body["model"], "claude-future");
            assert_eq!(body["messages"][0]["content"], "test");
            assert_eq!(headers.contains("fast-mode-2026-02-01"), speed == "fast");
            let content_type = if reply.starts_with("data:") {
                "text/event-stream"
            } else {
                "application/json"
            };
            socket.write_all(format!("HTTP/1.1 {status} Test\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).as_bytes()).await.unwrap();
        }
    });
    let mut channel = crate::default_config().channels.remove(0);
    channel.subscription.claude_fast_enabled = enabled;
    let client = Client::builder().no_proxy().build().unwrap();
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), send_claude_subscription_response(
        &client, &channel, &url, "fixture-token".to_string(),
        r#"{"model":"claude-future","speed":"fast","messages":[{"role":"user","content":"test"}]}"#.to_string(),
        "http://127.0.0.1/unused-token-url", None,
    )).await.unwrap().unwrap();
    let result = match response {
        ClaudeSubscriptionResponse::Response(response) => {
            (response.status().as_u16(), response.text().await.unwrap())
        }
        ClaudeSubscriptionResponse::Rejected(failure) => (failure.status, failure.raw),
    };
    server.await.unwrap();
    result
}

#[tokio::test]
async fn claude_fast_sender_is_opt_in_and_downgrades_at_most_once() {
    const OK: &str = r#"{"usage":{"input_tokens":2,"output_tokens":1,"speed":"standard"}}"#;
    const UNSUPPORTED: &str = r#"{"error":{"type":"invalid_request_error","message":"This model does not support fast mode"}}"#;
    assert_eq!(
        mock_claude_fast_responses(false, vec![(200, OK, "standard")]).await,
        (200, OK.to_string())
    );
    assert_eq!(
        mock_claude_fast_responses(
            true,
            vec![(400, UNSUPPORTED, "fast"), (200, OK, "standard")]
        )
        .await,
        (200, OK.to_string())
    );
    assert_eq!(
        mock_claude_fast_responses(
            true,
            vec![(400, UNSUPPORTED, "fast"), (400, UNSUPPORTED, "standard")]
        )
        .await,
        (400, UNSUPPORTED.to_string())
    );
    for status in [400, 403, 429, 503] {
        let error = r#"{"error":{"type":"invalid_request_error","message":"quota exhausted"}}"#;
        assert_eq!(
            mock_claude_fast_responses(true, vec![(status, error, "fast")]).await,
            (status, error.to_string())
        );
    }
    const SSE: &str =
        "data: {\"usage\":{\"speed\":\"fast\",\"input_tokens\":2,\"output_tokens\":1}}\n\n";
    assert_eq!(
        mock_claude_fast_responses(true, vec![(200, SSE, "fast")]).await,
        (200, SSE.to_string())
    );
}
