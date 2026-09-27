const CODEX_HTTP_CONTINUATION_CACHE_MAX_ENTRIES: usize = 128;
const CODEX_HTTP_CONTINUATION_CACHE_MAX_BYTES: usize = 64 * 1024 * 1024;
const CODEX_HTTP_CONTINUATION_CACHE_MAX_ENTRY_BYTES: usize = 16 * 1024 * 1024;
const CODEX_HTTP_CONTINUATION_CACHE_TTL: Duration = Duration::from_secs(2 * 60 * 60);
#[cfg(not(test))]
const CODEX_HTTP_CONTINUATION_DB_FILE: &str = "codex-http-continuations.sqlite3";

#[derive(Debug, Clone)]
struct CodexHttpContinuationEntry {
    history: Arc<Vec<serde_json::Value>>,
    cache_identity: Option<String>,
    bytes: usize,
    touched_at: Instant,
}

#[derive(Debug, Clone, PartialEq)]
struct CodexHttpContinuationState {
    history: Arc<Vec<serde_json::Value>>,
    cache_identity: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedCodexHttpContinuation {
    history: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    cache_identity: String,
}

impl CodexHttpContinuationState {
    fn serialized(&self) -> Result<Vec<u8>> {
        #[derive(serde::Serialize)]
        struct Borrowed<'a> {
            history: &'a [serde_json::Value],
            #[serde(skip_serializing_if = "str::is_empty")]
            cache_identity: &'a str,
        }
        Ok(serde_json::to_vec(&Borrowed {
            history: &self.history,
            cache_identity: self.cache_identity.as_deref().unwrap_or_default(),
        })?)
    }
}

#[derive(Debug, Default)]
struct CodexHttpContinuationCache {
    entries: HashMap<String, CodexHttpContinuationEntry>,
    bytes: usize,
    websocket_pins: HashMap<u128, String>,
}

/// Protect only the latest completed response of each live logical WS, not
/// every historical turn. The same global byte/entry limits still apply.
pub(crate) struct CodexWebsocketContinuationPin(u128);

impl CodexWebsocketContinuationPin {
    pub(crate) fn new() -> Self {
        Self(rand::random())
    }
}

impl Drop for CodexWebsocketContinuationPin {
    fn drop(&mut self) {
        codex_http_continuation_cache()
            .lock().unwrap_or_else(|poisoned| poisoned.into_inner())
            .websocket_pins.remove(&self.0);
    }
}

fn codex_websocket_pinned_keys() -> HashSet<String> {
    codex_http_continuation_cache()
        .lock().unwrap_or_else(|poisoned| poisoned.into_inner())
        .websocket_pins.values().cloned().collect()
}

static CODEX_HTTP_CONTINUATION_CACHE: OnceLock<StdMutex<CodexHttpContinuationCache>> =
    OnceLock::new();

#[cfg(all(debug_assertions, feature = "memory-diagnostics"))]
pub(crate) fn codex_continuation_memory_diagnostics() -> serde_json::Value {
    let Some(cache) = CODEX_HTTP_CONTINUATION_CACHE.get() else {
        return serde_json::json!({"initialized": false});
    };
    let Ok(cache) = cache.try_lock() else {
        return serde_json::json!({"initialized": true, "busy": true});
    };
    serde_json::json!({
        "initialized": true,
        "entries": cache.entries.len(),
        "serialized_bytes_not_heap_bytes": cache.bytes,
        "pinned_websocket_sessions": cache.websocket_pins.len(),
    })
}

fn codex_http_continuation_cache() -> &'static StdMutex<CodexHttpContinuationCache> {
    CODEX_HTTP_CONTINUATION_CACHE
        .get_or_init(|| StdMutex::new(CodexHttpContinuationCache::default()))
}

fn codex_http_continuation_key(config: &SupplierConfig, response_id: &str) -> String {
    let credential_scope = if config.credential_ref.trim().is_empty() {
        config.subscription.credential_ref.trim()
    } else {
        config.credential_ref.trim()
    };
    // Persist only a one-way scope key. Credential references can contain local account names or
    // paths and do not need to be recoverable from the continuation database.
    let mut digest = Sha256::new();
    digest.update(config.channel_id.trim().as_bytes());
    digest.update([0x1f]);
    digest.update(credential_scope.as_bytes());
    digest.update([0x1f]);
    digest.update(response_id.trim().as_bytes());
    hex::encode(digest.finalize())
}

#[cfg(not(test))]
fn codex_http_continuation_db_path() -> PathBuf {
    crate::client_data_root()
        .join("state")
        .join(CODEX_HTTP_CONTINUATION_DB_FILE)
}

fn open_codex_http_continuation_db(path: &std::path::Path) -> Result<rusqlite::Connection> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let connection = rusqlite::Connection::open(path)?;
    connection.busy_timeout(Duration::from_secs(2))?;
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         CREATE TABLE IF NOT EXISTS codex_http_continuations (
             cache_key TEXT PRIMARY KEY NOT NULL,
             touched_at_unix INTEGER NOT NULL,
             bytes INTEGER NOT NULL,
             history_json BLOB NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_codex_http_continuations_touched
             ON codex_http_continuations(touched_at_unix DESC);",
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

fn prune_codex_http_continuation_db(
    connection: &rusqlite::Connection,
    now: i64,
) -> Result<()> {
    let ttl_seconds = i64::try_from(CODEX_HTTP_CONTINUATION_CACHE_TTL.as_secs())
        .unwrap_or(i64::MAX);
    connection.execute(
        "DELETE FROM codex_http_continuations WHERE touched_at_unix < ?1",
        rusqlite::params![now.saturating_sub(ttl_seconds)],
    )?;

    let mut statement = connection.prepare(
        "SELECT cache_key, bytes FROM codex_http_continuations
         ORDER BY touched_at_unix DESC, cache_key ASC",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut rows = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    let pinned = codex_websocket_pinned_keys();
    // Stable sorting retains recency within each priority class. An old turn
    // of a busy session cannot evict the latest turn of another live session.
    rows.sort_by_key(|(key, _)| !pinned.contains(key));
    let mut kept_entries = 0usize;
    let mut kept_bytes = 0usize;
    let mut remove = Vec::new();
    for (key, bytes) in rows {
        let bytes = usize::try_from(bytes).unwrap_or(usize::MAX);
        let fits = kept_entries < CODEX_HTTP_CONTINUATION_CACHE_MAX_ENTRIES
            && kept_bytes.saturating_add(bytes) <= CODEX_HTTP_CONTINUATION_CACHE_MAX_BYTES;
        if fits {
            kept_entries += 1;
            kept_bytes = kept_bytes.saturating_add(bytes);
        } else {
            remove.push(key);
        }
    }
    drop(statement);
    for key in remove {
        log::debug!("[const-api][protocol] continuation_evicted store=disk reason=capacity cache_key={} pinned={}", key, pinned.contains(&key));
        connection.execute(
            "DELETE FROM codex_http_continuations WHERE cache_key = ?1",
            rusqlite::params![key],
        )?;
    }
    Ok(())
}

fn load_codex_http_continuation_from_path(
    path: &std::path::Path,
    key: &str,
    now: i64,
) -> Result<Option<CodexHttpContinuationState>> {
    let connection = open_codex_http_continuation_db(path)?;
    let mut statement = connection.prepare(
        "SELECT touched_at_unix, bytes, history_json
         FROM codex_http_continuations WHERE cache_key = ?1",
    )?;
    let mut rows = statement.query(rusqlite::params![key])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let touched_at_unix = row.get::<_, i64>(0)?;
    let bytes = usize::try_from(row.get::<_, i64>(1)?).unwrap_or(usize::MAX);
    let history_json = row.get::<_, Vec<u8>>(2)?;
    drop(rows);
    drop(statement);
    let ttl_seconds = i64::try_from(CODEX_HTTP_CONTINUATION_CACHE_TTL.as_secs())
        .unwrap_or(i64::MAX);
    if now.saturating_sub(touched_at_unix) > ttl_seconds
        || bytes > CODEX_HTTP_CONTINUATION_CACHE_MAX_ENTRY_BYTES
        || history_json.len() > CODEX_HTTP_CONTINUATION_CACHE_MAX_ENTRY_BYTES
    {
        connection.execute(
            "DELETE FROM codex_http_continuations WHERE cache_key = ?1",
            rusqlite::params![key],
        )?;
        return Ok(None);
    }
    let state = if history_json.iter().find(|byte| !byte.is_ascii_whitespace()) == Some(&b'[') {
        // Older clients persisted the history as a bare array. Keep those entries readable;
        // they simply cannot restore a cache identity until the next successful response.
        CodexHttpContinuationState {
            history: Arc::new(serde_json::from_slice(&history_json)?),
            cache_identity: None,
        }
    } else {
        let stored: PersistedCodexHttpContinuation = serde_json::from_slice(&history_json)?;
        CodexHttpContinuationState {
            history: Arc::new(stored.history),
            cache_identity: normalized_subscription_cache_identity(Some(&stored.cache_identity))
                .map(str::to_string),
        }
    };
    if state.history.is_empty() {
        connection.execute(
            "DELETE FROM codex_http_continuations WHERE cache_key = ?1",
            rusqlite::params![key],
        )?;
        return Ok(None);
    }
    connection.execute(
        "UPDATE codex_http_continuations SET touched_at_unix = ?2 WHERE cache_key = ?1",
        rusqlite::params![key, now],
    )?;
    Ok(Some(state))
}

#[cfg(test)]
fn persist_codex_http_continuation_to_path(
    path: &std::path::Path,
    key: &str,
    state: &CodexHttpContinuationState,
    now: i64,
) -> Result<()> {
    persist_codex_http_continuation_bytes_to_path(path, key, &state.serialized()?, now)
}

fn persist_codex_http_continuation_bytes_to_path(
    path: &std::path::Path,
    key: &str,
    history_json: &[u8],
    now: i64,
) -> Result<()> {
    if history_json.len() > CODEX_HTTP_CONTINUATION_CACHE_MAX_ENTRY_BYTES {
        return Err(anyhow!("codex continuation exceeds per-entry limit"));
    }
    let connection = open_codex_http_continuation_db(path)?;
    connection.execute(
        "INSERT INTO codex_http_continuations(cache_key, touched_at_unix, bytes, history_json)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(cache_key) DO UPDATE SET
             touched_at_unix = excluded.touched_at_unix,
             bytes = excluded.bytes,
             history_json = excluded.history_json",
        rusqlite::params![
            key,
            now,
            i64::try_from(history_json.len()).unwrap_or(i64::MAX),
            history_json
        ],
    )?;
    prune_codex_http_continuation_db(&connection, now)
}

#[cfg(not(test))]
fn load_persisted_codex_http_continuation(
    key: &str,
) -> Result<Option<CodexHttpContinuationState>> {
    load_codex_http_continuation_from_path(&codex_http_continuation_db_path(), key, now_unix())
}

#[cfg(test)]
fn load_persisted_codex_http_continuation(
    _key: &str,
) -> Result<Option<CodexHttpContinuationState>> {
    // Unit tests keep their existing process-local isolation. Persistence is exercised against an
    // explicit temporary database path below, never the developer's real state directory.
    Ok(None)
}

#[cfg(not(test))]
fn persist_codex_http_continuation(
    key: &str,
    history_json: &[u8],
) -> Result<()> {
    persist_codex_http_continuation_bytes_to_path(
        &codex_http_continuation_db_path(),
        key,
        history_json,
        now_unix(),
    )
}

#[cfg(test)]
fn persist_codex_http_continuation(
    _key: &str,
    _history_json: &[u8],
) -> Result<()> {
    Ok(())
}

fn persist_codex_http_continuation_owned(
    key: String,
    history_json: Vec<u8>,
) {
    if let Err(error) = persist_codex_http_continuation(&key, &history_json) {
        // Persistence improves restart continuity but must never turn a successful upstream
        // response into a client failure. The process-local cache remains usable.
        log::warn!(
            "[const-api][protocol] subscription continuation store write failed code=codex_continuation_store_write_failed error={error}"
        );
    }
}

#[cfg(not(test))]
fn schedule_codex_http_continuation_persist(
    key: String,
    history_json: Vec<u8>,
) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            // SQLite setup and pruning are blocking work. Keep them off the
            // response-stream polling task so a completed response can reach the client at once.
            drop(handle.spawn_blocking(move || {
                persist_codex_http_continuation_owned(key, history_json);
            }));
        }
        Err(_) => persist_codex_http_continuation_owned(key, history_json),
    }
}

#[cfg(test)]
fn schedule_codex_http_continuation_persist(
    key: String,
    history_json: Vec<u8>,
) {
    persist_codex_http_continuation_owned(key, history_json);
}

fn prune_codex_http_continuation_cache(
    cache: &mut CodexHttpContinuationCache,
    now: Instant,
) {
    let expired = cache
        .entries
        .iter()
        .filter(|(_, entry)| now.duration_since(entry.touched_at) > CODEX_HTTP_CONTINUATION_CACHE_TTL)
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    for key in expired {
        if let Some(entry) = cache.entries.remove(&key) {
            cache.bytes = cache.bytes.saturating_sub(entry.bytes);
            log::debug!("[const-api][protocol] continuation_evicted store=memory reason=ttl cache_key={key}");
        }
    }
    let pinned = cache.websocket_pins.values().cloned().collect::<HashSet<_>>();
    while cache.entries.len() > CODEX_HTTP_CONTINUATION_CACHE_MAX_ENTRIES
        || cache.bytes > CODEX_HTTP_CONTINUATION_CACHE_MAX_BYTES
    {
        let Some(oldest) = cache
            .entries
            .iter()
            .min_by_key(|(key, entry)| (pinned.contains(*key), entry.touched_at))
            .map(|(key, _)| key.clone())
        else {
            break;
        };
        if let Some(entry) = cache.entries.remove(&oldest) {
            cache.bytes = cache.bytes.saturating_sub(entry.bytes);
            if pinned.contains(&oldest) {
                log::warn!("[const-api][protocol] continuation_evicted store=memory reason=active_capacity cache_key={} entry_bytes={} cache_bytes={} entries={}", oldest, entry.bytes, cache.bytes, cache.entries.len());
            } else {
                log::debug!("[const-api][protocol] continuation_evicted store=memory reason=capacity cache_key={oldest}");
            }
        }
    }
}

fn codex_http_continuation_state(
    config: &SupplierConfig,
    response_id: &str,
) -> Option<CodexHttpContinuationState> {
    let key = codex_http_continuation_key(config, response_id);
    let now = Instant::now();
    {
        let mut cache = codex_http_continuation_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        prune_codex_http_continuation_cache(&mut cache, now);
        if let Some(entry) = cache.entries.get_mut(&key) {
            entry.touched_at = now;
            return Some(CodexHttpContinuationState {
                history: entry.history.clone(),
                cache_identity: entry.cache_identity.clone(),
            });
        }
    }

    let state = match load_persisted_codex_http_continuation(&key) {
        Ok(state) => state?,
        Err(error) => {
            log::warn!(
                "[const-api][protocol] subscription continuation store read failed code=codex_continuation_store_read_failed error={error}"
            );
            return None;
        }
    };
    insert_codex_http_continuation_memory(key, state.clone());
    log::info!(
        "[const-api][protocol] subscription continuation restored code=codex_response_context_restored"
    );
    Some(state)
}

fn insert_codex_http_continuation_memory(key: String, state: CodexHttpContinuationState) {
    let Ok(serialized) = state.serialized() else {
        return;
    };
    insert_codex_http_continuation_memory_with_pin(key, state, serialized.len(), None);
}

fn insert_codex_http_continuation_memory_with_pin(
    key: String,
    state: CodexHttpContinuationState,
    bytes: usize,
    websocket_pin: Option<u128>,
) {
    let now = Instant::now();
    let mut cache = codex_http_continuation_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(previous) = cache.entries.remove(&key) {
        cache.bytes = cache.bytes.saturating_sub(previous.bytes);
    }
    if let Some(owner) = websocket_pin {
        cache.websocket_pins.insert(owner, key.clone());
    }
    cache.bytes = cache.bytes.saturating_add(bytes);
    cache.entries.insert(
        key,
        CodexHttpContinuationEntry {
            history: state.history,
            cache_identity: state.cache_identity,
            bytes,
            touched_at: now,
        },
    );
    prune_codex_http_continuation_cache(&mut cache, now);
}

fn insert_codex_http_continuation_history(
    config: &SupplierConfig,
    response_id: &str,
    history: Vec<serde_json::Value>,
    cache_identity: Option<&str>,
) -> bool {
    insert_codex_http_continuation_history_with_pin(config, response_id, history, cache_identity, None)
}

fn insert_codex_http_continuation_history_with_pin(
    config: &SupplierConfig,
    response_id: &str,
    history: Vec<serde_json::Value>,
    cache_identity: Option<&str>,
    websocket_pin: Option<u128>,
) -> bool {
    let response_id = response_id.trim();
    if response_id.is_empty() || response_id.len() > 512 || history.is_empty() {
        return false;
    }
    let state = CodexHttpContinuationState {
        history: Arc::new(history),
        cache_identity: normalized_subscription_cache_identity(cache_identity).map(str::to_string),
    };
    // The same serialization supplies admission size, cache accounting and
    // SQLite persistence. The parsed tree is moved into the cache, not copied.
    let Ok(serialized) = state.serialized() else {
        return false;
    };
    let bytes = serialized.len();
    if bytes > CODEX_HTTP_CONTINUATION_CACHE_MAX_ENTRY_BYTES {
        log::warn!(
            "[const-api][protocol] subscription continuation cache skipped code=codex_continuation_too_large bytes={bytes}"
        );
        return false;
    }
    let key = codex_http_continuation_key(config, response_id);
    insert_codex_http_continuation_memory_with_pin(key.clone(), state, bytes, websocket_pin);
    schedule_codex_http_continuation_persist(key, serialized);
    true
}

fn apply_codex_continuation_state(
    object: &mut serde_json::Map<String, serde_json::Value>,
    state: CodexHttpContinuationState,
) -> (usize, usize) {
    if object
        .get("prompt_cache_key")
        .and_then(serde_json::Value::as_str)
        .is_none_or(|value| value.trim().is_empty())
    {
        if let Some(cache_identity) = state.cache_identity.as_deref() {
            object.insert(
                "prompt_cache_key".to_string(),
                serde_json::Value::String(cache_identity.to_string()),
            );
            log::debug!(
                "[const-api][protocol] subscription continuation cache identity replayed code=codex_cache_identity_replayed"
            );
        }
    }
    let history = state.history;
    let current = match object.remove("input") {
        Some(serde_json::Value::Array(input)) => input,
        _ => Vec::new(),
    };
    let history_len = history.len();
    let current_len = current.len();
    let expanded = if current.len() >= history.len() && current[..history.len()] == history[..] {
        current
    } else {
        // Cache lookups share immutable history. Clone only when replay really
        // needs a mutable transcript, and outside the global cache lock.
        let mut expanded = Arc::unwrap_or_clone(history);
        expanded.extend(current);
        expanded
    };
    object.remove("previous_response_id");
    object.insert("input".to_string(), serde_json::Value::Array(expanded));
    (history_len, current_len)
}

fn codex_websocket_replay_input_is_self_contained(
    input: &[serde_json::Value],
) -> std::result::Result<(), &'static str> {
    for item in input {
        let item_type = item
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if item_type == "reasoning"
            && !item
                .get("encrypted_content")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|value| !value.is_empty())
        {
            return Err("reasoning_encrypted_content_missing");
        }
    }
    if codex_orphaned_tool_output_count(input) > 0 {
        return Err("tool_call_context_missing");
    }
    Ok(())
}

/// Builds the one safe reconnect request for a store-disabled subscription
/// continuation. The caller may use it only after pre-send affinity loss or an
/// explicit upstream previous_response_not_found rejection before any output.
///
/// The legacy cache/collector names mention HTTP because that path introduced
/// the bounded store. WebSocket recovery intentionally shares that same store
/// instead of creating a second history database.
pub(crate) fn prepare_codex_websocket_subscription_replay(
    config: &SupplierConfig,
    body: &str,
) -> Result<Option<String>> {
    let mut value: serde_json::Value = serde_json::from_str(body)?;
    let Some(object) = value.as_object_mut() else {
        return Ok(None);
    };
    if object.get("store").and_then(serde_json::Value::as_bool) != Some(false) {
        log::warn!(
            "[const-api][upstream-ws] continuation_replay_skipped code=codex_ws_replay_requires_store_false action=fail_closed"
        );
        return Ok(None);
    }
    let Some(previous_response_id) = object
        .get("previous_response_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
    else {
        return Ok(None);
    };
    let Some(state) = codex_http_continuation_state(config, &previous_response_id) else {
        let cache = codex_http_continuation_cache().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        log::warn!(
            "[const-api][upstream-ws] continuation_replay_skipped code=codex_ws_replay_context_missing action=fail_closed channel_id={} cache_key={} cache_entries={} cache_bytes={} active_pins={}",
            config.channel_id, codex_http_continuation_key(config, &previous_response_id), cache.entries.len(), cache.bytes, cache.websocket_pins.len()
        );
        return Ok(None);
    };
    let (history_len, current_len) = apply_codex_continuation_state(object, state);
    let Some(input) = object
        .get("input")
        .and_then(serde_json::Value::as_array)
    else {
        return Ok(None);
    };
    if let Err(reason) = codex_websocket_replay_input_is_self_contained(input) {
        log::warn!(
            "[const-api][upstream-ws] continuation_replay_skipped code=codex_ws_replay_context_incomplete reason={} action=fail_closed",
            reason,
        );
        return Ok(None);
    }

    let mut actions = Vec::new();
    strip_output_only_fields_from_responses_input(&mut value, &mut actions);
    let Some(object) = value.as_object_mut() else {
        return Ok(None);
    };
    const ENCRYPTED_REASONING: &str = "reasoning.encrypted_content";
    match object.get_mut("include") {
        None => {
            object.insert(
                "include".to_string(),
                serde_json::json!([ENCRYPTED_REASONING]),
            );
        }
        Some(serde_json::Value::Array(include)) => {
            if !include
                .iter()
                .any(|item| item.as_str() == Some(ENCRYPTED_REASONING))
            {
                include.push(serde_json::json!(ENCRYPTED_REASONING));
            }
        }
        Some(_) => {
            log::warn!(
                "[const-api][upstream-ws] continuation_replay_skipped code=codex_ws_replay_include_invalid action=fail_closed"
            );
            return Ok(None);
        }
    }
    for action in actions {
        log::debug!(
            "[const-api][upstream-ws] continuation_replay_adjusted code={} path={}",
            action.code,
            action.path,
        );
    }
    log::info!(
        "[const-api][upstream-ws] continuation_replay_prepared code=codex_ws_previous_response_expanded history_items={history_len} delta_items={current_len}"
    );
    Ok(Some(serde_json::to_string(&value)?))
}

/// The ChatGPT Codex HTTP endpoint requires self-contained input and rejects
/// `previous_response_id`.
/// Public OpenAI Responses endpoints and the Codex WebSocket transport have different
/// continuation semantics, so normal WebSocket turns retain the field. Only safe affinity-loss
/// or explicit reference-rejection recovery enters `prepare_codex_websocket_subscription_replay`; this HTTP
/// executor always expands it because the subscription HTTP endpoint rejects the reference.
/// Unlike CLIProxyAPI's source-specific replay cache, we key the complete replay by the concrete
/// response id because VS Code sends only tool-output deltas.
pub(crate) fn prepare_codex_http_subscription_request(
    config: &SupplierConfig,
    body: &str,
) -> Result<String> {
    let mut value: serde_json::Value = serde_json::from_str(body)?;
    let Some(object) = value.as_object_mut() else {
        return Ok(body.to_string());
    };
    let Some(previous_response_id) = object
        .get("previous_response_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
    else {
        return Ok(body.to_string());
    };
    let Some(state) = codex_http_continuation_state(config, &previous_response_id) else {
        // Official Responses guidance requires an unresolved continuation to become a new turn
        // with self-contained input. The subscription HTTP endpoint rejects the reference itself,
        // while blindly removing it leaves function_call_output items orphaned. Follow
        // ref/CLIProxyAPI's pairing rule: retain complete call/output pairs and non-tool input,
        // discard only unresolvable tool items, then provide a neutral fallback turn if nothing
        // usable remains. This is deliberately the last-resort path; normal restarts are handled
        // by the bounded local continuation database above.
        let recovery = repair_unresolved_codex_continuation(object);
        let repaired = serde_json::to_string(&value)?;
        log::warn!(
            "[const-api][protocol] subscription continuation degraded code=codex_previous_response_context_missing action=start_new_turn kept_items={} dropped_orphan_tool_items={} fallback_message={}",
            recovery.kept_items,
            recovery.dropped_orphan_tool_items,
            recovery.fallback_message
        );
        return Ok(repaired);
    };
    let (history_len, current_len) = apply_codex_continuation_state(object, state);
    let repaired = serde_json::to_string(&value)?;
    log::info!(
        "[const-api][protocol] subscription continuation replayed code=codex_previous_response_expanded history_items={history_len} delta_items={current_len}"
    );
    Ok(repaired)
}

#[derive(Debug, Default, PartialEq, Eq)]
struct CodexUnresolvedContinuationRecovery {
    kept_items: usize,
    dropped_orphan_tool_items: usize,
    fallback_message: bool,
}

fn codex_tool_item_pair(
    item: &serde_json::Value,
) -> Option<(&'static str, bool, String)> {
    let object = item.as_object()?;
    let item_type = object.get("type")?.as_str()?;
    let (family, is_call) = match item_type {
        "function_call" | "local_shell_call" => ("function", true),
        "function_call_output" => ("function", false),
        "custom_tool_call" => ("custom", true),
        "custom_tool_call_output" => ("custom", false),
        "tool_search_call" => ("tool_search", true),
        "tool_search_output"
            if object.get("execution").and_then(serde_json::Value::as_str) != Some("server") =>
        {
            ("tool_search", false)
        }
        _ => return None,
    };
    let call_id = object
        .get("call_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_string();
    Some((family, is_call, call_id))
}

fn codex_tool_output_requires_call_context(item: &serde_json::Value) -> bool {
    let Some(object) = item.as_object() else {
        return false;
    };
    match object.get("type").and_then(serde_json::Value::as_str) {
        Some("function_call_output") => object
            .get("call_id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
            || !object
                .get("name")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|value| !value.trim().is_empty()),
        Some("custom_tool_call_output") => true,
        Some("tool_search_output") => {
            object.get("execution").and_then(serde_json::Value::as_str) != Some("server")
        }
        Some(item_type) if item_type.ends_with("_call_output") => true,
        _ => false,
    }
}

pub(crate) fn codex_orphaned_tool_output_count(input: &[serde_json::Value]) -> usize {
    let mut calls = HashSet::new();
    let mut orphaned = 0;
    for item in input {
        if let Some((family, is_call, call_id)) = codex_tool_item_pair(item) {
            let key = (family, call_id);
            if is_call {
                calls.insert(key);
            } else if !calls.contains(&key) {
                orphaned += 1;
            }
        } else if codex_tool_output_requires_call_context(item) {
            orphaned += 1;
        }
    }
    orphaned
}

fn repair_unresolved_codex_continuation(
    object: &mut serde_json::Map<String, serde_json::Value>,
) -> CodexUnresolvedContinuationRecovery {
    let current = object
        .get("input")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut calls = std::collections::HashSet::<(&'static str, String)>::new();
    let mut outputs = std::collections::HashSet::<(&'static str, String)>::new();
    for item in &current {
        let Some((family, is_call, call_id)) = codex_tool_item_pair(item) else {
            continue;
        };
        if is_call {
            calls.insert((family, call_id));
        } else {
            outputs.insert((family, call_id));
        }
    }

    let mut recovery = CodexUnresolvedContinuationRecovery::default();
    let mut repaired = Vec::with_capacity(current.len());
    for item in current {
        let item_type = item
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let is_tool_call = matches!(
            item_type,
            "function_call"
                | "local_shell_call"
                | "custom_tool_call"
                | "tool_search_call"
        );
        let keep = match codex_tool_item_pair(&item) {
            Some((family, true, call_id)) => outputs.contains(&(family, call_id)),
            Some((family, false, call_id)) => calls.contains(&(family, call_id)),
            None => !is_tool_call && !codex_tool_output_requires_call_context(&item),
        };
        if keep {
            repaired.push(item);
        } else {
            recovery.dropped_orphan_tool_items += 1;
        }
    }

    if repaired.is_empty() {
        repaired.push(serde_json::json!({
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": "Continue the task from the available context. The previous response context was unavailable after reconnecting."
            }]
        }));
        recovery.fallback_message = true;
    }
    recovery.kept_items = repaired.len();
    object.remove("previous_response_id");
    object.insert("input".to_string(), serde_json::Value::Array(repaired));
    recovery
}

fn codex_http_completed_response(
    raw: &str,
) -> Option<(String, Vec<serde_json::Value>)> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let response = value.get("response").unwrap_or(&value);
    let response_id = response.get("id")?.as_str()?.trim().to_string();
    let output = response.get("output")?.as_array()?.clone();
    (!response_id.is_empty() && !output.is_empty()).then_some((response_id, output))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CodexHttpContinuationProgress {
    pub(crate) terminal_seen: bool,
    pub(crate) cached: bool,
}

/// Collects only the continuation-relevant parts of a Codex Responses SSE stream.
///
/// Each byte is decoded once. In particular, a fragmented `response.completed` event is not
/// treated as complete merely because its name appeared in the partial JSON payload.
pub(crate) struct CodexHttpContinuationCollector {
    decoder: crate::protocol::stream::SseDecoder,
    input: Option<Vec<serde_json::Value>>,
    cache_identity: Option<String>,
    created_response_id: String,
    indexed_items: std::collections::BTreeMap<u64, serde_json::Value>,
    fallback_items: Vec<serde_json::Value>,
    terminal_seen: bool,
    cached: bool,
    failed: bool,
    websocket_pin: Option<u128>,
}

impl CodexHttpContinuationCollector {
    pub(crate) fn new(request_body: &str) -> Self {
        let request = serde_json::from_str::<serde_json::Value>(request_body).ok();
        let input = request
            .as_ref()
            .and_then(|request| request.get("input"))
            .and_then(serde_json::Value::as_array)
            .cloned();
        let cache_identity = request
            .as_ref()
            .and_then(|request| request.get("prompt_cache_key"))
            .and_then(serde_json::Value::as_str)
            .and_then(|value| normalized_subscription_cache_identity(Some(value)))
            .map(str::to_string);
        Self::from_capture_context(input, cache_identity)
    }

    /// Captures the complete logical input while the wire request may still use
    /// a same-connection previous_response_id. If the parent history is absent,
    /// collection is disabled so an incremental delta can never poison the
    /// bounded replay cache.
    pub(crate) fn new_for_websocket(
        config: &SupplierConfig,
        request_body: &str,
    ) -> Self {
        let mut request = serde_json::from_str::<serde_json::Value>(request_body).ok();
        let previous_response_id = request
            .as_ref()
            .and_then(|request| request.get("previous_response_id"))
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let input = match previous_response_id {
            Some(previous_response_id) => {
                let state = codex_http_continuation_state(config, &previous_response_id);
                match (request.as_mut().and_then(serde_json::Value::as_object_mut), state) {
                    (Some(object), Some(state)) => {
                        let _ = apply_codex_continuation_state(object, state);
                        object
                            .get("input")
                            .and_then(serde_json::Value::as_array)
                            .cloned()
                    }
                    _ => None,
                }
            }
            None => request
                .as_ref()
                .and_then(|request| request.get("input"))
                .and_then(serde_json::Value::as_array)
                .cloned(),
        };
        let cache_identity = request
            .as_ref()
            .and_then(|request| request.get("prompt_cache_key"))
            .and_then(serde_json::Value::as_str)
            .and_then(|value| normalized_subscription_cache_identity(Some(value)))
            .map(str::to_string);
        Self::from_capture_context(input, cache_identity)
    }

    pub(crate) fn new_for_pinned_websocket(
        config: &SupplierConfig,
        request_body: &str,
        pin: &CodexWebsocketContinuationPin,
    ) -> Self {
        // Protect a resumed parent before looking it up/inserting it from disk.
        if let Some(parent) = serde_json::from_str::<serde_json::Value>(request_body).ok()
            .and_then(|value| value.get("previous_response_id").and_then(serde_json::Value::as_str).map(str::to_string))
            .filter(|value| !value.trim().is_empty())
        {
            codex_http_continuation_cache().lock().unwrap_or_else(|poisoned| poisoned.into_inner())
                .websocket_pins.insert(pin.0, codex_http_continuation_key(config, &parent));
        }
        let mut collector = Self::new_for_websocket(config, request_body);
        collector.websocket_pin = Some(pin.0);
        collector
    }

    fn from_capture_context(
        input: Option<Vec<serde_json::Value>>,
        cache_identity: Option<String>,
    ) -> Self {
        Self {
            decoder: crate::protocol::stream::SseDecoder::default(),
            input,
            cache_identity,
            created_response_id: String::new(),
            indexed_items: std::collections::BTreeMap::new(),
            fallback_items: Vec::new(),
            terminal_seen: false,
            cached: false,
            failed: false,
            websocket_pin: None,
        }
    }

    pub(crate) fn progress(&self) -> CodexHttpContinuationProgress {
        CodexHttpContinuationProgress {
            terminal_seen: self.terminal_seen,
            cached: self.cached,
        }
    }

    pub(crate) fn push(
        &mut self,
        config: &SupplierConfig,
        bytes: &[u8],
    ) -> CodexHttpContinuationProgress {
        if self.terminal_seen || self.failed || self.input.is_none() {
            return self.progress();
        }
        match self.decoder.push(bytes) {
            Ok(frames) => self.process_frames(config, frames),
            Err(error) => {
                self.failed = true;
                log::warn!(
                    "[const-api][protocol] subscription continuation stream decode failed code=codex_continuation_stream_decode_failed error={error}"
                );
            }
        }
        self.progress()
    }

    pub(crate) fn finish(
        &mut self,
        config: &SupplierConfig,
    ) -> CodexHttpContinuationProgress {
        if self.terminal_seen || self.failed || self.input.is_none() {
            return self.progress();
        }
        match self.decoder.finish() {
            Ok(frames) => self.process_frames(config, frames),
            Err(error) => {
                self.failed = true;
                log::warn!(
                    "[const-api][protocol] subscription continuation stream finish failed code=codex_continuation_stream_finish_failed error={error}"
                );
            }
        }
        self.progress()
    }

    pub(crate) fn push_websocket_text(
        &mut self,
        config: &SupplierConfig,
        text: &str,
    ) -> CodexHttpContinuationProgress {
        if self.terminal_seen || self.failed || self.input.is_none() {
            return self.progress();
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
            return self.progress();
        };
        let kind = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        self.process_value(config, &kind, &value);
        self.progress()
    }

    fn process_frames(
        &mut self,
        config: &SupplierConfig,
        frames: Vec<crate::protocol::stream::SseFrame>,
    ) {
        for frame in frames {
            if self.terminal_seen {
                break;
            }
            let data = frame.data.trim();
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
                // Ignore forward-compatible non-JSON/diagnostic frames; one unrelated frame must
                // not prevent us from retaining the completed response state.
                continue;
            };
            let kind = value
                .get("type")
                .and_then(serde_json::Value::as_str)
                .or(frame.event.as_deref())
                .unwrap_or_default();
            self.process_value(config, kind, &value);
        }
    }

    fn process_value(
        &mut self,
        config: &SupplierConfig,
        kind: &str,
        value: &serde_json::Value,
    ) {
        match kind {
            "response.created" => {
                if let Some(id) = value
                    .pointer("/response/id")
                    .and_then(serde_json::Value::as_str)
                {
                    self.created_response_id = id.trim().to_string();
                }
            }
            "response.output_item.done" => {
                let Some(item) = value.get("item").cloned() else {
                    return;
                };
                if let Some(index) = value
                    .get("output_index")
                    .and_then(serde_json::Value::as_u64)
                {
                    self.indexed_items.insert(index, item);
                } else {
                    self.fallback_items.push(item);
                }
            }
            "response.completed" => self.complete(config, value),
            _ => {}
        }
    }

    fn complete(
        &mut self,
        config: &SupplierConfig,
        value: &serde_json::Value,
    ) {
        self.terminal_seen = true;
        let response_id = value
            .pointer("/response/id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| self.created_response_id.clone());
        let mut output = value
            .pointer("/response/output")
            .and_then(serde_json::Value::as_array)
            .filter(|output| !output.is_empty())
            .cloned()
            .unwrap_or_default();
        if output.is_empty() {
            output.extend(std::mem::take(&mut self.indexed_items).into_values());
            output.append(&mut self.fallback_items);
        }
        let Some(mut history) = self.input.take() else {
            return;
        };
        if response_id.is_empty() || output.is_empty() {
            return;
        }
        history.extend(output);
        self.cached = insert_codex_http_continuation_history_with_pin(
            config,
            &response_id,
            history,
            self.cache_identity.as_deref(),
            self.websocket_pin,
        );
        if self.cached {
            log::debug!(
                "[const-api][protocol] subscription continuation cached code=codex_response_context_captured"
            );
        }
    }
}

pub(crate) fn capture_codex_http_subscription_continuation(
    config: &SupplierConfig,
    request_body: &str,
    response_body: &str,
) -> bool {
    if looks_like_sse_body(response_body) {
        let mut collector = CodexHttpContinuationCollector::new(request_body);
        let _ = collector.push(config, response_body.as_bytes());
        return collector.finish(config).cached;
    }
    let Some((response_id, output)) = codex_http_completed_response(response_body) else {
        return false;
    };
    let Ok(request) = serde_json::from_str::<serde_json::Value>(request_body) else {
        return false;
    };
    let Some(input) = request
        .get("input")
        .and_then(serde_json::Value::as_array)
        .cloned()
    else {
        return false;
    };
    let mut history = input;
    history.extend(output);
    let cache_identity = request
        .get("prompt_cache_key")
        .and_then(serde_json::Value::as_str);
    let inserted = insert_codex_http_continuation_history(
        config,
        &response_id,
        history,
        cache_identity,
    );
    if inserted {
        log::debug!(
            "[const-api][protocol] subscription continuation cached code=codex_response_context_captured"
        );
    }
    inserted
}

#[cfg(test)]
mod codex_http_continuation_collector_tests {
    use super::*;

    #[test]
    fn continuation_shares_cached_history_without_mutating_replays() {
        let mut config = default_supplier_config();
        config.channel_id = "continuation-shared-history".into();
        let history = vec![serde_json::json!({"role":"user", "content":"cached input"})];
        assert!(insert_codex_http_continuation_history(&config, "resp-shared", history.clone(), Some("stable-key")));
        let first = codex_http_continuation_state(&config, "resp-shared").unwrap();
        let second = codex_http_continuation_state(&config, "resp-shared").unwrap();
        assert!(Arc::ptr_eq(&first.history, &second.history), "lookup must not deep-copy the transcript");
        for identity in [None, Some("caller-key")] {
            let mut request = serde_json::json!({
                "previous_response_id": "resp-shared", "input": [{"role":"user", "content":"next"}]
            }).as_object().unwrap().clone();
            if let Some(identity) = identity {
                request.insert("prompt_cache_key".into(), serde_json::json!(identity));
            }
            assert_eq!(apply_codex_continuation_state(&mut request, first.clone()), (1, 1));
            assert_eq!(request["prompt_cache_key"], identity.unwrap_or("stable-key"));
            assert_eq!(request["input"].as_array().unwrap().len(), 2);
            assert_eq!(*second.history, history, "replay must not modify cached history");
        }
        let mut complete = serde_json::json!({"input": history, "previous_response_id":"resp-shared"})
            .as_object().unwrap().clone();
        let input_pointer = complete["input"].as_array().unwrap().as_ptr();
        apply_codex_continuation_state(&mut complete, first);
        assert_eq!(complete["input"].as_array().unwrap().as_ptr(), input_pointer,
            "an already complete transcript should be moved, not copied or duplicated");
    }

    #[test]
    fn continuation_serialization_retains_legacy_and_current_disk_contracts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("continuations.sqlite3");
        let history = vec![serde_json::json!({"type":"reasoning", "encrypted_content":"opaque", "text":"中文"})];
        for identity in [None, Some("stable-key")] {
            let state = CodexHttpContinuationState {
                history: Arc::new(history.clone()),
                cache_identity: identity.map(str::to_string),
            };
            let old = serde_json::to_vec(&PersistedCodexHttpContinuation {
                history: history.clone(), cache_identity: identity.unwrap_or_default().into(),
            }).unwrap();
            assert_eq!(state.serialized().unwrap(), old);
            persist_codex_http_continuation_bytes_to_path(&path, "current", &old, 1000).unwrap();
            assert_eq!(load_codex_http_continuation_from_path(&path, "current", 1001).unwrap(), Some(state));
        }
        let legacy = format!(" \n{}", serde_json::to_string(&history).unwrap());
        persist_codex_http_continuation_bytes_to_path(&path, "legacy", legacy.as_bytes(), 1000).unwrap();
        let restored = load_codex_http_continuation_from_path(&path, "legacy", 1001).unwrap().unwrap();
        assert_eq!(*restored.history, history);
        assert_eq!(restored.cache_identity, None);
        assert!(load_codex_http_continuation_from_path(&path, "legacy", 1002 + CODEX_HTTP_CONTINUATION_CACHE_TTL.as_secs() as i64).unwrap().is_none());
    }

    #[test]
    fn continuation_pressure_preserves_latest_active_ws_and_stays_bounded() {
        let mut cache = CodexHttpContinuationCache::default();
        let now = Instant::now();
        for i in 0..10 {
            let key = format!("entry-{i}");
            cache.entries.insert(key, CodexHttpContinuationEntry { history: Arc::new(vec![]), cache_identity: None, bytes: 8 * 1024 * 1024, touched_at: now + Duration::from_millis(i) });
            cache.bytes += 8 * 1024 * 1024;
        }
        cache.websocket_pins.insert(1, "entry-0".into());
        prune_codex_http_continuation_cache(&mut cache, now);
        assert!(cache.entries.contains_key("entry-0"), "busy sessions evicted another live session");
        assert!(!cache.entries.contains_key("entry-1"));
        assert!(cache.bytes <= CODEX_HTTP_CONTINUATION_CACHE_MAX_BYTES);
        // Protection is priority, never an unbounded memory exemption.
        for i in 0..10 {
            cache.websocket_pins.insert(i + 2, format!("extra-{i}"));
            cache.entries.insert(format!("extra-{i}"), CodexHttpContinuationEntry { history: Arc::new(vec![]), cache_identity: None, bytes: 8 * 1024 * 1024, touched_at: now });
            cache.bytes += 8 * 1024 * 1024;
        }
        prune_codex_http_continuation_cache(&mut cache, now);
        assert!(cache.bytes <= CODEX_HTTP_CONTINUATION_CACHE_MAX_BYTES);
    }

    #[test]
    fn continuation_disk_pruning_honors_live_ws_pin_and_releases_it() {
        let dir = tempfile::tempdir().unwrap();
        let connection = open_codex_http_continuation_db(&dir.path().join("test.sqlite3")).unwrap();
        let pin = CodexWebsocketContinuationPin::new();
        let key = format!("disk-pin-{}", pin.0);
        codex_http_continuation_cache().lock().unwrap().websocket_pins.insert(pin.0, key.clone());
        connection.execute("INSERT INTO codex_http_continuations VALUES (?1, 1000, 8388608, X'5B5D')", [&key]).unwrap();
        for i in 0..8 {
            connection.execute("INSERT INTO codex_http_continuations VALUES (?1, ?2, 8388608, X'5B5D')", rusqlite::params![format!("new-{i}"), 1001 + i]).unwrap();
        }
        prune_codex_http_continuation_db(&connection, 1100).unwrap();
        let contains = || connection.query_row("SELECT COUNT(*) FROM codex_http_continuations WHERE cache_key=?1", [&key], |row| row.get::<_, i64>(0)).unwrap();
        assert_eq!(contains(), 1);
        drop(pin);
        assert!(!codex_websocket_pinned_keys().contains(&key));
        connection.execute("INSERT INTO codex_http_continuations VALUES ('newest', 1101, 8388608, X'5B5D')", []).unwrap();
        prune_codex_http_continuation_db(&connection, 1102).unwrap();
        assert_eq!(contains(), 0);
    }

    #[test]
    fn websocket_pin_advances_only_on_completed_response() {
        let mut config = default_supplier_config();
        config.channel_id = "pin-lifecycle-test".into();
        let pin = CodexWebsocketContinuationPin::new();
        let request = r#"{"input":[{"role":"user","content":"hello"}],"store":false,"prompt_cache_key":"stable-pin"}"#;
        let mut first = CodexHttpContinuationCollector::new_for_pinned_websocket(&config, request, &pin);
        first.push_websocket_text(&config, r#"{"type":"response.completed","response":{"id":"pin-parent","output":[{"type":"message","role":"assistant","content":"answer"}]}}"#);
        let parent = codex_http_continuation_key(&config, "pin-parent");
        assert!(codex_websocket_pinned_keys().contains(&parent));
        let next = r#"{"previous_response_id":"pin-parent","input":[{"role":"user","content":"next"}],"store":false}"#;
        let mut child = CodexHttpContinuationCollector::new_for_pinned_websocket(&config, next, &pin);
        child.push_websocket_text(&config, r#"{"type":"response.created","response":{"id":"pin-child"}}"#);
        assert!(codex_websocket_pinned_keys().contains(&parent));
        child.push_websocket_text(&config, r#"{"type":"response.completed","response":{"id":"pin-child","output":[{"type":"message","role":"assistant","content":"second"}]}}"#);
        let keys = codex_websocket_pinned_keys();
        assert!(!keys.contains(&parent));
        assert!(keys.contains(&codex_http_continuation_key(&config, "pin-child")));
        assert_eq!(codex_http_continuation_state(&config, "pin-child").unwrap().cache_identity.as_deref(), Some("stable-pin"));
    }

    #[test]
    fn codex_tool_output_validation_matches_current_codex_families() {
        let valid = vec![
            serde_json::json!({
                "type": "local_shell_call",
                "call_id": "shell-1",
                "status": "completed",
                "action": {"type": "exec", "command": ["pwd"]}
            }),
            serde_json::json!({
                "type": "function_call_output",
                "call_id": "shell-1",
                "output": "ok"
            }),
            serde_json::json!({
                "type": "tool_search_call",
                "call_id": "search-1",
                "execution": "client",
                "arguments": {"query": "files"}
            }),
            serde_json::json!({
                "type": "tool_search_output",
                "call_id": "search-1",
                "execution": "client",
                "status": "completed",
                "tools": []
            }),
            serde_json::json!({
                "type": "tool_search_output",
                "execution": "server",
                "status": "completed",
                "tools": []
            }),
            serde_json::json!({
                "type": "function_call_output",
                "name": "legacy_named_tool",
                "output": "ok"
            }),
        ];
        assert_eq!(codex_orphaned_tool_output_count(&valid), 0);

        let invalid = vec![
            serde_json::json!({
                "type": "custom_tool_call_output",
                "call_id": "missing-custom",
                "output": "result"
            }),
            serde_json::json!({
                "type": "tool_search_output",
                "call_id": "missing-search",
                "execution": "client",
                "status": "completed",
                "tools": []
            }),
            serde_json::json!({
                "type": "custom_tool_call_output",
                "call_id": "",
                "output": "result"
            }),
        ];
        assert_eq!(codex_orphaned_tool_output_count(&invalid), 3);
    }

    #[test]
    fn terminal_progress_is_reported_only_after_a_complete_sse_frame() {
        let mut config = default_supplier_config();
        config.channel_id = "fragmented-terminal".to_string();
        let request = serde_json::json!({
            "model": "gpt-5.6-sol",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "hello"}]
            }],
            "stream": true
        })
        .to_string();
        let mut collector = CodexHttpContinuationCollector::new(&request);

        let progress = collector.push(
            &config,
            b"event: response.completed\n\
              data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_fragmented\"",
        );
        assert!(!progress.terminal_seen);

        let progress = collector.push(&config, b",\"output\":[]}}");
        assert!(!progress.terminal_seen);

        let progress = collector.push(&config, b"\n\n");
        assert!(progress.terminal_seen);
        assert!(!progress.cached);
    }

    #[test]
    fn websocket_collector_builds_a_self_contained_reconnect_request() {
        let mut config = default_supplier_config();
        config.channel_id = "ws-reconnect-complete-context".to_string();
        config.credential_ref = "ws-reconnect-account".to_string();
        let first_request = serde_json::json!({
            "type": "response.create",
            "model": "gpt-5.6-sol",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "read a file"}]
            }],
            "prompt_cache_key": "stable-ws-session",
            "store": false,
            "stream": true
        })
        .to_string();
        let mut collector =
            CodexHttpContinuationCollector::new_for_websocket(&config, &first_request);
        let _ = collector.push_websocket_text(
            &config,
            r#"{"type":"response.created","response":{"id":"resp_ws_parent"}}"#,
        );
        let _ = collector.push_websocket_text(
            &config,
            r#"{"type":"response.output_item.done","output_index":0,"item":{"id":"rs_ws_parent","type":"reasoning","encrypted_content":"opaque","summary":[]}}"#,
        );
        let _ = collector.push_websocket_text(
            &config,
            r#"{"type":"response.output_item.done","output_index":1,"item":{"id":"fc_ws_parent","type":"function_call","call_id":"call_ws_parent","name":"read_file","arguments":"{}"}}"#,
        );
        let progress = collector.push_websocket_text(
            &config,
            r#"{"type":"response.completed","response":{"id":"resp_ws_parent","output":[]}}"#,
        );
        assert!(progress.terminal_seen);
        assert!(progress.cached);

        let continuation = serde_json::json!({
            "type": "response.create",
            "model": "gpt-5.6-sol",
            "previous_response_id": "resp_ws_parent",
            "input": [{
                "type": "function_call_output",
                "call_id": "call_ws_parent",
                "output": "file contents"
            }],
            "include": [],
            "store": false,
            "stream": true
        })
        .to_string();
        let replay = prepare_codex_websocket_subscription_replay(&config, &continuation)
            .expect("prepare reconnect replay")
            .expect("complete context should be replayable");
        let replay: serde_json::Value = serde_json::from_str(&replay).expect("replay JSON");

        assert!(replay.get("previous_response_id").is_none());
        assert_eq!(replay["prompt_cache_key"], "stable-ws-session");
        assert_eq!(replay["input"].as_array().map(Vec::len), Some(4));
        assert!(replay["input"][1].get("id").is_none());
        assert_eq!(replay["input"][1]["encrypted_content"], "opaque");
        assert_eq!(replay["input"][2]["call_id"], "call_ws_parent");
        assert_eq!(replay["input"][3]["call_id"], "call_ws_parent");
        assert!(replay["include"]
            .as_array()
            .is_some_and(|include| include.iter().any(|item| {
                item.as_str() == Some("reasoning.encrypted_content")
            })));

        let mut stored_continuation: serde_json::Value =
            serde_json::from_str(&continuation).expect("stored continuation JSON");
        stored_continuation["store"] = true.into();
        assert!(prepare_codex_websocket_subscription_replay(
            &config,
            &stored_continuation.to_string(),
        )
        .expect("store=true classification")
        .is_none());
    }

    #[test]
    fn websocket_collector_does_not_cache_an_incremental_child_without_its_parent() {
        let mut config = default_supplier_config();
        config.channel_id = "ws-reconnect-missing-parent".to_string();
        config.credential_ref = "ws-reconnect-missing-account".to_string();
        let continuation = serde_json::json!({
            "type": "response.create",
            "model": "gpt-5.6-sol",
            "previous_response_id": "resp_ws_unknown_parent",
            "input": [{"type": "message", "role": "user", "content": "delta"}],
            "store": false,
            "stream": true
        })
        .to_string();
        let mut collector =
            CodexHttpContinuationCollector::new_for_websocket(&config, &continuation);
        let _ = collector.push_websocket_text(
            &config,
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"answer"}]}}"#,
        );
        let progress = collector.push_websocket_text(
            &config,
            r#"{"type":"response.completed","response":{"id":"resp_ws_incomplete_child","output":[]}}"#,
        );
        assert!(!progress.cached);

        let next = serde_json::json!({
            "type": "response.create",
            "model": "gpt-5.6-sol",
            "previous_response_id": "resp_ws_incomplete_child",
            "input": [{"type": "message", "role": "user", "content": "next"}],
            "store": false,
            "stream": true
        })
        .to_string();
        assert!(prepare_codex_websocket_subscription_replay(&config, &next)
            .expect("missing replay is not an encoding error")
            .is_none());
    }
}
