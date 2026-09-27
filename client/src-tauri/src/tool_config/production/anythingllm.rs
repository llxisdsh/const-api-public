fn anythingllm_config_path() -> PathBuf {
    #[cfg(target_os = "windows")]
    let root = if test_home_active() {
        home_dir().join("AppData").join("Roaming")
    } else {
        nonempty_env_path("APPDATA").unwrap_or_else(|| home_dir().join("AppData").join("Roaming"))
    };
    #[cfg(target_os = "macos")]
    let root = home_dir().join("Library").join("Application Support");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let root = xdg_tool_config_root();
    root.join("anythingllm-desktop")
        .join("storage")
        .join(".env")
}

fn upsert_anythingllm_env(
    raw: &str,
    base_url: &str,
    api_key: &str,
    model: &ToolModelInfo,
) -> Result<String> {
    let base_url = tool_surface_url(base_url, "v1");
    // dotenv accepts single-quoted values verbatim, including '#' in local keys.
    // Reject line breaks/quotes instead of generating an ambiguous env file.
    let quote = |value: &str| -> Result<String> {
        if value.contains(['\r', '\n', '\'']) {
            return Err(anyhow!(
                "AnythingLLM configuration values cannot contain line breaks or single quotes"
            ));
        }
        Ok(format!("'{value}'"))
    };
    let mut updates = vec![
        ("LLM_PROVIDER", "generic-openai".to_string()),
        ("GENERIC_OPEN_AI_BASE_PATH", quote(&base_url)?),
        ("GENERIC_OPEN_AI_API_KEY", quote(api_key)?),
        ("GENERIC_OPEN_AI_MODEL_PREF", quote(model.id.trim())?),
    ];
    // Only publish limits that the selected model actually declares. The app's
    // existing max-output preference remains in charge, capped to the model.
    if let Some(context) = model.context_tokens.filter(|value| *value > 0) {
        updates.push(("GENERIC_OPEN_AI_MODEL_TOKEN_LIMIT", context.to_string()));
    }
    if let Some(output) = model.output_tokens.filter(|value| *value > 0) {
        let current = env_text_to_status_json(raw);
        let preferred = current
            .get("GENERIC_OPEN_AI_MAX_TOKENS")
            .and_then(serde_json::Value::as_str)
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            // Match GenericOpenAI's own default rather than raising the user's
            // output budget merely because a provider was reconfigured.
            .unwrap_or(1_024);
        updates.push((
            "GENERIC_OPEN_AI_MAX_TOKENS",
            output.min(preferred).to_string(),
        ));
    }
    let updates = updates
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect::<Vec<_>>();
    Ok(upsert_env_vars(raw, &updates))
}

fn apply_anythingllm_config(
    base_url: &str,
    api_key: &str,
    models: &[ToolModelInfo],
) -> Result<ToolApplyResult> {
    let models = configured_additional_tool_models("AnythingLLM", models)?;
    let path = anythingllm_config_path();
    let content =
        upsert_anythingllm_env(&read_text_or_empty(&path)?, base_url, api_key, &models[0])?;
    let mut result = ToolApplyBuilder::default();
    write_text_with_backup(&path, &content, "anythingllm", &mut result)?;
    let mut result = result.finish("anythingllm");
    attach_tool_protocol(&mut result, ToolProtocol::OpenAiChat);
    Ok(result)
}

fn check_anythingllm_config(base_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    let path = anythingllm_config_path();
    let raw = read_text_or_empty(&path)?;
    let current = env_text_to_status_json(&raw);
    let model = ToolModelInfo {
        id: current
            .get("GENERIC_OPEN_AI_MODEL_PREF")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        ..ToolModelInfo::default()
    };
    let content = upsert_anythingllm_env(&raw, base_url, api_key, &model)?;
    let mut result = ToolApplyBuilder::default();
    observe_text(&path, &content, &mut result)?;
    let mut result = result.finish("anythingllm");
    result.already_configured &= !model.id.trim().is_empty();
    attach_tool_protocol(&mut result, ToolProtocol::OpenAiChat);
    Ok(result)
}
