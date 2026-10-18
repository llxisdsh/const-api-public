use rusqlite::OptionalExtension;
use serde_json::{Map, Value};
use std::time::{SystemTime, UNIX_EPOCH};

const OCTOP_PROVIDER_NAME: &str = "CONST API";
const OCTOP_PROVIDER_NOTE: &str = "Managed by CONST API";
const OCTOP_MANAGED_FIELD: &str = "const_api_managed";
const OCTOP_PREVIOUS_ACTIVE_MODEL_FIELD: &str = "const_api_previous_active_model";

#[derive(Debug, Clone)]
struct OctopProvider {
    id: i64,
    kind: String,
    base_url: String,
    api_key: String,
    extra_json: String,
    models_json: String,
    note: String,
    enabled: i64,
}

fn octop_home() -> PathBuf {
    if test_home_active() {
        return home_dir().join(".octop");
    }
    nonempty_env_path("OCTOP_HOME")
        .map(expand_tool_home_path)
        .unwrap_or_else(|| home_dir().join(".octop"))
}

fn octop_database_path() -> Result<PathBuf> {
    let home = octop_home();
    let config_path = home.join("config.json");
    if config_path.exists() {
        let raw = fs::read_to_string(&config_path)
            .with_context(|| format!("read Octop config {}", config_path.display()))?;
        let config = serde_json::from_str::<Value>(&raw)
            .with_context(|| format!("parse Octop config {}", config_path.display()))?;
        if let Some(path) = config
            .get("database")
            .and_then(Value::as_object)
            .and_then(|database| database.get("sqlite_path"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|path| !path.is_empty())
        {
            let path = PathBuf::from(path);
            return Ok(if path.is_absolute() {
                path
            } else {
                home.join(path)
            });
        }
    }
    Ok(home.join("octop.db"))
}

fn open_octop_database(path: &Path, read_only: bool) -> Result<Connection> {
    if read_only {
        Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("open Octop database {}", path.display()))
    } else {
        Connection::open(path).with_context(|| format!("open Octop database {}", path.display()))
    }
    .and_then(|connection| {
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .context("configure Octop database busy timeout")?;
        Ok(connection)
    })
}

fn validate_octop_schema(connection: &Connection) -> Result<()> {
    let columns = |table: &str| -> Result<HashSet<String>> {
        let mut statement = connection
            .prepare(&format!("PRAGMA table_info({table})"))
            .with_context(|| format!("inspect Octop table {table}"))?;
        let values = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<HashSet<_>>>()?;
        Ok(values)
    };
    for (table, required) in [
        (
            "providers",
            [
                "id",
                "name",
                "kind",
                "base_url",
                "api_key",
                "extra_json",
                "models_json",
                "note",
                "enabled",
                "created_at",
                "updated_at",
            ]
            .as_slice(),
        ),
        ("settings", ["key", "value"].as_slice()),
    ] {
        let present = columns(table)?;
        for column in required {
            if !present.contains(*column) {
                return Err(anyhow!(
                    "OCTOP_SCHEMA_UNSUPPORTED: {table} is missing {column}"
                ));
            }
        }
    }
    Ok(())
}

fn load_octop_provider(connection: &Connection) -> Result<Option<OctopProvider>> {
    connection
        .query_row(
            "SELECT id, kind, COALESCE(base_url, ''), COALESCE(api_key, ''),\
             COALESCE(extra_json, ''), COALESCE(models_json, ''), COALESCE(note, ''), enabled \
             FROM providers WHERE name = ?1",
            [OCTOP_PROVIDER_NAME],
            |row| {
                Ok(OctopProvider {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    base_url: row.get(2)?,
                    api_key: row.get(3)?,
                    extra_json: row.get(4)?,
                    models_json: row.get(5)?,
                    note: row.get(6)?,
                    enabled: row.get(7)?,
                })
            },
        )
        .optional()
        .context("read Octop CONST API provider")
}

fn load_octop_active_model(connection: &Connection) -> Result<Option<String>> {
    connection
        .query_row(
            "SELECT value FROM settings WHERE key = 'active_model'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .context("read Octop active model")
}

fn octop_provider_is_managed(provider: &OctopProvider) -> Result<bool> {
    if provider.note == OCTOP_PROVIDER_NOTE {
        return Ok(true);
    }
    if provider.extra_json.trim().is_empty() {
        return Ok(false);
    }
    let value = serde_json::from_str::<Value>(&provider.extra_json)
        .context("parse Octop provider metadata")?;
    Ok(value.get(OCTOP_MANAGED_FIELD).and_then(Value::as_bool) == Some(true))
}

fn octop_provider_extra(
    provider: Option<&OctopProvider>,
    previous: Option<&str>,
) -> Result<String> {
    let mut extra = match provider.map(|provider| provider.extra_json.trim()) {
        Some(raw) if !raw.is_empty() => {
            serde_json::from_str::<Value>(raw).context("parse managed Octop provider metadata")?
        }
        _ => Value::Object(Map::new()),
    };
    if !extra.is_object() {
        return Err(anyhow!(
            "OCTOP_PROVIDER_METADATA_INVALID: extra_json is not an object"
        ));
    }
    let object = extra
        .as_object_mut()
        .ok_or_else(|| anyhow!("OCTOP_PROVIDER_METADATA_INVALID: extra_json is not an object"))?;
    object.insert(OCTOP_MANAGED_FIELD.to_string(), Value::Bool(true));
    if provider.is_none() {
        if let Some(previous) = previous.filter(|value| !value.trim().is_empty()) {
            object.insert(
                OCTOP_PREVIOUS_ACTIVE_MODEL_FIELD.to_string(),
                Value::String(previous.to_string()),
            );
        }
    }
    Ok(serde_json::to_string(&extra)?)
}

fn octop_previous_active_model(provider: &OctopProvider) -> Result<Option<String>> {
    if provider.extra_json.trim().is_empty() {
        return Ok(None);
    }
    let value = serde_json::from_str::<Value>(&provider.extra_json)
        .context("parse managed Octop provider metadata")?;
    Ok(value
        .get(OCTOP_PREVIOUS_ACTIVE_MODEL_FIELD)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty()))
}

fn octop_model_entries(models: &[ToolModelInfo]) -> Result<Vec<Value>> {
    let models = configured_additional_tool_models("Octop", models)?;
    Ok(models
        .into_iter()
        .map(|model| {
            let id = model.id.trim().to_string();
            let name = if model.display_name.trim().is_empty() {
                id.clone()
            } else {
                model.display_name.trim().to_string()
            };
            let mut input = Vec::new();
            for modality in &model.input_modalities {
                let modality = modality.trim();
                if !modality.is_empty() && !input.iter().any(|value| value == modality) {
                    input.push(modality.to_string());
                }
            }
            if input.is_empty() {
                input.push("text".to_string());
            }
            serde_json::json!({
                "id": id,
                "name": name,
                "enabled": true,
                "input": input,
                "thinking": Value::Null,
                "reasoning": model.reasoning,
            })
        })
        .collect())
}

fn octop_json_matches(raw: &str, expected: &Value) -> bool {
    serde_json::from_str::<Value>(raw)
        .map(|value| value == *expected)
        .unwrap_or(false)
}

fn octop_file_status(
    path: &Path,
    before: &[u8],
    already_configured: bool,
) -> Result<ToolFileStatus> {
    let after = fs::read(path).unwrap_or_else(|_| before.to_vec());
    Ok(ToolFileStatus {
        path: path.display().to_string(),
        exists_before: !before.is_empty(),
        changed: !already_configured,
        already_configured,
        before_sha256: sha256_hex(before),
        after_sha256: sha256_hex(&after),
    })
}

fn octop_result(
    tool: &str,
    path: &Path,
    before: &[u8],
    backups: Vec<String>,
    already_configured: bool,
    details: &[(&str, String)],
) -> Result<ToolApplyResult> {
    let mut builder = ToolApplyBuilder::default();
    builder.files.push(path.display().to_string());
    builder.backups = backups;
    builder
        .file_statuses
        .push(octop_file_status(path, before, already_configured)?);
    let mut result = builder.finish(tool);
    for (key, value) in details {
        result.details.insert((*key).to_string(), value.clone());
    }
    attach_tool_protocol(&mut result, ToolProtocol::OpenAiChat);
    Ok(result)
}

fn octop_connection_values(
    base_url: &str,
    api_key: &str,
    models: &[ToolModelInfo],
) -> Result<(String, String, Value, Vec<Value>)> {
    let endpoint = tool_surface_url(base_url, ToolProtocol::OpenAiChat.surface());
    let model_entries = octop_model_entries(models)?;
    let models_value = Value::Array(model_entries.clone());
    Ok((
        endpoint,
        api_key.trim().to_string(),
        models_value,
        model_entries,
    ))
}

pub(crate) fn apply_octop_config(
    base_url: &str,
    api_key: &str,
    models: &[ToolModelInfo],
) -> Result<ToolApplyResult> {
    let path = octop_database_path()?;
    let (endpoint, api_key, models_value, model_entries) =
        octop_connection_values(base_url, api_key, models)?;
    if tool_config_preview_active() {
        return check_octop_config(base_url, api_key.as_str());
    }
    if !path.exists() {
        return Err(anyhow!(
            "OCTOP_DATABASE_NOT_FOUND: complete Octop setup before configuring CONST API ({})",
            path.display()
        ));
    }
    let before =
        fs::read(&path).with_context(|| format!("read Octop database {}", path.display()))?;
    let mut connection = open_octop_database(&path, false)?;
    validate_octop_schema(&connection)?;
    let provider = load_octop_provider(&connection)?;
    if let Some(provider) = provider.as_ref() {
        if !octop_provider_is_managed(provider)? {
            return Err(anyhow!(
                "OCTOP_PROVIDER_NAME_CONFLICT: Octop already has a different provider named {OCTOP_PROVIDER_NAME}"
            ));
        }
    }
    let active_model = load_octop_active_model(&connection)?;
    let provider_extra = octop_provider_extra(
        provider.as_ref(),
        active_model
            .as_deref()
            .filter(|value| !value.starts_with(&format!("{OCTOP_PROVIDER_NAME}/"))),
    )?;
    let model_json = serde_json::to_string(&models_value)?;
    let first_model = model_entries
        .first()
        .and_then(|value| value.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("OCTOP_MODELS_REQUIRED: no usable models were returned"))?;
    let managed_active_prefix = format!("{OCTOP_PROVIDER_NAME}/");
    let should_select_active = provider.is_none()
        || active_model
            .as_deref()
            .is_none_or(|value| value.is_empty() || value.starts_with(&managed_active_prefix));
    let desired_active = format!("{OCTOP_PROVIDER_NAME}/{first_model}");
    let provider_matches = provider.as_ref().is_some_and(|provider| {
        provider.kind == "openai"
            && provider.base_url.trim_end_matches('/') == endpoint.trim_end_matches('/')
            && provider.api_key == api_key
            && provider.enabled != 0
            && provider.note == OCTOP_PROVIDER_NOTE
            && octop_json_matches(&provider.models_json, &models_value)
    });
    let active_matches =
        !should_select_active || active_model.as_deref() == Some(desired_active.as_str());
    let already_configured = provider_matches && active_matches;
    if already_configured {
        return octop_result(
            "octop",
            &path,
            &before,
            Vec::new(),
            true,
            &[
                ("provider_name", OCTOP_PROVIDER_NAME.to_string()),
                ("model_count", model_entries.len().to_string()),
            ],
        );
    }

    let mut backups = Vec::new();
    for sibling in [
        path.clone(),
        PathBuf::from(format!("{}-wal", path.display())),
        PathBuf::from(format!("{}-shm", path.display())),
    ] {
        backup_if_exists(&sibling, "octop", &mut backups)?;
    }

    let tx = connection
        .transaction()
        .context("begin Octop configuration transaction")?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    match provider.as_ref() {
        Some(provider) => {
            tx.execute(
                "UPDATE providers SET kind = ?1, base_url = ?2, api_key = ?3, extra_json = ?4,\
                 models_json = ?5, note = ?6, enabled = 1, updated_at = ?7 WHERE id = ?8",
                rusqlite::params![
                    "openai",
                    endpoint,
                    api_key,
                    provider_extra,
                    model_json,
                    OCTOP_PROVIDER_NOTE,
                    now,
                    provider.id,
                ],
            )?;
        }
        None => {
            tx.execute(
                "INSERT INTO providers (name, kind, base_url, api_key, extra_json, models_json,\
                 note, enabled, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8, ?8)",
                rusqlite::params![
                    OCTOP_PROVIDER_NAME,
                    "openai",
                    endpoint,
                    api_key,
                    provider_extra,
                    model_json,
                    OCTOP_PROVIDER_NOTE,
                    now,
                ],
            )?;
        }
    }
    if should_select_active {
        tx.execute(
            "INSERT INTO settings (key, value) VALUES ('active_model', ?1)\
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [desired_active.as_str()],
        )?;
    }
    tx.commit()
        .context("commit Octop configuration transaction")?;
    octop_result(
        "octop",
        &path,
        &before,
        backups,
        false,
        &[
            ("provider_name", OCTOP_PROVIDER_NAME.to_string()),
            ("model_count", model_entries.len().to_string()),
        ],
    )
}

pub(crate) fn check_octop_config(base_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    let path = octop_database_path()?;
    if !path.exists() {
        let mut builder = ToolApplyBuilder::default();
        builder.files.push(path.display().to_string());
        builder.file_statuses.push(ToolFileStatus {
            path: path.display().to_string(),
            exists_before: false,
            changed: true,
            already_configured: false,
            before_sha256: String::new(),
            after_sha256: String::new(),
        });
        let mut result = builder.finish("octop");
        result
            .details
            .insert("provider_name".to_string(), OCTOP_PROVIDER_NAME.to_string());
        attach_tool_protocol(&mut result, ToolProtocol::OpenAiChat);
        return Ok(result);
    }
    let before =
        fs::read(&path).with_context(|| format!("read Octop database {}", path.display()))?;
    let connection = open_octop_database(&path, true)?;
    validate_octop_schema(&connection)?;
    let provider = load_octop_provider(&connection)?;
    let endpoint = tool_surface_url(base_url, ToolProtocol::OpenAiChat.surface());
    let api_key = api_key.trim();
    let configured = provider.as_ref().is_some_and(|provider| {
        octop_provider_is_managed(provider).unwrap_or(false)
            && provider.kind == "openai"
            && provider.base_url.trim_end_matches('/') == endpoint.trim_end_matches('/')
            && provider.api_key == api_key
            && provider.enabled != 0
            && serde_json::from_str::<Value>(&provider.models_json)
                .is_ok_and(|value| value.as_array().is_some_and(|models| !models.is_empty()))
    });
    let mut builder = ToolApplyBuilder::default();
    builder.files.push(path.display().to_string());
    builder
        .file_statuses
        .push(octop_file_status(&path, &before, configured)?);
    let mut result = builder.finish("octop");
    result
        .details
        .insert("provider_name".to_string(), OCTOP_PROVIDER_NAME.to_string());
    result.details.insert(
        "model_count".to_string(),
        provider
            .as_ref()
            .and_then(|provider| serde_json::from_str::<Value>(&provider.models_json).ok())
            .and_then(|value| value.as_array().map(|models| models.len().to_string()))
            .unwrap_or_else(|| "0".to_string()),
    );
    result.details.insert(
        "active_model".to_string(),
        load_octop_active_model(&connection)?.unwrap_or_default(),
    );
    attach_tool_protocol(&mut result, ToolProtocol::OpenAiChat);
    Ok(result)
}

pub(crate) fn remove_octop_config() -> Result<ToolApplyResult> {
    let path = octop_database_path()?;
    if !path.exists() {
        return check_octop_config("", "");
    }
    let before =
        fs::read(&path).with_context(|| format!("read Octop database {}", path.display()))?;
    let mut connection = open_octop_database(&path, false)?;
    validate_octop_schema(&connection)?;
    let Some(provider) = load_octop_provider(&connection)? else {
        return check_octop_config("", "");
    };
    if !octop_provider_is_managed(&provider)? {
        return Err(anyhow!(
            "OCTOP_PROVIDER_NAME_CONFLICT: refusing to remove an unmanaged provider named {OCTOP_PROVIDER_NAME}"
        ));
    }
    let active_model = load_octop_active_model(&connection)?;
    let previous_active_model = octop_previous_active_model(&provider)?;
    let mut backups = Vec::new();
    for sibling in [
        path.clone(),
        PathBuf::from(format!("{}-wal", path.display())),
        PathBuf::from(format!("{}-shm", path.display())),
    ] {
        backup_if_exists(&sibling, "octop", &mut backups)?;
    }
    let tx = connection
        .transaction()
        .context("begin Octop removal transaction")?;
    tx.execute("DELETE FROM providers WHERE id = ?1", [provider.id])?;
    if active_model
        .as_deref()
        .is_some_and(|value| value.starts_with(&format!("{OCTOP_PROVIDER_NAME}/")))
    {
        if let Some(previous) = previous_active_model {
            tx.execute(
                "INSERT INTO settings (key, value) VALUES ('active_model', ?1)\
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [previous.as_str()],
            )?;
        } else {
            tx.execute("DELETE FROM settings WHERE key = 'active_model'", [])?;
        }
    }
    tx.commit().context("commit Octop removal transaction")?;
    octop_result(
        "octop",
        &path,
        &before,
        backups,
        false,
        &[
            ("provider_name", OCTOP_PROVIDER_NAME.to_string()),
            ("provider_removed", "true".to_string()),
        ],
    )
}
