const ADDITIONAL_CONST_API_PROVIDER_ID: &str = "const-api";
const ADDITIONAL_CONST_API_ENV_KEY: &str = "CONST_API_API_KEY";
const DEEPSEEK_HARNESS_CREDENTIAL_ENV_KEY: &str = "CONST_API_DEEPSEEK_HARNESS_API_KEY";

fn nonempty_env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn expand_tool_home_path(path: PathBuf) -> PathBuf {
    let raw = path.to_string_lossy();
    if raw == "~" {
        return home_dir();
    }
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        return home_dir().join(rest);
    }
    path
}

fn tool_home_override_or_default(name: &str, fallback: &str) -> PathBuf {
    if test_home_active() {
        return home_dir().join(fallback);
    }
    nonempty_env_path(name)
        .map(expand_tool_home_path)
        .unwrap_or_else(|| home_dir().join(fallback))
}

fn xdg_tool_config_root() -> PathBuf {
    if test_home_active() {
        return home_dir().join(".config");
    }
    nonempty_env_path("XDG_CONFIG_HOME")
        .map(expand_tool_home_path)
        .unwrap_or_else(|| home_dir().join(".config"))
}

fn codex_home() -> PathBuf {
    tool_home_override_or_default("CODEX_HOME", ".codex")
}

fn opencode_config_path() -> Result<PathBuf> {
    let (directory, file) = if test_home_active() {
        (None, None)
    } else {
        (
            nonempty_env_path("OPENCODE_CONFIG_DIR"),
            nonempty_env_path("OPENCODE_CONFIG"),
        )
    };
    opencode_config_path_from_sources(xdg_tool_config_root().join("opencode"), directory, file)
}

fn opencode_config_path_from_sources(
    default_root: PathBuf,
    directory: Option<PathBuf>,
    file: Option<PathBuf>,
) -> Result<PathBuf> {
    // OpenCode merges CONFIG_DIR after CONFIG. Write the higher-precedence
    // source if both are set, otherwise the configured provider is shadowed.
    if let Some(root) = directory {
        return Ok(opencode_json_path(resolve_tool_path_override(root)?));
    }
    if let Some(path) = file {
        return resolve_tool_path_override(path);
    }
    Ok(opencode_json_path(default_root))
}

fn opencode_json_path(root: PathBuf) -> PathBuf {
    let jsonc = root.join("opencode.jsonc");
    if jsonc.exists() {
        jsonc
    } else {
        root.join("opencode.json")
    }
}

fn openclaw_config_path() -> Result<PathBuf> {
    if !test_home_active() {
        if let Some(path) = nonempty_env_path("OPENCLAW_CONFIG_PATH") {
            return resolve_tool_path_override(path);
        }
    }
    Ok(tool_home_override_or_default("OPENCLAW_STATE_DIR", ".openclaw").join("openclaw.json"))
}

fn desktop_config_root() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if !test_home_active() {
            if let Some(root) = nonempty_env_path("APPDATA") {
                return root;
            }
        }
        home_dir().join("AppData").join("Roaming")
    }
    #[cfg(target_os = "macos")]
    {
        home_dir().join("Library").join("Application Support")
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        xdg_tool_config_root()
    }
}

fn resolve_tool_path_override(path: PathBuf) -> Result<PathBuf> {
    let path = expand_tool_home_path(path);
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn first_existing_config(root: PathBuf, names: &[&str]) -> PathBuf {
    names
        .iter()
        .map(|name| root.join(name))
        .find(|path| path.exists())
        .unwrap_or_else(|| root.join(names[0]))
}

fn kimicode_config_path() -> PathBuf {
    tool_home_override_or_default("KIMI_CODE_HOME", ".kimi-code").join("config.toml")
}

fn mimocode_config_path() -> Result<PathBuf> {
    let root = if test_home_active() {
        home_dir().join(".config").join("mimocode")
    } else if let Some(root) = nonempty_env_path("MIMOCODE_HOME") {
        let root = expand_tool_home_path(root);
        if !root.is_absolute() {
            return Err(anyhow!("MIMOCODE_HOME must be an absolute path"));
        }
        root.join("config")
    } else {
        xdg_tool_config_root().join("mimocode")
    };
    Ok(first_existing_config(
        root,
        &["mimocode.jsonc", "mimocode.json", "config.json"],
    ))
}

fn qwencode_config_path() -> PathBuf {
    tool_home_override_or_default("QWEN_HOME", ".qwen").join("settings.json")
}

fn openscience_config_path() -> Result<PathBuf> {
    // ai4s-research/open-science bundles an isolated OpenCode runtime. The
    // similarly named synthetic-sciences CLI's configuration is unrelated.
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let root = desktop_config_root();
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let root = if test_home_active() {
        home_dir().join(".local").join("share")
    } else {
        nonempty_env_path("XDG_DATA_HOME")
            .unwrap_or_else(|| home_dir().join(".local").join("share"))
    };
    Ok(opencode_json_path(
        root.join("com.ai4s.workbench")
            .join("runtime")
            .join("xdg-config")
            .join("opencode"),
    ))
}

fn vibe_trading_config_path() -> PathBuf {
    // The current CLI and Web UI both use this user-level provider file.
    // VIBE_TRADING_HOME relocates runtime artifacts, not the provider .env.
    home_dir().join(".vibe-trading").join(".env")
}

fn zcode_config_path() -> PathBuf {
    // ZCode 3.x stores its OpenCode-compatible provider registry here. The
    // desktop app preserves unrelated top-level fields when it rewrites this
    // file, so CONST API follows the same merge contract.
    home_dir().join(".zcode").join("v2").join("config.json")
}

fn claude_science_env_path() -> PathBuf {
    // Claude Science reserves config.toml for network and sandbox policy; it
    // does not expose an inference endpoint there. Keep the gateway values in
    // a manifest-owned file and inject them only when CONST API launches it.
    home_dir().join(".claude-science").join("const-api.env")
}

fn copilot_env_path() -> PathBuf {
    // Copilot CLI's BYOK contract is environment-based. Keep the values in a
    // manifest-owned file and inject them only into processes launched here.
    home_dir().join(".copilot").join("const-api.env")
}

fn raven_config_path() -> PathBuf {
    tool_home_override_or_default("RAVEN_HOME", ".raven").join("config.json")
}

fn pi_models_path() -> PathBuf {
    tool_home_override_or_default("PI_CODING_AGENT_DIR", ".pi/agent").join("models.json")
}

fn cline_settings_root() -> Result<PathBuf> {
    let data_root = if test_home_active() {
        home_dir().join(".cline").join("data")
    } else if let Some(path) = nonempty_env_path("CLINE_DATA_DIR") {
        resolve_tool_path_override(path)?
    } else {
        home_dir().join(".cline").join("data")
    };
    Ok(data_root.join("settings"))
}

fn cline_provider_paths() -> Result<(PathBuf, PathBuf)> {
    let root = cline_settings_root()?;
    Ok((root.join("providers.json"), root.join("models.json")))
}

fn reasonix_home_path() -> PathBuf {
    if !test_home_active() {
        if let Some(root) = nonempty_env_path("REASONIX_HOME") {
            return expand_tool_home_path(root);
        }
    }
    #[cfg(target_os = "windows")]
    {
        if test_home_active() {
            return home_dir().join("AppData").join("Roaming").join("reasonix");
        }
        if let Some(app_data) = std::env::var_os("APPDATA").map(PathBuf::from) {
            return app_data.join("reasonix");
        }
    }
    home_dir().join(".reasonix")
}

fn reasonix_config_paths() -> (PathBuf, PathBuf) {
    let root = reasonix_home_path();
    (root.join("config.toml"), root.join(".env"))
}

fn deepseek_harness_config_paths() -> (PathBuf, PathBuf) {
    let root = tool_home_override_or_default("DSH_HOME", ".dsh");
    (root.join("settings.yaml"), root.join(".credentials.yaml"))
}

fn goose_config_root() -> PathBuf {
    if test_home_active() {
        return home_dir().join(".config").join("goose");
    }
    #[cfg(target_os = "windows")]
    if let Some(app_data) = std::env::var_os("APPDATA").map(PathBuf::from) {
        return app_data.join("Block").join("goose").join("config");
    }
    xdg_tool_config_root().join("goose")
}

fn goose_config_paths() -> (PathBuf, PathBuf) {
    let root = goose_config_root();
    (
        root.join("custom_providers").join("const_api.json"),
        root.join("const-api.env"),
    )
}

fn mistral_vibe_config_paths() -> (PathBuf, PathBuf) {
    let root = tool_home_override_or_default("VIBE_HOME", ".vibe");
    (root.join("config.toml"), root.join(".env"))
}

fn open_design_desktop_config_path(root: &Path, program: Option<&Path>, platform: &str) -> PathBuf {
    let identity = program
        .map(|path| normalized_tool_program_identity(&path.to_string_lossy()))
        .unwrap_or_default();
    let (application, channel) = match identity.as_str() {
        "open design beta" => ("Open Design Beta", "beta"),
        "open design prerelease" => ("Open Design Prerelease", "prerelease"),
        "open design preview" => ("Open Design Preview", "preview"),
        _ => ("Open Design", "stable"),
    };
    root.join(application)
        .join("namespaces")
        .join(format!("release-{channel}-{platform}"))
        .join("data")
        .join("app-config.json")
}

fn open_design_config_path() -> Result<PathBuf> {
    let program = if test_home_active() {
        None
    } else {
        tool_launch_candidate_without_process_scan("open-design")?
    };
    let program_path = program.as_ref().map(|candidate| Path::new(&candidate.path));
    if program_path
        .is_some_and(|path| normalized_tool_program_identity(&path.to_string_lossy()) == "od")
    {
        let root = nonempty_env_path("OD_DATA_DIR")
            .map(resolve_tool_path_override)
            .transpose()?
            .unwrap_or(std::env::current_dir()?.join(".od"));
        return Ok(root.join("app-config.json"));
    }
    let platform = if cfg!(target_os = "windows") {
        "win"
    } else if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    };
    let default_path =
        open_design_desktop_config_path(&desktop_config_root(), program_path, platform);
    if test_home_active() {
        return Ok(default_path);
    }
    let config_path = nonempty_env_path("OD_PACKAGED_CONFIG_PATH")
        .map(resolve_tool_path_override)
        .transpose()?
        .or_else(|| {
            program_path.map(|path| {
                if path.extension().is_some_and(|ext| ext == "app") {
                    path.join("Contents/Resources/open-design-config.json")
                } else {
                    path.with_file_name("resources")
                        .join("open-design-config.json")
                }
            })
        });
    let config = config_path
        .as_ref()
        .map(|path| read_json_or_default(path, serde_json::json!({})))
        .transpose()?
        .unwrap_or_default();
    let namespace_root = nonempty_env_path("OD_PACKAGED_NAMESPACE_BASE_ROOT").or_else(|| {
        config
            .get("namespaceBaseRoot")
            .and_then(serde_json::Value::as_str)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    });
    let namespace = std::env::var("OD_PACKAGED_NAMESPACE")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| {
            config
                .get("namespace")
                .and_then(serde_json::Value::as_str)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        });
    if namespace_root.is_none() && namespace.is_none() {
        return Ok(default_path);
    }
    let mut default_namespace_path = default_path;
    default_namespace_path.pop(); // app-config.json
    default_namespace_path.pop(); // data
    let root = namespace_root
        .map(resolve_tool_path_override)
        .transpose()?
        .unwrap_or_else(|| default_namespace_path.with_file_name(""));
    let namespace = namespace
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default_namespace_path.file_name().unwrap_or_default()));
    Ok(root.join(namespace).join("data").join("app-config.json"))
}

fn env_value(raw: &str, key: &str) -> Option<String> {
    raw.lines().find_map(|line| {
        let trimmed = line.trim_start();
        let trimmed = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        let (candidate, value) = trimmed.split_once('=')?;
        (candidate.trim() == key)
            .then(|| {
                value
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string()
            })
            .filter(|value| !value.is_empty())
    })
}

fn model_ids(models: &[ToolModelInfo]) -> Vec<String> {
    models
        .iter()
        .map(|model| model.id.trim())
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect()
}

fn append_path(base_url: &str, path: &str) -> String {
    format!(
        "{}/{}",
        tool_surface_url(base_url, "v1").trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

fn apply_claude_science_config(root_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    resolve_tool_protocol(
        "claude-science",
        Some(ToolProtocol::AnthropicMessages.as_str()),
    )?;
    let path = claude_science_env_path();
    let raw = read_text_or_empty(&path)?;
    let content = upsert_env_vars(
        &raw,
        &[
            ("ANTHROPIC_BASE_URL", root_url),
            ("ANTHROPIC_AUTH_TOKEN", api_key),
        ],
    );
    let mut result = ToolApplyBuilder::default();
    write_text_with_backup(&path, &content, "claude-science", &mut result)?;
    let mut result = result.finish("claude-science");
    result.details.insert(
        "configuration_scope".to_string(),
        "const_api_launch_environment".to_string(),
    );
    attach_tool_protocol(&mut result, ToolProtocol::AnthropicMessages);
    Ok(result)
}

fn check_claude_science_config(root_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    let path = claude_science_env_path();
    let raw = read_text_or_empty(&path)?;
    let content = upsert_env_vars(
        &raw,
        &[
            ("ANTHROPIC_BASE_URL", root_url),
            ("ANTHROPIC_AUTH_TOKEN", api_key),
        ],
    );
    let mut result = ToolApplyBuilder::default();
    observe_text(&path, &content, &mut result)?;
    let mut result = result.finish("claude-science");
    result.details.insert(
        "configuration_scope".to_string(),
        "const_api_launch_environment".to_string(),
    );
    attach_tool_protocol(&mut result, ToolProtocol::AnthropicMessages);
    Ok(result)
}

fn remove_claude_science_config() -> Result<ToolApplyResult> {
    restore_tool_config_from_manifest("claude-science")
}

fn configured_additional_tool_models(
    tool: &str,
    models: &[ToolModelInfo],
) -> Result<Vec<ToolModelInfo>> {
    let mut models = models
        .iter()
        .filter(|model| {
            let id = model.id.trim();
            !id.is_empty() && id != "code-cheap" && id != LOCAL_PLACEHOLDER_KEY
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut seen = HashSet::new();
    models.retain(|model| seen.insert(model.id.clone()));
    models.sort_by(crate::tool_model_metadata::tool_model_display_order);
    if models.is_empty() {
        return Err(anyhow!("TOOL_CONFIG_MODELS_UNAVAILABLE: {tool}"));
    }
    Ok(models)
}

fn json_text(value: &serde_json::Value) -> Result<String> {
    Ok(serde_json::to_string_pretty(value)? + "\n")
}

fn ensure_json_object_field(root: &mut serde_json::Value, key: &str) {
    ensure_json_object(root);
    if root.get(key).is_none_or(|value| !value.is_object()) {
        root[key] = serde_json::json!({});
    }
}

fn kimi_provider_type(protocol: ToolProtocol) -> Result<&'static str> {
    match protocol {
        ToolProtocol::OpenAiResponses => Ok("openai_responses"),
        ToolProtocol::OpenAiChat => Ok("openai"),
        _ => Err(anyhow!(
            "Kimi Code does not support tool protocol {}",
            protocol.as_str()
        )),
    }
}

fn kimi_protocol_from_doc(doc: &DocumentMut) -> ToolProtocol {
    match doc
        .get("providers")
        .and_then(Item::as_table)
        .and_then(|providers| providers.get(ADDITIONAL_CONST_API_PROVIDER_ID))
        .and_then(Item::as_table)
        .and_then(|provider| provider.get("type"))
        .and_then(Item::as_str)
    {
        Some("openai") => ToolProtocol::OpenAiChat,
        _ => ToolProtocol::OpenAiResponses,
    }
}

fn kimi_owned_model_aliases(doc: &DocumentMut) -> Vec<String> {
    let mut aliases = doc
        .get("models")
        .and_then(Item::as_table)
        .map(|models| {
            models
                .iter()
                .filter_map(|(alias, item)| {
                    let model = item.as_table()?;
                    let owned = model.get("provider").and_then(Item::as_str)
                        == Some(ADDITIONAL_CONST_API_PROVIDER_ID);
                    let model_id = model
                        .get("model")
                        .and_then(Item::as_str)
                        .is_some_and(|value| !value.trim().is_empty());
                    (owned && model_id).then(|| alias.to_string())
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    aliases.sort();
    aliases
}

fn upsert_kimicode_toml(
    raw: &str,
    base_url: &str,
    api_key: &str,
    models: &[ToolModelInfo],
    protocol: ToolProtocol,
) -> Result<String> {
    let models = configured_additional_tool_models("Kimi Code", models)?;
    let mut doc = if raw.trim().is_empty() {
        DocumentMut::new()
    } else {
        raw.parse::<DocumentMut>()
            .context("parse Kimi Code config.toml")?
    };
    let first_alias = format!("{ADDITIONAL_CONST_API_PROVIDER_ID}/{}", models[0].id.trim());
    doc["default_model"] = toml_value(first_alias);

    if !doc.contains_key("providers") || !doc["providers"].is_table() {
        doc["providers"] = Item::Table(Table::new());
    }
    let providers = doc["providers"]
        .as_table_mut()
        .ok_or_else(|| anyhow!("Kimi Code providers is not a table"))?;
    if !providers.contains_key(ADDITIONAL_CONST_API_PROVIDER_ID)
        || !providers[ADDITIONAL_CONST_API_PROVIDER_ID].is_table()
    {
        providers[ADDITIONAL_CONST_API_PROVIDER_ID] = Item::Table(Table::new());
    }
    let provider = providers[ADDITIONAL_CONST_API_PROVIDER_ID]
        .as_table_mut()
        .ok_or_else(|| anyhow!("Kimi Code CONST API provider is not a table"))?;
    provider["type"] = toml_value(kimi_provider_type(protocol)?);
    provider["base_url"] = toml_value(tool_surface_url(base_url, "v1"));
    provider["api_key"] = toml_value(api_key);

    if !doc.contains_key("models") || !doc["models"].is_table() {
        doc["models"] = Item::Table(Table::new());
    }
    let model_table = doc["models"]
        .as_table_mut()
        .ok_or_else(|| anyhow!("Kimi Code models is not a table"))?;
    let stale = model_table
        .iter()
        .filter_map(|(alias, item)| {
            (item
                .as_table()
                .and_then(|model| model.get("provider"))
                .and_then(Item::as_str)
                == Some(ADDITIONAL_CONST_API_PROVIDER_ID))
            .then(|| alias.to_string())
        })
        .collect::<Vec<_>>();
    for alias in stale {
        model_table.remove(&alias);
    }
    for model in models {
        let id = model.id.trim();
        let alias = format!("{ADDITIONAL_CONST_API_PROVIDER_ID}/{id}");
        model_table[&alias] = Item::Table(Table::new());
        let entry = model_table[&alias]
            .as_table_mut()
            .ok_or_else(|| anyhow!("Kimi Code model entry is not a table"))?;
        entry["provider"] = toml_value(ADDITIONAL_CONST_API_PROVIDER_ID);
        entry["model"] = toml_value(id);
        // Kimi's model schema only overrides the protocol to Anthropic.
        // Chat vs Responses remains the provider/menu choice; do not write
        // unsupported model-level values such as "openai_responses".
        if select_model_protocol(
            &model,
            protocol,
            &[protocol, ToolProtocol::AnthropicMessages],
        ) == ToolProtocol::AnthropicMessages
        {
            entry["protocol"] = toml_value("anthropic");
            entry["base_url"] = toml_value(tool_surface_url(base_url, "anthropic"));
        }
        let mut capabilities = toml_edit::Array::new();
        if model.tool_call {
            capabilities.push("tool_use");
        }
        if model.reasoning {
            capabilities.push("thinking");
        }
        if model
            .input_modalities
            .iter()
            .any(|modality| modality == "image")
        {
            capabilities.push("image_in");
        }
        entry["capabilities"] = Item::Value(toml_edit::Value::Array(capabilities));
        let context = model
            .context_tokens
            .filter(|value| *value > 0)
            .unwrap_or(128_000);
        entry["max_context_size"] = toml_value(i64::try_from(context).unwrap_or(i64::MAX));
        let display_name = if model.display_name.trim().is_empty() {
            id
        } else {
            model.display_name.trim()
        };
        entry["display_name"] = toml_value(display_name);
        if let Some(output) = model.output_tokens.filter(|value| *value > 0) {
            entry["max_output_size"] = toml_value(i64::try_from(output).unwrap_or(i64::MAX));
            if let Some(input) = context.checked_sub(output).filter(|value| *value > 0) {
                entry["max_input_size"] = toml_value(i64::try_from(input).unwrap_or(i64::MAX));
            }
        }
        if model.reasoning && !model.reasoning_efforts.is_empty() {
            let mut efforts = toml_edit::Array::new();
            for effort in &model.reasoning_efforts {
                if !effort.trim().is_empty() {
                    efforts.push(effort.trim());
                }
            }
            if !efforts.is_empty() {
                entry["support_efforts"] = Item::Value(toml_edit::Value::Array(efforts));
            }
        }
    }

    let mut text = doc.to_string();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    Ok(text)
}

fn apply_kimicode_config(
    base_url: &str,
    api_key: &str,
    models: &[ToolModelInfo],
    protocol: ToolProtocol,
) -> Result<ToolApplyResult> {
    resolve_tool_protocol("kimicode", Some(protocol.as_str()))?;
    let path = kimicode_config_path();
    let raw = read_text_or_empty(&path)?;
    let content = upsert_kimicode_toml(&raw, base_url, api_key, models, protocol)?;
    let mut result = ToolApplyBuilder::default();
    write_text_with_backup(&path, &content, "kimicode", &mut result)?;
    let mut result = result.finish("kimicode");
    attach_tool_protocol(&mut result, protocol);
    Ok(result)
}

fn check_kimicode_config(base_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    let path = kimicode_config_path();
    let raw = read_text_or_empty(&path)?;
    let mut doc = if raw.trim().is_empty() {
        DocumentMut::new()
    } else {
        raw.parse::<DocumentMut>()
            .context("parse Kimi Code config.toml")?
    };
    let protocol = kimi_protocol_from_doc(&doc);
    let aliases = kimi_owned_model_aliases(&doc);
    let models_configured = !aliases.is_empty();
    let selected = doc
        .get("default_model")
        .and_then(Item::as_str)
        .filter(|value| aliases.iter().any(|alias| alias == value))
        .map(str::to_string)
        .or_else(|| aliases.first().cloned());
    if let Some(selected) = selected {
        doc["default_model"] = toml_value(selected);
    }
    if !doc.contains_key("providers") || !doc["providers"].is_table() {
        doc["providers"] = Item::Table(Table::new());
    }
    let providers = doc["providers"]
        .as_table_mut()
        .ok_or_else(|| anyhow!("Kimi Code providers is not a table"))?;
    if !providers.contains_key(ADDITIONAL_CONST_API_PROVIDER_ID)
        || !providers[ADDITIONAL_CONST_API_PROVIDER_ID].is_table()
    {
        providers[ADDITIONAL_CONST_API_PROVIDER_ID] = Item::Table(Table::new());
    }
    let provider = providers[ADDITIONAL_CONST_API_PROVIDER_ID]
        .as_table_mut()
        .ok_or_else(|| anyhow!("Kimi Code CONST API provider is not a table"))?;
    provider["type"] = toml_value(kimi_provider_type(protocol)?);
    provider["base_url"] = toml_value(tool_surface_url(base_url, "v1"));
    provider["api_key"] = toml_value(api_key);
    for alias in &aliases {
        if doc["models"][alias].get("protocol").and_then(Item::as_str) == Some("anthropic") {
            doc["models"][alias]["base_url"] = toml_value(tool_surface_url(base_url, "anthropic"));
        }
    }
    let mut content = doc.to_string();
    if !content.ends_with('\n') {
        content.push('\n');
    }
    let mut result = ToolApplyBuilder::default();
    observe_text(&path, &content, &mut result)?;
    let mut result = result.finish("kimicode");
    result.already_configured &= models_configured;
    attach_tool_protocol(&mut result, protocol);
    Ok(result)
}

fn upsert_openai_compatible_json_provider(
    tool: &str,
    root: &mut serde_json::Value,
    base_url: &str,
    api_key: &str,
    models: Option<serde_json::Map<String, serde_json::Value>>,
    only_configured_models: bool,
    protocol: ToolProtocol,
) -> Vec<String> {
    ensure_json_object_field(root, "provider");
    let mut provider = root["provider"]
        .get(ADDITIONAL_CONST_API_PROVIDER_ID)
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    if only_configured_models {
        provider["only_configured_models"] = serde_json::json!(true);
    }
    if let Some(models) = models {
        provider["models"] = serde_json::Value::Object(models);
    }
    refresh_opencode_provider_connection(tool, &mut provider, base_url, api_key, protocol);
    let mut model_ids = provider
        .get("models")
        .and_then(serde_json::Value::as_object)
        .map(|models| {
            models
                .keys()
                .filter(|id| {
                    let id = id.trim();
                    !id.is_empty() && id != "code-cheap" && id != LOCAL_PLACEHOLDER_KEY
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    model_ids.sort();
    root["provider"][ADDITIONAL_CONST_API_PROVIDER_ID] = provider;
    model_ids
}

fn apply_openai_compatible_json_tool(
    tool: &str,
    path: PathBuf,
    base_url: &str,
    api_key: &str,
    models: &[ToolModelInfo],
    only_configured_models: bool,
    protocol: ToolProtocol,
) -> Result<ToolApplyResult> {
    resolve_tool_protocol(tool, Some(protocol.as_str()))?;
    let models = configured_additional_tool_models(tool, models)?;
    let mut root = read_json5_or_default(&path, serde_json::json!({}))?;
    ensure_json_object(&mut root);
    let mut generated = opencode_compatible_provider(tool, base_url, api_key, &models, protocol);
    let model_map = generated
        .get_mut("models")
        .and_then(serde_json::Value::as_object_mut)
        .map(std::mem::take)
        .context("generated tool model catalog is not an object")?;
    upsert_openai_compatible_json_provider(
        tool,
        &mut root,
        base_url,
        api_key,
        Some(model_map),
        only_configured_models,
        protocol,
    );
    root["model"] = serde_json::json!(format!(
        "{ADDITIONAL_CONST_API_PROVIDER_ID}/{}",
        models[0].id
    ));
    let content = json_text_with_model_order(
        &root,
        &["provider", ADDITIONAL_CONST_API_PROVIDER_ID, "models"],
        &models,
    )?;
    let mut result = ToolApplyBuilder::default();
    write_text_with_backup(&path, &content, tool, &mut result)?;
    let mut result = result.finish(tool);
    attach_tool_protocol(&mut result, protocol);
    Ok(result)
}

fn check_openai_compatible_json_tool(
    tool: &str,
    path: PathBuf,
    base_url: &str,
    api_key: &str,
    only_configured_models: bool,
) -> Result<ToolApplyResult> {
    let mut root = read_json5_or_default(&path, serde_json::json!({}))?;
    ensure_json_object(&mut root);
    let protocol = root
        .pointer("/provider/const-api/npm")
        .and_then(serde_json::Value::as_str)
        .and_then(opencode_package_protocol)
        .unwrap_or(ToolProtocol::OpenAiChat);
    let model_ids = upsert_openai_compatible_json_provider(
        tool,
        &mut root,
        base_url,
        api_key,
        None,
        only_configured_models,
        protocol,
    );
    let models_configured = !model_ids.is_empty();
    let selected = root
        .get("model")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| value.strip_prefix(&format!("{ADDITIONAL_CONST_API_PROVIDER_ID}/")))
        .filter(|id| model_ids.iter().any(|candidate| candidate == id))
        .map(str::to_string)
        .or_else(|| model_ids.first().cloned());
    if let Some(selected) = selected {
        root["model"] = serde_json::json!(format!("{ADDITIONAL_CONST_API_PROVIDER_ID}/{selected}"));
    }
    let content = json_text(&root)?;
    let mut result = ToolApplyBuilder::default();
    observe_text(&path, &content, &mut result)?;
    let mut result = result.finish(tool);
    result.already_configured &= models_configured;
    attach_tool_protocol(&mut result, protocol);
    Ok(result)
}

fn qwen_model_entries(models: &[ToolModelInfo], base_url: &str) -> Vec<serde_json::Value> {
    models
        .iter()
        .map(|model| {
            let id = model.id.trim();
            let mut value = serde_json::json!({
                "id": id,
                "name": id,
                "envKey": ADDITIONAL_CONST_API_ENV_KEY,
                "baseUrl": tool_surface_url(base_url, "v1"),
                "capabilities": {"vision": model.input_modalities.iter().any(|modality| modality == "image")}
            });
            if let Some(context) = model.context_tokens.filter(|value| *value > 0) {
                value["generationConfig"] = serde_json::json!({
                    "contextWindowSize": context
                });
            }
            value
        })
        .collect()
}

fn upsert_qwencode_common(
    root: &mut serde_json::Value,
    base_url: &str,
    api_key: &str,
    replacement_models: Option<Vec<serde_json::Value>>,
) -> Vec<String> {
    ensure_json_object(root);
    ensure_json_object_field(root, "modelProviders");
    ensure_json_object_field(root, "providerProtocol");
    ensure_json_object_field(root, "env");
    if let Some(models) = replacement_models {
        root["modelProviders"][ADDITIONAL_CONST_API_PROVIDER_ID] = serde_json::json!(models);
    }
    let base_url = tool_surface_url(base_url, "v1");
    let mut models = root["modelProviders"]
        .get(ADDITIONAL_CONST_API_PROVIDER_ID)
        .and_then(serde_json::Value::as_array)
        .map(|models| {
            models
                .iter()
                .filter_map(|model| {
                    let mut model = model.as_object()?.clone();
                    let id = model.get("id")?.as_str()?.trim().to_string();
                    if id.is_empty() || id == "code-cheap" || id == LOCAL_PLACEHOLDER_KEY {
                        return None;
                    }
                    model.insert(
                        "envKey".to_string(),
                        serde_json::json!(ADDITIONAL_CONST_API_ENV_KEY),
                    );
                    model.insert("baseUrl".to_string(), serde_json::json!(base_url));
                    Some((id, serde_json::Value::Object(model)))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut seen = HashSet::new();
    models.retain(|(id, _)| seen.insert(id.clone()));
    root["modelProviders"][ADDITIONAL_CONST_API_PROVIDER_ID] =
        serde_json::json!(models.iter().map(|(_, model)| model).collect::<Vec<_>>());
    root["providerProtocol"][ADDITIONAL_CONST_API_PROVIDER_ID] = serde_json::json!("openai");
    root["env"][ADDITIONAL_CONST_API_ENV_KEY] = serde_json::json!(api_key);
    ensure_json_object_field(root, "security");
    ensure_json_object_field(&mut root["security"], "auth");
    root["security"]["auth"]["selectedType"] = serde_json::json!("openai");
    ensure_json_object_field(root, "model");
    models.into_iter().map(|(id, _)| id).collect()
}

fn apply_qwencode_config(
    base_url: &str,
    api_key: &str,
    models: &[ToolModelInfo],
) -> Result<ToolApplyResult> {
    let models = configured_additional_tool_models("Qwen Code", models)?;
    let path = qwencode_config_path();
    let mut root = read_json_or_default(&path, serde_json::json!({}))?;
    let entries = qwen_model_entries(&models, base_url);
    let model_ids = upsert_qwencode_common(&mut root, base_url, api_key, Some(entries));
    let base_url = tool_surface_url(base_url, "v1");
    root["model"]["name"] = serde_json::json!(model_ids[0]);
    root["model"]["baseUrl"] = serde_json::json!(base_url);
    let mut result = ToolApplyBuilder::default();
    write_json_with_backup(&path, &root, "qwencode", &mut result)?;
    let mut result = result.finish("qwencode");
    attach_tool_protocol(&mut result, ToolProtocol::OpenAiChat);
    Ok(result)
}

fn check_qwencode_config(base_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    let path = qwencode_config_path();
    let mut root = read_json_or_default(&path, serde_json::json!({}))?;
    let model_ids = upsert_qwencode_common(&mut root, base_url, api_key, None);
    let models_configured = !model_ids.is_empty();
    let local_base_url = tool_surface_url(base_url, "v1");
    let selected = root
        .get("model")
        .and_then(|model| model.get("name"))
        .and_then(serde_json::Value::as_str)
        .filter(|id| model_ids.iter().any(|candidate| candidate == id))
        .map(str::to_string)
        .or_else(|| model_ids.first().cloned());
    if let Some(selected) = selected {
        root["model"]["name"] = serde_json::json!(selected);
        root["model"]["baseUrl"] = serde_json::json!(local_base_url);
    }
    let mut result = ToolApplyBuilder::default();
    observe_json(&path, &root, &mut result)?;
    let mut result = result.finish("qwencode");
    result.already_configured &= models_configured;
    attach_tool_protocol(&mut result, ToolProtocol::OpenAiChat);
    Ok(result)
}

fn env_file_value<'a>(raw: &'a str, name: &str) -> Option<&'a str> {
    raw.lines().find_map(|line| {
        let line = line.trim();
        if line.starts_with('#') {
            return None;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let (key, value) = line.split_once('=')?;
        (key.trim() == name).then(|| value.trim())
    })
}

fn apply_vibe_trading_config(
    base_url: &str,
    api_key: &str,
    models: &[ToolModelInfo],
) -> Result<ToolApplyResult> {
    let models = configured_additional_tool_models("Vibe-Trading", models)?;
    let path = vibe_trading_config_path();
    let raw = read_text_or_empty(&path)?;
    let model = models[0].id.trim();
    let base_url = tool_surface_url(base_url, "v1");
    let content = upsert_env_vars(
        &raw,
        &[
            ("LANGCHAIN_PROVIDER", "openai"),
            ("LANGCHAIN_MODEL_NAME", model),
            ("OPENAI_BASE_URL", &base_url),
            ("OPENAI_API_KEY", api_key),
        ],
    );
    let mut result = ToolApplyBuilder::default();
    write_text_with_backup(&path, &content, "vibe-trading", &mut result)?;
    let mut result = result.finish("vibe-trading");
    attach_tool_protocol(&mut result, ToolProtocol::OpenAiChat);
    Ok(result)
}

fn check_vibe_trading_config(base_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    let path = vibe_trading_config_path();
    let raw = read_text_or_empty(&path)?;
    let models_configured = env_file_value(&raw, "LANGCHAIN_MODEL_NAME").is_some_and(|model| {
        !model.is_empty() && model != "code-cheap" && model != LOCAL_PLACEHOLDER_KEY
    });
    let base_url = tool_surface_url(base_url, "v1");
    let content = upsert_env_vars(
        &raw,
        &[
            ("LANGCHAIN_PROVIDER", "openai"),
            ("OPENAI_BASE_URL", &base_url),
            ("OPENAI_API_KEY", api_key),
        ],
    );
    let mut result = ToolApplyBuilder::default();
    observe_text(&path, &content, &mut result)?;
    let mut result = result.finish("vibe-trading");
    result.already_configured &= models_configured;
    attach_tool_protocol(&mut result, ToolProtocol::OpenAiChat);
    Ok(result)
}

fn upsert_zcode_provider(
    root: &mut serde_json::Value,
    base_url: &str,
    api_key: &str,
    replacement_models: Option<serde_json::Map<String, serde_json::Value>>,
) -> Vec<String> {
    ensure_json_object_field(root, "provider");
    let mut provider = root["provider"]
        .get(ADDITIONAL_CONST_API_PROVIDER_ID)
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    ensure_json_object(&mut provider);
    provider["name"] = serde_json::json!(CONST_API_DISPLAY_NAME);
    provider["kind"] = serde_json::json!("openai-compatible");
    provider["enabled"] = serde_json::json!(true);
    provider["source"] = serde_json::json!("custom");
    if provider
        .get("options")
        .is_none_or(|value| !value.is_object())
    {
        provider["options"] = serde_json::json!({});
    }
    provider["options"]["baseURL"] = serde_json::json!(tool_surface_url(base_url, "v1"));
    provider["options"]["apiKey"] = serde_json::json!(api_key);
    if let Some(models) = replacement_models {
        provider["models"] = serde_json::Value::Object(models);
    }
    let mut model_ids = provider
        .get("models")
        .and_then(serde_json::Value::as_object)
        .map(|models| {
            models
                .keys()
                .filter(|id| {
                    let id = id.trim();
                    !id.is_empty() && id != "code-cheap" && id != LOCAL_PLACEHOLDER_KEY
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    model_ids.sort();
    root["provider"][ADDITIONAL_CONST_API_PROVIDER_ID] = provider;
    model_ids
}

fn apply_zcode_config(
    base_url: &str,
    api_key: &str,
    models: &[ToolModelInfo],
) -> Result<ToolApplyResult> {
    let models = configured_additional_tool_models("ZCode", models)?;
    let path = zcode_config_path();
    let mut root = read_json_or_default(&path, serde_json::json!({}))?;
    ensure_json_object(&mut root);
    upsert_zcode_provider(
        &mut root,
        base_url,
        api_key,
        Some(opencode_models_object(&models)),
    );
    root["model"] = serde_json::json!(format!(
        "{ADDITIONAL_CONST_API_PROVIDER_ID}/{}",
        models[0].id
    ));
    let content = json_text_with_model_order(
        &root,
        &["provider", ADDITIONAL_CONST_API_PROVIDER_ID, "models"],
        &models,
    )?;
    let mut result = ToolApplyBuilder::default();
    write_text_with_backup(&path, &content, "zcode", &mut result)?;
    let mut result = result.finish("zcode");
    attach_tool_protocol(&mut result, ToolProtocol::OpenAiChat);
    Ok(result)
}

fn check_zcode_config(base_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    let path = zcode_config_path();
    let mut root = read_json_or_default(&path, serde_json::json!({}))?;
    ensure_json_object(&mut root);
    let model_ids = upsert_zcode_provider(&mut root, base_url, api_key, None);
    let models_configured = !model_ids.is_empty();
    let selected = root
        .get("model")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| value.strip_prefix(&format!("{ADDITIONAL_CONST_API_PROVIDER_ID}/")))
        .filter(|id| model_ids.iter().any(|candidate| candidate == id))
        .map(str::to_string)
        .or_else(|| model_ids.first().cloned());
    if let Some(selected) = selected {
        root["model"] = serde_json::json!(format!("{ADDITIONAL_CONST_API_PROVIDER_ID}/{selected}"));
    }
    let mut result = ToolApplyBuilder::default();
    observe_json(&path, &root, &mut result)?;
    let mut result = result.finish("zcode");
    result.already_configured &= models_configured;
    attach_tool_protocol(&mut result, ToolProtocol::OpenAiChat);
    Ok(result)
}

fn apply_copilot_config(
    base_url: &str,
    api_key: &str,
    models: &[ToolModelInfo],
) -> Result<ToolApplyResult> {
    let models = configured_additional_tool_models("GitHub Copilot CLI", models)?;
    configure_copilot(base_url, api_key, Some(models[0].id.trim()))
}

fn copilot_providers_path() -> PathBuf {
    copilot_env_path().with_file_name("const-api.providers.json")
}

fn check_copilot_config(base_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    configure_copilot(base_url, api_key, None)
}

fn configure_copilot(
    base_url: &str,
    api_key: &str,
    selected_model: Option<&str>,
) -> Result<ToolApplyResult> {
    let path = copilot_env_path();
    let registry_path = copilot_providers_path();
    let raw = read_text_or_empty(&path)?;
    let model = selected_model
        .map(str::to_string)
        .or_else(|| env_value(&raw, "COPILOT_MODEL"))
        .unwrap_or_default();
    let base_url = tool_surface_url(base_url, "v1");
    let content = upsert_env_vars(
        &raw,
        &[
            ("COPILOT_PROVIDER_TYPE", "openai"),
            ("COPILOT_PROVIDER_BASE_URL", &base_url),
            ("COPILOT_PROVIDER_API_KEY", api_key),
            ("COPILOT_MODEL", &model),
        ],
    );
    // A non-empty providers.json supersedes COPILOT_PROVIDER_* in newer CLIs.
    // Use a private empty registry only for our launch so the documented legacy
    // environment remains effective on both old and new CLI versions. Never
    // replace the user's own registry or guess the new provider entry schema.
    let registry = serde_json::json!({ "providers": {}, "models": {} });
    let configure = || -> Result<ToolApplyResult> {
        let mut result = ToolApplyBuilder::default();
        if selected_model.is_some() {
            write_text_with_backup(&path, &content, "copilot", &mut result)?;
            write_text_with_backup(
                &registry_path,
                &json_text(&registry)?,
                "copilot",
                &mut result,
            )?;
        } else {
            observe_text(&path, &content, &mut result)?;
            observe_json(&registry_path, &registry, &mut result)?;
        }
        let mut result = result.finish("copilot");
        result.already_configured &= !model.is_empty();
        result.details.insert(
            "configuration_scope".into(),
            "const_api_launch_environment".into(),
        );
        attach_tool_protocol(&mut result, ToolProtocol::OpenAiChat);
        Ok(result)
    };
    if selected_model.is_some() {
        with_tool_config_file_transaction(
            "copilot",
            &[path.clone(), registry_path.clone()],
            configure,
        )
    } else {
        configure()
    }
}

fn upsert_raven_config(
    root: &mut serde_json::Value,
    base_url: &str,
    api_key: &str,
    models: &[String],
) {
    // Raven's provider writer persists picker entries in its canonical
    // provider-qualified form. The custom gateway strips this public prefix
    // before routing the request through LiteLLM's OpenAI driver.
    let models = models
        .iter()
        .map(|model| {
            let model = model.trim();
            if model.starts_with("custom/") {
                model.to_string()
            } else {
                format!("custom/{model}")
            }
        })
        .collect::<Vec<_>>();
    ensure_json_object(root);
    ensure_json_object_field(root, "agents");
    ensure_json_object_field(&mut root["agents"], "defaults");
    root["agents"]["defaults"]["provider"] = serde_json::json!("custom");
    if let Some(first) = models.first() {
        root["agents"]["defaults"]["model"] = serde_json::json!(first);
    }
    ensure_json_object_field(root, "providers");
    if root["providers"]
        .get("custom")
        .is_none_or(|value| !value.is_object())
    {
        root["providers"]["custom"] = serde_json::json!({});
    }
    root["providers"]["custom"]["apiKey"] = serde_json::json!(api_key);
    root["providers"]["custom"]["apiBase"] = serde_json::json!(tool_surface_url(base_url, "v1"));
    root["providers"]["custom"]["models"] = serde_json::json!(models);
}

fn raven_model_ids(root: &serde_json::Value) -> Vec<String> {
    root.pointer("/providers/custom/models")
        .and_then(serde_json::Value::as_array)
        .map(|models| {
            models
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn apply_raven_config(
    base_url: &str,
    api_key: &str,
    models: &[ToolModelInfo],
) -> Result<ToolApplyResult> {
    let models = model_ids(&configured_additional_tool_models("Raven", models)?);
    let path = raven_config_path();
    let mut root = read_json_or_default(&path, serde_json::json!({}))?;
    upsert_raven_config(&mut root, base_url, api_key, &models);
    let mut result = ToolApplyBuilder::default();
    write_text_with_backup(&path, &json_text(&root)?, "raven", &mut result)?;
    let mut result = result.finish("raven");
    attach_tool_protocol(&mut result, ToolProtocol::OpenAiChat);
    Ok(result)
}

fn check_raven_config(base_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    let path = raven_config_path();
    let mut root = read_json_or_default(&path, serde_json::json!({}))?;
    let models = raven_model_ids(&root);
    upsert_raven_config(&mut root, base_url, api_key, &models);
    let mut result = ToolApplyBuilder::default();
    observe_json(&path, &root, &mut result)?;
    let mut result = result.finish("raven");
    result.already_configured &= !models.is_empty();
    attach_tool_protocol(&mut result, ToolProtocol::OpenAiChat);
    Ok(result)
}

fn pi_protocol(root: &serde_json::Value) -> ToolProtocol {
    root.pointer("/providers/const-api/api")
        .and_then(serde_json::Value::as_str)
        .and_then(pi_sdk_protocol)
        .unwrap_or(ToolProtocol::OpenAiResponses)
}

fn upsert_pi_config(
    root: &mut serde_json::Value,
    base_url: &str,
    api_key: &str,
    models: Option<&[ToolModelInfo]>,
    protocol: ToolProtocol,
) -> Result<Vec<String>> {
    ensure_json_object(root);
    ensure_json_object_field(root, "providers");
    if root["providers"]
        .get(ADDITIONAL_CONST_API_PROVIDER_ID)
        .is_none_or(|value| !value.is_object())
    {
        root["providers"][ADDITIONAL_CONST_API_PROVIDER_ID] = serde_json::json!({});
    }
    let provider = &mut root["providers"][ADDITIONAL_CONST_API_PROVIDER_ID];
    provider["baseUrl"] = serde_json::json!(pi_sdk_base_url(base_url, protocol));
    provider["api"] = serde_json::json!(pi_sdk_api_mode(protocol));
    provider["apiKey"] = serde_json::json!(api_key);
    if let Some(models) = models {
        provider["models"] = serde_json::Value::Array(
            models
                .iter()
                .map(|model| {
                    let id = model.id.trim();
                    let selected = tool_model_protocol("pi", model, protocol);
                    let mut entry = serde_json::json!({
                        "id": id,
                        "api": pi_sdk_api_mode(selected),
                        "baseUrl": pi_sdk_base_url(base_url, selected),
                        "name": if model.display_name.trim().is_empty() { id } else { model.display_name.trim() },
                        "contextWindow": model.context_tokens.unwrap_or(128_000),
                        "maxTokens": model.output_tokens.unwrap_or(16_384),
                        "reasoning": model.reasoning,
                        "input": if model.input_modalities.iter().any(|value| value == "image") {
                            vec!["text", "image"]
                        } else {
                            vec!["text"]
                        },
                    });
                    if let Some(levels) = pi_thinking_level_map(model) {
                        entry["thinkingLevelMap"] = levels;
                    }
                    entry
                })
                .collect(),
        );
    } else if let Some(models) = provider
        .get_mut("models")
        .and_then(serde_json::Value::as_array_mut)
    {
        for model in models {
            if let Some(selected) = model
                .get("api")
                .and_then(serde_json::Value::as_str)
                .and_then(pi_sdk_protocol)
            {
                model["baseUrl"] = serde_json::json!(pi_sdk_base_url(base_url, selected));
            }
        }
    }
    Ok(provider
        .get("models")
        .and_then(serde_json::Value::as_array)
        .map(|models| {
            models
                .iter()
                .filter_map(|model| model.get("id").and_then(serde_json::Value::as_str))
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default())
}

fn pi_thinking_level_map(model: &ToolModelInfo) -> Option<serde_json::Value> {
    let efforts = declared_reasoning_efforts(model);
    if efforts.is_empty() {
        return None;
    }
    // Pi's UI has a fixed ladder, while values are provider-native strings.
    // null disables a level; omission would enable Pi's guessed defaults.
    let mut levels = serde_json::Map::new();
    for level in ["off", "minimal", "low", "medium", "high", "xhigh", "max"] {
        let wire = if level == "off" { "none" } else { level };
        let value = if efforts.iter().any(|effort| effort == wire) {
            serde_json::json!(wire)
        } else if level == "max" && efforts.iter().any(|effort| effort == "ultra") {
            serde_json::json!("ultra")
        } else {
            serde_json::Value::Null
        };
        levels.insert(level.into(), value);
    }
    Some(levels.into())
}

fn apply_pi_config(
    base_url: &str,
    api_key: &str,
    models: &[ToolModelInfo],
    protocol: ToolProtocol,
) -> Result<ToolApplyResult> {
    let models = configured_additional_tool_models("Pi", models)?;
    let path = pi_models_path();
    let mut root = read_json_or_default(&path, serde_json::json!({}))?;
    upsert_pi_config(&mut root, base_url, api_key, Some(&models), protocol)?;
    let mut result = ToolApplyBuilder::default();
    write_text_with_backup(&path, &json_text(&root)?, "pi", &mut result)?;
    let mut result = result.finish("pi");
    attach_tool_protocol(&mut result, protocol);
    Ok(result)
}

fn check_pi_config(base_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    let path = pi_models_path();
    let mut root = read_json_or_default(&path, serde_json::json!({}))?;
    let protocol = pi_protocol(&root);
    let models = upsert_pi_config(&mut root, base_url, api_key, None, protocol)?;
    let mut result = ToolApplyBuilder::default();
    observe_json(&path, &root, &mut result)?;
    let mut result = result.finish("pi");
    result.already_configured &= !models.is_empty();
    attach_tool_protocol(&mut result, protocol);
    Ok(result)
}

fn cline_protocol_name(protocol: ToolProtocol) -> Result<(&'static str, &'static str)> {
    match protocol {
        ToolProtocol::OpenAiResponses => Ok(("openai-responses", "openai")),
        ToolProtocol::OpenAiChat => Ok(("openai-chat", "openai-compatible")),
        _ => Err(anyhow!(
            "Cline does not support tool protocol {}",
            protocol.as_str()
        )),
    }
}

fn cline_protocol_from_root(root: &serde_json::Value) -> ToolProtocol {
    match root
        .pointer("/providers/const-api/settings/protocol")
        .and_then(serde_json::Value::as_str)
    {
        Some("openai-chat") => ToolProtocol::OpenAiChat,
        _ => ToolProtocol::OpenAiResponses,
    }
}

fn upsert_cline_configs(
    providers: &mut serde_json::Value,
    catalog: &mut serde_json::Value,
    base_url: &str,
    api_key: &str,
    models: Option<&[ToolModelInfo]>,
    protocol: ToolProtocol,
) -> Result<Vec<String>> {
    let (protocol_name, client) = cline_protocol_name(protocol)?;
    ensure_json_object(providers);
    providers["version"] = serde_json::json!(1);
    providers["lastUsedProvider"] = serde_json::json!(ADDITIONAL_CONST_API_PROVIDER_ID);
    ensure_json_object_field(providers, "providers");
    let previous = providers["providers"]
        .get(ADDITIONAL_CONST_API_PROVIDER_ID)
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let mut entry = previous;
    ensure_json_object(&mut entry);
    let previous_settings = entry
        .get("settings")
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let current_model = previous_settings
        .get("model")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let model_ids = models.map(model_ids).unwrap_or_else(|| {
        catalog
            .pointer("/providers/const-api/models")
            .and_then(serde_json::Value::as_object)
            .map(|models| models.keys().cloned().collect())
            .unwrap_or_default()
    });
    let selected = current_model
        .filter(|model| model_ids.iter().any(|candidate| candidate == model))
        .or_else(|| model_ids.first().cloned())
        .unwrap_or_default();
    let mut settings = previous_settings;
    ensure_json_object(&mut settings);
    settings["provider"] = serde_json::json!(ADDITIONAL_CONST_API_PROVIDER_ID);
    settings["baseUrl"] = serde_json::json!(tool_surface_url(base_url, "v1"));
    settings["model"] = serde_json::json!(selected);
    settings["protocol"] = serde_json::json!(protocol_name);
    settings["apiKey"] = serde_json::json!(api_key);
    settings["capabilities"] = serde_json::json!(["reasoning", "tools"]);
    entry["settings"] = settings;
    entry["tokenSource"] = serde_json::json!("manual");
    // Required by Cline's provider registry schema. Preserve a valid timestamp
    // so checking an unchanged configuration does not request another restart.
    if !entry
        .get("updatedAt")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| {
            value.ends_with('Z')
                && time::OffsetDateTime::parse(
                    value,
                    &time::format_description::well_known::Rfc3339,
                )
                .is_ok()
        })
    {
        entry["updatedAt"] = serde_json::json!(
            time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)?
        );
    }
    providers["providers"][ADDITIONAL_CONST_API_PROVIDER_ID] = entry;

    ensure_json_object(catalog);
    catalog["version"] = serde_json::json!(1);
    ensure_json_object_field(catalog, "providers");
    let mut catalog_entry = catalog["providers"]
        .get(ADDITIONAL_CONST_API_PROVIDER_ID)
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    ensure_json_object(&mut catalog_entry);
    let mut provider_info = catalog_entry
        .get("provider")
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    ensure_json_object(&mut provider_info);
    provider_info["name"] = serde_json::json!(CONST_API_DISPLAY_NAME);
    provider_info["baseUrl"] = serde_json::json!(tool_surface_url(base_url, "v1"));
    provider_info["defaultModelId"] = serde_json::json!(selected);
    provider_info["protocol"] = serde_json::json!(protocol_name);
    provider_info["client"] = serde_json::json!(client);
    provider_info["capabilities"] = serde_json::json!(["reasoning", "tools"]);
    catalog_entry["provider"] = provider_info;
    if let Some(models) = models {
        let mut configured = serde_json::Map::new();
        for model in models {
            let id = model.id.trim();
            let mut capabilities = vec!["streaming"];
            if model.tool_call {
                capabilities.push("tools");
            }
            if model.reasoning {
                capabilities.push("reasoning");
            }
            // Cline's menu consumes reasoningOptions, not just `reasoning`.
            // Its schema is a closed enum: unknown levels must not invalidate
            // the complete provider catalog (e.g. `ultra` on older clients).
            let efforts = declared_reasoning_efforts(model)
                .into_iter()
                .filter(|effort| {
                    matches!(
                        effort.as_str(),
                        "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
                    )
                })
                .collect::<Vec<_>>();
            if !efforts.is_empty() {
                capabilities.push("reasoning-effort");
            }
            if model.input_modalities.iter().any(|value| value == "image") {
                capabilities.push("images");
            }
            if model
                .input_modalities
                .iter()
                .any(|value| value == "file" || value == "pdf")
            {
                capabilities.push("files");
            }
            let mut entry = serde_json::json!({
                "id": id,
                "name": if model.display_name.trim().is_empty() { id } else { model.display_name.trim() },
                "contextWindow": model.context_tokens.unwrap_or(128_000),
                "maxTokens": model.output_tokens.unwrap_or(16_384),
                "capabilities": capabilities,
            });
            if !efforts.is_empty() {
                entry["reasoningOptions"] =
                    serde_json::json!([{"type":"effort", "values":efforts}]);
            }
            configured.insert(id.to_string(), entry);
        }
        catalog_entry["models"] = serde_json::Value::Object(configured);
    }
    catalog["providers"][ADDITIONAL_CONST_API_PROVIDER_ID] = catalog_entry;
    Ok(model_ids)
}

fn apply_cline_config(
    base_url: &str,
    api_key: &str,
    models: &[ToolModelInfo],
    protocol: ToolProtocol,
) -> Result<ToolApplyResult> {
    let models = configured_additional_tool_models("Cline", models)?;
    let (providers_path, catalog_path) = cline_provider_paths()?;
    let paths = [providers_path.clone(), catalog_path.clone()];
    with_tool_config_file_transaction("cline", &paths, || {
        let mut providers = read_json_or_default(&providers_path, serde_json::json!({}))?;
        let mut catalog = read_json_or_default(&catalog_path, serde_json::json!({}))?;
        upsert_cline_configs(
            &mut providers,
            &mut catalog,
            base_url,
            api_key,
            Some(&models),
            protocol,
        )?;
        let mut result = ToolApplyBuilder::default();
        write_text_with_backup(
            &providers_path,
            &json_text(&providers)?,
            "cline",
            &mut result,
        )?;
        write_text_with_backup(
            &catalog_path,
            &json_text_with_model_order(
                &catalog,
                &["providers", ADDITIONAL_CONST_API_PROVIDER_ID, "models"],
                &models,
            )?,
            "cline",
            &mut result,
        )?;
        let mut result = result.finish("cline");
        attach_tool_protocol(&mut result, protocol);
        Ok(result)
    })
}

fn check_cline_config(base_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    let (providers_path, catalog_path) = cline_provider_paths()?;
    let mut providers = read_json_or_default(&providers_path, serde_json::json!({}))?;
    let mut catalog = read_json_or_default(&catalog_path, serde_json::json!({}))?;
    let protocol = cline_protocol_from_root(&providers);
    let models = upsert_cline_configs(
        &mut providers,
        &mut catalog,
        base_url,
        api_key,
        None,
        protocol,
    )?;
    let mut result = ToolApplyBuilder::default();
    observe_json(&providers_path, &providers, &mut result)?;
    observe_json(&catalog_path, &catalog, &mut result)?;
    let mut result = result.finish("cline");
    result.already_configured &= !models.is_empty();
    attach_tool_protocol(&mut result, protocol);
    Ok(result)
}

fn upsert_named_toml_provider(
    doc: &mut DocumentMut,
    base_url: &str,
    models: &[String],
    model_info: Option<&[ToolModelInfo]>,
) -> Result<()> {
    doc["default_model"] = toml_value(ADDITIONAL_CONST_API_PROVIDER_ID);
    if !doc.contains_key("providers") || !doc["providers"].is_array_of_tables() {
        doc["providers"] = Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }
    let providers = doc["providers"]
        .as_array_of_tables_mut()
        .ok_or_else(|| anyhow!("Reasonix providers is not an array of tables"))?;
    let mut provider = providers
        .iter()
        .find(|provider| {
            provider.get("name").and_then(Item::as_str) == Some(ADDITIONAL_CONST_API_PROVIDER_ID)
        })
        .cloned()
        .unwrap_or_default();
    providers.retain(|provider| {
        provider.get("name").and_then(Item::as_str) != Some(ADDITIONAL_CONST_API_PROVIDER_ID)
    });
    provider["name"] = toml_value(ADDITIONAL_CONST_API_PROVIDER_ID);
    provider["kind"] = toml_value("openai");
    provider["base_url"] = toml_value(tool_surface_url(base_url, "v1"));
    let mut model_array = toml_edit::Array::new();
    for model in models {
        model_array.push(model.as_str());
    }
    provider["models"] = Item::Value(toml_edit::Value::Array(model_array));
    provider["default"] = toml_value(models.first().cloned().unwrap_or_default());
    provider["api_key_env"] = toml_value(ADDITIONAL_CONST_API_ENV_KEY);
    if let Some(models) = model_info {
        let mut overrides = Table::new();
        for model in models {
            let id = model.id.trim();
            let mut entry = match provider
                .get("model_overrides")
                .and_then(|value| value.get(id))
            {
                Some(value) => value
                    .clone()
                    .into_table()
                    .map_err(|_| anyhow!("Reasonix model override {id} must be a table"))?,
                None => Table::new(),
            };
            for (field, value) in [
                ("context_window", model.context_tokens),
                ("max_output_tokens", model.output_tokens),
            ] {
                if let Some(value) = value.filter(|value| *value > 0) {
                    entry[field] = toml_value(
                        i64::try_from(value).context("Reasonix model token limit exceeds i64")?,
                    );
                } else {
                    entry.remove(field);
                }
            }
            entry["vision"] =
                toml_value(model.input_modalities.iter().any(|value| value == "image"));
            let efforts = declared_reasoning_efforts(model);
            if efforts.is_empty() {
                entry.remove("supported_efforts");
                entry.remove("default_effort");
            } else {
                let mut values = toml_edit::Array::new();
                for effort in &efforts {
                    values.push(effort.as_str());
                }
                entry["supported_efforts"] = Item::Value(toml_edit::Value::Array(values));
                if !entry
                    .get("default_effort")
                    .and_then(Item::as_str)
                    .is_some_and(|value| efforts.iter().any(|effort| effort == value))
                {
                    entry.remove("default_effort");
                }
            }
            // Keep Reasonix's model-specific wire normalization for reasoning
            // models. Only explicitly non-reasoning models disable its guessing.
            if !model.reasoning {
                entry["reasoning_protocol"] = toml_value("none");
            } else if entry.get("reasoning_protocol").and_then(Item::as_str) == Some("none") {
                entry.remove("reasoning_protocol");
            }
            overrides[id] = Item::Table(entry);
        }
        provider["model_overrides"] = Item::Table(overrides);
    }
    providers.push(provider);
    Ok(())
}

fn reasonix_models(doc: &DocumentMut) -> Vec<String> {
    doc.get("providers")
        .and_then(Item::as_array_of_tables)
        .and_then(|providers| {
            providers.iter().find(|provider| {
                provider.get("name").and_then(Item::as_str)
                    == Some(ADDITIONAL_CONST_API_PROVIDER_ID)
            })
        })
        .and_then(|provider| provider.get("models"))
        .and_then(Item::as_array)
        .map(|models| {
            models
                .iter()
                .filter_map(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn apply_reasonix_config(
    base_url: &str,
    api_key: &str,
    models: &[ToolModelInfo],
) -> Result<ToolApplyResult> {
    let models = configured_additional_tool_models("DeepSeek Reasonix", models)?;
    let (config_path, env_path) = reasonix_config_paths();
    let paths = [config_path.clone(), env_path.clone()];
    with_tool_config_file_transaction("reasonix", &paths, || {
        let raw = read_text_or_empty(&config_path)?;
        let mut doc = if raw.trim().is_empty() {
            DocumentMut::new()
        } else {
            raw.parse::<DocumentMut>()
                .context("parse Reasonix config.toml")?
        };
        upsert_named_toml_provider(&mut doc, base_url, &model_ids(&models), Some(&models))?;
        let mut config = doc.to_string();
        if !config.ends_with('\n') {
            config.push('\n');
        }
        let env = upsert_env_vars(
            &read_text_or_empty(&env_path)?,
            &[(ADDITIONAL_CONST_API_ENV_KEY, api_key)],
        );
        let mut result = ToolApplyBuilder::default();
        write_text_with_backup(&config_path, &config, "reasonix", &mut result)?;
        write_text_with_backup(&env_path, &env, "reasonix", &mut result)?;
        let mut result = result.finish("reasonix");
        attach_tool_protocol(&mut result, ToolProtocol::OpenAiChat);
        Ok(result)
    })
}

fn check_reasonix_config(base_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    let (config_path, env_path) = reasonix_config_paths();
    let raw = read_text_or_empty(&config_path)?;
    let mut doc = if raw.trim().is_empty() {
        DocumentMut::new()
    } else {
        raw.parse::<DocumentMut>()
            .context("parse Reasonix config.toml")?
    };
    let models = reasonix_models(&doc);
    upsert_named_toml_provider(&mut doc, base_url, &models, None)?;
    let mut config = doc.to_string();
    if !config.ends_with('\n') {
        config.push('\n');
    }
    let env = upsert_env_vars(
        &read_text_or_empty(&env_path)?,
        &[(ADDITIONAL_CONST_API_ENV_KEY, api_key)],
    );
    let mut result = ToolApplyBuilder::default();
    observe_text(&config_path, &config, &mut result)?;
    observe_text(&env_path, &env, &mut result)?;
    let mut result = result.finish("reasonix");
    result.already_configured &= !models.is_empty();
    attach_tool_protocol(&mut result, ToolProtocol::OpenAiChat);
    Ok(result)
}

fn parse_deepseek_harness_yaml_mapping(
    raw: &str,
    document_name: &str,
) -> Result<serde_yaml::Mapping> {
    if raw.trim().is_empty() {
        return Ok(serde_yaml::Mapping::new());
    }
    match serde_yaml::from_str::<serde_yaml::Value>(raw)
        .with_context(|| format!("parse DeepSeek Harness {document_name}"))?
    {
        serde_yaml::Value::Mapping(root) => Ok(root),
        _ => Err(anyhow!(
            "DeepSeek Harness {document_name} must contain a YAML mapping"
        )),
    }
}

fn deepseek_harness_reasoning_efforts(model: &ToolModelInfo) -> Option<serde_yaml::Value> {
    if !model.reasoning {
        return Some(serde_yaml::Value::Bool(false));
    }
    let declared = model
        .reasoning_efforts
        .iter()
        .map(|effort| effort.trim().to_ascii_lowercase())
        .filter(|effort| !effort.is_empty())
        .collect::<BTreeSet<_>>();
    let mut efforts = serde_yaml::Mapping::new();
    if declared.contains("none") {
        efforts.insert(yaml_key("off"), yaml_string("none"));
    } else if declared.contains("off") {
        efforts.insert(yaml_key("off"), serde_yaml::Value::Null);
    }
    let mut has_thinking_level = false;
    for effort in ["minimal", "low", "medium", "high", "xhigh", "max"] {
        if declared.contains(effort) {
            efforts.insert(yaml_key(effort), yaml_string(effort));
            has_thinking_level = true;
        }
    }
    if !declared.contains("max") && declared.contains("ultra") {
        efforts.insert(yaml_key("max"), yaml_string("ultra"));
        has_thinking_level = true;
    }
    has_thinking_level.then_some(serde_yaml::Value::Mapping(efforts))
}

fn deepseek_harness_model_entry(model: &ToolModelInfo) -> serde_yaml::Value {
    let id = model.id.trim();
    let mut entry = serde_yaml::Mapping::new();
    entry.insert(yaml_key("id"), yaml_string(id));
    if !model.display_name.trim().is_empty() {
        entry.insert(yaml_key("name"), yaml_string(model.display_name.trim()));
    }
    if let Some(context) = model.context_tokens.filter(|value| *value > 0) {
        if let Ok(value) = serde_yaml::to_value(context) {
            entry.insert(yaml_key("contextWindow"), value);
        }
    }
    if let Some(output) = model.output_tokens.filter(|value| *value > 0) {
        if let Ok(value) = serde_yaml::to_value(output) {
            entry.insert(yaml_key("maxTokens"), value);
        }
    }
    let accepts_images = model
        .input_modalities
        .iter()
        .any(|modality| modality == "image");
    let mut input = vec![yaml_string("text")];
    if accepts_images {
        input.push(yaml_string("image"));
    }
    entry.insert(yaml_key("input"), serde_yaml::Value::Sequence(input));
    if let Some(efforts) = deepseek_harness_reasoning_efforts(model) {
        entry.insert(yaml_key("reasoningEfforts"), efforts);
    }
    serde_yaml::Value::Mapping(entry)
}

fn deepseek_harness_api(protocol: ToolProtocol) -> Result<&'static str> {
    match protocol {
        ToolProtocol::OpenAiChat => Ok("openai-completions"),
        ToolProtocol::OpenAiResponses => Ok("openai-responses"),
        ToolProtocol::AnthropicMessages => Ok("anthropic-messages"),
        _ => Err(anyhow!(
            "DeepSeek Harness does not support tool protocol {}",
            protocol.as_str()
        )),
    }
}

fn deepseek_harness_protocol(root: &serde_yaml::Mapping) -> ToolProtocol {
    match root
        .get(yaml_key("llm-pi-ai"))
        .and_then(serde_yaml::Value::as_mapping)
        .and_then(|llm| llm.get(yaml_key("providers")))
        .and_then(serde_yaml::Value::as_mapping)
        .and_then(|providers| providers.get(yaml_key(ADDITIONAL_CONST_API_PROVIDER_ID)))
        .and_then(serde_yaml::Value::as_mapping)
        .and_then(|provider| provider.get(yaml_key("api")))
        .and_then(serde_yaml::Value::as_str)
    {
        Some("openai-responses") => ToolProtocol::OpenAiResponses,
        Some("anthropic-messages") => ToolProtocol::AnthropicMessages,
        _ => ToolProtocol::OpenAiChat,
    }
}

fn upsert_deepseek_harness_settings(
    raw: &str,
    base_url: &str,
    models: Option<&[ToolModelInfo]>,
    default_model: Option<&str>,
    protocol: ToolProtocol,
) -> Result<String> {
    let mut root = parse_deepseek_harness_yaml_mapping(raw, "settings.yaml")?;
    let llm = ensure_yaml_mapping(&mut root, "llm-pi-ai")?;
    let providers = ensure_yaml_mapping(llm, "providers")?;
    let provider_key = yaml_key(ADDITIONAL_CONST_API_PROVIDER_ID);
    if !matches!(
        providers.get(&provider_key),
        Some(serde_yaml::Value::Mapping(_))
    ) {
        providers.insert(
            provider_key.clone(),
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        );
    }
    let provider = providers
        .get_mut(&provider_key)
        .and_then(serde_yaml::Value::as_mapping_mut)
        .ok_or_else(|| anyhow!("DeepSeek Harness provider is not a YAML mapping"))?;
    provider.insert(yaml_key("displayName"), yaml_string(CONST_API_DISPLAY_NAME));
    provider.insert(
        yaml_key("apiKeyEnv"),
        yaml_string(DEEPSEEK_HARNESS_CREDENTIAL_ENV_KEY),
    );
    provider.insert(
        yaml_key("api"),
        yaml_string(deepseek_harness_api(protocol)?),
    );
    provider.insert(
        yaml_key("baseURL"),
        yaml_string(&tool_surface_url(base_url, protocol.surface())),
    );
    if let Some(models) = models {
        provider.insert(
            yaml_key("models"),
            serde_yaml::Value::Sequence(models.iter().map(deepseek_harness_model_entry).collect()),
        );
    }

    if let Some(model) = default_model {
        let default = ensure_yaml_mapping(&mut root, "agent-default-model")?;
        default.insert(
            yaml_key("provider"),
            yaml_string(ADDITIONAL_CONST_API_PROVIDER_ID),
        );
        default.insert(yaml_key("model"), yaml_string(model));
    }

    let mut content = serde_yaml::to_string(&serde_yaml::Value::Mapping(root))?;
    if !content.ends_with('\n') {
        content.push('\n');
    }
    Ok(content)
}

fn deepseek_harness_model_ids(root: &serde_yaml::Mapping) -> Vec<String> {
    root.get(yaml_key("llm-pi-ai"))
        .and_then(serde_yaml::Value::as_mapping)
        .and_then(|llm| llm.get(yaml_key("providers")))
        .and_then(serde_yaml::Value::as_mapping)
        .and_then(|providers| providers.get(yaml_key(ADDITIONAL_CONST_API_PROVIDER_ID)))
        .and_then(serde_yaml::Value::as_mapping)
        .and_then(|provider| provider.get(yaml_key("models")))
        .and_then(serde_yaml::Value::as_sequence)
        .map(|models| {
            models
                .iter()
                .filter_map(|model| {
                    model
                        .as_mapping()?
                        .get(yaml_key("id"))?
                        .as_str()
                        .map(str::trim)
                        .filter(|id| !id.is_empty())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn deepseek_harness_default_selection(root: &serde_yaml::Mapping) -> (String, String) {
    let Some(default) = root
        .get(yaml_key("agent-default-model"))
        .and_then(serde_yaml::Value::as_mapping)
    else {
        return (String::new(), String::new());
    };
    let provider = default
        .get(yaml_key("provider"))
        .and_then(serde_yaml::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let model = default
        .get(yaml_key("model"))
        .and_then(serde_yaml::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    (provider, model)
}

fn upsert_deepseek_harness_credentials(raw: &str, api_key: &str) -> Result<String> {
    let mut root = parse_deepseek_harness_yaml_mapping(raw, ".credentials.yaml")?;
    let refs = if let Some(version) = root.get(yaml_key("version")) {
        // Harness now stores references under refs and keeps browser/OAuth
        // records alongside them. Only our reference belongs to this writer.
        if version.as_u64() != Some(1) {
            return Err(anyhow!(
                "DeepSeek Harness .credentials.yaml uses an unsupported format version"
            ));
        }
        if root
            .get(yaml_key("refs"))
            .is_some_and(|value| !value.is_null() && !value.is_mapping())
        {
            return Err(anyhow!(
                "DeepSeek Harness .credentials.yaml refs must be a YAML mapping"
            ));
        }
        ensure_yaml_mapping(&mut root, "refs")?
    } else {
        // Keep the flat layout for older Harness builds, including first-time
        // setup. New Harness builds migrate it themselves on startup.
        for (key, value) in &root {
            let valid = key.as_str().is_some_and(|key| !key.trim().is_empty())
                && value.as_str().is_some_and(|value| !value.trim().is_empty());
            if !valid {
                return Err(anyhow!(
                    "DeepSeek Harness .credentials.yaml must contain non-empty string values"
                ));
            }
        }
        &mut root
    };
    refs.insert(
        yaml_key(DEEPSEEK_HARNESS_CREDENTIAL_ENV_KEY),
        yaml_string(api_key),
    );
    let mut content = serde_yaml::to_string(&serde_yaml::Value::Mapping(root))?;
    if !content.ends_with('\n') {
        content.push('\n');
    }
    Ok(content)
}

fn normalize_deepseek_harness_credential_ownership(
    ownership: &mut ToolConfigOwnership,
    current: Option<&[u8]>,
) -> Result<()> {
    if ownership.tool != "deepseek-harness" || ownership.format != ToolConfigFormat::Yaml {
        return Ok(());
    }
    let legacy_path = [ToolConfigFieldPath::Key(
        DEEPSEEK_HARNESS_CREDENTIAL_ENV_KEY.to_string(),
    )];
    let Some(field) = ownership
        .fields
        .iter_mut()
        .find(|field| field.path == legacy_path)
    else {
        return Ok(());
    };
    let document = parse_tool_config_document(ToolConfigFormat::Yaml, current)?;
    if document.get("version").and_then(serde_json::Value::as_u64) == Some(1) {
        // Harness's own flat -> refs migration moves our field too. Keep its
        // original value so reconfiguration/cancellation remains a three-way
        // restore, without undoing the migration or touching login records.
        field
            .path
            .insert(0, ToolConfigFieldPath::Key("refs".to_string()));
        ownership.whole_file_restore_safe = false;
    }
    Ok(())
}

fn apply_deepseek_harness_config(
    base_url: &str,
    api_key: &str,
    models: &[ToolModelInfo],
    protocol: ToolProtocol,
) -> Result<ToolApplyResult> {
    let models = configured_additional_tool_models("DeepSeek Harness", models)?;
    let default_model = models[0].id.trim();
    let (_, credentials_path) = deepseek_harness_config_paths();
    let settings_groups = deepseek_harness_settings_groups()?;
    let mut paths = Vec::new();
    for file in settings_groups.iter().flatten() {
        push_unique_path(&mut paths, file.path.clone());
    }
    paths.push(credentials_path.clone());
    with_tool_config_file_transaction("deepseek-harness", &paths, || {
        let mut settings = Vec::<(PathBuf, String)>::new();
        for files in &settings_groups {
            let updated = upsert_deepseek_harness_settings(
                &deepseek_harness_effective_settings(files)?,
                base_url,
                Some(&models),
                Some(default_model),
                protocol,
            )?;
            for file in files {
                if !settings.iter().any(|(path, _)| path == &file.path) {
                    settings.push((
                        file.path.clone(),
                        render_deepseek_harness_settings(file, &updated)?,
                    ));
                }
            }
        }
        let credentials =
            upsert_deepseek_harness_credentials(&read_text_or_empty(&credentials_path)?, api_key)?;
        let mut result = ToolApplyBuilder::default();
        for (path, content) in settings {
            write_text_with_backup(&path, &content, "deepseek-harness", &mut result)?;
        }
        write_text_with_backup(
            &credentials_path,
            &credentials,
            "deepseek-harness",
            &mut result,
        )?;
        let mut result = result.finish("deepseek-harness");
        result
            .details
            .insert("model_count".to_string(), models.len().to_string());
        attach_tool_protocol(&mut result, protocol);
        Ok(result)
    })
}

fn check_deepseek_harness_config(base_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    let (_, credentials_path) = deepseek_harness_config_paths();
    let settings_groups = deepseek_harness_settings_groups()?;
    let mut result = ToolApplyBuilder::default();
    let mut configured = true;
    let mut first_protocol = None;
    let mut model_count = 0;
    let mut observed = HashSet::new();
    for settings_files in &settings_groups {
        let raw_settings = deepseek_harness_effective_settings(settings_files)?;
        let root = parse_deepseek_harness_yaml_mapping(&raw_settings, "settings.yaml")?;
        let protocol = deepseek_harness_protocol(&root);
        let models = deepseek_harness_model_ids(&root);
        let (selected_provider, selected_model) = deepseek_harness_default_selection(&root);
        let default_is_configured = selected_provider == ADDITIONAL_CONST_API_PROVIDER_ID
            && models.iter().any(|model| model == &selected_model);
        let expected_default = if default_is_configured {
            selected_model.as_str()
        } else {
            models.first().map(String::as_str).unwrap_or_default()
        };
        let settings = upsert_deepseek_harness_settings(
            &raw_settings,
            base_url,
            None,
            Some(expected_default),
            protocol,
        )?;
        for file in settings_files {
            if !observed.insert(file.path.clone()) {
                continue;
            }
            observe_text(
                &file.path,
                &render_deepseek_harness_settings(file, &settings)?,
                &mut result,
            )?;
        }
        configured &= !models.is_empty() && default_is_configured;
        if let Some(first) = first_protocol {
            configured &= protocol == first;
        } else {
            first_protocol = Some(protocol);
            model_count = models.len();
        }
    }
    let credentials =
        upsert_deepseek_harness_credentials(&read_text_or_empty(&credentials_path)?, api_key)?;
    observe_text(&credentials_path, &credentials, &mut result)?;
    let mut result = result.finish("deepseek-harness");
    result.already_configured &= configured;
    result
        .details
        .insert("model_count".to_string(), model_count.to_string());
    attach_tool_protocol(
        &mut result,
        first_protocol.expect("Harness always has a settings group"),
    );
    Ok(result)
}

fn upsert_goose_provider_config(
    provider: &mut serde_json::Value,
    base_url: &str,
    models: &[ToolModelInfo],
) {
    ensure_json_object(provider);
    provider["name"] = serde_json::json!("const_api");
    provider["engine"] = serde_json::json!("openai");
    provider["display_name"] = serde_json::json!(CONST_API_DISPLAY_NAME);
    provider["description"] = serde_json::json!("CONST API OpenAI-compatible provider");
    provider["api_key_env"] = serde_json::json!(ADDITIONAL_CONST_API_ENV_KEY);
    provider["base_url"] = serde_json::json!(append_path(base_url, "chat/completions"));
    provider["models"] = serde_json::Value::Array(
        models
            .iter()
            .map(|model| {
                serde_json::json!({
                    "name": model.id.trim(),
                    "context_limit": model.context_tokens.unwrap_or(128_000),
                    "reasoning": model.reasoning,
                })
            })
            .collect(),
    );
    provider["supports_streaming"] = serde_json::json!(true);
    provider["requires_auth"] = serde_json::json!(true);
}

fn goose_models(provider: &serde_json::Value) -> Vec<ToolModelInfo> {
    provider
        .get("models")
        .and_then(serde_json::Value::as_array)
        .map(|models| {
            models
                .iter()
                .filter_map(|model| {
                    let id = model.get("name")?.as_str()?.trim();
                    (!id.is_empty()).then(|| ToolModelInfo {
                        id: id.to_string(),
                        context_tokens: model
                            .get("context_limit")
                            .and_then(serde_json::Value::as_u64),
                        reasoning: model
                            .get("reasoning")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false),
                        ..ToolModelInfo::default()
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn apply_goose_config(
    base_url: &str,
    api_key: &str,
    models: &[ToolModelInfo],
) -> Result<ToolApplyResult> {
    let models = configured_additional_tool_models("Goose", models)?;
    let (provider_path, env_path) = goose_config_paths();
    let paths = [provider_path.clone(), env_path.clone()];
    with_tool_config_file_transaction("goose", &paths, || {
        let mut provider = read_json_or_default(&provider_path, serde_json::json!({}))?;
        upsert_goose_provider_config(&mut provider, base_url, &models);
        let env = upsert_env_vars(
            &read_text_or_empty(&env_path)?,
            &[
                ("GOOSE_PROVIDER", "const_api"),
                ("GOOSE_MODEL", models[0].id.trim()),
                (ADDITIONAL_CONST_API_ENV_KEY, api_key),
            ],
        );
        let mut result = ToolApplyBuilder::default();
        write_text_with_backup(&provider_path, &json_text(&provider)?, "goose", &mut result)?;
        write_text_with_backup(&env_path, &env, "goose", &mut result)?;
        let mut result = result.finish("goose");
        result.details.insert(
            "configuration_scope".to_string(),
            "provider_and_const_api_launch_environment".to_string(),
        );
        attach_tool_protocol(&mut result, ToolProtocol::OpenAiChat);
        Ok(result)
    })
}

fn check_goose_config(base_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    let (provider_path, env_path) = goose_config_paths();
    let mut current = read_json_or_default(&provider_path, serde_json::json!({}))?;
    let models = goose_models(&current);
    upsert_goose_provider_config(&mut current, base_url, &models);
    let raw_env = read_text_or_empty(&env_path)?;
    let model = env_value(&raw_env, "GOOSE_MODEL")
        .filter(|selected| models.iter().any(|candidate| candidate.id == *selected))
        .or_else(|| models.first().map(|model| model.id.clone()))
        .unwrap_or_default();
    let env = upsert_env_vars(
        &raw_env,
        &[
            ("GOOSE_PROVIDER", "const_api"),
            ("GOOSE_MODEL", &model),
            (ADDITIONAL_CONST_API_ENV_KEY, api_key),
        ],
    );
    let mut result = ToolApplyBuilder::default();
    observe_json(&provider_path, &current, &mut result)?;
    observe_text(&env_path, &env, &mut result)?;
    let mut result = result.finish("goose");
    result.already_configured &= !models.is_empty() && !model.is_empty();
    result.details.insert(
        "configuration_scope".to_string(),
        "provider_and_const_api_launch_environment".to_string(),
    );
    attach_tool_protocol(&mut result, ToolProtocol::OpenAiChat);
    Ok(result)
}

fn upsert_mistral_vibe_toml(raw: &str, base_url: &str, models: &[ToolModelInfo]) -> Result<String> {
    let mut doc = if raw.trim().is_empty() {
        DocumentMut::new()
    } else {
        raw.parse::<DocumentMut>()
            .context("parse Mistral Vibe config.toml")?
    };
    let first_alias = format!(
        "{ADDITIONAL_CONST_API_PROVIDER_ID}/{}",
        models
            .first()
            .map(|model| model.id.trim())
            .unwrap_or_default()
    );
    doc["active_model"] = toml_value(first_alias);
    if !doc.contains_key("providers") || !doc["providers"].is_array_of_tables() {
        doc["providers"] = Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }
    let providers = doc["providers"]
        .as_array_of_tables_mut()
        .ok_or_else(|| anyhow!("Mistral Vibe providers is not an array of tables"))?;
    providers.retain(|provider| {
        provider.get("name").and_then(Item::as_str) != Some(ADDITIONAL_CONST_API_PROVIDER_ID)
    });
    let mut provider = Table::new();
    provider["name"] = toml_value(ADDITIONAL_CONST_API_PROVIDER_ID);
    provider["api_base"] = toml_value(tool_surface_url(base_url, "v1"));
    provider["api_key_env_var"] = toml_value(ADDITIONAL_CONST_API_ENV_KEY);
    provider["api_style"] = toml_value("openai");
    provider["backend"] = toml_value("generic");
    providers.push(provider);

    if !doc.contains_key("models") || !doc["models"].is_array_of_tables() {
        doc["models"] = Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }
    let model_tables = doc["models"]
        .as_array_of_tables_mut()
        .ok_or_else(|| anyhow!("Mistral Vibe models is not an array of tables"))?;
    let mut existing = model_tables
        .iter()
        .filter(|model| {
            model.get("provider").and_then(Item::as_str) == Some(ADDITIONAL_CONST_API_PROVIDER_ID)
        })
        .filter_map(|model| {
            model
                .get("name")
                .and_then(Item::as_str)
                .map(|name| (name.to_owned(), model.clone()))
        })
        .collect::<HashMap<_, _>>();
    model_tables.retain(|model| {
        model.get("provider").and_then(Item::as_str) != Some(ADDITIONAL_CONST_API_PROVIDER_ID)
    });
    for model in models {
        let id = model.id.trim();
        let mut entry = existing.remove(id).unwrap_or_default();
        entry.set_position(None);
        entry["name"] = toml_value(id);
        entry["provider"] = toml_value(ADDITIONAL_CONST_API_PROVIDER_ID);
        entry["alias"] = toml_value(format!("{ADDITIONAL_CONST_API_PROVIDER_ID}/{id}"));
        // `thinking` is a selected effort, not a capability flag. New models
        // follow Vibe's default; preserve the user's existing choice.
        entry["supports_images"] =
            toml_value(model.input_modalities.iter().any(|value| value == "image"));
        model_tables.push(entry);
    }
    let mut content = doc.to_string();
    if !content.ends_with('\n') {
        content.push('\n');
    }
    Ok(content)
}

fn mistral_vibe_models(raw: &str) -> Vec<ToolModelInfo> {
    raw.parse::<DocumentMut>()
        .ok()
        .and_then(|doc| {
            doc.get("models")
                .and_then(Item::as_array_of_tables)
                .map(|models| {
                    models
                        .iter()
                        .filter(|model| {
                            model.get("provider").and_then(Item::as_str)
                                == Some(ADDITIONAL_CONST_API_PROVIDER_ID)
                        })
                        .filter_map(|model| {
                            let id = model.get("name").and_then(Item::as_str)?;
                            let reasoning = model
                                .get("thinking")
                                .and_then(Item::as_str)
                                .is_some_and(|value| value != "off");
                            let supports_images = model
                                .get("supports_images")
                                .and_then(Item::as_bool)
                                .unwrap_or(false);
                            Some(ToolModelInfo {
                                id: id.to_string(),
                                reasoning,
                                input_modalities: if supports_images {
                                    vec!["text".into(), "image".into()]
                                } else {
                                    vec!["text".into()]
                                },
                                ..ToolModelInfo::default()
                            })
                        })
                        .collect()
                })
        })
        .unwrap_or_default()
}

fn apply_mistral_vibe_config(
    base_url: &str,
    api_key: &str,
    models: &[ToolModelInfo],
) -> Result<ToolApplyResult> {
    let models = configured_additional_tool_models("Mistral Vibe", models)?;
    let (config_path, env_path) = mistral_vibe_config_paths();
    let paths = [config_path.clone(), env_path.clone()];
    with_tool_config_file_transaction("mistral-vibe", &paths, || {
        let config =
            upsert_mistral_vibe_toml(&read_text_or_empty(&config_path)?, base_url, &models)?;
        let env = upsert_env_vars(
            &read_text_or_empty(&env_path)?,
            &[(ADDITIONAL_CONST_API_ENV_KEY, api_key)],
        );
        let mut result = ToolApplyBuilder::default();
        write_text_with_backup(&config_path, &config, "mistral-vibe", &mut result)?;
        write_text_with_backup(&env_path, &env, "mistral-vibe", &mut result)?;
        let mut result = result.finish("mistral-vibe");
        attach_tool_protocol(&mut result, ToolProtocol::OpenAiChat);
        Ok(result)
    })
}

fn check_mistral_vibe_config(base_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    let (config_path, env_path) = mistral_vibe_config_paths();
    let raw = read_text_or_empty(&config_path)?;
    let models = mistral_vibe_models(&raw);
    let config = upsert_mistral_vibe_toml(&raw, base_url, &models)?;
    let env = upsert_env_vars(
        &read_text_or_empty(&env_path)?,
        &[(ADDITIONAL_CONST_API_ENV_KEY, api_key)],
    );
    let mut result = ToolApplyBuilder::default();
    observe_text(&config_path, &config, &mut result)?;
    observe_text(&env_path, &env, &mut result)?;
    let mut result = result.finish("mistral-vibe");
    result.already_configured &= !models.is_empty();
    attach_tool_protocol(&mut result, ToolProtocol::OpenAiChat);
    Ok(result)
}

fn upsert_open_design_config(
    root: &mut serde_json::Value,
    base_url: &str,
    api_key: &str,
    model: &str,
) {
    ensure_json_object(root);
    root["agentId"] = serde_json::json!("codex");
    ensure_json_object_field(root, "agentModels");
    if root["agentModels"]
        .get("codex")
        .is_none_or(|value| !value.is_object())
    {
        root["agentModels"]["codex"] = serde_json::json!({});
    }
    root["agentModels"]["codex"]["model"] = serde_json::json!(model);
    ensure_json_object_field(root, "agentCliEnv");
    if root["agentCliEnv"]
        .get("codex")
        .is_none_or(|value| !value.is_object())
    {
        root["agentCliEnv"]["codex"] = serde_json::json!({});
    }
    root["agentCliEnv"]["codex"]["OPENAI_BASE_URL"] =
        serde_json::json!(tool_surface_url(base_url, "v1"));
    root["agentCliEnv"]["codex"]["OPENAI_API_KEY"] = serde_json::json!(api_key);
    root["agentCliEnv"]["codex"]["CODEX_API_KEY"] = serde_json::json!(api_key);
    ensure_json_object_field(root, "agentCliEnvIntent");
    root["agentCliEnvIntent"]["codex"] = serde_json::json!({ "apiKeyOverride": true });
}

fn apply_open_design_config(
    base_url: &str,
    api_key: &str,
    models: &[ToolModelInfo],
) -> Result<ToolApplyResult> {
    let models = configured_additional_tool_models("Open Design", models)?;
    let path = open_design_config_path()?;
    let mut root = read_json_or_default(&path, serde_json::json!({}))?;
    upsert_open_design_config(&mut root, base_url, api_key, models[0].id.trim());
    let mut result = ToolApplyBuilder::default();
    write_text_with_backup(&path, &json_text(&root)?, "open-design", &mut result)?;
    let mut result = result.finish("open-design");
    result.details.insert(
        "configuration_scope".to_string(),
        "official_codex_adapter".to_string(),
    );
    attach_tool_protocol(&mut result, ToolProtocol::OpenAiResponses);
    Ok(result)
}

fn check_open_design_config(base_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    let path = open_design_config_path()?;
    let mut root = read_json_or_default(&path, serde_json::json!({}))?;
    let model = root
        .pointer("/agentModels/codex/model")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    upsert_open_design_config(&mut root, base_url, api_key, &model);
    let mut result = ToolApplyBuilder::default();
    observe_json(&path, &root, &mut result)?;
    let mut result = result.finish("open-design");
    result.already_configured &= !model.is_empty();
    result.details.insert(
        "configuration_scope".to_string(),
        "official_codex_adapter".to_string(),
    );
    attach_tool_protocol(&mut result, ToolProtocol::OpenAiResponses);
    Ok(result)
}

pub(crate) fn apply_additional_tool_config_with_model_info_for_protocol(
    tool: &str,
    base_url: &str,
    api_key: &str,
    models: &[ToolModelInfo],
    protocol: ToolProtocol,
) -> Result<ToolApplyResult> {
    resolve_tool_protocol(tool, Some(protocol.as_str()))?;
    match tool {
        "grok-build" => configure_grok_build(base_url, api_key, Some(models), protocol),
        "minimax-code" => configure_minimax_code(base_url, api_key, Some(models), protocol),
        "copilot" => apply_copilot_config(base_url, api_key, models),
        "copilot-desktop" => copilot_desktop::apply(base_url, api_key, models, protocol),
        "raven" => apply_raven_config(base_url, api_key, models),
        "pi" => apply_pi_config(base_url, api_key, models, protocol),
        "cline" => apply_cline_config(base_url, api_key, models, protocol),
        "reasonix" => apply_reasonix_config(base_url, api_key, models),
        "deepseek-harness" => apply_deepseek_harness_config(base_url, api_key, models, protocol),
        "open-interpreter" => apply_open_interpreter_config(base_url, api_key, models),
        "anythingllm" => apply_anythingllm_config(base_url, api_key, models),
        "goose" => apply_goose_config(base_url, api_key, models),
        "mistral-vibe" => apply_mistral_vibe_config(base_url, api_key, models),
        "open-design" => apply_open_design_config(base_url, api_key, models),
        "kimicode" => apply_kimicode_config(base_url, api_key, models, protocol),
        "mimocode" => apply_openai_compatible_json_tool(
            tool,
            mimocode_config_path()?,
            base_url,
            api_key,
            models,
            true,
            protocol,
        ),
        "qwencode" => apply_qwencode_config(base_url, api_key, models),
        "openscience" => apply_openai_compatible_json_tool(
            tool,
            openscience_config_path()?,
            base_url,
            api_key,
            models,
            false,
            protocol,
        ),
        "vibe-trading" => apply_vibe_trading_config(base_url, api_key, models),
        "zcode" => apply_zcode_config(base_url, api_key, models),
        _ => Err(anyhow!("unknown additional tool: {tool}")),
    }
}

pub(crate) fn check_additional_tool_config(
    tool: &str,
    base_url: &str,
    api_key: &str,
) -> Result<ToolApplyResult> {
    match tool {
        "grok-build" => {
            configure_grok_build(base_url, api_key, None, ToolProtocol::OpenAiResponses)
        }
        "minimax-code" => {
            configure_minimax_code(base_url, api_key, None, ToolProtocol::AnthropicMessages)
        }
        "copilot" => check_copilot_config(base_url, api_key),
        "copilot-desktop" => copilot_desktop::check(base_url, api_key),
        "raven" => check_raven_config(base_url, api_key),
        "pi" => check_pi_config(base_url, api_key),
        "cline" => check_cline_config(base_url, api_key),
        "reasonix" => check_reasonix_config(base_url, api_key),
        "deepseek-harness" => check_deepseek_harness_config(base_url, api_key),
        "open-interpreter" => check_open_interpreter_config(base_url, api_key),
        "anythingllm" => check_anythingllm_config(base_url, api_key),
        "goose" => check_goose_config(base_url, api_key),
        "mistral-vibe" => check_mistral_vibe_config(base_url, api_key),
        "open-design" => check_open_design_config(base_url, api_key),
        "kimicode" => check_kimicode_config(base_url, api_key),
        "mimocode" => check_openai_compatible_json_tool(
            tool,
            mimocode_config_path()?,
            base_url,
            api_key,
            true,
        ),
        "qwencode" => check_qwencode_config(base_url, api_key),
        "openscience" => check_openai_compatible_json_tool(
            tool,
            openscience_config_path()?,
            base_url,
            api_key,
            false,
        ),
        "vibe-trading" => check_vibe_trading_config(base_url, api_key),
        "zcode" => check_zcode_config(base_url, api_key),
        _ => Err(anyhow!("unknown additional tool: {tool}")),
    }
}

pub(crate) fn remove_additional_tool_config(tool: &str) -> Result<ToolApplyResult> {
    match tool {
        "copilot-desktop" => copilot_desktop::remove(),
        "copilot" | "grok-build" | "minimax-code" | "raven" | "pi" | "cline" | "reasonix"
        | "deepseek-harness" | "open-interpreter" | "anythingllm" | "goose" | "mistral-vibe"
        | "open-design" | "kimicode" | "mimocode" | "qwencode" | "openscience" | "vibe-trading"
        | "zcode" => restore_tool_config_from_manifest(tool),
        _ => Err(anyhow!("unknown additional tool: {tool}")),
    }
}
