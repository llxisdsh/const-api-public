use crate::{
    MODEL_REQUEST_TIMEOUT,
    model::Endpoint,
    supplier::{parse_supplier_quic_url, rustls_client_config_for_supplier_quic},
};
use anyhow::{Context, Result, anyhow};
use reqwest::{Client, Response, Url, Version, header::HeaderMap};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};

const HTTP3_PROBE_TIMEOUT: Duration = Duration::from_secs(4);
const PLATFORM_RECONNECT_INTERVAL: Duration = Duration::from_secs(2);
// Quinn defaults to a 30-second idle timeout. A model stream can legitimately
// pause longer than that while the supplier is thinking, so the platform HTTP/3
// transport must outlive the protocol-level stream inactivity deadline.
const PLATFORM_HTTP3_IDLE_TIMEOUT: Duration = MODEL_REQUEST_TIMEOUT;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PlatformTransportSnapshot {
    pub(crate) endpoint: String,
    endpoint_key: String,
    pub(crate) server_version: String,
    pub(crate) connection_state: String,
    pub(crate) connected_transport: String,
    pub(crate) last_error: String,
}

impl PlatformTransportSnapshot {
    fn select_endpoint(&mut self, endpoint: &Endpoint) {
        let key = endpoint_key(endpoint);
        if self.endpoint_key != key {
            self.server_version.clear();
            self.endpoint_key = key;
        }
        self.endpoint = endpoint.name.clone();
    }
}

#[derive(Debug, Clone, Copy)]
enum HTTP3State {
    Probing,
    Ready,
    CoolingDown(Instant),
}

pub(crate) struct PlatformHttpTransport {
    tcp_client: Client,
    tcp_stream_client: Client,
    http3_clients: StdMutex<HashMap<String, Client>>,
    http3_states: StdMutex<HashMap<String, HTTP3State>>,
    tcp_probes: StdMutex<HashSet<String>>,
    health_refresh_attempts: StdMutex<HashMap<String, Instant>>,
    state: Arc<StdMutex<PlatformTransportSnapshot>>,
}

impl PlatformHttpTransport {
    pub(crate) fn new(tcp_client: Client, state: Arc<StdMutex<PlatformTransportSnapshot>>) -> Self {
        // Streaming requests deliberately have no total-duration timeout. They
        // are still bounded by connect timeout, downstream cancellation, QUIC
        // idle timeout, and the supplier protocol's inactivity watchdog.
        let tcp_stream_client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .pool_idle_timeout(Duration::from_secs(90))
            .retry(reqwest::retry::never())
            .build()
            .expect("platform streaming HTTP client");
        Self {
            tcp_client,
            tcp_stream_client,
            http3_clients: StdMutex::new(HashMap::new()),
            http3_states: StdMutex::new(HashMap::new()),
            tcp_probes: StdMutex::new(HashSet::new()),
            health_refresh_attempts: StdMutex::new(HashMap::new()),
            state,
        }
    }

    /// Refresh the small platform health document without relying on the
    /// WebView lifecycle. Callers supply their own cadence; the shared gate
    /// coalesces background and About-page requests for the same endpoint.
    pub(crate) async fn refresh_health_if_due(
        self: &Arc<Self>,
        endpoint: &Endpoint,
        minimum_interval: Duration,
    ) -> Result<bool> { anyhow::bail!("Hosted platform access is unavailable in the local edition") }

    fn begin_health_refresh(&self, endpoint: &Endpoint, minimum_interval: Duration) -> bool {
        let Ok(mut attempts) = self.health_refresh_attempts.lock() else {
            return false;
        };
        let key = endpoint_key(endpoint);
        let now = Instant::now();
        if attempts
            .get(&key)
            .is_some_and(|last| now.saturating_duration_since(*last) < minimum_interval)
        {
            return false;
        }
        attempts.insert(key, now);
        true
    }

    pub(crate) fn tcp_client(&self, stream_requested: bool) -> &Client {
        if stream_requested {
            &self.tcp_stream_client
        } else {
            &self.tcp_client
        }
    }

    /// Send an idempotent platform control-plane GET through the same
    /// negotiated transport and connection pools used by model forwarding.
    pub(crate) async fn get(
        self: &Arc<Self>,
        endpoint: &Endpoint,
        path: &str,
        headers: HeaderMap,
    ) -> Result<Response> { anyhow::bail!("Hosted platform access is unavailable in the local edition") }

    pub(crate) fn start_probe(self: &Arc<Self>, endpoint: Endpoint) {}

    fn start_tcp_probe(self: &Arc<Self>, endpoint: Endpoint) {}

    pub(crate) fn http3_target(&self, endpoint: &Endpoint) -> Option<(Client, String)> {
        let key = endpoint_key(endpoint);
        let ready = self
            .http3_states
            .lock()
            .ok()
            .and_then(|states| states.get(&key).copied())
            .is_some_and(|state| matches!(state, HTTP3State::Ready));
        if !ready {
            return None;
        }
        let base_url = endpoint_http3_base_url(endpoint).ok()?;
        let client = self
            .http3_clients
            .lock()
            .ok()
            .and_then(|clients| clients.get(&key).cloned())?;
        Some((client, base_url))
    }

    pub(crate) fn mark_response_version(&self, endpoint: &Endpoint, version: Version) {
        let transport = match version {
            Version::HTTP_3 => "http3",
            Version::HTTP_2 => "http2",
            Version::HTTP_11 | Version::HTTP_10 | Version::HTTP_09 => "http1",
            _ => "http",
        };
        let preserve_http3_error = version != Version::HTTP_3
            && self
                .http3_states
                .lock()
                .ok()
                .and_then(|states| states.get(&endpoint_key(endpoint)).copied())
                .is_some_and(|state| matches!(state, HTTP3State::CoolingDown(_)));
        if let Ok(mut state) = self.state.lock() {
            if version != Version::HTTP_3
                && state.endpoint_key == endpoint_key(endpoint)
                && state.connected_transport == "http3"
            {
                return;
            }
            state.select_endpoint(endpoint);
            state.connection_state = "established".to_string();
            state.connected_transport = transport.to_string();
            if !preserve_http3_error {
                state.last_error.clear();
            }
        }
    }

    fn mark_health_response(&self, endpoint: &Endpoint, version: Version, body: &[u8]) {
        self.mark_response_version(endpoint, version);
        // Version metadata is optional. Older servers or non-JSON health bodies
        // must remain reachable, and never inherit another endpoint's version.
        let server_version = serde_json::from_slice::<serde_json::Value>(body)
            .ok()
            .and_then(|value| value.get("server_version")?.as_str().map(str::to_owned))
            .map(|value| value.trim().trim_start_matches('v').to_string())
            .filter(|value| value.len() <= 64 && semver::Version::parse(value).is_ok())
            .unwrap_or_default();
        if let Ok(mut state) = self.state.lock() {
            if state.endpoint_key == endpoint_key(endpoint) {
                state.server_version = server_version;
            }
        }
    }

    pub(crate) fn mark_http3_request_failed(
        self: &Arc<Self>,
        endpoint: &Endpoint,
        error: &anyhow::Error,
    ) {
        self.mark_http3_failed(endpoint, error);
        self.schedule_http3_retry(endpoint.clone());
    }

    pub(crate) fn mark_tcp_request_failed(
        self: &Arc<Self>,
        endpoint: &Endpoint,
        error: &anyhow::Error,
    ) {
        self.mark_tcp_failed(endpoint, error);
        if let Ok(mut probes) = self.tcp_probes.lock() {
            probes.remove(&endpoint_key(endpoint));
        }
        self.start_tcp_probe(endpoint.clone());
    }

    fn begin_probe(&self, key: &str) -> bool {
        let Ok(mut states) = self.http3_states.lock() else {
            return false;
        };
        let now = Instant::now();
        match states.get(key).copied() {
            Some(HTTP3State::Probing | HTTP3State::Ready) => false,
            Some(HTTP3State::CoolingDown(until)) if until > now => false,
            _ => {
                states.insert(key.to_string(), HTTP3State::Probing);
                true
            }
        }
    }

    async fn probe_http3(&self, endpoint: &Endpoint) -> Result<()> { anyhow::bail!("Hosted platform access is unavailable in the local edition") }

    async fn probe_tcp(&self, endpoint: &Endpoint) -> Result<()> { anyhow::bail!("Hosted platform access is unavailable in the local edition") }

    async fn http3_client_and_base(&self, endpoint: &Endpoint) -> Result<(Client, String)> { anyhow::bail!("Hosted platform access is unavailable in the local edition") }

    fn mark_probing(&self, endpoint: &Endpoint) {
        if let Ok(mut state) = self.state.lock() {
            if state.connected_transport.is_empty() {
                state.select_endpoint(endpoint);
                state.connection_state = "probing".to_string();
                state.last_error.clear();
            }
        }
    }

    fn mark_http3_ready(&self, endpoint: &Endpoint) {
        let key = endpoint_key(endpoint);
        if let Ok(mut states) = self.http3_states.lock() {
            states.insert(key, HTTP3State::Ready);
        }
        self.mark_response_version(endpoint, Version::HTTP_3);
    }

    fn mark_http3_failed(&self, endpoint: &Endpoint, error: &anyhow::Error) {
        let key = endpoint_key(endpoint);
        if let Ok(mut states) = self.http3_states.lock() {
            states.insert(
                key.clone(),
                HTTP3State::CoolingDown(Instant::now() + PLATFORM_RECONNECT_INTERVAL),
            );
        }
        if let Ok(mut clients) = self.http3_clients.lock() {
            clients.remove(&key);
        }
        if let Ok(mut state) = self.state.lock() {
            if state.endpoint.is_empty() || state.endpoint == endpoint.name {
                state.select_endpoint(endpoint);
                if state.connected_transport == "http3" || state.connected_transport.is_empty() {
                    state.connection_state = "degraded".to_string();
                    state.connected_transport.clear();
                }
                state.last_error = format!("{error:#}");
            }
        }
    }

    fn mark_tcp_failed(&self, endpoint: &Endpoint, error: &anyhow::Error) {
        if let Ok(mut state) = self.state.lock() {
            if (state.endpoint.is_empty() || state.endpoint == endpoint.name)
                && state.connected_transport != "http3"
            {
                state.select_endpoint(endpoint);
                state.connection_state = "reconnecting".to_string();
                state.connected_transport.clear();
                state.last_error = format!("{error:#}");
            }
        }
    }

    fn schedule_http3_retry(self: &Arc<Self>, endpoint: Endpoint) {
        let transport = self.clone();
        crate::spawn_logged("platform HTTP/3 reconnect", async move {
            tokio::time::sleep(PLATFORM_RECONNECT_INTERVAL).await;
            transport.start_probe(endpoint);
        });
    }
}

fn platform_request_url(base_url: &str, path: &str) -> Result<String> {
    let base_url = base_url.trim().trim_end_matches('/');
    let path = path.trim();
    if base_url.is_empty() || !path.starts_with('/') {
        return Err(anyhow!("invalid platform request URL"));
    }
    Ok(format!("{base_url}{path}"))
}

fn endpoint_key(endpoint: &Endpoint) -> String {
    format!(
        "{}|{}",
        endpoint.base_url.trim(),
        endpoint.supplier_quic_url.trim()
    )
}

fn endpoint_http3_base_url(endpoint: &Endpoint) -> Result<String> {
    let target = parse_supplier_quic_url(&endpoint.supplier_quic_url)?;
    let mut url = Url::parse(endpoint.base_url.trim()).context("invalid platform API URL")?;
    url.set_scheme("https")
        .map_err(|_| anyhow!("platform API URL cannot use HTTPS"))?;
    url.set_host(Some(&target.host))
        .map_err(|_| anyhow!("invalid HTTP/3 host"))?;
    url.set_port(Some(target.port))
        .map_err(|_| anyhow!("invalid HTTP/3 port"))?;
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string().trim_end_matches('/').to_string())
}

async fn http3_socket_address(
    target: &crate::supplier::SupplierQuicUrl,
) -> Result<(std::net::IpAddr, Option<std::net::SocketAddr>)> {
    if let Ok(address) = target.host.parse::<std::net::IpAddr>() {
        let local = match address {
            std::net::IpAddr::V4(_) => std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
            std::net::IpAddr::V6(_) => std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED),
        };
        return Ok((local, None));
    }
    let addresses: Vec<_> = tokio::net::lookup_host((target.host.as_str(), target.port))
        .await
        .with_context(|| format!("resolve HTTP/3 host {}", target.host))?
        .collect();
    let selected = addresses
        .iter()
        .find(|address| address.is_ipv4())
        .or_else(|| addresses.first())
        .copied()
        .ok_or_else(|| anyhow!("HTTP/3 host {} resolved no addresses", target.host))?;
    let local = match selected.ip() {
        std::net::IpAddr::V4(_) => std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
        std::net::IpAddr::V6(_) => std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED),
    };
    Ok((local, Some(selected)))
}
