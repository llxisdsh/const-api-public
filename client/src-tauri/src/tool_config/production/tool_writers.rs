fn upsert_claude_onboarding_state(
    mut root: serde_json::Value,
) -> serde_json::Value {
    ensure_json_object(&mut root);
    root["hasCompletedOnboarding"] = serde_json::json!(true);
    root
}

fn remove_json_environment_names(
    env: &mut serde_json::Map<String, serde_json::Value>,
    names: &[&str],
) {
    for name in names {
        env.remove(*name);
    }
}

fn upsert_gemini_auth_settings(
    mut root: serde_json::Value,
) -> serde_json::Value {
    ensure_json_object(&mut root);
    if !root
        .get("security")
        .is_some_and(serde_json::Value::is_object)
    {
        root["security"] = serde_json::json!({});
    }
    if !root["security"]
        .get("auth")
        .is_some_and(serde_json::Value::is_object)
    {
        root["security"]["auth"] = serde_json::json!({});
    }
    root["security"]["auth"]["selectedType"] = serde_json::json!("gemini-api-key");
    root
}

const CLAUDE_MODEL_CONFIG_PATHS: [&[&str]; 4] = [
    &["env", "ANTHROPIC_MODEL"],
    &["env", "ANTHROPIC_DEFAULT_OPUS_MODEL"],
    &["env", "ANTHROPIC_DEFAULT_SONNET_MODEL"],
    &["env", "ANTHROPIC_DEFAULT_HAIKU_MODEL"],
];

// These are tool hints, not public model identities or upstream SKUs.
pub(crate) fn claude_model_with_context(
    config: &crate::model::ClientConfig,
    model: &str,
    models: &[ToolModelInfo],
) -> String {
    let model = crate::config::public_model_name(model);
    let (_, _, supports_1m) = crate::tool_model_metadata::route_token_limits(config, &model, models);
    if supports_1m && model.starts_with("claude-") { format!("{model}[1m]") } else { model }
}

pub(crate) fn claude_settings_with_context(
    config: &crate::model::ClientConfig,
    settings: &ClaudeModelSettings,
    models: &[ToolModelInfo],
) -> ClaudeModelSettings {
    let mut settings = settings.clone();
    for model in [&mut settings.main, &mut settings.opus, &mut settings.sonnet, &mut settings.haiku] {
        if !model.trim().is_empty() && model.trim() != CLAUDE_MODEL_FOLLOW_MAIN {
            *model = claude_model_with_context(config, model, models);
        }
    }
    settings
}

const CLAUDE_DESKTOP_RETIRED_PROFILE_PATHS: &[&[&str]] = &[&["disableDeploymentModeChooser"]];

fn normalized_claude_model_choice(value: &str, field: &str) -> Result<Option<String>> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() > 512 || value.chars().any(char::is_control) {
        return Err(anyhow!("CLAUDE_MODEL_INVALID: {field}"));
    }
    Ok(Some(value.to_string()))
}

fn resolved_claude_role_model(
    value: &str,
    field: &str,
    main: &Option<String>,
) -> Result<Option<String>> {
    if value.trim() == CLAUDE_MODEL_FOLLOW_MAIN {
        Ok(main.clone())
    } else {
        normalized_claude_model_choice(value, field)
    }
}

fn resolved_claude_model_settings(
    settings: &ClaudeModelSettings,
) -> Result<[Option<String>; 4]> {
    if settings.main.trim() == CLAUDE_MODEL_FOLLOW_MAIN {
        return Err(anyhow!("CLAUDE_MODEL_INVALID: main cannot follow itself"));
    }
    let main = normalized_claude_model_choice(&settings.main, "main")?;
    let opus = resolved_claude_role_model(&settings.opus, "opus", &main)?;
    let sonnet = resolved_claude_role_model(&settings.sonnet, "sonnet", &main)?;
    let haiku = resolved_claude_role_model(&settings.haiku, "haiku", &main)?;
    Ok([
        main,
        opus,
        sonnet,
        haiku,
    ])
}

pub(crate) fn validate_claude_model_settings(settings: &ClaudeModelSettings) -> Result<()> {
    resolved_claude_model_settings(settings).map(|_| ())
}

fn upsert_claude_config(
    root_url: &str,
    api_key: &str,
    path: &Path,
    settings: &ClaudeModelSettings,
) -> Result<serde_json::Value> {
    let root_url = tool_surface_url(root_url, "anthropic");
    let mut root = read_json_or_default(path, serde_json::json!({}))?;
    ensure_json_object(&mut root);
    restore_managed_json_fields_for_reapply(
        "claude",
        path,
        &mut root,
        &CLAUDE_MODEL_CONFIG_PATHS,
    )?;
    if root.get("env").is_none() || !root["env"].is_object() {
        root["env"] = serde_json::json!({});
    }
    let env = root["env"]
        .as_object_mut()
        .ok_or_else(|| anyhow!("Claude settings env is not an object"))?;
    remove_json_environment_names(env, CLAUDE_CODE_CONFLICTING_ENV_NAMES);
    env.insert(
        "ANTHROPIC_BASE_URL".to_string(),
        serde_json::json!(root_url),
    );
    env.insert(
        "ANTHROPIC_AUTH_TOKEN".to_string(),
        serde_json::json!(api_key),
    );
    remove_json_string_if_equal(env, "ANTHROPIC_MODEL", "code-cheap");
    remove_json_string_if_equal(env, "ANTHROPIC_DEFAULT_HAIKU_MODEL", "code-cheap");
    remove_json_string_if_equal(env, "ANTHROPIC_DEFAULT_SONNET_MODEL", "code-cheap");
    remove_json_string_if_equal(env, "ANTHROPIC_DEFAULT_OPUS_MODEL", "code-cheap");

    for (name, model) in [
        "ANTHROPIC_MODEL",
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    ]
    .into_iter()
    .zip(resolved_claude_model_settings(settings)?)
    {
        if let Some(model) = model {
            env.insert(name.to_string(), serde_json::json!(model));
        }
    }
    Ok(root)
}

pub(crate) fn apply_claude_config(root_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    apply_claude_config_with_model_settings(root_url, api_key, &ClaudeModelSettings::default())
}

pub(crate) fn apply_claude_config_with_model_settings(
    root_url: &str,
    api_key: &str,
    settings: &ClaudeModelSettings,
) -> Result<ToolApplyResult> {
    apply_claude_config_with_model_info(root_url, api_key, settings, &[], &crate::default_config())
}

pub(crate) fn apply_claude_config_with_model_info(
    root_url: &str,
    api_key: &str,
    settings: &ClaudeModelSettings,
    models: &[ToolModelInfo],
    config: &crate::model::ClientConfig,
) -> Result<ToolApplyResult> {
    let path = home_dir().join(".claude").join("settings.json");
    let onboarding_path = home_dir().join(".claude.json");
    let mut result = ToolApplyBuilder::default();
    let mut root = upsert_claude_config(root_url, api_key, &path, settings)?;
    restore_managed_json_fields_for_reapply("claude", &path, &mut root, &[&["modelPicker"]])?;
    if !models.is_empty() {
        let ids = models.iter().map(|model| model.id.clone()).collect::<Vec<_>>();
        let options = crate::model_compatibility::anthropic_model_routes(config, &ids)
            .into_iter()
            .map(|id| serde_json::json!({
                "model": claude_model_with_context(config, &id, models),
                "label": id,
            }))
            .collect::<Vec<_>>();
        if !options.is_empty() {
            // Official user-settings schema (Claude Code 2.1.242+). One clean
            // row per route, while the selected value retains its context hint.
            root["modelPicker"] = serde_json::json!({
                "options": options,
                "replaceBuiltInOptions": true,
            });
        }
    }
    let onboarding = upsert_claude_onboarding_state(read_json_or_default(
        &onboarding_path,
        serde_json::json!({}),
    )?);
    write_json_with_backup(&path, &root, "claude", &mut result)?;
    write_json_with_backup(
        &onboarding_path,
        &onboarding,
        "claude",
        &mut result,
    )?;
    Ok(result.finish("claude"))
}

pub(crate) fn check_claude_config(root_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    check_claude_config_with_model_settings(root_url, api_key, &ClaudeModelSettings::default())
}

pub(crate) fn check_claude_config_with_model_settings(
    root_url: &str,
    api_key: &str,
    settings: &ClaudeModelSettings,
) -> Result<ToolApplyResult> {
    let path = home_dir().join(".claude").join("settings.json");
    let onboarding_path = home_dir().join(".claude.json");
    let mut result = ToolApplyBuilder::default();
    let mut root = upsert_claude_config(root_url, api_key, &path, settings)?;
    let current = read_json_or_default(&path, serde_json::json!({}))?;
    // The status dot is an offline check of the user's selection, not a fresh
    // capability probe. An auto-added context hint does not change that choice.
    // Explicitly requesting [1m] still requires it to be present in the file.
    for (field, expected) in CLAUDE_MODEL_CONFIG_PATHS
        .iter()
        .zip(resolved_claude_model_settings(settings)?)
    {
        let Some(expected) = expected else {
            continue;
        };
        let key = field[1];
        if crate::config::without_context_hint(&expected) != expected {
            continue;
        }
        if let Some(actual) = current
            .get("env")
            .and_then(|env| env.get(key))
            .and_then(serde_json::Value::as_str)
        {
            if actual != crate::config::without_context_hint(actual)
                && crate::config::without_context_hint(actual).eq_ignore_ascii_case(&expected)
            {
                root["env"][key] = serde_json::json!(actual);
            }
        }
    }
    let onboarding = upsert_claude_onboarding_state(read_json_or_default(
        &onboarding_path,
        serde_json::json!({}),
    )?);
    observe_json(&path, &root, &mut result)?;
    observe_json(&onboarding_path, &onboarding, &mut result)?;
    Ok(result.finish("claude"))
}

pub(crate) fn apply_claude_desktop_config(
    root_url: &str,
    api_key: &str,
) -> Result<ToolApplyResult> {
    apply_claude_desktop_config_with_models(root_url, api_key, &[])
}

fn claude_desktop_transaction_paths() -> Vec<PathBuf> {
    let paths = claude_desktop_paths();
    vec![
        paths.normal_config_path,
        paths.threep_config_path,
        paths.profile_path,
        paths.legacy_profile_path,
        paths.meta_path,
    ]
}

fn claude_desktop_meta_has_legacy_marker(meta: &serde_json::Value) -> bool {
    meta.get("appliedProfileId")
        .and_then(serde_json::Value::as_str)
        == Some(CLAUDE_DESKTOP_LEGACY_PROFILE_ID)
}

fn claude_desktop_meta_has_cc_switch_entry(meta: &serde_json::Value) -> bool {
    meta.get("entries")
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries.iter().any(|entry| {
                entry.get("id").and_then(serde_json::Value::as_str)
                    == Some(CLAUDE_DESKTOP_LEGACY_PROFILE_ID)
                    && entry
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .map(|name| name.eq_ignore_ascii_case("CC Switch"))
                        .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn claude_desktop_profile_matches_const_api(
    profile: &serde_json::Value,
    root_url: &str,
    api_key: &str,
) -> bool {
    let Some(profile_object) = profile.as_object() else {
        return false;
    };
    // Delete the shared-ID legacy file only when it still has the shape written
    // by old CONST API builds. Unknown fields may be user or third-party edits;
    // leaving an unselected orphan profile is safer than claiming them.
    const LEGACY_PROFILE_FIELDS: &[&str] = &[
        "coworkEgressAllowedHosts",
        // Kept only to recognize and clean profiles written by older releases.
        "disableDeploymentModeChooser",
        "inferenceGatewayApiKey",
        "inferenceGatewayAuthScheme",
        "inferenceGatewayBaseUrl",
        "inferenceModels",
        "inferenceProvider",
    ];
    if profile_object
        .keys()
        .any(|key| !LEGACY_PROFILE_FIELDS.contains(&key.as_str()))
    {
        return false;
    }
    let gateway = profile
        .get("inferenceGatewayBaseUrl")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let key = profile
        .get("inferenceGatewayApiKey")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    profile
        .get("inferenceProvider")
        .and_then(serde_json::Value::as_str)
        == Some("gateway")
        && (is_const_api_local_tool_url(gateway)
            || is_current_or_const_api_local_url(gateway, root_url)
            || key == api_key
            || key == LOCAL_PLACEHOLDER_KEY)
}

fn claude_desktop_meta_for_apply(mut meta: serde_json::Value) -> serde_json::Value {
    ensure_json_object(&mut meta);
    let Some(object) = meta.as_object_mut() else {
        return serde_json::json!({});
    };
    if object
        .get("appliedProfileId")
        .and_then(serde_json::Value::as_str)
        .map(|id| {
            id == CLAUDE_DESKTOP_LEGACY_PROFILE_ID || id == CLAUDE_DESKTOP_PROFILE_ID
        })
        .unwrap_or(false)
    {
        object.remove("appliedProfileId");
    }

    let entries = object
        .entry("entries".to_string())
        .or_insert_with(|| serde_json::json!([]));
    if !entries.is_array() {
        *entries = serde_json::json!([]);
    }
    if let Some(entries) = entries.as_array_mut() {
        entries.retain(|entry| {
            let id = entry.get("id").and_then(serde_json::Value::as_str);
            let name = entry.get("name").and_then(serde_json::Value::as_str);
            id != Some(CLAUDE_DESKTOP_PROFILE_ID)
                && !(id == Some(CLAUDE_DESKTOP_LEGACY_PROFILE_ID)
                    && name == Some(CLAUDE_DESKTOP_PROFILE_NAME))
        });
        entries.push(serde_json::json!({
            "id": CLAUDE_DESKTOP_PROFILE_ID,
            "name": CLAUDE_DESKTOP_PROFILE_NAME
        }));
    }
    object.insert(
        "appliedId".to_string(),
        serde_json::json!(CLAUDE_DESKTOP_PROFILE_ID),
    );
    meta
}

fn merge_tool_apply_results(
    mut result: ToolApplyResult,
    mut appended: ToolApplyResult,
) -> ToolApplyResult {
    result.files.append(&mut appended.files);
    result.backups.append(&mut appended.backups);
    result.file_statuses.append(&mut appended.file_statuses);
    result.details.extend(appended.details);
    result.already_configured = !result.file_statuses.is_empty()
        && result
            .file_statuses
            .iter()
            .all(|status| status.already_configured);
    result
}

fn read_claude_desktop_meta_for_apply(
    path: &Path,
    result: &mut ToolApplyResult,
) -> Result<serde_json::Value> {
    match read_json_or_default(path, serde_json::json!({})) {
        Ok(meta) => Ok(meta),
        Err(error) if path.exists() => {
            // A malformed _meta.json prevents Claude's own configuration UI
            // from loading. Preserve a timestamped backup, establish an empty
            // recoverable baseline, and let the normal writer create the
            // canonical schema. The surrounding transaction restores the
            // malformed file if a later step fails.
            let before = fs::read(path)?;
            record_post_restore_cleanup(result, path, before, None, "claude-desktop")?;
            result.details.insert(
                "claude_desktop_meta_repair".to_string(),
                format!("replaced_invalid_json: {error}"),
            );
            Ok(serde_json::json!({}))
        }
        Err(error) => Err(error),
    }
}

fn remove_json_field_if_equal(
    path: &Path,
    field: &str,
    expected: &str,
    tool: &str,
    result: &mut ToolApplyResult,
) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let before = fs::read(path)?;
    let mut value = serde_json::from_slice::<serde_json::Value>(&before)
        .with_context(|| format!("parse json {}", path.display()))?;
    let Some(object) = value.as_object_mut() else {
        return Ok(());
    };
    if object.get(field).and_then(serde_json::Value::as_str) != Some(expected) {
        return Ok(());
    }
    object.remove(field);
    let after = if object.is_empty() {
        None
    } else {
        let mut bytes = serde_json::to_vec_pretty(&value)?;
        bytes.push(b'\n');
        Some(bytes)
    };
    record_post_restore_cleanup(result, path, before, after, tool)
}

fn cleanup_legacy_claude_desktop_config(
    result: &mut ToolApplyResult,
    root_url: &str,
    api_key: &str,
    legacy_marker_was_present: bool,
) -> Result<bool> {
    if !legacy_marker_was_present {
        return Ok(false);
    }
    let paths = claude_desktop_paths();
    let mut meta = read_json_or_default(&paths.meta_path, serde_json::json!({}))?;
    ensure_json_object(&mut meta);
    let has_cc_switch_entry = claude_desktop_meta_has_cc_switch_entry(&meta);

    for path in [&paths.normal_config_path, &paths.threep_config_path] {
        remove_json_field_if_equal(path, "deploymentMode", "3p", "claude-desktop", result)?;
    }

    if paths.legacy_profile_path.exists() && !has_cc_switch_entry {
        let profile =
            read_json_or_default(&paths.legacy_profile_path, serde_json::json!({}))?;
        if claude_desktop_profile_matches_const_api(&profile, root_url, api_key) {
            let before = fs::read(&paths.legacy_profile_path)?;
            record_post_restore_cleanup(
                result,
                &paths.legacy_profile_path,
                before,
                None,
                "claude-desktop",
            )?;
        }
    }

    if paths.meta_path.exists() {
        let before = fs::read(&paths.meta_path)?;
        let object = meta
            .as_object_mut()
            .ok_or_else(|| anyhow!("Claude Desktop metadata is not an object"))?;
        if object
            .get("appliedProfileId")
            .and_then(serde_json::Value::as_str)
            == Some(CLAUDE_DESKTOP_LEGACY_PROFILE_ID)
        {
            object.remove("appliedProfileId");
        }
        if !has_cc_switch_entry
            && object.get("appliedId").and_then(serde_json::Value::as_str)
                == Some(CLAUDE_DESKTOP_LEGACY_PROFILE_ID)
        {
            object.remove("appliedId");
        }
        if let Some(entries) = object
            .get_mut("entries")
            .and_then(serde_json::Value::as_array_mut)
        {
            entries.retain(|entry| {
                !(entry.get("id").and_then(serde_json::Value::as_str)
                    == Some(CLAUDE_DESKTOP_LEGACY_PROFILE_ID)
                    && entry.get("name").and_then(serde_json::Value::as_str)
                        == Some(CLAUDE_DESKTOP_PROFILE_NAME))
            });
            if entries.is_empty() {
                object.remove("entries");
            }
        }
        let after = if object.is_empty() {
            None
        } else {
            let mut bytes = serde_json::to_vec_pretty(&meta)?;
            bytes.push(b'\n');
            Some(bytes)
        };
        record_post_restore_cleanup(
            result,
            &paths.meta_path,
            before,
            after,
            "claude-desktop",
        )?;
    }
    result.details.insert(
        "claude_desktop_legacy_migration".to_string(),
        "recovered_official_baseline".to_string(),
    );
    Ok(true)
}

pub(crate) fn apply_claude_desktop_config_with_models(
    root_url: &str,
    api_key: &str,
    model_ids: &[String],
) -> Result<ToolApplyResult> {
    apply_claude_desktop_config_with_models_and_config(
        root_url,
        api_key,
        model_ids,
        &crate::default_config(),
    )
}

pub(crate) fn apply_claude_desktop_config_with_models_and_config(
    root_url: &str,
    api_key: &str,
    model_ids: &[String],
    config: &crate::model::ClientConfig,
) -> Result<ToolApplyResult> {
    apply_claude_desktop_config_with_model_info(
        root_url, api_key, &crate::tool_model_metadata::tool_models_from_ids(model_ids), config,
    )
}

pub(crate) fn apply_claude_desktop_config_with_model_info(
    root_url: &str,
    api_key: &str,
    models: &[ToolModelInfo],
    config: &crate::model::ClientConfig,
) -> Result<ToolApplyResult> {
    let root_url = tool_surface_url(root_url, "anthropic");
    let transaction_paths = claude_desktop_transaction_paths();
    with_tool_config_file_transaction("claude-desktop", &transaction_paths, || {
        let paths = claude_desktop_paths();
        let mut migration_result = ToolApplyBuilder::default().finish("claude-desktop");
        let current_meta =
            read_claude_desktop_meta_for_apply(&paths.meta_path, &mut migration_result)?;
        let legacy_marker_was_present =
            claude_desktop_meta_has_legacy_marker(&current_meta);
        if legacy_marker_was_present {
            migration_result = merge_tool_apply_results(
                migration_result,
                restore_tool_config_from_manifest("claude-desktop")?,
            );
            cleanup_legacy_claude_desktop_config(
                &mut migration_result,
                &root_url,
                api_key,
                true,
            )?;
        }

        let mut result = ToolApplyBuilder::default();
        write_deployment_mode(
            &paths.normal_config_path,
            "3p",
            "claude-desktop",
            &mut result,
        )?;
        write_deployment_mode(
            &paths.threep_config_path,
            "3p",
            "claude-desktop",
            &mut result,
        )?;
        let mut profile = serde_json::json!({
            "coworkEgressAllowedHosts": ["*"],
            "inferenceGatewayApiKey": api_key,
            "inferenceGatewayAuthScheme": "bearer",
            "inferenceGatewayBaseUrl": root_url,
            "inferenceProvider": "gateway"
        });
        let inference_models = claude_desktop_inference_models(config, models);
        if !inference_models.is_empty() {
            profile["inferenceModels"] = serde_json::Value::Array(inference_models);
        } else if let Some(existing_models) = fs::read(&paths.profile_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|existing| existing.get("inferenceModels").cloned())
            .and_then(|models| models.as_array().filter(|models| !models.is_empty()).cloned())
        {
            profile["inferenceModels"] = serde_json::Value::Array(existing_models);
        }
        write_json_with_backup(
            &paths.profile_path,
            &profile,
            "claude-desktop",
            &mut result,
        )?;
        release_managed_fields_after_reapply(
            "claude-desktop",
            &paths.profile_path,
            CLAUDE_DESKTOP_RETIRED_PROFILE_PATHS,
        )?;
        let meta = claude_desktop_meta_for_apply(read_json_or_default(
            &paths.meta_path,
            serde_json::json!({}),
        )?);
        write_json_with_backup(&paths.meta_path, &meta, "claude-desktop", &mut result)?;
        let mut finished =
            merge_tool_apply_results(migration_result, result.finish("claude-desktop"));
        finished.details.insert(
            "claude_desktop_profile_id".to_string(),
            CLAUDE_DESKTOP_PROFILE_ID.to_string(),
        );
        finished.details.insert(
            "claude_desktop_config_schema".to_string(),
            "config_library_applied_id".to_string(),
        );
        Ok(finished)
    })
}

pub(crate) fn check_claude_desktop_config(
    root_url: &str,
    api_key: &str,
) -> Result<ToolApplyResult> {
    let root_url = tool_surface_url(root_url, "anthropic");
    let paths = claude_desktop_paths();
    let mut result = ToolApplyBuilder::default();
    observe_deployment_mode(&paths.normal_config_path, "3p", &mut result)?;
    observe_deployment_mode(&paths.threep_config_path, "3p", &mut result)?;
    let mut profile = read_json_or_default(&paths.profile_path, serde_json::json!({}))?;
    ensure_json_object(&mut profile);
    // The model catalog is written during configuration, but it is not a server-health signal.
    // Preserve it here so a transiently unavailable local server cannot change the status dot.
    profile["coworkEgressAllowedHosts"] = serde_json::json!(["*"]);
    profile["inferenceGatewayApiKey"] = serde_json::json!(api_key);
    profile["inferenceGatewayAuthScheme"] = serde_json::json!("bearer");
    profile["inferenceGatewayBaseUrl"] = serde_json::json!(root_url);
    profile["inferenceProvider"] = serde_json::json!("gateway");
    observe_json(&paths.profile_path, &profile, &mut result)?;
    let meta = claude_desktop_meta_for_apply(read_json_or_default(
        &paths.meta_path,
        serde_json::json!({}),
    )?);
    observe_json(&paths.meta_path, &meta, &mut result)?;
    Ok(result.finish("claude-desktop"))
}

pub(crate) fn apply_gemini_config(root_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    let root_url = tool_surface_url(root_url, "gemini");
    let path = home_dir().join(".gemini").join(".env");
    let settings_path = home_dir().join(".gemini").join("settings.json");
    let mut result = ToolApplyBuilder::default();
    let settings = upsert_gemini_auth_settings(read_json_or_default(
        &settings_path,
        serde_json::json!({}),
    )?);
    let content = upsert_env_vars(
        &read_text_or_empty(&path)?,
        &[
            ("GOOGLE_GEMINI_BASE_URL", &root_url),
            ("GEMINI_API_KEY", api_key),
        ],
    );
    let content = remove_env_vars_if_value(&content, &[("GEMINI_MODEL", "code-cheap")]);
    let content = remove_env_vars_if(&content, |name, _| {
        GEMINI_CLI_CONFLICTING_ENV_NAMES.contains(&name)
    });
    write_text_with_backup(&path, &content, "gemini", &mut result)?;
    write_json_with_backup(
        &settings_path,
        &settings,
        "gemini",
        &mut result,
    )?;
    Ok(result.finish("gemini"))
}

pub(crate) fn check_gemini_config(root_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    let root_url = tool_surface_url(root_url, "gemini");
    let path = home_dir().join(".gemini").join(".env");
    let settings_path = home_dir().join(".gemini").join("settings.json");
    let mut result = ToolApplyBuilder::default();
    let settings = upsert_gemini_auth_settings(read_json_or_default(
        &settings_path,
        serde_json::json!({}),
    )?);
    let content = upsert_env_vars(
        &read_text_or_empty(&path)?,
        &[
            ("GOOGLE_GEMINI_BASE_URL", &root_url),
            ("GEMINI_API_KEY", api_key),
        ],
    );
    let content = remove_env_vars_if_value(&content, &[("GEMINI_MODEL", "code-cheap")]);
    let content = remove_env_vars_if(&content, |name, _| {
        GEMINI_CLI_CONFLICTING_ENV_NAMES.contains(&name)
    });
    observe_text(&path, &content, &mut result)?;
    observe_json(&settings_path, &settings, &mut result)?;
    Ok(result.finish("gemini"))
}

fn workbuddy_models_path_from_config_dirs(
    home: &Path,
    workbuddy_config_dir: Option<&str>,
    codebuddy_config_dir: Option<&str>,
) -> PathBuf {
    [workbuddy_config_dir, codebuddy_config_dir]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".workbuddy"))
        .join("models.json")
}

pub(crate) fn workbuddy_models_path() -> PathBuf {
    let home = home_dir();
    if test_home_active() {
        return workbuddy_models_path_from_config_dirs(&home, None, None);
    }

    let workbuddy_config_dir = std::env::var("WORKBUDDY_CONFIG_DIR").ok();
    let codebuddy_config_dir = std::env::var("CODEBUDDY_CONFIG_DIR").ok();
    workbuddy_models_path_from_config_dirs(
        &home,
        workbuddy_config_dir.as_deref(),
        codebuddy_config_dir.as_deref(),
    )
}

fn workbuddy_models(root: &serde_json::Value) -> Result<&[serde_json::Value]> {
    match root {
        serde_json::Value::Array(models) => Ok(models),
        serde_json::Value::Object(object) => match object.get("models") {
            Some(serde_json::Value::Array(models)) => Ok(models),
            Some(_) => Err(anyhow!(
                "WorkBuddy models.json field `models` must contain a JSON array"
            )),
            None => Ok(&[]),
        },
        _ => Err(anyhow!(
            "WorkBuddy models.json must contain a JSON array or an object with a models array"
        )),
    }
}

fn workbuddy_models_mut(
    root: &mut serde_json::Value,
) -> Result<&mut Vec<serde_json::Value>> {
    match root {
        serde_json::Value::Array(models) => Ok(models),
        serde_json::Value::Object(object) => object
            .entry("models".to_string())
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .ok_or_else(|| {
                anyhow!("WorkBuddy models.json field `models` must contain a JSON array")
            }),
        _ => Err(anyhow!(
            "WorkBuddy models.json must contain a JSON array or an object with a models array"
        )),
    }
}

fn include_workbuddy_available_models(root: &mut serde_json::Value, model_ids: &[String]) {
    let Some(available_models) = root
        .as_object_mut()
        .and_then(|object| object.get_mut("availableModels"))
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };

    for model_id in model_ids {
        if !available_models
            .iter()
            .any(|value| value.as_str() == Some(model_id))
        {
            available_models.push(serde_json::json!(model_id));
        }
    }
}

fn workbuddy_model_is_available(
    root: &serde_json::Value,
    entry: &serde_json::Value,
) -> bool {
    let Some(available_models) = root
        .as_object()
        .and_then(|object| object.get("availableModels"))
        .and_then(serde_json::Value::as_array)
    else {
        return true;
    };
    let Some(model_id) = entry.get("id").and_then(serde_json::Value::as_str) else {
        return false;
    };
    available_models
        .iter()
        .any(|value| value.as_str() == Some(model_id))
}

fn workbuddy_endpoint_url(base_url: &str, protocol: ToolProtocol) -> Result<String> {
    if protocol != ToolProtocol::OpenAiChat {
        return Err(anyhow!(
            "WorkBuddy only supports OpenAI Chat Completions in CONST API"
        ));
    }
    Ok(tool_surface_url(base_url, "v1/chat/completions"))
}

fn workbuddy_model_matches_id(entry: &serde_json::Value, model_id: &str) -> bool {
    ["id", "name"].into_iter().any(|field| {
        entry
            .get(field)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            == Some(model_id)
    })
}

// Evidence for this allowlist is tool-specific, not a CONST API protocol
// preference. WorkBuddy 5.3.5's packaged REASONING_EFFORT_META exposes exactly
// these five picker entries. Its runtime recognizes `minimal` internally but
// has no UI entry for it, while disabling is represented separately through
// `canDisableThinking` rather than a `none` effort option.
const WORKBUDDY_REASONING_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];
// WorkBuddy keeps every custom endpoint in one flat model array rather than a
// provider namespace. Its config loader preserves unknown fields, so this
// marker is the least invasive way to distinguish our entries from user data.
const WORKBUDDY_CONST_MANAGED_FIELD: &str = "constApiManaged";

fn declared_reasoning_efforts(model: &ToolModelInfo) -> Vec<String> {
    if !model.reasoning {
        return Vec::new();
    }
    // ToolModelInfo has already normalized and deduplicated these labels. Do
    // not add a second protocol whitelist here: tools with open string schemas
    // must receive future provider labels unchanged.
    model.reasoning_efforts.clone()
}

fn workbuddy_reasoning_efforts(model: &ToolModelInfo) -> Vec<String> {
    declared_reasoning_efforts(model)
        .into_iter()
        .filter(|effort| {
            WORKBUDDY_REASONING_EFFORTS
                .iter()
                .any(|supported| effort == supported)
        })
        .collect()
}

fn configure_workbuddy_reasoning(
    object: &mut serde_json::Map<String, serde_json::Value>,
    model: &ToolModelInfo,
) {
    let supported_efforts = workbuddy_reasoning_efforts(model);
    let can_disable_thinking =
        model.reasoning && model.reasoning_efforts.iter().any(|effort| effort == "none");
    if supported_efforts.is_empty() && !can_disable_thinking {
        // In WorkBuddy an absent list means that the model supports a thinking
        // mode but does not expose the per-request effort selector.
        object.remove("reasoning");
        return;
    }

    let mut reasoning = object
        .remove("reasoning")
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    let Some(reasoning_object) = reasoning.as_object_mut() else {
        return;
    };
    if supported_efforts.is_empty() {
        reasoning_object.remove("supportedEfforts");
    } else {
        reasoning_object.insert(
            "supportedEfforts".to_string(),
            serde_json::json!(supported_efforts),
        );
    }
    if can_disable_thinking {
        reasoning_object.insert(
            "canDisableThinking".to_string(),
            serde_json::Value::Bool(true),
        );
    } else {
        reasoning_object.remove("canDisableThinking");
    }
    for field in ["effort", "defaultEffort"] {
        let keep = reasoning_object
            .get(field)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| {
                value.trim().is_empty()
                    || supported_efforts
                        .iter()
                        .any(|effort| effort == value.trim())
            });
        if reasoning_object.contains_key(field) && !keep {
            reasoning_object.remove(field);
        }
    }
    object.insert("reasoning".to_string(), reasoning);
}

fn configure_workbuddy_model_entry(
    entry: &mut serde_json::Value,
    model: &ToolModelInfo,
    endpoint_url: &str,
    api_key: &str,
) {
    if !entry.is_object() {
        *entry = serde_json::json!({});
    }
    let Some(object) = entry.as_object_mut() else {
        return;
    };
    object.insert("id".to_string(), serde_json::json!(model.id));
    // WorkBuddy renders a custom model as `name:id` whenever these fields
    // differ. Keep its name identical to the routing ID so the selector shows
    // one model label instead of two equivalent labels.
    object.insert("name".to_string(), serde_json::json!(model.id));
    object.insert("vendor".to_string(), serde_json::json!("Custom"));
    object.insert(
        WORKBUDDY_CONST_MANAGED_FIELD.to_string(),
        serde_json::Value::Bool(true),
    );
    object.insert("url".to_string(), serde_json::json!(endpoint_url));
    object.insert("apiKey".to_string(), serde_json::json!(api_key));
    object.insert(
        "supportsToolCall".to_string(),
        serde_json::json!(model.tool_call),
    );
    object.insert(
        "supportsImages".to_string(),
        serde_json::json!(model.input_modalities.iter().any(|value| value == "image")),
    );
    object.insert(
        "supportsReasoning".to_string(),
        serde_json::json!(model.reasoning),
    );
    configure_workbuddy_reasoning(object, model);
    if let Some(context_tokens) = model.context_tokens.filter(|tokens| *tokens > 0) {
        // WorkBuddy names this field maxInputTokens, but internally treats it
        // as the complete context window (the denominator of context usage).
        object.insert(
            "maxInputTokens".to_string(),
            serde_json::json!(context_tokens),
        );
        configure_workbuddy_context_window(object, context_tokens);
    } else {
        // A refreshed route with unknown limits must not inherit a stale 1M claim.
        object.remove("maxInputTokens");
        object.remove("contextWindow");
    }
    if let Some(output_tokens) = model.output_tokens.filter(|tokens| *tokens > 0) {
        object.insert(
            "maxOutputTokens".to_string(),
            serde_json::json!(output_tokens),
        );
    } else {
        object.remove("maxOutputTokens");
    }
    object.insert("useCustomProtocol".to_string(), serde_json::json!(false));
}

fn configure_workbuddy_context_window(
    object: &mut serde_json::Map<String, serde_json::Value>,
    context_tokens: u64,
) {
    // WorkBuddy 5.5.6's CLI and renderer both consume this budget schema.
    // Budgets may be smaller than the physical limit, never larger. Keep the
    // previous valid default; without a prior budget the CLI used the full limit.
    let mut window = object
        .remove("contextWindow")
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    let mut lengths = window
        .get("supportedLengths")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_u64)
        .filter(|value| *value > 0 && *value <= context_tokens)
        .collect::<Vec<_>>();
    if context_tokens > 200_000 {
        lengths.push(200_000);
    }
    lengths.push(context_tokens);
    lengths.sort_unstable();
    lengths.dedup();
    if lengths.len() < 2 {
        return;
    }
    let default = window
        .get("defaultLength")
        .and_then(serde_json::Value::as_u64)
        .filter(|value| lengths.contains(value))
        .unwrap_or(context_tokens);
    window["supportedLengths"] = serde_json::json!(lengths);
    window["defaultLength"] = serde_json::json!(default);
    object.insert("contextWindow".to_string(), window);
}

fn workbuddy_model_uses_endpoint(entry: &serde_json::Value, endpoint_url: &str) -> bool {
    entry
        .get("vendor")
        .and_then(serde_json::Value::as_str)
        .map(|vendor| vendor.eq_ignore_ascii_case("Custom"))
        .unwrap_or(false)
        && entry
            .get("url")
            .and_then(serde_json::Value::as_str)
            .map(|url| url.trim().trim_end_matches('/') == endpoint_url.trim_end_matches('/'))
            .unwrap_or(false)
}

fn workbuddy_model_uses_connection(
    entry: &serde_json::Value,
    endpoint_url: &str,
    api_key: &str,
) -> bool {
    workbuddy_model_uses_endpoint(entry, endpoint_url)
        && entry
            .get("apiKey")
            .and_then(serde_json::Value::as_str)
            == Some(api_key)
}

fn workbuddy_model_uses_const_local_proxy(entry: &serde_json::Value) -> bool {
    let is_custom = entry
        .get("vendor")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|vendor| vendor.eq_ignore_ascii_case("Custom"));
    let Some(authority) = entry
        .get("url")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .map(|url| url.trim_end_matches('/'))
        .and_then(|url| url.strip_prefix("http://"))
        .and_then(|url| url.strip_suffix("/v1/chat/completions"))
    else {
        return false;
    };

    is_custom
        && (authority.eq_ignore_ascii_case(crate::config::DEFAULT_LOCAL_PROXY_LISTEN)
            || authority.eq_ignore_ascii_case(crate::config::DEFAULT_DEVELOPMENT_PROXY_LISTEN))
}

fn workbuddy_model_has_const_marker(entry: &serde_json::Value) -> bool {
    entry
        .get(WORKBUDDY_CONST_MANAGED_FIELD)
        .and_then(serde_json::Value::as_bool)
        == Some(true)
}

fn prune_stale_workbuddy_const_profiles(
    root: &mut serde_json::Value,
    endpoint_url: &str,
    api_key: &str,
) -> Result<()> {
    let mut removed = HashSet::new();
    let models = workbuddy_models_mut(root)?;
    models.retain(|entry| {
        let legacy_other_profile = workbuddy_model_uses_const_local_proxy(entry)
            && !workbuddy_model_uses_endpoint(entry, endpoint_url);
        if (workbuddy_model_has_const_marker(entry) || legacy_other_profile)
            && !workbuddy_model_uses_connection(entry, endpoint_url, api_key)
        {
            if let Some(id) = entry.get("id").and_then(serde_json::Value::as_str) {
                removed.insert(id.to_string());
            }
            return false;
        }
        true
    });
    if removed.is_empty() {
        return Ok(());
    }

    let retained = models
        .iter()
        .filter_map(|entry| entry.get("id").and_then(serde_json::Value::as_str))
        .map(str::to_string)
        .collect::<HashSet<_>>();
    if let Some(available) = root
        .get_mut("availableModels")
        .and_then(serde_json::Value::as_array_mut)
    {
        available.retain(|value| {
            value
                .as_str()
                .is_none_or(|id| !removed.contains(id) || retained.contains(id))
        });
    }
    Ok(())
}

fn workbuddy_model_is_configured(
    entry: &serde_json::Value,
    endpoint_url: &str,
    api_key: &str,
) -> bool {
    workbuddy_model_uses_connection(entry, endpoint_url, api_key)
        && entry
            .get("id")
            .and_then(serde_json::Value::as_str)
            .zip(entry.get("name").and_then(serde_json::Value::as_str))
            .is_some_and(|(id, name)| !id.trim().is_empty() && name == id)
        && ["supportsToolCall", "supportsImages", "supportsReasoning"]
            .into_iter()
            .all(|field| entry.get(field).is_some_and(serde_json::Value::is_boolean))
        && entry
            .get("useCustomProtocol")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
}

pub(crate) fn apply_workbuddy_config_with_model_info_for_protocol(
    base_url: &str,
    api_key: &str,
    models: &[ToolModelInfo],
    protocol: ToolProtocol,
) -> Result<ToolApplyResult> {
    resolve_tool_protocol("workbuddy", Some(protocol.as_str()))?;
    if models.is_empty() {
        return Err(anyhow!(
            "WORKBUDDY_MODELS_REQUIRED: no available models were returned for WorkBuddy"
        ));
    }
    let endpoint_url = workbuddy_endpoint_url(base_url, protocol)?;
    let path = workbuddy_models_path();
    let mut root = read_json_or_default(&path, serde_json::json!([]))?;
    // WorkBuddy stores all custom models in one additive array. Replace models
    // marked by another CONST profile; the two legacy loopback ports migrate
    // entries written before the explicit ownership marker existed.
    prune_stale_workbuddy_const_profiles(&mut root, &endpoint_url, api_key)?;
    normalize_workbuddy_managed_model_ids(&mut root, &endpoint_url, api_key, &[])?;
    let mut models = models.iter().collect::<Vec<_>>();
    models.sort_by(|left, right| crate::tool_model_metadata::tool_model_display_order(left, right));
    let model_ids = models
        .iter()
        .map(|model| model.id.clone())
        .collect::<Vec<_>>();
    prune_workbuddy_managed_models(&mut root, &endpoint_url, api_key, &model_ids)?;
    {
        let configured_models = workbuddy_models_mut(&mut root)?;
        for model in models {
            if let Some(entry) = configured_models
                .iter_mut()
                .find(|entry| workbuddy_model_matches_id(entry, model.id.trim()))
            {
                configure_workbuddy_model_entry(entry, model, &endpoint_url, api_key);
            } else {
                let mut entry = serde_json::json!({});
                configure_workbuddy_model_entry(&mut entry, model, &endpoint_url, api_key);
                configured_models.push(entry);
            }
        }
    }
    include_workbuddy_available_models(&mut root, &model_ids);
    normalize_workbuddy_managed_model_ids(&mut root, &endpoint_url, api_key, &model_ids)?;
    let mut result = ToolApplyBuilder::default();
    write_json_with_backup(&path, &root, "workbuddy", &mut result)?;
    let mut result = result.finish("workbuddy");
    attach_tool_protocol(&mut result, protocol);
    Ok(result)
}

fn prune_workbuddy_managed_models(
    root: &mut serde_json::Value,
    endpoint_url: &str,
    api_key: &str,
    model_ids: &[String],
) -> Result<()> {
    let live = model_ids.iter().map(String::as_str).collect::<HashSet<_>>();
    let mut removed = HashSet::new();
    let models = workbuddy_models_mut(root)?;
    models.retain(|entry| {
        let Some(id) = entry.get("id").and_then(serde_json::Value::as_str) else {
            return true;
        };
        if workbuddy_model_uses_connection(entry, endpoint_url, api_key) && !live.contains(id) {
            removed.insert(id.to_string());
            false
        } else {
            true
        }
    });
    // A different connection can legitimately expose the same model ID.
    for entry in models.iter() {
        if let Some(id) = entry.get("id").and_then(serde_json::Value::as_str) {
            removed.remove(id);
        }
    }
    if let Some(available) = root
        .get_mut("availableModels")
        .and_then(serde_json::Value::as_array_mut)
    {
        available.retain(|value| value.as_str().is_none_or(|id| !removed.contains(id)));
    }
    Ok(())
}

// WorkBuddy updates an additive array rather than replacing a managed provider
// block. Migrate only this CONST connection's old entries; preserve other URLs,
// keys and user fields, and let the existing backup/restore machinery own writes.
fn normalize_workbuddy_managed_model_ids(
    root: &mut serde_json::Value,
    endpoint_url: &str,
    api_key: &str,
    ordered_ids: &[String],
) -> Result<()> {
    let ranks = ordered_ids.iter().enumerate().map(|(index, id)| (id.as_str(), index)).collect::<std::collections::HashMap<_, _>>();
    let rank = |id: &str| ranks.get(id).copied().unwrap_or(usize::MAX);
    let mut renamed = std::collections::HashMap::new();
    let models = workbuddy_models_mut(root)?;
    let mut seen = std::collections::HashSet::new();
    models.retain_mut(|entry| {
        if !workbuddy_model_uses_connection(entry, endpoint_url, api_key) {
            return true;
        }
        let Some(old_id) = entry.get("id").and_then(serde_json::Value::as_str) else {
            return true;
        };
        let id = crate::config::public_model_name(old_id);
        if id.is_empty() {
            return true;
        }
        renamed.insert(old_id.to_string(), id.clone());
        entry["id"] = serde_json::json!(id);
        entry["name"] = serde_json::json!(id);
        seen.insert(id)
    });
    let mut ordered = models
        .iter()
        .filter(|entry| workbuddy_model_uses_connection(entry, endpoint_url, api_key))
        .cloned()
        .collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        let a = left["id"].as_str().unwrap_or_default();
        let b = right["id"].as_str().unwrap_or_default();
        rank(a).cmp(&rank(b)).then_with(|| a.cmp(b))
    });
    let mut ordered = ordered.into_iter();
    for entry in models.iter_mut() {
        if workbuddy_model_uses_connection(entry, endpoint_url, api_key) {
            if let Some(replacement) = ordered.next() {
                *entry = replacement;
            }
        }
    }
    if let Some(available) = root
        .get_mut("availableModels")
        .and_then(serde_json::Value::as_array_mut)
    {
        let owned = renamed
            .values()
            .map(String::as_str)
            .collect::<std::collections::HashSet<_>>();
        let mut seen = std::collections::HashSet::new();
        available.retain_mut(|value| {
            if let Some(id) = value.as_str().and_then(|id| renamed.get(id)) {
                *value = serde_json::json!(id);
            }
            value
                .as_str()
                .is_none_or(|id| !owned.contains(id) || seen.insert(id.to_string()))
        });
        let mut ordered = seen.into_iter().collect::<Vec<_>>();
        ordered.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.cmp(b)));
        let mut ordered = ordered.into_iter();
        for value in available {
            if value.as_str().is_some_and(|id| owned.contains(id)) {
                if let Some(id) = ordered.next() {
                    *value = serde_json::json!(id);
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn check_workbuddy_config(
    base_url: &str,
    api_key: &str,
) -> Result<ToolApplyResult> {
    let protocol = ToolProtocol::OpenAiChat;
    let endpoint_url = workbuddy_endpoint_url(base_url, protocol)?;
    let path = workbuddy_models_path();
    let root = read_json_or_default(&path, serde_json::json!([]))?;
    let models_configured = workbuddy_models(&root)?
        .iter()
        .any(|entry| {
            workbuddy_model_is_configured(entry, &endpoint_url, api_key)
                && workbuddy_model_is_available(&root, entry)
        });
    let mut builder = ToolApplyBuilder::default();
    observe_json(&path, &root, &mut builder)?;
    let mut result = builder.finish("workbuddy");
    result.already_configured &= models_configured;
    attach_tool_protocol(&mut result, protocol);
    Ok(result)
}

fn cleanup_unmanaged_workbuddy_models(
    result: &mut ToolApplyResult,
    base_url: &str,
    api_key: &str,
) -> Result<()> {
    let path = workbuddy_models_path();
    if !path.exists() {
        return Ok(());
    }
    let endpoint_url = workbuddy_endpoint_url(base_url, ToolProtocol::OpenAiChat)?;
    let before = fs::read(&path)?;
    let mut root = serde_json::from_slice::<serde_json::Value>(&before)
        .with_context(|| format!("parse {}", path.display()))?;
    let models = workbuddy_models_mut(&mut root)?;
    let original_len = models.len();
    models.retain(|entry| !workbuddy_model_uses_connection(entry, &endpoint_url, api_key));
    if models.len() == original_len {
        return Ok(());
    }
    let mut after = serde_json::to_vec_pretty(&root)?;
    after.push(b'\n');
    record_post_restore_cleanup(result, &path, before, Some(after), "workbuddy")
}

pub(crate) fn remove_workbuddy_config(
    base_url: &str,
    api_key: &str,
) -> Result<ToolApplyResult> {
    let mut result = restore_tool_config_from_manifest("workbuddy")?;
    if result
        .details
        .get("manifest_entries_restored")
        .map(String::as_str)
        == Some("0")
    {
        cleanup_unmanaged_workbuddy_models(&mut result, base_url, api_key)?;
    }
    Ok(result)
}

fn vscode_api_type(protocol: ToolProtocol) -> Result<&'static str> {
    match protocol {
        ToolProtocol::OpenAiResponses => Ok("responses"),
        ToolProtocol::OpenAiChat => Ok("chat-completions"),
        _ => Err(anyhow!("VS Code only supports OpenAI protocols in CONST API")),
    }
}

fn vscode_endpoint_url(base_url: &str, protocol: ToolProtocol) -> Result<String> {
    let operation = match protocol {
        ToolProtocol::OpenAiResponses => "responses",
        ToolProtocol::OpenAiChat => "chat/completions",
        _ => return Err(anyhow!("VS Code only supports OpenAI protocols in CONST API")),
    };
    Ok(format!("{}/{}", base_url.trim_end_matches('/'), operation))
}

fn vscode_model_entry(
    model: &ToolModelInfo,
    endpoint_url: &str,
    api_key: &str,
    protocol: ToolProtocol,
) -> serde_json::Value {
    let display_name = if model.display_name.trim().is_empty() {
        &model.id
    } else {
        &model.display_name
    };
    let mut entry = serde_json::json!({
        "id": model.id,
        "name": display_name,
        "url": endpoint_url,
        "toolCalling": model.tool_call,
        "vision": model.input_modalities.iter().any(|value| value == "image"),
        "thinking": model.reasoning,
        "streaming": true,
        "requestHeaders": {
            "Authorization": format!("Bearer {}", api_key.trim())
        }
    });
    if let Some(output_tokens) = model.output_tokens {
        entry["maxOutputTokens"] = serde_json::json!(output_tokens);
    }
    if let Some(context_tokens) = model.context_tokens {
        // Current VS Code treats contextWindow as the source of truth. Keep
        // maxInputTokens too for compatibility with older BYOK schemas.
        entry["contextWindow"] = serde_json::json!(context_tokens);
        let input_tokens = context_tokens.saturating_sub(model.output_tokens.unwrap_or_default());
        if input_tokens > 0 {
            entry["maxInputTokens"] = serde_json::json!(input_tokens);
        }
    }
    // VS Code declares this field as `string[]`, and its endpoint tests
    // intentionally accept unknown future levels. Preserve the catalog list
    // instead of constraining it to today's OpenAI vocabulary.
    let reasoning_efforts = declared_reasoning_efforts(model);
    if !reasoning_efforts.is_empty() {
        entry["supportsReasoningEffort"] = serde_json::json!(reasoning_efforts);
        let format = match protocol {
            ToolProtocol::OpenAiResponses => "responses",
            ToolProtocol::OpenAiChat => "chat-completions",
            _ => return entry,
        };
        entry["reasoningEffortFormat"] = serde_json::json!(format);
    }
    entry
}

fn vscode_const_api_provider(
    base_url: &str,
    api_key: &str,
    models: &[ToolModelInfo],
    protocol: ToolProtocol,
) -> Result<serde_json::Value> {
    let endpoint_url = vscode_endpoint_url(base_url, protocol)?;
    let mut models = models.iter().collect::<Vec<_>>();
    models.sort_by(|left, right| crate::tool_model_metadata::tool_model_display_order(left, right));
    Ok(serde_json::json!({
        "name": CONST_API_DISPLAY_NAME,
        "vendor": "customendpoint",
        "apiKey": api_key,
        "apiType": vscode_api_type(protocol)?,
        "models": models
            .into_iter()
            .map(|model| vscode_model_entry(model, &endpoint_url, api_key, protocol))
            .collect::<Vec<_>>()
    }))
}

fn vscode_const_api_provider_for_status(
    existing: Option<&serde_json::Value>,
    base_url: &str,
    api_key: &str,
    protocol: ToolProtocol,
) -> Result<(serde_json::Value, bool)> {
    let Some(mut provider) = existing.filter(|value| value.is_object()).cloned() else {
        return Ok((
            vscode_const_api_provider(base_url, api_key, &[], protocol)?,
            false,
        ));
    };
    let endpoint_url = vscode_endpoint_url(base_url, protocol)?;
    let provider_object = provider
        .as_object_mut()
        .ok_or_else(|| anyhow!("VS Code provider is not an object"))?;
    provider_object.insert("name".to_string(), serde_json::json!(CONST_API_DISPLAY_NAME));
    provider_object.insert("vendor".to_string(), serde_json::json!("customendpoint"));
    provider_object.insert("apiKey".to_string(), serde_json::json!(api_key));
    provider_object.insert(
        "apiType".to_string(),
        serde_json::json!(vscode_api_type(protocol)?),
    );

    // Tool status is intentionally local-only. Keep the last configured model catalog while
    // checking the endpoint, key and protocol fields that CONST API owns.
    let mut models_configured = false;
    if let Some(models) = provider_object
        .get_mut("models")
        .and_then(serde_json::Value::as_array_mut)
    {
        models_configured = !models.is_empty();
        for model in models {
            let Some(model_object) = model.as_object_mut() else {
                models_configured = false;
                continue;
            };
            if model_object
                .get("id")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|id| id.trim().is_empty())
            {
                models_configured = false;
            }
            model_object.insert("url".to_string(), serde_json::json!(endpoint_url));
            model_object.insert("streaming".to_string(), serde_json::json!(true));
            let request_headers = model_object
                .entry("requestHeaders".to_string())
                .or_insert_with(|| serde_json::json!({}));
            if !request_headers.is_object() {
                *request_headers = serde_json::json!({});
            }
            request_headers["Authorization"] =
                serde_json::json!(format!("Bearer {}", api_key.trim()));
        }
    }
    Ok((provider, models_configured))
}

fn upsert_vscode_const_api_provider(
    root: &mut serde_json::Value,
    provider: serde_json::Value,
) -> Result<()> {
    let providers = root
        .as_array_mut()
        .ok_or_else(|| anyhow!("VS Code chatLanguageModels.json must contain a JSON array"))?;
    providers.retain(|entry| {
        entry.get("name").and_then(serde_json::Value::as_str) != Some(CONST_API_DISPLAY_NAME)
    });
    providers.push(provider);
    Ok(())
}

const VSCODE_BYOK_UTILITY_MODEL_DEFAULT: &str = "chat.byokUtilityModelDefault";

fn configure_vscode_byok_utility_model(root: &mut serde_json::Value) -> Result<()> {
    let settings = root
        .as_object_mut()
        .ok_or_else(|| anyhow!("VS Code settings.json must contain a JSON object"))?;
    // VS Code's bundled Copilot extension otherwise defaults BYOK utility resolution to `none`
    // and fails locally before any request is sent. Reusing the selected main model is the
    // extension's supported availability-first fallback and avoids inventing a proxy model alias.
    settings.insert(
        VSCODE_BYOK_UTILITY_MODEL_DEFAULT.to_string(),
        serde_json::Value::String("mainAgent".to_string()),
    );
    Ok(())
}

pub(crate) fn apply_vscode_config_with_model_info_for_protocol(
    base_url: &str,
    api_key: &str,
    models: &[ToolModelInfo],
    protocol: ToolProtocol,
) -> Result<ToolApplyResult> {
    resolve_tool_protocol("vscode", Some(protocol.as_str()))?;
    let path = vscode_chat_language_models_path();
    let mut root = read_json_or_default(&path, serde_json::json!([]))?;
    let provider = vscode_const_api_provider(base_url, api_key, models, protocol)?;
    upsert_vscode_const_api_provider(&mut root, provider)?;
    let settings_path = vscode_user_settings_path();
    let mut settings = read_json5_or_default(&settings_path, serde_json::json!({}))?;
    configure_vscode_byok_utility_model(&mut settings)?;
    let mut result = ToolApplyBuilder::default();
    write_json_with_backup(&path, &root, "vscode", &mut result)?;
    write_json_with_backup(&settings_path, &settings, "vscode", &mut result)?;
    let mut result = result.finish("vscode");
    attach_tool_protocol(&mut result, protocol);
    Ok(result)
}

pub(crate) fn check_vscode_config(
    base_url: &str,
    api_key: &str,
) -> Result<ToolApplyResult> {
    let path = vscode_chat_language_models_path();
    let mut root = read_json_or_default(&path, serde_json::json!([]))?;
    let existing_provider = root
        .as_array()
        .and_then(|providers| {
            providers.iter().find(|entry| {
                entry.get("name").and_then(serde_json::Value::as_str)
                    == Some(CONST_API_DISPLAY_NAME)
            })
        });
    let protocol = existing_provider
        .and_then(|provider| provider.get("apiType").and_then(serde_json::Value::as_str))
        .and_then(|value| match value {
            "responses" => Some(ToolProtocol::OpenAiResponses),
            "chat-completions" => Some(ToolProtocol::OpenAiChat),
            _ => None,
        })
        .unwrap_or(ToolProtocol::OpenAiResponses);
    let (provider, models_configured) =
        vscode_const_api_provider_for_status(existing_provider, base_url, api_key, protocol)?;
    upsert_vscode_const_api_provider(&mut root, provider)?;
    let settings_path = vscode_user_settings_path();
    let mut settings = read_json5_or_default(&settings_path, serde_json::json!({}))?;
    configure_vscode_byok_utility_model(&mut settings)?;
    let mut result = ToolApplyBuilder::default();
    observe_json(&path, &root, &mut result)?;
    observe_json(&settings_path, &settings, &mut result)?;
    let mut result = result.finish("vscode");
    result.already_configured &= models_configured;
    attach_tool_protocol(&mut result, protocol);
    Ok(result)
}

pub(crate) fn remove_vscode_config() -> Result<ToolApplyResult> {
    let mut result = restore_tool_config_from_manifest("vscode")?;
    // Older releases wrote VS Code's CONST API provider before the ownership
    // manifest existed. With no manifest entry, a normal restore is a no-op
    // and the provider remains active (and the dock status stays green).
    // Remove only our unmistakably branded legacy provider in that case;
    // manifest-backed cancellation still restores the user's original value.
    if result
        .details
        .get("manifest_entries_restored")
        .map(String::as_str)
        == Some("0")
    {
        cleanup_unmanaged_vscode_provider_aliases(&mut result)?;
    }
    Ok(result)
}
#[cfg(test)]
pub(crate) fn apply_opencode_config(base_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    apply_opencode_config_with_models(base_url, api_key, &[])
}

#[cfg(test)]
pub(crate) fn apply_opencode_config_with_models(
    base_url: &str,
    api_key: &str,
    model_ids: &[String],
) -> Result<ToolApplyResult> {
    apply_opencode_config_with_model_info(base_url, api_key, &tool_models_from_ids(model_ids))
}

#[cfg(test)]
pub(crate) fn apply_opencode_config_with_model_info(
    base_url: &str,
    api_key: &str,
    models: &[ToolModelInfo],
) -> Result<ToolApplyResult> {
    apply_opencode_config_with_model_info_for_protocol(
        base_url,
        api_key,
        models,
        ToolProtocol::OpenAiResponses,
    )
}

pub(crate) fn apply_opencode_config_with_model_info_for_protocol(
    base_url: &str,
    api_key: &str,
    models: &[ToolModelInfo],
    protocol: ToolProtocol,
) -> Result<ToolApplyResult> {
    resolve_tool_protocol("opencode", Some(protocol.as_str()))?;
    let path = opencode_config_path()?;
    let mut result = ToolApplyBuilder::default();
    let mut root = read_json5_or_default(&path, serde_json::json!({}))?;
    ensure_json_object(&mut root);
    if root.get("provider").is_none() {
        root["provider"] = serde_json::json!({});
    }
    remove_opencode_legacy_const_api_providers(&mut root, base_url, api_key);
    root["provider"][CODEX_CONST_API_PROVIDER_ID] =
        opencode_const_api_provider(base_url, api_key, models, protocol);
    let content = serde_json::to_string_pretty(&root)?;
    write_text_with_backup(&path, &(content + "\n"), "opencode", &mut result)?;
    let mut result = result.finish("opencode");
    attach_tool_protocol(&mut result, protocol);
    Ok(result)
}

pub(crate) fn check_opencode_config(base_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    let path = opencode_config_path()?;
    let mut result = ToolApplyBuilder::default();
    let mut root = read_json5_or_default(&path, serde_json::json!({}))?;
    let protocol = detect_opencode_tool_protocol(&root).unwrap_or(ToolProtocol::OpenAiResponses);
    ensure_json_object(&mut root);
    if root.get("provider").is_none() {
        root["provider"] = serde_json::json!({});
    }
    let existing_provider = root["provider"]
        .get(CODEX_CONST_API_PROVIDER_ID)
        .filter(|value| value.is_object())
        .cloned();
    remove_opencode_legacy_const_api_providers(&mut root, base_url, api_key);
    let mut provider = existing_provider
        .unwrap_or_else(|| opencode_const_api_provider(base_url, api_key, &[], protocol));
    ensure_json_object(&mut provider);
    provider["npm"] = serde_json::json!(opencode_provider_package(protocol));
    provider["name"] = serde_json::json!(CONST_API_DISPLAY_NAME);
    if provider.get("options").is_none_or(|value| !value.is_object()) {
        provider["options"] = serde_json::json!({});
    }
    provider["options"]["baseURL"] =
        serde_json::json!(tool_surface_url(base_url, protocol.surface()));
    provider["options"]["apiKey"] = serde_json::json!(api_key);
    if matches!(protocol, ToolProtocol::OpenAiResponses) {
        remove_opencode_legacy_cache_disable(&mut provider);
    }
    root["provider"][CODEX_CONST_API_PROVIDER_ID] = provider;
    let content = serde_json::to_string_pretty(&root)? + "\n";
    observe_text(&path, &content, &mut result)?;
    let mut result = result.finish("opencode");
    attach_tool_protocol(&mut result, protocol);
    Ok(result)
}

#[cfg(test)]
pub(crate) fn apply_openclaw_config(root_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    apply_openclaw_config_for_protocol(root_url, api_key, ToolProtocol::OpenAiResponses)
}

pub(crate) fn apply_openclaw_config_for_protocol(
    root_url: &str,
    api_key: &str,
    protocol: ToolProtocol,
) -> Result<ToolApplyResult> {
    apply_openclaw_config_with_model_info_for_protocol(
        root_url,
        api_key,
        &[],
        protocol,
    )
}

fn openclaw_model_entries(models: &[ToolModelInfo]) -> Vec<serde_json::Value> {
    let mut models = models.iter().collect::<Vec<_>>();
    let mut seen = HashSet::new();
    models.retain(|model| seen.insert(model.id.as_str()));
    models.sort_by(|left, right| crate::tool_model_metadata::tool_model_display_order(left, right));
    models
        .into_iter()
        .filter_map(|model| {
            let id = model.id.trim();
            if id.is_empty() || id == "code-cheap" || id == LOCAL_PLACEHOLDER_KEY {
                return None;
            }
            let display_name = if model.display_name.trim().is_empty() {
                id
            } else {
                model.display_name.trim()
            };
            let mut input = model
                .input_modalities
                .iter()
                .filter(|modality| {
                    matches!(modality.as_str(), "text" | "image" | "video" | "audio")
                })
                .cloned()
                .collect::<Vec<_>>();
            if !input.iter().any(|modality| modality == "text") {
                input.push("text".to_string());
            }
            // OpenClaw's config schema deliberately accepts non-empty strings
            // for both the effort list and mapping values. Preserve provider
            // labels so newer levels remain available without a client update.
            let efforts = declared_reasoning_efforts(model);
            let mut compat = serde_json::json!({
                "supportsTools": model.tool_call
            });
            if !efforts.is_empty() {
                compat["supportsReasoningEffort"] = serde_json::Value::Bool(true);
                compat["supportedReasoningEfforts"] = serde_json::json!(&efforts);
                if efforts.iter().any(|effort| effort == "none") {
                    // OpenClaw's user-facing disabled level is `off`; OpenAI
                    // compatible endpoints use `none` on the wire.
                    compat["reasoningEffortMap"] = serde_json::json!({
                        "off": "none",
                        "none": "none"
                    });
                }
            }
            let mut entry = serde_json::json!({
                "id": id,
                "name": display_name,
                "reasoning": model.reasoning,
                "input": input,
                "compat": compat
            });
            if let Some(context_tokens) = model.context_tokens {
                entry["contextWindow"] = serde_json::json!(context_tokens);
            }
            if let Some(output_tokens) = model.output_tokens {
                entry["maxTokens"] = serde_json::json!(output_tokens);
            }
            Some(entry)
        })
        .collect()
}

pub(crate) fn apply_openclaw_config_with_model_info_for_protocol(
    root_url: &str,
    api_key: &str,
    models: &[ToolModelInfo],
    protocol: ToolProtocol,
) -> Result<ToolApplyResult> {
    let root_url = tool_surface_url(root_url, protocol.surface());
    let path = openclaw_config_path()?;
    let mut result = ToolApplyBuilder::default();
    let mut root = read_json5_or_default(&path, serde_json::json!({}))?;
    ensure_json_object(&mut root);
    if root.get("models").is_none() || !root["models"].is_object() {
        root["models"] = serde_json::json!({"mode": "merge", "providers": {}});
    }
    if root["models"].get("providers").is_none() || !root["models"]["providers"].is_object() {
        root["models"]["providers"] = serde_json::json!({});
    }
    remove_openclaw_const_api_providers(&mut root, &root_url, api_key);
    root["models"]["providers"][CODEX_CONST_API_PROVIDER_ID] = serde_json::json!({
        "baseUrl": root_url,
        "apiKey": api_key,
        "api": openclaw_api_mode(protocol),
        "models": openclaw_model_entries(models)
    });
    remove_openclaw_legacy_default_model(&mut root);
    write_json_with_backup(&path, &root, "openclaw", &mut result)?;
    let mut result = result.finish("openclaw");
    attach_tool_protocol(&mut result, protocol);
    Ok(result)
}

pub(crate) fn check_openclaw_config(root_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    let path = openclaw_config_path()?;
    let mut result = ToolApplyBuilder::default();
    let mut root = read_json5_or_default(&path, serde_json::json!({}))?;
    let protocol = detect_openclaw_tool_protocol(&root).unwrap_or(ToolProtocol::OpenAiResponses);
    let root_url = tool_surface_url(root_url, protocol.surface());
    ensure_json_object(&mut root);
    if root.get("models").is_none() || !root["models"].is_object() {
        root["models"] = serde_json::json!({"mode": "merge", "providers": {}});
    }
    if root["models"].get("providers").is_none() || !root["models"]["providers"].is_object() {
        root["models"]["providers"] = serde_json::json!({});
    }
    let existing_models = root["models"]["providers"]
        .get(CODEX_CONST_API_PROVIDER_ID)
        .or_else(|| root["models"]["providers"].get(LEGACY_CONST_API_PROVIDER_ID))
        .and_then(|provider| provider.get("models"))
        .cloned();
    remove_openclaw_const_api_providers(&mut root, &root_url, api_key);
    let mut provider = serde_json::json!({
        "baseUrl": root_url,
        "apiKey": api_key,
        "api": openclaw_api_mode(protocol)
    });
    if let Some(models) = existing_models {
        provider["models"] = models;
    }
    root["models"]["providers"][CODEX_CONST_API_PROVIDER_ID] = provider;
    remove_openclaw_legacy_default_model(&mut root);
    observe_json(&path, &root, &mut result)?;
    let mut result = result.finish("openclaw");
    attach_tool_protocol(&mut result, protocol);
    Ok(result)
}

#[cfg(test)]
pub(crate) fn apply_hermes_config(root_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    apply_hermes_config_for_protocol(root_url, api_key, ToolProtocol::OpenAiChat)
}

pub(crate) fn apply_hermes_config_for_protocol(
    root_url: &str,
    api_key: &str,
    protocol: ToolProtocol,
) -> Result<ToolApplyResult> {
    resolve_tool_protocol("hermes", Some(protocol.as_str()))?;
    let mut result = ToolApplyBuilder::default();
    let mut plans = Vec::new();
    for path in hermes_config_paths() {
        let raw = if path.exists() {
            fs::read_to_string(&path)?
        } else {
            String::new()
        };
        let content = upsert_hermes_sections_for_protocol(&raw, root_url, api_key, protocol)?;
        plans.push((path, content));
    }
    for (path, content) in plans {
        write_text_with_backup(&path, &content, "hermes", &mut result)?;
    }
    let mut result = result.finish("hermes");
    attach_tool_protocol(&mut result, protocol);
    Ok(result)
}

pub(crate) fn check_hermes_config(root_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    let mut result = ToolApplyBuilder::default();
    let configs = hermes_config_paths()
        .into_iter()
        .map(|path| {
            let raw = if path.exists() {
                fs::read_to_string(&path)?
            } else {
                String::new()
            };
            Ok((path, raw))
        })
        .collect::<Result<Vec<_>>>()?;
    let protocol = configs
        .iter()
        .find_map(|(_, raw)| detect_hermes_tool_protocol(raw))
        .unwrap_or(ToolProtocol::OpenAiChat);
    for (path, raw) in configs {
        let content = upsert_hermes_sections_for_protocol(&raw, root_url, api_key, protocol)?;
        observe_text(&path, &content, &mut result)?;
    }
    let mut result = result.finish("hermes");
    attach_tool_protocol(&mut result, protocol);
    Ok(result)
}

#[cfg(test)]
pub(crate) fn apply_tool_config_by_name(
    tool: &str,
    base_url: &str,
    root_url: &str,
    api_key: &str,
) -> Result<ToolApplyResult> {
    let protocol = resolve_tool_protocol(tool, None)?;
    apply_tool_config_by_name_for_protocol(tool, base_url, root_url, api_key, protocol)
}

pub(crate) fn apply_tool_config_by_name_for_protocol(
    tool: &str,
    base_url: &str,
    root_url: &str,
    api_key: &str,
    protocol: ToolProtocol,
) -> Result<ToolApplyResult> {
    resolve_tool_protocol(tool, Some(protocol.as_str()))?;
    let mut result = match tool {
        "codex" => apply_codex_config(base_url, api_key),
        "claude" => apply_claude_config(root_url, api_key),
        "claude-desktop" => apply_claude_desktop_config(root_url, api_key),
        "claude-science" => apply_claude_science_config(root_url, api_key),
        "gemini" => apply_gemini_config(root_url, api_key),
        "opencode" => {
            apply_opencode_config_with_model_info_for_protocol(base_url, api_key, &[], protocol)
        }
        "openclaw" => apply_openclaw_config_for_protocol(root_url, api_key, protocol),
        "hermes" => apply_hermes_config_for_protocol(root_url, api_key, protocol),
        "vscode" => {
            apply_vscode_config_with_model_info_for_protocol(base_url, api_key, &[], protocol)
        }
        "workbuddy" => {
            apply_workbuddy_config_with_model_info_for_protocol(base_url, api_key, &[], protocol)
        }
        "copilot" | "raven" | "pi" | "cline" | "reasonix" | "deepseek-harness" | "open-interpreter"
        | "goose" | "mistral-vibe" | "open-design" | "kimicode" | "mimocode"
        | "qwencode" | "openscience" | "vibe-trading" | "zcode" | "anythingllm" => {
            apply_additional_tool_config_with_model_info_for_protocol(
                tool,
                base_url,
                api_key,
                &[],
                protocol,
            )
        }
        _ => Err(anyhow!("unknown tool: {tool}")),
    }?;
    attach_tool_protocol_if_missing(&mut result, protocol);
    Ok(result)
}

#[cfg(test)]
pub(crate) fn apply_tool_config_by_name_with_runtime_handling(
    tool: &str,
    base_url: &str,
    root_url: &str,
    api_key: &str,
) -> Result<ToolApplyResult> {
    let protocol = resolve_tool_protocol(tool, None)?;
    apply_tool_config_by_name_with_runtime_handling_for_protocol(
        tool, base_url, root_url, api_key, protocol,
    )
}

#[cfg(test)]
pub(crate) fn apply_tool_config_by_name_with_runtime_handling_for_protocol(
    tool: &str,
    base_url: &str,
    root_url: &str,
    api_key: &str,
    protocol: ToolProtocol,
) -> Result<ToolApplyResult> {
    let closed_before_config = close_running_tool_before_config(tool)?;
    let mut result =
        apply_tool_config_by_name_for_protocol(tool, base_url, root_url, api_key, protocol)?;
    attach_closed_before_config(&mut result, closed_before_config);
    Ok(result)
}

pub(crate) fn check_tool_config_by_name(
    tool: &str,
    base_url: &str,
    root_url: &str,
    api_key: &str,
) -> Result<ToolApplyResult> {
    let protocol = resolve_tool_protocol(tool, None)?;
    let mut result = match tool {
        "codex" => check_codex_config(base_url, api_key),
        "claude" => check_claude_config(root_url, api_key),
        "claude-desktop" => check_claude_desktop_config(root_url, api_key),
        "claude-science" => check_claude_science_config(root_url, api_key),
        "gemini" => check_gemini_config(root_url, api_key),
        "opencode" => check_opencode_config(base_url, api_key),
        "openclaw" => check_openclaw_config(root_url, api_key),
        "hermes" => check_hermes_config(root_url, api_key),
        "vscode" => check_vscode_config(base_url, api_key),
        "workbuddy" => check_workbuddy_config(base_url, api_key),
        "copilot" | "raven" | "pi" | "cline" | "reasonix" | "deepseek-harness" | "open-interpreter"
        | "goose" | "mistral-vibe" | "open-design" | "kimicode" | "mimocode"
        | "qwencode" | "openscience" | "vibe-trading" | "zcode" | "anythingllm" => {
            check_additional_tool_config(tool, base_url, api_key)
        }
        _ => Err(anyhow!("unknown tool: {tool}")),
    }?;
    attach_tool_protocol_if_missing(&mut result, protocol);
    Ok(result)
}

#[cfg(test)]
pub(crate) fn remove_tool_config_by_name(
    tool: &str,
    base_url: &str,
    root_url: &str,
    api_key: &str,
) -> Result<ToolApplyResult> {
    remove_tool_config_by_name_with_mode(
        tool,
        base_url,
        root_url,
        api_key,
        ToolConfigRemoveMode::RestorePreConst,
    )
}

pub(crate) fn remove_tool_config_by_name_with_mode(
    tool: &str,
    base_url: &str,
    root_url: &str,
    api_key: &str,
    remove_mode: ToolConfigRemoveMode,
) -> Result<ToolApplyResult> {
    remove_mode.validate_for_tool(tool)?;
    let mut result = match tool {
        "codex" => {
            let mut ignore_progress =
                |_: &str, _: Option<u64>, _: Option<u64>, _: Option<f64>| {};
            remove_codex_config_with_progress_and_mode(
                base_url,
                api_key,
                remove_mode,
                &mut ignore_progress,
            )
        }
        "claude" => remove_claude_config_with_mode(root_url, api_key, remove_mode),
        "claude-desktop" => {
            remove_claude_desktop_config_with_mode(root_url, api_key, remove_mode)
        }
        "claude-science" => remove_claude_science_config(),
        "gemini" => remove_gemini_config_with_mode(root_url, api_key, remove_mode),
        "opencode" => remove_opencode_config(base_url, api_key),
        "openclaw" => remove_openclaw_config(root_url, api_key),
        "hermes" => remove_hermes_config(root_url, api_key),
        "vscode" => remove_vscode_config(),
        "workbuddy" => remove_workbuddy_config(base_url, api_key),
        "copilot" | "raven" | "pi" | "cline" | "reasonix" | "deepseek-harness" | "open-interpreter"
        | "goose" | "mistral-vibe" | "open-design" | "kimicode" | "mimocode"
        | "qwencode" | "openscience" | "vibe-trading" | "zcode" | "anythingllm" => {
            remove_additional_tool_config(tool)
        }
        _ => Err(anyhow!("unknown tool: {tool}")),
    }?;
    attach_tool_config_remove_mode(&mut result, remove_mode, None);
    Ok(result)
}
