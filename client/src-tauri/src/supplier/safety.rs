#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SubscriptionRequestCapacity {
    counts_concurrency: bool,
    starts_logical_request: bool,
}

impl SubscriptionRequestCapacity {
    pub(crate) fn from_flags(counts_concurrency: bool, starts_logical_request: bool) -> Self {
        Self {
            counts_concurrency,
            starts_logical_request,
        }
    }

    fn for_operation(operation: crate::surface::ApiOperation) -> Self {
        let uses_request_capacity = !matches!(
            operation,
            crate::surface::ApiOperation::CountTokens
                | crate::surface::ApiOperation::ResponsesInputTokens
        );
        Self {
            counts_concurrency: uses_request_capacity,
            starts_logical_request: uses_request_capacity,
        }
    }

    pub(crate) fn for_path(path: &str) -> Self {
        let path = path.split('?').next().unwrap_or(path);
        crate::surface::resolve_api_route("POST", path, false)
            .map(|route| Self::for_operation(route.operation))
            .unwrap_or(Self {
                counts_concurrency: true,
                starts_logical_request: true,
            })
    }

    pub(crate) fn from_supplier_payload(
        payload: &serde_json::Map<String, serde_json::Value>,
        operation: crate::surface::ApiOperation,
    ) -> Self {
        let mut capacity = Self::for_operation(operation);
        if let Some(value) = payload
            .get(crate::surface_wire::COUNTS_CONCURRENCY_FIELD)
            .and_then(|value| value.as_bool())
        {
            capacity.counts_concurrency = value;
        }
        if let Some(value) = payload
            .get(crate::surface_wire::STARTS_LOGICAL_REQUEST_FIELD)
            .or_else(|| payload.get(crate::surface_wire::LEGACY_COUNTS_RPM_FIELD))
            .and_then(|value| value.as_bool())
        {
            capacity.starts_logical_request = value;
        }
        capacity
    }
}

#[derive(Debug)]
pub(crate) struct SubscriptionSafetyGuard {
    channel_id: String,
}

impl Drop for SubscriptionSafetyGuard {
    fn drop(&mut self) {
        if self.channel_id.is_empty() {
            return;
        }
        if let Ok(mut guard) = subscription_concurrency_map().lock() {
            let value = guard.entry(self.channel_id.clone()).or_insert(0);
            *value = value.saturating_sub(1);
        }
        // Concurrency is process-local and is hydrated from the in-memory counter whenever the
        // state is read. Persisting every guard drop caused an unnecessary full state-file
        // read/write on the request hot path and could only store a transient value.
    }
}

#[cfg(test)]
pub(crate) fn subscription_safety_preflight(
    config: &SupplierConfig,
) -> std::result::Result<(), serde_json::Map<String, serde_json::Value>> {
    if config.kind.trim() != "subscription_adapter" {
        return Ok(());
    }
    let state = subscription_safety_admission_state_for_channel(config).map_err(|err| {
        subscription_structured_error_response(
            429,
            "rate_limited",
            &format!("subscription safety state unavailable: {err}"),
            5,
            now_unix() + 5,
        )
    })?;
    subscription_safety_validate_state(&state)?;
    subscription_quota_reserve_validate(config, &state)
}

pub(crate) fn subscription_safety_enter(
    config: &SupplierConfig,
    request_body: &str,
) -> std::result::Result<SubscriptionSafetyGuard, serde_json::Map<String, serde_json::Value>> {
    subscription_safety_enter_with_capacity(
        config,
        request_body,
        SubscriptionRequestCapacity::from_flags(true, true),
    )
}

pub(crate) fn subscription_safety_enter_with_capacity(
    config: &SupplierConfig,
    _request_body: &str,
    capacity: SubscriptionRequestCapacity,
) -> std::result::Result<SubscriptionSafetyGuard, serde_json::Map<String, serde_json::Value>> {
    if config.kind.trim() != "subscription_adapter" {
        return Ok(SubscriptionSafetyGuard {
            channel_id: String::new(),
        });
    }
    #[cfg(test)]
    if !subscription_safety_enabled_for_tests(config) {
        return Ok(SubscriptionSafetyGuard {
            channel_id: String::new(),
        });
    }
    let channel_id = subscription_safety_channel_id(config);
    let state = subscription_safety_admission_state_for_channel(config).map_err(|err| {
        subscription_structured_error_response(
            429,
            "rate_limited",
            &format!("subscription safety state unavailable: {err}"),
            5,
            now_unix() + 5,
        )
    })?;
    subscription_safety_validate_state(&state)?;
    if capacity.starts_logical_request {
        subscription_quota_reserve_validate(config, &state)?;
    }

    // Token counting bypasses request capacity, while a routed retry can keep
    // concurrency protection without re-checking the same logical request's reserve.
    if !capacity.counts_concurrency && !capacity.starts_logical_request {
        return Ok(SubscriptionSafetyGuard {
            channel_id: String::new(),
        });
    }

    let now = now_unix();
    let max_concurrency = config.max_concurrency();
    let guard_channel_id = if capacity.counts_concurrency {
        if !try_increment_subscription_concurrency(&channel_id, max_concurrency) {
            return Err(subscription_structured_error_response(
                429,
                "concurrency_full",
                "subscription channel concurrency limit reached locally",
                1,
                now + 1,
            ));
        }
        channel_id.clone()
    } else {
        String::new()
    };
    let request_guard = SubscriptionSafetyGuard {
        channel_id: guard_channel_id,
    };

    if capacity.starts_logical_request && config.subscription.rpm_limit > 0 {
        if let Err(retry_after) = reserve_subscription_rpm(
            &channel_id,
            &state.rpm_window_unix,
            config.subscription.rpm_limit,
            now,
        ) {
            return Err(subscription_structured_error_response(
                429,
                "rate_limited",
                "subscription channel RPM limit reached locally",
                retry_after,
                now + retry_after,
            ));
        }
    }

    // Admission counters are process state. Persisting them before contacting the
    // provider forced a read/serialize/fsync/rename cycle into every request's
    // first-byte path. The next result/health update persists the current RPM
    // window together with the safety state; durable risk and quota transitions
    // remain fail-closed and synchronous.
    Ok(request_guard)
}

pub(crate) fn subscription_safety_validate_state(
    state: &SubscriptionSafetyState,
) -> std::result::Result<(), serde_json::Map<String, serde_json::Value>> {
    if state.state == "risk_blocked" || state.state == "auth_error" {
        return Err(subscription_structured_error_response(
            403,
            if state.last_error_kind.is_empty() {
                "risk_challenge"
            } else {
                &state.last_error_kind
            },
            "subscription channel requires manual recovery",
            0,
            0,
        ));
    }
    Ok(())
}

fn subscription_quota_reserve_validate(
    config: &SupplierConfig,
    state: &SubscriptionSafetyState,
) -> std::result::Result<(), serde_json::Map<String, serde_json::Value>> {
    if let Some(decision) = quota_reserve_decision_for_state(
        state,
        config.quota_reserve_percent(),
        now_unix(),
    ) {
        if decision.blocked {
            let now = now_unix();
            let retry_after = if decision.reset_at_unix > now {
                decision.reset_at_unix - now
            } else {
                60
            };
            let mut payload = subscription_structured_error_response(
                429,
                "quota_reserved",
                "subscription channel quota reserve reached locally",
                retry_after,
                decision.reset_at_unix.max(now + retry_after),
            );
            payload.insert(
                "quota_reserve_percent".to_string(),
                serde_json::json!(config.quota_reserve_percent()),
            );
            payload.insert(
                "quota_remaining_percent".to_string(),
                serde_json::json!(decision.remaining_percent),
            );
            payload.insert(
                "quota_window".to_string(),
                serde_json::json!(decision.window),
            );
            payload.insert(
                "failure_scope".to_string(),
                serde_json::Value::String("channel".to_string()),
            );
            return Err(payload);
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn subscription_safety_record_result(
    config: &SupplierConfig,
    response_payload: &serde_json::Map<String, serde_json::Value>,
    elapsed: Duration,
    request_body: &str,
) -> Result<()> {
    subscription_safety_record_result_for_path(config, response_payload, elapsed, request_body, "")
}

pub(crate) fn subscription_safety_record_result_for_path(
    config: &SupplierConfig,
    response_payload: &serde_json::Map<String, serde_json::Value>,
    elapsed: Duration,
    request_body: &str,
    path: &str,
) -> Result<()> {
    if config.kind.trim() != "subscription_adapter" || is_local_subscription_block(response_payload)
    {
        return Ok(());
    }
    #[cfg(test)]
    if !subscription_safety_enabled_for_tests(config) {
        return Ok(());
    }
    let wire = crate::surface_wire::SurfaceEnvelope::from_payload(response_payload)?;
    let now = now_unix();
    let mut state = subscription_safety_state_for_channel(config)?;
    let status = wire.status.unwrap_or_default();
    let error_kind = response_payload
        .get("error_kind")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let requested_model = requested_model_from_body_or_path(request_body, path);
    let request_model = response_payload
        .get("upstream_model")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(normalize_model_name)
        .unwrap_or_else(|| {
            normalize_model_name(&supplier_upstream_model_for_request(
                config,
                requested_model.as_deref(),
            ))
        });
    let declared_failure_model = response_payload
        .get("failure_model")
        .and_then(serde_json::Value::as_str)
        .map(normalize_model_name)
        .unwrap_or_default();
    let declared_failure_scope = response_payload
        .get("failure_scope")
        .and_then(serde_json::Value::as_str)
        .map(str::trim);
    let declared_model_scoped =
        declared_failure_scope.is_some_and(|scope| scope.eq_ignore_ascii_case("model"));
    let explicitly_model_scoped = declared_model_scoped
        && !request_model.is_empty()
        && declared_failure_model == request_model;
    let explicitly_request_scoped = declared_failure_scope.is_some_and(|scope| {
        scope.eq_ignore_ascii_case("request") || scope.eq_ignore_ascii_case("operation")
    });
    let invalid_model_scope = declared_model_scoped && !explicitly_model_scoped;
    let model_scoped_failure = explicitly_model_scoped
        || (status == 429
            && matches!(error_kind, "quota_exhausted" | "rate_limited")
            && !request_model.is_empty()
            && state.quota_windows.iter().any(|window| {
                quota_window_limits_supply(window) && window
                    .model
                    .as_deref()
                    .is_some_and(|model| normalize_model_name(model) == request_model)
            }));
    let route_scoped_failure =
        model_scoped_failure || explicitly_request_scoped || invalid_model_scope;
    let ok = (200..300).contains(&status);
    let channel_state_was_active = state.state == "active";
    state.last_status = status;
    state.daily_limit = config.subscription.daily_request_limit;
    state.rpm_limit = config.subscription.rpm_limit;
    state.max_concurrency = config.max_concurrency();
    state.current_concurrency =
        subscription_current_concurrency(&subscription_safety_channel_id(config));
    let latency_sample_ms = subscription_routing_latency_sample_ms(response_payload)
        .unwrap_or_else(|| elapsed.as_millis().min(u128::from(u64::MAX)) as u64);
    if !route_scoped_failure {
        state.latency_ewma_ms = ewma(state.latency_ewma_ms, latency_sample_ms as f64, 0.35);
    }
    state.estimated_input_tokens = estimate_subscription_tokens(request_body);
    state.estimated_output_tokens = wire
        .body
        .utf8()
        .map(estimate_subscription_tokens)
        .unwrap_or_default();
    let official_quota_windows = official_quota_windows_from_response_payload(response_payload);
    if ok {
        // A real successful request is the strongest channel-health evidence.
        // It reopens the channel; model-route failures remain in their separate
        // runtime map and are cleared only by success on that exact route.
        state.state = "active".to_string();
        state.last_error_kind.clear();
        state.consecutive_401 = 0;
        state.consecutive_403 = 0;
        state.consecutive_429 = 0;
        state.consecutive_5xx = 0;
        state.success_ewma = ewma(state.success_ewma, 1.0, 0.25).clamp(0.0, 1.0);
        state.cooldown_until_unix = 0;
    } else {
        let observed_error_kind = if error_kind.is_empty() {
            "unknown_upstream_error".to_string()
        } else {
            error_kind.to_string()
        };
        if route_scoped_failure {
            // Model/request diagnostics cannot recover or otherwise rewrite a
            // pre-existing channel fault. When the channel is active, retain a
            // scoped diagnostic for Admin without changing health.
            if channel_state_was_active {
                state.last_error_kind = format!(
                    "{}_{}",
                    if model_scoped_failure {
                        "model"
                    } else {
                        "request"
                    },
                    observed_error_kind
                );
                state.last_error_at_unix = now;
            }
        } else {
            state.last_error_kind = observed_error_kind;
            state.last_error_at_unix = now;
            state.success_ewma = ewma(state.success_ewma, 0.0, 0.25).clamp(0.0, 1.0);
            if status == 401 {
                state.consecutive_401 = state.consecutive_401.saturating_add(1);
            } else if status == 403 {
                state.consecutive_403 = state.consecutive_403.saturating_add(1);
            } else if status == 429 {
                state.consecutive_429 = state.consecutive_429.saturating_add(1);
            } else if status >= 500 {
                state.consecutive_5xx = state.consecutive_5xx.saturating_add(1);
            }
            let retry_after = response_payload
                .get("retry_after_seconds")
                .and_then(|value| value.as_i64())
                .unwrap_or_default();
            let cooldown_until = response_payload
                .get("cooldown_until_unix")
                .and_then(|value| value.as_i64())
                .unwrap_or_default()
                .max(if retry_after > 0 {
                    now + retry_after
                } else {
                    0
                });
            match state.last_error_kind.as_str() {
                "risk_challenge" => {
                    state.state = "risk_blocked".to_string();
                    state.cooldown_until_unix = 0;
                }
                "auth_error" | "auth_revoked" => {
                    state.state = "auth_error".to_string();
                    state.cooldown_until_unix = 0;
                }
                "quota_exhausted" => {
                    state.state = "quota_exhausted".to_string();
                    state.cooldown_until_unix = if cooldown_until > 0 {
                        cooldown_until
                    } else {
                        now + 30 * 60
                    };
                }
                "rate_limited" => {
                    state.state = "cooldown".to_string();
                    state.cooldown_until_unix = cooldown_until.max(now + 60);
                }
                "permission_denied" => {
                    if state.consecutive_403 >= 2 {
                        state.state = "risk_blocked".to_string();
                        state.cooldown_until_unix = 0;
                    } else {
                        state.state = "cooldown".to_string();
                        state.cooldown_until_unix = cooldown_until.max(now + 5 * 60);
                    }
                }
                "provider_overload" | "network_error" | "timeout" => {
                    state.state = "cooldown".to_string();
                    state.cooldown_until_unix = cooldown_until.max(now + 60);
                }
                _ => {
                    state.state = "cooldown".to_string();
                    state.cooldown_until_unix = cooldown_until.max(now + 2 * 60);
                }
            }
        }
    }
    if !official_quota_windows.is_empty() {
        apply_official_quota_windows_to_state(&mut state, &official_quota_windows);
    }
    let quota = quota_snapshot_from_local(
        local_subscription_quota_snapshot_with_usage(config, state.daily_used),
        Some(&state),
    );
    state.remaining_ratio = quota.remaining_ratio;
    save_subscription_safety_state(state)
}

pub(crate) fn subscription_safety_state_for_channel(
    config: &SupplierConfig,
) -> Result<SubscriptionSafetyState> {
    let channel_id = subscription_safety_channel_id(config);
    let mut state = cached_subscription_safety_state(&channel_id)?
        .unwrap_or_else(|| default_subscription_safety_state(config));
    hydrate_subscription_safety_state(config, &mut state);
    state.rpm_window_unix =
        runtime_subscription_rpm_window(&channel_id, &state.rpm_window_unix, now_unix());
    Ok(state)
}

fn subscription_safety_admission_state_for_channel(
    config: &SupplierConfig,
) -> Result<SubscriptionSafetyState> {
    let channel_id = subscription_safety_channel_id(config);
    let mut state = cached_subscription_safety_state(&channel_id)?
        .unwrap_or_else(|| default_subscription_safety_state(config));
    state.rpm_window_unix =
        runtime_subscription_rpm_window(&channel_id, &state.rpm_window_unix, now_unix());
    Ok(state)
}

static SUBSCRIPTION_SAFETY_FILE_LOCK: OnceLock<StdMutex<()>> = OnceLock::new();
static SUBSCRIPTION_SAFETY_STATE_CACHE: OnceLock<
    StdMutex<Option<(PathBuf, HashMap<String, SubscriptionSafetyState>)>>,
> = OnceLock::new();
static SUBSCRIPTION_SAFETY_WRITER: OnceLock<
    std::sync::mpsc::SyncSender<SubscriptionSafetyWriteCommand>,
> = OnceLock::new();
static SUBSCRIPTION_SAFETY_WRITER_INIT: OnceLock<StdMutex<()>> = OnceLock::new();
static SUBSCRIPTION_RPM_WINDOWS: OnceLock<StdMutex<HashMap<String, VecDeque<i64>>>> =
    OnceLock::new();

const SUBSCRIPTION_SAFETY_WRITE_DEBOUNCE: Duration = Duration::from_millis(250);

enum SubscriptionSafetyWriteCommand {
    Dirty,
    Flush(std::sync::mpsc::Sender<Option<String>>),
}

fn subscription_safety_file_lock() -> Result<std::sync::MutexGuard<'static, ()>> {
    SUBSCRIPTION_SAFETY_FILE_LOCK
        .get_or_init(|| StdMutex::new(()))
        .lock()
        .map_err(|_| anyhow!("subscription safety state file lock poisoned"))
}

pub(crate) fn save_subscription_safety_state(mut state: SubscriptionSafetyState) -> Result<()> {
    state.rpm_window_unix =
        runtime_subscription_rpm_window(&state.channel_id, &state.rpm_window_unix, now_unix());
    let path = subscription_safety_state_path();
    // Load the previous process snapshot at most once. Routing always observes
    // the in-memory update below; durable storage is a restart aid and must not
    // delay the current model response.
    let _ = cached_subscription_safety_state(&state.channel_id)?;
    let mut cache = SUBSCRIPTION_SAFETY_STATE_CACHE
        .get_or_init(|| StdMutex::new(None))
        .lock()
        .map_err(|_| anyhow!("subscription safety state cache lock poisoned"))?;
    let Some((cached_path, states)) = cache.as_mut() else {
        return Err(anyhow!("subscription safety state cache is unavailable"));
    };
    if cached_path != &path {
        return Err(anyhow!("subscription safety state path changed while updating"));
    }
    states.insert(state.channel_id.clone(), state);
    drop(cache);
    schedule_subscription_safety_write()
}

pub(crate) fn load_subscription_safety_states() -> Result<HashMap<String, SubscriptionSafetyState>>
{
    let path = subscription_safety_state_path();
    let cache = SUBSCRIPTION_SAFETY_STATE_CACHE.get_or_init(|| StdMutex::new(None));
    {
        let cache = cache
            .lock()
            .map_err(|_| anyhow!("subscription safety state cache lock poisoned"))?;
        if let Some((cached_path, states)) = cache.as_ref() {
            if cached_path == &path {
                return Ok(states.clone());
            }
        }
    }
    let loaded = {
        let _guard = subscription_safety_file_lock()?;
        load_subscription_safety_states_unlocked()?
    };
    let mut cache = cache
        .lock()
        .map_err(|_| anyhow!("subscription safety state cache lock poisoned"))?;
    if let Some((cached_path, states)) = cache.as_ref() {
        if cached_path == &path {
            return Ok(states.clone());
        }
    }
    *cache = Some((path, loaded.clone()));
    Ok(loaded)
}

fn load_subscription_safety_states_unlocked() -> Result<HashMap<String, SubscriptionSafetyState>> {
    let path = subscription_safety_state_path();
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("read subscription safety state {}", path.display()))
        }
    };
    match serde_json::from_str(&raw) {
        Ok(states) => Ok(states),
        Err(original_error) => {
            let mut values = serde_json::Deserializer::from_str(&raw)
                .into_iter::<HashMap<String, SubscriptionSafetyState>>();
            let Some(Ok(states)) = values.next() else {
                return Err(original_error).with_context(|| {
                    format!("parse subscription safety state {}", path.display())
                });
            };
            let trailing = &raw[values.byte_offset()..];
            if trailing.trim().is_empty()
                || !trailing.trim().chars().all(|character| character == '}')
            {
                return Err(original_error).with_context(|| {
                    format!("parse subscription safety state {}", path.display())
                });
            }

            // Older builds wrote this process-wide state through unsynchronised truncate/write
            // operations. Concurrent writers could leave one or more redundant closing braces
            // after an otherwise complete object. Preserve the successfully decoded state and
            // canonicalise only this narrowly identifiable corruption shape.
            save_subscription_safety_states_unlocked(&states).with_context(|| {
                format!(
                    "repair subscription safety state with redundant trailing delimiter {}",
                    path.display()
                )
            })?;
            log::warn!(
                "[const-api][supplier] repaired subscription safety state with redundant trailing delimiter: {}",
                path.display()
            );
            Ok(states)
        }
    }
}

fn cached_subscription_safety_state(channel_id: &str) -> Result<Option<SubscriptionSafetyState>> {
    let path = subscription_safety_state_path();
    let cache = SUBSCRIPTION_SAFETY_STATE_CACHE.get_or_init(|| StdMutex::new(None));
    {
        let cache = cache
            .lock()
            .map_err(|_| anyhow!("subscription safety state cache lock poisoned"))?;
        if let Some((cached_path, states)) = cache.as_ref() {
            if cached_path == &path {
                return Ok(states.get(channel_id).cloned());
            }
        }
    }

    let loaded = load_subscription_safety_states()?;
    let mut cache = cache
        .lock()
        .map_err(|_| anyhow!("subscription safety state cache lock poisoned"))?;
    if let Some((cached_path, states)) = cache.as_ref() {
        if cached_path == &path {
            return Ok(states.get(channel_id).cloned());
        }
    }
    let state = loaded.get(channel_id).cloned();
    *cache = Some((path, loaded));
    Ok(state)
}

fn subscription_rpm_windows() -> &'static StdMutex<HashMap<String, VecDeque<i64>>> {
    SUBSCRIPTION_RPM_WINDOWS.get_or_init(|| StdMutex::new(HashMap::new()))
}

fn runtime_subscription_rpm_window(channel_id: &str, persisted: &[i64], now: i64) -> Vec<i64> {
    let Ok(mut windows) = subscription_rpm_windows().lock() else {
        return persisted
            .iter()
            .copied()
            .filter(|timestamp| *timestamp > now.saturating_sub(60))
            .collect();
    };
    let window = windows.entry(channel_id.to_string()).or_insert_with(|| {
        persisted
            .iter()
            .copied()
            .filter(|timestamp| *timestamp > now.saturating_sub(60))
            .collect()
    });
    window.retain(|timestamp| *timestamp > now.saturating_sub(60));
    window.iter().copied().collect()
}

fn reserve_subscription_rpm(
    channel_id: &str,
    persisted: &[i64],
    rpm_limit: u32,
    now: i64,
) -> std::result::Result<(), i64> {
    let mut windows = subscription_rpm_windows().lock().map_err(|_| 1_i64)?;
    let window = windows.entry(channel_id.to_string()).or_insert_with(|| {
        persisted
            .iter()
            .copied()
            .filter(|timestamp| *timestamp > now.saturating_sub(60))
            .collect()
    });
    window.retain(|timestamp| *timestamp > now.saturating_sub(60));
    if rpm_limit > 0 && window.len() as u32 >= rpm_limit {
        let oldest = window.iter().min().copied().unwrap_or(now);
        return Err((oldest + 60 - now).max(1));
    }
    window.push_back(now);
    Ok(())
}

#[cfg(test)]
fn reset_subscription_safety_state_cache_for_tests() {
    let _ = flush_subscription_safety_writer();
    if let Some(cache) = SUBSCRIPTION_SAFETY_STATE_CACHE.get() {
        if let Ok(mut cache) = cache.lock() {
            *cache = None;
        }
    }
    if let Some(windows) = SUBSCRIPTION_RPM_WINDOWS.get() {
        if let Ok(mut windows) = windows.lock() {
            windows.clear();
        }
    }
}

fn schedule_subscription_safety_write() -> Result<()> {
    match subscription_safety_writer()?.try_send(SubscriptionSafetyWriteCommand::Dirty) {
        Ok(()) | Err(std::sync::mpsc::TrySendError::Full(_)) => Ok(()),
        Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
            Err(anyhow!("subscription safety writer stopped"))
        }
    }
}

fn subscription_safety_writer(
) -> Result<&'static std::sync::mpsc::SyncSender<SubscriptionSafetyWriteCommand>> {
    if let Some(writer) = SUBSCRIPTION_SAFETY_WRITER.get() {
        return Ok(writer);
    }
    let _initialization = SUBSCRIPTION_SAFETY_WRITER_INIT
        .get_or_init(|| StdMutex::new(()))
        .lock()
        .map_err(|_| anyhow!("subscription safety writer initialization poisoned"))?;
    if let Some(writer) = SUBSCRIPTION_SAFETY_WRITER.get() {
        return Ok(writer);
    }
    // One pending dirty signal is sufficient: the writer always snapshots the
    // latest state for every channel instead of replaying obsolete versions.
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("const-safety-state".to_string())
        .spawn(move || subscription_safety_writer_loop(receiver))
        .context("start subscription safety writer")?;
    let _ = SUBSCRIPTION_SAFETY_WRITER.set(sender);
    SUBSCRIPTION_SAFETY_WRITER
        .get()
        .ok_or_else(|| anyhow!("subscription safety writer initialization failed"))
}

fn subscription_safety_writer_loop(
    receiver: std::sync::mpsc::Receiver<SubscriptionSafetyWriteCommand>,
) {
    while let Ok(command) = receiver.recv() {
        let mut acknowledgements = Vec::new();
        let mut force = false;
        match command {
            SubscriptionSafetyWriteCommand::Dirty => {}
            SubscriptionSafetyWriteCommand::Flush(acknowledge) => {
                acknowledgements.push(acknowledge);
                force = true;
            }
        }
        if !force {
            let deadline = Instant::now() + SUBSCRIPTION_SAFETY_WRITE_DEBOUNCE;
            loop {
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                match receiver.recv_timeout(deadline.saturating_duration_since(now)) {
                    Ok(SubscriptionSafetyWriteCommand::Dirty) => {}
                    Ok(SubscriptionSafetyWriteCommand::Flush(acknowledge)) => {
                        acknowledgements.push(acknowledge);
                        break;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
        }
        let error = persist_latest_subscription_safety_states().err().map(|error| {
            log::warn!("[const-api][supplier] asynchronous safety state write failed: {error}");
            error.to_string()
        });
        for acknowledge in acknowledgements {
            let _ = acknowledge.send(error.clone());
        }
    }
}

fn persist_latest_subscription_safety_states() -> Result<()> {
    let snapshot = {
        let cache = SUBSCRIPTION_SAFETY_STATE_CACHE
            .get_or_init(|| StdMutex::new(None))
            .lock()
            .map_err(|_| anyhow!("subscription safety state cache lock poisoned"))?;
        cache.clone()
    };
    let Some((path, states)) = snapshot else {
        return Ok(());
    };
    let _guard = subscription_safety_file_lock()?;
    save_subscription_safety_states_at(&path, &states)
}

pub(crate) fn flush_subscription_safety_writer() -> Result<()> {
    let Some(writer) = SUBSCRIPTION_SAFETY_WRITER.get() else {
        return Ok(());
    };
    let (acknowledge, completed) = std::sync::mpsc::channel();
    writer
        .send(SubscriptionSafetyWriteCommand::Flush(acknowledge))
        .map_err(|_| anyhow!("subscription safety writer stopped"))?;
    match completed
        .recv()
        .map_err(|_| anyhow!("subscription safety writer stopped"))?
    {
        Some(error) => Err(anyhow!(error)),
        None => Ok(()),
    }
}

fn save_subscription_safety_states_unlocked(
    states: &HashMap<String, SubscriptionSafetyState>,
) -> Result<()> {
    let path = subscription_safety_state_path();
    save_subscription_safety_states_at(&path, states)
}

fn save_subscription_safety_states_at(
    path: &std::path::Path,
    states: &HashMap<String, SubscriptionSafetyState>,
) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("subscription safety state path has no parent"))?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "create subscription safety state directory {}",
            parent.display()
        )
    })?;
    let payload = serde_json::to_vec_pretty(states)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).with_context(|| {
        format!(
            "create subscription safety state temp file beside {}",
            path.display()
        )
    })?;
    std::io::Write::write_all(&mut temp, &payload)
        .with_context(|| format!("write subscription safety state {}", path.display()))?;
    std::io::Write::write_all(&mut temp, b"\n")?;
    std::io::Write::flush(&mut temp)?;
    temp.as_file().sync_all()?;
    temp.persist(&path)
        .map_err(|error| error.error)
        .with_context(|| format!("replace subscription safety state {}", path.display()))?;
    Ok(())
}

pub(crate) fn default_subscription_safety_state(
    config: &SupplierConfig,
) -> SubscriptionSafetyState {
    let channel_id = subscription_safety_channel_id(config);
    SubscriptionSafetyState {
        channel_id: channel_id.clone(),
        provider: subscription_provider_from_config(config),
        state: "active".to_string(),
        last_error_kind: String::new(),
        last_error_at_unix: 0,
        last_status: 0,
        cooldown_until_unix: config.subscription.cooldown_until_unix,
        daily_used: 0,
        daily_limit: config.subscription.daily_request_limit,
        rpm_limit: config.subscription.rpm_limit,
        current_concurrency: subscription_current_concurrency(&channel_id),
        max_concurrency: config.max_concurrency(),
        remaining_ratio: if config.subscription.daily_request_limit > 0 {
            1.0
        } else {
            0.0
        },
        quota_source: String::new(),
        quota_window: String::new(),
        quota_used_percent: 0.0,
        quota_reset_at_unix: 0,
        quota_checked_at_unix: 0,
        quota_windows: Vec::new(),
        success_ewma: 1.0,
        latency_ewma_ms: 0.0,
        consecutive_401: 0,
        consecutive_403: 0,
        consecutive_429: 0,
        consecutive_5xx: 0,
        estimated_input_tokens: 0,
        estimated_output_tokens: 0,
        rpm_window_unix: Vec::new(),
    }
}

pub(crate) fn hydrate_subscription_safety_state(
    config: &SupplierConfig,
    state: &mut SubscriptionSafetyState,
) {
    let now = now_unix();
    let local = local_subscription_quota_snapshot(config);
    state.provider = subscription_provider_from_config(config);
    state.daily_used = local.daily_used;
    state.daily_limit = local.daily_limit;
    state.rpm_limit = config.subscription.rpm_limit;
    state.max_concurrency = config.max_concurrency();
    state.current_concurrency = subscription_current_concurrency(&state.channel_id);
    state.remaining_ratio = quota_snapshot_from_local(local, Some(state)).remaining_ratio;
    state
        .rpm_window_unix
        .retain(|ts| *ts > now.saturating_sub(60));
    if state.state == "cooldown" && state.cooldown_until_unix <= now {
        state.state = "active".to_string();
        state.cooldown_until_unix = 0;
    }
    if state.state == "quota_exhausted"
        && quota_snapshot_for_state(config, Some(state)).status != "exhausted"
        && state.cooldown_until_unix <= now
    {
        state.state = "active".to_string();
    }
}

pub(crate) fn subscription_safety_channel_id(config: &SupplierConfig) -> String {
    if !config.channel_id.trim().is_empty() {
        config.channel_id.trim().to_string()
    } else if !config.node_id.trim().is_empty() {
        config.node_id.trim().to_string()
    } else {
        config.name.trim().to_string()
    }
}

pub(crate) fn subscription_current_concurrency(channel_id: &str) -> u32 {
    subscription_concurrency_map()
        .lock()
        .ok()
        .and_then(|guard| guard.get(channel_id).cloned())
        .unwrap_or(0)
}

fn try_increment_subscription_concurrency(channel_id: &str, limit: u32) -> bool {
    let Ok(mut guard) = subscription_concurrency_map().lock() else {
        return false;
    };
    let value = guard.entry(channel_id.to_string()).or_insert(0);
    if limit > 0 && *value >= limit {
        return false;
    }
    *value = value.saturating_add(1);
    true
}

pub(crate) fn subscription_safety_rpm_remaining(
    state: &SubscriptionSafetyState,
    rpm_limit: u32,
) -> u32 {
    if rpm_limit == 0 {
        return 0;
    }
    rpm_limit.saturating_sub(state.rpm_window_unix.len() as u32)
}

pub(crate) fn official_quota_windows_from_response_payload(
    payload: &serde_json::Map<String, serde_json::Value>,
) -> Vec<QuotaWindow> {
    payload
        .get("quota_windows")
        .and_then(|value| serde_json::from_value::<Vec<QuotaWindow>>(value.clone()).ok())
        .unwrap_or_default()
}

pub(crate) fn apply_official_quota_windows_to_state(
    state: &mut SubscriptionSafetyState,
    windows: &[QuotaWindow],
) {
    let windows: Vec<QuotaWindow> = windows
        .iter()
        .filter(|window| !window.source.trim().is_empty() && !window.window.trim().is_empty())
        .cloned()
        .collect();
    if windows.is_empty() {
        return;
    }
    merge_official_quota_windows(&mut state.quota_windows, &windows);
    let fresh_windows = state
        .quota_windows
        .iter()
        .filter(|window| official_quota_window_is_fresh(window, now_unix()))
        .cloned()
        .collect::<Vec<_>>();
    let Some(summary) = channel_quota_summary(&fresh_windows) else {
        return;
    };
    let representative = summary.representative;
    state.quota_source = summary_quota_source(&representative.source);
    state.quota_window = representative.window.clone();
    state.quota_used_percent = representative.used_percent.max(0.0);
    state.quota_reset_at_unix = representative.reset_at_unix;
    state.quota_checked_at_unix = representative.checked_at_unix;
    state.remaining_ratio = summary.remaining_ratio;
    if summary.status == "exhausted" {
        state.state = "quota_exhausted".to_string();
        state.last_error_kind = "quota_exhausted".to_string();
        state.cooldown_until_unix = if representative.reset_at_unix > 0 {
            representative.reset_at_unix
        } else {
            representative.checked_at_unix + 30 * 60
        };
        state.remaining_ratio = 0.0;
    } else if state.state == "quota_exhausted" && summary.status != "unknown" {
        state.state = "active".to_string();
        if state.last_error_kind == "quota_exhausted" {
            state.last_error_kind.clear();
        }
        state.cooldown_until_unix = 0;
    }
}

pub(crate) fn estimate_subscription_tokens(raw: &str) -> u32 {
    if raw.trim().is_empty() {
        return 0;
    }
    ((raw.chars().count() as f64) / 4.0).ceil() as u32
}

pub(crate) fn ewma(current: f64, sample: f64, alpha: f64) -> f64 {
    if current <= 0.0 {
        sample
    } else {
        current * (1.0 - alpha) + sample * alpha
    }
}

fn subscription_routing_latency_sample_ms(
    response_payload: &serde_json::Map<String, serde_json::Value>,
) -> Option<u64> {
    if let Some(latency) = response_payload
        .get("routing_latency_ms")
        .and_then(serde_json::Value::as_u64)
    {
        return Some(latency);
    }
    if let Some(latency) = response_payload
        .get("first_meaningful_event_ms")
        .and_then(serde_json::Value::as_u64)
        .or_else(|| {
            response_payload
                .get("first_sse_event_ms")
                .and_then(serde_json::Value::as_u64)
        })
    {
        return Some(latency);
    }
    let send = response_payload.get("upstream_send_ms")?.as_u64()?;
    let first_chunk = response_payload.get("first_body_chunk_ms")?.as_u64()?;
    Some(send.saturating_add(first_chunk))
}
