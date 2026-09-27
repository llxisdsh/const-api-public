use serde::Serialize;
use std::{
    collections::HashMap,
    future::Future,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::sync::Notify;

tokio::task_local! {
    static SUPPLIER_REQUEST_OUTPUT_SCOPE: String;
}

static UPDATE_ACTIVITY: OnceLock<UpdateActivity> = OnceLock::new();

fn update_activity() -> &'static UpdateActivity {
    UPDATE_ACTIVITY.get_or_init(UpdateActivity::new)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct UpdateInstallReadiness {
    pub(crate) idle: bool,
    pub(crate) gate_acquired: bool,
    pub(crate) draining: bool,
    pub(crate) active_logical_requests: usize,
    pub(crate) active_proxy_requests: usize,
    pub(crate) active_supplier_requests: usize,
    pub(crate) pending_supplier_outbound_messages: usize,
    pub(crate) unacknowledged_supplier_replay_messages: usize,
    pub(crate) reason: String,
}

#[derive(Debug, Default)]
struct SupplierRequestActivity {
    executing: usize,
    pending_messages: usize,
    replay_messages: usize,
}

impl SupplierRequestActivity {
    fn active(&self) -> bool {
        self.executing > 0 || self.pending_messages > 0 || self.replay_messages > 0
    }
}

struct UpdateActivity {
    install_gate: AtomicBool,
    draining: AtomicBool,
    active_proxy_requests: AtomicUsize,
    supplier_requests: Mutex<HashMap<String, SupplierRequestActivity>>,
    changed: Notify,
}

impl UpdateActivity {
    fn new() -> Self {
        Self {
            install_gate: AtomicBool::new(false),
            draining: AtomicBool::new(false),
            active_proxy_requests: AtomicUsize::new(0),
            supplier_requests: Mutex::new(HashMap::new()),
            changed: Notify::new(),
        }
    }

    fn accepting_requests(&self) -> bool {
        !self.install_gate.load(Ordering::Acquire) && !self.draining.load(Ordering::Acquire)
    }

    fn try_begin_proxy(&'static self) -> Option<UpdateActivityGuard> {
        if !self.accepting_requests() {
            return None;
        }
        self.active_proxy_requests.fetch_add(1, Ordering::AcqRel);
        if !self.accepting_requests() {
            decrement(&self.active_proxy_requests, 1);
            return None;
        }
        self.changed.notify_waiters();
        Some(UpdateActivityGuard {
            activity: self,
            kind: ActivityKind::Proxy,
        })
    }

    fn try_begin_supplier(&'static self, request_id: &str) -> Option<UpdateActivityGuard> {
        if !self.accepting_requests() {
            return None;
        }
        let request_id = request_id.trim().to_string();
        if request_id.is_empty() {
            return None;
        }
        {
            let mut requests = lock_supplier_requests(&self.supplier_requests);
            if !self.accepting_requests() {
                return None;
            }
            requests.entry(request_id.clone()).or_default().executing += 1;
        }
        if !self.accepting_requests() {
            self.finish_supplier_execution(&request_id);
            return None;
        }
        self.changed.notify_waiters();
        Some(UpdateActivityGuard {
            activity: self,
            kind: ActivityKind::Supplier(request_id),
        })
    }

    fn finish_supplier_execution(&self, request_id: &str) {
        self.update_supplier_request(request_id, |request| {
            request.executing = request.executing.saturating_sub(1);
        });
    }

    fn begin_supplier_pending(&self, request_id: &str) {
        self.update_supplier_request(request_id, |request| request.pending_messages += 1);
    }

    fn finish_supplier_pending(&self, request_id: &str) {
        self.update_supplier_request(request_id, |request| {
            request.pending_messages = request.pending_messages.saturating_sub(1);
        });
    }

    fn transfer_supplier_pending_to_replay(&self, request_id: &str) {
        self.update_supplier_request(request_id, |request| {
            request.pending_messages = request.pending_messages.saturating_sub(1);
            request.replay_messages += 1;
        });
    }

    fn finish_supplier_replay(&self, request_id: &str) {
        self.update_supplier_request(request_id, |request| {
            request.replay_messages = request.replay_messages.saturating_sub(1);
        });
    }

    fn update_supplier_request(
        &self,
        request_id: &str,
        update: impl FnOnce(&mut SupplierRequestActivity),
    ) {
        let request_id = request_id.trim();
        if request_id.is_empty() {
            return;
        }
        let mut requests = lock_supplier_requests(&self.supplier_requests);
        let request = requests.entry(request_id.to_string()).or_default();
        update(request);
        if !request.active() {
            requests.remove(request_id);
        }
        drop(requests);
        self.changed.notify_waiters();
    }

    fn snapshot(&self, gate_acquired: bool) -> UpdateInstallReadiness {
        let active_proxy_requests = self.active_proxy_requests.load(Ordering::Acquire);
        let requests = lock_supplier_requests(&self.supplier_requests);
        let active_supplier_requests = requests
            .values()
            .filter(|request| request.executing > 0)
            .count();
        let pending_supplier_outbound_messages = requests
            .values()
            .map(|request| request.pending_messages)
            .sum();
        let unacknowledged_supplier_replay_messages = requests
            .values()
            .map(|request| request.replay_messages)
            .sum();
        let active_logical_requests = active_proxy_requests + requests.len();
        drop(requests);
        let draining = self.draining.load(Ordering::Acquire);
        let gate_busy = self.install_gate.load(Ordering::Acquire) && !gate_acquired;
        let reason = if active_logical_requests > 0 {
            "active_logical_requests"
        } else if gate_busy {
            "update_install_in_progress"
        } else if draining {
            "update_draining"
        } else {
            ""
        };
        UpdateInstallReadiness {
            idle: active_logical_requests == 0 && !gate_busy,
            gate_acquired,
            draining,
            active_logical_requests,
            active_proxy_requests,
            active_supplier_requests,
            pending_supplier_outbound_messages,
            unacknowledged_supplier_replay_messages,
            reason: reason.to_string(),
        }
    }

    fn try_prepare_install(&self) -> UpdateInstallReadiness {
        let before = self.snapshot(false);
        if !before.idle {
            return before;
        }
        if self
            .install_gate
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return self.snapshot(false);
        }
        let after = self.snapshot(true);
        if after.active_logical_requests > 0 {
            self.install_gate.store(false, Ordering::Release);
            self.changed.notify_waiters();
            return self.snapshot(false);
        }
        after
    }

    fn begin_drain(&self) {
        self.draining.store(true, Ordering::Release);
        self.changed.notify_waiters();
    }

    fn release_install_gate(&self) {
        self.install_gate.store(false, Ordering::Release);
        self.draining.store(false, Ordering::Release);
        self.changed.notify_waiters();
    }
}

fn lock_supplier_requests(
    requests: &Mutex<HashMap<String, SupplierRequestActivity>>,
) -> std::sync::MutexGuard<'_, HashMap<String, SupplierRequestActivity>> {
    requests
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

enum ActivityKind {
    Proxy,
    Supplier(String),
}

pub(crate) struct UpdateActivityGuard {
    activity: &'static UpdateActivity,
    kind: ActivityKind,
}

impl Drop for UpdateActivityGuard {
    fn drop(&mut self) {
        match &self.kind {
            ActivityKind::Proxy => decrement(&self.activity.active_proxy_requests, 1),
            ActivityKind::Supplier(request_id) => {
                self.activity.finish_supplier_execution(request_id)
            }
        }
        self.activity.changed.notify_waiters();
    }
}

fn decrement(counter: &AtomicUsize, amount: usize) {
    if amount == 0 {
        return;
    }
    let _ = counter.fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
        Some(value.saturating_sub(amount))
    });
}

pub(crate) fn try_begin_proxy_request() -> Option<UpdateActivityGuard> {
    update_activity().try_begin_proxy()
}

pub(crate) fn try_begin_supplier_request(request_id: &str) -> Option<UpdateActivityGuard> {
    update_activity().try_begin_supplier(request_id)
}

pub(crate) async fn scope_supplier_request_outputs<F: Future>(
    request_id: String,
    future: F,
) -> F::Output {
    SUPPLIER_REQUEST_OUTPUT_SCOPE
        .scope(request_id, future)
        .await
}

pub(crate) struct SupplierOutputPendingGuard {
    activity: &'static UpdateActivity,
    request_id: Option<String>,
    transferred: bool,
}

impl SupplierOutputPendingGuard {
    pub(crate) fn transfer(mut self, delivered_request_id: &str) {
        if self
            .request_id
            .as_deref()
            .is_some_and(|request_id| request_id != delivered_request_id)
        {
            if let Some(request_id) = self.request_id.as_deref() {
                self.activity.finish_supplier_pending(request_id);
            }
            let delivered_request_id = delivered_request_id.trim();
            if delivered_request_id.is_empty() {
                self.request_id = None;
            } else {
                self.activity.begin_supplier_pending(delivered_request_id);
                self.request_id = Some(delivered_request_id.to_string());
            }
        }
        self.transferred = true;
    }
}

impl Drop for SupplierOutputPendingGuard {
    fn drop(&mut self) {
        if !self.transferred {
            if let Some(request_id) = self.request_id.as_deref() {
                self.activity.finish_supplier_pending(request_id);
            }
        }
    }
}

pub(crate) fn track_supplier_output_before_send() -> SupplierOutputPendingGuard {
    let activity = update_activity();
    let request_id = SUPPLIER_REQUEST_OUTPUT_SCOPE.try_with(Clone::clone).ok();
    if let Some(request_id) = request_id.as_deref() {
        activity.begin_supplier_pending(request_id);
    }
    SupplierOutputPendingGuard {
        activity,
        request_id,
        transferred: false,
    }
}

pub(crate) fn supplier_output_send_failed_or_delivered_without_replay(request_id: &str) {
    update_activity().finish_supplier_pending(request_id);
}

pub(crate) fn supplier_output_enqueued_for_replay(request_id: &str) {
    update_activity().transfer_supplier_pending_to_replay(request_id);
}

pub(crate) fn supplier_replay_message_removed(request_id: &str) {
    update_activity().finish_supplier_replay(request_id);
}

pub(crate) fn supplier_request_is_producing(request_id: &str) -> bool {
    lock_supplier_requests(&update_activity().supplier_requests)
        .get(request_id)
        .is_some_and(|request| request.executing > 0 || request.pending_messages > 0)
}

pub(crate) fn update_install_readiness() -> UpdateInstallReadiness {
    update_activity().snapshot(false)
}

pub(crate) fn try_prepare_update_install() -> UpdateInstallReadiness {
    update_activity().try_prepare_install()
}

pub(crate) fn begin_update_drain() {
    update_activity().begin_drain();
}

pub(crate) async fn wait_for_update_activity_change() {
    update_activity().changed.notified().await;
}

pub(crate) async fn wait_for_update_drain() {
    loop {
        if update_activity().draining.load(Ordering::Acquire) {
            return;
        }
        let changed = update_activity().changed.notified();
        if update_activity().draining.load(Ordering::Acquire) {
            return;
        }
        changed.await;
    }
}

pub(crate) fn release_update_install_gate() {
    update_activity().release_install_gate();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_gate_uses_distinct_logical_requests_not_message_count() {
        let activity = Box::leak(Box::new(UpdateActivity::new()));
        let supplier = activity
            .try_begin_supplier("request-a")
            .expect("supplier request should begin");
        activity.begin_supplier_pending("request-a");
        activity.transfer_supplier_pending_to_replay("request-a");
        activity.begin_supplier_pending("request-a");
        activity.transfer_supplier_pending_to_replay("request-a");

        let busy = activity.snapshot(false);
        assert_eq!(busy.active_logical_requests, 1);
        assert_eq!(busy.unacknowledged_supplier_replay_messages, 2);

        drop(supplier);
        assert_eq!(activity.snapshot(false).active_logical_requests, 1);
        activity.finish_supplier_replay("request-a");
        assert_eq!(activity.snapshot(false).active_logical_requests, 1);
        activity.finish_supplier_replay("request-a");
        assert!(activity.try_prepare_install().gate_acquired);
    }

    #[test]
    fn drain_rejects_new_requests_without_interrupting_existing_ones() {
        let activity = Box::leak(Box::new(UpdateActivity::new()));
        let proxy = activity.try_begin_proxy().expect("proxy should begin");
        activity.begin_drain();
        assert!(activity.try_begin_proxy().is_none());
        assert!(activity.try_begin_supplier("request-b").is_none());
        assert_eq!(activity.snapshot(false).active_logical_requests, 1);
        drop(proxy);
        assert!(activity.try_prepare_install().gate_acquired);
    }

    #[test]
    fn canceled_supplier_send_releases_its_request() {
        let activity = Box::leak(Box::new(UpdateActivity::new()));
        activity.begin_supplier_pending("request-a");
        let pending = SupplierOutputPendingGuard {
            activity,
            request_id: Some("request-a".to_string()),
            transferred: false,
        };
        assert_eq!(activity.snapshot(false).active_logical_requests, 1);
        drop(pending);
        assert_eq!(activity.snapshot(false).active_logical_requests, 0);
    }

    #[test]
    fn pending_output_follows_the_delivered_request_identity() {
        let activity = Box::leak(Box::new(UpdateActivity::new()));
        activity.begin_supplier_pending("request-a");
        SupplierOutputPendingGuard {
            activity,
            request_id: Some("request-a".to_string()),
            transferred: false,
        }
        .transfer("request-b");

        let pending = activity.snapshot(false);
        assert_eq!(pending.active_logical_requests, 1);
        assert_eq!(pending.pending_supplier_outbound_messages, 1);
        assert!(!lock_supplier_requests(&activity.supplier_requests).contains_key("request-a"));
        activity.finish_supplier_pending("request-b");
        assert_eq!(activity.snapshot(false).active_logical_requests, 0);
    }
}
