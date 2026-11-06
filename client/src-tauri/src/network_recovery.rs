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
use tokio::sync::{Notify, watch};

const NETWORK_CHECK_INTERVAL: Duration = Duration::from_secs(5);
static CHANGES: OnceLock<watch::Sender<u64>> = OnceLock::new();
static RECHECK: Notify = Notify::const_new();

pub(crate) fn request_check() {
    // Coalesced by Notify and throttled by http_client. No OS calls or network
    // probes run on the failing request's critical path.
    RECHECK.notify_one();
}

pub(crate) fn subscribe() -> watch::Receiver<u64> {
    CHANGES.get_or_init(|| watch::channel(0).0).subscribe()
}

struct NetworkSnapshot {
    interfaces: Vec<u64>,
    routes: [Option<IpAddr>; 2],
    proxies: Matcher,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NetworkChange {
    None,
    SystemProxy,
    Network,
}

impl NetworkSnapshot {
    fn change_since(&self, previous: &Self, targets: &[String]) -> NetworkChange {
        if self.interfaces != previous.interfaces || self.routes != previous.routes {
            NetworkChange::Network
        } else if proxy_rules_fingerprint(&self.proxies)
            != proxy_rules_fingerprint(&previous.proxies)
            // Compare the same targets against both snapshots. Adding/removing
            // a channel is not itself a system network configuration change.
            || proxy_snapshot(&self.proxies, targets) != proxy_snapshot(&previous.proxies, targets)
        {
            NetworkChange::SystemProxy
        } else {
            NetworkChange::None
        }
    }
}

fn publish_change(change: NetworkChange, reset_platform: impl FnOnce()) {
    match change {
        NetworkChange::None => {}
        NetworkChange::SystemProxy => {
            crate::http_client::system_proxy_changed();
            log::info!(
                "[const-api][network] system proxy configuration changed; refreshing proxy clients"
            );
        }
        NetworkChange::Network => {
            CHANGES
                .get_or_init(|| watch::channel(0).0)
                .send_modify(|generation| {
                    // Serialize invalidation with probes committing their results.
                    crate::http_client::network_changed();
                    reset_platform();
                    *generation += 1;
                });
        }
    }
}

fn fingerprint(value: &impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn proxy_rules_fingerprint(matcher: &Matcher) -> u64 {
    // Matcher exposes neither equality nor its full bypass list. Its Debug view
    // includes endpoints and all bypass rules, but redacts authentication. Hash
    // that view only in memory, without parsing, logging or persisting it. Both
    // snapshots use the same library build; tests cover bypass-rule detection.
    // Sampling known URLs alone misses OAuth/CDN hosts outside the channel list.
    // proxy_snapshot separately compares authentication via public accessors.
    fingerprint(&format!("{matcher:?}"))
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

fn snapshot() -> std::io::Result<NetworkSnapshot> {
    let mut interfaces: Vec<_> = if_addrs::get_if_addrs()?.iter().map(fingerprint).collect();
    interfaces.sort_unstable();
    Ok(NetworkSnapshot {
        interfaces,
        routes: [
            route_source(SocketAddr::from(([192, 0, 2, 1], 9))),
            route_source(SocketAddr::from(([0x2001, 0xdb8, 0, 0, 0, 0, 0, 1], 9))),
        ],
        proxies: Matcher::from_system(),
    })
}

fn proxy_snapshot(matcher: &Matcher, targets: &[String]) -> Vec<u64> {
    // These URIs only select proxy rules; no packets are sent. Platform URLs
    // may all be bypassed (or absent in Local), while OAuth/downloads and
    // overseas suppliers must still notice changes to HTTP/HTTPS proxies.
    let mut proxies: Vec<_> = ["http://proxy-check.invalid", "https://proxy-check.invalid"]
        .into_iter()
        .chain(targets.iter().map(String::as_str))
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
                proxy.as_ref().and_then(|proxy| proxy.raw_auth()),
            )))
        })
        .collect();
    proxies.sort_unstable();
    proxies.dedup();
    proxies
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
                .chain(
                    config
                        .channels
                        .iter()
                        .filter(|channel| channel.v2.use_system_proxy)
                        .map(|channel| channel.upstream_base_url.clone()),
                )
                .collect::<Vec<_>>()
        };
        // OS interface/proxy discovery stays off the async runtime workers.
        if let Ok(Ok(current)) = tokio::task::spawn_blocking(snapshot).await {
            let change = previous.as_ref().map_or(NetworkChange::None, |previous| {
                current.change_since(previous, &targets)
            });
            previous = Some(current);
            publish_change(change, || state.platform_transport.reset_network());
            if change == NetworkChange::Network {
                state.account_refresh_notify.notify_one();
                log::info!(
                    "[const-api][network] local network configuration changed; refreshing connections"
                );
                let _ =
                    crate::runtime::refresh_platform_health_if_due(&state, Duration::ZERO).await;
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(NETWORK_CHECK_INTERVAL) => {}
            _ = RECHECK.notified() => {}
        }
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

    fn network_with_proxy(proxies: Matcher) -> NetworkSnapshot {
        NetworkSnapshot {
            interfaces: vec![1],
            routes: [None, None],
            proxies,
        }
    }

    #[test]
    fn proxy_changes_are_detected_without_platform_endpoints() {
        let direct = network_with_proxy(Matcher::builder().build());
        for proxies in [
            Matcher::builder().http("http://127.0.0.1:8081").build(),
            Matcher::builder().https("http://127.0.0.1:8082").build(),
        ] {
            let changed = network_with_proxy(proxies);
            assert_eq!(
                changed.change_since(&direct, &[]),
                NetworkChange::SystemProxy
            );
            assert_eq!(
                direct.change_since(&changed, &[]),
                NetworkChange::SystemProxy
            );
        }
    }

    #[test]
    fn supplier_bypass_and_proxy_credentials_are_observed() {
        let targets = vec!["https://supplier.example/v1".to_string()];
        let initial = network_with_proxy(
            Matcher::builder()
                .all("socks5h://test:first@127.0.0.1:8081")
                .no("platform.example")
                .build(),
        );
        let bypassed = network_with_proxy(
            Matcher::builder()
                .all("socks5h://test:first@127.0.0.1:8081")
                .no("platform.example,supplier.example")
                .build(),
        );
        assert_eq!(
            bypassed.change_since(&initial, &targets),
            NetworkChange::SystemProxy
        );
        let credentials = network_with_proxy(
            Matcher::builder()
                .all("socks5h://test:second@127.0.0.1:8081")
                .no("platform.example")
                .build(),
        );
        assert_eq!(
            credentials.change_since(&initial, &targets),
            NetworkChange::SystemProxy
        );
    }

    #[test]
    fn bypass_changes_outside_channel_targets_are_observed() {
        let initial = network_with_proxy(Matcher::builder().all("http://127.0.0.1:8081").build());
        let changed = network_with_proxy(
            Matcher::builder()
                .all("http://127.0.0.1:8081")
                .no("auth.openai.com")
                .build(),
        );
        let targets = vec!["https://chatgpt.com/backend-api/codex".to_string()];
        assert_eq!(
            proxy_snapshot(&initial.proxies, &targets),
            proxy_snapshot(&changed.proxies, &targets),
            "channel and generic targets do not cover the OAuth bypass rule"
        );
        assert_eq!(
            changed.change_since(&initial, &targets),
            NetworkChange::SystemProxy
        );
        assert_eq!(
            initial.change_since(&changed, &[]),
            NetworkChange::SystemProxy
        );
    }

    #[test]
    fn unchanged_settings_preserve_pools_when_channel_targets_change() {
        let previous = network_with_proxy(Matcher::builder().all("http://127.0.0.1:8081").build());
        let current = network_with_proxy(Matcher::builder().all("http://127.0.0.1:8081").build());
        for targets in [vec![], vec!["https://new-channel.example/v1".to_string()]] {
            assert_eq!(
                current.change_since(&previous, &targets),
                NetworkChange::None
            );
        }
        let changed_interface = NetworkSnapshot {
            interfaces: vec![2],
            ..network_with_proxy(Matcher::builder().all("http://127.0.0.1:8081").build())
        };
        assert_eq!(
            changed_interface.change_since(&previous, &[]),
            NetworkChange::Network
        );
        let changed_route = NetworkSnapshot {
            routes: [Some(IpAddr::from([192, 0, 2, 2])), None],
            ..network_with_proxy(Matcher::builder().all("http://127.0.0.1:8081").build())
        };
        assert_eq!(
            changed_route.change_since(&previous, &[]),
            NetworkChange::Network
        );
        let changed_both = NetworkSnapshot {
            interfaces: vec![2],
            ..network_with_proxy(Matcher::builder().all("http://127.0.0.1:8082").build())
        };
        assert_eq!(
            changed_both.change_since(&previous, &[]),
            NetworkChange::Network
        );
    }

    #[test]
    fn configuration_changes_respect_transport_boundaries() {
        // Isolate generation counters/watchers from other suites that deliberately
        // publish network changes. No running application's settings are touched.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "network_recovery::tests::configuration_change_child",
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
    #[ignore = "isolated child of configuration_changes_respect_transport_boundaries"]
    async fn configuration_change_child() {
        use futures_util::FutureExt;
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        let direct_builds = Arc::new(AtomicUsize::new(0));
        let builds = direct_builds.clone();
        let direct = crate::http_client::HttpClient::new(false, move || {
            builds.fetch_add(1, Ordering::Relaxed);
            reqwest::Client::builder().no_proxy()
        })
        .unwrap();
        let proxy_builds = Arc::new(AtomicUsize::new(0));
        let builds = proxy_builds.clone();
        let proxy = crate::http_client::HttpClient::new(true, move || {
            builds.fetch_add(1, Ordering::Relaxed);
            reqwest::Client::builder()
                .no_proxy()
                .proxy(reqwest::Proxy::all("http://127.0.0.1:9").unwrap())
        })
        .unwrap();
        // These are the same notification channel used by platform WS/QUIC
        // connections and the same stream wrapper used by resumable SSE.
        let mut changes = subscribe();
        let source = futures_util::stream::pending::<Result<bytes::Bytes, std::io::Error>>();
        let mut stream = watch_stream(source, changes.clone());
        let mut platform_resets = 0;
        let initial_network = crate::http_client::network_generation();
        let initial_proxy = crate::http_client::system_proxy_generation();

        publish_change(NetworkChange::None, || platform_resets += 1);
        assert_eq!(crate::http_client::system_proxy_generation(), initial_proxy);
        publish_change(NetworkChange::SystemProxy, || platform_resets += 1);
        assert_eq!(
            crate::http_client::system_proxy_generation(),
            initial_proxy + 1
        );
        assert_eq!(crate::http_client::network_generation(), initial_network);
        assert_eq!(platform_resets, 0);
        assert!(
            !changes.has_changed().unwrap(),
            "platform WS/QUIC must stay connected"
        );
        assert!(
            stream.next().now_or_never().is_none(),
            "SSE must not enter recovery"
        );
        direct.snapshot();
        proxy.snapshot();
        assert_eq!(direct_builds.load(Ordering::Relaxed), 1);
        assert_eq!(proxy_builds.load(Ordering::Relaxed), 2);

        publish_change(NetworkChange::Network, || platform_resets += 1);
        assert_eq!(platform_resets, 1);
        assert_eq!(
            crate::http_client::network_generation(),
            initial_network + 1
        );
        assert_eq!(
            crate::http_client::system_proxy_generation(),
            initial_proxy + 2
        );
        assert!(changes.has_changed().unwrap());
        changes.borrow_and_update();
        assert!(stream.next().await.unwrap().is_err());
        assert!(stream.next().await.is_none());
        direct.snapshot();
        proxy.snapshot();
        assert_eq!(direct_builds.load(Ordering::Relaxed), 2);
        assert_eq!(proxy_builds.load(Ordering::Relaxed), 3);
    }

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
