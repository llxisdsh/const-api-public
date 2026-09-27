//! OpenRouter's model naming is a source-wire concern, not a platform identity.
//! Keep existing public catalog IDs and translate only at this source's boundary.
use crate::{model::ChannelConfig, source_driver::SourceDriverId};
use std::borrow::Cow;

mod catalog;
mod diagnostics;
mod request;
pub(crate) use catalog::{
    apply_catalog_capabilities, apply_refreshed_catalog, catalog_cache_key, empty_catalog_health,
    fetch_catalog, protocol_probe_model,
};

/// OpenRouter's skin-native error code may be lossy. Only read error-envelope fields.
pub(crate) fn canonical_error_type(
    envelope: &serde_json::Value,
    error: &serde_json::Value,
) -> String {
    envelope
        .get("error_type")
        .or_else(|| error.get("error_type"))
        .or_else(|| error.pointer("/metadata/error_type"))
        .and_then(serde_json::Value::as_str)
        .filter(|s| s.len() <= 128 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
        .unwrap_or("")
        .to_ascii_lowercase()
}
pub(crate) use diagnostics::{ResponseDiagnostics, observe_response, response_diagnostics};
pub(crate) use request::prepare_headers;

#[cfg(test)]
pub(crate) async fn fetch_models(
    client: &reqwest::Client,
    base_url: &str,
    key: &str,
) -> anyhow::Result<Vec<String>> {
    Ok(crate::model_catalog::model_ids(
        fetch_catalog(client, base_url, key, None).await?,
    ))
}

pub(crate) async fn fetch_channel_catalog(
    client: &reqwest::Client,
    channel: &ChannelConfig,
) -> anyhow::Result<Vec<crate::model_catalog::ModelObservation>> {
    let user_agent = crate::channel_user_agent::value(channel);
    fetch_catalog(
        client,
        &channel.upstream_base_url,
        &channel.upstream_api_key,
        user_agent.as_deref(),
    )
    .await
}

fn is_batch_model(model: &str) -> bool {
    crate::config::without_context_hint(model)
        .to_ascii_lowercase()
        .ends_with(":batch")
}

fn wire_id(model: &crate::tool_model_metadata::EmbeddedToolCatalogModel) -> String {
    if model.id.contains('/') {
        return model.id.clone();
    }
    let vendor = match model.vendor.as_str() {
        "zai" => "z-ai",
        "xai" => "x-ai",
        "moonshot" => "moonshotai",
        "mistral" => "mistralai",
        vendor => vendor,
    };
    let mut name = model.id.to_ascii_lowercase();
    // Claude's official `claude-sonnet-4-6` is `claude-sonnet-4.6`
    // on OpenRouter. Do not remove dates, versions or variant suffixes.
    if vendor == "anthropic" {
        if let Some((prefix, minor)) = name.rsplit_once('-') {
            if !minor.is_empty() && minor.len() <= 2 && minor.bytes().all(|c| c.is_ascii_digit()) {
                if let Some((_, major)) = prefix.rsplit_once('-') {
                    if !major.is_empty()
                        && major.len() <= 2
                        && major.bytes().all(|c| c.is_ascii_digit())
                    {
                        name = format!("{prefix}.{minor}");
                    }
                }
            }
        }
    }
    format!("{vendor}/{name}")
}

fn public_id_with_catalog(
    model: &str,
    catalog: &[crate::tool_model_metadata::EmbeddedToolCatalogModel],
) -> String {
    let model = crate::config::without_context_hint(model);
    // Variants are different products; never advertise them as the ordinary model.
    if model.contains(':') || model.contains('[') || model.starts_with('~') {
        return model.to_string();
    }
    catalog
        .iter()
        .find(|entry| {
            let wire = wire_id(entry);
            model.eq_ignore_ascii_case(&entry.id)
                || model.eq_ignore_ascii_case(&wire)
                // Older Anthropic-compatible catalogs wrapped an OpenRouter ID.
                || model.eq_ignore_ascii_case(&format!("anthropic/{wire}"))
                || (entry.vendor == "anthropic"
                    && model.eq_ignore_ascii_case(&format!("anthropic/{}", entry.id)))
        })
        .map(|entry| entry.id.clone())
        // Unreviewed names keep their full ID, preventing namespace collisions.
        .unwrap_or_else(|| model.to_string())
}

pub(crate) fn public_models(models: Vec<String>) -> Vec<String> {
    let catalog = crate::tool_model_metadata::active_tool_catalog_models();
    crate::config::dedupe_models(
        models
            .into_iter()
            // Batch models use /api/beta/batches, not synchronous inference.
            .filter(|id| !is_batch_model(id))
            .map(|id| public_id_with_catalog(&id, &catalog))
            .collect(),
    )
}

pub(crate) fn upstream_model(channel: &ChannelConfig, model: &str) -> String {
    if channel.source_driver() != SourceDriverId::Openrouter {
        return model.to_string();
    }
    let catalog = crate::tool_model_metadata::active_tool_catalog_models();
    let public = public_id_with_catalog(model, &catalog);
    catalog
        .iter()
        .find(|entry| entry.id.eq_ignore_ascii_case(&public))
        .map(wire_id)
        .unwrap_or_else(|| crate::config::without_context_hint(model).to_string())
}

pub(crate) fn normalize_channel(channel: &mut ChannelConfig) {
    if channel.source_driver() != SourceDriverId::Openrouter {
        return;
    }
    let catalog = crate::tool_model_metadata::active_tool_catalog_models();
    channel.public_model = public_id_with_catalog(&channel.public_model, &catalog);
    channel.upstream_model = public_id_with_catalog(&channel.upstream_model, &catalog);
    normalize_catalog_projection(&mut channel.models, &mut channel.capability_profiles);
}

/// Project public names only after catalog evidence captured the original source ID.
/// Keep that source identity unchanged across refreshes and execution checks.
pub(crate) fn normalize_catalog_projection(
    models: &mut Vec<String>,
    profiles: &mut [crate::model::ChannelCapabilityProfile],
) {
    let catalog = crate::tool_model_metadata::active_tool_catalog_models();
    *models = public_models(std::mem::take(models));
    for profile in profiles {
        profile.model_pattern = public_id_with_catalog(&profile.model_pattern, &catalog);
    }
}

// Detection deliberately probes source IDs. Only its returned catalog/evidence
// is normalized, so generic provider probes need no naming special cases.
pub(crate) fn prepare_probe_channel(channel: &mut ChannelConfig) {
    if channel.source_driver() != SourceDriverId::Openrouter {
        return;
    }
    channel.models = channel
        .models
        .iter()
        .map(|id| upstream_model(channel, id))
        .collect();
    channel.public_model = upstream_model(channel, &channel.public_model);
    channel.upstream_model = upstream_model(channel, &channel.upstream_model);
}

pub(crate) fn request_body<'a>(channel: Option<&ChannelConfig>, body: &'a str) -> Cow<'a, str> {
    let Some(channel) = channel.filter(|c| c.source_driver() == SourceDriverId::Openrouter) else {
        return Cow::Borrowed(body);
    };
    let Some(model) = crate::supplier::top_level_json_string(body.as_bytes(), "model") else {
        return Cow::Borrowed(body);
    };
    let upstream = upstream_model(channel, &model);
    if model == upstream {
        return Cow::Borrowed(body);
    }
    // Preserve every other byte: tool IDs, encrypted state, cache prefix, etc.
    Cow::Owned(crate::supplier::rewrite_supplier_body_model(
        body, &upstream,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "read-only online catalog check using the existing dev OpenRouter channel; no inference"]
    async fn live_dev_openrouter_context_catalog_snapshot() {
        let raw = std::fs::read(crate::home_dir().join(".const-api-dev/client.json")).unwrap();
        let config: crate::model::ClientConfig = serde_json::from_slice(&raw).unwrap();
        let mut channel = config
            .channels
            .into_iter()
            .find(|channel| channel.source_driver() == SourceDriverId::Openrouter)
            .expect("an existing dev OpenRouter channel (refresh does not require activation)");
        let detection = crate::detection::refresh_channel(channel.clone())
            .await
            .expect("read-only information refresh");
        assert!(!detection.models.is_empty());
        assert!(detection.models.iter().all(|id| !model_hint_or_batch(id)));
        crate::supplier::merge_channel_detection_result(&mut channel, &detection);
        // Availability and capacities are live account data, not a fixed list.
        let observed = channel
            .capability_profiles
            .iter()
            .filter(|profile| profile.catalog_metadata && profile.context_tokens.is_some())
            .collect::<Vec<_>>();
        assert!(
            !observed.is_empty(),
            "account catalog should carry numeric capacity metadata"
        );
        for profile in observed.iter().take(8) {
            let (context, output) =
                crate::tool_model_metadata::channel_token_limits(&channel, &profile.model_pattern);
            assert_eq!(context, profile.context_tokens);
            println!(
                "catalog model={} context={} output={} source=provider_metadata",
                profile.model_pattern,
                context.unwrap(),
                output.unwrap_or_default()
            );
        }
        println!(
            "OpenRouter readonly snapshot: {} models; {} numeric profiles; no inference",
            detection.models.len(),
            channel
                .capability_profiles
                .iter()
                .filter(|p| p.catalog_metadata && p.context_tokens.is_some())
                .count()
        );
    }

    fn model_hint_or_batch(id: &str) -> bool {
        let id = id.to_ascii_lowercase();
        id.contains("[1m]") || id.ends_with(":batch")
    }

    // Explicitly selected by a developer, never part of automatic tests. Reads
    // the existing dev channel without changing credentials/config/registration.
    #[tokio::test]
    #[ignore = "spends a small amount of the configured dev OpenRouter quota"]
    async fn live_dev_openrouter_normalized_requests() {
        use crate::channel_executor::{ChannelRequest, execute_channel_request};
        let raw = std::fs::read(crate::home_dir().join(".const-api-dev/client.json")).unwrap();
        let config: crate::model::ClientConfig = serde_json::from_slice(&raw).unwrap();
        let mut channel = config
            .channels
            .into_iter()
            .find(|channel| {
                channel.source_driver() == SourceDriverId::Openrouter && channel.enabled
            })
            .expect("an enabled OpenRouter channel in the existing dev configuration");
        normalize_channel(&mut channel);
        let client = crate::short_http_client();
        let models = fetch_models(
            client,
            &channel.upstream_base_url,
            &channel.v2.credential_ref,
        )
        .await
        .unwrap();
        channel.models = public_models(models.clone());
        let catalog = crate::tool_model_metadata::active_tool_catalog_models();
        let priced = channel
            .models
            .iter()
            .filter(|id| {
                catalog
                    .iter()
                    .any(|entry| entry.id.eq_ignore_ascii_case(id))
            })
            .count();
        println!(
            "OpenRouter interactive={} compatible_with_existing_catalog={priced}",
            channel.models.len()
        );
        let model = ["deepseek-v4-flash", "qwen3.8-27b"]
            .into_iter()
            .find(|model| channel.models.iter().any(|id| id == model))
            .expect("one of the small, bounded-output smoke-test models must be account-available");
        let prompt = format!("{}\nReply only OK.", "CONST deterministic prefix: a model identifier must not change tool state or cache content. ".repeat(128));
        let identity = format!("c1_openrouter_dev_smoke_{}", crate::now_unix());
        for (protocol, path, stream) in [
            ("openai_chat", "/v1/chat/completions", false),
            ("openai_responses", "/v1/responses", true),
            ("anthropic_messages", "/anthropic/v1/messages", true),
        ] {
            let mut providers = Vec::new();
            for attempt in 1..=2 {
                let body = match protocol {
                    "openai_chat" => {
                        serde_json::json!({"model":model,"messages":[{"role":"user","content":prompt}],"max_tokens":128,"reasoning":{"effort":"none"},"stream":stream})
                    }
                    "openai_responses" => {
                        serde_json::json!({"model":model,"input":prompt,"store":false,"max_output_tokens":128,"reasoning":{"effort":"none"},"stream":stream})
                    }
                    _ => {
                        serde_json::json!({"model":model,"messages":[{"role":"user","content":prompt}],"max_tokens":128,"thinking":{"type":"disabled"},"stream":stream})
                    }
                };
                let started = std::time::Instant::now();
                let result = execute_channel_request(
                    crate::long_http_client(),
                    &channel,
                    ChannelRequest {
                        method: reqwest::Method::POST,
                        path: path.into(),
                        raw_query: String::new(),
                        has_query: false,
                        headers: reqwest::header::HeaderMap::new(),
                        body: serde_json::to_vec(&body).unwrap().into(),
                        stream_requested: Some(stream),
                        selected_upstream_model: Some(model.into()),
                        selected_target_protocol: Some(protocol.into()),
                        cache_identity: Some(identity.clone()),
                        safety_identifier: None,
                    },
                )
                .await
                .unwrap();
                assert_eq!(result.upstream_model, model);
                let status = result.response.status();
                let report =
                    response_diagnostics(&result.response).expect("OpenRouter response observer");
                let bytes = result.response.bytes().await.unwrap();
                let mut diagnostic_payload = serde_json::Map::new();
                report.attach(&mut diagnostic_payload);
                let mut usage = crate::supplier::SupplierUpstreamUsageAccumulator::new(protocol);
                usage.push(&bytes);
                let usage = serde_json::to_value(usage.finish()).unwrap();
                let diagnostic = &diagnostic_payload["upstream_diagnostics"];
                println!(
                    "{protocol} attempt={attempt} HTTP={} elapsed_ms={} usage={} diagnostics={}",
                    status.as_u16(),
                    started.elapsed().as_millis(),
                    usage,
                    diagnostic
                );
                assert!(status.is_success(), "upstream HTTP {}", status.as_u16());
                assert!(
                    diagnostic.get("error_status").is_none()
                        && diagnostic.get("error_type").is_none(),
                    "upstream error event"
                );
                let raw = std::str::from_utf8(&bytes).unwrap();
                let events = if stream {
                    raw.lines()
                        .filter_map(|line| line.strip_prefix("data:"))
                        .filter_map(|line| {
                            serde_json::from_str::<serde_json::Value>(line.trim()).ok()
                        })
                        .collect::<Vec<_>>()
                } else {
                    vec![serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()]
                };
                assert!(
                    events
                        .iter()
                        .all(|event| event.get("error").is_none_or(serde_json::Value::is_null)),
                    "HTTP success must not hide an error envelope"
                );
                let completed = events.iter().any(|event| match protocol {
                    "openai_chat" => event
                        .pointer("/choices/0/message/content")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|text| !text.is_empty()),
                    "openai_responses" => event["type"] == "response.completed",
                    _ => event["type"] == "message_stop",
                });
                assert!(
                    completed,
                    "model completion is required, not merely HTTP 200 or metadata"
                );
                assert!(
                    usage["output_tokens"]
                        .as_u64()
                        .is_some_and(|tokens| tokens > 0)
                );
                providers.push(diagnostic["provider"].as_str().map(str::to_string));
            }
            println!(
                "{protocol} provider_stable={} (cache tokens above are authoritative; latency is not cache proof)",
                providers[0].is_some() && providers[0] == providers[1]
            );
        }
    }

    fn channel() -> ChannelConfig {
        let mut channel =
            crate::channel_from_supplier("router".into(), &crate::default_supplier_config());
        channel.set_source_driver(SourceDriverId::Openrouter);
        channel
    }

    async fn catalog_server(status: u16, payload: &str) -> (String, tokio::task::JoinHandle<()>) {
        use warp::Filter;
        let payload = payload.to_string();
        let route = warp::get()
            .and(warp::path!("api" / "v1" / "models" / "user"))
            .map(move || {
                warp::reply::with_status(
                    payload.clone(),
                    warp::http::StatusCode::from_u16(status).unwrap(),
                )
            });
        let (address, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
        (format!("http://{address}/api/v1"), tokio::spawn(server))
    }

    #[tokio::test]
    async fn channel_catalog_applies_the_configured_user_agent_profile() {
        use warp::Filter;
        let route = warp::get()
            .and(warp::path!("api" / "v1" / "models" / "user"))
            .and(warp::header::exact("user-agent", "opencode/1.18.31"))
            .map(|| warp::reply::json(&serde_json::json!({"data":[{"id":"vendor/model"}]})));
        let (address, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
        let server = tokio::spawn(server);
        let mut channel = channel();
        channel.upstream_base_url = format!("http://{address}/api/v1");
        channel.v2.user_agent_profile = crate::channel_user_agent::PROFILE_OPENCODE.into();

        let models = fetch_channel_catalog(&reqwest::Client::new(), &channel)
            .await
            .unwrap();

        server.abort();
        assert_eq!(models[0].id, "vendor/model");
    }

    #[tokio::test]
    async fn openrouter_catalog_keeps_real_capacities_and_excludes_only_batch_entries() {
        let payload = serde_json::json!({"data":[
            {"id":"openai/gpt-5.6-luna:batch","context_length":1000000},
            {"id":"anthropic/claude-sonnet-4.6:BATCH[1M]"},
            {"id":"vendor/branch/MiXeD:free","context_length":"1000000","top_provider":{"context_length":256000,"max_completion_tokens":16000},"architecture":{"input_modalities":["text","image"]}},
            {"id":"vendor/my-batch-model","context_length":32768},
            {"id":"anthropic/claude-sonnet-4.6","context_length":1000000,"top_provider":{"max_completion_tokens":128000}}
        ]});
        let (base, server) = catalog_server(200, &payload.to_string()).await;
        let models = fetch_catalog(&reqwest::Client::new(), &base, "", None)
            .await
            .unwrap();
        server.abort();
        assert_eq!(models.len(), 3);
        assert_eq!(models[0].id, "vendor/branch/MiXeD:free");
        assert_eq!(
            (models[0].context_tokens, models[0].output_tokens),
            (Some(256000), Some(16000))
        );
        assert_eq!(models[0].input_modalities, ["text", "image"]);
        assert_eq!(models[1].id, "vendor/my-batch-model");
        assert_eq!(models[2].context_tokens, Some(1000000));
    }

    #[tokio::test]
    async fn openrouter_invalid_catalog_never_becomes_an_empty_success() {
        for (status, payload, expected) in [
            (200, r#"{"data":[null,{},7,{"id":" "}]}"#, "no valid models"),
            (
                200,
                r#"{"error":{"message":"not a catalog"}}"#,
                "error envelope",
            ),
            (200, "<html>temporary gateway error</html>", "non-JSON"),
            (401, r#"{"error":{"message":"Unauthorized"}}"#, "401"),
            (429, r#"{"error":{"message":"rate limited"}}"#, "429"),
            (
                503,
                r#"{"error":{"message":"temporarily unavailable"}}"#,
                "503",
            ),
        ] {
            let (base, server) = catalog_server(status, payload).await;
            let result = fetch_catalog(&reqwest::Client::new(), &base, "", None).await;
            server.abort();
            let error = format!(
                "{:#}",
                result.expect_err("invalid catalog cannot replace the last usable list")
            );
            assert!(error.contains(expected), "HTTP {status}: {error}");
        }
    }

    #[tokio::test]
    async fn openrouter_snapshot_refreshes_all_protocol_metadata_without_generation_or_losing_config_on_failure()
     {
        use std::sync::{Arc, Mutex};
        use warp::Filter;
        let payload = Arc::new(Mutex::new(serde_json::json!({"data":[
            {"id":"anthropic/claude-sonnet-4.6", "context_length":256000,"top_provider":{"max_completion_tokens":32000}},
            {"id":"Vendor/Branch/Unknown:free","context_length":64000},
            {"id":"Vendor/batch-only:batch"}
        ]}).to_string()));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let reply_payload = payload.clone();
        let captured = calls.clone();
        let route = warp::method().and(warp::path::full()).map(
            move |method: warp::http::Method, path: warp::path::FullPath| {
                captured
                    .lock()
                    .unwrap()
                    .push((method.to_string(), path.as_str().to_string()));
                let status = if method == warp::http::Method::GET
                    && path.as_str() == "/api/v1/models/user"
                {
                    200
                } else {
                    405
                };
                warp::reply::with_status(
                    reply_payload.lock().unwrap().clone(),
                    warp::http::StatusCode::from_u16(status).unwrap(),
                )
            },
        );
        let (address, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
        let server = tokio::spawn(server);
        let mut source = channel();
        source.upstream_base_url = format!("http://{address}/api/v1");
        source.surface_bindings = crate::source_driver_surface_bindings(
            SourceDriverId::Openrouter,
            &source.upstream_base_url,
        );
        for binding in &mut source.surface_bindings {
            binding.verification.state = "verified".into();
            for protocol in &mut binding.protocols {
                protocol.verification.state = "verified".into();
            }
        }
        source.models = vec!["claude-sonnet-4-6".into()];
        source.capability_profiles = vec![crate::model::ChannelCapabilityProfile {
            protocol: "openai_chat".into(),
            model_pattern: "claude-sonnet-4-6".into(),
            catalog_metadata: true,
            context_tokens: Some(1000000),
            verification_state: "declared".into(),
            ..Default::default()
        }];
        let snapshot = crate::detection::snapshot_channel(source.clone())
            .await
            .unwrap();
        assert_eq!(snapshot.surface_results.len(), 3);
        assert_eq!(
            snapshot.models,
            ["claude-sonnet-4-6", "Vendor/Branch/Unknown:free"]
        );
        for protocol in ["openai_chat", "openai_responses", "anthropic_messages"] {
            let profile = snapshot
                .model_capability_evidence
                .iter()
                .find(|profile| {
                    profile.protocol == protocol
                        && profile.model_pattern == "claude-sonnet-4-6"
                        && profile.catalog_metadata
                })
                .unwrap();
            assert_eq!(
                (profile.context_tokens, profile.output_tokens),
                (Some(256000), Some(32000))
            );
            assert_eq!(
                profile.verification_state, "declared",
                "catalog evidence must not claim a paid inference probe passed"
            );
        }
        crate::supplier::merge_channel_detection_result(&mut source, &snapshot);
        assert_eq!(
            crate::tool_model_metadata::channel_token_limits(&source, "claude-sonnet-4-6"),
            (Some(256000), Some(32000))
        );
        let saved = serde_json::to_value(&source).unwrap();
        *payload.lock().unwrap() = "{\"error\":{\"message\":\"temporary failure\"}}".into();
        assert!(
            crate::detection::snapshot_channel(source.clone())
                .await
                .is_err()
        );
        assert_eq!(serde_json::to_value(&source).unwrap(), saved);
        server.abort();
        let calls = calls.lock().unwrap();
        // All three protocols share one account catalog in this refresh.
        // The failed refresh adds one request, and no inference.
        assert_eq!(calls.len(), 2);
        assert!(
            calls
                .iter()
                .all(|(method, path)| method == "GET" && path == "/api/v1/models/user")
        );
    }

    #[test]
    fn openrouter_normalization_is_idempotent_and_does_not_touch_other_drivers() {
        let mut source = channel();
        source.models = vec![
            "anthropic/claude-sonnet-4.6[1m]".into(),
            "anthropic/claude-sonnet-4.6".into(),
            "openai/gpt-5.6-luna:batch".into(),
            "First/Branch/MODEL:free".into(),
            "Second/model:free".into(),
        ];
        source.upstream_model = source.models[0].clone();
        let mut ordinary = source.clone();
        ordinary.set_source_driver(SourceDriverId::OpenAiApi);
        let untouched = serde_json::to_value(&ordinary).unwrap();
        normalize_channel(&mut ordinary);
        assert_eq!(serde_json::to_value(ordinary).unwrap(), untouched);
        normalize_channel(&mut source);
        let once = serde_json::to_value(&source).unwrap();
        normalize_channel(&mut source);
        assert_eq!(serde_json::to_value(&source).unwrap(), once);
        let presented = crate::tool_model_metadata::tool_models_from_ids(&source.models);
        assert_eq!(
            presented
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["claude-sonnet-4-6", "model:free"]
        );
        let supplier = crate::supplier_from_channel(&source);
        assert_eq!(
            crate::supplier::supplier_upstream_model_for_request(&supplier, Some("model:free")),
            "First/Branch/MODEL:free"
        );
    }

    #[test]
    fn openrouter_wire_wrapper_preserves_payload_bytes_for_model_like_content_and_escaped_ids() {
        let source = channel();
        for wire_json in [r#""gpt-5.6-luna""#, r#""gpt-5.6-\u006cuna""#] {
            let body = format!(
                "{{\n \"input\":[{{\"model\":\"gpt-5.6-luna\",\"text\":\"\\\"model\\\":\\\"gpt-5.6-luna\\\"\",\"encrypted_content\":\"opaque\\nbytes\"}}], \"model\":{wire_json},\"prompt_cache_key\":\"unchanged\" }}"
            );
            let expected = body.replacen(
                &format!("\"model\":{wire_json},\"prompt_cache_key\""),
                "\"model\":\"openai/gpt-5.6-luna\",\"prompt_cache_key\"",
                1,
            );
            let adapted = request_body(Some(&source), &body);
            assert_eq!(adapted, expected);
            assert!(matches!(
                request_body(Some(&source), &adapted),
                Cow::Borrowed(_)
            ));
        }
        for body in [
            "{}",
            "null",
            "[]",
            r#"{"model":null}"#,
            r#"{"model":123}"#,
            r#"{"nested":{"model":"gpt-5.6-luna"}}"#,
        ] {
            assert!(matches!(
                request_body(Some(&source), body),
                Cow::Borrowed(_)
            ));
        }
    }

    #[test]
    fn openrouter_variant_names_never_collapse_to_a_paid_base_model_on_the_wire() {
        for wire in [
            "Vendor/Branch/MiXeD:free",
            "vendor/model:extended",
            "vendor/model:free[1m]",
        ] {
            let public = crate::config::public_model_name(wire);
            let mut source = channel();
            source.models = vec![wire.into()];
            let supplier = crate::supplier_from_channel(&source);
            let selected =
                crate::supplier::supplier_upstream_model_for_request(&supplier, Some(&public));
            assert_eq!(selected, wire);
            let native = upstream_model(&source, &selected);
            assert_eq!(native, crate::config::without_context_hint(wire));
            assert!(native.contains(':'));
        }
    }

    #[test]
    fn public_catalog_and_wire_roundtrip_keep_existing_tool_names() {
        let channel = channel();
        for (upstream, public) in [
            ("openai/gpt-5.6-luna", "gpt-5.6-luna"),
            ("anthropic/claude-fable-5.1", "claude-fable-5-1"),
            ("google/gemini-3.7-flash", "gemini-3.7-flash"),
            ("qwen/qwen3.8-27b", "qwen3.8-27b"),
            ("z-ai/glm-5.3", "glm-5.3"),
            ("minimax/minimax-m3", "minimax-m3"),
            ("deepseek/deepseek-v4.1-flash", "deepseek-v4.1-flash"),
            ("deepseek/deepseek-v4-pro-0813", "deepseek-v4-pro-0813"),
            ("qwen/qwen3.8-max-0902", "qwen3.8-max-0902"),
            ("inception/mercury-2.5", "mercury-2.5"),
            ("inclusionai/ling-3.0-flash-vl", "ling-3.0-flash-vl"),
            (
                "meta-llama/llama-3.3-70b-instruct",
                "llama-3.3-70b-instruct",
            ),
            ("bytedance-seed/seed-2.0-code", "seed-2.0-code"),
        ] {
            assert_eq!(public_models(vec![upstream.into()]), vec![public]);
            assert_eq!(upstream_model(&channel, public), upstream);
        }
        assert_eq!(
            upstream_model(&channel, "qwen/qwen3.8-27b"),
            "qwen/qwen3.8-27b"
        );
        assert_eq!(upstream_model(&channel, "MiniMax-M3"), "minimax/minimax-m3");
    }

    #[test]
    fn variants_unknown_names_and_unreviewed_dated_versions_keep_wire_ids() {
        let models = [
            "openai/gpt-5.6-luna:batch",
            "openai/gpt-5.6-luna:free",
            "anthropic/claude-sonnet-4-6[1m]",
            "other/gpt-5.6-luna",
            "deepseek/deepseek-v4-pro-20991231",
            "~openai/gpt-latest",
        ];
        let actual = public_models(models.iter().map(|s| s.to_string()).collect());
        assert_eq!(
            actual,
            [
                "openai/gpt-5.6-luna:free",
                "claude-sonnet-4-6",
                "other/gpt-5.6-luna",
                "deepseek/deepseek-v4-pro-20991231",
                "~openai/gpt-latest"
            ]
        );
        for id in actual {
            let expected = if id == "claude-sonnet-4-6" {
                "anthropic/claude-sonnet-4.6"
            } else {
                &id
            };
            assert_eq!(upstream_model(&channel(), &id), expected);
        }
    }

    #[test]
    fn legacy_catalog_is_normalized_without_new_configuration_fields() {
        let mut channel = channel();
        channel.models = vec![
            "openai/gpt-5.6-luna".into(),
            "anthropic/openai/gpt-5.6-luna".into(),
        ];
        channel.upstream_model = channel.models[0].clone();
        channel.capability_profiles = vec![crate::model::ChannelCapabilityProfile {
            model_pattern: channel.models[1].clone(),
            custom_tool: true,
            ..Default::default()
        }];
        let normalized = crate::config::normalize_channel(channel, 0, None, None);
        assert_eq!(normalized.models, vec!["gpt-5.6-luna"]);
        assert_eq!(normalized.upstream_model, "gpt-5.6-luna");
        assert_eq!(
            normalized.capability_profiles[0].model_pattern,
            "gpt-5.6-luna"
        );
        let stored = serde_json::to_value(crate::model::ChannelConfigV2::from(normalized)).unwrap();
        let restored: ChannelConfig =
            serde_json::from_value::<crate::model::ChannelConfigV2>(stored)
                .unwrap()
                .into();
        assert_eq!(
            upstream_model(&restored, &restored.models[0]),
            "openai/gpt-5.6-luna"
        );
    }

    #[test]
    fn wire_wrapper_changes_only_top_level_model_and_only_openrouter() {
        let original = "{ \"model\": \"gpt-5.6-luna\", \"input\": [ {\"type\":\"custom_tool_call_output\",\"call_id\":\"call_abc\",\"output\":\"ok\"}, {\"encrypted_content\":\"exact bytes\",\"model\":\"nested\"} ], \"store\":false }";
        assert_eq!(
            request_body(Some(&channel()), original),
            original.replacen("gpt-5.6-luna", "openai/gpt-5.6-luna", 1)
        );
        assert_eq!(request_body(None, original), original);
        let mut other = channel();
        other.set_source_driver(SourceDriverId::OpenAiApi);
        assert_eq!(request_body(Some(&other), original), original);
        assert_eq!(
            request_body(Some(&channel()), "{\"type\":\"response.cancel\"}"),
            "{\"type\":\"response.cancel\"}"
        );
    }

    #[test]
    fn qwen_short_billing_name_is_never_sent_as_the_wire_model() {
        let source = channel();
        let body =
            r#"{"model":"qwen3.8-27b","messages":[{"role":"user","content":"qwen3.8-27b"}]}"#;
        let rewritten = request_body(Some(&source), body);
        assert_eq!(
            rewritten,
            body.replacen("qwen3.8-27b", "qwen/qwen3.8-27b", 1)
        );
        assert!(matches!(
            request_body(Some(&source), &rewritten),
            Cow::Borrowed(_)
        ));
        assert_eq!(upstream_model(&source, "qwen3.8-27b"), "qwen/qwen3.8-27b");
        assert_eq!(
            upstream_model(&source, "qwen/qwen3.8-27b"),
            "qwen/qwen3.8-27b"
        );
    }
}
