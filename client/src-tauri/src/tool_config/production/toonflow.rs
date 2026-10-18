const TOONFLOW_TOOL: &str = "toonflow";
const TOONFLOW_PROVIDER_ID: &str = "const-api";
const TOONFLOW_PROVIDER_LABEL: &str = "CONST API";

fn toonflow_protocol_name(protocol: ToolProtocol) -> Result<&'static str> {
    match protocol {
        ToolProtocol::OpenAiResponses => Ok("openai-responses"),
        ToolProtocol::OpenAiChat => Ok("openai-completions"),
        _ => Err(anyhow!(
            "Toonflow does not support tool protocol {}",
            protocol.as_str()
        )),
    }
}

fn toonflow_model_entries(models: &[ToolModelInfo]) -> Result<Vec<serde_json::Value>> {
    configured_additional_tool_models("Toonflow", models)?
        .into_iter()
        .map(|model| {
            let id = model.id.trim();
            let label = if model.display_name.trim().is_empty() {
                id
            } else {
                model.display_name.trim()
            };
            let mut entry = serde_json::Map::new();
            entry.insert("id".to_string(), serde_json::Value::String(id.to_string()));
            entry.insert(
                "label".to_string(),
                serde_json::Value::String(label.to_string()),
            );
            if let Some(value) = model.context_tokens {
                entry.insert(
                    "contextWindow".to_string(),
                    serde_json::Value::Number(value.into()),
                );
            }
            if let Some(value) = model.output_tokens {
                entry.insert(
                    "maxOutputTokens".to_string(),
                    serde_json::Value::Number(value.into()),
                );
            }
            Ok(serde_json::Value::Object(entry))
        })
        .collect()
}

fn toonflow_data_dir_from_program(path: &Path) -> Option<PathBuf> {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let candidate = match file_name.as_str() {
        "startuplauncher.exe" => path
            .parent()?
            .parent()?
            .parent()?
            .parent()?
            .to_path_buf(),
        "launcher.exe" => path.parent()?.parent()?.parent()?.to_path_buf(),
        _ => path.to_path_buf(),
    };
    let mut ancestors = candidate.ancestors();
    while let Some(ancestor) = ancestors.next() {
        let data = ancestor.join("data");
        if data.exists()
            || ancestor
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("stable"))
        {
            return Some(data);
        }
    }
    Some(candidate.join("data"))
}

fn toonflow_data_directory() -> Result<PathBuf> {
    let mut candidates = Vec::new();
    if !test_home_active() {
        if let Some(path) = nonempty_env_path("TOONFLOW_DATA_DIR") {
            candidates.push(expand_tool_home_path(path));
        }
    }

    if let Some(path) = load_tool_program_locations()?.get(TOONFLOW_TOOL) {
        if let Some(data) = toonflow_data_dir_from_program(Path::new(path)) {
            candidates.push(data);
        }
    }
    for path in known_tool_program_paths(TOONFLOW_TOOL) {
        if let Some(data) = toonflow_data_dir_from_program(&path) {
            candidates.push(data);
        }
    }

    if test_home_active() {
        candidates.push(home_dir().join(".toonflow").join("data"));
    } else if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(
            PathBuf::from(local_app_data)
                .join("local.toonflow.desktop")
                .join("stable")
                .join("data"),
        );
    }

    candidates
        .into_iter()
        .find(|path| path.exists())
        .or_else(|| {
            if test_home_active() {
                Some(home_dir().join(".toonflow").join("data"))
            } else {
                std::env::var_os("LOCALAPPDATA").map(|local_app_data| {
                    PathBuf::from(local_app_data)
                        .join("local.toonflow.desktop")
                        .join("stable")
                        .join("data")
                })
            }
        })
        .ok_or_else(|| anyhow!("TOONFLOW_DATA_DIR_UNAVAILABLE: cannot determine Toonflow data directory"))
}

fn toonflow_settings_path() -> Result<PathBuf> {
    Ok(toonflow_data_directory()?.join("settings.json"))
}

fn toonflow_custom_providers_mut(
    root: &mut serde_json::Value,
) -> Result<&mut Vec<serde_json::Value>> {
    ensure_json_object(root);
    if root
        .get("settings")
        .is_none_or(|value| !value.is_object())
    {
        root["settings"] = serde_json::json!({});
    }
    let settings = root
        .get_mut("settings")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| anyhow!("TOONFLOW_SETTINGS_INVALID: settings must be an object"))?;
    let providers = settings
        .entry("customProviders".to_string())
        .or_insert_with(|| serde_json::json!([]));
    providers.as_array_mut().ok_or_else(|| {
        anyhow!("TOONFLOW_CUSTOM_PROVIDERS_INVALID: customProviders must be an array")
    })
}

fn toonflow_provider_matches(
    provider: &serde_json::Value,
    endpoint: &str,
    api_key: &str,
) -> bool {
    let protocol = provider
        .get("protocol")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    provider
        .get("apiUrl")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| value.trim_end_matches('/') == endpoint.trim_end_matches('/'))
        && provider
            .get("apiKey")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| value == api_key)
        && matches!(protocol, "openai-responses" | "openai-completions")
        && provider
            .get("models")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|models| !models.is_empty())
}

fn upsert_toonflow_provider(
    root: &mut serde_json::Value,
    endpoint: &str,
    api_key: &str,
    protocol: ToolProtocol,
    model_entries: Vec<serde_json::Value>,
) -> Result<()> {
    let provider = serde_json::json!({
        "id": TOONFLOW_PROVIDER_ID,
        "label": TOONFLOW_PROVIDER_LABEL,
        "apiUrl": endpoint,
        "apiKey": api_key.trim(),
        "protocol": toonflow_protocol_name(protocol)?,
        "models": model_entries,
    });
    let providers = toonflow_custom_providers_mut(root)?;
    if let Some(existing) = providers.iter_mut().find(|item| {
        item.get("id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|id| id == TOONFLOW_PROVIDER_ID)
    }) {
        *existing = provider;
    } else {
        providers.push(provider);
    }
    Ok(())
}

fn toonflow_result(
    path: &Path,
    root: &serde_json::Value,
    api_key: &str,
    already_configured: bool,
    details: &[(&str, String)],
) -> Result<ToolApplyResult> {
    let mut builder = ToolApplyBuilder::default();
    observe_json(path, root, &mut builder)?;
    let mut result = builder.finish(TOONFLOW_TOOL);
    result.already_configured &= already_configured;
    result
        .details
        .insert("provider_id".to_string(), TOONFLOW_PROVIDER_ID.to_string());
    result
        .details
        .insert("api_key_configured".to_string(), (!api_key.is_empty()).to_string());
    for (key, value) in details {
        result.details.insert((*key).to_string(), value.clone());
    }
    attach_tool_protocol(&mut result, ToolProtocol::OpenAiResponses);
    Ok(result)
}

pub(crate) fn apply_toonflow_config(
    base_url: &str,
    api_key: &str,
    models: &[ToolModelInfo],
    protocol: ToolProtocol,
) -> Result<ToolApplyResult> {
    let endpoint = tool_surface_url(base_url, ToolProtocol::OpenAiResponses.surface());
    let model_entries = toonflow_model_entries(models)?;
    let path = toonflow_settings_path()?;
    with_tool_config_file_transaction(TOONFLOW_TOOL, std::slice::from_ref(&path), || {
        let mut root = read_json_or_default(&path, serde_json::json!({}))?;
        upsert_toonflow_provider(
            &mut root,
            &endpoint,
            api_key,
            protocol,
            model_entries,
        )?;
        let content = json_text(&root)?;
        let mut result = ToolApplyBuilder::default();
        write_text_with_backup(&path, &content, TOONFLOW_TOOL, &mut result)?;
        let mut result = result.finish(TOONFLOW_TOOL);
        result.details.insert(
            "provider_id".to_string(),
            TOONFLOW_PROVIDER_ID.to_string(),
        );
        result.details.insert(
            "config_path".to_string(),
            path.display().to_string(),
        );
        result.details.insert(
            "model_count".to_string(),
            root["settings"]["customProviders"]
                .as_array()
                .and_then(|providers| {
                    providers.iter().find(|provider| {
                        provider.get("id").and_then(serde_json::Value::as_str)
                            == Some(TOONFLOW_PROVIDER_ID)
                    })
                })
                .and_then(|provider| provider.get("models"))
                .and_then(serde_json::Value::as_array)
                .map_or_else(|| "0".to_string(), |models| models.len().to_string()),
        );
        attach_tool_protocol(&mut result, protocol);
        Ok(result)
    })
}

pub(crate) fn check_toonflow_config(
    base_url: &str,
    api_key: &str,
) -> Result<ToolApplyResult> {
    let path = toonflow_settings_path()?;
    let root = read_json_or_default(&path, serde_json::json!({}))?;
    let endpoint = tool_surface_url(base_url, ToolProtocol::OpenAiResponses.surface());
    let configured = root
        .get("settings")
        .and_then(serde_json::Value::as_object)
        .and_then(|settings| settings.get("customProviders"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|providers| {
            providers
                .iter()
                .any(|provider| toonflow_provider_matches(provider, &endpoint, api_key.trim()))
        });
    toonflow_result(
        &path,
        &root,
        api_key,
        configured,
        &[("config_path", path.display().to_string())],
    )
}

pub(crate) fn remove_toonflow_config() -> Result<ToolApplyResult> {
    let path = toonflow_settings_path()?;
    if !path.exists() {
        return check_toonflow_config("", "");
    }
    with_tool_config_file_transaction(TOONFLOW_TOOL, std::slice::from_ref(&path), || {
        let mut root = read_json_or_default(&path, serde_json::json!({}))?;
        let providers = root
            .get_mut("settings")
            .and_then(serde_json::Value::as_object_mut)
            .and_then(|settings| settings.get_mut("customProviders"))
            .and_then(serde_json::Value::as_array_mut);
        let Some(providers) = providers else {
            return check_toonflow_config("", "");
        };
        let before_len = providers.len();
        providers.retain(|provider| {
            provider
                .get("id")
                .and_then(serde_json::Value::as_str)
                != Some(TOONFLOW_PROVIDER_ID)
        });
        if providers.len() == before_len {
            let mut result = check_toonflow_config("", "")?;
            result.details.insert("provider_removed".to_string(), "false".to_string());
            return Ok(result);
        }
        let content = json_text(&root)?;
        let mut result = ToolApplyBuilder::default();
        write_text_with_backup(&path, &content, TOONFLOW_TOOL, &mut result)?;
        let mut result = result.finish(TOONFLOW_TOOL);
        result.details.insert("provider_removed".to_string(), "true".to_string());
        result.details.insert("config_path".to_string(), path.display().to_string());
        attach_tool_protocol(&mut result, ToolProtocol::OpenAiResponses);
        Ok(result)
    })
}
