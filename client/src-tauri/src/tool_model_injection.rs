//! Use-page model injection preferences, never routing or discovery permissions.
use crate::{model_discovery::ModelPresentation, tool_model_metadata::ToolModelInfo};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, LazyLock},
};

// Ordinary catalogs follow their sync policy. Share only the exceptions with
// frontend bulk sync: Claude's compatible aliases need the full snapshot, while
// Codex loads a snapshot only when its optional CONST catalog is selected.
static SELECTION_OVERRIDES: LazyLock<HashMap<&str, bool>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../tool-model-injection-overrides.json"))
        .expect("bundled tool model injection overrides must be valid")
});

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub(crate) struct ToolModelInjectionSettings {
    pub(crate) all_models: bool,
    pub(crate) extra_models: Vec<String>,
}

impl Default for ToolModelInjectionSettings {
    fn default() -> Self {
        Self {
            all_models: true,
            extra_models: Vec::new(),
        }
    }
}

impl ToolModelInjectionSettings {
    pub(crate) fn select_from_payload(
        &self,
        tool: &str,
        payload: &serde_json::Value,
    ) -> Result<Vec<ToolModelInfo>, String> {
        let models = crate::tool_model_metadata::tool_models_from_response(payload);
        if self.all_models || !applies_to(tool) {
            return Ok(models);
        }
        self.select(
            tool,
            models,
            ModelPresentation::recommendations_from_payload(payload)?.as_ref(),
        )
    }

    pub(crate) fn normalized(mut self) -> Result<Self, String> {
        let mut seen = HashSet::new();
        if self.extra_models.len() > 128 {
            return Err("TOOL_MODEL_INJECTION_INVALID: at most 128 additions".into());
        }
        for pattern in &mut self.extra_models {
            *pattern = pattern.trim().to_lowercase();
            let prefix = pattern.strip_suffix('*').unwrap_or(pattern);
            if pattern.is_empty()
                || pattern.len() > 128
                || prefix.contains(['*', '/', '?', '\\', ' ', '\t', '\r', '\n'])
                || !seen.insert(pattern.clone())
            {
                return Err("TOOL_MODEL_INJECTION_INVALID: use unique short model names or trailing-* prefixes".into());
            }
        }
        Ok(self)
    }

    pub(crate) fn select(
        &self,
        tool: &str,
        models: Vec<ToolModelInfo>,
        recommendations: Option<&ModelPresentation>,
    ) -> Result<Vec<ToolModelInfo>, String> {
        if self.all_models || !applies_to(tool) {
            return Ok(models);
        }
        let settings = self.clone().normalized()?;
        let policy = Arc::new(ModelPresentation::injection_policy(
            recommendations,
            &settings.extra_models,
        ));
        let mut selected = models
            .into_iter()
            .filter_map(|mut model| {
                let (priority, hidden) = policy.rank(&model.id);
                if hidden {
                    return None;
                }
                model.display_priority = priority;
                model.presentation = Some(policy.clone());
                Some(model)
            })
            .collect::<Vec<_>>();
        selected.sort_by(crate::tool_model_metadata::tool_model_display_order);
        if selected.is_empty() {
            // Do not fall back to the full catalog, erase working configuration,
            // or pass an empty list into adapters that require a default model.
            return Err("TOOL_MODEL_INJECTION_EMPTY".into());
        }
        Ok(selected)
    }
}

pub(crate) fn applies_to(tool: &str) -> bool {
    SELECTION_OVERRIDES.get(tool).copied().unwrap_or_else(|| {
        crate::tool_config::tool_model_sync_policy(tool)
            == crate::tool_config::ToolModelSyncPolicy::Catalog
    })
}

#[derive(Serialize)]
pub(crate) struct ToolModelRecommendations {
    patterns: Vec<String>,
    available_models: Vec<String>,
}

#[tauri::command]
pub(crate) async fn get_tool_model_recommendations(
    state: tauri::State<'_, crate::model::AppState>,
) -> Result<ToolModelRecommendations, String> {
    let config =
        crate::load_config_from_path(&state.config_path).map_err(|error| error.to_string())?;
    let urls = crate::tool_config_urls(&config)?;
    let payload = crate::short_http_client()
        .get(format!("{}/models", urls.openai))
        .header("Authorization", crate::local_proxy_auth_header(&config))
        .header(crate::model_discovery::CATALOG_HEADER, "compact-v1")
        .send()
        .await
        .inspect_err(crate::http_client::observe_error)
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| error.to_string())?
        .json::<serde_json::Value>()
        .await
        .map_err(|error| error.to_string())?;
    Ok(ToolModelRecommendations {
        patterns: ModelPresentation::recommendations_from_payload(&payload)?
            .map(|policy| policy.recommended_patterns())
            .unwrap_or_default(),
        available_models: crate::tool_model_metadata::tool_models_from_response(&payload)
            .into_iter()
            .map(|model| model.id)
            .collect(),
    })
}

#[tauri::command]
pub(crate) fn save_tool_model_injection_settings(
    state: tauri::State<'_, crate::model::AppState>,
    settings: ToolModelInjectionSettings,
) -> Result<serde_json::Value, String> {
    let settings = settings.normalized()?;
    let (config, ()) =
        crate::commit_config_update(&state.config_path, &state.proxy_config, |config| {
            config.tool_model_injection = settings;
            Ok(())
        })
        .map_err(|error| error.to_string())?;
    crate::config_for_renderer(&config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn selection_follows_catalog_sync_with_shared_exceptions() {
        for (&tool, &select) in SELECTION_OVERRIDES.iter() {
            assert_ne!(
                select,
                crate::tool_config::tool_model_sync_policy(tool)
                    == crate::tool_config::ToolModelSyncPolicy::Catalog,
                "{tool} does not need an override for the default behavior"
            );
            assert_eq!(applies_to(tool), select);
        }
        for tool in [
            "trae",
            "workbuddy",
            "workbuddy-ai",
            "opencode",
            "cline",
            "codex",
        ] {
            assert!(applies_to(tool), "{tool}");
        }
        assert_eq!(
            crate::tool_config::tool_model_sync_policy("claude-desktop"),
            crate::tool_config::ToolModelSyncPolicy::Catalog
        );
        assert!(!applies_to("claude-desktop"));
        assert!(!applies_to("future-catalog-tool"));
    }

    fn models() -> Vec<ToolModelInfo> {
        crate::tool_model_metadata::tool_models_from_ids(&[
            "extra-a".into(),
            "gpt-new".into(),
            "unknown".into(),
            "gpt-old".into(),
            "extra-z".into(),
        ])
    }

    #[test]
    fn legacy_configs_default_to_all_and_settings_survive_roundtrip() {
        let mut value = serde_json::to_value(crate::default_config()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("tool_model_injection");
        let mut config: crate::model::ClientConfig = serde_json::from_value(value).unwrap();
        assert_eq!(
            config.tool_model_injection,
            ToolModelInjectionSettings::default()
        );
        config.tool_model_injection = ToolModelInjectionSettings {
            all_models: false,
            extra_models: vec!["custom-*".into()],
        };
        let restored: crate::model::ClientConfig =
            serde_json::from_value(serde_json::to_value(&config).unwrap()).unwrap();
        assert_eq!(restored.tool_model_injection, config.tool_model_injection);
        assert_eq!(
            restored.allow_model_equivalence,
            config.allow_model_equivalence
        );
    }

    #[test]
    fn recommendations_keep_admin_precedence_then_append_in_user_order() {
        let policy = ModelPresentation::from_payload(&json!({"const_api_model_presentation": {
            "schema_version":1, "rules":[
                {"pattern":"gpt-old", "hidden":true},
                {"pattern":"gpt-*", "hidden":false}
            ]
        }}))
        .unwrap();
        let settings = ToolModelInjectionSettings {
            all_models: false,
            extra_models: vec!["extra-z".into(), "extra-a".into(), "gpt-old".into()],
        };
        for tool in [
            "trae",
            "trae-cn",
            "trae-work",
            "workbuddy",
            "workbuddy-ai",
            "opencode",
            "mimocode",
            "minimax-code",
            "kimicode",
            "zcode",
            "codex",
        ] {
            let result = settings.select(tool, models(), Some(&policy)).unwrap();
            assert_eq!(
                result
                    .iter()
                    .map(|model| model.id.as_str())
                    .collect::<Vec<_>>(),
                ["gpt-new", "extra-z", "extra-a"],
                "{tool}"
            );
            assert!(
                result[0]
                    .presentation
                    .as_ref()
                    .unwrap()
                    .rank("unknown-alias")
                    .1,
                "post-selection alias generation cannot escape the whitelist"
            );
        }
        assert_eq!(
            models().len(),
            5,
            "selection does not alter the global catalog"
        );
    }

    #[test]
    fn local_or_missing_recommendations_are_empty_not_packaged_defaults() {
        let mut payload = json!({"const_api_model_presentation": ModelPresentation::packaged()});
        payload[crate::model_discovery::POLICY_SOURCE_FIELD] = json!("fallback");
        assert!(
            ModelPresentation::recommendations_from_payload(&payload)
                .unwrap()
                .is_none()
        );
        assert!(
            ModelPresentation::recommendations_from_payload(&json!({"data":[]}))
                .unwrap()
                .is_none()
        );
        let settings = ToolModelInjectionSettings {
            all_models: false,
            extra_models: vec![],
        };
        assert_eq!(
            settings.select("workbuddy", models(), None).unwrap_err(),
            "TOOL_MODEL_INJECTION_EMPTY"
        );
        let settings = ToolModelInjectionSettings {
            extra_models: vec!["extra-*".into()],
            ..settings
        };
        let result = settings.select("workbuddy", models(), None).unwrap();
        assert_eq!(
            result
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["extra-a", "extra-z"]
        );
    }

    #[test]
    fn invalid_recommendations_stop_restricted_writes_but_not_all_mode() {
        let payload = json!({"data":[{"id":"extra-model"}], "const_api_model_presentation":{"schema_version":99,"rules":[]}});
        let restricted = ToolModelInjectionSettings {
            all_models: false,
            extra_models: vec!["extra-model".into()],
        };
        assert_eq!(
            restricted
                .select_from_payload("opencode", &payload)
                .unwrap_err(),
            "TOOL_MODEL_RECOMMENDATIONS_INVALID"
        );
        assert_eq!(
            ToolModelInjectionSettings::default()
                .select_from_payload("opencode", &payload)
                .unwrap()
                .len(),
            1
        );
        for tool in ["claude", "claude-desktop"] {
            assert_eq!(
                restricted
                    .select_from_payload(tool, &payload)
                    .unwrap()
                    .len(),
                1,
                "{tool} does not depend on recommendation rules"
            );
        }
    }

    #[test]
    fn all_models_and_exempt_tools_are_unchanged() {
        let models = models();
        let all = ToolModelInjectionSettings::default();
        assert_eq!(all.select("trae", models.clone(), None).unwrap(), models);
        let restricted = ToolModelInjectionSettings {
            all_models: false,
            extra_models: vec![],
        };
        for tool in [
            "claude",
            "claude-desktop",
            "claude-science",
            "gemini",
            "hermes",
            "copilot",
            "open-interpreter",
            "anythingllm",
            "open-design",
            "vibe-trading",
            "future-catalog-tool",
        ] {
            assert_eq!(
                restricted.select(tool, models.clone(), None).unwrap(),
                models,
                "{tool}"
            );
        }
    }

    #[test]
    fn additions_are_normalized_and_validated_at_the_persistence_boundary() {
        let settings = ToolModelInjectionSettings {
            all_models: false,
            extra_models: vec![" CUSTOM-* ".into()],
        };
        assert_eq!(settings.normalized().unwrap().extra_models, ["custom-*"]);
        for additions in [
            vec!["x", " X "],
            vec![""],
            vec!["provider/model"],
            vec!["a*b"],
            vec!["a?"],
            vec!["a b"],
        ] {
            let settings = ToolModelInjectionSettings {
                all_models: true,
                extra_models: additions.into_iter().map(str::to_owned).collect(),
            };
            assert!(settings.normalized().is_err());
        }
    }
}
