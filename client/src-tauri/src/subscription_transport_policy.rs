use crate::{
    account::select_account_endpoint,
    config::load_config_from_path,
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
    AnthropicMessagesHttp,
}

impl HeaderOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::ResponsesHttp => "responses_http",
            Self::ResponsesWebsocket => "responses_ws",
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
        if rule.operations.is_empty() || rule.operations.len() > 4 {
            return Err(anyhow!("subscription transport rule has no operation"));
        }
        for operation in &mut rule.operations {
            *operation = operation.trim().to_ascii_lowercase();
            if !matches!(
                operation.as_str(),
                "all" | "responses_http" | "responses_ws" | "anthropic_messages_http"
            ) {
                return Err(anyhow!("unsupported subscription transport operation"));
            }
            if (rule.source_driver == "openai_subscription"
                && operation == "anthropic_messages_http")
                || (rule.source_driver == "claude_subscription"
                    && matches!(operation.as_str(), "responses_http" | "responses_ws"))
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

fn current_scope(config: &ClientConfig, endpoint: &str, platform_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(config.account_device_api_key.trim().as_bytes());
    format!(
        "{}\0{}\0{}\0{}",
        endpoint.trim_end_matches('/'),
        platform_id,
        config.account_user_id.trim(),
        hex::encode(hasher.finalize())
    )
}

fn reset_scope(store: &SubscriptionTransportPolicyStore, scope: Option<String>) {
    let mut document = store
        .document
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if document.scope != scope {
        document.scope = scope;
        document.etag = None;
        document.policy = default_policy();
    }
}

fn clear_policy(store: &SubscriptionTransportPolicyStore) {
    let mut document = store
        .document
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    document.scope = None;
    document.etag = None;
    document.policy = default_policy();
}

fn cached_etag(store: &SubscriptionTransportPolicyStore, scope: &str) -> Option<HeaderValue> {
    let document = store
        .document
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    (document.scope.as_deref() == Some(scope))
        .then(|| document.etag.clone())
        .flatten()
}

fn store_policy(
    store: &SubscriptionTransportPolicyStore,
    scope: &str,
    etag: Option<HeaderValue>,
    policy: SubscriptionTransportPolicy,
) {
    let mut document = store
        .document
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if document.scope.as_deref() == Some(scope) {
        document.etag = etag;
        document.policy = policy;
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
        clear_policy(&global_store());
        return Ok(());
    };
    let platform_id = if config.account_platform_id.trim().is_empty() {
        config.platform_id.trim()
    } else {
        config.account_platform_id.trim()
    };
    if platform_id.is_empty() || config.account_user_id.trim().is_empty() {
        clear_policy(&global_store());
        return Ok(());
    }
    let (_, endpoint) = select_account_endpoint(&config, Some(platform_id))?;
    let base_url = endpoint.base_url.trim_end_matches('/');
    let scope = current_scope(&config, base_url, platform_id);
    let store = global_store();
    reset_scope(&store, Some(scope.clone()));
    let _request_guard = store.request_lock.lock().await;
    let url = format!("{base_url}{ACCOUNT_SUBSCRIPTION_TRANSPORT_POLICY_PATH}");
    let mut request = crate::short_http_client()
        .request(Method::GET, url)
        .bearer_auth(config.account_device_api_key.trim());
    if let Some(etag) = cached_etag(&store, &scope) {
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
        clear_policy(&store);
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
    store_policy(&store, &scope, etag, policy);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> SubscriptionTransportPolicy {
        normalize_policy(
            serde_json::from_value(serde_json::json!({
                "schema_version": 1,
                "version": "test",
                "rules": [{
                    "source_driver": "openai_subscription",
                    "operations": ["responses_http", "responses_ws"],
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
