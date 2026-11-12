//! Recovery uses evidence from existing user checks, not extra inference or polling.
use super::*;
use std::cell::RefCell;

#[derive(Clone, Default, serde::Serialize)]
pub(crate) struct HealthObservation {
    pub(crate) token: String,
    pub(crate) reachable: bool,
    pub(crate) models: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    quota_windows: Vec<QuotaWindow>,
}

tokio::task_local! {
    static CHECK: RefCell<HealthObservation>;
}

pub(crate) fn record_metadata_success() {
    let _ = CHECK.try_with(|check| check.borrow_mut().reachable = true);
}

pub(crate) fn record_quota_success(windows: &[QuotaWindow]) {
    if windows.is_empty() {
        return;
    }
    let _ = CHECK.try_with(|check| {
        let mut check = check.borrow_mut();
        check.reachable = true;
        merge_official_quota_windows(&mut check.quota_windows, windows);
    });
}

pub(crate) fn record_model_success(model: &str) {
    let _ = CHECK.try_with(|check| {
        let mut check = check.borrow_mut();
        check.reachable = true;
        if check.models.len() < 64 && !check.models.iter().any(|item| item == model) {
            check.models.push(model.to_string());
        }
    });
}

pub(crate) fn check_channel<T>(
    state: &AppState,
    channel: &ChannelConfig,
    check: impl std::future::Future<Output = Result<T, String>>,
) -> impl std::future::Future<Output = Result<T, String>> {
    // Box before constructing the recovery/task-local future. Boxing inside an
    // async fn would still embed the original probe in its initial state; Tauri's
    // IPC wrappers can then exhaust a worker stack before the probe is polled.
    check_channel_inner(state, channel, Box::pin(check))
}

async fn check_channel_inner<T>(
    state: &AppState,
    channel: &ChannelConfig,
    check: std::pin::Pin<Box<impl std::future::Future<Output = Result<T, String>>>>,
) -> Result<T, String> {
    // Explicit refresh also handles proxy node changes that are invisible to
    // OS interface/PAC detection. In-flight clients remain valid; direct pools
    // and the shared platform transport are not reset.
    if channel.v2.use_system_proxy {
        crate::http_client::system_proxy_changed();
    }
    crate::network_recovery::request_check();
    let generation = local_model_failure_generation();
    let previous_safety = if lifecycle_channel_execution_kind(channel).map_err(|e| e.to_string())?
        != ExecutionKind::HttpSurface
        || crate::coding_plan::has_quota(channel.source_driver())
    {
        Some(
            subscription_safety_state_for_channel(&supplier_from_channel(channel))
                .map_err(|error| error.to_string())?,
        )
    } else {
        None
    };
    let token = {
        let runtime = state.supplier.lock().await;
        let snapshot = runtime.transport_state.lock().map_err(|e| e.to_string())?;
        snapshot
            .nodes
            .values()
            .filter_map(|node| node.route_status.as_ref())
            .find(|route| route.channel_id == channel.id)
            .map(|route| route.recovery_token.clone())
            .unwrap_or_default()
    };
    let (result, observation) = CHECK
        .scope(
            RefCell::new(HealthObservation {
                token,
                ..Default::default()
            }),
            async {
                let result = check.await;
                (result, CHECK.with(|check| check.borrow().clone()))
            },
        )
        .await;
    // A failed action is not recovery, even if an earlier metadata substep worked.
    if result.is_ok() && observation.reachable {
        apply_observation(
            state,
            channel,
            previous_safety.as_ref(),
            generation,
            observation,
        )
        .await?;
    }
    result
}

async fn apply_observation(
    state: &AppState,
    channel: &ChannelConfig,
    previous_safety: Option<&SubscriptionSafetyState>,
    generation: u64,
    observation: HealthObservation,
) -> Result<(), String> {
    let mut runtime = state.supplier.lock().await;
    // An editor can check changed credentials before saving. Never rehabilitate
    // the currently running channel using evidence from that different identity.
    let cfg = state
        .proxy_config
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    let Some(current) = cfg.channels.iter().find(|current| {
        current.id == channel.id
            && current.upstream_api_key == channel.upstream_api_key
            && crate::config::channel_detection_inputs_match(current, channel)
    }) else {
        return Ok(());
    };
    if let Some(previous_safety) = previous_safety {
        recover_subscription_check(
            previous_safety,
            observation.reachable,
            &observation.quota_windows,
        )
        .map_err(|e| e.to_string())?;
    }
    if let Ok(mut routes) = state.local_model_quota_routes.lock() {
        recover_local_models(&mut routes, &channel.id, generation, &observation);
    }
    availability::record_health_observation(channel, observation);
    // Cached validation is free: retain group, authentication, reserve and quota
    // gates instead of marking the whole channel ready after one model succeeds.
    let (_, health) = validate_supplier_channel(current.clone(), SupplierCheckMode::Cached)
        .await
        .map_err(|e| e.to_string())?;
    update_local_channel_readiness(
        &state.local_channel_readiness,
        &state.local_model_quota_routes,
        std::slice::from_ref(&health),
    );
    update_supplier_channel_health_state(&runtime.transport_state, std::slice::from_ref(&health));
    if let Some(previous) = runtime
        .channels
        .iter_mut()
        .find(|item| item.channel_id == channel.id)
    {
        *previous = health.clone();
    }
    for handle in runtime.channel_agents.values_mut() {
        if let Some(previous) = handle
            .suppliers
            .iter_mut()
            .find(|supplier| supplier.channel_id == channel.id)
        {
            if let Some(next) = supplier_registration_snapshot(
                &cfg,
                &[channel.id.clone()],
                std::slice::from_ref(&health),
            )
            .pop()
            {
                *previous = next.clone();
                send_forced_register_update(&handle.register_update, vec![next]);
            }
        }
    }
    Ok(())
}

fn recover_local_models(
    routes: &mut HashMap<String, LocalModelQuotaRouteState>,
    channel_id: &str,
    generation: u64,
    observation: &HealthObservation,
) {
    for (key, route) in routes.iter_mut() {
        let Some((id, model)) = key.split_once('\0') else {
            continue;
        };
        if id != channel_id || route.runtime_failure_sequence >= generation {
            continue;
        }
        let tested = observation
            .models
            .iter()
            .any(|tested| normalize_model_name(tested) == model);
        let windows = observation
            .quota_windows
            .iter()
            .filter(|window| {
                window
                    .model
                    .as_deref()
                    .is_some_and(|name| normalize_model_name(name) == model)
            })
            .cloned()
            .collect::<Vec<_>>();
        let quota_recovered = route.runtime_quota_failure
            && scoped_quota_summary(&windows)
                .is_some_and(|summary| matches!(summary.status.as_str(), "available" | "low"));
        if tested || quota_recovered {
            route.record_success();
        } else if observation.reachable && route.runtime_network_failure {
            // Connectivity cannot erase a separately observed official quota gate.
            *route = LocalModelQuotaRouteState {
                cooldown_until_unix: route.cooldown_until_unix,
                failed_at_unix: route.failed_at_unix,
                ..Default::default()
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_network_recovery_preserves_quota_overload_and_newer_failures() {
        let mut network = LocalModelQuotaRouteState::default();
        network.record_runtime_failure(100, 60, true);
        network.runtime_network_failure = true;
        network.cooldown_until_unix = 500;
        let mut overload = LocalModelQuotaRouteState::default();
        overload.record_runtime_overload(100, 0, true);
        let generation = local_model_failure_generation();
        let mut newer = network;
        newer.record_runtime_failure(101, 60, true);
        newer.runtime_network_failure = true;
        let mut routes = HashMap::from([
            (model_quota_route_key("a", "network"), network),
            (model_quota_route_key("a", "overload"), overload),
            (model_quota_route_key("a", "newer"), newer),
            (model_quota_route_key("b", "network"), network),
        ]);
        recover_local_models(
            &mut routes,
            "a",
            generation,
            &HealthObservation {
                reachable: true,
                ..Default::default()
            },
        );
        let recovered = routes[&model_quota_route_key("a", "network")];
        assert!(!recovered.has_runtime_failure());
        assert_eq!(
            recovered.cooldown_until_unix, 500,
            "network success is not quota evidence"
        );
        assert_eq!(routes[&model_quota_route_key("a", "overload")], overload);
        assert_eq!(routes[&model_quota_route_key("a", "newer")], newer);
        assert_eq!(routes[&model_quota_route_key("b", "network")], network);
    }

    #[test]
    fn explicit_model_success_restores_only_tested_routes() {
        let mut failed = LocalModelQuotaRouteState::default();
        failed.record_runtime_overload(100, 0, true);
        failed.cooldown_until_unix = 500;
        let generation = local_model_failure_generation();
        let mut routes = HashMap::from([
            (model_quota_route_key("a", "model-a"), failed),
            (model_quota_route_key("a", "model-b"), failed),
        ]);
        recover_local_models(
            &mut routes,
            "a",
            generation,
            &HealthObservation {
                reachable: true,
                models: vec!["model-a".into()],
                ..Default::default()
            },
        );
        assert_eq!(
            routes[&model_quota_route_key("a", "model-a")],
            LocalModelQuotaRouteState::default()
        );
        assert_eq!(routes[&model_quota_route_key("a", "model-b")], failed);
    }

    #[test]
    fn manual_quota_recovery_accepts_low_balance_without_clearing_other_models() {
        let mut failed = LocalModelQuotaRouteState::default();
        failed.record_runtime_failure(100, 300, false);
        failed.runtime_quota_failure = true;
        let generation = local_model_failure_generation();
        let key_a = model_quota_route_key("a", "model-a");
        let key_b = model_quota_route_key("a", "model-b");
        let window = QuotaWindow {
            source: "antigravity_retrieve_user_quota".into(),
            window: "requests:model-a".into(),
            model: Some("model-a".into()),
            remaining_ratio: 0.1,
            used_percent: 90.0,
            checked_at_unix: 100,
            ..Default::default()
        };
        for exhausted in [false, true] {
            let mut routes = HashMap::from([(key_a.clone(), failed), (key_b.clone(), failed)]);
            let mut windows = vec![window.clone()];
            if exhausted {
                windows.push(QuotaWindow {
                    window: "weekly".into(),
                    remaining_ratio: 0.0,
                    used_percent: 100.0,
                    ..window.clone()
                });
            }
            recover_local_models(
                &mut routes,
                "a",
                generation,
                &HealthObservation {
                    reachable: true,
                    quota_windows: windows,
                    ..Default::default()
                },
            );
            assert_eq!(routes[&key_a].has_runtime_failure(), exhausted);
            assert_eq!(routes[&key_b], failed);
        }
    }

    #[tokio::test]
    async fn recovery_evidence_is_scoped_and_does_not_claim_a_model_for_metadata() {
        record_model_success("outside-check");
        CHECK
            .scope(RefCell::new(HealthObservation::default()), async {
                record_metadata_success();
                CHECK.with(|check| {
                    assert!(check.borrow().reachable);
                    assert!(check.borrow().models.is_empty());
                });
                record_model_success("model-a");
                record_model_success("model-a");
                CHECK.with(|check| assert_eq!(check.borrow().models, ["model-a"]));
            })
            .await;
        CHECK
            .scope(RefCell::new(HealthObservation::default()), async {
                record_quota_success(&[]);
                CHECK.with(|check| assert!(!check.borrow().reachable));
            })
            .await;
    }

    #[test]
    fn registration_recovery_evidence_is_bound_to_channel_credentials() {
        let mut channel = channel_from_supplier(
            format!("recovery-{}", rand::random::<u64>()),
            &default_supplier_config(),
        );
        availability::record_health_observation(
            &channel,
            HealthObservation {
                token: "failure-1".into(),
                reachable: true,
                ..Default::default()
            },
        );
        assert!(availability::health_observation(&channel).is_some());
        let supplier = supplier_from_channel(&channel);
        let payload =
            supplier_register_payload(&SupplierAgentConfig::single(supplier.clone()), &supplier);
        assert_eq!(payload["health_observation"]["token"], "failure-1");
        channel.upstream_api_key = "changed-fixture-key".into();
        assert!(availability::health_observation(&channel).is_none());
    }
}
