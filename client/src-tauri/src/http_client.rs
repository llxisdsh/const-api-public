//! Refresh shared HTTP pools after a confirmed relevant configuration change.
//! Clones held by in-flight requests remain valid; this never replays a request.
use reqwest::{Client, ClientBuilder};
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

const RECOVERY_INTERVAL: Duration = Duration::from_secs(5);

// Recommended routes for CONST-owned traffic. These are code policy, not user
// settings. Current platform endpoints are domestic; channel traffic instead
// always uses its persisted choice (new-channel defaults live in source-drivers).
pub(crate) const PLATFORM_USE_SYSTEM_PROXY: bool = false;
const PUBLIC_RESOURCE_USE_SYSTEM_PROXY: bool = true;
const DOMESTIC_RESOURCE_HOSTS: &[&str] = &["const.tos-cn-shanghai.volces.com"];

/// Public overseas sources (GitHub, its CDN, future overseas mirrors) use the
/// system proxy. Maintain known domestic origins here; do not infer geography
/// from a model brand, DNS resolution, or a failed credential-bearing request.
pub(crate) fn public_resource_uses_system_proxy(url: &reqwest::Url) -> bool {
    let host = url.host_str().unwrap_or_default().trim_matches(['[', ']']);
    let local = host == "localhost"
        || host.ends_with(".localhost")
        || host.parse::<std::net::IpAddr>().is_ok_and(|ip| match ip {
            std::net::IpAddr::V4(ip) => {
                ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip.is_unspecified()
            }
            std::net::IpAddr::V6(ip) => {
                ip.is_loopback()
                    || ip.is_unique_local()
                    || ip.is_unicast_link_local()
                    || ip.is_unspecified()
                    || ip
                        .to_ipv4_mapped()
                        .is_some_and(|ip| ip.is_loopback() || ip.is_private() || ip.is_link_local())
            }
        });
    !local && !DOMESTIC_RESOURCE_HOSTS.contains(&host) && PUBLIC_RESOURCE_USE_SYSTEM_PROXY
}

pub(crate) fn public_resource_client(url: &str) -> anyhow::Result<Client> {
    Ok(channel_short_client(public_resource_uses_system_proxy(
        &reqwest::Url::parse(url)?,
    ))?)
}
static NETWORK_GENERATION: AtomicU64 = AtomicU64::new(0);
static SYSTEM_PROXY_GENERATION: AtomicU64 = AtomicU64::new(0);
static LAST_RECOVERY: Mutex<Option<Instant>> = Mutex::new(None);

pub(crate) fn network_generation() -> u64 {
    NETWORK_GENERATION.load(Ordering::Relaxed)
}

// Proxy-aware clients and matchers also refresh on physical network changes.
// Direct clients must not be invalidated by proxy settings alone.
pub(crate) fn system_proxy_generation() -> u64 {
    SYSTEM_PROXY_GENERATION.load(Ordering::Relaxed)
}

fn client_generation(use_system_proxy: bool) -> u64 {
    if use_system_proxy {
        system_proxy_generation()
    } else {
        network_generation()
    }
}

pub(crate) fn system_proxy_changed() {
    SYSTEM_PROXY_GENERATION.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn observe_error(error: &reqwest::Error) {
    if error.is_connect() || error.is_timeout() {
        connection_failed();
    }
}

pub(crate) fn observe_response(response: &reqwest::Response) {
    // A gateway error can come from either the proxy or the origin. Ask the
    // background monitor to check local settings, never discard healthy pools
    // merely because one upstream is overloaded.
    if gateway_response_needs_network_check(response.status(), response.version()) {
        connection_failed();
    }
}

fn gateway_response_needs_network_check(
    status: reqwest::StatusCode,
    version: reqwest::Version,
) -> bool {
    version != reqwest::Version::HTTP_3 && matches!(status.as_u16(), 502 | 503 | 504)
}

pub(crate) fn connection_failed() {
    let mut last = LAST_RECOVERY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if begin_recovery(&mut last, Instant::now()) {
        crate::network_recovery::request_check();
    }
}

pub(crate) fn network_changed() {
    // Only the network monitor (or an explicit configuration change) may
    // invalidate all pools. A failed request alone is not evidence of one.
    let mut last = LAST_RECOVERY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *last = Some(Instant::now());
    NETWORK_GENERATION.fetch_add(1, Ordering::Relaxed);
    system_proxy_changed();
}

fn begin_recovery(last: &mut Option<Instant>, now: Instant) -> bool {
    if last.is_some_and(|previous| now.duration_since(previous) < RECOVERY_INTERVAL) {
        return false;
    }
    *last = Some(now);
    true
}

#[derive(Clone)]
pub(crate) struct HttpClient(Arc<ClientSource>);

struct ClientSource {
    use_system_proxy: bool,
    factory: Option<Box<dyn Fn() -> ClientBuilder + Send + Sync>>,
    cached: Mutex<CachedClient>,
}

struct CachedClient {
    generation: u64,
    client: Client,
    failed_rebuild: Option<(u64, Instant)>,
}

impl HttpClient {
    pub(crate) fn new(
        use_system_proxy: bool,
        factory: impl Fn() -> ClientBuilder + Send + Sync + 'static,
    ) -> reqwest::Result<Self> {
        // If settings change while building, the first snapshot must rebuild;
        // never label a client built from older settings with the newer epoch.
        let generation = client_generation(use_system_proxy);
        let client = factory().build()?;
        Ok(Self(Arc::new(ClientSource {
            use_system_proxy,
            factory: Some(Box::new(factory)),
            cached: Mutex::new(CachedClient {
                generation,
                client,
                failed_rebuild: None,
            }),
        })))
    }

    pub(crate) fn snapshot(&self) -> Client {
        self.snapshot_at(client_generation(self.0.use_system_proxy))
    }

    fn snapshot_at(&self, generation: u64) -> Client {
        let mut cached = self
            .0
            .cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cached.generation == generation {
            return cached.client.clone();
        }
        let now = Instant::now();
        let rebuild_due = cached
            .failed_rebuild
            .is_none_or(|(failed_generation, failed_at)| {
                failed_generation != generation
                    || now.duration_since(failed_at) >= RECOVERY_INTERVAL
            });
        if rebuild_due {
            if let Some(factory) = &self.0.factory {
                match factory().build() {
                    Ok(client) => {
                        cached.client = client;
                        cached.generation = generation;
                        cached.failed_rebuild = None;
                    }
                    // Preserve the last usable pool if the OS/TLS configuration
                    // cannot currently build a replacement. Retry only this
                    // pool, at a bounded cadence, without invalidating others.
                    Err(error) => {
                        cached.failed_rebuild = Some((generation, now));
                        log::warn!(
                            "[const-api][network] HTTP pool rebuild failed: {}",
                            error.without_url()
                        );
                    }
                }
            } else {
                cached.generation = generation;
            }
        }
        cached.client.clone()
    }
}

impl From<Client> for HttpClient {
    fn from(client: Client) -> Self {
        Self(Arc::new(ClientSource {
            use_system_proxy: false,
            factory: None,
            cached: Mutex::new(CachedClient {
                generation: network_generation(),
                client,
                failed_rebuild: None,
            }),
        }))
    }
}

pub(crate) fn cached_client(
    slot: &'static OnceLock<HttpClient>,
    use_system_proxy: bool,
    factory: impl Fn() -> ClientBuilder + Send + Sync + 'static,
) -> reqwest::Result<Client> {
    if let Some(client) = slot.get() {
        return Ok(client.snapshot());
    }
    let client = HttpClient::new(use_system_proxy, factory)?;
    Ok(slot.get_or_init(|| client).snapshot())
}

/// Separate direct/proxy pools; neither setting controls OS TUN routing.
pub(crate) fn channel_client(use_system_proxy: bool) -> reqwest::Result<Client> {
    static DIRECT: OnceLock<HttpClient> = OnceLock::new();
    static PROXY: OnceLock<HttpClient> = OnceLock::new();
    cached_client(
        if use_system_proxy { &PROXY } else { &DIRECT },
        use_system_proxy,
        move || {
            let builder = Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(crate::MODEL_REQUEST_TIMEOUT)
                .retry(reqwest::retry::never());
            proxy_policy(builder, use_system_proxy)
        },
    )
}

pub(crate) fn channel_short_client(use_system_proxy: bool) -> reqwest::Result<Client> {
    static DIRECT: OnceLock<HttpClient> = OnceLock::new();
    static PROXY: OnceLock<HttpClient> = OnceLock::new();
    cached_client(
        if use_system_proxy { &PROXY } else { &DIRECT },
        use_system_proxy,
        move || {
            proxy_policy(
                Client::builder()
                    .timeout(Duration::from_secs(30))
                    .retry(reqwest::retry::never()),
                use_system_proxy,
            )
        },
    )
}

pub(crate) fn proxy_policy(builder: ClientBuilder, use_system_proxy: bool) -> ClientBuilder {
    if use_system_proxy {
        builder
    } else {
        builder.no_proxy()
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ChannelProxy(pub(crate) bool);

pub(crate) async fn connect_channel_websocket(
    request: http::Request<()>,
    config: tokio_tungstenite::tungstenite::protocol::WebSocketConfig,
) -> anyhow::Result<(
    tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<crate::openai_transport::dialer::Socket>,
    >,
    tokio_tungstenite::tungstenite::handshake::client::Response,
)> {
    // TLS to an HTTPS proxy uses ordinary native trust. Upstream TLS remains
    // the registry connector's native TLS; Codex-only CA/fingerprint policy
    // is deliberately not inherited by other channel types.
    static TLS: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    let tls = TLS.get_or_init(|| {
        let mut roots = rustls::RootCertStore::empty();
        roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
        Arc::new(
            rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::aws_lc_rs::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .expect("aws-lc supports default TLS versions")
            .with_root_certificates(roots)
            .with_no_client_auth(),
        )
    });
    let stream = crate::openai_transport::dialer::connect(request.uri(), true, tls.clone()).await?;
    Ok(
        tokio_tungstenite::client_async_tls_with_config(request, stream, Some(config), None)
            .await?,
    )
}

/// Use the same direct policy as platform HTTP for the supplier control tunnel.
/// Redirects and automatic retries are disabled before any registration is sent.
pub(crate) async fn connect_websocket(
    url: &str,
) -> anyhow::Result<tokio_tungstenite::WebSocketStream<reqwest::Upgraded>> {
    static CLIENT: OnceLock<HttpClient> = OnceLock::new();
    let client = cached_client(&CLIENT, PLATFORM_USE_SYSTEM_PROXY, || {
        proxy_policy(
            Client::builder()
                .http1_only()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(5))
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never()),
            PLATFORM_USE_SYSTEM_PROXY,
        )
    })?;
    connect_websocket_with_client(&client, url).await
}

async fn connect_websocket_with_client(
    client: &Client,
    url: &str,
) -> anyhow::Result<tokio_tungstenite::WebSocketStream<reqwest::Upgraded>> {
    use tokio_tungstenite::tungstenite::{
        handshake::{client::generate_key, derive_accept_key},
        protocol::Role,
    };
    let mut url = reqwest::Url::parse(url)?;
    let scheme = match url.scheme() {
        "ws" => "http",
        "wss" => "https",
        _ => anyhow::bail!("invalid WebSocket URL scheme"),
    };
    url.set_scheme(scheme)
        .map_err(|_| anyhow::anyhow!("invalid WebSocket URL"))?;
    let key = generate_key();
    let response = client
        .get(url)
        .version(reqwest::Version::HTTP_11)
        .header("connection", "Upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", &key)
        .send()
        .await
        .inspect_err(observe_error)?;
    observe_response(&response);
    let headers = response.headers();
    let upgrade = headers
        .get("upgrade")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"));
    let connection = headers
        .get("connection")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
        });
    let accept = derive_accept_key(key.as_bytes());
    if response.status() != reqwest::StatusCode::SWITCHING_PROTOCOLS
        || !upgrade
        || !connection
        || headers
            .get("sec-websocket-accept")
            .is_none_or(|value| value != accept.as_str())
        || headers.contains_key("sec-websocket-protocol")
        || headers.contains_key("sec-websocket-extensions")
    {
        anyhow::bail!(
            "invalid WebSocket upgrade response (HTTP {})",
            response.status()
        );
    }
    let socket = response.upgrade().await?;
    Ok(tokio_tungstenite::WebSocketStream::from_raw_socket(socket, Role::Client, None).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use warp::Filter;

    #[test]
    fn public_sources_use_domestic_direct_and_overseas_proxy_recommendations() {
        for url in [
            "https://const.tos-cn-shanghai.volces.com/catalog-registry/manifest.json",
            "http://localhost:15420/file",
            "http://127.0.0.1/check",
            "http://192.168.1.1/file",
            "http://[::1]/file",
            "http://[::ffff:127.0.0.1]/file",
        ] {
            assert!(
                !public_resource_uses_system_proxy(&url.parse().unwrap()),
                "{url}"
            );
        }
        for url in [
            "https://github.com/const/releases",
            "https://api.github.com/repos/const",
            "https://raw.githubusercontent.com/const/file",
            "https://release-assets.githubusercontent.com/file",
            "https://overseas-mirror.example/file",
            "https://const.tos-cn-shanghai.volces.com.attacker.example/file",
            "https://const.tos-cn-shanghai.volces.com@overseas.example/file",
        ] {
            assert!(
                public_resource_uses_system_proxy(&url.parse().unwrap()),
                "{url}"
            );
        }
    }

    #[tokio::test]
    async fn channel_http_proxy_is_opt_in_and_pool_modes_do_not_leak() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let direct = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child.args([
            "--ignored",
            "--exact",
            "http_client::tests::channel_proxy_child",
            "--nocapture",
        ]);
        for name in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "NO_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
            "no_proxy",
            "REQUEST_METHOD",
        ] {
            child.env_remove(name);
        }
        child
            .env(
                "HTTP_PROXY",
                format!("http://{}", proxy.local_addr().unwrap()),
            )
            .env(
                "CONST_CHANNEL_PROXY_TEST_URL",
                format!("http://{}/check", direct.local_addr().unwrap()),
            )
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = child.spawn().unwrap();
        let result = tokio::time::timeout(Duration::from_secs(20), async {
            for (listener, label) in [
                (&direct, "direct"),
                (&proxy, "proxy"),
                (&direct, "direct"),
                (&direct, "direct"),
                (&direct, "direct"),
                (&direct, "direct"),
                (&proxy, "proxy"),
            ] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                while !bytes.ends_with(b"\r\n\r\n") {
                    bytes.push(socket.read_u8().await.unwrap());
                }
                let head = String::from_utf8(bytes).unwrap();
                assert_eq!(head.starts_with("GET http://"), label == "proxy");
                socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            label.len(),
                            label
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
            let (socket, _) = direct.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
            use futures_util::{SinkExt, StreamExt};
            let frame = socket.next().await.unwrap().unwrap();
            socket.send(frame).await.unwrap();
            while child.try_wait().unwrap().is_none() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        if result.is_err() {
            let _ = child.kill();
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            result.is_ok() && output.status.success(),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    #[ignore = "isolated child of channel_http_proxy_is_opt_in_and_pool_modes_do_not_leak"]
    async fn channel_proxy_child() {
        let url = std::env::var("CONST_CHANNEL_PROXY_TEST_URL").unwrap();
        for (enabled, expected) in [(false, "direct"), (true, "proxy"), (false, "direct")] {
            let client = channel_client(enabled).unwrap();
            let target = if enabled {
                "http://channel.invalid/check"
            } else {
                &url
            };
            assert_eq!(
                client
                    .get(target)
                    .send()
                    .await
                    .unwrap()
                    .text()
                    .await
                    .unwrap(),
                expected
            );
        }
        for client in [
            crate::short_http_client(),
            crate::long_http_client(),
            public_resource_client(&url).unwrap(),
        ] {
            assert_eq!(
                client.get(&url).send().await.unwrap().text().await.unwrap(),
                "direct"
            );
        }
        assert_eq!(
            public_resource_client("http://overseas.invalid/file")
                .unwrap()
                .get("http://overseas.invalid/file")
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            "proxy"
        );
        use futures_util::{SinkExt, StreamExt};
        let mut socket = connect_websocket(&url.replacen("http:", "ws:", 1))
            .await
            .unwrap();
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                "direct-platform".into(),
            ))
            .await
            .unwrap();
        assert_eq!(
            socket.next().await.unwrap().unwrap().into_text().unwrap(),
            "direct-platform"
        );
    }

    #[tokio::test]
    async fn websocket_uses_configured_proxy_and_preserves_frames() {
        use futures_util::{SinkExt, StreamExt};
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut websocket = tokio_tungstenite::accept_hdr_async(
                socket,
                |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
                    assert_eq!(request.uri().host(), Some("platform.invalid"));
                    assert_eq!(request.uri().path(), "/supplier");
                    Ok(response)
                },
            )
            .await
            .unwrap();
            let frame = websocket.next().await.unwrap().unwrap();
            websocket.send(frame).await.unwrap();
        });
        let client = Client::builder()
            .no_proxy()
            .proxy(reqwest::Proxy::all(format!("http://{address}")).unwrap())
            .timeout(Duration::from_secs(2))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .unwrap();
        // The reserved hostname cannot resolve: only the configured proxy can serve it.
        let mut socket = connect_websocket_with_client(&client, "ws://platform.invalid/supplier")
            .await
            .unwrap();
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                "test-only".into(),
            ))
            .await
            .unwrap();
        assert_eq!(
            socket.next().await.unwrap().unwrap().into_text().unwrap(),
            "test-only"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn openai_oauth_exchange_uses_selected_system_proxy_without_tun() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let certificate =
            rcgen::generate_simple_self_signed(vec!["auth.openai.com".into()]).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ca = dir.path().join("ca.pem");
        std::fs::write(&ca, certificate.cert.pem()).unwrap();
        let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![certificate.cert.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der())
                .into(),
        )
        .unwrap();
        tls.alpn_protocols = vec![b"http/1.1".to_vec()];
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.args([
            "--ignored",
            "--exact",
            "http_client::tests::openai_oauth_proxy_child",
            "--nocapture",
        ]);
        for name in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "NO_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
            "no_proxy",
            "REQUEST_METHOD",
            "SSL_CERT_FILE",
        ] {
            command.env_remove(name);
        }
        let mut child = command
            .env(
                "HTTPS_PROXY",
                format!("http://{}", listener.local_addr().unwrap()),
            )
            .env("CODEX_CA_CERTIFICATE", &ca)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let result = tokio::time::timeout(Duration::from_secs(25), async {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") { head.push(socket.read_u8().await.unwrap()); }
            assert!(String::from_utf8(head).unwrap().starts_with("CONNECT auth.openai.com:443 HTTP/1.1"));
            socket.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n").await.unwrap();
            let mut socket = tokio_rustls::TlsAcceptor::from(Arc::new(tls)).accept(socket).await.unwrap();
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") { head.push(socket.read_u8().await.unwrap()); }
            let head = String::from_utf8(head).unwrap().to_ascii_lowercase();
            assert!(head.starts_with("post /oauth/token http/1.1"));
            let length: usize = head.lines().find_map(|line| line.strip_prefix("content-length: ")).unwrap().parse().unwrap();
            let mut body = vec![0; length];
            socket.read_exact(&mut body).await.unwrap();
            let body = String::from_utf8(body).unwrap();
            assert!(body.contains("grant_type=authorization_code"));
            assert!(body.contains("code=fixture-code"));
            assert!(body.contains("code_verifier=fixture-verifier"));
            let body = r#"{"access_token":"fixture-access","refresh_token":"fixture-refresh","expires_in":3600}"#;
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            while child.try_wait().unwrap().is_none() { tokio::time::sleep(Duration::from_millis(10)).await; }
        }).await;
        if result.is_err() {
            let _ = child.kill();
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            result.is_ok() && output.status.success(),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    #[ignore = "isolated child of openai_oauth_exchange_uses_selected_system_proxy_without_tun"]
    async fn openai_oauth_proxy_child() {
        let token = crate::detection::exchange_openai_oauth_code(
            &channel_client(true).unwrap(),
            true,
            "fixture-code",
            "fixture-verifier",
            "http://localhost:1455/auth/callback",
        )
        .await
        .unwrap();
        assert_eq!(token["access_token"], "fixture-access");
    }

    #[tokio::test]
    async fn websocket_rejects_invalid_upgrade_and_does_not_follow_redirects() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for response in [
            "HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: wrong\r\n\r\n",
            "HTTP/1.1 302 Found\r\nLocation: http://platform.invalid/redirected\r\nContent-Length: 0\r\n\r\n",
        ] {
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0; 2048];
                assert!(socket.read(&mut request).await.unwrap() > 0);
                socket.write_all(response.as_bytes()).await.unwrap();
            });
            let client = Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(2))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap();
            let error = connect_websocket_with_client(&client, &format!("ws://{address}/supplier"))
                .await
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("invalid WebSocket upgrade response"),
                "{error}"
            );
            server.await.unwrap();
        }
    }

    #[test]
    fn recovery_is_coalesced_and_bounded() {
        let now = Instant::now();
        let mut last = None;
        assert!(begin_recovery(&mut last, now));
        for offset in 0..5 {
            assert!(!begin_recovery(
                &mut last,
                now + Duration::from_secs(offset)
            ));
        }
        assert!(begin_recovery(&mut last, now + RECOVERY_INTERVAL));
    }

    #[tokio::test]
    async fn cached_clones_recover_after_proxy_stops_without_replaying_requests() {
        let origin_calls = Arc::new(AtomicUsize::new(0));
        let calls = origin_calls.clone();
        let origin = warp::any().map(move || {
            calls.fetch_add(1, Ordering::Relaxed);
            "origin OK"
        });
        let (origin_address, origin_server) = crate::bind_ephemeral!(origin, ([127, 0, 0, 1], 0));
        let origin_task = tokio::spawn(origin_server);
        let proxy = warp::any().map(|| warp::reply::with_header("proxy OK", "connection", "close"));
        let (proxy_address, proxy_server) = crate::bind_ephemeral!(proxy, ([127, 0, 0, 1], 0));
        let proxy_task = tokio::spawn(proxy_server);
        let enabled = Arc::new(AtomicBool::new(true));
        let builds = Arc::new(AtomicUsize::new(0));
        let setting = enabled.clone();
        let built = builds.clone();
        let source = HttpClient::new(true, move || {
            built.fetch_add(1, Ordering::Relaxed);
            let builder = Client::builder()
                .timeout(Duration::from_secs(2))
                .retry(reqwest::retry::never());
            if setting.load(Ordering::Relaxed) {
                builder.proxy(reqwest::Proxy::all(format!("http://{proxy_address}")).unwrap())
            } else {
                builder.no_proxy()
            }
        })
        .unwrap();
        let held_source = source.clone();
        let generation = source.0.cached.lock().unwrap().generation;
        let url = format!("http://{origin_address}/audit");
        assert_eq!(
            source
                .snapshot_at(generation)
                .get(&url)
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            "proxy OK"
        );
        for _ in 0..10 {
            held_source.snapshot_at(generation);
        }
        assert_eq!(builds.load(Ordering::Relaxed), 1);
        let in_flight_snapshot = source.snapshot_at(generation);
        proxy_task.abort();
        let _ = proxy_task.await;
        enabled.store(false, Ordering::Relaxed);
        assert!(
            in_flight_snapshot
                .post(&url)
                .body("test-only")
                .send()
                .await
                .is_err()
        );
        assert_eq!(
            origin_calls.load(Ordering::Relaxed),
            0,
            "a failed request must not be replayed"
        );
        assert_eq!(
            held_source
                .snapshot_at(generation + 1)
                .get(&url)
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            "origin OK"
        );
        source.snapshot_at(generation + 1);
        assert_eq!(
            builds.load(Ordering::Relaxed),
            2,
            "one replacement shared by all owners"
        );
        assert_eq!(origin_calls.load(Ordering::Relaxed), 1);
        origin_task.abort();
    }

    #[test]
    fn injected_clients_keep_their_explicit_configuration() {
        let source = HttpClient::from(Client::builder().no_proxy().build().unwrap());
        assert!(source.0.factory.is_none());
        source.snapshot_at(u64::MAX);
    }

    #[test]
    fn gateway_network_check_excludes_auth_limits_and_http3() {
        for status in [200, 400, 401, 403, 408, 429, 500, 502, 503, 504] {
            let status = reqwest::StatusCode::from_u16(status).unwrap();
            assert_eq!(
                gateway_response_needs_network_check(status, reqwest::Version::HTTP_2),
                matches!(status.as_u16(), 502 | 503 | 504),
            );
            assert!(!gateway_response_needs_network_check(
                status,
                reqwest::Version::HTTP_3
            ));
        }
    }

    #[test]
    fn gateway_failure_preserves_pools_until_confirmed_network_change() {
        // Other suites intentionally signal network changes. Isolate global
        // generation assertions without changing the running app's settings.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "http_client::tests::gateway_recovery_child",
                "--nocapture",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    #[ignore = "isolated child of gateway_failure_preserves_pools_until_confirmed_network_change"]
    async fn gateway_recovery_child() {
        use crate::upstream_transport::AdaptiveRequestBuilderExt;
        let calls = Arc::new(AtomicUsize::new(0));
        let origin_calls = calls.clone();
        let origin = warp::any().map(move || {
            origin_calls.fetch_add(1, Ordering::Relaxed);
            "origin OK"
        });
        let (origin_address, origin_server) = crate::bind_ephemeral!(origin, ([127, 0, 0, 1], 0));
        let origin_task = tokio::spawn(origin_server);
        let proxy_calls = Arc::new(AtomicUsize::new(0));
        let calls_for_proxy = proxy_calls.clone();
        let proxy = warp::any().map(move || {
            calls_for_proxy.fetch_add(1, Ordering::Relaxed);
            warp::reply::with_status("gateway unavailable", warp::http::StatusCode::BAD_GATEWAY)
        });
        let (proxy_address, proxy_server) = crate::bind_ephemeral!(proxy, ([127, 0, 0, 1], 0));
        let proxy_task = tokio::spawn(proxy_server);
        let enabled = Arc::new(AtomicBool::new(true));
        let setting = enabled.clone();
        let builds = Arc::new(AtomicUsize::new(0));
        let built = builds.clone();
        let source = HttpClient::new(true, move || {
            built.fetch_add(1, Ordering::Relaxed);
            let builder = Client::builder().no_proxy().retry(reqwest::retry::never());
            if setting.load(Ordering::Relaxed) {
                builder.proxy(reqwest::Proxy::all(format!("http://{proxy_address}")).unwrap())
            } else {
                builder
            }
        })
        .unwrap();
        let held_source = source.clone();
        let other_builds = Arc::new(AtomicUsize::new(0));
        let built = other_builds.clone();
        let unrelated = HttpClient::new(false, move || {
            built.fetch_add(1, Ordering::Relaxed);
            Client::builder().no_proxy()
        })
        .unwrap();
        let snapshot = source.snapshot();
        let generation = network_generation();
        let proxy_generation = system_proxy_generation();
        enabled.store(false, Ordering::Relaxed);
        let url = format!("http://{origin_address}/responses");
        let response = snapshot.post(&url).send_adaptive().await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::BAD_GATEWAY);
        assert_eq!(calls.load(Ordering::Relaxed), 0, "POST was not replayed");
        assert_eq!(proxy_calls.load(Ordering::Relaxed), 1);
        for _ in 0..10 {
            observe_response(&response);
            connection_failed();
            held_source.snapshot();
            unrelated.snapshot();
        }
        assert_eq!(
            network_generation(),
            generation,
            "an origin/proxy error alone must not reset pools"
        );
        assert_eq!(system_proxy_generation(), proxy_generation);
        assert_eq!(builds.load(Ordering::Relaxed), 1);
        assert_eq!(other_builds.load(Ordering::Relaxed), 1);
        // Simulate the background monitor confirming changed proxy settings.
        // This must bypass the preceding failure-check cooldown.
        system_proxy_changed();
        assert_eq!(network_generation(), generation);
        assert_eq!(
            held_source
                .snapshot()
                .get(&url)
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            "origin OK"
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(proxy_calls.load(Ordering::Relaxed), 1);
        assert_eq!(builds.load(Ordering::Relaxed), 2);
        unrelated.snapshot();
        assert_eq!(other_builds.load(Ordering::Relaxed), 1);
        // A physical network change still refreshes both modes.
        network_changed();
        assert_ne!(network_generation(), generation);
        held_source.snapshot();
        unrelated.snapshot();
        assert_eq!(builds.load(Ordering::Relaxed), 3);
        assert_eq!(other_builds.load(Ordering::Relaxed), 2);
        let racing_builds = Arc::new(AtomicUsize::new(0));
        let built = racing_builds.clone();
        let racing = HttpClient::new(false, move || {
            if built.fetch_add(1, Ordering::Relaxed) == 0 {
                network_changed();
            }
            Client::builder().no_proxy()
        })
        .unwrap();
        racing.snapshot();
        assert_eq!(
            racing_builds.load(Ordering::Relaxed),
            2,
            "a network change during construction must not be marked applied"
        );
        proxy_task.abort();
        origin_task.abort();
    }

    #[test]
    fn failed_pool_rebuild_retries_locally_without_consuming_network_change() {
        let invalid = Arc::new(AtomicBool::new(false));
        let setting = invalid.clone();
        let builds = Arc::new(AtomicUsize::new(0));
        let built = builds.clone();
        let source = HttpClient::new(false, move || {
            built.fetch_add(1, Ordering::Relaxed);
            Client::builder()
                .no_proxy()
                .user_agent(if setting.load(Ordering::Relaxed) {
                    "invalid\nheader"
                } else {
                    "test"
                })
        })
        .unwrap();
        let generation = source.0.cached.lock().unwrap().generation;
        invalid.store(true, Ordering::Relaxed);
        source.snapshot_at(generation + 1);
        assert_eq!(builds.load(Ordering::Relaxed), 2);
        for _ in 0..10 {
            source.snapshot_at(generation + 1);
        }
        assert_eq!(
            builds.load(Ordering::Relaxed),
            2,
            "failed rebuilds are throttled"
        );
        assert_eq!(source.0.cached.lock().unwrap().generation, generation);
        source.0.cached.lock().unwrap().failed_rebuild =
            Some((generation + 1, Instant::now() - RECOVERY_INTERVAL));
        invalid.store(false, Ordering::Relaxed);
        source.snapshot_at(generation + 1);
        assert_eq!(builds.load(Ordering::Relaxed), 3);
        assert_eq!(source.0.cached.lock().unwrap().generation, generation + 1);
        invalid.store(true, Ordering::Relaxed);
        source.snapshot_at(generation + 2);
        invalid.store(false, Ordering::Relaxed);
        source.snapshot_at(generation + 3);
        assert_eq!(
            source.0.cached.lock().unwrap().generation,
            generation + 3,
            "a newer configuration bypasses a failed rebuild's cooldown"
        );
    }
}
