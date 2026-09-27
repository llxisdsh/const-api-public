// Reserve the final seconds for the atomic config write, agent handoff, command serialization,
// and failure cleanup before the UI restores its 30-second retry button.
const SUPPLIER_ACTIVATION_PROBE_TIMEOUT: Duration = Duration::from_secs(27);
const SUPPLIER_START_PROBE_TIMEOUT: Duration = Duration::from_secs(55);
const SUPPLIER_VALIDATION_CLOCK_SKEW_SECONDS: i64 = 5 * 60;

pub(crate) async fn start_supplier_inner(
    state: &AppState,
    channel_id: Option<String>,
    channel_ids: Option<Vec<String>>,
) -> Result<SupplierStatus, String> {
    let mut cfg = load_config_from_path(&state.config_path).map_err(|err| err.to_string())?;
    let original_cfg = cfg.clone();
    let access_token = state.account.lock().await.access_token_handle();
    let requested_channel_ids = requested_supplier_channel_ids(channel_id, channel_ids);
    {
        let mut runtime = state.supplier.lock().await;
        if runtime.starting {
            return Ok(runtime.status());
        }
        runtime.starting = true;
        runtime.last_error.clear();
        if !runtime.running {
            clear_supplier_transport_state(&runtime.transport_state);
            update_supplier_transport_state(&runtime.transport_state, "startup", "starting", "");
        }
    }

    let observed_health = match tokio::time::timeout(
        SUPPLIER_START_PROBE_TIMEOUT,
        crate::detection::with_channel_check_deadline(
            SUPPLIER_START_PROBE_TIMEOUT - Duration::from_secs(2),
            prepare_supplier_channels_with_mode(
                &mut cfg,
                &requested_channel_ids,
                SupplierCheckMode::Activation,
            ),
        ),
    )
    .await
    {
        Ok(Ok(health)) => health,
        Ok(Err(err)) => {
            let message = err.to_string();
            fail_supplier_start(state, &message).await;
            return Err(message);
        }
        Err(_) => {
            let message = "supplier startup validation timed out";
            fail_supplier_start(state, message).await;
            return Err(message.to_string());
        }
    };
    finish_supplier_start(
        state,
        original_cfg,
        cfg,
        access_token,
        requested_channel_ids,
        observed_health,
        false,
    )
    .await
}

async fn finish_supplier_start(
    state: &AppState,
    original_cfg: ClientConfig,
    mut cfg: ClientConfig,
    access_token: Arc<StdMutex<String>>,
    requested_channel_ids: Vec<String>,
    observed_health: Vec<SupplierChannelHealth>,
    force_selected_registration: bool,
) -> Result<SupplierStatus, String> {
    let mut health = successful_supplier_monitor_health(&observed_health);
    if health.is_empty() {
        let message = observed_health
            .first()
            .map(|item| format!("{}: {}", item.name, item.message))
            .unwrap_or_else(|| "no channel passed supplier validation".to_string());
        fail_supplier_start(state, &message).await;
        return Err(message);
    }
    let (authoritative, merge) = match commit_detected_channel_updates(
        &state.config_path,
        &state.proxy_config,
        &original_cfg,
        &cfg,
        &requested_channel_ids,
    ) {
        Ok(result) => result,
        Err(err) => {
            let message = err.to_string();
            fail_supplier_start(state, &message).await;
            return Err(message);
        }
    };
    health.retain(|item| merge.committed.iter().any(|id| id == &item.channel_id));
    if health.is_empty() {
        let message = if merge.conflicted.is_empty() {
            "no channel passed supplier validation".to_string()
        } else {
            format!(
                "channel configuration changed during validation: {}",
                merge.conflicted.join(", ")
            )
        };
        fail_supplier_start(state, &message).await;
        return Err(message);
    }
    cfg = authoritative;
    let startable_channel_ids: std::collections::HashSet<String> = health
        .iter()
        .filter(|item| item.status == "available")
        .map(|item| item.channel_id.clone())
        .collect();
    if startable_channel_ids.is_empty() {
        let message = health
            .iter()
            .find(|item| item.status != "available")
            .map(|item| format!("{}: {}", item.name, item.message))
            .unwrap_or_else(|| "no enabled channel".to_string());
        fail_supplier_start(state, &message).await;
        return Err(message);
    }
    let suppliers: Vec<SupplierConfig> = cfg
        .channels
        .iter()
        .filter(|channel| {
            channel.enabled
                && crate::source_driver::channel_is_platform_shareable(channel)
                && supplier_channel_selected(&channel.id, &requested_channel_ids)
                && startable_channel_ids.contains(&channel.id)
        })
        .map(supplier_from_channel_for_node)
        .collect();
    if suppliers.is_empty() {
        fail_supplier_start(state, "no enabled channel").await;
        return Err("no enabled channel".to_string());
    }
    for supplier in &suppliers {
        let channel = cfg
            .channels
            .iter()
            .find(|channel| channel.id == supplier.channel_id);
        let requires_http_surface = channel
            .and_then(|channel| lifecycle_channel_execution_kind(channel).ok())
            .map(|execution_kind| execution_kind == ExecutionKind::HttpSurface)
            .unwrap_or(true);
        let missing_retained_credential = !requires_http_surface
            && channel.is_none_or(|channel| channel.v2.credential_ref.trim().is_empty());
        let validation_error = if missing_retained_credential {
            Some(format!(
                "{} credential reference is required",
                supplier.name
            ))
        } else if requires_http_surface && supplier.upstream_base_url.trim().is_empty() {
            Some(format!("{} upstream base url is required", supplier.name))
        } else if supplier.upstream_model.trim().is_empty() && supplier.models.is_empty() {
            Some(format!("{} upstream model is required", supplier.name))
        } else if supplier.public_model.trim().is_empty() {
            Some(format!("{} public model is required", supplier.name))
        } else if supplier.server_ws_url.trim().is_empty() {
            Some(format!(
                "{} platform websocket url is required",
                supplier.name
            ))
        } else {
            None
        };
        if let Some(message) = validation_error {
            fail_supplier_start(state, &message).await;
            return Err(message);
        }
    }
    let agent_configs = supplier_agent_groups(
        cfg.client_id.clone(),
        suppliers,
        access_token,
        state.account_refresh_notify.clone(),
    );
    let transport_preference = agent_configs
        .first()
        .and_then(|group| supplier_agent_transport_candidates(group).first().cloned())
        .map(|candidate| candidate.transport)
        .unwrap_or_else(|| "websocket".to_string());
    update_local_channel_readiness(
        &state.local_channel_readiness,
        &state.local_model_quota_routes,
        &health,
    );
    let mut runtime = state.supplier.lock().await;
    if !runtime.starting {
        return Ok(runtime.status());
    }
    let previous_health = runtime.status().channels;
    preserve_last_observed_http_versions(&mut health, &previous_health);
    let replace_all_channels = requested_channel_ids.is_empty();
    if replace_all_channels {
        let desired_endpoint_keys: std::collections::HashSet<String> = agent_configs
            .iter()
            .map(SupplierAgentConfig::endpoint_key)
            .collect();
        let stale_endpoints: Vec<String> = runtime
            .channel_agents
            .keys()
            .filter(|endpoint_key| !desired_endpoint_keys.contains(*endpoint_key))
            .cloned()
            .collect();
        for endpoint_key in stale_endpoints {
            if let Some(handle) = runtime.channel_agents.remove(&endpoint_key) {
                let _ = handle.agent_shutdown.send(());
                let _ = handle.monitor_shutdown.send(());
                clear_supplier_transport_nodes(&runtime.transport_state, &handle.node_ids);
                runtime
                    .nodes
                    .retain(|node_id| !handle.node_ids.contains(node_id));
            }
        }
    }
    let mut new_nodes = Vec::new();
    let shared_transport_state = runtime.transport_state.clone();
    let requested_channel_set: std::collections::HashSet<String> =
        requested_channel_ids.iter().cloned().collect();
    for mut group in agent_configs {
        let endpoint_key = group.endpoint_key();
        if !replace_all_channels {
            if let Some(handle) = runtime.channel_agents.get(&endpoint_key) {
                let mut merged = handle.suppliers.clone();
                let updating_channels: std::collections::HashSet<String> =
                    group.channel_ids().into_iter().collect();
                merged.retain(|supplier| !updating_channels.contains(&supplier.channel_id));
                merged.extend(group.suppliers.clone());
                group.suppliers = merged;
            }
        }
        let update_queue_closed = runtime
            .channel_agents
            .get(&endpoint_key)
            .is_some_and(|handle| handle.register_update.is_closed());
        if update_queue_closed {
            if let Some(handle) = runtime.channel_agents.remove(&endpoint_key) {
                log::warn!(
                    "[const-api][supplier] restarting stopped endpoint agent endpoint_key={}",
                    endpoint_key
                );
                let _ = handle.agent_shutdown.send(());
                let _ = handle.monitor_shutdown.send(());
                clear_supplier_transport_nodes(&runtime.transport_state, &handle.node_ids);
                runtime
                    .nodes
                    .retain(|node_id| !handle.node_ids.contains(node_id));
            }
        }
        let group_channel_ids = group.channel_ids();
        let group_node_ids = group.supplier_unit_ids();
        let existing_node_ids = runtime
            .channel_agents
            .get(&endpoint_key)
            .map(|handle| handle.node_ids.clone())
            .unwrap_or_default();
        inherit_supplier_shared_transport(
            &shared_transport_state,
            &existing_node_ids,
            &group_node_ids,
        );
        for node_id in &group_node_ids {
            if !existing_node_ids.contains(node_id) {
                update_supplier_transport_state(&runtime.transport_state, node_id, "starting", "");
            }
        }
        let forced_suppliers = if force_selected_registration {
            group
                .suppliers
                .iter()
                .filter(|supplier| requested_channel_set.contains(&supplier.channel_id))
                .cloned()
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        if force_selected_registration {
            let forced_node_ids = forced_suppliers
                .iter()
                .map(|supplier| supplier_unit_id(&group.client_id, &supplier.channel_id))
                .collect::<Vec<_>>();
            invalidate_supplier_registration_acks(&shared_transport_state, &forced_node_ids);
        }
        if let Some(handle) = runtime.channel_agents.get_mut(&endpoint_key) {
            let (monitor_shutdown_tx, monitor_shutdown_rx) = oneshot::channel();
            let old_monitor_shutdown =
                std::mem::replace(&mut handle.monitor_shutdown, monitor_shutdown_tx);
            let _ = old_monitor_shutdown.send(());
            spawn_logged(
                "supplier channel monitor",
                monitor_supplier_channels(
                    state.config_path.clone(),
                    group_channel_ids.clone(),
                    Some(handle.register_update.clone()),
                    state.proxy_config.clone(),
                    state.local_channel_readiness.clone(),
                    state.local_model_quota_routes.clone(),
                    shared_transport_state.clone(),
                    monitor_shutdown_rx,
                ),
            );
            handle.channel_ids = group_channel_ids.clone();
            handle.node_ids = group_node_ids.clone();
            handle.suppliers = group.suppliers.clone();
            if force_selected_registration {
                if forced_suppliers.is_empty() {
                    log::warn!(
                        "[const-api][supplier] skipped empty forced registration endpoint_key={}",
                        endpoint_key
                    );
                } else {
                    send_forced_register_update(&handle.register_update, forced_suppliers);
                }
            } else {
                send_register_update(&handle.register_update, group.suppliers.clone());
            }
        } else {
            let (agent_shutdown_tx, agent_shutdown_rx) = oneshot::channel();
            let (monitor_shutdown_tx, monitor_shutdown_rx) = oneshot::channel();
            let (register_update_tx, register_update_rx) =
                mpsc::channel::<SupplierRegistrationUpdate>(8);
            spawn_logged(
                "supplier channel monitor",
                monitor_supplier_channels(
                    state.config_path.clone(),
                    group_channel_ids.clone(),
                    Some(register_update_tx.clone()),
                    state.proxy_config.clone(),
                    state.local_channel_readiness.clone(),
                    state.local_model_quota_routes.clone(),
                    shared_transport_state.clone(),
                    monitor_shutdown_rx,
                ),
            );
            spawn_logged(
                "supplier agent",
                run_supplier_agent(
                    group.clone(),
                    agent_shutdown_rx,
                    register_update_rx,
                    runtime.transport_state.clone(),
                ),
            );
            runtime.channel_agents.insert(
                endpoint_key.clone(),
                SupplierAgentHandle {
                    client_id: group.client_id.clone(),
                    channel_ids: group_channel_ids.clone(),
                    node_ids: group_node_ids.clone(),
                    suppliers: group.suppliers.clone(),
                    agent_shutdown: agent_shutdown_tx,
                    monitor_shutdown: monitor_shutdown_tx,
                    register_update: register_update_tx,
                },
            );
        }
        new_nodes.extend(group_node_ids);
    }
    runtime.running = true;
    runtime.starting = false;
    runtime.node_id = runtime.nodes.first().cloned().unwrap_or_default();
    runtime.server_ws_url = cfg
        .channels
        .iter()
        .find(|channel| {
            channel.enabled
                && crate::source_driver::channel_is_platform_shareable(channel)
                && supplier_channel_selected(&channel.id, &requested_channel_ids)
        })
        .map(|channel| channel.server_ws_url.clone())
        .unwrap_or_default();
    runtime.server_quic_url = cfg
        .channels
        .iter()
        .find(|channel| {
            channel.enabled
                && crate::source_driver::channel_is_platform_shareable(channel)
                && supplier_channel_selected(&channel.id, &requested_channel_ids)
        })
        .map(|channel| channel.server_quic_url.clone())
        .unwrap_or_default();
    runtime.transport_preference = transport_preference;
    runtime.nodes.retain(|node_id| !new_nodes.contains(node_id));
    runtime.nodes.extend(new_nodes);
    runtime.node_id = runtime.nodes.first().cloned().unwrap_or_default();
    let health_channel_ids: std::collections::HashSet<String> =
        health.iter().map(|item| item.channel_id.clone()).collect();
    let failed_count = health
        .iter()
        .filter(|item| item.status != "available")
        .count();
    update_supplier_channel_health_state(&runtime.transport_state, &health);
    runtime
        .channels
        .retain(|item| !health_channel_ids.contains(&item.channel_id));
    runtime.channels.extend(health);
    runtime.last_error = if failed_count > 0 {
        format!("{failed_count} channel(s) failed to start")
    } else {
        String::new()
    };
    Ok(runtime.status())
}

async fn fail_supplier_start(state: &AppState, message: &str) {
    let mut runtime = state.supplier.lock().await;
    runtime.starting = false;
    runtime.last_error = message.to_string();
    if !runtime.running {
        clear_supplier_transport_state(&runtime.transport_state);
    }
}

pub(crate) async fn activate_supplier_channel_inner(
    state: &AppState,
    channel_id: String,
) -> Result<SupplierStatus, String> {
    let channel_id = channel_id.trim().to_string();
    if channel_id.is_empty() {
        return Err("channel_id is required".to_string());
    }

    let mut cfg = load_config_from_path(&state.config_path).map_err(|err| err.to_string())?;
    let original_cfg = cfg.clone();
    let channel = cfg
        .channels
        .iter_mut()
        .find(|channel| channel.id == channel_id)
        .ok_or_else(|| format!("channel {channel_id} was not found"))?;
    let platform_shareable = crate::source_driver::channel_is_platform_shareable(channel);
    if channel.enabled || channel.share_enabled {
        return Err("channel must be saved as disabled before atomic activation".to_string());
    }
    channel.enabled = true;
    channel.share_enabled = platform_shareable;
    let selected_ids = vec![channel_id.clone()];
    if !platform_shareable {
        return activate_local_only_channel_inner(state, original_cfg, cfg, selected_ids).await;
    }

    let access_token = state.account.lock().await.access_token_handle();
    {
        let mut runtime = state.supplier.lock().await;
        if runtime.starting {
            return Err("another supplier operation is already in progress".to_string());
        }
        runtime.starting = true;
        runtime.last_error.clear();
        stop_supplier_channel_agents(&mut runtime, std::slice::from_ref(&channel_id));
        update_supplier_runtime_after_channel_stop(&mut runtime, true);
        if !runtime.running {
            clear_supplier_transport_state(&runtime.transport_state);
            update_supplier_transport_state(&runtime.transport_state, "startup", "starting", "");
        }
    }

    let observed_health = match tokio::time::timeout(
        SUPPLIER_ACTIVATION_PROBE_TIMEOUT,
        crate::detection::with_channel_check_deadline(
            SUPPLIER_ACTIVATION_PROBE_TIMEOUT - Duration::from_secs(2),
            prepare_supplier_channels_with_mode(
                &mut cfg,
                &selected_ids,
                SupplierCheckMode::Activation,
            ),
        ),
    )
    .await
    {
        Ok(Ok(health)) => health,
        Ok(Err(err)) => {
            let message = err.to_string();
            fail_supplier_start(state, &message).await;
            return Err(message);
        }
        Err(_) => {
            let message = "channel activation timed out; the channel remains disabled";
            fail_supplier_start(state, message).await;
            return Err(message.to_string());
        }
    };

    finish_supplier_start(
        state,
        original_cfg,
        cfg,
        access_token,
        selected_ids,
        observed_health,
        false,
    )
    .await
}

async fn activate_local_only_channel_inner(
    state: &AppState,
    original_cfg: ClientConfig,
    mut cfg: ClientConfig,
    selected_ids: Vec<String>,
) -> Result<SupplierStatus, String> {
    {
        let mut runtime = state.supplier.lock().await;
        if runtime.starting {
            return Err("another supplier operation is already in progress".to_string());
        }
        runtime.starting = true;
    }

    let observed_health = match tokio::time::timeout(
        SUPPLIER_ACTIVATION_PROBE_TIMEOUT,
        crate::detection::with_channel_check_deadline(
            SUPPLIER_ACTIVATION_PROBE_TIMEOUT - Duration::from_secs(2),
            prepare_local_only_channels_with_mode(
                &mut cfg,
                &selected_ids,
                SupplierCheckMode::Activation,
            ),
        ),
    )
    .await
    {
        Ok(Ok(health)) => health,
        Ok(Err(err)) => {
            let message = err.to_string();
            finish_local_channel_activation(state).await;
            return Err(message);
        }
        Err(_) => {
            let message = "channel activation timed out; the channel remains disabled";
            finish_local_channel_activation(state).await;
            return Err(message.to_string());
        }
    };

    let mut health = successful_supplier_monitor_health(&observed_health);
    if !health.iter().any(|item| item.status == "available") {
        let message = observed_health
            .first()
            .map(|item| format!("{}: {}", item.name, item.message))
            .unwrap_or_else(|| "no channel passed local validation".to_string());
        finish_local_channel_activation(state).await;
        return Err(message);
    }
    let (_, merge) = match commit_detected_channel_updates(
        &state.config_path,
        &state.proxy_config,
        &original_cfg,
        &cfg,
        &selected_ids,
    ) {
        Ok(result) => result,
        Err(err) => {
            let message = err.to_string();
            finish_local_channel_activation(state).await;
            return Err(message);
        }
    };
    health.retain(|item| merge.committed.iter().any(|id| id == &item.channel_id));
    if !health.iter().any(|item| item.status == "available") {
        let message = if merge.conflicted.is_empty() {
            "no channel passed local validation".to_string()
        } else {
            format!(
                "channel configuration changed during validation: {}",
                merge.conflicted.join(", ")
            )
        };
        finish_local_channel_activation(state).await;
        return Err(message);
    }

    let committed_ids = health
        .iter()
        .map(|item| item.channel_id.clone())
        .collect::<std::collections::HashSet<_>>();
    update_local_channel_readiness(
        &state.local_channel_readiness,
        &state.local_model_quota_routes,
        &health,
    );
    let mut runtime = state.supplier.lock().await;
    let previous_health = runtime.status().channels;
    preserve_last_observed_http_versions(&mut health, &previous_health);
    update_supplier_channel_health_state(&runtime.transport_state, &health);
    runtime
        .channels
        .retain(|item| !committed_ids.contains(&item.channel_id));
    runtime.channels.extend(health);
    runtime.starting = false;
    Ok(runtime.status())
}

async fn finish_local_channel_activation(state: &AppState) {
    let mut runtime = state.supplier.lock().await;
    runtime.starting = false;
}

pub(crate) async fn recover_supplier_channel_inner(
    state: &AppState,
    channel_id: String,
    full_check: bool,
) -> Result<SupplierStatus, String> {
    let channel_id = channel_id.trim().to_string();
    if channel_id.is_empty() {
        return Err("channel_id is required".to_string());
    }

    let mut cfg = load_config_from_path(&state.config_path).map_err(|err| err.to_string())?;
    let original_cfg = cfg.clone();
    let access_token = state.account.lock().await.access_token_handle();
    let selected_ids = vec![channel_id.clone()];
    {
        let mut runtime = state.supplier.lock().await;
        if runtime.starting {
            return Err("another supplier operation is already in progress".to_string());
        }
        runtime.starting = true;
        runtime.last_error.clear();
    }
    let (check_mode, recovery_mode) = if full_check {
        (SupplierCheckMode::FullInteractive, "full")
    } else {
        (SupplierCheckMode::Snapshot, "snapshot")
    };
    log::info!(
        "[const-api][supplier] channel recovery begin channel_id={} mode={}",
        channel_id,
        recovery_mode
    );
    let observed_health = match tokio::time::timeout(
        SUPPLIER_ACTIVATION_PROBE_TIMEOUT,
        crate::detection::with_channel_check_deadline(
            SUPPLIER_ACTIVATION_PROBE_TIMEOUT - Duration::from_secs(2),
            prepare_supplier_channels_with_mode(&mut cfg, &selected_ids, check_mode),
        ),
    )
    .await
    {
        Ok(Ok(health)) => health,
        Ok(Err(err)) => {
            let message = err.to_string();
            log::warn!(
                "[const-api][supplier] channel recovery probe failed channel_id={} mode={}: {}",
                channel_id,
                recovery_mode,
                message
            );
            fail_supplier_start(state, &message).await;
            return Err(message);
        }
        Err(_) => {
            let message = format!(
                "channel {recovery_mode} recovery timed out; the existing runtime was kept"
            );
            log::warn!(
                "[const-api][supplier] channel recovery timed out channel_id={} mode={}",
                channel_id,
                recovery_mode
            );
            fail_supplier_start(state, &message).await;
            return Err(message);
        }
    };

    // A fresh snapshot is enough to refresh the selected runtime without spending a generation.
    // Automatic recovery may use one full validation per failure episode when local health is
    // still unavailable. Recovery keeps a healthy shared transport alive and forces a
    // channel-scoped registration update instead of unregistering the channel first.
    let result = finish_supplier_start(
        state,
        original_cfg,
        cfg,
        access_token,
        selected_ids,
        observed_health,
        true,
    )
    .await;
    match &result {
        Ok(_) => log::info!(
            "[const-api][supplier] channel recovery registration requested channel_id={} mode={}",
            channel_id,
            recovery_mode
        ),
        Err(error) => log::warn!(
            "[const-api][supplier] channel recovery failed channel_id={} mode={}: {}",
            channel_id,
            recovery_mode,
            error
        ),
    }
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SupplierCheckMode {
    FullInteractive,
    Snapshot,
    // Local evidence only. Background snapshots/recovery own freshness and failures.
    Cached,
    // Reuse prior checks; only a never-checked identity needs initial validation.
    Activation,
}

static SUBSCRIPTION_CONCURRENCY: OnceLock<StdMutex<HashMap<String, u32>>> = OnceLock::new();

fn subscription_concurrency_map() -> &'static StdMutex<HashMap<String, u32>> {
    SUBSCRIPTION_CONCURRENCY.get_or_init(|| StdMutex::new(HashMap::new()))
}

#[cfg(test)]
pub(crate) fn reset_subscription_safety_runtime_for_tests() {
    if let Ok(mut guard) = subscription_concurrency_map().lock() {
        guard.clear();
    }
    reset_subscription_safety_state_cache_for_tests();
}

#[cfg(test)]
pub(crate) fn subscription_safety_enabled_for_tests(config: &SupplierConfig) -> bool {
    let channel_id = subscription_safety_channel_id(config);
    channel_id.starts_with("safety-")
        || channel_id.starts_with("risk-")
        || channel_id.starts_with("register-")
}

#[cfg(test)]
pub(crate) async fn prepare_supplier_channels(
    cfg: &mut ClientConfig,
    channel_ids: &[String],
) -> Result<Vec<SupplierChannelHealth>> {
    prepare_supplier_channels_with_mode(cfg, channel_ids, SupplierCheckMode::FullInteractive).await
}

pub(crate) async fn prepare_supplier_channels_snapshot(
    cfg: &mut ClientConfig,
    channel_ids: &[String],
) -> Result<Vec<SupplierChannelHealth>> {
    prepare_supplier_channels_with_mode(cfg, channel_ids, SupplierCheckMode::Snapshot).await
}

pub(crate) async fn prepare_supplier_channels_with_mode(
    cfg: &mut ClientConfig,
    channel_ids: &[String],
    mode: SupplierCheckMode,
) -> Result<Vec<SupplierChannelHealth>> {
    prepare_channels_with_mode(cfg, channel_ids, mode, |channel| {
        crate::source_driver::channel_is_platform_shareable(channel)
    })
    .await
}

async fn prepare_local_only_channels_snapshot(
    cfg: &mut ClientConfig,
    channel_ids: &[String],
) -> Result<Vec<SupplierChannelHealth>> {
    prepare_local_only_channels_with_mode(cfg, channel_ids, SupplierCheckMode::Snapshot).await
}

async fn prepare_local_only_channels_with_mode(
    cfg: &mut ClientConfig,
    channel_ids: &[String],
    mode: SupplierCheckMode,
) -> Result<Vec<SupplierChannelHealth>> {
    prepare_channels_with_mode(cfg, channel_ids, mode, |channel| {
        !crate::source_driver::channel_is_platform_shareable(channel)
    })
    .await
}

async fn prepare_channels_with_mode(
    cfg: &mut ClientConfig,
    channel_ids: &[String],
    mode: SupplierCheckMode,
    include_channel: impl Fn(&ChannelConfig) -> bool,
) -> Result<Vec<SupplierChannelHealth>> {
    let enabled_ids: std::collections::HashSet<String> = cfg
        .channels
        .iter()
        .filter(|channel| {
            channel.enabled
                && include_channel(channel)
                && supplier_channel_selected(&channel.id, channel_ids)
        })
        .map(|channel| channel.id.clone())
        .collect();
    if enabled_ids.is_empty() {
        return Err(anyhow!(
            "{}",
            if channel_ids.is_empty() {
                "no enabled channel"
            } else {
                "selected channel is not enabled"
            }
        ));
    }

    let mut health = Vec::new();
    let mut prepared = Vec::with_capacity(cfg.channels.len());
    // A recurring snapshot must return a health observation even when every
    // selected channel is unreachable. Interactive activation still fails
    // closed and leaves the saved enablement unchanged.
    let allow_partial = mode == SupplierCheckMode::Snapshot
        || channel_ids.is_empty()
        || enabled_ids.len() > 1;
    let mut failures = Vec::new();
    for channel in cfg.channels.clone() {
        if !enabled_ids.contains(&channel.id) {
            prepared.push(channel);
            continue;
        }
        let started_at = std::time::Instant::now();
        match validate_supplier_channel(channel.clone(), mode).await {
            Ok((next_channel, channel_health)) => {
                prepared.push(next_channel);
                health.push(channel_health);
            }
            Err(err) if allow_partial => {
                let message = err.to_string();
                failures.push(format!("{}: {message}", channel.name));
                health.push(failed_supplier_channel_health(
                    &channel,
                    message,
                    started_at.elapsed().as_millis(),
                    now_unix(),
                ));
                prepared.push(channel);
            }
            Err(err) => return Err(err),
        }
    }
    cfg.channels = prepared;
    if mode != SupplierCheckMode::Snapshot
        && health.iter().all(|item| item.status != "available")
    {
        return Err(anyhow!(
            "{}",
            failures
                .first()
                .cloned()
                .or_else(|| health.iter().find(|item| !item.message.is_empty()).map(|item| item.message.clone()))
                .unwrap_or_else(|| "no channel passed supplier validation".to_string())
        ));
    }
    Ok(health)
}

pub(crate) fn failed_supplier_channel_health(
    channel: &ChannelConfig,
    message: String,
    latency_ms: u128,
    checked_at_unix: i64,
) -> SupplierChannelHealth {
    let model = pick_primary_model(&channel.models, channel);
    SupplierChannelHealth {
        channel_id: channel.id.clone(),
        name: channel.name.clone(),
        status: "error".to_string(),
        model,
        model_count: channel.models.len(),
        latency_ms,
        upstream_http_version: String::new(),
        credential_status: "unknown".to_string(),
        quota_status: "unknown".to_string(),
        remaining_ratio: 0.0,
        daily_used: 0,
        daily_limit: channel.subscription.daily_request_limit,
        quota_checked_at_unix: checked_at_unix,
        quota_windows: Vec::new(),
        message,
        checked_at_unix,
        safety_state: "error".to_string(),
        last_error_kind: String::new(),
        cooldown_until_unix: 0,
        success_ewma: 0.0,
        latency_ewma_ms: latency_ms as f64,
        current_concurrency: 0,
        max_concurrency: channel.max_concurrency(),
        rpm_limit: channel.subscription.rpm_limit,
        rpm_remaining: channel.subscription.rpm_limit,
    }
}

pub(crate) async fn validate_supplier_channel(
    channel: ChannelConfig,
    mode: SupplierCheckMode,
) -> Result<(ChannelConfig, SupplierChannelHealth)> {
    crate::channel_surface::validate_channel_v2(&channel)?;
    let mode = supplier_effective_check_mode(&channel, mode, now_unix());
    let started_at = std::time::Instant::now();
    let detection = if mode == SupplierCheckMode::Cached {
        Ok(ChannelDetectionResult {
            availability: None,
            models: channel.models.clone(),
            surface_results: Vec::new(),
            model_capability_evidence: Vec::new(),
            detection_evidence: Vec::new(),
            quota_status: "unknown".into(),
            remaining_ratio: 0.0,
            quota_windows: Vec::new(),
            checks: Vec::new(),
            warnings: Vec::new(),
        })
    } else if mode == SupplierCheckMode::Snapshot {
        snapshot_channel(channel.clone()).await
    } else {
        detect_channel(channel.clone()).await
    }
    .with_context(|| format!("{} detection failed", channel.name))?;
    let mut next_channel = channel.clone();
    merge_channel_detection_result(&mut next_channel, &detection);
    if let Some(health) = crate::openrouter::empty_catalog_health(&next_channel, started_at.elapsed().as_millis(), now_unix()) {
        return Ok((next_channel, health));
    }
    let model = pick_primary_model(&next_channel.models, &next_channel);
    let execution_kind = lifecycle_channel_execution_kind(&next_channel)?;
    let retained_subscription = execution_kind != ExecutionKind::HttpSurface
        || crate::coding_plan::has_quota(next_channel.source_driver());
    next_channel.enabled = true;
    next_channel.share_enabled = crate::source_driver::channel_is_platform_shareable(&next_channel);
    let supplier_config = supplier_from_channel(&next_channel);
    if retained_subscription && !detection.quota_windows.is_empty() {
        let mut state = subscription_safety_state_for_channel(&supplier_config)?;
        apply_official_quota_windows_to_state(&mut state, &detection.quota_windows);
        save_subscription_safety_state(state)?;
    }
    let quota_reserve_blocked = if retained_subscription {
        let state = subscription_safety_state_for_channel(&supplier_config)?;
        quota_reserve_decision_for_state(
            &state,
            next_channel.quota_reserve_percent(),
            now_unix(),
        )
        .is_some_and(|decision| decision.blocked)
    } else {
        false
    };

    let test = if mode == SupplierCheckMode::FullInteractive && !quota_reserve_blocked {
        if let Some(report) = &detection.availability {
            report.test.clone()
        } else { Some(
            test_channel_upstream_inner(next_channel.clone(), normalized_health_prompt(""))
                .await
                .map_err(|err| anyhow!("{} upstream interaction failed: {err}", channel.name))?,
        ) }
    } else {
        None
    };

    let credential_status = if detection
        .checks
        .iter()
        .any(|check| check.to_lowercase().contains("refreshed"))
        || test
            .as_ref()
            .map(|item| item.raw.to_lowercase().contains("\"refreshed\":true"))
            .unwrap_or(false)
    {
        "refreshed"
    } else if retained_subscription && !next_channel.v2.credential_ref.trim().is_empty() {
        "ok"
    } else if retained_subscription {
        "missing"
    } else {
        "not_required"
    }
    .to_string();
    let quota = if retained_subscription {
        let local = subscription_quota_snapshot(&supplier_config);
        let raw = test
            .as_ref()
            .and_then(|item| serde_json::from_str::<serde_json::Value>(&item.raw).ok());
        merge_quota_snapshot(
            local,
            raw.as_ref()
                .and_then(|value| value.get("quota_status"))
                .and_then(|value| value.as_str())
                .unwrap_or("unknown"),
            raw.as_ref()
                .and_then(|value| value.get("remaining_ratio"))
                .and_then(|value| value.as_f64())
                .unwrap_or(0.0),
        )
    } else {
        SubscriptionQuotaSnapshot {
            status: "not_applicable".to_string(),
            remaining_ratio: 0.0,
            daily_used: 0,
            daily_limit: 0,
            checked_at_unix: now_unix(),
            quota_source: "unknown".to_string(),
            quota_window: String::new(),
            quota_used_percent: 0.0,
            quota_reset_at_unix: 0,
            quota_windows: Vec::new(),
        }
    };

    let safety = if retained_subscription {
        Some(subscription_safety_state_for_channel(
            &supplier_from_channel(&next_channel),
        )?)
    } else {
        None
    };
    let rpm_remaining = safety
        .as_ref()
        .map(|state| subscription_safety_rpm_remaining(state, next_channel.subscription.rpm_limit))
        .unwrap_or(next_channel.subscription.rpm_limit);
    let max_concurrency = next_channel.max_concurrency();
    let rpm_limit = next_channel.subscription.rpm_limit;

    let effective_models = availability::effective_models(&next_channel, next_channel.models.clone());
    let admission_paused = !next_channel.models.is_empty() && effective_models.is_empty();
    let admission_unverified = availability::admission_pending(&next_channel);
    let admission_message = detection.availability.as_ref().map(|report| report.groups.iter()
        .map(|g| format!("{} {}: {}{}{}", g.group, g.representative, g.status,
            if g.message.is_empty() { "" } else { " — " }, g.message))
        .collect::<Vec<_>>().join("; "));
    Ok((
        next_channel,
        SupplierChannelHealth {
            channel_id: channel.id,
            name: channel.name,
            status: if admission_paused || admission_unverified { "unavailable".into() } else { safety
                .as_ref()
                .map(|state| state.state.clone())
                .filter(|state| state != "active")
                .unwrap_or_else(|| "available".to_string()) },
            model: test
                .as_ref()
                .map(|item| item.model.clone())
                .unwrap_or(model),
            model_count: effective_models.len(),
            latency_ms: started_at.elapsed().as_millis(),
            upstream_http_version: test
                .as_ref()
                .map(|item| item.upstream_http_version.clone())
                .unwrap_or_default(),
            credential_status,
            quota_status: quota.status,
            remaining_ratio: quota.remaining_ratio,
            daily_used: quota.daily_used,
            daily_limit: quota.daily_limit,
            quota_checked_at_unix: quota.checked_at_unix,
            quota_windows: quota.quota_windows.clone(),
            message: admission_message.or_else(|| test
                .as_ref()
                .map(|item| item.content.clone()))
                .unwrap_or_else(|| {
                    if quota_reserve_blocked {
                        "live interaction skipped because quota reserve is active".to_string()
                    } else {
                        String::new()
                    }
                }),
            checked_at_unix: now_unix(),
            safety_state: safety
                .as_ref()
                .map(|state| state.state.clone())
                .unwrap_or_else(|| "not_applicable".to_string()),
            last_error_kind: safety
                .as_ref()
                .map(|state| state.last_error_kind.clone())
                .unwrap_or_default(),
            cooldown_until_unix: safety
                .as_ref()
                .map(|state| state.cooldown_until_unix)
                .unwrap_or_default(),
            success_ewma: safety
                .as_ref()
                .map(|state| state.success_ewma)
                .unwrap_or(1.0),
            latency_ewma_ms: safety
                .as_ref()
                .map(|state| state.latency_ewma_ms)
                .unwrap_or(started_at.elapsed().as_millis() as f64),
            current_concurrency: safety
                .as_ref()
                .map(|state| state.current_concurrency)
                .unwrap_or_default(),
            max_concurrency,
            rpm_limit,
            rpm_remaining,
        },
    ))
}

fn supplier_effective_check_mode(
    channel: &ChannelConfig,
    requested: SupplierCheckMode,
    now: i64,
) -> SupplierCheckMode {
    if requested != SupplierCheckMode::Activation {
        return requested;
    }
    if !supplier_channel_has_validation(channel, now) && !availability::has_check_snapshot(channel)
    {
        return SupplierCheckMode::FullInteractive;
    }
    // Restarting or changing a limit is not consent to repeat paid checks. The
    // normal validator still applies saved admission, safety and quota gates;
    // the existing monitor and bounded recovery handle subsequent observations.
    SupplierCheckMode::Cached
}

fn supplier_channel_has_validation(channel: &ChannelConfig, now: i64) -> bool {
    if channel.models.is_empty() {
        return false;
    }
    let checked_at = channel
        .surface_bindings
        .iter()
        .filter(|binding| binding.surface == channel.v2.default_target.surface)
        .filter_map(|binding| {
            let surface_checked = if binding.verification.state.trim() == "verified" {
                binding.verification.checked_at_unix
            } else {
                0
            };
            let protocol_checked = binding
                .protocols
                .iter()
                .filter(|protocol| {
                    crate::protocol::kind::ProtocolKind::parse(&protocol.protocol).ok()
                        == Some(channel.v2.default_target.protocol)
                        && protocol.verification.state.trim() == "verified"
                })
                .map(|protocol| protocol.verification.checked_at_unix)
                .max()
                .unwrap_or_default();
            let checked_at = surface_checked.max(protocol_checked);
            (checked_at > 0).then_some(checked_at)
        })
        .max()
        .unwrap_or_default();
    checked_at > 0 && checked_at <= now.saturating_add(SUPPLIER_VALIDATION_CLOCK_SKEW_SECONDS)
}

pub(crate) fn merge_channel_detection_result(
    channel: &mut ChannelConfig,
    detection: &ChannelDetectionResult,
) {
    channel.models = dedupe_models(detection.models.clone());
    crate::openrouter::apply_refreshed_catalog(channel);

    for result in &detection.surface_results {
        if let Some(binding) = channel
            .surface_bindings
            .iter_mut()
            .find(|binding| binding.surface == result.surface)
        {
            if result.surface_verification.state == "verified"
                || binding.verification.state != "verified"
            {
                binding.verification = result.surface_verification.clone();
            }
            if let Some(protocol) = binding.protocols.iter_mut().find(|binding| {
                crate::protocol::kind::ProtocolKind::parse(&binding.protocol).ok()
                    == Some(result.protocol)
            }) {
                protocol.verification = result.protocol_verification.clone();
            }
        }
    }

    let refreshed_capability_protocols = detection
        .model_capability_evidence
        .iter()
        .filter(|evidence| !evidence.catalog_metadata && matches!(evidence.verification_state.as_str(), "verified" | "rejected"))
        .map(|evidence| evidence.protocol.as_str())
        .collect::<std::collections::HashSet<_>>();
    channel.capability_profiles.retain(|existing| {
        if existing.catalog_metadata {
            // A successful refresh replaces the entire catalog claim, even if
            // the new directory omits a limit that used to be present.
            return !detection.surface_results.iter().any(|result| result.protocol.as_str() == existing.protocol && result.surface_verification.state == "verified");
        }
        existing.model_pattern.is_empty()
            || !refreshed_capability_protocols.contains(existing.protocol.as_str())
    });
    for evidence in &detection.model_capability_evidence {
        if let Some(existing) = channel
            .capability_profiles
            .iter_mut()
            .find(|existing| {
                existing.protocol == evidence.protocol
                    && existing.model_pattern == evidence.model_pattern
                    && existing.catalog_metadata == evidence.catalog_metadata
            })
        {
            // A directory refresh can repeat a provider declaration, but it
            // must not downgrade a successful live check to declared evidence.
            if existing.verification_state != "verified"
                || evidence.verification_state != "declared"
                || evidence.catalog_metadata
            {
                *existing = evidence.clone();
            }
        } else {
            channel.capability_profiles.push(evidence.clone());
        }
    }
    for evidence in &detection.detection_evidence {
        if let Some(existing) = channel.detection_checks.iter_mut().find(|existing| {
            existing.name == evidence.name
                && existing.protocol == evidence.protocol
                && existing.capability == evidence.capability
        }) {
            if detection_evidence_replaces_existing(existing, evidence) {
                *existing = evidence.clone();
            }
        } else {
            channel.detection_checks.push(evidence.clone());
        }
    }
    availability::reconcile_catalog(channel);
}

fn detection_evidence_replaces_existing(
    existing: &ChannelDetectionCheck,
    incoming: &ChannelDetectionCheck,
) -> bool {
    if incoming.name != "surface_operation_family"
        && !(incoming.name == "responses_operation" && incoming.capability.starts_with("openai."))
    {
        return true;
    }
    match incoming.status.trim().to_ascii_lowercase().as_str() {
        "driver_contract" | "verified" | "unsupported" => true,
        // Authentication, throttling, transient provider errors and network
        // failures say nothing conclusive about whether the operation exists.
        // Preserve the last decisive observation instead of making the UI and
        // supplier declaration flap with every manual refresh.
        "credential_limited" | "unknown" => !matches!(
            existing.status.trim().to_ascii_lowercase().as_str(),
            "driver_contract" | "verified" | "unsupported"
        ),
        _ => true,
    }
}

pub(crate) fn lifecycle_channel_execution_kind(channel: &ChannelConfig) -> Result<ExecutionKind> {
    Ok(source_driver(channel.source_driver())?.execution_kind())
}

#[derive(Default)]
struct DetectedChannelMerge {
    committed: Vec<String>,
    conflicted: Vec<String>,
}

fn channel_configuration_matches_ignoring_detection_state(
    left: &ChannelConfig,
    right: &ChannelConfig,
) -> bool {
    let mut left = left.clone();
    let mut right = right.clone();
    clear_channel_detection_state(&mut left);
    clear_channel_detection_state(&mut right);
    channel_configs_match(&left, &right)
}

fn clear_channel_detection_state(channel: &mut ChannelConfig) {
    // These fields are refreshed by both the periodic monitor and interactive validation. They
    // are observations, not editable channel settings, so one refresh must not make another
    // refresh look like a concurrent user edit.
    channel.models.clear();
    channel.capability_profiles.clear();
    channel.detection_checks.clear();
    for binding in &mut channel.surface_bindings {
        binding.verification = SurfaceVerification::default();
        for protocol in &mut binding.protocols {
            protocol.verification = ProtocolVerification::default();
        }
    }
}

fn merge_detected_channels(
    current: &mut ClientConfig,
    original: &ClientConfig,
    detected: &ClientConfig,
    channel_ids: &[String],
) -> DetectedChannelMerge {
    let mut result = DetectedChannelMerge::default();
    for detected_channel in detected
        .channels
        .iter()
        .filter(|channel| supplier_channel_selected(&channel.id, channel_ids))
    {
        let Some(original_channel) = original
            .channels
            .iter()
            .find(|channel| channel.id == detected_channel.id)
        else {
            result.conflicted.push(detected_channel.id.clone());
            continue;
        };
        let Some(current_channel) = current
            .channels
            .iter_mut()
            .find(|channel| channel.id == detected_channel.id)
        else {
            result.conflicted.push(detected_channel.id.clone());
            continue;
        };
        if !channel_configuration_matches_ignoring_detection_state(
            current_channel,
            original_channel,
        ) {
            result.conflicted.push(detected_channel.id.clone());
            continue;
        }
        *current_channel = detected_channel.clone();
        result.committed.push(detected_channel.id.clone());
    }
    result
}

fn commit_detected_channel_updates(
    config_path: &PathBuf,
    proxy_config: &Arc<StdMutex<ClientConfig>>,
    original: &ClientConfig,
    detected: &ClientConfig,
    channel_ids: &[String],
) -> Result<(ClientConfig, DetectedChannelMerge)> {
    let (authoritative, merge) = commit_config_update(config_path, proxy_config, |current| {
        Ok(merge_detected_channels(
            current,
            original,
            detected,
            channel_ids,
        ))
    })?;
    Ok((authoritative, merge))
}

#[cfg(test)]
mod lifecycle_config_tests {
    use super::*;

    #[test]
    fn subscription_catalog_declarations_do_not_erase_verified_model_features() {
        let mut channel = default_config().channels.remove(0);
        let live = ChannelCapabilityProfile { protocol:"openai_responses".into(), model_pattern:"selected-model".into(), custom_tool:true, verification_state:"verified".into(), verified_at_unix:42, ..Default::default() };
        channel.capability_profiles = vec![live.clone()];
        let detection = ChannelDetectionResult {
            availability: None,
            models:vec!["selected-model".into()], surface_results:vec![],
            model_capability_evidence:vec![ChannelCapabilityProfile { protocol:"openai_responses".into(), tool_calls:true, verification_state:"declared".into(), ..Default::default() }],
            detection_evidence:vec![], quota_status:"unknown".into(), remaining_ratio:0.0, quota_windows:vec![], checks:vec![], warnings:vec![],
        };
        merge_channel_detection_result(&mut channel, &detection);
        assert!(channel.capability_profiles.contains(&live));
        assert!(channel.capability_profiles.iter().any(|profile| profile.model_pattern.is_empty() && profile.tool_calls));
    }

    #[test]
    fn catalog_refresh_replaces_limits_without_erasing_inference_evidence() {
        let mut channel = default_config().channels.remove(0);
        let profile = ChannelCapabilityProfile { protocol:"openai_chat".into(), model_pattern:"claude-sonnet-4-6".into(), catalog_metadata:true, context_tokens:Some(1000000), verification_state:"declared".into(), ..Default::default() };
        channel.capability_profiles = vec![profile.clone(), ChannelCapabilityProfile { catalog_metadata:false, context_tokens:None, tool_calls:true, verification_state:"verified".into(), ..profile.clone() }];
        let mut detection = ChannelDetectionResult {
            availability: None,
            models:vec!["claude-sonnet-4-6".into()], surface_results:vec![SurfaceProbeResult {
                surface:crate::surface::ApiSurface::OpenAi, protocol:crate::protocol::kind::ProtocolKind::OpenAiChat,
                surface_verification:SurfaceVerification {state:"verified".into(), ..Default::default()},
                protocol_verification:ProtocolVerification::default(), models:vec!["claude-sonnet-4-6".into()],
                model_capability_evidence:vec![], detection_evidence:vec![], checks:vec![], warnings:vec![],
            }], model_capability_evidence:vec![ChannelCapabilityProfile { context_tokens:Some(128000), ..profile }],
            detection_evidence:vec![], quota_status:"unknown".into(), remaining_ratio:0.0, quota_windows:vec![], checks:vec![], warnings:vec![],
        };
        merge_channel_detection_result(&mut channel, &detection);
        assert_eq!(crate::tool_model_metadata::channel_token_limits(&channel, "claude-sonnet-4-6").0, Some(128000));
        assert!(channel.capability_profiles.iter().any(|p| !p.catalog_metadata && p.tool_calls && p.verification_state == "verified"));
        let raw = serde_json::to_value(&channel).unwrap();
        let restored: ChannelConfig = serde_json::from_value(raw).unwrap();
        assert_eq!(crate::tool_model_metadata::channel_token_limits(&restored, "claude-sonnet-4-6").0, Some(128000));
        detection.model_capability_evidence.clear();
        detection.surface_results[0].surface_verification.state = "unknown".into();
        merge_channel_detection_result(&mut channel, &detection);
        assert!(channel.capability_profiles.iter().any(|p| p.catalog_metadata));
        detection.surface_results[0].surface_verification.state = "verified".into();
        merge_channel_detection_result(&mut channel, &detection);
        assert!(!channel.capability_profiles.iter().any(|p| p.catalog_metadata));
        assert_eq!(crate::tool_model_metadata::channel_token_limits(&channel, "claude-sonnet-4-6"), crate::tool_model_metadata::catalog_token_limits("claude-sonnet-4-6"));
    }

    fn http_channel_with_validation_evidence(checked_at_unix: i64) -> ChannelConfig {
        let mut channel = default_config().channels.remove(0);
        channel.models = vec!["validated-model".to_string()];
        let target_surface = channel.v2.default_target.surface;
        let binding = channel
            .surface_bindings
            .iter_mut()
            .find(|binding| binding.surface == target_surface)
            .expect("default surface binding");
        binding.verification = SurfaceVerification {
            state: "verified".to_string(),
            checked_at_unix,
            summary: "validation passed".to_string(),
        };
        channel
    }

    #[test]
    fn activation_reuses_validation_without_repeating_paid_checks_on_age_alone() {
        let now = 1_000_000;
        let recent = http_channel_with_validation_evidence(now - 60);
        assert_eq!(
            supplier_effective_check_mode(&recent, SupplierCheckMode::Activation, now),
            SupplierCheckMode::Cached
        );

        let stale = http_channel_with_validation_evidence(now - 7 * 24 * 60 * 60);
        assert_eq!(
            supplier_effective_check_mode(&stale, SupplierCheckMode::Activation, now),
            SupplierCheckMode::Cached
        );

        let future =
            http_channel_with_validation_evidence(now + SUPPLIER_VALIDATION_CLOCK_SKEW_SECONDS + 1);
        assert_eq!(
            supplier_effective_check_mode(&future, SupplierCheckMode::Activation, now),
            SupplierCheckMode::FullInteractive
        );

        let mut missing_models = recent;
        missing_models.models.clear();
        assert_eq!(
            supplier_effective_check_mode(&missing_models, SupplierCheckMode::Activation, now),
            SupplierCheckMode::FullInteractive
        );
        assert_eq!(
            supplier_effective_check_mode(&stale, SupplierCheckMode::FullInteractive, now),
            SupplierCheckMode::FullInteractive
        );
        assert_eq!(
            supplier_effective_check_mode(&stale, SupplierCheckMode::Snapshot, now),
            SupplierCheckMode::Snapshot
        );
    }

    #[tokio::test]
    async fn activation_of_verified_channel_is_local_and_preserves_evidence_age() {
        let checked_at = now_unix() - 7 * 24 * 60 * 60;
        let mut channel = http_channel_with_validation_evidence(checked_at);
        channel.id = "cached-activation-no-network".into();
        // No listener: any accidental catalog or generation request would fail.
        for binding in &mut channel.surface_bindings {
            binding.base_url = "http://127.0.0.1:0/v1".into();
        }
        let (outcome, attempts) = crate::upstream_transport::with_probe_budget(
            0,
            validate_supplier_channel(channel.clone(), SupplierCheckMode::Activation),
        )
        .await;
        let (restored, health) = outcome.expect("saved evidence should restore without I/O");
        assert_eq!(health.status, "available");
        assert_eq!(attempts, 0);
        assert_eq!(restored.models, channel.models);
        assert_eq!(restored.surface_bindings, channel.surface_bindings);
    }

    #[test]
    fn transient_surface_probe_does_not_erase_decisive_evidence() {
        for (name, capability) in [
            ("surface_operation_family", "openai.files_uploads"),
            ("responses_operation", "openai.responses_compact"),
            ("responses_operation", "openai.responses_input_tokens"),
            ("responses_operation", "openai.responses_websocket"),
        ] {
            let decisive = ChannelDetectionCheck {
                name: name.to_string(),
                status: "verified".to_string(),
                checked_at_unix: 42,
                protocol: "openai_responses".to_string(),
                capability: capability.to_string(),
                message: "HTTP 200".to_string(),
            };
            for status in ["unknown", "credential_limited"] {
                let incoming = ChannelDetectionCheck {
                    status: status.to_string(),
                    ..decisive.clone()
                };
                assert!(!detection_evidence_replaces_existing(&decisive, &incoming));
            }
            let unsupported = ChannelDetectionCheck {
                status: "unsupported".to_string(),
                ..decisive.clone()
            };
            assert!(detection_evidence_replaces_existing(
                &decisive,
                &unsupported
            ));
            assert!(detection_evidence_replaces_existing(&unsupported, &decisive));
        }
    }

    fn health_with_http_version(channel_id: &str, version: &str) -> SupplierChannelHealth {
        let mut channel = default_config().channels[0].clone();
        channel.id = channel_id.to_string();
        let mut health = failed_supplier_channel_health(&channel, String::new(), 12, 1);
        health.upstream_http_version = version.to_string();
        health
    }

    #[test]
    fn detected_channel_merge_updates_only_an_unchanged_channel_snapshot() {
        let original = default_config();
        let mut detected = original.clone();
        detected.channels[0].models = vec!["detected-model".to_string()];

        let mut unchanged = original.clone();
        unchanged.allow_model_equivalence = true;
        let merged = merge_detected_channels(&mut unchanged, &original, &detected, &[]);
        assert_eq!(merged.committed, vec![original.channels[0].id.clone()]);
        assert!(merged.conflicted.is_empty());
        assert!(unchanged.allow_model_equivalence);
        assert_eq!(unchanged.channels[0].models, detected.channels[0].models);

        let mut edited = original.clone();
        edited.channels[0].name = "user-edited-while-detecting".to_string();
        let merged = merge_detected_channels(&mut edited, &original, &detected, &[]);
        assert!(merged.committed.is_empty());
        assert_eq!(merged.conflicted, vec![original.channels[0].id.clone()]);
        assert_eq!(edited.channels[0].name, "user-edited-while-detecting");
        assert_ne!(edited.channels[0].models, detected.channels[0].models);
    }

    #[test]
    fn detected_channel_merge_rebases_over_a_concurrent_detection_refresh() {
        let original = default_config();
        let mut interactive = original.clone();
        interactive.channels[0].models = vec!["interactive-model".to_string()];

        let mut refreshed = original.clone();
        refreshed.channels[0].models = vec!["background-model".to_string()];
        refreshed.channels[0].detection_checks = vec![ChannelDetectionCheck {
            name: "background-refresh".to_string(),
            status: "ok".to_string(),
            ..ChannelDetectionCheck::default()
        }];

        let merged = merge_detected_channels(&mut refreshed, &original, &interactive, &[]);

        assert_eq!(merged.committed, vec![original.channels[0].id.clone()]);
        assert!(merged.conflicted.is_empty());
        assert_eq!(refreshed.channels[0].models, interactive.channels[0].models);
    }

    #[test]
    fn snapshot_without_http_version_preserves_the_last_observation() {
        let previous = vec![health_with_http_version("channel-a", "http3")];
        let mut refreshed = vec![health_with_http_version("channel-a", "")];

        preserve_last_observed_http_versions(&mut refreshed, &previous);

        assert_eq!(refreshed[0].upstream_http_version, "http3");
    }

    #[test]
    fn a_new_http_version_observation_replaces_the_previous_one() {
        let previous = vec![health_with_http_version("channel-a", "http3")];
        let mut refreshed = vec![health_with_http_version("channel-a", "http2")];

        preserve_last_observed_http_versions(&mut refreshed, &previous);

        assert_eq!(refreshed[0].upstream_http_version, "http2");
    }

    #[test]
    fn a_channel_without_any_http_version_observation_stays_empty() {
        let mut refreshed = vec![health_with_http_version("channel-a", "")];

        preserve_last_observed_http_versions(&mut refreshed, &[]);

        assert!(refreshed[0].upstream_http_version.is_empty());
    }
}

#[derive(Clone, Copy)]
pub(crate) enum ChannelMonitorScope {
    PlatformSupplier,
    LocalOnly,
}

pub(crate) fn monitored_channel_ids(
    cfg: &ClientConfig,
    requested_channel_ids: &[String],
    scope: ChannelMonitorScope,
) -> Vec<String> {
    cfg.channels
        .iter()
        .filter(|channel| {
            channel.enabled
                && supplier_channel_selected(&channel.id, requested_channel_ids)
                && match scope {
                    ChannelMonitorScope::PlatformSupplier => {
                        crate::source_driver::channel_is_platform_shareable(channel)
                    }
                    ChannelMonitorScope::LocalOnly => {
                        !crate::source_driver::channel_is_platform_shareable(channel)
                    }
                }
        })
        .map(|channel| channel.id.clone())
        .collect()
}

async fn refresh_monitored_channel_health(
    config_path: &PathBuf,
    proxy_config: &Arc<StdMutex<ClientConfig>>,
    requested_channel_ids: &[String],
    scope: ChannelMonitorScope,
) -> Result<Option<(ClientConfig, Vec<String>, Vec<SupplierChannelHealth>)>> {
    let mut cfg = load_config_from_path(config_path)?;
    let channel_ids = monitored_channel_ids(&cfg, requested_channel_ids, scope);
    if channel_ids.is_empty() {
        return Ok(None);
    }

    let original_cfg = cfg.clone();
    let observed_health = match scope {
        ChannelMonitorScope::PlatformSupplier => {
            prepare_supplier_channels_snapshot(&mut cfg, &channel_ids).await?
        }
        ChannelMonitorScope::LocalOnly => {
            prepare_local_only_channels_snapshot(&mut cfg, &channel_ids).await?
        }
    };
    let mut health = successful_supplier_monitor_health(&observed_health);
    if health.is_empty() {
        // A generic snapshot failure proves only that this metadata sample failed.
        // Preserve the last actionable health until a real request or a structured
        // health signal provides stronger evidence.
        return Ok(None);
    }

    match commit_detected_channel_updates(
        config_path,
        proxy_config,
        &original_cfg,
        &cfg,
        &channel_ids,
    ) {
        Ok((authoritative, merge)) => {
            health.retain(|item| merge.committed.iter().any(|id| id == &item.channel_id));
            Ok(Some((authoritative, channel_ids, health)))
        }
        Err(error) => {
            log::warn!("[const-api][supplier] channel refresh commit deferred: {error:#}");
            Ok(None)
        }
    }
}

pub(crate) fn apply_monitored_channel_health(
    authoritative: &ClientConfig,
    channel_ids: &[String],
    health: &[SupplierChannelHealth],
    register_update: Option<&mpsc::Sender<SupplierRegistrationUpdate>>,
    local_channel_readiness: &Arc<StdMutex<HashMap<String, bool>>>,
    local_model_quota_routes: &Arc<StdMutex<HashMap<String, LocalModelQuotaRouteState>>>,
    transport_state: &Arc<StdMutex<SupplierTransportSnapshot>>,
) {
    if health.is_empty() {
        return;
    }
    update_local_channel_readiness(
        local_channel_readiness,
        local_model_quota_routes,
        health,
    );
    update_supplier_channel_health_state(transport_state, health);
    if let Some(tx) = register_update {
        let suppliers = supplier_registration_snapshot(authoritative, channel_ids, health);
        send_register_update(tx, suppliers);
    }
}

pub(crate) async fn refresh_local_only_channel_health_once(
    state: &AppState,
) -> Result<(), String> {
    let has_enabled_local_only_channel = state
        .proxy_config
        .lock()
        .map_err(|_| "proxy live config lock poisoned".to_string())?
        .channels
        .iter()
        .any(|channel| {
            channel.enabled && !crate::source_driver::channel_is_platform_shareable(channel)
        });
    if !has_enabled_local_only_channel {
        return Ok(());
    }
    let transport_state = {
        let runtime = state.supplier.lock().await;
        runtime.transport_state.clone()
    };
    match refresh_monitored_channel_health(
        &state.config_path,
        &state.proxy_config,
        &[],
        ChannelMonitorScope::LocalOnly,
    )
    .await
    .map_err(|error| error.to_string())?
    {
        Some((authoritative, channel_ids, health)) => apply_monitored_channel_health(
            &authoritative,
            &channel_ids,
            &health,
            None,
            &state.local_channel_readiness,
            &state.local_model_quota_routes,
            &transport_state,
        ),
        None => {}
    }
    Ok(())
}

pub(crate) async fn run_local_only_channel_health_loop(state: AppState) {
    let mut refresh_tick = tokio::time::interval(supplier_channel_report_interval());
    refresh_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        refresh_tick.tick().await;
        if let Err(error) = refresh_local_only_channel_health_once(&state).await {
            log::warn!("[const-api][supplier] local-only channel refresh failed: {error}");
        }
    }
}

pub(crate) async fn monitor_supplier_channels(
    config_path: PathBuf,
    channel_ids: Vec<String>,
    register_update: Option<mpsc::Sender<SupplierRegistrationUpdate>>,
    proxy_config: Arc<StdMutex<ClientConfig>>,
    local_channel_readiness: Arc<StdMutex<HashMap<String, bool>>>,
    local_model_quota_routes: Arc<StdMutex<HashMap<String, LocalModelQuotaRouteState>>>,
    transport_state: Arc<StdMutex<SupplierTransportSnapshot>>,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut refresh_tick = tokio::time::interval(supplier_channel_report_interval());
    refresh_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Channel state is sampled once before the agent is started. Consume interval's immediate
    // tick so the recurring refresh remains a fixed 30-second cadence instead of sending twice.
    refresh_tick.tick().await;
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = refresh_tick.tick() => {
                match refresh_monitored_channel_health(
                    &config_path,
                    &proxy_config,
                    &channel_ids,
                    ChannelMonitorScope::PlatformSupplier,
                ).await {
                    Ok(Some((authoritative, refreshed_ids, health))) => {
                        apply_monitored_channel_health(
                            &authoritative,
                            &refreshed_ids,
                            &health,
                            register_update.as_ref(),
                            &local_channel_readiness,
                            &local_model_quota_routes,
                            &transport_state,
                        );
                    }
                    Ok(None) => {}
                    Err(err) => eprintln!("supplier channel refresh failed: {err}"),
                }
            }
        }
    }
}

pub(crate) fn successful_supplier_monitor_health(
    health: &[SupplierChannelHealth],
) -> Vec<SupplierChannelHealth> {
    health
        .iter()
        .filter(|item| {
            !(item.status == "error"
                && item.safety_state == "error"
                && item.credential_status == "unknown"
                && item.quota_status == "unknown")
        })
        .cloned()
        .collect()
}

pub(crate) fn supplier_registration_snapshot(
    cfg: &ClientConfig,
    channel_ids: &[String],
    health: &[SupplierChannelHealth],
) -> Vec<SupplierConfig> {
    let health_by_channel: HashMap<&str, &SupplierChannelHealth> = health
        .iter()
        .map(|item| (item.channel_id.as_str(), item))
        .collect();
    cfg.channels
        .iter()
        .filter(|channel| {
            channel.enabled
                && crate::source_driver::channel_is_platform_shareable(channel)
                && supplier_channel_selected(&channel.id, channel_ids)
                && (health.is_empty() || health_by_channel.contains_key(channel.id.as_str()))
        })
        .map(|channel| {
            let mut supplier = supplier_from_channel_for_node(channel);
            if let Some(item) = health_by_channel.get(channel.id.as_str()) {
                if supplier.registration_health_status.as_deref() != Some("blocked") {
                    supplier.registration_health_status = Some(
                        match item.status.as_str() {
                            "error" | "unavailable" | "failed" => "unavailable",
                            "risk_blocked" | "auth_error" | "disabled" => "blocked",
                            "cooldown" => "degraded",
                            "available" | "active" => "available",
                            other => other,
                        }
                        .to_string(),
                    );
                }
                supplier.registration_quota_status = Some(item.quota_status.clone());
            }
            supplier
        })
        .collect()
}

pub(crate) fn inherit_supplier_shared_transport(
    state: &Arc<StdMutex<SupplierTransportSnapshot>>,
    existing_node_ids: &[String],
    new_node_ids: &[String],
) {
    let Ok(mut snapshot) = state.lock() else {
        return;
    };
    let source = existing_node_ids
        .iter()
        .filter_map(|node_id| snapshot.nodes.get(node_id))
        .filter(|node| !node.connected_transport.is_empty() || !node.active_transport.is_empty())
        .max_by_key(|node| node.connected_changed_at_unix.max(node.changed_at_unix))
        .cloned();
    let Some(source) = source else {
        return;
    };
    for node_id in new_node_ids {
        if snapshot.nodes.contains_key(node_id) {
            continue;
        }
        let target = snapshot.nodes.entry(node_id.clone()).or_default();
        target.connected_transport = source.connected_transport.clone();
        target.connected_changed_at_unix = source.connected_changed_at_unix;
        target.active_transport = source.active_transport.clone();
        target.last_transport_error = source.last_transport_error.clone();
        target.changed_at_unix = source.changed_at_unix;
        target.last_register_ack_unix = 0;
        target.route_status = None;
    }
}

pub(crate) fn invalidate_supplier_registration_acks(
    state: &Arc<StdMutex<SupplierTransportSnapshot>>,
    node_ids: &[String],
) {
    if let Ok(mut snapshot) = state.lock() {
        for node_id in node_ids {
            if let Some(node) = snapshot.nodes.get_mut(node_id) {
                node.last_register_ack_unix = 0;
            }
        }
    }
}

pub(crate) fn supplier_channel_report_interval() -> Duration {
    Duration::from_secs(30)
}

pub(crate) fn requested_supplier_channel_ids(
    channel_id: Option<String>,
    channel_ids: Option<Vec<String>>,
) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(ids) = channel_ids {
        for id in ids {
            let trimmed = id.trim();
            if !trimmed.is_empty() && !out.iter().any(|item| item == trimmed) {
                out.push(trimmed.to_string());
            }
        }
    }
    if out.is_empty() {
        if let Some(id) = channel_id {
            let trimmed = id.trim();
            if !trimmed.is_empty() {
                out.push(trimmed.to_string());
            }
        }
    }
    out
}

pub(crate) fn supplier_channel_selected(channel_id: &str, selected_ids: &[String]) -> bool {
    selected_ids.is_empty() || selected_ids.iter().any(|id| id == channel_id)
}

pub(crate) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

pub(crate) fn now_unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}

pub(crate) fn update_supplier_transport_state(
    state: &Arc<StdMutex<SupplierTransportSnapshot>>,
    node_id: &str,
    active_transport: &str,
    last_transport_error: &str,
) {
    if let Ok(mut snapshot) = state.lock() {
        let node_key = if node_id.trim().is_empty() {
            "default"
        } else {
            node_id.trim()
        }
        .to_string();
        {
            let node = snapshot.nodes.entry(node_key).or_default();
            node.active_transport = active_transport.to_string();
            node.last_transport_error = last_transport_error.to_string();
            node.changed_at_unix = now_unix();
        }
        refresh_supplier_transport_error(&mut snapshot);
    }
}

fn refresh_supplier_transport_error(snapshot: &mut SupplierTransportSnapshot) {
    snapshot.last_transport_error = snapshot
        .nodes
        .values()
        .filter(|node| !node.last_transport_error.trim().is_empty())
        .max_by_key(|node| node.changed_at_unix)
        .map(|node| node.last_transport_error.clone())
        .unwrap_or_default();
}

pub(crate) fn update_local_channel_readiness(
    readiness: &Arc<StdMutex<HashMap<String, bool>>>,
    model_quota_routes: &Arc<StdMutex<HashMap<String, LocalModelQuotaRouteState>>>,
    health: &[SupplierChannelHealth],
) {
    if let Ok(mut readiness) = readiness.lock() {
        for item in health {
            readiness.insert(item.channel_id.clone(), supplier_health_allows_local(item));
        }
    }
    if let Ok(mut routes) = model_quota_routes.lock() {
        let now = now_unix();
        let reported_channels = health
            .iter()
            .map(|item| item.channel_id.trim())
            .filter(|channel_id| !channel_id.is_empty())
            .collect::<std::collections::HashSet<_>>();
        let mut reported_model_keys = std::collections::HashSet::new();
        for item in health {
            let mut models = item
                .quota_windows
                .iter()
                .filter_map(|window| window.model.as_deref())
                .map(str::trim)
                .filter(|model| !model.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>();
            models.sort_by_key(|model| normalize_model_name(model));
            models
                .dedup_by(|left, right| normalize_model_name(left) == normalize_model_name(right));
            for model in models {
                let model_windows = item
                    .quota_windows
                    .iter()
                    .filter(|window| {
                        window.model.as_deref().is_some_and(|candidate| {
                            normalize_model_name(candidate) == normalize_model_name(&model)
                        })
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                let Some(summary) = scoped_quota_summary(&model_windows) else {
                    continue;
                };
                let key = model_quota_route_key(&item.channel_id, &model);
                reported_model_keys.insert(key.clone());
                let existing = routes.get(&key).copied().unwrap_or_default();
                if summary.status == "exhausted" {
                    let reset_at = summary.representative.reset_at_unix;
                    routes.insert(
                        key,
                        LocalModelQuotaRouteState {
                            cooldown_until_unix: existing.cooldown_until_unix.max(
                                if reset_at > now {
                                    reset_at
                                } else {
                                    now + 2 * supplier_channel_report_interval().as_secs() as i64
                                },
                            ),
                            failed_at_unix: existing.failed_at_unix,
                            ..existing
                        },
                    );
                } else {
                    let checked_at_unix = model_windows
                        .iter()
                        .map(|window| window.checked_at_unix)
                        .max()
                        .unwrap_or_default();
                    if !existing.available_at(now)
                        && existing.failed_at_unix > 0
                        && checked_at_unix <= existing.failed_at_unix
                    {
                        // A cached quota snapshot must not erase a newer
                        // runtime 429. A successful inference clears it
                        // immediately; otherwise wait for a genuinely newer
                        // quota observation or the cooldown expiry.
                        continue;
                    }
                    // Presence with a zero value records that this channel has
                    // an exact model quota scope. Runtime 429s can then be
                    // isolated to this model instead of disabling the channel.
                    routes.insert(
                        key,
                        LocalModelQuotaRouteState {
                            cooldown_until_unix: 0,
                            failed_at_unix: 0,
                            ..existing
                        },
                    );
                }
            }
        }
        routes.retain(|key, state| {
            key.split_once('\0')
                .map(|(channel_id, _)| {
                    state.has_runtime_failure()
                        || !reported_channels.contains(channel_id)
                        || reported_model_keys.contains(key)
                })
                .unwrap_or(false)
        });
    }
}

pub(crate) fn update_supplier_channel_health_state(
    state: &Arc<StdMutex<SupplierTransportSnapshot>>,
    health: &[SupplierChannelHealth],
) {
    if let Ok(mut snapshot) = state.lock() {
        let previous = snapshot
            .channel_health
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut merged = health.to_vec();
        preserve_last_observed_http_versions(&mut merged, &previous);
        for item in merged {
            snapshot
                .channel_health
                .insert(item.channel_id.clone(), item);
        }
    }
}

fn preserve_last_observed_http_versions(
    health: &mut [SupplierChannelHealth],
    previous: &[SupplierChannelHealth],
) {
    let previous_versions: HashMap<&str, &str> = previous
        .iter()
        .filter_map(|item| {
            let version = item.upstream_http_version.trim();
            (!version.is_empty()).then_some((item.channel_id.as_str(), version))
        })
        .collect();
    for item in health {
        if item.upstream_http_version.trim().is_empty() {
            if let Some(version) = previous_versions.get(item.channel_id.as_str()) {
                item.upstream_http_version = (*version).to_string();
            }
        }
    }
}

fn supplier_health_allows_local(health: &SupplierChannelHealth) -> bool {
    (health.status == "available" || health.safety_state == "quota_low")
        && !matches!(
            health.safety_state.as_str(),
            "cooldown"
                | "auth_refreshing"
                | "quota_exhausted"
                | "auth_error"
                | "risk_blocked"
                | "disabled"
                | "error"
        )
}

pub(crate) fn update_supplier_connected_transport_state(
    state: &Arc<StdMutex<SupplierTransportSnapshot>>,
    node_id: &str,
    connected_transport: &str,
) {
    if let Ok(mut snapshot) = state.lock() {
        let node_key = if node_id.trim().is_empty() {
            "default"
        } else {
            node_id.trim()
        }
        .to_string();
        let node = snapshot.nodes.entry(node_key).or_default();
        if node.connected_transport != connected_transport {
            node.last_register_ack_unix = 0;
        }
        node.connected_transport = connected_transport.to_string();
        node.connected_changed_at_unix = now_unix();
    }
}

pub(crate) fn update_supplier_agent_transport_state(
    state: &Arc<StdMutex<SupplierTransportSnapshot>>,
    config: &SupplierAgentConfig,
    active_transport: &str,
    last_transport_error: &str,
) {
    for unit_id in config.supplier_unit_ids() {
        update_supplier_transport_state(state, &unit_id, active_transport, last_transport_error);
    }
}

pub(crate) fn update_supplier_agent_connected_transport_state(
    state: &Arc<StdMutex<SupplierTransportSnapshot>>,
    config: &SupplierAgentConfig,
    connected_transport: &str,
) {
    for unit_id in config.supplier_unit_ids() {
        update_supplier_connected_transport_state(state, &unit_id, connected_transport);
    }
}

pub(crate) fn update_supplier_route_state(
    state: &Arc<StdMutex<SupplierTransportSnapshot>>,
    mut route_status: SupplierNodeRouteStatus,
) {
    let key = route_status_key(&route_status);
    if key.is_empty() {
        return;
    }
    if let Ok(mut snapshot) = state.lock() {
        let node = snapshot.nodes.entry(key).or_default();
        if node
            .route_status
            .as_ref()
            .is_some_and(|current| supplier_route_status_semantically_equal(current, &route_status))
        {
            return;
        }
        if route_status.updated_at_unix <= 0 {
            route_status.updated_at_unix = now_unix();
        }
        node.route_status = Some(route_status);
    }
}

pub(crate) fn clear_supplier_registration_lifecycle_route_state(
    state: &Arc<StdMutex<SupplierTransportSnapshot>>,
    supplier_unit_id: &str,
) -> bool {
    let supplier_unit_id = supplier_unit_id.trim();
    if supplier_unit_id.is_empty() {
        return false;
    }
    if let Ok(mut snapshot) = state.lock() {
        let Some(node) = snapshot.nodes.get_mut(supplier_unit_id) else {
            return false;
        };
        let should_clear = node.route_status.as_ref().is_some_and(|status| {
            let state = status.state.trim().to_ascii_lowercase();
            let reason = status.reason.trim().to_ascii_lowercase();
            matches!(state.as_str(), "removed" | "deleted")
                || matches!(reason.as_str(), "removed" | "unregistered")
        });
        if should_clear {
            node.route_status = None;
            return true;
        }
    }
    false
}

fn supplier_route_status_semantically_equal(
    current: &SupplierNodeRouteStatus,
    next: &SupplierNodeRouteStatus,
) -> bool {
    let mut current = current.clone();
    let mut next = next.clone();
    current.updated_at_unix = 0;
    next.updated_at_unix = 0;
    current == next
}

pub(crate) fn update_supplier_model_admission_route_state(
    state: &Arc<StdMutex<SupplierTransportSnapshot>>,
    mut route_status: SupplierNodeRouteStatus,
) {
    let key = route_status_key(&route_status);
    if key.is_empty() {
        return;
    }
    if let Ok(mut snapshot) = state.lock() {
        if snapshot
            .model_admission_statuses
            .get(&key)
            .is_some_and(|current| supplier_route_status_semantically_equal(current, &route_status))
        {
            return;
        }
        if route_status.updated_at_unix <= 0 {
            route_status.updated_at_unix = now_unix();
        }
        snapshot.model_admission_statuses.insert(key, route_status);
    }
}

pub(crate) fn clear_supplier_model_admission_route_state(
    state: &Arc<StdMutex<SupplierTransportSnapshot>>,
    node_id: &str,
) {
    let node_id = node_id.trim();
    if node_id.is_empty() {
        return;
    }
    if let Ok(mut snapshot) = state.lock() {
        snapshot.model_admission_statuses.remove(node_id);
    }
}

fn route_status_key(route_status: &SupplierNodeRouteStatus) -> String {
    let supplier_unit_id = route_status.supplier_unit_id.trim();
    if !supplier_unit_id.is_empty() {
        return supplier_unit_id.to_string();
    }
    let node_id = route_status.node_id.trim();
    if !node_id.is_empty() {
        return node_id.to_string();
    }
    route_status.channel_id.trim().to_string()
}

pub(crate) fn clear_supplier_transport_state(state: &Arc<StdMutex<SupplierTransportSnapshot>>) {
    if let Ok(mut snapshot) = state.lock() {
        snapshot.nodes.clear();
        snapshot.model_admission_statuses.clear();
        snapshot.channel_health.clear();
        snapshot.last_transport_error.clear();
    }
}

pub(crate) fn clear_supplier_transport_nodes(
    state: &Arc<StdMutex<SupplierTransportSnapshot>>,
    node_ids: &[String],
) {
    if let Ok(mut snapshot) = state.lock() {
        for node_id in node_ids {
            snapshot.nodes.remove(node_id);
            snapshot.model_admission_statuses.remove(node_id);
        }
        refresh_supplier_transport_error(&mut snapshot);
    }
}

pub(crate) fn clear_supplier_channel_health_state(
    state: &Arc<StdMutex<SupplierTransportSnapshot>>,
    channel_ids: &[String],
) {
    if let Ok(mut snapshot) = state.lock() {
        for channel_id in channel_ids {
            snapshot.channel_health.remove(channel_id);
        }
    }
}

pub(crate) fn stop_supplier_channel_agents(runtime: &mut SupplierRuntime, channel_ids: &[String]) {
    let mut removed_nodes = Vec::new();
    let channel_set: std::collections::HashSet<String> = channel_ids.iter().cloned().collect();
    let mut remove_endpoints = Vec::new();
    for (endpoint_key, handle) in runtime.channel_agents.iter_mut() {
        let before_nodes = handle.node_ids.clone();
        let mut removed_suppliers = handle
            .suppliers
            .iter()
            .filter(|supplier| channel_set.contains(&supplier.channel_id))
            .cloned()
            .map(|mut supplier| {
                supplier.registration_removed = true;
                supplier
            })
            .collect::<Vec<_>>();
        handle
            .suppliers
            .retain(|supplier| !channel_set.contains(&supplier.channel_id));
        handle
            .channel_ids
            .retain(|channel_id| !channel_set.contains(channel_id));
        handle.node_ids = handle
            .suppliers
            .iter()
            .map(|supplier| supplier_unit_id(&handle.client_id, &supplier.channel_id))
            .collect();
        removed_nodes.extend(
            before_nodes
                .into_iter()
                .filter(|node_id| !handle.node_ids.contains(node_id)),
        );
        if handle.suppliers.is_empty() {
            remove_endpoints.push(endpoint_key.clone());
        } else {
            let mut update = handle.suppliers.clone();
            update.append(&mut removed_suppliers);
            send_register_update(&handle.register_update, update);
        }
    }
    for endpoint_key in remove_endpoints {
        if let Some(handle) = runtime.channel_agents.remove(&endpoint_key) {
            let _ = handle.agent_shutdown.send(());
            let _ = handle.monitor_shutdown.send(());
        }
    }
    runtime
        .channels
        .retain(|channel| !channel_ids.contains(&channel.channel_id));
    clear_supplier_channel_health_state(&runtime.transport_state, channel_ids);
    if removed_nodes.is_empty() {
        return;
    }
    runtime
        .nodes
        .retain(|node_id| !removed_nodes.contains(node_id));
    clear_supplier_transport_nodes(&runtime.transport_state, &removed_nodes);
    runtime.node_id = runtime.nodes.first().cloned().unwrap_or_default();
}

fn update_supplier_runtime_after_channel_stop(runtime: &mut SupplierRuntime, keep_starting: bool) {
    if !runtime.channel_agents.is_empty() {
        return;
    }
    runtime.running = false;
    runtime.starting = keep_starting;
    runtime.transport_preference.clear();
    runtime.server_ws_url.clear();
    runtime.server_quic_url.clear();
    runtime.last_error.clear();
}

pub(crate) fn notify_supplier_runtime_offline(runtime: &SupplierRuntime) -> bool {
    let mut sent = false;
    for handle in runtime.channel_agents.values() {
        send_register_update(&handle.register_update, Vec::new());
        sent = true;
    }
    sent
}

pub(crate) async fn stop_supplier_inner(state: &AppState) -> Result<SupplierStatus, String> {
    let mut runtime = state.supplier.lock().await;
    for (_, handle) in runtime.channel_agents.drain() {
        let _ = handle.agent_shutdown.send(());
        let _ = handle.monitor_shutdown.send(());
    }
    runtime.running = false;
    runtime.starting = false;
    runtime.transport_preference.clear();
    clear_supplier_transport_state(&runtime.transport_state);
    runtime.nodes.clear();
    runtime.channels.clear();
    runtime.last_error.clear();
    Ok(runtime.status())
}

pub(crate) async fn notify_supplier_offline_inner(
    state: &AppState,
) -> Result<SupplierStatus, String> {
    let (status, sent) = {
        let runtime = state.supplier.lock().await;
        (runtime.status(), notify_supplier_runtime_offline(&runtime))
    };
    if sent {
        tokio::time::sleep(Duration::from_millis(260)).await;
    }
    Ok(status)
}

pub(crate) async fn stop_supplier_channel_inner(
    state: &AppState,
    channel_id: String,
) -> Result<SupplierStatus, String> {
    let channel_id = channel_id.trim().to_string();
    if channel_id.is_empty() {
        return Err("channel_id is required".to_string());
    }
    let mut runtime = state.supplier.lock().await;
    stop_supplier_channel_agents(&mut runtime, &[channel_id]);
    update_supplier_runtime_after_channel_stop(&mut runtime, false);
    Ok(runtime.status())
}

pub(crate) async fn refresh_supplier_registration_inner(
    state: &AppState,
    channel_id: Option<&str>,
) -> Result<SupplierStatus, String> {
    let mut runtime = state.supplier.lock().await;
    // Read after acquiring the runtime lock: a delayed refresh must not publish
    // a config snapshot older than another save. No disk/network/probe here.
    let cfg = state
        .proxy_config
        .lock()
        .map_err(|err| err.to_string())?
        .clone();
    refresh_supplier_registration_from_config(&mut runtime, &cfg, channel_id);
    Ok(runtime.status())
}

pub(crate) fn refresh_supplier_registration_from_config(
    runtime: &mut SupplierRuntime,
    cfg: &ClientConfig,
    channel_id: Option<&str>,
) {
    if !runtime.running {
        return;
    }
    let health = runtime.status().channels;
    for handle in runtime.channel_agents.values_mut() {
        let mut suppliers = Vec::new();
        for previous in &mut handle.suppliers {
            if channel_id.is_some_and(|id| id != previous.channel_id) {
                continue;
            }
            let Some(channel) = cfg
                .channels
                .iter()
                .find(|channel| channel.id == previous.channel_id)
            else {
                continue;
            };
            if !channel.enabled
                || !crate::source_driver::channel_is_platform_shareable(channel)
                || channel.server_ws_url != previous.server_ws_url
                || channel.server_quic_url != previous.server_quic_url
                || channel.node_id != previous.node_id
                || !crate::config::channel_detection_inputs_match(
                    channel,
                    &channel_from_supplier(previous.channel_id.clone(), previous),
                )
            {
                continue;
            }
            let observed = health
                .iter()
                .filter(|item| item.channel_id == channel.id)
                .cloned()
                .collect::<Vec<_>>();
            let Some(mut next) =
                supplier_registration_snapshot(cfg, &[channel.id.clone()], &observed).pop()
            else {
                continue;
            };
            // A policy edit cannot rehabilitate a failed route. If no runtime
            // observation exists, retain the agent's last registration gates.
            if observed.is_empty() {
                if next.registration_health_status.is_none() {
                    next.registration_health_status = previous.registration_health_status.clone();
                }
                next.registration_quota_status = previous.registration_quota_status.clone();
            }
            *previous = next.clone();
            suppliers.push(next);
        }
        if suppliers.is_empty() {
            continue;
        }
        send_register_update(&handle.register_update, suppliers);
    }
    let update_limits = |health: &mut SupplierChannelHealth| {
        if channel_id.is_some_and(|id| id != health.channel_id) {
            return;
        }
        if let Some(channel) = cfg
            .channels
            .iter()
            .find(|channel| channel.id == health.channel_id)
        {
            health.name = channel.name.clone();
            health.max_concurrency = channel.max_concurrency();
            health.rpm_limit = channel.subscription.rpm_limit;
            health.daily_limit = channel.subscription.daily_request_limit;
        }
    };
    for health in &mut runtime.channels {
        update_limits(health);
    }
    if let Ok(mut transport) = runtime.transport_state.lock() {
        for health in transport.channel_health.values_mut() {
            update_limits(health);
        }
    }
}

pub(crate) async fn supplier_status_inner(state: &AppState) -> Result<SupplierStatus, String> {
    let mut status = state.supplier.lock().await.status();
    // Reuse the existing local status read, including when the supplier is stopped.
    // Do not probe, reload credentials, or write UI observations into channel config.
    if let Ok(config) = state.proxy_config.lock() {
        status.availability_groups = Some(
            config
                .channels
                .iter()
                .map(|channel| (channel.id.clone(), availability::groups_for_channel(channel)))
                .collect(),
        );
    }
    Ok(status)
}

pub(crate) async fn record_supplier_channel_upstream_observation(
    state: &AppState,
    channel_id: &str,
    result: &ChannelUpstreamTestResult,
    failure_generation: u64,
) {
    if (200..300).contains(&result.http_status) {
        if let Ok(mut routes) = state.local_model_quota_routes.lock() {
            if let Some(route) = routes.get_mut(&model_quota_route_key(channel_id, &result.model)) {
                // Only this checked model is proven; an older check cannot
                // erase failures that arrived while it was still running.
                if route.runtime_failure_sequence < failure_generation {
                    route.record_success();
                }
            }
        }
        if let Ok(mut readiness) = state.local_channel_readiness.lock() {
            readiness.insert(channel_id.to_string(), true);
        }
    }
    let version = result.upstream_http_version.trim();
    let checked_at_unix = now_unix();
    let transport_state = {
        let mut runtime = state.supplier.lock().await;
        if let Some(health) = runtime
            .channels
            .iter_mut()
            .find(|item| item.channel_id == channel_id)
        {
            health.latency_ms = result.latency_ms;
            health.checked_at_unix = checked_at_unix;
            if !version.is_empty() {
                health.upstream_http_version = version.to_string();
            }
        }
        runtime.transport_state.clone()
    };
    if let Ok(mut snapshot) = transport_state.lock() {
        if let Some(health) = snapshot.channel_health.get_mut(channel_id) {
            health.latency_ms = result.latency_ms;
            health.checked_at_unix = checked_at_unix;
            if !version.is_empty() {
                health.upstream_http_version = version.to_string();
            }
        }
    };
}
