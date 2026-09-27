use hyper_util::client::proxy::matcher::Matcher;
use reqwest::{Client, Method, RequestBuilder, Response, Url, Version};
use std::{
    collections::HashMap,
    error::Error as _,
    fmt,
    future::Future,
    sync::{Arc, Mutex as StdMutex, OnceLock},
    time::{Duration, Instant},
};

const HTTP3_PROBE_TIMEOUT: Duration = Duration::from_secs(4);
const HTTP3_COOLDOWN: Duration = Duration::from_secs(5 * 60);
const HTTP3_COOLDOWN_JITTER_SECONDS: u64 = 30;
const MAX_AUTHORITY_STATES: usize = 256;

static UPSTREAM_TRANSPORT: OnceLock<AdaptiveUpstreamTransport> = OnceLock::new();

tokio::task_local! {
    static HTTP_VERSION_OBSERVER: Arc<StdMutex<Option<Version>>>;
    static PROBE_BUDGET: Vec<Arc<std::sync::atomic::AtomicUsize>>;
}

#[derive(Debug)]
pub(crate) enum UpstreamTransportError {
    ProbeBudgetExceeded,
    Request(reqwest::Error),
    AmbiguousHttp3 {
        authority: String,
        source: reqwest::Error,
    },
}

impl UpstreamTransportError {
    pub(crate) fn is_timeout(&self) -> bool {
        match self {
            Self::ProbeBudgetExceeded => false,
            Self::Request(error) | Self::AmbiguousHttp3 { source: error, .. } => error.is_timeout(),
        }
    }

    pub(crate) fn is_ambiguous(&self) -> bool {
        matches!(self, Self::AmbiguousHttp3 { .. })
    }
}

impl fmt::Display for UpstreamTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProbeBudgetExceeded => {
                formatter.write_str("probe attempt budget exhausted before sending upstream")
            }
            Self::Request(error) => error.fmt(formatter),
            Self::AmbiguousHttp3 { authority, source } => write!(
                formatter,
                "HTTP/3 request outcome for {authority} is unknown; automatic replay was refused: {source}"
            ),
        }
    }
}

impl std::error::Error for UpstreamTransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ProbeBudgetExceeded => None,
            Self::Request(error) | Self::AmbiguousHttp3 { source: error, .. } => Some(error),
        }
    }
}

pub(crate) async fn with_probe_budget<F: Future>(limit: usize, future: F) -> (F::Output, usize) {
    let remaining = Arc::new(std::sync::atomic::AtomicUsize::new(limit));
    let mut budgets = PROBE_BUDGET.try_with(Clone::clone).unwrap_or_default();
    budgets.push(remaining.clone());
    let output = PROBE_BUDGET.scope(budgets, future).await;
    (
        output,
        limit.saturating_sub(remaining.load(std::sync::atomic::Ordering::Relaxed)),
    )
}

pub(crate) fn probe_budget_active() -> bool {
    PROBE_BUDGET
        .try_with(|budgets| !budgets.is_empty())
        .unwrap_or(false)
}

fn claim_probe_attempt(request: &reqwest::Request) -> Result<(), UpstreamTransportError> {
    if !probe_budget_active() {
        return Ok(());
    }
    let path = request.url().path().to_ascii_lowercase();
    let inference = request.method() == Method::POST
        && [
            "/responses",
            "/messages",
            "/chat/completions",
            "generatecontent",
        ]
        .iter()
        .any(|suffix| path.contains(suffix));
    if !inference {
        return Ok(());
    }
    PROBE_BUDGET
        .try_with(|budgets| {
            use std::sync::atomic::Ordering::Relaxed;
            for (index, remaining) in budgets.iter().enumerate() {
                if remaining
                    .fetch_update(Relaxed, Relaxed, |v| v.checked_sub(1))
                    .is_err()
                {
                    for reserved in &budgets[..index] {
                        reserved.fetch_add(1, Relaxed);
                    }
                    return Err(UpstreamTransportError::ProbeBudgetExceeded);
                }
            }
            Ok(())
        })
        .unwrap_or(Ok(()))
}

#[cfg(test)]
mod probe_budget_tests {
    use super::*;
    #[tokio::test]
    async fn metadata_is_free_and_nested_attempts_share_one_budget() {
        let client = Client::new();
        let catalog = client.get("http://127.0.0.1:9/models").build().unwrap();
        let auth = client
            .post("http://127.0.0.1:9/oauth/token")
            .build()
            .unwrap();
        let infer = client
            .post("http://127.0.0.1:9/v1/responses")
            .build()
            .unwrap();
        let (_, used) = with_probe_budget(2, async {
            claim_probe_attempt(&catalog).unwrap();
            claim_probe_attempt(&auth).unwrap();
            claim_probe_attempt(&infer).unwrap();
            claim_probe_attempt(&infer).unwrap();
            assert!(matches!(
                claim_probe_attempt(&infer),
                Err(UpstreamTransportError::ProbeBudgetExceeded)
            ));
        })
        .await;
        assert_eq!(used, 2);
        claim_probe_attempt(&infer).unwrap();
        let (_, used) = with_probe_budget(1, async {
            let (_, nested_used) = with_probe_budget(8, async {
                claim_probe_attempt(&infer).unwrap();
                assert!(claim_probe_attempt(&infer).is_err());
            })
            .await;
            assert_eq!(nested_used, 1);
        })
        .await;
        assert_eq!(used, 1);
    }
}

pub(crate) async fn send(request: RequestBuilder) -> Result<Response, UpstreamTransportError> {
    UPSTREAM_TRANSPORT
        .get_or_init(AdaptiveUpstreamTransport::from_system)
        .send(request)
        .await
}

pub(crate) async fn observe_http_version<F>(future: F) -> (F::Output, Option<Version>)
where
    F: Future,
{
    let observed = Arc::new(StdMutex::new(None));
    let output = HTTP_VERSION_OBSERVER.scope(observed.clone(), future).await;
    let version = observed.lock().ok().and_then(|value| *value);
    (output, version)
}

pub(crate) fn http_version_transport(version: Version) -> &'static str {
    match version {
        Version::HTTP_3 => "http3",
        Version::HTTP_2 => "http2",
        Version::HTTP_11 | Version::HTTP_10 | Version::HTTP_09 => "http1",
        _ => "http",
    }
}

fn record_http_version(version: Version) {
    let _ = HTTP_VERSION_OBSERVER.try_with(|observed| {
        if let Ok(mut value) = observed.lock() {
            *value = Some(version);
        }
    });
}

async fn execute_observed(
    client: &Client,
    request: reqwest::Request,
) -> Result<Response, reqwest::Error> {
    let response = client.execute(request).await?;
    record_http_version(response.version());
    Ok(response)
}

pub(crate) trait AdaptiveRequestBuilderExt {
    fn send_adaptive(self) -> impl Future<Output = Result<Response, UpstreamTransportError>>;
}

impl AdaptiveRequestBuilderExt for RequestBuilder {
    fn send_adaptive(self) -> impl Future<Output = Result<Response, UpstreamTransportError>> {
        send(self)
    }
}

pub(crate) fn is_ambiguous_transport_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<UpstreamTransportError>()
            .is_some_and(UpstreamTransportError::is_ambiguous)
    }) || crate::channel_executor::channel_execution_error_code(error)
        == Some("ambiguous_transport")
}

#[derive(Clone)]
struct AdaptiveUpstreamTransport {
    inner: Arc<AdaptiveUpstreamTransportInner>,
}

struct AdaptiveUpstreamTransportInner {
    proxy_matcher: Matcher,
    authorities: StdMutex<HashMap<String, AuthorityState>>,
}

#[derive(Clone, Copy, Debug)]
enum AuthorityMode {
    Probing,
    Ready,
    CoolingDown { until: Instant },
}

#[derive(Clone, Copy, Debug)]
struct AuthorityState {
    mode: AuthorityMode,
    last_touched: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AuthoritySelection {
    Normal,
    Http3,
}

impl AdaptiveUpstreamTransport {
    fn from_system() -> Self {
        Self::with_proxy_matcher(Matcher::from_system())
    }

    fn with_proxy_matcher(proxy_matcher: Matcher) -> Self {
        Self {
            inner: Arc::new(AdaptiveUpstreamTransportInner {
                proxy_matcher,
                authorities: StdMutex::new(HashMap::new()),
            }),
        }
    }

    async fn send(&self, request: RequestBuilder) -> Result<Response, UpstreamTransportError> {
        let (client, request) = request.build_split();
        let request = request.map_err(UpstreamTransportError::Request)?;
        claim_probe_attempt(&request)?;
        if request.version() == Version::HTTP_3 {
            return execute_observed(&client, request)
                .await
                .map_err(UpstreamTransportError::Request);
        }

        // Official subscription clients use their normal HTTP stacks for auth,
        // catalog, usage and inference. Do not add a synthetic root HEAD probe
        // or opportunistic HTTP/3 identity to first-party subscription traffic.
        if uses_native_subscription_transport(request.url()) {
            return execute_observed(&client, request)
                .await
                .map_err(UpstreamTransportError::Request);
        }

        let Some(authority) = authority_key(request.url()) else {
            return execute_observed(&client, request)
                .await
                .map_err(UpstreamTransportError::Request);
        };
        if proxy_applies(&self.inner.proxy_matcher, request.url()) {
            return execute_observed(&client, request)
                .await
                .map_err(UpstreamTransportError::Request);
        }

        let (selection, start_probe) = self.select_authority(&authority);
        if start_probe {
            self.start_probe(client.clone(), authority.clone());
        }
        if selection != AuthoritySelection::Http3 {
            return execute_observed(&client, request)
                .await
                .map_err(UpstreamTransportError::Request);
        }

        let Some(mut http3_request) = request.try_clone() else {
            return execute_observed(&client, request)
                .await
                .map_err(UpstreamTransportError::Request);
        };
        *http3_request.version_mut() = Version::HTTP_3;
        let method = request.method().clone();
        match execute_observed(&client, http3_request).await {
            Ok(response) => {
                if response.version() != Version::HTTP_3 {
                    self.mark_http3_failed(&authority, "unexpected_version");
                } else {
                    self.touch_ready(&authority);
                }
                Ok(response)
            }
            Err(error) => {
                self.mark_http3_failed(&authority, http3_error_kind(&error));
                if http3_error_is_safe_to_fallback(&method, &error) {
                    claim_probe_attempt(&request)?;
                    log::debug!(
                        "[const-api][upstream-transport] authority={} event=http3_fallback",
                        authority
                    );
                    return execute_observed(&client, request)
                        .await
                        .map_err(UpstreamTransportError::Request);
                }
                Err(UpstreamTransportError::AmbiguousHttp3 {
                    authority,
                    source: error.without_url(),
                })
            }
        }
    }

    fn select_authority(&self, authority: &str) -> (AuthoritySelection, bool) {
        let now = Instant::now();
        let Ok(mut authorities) = self.inner.authorities.lock() else {
            return (AuthoritySelection::Normal, false);
        };
        if let Some(state) = authorities.get_mut(authority) {
            state.last_touched = now;
            return match state.mode {
                AuthorityMode::Ready => (AuthoritySelection::Http3, false),
                AuthorityMode::Probing => (AuthoritySelection::Normal, false),
                AuthorityMode::CoolingDown { until } if until > now => {
                    (AuthoritySelection::Normal, false)
                }
                AuthorityMode::CoolingDown { .. } => {
                    state.mode = AuthorityMode::Probing;
                    (AuthoritySelection::Normal, true)
                }
            };
        }

        if authorities.len() >= MAX_AUTHORITY_STATES {
            let removable = authorities
                .iter()
                .filter(|(_, state)| !matches!(state.mode, AuthorityMode::Probing))
                .min_by_key(|(_, state)| state.last_touched)
                .map(|(key, _)| key.clone());
            if let Some(key) = removable {
                authorities.remove(&key);
            } else {
                return (AuthoritySelection::Normal, false);
            }
        }
        authorities.insert(
            authority.to_string(),
            AuthorityState {
                mode: AuthorityMode::Probing,
                last_touched: now,
            },
        );
        (AuthoritySelection::Normal, true)
    }

    fn start_probe(&self, client: Client, authority: String) {
        let transport = self.clone();
        crate::spawn_logged("adaptive upstream HTTP/3 probe", async move {
            let result = transport.probe_http3(&client, &authority).await;
            match result {
                Ok(()) => transport.mark_http3_ready(&authority),
                Err(kind) => transport.mark_http3_failed(&authority, kind),
            }
        });
    }

    async fn probe_http3(&self, client: &Client, authority: &str) -> Result<(), &'static str> {
        let url = format!("{}/", authority.trim_end_matches('/'));
        let response = tokio::time::timeout(
            HTTP3_PROBE_TIMEOUT,
            client
                .head(url)
                .version(Version::HTTP_3)
                .timeout(HTTP3_PROBE_TIMEOUT)
                .send(),
        )
        .await
        .map_err(|_| "timeout")?
        .map_err(|error| http3_error_kind(&error))?;
        if response.version() != Version::HTTP_3 {
            return Err("unexpected_version");
        }
        if authority_key(response.url()).as_deref() != Some(authority) {
            return Err("cross_authority_redirect");
        }
        Ok(())
    }

    fn mark_http3_ready(&self, authority: &str) {
        let Ok(mut authorities) = self.inner.authorities.lock() else {
            return;
        };
        let Some(state) = authorities.get_mut(authority) else {
            return;
        };
        if !matches!(state.mode, AuthorityMode::Probing) {
            return;
        }
        state.mode = AuthorityMode::Ready;
        state.last_touched = Instant::now();
        log::debug!(
            "[const-api][upstream-transport] authority={} event=http3_ready",
            authority
        );
    }

    fn touch_ready(&self, authority: &str) {
        if let Ok(mut authorities) = self.inner.authorities.lock() {
            if let Some(state) = authorities.get_mut(authority) {
                if matches!(state.mode, AuthorityMode::Ready) {
                    state.last_touched = Instant::now();
                }
            }
        }
    }

    fn mark_http3_failed(&self, authority: &str, kind: &'static str) {
        let now = Instant::now();
        let Ok(mut authorities) = self.inner.authorities.lock() else {
            return;
        };
        let Some(state) = authorities.get_mut(authority) else {
            return;
        };
        state.mode = AuthorityMode::CoolingDown {
            until: now + cooldown_duration(authority),
        };
        state.last_touched = now;
        log::debug!(
            "[const-api][upstream-transport] authority={} event=http3_cooldown reason={}",
            authority,
            kind
        );
    }
}

fn uses_native_subscription_transport(url: &Url) -> bool {
    matches!(
        url.host_str()
            .map(|host| host.to_ascii_lowercase())
            .as_deref(),
        Some(
            "auth.openai.com"
                | "api.openai.com"
                | "chat.openai.com"
                | "chatgpt.com"
                | "api.anthropic.com"
                | "platform.claude.com"
                | "claude.com"
                | "claude.ai"
        )
    )
}

fn authority_key(url: &Url) -> Option<String> {
    (url.scheme() == "https" && url.host_str().is_some())
        .then(|| url.origin().ascii_serialization())
}

fn proxy_applies(matcher: &Matcher, url: &Url) -> bool {
    url.as_str()
        .parse::<http::Uri>()
        .ok()
        .is_some_and(|uri| matcher.intercept(&uri).is_some())
}

fn http3_error_is_safe_to_fallback(method: &Method, error: &reqwest::Error) -> bool {
    method_is_intrinsically_safe_to_replay(method) || provable_http3_connect_failure(error)
}

fn method_is_intrinsically_safe_to_replay(method: &Method) -> bool {
    matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

fn provable_http3_connect_failure(error: &reqwest::Error) -> bool {
    if !error.is_request() {
        return false;
    }
    let mut source = error.source();
    let mut pre_request_evidence = false;
    while let Some(cause) = source {
        // A stream or HTTP/3 connection error may occur after request headers
        // reached the peer. Do not let a nested socket error widen it into a
        // replay-safe connect failure.
        if cause.is::<h3::error::StreamError>() || cause.is::<h3::error::ConnectionError>() {
            return false;
        }
        if cause.is::<quinn::ConnectError>() {
            // Quinn documents ConnectError as occurring before any I/O.
            pre_request_evidence = true;
        }
        if let Some(error) = cause.downcast_ref::<quinn::ConnectionError>() {
            pre_request_evidence |= quinn_failure_proves_handshake_never_completed(error);
        }
        source = cause.source();
    }
    // reqwest only marks connector failures before an HTTP connection exists.
    // Generic QUIC connection and I/O errors are intentionally insufficient:
    // both can also occur after a non-idempotent request reached the peer.
    pre_request_evidence || error.is_connect()
}

fn quinn_failure_proves_handshake_never_completed(error: &quinn::ConnectionError) -> bool {
    match error {
        quinn::ConnectionError::VersionMismatch => true,
        quinn::ConnectionError::TransportError(error) => {
            let code = u64::from(error.code);
            // QUIC maps TLS handshake alerts to 0x100..=0x1ff. HTTP/3 request
            // streams do not exist until that handshake has completed.
            (0x100..0x200).contains(&code)
        }
        _ => false,
    }
}

fn http3_error_kind(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timeout"
    } else if provable_http3_connect_failure(error) {
        "connect"
    } else if error.is_redirect() {
        "redirect"
    } else if error.is_body() {
        "body"
    } else if error.is_request() {
        "request"
    } else {
        "other"
    }
}

fn cooldown_duration(authority: &str) -> Duration {
    let hash = authority
        .as_bytes()
        .iter()
        .fold(2_166_136_261_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(16_777_619)
        });
    HTTP3_COOLDOWN + Duration::from_secs(hash % (HTTP3_COOLDOWN_JITTER_SECONDS.saturating_add(1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn self_signed_server_identity(
        dns_name: &str,
    ) -> (
        rustls_pki_types::CertificateDer<'static>,
        rustls_pki_types::PrivatePkcs8KeyDer<'static>,
    ) {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let cert = rcgen::generate_simple_self_signed(vec![dns_name.to_string()])
            .expect("self-signed cert");
        (
            cert.cert.der().clone(),
            rustls_pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()),
        )
    }

    fn quic_server_config(
        cert: rustls_pki_types::CertificateDer<'static>,
        key: rustls_pki_types::PrivatePkcs8KeyDer<'static>,
        alpn: &[u8],
    ) -> quinn::ServerConfig {
        let mut tls_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert], key.into())
            .expect("TLS server config");
        tls_config.alpn_protocols = vec![alpn.to_vec()];
        quinn::ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(tls_config)
                .expect("QUIC server config"),
        ))
    }

    #[test]
    fn https_authorities_are_normalized_and_plain_http_is_excluded() {
        assert_eq!(
            authority_key(&Url::parse("https://EXAMPLE.com:443/v1?q=secret").unwrap()).as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            authority_key(&Url::parse("https://example.com:8443/v1").unwrap()).as_deref(),
            Some("https://example.com:8443")
        );
        assert_eq!(
            authority_key(&Url::parse("http://example.com/v1").unwrap()),
            None
        );
    }

    #[test]
    fn native_subscription_endpoints_skip_adaptive_http3_probes() {
        for url in [
            "https://auth.openai.com/oauth/token",
            "https://chatgpt.com/backend-api/codex/models",
            "https://api.openai.com/v1/responses",
            "https://api.anthropic.com/v1/messages",
            "https://platform.claude.com/v1/oauth/token",
            "https://claude.com/cai/oauth/authorize",
        ] {
            assert!(uses_native_subscription_transport(
                &Url::parse(url).unwrap()
            ));
        }
        assert!(!uses_native_subscription_transport(
            &Url::parse("https://gateway.example/v1/responses").unwrap()
        ));
    }

    #[test]
    fn matched_proxy_disables_http3_selection() {
        let matcher = Matcher::builder().https("http://127.0.0.1:8888").build();
        assert!(proxy_applies(
            &matcher,
            &Url::parse("https://api.example.test/v1").unwrap()
        ));
        assert!(!proxy_applies(
            &matcher,
            &Url::parse("http://api.example.test/v1").unwrap()
        ));
    }

    #[test]
    fn authority_probe_is_single_flight_and_cooldown_is_stable() {
        let transport = AdaptiveUpstreamTransport::with_proxy_matcher(Matcher::builder().build());
        assert_eq!(
            transport.select_authority("https://api.example.test"),
            (AuthoritySelection::Normal, true)
        );
        assert_eq!(
            transport.select_authority("https://api.example.test"),
            (AuthoritySelection::Normal, false)
        );
        transport.mark_http3_ready("https://api.example.test");
        assert_eq!(
            transport.select_authority("https://api.example.test"),
            (AuthoritySelection::Http3, false)
        );
        transport.mark_http3_failed("https://api.example.test", "test");
        assert_eq!(
            transport.select_authority("https://api.example.test"),
            (AuthoritySelection::Normal, false)
        );
        assert_eq!(
            cooldown_duration("https://api.example.test"),
            cooldown_duration("https://api.example.test")
        );
    }

    #[test]
    fn only_intrinsically_safe_methods_replay_without_connect_proof() {
        for method in [Method::GET, Method::HEAD, Method::OPTIONS] {
            assert!(method_is_intrinsically_safe_to_replay(&method));
        }
        for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            assert!(!method_is_intrinsically_safe_to_replay(&method));
        }
    }

    #[tokio::test]
    async fn post_falls_back_to_tcp_when_http3_handshake_cannot_complete() {
        use std::{
            io::{Read, Write},
            net::{Ipv4Addr, SocketAddr, TcpListener},
        };

        let (cert_der, key_der) = self_signed_server_identity("localhost");
        let (wrong_name_cert_der, wrong_name_key_der) =
            self_signed_server_identity("wrong-name.invalid");
        let tcp_listener =
            TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).expect("TCP listener");
        let server_addr = tcp_listener.local_addr().expect("TCP server address");
        let tls_config = Arc::new(
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(vec![cert_der.clone()], key_der.clone_key().into())
                .expect("TCP TLS config"),
        );
        let (tcp_seen_tx, tcp_seen_rx) = std::sync::mpsc::channel();
        let tcp_task = std::thread::spawn(move || {
            let (socket, _) = tcp_listener.accept().expect("TCP fallback connection");
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("TCP read timeout");
            let connection = rustls::ServerConnection::new(tls_config).expect("TCP TLS connection");
            let mut stream = rustls::StreamOwned::new(connection, socket);
            let mut request = [0_u8; 4096];
            let size = stream.read(&mut request).expect("read HTTP/1 request");
            assert!(String::from_utf8_lossy(&request[..size]).starts_with("POST /models "));
            tcp_seen_tx.send(()).expect("report TCP fallback");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .expect("write HTTP/1 response");
            stream.flush().expect("flush HTTP/1 response");
        });

        // The UDP endpoint presents a trusted certificate for the wrong DNS
        // name. Certificate validation therefore fails before an HTTP request
        // can exist, making this POST safe to retry over the normal TLS client.
        let quic_server = quinn::Endpoint::server(
            quic_server_config(wrong_name_cert_der.clone(), wrong_name_key_der, b"h3"),
            server_addr,
        )
        .expect("wrong-name QUIC endpoint");
        let quic_task = tokio::spawn(async move {
            if let Some(incoming) = quic_server.accept().await {
                let _ = incoming.await;
            }
        });

        let authority = format!("https://localhost:{}", server_addr.port());
        let client = Client::builder()
            .add_root_certificate(
                reqwest::Certificate::from_der(cert_der.as_ref()).expect("TCP root certificate"),
            )
            .add_root_certificate(
                reqwest::Certificate::from_der(wrong_name_cert_der.as_ref())
                    .expect("QUIC root certificate"),
            )
            .resolve("localhost", server_addr)
            .retry(reqwest::retry::never())
            .build()
            .expect("adaptive client");
        let transport = AdaptiveUpstreamTransport::with_proxy_matcher(Matcher::builder().build());
        transport.select_authority(&authority);
        transport.mark_http3_ready(&authority);

        let (response, observed_version) = observe_http_version(
            transport.send(
                client
                    .post(format!("{authority}/models"))
                    .timeout(Duration::from_secs(5)),
            ),
        )
        .await;
        let response = response.expect("POST should fall back before H3 request dispatch");
        assert_eq!(response.version(), Version::HTTP_11);
        assert_eq!(observed_version, Some(Version::HTTP_11));
        assert_eq!(response.bytes().await.unwrap().as_ref(), b"ok");
        tcp_seen_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("TCP fallback was not observed");
        tcp_task.join().expect("TCP server task");
        tokio::time::timeout(Duration::from_secs(5), quic_task)
            .await
            .expect("QUIC handshake task timeout")
            .expect("QUIC handshake task");
    }

    #[tokio::test]
    async fn post_is_not_replayed_after_http3_server_receives_it() {
        use std::net::{Ipv4Addr, SocketAddr, TcpListener};

        let (cert_der, key_der) = self_signed_server_identity("localhost");
        let tcp_listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .expect("TCP replay detector");
        let server_addr = tcp_listener.local_addr().expect("server address");
        tcp_listener
            .set_nonblocking(true)
            .expect("nonblocking replay detector");
        let quic_server =
            quinn::Endpoint::server(quic_server_config(cert_der, key_der, b"h3"), server_addr)
                .expect("HTTP/3 endpoint");
        let (received_tx, received_rx) = tokio::sync::oneshot::channel();
        let server_task = tokio::spawn(async move {
            let incoming = quic_server
                .accept()
                .await
                .expect("incoming QUIC connection");
            let connection = incoming.await.expect("QUIC handshake");
            let closer = connection.clone();
            let mut h3: h3::server::Connection<_, bytes::Bytes> =
                h3::server::Connection::new(h3_quinn::Connection::new(connection))
                    .await
                    .expect("HTTP/3 connection");
            let resolver = h3
                .accept()
                .await
                .expect("accept HTTP/3 request")
                .expect("request present");
            let (request, _stream) = resolver.resolve_request().await.expect("resolve request");
            assert_eq!(request.method(), http::Method::POST);
            assert_eq!(request.uri().path(), "/models");
            let _ = received_tx.send(());
            closer.close(0_u32.into(), b"intentional response loss");
        });

        let authority = format!("https://localhost:{}", server_addr.port());
        let client = Client::builder()
            .danger_accept_invalid_certs(true)
            .retry(reqwest::retry::never())
            .build()
            .expect("HTTP/3 client");
        let transport = AdaptiveUpstreamTransport::with_proxy_matcher(Matcher::builder().build());
        transport.select_authority(&authority);
        transport.mark_http3_ready(&authority);

        let error = transport
            .send(
                client
                    .post(format!("{authority}/models"))
                    .body("non-idempotent")
                    .timeout(Duration::from_secs(5)),
            )
            .await
            .expect_err("lost H3 response must be ambiguous");
        assert!(error.is_ambiguous(), "unexpected error: {error}");
        received_rx.await.expect("server did not receive POST");
        tokio::time::timeout(Duration::from_secs(5), server_task)
            .await
            .expect("HTTP/3 server timeout")
            .expect("HTTP/3 server task");

        // A TCP connection on the same authority would prove that the POST was
        // replayed after the H3 request reached the server.
        tokio::time::sleep(Duration::from_millis(100)).await;
        match tcp_listener.accept() {
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Ok(_) => panic!("ambiguous HTTP/3 POST was replayed over TCP"),
            Err(error) => panic!("TCP replay detector failed: {error}"),
        }
    }

    #[tokio::test]
    async fn ready_authority_uses_http3_for_a_real_request() {
        use std::net::{Ipv4Addr, SocketAddr};

        let (cert_der, key_der) = self_signed_server_identity("localhost");
        let server_config = quic_server_config(cert_der, key_der, b"h3");
        let server =
            quinn::Endpoint::server(server_config, SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
                .expect("HTTP/3 server endpoint");
        let server_addr = server.local_addr().expect("server address");
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let server_task = tokio::spawn(async move {
            let incoming = server.accept().await.expect("incoming QUIC connection");
            let connection = incoming.await.expect("QUIC handshake");
            let mut h3: h3::server::Connection<_, bytes::Bytes> =
                h3::server::Connection::new(h3_quinn::Connection::new(connection))
                    .await
                    .expect("HTTP/3 connection");
            for (expected_method, expected_path, body) in [(
                http::Method::POST,
                "/models",
                Some(bytes::Bytes::from_static(b"ok")),
            )] {
                let resolver = h3
                    .accept()
                    .await
                    .expect("accept HTTP/3 request")
                    .expect("request present");
                let (request, mut stream) =
                    resolver.resolve_request().await.expect("resolve request");
                assert_eq!(request.method(), expected_method);
                assert_eq!(request.uri().path(), expected_path);
                stream
                    .send_response(
                        http::Response::builder()
                            .status(http::StatusCode::OK)
                            .body(())
                            .expect("response"),
                    )
                    .await
                    .expect("send response");
                if let Some(body) = body {
                    stream.send_data(body).await.expect("send body");
                }
                stream.finish().await.expect("finish response");
            }
            let _ = shutdown_rx.await;
        });

        let authority = format!("https://localhost:{}", server_addr.port());
        let client = Client::builder()
            .danger_accept_invalid_certs(true)
            .retry(reqwest::retry::never())
            .build()
            .expect("HTTP/3 client");
        let transport = AdaptiveUpstreamTransport::with_proxy_matcher(Matcher::builder().build());
        transport.select_authority(&authority);
        transport.mark_http3_ready(&authority);

        let (response, observed_version) =
            observe_http_version(transport.send(client.post(format!("{authority}/models")))).await;
        let response = response.expect("adaptive HTTP/3 request");
        assert_eq!(response.version(), Version::HTTP_3);
        assert_eq!(observed_version, Some(Version::HTTP_3));
        assert_eq!(response.bytes().await.unwrap().as_ref(), b"ok");
        let _ = shutdown_tx.send(());
        tokio::time::timeout(Duration::from_secs(5), server_task)
            .await
            .expect("server timeout")
            .expect("server task");
    }
}
