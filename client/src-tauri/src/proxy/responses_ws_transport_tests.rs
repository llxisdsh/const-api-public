use super::*;
use crate::proxy::*;
use futures_util::{SinkExt, StreamExt};
use registry::client::IntoClientRequest;
use std::{
    process::{Command, Stdio},
    sync::atomic::Ordering,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

const CHILD: &str = "proxy::responses_ws_transport::tests::production_transport_child";

fn child_command() -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["--ignored", "--exact", CHILD, "--nocapture"]);
    for name in [
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "no_proxy",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "CODEX_CA_CERTIFICATE",
        "SSL_CERT_FILE",
    ] {
        command.env_remove(name);
    }
    command.env_remove("CONST_TEST_CODEX_WS_DEST");
    command.env_remove("CONST_TEST_CODEX_DIRECT");
    command.env_remove("REQUEST_METHOD");
    command.env_remove("CONST_TEST_CODEX_WS_LIVE");
    command.env_remove("CONST_TEST_CODEX_COOKIES");
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    command
}

async fn finish_child(mut child: std::process::Child) {
    let completed = tokio::time::timeout(Duration::from_secs(30), async {
        while child.try_wait().unwrap().is_none() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    if completed.is_err() {
        let _ = child.kill();
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        completed.is_ok() && output.status.success(),
        "local transport child failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn production_codex_transport_pool_and_error_contracts() {
    // Environment belongs to the fresh child, never the parallel Rust test process.
    finish_child(child_command().env("NO_PROXY", "*").spawn().unwrap()).await;
}

async fn head(socket: &mut (impl tokio::io::AsyncRead + Unpin)) -> String {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        assert!(bytes.len() < 16 * 1024);
        bytes.push(socket.read_u8().await.unwrap());
    }
    String::from_utf8(bytes).unwrap()
}

#[tokio::test]
async fn production_codex_connector_honors_proxy_and_no_proxy() {
    for live in [false, true] {
        for mode in ["http-proxy", "https-proxy", "no-proxy", "direct"] {
            let bypass = matches!(mode, "no-proxy" | "direct");
            let secure_proxy = mode == "https-proxy";
            let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
            let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::aws_lc_rs::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.cert.der().clone()],
                rustls::pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der())
                    .into(),
            )
            .unwrap();
            let dir = tempfile::tempdir().unwrap();
            let ca = dir.path().join("proxy-ca.pem");
            std::fs::write(&ca, cert.cert.pem()).unwrap();
            let direct = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let dest = direct.local_addr().unwrap();
            let mut command = child_command();
            command
                .env(
                    "HTTP_PROXY",
                    format!(
                        "{}://fixture:secret@{}",
                        if secure_proxy { "https" } else { "http" },
                        proxy.local_addr().unwrap()
                    ),
                )
                .env("CONST_TEST_CODEX_WS_DEST", format!("ws://{dest}/responses"));
            if live {
                command.env("CONST_TEST_CODEX_WS_LIVE", "1");
            }
            // Explicit no-proxy value avoids inheriting the host OS's loopback bypass.
            command.env(
                "NO_PROXY",
                if mode == "no-proxy" {
                    "127.0.0.1"
                } else {
                    "not-this-host.invalid"
                },
            );
            if mode == "direct" {
                command.env("CONST_TEST_CODEX_DIRECT", "1");
            }
            if secure_proxy {
                command.env("CODEX_CA_CERTIFICATE", &ca);
            }
            let child = command.spawn().unwrap();
            let peer = tokio::time::timeout(Duration::from_secs(10), async {
                let (stream, via_proxy) = tokio::select! {
                    result = direct.accept() => (result.unwrap().0, false),
                    result = proxy.accept() => (result.unwrap().0, true),
                };
                assert_eq!(via_proxy, !bypass, "{mode}");
                let mut stream: crate::openai_transport::dialer::Socket =
                    if secure_proxy && via_proxy {
                        Box::new(
                            tokio_rustls::TlsAcceptor::from(Arc::new(tls))
                                .accept(stream)
                                .await
                                .unwrap(),
                        )
                    } else {
                        Box::new(stream)
                    };
                if via_proxy {
                    let connect = head(&mut stream).await;
                    assert!(connect.starts_with(&format!("CONNECT {dest} HTTP/1.1")));
                    assert!(
                        connect
                            .to_ascii_lowercase()
                            .contains("proxy-authorization: basic ")
                    );
                    stream
                        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                        .await
                        .unwrap();
                }
                let mut ws = registry_ws::accept_hdr_async(
                    stream,
                    |request: &registry::handshake::server::Request, response| {
                        assert_eq!(
                            request
                                .headers()
                                .get("sec-websocket-extensions")
                                .map(|v| v.to_str().unwrap()),
                            (!live).then_some("permessage-deflate; client_max_window_bits")
                        );
                        assert!(!request.headers().contains_key("proxy-authorization"));
                        Ok(response)
                    },
                )
                .await
                .unwrap();
                assert!(ws.next().await.unwrap().unwrap().is_close());
            })
            .await;
            finish_child(child).await;
            peer.expect("local proxy handshake");
        }
    }
}

#[tokio::test]
async fn production_generic_channel_ws_proxy_is_opt_in() {
    for mode in ["direct", "proxy", "no-proxy"] {
        let direct = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dest = direct.local_addr().unwrap();
        let mut command = child_command();
        command
            .env("CONST_TEST_CHANNEL_WS", mode)
            .env(
                "CONST_TEST_CODEX_WS_DEST",
                if mode == "proxy" {
                    "ws://channel.invalid/responses".to_string()
                } else {
                    format!("ws://{dest}/responses")
                },
            )
            .env(
                "HTTP_PROXY",
                format!("http://{}", proxy.local_addr().unwrap()),
            );
        if mode == "no-proxy" {
            command.env("NO_PROXY", "127.0.0.1");
        }
        let child = command.spawn().unwrap();
        let peer = tokio::time::timeout(Duration::from_secs(20), async {
            let listener = if mode == "proxy" { &proxy } else { &direct };
            let (mut socket, _) = listener.accept().await.unwrap();
            if mode == "proxy" {
                assert!(
                    head(&mut socket)
                        .await
                        .starts_with("CONNECT channel.invalid:80 ")
                );
                socket
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .await
                    .unwrap();
            }
            let mut ws = registry_ws::accept_hdr_async(
                socket,
                |request: &registry::handshake::server::Request, response| {
                    assert!(!request.headers().contains_key("sec-websocket-extensions"));
                    Ok(response)
                },
            )
            .await
            .unwrap();
            assert!(ws.next().await.unwrap().unwrap().is_close());
        })
        .await;
        finish_child(child).await;
        peer.unwrap_or_else(|_| panic!("generic channel WS handshake: {mode}"));
    }
}

#[tokio::test]
#[ignore = "isolated local transport child, executed by the parent tests"]
async fn production_transport_child() {
    tokio::time::timeout(Duration::from_secs(20), async {
        if let Ok(mode) = std::env::var("CONST_TEST_CHANNEL_WS") {
            let mut request = std::env::var("CONST_TEST_CODEX_WS_DEST")
                .unwrap()
                .into_client_request()
                .unwrap();
            request
                .extensions_mut()
                .insert(crate::http_client::ChannelProxy(mode != "direct"));
            let mut socket = connect_responses_websocket_request(request).await.unwrap();
            socket.close(None).await.unwrap();
            return;
        }
        if let Ok(mode) = std::env::var("CONST_TEST_CODEX_COOKIES") {
            if mode == "invalid-ca" {
                let url = "https://chatgpt.com/backend-api/codex/responses";
                let credential = serde_json::json!({"type":"openai", "account_id":"invalid-ca"});
                let error =
                    crate::openai_transport::http_client(&reqwest::Client::new(), url, &credential)
                        .unwrap_err();
                assert!(format!("{error:#}").contains("CODEX_CA_CERTIFICATE"));
                let request = openai_subscription_websocket_request(
                    url,
                    "fixture",
                    &credential,
                    &Default::default(),
                )
                .unwrap();
                let error = connect_codex_responses_websocket_request(request)
                    .await
                    .unwrap_err();
                assert!(format!("{error:#}").contains("CODEX_CA_CERTIFICATE"));
            } else {
                subscription_cookie_ca_child().await;
            }
            return;
        }
        if let Ok(dest) = std::env::var("CONST_TEST_CODEX_WS_DEST") {
            let live = std::env::var_os("CONST_TEST_CODEX_WS_LIVE").is_some();
            let mut request = dest.into_client_request().unwrap();
            request
                .extensions_mut()
                .insert(crate::openai_transport::TransportOptions {
                    use_system_proxy: std::env::var_os("CONST_TEST_CODEX_DIRECT").is_none(),
                    ..Default::default()
                });
            let mut socket = subscription_connect(request, live).await.unwrap();
            socket.close(None).await.unwrap();
            return;
        }
        for compression in [false, true] {
            pool_two_turns_and_idle_close(compression).await;
        }
        uncompressed_paths_keep_their_handshake().await;
        handshake_rejections_keep_metadata().await;
        for live in [false, true] {
            adapter_split_preserves_control_and_binary_frames(live).await;
            cancelled_connect_releases_socket(live).await;
        }
        compressed_limit_and_untrusted_tls_remain_errors().await;
        responses_websocket_tests::subscription_https_provider_is_stable_after_quic_setup().await;
    })
    .await
    .expect("production transport suite timeout");
}

#[tokio::test]
async fn production_subscription_http_ws_share_account_cookies_and_custom_ca() {
    let cert =
        rcgen::generate_simple_self_signed(vec!["chatgpt.com".into(), "api.openai.com".into()])
            .unwrap();
    let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![cert.cert.der().clone()],
        rustls::pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()).into(),
    )
    .unwrap();
    tls.alpn_protocols = vec![b"http/1.1".to_vec()];
    let tls = Arc::new(tls);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fixture-ca.pem");
    std::fs::write(&path, cert.cert.pem()).unwrap();
    let other_path = dir.path().join("other-ca.pem");
    std::fs::write(
        &other_path,
        rcgen::generate_simple_self_signed(vec!["unrelated.test".into()])
            .unwrap()
            .cert
            .pem(),
    )
    .unwrap();
    for selected in ["CODEX_CA_CERTIFICATE", "SSL_CERT_FILE"] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut command = child_command();
        command
            .env(
                "HTTPS_PROXY",
                format!("http://{}", listener.local_addr().unwrap()),
            )
            .env("CONST_TEST_CODEX_COOKIES", "1")
            .env(selected, &path);
        if selected == "CODEX_CA_CERTIFICATE" {
            command.env("SSL_CERT_FILE", &other_path);
        }
        let child = command.spawn().unwrap();
        let peer = tokio::time::timeout(Duration::from_secs(20), async {
            for index in 0..11 {
                let (mut tcp, _) = listener.accept().await.unwrap();
                let connect = head(&mut tcp).await;
                let host = if index >= 9 { "chat.openai.com" } else if index == 6 { "api.openai.com" } else { "chatgpt.com" };
                assert!(connect.starts_with(&format!("CONNECT {host}:443 HTTP/1.1")));
                tcp.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await.unwrap();
                let handshake = tokio_rustls::LazyConfigAcceptor::new(rustls::server::Acceptor::default(), tcp).await.unwrap();
                let alpn: Vec<_> = handshake.client_hello().alpn().into_iter().flatten().map(|value| value.to_vec()).collect();
                let websocket = matches!(index, 1 | 2 | 6 | 10);
                assert_eq!(alpn, if websocket { vec![] } else { vec![b"h2".to_vec(), b"http/1.1".to_vec()] });
                let stream = handshake.into_stream(tls.clone()).await;
                if index >= 9 {
                    assert!(stream.is_err(), "custom CA must not disable hostname validation");
                    continue;
                }
                let mut stream = stream.unwrap();
                if matches!(index, 1 | 2 | 6) {
                    assert!(stream.get_ref().1.alpn_protocol().is_none(), "WS must not advertise h2");
                    let result = registry_ws::accept_hdr_async(stream,
                        |request: &registry::handshake::server::Request, mut response: registry::handshake::server::Response| {
                            let expected = match index { 1 => Some("__oailb=http"), 2 => Some("__oailb=rejected"), _ => None };
                            assert_eq!(request.headers().get("cookie").map(|v| v.to_str().unwrap()), expected);
                            assert_eq!(request.headers().contains_key("sec-websocket-extensions"), index != 6);
                            if index == 1 {
                                let rejected = http::Response::builder().status(401)
                                    .header("set-cookie", "__oailb=rejected; Secure; Path=/")
                                    .header("content-length", "0").body(None).unwrap();
                                return Err(rejected);
                            }
                            response.headers_mut().append("set-cookie", "__oailb=ws; Secure; Path=/".parse().unwrap());
                            Ok(response)
                        }).await;
                    if index == 1 { assert!(result.is_err()); }
                    else { assert!(result.unwrap().next().await.unwrap().unwrap().is_close()); }
                } else {
                    assert_eq!(stream.get_ref().1.alpn_protocol(), Some(b"http/1.1".as_slice()));
                    let request = head(&mut stream).await;
                    let cookie = request.lines().find_map(|line| line.split_once(':').filter(|(name, _)| name.eq_ignore_ascii_case("cookie")).map(|(_, v)| v.trim()));
                    let expected = match index { 3 => Some("__oailb=ws"), 5 => Some("admin=explicit"), 7 | 8 => Some("__oailb=http"), _ => None };
                    assert_eq!(cookie, expected);
                    if index == 7 || index == 8 {
                        let path = if index == 7 { "/backend-api/codex/images/generations" } else { "/backend-api/codex/realtime/calls?" };
                        assert!(request.starts_with(&format!("POST {path}")));
                        let length: usize = request.lines().find_map(|line| line.split_once(':').filter(|(name, _)| name.eq_ignore_ascii_case("content-length")).map(|(_, value)| value.trim().parse().unwrap())).unwrap();
                        stream.read_exact(&mut vec![0; length]).await.unwrap();
                    }
                    stream.write_all(b"HTTP/1.1 200 OK\r\nSet-Cookie: __oailb=http; Secure; Path=/\r\nSet-Cookie: session=never-store; Secure; Path=/\r\nConnection: close\r\nContent-Length: 2\r\n\r\nok").await.unwrap();
                    stream.shutdown().await.unwrap();
                }
            }
        }).await;
        finish_child(child).await;
        peer.expect("local cookie/CA peer timeout");
    }
    // A broken higher-priority bundle must fail both transports before dialing,
    // even though SSL_CERT_FILE points to a valid lower-priority bundle.
    finish_child(
        child_command()
            .env("CONST_TEST_CODEX_COOKIES", "invalid-ca")
            .env("CODEX_CA_CERTIFICATE", dir.path().join("missing.pem"))
            .env("SSL_CERT_FILE", &path)
            .spawn()
            .unwrap(),
    )
    .await;
}

async fn subscription_cookie_ca_child() {
    let baseline = reqwest::Client::new();
    let options = crate::openai_transport::TransportOptions {
        use_system_proxy: true,
        ..Default::default()
    };
    let credential =
        serde_json::json!({"type":"openai", "account_id":"one", "access_token":"fixture"});
    let url = "https://chatgpt.com/backend-api/codex/responses";
    let client =
        crate::openai_transport::http_client_with_options(&baseline, url, &credential, options)
            .unwrap();
    assert_eq!(
        client.get(url).send().await.unwrap().text().await.unwrap(),
        "ok"
    );
    for rejected in [true, false] {
        let request = openai_subscription_websocket_request_with_options(
            url,
            "fixture",
            &credential,
            &Default::default(),
            options,
        )
        .unwrap();
        let result = connect_codex_responses_websocket_request(request).await;
        if rejected {
            assert_eq!(
                responses_websocket_error_status(&result.unwrap_err()),
                Some(401)
            );
        } else {
            result.unwrap().close(None).await.unwrap();
        }
    }
    // WS cookie is visible to an already-built HTTP client, even after its pool
    // is recreated by the existing network recovery mechanism.
    crate::http_client::network_changed();
    let client =
        crate::openai_transport::http_client_with_options(&baseline, url, &credential, options)
            .unwrap();
    assert_eq!(
        client.get(url).send().await.unwrap().text().await.unwrap(),
        "ok"
    );
    let second = crate::openai_transport::http_client_with_options(
        &baseline,
        url,
        &serde_json::json!({"type":"openai", "account_id":"two"}),
        options,
    )
    .unwrap();
    assert_eq!(
        second.get(url).send().await.unwrap().text().await.unwrap(),
        "ok"
    );
    assert_eq!(
        client
            .get(url)
            .header("cookie", "admin=explicit")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "ok"
    );
    let request = openai_subscription_live_websocket_request(
        None,
        "",
        "fixture",
        &credential,
        &Default::default(),
        options,
    )
    .unwrap();
    connect_codex_live_websocket_request(request)
        .await
        .unwrap()
        .close(None)
        .await
        .unwrap();
    // Exercise the real forwarding boundaries, not just the jar: neither media
    // route may pass the upstream's infrastructure or session cookies downstream.
    let dir = tempfile::tempdir().unwrap();
    let credential_path = dir.path().join("openai.json");
    let mut saved = credential.clone();
    saved["expired"] = (crate::now_unix() + 3600).into();
    std::fs::write(&credential_path, serde_json::to_vec(&saved).unwrap()).unwrap();
    let mut channel = crate::config::default_config().channels.remove(0);
    channel.set_source_driver(crate::source_driver::SourceDriverId::OpenAiSubscription);
    channel.v2.credential_ref = credential_path.to_string_lossy().into_owned();
    channel.subscription.credential_ref = channel.v2.credential_ref.clone();
    channel.subscription.platform = "openai".to_string();
    channel.v2.use_system_proxy = true;
    let mut headers = warp::http::HeaderMap::new();
    headers.insert("content-type", "application/json".parse().unwrap());
    headers.insert("cookie", "caller=must-not-forward".parse().unwrap());
    let image = forward_openai_subscription_image_request(
        &baseline,
        &channel,
        "/v1/images/generations",
        &headers,
        bytes::Bytes::from_static(br#"{"model":"fixture","prompt":"test"}"#),
    )
    .await
    .unwrap();
    assert!(!image.headers().contains_key("set-cookie"));
    assert_eq!(
        crate::test_body_bytes(image.into_body())
            .await
            .unwrap()
            .as_ref(),
        b"ok"
    );
    let realtime = forward_openai_subscription_realtime_call(
        &baseline,
        &channel,
        &headers,
        bytes::Bytes::from_static(br#"{"sdp":"v=0","session":{}}"#),
        true,
    )
    .await
    .unwrap();
    assert!(!realtime.headers().contains_key("set-cookie"));
    assert_eq!(
        crate::test_body_bytes(realtime.into_body())
            .await
            .unwrap()
            .as_ref(),
        b"ok"
    );
    let wrong_host = "https://chat.openai.com/backend-api/codex/responses";
    assert!(
        client
            .get(wrong_host)
            .send()
            .await
            .unwrap_err()
            .is_connect()
    );
    let request = openai_subscription_websocket_request_with_options(
        wrong_host,
        "fixture",
        &credential,
        &Default::default(),
        options,
    )
    .unwrap();
    assert_eq!(
        connect_codex_responses_websocket_request(request)
            .await
            .unwrap_err()
            .downcast_ref::<ResponsesWsConnectFailure>()
            .unwrap()
            .cause,
        "tls_error"
    );
}

async fn subscription_connect(
    request: http::Request<()>,
    live: bool,
) -> Result<ResponsesUpstreamSocket> {
    if live {
        connect_codex_live_websocket_request(request).await
    } else {
        connect_codex_responses_websocket_request(request).await
    }
}

fn pool_bucket() -> Arc<ResponsesWsPoolBucket> {
    let channel = channel_from_supplier("compression-test".into(), &default_supplier_config());
    let context = ResponsesWsConnectContext {
        client: reqwest::Client::new(),
        route: ResponsesWebsocketNativeRoute::OpenAiSubscription {
            channel: Box::new(channel.clone()),
        },
        headers: Default::default(),
        channel_id: channel.id,
        request_source: "test".into(),
    };
    Arc::new(ResponsesWsPoolBucket {
        state: Default::default(),
        notify: Default::default(),
        capability: Arc::new(ResponsesWsCapabilityState {
            value: RESPONSES_WS_CAPABILITY_EXCLUSIVE.into(),
            notify: Default::default(),
            probe_lock: Default::default(),
        }),
        target_connections: 1.into(),
        hard_connections: 2.into(),
        exclusive_connections: 1.into(),
        open_connections: 0.into(),
        context,
    })
}

async fn pool_two_turns_and_idle_close(compression_accepted: bool) {
    let requests = [
        r#"{ "type":"response.create", "input":[{"role":"user","content":"工具 ✓"}] }"#,
        r#"{ "type":"response.create", "previous_response_id":"resp_fixture_0", "input":[{"type":"function_call_output","call_id":"call_fixture","output":"unchanged ✓"}] }"#,
    ];
    let events = [
        r#"{"type":"response.output_item.done","item":{"type":"function_call","call_id":"call_fixture","name":"read_file","arguments":"{}"}}"#,
        r#"{"type":"response.output_text.delta","delta":"done ✓"}"#,
    ];
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let peer = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        // Responses always offers compression. The server may decline it;
        // the same socket must still support both turns and pool retirement.
        let config = codex_config(RESPONSES_WEBSOCKET_MAX_MESSAGE_BYTES, compression_accepted);
        let mut socket = codex_ws::accept_hdr_async_with_config(
            stream,
            |request: &codex::handshake::server::Request,
             mut response: codex::handshake::server::Response| {
                response
                    .headers_mut()
                    .insert("x-codex-turn-state", "pool-opening".parse().unwrap());
                assert_eq!(
                    request.headers()["sec-websocket-extensions"],
                    "permessage-deflate; client_max_window_bits"
                );
                Ok(response)
            },
            Some(config),
        )
        .await
        .unwrap();
        for (index, request) in requests.into_iter().enumerate() {
            assert_eq!(
                socket.next().await.unwrap().unwrap().into_text().unwrap(),
                request
            );
            socket
                .send(codex::Message::Text(events[index].into()))
                .await
                .unwrap();
            socket.send(codex::Message::Text(format!(r#"{{"type":"response.completed","response":{{"id":"resp_fixture_{index}"}}}}"#).into())).await.unwrap();
        }
        assert!(
            socket.next().await.unwrap().unwrap().is_close(),
            "idle pool sends Close"
        );
    });
    let socket = connect_codex_responses_websocket_request(
        format!("ws://{address}/responses")
            .into_client_request()
            .unwrap(),
    )
    .await
    .unwrap();
    assert!(matches!(socket, ResponsesUpstreamSocket::Codex(_)));
    let bucket = pool_bucket();
    let connection = ResponsesWsPooledConnection::spawn(socket, &bucket, &bucket.context, false, 0);
    bucket
        .state
        .lock()
        .await
        .connections
        .push(connection.clone());
    connection.pin_session();
    for (index, request) in requests.into_iter().enumerate() {
        let mut lease = connection.try_allocate_for_session(false).unwrap();
        lease.start_turn(request.into()).await.unwrap();
        let mut turn = ResponsesWsPoolTurn {
            lease: Some(lease),
            stream_id: None,
        };
        if index == 0 {
            let text = turn.next().await.unwrap().unwrap().into_text().unwrap();
            let value: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(value["type"], "response.metadata");
            assert_eq!(value["headers"]["x-codex-turn-state"], "pool-opening");
        }
        assert_eq!(
            turn.next().await.unwrap().unwrap().into_text().unwrap(),
            events[index]
        );
        assert!(
            turn.next()
                .await
                .unwrap()
                .unwrap()
                .into_text()
                .unwrap()
                .contains("response.completed")
        );
        turn.finish();
    }
    assert_eq!(connection.turns_started.load(Ordering::Relaxed), 2);
    connection.unpin_session();
    peer.await.unwrap();
    while bucket.open_connections.load(Ordering::Relaxed) != 0 {
        tokio::task::yield_now().await;
    }
}

async fn uncompressed_paths_keep_their_handshake() {
    for subscription in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = registry_ws::accept_hdr_async(
                stream,
                |request: &registry::handshake::server::Request, response| {
                    assert!(!request.headers().contains_key("sec-websocket-extensions"));
                    Ok(response)
                },
            )
            .await
            .unwrap();
            assert!(socket.next().await.unwrap().unwrap().is_close());
        });
        let request = format!("ws://{address}/live")
            .into_client_request()
            .unwrap();
        let mut socket = if subscription {
            connect_codex_live_websocket_request(request).await
        } else {
            connect_responses_websocket_request(request).await
        }
        .unwrap();
        assert_eq!(
            matches!(socket, ResponsesUpstreamSocket::Codex(_)),
            subscription
        );
        socket.close(None).await.unwrap();
        peer.await.unwrap();
    }
}

async fn handshake_rejections_keep_metadata() {
    for live in [false, true] {
        for status in [401, 429] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let peer = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                head(&mut stream).await;
                let body =
                    r#"{"error":{"code":"fixture_rejection","type":"invalid_request_error"}}"#;
                stream.write_all(format!("HTTP/1.1 {status} Rejected\r\nContent-Length: {}\r\nX-Request-Id: fixture-id\r\nRetry-After: 7\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            });
            let error = subscription_connect(
                format!("ws://{address}/responses")
                    .into_client_request()
                    .unwrap(),
                live,
            )
            .await
            .unwrap_err();
            assert_eq!(responses_websocket_error_status(&error), Some(status));
            assert!(responses_websocket_connect_error_payload(&error).is_none());
            let metadata = responses_websocket_connect_error_metadata(&error);
            assert_eq!(metadata.error_code.as_deref(), Some("fixture_rejection"));
            assert_eq!(metadata.provider_request_id.as_deref(), Some("fixture-id"));
            assert_eq!(metadata.retry_after_seconds, Some(7));
            peer.await.unwrap();
        }
    }
}

async fn adapter_split_preserves_control_and_binary_frames(live: bool) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let peer = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket =
            codex_ws::accept_async_with_config(stream, Some(codex_config(1024, !live)))
                .await
                .unwrap();
        assert_eq!(
            socket.next().await.unwrap().unwrap(),
            codex::Message::Binary(vec![0, 255, 1].into())
        );
        socket
            .send(codex::Message::Ping(vec![7].into()))
            .await
            .unwrap();
        assert_eq!(
            socket.next().await.unwrap().unwrap(),
            codex::Message::Pong(vec![7].into())
        );
        socket
            .send(codex::Message::Close(Some(codex::protocol::CloseFrame {
                code: 1001.into(),
                reason: "fixture ✓".into(),
            })))
            .await
            .unwrap();
    });
    let socket = subscription_connect(
        format!("ws://{address}/responses")
            .into_client_request()
            .unwrap(),
        live,
    )
    .await
    .unwrap();
    let (mut sender, mut receiver) = socket.split();
    sender
        .send(registry::Message::Binary(vec![0, 255, 1].into()))
        .await
        .unwrap();
    assert_eq!(
        receiver.next().await.unwrap().unwrap(),
        registry::Message::Ping(vec![7].into())
    );
    sender
        .send(registry::Message::Pong(vec![7].into()))
        .await
        .unwrap();
    assert_eq!(
        receiver.next().await.unwrap().unwrap(),
        registry::Message::Close(Some(registry::protocol::CloseFrame {
            code: 1001.into(),
            reason: "fixture ✓".into()
        }))
    );
    peer.await.unwrap();
}

#[test]
fn production_limits_and_proxy_error_redaction() {
    let config = codex_config(RESPONSES_WEBSOCKET_MAX_MESSAGE_BYTES, true);
    assert_eq!(config.max_message_size, Some(64 * 1024 * 1024));
    assert_eq!(config.max_frame_size, config.max_message_size);
    let error = codex_error(codex::Error::Url(
        codex::error::UrlError::InvalidProxyConfig("http://fixture:secret@proxy.invalid".into()),
    ));
    assert!(!format!("{error:#}").contains("secret"));
}

async fn cancelled_connect_releases_socket(live: bool) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let request = format!("ws://{address}/responses")
        .into_client_request()
        .unwrap();
    let connect = tokio::spawn(subscription_connect(request, live));
    let (mut stream, _) = listener.accept().await.unwrap();
    head(&mut stream).await;
    connect.abort();
    assert!(connect.await.unwrap_err().is_cancelled());
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), stream.read(&mut [0u8]))
            .await
            .expect("cancelled handshake must release its TCP socket")
            .unwrap(),
        0
    );
}

async fn compressed_limit_and_untrusted_tls_remain_errors() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let peer = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = codex_ws::accept_async_with_config(stream, Some(codex_config(4096, true)))
            .await
            .unwrap();
        socket
            .send(codex::Message::Text("x".repeat(2048).into()))
            .await
            .unwrap();
        let _ = socket.next().await;
    });
    let mut socket =
        CodexSubscriptionWebSocketConnector::new(responses_websocket_tls_config().unwrap())
            .connect(
                format!("ws://{address}/responses")
                    .into_client_request()
                    .unwrap(),
                1024,
                true,
                true,
            )
            .await
            .unwrap();
    let error = socket.next().await.unwrap().unwrap_err();
    assert!(matches!(
        error.downcast_ref::<codex::Error>(),
        Some(codex::Error::Capacity(_))
    ));
    drop(socket);
    peer.await.unwrap();

    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![cert.cert.der().clone()],
        rustls::pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()).into(),
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let peer = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        assert!(
            tokio_rustls::TlsAcceptor::from(Arc::new(tls))
                .accept(stream)
                .await
                .is_err()
        );
    });
    let error = connect_codex_responses_websocket_request(
        format!("wss://localhost:{}/responses", address.port())
            .into_client_request()
            .unwrap(),
    )
    .await
    .unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<ResponsesWsConnectFailure>()
            .unwrap()
            .cause,
        "tls_error"
    );
    peer.await.unwrap();
}

#[tokio::test]
async fn successful_codex_handshake_state_is_once_only_and_recovery_safe() {
    for live in [false, true] {
        for fresh_state in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let rejected = r#"{"type":"error","error":{"code":"previous_response_not_found"}}"#;
            let fresh = r#"{"type":"response.metadata","headers":{"x-codex-turn-state":"newer"},"stream_id":"lane-a"}"#;
            let event =
                r#"{"type":"response.created","response":{"id":"resp-one"},"stream_id":"lane-a"}"#;
            let peer = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut ws = registry_ws::accept_hdr_async(
                    stream,
                    |_: &registry::handshake::server::Request,
                     mut response: registry::handshake::server::Response| {
                        for (name, value) in [
                            ("x-codex-turn-state", "opening"),
                            ("openai-model", "fixture-model"),
                            ("x-reasoning-included", "true"),
                            ("x-codex-primary-used-percent", "21"),
                            ("set-cookie", "session=never-forward"),
                        ] {
                            response.headers_mut().insert(name, value.parse().unwrap());
                        }
                        Ok(response)
                    },
                )
                .await
                .unwrap();
                ws.send(registry::Message::Text(rejected.into()))
                    .await
                    .unwrap();
                if fresh_state {
                    ws.send(registry::Message::Text(fresh.into()))
                        .await
                        .unwrap();
                }
                for _ in 0..2 {
                    ws.send(registry::Message::Text(event.into()))
                        .await
                        .unwrap();
                }
                assert!(ws.next().await.unwrap().unwrap().is_close());
            });
            let request = format!("ws://{address}/responses")
                .into_client_request()
                .unwrap();
            let mut socket = subscription_connect(request, live).await.unwrap();
            let headers = socket.take_handshake_headers().unwrap();
            assert_eq!(headers["x-codex-primary-used-percent"], "21");
            assert!(socket.take_handshake_headers().unwrap().is_empty());
            assert_eq!(
                socket.next().await.unwrap().unwrap().into_text().unwrap(),
                rejected
            );
            if fresh_state {
                assert_eq!(
                    socket.next().await.unwrap().unwrap().into_text().unwrap(),
                    fresh
                );
            }
            if !live {
                let text = socket.next().await.unwrap().unwrap().into_text().unwrap();
                let metadata: serde_json::Value = serde_json::from_str(&text).unwrap();
                assert_eq!(metadata["type"], "response.metadata");
                assert_eq!(metadata["stream_id"], "lane-a");
                assert_eq!(metadata["headers"]["openai-model"], "fixture-model");
                assert_eq!(metadata["headers"]["x-reasoning-included"], "true");
                if fresh_state {
                    assert!(metadata["headers"].get("x-codex-turn-state").is_none());
                } else {
                    assert_eq!(metadata["headers"]["x-codex-turn-state"], "opening");
                }
                assert!(metadata["headers"].get("set-cookie").is_none());
                assert!(
                    metadata["headers"]
                        .get("x-codex-primary-used-percent")
                        .is_none()
                );
                assert!(is_responses_metadata_frame(&text));
            }
            for _ in 0..2 {
                assert_eq!(
                    socket.next().await.unwrap().unwrap().into_text().unwrap(),
                    event
                );
            }
            socket.close(None).await.unwrap();
            peer.await.unwrap();
        }
    }
}

#[tokio::test]
async fn codex_dial_failure_keeps_connect_stage_and_unsent_request() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let request = format!("ws://{address}/responses")
        .into_client_request()
        .unwrap();
    let error = connect_codex_responses_websocket_request(request)
        .await
        .unwrap_err();
    let payload = responses_websocket_connect_error_payload(&error).unwrap();
    assert_eq!(payload["failure_stage"], "upstream_connect");
    assert_eq!(payload["transport_cause"], "connection_refused");
    assert_eq!(payload["request_sent"], false);
}
