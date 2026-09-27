//! Account-scoped discovery. Never turn an authentication failure or an empty
//! allowed list into a successful refresh of the public, unfiltered catalog.
use crate::model_catalog::{ModelCatalogKind, ModelObservation};
use crate::upstream_transport::AdaptiveRequestBuilderExt;
use anyhow::{Context, Result, anyhow, ensure};
use reqwest::{Client, Url};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

const MAX_CATALOG_BYTES: usize = 16 * 1024 * 1024;
const MAX_CATALOG_PAGES: usize = 100;

pub(crate) fn protocol_probe_model(
    channel: &crate::model::ChannelConfig,
    catalog: &[ModelObservation],
    selected: String,
) -> Result<String> {
    if channel.source_driver() != crate::source_driver::SourceDriverId::Openrouter {
        return Ok(selected);
    }
    let conversational = catalog
        .iter()
        .filter(|model| {
            model.output_modalities.is_empty()
                || model.output_modalities.iter().any(|kind| kind == "text")
        })
        .map(|model| model.id.clone())
        .collect::<Vec<_>>();
    ensure!(
        !conversational.is_empty(),
        "OpenRouter account catalog has no text-output model for conversation protocol verification"
    );
    if conversational
        .iter()
        .any(|model| model.eq_ignore_ascii_case(&selected))
    {
        return Ok(selected);
    }
    // All modalities belong in discovery, but a text protocol probe must not
    // accidentally invoke speech, video, embedding or rerank models.
    Ok(crate::detection::pick_primary_model(
        &conversational,
        channel,
    ))
}

pub(crate) fn apply_refreshed_catalog(channel: &mut crate::model::ChannelConfig) {
    if channel.source_driver() != crate::source_driver::SourceDriverId::Openrouter {
        return;
    }
    let default = if channel.models.is_empty() {
        String::new()
    } else {
        channel.default_model_from(&channel.models)
    };
    // A successful empty account catalog is not an unknown legacy catalog.
    // Clear its fallback IDs so local/platform listings cannot resurrect them.
    channel.public_model = default.clone();
    channel.upstream_model = default;
}

pub(crate) fn empty_catalog_health(
    channel: &crate::model::ChannelConfig,
    latency_ms: u128,
    now: i64,
) -> Option<crate::model::SupplierChannelHealth> {
    if channel.source_driver() != crate::source_driver::SourceDriverId::Openrouter
        || !channel.models.is_empty()
    {
        return None;
    }
    let mut health = crate::supplier::failed_supplier_channel_health(
        channel,
        "OpenRouter account catalog currently allows no interactive models".into(),
        latency_ms,
        now,
    );
    // Discovery authenticated successfully. Commit this authoritative empty
    // snapshot; it is not an unknown/network failure retaining the old catalog.
    health.credential_status = "ok".into();
    health.model.clear();
    Some(health)
}

fn catalog_url(base_url: &str) -> Result<Url> {
    let mut url = Url::parse(&crate::supplier::join_upstream_url(
        base_url,
        "/v1/models/user",
    ))?;
    url.query_pairs_mut()
        .append_pair("output_modalities", "all");
    Ok(url)
}

pub(crate) fn catalog_cache_key(
    driver: crate::source_driver::SourceDriverId,
    surface: crate::surface::ApiSurface,
    base_url: &str,
) -> (crate::surface::ApiSurface, String) {
    if driver == crate::source_driver::SourceDriverId::Openrouter {
        if let Ok(url) = catalog_url(base_url) {
            // Chat, Responses and Messages share the same account directory.
            // Only this refresh is cached; distinct regional origins stay separate.
            return (crate::surface::ApiSurface::OpenAi, url.into());
        }
    }
    (surface, base_url.into())
}

pub(crate) async fn fetch_catalog(
    client: &Client,
    base_url: &str,
    key: &str,
    user_agent: Option<&str>,
) -> Result<Vec<ModelObservation>> {
    let mut url = catalog_url(base_url)?;
    let source = url.to_string();
    let mut models = Vec::new();
    let mut seen = HashSet::new();
    let mut offset = 0_u64;
    for page_index in 0..MAX_CATALOG_PAGES {
        let request = client.get(url.clone());
        let request = if let Some(user_agent) = user_agent {
            request.header(reqwest::header::USER_AGENT, user_agent)
        } else {
            request
        };
        let request = if key.trim().is_empty() {
            request
        } else {
            request.bearer_auth(key.trim())
        };
        let response = request
            .send_adaptive()
            .await
            .context("OpenRouter account catalog request failed")?;
        let status = response.status();
        let etag = response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        ensure!(
            response
                .content_length()
                .is_none_or(|n| n <= MAX_CATALOG_BYTES as u64),
            "OpenRouter account catalog response is too large"
        );
        let mut response = response;
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .context("OpenRouter account catalog read failed")?
        {
            ensure!(
                bytes.len().saturating_add(chunk.len()) <= MAX_CATALOG_BYTES,
                "OpenRouter account catalog response is too large"
            );
            bytes.extend_from_slice(&chunk);
        }
        // Do not echo arbitrary gateway HTML, credentials or provider prompts.
        ensure!(
            status.is_success(),
            "OpenRouter account catalog returned HTTP {status}; allowed models were not refreshed"
        );
        let value: Value = serde_json::from_slice(&bytes)
            .context("OpenRouter account catalog returned non-JSON data")?;
        ensure!(
            value.get("error").is_none_or(Value::is_null),
            "OpenRouter account catalog returned an error envelope"
        );
        let entries = value
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("OpenRouter account catalog omitted the data array"))?;
        let raw_count = entries.len() as u64;
        let mut page = crate::model_catalog::model_observations_from_value(
            ModelCatalogKind::OpenAI,
            &value,
            &source,
        );
        ensure!(
            entries.is_empty() || !page.is_empty(),
            "OpenRouter account catalog contained no valid models"
        );
        let before = seen.len();
        let mut by_id = HashMap::new();
        for entry in entries {
            if let Some(id) = entry.get("id").and_then(Value::as_str) {
                by_id.entry(id.trim().to_ascii_lowercase()).or_insert(entry);
            }
        }
        for model in &mut page {
            model.etag = etag.clone();
            if let Some(entry) = by_id.get(&model.id.to_ascii_lowercase()) {
                model.supported_parameters = entry
                    .get("supported_parameters")
                    .and_then(Value::as_array)
                    .filter(|items| items.iter().all(Value::is_string))
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(Value::as_str)
                            .map(|s| s.trim().to_ascii_lowercase())
                            .collect()
                    });
            }
            seen.insert(model.id.to_ascii_lowercase());
        }
        models.extend(
            page.into_iter()
                .filter(|model| !super::is_batch_model(&model.id)),
        );
        offset = offset.saturating_add(raw_count);
        let next = value
            .pointer("/links/next")
            .filter(|v| !v.is_null())
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty());
        let incomplete = value
            .get("total_count")
            .and_then(Value::as_u64)
            .is_some_and(|n| offset < n);
        if !next && !incomplete {
            return Ok(crate::model_catalog::stable_dedupe_models(models));
        }
        ensure!(
            raw_count > 0 && seen.len() > before && page_index + 1 < MAX_CATALOG_PAGES,
            "OpenRouter account catalog pagination did not complete"
        );
        // Keep the configured origin and account. Never follow a server-provided
        // next URL with our credential (including across regional domains).
        url.set_query(None);
        url.query_pairs_mut()
            .append_pair("output_modalities", "all")
            .append_pair("offset", &offset.to_string())
            .append_pair("limit", "1000");
    }
    Err(anyhow!(
        "OpenRouter account catalog pagination exceeded its limit"
    ))
}

pub(crate) fn apply_catalog_capabilities(
    profile: &mut crate::model::ChannelCapabilityProfile,
    model: &ModelObservation,
) {
    let Some(parameters) = &model.supported_parameters else {
        return;
    };
    let has = |name| parameters.iter().any(|parameter| parameter == name);
    // These are declarations, not paid semantic probes. Missing fields do not
    // reject working routes, and tools never imply Codex custom/hosted tools.
    profile.tool_calls = has("tools");
    profile.tool_choice = has("tool_choice");
    profile.parallel_tool_calls = has("parallel_tool_calls");
    profile.json_schema = has("structured_outputs");
    profile.reasoning = has("reasoning") || has("reasoning_effort");
    profile.cache_control = has("cache_control");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use warp::Filter;

    #[test]
    fn conversation_probe_does_not_use_non_text_models_from_the_all_modalities_catalog() {
        use crate::source_driver::SourceDriverId;
        let catalog = vec![
            ModelObservation {
                id: "vendor/speech".into(),
                output_modalities: vec!["speech".into()],
                ..Default::default()
            },
            ModelObservation {
                id: "vendor/text".into(),
                output_modalities: vec!["text".into()],
                ..Default::default()
            },
        ];
        let mut channel =
            crate::channel_from_supplier("test".into(), &crate::default_supplier_config());
        for driver in crate::source_driver::ALL_SOURCE_DRIVER_IDS {
            channel.set_source_driver(driver);
            assert_eq!(
                protocol_probe_model(&channel, &catalog, "vendor/speech".into()).unwrap(),
                if driver == SourceDriverId::Openrouter {
                    "vendor/text"
                } else {
                    "vendor/speech"
                }
            );
        }
        channel.set_source_driver(SourceDriverId::Openrouter);
        assert!(protocol_probe_model(&channel, &catalog[..1], "vendor/speech".into()).is_err());
        assert_eq!(
            protocol_probe_model(&channel, &catalog, "vendor/text".into()).unwrap(),
            "vendor/text"
        );
    }

    #[test]
    fn account_catalog_cache_is_refresh_local_and_keeps_origins_and_other_drivers_separate() {
        use crate::{source_driver::SourceDriverId, surface::ApiSurface};
        let openai = catalog_cache_key(
            SourceDriverId::Openrouter,
            ApiSurface::OpenAi,
            "https://openrouter.ai/api/v1",
        );
        let anthropic = catalog_cache_key(
            SourceDriverId::Openrouter,
            ApiSurface::Anthropic,
            "https://openrouter.ai/api",
        );
        assert_eq!(openai, anthropic);
        assert_ne!(
            openai,
            catalog_cache_key(
                SourceDriverId::Openrouter,
                ApiSurface::OpenAi,
                "https://eu.openrouter.ai/api/v1"
            )
        );
        for driver in crate::source_driver::ALL_SOURCE_DRIVER_IDS {
            if driver != SourceDriverId::Openrouter {
                assert_eq!(
                    catalog_cache_key(driver, ApiSurface::Anthropic, "https://example.test/api"),
                    (ApiSurface::Anthropic, "https://example.test/api".into())
                );
            }
        }
    }

    #[tokio::test]
    async fn duplicate_parameter_declarations_keep_the_first_entry_without_overstating_capacity() {
        let route = warp::get().map(|| warp::reply::json(&json!({"data":[
            {"id":"vendor/MODEL","context_length":200000,"supported_parameters":["tools"]},
            {"id":"vendor/model","context_length":1000000,"supported_parameters":["tools","parallel_tool_calls"]}
        ]})));
        let (addr, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
        let server = tokio::spawn(server);
        let models = fetch_catalog(&Client::new(), &format!("http://{addr}"), "", None)
            .await
            .unwrap();
        server.abort();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].context_tokens, Some(200000));
        assert_eq!(models[0].supported_parameters, Some(vec!["tools".into()]));
    }

    #[tokio::test]
    async fn account_catalog_keeps_all_modalities_and_paginates_without_following_external_links() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let route = warp::get().and(warp::path::full()).and(warp::query::<HashMap<String, String>>())
            .and(warp::header::<String>("authorization"))
            .map(move |path: warp::path::FullPath, query: HashMap<String, String>, auth: String| {
                assert_eq!(path.as_str(), "/api/v1/models/user");
                assert_eq!(query.get("output_modalities").map(String::as_str), Some("all"));
                assert_eq!(auth, "Bearer account-fixture");
                captured.lock().unwrap().push(query.clone());
                if query.contains_key("offset") {
                    assert_eq!(query["offset"], "2");
                    warp::reply::json(&json!({"data":[
                        {"id":"vendor/speech","architecture":{"output_modalities":["speech"]}},
                        {"id":"vendor/batch:batch"}
                    ],"total_count":4,"links":{"next":null}}))
                } else {
                    warp::reply::json(&json!({"data":[
                        {"id":"vendor/text","context_length":1000000,"top_provider":{"context_length":200000},"supported_parameters":["tools","tool_choice","structured_outputs","reasoning"]},
                        {"id":"vendor/embedding","architecture":{"output_modalities":["embeddings"]}}
                    ],"total_count":4,"links":{"next":"https://untrusted.invalid/steal-credentials"}}))
                }
            });
        let (addr, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
        let server = tokio::spawn(server);
        let models = fetch_catalog(
            &Client::new(),
            &format!("http://{addr}/api/v1"),
            "account-fixture",
            None,
        )
        .await
        .unwrap();
        server.abort();
        assert_eq!(models.len(), 3);
        assert_eq!(requests.lock().unwrap().len(), 2);
        assert_eq!(models[0].context_tokens, Some(200000));
        assert_eq!(models[1].output_modalities, ["embeddings"]);
        assert_eq!(models[2].output_modalities, ["speech"]);
        let mut profile = crate::model::ChannelCapabilityProfile::default();
        apply_catalog_capabilities(&mut profile, &models[0]);
        assert!(
            profile.tool_calls && profile.tool_choice && profile.json_schema && profile.reasoning
        );
        assert!(!profile.custom_tool && profile.hosted_tools.is_empty());
    }

    #[tokio::test]
    async fn valid_empty_account_catalog_clears_snapshot_but_malformed_data_does_not() {
        let payload = Arc::new(Mutex::new(json!({"data":[],"total_count":0})));
        let reply = payload.clone();
        let route = warp::get().map(move || warp::reply::json(&*reply.lock().unwrap()));
        let (addr, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
        let server = tokio::spawn(server);
        let mut channel =
            crate::channel_from_supplier("router-empty".into(), &crate::default_supplier_config());
        channel.set_source_driver(crate::source_driver::SourceDriverId::Openrouter);
        channel.upstream_base_url = format!("http://{addr}/api/v1");
        channel.surface_bindings = crate::source_driver_surface_bindings(
            channel.source_driver(),
            &channel.upstream_base_url,
        );
        for binding in &mut channel.surface_bindings {
            binding.verification.state = "verified".into();
        }
        channel.models = vec!["old-model".into()];
        channel.public_model = "old-model".into();
        channel.upstream_model = "old-model".into();
        let result = crate::detection::snapshot_channel(channel.clone())
            .await
            .unwrap();
        crate::supplier::merge_channel_detection_result(&mut channel, &result);
        assert!(channel.models.is_empty());
        assert!(channel.public_model.is_empty() && channel.upstream_model.is_empty());
        let normalized = crate::normalize_channel(channel.clone(), 0, None, None);
        assert!(normalized.models.is_empty());
        assert!(crate::proxy::visible_channel_models(&normalized).is_empty());
        let health = empty_catalog_health(&normalized, 1, crate::now_unix()).unwrap();
        assert_eq!(health.status, "error");
        assert_eq!(health.model_count, 0);
        assert_eq!(
            crate::supplier::successful_supplier_monitor_health(&[health]).len(),
            1
        );
        *payload.lock().unwrap() = json!({"error":{"message":"temporary"}});
        assert!(crate::detection::snapshot_channel(channel).await.is_err());
        server.abort();
    }

    #[tokio::test]
    async fn repeated_partial_catalog_is_an_error_not_a_truncated_success() {
        let route = warp::get()
            .map(|| warp::reply::json(&json!({"data":[{"id":"vendor/one"}],"total_count":3})));
        let (addr, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
        let server = tokio::spawn(server);
        let error = fetch_catalog(&Client::new(), &format!("http://{addr}"), "", None)
            .await
            .unwrap_err();
        server.abort();
        assert!(error.to_string().contains("pagination"));
    }
}
