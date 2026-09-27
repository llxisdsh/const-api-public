use crate::{
    commit_config_update, load_config_from_path, model::*, source_driver::channel_is_lan_share,
};
use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    net::{IpAddr, SocketAddr, UdpSocket},
    path::PathBuf,
    sync::{Mutex as StdMutex, OnceLock},
    time::Duration,
};
use tauri::State;

pub(crate) const LAN_SHARE_PATH_HEADER: &str = "x-const-lan-share-path";
pub(crate) const LAN_SHARE_INTERNAL_PATH_HEADER: &str = "x-const-internal-lan-share-path";
pub(crate) const LAN_SHARE_MAX_HOPS: usize = 8;
pub(crate) const LAN_SHARE_INPUT_WEIGHT: u64 = 1;
pub(crate) const LAN_SHARE_OUTPUT_WEIGHT: u64 = 4;
const LAN_SHARE_USAGE_RETENTION_WEEKS: i64 = 16;

static LAN_SHARE_DB: OnceLock<StdMutex<Option<(PathBuf, Connection)>>> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub(crate) struct LanShareUsageSnapshot {
    pub(crate) week_started_at_unix: i64,
    pub(crate) resets_at_unix: i64,
    pub(crate) input_tokens: u64,
    pub(crate) output_tokens: u64,
    pub(crate) used_tokens: u64,
    pub(crate) requests: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct LanShareMemberStatus {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) api_key: String,
    pub(crate) enabled: bool,
    pub(crate) weekly_token_limit: u64,
    pub(crate) remaining_tokens: u64,
    pub(crate) exhausted: bool,
    #[serde(flatten)]
    pub(crate) usage: LanShareUsageSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct LanShareConnectAddress {
    pub(crate) interface_name: String,
    pub(crate) ip: String,
    pub(crate) url: String,
    pub(crate) is_primary: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct LanShareHostStatus {
    pub(crate) allow_lan_access: bool,
    pub(crate) listen: String,
    pub(crate) connect_url: String,
    pub(crate) connect_addresses: Vec<LanShareConnectAddress>,
    pub(crate) shared_models: Vec<String>,
    pub(crate) available_models: Vec<String>,
    pub(crate) input_weight: u64,
    pub(crate) output_weight: u64,
    pub(crate) members: Vec<LanShareMemberStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct LanShareSelfStatus {
    pub(crate) name: String,
    pub(crate) models: Vec<String>,
    pub(crate) enabled: bool,
    pub(crate) weekly_token_limit: u64,
    pub(crate) remaining_tokens: u64,
    pub(crate) exhausted: bool,
    pub(crate) input_weight: u64,
    pub(crate) output_weight: u64,
    #[serde(flatten)]
    pub(crate) usage: LanShareUsageSnapshot,
}

fn lan_share_db_path() -> PathBuf {
    crate::client_data_root()
        .join("state")
        .join("lan-sharing.sqlite3")
}

fn initialize_lan_share_db(connection: &mut Connection) -> Result<()> {
    connection.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS lan_share_members (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            api_key TEXT NOT NULL UNIQUE,
            enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0, 1)),
            weekly_token_limit INTEGER NOT NULL CHECK(weekly_token_limit > 0),
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_lan_share_members_name
            ON lan_share_members(name COLLATE NOCASE);
        CREATE TABLE IF NOT EXISTS lan_share_usage (
            member_id TEXT NOT NULL REFERENCES lan_share_members(id) ON DELETE CASCADE,
            week_start INTEGER NOT NULL,
            input_tokens INTEGER NOT NULL DEFAULT 0,
            output_tokens INTEGER NOT NULL DEFAULT 0,
            weighted_tokens INTEGER NOT NULL DEFAULT 0,
            requests INTEGER NOT NULL DEFAULT 0,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY(member_id, week_start)
        );
        CREATE INDEX IF NOT EXISTS idx_lan_share_usage_week
            ON lan_share_usage(week_start);
        ",
    )?;
    Ok(())
}

fn with_lan_share_db<T>(operation: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
    let path = lan_share_db_path();
    let mut database = LAN_SHARE_DB
        .get_or_init(|| StdMutex::new(None))
        .lock()
        .map_err(|_| anyhow!("LAN share usage database lock poisoned"))?;
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
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        initialize_lan_share_db(&mut connection)?;
        *database = Some((path, connection));
    }
    let Some((_, connection)) = database.as_mut() else {
        return Err(anyhow!("LAN share usage database was not initialized"));
    };
    operation(connection)
}

fn local_utc_offset_seconds() -> i64 {
    time::UtcOffset::current_local_offset()
        .map(|offset| i64::from(offset.whole_seconds()))
        .unwrap_or(0)
}

fn local_utc_offset_seconds_at(unix: i64) -> i64 {
    time::OffsetDateTime::from_unix_timestamp(unix)
        .ok()
        .and_then(|datetime| time::UtcOffset::local_offset_at(datetime).ok())
        .map(|offset| i64::from(offset.whole_seconds()))
        .unwrap_or_else(local_utc_offset_seconds)
}

fn local_midnight_unix(local_day: i64, offset_hint: i64, offset_at: &impl Fn(i64) -> i64) -> i64 {
    let local_midnight = local_day.saturating_mul(86_400);
    let mut candidate = local_midnight.saturating_sub(offset_hint);
    for _ in 0..3 {
        let revised = local_midnight.saturating_sub(offset_at(candidate));
        if revised == candidate {
            break;
        }
        candidate = revised;
    }
    candidate
}

fn lan_share_week_window_with_offset(now_unix: i64, offset_at: impl Fn(i64) -> i64) -> (i64, i64) {
    let current_offset = offset_at(now_unix);
    let local_day = now_unix.saturating_add(current_offset).div_euclid(86_400);
    // Unix day zero was Thursday, three days after Monday.
    let days_since_monday = (local_day + 3).rem_euclid(7);
    let monday = local_day.saturating_sub(days_since_monday);
    let start = local_midnight_unix(monday, current_offset, &offset_at);
    let end = local_midnight_unix(monday.saturating_add(7), current_offset, &offset_at);
    (start, end)
}

pub(crate) fn lan_share_week_window(now_unix: i64) -> (i64, i64) {
    lan_share_week_window_with_offset(now_unix, local_utc_offset_seconds_at)
}

pub(crate) fn lan_share_usage_snapshot(member_id: &str) -> Result<LanShareUsageSnapshot> {
    let (week_start, resets_at) = lan_share_week_window(crate::now_unix());
    with_lan_share_db(|connection| {
        let row = connection
            .query_row(
                "SELECT input_tokens, output_tokens, weighted_tokens, requests
                 FROM lan_share_usage WHERE member_id = ?1 AND week_start = ?2",
                params![member_id, week_start],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()?;
        let (input, output, weighted, requests) = row.unwrap_or_default();
        Ok(LanShareUsageSnapshot {
            week_started_at_unix: week_start,
            resets_at_unix: resets_at,
            input_tokens: input.max(0) as u64,
            output_tokens: output.max(0) as u64,
            used_tokens: weighted.max(0) as u64,
            requests: requests.max(0) as u64,
        })
    })
}

pub(crate) fn record_lan_share_usage(
    member_id: &str,
    request_bytes: usize,
    reported_usage: crate::supplier::UsageTokenCounts,
    response_bytes: usize,
) -> Result<()> {
    let (input_tokens, output_tokens, weighted_tokens) =
        measured_lan_share_tokens(request_bytes, reported_usage, response_bytes);
    let now = crate::now_unix();
    let (week_start, _) = lan_share_week_window(now);
    with_lan_share_db(|connection| {
        connection.execute(
            "INSERT INTO lan_share_usage(
                member_id, week_start, input_tokens, output_tokens,
                weighted_tokens, requests, updated_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, 1, ?6)
             ON CONFLICT(member_id, week_start) DO UPDATE SET
                input_tokens = input_tokens + excluded.input_tokens,
                output_tokens = output_tokens + excluded.output_tokens,
                weighted_tokens = weighted_tokens + excluded.weighted_tokens,
                requests = requests + 1,
                updated_at = excluded.updated_at",
            params![
                member_id,
                week_start,
                i64::try_from(input_tokens).unwrap_or(i64::MAX),
                i64::try_from(output_tokens).unwrap_or(i64::MAX),
                i64::try_from(weighted_tokens).unwrap_or(i64::MAX),
                now,
            ],
        )?;
        connection.execute(
            "DELETE FROM lan_share_usage WHERE week_start < ?1",
            [week_start.saturating_sub(LAN_SHARE_USAGE_RETENTION_WEEKS * 7 * 86_400)],
        )?;
        Ok(())
    })
}

fn measured_lan_share_tokens(
    request_bytes: usize,
    reported_usage: crate::supplier::UsageTokenCounts,
    response_bytes: usize,
) -> (u64, u64, u64) {
    let input_tokens = reported_usage
        .input_tokens
        .unwrap_or(request_bytes.div_ceil(4) as u64);
    let output_tokens = reported_usage
        .output_tokens
        .unwrap_or(response_bytes.div_ceil(4) as u64);
    let weighted_tokens = input_tokens
        .saturating_mul(LAN_SHARE_INPUT_WEIGHT)
        .saturating_add(output_tokens.saturating_mul(LAN_SHARE_OUTPUT_WEIGHT));
    (input_tokens, output_tokens, weighted_tokens)
}

fn read_lan_share_members(connection: &Connection) -> Result<Vec<LanShareMember>> {
    let mut statement = connection.prepare(
        "SELECT id, name, api_key, enabled, weekly_token_limit
         FROM lan_share_members
         ORDER BY created_at DESC, rowid DESC",
    )?;
    let members = statement
        .query_map([], |row| {
            let weekly_token_limit = row.get::<_, i64>(4)?;
            Ok(LanShareMember {
                id: row.get(0)?,
                name: row.get(1)?,
                api_key: row.get(2)?,
                enabled: row.get::<_, i64>(3)? != 0,
                weekly_token_limit: weekly_token_limit.max(1) as u64,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(members)
}

fn update_lan_share_member_record(
    connection: &Connection,
    member_id: &str,
    name: &str,
    weekly_token_limit: i64,
    enabled: bool,
    updated_at: i64,
) -> Result<usize> {
    Ok(connection.execute(
        "UPDATE lan_share_members
         SET name = ?2, weekly_token_limit = ?3, enabled = ?4, updated_at = ?5
         WHERE id = ?1",
        params![
            member_id,
            name,
            weekly_token_limit,
            if enabled { 1_i64 } else { 0_i64 },
            updated_at,
        ],
    )?)
}

pub(crate) fn lan_share_members() -> Result<Vec<LanShareMember>> {
    with_lan_share_db(|connection| read_lan_share_members(connection))
}

fn member_status(member: &LanShareMember) -> Result<LanShareMemberStatus> {
    let usage = lan_share_usage_snapshot(&member.id)?;
    Ok(LanShareMemberStatus {
        id: member.id.clone(),
        name: member.name.clone(),
        api_key: member.api_key.clone(),
        enabled: member.enabled,
        weekly_token_limit: member.weekly_token_limit,
        remaining_tokens: member.weekly_token_limit.saturating_sub(usage.used_tokens),
        exhausted: usage.used_tokens >= member.weekly_token_limit,
        usage,
    })
}

fn configured_models(config: &ClientConfig) -> Vec<String> {
    let mut models = config
        .channels
        .iter()
        .filter(|channel| channel.enabled)
        .flat_map(|channel| channel.models.iter())
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty())
        .collect::<Vec<_>>();
    models.sort_by_key(|model| model.to_ascii_lowercase());
    models.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    models
}

fn primary_route_ip() -> Option<IpAddr> {
    UdpSocket::bind("0.0.0.0:0")
        .and_then(|socket| {
            socket.connect("8.8.8.8:80")?;
            socket.local_addr()
        })
        .ok()
        .map(|local| local.ip())
        .filter(|ip| usable_lan_ip(*ip))
}

fn usable_lan_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            !ip.is_loopback()
                && !ip.is_unspecified()
                && !ip.is_link_local()
                && !ip.is_multicast()
                && !ip.is_broadcast()
        }
        IpAddr::V6(ip) => {
            !ip.is_loopback()
                && !ip.is_unspecified()
                && !ip.is_unicast_link_local()
                && !ip.is_multicast()
        }
    }
}

fn lan_share_url(ip: IpAddr, port: u16) -> String {
    match ip {
        IpAddr::V4(ip) => format!("http://{ip}:{port}"),
        IpAddr::V6(ip) => format!("http://[{ip}]:{port}"),
    }
}

fn lan_connect_addresses_for(
    listen: &str,
    mut interfaces: Vec<(String, IpAddr)>,
    primary_ip: Option<IpAddr>,
) -> Vec<LanShareConnectAddress> {
    let Ok(address) = listen.parse::<SocketAddr>() else {
        return Vec::new();
    };
    if !address.ip().is_unspecified() {
        let ip = address.ip();
        return usable_lan_ip(ip)
            .then(|| LanShareConnectAddress {
                interface_name: String::new(),
                ip: ip.to_string(),
                url: lan_share_url(ip, address.port()),
                is_primary: true,
            })
            .into_iter()
            .collect();
    }

    interfaces.retain(|(_, ip)| {
        usable_lan_ip(*ip)
            && matches!(
                (address.ip(), ip),
                (IpAddr::V4(_), IpAddr::V4(_)) | (IpAddr::V6(_), IpAddr::V6(_))
            )
    });
    if let Some(primary_ip) = primary_ip.filter(|ip| {
        usable_lan_ip(*ip)
            && matches!(
                (address.ip(), ip),
                (IpAddr::V4(_), IpAddr::V4(_)) | (IpAddr::V6(_), IpAddr::V6(_))
            )
    }) {
        if !interfaces.iter().any(|(_, ip)| *ip == primary_ip) {
            interfaces.push((String::new(), primary_ip));
        }
    }
    interfaces.sort_by(|(left_name, left_ip), (right_name, right_ip)| {
        let left_primary = Some(*left_ip) == primary_ip;
        let right_primary = Some(*right_ip) == primary_ip;
        right_primary
            .cmp(&left_primary)
            .then_with(|| {
                left_name
                    .to_ascii_lowercase()
                    .cmp(&right_name.to_ascii_lowercase())
            })
            .then_with(|| left_ip.to_string().cmp(&right_ip.to_string()))
    });
    let mut seen = HashSet::new();
    interfaces
        .into_iter()
        .filter(|(_, ip)| seen.insert(*ip))
        .map(|(interface_name, ip)| LanShareConnectAddress {
            interface_name,
            ip: ip.to_string(),
            url: lan_share_url(ip, address.port()),
            is_primary: Some(ip) == primary_ip,
        })
        .collect()
}

fn lan_connect_addresses(listen: &str) -> Vec<LanShareConnectAddress> {
    let interfaces = if_addrs::get_if_addrs()
        .map(|interfaces| {
            interfaces
                .into_iter()
                .map(|interface| {
                    let ip = interface.ip();
                    (interface.name, ip)
                })
                .collect()
        })
        .unwrap_or_default();
    lan_connect_addresses_for(listen, interfaces, primary_route_ip())
}

fn preferred_connect_url(addresses: &[LanShareConnectAddress], preferred_ip: &str) -> String {
    addresses
        .iter()
        .find(|address| address.ip == preferred_ip.trim())
        .or_else(|| addresses.first())
        .map(|address| address.url.clone())
        .unwrap_or_default()
}

pub(crate) fn lan_share_host_status(config: &ClientConfig) -> Result<LanShareHostStatus> {
    let members = lan_share_members()?
        .iter()
        .map(member_status)
        .collect::<Result<Vec<_>>>()?;
    let connect_addresses = lan_connect_addresses(&config.listen);
    Ok(LanShareHostStatus {
        allow_lan_access: config.allow_lan_access,
        listen: config.listen.clone(),
        connect_url: preferred_connect_url(
            &connect_addresses,
            &config.lan_share.preferred_connect_ip,
        ),
        connect_addresses,
        shared_models: config.lan_share.shared_models.clone(),
        available_models: configured_models(config),
        input_weight: LAN_SHARE_INPUT_WEIGHT,
        output_weight: LAN_SHARE_OUTPUT_WEIGHT,
        members,
    })
}

pub(crate) fn lan_share_self_status(
    config: &ClientConfig,
    member: &LanShareMember,
) -> Result<LanShareSelfStatus> {
    let status = member_status(member)?;
    Ok(LanShareSelfStatus {
        name: member.name.clone(),
        models: config.lan_share.shared_models.clone(),
        enabled: member.enabled,
        weekly_token_limit: member.weekly_token_limit,
        remaining_tokens: status.remaining_tokens,
        exhausted: status.exhausted,
        input_weight: LAN_SHARE_INPUT_WEIGHT,
        output_weight: LAN_SHARE_OUTPUT_WEIGHT,
        usage: status.usage,
    })
}

// Call only after restricting the channel catalog to the literal allowlist.
// Resolving against the original catalog could expose another vendor's same-
// short-name model that the host did not share.
pub(crate) fn lan_share_model_is_allowed(restricted: &ClientConfig, requested: &str) -> bool {
    restricted
        .channels
        .iter()
        .any(|channel| crate::config::resolve_model_name(&channel.models, requested).is_some())
}

pub(crate) fn lan_share_restricted_config(config: &ClientConfig) -> ClientConfig {
    let allowed = config
        .lan_share
        .shared_models
        .iter()
        .map(|model| model.trim().to_ascii_lowercase())
        .filter(|model| !model.is_empty())
        .collect::<HashSet<_>>();
    let mut restricted = config.clone();
    restricted.account_device_api_key.clear();
    restricted.endpoints.clear();
    restricted.prefer_local_supply = true;
    restricted.allow_model_equivalence = false;
    restricted.channels = restricted
        .channels
        .into_iter()
        .filter_map(|mut channel| {
            channel
                .models
                .retain(|model| allowed.contains(&model.trim().to_ascii_lowercase()));
            if channel.models.is_empty() {
                return None;
            }
            if !allowed.contains(&channel.upstream_model.trim().to_ascii_lowercase()) {
                channel.upstream_model = channel.models[0].clone();
                channel.public_model = channel.models[0].clone();
            }
            Some(channel)
        })
        .collect();
    restricted
}

pub(crate) fn parse_lan_share_path(value: Option<&str>) -> Result<Vec<String>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(Vec::new());
    };
    value
        .split(',')
        .map(str::trim)
        .map(|node| {
            if node.is_empty()
                || node.len() > 128
                || !node
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
            {
                return Err(anyhow!("invalid LAN share path"));
            }
            Ok(node.to_string())
        })
        .collect()
}

pub(crate) fn lan_share_path_for_next_hop(
    current_path: Option<&str>,
    client_id: &str,
) -> Result<String> {
    let mut path = parse_lan_share_path(current_path)?;
    let client_id = client_id.trim();
    let client_id_parts = parse_lan_share_path(Some(client_id))?;
    let [client_id] = client_id_parts.as_slice() else {
        return Err(anyhow!("client_id is invalid for LAN share forwarding"));
    };
    if path.iter().any(|node| node == client_id) {
        return Err(anyhow!("LAN share loop detected"));
    }
    if path.len() >= LAN_SHARE_MAX_HOPS {
        return Err(anyhow!("LAN share hop limit reached"));
    }
    path.push(client_id.to_string());
    Ok(path.join(","))
}

fn new_member_key(owner_api_key: &str) -> String {
    loop {
        let key = crate::config::generate_scoped_api_key("lan");
        if key != owner_api_key {
            return key;
        }
    }
}

fn validate_weekly_token_limit(weekly_token_limit: u64) -> Result<i64> {
    if weekly_token_limit == 0 {
        return Err(anyhow!("weekly token limit must be greater than zero"));
    }
    i64::try_from(weekly_token_limit).map_err(|_| anyhow!("weekly token limit is too large"))
}

fn validate_member_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(anyhow!("member name is required"));
    }
    if name.chars().count() > 80 {
        return Err(anyhow!("member name is too long"));
    }
    Ok(name.to_string())
}

#[tauri::command]
pub(crate) fn get_lan_share_status(
    state: State<'_, AppState>,
) -> std::result::Result<LanShareHostStatus, String> {
    let config = load_config_from_path(&state.config_path).map_err(|error| error.to_string())?;
    lan_share_host_status(&config).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn set_lan_share_models(
    state: State<'_, AppState>,
    models: Vec<String>,
) -> std::result::Result<LanShareHostStatus, String> {
    let (config, ()) =
        commit_config_update(&state.config_path, &state.proxy_config, move |config| {
            let available = configured_models(config)
                .into_iter()
                .map(|model| model.to_ascii_lowercase())
                .collect::<HashSet<_>>();
            let mut selected = models
                .into_iter()
                .map(|model| model.trim().to_string())
                .filter(|model| {
                    !model.is_empty() && available.contains(&model.to_ascii_lowercase())
                })
                .collect::<Vec<_>>();
            selected.sort_by_key(|model| model.to_ascii_lowercase());
            selected.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
            config.lan_share.shared_models = selected;
            Ok(())
        })
        .map_err(|error| error.to_string())?;
    lan_share_host_status(&config).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn set_lan_share_connect_address(
    state: State<'_, AppState>,
    connect_url: String,
) -> std::result::Result<LanShareHostStatus, String> {
    let connect_url = connect_url.trim().to_string();
    let (config, ()) =
        commit_config_update(&state.config_path, &state.proxy_config, move |config| {
            let selected = lan_connect_addresses(&config.listen)
                .into_iter()
                .find(|address| address.url == connect_url)
                .ok_or_else(|| anyhow!("selected LAN share address is not available"))?;
            config.lan_share.preferred_connect_ip = selected.ip;
            Ok(())
        })
        .map_err(|error| error.to_string())?;
    lan_share_host_status(&config).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn create_lan_share_member(
    state: State<'_, AppState>,
    name: String,
    weekly_token_limit: u64,
) -> std::result::Result<LanShareHostStatus, String> {
    let name = validate_member_name(&name).map_err(|error| error.to_string())?;
    let weekly_token_limit =
        validate_weekly_token_limit(weekly_token_limit).map_err(|error| error.to_string())?;
    let config = load_config_from_path(&state.config_path).map_err(|error| error.to_string())?;
    let member_id = format!("lan-member-{:032x}", rand::random::<u128>());
    let member_key = new_member_key(&config.api_key);
    let now = crate::now_unix();
    with_lan_share_db(|connection| {
        let member_count =
            connection.query_row("SELECT COUNT(*) FROM lan_share_members", [], |row| {
                row.get::<_, i64>(0)
            })?;
        if member_count >= 128 {
            return Err(anyhow!("LAN share supports at most 128 members"));
        }
        connection.execute(
            "INSERT INTO lan_share_members(
                id, name, api_key, enabled, weekly_token_limit, created_at, updated_at
             ) VALUES(?1, ?2, ?3, 1, ?4, ?5, ?5)",
            params![member_id, name, member_key, weekly_token_limit, now],
        )?;
        Ok(())
    })
    .map_err(|error| error.to_string())?;
    lan_share_host_status(&config).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn update_lan_share_member(
    state: State<'_, AppState>,
    member_id: String,
    name: String,
    weekly_token_limit: u64,
    enabled: bool,
) -> std::result::Result<LanShareHostStatus, String> {
    let name = validate_member_name(&name).map_err(|error| error.to_string())?;
    let member_id = member_id.trim().to_string();
    let weekly_token_limit =
        validate_weekly_token_limit(weekly_token_limit).map_err(|error| error.to_string())?;
    let config = load_config_from_path(&state.config_path).map_err(|error| error.to_string())?;
    let changed = with_lan_share_db(|connection| {
        update_lan_share_member_record(
            connection,
            &member_id,
            &name,
            weekly_token_limit,
            enabled,
            crate::now_unix(),
        )
    })
    .map_err(|error| error.to_string())?;
    if changed == 0 {
        return Err("LAN share member was not found".to_string());
    }
    lan_share_host_status(&config).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn reset_lan_share_member(
    state: State<'_, AppState>,
    member_id: String,
) -> std::result::Result<LanShareHostStatus, String> {
    let config = load_config_from_path(&state.config_path).map_err(|error| error.to_string())?;
    let member_exists = with_lan_share_db(|connection| {
        let member_exists = connection
            .query_row(
                "SELECT 1 FROM lan_share_members WHERE id = ?1",
                [&member_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if member_exists {
            let (week_start, _) = lan_share_week_window(crate::now_unix());
            connection.execute(
                "DELETE FROM lan_share_usage WHERE member_id = ?1 AND week_start = ?2",
                params![member_id, week_start],
            )?;
        }
        Ok(member_exists)
    })
    .map_err(|error| error.to_string())?;
    if !member_exists {
        return Err("LAN share member was not found".to_string());
    }
    lan_share_host_status(&config).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn regenerate_lan_share_member_key(
    state: State<'_, AppState>,
    member_id: String,
) -> std::result::Result<LanShareHostStatus, String> {
    let member_id = member_id.trim().to_string();
    let config = load_config_from_path(&state.config_path).map_err(|error| error.to_string())?;
    let member_key = new_member_key(&config.api_key);
    let changed = with_lan_share_db(|connection| {
        Ok(connection.execute(
            "UPDATE lan_share_members SET api_key = ?2, updated_at = ?3 WHERE id = ?1",
            params![member_id, member_key, crate::now_unix()],
        )?)
    })
    .map_err(|error| error.to_string())?;
    if changed == 0 {
        return Err("LAN share member was not found".to_string());
    }
    lan_share_host_status(&config).map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn delete_lan_share_member(
    state: State<'_, AppState>,
    member_id: String,
) -> std::result::Result<LanShareHostStatus, String> {
    let member_id = member_id.trim().to_string();
    let config = load_config_from_path(&state.config_path).map_err(|error| error.to_string())?;
    let removed = with_lan_share_db(|connection| {
        Ok(connection.execute("DELETE FROM lan_share_members WHERE id = ?1", [&member_id])? > 0)
    })
    .map_err(|error| error.to_string())?;
    if !removed {
        return Err("LAN share member was not found".to_string());
    }
    lan_share_host_status(&config).map_err(|error| error.to_string())
}

fn lan_share_status_url(channel: &ChannelConfig) -> Result<reqwest::Url> {
    let base_url = channel
        .surface_bindings
        .iter()
        .find(|surface| surface.surface == crate::surface::ApiSurface::OpenAi)
        .or_else(|| channel.surface_bindings.first())
        .map(|surface| surface.base_url.as_str())
        .unwrap_or_default();
    let mut url = reqwest::Url::parse(base_url).context("invalid LAN share address")?;
    url.set_path("/api/lan-share/me");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

#[tauri::command]
pub(crate) async fn get_lan_share_channel_status(
    state: State<'_, AppState>,
    channel_id: String,
) -> std::result::Result<LanShareSelfStatus, String> {
    let config = load_config_from_path(&state.config_path).map_err(|error| error.to_string())?;
    let channel = config
        .channels
        .iter()
        .find(|channel| channel.id == channel_id && channel_is_lan_share(channel))
        .ok_or_else(|| "LAN share channel was not found".to_string())?;
    let url = lan_share_status_url(channel).map_err(|error| error.to_string())?;
    let request = crate::long_http_client()
        .get(url)
        .bearer_auth(channel.v2.credential_ref.trim())
        .header("accept", "application/json");
    let response = tokio::time::timeout(Duration::from_secs(5), request.send())
        .await
        .map_err(|_| "LAN share status request timed out".to_string())?
        .map_err(|error| error.to_string())?;
    let status = response.status();
    let body = response.text().await.map_err(|error| error.to_string())?;
    if !status.is_success() {
        return Err(format!("LAN share returned HTTP {status}: {body}"));
    }
    serde_json::from_str(&body).map_err(|error| format!("invalid LAN share status: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_keys_match_the_local_key_strength_without_the_old_extra_length() {
        let key = new_member_key("sk-api-00000000000000000000000000000000");

        assert_eq!(key.len(), 39);
        assert!(key.starts_with("sk-lan-"));
        assert!(key[7..].bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn week_window_starts_on_monday_and_spans_seven_days() {
        let now = 1_704_067_200;
        let (start, end) = lan_share_week_window(now);
        assert!(start <= now && now < end);
        assert!((7 * 86_400 - 7_200..=7 * 86_400 + 7_200).contains(&(end - start)));
    }

    #[test]
    fn week_window_keeps_local_midnights_across_a_dst_change() {
        let monday_day = 998_i64;
        let transition = (monday_day + 3) * 86_400 + 3_600;
        let now = (monday_day + 4) * 86_400 + 12 * 3_600 - 7_200;
        let offset_at = |unix: i64| if unix < transition { 3_600 } else { 7_200 };

        let (start, end) = lan_share_week_window_with_offset(now, offset_at);

        assert_eq!(start, monday_day * 86_400 - 3_600);
        assert_eq!(end, (monday_day + 7) * 86_400 - 7_200);
        assert_eq!(end - start, 7 * 86_400 - 3_600);
    }

    #[test]
    fn path_allows_layers_but_rejects_a_repeated_node() {
        assert_eq!(
            lan_share_path_for_next_hop(Some("node-a,node-b"), "node-c").unwrap(),
            "node-a,node-b,node-c"
        );
        assert!(lan_share_path_for_next_hop(Some("node-a,node-b"), "node-a").is_err());
    }

    #[test]
    fn path_rejects_more_than_eight_forwarding_nodes() {
        assert!(lan_share_path_for_next_hop(Some("a,b,c,d,e,f,g,h"), "i").is_err());
    }

    #[test]
    fn restricted_config_keeps_only_explicit_shared_models() {
        let mut config = crate::default_config();
        config.channels[0].models = vec!["cheap".to_string(), "private".to_string()];
        let mut downstream = config.channels[0].clone();
        downstream.id = "downstream-team".to_string();
        downstream.set_source_driver(crate::source_driver::SourceDriverId::LanShare);
        downstream.models = vec!["cheap".to_string()];
        config.channels.push(downstream);
        config.lan_share.shared_models = vec!["cheap".to_string()];
        let restricted = lan_share_restricted_config(&config);
        assert_eq!(restricted.channels[0].models, ["cheap"]);
        assert_eq!(restricted.channels.len(), 2);
        assert!(restricted.channels.iter().any(channel_is_lan_share));
        assert!(restricted.endpoints.is_empty());
        assert!(!restricted.allow_model_equivalence);
    }

    #[test]
    fn shared_short_names_only_resolve_inside_the_literal_allowlist() {
        let mut config = crate::default_config();
        config.channels[0].models = vec![
            "Private/Mixed:free".into(),
            "Shared/Mixed:free".into(),
            "Shared/Paid".into(),
        ];
        config.lan_share.shared_models = vec!["Shared/Mixed:free".into()];
        let restricted = lan_share_restricted_config(&config);
        assert_eq!(restricted.channels[0].models, ["Shared/Mixed:free"]);
        for name in ["mixed:free", "shared/mixed:free"] {
            assert!(lan_share_model_is_allowed(&restricted, name));
        }
        for name in ["private/mixed:free", "paid", "mixed", "mixed:extended"] {
            assert!(!lan_share_model_is_allowed(&restricted, name), "{name}");
        }
        assert_eq!(
            crate::proxy::channel_upstream_model_for_request(
                &restricted.channels[0],
                Some("mixed:free")
            ),
            "Shared/Mixed:free"
        );
    }

    #[test]
    fn path_rejects_an_invalid_local_client_id() {
        assert!(lan_share_path_for_next_hop(None, "node-a,node-b").is_err());
    }

    #[test]
    fn metering_prefers_reported_tokens_and_applies_the_output_weight() {
        let response = br#"{"usage":{"prompt_tokens":10,"completion_tokens":5}}"#;
        let reported =
            crate::supplier::extract_usage_token_counts(&String::from_utf8_lossy(response));
        assert_eq!(
            measured_lan_share_tokens(4_000, reported, 8_000),
            (10, 5, 30)
        );
    }

    #[test]
    fn metering_falls_back_to_rounded_byte_estimates() {
        assert_eq!(
            measured_lan_share_tokens(9, crate::supplier::UsageTokenCounts::default(), 17,),
            (3, 5, 23)
        );
    }

    #[test]
    fn metering_does_not_estimate_a_token_field_explicitly_reported_as_zero() {
        let reported = crate::supplier::extract_usage_token_counts(
            r#"{"usage":{"prompt_tokens":10,"completion_tokens":0}}"#,
        );

        assert_eq!(
            measured_lan_share_tokens(4_000, reported, 100 * 1024),
            (10, 0, 10)
        );
    }

    #[test]
    fn updating_one_member_changes_only_that_sqlite_row() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        initialize_lan_share_db(&mut connection).unwrap();
        for (id, name, key) in [
            ("member-a", "Alice", "cst-lan-a"),
            ("member-b", "Bob", "cst-lan-b"),
        ] {
            connection
                .execute(
                    "INSERT INTO lan_share_members(
                        id, name, api_key, enabled, weekly_token_limit, created_at, updated_at
                     ) VALUES(?1, ?2, ?3, 1, 1000, 1, 1)",
                    params![id, name, key],
                )
                .unwrap();
        }

        assert_eq!(
            update_lan_share_member_record(&connection, "member-a", "Alice desk", 2_000, false, 2,)
                .unwrap(),
            1,
        );
        let members = read_lan_share_members(&connection).unwrap();
        let alice = members
            .iter()
            .find(|member| member.id == "member-a")
            .unwrap();
        let bob = members
            .iter()
            .find(|member| member.id == "member-b")
            .unwrap();
        assert_eq!(alice.name, "Alice desk");
        assert_eq!(alice.weekly_token_limit, 2_000);
        assert!(!alice.enabled);
        assert_eq!(bob.name, "Bob");
        assert_eq!(bob.weekly_token_limit, 1_000);
        assert!(bob.enabled);
    }

    #[test]
    fn members_are_read_newest_first_even_when_created_in_the_same_second() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        initialize_lan_share_db(&mut connection).unwrap();
        for (id, name, created_at) in [
            ("member-old", "Zulu", 10_i64),
            ("member-new-a", "Alpha", 20_i64),
            ("member-new-b", "Beta", 20_i64),
        ] {
            connection
                .execute(
                    "INSERT INTO lan_share_members(
                        id, name, api_key, enabled, weekly_token_limit, created_at, updated_at
                     ) VALUES(?1, ?2, ?3, 1, 1000, ?4, ?4)",
                    params![id, name, format!("cst-lan-{id}"), created_at],
                )
                .unwrap();
        }

        let ids = read_lan_share_members(&connection)
            .unwrap()
            .into_iter()
            .map(|member| member.id)
            .collect::<Vec<_>>();
        assert_eq!(ids, ["member-new-b", "member-new-a", "member-old"]);
    }

    #[test]
    fn connect_addresses_include_every_usable_adapter_and_prioritize_the_default_route() {
        let ethernet = "192.168.1.20".parse().unwrap();
        let overlay = "100.64.0.8".parse().unwrap();
        let addresses = lan_connect_addresses_for(
            "0.0.0.0:38788",
            vec![
                ("Ethernet".to_string(), ethernet),
                ("Overlay".to_string(), overlay),
                ("Loopback".to_string(), "127.0.0.1".parse().unwrap()),
                ("Link local".to_string(), "169.254.1.2".parse().unwrap()),
            ],
            Some(overlay),
        );

        assert_eq!(addresses.len(), 2);
        assert_eq!(addresses[0].interface_name, "Overlay");
        assert_eq!(addresses[0].url, "http://100.64.0.8:38788");
        assert!(addresses[0].is_primary);
        assert_eq!(addresses[1].url, "http://192.168.1.20:38788");
    }

    #[test]
    fn saved_adapter_ip_overrides_the_default_route_for_member_invites() {
        let ethernet = "192.168.1.20".parse().unwrap();
        let overlay = "100.64.0.8".parse().unwrap();
        let addresses = lan_connect_addresses_for(
            "0.0.0.0:38788",
            vec![
                ("Ethernet".to_string(), ethernet),
                ("Overlay".to_string(), overlay),
            ],
            Some(ethernet),
        );

        assert_eq!(
            preferred_connect_url(&addresses, "100.64.0.8"),
            "http://100.64.0.8:38788"
        );
        assert_eq!(
            preferred_connect_url(&addresses, "10.0.0.99"),
            "http://192.168.1.20:38788"
        );
    }

    #[test]
    fn explicit_listen_address_does_not_advertise_other_adapters() {
        let addresses = lan_connect_addresses_for(
            "10.0.0.5:38788",
            vec![("Ethernet".to_string(), "192.168.1.20".parse().unwrap())],
            Some("192.168.1.20".parse().unwrap()),
        );

        assert_eq!(addresses.len(), 1);
        assert_eq!(addresses[0].url, "http://10.0.0.5:38788");
        assert!(addresses[0].is_primary);
    }
}
