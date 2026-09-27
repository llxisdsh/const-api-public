// Open Interpreter's Rust terminal uses its own Codex-derived provider schema.
// It deliberately ignores CODEX_HOME; never touch the user's Codex configuration.
fn open_interpreter_config_path() -> Result<PathBuf> {
    if !test_home_active() {
        if let Some(path) = nonempty_env_path("INTERPRETER_HOME") {
            let root = fs::canonicalize(path).context("resolve INTERPRETER_HOME")?;
            if !root.is_dir() {
                return Err(anyhow!(
                    "INTERPRETER_HOME must point to an existing directory"
                ));
            }
            return Ok(root.join("config.toml"));
        }
    }
    Ok(home_dir().join(".openinterpreter").join("config.toml"))
}

fn parse_open_interpreter_toml(raw: &str) -> Result<DocumentMut> {
    raw.parse::<DocumentMut>()
        .context("parse Open Interpreter config.toml")
}

fn upsert_open_interpreter_toml(
    raw: &str,
    base_url: &str,
    api_key: &str,
    model: &str,
) -> Result<String> {
    let mut doc = parse_open_interpreter_toml(raw)?;
    doc["model"] = toml_value(model);
    doc["model_provider"] = toml_value(CODEX_CONST_API_PROVIDER_ID);
    if !doc.contains_key("model_providers") {
        doc["model_providers"] = Item::Table(Table::new());
    }
    let providers = doc["model_providers"]
        .as_table_like_mut()
        .ok_or_else(|| anyhow!("Open Interpreter model_providers must be a table"))?;
    if !providers.contains_key(CODEX_CONST_API_PROVIDER_ID) {
        providers.insert(CODEX_CONST_API_PROVIDER_ID, Item::Table(Table::new()));
    }
    let provider = providers
        .get_mut(CODEX_CONST_API_PROVIDER_ID)
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| anyhow!("Open Interpreter CONST_API provider must be a table"))?;
    // These authentication modes are mutually exclusive in the upstream schema.
    // A durable local key also works when the user launches Interpreter elsewhere.
    provider.remove("env_key");
    provider.remove("auth");
    for (key, value) in [
        ("name", toml_value(CONST_API_DISPLAY_NAME)),
        ("base_url", toml_value(tool_surface_url(base_url, "v1"))),
        ("experimental_bearer_token", toml_value(api_key)),
        ("wire_api", toml_value("chat")),
        ("requires_openai_auth", toml_value(false)),
        ("supports_websockets", toml_value(false)),
    ] {
        provider.insert(key, value);
    }
    Ok(doc.to_string())
}

fn upsert_open_interpreter_yaml(
    raw: &str,
    base_url: &str,
    api_key: &str,
    model: &str,
) -> Result<String> {
    let mut root = if raw.trim().is_empty() {
        serde_yaml::Mapping::new()
    } else {
        match serde_yaml::from_str::<serde_yaml::Value>(raw)
            .context("parse Open Interpreter profile")?
        {
            serde_yaml::Value::Mapping(root) => root,
            _ => return Err(anyhow!("Open Interpreter profile must be a YAML mapping")),
        }
    };
    root.entry(yaml_key("version"))
        .or_insert_with(|| yaml_string(OPEN_INTERPRETER_LEGACY_PROFILE_VERSION));
    let llm = ensure_yaml_mapping(&mut root, "llm")?;
    llm.insert(yaml_key("model"), yaml_string(&format!("openai/{model}")));
    llm.insert(
        yaml_key("api_base"),
        yaml_string(&tool_surface_url(base_url, "v1")),
    );
    llm.insert(yaml_key("api_key"), yaml_string(api_key));
    llm.insert(
        yaml_key("supports_functions"),
        serde_yaml::Value::Bool(true),
    );
    let mut content = serde_yaml::to_string(&serde_yaml::Value::Mapping(root))?;
    if !content.ends_with('\n') {
        content.push('\n');
    }
    Ok(content)
}

fn open_interpreter_model(raw: &str) -> String {
    serde_yaml::from_str::<serde_yaml::Value>(raw)
        .ok()
        .and_then(|root| {
            root.get(yaml_key("llm"))?
                .get(yaml_key("model"))?
                .as_str()
                .map(str::to_string)
        })
        .and_then(|model| model.strip_prefix("openai/").map(str::to_string))
        .unwrap_or_default()
}

fn apply_open_interpreter_config(
    base_url: &str,
    api_key: &str,
    models: &[ToolModelInfo],
) -> Result<ToolApplyResult> {
    let models = configured_additional_tool_models("Open Interpreter", models)?;
    let config_path = open_interpreter_config_path()?;
    let profile_path = open_interpreter_profile_path();
    let paths = [config_path.clone(), profile_path.clone()];
    // The Rust terminal and legacy Python CLI share the executable name.
    // Keep both native formats in sync without running either during discovery.
    with_tool_config_file_transaction("open-interpreter", &paths, || {
        let config = upsert_open_interpreter_toml(
            &read_text_or_empty(&config_path)?,
            base_url,
            api_key,
            models[0].id.trim(),
        )?;
        let profile = upsert_open_interpreter_yaml(
            &read_text_or_empty(&profile_path)?,
            base_url,
            api_key,
            models[0].id.trim(),
        )?;
        let mut result = ToolApplyBuilder::default();
        write_text_with_backup(&config_path, &config, "open-interpreter", &mut result)?;
        write_text_with_backup(&profile_path, &profile, "open-interpreter", &mut result)?;
        let mut result = result.finish("open-interpreter");
        attach_tool_protocol(&mut result, ToolProtocol::OpenAiChat);
        Ok(result)
    })
}

fn check_open_interpreter_config(base_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    let config_path = open_interpreter_config_path()?;
    let raw_config = read_text_or_empty(&config_path)?;
    let config_doc = parse_open_interpreter_toml(&raw_config)?;
    let path = open_interpreter_profile_path();
    let raw = read_text_or_empty(&path)?;
    let model = config_doc
        .get("model")
        .and_then(Item::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| open_interpreter_model(&raw));
    let config = upsert_open_interpreter_toml(&raw_config, base_url, api_key, &model)?;
    let content = upsert_open_interpreter_yaml(&raw, base_url, api_key, &model)?;
    let mut result = ToolApplyBuilder::default();
    observe_text(&config_path, &config, &mut result)?;
    observe_text(&path, &content, &mut result)?;
    let mut result = result.finish("open-interpreter");
    result.already_configured &= !model.is_empty();
    attach_tool_protocol(&mut result, ToolProtocol::OpenAiChat);
    Ok(result)
}
// Profile schema version used by the legacy Python CLI, not its package version.
const OPEN_INTERPRETER_LEGACY_PROFILE_VERSION: &str = "0.2.5";

fn open_interpreter_profile_path() -> PathBuf {
    // Python uses platformdirs.user_config_dir("open-interpreter"). On Windows
    // its default appauthor repeats appname under Local (not Roaming).
    #[cfg(target_os = "windows")]
    let root = if test_home_active() {
        home_dir().join("AppData").join("Local")
    } else {
        nonempty_env_path("LOCALAPPDATA")
            .unwrap_or_else(|| home_dir().join("AppData").join("Local"))
    }
    .join("open-interpreter");
    #[cfg(target_os = "macos")]
    let root = home_dir().join("Library").join("Application Support");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let root = xdg_tool_config_root();
    root.join("open-interpreter")
        .join("profiles")
        .join("default.yaml")
}
