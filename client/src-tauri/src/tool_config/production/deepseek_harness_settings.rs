// Harness's legacy settings and Cordis patches expose the same two plugin
// configs. Project only those configs for the shared semantic/ownership logic;
// keep unrelated patch entries in their YAML form.
const DEEPSEEK_HARNESS_SETTINGS_SECTIONS: [&str; 2] = ["llm-pi-ai", "agent-default-model"];

fn is_deepseek_harness_patch_path(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()) == Some("cordis.patch.yml")
}

fn deepseek_harness_has_expression(value: &serde_yaml::Value) -> bool {
    match value {
        serde_yaml::Value::Tagged(value) => value.tag == "const-api-dsh-js",
        serde_yaml::Value::Sequence(values) => values.iter().any(deepseek_harness_has_expression),
        serde_yaml::Value::Mapping(values) => values.iter().any(|(key, value)| {
            deepseek_harness_has_expression(key) || deepseek_harness_has_expression(value)
        }),
        _ => false,
    }
}

fn deepseek_harness_patch_entries(raw: &str) -> Result<Vec<serde_yaml::Value>> {
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    // serde_yaml discards nonstandard global tags. Harness evaluates !!js at
    // runtime, so never rewrite such a file as if expressions were plain text.
    // A temporary local tag retains the expression's type for inspection only.
    // Comments (including Harness's default template) and quoted strings that
    // merely mention !!js must not prevent normal configuration.
    let tagged = raw
        .replace("!<tag:yaml.org,2002:js>", "!const-api-dsh-js")
        .replace("!!js", "!const-api-dsh-js");
    let uses_tag_directive = raw.lines().any(|line| line.starts_with("%TAG "));
    if uses_tag_directive
        || (tagged != raw && deepseek_harness_has_expression(&serde_yaml::from_str(&tagged)?))
    {
        return Err(anyhow!(
            "DeepSeek Harness cordis.patch.yml uses dynamic YAML tags; configure it manually to preserve their behavior"
        ));
    }
    match serde_yaml::from_str::<serde_yaml::Value>(raw)
        .context("parse DeepSeek Harness cordis.patch.yml")?
    {
        serde_yaml::Value::Sequence(entries) => Ok(entries),
        _ => Err(anyhow!(
            "DeepSeek Harness cordis.patch.yml must contain a YAML sequence"
        )),
    }
}

fn deepseek_harness_patch_matches(entry: &serde_yaml::Value, section: &str) -> bool {
    entry.get("id").and_then(serde_yaml::Value::as_str) == Some(section)
        && entry.get("insert").is_none()
        && entry.get("name").is_none_or(|name| {
            name.as_str() == Some(format!("@deepseek-ai/dsh-{section}").as_str())
        })
}

fn deepseek_harness_patch_settings(raw: &str) -> Result<serde_yaml::Mapping> {
    let mut settings = serde_yaml::Mapping::new();
    for entry in deepseek_harness_patch_entries(raw)? {
        for section in DEEPSEEK_HARNESS_SETTINGS_SECTIONS {
            if !deepseek_harness_patch_matches(&entry, section) {
                continue;
            }
            if let Some(config) = entry.get("config") {
                if !config.is_mapping() {
                    return Err(anyhow!(
                        "DeepSeek Harness {section} config must be a YAML mapping; dynamic config cannot be edited automatically"
                    ));
                }
                settings.insert(yaml_key(section), config.clone());
            }
        }
    }
    Ok(settings)
}

fn render_deepseek_harness_patch(raw: &str, settings: &serde_yaml::Mapping) -> Result<String> {
    let mut entries = deepseek_harness_patch_entries(raw)?;
    for section in DEEPSEEK_HARNESS_SETTINGS_SECTIONS {
        let index = entries.iter().rposition(|entry| {
            deepseek_harness_patch_matches(entry, section) && entry.get("config").is_some()
        });
        match (index, settings.get(yaml_key(section))) {
            (Some(index), Some(config)) => entries[index]["config"] = config.clone(),
            (Some(index), None) => {
                let row = entries[index].as_mapping_mut().ok_or_else(|| {
                    anyhow!("DeepSeek Harness patch entry must be a YAML mapping")
                })?;
                row.remove(yaml_key("config"));
                if row
                    .keys()
                    .all(|key| matches!(key.as_str(), Some("id" | "name")))
                {
                    entries.remove(index);
                }
            }
            (None, Some(config)) => {
                let mut row = serde_yaml::Mapping::new();
                row.insert(yaml_key("id"), yaml_string(section));
                row.insert(yaml_key("config"), config.clone());
                entries.push(serde_yaml::Value::Mapping(row));
            }
            (None, None) => {}
        }
    }
    Ok(serde_yaml::to_string(&entries)?)
}

struct DeepseekHarnessSettingsFile {
    path: PathBuf,
    sections: Vec<&'static str>,
}

fn deepseek_harness_profile_patch_path() -> PathBuf {
    tool_home_override_or_default("DSH_HOME", ".dsh")
        .join("profiles")
        .join("web")
        .join("cordis.patch.yml")
}

fn deepseek_harness_settings_files() -> Result<Vec<DeepseekHarnessSettingsFile>> {
    let (legacy, _) = deepseek_harness_config_paths();
    let legacy_file = || {
        vec![DeepseekHarnessSettingsFile {
            path: legacy.clone(),
            sections: DEEPSEEK_HARNESS_SETTINGS_SECTIONS.to_vec(),
        }]
    };
    if legacy.exists() {
        return Ok(legacy_file());
    }
    let root = tool_home_override_or_default("DSH_HOME", ".dsh");
    let home_patch = root.join("cordis.patch.yml");
    let profile_patch = deepseek_harness_profile_patch_path();
    let home = deepseek_harness_patch_settings(&read_text_or_empty(&home_patch)?)?;
    let profile = deepseek_harness_patch_settings(&read_text_or_empty(&profile_patch)?)?;
    if home.is_empty() && profile.is_empty() && !root.join("settings.yaml.imported").exists() {
        // Fresh installations: old builds read this directly; current new builds
        // import it themselves. Their resulting files select patches next time.
        return Ok(legacy_file());
    }
    // Config overrides replace a whole plugin config. Edit each section at its
    // effective layer, rather than copying inherited user settings into home.
    let (home_sections, profile_sections): (Vec<_>, Vec<_>) = DEEPSEEK_HARNESS_SETTINGS_SECTIONS
        .into_iter()
        .partition(|section| home.contains_key(yaml_key(section)));
    Ok([
        (profile_patch, profile_sections),
        (home_patch, home_sections),
    ]
    .into_iter()
    .filter(|(_, sections)| !sections.is_empty())
    .map(|(path, sections)| DeepseekHarnessSettingsFile { path, sections })
    .collect())
}

fn deepseek_harness_settings_text(path: &Path) -> Result<String> {
    let raw = read_text_or_empty(path)?;
    if !is_deepseek_harness_patch_path(path) {
        return Ok(raw);
    }
    Ok(serde_yaml::to_string(&deepseek_harness_patch_settings(
        &raw,
    )?)?)
}

fn deepseek_harness_effective_settings(files: &[DeepseekHarnessSettingsFile]) -> Result<String> {
    let mut settings = serde_yaml::Mapping::new();
    for file in files {
        let local = parse_deepseek_harness_yaml_mapping(
            &deepseek_harness_settings_text(&file.path)?,
            "settings",
        )?;
        if !is_deepseek_harness_patch_path(&file.path) {
            return Ok(serde_yaml::to_string(&local)?);
        }
        for section in &file.sections {
            if let Some(config) = local.get(yaml_key(section)) {
                settings.insert(yaml_key(section), config.clone());
            }
        }
    }
    Ok(serde_yaml::to_string(&settings)?)
}

fn render_deepseek_harness_settings(
    file: &DeepseekHarnessSettingsFile,
    settings: &str,
) -> Result<String> {
    if is_deepseek_harness_patch_path(&file.path) {
        let raw = read_text_or_empty(&file.path)?;
        let mut local = deepseek_harness_patch_settings(&raw)?;
        let updated = parse_deepseek_harness_yaml_mapping(settings, "settings")?;
        for section in &file.sections {
            if let Some(config) = updated.get(yaml_key(section)) {
                local.insert(yaml_key(section), config.clone());
            }
        }
        render_deepseek_harness_patch(&raw, &local)
    } else {
        Ok(settings.to_string())
    }
}

fn parse_tool_config_document_at(
    path: &Path,
    format: ToolConfigFormat,
    content: Option<&[u8]>,
) -> Result<serde_json::Value> {
    if is_deepseek_harness_patch_path(path) {
        let raw = std::str::from_utf8(content.unwrap_or_default())?;
        // Legacy ownership snapshots remain mappings after Harness moves the
        // live settings into a patch; keep the original evidence unchanged.
        if matches!(
            serde_yaml::from_str::<serde_yaml::Value>(raw)?,
            serde_yaml::Value::Mapping(_)
        ) {
            return parse_tool_config_document(format, content);
        }
        return Ok(serde_json::to_value(deepseek_harness_patch_settings(raw)?)?);
    }
    parse_tool_config_document(format, content)
}

fn migrate_deepseek_harness_settings_ownership(manifest: &mut ToolConfigManifest) {
    let (legacy, _) = deepseek_harness_config_paths();
    let legacy_key = manifest_file_key(&legacy);
    if legacy.exists()
        || !manifest
            .files
            .get(&legacy_key)
            .is_some_and(|entry| entry.tool == "deepseek-harness")
        || !legacy.with_file_name("settings.yaml.imported").exists()
    {
        return;
    }
    // The official importer always writes into the active profile, not home.
    let patch = deepseek_harness_profile_patch_path();
    if !patch.exists() {
        return;
    }
    let patch_key = manifest_file_key(&patch);
    if manifest.files.contains_key(&patch_key) {
        return;
    }
    if let Some(mut ownership) = manifest.files.remove(&legacy_key) {
        ownership.whole_file_restore_safe = false;
        manifest.files.insert(patch_key, ownership);
    }
}
