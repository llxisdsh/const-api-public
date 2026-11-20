use super::*;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use std::{convert::Infallible, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, UdpSocket},
};

fn tls_fixture(alpn: &[u8]) -> (Client, tokio_rustls::TlsAcceptor) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.cert.der().clone()).unwrap();
    let mut client_tls = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    client_tls.alpn_protocols = vec![alpn.to_vec()];
    let mut server_tls = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.cert.der().clone()],
            rustls_pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()).into(),
        )
        .unwrap();
    server_tls.alpn_protocols = vec![alpn.to_vec()];
    (
        Client::builder()
            .no_proxy()
            .retry(reqwest::retry::never())
            .tls_backend_preconfigured(client_tls)
            .build()
            .unwrap(),
        tokio_rustls::TlsAcceptor::from(Arc::new(server_tls)),
    )
}

async fn read_head(stream: &mut (impl AsyncRead + Unpin)) -> String {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        bytes.push(stream.read_u8().await.unwrap());
        assert!(bytes.len() < 16_384);
    }
    String::from_utf8(bytes).unwrap()
}

#[tokio::test]
async fn advertised_http3_never_adds_probe_or_changes_next_request() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let udp = UdpSocket::bind(address).await.unwrap();
    let (client, tls) = tls_fixture(b"http/1.1");
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = tls.accept(stream).await.unwrap();
            assert!(
                read_head(&mut stream)
                    .await
                    .starts_with("POST /v1/messages HTTP/1.1")
            );
            let mut body = [0; 7];
            stream.read_exact(&mut body).await.unwrap();
            assert_eq!(&body, b"payload");
            stream.write_all(format!("HTTP/1.1 200 OK\r\nAlt-Svc: h3=\":{}\"; ma=86400\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK", address.port()).as_bytes()).await.unwrap();
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(150), listener.accept())
                .await
                .is_err()
        );
    });
    let mut channel = crate::channel_from_supplier(
        "http3-disabled-fixture".into(),
        &crate::default_supplier_config(),
    );
    channel.set_source_driver(crate::source_driver::SourceDriverId::CustomEndpoint);
    for index in 0..2 {
        let request = client
            .post(format!("https://localhost:{}/v1/messages", address.port()))
            .body("payload");
        let response = if index == 0 {
            send(request).await
        } else {
            send_for_channel(request, &channel).await
        }
        .unwrap();
        assert_eq!(response.version(), Version::HTTP_11);
        assert_eq!(response.text().await.unwrap(), "OK");
    }
    server.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(150), udp.recv_from(&mut [0; 1500]))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn adapter_http2_selection_and_body_are_preserved() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (client, tls) = tls_fixture(b"h2");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let stream = tls.accept(stream).await.unwrap();
        hyper::server::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
            .serve_connection(
                hyper_util::rt::TokioIo::new(stream),
                hyper::service::service_fn(
                    |request: hyper::Request<hyper::body::Incoming>| async move {
                        assert_eq!(request.version(), Version::HTTP_2);
                        assert_eq!(request.headers()["x-test-session"], "unchanged");
                        assert_eq!(
                            request.into_body().collect().await.unwrap().to_bytes(),
                            "payload"
                        );
                        Ok::<_, Infallible>(hyper::Response::new(Full::new(Bytes::from_static(
                            b"OK",
                        ))))
                    },
                ),
            )
            .await
            .unwrap();
    });
    let response = send(
        client
            .post(format!("https://localhost:{}/run", address.port()))
            .version(Version::HTTP_2)
            .header("x-test-session", "unchanged")
            .body("payload"),
    )
    .await
    .unwrap();
    assert_eq!(response.version(), Version::HTTP_2);
    assert_eq!(response.text().await.unwrap(), "OK");
    drop(client);
    tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn post_connection_loss_keeps_ambiguity_without_replay_or_url_leak() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/v1/messages?private-fixture",
        listener.local_addr().unwrap()
    );
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_head(&mut stream).await;
        let mut body = [0; 7];
        stream.read_exact(&mut body).await.unwrap();
        assert_eq!(&body, b"payload");
        drop(stream);
        assert!(
            tokio::time::timeout(Duration::from_millis(150), listener.accept())
                .await
                .is_err()
        );
    });
    let client = Client::builder()
        .no_proxy()
        .retry(reqwest::retry::never())
        .build()
        .unwrap();
    let error = send(client.post(url).body("payload")).await.unwrap_err();
    assert!(error.is_ambiguous());
    assert!(!error.to_string().contains("private-fixture"));
    let evidence = crate::supplier::execution_evidence_for_error(&anyhow::Error::new(error));
    assert_eq!(evidence.error_code, "ambiguous_transport");
    assert_eq!(evidence.recommendation.retry, "none");
    server.await.unwrap();
}

#[tokio::test]
async fn connect_failure_is_distinct_from_an_unacknowledged_post() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    drop(listener);
    let client = Client::builder().no_proxy().build().unwrap();
    let error = send(client.post(url).body("payload")).await.unwrap_err();
    assert!(!error.is_ambiguous());
}
