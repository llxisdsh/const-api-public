use crate::upstream_transport::AdaptiveRequestBuilderExt;
use anyhow::{Context, Result, anyhow};
use reqwest::{Client, Url, header::HeaderMap};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ModelObservation {
    pub(crate) id: String,
    pub(crate) priority: Option<i64>,
    pub(crate) source: String,
    pub(crate) etag: Option<String>,
    /// Transport dialect declared by the Codex model manifest. `None` means the
    /// catalog did not make a claim; it must not be guessed from the model name.
    pub(crate) use_responses_lite: Option<bool>,
    pub(crate) input_modalities: Vec<String>,
    pub(crate) output_modalities: Vec<String>,
    pub(crate) context_tokens: Option<u64>,
    pub(crate) output_tokens: Option<u64>,
    /// Optional parameter declarations populated by source-specific discovery.
    pub(crate) supported_parameters: Option<Vec<String>>,
    /// Explicit endpoint contract from a maintained provider's catalog.
    pub(crate) supported_endpoints: Option<Vec<String>>,
    /// Kiro's per-model contract for optional native generation controls.
    pub(crate) kiro_request_schema: Option<serde_json::Value>,
    /// Catalog quote for a tiny 32-input/16-output check, never a billing rule.
    pub(crate) probe_cost_nanos: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModelCatalogKind {
    OpenAI,
    CommandCode,
    Copilot,
    Abacus,
    Poe,
    Grok,
    Codex,
    Anthropic,
    Gemini,
    Ollama,
}

pub(crate) fn stable_dedupe_models(observations: Vec<ModelObservation>) -> Vec<ModelObservation> {
    let mut positions: HashMap<String, usize> = HashMap::new();
    let mut unique: Vec<ModelObservation> = Vec::new();
    for mut observation in observations {
        observation.id = observation.id.trim().to_string();
        if observation.id.is_empty() {
            continue;
        }
        let comparison_key = observation.id.to_lowercase();
        if let Some(index) = positions.get(&comparison_key).copied() {
            // Conflicting declarations must not overstate the usable window.
            unique[index].context_tokens =
                conservative_limit(unique[index].context_tokens, observation.context_tokens);
            unique[index].output_tokens =
                conservative_limit(unique[index].output_tokens, observation.output_tokens);
            if unique[index].priority.is_none() && observation.priority.is_some() {
                unique[index].priority = observation.priority;
            }
            if unique[index].use_responses_lite.is_none()
                && observation.use_responses_lite.is_some()
            {
                unique[index].use_responses_lite = observation.use_responses_lite;
            }
            merge_unique_strings(
                &mut unique[index].input_modalities,
                observation.input_modalities,
            );
            merge_unique_strings(
                &mut unique[index].output_modalities,
                observation.output_modalities,
            );
            continue;
        }
        positions.insert(comparison_key, unique.len());
        unique.push(observation);
    }
    if unique.iter().any(|item| item.priority.is_some()) {
        unique.sort_by_key(|item| item.priority.unwrap_or(i64::MAX));
    }
    unique
}

pub(crate) fn model_ids(observations: Vec<ModelObservation>) -> Vec<String> {
    stable_dedupe_models(observations)
        .into_iter()
        .map(|observation| observation.id)
        .collect()
}

/// Merge the authenticated OpenCode Console price hints into the models
/// returned by the public inference directory.
///
/// Console's `/api/v2/config` is workspace-scoped and its prices are advisory
/// for the local catalog only. They must never be serialized as a platform
/// billing rule. OpenCode expresses prices as USD per million tokens, while
/// `probe_cost_nanos` is the estimated USD cost of a 32-input/16-output probe
/// in nanos, so the conversion includes the per-million divisor.
pub(crate) fn apply_opencode_console_costs(
    models: &mut [ModelObservation],
    config: &serde_json::Value,
) {
    let Some(provider_models) = config
        .get("providers")
        .and_then(|providers| providers.get("opencode"))
        .and_then(|provider| provider.get("models"))
        .and_then(serde_json::Value::as_object)
    else {
        return;
    };

    for model in models {
        let entry = provider_models
            .get(&model.id)
            .or_else(|| {
                provider_models
                    .iter()
                    .find_map(|(id, value)| id.eq_ignore_ascii_case(&model.id).then_some(value))
            })
            .or_else(|| {
                provider_models.values().find(|value| {
                    value
                        .get("modelID")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|id| id.eq_ignore_ascii_case(&model.id))
                })
            });
        let Some(entry) = entry else {
            continue;
        };
        let Some(probe_cost) = opencode_console_probe_cost(entry.get("cost")) else {
            continue;
        };
        model.probe_cost_nanos = Some(
            model
                .probe_cost_nanos
                .map_or(probe_cost, |current| current.min(probe_cost)),
        );
    }
}

fn opencode_console_probe_cost(value: Option<&serde_json::Value>) -> Option<u64> {
    let entries = match value {
        Some(value) if value.is_array() => value.as_array()?.as_slice(),
        Some(value) => std::slice::from_ref(value),
        None => return None,
    };
    entries
        .iter()
        .filter_map(|entry| {
            let input = entry.get("input").and_then(json_f64)?;
            let output = entry.get("output").and_then(json_f64)?;
            let cost = (input * 32.0 + output * 16.0) * 1_000.0;
            (cost.is_finite() && cost >= 0.0 && cost <= u64::MAX as f64)
                .then_some(cost.ceil() as u64)
        })
        .min()
}

pub(crate) fn model_observations_from_value(
    kind: ModelCatalogKind,
    value: &serde_json::Value,
    source: &str,
) -> Vec<ModelObservation> {
    let mut out = Vec::new();
    match kind {
        ModelCatalogKind::OpenAI
        | ModelCatalogKind::CommandCode
        | ModelCatalogKind::Anthropic
        | ModelCatalogKind::Abacus
        | ModelCatalogKind::Poe => {
            if let Some(items) = value.get("data").and_then(|value| value.as_array()) {
                for item in items {
                    if kind == ModelCatalogKind::Abacus
                        && item["model_type"]
                            .as_str()
                            .is_some_and(|kind| kind != "text_generation")
                    {
                        continue;
                    }
                    // Poe's public directory includes arbitrary user bots and agents.
                    // This maintained preset offers only Anthropic's own models.
                    if kind == ModelCatalogKind::Poe
                        && item["owned_by"].as_str() != Some("Anthropic")
                    {
                        continue;
                    }
                    push_model_observation(&mut out, item, None, source, false);
                    if matches!(
                        kind,
                        ModelCatalogKind::CommandCode
                            | ModelCatalogKind::Abacus
                            | ModelCatalogKind::Poe
                    ) {
                        if let Some(model) = out
                            .last_mut()
                            .filter(|model| item["id"].as_str() == Some(model.id.as_str()))
                        {
                            model.supported_endpoints = item
                                .get("supported_endpoints")
                                .and_then(serde_json::Value::as_array)
                                .filter(|items| items.iter().all(serde_json::Value::is_string))
                                .map(|items| {
                                    items
                                        .iter()
                                        .filter_map(serde_json::Value::as_str)
                                        .map(|endpoint| endpoint.trim().to_string())
                                        .collect()
                                });
                            if kind == ModelCatalogKind::Abacus {
                                if let Some(formats) = item["api_formats"]
                                    .as_array()
                                    .filter(|items| items.iter().all(serde_json::Value::is_string))
                                {
                                    model.supported_endpoints = Some(
                                        formats
                                            .iter()
                                            .filter_map(|format| match format.as_str()? {
                                                "openai" => {
                                                    Some("/v1/chat/completions".to_string())
                                                }
                                                "responses" => Some("/v1/responses".to_string()),
                                                "anthropic" => Some("/v1/messages".to_string()),
                                                _ => None,
                                            })
                                            .collect(),
                                    );
                                }
                                if let Some(tools) = item["tools"].as_bool() {
                                    model.supported_parameters = Some(if tools {
                                        vec!["tools".into(), "tool_choice".into()]
                                    } else {
                                        Vec::new()
                                    });
                                }
                            }
                            if kind == ModelCatalogKind::Poe {
                                model.context_tokens = positive_token_limit(
                                    &item["context_window"],
                                    &["context_length"],
                                );
                                model.output_tokens = positive_token_limit(
                                    &item["context_window"],
                                    &["max_output_tokens"],
                                );
                                model.supported_parameters = item["supported_features"]
                                    .as_array()
                                    .filter(|items| items.iter().all(serde_json::Value::is_string))
                                    .map(|features| {
                                        if features.iter().any(|feature| feature == "tools") {
                                            vec!["tools".into(), "tool_choice".into()]
                                        } else {
                                            Vec::new()
                                        }
                                    });
                            }
                            if matches!(kind, ModelCatalogKind::Poe | ModelCatalogKind::Abacus) {
                                // Do not infer USD/token from an unverified provider price unit.
                                model.probe_cost_nanos = None;
                            }
                        }
                    }
                }
            }
        }
        ModelCatalogKind::Copilot => {
            for item in value["data"].as_array().into_iter().flatten() {
                if item.get("custom_model").is_some_and(|custom| {
                    !custom.is_null() && custom != &serde_json::Value::Bool(false)
                }) || item
                    .pointer("/policy/state")
                    .and_then(serde_json::Value::as_str)
                    == Some("disabled")
                    || item
                        .pointer("/capabilities/type")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|kind| kind != "chat")
                {
                    continue;
                }
                let before = out.len();
                push_model_observation(&mut out, item, None, source, false);
                if out.len() == before {
                    continue;
                }
                let model = out.last_mut().expect("model was appended");
                // Copilot publishes AI credits, not a USD-per-token tariff.
                model.probe_cost_nanos = None;
                model.supported_endpoints = item["supported_endpoints"]
                    .as_array()
                    .filter(|items| items.iter().all(serde_json::Value::is_string))
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .map(str::to_string)
                            .collect()
                    });
                let limits = &item["capabilities"]["limits"];
                model.context_tokens = positive_token_limit(
                    limits,
                    &["max_context_window_tokens", "max_prompt_tokens"],
                );
                model.output_tokens = positive_token_limit(limits, &["max_output_tokens"]);
                model.input_modalities = vec!["text".into()];
                model.output_modalities = vec!["text".into()];
                let supports = &item["capabilities"]["supports"];
                if supports["vision"].as_bool() == Some(true) {
                    model.input_modalities.push("image".into());
                }
                if supports["tool_calls"].is_boolean() {
                    let mut parameters = Vec::new();
                    if supports["tool_calls"].as_bool() == Some(true) {
                        parameters.push("tools".into());
                    }
                    if supports["parallel_tool_calls"].as_bool() == Some(true) {
                        parameters.push("parallel_tool_calls".into());
                    }
                    model.supported_parameters = Some(parameters);
                }
            }
        }
        ModelCatalogKind::Grok => {
            let entries = value
                .get("data")
                .and_then(serde_json::Value::as_array)
                .or_else(|| value.get("models").and_then(serde_json::Value::as_array))
                .or_else(|| value.as_array());
            if let Some(entries) = entries {
                for entry in entries {
                    push_grok_model_observation(&mut out, entry, source);
                }
            }
        }
        ModelCatalogKind::Codex => {
            let entries = value
                .get("data")
                .and_then(|value| value.as_array())
                .or_else(|| value.get("models").and_then(|value| value.as_array()))
                .or_else(|| value.get("items").and_then(|value| value.as_array()))
                .or_else(|| value.as_array());
            if let Some(entries) = entries {
                for entry in entries {
                    push_model_observation(&mut out, entry, None, source, true);
                }
            }
            if let Some(model_map) = value.get("models").and_then(|value| value.as_object()) {
                for (key, entry) in model_map {
                    push_model_observation(&mut out, entry, Some(key), source, true);
                }
            }
        }
        ModelCatalogKind::Gemini => {
            if let Some(items) = value.get("models").and_then(|value| value.as_array()) {
                for item in items {
                    if !gemini_model_supports_generate_content(item) {
                        continue;
                    }
                    let fallback = item
                        .get("name")
                        .and_then(|value| value.as_str())
                        .map(|value| value.trim_start_matches("models/"));
                    push_model_observation(&mut out, item, fallback, source, false);
                }
            }
        }
        ModelCatalogKind::Ollama => {
            if let Some(items) = value.get("models").and_then(|value| value.as_array()) {
                for item in items {
                    let fallback = item
                        .get("name")
                        .and_then(|value| value.as_str())
                        .or_else(|| item.get("model").and_then(|value| value.as_str()));
                    push_model_observation(&mut out, item, fallback, source, false);
                }
            }
        }
    }
    stable_dedupe_models(out)
}

fn gemini_model_supports_generate_content(model: &serde_json::Value) -> bool {
    let Some(methods) = model
        .get("supportedGenerationMethods")
        .or_else(|| model.get("supported_generation_methods"))
        .and_then(|value| value.as_array())
    else {
        return true;
    };
    methods.iter().any(|method| {
        method
            .as_str()
            .is_some_and(|method| method.eq_ignore_ascii_case("generateContent"))
    })
}

fn push_model_observation(
    out: &mut Vec<ModelObservation>,
    entry: &serde_json::Value,
    fallback_id: Option<&str>,
    source: &str,
    read_priority: bool,
) {
    let id = entry
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let object = entry.as_object()?;
            ["slug", "id", "model", "name"]
                .iter()
                .filter_map(|key| object.get(*key))
                .filter_map(|value| value.as_str())
                .map(str::trim)
                .find(|value| !value.is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            fallback_id
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        });
    let Some(id) = id else {
        return;
    };
    push_model_observation_with_id(out, entry, id, source, read_priority);
}

fn push_grok_model_observation(
    out: &mut Vec<ModelObservation>,
    entry: &serde_json::Value,
    source: &str,
) {
    let id = entry
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let object = entry.as_object()?;
            ["model", "modelId", "model_id", "id"]
                .iter()
                .filter_map(|key| object.get(*key))
                .filter_map(serde_json::Value::as_str)
                .map(str::trim)
                .find(|value| !value.is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            let metadata = entry.get("_meta")?.as_object()?;
            ["model", "modelId", "model_id", "id", "name"]
                .iter()
                .filter_map(|key| metadata.get(*key))
                .filter_map(serde_json::Value::as_str)
                .map(str::trim)
                .find(|value| !value.is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            entry
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        });
    let Some(id) = id else {
        return;
    };
    push_model_observation_with_id(out, entry, id, source, false);
}

fn push_model_observation_with_id(
    out: &mut Vec<ModelObservation>,
    entry: &serde_json::Value,
    mut id: String,
    source: &str,
    read_priority: bool,
) {
    if matches!(id.strip_prefix("models/"), Some(_)) {
        id = id.trim_start_matches("models/").to_string();
    }
    let priority = read_priority
        .then(|| entry.get("priority").and_then(json_i64))
        .flatten();
    let input_modalities = json_string_array(
        entry
            .get("input_modalities")
            .or_else(|| entry.get("inputModalities"))
            .or_else(|| entry.get("architecture")?.get("input_modalities")),
    );
    let output_modalities = json_string_array(
        entry
            .get("output_modalities")
            .or_else(|| entry.get("outputModalities"))
            .or_else(|| entry.get("architecture")?.get("output_modalities")),
    );
    out.push(ModelObservation {
        id,
        priority,
        source: source.to_string(),
        etag: None,
        use_responses_lite: entry
            .get("use_responses_lite")
            .and_then(serde_json::Value::as_bool),
        input_modalities,
        output_modalities,
        context_tokens: conservative_limit(
            positive_token_limit(
                entry,
                &[
                    "context_length",
                    "context_window",
                    "contextWindow",
                    "max_input_tokens",
                    "inputTokenLimit",
                ],
            ),
            entry
                .get("top_provider")
                .and_then(|provider| positive_token_limit(provider, &["context_length"])),
        ),
        output_tokens: conservative_limit(
            positive_token_limit(
                entry,
                &[
                    "max_output_tokens",
                    "maxOutputTokens",
                    "outputTokenLimit",
                    "max_completion_tokens",
                ],
            ),
            entry
                .get("top_provider")
                .and_then(|provider| positive_token_limit(provider, &["max_completion_tokens"])),
        ),
        supported_parameters: entry
            .get("supported_parameters")
            .and_then(serde_json::Value::as_array)
            .filter(|items| items.iter().all(serde_json::Value::is_string))
            .map(|items| {
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(|s| s.trim().to_ascii_lowercase())
                    .collect()
            }),
        supported_endpoints: None,
        kiro_request_schema: None,
        probe_cost_nanos: catalog_probe_cost(entry),
    });
}

fn catalog_probe_cost(entry: &serde_json::Value) -> Option<u64> {
    let price = |key: &str| {
        let value = entry.get("pricing")?.get(key)?;
        let number = value
            .as_f64()
            .or_else(|| value.as_str()?.parse::<f64>().ok())?;
        (number.is_finite() && number >= 0.0).then_some(number)
    };
    let cost = (price("prompt")? * 32.0 + price("completion")? * 16.0) * 1_000_000_000.0;
    (cost.is_finite() && cost <= u64::MAX as f64).then(|| cost.ceil() as u64)
}

pub(crate) fn conservative_limit(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left.filter(|v| *v > 0), right.filter(|v| *v > 0)) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

fn positive_token_limit(value: &serde_json::Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .filter_map(|key| value.get(key))
        .filter_map(json_i64)
        .find(|tokens| *tokens > 0)
        .map(|tokens| tokens as u64)
}

fn json_string_array(value: Option<&serde_json::Value>) -> Vec<String> {
    let mut values = Vec::new();
    for value in value
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
    {
        let value = value.trim().to_ascii_lowercase();
        if !value.is_empty() && !values.contains(&value) {
            values.push(value);
        }
    }
    values
}

fn merge_unique_strings(target: &mut Vec<String>, values: Vec<String>) {
    for value in values {
        if !target.iter().any(|existing| existing == &value) {
            target.push(value);
        }
    }
}

fn json_i64(value: &serde_json::Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_str().and_then(|value| value.trim().parse().ok()))
}

fn json_f64(value: &serde_json::Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse::<f64>().ok())
}

pub(crate) async fn fetch_model_catalog(
    client: &Client,
    initial_url: Url,
    headers: HeaderMap,
    kind: ModelCatalogKind,
) -> Result<Vec<ModelObservation>> {
    let source = initial_url.to_string();
    let mut url = initial_url;
    let mut all = Vec::new();
    let mut seen_cursors = HashSet::new();
    for page_index in 0..100 {
        let request = client.get(url.clone()).headers(headers.clone());
        // Codex's account-scoped client also serves long model streams. Keep
        // catalog reads at the previous short-client deadline and preserve a
        // reqwest timeout error so cached-catalog recovery can classify it.
        let request = if matches!(kind, ModelCatalogKind::Codex) {
            request.timeout(std::time::Duration::from_secs(30))
        } else {
            request
        };
        let response = request.send_adaptive().await?;
        let status = response.status();
        let status_error = response.error_for_status_ref().err();
        let mut response_url = response.url().clone();
        response_url.set_query(None);
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("unknown")
            .to_string();
        let etag = response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body = response.bytes().await.map_err(|error| {
            anyhow!("failed to read model catalog response from {response_url}: {error}")
        })?;
        if let Some(status_error) = status_error {
            let detail = serde_json::from_slice::<serde_json::Value>(&body)
                .map(|value| value.to_string())
                .unwrap_or_else(|error| {
                    format!(
                        "non-JSON body ({content_type}): {error}; body: {}",
                        response_body_excerpt(&body)
                    )
                });
            return Err(status_error).with_context(|| {
                format!("model catalog {response_url} returned {status}: {detail}")
            });
        }
        let value = serde_json::from_slice::<serde_json::Value>(&body).map_err(|error| {
            anyhow!(
                "model catalog {response_url} returned {status} with non-JSON body ({content_type}): {error}; body: {}",
                response_body_excerpt(&body)
            )
        })?;
        let mut page = model_observations_from_value(kind, &value, &source);
        if let Some(etag) = etag.as_ref() {
            for observation in &mut page {
                observation.etag = Some(etag.clone());
            }
        }
        all.extend(page);
        let Some((query_key, cursor)) = next_page_cursor(kind, &value)? else {
            break;
        };
        if page_index == 99 {
            return Err(anyhow!("model catalog pagination exceeded 100 pages"));
        }
        if !seen_cursors.insert(format!("{query_key}:{cursor}")) {
            return Err(anyhow!("model catalog pagination cursor repeated"));
        }
        set_query_param(&mut url, query_key, &cursor);
    }
    let models = stable_dedupe_models(all);
    if models.is_empty() {
        return Err(anyhow!("model catalog returned no models"));
    }
    Ok(models)
}

fn response_body_excerpt(body: &[u8]) -> String {
    const MAX_BYTES: usize = 512;
    let end = body.len().min(MAX_BYTES);
    let text = String::from_utf8_lossy(&body[..end]);
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if body.len() > MAX_BYTES {
        format!("{compact}...")
    } else if compact.is_empty() {
        "<empty>".to_string()
    } else {
        compact
    }
}

fn set_query_param(url: &mut Url, key: &str, value: &str) {
    let existing = url
        .query_pairs()
        .filter(|(candidate, _)| candidate != key)
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    url.set_query(None);
    let mut query = url.query_pairs_mut();
    for (key, value) in existing {
        query.append_pair(&key, &value);
    }
    query.append_pair(key, value);
}

fn next_page_cursor(
    kind: ModelCatalogKind,
    value: &serde_json::Value,
) -> Result<Option<(&'static str, String)>> {
    let cursor = match kind {
        ModelCatalogKind::Gemini => value
            .get("nextPageToken")
            .or_else(|| value.get("next_page_token"))
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| ("pageToken", value.to_string())),
        ModelCatalogKind::Anthropic => {
            let has_more = value
                .get("has_more")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            if !has_more {
                None
            } else {
                Some((
                    "after_id",
                    value
                        .get("last_id")
                        .and_then(|value| value.as_str())
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .ok_or_else(|| anyhow!("Anthropic model catalog omitted last_id"))?
                        .to_string(),
                ))
            }
        }
        ModelCatalogKind::OpenAI | ModelCatalogKind::CommandCode | ModelCatalogKind::Abacus => {
            let has_more = value
                .get("has_more")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            if !has_more {
                None
            } else {
                Some((
                    "after",
                    value
                        .get("last_id")
                        .and_then(|value| value.as_str())
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .ok_or_else(|| anyhow!("OpenAI model catalog omitted last_id"))?
                        .to_string(),
                ))
            }
        }
        ModelCatalogKind::Codex
        | ModelCatalogKind::Grok
        | ModelCatalogKind::Ollama
        | ModelCatalogKind::Copilot
        | ModelCatalogKind::Poe => None,
    };
    Ok(cursor)
}

#[cfg(test)]
pub(crate) fn models_from_quota_observations<'a>(
    models: impl IntoIterator<Item = &'a str>,
    source: &str,
) -> Vec<String> {
    model_ids(
        models
            .into_iter()
            .map(|model| ModelObservation {
                context_tokens: None,
                output_tokens: None,
                supported_parameters: None,
                supported_endpoints: None,
                kiro_request_schema: None,
                probe_cost_nanos: None,
                id: model.to_string(),
                priority: None,
                source: source.to_string(),
                etag: None,
                use_responses_lite: None,
                input_modalities: Vec::new(),
                output_modalities: Vec::new(),
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use warp::Filter;

    #[test]
    fn opencode_console_costs_are_advisory_and_use_workspace_model_ids() {
        let mut models = vec![
            ModelObservation {
                id: "deepseek-v4.1-flash-free".into(),
                ..Default::default()
            },
            ModelObservation {
                id: "big-pickle".into(),
                ..Default::default()
            },
            ModelObservation {
                id: "alias-model".into(),
                ..Default::default()
            },
            ModelObservation {
                id: "ignored-provider-model".into(),
                ..Default::default()
            },
        ];
        apply_opencode_console_costs(
            &mut models,
            &serde_json::json!({
                "providers": {
                    "opencode": {
                        "models": {
                            "DeepSeek-V4.1-Flash-Free": {
                                "cost": [
                                    {"input": 0.5, "output": 1.25},
                                    {"tier": {"type": "context", "size": 200000}, "input": 1.0, "output": 2.0}
                                ]
                            },
                            "big-pickle": {"cost": {"input": 0, "output": 0}},
                            "alias-key": {"modelID": "alias-model", "cost": {"input": 0.25, "output": 0.5}},
                            "invalid": {"cost": {"input": -1, "output": 1}}
                        }
                    },
                    "anthropic": {
                        "models": {
                            "ignored-provider-model": {"cost": {"input": 0, "output": 0}}
                        }
                    }
                }
            }),
        );

        assert_eq!(models[0].probe_cost_nanos, Some(36_000));
        assert_eq!(models[1].probe_cost_nanos, Some(0));
        assert_eq!(models[2].probe_cost_nanos, Some(16_000));
        assert_eq!(models[3].probe_cost_nanos, None);
    }

    #[test]
    fn catalog_limits_are_parsed_without_inference_and_duplicates_use_lower_claim() {
        let values = model_observations_from_value(
            ModelCatalogKind::OpenAI,
            &serde_json::json!({"data": [
                {"id":"vendor/model", "context_length":1048576, "top_provider":{"context_length":1000000,"max_completion_tokens":128000},"architecture":{"input_modalities":["text","image"]}},
                {"id":"vendor/MODEL", "context_window":800000,"max_output_tokens":64000},
                {"id":"no-limit", "context_length":0,"max_output_tokens":-1},
                {"id":"bad-limit", "context_length":"invalid"}
            ]}),
            "test",
        );
        assert_eq!(values.len(), 3);
        assert_eq!(values[0].context_tokens, Some(800000));
        assert_eq!(values[0].output_tokens, Some(64000));
        assert!(values[0].input_modalities.contains(&"image".to_string()));
        assert_eq!(values[1].context_tokens, None);
        assert_eq!(values[1].output_tokens, None);
        assert_eq!(values[2].context_tokens, None);
        for (kind, json) in [
            (
                ModelCatalogKind::Gemini,
                serde_json::json!({"models":[{"name":"models/gemini", "inputTokenLimit":1048576, "outputTokenLimit":65536}]}),
            ),
            (
                ModelCatalogKind::Anthropic,
                serde_json::json!({"data":[{"id":"claude", "max_input_tokens":1000000,"max_tokens":9999,"max_output_tokens":128000}]}),
            ),
            (
                ModelCatalogKind::Codex,
                serde_json::json!({"models":[{"slug":"codex","context_window":372000,"max_output_tokens":128000}]}),
            ),
            (
                ModelCatalogKind::Grok,
                serde_json::json!({"models":[{"model":"grok","contextWindow":"256000","maxOutputTokens":32000}]}),
            ),
        ] {
            let models = model_observations_from_value(kind, &json, "test");
            assert!(models[0].context_tokens.is_some());
            assert!(models[0].output_tokens.is_some());
        }
    }

    #[test]
    fn model_catalog_openai_preserves_upstream_order_and_stably_dedupes() {
        let value = serde_json::json!({
            "data": [{"id": "z-model"}, {"id": "a-model"}, {"id": "z-model"}]
        });
        assert_eq!(
            model_ids(model_observations_from_value(
                ModelCatalogKind::OpenAI,
                &value,
                "openai"
            )),
            vec!["z-model", "a-model"]
        );
    }

    #[test]
    fn abacus_public_catalog_keeps_text_models_and_explicit_api_formats() {
        let models = model_observations_from_value(
            ModelCatalogKind::Abacus,
            &serde_json::json!({"data":[
                {"id":"claude-sonnet-5-5","model_type":"text_generation","context_length":1000000,
                 "max_completion_tokens":128000,"api_formats":["openai","anthropic"],"tools":true,
                 "input_token_rate":"0.000002","output_token_rate":"0.00001"},
                {"id":"gpt-future","model_type":"text_generation","api_formats":["openai"],"tools":false},
                {"id":"route-llm-code","model_type":"text_generation","api_formats":["openai","anthropic"]},
                {"id":"media","model_type":"image_generation"}
            ]}),
            "abacus",
        );
        assert_eq!(models.len(), 3);
        assert_eq!(models[0].context_tokens, Some(1000000));
        assert_eq!(models[0].output_tokens, Some(128000));
        assert_eq!(
            models[0].supported_parameters.as_deref(),
            Some(["tools".to_string(), "tool_choice".to_string()].as_slice())
        );
        assert!(models[0].probe_cost_nanos.is_none());
        assert_eq!(
            crate::coding_gateway::catalog_supports_protocol(&models[1], "openai_responses"),
            Some(false)
        );
        assert_eq!(
            models[2].id, "route-llm-code",
            "an automatic router is not a Claude identity"
        );
        assert_eq!(
            crate::coding_gateway::catalog_supports_protocol(&models[2], "anthropic_messages"),
            Some(true)
        );
    }

    #[test]
    fn openrouter_provider_limits_never_raise_the_catalog_limits() {
        for (catalog, provider, expected) in [
            (128000, 16000, 16000),
            (16000, 128000, 16000),
            (0, 32000, 32000),
            (32000, 0, 32000),
        ] {
            let models = model_observations_from_value(
                ModelCatalogKind::OpenAI,
                &serde_json::json!({"data":[{
                    "id":"vendor/model", "context_length":catalog, "max_output_tokens":catalog,
                    "top_provider":{"context_length":provider,"max_completion_tokens":provider}
                }]}),
                "openrouter",
            );
            assert_eq!(
                (models[0].context_tokens, models[0].output_tokens),
                (Some(expected), Some(expected))
            );
        }
    }

    #[test]
    fn invalid_catalog_limits_stay_unknown_without_losing_the_callable_model() {
        for value in [
            serde_json::Value::Null,
            serde_json::json!(0),
            serde_json::json!(-1),
            serde_json::json!(true),
            serde_json::json!(1.5),
            serde_json::json!(u64::MAX),
            serde_json::json!("9223372036854775808"),
            serde_json::json!("1m"),
            serde_json::json!({}),
            serde_json::json!([]),
        ] {
            let models = model_observations_from_value(
                ModelCatalogKind::OpenAI,
                &serde_json::json!({"data":[{
                    "id":"Vendor/Branch/Model:free", "context_length":value, "max_output_tokens":value,
                    "top_provider":{"context_length":value,"max_completion_tokens":value},
                    "architecture":{"input_modalities":[null,42," TEXT ","Image","image"],"output_modalities":42}
                }]}),
                "openrouter",
            );
            assert_eq!(
                models.len(),
                1,
                "invalid metadata must not remove a callable model"
            );
            assert_eq!(models[0].id, "Vendor/Branch/Model:free");
            assert_eq!(
                (models[0].context_tokens, models[0].output_tokens),
                (None, None),
                "{value}"
            );
            assert_eq!(models[0].input_modalities, ["text", "image"]);
            assert!(models[0].output_modalities.is_empty());
        }
    }

    #[test]
    fn model_catalog_grok_prefers_callable_route_fields_over_display_fields() {
        let value = serde_json::json!({
            "data": [
                {"id": "display-id", "model": "grok-4.5"},
                {"modelId": "grok-build-0.1"},
                {"model_id": "grok-composer-2.5-fast"},
                {"name": "Grok Meta Display Name", "_meta": {"model": "grok-meta"}},
                {"name": "grok-name"},
                {"id": "grok-safe", "_meta": "not-an-object"}
            ]
        });

        assert_eq!(
            model_ids(model_observations_from_value(
                ModelCatalogKind::Grok,
                &value,
                "grok"
            )),
            [
                "grok-4.5",
                "grok-build-0.1",
                "grok-composer-2.5-fast",
                "grok-meta",
                "grok-name",
                "grok-safe"
            ]
        );
    }

    #[test]
    fn model_catalog_dedupes_case_insensitively_and_preserves_first_spelling() {
        let observations = vec![
            ModelObservation {
                context_tokens: None,
                output_tokens: None,
                supported_parameters: None,
                supported_endpoints: None,
                kiro_request_schema: None,
                probe_cost_nanos: None,
                id: " GPT-5.6-Terra ".to_string(),
                priority: None,
                source: "first".to_string(),
                etag: None,
                use_responses_lite: None,
                input_modalities: vec!["text".to_string()],
                output_modalities: Vec::new(),
            },
            ModelObservation {
                context_tokens: None,
                output_tokens: None,
                supported_parameters: None,
                supported_endpoints: None,
                kiro_request_schema: None,
                probe_cost_nanos: None,
                id: "gpt-5.6-terra".to_string(),
                priority: Some(0),
                source: "second".to_string(),
                etag: None,
                use_responses_lite: Some(true),
                input_modalities: vec!["audio".to_string()],
                output_modalities: vec!["text".to_string()],
            },
        ];

        let models = stable_dedupe_models(observations);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "GPT-5.6-Terra");
        assert_eq!(models[0].priority, Some(0));
        assert_eq!(models[0].source, "first");
        assert_eq!(models[0].use_responses_lite, Some(true));
        assert_eq!(models[0].input_modalities, ["text", "audio"]);
        assert_eq!(models[0].output_modalities, ["text"]);
    }

    #[test]
    fn model_catalog_codex_uses_explicit_priority() {
        let value = serde_json::json!({
            "models": [
                {
                    "slug": "later",
                    "priority": 20,
                    "use_responses_lite": false,
                    "input_modalities": ["text", "image"]
                },
                {
                    "slug": "first",
                    "priority": 0,
                    "use_responses_lite": true,
                    "input_modalities": ["text", "audio"],
                    "output_modalities": ["text"]
                },
                {"slug": "middle", "priority": 10},
                {"slug": "first", "priority": 99}
            ]
        });
        let observations = model_observations_from_value(ModelCatalogKind::Codex, &value, "codex");
        assert_eq!(
            observations
                .iter()
                .map(|observation| observation.id.as_str())
                .collect::<Vec<_>>(),
            ["first", "middle", "later"]
        );
        assert_eq!(observations[0].use_responses_lite, Some(true));
        assert_eq!(observations[2].use_responses_lite, Some(false));
        assert_eq!(observations[0].input_modalities, ["text", "audio"]);
        assert_eq!(observations[0].output_modalities, ["text"]);
    }

    #[test]
    fn model_catalog_parses_anthropic_and_gemini_pagination() {
        let anthropic = serde_json::json!({"has_more": true, "last_id": "claude-b"});
        assert_eq!(
            next_page_cursor(ModelCatalogKind::Anthropic, &anthropic).expect("cursor"),
            Some(("after_id", "claude-b".to_string()))
        );
        let gemini = serde_json::json!({"nextPageToken": "page-2"});
        assert_eq!(
            next_page_cursor(ModelCatalogKind::Gemini, &gemini).expect("cursor"),
            Some(("pageToken", "page-2".to_string()))
        );
    }

    #[test]
    fn model_catalog_later_explicit_priority_reorders_first_observation() {
        let observations = vec![
            ModelObservation {
                context_tokens: None,
                output_tokens: None,
                supported_parameters: None,
                supported_endpoints: None,
                kiro_request_schema: None,
                probe_cost_nanos: None,
                id: "duplicate".to_string(),
                priority: None,
                source: "first".to_string(),
                etag: None,
                use_responses_lite: None,
                input_modalities: Vec::new(),
                output_modalities: Vec::new(),
            },
            ModelObservation {
                context_tokens: None,
                output_tokens: None,
                supported_parameters: None,
                supported_endpoints: None,
                kiro_request_schema: None,
                probe_cost_nanos: None,
                id: "other".to_string(),
                priority: Some(10),
                source: "second".to_string(),
                etag: None,
                use_responses_lite: None,
                input_modalities: Vec::new(),
                output_modalities: Vec::new(),
            },
            ModelObservation {
                context_tokens: None,
                output_tokens: None,
                supported_parameters: None,
                supported_endpoints: None,
                kiro_request_schema: None,
                probe_cost_nanos: None,
                id: "duplicate".to_string(),
                priority: Some(0),
                source: "third".to_string(),
                etag: None,
                use_responses_lite: None,
                input_modalities: Vec::new(),
                output_modalities: Vec::new(),
            },
        ];
        assert_eq!(model_ids(observations), vec!["duplicate", "other"]);
    }

    #[test]
    fn model_catalog_rejects_missing_anthropic_cursor() {
        let value = serde_json::json!({"has_more": true, "data": [{"id": "claude-a"}]});
        assert!(next_page_cursor(ModelCatalogKind::Anthropic, &value).is_err());
    }

    #[test]
    fn model_catalog_extracts_stable_quota_model_observations() {
        assert_eq!(
            models_from_quota_observations(
                ["gemini-3-pro", "gemini-3-flash", "gemini-3-pro"],
                "quota"
            ),
            vec!["gemini-3-pro", "gemini-3-flash"]
        );
    }

    #[tokio::test]
    async fn model_catalog_fetches_all_anthropic_pages_in_order() {
        let route = warp::path("models")
            .and(warp::query::<HashMap<String, String>>())
            .map(|query: HashMap<String, String>| {
                if query.get("after_id").map(String::as_str) == Some("claude-a") {
                    warp::reply::json(&serde_json::json!({
                        "data": [{"id": "claude-c"}],
                        "has_more": false
                    }))
                } else {
                    warp::reply::json(&serde_json::json!({
                        "data": [{"id": "claude-b"}, {"id": "claude-a"}],
                        "has_more": true,
                        "last_id": "claude-a"
                    }))
                }
            });
        let (address, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
        tokio::spawn(server);

        let models = fetch_model_catalog(
            &Client::new(),
            Url::parse(&format!("http://{address}/models")).expect("url"),
            HeaderMap::new(),
            ModelCatalogKind::Anthropic,
        )
        .await
        .expect("catalog");
        assert_eq!(model_ids(models), vec!["claude-b", "claude-a", "claude-c"]);
    }

    #[tokio::test]
    async fn model_catalog_fetches_all_gemini_pages_in_order() {
        let route = warp::path("models")
            .and(warp::query::<HashMap<String, String>>())
            .map(|query: HashMap<String, String>| {
                if query.get("pageToken").map(String::as_str) == Some("next") {
                    warp::reply::json(&serde_json::json!({
                        "models": [{"name": "models/gemini-second"}]
                    }))
                } else {
                    warp::reply::json(&serde_json::json!({
                        "models": [{"name": "models/gemini-first"}],
                        "nextPageToken": "next"
                    }))
                }
            });
        let (address, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
        tokio::spawn(server);

        let models = fetch_model_catalog(
            &Client::new(),
            Url::parse(&format!("http://{address}/models?key=preserved")).expect("url"),
            HeaderMap::new(),
            ModelCatalogKind::Gemini,
        )
        .await
        .expect("catalog");
        assert_eq!(model_ids(models), vec!["gemini-first", "gemini-second"]);
    }

    #[tokio::test]
    async fn model_catalog_reports_non_json_response_details() {
        let route = warp::path("models").map(|| {
            warp::reply::with_header(
                "model service is starting",
                "content-type",
                "text/plain; charset=utf-8",
            )
        });
        let (address, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
        tokio::spawn(server);
        let url = Url::parse(&format!("http://{address}/models")).expect("url");

        let error = fetch_model_catalog(
            &Client::new(),
            url.clone(),
            HeaderMap::new(),
            ModelCatalogKind::OpenAI,
        )
        .await
        .expect_err("non-JSON catalog must fail")
        .to_string();

        assert!(error.contains(url.as_str()), "{error}");
        assert!(error.contains("200 OK"), "{error}");
        assert!(error.contains("text/plain"), "{error}");
        assert!(error.contains("model service is starting"), "{error}");
    }

    #[tokio::test]
    async fn model_catalog_keeps_http_status_in_error_chain() {
        let route = warp::path("models").map(|| {
            warp::reply::with_status(
                warp::reply::json(&serde_json::json!({"error": "temporarily unavailable"})),
                warp::http::StatusCode::SERVICE_UNAVAILABLE,
            )
        });
        let (address, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
        tokio::spawn(server);

        let error = fetch_model_catalog(
            &Client::new(),
            Url::parse(&format!("http://{address}/models")).expect("url"),
            HeaderMap::new(),
            ModelCatalogKind::OpenAI,
        )
        .await
        .expect_err("503 catalog must fail");

        assert!(error.to_string().contains("temporarily unavailable"));
        assert!(error.chain().any(|cause| {
            cause
                .downcast_ref::<reqwest::Error>()
                .and_then(reqwest::Error::status)
                == Some(reqwest::StatusCode::SERVICE_UNAVAILABLE)
        }));
    }

    #[tokio::test]
    async fn model_catalog_keeps_http_status_for_non_json_error_body() {
        let route = warp::path("models").map(|| {
            warp::reply::with_status(
                warp::reply::with_header("temporarily unavailable", "content-type", "text/plain"),
                warp::http::StatusCode::SERVICE_UNAVAILABLE,
            )
        });
        let (address, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
        tokio::spawn(server);

        let error = fetch_model_catalog(
            &Client::new(),
            Url::parse(&format!("http://{address}/models")).expect("url"),
            HeaderMap::new(),
            ModelCatalogKind::OpenAI,
        )
        .await
        .expect_err("503 catalog must fail");

        assert!(error.to_string().contains("temporarily unavailable"));
        assert!(error.chain().any(|cause| {
            cause
                .downcast_ref::<reqwest::Error>()
                .and_then(reqwest::Error::status)
                == Some(reqwest::StatusCode::SERVICE_UNAVAILABLE)
        }));
    }

    #[test]
    fn gemini_catalog_keeps_only_declared_generate_content_models() {
        let models = model_observations_from_value(
            ModelCatalogKind::Gemini,
            &serde_json::json!({
                "models": [
                    {
                        "name": "models/text-embedding",
                        "supportedGenerationMethods": ["embedContent"]
                    },
                    {
                        "name": "models/gemini-chat",
                        "supportedGenerationMethods": ["generateContent", "countTokens"]
                    },
                    {"name": "models/future-model"}
                ]
            }),
            "gemini_models",
        );

        assert_eq!(model_ids(models), vec!["gemini-chat", "future-model"]);
    }
}
