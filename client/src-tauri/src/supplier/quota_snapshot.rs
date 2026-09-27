pub(crate) fn subscription_quota_snapshot(config: &SupplierConfig) -> SubscriptionQuotaSnapshot {
    let states = match load_subscription_safety_states() {
        Ok(states) => states,
        Err(_) => {
            return subscription_quota_state_error_snapshot(config);
        }
    };
    let channel_id = subscription_safety_channel_id(config);
    quota_snapshot_for_state(config, states.get(&channel_id))
}

fn subscription_quota_state_error_snapshot(config: &SupplierConfig) -> SubscriptionQuotaSnapshot {
    SubscriptionQuotaSnapshot {
        status: "exhausted".to_string(),
        remaining_ratio: 0.0,
        daily_used: 0,
        daily_limit: config.subscription.daily_request_limit,
        checked_at_unix: now_unix(),
        quota_source: "state_error".to_string(),
        quota_window: "local_safety".to_string(),
        quota_used_percent: 100.0,
        quota_reset_at_unix: 0,
        quota_windows: Vec::new(),
    }
}

pub(crate) fn local_subscription_quota_snapshot(
    config: &SupplierConfig,
) -> SubscriptionQuotaSnapshot {
    let daily_used = subscription_daily_used(config).unwrap_or(0);
    local_subscription_quota_snapshot_with_usage(config, daily_used)
}

fn local_subscription_quota_snapshot_with_usage(
    config: &SupplierConfig,
    daily_used: u32,
) -> SubscriptionQuotaSnapshot {
    let daily_limit = config.subscription.daily_request_limit;
    let checked_at_unix = now_unix();
    if daily_limit == 0 {
        return SubscriptionQuotaSnapshot {
            status: "unknown".to_string(),
            remaining_ratio: 0.0,
            daily_used,
            daily_limit,
            checked_at_unix,
            quota_source: "unknown".to_string(),
            quota_window: String::new(),
            quota_used_percent: 0.0,
            quota_reset_at_unix: 0,
            quota_windows: Vec::new(),
        };
    }
    let remaining = daily_limit.saturating_sub(daily_used);
    let remaining_ratio = (remaining as f64 / daily_limit as f64).clamp(0.0, 1.0);
    let status = if remaining == 0 {
        "exhausted"
    } else if remaining_ratio <= 0.2 {
        "low"
    } else {
        "available"
    };
    SubscriptionQuotaSnapshot {
        status: status.to_string(),
        remaining_ratio,
        daily_used,
        daily_limit,
        checked_at_unix,
        quota_source: "local_daily".to_string(),
        quota_window: "daily".to_string(),
        quota_used_percent: if daily_limit > 0 {
            ((daily_used as f64 / daily_limit as f64) * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        },
        quota_reset_at_unix: 0,
        quota_windows: Vec::new(),
    }
}

pub(crate) fn quota_snapshot_for_state(
    config: &SupplierConfig,
    state: Option<&SubscriptionSafetyState>,
) -> SubscriptionQuotaSnapshot {
    let local = local_subscription_quota_snapshot(config);
    quota_snapshot_from_local(local, state)
}

fn quota_snapshot_from_local(
    local: SubscriptionQuotaSnapshot,
    state: Option<&SubscriptionSafetyState>,
) -> SubscriptionQuotaSnapshot {
    let Some(state) = state else {
        return local;
    };
    let Some(official) = official_quota_snapshot_from_state(state, local.checked_at_unix) else {
        return local;
    };
    if local.status == "unknown" {
        return official;
    }
    if local.status == "exhausted"
        || local.remaining_ratio <= official.remaining_ratio
        || (local.status == "low" && official.status == "available")
    {
        SubscriptionQuotaSnapshot {
            remaining_ratio: local.remaining_ratio.min(official.remaining_ratio),
            status: if local.status == "exhausted" || official.status == "exhausted" {
                "exhausted".to_string()
            } else if local.status == "low" || official.status == "low" {
                "low".to_string()
            } else {
                "available".to_string()
            },
            quota_windows: official.quota_windows.clone(),
            ..local
        }
    } else {
        SubscriptionQuotaSnapshot {
            daily_used: local.daily_used,
            daily_limit: local.daily_limit,
            checked_at_unix: local.checked_at_unix,
            remaining_ratio: local.remaining_ratio.min(official.remaining_ratio),
            ..official
        }
    }
}

fn official_quota_snapshot_from_state(
    state: &SubscriptionSafetyState,
    now: i64,
) -> Option<SubscriptionQuotaSnapshot> {
    let windows: Vec<QuotaWindow> = if state.quota_windows.is_empty() {
        if !official_quota_signal_is_fresh(state, now) {
            return None;
        }
        vec![QuotaWindow {
            source: state.quota_source.clone(),
            window: state.quota_window.clone(),
            remaining_ratio: (1.0 - state.quota_used_percent.clamp(0.0, 100.0) / 100.0)
                .clamp(0.0, 1.0),
            used_percent: state.quota_used_percent.max(0.0),
            reset_at_unix: state.quota_reset_at_unix,
            checked_at_unix: state.quota_checked_at_unix,
            model: None,
            token_type: None,
        }]
    } else {
        state
            .quota_windows
            .iter()
            .filter(|window| official_quota_window_is_fresh(window, now))
            .cloned()
            .collect()
    };
    let Some(summary) = channel_quota_summary(&windows) else {
        if windows.is_empty() { return None; }
        return Some(SubscriptionQuotaSnapshot {
            status: "unknown".to_string(), remaining_ratio: 1.0,
            daily_used: state.daily_used, daily_limit: state.daily_limit,
            checked_at_unix: windows.iter().map(|window| window.checked_at_unix).max().unwrap_or_default(),
            quota_source: "unknown".to_string(), quota_window: String::new(),
            quota_used_percent: 0.0, quota_reset_at_unix: 0, quota_windows: windows,
        });
    };
    let representative = summary.representative;
    Some(SubscriptionQuotaSnapshot {
        status: summary.status,
        remaining_ratio: summary.remaining_ratio,
        daily_used: state.daily_used,
        daily_limit: state.daily_limit,
        checked_at_unix: representative.checked_at_unix,
        quota_source: summary_quota_source(&representative.source),
        quota_window: representative.window,
        quota_used_percent: representative.used_percent,
        quota_reset_at_unix: representative.reset_at_unix,
        quota_windows: windows,
    })
}

fn official_quota_signal_is_fresh(state: &SubscriptionSafetyState, now: i64) -> bool {
    if !matches!(
        state.quota_source.as_str(),
        "official_header" | "official_error"
    ) {
        return false;
    }
    if state.quota_used_percent >= 100.0 && state.quota_reset_at_unix > 0 {
        return state.quota_reset_at_unix > now;
    }
    state.quota_checked_at_unix > now.saturating_sub(OFFICIAL_QUOTA_SIGNAL_MAX_AGE_SECONDS)
}

fn official_quota_window_is_fresh(window: &QuotaWindow, now: i64) -> bool {
    if !matches!(
        summary_quota_source(&window.source).as_str(),
        "official_header" | "official_error"
    ) {
        return false;
    }
    if window.used_percent >= 100.0 && window.reset_at_unix > 0 {
        return window.reset_at_unix > now;
    }
    window.checked_at_unix > now.saturating_sub(OFFICIAL_QUOTA_SIGNAL_MAX_AGE_SECONDS)
}

#[derive(Debug, Clone)]
pub(crate) struct QuotaReserveDecision {
    pub(crate) blocked: bool,
    pub(crate) window: String,
    pub(crate) remaining_percent: f64,
    pub(crate) reset_at_unix: i64,
}

fn quota_reserve_window_duration_minutes(window: &str) -> Option<u32> {
    let window = window.trim().to_ascii_lowercase();
    if window.contains("weekly") || window.contains("week") {
        return Some(7 * 24 * 60);
    }
    let bytes = window.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if !bytes[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        let value = bytes[start..index].iter().fold(0_u32, |value, digit| {
            value
                .saturating_mul(10)
                .saturating_add(u32::from(*digit - b'0'))
        });
        let minutes = match bytes.get(index).copied() {
            Some(b'h') => value.saturating_mul(60),
            Some(b'd') => value.saturating_mul(24 * 60),
            Some(b'w') => value.saturating_mul(7 * 24 * 60),
            _ => continue,
        };
        return Some(minutes);
    }
    None
}

fn quota_reserve_window_duration(window: &QuotaWindow) -> Option<u64> {
    if is_antigravity_quota_source(&window.source) {
        // Preserve Antigravity's existing model-bucket/reset-time selection.
        quota_reserve_window_duration_minutes(&window.window).map(|minutes| u64::from(minutes) * 60)
    } else {
        quota_window_period_seconds(&window.window)
    }
}

pub(crate) fn quota_reserve_decision(
    windows: &[QuotaWindow],
    reserve_percent: u32,
    now: i64,
) -> Option<QuotaReserveDecision> {
    if reserve_percent == 0 {
        return None;
    }
    let valid = windows
        .iter()
        .filter(|window| official_quota_window_is_fresh(window, now)
            && window.remaining_ratio.is_finite() && window.used_percent.is_finite()
            && quota_window_limits_supply(window))
        .collect::<Vec<_>>();
    let shortest_duration = valid
        .iter()
        .filter_map(|window| quota_reserve_window_duration(window))
        .min();
    let shortest_reset = if shortest_duration.is_none() {
        valid
            .iter()
            .filter_map(|window| (window.reset_at_unix > now).then_some(window.reset_at_unix))
            .min()
    } else {
        None
    };
    let representative = valid
        .into_iter()
        .filter(|window| {
            shortest_duration.map_or(true, |duration| {
                quota_reserve_window_duration(window) == Some(duration)
            })
        })
        .filter(|window| {
            shortest_reset.map_or(true, |reset_at_unix| window.reset_at_unix == reset_at_unix)
        })
        .min_by(|left, right| {
            quota_window_effective_ratio(left)
                .partial_cmp(&quota_window_effective_ratio(right))
                .unwrap_or(std::cmp::Ordering::Equal)
        })?;
    let remaining_percent = quota_window_effective_ratio(representative) * 100.0;
    Some(QuotaReserveDecision {
        blocked: remaining_percent < f64::from(reserve_percent.min(100)),
        window: representative.window.clone(),
        remaining_percent,
        reset_at_unix: representative.reset_at_unix,
    })
}

pub(crate) fn quota_reserve_decision_for_state(
    state: &SubscriptionSafetyState,
    reserve_percent: u32,
    now: i64,
) -> Option<QuotaReserveDecision> {
    if !state.quota_windows.is_empty() {
        return quota_reserve_decision(&state.quota_windows, reserve_percent, now);
    }
    if !official_quota_signal_is_fresh(state, now) {
        return None;
    }
    quota_reserve_decision(
        &[QuotaWindow {
            source: state.quota_source.clone(),
            window: state.quota_window.clone(),
            remaining_ratio: (1.0
                - state.quota_used_percent.clamp(0.0, 100.0) / 100.0)
                .clamp(0.0, 1.0),
            used_percent: state.quota_used_percent.max(0.0),
            reset_at_unix: state.quota_reset_at_unix,
            checked_at_unix: state.quota_checked_at_unix,
            model: None,
            token_type: None,
        }],
        reserve_percent,
        now,
    )
}

pub(crate) fn merge_quota_snapshot(
    local: SubscriptionQuotaSnapshot,
    upstream_status: &str,
    upstream_remaining_ratio: f64,
) -> SubscriptionQuotaSnapshot {
    if upstream_status.trim().is_empty() || upstream_status == "unknown" {
        return local;
    }
    let remaining_ratio = if upstream_remaining_ratio > 0.0 {
        local.remaining_ratio.min(upstream_remaining_ratio)
    } else {
        local.remaining_ratio
    };
    let status = if local.status == "exhausted" || upstream_status == "exhausted" {
        "exhausted"
    } else if local.status == "low" || upstream_status == "low" || remaining_ratio <= 0.2 {
        "low"
    } else {
        "available"
    };
    SubscriptionQuotaSnapshot {
        status: status.to_string(),
        remaining_ratio,
        quota_source: if upstream_status == "exhausted" || upstream_status == "low" {
            "unknown".to_string()
        } else {
            local.quota_source.clone()
        },
        quota_window: local.quota_window.clone(),
        quota_used_percent: local.quota_used_percent,
        quota_reset_at_unix: local.quota_reset_at_unix,
        quota_windows: local.quota_windows.clone(),
        ..local
    }
}

pub(crate) fn subscription_daily_used(config: &SupplierConfig) -> Result<u32> {
    query_subscription_daily_used(config)
}

pub(crate) fn local_timezone_offset_seconds() -> i64 {
    std::env::var("TZ")
        .ok()
        .and_then(|tz| parse_fixed_tz_offset(&tz))
        .unwrap_or(8 * 3600)
}

pub(crate) fn parse_fixed_tz_offset(value: &str) -> Option<i64> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("utc") || value == "Z" {
        return Some(0);
    }
    let offset = value
        .strip_prefix("UTC")
        .or_else(|| value.strip_prefix("GMT"))
        .unwrap_or(value);
    let sign = if offset.starts_with('-') { -1 } else { 1 };
    let offset = offset.trim_start_matches(['+', '-']);
    let (hours, minutes) = offset
        .split_once(':')
        .map(|(h, m)| (h, m))
        .unwrap_or((offset, "0"));
    let hours = hours.parse::<i64>().ok()?;
    let minutes = minutes.parse::<i64>().ok()?;
    Some(sign * (hours * 3600 + minutes * 60))
}

pub(crate) fn subscription_audit_log_path() -> PathBuf {
    subscription_audit_log_path_for_day(subscription_audit_day_index(now_unix()))
}
pub(crate) fn subscription_audit_log_path_for_day(day_index: i64) -> PathBuf {
    crate::client_data_root().join("audit").join(format!(
        "subscription-{}.jsonl",
        subscription_audit_date_string_for_day(day_index)
    ))
}

pub(crate) fn subscription_usage_log_path() -> PathBuf {
    crate::client_data_root()
        .join("state")
        .join("channel-usage.jsonl")
}

pub(crate) fn subscription_safety_state_path() -> PathBuf {
    crate::client_data_root()
        .join("state")
        .join("channel-safety.json")
}

fn subscription_audit_day_index(ts: i64) -> i64 {
    (ts + local_timezone_offset_seconds()).div_euclid(86_400)
}

fn subscription_audit_date_string_for_day(day_index: i64) -> String {
    let (year, month, day) = civil_from_days(day_index);
    format!("{year:04}-{month:02}-{day:02}")
}

fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = year + if month <= 2 { 1 } else { 0 };
    (year, month as u32, day as u32)
}
