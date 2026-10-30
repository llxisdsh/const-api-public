use crate::model::CodexClientIdentity;
#[cfg(test)]
use crate::model_catalog::{ModelCatalogKind, model_observations_from_value};
use reqwest::header::{HeaderMap, HeaderValue};
use std::sync::OnceLock;
mod installation;
pub(crate) use installation::installation_id;

/// The latest stable Codex wire version validated with this client release.
/// It changes together with the adapter; discovering a newer package version
/// at runtime is not evidence that this binary implements its wire contract.
/// The models endpoint also gates its catalog on this version's `client_version`.
pub(crate) const BUNDLED_CODEX_CLIENT_VERSION: &str = "0.163.0";
/// The subscription adapter presents one official interactive Codex client.
/// The User-Agent prefix, `originator`, and `version` header must always name
/// the same client family. This is a compatibility invariant, not evidence
/// about upstream capacity or risk-control decisions.
pub(crate) const CODEX_ORIGINATOR: &str = "Codex Desktop";
// Verified against the Windows Desktop package on 2026-10-30: owl-app.ini
// declares this app version; packaged codex.exe matches the running Core by SHA256.
const DESKTOP_CORE_VERSION: &str = "0.166.0-alpha.5.1";
const DESKTOP_APP_VERSION: &str = "26.1027.30824";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum CodexProfile {
    #[default]
    Desktop,
    Tui,
    Cli,
    Exec,
}

impl CodexProfile {
    pub(crate) fn from_setting(value: &str) -> Self {
        match value.trim() {
            "codex_tui" | "codex" => Self::Tui,
            "codex_cli" => Self::Cli,
            "codex_exec" => Self::Exec,
            _ => Self::Desktop,
        }
    }

    pub(crate) fn identity(self) -> CodexClientIdentity {
        static DESKTOP: OnceLock<CodexClientIdentity> = OnceLock::new();
        static TUI: OnceLock<CodexClientIdentity> = OnceLock::new();
        static CLI: OnceLock<CodexClientIdentity> = OnceLock::new();
        static EXEC: OnceLock<CodexClientIdentity> = OnceLock::new();
        let slot = match self {
            Self::Desktop => &DESKTOP,
            Self::Tui => &TUI,
            Self::Cli => &CLI,
            Self::Exec => &EXEC,
        };
        slot.get_or_init(|| self.build_identity()).clone()
    }

    fn build_identity(self) -> CodexClientIdentity {
        let (originator, version, app_version) = match self {
            Self::Desktop => (
                CODEX_ORIGINATOR,
                DESKTOP_CORE_VERSION,
                Some(DESKTOP_APP_VERSION),
            ),
            Self::Tui => (
                "codex-tui",
                BUNDLED_CODEX_CLIENT_VERSION,
                Some(BUNDLED_CODEX_CLIENT_VERSION),
            ),
            Self::Cli => ("codex_cli_rs", BUNDLED_CODEX_CLIENT_VERSION, None),
            Self::Exec => ("codex_exec", BUNDLED_CODEX_CLIENT_VERSION, None),
        };
        build_codex_identity(originator, version, app_version, "bundled", 0)
    }
}

pub(crate) fn codex_identity_for_channel(
    channel: &crate::model::ChannelConfig,
) -> CodexClientIdentity {
    CodexProfile::from_setting(&channel.v2.user_agent_profile).identity()
}
/// Older Codex HTTP calls negotiated this beta. Current Codex sends no beta over
/// HTTP and only `responses_websockets=…` in its WebSocket handshake.
const CODEX_LEGACY_RESPONSES_BETA: &str = "responses=experimental";

fn build_codex_identity(
    originator: &str,
    version: &str,
    app_version: Option<&str>,
    source: &str,
    checked_at_unix: i64,
) -> CodexClientIdentity {
    let os_info = os_info::get();
    let prefix = format!(
        "{originator}/{version} ({} {}; {}) {}",
        os_info.os_type(),
        os_info.version(),
        os_info.architecture().unwrap_or("unknown"),
        if originator == CODEX_ORIGINATOR {
            "unknown".into()
        } else {
            codex_terminal_user_agent_token()
        },
    );
    // The official TUI reaches the model through its app-server, which appends
    // the initialized client's `(name; version)` to every User-Agent.
    let user_agent = sanitize_codex_user_agent(
        app_version.map_or_else(
            || prefix.clone(),
            |app| format!("{prefix} ({originator}; {app})"),
        ),
        &prefix,
    );

    log::debug!(
        "[const-api][codex] identity constructed: user_agent={}, originator={}, version={}",
        user_agent,
        originator,
        version
    );

    CodexClientIdentity {
        version: version.to_string(),
        user_agent,
        originator: originator.to_string(),
        checked_at_unix,
        source: source.to_string(),
    }
}

fn sanitize_codex_user_agent(candidate: String, fallback: &str) -> String {
    if HeaderValue::from_str(&candidate).is_ok() {
        return candidate;
    }
    let sanitized = candidate
        .chars()
        .map(|character| {
            if matches!(character, ' '..='~') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if HeaderValue::from_str(&sanitized).is_ok() {
        sanitized
    } else if HeaderValue::from_str(fallback).is_ok() {
        fallback.to_string()
    } else {
        CODEX_ORIGINATOR.to_string()
    }
}

fn codex_terminal_user_agent_token() -> String {
    let raw = std::env::var("TERM_PROGRAM")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|program| {
            std::env::var("TERM_PROGRAM_VERSION")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .map_or(program.clone(), |version| format!("{program}/{version}"))
        })
        .or_else(|| {
            std::env::var_os("WT_SESSION")
                .is_some()
                .then(|| "WindowsTerminal".to_string())
        })
        .or_else(|| {
            std::env::var("TERM")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(|| "unknown".to_string());
    raw.chars()
        .map(|character| {
            if character.is_ascii_graphic() {
                character
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
pub(crate) fn bundled_codex_identity() -> CodexClientIdentity {
    CodexProfile::Desktop.identity()
}

#[cfg(test)]
pub(crate) fn codex_manifest_is_valid(value: &serde_json::Value) -> bool {
    !model_observations_from_value(ModelCatalogKind::Codex, value, "codex_manifest").is_empty()
}

pub(crate) fn apply_codex_control_identity_headers(
    request: reqwest::RequestBuilder,
    identity: &CodexClientIdentity,
) -> reqwest::RequestBuilder {
    request.headers(codex_control_identity_headers(identity))
}

pub(crate) fn codex_control_identity_headers(identity: &CodexClientIdentity) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Ok(originator) = HeaderValue::from_str(&identity.originator) {
        headers.insert("originator", originator);
    }
    if let Ok(user_agent) = HeaderValue::from_str(&identity.user_agent) {
        headers.insert(reqwest::header::USER_AGENT, user_agent);
    }
    headers
}

pub(crate) fn codex_model_identity_headers(identity: &CodexClientIdentity) -> HeaderMap {
    let mut headers = codex_control_identity_headers(identity);
    if let Ok(version) = HeaderValue::from_str(&identity.version) {
        headers.insert("version", version);
    }
    headers
}

/// Rebuild the ChatGPT-backend routing hint from the final upstream request.
///
/// A caller's hint describes its own model and can become stale after CONST
/// model substitution. The subscription transport applies operator header
/// policy after this helper, so an explicit configured override still wins.
pub(crate) fn set_codex_routing_hint(
    headers: &mut HeaderMap,
    model: Option<&str>,
    service_tier: Option<&str>,
) {
    headers.remove("x-codex-routing-hint");
    let Some(model) = model.map(str::trim).filter(|model| !model.is_empty()) else {
        return;
    };
    let hint = match service_tier.map(str::trim).filter(|tier| !tier.is_empty()) {
        Some(tier) => format!("model={model};tier={tier}"),
        None => format!("model={model}"),
    };
    if let Ok(value) = HeaderValue::from_str(&hint) {
        headers.insert("x-codex-routing-hint", value);
    }
}

/// Drop only the legacy Responses beta from forwarded native context; keep any
/// independent beta negotiation such as the WebSocket protocol version.
fn strip_codex_legacy_responses_beta(headers: &mut HeaderMap) {
    let Some(value) = headers
        .get("openai-beta")
        .and_then(|value| value.to_str().ok())
    else {
        return;
    };
    let kept = value
        .split(',')
        .map(str::trim)
        .filter(|token| !token.is_empty() && *token != CODEX_LEGACY_RESPONSES_BETA)
        .collect::<Vec<_>>()
        .join(", ");
    match HeaderValue::from_str(&kept) {
        Ok(value) if !kept.is_empty() => {
            headers.insert("openai-beta", value);
        }
        _ => {
            headers.remove("openai-beta");
        }
    }
}

/// A request is native only when both identity components belong to a known
/// first-party Codex originator. Thread-scoped originator overrides can differ
/// from the process-wide User-Agent prefix, so validate both independently.
pub(crate) fn codex_request_is_native(inbound: Option<&HeaderMap>) -> bool {
    let Some(inbound) = inbound else {
        return false;
    };
    let Some(originator) = bounded_ascii_header(inbound, "originator", 256)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
    else {
        return false;
    };
    let Some(user_agent_originator) = bounded_ascii_header(inbound, "user-agent", 2 * 1024)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .trim()
                .split_once('/')
                .map(|(originator, _)| originator)
        })
    else {
        return false;
    };
    codex_originator_is_first_party(originator)
        && codex_originator_is_first_party(user_agent_originator)
}

fn codex_originator_is_first_party(originator: &str) -> bool {
    matches!(
        originator,
        "codex_cli_rs"
            | "codex_exec"
            | "codex-tui"
            | "codex_vscode"
            | "codex_atlas"
            | "codex_chatgpt_desktop"
    ) || originator.starts_with("Codex ")
}

const CODEX_REQUEST_CONTEXT_HEADERS: &[&str] = &[
    "openai-beta",
    "session-id",
    "thread-id",
    "x-client-request-id",
    "x-codex-installation-id",
    "x-codex-window-id",
    "x-codex-turn-metadata",
    "x-codex-parent-thread-id",
    "x-codex-turn-state",
    "x-codex-beta-features",
    "x-codex-routing-hint",
    "x-openai-subagent",
    "x-openai-memgen-request",
    "x-openai-internal-codex-responses-lite",
    "x-openai-internal-codex-residency",
    "x-oai-attestation",
    "x-responsesapi-include-timing-metrics",
];

// A subscription adapter owns the upstream Codex identity. Keep protocol
// session/turn context, but do not attach a caller's device identity or
// attestation to the adapter's account. The native Responses transport carries
// installation identity in client_metadata when it needs it; it does not use
// either value as a generic subscription handshake header.
const CODEX_SUBSCRIPTION_CONTEXT_HEADERS: &[&str] = &[
    "openai-beta",
    "session-id",
    "thread-id",
    "x-client-request-id",
    "x-codex-window-id",
    "x-codex-turn-metadata",
    "x-codex-parent-thread-id",
    "x-codex-turn-state",
    "x-codex-beta-features",
    "x-codex-routing-hint",
    "x-openai-subagent",
    "x-openai-memgen-request",
    "x-openai-internal-codex-responses-lite",
    "x-openai-internal-codex-residency",
    "x-responsesapi-include-timing-metrics",
];

/// Preserve a first-party caller's model identity and copy only known Codex
/// request context. If an older caller omits `Version`, derive it best-effort
/// from that caller's User-Agent; otherwise use the bundled compatible identity.
pub(crate) fn codex_model_request_context_headers(
    inbound: Option<&HeaderMap>,
    fallback: &CodexClientIdentity,
) -> HeaderMap {
    let native_inbound = inbound.filter(|headers| codex_request_is_native(Some(headers)));
    let mut headers = native_inbound
        .and_then(complete_codex_model_identity_headers)
        .unwrap_or_else(|| codex_model_identity_headers(fallback));
    let Some(inbound) = native_inbound else {
        return headers;
    };
    copy_codex_request_context_headers(inbound, &mut headers);
    headers
}

/// Build the identity for the retained OpenAI/Codex subscription adapter.
///
/// A subscription request may arrive through a first-party client, but the
/// adapter owns the upstream credential and must not forward that caller's
/// `originator`, `User-Agent`, or `version`. Those fields identify a different
/// product and can make one subscription account appear to be several clients.
/// Known turn/session context remains protocol data and is copied below.
pub(crate) fn codex_subscription_model_request_context_headers(
    inbound: Option<&HeaderMap>,
    fallback: &CodexClientIdentity,
) -> HeaderMap {
    let mut headers = codex_model_identity_headers(fallback);
    if let Some(inbound) = inbound.filter(|headers| codex_request_is_native(Some(headers))) {
        copy_codex_subscription_context_headers(inbound, &mut headers);
        strip_codex_legacy_responses_beta(&mut headers);
    }
    headers
}

fn copy_codex_request_context_headers(inbound: &HeaderMap, headers: &mut HeaderMap) {
    for &name in CODEX_REQUEST_CONTEXT_HEADERS {
        let Some(value) = bounded_ascii_header(inbound, name, 32 * 1024) else {
            continue;
        };
        headers.insert(
            reqwest::header::HeaderName::from_static(name),
            value.clone(),
        );
    }
}

fn copy_codex_subscription_context_headers(inbound: &HeaderMap, headers: &mut HeaderMap) {
    for &name in CODEX_SUBSCRIPTION_CONTEXT_HEADERS {
        let Some(value) = bounded_ascii_header(inbound, name, 32 * 1024).and_then(|value| {
            if name == "x-codex-turn-metadata" {
                sanitize_codex_turn_metadata_header(value)
            } else {
                Some(value.clone())
            }
        }) else {
            continue;
        };
        headers.insert(
            reqwest::header::HeaderName::from_static(name),
            value.clone(),
        );
    }
}

fn complete_codex_model_identity_headers(inbound: &HeaderMap) -> Option<HeaderMap> {
    let originator = bounded_ascii_header(inbound, "originator", 256)?;
    let user_agent = bounded_ascii_header(inbound, "user-agent", 2 * 1024)?;
    let version = match bounded_ascii_header(inbound, "version", 256) {
        Some(version) => version.clone(),
        None => HeaderValue::from_str(codex_user_agent_version(user_agent.to_str().ok()?)?).ok()?,
    };
    let mut headers = HeaderMap::new();
    headers.insert("originator", originator.clone());
    headers.insert(reqwest::header::USER_AGENT, user_agent.clone());
    headers.insert("version", version);
    Some(headers)
}

fn codex_user_agent_version(user_agent: &str) -> Option<&str> {
    let (_, suffix) = user_agent.trim().split_once('/')?;
    let version = suffix.split_ascii_whitespace().next()?.trim();
    (!version.is_empty() && version.len() <= 256).then_some(version)
}

fn bounded_ascii_header<'a>(
    headers: &'a HeaderMap,
    name: &str,
    max_len: usize,
) -> Option<&'a HeaderValue> {
    let value = headers.get(name)?;
    let text = value.to_str().ok()?.trim();
    (!text.is_empty() && text.len() <= max_len && text.is_ascii()).then_some(value)
}

fn sanitize_codex_turn_metadata_header(value: &HeaderValue) -> Option<HeaderValue> {
    let raw = value.to_str().ok()?.trim();
    let mut metadata = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    let object = metadata.as_object_mut()?;
    let mut changed = object.remove("installation_id").is_some();
    changed |= object.remove("x-codex-installation-id").is_some();
    if !changed {
        return Some(value.clone());
    }
    HeaderValue::from_str(&serde_json::to_string(&metadata).ok()?).ok()
}

pub(crate) fn apply_codex_client_version_query(
    url: &mut reqwest::Url,
    identity: &CodexClientIdentity,
) {
    let existing = url
        .query_pairs()
        .filter(|(key, _)| key != "client_version")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    url.set_query(None);
    let mut query = url.query_pairs_mut();
    for (key, value) in existing {
        query.append_pair(&key, &value);
    }
    query.append_pair("client_version", &identity.version);
}

#[cfg(test)]
pub(crate) struct CodexIdentityResolver {
    active: CodexClientIdentity,
}

#[cfg(test)]
impl CodexIdentityResolver {
    fn pinned() -> Self {
        Self {
            active: bundled_codex_identity(),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self::pinned()
    }

    pub(crate) fn active(&self) -> CodexClientIdentity {
        self.active.clone()
    }
}

#[cfg(test)]
static CODEX_IDENTITY_RESOLVER: OnceLock<CodexIdentityResolver> = OnceLock::new();

#[cfg(test)]
pub(crate) fn codex_identity_resolver() -> &'static CodexIdentityResolver {
    CODEX_IDENTITY_RESOLVER.get_or_init(CodexIdentityResolver::pinned)
}

#[cfg(test)]
pub(crate) fn active_codex_identity() -> CodexClientIdentity {
    codex_identity_resolver().active()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_identity_user_agent_matches_official_shape() {
        let identity = bundled_codex_identity();
        assert!(
            identity
                .user_agent
                .starts_with(&format!("{CODEX_ORIGINATOR}/{DESKTOP_CORE_VERSION} ("))
        );
        assert!(identity.user_agent.contains(std::env::consts::ARCH));
        assert!(!identity.user_agent.contains("CONST API"));
        let version = semver::Version::parse(DESKTOP_CORE_VERSION)
            .expect("bundled Codex wire version must be semver");
        assert_eq!(version.to_string(), DESKTOP_CORE_VERSION);
    }

    #[test]
    fn codex_identity_manifest_requires_a_real_model_catalog() {
        assert!(codex_manifest_is_valid(&serde_json::json!({
            "models": [{"slug": "gpt-test", "priority": 1}]
        })));
        assert!(codex_manifest_is_valid(&serde_json::json!({
            "data": [{"id": "gpt-test"}]
        })));
        assert!(!codex_manifest_is_valid(&serde_json::json!({"models": []})));
    }

    #[test]
    fn codex_identity_uses_native_headers_and_catalog_version_query() {
        let identity = bundled_codex_identity();
        let request = apply_codex_control_identity_headers(
            reqwest::Client::new().get("https://example.test"),
            &identity,
        )
        .build()
        .expect("request");
        assert_eq!(request.headers()["originator"], CODEX_ORIGINATOR);
        assert_eq!(request.headers()["user-agent"], identity.user_agent);
        assert!(!request.headers().contains_key("version"));

        let headers = codex_model_identity_headers(&identity);
        assert_eq!(headers["version"], DESKTOP_CORE_VERSION);

        let mut url = reqwest::Url::parse("https://example.test/models?other=1").expect("url");
        apply_codex_client_version_query(&mut url, &identity);
        assert_eq!(
            url.query(),
            Some(format!("other=1&client_version={DESKTOP_CORE_VERSION}").as_str())
        );
    }

    #[test]
    fn codex_auth_identity_omits_model_provider_version() {
        let identity = bundled_codex_identity();
        let headers = codex_control_identity_headers(&identity);
        assert_eq!(headers["originator"], CODEX_ORIGINATOR);
        assert_eq!(headers["user-agent"], identity.user_agent);
        assert!(!headers.contains_key("version"));
    }

    #[test]
    fn codex_request_context_preserves_first_party_identity_and_turn_state() {
        let fallback = bundled_codex_identity();
        let mut partial = HeaderMap::new();
        partial.insert("originator", HeaderValue::from_static("codex_vscode"));
        partial.insert(
            "x-codex-turn-state",
            HeaderValue::from_static("sticky-turn"),
        );
        let headers = codex_model_request_context_headers(Some(&partial), &fallback);
        assert_eq!(headers["originator"], CODEX_ORIGINATOR);
        assert_eq!(headers["version"], DESKTOP_CORE_VERSION);
        assert!(!headers.contains_key("x-codex-turn-state"));

        partial.insert("user-agent", HeaderValue::from_static("codex_vscode/1.2.3"));
        let headers = codex_model_request_context_headers(Some(&partial), &fallback);
        assert_eq!(headers["originator"], "codex_vscode");
        assert_eq!(headers["user-agent"], "codex_vscode/1.2.3");
        assert_eq!(headers["version"], "1.2.3");
        assert_eq!(headers["x-codex-turn-state"], "sticky-turn");

        partial.insert("version", HeaderValue::from_static("9.9.9"));
        let headers = codex_model_request_context_headers(Some(&partial), &fallback);
        assert_eq!(headers["originator"], "codex_vscode");
        assert_eq!(headers["user-agent"], "codex_vscode/1.2.3");
        assert_eq!(headers["version"], "9.9.9");
        assert_eq!(headers["x-codex-turn-state"], "sticky-turn");

        partial.insert("originator", HeaderValue::from_static("custom_gateway"));
        partial.insert("user-agent", HeaderValue::from_static("custom_gateway/1"));
        let headers = codex_model_request_context_headers(Some(&partial), &fallback);
        assert_eq!(headers["originator"], CODEX_ORIGINATOR);
        assert_eq!(headers["user-agent"], fallback.user_agent);
        assert_eq!(headers["version"], DESKTOP_CORE_VERSION);
        assert!(!headers.contains_key("x-codex-turn-state"));
    }

    #[test]
    fn codex_subscription_context_uses_adapter_identity_and_keeps_turn_state() {
        let fallback = bundled_codex_identity();
        let mut inbound = HeaderMap::new();
        inbound.insert("originator", HeaderValue::from_static("codex_vscode"));
        inbound.insert("user-agent", HeaderValue::from_static("codex_vscode/1.2.3"));
        inbound.insert("version", HeaderValue::from_static("1.2.3"));
        inbound.insert(
            "x-codex-turn-state",
            HeaderValue::from_static("sticky-turn"),
        );
        inbound.insert(
            "x-codex-installation-id",
            HeaderValue::from_static("caller-installation"),
        );
        inbound.insert(
            "x-codex-turn-metadata",
            HeaderValue::from_static(
                r#"{"installation_id":"nested-installation","x-codex-installation-id":"prefixed-installation","session_id":"caller-session"}"#,
            ),
        );
        inbound.insert("x-not-codex", HeaderValue::from_static("drop-me"));

        let headers = codex_subscription_model_request_context_headers(Some(&inbound), &fallback);

        assert_eq!(headers["originator"], fallback.originator);
        assert_eq!(headers["user-agent"], fallback.user_agent);
        assert_eq!(headers["version"], fallback.version);
        assert_eq!(headers["x-codex-turn-state"], "sticky-turn");
        assert!(!headers.contains_key("x-codex-installation-id"));
        let turn_metadata: serde_json::Value =
            serde_json::from_str(headers["x-codex-turn-metadata"].to_str().unwrap()).unwrap();
        assert!(turn_metadata.get("installation_id").is_none());
        assert!(turn_metadata.get("x-codex-installation-id").is_none());
        assert_eq!(turn_metadata["session_id"], "caller-session");
        assert!(!headers.contains_key("x-not-codex"));

        // Another client's attestation and the legacy HTTP beta are not forwarded.
        inbound.insert(
            "x-oai-attestation",
            HeaderValue::from_static("vscode-attestation"),
        );
        inbound.insert(
            "openai-beta",
            HeaderValue::from_static("responses=experimental, responses_multi_agent=v1"),
        );
        let headers = codex_subscription_model_request_context_headers(Some(&inbound), &fallback);
        assert!(!headers.contains_key("x-oai-attestation"));
        assert_eq!(headers["openai-beta"], "responses_multi_agent=v1");
        inbound.insert(
            "openai-beta",
            HeaderValue::from_static("responses=experimental"),
        );
        inbound.insert("originator", HeaderValue::from_static(CODEX_ORIGINATOR));
        inbound.insert("user-agent", HeaderValue::from_static("codex-tui/0.163.0"));
        let headers = codex_subscription_model_request_context_headers(Some(&inbound), &fallback);
        assert!(!headers.contains_key("x-oai-attestation"));
        assert!(!headers.contains_key("openai-beta"));
    }

    #[test]
    fn routing_hint_is_rebuilt_from_effective_model_and_tier() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-codex-routing-hint",
            HeaderValue::from_static("model=caller-model;tier=stale"),
        );
        set_codex_routing_hint(&mut headers, Some("gpt-effective"), Some("priority"));
        assert_eq!(
            headers["x-codex-routing-hint"],
            "model=gpt-effective;tier=priority"
        );

        set_codex_routing_hint(&mut headers, Some("gpt-effective"), None);
        assert_eq!(headers["x-codex-routing-hint"], "model=gpt-effective");

        set_codex_routing_hint(&mut headers, None, Some("priority"));
        assert!(!headers.contains_key("x-codex-routing-hint"));
    }

    #[test]
    fn codex_adapter_user_agent_matches_the_verified_desktop_shape() {
        let identity = bundled_codex_identity();
        let expected_prefix = format!("{CODEX_ORIGINATOR}/{DESKTOP_CORE_VERSION} (");
        let expected_suffix = format!(" ({CODEX_ORIGINATOR}; {DESKTOP_APP_VERSION})");
        assert!(
            identity.user_agent.starts_with(&expected_prefix),
            "{}",
            identity.user_agent
        );
        assert!(
            identity.user_agent.ends_with(&expected_suffix),
            "{}",
            identity.user_agent
        );
    }

    #[test]
    fn native_codex_detection_requires_first_party_originators() {
        let mut headers = HeaderMap::new();
        headers.insert("originator", HeaderValue::from_static("codex_vscode"));
        headers.insert(
            "user-agent",
            HeaderValue::from_static("codex_cli_rs/0.149.1 (Windows 11; x86_64) vscode"),
        );
        assert!(codex_request_is_native(Some(&headers)));

        headers.insert("originator", HeaderValue::from_static("codex_exec"));
        headers.insert(
            "user-agent",
            HeaderValue::from_static("codex_exec/0.149.1 (Windows 11; x86_64) unknown"),
        );
        assert!(codex_request_is_native(Some(&headers)));

        headers.insert("originator", HeaderValue::from_static("custom_gateway"));
        assert!(!codex_request_is_native(Some(&headers)));
    }

    #[test]
    fn native_codex_versions_and_beta_are_not_bounded_by_the_bundled_version() {
        for version in ["0.120.0", "0.154.0", "0.200.0", "1.0.0-alpha.1"] {
            let mut inbound = HeaderMap::new();
            inbound.insert("originator", "codex_cli_rs".parse().unwrap());
            let user_agent = format!("codex_cli_rs/{version} (Windows; x86_64)");
            inbound.insert("user-agent", user_agent.parse().unwrap());
            inbound.insert("version", version.parse().unwrap());
            inbound.insert(
                "openai-beta",
                "responses=future-v9,tools=future-v2".parse().unwrap(),
            );
            let headers =
                codex_model_request_context_headers(Some(&inbound), &bundled_codex_identity());
            assert_eq!(headers["version"], version);
            assert_eq!(headers["user-agent"], user_agent);
            assert_eq!(headers["openai-beta"], inbound["openai-beta"]);
        }
    }

    #[test]
    fn bundled_upgrade_does_not_replace_old_or_future_native_identities() {
        for version in ["0.150.1", "0.153.4", "0.999.0"] {
            let mut inbound = HeaderMap::new();
            inbound.insert("originator", HeaderValue::from_static("codex_cli_rs"));
            inbound.insert(
                "user-agent",
                HeaderValue::from_str(&format!("codex_cli_rs/{version} (Mac OS; arm64) terminal"))
                    .unwrap(),
            );
            let headers =
                codex_model_request_context_headers(Some(&inbound), &bundled_codex_identity());
            assert_eq!(headers["version"], version);
            assert_eq!(headers["user-agent"], inbound["user-agent"]);
        }
    }
}
