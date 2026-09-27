fn const_api_state_path(filename: &str) -> PathBuf {
    crate::client_data_root().join(filename)
}

pub(crate) fn backup_if_exists(path: &Path, tool: &str, backups: &mut Vec<String>) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let filename = path
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("config");
    // Different configuration roots often contain the same filename (Claude
    // Desktop has two claude_desktop_config.json files). Include a stable path
    // fingerprint so retention and restore diagnostics cannot mix their backup
    // series.
    let path_fingerprint = sha256_hex(path.to_string_lossy().as_bytes());
    let series_filename = format!("{}-{filename}", &path_fingerprint[..12]);
    let directory = backup_root().join(tool);
    fs::create_dir_all(&directory)?;
    let retention = backup_retention_for_filename(&series_filename);
    // Keep pre-fingerprint backup series bounded as well. They cannot be
    // attributed to one of several same-named source paths safely, so retain
    // them as a separate legacy series rather than guessing.
    prune_backup_series(&directory, filename, retention)?;
    let existing = backup_series_files(&directory, &series_filename)?;
    if let Some((_, latest)) = existing.last() {
        if files_have_same_content(path, latest)? {
            prune_backup_series(&directory, &series_filename, retention)?;
            return Ok(());
        }
    }

    let mut stamp = chrono_like_stamp().parse::<u128>().unwrap_or_default();
    let mut backup = directory.join(format!("{stamp}-{series_filename}"));
    while backup.exists() {
        stamp = stamp.saturating_add(1);
        backup = directory.join(format!("{stamp}-{series_filename}"));
    }
    fs::copy(path, &backup)?;
    backups.push(backup.display().to_string());
    prune_backup_series(&directory, &series_filename, retention)?;
    Ok(())
}

fn backup_retention_for_filename(filename: &str) -> usize {
    let filename = filename.to_ascii_lowercase();
    if filename.ends_with(".db")
        || filename.ends_with(".sqlite")
        || filename.ends_with(".sqlite3")
        || filename.ends_with("-wal")
        || filename.ends_with("-shm")
    {
        DATABASE_BACKUP_RETENTION
    } else {
        CONFIG_BACKUP_RETENTION
    }
}

fn backup_name_parts(path: &Path) -> Option<(u128, String)> {
    let name = path.file_name()?.to_str()?;
    let (stamp, filename) = name.split_once('-')?;
    if filename.is_empty() || !stamp.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some((stamp.parse().ok()?, filename.to_string()))
}

fn backup_series_files(directory: &Path, filename: &str) -> Result<Vec<(u128, PathBuf)>> {
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(directory)
        .with_context(|| format!("read backup directory {}", directory.display()))?
    {
        let path = entry?.path();
        let Some((stamp, source_name)) = backup_name_parts(&path) else {
            continue;
        };
        if source_name == filename {
            files.push((stamp, path));
        }
    }
    files.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    Ok(files)
}

fn files_have_same_content(left: &Path, right: &Path) -> Result<bool> {
    if fs::metadata(left)?.len() != fs::metadata(right)?.len() {
        return Ok(false);
    }
    let mut left = fs::File::open(left)?;
    let mut right = fs::File::open(right)?;
    let mut left_buffer = [0_u8; 64 * 1024];
    let mut right_buffer = [0_u8; 64 * 1024];
    loop {
        let left_read = left.read(&mut left_buffer)?;
        let right_read = right.read(&mut right_buffer)?;
        if left_read != right_read || left_buffer[..left_read] != right_buffer[..right_read] {
            return Ok(false);
        }
        if left_read == 0 {
            return Ok(true);
        }
    }
}

fn prune_backup_series(directory: &Path, filename: &str, retention: usize) -> Result<()> {
    let files = backup_series_files(directory, filename)?;
    let remove_count = files.len().saturating_sub(retention);
    for (_, path) in files.into_iter().take(remove_count) {
        fs::remove_file(&path)
            .with_context(|| format!("remove expired backup {}", path.display()))?;
    }
    Ok(())
}

pub(crate) fn cleanup_backup_retention() -> Result<()> {
    let root = backup_root();
    if !root.exists() {
        return Ok(());
    }
    for tool_entry in
        fs::read_dir(&root).with_context(|| format!("read backup root {}", root.display()))?
    {
        let tool_dir = tool_entry?.path();
        if !tool_dir.is_dir() {
            continue;
        }
        let mut filenames = HashSet::new();
        for entry in fs::read_dir(&tool_dir)? {
            if let Some((_, filename)) = backup_name_parts(&entry?.path()) {
                filenames.insert(filename);
            }
        }
        for filename in filenames {
            prune_backup_series(
                &tool_dir,
                &filename,
                backup_retention_for_filename(&filename),
            )?;
        }
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn file_status_for(path: &Path, content: &[u8]) -> Result<ToolFileStatus> {
    let exists_before = path.exists();
    let before = if exists_before {
        Some(fs::read(path).with_context(|| format!("read {}", path.display()))?)
    } else {
        None
    };
    let before_sha256 = before.as_deref().map(sha256_hex).unwrap_or_default();
    let after_sha256 = sha256_hex(content);
    let already_configured = before.as_deref() == Some(content);
    Ok(ToolFileStatus {
        path: path.display().to_string(),
        exists_before,
        changed: !already_configured,
        already_configured,
        before_sha256,
        after_sha256,
    })
}

// Status checks compare structured configuration by meaning, not serialized bytes.
// Tools are free to reorder keys, retain comments, change indentation, or omit a
// trailing newline after launch. Plain text files retain exact-byte comparison.
fn observe_text(path: &Path, content: &str, result: &mut ToolApplyBuilder) -> Result<()> {
    result
        .file_statuses
        .push(semantic_file_status_for(path, content.as_bytes())?);
    Ok(())
}

fn observe_json(
    path: &Path,
    value: &serde_json::Value,
    result: &mut ToolApplyBuilder,
) -> Result<()> {
    let content = serde_json::to_string_pretty(value)? + "\n";
    observe_text(path, &content, result)
}

fn semantic_file_status_for(path: &Path, content: &[u8]) -> Result<ToolFileStatus> {
    let exists_before = path.exists();
    let before = if exists_before {
        Some(fs::read(path).with_context(|| format!("read {}", path.display()))?)
    } else {
        None
    };
    let before_sha256 = before.as_deref().map(sha256_hex).unwrap_or_default();
    let after_sha256 = sha256_hex(content);
    let format = tool_config_format(path);
    let already_configured = match before.as_deref() {
        Some(before) if is_deepseek_harness_patch_path(path) => {
            parse_tool_config_document_at(path, format, Some(before))?
                == parse_tool_config_document_at(path, format, Some(content))?
        }
        Some(before) if format != ToolConfigFormat::Text => {
            parse_tool_config_status_document(format, before).with_context(|| {
                format!("parse current tool configuration {}", path.display())
            })? == parse_tool_config_status_document(format, content).with_context(|| {
                format!("parse expected tool configuration {}", path.display())
            })?
        }
        Some(before) => before == content,
        None => false,
    };
    Ok(ToolFileStatus {
        path: path.display().to_string(),
        exists_before,
        changed: !already_configured,
        already_configured,
        before_sha256,
        after_sha256,
    })
}

fn atomic_write(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("create temp file beside {}", path.display()))?;
    temp.write_all(content)
        .with_context(|| format!("write temp file for {}", path.display()))?;
    temp.flush()
        .with_context(|| format!("flush temp file for {}", path.display()))?;
    temp.as_file()
        .sync_all()
        .with_context(|| format!("sync temp file for {}", path.display()))?;
    temp.persist(path)
        .map_err(|err| err.error)
        .with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

fn restore_file_snapshot(path: &Path, content: Option<&[u8]>) -> Result<()> {
    match content {
        Some(content) => atomic_write(path, content),
        None if path.exists() => {
            fs::remove_file(path).with_context(|| format!("rollback {}", path.display()))
        }
        None => Ok(()),
    }
}

fn with_tool_config_file_transaction<T>(
    tool: &str,
    paths: &[PathBuf],
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    if tool_config_preview_active() {
        // Preview performs no writes, so rollback would only introduce the
        // side effects this mode is designed to avoid.
        return operation();
    }
    let original_tool_ownership = {
        let _guard = tool_config_manifest_lock()
            .lock()
            .map_err(|_| anyhow!("TOOL_CONFIG_MANIFEST_LOCK_POISONED: manifest lock poisoned"))?;
        load_tool_config_manifest()?
            .files
            .into_iter()
            .filter(|(_, ownership)| ownership.tool == tool)
            .collect::<HashMap<_, _>>()
    };
    let mut unique_paths = Vec::new();
    for path in paths
        .iter()
        .cloned()
        .chain(original_tool_ownership.keys().map(PathBuf::from))
    {
        if !unique_paths.iter().any(|existing| existing == &path) {
            unique_paths.push(path);
        }
    }
    let snapshots = unique_paths
        .iter()
        .map(|path| Ok((path.clone(), read_optional_file(path)?)))
        .collect::<Result<Vec<_>>>()?;

    match operation() {
        Ok(value) => Ok(value),
        Err(operation_error) => {
            let mut rollback_errors = Vec::new();
            for (path, content) in snapshots.iter().rev() {
                if let Err(error) = restore_file_snapshot(path, content.as_deref()) {
                    rollback_errors.push(format!("{}: {error}", path.display()));
                }
            }
            let manifest_rollback = (|| -> Result<()> {
                let _guard = tool_config_manifest_lock().lock().map_err(|_| {
                    anyhow!("TOOL_CONFIG_MANIFEST_LOCK_POISONED: manifest lock poisoned")
                })?;
                let mut manifest = load_tool_config_manifest()?;
                // Restore only this tool's ownership records. Other tools may
                // complete concurrently and their manifest entries must not be
                // replaced by this transaction's older snapshot.
                manifest
                    .files
                    .retain(|_, ownership| ownership.tool != tool);
                manifest.files.extend(original_tool_ownership);
                save_tool_config_manifest(&manifest)
            })();
            if let Err(error) = manifest_rollback {
                rollback_errors.push(format!("tool config manifest: {error}"));
            }
            if rollback_errors.is_empty() {
                Err(operation_error)
            } else {
                Err(anyhow!(
                    "{operation_error}; configuration rollback also failed: {}",
                    rollback_errors.join("; ")
                ))
            }
        }
    }
}

fn tool_config_manifest_path() -> PathBuf {
    crate::client_data_root().join(TOOL_CONFIG_MANIFEST)
}

fn tool_config_manifest_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn load_tool_config_manifest() -> Result<ToolConfigManifest> {
    let path = tool_config_manifest_path();
    if !path.exists() {
        return Ok(ToolConfigManifest {
            version: 2,
            ..Default::default()
        });
    }
    let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let mut manifest = serde_json::from_str::<ToolConfigManifest>(&raw)
        .with_context(|| format!("parse {}", path.display()))?;
    manifest.version = 2;
    migrate_deepseek_harness_settings_ownership(&mut manifest);
    Ok(manifest)
}

fn save_tool_config_manifest(manifest: &ToolConfigManifest) -> Result<()> {
    let path = tool_config_manifest_path();
    let mut content = serde_json::to_vec_pretty(manifest)?;
    content.push(b'\n');
    atomic_write(&path, &content).with_context(|| format!("save {}", path.display()))
}

fn manifest_manages_tool(tool: &str) -> bool {
    matches!(
        tool,
        "codex"
            | "claude"
            | "claude-desktop"
            | "claude-science"
            | "gemini"
            | "opencode"
            | "openclaw"
            | "hermes"
            | "vscode"
            | "workbuddy"
            | "copilot"
            | "raven"
            | "pi"
            | "cline"
            | "reasonix"
            | "deepseek-harness"
            | "open-interpreter"
            | "anythingllm"
            | "goose"
            | "mistral-vibe"
            | "open-design"
            | "kimicode"
            | "mimocode"
            | "qwencode"
            | "openscience"
            | "vibe-trading"
            | "zcode"
    )
}

fn manifest_file_key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn read_optional_file(path: &Path) -> Result<Option<Vec<u8>>> {
    if path.exists() {
        fs::read(path)
            .map(Some)
            .with_context(|| format!("read {}", path.display()))
    } else {
        Ok(None)
    }
}

fn tool_config_format(path: &Path) -> ToolConfigFormat {
    if path.file_name().and_then(|name| name.to_str()) == Some(".env") {
        return ToolConfigFormat::Env;
    }
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "json" | "json5" | "jsonc" => ToolConfigFormat::Json,
        "toml" => ToolConfigFormat::Toml,
        "env" => ToolConfigFormat::Env,
        "yaml" | "yml" => ToolConfigFormat::Yaml,
        _ => ToolConfigFormat::Text,
    }
}

fn parse_tool_config_document(
    format: ToolConfigFormat,
    content: Option<&[u8]>,
) -> Result<serde_json::Value> {
    let Some(content) = content.filter(|content| !content.is_empty()) else {
        return Ok(serde_json::json!({}));
    };
    let raw = std::str::from_utf8(content).context("tool config is not valid UTF-8")?;
    match format {
        ToolConfigFormat::Json => {
            json5::from_str(raw).context("parse JSON tool config for ownership")
        }
        ToolConfigFormat::Toml => {
            let doc = raw
                .parse::<DocumentMut>()
                .context("parse TOML tool config for ownership")?;
            Ok(toml_table_to_owned_json(doc.as_table()))
        }
        ToolConfigFormat::Env => Ok(env_text_to_owned_json(raw)),
        ToolConfigFormat::Yaml => {
            let yaml = serde_yaml::from_str::<serde_yaml::Value>(raw)
                .context("parse YAML tool config for ownership")?;
            serde_json::to_value(yaml).context("normalize YAML tool config for ownership")
        }
        ToolConfigFormat::Text => Ok(serde_json::Value::String(raw.to_string())),
    }
}

fn parse_tool_config_status_document(
    format: ToolConfigFormat,
    content: &[u8],
) -> Result<serde_json::Value> {
    if content.is_empty() {
        return Ok(serde_json::json!({}));
    }
    let raw = std::str::from_utf8(content).context("tool config is not valid UTF-8")?;
    match format {
        ToolConfigFormat::Toml => {
            toml_edit::de::from_str(raw).context("parse TOML tool config for status")
        }
        ToolConfigFormat::Env => Ok(env_text_to_status_json(raw)),
        _ => parse_tool_config_document(format, Some(content)),
    }
}

fn env_text_to_status_json(raw: &str) -> serde_json::Value {
    let mut values = serde_json::Map::new();
    for line in raw.lines() {
        let trimmed = line.trim_start();
        let trimmed = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty()
            || !key
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            continue;
        }
        let value = value.trim();
        let value = if value.len() >= 2
            && ((value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\'')))
        {
            &value[1..value.len() - 1]
        } else {
            value
        };
        values.insert(
            key.to_string(),
            serde_json::Value::String(value.to_string()),
        );
    }
    serde_json::Value::Object(values)
}

fn env_text_to_owned_json(raw: &str) -> serde_json::Value {
    let mut values = serde_json::Map::new();
    for line in raw.lines() {
        let trimmed = line.trim_start();
        let trimmed = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !key.is_empty()
            && key
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            values.insert(
                key.to_string(),
                serde_json::Value::String(value.to_string()),
            );
        }
    }
    serde_json::Value::Object(values)
}

fn toml_table_to_owned_json(table: &Table) -> serde_json::Value {
    let values = table
        .iter()
        .filter_map(|(key, item)| {
            toml_item_to_owned_json(item).map(|value| (key.to_string(), value))
        })
        .collect::<serde_json::Map<_, _>>();
    serde_json::Value::Object(values)
}

fn toml_item_to_owned_json(item: &Item) -> Option<serde_json::Value> {
    match item {
        Item::None => None,
        Item::Table(table) => Some(toml_table_to_owned_json(table)),
        Item::Value(value) => Some(serde_json::Value::String(format!(
            "{TOML_VALUE_PREFIX}{value}"
        ))),
        Item::ArrayOfTables(array) => Some(serde_json::Value::Array(
            array.iter().map(toml_table_to_owned_json).collect(),
        )),
    }
}

fn named_array_parts(
    value: Option<&serde_json::Value>,
) -> Option<(
    BTreeMap<String, &serde_json::Value>,
    Vec<&serde_json::Value>,
)> {
    let Some(value) = value else {
        return Some((BTreeMap::new(), Vec::new()));
    };
    let array = value.as_array()?;
    let mut named = BTreeMap::new();
    let mut unnamed = Vec::new();
    for entry in array {
        let name = entry
            .get("name")
            .and_then(serde_json::Value::as_str)
            .filter(|name| !name.is_empty());
        let Some(name) = name else {
            unnamed.push(entry);
            continue;
        };
        if named.insert(name.to_string(), entry).is_some() {
            return None;
        }
    }
    Some((named, unnamed))
}

fn named_array_entry<'a>(
    value: Option<&'a serde_json::Value>,
    name: &str,
) -> Option<&'a serde_json::Value> {
    value?
        .as_array()?
        .iter()
        .find(|entry| entry.get("name").and_then(serde_json::Value::as_str) == Some(name))
}

fn collect_tool_config_owned_fields(
    original: Option<&serde_json::Value>,
    applied: Option<&serde_json::Value>,
    path: &mut Vec<ToolConfigFieldPath>,
    fields: &mut Vec<ToolConfigOwnedField>,
) {
    if original == applied {
        return;
    }
    // A named array entry created by CONST API is owned as one logical item.
    // Tools may append runtime metadata to that item later; cancellation can
    // still remove the entry by its stable name without touching siblings.
    if matches!(path.last(), Some(ToolConfigFieldPath::Named(_)))
        && (original.is_none() || applied.is_none())
    {
        fields.push(ToolConfigOwnedField {
            path: path.clone(),
            original: original
                .cloned()
                .map(ToolConfigFieldValue::Value)
                .unwrap_or(ToolConfigFieldValue::Missing),
            applied: applied
                .cloned()
                .map(ToolConfigFieldValue::Value)
                .unwrap_or(ToolConfigFieldValue::Missing),
        });
        return;
    }
    let original_object = original.and_then(serde_json::Value::as_object);
    let applied_object = applied.and_then(serde_json::Value::as_object);
    if (original.is_none() || original_object.is_some())
        && (applied.is_none() || applied_object.is_some())
        // An added/removed empty object is itself a change. There are no child
        // fields to record, so let it fall through to the leaf ownership rule.
        && !(original.is_none() && applied_object.is_some_and(serde_json::Map::is_empty))
        && !(applied.is_none() && original_object.is_some_and(serde_json::Map::is_empty))
    {
        let mut keys = BTreeSet::new();
        if let Some(object) = original_object {
            keys.extend(object.keys().cloned());
        }
        if let Some(object) = applied_object {
            keys.extend(object.keys().cloned());
        }
        for key in keys {
            path.push(ToolConfigFieldPath::Key(key.clone()));
            collect_tool_config_owned_fields(
                original_object.and_then(|object| object.get(&key)),
                applied_object.and_then(|object| object.get(&key)),
                path,
                fields,
            );
            path.pop();
        }
        return;
    }

    if let (Some((original_named, original_unnamed)), Some((applied_named, applied_unnamed))) =
        (named_array_parts(original), named_array_parts(applied))
    {
        // Structured tool configs often mix named provider entries with
        // legacy/anonymous entries. The anonymous slice belongs to the tool;
        // when it is unchanged, track only the named entries CONST API
        // actually touched instead of claiming the entire array.
        if original_unnamed != applied_unnamed {
            fields.push(ToolConfigOwnedField {
                path: path.clone(),
                original: original
                    .cloned()
                    .map(ToolConfigFieldValue::Value)
                    .unwrap_or(ToolConfigFieldValue::Missing),
                applied: applied
                    .cloned()
                    .map(ToolConfigFieldValue::Value)
                    .unwrap_or(ToolConfigFieldValue::Missing),
            });
            return;
        }
        let names = original_named
            .keys()
            .chain(applied_named.keys())
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        for name in names {
            path.push(ToolConfigFieldPath::Named(name.clone()));
            collect_tool_config_owned_fields(
                original_named.get(&name).copied(),
                applied_named.get(&name).copied(),
                path,
                fields,
            );
            path.pop();
        }
        return;
    }

    fields.push(ToolConfigOwnedField {
        path: path.clone(),
        original: original
            .cloned()
            .map(ToolConfigFieldValue::Value)
            .unwrap_or(ToolConfigFieldValue::Missing),
        applied: applied
            .cloned()
            .map(ToolConfigFieldValue::Value)
            .unwrap_or(ToolConfigFieldValue::Missing),
    });
}

fn owned_field_json_value(value: &ToolConfigFieldValue) -> Option<&serde_json::Value> {
    match value {
        ToolConfigFieldValue::Missing => None,
        ToolConfigFieldValue::Value(value) => Some(value),
    }
}

fn owned_field_is_added_named_entry(field: &ToolConfigOwnedField) -> bool {
    matches!(field.original, ToolConfigFieldValue::Missing)
        && matches!(field.path.last(), Some(ToolConfigFieldPath::Named(_)))
}

fn refine_tool_config_owned_fields(
    fields: Vec<ToolConfigOwnedField>,
) -> Vec<ToolConfigOwnedField> {
    let mut refined = Vec::new();
    for field in fields {
        let mut path = field.path.clone();
        collect_tool_config_owned_fields(
            owned_field_json_value(&field.original),
            owned_field_json_value(&field.applied),
            &mut path,
            &mut refined,
        );
    }
    refined
}

fn tool_config_owned_fields(
    format: ToolConfigFormat,
    original: Option<&[u8]>,
    applied: &[u8],
) -> Result<Vec<ToolConfigOwnedField>> {
    if original == Some(applied) {
        return Ok(Vec::new());
    }
    if format == ToolConfigFormat::Text {
        return Ok(vec![ToolConfigOwnedField {
            path: Vec::new(),
            original: original
                .map(|content| {
                    ToolConfigFieldValue::Value(serde_json::Value::String(
                        String::from_utf8_lossy(content).into_owned(),
                    ))
                })
                .unwrap_or(ToolConfigFieldValue::Missing),
            applied: ToolConfigFieldValue::Value(serde_json::Value::String(
                String::from_utf8_lossy(applied).into_owned(),
            )),
        }]);
    }
    let original = parse_tool_config_document(format, original)?;
    let applied = parse_tool_config_document(format, Some(applied))?;
    let mut fields = Vec::new();
    collect_tool_config_owned_fields(
        Some(&original),
        Some(&applied),
        &mut Vec::new(),
        &mut fields,
    );
    Ok(fields)
}

fn tool_config_field_value_at(
    root: &serde_json::Value,
    path: &[ToolConfigFieldPath],
) -> ToolConfigFieldValue {
    let mut current = root;
    for segment in path {
        current = match segment {
            ToolConfigFieldPath::Key(key) => match current.get(key) {
                Some(value) => value,
                None => return ToolConfigFieldValue::Missing,
            },
            ToolConfigFieldPath::Named(name) => match named_array_entry(Some(current), name) {
                Some(value) => value,
                None => return ToolConfigFieldValue::Missing,
            },
        };
    }
    ToolConfigFieldValue::Value(current.clone())
}

fn set_tool_config_field_value(
    root: &mut serde_json::Value,
    path: &[ToolConfigFieldPath],
    value: &ToolConfigFieldValue,
) {
    if path.is_empty() {
        if let ToolConfigFieldValue::Value(value) = value {
            *root = value.clone();
        }
        return;
    }
    match &path[0] {
        ToolConfigFieldPath::Key(key) => {
            let Some(object) = root.as_object_mut() else {
                return;
            };
            if path.len() == 1 {
                match value {
                    ToolConfigFieldValue::Missing => {
                        object.remove(key);
                    }
                    ToolConfigFieldValue::Value(value) => {
                        object.insert(key.clone(), value.clone());
                    }
                }
                return;
            }
            if !object.contains_key(key) {
                if matches!(value, ToolConfigFieldValue::Missing) {
                    return;
                }
                let container = if matches!(path.get(1), Some(ToolConfigFieldPath::Named(_))) {
                    serde_json::json!([])
                } else {
                    serde_json::json!({})
                };
                object.insert(key.clone(), container);
            }
            if let Some(next) = object.get_mut(key) {
                set_tool_config_field_value(next, &path[1..], value);
            }
        }
        ToolConfigFieldPath::Named(name) => {
            let Some(array) = root.as_array_mut() else {
                return;
            };
            let position = array.iter().position(|entry| {
                entry.get("name").and_then(serde_json::Value::as_str) == Some(name.as_str())
            });
            if path.len() == 1 {
                match (position, value) {
                    (Some(position), ToolConfigFieldValue::Missing) => {
                        array.remove(position);
                    }
                    (Some(position), ToolConfigFieldValue::Value(value)) => {
                        array[position] = value.clone();
                    }
                    (None, ToolConfigFieldValue::Value(value)) => array.push(value.clone()),
                    (None, ToolConfigFieldValue::Missing) => {}
                }
                return;
            }
            let position = match position {
                Some(position) => position,
                None if matches!(value, ToolConfigFieldValue::Missing) => return,
                None => {
                    array.push(serde_json::json!({"name": name}));
                    array.len() - 1
                }
            };
            set_tool_config_field_value(&mut array[position], &path[1..], value);
        }
    }
}

fn cleanup_created_empty_json_containers(
    value: &mut serde_json::Value,
    original: Option<&serde_json::Value>,
) {
    match value {
        serde_json::Value::Object(object) => {
            let keys = object.keys().cloned().collect::<Vec<_>>();
            for key in keys {
                let original_value = original
                    .and_then(serde_json::Value::as_object)
                    .and_then(|object| object.get(&key));
                let Some(child) = object.get_mut(&key) else {
                    continue;
                };
                cleanup_created_empty_json_containers(child, original_value);
                let remove = match child {
                    serde_json::Value::Object(child) => child.is_empty(),
                    serde_json::Value::Array(child) => child.is_empty(),
                    _ => false,
                } && original_value.is_none();
                if remove {
                    object.remove(&key);
                }
            }
        }
        serde_json::Value::Array(array) => {
            for entry in array.iter_mut() {
                let original_entry = entry
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|name| named_array_entry(original, name));
                cleanup_created_empty_json_containers(entry, original_entry);
            }
            array.retain(|entry| {
                let placeholder_name = entry
                    .as_object()
                    .filter(|object| object.len() == 1)
                    .and_then(|object| object.get("name"))
                    .and_then(serde_json::Value::as_str);
                placeholder_name
                    .map(|name| named_array_entry(original, name).is_some())
                    .unwrap_or(true)
            });
        }
        _ => {}
    }
}

fn tool_config_document_is_empty(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(object) => object.values().all(tool_config_document_is_empty),
        serde_json::Value::Array(array) => array.iter().all(tool_config_document_is_empty),
        serde_json::Value::String(value) => value.is_empty(),
        serde_json::Value::Null => true,
        _ => false,
    }
}

fn toml_item_from_owned_value(value: &serde_json::Value) -> Result<Item> {
    let encoded = value
        .as_str()
        .and_then(|value| value.strip_prefix(TOML_VALUE_PREFIX))
        .ok_or_else(|| anyhow!("invalid TOML manifest field value"))?;
    let mut doc = format!("value = {encoded}\n")
        .parse::<DocumentMut>()
        .context("parse TOML manifest field value")?;
    doc.as_table_mut()
        .remove("value")
        .ok_or_else(|| anyhow!("TOML manifest field value is missing"))
}

fn toml_field_value_at(doc: &DocumentMut, path: &[ToolConfigFieldPath]) -> ToolConfigFieldValue {
    let mut item = doc.as_item();
    for segment in path {
        let ToolConfigFieldPath::Key(key) = segment else {
            return ToolConfigFieldValue::Missing;
        };
        let Some(next) = item.get(key) else {
            return ToolConfigFieldValue::Missing;
        };
        item = next;
    }
    toml_item_to_owned_json(item)
        .map(ToolConfigFieldValue::Value)
        .unwrap_or(ToolConfigFieldValue::Missing)
}

fn toml_field_values_match(
    current: &ToolConfigFieldValue,
    expected: &ToolConfigFieldValue,
) -> Result<bool> {
    if current == expected {
        return Ok(true);
    }
    // The manifest keeps original TOML spelling so it can restore it, but quote,
    // whitespace and numeric formatting changes do not relinquish field ownership.
    let decode = |field: &ToolConfigFieldValue| -> Result<Option<serde_json::Value>> {
        let ToolConfigFieldValue::Value(value) = field else {
            return Ok(None);
        };
        let Some(raw) = value.as_str().and_then(|value| value.strip_prefix(TOML_VALUE_PREFIX)) else {
            return Ok(None);
        };
        toml_edit::de::from_str(&format!("value = {raw}"))
            .map(Some)
            .context("parse TOML value for managed-field comparison")
    };
    Ok(match (decode(current)?, decode(expected)?) {
        (Some(current), Some(expected)) => current == expected,
        _ => false,
    })
}

fn set_toml_field_value(
    table: &mut Table,
    path: &[ToolConfigFieldPath],
    value: &ToolConfigFieldValue,
) -> Result<()> {
    let Some(ToolConfigFieldPath::Key(key)) = path.first() else {
        return Ok(());
    };
    if path.len() == 1 {
        match value {
            ToolConfigFieldValue::Missing => {
                table.remove(key);
            }
            ToolConfigFieldValue::Value(value) => {
                table[key] = toml_item_from_owned_value(value)?;
            }
        }
        return Ok(());
    }
    if !table.contains_key(key) {
        if matches!(value, ToolConfigFieldValue::Missing) {
            return Ok(());
        }
        table[key] = Item::Table(Table::new());
    }
    let Some(next) = table.get_mut(key).and_then(Item::as_table_mut) else {
        return Ok(());
    };
    set_toml_field_value(next, &path[1..], value)
}

fn cleanup_created_empty_toml_tables(table: &mut Table, original: Option<&Table>) {
    let keys = table
        .iter()
        .map(|(key, _)| key.to_string())
        .collect::<Vec<_>>();
    for key in keys {
        let original_item = original.and_then(|table| table.get(&key));
        let original_table = original_item.and_then(Item::as_table);
        let remove = table
            .get_mut(&key)
            .and_then(Item::as_table_mut)
            .map(|child| {
                cleanup_created_empty_toml_tables(child, original_table);
                child.is_empty() && original_item.is_none()
            })
            .unwrap_or(false);
        if remove {
            table.remove(&key);
        }
    }
}

fn set_env_field_value(raw: &str, key: &str, value: &ToolConfigFieldValue) -> String {
    let replacement = match value {
        ToolConfigFieldValue::Missing => None,
        ToolConfigFieldValue::Value(value) => value.as_str(),
    };
    let mut lines = Vec::new();
    let mut replaced = false;
    for line in raw.lines() {
        let trimmed = line.trim_start();
        let matches = trimmed
            .split_once('=')
            .map(|(line_key, _)| line_key.trim() == key)
            .unwrap_or(false);
        if !matches {
            lines.push(line.to_string());
            continue;
        }
        if let Some(replacement) = replacement {
            if !replaced {
                lines.push(format!("{key}={replacement}"));
                replaced = true;
            }
        }
    }
    if !replaced {
        if let Some(replacement) = replacement {
            lines.push(format!("{key}={replacement}"));
        }
    }
    let mut content = lines.join("\n");
    if !content.is_empty() && (raw.ends_with('\n') || !content.ends_with('\n')) {
        content.push('\n');
    }
    content
}

fn ownership_original_bytes(ownership: &ToolConfigOwnership) -> Result<Option<Vec<u8>>> {
    if !ownership.original_exists {
        return Ok(None);
    }
    let encoded = ownership
        .original_content_base64
        .as_deref()
        .ok_or_else(|| {
            anyhow!(
                "tool config manifest is missing original content for {}",
                ownership.tool
            )
        })?;
    let content = BASE64
        .decode(encoded)
        .with_context(|| format!("decode {} original config", ownership.tool))?;
    if sha256_hex(&content) != ownership.original_sha256 {
        return Err(anyhow!(
            "tool config manifest original hash mismatch for {}",
            ownership.tool
        ));
    }
    Ok(Some(content))
}

fn ownership_applied_bytes(ownership: &ToolConfigOwnership) -> Result<Option<Vec<u8>>> {
    let Some(encoded) = ownership.applied_content_base64.as_deref() else {
        return Ok(None);
    };
    let content = BASE64
        .decode(encoded)
        .with_context(|| format!("decode {} applied config", ownership.tool))?;
    if sha256_hex(&content) != ownership.applied_sha256 {
        return Err(anyhow!(
            "tool config manifest applied hash mismatch for {}",
            ownership.tool
        ));
    }
    Ok(Some(content))
}

fn normalize_tool_config_ownership_fields(
    ownership: &mut ToolConfigOwnership,
    fallback_applied: Option<&[u8]>,
) -> Result<()> {
    if ownership.fields.is_empty() {
        let original = ownership_original_bytes(ownership)?;
        let applied = ownership_applied_bytes(ownership)?
            .or_else(|| fallback_applied.map(<[u8]>::to_vec));
        if let Some(applied) = applied {
            ownership.fields =
                tool_config_owned_fields(ownership.format, original.as_deref(), &applied)?;
        }
    } else {
        ownership.fields =
            refine_tool_config_owned_fields(std::mem::take(&mut ownership.fields));
    }
    Ok(())
}

fn restore_managed_json_fields_for_reapply(
    tool: &str,
    path: &Path,
    document: &mut serde_json::Value,
    field_paths: &[&[&str]],
) -> Result<()> {
    let _guard = tool_config_manifest_lock()
        .lock()
        .map_err(|_| anyhow!("TOOL_CONFIG_MANIFEST_LOCK_POISONED: manifest lock poisoned"))?;
    let manifest = load_tool_config_manifest()?;
    let Some(mut ownership) = manifest.files.get(&manifest_file_key(path)).cloned() else {
        return Ok(());
    };
    if ownership.tool != tool || ownership.format != ToolConfigFormat::Json {
        return Ok(());
    }
    normalize_tool_config_ownership_fields(&mut ownership, None)?;
    for field_path in field_paths {
        let field_path = field_path
            .iter()
            .map(|key| ToolConfigFieldPath::Key((*key).to_string()))
            .collect::<Vec<_>>();
        let Some(field) = ownership
            .fields
            .iter()
            .find(|field| field.path == field_path)
        else {
            continue;
        };
        // Restore only a value that still matches the last CONST API write.
        // A later user/tool edit is intentionally left untouched.
        if tool_config_field_value_at(document, &field_path) == field.applied {
            set_tool_config_field_value(document, &field_path, &field.original);
        }
    }
    Ok(())
}

fn restore_managed_toml_fields_for_reapply(
    tool: &str,
    path: &Path,
    raw: &str,
    field_paths: &[&[&str]],
) -> Result<String> {
    let _guard = tool_config_manifest_lock()
        .lock()
        .map_err(|_| anyhow!("TOOL_CONFIG_MANIFEST_LOCK_POISONED: manifest lock poisoned"))?;
    let manifest = load_tool_config_manifest()?;
    let Some(mut ownership) = manifest.files.get(&manifest_file_key(path)).cloned() else {
        return Ok(raw.to_string());
    };
    if ownership.tool != tool || ownership.format != ToolConfigFormat::Toml {
        return Ok(raw.to_string());
    }
    normalize_tool_config_ownership_fields(&mut ownership, None)?;
    let mut document = if raw.trim().is_empty() {
        DocumentMut::new()
    } else {
        raw.parse::<DocumentMut>()
            .with_context(|| format!("parse {} during managed-field reapply", path.display()))?
    };
    for field_path in field_paths {
        let field_path = field_path
            .iter()
            .map(|key| ToolConfigFieldPath::Key((*key).to_string()))
            .collect::<Vec<_>>();
        let Some(field) = ownership
            .fields
            .iter()
            .find(|field| field.path == field_path)
        else {
            continue;
        };
        // Restore only a value that still matches the last CONST API write.
        // A later user/tool edit is intentionally left untouched.
        if toml_field_values_match(&toml_field_value_at(&document, &field_path), &field.applied)? {
            set_toml_field_value(document.as_table_mut(), &field_path, &field.original)?;
        }
    }
    Ok(document.to_string())
}

fn release_managed_fields_after_reapply(
    tool: &str,
    path: &Path,
    field_paths: &[&[&str]],
) -> Result<()> {
    if tool_config_preview_active() {
        return Ok(());
    }
    let _guard = tool_config_manifest_lock()
        .lock()
        .map_err(|_| anyhow!("TOOL_CONFIG_MANIFEST_LOCK_POISONED: manifest lock poisoned"))?;
    let mut manifest = load_tool_config_manifest()?;
    let key = manifest_file_key(path);
    let current = read_optional_file(path)?;
    let remove_entry = {
        let Some(ownership) = manifest.files.get_mut(&key) else {
            return Ok(());
        };
        if ownership.tool != tool || ownership.format != tool_config_format(path) {
            return Ok(());
        }
        normalize_tool_config_ownership_fields(ownership, None)?;
        let current_document = parse_tool_config_document(ownership.format, current.as_deref())?;
        let retired_paths = field_paths
            .iter()
            .map(|field_path| {
                field_path
                    .iter()
                    .map(|key| ToolConfigFieldPath::Key((*key).to_string()))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let previous_len = ownership.fields.len();
        ownership.fields.retain(|field| {
            if !retired_paths.contains(&field.path) {
                return true;
            }
            // Release only ownership carried in from an older apply. If this
            // apply changed a previously unmanaged value, keep ownership so
            // cancellation can still restore it.
            let current_value = tool_config_field_value_at(&current_document, &field.path);
            !(field.original == field.applied || current_value != field.applied)
        });
        if ownership.fields.len() == previous_len {
            return Ok(());
        }
        ownership.fields.is_empty()
    };
    if remove_entry {
        manifest.files.remove(&key);
    }
    save_tool_config_manifest(&manifest)
}

fn remember_tool_config_write(tool: &str, path: &Path, content: &[u8]) -> Result<()> {
    if !manifest_manages_tool(tool) {
        return Ok(());
    }
    let _guard = tool_config_manifest_lock()
        .lock()
        .map_err(|_| anyhow!("TOOL_CONFIG_MANIFEST_LOCK_POISONED: manifest lock is poisoned"))?;
    let mut manifest = load_tool_config_manifest()?;
    let key = manifest_file_key(path);
    let current = read_optional_file(path)?;
    let format = tool_config_format(path);
    let applied_sha256 = sha256_hex(content);
    if let Some(ownership) = manifest.files.get_mut(&key) {
        if ownership.tool != tool {
            return Err(anyhow!(
                "{} is already owned by another CONST API tool config: {}",
                path.display(),
                ownership.tool
            ));
        }
        let current_sha256 = current.as_deref().map(sha256_hex).unwrap_or_default();
        // Applying configuration is an explicit overwrite operation. External
        // edits are useful input to the merge, not a reason to reject the
        // operation. Writers build `content` from the current document and
        // change only their own fields, so unrelated tool/user data survives.
        normalize_tool_config_ownership_fields(ownership, Some(content))?;
        normalize_deepseek_harness_credential_ownership(ownership, current.as_deref())?;
        let current_document = parse_tool_config_document_at(path, format, current.as_deref())?;
        let applied_document = parse_tool_config_document_at(path, format, Some(content))?;
        let mut changed_fields = Vec::new();
        collect_tool_config_owned_fields(
            Some(&current_document),
            Some(&applied_document),
            &mut Vec::new(),
            &mut changed_fields,
        );
        for changed in changed_fields {
            if let Some(existing) = ownership
                .fields
                .iter_mut()
                .find(|field| field.path == changed.path)
            {
                existing.applied = changed.applied;
            } else {
                ownership.fields.push(changed);
            }
        }
        if current_sha256 != ownership.applied_sha256 && current_sha256 != ownership.original_sha256
        {
            ownership.whole_file_restore_safe = false;
        }
        ownership.format = format;
        ownership.applied_content_base64 = Some(BASE64.encode(content));
        ownership.applied_sha256 = applied_sha256;
    } else {
        let fields = if is_deepseek_harness_patch_path(path) {
            let original = parse_tool_config_document_at(path, format, current.as_deref())?;
            let applied = parse_tool_config_document_at(path, format, Some(content))?;
            let mut fields = Vec::new();
            collect_tool_config_owned_fields(
                Some(&original),
                Some(&applied),
                &mut Vec::new(),
                &mut fields,
            );
            fields
        } else {
            tool_config_owned_fields(format, current.as_deref(), content)?
        };
        if fields.is_empty() {
            return Ok(());
        }
        let original_sha256 = current.as_deref().map(sha256_hex).unwrap_or_default();
        manifest.files.insert(
            key,
            ToolConfigOwnership {
                tool: tool.to_string(),
                format,
                fields,
                original_exists: current.is_some(),
                original_content_base64: current.as_deref().map(|bytes| BASE64.encode(bytes)),
                original_sha256,
                applied_content_base64: Some(BASE64.encode(content)),
                applied_sha256,
                whole_file_restore_safe: true,
            },
        );
    }
    save_tool_config_manifest(&manifest)
}

fn restore_structured_tool_config_fields(
    path: &Path,
    current: Option<&[u8]>,
    ownership: &ToolConfigOwnership,
) -> Result<(Option<Vec<u8>>, usize)> {
    let raw = current
        .map(String::from_utf8_lossy)
        .map(|value| value.into_owned())
        .unwrap_or_default();
    let mut preserved = 0usize;
    match ownership.format {
        ToolConfigFormat::Toml => {
            let mut doc = if raw.trim().is_empty() {
                DocumentMut::new()
            } else {
                raw.parse::<DocumentMut>()
                    .with_context(|| format!("parse {} during field restore", path.display()))?
            };
            let mut changed = false;
            for field in &ownership.fields {
                let current_value = toml_field_value_at(&doc, &field.path);
                if toml_field_values_match(&current_value, &field.applied)? {
                    set_toml_field_value(doc.as_table_mut(), &field.path, &field.original)?;
                    changed = true;
                } else if !toml_field_values_match(&current_value, &field.original)? {
                    preserved += 1;
                }
            }
            if !changed {
                return Ok((current.map(<[u8]>::to_vec), preserved));
            }
            let original = ownership_original_bytes(ownership)?;
            let original_doc = original
                .as_deref()
                .map(String::from_utf8_lossy)
                .filter(|raw| !raw.trim().is_empty())
                .map(|raw| raw.parse::<DocumentMut>())
                .transpose()
                .with_context(|| {
                    format!("parse original {} during field restore", path.display())
                })?;
            cleanup_created_empty_toml_tables(
                doc.as_table_mut(),
                original_doc.as_ref().map(DocumentMut::as_table),
            );
            let content = doc.to_string();
            if !ownership.original_exists && content.trim().is_empty() {
                Ok((None, preserved))
            } else {
                Ok((Some(content.into_bytes()), preserved))
            }
        }
        ToolConfigFormat::Env => {
            let mut content = raw;
            let mut changed = false;
            for field in &ownership.fields {
                let [ToolConfigFieldPath::Key(key)] = field.path.as_slice() else {
                    preserved += 1;
                    continue;
                };
                let document = env_text_to_owned_json(&content);
                let current_value =
                    tool_config_field_value_at(&document, &[ToolConfigFieldPath::Key(key.clone())]);
                if current_value == field.applied {
                    content = set_env_field_value(&content, key, &field.original);
                    changed = true;
                } else if current_value != field.original {
                    preserved += 1;
                }
            }
            if !changed {
                return Ok((current.map(<[u8]>::to_vec), preserved));
            }
            if !ownership.original_exists && content.trim().is_empty() {
                Ok((None, preserved))
            } else {
                Ok((Some(content.into_bytes()), preserved))
            }
        }
        ToolConfigFormat::Json | ToolConfigFormat::Yaml | ToolConfigFormat::Text => {
            let mut document = parse_tool_config_document_at(path, ownership.format, current)?;
            let mut changed = false;
            for field in &ownership.fields {
                let current_value = tool_config_field_value_at(&document, &field.path);
                if current_value == field.applied || owned_field_is_added_named_entry(field) {
                    set_tool_config_field_value(&mut document, &field.path, &field.original);
                    changed = true;
                } else if current_value != field.original {
                    preserved += 1;
                }
            }
            if !changed {
                return Ok((current.map(<[u8]>::to_vec), preserved));
            }
            let original = ownership_original_bytes(ownership)?;
            let original_document =
                parse_tool_config_document_at(path, ownership.format, original.as_deref())?;
            cleanup_created_empty_json_containers(&mut document, Some(&original_document));
            if is_deepseek_harness_patch_path(path) {
                let settings = serde_yaml::from_value(serde_yaml::to_value(document)?)?;
                let content = render_deepseek_harness_patch(&raw, &settings)?;
                return Ok((Some(content.into_bytes()), preserved));
            }
            if !ownership.original_exists && tool_config_document_is_empty(&document) {
                return Ok((None, preserved));
            }
            let content = match ownership.format {
                ToolConfigFormat::Json => serde_json::to_string_pretty(&document)? + "\n",
                ToolConfigFormat::Yaml => serde_yaml::to_string(&document)?,
                ToolConfigFormat::Text => document.as_str().unwrap_or_default().to_string(),
                format => {
                    return Err(anyhow!(
                        "unsupported restorable tool config format: {format:?}"
                    ));
                }
            };
            Ok((Some(content.into_bytes()), preserved))
        }
    }
}

fn restore_tool_config_from_manifest(tool: &str) -> Result<ToolApplyResult> {
    let _guard = tool_config_manifest_lock()
        .lock()
        .map_err(|_| anyhow!("TOOL_CONFIG_MANIFEST_LOCK_POISONED: manifest lock is poisoned"))?;
    let mut manifest = load_tool_config_manifest()?;
    let mut owned = manifest
        .files
        .iter()
        .filter(|(_, ownership)| ownership.tool == tool)
        .map(|(key, ownership)| (key.clone(), PathBuf::from(key), ownership.clone()))
        .collect::<Vec<_>>();
    for (_, _, ownership) in &mut owned {
        normalize_tool_config_ownership_fields(ownership, None)?;
    }
    let owned_count = owned.len();
    let mut current_files = Vec::with_capacity(owned.len());
    for (key, path, ownership) in &mut owned {
        let current = read_optional_file(path)?;
        normalize_deepseek_harness_credential_ownership(ownership, current.as_deref())?;
        let matches_applied = current
            .as_deref()
            .map(sha256_hex)
            .map(|hash| hash == ownership.applied_sha256)
            .unwrap_or(false);
        let (restored, preserved) = if ownership.fields.is_empty() {
            let original = ownership_original_bytes(ownership)?;
            let matches_original = match (original.as_deref(), current.as_deref()) {
                (Some(original), Some(current)) => original == current,
                (None, None) => true,
                _ => false,
            };
            if matches_applied || matches_original {
                (original, 0)
            } else {
                // Very old manifests may not contain enough information for a
                // field-level restore. Cancellation must still complete: keep
                // the externally changed file and let the tool-specific alias
                // cleanup remove any recognizable CONST API entries.
                (current.clone(), 1)
            }
        } else if ownership.whole_file_restore_safe && matches_applied {
            (ownership_original_bytes(ownership)?, 0)
        } else {
            restore_structured_tool_config_fields(path, current.as_deref(), ownership)?
        };
        current_files.push((key.clone(), path.clone(), restored, current, preserved));
    }

    let mut result = ToolApplyBuilder::default();
    let mut preserved_fields = 0usize;
    for (key, path, restored, current, preserved) in current_files {
        preserved_fields += preserved;
        let changed = restored.as_deref() != current.as_deref();
        let status = ToolFileStatus {
            path: path.display().to_string(),
            exists_before: current.is_some(),
            changed,
            already_configured: !changed,
            before_sha256: current.as_deref().map(sha256_hex).unwrap_or_default(),
            after_sha256: restored.as_deref().map(sha256_hex).unwrap_or_default(),
        };
        if changed && !tool_config_preview_active() {
            if current.is_some() {
                backup_if_exists(&path, tool, &mut result.backups)?;
            }
            match restored {
                Some(content) => atomic_write(&path, &content)?,
                None if path.exists() => {
                    fs::remove_file(&path).with_context(|| format!("delete {}", path.display()))?
                }
                None => {}
            }
            result.files.push(path.display().to_string());
        }
        result.file_statuses.push(status);
        manifest.files.remove(&key);
    }
    if owned_count > 0 && !tool_config_preview_active() {
        save_tool_config_manifest(&manifest)?;
    }
    let mut finished = result.finish(tool);
    finished.details.insert(
        "manifest_entries_restored".to_string(),
        owned_count.to_string(),
    );
    finished.details.insert(
        "manifest_fields_preserved".to_string(),
        preserved_fields.to_string(),
    );
    Ok(finished)
}

fn record_post_restore_cleanup(
    result: &mut ToolApplyResult,
    path: &Path,
    before: Vec<u8>,
    after: Option<Vec<u8>>,
    tool: &str,
) -> Result<()> {
    if after.as_deref() == Some(before.as_slice()) {
        return Ok(());
    }
    if tool_config_preview_active() {
        result.file_statuses.push(ToolFileStatus {
            path: path.display().to_string(),
            exists_before: true,
            changed: true,
            already_configured: false,
            before_sha256: sha256_hex(&before),
            after_sha256: after.as_deref().map(sha256_hex).unwrap_or_default(),
        });
        result.already_configured = false;
        return Ok(());
    }
    backup_if_exists(path, tool, &mut result.backups)?;
    match after.as_deref() {
        Some(content) => atomic_write(path, content)?,
        None if path.exists() => fs::remove_file(path)?,
        None => {}
    }
    result.files.push(path.display().to_string());
    result.file_statuses.push(ToolFileStatus {
        path: path.display().to_string(),
        exists_before: true,
        changed: true,
        already_configured: false,
        before_sha256: sha256_hex(&before),
        after_sha256: after.as_deref().map(sha256_hex).unwrap_or_default(),
    });
    result.already_configured = false;
    Ok(())
}

fn record_native_route_update(
    result: &mut ToolApplyResult,
    path: &Path,
    after: Option<Vec<u8>>,
    tool: &str,
) -> Result<()> {
    let before = read_optional_file(path)?;
    match before {
        Some(before) => record_post_restore_cleanup(result, path, before, after, tool),
        None => {
            let Some(content) = after else {
                return Ok(());
            };
            if tool_config_preview_active() {
                result.file_statuses.push(ToolFileStatus {
                    path: path.display().to_string(),
                    exists_before: false,
                    changed: true,
                    already_configured: false,
                    before_sha256: String::new(),
                    after_sha256: sha256_hex(&content),
                });
                result.already_configured = false;
                return Ok(());
            }
            atomic_write(path, &content)?;
            result.files.push(path.display().to_string());
            result.file_statuses.push(ToolFileStatus {
                path: path.display().to_string(),
                exists_before: false,
                changed: true,
                already_configured: false,
                before_sha256: String::new(),
                after_sha256: sha256_hex(&content),
            });
            result.already_configured = false;
            Ok(())
        }
    }
}

fn cleanup_codex_provider_aliases(
    result: &mut ToolApplyResult,
    base_url: &str,
    api_key: &str,
) -> Result<()> {
    let path = codex_home().join("config.toml");
    if !path.exists() {
        return Ok(());
    }
    let before = fs::read(&path)?;
    let raw = String::from_utf8_lossy(&before);
    let mut doc = raw.parse::<DocumentMut>().with_context(|| {
        format!(
            "parse Codex config during provider cleanup {}",
            path.display()
        )
    })?;
    let mut changed = false;
    let owned_provider_ids = doc
        .get("model_providers")
        .and_then(Item::as_table)
        .map(|providers| {
            [CODEX_CONST_API_PROVIDER_ID, LEGACY_CONST_API_PROVIDER_ID]
                .into_iter()
                .filter(|provider_id| {
                    providers
                        .get(provider_id)
                        .and_then(Item::as_table)
                        .map(|provider| {
                            codex_provider_table_matches_const_api(provider, base_url, api_key)
                        })
                        .unwrap_or(false)
                })
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();
    if doc
        .get("model_provider")
        .and_then(Item::as_str)
        .map(|value| owned_provider_ids.contains(value))
        .unwrap_or(false)
    {
        doc.as_table_mut().remove("model_provider");
        changed = true;
    }
    if let Some(providers) = doc.get_mut("model_providers").and_then(Item::as_table_mut) {
        for provider_id in [CODEX_CONST_API_PROVIDER_ID, LEGACY_CONST_API_PROVIDER_ID] {
            if owned_provider_ids.contains(provider_id) {
                providers.remove(provider_id);
                changed = true;
            }
        }
        if providers.is_empty() {
            doc.as_table_mut().remove("model_providers");
        }
    }
    if !changed {
        return Ok(());
    }
    let content = doc.to_string().into_bytes();
    record_post_restore_cleanup(result, &path, before, Some(content), "codex")
}

fn cleanup_opencode_provider_aliases(
    result: &mut ToolApplyResult,
    base_url: &str,
    api_key: &str,
) -> Result<()> {
    let path = opencode_config_path()?;
    if !path.exists() {
        return Ok(());
    }
    let before = fs::read(&path)?;
    let mut root = json5::from_str::<serde_json::Value>(&String::from_utf8_lossy(&before))?;
    let original = root.clone();
    remove_opencode_const_api_provider_aliases(&mut root, base_url, api_key);
    if root
        .get("provider")
        .and_then(serde_json::Value::as_object)
        .map(serde_json::Map::is_empty)
        .unwrap_or(false)
    {
        if let Some(root) = root.as_object_mut() {
            root.remove("provider");
        }
    }
    if root == original {
        return Ok(());
    }
    let mut after = serde_json::to_vec_pretty(&root)?;
    after.push(b'\n');
    record_post_restore_cleanup(result, &path, before, Some(after), "opencode")
}

fn cleanup_unmanaged_vscode_provider_aliases(result: &mut ToolApplyResult) -> Result<()> {
    let path = vscode_chat_language_models_path();
    if !path.exists() {
        return Ok(());
    }
    let before = fs::read(&path)?;
    let mut root = serde_json::from_slice::<serde_json::Value>(&before)
        .with_context(|| format!("parse {}", path.display()))?;
    let providers = root
        .as_array_mut()
        .ok_or_else(|| anyhow!("VS Code chatLanguageModels.json must contain a JSON array"))?;
    let original_len = providers.len();
    providers.retain(|provider| {
        provider
            .get("name")
            .and_then(serde_json::Value::as_str)
            .is_none_or(|name| {
                name != CONST_API_DISPLAY_NAME && name != CONST_API_LEGACY_DISPLAY_NAME
            })
    });
    if providers.len() == original_len {
        return Ok(());
    }
    let mut after = serde_json::to_vec_pretty(&root)?;
    after.push(b'\n');
    record_post_restore_cleanup(result, &path, before, Some(after), "vscode")
}

fn cleanup_openclaw_provider_aliases(
    result: &mut ToolApplyResult,
    root_url: &str,
    api_key: &str,
) -> Result<()> {
    let path = openclaw_config_path()?;
    if !path.exists() {
        return Ok(());
    }
    let before = fs::read(&path)?;
    let mut root = json5::from_str::<serde_json::Value>(&String::from_utf8_lossy(&before))?;
    let original = root.clone();
    remove_openclaw_const_api_providers(&mut root, root_url, api_key);
    if root == original {
        return Ok(());
    }
    let mut after = serde_json::to_vec_pretty(&root)?;
    after.push(b'\n');
    record_post_restore_cleanup(result, &path, before, Some(after), "openclaw")
}

fn cleanup_hermes_provider_aliases(
    result: &mut ToolApplyResult,
    root_url: &str,
    api_key: &str,
) -> Result<()> {
    for path in hermes_config_paths() {
        if !path.exists() {
            continue;
        }
        let before = fs::read(&path)?;
        let value = serde_yaml::from_slice::<serde_yaml::Value>(&before)?;
        let mut root = value.as_mapping().cloned().unwrap_or_default();
        let original = root.clone();
        let model_key = yaml_key("model");
        if let Some(model) = root
            .get_mut(&model_key)
            .and_then(serde_yaml::Value::as_mapping_mut)
        {
            let provider_is_const = model
                .get(yaml_key("provider"))
                .and_then(serde_yaml::Value::as_str)
                .map(|provider| {
                    provider == HERMES_CONST_API_PROVIDER_ID
                        || provider == HERMES_LEGACY_CONST_API_PROVIDER_ID
                })
                .unwrap_or(false);
            let local = model
                .get(yaml_key("base_url"))
                .and_then(serde_yaml::Value::as_str)
                .map(is_const_api_local_tool_url)
                .unwrap_or(false);
            let owned_key = model
                .get(yaml_key("api_key"))
                .and_then(serde_yaml::Value::as_str)
                .map(|value| value == api_key || value == LOCAL_PLACEHOLDER_KEY)
                .unwrap_or(false);
            if provider_is_const && (local || owned_key) {
                for key in ["provider", "base_url", "api_key", "api_mode"] {
                    model.remove(yaml_key(key));
                }
            }
            if model.is_empty() {
                root.remove(&model_key);
            }
        }
        if let Some(providers) = root
            .get_mut(yaml_key("custom_providers"))
            .and_then(serde_yaml::Value::as_sequence_mut)
        {
            providers.retain(|value| {
                let Some(provider) = value.as_mapping() else {
                    return true;
                };
                let known_name = provider
                    .get(yaml_key("name"))
                    .and_then(serde_yaml::Value::as_str)
                    .map(|name| {
                        name == CODEX_CONST_API_PROVIDER_ID || name == LEGACY_CONST_API_PROVIDER_ID
                    })
                    .unwrap_or(false);
                let local = provider
                    .get(yaml_key("base_url"))
                    .and_then(serde_yaml::Value::as_str)
                    .map(is_const_api_local_tool_url)
                    .unwrap_or(false);
                let owned_key = provider
                    .get(yaml_key("api_key"))
                    .and_then(serde_yaml::Value::as_str)
                    .map(|value| value == api_key || value == LOCAL_PLACEHOLDER_KEY)
                    .unwrap_or(false);
                !(known_name && (local || owned_key))
            });
            if providers.is_empty() {
                root.remove(yaml_key("custom_providers"));
            }
        }
        remove_hermes_const_api_provider_dict(&mut root);
        if root == original {
            continue;
        }
        let after = serde_yaml::to_string(&serde_yaml::Value::Mapping(root))?.into_bytes();
        record_post_restore_cleanup(result, &path, before, Some(after), "hermes")?;
    }
    let _ = root_url;
    Ok(())
}

fn write_text_with_backup(
    path: &Path,
    content: &str,
    tool: &str,
    result: &mut ToolApplyBuilder,
) -> Result<()> {
    if tool_config_preview_active() {
        return observe_text(path, content, result);
    }
    let after = content.as_bytes();
    // Match the Codex preview's semantic comparison. In particular, serializing an
    // unchanged official auth.json must not back up or rewrite its credentials.
    if tool == "codex" {
        let mut status = semantic_file_status_for(path, after)?;
        if status.already_configured {
            status.after_sha256.clone_from(&status.before_sha256);
            result.file_statuses.push(status);
            return Ok(());
        }
    }
    let status = file_status_for(path, after)?;
    remember_tool_config_write(tool, path, after)?;
    if status.already_configured {
        result.file_statuses.push(status);
        return Ok(());
    }
    if status.exists_before {
        backup_if_exists(path, tool, &mut result.backups)?;
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    atomic_write(path, after)?;
    result.files.push(path.display().to_string());
    result.file_statuses.push(status);
    Ok(())
}

pub(crate) fn read_text_or_empty(path: &Path) -> Result<String> {
    if path.exists() {
        fs::read_to_string(path).with_context(|| format!("read {}", path.display()))
    } else {
        Ok(String::new())
    }
}

pub(crate) fn read_json_or_default(
    path: &Path,
    default: serde_json::Value,
) -> Result<serde_json::Value> {
    if !path.exists() {
        return Ok(default);
    }
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("parse json {}", path.display()))
}

pub(crate) fn read_json5_or_default(
    path: &Path,
    default: serde_json::Value,
) -> Result<serde_json::Value> {
    if !path.exists() {
        return Ok(default);
    }
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    json5::from_str(&raw).with_context(|| format!("parse json5 {}", path.display()))
}
