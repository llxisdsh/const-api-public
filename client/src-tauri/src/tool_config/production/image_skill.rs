const IMAGE_SKILL_FILE: &str = "skills/const-api-image/SKILL.md";

static MEDIA_SKILL_TOOLS: std::sync::LazyLock<BTreeMap<String, bool>> =
    std::sync::LazyLock::new(|| {
        serde_json::from_str(include_str!("../../../../tool-media-skill-tools.json"))
            .expect("valid bundled media skill tool defaults")
    });

pub(crate) fn default_media_skill_tools() -> Vec<String> {
    MEDIA_SKILL_TOOLS
        .iter()
        .filter(|(_, enabled)| **enabled)
        .map(|(id, _)| id.clone())
        .collect()
}

pub(crate) fn media_skill_enabled(cfg: &ClientConfig, tool: &str) -> bool {
    cfg.configure_image_skill && cfg.media_skill_tools.iter().any(|id| id == tool)
}

#[tauri::command]
pub(crate) fn save_media_skill_tools(
    state: tauri::State<'_, AppState>,
    tools: Vec<String>,
) -> Result<Vec<String>, String> {
    if tools.iter().any(|id| !MEDIA_SKILL_TOOLS.contains_key(id)) {
        return Err("Unknown media skill tool".into());
    }
    let (cfg, ()) = crate::commit_config_update(&state.config_path, &state.proxy_config, |cfg| {
        cfg.media_skill_tools = tools
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Ok(())
    })
    .map_err(|err| err.to_string())?;
    Ok(cfg.media_skill_tools.clone())
}

// Do not install in ~/.agents/skills: removing one tool's configuration would
// remove the shared capability from every other configured tool.
fn tool_image_skill_paths(tool: &str) -> Result<Vec<PathBuf>> {
    if tool == "hermes" {
        return Ok(hermes_config_paths()
            .into_iter()
            .map(|path| path.with_file_name(IMAGE_SKILL_FILE))
            .collect());
    }
    let root = match tool {
        "codex" => codex_home(),
        "claude" => home_dir().join(".claude"),
        "gemini" => home_dir().join(".gemini"),
        "workbuddy" | "workbuddy-ai" => workbuddy_models_path(tool).with_file_name(""),
        "opencode" => {
            match nonempty_env_path("OPENCODE_CONFIG_DIR").filter(|_| !test_home_active()) {
                Some(dir) => resolve_tool_path_override(dir)?,
                None => xdg_tool_config_root().join("opencode"),
            }
        }
        "pi" => tool_home_override_or_default("PI_CODING_AGENT_DIR", ".pi/agent"),
        "mistral-vibe" => tool_home_override_or_default("VIBE_HOME", ".vibe"),
        "grok-build" => tool_home_override_or_default("GROK_HOME", ".grok"),
        "qwencode" => tool_home_override_or_default("QWEN_HOME", ".qwen"),
        "kimicode" => tool_home_override_or_default("KIMI_CODE_HOME", ".kimi-code"),
        "deepseek-harness" => tool_home_override_or_default("DSH_HOME", ".dsh"),
        "mimocode" => mimocode_config_path()?.with_file_name(""),
        "reasonix" => reasonix_home_path(),
        "qoder" | "qoder-cn" => qoder_settings_path(tool)?.with_file_name(""),
        _ => return Ok(Vec::new()),
    };
    Ok(vec![root.join(IMAGE_SKILL_FILE)])
}

fn tool_image_skill_content() -> Result<String> {
    let invocation = crate::image_cli::image_skill_invocation().map_err(anyhow::Error::msg)?;
    Ok(format!(
        "{}\n\n```json\n{invocation}\n```\n",
        include_str!("../const-api-image/SKILL.md")
            .trim_end()
            .replace("\r\n", "\n")
    ))
}

fn image_skill_reserved_by_user(path: &Path) -> Result<bool> {
    // Inspect parents before children: either directory may point into a shared
    // collection, or the user may already have a regular file at that name.
    let skill = path.parent().context("missing skill directory")?;
    let root = skill.parent().context("missing skills root")?;
    for (entry, directory) in [(root, true), (skill, true), (path, false)] {
        match fs::symlink_metadata(entry) {
            Ok(meta) if (directory && !meta.is_dir()) || (!directory && !meta.is_file()) => {
                return Ok(true);
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err).with_context(|| format!("inspect {}", entry.display())),
        }
    }
    Ok(false)
}

/// Shared by the explicit configure operation and its read-only preview.
pub(crate) fn sync_tool_image_skill(result: &mut ToolApplyResult, enabled: bool) -> Result<()> {
    let tool = result.tool.as_str();
    if !MEDIA_SKILL_TOOLS.contains_key(tool) {
        return Ok(());
    }
    let owned = with_tool_config_presence_files(tool, |files| {
        Ok(files
            .iter()
            .filter(|file| file.path.ends_with(IMAGE_SKILL_FILE))
            .map(|file| (file.path.clone(), file.applied_sha256.clone()))
            .collect::<Vec<_>>())
    })?;
    let mut change = if enabled {
        let mut builder = ToolApplyBuilder::default();
        let content = tool_image_skill_content()?;
        for path in tool_image_skill_paths(tool)? {
            let owned_here = owned.iter().find(|(file, _)| file == &path);
            let reserved = image_skill_reserved_by_user(&path)?
                || (path.try_exists()?
                    && match owned_here {
                        Some((_, hash)) => sha256_hex(&fs::read(&path)?) != *hash,
                        None => true,
                    });
            if reserved {
                result
                    .details
                    .insert("image_skill".into(), "user_managed".into());
                continue;
            }
            write_text_with_backup(&path, &content, tool, &mut builder)?;
        }
        builder.finish(tool)
    } else {
        if owned.is_empty() {
            return Ok(());
        }
        // A deleted or user-edited file can still have an ownership record to clear.
        result.already_configured = false;
        restore_tool_config_from_manifest_matching(tool, |file| file.ends_with(IMAGE_SKILL_FILE))?
    };
    result.already_configured &= change.file_statuses.iter().all(|status| !status.changed);
    result.files.append(&mut change.files);
    result.backups.append(&mut change.backups);
    result.file_statuses.append(&mut change.file_statuses);
    Ok(())
}
