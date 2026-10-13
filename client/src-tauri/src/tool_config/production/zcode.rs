fn zcode_provider_config_path() -> PathBuf {
    home_dir()
        .join(".zcode")
        .join("v2")
        .join("provider_config.json")
}

fn zcode_model_config(
    model: &ToolModelInfo,
    existing: Option<&serde_json::Value>,
) -> serde_json::Value {
    let mut config = existing.cloned().unwrap_or_else(|| serde_json::json!({}));
    config["enabled"] = serde_json::json!(true);
    ensure_json_object_field(&mut config, "properties");
    config["properties"]["supportsToolCall"] = serde_json::json!(model.tool_call);
    ensure_json_object_field(&mut config["properties"], "inputFormat");
    for (field, modality) in [
        ("supportsImage", "image"),
        ("supportsVideo", "video"),
        ("supportsAudio", "audio"),
        ("supportsPdf", "pdf"),
    ] {
        config["properties"]["inputFormat"][field] =
            serde_json::json!(model.input_modalities.iter().any(|kind| kind == modality));
    }
    ensure_json_object_field(&mut config, "optionSpecs");
    if let Some(context) = model.context_tokens.filter(|value| *value > 0) {
        config["properties"]["contextWindow"] = serde_json::json!(context);
    }
    if let Some(output) = model.output_tokens.filter(|value| *value > 0) {
        ensure_json_object_field(&mut config["optionSpecs"], "maxOutputTokens");
        config["optionSpecs"]["maxOutputTokens"]["max"] = serde_json::json!(output);
    }
    if !model.reasoning {
        config["optionSpecs"]["reasoningLevel"] = serde_json::Value::Null;
    } else if !model.reasoning_efforts.is_empty() {
        config["optionSpecs"]["reasoningLevel"] = serde_json::json!({
            "values": model.reasoning_efforts,
            "map": "{\"reasoning_effort\": reasoningLevel}",
        });
    } else {
        config["optionSpecs"]
            .as_object_mut()
            .expect("initialized option specs")
            .remove("reasoningLevel");
    }
    config
}

fn upsert_zcode_provider_registry(
    root: &mut serde_json::Value,
    base_url: &str,
    api_key: &str,
    models: Option<&[ToolModelInfo]>,
) -> Result<bool> {
    // The native decoder is strict. An unknown/corrupt version must not fall
    // back to writing the obsolete file and then claim configuration succeeded.
    if root
        .get("schemaVersion")
        .and_then(serde_json::Value::as_u64)
        != Some(1)
    {
        return Err(anyhow!(
            "ZCode provider_config.json requires supported schemaVersion 1"
        ));
    }
    let mut identities = HashSet::new();
    for pointer in [
        "/config/providerConfigRules/providerRules",
        "/config/modelConfigRules/providerModelRules",
        "/config/modelConfigRules/manualProviderModelRules",
    ] {
        let rules = root
            .pointer(pointer)
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| anyhow!("ZCode provider_config.json requires array {pointer}"))?;
        for rule in rules {
            let provider = rule
                .get("providerId")
                .and_then(serde_json::Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or_else(|| anyhow!("ZCode {pointer} contains a rule without providerId"))?;
            let model = if pointer.ends_with("/providerRules") {
                ""
            } else {
                rule.get("modelId")
                    .and_then(serde_json::Value::as_str)
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| anyhow!("ZCode {pointer} contains a rule without modelId"))?
            };
            if !identities.insert((provider, model))
                || !rule.get("config").is_some_and(serde_json::Value::is_object)
            {
                return Err(anyhow!(
                    "ZCode {pointer} contains duplicate IDs or invalid config"
                ));
            }
        }
    }
    let manual_ids = root["config"]["modelConfigRules"]["manualProviderModelRules"]
        .as_array()
        .expect("validated rule array")
        .iter()
        .filter(|rule| rule["providerId"] == ADDITIONAL_CONST_API_PROVIDER_ID)
        .filter_map(|rule| rule["modelId"].as_str().map(str::to_string))
        .collect::<HashSet<_>>();
    let rules = root["config"]["providerConfigRules"]["providerRules"]
        .as_array_mut()
        .expect("validated rule array");
    let index = rules
        .iter()
        .position(|rule| rule["providerId"] == ADDITIONAL_CONST_API_PROVIDER_ID)
        .unwrap_or_else(|| {
            rules.push(
                serde_json::json!({"providerId": ADDITIONAL_CONST_API_PROVIDER_ID, "config": {}}),
            );
            rules.len() - 1
        });
    let provider = &mut rules[index];
    provider["providerName"] = serde_json::json!(CONST_API_DISPLAY_NAME);
    provider["enabled"] = serde_json::json!(true);
    let config = &mut provider["config"];
    config["group"] = serde_json::json!("standard-personal");
    ensure_json_object_field(config, "access");
    config["access"]["type"] = serde_json::json!("api-key");
    config["access"]["apiKey"] = serde_json::json!(api_key);
    ensure_json_object_field(config, "api");
    config["api"]["type"] = serde_json::json!("openai-chat-completions");
    config["api"]["baseUrl"] = serde_json::json!(tool_surface_url(base_url, "v1"));
    if let Some(models) = models {
        let ids = models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>();
        config["personalModelIds"] = serde_json::json!(ids);
        config["modelOrder"] = serde_json::json!(ids);
    }
    let models_present = config["personalModelIds"].as_array().is_some_and(|ids| {
        !ids.is_empty()
            && ids.iter().all(|id| {
                id.as_str().is_some_and(|id| {
                    !id.trim().is_empty() && id != "code-cheap" && id != LOCAL_PLACEHOLDER_KEY
                })
            })
    });
    if let Some(models) = models {
        let rules = root["config"]["modelConfigRules"]["providerModelRules"]
            .as_array_mut()
            .expect("validated rule array");
        let existing = rules
            .iter()
            .filter(|rule| rule["providerId"] == ADDITIONAL_CONST_API_PROVIDER_ID)
            .map(|rule| {
                (
                    rule["modelId"]
                        .as_str()
                        .expect("validated model ID")
                        .to_string(),
                    rule["config"].clone(),
                )
            })
            .collect::<HashMap<_, _>>();
        rules.retain(|rule| rule["providerId"] != ADDITIONAL_CONST_API_PROVIDER_ID);
        rules.extend(
            models
                .iter()
                .filter(|model| !manual_ids.contains(&model.id))
                .map(|model| {
                    serde_json::json!({
                        "providerId": ADDITIONAL_CONST_API_PROVIDER_ID,
                        "modelId": model.id,
                        "config": zcode_model_config(model, existing.get(&model.id)),
                    })
                }),
        );
    }
    Ok(models_present)
}

fn configure_zcode_provider_registry(
    base_url: &str,
    api_key: &str,
    models: Option<&[ToolModelInfo]>,
) -> Result<ToolApplyResult> {
    let path = zcode_provider_config_path();
    let mut root = read_json_or_default(&path, serde_json::json!({}))?;
    let models_present = upsert_zcode_provider_registry(&mut root, base_url, api_key, models)?;
    let mut result = ToolApplyBuilder::default();
    if models.is_some() {
        write_text_with_backup(&path, &json_text(&root)?, "zcode", &mut result)?;
    } else {
        observe_json(&path, &root, &mut result)?;
    }
    let mut result = result.finish("zcode");
    result.already_configured &= models_present;
    attach_tool_protocol(&mut result, ToolProtocol::OpenAiChat);
    Ok(result)
}
