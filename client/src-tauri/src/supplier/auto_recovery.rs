const SUPPLIER_AUTO_RECOVERY_INTERVAL: Duration = Duration::from_secs(30);
const SUPPLIER_AUTO_RECOVERY_POLL_INTERVAL: Duration = Duration::from_secs(2);
const SUPPLIER_AUTO_RECOVERY_ACK_TIMEOUT: Duration = Duration::from_secs(10);
const SUPPLIER_AUTO_RECOVERY_ACK_POLL_INTERVAL: Duration = Duration::from_millis(400);

#[derive(Clone, Copy, Debug)]
struct SupplierAutoRecoveryAttempt {
    failed_attempts: u32,
    next_attempt: Instant,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SupplierAutoRecoveryCandidate {
    pooled: bool,
    detectable: bool,
    signed_in: bool,
    runtime_starting: bool,
    online: bool,
    route_retryable: bool,
    safety_retryable: bool,
    group_recheck_due: bool,
}

impl SupplierAutoRecoveryCandidate {
    fn eligible(self) -> bool {
        self.pooled
            && self.detectable
            && self.signed_in
            && !self.runtime_starting
            && (!self.online || self.group_recheck_due)
            && self.route_retryable
            && self.safety_retryable
    }
}

fn supplier_auto_recovery_uses_full_check(
    failed_attempts: u32,
    health_available: bool,
) -> bool {
    !health_available && failed_attempts == 1
}

fn supplier_auto_recovery_value_allows_retry(value: &str, safety: bool) -> bool {
    let value = value.trim().to_ascii_lowercase();
    if value.starts_with("model_") {
        return false;
    }
    if safety {
        !matches!(
            value.as_str(),
            "auth_error"
                | "auth_refreshing"
                | "cooldown"
                | "disabled"
                | "quota_exhausted"
                | "quota_low"
                | "risk_blocked"
        )
    } else {
        !matches!(
            value.as_str(),
            "account_mismatch"
                | "auth_error"
                | "auth_revoked"
                | "cooldown"
                | "model_not_supported"
                | "model_unsupported"
                | "models_partially_supported"
                | "pricing_fallback"
                | "quota_exhausted"
                | "rate_limited"
                | "risk_blocked"
                | "risk_challenge"
        )
    }
}

fn supplier_auto_recovery_route_blocks_ready(
    route: Option<&SupplierNodeRouteStatus>,
) -> bool {
    let Some(route) = route else {
        return false;
    };
    if route.reason.trim().eq_ignore_ascii_case("models_partially_supported")
        && !route.accepted_models.is_empty()
    {
        return false;
    }
    matches!(
        route.state.trim().to_ascii_lowercase().as_str(),
        "offline" | "removed" | "deleted" | "failed"
    )
}

fn supplier_auto_recovery_safety_state(
    channel: &ChannelConfig,
    health: Option<&SupplierChannelHealth>,
) -> String {
    if channel.kind != "subscription_adapter" {
        return "not_applicable".to_string();
    }
    if let Some(state) = health.map(|item| item.safety_state.trim()).filter(|item| !item.is_empty()) {
        return state.to_string();
    }
    if health.is_some_and(|item| item.status == "available") {
        "active".to_string()
    } else {
        "unknown".to_string()
    }
}

fn supplier_auto_recovery_channel_online(
    status: &SupplierStatus,
    channel_id: &str,
) -> bool {
    let health_available = status
        .channels
        .iter()
        .find(|health| health.channel_id == channel_id)
        .is_some_and(|health| health.status == "available");
    let transport_ready = status
        .channel_transports
        .iter()
        .find(|transport| transport.channel_id == channel_id)
        .is_some_and(|transport| {
            matches!(transport.active_transport.as_str(), "quic" | "websocket")
                && transport.platform_registered
        });
    let route = status
        .route_statuses
        .iter()
        .find(|route| route.channel_id == channel_id);
    supplier_auto_recovery_is_online(
        health_available,
        transport_ready,
        !supplier_auto_recovery_route_blocks_ready(route),
    )
}

fn supplier_auto_recovery_is_online(
    health_available: bool,
    transport_registered: bool,
    route_ready: bool,
) -> bool {
    health_available && transport_registered && route_ready
}

fn supplier_auto_recovery_candidate(
    channel: &ChannelConfig,
    status: &SupplierStatus,
    signed_in: bool,
) -> SupplierAutoRecoveryCandidate {
    let health = status
        .channels
        .iter()
        .find(|health| health.channel_id == channel.id);
    let route = status
        .route_statuses
        .iter()
        .find(|route| route.channel_id == channel.id);
    let safety_state = supplier_auto_recovery_safety_state(channel, health);
    SupplierAutoRecoveryCandidate {
        pooled: channel.enabled
            && crate::source_driver::channel_is_platform_shareable(channel),
        detectable: channel.kind == "subscription_adapter"
            || !channel.upstream_base_url.trim().is_empty(),
        signed_in,
        runtime_starting: status.starting,
        online: supplier_auto_recovery_channel_online(status, &channel.id),
        route_retryable: supplier_auto_recovery_value_allows_retry(
            route.map(|item| item.reason.as_str()).unwrap_or_default(),
            false,
        ),
        safety_retryable: supplier_auto_recovery_value_allows_retry(&safety_state, true),
        group_recheck_due: availability::recheck_due(channel, now_unix()),
    }
}

async fn wait_for_supplier_auto_recovery_ack(state: &AppState, channel_id: &str) -> bool {
    let deadline = Instant::now() + SUPPLIER_AUTO_RECOVERY_ACK_TIMEOUT;
    loop {
        let status = state.supplier.lock().await.status();
        if supplier_auto_recovery_channel_online(&status, channel_id) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(SUPPLIER_AUTO_RECOVERY_ACK_POLL_INTERVAL).await;
    }
}

pub(crate) async fn run_supplier_auto_recovery_loop(state: AppState) {
    let mut attempts = HashMap::<String, SupplierAutoRecoveryAttempt>::new();
    // Discover candidates only after normal startup has begun. Each newly
    // discovered offline channel still receives the full recovery grace period.
    tokio::time::sleep(SUPPLIER_AUTO_RECOVERY_POLL_INTERVAL).await;

    loop {
        // Only persisted configuration is eligible. The recovery path below
        // still uses the existing detection-only merge and rejects concurrent
        // persisted edits, so the native scheduler cannot overwrite user input.
        let config = state
            .proxy_config
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let signed_in = state.account.lock().await.status().state == "signed_in";
        let status = state.supplier.lock().await.status();
        let eligible = config
            .channels
            .iter()
            .filter(|channel| {
                supplier_auto_recovery_candidate(channel, &status, signed_in).eligible()
            })
            .map(|channel| channel.id.clone())
            .collect::<HashSet<_>>();
        attempts.retain(|channel_id, _| eligible.contains(channel_id));

        let now = Instant::now();
        for channel_id in &eligible {
            attempts.entry(channel_id.clone()).or_insert(SupplierAutoRecoveryAttempt {
                failed_attempts: 0,
                next_attempt: now + SUPPLIER_AUTO_RECOVERY_INTERVAL,
            });
        }

        let due_channel_id = attempts
            .iter()
            .filter(|(_, attempt)| attempt.next_attempt <= now)
            .min_by_key(|(_, attempt)| attempt.next_attempt)
            .map(|(channel_id, _)| channel_id.clone());
        let Some(channel_id) = due_channel_id else {
            tokio::time::sleep(SUPPLIER_AUTO_RECOVERY_POLL_INTERVAL).await;
            continue;
        };

        // Re-read status immediately before an expensive probe. A manual action
        // or transport ACK may have made this scheduled attempt unnecessary.
        let status = state.supplier.lock().await.status();
        let Some(channel) = config.channels.iter().find(|channel| channel.id == channel_id) else {
            attempts.remove(&channel_id);
            continue;
        };
        if !supplier_auto_recovery_candidate(channel, &status, signed_in).eligible() {
            attempts.remove(&channel_id);
            continue;
        }
        let health_available = status
            .channels
            .iter()
            .find(|health| health.channel_id == channel_id)
            .is_some_and(|health| health.status == "available");
        let failed_attempts = attempts
            .get(&channel_id)
            .map(|attempt| attempt.failed_attempts)
            .unwrap_or_default();
        let group_recheck = availability::recheck_due(channel, now_unix());
        let mut full_check = group_recheck
            || supplier_auto_recovery_uses_full_check(failed_attempts, health_available);
        let offline_budget = if full_check && !group_recheck {
            availability::reserve_offline_recovery_budget(channel)
        } else { 0 };
        if full_check && !group_recheck && offline_budget == 0 { full_check = false; }

        log::info!(
            "[const-api][supplier] automatic recovery attempt channel_id={} mode={} failed_attempts={}",
            channel_id,
            if full_check { "full" } else { "snapshot" },
            failed_attempts,
        );
        let recovering = recover_supplier_channel_inner(&state, channel_id.clone(), full_check);
        let outcome = if group_recheck {
            availability::automatic_recheck(recovering).await
        } else if full_check {
            crate::upstream_transport::with_probe_budget(offline_budget, recovering).await.0
        } else { recovering.await };
        let recovered = match outcome
        {
            Ok(_) => wait_for_supplier_auto_recovery_ack(&state, &channel_id).await,
            Err(error) => {
                log::warn!(
                    "[const-api][supplier] automatic recovery attempt failed channel_id={}: {}",
                    channel_id,
                    error
                );
                false
            }
        };

        if recovered {
            attempts.remove(&channel_id);
            log::info!(
                "[const-api][supplier] automatic recovery confirmed channel_id={}",
                channel_id
            );
        } else if let Some(attempt) = attempts.get_mut(&channel_id) {
            attempt.failed_attempts = attempt.failed_attempts.saturating_add(1);
            attempt.next_attempt = Instant::now() + SUPPLIER_AUTO_RECOVERY_INTERVAL;
        }
    }
}

#[cfg(test)]
mod supplier_auto_recovery_tests {
    use super::*;

    #[test]
    fn each_recovery_episode_runs_at_most_one_full_check() {
        let modes = (0..8)
            .map(|attempts| supplier_auto_recovery_uses_full_check(attempts, false))
            .collect::<Vec<_>>();
        assert_eq!(modes, vec![false, true, false, false, false, false, false, false]);
        assert!(!supplier_auto_recovery_uses_full_check(1, true));
    }

    #[test]
    fn policy_and_credential_failures_are_not_retried() {
        assert!(supplier_auto_recovery_value_allows_retry("removed", false));
        assert!(supplier_auto_recovery_value_allows_retry("unregistered", false));
        assert!(!supplier_auto_recovery_value_allows_retry(
            "model_not_supported",
            false
        ));
        assert!(!supplier_auto_recovery_value_allows_retry("rate_limited", false));
        assert!(!supplier_auto_recovery_value_allows_retry("auth_error", true));
        assert!(!supplier_auto_recovery_value_allows_retry("risk_blocked", true));
    }

    #[test]
    fn eligibility_requires_an_offline_retryable_signed_in_channel() {
        let ready = SupplierAutoRecoveryCandidate {
            pooled: true,
            detectable: true,
            signed_in: true,
            route_retryable: true,
            safety_retryable: true,
            ..SupplierAutoRecoveryCandidate::default()
        };
        assert!(ready.eligible());
        assert!(!SupplierAutoRecoveryCandidate { online: true, ..ready }.eligible());
        assert!(!SupplierAutoRecoveryCandidate {
            runtime_starting: true,
            ..ready
        }
        .eligible());
        assert!(!SupplierAutoRecoveryCandidate {
            signed_in: false,
            ..ready
        }
        .eligible());
    }

    #[test]
    fn lan_share_channels_never_enter_platform_auto_recovery() {
        let mut channel = crate::default_config().channels.remove(0);
        channel.enabled = true;
        channel.v2.source_driver = crate::source_driver::SourceDriverId::LanShare;
        channel.kind = "lan_share".to_string();
        channel.upstream_base_url = "http://192.168.1.20:38787/v1".to_string();
        let status = SupplierStatus {
            running: false,
            starting: false,
            node_id: String::new(),
            server_ws_url: String::new(),
            server_quic_url: String::new(),
            transport_preference: String::new(),
            connected_transport: String::new(),
            active_transport: String::new(),
            last_transport_error: String::new(),
            nodes: Vec::new(),
            channels: Vec::new(),
            last_error: String::new(),
            route_statuses: Vec::new(),
            channel_transports: Vec::new(),
            availability_groups: None,
        };

        let candidate = supplier_auto_recovery_candidate(&channel, &status, true);

        assert!(!candidate.pooled);
        assert!(!candidate.eligible());
    }

    #[test]
    fn online_requires_health_transport_registration_and_a_ready_route() {
        assert!(supplier_auto_recovery_is_online(true, true, true));
        assert!(!supplier_auto_recovery_is_online(false, true, true));
        assert!(!supplier_auto_recovery_is_online(true, false, true));
        assert!(!supplier_auto_recovery_is_online(true, true, false));
    }

    #[test]
    fn fallback_is_a_routable_server_state_not_a_registration_failure() {
        let fallback = SupplierNodeRouteStatus {
            state: "fallback".to_string(),
            reason: "network_error".to_string(),
            ..SupplierNodeRouteStatus::default()
        };
        let offline = SupplierNodeRouteStatus {
            state: "offline".to_string(),
            ..SupplierNodeRouteStatus::default()
        };

        assert!(!supplier_auto_recovery_route_blocks_ready(Some(&fallback)));
        assert!(supplier_auto_recovery_route_blocks_ready(Some(&offline)));
    }
}
