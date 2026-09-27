pub(crate) fn ensure_json_object(value: &mut serde_json::Value) {
    if !value.is_object() {
        *value = serde_json::json!({});
    }
}

fn remove_json_string_if_equal(
    object: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    expected: &str,
) {
    if object
        .get(key)
        .and_then(|value| value.as_str())
        .map(|value| value == expected)
        .unwrap_or(false)
    {
        object.remove(key);
    }
}

fn is_current_or_const_api_local_url(value: &str, expected: &str) -> bool {
    let value = value.trim().trim_end_matches('/');
    let expected = expected.trim().trim_end_matches('/');
    if value == expected {
        return true;
    }
    matches!(
        (
            const_api_local_tool_url_surface(value),
            const_api_local_tool_url_surface(expected),
        ),
        (Some(value_surface), Some(expected_surface)) if value_surface == expected_surface
    )
}

fn is_const_api_local_tool_url(value: &str) -> bool {
    const_api_local_tool_url_surface(value).is_some()
}

fn const_api_local_tool_url_surface(value: &str) -> Option<&str> {
    let value = value.trim().trim_end_matches('/');
    let endpoint = value.strip_prefix("http://127.0.0.1:")?;
    let (port, surface) = endpoint.split_once('/').unwrap_or((endpoint, ""));
    port.parse::<u16>().ok()?;
    match surface {
        "" | "v1" | "anthropic" | "gemini" => Some(surface),
        _ => None,
    }
}

fn local_tool_url_port(value: &str) -> Option<u16> {
    let value = value.trim().trim_end_matches('/');
    for prefix in [
        "http://127.0.0.1:",
        "http://0.0.0.0:",
        "http://localhost:",
        "http://[::1]:",
    ] {
        let Some(endpoint) = value.strip_prefix(prefix) else {
            continue;
        };
        let (port, surface) = endpoint.split_once('/').unwrap_or((endpoint, ""));
        if !matches!(surface, "" | "v1" | "anthropic" | "gemini") {
            continue;
        }
        if let Ok(port) = port.parse::<u16>() {
            return Some(port);
        }
    }
    None
}

fn tool_surface_url(root_url: &str, surface: &str) -> String {
    let surface = surface.trim_matches('/');
    if let Some(port) = local_tool_url_port(root_url) {
        return format!("http://127.0.0.1:{port}/{surface}");
    }

    let mut root = root_url.trim().trim_end_matches('/');
    for suffix in ["/v1", "/anthropic", "/gemini"] {
        if let Some(stripped) = root.strip_suffix(suffix) {
            root = stripped;
            break;
        }
    }
    format!("{root}/{surface}")
}

fn remove_opencode_legacy_const_api_providers(
    root: &mut serde_json::Value,
    base_url: &str,
    api_key: &str,
) {
    let Some(providers) = root
        .get_mut("provider")
        .and_then(|value| value.as_object_mut())
    else {
        return;
    };
    let keys: Vec<String> = providers
        .iter()
        .filter_map(|(key, value)| {
            if is_opencode_legacy_const_api_provider(key, value, base_url, api_key) {
                Some(key.clone())
            } else {
                None
            }
        })
        .collect();
    for key in keys {
        providers.remove(&key);
    }
}

fn remove_opencode_const_api_provider_aliases(
    root: &mut serde_json::Value,
    base_url: &str,
    api_key: &str,
) {
    let Some(providers) = root
        .get_mut("provider")
        .and_then(|value| value.as_object_mut())
    else {
        return;
    };
    for provider_id in [CODEX_CONST_API_PROVIDER_ID, LEGACY_CONST_API_PROVIDER_ID] {
        let owned = providers
            .get(provider_id)
            .map(|value| {
                is_opencode_legacy_const_api_provider(provider_id, value, base_url, api_key)
            })
            .unwrap_or(false);
        if owned {
            providers.remove(provider_id);
        }
    }
}

fn opencode_const_api_provider(
    base_url: &str,
    api_key: &str,
    model_info: &[ToolModelInfo],
    protocol: ToolProtocol,
) -> serde_json::Value {
    let base_url = tool_surface_url(base_url, protocol.surface());
    let mut provider = serde_json::json!({
        "npm": opencode_provider_package(protocol),
        "name": CONST_API_DISPLAY_NAME,
        "options": {
            "baseURL": base_url,
            "apiKey": api_key
        }
    });
    let models = opencode_models_object(model_info);
    if !models.is_empty() {
        provider["models"] = serde_json::Value::Object(models);
    }
    provider
}

fn remove_opencode_legacy_cache_disable(provider: &mut serde_json::Value) {
    let Some(options) = provider
        .get_mut("options")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    if options.get("setCacheKey") == Some(&serde_json::Value::Bool(false)) {
        // Older CONST API releases disabled OpenCode's stable per-session cache
        // key to avoid cross-protocol admission failures. Conversion now treats
        // that hint as safely omittable, so keeping this flag only lowers hits.
        options.remove("setCacheKey");
    }
}

fn opencode_provider_package(protocol: ToolProtocol) -> &'static str {
    match protocol {
        ToolProtocol::OpenAiResponses => "@ai-sdk/openai",
        ToolProtocol::OpenAiChat => "@ai-sdk/openai-compatible",
        ToolProtocol::AnthropicMessages => "@ai-sdk/anthropic",
        ToolProtocol::GeminiNative => "@ai-sdk/google",
    }
}

fn detect_opencode_tool_protocol(root: &serde_json::Value) -> Option<ToolProtocol> {
    let providers = root.get("provider")?;
    let package = providers
        .get(CODEX_CONST_API_PROVIDER_ID)
        .or_else(|| providers.get(LEGACY_CONST_API_PROVIDER_ID))?
        .get("npm")?
        .as_str()?;
    match package {
        "@ai-sdk/openai" => Some(ToolProtocol::OpenAiResponses),
        "@ai-sdk/openai-compatible" => Some(ToolProtocol::OpenAiChat),
        "@ai-sdk/anthropic" => Some(ToolProtocol::AnthropicMessages),
        "@ai-sdk/google" => Some(ToolProtocol::GeminiNative),
        _ => None,
    }
}

fn claude_desktop_inference_models(
    config: &crate::model::ClientConfig,
    models: &[ToolModelInfo],
) -> Vec<serde_json::Value> {
    let model_ids = models
        .iter()
        .map(|model| model.id.trim())
        .filter(|model| {
            !model.is_empty() && *model != "code-cheap" && *model != LOCAL_PLACEHOLDER_KEY
        })
        .map(str::to_string)
        .collect::<Vec<_>>();
    let mut routes = crate::model_compatibility::anthropic_model_routes(config, &model_ids);
    if let Some(policy) = models.iter().find_map(|model| model.presentation.as_deref()) {
        // Alias generation must not resurrect models hidden from the tool
        // list. Route eligibility and context intersections are unchanged.
        routes.retain(|route| !policy.rank(&crate::config::public_model_name(route)).1);
        routes.sort_by_cached_key(|route| (
            std::cmp::Reverse(policy.rank(&crate::config::public_model_name(route)).0),
            route.clone(),
        ));
    }
    routes
        .into_iter()
        .map(|route| {
            let (_, _, supports_1m) = crate::tool_model_metadata::route_token_limits(config, &route, models);
            // Desktop's schema uses a capability, never a literal [1m] ID.
            // Explicit false matters for Claude IDs whose vendor default is 1M
            // but whose configured compatible routes have a smaller window.
            serde_json::json!({"name": route, "labelOverride": route, "supports1m": supports_1m, "prefer1m": supports_1m})
        })
        .collect()
}

fn opencode_models_object(
    model_info: &[ToolModelInfo],
) -> serde_json::Map<String, serde_json::Value> {
    let mut seen = HashSet::new();
    let mut models = serde_json::Map::new();
    for model in model_info {
        let id = model.id.trim();
        if id.is_empty() || id == "code-cheap" || id == LOCAL_PLACEHOLDER_KEY {
            continue;
        }
        if !seen.insert(id.to_string()) {
            continue;
        }
        let mut value = serde_json::json!({
            "name": if model.display_name.trim().is_empty() { id } else { model.display_name.trim() },
            "reasoning": model.reasoning,
            "tool_call": model.tool_call,
            "attachment": model.attachment,
            "modalities": {
                "input": model.input_modalities,
                "output": model.output_modalities
            }
        });
        if !model.family.trim().is_empty() {
            value["family"] = serde_json::json!(model.family.trim());
        }
        if let (Some(context), Some(output)) = (model.context_tokens, model.output_tokens) {
            let mut limit = serde_json::json!({
                "context": context,
                "output": output
            });
            let input = context.saturating_sub(output);
            if input > 0 {
                limit["input"] = serde_json::json!(input);
            }
            value["limit"] = limit;
        }
        if model.reasoning {
            let variants = model
                .reasoning_efforts
                .iter()
                // OpenCode defines variants as an open Record and renders its
                // keys dynamically. Preserve every catalog-declared label,
                // including future provider-specific values.
                .filter(|effort| !effort.trim().is_empty())
                .map(|effort| {
                    (
                        effort.clone(),
                        serde_json::json!({"reasoningEffort": effort}),
                    )
                })
                .collect::<serde_json::Map<_, _>>();
            if !variants.is_empty() {
                // Explicit variants prevent OpenCode from guessing supported
                // strengths from the model ID and provider package.
                value["variants"] = serde_json::Value::Object(variants);
            }
        }
        models.insert(id.to_string(), value);
    }
    models
}

fn is_opencode_legacy_const_api_provider(
    key: &str,
    value: &serde_json::Value,
    base_url: &str,
    api_key: &str,
) -> bool {
    if key == "newapi_const" || key == "newapi.const" {
        return true;
    }
    let Some(provider) = value.as_object() else {
        return false;
    };
    let name_is_legacy = provider
        .get("name")
        .and_then(|value| value.as_str())
        .map(|name| {
            name.eq_ignore_ascii_case("newapi.const")
                || name == CONST_API_DISPLAY_NAME
                || name == CONST_API_LEGACY_DISPLAY_NAME
        })
        .unwrap_or(false);
    let has_legacy_model = provider
        .get("models")
        .and_then(|models| models.as_object())
        .map(|models| models.contains_key("code-cheap"))
        .unwrap_or(false);
    let options = provider.get("options").and_then(|value| value.as_object());
    let local_base_url = options
        .and_then(|options| options.get("baseURL"))
        .and_then(|value| value.as_str())
        .map(|value| is_current_or_const_api_local_url(value, base_url))
        .unwrap_or(false);
    let old_api_key = options
        .and_then(|options| options.get("apiKey"))
        .and_then(|value| value.as_str())
        .map(|value| value == LOCAL_PLACEHOLDER_KEY || value == api_key)
        .unwrap_or(false);

    let known_provider_id =
        key == CODEX_CONST_API_PROVIDER_ID || key == LEGACY_CONST_API_PROVIDER_ID;
    (name_is_legacy && (has_legacy_model || local_base_url || old_api_key))
        || (known_provider_id && (local_base_url || old_api_key))
}

fn openclaw_api_mode(protocol: ToolProtocol) -> &'static str {
    match protocol {
        ToolProtocol::OpenAiResponses => "openai-responses",
        ToolProtocol::OpenAiChat => "openai-completions",
        ToolProtocol::AnthropicMessages => "anthropic-messages",
        ToolProtocol::GeminiNative => "google-generative-ai",
    }
}

fn detect_openclaw_tool_protocol(root: &serde_json::Value) -> Option<ToolProtocol> {
    let providers = root.get("models")?.get("providers")?;
    let mode = providers
        .get(CODEX_CONST_API_PROVIDER_ID)
        .or_else(|| providers.get(LEGACY_CONST_API_PROVIDER_ID))?
        .get("api")?
        .as_str()?;
    match mode {
        "openai-responses" => Some(ToolProtocol::OpenAiResponses),
        "openai-completions" => Some(ToolProtocol::OpenAiChat),
        "anthropic-messages" => Some(ToolProtocol::AnthropicMessages),
        "google-generative-ai" => Some(ToolProtocol::GeminiNative),
        _ => None,
    }
}

fn remove_openclaw_const_api_providers(
    root: &mut serde_json::Value,
    base_url: &str,
    api_key: &str,
) {
    let Some(providers) = root
        .get_mut("models")
        .and_then(|models| models.get_mut("providers"))
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    for provider_id in [CODEX_CONST_API_PROVIDER_ID, LEGACY_CONST_API_PROVIDER_ID] {
        let owned = providers
            .get(provider_id)
            .and_then(serde_json::Value::as_object)
            .map(|provider| {
                provider
                    .get("baseUrl")
                    .and_then(serde_json::Value::as_str)
                    .map(|value| {
                        is_current_or_const_api_local_url(value, base_url)
                            || is_const_api_local_tool_url(value)
                    })
                    .unwrap_or(false)
                    || provider
                        .get("apiKey")
                        .and_then(serde_json::Value::as_str)
                        .map(|value| value == LOCAL_PLACEHOLDER_KEY || value == api_key)
                        .unwrap_or(false)
            })
            .unwrap_or(false);
        if owned {
            providers.remove(provider_id);
        }
    }
}

fn remove_openclaw_legacy_default_model(root: &mut serde_json::Value) {
    let Some(defaults) = root
        .get_mut("agents")
        .and_then(|agents| agents.get_mut("defaults"))
        .and_then(|defaults| defaults.as_object_mut())
    else {
        return;
    };
    let is_legacy_model = defaults
        .get("model")
        .and_then(|model| model.get("primary"))
        .and_then(|primary| primary.as_str())
        .map(|primary| primary == "code-cheap")
        .unwrap_or(false);
    if is_legacy_model {
        defaults.remove("model");
    }
}

#[cfg(test)]
pub(crate) fn upsert_hermes_sections(raw: &str, root_url: &str, api_key: &str) -> Result<String> {
    upsert_hermes_sections_for_protocol(raw, root_url, api_key, ToolProtocol::OpenAiChat)
}

pub(crate) fn upsert_hermes_sections_for_protocol(
    raw: &str,
    root_url: &str,
    api_key: &str,
    protocol: ToolProtocol,
) -> Result<String> {
    let value = if raw.trim().is_empty() {
        serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
    } else {
        serde_yaml::from_str::<serde_yaml::Value>(raw)
            .with_context(|| "parse Hermes config.yaml")?
    };
    let mut root = match value {
        serde_yaml::Value::Mapping(root) => root,
        _ => serde_yaml::Mapping::new(),
    };

    let base_url = tool_surface_url(root_url, protocol.surface());
    let api_mode = hermes_api_mode(protocol)?;
    upsert_hermes_model(&mut root, &base_url, api_key, api_mode)?;
    remove_hermes_const_api_provider_dict(&mut root);
    upsert_hermes_custom_provider_list(&mut root, &base_url, api_key, api_mode);

    let mut content = serde_yaml::to_string(&serde_yaml::Value::Mapping(root))
        .with_context(|| "serialize Hermes config.yaml")?;
    if !content.ends_with('\n') {
        content.push('\n');
    }
    Ok(content)
}

fn hermes_api_mode(protocol: ToolProtocol) -> Result<&'static str> {
    match protocol {
        ToolProtocol::OpenAiResponses => Ok("codex_responses"),
        ToolProtocol::OpenAiChat => Ok("chat_completions"),
        ToolProtocol::AnthropicMessages => Ok("anthropic_messages"),
        ToolProtocol::GeminiNative => Err(anyhow!("Hermes does not support Gemini Native")),
    }
}

fn upsert_hermes_model(
    root: &mut serde_yaml::Mapping,
    base_url: &str,
    api_key: &str,
    api_mode: &str,
) -> Result<()> {
    let model = ensure_yaml_mapping(root, "model")?;
    model.insert(
        yaml_key("provider"),
        yaml_string(HERMES_CONST_API_PROVIDER_ID),
    );
    model.insert(yaml_key("base_url"), yaml_string(base_url));
    model.insert(yaml_key("api_key"), yaml_string(api_key));
    model.insert(yaml_key("api_mode"), yaml_string(api_mode));
    if model
        .get(yaml_key("default"))
        .and_then(|value| value.as_str())
        .map(|value| value == "code-cheap")
        .unwrap_or(false)
    {
        model.remove(yaml_key("default"));
    }
    Ok(())
}

fn remove_hermes_const_api_provider_dict(root: &mut serde_yaml::Mapping) {
    let providers_key = yaml_key("providers");
    if let Some(providers) = root
        .get_mut(&providers_key)
        .and_then(|value| value.as_mapping_mut())
    {
        providers.remove(&yaml_key(CODEX_CONST_API_PROVIDER_ID));
        providers.remove(&yaml_key(LEGACY_CONST_API_PROVIDER_ID));
        if providers.is_empty() {
            root.remove(&providers_key);
        }
    }
}

fn upsert_hermes_custom_provider_list(
    root: &mut serde_yaml::Mapping,
    base_url: &str,
    api_key: &str,
    api_mode: &str,
) {
    let existing_providers = root
        .get(&yaml_key("custom_providers"))
        .and_then(|value| value.as_sequence())
        .cloned()
        .unwrap_or_default();

    let mut providers = Vec::new();
    let mut existing_const_api = None;
    for value in existing_providers {
        if value
            .as_mapping()
            .and_then(|mapping| mapping.get(&yaml_key("name")))
            .and_then(|value| value.as_str())
            .map(|name| name == CODEX_CONST_API_PROVIDER_ID || name == LEGACY_CONST_API_PROVIDER_ID)
            .unwrap_or(false)
        {
            if existing_const_api.is_none() {
                existing_const_api = Some(value);
            }
            continue;
        }
        providers.push(value);
    }

    let mut provider = existing_const_api
        .as_ref()
        .and_then(|value| value.as_mapping())
        .cloned()
        .unwrap_or_default();
    provider.insert(yaml_key("name"), yaml_string(CODEX_CONST_API_PROVIDER_ID));
    provider.insert(yaml_key("base_url"), yaml_string(base_url));
    provider.insert(yaml_key("api_key"), yaml_string(api_key));
    provider.insert(yaml_key("api_mode"), yaml_string(api_mode));
    provider.insert(yaml_key("discover_models"), serde_yaml::Value::Bool(true));
    // Hermes may persist its selected model and live-discovered catalog here.
    // They are runtime-owned metadata, so preserve them across reconfiguration.
    // Remove only the obsolete CONST API placeholder used by older releases.
    if provider
        .get(yaml_key("model"))
        .and_then(|value| value.as_str())
        == Some("code-cheap")
    {
        provider.remove(yaml_key("model"));
    }
    if provider
        .get(yaml_key("models"))
        .and_then(|value| value.as_mapping())
        .is_some_and(|models| {
            models.len() == 1 && models.contains_key(yaml_key("code-cheap"))
        })
    {
        provider.remove(yaml_key("models"));
    }

    let provider = serde_yaml::Value::Mapping(provider);
    providers.insert(0, provider);
    root.insert(
        yaml_key("custom_providers"),
        serde_yaml::Value::Sequence(providers),
    );
}

fn detect_hermes_tool_protocol(raw: &str) -> Option<ToolProtocol> {
    let value = serde_yaml::from_str::<serde_yaml::Value>(raw).ok()?;
    let root = value.as_mapping()?;
    let mode = root
        .get(yaml_key("model"))?
        .as_mapping()?
        .get(yaml_key("api_mode"))?
        .as_str()?;
    match mode {
        "codex_responses" => Some(ToolProtocol::OpenAiResponses),
        "chat_completions" => Some(ToolProtocol::OpenAiChat),
        "anthropic_messages" => Some(ToolProtocol::AnthropicMessages),
        _ => None,
    }
}

fn ensure_yaml_mapping<'a>(
    root: &'a mut serde_yaml::Mapping,
    key: &str,
) -> Result<&'a mut serde_yaml::Mapping> {
    let key = yaml_key(key);
    if !matches!(root.get(&key), Some(serde_yaml::Value::Mapping(_))) {
        root.insert(
            key.clone(),
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        );
    }
    root.get_mut(&key)
        .and_then(|value| value.as_mapping_mut())
        .ok_or_else(|| anyhow!("YAML field {key:?} is not a mapping"))
}

fn yaml_key(value: &str) -> serde_yaml::Value {
    serde_yaml::Value::String(value.to_string())
}

fn yaml_string(value: &str) -> serde_yaml::Value {
    serde_yaml::Value::String(value.to_string())
}

// This ID is owned by CONST API and must remain stable. Claude Desktop treats it
// as an opaque config-library selector; the profile filename, entries[].id and
// _meta.appliedId must all agree.
pub(crate) const CLAUDE_DESKTOP_PROFILE_ID: &str =
    "2b067cc3-e571-442d-bf7e-03d05d72b0aa";
// Older CONST API builds accidentally reused CC Switch's profile ID and wrote
// it under the unsupported `appliedProfileId` key. Keep the value only for a
// narrowly-scoped migration; never use it for new profiles.
pub(crate) const CLAUDE_DESKTOP_LEGACY_PROFILE_ID: &str =
    "00000000-0000-4000-8000-000000157210";
pub(crate) const CLAUDE_DESKTOP_PROFILE_NAME: &str = "CONST API";

pub(crate) struct ClaudeDesktopPaths {
    pub(crate) normal_config_path: PathBuf,
    pub(crate) threep_config_path: PathBuf,
    pub(crate) profile_path: PathBuf,
    pub(crate) legacy_profile_path: PathBuf,
    pub(crate) meta_path: PathBuf,
}

pub(crate) fn claude_desktop_paths() -> ClaudeDesktopPaths {
    let home = home_dir();
    #[cfg(target_os = "macos")]
    let (normal_dir, threep_dir) = {
        let app_support = home.join("Library").join("Application Support");
        (app_support.join("Claude"), app_support.join("Claude-3p"))
    };
    #[cfg(target_os = "windows")]
    let (normal_dir, threep_dir) = {
        let local_app_data = const_api_test_home()
            .map(|home| home.join("AppData").join("Local"))
            .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
            .unwrap_or_else(|| home.join("AppData").join("Local"));
        (
            local_app_data.join("Claude"),
            local_app_data.join("Claude-3p"),
        )
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let (normal_dir, threep_dir) = {
        let app_support = xdg_tool_config_root();
        (app_support.join("Claude"), app_support.join("Claude-3p"))
    };
    let config_library_path = threep_dir.join("configLibrary");
    ClaudeDesktopPaths {
        normal_config_path: normal_dir.join("claude_desktop_config.json"),
        threep_config_path: threep_dir.join("claude_desktop_config.json"),
        profile_path: config_library_path.join(format!("{CLAUDE_DESKTOP_PROFILE_ID}.json")),
        legacy_profile_path: config_library_path
            .join(format!("{CLAUDE_DESKTOP_LEGACY_PROFILE_ID}.json")),
        meta_path: config_library_path.join("_meta.json"),
    }
}

pub(crate) fn vscode_chat_language_models_path() -> PathBuf {
    let home = home_dir();
    #[cfg(target_os = "windows")]
    let config_root = const_api_test_home()
        .map(|home| home.join("AppData").join("Roaming"))
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
        .unwrap_or_else(|| home.join("AppData").join("Roaming"));
    #[cfg(target_os = "macos")]
    let config_root = home.join("Library").join("Application Support");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let config_root = const_api_test_home()
        .map(|home| home.join(".config"))
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .unwrap_or_else(|| home.join(".config"));
    config_root
        .join("Code")
        .join("User")
        .join("chatLanguageModels.json")
}

pub(crate) fn vscode_user_settings_path() -> PathBuf {
    let home = home_dir();
    #[cfg(target_os = "windows")]
    let config_root = const_api_test_home()
        .map(|home| home.join("AppData").join("Roaming"))
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
        .unwrap_or_else(|| home.join("AppData").join("Roaming"));
    #[cfg(target_os = "macos")]
    let config_root = home.join("Library").join("Application Support");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let config_root = const_api_test_home()
        .map(|home| home.join(".config"))
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .unwrap_or_else(|| home.join(".config"));
    config_root.join("Code").join("User").join("settings.json")
}

fn write_json_with_backup(
    path: &Path,
    value: &serde_json::Value,
    tool: &str,
    result: &mut ToolApplyBuilder,
) -> Result<()> {
    let content = serde_json::to_string_pretty(value)? + "\n";
    write_text_with_backup(path, &content, tool, result)
}

fn observe_deployment_mode(path: &Path, mode: &str, result: &mut ToolApplyBuilder) -> Result<()> {
    let mut value = if path.exists() {
        let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_str::<serde_json::Value>(&raw)
            .with_context(|| format!("parse json {}", path.display()))?
    } else {
        serde_json::json!({})
    };
    if !value.is_object() {
        value = serde_json::json!({});
    }
    value["deploymentMode"] = serde_json::json!(mode);
    observe_json(path, &value, result)
}

fn write_deployment_mode(
    path: &Path,
    mode: &str,
    tool: &str,
    result: &mut ToolApplyBuilder,
) -> Result<()> {
    let mut value = if path.exists() {
        let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_str::<serde_json::Value>(&raw)
            .with_context(|| format!("parse json {}", path.display()))?
    } else {
        serde_json::json!({})
    };
    if !value.is_object() {
        value = serde_json::json!({});
    }
    value["deploymentMode"] = serde_json::json!(mode);
    write_json_with_backup(path, &value, tool, result)
}

pub(crate) fn chrono_like_stamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    now.to_string()
}
