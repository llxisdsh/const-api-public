// Muse Code 1.4.7: verified with the official native host and a local Responses
// fixture. `model_catalog` is a settings overlay, not its remote catalog cache.
fn muse_code_paths() -> (PathBuf, PathBuf) {
    let root = xdg_tool_config_root().join("muse");
    (root.join("settings.json"), root.join("auth.json"))
}

fn muse_code_runtime_executables(launcher: &Path) -> Vec<PathBuf> {
    // Official launchers run a versioned sibling binary. Match only those
    // exact files, never claim the whole install directory (often ~/.local/bin).
    if normalized_tool_program_identity(&launcher.to_string_lossy()) != "muse" {
        return Vec::new();
    }
    let Some(entries) = launcher.parent().and_then(|dir| fs::read_dir(dir).ok()) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let version = name.strip_prefix("muse-bin-")?;
            let version = if cfg!(target_os = "windows") {
                version.strip_suffix(".exe")?
            } else {
                version
            };
            (!version.is_empty()
                && version.as_bytes()[0].is_ascii_digit()
                && version
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
                && entry.file_type().ok()?.is_file())
            .then(|| entry.path())
        })
        .collect()
}

fn read_muse_code_document(path: &Path) -> Result<serde_json::Value> {
    let raw = read_text_or_empty(path)?;
    let mut root: serde_json::Value = if raw.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?
    };
    if !root.is_object()
        || root
            .get("schema_version")
            .is_some_and(|version| version.as_u64() != Some(1))
    {
        return Err(anyhow!(
            "Muse Code requires a JSON object with schema_version 1: {}",
            path.display()
        ));
    }
    root["schema_version"] = serde_json::json!(1);
    Ok(root)
}

fn muse_code_object<'a>(
    root: &'a mut serde_json::Value,
    key: &str,
) -> Result<&'a mut serde_json::Value> {
    if root.get(key).is_none() {
        root[key] = serde_json::json!({});
    }
    let value = &mut root[key];
    if !value.is_object() {
        return Err(anyhow!("Muse Code {key} must be an object"));
    }
    Ok(value)
}

fn muse_code_model_row(model: &ToolModelInfo, order: usize, is_default: bool) -> serde_json::Value {
    // The meta transport rejects `none`, despite the top-level CLI help listing
    // it. Empty variants is the native representation for no selectable tiers.
    let efforts = declared_reasoning_efforts(model)
        .into_iter()
        .filter(|effort| {
            matches!(
                effort.as_str(),
                "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra"
            )
        })
        .map(|tier| serde_json::json!({"tier": tier}))
        .collect::<Vec<_>>();
    let label = if model.display_name.trim().is_empty() {
        model.id.trim()
    } else {
        model.display_name.trim()
    };
    serde_json::json!({
        "model_id": model.id.trim(),
        "provider_id": "meta",
        "profile_id": "tbh",
        "display_label": format!("{label} (CONST API)"),
        "visibility": "visible",
        "display_order": order,
        "is_default": is_default,
        "context_limit": model.context_tokens.filter(|v| *v > 0),
        "output_limit": model.output_tokens.filter(|v| *v > 0),
        "reasoning_effort_variants": efforts,
    })
}

fn restore_stale_muse_code_models(
    path: &Path,
    root: &mut serde_json::Value,
    ids: &HashSet<&str>,
) -> Result<()> {
    let _guard = tool_config_manifest_lock()
        .lock()
        .map_err(|_| anyhow!("TOOL_CONFIG_MANIFEST_LOCK_POISONED: manifest lock poisoned"))?;
    let manifest = load_tool_config_manifest()?;
    let Some(ownership) = manifest
        .files
        .get(&manifest_file_key(path))
        .filter(|v| v.tool == "muse-code")
    else {
        return Ok(());
    };
    for field in &ownership.fields {
        let [
            ToolConfigFieldPath::Key(collection),
            ToolConfigFieldPath::Identified(identity),
            ..,
        ] = field.path.as_slice()
        else {
            continue;
        };
        if collection == "model_catalog"
            && identity
                .get("model_id")
                .is_some_and(|id| !ids.contains(id.as_str()))
            && (owned_field_is_added_entry(field)
                || tool_config_field_value_at(root, &field.path) == field.applied)
        {
            set_tool_config_field_value(root, &field.path, &field.original);
        }
    }
    Ok(())
}

fn configure_muse_code(
    base_url: &str,
    api_key: &str,
    models: Option<&[ToolModelInfo]>,
) -> Result<ToolApplyResult> {
    let (settings_path, auth_path) = muse_code_paths();
    let mut settings = read_muse_code_document(&settings_path)?;
    let mut auth = read_muse_code_document(&auth_path)?;
    settings["provider"] = serde_json::json!("meta");
    let transport = muse_code_object(&mut settings, "endpoint_transport")?;
    // An off-origin endpoint must explicitly opt into bearer authentication;
    // --base-url alone makes Muse intentionally withhold the credential.
    transport["base_url"] = serde_json::json!(tool_surface_url(base_url, "v1"));
    transport["auth"] = serde_json::json!("bearer");
    let provider = muse_code_object(muse_code_object(&mut auth, "providers")?, "meta")?;
    provider["api_key"] = serde_json::json!(api_key);

    if let Some(models) = models {
        let models = configured_additional_tool_models("Muse Code", models)?;
        let ids = models.iter().map(|m| m.id.trim()).collect::<HashSet<_>>();
        restore_stale_muse_code_models(&settings_path, &mut settings, &ids)?;
        let previous = settings
            .get("model")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let selected = if ids.contains(previous) {
            previous.to_string()
        } else {
            models[0].id.trim().to_string()
        };
        settings["model"] = serde_json::json!(selected);
        if settings.get("model_catalog").is_none() {
            settings["model_catalog"] = serde_json::json!([]);
        }
        let rows = settings["model_catalog"]
            .as_array_mut()
            .ok_or_else(|| anyhow!("Muse Code model_catalog must be an array"))?;
        let mut seen = BTreeSet::new();
        for row in rows.iter() {
            let Some(identity @ ToolConfigFieldPath::Identified(_)) =
                tool_config_array_entry_identity(row)
                    .filter(|_| row.get("provider_id").is_some() && row.get("model_id").is_some())
            else {
                return Err(anyhow!(
                    "Muse Code model_catalog row requires provider_id and model_id"
                ));
            };
            if !seen.insert(identity) {
                return Err(anyhow!(
                    "Muse Code model_catalog contains duplicate model identities"
                ));
            }
        }
        let mut previous_rows = HashMap::new();
        rows.retain(|row| {
            if row["provider_id"] == "meta"
                && row["model_id"].as_str().is_some_and(|id| ids.contains(id))
            {
                previous_rows.insert(row["model_id"].as_str().unwrap().to_string(), row.clone());
                false
            } else {
                true
            }
        });
        for (order, model) in models.iter().enumerate() {
            let mut row = previous_rows
                .remove(model.id.trim())
                .unwrap_or_else(|| serde_json::json!({}));
            row.as_object_mut().expect("validated catalog row").extend(
                muse_code_model_row(model, order, model.id.trim() == selected)
                    .as_object()
                    .unwrap()
                    .clone(),
            );
            rows.push(row);
        }
    }
    let configured = settings
        .get("model")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|id| {
            settings
                .get("model_catalog")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|rows| {
                    rows.iter()
                        .any(|row| row["provider_id"] == "meta" && row["model_id"] == id)
                })
        });
    let mut result = ToolApplyBuilder::default();
    if models.is_some() {
        with_tool_config_file_transaction(
            "muse-code",
            &[settings_path.clone(), auth_path.clone()],
            || {
                write_json_with_backup(&settings_path, &settings, "muse-code", &mut result)?;
                write_json_with_backup(&auth_path, &auth, "muse-code", &mut result)
            },
        )?;
    } else {
        observe_json(&settings_path, &settings, &mut result)?;
        observe_json(&auth_path, &auth, &mut result)?;
    }
    let mut result = result.finish("muse-code");
    if models.is_none() {
        result.already_configured &= configured;
    }
    attach_tool_protocol(&mut result, ToolProtocol::OpenAiResponses);
    Ok(result)
}

pub(crate) fn launch_muse_code_program(api_key: &str) -> Result<(String, PathBuf)> {
    launch_muse_code_program_inner(api_key, None)
}

pub(crate) fn launch_muse_code_program_for_atomic_operation(
    api_key: &str,
    context: &ToolConfigLaunchContext<'_>,
) -> Result<(String, PathBuf)> {
    launch_muse_code_program_inner(api_key, Some(context))
}

fn launch_muse_code_program_inner(
    api_key: &str,
    context: Option<&ToolConfigLaunchContext<'_>>,
) -> Result<(String, PathBuf)> {
    launch_tool_program_with_environment_inner(
        "muse-code",
        &[("META_API_KEY", api_key)],
        &["MUSE_CUSTOM_HEADERS"],
        context,
    )
}
