#[cfg(test)]
pub(crate) fn remove_codex_config(base_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    let mut ignore_progress = |_: &str, _: Option<u64>, _: Option<u64>, _: Option<f64>| {};
    remove_codex_config_with_progress_and_mode(
        base_url,
        api_key,
        ToolConfigRemoveMode::RestorePreConst,
        &mut ignore_progress,
    )
}

pub(crate) fn remove_codex_config_with_progress_and_mode(
    base_url: &str,
    api_key: &str,
    remove_mode: ToolConfigRemoveMode,
    report_progress: &mut dyn FnMut(&'static str, Option<u64>, Option<u64>, Option<f64>),
) -> Result<ToolApplyResult> {
    remove_mode.validate_for_tool("codex")?;
    let dir = codex_home();
    let transaction_paths = vec![dir.join("config.toml"), dir.join("auth.json")];
    let mut finished = with_tool_config_file_transaction("codex", &transaction_paths, || {
        let mut result = restore_tool_config_from_manifest("codex")?;
        cleanup_codex_provider_aliases(&mut result, base_url, api_key)?;
        if remove_mode == ToolConfigRemoveMode::NativeRoute {
            normalize_codex_native_route(&mut result, api_key)?;
        }
        Ok(result)
    })?;
    let config_path = codex_home().join("config.toml");
    let current_config = read_text_or_empty(&config_path)?;
    let target_provider = codex_effective_provider_from_config_text(&current_config);
    let sync = restore_codex_sessions_to_provider_with_progress(&target_provider, report_progress);
    attach_codex_session_sync_details(&mut finished, sync);
    if finished
        .details
        .get("manifest_fields_preserved")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or_default()
        > 0
    {
        finished
            .details
            .insert("provider_restore_skipped".to_string(), target_provider);
    }
    attach_tool_config_remove_mode(
        &mut finished,
        remove_mode,
        Some("openai_builtin_provider"),
    );
    Ok(finished)
}

#[cfg(test)]
pub(crate) fn remove_codex_config_with_runtime_handling(
    base_url: &str,
    api_key: &str,
) -> Result<ToolApplyResult> {
    let closed_before_config = close_running_tool_before_config("codex")?;
    let mut result = remove_codex_config(base_url, api_key)?;
    attach_closed_before_config(&mut result, closed_before_config);
    Ok(result)
}

#[cfg(test)]
pub(crate) fn remove_claude_config(root_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    remove_claude_config_with_mode(
        root_url,
        api_key,
        ToolConfigRemoveMode::RestorePreConst,
    )
}

pub(crate) fn remove_claude_config_with_mode(
    _root_url: &str,
    _api_key: &str,
    remove_mode: ToolConfigRemoveMode,
) -> Result<ToolApplyResult> {
    remove_mode.validate_for_tool("claude")?;
    let settings_path = home_dir().join(".claude").join("settings.json");
    let transaction_paths = vec![settings_path.clone(), home_dir().join(".claude.json")];
    let mut result = with_tool_config_file_transaction("claude", &transaction_paths, || {
        let mut result = restore_tool_config_from_manifest("claude")?;
        if remove_mode == ToolConfigRemoveMode::NativeRoute {
            normalize_claude_code_native_route(&mut result, &settings_path)?;
        }
        Ok(result)
    })?;
    attach_tool_config_remove_mode(
        &mut result,
        remove_mode,
        Some("claude_first_party_login"),
    );
    Ok(result)
}

#[cfg(test)]
pub(crate) fn remove_claude_desktop_config(
    root_url: &str,
    api_key: &str,
) -> Result<ToolApplyResult> {
    remove_claude_desktop_config_with_mode(
        root_url,
        api_key,
        ToolConfigRemoveMode::RestorePreConst,
    )
}

pub(crate) fn remove_claude_desktop_config_with_mode(
    root_url: &str,
    api_key: &str,
    remove_mode: ToolConfigRemoveMode,
) -> Result<ToolApplyResult> {
    remove_mode.validate_for_tool("claude-desktop")?;
    let root_url = tool_surface_url(root_url, "anthropic");
    let transaction_paths = claude_desktop_transaction_paths();
    with_tool_config_file_transaction("claude-desktop", &transaction_paths, || {
        let paths = claude_desktop_paths();
        let current_meta = read_json_or_default(&paths.meta_path, serde_json::json!({}))?;
        let legacy_marker_was_present =
            claude_desktop_meta_has_legacy_marker(&current_meta);
        let mut result = restore_tool_config_from_manifest("claude-desktop")?;
        cleanup_legacy_claude_desktop_config(
            &mut result,
            &root_url,
            api_key,
            legacy_marker_was_present,
        )?;
        if remove_mode == ToolConfigRemoveMode::NativeRoute {
            normalize_claude_desktop_native_route(&mut result, &root_url, api_key)?;
        }
        result.details.insert(
            "claude_desktop_restore_kind".to_string(),
            if remove_mode == ToolConfigRemoveMode::NativeRoute {
                "native_first_party"
            } else if legacy_marker_was_present {
                "legacy_cleanup_to_official_baseline"
            } else {
                "managed_original"
            }
            .to_string(),
        );
        attach_tool_config_remove_mode(
            &mut result,
            remove_mode,
            Some("claude_desktop_first_party"),
        );
        Ok(result)
    })
}

#[cfg(test)]
pub(crate) fn remove_gemini_config(root_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    remove_gemini_config_with_mode(
        root_url,
        api_key,
        ToolConfigRemoveMode::RestorePreConst,
    )
}

pub(crate) fn remove_gemini_config_with_mode(
    _root_url: &str,
    _api_key: &str,
    remove_mode: ToolConfigRemoveMode,
) -> Result<ToolApplyResult> {
    remove_mode.validate_for_tool("gemini")?;
    let gemini_dir = home_dir().join(".gemini");
    let env_path = gemini_dir.join(".env");
    let settings_path = gemini_dir.join("settings.json");
    let transaction_paths = vec![env_path.clone(), settings_path.clone()];
    let mut result = with_tool_config_file_transaction("gemini", &transaction_paths, || {
        let mut result = restore_tool_config_from_manifest("gemini")?;
        if remove_mode == ToolConfigRemoveMode::NativeRoute {
            normalize_gemini_native_route(&mut result, &env_path, &settings_path)?;
        }
        Ok(result)
    })?;
    attach_tool_config_remove_mode(
        &mut result,
        remove_mode,
        Some("google_personal_oauth"),
    );
    Ok(result)
}

fn attach_tool_config_remove_mode(
    result: &mut ToolApplyResult,
    remove_mode: ToolConfigRemoveMode,
    native_route_kind: Option<&str>,
) {
    result.details.insert(
        "remove_mode".to_string(),
        remove_mode.as_str().to_string(),
    );
    if remove_mode == ToolConfigRemoveMode::NativeRoute {
        result.details.insert(
            "native_route_applied".to_string(),
            "true".to_string(),
        );
        if let Some(kind) = native_route_kind {
            result
                .details
                .insert("native_route_kind".to_string(), kind.to_string());
        }
    }
}

fn normalize_codex_native_route(result: &mut ToolApplyResult, api_key: &str) -> Result<()> {
    let dir = codex_home();
    let config_path = dir.join("config.toml");
    if config_path.exists() {
        let before = fs::read(&config_path)?;
        let mut doc = String::from_utf8_lossy(&before)
            .parse::<DocumentMut>()
            .with_context(|| format!("parse {} for native Codex route", config_path.display()))?;
        if doc.as_table_mut().remove("model_provider").is_some() {
            record_post_restore_cleanup(
                result,
                &config_path,
                before,
                Some(doc.to_string().into_bytes()),
                "codex",
            )?;
        }
    }

    let auth_path = dir.join("auth.json");
    if auth_path.exists() {
        let before = fs::read(&auth_path)?;
        let mut auth = serde_json::from_slice::<serde_json::Value>(&before)
            .with_context(|| format!("parse {} for native Codex auth", auth_path.display()))?;
        let has_official_login = codex_account_status_from_auth(&auth) == "official";
        let object = auth
            .as_object_mut()
            .ok_or_else(|| anyhow!("{} must contain a JSON object", auth_path.display()))?;
        let remove_const_key = object
            .get("OPENAI_API_KEY")
            .and_then(serde_json::Value::as_str)
            .map(|value| {
                has_official_login
                    || value == LOCAL_PLACEHOLDER_KEY
                    || (!api_key.trim().is_empty() && value == api_key.trim())
            })
            .unwrap_or(false);
        if remove_const_key {
            object.remove("OPENAI_API_KEY");
            let after = if object.is_empty() {
                None
            } else {
                let mut bytes = serde_json::to_vec_pretty(&auth)?;
                bytes.push(b'\n');
                Some(bytes)
            };
            record_post_restore_cleanup(result, &auth_path, before, after, "codex")?;
        }
    }
    Ok(())
}

fn normalize_claude_code_native_route(
    result: &mut ToolApplyResult,
    settings_path: &Path,
) -> Result<()> {
    if !settings_path.exists() {
        return Ok(());
    }
    let before = fs::read(settings_path)?;
    let mut settings = serde_json::from_slice::<serde_json::Value>(&before)
        .with_context(|| format!("parse {} for native Claude Code route", settings_path.display()))?;
    let object = settings
        .as_object_mut()
        .ok_or_else(|| anyhow!("{} must contain a JSON object", settings_path.display()))?;
    let mut changed = false;
    if let Some(env) = object
        .get_mut("env")
        .and_then(serde_json::Value::as_object_mut)
    {
        let before_len = env.len();
        remove_json_environment_names(env, CLAUDE_CODE_EXTERNAL_ENVIRONMENT_NAMES);
        changed |= env.len() != before_len;
    }
    for setting in CLAUDE_CODE_NATIVE_ROUTE_SETTING_NAMES {
        changed |= object.remove(*setting).is_some();
    }
    if !changed {
        return Ok(());
    }
    let mut after = serde_json::to_vec_pretty(&settings)?;
    after.push(b'\n');
    record_post_restore_cleanup(result, settings_path, before, Some(after), "claude")
}

fn normalize_claude_desktop_native_route(
    result: &mut ToolApplyResult,
    root_url: &str,
    api_key: &str,
) -> Result<()> {
    let paths = claude_desktop_paths();
    for path in [&paths.normal_config_path, &paths.threep_config_path] {
        let mut config = read_json_or_default(path, serde_json::json!({}))?;
        ensure_json_object(&mut config);
        if config
            .get("deploymentMode")
            .and_then(serde_json::Value::as_str)
            == Some("1p")
        {
            continue;
        }
        config["deploymentMode"] = serde_json::json!("1p");
        let mut after = serde_json::to_vec_pretty(&config)?;
        after.push(b'\n');
        record_native_route_update(result, path, Some(after), "claude-desktop")?;
    }

    if paths.profile_path.exists() {
        let profile = read_json_or_default(&paths.profile_path, serde_json::json!({}))?;
        if claude_desktop_profile_matches_const_api(&profile, root_url, api_key) {
            record_native_route_update(result, &paths.profile_path, None, "claude-desktop")?;
        }
    }

    let legacy_profile_was_const = if paths.legacy_profile_path.exists() {
        let legacy_profile =
            read_json_or_default(&paths.legacy_profile_path, serde_json::json!({}))?;
        claude_desktop_profile_matches_const_api(&legacy_profile, root_url, api_key)
    } else {
        false
    };
    let mut has_cc_switch_entry = false;
    if paths.meta_path.exists() {
        let before = fs::read(&paths.meta_path)?;
        let mut meta = serde_json::from_slice::<serde_json::Value>(&before)
            .with_context(|| format!("parse {} for native Claude Desktop route", paths.meta_path.display()))?;
        has_cc_switch_entry = claude_desktop_meta_has_cc_switch_entry(&meta);
        let object = meta
            .as_object_mut()
            .ok_or_else(|| anyhow!("{} must contain a JSON object", paths.meta_path.display()))?;
        let mut changed = false;
        for field in ["appliedId", "appliedProfileId"] {
            let selected_id = object.get(field).and_then(serde_json::Value::as_str);
            if selected_id == Some(CLAUDE_DESKTOP_PROFILE_ID)
                || (selected_id == Some(CLAUDE_DESKTOP_LEGACY_PROFILE_ID)
                    && !has_cc_switch_entry
                    && legacy_profile_was_const)
            {
                object.remove(field);
                changed = true;
            }
        }
        if let Some(entries) = object
            .get_mut("entries")
            .and_then(serde_json::Value::as_array_mut)
        {
            let previous_len = entries.len();
            entries.retain(|entry| {
                let id = entry.get("id").and_then(serde_json::Value::as_str);
                let name = entry.get("name").and_then(serde_json::Value::as_str);
                id != Some(CLAUDE_DESKTOP_PROFILE_ID)
                    && !(id == Some(CLAUDE_DESKTOP_LEGACY_PROFILE_ID)
                        && name == Some(CLAUDE_DESKTOP_PROFILE_NAME)
                        && !has_cc_switch_entry)
            });
            changed |= entries.len() != previous_len;
            if entries.is_empty() {
                object.remove("entries");
            }
        }
        if changed {
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
    }
    if legacy_profile_was_const && !has_cc_switch_entry {
        record_native_route_update(
            result,
            &paths.legacy_profile_path,
            None,
            "claude-desktop",
        )?;
    }
    Ok(())
}

fn normalize_gemini_native_route(
    result: &mut ToolApplyResult,
    env_path: &Path,
    settings_path: &Path,
) -> Result<()> {
    if env_path.exists() {
        let before = fs::read(env_path)?;
        let current = String::from_utf8_lossy(&before);
        let cleaned = remove_env_vars_if(&current, |name, _| {
            GEMINI_CLI_EXTERNAL_ENVIRONMENT_NAMES.contains(&name)
        });
        let after = if cleaned.trim().is_empty() {
            None
        } else {
            Some(cleaned.into_bytes())
        };
        record_post_restore_cleanup(result, env_path, before, after, "gemini")?;
    }

    let mut settings = read_json_or_default(settings_path, serde_json::json!({}))?;
    ensure_json_object(&mut settings);
    if settings
        .pointer("/security/auth/selectedType")
        .and_then(serde_json::Value::as_str)
        == Some("oauth-personal")
    {
        return Ok(());
    }
    if !settings
        .get("security")
        .is_some_and(serde_json::Value::is_object)
    {
        settings["security"] = serde_json::json!({});
    }
    if !settings["security"]
        .get("auth")
        .is_some_and(serde_json::Value::is_object)
    {
        settings["security"]["auth"] = serde_json::json!({});
    }
    settings["security"]["auth"]["selectedType"] = serde_json::json!("oauth-personal");
    let mut after = serde_json::to_vec_pretty(&settings)?;
    after.push(b'\n');
    record_native_route_update(result, settings_path, Some(after), "gemini")
}

pub(crate) fn remove_opencode_config(base_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    let mut result = restore_tool_config_from_manifest("opencode")?;
    cleanup_opencode_provider_aliases(&mut result, base_url, api_key)?;
    Ok(result)
}

pub(crate) fn remove_openclaw_config(root_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    let mut result = restore_tool_config_from_manifest("openclaw")?;
    cleanup_openclaw_provider_aliases(&mut result, root_url, api_key)?;
    Ok(result)
}

pub(crate) fn remove_hermes_config(root_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    let mut result = restore_tool_config_from_manifest("hermes")?;
    cleanup_hermes_provider_aliases(&mut result, root_url, api_key)?;
    Ok(result)
}

fn tool_program_locations_path() -> PathBuf {
    crate::client_data_root().join("tool-programs.json")
}

fn load_tool_program_locations() -> Result<HashMap<String, String>> {
    let path = tool_program_locations_path();
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))
}

fn save_tool_program_locations(locations: &HashMap<String, String>) -> Result<()> {
    let path = tool_program_locations_path();
    let content = serde_json::to_vec_pretty(locations)?;
    atomic_write(&path, &[content.as_slice(), b"\n"].concat())
}

pub(crate) fn save_tool_program_path(
    tool: &str,
    path: String,
) -> Result<ToolProgramLocationResult> {
    if tool == "codex" && !is_supported_codex_desktop_path(Path::new(&path)) {
        return Err(anyhow!(
            "Select official ChatGPT or legacy Codex Desktop; Codex CLI and Codex++ are not desktop launch targets."
        ));
    }
    if !is_tool_owned_program_path(tool, Path::new(&path)) {
        return Err(anyhow!(crate::native_i18n::text(
            "所选程序不在此工具的可执行文件白名单中，请选择工具自身的 EXE、App 或命令入口。",
            "The selected program is not in this tool's executable allowlist. Choose the tool's own EXE, app, or command entry point.",
        )));
    }
    let mut locations = load_tool_program_locations()?;
    locations.insert(tool.to_string(), path.clone());
    save_tool_program_locations(&locations)?;
    // Saving an explicit user choice must be immediate. A full discovery can
    // launch a login shell on macOS and belongs only to the explicit re-search
    // action, not to confirmation of a path the user has already selected.
    let mut candidates = Vec::new();
    push_candidate(
        &mut candidates,
        tool,
        path.clone(),
        "Selected",
        selected_path_kind(tool, &path),
        true,
    );
    for candidate in &mut candidates {
        annotate_tool_program_candidate(tool, candidate);
    }
    Ok(ToolProgramLocationResult {
        tool: tool.to_string(),
        selected_path: path,
        candidates,
    })
}

pub(crate) fn choose_tool_program_path(tool: &str) -> Result<ToolProgramLocationResult> {
    let picked = if cfg!(target_os = "macos") {
        rfd::FileDialog::new()
            .set_title(crate::native_i18n::text(
                "选择程序或 App",
                "Choose a program or app",
            ))
            .pick_file()
    } else {
        rfd::FileDialog::new()
            .set_title(crate::native_i18n::text(
                "选择可执行文件",
                "Choose an executable",
            ))
            .pick_file()
    };
    let path = picked
        .ok_or_else(|| {
            anyhow!(crate::native_i18n::text(
                "未选择程序",
                "No program was selected",
            ))
        })?
        .display()
        .to_string();
    save_tool_program_path(tool, path)
}

pub(crate) fn locate_tool_program(tool: &str) -> Result<ToolProgramLocationResult> {
    let mut locations = load_tool_program_locations()?;
    let mut selected_path = locations.get(tool).cloned().unwrap_or_default();
    let known_paths = known_tool_program_paths(tool);
    if let Some(refreshed) =
        refreshed_saved_windows_appx_path(tool, &selected_path, &known_paths)
    {
        selected_path = refreshed;
        locations.insert(tool.to_string(), selected_path.clone());
        save_tool_program_locations(&locations)?;
    }
    let mut candidates = Vec::new();
    if !selected_path.is_empty() {
        push_candidate(
            &mut candidates,
            tool,
            selected_path.clone(),
            crate::native_i18n::text("已选择", "Selected"),
            selected_path_kind(tool, &selected_path),
            true,
        );
    }
    for path in known_paths {
        push_candidate(
            &mut candidates,
            tool,
            path.display().to_string(),
            crate::native_i18n::text("自动发现", "Auto detected"),
            tool_path_candidate_kind(tool, &path.display().to_string()),
            false,
        );
    }
    let running_paths = locator_running_tool_paths(
        tool,
        &candidates,
        running_tool_executable_paths(tool),
    )?;
    for path in running_paths {
        push_candidate(
            &mut candidates,
            tool,
            path.display().to_string(),
            crate::native_i18n::text("正在运行", "Running"),
            tool_path_candidate_kind(tool, &path.display().to_string()),
            false,
        );
    }
    for command in tool_program_commands(tool) {
        for path in find_tool_command_paths(tool, command) {
            push_candidate(
                &mut candidates,
                tool,
                path.display().to_string(),
                command,
                "command".to_string(),
                false,
            );
        }
    }
    refresh_saved_tool_program_selection(
        tool,
        &mut locations,
        &mut selected_path,
        &mut candidates,
    )?;
    for candidate in &mut candidates {
        annotate_tool_program_candidate(tool, candidate);
    }
    Ok(ToolProgramLocationResult {
        tool: tool.to_string(),
        selected_path,
        candidates,
    })
}

fn locator_running_tool_paths(
    tool: &str,
    candidates: &[ToolProgramCandidate],
    running_paths: Result<Vec<PathBuf>>,
) -> Result<Vec<PathBuf>> {
    match running_paths {
        Ok(paths) => Ok(paths),
        Err(error)
            if candidates
                .iter()
                .any(|candidate| is_launchable_tool_candidate(tool, candidate)) =>
        {
            eprintln!(
                "[const-api][tool-config] running process discovery skipped tool={tool} error={error:#}"
            );
            Ok(Vec::new())
        }
        Err(error) => Err(error),
    }
}

// Status badges only need installation/launch availability. Deliberately skip
// the full running-process snapshot used by the interactive locator: repeating
// that scan for every tool would make the home screen slower and less reliable.
pub(crate) fn tool_program_located_without_process_scan(tool: &str) -> Result<bool> {
    Ok(tool_launch_candidate_without_process_scan(tool)?.is_some())
}

fn tool_launch_candidate_without_process_scan(tool: &str) -> Result<Option<ToolProgramCandidate>> {
    let candidates = tool_program_candidates_without_process_scan(tool)?;
    Ok(select_tool_launch_candidate(tool, &candidates).cloned())
}

fn tool_program_candidates_without_process_scan(tool: &str) -> Result<Vec<ToolProgramCandidate>> {
    let mut locations = load_tool_program_locations()?;
    let mut selected_path = locations.get(tool).cloned().unwrap_or_default();
    let known_paths = known_tool_program_paths(tool);
    if let Some(refreshed) =
        refreshed_saved_windows_appx_path(tool, &selected_path, &known_paths)
    {
        selected_path = refreshed;
        locations.insert(tool.to_string(), selected_path.clone());
        save_tool_program_locations(&locations)?;
    }
    let mut candidates = Vec::new();
    if !selected_path.is_empty() {
        push_candidate(
            &mut candidates,
            tool,
            selected_path.clone(),
            "Selected",
            selected_path_kind(tool, &selected_path),
            true,
        );
    }
    for path in known_paths {
        push_candidate(
            &mut candidates,
            tool,
            path.display().to_string(),
            "Auto-detected",
            tool_path_candidate_kind(tool, &path.display().to_string()),
            false,
        );
    }
    for command in tool_program_commands(tool) {
        for path in find_tool_command_paths_without_shell(tool, command) {
            push_candidate(
                &mut candidates,
                tool,
                path.display().to_string(),
                command,
                "command".to_string(),
                false,
            );
        }
    }
    for candidate in &mut candidates {
        annotate_tool_program_candidate(tool, candidate);
    }
    Ok(candidates)
}

fn tool_program_selection_required(tool: &str, candidates: &[ToolProgramCandidate]) -> bool {
    if !tool_prefers_desktop_program(tool)
        || candidates.iter().any(|c| c.selected && is_launchable_tool_candidate(tool, c))
    {
        return false;
    }
    // Different editions sharing one adapter need a user decision. Old install
    // versions of the same executable still use the existing version ranking.
    candidates
        .iter()
        .filter(|c| is_launchable_tool_candidate(tool, c) && is_desktop_program_candidate(c))
        .map(|c| normalized_tool_program_identity(&c.path))
        .collect::<HashSet<_>>()
        .len() > 1
}

fn require_unambiguous_tool_program(tool: &str, candidates: &[ToolProgramCandidate]) -> Result<()> {
    if tool_program_selection_required(tool, candidates) {
        return Err(anyhow!(
            "TOOL_PROGRAM_SELECTION_REQUIRED: multiple desktop editions are installed for {tool}"
        ));
    }
    Ok(())
}

const DEEPSEEK_HARNESS_WEB_URL: &str = "http://127.0.0.1:3080";
const DEEPSEEK_HARNESS_WEB_ADDRESS: &str = "127.0.0.1:3080";
const DEEPSEEK_HARNESS_WEB_START_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeepSeekHarnessWebProbe {
    NotRunning,
    Ready,
    ReadyRequiresBrowserAuth,
    PortOccupied,
}

fn deepseek_harness_http_response_probe(response: &str) -> DeepSeekHarnessWebProbe {
    if (response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200"))
        && response.contains("<title>DeepSeek Harness</title>")
        && response.contains("manifest.webmanifest")
        && response.contains("__DSH_BOOT__")
    {
        return DeepSeekHarnessWebProbe::Ready;
    }
    if (response.starts_with("HTTP/1.1 401") || response.starts_with("HTTP/1.0 401"))
        && response.contains(
            "dsh web authentication required; reopen the URL printed by dsh web.",
        )
    {
        return DeepSeekHarnessWebProbe::ReadyRequiresBrowserAuth;
    }
    DeepSeekHarnessWebProbe::PortOccupied
}

fn deepseek_harness_web_probe() -> Result<DeepSeekHarnessWebProbe> {
    let address = DEEPSEEK_HARNESS_WEB_ADDRESS
        .parse()
        .context("DeepSeek Harness loopback address is invalid")?;
    let mut stream = match std::net::TcpStream::connect_timeout(
        &address,
        Duration::from_millis(300),
    ) {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::NotConnected
                    | std::io::ErrorKind::AddrNotAvailable
            ) =>
        {
            return Ok(DeepSeekHarnessWebProbe::NotRunning)
        }
        Err(error) => {
            return Err(error).context("probe DeepSeek Harness Web UI loopback port")
        }
    };
    stream.set_read_timeout(Some(Duration::from_millis(750)))?;
    stream.set_write_timeout(Some(Duration::from_millis(750)))?;
    if stream
        .write_all(
            b"GET / HTTP/1.0\r\nHost: 127.0.0.1:3080\r\nConnection: close\r\n\r\n",
        )
        .is_err()
    {
        return Ok(DeepSeekHarnessWebProbe::PortOccupied);
    }
    let mut response = Vec::new();
    let read_result = stream.take(256 * 1024).read_to_end(&mut response);
    if let Err(error) = read_result {
        if !matches!(
            error.kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
        ) {
            return Err(error).context("read DeepSeek Harness Web UI identity response");
        }
    }
    let response = String::from_utf8_lossy(&response);
    Ok(deepseek_harness_http_response_probe(&response))
}

fn wait_for_deepseek_harness_web_start() -> Result<DeepSeekHarnessWebProbe> {
    let deadline = Instant::now() + DEEPSEEK_HARNESS_WEB_START_TIMEOUT;
    let mut port_was_occupied = false;
    loop {
        match deepseek_harness_web_probe()? {
            ready @ (DeepSeekHarnessWebProbe::Ready
            | DeepSeekHarnessWebProbe::ReadyRequiresBrowserAuth) => return Ok(ready),
            DeepSeekHarnessWebProbe::PortOccupied => port_was_occupied = true,
            DeepSeekHarnessWebProbe::NotRunning => {}
        }
        if Instant::now() >= deadline {
            return Err(if port_was_occupied {
                anyhow!(
                    "TOOL_START_PORT_CONFLICT: port 3080 answered but did not identify as DeepSeek Harness"
                )
            } else {
                anyhow!(
                    "TOOL_START_TIMEOUT: DeepSeek Harness Web UI was not ready at {DEEPSEEK_HARNESS_WEB_URL} within {} seconds",
                    DEEPSEEK_HARNESS_WEB_START_TIMEOUT.as_secs()
                )
            });
        }
        thread::sleep(TOOL_PROCESS_POLL_INTERVAL);
    }
}

fn open_deepseek_harness_web_url(
    launch_context: Option<&ToolConfigLaunchContext<'_>>,
) -> Result<()> {
    let (program, args) = crate::external_url_open_command(DEEPSEEK_HARNESS_WEB_URL)
        .map_err(|error| anyhow!("open DeepSeek Harness Web UI: {error}"))?;
    let mut command = Command::new(program);
    command.args(args);
    spawn_launch_command(&mut command, launch_context, "DeepSeek Harness Web UI")?;
    Ok(())
}

pub(crate) fn launch_tool_program(tool: &str) -> Result<(String, PathBuf)> {
    launch_tool_program_inner(tool, None)
}

fn launch_tool_program_inner(
    tool: &str,
    launch_context: Option<&ToolConfigLaunchContext<'_>>,
) -> Result<(String, PathBuf)> {
    #[cfg(test)]
    if test_tool_runtime_snapshot(tool).is_some() {
        let launched = match launch_context {
            Some(context) => context.spawn("test tool process", || {
                test_tool_runtime_launch(tool).expect("test runtime checked above")
            })?,
            None => test_tool_runtime_launch(tool).expect("test runtime checked above")?,
        };
        wait_for_tool_program_start(tool)?;
        return Ok(launched);
    }

    let location = locate_tool_program(tool)?;
    require_unambiguous_tool_program(tool, &location.candidates)?;
    let candidate = select_tool_launch_candidate(tool, &location.candidates);
    // The desktop shell owns a separate backend and does not use the CLI's
    // fixed browser port. Resolve the user's program choice before probing it.
    let harness_web = tool == "deepseek-harness"
        && candidate.is_none_or(|candidate| !is_desktop_program_candidate(candidate));
    if harness_web {
        match deepseek_harness_web_probe()? {
            DeepSeekHarnessWebProbe::Ready | DeepSeekHarnessWebProbe::ReadyRequiresBrowserAuth => {
                open_deepseek_harness_web_url(launch_context)?;
                return Ok((
                    "Open DeepSeek Harness".to_string(),
                    PathBuf::from(DEEPSEEK_HARNESS_WEB_URL),
                ));
            }
            DeepSeekHarnessWebProbe::PortOccupied => {
                return Err(anyhow!(
                    "TOOL_START_PORT_CONFLICT: port 3080 is already used by a service other than DeepSeek Harness"
                ))
            }
            DeepSeekHarnessWebProbe::NotRunning => {}
        }
    }

    let Some(candidate) = candidate else {
        return Err(anyhow!(
            "TOOL_START_CANDIDATE_MISSING: no launchable {} found",
            tool_runtime_display_name(tool)
        ));
    };
    let path = PathBuf::from(&candidate.path);
    let launch_args = tool_candidate_launch_args(tool, candidate);
    let confirm_running =
        matches!(
            candidate.kind.as_str(),
            "mac_app" | "windows_exe" | "windows_appx" | "linux_desktop"
        ) || (cfg!(target_os = "macos") && candidate.kind != "command" && !path.is_file());
    let launched: Result<(String, PathBuf)> = match candidate.kind.as_str() {
        "mac_app" => {
            launch_macos_app(&path, launch_context)?;
            Ok(("Configure and restart".to_string(), path))
        }
        "windows_exe" => {
            let mut command = Command::new(&path);
            command.args(launch_args);
            spawn_launch_command(&mut command, launch_context, &path.display().to_string())?;
            Ok(("Configure and restart".to_string(), path))
        }
        "windows_appx" => {
            launch_windows_appx_executable(&path, launch_context)?;
            Ok(("Configure and restart".to_string(), path))
        }
        "command" => {
            launch_command_tool_with_env_and_args(
                tool,
                &path,
                launch_args,
                &[],
                &[],
                launch_context,
            )?;
            Ok(("Configure and restart".to_string(), path))
        }
        _ => {
            if cfg!(target_os = "macos") {
                if path.is_file() {
                    launch_command_tool_with_env_and_args(
                        tool,
                        &path,
                        launch_args,
                        &[],
                        &[],
                        launch_context,
                    )?;
                    Ok(("Configure and restart".to_string(), path))
                } else {
                    launch_macos_app(&path, launch_context)?;
                    Ok(("Configure and restart".to_string(), path))
                }
            } else {
                let mut command = Command::new(&path);
                command.args(launch_args);
                spawn_launch_command(&mut command, launch_context, &path.display().to_string())?;
                Ok(("Configure and restart".to_string(), path))
            }
        }
    };
    let launched = launched?;
    if harness_web {
        // Newer dsh versions open their one-time authenticated URL themselves.
        // Opening the bare URL here would show a 401 page instead.
        if wait_for_deepseek_harness_web_start()? == DeepSeekHarnessWebProbe::Ready {
            open_deepseek_harness_web_url(None)?;
        }
        return Ok(("dsh web".to_string(), launched.1));
    }
    if confirm_running {
        wait_for_tool_program_start(tool)?;
    }
    Ok(launched)
}

#[cfg(not(target_os = "windows"))]
fn spawn_launch_command(
    command: &mut Command,
    launch_context: Option<&ToolConfigLaunchContext<'_>>,
    purpose: &str,
) -> Result<Child> {
    match launch_context {
        Some(context) => context.spawn_command(command, purpose),
        None => command.spawn().with_context(|| format!("launch {purpose}")),
    }
}

#[cfg(target_os = "windows")]
fn spawn_launch_command(
    command: &mut Command,
    launch_context: Option<&ToolConfigLaunchContext<'_>>,
    purpose: &str,
) -> Result<Child> {
    let spawn = || spawn_windows_detached_launch_proxy(command, purpose);
    match launch_context {
        Some(context) => context.spawn(purpose, spawn),
        None => spawn(),
    }
}

#[cfg(target_os = "windows")]
fn spawn_windows_detached_launch_proxy(command: &Command, purpose: &str) -> Result<Child> {
    use std::os::windows::process::CommandExt;

    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

    let program = command.get_program().to_string_lossy().into_owned();
    let argument_line = command
        .get_args()
        .map(windows_quote_process_argument)
        .collect::<Vec<_>>()
        .join(" ");
    let working_directory = command
        .get_current_dir()
        .map(|path| path.to_string_lossy().into_owned());
    let environments = command
        .get_envs()
        .map(|(key, value)| (key.to_os_string(), value.map(|value| value.to_os_string())))
        .collect::<Vec<_>>();

    let mut script = format!(
        "$ErrorActionPreference = 'Stop'; Start-Process -FilePath {}",
        ps_single_quote(&program)
    );
    if !argument_line.is_empty() {
        script.push_str(" -ArgumentList ");
        script.push_str(&ps_single_quote(&argument_line));
    }
    if let Some(directory) = working_directory {
        script.push_str(" -WorkingDirectory ");
        script.push_str(&ps_single_quote(&directory));
    }

    let build_helper = |break_away: bool| {
        let mut helper = Command::new("powershell.exe");
        helper
            .args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(&script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(
                windows_hidden_console_flag()
                    | CREATE_NEW_PROCESS_GROUP
                    | if break_away { CREATE_BREAKAWAY_FROM_JOB } else { 0 },
            );
        for (key, value) in &environments {
            match value {
                Some(value) => {
                    helper.env(key, value);
                }
                None => {
                    helper.env_remove(key);
                }
            }
        }
        helper
    };

    let mut child = match build_helper(true).spawn() {
        Ok(child) => child,
        Err(breakaway_error) => build_helper(false).spawn().with_context(|| {
            format!(
                "launch detached proxy for {purpose}; breakaway attempt failed: {breakaway_error}"
            )
        })?,
    };
    let deadline = Instant::now() + TOOL_HELPER_COMMAND_TIMEOUT;
    loop {
        if let Some(status) = child
            .try_wait()
            .with_context(|| format!("wait for detached launch proxy for {purpose}"))?
        {
            if status.success() {
                return Ok(child);
            }
            return Err(anyhow!(
                "TOOL_START_PROXY_FAILED: detached launch proxy for {purpose} exited with {status}"
            ));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return Err(anyhow!(
                "TOOL_START_PROXY_TIMEOUT: detached launch proxy for {purpose} did not exit"
            ));
        }
        thread::sleep(TOOL_PROCESS_POLL_INTERVAL);
    }
}

#[cfg(target_os = "windows")]
fn windows_quote_process_argument(value: &OsStr) -> String {
    let value = value.to_string_lossy();
    if !value.is_empty()
        && !value
            .chars()
            .any(|character| character.is_whitespace() || character == '"')
    {
        return value.into_owned();
    }

    let mut quoted = String::from("\"");
    let mut backslashes = 0usize;
    for character in value.chars() {
        if character == '\\' {
            backslashes += 1;
            continue;
        }
        if character == '"' {
            quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
            quoted.push('"');
        } else {
            quoted.push_str(&"\\".repeat(backslashes));
            quoted.push(character);
        }
        backslashes = 0;
    }
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    quoted
}

#[cfg(test)]
fn close_tool_program_instances(tool: &str) -> Result<bool> {
    #[cfg(test)]
    if let Some(closed) = test_tool_runtime_close(tool) {
        return Ok(closed);
    }

    let location = locate_tool_program(tool)?;
    let Some(candidate) = select_tool_launch_candidate(tool, &location.candidates) else {
        close_known_tool_processes(tool)?;
        return Ok(false);
    };
    let path = PathBuf::from(&candidate.path);
    close_tool_candidate_instances(tool, candidate, &path)?;
    Ok(true)
}

#[cfg(test)]
fn close_tool_candidate_instances(
    tool: &str,
    candidate: &ToolProgramCandidate,
    path: &Path,
) -> Result<()> {
    match candidate.kind.as_str() {
        "mac_app" => close_macos_app_instances(path)?,
        "windows_exe" | "windows_appx" => close_windows_executable(path)?,
        "command" => close_command_candidate_instances(tool, candidate, path)?,
        _ => {
            if cfg!(target_os = "macos") {
                close_macos_app_instances(path)?;
            } else if cfg!(target_os = "windows") {
                close_windows_executable(path)?;
            } else {
                close_command_instances(&tool_command_name(candidate, path), path)?;
            }
        }
    }
    close_known_tool_processes(tool)
}

pub(crate) fn tool_program_running(tool: &str) -> Result<bool> {
    #[cfg(test)]
    if test_home_active() {
        return Ok(tool_process_snapshot(tool)?.running());
    }
    if tool == "deepseek-harness" {
        if tool_process_snapshot(tool)?.running() {
            return Ok(true);
        }
        return Ok(matches!(
            deepseek_harness_web_probe()?,
            DeepSeekHarnessWebProbe::Ready | DeepSeekHarnessWebProbe::ReadyRequiresBrowserAuth
        ));
    }
    Ok(tool_process_snapshot(tool)?.running())
}

fn tool_process_snapshot(tool: &str) -> Result<ToolProcessSnapshot> {
    #[cfg(test)]
    if let Some(snapshot) = test_tool_process_snapshot(tool) {
        if let Some(state) = test_tool_runtime_snapshot(tool) {
            if state.probe_failure {
                return Err(anyhow!(
                    "TOOL_CONFIG_PROCESS_PROBE_FAILED: injected process identity failure for {tool}"
                ));
            }
            let delay = state.state_check_delay;
            if !delay.is_zero() {
                thread::sleep(delay);
            }
        }
        return Ok(snapshot);
    }

    #[cfg(test)]
    if let Some(match_names) = test_process_match_names(tool) {
        return Ok(ToolProcessSnapshot::new(platform_process_snapshot(
            &match_names,
        )?));
    }

    if test_home_active() {
        return Ok(ToolProcessSnapshot::default());
    }
    let match_names = tool_process_match_names(tool)?;
    #[cfg(target_os = "windows")]
    {
        let scope = windows_tool_process_scope(tool, &match_names)?;
        return windows_tool_process_snapshot_for_scope(tool, &scope);
    }
    #[cfg(not(target_os = "windows"))]
    {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        let processes = platform_process_snapshot_with_scope(&unix_tool_process_scope(
            tool,
            &match_names,
        )?)?;
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let processes = platform_process_snapshot(&match_names)?;
        Ok(filter_tool_process_snapshot(
            tool,
            ToolProcessSnapshot::new(processes),
        ))
    }
}

fn filter_tool_process_snapshot(
    tool: &str,
    mut snapshot: ToolProcessSnapshot,
) -> ToolProcessSnapshot {
    if tool == "codex" {
        snapshot.processes.retain(|process| {
            !is_codex_plus_plus_path(Path::new(&process.executable_path))
        });
    }
    snapshot
}

#[cfg(target_os = "windows")]
fn windows_tool_process_snapshot_for_scope(
    tool: &str,
    scope: &ToolProcessScope,
) -> Result<ToolProcessSnapshot> {
    Ok(filter_tool_process_snapshot(
        tool,
        windows_process_snapshot(scope)?,
    ))
}

fn tool_process_match_names(tool: &str) -> Result<HashSet<String>> {
    let mut names = known_tool_process_names(tool)
        .into_iter()
        .map(|name| normalized_process_name(&name))
        .collect::<HashSet<_>>();
    for command in tool_runtime_commands(tool) {
        names.insert(normalized_process_name(command));
    }
    for path in known_tool_program_paths(tool) {
        if is_supported_tool_runtime_owner_path(tool, &path) {
            insert_process_match_names_for_path(&mut names, &path);
        }
    }
    if let Some(path) = load_tool_program_locations()?.get(tool) {
        if is_supported_tool_runtime_owner_path(tool, Path::new(path)) {
            insert_process_match_names_for_path(&mut names, Path::new(path));
        }
    }
    Ok(names)
}

#[cfg(any(
    target_os = "windows",
    target_os = "macos",
    target_os = "linux"
))]
fn tool_process_owner_paths(tool: &str) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    let locations = load_tool_program_locations()?;
    if let Some(path) = locations.get(tool).map(PathBuf::from).filter(|path| {
        path.exists() && is_supported_tool_runtime_owner_path(tool, path)
    }) {
        push_unique_path(&mut paths, path);
    }
    for path in known_tool_program_paths(tool)
        .into_iter()
        .filter(|path| path.exists() && is_supported_tool_runtime_owner_path(tool, path))
    {
        push_unique_path(&mut paths, path);
    }
    for command in tool_runtime_commands(tool) {
        for path in find_tool_command_paths_without_shell(tool, command)
            .into_iter()
            .filter(|path| is_supported_tool_runtime_owner_path(tool, path))
        {
            push_unique_path(&mut paths, path);
        }
    }
    Ok(paths)
}

fn is_supported_tool_runtime_owner_path(tool: &str, path: &Path) -> bool {
    (tool != "codex" || !is_codex_plus_plus_path(path))
        && !is_ambiguous_system_tool_command(tool, path)
}

fn is_ambiguous_system_tool_command(tool: &str, path: &Path) -> bool {
    if tool != "open-design" {
        return false;
    }
    let is_system_od = |candidate: &Path| {
        [Path::new("/usr/bin/od"), Path::new("/bin/od")]
            .iter()
            .any(|system_path| candidate == *system_path)
    };
    is_system_od(path)
        || fs::canonicalize(path)
            .ok()
            .is_some_and(|canonical| is_system_od(&canonical))
}

#[cfg(target_os = "windows")]
fn windows_tool_process_scope(
    tool: &str,
    match_names: &HashSet<String>,
) -> Result<ToolProcessScope> {
    let mut scope = ToolProcessScope {
        executable_names: match_names.iter().cloned().collect(),
        ..Default::default()
    };
    for path in tool_process_owner_paths(tool)? {
        insert_windows_tool_process_scope_path(&mut scope, tool, &path);
    }
    for root in tool_runtime_extension_install_roots(tool) {
        insert_windows_tool_process_scope_root(&mut scope, &root);
    }
    if !scope.command_line_markers.is_empty() {
        for name in windows_runtime_host_process_names() {
            scope
                .executable_names
                .insert(normalized_process_name(name));
        }
    }
    scope.allow_name_fallback = !scope.has_concrete_owner();
    Ok(scope)
}

#[cfg(target_os = "windows")]
fn insert_windows_tool_process_scope_path(
    scope: &mut ToolProcessScope,
    tool: &str,
    path: &Path,
) {
    let is_command_path = tool_runtime_commands(tool).iter().any(|command| {
        find_tool_command_paths_without_shell(tool, command)
            .iter()
            .any(|candidate| {
                tool_program_candidate_paths_equal(
                    &candidate.display().to_string(),
                    &path.display().to_string(),
                )
            })
    });
    if is_command_path {
        insert_tool_process_command_line_markers(scope, path);
    }
    if !path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("exe"))
    {
        return;
    }
    scope
        .exact_executable_paths
        .insert(normalized_windows_process_path(path));
    if let Ok(canonical) = fs::canonicalize(path) {
        scope
            .exact_executable_paths
            .insert(normalized_windows_process_path(&canonical));
    }

    if let Some(package_dir) = windows_appx_package_dir(path) {
        scope
            .install_roots
            .insert(normalized_windows_process_path(&package_dir));
        if let Some(family_name) = windows_appx_package_family_name(&package_dir) {
            scope
                .package_family_names
                .insert(family_name.to_ascii_lowercase());
        }
        return;
    }
    if is_command_path {
        return;
    }
    if let Some(parent) = windows_tool_install_root(tool, path) {
        scope
            .install_roots
            .insert(normalized_windows_process_path(&parent));
    }
}

#[cfg(target_os = "windows")]
fn insert_windows_tool_process_scope_root(scope: &mut ToolProcessScope, root: &Path) {
    let normalized = normalized_windows_process_path(root);
    scope.install_roots.insert(normalized.clone());
    scope.command_line_markers.insert(normalized);
    if let Ok(canonical) = fs::canonicalize(root) {
        let normalized = normalized_windows_process_path(&canonical);
        scope.install_roots.insert(normalized.clone());
        scope.command_line_markers.insert(normalized);
    }
}

#[cfg(target_os = "windows")]
fn windows_runtime_host_process_names() -> &'static [&'static str] {
    &[
        "node.exe",
        "nodejs.exe",
        "python.exe",
        "pythonw.exe",
        "py.exe",
        "bun.exe",
        "deno.exe",
        "cmd.exe",
        "powershell.exe",
        "pwsh.exe",
    ]
}

#[cfg(target_os = "windows")]
fn windows_tool_install_root(tool: &str, executable: &Path) -> Option<PathBuf> {
    let executable_key = executable
        .file_stem()
        .and_then(|value| value.to_str())
        .map(windows_process_scope_key)
        .unwrap_or_default();
    let tool_key = windows_process_scope_key(tool);
    executable
        .ancestors()
        .skip(1)
        .take(4)
        .find(|ancestor| {
            let name = ancestor
                .file_name()
                .and_then(|value| value.to_str())
                .map(windows_process_scope_key)
                .unwrap_or_default();
            name == "winunpacked"
                || [&executable_key, &tool_key]
                    .into_iter()
                    .filter(|key| key.len() >= 4)
                    .any(|key| name.contains(key))
        })
        .map(Path::to_path_buf)
}

#[cfg(target_os = "windows")]
fn windows_process_scope_key(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn unix_tool_process_scope(
    tool: &str,
    match_names: &HashSet<String>,
) -> Result<ToolProcessScope> {
    let mut scope = ToolProcessScope {
        executable_names: match_names.iter().cloned().collect(),
        ..Default::default()
    };
    for path in tool_process_owner_paths(tool)? {
        insert_unix_tool_process_scope_path(&mut scope, tool, &path);
    }
    for root in tool_runtime_extension_install_roots(tool) {
        insert_unix_tool_process_scope_root(&mut scope, &root);
    }
    if !scope.command_line_markers.is_empty() {
        for name in unix_runtime_host_process_names() {
            scope
                .executable_names
                .insert(normalized_process_name(name));
        }
    }
    scope.allow_name_fallback = !scope.has_concrete_owner();
    Ok(scope)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn insert_unix_tool_process_scope_path(
    scope: &mut ToolProcessScope,
    tool: &str,
    path: &Path,
) {
    let is_command_path = tool_runtime_commands(tool).iter().any(|command| {
        find_tool_command_paths_without_shell(tool, command)
            .iter()
            .any(|candidate| {
                tool_program_candidate_paths_equal(
                    &candidate.display().to_string(),
                    &path.display().to_string(),
                )
            })
    });
    if is_command_path {
        insert_tool_process_command_line_markers(scope, path);
    }

    #[cfg(target_os = "macos")]
    if let Some(executable) = macos_app_main_executable_path(path) {
        insert_unix_exact_executable_path(scope, &executable);
        insert_unix_tool_process_scope_root(scope, path);
        return;
    }

    if path.is_file() && native_unix_executable(path) {
        insert_unix_exact_executable_path(scope, path);
    } else if is_command_path {
        insert_tool_process_command_line_markers(scope, path);
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn insert_unix_exact_executable_path(scope: &mut ToolProcessScope, path: &Path) {
    scope
        .exact_executable_paths
        .insert(path.display().to_string());
    if let Ok(canonical) = fs::canonicalize(path) {
        scope
            .exact_executable_paths
            .insert(canonical.display().to_string());
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn insert_unix_tool_process_scope_root(scope: &mut ToolProcessScope, root: &Path) {
    let root = root.display().to_string();
    scope.install_roots.insert(root.clone());
    scope.command_line_markers.insert(root.clone());
    if let Ok(canonical) = fs::canonicalize(&root) {
        let canonical = canonical.display().to_string();
        scope.install_roots.insert(canonical.clone());
        scope.command_line_markers.insert(canonical);
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn unix_runtime_host_process_names() -> &'static [&'static str] {
    &[
        "node", "nodejs", "python", "python3", "bun", "deno", "ruby", "java", "sh",
        "bash", "zsh", "fish",
    ]
}

fn tool_runtime_extension_install_roots(tool: &str) -> Vec<PathBuf> {
    let Some(manifest) = tool_runtime_kill_manifest(tool) else {
        return Vec::new();
    };
    if manifest.extension_dir_prefixes.is_empty() {
        return Vec::new();
    }
    let home = home_dir();
    let extension_dirs = [
        home.join(".vscode/extensions"),
        home.join(".vscode-insiders/extensions"),
        home.join(".vscode-oss/extensions"),
        home.join(".vscode-server/extensions"),
        home.join(".cursor/extensions"),
        home.join(".cursor-server/extensions"),
        home.join(".windsurf/extensions"),
        home.join(".windsurf-server/extensions"),
    ];
    let prefixes = manifest
        .extension_dir_prefixes
        .iter()
        .map(|prefix| prefix.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let mut roots = Vec::new();
    for extension_dir in extension_dirs {
        let Ok(entries) = fs::read_dir(extension_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if prefixes.iter().any(|prefix| name.starts_with(prefix)) {
                push_unique_path(&mut roots, path);
            }
        }
    }
    roots
}

fn insert_tool_process_command_line_markers(scope: &mut ToolProcessScope, path: &Path) {
    let mut marker_paths = Vec::new();
    push_unique_path(&mut marker_paths, path.to_path_buf());
    if let Ok(canonical) = fs::canonicalize(path) {
        if command_runtime_canonical_is_specific(path, &canonical) {
            push_unique_path(&mut marker_paths, canonical);
        }
    }
    if let (Some(parent), Some(stem)) = (
        path.parent(),
        path.file_stem().and_then(|value| value.to_str()),
    ) {
        for sibling in [
            parent.join(stem),
            parent.join(format!("{stem}.cmd")),
            parent.join(format!("{stem}.ps1")),
        ] {
            if sibling.exists() {
                push_unique_path(&mut marker_paths, sibling.clone());
                if let Ok(canonical) = fs::canonicalize(&sibling) {
                    if command_runtime_canonical_is_specific(&sibling, &canonical) {
                        push_unique_path(&mut marker_paths, canonical);
                    }
                }
                for entry in command_wrapper_node_module_entry_paths(&sibling) {
                    #[cfg(target_os = "windows")]
                    if entry
                        .extension()
                        .and_then(|value| value.to_str())
                        .is_some_and(|value| value.eq_ignore_ascii_case("exe"))
                    {
                        scope
                            .exact_executable_paths
                            .insert(normalized_windows_process_path(&entry));
                    }
                    push_unique_path(&mut marker_paths, entry.clone());
                    if let Ok(canonical) = fs::canonicalize(&entry) {
                        if command_runtime_canonical_is_specific(&entry, &canonical) {
                            push_unique_path(&mut marker_paths, canonical);
                        }
                    }
                }
            }
        }
    }
    for marker in marker_paths {
        if !marker.is_absolute() {
            continue;
        }
        #[cfg(target_os = "windows")]
        let marker = normalized_windows_process_path(&marker);
        #[cfg(not(target_os = "windows"))]
        let marker = marker.display().to_string();
        if marker.len() >= 8 {
            scope.command_line_markers.insert(marker);
        }
    }
}

fn command_runtime_canonical_is_specific(source: &Path, canonical: &Path) -> bool {
    source == canonical
        || source
            .file_stem()
            .and_then(|value| value.to_str())
            .zip(canonical.file_stem().and_then(|value| value.to_str()))
            .is_some_and(|(source, target)| source.eq_ignore_ascii_case(target))
        || canonical
            .components()
            .any(|component| {
                component
                    .as_os_str()
                    .to_str()
                    .is_some_and(|value| value.eq_ignore_ascii_case("node_modules"))
            })
}

fn command_wrapper_node_module_entry_paths(path: &Path) -> Vec<PathBuf> {
    let Ok(metadata) = fs::metadata(path) else {
        return Vec::new();
    };
    if metadata.len() > 256 * 1024 {
        return Vec::new();
    }
    let Ok(raw) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let lower = raw.to_ascii_lowercase();
    let Some(parent) = path.parent() else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for (start, _) in lower.match_indices("node_modules") {
        let tail = &raw[start..];
        let end = tail
            .find(|character: char| {
                character.is_whitespace()
                    || matches!(character, '\"' | '\'' | '`' | ';' | ')' | '}' | '%' | '$')
            })
            .unwrap_or(tail.len());
        let relative = tail[..end].trim_end_matches(['\\', '/']);
        if relative.len() <= "node_modules".len() {
            continue;
        }
        let entry = parent.join(relative);
        if entry.exists() {
            push_unique_path(&mut entries, entry);
        }
    }
    entries
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn native_unix_executable(path: &Path) -> bool {
    let mut magic = [0_u8; 4];
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    if file.read_exact(&mut magic).is_err() {
        return false;
    }
    #[cfg(target_os = "linux")]
    return magic == [0x7f, b'E', b'L', b'F'];
    #[cfg(target_os = "macos")]
    return matches!(
        magic,
        [0xfe, 0xed, 0xfa, 0xce]
            | [0xce, 0xfa, 0xed, 0xfe]
            | [0xfe, 0xed, 0xfa, 0xcf]
            | [0xcf, 0xfa, 0xed, 0xfe]
            | [0xca, 0xfe, 0xba, 0xbe]
            | [0xbe, 0xba, 0xfe, 0xca]
            | [0xca, 0xfe, 0xba, 0xbf]
            | [0xbf, 0xba, 0xfe, 0xca]
    );
}

#[cfg(any(test, not(target_os = "windows")))]
fn app_bundle_root(path: &Path) -> Option<&Path> {
    path.ancestors().find(|ancestor| {
        ancestor
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value == "app")
    })
}

#[cfg(any(test, not(target_os = "windows")))]
fn process_path_matches_owner(
    process_path: &Path,
    snapshot_process_name: &str,
    match_names: &HashSet<String>,
    exact_executable_paths: &HashSet<String>,
    include_app_bundle_helpers: bool,
) -> bool {
    if exact_executable_paths.is_empty() {
        return match_names.contains(snapshot_process_name);
    }
    if exact_executable_paths.contains(process_path.to_string_lossy().as_ref()) {
        return true;
    }
    include_app_bundle_helpers
        && exact_executable_paths.iter().any(|executable| {
            app_bundle_root(Path::new(executable))
                .is_some_and(|bundle| process_path.starts_with(bundle))
        })
}

#[cfg(not(target_os = "windows"))]
fn process_path_matches_scope_owner(
    process_path: &Path,
    snapshot_process_name: &str,
    scope: &ToolProcessScope,
    include_app_bundle_helpers: bool,
) -> bool {
    if scope
        .exact_executable_paths
        .contains(process_path.to_string_lossy().as_ref())
        || scope
            .install_roots
            .iter()
            .any(|root| process_path.starts_with(Path::new(root)))
    {
        return true;
    }
    if include_app_bundle_helpers
        && scope.exact_executable_paths.iter().any(|executable| {
            app_bundle_root(Path::new(executable))
                .is_some_and(|bundle| process_path.starts_with(bundle))
        })
    {
        return true;
    }
    scope.allow_name_fallback && scope.executable_names.contains(snapshot_process_name)
}

#[cfg(any(test, not(target_os = "windows")))]
fn process_command_line_matches_scope_owner(
    snapshot_process_name: &str,
    command_line: Option<&str>,
    scope: &ToolProcessScope,
) -> bool {
    scope.executable_names.contains(snapshot_process_name)
        && command_line.is_some_and(|command_line| {
            scope
                .command_line_markers
                .iter()
                .any(|owner| command_line.contains(owner))
        })
}

fn insert_process_match_names_for_path(names: &mut HashSet<String>, path: &Path) {
    if let Some(file_name) = path.file_name().and_then(|value| value.to_str()) {
        names.insert(normalized_process_name(file_name));
    }
    if path.extension().and_then(|value| value.to_str()) == Some("app") {
        if let Some(stem) = path.file_stem().and_then(|value| value.to_str()) {
            names.insert(normalized_process_name(stem));
        }
    }
}

fn normalized_process_name(name: &str) -> String {
    let name = name.trim();
    #[cfg(target_os = "windows")]
    {
        name.to_ascii_lowercase()
    }
    #[cfg(not(target_os = "windows"))]
    {
        name.to_string()
    }
}

#[cfg(target_os = "windows")]
fn platform_process_snapshot(match_names: &HashSet<String>) -> Result<Vec<ToolProcessIdentity>> {
    windows_process_snapshot_by_names(match_names)
}

fn tool_program_launch_args(tool: &str) -> &'static [&'static str] {
    tool_profile(tool)
        .map(|profile| profile.launch_args)
        .unwrap_or_default()
}

fn tool_candidate_launch_args(
    tool: &str,
    candidate: &ToolProgramCandidate,
) -> &'static [&'static str] {
    if tool == "deepseek-harness" && is_desktop_program_candidate(candidate) {
        &[]
    } else {
        tool_program_launch_args(tool)
    }
}

#[cfg(all(
    not(target_os = "windows"),
    any(test, not(target_os = "macos"))
))]
fn platform_process_snapshot(match_names: &HashSet<String>) -> Result<Vec<ToolProcessIdentity>> {
    platform_process_snapshot_with_exact_paths(match_names, &HashSet::new())
}

#[cfg(not(target_os = "windows"))]
fn platform_process_snapshot_with_scope(
    scope: &ToolProcessScope,
) -> Result<Vec<ToolProcessIdentity>> {
    let mut command = helper_command("ps");
    command.args(["-axo", "pid=,lstart=,comm="]);
    let output = run_process_probe_command_with_timeout(
        &mut command,
        TOOL_PROCESS_SNAPSHOT_TIMEOUT,
        "ps process snapshot",
    )?;
    let command_lines = if scope.command_line_markers.is_empty() {
        HashMap::new()
    } else {
        let mut command = helper_command("ps");
        command.args(["-axo", "pid=,command="]);
        let output = run_process_probe_command_with_timeout(
            &mut command,
            TOOL_PROCESS_SNAPSHOT_TIMEOUT,
            "ps process command-line snapshot",
        )?;
        parse_unix_process_command_lines(&output.stdout)?
    };
    parse_unix_process_snapshot_with_scope(&output.stdout, &command_lines, scope)
}

#[cfg(not(target_os = "windows"))]
fn platform_process_snapshot_with_exact_paths(
    match_names: &HashSet<String>,
    exact_executable_paths: &HashSet<String>,
) -> Result<Vec<ToolProcessIdentity>> {
    let scope = ToolProcessScope {
        executable_names: match_names.iter().cloned().collect(),
        exact_executable_paths: exact_executable_paths.iter().cloned().collect(),
        allow_name_fallback: exact_executable_paths.is_empty(),
        ..Default::default()
    };
    platform_process_snapshot_with_scope(&scope)
}

#[cfg(all(
    not(target_os = "windows"),
    any(test, not(target_os = "macos"))
))]
fn parse_unix_process_snapshot(
    stdout: &[u8],
    match_names: &HashSet<String>,
) -> Result<Vec<ToolProcessIdentity>> {
    parse_unix_process_snapshot_with_exact_paths(stdout, match_names, &HashSet::new())
}

#[cfg(not(target_os = "windows"))]
fn parse_unix_process_snapshot_with_exact_paths(
    stdout: &[u8],
    match_names: &HashSet<String>,
    exact_executable_paths: &HashSet<String>,
) -> Result<Vec<ToolProcessIdentity>> {
    let scope = ToolProcessScope {
        executable_names: match_names.iter().cloned().collect(),
        exact_executable_paths: exact_executable_paths.iter().cloned().collect(),
        allow_name_fallback: exact_executable_paths.is_empty(),
        ..Default::default()
    };
    parse_unix_process_snapshot_with_scope(stdout, &HashMap::new(), &scope)
}

#[cfg(not(target_os = "windows"))]
fn parse_unix_process_snapshot_with_scope(
    stdout: &[u8],
    command_lines: &HashMap<u32, String>,
    scope: &ToolProcessScope,
) -> Result<Vec<ToolProcessIdentity>> {
    let stdout = std::str::from_utf8(stdout)
        .context("TOOL_CONFIG_PROCESS_SNAPSHOT_PARSE_FAILED: ps output is not UTF-8")?;
    let mut processes = Vec::new();
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let identity = parse_unix_process_snapshot_row(line)?;
        let snapshot_process_name = normalized_process_name(
            Path::new(&identity.executable_path)
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or(&identity.executable_path),
        );
        let matches_name = scope.executable_names.contains(&snapshot_process_name);
        // Linux `/proc/<pid>/exe` may be unreadable for unrelated processes
        // owned by a different user (for example PID 1 in a CI container).
        // The selected executable's file name is always included in
        // `match_names`, so discard unrelated rows before probing `/proc`.
        // A matching row still fails closed below if its executable identity
        // cannot be read, and exact-path matching remains authoritative.
        #[cfg(target_os = "linux")]
        if !matches_name && scope.install_roots.is_empty() {
            continue;
        }
        #[cfg(target_os = "linux")]
        let identity = {
            let mut identity = identity;
            match fs::read_link(format!("/proc/{}/exe", identity.pid)) {
                Ok(path) => identity.executable_path = path.display().to_string(),
                Err(error) if !matches_name => continue,
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "TOOL_CONFIG_PROCESS_IDENTITY_UNAVAILABLE: read executable path for PID {}",
                            identity.pid
                        )
                    })
                }
            }
            identity
        };
        let process_path = Path::new(identity.executable_path.trim());
        let command_line_matches = process_command_line_matches_scope_owner(
            &snapshot_process_name,
            command_lines.get(&identity.pid).map(String::as_str),
            scope,
        );
        if !process_path_matches_scope_owner(
            process_path,
            &snapshot_process_name,
            scope,
            cfg!(target_os = "macos"),
        ) && !command_line_matches
        {
            continue;
        }
        processes.push(identity);
    }
    Ok(processes)
}

#[cfg(not(target_os = "windows"))]
fn parse_unix_process_command_lines(stdout: &[u8]) -> Result<HashMap<u32, String>> {
    let stdout = std::str::from_utf8(stdout)
        .context("TOOL_CONFIG_PROCESS_SNAPSHOT_PARSE_FAILED: ps command lines are not UTF-8")?;
    let mut command_lines = HashMap::new();
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let line = line.trim_start();
        let Some(pid_end) = line.find(char::is_whitespace) else {
            return Err(anyhow!(
                "TOOL_CONFIG_PROCESS_SNAPSHOT_PARSE_FAILED: malformed command-line row: {line}"
            ));
        };
        let pid = line[..pid_end].parse::<u32>().with_context(|| {
            format!(
                "TOOL_CONFIG_PROCESS_SNAPSHOT_PARSE_FAILED: invalid PID in command-line row: {line}"
            )
        })?;
        command_lines.insert(pid, line[pid_end..].trim_start().to_string());
    }
    Ok(command_lines)
}

#[cfg(any(test, not(target_os = "windows")))]
fn parse_unix_process_snapshot_row(line: &str) -> Result<ToolProcessIdentity> {
    let mut rest = line.trim_start();
    let mut fixed = Vec::with_capacity(6);
    for _ in 0..6 {
        let token_end = rest.find(char::is_whitespace).ok_or_else(|| {
            anyhow!("TOOL_CONFIG_PROCESS_SNAPSHOT_PARSE_FAILED: malformed ps row: {line}")
        })?;
        fixed.push(&rest[..token_end]);
        rest = rest[token_end..].trim_start();
    }
    if rest.is_empty() {
        return Err(anyhow!(
            "TOOL_CONFIG_PROCESS_SNAPSHOT_PARSE_FAILED: missing executable path in ps row: {line}"
        ));
    }
    let pid = fixed[0].parse::<u32>().with_context(|| {
        format!("TOOL_CONFIG_PROCESS_SNAPSHOT_PARSE_FAILED: invalid PID in ps row: {line}")
    })?;
    let start_identity = fixed[1..6].join(" ");
    Ok(ToolProcessIdentity {
        pid,
        executable_path: rest.to_string(),
        start_identity,
    })
}

fn is_ignored_tool_program_candidate(tool: &str, candidate: &ToolProgramCandidate) -> bool {
    let path = Path::new(&candidate.path);
    tool == "codex" && is_codex_plus_plus_path(path)
}

fn tool_prefers_desktop_program(tool: &str) -> bool {
    tool_profile(tool).is_some_and(|profile| profile.desktop_preferred)
}

fn is_desktop_program_candidate(candidate: &ToolProgramCandidate) -> bool {
    matches!(
        candidate.kind.as_str(),
        "windows_exe" | "windows_appx" | "mac_app" | "linux_desktop"
    ) || (cfg!(target_os = "linux") && candidate.path.ends_with(".AppImage"))
}

fn preferred_tool_program_path_for_saved_selection(
    tool: &str,
    selected_path: &str,
    candidates: &[ToolProgramCandidate],
) -> Option<String> {
    if selected_path.trim().is_empty() {
        return None;
    }
    let selected = candidates.iter().find(|candidate| {
        tool_program_candidate_paths_equal(&candidate.path, selected_path)
    })?;
    if !is_launchable_tool_candidate(tool, selected) {
        if tool_program_selection_required(tool, candidates) {
            return None;
        }
        return select_tool_launch_candidate(tool, candidates)
            .filter(|candidate| {
                !tool_program_candidate_paths_equal(&candidate.path, selected_path)
            })
            .map(|candidate| candidate.path.clone());
    }
    // A valid saved path is an explicit user choice, including an older CLI or
    // an npx entry. Desktop/version preference applies only to automatic selection.
    None
}

fn refresh_saved_tool_program_selection(
    tool: &str,
    locations: &mut HashMap<String, String>,
    selected_path: &mut String,
    candidates: &mut [ToolProgramCandidate],
) -> Result<()> {
    if let Some(preferred_path) =
        preferred_tool_program_path_for_saved_selection(tool, selected_path, candidates)
    {
        *selected_path = preferred_path;
        locations.insert(tool.to_string(), selected_path.clone());
        save_tool_program_locations(locations)?;
    }
    for candidate in candidates {
        candidate.selected =
            tool_program_candidate_paths_equal(&candidate.path, selected_path);
    }
    Ok(())
}

fn select_tool_launch_candidate<'a>(
    tool: &str,
    candidates: &'a [ToolProgramCandidate],
) -> Option<&'a ToolProgramCandidate> {
    let selected = candidates
        .iter()
        .filter(|candidate| is_launchable_tool_candidate(tool, candidate))
        .find(|candidate| candidate.selected);
    if let Some(selected) = selected {
        return Some(selected);
    }
    select_automatic_tool_launch_candidate(tool, candidates)
}

fn select_automatic_tool_launch_candidate<'a>(
    tool: &str,
    candidates: &'a [ToolProgramCandidate],
) -> Option<&'a ToolProgramCandidate> {
    if tool == "codex" {
        return select_automatic_codex_launch_candidate(candidates);
    }
    if tool == "claude-desktop" {
        // AppX discovery is already ordered by package version
        // descending. Preserve that order instead of using file mtime,
        // which is not a reliable proxy for package version.
        if let Some(candidate) = candidates.iter().find(|candidate| {
            candidate.kind == "windows_appx"
                && is_launchable_tool_candidate(tool, candidate)
        }) {
            return Some(candidate);
        }
    }
    candidates
        .iter()
        .filter(|candidate| is_launchable_tool_candidate(tool, candidate))
        .max_by(|left, right| compare_tool_program_candidates(tool, left, right))
}

fn select_automatic_codex_launch_candidate(
    candidates: &[ToolProgramCandidate],
) -> Option<&ToolProgramCandidate> {
    // ChatGPT is the current desktop product. Keep the legacy Codex desktop
    // app as a fallback, but never let its package kind or mtime outrank ChatGPT.
    select_codex_product_candidate(candidates, is_chatgpt_desktop_path)
        .or_else(|| select_codex_product_candidate(candidates, is_legacy_codex_desktop_path))
}

fn select_codex_product_candidate(
    candidates: &[ToolProgramCandidate],
    matches_product: fn(&Path) -> bool,
) -> Option<&ToolProgramCandidate> {
    let launchable_product = |candidate: &&ToolProgramCandidate| {
        is_launchable_tool_candidate("codex", candidate)
            && matches_product(Path::new(&candidate.path))
    };
    candidates
        .iter()
        .filter(launchable_product)
        .find(|candidate| candidate.kind == "windows_appx")
        .or_else(|| {
            candidates
                .iter()
                .filter(launchable_product)
                .max_by(|left, right| {
                    tool_program_candidate_priority("codex", left)
                        .cmp(&tool_program_candidate_priority("codex", right))
                        .then_with(|| left.modified_at_unix.cmp(&right.modified_at_unix))
                })
        })
}

fn compare_tool_program_candidates(
    tool: &str,
    left: &ToolProgramCandidate,
    right: &ToolProgramCandidate,
) -> std::cmp::Ordering {
    let version_order = left
        .version
        .as_deref()
        .zip(right.version.as_deref())
        .and_then(|(left, right)| compare_tool_program_versions(left, right))
        .unwrap_or(std::cmp::Ordering::Equal);
    let priority_order = tool_program_candidate_priority(tool, left)
        .cmp(&tool_program_candidate_priority(tool, right));
    let cache_order = is_npx_cache_path(&right.path).cmp(&is_npx_cache_path(&left.path));
    // Keep product/launch-surface priorities intact. Within one priority tier,
    // compare real versions before cache provenance and file modification time.
    priority_order
        .then_with(|| left.version.is_some().cmp(&right.version.is_some()))
        .then(version_order)
        .then(cache_order)
        .then_with(|| left.modified_at_unix.cmp(&right.modified_at_unix))
}

fn compare_tool_program_versions(left: &str, right: &str) -> Option<std::cmp::Ordering> {
    fn parse(raw: &str) -> Option<(Vec<u64>, Option<semver::Prerelease>)> {
        if let Ok(version) = semver::Version::parse(raw) {
            return Some((
                vec![version.major, version.minor, version.patch],
                (!version.pre.is_empty()).then_some(version.pre),
            ));
        }
        let numeric = raw
            .split('.')
            .map(|part| part.parse::<u64>().ok())
            .collect::<Option<Vec<_>>>()?;
        (1..=4).contains(&numeric.len()).then_some((numeric, None))
    }
    let (left_numbers, left_pre) = parse(left)?;
    let (right_numbers, right_pre) = parse(right)?;
    for index in 0..left_numbers.len().max(right_numbers.len()) {
        let order = left_numbers.get(index).unwrap_or(&0).cmp(right_numbers.get(index).unwrap_or(&0));
        if !order.is_eq() {
            return Some(order);
        }
    }
    Some(match (left_pre, right_pre) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(left), Some(right)) => left.cmp(&right),
    })
}

fn tool_program_candidate_priority(tool: &str, candidate: &ToolProgramCandidate) -> u16 {
    if tool == "hermes" {
        if is_hermes_desktop_app_path(Path::new(&candidate.path)) {
            return 900;
        }
        if is_hermes_setup_app_path(Path::new(&candidate.path)) {
            return 100;
        }
    }
    if tool == "codex" {
        let product = if is_chatgpt_desktop_path(Path::new(&candidate.path)) {
            1_000
        } else if is_legacy_codex_desktop_path(Path::new(&candidate.path)) {
            700
        } else {
            0
        };
        let package = match candidate.kind.as_str() {
            "windows_appx" => 90,
            "mac_app" => 80,
            "windows_exe" => 70,
            _ => 0,
        };
        return product + package;
    }
    let base_priority = match (tool, candidate.kind.as_str()) {
        ("claude-desktop", "windows_appx") => 900,
        ("claude-desktop", "mac_app") => 850,
        ("claude-desktop", "windows_exe") => 700,
        (tool, "windows_appx" | "mac_app" | "linux_desktop")
            if tool_prefers_desktop_program(tool)
                && is_desktop_program_candidate(candidate) =>
        {
            800
        }
        ("claude" | "gemini", "command") => 800,
        (_, "command") => 600,
        // An allowlisted native EXE is the most reliable Windows launch target.
        // Keep AppX's product-specific priority above it where applicable.
        (_, "windows_exe") => 850,
        (_, "mac_app") => 500,
        (_, "windows_appx") => 450,
        _ => 300,
    };
    if candidate.kind == "command"
        && candidate.path.to_ascii_lowercase().ends_with(".exe")
    {
        base_priority + 25
    } else {
        base_priority
    }
}

fn is_npx_cache_path(path: &str) -> bool {
    path.replace('\\', "/")
        .to_ascii_lowercase()
        .contains("/_npx/")
}

fn normalized_tool_program_identity(value: &str) -> String {
    let file_name = value
        .replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or(value)
        .trim()
        .to_ascii_lowercase();
    [".exe", ".cmd", ".ps1", ".bat", ".com", ".app"]
        .iter()
        .find_map(|suffix| file_name.strip_suffix(suffix))
        .unwrap_or(&file_name)
        .to_string()
}

fn tool_program_identity_aliases(tool: &str) -> &'static [&'static str] {
    tool_profile(tool)
        .map(|profile| profile.identity_aliases)
        .unwrap_or_default()
}

fn is_tool_owned_program_path(tool: &str, path: &Path) -> bool {
    if tool == "codex" {
        return is_supported_codex_desktop_path(path);
    }
    if tool == "hermes"
        && (is_hermes_desktop_app_path(path) || is_hermes_setup_app_path(path))
    {
        return true;
    }
    let Some(manifest) = tool_runtime_kill_manifest(tool) else {
        return false;
    };
    let identity = normalized_tool_program_identity(&path.as_os_str().to_string_lossy());
    manifest
        .commands
        .iter()
        .chain(manifest.windows_process_names.iter())
        .chain(manifest.macos_process_names.iter())
        .chain(manifest.linux_process_names.iter())
        .chain(tool_program_identity_aliases(tool).iter())
        .any(|allowed| normalized_tool_program_identity(allowed) == identity)
}

fn is_launchable_tool_candidate(tool: &str, candidate: &ToolProgramCandidate) -> bool {
    if !candidate.exists
        || is_ignored_tool_program_candidate(tool, candidate)
        || !is_tool_owned_program_path(tool, Path::new(&candidate.path))
    {
        return false;
    }
    if tool == "codex" && !is_supported_codex_desktop_path(Path::new(&candidate.path)) {
        return false;
    }
    true
}

fn is_codex_plus_plus_path(path: &Path) -> bool {
    let text = path.display().to_string().to_ascii_lowercase();
    text.contains("codex++") || text.contains("codexplusplus")
}

fn remember_running_tool_program_path(tool: &str) -> Result<()> {
    let Some(path) = running_tool_launch_path(tool)? else {
        return Ok(());
    };
    if tool == "codex" && is_codex_plus_plus_path(&path) {
        return Ok(());
    }
    let path_text = path.display().to_string();
    let mut locations = load_tool_program_locations()?;
    // A restart may discover another edition running. Remember it only when
    // there is no usable saved choice; never replace a user's selected program.
    let should_update = locations
            .get(tool)
            .map(|selected| {
                let selected_path = Path::new(selected);
                let candidate = ToolProgramCandidate {
                    path: selected.clone(),
                    label: "selected".to_string(),
                    kind: selected_path_kind(tool, selected),
                    exists: selected_path.exists(),
                    selected: true,
                    ..Default::default()
                };
                !is_launchable_tool_candidate(tool, &candidate)
            })
            .unwrap_or(true);
    if should_update {
        locations.insert(tool.to_string(), path_text);
        save_tool_program_locations(&locations)?;
    }
    Ok(())
}

fn is_chatgpt_desktop_path(path: &Path) -> bool {
    let text = path
        .display()
        .to_string()
        .replace('\\', "/")
        .to_ascii_lowercase();
    text.ends_with("/chatgpt.exe")
        || text.ends_with("/chatgpt.app")
        || text.contains("/chatgpt.app/contents/macos/chatgpt")
}

fn is_legacy_codex_desktop_path(path: &Path) -> bool {
    let text = path
        .display()
        .to_string()
        .replace('\\', "/")
        .to_ascii_lowercase();
    text.ends_with("/codex.app")
        || text.contains("/codex.app/contents/macos/codex")
        || text.ends_with("/app/codex.exe")
        || text.ends_with("/codex/codex.exe")
        || text.ends_with("/openai codex/codex.exe")
}

fn is_supported_codex_desktop_path(path: &Path) -> bool {
    !is_codex_plus_plus_path(path)
        && (is_chatgpt_desktop_path(path) || is_legacy_codex_desktop_path(path))
}

fn running_tool_launch_path(tool: &str) -> Result<Option<PathBuf>> {
    let paths = running_tool_executable_paths(tool)?;
    if tool == "codex" {
        let candidates = paths
            .iter()
            .map(|path| ToolProgramCandidate {
                path: path.display().to_string(),
                label: "running".to_string(),
                kind: tool_path_candidate_kind(tool, &path.display().to_string()),
                exists: path.exists(),
                ..Default::default()
            })
            .collect::<Vec<_>>();
        return Ok(select_automatic_codex_launch_candidate(&candidates)
            .map(|candidate| PathBuf::from(&candidate.path)));
    }
    Ok(paths.into_iter()
        .find(|path| {
            let candidate = ToolProgramCandidate {
                path: path.display().to_string(),
                label: "running".to_string(),
                kind: tool_path_candidate_kind(tool, &path.display().to_string()),
                exists: path.exists(),
                ..Default::default()
            };
            is_launchable_tool_candidate(tool, &candidate)
        }))
}

fn is_hermes_setup_app_path(path: &Path) -> bool {
    if !path
        .file_name()
        .and_then(|value| value.to_str())
        .map(|value| value.ends_with(".app"))
        .unwrap_or(false)
    {
        return false;
    }
    if path
        .join("Contents")
        .join("MacOS")
        .join("Hermes-Setup")
        .exists()
    {
        return true;
    }
    fs::read_to_string(path.join("Contents").join("Info.plist"))
        .map(|raw| {
            raw.contains("com.nousresearch.hermes.setup")
                || raw.contains("<string>Hermes-Setup</string>")
        })
        .unwrap_or(false)
}

fn is_hermes_desktop_app_path(path: &Path) -> bool {
    if !path
        .file_name()
        .and_then(|value| value.to_str())
        .map(|value| value == "Hermes.app" || value == "Hermes Agent.app")
        .unwrap_or(false)
    {
        return false;
    }
    if path.join("Contents").join("MacOS").join("Hermes").exists() {
        return true;
    }
    fs::read_to_string(path.join("Contents").join("Info.plist"))
        .map(|raw| {
            raw.contains("com.nousresearch.hermes</string>")
                || raw.contains("<string>Hermes</string>")
        })
        .unwrap_or(false)
        && !is_hermes_setup_app_path(path)
}

#[cfg(target_os = "macos")]
fn launch_macos_app(
    path: &Path,
    launch_context: Option<&ToolConfigLaunchContext<'_>>,
) -> Result<()> {
    let mut command = helper_command("open");
    command.arg(path);
    let output = match launch_context {
        Some(context) => run_launch_helper_command_with_timeout(
            &mut command,
            TOOL_HELPER_COMMAND_TIMEOUT,
            &format!("open {}", path.display()),
            context,
        ),
        None => run_helper_command_with_timeout(
            &mut command,
            TOOL_HELPER_COMMAND_TIMEOUT,
            &format!("open {}", path.display()),
        ),
    }?;
    if output.status.success() {
        Ok(())
    } else {
        Err(anyhow!("failed to open {}", path.display()))
    }
}

#[cfg(not(target_os = "macos"))]
fn launch_macos_app(
    path: &Path,
    launch_context: Option<&ToolConfigLaunchContext<'_>>,
) -> Result<()> {
    let mut command = Command::new(path);
    spawn_launch_command(&mut command, launch_context, &path.display().to_string())?;
    Ok(())
}

#[cfg(all(test, target_os = "macos"))]
fn close_macos_app_instances(path: &Path) -> Result<()> {
    let Some(app_name) = path.file_stem().and_then(|value| value.to_str()) else {
        return Err(anyhow!(
            "TOOL_CONFIG_PROCESS_IDENTITY_UNAVAILABLE: app path has no name: {}",
            path.display()
        ));
    };
    let script = format!(
        "tell application {} to quit",
        sh_quote_for_applescript(app_name)
    );
    let mut osascript = helper_command("osascript");
    osascript.arg("-e").arg(script);
    run_macos_quit_helper(&mut osascript, app_name)?;
    thread::sleep(Duration::from_millis(1200));
    let mut by_name = helper_command("pkill");
    by_name.args(["-x", app_name]);
    run_close_helper_command_with_timeout(
        &mut by_name,
        TOOL_HELPER_COMMAND_TIMEOUT,
        &format!("pkill -x {app_name}"),
        &[1],
    )?;
    let executable_pattern = format!("{}/Contents/MacOS/", path.display());
    let mut by_path = helper_command("pkill");
    by_path.args(["-f", &executable_pattern]);
    run_close_helper_command_with_timeout(
        &mut by_path,
        TOOL_HELPER_COMMAND_TIMEOUT,
        &format!("pkill -f {executable_pattern}"),
        &[1],
    )?;
    close_codex_app_server_for_app(path)?;
    thread::sleep(Duration::from_millis(500));
    Ok(())
}

#[cfg(all(test, not(target_os = "macos")))]
fn close_macos_app_instances(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(all(test, target_os = "macos"))]
fn run_macos_quit_helper(command: &mut Command, app_name: &str) -> Result<bool> {
    let output = run_helper_command_with_timeout(
        command,
        TOOL_HELPER_COMMAND_TIMEOUT,
        &format!("osascript quit {app_name}"),
    )?;
    if output.status.success() {
        return Ok(true);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("(-600)") || stderr.to_ascii_lowercase().contains("not running") {
        return Ok(false);
    }
    Err(anyhow!(
        "TOOL_CONFIG_HELPER_FAILED: osascript quit {app_name} exited with {}: {}",
        output.status,
        stderr.trim()
    ))
}

#[cfg(all(target_os = "macos", test))]
fn close_codex_app_server_for_app(_path: &Path) -> Result<()> {
    Ok(())
}

fn launch_command_tool_with_env(
    tool: &str,
    path: &Path,
    envs: &[(&str, &str)],
    removed_envs: &[&str],
    launch_context: Option<&ToolConfigLaunchContext<'_>>,
) -> Result<()> {
    launch_command_tool_with_env_and_args(
        tool,
        path,
        &[],
        envs,
        removed_envs,
        launch_context,
    )
}

fn launch_command_tool_with_env_and_args(
    tool: &str,
    path: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
    removed_envs: &[&str],
    launch_context: Option<&ToolConfigLaunchContext<'_>>,
) -> Result<()> {
    let launch_path = command_launch_path(path);
    #[cfg(not(target_os = "windows"))]
    if tool_profile(tool).is_some_and(|profile| profile.command_launch == ToolCommandLaunch::Desktop) {
        // `code` and similar launchers open a GUI and may exit immediately.
        // Verify the app process instead of requiring a terminal/CLI handshake.
        let mut command = Command::new(&launch_path);
        command.args(args);
        for name in removed_envs {
            command.env_remove(name);
        }
        for (name, value) in envs {
            command.env(name, value);
        }
        spawn_launch_command(&mut command, launch_context, &path.display().to_string())?;
        return wait_for_tool_program_start(tool);
    }
    launch_command_in_terminal(
        tool,
        &launch_path,
        args,
        envs,
        removed_envs,
        launch_context,
    )
}

#[cfg(test)]
fn close_command_candidate_instances(
    tool: &str,
    candidate: &ToolProgramCandidate,
    path: &Path,
) -> Result<()> {
    let command = tool_command_name(candidate, path);
    if cfg!(target_os = "macos") && tool == "codex" && command == "codex" {
        return close_codex_command_processes();
    }
    close_command_instances(&command, path)
}

#[cfg(test)]
fn tool_command_name(candidate: &ToolProgramCandidate, path: &Path) -> String {
    if !candidate.label.trim().is_empty()
        && !matches!(candidate.label.as_str(), "Selected" | "Auto-detected")
    {
        return candidate.label.clone();
    }
    path.file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("tool")
        .to_string()
}

#[cfg(test)]
fn close_command_instances(_command: &str, _path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(any(unix, test))]
fn unix_terminal_launch_script(
    path: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
    removed_envs: &[&str],
    handshake: &CommandLaunchHandshake,
) -> String {
    let mut environment = String::new();
    for name in removed_envs {
        environment.push_str("unset ");
        environment.push_str(name);
        environment.push('\n');
    }
    for (name, value) in envs {
        environment.push_str("export ");
        environment.push_str(name);
        environment.push('=');
        environment.push_str(&sh_quote(value));
        environment.push('\n');
    }
    let command = std::iter::once(path.display().to_string())
        .chain(args.iter().map(|value| (*value).to_string()))
        .map(|value| sh_quote(&value))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "#!/bin/sh\n{environment}printf started > {}\n{}\nstatus=$?\nprintf 'exit=%s' \"$status\" > {}\nexit \"$status\"\n",
        sh_quote(&handshake.started.display().to_string()),
        command,
        sh_quote(&handshake.exited.display().to_string()),
    )
}

#[cfg(unix)]
fn write_unix_terminal_launch_script(
    tool: &str,
    path: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
    removed_envs: &[&str],
) -> Result<(PathBuf, CommandLaunchHandshake)> {
    use std::os::unix::fs::PermissionsExt;
    let script_dir = crate::client_data_root().join("runtime").join("tools");
    let handshake = create_command_launch_handshake(tool)?;
    let script_path = script_dir.join(format!("launch-{}.sh", handshake.id));
    // atomic_write creates a private (0600) tempfile before writing credentials.
    atomic_write(
        &script_path,
        unix_terminal_launch_script(path, args, envs, removed_envs, &handshake).as_bytes(),
    )?;
    fs::set_permissions(&script_path, fs::Permissions::from_mode(0o700))?;
    Ok((script_path, handshake))
}

#[cfg(target_os = "macos")]
fn launch_command_in_terminal(
    tool: &str,
    path: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
    removed_envs: &[&str],
    launch_context: Option<&ToolConfigLaunchContext<'_>>,
) -> Result<()> {
    let (script_path, handshake) =
        write_unix_terminal_launch_script(tool, path, args, envs, removed_envs)?;
    let mut command = helper_command("open");
    command.args(["-a", "Terminal"]).arg(&script_path);
    let output = match match launch_context {
        Some(context) => run_launch_helper_command_with_timeout(
            &mut command,
            TOOL_HELPER_COMMAND_TIMEOUT,
            &format!("open Terminal {}", script_path.display()),
            context,
        ),
        None => run_helper_command_with_timeout(
            &mut command,
            TOOL_HELPER_COMMAND_TIMEOUT,
            &format!("open Terminal {}", script_path.display()),
        ),
    } {
        Ok(output) => output,
        Err(error) => {
            let _ = fs::remove_file(&script_path);
            return Err(error);
        }
    };
    if !output.status.success() {
        let _ = fs::remove_file(&script_path);
        return Err(anyhow!("failed to open Terminal"));
    }
    let confirmed = confirm_command_launch(&handshake);
    let _ = fs::remove_file(&script_path);
    confirmed
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn launch_command_in_terminal(
    tool: &str,
    path: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
    removed_envs: &[&str],
    launch_context: Option<&ToolConfigLaunchContext<'_>>,
) -> Result<()> {
    let (terminal, terminal_args) = linux_terminal_candidates()
        .iter()
        .find_map(|(name, args)| {
            find_tool_command_paths_without_shell(tool, name)
                .into_iter()
                .find(|path| path.is_file())
                .map(|path| (path, *args))
        })
        .ok_or_else(|| anyhow!("TOOL_START_TERMINAL_MISSING: install a terminal emulator (x-terminal-emulator, GNOME Terminal, Konsole, XFCE Terminal or xterm) to open this interactive tool"))?;
    let (script_path, handshake) =
        write_unix_terminal_launch_script(tool, path, args, envs, removed_envs)?;
    let mut command = Command::new(&terminal);
    // Put environment changes in the script: a terminal's existing D-Bus
    // process need not inherit the environment of this launcher.
    command.args(terminal_args).arg("/bin/sh").arg(&script_path);
    let result = spawn_launch_command(&mut command, launch_context, "interactive terminal")
        .and_then(|_child| confirm_command_launch(&handshake));
    let _ = fs::remove_file(&script_path);
    result
}

#[cfg(any(target_os = "linux", test))]
fn linux_terminal_candidates() -> &'static [(&'static str, &'static [&'static str])] {
    &[
        ("x-terminal-emulator", &["-e"]),
        ("gnome-terminal", &["--"]),
        ("konsole", &["-e"]),
        ("xfce4-terminal", &["-x"]),
        ("xterm", &["-e"]),
    ]
}

#[cfg(target_os = "windows")]
fn launch_command_in_terminal(
    tool: &str,
    path: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
    removed_envs: &[&str],
    launch_context: Option<&ToolConfigLaunchContext<'_>>,
) -> Result<()> {
    let script_dir = crate::client_data_root().join("runtime").join("tools");
    fs::create_dir_all(&script_dir)?;
    let handshake = create_command_launch_handshake(tool)?;
    let script_path = script_dir.join(format!("launch-{}.ps1", handshake.id));
    atomic_write(
        &script_path,
        windows_terminal_launch_script_with_handshake(
            path,
            &tool_display_name_from_path(path),
            args,
            Some(&handshake),
        )
        .as_bytes(),
    )?;
    spawn_windows_command_terminal(
        &script_path,
        &format!("CONST API - {}", tool_display_name_from_path(path)),
        envs,
        removed_envs,
        launch_context,
    )
    .with_context(|| format!("open terminal for {}", path.display()))?;
    confirm_command_launch(&handshake)
}

#[cfg(target_os = "windows")]
#[cfg(test)]
fn windows_terminal_launch_script(path: &Path, title: &str) -> String {
    windows_terminal_launch_script_with_handshake(path, title, &[], None)
}

#[cfg(target_os = "windows")]
fn windows_terminal_launch_script_with_handshake(
    path: &Path,
    title: &str,
    args: &[&str],
    handshake: Option<&CommandLaunchHandshake>,
) -> String {
    let started = handshake
        .map(|handshake| {
            format!(
                "Set-Content -LiteralPath {} -Value 'started' -NoNewline\r\n",
                ps_single_quote(&handshake.started.display().to_string())
            )
        })
        .unwrap_or_default();
    let exited_marker = handshake
        .map(|handshake| {
            format!(
                "Set-Content -LiteralPath {} -Value (\"exit=\" + $constExitCode) -NoNewline\r\n",
                ps_single_quote(&handshake.exited.display().to_string())
            )
        })
        .unwrap_or_default();
    let exited = format!(
        "$constCommandSucceeded = $?\r\n$constExitCode = if ($LASTEXITCODE -ne $null) {{ $LASTEXITCODE }} elseif ($constCommandSucceeded) {{ 0 }} else {{ 1 }}\r\n{exited_marker}"
    );
    let argument_line = args
        .iter()
        .map(|value| ps_single_quote(value))
        .collect::<Vec<_>>()
        .join(" ");
    let argument_line = if argument_line.is_empty() {
        String::new()
    } else {
        format!(" {argument_line}")
    };
    format!(
        "$ErrorActionPreference = 'Continue'\r\n$Host.UI.RawUI.WindowTitle = {}\r\n{}& {}{}\r\n{}if ($constExitCode -ne $null -and $constExitCode -ne 0) {{ Write-Host ({} + $constExitCode) }}\r\n",
        ps_single_quote(&format!("CONST API - {title}")),
        started,
        ps_single_quote(&path.display().to_string()),
        argument_line,
        exited,
        ps_single_quote(&format!("{title} exited with code "))
    )
}

#[cfg(target_os = "windows")]
fn tool_display_name_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("tool")
        .to_string()
}

#[cfg(target_os = "windows")]
fn spawn_windows_command_terminal(
    script_path: &Path,
    title: &str,
    envs: &[(&str, &str)],
    removed_envs: &[&str],
    launch_context: Option<&ToolConfigLaunchContext<'_>>,
) -> Result<()> {
    let purpose = format!("terminal script {}", script_path.display());
    let launch = || match spawn_windows_terminal_script_window(
        script_path,
        title,
        envs,
        removed_envs,
    ) {
        Ok(()) => Ok(()),
        Err(terminal_error) => {
            eprintln!(
                "[const-api][tool-config] Windows Terminal unavailable; falling back to PowerShell: {terminal_error:#}"
            );
            spawn_powershell_script_window(script_path, envs, removed_envs).map_err(
                |powershell_error| {
                    anyhow!(
                        "failed to launch command terminal; Windows Terminal: {terminal_error:#}; PowerShell fallback: {powershell_error:#}"
                    )
                },
            )
        }
    };
    match launch_context {
        Some(context) => context.spawn(&purpose, launch),
        None => launch(),
    }
}

#[cfg(target_os = "windows")]
fn spawn_windows_terminal_script_window(
    script_path: &Path,
    title: &str,
    envs: &[(&str, &str)],
    removed_envs: &[&str],
) -> Result<()> {
    let mut command = Command::new(windows_terminal_executable());
    command.args([
        "-w",
        "new",
        "new-tab",
        "--inheritEnvironment",
        "--title",
        title,
    ]);
    if let Ok(directory) = std::env::current_dir() {
        command.arg("--startingDirectory").arg(directory);
    }
    command
        .arg("powershell.exe")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-NoExit",
            "-File",
        ])
        .arg(script_path);
    apply_windows_command_environment(&mut command, envs, removed_envs);
    spawn_windows_detached_launch_proxy(
        &command,
        &format!("Windows Terminal script {}", script_path.display()),
    )?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn windows_terminal_executable() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|path| path.join("Microsoft").join("WindowsApps").join("wt.exe"))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("wt.exe"))
}

#[cfg(target_os = "windows")]
fn spawn_powershell_script_window(
    script_path: &Path,
    envs: &[(&str, &str)],
    removed_envs: &[&str],
) -> Result<()> {
    use std::os::windows::process::CommandExt;

    let mut command = Command::new("powershell.exe");
    command
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-NoExit",
            "-File",
        ])
        .arg(script_path)
        .creation_flags(windows_visible_terminal_flag());
    apply_windows_command_environment(&mut command, envs, removed_envs);
    spawn_windows_detached_launch_proxy(
        &command,
        &format!("PowerShell script {}", script_path.display()),
    )?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn apply_windows_command_environment(
    command: &mut Command,
    envs: &[(&str, &str)],
    removed_envs: &[&str],
) {
    for name in removed_envs {
        command.env_remove(name);
    }
    for (key, value) in envs {
        command.env(key, value);
    }
}

#[cfg(target_os = "windows")]
fn command_launch_path(path: &Path) -> PathBuf {
    windows_command_launch_candidates(path, "")
        .into_iter()
        .find(|candidate| candidate.exists())
        .unwrap_or_else(|| path.to_path_buf())
}

#[cfg(not(target_os = "windows"))]
fn command_launch_path(path: &Path) -> PathBuf {
    path.to_path_buf()
}

#[cfg(all(test, target_os = "windows"))]
fn close_windows_executable(path: &Path) -> Result<()> {
    if let Some(file_name) = path.file_name().and_then(|value| value.to_str()) {
        close_windows_process_name(file_name)?;
    }
    Ok(())
}

#[cfg(all(test, not(target_os = "windows")))]
fn close_windows_executable(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(target_os = "windows")]
fn running_tool_executable_paths(tool: &str) -> Result<Vec<PathBuf>> {
    windows_running_process_executable_paths(&known_tool_process_names(tool))
}

#[cfg(not(target_os = "windows"))]
fn running_tool_executable_paths(_tool: &str) -> Result<Vec<PathBuf>> {
    Ok(Vec::new())
}

#[cfg(target_os = "windows")]
fn windows_running_process_executable_paths(names: &[String]) -> Result<Vec<PathBuf>> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let match_names = names
        .iter()
        .map(|name| normalized_process_name(name))
        .collect::<HashSet<_>>();
    let mut paths = Vec::new();
    for process in platform_process_snapshot(&match_names)? {
        let path = PathBuf::from(process.executable_path);
        if path.exists() {
            push_unique_path(&mut paths, path);
        }
    }
    Ok(paths)
}

#[cfg(test)]
fn close_known_tool_processes(tool: &str) -> Result<()> {
    for name in known_tool_process_names(tool) {
        close_process_name(&name)?;
    }
    Ok(())
}

fn tool_runtime_commands(tool: &str) -> &'static [&'static str] {
    tool_runtime_kill_manifest(tool)
        .map(|manifest| manifest.commands)
        .unwrap_or_default()
}

fn known_tool_process_names(tool: &str) -> Vec<String> {
    let Some(manifest) = tool_runtime_kill_manifest(tool) else {
        return Vec::new();
    };
    #[cfg(target_os = "windows")]
    let platform_names = manifest.windows_process_names;
    #[cfg(target_os = "macos")]
    let platform_names = manifest.macos_process_names;
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let platform_names = manifest.linux_process_names;

    let mut names = Vec::new();
    for name in platform_names {
        if !names.iter().any(|existing: &String| existing == name) {
            names.push((*name).to_string());
        }
    }
    for command in manifest.commands {
        #[cfg(target_os = "windows")]
        let name = format!("{command}.exe");
        #[cfg(not(target_os = "windows"))]
        let name = (*command).to_string();
        if !names
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&name))
        {
            names.push(name);
        }
    }
    names
}

#[cfg(target_os = "macos")]
fn macos_app_main_executable_path(path: &Path) -> Option<PathBuf> {
    if !path
        .file_name()
        .and_then(|value| value.to_str())
        .map(|value| value.ends_with(".app"))
        .unwrap_or(false)
    {
        return None;
    }
    let executable = macos_info_plist_string(path, "CFBundleExecutable").or_else(|| {
        path.file_stem()
            .and_then(|value| value.to_str())
            .map(str::to_string)
    })?;
    let executable_path = path.join("Contents").join("MacOS").join(executable);
    executable_path.exists().then_some(executable_path)
}

#[cfg(target_os = "macos")]
fn macos_info_plist_string(path: &Path, key: &str) -> Option<String> {
    let raw = fs::read_to_string(path.join("Contents").join("Info.plist")).ok()?;
    let key_tag = format!("<key>{key}</key>");
    let after_key = raw.split_once(&key_tag)?.1;
    let after_string = after_key.split_once("<string>")?.1;
    let value = after_string.split_once("</string>")?.0.trim();
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(target_os = "macos")]
fn process_command_line_matches_executable(command_line: &str, executable: &Path) -> bool {
    let line = command_line.trim_start();
    let executable = executable.display().to_string();
    line == executable
        || line
            .strip_prefix(&executable)
            .map(|rest| rest.starts_with(' ') || rest.starts_with('\t'))
            .unwrap_or(false)
}

#[cfg(test)]
fn close_codex_command_processes() -> Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn codex_ucomm_process_command_line(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix("codex")?;
    if !rest
        .chars()
        .next()
        .map(char::is_whitespace)
        .unwrap_or(false)
    {
        return None;
    }
    Some(rest.trim_start())
}

#[cfg(target_os = "macos")]
fn codex_pid_and_command_line(line: &str) -> Option<(u32, &str)> {
    let line = line.trim_start();
    let pid_end = line.find(char::is_whitespace)?;
    let pid = line[..pid_end].parse().ok()?;
    let command_line = codex_ucomm_process_command_line(&line[pid_end..])?;
    Some((pid, command_line))
}

#[cfg(target_os = "macos")]
fn is_codex_app_server_command_line(command_line: &str) -> bool {
    command_line == "codex app-server"
        || command_line.starts_with("codex app-server ")
        || command_line.contains("/Contents/Resources/codex app-server")
}

#[cfg(target_os = "macos")]
fn codex_app_server_executable_path(app_path: &Path) -> Option<PathBuf> {
    if !matches!(
        app_path.file_name().and_then(|value| value.to_str()),
        Some("ChatGPT.app") | Some("Codex.app")
    ) {
        return None;
    }
    let path = app_path.join("Contents").join("Resources").join("codex");
    path.exists().then_some(path)
}

#[cfg(target_os = "macos")]
fn codex_app_server_command_line_matches_app(command_line: &str, app_path: &Path) -> bool {
    let Some(server_path) = codex_app_server_executable_path(app_path) else {
        return false;
    };
    let server = server_path.display().to_string();
    let line = command_line.trim_start();
    line == format!("{server} app-server") || line.starts_with(&format!("{server} app-server "))
}

#[cfg(target_os = "macos")]
#[cfg_attr(test, allow(dead_code))]
fn macos_codex_app_server_process_ids_for_app(app_path: &Path) -> Result<Vec<u32>> {
    let mut probe = helper_command("ps");
    probe.args(["-axo", "pid=,ucomm=,command="]);
    let output = run_process_probe_command_with_timeout(
        &mut probe,
        TOOL_HELPER_COMMAND_TIMEOUT,
        "ps Codex app-server snapshot",
    )?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(codex_pid_and_command_line)
        .filter(|(_, command_line)| {
            codex_app_server_command_line_matches_app(command_line, app_path)
        })
        .map(|(pid, _)| pid)
        .collect())
}

#[cfg(test)]
fn close_process_name(_name: &str) -> Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
#[cfg_attr(test, allow(dead_code))]
fn macos_process_ids_by_name(name: &str) -> Result<Vec<u32>> {
    let mut probe = helper_command("ps");
    probe.args(["-axo", "pid=,ucomm="]);
    let output = run_process_probe_command_with_timeout(
        &mut probe,
        TOOL_HELPER_COMMAND_TIMEOUT,
        "ps process-name snapshot",
    )?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| macos_process_id_for_name(line, name))
        .collect())
}

#[cfg(target_os = "macos")]
fn macos_process_id_for_name(line: &str, name: &str) -> Option<u32> {
    let line = line.trim_start();
    let pid_end = line.find(char::is_whitespace)?;
    let pid = line[..pid_end].parse().ok()?;
    let process_name = line[pid_end..].trim();
    (process_name == name).then_some(pid)
}

#[cfg(all(test, target_os = "windows"))]
fn close_windows_process_name(name: &str) -> Result<()> {
    let mut taskkill = helper_command("taskkill");
    taskkill.args(["/IM", name, "/T", "/F"]);
    run_close_helper_command_with_timeout(
        &mut taskkill,
        TOOL_HELPER_COMMAND_TIMEOUT,
        &format!("taskkill {name}"),
        &[128],
    )?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn sh_quote_for_applescript(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn push_candidate(
    candidates: &mut Vec<ToolProgramCandidate>,
    _tool: &str,
    path: String,
    label: &str,
    kind: String,
    selected: bool,
) {
    if path.trim().is_empty()
        || candidates
            .iter()
            .any(|candidate| tool_program_candidate_paths_equal(&candidate.path, &path))
    {
        return;
    }
    let exists = Path::new(&path).exists();
    let modified_at_unix = tool_program_modified_at_unix(&path);
    let version = exists.then(|| tool_program_candidate_version(Path::new(&path), &kind)).flatten();
    candidates.push(ToolProgramCandidate {
        path,
        label: label.to_string(),
        kind,
        edition: None,
        exists,
        modified_at_unix,
        version,
        selected,
        launchable: false,
        recommended: false,
        status: String::new(),
        reason: String::new(),
    });
}

fn tool_program_candidate_paths_equal(left: &str, right: &str) -> bool {
    #[cfg(windows)]
    {
        let separator = std::path::MAIN_SEPARATOR.to_string();
        return left.replace('/', &separator).to_lowercase()
            == right.replace('/', &separator).to_lowercase();
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

fn tool_program_modified_at_unix(path: &str) -> u64 {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn annotate_tool_program_candidate(tool: &str, candidate: &mut ToolProgramCandidate) {
    if tool == "hermes" && is_hermes_setup_app_path(Path::new(&candidate.path)) {
        candidate.label = "Hermes installer".to_string();
    } else if tool == "hermes" && is_hermes_desktop_app_path(Path::new(&candidate.path)) {
        candidate.label = "Hermes Desktop".to_string();
    }
    candidate.launchable = is_launchable_tool_candidate(tool, candidate);
    candidate.edition = tool_program_candidate_edition(tool, candidate).map(str::to_string);
    candidate.recommended =
        candidate.launchable && is_recommended_tool_program_candidate(tool, candidate);
    candidate.status = if !candidate.exists {
        "Missing".to_string()
    } else if tool == "hermes" && is_hermes_setup_app_path(Path::new(&candidate.path)) {
        "Installer".to_string()
    } else if candidate.selected && !candidate.launchable {
        "Selected but not launchable".to_string()
    } else if candidate.selected {
        "Selected".to_string()
    } else if candidate.recommended {
        "Recommended".to_string()
    } else if candidate.launchable {
        "Launchable".to_string()
    } else {
        "Not launchable".to_string()
    };
    candidate.reason = tool_program_candidate_reason(tool, candidate);
}

fn tool_program_candidate_edition(
    tool: &str,
    candidate: &ToolProgramCandidate,
) -> Option<&'static str> {
    if !is_tool_owned_program_path(tool, Path::new(&candidate.path))
        || (tool == "hermes" && is_hermes_setup_app_path(Path::new(&candidate.path)))
    {
        return None;
    }
    if is_desktop_program_candidate(candidate) {
        return Some("desktop");
    }
    // A command shim such as `code` can open a desktop app; file extensions
    // alone do not identify the edition. Reuse the actual launch policy.
    match tool_profile(tool)?.command_launch {
        ToolCommandLaunch::Desktop => Some("desktop"),
        ToolCommandLaunch::Terminal => Some("cli"),
        ToolCommandLaunch::Disabled => None,
    }
}

fn is_recommended_tool_program_candidate(tool: &str, candidate: &ToolProgramCandidate) -> bool {
    if !is_launchable_tool_candidate(tool, candidate) {
        return false;
    }
    if tool == "hermes" {
        return is_hermes_desktop_app_path(Path::new(&candidate.path))
            || (candidate.kind == "windows_exe"
                && candidate
                    .path
                    .rsplit(['/', '\\'])
                    .next()
                    .is_some_and(|value| value.eq_ignore_ascii_case("Hermes.exe")));
    }
    match tool {
        "claude-desktop" => matches!(
            candidate.kind.as_str(),
            "windows_appx" | "mac_app"
        ),
        "codex" => {
            is_chatgpt_desktop_path(Path::new(&candidate.path))
                && matches!(
                    candidate.kind.as_str(),
                    "windows_appx" | "mac_app" | "windows_exe"
                )
        }
        tool if tool_prefers_desktop_program(tool) => is_desktop_program_candidate(candidate),
        "claude-science" => {
            matches!(candidate.kind.as_str(), "command" | "windows_exe" | "mac_app")
        }
        tool if tool_profile(tool).is_some() => candidate.kind == "command",
        _ => candidate.selected && candidate.launchable,
    }
}

fn tool_program_candidate_reason(tool: &str, candidate: &ToolProgramCandidate) -> String {
    let path = Path::new(&candidate.path);
    if !candidate.exists {
        return "This path does not exist.".to_string();
    }
    if tool == "codex" && is_codex_plus_plus_path(path) {
        return "Codex++ takes over and overwrites the official Codex configuration.".to_string();
    }
    if !is_tool_owned_program_path(tool, path) {
        return "This program is not in the tool's executable allowlist. Select the tool's own EXE, app, or command."
            .to_string();
    }
    if tool == "codex" && is_chatgpt_desktop_path(path) {
        return "Current ChatGPT desktop app; preferred for automatic selection.".to_string();
    }
    if tool == "codex" && is_legacy_codex_desktop_path(path) {
        return "Legacy Codex desktop app; used when ChatGPT is unavailable.".to_string();
    }
    if matches!(tool, "claude-desktop" | "codex") && candidate.kind == "windows_appx" {
        return "Registered MSIX app; launches through AppsFolder and follows installed updates."
            .to_string();
    }
    if matches!(tool, "claude-desktop" | "codex") && candidate.kind == "windows_exe" {
        return "Traditional desktop executable; available for explicit selection. Automatic selection prefers official MSIX."
            .to_string();
    }
    if tool == "hermes" && is_hermes_setup_app_path(path) {
        return "Hermes installer; launchable, but installation or updates may delay a restart."
            .to_string();
    }
    if tool == "hermes" && is_hermes_desktop_app_path(path) {
        return "Installed Hermes Desktop; ready to configure and restart.".to_string();
    }
    if candidate.launchable {
        return "Can start automatically.".to_string();
    }
    "Not suitable for automatic launch.".to_string()
}

fn tool_program_candidate_version(path: &Path, kind: &str) -> Option<String> {
    let version = match kind {
        "command" => npm_command_package_version(path),
        #[cfg(target_os = "windows")]
        "windows_appx" => windows_appx_package_dir(path)?
            .file_name()?
            .to_str()?
            .split('_')
            .nth(1)
            .map(str::to_string),
        #[cfg(target_os = "macos")]
        "mac_app" => {
            let app = path.ancestors().find(|ancestor| {
                ancestor.extension().is_some_and(|extension| extension == "app")
            })?;
            macos_info_plist_string(app, "CFBundleShortVersionString")
        }
        _ => None,
    }?;
    compare_tool_program_versions(&version, &version).map(|_| version)
}

fn npm_command_package_version(path: &Path) -> Option<String> {
    let package_root = fs::canonicalize(path)
        .ok()
        .and_then(|entry| node_package_root_from_entry(&entry))
        .or_else(|| npm_shim_package_root(path))?;
    let manifest = package_root.join("package.json");
    if fs::metadata(&manifest).ok()?.len() > 256 * 1024 {
        return None;
    }
    let raw = fs::read_to_string(manifest).ok()?;
    let package = serde_json::from_str::<serde_json::Value>(&raw).ok()?;
    let version = package
        .get("version")?
        .as_str()?;
    compare_tool_program_versions(version, version).map(|_| version.to_string())
}

fn node_package_root_from_entry(entry: &Path) -> Option<PathBuf> {
    for ancestor in entry.ancestors() {
        if ancestor.file_name().is_none_or(|name| name != "node_modules") {
            continue;
        }
        let mut relative = entry.strip_prefix(ancestor).ok()?.components();
        let first = relative.next()?.as_os_str().to_str()?;
        if first == ".bin" {
            continue;
        }
        let package = if first.starts_with('@') {
            ancestor.join(first).join(relative.next()?.as_os_str())
        } else {
            ancestor.join(first)
        };
        if package.join("package.json").is_file() {
            return Some(package);
        }
    }
    None
}

fn npm_shim_package_root(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    if fs::metadata(path).ok()?.len() > 256 * 1024 {
        return None;
    }
    let raw = fs::read_to_string(path).ok()?.replace('\\', "/");
    let mut sources = vec![("/node_modules/", parent.join("node_modules"))];
    if parent.file_name().is_some_and(|name| name == ".bin") {
        sources.push(("/../", parent.join("..")));
    }
    for (marker, node_modules) in sources {
        for (offset, _) in raw.match_indices(marker) {
            let mut segments = raw[offset + marker.len()..].split('/');
            let Some(first) = segments.next().filter(|part| valid_node_package_name(part)) else {
                continue;
            };
            let package = if first.starts_with('@') {
                let Some(second) = segments.next().filter(|part| valid_node_package_name(part)) else {
                    continue;
                };
                node_modules.join(first).join(second)
            } else {
                node_modules.join(first)
            };
            if package.join("package.json").is_file() {
                return Some(package);
            }
        }
    }
    None
}

fn valid_node_package_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '@' | '-' | '_' | '.'))
}

fn path_candidate_kind(path: &str) -> String {
    if cfg!(target_os = "macos") && path.ends_with(".app") {
        "mac_app".to_string()
    } else if cfg!(target_os = "windows") && windows_appx_executable_path(Path::new(path)) {
        "windows_appx".to_string()
    } else if cfg!(target_os = "windows") && path.to_ascii_lowercase().ends_with(".exe") {
        "windows_exe".to_string()
    } else {
        "path".to_string()
    }
}

fn is_linux_desktop_program_path(tool: &str, path: &str) -> bool {
    let Some(profile) = tool_profile(tool).filter(|profile| profile.desktop_preferred) else {
        return false;
    };
    let identity = normalized_tool_program_identity(path);
    path.ends_with(".AppImage")
        || (profile
            .runtime
            .linux_process_names
            .iter()
            .any(|name| normalized_tool_program_identity(name) == identity)
            && (profile.command_launch == ToolCommandLaunch::Desktop
                || !profile
                    .runtime
                    .commands
                    .iter()
                    .any(|name| normalized_tool_program_identity(name) == identity)))
}

fn tool_path_candidate_kind(tool: &str, path: &str) -> String {
    // Running/discovered native CLIs need the same terminal classification as
    // manually located ones; an .exe suffix alone does not make one a desktop app.
    selected_path_kind(tool, path)
}

fn selected_path_kind(tool: &str, path: &str) -> String {
    if cfg!(target_os = "linux") && is_linux_desktop_program_path(tool, path) {
        return "linux_desktop".to_string();
    }
    let identity = normalized_tool_program_identity(path);
    // Some tools share a config but ship separate desktop/CLI executables.
    // A manually located CLI outside PATH must still open in a terminal. Do
    // not apply this inference to tools whose desktop and CLI share a name,
    // or to a macOS app bundle (Cline.app and `cline` are distinct entries).
    let distinct_cli = !path.ends_with(".app")
        && tool_profile(tool).is_some_and(|profile| {
            profile.command_launch == ToolCommandLaunch::Terminal
                && !profile.windows_program_paths.is_empty()
                && profile.windows_program_paths.iter().all(|(_, executable)| {
                    normalized_tool_program_identity(executable) != identity
                })
        });
    if (!tool_prefers_desktop_program(tool) || distinct_cli)
        && tool_program_commands(tool)
            .iter()
            .any(|command| normalized_tool_program_identity(command) == identity)
    {
        // An explicitly located CLI outside PATH still needs an interactive
        // terminal; a bare executable spawn cannot present its prompt.
        "command".to_string()
    } else if tool_program_commands(tool).iter().any(|command| {
        find_tool_command_paths_without_shell(tool, command)
            .iter()
            .any(|candidate| candidate.display().to_string() == path)
    }) {
        "command".to_string()
    } else {
        path_candidate_kind(path)
    }
}

fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if path.as_os_str().is_empty() || paths.iter().any(|existing| existing == &path) {
        return;
    }
    paths.push(path);
}

#[cfg(target_os = "windows")]
fn refreshed_saved_windows_appx_path(
    tool: &str,
    selected_path: &str,
    known_paths: &[PathBuf],
) -> Option<String> {
    if !matches!(tool, "claude-desktop" | "codex")
        || selected_path.trim().is_empty()
        || !windows_appx_executable_path(Path::new(selected_path))
    {
        return None;
    }
    let launchable = |path: &&PathBuf| {
        path.exists()
            && windows_appx_executable_path(path)
            && is_launchable_tool_candidate(
                tool,
                &ToolProgramCandidate {
                    path: path.display().to_string(),
                    kind: "windows_appx".to_string(),
                    exists: true,
                    ..Default::default()
                },
            )
    };
    let current = if tool == "codex" {
        known_paths
            .iter()
            .filter(launchable)
            .find(|path| is_chatgpt_desktop_path(path))
            .or_else(|| known_paths.iter().filter(launchable).next())?
    } else {
        known_paths.iter().find(launchable)?
    };
    let current = current.display().to_string();
    (!tool_program_candidate_paths_equal(selected_path, &current)).then_some(current)
}

#[cfg(not(target_os = "windows"))]
fn refreshed_saved_windows_appx_path(
    _tool: &str,
    _selected_path: &str,
    _known_paths: &[PathBuf],
) -> Option<String> {
    None
}

fn extend_existing_versioned_bin_dirs(paths: &mut Vec<PathBuf>, base: PathBuf) {
    if !base.exists() {
        return;
    }
    if let Ok(entries) = fs::read_dir(base) {
        for entry in entries.flatten() {
            let bin = entry.path().join("bin");
            if bin.exists() {
                push_unique_path(paths, bin);
            }
        }
    }
}

fn command_search_dirs(home: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if !home.as_os_str().is_empty() {
        push_unique_path(&mut paths, home.join(".local/bin"));
        push_unique_path(&mut paths, home.join(".npm-global/bin"));
        push_unique_path(&mut paths, home.join("n/bin"));
        push_unique_path(&mut paths, home.join(".volta/bin"));
        push_unique_path(&mut paths, home.join(".local/share/mise/shims"));
        push_unique_path(&mut paths, home.join(".local/share/pnpm"));
        push_unique_path(&mut paths, home.join("Library/pnpm"));
        push_unique_path(&mut paths, home.join(".bun/bin"));
        push_unique_path(&mut paths, home.join(".opencode/bin"));
        push_unique_path(&mut paths, home.join(".hermes/node/bin"));
        push_unique_path(&mut paths, home.join(".kimi-code/bin"));
        push_unique_path(&mut paths, home.join(".mimocode/bin"));
        push_unique_path(&mut paths, home.join(".grok/bin"));
        push_unique_path(&mut paths, home.join(".minimax-code/bin"));
        push_unique_path(&mut paths, home.join(".minimax-code"));
        push_unique_path(&mut paths, home.join(".reasonix/bin"));
        push_unique_path(&mut paths, home.join(".raven/bin"));
        push_unique_path(&mut paths, home.join(".cline/bin"));
        extend_existing_versioned_bin_dirs(&mut paths, home.join(".nvm/versions/node"));
        extend_existing_versioned_bin_dirs(&mut paths, home.join(".local/state/fnm_multishells"));
    }

    if !test_home_active() {
        if let Some(root) = nonempty_env_path("MCODE_INSTALL_DIR").map(expand_tool_home_path) {
            push_unique_path(&mut paths, root.join("bin"));
            push_unique_path(&mut paths, root);
        }
    }

    #[cfg(target_os = "macos")]
    {
        push_unique_path(&mut paths, PathBuf::from("/opt/homebrew/bin"));
        push_unique_path(&mut paths, PathBuf::from("/usr/local/bin"));
    }

    #[cfg(target_os = "linux")]
    {
        push_unique_path(&mut paths, PathBuf::from("/usr/local/bin"));
        push_unique_path(&mut paths, PathBuf::from("/usr/bin"));
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) {
            push_unique_path(&mut paths, appdata.join("npm"));
            push_unique_path(&mut paths, appdata.join("Python/Scripts"));
        }
        if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
            push_unique_path(&mut paths, local_app_data.join("pnpm"));
            push_unique_path(&mut paths, local_app_data.join("Volta/bin"));
            push_unique_path(&mut paths, local_app_data.join("Microsoft/WinGet/Links"));
            for python in ["Python313", "Python312", "Python311", "Python310"] {
                push_unique_path(
                    &mut paths,
                    local_app_data
                        .join("Programs")
                        .join("Python")
                        .join(python)
                        .join("Scripts"),
                );
            }
        }
        push_unique_path(&mut paths, home.join("scoop/shims"));
        push_unique_path(&mut paths, PathBuf::from("C:\\Program Files\\nodejs"));
    }

    if let Some(path_env) = std::env::var_os("PATH") {
        for path in std::env::split_paths(&path_env) {
            push_unique_path(&mut paths, path);
        }
    }
    paths
}

#[cfg(not(target_os = "windows"))]
fn first_abs_path_line(raw: &str) -> Option<&str> {
    raw.lines()
        .map(str::trim)
        .find(|line| line.starts_with('/'))
}

#[cfg(not(target_os = "windows"))]
const SHELL_COMMAND_LOOKUP_FLAG: &str = "-lc";

#[cfg(not(target_os = "windows"))]
fn shell_command_path(command: &str) -> Option<PathBuf> {
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "sh".to_string());
    let mut helper = helper_command(shell);
    // An interactive shell enables job control. The helper runs in its own
    // process group, so Unix can stop it as a background TTY job and leave
    // discovery waiting for the full timeout. A login shell still loads the
    // PATH initialization needed for command lookup without enabling job control.
    helper
        .arg(SHELL_COMMAND_LOOKUP_FLAG)
        .arg(format!("command -v {command}"));
    let output = run_helper_command_with_timeout(
        &mut helper,
        TOOL_HELPER_COMMAND_TIMEOUT,
        &format!("command -v {command}"),
    )
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    first_abs_path_line(&raw).map(PathBuf::from)
}

#[cfg(target_os = "windows")]
fn shell_command_path(command: &str) -> Option<PathBuf> {
    let mut helper = helper_command("where");
    helper.arg(command);
    let output = run_helper_command_with_timeout(
        &mut helper,
        TOOL_HELPER_COMMAND_TIMEOUT,
        &format!("where {command}"),
    )
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    raw.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(PathBuf::from)
}

#[cfg(target_os = "windows")]
fn windows_command_launch_candidates(path: &Path, command: &str) -> Vec<PathBuf> {
    if path.extension().is_some() {
        return vec![path.to_path_buf()];
    }

    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(command);
    let Some(parent) = path.parent() else {
        return vec![path.to_path_buf()];
    };
    [".cmd", ".exe", ".ps1", ""]
        .iter()
        .map(|suffix| parent.join(format!("{name}{suffix}")))
        .collect()
}

#[cfg(target_os = "windows")]
fn windows_appx_install_locations(package_names: &[&str]) -> Vec<PathBuf> {
    let mut locations = Vec::new();
    let mut seen = HashSet::new();

    for package_name in package_names {
        let escaped = package_name.replace('\'', "''");
        let script = format!(
            "Get-AppxPackage -Name '{}' -ErrorAction SilentlyContinue | \
             Sort-Object Version -Descending | \
             ForEach-Object {{ $_.InstallLocation }}",
            escaped
        );
        let mut command = helper_command("powershell.exe");
        command
            .args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(script);
        let Ok(output) = run_helper_command_with_timeout(
            &mut command,
            TOOL_DISCOVERY_HELPER_TIMEOUT,
            &format!("Get-AppxPackage {package_name}"),
        ) else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        let raw = String::from_utf8_lossy(&output.stdout);
        for line in raw.lines().map(str::trim).filter(|line| !line.is_empty()) {
            let path = PathBuf::from(line);
            if seen.insert(path.clone()) {
                locations.push(path);
            }
        }
    }

    locations
}

#[cfg(target_os = "windows")]
fn windows_appx_executable_candidates(
    package_names: &[&str],
    relative_exes: &[&str],
) -> Vec<PathBuf> {
    windows_appx_executable_candidates_from_locations(
        windows_appx_install_locations(package_names),
        relative_exes,
    )
}

#[cfg(target_os = "windows")]
fn windows_appx_protocol_executable_candidates(
    package_names: &[&str],
    protocol: &str,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for install_location in windows_appx_install_locations(package_names) {
        let Ok(manifest) = fs::read_to_string(install_location.join("AppxManifest.xml")) else {
            continue;
        };
        let Some(relative_executable) =
            appx_manifest_protocol_executable(&manifest, protocol)
        else {
            continue;
        };
        push_unique_path(&mut paths, install_location.join(relative_executable));
    }
    paths
}

#[cfg(target_os = "windows")]
fn windows_appx_executable_candidates_from_locations(
    install_locations: Vec<PathBuf>,
    relative_exes: &[&str],
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for install_location in install_locations {
        for relative_exe in relative_exes {
            push_unique_path(&mut paths, install_location.join(relative_exe));
        }
    }
    paths
}

#[cfg(target_os = "windows")]
fn windows_appx_executable_path(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|value| value.eq_ignore_ascii_case("exe"))
        .unwrap_or(false)
        && windows_appx_package_dir(path).is_some()
}

#[cfg(not(target_os = "windows"))]
fn windows_appx_executable_path(_path: &Path) -> bool {
    false
}

#[cfg(target_os = "windows")]
fn windows_appx_package_dir(path: &Path) -> Option<PathBuf> {
    path.ancestors().find_map(|ancestor| {
        let parent = ancestor.parent()?;
        let parent_name = parent.file_name()?.to_str()?;
        if parent_name.eq_ignore_ascii_case("WindowsApps") {
            Some(ancestor.to_path_buf())
        } else {
            None
        }
    })
}

#[cfg(target_os = "windows")]
fn windows_appx_package_family_name(package_dir: &Path) -> Option<String> {
    let package_dir_text = package_dir.display().to_string().replace('\'', "''");
    let script = format!(
        "$target = '{}'; Get-AppxPackage -ErrorAction SilentlyContinue | \
         Where-Object {{ $_.InstallLocation -eq $target }} | \
         Select-Object -First 1 -ExpandProperty PackageFamilyName",
        package_dir_text
    );
    let mut command = helper_command("powershell.exe");
    command
        .args(["-NoProfile", "-NonInteractive", "-Command"])
        .arg(script);
    let output = run_helper_command_with_timeout(
        &mut command,
        TOOL_DISCOVERY_HELPER_TIMEOUT,
        "Get-AppxPackage family name",
    )
    .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

#[cfg(target_os = "windows")]
fn windows_appx_manifest_application_id(package_dir: &Path, executable: &Path) -> Option<String> {
    let manifest = fs::read_to_string(package_dir.join("AppxManifest.xml")).ok()?;
    let relative_executable = executable.strip_prefix(package_dir).ok()?;
    let relative_executable = relative_executable.display().to_string();
    appx_manifest_application_id_for_executable(&manifest, &relative_executable)
}

#[cfg(target_os = "windows")]
fn appx_manifest_application_id_for_executable(manifest: &str, executable: &str) -> Option<String> {
    let wanted = normalize_windows_relative_path(executable);
    let mut rest = manifest;
    let mut first_id = None;

    while let Some(start) = rest.find("<Application") {
        rest = &rest[start..];
        let Some(end) = rest.find('>') else {
            break;
        };
        let tag = &rest[..=end];
        let id = xml_attribute(tag, "Id");
        if first_id.is_none() {
            first_id = id.clone();
        }
        let executable_attr =
            xml_attribute(tag, "Executable").map(|value| normalize_windows_relative_path(&value));
        if executable_attr.as_deref() == Some(&wanted) {
            return id;
        }
        rest = &rest[end + 1..];
    }

    first_id
}

#[cfg(target_os = "windows")]
fn appx_manifest_protocol_executable(manifest: &str, protocol: &str) -> Option<String> {
    let mut rest = manifest;
    while let Some(start) = rest.find("<Application ") {
        rest = &rest[start..];
        let opening_end = rest.find('>')?;
        let opening_tag = &rest[..=opening_end];
        if opening_tag.trim_end_matches('>').trim_end().ends_with('/') {
            rest = &rest[opening_end + 1..];
            continue;
        }
        let application_end = rest[opening_end + 1..].find("</Application>")?
            + opening_end
            + 1;
        let application = &rest[opening_end + 1..application_end];
        let declares_windows_protocol = application
            .split('<')
            .filter_map(|fragment| fragment.split_once('>').map(|(tag, _)| tag))
            .any(|tag| {
                tag.split_whitespace()
                    .next()
                    .and_then(|name| name.rsplit(':').next())
                    .is_some_and(|name| name == "Protocol")
                    && xml_attribute(tag, "Name")
                        .is_some_and(|name| name.eq_ignore_ascii_case(protocol))
            });
        if application.contains("Category=\"windows.protocol\"") && declares_windows_protocol {
            if let Some(executable) = xml_attribute(opening_tag, "Executable")
                .filter(|value| !value.trim().is_empty())
            {
                return Some(executable);
            }
        }
        rest = &rest[application_end + "</Application>".len()..];
    }
    None
}

#[cfg(target_os = "windows")]
fn xml_attribute(tag: &str, name: &str) -> Option<String> {
    let pattern = format!("{name}=\"");
    let start = tag.find(&pattern)? + pattern.len();
    let value = &tag[start..];
    let end = value.find('"')?;
    Some(value[..end].to_string())
}

#[cfg(target_os = "windows")]
fn normalize_windows_relative_path(value: &str) -> String {
    value
        .replace('/', "\\")
        .trim_start_matches('\\')
        .to_ascii_lowercase()
}

#[cfg(target_os = "windows")]
fn windows_appx_shell_target_for_executable(path: &Path) -> Option<String> {
    let package_dir = windows_appx_package_dir(path)?;
    let family_name = windows_appx_package_family_name(&package_dir)?;
    let app_id = windows_appx_manifest_application_id(&package_dir, path)?;
    Some(format!("shell:AppsFolder\\{family_name}!{app_id}"))
}

#[cfg(target_os = "windows")]
fn launch_windows_appx_executable(
    path: &Path,
    launch_context: Option<&ToolConfigLaunchContext<'_>>,
) -> Result<()> {
    let target = windows_appx_shell_target_for_executable(path)
        .ok_or_else(|| anyhow!("cannot resolve AppX launch target for {}", path.display()))?;
    let mut command = Command::new("explorer.exe");
    command.arg(&target);
    spawn_launch_command(&mut command, launch_context, &target)?;
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn launch_windows_appx_executable(
    path: &Path,
    launch_context: Option<&ToolConfigLaunchContext<'_>>,
) -> Result<()> {
    let mut command = Command::new(path);
    spawn_launch_command(&mut command, launch_context, &path.display().to_string())?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn tool_specific_command_paths(tool: &str, command: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if tool == "codex" && command == "codex" {
        let home = home_dir();
        for app_dir in [PathBuf::from("/Applications"), home.join("Applications")] {
            for app_name in ["ChatGPT.app", "Codex.app"] {
                paths.push(app_dir.join(app_name).join("Contents/Resources/codex"));
                paths.push(app_dir.join(app_name).join("Contents/MacOS").join(
                    if app_name == "ChatGPT.app" {
                        "ChatGPT"
                    } else {
                        "Codex"
                    },
                ));
            }
        }
    }
    if tool == "deepseek-harness" && command == "dsh" {
        paths.extend(deepseek_harness_npx_cached_command_paths());
    }
    paths
}

#[cfg(target_os = "windows")]
fn tool_specific_command_paths(tool: &str, command: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if tool == "openclaw" && command == "openclaw" {
        if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
            paths.push(local_app_data.join("OpenClaw/bin/openclaw.exe"));
        }
    }
    if tool == "deepseek-harness" && command == "dsh" {
        paths.extend(deepseek_harness_npx_cached_command_paths());
    }
    paths
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn tool_specific_command_paths(tool: &str, command: &str) -> Vec<PathBuf> {
    if tool == "deepseek-harness" && command == "dsh" {
        return deepseek_harness_npx_cached_command_paths();
    }
    Vec::new()
}

fn deepseek_harness_npx_cached_command_paths() -> Vec<PathBuf> {
    let mut cache_roots = Vec::new();
    push_unique_path(&mut cache_roots, home_dir().join(".npm"));
    if !test_home_active() {
        for name in ["npm_config_cache", "NPM_CONFIG_CACHE"] {
            if let Some(path) = std::env::var_os(name).filter(|value| !value.is_empty()) {
                push_unique_path(&mut cache_roots, PathBuf::from(path));
            }
        }
        #[cfg(target_os = "windows")]
        if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
            push_unique_path(&mut cache_roots, local_app_data.join("npm-cache"));
        }
    }

    let mut paths = Vec::new();
    for cache_root in cache_roots {
        let Ok(entries) = fs::read_dir(cache_root.join("_npx")) else {
            continue;
        };
        let mut package_bins = entries
            .flatten()
            .map(|entry| entry.path().join("node_modules").join(".bin"))
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        package_bins.sort();
        for bin in package_bins {
            #[cfg(target_os = "windows")]
            for name in ["dsh.cmd", "dsh.exe", "dsh.ps1", "dsh"] {
                push_unique_path(&mut paths, bin.join(name));
            }
            #[cfg(not(target_os = "windows"))]
            push_unique_path(&mut paths, bin.join("dsh"));
        }
    }
    paths
}

fn find_tool_command_paths(tool: &str, command: &str) -> Vec<PathBuf> {
    find_tool_command_paths_inner(tool, command, true)
}

fn find_tool_command_paths_without_shell(tool: &str, command: &str) -> Vec<PathBuf> {
    find_tool_command_paths_inner(tool, command, false)
}

fn find_tool_command_paths_inner(
    tool: &str,
    command: &str,
    include_login_shell: bool,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    let mut push_existing = |path: PathBuf| {
        if !path.exists() {
            return false;
        }
        let key = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if !seen.insert(key) {
            return false;
        }
        paths.push(path);
        true
    };
    let mut deterministic_path_found = false;

    for path in tool_specific_command_paths(tool, command) {
        deterministic_path_found |= push_existing(path);
    }

    for dir in command_search_dirs(&home_dir()) {
        #[cfg(target_os = "windows")]
        {
            deterministic_path_found |= push_existing(dir.join(format!("{command}.cmd")));
            deterministic_path_found |= push_existing(dir.join(format!("{command}.exe")));
            deterministic_path_found |= push_existing(dir.join(format!("{command}.ps1")));
            deterministic_path_found |= push_existing(dir.join(command));
        }
        #[cfg(not(target_os = "windows"))]
        {
            deterministic_path_found |= push_existing(dir.join(command));
        }
    }

    // Deterministic directories cover the supported package managers and avoid
    // launching a shell (or where.exe on Windows) in the common case. Keep the
    // platform lookup only as a fallback for custom PATH initialization.
    if include_login_shell && !deterministic_path_found {
        if let Some(path) = shell_command_path(command) {
            #[cfg(target_os = "windows")]
            for candidate in windows_command_launch_candidates(&path, command) {
                push_existing(candidate);
            }
            #[cfg(not(target_os = "windows"))]
            push_existing(path);
        }
    }

    paths
}

fn tool_program_commands(tool: &str) -> &'static [&'static str] {
    tool_profile(tool)
        .filter(|profile| profile.command_launch != ToolCommandLaunch::Disabled)
        .map(|profile| profile.runtime.commands)
        .unwrap_or_default()
}

fn known_tool_program_paths(tool: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    #[cfg(target_os = "linux")]
    if let Some(profile) = tool_profile(tool) {
        for command in profile.runtime.linux_process_names {
            if is_linux_desktop_program_path(tool, command) {
                // Desktop binaries are launched directly, not as interactive CLI tools.
                paths.extend(find_tool_command_paths_without_shell(tool, command));
            }
        }
    }
    #[cfg(target_os = "linux")]
    if tool == "anythingllm" {
        // The official Linux installer saves this AppImage in $HOME unless
        // ANYTHING_LLM_INSTALL_DIR was supplied. No installer is run here.
        let root = if test_home_active() {
            home_dir()
        } else {
            nonempty_env_path("ANYTHING_LLM_INSTALL_DIR").unwrap_or_else(home_dir)
        };
        paths.push(root.join("AnythingLLMDesktop.AppImage"));
    }
    #[cfg(target_os = "macos")]
    {
        let home = home_dir();
        if tool == "hermes" {
            for path in hermes_desktop_executable_candidates() {
                push_unique_path(&mut paths, path);
            }
        }
        let app_dirs = [PathBuf::from("/Applications"), home.join("Applications")];
        let app_names = tool_profile(tool)
            .map(|profile| profile.macos_app_names)
            .unwrap_or_default();
        for dir in app_dirs {
            for app_name in app_names {
                push_unique_path(&mut paths, dir.join(app_name));
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        if tool == "hermes" {
            paths.extend(hermes_desktop_executable_candidates());
        }
        // Store packages use versioned installation directories. Discover them
        // first so automatic selection resolves the newest registered package;
        // an explicit non-MSIX user choice is still honored by the selector.
        if tool == "claude-desktop" {
            paths.extend(windows_appx_executable_candidates(
                &["Claude"],
                &["app\\claude.exe", "claude.exe"],
            ));
        }
        if tool == "codex" {
            // OpenAI keeps the OpenAI.Codex package identity and codex protocol stable across
            // Codex- and ChatGPT-branded desktop builds. Resolve the package-declared executable
            // first so both app\Codex.exe and app\ChatGPT.exe remain compatible without relying
            // on branding-specific paths.
            paths.extend(windows_appx_protocol_executable_candidates(
                &["OpenAI.Codex"],
                "codex",
            ));
            paths.extend(windows_appx_executable_candidates(
                &["OpenAI.Codex", "ChatGPT", "OpenAI.ChatGPT"],
                &["app\\chatgpt.exe", "chatgpt.exe", "app\\codex.exe"],
            ));
        }
        let local_app_data = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
        let program_files = std::env::var_os("ProgramFiles").map(PathBuf::from);
        let program_files_x86 = std::env::var_os("ProgramFiles(x86)").map(PathBuf::from);
        let mut base_dirs = Vec::new();
        if let Some(path) = local_app_data {
            base_dirs.push(path.join("Programs"));
            base_dirs.push(path);
        }
        if let Some(path) = program_files {
            base_dirs.push(path);
        }
        if let Some(path) = program_files_x86 {
            base_dirs.push(path);
        }
        let exe_paths = tool_profile(tool)
            .map(|profile| profile.windows_program_paths)
            .unwrap_or_default();
        for base_dir in base_dirs {
            for (dir, exe) in exe_paths {
                paths.push(base_dir.join(dir).join(exe));
            }
        }
        if !test_home_active() {
            for path in windows_installed_program_paths(tool) {
                push_unique_path(&mut paths, path);
            }
        }
    }
    paths
}

#[cfg(test)]
pub(crate) fn hermes_const_api_command() -> &'static str {
    if cfg!(any(target_os = "windows", target_os = "macos")) {
        "Restart Hermes Desktop"
    } else {
        "Open Hermes Desktop"
    }
}

#[cfg(test)]
pub(crate) fn hermes_desktop_launch_uri() -> &'static str {
    "hermes://const-api"
}

#[cfg(target_os = "windows")]
pub(crate) fn hermes_desktop_executable_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        candidates.push(
            local_app_data
                .join("hermes")
                .join("hermes-agent")
                .join("apps")
                .join("desktop")
                .join("release")
                .join("win-unpacked")
                .join("Hermes.exe"),
        );
        candidates.push(
            local_app_data
                .join("Programs")
                .join("Hermes")
                .join("Hermes.exe"),
        );
        candidates.push(
            local_app_data
                .join("Programs")
                .join("hermes")
                .join("Hermes.exe"),
        );
    }
    candidates
}

#[cfg(target_os = "macos")]
pub(crate) fn hermes_desktop_executable_candidates() -> Vec<PathBuf> {
    let release_dir = home_dir()
        .join(".hermes")
        .join("hermes-agent")
        .join("apps")
        .join("desktop")
        .join("release");
    [
        release_dir.join("mac-arm64").join("Hermes.app"),
        release_dir.join("mac-x64").join("Hermes.app"),
        release_dir.join("mac").join("Hermes.app"),
    ]
    .into_iter()
    .collect()
}
