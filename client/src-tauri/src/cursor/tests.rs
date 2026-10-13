use super::*;
use crate::protocol::{ir::CanonicalStreamEvent as Event, kind::ProtocolKind};
use http_body_util::BodyExt;
use sha2::Digest;
use std::{
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::mpsc;
use wire as w;

#[test]
fn protobuf_json_and_fragmented_gzip_frames_are_bounded() {
    let value = json!({"ok":true,"n":1.5,"nested":[null,"x",{"a":2}]});
    assert_eq!(w::decode_json(&w::json(&value), 0).unwrap(), value);
    let mut encoded = w::frame(&w::message(1, &[w::message(1, &[w::bytes(1, "hello")])])).unwrap();
    let mut frames = w::Frames::default();
    let mut count = 0;
    for chunk in encoded.chunks(2) {
        count += frames.push(chunk).unwrap().len();
    }
    assert_eq!(count, 1);
    frames.finish().unwrap();
    encoded.pop();
    let mut partial = w::Frames::default();
    partial.push(&encoded).unwrap();
    assert!(partial.finish().is_err());
    assert!(
        w::Frames::default()
            .push(&[0, 0xff, 0xff, 0xff, 0xff])
            .is_err()
    );
    assert!(w::fields(&[0x0a, 0x80]).is_err());
    assert!(w::fields(&[0]).is_err());
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut gzip, b"abc").unwrap();
    let compressed = gzip.finish().unwrap();
    let mut frame = w::frame(&compressed).unwrap();
    frame[0] = 1;
    assert_eq!(w::Frames::default().push(&frame).unwrap()[0].1, b"abc");
}

#[test]
fn credentials_keep_refresh_rotation_and_do_not_invent_expiry() {
    let value = normalize_token(
        json!({"accessToken":"synthetic-access","refreshToken":"synthetic-refresh"}),
        None,
    )
    .unwrap();
    assert!(value.get("expires_at_unix").is_none());
    assert_eq!(
        normalize_token(json!({"accessToken":"rotated"}), Some(&value)).unwrap()["refresh_token"],
        "synthetic-refresh"
    );
    assert!(normalize_token(json!({"accessToken":"bad\r\nheader"}), None).is_err());
    assert!(normalize_token(json!({}), None).is_err());
    let imported = crate::detection::normalize_subscription_token("cursor", value).unwrap();
    assert_eq!(imported.0, "cursor");
    let session = auth::create_cursor_auth_session().unwrap();
    assert!(!session.to_string().contains("verifier"));
    let url = reqwest::Url::parse(session["verification_uri"].as_str().unwrap()).unwrap();
    assert_eq!(url.host_str(), Some("cursor.com"));
    assert_eq!(url.path(), "/loginDeepControl");
    auth::cancel_cursor_auth_session(session["device_code"].as_str().unwrap().into()).unwrap();
}

#[test]
fn account_endpoint_is_privacy_preserving_and_cannot_receive_tokens_off_domain() {
    let config = |url: &str| {
        w::message(
            27,
            &[
                w::bytes(1, url),
                w::bytes(2, "https://agentn.global.api5.cursor.sh"),
            ],
        )
    };
    assert_eq!(
        endpoint::parse(&config("https://agent.eu.api5.cursor.sh")).unwrap(),
        "https://agent.eu.api5.cursor.sh/agent.v1.AgentService/Run"
    );
    for url in [
        "https://api5.cursor.sh.attacker.test",
        "https://attacker.test",
        "http://agent.eu.api5.cursor.sh",
        "https://user@agent.eu.api5.cursor.sh",
        "https://agent.eu.api5.cursor.sh/redirect",
        "https://agent.eu.api5.cursor.sh?token=x",
    ] {
        assert!(endpoint::parse(&config(url)).is_err(), "{url}");
    }
    assert!(endpoint::parse(&[]).is_err());
}

fn chat() -> Value {
    json!({"model":"claude-test","messages":[{"role":"user","content":"Call read"}],
        "tools":[{"type":"function","function":{"name":"read","description":"Read a file","parameters":{"type":"object","properties":{"path":{"type":"string"}}}}}]})
}

#[test]
fn all_protocols_keep_tools_and_wire_model_with_caller_isolation() {
    let bodies = [
        (ProtocolKind::OpenAiChat, chat()),
        (
            ProtocolKind::AnthropicMessages,
            json!({"model":"claude-test","messages":[{"role":"user","content":"Call read"}],"tools":[{"name":"read","input_schema":{"type":"object"}}],"max_tokens":32}),
        ),
        (
            ProtocolKind::OpenAiResponses,
            json!({"model":"claude-test","input":[{"type":"additional_tools","role":"developer","tools":[{"type":"function","name":"read","parameters":{"type":"object"}}]},{"role":"user","content":"Call read"}]}),
        ),
        (
            ProtocolKind::GeminiNative,
            json!({"model":"claude-test","contents":[{"role":"user","parts":[{"text":"Call read"}]}],"tools":[{"functionDeclarations":[{"name":"read","parameters":{"type":"OBJECT"}}]}]}),
        ),
    ];
    for (protocol, body) in bodies {
        let prepared =
            request::prepare(protocol, &body, "anthropic/claude-test", "caller-a").unwrap();
        assert_eq!(prepared.tools.len(), 1, "{protocol:?}");
        let run = w::fields(w::data(&w::fields(&prepared.run).unwrap(), 1).unwrap()).unwrap();
        assert_eq!(w::integer(&run, 12), Some(0));
        assert!(w::data(&run, 8).is_none());
        let state = w::fields(w::data(&run, 1).unwrap()).unwrap();
        let rule_id = w::data(&state, 1).unwrap();
        let rules: Value = serde_json::from_slice(&prepared.blobs[rule_id]).unwrap();
        assert_eq!(rules["role"], "user");
        assert!(
            rules["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("CONST MCP tools")
        );
        let context = request::context(&prepared.tools);
        let requested_model = w::fields(w::data(&run, 9).unwrap()).unwrap();
        assert_eq!(
            w::string(&requested_model, 1).unwrap(),
            "anthropic/claude-test"
        );
        assert_ne!(
            prepared.scope,
            request::prepare(protocol, &body, "anthropic/claude-test", "caller-b")
                .unwrap()
                .scope
        );
        assert!(w::data(&w::fields(&context).unwrap(), 34).is_some());
        assert!(
            w::fields(&context)
                .unwrap()
                .iter()
                .all(|(field, _)| matches!(field, 7 | 34))
        );
    }
}

fn history_request() -> Value {
    let mut body = chat();
    body["messages"].as_array_mut().unwrap().extend([
        json!({"role":"assistant","content":null,"tool_calls":[{"type":"function","id":"call_1","function":{"name":"read","arguments":"{\"path\":\"file\"}"}}]}),
        json!({"role":"tool","tool_call_id":"call_1","content":"file content"}),
    ]);
    body
}

#[test]
fn native_history_retains_tool_results_for_connection_loss() {
    let body = history_request();
    let prepared =
        request::prepare(ProtocolKind::OpenAiChat, &body, "claude-test", "caller").unwrap();
    assert_eq!(prepared.results.len(), 1);
    let run = w::fields(w::data(&w::fields(&prepared.run).unwrap(), 1).unwrap()).unwrap();
    let state = w::fields(w::data(&run, 1).unwrap()).unwrap();
    assert!(w::data(&state, 8).is_none());
    let history = state
        .iter()
        .filter_map(|(n, v)| match (n, v) {
            (1, w::ValueRef::Bytes(id)) => {
                Some(serde_json::from_slice::<Value>(&prepared.blobs[*id]).unwrap())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_imported_tool_history(&history);
    assert!(!String::from_utf8_lossy(&prepared.run).contains("file content"));
    for (id, value) in &prepared.blobs {
        assert_eq!(*id, sha2::Sha256::digest(value).to_vec());
    }
}

fn assert_imported_tool_history(history: &[Value]) {
    assert_eq!(history.len(), 4);
    assert_eq!(history[0]["role"], "user");
    assert!(
        history[0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("<user_rules>")
    );
    assert_eq!(
        history[1],
        json!({"role":"user","content":[{"type":"text","text":"Call read"}]})
    );
    assert_eq!(
        history[2],
        json!({"role":"assistant","content":[{"type":"tool-call","toolCallId":"call_1","toolName":"read","args":{"path":"file"}}]})
    );
    assert_eq!(
        history[3],
        json!({"role":"tool","content":[{"type":"tool-result","toolCallId":"call_1","toolName":"read","result":"file content","isError":false}]})
    );
}

#[test]
fn cursor_model_history_preserves_roles_images_and_stable_prefixes() {
    let mut body = json!({"model":"claude-test","messages":[
        {"role":"system","content":"Follow the caller's fixed rule."},
        {"role":"user","content":[{"type":"text","text":"First message"},{"type":"image_url","image_url":{"url":"data:image/png;base64,aGk="}}]},
        {"role":"assistant","content":"First answer"},
        {"role":"user","content":"Next question"}
    ]});
    let first = request::prepare(
        ProtocolKind::OpenAiChat,
        &body,
        "claude-test",
        "same-caller",
    )
    .unwrap();
    let ids = |prepared: &request::Prepared| {
        let run = w::fields(w::data(&w::fields(&prepared.run).unwrap(), 1).unwrap()).unwrap();
        w::fields(w::data(&run, 1).unwrap())
            .unwrap()
            .into_iter()
            .filter_map(|(n, v)| match (n, v) {
                (1, w::ValueRef::Bytes(id)) => Some(id.to_vec()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let first_ids = ids(&first);
    assert_eq!(first_ids.len(), 3);
    let rules: Value = serde_json::from_slice(&first.blobs[&first_ids[0]]).unwrap();
    assert!(
        rules["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("caller's fixed rule")
    );
    let user: Value = serde_json::from_slice(&first.blobs[&first_ids[1]]).unwrap();
    assert_eq!(user["role"], "user");
    assert_eq!(user["content"][0]["text"], "First message");
    assert_eq!(
        user["content"][1],
        json!({"type":"image","image":{"__type":"Uint8Array","hex":"6869"},"mimeType":"image/png"})
    );
    let assistant: Value = serde_json::from_slice(&first.blobs[&first_ids[2]]).unwrap();
    assert_eq!(assistant["role"], "assistant");
    assert_eq!(assistant["content"][0]["text"], "First answer");
    body["messages"].as_array_mut().unwrap().extend([
        json!({"role":"assistant","content":"Second answer"}),
        json!({"role":"user","content":"Third question"}),
    ]);
    let next = request::prepare(
        ProtocolKind::OpenAiChat,
        &body,
        "claude-test",
        "same-caller",
    )
    .unwrap();
    assert_eq!(&ids(&next)[..first_ids.len()], first_ids.as_slice());
    assert_eq!(first.scope, next.scope);
}

#[test]
fn cursor_cold_tool_result_keeps_the_new_user_instruction() {
    let mut body = history_request();
    body["messages"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role":"user","content":"Answer from the stored result; do not call again."}));
    let prepared =
        request::prepare(ProtocolKind::OpenAiChat, &body, "claude-test", "caller").unwrap();
    assert!(prepared.results.is_empty());
    let run = w::fields(w::data(&w::fields(&prepared.run).unwrap(), 1).unwrap()).unwrap();
    let action = w::fields(w::data(&run, 2).unwrap()).unwrap();
    let user_action = w::fields(w::data(&action, 1).unwrap()).unwrap();
    let user = w::fields(w::data(&user_action, 1).unwrap()).unwrap();
    assert_eq!(
        w::string(&user, 1).unwrap(),
        "Answer from the stored result; do not call again."
    );
    let history = w::fields(w::data(&run, 1).unwrap())
        .unwrap()
        .into_iter()
        .filter_map(|(n, v)| match (n, v) {
            (1, w::ValueRef::Bytes(id)) => {
                Some(serde_json::from_slice::<Value>(&prepared.blobs[id]).unwrap())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_imported_tool_history(&history);
}

// A real local HTTP/2 endpoint: it waits for replies on the still-open request
// body before producing more response data, so a half-duplex implementation fails.
async fn mock(
    mode: &'static str,
) -> (
    String,
    Arc<AtomicUsize>,
    mpsc::Receiver<()>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let count = runs.clone();
    let (closed, closed_rx) = mpsc::channel(16);
    let server = tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let count = count.clone();
            let closed = closed.clone();
            tokio::spawn(async move {
                let service = hyper::service::service_fn(
                    move |req: hyper::Request<hyper::body::Incoming>| {
                        let count = count.clone();
                        let closed = closed.clone();
                        async move {
                            let (tx, rx) = mpsc::channel::<Vec<u8>>(16);
                            let path = req.uri().path().to_string();
                            let status = if mode == "forbidden" { 403 } else { 200 };
                            tokio::spawn(async move {
                                if mode == "forbidden" {
                                    tx.send(br#"{"code":"permission_denied","message":"Requires a paid plan; synthetic-secret"}"#.to_vec()).await.unwrap();
                                    return;
                                }
                                let mut input = req.into_body();
                                if path.ends_with("GetUsableModels") {
                                    assert!(input.collect().await.unwrap().to_bytes().is_empty());
                                    tx.send(w::message(
                                        1,
                                        &[w::bytes(1, "claude-live"), w::bytes(4, "Claude Live")],
                                    ))
                                    .await
                                    .unwrap();
                                    return;
                                }
                                count.fetch_add(1, Ordering::SeqCst);
                                let first =
                                    input.frame().await.unwrap().unwrap().into_data().unwrap();
                                assert_eq!(w::Frames::default().push(&first).unwrap().len(), 1);
                                if mode == "canonical" {
                                    let frames = w::Frames::default().push(&first).unwrap();
                                    let message = w::fields(&frames[0].1).unwrap();
                                    let run = w::fields(w::data(&message, 1).unwrap()).unwrap();
                                    let requested = w::fields(w::data(&run, 9).unwrap()).unwrap();
                                    assert_eq!(
                                        w::string(&requested, 1).unwrap(),
                                        "claude-haiku-5-5-medium"
                                    );
                                }
                                if mode == "history" {
                                    let frames = w::Frames::default().push(&first).unwrap();
                                    let message = w::fields(&frames[0].1).unwrap();
                                    let run = w::fields(w::data(&message, 1).unwrap()).unwrap();
                                    let state = w::fields(w::data(&run, 1).unwrap()).unwrap();
                                    assert!(w::data(&state, 8).is_none());
                                    let mut history = Vec::new();
                                    for (n, v) in state {
                                        if let (1, w::ValueRef::Bytes(id)) = (n, v) {
                                            let blob = fetch_blob(&tx, &mut input, id).await;
                                            history.push(
                                                serde_json::from_slice::<Value>(&blob).unwrap(),
                                            );
                                        }
                                    }
                                    assert_imported_tool_history(&history);
                                }
                                if matches!(
                                    mode,
                                    "text"
                                        | "history"
                                        | "canonical"
                                        | "metered"
                                        | "partial_usage"
                                        | "zero_usage"
                                ) {
                                    tx.send(
                                        w::frame(&w::message(
                                            1,
                                            &[w::message(1, &[w::bytes(1, "hello world")])],
                                        ))
                                        .unwrap(),
                                    )
                                    .await
                                    .unwrap();
                                    let usage = match mode {
                                        "metered" => vec![
                                            w::int(1, 200),
                                            w::int(2, 8),
                                            w::int(3, 80),
                                            w::int(4, 20),
                                            w::int(5, 3),
                                        ],
                                        "partial_usage" => vec![w::int(1, 100)],
                                        "zero_usage" => vec![w::int(1, 0), w::int(2, 0)],
                                        _ => vec![],
                                    };
                                    tx.send(
                                        w::frame(&w::message(1, &[w::message(14, &usage)]))
                                            .unwrap(),
                                    )
                                    .await
                                    .unwrap();
                                    return;
                                }
                                if mode == "connect_denied" || mode == "late_error" {
                                    if mode == "late_error" {
                                        tx.send(
                                            w::frame(&w::message(
                                                1,
                                                &[w::message(1, &[w::bytes(1, "partial")])],
                                            ))
                                            .unwrap(),
                                        )
                                        .await
                                        .unwrap();
                                    }
                                    let mut end = w::frame(br#"{"error":{"code":"permission_denied","message":"Account subscription is required"}}"#).unwrap();
                                    end[0] = 2;
                                    tx.send(end).await.unwrap();
                                    return;
                                }
                                if mode == "cancel" {
                                    while let Some(frame) = input.frame().await {
                                        if frame.is_err() {
                                            break;
                                        }
                                    }
                                    let _ = closed.send(()).await;
                                    return;
                                }
                                let context = w::message(
                                    2,
                                    &[w::int(1, 7), w::bytes(15, "ctx"), w::bytes(10, [])],
                                );
                                tx.send(w::frame(&context).unwrap()).await.unwrap();
                                read_reply(&mut input, 2, 10).await;
                                let state = w::message(
                                    2,
                                    &[w::int(1, 8), w::bytes(15, "state"), w::bytes(36, [])],
                                );
                                tx.send(w::frame(&state).unwrap()).await.unwrap();
                                read_reply(&mut input, 2, 36).await;
                                // The ordinary workspace handshake must not
                                // grant any access to the supplier's machine.
                                for operation in [7, 2] {
                                    let request = w::message(
                                        2,
                                        &[
                                            w::int(1, 80 + u64::from(operation)),
                                            w::bytes(15, "denied-local-exec"),
                                            w::message(
                                                operation,
                                                &[w::bytes(1, "supplier-private")],
                                            ),
                                        ],
                                    );
                                    tx.send(w::frame(&request).unwrap()).await.unwrap();
                                    let reply = read_reply(&mut input, 2, operation).await;
                                    assert!(
                                        String::from_utf8_lossy(&reply)
                                            .contains("Local execution is unavailable")
                                    );
                                }
                                let kv = w::message(
                                    4,
                                    &[
                                        w::int(1, 1),
                                        w::message(3, &[w::bytes(1, "blob"), w::bytes(2, "value")]),
                                        w::bytes(4, "metadata"),
                                    ],
                                );
                                tx.send(w::frame(&kv).unwrap()).await.unwrap();
                                let reply = read_reply(&mut input, 3, 3).await;
                                let fields =
                                    w::fields(w::data(&w::fields(&reply).unwrap(), 3).unwrap())
                                        .unwrap();
                                assert_eq!(w::data(&fields, 4).unwrap(), b"metadata");
                                let text =
                                    w::message(1, &[w::message(1, &[w::bytes(1, "Checking")])]);
                                tx.send(w::frame(&text).unwrap()).await.unwrap();
                                let tool = w::message(
                                    2,
                                    &[
                                        w::int(1, 9),
                                        w::bytes(15, "exec-1"),
                                        w::message(
                                            11,
                                            &[
                                                w::bytes(1, "read"),
                                                w::bytes(5, "read"),
                                                w::bytes(3, "upstream-call"),
                                                w::message(
                                                    2,
                                                    &[
                                                        w::bytes(1, "path"),
                                                        w::bytes(2, w::json(&json!("file"))),
                                                    ],
                                                ),
                                            ],
                                        ),
                                    ],
                                );
                                tx.send(w::frame(&tool).unwrap()).await.unwrap();
                                let reply = read_reply(&mut input, 2, 11).await;
                                assert!(String::from_utf8_lossy(&reply).contains("file content"));
                                let mut text = "Done".to_string();
                                if mode == "isolated_tools" {
                                    let mut content = reply.as_slice();
                                    for field in [2, 11, 1, 1, 1, 1] {
                                        content =
                                            w::data(&w::fields(content).unwrap(), field).unwrap();
                                    }
                                    text = std::str::from_utf8(content).unwrap().into();
                                }
                                tx.send(
                                    w::frame(&w::message(
                                        1,
                                        &[w::message(1, &[w::bytes(1, text)])],
                                    ))
                                    .unwrap(),
                                )
                                .await
                                .unwrap();
                                // Cumulative counters must not be charged again
                                // against the final public tool-result request.
                                tx.send(
                                    w::frame(&w::message(
                                        1,
                                        &[w::message(
                                            14,
                                            &[w::int(1, 900), w::int(2, 30), w::int(3, 600)],
                                        )],
                                    ))
                                    .unwrap(),
                                )
                                .await
                                .unwrap();
                            });
                            let stream = futures_util::stream::unfold(rx, |mut rx| async {
                                rx.recv().await.map(|b| {
                                    (
                                        Ok::<_, Infallible>(hyper::body::Frame::data(
                                            bytes::Bytes::from(b),
                                        )),
                                        rx,
                                    )
                                })
                            });
                            Ok::<_, Infallible>(
                                hyper::Response::builder()
                                    .status(status)
                                    .header("content-type", "application/connect+proto")
                                    .body(http_body_util::StreamBody::new(stream))
                                    .unwrap(),
                            )
                        }
                    },
                );
                let _ =
                    hyper::server::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
                        .serve_connection(hyper_util::rt::TokioIo::new(socket), service)
                        .await;
            });
        }
    });
    (format!("http://{address}"), runs, closed_rx, server)
}

async fn read_reply(input: &mut hyper::body::Incoming, top: u32, member: u32) -> Vec<u8> {
    let mut frames = w::Frames::default();
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(5), input.frame())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Ok(bytes) = frame.into_data() {
            for (_, reply) in frames.push(&bytes).unwrap() {
                let fields = w::fields(&reply).unwrap();
                if let Some(body) = w::data(&fields, top) {
                    assert!(w::data(&w::fields(body).unwrap(), member).is_some());
                    return reply;
                }
            }
        }
    }
}

async fn fetch_blob(
    tx: &mpsc::Sender<Vec<u8>>,
    input: &mut hyper::body::Incoming,
    id: &[u8],
) -> Vec<u8> {
    assert_eq!(id.len(), 32);
    tx.send(
        w::frame(&w::message(
            4,
            &[w::int(1, 1), w::message(2, &[w::bytes(1, id)])],
        ))
        .unwrap(),
    )
    .await
    .unwrap();
    let reply = read_reply(input, 3, 2).await;
    let reply = w::fields(w::data(&w::fields(&reply).unwrap(), 3).unwrap()).unwrap();
    let value = w::fields(w::data(&reply, 2).unwrap()).unwrap();
    let value = w::data(&value, 1).unwrap().to_vec();
    assert_eq!(id, sha2::Sha256::digest(&value).as_slice());
    value
}

fn h2_client() -> Client {
    Client::builder()
        .no_proxy()
        .http2_prior_knowledge()
        .build()
        .unwrap()
}

// Manual account acceptance only. Normal test runs never read a credential or
// call upstream. `text` spends one short request; `tools` spends at most two.
#[tokio::test]
#[ignore = "requires explicit Cursor account and paid-probe authorization"]
async fn cursor_live_account_acceptance() -> Result<()> {
    use crate::protocol::{
        adapters::AdapterContext, conversion::ConversionPlan, stream::CanonicalAccumulator,
    };
    async fn call(
        client: &Client,
        token: &str,
        body: &Value,
        model: &str,
        scope: &str,
    ) -> Result<Value> {
        let prepared = request::prepare(ProtocolKind::OpenAiChat, body, model, scope)?;
        let mut events = runtime::start(client, token, &prepared, None).await?;
        let response = tokio::time::timeout(Duration::from_secs(120), async {
            let mut accumulator = CanonicalAccumulator::new();
            loop {
                let event = events.recv().await.context("Cursor live stream closed")??;
                let done = matches!(event, Event::ResponseDone(_));
                accumulator.push(&event)?;
                if done {
                    return Ok::<_, anyhow::Error>(accumulator.finish()?);
                }
            }
        })
        .await
        .context("Cursor live check timed out")??;
        Ok(crate::protocol::encode_response(
            ProtocolKind::OpenAiChat,
            &response,
            &ConversionPlan::native_passthrough(ProtocolKind::OpenAiChat),
            &AdapterContext::runtime("cursor", "cursor_agent", None, model),
        )?)
    }

    let path = std::env::var("CONST_CURSOR_LIVE_CREDENTIAL_PATH")?;
    let phase = std::env::var("CONST_CURSOR_LIVE_PHASE")?;
    ensure!(
        matches!(
            phase.as_str(),
            "catalog" | "text" | "tools" | "cold_tools" | "system"
        ),
        "unknown live phase"
    );
    let credential = normalize_token(serde_json::from_slice(&std::fs::read(path)?)?, None)?;
    let token = credential["access_token"].as_str().unwrap();
    let client = http_client()?;
    let models = catalog(&client, token).await?;
    if phase == "catalog" {
        println!(
            "Cursor account catalog: {} models; Claude IDs: {}",
            models.len(),
            json!(
                models
                    .iter()
                    .filter(|m| m.id.contains("claude"))
                    .map(|m| &m.id)
                    .collect::<Vec<_>>()
            )
        );
        return Ok(());
    }
    ensure!(
        std::env::var("CONST_CURSOR_LIVE_ALLOW_PAID").as_deref() == Ok("approved-short-probe"),
        "explicit paid-probe authorization is required"
    );
    let model = std::env::var("CONST_CURSOR_LIVE_MODEL")?;
    ensure!(
        models.iter().any(|m| m.id == model),
        "selected model is absent from the account catalog"
    );
    let scope = format!("cursor-live-{}", uuid::Uuid::new_v4());
    let started = std::time::Instant::now();
    if phase == "text" {
        let body = json!({"model":model,"messages":[{"role":"user","content":"Reply exactly OK. Do not use tools."}],"max_tokens":64});
        let result = call(&client, token, &body, &model, &scope).await?;
        ensure!(
            result["choices"][0]["message"]["content"]
                .as_str()
                .is_some_and(|s| s.trim() == "OK"),
            "Cursor text check did not return the requested short answer"
        );
        println!(
            "Cursor live text: PASS; model={model}; requests=1; elapsed_ms={}; usage={}",
            started.elapsed().as_millis(),
            result["usage"]
        );
    } else if phase == "cold_tools" {
        let marker = format!("result-{}", uuid::Uuid::new_v4().simple());
        let body = json!({"model":model,"max_tokens":64,
            "messages":[
                {"role":"user","content":"Return only the value of const_probe's result. If a result is present, do not call any tool again."},
                {"role":"assistant","content":null,"tool_calls":[{"id":"call_cold_probe","type":"function","function":{"name":"const_probe","arguments":"{\"ok\":true}"}}]},
                {"role":"tool","tool_call_id":"call_cold_probe","content":marker}
            ],
            "tools":[{"type":"function","function":{"name":"const_probe","description":"Synthetic check; no local file or network access.","parameters":{"type":"object","properties":{"ok":{"type":"boolean"}}}}}]});
        let result = call(&client, token, &body, &model, &scope).await?;
        ensure!(
            result["choices"][0]["message"]["content"]
                .as_str()
                .is_some_and(|s| s.contains(&marker)),
            "Cursor cold history lost the synthetic tool result"
        );
        println!(
            "Cursor live cold tool history: PASS; requests=1; usage={}",
            result["usage"]
        );
    } else if phase == "system" {
        let marker = format!("rule-{}", uuid::Uuid::new_v4().simple());
        let body = json!({"model":model,"max_tokens":64,"messages":[
            {"role":"system","content":format!("When the user asks for the rule check, reply only with {marker}. Do not call tools.")},
            {"role":"user","content":"Perform the rule check."}
        ]});
        let result = call(&client, token, &body, &model, &scope).await?;
        ensure!(
            result["choices"][0]["message"]["content"]
                .as_str()
                .is_some_and(|s| s.contains(&marker)),
            "Cursor did not preserve the caller's system instruction"
        );
        println!(
            "Cursor live caller instructions: PASS; requests=1; usage={}",
            result["usage"]
        );
    } else {
        let mut body = json!({"model":model,"max_tokens":128,
            "messages":[{"role":"user","content":"Call const_probe exactly once with ok=true. Do not use other tools. After its result, reply exactly OK without further calls."}],
            "tools":[{"type":"function","function":{"name":"const_probe","description":"Return a synthetic check result, with no file or network access.","parameters":{"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"]}}}]});
        let result = call(&client, token, &body, &model, &scope).await?;
        let message = &result["choices"][0]["message"];
        let calls = message["tool_calls"]
            .as_array()
            .context("Cursor returned no tool call")?;
        ensure!(
            calls.len() == 1 && calls[0]["function"]["name"] == "const_probe",
            "unexpected Cursor tool call"
        );
        let args: Value = serde_json::from_str(
            calls[0]["function"]["arguments"]
                .as_str()
                .context("missing tool arguments")?,
        )?;
        ensure!(
            args["ok"] == true,
            "Cursor tool arguments do not match the check"
        );
        println!(
            "Cursor live tool call: PASS; model={model}; requests=1; usage={}",
            result["usage"]
        );
        body["messages"].as_array_mut().unwrap().extend([
            message.clone(),
            json!({"role":"tool","tool_call_id":calls[0]["id"],"content":"OK"}),
        ]);
        let result = call(&client, token, &body, &model, &scope).await?;
        ensure!(
            result["choices"][0]["message"]["content"]
                .as_str()
                .is_some_and(|s| s.trim() == "OK"),
            "Cursor continuation did not return the requested short answer"
        );
        println!(
            "Cursor live continuation: PASS; model={model}; requests=2; elapsed_ms={}; usage={}",
            started.elapsed().as_millis(),
            result["usage"]
        );
    }
    Ok(())
}

#[tokio::test]
async fn cold_continuation_imports_native_history() {
    let (origin, runs, _, server) = mock("history").await;
    let prepared = request::prepare(
        ProtocolKind::OpenAiChat,
        &history_request(),
        "claude-test",
        "caller",
    )
    .unwrap();
    let client = h2_client();
    let (response, attempts) = crate::upstream_transport::with_probe_budget(
        1,
        runtime::start(&client, "synthetic-secret", &prepared, Some(&origin)),
    )
    .await;
    assert_eq!(attempts, 1);
    let mut response = response.unwrap();
    let mut finished = false;
    while let Some(event) = tokio::time::timeout(Duration::from_secs(5), response.recv())
        .await
        .unwrap()
    {
        if matches!(event.unwrap(), Event::ResponseDone(_)) {
            finished = true;
        }
    }
    assert!(finished);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn cursor_catalog_is_metadata_only_and_never_invents_models_or_context() {
    let (origin, runs, _, server) = mock("tools").await;
    let models = catalog_at(
        &h2_client(),
        "synthetic-secret",
        &format!("{origin}/agent.v1.AgentService/GetUsableModels"),
    )
    .await
    .unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "claude-live");
    assert!(models[0].context_tokens.is_none());
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn cursor_tools_resume_the_same_h2_stream_and_preserve_kv_handshakes() {
    let (origin, runs, _, server) = mock("tools").await;
    let client = h2_client();
    let mut body = chat();
    let mut another_tool = body["tools"][0].clone();
    another_tool["function"]["name"] = json!("alpha");
    body["tools"].as_array_mut().unwrap().push(another_tool);
    let prepared = request::prepare(
        ProtocolKind::OpenAiChat,
        &body,
        "claude-test",
        "isolated-caller",
    )
    .unwrap();
    let mut first = runtime::start(&client, "synthetic-secret", &prepared, Some(&origin))
        .await
        .unwrap();
    let mut call_id = String::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), first.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        match event {
            Event::BlockStart { block, .. } if block.call_id.is_some() => {
                call_id = block.call_id.unwrap()
            }
            Event::ResponseDone(finish) => {
                assert_eq!(finish.reason, crate::protocol::ir::FinishReason::ToolCalls);
                break;
            }
            _ => {}
        }
    }
    drop(first);
    // Enumeration order is not a semantic tool change. It must not force a
    // second native run or churn the prompt/context presented to Cursor.
    body["tools"].as_array_mut().unwrap().reverse();
    let reordered = request::prepare(
        ProtocolKind::OpenAiChat,
        &body,
        "claude-test",
        "isolated-caller",
    )
    .unwrap();
    assert_eq!(prepared.scope, reordered.scope);
    assert_eq!(prepared.run, reordered.run);
    assert_eq!(prepared.blobs, reordered.blobs);
    body["messages"].as_array_mut().unwrap().extend([
        json!({"role":"assistant","tool_calls":[{"type":"function","id":call_id,"function":{"name":"read","arguments":"{\"path\":\"file\"}"}}]}),
        json!({"role":"tool","tool_call_id":call_id,"content":"file content"}),
    ]);
    let prepared = request::prepare(
        ProtocolKind::OpenAiChat,
        &body,
        "claude-test",
        "isolated-caller",
    )
    .unwrap();
    let mut next = runtime::start(&client, "synthetic-secret", &prepared, Some(&origin))
        .await
        .unwrap();
    let mut text = String::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), next.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        match event {
            Event::TextDelta { text: part, .. } => text.push_str(&part),
            Event::UsageUpdate(_) => {
                panic!("cumulative Cursor turn usage leaked into a tool continuation")
            }
            Event::ResponseDone(_) => break,
            _ => {}
        }
    }
    assert_eq!(text, "Done");
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn cursor_cancellation_resets_upload_and_preserves_payment_error() {
    let (origin, _, mut closed, server) = mock("cancel").await;
    let prepared = request::prepare(
        ProtocolKind::OpenAiChat,
        &chat(),
        "claude-test",
        "cancel-caller",
    )
    .unwrap();
    let mut events = runtime::start(&h2_client(), "synthetic-secret", &prepared, Some(&origin))
        .await
        .unwrap();
    assert!(matches!(
        events.recv().await.unwrap().unwrap(),
        Event::ResponseStart(_)
    ));
    drop(events);
    tokio::time::timeout(Duration::from_secs(5), closed.recv())
        .await
        .unwrap()
        .unwrap();
    server.abort();
    let (origin, _, _, server) = mock("forbidden").await;
    let error = runtime::start(&h2_client(), "synthetic-secret", &prepared, Some(&origin))
        .await
        .unwrap_err();
    assert_eq!(error.downcast_ref::<ApiError>().unwrap().status, 403);
    assert!(error.to_string().contains("paid plan"));
    assert!(!error.to_string().contains("synthetic-secret"));
    server.abort();
}

async fn tool_call(events: &mut mpsc::Receiver<Result<Event>>) -> String {
    let mut id = String::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
        {
            Event::BlockStart { block, .. } if block.call_id.is_some() => {
                id = block.call_id.unwrap()
            }
            Event::ResponseDone(finish) => {
                assert_eq!(finish.reason, crate::protocol::ir::FinishReason::ToolCalls);
                assert!(!id.is_empty());
                return id;
            }
            _ => {}
        }
    }
}

fn with_result(id: &str, content: &str) -> Value {
    let mut body = chat();
    body["messages"].as_array_mut().unwrap().extend([
        json!({"role":"assistant","tool_calls":[{"type":"function","id":id,"function":{"name":"read","arguments":"{\"path\":\"file\"}"}}]}),
        json!({"role":"tool","tool_call_id":id,"content":content}),
    ]);
    body
}

async fn completed_text(events: &mut mpsc::Receiver<Result<Event>>) -> String {
    let mut text = String::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
        {
            Event::TextDelta { text: part, .. } => text.push_str(&part),
            Event::ResponseDone(_) => return text,
            _ => {}
        }
    }
}

#[tokio::test]
async fn four_concurrent_callers_resume_without_extra_runs_or_shared_tool_results() {
    let (origin, runs, _, server) = mock("isolated_tools").await;
    let client = h2_client();
    futures_util::future::join_all((0..4).map(|i| {
        let client = &client;
        let origin = &origin;
        async move {
            let caller = format!("concurrent-{i}");
            let prepared =
                request::prepare(ProtocolKind::OpenAiChat, &chat(), "claude-test", &caller)
                    .unwrap();
            let mut events = runtime::start(client, "synthetic-account", &prepared, Some(origin))
                .await
                .unwrap();
            let id = tool_call(&mut events).await;
            drop(events);
            let result = format!("file content {i}");
            let prepared = request::prepare(
                ProtocolKind::OpenAiChat,
                &with_result(&id, &result),
                "claude-test",
                &caller,
            )
            .unwrap();
            let mut events = runtime::start(client, "synthetic-account", &prepared, Some(origin))
                .await
                .unwrap();
            assert_eq!(completed_text(&mut events).await, result);
        }
    }))
    .await;
    assert_eq!(runs.load(Ordering::SeqCst), 4);
    server.abort();
}

#[tokio::test]
async fn a_different_caller_or_account_cannot_take_a_pending_tool_connection() {
    let (origin, runs, _, server) = mock("tools").await;
    let (cold_origin, cold_runs, _, cold_server) = mock("text").await;
    let client = h2_client();
    for (foreign_caller, foreign_token) in [
        ("foreign-caller", "original-account"),
        ("original-caller", "foreign-account"),
    ] {
        let prepared = request::prepare(
            ProtocolKind::OpenAiChat,
            &chat(),
            "claude-test",
            "original-caller",
        )
        .unwrap();
        let mut first = runtime::start(&client, "original-account", &prepared, Some(&origin))
            .await
            .unwrap();
        let id = tool_call(&mut first).await;
        drop(first);
        let body = with_result(&id, "file content");
        let foreign = request::prepare(
            ProtocolKind::OpenAiChat,
            &body,
            "claude-test",
            foreign_caller,
        )
        .unwrap();
        let mut events = runtime::start(&client, foreign_token, &foreign, Some(&cold_origin))
            .await
            .unwrap();
        assert_eq!(completed_text(&mut events).await, "hello world");
        let owner = request::prepare(
            ProtocolKind::OpenAiChat,
            &body,
            "claude-test",
            "original-caller",
        )
        .unwrap();
        let mut events = runtime::start(&client, "original-account", &owner, Some(&origin))
            .await
            .unwrap();
        assert_eq!(completed_text(&mut events).await, "Done");
    }
    assert_eq!(runs.load(Ordering::SeqCst), 2);
    assert_eq!(cold_runs.load(Ordering::SeqCst), 2);
    server.abort();
    cold_server.abort();
}

fn test_supplier(directory: &std::path::Path) -> crate::SupplierConfig {
    let mut channel = crate::detection::save_subscription_token_and_import_in_dir(
        "cursor",
        json!({"accessToken":"synthetic-access","refreshToken":"synthetic-refresh"}),
        directory,
    )
    .unwrap();
    channel.enabled = true;
    channel.models = vec!["claude-test".into()];
    channel.public_model = "claude-test".into();
    channel.upstream_model = "claude-test".into();
    channel.subscription.audit_enabled = false;
    crate::supplier_from_channel_unfiltered(&channel)
}

#[test]
fn cursor_projects_executable_platform_operations_without_claiming_native_fidelity() {
    let directory = tempfile::tempdir().unwrap();
    let mut supplier = test_supplier(directory.path());
    supplier.capability_profiles = vec![crate::model::ChannelCapabilityProfile {
        protocol: "anthropic_messages".into(),
        non_stream_json: true,
        stream_sse: true,
        tool_calls: true,
        verification_state: "declared".into(),
        capability_layer: "driver".into(),
        release_status: "supported".into(),
        ..Default::default()
    }];
    let agent = crate::supplier::SupplierAgentConfig::single(supplier.clone());
    let payload = crate::supplier::supplier_register_payload(&agent, &supplier);
    assert_eq!(payload["provider_kind"], "cursor");
    assert_eq!(payload["endpoint_kind"], "subscription");
    assert_eq!(payload["models"], json!(["claude-test"]));
    assert_eq!(
        crate::detection::subscription_supported_protocols("cursor"),
        ["anthropic_messages"]
    );
    for protocol in [
        "anthropic_messages",
        "openai_chat",
        "openai_responses",
        "gemini_native",
    ] {
        assert!(
            payload["effective_capabilities"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["protocol"] == protocol
                    && item["feature"] == "tool_calls"
                    && item["state"] == "supported"
                    && item["conversion_level"] == "compatible"),
            "{protocol}"
        );
    }
}

#[tokio::test]
async fn cursor_serves_all_four_protocols_in_buffered_and_platform_stream_envelopes() {
    for mode in ["canonical", "metered", "zero_usage", "partial_usage"] {
        check_cursor_usage_envelopes(mode).await;
    }
}

async fn check_cursor_usage_envelopes(mode: &'static str) {
    let (origin, runs, _, server) = mock(mode).await;
    let directory = tempfile::tempdir().unwrap();
    let mut channel =
        crate::channel_from_supplier("cursor-canonical".into(), &test_supplier(directory.path()));
    channel.models = vec![
        "claude-haiku-5-5-high".into(),
        "claude-haiku-5-5-medium".into(),
    ];
    channel.public_model = channel.models[0].clone();
    channel.upstream_model = channel.models[0].clone();
    let channel = crate::config::normalize_channel(channel, 0, None, None);
    let supplier = crate::supplier_from_channel_unfiltered(&channel);
    assert_eq!(supplier.models, ["claude-haiku-5-5"]);
    for (path, body) in [
        (
            "/v1/messages",
            json!({"model":"claude-haiku-5-5","max_tokens":32,"messages":[{"role":"user","content":"hello"}]}),
        ),
        (
            "/v1/chat/completions",
            json!({"model":"claude-haiku-5-5","messages":[{"role":"user","content":"hello"}]}),
        ),
        (
            "/v1/responses",
            json!({"model":"claude-haiku-5-5","input":"hello"}),
        ),
        (
            "/v1beta/models/claude-haiku-5-5:generateContent",
            json!({"model":"claude-haiku-5-5","contents":[{"role":"user","parts":[{"text":"hello"}]}]}),
        ),
    ] {
        let body = body.to_string();
        let payload = crate::supplier::forward_cursor_subscription_request(
            &supplier,
            path,
            &body,
            None,
            Some(&origin),
            None,
            Some("caller"),
        )
        .await
        .unwrap()
        .unwrap();
        let wire = crate::surface_wire::SurfaceEnvelope::from_payload(&payload).unwrap();
        assert_eq!(wire.status, Some(200));
        assert!(wire.body.utf8().unwrap().contains("hello world"));
        assert!(wire.body.utf8().unwrap().contains("claude-haiku-5-5"));
        assert!(
            !wire
                .body
                .utf8()
                .unwrap()
                .contains("claude-haiku-5-5-medium")
        );
        assert_eq!(
            wire.body.utf8().unwrap().contains("const_api_usage_source"),
            matches!(mode, "canonical" | "partial_usage")
        );
        assert_cursor_reported_usage(&payload, mode);
        let value: Value = serde_json::from_str(wire.body.utf8().unwrap()).unwrap();
        if mode == "metered" {
            let (usage, input_field, input) = if path == "/v1/messages" {
                (&value["usage"], "input_tokens", 100)
            } else if path == "/v1/chat/completions" {
                (&value["usage"], "prompt_tokens", 200)
            } else if path == "/v1/responses" {
                (&value["usage"], "input_tokens", 200)
            } else {
                (&value["usageMetadata"], "promptTokenCount", 200)
            };
            assert_eq!(usage[input_field], input, "{path}: {usage}");
        }
        let (tx, mut rx) = mpsc::channel(128);
        assert!(
            crate::supplier::forward_cursor_subscription_request(
                &supplier,
                path,
                &body,
                Some((&tx, "cursor-test")),
                Some(&origin),
                None,
                Some("caller")
            )
            .await
            .unwrap()
            .is_none()
        );
        drop(tx);
        let mut bytes = Vec::new();
        let mut ended = false;
        while let Some(message) = rx.recv().await {
            assert_ne!(message.kind, "error");
            if message.kind == "stream_chunk" {
                bytes.extend(
                    crate::surface_wire::SurfaceEnvelope::from_payload(&message.payload)
                        .unwrap()
                        .body
                        .decode()
                        .unwrap(),
                );
            }
            if message.kind == "stream_end" {
                assert_cursor_reported_usage(&message.payload, mode);
            }
            ended |= message.kind == "stream_end";
        }
        assert!(ended);
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("hello world"));
        if matches!(mode, "canonical" | "partial_usage") {
            assert!(text.contains("const_api_usage_source"));
        }
        assert!(!text.contains("claude-haiku-5-5-medium"));
        if mode == "metered" {
            use crate::protocol::stream::{CanonicalAccumulator, StreamParser};
            let protocol = match path {
                "/v1/messages" => ProtocolKind::AnthropicMessages,
                "/v1/chat/completions" => ProtocolKind::OpenAiChat,
                "/v1/responses" => ProtocolKind::OpenAiResponses,
                _ => ProtocolKind::GeminiNative,
            };
            let mut parser = StreamParser::new(protocol, "claude-haiku-5-5");
            let mut accumulator = CanonicalAccumulator::new();
            for event in parser
                .push(text.as_bytes())
                .unwrap()
                .into_iter()
                .chain(parser.finish().unwrap())
            {
                accumulator.push(&event).unwrap();
            }
            let usage = accumulator.finish().unwrap().usage;
            assert_eq!(
                usage.input_tokens.value,
                Some(if protocol == ProtocolKind::AnthropicMessages {
                    100
                } else {
                    200
                }),
                "{path}"
            );
            assert_eq!(usage.output_tokens.value, Some(8), "{path}");
            assert_eq!(usage.reasoning_tokens.value, Some(3), "{path}");
            assert_eq!(usage.cache_read_tokens.value, Some(80), "{path}");
            if protocol != ProtocolKind::GeminiNative {
                // Gemini has no native cache-write field; the platform sidecar
                // above still carries it to the shared billing meters.
                assert_eq!(usage.cache_write_tokens.value, Some(20), "{path}");
            }
        }
    }
    assert_eq!(runs.load(Ordering::SeqCst), 8);
    server.abort();
}

fn assert_cursor_reported_usage(payload: &serde_json::Map<String, Value>, mode: &str) {
    let usage = &payload["upstream_usage"];
    assert_eq!(usage["source_protocol"], "cursor_agent");
    assert_eq!(usage["speed"], "standard");
    assert_eq!(usage["input_tokens_include_cache_read"], true);
    assert_eq!(usage["input_tokens_include_cache_write"], true);
    match mode {
        "metered" => {
            assert_eq!(usage["input_tokens"], 200);
            assert_eq!(usage["output_tokens"], 8);
            assert_eq!(usage["cache_read_tokens"], 80);
            assert_eq!(usage["cache_write_tokens"], 20);
        }
        "partial_usage" => {
            assert_eq!(usage["input_tokens"], 100);
            assert!(usage["output_tokens"].is_null());
        }
        "zero_usage" => {
            assert_eq!(usage["input_tokens"], 0);
            assert_eq!(usage["output_tokens"], 0);
        }
        _ => {
            assert!(usage["input_tokens"].is_null());
            assert!(usage["output_tokens"].is_null());
        }
    }
}

#[tokio::test]
async fn connect_rejection_keeps_403_and_late_failure_never_becomes_success() {
    let directory = tempfile::tempdir().unwrap();
    let supplier = test_supplier(directory.path());
    for mode in ["connect_denied", "late_error"] {
        let (origin, runs, _, server) = mock(mode).await;
        let (tx, mut rx) = mpsc::channel(128);
        let result = crate::supplier::forward_cursor_subscription_request(&supplier, "/v1/messages",
            &json!({"model":"claude-test","max_tokens":32,"messages":[{"role":"user","content":"hello"}]}).to_string(),
            Some((&tx, "cursor-error-test")), Some(&origin), None, Some("caller")).await.unwrap();
        drop(tx);
        if mode == "connect_denied" {
            let wire =
                crate::surface_wire::SurfaceEnvelope::from_payload(&result.unwrap()).unwrap();
            assert_eq!(wire.status, Some(403));
            assert!(rx.recv().await.is_none());
        } else {
            assert!(result.is_none());
            let mut failed = false;
            while let Some(message) = rx.recv().await {
                assert_ne!(message.kind, "stream_end");
                if message.kind == "error" {
                    failed = true;
                    assert_eq!(
                        crate::surface_wire::SurfaceEnvelope::from_payload(&message.payload)
                            .unwrap()
                            .status,
                        Some(403)
                    );
                }
            }
            assert!(failed);
        }
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        server.abort();
    }
}
