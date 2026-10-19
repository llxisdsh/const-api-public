//! Maintained coding gateways. Provider wire contracts stay at the supplier boundary;
//! the platform receives ordinary model-scoped capability evidence.
use crate::{
    model::{ChannelCapabilityProfile, ChannelConfig, SupplierCapabilityEvidence},
    model_catalog::ModelObservation,
    protocol::kind::ProtocolKind,
    source_driver::SourceDriverId,
    surface::{ApiOperation, ApiSurface},
};
use anyhow::{Result, ensure};
use reqwest::header::{HeaderMap, HeaderValue};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

struct CachedCatalog {
    models: Vec<ModelObservation>,
    loaded_at: Instant,
    retry_after: Instant,
}
type CatalogSlot = Arc<tokio::sync::Mutex<Option<CachedCatalog>>>;
type CatalogKey = (SourceDriverId, ApiSurface, String, String);
static CATALOGS: OnceLock<Mutex<HashMap<CatalogKey, CatalogSlot>>> = OnceLock::new();
const API_MODEL_CATALOG_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);
const CATALOG_STALE_RETRY_INTERVAL: Duration = Duration::from_secs(60);

/// Share free metadata between health ticks. Explicit refreshes remain fresh;
/// account, API surface, and endpoint changes never reuse another catalog.
pub(crate) async fn cached_catalog<F>(
    driver: SourceDriverId,
    surface: ApiSurface,
    credential: &str,
    base: &str,
    fresh: bool,
    fetch: F,
) -> Result<Vec<ModelObservation>>
where
    F: std::future::Future<Output = Result<Vec<ModelObservation>>>,
{
    let ttl = match driver {
        SourceDriverId::PoeApi => Duration::from_secs(60 * 60),
        SourceDriverId::CopilotSubscription | SourceDriverId::AbacusRoutellm => {
            Duration::from_secs(5 * 60)
        }
        // API model directories are metadata, not a request capability probe.
        // Periodic snapshots use this short cache; explicit refreshes bypass it.
        // OpenRouter is included because its account catalog may be paginated.
        SourceDriverId::Openrouter
        | SourceDriverId::OpenAiApi
        | SourceDriverId::AnthropicApi
        | SourceDriverId::GeminiApi
        | SourceDriverId::XaiApi
        | SourceDriverId::MistralApi
        | SourceDriverId::DeepseekApi
        | SourceDriverId::DashscopeApi
        | SourceDriverId::MoonshotApi
        | SourceDriverId::ZhipuApi
        | SourceDriverId::MinimaxApi
        | SourceDriverId::StepfunApi
        | SourceDriverId::AzureOpenAi
        | SourceDriverId::BedrockMantle
        | SourceDriverId::GroqApi
        | SourceDriverId::TogetherApi
        | SourceDriverId::FireworksApi
        | SourceDriverId::PerplexityApi
        | SourceDriverId::HuggingfaceApi
        | SourceDriverId::NvidiaApi
        | SourceDriverId::SiliconflowApi
        | SourceDriverId::VolcengineArkApi
        | SourceDriverId::BaiduQianfanApi
        | SourceDriverId::TencentHunyuanApi
        | SourceDriverId::CustomEndpoint => API_MODEL_CATALOG_REFRESH_INTERVAL,
        _ => return fetch.await,
    };
    let started = Instant::now();
    let key = (
        driver,
        surface,
        hex::encode(Sha256::digest(credential.trim().as_bytes())),
        base.to_string(),
    );
    let slot = CATALOGS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| anyhow::anyhow!("gateway catalog cache poisoned"))?
        .entry(key)
        .or_default()
        .clone();
    let mut cached = slot.lock().await;
    if let Some(entry) = cached.as_ref() {
        if entry.loaded_at >= started || (!fresh && started < entry.retry_after) {
            return Ok(entry.models.clone());
        }
    }
    match fetch.await {
        Ok(models) => {
            *cached = Some(CachedCatalog {
                models: models.clone(),
                loaded_at: Instant::now(),
                retry_after: Instant::now() + ttl,
            });
            Ok(models)
        }
        Err(error)
            if !fresh
                && crate::detection::subscription_catalog_error_allows_stale(&error)
                && cached.is_some() =>
        {
            let entry = cached
                .as_mut()
                .expect("cached catalog exists after the guard");
            entry.retry_after = Instant::now() + CATALOG_STALE_RETRY_INTERVAL;
            let detail = crate::upstream_failure::safe_excerpt(&format!("{error:#}"));
            log::warn!(
                "[const-api][catalog] preserving cached model directory after transient refresh error: {detail}"
            );
            Ok(entry.models.clone())
        }
        Err(error) => {
            // Surface the refresh failure, but keep the last complete snapshot
            // available for a later periodic retry. Mark it due immediately so
            // a health tick cannot silently reuse it without attempting recovery.
            if let Some(entry) = cached.as_mut() {
                entry.retry_after = Instant::now();
            }
            Err(error)
        }
    }
}

pub(crate) fn catalog_url(driver: SourceDriverId, base: &str) -> Result<reqwest::Url> {
    let mut base = reqwest::Url::parse(base)?;
    match driver {
        SourceDriverId::GlmCodingPlan => base.set_path("/api/coding/paas/v4"),
        SourceDriverId::MinimaxTokenPlan => base.set_path("/v1"),
        SourceDriverId::OllamaCloud => {
            base.set_path("/api/tags");
            return Ok(base);
        }
        _ => {}
    }
    // Cline's /models is ID-only; its own client catalog also exposes context,
    // modalities and supported parameters using the same OpenAI list shape.
    let path = if driver == SourceDriverId::ClineApi {
        "/v1/ai/cline/models"
    } else {
        "/v1/models"
    };
    Ok(reqwest::Url::parse(&crate::supplier::join_upstream_url(
        base.as_str(),
        path,
    ))?)
}

pub(crate) fn is_gateway(driver: SourceDriverId) -> bool {
    matches!(
        driver,
        SourceDriverId::OpencodeGo
            | SourceDriverId::OpencodeZen
            | SourceDriverId::KiloGateway
            | SourceDriverId::ClineApi
            | SourceDriverId::CommandCode
            | SourceDriverId::KimiCode
            | SourceDriverId::GlmCodingPlan
            | SourceDriverId::MinimaxTokenPlan
            | SourceDriverId::OllamaCloud
            | SourceDriverId::MuseCode
            | SourceDriverId::CopilotSubscription
            | SourceDriverId::AbacusRoutellm
            | SourceDriverId::PoeApi
    )
}

pub(crate) fn is_opencode(driver: SourceDriverId) -> bool {
    matches!(
        driver,
        SourceDriverId::OpencodeGo | SourceDriverId::OpencodeZen
    )
}

/// Zen's Jev/System One entries are decision endpoints, not text-generation
/// models. They must not enter the normal conversational model pool.
pub(crate) fn is_non_conversational_model(driver: SourceDriverId, model: &str) -> bool {
    driver == SourceDriverId::OpencodeZen
        && crate::config::without_context_hint(model)
            .to_ascii_lowercase()
            .starts_with("jev-")
}

pub(crate) fn default_user_agent(driver: SourceDriverId) -> Option<&'static str> {
    matches!(
        driver,
        SourceDriverId::OpencodeGo
            | SourceDriverId::OpencodeZen
            | SourceDriverId::CommandCode
            | SourceDriverId::KimiCode
            | SourceDriverId::GlmCodingPlan
            | SourceDriverId::MinimaxTokenPlan
            | SourceDriverId::OllamaCloud
            | SourceDriverId::MuseCode
    )
    .then_some(concat!("const-api/", env!("CARGO_PKG_VERSION")))
}

/// Most gateways publish one OpenAI-shaped directory for every dialect.
pub(crate) fn shares_catalog(driver: SourceDriverId) -> bool {
    is_gateway(driver)
}

pub(crate) fn catalog_supports_protocol(model: &ModelObservation, protocol: &str) -> Option<bool> {
    let path = match protocol {
        "openai_chat" => "/chat/completions",
        "openai_responses" => "/responses",
        "anthropic_messages" => "/messages",
        _ => return None,
    };
    model.supported_endpoints.as_ref().map(|endpoints| {
        endpoints
            .iter()
            .any(|endpoint| endpoint == path || endpoint.strip_prefix("/v1") == Some(path))
    })
}

pub(crate) fn catalog_protocol_support(
    profiles: &[ChannelCapabilityProfile],
    model: &str,
    protocol: ProtocolKind,
) -> Option<bool> {
    let mut supported = None;
    for profile in profiles.iter().filter(|profile| {
        profile.catalog_metadata
            && profile.protocol == protocol.as_str()
            && crate::config::model_name_matches(&profile.model_pattern, model)
    }) {
        match profile.catalog_protocol_supported {
            Some(false) => return Some(false),
            Some(true) => supported = Some(true),
            None => {}
        }
    }
    supported
}

pub(crate) fn model_supports_protocol(
    driver: SourceDriverId,
    model: &str,
    protocol: ProtocolKind,
) -> bool {
    if is_non_conversational_model(driver, model) {
        return false;
    }
    if matches!(
        driver,
        SourceDriverId::CopilotSubscription | SourceDriverId::PoeApi
    ) {
        // Older catalogs predate endpoint declarations and exposed Chat only.
        return protocol == ProtocolKind::OpenAiChat;
    }
    if driver == SourceDriverId::AbacusRoutellm {
        let model = model
            .rsplit('/')
            .next()
            .unwrap_or(model)
            .to_ascii_lowercase();
        return match protocol {
            ProtocolKind::OpenAiChat => true,
            ProtocolKind::AnthropicMessages => model.starts_with("claude-"),
            ProtocolKind::OpenAiResponses => {
                model.starts_with("gpt-")
                    || model.starts_with("grok-")
                    || model.starts_with("o1")
                    || model.starts_with("o3")
                    || model.starts_with("o4")
            }
            _ => false,
        };
    }
    if driver == SourceDriverId::CommandCode {
        let model = model
            .rsplit('/')
            .next()
            .unwrap_or(model)
            .to_ascii_lowercase();
        return if model.starts_with("claude-") {
            protocol == ProtocolKind::AnthropicMessages
        } else {
            matches!(
                protocol,
                ProtocolKind::OpenAiChat | ProtocolKind::OpenAiResponses
            )
        };
    }
    model_protocol(driver, model).is_none_or(|native| native == protocol)
}

/// Zen keeps a legacy Gemini-compatible /zen/v1 root. Go's current native
/// endpoint uses the shared /inference/go root with an additional /google
/// family segment; other Gemini providers retain their existing URL policy.
pub(crate) fn gemini_url(driver: SourceDriverId, base: &str, path: &str) -> Result<String> {
    if driver == SourceDriverId::OpencodeZen {
        let relative = path.strip_prefix("/v1beta/").ok_or_else(|| {
            anyhow::anyhow!("Zen Gemini requires a /v1beta/ protocol-relative path")
        })?;
        Ok(crate::supplier::join_upstream_url(
            base,
            &format!("/v1/{relative}"),
        ))
    } else if driver == SourceDriverId::OpencodeGo {
        let base = base.trim_end_matches('/');
        let family_base = if base.ends_with("/google") {
            base.to_string()
        } else {
            format!("{base}/google")
        };
        Ok(format!("{family_base}{path}"))
    } else {
        crate::detection::join_gemini_native_url(base, path)
    }
}

/// Upgrade only the former two-surface official Zen preset. Validate the entire
/// candidate before retaining the new declaration; never repair altered URLs.
/// Called at config boundaries, not on internal single-protocol probe projections.
pub(crate) fn upgrade_legacy_zen_surfaces(channel: &mut ChannelConfig) {
    if channel.source_driver() != SourceDriverId::OpencodeZen
        || channel.surface_bindings.len() != 2
        || channel
            .surface_bindings
            .iter()
            .any(|binding| binding.surface == crate::surface::ApiSurface::Gemini)
    {
        return;
    }
    let legacy_base = "https://opencode.ai/zen/v1";
    let official = channel.surface_bindings.iter().all(|binding| {
        let (endpoint_profile, expected_protocols): (&str, &[&str]) = match binding.surface {
            crate::surface::ApiSurface::OpenAi => {
                ("openai_compatible", &["openai_chat", "openai_responses"])
            }
            crate::surface::ApiSurface::Anthropic => {
                ("anthropic_compatible", &["anthropic_messages"])
            }
            crate::surface::ApiSurface::Gemini => return false,
        };
        binding.base_url.trim_end_matches('/') == legacy_base
            && binding.endpoint_profile == endpoint_profile
            && binding.operation_overrides.is_empty()
            && binding.protocols.len() == expected_protocols.len()
            && binding
                .protocols
                .iter()
                .enumerate()
                .all(|(index, protocol)| {
                    protocol.protocol == expected_protocols[index]
                        && protocol.preferred == (index == 0)
                })
            && (binding.auth_scheme == "bearer"
                || (binding.surface == crate::surface::ApiSurface::Anthropic
                    && binding.auth_scheme == "x_api_key"))
    });
    if !official {
        return;
    }
    if let Some(binding) = channel
        .surface_bindings
        .iter_mut()
        .find(|binding| binding.surface == crate::surface::ApiSurface::Anthropic)
    {
        binding.auth_scheme = "x_api_key".to_string();
    }
    let binding = crate::source_driver_surface_bindings(SourceDriverId::OpencodeZen, "")
        .into_iter()
        .find(|binding| binding.surface == crate::surface::ApiSurface::Gemini)
        .expect("Zen manifest declares Gemini");
    channel.surface_bindings.push(binding);
    if crate::channel_surface::validate_channel_source_policy(channel).is_err() {
        channel.surface_bindings.pop();
    }
}

/// Upgrade an untouched official Go preset from its former two-surface
/// declaration. Altered endpoints remain invalid and are never silently
/// repaired.
pub(crate) fn upgrade_legacy_go_surfaces(channel: &mut ChannelConfig) {
    if channel.source_driver() != SourceDriverId::OpencodeGo
        || channel.surface_bindings.len() != 2
        || channel
            .surface_bindings
            .iter()
            .any(|binding| binding.surface == crate::surface::ApiSurface::Gemini)
    {
        return;
    }
    let legacy_base = "https://opencode.ai/zen/go/v1";
    let official = channel.surface_bindings.iter().all(|binding| {
        let (endpoint_profile, expected_protocols): (&str, &[&str]) = match binding.surface {
            crate::surface::ApiSurface::OpenAi => {
                ("openai_compatible", &["openai_chat", "openai_responses"])
            }
            crate::surface::ApiSurface::Anthropic => {
                ("anthropic_compatible", &["anthropic_messages"])
            }
            crate::surface::ApiSurface::Gemini => return false,
        };
        binding.base_url.trim_end_matches('/') == legacy_base
            && binding.endpoint_profile == endpoint_profile
            && binding.operation_overrides.is_empty()
            && binding.protocols.len() == expected_protocols.len()
            && binding
                .protocols
                .iter()
                .enumerate()
                .all(|(index, protocol)| {
                    protocol.protocol == expected_protocols[index]
                        && protocol.preferred == (index == 0)
                })
            && (binding.auth_scheme == "bearer"
                || (binding.surface == crate::surface::ApiSurface::Anthropic
                    && binding.auth_scheme == "x_api_key"))
    });
    if !official {
        return;
    }
    if let Some(binding) = channel
        .surface_bindings
        .iter_mut()
        .find(|binding| binding.surface == crate::surface::ApiSurface::Anthropic)
    {
        binding.auth_scheme = "x_api_key".to_string();
    }
    let binding = crate::source_driver_surface_bindings(SourceDriverId::OpencodeGo, "")
        .into_iter()
        .find(|binding| binding.surface == crate::surface::ApiSurface::Gemini)
        .expect("Go manifest declares Gemini");
    channel.surface_bindings.push(binding);
}

/// The published Go/Zen catalogs contain IDs only. Their endpoint table defines
/// family-specific dialects, with Chat as the default (see docs/coding-gateways.md).
pub(crate) fn model_protocol(driver: SourceDriverId, model: &str) -> Option<ProtocolKind> {
    if is_non_conversational_model(driver, model) {
        return None;
    }
    if driver == SourceDriverId::AbacusRoutellm {
        return Some(
            if model_supports_protocol(driver, model, ProtocolKind::AnthropicMessages) {
                ProtocolKind::AnthropicMessages
            } else if model_supports_protocol(driver, model, ProtocolKind::OpenAiResponses) {
                ProtocolKind::OpenAiResponses
            } else {
                ProtocolKind::OpenAiChat
            },
        );
    }
    if driver == SourceDriverId::CommandCode {
        return Some(
            if model
                .rsplit('/')
                .next()
                .unwrap_or(model)
                .to_ascii_lowercase()
                .starts_with("claude-")
            {
                ProtocolKind::AnthropicMessages
            } else {
                ProtocolKind::OpenAiChat
            },
        );
    }
    if !is_opencode(driver) {
        return None;
    }
    let model = crate::config::without_context_hint(model).to_ascii_lowercase();
    Some(
        if ["gpt-", "grok-", "muse-spark"]
            .iter()
            .any(|prefix| model.starts_with(prefix))
        {
            ProtocolKind::OpenAiResponses
        } else if model.starts_with("qwen")
            || model.starts_with("claude-")
            || (driver == SourceDriverId::OpencodeGo && model.starts_with("minimax-"))
        {
            ProtocolKind::AnthropicMessages
        } else if matches!(
            driver,
            SourceDriverId::OpencodeGo | SourceDriverId::OpencodeZen
        ) && model.starts_with("gemini-")
        {
            ProtocolKind::GeminiNative
        } else {
            ProtocolKind::OpenAiChat
        },
    )
}

pub(crate) fn channel_model_protocol(channel: &ChannelConfig, model: &str) -> Option<ProtocolKind> {
    if is_non_conversational_model(channel.source_driver(), model) {
        return None;
    }
    if matches!(
        channel.source_driver(),
        SourceDriverId::CopilotSubscription
            | SourceDriverId::PoeApi
            | SourceDriverId::AbacusRoutellm
    ) {
        // A model name is not evidence of its native wire dialect.
        return [
            ProtocolKind::AnthropicMessages,
            ProtocolKind::OpenAiResponses,
            ProtocolKind::OpenAiChat,
        ]
        .into_iter()
        .find(|protocol| {
            catalog_protocol_support(&channel.capability_profiles, model, *protocol) == Some(true)
        })
        .or_else(|| model_protocol(channel.source_driver(), model))
        .or(Some(ProtocolKind::OpenAiChat));
    }
    model_protocol(channel.source_driver(), model)
}

pub(crate) fn probe_model(
    channel: &ChannelConfig,
    catalog: &[ModelObservation],
    selected: String,
) -> Result<String> {
    if !is_gateway(channel.source_driver()) {
        return Ok(selected);
    }
    let models = catalog
        .iter()
        .filter(|model| {
            ProtocolKind::parse(&channel.api_format).is_ok_and(|protocol| {
                catalog_supports_protocol(model, protocol.as_str()).unwrap_or_else(|| {
                    model_supports_protocol(channel.source_driver(), &model.id, protocol)
                })
            }) && (model.output_modalities.is_empty()
                || model.output_modalities.iter().any(|kind| kind == "text"))
        })
        .map(|model| model.id.clone())
        .collect::<Vec<_>>();
    ensure!(
        !models.is_empty(),
        "gateway catalog has no model for {} verification",
        channel.api_format
    );
    Ok(
        if models
            .iter()
            .any(|model| model.eq_ignore_ascii_case(&selected))
        {
            selected
        } else {
            crate::detection::pick_primary_model(&models, channel)
        },
    )
}

/// Do not advertise every dialect for every OpenCode model. Negative mode
/// evidence also lets the server select a conversion target without provider code.
pub(crate) fn scope_native_evidence(
    driver: SourceDriverId,
    models: &[String],
    profiles: &[ChannelCapabilityProfile],
    evidence: &mut Vec<SupplierCapabilityEvidence>,
) {
    if !is_opencode(driver)
        && !matches!(
            driver,
            SourceDriverId::CommandCode
                | SourceDriverId::CopilotSubscription
                | SourceDriverId::PoeApi
                | SourceDriverId::AbacusRoutellm
        )
    {
        return;
    }
    let protocols = crate::source_driver::source_driver(driver)
        .expect("gateway driver is registered")
        .protocols();
    // Resolve once per model/dialect, not once for every capability evidence row.
    let mut support = std::collections::HashMap::new();
    for model in models {
        for protocol in &protocols {
            let supported = ProtocolKind::parse(protocol).is_ok_and(|protocol| {
                catalog_protocol_support(profiles, model, protocol)
                    .unwrap_or_else(|| model_supports_protocol(driver, model, protocol))
            });
            support.insert((model.as_str(), *protocol), supported);
        }
    }
    let mut scoped = Vec::new();
    for item in std::mem::take(evidence) {
        for model in models {
            if support.get(&(model.as_str(), item.protocol.as_str())) != Some(&true) {
                continue;
            }
            let pattern = item.model_pattern.as_deref().unwrap_or("");
            if !pattern.is_empty()
                && pattern != "*"
                && !crate::config::model_name_matches(pattern, model)
            {
                continue;
            }
            let mut item = item.clone();
            item.model_pattern = Some(model.clone());
            scoped.push(item);
        }
    }
    for model in models {
        for protocol in &protocols {
            if support[&(model.as_str(), *protocol)] {
                continue;
            }
            for feature in ["non_stream_json", "stream"] {
                scoped.push(SupplierCapabilityEvidence {
                    feature: feature.into(),
                    state: "unsupported".into(),
                    protocol: (*protocol).into(),
                    source: "driver_contract".into(),
                    capability_layer: "model_wire".into(),
                    model_pattern: Some(model.clone()),
                    observed_at_unix: crate::now_unix(),
                    token_limit: None,
                    target_protocol: None,
                    conversion_level: None,
                    release_status: Some("supported".into()),
                });
            }
        }
    }
    *evidence = scoped;
}

pub(crate) fn prepare_headers(
    channel: &ChannelConfig,
    operation: ApiOperation,
    target_protocol: ProtocolKind,
    headers: &mut HeaderMap,
    cache_identity: Option<&str>,
) {
    let header = match channel.source_driver() {
        SourceDriverId::OpencodeGo | SourceDriverId::OpencodeZen => "x-opencode-session",
        SourceDriverId::KiloGateway => "x-kilocode-taskid",
        _ => return,
    };
    if is_opencode(channel.source_driver()) && target_protocol == ProtocolKind::AnthropicMessages {
        headers
            .entry("anthropic-version")
            .or_insert(HeaderValue::from_static("2023-06-01"));
    }
    if !matches!(
        operation,
        ApiOperation::ChatCompletions
            | ApiOperation::Responses
            | ApiOperation::Messages
            | ApiOperation::GenerateContent
            | ApiOperation::StreamGenerateContent
    ) || headers.contains_key(header)
    {
        return;
    }
    if let Some(value) = cache_identity
        .filter(|value| !value.is_empty() && value.len() <= 256)
        .and_then(|value| HeaderValue::from_str(value).ok())
    {
        headers.insert(header, value);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{channel_from_supplier, default_supplier_config};

    pub(crate) fn channel(driver: SourceDriverId, base: &str) -> ChannelConfig {
        let mut channel =
            channel_from_supplier(format!("test-{driver:?}"), &default_supplier_config());
        channel.v2 = crate::channel_v2_contract_for_source(driver);
        channel.surface_bindings = crate::source_driver_surface_bindings(driver, "");
        for binding in &mut channel.surface_bindings {
            binding.base_url = base.into();
        }
        channel.upstream_base_url = base.into();
        channel.kind = "custom_endpoint".into();
        channel.api_format = "openai_chat".into();
        let key = if driver == SourceDriverId::MinimaxTokenPlan {
            "sk-cp-test-key"
        } else {
            "test-key"
        };
        channel.upstream_api_key = key.into();
        channel.v2.credential_ref = key.into();
        channel.public_model.clear();
        channel.upstream_model.clear();
        channel.models.clear();
        crate::project_channel_legacy_fields(channel)
    }

    #[test]
    fn published_dialects_and_plan_difference_are_preserved() {
        use ProtocolKind::*;
        assert!(model_supports_protocol(
            SourceDriverId::AbacusRoutellm,
            "claude-sonnet-5-5-thinking",
            AnthropicMessages
        ));
        assert!(model_supports_protocol(
            SourceDriverId::AbacusRoutellm,
            "claude-sonnet-5-5-thinking",
            OpenAiChat
        ));
        assert!(!model_supports_protocol(
            SourceDriverId::AbacusRoutellm,
            "claude-sonnet-5-5-thinking",
            OpenAiResponses
        ));
        assert!(!model_supports_protocol(
            SourceDriverId::AbacusRoutellm,
            "route-llm",
            AnthropicMessages
        ));
        assert_eq!(
            model_protocol(SourceDriverId::AbacusRoutellm, "gpt-6-luna"),
            Some(OpenAiResponses)
        );
        assert!(model_supports_protocol(
            SourceDriverId::CommandCode,
            "openai/gpt-5.6-luna",
            OpenAiResponses
        ));
        assert!(model_supports_protocol(
            SourceDriverId::CommandCode,
            "openai/gpt-5.6-luna",
            OpenAiChat
        ));
        assert!(!model_supports_protocol(
            SourceDriverId::CommandCode,
            "anthropic/claude-sonnet-5",
            OpenAiResponses
        ));
        assert!(model_supports_protocol(
            SourceDriverId::CommandCode,
            "anthropic/claude-sonnet-5",
            AnthropicMessages
        ));
        for (model, go, zen) in [
            ("gpt-5.6-luna", OpenAiResponses, OpenAiResponses),
            ("grok-4.6", OpenAiResponses, OpenAiResponses),
            (
                "muse-spark-1.3-contributor",
                OpenAiResponses,
                OpenAiResponses,
            ),
            ("qwen3.8-max", AnthropicMessages, AnthropicMessages),
            ("MINIMAX-M3", AnthropicMessages, OpenAiChat),
            ("claude-sonnet-5", AnthropicMessages, AnthropicMessages),
            ("deepseek-v4.1-flash", OpenAiChat, OpenAiChat),
            ("kimi-k3", OpenAiChat, OpenAiChat),
            ("gemini-3.8-flash", GeminiNative, GeminiNative),
        ] {
            assert_eq!(model_protocol(SourceDriverId::OpencodeGo, model), Some(go));
            assert_eq!(
                model_protocol(SourceDriverId::OpencodeZen, model),
                Some(zen)
            );
        }
        assert_eq!(
            model_protocol(SourceDriverId::OpencodeZen, "jev-1.13"),
            None
        );
        assert!(!model_supports_protocol(
            SourceDriverId::OpencodeZen,
            "jev-1.13",
            OpenAiChat
        ));
        assert_eq!(
            model_protocol(SourceDriverId::Openrouter, "gpt-5.6-luna"),
            None
        );
    }

    #[test]
    fn each_protocol_check_chooses_an_appropriate_catalog_model() {
        let mut channel = channel(SourceDriverId::OpencodeZen, "https://opencode.ai/zen/v1");
        let catalog = ["qwen3.8-max", "gpt-5.6-luna", "kimi-k3", "gemini-3.8-flash"].map(|id| {
            ModelObservation {
                id: id.into(),
                ..Default::default()
            }
        });
        for (protocol, expected) in [
            ("openai_chat", "kimi-k3"),
            ("openai_responses", "gpt-5.6-luna"),
            ("anthropic_messages", "qwen3.8-max"),
            ("gemini_native", "gemini-3.8-flash"),
        ] {
            channel.api_format = protocol.into();
            assert_eq!(
                probe_model(&channel, &catalog, "qwen3.8-max".into()).unwrap(),
                expected
            );
        }
        channel.api_format = "openai_responses".into();
        assert!(probe_model(&channel, &catalog[..1], "qwen3.8-max".into()).is_err());
    }

    #[test]
    fn cache_sessions_are_stable_and_caller_headers_win() {
        for (driver, name) in [
            (SourceDriverId::OpencodeGo, "x-opencode-session"),
            (SourceDriverId::OpencodeZen, "x-opencode-session"),
            (SourceDriverId::KiloGateway, "x-kilocode-taskid"),
        ] {
            let channel = channel(driver, "https://example.test/v1");
            for identity in ["user-a-session-1", "user-a-session-2", "user-b-session-1"] {
                let mut headers = HeaderMap::new();
                prepare_headers(
                    &channel,
                    ApiOperation::ListModels,
                    ProtocolKind::OpenAiChat,
                    &mut headers,
                    Some(identity),
                );
                assert!(headers.is_empty());
                for _ in 0..2 {
                    prepare_headers(
                        &channel,
                        ApiOperation::ChatCompletions,
                        ProtocolKind::OpenAiChat,
                        &mut headers,
                        Some(identity),
                    );
                }
                assert_eq!(headers[name], identity);
                headers.insert(name, HeaderValue::from_static("caller-session"));
                prepare_headers(
                    &channel,
                    ApiOperation::ChatCompletions,
                    ProtocolKind::OpenAiChat,
                    &mut headers,
                    Some(identity),
                );
                assert_eq!(headers[name], "caller-session");
            }
        }
    }

    #[test]
    fn gateway_urls_preserve_the_documented_api_roots() {
        for (driver, base, expected) in [
            (
                SourceDriverId::MuseCode,
                "https://api.meta.ai/v1",
                "https://api.meta.ai/v1/models",
            ),
            (
                SourceDriverId::MuseCode,
                "https://api.meta.ai",
                "https://api.meta.ai/v1/models",
            ),
            (
                SourceDriverId::CommandCode,
                "https://api.commandcode.ai/provider/v1",
                "https://api.commandcode.ai/provider/v1/models",
            ),
            (
                SourceDriverId::KimiCode,
                "https://api.kimi.com/coding",
                "https://api.kimi.com/coding/v1/models",
            ),
            (
                SourceDriverId::KimiCode,
                "https://api.kimi.com/coding/v1",
                "https://api.kimi.com/coding/v1/models",
            ),
            (
                SourceDriverId::GlmCodingPlan,
                "https://api.z.ai/api/anthropic",
                "https://api.z.ai/api/coding/paas/v4/models",
            ),
            (
                SourceDriverId::MinimaxTokenPlan,
                "https://api.minimax.io/anthropic",
                "https://api.minimax.io/v1/models",
            ),
            (
                SourceDriverId::OllamaCloud,
                "https://ollama.com/v1",
                "https://ollama.com/api/tags",
            ),
        ] {
            assert_eq!(catalog_url(driver, base).unwrap().as_str(), expected);
        }
        for action in ["generateContent", "streamGenerateContent?alt=sse"] {
            let path = format!("/v1beta/models/gemini-3.8-flash:{action}");
            assert_eq!(
                gemini_url(
                    SourceDriverId::OpencodeZen,
                    "https://opencode.ai/zen/v1",
                    &path
                )
                .unwrap(),
                format!("https://opencode.ai/zen/v1/models/gemini-3.8-flash:{action}")
            );
            assert_eq!(
                gemini_url(
                    SourceDriverId::OpencodeGo,
                    "https://opencode.ai/inference/go",
                    &path
                )
                .unwrap(),
                format!(
                    "https://opencode.ai/inference/go/google/v1beta/models/gemini-3.8-flash:{action}"
                )
            );
            assert_eq!(
                gemini_url(
                    SourceDriverId::GeminiApi,
                    "https://generativelanguage.googleapis.com/v1beta",
                    &path
                )
                .unwrap(),
                format!("https://generativelanguage.googleapis.com{path}")
            );
        }
        assert_eq!(
            catalog_url(
                SourceDriverId::KiloGateway,
                "https://api.kilo.ai/api/gateway"
            )
            .unwrap()
            .as_str(),
            "https://api.kilo.ai/api/gateway/models"
        );
        assert_eq!(
            crate::supplier::join_upstream_url(
                "https://api.kilo.ai/api/gateway",
                "/v1/chat/completions"
            ),
            "https://api.kilo.ai/api/gateway/chat/completions"
        );
        assert_eq!(
            catalog_url(SourceDriverId::ClineApi, "https://api.cline.bot/api/v1")
                .unwrap()
                .as_str(),
            "https://api.cline.bot/api/v1/ai/cline/models"
        );
    }

    #[tokio::test]
    async fn refresh_is_get_only_and_preserves_catalog_metadata_for_platform_supply() {
        use warp::Filter;
        let gets = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = gets.clone();
        let catalog = warp::any().and(warp::method()).and(warp::path::full()).map(move |method: warp::http::Method, path: warp::path::FullPath| {
            assert_eq!(method, warp::http::Method::GET, "refresh must not run inference");
            seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            assert!(matches!(path.as_str(), "/v1/models" | "/v1/ai/cline/models"));
            warp::reply::json(&serde_json::json!({"data":[
                {"id":"gpt-5.6-luna"}, {"id":"qwen3.8-max"}, {"id":"gemini-3.8-flash", "supported_endpoints":["/chat/completions"]},
                {"id":"vendor/kimi-k3", "context_length":262144, "top_provider":{"max_completion_tokens":8192}, "architecture":{"input_modalities":["text","image"],"output_modalities":["text"]},"supported_parameters":["tools","tool_choice"]}
            ]}))
        });
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(warp::serve(catalog).incoming(listener).run());
        for driver in [
            SourceDriverId::OpencodeGo,
            SourceDriverId::OpencodeZen,
            SourceDriverId::KiloGateway,
            SourceDriverId::ClineApi,
            SourceDriverId::CommandCode,
            SourceDriverId::MuseCode,
            SourceDriverId::AbacusRoutellm,
        ] {
            let mut channel = channel(driver, &base);
            let result = crate::detection::refresh_channel(channel.clone())
                .await
                .unwrap();
            assert_eq!(result.models.len(), 4);
            let metadata = result
                .model_capability_evidence
                .iter()
                .find(|profile| profile.catalog_source_model.as_deref() == Some("vendor/kimi-k3"))
                .unwrap();
            assert_eq!(metadata.context_tokens, Some(262144));
            assert_eq!(metadata.output_tokens, Some(8192));
            assert!(metadata.tool_calls && metadata.tool_choice && metadata.image_input);
            assert!(
                !result
                    .surface_results
                    .iter()
                    .any(|surface| surface.protocol_verification.state == "verified")
            );
            crate::supplier::merge_channel_detection_result(&mut channel, &result);
            if driver == SourceDriverId::CommandCode {
                assert_eq!(
                    catalog_protocol_support(
                        &channel.capability_profiles,
                        "gemini-3.8-flash",
                        ProtocolKind::OpenAiResponses
                    ),
                    Some(false)
                );
                assert_eq!(
                    catalog_protocol_support(
                        &channel.capability_profiles,
                        "gemini-3.8-flash",
                        ProtocolKind::OpenAiChat
                    ),
                    Some(true)
                );
                let mut evidence = Vec::new();
                scope_native_evidence(
                    driver,
                    &channel.models,
                    &channel.capability_profiles,
                    &mut evidence,
                );
                assert!(evidence.iter().any(|item| item.model_pattern.as_deref()
                    == Some("gemini-3.8-flash")
                    && item.protocol == "openai_responses"
                    && item.feature == "non_stream_json"
                    && item.state == "unsupported"));
                let catalog = crate::model_catalog::model_observations_from_value(
                    crate::model_catalog::ModelCatalogKind::CommandCode,
                    &serde_json::json!({"data":[{"id":"gemini-3.8-flash","supported_endpoints":["/chat/completions"]}]}),
                    "mock",
                );
                channel.api_format = "openai_responses".into();
                assert!(probe_model(&channel, &catalog, "gemini-3.8-flash".into()).is_err());
            }
            assert!(crate::source_driver::channel_is_platform_shareable(
                &channel
            ));
            assert_eq!(crate::supplier_from_channel(&channel).source_driver, driver);
        }
        assert_eq!(
            gets.load(std::sync::atomic::Ordering::Relaxed),
            7,
            "one shared catalog GET per refresh"
        );
        server.abort();
    }

    #[tokio::test]
    async fn catalog_cache_avoids_health_tick_downloads_but_refreshes_and_accounts_are_isolated() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = AtomicUsize::new(0);
        let key = format!("catalog-test-{}", rand::random::<u64>());
        let fetch = || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![ModelObservation {
                id: "claude-test".into(),
                ..Default::default()
            }])
        };
        for _ in 0..2 {
            assert_eq!(
                cached_catalog(
                    SourceDriverId::PoeApi,
                    ApiSurface::OpenAi,
                    &key,
                    "https://api.poe.com/v1/models",
                    false,
                    fetch()
                )
                .await
                .unwrap()
                .len(),
                1
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        cached_catalog(
            SourceDriverId::PoeApi,
            ApiSurface::OpenAi,
            &key,
            "https://api.poe.com/v1/models",
            true,
            fetch(),
        )
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let failed = cached_catalog(
            SourceDriverId::PoeApi,
            ApiSurface::OpenAi,
            &key,
            "https://api.poe.com/v1/models",
            true,
            async { Err(anyhow::anyhow!("HTTP 401")) },
        )
        .await;
        assert!(failed.is_err());
        cached_catalog(
            SourceDriverId::PoeApi,
            ApiSurface::OpenAi,
            &key,
            "https://api.poe.com/v1/models",
            false,
            fetch(),
        )
        .await
        .unwrap();
        cached_catalog(
            SourceDriverId::PoeApi,
            ApiSurface::OpenAi,
            &format!("{key}-other"),
            "https://api.poe.com/v1/models",
            false,
            fetch(),
        )
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn api_catalog_cache_reuses_periodic_snapshots_but_explicit_refreshes() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = AtomicUsize::new(0);
        let key = format!("api-catalog-test-{}", rand::random::<u64>());
        let fetch = || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![ModelObservation {
                id: "api-model".into(),
                ..Default::default()
            }])
        };

        cached_catalog(
            SourceDriverId::Openrouter,
            ApiSurface::OpenAi,
            &key,
            "https://openrouter.ai/api/v1/models/user",
            false,
            fetch(),
        )
        .await
        .unwrap();
        cached_catalog(
            SourceDriverId::Openrouter,
            ApiSurface::OpenAi,
            &key,
            "https://openrouter.ai/api/v1/models/user",
            false,
            fetch(),
        )
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        cached_catalog(
            SourceDriverId::Openrouter,
            ApiSurface::OpenAi,
            &key,
            "https://openrouter.ai/api/v1/models/user",
            true,
            fetch(),
        )
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn catalog_cache_keeps_same_endpoint_surfaces_separate() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let calls = AtomicUsize::new(0);
        let key = format!("surface-cache-test-{}", rand::random::<u64>());
        let fetch = |id: &'static str| async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![ModelObservation {
                id: id.into(),
                ..Default::default()
            }])
        };

        let openai = cached_catalog(
            SourceDriverId::CustomEndpoint,
            ApiSurface::OpenAi,
            &key,
            "https://example.test/v1",
            false,
            fetch("openai-model"),
        )
        .await
        .unwrap();
        let anthropic = cached_catalog(
            SourceDriverId::CustomEndpoint,
            ApiSurface::Anthropic,
            &key,
            "https://example.test/v1",
            false,
            fetch("anthropic-model"),
        )
        .await
        .unwrap();

        assert_eq!(openai[0].id, "openai-model");
        assert_eq!(anthropic[0].id, "anthropic-model");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn periodic_catalog_refresh_keeps_last_complete_snapshot_after_transient_error() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use warp::Filter;

        let key = format!("stale-catalog-test-{}", rand::random::<u64>());
        let base = "https://api.example.test/v1/models";
        let calls = std::sync::Arc::new(AtomicUsize::new(0));
        let models = vec![ModelObservation {
            id: "complete-model".into(),
            ..Default::default()
        }];
        cached_catalog(
            SourceDriverId::Openrouter,
            ApiSurface::OpenAi,
            &key,
            base,
            false,
            async { Ok(models.clone()) },
        )
        .await
        .unwrap();

        let failed = cached_catalog(
            SourceDriverId::Openrouter,
            ApiSurface::OpenAi,
            &key,
            base,
            true,
            async { Err::<Vec<ModelObservation>, _>(anyhow::anyhow!("explicit refresh failed")) },
        )
        .await;
        assert!(failed.is_err());

        let cache_key = (
            SourceDriverId::Openrouter,
            ApiSurface::OpenAi,
            hex::encode(Sha256::digest(key.trim().as_bytes())),
            base.to_string(),
        );
        let slot = CATALOGS
            .get()
            .unwrap()
            .lock()
            .unwrap()
            .get(&cache_key)
            .unwrap()
            .clone();
        {
            let mut cached = slot.lock().await;
            let entry = cached.as_mut().unwrap();
            entry.loaded_at = Instant::now() - Duration::from_secs(301);
            entry.retry_after = Instant::now() - Duration::from_secs(1);
        }

        let seen = calls.clone();
        let failure = warp::any().map(move || {
            seen.fetch_add(1, Ordering::SeqCst);
            warp::reply::with_status(
                "temporary upstream failure",
                warp::http::StatusCode::SERVICE_UNAVAILABLE,
            )
        });
        let (addr, server) = crate::bind_ephemeral!(failure, ([127, 0, 0, 1], 0));
        let task = tokio::spawn(server);
        let failed_url = format!("http://{addr}/v1/models");
        let retained = cached_catalog(
            SourceDriverId::Openrouter,
            ApiSurface::OpenAi,
            &key,
            base,
            false,
            async move {
                let error = reqwest::get(failed_url)
                    .await
                    .unwrap()
                    .error_for_status()
                    .unwrap_err();
                Err::<Vec<ModelObservation>, _>(anyhow::Error::new(error))
            },
        )
        .await
        .unwrap();
        task.abort();

        assert_eq!(retained, models);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn poe_refresh_filters_bots_and_keeps_per_model_endpoints_for_both_supply_paths() {
        use warp::Filter;
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = seen.clone();
        let route = warp::get().and(warp::path!("v1" / "models")).map(move || {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            warp::reply::json(&serde_json::json!({"data":[
                {"id":"claude-sonnet-5.5", "owned_by":"Anthropic", "supported_endpoints":["/v1/chat/completions"],
                 "supported_features":["tools"], "context_window":{"context_length":1000000},
                 "architecture":{"input_modalities":["text","image"],"output_modalities":["text"]}, "pricing":null},
                {"id":"claude-opus-4.8", "owned_by":"Anthropic", "supported_endpoints":["/v1/messages","/v1/responses","/v1/chat/completions"]},
                {"id":"claude-code", "owned_by":"Poe Tools"}, {"id":"claude-community", "owned_by":"someone"}
            ]}))
        });
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(warp::serve(route).incoming(listener).run());
        let mut channel = channel(SourceDriverId::PoeApi, &base);
        let result = crate::detection::refresh_channel(channel.clone())
            .await
            .unwrap();
        assert_eq!(result.models.len(), 2);
        assert!(
            !result
                .models
                .iter()
                .any(|model| model.contains("community") || model == "claude-code")
        );
        crate::supplier::merge_channel_detection_result(&mut channel, &result);
        assert_eq!(
            channel_model_protocol(&channel, "claude-sonnet-5.5"),
            Some(ProtocolKind::OpenAiChat)
        );
        assert_eq!(
            channel_model_protocol(&channel, "claude-opus-4.8"),
            Some(ProtocolKind::AnthropicMessages)
        );
        let metadata = channel
            .capability_profiles
            .iter()
            .find(|p| {
                p.catalog_metadata
                    && p.protocol == "openai_chat"
                    && p.model_pattern == "claude-sonnet-5.5"
            })
            .unwrap();
        assert_eq!(metadata.context_tokens, Some(1000000));
        assert!(metadata.image_input && metadata.tool_calls);
        let mut evidence = Vec::new();
        scope_native_evidence(
            SourceDriverId::PoeApi,
            &channel.models,
            &channel.capability_profiles,
            &mut evidence,
        );
        assert!(
            evidence
                .iter()
                .any(|e| e.model_pattern.as_deref() == Some("claude-sonnet-5.5")
                    && e.protocol == "anthropic_messages"
                    && e.state == "unsupported")
        );
        assert!(crate::source_driver::channel_is_platform_shareable(
            &channel
        ));
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn abacus_and_poe_preserve_native_claude_cache_tools_and_sse() {
        use crate::channel_executor::{ChannelRequest, execute_channel_request};
        use warp::Filter;
        let body = "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"c1\",\"name\":\"read\",\"input\":{}}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        let route = warp::post()
            .and(warp::path!("v1" / "messages"))
            .and(warp::header::headers_cloned())
            .and(warp::body::json())
            .map(move |headers: HeaderMap, value: serde_json::Value| {
                assert_eq!(headers["authorization"], "Bearer test-key");
                assert_eq!(headers["anthropic-beta"], "future-beta");
                assert_eq!(value["system"][0]["cache_control"]["type"], "ephemeral");
                assert_eq!(value["tools"][0]["name"], "read");
                assert_eq!(value["future_field"], true);
                warp::reply::with_header(body, "content-type", "text/event-stream")
            });
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(warp::serve(route).incoming(listener).run());
        for driver in [SourceDriverId::AbacusRoutellm, SourceDriverId::PoeApi] {
            let mut channel = channel(driver, &base);
            channel.capability_profiles.push(ChannelCapabilityProfile {
                protocol: "anthropic_messages".into(),
                model_pattern: "claude-test".into(),
                catalog_metadata: true,
                catalog_protocol_supported: Some(true),
                ..Default::default()
            });
            let mut headers = HeaderMap::new();
            headers.insert("content-type", HeaderValue::from_static("application/json"));
            headers.insert("anthropic-beta", HeaderValue::from_static("future-beta"));
            for content in [
                serde_json::json!("hello"),
                serde_json::json!([{"type":"tool_result","tool_use_id":"c1","content":"ok"}]),
            ] {
                let request = ChannelRequest { method:reqwest::Method::POST, path:"/anthropic/v1/messages".into(), raw_query:String::new(),
                    has_query:false, headers:headers.clone(), body:serde_json::to_vec(&serde_json::json!({"model":"claude-test", "stream":true,
                        "messages":[{"role":"user","content":content}], "system":[{"type":"text","text":"test","cache_control":{"type":"ephemeral"}}],
                        "tools":[{"name":"read","input_schema":{"type":"object"}}], "future_field":true})).unwrap().into(),
                    stream_requested:Some(true), selected_upstream_model:Some("claude-test".into()), selected_target_protocol:None,
                    cache_identity:None, safety_identifier:None };
                let response = execute_channel_request(&reqwest::Client::new(), &channel, request)
                    .await
                    .unwrap();
                assert!(response.native_passthrough);
                assert_eq!(response.response.text().await.unwrap(), body);
            }
        }
        server.abort();
    }

    #[tokio::test]
    async fn zen_gemini_catalog_and_probes_use_the_documented_wire_contract() {
        use warp::Filter;
        let route = warp::any().and(warp::method()).and(warp::path::full())
            .and(warp::query::<std::collections::HashMap<String, String>>())
            .and(warp::header::headers_cloned())
            .map(|method: warp::http::Method, path: warp::path::FullPath, query: std::collections::HashMap<String, String>, headers: warp::http::HeaderMap| {
                let (body, content_type) = if method == warp::http::Method::GET {
                    assert_eq!(path.as_str(), "/zen/v1/models");
                    assert_eq!(headers["authorization"], "Bearer test-key");
                    (r#"{"data":[{"id":"gpt-5.6-luna"},{"id":"gemini-3.8-flash"}]}"#, "application/json")
                } else {
                    assert_eq!(headers["x-goog-api-key"], "test-key");
                    if path.as_str().ends_with(":streamGenerateContent") {
                        assert_eq!(path.as_str(), "/zen/v1/models/gemini-3.8-flash:streamGenerateContent");
                        assert_eq!(query.get("alt").map(String::as_str), Some("sse"));
                        ("data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"ok\"}]},\"finishReason\":\"STOP\"}]}\n\n", "text/event-stream")
                    } else {
                        assert_eq!(path.as_str(), "/zen/v1/models/gemini-3.8-flash:generateContent");
                        (r#"{"candidates":[{"content":{"parts":[{"text":"ok"}]},"finishReason":"STOP"}]}"#, "application/json")
                    }
                };
                warp::reply::with_header(body, "content-type", content_type)
            });
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let base = format!("http://{}/zen/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(warp::serve(route).incoming(listener).run());
        let mut channel = channel(SourceDriverId::OpencodeZen, &base);
        channel
            .surface_bindings
            .retain(|binding| binding.surface == crate::surface::ApiSurface::Gemini);
        channel.v2.default_target = crate::model::ChannelTarget {
            surface: crate::surface::ApiSurface::Gemini,
            protocol: ProtocolKind::GeminiNative,
        };
        channel = crate::project_channel_legacy_fields(channel);
        let client = reqwest::Client::new();
        let catalog =
            crate::detection::fetch_gemini_native_catalog(&client, &channel, &base, "test-key")
                .await
                .unwrap();
        let model = probe_model(&channel, &catalog, "gpt-5.6-luna".into()).unwrap();
        assert_eq!(model, "gemini-3.8-flash");
        crate::detection::probe_gemini_native(&client, &channel, &model)
            .await
            .unwrap();
        crate::detection::probe_gemini_native_stream(&client, &channel, &model)
            .await
            .unwrap();
        let (url, _, _) =
            crate::supplier::test_request_for_channel(&channel, &model, "ping").unwrap();
        assert_eq!(url, format!("{base}/models/{model}:generateContent"));
        server.abort();
    }
}
