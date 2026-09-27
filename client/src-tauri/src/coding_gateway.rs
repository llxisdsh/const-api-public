//! Maintained coding gateways. Provider wire contracts stay at the supplier boundary;
//! the platform receives ordinary model-scoped capability evidence.
use crate::{
    model::{ChannelCapabilityProfile, ChannelConfig, SupplierCapabilityEvidence},
    model_catalog::ModelObservation,
    protocol::kind::ProtocolKind,
    source_driver::SourceDriverId,
    surface::ApiOperation,
};
use anyhow::{Result, ensure};
use reqwest::header::{HeaderMap, HeaderValue};

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
    )
}

pub(crate) fn is_opencode(driver: SourceDriverId) -> bool {
    matches!(
        driver,
        SourceDriverId::OpencodeGo | SourceDriverId::OpencodeZen
    )
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

/// Zen speaks Gemini under its own /zen/v1 root, not Google's /v1beta root.
/// Other Gemini providers retain their existing URL policy.
pub(crate) fn gemini_url(driver: SourceDriverId, base: &str, path: &str) -> Result<String> {
    if driver == SourceDriverId::OpencodeZen {
        let relative = path.strip_prefix("/v1beta/").ok_or_else(|| {
            anyhow::anyhow!("Zen Gemini requires a /v1beta/ protocol-relative path")
        })?;
        Ok(crate::supplier::join_upstream_url(
            base,
            &format!("/v1/{relative}"),
        ))
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
    let binding = crate::source_driver_surface_bindings(SourceDriverId::OpencodeZen, "")
        .into_iter()
        .find(|binding| binding.surface == crate::surface::ApiSurface::Gemini)
        .expect("Zen manifest declares Gemini");
    channel.surface_bindings.push(binding);
    if crate::channel_surface::validate_channel_source_policy(channel).is_err() {
        channel.surface_bindings.pop();
    }
}

/// The published Go/Zen catalogs contain IDs only. Their endpoint table defines
/// family-specific dialects, with Chat as the default (see docs/coding-gateways.md).
pub(crate) fn model_protocol(driver: SourceDriverId, model: &str) -> Option<ProtocolKind> {
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
        } else if driver == SourceDriverId::OpencodeZen && model.starts_with("gemini-") {
            ProtocolKind::GeminiNative
        } else {
            ProtocolKind::OpenAiChat
        },
    )
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
    if !is_opencode(driver) && driver != SourceDriverId::CommandCode {
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
            ("gemini-3.8-flash", OpenAiChat, GeminiNative),
        ] {
            assert_eq!(model_protocol(SourceDriverId::OpencodeGo, model), Some(go));
            assert_eq!(
                model_protocol(SourceDriverId::OpencodeZen, model),
                Some(zen)
            );
        }
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
            5,
            "one shared catalog GET per refresh"
        );
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
