#[cfg(not(test))]
const LOCAL_RESOURCE_OWNER_DB_FILE: &str = "resource-owners-v1.sqlite3";
const LOCAL_RESOURCE_OWNER_MAX_ENTRIES: i64 = 50_000;
const LOCAL_RESOURCE_ID_MAX_BYTES: usize = 1_024;
const LOCAL_RESOURCE_STREAM_SCAN_MAX_BYTES: usize = 256 * 1_024;

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct LocalResourceOwnerKey {
    surface: crate::surface::ApiSurface,
    resource_type: String,
    resource_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LocalResourceOwner {
    key: LocalResourceOwnerKey,
    channel_id: String,
    route_model: String,
    updated_at_unix: i64,
}

#[derive(Debug)]
struct LocalResourceOwnerRegistry {
    owners: std::sync::RwLock<HashMap<LocalResourceOwnerKey, LocalResourceOwner>>,
    database_path: Option<PathBuf>,
    persistence_gate: Mutex<()>,
}

impl LocalResourceOwnerRegistry {
    #[cfg(test)]
    fn in_memory() -> Arc<Self> {
        Arc::new(Self {
            owners: std::sync::RwLock::new(HashMap::new()),
            database_path: None,
            persistence_gate: Mutex::new(()),
        })
    }

    fn open(database_path: PathBuf) -> Arc<Self> {
        let owners = match load_local_resource_owners(&database_path) {
            Ok(owners) => owners,
            Err(error) => {
                log::warn!(
                    "[const-api][resource-owner] load failed code=local_resource_owner_store_load_failed error={error}"
                );
                HashMap::new()
            }
        };
        Arc::new(Self {
            owners: std::sync::RwLock::new(owners),
            database_path: Some(database_path),
            persistence_gate: Mutex::new(()),
        })
    }

    fn get(
        &self,
        surface: crate::surface::ApiSurface,
        resource_type: &str,
        resource_id: &str,
    ) -> Option<LocalResourceOwner> {
        let key = normalize_local_resource_owner_key(surface, resource_type, resource_id)?;
        self.owners
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&key)
            .cloned()
    }

    async fn bind_many(&self, owners: Vec<LocalResourceOwner>) -> Result<()> {
        let owners = owners
            .into_iter()
            .map(normalize_local_resource_owner)
            .collect::<Result<Vec<_>>>()?;
        if owners.is_empty() {
            return Ok(());
        }
        let _gate = self.persistence_gate.lock().await;
        {
            let current = self
                .owners
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for owner in &owners {
                if let Some(existing) = current.get(&owner.key) {
                    if existing.channel_id != owner.channel_id {
                        return Err(anyhow!(
                            "resource owner conflict for {} {}",
                            owner.key.resource_type,
                            owner.key.resource_id
                        ));
                    }
                }
            }
        }

        // Publish the owner in memory before the caller can expose the resource id.
        // Reads are therefore database-free and a follow-up request cannot race the
        // local persistence step.
        {
            let mut current = self
                .owners
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for owner in &owners {
                current.insert(owner.key.clone(), owner.clone());
            }
        }
        let Some(path) = self.database_path.clone() else {
            return Ok(());
        };
        let persisted = owners.clone();
        match tokio::task::spawn_blocking(move || persist_local_resource_owners(&path, &persisted))
            .await
        {
            Ok(result) => result,
            Err(error) => Err(anyhow!("resource owner persistence task failed: {error}")),
        }
    }

    async fn delete(
        &self,
        surface: crate::surface::ApiSurface,
        resource_type: &str,
        resource_id: &str,
    ) -> Result<()> {
        let Some(key) = normalize_local_resource_owner_key(surface, resource_type, resource_id)
        else {
            return Ok(());
        };
        let _gate = self.persistence_gate.lock().await;
        self.owners
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&key);
        let Some(path) = self.database_path.clone() else {
            return Ok(());
        };
        match tokio::task::spawn_blocking(move || delete_local_resource_owner(&path, &key)).await {
            Ok(result) => result,
            Err(error) => Err(anyhow!("resource owner delete task failed: {error}")),
        }
    }
}

#[cfg(not(test))]
fn local_resource_owner_database_path(config_path: &std::path::Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("state")
        .join(LOCAL_RESOURCE_OWNER_DB_FILE)
}

fn open_local_resource_owner_database(path: &std::path::Path) -> Result<rusqlite::Connection> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let connection = rusqlite::Connection::open(path)?;
    connection.busy_timeout(Duration::from_secs(2))?;
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA wal_autocheckpoint = 256;
         PRAGMA journal_size_limit = 4194304;
         CREATE TABLE IF NOT EXISTS local_resource_owners (
             surface TEXT NOT NULL,
             resource_type TEXT NOT NULL,
             resource_id TEXT NOT NULL,
             channel_id TEXT NOT NULL,
             route_model TEXT NOT NULL DEFAULT '',
             updated_at_unix INTEGER NOT NULL,
             PRIMARY KEY(surface, resource_type, resource_id)
         );
         CREATE INDEX IF NOT EXISTS idx_local_resource_owners_updated
             ON local_resource_owners(updated_at_unix DESC);",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(path, permissions)?;
    }
    Ok(connection)
}

fn load_local_resource_owners(
    path: &std::path::Path,
) -> Result<HashMap<LocalResourceOwnerKey, LocalResourceOwner>> {
    let connection = open_local_resource_owner_database(path)?;
    let mut statement = connection.prepare(
        "SELECT surface, resource_type, resource_id, channel_id, route_model, updated_at_unix
         FROM local_resource_owners
         ORDER BY updated_at_unix DESC
         LIMIT ?1",
    )?;
    let rows = statement.query_map([LOCAL_RESOURCE_OWNER_MAX_ENTRIES], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, i64>(5)?,
        ))
    })?;
    let mut owners = HashMap::new();
    for row in rows {
        let (surface, resource_type, resource_id, channel_id, route_model, updated_at_unix) = row?;
        let Some(surface) = local_resource_owner_surface(&surface) else {
            continue;
        };
        let owner = normalize_local_resource_owner(LocalResourceOwner {
            key: LocalResourceOwnerKey {
                surface,
                resource_type,
                resource_id,
            },
            channel_id,
            route_model,
            updated_at_unix,
        })?;
        owners.insert(owner.key.clone(), owner);
    }
    Ok(owners)
}

fn persist_local_resource_owners(
    path: &std::path::Path,
    owners: &[LocalResourceOwner],
) -> Result<()> {
    let mut connection = open_local_resource_owner_database(path)?;
    let transaction = connection.transaction()?;
    for owner in owners {
        transaction.execute(
            "INSERT INTO local_resource_owners(
                 surface, resource_type, resource_id, channel_id, route_model, updated_at_unix
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(surface, resource_type, resource_id) DO UPDATE SET
                 channel_id = excluded.channel_id,
                 route_model = excluded.route_model,
                 updated_at_unix = excluded.updated_at_unix
             WHERE local_resource_owners.channel_id = excluded.channel_id",
            rusqlite::params![
                owner.key.surface.as_str(),
                owner.key.resource_type,
                owner.key.resource_id,
                owner.channel_id,
                owner.route_model,
                owner.updated_at_unix,
            ],
        )?;
    }
    transaction.execute(
        "DELETE FROM local_resource_owners
         WHERE rowid IN (
             SELECT rowid FROM local_resource_owners
             ORDER BY updated_at_unix DESC, rowid DESC
             LIMIT -1 OFFSET ?1
         )",
        [LOCAL_RESOURCE_OWNER_MAX_ENTRIES],
    )?;
    transaction.commit()?;
    Ok(())
}

fn delete_local_resource_owner(
    path: &std::path::Path,
    key: &LocalResourceOwnerKey,
) -> Result<()> {
    let connection = open_local_resource_owner_database(path)?;
    connection.execute(
        "DELETE FROM local_resource_owners
         WHERE surface = ?1 AND resource_type = ?2 AND resource_id = ?3",
        rusqlite::params![key.surface.as_str(), key.resource_type, key.resource_id],
    )?;
    Ok(())
}

fn local_resource_owner_surface(value: &str) -> Option<crate::surface::ApiSurface> {
    match value.trim().to_ascii_lowercase().as_str() {
        "openai" => Some(crate::surface::ApiSurface::OpenAi),
        "anthropic" => Some(crate::surface::ApiSurface::Anthropic),
        "gemini" => Some(crate::surface::ApiSurface::Gemini),
        _ => None,
    }
}

fn normalize_local_resource_owner_key(
    surface: crate::surface::ApiSurface,
    resource_type: &str,
    resource_id: &str,
) -> Option<LocalResourceOwnerKey> {
    let resource_type = resource_type.trim().to_ascii_lowercase();
    let resource_id = resource_id.trim();
    if resource_type.is_empty()
        || resource_type.len() > 64
        || resource_id.is_empty()
        || resource_id.len() > LOCAL_RESOURCE_ID_MAX_BYTES
    {
        return None;
    }
    Some(LocalResourceOwnerKey {
        surface,
        resource_type,
        resource_id: resource_id.to_string(),
    })
}

fn normalize_local_resource_owner(mut owner: LocalResourceOwner) -> Result<LocalResourceOwner> {
    owner.key = normalize_local_resource_owner_key(
        owner.key.surface,
        &owner.key.resource_type,
        &owner.key.resource_id,
    )
    .ok_or_else(|| anyhow!("invalid local resource owner identity"))?;
    owner.channel_id = owner.channel_id.trim().to_string();
    owner.route_model = owner.route_model.trim().to_string();
    if owner.channel_id.is_empty() || owner.channel_id.len() > 512 || owner.route_model.len() > 512 {
        return Err(anyhow!("invalid local resource owner route"));
    }
    if owner.updated_at_unix <= 0 {
        owner.updated_at_unix = now_unix();
    }
    Ok(owner)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LocalResourceReference {
    surface: crate::surface::ApiSurface,
    resource_type: &'static str,
    resource_id: String,
    required: bool,
}

#[derive(Clone, Copy)]
struct LocalNativeResourceRule {
    surface: crate::surface::ApiSurface,
    resource_type: &'static str,
    collection_path: &'static str,
    reserved_ids: &'static [&'static str],
}

const LOCAL_NATIVE_RESOURCE_RULES: &[LocalNativeResourceRule] = &[
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::OpenAi, resource_type: "chat_completion", collection_path: "/v1/chat/completions", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::OpenAi, resource_type: "realtime_call", collection_path: "/v1/realtime/calls", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::OpenAi, resource_type: "realtime_call", collection_path: "/v1/live", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::OpenAi, resource_type: "realtime_translation_call", collection_path: "/v1/realtime/translations/calls", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::OpenAi, resource_type: "vector_store", collection_path: "/v1/vector_stores", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::OpenAi, resource_type: "container", collection_path: "/v1/containers", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::OpenAi, resource_type: "skill", collection_path: "/v1/skills", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::OpenAi, resource_type: "assistant", collection_path: "/v1/assistants", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::OpenAi, resource_type: "thread", collection_path: "/v1/threads", reserved_ids: &["runs"] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::OpenAi, resource_type: "fine_tuning_job", collection_path: "/v1/fine_tuning/jobs", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::OpenAi, resource_type: "eval", collection_path: "/v1/evals", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::OpenAi, resource_type: "chatkit_session", collection_path: "/v1/chatkit/sessions", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::OpenAi, resource_type: "chatkit_thread", collection_path: "/v1/chatkit/threads", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::OpenAi, resource_type: "video_character", collection_path: "/v1/videos/characters", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::OpenAi, resource_type: "voice_consent", collection_path: "/v1/audio/voice_consents", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::OpenAi, resource_type: "voice", collection_path: "/v1/audio/voices", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::Anthropic, resource_type: "skill", collection_path: "/anthropic/v1/skills", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::Anthropic, resource_type: "agent", collection_path: "/anthropic/v1/agents", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::Anthropic, resource_type: "session", collection_path: "/anthropic/v1/sessions", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::Anthropic, resource_type: "environment", collection_path: "/anthropic/v1/environments", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::Anthropic, resource_type: "deployment", collection_path: "/anthropic/v1/deployments", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::Anthropic, resource_type: "deployment_run", collection_path: "/anthropic/v1/deployment_runs", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::Anthropic, resource_type: "vault", collection_path: "/anthropic/v1/vaults", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::Anthropic, resource_type: "memory_store", collection_path: "/anthropic/v1/memory_stores", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::Anthropic, resource_type: "dream", collection_path: "/anthropic/v1/dreams", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::Anthropic, resource_type: "tunnel", collection_path: "/anthropic/v1/tunnels", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::Anthropic, resource_type: "user_profile", collection_path: "/anthropic/v1/user_profiles", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::Gemini, resource_type: "batch", collection_path: "/gemini/v1beta/batches", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::Gemini, resource_type: "file_search_store", collection_path: "/gemini/v1beta/fileSearchStores", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::Gemini, resource_type: "generated_file", collection_path: "/gemini/v1beta/generatedFiles", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::Gemini, resource_type: "tuned_model", collection_path: "/gemini/v1beta/tunedModels", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::Gemini, resource_type: "corpus", collection_path: "/gemini/v1beta/corpora", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::Gemini, resource_type: "environment", collection_path: "/gemini/v1beta/environments", reserved_ids: &[] },
    LocalNativeResourceRule { surface: crate::surface::ApiSurface::Gemini, resource_type: "operation", collection_path: "/gemini/v1beta/operations", reserved_ids: &[] },
];

fn local_native_resource_rule_for_collection(
    surface: crate::surface::ApiSurface,
    path: &str,
) -> Option<LocalNativeResourceRule> {
    let path = path.split_once('?').map_or(path, |(path, _)| path);
    LOCAL_NATIVE_RESOURCE_RULES
        .iter()
        .copied()
        .find(|rule| rule.surface == surface && path == rule.collection_path)
}

fn local_realtime_call_id_from_location(headers: &warp::http::HeaderMap) -> Option<String> {
    let location = headers
        .get(reqwest::header::LOCATION)?
        .to_str()
        .ok()?
        .split('?')
        .next()?;
    location.rsplit('/').find_map(|segment| {
        let rtc = segment.starts_with("rtc_") && segment.len() > "rtc_".len();
        let uuid = segment.len() == 36
            && segment.char_indices().all(|(index, character)| match index {
                8 | 13 | 18 | 23 => character == '-',
                _ => character.is_ascii_hexdigit(),
            });
        (rtc || uuid).then(|| segment.to_string())
    })
}

fn local_native_resource_reference(
    surface: crate::surface::ApiSurface,
    path: &str,
) -> Option<LocalResourceReference> {
    let path = path.split_once('?').map_or(path, |(path, _)| path);
    if surface == crate::surface::ApiSurface::Gemini {
        if let Some(value) = path.strip_prefix("/gemini/upload/v1beta/fileSearchStores/") {
            let resource_id = value.split(':').next().unwrap_or_default();
            if !resource_id.is_empty() && !resource_id.contains('/') {
                return Some(LocalResourceReference {
                    surface,
                    resource_type: "file_search_store",
                    resource_id: resource_id.to_string(),
                    required: true,
                });
            }
        }
    }
    for rule in LOCAL_NATIVE_RESOURCE_RULES
        .iter()
        .copied()
        .filter(|rule| rule.surface == surface)
    {
        let Some(value) = path
            .strip_prefix(rule.collection_path)
            .and_then(|value| value.strip_prefix('/'))
        else {
            continue;
        };
        let resource_id = value.split('/').next().unwrap_or_default();
        if resource_id.is_empty() || rule.reserved_ids.contains(&resource_id) {
            continue;
        }
        return Some(LocalResourceReference {
            surface,
            resource_type: rule.resource_type,
            resource_id: resource_id.to_string(),
            required: true,
        });
    }
    None
}

fn local_resource_references(
    route: &crate::surface::ApiRoute,
    path: &str,
    body: &[u8],
) -> Vec<LocalResourceReference> {
    let reference = |resource_type, resource_id: String, required| LocalResourceReference {
        surface: route.surface,
        resource_type,
        resource_id,
        required,
    };
    if let Some(reference) = local_native_resource_reference(route.surface, path) {
        return vec![reference];
    }
    match route.operation {
        crate::surface::ApiOperation::ResponsesGet
        | crate::surface::ApiOperation::ResponsesDelete
        | crate::surface::ApiOperation::ResponsesCancel
        | crate::surface::ApiOperation::ResponsesInputItems => {
            return local_openai_response_id_from_path(path)
                .map(|id| vec![reference("response", id, true)])
                .unwrap_or_default();
        }
        crate::surface::ApiOperation::InteractionsGet
        | crate::surface::ApiOperation::InteractionsDelete
        | crate::surface::ApiOperation::InteractionsCancel => {
            return local_gemini_interaction_id_from_path(path)
                .map(|id| vec![reference("interaction", id, true)])
                .unwrap_or_default();
        }
        crate::surface::ApiOperation::ConversationsGet
        | crate::surface::ApiOperation::ConversationsUpdate
        | crate::surface::ApiOperation::ConversationsDelete
        | crate::surface::ApiOperation::ConversationItemsCreate
        | crate::surface::ApiOperation::ConversationItemsList
        | crate::surface::ApiOperation::ConversationItemGet
        | crate::surface::ApiOperation::ConversationItemDelete => {
            return local_openai_conversation_id_from_path(path)
                .map(|id| vec![reference("conversation", id, true)])
                .unwrap_or_default();
        }
        crate::surface::ApiOperation::FilesGet
        | crate::surface::ApiOperation::FilesDelete
        | crate::surface::ApiOperation::FilesContent => {
            return local_file_id_from_path(route.surface, path)
                .map(|id| vec![reference("file", id, true)])
                .unwrap_or_default();
        }
        crate::surface::ApiOperation::UploadPartsCreate
        | crate::surface::ApiOperation::UploadsComplete
        | crate::surface::ApiOperation::UploadsCancel => {
            return local_openai_upload_id_from_path(path)
                .map(|id| vec![reference("upload", id, true)])
                .unwrap_or_default();
        }
        crate::surface::ApiOperation::VideosGet
        | crate::surface::ApiOperation::VideosDelete
        | crate::surface::ApiOperation::VideosContent
        | crate::surface::ApiOperation::VideosRemix => {
            return local_openai_video_id_from_path(path)
                .map(|id| vec![reference("video", id, true)])
                .unwrap_or_default();
        }
        crate::surface::ApiOperation::BatchesGet
        | crate::surface::ApiOperation::BatchesCancel
        | crate::surface::ApiOperation::BatchesDelete
        | crate::surface::ApiOperation::BatchesResults => {
            return local_batch_id_from_path(route.surface, path)
                .map(|id| vec![reference("batch", id, true)])
                .unwrap_or_default();
        }
        crate::surface::ApiOperation::CachedContentsGet
        | crate::surface::ApiOperation::CachedContentsUpdate
        | crate::surface::ApiOperation::CachedContentsDelete => {
            return local_gemini_cached_content_id_from_path(path)
                .map(|id| vec![reference("cached_content", id, true)])
                .unwrap_or_default();
        }
        _ => {}
    }
    let Ok(fields) = serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(body)
    else {
        return Vec::new();
    };
    let mut references = Vec::new();
    match route.operation {
        crate::surface::ApiOperation::Responses => {
            if let Some(id) = fields
                .get("previous_response_id")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
            {
                references.push(reference("response", id.to_string(), false));
            }
            if let Some(id) = fields.get("conversation").and_then(local_resource_id_from_json) {
                references.push(reference("conversation", id, false));
            }
        }
        crate::surface::ApiOperation::InteractionsCreate => {
            if let Some(id) = fields
                .get("previous_interaction_id")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
            {
                references.push(reference("interaction", id.to_string(), false));
            }
        }
        crate::surface::ApiOperation::BatchesCreate => {
            if let Some(id) = fields
                .get("input_file_id")
                .and_then(local_resource_id_from_json)
            {
                references.push(reference("file", id, false));
            }
        }
        _ => {}
    }
    if matches!(
        route.operation,
        crate::surface::ApiOperation::GenerateContent
            | crate::surface::ApiOperation::StreamGenerateContent
            | crate::surface::ApiOperation::InteractionsCreate
    ) {
        if let Some(id) = fields
            .get("cachedContent")
            .or_else(|| fields.get("cached_content"))
            .and_then(local_resource_id_from_json)
            .and_then(|value| local_gemini_cached_content_id(&value))
        {
            references.push(reference("cached_content", id, true));
        }
    }
    if matches!(
        route.operation,
        crate::surface::ApiOperation::Responses
            | crate::surface::ApiOperation::Messages
            | crate::surface::ApiOperation::GenerateContent
            | crate::surface::ApiOperation::StreamGenerateContent
            | crate::surface::ApiOperation::InteractionsCreate
    ) {
        collect_local_file_references(
            &serde_json::Value::Object(fields),
            &mut references,
            route.surface,
        );
    }
    references.sort_by(|left, right| left.resource_id.cmp(&right.resource_id));
    references.dedup_by(|left, right| {
        left.resource_type == right.resource_type && left.resource_id == right.resource_id
    });
    references
}

fn collect_local_file_references(
    value: &serde_json::Value,
    references: &mut Vec<LocalResourceReference>,
    surface: crate::surface::ApiSurface,
) {
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                collect_local_file_references(item, references, surface);
            }
        }
        serde_json::Value::Object(fields) => {
            for (key, value) in fields {
                let id = match key.as_str() {
                    "file_id" | "fileId" | "input_file_id" | "inputFileId" => {
                        local_resource_id_from_json(value)
                    }
                    "file_uri" | "fileUri" | "uri" => value
                        .as_str()
                        .and_then(local_gemini_file_id_from_uri),
                    _ => None,
                };
                if let Some(resource_id) = id {
                    references.push(LocalResourceReference {
                        surface,
                        resource_type: "file",
                        resource_id,
                        required: false,
                    });
                }
                collect_local_file_references(value, references, surface);
            }
        }
        _ => {}
    }
}

fn local_gemini_file_id_from_uri(uri: &str) -> Option<String> {
    let path = uri
        .split_once('?')
        .map_or(uri, |(path, _)| path)
        .trim_end_matches('/');
    let id = path.rsplit_once("/files/")?.1;
    (!id.is_empty() && !id.contains('/')).then(|| id.to_string())
}

fn local_resource_id_from_json(value: &serde_json::Value) -> Option<String> {
    value
        .as_str()
        .or_else(|| value.get("id").and_then(serde_json::Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn local_openai_response_id_from_path(path: &str) -> Option<String> {
    let mut value = path
        .split_once('?')
        .map_or(path, |(path, _)| path)
        .strip_prefix("/v1/responses/")?;
    value = value.strip_suffix("/cancel").unwrap_or(value);
    value = value.strip_suffix("/input_items").unwrap_or(value);
    (!value.is_empty()
        && value != "compact"
        && value != "input_tokens"
        && !value.contains('/'))
    .then(|| value.to_string())
}

fn local_gemini_interaction_id_from_path(path: &str) -> Option<String> {
    let mut value = path
        .split_once('?')
        .map_or(path, |(path, _)| path)
        .strip_prefix("/gemini/v1beta/interactions/")?;
    value = value.strip_suffix("/cancel").unwrap_or(value);
    (!value.is_empty() && !value.contains('/')).then(|| value.to_string())
}

fn local_gemini_cached_content_id_from_path(path: &str) -> Option<String> {
    let value = path
        .split_once('?')
        .map_or(path, |(path, _)| path)
        .strip_prefix("/gemini/v1beta/cachedContents/")?;
    local_gemini_cached_content_id(value)
}

fn local_gemini_cached_content_id(value: &str) -> Option<String> {
    let value = value
        .split_once('?')
        .map_or(value, |(path, _)| path)
        .trim_end_matches('/');
    let value = value
        .rsplit_once("/cachedContents/")
        .map(|(_, id)| id)
        .or_else(|| value.strip_prefix("cachedContents/"))
        .unwrap_or(value);
    (!value.is_empty() && !value.contains('/')).then(|| value.to_string())
}

fn local_openai_conversation_id_from_path(path: &str) -> Option<String> {
    let value = path
        .split_once('?')
        .map_or(path, |(path, _)| path)
        .strip_prefix("/v1/conversations/")?;
    let id = value.split('/').next().unwrap_or_default();
    (!id.is_empty()).then(|| id.to_string())
}

fn local_file_id_from_path(
    surface: crate::surface::ApiSurface,
    path: &str,
) -> Option<String> {
    let prefix = match surface {
        crate::surface::ApiSurface::OpenAi => "/v1/files/",
        crate::surface::ApiSurface::Anthropic => "/anthropic/v1/files/",
        crate::surface::ApiSurface::Gemini => "/gemini/v1beta/files/",
    };
    let value = path
        .split_once('?')
        .map_or(path, |(path, _)| path)
        .strip_prefix(prefix)?;
    let id = value.strip_suffix("/content").unwrap_or(value);
    (!id.is_empty() && !id.contains('/')).then(|| id.to_string())
}

fn local_batch_id_from_path(
    surface: crate::surface::ApiSurface,
    path: &str,
) -> Option<String> {
    let prefix = match surface {
        crate::surface::ApiSurface::OpenAi => "/v1/batches/",
        crate::surface::ApiSurface::Anthropic => "/anthropic/v1/messages/batches/",
        crate::surface::ApiSurface::Gemini => return None,
    };
    let value = path
        .split_once('?')
        .map_or(path, |(path, _)| path)
        .strip_prefix(prefix)?;
    let id = ["/cancel", "/results"]
        .into_iter()
        .find_map(|suffix| value.strip_suffix(suffix))
        .unwrap_or(value);
    (!id.is_empty() && !id.contains('/')).then(|| id.to_string())
}

fn local_openai_upload_id_from_path(path: &str) -> Option<String> {
    let value = path
        .split_once('?')
        .map_or(path, |(path, _)| path)
        .strip_prefix("/v1/uploads/")?;
    let id = ["/parts", "/complete", "/cancel"]
        .into_iter()
        .find_map(|suffix| value.strip_suffix(suffix))
        .unwrap_or(value);
    (!id.is_empty() && !id.contains('/')).then(|| id.to_string())
}

fn local_openai_video_id_from_path(path: &str) -> Option<String> {
    let value = path
        .split_once('?')
        .map_or(path, |(path, _)| path)
        .strip_prefix("/v1/videos/")?;
    let id = ["/content", "/remix"]
        .into_iter()
        .find_map(|suffix| value.strip_suffix(suffix))
        .unwrap_or(value);
    (!id.is_empty()
        && !id.contains('/')
        && !matches!(id, "edits" | "extensions" | "characters"))
    .then(|| id.to_string())
}

struct LocalResourceRouteError {
    surface: crate::surface::ApiSurface,
    status: StatusCode,
    message: &'static str,
}

fn select_resource_owned_local_channel<'a>(
    registry: &LocalResourceOwnerRegistry,
    config: &'a ClientConfig,
    route: &crate::surface::ApiRoute,
    path: &str,
    body: &[u8],
    ready_channel_ids: &HashSet<String>,
) -> std::result::Result<Option<LocalChannelSelection<'a>>, LocalResourceRouteError> {
    let references = local_resource_references(route, path, body);
    let mut selected: Option<LocalResourceOwner> = None;
    for reference in references {
        let owner = registry.get(
            reference.surface,
            reference.resource_type,
            &reference.resource_id,
        );
        let Some(owner) = owner else {
            if reference.required {
                return Err(LocalResourceRouteError {
                    surface: route.surface,
                    status: StatusCode::NOT_FOUND,
                    message: "resource owner is unknown; lifecycle requests cannot be rebalanced",
                });
            }
            continue;
        };
        if selected
            .as_ref()
            .is_some_and(|current| current.channel_id != owner.channel_id)
        {
            return Err(LocalResourceRouteError {
                surface: route.surface,
                status: StatusCode::CONFLICT,
                message: "referenced resources belong to different channels",
            });
        }
        selected = Some(owner);
    }
    let Some(owner) = selected else {
        return Ok(None);
    };
    let Some(channel) = local_channels(config).find(|channel| channel.id == owner.channel_id) else {
        return Err(LocalResourceRouteError {
            surface: route.surface,
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: "the resource owner channel is no longer configured",
        });
    };
    if !ready_channel_ids.contains(&channel.id) {
        return Err(LocalResourceRouteError {
            surface: route.surface,
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: "the resource owner channel is temporarily unavailable",
        });
    }
    Ok(Some(LocalChannelSelection {
        channel,
        route_model: request_model_from_body(body).unwrap_or(owner.route_model),
        substituted: false,
    }))
}

#[derive(Default)]
struct LocalResourceIdentityScanner {
    pending: Vec<u8>,
    generic_resource_type: Option<&'static str>,
    generic_resource_id: Option<String>,
    response_id: Option<String>,
    conversation_id: Option<String>,
    interaction_id: Option<String>,
}

impl LocalResourceIdentityScanner {
    fn write(&mut self, chunk: &[u8]) -> Vec<(&'static str, String)> {
        if chunk.is_empty() || self.pending.len() >= LOCAL_RESOURCE_STREAM_SCAN_MAX_BYTES {
            return Vec::new();
        }
        let remaining = LOCAL_RESOURCE_STREAM_SCAN_MAX_BYTES - self.pending.len();
        self.pending
            .extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        let mut discovered = Vec::new();
        while let Some(newline) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line = self.pending.drain(..=newline).collect::<Vec<_>>();
            let line = line.strip_suffix(b"\n").unwrap_or(&line);
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let Some(payload) = line.strip_prefix(b"data:") else {
                continue;
            };
            let payload = trim_ascii_whitespace(payload);
            if payload.is_empty() || payload == b"[DONE]" {
                continue;
            }
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(payload) else {
                continue;
            };
            if self.generic_resource_id.is_none() {
                if let (Some(resource_type), Some(id)) = (
                    self.generic_resource_type,
                    value.get("id").and_then(local_resource_id_from_json),
                ) {
                    self.generic_resource_id = Some(id.clone());
                    discovered.push((resource_type, id));
                }
            }
            if self.response_id.is_none() {
                if let Some(id) = value.pointer("/response/id").and_then(local_resource_id_from_json)
                {
                    self.response_id = Some(id.clone());
                    discovered.push(("response", id));
                }
            }
            if self.conversation_id.is_none() {
                if let Some(id) = value
                    .pointer("/response/conversation")
                    .and_then(local_resource_id_from_json)
                {
                    self.conversation_id = Some(id.clone());
                    discovered.push(("conversation", id));
                }
            }
            if self.interaction_id.is_none() {
                if let Some(id) = value
                    .get("interaction")
                    .and_then(local_resource_id_from_json)
                    .or_else(|| {
                        value
                            .get("interaction_id")
                            .and_then(local_resource_id_from_json)
                    })
                {
                    self.interaction_id = Some(id.clone());
                    discovered.push(("interaction", id));
                }
            }
        }
        discovered
    }
}

fn trim_ascii_whitespace(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

fn local_resource_owners_from_json_response(
    route: &crate::surface::ApiRoute,
    path: &str,
    channel_id: &str,
    route_model: &str,
    body: &[u8],
) -> Vec<LocalResourceOwner> {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let mut resources = Vec::new();
    if let Some(rule) = local_native_resource_rule_for_collection(route.surface, path) {
        let id = value
            .get("id")
            .or_else(|| value.get("name"))
            .and_then(local_resource_id_from_json)
            .and_then(|id| {
                let id = id.trim_end_matches('/').rsplit('/').next().unwrap_or_default();
                (!id.is_empty()).then(|| id.to_string())
            });
        if let Some(id) = id {
            resources.push((rule.resource_type, id));
        }
    }
    match route.operation {
        crate::surface::ApiOperation::Responses => {
            if let Some(id) = value.get("id").and_then(local_resource_id_from_json) {
                resources.push(("response", id));
            }
            if let Some(id) = value.get("conversation").and_then(local_resource_id_from_json) {
                resources.push(("conversation", id));
            }
        }
        crate::surface::ApiOperation::InteractionsCreate => {
            if let Some(id) = value.get("id").and_then(local_resource_id_from_json) {
                resources.push(("interaction", id));
            }
        }
        crate::surface::ApiOperation::ConversationsCreate => {
            if let Some(id) = value.get("id").and_then(local_resource_id_from_json) {
                resources.push(("conversation", id));
            }
        }
        crate::surface::ApiOperation::FilesCreate => {
            let id = value
                .get("id")
                .and_then(local_resource_id_from_json)
                .or_else(|| {
                    value
                        .pointer("/file/name")
                        .and_then(local_resource_id_from_json)
                        .and_then(|name| {
                            name.strip_prefix("files/")
                                .map(str::to_string)
                                .or(Some(name))
                        })
                });
            if let Some(id) = id {
                resources.push(("file", id));
            }
        }
        crate::surface::ApiOperation::UploadsCreate => {
            if let Some(id) = value.get("id").and_then(local_resource_id_from_json) {
                resources.push(("upload", id));
            }
        }
        crate::surface::ApiOperation::UploadsComplete => {
            if let Some(id) = value.get("id").and_then(local_resource_id_from_json) {
                resources.push(("upload", id));
            }
            if let Some(id) = value.get("file").and_then(local_resource_id_from_json) {
                resources.push(("file", id));
            }
        }
        crate::surface::ApiOperation::VideosCreate
        | crate::surface::ApiOperation::VideosRemix
        | crate::surface::ApiOperation::VideosEdit
        | crate::surface::ApiOperation::VideosExtend => {
            if let Some(id) = value.get("id").and_then(local_resource_id_from_json) {
                resources.push(("video", id));
            }
        }
        crate::surface::ApiOperation::BatchesCreate => {
            if let Some(id) = value.get("id").and_then(local_resource_id_from_json) {
                resources.push(("batch", id));
            }
        }
        crate::surface::ApiOperation::CachedContentsCreate => {
            if let Some(id) = value
                .get("name")
                .and_then(local_resource_id_from_json)
                .and_then(|value| local_gemini_cached_content_id(&value))
            {
                resources.push(("cached_content", id));
            }
        }
        _ => {}
    }
    if route.surface == crate::surface::ApiSurface::Gemini {
        if let Some(name) = value.get("name").and_then(serde_json::Value::as_str) {
            for (prefix, resource_type) in [
                ("operations/", "operation"),
                ("batches/", "batch"),
                ("generatedFiles/", "generated_file"),
                ("fileSearchStores/", "file_search_store"),
                ("tunedModels/", "tuned_model"),
                ("corpora/", "corpus"),
                ("environments/", "environment"),
            ] {
                if let Some(id) = name.trim().strip_prefix(prefix) {
                    if !id.is_empty()
                        && !id.contains('/')
                        && !resources.iter().any(|(existing_type, existing_id)| {
                            *existing_type == resource_type && existing_id == id
                        })
                    {
                        resources.push((resource_type, id.to_string()));
                    }
                }
            }
        }
    }
    if route.surface == crate::surface::ApiSurface::Anthropic
        && value.get("type").and_then(serde_json::Value::as_str) == Some("deployment_run")
    {
        if let Some(id) = value.get("id").and_then(local_resource_id_from_json) {
            if !resources.iter().any(|(resource_type, resource_id)| {
                *resource_type == "deployment_run" && resource_id == &id
            }) {
                resources.push(("deployment_run", id));
            }
        }
    }
    resources
        .into_iter()
        .filter_map(|(resource_type, resource_id)| {
            local_resource_owner_from_identity(
                route.surface,
                resource_type,
                resource_id,
                channel_id,
                route_model,
            )
        })
        .collect()
}

fn local_resource_owner_from_identity(
    surface: crate::surface::ApiSurface,
    resource_type: &str,
    resource_id: String,
    channel_id: &str,
    route_model: &str,
) -> Option<LocalResourceOwner> {
    Some(LocalResourceOwner {
        key: normalize_local_resource_owner_key(surface, resource_type, &resource_id)?,
        channel_id: channel_id.to_string(),
        route_model: route_model.to_string(),
        updated_at_unix: now_unix(),
    })
}

async fn capture_local_resource_owner_response(
    registry: Arc<LocalResourceOwnerRegistry>,
    route: crate::surface::ApiRoute,
    method: &str,
    path: &str,
    channel_id: &str,
    route_model: &str,
    response: warp::reply::Response,
) -> Result<warp::reply::Response> {
    if !response.status().is_success() {
        return Ok(response);
    }
    if method.eq_ignore_ascii_case("POST") {
        if let Some(rule) = local_native_resource_rule_for_collection(route.surface, path) {
            if matches!(
                rule.resource_type,
                "realtime_call" | "realtime_translation_call"
            ) {
                if let Some(resource_id) =
                    local_realtime_call_id_from_location(response.headers())
                {
                    if let Some(owner) = local_resource_owner_from_identity(
                        route.surface,
                        rule.resource_type,
                        resource_id,
                        channel_id,
                        route_model,
                    ) {
                        if let Err(error) = registry.bind_many(vec![owner]).await {
                            log::warn!(
                                "[const-api][resource-owner] realtime call bind failed code=local_resource_owner_realtime_bind_failed error={error}"
                            );
                        }
                    }
                    return Ok(response);
                }
            }
        }
    }
    if method.eq_ignore_ascii_case("DELETE") {
        if let Some(reference) = local_native_resource_reference(route.surface, path) {
            if let Err(error) = registry
                .delete(
                    reference.surface,
                    reference.resource_type,
                    &reference.resource_id,
                )
                .await
            {
                log::warn!(
                    "[const-api][resource-owner] delete failed code=local_resource_owner_delete_failed error={error}"
                );
            }
            return Ok(response);
        }
    }
    if route.operation == crate::surface::ApiOperation::ResponsesDelete {
        if let Some(id) = local_openai_response_id_from_path(path) {
            if let Err(error) = registry.delete(route.surface, "response", &id).await {
                log::warn!(
                    "[const-api][resource-owner] delete failed code=local_resource_owner_delete_failed error={error}"
                );
            }
        }
        return Ok(response);
    }
    if route.operation == crate::surface::ApiOperation::InteractionsDelete {
        if let Some(id) = local_gemini_interaction_id_from_path(path) {
            if let Err(error) = registry.delete(route.surface, "interaction", &id).await {
                log::warn!(
                    "[const-api][resource-owner] delete failed code=local_resource_owner_delete_failed error={error}"
                );
            }
        }
        return Ok(response);
    }
    if route.operation == crate::surface::ApiOperation::ConversationsDelete {
        if let Some(id) = local_openai_conversation_id_from_path(path) {
            if let Err(error) = registry.delete(route.surface, "conversation", &id).await {
                log::warn!(
                    "[const-api][resource-owner] delete failed code=local_resource_owner_delete_failed error={error}"
                );
            }
        }
        return Ok(response);
    }
    if route.operation == crate::surface::ApiOperation::FilesDelete {
        if let Some(id) = local_file_id_from_path(route.surface, path) {
            if let Err(error) = registry.delete(route.surface, "file", &id).await {
                log::warn!(
                    "[const-api][resource-owner] delete failed code=local_resource_owner_delete_failed error={error}"
                );
            }
        }
        return Ok(response);
    }
    if route.operation == crate::surface::ApiOperation::UploadsCancel {
        if let Some(id) = local_openai_upload_id_from_path(path) {
            if let Err(error) = registry.delete(route.surface, "upload", &id).await {
                log::warn!(
                    "[const-api][resource-owner] delete failed code=local_resource_owner_delete_failed error={error}"
                );
            }
        }
        return Ok(response);
    }
    if route.operation == crate::surface::ApiOperation::VideosDelete {
        if let Some(id) = local_openai_video_id_from_path(path) {
            if let Err(error) = registry.delete(route.surface, "video", &id).await {
                log::warn!(
                    "[const-api][resource-owner] delete failed code=local_resource_owner_delete_failed error={error}"
                );
            }
        }
        return Ok(response);
    }
    if route.operation == crate::surface::ApiOperation::BatchesDelete {
        if let Some(id) = local_batch_id_from_path(route.surface, path) {
            if let Err(error) = registry.delete(route.surface, "batch", &id).await {
                log::warn!(
                    "[const-api][resource-owner] delete failed code=local_resource_owner_delete_failed error={error}"
                );
            }
        }
        return Ok(response);
    }
    if route.operation == crate::surface::ApiOperation::CachedContentsDelete {
        if let Some(id) = local_gemini_cached_content_id_from_path(path) {
            if let Err(error) = registry
                .delete(route.surface, "cached_content", &id)
                .await
            {
                log::warn!(
                    "[const-api][resource-owner] delete failed code=local_resource_owner_delete_failed error={error}"
                );
            }
        }
        return Ok(response);
    }
    let generic_resource_create =
        local_native_resource_rule_for_collection(route.surface, path).is_some();
    if !generic_resource_create
        && !matches!(
        route.operation,
        crate::surface::ApiOperation::Responses
            | crate::surface::ApiOperation::InteractionsCreate
            | crate::surface::ApiOperation::ConversationsCreate
            | crate::surface::ApiOperation::FilesCreate
            | crate::surface::ApiOperation::UploadsCreate
            | crate::surface::ApiOperation::UploadsComplete
            | crate::surface::ApiOperation::VideosCreate
            | crate::surface::ApiOperation::VideosRemix
            | crate::surface::ApiOperation::VideosEdit
            | crate::surface::ApiOperation::VideosExtend
            | crate::surface::ApiOperation::BatchesCreate
            | crate::surface::ApiOperation::CachedContentsCreate
    )
    {
        return Ok(response);
    }

    let status = response.status();
    let mut headers = response.headers().clone();
    if response_headers_are_event_stream(&headers) {
        headers.remove(reqwest::header::CONTENT_LENGTH);
        let scanner = Arc::new(Mutex::new(LocalResourceIdentityScanner {
            generic_resource_type: local_native_resource_rule_for_collection(
                route.surface,
                path,
            )
            .map(|rule| rule.resource_type),
            ..LocalResourceIdentityScanner::default()
        }));
        let surface = route.surface;
        let channel_id = channel_id.to_string();
        let route_model = route_model.to_string();
        let stream = response.into_body().into_data_stream().then(move |chunk| {
            let registry = registry.clone();
            let scanner = scanner.clone();
            let channel_id = channel_id.clone();
            let route_model = route_model.clone();
            async move {
                if let Ok(bytes) = &chunk {
                    let identities = scanner.lock().await.write(bytes);
                    let owners = identities
                        .into_iter()
                        .filter_map(|(resource_type, resource_id)| {
                            local_resource_owner_from_identity(
                                surface,
                                resource_type,
                                resource_id,
                                &channel_id,
                                &route_model,
                            )
                        })
                        .collect::<Vec<_>>();
                    if !owners.is_empty() {
                        if let Err(error) = registry.bind_many(owners).await {
                            log::warn!(
                                "[const-api][resource-owner] stream bind failed code=local_resource_owner_stream_bind_failed error={error}"
                            );
                        }
                    }
                }
                chunk
            }
        });
        return response_from_stream(status, &headers, stream);
    }

    let bytes = response.into_body().collect().await?.to_bytes();
    let owners =
        local_resource_owners_from_json_response(&route, path, channel_id, route_model, &bytes);
    if !owners.is_empty() {
        if let Err(error) = registry.bind_many(owners).await {
            // The upstream operation already succeeded. Keep its exact response and
            // retain the process-local owner; report persistence failure without
            // encouraging a caller to duplicate a resource-creating request.
            log::warn!(
                "[const-api][resource-owner] buffered bind failed code=local_resource_owner_bind_failed error={error}"
            );
        }
    }
    response_from_body(status, &headers, bytes)
}

#[cfg(test)]
mod local_resource_owner_tests {
    use super::*;

    #[test]
    fn realtime_call_owner_is_read_from_provider_location_header() {
        let mut headers = warp::http::HeaderMap::new();
        headers.insert(
            reqwest::header::LOCATION,
            "/v1/realtime/calls/calls/rtc_owner_1?trace=1"
                .parse()
                .expect("Location"),
        );
        assert_eq!(
            local_realtime_call_id_from_location(&headers).as_deref(),
            Some("rtc_owner_1")
        );
        headers.insert(
            reqwest::header::LOCATION,
            "/v1/live/rtc_live_owner".parse().expect("Live Location"),
        );
        assert_eq!(
            local_realtime_call_id_from_location(&headers).as_deref(),
            Some("rtc_live_owner")
        );
        let route = crate::surface::resolve_api_route("GET", "/v1/live/rtc_live_owner", true)
            .expect("Live sideband route");
        let references = local_resource_references(&route, "/v1/live/rtc_live_owner", b"");
        assert_eq!(references.len(), 1);
        assert_eq!(references[0].resource_type, "realtime_call");
        assert_eq!(references[0].resource_id, "rtc_live_owner");
        assert!(references[0].required);
    }

    #[tokio::test]
    async fn local_resource_owner_round_trips_through_sqlite_without_credentials() {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("owners.sqlite3");
        let registry = LocalResourceOwnerRegistry::open(path.clone());
        registry
            .bind_many(vec![LocalResourceOwner {
                key: LocalResourceOwnerKey {
                    surface: crate::surface::ApiSurface::OpenAi,
                    resource_type: "response".to_string(),
                    resource_id: "resp_persisted".to_string(),
                },
                channel_id: "channel-a".to_string(),
                route_model: "gpt-test".to_string(),
                updated_at_unix: now_unix(),
            }])
            .await
            .expect("bind owner");
        drop(registry);

        let restored = LocalResourceOwnerRegistry::open(path);
        let owner = restored
            .get(
                crate::surface::ApiSurface::OpenAi,
                "response",
                "resp_persisted",
            )
            .expect("restored owner");
        assert_eq!(owner.channel_id, "channel-a");
        assert_eq!(owner.route_model, "gpt-test");
    }

    #[test]
    fn local_resource_identity_scanner_handles_split_response_and_interaction_events() {
        let mut scanner = LocalResourceIdentityScanner::default();
        assert!(scanner
            .write(b"event: response.created\ndata: {\"response\":{\"id\":\"resp_")
            .is_empty());
        let discovered =
            scanner.write(b"1\",\"conversation\":{\"id\":\"conv_1\"}}}\n\nevent: interaction.created\n");
        assert_eq!(
            discovered,
            vec![
                ("response", "resp_1".to_string()),
                ("conversation", "conv_1".to_string())
            ]
        );
        let discovered =
            scanner.write(b"data: {\"interaction\":{\"id\":\"int_1\"}}\n\n");
        assert_eq!(discovered, vec![("interaction", "int_1".to_string())]);

        let mut stored_chat = LocalResourceIdentityScanner {
            generic_resource_type: Some("chat_completion"),
            ..LocalResourceIdentityScanner::default()
        };
        let discovered = stored_chat.write(
            b"data: {\"id\":\"chatcmpl_owned\",\"object\":\"chat.completion.chunk\"}\n\n",
        );
        assert_eq!(
            discovered,
            vec![("chat_completion", "chatcmpl_owned".to_string())]
        );
    }

    #[test]
    fn local_resource_references_distinguish_lifecycle_and_continuation() {
        let lifecycle =
            crate::surface::resolve_api_route("GET", "/v1/responses/resp_1", false).unwrap();
        let references =
            local_resource_references(&lifecycle, "/v1/responses/resp_1", b"");
        assert_eq!(references.len(), 1);
        assert!(references[0].required);

        let create = crate::surface::resolve_api_route("POST", "/v1/responses", false).unwrap();
        let references = local_resource_references(
            &create,
            "/v1/responses",
            br#"{"previous_response_id":"resp_1","conversation":{"id":"conv_1"}}"#,
        );
        assert_eq!(references.len(), 2);
        assert!(references.iter().all(|reference| !reference.required));
    }

    #[test]
    fn file_references_are_discovered_across_standard_generation_protocols() {
        let anthropic = crate::surface::resolve_api_route(
            "POST",
            "/anthropic/v1/messages",
            false,
        )
        .unwrap();
        let anthropic_refs = local_resource_references(
            &anthropic,
            "/anthropic/v1/messages",
            br#"{"messages":[{"role":"user","content":[{"type":"document","source":{"type":"file","file_id":"file_claude"}}]}]}"#,
        );
        assert_eq!(anthropic_refs.len(), 1);
        assert_eq!(anthropic_refs[0].resource_id, "file_claude");

        let gemini = crate::surface::resolve_api_route(
            "POST",
            "/gemini/v1beta/models/gemini-test:generateContent",
            false,
        )
        .unwrap();
        let gemini_refs = local_resource_references(
            &gemini,
            "/gemini/v1beta/models/gemini-test:generateContent",
            br#"{"contents":[{"parts":[{"fileData":{"fileUri":"https://generativelanguage.googleapis.com/v1beta/files/gemini_owned?alt=media"}},{"file_data":{"file_uri":"https://generativelanguage.googleapis.com/v1beta/files/gemini_owned"}}]}]}"#,
        );
        assert_eq!(gemini_refs.len(), 1);
        assert_eq!(gemini_refs[0].resource_id, "gemini_owned");

        let openai_batch =
            crate::surface::resolve_api_route("POST", "/v1/batches", false).unwrap();
        let batch_refs = local_resource_references(
            &openai_batch,
            "/v1/batches",
            br#"{"input_file_id":"file_batch","endpoint":"/v1/responses"}"#,
        );
        assert_eq!(batch_refs.len(), 1);
        assert_eq!(batch_refs[0].resource_id, "file_batch");
    }

    #[test]
    fn opaque_official_resources_share_one_declarative_hard_owner_mechanism() {
        let vector_create =
            crate::surface::resolve_api_route("POST", "/v1/vector_stores", false).unwrap();
        let owners = local_resource_owners_from_json_response(
            &vector_create,
            "/v1/vector_stores",
            "openai-channel",
            "",
            br#"{"id":"vs_owned","object":"vector_store"}"#,
        );
        assert_eq!(owners.len(), 1);
        assert_eq!(owners[0].key.resource_type, "vector_store");
        assert_eq!(owners[0].key.resource_id, "vs_owned");

        let vector_files = crate::surface::resolve_api_route(
            "GET",
            "/v1/vector_stores/vs_owned/files",
            false,
        )
        .unwrap();
        let references = local_resource_references(
            &vector_files,
            "/v1/vector_stores/vs_owned/files",
            b"",
        );
        assert_eq!(references.len(), 1);
        assert_eq!(references[0].resource_type, "vector_store");
        assert_eq!(references[0].resource_id, "vs_owned");
        assert!(references[0].required);

        let gemini_create = crate::surface::resolve_api_route(
            "POST",
            "/gemini/v1beta/fileSearchStores",
            false,
        )
        .unwrap();
        let owners = local_resource_owners_from_json_response(
            &gemini_create,
            "/gemini/v1beta/fileSearchStores",
            "gemini-channel",
            "",
            br#"{"name":"fileSearchStores/store_owned"}"#,
        );
        assert_eq!(owners.len(), 1);
        assert_eq!(owners[0].key.resource_type, "file_search_store");
        assert_eq!(owners[0].key.resource_id, "store_owned");

        let voice_consent = crate::surface::resolve_api_route(
            "POST",
            "/v1/audio/voice_consents",
            false,
        )
        .unwrap();
        let owners = local_resource_owners_from_json_response(
            &voice_consent,
            "/v1/audio/voice_consents",
            "openai-channel",
            "",
            br#"{"id":"consent_owned"}"#,
        );
        assert_eq!(owners[0].key.resource_type, "voice_consent");
        assert_eq!(owners[0].key.resource_id, "consent_owned");

        let memory_store = crate::surface::resolve_api_route(
            "POST",
            "/anthropic/v1/memory_stores",
            false,
        )
        .unwrap();
        let owners = local_resource_owners_from_json_response(
            &memory_store,
            "/anthropic/v1/memory_stores",
            "anthropic-channel",
            "",
            br#"{"id":"memstore_owned"}"#,
        );
        assert_eq!(owners[0].key.resource_type, "memory_store");
        let nested = crate::surface::resolve_api_route(
            "GET",
            "/anthropic/v1/memory_stores/memstore_owned/memories",
            false,
        )
        .unwrap();
        let references = local_resource_references(
            &nested,
            "/anthropic/v1/memory_stores/memstore_owned/memories",
            b"",
        );
        assert_eq!(references[0].resource_type, "memory_store");
        assert_eq!(references[0].resource_id, "memstore_owned");

        let upload = crate::surface::resolve_api_route(
            "POST",
            "/gemini/upload/v1beta/fileSearchStores/store_owned:uploadToFileSearchStore",
            false,
        )
        .unwrap();
        let references = local_resource_references(
            &upload,
            "/gemini/upload/v1beta/fileSearchStores/store_owned:uploadToFileSearchStore",
            b"",
        );
        assert_eq!(references[0].resource_type, "file_search_store");
        assert_eq!(references[0].resource_id, "store_owned");

        let predict = crate::surface::resolve_api_route(
            "POST",
            "/gemini/v1beta/models/veo:predictLongRunning",
            false,
        )
        .unwrap();
        let owners = local_resource_owners_from_json_response(
            &predict,
            "/gemini/v1beta/models/veo:predictLongRunning",
            "gemini-channel",
            "veo",
            br#"{"name":"operations/op_owned"}"#,
        );
        assert!(owners.iter().any(|owner| {
            owner.key.resource_type == "operation" && owner.key.resource_id == "op_owned"
        }));

        let run = crate::surface::resolve_api_route(
            "POST",
            "/anthropic/v1/deployments/deploy_1/run",
            false,
        )
        .unwrap();
        let owners = local_resource_owners_from_json_response(
            &run,
            "/anthropic/v1/deployments/deploy_1/run",
            "anthropic-channel",
            "",
            br#"{"id":"deprun_owned","type":"deployment_run"}"#,
        );
        assert!(owners.iter().any(|owner| {
            owner.key.resource_type == "deployment_run"
                && owner.key.resource_id == "deprun_owned"
        }));

        let thread_and_run =
            crate::surface::resolve_api_route("POST", "/v1/threads/runs", false).unwrap();
        assert!(local_resource_references(&thread_and_run, "/v1/threads/runs", b"").is_empty());
    }

    #[tokio::test]
    async fn opaque_resource_delete_releases_the_hard_owner() {
        let registry = LocalResourceOwnerRegistry::in_memory();
        registry
            .bind_many(vec![LocalResourceOwner {
                key: LocalResourceOwnerKey {
                    surface: crate::surface::ApiSurface::OpenAi,
                    resource_type: "vector_store".to_string(),
                    resource_id: "vs_deleted".to_string(),
                },
                channel_id: "openai-channel".to_string(),
                route_model: String::new(),
                updated_at_unix: now_unix(),
            }])
            .await
            .expect("bind owner");
        let route =
            crate::surface::resolve_api_route("DELETE", "/v1/vector_stores/vs_deleted", false)
                .expect("opaque vector-store route");
        let response = response_from_body(
            StatusCode::OK,
            &reqwest::header::HeaderMap::new(),
            bytes::Bytes::from_static(br#"{"id":"vs_deleted","deleted":true}"#),
        )
        .expect("response");

        let response = capture_local_resource_owner_response(
            registry.clone(),
            route,
            "DELETE",
            "/v1/vector_stores/vs_deleted",
            "openai-channel",
            "",
            response,
        )
        .await
        .expect("capture delete");

        assert_eq!(response.status(), StatusCode::OK);
        assert!(registry
            .get(
                crate::surface::ApiSurface::OpenAi,
                "vector_store",
                "vs_deleted",
            )
            .is_none());
    }

    #[test]
    fn gemini_nested_upload_response_binds_the_canonical_file_id() {
        let route = crate::surface::resolve_api_route(
            "POST",
            "/gemini/upload/v1beta/files",
            false,
        )
        .unwrap();
        let owners = local_resource_owners_from_json_response(
            &route,
            "/gemini/upload/v1beta/files",
            "gemini-channel",
            "",
            br#"{"file":{"name":"files/gemini_owned","state":"ACTIVE"}}"#,
        );
        assert_eq!(owners.len(), 1);
        assert_eq!(owners[0].key.resource_type, "file");
        assert_eq!(owners[0].key.resource_id, "gemini_owned");
    }

    #[test]
    fn gemini_cached_content_lifecycle_and_generation_share_one_hard_owner() {
        let create = crate::surface::resolve_api_route(
            "POST",
            "/gemini/v1beta/cachedContents",
            false,
        )
        .unwrap();
        let owners = local_resource_owners_from_json_response(
            &create,
            "/gemini/v1beta/cachedContents",
            "gemini-channel",
            "gemini-test",
            br#"{"name":"cachedContents/cache_owned","model":"models/gemini-test"}"#,
        );
        assert_eq!(owners.len(), 1);
        assert_eq!(owners[0].key.resource_type, "cached_content");
        assert_eq!(owners[0].key.resource_id, "cache_owned");

        let lifecycle = crate::surface::resolve_api_route(
            "PATCH",
            "/gemini/v1beta/cachedContents/cache_owned",
            false,
        )
        .unwrap();
        let references = local_resource_references(
            &lifecycle,
            "/gemini/v1beta/cachedContents/cache_owned",
            br#"{"ttl":"600s"}"#,
        );
        assert_eq!(references.len(), 1);
        assert!(references[0].required);
        assert_eq!(references[0].resource_id, "cache_owned");

        let generate = crate::surface::resolve_api_route(
            "POST",
            "/gemini/v1beta/models/gemini-test:generateContent",
            false,
        )
        .unwrap();
        let references = local_resource_references(
            &generate,
            "/gemini/v1beta/models/gemini-test:generateContent",
            br#"{"cachedContent":"cachedContents/cache_owned","contents":[{"role":"user","parts":[{"text":"continue"}]}]}"#,
        );
        assert_eq!(references.len(), 1);
        assert!(references[0].required);
        assert_eq!(references[0].resource_id, "cache_owned");
    }
}
