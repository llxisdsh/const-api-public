use crate::{
    account::select_account_endpoint,
    config::{load_config_from_path, supplier_ws_from_endpoint},
    model::{AppState, ClientConfig},
    source_driver::SourceDriverId,
};
use anyhow::{Context, Result, anyhow};
use reqwest::{
    Method, StatusCode,
    header::{ETAG, HeaderMap, HeaderName, HeaderValue, IF_NONE_MATCH},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};

const ACCOUNT_SUBSCRIPTION_TRANSPORT_POLICY_PATH: &str =
    "/api/account/subscription-transport-policy";
const POLICY_SCHEMA_VERSION: u32 = 1;
const MAX_RULES: usize = 32;
const MAX_HEADERS: usize = 64;
const MAX_HEADER_NAME_BYTES: usize = 128;
const MAX_HEADER_VALUE_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HeaderOperation {
    ResponsesHttp,
    ResponsesWebsocket,
    OpenAiImagesHttp,
    OpenAiRealtimeHttp,
    OpenAiRealtimeWebsocket,
    AnthropicMessagesHttp,
}

impl HeaderOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::ResponsesHttp => "responses_http",
            Self::ResponsesWebsocket => "responses_ws",
            Self::OpenAiImagesHttp => "openai_images_http",
            Self::OpenAiRealtimeHttp => "openai_realtime_http",
            Self::OpenAiRealtimeWebsocket => "openai_realtime_ws",
            Self::AnthropicMessagesHttp => "anthropic_messages_http",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
struct SubscriptionTransportPolicy {
    schema_version: u32,
    #[serde(default)]
    rules: Vec<SubscriptionTransportRule>,
}

#[derive(Clone, Debug, Deserialize)]
struct SubscriptionTransportRule {
    source_driver: String,
    operations: Vec<String>,
    headers: Vec<SubscriptionTransportHeader>,
}

#[derive(Clone, Debug, Deserialize)]
struct SubscriptionTransportHeader {
    name: String,
    value: String,
}

#[derive(Default)]
struct PolicyDocument {
    scope: Option<String>,
    etag: Option<HeaderValue>,
    policy: SubscriptionTransportPolicy,
    revision: u64,
}

#[derive(Default)]
pub(crate) struct SubscriptionTransportPolicyStore {
    document: StdMutex<PolicyDocument>,
    request_lock: tokio::sync::Mutex<()>,
}

static GLOBAL_STORE: OnceLock<Arc<SubscriptionTransportPolicyStore>> = OnceLock::new();

fn global_store() -> Arc<SubscriptionTransportPolicyStore> {
    GLOBAL_STORE
        .get_or_init(|| Arc::new(SubscriptionTransportPolicyStore::default()))
        .clone()
}

fn default_policy() -> SubscriptionTransportPolicy {
    SubscriptionTransportPolicy {
        schema_version: POLICY_SCHEMA_VERSION,
        rules: Vec::new(),
    }
}

fn normalize_policy(
    mut policy: SubscriptionTransportPolicy,
) -> Result<SubscriptionTransportPolicy> {
    if policy.schema_version != POLICY_SCHEMA_VERSION {
        return Err(anyhow!("unsupported subscription transport policy schema"));
    }
    if policy.rules.len() > MAX_RULES {
        return Err(anyhow!("too many subscription transport policy rules"));
    }
    let mut seen_operations = std::collections::HashSet::new();
    let mut total_headers = 0;
    for rule in &mut policy.rules {
        rule.source_driver = rule.source_driver.trim().to_ascii_lowercase();
        if rule.source_driver != "openai_subscription"
            && rule.source_driver != "claude_subscription"
        {
            return Err(anyhow!("unsupported subscription transport source"));
        }
        if rule.operations.is_empty() || rule.operations.len() > 7 {
            return Err(anyhow!("subscription transport rule has no operation"));
        }
        for operation in &mut rule.operations {
            *operation = operation.trim().to_ascii_lowercase();
            if !matches!(
                operation.as_str(),
                "all"
                    | "responses_http"
                    | "responses_ws"
                    | "openai_images_http"
                    | "openai_realtime_http"
                    | "openai_realtime_ws"
                    | "anthropic_messages_http"
            ) {
                return Err(anyhow!("unsupported subscription transport operation"));
            }
            if (rule.source_driver == "openai_subscription"
                && operation == "anthropic_messages_http")
                || (rule.source_driver == "claude_subscription"
                    && matches!(
                        operation.as_str(),
                        "responses_http"
                            | "responses_ws"
                            | "openai_images_http"
                            | "openai_realtime_http"
                            | "openai_realtime_ws"
                    ))
            {
                return Err(anyhow!(
                    "subscription transport operation does not match source"
                ));
            }
            if !seen_operations.insert(format!("{}\0{}", rule.source_driver, operation)) {
                return Err(anyhow!("duplicate subscription transport rule"));
            }
        }
        if rule.headers.is_empty() || rule.headers.len() > MAX_HEADERS {
            return Err(anyhow!("invalid subscription transport header count"));
        }
        for header in &mut rule.headers {
            header.name = header.name.trim().to_ascii_lowercase();
            header.value = header.value.trim().to_string();
            if header.name.is_empty()
                || header.name.len() > MAX_HEADER_NAME_BYTES
                || HeaderName::from_bytes(header.name.as_bytes()).is_err()
            {
                return Err(anyhow!("subscription transport header name is not allowed"));
            }
            if header.value.is_empty()
                || header.value.len() > MAX_HEADER_VALUE_BYTES
                || header.value.contains(['\r', '\n'])
                || HeaderValue::from_str(&header.value).is_err()
            {
                return Err(anyhow!("invalid subscription transport header value"));
            }
            total_headers += 1;
        }
    }
    if total_headers > MAX_HEADERS {
        return Err(anyhow!("too many subscription transport headers"));
    }
    Ok(policy)
}

fn source_name(source_driver: SourceDriverId) -> Option<&'static str> {
    match source_driver {
        SourceDriverId::OpenAiSubscription => Some("openai_subscription"),
        SourceDriverId::ClaudeSubscription => Some("claude_subscription"),
        _ => None,
    }
}

fn apply_policy_to_headers(
    policy: &SubscriptionTransportPolicy,
    source_driver: SourceDriverId,
    operation: HeaderOperation,
    headers: &mut HeaderMap,
) {
    let Some(source) = source_name(source_driver) else {
        return;
    };
    let operation = operation.as_str();
    for rule in &policy.rules {
        if rule.source_driver != source
            || !rule
                .operations
                .iter()
                .any(|candidate| candidate == "all" || candidate == operation)
        {
            continue;
        }
        for configured in &rule.headers {
            let Ok(name) = HeaderName::from_bytes(configured.name.as_bytes()) else {
                continue;
            };
            let Ok(value) = HeaderValue::from_str(&configured.value) else {
                continue;
            };
            // An explicitly configured header replaces every existing value with
            // the same name; a missing name is inserted.
            headers.insert(name, value);
        }
    }
}

pub(crate) fn apply_remote_policy_value(
    server_ws_url: &str,
    access_token: &str,
    value: &serde_json::Value,
) -> Result<()> {
    apply_remote_policy(
        &global_store(),
        current_scope(server_ws_url, access_token),
        value,
    )
}

fn apply_remote_policy(
    store: &SubscriptionTransportPolicyStore,
    scope: String,
    value: &serde_json::Value,
) -> Result<()> {
    let policy: SubscriptionTransportPolicy = serde_json::from_value(value.clone())
        .context("decode remote subscription transport policy")?;
    let policy = normalize_policy(policy)?;
    let mut document = store
        .document
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    document.scope = Some(scope);
    document.etag = None;
    document.policy = policy;
    document.revision = document.revision.wrapping_add(1);
    Ok(())
}

pub(crate) fn apply_to_headers(
    source_driver: SourceDriverId,
    operation: HeaderOperation,
    headers: &mut HeaderMap,
) {
    let store = global_store();
    let document = store
        .document
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    apply_policy_to_headers(&document.policy, source_driver, operation, headers);
}

fn current_scope(server_ws_url: &str, access_token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(access_token.trim().as_bytes());
    // Both supplier push and account HTTP pull know this endpoint and device
    // key. Using the same identity avoids clearing a pushed policy on refresh,
    // including when the supplier WS endpoint has a separate hostname.
    format!(
        "{}\0{}",
        server_ws_url.trim().trim_end_matches('/'),
        hex::encode(hasher.finalize())
    )
}

fn reset_scope(
    store: &SubscriptionTransportPolicyStore,
    scope: Option<String>,
) -> (u64, Option<HeaderValue>) {
    let mut document = store
        .document
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if document.scope != scope {
        document.scope = scope;
        document.etag = None;
        document.policy = default_policy();
        document.revision = document.revision.wrapping_add(1);
    }
    (document.revision, document.etag.clone())
}

fn store_policy(
    store: &SubscriptionTransportPolicyStore,
    scope: &str,
    revision: u64,
    etag: Option<HeaderValue>,
    policy: SubscriptionTransportPolicy,
) {
    let mut document = store
        .document
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    // A push or account change while HTTP was in flight supersedes its result,
    // including a rejection from an old device key.
    if document.scope.as_deref() == Some(scope) && document.revision == revision {
        document.etag = etag;
        document.policy = policy;
        document.revision = document.revision.wrapping_add(1);
    }
}

fn active_config(state: &AppState) -> Result<Option<ClientConfig>> {
    let mut config = load_config_from_path(&state.config_path)?;
    let active_key = state
        .platform_api_key
        .lock()
        .map_err(|_| anyhow!("platform API key lock poisoned"))?
        .clone();
    if active_key.trim().is_empty() {
        return Ok(None);
    }
    config.account_device_api_key = active_key;
    Ok(Some(config))
}

pub(crate) async fn sync_for_runtime(state: &AppState) -> Result<()> {
    let Some(config) = active_config(state)? else {
        reset_scope(&global_store(), None);
        return Ok(());
    };
    let platform_id = if config.account_platform_id.trim().is_empty() {
        config.platform_id.trim()
    } else {
        config.account_platform_id.trim()
    };
    if platform_id.is_empty() || config.account_user_id.trim().is_empty() {
        reset_scope(&global_store(), None);
        return Ok(());
    }
    let (_, endpoint) = select_account_endpoint(&config, Some(platform_id))?;
    let base_url = endpoint.base_url.trim_end_matches('/');
    let server_ws_url = supplier_ws_from_endpoint(&endpoint)
        .ok_or_else(|| anyhow!("subscription transport policy endpoint is invalid"))?;
    let scope = current_scope(&server_ws_url, &config.account_device_api_key);
    let store = global_store();
    let url = format!("{base_url}{ACCOUNT_SUBSCRIPTION_TRANSPORT_POLICY_PATH}");
    refresh_policy(
        &store,
        &crate::short_http_client(),
        &url,
        &scope,
        &config.account_device_api_key,
    )
    .await
}

async fn refresh_policy(
    store: &SubscriptionTransportPolicyStore,
    client: &reqwest::Client,
    url: &str,
    scope: &str,
    access_token: &str,
) -> Result<()> {
    let _request_guard = store.request_lock.lock().await;
    let (revision, etag) = reset_scope(store, Some(scope.to_owned()));
    let mut request = client
        .request(Method::GET, url)
        .bearer_auth(access_token.trim());
    if let Some(etag) = etag {
        request = request.header(IF_NONE_MATCH, etag);
    }
    let response = request
        .send()
        .await
        .context("fetch subscription transport policy")?;
    if response.status() == StatusCode::NOT_MODIFIED {
        return Ok(());
    }
    if matches!(
        response.status(),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
    ) {
        store_policy(store, scope, revision, None, default_policy());
        return Err(anyhow!("subscription transport policy is unavailable"));
    }
    if !response.status().is_success() {
        return Err(anyhow!(
            "subscription transport policy returned HTTP {}",
            response.status()
        ));
    }
    let etag = response.headers().get(ETAG).cloned();
    let body = response.bytes().await?;
    if body.len() > 256 * 1024 {
        return Err(anyhow!(
            "subscription transport policy response is too large"
        ));
    }
    let policy: SubscriptionTransportPolicy =
        serde_json::from_slice(&body).context("decode subscription transport policy")?;
    let policy = normalize_policy(policy)?;
    store_policy(store, scope, revision, etag, policy);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn policy_value(hint: &str) -> serde_json::Value {
        serde_json::json!({
            "schema_version": 1,
            "rules": [{
                "source_driver": "openai_subscription", "operations": ["all"],
                "headers": [{"name": "x-openai-account-routing-override", "value": hint}]
            }]
        })
    }

    fn routing_hint(store: &SubscriptionTransportPolicyStore) -> Option<HeaderValue> {
        let mut headers = HeaderMap::new();
        apply_policy_to_headers(
            &store.document.lock().unwrap().policy,
            SourceDriverId::OpenAiSubscription,
            HeaderOperation::ResponsesWebsocket,
            &mut headers,
        );
        headers.get("x-openai-account-routing-override").cloned()
    }

    // Delay the HTTP response until the test has inspected the effective policy
    // or delivered a newer push, without timing-dependent sleeps.
    async fn delayed_response() -> (
        String,
        tokio::sync::oneshot::Receiver<String>,
        tokio::sync::oneshot::Sender<String>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (request_tx, request_rx) = tokio::sync::oneshot::channel();
        let (response_tx, response_rx) = tokio::sync::oneshot::channel::<String>();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 1024];
            while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                let count = socket.read(&mut buffer).await.unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&buffer[..count]);
            }
            request_tx
                .send(String::from_utf8(request).unwrap())
                .unwrap();
            socket
                .write_all(response_rx.await.unwrap().as_bytes())
                .await
                .unwrap();
        });
        (url, request_rx, response_tx, task)
    }

    fn http_response(status: u16, body: &str) -> String {
        format!(
            "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nETag: \"policy-v1\"\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn start_refresh(
        store: Arc<SubscriptionTransportPolicyStore>,
        scope: String,
        url: String,
    ) -> tokio::task::JoinHandle<Result<()>> {
        tokio::spawn(async move {
            let client = reqwest::Client::builder()
                .no_proxy()
                .timeout(std::time::Duration::from_secs(3))
                .build()
                .unwrap();
            refresh_policy(&store, &client, &url, &scope, "test-device-key").await
        })
    }

    #[tokio::test]
    async fn pushed_policy_survives_pending_and_failed_same_scope_refresh() {
        let store = Arc::new(SubscriptionTransportPolicyStore::default());
        let scope = current_scope("wss://supplier.example/supplier/ws", "device-a");
        apply_remote_policy(&store, scope.clone(), &policy_value("us_cr")).unwrap();
        let (url, request, reply, server) = delayed_response().await;
        let refresh = start_refresh(store.clone(), scope, url);
        request.await.unwrap();
        assert_eq!(routing_hint(&store).unwrap(), "us_cr");
        reply.send(http_response(500, "")).unwrap();
        assert!(refresh.await.unwrap().is_err());
        server.await.unwrap();
        assert_eq!(routing_hint(&store).unwrap(), "us_cr");
    }

    #[tokio::test]
    async fn newer_push_supersedes_inflight_http_policy_or_rejection() {
        for status in [200, 401, 403, 404] {
            let store = Arc::new(SubscriptionTransportPolicyStore::default());
            let scope = current_scope("wss://supplier.example/supplier/ws", "device-a");
            apply_remote_policy(&store, scope.clone(), &policy_value("initial")).unwrap();
            let (url, request, reply, server) = delayed_response().await;
            let refresh = start_refresh(store.clone(), scope.clone(), url);
            request.await.unwrap();
            apply_remote_policy(&store, scope, &policy_value("newer-push")).unwrap();
            reply
                .send(http_response(
                    status,
                    &policy_value("older-pull").to_string(),
                ))
                .unwrap();
            assert_eq!(refresh.await.unwrap().is_ok(), status == 200);
            server.await.unwrap();
            assert_eq!(routing_hint(&store).unwrap(), "newer-push");
            assert!(store.document.lock().unwrap().etag.is_none());
        }
    }

    #[tokio::test]
    async fn http_refresh_applies_updates_and_reuses_etag() {
        let store = Arc::new(SubscriptionTransportPolicyStore::default());
        let scope = current_scope("wss://supplier.example/supplier/ws", "device-a");
        apply_remote_policy(&store, scope.clone(), &policy_value("initial")).unwrap();
        for (status, expected_etag, body) in [
            (200, false, policy_value("updated").to_string()),
            (304, true, String::new()),
            (
                200,
                true,
                serde_json::json!({"schema_version": 1, "rules": []}).to_string(),
            ),
        ] {
            let (url, request, reply, server) = delayed_response().await;
            let refresh = start_refresh(store.clone(), scope.clone(), url);
            let request = request.await.unwrap().to_ascii_lowercase();
            assert_eq!(
                request.contains("if-none-match: \"policy-v1\""),
                expected_etag
            );
            reply.send(http_response(status, &body)).unwrap();
            refresh.await.unwrap().unwrap();
            server.await.unwrap();
            if body.contains("\"rules\":[]") {
                assert!(
                    routing_hint(&store).is_none(),
                    "removing policy must take effect"
                );
            } else {
                assert_eq!(routing_hint(&store).unwrap(), "updated");
            }
        }
    }

    #[tokio::test]
    async fn scope_switch_clears_old_policy_and_rejects_old_inflight_result() {
        let store = Arc::new(SubscriptionTransportPolicyStore::default());
        let first = current_scope("wss://supplier.example/supplier/ws", "device-a");
        let second = current_scope("wss://supplier.example/supplier/ws", "device-b");
        apply_remote_policy(&store, first.clone(), &policy_value("account-a")).unwrap();
        let (url, request, reply, server) = delayed_response().await;
        let refresh = start_refresh(store.clone(), first, url);
        request.await.unwrap();
        reset_scope(&store, Some(second));
        assert!(routing_hint(&store).is_none());
        reply
            .send(http_response(200, &policy_value("account-a").to_string()))
            .unwrap();
        refresh.await.unwrap().unwrap();
        server.await.unwrap();
        assert!(routing_hint(&store).is_none());
    }

    #[test]
    fn invalid_push_preserves_policy_and_scope_separates_accounts_and_servers() {
        let store = SubscriptionTransportPolicyStore::default();
        let scope = current_scope("wss://supplier.example/supplier/ws", "device-a");
        assert_eq!(
            scope,
            current_scope(" wss://supplier.example/supplier/ws/ ", " device-a ")
        );
        assert_ne!(
            scope,
            current_scope("wss://other.example/supplier/ws", "device-a")
        );
        assert_ne!(
            scope,
            current_scope("wss://supplier.example/supplier/ws", "device-b")
        );
        apply_remote_policy(&store, scope.clone(), &policy_value("us_cr")).unwrap();
        assert!(
            apply_remote_policy(&store, scope, &serde_json::json!({"schema_version": 99})).is_err()
        );
        assert_eq!(routing_hint(&store).unwrap(), "us_cr");
        reset_scope(&store, None);
        assert!(
            routing_hint(&store).is_none(),
            "logout must clear pushed policy too"
        );
    }

    fn policy() -> SubscriptionTransportPolicy {
        normalize_policy(
            serde_json::from_value(serde_json::json!({
                "schema_version": 1,
                "version": "test",
                "rules": [{
                    "source_driver": "openai_subscription",
                    "operations": [
                        "responses_http",
                        "responses_ws",
                        "openai_images_http",
                        "openai_realtime_http",
                        "openai_realtime_ws"
                    ],
                    "headers": [{"name": "x-openai-account-routing-override", "value": "us_cr"}]
                }, {
                    "source_driver": "claude_subscription",
                    "operations": ["anthropic_messages_http"],
                    "headers": [{"name": "x-const-routing-hint", "value": "stable"}]
                }]
            }))
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn policy_overrides_or_inserts_and_is_scoped_to_subscription_operation() {
        let policy = policy();
        let mut openai = HeaderMap::new();
        apply_policy_to_headers(
            &policy,
            SourceDriverId::OpenAiSubscription,
            HeaderOperation::ResponsesHttp,
            &mut openai,
        );
        assert_eq!(openai["x-openai-account-routing-override"], "us_cr");

        let mut ws = HeaderMap::new();
        apply_policy_to_headers(
            &policy,
            SourceDriverId::OpenAiSubscription,
            HeaderOperation::ResponsesWebsocket,
            &mut ws,
        );
        assert_eq!(ws["x-openai-account-routing-override"], "us_cr");

        for operation in [
            HeaderOperation::OpenAiImagesHttp,
            HeaderOperation::OpenAiRealtimeHttp,
            HeaderOperation::OpenAiRealtimeWebsocket,
        ] {
            let mut headers = HeaderMap::new();
            apply_policy_to_headers(
                &policy,
                SourceDriverId::OpenAiSubscription,
                operation,
                &mut headers,
            );
            assert_eq!(headers["x-openai-account-routing-override"], "us_cr");
        }

        let mut claude = HeaderMap::new();
        apply_policy_to_headers(
            &policy,
            SourceDriverId::ClaudeSubscription,
            HeaderOperation::AnthropicMessagesHttp,
            &mut claude,
        );
        assert_eq!(claude["x-const-routing-hint"], "stable");

        let mut existing = HeaderMap::new();
        existing.insert("x-const-routing-hint", HeaderValue::from_static("caller"));
        apply_policy_to_headers(
            &policy,
            SourceDriverId::ClaudeSubscription,
            HeaderOperation::AnthropicMessagesHttp,
            &mut existing,
        );
        assert_eq!(existing["x-const-routing-hint"], "stable");
    }

    #[test]
    fn policy_allows_configured_special_headers_but_rejects_injection_values() {
        let policy = normalize_policy(
            serde_json::from_value(serde_json::json!({
                "schema_version": 1,
                "rules": [{"source_driver":"claude_subscription","operations":["all"],"headers":[
                    {"name":"authorization","value":"configured-token"},
                    {"name":"cookie","value":"configured-cookie"},
                    {"name":"proxy-authorization","value":"configured-proxy-token"}
                ]}]
            }))
            .unwrap(),
        )
        .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("caller-token"));
        headers.insert("cookie", HeaderValue::from_static("caller-cookie"));
        apply_policy_to_headers(
            &policy,
            SourceDriverId::ClaudeSubscription,
            HeaderOperation::AnthropicMessagesHttp,
            &mut headers,
        );
        assert_eq!(headers["authorization"], "configured-token");
        assert_eq!(headers["cookie"], "configured-cookie");
        assert_eq!(headers["proxy-authorization"], "configured-proxy-token");

        let result = normalize_policy(serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "rules": [{"source_driver":"claude_subscription","operations":["all"],"headers":[{"name":"x-test","value":"ok\r\nInjected: yes"}]}]
        })).unwrap());
        assert!(result.is_err());
    }
}
