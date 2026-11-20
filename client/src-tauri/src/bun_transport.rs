//! Released Bun 1.4.4 HTTP profile shared by explicit subscription adapters.
//! Account headers/body remain adapter-owned; this module has no cookie store.
use bytes::Bytes;
use hyper_util::client::proxy::matcher::Matcher;
use reqwest::{Method, Response, ResponseBuilderExt, Url, header::HeaderMap};
use std::sync::Mutex;
use std::time::Duration;
use wreq::tls::{TlsOptions, TlsVersion, trust::CertStore};

type Error = crate::upstream_transport::UpstreamTransportError;

fn tls_options() -> TlsOptions {
    TlsOptions::builder()
        .min_tls_version(TlsVersion::TLS_1_2)
        .enable_ocsp_stapling(true)
        .enable_signed_cert_timestamps(true)
        .aes_hw_override(true)
        .curves_list("X25519MLKEM768:X25519:P-256:P-384")
        .sigalgs_list("ecdsa_secp256r1_sha256:rsa_pss_rsae_sha256:rsa_pkcs1_sha256:ecdsa_secp384r1_sha384:rsa_pss_rsae_sha384:rsa_pkcs1_sha384:rsa_pss_rsae_sha512:rsa_pkcs1_sha512:ed25519:ecdsa_secp521r1_sha512:rsa_pkcs1_sha1")
        .cipher_list("TLS_AES_128_GCM_SHA256:TLS_AES_256_GCM_SHA384:TLS_CHACHA20_POLY1305_SHA256:ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256:ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-RSA-AES256-GCM-SHA384:ECDHE-ECDSA-CHACHA20-POLY1305:ECDHE-RSA-CHACHA20-POLY1305:ECDHE-ECDSA-AES128-SHA:ECDHE-RSA-AES128-SHA:ECDHE-ECDSA-AES256-SHA:ECDHE-RSA-AES256-SHA:AES128-GCM-SHA256:AES256-GCM-SHA384:AES128-SHA:AES256-SHA")
        .preserve_tls13_cipher_list(true)
        .build()
}

type CachedClient = Option<(u64, String, wreq::Client)>;

/// Separate each adapter's pools; match proxies against the actual destination.
#[derive(Default)]
pub(crate) struct Transport {
    direct: Mutex<CachedClient>,
    proxy: Mutex<CachedClient>,
}

impl Transport {
    pub(crate) const fn new() -> Self {
        Self {
            direct: Mutex::new(None),
            proxy: Mutex::new(None),
        }
    }

    fn client(&self, url: &Url, use_system_proxy: bool) -> wreq::Result<wreq::Client> {
        let generation = if use_system_proxy {
            crate::http_client::system_proxy_generation()
        } else {
            crate::http_client::network_generation()
        };
        let origin = url.origin().ascii_serialization();
        let mut cached = if use_system_proxy {
            &self.proxy
        } else {
            &self.direct
        }
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((existing, destination, client)) = &*cached
            && *existing == generation
            && destination == &origin
        {
            return Ok(client.clone());
        }
        let client = build_with_proxy(use_system_proxy.then(Matcher::from_system), url)?.build()?;
        *cached = Some((generation, origin, client.clone()));
        Ok(client)
    }

    pub(crate) async fn send(
        &self,
        method: Method,
        url: Url,
        headers: HeaderMap,
        body: Bytes,
        use_system_proxy: bool,
    ) -> Result<Response, Error> {
        let client = self
            .client(&url, use_system_proxy)
            .map_err(|error| Error::Bun(error.without_uri()))?;
        crate::upstream_transport::claim_http_probe_attempt(&method, &url, &headers)?;
        send_with_client(&client, method, url, headers, body).await
    }
}

fn build_with_proxy(matcher: Option<Matcher>, url: &Url) -> wreq::Result<wreq::ClientBuilder> {
    let mut builder = builder()?;
    if let Some(proxy) = matcher.and_then(|matcher| {
        matcher.intercept(
            &url.origin()
                .ascii_serialization()
                .parse::<http::Uri>()
                .expect("HTTP origin is a valid URI"),
        )
    }) {
        let mut config = wreq::Proxy::all(proxy.uri().to_string())?;
        if let Some((user, password)) = proxy.raw_auth() {
            config = config.basic_auth(user, password);
        } else if let Some(auth) = proxy.basic_auth() {
            config = config.custom_http_auth(auth.clone());
        }
        builder = builder.proxy(config);
    }
    Ok(builder)
}

fn builder() -> wreq::Result<wreq::ClientBuilder> {
    // Certificate and hostname verification stay enabled. Native trust is also
    // needed for user-installed corporate/proxy CAs; no permissive verifier.
    let certificates = rustls_native_certs::load_native_certs();
    let roots = CertStore::builder()
        .add_der_certs(certificates.certs.iter().map(|cert| cert.as_ref()))
        .build()?;
    Ok(wreq::Client::builder()
        .no_proxy()
        .http1_only()
        .tls_options(tls_options())
        .tls_cert_store(roots)
        .redirect(wreq::redirect::Policy::none())
        .retry(wreq::retry::Policy::never())
        .connect_timeout(Duration::from_secs(10))
        .timeout(crate::MODEL_REQUEST_TIMEOUT))
}

async fn send_with_client(
    client: &wreq::Client,
    method: Method,
    url: Url,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    // Box the additional transport future before composing it with protocol
    // conversion; channel probes must not grow the Tokio worker stack.
    let response = Box::pin(
        client
            .request(method, url.as_str())
            .headers(headers)
            .body(body)
            .send(),
    )
    .await
    .map_err(|error| {
        if error.is_connect() || error.is_timeout() {
            crate::http_client::connection_failed();
        }
        Error::Bun(error.without_uri())
    })?;
    let mut output = http::Response::builder()
        .status(response.status())
        .version(response.version())
        .url(url);
    *output
        .headers_mut()
        .expect("upstream response headers are valid") = response.headers().clone();
    // Preserve incremental delivery and cancellation. No full-body buffer and
    // no retry after a POST, even when a connection or stream fails.
    let response = Response::from(
        output
            .body(reqwest::Body::wrap_stream(response.bytes_stream()))
            .expect("upstream response metadata is valid"),
    );
    crate::http_client::observe_response(&response);
    crate::upstream_transport::record_http_version(response.version());
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn read_head(stream: &mut TcpStream) -> String {
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(stream.read_u8().await.unwrap());
            assert!(head.len() < 16_384);
        }
        String::from_utf8(head).unwrap()
    }

    #[tokio::test]
    async fn stream_is_incremental_cancelable_and_never_replayed() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url: Url = format!("http://{}/stream", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_head(&mut stream).await;
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 1000\r\nX-Test: streaming\r\n\r\ndata: first\n\n").await.unwrap();
            let closed = tokio::time::timeout(Duration::from_secs(3), stream.read_u8()).await;
            assert!(
                closed.unwrap().is_err(),
                "dropping response must cancel upstream body"
            );
            assert!(
                tokio::time::timeout(Duration::from_millis(100), listener.accept())
                    .await
                    .is_err()
            );
        });
        let client = builder().unwrap().build().unwrap();
        let mut response = send_with_client(
            &client,
            Method::POST,
            url.clone(),
            HeaderMap::new(),
            Bytes::new(),
        )
        .await
        .unwrap();
        assert_eq!(response.url(), &url);
        assert_eq!(response.headers()["x-test"], "streaming");
        let first = tokio::time::timeout(Duration::from_secs(1), response.chunk())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(first.as_ref(), b"data: first\n\n");
        drop(response);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn proxy_preserves_body_auth_and_rejection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut proxy: Url = format!("http://{}", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        proxy.set_username("test").unwrap();
        proxy.set_password(Some("password")).unwrap();
        let body = Bytes::from_static(br#"{ "tools": [{"caller_tool":true}], "stream":true }"#);
        let expected = body.clone();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let head = read_head(&mut stream).await.to_ascii_lowercase();
            assert!(head.starts_with("post http://opencode.ai/v1/chat/completions http/1.1"));
            assert!(head.contains("authorization: bearer fixture"));
            assert!(head.contains("proxy-authorization: basic dgvzddpwyxnzd29yza=="));
            let mut received = vec![0; expected.len()];
            stream.read_exact(&mut received).await.unwrap();
            assert_eq!(received.as_slice(), expected.as_ref());
            stream.write_all(b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 4\r\nRetry-After: 60\r\nConnection: close\r\n\r\nnope").await.unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(100), listener.accept())
                    .await
                    .is_err()
            );
        });
        let matcher = Matcher::builder().https(proxy.as_str()).build();
        let client = build_with_proxy(Some(matcher), &"https://opencode.ai".parse().unwrap())
            .unwrap()
            .build()
            .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer fixture".parse().unwrap());
        let response = send_with_client(
            &client,
            Method::POST,
            "http://opencode.ai/v1/chat/completions".parse().unwrap(),
            headers,
            body,
        )
        .await
        .unwrap();
        assert_eq!(response.status(), 429);
        assert_eq!(response.headers()["retry-after"], "60");
        assert_eq!(response.text().await.unwrap(), "nope");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn no_proxy_bypass_does_not_reuse_the_proxy_route() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let matcher = Matcher::builder()
            .https("http://127.0.0.1:1")
            .no("opencode.ai")
            .build();
        // Resolve the canonical host to the fixture. If NO_PROXY is lost, this
        // attempts the deliberately closed proxy port instead of our server.
        let client = build_with_proxy(Some(matcher), &"https://opencode.ai".parse().unwrap())
            .unwrap()
            .resolve("opencode.ai", address)
            .build()
            .unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            assert!(
                read_head(&mut stream)
                    .await
                    .starts_with("POST /fixture HTTP/1.1")
            );
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK")
                .await
                .unwrap();
        });
        let url = format!("http://opencode.ai:{}/fixture", address.port())
            .parse()
            .unwrap();
        let response = tokio::time::timeout(
            Duration::from_secs(3),
            send_with_client(&client, Method::POST, url, HeaderMap::new(), Bytes::new()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(response.text().await.unwrap(), "OK");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn proxy_bypass_uses_the_actual_service_destination() {
        // OpenCode is bypassed, Claude is not. A factory still matching the old
        // hard-coded OpenCode host would wrongly connect directly for Claude.
        let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let direct = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target: Url = format!(
            "http://api.anthropic.com:{}/v1/messages",
            direct.local_addr().unwrap().port()
        )
        .parse()
        .unwrap();
        let matcher = Matcher::builder()
            .http(format!("http://{}", proxy.local_addr().unwrap()))
            .no("opencode.ai")
            .build();
        let client = build_with_proxy(Some(matcher), &target)
            .unwrap()
            .resolve("api.anthropic.com", direct.local_addr().unwrap())
            .build()
            .unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = proxy.accept().await.unwrap();
            let head = read_head(&mut stream).await;
            assert!(head.starts_with("POST http://api.anthropic.com:"));
            assert!(
                head.to_ascii_lowercase()
                    .contains("x-claude-code-session-id: fixture-session")
            );
            let mut body = [0; 7];
            stream.read_exact(&mut body).await.unwrap();
            assert_eq!(&body, b"payload");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK")
                .await
                .unwrap();
        });
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-claude-code-session-id",
            "fixture-session".parse().unwrap(),
        );
        let response = tokio::time::timeout(
            Duration::from_secs(3),
            send_with_client(
                &client,
                Method::POST,
                target,
                headers,
                Bytes::from_static(b"payload"),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(response.text().await.unwrap(), "OK");
        server.await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), direct.accept())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn bun_transport_rejects_untrusted_certificates() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url: Url = format!(
            "https://localhost:{}/",
            listener.local_addr().unwrap().port()
        )
        .parse()
        .unwrap();
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let tls = rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.cert.der().clone()],
            rustls_pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()).into(),
        )
        .unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let tls = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(tls));
            assert!(tls.accept(stream).await.is_err());
        });
        let client = builder().unwrap().build().unwrap();
        let error = tokio::time::timeout(
            Duration::from_secs(3),
            send_with_client(&client, Method::GET, url, HeaderMap::new(), Bytes::new()),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(matches!(error, Error::Bun(ref cause) if cause.is_connect()));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn connect_failure_keeps_health_evidence_without_retry_permission() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/?private-test", listener.local_addr().unwrap());
        drop(listener);
        let client = builder().unwrap().build().unwrap();
        let error = send_with_client(
            &client,
            Method::POST,
            url.parse().unwrap(),
            HeaderMap::new(),
            Bytes::new(),
        )
        .await
        .unwrap_err();
        assert!(!error.to_string().contains("private-test"));
        let evidence = crate::supplier::execution_evidence_for_error(&anyhow::Error::new(error));
        assert_eq!(evidence.recommendation.retry, "none");
        assert_eq!(evidence.error_code, "ambiguous_transport");
        assert_eq!(evidence.diagnostics.unwrap().error_class, "connect");
    }

    #[tokio::test]
    async fn tls_client_hello_matches_released_bun_profile() {
        // Captured from OpenCode 2.3.0 and Claude Code 2.1.302/2.1.310,
        // using a loopback TLS sink. Keep this when updating wreq/BoringSSL.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url: Url = format!(
            "https://localhost:{}/",
            listener.local_addr().unwrap().port()
        )
        .parse()
        .unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut record = [0; 5];
            stream.read_exact(&mut record).await.unwrap();
            assert_eq!(record[0], 22);
            let mut hello = vec![0; u16::from_be_bytes([record[3], record[4]]) as usize];
            stream.read_exact(&mut hello).await.unwrap();
            assert_eq!(hello[0], 1);
            let mut offset = 4 + 2 + 32;
            offset += 1 + hello[offset] as usize;
            let list = |bytes: &[u8]| {
                bytes
                    .chunks_exact(2)
                    .map(|p| u16::from_be_bytes([p[0], p[1]]))
                    .collect::<Vec<_>>()
            };
            let length = u16::from_be_bytes([hello[offset], hello[offset + 1]]) as usize;
            offset += 2;
            assert_eq!(
                list(&hello[offset..offset + length]),
                [
                    4865, 4866, 4867, 49195, 49199, 49196, 49200, 52393, 52392, 49161, 49171,
                    49162, 49172, 156, 157, 47, 53
                ]
            );
            offset += length;
            offset += 1 + hello[offset] as usize;
            offset += 2;
            let mut extensions = Vec::new();
            while offset < hello.len() {
                let kind = u16::from_be_bytes([hello[offset], hello[offset + 1]]);
                let length = u16::from_be_bytes([hello[offset + 2], hello[offset + 3]]) as usize;
                let value = &hello[offset + 4..offset + 4 + length];
                extensions.push(kind);
                match kind {
                    10 => assert_eq!(list(&value[2..]), [4588, 29, 23, 24]),
                    13 => assert_eq!(
                        list(&value[2..]),
                        [
                            1027, 2052, 1025, 1283, 2053, 1281, 2054, 1537, 2055, 1539, 513
                        ]
                    ),
                    16 => assert_eq!(value, b"\x00\x09\x08http/1.1"),
                    43 => assert_eq!(list(&value[1..]), [772, 771]),
                    _ => (),
                }
                offset += 4 + length;
            }
            assert_eq!(
                extensions,
                [0, 23, 65281, 10, 11, 35, 16, 5, 13, 18, 51, 45, 43]
            );
        });
        let client = builder().unwrap().build().unwrap();
        assert!(
            tokio::time::timeout(
                Duration::from_secs(5),
                send_with_client(&client, Method::GET, url, HeaderMap::new(), Bytes::new())
            )
            .await
            .unwrap()
            .is_err()
        );
        server.await.unwrap();
    }
}
