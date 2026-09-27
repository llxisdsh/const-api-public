use std::{
    collections::HashMap,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Mutex, oneshot},
    task::JoinHandle,
    time::timeout,
};

const CALLBACK_LIFETIME: Duration = Duration::from_secs(10 * 60);
const CALLBACK_READ_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CALLBACK_REQUEST_BYTES: usize = 16 * 1024;

#[derive(Clone, Default)]
pub(crate) struct SubscriptionOAuthCallbackManager {
    sessions: Arc<Mutex<HashMap<String, PendingCallback>>>,
}

struct PendingCallback {
    port: u16,
    receiver: Option<oneshot::Receiver<String>>,
    task: JoinHandle<()>,
}

#[derive(Debug, Clone)]
struct CallbackTarget {
    host: CallbackHost,
    port: u16,
    path: String,
    redirect_uri: reqwest::Url,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallbackHost {
    Localhost,
    Ipv4,
}

struct CallbackListeners {
    ipv4: TcpListener,
    ipv6: Option<TcpListener>,
}

enum CallbackRequest {
    Authorized(String),
    Incomplete,
    NotFound,
}

impl SubscriptionOAuthCallbackManager {
    pub(crate) async fn prepare(&self, state: &str, redirect_uri: &str) -> bool {
        let state = state.trim();
        let Some(target) = CallbackTarget::parse(redirect_uri) else {
            return false;
        };
        if state.is_empty() {
            return false;
        }

        self.cancel_port(target.port).await;
        let listeners = match CallbackListeners::bind(target.host, target.port).await {
            Ok(listeners) => listeners,
            Err(error) => {
                log::warn!(
                    "[const-api][subscription-oauth] callback listener unavailable port={} kind={:?} os_error={:?}; manual paste remains available",
                    target.port,
                    error.kind(),
                    error.raw_os_error(),
                );
                return false;
            }
        };

        let (callback_sender, callback_receiver) = oneshot::channel();
        let expected_state = state.to_string();
        let port = target.port;
        let task = tokio::spawn(async move {
            run_callback_listener(listeners, target, expected_state, callback_sender).await;
        });
        self.sessions.lock().await.insert(
            state.to_string(),
            PendingCallback {
                port,
                receiver: Some(callback_receiver),
                task,
            },
        );
        true
    }

    pub(crate) async fn wait(&self, state: &str) -> Option<String> {
        self.wait_with_timeout(state, CALLBACK_LIFETIME).await
    }

    async fn wait_with_timeout(&self, state: &str, wait_timeout: Duration) -> Option<String> {
        let receiver = {
            let mut sessions = self.sessions.lock().await;
            sessions.get_mut(state)?.receiver.take()?
        };
        let callback = timeout(wait_timeout, receiver)
            .await
            .ok()
            .and_then(Result::ok);
        self.cancel(state).await;
        callback
    }

    pub(crate) async fn cancel(&self, state: &str) {
        let pending = self.sessions.lock().await.remove(state);
        if let Some(pending) = pending {
            stop_pending_callback(pending).await;
        }
    }

    async fn cancel_port(&self, port: u16) {
        let pending = {
            let mut sessions = self.sessions.lock().await;
            let states = sessions
                .iter()
                .filter_map(|(state, pending)| (pending.port == port).then_some(state.clone()))
                .collect::<Vec<_>>();
            states
                .into_iter()
                .filter_map(|state| sessions.remove(&state))
                .collect::<Vec<_>>()
        };
        for pending in pending {
            stop_pending_callback(pending).await;
        }
    }
}

impl CallbackTarget {
    fn parse(raw: &str) -> Option<Self> {
        let redirect_uri = reqwest::Url::parse(raw.trim()).ok()?;
        if redirect_uri.scheme() != "http" {
            return None;
        }
        let host = match redirect_uri.host_str()? {
            host if host.eq_ignore_ascii_case("localhost") => CallbackHost::Localhost,
            "127.0.0.1" => CallbackHost::Ipv4,
            _ => return None,
        };
        let port = redirect_uri.port_or_known_default()?;
        if port == 0 {
            return None;
        }
        Some(Self {
            host,
            port,
            path: redirect_uri.path().to_string(),
            redirect_uri,
        })
    }
}

impl CallbackListeners {
    async fn bind(host: CallbackHost, port: u16) -> std::io::Result<Self> {
        let ipv4 = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
        let ipv6 = if host == CallbackHost::Localhost {
            TcpListener::bind((Ipv6Addr::LOCALHOST, port)).await.ok()
        } else {
            None
        };
        Ok(Self { ipv4, ipv6 })
    }

    async fn accept(&self) -> std::io::Result<(TcpStream, SocketAddr)> {
        if let Some(ipv6) = &self.ipv6 {
            tokio::select! {
                accepted = self.ipv4.accept() => accepted,
                accepted = ipv6.accept() => accepted,
            }
        } else {
            self.ipv4.accept().await
        }
    }
}

async fn stop_pending_callback(pending: PendingCallback) {
    pending.task.abort();
    let _ = pending.task.await;
}

async fn run_callback_listener(
    listeners: CallbackListeners,
    target: CallbackTarget,
    expected_state: String,
    callback_sender: oneshot::Sender<String>,
) {
    let mut callback_sender = Some(callback_sender);
    let serve = async {
        loop {
            let Ok((mut stream, _)) = listeners.accept().await else {
                continue;
            };
            let request = read_callback_request(&mut stream, &target, &expected_state).await;
            match request {
                CallbackRequest::Authorized(callback) => {
                    let _ = write_callback_response(&mut stream, 200, success_page()).await;
                    if let Some(sender) = callback_sender.take() {
                        let _ = sender.send(callback);
                    }
                    break;
                }
                CallbackRequest::Incomplete => {
                    let _ = write_callback_response(&mut stream, 400, incomplete_page()).await;
                }
                CallbackRequest::NotFound => {
                    let _ = write_callback_response(&mut stream, 404, not_found_page()).await;
                }
            }
        }
    };
    let _ = timeout(CALLBACK_LIFETIME, serve).await;
}

async fn read_callback_request(
    stream: &mut TcpStream,
    target: &CallbackTarget,
    expected_state: &str,
) -> CallbackRequest {
    let request = match timeout(CALLBACK_READ_TIMEOUT, read_http_headers(stream)).await {
        Ok(Some(request)) => request,
        _ => return CallbackRequest::NotFound,
    };
    let Some(request_line) = request.lines().next() else {
        return CallbackRequest::NotFound;
    };
    let mut parts = request_line.split_whitespace();
    if parts.next() != Some("GET") {
        return CallbackRequest::NotFound;
    }
    let Some(request_target) = parts.next() else {
        return CallbackRequest::NotFound;
    };
    let request_url = if request_target.starts_with("http://") {
        reqwest::Url::parse(request_target).ok()
    } else if request_target.starts_with('/') {
        reqwest::Url::parse(&format!("http://localhost{request_target}")).ok()
    } else {
        None
    };
    let Some(request_url) = request_url else {
        return CallbackRequest::NotFound;
    };
    if request_url.path() != target.path {
        return CallbackRequest::NotFound;
    }
    let code = request_url
        .query_pairs()
        .find(|(key, _)| key == "code")
        .map(|(_, value)| value.into_owned())
        .unwrap_or_default();
    let state = request_url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.into_owned())
        .unwrap_or_default();
    if code.is_empty() || state != expected_state {
        return CallbackRequest::Incomplete;
    }

    let mut callback = target.redirect_uri.clone();
    callback.set_query(request_url.query());
    callback.set_fragment(request_url.fragment());
    CallbackRequest::Authorized(callback.to_string())
}

async fn read_http_headers(stream: &mut TcpStream) -> Option<String> {
    let mut bytes = Vec::with_capacity(2048);
    let mut chunk = [0_u8; 1024];
    loop {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > MAX_CALLBACK_REQUEST_BYTES {
            return None;
        }
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8(bytes).ok()
}

async fn write_callback_response(
    stream: &mut TcpStream,
    status: u16,
    body: &'static str,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Not Found",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

fn success_page() -> &'static str {
    r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>CONST API</title><style>body{font-family:system-ui,sans-serif;margin:0;min-height:100vh;display:grid;place-items:center;background:#f6f7f9;color:#172033}.card{max-width:520px;margin:24px;padding:28px;border-radius:14px;background:#fff;box-shadow:0 8px 30px #17203318}h1{font-size:22px;margin:0 0 10px}p{line-height:1.6;margin:0;color:#526078}.secondary{margin-top:10px;font-size:14px;color:#7a8498}</style></head><body><main class="card"><h1>Authorization received</h1><p>You can close this page and return to CONST API.</p><p class="secondary" lang="zh-CN">授权信息已收到，可以关闭此页面并返回 CONST API。</p></main></body></html>"#
}

fn incomplete_page() -> &'static str {
    r#"<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>CONST API</title><style>body{font-family:system-ui,sans-serif;margin:0;min-height:100vh;display:grid;place-items:center;background:#f6f7f9;color:#172033}.card{max-width:520px;margin:24px;padding:28px;border-radius:14px;background:#fff;box-shadow:0 8px 30px #17203318}h1{font-size:22px;margin:0 0 10px}p{line-height:1.6;margin:0;color:#526078}</style></head><body><main class="card"><h1>授权尚未完成</h1><p>请返回 CONST API，仍可按原方式复制并粘贴回调地址。<br>Return to CONST API to continue with the callback address.</p></main></body></html>"#
}

fn not_found_page() -> &'static str {
    r#"<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>CONST API</title></head><body></body></html>"#
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unused_redirect(path: &str) -> (u16, String) {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("reserve port");
        let port = listener.local_addr().expect("reserved address").port();
        drop(listener);
        (port, format!("http://127.0.0.1:{port}{path}"))
    }

    async fn http_get(port: u16, target: &str) -> String {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .expect("connect callback listener");
        stream
            .write_all(
                format!(
                    "GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .expect("write callback request");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("read callback response");
        String::from_utf8(response).expect("UTF-8 callback response")
    }

    #[test]
    fn accepts_only_supported_loopback_redirects() {
        assert!(CallbackTarget::parse("http://localhost:1455/auth/callback").is_some());
        assert!(CallbackTarget::parse("http://127.0.0.1:56121/callback").is_some());
        assert!(CallbackTarget::parse("https://platform.claude.com/oauth/code/callback").is_none());
        assert!(CallbackTarget::parse("http://0.0.0.0:1455/callback").is_none());
        assert!(CallbackTarget::parse("http://example.com:1455/callback").is_none());
    }

    #[tokio::test]
    async fn captures_matching_callback_once_without_exposing_code_in_page() {
        let manager = SubscriptionOAuthCallbackManager::default();
        let (port, redirect_uri) = unused_redirect("/auth/callback");
        assert!(manager.prepare("expected-state", &redirect_uri).await);

        let waiting = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .wait_with_timeout("expected-state", Duration::from_secs(2))
                    .await
            })
        };
        let response = http_get(port, "/auth/callback?code=secret-code&state=expected-state").await;
        let callback = waiting
            .await
            .expect("join callback wait")
            .expect("captured callback");

        assert!(response.starts_with("HTTP/1.1 200 OK"));
        let english = response
            .find("<h1>Authorization received</h1>")
            .expect("English success heading");
        let chinese = response
            .find("授权信息已收到")
            .expect("Chinese helper text");
        assert!(english < chinese);
        assert!(!response.contains("secret-code"));
        assert!(callback.contains("code=secret-code"));
        assert!(callback.contains("state=expected-state"));
    }

    #[tokio::test]
    async fn ignores_wrong_state_and_keeps_waiting_for_valid_callback() {
        let manager = SubscriptionOAuthCallbackManager::default();
        let (port, redirect_uri) = unused_redirect("/callback");
        assert!(manager.prepare("expected-state", &redirect_uri).await);

        let waiting = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .wait_with_timeout("expected-state", Duration::from_secs(2))
                    .await
            })
        };
        let wrong_path = http_get(port, "/other?code=wrong&state=expected-state").await;
        assert!(wrong_path.starts_with("HTTP/1.1 404 Not Found"));
        let rejected = http_get(port, "/callback?code=wrong&state=wrong-state").await;
        assert!(rejected.starts_with("HTTP/1.1 400 Bad Request"));
        let accepted = http_get(port, "/callback?code=right&state=expected-state").await;
        assert!(accepted.starts_with("HTTP/1.1 200 OK"));

        let callback = waiting
            .await
            .expect("join callback wait")
            .expect("captured callback");
        assert!(callback.contains("code=right"));
    }

    #[tokio::test]
    async fn occupied_port_disables_automatic_callback_without_blocking_manual_paste() {
        let occupied = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("occupy callback port");
        let port = occupied.local_addr().expect("occupied address").port();
        let manager = SubscriptionOAuthCallbackManager::default();

        assert!(
            !manager
                .prepare(
                    "expected-state",
                    &format!("http://127.0.0.1:{port}/callback")
                )
                .await
        );
    }

    #[tokio::test]
    async fn cancel_releases_listener_and_unblocks_waiter() {
        let manager = SubscriptionOAuthCallbackManager::default();
        let (port, redirect_uri) = unused_redirect("/callback");
        assert!(manager.prepare("expected-state", &redirect_uri).await);

        let waiting = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .wait_with_timeout("expected-state", Duration::from_secs(2))
                    .await
            })
        };
        manager.cancel("expected-state").await;
        assert_eq!(waiting.await.expect("join callback wait"), None);

        let rebound = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
            .await
            .expect("rebind canceled callback port");
        drop(rebound);
    }

    #[tokio::test]
    async fn wait_timeout_releases_listener_without_callback() {
        let manager = SubscriptionOAuthCallbackManager::default();
        let (port, redirect_uri) = unused_redirect("/callback");
        assert!(manager.prepare("expected-state", &redirect_uri).await);

        assert_eq!(
            manager
                .wait_with_timeout("expected-state", Duration::from_millis(50))
                .await,
            None
        );
        let rebound = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
            .await
            .expect("rebind timed-out callback port");
        drop(rebound);
    }

    #[tokio::test]
    async fn regenerating_same_port_cancels_stale_session() {
        let manager = SubscriptionOAuthCallbackManager::default();
        let (port, redirect_uri) = unused_redirect("/callback");
        assert!(manager.prepare("old-state", &redirect_uri).await);
        let old_wait = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .wait_with_timeout("old-state", Duration::from_secs(2))
                    .await
            })
        };

        assert!(manager.prepare("new-state", &redirect_uri).await);
        assert_eq!(old_wait.await.expect("join stale callback wait"), None);
        let new_wait = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .wait_with_timeout("new-state", Duration::from_secs(2))
                    .await
            })
        };
        let response = http_get(port, "/callback?code=new-code&state=new-state").await;

        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(
            new_wait
                .await
                .expect("join replacement callback wait")
                .is_some_and(|callback| callback.contains("code=new-code"))
        );
    }
}
