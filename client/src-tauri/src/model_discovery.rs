//! Model directory presentation only: never used to select or authorize routes.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{collections::HashSet, sync::OnceLock};

pub(crate) const CATALOG_HEADER: &str = "x-const-model-catalog";
pub(crate) const POLICY_FIELD: &str = "const_api_model_presentation";

#[tauri::command]
pub(crate) async fn get_model_presentation(
    state: tauri::State<'_, crate::model::AppState>,
) -> Result<ModelPresentation, String> {
    let config =
        crate::load_config_from_path(&state.config_path).map_err(|error| error.to_string())?;
    fetch_model_presentation(&config).await
}

async fn fetch_model_presentation(
    config: &crate::model::ClientConfig,
) -> Result<ModelPresentation, String> {
    // Reuse the local proxy's compact catalog cache and offline fallback;
    // never contact a supplier or run a model probe for display rules.
    let urls = crate::tool_config_urls(config)?;
    let payload = crate::short_http_client()
        .get(format!("{}/models", urls.openai))
        .header("Authorization", crate::local_proxy_auth_header(config))
        .header(CATALOG_HEADER, "compact-v1")
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| error.to_string())?
        .json::<Value>()
        .await
        .map_err(|error| error.to_string())?;
    // Older servers do not send the policy, just as in tool discovery.
    Ok(ModelPresentation::from_payload(&payload)
        .unwrap_or_else(|| ModelPresentation::packaged().clone()))
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ModelPresentation {
    schema_version: u32,
    rules: Vec<ModelPresentationRule>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct ModelPresentationRule {
    pattern: String,
    // Derived from row order, but retained for older directory consumers.
    #[serde(default)]
    priority: i32,
    hidden: bool,
}

impl ModelPresentation {
    pub(crate) fn packaged() -> &'static Self {
        static DEFAULT: OnceLock<ModelPresentation> = OnceLock::new();
        DEFAULT.get_or_init(|| {
            serde_json::from_str::<Self>(include_str!(
                "../../../shared/tool_model_presentation_defaults.json"
            ))
            .expect("valid embedded model presentation")
            .normalized()
            .expect("valid embedded model presentation")
        })
    }

    pub(crate) fn from_payload(payload: &Value) -> Option<Self> {
        serde_json::from_value::<Self>(payload.get(POLICY_FIELD)?.clone())
            .ok()?
            .normalized()
    }

    fn normalized(mut self) -> Option<Self> {
        if self.schema_version != 1 || self.rules.len() > 128 {
            return None;
        }
        let mut seen = HashSet::new();
        let count = self.rules.len() as i32;
        for (index, rule) in self.rules.iter_mut().enumerate() {
            rule.pattern = rule.pattern.trim().to_lowercase();
            let prefix = rule.pattern.strip_suffix('*').unwrap_or(&rule.pattern);
            if rule.pattern.is_empty()
                || rule.pattern.len() > 128
                || prefix.contains(['*', '/', '?', '\\', ' ', '\t', '\r', '\n'])
                || !seen.insert(rule.pattern.clone())
            {
                return None;
            }
            rule.priority = count - index as i32;
        }
        Some(self)
    }

    pub(crate) fn rank(&self, model: &str) -> (i32, bool) {
        for rule in &self.rules {
            if rule.pattern == model
                || rule
                    .pattern
                    .strip_suffix('*')
                    .is_some_and(|prefix| model.starts_with(prefix))
            {
                return (rule.priority, rule.hidden);
            }
        }
        (0, false)
    }

    // Rank once per model, not once per comparator invocation. This runs after
    // short-name normalization, capability merging and alias context limits.
    pub(crate) fn apply(&self, models: &mut Vec<Value>) {
        let mut ranked = Vec::with_capacity(models.len());
        for mut model in models.drain(..) {
            let id = model
                .get("id")
                .or_else(|| model.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let id = crate::config::public_model_name(id);
            let (priority, hidden) = self.rank(&id);
            if hidden {
                continue;
            }
            if let Some(object) = model.as_object_mut() {
                let metadata = object
                    .entry("const_api")
                    .or_insert_with(|| Value::Object(Map::new()));
                if let Some(metadata) = metadata.as_object_mut() {
                    metadata.insert("display_priority".into(), Value::from(priority));
                }
            }
            ranked.push((priority, id, model));
        }
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        models.extend(ranked.into_iter().map(|(_, _, model)| model));
    }
}

/// Preserve known false versus unknown, and the legacy support-wins semantics.
/// Routing/diagnostics still own full evidence; only tool discovery is compact.
pub(crate) fn compact_entry(model: &mut Value) {
    let id = model
        .get("id")
        .or_else(|| model.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim_start_matches("models/")
        .to_string();
    let Some(meta) = model.get_mut("const_api").and_then(Value::as_object_mut) else {
        return;
    };
    let mut states = meta
        .remove("capability_states")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    for key in ["native_capabilities", "effective_capabilities"] {
        let Some(Value::Array(evidence)) = meta.remove(key) else {
            continue;
        };
        for item in evidence {
            let Some(feature) = item.get("feature").and_then(Value::as_str) else {
                continue;
            };
            let pattern = item
                .get("model_pattern")
                .and_then(Value::as_str)
                .unwrap_or("");
            if !crate::tool_model_metadata::model_pattern_matches(pattern, &id) {
                continue;
            }
            match item
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
                .as_str()
            {
                "supported" | "verified" | "declared" => {
                    states.insert(feature.into(), Value::Bool(true));
                }
                "unsupported" | "rejected" => {
                    states.entry(feature).or_insert(Value::Bool(false));
                }
                _ => {}
            }
        }
    }
    meta.remove("conversion_reachability");
    if !states.is_empty() {
        meta.insert("capability_states".into(), Value::Object(states));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn display_rules_use_authenticated_compact_local_discovery_and_legacy_defaults() {
        use warp::Filter;
        for (status, payload) in [
            (
                200,
                json!({POLICY_FIELD: {"schema_version": 1, "rules": [
                    {"pattern": " GPT-* ", "priority": 0, "hidden": true},
                    {"pattern": "claude-*", "priority": 900, "hidden": false}
                ]}}),
            ),
            (200, json!({"data": []})),
            (503, json!({"error": "catalog unavailable"})),
        ] {
            let has_policy = payload.get(POLICY_FIELD).is_some();
            let route = warp::get()
                .and(warp::path!("v1" / "models"))
                .and(warp::header::exact(
                    "authorization",
                    "Bearer test-display-key",
                ))
                .and(warp::header::exact(CATALOG_HEADER, "compact-v1"))
                .map(move || {
                    warp::reply::with_status(
                        warp::reply::json(&payload),
                        warp::http::StatusCode::from_u16(status).unwrap(),
                    )
                });
            let (address, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
            let server = tokio::spawn(server);
            let mut config = crate::config::default_config();
            config.listen = address.to_string();
            config.api_key = "test-display-key".into();
            let result = fetch_model_presentation(&config).await;
            server.abort();
            if status != 200 {
                assert!(result.unwrap_err().contains("503"));
            } else if has_policy {
                let policy = result.unwrap();
                assert_eq!(policy.rank("gpt-future"), (2, true));
                assert_eq!(policy.rank("claude-future"), (1, false));
            } else {
                assert_eq!(&result.unwrap(), ModelPresentation::packaged());
            }
        }
    }

    #[test]
    fn compact_discovery_preserves_tool_capabilities_and_limits() {
        let original = json!({"data": [{"id":"gpt-5.6-sol", "const_api": {
            "context_tokens": 1000000, "output_tokens": null, "supports_1m": false,
            "reasoning": true, "reasoning_efforts": ["low", "high"],
            "native_capabilities": [
                {"feature":"reasoning","state":"unsupported"},
                {"feature":"tool_calls","state":"rejected"},
                {"feature":"vision","state":"supported","model_pattern":"other-*"}
            ],
            "effective_capabilities": [{"feature":"tool_calls","state":"supported"}],
            "conversion_reachability":[{"source_protocol":"openai_chat","target_protocol":"openai_responses"}]
        }}]});
        let before = crate::tool_model_metadata::tool_models_from_response(&original);
        let mut compact = original.clone();
        compact_entry(&mut compact["data"][0]);
        assert_eq!(
            before,
            crate::tool_model_metadata::tool_models_from_response(&compact)
        );
        assert_eq!(
            compact["data"][0]["const_api"]["capability_states"],
            json!({"reasoning":false,"tool_calls":true})
        );
        assert!(
            compact["data"][0]["const_api"]
                .get("conversion_reachability")
                .is_none()
        );
        assert!(
            serde_json::to_vec(&compact).unwrap().len()
                < serde_json::to_vec(&original).unwrap().len() / 2
        );
        let once = compact.clone();
        compact_entry(&mut compact["data"][0]);
        assert_eq!(once, compact, "compaction must be idempotent");
    }

    #[test]
    fn model_presentation_rules_are_ordered_bounded_and_future_friendly() {
        let policy =
            ModelPresentation::from_payload(&json!({POLICY_FIELD:{"schema_version":1,"rules":[
                {"pattern":" GPT-OLD ","priority":-10,"hidden":true},
                {"pattern":"gpt-*","priority":400,"hidden":false}
            ]}}))
            .unwrap();
        let mut models = vec![
            json!({"id":"aaa"}),
            json!({"id":"gpt-old"}),
            json!({"id":"gpt-future-z"}),
            json!({"name":"models/gpt-future-a"}),
        ];
        policy.apply(&mut models);
        assert_eq!(models.len(), 3);
        assert_eq!(models[0]["name"], "models/gpt-future-a");
        assert_eq!(models[1]["id"], "gpt-future-z");
        assert_eq!(models[2]["id"], "aaa");
        assert_eq!(models[1]["const_api"]["display_priority"], 1);
        assert!(
            ModelPresentation::from_payload(&json!({POLICY_FIELD:{"schema_version":2,"rules":[]}}))
                .is_none()
        );
        for pattern in ["", "vendor/model", "gpt-*-mini", "gpt-?", "gpt model"] {
            assert!(ModelPresentation::from_payload(&json!({POLICY_FIELD:{"schema_version":1,"rules":[{"pattern":pattern,"priority":0,"hidden":false}]}})).is_none());
        }
        let empty =
            ModelPresentation::from_payload(&json!({POLICY_FIELD:{"schema_version":1,"rules":[]}}))
                .unwrap();
        empty.apply(&mut models);
        assert_eq!(
            models[0]["id"], "aaa",
            "empty policy restores alphabetic order"
        );
    }

    #[test]
    fn zero_priorities_and_moving_rows_control_discovery_and_tool_injection() {
        let mut payload = json!({POLICY_FIELD:{"schema_version":1,"rules":[
            {"pattern":"gpt-6*","priority":0,"hidden":false},
            {"pattern":"gpt-5.6*","priority":0,"hidden":false},
            {"pattern":"claude-*","priority":0,"hidden":false}
        ]}, "data":[
            {"id":"aaa"}, {"id":"gpt-5.6-sol"}, {"id":"claude-future"},
            {"id":"gpt-6-astra"}, {"id":"gpt-5.6-luna"}
        ]});
        for expected in [
            [
                "gpt-6-astra",
                "gpt-5.6-luna",
                "gpt-5.6-sol",
                "claude-future",
                "aaa",
            ],
            [
                "claude-future",
                "gpt-5.6-luna",
                "gpt-5.6-sol",
                "gpt-6-astra",
                "aaa",
            ],
        ] {
            let policy = ModelPresentation::from_payload(&payload).unwrap();
            let mut models = payload["data"].as_array().unwrap().clone();
            policy.apply(&mut models);
            assert_eq!(
                models
                    .iter()
                    .map(|m| m["id"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                expected
            );
            let tool_models = crate::tool_model_metadata::tool_models_from_response(&payload);
            assert_eq!(
                tool_models
                    .iter()
                    .map(|m| m.id.as_str())
                    .collect::<Vec<_>>(),
                expected
            );
            let wire = serde_json::to_value(&policy).unwrap();
            assert_eq!(wire["rules"][0]["priority"], 3);
            assert_eq!(wire["rules"][2]["priority"], 1);
            payload[POLICY_FIELD]["rules"]
                .as_array_mut()
                .unwrap()
                .swap(0, 2);
        }
    }

    #[test]
    fn default_discovery_keeps_gpt_inside_first_fifty_without_truncating() {
        let mut models = (0..425)
            .map(|i| json!({"id":format!("aaa-model-{i:04}")}))
            .collect::<Vec<_>>();
        models.push(json!({"id":"gpt-future"}));
        ModelPresentation::packaged().apply(&mut models);
        assert_eq!(models.len(), 426);
        assert_eq!(models[0]["id"], "gpt-future");
        assert_eq!(
            crate::tool_model_metadata::tool_models_from_response(&json!({"data":models}))[0].id,
            "gpt-future"
        );
    }
}
