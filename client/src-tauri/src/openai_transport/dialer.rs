//! Codex subscription dialing: one system/env proxy selection, explicit direct
//! mode, and the same 250ms IPv4/IPv6 racing used by the official connector.
use anyhow::{Context, Result};
use base64::Engine as _;
use codex_tokio_tungstenite::tungstenite::proxy::{ProxyAuth, ProxyConfig};
use futures_util::{StreamExt, stream::FuturesUnordered};
use hyper_util::client::proxy::matcher::Matcher;
use std::{
    collections::VecDeque,
    io,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
};

pub(crate) trait SocketIo: AsyncRead + AsyncWrite + Unpin + Send + std::fmt::Debug {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + std::fmt::Debug> SocketIo for T {}
pub(crate) type Socket = Box<dyn SocketIo>;

pub(crate) async fn connect(
    uri: &http::Uri,
    use_system_proxy: bool,
    tls: Arc<rustls::ClientConfig>,
) -> Result<Socket> {
    let mut url = reqwest::Url::parse(&uri.to_string())?;
    let scheme = match url.scheme() {
        "wss" => "https",
        "ws" => "http",
        _ => anyhow::bail!("invalid Codex WebSocket scheme"),
    };
    url.set_scheme(scheme)
        .map_err(|_| anyhow::anyhow!("invalid Codex WebSocket URL"))?;
    let host = url
        .host_str()
        .context("missing Codex WebSocket host")?
        .trim_matches(['[', ']']);
    let port = url
        .port_or_known_default()
        .context("missing Codex WebSocket port")?;
    let proxy = if use_system_proxy {
        static MATCHER: OnceLock<Mutex<(u64, Matcher)>> = OnceLock::new();
        let generation = crate::http_client::network_generation();
        let mut matcher = MATCHER
            .get_or_init(|| Mutex::new((generation, Matcher::from_system())))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matcher.0 != generation {
            *matcher = (generation, Matcher::from_system());
        }
        matcher.1.intercept(&url.as_str().parse::<http::Uri>()?)
    } else {
        None
    };
    let Some(proxy) = proxy else {
        return Ok(Box::new(connect_tcp(host, port).await?));
    };
    let mut proxy_url = reqwest::Url::parse(&proxy.uri().to_string())
        .map_err(|_| anyhow::anyhow!("invalid OpenAI subscription system proxy"))?;
    let proxy_tls = proxy_url.scheme() == "https";
    let proxy_host = proxy_url
        .host_str()
        .context("system proxy is missing host")?
        .trim_matches(['[', ']'])
        .to_string();
    let proxy_port = proxy_url.port_or_known_default().unwrap_or(1080);
    if proxy_tls {
        proxy_url
            .set_scheme("http")
            .map_err(|_| anyhow::anyhow!("invalid system proxy scheme"))?;
        proxy_url
            .set_port(Some(proxy_port))
            .map_err(|_| anyhow::anyhow!("invalid system proxy port"))?;
    }
    let mut config = ProxyConfig::parse(proxy_url.as_str())
        .map_err(|_| anyhow::anyhow!("unsupported or invalid OpenAI subscription system proxy"))?;
    config.auth = if let Some((user, password)) = proxy.raw_auth() {
        Some(ProxyAuth {
            username: user.into(),
            password: password.into(),
        })
    } else if let Some(basic) = proxy.basic_auth() {
        let encoded = basic
            .to_str()?
            .strip_prefix("Basic ")
            .context("invalid system proxy authentication")?;
        let decoded =
            String::from_utf8(base64::engine::general_purpose::STANDARD.decode(encoded)?)?;
        let (user, password) = decoded
            .split_once(':')
            .context("invalid system proxy authentication")?;
        Some(ProxyAuth {
            username: user.into(),
            password: password.into(),
        })
    } else {
        None
    };
    let tcp = connect_tcp(&proxy_host, proxy_port).await?;
    let stream: Socket = if proxy_tls {
        let name = rustls_pki_types::ServerName::try_from(proxy_host)?;
        Box::new(
            tokio_rustls::TlsConnector::from(tls)
                .connect(name, tcp)
                .await?,
        )
    } else {
        Box::new(tcp)
    };
    codex_tokio_tungstenite::proxy::connect_via_proxy(stream, &config, host, port)
        .await
        .map_err(Into::into)
}

async fn connect_tcp(host: &str, port: u16) -> io::Result<TcpStream> {
    let addresses: Vec<_> = tokio::net::lookup_host((host, port)).await?.collect();
    let first_ipv4 = addresses
        .first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "host has no addresses"))?
        .is_ipv4();
    let (mut preferred, mut alternate): (VecDeque<_>, VecDeque<_>) = addresses
        .into_iter()
        .partition(|a| a.is_ipv4() == first_ipv4);
    let mut addresses = VecDeque::new();
    while !preferred.is_empty() || !alternate.is_empty() {
        if let Some(a) = preferred.pop_front() {
            addresses.push_back(a);
        }
        if let Some(a) = alternate.pop_front() {
            addresses.push_back(a);
        }
    }
    let mut pending = FuturesUnordered::new();
    pending.push(TcpStream::connect(
        addresses.pop_front().expect("resolved address"),
    ));
    let mut next_at = tokio::time::Instant::now() + Duration::from_millis(250);
    loop {
        tokio::select! {
            result = pending.next() => match result.expect("at least one connection attempt") {
                Ok(stream) => return Ok(stream),
                Err(error) => {
                    if let Some(address) = addresses.pop_front() {
                        pending.push(TcpStream::connect(address));
                        next_at = tokio::time::Instant::now() + Duration::from_millis(250);
                    } else if pending.is_empty() { return Err(error); }
                }
            },
            _ = tokio::time::sleep_until(next_at), if !addresses.is_empty() => {
                pending.push(TcpStream::connect(addresses.pop_front().expect("remaining address")));
                next_at = tokio::time::Instant::now() + Duration::from_millis(250);
            }
        }
    }
}
