pub(crate) fn send_register_update(
    tx: &mpsc::Sender<SupplierRegistrationUpdate>,
    payload: Vec<SupplierConfig>,
) {
    send_supplier_registration_update(
        tx,
        SupplierRegistrationUpdate {
            suppliers: payload,
            force: false,
        },
    );
}

pub(crate) fn send_forced_register_update(
    tx: &mpsc::Sender<SupplierRegistrationUpdate>,
    payload: Vec<SupplierConfig>,
) {
    send_supplier_registration_update(
        tx,
        SupplierRegistrationUpdate {
            suppliers: payload,
            force: true,
        },
    );
}

fn send_supplier_registration_update(
    tx: &mpsc::Sender<SupplierRegistrationUpdate>,
    update: SupplierRegistrationUpdate,
) {
    match tx.try_send(update) {
        Ok(()) => {}
        Err(tokio::sync::mpsc::error::TrySendError::Full(update)) => {
            let tx = tx.clone();
            spawn_logged("supplier register update send", async move {
                if tx.send(update).await.is_err() {
                    log::warn!("[const-api][supplier] registration update queue closed");
                }
            });
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            log::warn!("[const-api][supplier] registration update queue is closed");
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct RegisterSendState {
    last_signatures: HashMap<String, String>,
    last_sent_at: HashMap<String, Instant>,
}

#[derive(Default)]
struct SupplierPendingRegistrations {
    // At most one queued snapshot per channel, rather than retaining every
    // large intermediate catalog while the connection is backpressured.
    updates: VecDeque<(String, SupplierAgentConfig, Duration, bool)>,
    invalidate: bool,
}

impl SupplierPendingRegistrations {
    fn publish(&mut self, config: &SupplierAgentConfig, interval: Duration, force: bool) {
        for (scope, update) in supplier_register_updates(config) {
            if scope == "session" {
                self.updates.clear();
            }
            if let Some(pending) = self.updates.iter_mut().find(|item| item.0 == scope) {
                pending.1 = update;
                pending.2 = interval;
                pending.3 |= force;
            } else {
                self.updates.push_back((scope, update, interval, force));
            }
        }
    }

    fn next_due(
        &mut self,
        state: &RegisterSendState,
    ) -> (
        Option<(String, SupplierAgentConfig, Duration, bool)>,
        Option<Duration>,
    ) {
        let mut delay: Option<Duration> = None;
        for (index, (scope, _, interval, force)) in self.updates.iter().enumerate() {
            let remaining = if *force {
                Duration::ZERO
            } else {
                state
                    .last_sent_at
                    .get(scope)
                    .map(|sent| interval.saturating_sub(sent.elapsed()))
                    .unwrap_or_default()
            };
            if remaining.is_zero() {
                return (self.updates.remove(index), None);
            }
            delay = Some(delay.map_or(remaining, |previous| previous.min(remaining)));
        }
        (None, delay)
    }
}

/// One ordered worker per physical connection. Building a large catalog and
/// waiting for registration throttling must never block that connection's reads.
pub(crate) struct SupplierRegistrationDispatcher {
    pending: Arc<StdMutex<SupplierPendingRegistrations>>,
    changed: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
    initial_queued: Arc<AtomicBool>,
}

impl SupplierRegistrationDispatcher {
    pub(crate) fn new(
        outbound: SupplierOutbound,
        errors: mpsc::UnboundedSender<(&'static str, String)>,
    ) -> Self {
        let pending = Arc::new(StdMutex::new(SupplierPendingRegistrations::default()));
        let changed = Arc::new(Notify::new());
        let worker_pending = pending.clone();
        let worker_changed = changed.clone();
        let initial_queued = Arc::new(AtomicBool::new(false));
        let queued = initial_queued.clone();
        let task = spawn_logged("supplier registration writer", async move {
            let mut state = RegisterSendState::default();
            loop {
                let (command, delay) = match worker_pending.lock() {
                    Ok(mut pending) => {
                        if std::mem::take(&mut pending.invalidate) {
                            state.force_next_registration();
                        }
                        pending.next_due(&state)
                    }
                    Err(_) => {
                        let _ = errors.send((
                            "registration send",
                            "supplier registration queue is poisoned".into(),
                        ));
                        break;
                    }
                };
                match command {
                    Some((_, config, _, force)) => {
                        if force {
                            state.force_channel_registrations(&config.suppliers);
                        }
                        if let Err(error) = send_supplier_register_if_due(
                            &outbound,
                            &config,
                            &mut state,
                            Duration::ZERO,
                        )
                        .await
                        {
                            let _ = errors.send((
                                "registration send",
                                compact_supplier_transport_error(error),
                            ));
                            break;
                        }
                        queued.store(true, Ordering::Release);
                    }
                    None => match delay {
                        Some(delay) => {
                            tokio::select! {
                                _ = worker_changed.notified() => {},
                                _ = tokio::time::sleep(delay) => {},
                            }
                        }
                        None => worker_changed.notified().await,
                    },
                }
            }
        });
        Self {
            pending,
            changed,
            task,
            initial_queued,
        }
    }

    pub(crate) fn publish(
        &self,
        config: SupplierAgentConfig,
        interval: Duration,
        force: bool,
    ) -> Result<()> {
        if self.task.is_finished() {
            return Err(anyhow!("supplier registration writer closed"));
        }
        self.pending
            .lock()
            .map_err(|_| anyhow!("supplier registration queue is poisoned"))?
            .publish(&config, interval, force);
        self.changed.notify_one();
        Ok(())
    }

    fn initial_queued(&self) -> bool {
        self.initial_queued.load(Ordering::Acquire)
    }

    fn invalidate(&self) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.invalidate = true;
        }
        self.changed.notify_one();
    }
}

impl Drop for SupplierRegistrationDispatcher {
    fn drop(&mut self) {
        // A reconnect has a new authentication/codec context. Never let an old
        // registration worker outlive its physical connection or retain its queue.
        self.task.abort();
    }
}

impl RegisterSendState {
    pub(crate) fn force_next_registration(&mut self) {
        self.last_signatures.clear();
        self.last_sent_at.clear();
    }

    pub(crate) fn force_channel_registrations(&mut self, suppliers: &[SupplierConfig]) {
        if suppliers.is_empty() {
            self.force_next_registration();
            return;
        }
        for supplier in suppliers {
            let channel_id = if supplier.channel_id.trim().is_empty() {
                supplier.node_id.trim()
            } else {
                supplier.channel_id.trim()
            };
            let scope = format!("channel:{channel_id}");
            self.last_signatures.remove(&scope);
            self.last_sent_at.remove(&scope);
        }
    }
}

#[cfg(test)]
pub(crate) fn supplier_register_signature(config: &SupplierAgentConfig) -> String {
    supplier_register_message_signature(&supplier_register_message(config))
}

fn supplier_register_message_signature(message: &SupplierMessage) -> String {
    let mut payload = serde_json::Value::Object(message.payload.clone());
    remove_volatile_supplier_observation_times(&mut payload);
    serde_json::to_vec(&payload)
        .map(|bytes| hex::encode(Sha256::digest(bytes)))
        .unwrap_or_default()
}

pub(crate) fn remove_volatile_supplier_observation_times(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            // These timestamps describe when an unchanged observation was sampled. They must not
            // turn the registration itself into a change; reset/cooldown timestamps remain intact.
            object.remove("quota_checked_at_unix");
            object.remove("observed_at_unix");
            object.remove("checked_at_unix");
            for child in object.values_mut() {
                remove_volatile_supplier_observation_times(child);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                remove_volatile_supplier_observation_times(item);
            }
        }
        _ => {}
    }
}

fn supplier_register_updates(config: &SupplierAgentConfig) -> Vec<(String, SupplierAgentConfig)> {
    if config.suppliers.is_empty() {
        return vec![("session".to_string(), config.clone())];
    }
    config
        .suppliers
        .iter()
        .map(|supplier| {
            let channel_id = if supplier.channel_id.trim().is_empty() {
                supplier.node_id.trim()
            } else {
                supplier.channel_id.trim()
            };
            let update = config.with_suppliers(vec![supplier.clone()]);
            (format!("channel:{channel_id}"), update)
        })
        .collect()
}

pub(crate) async fn send_supplier_register_if_due(
    outbound: &SupplierOutbound,
    config: &SupplierAgentConfig,
    state: &mut RegisterSendState,
    min_interval: Duration,
) -> Result<()> {
    for (scope, update) in supplier_register_updates(config) {
        let (message, signature) = tokio::task::spawn_blocking(move || {
            let message = supplier_register_message(&update);
            let signature = supplier_register_message_signature(&message);
            (message, signature)
        })
        .await
        .context("prepare supplier registration")?;
        let unchanged = !signature.is_empty()
            && state
                .last_signatures
                .get(&scope)
                .is_some_and(|previous| previous == &signature);
        // Registration declares channel metadata; the connection heartbeat owns liveness.
        // Reconnect and authentication create a fresh send state, while unchanged monitor
        // snapshots remain local and do not repeatedly reindex the server.
        if unchanged {
            continue;
        }
        if let Some(last_sent_at) = state.last_sent_at.get(&scope) {
            let elapsed = last_sent_at.elapsed();
            if elapsed < min_interval {
                tokio::time::sleep(min_interval - elapsed).await;
            }
        }
        send_supplier_message(outbound, message).await?;
        if scope == "session" {
            state.last_signatures.clear();
            state.last_sent_at.clear();
        }
        state.last_signatures.insert(scope.clone(), signature);
        state.last_sent_at.insert(scope, Instant::now());
    }
    Ok(())
}
