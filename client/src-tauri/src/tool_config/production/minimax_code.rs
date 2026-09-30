// Official contract: MiniMax-AI/minimax-code packages/config and model-resolver-byok.
// MCode 0.5.8 resolves `api` from the provider, not individual model entries.
fn minimax_code_config_path() -> PathBuf {
    let root = if test_home_active() {
        home_dir().join(".minimax")
    } else {
        nonempty_env_path("MINIMAX_DATA_DIR")
            .or_else(|| nonempty_env_path("MAVIS_DATA_DIR"))
            .map(expand_tool_home_path)
            .unwrap_or_else(|| home_dir().join(".minimax"))
    };
    root.join("config.yaml")
}

fn configure_minimax_code(
    base_url: &str,
    api_key: &str,
    models: Option<&[ToolModelInfo]>,
    protocol: ToolProtocol,
) -> Result<ToolApplyResult> {
    let path = minimax_code_config_path();
    let raw = read_text_or_empty(&path)?;
    let mut root: serde_json::Value = if raw.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_yaml::from_str(&raw).context("parse MiniMax Code config.yaml")?
    };
    let root = root
        .as_object_mut()
        .context("MiniMax Code config.yaml must be a mapping")?;
    let previous = root
        .get("defaultModel")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let providers = root
        .entry("custom_provider")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .context("MiniMax Code custom_provider must be a mapping")?;
    let provider = providers
        .entry(ADDITIONAL_CONST_API_PROVIDER_ID)
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .context("MiniMax Code CONST API provider must be a mapping")?;
    let protocol = if models.is_some() {
        protocol
    } else {
        provider
            .get("api")
            .and_then(serde_json::Value::as_str)
            .and_then(pi_sdk_protocol)
            .filter(|p| tool_supports_protocol("minimax-code", *p))
            .unwrap_or(protocol)
    };
    provider.insert("kind".into(), serde_json::json!("custom"));
    provider.insert("enabled".into(), serde_json::json!(true));
    provider.insert("name".into(), serde_json::json!("CONST API"));
    provider.insert("api".into(), serde_json::json!(pi_sdk_api_mode(protocol)));
    let options = provider
        .entry("options")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .context("MiniMax Code provider options must be a mapping")?;
    options.insert(
        "baseURL".into(),
        serde_json::json!(pi_sdk_base_url(base_url, protocol)),
    );
    options.insert("apiKey".into(), serde_json::json!(api_key));
    options.insert("authMode".into(), serde_json::json!("api-key"));
    if let Some(models) = models {
        let models = configured_additional_tool_models("MiniMax Code", models)?;
        let mut entries = serde_json::Map::new();
        for model in &models {
            let id = model.id.trim();
            let mut entry = provider
                .get("models")
                .and_then(|v| v.get(id))
                .and_then(serde_json::Value::as_object)
                .cloned()
                .unwrap_or_default();
            entry.insert(
                "name".into(),
                serde_json::json!(if model.display_name.trim().is_empty() {
                    id
                } else {
                    model.display_name.trim()
                }),
            );
            entry.insert("enabled".into(), serde_json::json!(true));
            entry.insert("reasoning".into(), serde_json::json!(model.reasoning));
            // Desktop and CLI read selectable depths from this field, not the
            // reasoning boolean or OpenCode's `variants` schema.
            let efforts = declared_reasoning_efforts(model);
            let mut thinking = entry
                .remove("thinking")
                .and_then(|value| value.as_object().cloned())
                .unwrap_or_default();
            thinking.remove("effort"); // legacy selected value, not capability metadata
            if efforts.is_empty() {
                thinking.remove("effortOptions");
                thinking.remove("defaultEffort");
            } else {
                if !thinking
                    .get("defaultEffort")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|effort| efforts.iter().any(|value| value == effort))
                {
                    thinking.remove("defaultEffort");
                }
                thinking.insert("effortOptions".into(), serde_json::json!(efforts));
            }
            if !thinking.is_empty() {
                entry.insert("thinking".into(), thinking.into());
            }
            entry.insert("tool_call".into(), serde_json::json!(model.tool_call));
            entry.insert("modalities".into(), serde_json::json!({
                "input": if model.input_modalities.iter().any(|v| v == "image") { vec!["text", "image"] } else { vec!["text"] },
                "output": ["text"]
            }));
            let mut capabilities = entry
                .remove("capabilities")
                .and_then(|value| value.as_object().cloned())
                .unwrap_or_default();
            capabilities.insert(
                "support_image".into(),
                serde_json::json!(model.input_modalities.iter().any(|v| v == "image")),
            );
            entry.insert("capabilities".into(), capabilities.into());
            let mut limits = serde_json::Map::new();
            for (key, value) in [
                ("context", model.context_tokens),
                ("output", model.output_tokens),
            ] {
                if let Some(value) = value.filter(|v| *v > 0) {
                    limits.insert(key.into(), value.into());
                }
            }
            if !limits.is_empty() {
                entry.insert("limit".into(), limits.into());
            }
            if let Some(context) = model.context_tokens.filter(|value| *value > 0) {
                // These are local conversation/compaction budgets, not claims
                // about upstream context tiers or their prices.
                let choices = tool_context_budget_choices(
                    entry.get("contextWindowOptions"),
                    context,
                    model.output_tokens,
                );
                entry.insert("contextWindowOptions".into(), serde_json::json!(choices));
            } else {
                entry.remove("contextWindowOptions");
            }
            entries.insert(id.into(), entry.into());
        }
        let ids = models.iter().map(|m| m.id.trim()).collect::<Vec<_>>();
        provider.insert("model_order".into(), serde_json::json!(ids));
        provider.insert("models".into(), entries.into());
        let selected = previous
            .strip_prefix("custom_provider:const-api/")
            .filter(|id| ids.contains(id))
            .map(str::to_string)
            .unwrap_or_else(|| ids[0].to_string());
        let selected = format!("custom_provider:const-api/{selected}");
        if previous != selected {
            for key in [
                "defaultModelThinking",
                "defaultModelContextWindow",
                "defaultModelVariant",
            ] {
                root.remove(key);
            }
        } else if let Some(model) = models.iter().find(|model| {
            selected.strip_prefix("custom_provider:const-api/") == Some(model.id.trim())
        }) {
            // A refreshed route can shrink its limits or supported depths.
            // Keep a valid user choice; stale defaults otherwise fail before
            // MiniMax sends a request, even though the model still exists.
            if root.get("defaultModelThinking").is_some_and(|value| {
                let effort = value
                    .get("effort")
                    .and_then(serde_json::Value::as_str)
                    .or_else(|| value.as_str());
                effort.is_some_and(|effort| {
                    !model.reasoning
                        || (!model.reasoning_efforts.is_empty()
                            && !model.reasoning_efforts.iter().any(|value| value == effort))
                })
            }) {
                root.remove("defaultModelThinking");
            }
            if root
                .get("defaultModelContextWindow")
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|value| {
                    value == 0 || model.context_tokens.is_some_and(|maximum| value > maximum)
                })
            {
                root.remove("defaultModelContextWindow");
            }
        }
        root.insert("defaultModel".into(), serde_json::json!(selected));
        // Auxiliary tasks otherwise keep using a previously selected paid provider.
        root.remove("defaultLightModel");
    }
    let selected = root
        .get("defaultModel")
        .and_then(serde_json::Value::as_str)
        .and_then(|v| v.strip_prefix("custom_provider:const-api/"));
    let configured = !root.contains_key("defaultLightModel")
        && selected.is_some_and(|id| {
            !id.is_empty()
                && root
                    .get("custom_provider")
                    .and_then(|v| v.get("const-api"))
                    .and_then(|v| v.get("models"))
                    .and_then(|v| v.get(id))
                    .is_some_and(|v| {
                        v.is_object()
                            && v.get("enabled").and_then(serde_json::Value::as_bool) != Some(false)
                    })
        });
    let content = serde_yaml::to_string(&root)?;
    let mut result = ToolApplyBuilder::default();
    if models.is_some() {
        write_text_with_backup(&path, &content, "minimax-code", &mut result)?;
    } else {
        observe_text(&path, &content, &mut result)?;
    }
    let mut result = result.finish("minimax-code");
    result.already_configured &= configured;
    attach_tool_protocol(&mut result, protocol);
    Ok(result)
}
