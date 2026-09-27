#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CodexModelSource {
    #[default]
    Codex,
    Const,
}

impl CodexModelSource {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Const => "const",
        }
    }
}

const CODEX_CATALOG_CONFIG_PATHS: &[&[&str]] = &[&["model_catalog_json"], &["web_search"]];
const CODEX_MAX_CATALOG_CONTEXT: u64 = i64::MAX as u64 / 100;

fn codex_managed_catalog_path() -> PathBuf {
    codex_home().join("const-api.models.json")
}

fn codex_model_source_from_config(raw: &str) -> CodexModelSource {
    let managed = codex_managed_catalog_path();
    if raw.parse::<DocumentMut>().ok().is_some_and(|doc| {
        doc.get("model_catalog_json")
            .and_then(Item::as_str)
            .is_some_and(|path| Path::new(path) == managed)
    }) {
        CodexModelSource::Const
    } else {
        CodexModelSource::Codex
    }
}

pub(crate) fn current_codex_model_source() -> Result<CodexModelSource> {
    Ok(codex_model_source_from_config(&read_text_or_empty(
        &codex_home().join("config.toml"),
    )?))
}

pub(crate) fn read_codex_cached_models() -> serde_json::Value {
    // Read-only reuse of Codex's own metadata, never modify its cache. Missing,
    // corrupt or oversized caches are optional; the generic catalog still works.
    let path = codex_home().join("models_cache.json");
    fs::metadata(&path)
        .ok()
        .filter(|metadata| metadata.len() <= 8 * 1024 * 1024)
        .and_then(|_| fs::read(&path).ok())
        .and_then(|raw| serde_json::from_slice(&raw).ok())
        .unwrap_or(serde_json::Value::Null)
}

pub(crate) fn codex_catalog_model_eligible(model: &ToolModelInfo) -> bool {
    model.tool_call
        && !model.id.trim().is_empty()
        && (model.output_modalities.is_empty()
            || model
                .output_modalities
                .iter()
                .any(|modality| modality == "text"))
}

// The public model list has already merged platform/local availability and
// normalized IDs. No inference probes are performed while configuring a tool.
pub(crate) fn build_codex_model_catalog(
    models: &[ToolModelInfo],
    cached: &serde_json::Value,
) -> Result<serde_json::Value> {
    let mut entries = BTreeMap::new();
    let mut priorities = HashMap::new();
    for model in models
        .iter()
        .filter(|model| codex_catalog_model_eligible(model))
    {
        let id = crate::config::public_model_name(&model.id);
        if id.is_empty() || entries.contains_key(&id) {
            continue;
        }
        // Exact identity only: never borrow another model's instructions,
        // reasoning limits, freeform tools or experimental Codex capabilities.
        let native = cached
            .get("models")
            .and_then(serde_json::Value::as_array)
            .and_then(|models| {
                models.iter().find(|entry| {
                    entry.get("slug").and_then(serde_json::Value::as_str) == Some(id.as_str())
                })
            })
            .filter(|entry| codex_cached_model_complete(entry));
        let mut entry = generic_codex_catalog_model(model);
        if let Some(native) = native.and_then(serde_json::Value::as_object) {
            // Keep newer native metadata while supplying the fields required
            // by older Codex catalog readers (which ignore newer fields).
            if let Some(object) = entry.as_object_mut() {
                object.extend(native.clone());
            }
            let supports_summary = native
                .get("supports_reasoning_summary_parameter")
                .or_else(|| native.get("supports_reasoning_summaries"))
                .cloned()
                .unwrap_or(serde_json::json!(true));
            entry["supports_reasoning_summaries"] = supports_summary.clone();
            entry["supports_reasoning_summary_parameter"] = supports_summary;
            if !native.contains_key("supports_parallel_tool_calls") {
                entry["supports_parallel_tool_calls"] = serde_json::json!(true);
            }
            if let Some(template) = entry
                .pointer("/model_messages/instructions_template")
                .and_then(serde_json::Value::as_str)
            {
                let personality = entry
                    .pointer("/model_messages/instructions_variables/personality_default")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                entry["base_instructions"] =
                    serde_json::json!(template.replace("{{ personality }}", personality));
            }
        }
        entry["slug"] = serde_json::json!(id);
        entry["display_name"] = serde_json::json!(id);
        entry["visibility"] = serde_json::json!("list");
        entry["supported_in_api"] = serde_json::json!(true);
        // Native Codex defaults may be smaller than a model's maximum. Keep
        // that headroom; a smaller detected route capacity always wins.
        if let Some(limit) = model.context_tokens.filter(|limit| *limit > 0) {
            for key in ["context_window", "max_context_window"] {
                let limit = entry
                    .get(key)
                    .and_then(serde_json::Value::as_u64)
                    .filter(|current| *current > 0)
                    .map_or(limit, |current| current.min(limit));
                entry[key] = serde_json::json!(limit.min(CODEX_MAX_CATALOG_CONTEXT));
            }
        }
        priorities.insert(id.clone(), model.display_priority);
        entries.insert(id, entry);
    }
    if entries.is_empty() {
        return Err(anyhow!(
            "TOOL_CONFIG_MODELS_UNAVAILABLE: No text tool-calling models are available for Codex injection; existing configuration is unchanged."
        ));
    }
    let mut ordered_entries = entries.into_iter().collect::<Vec<_>>();
    ordered_entries.sort_by(|(a, _), (b, _)| priorities[b].cmp(&priorities[a]).then_with(|| a.cmp(b)));
    let models = ordered_entries
        .into_iter().map(|(_, entry)| entry)
        .enumerate()
        .map(|(index, mut entry)| {
            entry["priority"] = serde_json::json!(index);
            entry
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({"models": models}))
}

fn codex_cached_model_complete(entry: &serde_json::Value) -> bool {
    entry
        .get("shell_type")
        .is_some_and(serde_json::Value::is_string)
        && entry
            .get("supported_reasoning_levels")
            .is_some_and(serde_json::Value::is_array)
        && entry
            .get("support_verbosity")
            .is_some_and(serde_json::Value::is_boolean)
        && entry
            .get("truncation_policy")
            .is_some_and(serde_json::Value::is_object)
        && (entry
            .get("base_instructions")
            .is_some_and(serde_json::Value::is_string)
            || entry
                .pointer("/model_messages/instructions_template")
                .is_some_and(serde_json::Value::is_string))
}

fn generic_codex_catalog_model(model: &ToolModelInfo) -> serde_json::Value {
    let efforts = model
        .reasoning_efforts
        .iter()
        .filter(|effort| {
            matches!(
                effort.as_str(),
                "none" | "minimal" | "low" | "medium" | "high" | "xhigh"
            )
        })
        .map(|effort| serde_json::json!({"effort": effort, "description": effort}))
        .collect::<Vec<_>>();
    let default_effort = efforts
        .iter()
        .find(|effort| effort["effort"] == "medium")
        .or_else(|| efforts.first())
        .map(|effort| effort["effort"].clone());
    // Unknown route capacity must not turn into Codex's large OpenAI fallback.
    let context = model
        .context_tokens
        .filter(|limit| *limit > 0)
        .unwrap_or(32_768)
        .min(CODEX_MAX_CATALOG_CONTEXT);
    let compact = (context - context / 10)
        .min(context.saturating_sub(model.output_tokens.unwrap_or(4_096).min(context / 2)));
    let instructions = "You are a coding assistant working in the user's workspace. Follow the user's instructions and repository guidance. Inspect relevant files before editing, preserve unrelated changes, use the available tools to complete the task, and verify your work. Report results and any remaining limitations clearly.";
    serde_json::json!({
        "slug": model.id,
        "display_name": model.id,
        "description": "CONST API",
        "default_reasoning_level": default_effort,
        "supported_reasoning_levels": efforts,
        // `default` is supported by older Codex and an alias of unified_exec
        // in current Codex. It uses function tools, not native OpenAI tools.
        "shell_type": "default",
        "visibility": "list",
        "supported_in_api": true,
        "priority": 0,
        "availability_nux": null,
        "upgrade": null,
        "base_instructions": instructions,
        "supports_reasoning_summaries": false,
        "supports_parallel_tool_calls": false,
        "supports_reasoning_summary_parameter": false,
        "default_reasoning_summary": "none",
        "support_verbosity": false,
        "default_verbosity": null,
        "apply_patch_tool_type": null,
        "truncation_policy": {"mode": "tokens", "limit": 10_000},
        "context_window": context,
        "max_context_window": context,
        "auto_compact_token_limit": compact,
        "effective_context_window_percent": 95,
        "experimental_supported_tools": [],
        "input_modalities": if model.input_modalities.iter().any(|modality| modality == "image") {
            vec!["text", "image"]
        } else { vec!["text"] },
        "supports_search_tool": false,
        "supports_experimental_context": false,
        "use_responses_lite": false,
        "node_repl_disabled": true,
    })
}

fn check_codex_managed_catalog(result: &mut ToolApplyBuilder) -> Result<()> {
    let path = codex_managed_catalog_path();
    let value = read_json_or_default(&path, serde_json::Value::Null)?;
    if !value
        .get("models")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|models| !models.is_empty())
    {
        return Err(anyhow!("Codex model catalog is missing or empty; configure it again."));
    }
    observe_json(&path, &value, result)
}
