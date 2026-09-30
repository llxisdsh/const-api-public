use super::*;
use rusqlite::{OpenFlags, OptionalExtension, params};
use serde_json::{Value, json};

#[path = "copilot_desktop_secrets.rs"]
mod secrets;
use secrets::{Secrets, SystemSecrets};

const TOOL: &str = "copilot-desktop";
// Dedicated rows, not a replacement for the user's provider list or settings.
const PROVIDERS: [(&str, &str); 2] = [
    ("const-api-openai", "openai"),
    ("const-api-anthropic", "anthropic"),
];

fn database_path() -> PathBuf {
    tool_home_override_or_default("COPILOT_HOME", ".copilot").join("data.db")
}

fn protocol_path() -> PathBuf {
    const_api_state_path("copilot-desktop-protocol.json")
}

fn configured_protocol() -> Result<ToolProtocol> {
    let raw = read_text_or_empty(&protocol_path())?;
    if raw.trim().is_empty() {
        return Ok(ToolProtocol::OpenAiResponses);
    }
    let value: Value = serde_json::from_str(&raw).context("read Copilot desktop protocol")?;
    resolve_tool_protocol(TOOL, value.get("protocol").and_then(Value::as_str))
}

fn open_database(path: &Path, write: bool) -> Result<Connection> {
    let flags = if write {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    } else {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    };
    let connection = Connection::open_with_flags(path, flags)
        .context("TOOL_CONFIG_COPILOT_DESKTOP_INITIALIZE")?;
    connection.busy_timeout(Duration::from_secs(3))?;
    connection.execute_batch("PRAGMA foreign_keys = ON")?;
    // The app owns schema creation/migration. Never create an incomplete app DB.
    connection
        .prepare("SELECT id, name, type, settings_json FROM model_providers LIMIT 0")
        .context("TOOL_CONFIG_COPILOT_DESKTOP_INITIALIZE")?;
    connection
        .prepare(
            "SELECT id, provider_id, model_id, wire_model, display_name,
        max_prompt_tokens, max_output_tokens, wire_api_override, supported_reasoning_efforts
        FROM provider_models LIMIT 0",
        )
        .context("TOOL_CONFIG_COPILOT_DESKTOP_INITIALIZE")?;
    Ok(connection)
}

fn provider_settings(base_url: &str, kind: &str) -> Value {
    json!({
        "baseUrl": tool_surface_url(base_url, if kind == "anthropic" { "anthropic" } else { "v1" }),
        "authKind": "api_key",
        "headersJson": "{}",
        "wireApi": "responses",
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct DesktopModel {
    id: String,
    provider: String,
    model: String,
    wire_model: Option<String>,
    display_name: String,
    context: Option<i64>,
    output: Option<i64>,
    wire_api: Option<String>,
    reasoning: Option<String>,
}

fn desired_models(models: &[ToolModelInfo], fallback: ToolProtocol) -> Result<Vec<DesktopModel>> {
    configured_additional_tool_models("GitHub Copilot", models)?
        .iter()
        .map(|model| {
            let protocol = tool_model_protocol(TOOL, model, fallback);
            let (provider, wire_api) = match protocol {
                ToolProtocol::AnthropicMessages => (PROVIDERS[1].0, None),
                ToolProtocol::OpenAiChat => (PROVIDERS[0].0, Some("completions")),
                _ => (PROVIDERS[0].0, Some("responses")),
            };
            let raw_id = model.id.trim();
            Ok(DesktopModel {
                id: format!("{provider}-{}", sha256_hex(raw_id.as_bytes())),
                provider: provider.into(),
                model: raw_id.into(),
                wire_model: Some(raw_id.into()),
                display_name: if model.display_name.is_empty() {
                    raw_id.into()
                } else {
                    model.display_name.clone()
                },
                context: model
                    .context_tokens
                    .filter(|v| *v > 0)
                    .and_then(|v| v.try_into().ok()),
                output: model
                    .output_tokens
                    .filter(|v| *v > 0)
                    .and_then(|v| v.try_into().ok()),
                wire_api: wire_api.map(str::to_string),
                reasoning: if model.reasoning_efforts.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&model.reasoning_efforts)?)
                },
            })
        })
        .collect()
}

fn current_models(db: &Connection, provider: &str) -> Result<Vec<DesktopModel>> {
    let mut stmt = db.prepare(
        "SELECT id, provider_id, model_id, wire_model, display_name,
        max_prompt_tokens, max_output_tokens, wire_api_override, supported_reasoning_efforts
        FROM provider_models WHERE provider_id = ?1 ORDER BY id",
    )?;
    Ok(stmt
        .query_map([provider], |row| {
            Ok(DesktopModel {
                id: row.get(0)?,
                provider: row.get(1)?,
                model: row.get(2)?,
                wire_model: row.get(3)?,
                display_name: row.get(4)?,
                context: row.get(5)?,
                output: row.get(6)?,
                wire_api: row.get(7)?,
                reasoning: row.get(8)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

fn current_provider(db: &Connection, id: &str) -> Result<Option<(String, String, Value)>> {
    let row = db
        .query_row(
            "SELECT name, type, settings_json FROM model_providers WHERE id = ?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    row.map(|(name, kind, settings)| Ok((name, kind, serde_json::from_str(&settings)?)))
        .transpose()
}

fn result(path: &Path, configured: bool, changed: bool, exists: bool) -> ToolApplyResult {
    ToolApplyResult {
        tool: TOOL.into(),
        backups: vec![],
        files: if changed && !tool_config_preview_active() {
            vec![path.display().to_string()]
        } else {
            vec![]
        },
        already_configured: configured,
        file_statuses: vec![ToolFileStatus {
            path: path.display().to_string(),
            exists_before: exists,
            changed,
            already_configured: configured,
            before_sha256: String::new(),
            after_sha256: String::new(),
        }],
        details: HashMap::from([("tool_protocol".into(), "openai_responses".into())]),
    }
}

pub(super) fn check(base_url: &str, api_key: &str) -> Result<ToolApplyResult> {
    let mut result = check_at(&database_path(), base_url, api_key, &mut SystemSecrets)?;
    attach_tool_protocol(&mut result, configured_protocol()?);
    Ok(result)
}

fn check_at(
    path: &Path,
    base_url: &str,
    api_key: &str,
    secrets: &mut impl Secrets,
) -> Result<ToolApplyResult> {
    if !path.exists() {
        return Ok(result(path, false, false, false));
    }
    let db = open_database(path, false)?;
    let mut checked = result(path, false, false, true);
    for (id, kind) in PROVIDERS {
        if let Some((_, actual_kind, settings)) = current_provider(&db, id)? {
            checked
                .details
                .insert("has_managed_config".into(), "true".into());
            let expected = provider_settings(base_url, kind);
            if actual_kind != kind
                || settings != expected
                || current_models(&db, id)?.is_empty()
                || secrets.read(id)?.as_deref() != Some(api_key)
            {
                return Ok(checked);
            }
        }
    }
    checked.already_configured = checked.details.contains_key("has_managed_config");
    Ok(checked)
}

pub(super) fn apply(
    base_url: &str,
    api_key: &str,
    models: &[ToolModelInfo],
    protocol: ToolProtocol,
) -> Result<ToolApplyResult> {
    let desired = desired_models(models, protocol)?;
    let path = protocol_path();
    // Only the fallback choice lives in our own metadata, never a made-up field
    // in the vendor schema. Reuse the existing file transaction/ownership layer.
    with_tool_config_file_transaction(TOOL, std::slice::from_ref(&path), || {
        let mut metadata = ToolApplyBuilder::default();
        write_text_with_backup(
            &path,
            &json!({"protocol": protocol.as_str()}).to_string(),
            TOOL,
            &mut metadata,
        )?;
        let mut result = apply_at(
            &database_path(),
            base_url,
            Some(api_key),
            &desired,
            &mut SystemSecrets,
        )?;
        result.already_configured &= metadata.file_statuses.iter().all(|s| s.already_configured);
        result.files.extend(metadata.files);
        result.backups.extend(metadata.backups);
        result.file_statuses.extend(metadata.file_statuses);
        attach_tool_protocol(&mut result, protocol);
        Ok(result)
    })
}

pub(super) fn remove() -> Result<ToolApplyResult> {
    let path = database_path();
    with_tool_config_file_transaction(TOOL, &[protocol_path()], || {
        let metadata = restore_tool_config_from_manifest(TOOL)?;
        let mut result = if path.exists() {
            apply_at(&path, "", None, &[], &mut SystemSecrets)?
        } else {
            result(&path, false, false, false)
        };
        result.files.extend(metadata.files);
        result.backups.extend(metadata.backups);
        result.file_statuses.extend(metadata.file_statuses);
        Ok(result)
    })
}

fn apply_at(
    path: &Path,
    base_url: &str,
    api_key: Option<&str>,
    desired: &[DesktopModel],
    secrets: &mut impl Secrets,
) -> Result<ToolApplyResult> {
    let mut db = open_database(path, !tool_config_preview_active())?;
    let mut changes = Vec::new();
    for (id, kind) in PROVIDERS {
        let before = current_provider(&db, id)?;
        let wanted: Vec<_> = desired
            .iter()
            .filter(|m| m.provider == id)
            .cloned()
            .collect();
        let old_key = secrets.read(id)?;
        let new_key = if wanted.is_empty() { None } else { api_key };
        let settings = provider_settings(base_url, kind);
        let name = format!(
            "CONST API · {}",
            if kind == "anthropic" {
                "Messages"
            } else {
                "OpenAI"
            }
        );
        let same = if wanted.is_empty() {
            before.is_none() && old_key.is_none()
        } else {
            let current = current_models(&db, id)?;
            let models_match = wanted.iter().all(|wanted| {
                current.iter().any(|actual| {
                    let mut actual = actual.clone();
                    actual.id.clone_from(&wanted.id);
                    actual == *wanted
                })
            }) && current
                .iter()
                .filter(|m| m.id.starts_with(&format!("{id}-")))
                .all(|m| wanted.iter().any(|wanted| wanted.model == m.model));
            before.as_ref() == Some(&(name, kind.into(), settings))
                && models_match
                && old_key.as_deref() == new_key
        };
        if !same {
            changes.push((id, kind, old_key, new_key, wanted));
        }
    }
    let changed = !changes.is_empty();
    let response = result(path, !changed && api_key.is_some(), changed, true);
    if !changed || tool_config_preview_active() {
        return Ok(response);
    }
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    for (id, kind, _, _, models) in &changes {
        if models.is_empty() {
            // FK SET NULL detaches our provider from sessions; no history is deleted.
            tx.execute("DELETE FROM model_providers WHERE id = ?1", [id])?;
            continue;
        }
        tx.execute("INSERT INTO model_providers(id, name, type, settings_json) VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(id) DO UPDATE SET name=excluded.name, type=excluded.type, settings_json=excluded.settings_json,
            updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')", params![id,
            format!("CONST API · {}", if *kind == "anthropic" { "Messages" } else { "OpenAI" }),
            kind, provider_settings(base_url, kind).to_string()])?;
        for model in models {
            tx.execute("INSERT INTO provider_models(id,provider_id,model_id,wire_model,display_name,
                max_prompt_tokens,max_output_tokens,wire_api_override,supported_reasoning_efforts)
                VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(provider_id,model_id) DO UPDATE SET
                wire_model=excluded.wire_model,display_name=excluded.display_name,
                max_prompt_tokens=excluded.max_prompt_tokens,max_output_tokens=excluded.max_output_tokens,
                wire_api_override=excluded.wire_api_override,supported_reasoning_efforts=excluded.supported_reasoning_efforts,
                updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')", params![
                model.id, id, model.model, model.wire_model, model.display_name,
                model.context, model.output, model.wire_api, model.reasoning])?;
        }
        // Do not delete extra models a user added to our provider in the app.
        // Retire only our stable IDs, which also keeps preview idempotent.
        for old in current_models(&tx, id)? {
            if old.id.starts_with(&format!("{id}-")) && !models.iter().any(|m| m.model == old.model)
            {
                tx.execute("DELETE FROM provider_models WHERE id=?1", [old.id])?;
            }
        }
    }
    let mut applied = 0;
    let write_result = (|| {
        for (id, _, _, new_key, _) in &changes {
            secrets.write(id, *new_key)?;
            applied += 1;
        }
        tx.commit().context("save Copilot desktop models")
    })();
    if let Err(error) = write_result {
        for (id, _, old_key, _, _) in changes[..applied].iter().rev() {
            if let Err(restore) = secrets.write(id, old_key.as_deref()) {
                return Err(error.context(format!("Copilot credential rollback failed: {restore}")));
            }
        }
        return Err(error);
    }
    Ok(response)
}

#[cfg(test)]
#[path = "tests/copilot_desktop.rs"]
mod tests;
