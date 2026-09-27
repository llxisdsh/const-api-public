use rusqlite::{params, Connection, OptionalExtension};

// The UI keeps at most 200 recent local calls. Retain a modest restart/history
// margin while long-term channel totals stay in the separate aggregate tables.
const SUBSCRIPTION_USAGE_RECORD_LIMIT: i64 = 500;
const SUBSCRIPTION_USAGE_STATS_RETENTION_DAYS: i64 = 90;
const SUBSCRIPTION_USAGE_DB_MAX_PAGES: i64 = 16_384;
const SUBSCRIPTION_USAGE_WAL_LIMIT_BYTES: i64 = 4 * 1024 * 1024;
const SUBSCRIPTION_USAGE_RECORD_CLEANUP_INTERVAL: u64 = 64;
const SUBSCRIPTION_USAGE_HISTORY_CLEANUP_INTERVAL: u64 = 256;
const SUBSCRIPTION_USAGE_WRITE_QUEUE_CAPACITY: usize = 512;

static SUBSCRIPTION_USAGE_DB: OnceLock<StdMutex<Option<(PathBuf, Connection)>>> = OnceLock::new();
static SUBSCRIPTION_USAGE_WRITES_SINCE_HISTORY_CLEANUP: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
static SUBSCRIPTION_USAGE_WRITER: OnceLock<
    std::sync::mpsc::SyncSender<SubscriptionUsageWriteCommand>,
> = OnceLock::new();
static SUBSCRIPTION_USAGE_WRITER_INIT: OnceLock<StdMutex<()>> = OnceLock::new();
static SUBSCRIPTION_USAGE_WRITER_ERROR: OnceLock<StdMutex<Option<String>>> = OnceLock::new();

enum SubscriptionUsageWriteCommand {
    Record {
        path: PathBuf,
        record: serde_json::Value,
    },
    Flush(std::sync::mpsc::Sender<Option<String>>),
}

pub(crate) fn subscription_usage_db_path() -> PathBuf {
    crate::client_data_root()
        .join("state")
        .join("channel-usage.sqlite3")
}

fn with_subscription_usage_db<T>(
    operation: impl FnOnce(&mut Connection) -> Result<T>,
) -> Result<T> {
    with_subscription_usage_db_at(subscription_usage_db_path(), operation)
}

fn with_subscription_usage_db_at<T>(
    path: PathBuf,
    operation: impl FnOnce(&mut Connection) -> Result<T>,
) -> Result<T> {
    let mut database = SUBSCRIPTION_USAGE_DB
        .get_or_init(|| StdMutex::new(None))
        .lock()
        .map_err(|_| anyhow!("subscription usage database lock poisoned"))?;
    let path_changed = database
        .as_ref()
        .map(|(open_path, _)| open_path != &path)
        .unwrap_or(true);
    if path_changed {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut connection = Connection::open(&path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        connection.pragma_update(None, "wal_autocheckpoint", 256)?;
        connection.pragma_update(
            None,
            "journal_size_limit",
            SUBSCRIPTION_USAGE_WAL_LIMIT_BYTES,
        )?;
        connection.pragma_update(None, "max_page_count", SUBSCRIPTION_USAGE_DB_MAX_PAGES)?;
        initialize_subscription_usage_db(&mut connection)?;
        *database = Some((path, connection));
    }
    let Some((_, connection)) = database.as_mut() else {
        return Err(anyhow!("subscription usage database was not initialized"));
    };
    operation(connection)
}

fn initialize_subscription_usage_db(connection: &mut Connection) -> Result<()> {
    connection.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS channel_usage_records (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ts INTEGER NOT NULL,
            ts_ms INTEGER NOT NULL,
            provider TEXT NOT NULL DEFAULT '',
            channel_id TEXT NOT NULL DEFAULT '',
            channel TEXT NOT NULL DEFAULT '',
            record_json TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_channel_usage_recent
            ON channel_usage_records(ts_ms DESC, id DESC);

        CREATE TABLE IF NOT EXISTS channel_usage_stats (
            stat_key TEXT PRIMARY KEY,
            bucket TEXT NOT NULL,
            record_json TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_channel_usage_stats_bucket
            ON channel_usage_stats(bucket DESC);
        CREATE TABLE IF NOT EXISTS channel_usage_daily (
            provider TEXT NOT NULL,
            channel_key TEXT NOT NULL,
            day_index INTEGER NOT NULL,
            requests INTEGER NOT NULL,
            PRIMARY KEY(provider, channel_key, day_index)
        );
        CREATE TABLE IF NOT EXISTS channel_usage_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        ",
    )?;
    migrate_legacy_subscription_usage(connection)
}

fn migrate_legacy_subscription_usage(connection: &mut Connection) -> Result<()> {
    let migrated: Option<String> = connection
        .query_row(
            "SELECT value FROM channel_usage_meta WHERE key = 'legacy_json_v1_imported'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if migrated.is_some() {
        return Ok(());
    }

    let usage_raw = match fs::read_to_string(subscription_usage_log_path()) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            fs::read_to_string(subscription_audit_log_path()).unwrap_or_default()
        }
        Err(err) => return Err(err.into()),
    };
    let mut records = usage_raw
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect::<Vec<_>>();
    if records.len() > SUBSCRIPTION_USAGE_RECORD_LIMIT as usize {
        records.drain(0..records.len() - SUBSCRIPTION_USAGE_RECORD_LIMIT as usize);
    }
    let legacy_stats = fs::read_to_string(subscription_usage_stats_path())
        .ok()
        .and_then(|raw| {
            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&raw).ok()
        })
        .unwrap_or_default();

    let transaction = connection.transaction()?;
    for record in &records {
        insert_subscription_usage_record(&transaction, record)?;
        increment_subscription_usage_daily_tx(&transaction, record)?;
    }
    if legacy_stats.is_empty() {
        for record in &records {
            upsert_subscription_usage_stat_tx(&transaction, record)?;
        }
    } else {
        let cutoff = subscription_usage_stats_cutoff();
        for (key, stat) in legacy_stats {
            let bucket = stat
                .get("bucket")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            if bucket >= cutoff.as_str() {
                transaction.execute(
                    "INSERT INTO channel_usage_stats(stat_key, bucket, record_json)
                     VALUES(?1, ?2, ?3)
                     ON CONFLICT(stat_key) DO UPDATE SET
                        bucket = excluded.bucket, record_json = excluded.record_json",
                    params![key, bucket, serde_json::to_string(&stat)?],
                )?;
            }
        }
    }
    enforce_subscription_usage_retention(&transaction)?;
    transaction.execute(
        "INSERT INTO channel_usage_meta(key, value) VALUES('legacy_json_v1_imported', '1')
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [],
    )?;
    transaction.commit()?;
    Ok(())
}

pub(crate) fn append_subscription_usage_record(record: &serde_json::Value) -> Result<()> {
    let command = SubscriptionUsageWriteCommand::Record {
        path: subscription_usage_db_path(),
        record: record.clone(),
    };
    match subscription_usage_writer()?.try_send(command) {
        Ok(()) => Ok(()),
        Err(std::sync::mpsc::TrySendError::Full(command)) => {
            // Preserve every local history/stat record under an extreme
            // completion burst. Backpressure happens only after 512 queued
            // completions and still keeps SQLite on the single writer.
            subscription_usage_writer()?
                .send(command)
                .map_err(|_| anyhow!("subscription usage writer stopped"))
        }
        Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
            Err(anyhow!("subscription usage writer stopped"))
        }
    }
}

fn subscription_usage_writer(
) -> Result<&'static std::sync::mpsc::SyncSender<SubscriptionUsageWriteCommand>> {
    if let Some(writer) = SUBSCRIPTION_USAGE_WRITER.get() {
        return Ok(writer);
    }
    let _initialization = SUBSCRIPTION_USAGE_WRITER_INIT
        .get_or_init(|| StdMutex::new(()))
        .lock()
        .map_err(|_| anyhow!("subscription usage writer initialization poisoned"))?;
    if let Some(writer) = SUBSCRIPTION_USAGE_WRITER.get() {
        return Ok(writer);
    }
    let (sender, receiver) =
        std::sync::mpsc::sync_channel(SUBSCRIPTION_USAGE_WRITE_QUEUE_CAPACITY);
    std::thread::Builder::new()
        .name("const-usage-db".to_string())
        .spawn(move || subscription_usage_writer_loop(receiver))
        .context("start subscription usage writer")?;
    let _ = SUBSCRIPTION_USAGE_WRITER.set(sender);
    SUBSCRIPTION_USAGE_WRITER
        .get()
        .ok_or_else(|| anyhow!("subscription usage writer initialization failed"))
}

fn subscription_usage_writer_loop(
    receiver: std::sync::mpsc::Receiver<SubscriptionUsageWriteCommand>,
) {
    while let Ok(command) = receiver.recv() {
        match command {
            SubscriptionUsageWriteCommand::Record { path, record } => {
                let result = write_subscription_usage_record(path, &record);
                if let Err(error) = result {
                    if let Ok(mut slot) = SUBSCRIPTION_USAGE_WRITER_ERROR
                        .get_or_init(|| StdMutex::new(None))
                        .lock()
                    {
                        *slot = Some(error.to_string());
                    }
                    log::warn!("[const-api][usage-db] asynchronous write failed: {error}");
                }
            }
            SubscriptionUsageWriteCommand::Flush(acknowledge) => {
                let error = SUBSCRIPTION_USAGE_WRITER_ERROR
                    .get_or_init(|| StdMutex::new(None))
                    .lock()
                    .map(|mut slot| slot.take())
                    .unwrap_or_else(|_| {
                        Some("subscription usage writer error state poisoned".to_string())
                    });
                let _ = acknowledge.send(error);
            }
        }
    }
}

fn write_subscription_usage_record(path: PathBuf, record: &serde_json::Value) -> Result<()> {
    with_subscription_usage_db_at(path, |connection| {
        let transaction = connection.transaction()?;
        insert_subscription_usage_record(&transaction, record)?;
        upsert_subscription_usage_stat_tx(&transaction, record)?;
        increment_subscription_usage_daily_tx(&transaction, record)?;
        let writes = SUBSCRIPTION_USAGE_WRITES_SINCE_HISTORY_CLEANUP
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .saturating_add(1);
        if writes == 1 || writes % SUBSCRIPTION_USAGE_RECORD_CLEANUP_INTERVAL == 0 {
            enforce_subscription_usage_record_limit(&transaction)?;
        }
        if writes >= SUBSCRIPTION_USAGE_HISTORY_CLEANUP_INTERVAL {
            enforce_subscription_usage_history_retention(&transaction)?;
            SUBSCRIPTION_USAGE_WRITES_SINCE_HISTORY_CLEANUP
                .store(0, std::sync::atomic::Ordering::Relaxed);
        }
        transaction.commit()?;
        Ok(())
    })
}

fn flush_subscription_usage_writer() -> Result<()> {
    let Some(writer) = SUBSCRIPTION_USAGE_WRITER.get() else {
        return Ok(());
    };
    let (acknowledge, completed) = std::sync::mpsc::channel();
    writer
        .send(SubscriptionUsageWriteCommand::Flush(acknowledge))
        .map_err(|_| anyhow!("subscription usage writer stopped"))?;
    let error = completed
        .recv()
        .map_err(|_| anyhow!("subscription usage writer stopped"))?;
    if let Some(error) = error {
        return Err(anyhow!(error));
    }
    Ok(())
}

fn insert_subscription_usage_record(
    transaction: &rusqlite::Transaction<'_>,
    record: &serde_json::Value,
) -> Result<()> {
    let ts = record.get("ts").and_then(|value| value.as_i64()).unwrap_or(0);
    let ts_ms = record
        .get("ts_ms")
        .and_then(|value| value.as_i64())
        .unwrap_or_else(|| ts.saturating_mul(1000));
    transaction.execute(
        "INSERT INTO channel_usage_records(
            ts, ts_ms, provider, channel_id, channel, record_json
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            ts,
            ts_ms,
            record.get("provider").and_then(|value| value.as_str()).unwrap_or(""),
            record.get("channel_id").and_then(|value| value.as_str()).unwrap_or(""),
            record.get("channel").and_then(|value| value.as_str()).unwrap_or(""),
            serde_json::to_string(record)?,
        ],
    )?;
    Ok(())
}

fn upsert_subscription_usage_stat_tx(
    transaction: &rusqlite::Transaction<'_>,
    record: &serde_json::Value,
) -> Result<()> {
    let key = subscription_usage_stat_key(record);
    let existing: Option<String> = transaction
        .query_row(
            "SELECT record_json FROM channel_usage_stats WHERE stat_key = ?1",
            [&key],
            |row| row.get(0),
        )
        .optional()?;
    let existing = existing
        .as_deref()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok());
    let (key, bucket, stat) = updated_subscription_usage_stat(record, existing);
    transaction.execute(
        "INSERT INTO channel_usage_stats(stat_key, bucket, record_json)
         VALUES(?1, ?2, ?3)
         ON CONFLICT(stat_key) DO UPDATE SET
            bucket = excluded.bucket, record_json = excluded.record_json",
        params![key, bucket, serde_json::to_string(&stat)?],
    )?;
    Ok(())
}

fn increment_subscription_usage_daily_tx(
    transaction: &rusqlite::Transaction<'_>,
    record: &serde_json::Value,
) -> Result<()> {
    let ts = record
        .get("ts")
        .and_then(|value| value.as_i64())
        .unwrap_or_else(now_unix);
    let provider = record
        .get("provider")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let channel_id = record
        .get("channel_id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let channel = record
        .get("channel")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let channel_key = if channel_id.is_empty() {
        format!("name:{channel}")
    } else {
        format!("id:{channel_id}")
    };
    transaction.execute(
        "INSERT INTO channel_usage_daily(provider, channel_key, day_index, requests)
         VALUES(?1, ?2, ?3, 1)
         ON CONFLICT(provider, channel_key, day_index)
         DO UPDATE SET requests = requests + 1",
        params![provider, channel_key, subscription_audit_day_index(ts)],
    )?;
    Ok(())
}

fn enforce_subscription_usage_retention(
    transaction: &rusqlite::Transaction<'_>,
) -> Result<()> {
    enforce_subscription_usage_record_limit(transaction)?;
    enforce_subscription_usage_history_retention(transaction)
}

fn enforce_subscription_usage_record_limit(
    transaction: &rusqlite::Transaction<'_>,
) -> Result<()> {
    transaction.execute(
        "DELETE FROM channel_usage_records
         WHERE id < COALESCE((
            SELECT id FROM channel_usage_records
            ORDER BY id DESC LIMIT 1 OFFSET ?1
         ), 0)",
        [SUBSCRIPTION_USAGE_RECORD_LIMIT - 1],
    )?;
    Ok(())
}

fn enforce_subscription_usage_history_retention(
    transaction: &rusqlite::Transaction<'_>,
) -> Result<()> {
    transaction.execute(
        "DELETE FROM channel_usage_stats WHERE bucket < ?1",
        [subscription_usage_stats_cutoff()],
    )?;
    transaction.execute(
        "DELETE FROM channel_usage_daily WHERE day_index < ?1",
        [subscription_audit_day_index(now_unix())
            .saturating_sub(SUBSCRIPTION_USAGE_STATS_RETENTION_DAYS)],
    )?;
    Ok(())
}

fn subscription_usage_stats_cutoff() -> String {
    unix_day_bucket(
        now_unix().saturating_sub(SUBSCRIPTION_USAGE_STATS_RETENTION_DAYS * 86_400),
    )
}

pub(crate) fn query_subscription_usage_records(
    limit: usize,
    after_ms: Option<i64>,
) -> Result<Vec<serde_json::Value>> {
    flush_subscription_usage_writer()?;
    with_subscription_usage_db(|connection| {
        let mut statement = connection.prepare(
            "SELECT record_json
             FROM channel_usage_records
             WHERE (?1 IS NULL OR ts_ms > ?1)
             ORDER BY ts_ms DESC, id DESC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![after_ms, limit as i64], |row| {
            row.get::<_, String>(0)
        })?;
        let mut records = Vec::new();
        for row in rows {
            if let Ok(record) = serde_json::from_str::<serde_json::Value>(&row?) {
                records.push(record);
            }
        }
        Ok(records)
    })
}

pub(crate) fn query_subscription_usage_stats() -> Result<Vec<serde_json::Value>> {
    flush_subscription_usage_writer()?;
    with_subscription_usage_db(|connection| {
        let mut statement = connection.prepare(
            "SELECT record_json
             FROM channel_usage_stats
             ORDER BY bucket DESC, stat_key",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut stats = Vec::new();
        for row in rows {
            if let Ok(stat) = serde_json::from_str::<serde_json::Value>(&row?) {
                stats.push(stat);
            }
        }
        Ok(stats)
    })
}

pub(crate) fn query_subscription_daily_used(config: &SupplierConfig) -> Result<u32> {
    flush_subscription_usage_writer()?;
    with_subscription_usage_db(|connection| {
        let provider = subscription_provider_from_config(config);
        let channel_id = config.channel_id.trim();
        let channel_key = if channel_id.is_empty() {
            format!("name:{}", config.name.trim())
        } else {
            format!("id:{channel_id}")
        };
        let count: i64 = connection
            .query_row(
                "SELECT requests
                 FROM channel_usage_daily
                 WHERE provider = ?1 AND channel_key = ?2 AND day_index = ?3",
                params![
                    provider,
                    channel_key,
                    subscription_audit_day_index(now_unix())
                ],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0);
        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    })
}
