// Output credits travel with the message through the producer queue, transport
// copies and replay. Only ACK/cancel/drop releases them; receipt progress does not.
const SUPPLIER_OUTPUT_TOTAL_BYTES: usize = 128 * 1024 * 1024;
const SUPPLIER_OUTPUT_REQUEST_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone)]
struct SupplierOutputBudget {
    total: Arc<tokio::sync::Semaphore>,
    request: Arc<tokio::sync::Semaphore>,
    total_limit: usize,
    request_limit: usize,
}

#[derive(Debug)]
pub(crate) struct SupplierOutputCredit {
    _total: tokio::sync::OwnedSemaphorePermit,
    _request: tokio::sync::OwnedSemaphorePermit,
}

tokio::task_local! {
    static SUPPLIER_OUTPUT_BUDGET: SupplierOutputBudget;
}

async fn scope_supplier_output_budget<F: std::future::Future>(future: F) -> F::Output {
    static TOTAL: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    let budget = SupplierOutputBudget {
        total: TOTAL
            .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(SUPPLIER_OUTPUT_TOTAL_BYTES)))
            .clone(),
        request: Arc::new(tokio::sync::Semaphore::new(SUPPLIER_OUTPUT_REQUEST_BYTES)),
        total_limit: SUPPLIER_OUTPUT_TOTAL_BYTES,
        request_limit: SUPPLIER_OUTPUT_REQUEST_BYTES,
    };
    SUPPLIER_OUTPUT_BUDGET.scope(budget, future).await
}

fn supplier_output_memory_cost(message: &SupplierMessage) -> usize {
    fn value_cost(value: &serde_json::Value) -> usize {
        let owned = match value {
            serde_json::Value::String(text) => text.capacity(),
            serde_json::Value::Array(items) => items
                .iter()
                .fold(0usize, |n, v| n.saturating_add(value_cost(v)))
                .saturating_add(
                    items
                        .capacity()
                        .saturating_mul(std::mem::size_of::<serde_json::Value>()),
                ),
            serde_json::Value::Object(items) => items.iter().fold(0usize, |n, (k, v)| {
                n.saturating_add(k.capacity())
                    .saturating_add(value_cost(v))
                    .saturating_add(128)
            }),
            _ => 0,
        };
        owned.saturating_add(std::mem::size_of::<serde_json::Value>())
    }
    // Include the retained JSON tree, queue overhead and a transport copy. This
    // is a conservative payload budget, not an allocator/whole-process metric.
    message
        .payload
        .iter()
        .fold(
            512usize + message.id.capacity() + message.kind.capacity(),
            |n, (k, v)| {
                n.saturating_add(k.capacity())
                    .saturating_add(value_cost(v))
                    .saturating_add(128)
            },
        )
        .saturating_mul(2)
}

async fn reserve_supplier_output(
    outbound: &SupplierOutbound,
    message: &mut SupplierMessage,
) -> Result<()> {
    if message.output_credit.is_some() {
        return Ok(());
    }
    let Ok(budget) = SUPPLIER_OUTPUT_BUDGET.try_with(Clone::clone) else {
        // The transport control loop must never wait for business-output credit:
        // it receives ACK/cancel and sends heartbeat/admission errors.
        return Ok(());
    };
    let cost = supplier_output_memory_cost(message);
    let reserve = async {
        // Reserve per request first so a stalled request cannot hoard global
        // credit. An already decoded oversized message travels alone under its
        // request budget, and is still charged to the larger global budget.
        let request = budget
            .request
            .acquire_many_owned(cost.min(budget.request_limit) as u32)
            .await?;
        let total = budget
            .total
            .acquire_many_owned(cost.min(budget.total_limit) as u32)
            .await?;
        Ok::<_, anyhow::Error>(Arc::new(SupplierOutputCredit {
            _total: total,
            _request: request,
        }))
    };
    message.output_credit = Some(tokio::select! {
        _ = outbound.closed() => return Err(anyhow!("supplier transport closed")),
        result = reserve => result?,
    });
    Ok(())
}

#[cfg(test)]
mod output_budget_tests {
    use super::*;

    fn budget(total: usize, request: usize) -> SupplierOutputBudget {
        SupplierOutputBudget {
            total: Arc::new(tokio::sync::Semaphore::new(total)),
            request: Arc::new(tokio::sync::Semaphore::new(request)),
            total_limit: total,
            request_limit: request,
        }
    }

    fn message(id: &str, size: usize) -> SupplierMessage {
        SupplierMessage {
            id: id.into(),
            kind: "stream_chunk".into(),
            payload: serde_json::json!({"data":"x".repeat(size)})
                .as_object()
                .unwrap()
                .clone(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn durable_ack_and_last_writer_copy_release_credit_but_receipt_progress_does_not() {
        let first = message("slow", 4096);
        let cost = supplier_output_memory_cost(&first);
        let budget = budget(cost * 2, cost);
        let (tx, mut rx) = mpsc::channel(8);
        SUPPLIER_OUTPUT_BUDGET
            .scope(budget.clone(), send_supplier_message(&tx, first))
            .await
            .unwrap();
        let mut replay = SupplierReplayState::default();
        replay.flow_control_enabled = true;
        replay.enqueue(rx.recv().await.unwrap());
        let writer = replay.next_unsent().unwrap();
        let execution = writer.payload[SUPPLIER_STREAM_EXECUTION_ID_FIELD]
            .as_str()
            .unwrap()
            .to_owned();
        let mut next = Box::pin(SUPPLIER_OUTPUT_BUDGET.scope(
            budget.clone(),
            send_supplier_message(&tx, message("slow", 4096)),
        ));
        assert!(futures_util::poll!(&mut next).is_pending());
        replay.receive_progress("slow", 1, Some(&execution));
        assert_eq!(budget.request.available_permits(), 0);
        replay.reset_for_connection();
        let resent = replay.next_unsent().unwrap();
        assert!(Arc::ptr_eq(
            writer.output_credit.as_ref().unwrap(),
            resent.output_credit.as_ref().unwrap()
        ));
        drop(resent);
        replay.acknowledge("slow", 1, Some(&execution));
        assert!(
            futures_util::poll!(&mut next).is_pending(),
            "writer still owns the output"
        );
        drop(writer);
        tokio::time::timeout(Duration::from_secs(1), next)
            .await
            .unwrap()
            .unwrap();
        drop(rx.recv().await.unwrap());
        assert_eq!(budget.request.available_permits(), cost);
        assert_eq!(budget.total.available_permits(), cost * 2);
    }

    #[tokio::test]
    async fn stalled_request_does_not_reserve_global_credit_or_block_controls_and_other_requests() {
        let slow = message("slow", 4096);
        let cost = supplier_output_memory_cost(&slow);
        let budget = budget(cost * 3, cost);
        let (tx, mut rx) = mpsc::channel(8);
        SUPPLIER_OUTPUT_BUDGET
            .scope(budget.clone(), send_supplier_message(&tx, slow))
            .await
            .unwrap();
        let retained = rx.recv().await.unwrap();
        let mut waiting = Box::pin(SUPPLIER_OUTPUT_BUDGET.scope(
            budget.clone(),
            send_supplier_message(&tx, message("slow", 4096)),
        ));
        assert!(futures_util::poll!(&mut waiting).is_pending());
        assert_eq!(budget.total.available_permits(), cost * 2);
        // A control response runs outside the request's task-local scope.
        send_supplier_message(
            &tx,
            SupplierMessage {
                kind: "pong".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let pong = rx.recv().await.unwrap();
        assert!(pong.output_credit.is_none());
        let other = SupplierOutputBudget {
            request: Arc::new(tokio::sync::Semaphore::new(cost)),
            ..budget.clone()
        };
        SUPPLIER_OUTPUT_BUDGET
            .scope(other, send_supplier_message(&tx, message("fast", 64)))
            .await
            .unwrap();
        assert_eq!(rx.recv().await.unwrap().id, "fast");
        // Abort/drop releases any partially acquired permits without emitting a frame.
        drop(waiting);
        drop(retained);
        assert_eq!(budget.total.available_permits(), cost * 3);
        assert_eq!(budget.request.available_permits(), cost);
    }

    #[tokio::test]
    async fn global_pressure_waits_and_closed_output_cancels_partial_reservation() {
        let budget = budget(2048, 2048);
        let (tx, mut rx) = mpsc::channel(8);
        SUPPLIER_OUTPUT_BUDGET
            .scope(
                budget.clone(),
                send_supplier_message(&tx, message("first", 4096)),
            )
            .await
            .unwrap();
        let retained = rx.recv().await.unwrap();
        let other = SupplierOutputBudget {
            request: Arc::new(tokio::sync::Semaphore::new(2048)),
            ..budget.clone()
        };
        let mut waiting = Box::pin(SUPPLIER_OUTPUT_BUDGET.scope(
            other.clone(),
            send_supplier_message(&tx, message("second", 4096)),
        ));
        assert!(futures_util::poll!(&mut waiting).is_pending());
        assert_eq!(other.request.available_permits(), 0);
        rx.close();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), waiting)
                .await
                .unwrap()
                .is_err()
        );
        assert_eq!(other.request.available_permits(), 2048);
        drop(retained);
        assert_eq!(budget.total.available_permits(), 2048);
    }

    #[tokio::test]
    async fn legacy_writer_holds_credit_until_write_and_wire_format_is_unchanged() {
        let budget = budget(4096, 4096);
        let (tx, rx) = mpsc::channel(4);
        let (_, heartbeats) = mpsc::channel(4);
        let input = message("legacy", 4096);
        let expected = serde_json::to_vec(&input).unwrap();
        SUPPLIER_OUTPUT_BUDGET
            .scope(budget.clone(), send_supplier_message(&tx, input))
            .await
            .unwrap();
        let mut queue = SupplierTransportSendQueue {
            data: rx,
            heartbeats,
            current: None,
        };
        let codec = SupplierTransportCodec::new(None);
        let part = queue.next(&codec).await.unwrap().unwrap();
        assert_eq!(part.raw, expected);
        assert_eq!(
            budget.total.available_permits(),
            0,
            "the socket write still owns this frame"
        );
        let decoded: SupplierMessage = serde_json::from_slice(&part.raw).unwrap();
        assert!(decoded.output_credit.is_none());
        drop(part); // Socket write completed.
        drop(tx);
        assert!(queue.next(&codec).await.unwrap().is_none());
        assert_eq!(budget.total.available_permits(), 4096);
    }

    #[tokio::test]
    async fn execution_cancellation_releases_replay_credit() {
        let budget = budget(4096, 4096);
        let (tx, mut rx) = mpsc::channel(4);
        SUPPLIER_OUTPUT_BUDGET
            .scope(
                budget.clone(),
                send_supplier_message(&tx, message("cancel", 4096)),
            )
            .await
            .unwrap();
        let mut replay = SupplierReplayState::default();
        replay.enqueue(rx.recv().await.unwrap());
        let execution = replay.execution_ids["cancel"].clone();
        assert!(!replay.cancel_execution("cancel", "stale-execution"));
        assert_eq!(budget.total.available_permits(), 0);
        assert!(replay.cancel_execution("cancel", &execution));
        assert_eq!(budget.total.available_permits(), 4096);
    }

    #[tokio::test]
    async fn request_cancellation_drops_queued_late_output_instead_of_recreating_replay() {
        let budget = budget(8192, 8192);
        let (tx, mut rx) = mpsc::channel(4);
        SUPPLIER_OUTPUT_BUDGET
            .scope(
                budget.clone(),
                send_supplier_message(&tx, message("cancel-before-replay", 64)),
            )
            .await
            .unwrap();
        let mut replay = SupplierReplayState::default();
        replay.cancel_request("cancel-before-replay");
        replay.enqueue(rx.recv().await.unwrap());
        assert!(replay.next_unsent().is_none());
        assert_eq!(budget.total.available_permits(), 8192);
        assert!(!replay.execution_ids.contains_key("cancel-before-replay"));
        replay.cancelled.insert(
            "cancel-before-replay".into(),
            Instant::now() - Duration::from_secs(1),
        );
        assert!(
            !replay.is_cancelled("cancel-before-replay"),
            "cancellation retention is bounded"
        );
    }

    #[tokio::test]
    async fn duplex_input_overflow_does_not_wait_for_blocked_output_producer() {
        let tasks = SupplierRequestTasks::default();
        let (sender, _commands) = mpsc::channel(1);
        let (input_overflow, mut overflow) = tokio::sync::watch::channel(false);
        let (accepting, _) = tokio::sync::watch::channel(true);
        tasks.duplex_commands.lock().unwrap().insert(
            "duplex".into(),
            SupplierDuplexCommands {
                channel_id: "test".into(),
                sender,
                accepting,
                input_overflow,
            },
        );
        assert!(tasks.send_duplex(message("duplex", 1)));
        assert!(!*overflow.borrow());
        assert!(tasks.send_duplex(message("duplex", 2)));
        tokio::time::timeout(Duration::from_secs(1), overflow.changed())
            .await
            .unwrap()
            .unwrap();
        assert!(
            *overflow.borrow(),
            "the owning task must report an explicit terminal error"
        );
        assert!(
            tasks.send_duplex(message("duplex", 3)),
            "no duplicate rejection while closing"
        );
    }

    #[tokio::test]
    async fn duplex_overflow_reports_request_error_even_with_a_full_output_queue() {
        let (tx, mut rx) = mpsc::channel(1);
        tx.send(message("earlier-output", 1)).await.unwrap();
        let tasks = SupplierRequestTasks::default();
        let _guard = SupplierRequestTaskGuard::new(tasks.clone());
        tasks.spawn_duplex(
            Client::new(),
            default_supplier_config(),
            SupplierMessage {
                id: "overflow-before-connect".into(),
                kind: "duplex_open".into(),
                ..Default::default()
            },
            tx,
        );
        // No yield: deterministically fill the bounded mailbox before execution
        // begins, so the test never contacts an upstream provider.
        for _ in 0..65 {
            assert!(tasks.send_duplex(message("overflow-before-connect", 1)));
        }
        assert_eq!(rx.recv().await.unwrap().id, "earlier-output");
        let error = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(error.kind, "duplex_error");
        assert_eq!(error.payload["failure_scope"], "request");
        assert!(
            serde_json::to_string(&error.payload)
                .unwrap()
                .contains("duplex_input_backpressure")
        );
    }
}
