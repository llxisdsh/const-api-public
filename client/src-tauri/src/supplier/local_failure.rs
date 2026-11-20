//! Small, ephemeral outbox for local inference observations. Routing state and
//! recovery stay in the existing local/server health layers; this is not a pool.
use super::*;
use std::collections::BTreeMap;

const MAX_REPORTS: usize = 256;
const MAX_CHANNEL_REPORTS: usize = 64;
const REPORT_MAX_AGE: Duration = Duration::from_secs(300);

#[derive(Clone)]
struct Pending {
    id: String,
    observed: Instant,
    model: String,
    status: u16,
    payload: serde_json::Map<String, serde_json::Value>,
}

fn reports() -> &'static StdMutex<BTreeMap<(String, String, String), Pending>> {
    static REPORTS: OnceLock<StdMutex<BTreeMap<(String, String, String), Pending>>> =
        OnceLock::new();
    REPORTS.get_or_init(Default::default)
}

fn changes() -> &'static tokio::sync::watch::Sender<u64> {
    static CHANGES: OnceLock<tokio::sync::watch::Sender<u64>> = OnceLock::new();
    CHANGES.get_or_init(|| tokio::sync::watch::channel(0).0)
}

fn next_id() -> String {
    static EPOCH: OnceLock<String> = OnceLock::new();
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    format!(
        "{}:{}",
        EPOCH.get_or_init(|| uuid::Uuid::new_v4().to_string()),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

pub(super) fn subscribe() -> tokio::sync::watch::Receiver<u64> {
    changes().subscribe()
}

pub(crate) struct Reporter {
    channel_id: String,
    identity: String,
}

impl Reporter {
    pub(crate) fn new(channel: &ChannelConfig) -> Self {
        Self {
            channel_id: channel.id.clone(),
            identity: availability::fingerprint(channel),
        }
    }

    pub(crate) fn failure(&self, failure: &crate::upstream_failure::Failure) {
        // Mirror the observation sent by platform execution. Do not export raw error
        // text, private parameter values, request failures or local admission misses.
        if failure.version != 1
            || failure.cause == "unknown"
            || failure.model.trim().is_empty()
            || !matches!(failure.scope.as_str(), "channel" | "model" | "request")
            || (failure.scope == "request"
                && !matches!(
                    failure.cause.as_str(),
                    "rate_limited"
                        | "provider_overload"
                        | "permission_denied"
                        | "provider_auth_error"
                        | "timeout"
                        | "network_error"
                ))
        {
            return;
        }
        let safe = crate::upstream_failure::Failure {
            version: 1,
            cause: failure.cause.clone(),
            scope: failure.scope.clone(),
            model: failure.model.clone(),
            status: failure.status,
            rule_id: failure.rule_id.clone(),
            retry_after_seconds: failure.retry_after_seconds,
            retry_source: failure.retry_source.clone(),
            ..Default::default()
        };
        let mut payload = serde_json::Map::new();
        safe.attach(&mut payload);
        self.insert(
            &failure.model,
            failure.scope == "channel",
            failure.effective_status(),
            payload,
        );
    }

    fn insert(
        &self,
        model: &str,
        channel_scope: bool,
        status: u16,
        payload: serde_json::Map<String, serde_json::Value>,
    ) {
        let key = (
            self.channel_id.clone(),
            self.identity.clone(),
            if channel_scope {
                String::new()
            } else {
                normalize_model_name(model)
            },
        );
        let mut pending = reports()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pending.retain(|_, report| report.observed.elapsed() < REPORT_MAX_AGE);
        let channel_full = pending
            .keys()
            .filter(|(id, _, _)| id == &self.channel_id)
            .count()
            >= MAX_CHANNEL_REPORTS;
        if !pending.contains_key(&key) && (pending.len() >= MAX_REPORTS || channel_full) {
            if let Some(oldest) = pending
                .iter()
                .filter(|((id, _, _), _)| !channel_full || id == &self.channel_id)
                .min_by_key(|(_, report)| report.observed)
                .map(|(key, _)| key.clone())
            {
                pending.remove(&oldest);
            }
        }
        pending.insert(
            key,
            Pending {
                id: next_id(),
                observed: Instant::now(),
                model: model.to_string(),
                status,
                payload,
            },
        );
        drop(pending);
        changes().send_modify(|version| *version = version.wrapping_add(1));
    }

    /// Drop only older unsent evidence for this execution identity. A successful
    /// turn is not authority to clear a newer platform fault or another model.
    pub(crate) fn success(&self, model: &str, started: Instant) {
        reports()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(id, identity, route_model), report| {
                id != &self.channel_id
                    || (!route_model.is_empty() && route_model != &normalize_model_name(model))
                    || identity != &self.identity
                    || report.observed >= started
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel() -> ChannelConfig {
        let mut channel = default_config().channels.remove(0);
        channel.id = uuid::Uuid::new_v4().to_string();
        channel
    }

    #[test]
    fn registration_signature_ignores_only_report_age_not_new_failures() {
        let mut message = SupplierMessage {
            output_credit: None,
            id: "register".into(),
            kind: "register".into(),
            payload: serde_json::json!({"channels": [{"local_failure_observations": [{
                "id": "process:1", "age_seconds": 0, "failure": {"retry_after_seconds": 60}
            }]}]})
            .as_object()
            .unwrap()
            .clone(),
        };
        let original = supplier_register_message_signature(&message);
        message.payload["channels"][0]["local_failure_observations"][0]["age_seconds"] = 30.into();
        assert_eq!(supplier_register_message_signature(&message), original);
        message.payload["channels"][0]["local_failure_observations"][0]["id"] = "process:2".into();
        assert_ne!(supplier_register_message_signature(&message), original);
        message.payload["channels"][0]["local_failure_observations"][0]["id"] = "process:1".into();
        message.payload["channels"][0]["local_failure_observations"][0]["failure"]["retry_after_seconds"] =
            120.into();
        assert_ne!(supplier_register_message_signature(&message), original);
    }

    #[test]
    fn local_outbox_is_private_bounded_and_identity_scoped() {
        let channel = channel();
        let reporter = Reporter::new(&channel);
        let failure = crate::upstream_failure::Failure {
            version: 1,
            cause: "model_quota".into(),
            scope: "model".into(),
            model: "model-a".into(),
            status: 429,
            message: "private prompt".into(),
            param: "private parameter".into(),
            code: Some("private code".into()),
            ..Default::default()
        };
        let old_request = Instant::now();
        reporter.failure(&failure);
        let first = snapshot(&channel);
        assert_eq!(first.len(), 1);
        assert!(!serde_json::to_string(&first).unwrap().contains("private"));
        assert_eq!(
            snapshot(&channel)[0]["id"],
            first[0]["id"],
            "resends retain identity"
        );
        reporter.success("model-a", old_request);
        assert_eq!(
            snapshot(&channel).len(),
            1,
            "old in-flight success cannot erase newer fault"
        );
        reporter.success("model-b", Instant::now());
        assert_eq!(
            snapshot(&channel).len(),
            1,
            "other model success cannot erase fault"
        );
        let mut replacement = channel.clone();
        replacement.upstream_api_key = "new-test-key".into();
        assert!(
            snapshot(&replacement).is_empty(),
            "credential replacement cannot inherit reports"
        );
        Reporter::new(&replacement).failure(&failure);
        let replacement_id = snapshot(&replacement)[0]["id"].clone();
        reporter.failure(&failure);
        assert_eq!(
            snapshot(&replacement)[0]["id"],
            replacement_id,
            "late completion with old credentials cannot overwrite new evidence"
        );
        reporter.success("model-a", Instant::now());
        assert!(snapshot(&channel).is_empty());
        assert_eq!(snapshot(&replacement).len(), 1);
        for index in 0..100 {
            reporter.failure(&crate::upstream_failure::Failure {
                model: format!("model-{index}"),
                ..failure.clone()
            });
        }
        assert_eq!(snapshot(&channel).len(), MAX_CHANNEL_REPORTS);
        reports()
            .lock()
            .unwrap()
            .retain(|(id, _, _), _| id != &channel.id);
    }

    #[tokio::test]
    async fn local_transport_observation_keeps_health_evidence_without_replay_or_secrets() {
        let channel = channel();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        let error = anyhow::Error::new(
            crate::upstream_transport::send(
                client.post(format!("http://{address}/?key=private-test")),
            )
            .await
            .unwrap_err(),
        );
        record_execution_error(&channel, "model-a", &error);
        let values = snapshot(&channel);
        assert_eq!(values.len(), 1);
        assert_eq!(
            values[0]["failure"][EXECUTION_EVIDENCE_FIELD]["recommendation"]["retry"],
            "none"
        );
        assert_eq!(
            values[0]["failure"][EXECUTION_EVIDENCE_FIELD]["diagnostics"]["error_class"],
            "connect"
        );
        assert!(
            !serde_json::to_string(&values)
                .unwrap()
                .contains("private-test")
        );
        reports()
            .lock()
            .unwrap()
            .retain(|(id, _, _), _| id != &channel.id);
    }
}

pub(crate) fn record(channel: &ChannelConfig, failure: &crate::upstream_failure::Failure) {
    Reporter::new(channel).failure(failure);
}

pub(crate) fn record_payload(
    channel: &ChannelConfig,
    payload: &serde_json::Map<String, serde_json::Value>,
) {
    if let Some(failure) = payload.get("failure_observation_v1").and_then(|value| {
        serde_json::from_value::<crate::upstream_failure::Failure>(value.clone()).ok()
    }) {
        record(channel, &failure);
    }
}

pub(crate) fn record_execution_error(channel: &ChannelConfig, model: &str, error: &anyhow::Error) {
    let mut evidence = execution_evidence_for_error(error);
    // Same typed evidence as platform execution. Availability evidence does not
    // establish whether the request ran, so it must never grant replay.
    if evidence.error_code != "ambiguous_transport"
        || !evidence.diagnostics.as_ref().is_some_and(|value| {
            value.phase == "upstream_send"
                && matches!(value.error_class.as_str(), "connect" | "connect_timeout")
        })
    {
        return;
    }
    if let Some(value) = evidence.diagnostics.as_mut() {
        value.cause_codes.clear();
        value.chain_fingerprint.clear();
        value.os_error = None;
        value.chain_depth = 0;
    }
    let mut payload = serde_json::Map::new();
    payload.insert("error_kind".into(), "ambiguous_transport".into());
    payload.insert("failure_scope".into(), "channel".into());
    attach_execution_evidence(&mut payload, &evidence);
    Reporter::new(channel).insert(model, true, 502, payload);
}

pub(super) fn snapshot(channel: &ChannelConfig) -> Vec<serde_json::Value> {
    let identity = availability::fingerprint(channel);
    let mut pending = reports()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    pending.retain(|_, report| report.observed.elapsed() < REPORT_MAX_AGE);
    pending.iter().filter(|((id, execution_identity, _), _)| id == &channel.id && execution_identity == &identity)
        .map(|(_, report)| {
            serde_json::json!({"id": report.id, "model": report.model,
                "age_seconds": report.observed.elapsed().as_secs(), "status": report.status, "failure": report.payload})
        }).collect()
}
