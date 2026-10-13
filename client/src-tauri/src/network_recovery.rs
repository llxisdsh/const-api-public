//! Observe local network configuration without sending probes or logging addresses.
//! A change invalidates future connections; only explicitly resumable streams may
//! reconnect in place. Ordinary POSTs are never replayed here.
use crate::model::AppState;
use futures_util::{Stream, StreamExt, stream::BoxStream};
use hyper_util::client::proxy::matcher::Matcher;
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    net::{IpAddr, SocketAddr, UdpSocket},
    sync::OnceLock,
    time::Duration,
};
use tokio::sync::watch;

const NETWORK_CHECK_INTERVAL: Duration = Duration::from_secs(5);
static CHANGES: OnceLock<watch::Sender<u64>> = OnceLock::new();

pub(crate) fn subscribe() -> watch::Receiver<u64> {
    CHANGES.get_or_init(|| watch::channel(0).0).subscribe()
}

#[derive(PartialEq, Eq)]
struct NetworkSnapshot {
    interfaces: Vec<u64>,
    routes: [Option<IpAddr>; 2],
    proxies: Vec<u64>,
}

fn fingerprint(value: &impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn route_source(destination: SocketAddr) -> Option<IpAddr> {
    let bind = if destination.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let socket = UdpSocket::bind(bind).ok()?;
    // UDP connect selects a route/local address but sends no datagrams.
    socket.connect(destination).ok()?;
    socket.local_addr().ok().map(|address| address.ip())
}

fn snapshot(targets: &[String]) -> std::io::Result<NetworkSnapshot> {
    let mut interfaces: Vec<_> = if_addrs::get_if_addrs()?.iter().map(fingerprint).collect();
    interfaces.sort_unstable();
    let matcher = Matcher::from_system();
    let mut proxies: Vec<_> = targets
        .iter()
        .filter_map(|target| {
            let uri = target.parse::<http::Uri>().ok()?;
            let proxy = matcher.intercept(&uri);
            Some(fingerprint(&(
                target,
                proxy.as_ref().map(|proxy| proxy.uri()),
                proxy
                    .as_ref()
                    .and_then(|proxy| proxy.basic_auth())
                    .map(|value| value.as_bytes()),
            )))
        })
        .collect();
    proxies.sort_unstable();
    Ok(NetworkSnapshot {
        interfaces,
        routes: [
            route_source(SocketAddr::from(([192, 0, 2, 1], 9))),
            route_source(SocketAddr::from(([0x2001, 0xdb8, 0, 0, 0, 0, 0, 1], 9))),
        ],
        proxies,
    })
}

pub(crate) async fn run(state: AppState) {
    let mut previous = None;
    loop {
        let targets = {
            let config = state
                .proxy_config
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            config
                .endpoints
                .iter()
                .filter(|endpoint| endpoint.enabled)
                .map(|endpoint| endpoint.base_url.clone())
                .collect::<Vec<_>>()
        };
        // OS interface/proxy discovery stays off the async runtime workers.
        if let Ok(Ok(current)) = tokio::task::spawn_blocking(move || snapshot(&targets)).await {
            let changed = previous
                .as_ref()
                .is_some_and(|previous| previous != &current);
            previous = Some(current);
            if changed {
                CHANGES
                    .get_or_init(|| watch::channel(0).0)
                    .send_modify(|generation| {
                        // Serialize invalidation with probes committing their results.
                        crate::http_client::network_changed();
                        state.platform_transport.reset_network();
                        *generation += 1;
                    });
                state.account_refresh_notify.notify_one();
                log::info!(
                    "[const-api][network] local network configuration changed; refreshing connections"
                );
                let _ =
                    crate::runtime::refresh_platform_health_if_due(&state, Duration::ZERO).await;
            }
        }
        tokio::time::sleep(NETWORK_CHECK_INTERVAL).await;
    }
}

pub(crate) fn watch_stream<S, E>(
    stream: S,
    changes: watch::Receiver<u64>,
) -> BoxStream<'static, Result<bytes::Bytes, std::io::Error>>
where
    S: Stream<Item = Result<bytes::Bytes, E>> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    Box::pin(futures_util::stream::unfold(
        (Box::pin(stream), changes, false),
        |(mut stream, mut changes, finished)| async move {
            if finished {
                return None;
            }
            let item = tokio::select! {
                biased;
                _ = changes.changed() => Some(Err(std::io::Error::new(std::io::ErrorKind::ConnectionAborted, "network configuration changed"))),
                item = stream.next() => item.map(|item| item.map_err(|error| std::io::Error::other(error.to_string()))),
            };
            item.map(|item| {
                let finished = item.is_err();
                (item, (stream, changes, finished))
            })
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn network_change_interrupts_an_idle_stream_once() {
        let (changes, receiver) = watch::channel(0);
        let source = futures_util::stream::pending::<Result<bytes::Bytes, std::io::Error>>();
        let mut stream = watch_stream(source, receiver);
        changes.send_modify(|generation| *generation += 1);
        let error = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::ConnectionAborted);
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn unchanged_network_preserves_bytes_and_normal_completion() {
        let (_changes, receiver) = watch::channel(0);
        let source = futures_util::stream::iter([Ok::<_, std::io::Error>(
            bytes::Bytes::from_static(b"original"),
        )]);
        let mut stream = watch_stream(source, receiver);
        assert_eq!(stream.next().await.unwrap().unwrap(), "original");
        assert!(stream.next().await.is_none());
    }
}
