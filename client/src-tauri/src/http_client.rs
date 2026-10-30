//! Refresh the shared HTTP pools after a connection failure, not each request.
//! Clones held by in-flight requests remain valid; this never replays a request.
use reqwest::{Client, ClientBuilder};
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

const RECOVERY_INTERVAL: Duration = Duration::from_secs(5);
static NETWORK_GENERATION: AtomicU64 = AtomicU64::new(0);
static LAST_RECOVERY: Mutex<Option<Instant>> = Mutex::new(None);

pub(crate) fn network_generation() -> u64 {
    NETWORK_GENERATION.load(Ordering::Relaxed)
}

pub(crate) fn observe_error(error: &reqwest::Error) {
    if error.is_connect() || error.is_timeout() {
        connection_failed();
    }
}

pub(crate) fn observe_response(response: &reqwest::Response) {
    // A reachable proxy can still return gateway errors after its settings or
    // route change. Re-read proxy settings at the same bounded recovery cadence;
    // only future snapshots change, and this never retries the failed request.
    if gateway_response_needs_recovery(response.status(), response.version()) {
        connection_failed();
    }
}

fn gateway_response_needs_recovery(status: reqwest::StatusCode, version: reqwest::Version) -> bool {
    version != reqwest::Version::HTTP_3 && matches!(status.as_u16(), 502 | 503 | 504)
}

pub(crate) fn connection_failed() {
    let mut last = LAST_RECOVERY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if begin_recovery(&mut last, Instant::now()) {
        NETWORK_GENERATION.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn network_changed() {
    // A real OS configuration change must not be swallowed by an earlier
    // failure's cooldown, otherwise the next retry still uses the old proxy.
    let mut last = LAST_RECOVERY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *last = Some(Instant::now());
    NETWORK_GENERATION.fetch_add(1, Ordering::Relaxed);
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
    factory: Option<Box<dyn Fn() -> ClientBuilder + Send + Sync>>,
    cached: Mutex<(u64, Client)>,
}

impl HttpClient {
    pub(crate) fn new(
        factory: impl Fn() -> ClientBuilder + Send + Sync + 'static,
    ) -> reqwest::Result<Self> {
        let client = factory().build()?;
        Ok(Self(Arc::new(ClientSource {
            factory: Some(Box::new(factory)),
            cached: Mutex::new((network_generation(), client)),
        })))
    }

    pub(crate) fn snapshot(&self) -> Client {
        self.snapshot_at(network_generation())
    }

    fn snapshot_at(&self, generation: u64) -> Client {
        let mut cached = self
            .0
            .cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cached.0 != generation {
            if let Some(factory) = &self.0.factory {
                match factory().build() {
                    Ok(client) => cached.1 = client,
                    // Preserve the last usable pool if the OS/TLS configuration
                    // cannot currently build a replacement. A later failed
                    // connection permits another attempt, at the shared cadence.
                    Err(error) => log::warn!(
                        "[const-api][network] HTTP pool rebuild failed: {}",
                        error.without_url()
                    ),
                }
            }
            cached.0 = generation;
        }
        cached.1.clone()
    }
}

impl From<Client> for HttpClient {
    fn from(client: Client) -> Self {
        Self(Arc::new(ClientSource {
            factory: None,
            cached: Mutex::new((network_generation(), client)),
        }))
    }
}

pub(crate) fn cached_client(
    slot: &'static OnceLock<HttpClient>,
    factory: impl Fn() -> ClientBuilder + Send + Sync + 'static,
) -> reqwest::Result<Client> {
    if let Some(client) = slot.get() {
        return Ok(client.snapshot());
    }
    let client = HttpClient::new(factory)?;
    Ok(slot.get_or_init(|| client).snapshot())
}

/// Channel data/metadata traffic has its own pools. Platform control traffic
/// keeps its existing system-proxy policy. Neither setting controls OS TUN routing.
pub(crate) fn channel_client(use_system_proxy: bool) -> reqwest::Result<Client> {
    static DIRECT: OnceLock<HttpClient> = OnceLock::new();
    static PROXY: OnceLock<HttpClient> = OnceLock::new();
    cached_client(if use_system_proxy { &PROXY } else { &DIRECT }, move || {
        let builder = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(crate::MODEL_REQUEST_TIMEOUT)
            .retry(reqwest::retry::never());
        proxy_policy(builder, use_system_proxy)
    })
}

pub(crate) fn channel_short_client(use_system_proxy: bool) -> reqwest::Result<Client> {
    static DIRECT: OnceLock<HttpClient> = OnceLock::new();
    static PROXY: OnceLock<HttpClient> = OnceLock::new();
    cached_client(if use_system_proxy { &PROXY } else { &DIRECT }, move || {
        proxy_policy(
            Client::builder()
                .timeout(Duration::from_secs(30))
                .retry(reqwest::retry::never()),
            use_system_proxy,
        )
    })
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

/// Use the same system-proxy recovery as HTTP for the supplier control tunnel.
/// Redirects and automatic retries are disabled before any registration is sent.
pub(crate) async fn connect_websocket(
    url: &str,
) -> anyhow::Result<tokio_tungstenite::WebSocketStream<reqwest::Upgraded>> {
    static CLIENT: OnceLock<HttpClient> = OnceLock::new();
    let client = cached_client(&CLIENT, || {
        Client::builder()
            .http1_only()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
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
            for (listener, label) in [(&direct, "direct"), (&proxy, "proxy"), (&direct, "direct")] {
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
        let source = HttpClient::new(move || {
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
        let generation = source.0.cached.lock().unwrap().0;
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
    fn gateway_recovery_excludes_auth_limits_and_http3() {
        for status in [200, 400, 401, 403, 408, 429, 500, 502, 503, 504] {
            let status = reqwest::StatusCode::from_u16(status).unwrap();
            assert_eq!(
                gateway_response_needs_recovery(status, reqwest::Version::HTTP_2),
                matches!(status.as_u16(), 502 | 503 | 504),
            );
            assert!(!gateway_response_needs_recovery(
                status,
                reqwest::Version::HTTP_3
            ));
        }
    }

    #[tokio::test]
    async fn gateway_failure_refreshes_proxy_without_replaying_post() {
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
        let source = HttpClient::new(move || {
            let builder = Client::builder().no_proxy().retry(reqwest::retry::never());
            if setting.load(Ordering::Relaxed) {
                builder.proxy(reqwest::Proxy::all(format!("http://{proxy_address}")).unwrap())
            } else {
                builder
            }
        })
        .unwrap();
        let held_source = source.clone();
        let snapshot = source.snapshot();
        let generation = network_generation();
        enabled.store(false, Ordering::Relaxed);
        let url = format!("http://{origin_address}/responses");
        let response = snapshot.post(&url).send_adaptive().await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::BAD_GATEWAY);
        assert_eq!(calls.load(Ordering::Relaxed), 0, "POST was not replayed");
        assert_eq!(proxy_calls.load(Ordering::Relaxed), 1);
        // Another test/recovery may have just used the process-wide cooldown.
        // An actual later gateway failure must still invalidate the pool.
        if network_generation() == generation {
            tokio::time::sleep(RECOVERY_INTERVAL).await;
            observe_response(&response);
        }
        assert_ne!(network_generation(), generation);
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
        proxy_task.abort();
        origin_task.abort();
    }
}
