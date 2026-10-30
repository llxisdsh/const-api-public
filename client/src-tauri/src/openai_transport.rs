//! OpenAI subscription transport state. Never share account cookies with callers
//! or other subscription accounts. Public API/custom channels do not enter here.

use anyhow::Result;
use reqwest::{
    Client, Url,
    cookie::{CookieStore, Jar},
    header::{COOKIE, HeaderMap, HeaderValue, SET_COOKIE},
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

mod custom_ca;
pub(crate) mod dialer;
pub(crate) use custom_ca::tls_config;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct TransportOptions {
    pub(crate) use_system_proxy: bool,
    pub(crate) profile: crate::codex_identity::CodexProfile,
}

impl TransportOptions {
    pub(crate) fn for_channel(channel: &crate::model::ChannelConfig) -> Self {
        Self {
            use_system_proxy: channel.v2.use_system_proxy,
            profile: crate::codex_identity::CodexProfile::from_setting(
                &channel.v2.user_agent_profile,
            ),
        }
    }
}

const MAX_ACCOUNTS: usize = 128;

struct AccountTransport {
    cookies: Arc<InfrastructureCookies>,
    http: OnceLock<crate::http_client::HttpClient>,
}

impl Default for AccountTransport {
    fn default() -> Self {
        Self {
            cookies: Arc::new(InfrastructureCookies::default()),
            http: OnceLock::new(),
        }
    }
}

impl AccountTransport {
    fn client(&self, options: TransportOptions) -> Result<Client> {
        if let Some(client) = self.http.get() {
            return Ok(client.snapshot());
        }
        let tls = tls_config()?;
        let mut http_tls = tls.as_ref().clone();
        // reqwest preserves a preconfigured ALPN list. HTTPS still negotiates
        // HTTP/2; the shared WS config deliberately offers no ALPN.
        http_tls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        let cookies = self.cookies.clone();
        let client = crate::http_client::HttpClient::new(move || {
            let builder = Client::builder()
                .tls_backend_preconfigured(http_tls.clone())
                .cookie_provider(cookies.clone())
                .connect_timeout(Duration::from_secs(10))
                .timeout(crate::MODEL_REQUEST_TIMEOUT)
                .retry(reqwest::retry::never())
                // Subscription endpoints are final destinations. Do not carry
                // auth, account headers or explicit cookies across redirects.
                .redirect(reqwest::redirect::Policy::none());
            if options.use_system_proxy {
                builder
            } else {
                builder.no_proxy()
            }
        })?;
        Ok(self.http.get_or_init(|| client).snapshot())
    }
}

#[derive(Default)]
struct AccountTransports(Mutex<VecDeque<((String, TransportOptions), Arc<AccountTransport>)>>);

impl AccountTransports {
    fn get(
        &self,
        credential: &serde_json::Value,
        options: TransportOptions,
    ) -> Arc<AccountTransport> {
        let Some(key) = crate::detection::subscription_account_identity_from_value(credential)
        else {
            // Missing identity must never select a shared anonymous account jar.
            return Arc::new(AccountTransport::default());
        };
        let key = (key, options);
        let mut entries = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(index) = entries.iter().position(|(id, _)| *id == key) {
            let entry = entries.remove(index).expect("located account transport");
            let transport = entry.1.clone();
            entries.push_back(entry);
            return transport;
        }
        let transport = Arc::new(AccountTransport::default());
        if entries.len() >= MAX_ACCOUNTS {
            entries.pop_front();
        }
        entries.push_back((key, transport.clone()));
        transport
    }
}

fn account_transport(
    credential: &serde_json::Value,
    options: TransportOptions,
) -> Arc<AccountTransport> {
    static TRANSPORTS: OnceLock<AccountTransports> = OnceLock::new();
    TRANSPORTS
        .get_or_init(AccountTransports::default)
        .get(credential, options)
}

/// Select the subscription client before constructing a request. Injected local
/// endpoints keep their caller-owned proxy/DNS/timeouts (including test clients).
pub(crate) fn http_client(
    client: &Client,
    url: &str,
    credential: &serde_json::Value,
) -> Result<Client> {
    http_client_with_options(client, url, credential, TransportOptions::default())
}

pub(crate) fn http_client_with_options(
    client: &Client,
    url: &str,
    credential: &serde_json::Value,
    options: TransportOptions,
) -> Result<Client> {
    let url = Url::parse(url)?;
    if url.scheme() != "https"
        || !url.host_str().is_some_and(|host| {
            is_chatgpt_host(host) || matches!(host, "auth.openai.com" | "api.openai.com")
        })
    {
        return Ok(client.clone());
    }
    account_transport(credential, options).client(options)
}

fn is_chatgpt_host(host: &str) -> bool {
    matches!(
        host,
        "chatgpt.com" | "chat.openai.com" | "chatgpt-staging.com"
    ) || host.ends_with(".chatgpt.com")
        || host.ends_with(".chatgpt-staging.com")
}

fn cookie_url(uri: &http::Uri) -> Option<Url> {
    let mut url = Url::parse(&uri.to_string()).ok()?;
    if url.scheme() == "wss" {
        url.set_scheme("https").ok()?;
    }
    accepts_cookies(&url).then_some(url)
}

fn accepts_cookies(url: &Url) -> bool {
    url.scheme() == "https" && url.host_str().is_some_and(is_chatgpt_host)
}

fn allowed_cookie(name: &str) -> bool {
    // Match ref/codex's ChatGPT infrastructure allowlist, including challenge
    // cookies. Never expand this to account/login/session cookies.
    matches!(
        name,
        "__cf_bm"
            | "__cflb"
            | "__cfruid"
            | "__cfseq"
            | "__cfwaitingroom"
            | "__oailb"
            | "_cfuvid"
            | "cf_clearance"
            | "cf_ob_info"
            | "cf_use_ob"
    ) || name.starts_with("cf_chl_")
}

#[derive(Default)]
struct InfrastructureCookies(Jar);

impl CookieStore for InfrastructureCookies {
    fn set_cookies(&self, headers: &mut dyn Iterator<Item = &HeaderValue>, url: &Url) {
        if accepts_cookies(url) {
            self.0.set_cookies(
                &mut headers.filter(|header| {
                    header
                        .to_str()
                        .ok()
                        .and_then(|value| value.split_once('='))
                        .is_some_and(|(name, _)| allowed_cookie(name.trim()))
                }),
                url,
            );
        }
    }

    fn cookies(&self, url: &Url) -> Option<HeaderValue> {
        if !accepts_cookies(url) {
            return None;
        }
        let mut header = self.0.cookies(url)?;
        header.set_sensitive(true);
        Some(header)
    }
}

/// Private request metadata, never a wire header or a downstream credential.
#[derive(Clone)]
pub(crate) struct WebSocketCookies {
    jar: Arc<InfrastructureCookies>,
    url: Url,
}

pub(crate) fn attach_websocket_cookies(
    request: &mut http::Request<()>,
    credential: &serde_json::Value,
    options: TransportOptions,
) {
    request.extensions_mut().insert(options);
    if let Some(url) = cookie_url(request.uri()) {
        request.extensions_mut().insert(WebSocketCookies {
            jar: account_transport(credential, options).cookies.clone(),
            url,
        });
    }
}

impl WebSocketCookies {
    pub(crate) fn apply(&self, headers: &mut HeaderMap) {
        if !headers.contains_key(COOKIE) {
            if let Some(cookie) = self.jar.cookies(&self.url) {
                headers.insert(COOKIE, cookie);
            }
        }
    }

    pub(crate) fn store(&self, headers: &HeaderMap) {
        self.jar
            .set_cookies(&mut headers.get_all(SET_COOKIE).iter(), &self.url);
    }
}

#[cfg(test)]
mod tests;
