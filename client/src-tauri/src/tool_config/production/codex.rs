const CODEX_RETIRED_CONFIG_PATHS: &[&[&str]] =
    &[&["model_reasoning_effort"], &["disable_response_storage"]];

pub(crate) fn upsert_codex_toml(raw: &str, base_url: &str, api_key: &str) -> Result<String> {
    let mut doc = if raw.trim().is_empty() {
        DocumentMut::new()
    } else {
        raw.parse::<DocumentMut>()
            .with_context(|| "parse Codex config.toml")?
    };

    doc["model_provider"] = toml_value(CODEX_CONST_API_PROVIDER_ID);
    doc["experimental_realtime_ws_base_url"] = toml_value(base_url);
    doc["experimental_realtime_webrtc_call_base_url"] = toml_value(base_url);
    if doc.get("model").and_then(|item| item.as_str()) == Some("code-cheap") {
        doc.as_table_mut().remove("model");
    }

    if !doc.contains_key("model_providers") || !doc["model_providers"].is_table() {
        doc["model_providers"] = Item::Table(Table::new());
    }
    let providers = doc["model_providers"]
        .as_table_mut()
        .ok_or_else(|| anyhow!("Codex model_providers is not a table"))?;
    remove_legacy_codex_const_api_provider(providers, base_url, api_key);
    if !providers.contains_key(CODEX_CONST_API_PROVIDER_ID)
        || !providers[CODEX_CONST_API_PROVIDER_ID].is_table()
    {
        providers[CODEX_CONST_API_PROVIDER_ID] = Item::Table(Table::new());
    }
    let provider = providers[CODEX_CONST_API_PROVIDER_ID]
        .as_table_mut()
        .ok_or_else(|| anyhow!("Codex const_api provider is not a table"))?;
    provider["name"] = toml_value(CONST_API_DISPLAY_NAME);
    provider["base_url"] = toml_value(base_url);
    provider["wire_api"] = toml_value("responses");
    provider["supports_websockets"] = toml_value(true);
    provider["requires_openai_auth"] = toml_value(true);
    provider["experimental_bearer_token"] = toml_value(api_key);

    let mut text = doc.to_string();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    Ok(text)
}

fn remove_legacy_codex_const_api_provider(providers: &mut Table, base_url: &str, api_key: &str) {
    for provider_id in [CODEX_LEGACY_PROVIDER_ID, LEGACY_CONST_API_PROVIDER_ID] {
        let should_remove = providers
            .get(provider_id)
            .and_then(|provider| provider.as_table())
            .map(|provider| codex_provider_table_matches_const_api(provider, base_url, api_key))
            .unwrap_or(false);
        if should_remove {
            providers.remove(provider_id);
        }
    }
}

fn codex_provider_table_matches_const_api(provider: &Table, base_url: &str, api_key: &str) -> bool {
    provider
        .get("base_url")
        .and_then(|value| value.as_str())
        .map(|value| is_current_or_const_api_local_url(value, base_url))
        .unwrap_or(false)
        || provider
            .get("name")
            .and_then(|value| value.as_str())
            .map(|value| value == CONST_API_DISPLAY_NAME || value == CONST_API_LEGACY_DISPLAY_NAME)
            .unwrap_or(false)
        || provider
            .get("experimental_bearer_token")
            .and_then(|value| value.as_str())
            .map(|value| value == LOCAL_PLACEHOLDER_KEY || value == api_key)
            .unwrap_or(false)
}

fn codex_effective_provider_from_config_text(raw: &str) -> String {
    raw.parse::<DocumentMut>()
        .ok()
        .map(|doc| codex_effective_provider_from_doc(&doc))
        .unwrap_or_else(|| "openai".to_string())
}

fn codex_effective_provider_from_doc(doc: &DocumentMut) -> String {
    doc.get("model_provider")
        .and_then(|value| value.as_str())
        .map(normalize_codex_provider_id)
        .unwrap_or_else(|| "openai".to_string())
}

fn normalize_codex_provider_id(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        "openai".to_string()
    } else if trimmed == LEGACY_CONST_API_PROVIDER_ID {
        CODEX_CONST_API_PROVIDER_ID.to_string()
    } else {
        trimmed.to_string()
    }
}

pub(crate) fn upsert_codex_auth(
    raw: serde_json::Value,
    api_key: &str,
) -> Result<serde_json::Value> {
    let mut auth = raw;
    ensure_json_object(&mut auth);
    let account_status = codex_account_status_from_auth(&auth);
    let object = auth
        .as_object_mut()
        .ok_or_else(|| anyhow!("Codex auth document is not an object"))?;
    if account_status == "official" {
        object.remove("OPENAI_API_KEY");
    } else {
        object.insert(
            "OPENAI_API_KEY".to_string(),
            serde_json::Value::String(api_key.to_string()),
        );
    }
    Ok(auth)
}

pub(crate) fn upsert_env_vars(raw: &str, updates: &[(&str, &str)]) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for line in raw.lines() {
        let trimmed = line.trim_start();
        let trimmed = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        let key = trimmed
            .split_once('=')
            .map(|(key, _)| key.trim())
            .filter(|key| {
                !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            });
        if let Some(key) = key {
            if let Some((_, value)) = updates.iter().find(|(update_key, _)| *update_key == key) {
                lines.push(format!("{key}={value}"));
                seen.insert(key.to_string());
                continue;
            }
        }
        lines.push(line.to_string());
    }
    if !lines.is_empty() && !lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.push(String::new());
    }
    for (key, value) in updates {
        if !seen.contains(*key) {
            lines.push(format!("{key}={value}"));
        }
    }
    let mut text = lines.join("\n");
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

pub(crate) fn remove_env_vars_if_value(raw: &str, removals: &[(&str, &str)]) -> String {
    remove_env_vars_if(raw, |key, value| {
        removals
            .iter()
            .any(|(remove_key, remove_value)| key == *remove_key && value == *remove_value)
    })
}

pub(crate) fn remove_env_vars_if(raw: &str, should_remove: impl Fn(&str, &str) -> bool) -> String {
    let mut lines = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim_start();
        let trimmed = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        let should_remove = trimmed.split_once('=').is_some_and(|(key, value)| {
            let key = key.trim();
            let value = value.trim().trim_matches('"').trim_matches('\'');
            should_remove(key, value)
        });
        if !should_remove {
            lines.push(line.to_string());
        }
    }
    let mut text = lines.join("\n");
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

pub(crate) fn apply_codex_config(base_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    let mut ignore_progress = |_: &str, _: Option<u64>, _: Option<u64>, _: Option<f64>| {};
    apply_codex_config_with_progress(base_url, api_key, &mut ignore_progress)
}

pub(crate) fn apply_codex_config_with_progress(
    base_url: &str,
    api_key: &str,
    report_progress: &mut dyn FnMut(&'static str, Option<u64>, Option<u64>, Option<f64>),
) -> Result<ToolApplyResult> {
    // Legacy callers retain the selected directory mode; only the explicit
    // menu choice switches it off. No model discovery on the default path.
    apply_codex_config_with_catalog_and_progress(base_url, api_key, None, None, report_progress)
}

pub(crate) fn apply_codex_config_with_catalog_and_progress(
    base_url: &str,
    api_key: &str,
    model_source: Option<CodexModelSource>,
    catalog: Option<&serde_json::Value>,
    report_progress: &mut dyn FnMut(&'static str, Option<u64>, Option<u64>, Option<f64>),
) -> Result<ToolApplyResult> {
    let base_url = tool_surface_url(base_url, "v1");
    let dir = codex_home();
    let auth_path = dir.join("auth.json");
    let config_path = dir.join("config.toml");
    let mut result = ToolApplyBuilder::default();
    let current_config = restore_managed_toml_fields_for_reapply(
        "codex",
        &config_path,
        &read_text_or_empty(&config_path)?,
        CODEX_RETIRED_CONFIG_PATHS,
    )?;
    let current_config = if model_source == Some(CodexModelSource::Codex) {
        restore_managed_toml_fields_for_reapply(
            "codex", &config_path, &current_config, CODEX_CATALOG_CONFIG_PATHS,
        )?
    } else {
        current_config
    };
    let auth = upsert_codex_auth(
        read_json_or_default(&auth_path, serde_json::json!({}))?,
        api_key,
    )?;
    let mut config = upsert_codex_toml(&current_config, &base_url, api_key)?;
    if model_source == Some(CodexModelSource::Codex)
        && codex_model_source_from_config(&config) == CodexModelSource::Const {
        // Also permit disabling our path when a previous installation's
        // manifest is unavailable. Never remove a user's custom catalog path.
        let mut doc = config.parse::<DocumentMut>()?;
        doc.as_table_mut().remove("model_catalog_json");
        config = doc.to_string();
    }
    if model_source == Some(CodexModelSource::Const) {
        if !catalog.and_then(|value| value.get("models"))
            .and_then(serde_json::Value::as_array).is_some_and(|models| !models.is_empty()) {
            return Err(anyhow!("TOOL_CONFIG_MODELS_UNAVAILABLE: Codex"));
        }
        let mut doc = config.parse::<DocumentMut>()?;
        doc["model_catalog_json"] = toml_value(codex_managed_catalog_path().to_string_lossy().as_ref());
        // Codex includes this hosted tool even on an ordinary greeting. A
        // function-capable third-party model is not evidence of hosted search.
        doc["web_search"] = toml_value("disabled");
        config = doc.to_string();
    }
    let paths = [auth_path.clone(), config_path.clone(), codex_managed_catalog_path()];
    with_tool_config_file_transaction("codex", &paths, || {
        // Publish the complete directory before referencing it. Roll back the
        // config, directory and manifest together if any write fails.
        if let Some(catalog) = catalog.filter(|_| model_source == Some(CodexModelSource::Const)) {
            write_json_with_backup(&codex_managed_catalog_path(), catalog, "codex", &mut result)?;
        }
        write_json_with_backup(&auth_path, &auth, "codex", &mut result)?;
        write_text_with_backup(&config_path, &config, "codex", &mut result)?;
        verify_codex_config_written(&config_path, &base_url, api_key)?;
        release_managed_fields_after_reapply("codex", &config_path, CODEX_RETIRED_CONFIG_PATHS)?;
        if model_source == Some(CodexModelSource::Codex) {
            release_managed_fields_after_reapply("codex", &config_path, CODEX_CATALOG_CONFIG_PATHS)?;
        }
        Ok(())
    })?;
    let mut finished = result.finish("codex");
    finished.details = codex_status_details(&auth, &config);
    let active_const_provider = codex_effective_provider_from_config_text(&config);
    let sync =
        sync_codex_sessions_to_provider_inner(&active_const_provider, report_progress)?;
    attach_codex_session_sync_details(&mut finished, Ok(sync));
    Ok(finished)
}

pub(crate) fn check_codex_config(base_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    let base_url = tool_surface_url(base_url, "v1");
    let dir = codex_home();
    let auth_path = dir.join("auth.json");
    let config_path = dir.join("config.toml");
    let mut result = ToolApplyBuilder::default();
    let raw_auth = read_json_or_default(&auth_path, serde_json::json!({}))?;
    let current_config = read_text_or_empty(&config_path)?;
    let auth = upsert_codex_auth(raw_auth.clone(), api_key)?;
    let config = upsert_codex_toml(&current_config, &base_url, api_key)?;
    observe_json(&auth_path, &auth, &mut result)?;
    observe_text(&config_path, &config, &mut result)?;
    if codex_model_source_from_config(&current_config) == CodexModelSource::Const {
        check_codex_managed_catalog(&mut result)?;
    }
    let mut finished = result.finish("codex");
    finished.details = codex_status_details(&raw_auth, &current_config);
    Ok(finished)
}

fn attach_codex_session_sync_details(
    result: &mut ToolApplyResult,
    sync_result: Result<CodexSessionMigrationResult>,
) {
    attach_codex_session_sync_details_with_prefix(result, sync_result, "session_migrated");
}

fn attach_codex_session_sync_details_with_prefix(
    result: &mut ToolApplyResult,
    sync_result: Result<CodexSessionMigrationResult>,
    prefix: &str,
) {
    match sync_result {
        Ok(sync) => {
            result.backups.extend(sync.backups.clone());
            if sync.migrated_files > 0 || sync.migrated_sqlite_rows > 0 {
                result.already_configured = false;
            }
            result
                .details
                .insert(format!("{prefix}_files"), sync.migrated_files.to_string());
            result.details.insert(
                format!("{prefix}_sqlite_rows"),
                sync.migrated_sqlite_rows.to_string(),
            );
            if sync.skipped_invalid_sqlite_rows > 0 {
                result.details.insert(
                    format!("{prefix}_sqlite_invalid_rows"),
                    sync.skipped_invalid_sqlite_rows.to_string(),
                );
            }
            result
                .details
                .insert(format!("{prefix}_backups"), sync.backups.len().to_string());
            if let Some(err) = sync.sqlite_error.as_ref() {
                result
                    .details
                    .insert(format!("{prefix}_sqlite_error"), err.clone());
            }
            if !sync.target_provider.trim().is_empty() {
                result
                    .details
                    .insert("session_target_provider".to_string(), sync.target_provider);
            }
        }
        Err(err) => {
            result.already_configured = false;
            result
                .details
                .insert("session_sync_error".to_string(), err.to_string());
        }
    }
}

fn verify_codex_config_written(config_path: &Path, base_url: &str, api_key: &str) -> Result<()> {
    let raw = read_text_or_empty(config_path)?;
    let doc = raw.parse::<DocumentMut>().with_context(|| {
        format!(
            "parse Codex config.toml after write {}",
            config_path.display()
        )
    })?;
    let active_provider = doc.get("model_provider").and_then(|value| value.as_str());
    let provider_matches = doc
        .get("model_providers")
        .and_then(|providers| providers.get(CODEX_CONST_API_PROVIDER_ID))
        .and_then(|provider| provider.as_table())
        .map(|provider| codex_provider_table_matches_const_api(provider, base_url, api_key))
        .unwrap_or(false);
    if active_provider == Some(CODEX_CONST_API_PROVIDER_ID) && provider_matches {
        return Ok(());
    }
    Err(anyhow!(
        "Codex config verification failed: config.toml provider is {:?}; CONST API configuration is incomplete and may have been overwritten by Codex++ or another process.",
        active_provider
    ))
}

fn codex_status_details(auth: &serde_json::Value, config_text: &str) -> HashMap<String, String> {
    let mut details = HashMap::new();
    let model_source = codex_model_source_from_config(config_text);
    details.insert("codex_model_source".to_string(), model_source.as_str().to_string());
    details.insert("model_sync_policy".to_string(), if model_source == CodexModelSource::Const {
        "catalog"
    } else {
        "none"
    }.to_string());
    let account = codex_account_status_from_auth(auth);
    details.insert("account".to_string(), account.to_string());

    let parsed = config_text.parse::<DocumentMut>().ok();
    let provider_id = parsed
        .as_ref()
        .and_then(|doc| doc.get("model_provider").and_then(|value| value.as_str()))
        .unwrap_or("openai")
        .to_string();
    let base_url = parsed
        .as_ref()
        .and_then(|doc| {
            doc.get("model_providers")
                .and_then(|providers| providers.get(&provider_id))
                .and_then(|provider| provider.get("base_url"))
                .and_then(|value| value.as_str())
        })
        .unwrap_or_default()
        .to_string();
    let traffic = if provider_id == CODEX_CONST_API_PROVIDER_ID
        || provider_id == LEGACY_CONST_API_PROVIDER_ID
    {
        "const_api"
    } else if provider_id == "openai" {
        "openai"
    } else {
        "other"
    };
    details.insert("traffic".to_string(), traffic.to_string());
    details.insert("provider".to_string(), provider_id);
    details.insert("base_url".to_string(), base_url);
    if let Some(recent_hit) = recent_local_proxy_hit_summary() {
        details.insert("recent_hit".to_string(), recent_hit);
    }
    if codex_plus_plus_running().unwrap_or(true) {
        details.insert(
            "external_launcher".to_string(),
            "codex_plus_plus".to_string(),
        );
        details.insert(
            "external_launcher_warning".to_string(),
            "Codex++ is running and may overwrite the provider configuration. Close it, then use CONST API to launch official ChatGPT or the legacy Codex desktop app.".to_string(),
        );
    }
    details
}

#[cfg(target_os = "macos")]
fn codex_plus_plus_running() -> Result<bool> {
    let mut command = helper_command("pgrep");
    command.args(["-f", "CodexPlusPlus|Codex\\+\\+"]);
    let output = run_helper_command_with_timeout(
        &mut command,
        TOOL_HELPER_COMMAND_TIMEOUT,
        "pgrep Codex++",
    )?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(anyhow!(
            "TOOL_CONFIG_PROCESS_PROBE_FAILED: pgrep Codex++ exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )),
    }
}

#[cfg(not(target_os = "macos"))]
fn codex_plus_plus_running() -> Result<bool> {
    Ok(false)
}

fn recent_local_proxy_hit_summary() -> Option<String> {
    let value = crate::proxy::local_proxy_recent_request_status()?;
    let created_at = value
        .get("created_at_unix")
        .and_then(|value| value.as_i64())?;
    let age = (now_unix() - created_at).max(0);
    let method = value
        .get("method")
        .and_then(|value| value.as_str())
        .unwrap_or("-");
    let path = value
        .get("path")
        .and_then(|value| value.as_str())
        .unwrap_or("-");
    let status = value
        .get("status")
        .and_then(|value| value.as_u64())
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string());
    let model = value
        .get("model")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("-");
    Some(format!(
        "{method} {path} · HTTP {status} · {model} · {age}s ago"
    ))
}

fn codex_account_status_from_auth(auth: &serde_json::Value) -> &'static str {
    let has_official = auth
        .get("tokens")
        .map(|value| !value.is_null())
        .unwrap_or(false)
        || auth
            .get("refresh_token")
            .and_then(|value| value.as_str())
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
        || auth
            .get("auth_mode")
            .and_then(|value| value.as_str())
            .map(|value| value != "apikey")
            .unwrap_or(false);
    if has_official {
        return "official";
    }
    if auth
        .get("OPENAI_API_KEY")
        .and_then(|value| value.as_str())
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
    {
        return "api_key";
    }
    "none"
}

pub(crate) fn scan_codex_sessions() -> Result<CodexSessionScanResult> {
    let mut result = CodexSessionScanResult::default();
    let mut catalog_providers = None;
    for sessions_root in codex_session_roots() {
        for path in collect_jsonl_files(&sessions_root)? {
            let Some(meta) = read_codex_session_meta(&path, false)? else {
                continue;
            };
            let provider = if meta.metadata_in_sqlite {
                if catalog_providers.is_none() {
                    catalog_providers = Some(read_codex_catalog_providers()?);
                }
                meta.thread_id
                    .as_ref()
                    .and_then(|id| catalog_providers.as_ref().and_then(|providers| providers.get(id)))
                    .cloned()
                    .unwrap_or(meta.provider)
            } else {
                meta.provider
            };
            *result.provider_counts.entry(provider.clone()).or_insert(0) += 1;
            result.files.push(CodexSessionFileSummary {
                path: path.display().to_string(),
                provider,
            });
        }
    }
    Ok(result)
}

// User events belong near the session header. Bounding this secondary scan
// prevents metadata-only or damaged sessions from forcing a multi-gigabyte
// read merely to update the desktop visibility flag.
const CODEX_SESSION_USER_EVENT_SCAN_LIMIT_BYTES: u64 = 16 * 1024 * 1024;
const CODEX_SESSION_COPY_BUFFER_BYTES: usize = 1024 * 1024;
const CODEX_SESSION_PROGRESS_BYTES: u64 = 8 * 1024 * 1024;
const CODEX_SESSION_PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(50);
const CODEX_SESSION_PROGRESS_INTERVAL: Duration = Duration::from_millis(200);
// Reserve a small visible tail for flush/sync so a completed copy does not
// look frozen while the filesystem is still committing a large temp file.
const CODEX_SESSION_COPY_PROGRESS_SHARE: f64 = 0.98;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CodexSessionRewritePhase {
    Copying,
    Syncing,
}

#[derive(Clone, Copy, Debug)]
struct CodexSessionRewriteProgress {
    phase: CodexSessionRewritePhase,
    copied_bytes: u64,
    total_bytes: u64,
    sync_fraction: f64,
}

struct CodexSessionMigrationFile {
    path: PathBuf,
    bytes: u64,
}

fn sync_codex_sessions_to_provider_inner(
    target_provider: &str,
    report_progress: &mut dyn FnMut(&'static str, Option<u64>, Option<u64>, Option<f64>),
) -> Result<CodexSessionMigrationResult> {
    let target_provider = normalize_codex_provider_id(target_provider);
    let mut result = CodexSessionMigrationResult::default();
    result.target_provider = target_provider.clone();
    let mut manifest = load_codex_session_sync_manifest()?;
    let mut manifest_changed = false;
    if manifest.previous_provider.take().is_some()
        || !manifest.rollouts.is_empty()
        || !manifest.threads.is_empty()
    {
        manifest.rollouts.clear();
        manifest.threads.clear();
        manifest.version = 3;
        manifest_changed = true;
    }
    if manifest.last_session_provider.as_deref() != Some(target_provider.as_str()) {
        manifest.last_session_provider = Some(target_provider.clone());
        manifest.version = 3;
        manifest_changed = true;
    }
    let mut session_files = Vec::new();
    for sessions_root in codex_session_roots() {
        session_files.extend(collect_jsonl_files(&sessions_root)?);
    }
    session_files.sort();
    session_files.dedup();
    let session_total = u64::try_from(session_files.len()).unwrap_or(u64::MAX);
    report_progress("scanning_sessions", Some(0), Some(session_total), None);
    let mut planned_files = Vec::new();
    let mut thread_metadata = HashMap::new();
    let mut indexed_sessions = Vec::new();
    for (index, path) in session_files.into_iter().enumerate() {
        let Some(meta) = read_codex_session_meta(&path, true)? else {
            result.skipped_files += 1;
            report_progress(
                "scanning_sessions",
                Some(u64::try_from(index + 1).unwrap_or(u64::MAX)),
                Some(session_total),
                None,
            );
            continue;
        };
        if meta.metadata_in_sqlite {
            // Codex paginated metadata is SQLite-owned. Rewriting SessionMeta changes
            // every following byte offset in thread_history_*.sqlite, and even a
            // same-length rewrite would mutate canonical/shared rollout history.
            // Keep the entire file immutable; the catalog plan below migrates provider
            // visibility, including sessions whose original provider differs from it.
            indexed_sessions.push(meta.thread_id);
            result.skipped_files += 1;
            report_progress(
                "scanning_sessions",
                Some(u64::try_from(index + 1).unwrap_or(u64::MAX)),
                Some(session_total),
                None,
            );
            continue;
        }
        remember_codex_session_thread_metadata(&mut thread_metadata, &meta);
        let should_migrate = meta.provider != target_provider;
        if !should_migrate {
            result.skipped_files += 1;
            report_progress(
                "scanning_sessions",
                Some(u64::try_from(index + 1).unwrap_or(u64::MAX)),
                Some(session_total),
                None,
            );
            continue;
        }
        let bytes = fs::metadata(&path)
            .with_context(|| format!("stat {}", path.display()))?
            .len();
        planned_files.push(CodexSessionMigrationFile { path, bytes });
        report_progress(
            "scanning_sessions",
            Some(u64::try_from(index + 1).unwrap_or(u64::MAX)),
            Some(session_total),
            None,
        );
    }
    report_progress("planning_session_index", None, None, None);
    let state_plan = match plan_codex_state_db_provider_updates(
        &target_provider,
        &thread_metadata,
    ) {
        Ok(plan) => plan,
        Err(err) => {
            result.sqlite_error = Some(err.to_string());
            CodexStateProviderUpdatePlan::default()
        }
    };
    // An empty Codex home has nothing to migrate. In particular, cancelling
    // integration there must remain a write-free no-op.
    if manifest_changed && (session_total > 0 || !state_plan.updates.is_empty()) {
        save_codex_session_sync_manifest(&manifest)?;
    }
    let migration_total = u64::try_from(planned_files.len()).unwrap_or(u64::MAX);
    let migration_total_bytes = planned_files
        .iter()
        .fold(0_u64, |total, file| total.saturating_add(file.bytes));
    let mut migrated_bytes = 0_u64;
    report_progress(
        "migrating_sessions",
        Some(0),
        Some(migration_total),
        Some(0.0),
    );
    for (index, migration_file) in planned_files.into_iter().enumerate() {
        let path = migration_file.path;
        let file_bytes = migration_file.bytes;
        let completed_files = u64::try_from(index).unwrap_or(u64::MAX);
        let mut file_progress = |progress: CodexSessionRewriteProgress| {
            let copy_fraction = if progress.total_bytes == 0 {
                1.0
            } else {
                progress.copied_bytes.min(progress.total_bytes) as f64 / progress.total_bytes as f64
            };
            let file_fraction = match progress.phase {
                CodexSessionRewritePhase::Copying => {
                    copy_fraction * CODEX_SESSION_COPY_PROGRESS_SHARE
                }
                CodexSessionRewritePhase::Syncing => {
                    CODEX_SESSION_COPY_PROGRESS_SHARE
                        + progress.sync_fraction.clamp(0.0, 1.0)
                            * (1.0 - CODEX_SESSION_COPY_PROGRESS_SHARE)
                }
            };
            let stage_fraction = if migration_total_bytes == 0 {
                completed_files as f64 / migration_total.max(1) as f64
            } else {
                (migrated_bytes as f64 + file_bytes as f64 * file_fraction)
                    / migration_total_bytes as f64
            };
            let stage = match progress.phase {
                CodexSessionRewritePhase::Copying => "migrating_sessions",
                CodexSessionRewritePhase::Syncing => "syncing_sessions",
            };
            report_progress(
                stage,
                Some(completed_files),
                Some(migration_total),
                Some(stage_fraction),
            );
        };
        rewrite_codex_session_provider(&path, &target_provider, &mut file_progress).with_context(|| {
            format!(
                "CODEX_SESSION_MIGRATION_RETRY_REQUIRED: migrated {index}/{} session files before {} failed; close any remaining ChatGPT, Codex CLI, or codex app-server process and retry (the migration is idempotent)",
                migration_total,
                path.display()
            )
        })?;
        migrated_bytes = migrated_bytes.saturating_add(file_bytes);
        result.migrated_files += 1;
        report_progress(
            "migrating_sessions",
            Some(u64::try_from(index + 1).unwrap_or(u64::MAX)),
            Some(migration_total),
            Some(if migration_total_bytes == 0 {
                u64::try_from(index + 1).unwrap_or(u64::MAX) as f64 / migration_total.max(1) as f64
            } else {
                migrated_bytes as f64 / migration_total_bytes as f64
            }),
        );
    }
    report_progress("updating_session_index", None, None, None);
    if result.sqlite_error.is_none() {
        match apply_codex_state_db_provider_updates(&state_plan.updates) {
            Ok(rows) => result.migrated_sqlite_rows = rows,
            Err(err) => result.sqlite_error = Some(err.to_string()),
        }
    }
    result.skipped_invalid_sqlite_rows = state_plan.skipped_invalid_rows;
    if result.sqlite_error.is_none() && !indexed_sessions.is_empty() {
        // Never silently fall back to rewriting an indexed rollout when its mutable
        // metadata is unavailable. Surface the existing session-index diagnostic.
        let validation = read_codex_catalog_providers().and_then(|providers| {
            let missing = indexed_sessions
                .iter()
                .filter(|id| !id.as_ref().is_some_and(|id| providers.contains_key(id)))
                .count();
            if missing > 0 {
                Err(anyhow!(
                    "CODEX_SESSION_INDEX_UNAVAILABLE: {missing} indexed Codex sessions have no usable active state row; history files were left unchanged. Open Codex to restore its catalog, then retry."
                ))
            } else {
                Ok(())
            }
        });
        if let Err(error) = validation {
            result.sqlite_error = Some(error.to_string());
        }
    }
    Ok(result)
}

fn restore_codex_sessions_to_provider_with_progress(
    target_provider: &str,
    report_progress: &mut dyn FnMut(&'static str, Option<u64>, Option<u64>, Option<f64>),
) -> Result<CodexSessionMigrationResult> {
    // Provider is a visibility bucket, not ownership of the local history. Both
    // switch directions use the same idempotent migration; indexed rollouts stay
    // immutable and already matching records require no writes.
    sync_codex_sessions_to_provider_inner(target_provider, report_progress)
}

fn codex_session_roots() -> Vec<PathBuf> {
    let codex = codex_home();
    vec![codex.join("sessions"), codex.join("archived_sessions")]
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CodexSessionSyncManifest {
    version: u32,
    #[serde(default)]
    previous_provider: Option<String>,
    #[serde(default)]
    last_session_provider: Option<String>,
    #[serde(default)]
    rollouts: HashMap<String, String>,
    #[serde(default)]
    threads: HashMap<String, String>,
}

#[derive(Debug, Clone)]
struct CodexSessionMeta {
    provider: String,
    thread_id: Option<String>,
    cwd: Option<String>,
    has_user_event: bool,
    metadata_in_sqlite: bool,
}

#[derive(Debug, Clone, Default)]
struct CodexSessionThreadMetadata {
    cwd: Option<String>,
    has_user_event: bool,
}

#[derive(Debug, Clone)]
struct CodexStateProviderUpdate {
    path: PathBuf,
    table: CodexStateProviderTable,
    thread_id: String,
    provider: Option<String>,
    target_provider: String,
    cwd: Option<String>,
    has_user_event: bool,
}

#[derive(Debug, Default)]
struct CodexStateProviderUpdatePlan {
    updates: Vec<CodexStateProviderUpdate>,
    skipped_invalid_rows: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexStateProviderTable {
    Threads,
    LocalThreadCatalog,
}

impl CodexStateProviderTable {
    fn name(self) -> &'static str {
        match self {
            Self::Threads => "threads",
            Self::LocalThreadCatalog => "local_thread_catalog",
        }
    }

    fn id_column(self) -> &'static str {
        match self {
            Self::Threads => "id",
            Self::LocalThreadCatalog => "thread_id",
        }
    }
}

fn load_codex_session_sync_manifest() -> Result<CodexSessionSyncManifest> {
    let path = const_api_state_path(CODEX_SESSION_SYNC_MANIFEST);
    if !path.exists() {
        return Ok(CodexSessionSyncManifest {
            version: 1,
            ..Default::default()
        });
    }
    let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))
}

fn save_codex_session_sync_manifest(manifest: &CodexSessionSyncManifest) -> Result<()> {
    let path = const_api_state_path(CODEX_SESSION_SYNC_MANIFEST);
    let content = serde_json::to_vec_pretty(manifest)?;
    let mut content = content;
    content.push(b'\n');
    atomic_write(&path, &content)
}

fn remember_codex_session_thread_metadata(
    metadata: &mut HashMap<String, CodexSessionThreadMetadata>,
    meta: &CodexSessionMeta,
) {
    let Some(thread_id) = meta
        .thread_id
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    else {
        return;
    };
    let entry = metadata.entry(thread_id.clone()).or_default();
    if entry.cwd.is_none() {
        entry.cwd = meta.cwd.clone();
    }
    entry.has_user_event |= meta.has_user_event;
}

fn plan_codex_state_db_provider_updates(
    target_provider: &str,
    thread_metadata: &HashMap<String, CodexSessionThreadMetadata>,
) -> Result<CodexStateProviderUpdatePlan> {
    let mut plan = CodexStateProviderUpdatePlan::default();
    for path in collect_codex_state_db_paths()? {
        let conn = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("open Codex state db {}", path.display()))?;
        conn.busy_timeout(Duration::from_millis(1500))?;
        for table in codex_state_provider_tables(&conn)? {
            let columns = codex_state_table_columns(&conn, table.name())?;
            let cwd_column = if columns.contains("cwd") { "cwd" } else { "NULL" };
            let user_event_column = if table == CodexStateProviderTable::Threads
                && columns.contains("has_user_event")
            {
                "has_user_event"
            } else {
                "NULL"
            };
            let query = format!(
                "SELECT {}, model_provider, {cwd_column}, {user_event_column} FROM {}",
                table.id_column(),
                table.name()
            );
            let mut stmt = conn
                .prepare(&query)
                .with_context(|| format!("prepare Codex state db {}", path.display()))?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            })?;
            let mut missing_thread_ids = 0usize;
            for row in rows {
                let (thread_id, provider, current_cwd, current_has_user_event) =
                    row.with_context(|| {
                        format!(
                            "read Codex state db {} table {}",
                            path.display(),
                            table.name()
                        )
                    })?;
                let Some(thread_id) = thread_id.filter(|value| !value.trim().is_empty()) else {
                    missing_thread_ids += 1;
                    continue;
                };
                let metadata = thread_metadata.get(&thread_id);
                let needs_provider_update = provider.as_deref() != Some(target_provider);
                let needs_cwd_update = table == CodexStateProviderTable::Threads
                    && columns.contains("cwd")
                    && metadata
                        .and_then(|meta| meta.cwd.as_deref())
                        .filter(|cwd| !cwd.trim().is_empty())
                        .is_some_and(|cwd| current_cwd.as_deref() != Some(cwd));
                let needs_user_event_update = table == CodexStateProviderTable::Threads
                    && columns.contains("has_user_event")
                    && metadata.is_some_and(|meta| {
                        meta.has_user_event && current_has_user_event.unwrap_or(0) != 1
                    });
                let needs_visibility_update = provider.as_deref() == Some(target_provider)
                    && (needs_cwd_update || needs_user_event_update);
                if needs_provider_update || needs_visibility_update {
                    plan.updates.push(CodexStateProviderUpdate {
                        path: path.clone(),
                        table,
                        cwd: metadata.and_then(|meta| meta.cwd.clone()),
                        has_user_event: metadata.map(|meta| meta.has_user_event).unwrap_or(false),
                        thread_id,
                        provider,
                        target_provider: target_provider.to_string(),
                    });
                }
            }
            if missing_thread_ids > 0 {
                plan.skipped_invalid_rows += missing_thread_ids;
            }
        }
    }
    Ok(plan)
}

fn apply_codex_state_db_provider_updates(planned: &[CodexStateProviderUpdate]) -> Result<usize> {
    let mut migrated = 0usize;
    let mut paths = planned
        .iter()
        .map(|row| row.path.clone())
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    for path in paths {
        let rows = planned
            .iter()
            .filter(|row| row.path == path)
            .collect::<Vec<_>>();
        if rows.is_empty() {
            continue;
        }
        let mut conn = Connection::open(&path)
            .with_context(|| format!("open Codex state db {}", path.display()))?;
        conn.busy_timeout(Duration::from_millis(1500))?;
        // Only provider/index fields change. SQLite rolls this transaction back on failure;
        // cancellation switches those fields to the restored provider, not an old whole DB.
        let tx = conn.transaction()?;
        let mut changed_catalog = false;
        for row in rows {
            let columns = codex_state_table_columns(&tx, row.table.name())?;
            if row.provider.as_deref() != Some(row.target_provider.as_str()) {
                let changed = if let Some(provider) = row.provider.as_deref() {
                    let update_provider = format!(
                        "UPDATE {} SET model_provider = ?1 WHERE {} = ?2 AND model_provider = ?3",
                        row.table.name(),
                        row.table.id_column()
                    );
                    tx.execute(
                        &update_provider,
                        (
                            row.target_provider.as_str(),
                            row.thread_id.as_str(),
                            provider,
                        ),
                    )
                } else {
                    let update_provider = format!(
                        "UPDATE {} SET model_provider = ?1 WHERE {} = ?2 AND model_provider IS NULL",
                        row.table.name(),
                        row.table.id_column()
                    );
                    tx.execute(
                        &update_provider,
                        (row.target_provider.as_str(), row.thread_id.as_str()),
                    )
                }
                .with_context(|| format!("update Codex state db {}", path.display()))?;
                migrated += changed;
                changed_catalog |=
                    changed > 0 && row.table == CodexStateProviderTable::LocalThreadCatalog;
            }
            if row.table == CodexStateProviderTable::Threads
                && row.has_user_event
                && columns.contains("has_user_event")
            {
                migrated += tx
                    .execute(
                        "UPDATE threads SET has_user_event = 1 WHERE id = ?1 AND COALESCE(has_user_event, 0) <> 1",
                        [row.thread_id.as_str()],
                    )
                    .with_context(|| format!("update Codex state db {}", path.display()))?;
            }
            if let Some(cwd) = row.cwd.as_ref().filter(|value| !value.trim().is_empty()) {
                if columns.contains("cwd") {
                    let update_cwd = format!(
                        "UPDATE {} SET cwd = ?1 WHERE {} = ?2 AND COALESCE(cwd, '') <> ?1",
                        row.table.name(),
                        row.table.id_column()
                    );
                    let changed = tx
                        .execute(&update_cwd, (cwd.as_str(), row.thread_id.as_str()))
                        .with_context(|| format!("update Codex state db {}", path.display()))?;
                    migrated += changed;
                    changed_catalog |=
                        changed > 0 && row.table == CodexStateProviderTable::LocalThreadCatalog;
                }
            }
        }
        if changed_catalog {
            bump_codex_local_thread_catalog_revision(&tx)?;
        }
        tx.commit()?;
    }
    Ok(migrated)
}

fn codex_state_provider_tables(conn: &Connection) -> Result<Vec<CodexStateProviderTable>> {
    let mut tables = Vec::new();
    if codex_state_table_has_model_provider(conn, "threads")? {
        tables.push(CodexStateProviderTable::Threads);
    }
    if codex_state_table_has_model_provider(conn, "local_thread_catalog")? {
        tables.push(CodexStateProviderTable::LocalThreadCatalog);
    }
    Ok(tables)
}

fn bump_codex_local_thread_catalog_revision(conn: &Connection) -> Result<()> {
    let has_metadata = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'local_thread_catalog_metadata'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
        > 0;
    if !has_metadata {
        return Ok(());
    }
    conn.execute(
        "UPDATE local_thread_catalog_metadata SET catalog_revision = catalog_revision + 1 WHERE id = 1",
        [],
    )?;
    Ok(())
}

fn collect_codex_state_db_paths() -> Result<Vec<PathBuf>> {
    let root = codex_home();
    let mut files = Vec::new();
    let state_db = codex_active_sqlite_home(&root)?.join("state_5.sqlite");
    if state_db.is_file() {
        files.push(state_db);
    }
    let desktop_catalog = root.join("sqlite").join("codex-dev.db");
    if desktop_catalog.is_file() {
        files.push(desktop_catalog);
    }
    files.sort();
    files.dedup();
    Ok(files)
}

fn read_codex_catalog_providers() -> Result<HashMap<String, String>> {
    let root = codex_home();
    let path = codex_active_sqlite_home(&root)?.join("state_5.sqlite");
    if !path.is_file() {
        return Ok(HashMap::new());
    }
    let conn = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("open Codex state db {}", path.display()))?;
    conn.busy_timeout(Duration::from_millis(1500))?;
    if !codex_state_table_has_model_provider(&conn, "threads")? {
        return Ok(HashMap::new());
    }
    let mut stmt = conn.prepare("SELECT id, model_provider FROM threads")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, Option<String>>(1)?,
        ))
    })?;
    let mut providers = HashMap::new();
    for row in rows {
        if let (Some(id), Some(provider)) = row? {
            if !id.trim().is_empty() && !provider.trim().is_empty() {
                providers.insert(id, provider);
            }
        }
    }
    Ok(providers)
}

fn codex_active_sqlite_home(codex_home: &Path) -> Result<PathBuf> {
    let config_path = codex_home.join("config.toml");
    if config_path.is_file() {
        let raw = fs::read_to_string(&config_path)
            .with_context(|| format!("read {}", config_path.display()))?;
        let doc = raw
            .parse::<DocumentMut>()
            .with_context(|| format!("parse {}", config_path.display()))?;
        if let Some(value) = doc.get("sqlite_home").and_then(Item::as_str) {
            return codex_absolute_sqlite_home(value, "config.toml sqlite_home");
        }
    }
    if !test_home_active() {
        if let Some(value) = std::env::var_os("CODEX_SQLITE_HOME") {
            return codex_absolute_sqlite_home(&value.to_string_lossy(), "CODEX_SQLITE_HOME");
        }
    }
    Ok(codex_home.to_path_buf())
}

fn codex_absolute_sqlite_home(value: &str, source: &str) -> Result<PathBuf> {
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(anyhow!(
            "CODEX_SQLITE_HOME_INVALID: {source} must be an absolute path: {value}"
        ));
    }
    Ok(path)
}

fn codex_state_table_has_model_provider(conn: &Connection, table: &str) -> Result<bool> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if count == 0 {
        return Ok(false);
    }
    Ok(codex_state_table_columns(conn, table)?.contains("model_provider"))
}

fn codex_state_table_columns(conn: &Connection, table: &str) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = stmt.query_map([], |row| row.get::<_, String>(1))?;
    Ok(columns.collect::<rusqlite::Result<HashSet<_>>>()?)
}

fn collect_jsonl_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_jsonl_files_inner(root, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_jsonl_files_inner(path: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if path.is_file() {
        if path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
            files.push(path.to_path_buf());
        }
        return Ok(());
    }
    if !path.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(path).with_context(|| format!("read dir {}", path.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value == ".git")
        {
            continue;
        }
        collect_jsonl_files_inner(&path, files)?;
    }
    Ok(())
}

#[cfg(test)]
fn read_codex_session_provider(path: &Path) -> Result<Option<String>> {
    Ok(read_codex_session_meta(path, false)?.map(|meta| meta.provider))
}

fn codex_session_metadata_in_sqlite(value: &serde_json::Value) -> bool {
    // Unknown future history modes and numbered rollouts must not be treated as
    // legacy merely because this CONST version does not recognize their format.
    value
        .get("ordinal")
        .and_then(serde_json::Value::as_u64)
        .is_some()
        || value
            .pointer("/payload/history_mode")
            .is_some_and(|mode| mode.as_str() != Some("legacy"))
}

fn read_codex_session_meta(path: &Path, scan_user_events: bool) -> Result<Option<CodexSessionMeta>> {
    let file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut line = Vec::new();
    let mut session_meta = None;
    let mut has_user_event = false;
    loop {
        line.clear();
        if reader
            .read_until(b'\n', &mut line)
            .with_context(|| format!("read {}", path.display()))?
            == 0
        {
            break;
        }
        has_user_event |= codex_session_line_has_user_event(&line);
        if has_user_event && session_meta.is_some() {
            break;
        }
        if !codex_session_line_is_meta_candidate(&line) {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(|value| value.as_str()) != Some("session_meta") {
            continue;
        }
        let provider = value
            .pointer("/payload/model_provider")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string())
            .unwrap_or_else(|| "openai".to_string());
        let thread_id = value
            .pointer("/payload/id")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string());
        let cwd = value
            .pointer("/payload/cwd")
            .and_then(|value| value.as_str())
            .map(normalize_codex_session_cwd);
        let metadata_in_sqlite = codex_session_metadata_in_sqlite(&value);
        session_meta = Some(CodexSessionMeta {
            provider,
            thread_id,
            cwd,
            has_user_event: false,
            metadata_in_sqlite,
        });
        if scan_user_events && !has_user_event && !metadata_in_sqlite {
            has_user_event =
                codex_reader_has_user_event(&mut reader, CODEX_SESSION_USER_EVENT_SCAN_LIMIT_BYTES)
                    .with_context(|| format!("scan user events in {}", path.display()))?;
        }
        break;
    }
    if let Some(meta) = session_meta.as_mut() {
        meta.has_user_event = has_user_event;
    }
    Ok(session_meta)
}

fn codex_reader_has_user_event(
    reader: &mut impl BufRead,
    scan_limit: u64,
) -> std::io::Result<bool> {
    const USER_EVENT_OVERLAP_BYTES: usize = 15;
    let mut scanned = 0_u64;
    let mut tail = Vec::with_capacity(USER_EVENT_OVERLAP_BYTES);
    while scanned < scan_limit {
        let (consumed, found) = {
            let buffer = reader.fill_buf()?;
            if buffer.is_empty() {
                break;
            }
            let consumed = usize::try_from((scan_limit - scanned).min(buffer.len() as u64))
                .unwrap_or(buffer.len());
            let chunk = &buffer[..consumed];
            let mut found = codex_session_line_has_user_event(chunk);
            if !found && !tail.is_empty() {
                let prefix_len = chunk.len().min(USER_EVENT_OVERLAP_BYTES);
                let mut boundary = Vec::with_capacity(tail.len() + prefix_len);
                boundary.extend_from_slice(&tail);
                boundary.extend_from_slice(&chunk[..prefix_len]);
                found = codex_session_line_has_user_event(&boundary);
            }
            tail.clear();
            let tail_start = chunk.len().saturating_sub(USER_EVENT_OVERLAP_BYTES);
            tail.extend_from_slice(&chunk[tail_start..]);
            (consumed, found)
        };
        reader.consume(consumed);
        scanned = scanned.saturating_add(consumed as u64);
        if found {
            return Ok(true);
        }
    }
    Ok(false)
}

fn codex_session_line_has_user_event(line: &[u8]) -> bool {
    const USER_MESSAGE: &[u8] = b"\"user_message\"";
    const USER_INPUT: &[u8] = b"\"user_input\"";
    line.windows(USER_MESSAGE.len())
        .any(|window| window == USER_MESSAGE)
        || line
            .windows(USER_INPUT.len())
            .any(|window| window == USER_INPUT)
}

fn codex_session_line_is_meta_candidate(line: &[u8]) -> bool {
    const SESSION_META: &[u8] = b"\"session_meta\"";
    line.windows(SESSION_META.len())
        .any(|window| window == SESSION_META)
}

fn normalize_codex_session_cwd(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.to_ascii_lowercase().starts_with(r"\\?\unc\") && trimmed.len() > 8 {
        return format!(r"\\{}", trimmed[8..].replace('/', "\\"));
    }
    if trimmed.starts_with(r"\\?\") && trimmed.len() > 4 {
        return trimmed[4..].replace('\\', "/");
    }
    trimmed.to_string()
}

fn report_codex_session_copy_progress(
    report_progress: &mut dyn FnMut(CodexSessionRewriteProgress),
    copied_bytes: u64,
    total_bytes: u64,
    last_reported_bytes: &mut u64,
    last_reported_at: &mut Instant,
    force: bool,
) {
    if !force {
        let elapsed = last_reported_at.elapsed();
        if elapsed < CODEX_SESSION_PROGRESS_MIN_INTERVAL
            || (copied_bytes.saturating_sub(*last_reported_bytes) < CODEX_SESSION_PROGRESS_BYTES
                && elapsed < CODEX_SESSION_PROGRESS_INTERVAL)
        {
            return;
        }
    }
    *last_reported_bytes = copied_bytes;
    *last_reported_at = Instant::now();
    report_progress(CodexSessionRewriteProgress {
        phase: CodexSessionRewritePhase::Copying,
        copied_bytes,
        total_bytes,
        sync_fraction: 0.0,
    });
}

fn rewrite_codex_session_provider(
    path: &Path,
    provider: &str,
    report_progress: &mut dyn FnMut(CodexSessionRewriteProgress),
) -> Result<()> {
    let source = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let before = source
        .metadata()
        .with_context(|| format!("stat {}", path.display()))?;
    let total_bytes = before.len();
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("missing parent for {}", path.display()))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    fs::set_permissions(temp.path(), before.permissions())?;
    let mut reader = BufReader::new(source);
    let mut writer = BufWriter::new(temp.as_file_mut());
    let mut segment = Vec::new();
    let mut changed = false;
    let mut copied_bytes = 0_u64;
    let mut last_reported_bytes = 0_u64;
    let mut last_reported_at = Instant::now();
    loop {
        segment.clear();
        if reader
            .read_until(b'\n', &mut segment)
            .with_context(|| format!("read {}", path.display()))?
            == 0
        {
            break;
        }
        copied_bytes = copied_bytes.saturating_add(segment.len() as u64);
        let line_end = if segment.ends_with(b"\r\n") {
            b"\r\n".as_slice()
        } else if segment.ends_with(b"\n") {
            b"\n".as_slice()
        } else {
            b"".as_slice()
        };
        let line_len = segment.len().saturating_sub(line_end.len());
        if !codex_session_line_is_meta_candidate(&segment[..line_len]) {
            writer.write_all(&segment)?;
            continue;
        }
        if let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&segment[..line_len]) {
            if value.get("type").and_then(|value| value.as_str()) == Some("session_meta") {
                // Recheck at the actual write boundary: Codex may have migrated a
                // legacy file after our planning pass. Never publish over its index.
                if codex_session_metadata_in_sqlite(&value) {
                    return Err(anyhow!(
                        "CODEX_SESSION_INDEXED_HISTORY: {} uses SQLite-owned metadata; refusing to rewrite its history",
                        path.display()
                    ));
                }
                if let Some(payload) = value
                    .get_mut("payload")
                    .and_then(|value| value.as_object_mut())
                {
                    if payload
                        .get("model_provider")
                        .and_then(|value| value.as_str())
                        == Some(provider)
                    {
                        return Ok(());
                    }
                    payload.insert(
                        "model_provider".to_string(),
                        serde_json::Value::String(provider.to_string()),
                    );
                    serde_json::to_writer(&mut writer, &value)?;
                    writer.write_all(line_end)?;
                    changed = true;
                    report_codex_session_copy_progress(
                        report_progress,
                        copied_bytes,
                        total_bytes,
                        &mut last_reported_bytes,
                        &mut last_reported_at,
                        false,
                    );
                    break;
                }
            }
        }
        writer.write_all(&segment)?;
        report_codex_session_copy_progress(
            report_progress,
            copied_bytes,
            total_bytes,
            &mut last_reported_bytes,
            &mut last_reported_at,
            false,
        );
    }
    if !changed {
        return Ok(());
    }
    let mut copy_buffer = vec![0_u8; CODEX_SESSION_COPY_BUFFER_BYTES];
    loop {
        let read = reader
            .read(&mut copy_buffer)
            .with_context(|| format!("read {}", path.display()))?;
        if read == 0 {
            break;
        }
        writer.write_all(&copy_buffer[..read])?;
        copied_bytes = copied_bytes.saturating_add(read as u64);
        report_codex_session_copy_progress(
            report_progress,
            copied_bytes,
            total_bytes,
            &mut last_reported_bytes,
            &mut last_reported_at,
            false,
        );
    }
    drop(reader);
    writer.flush()?;
    drop(writer);
    report_codex_session_copy_progress(
        report_progress,
        copied_bytes,
        total_bytes,
        &mut last_reported_bytes,
        &mut last_reported_at,
        true,
    );
    report_progress(CodexSessionRewriteProgress {
        phase: CodexSessionRewritePhase::Syncing,
        copied_bytes,
        total_bytes,
        sync_fraction: 0.0,
    });
    let sync_file = temp.as_file().try_clone()?;
    std::thread::scope(|scope| -> Result<()> {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        scope.spawn(move || {
            let _ = sender.send(sync_file.sync_all());
        });
        let mut heartbeat = 0_u64;
        loop {
            match receiver.recv_timeout(CODEX_SESSION_PROGRESS_INTERVAL) {
                Ok(result) => {
                    result?;
                    break;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    heartbeat = heartbeat.saturating_add(1);
                    report_progress(CodexSessionRewriteProgress {
                        phase: CodexSessionRewritePhase::Syncing,
                        copied_bytes,
                        total_bytes,
                        sync_fraction: (heartbeat as f64 * 0.08).min(0.9),
                    });
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(anyhow!("sync worker disconnected for {}", path.display()));
                }
            }
        }
        Ok(())
    })?;
    report_progress(CodexSessionRewriteProgress {
        phase: CodexSessionRewritePhase::Syncing,
        copied_bytes,
        total_bytes,
        sync_fraction: 1.0,
    });
    let after = fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
        return Err(anyhow!(
            "CODEX_SESSION_CHANGED_DURING_MIGRATION: {} changed while it was being migrated; close any remaining ChatGPT, Codex CLI, or codex app-server process and retry",
            path.display()
        ));
    }
    temp.persist(path)
        .map_err(|err| anyhow!("persist {}: {}", path.display(), err.error))?;
    Ok(())
}
