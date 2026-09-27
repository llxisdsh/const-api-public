pub(crate) async fn handle_supplier_request(
    client: &Client,
    config: &SupplierConfig,
    msg: SupplierMessage,
    outbound: &SupplierOutbound,
) -> Result<()> {
    let id = msg.id.clone();
    let wire =
        crate::surface_wire::SurfaceEnvelope::from_payload(&msg.payload).map_err(|error| {
            platform_execution_failure("request_surface_decode", "invalid_surface_wire", error)
        })?;
    let stream_requested = wire.stream.unwrap_or_else(|| {
        wire.body
            .utf8()
            .ok()
            .and_then(|body| top_level_json_bool(body.as_bytes(), "stream"))
            .unwrap_or(false)
    });
    let response =
        forward_supplier_request(client, config, &msg, stream_requested, outbound).await?;
    if let Some(payload) = response {
        let msg = SupplierMessage {
            id,
            kind: "http_response".to_string(),
            payload,
        };
        send_supplier_message(outbound, msg).await?;
    }
    Ok(())
}

#[derive(Clone, Default)]
pub(crate) struct SupplierRequestTasks {
    active: Arc<StdMutex<HashMap<String, (u64, tokio::task::AbortHandle)>>>,
    duplex_commands: Arc<StdMutex<HashMap<String, mpsc::Sender<SupplierMessage>>>>,
    completed: Arc<StdMutex<HashMap<String, Instant>>>,
    next_generation: Arc<std::sync::atomic::AtomicU64>,
    responses_ws_pool: crate::proxy::ResponsesWsPool,
}

impl SupplierRequestTasks {
    pub(crate) fn spawn(
        &self,
        client: Client,
        supplier: SupplierConfig,
        message: SupplierMessage,
        outbound: SupplierOutbound,
    ) {
        let request_id = message.id.clone();
        if self.should_ignore_duplicate(&request_id) {
            return;
        }
        let Some(update_activity_guard) =
            crate::update_activity::try_begin_supplier_request(&request_id)
        else {
            return;
        };
        let task_id = request_id.clone();
        let activity_request_id = request_id.clone();
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed) + 1;
        let tasks = self.clone();
        let (start_tx, start_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _update_activity_guard = update_activity_guard;
            if start_rx.await.is_err() {
                return;
            }
            let panic_request_id = task_id.clone();
            let _guard = SupplierRequestTaskEntryGuard {
                request_id: task_id,
                generation,
                tasks: tasks.clone(),
            };
            let panic_outbound = outbound.clone();
            let guarded = crate::catch_runtime_panic(
                format!("supplier request {panic_request_id}"),
                crate::update_activity::scope_supplier_request_outputs(
                    activity_request_id,
                    execute_supplier_request_and_report(&client, &supplier, message, &outbound),
                ),
            )
            .await;
            if let Err(recovered) = guarded {
                let error = platform_execution_failure(
                    "supplier_request_execute",
                    "supplier_runtime_panic",
                    recovered.message,
                );
                let response = SupplierMessage {
                    id: panic_request_id,
                    kind: "error".to_string(),
                    payload: supplier_execution_error_payload(&error),
                };
                let _ = send_supplier_message(&panic_outbound, response).await;
            }
        });
        match self.active.lock() {
            Ok(mut active) => {
                if active.contains_key(&request_id) {
                    task.abort();
                    return;
                }
                active.insert(request_id, (generation, task.abort_handle()));
            }
            Err(_) => {
                task.abort();
            }
        }
        let _ = start_tx.send(());
    }

    pub(crate) fn spawn_duplex(
        &self,
        client: Client,
        supplier: SupplierConfig,
        message: SupplierMessage,
        outbound: SupplierOutbound,
    ) {
        let request_id = message.id.clone();
        if self.should_ignore_duplicate(&request_id) {
            return;
        }
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed) + 1;
        let tasks = self.clone();
        let task_id = request_id.clone();
        let (commands_tx, commands_rx) = mpsc::channel(64);
        let (start_tx, start_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            if start_rx.await.is_err() {
                return;
            }
            let panic_request_id = task_id.clone();
            let _guard = SupplierRequestTaskEntryGuard {
                request_id: task_id,
                generation,
                tasks: tasks.clone(),
            };
            let panic_outbound = outbound.clone();
            let guarded = crate::catch_runtime_panic(
                format!("supplier duplex session {panic_request_id}"),
                crate::update_activity::scope_supplier_request_outputs(
                    panic_request_id.clone(),
                    crate::proxy::run_supplier_responses_duplex(
                        &client,
                        &supplier,
                        message,
                        commands_rx,
                        &outbound,
                        tasks.responses_ws_pool.clone(),
                    ),
                ),
            )
            .await;
            let error = match guarded {
                Ok(Ok(())) => return,
                Ok(Err(error)) => error,
                Err(recovered) => platform_execution_failure(
                    "supplier_duplex_execute",
                    "supplier_runtime_panic",
                    recovered.message,
                ),
            };
            log::warn!(
                "[const-api][supplier] duplex session failed request_id={} channel_id={} source_driver={:?} error={error:#}",
                panic_request_id,
                supplier.channel_id,
                supplier.source_driver,
            );
            let mut payload = supplier_execution_error_payload(&error);
            if !payload.contains_key("failure_scope") {
                let evidence = execution_evidence_for_error(&error);
                let scope = if matches!(
                    evidence.fault_domain_hint.as_str(),
                    "supplier_config" | "upstream_provider" | "transport"
                ) {
                    "channel"
                } else {
                    // A duplex operation can fail before any model request is
                    // sent (for example, an endpoint without Responses WS).
                    // Without channel-wide evidence, do not evict otherwise
                    // healthy HTTP/SSE routes that share the same channel.
                    "operation"
                };
                payload.insert(
                    "failure_scope".to_string(),
                    serde_json::Value::String(scope.to_string()),
                );
            }
            let response = SupplierMessage {
                id: panic_request_id,
                kind: "duplex_error".to_string(),
                payload,
            };
            let _ = send_supplier_message(&panic_outbound, response).await;
        });
        match self.active.lock() {
            Ok(mut active) => {
                if active.contains_key(&request_id) {
                    task.abort();
                    return;
                }
                active.insert(request_id.clone(), (generation, task.abort_handle()));
            }
            Err(_) => {
                task.abort();
                return;
            }
        }
        match self.duplex_commands.lock() {
            Ok(mut commands) => {
                commands.insert(request_id.clone(), commands_tx);
            }
            Err(_) => {
                task.abort();
                if let Ok(mut active) = self.active.lock() {
                    active.remove(&request_id);
                }
                return;
            }
        }
        let _ = start_tx.send(());
    }

    pub(crate) async fn send_duplex(&self, message: SupplierMessage) -> bool {
        let sender = self
            .duplex_commands
            .lock()
            .ok()
            .and_then(|commands| commands.get(&message.id).cloned());
        match sender {
            Some(sender) => sender.send(message).await.is_ok(),
            None => false,
        }
    }

    fn should_ignore_duplicate(&self, request_id: &str) -> bool {
        if self
            .active
            .lock()
            .is_ok_and(|active| active.contains_key(request_id))
        {
            return true;
        }
        let now = Instant::now();
        self.completed.lock().is_ok_and(|mut completed| {
            completed.retain(|_, expires_at| *expires_at > now);
            completed.contains_key(request_id)
        })
    }

    pub(crate) fn cancel(&self, request_id: &str) -> bool {
        if let Ok(mut commands) = self.duplex_commands.lock() {
            commands.remove(request_id);
        }
        let handle = self
            .active
            .lock()
            .ok()
            .and_then(|mut active| active.remove(request_id));
        if let Some((_, handle)) = handle {
            handle.abort();
            true
        } else {
            false
        }
    }

    pub(crate) fn abort_all(&self) {
        let handles = self
            .active
            .lock()
            .map(|mut active| {
                active
                    .drain()
                    .map(|(_, (_, handle))| handle)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for handle in handles {
            handle.abort();
        }
        if let Ok(mut commands) = self.duplex_commands.lock() {
            commands.clear();
        }
        if let Ok(mut completed) = self.completed.lock() {
            completed.clear();
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.active
            .lock()
            .map(|active| active.len())
            .unwrap_or_default()
    }
}

struct SupplierRequestTaskEntryGuard {
    request_id: String,
    generation: u64,
    tasks: SupplierRequestTasks,
}

impl Drop for SupplierRequestTaskEntryGuard {
    fn drop(&mut self) {
        // Publish the completion tombstone before the request can disappear
        // from `active`, so a replay can never slip through that transition.
        if let Ok(mut completed) = self.tasks.completed.lock() {
            if let Ok(mut active) = self.tasks.active.lock() {
                if active
                    .get(&self.request_id)
                    .is_some_and(|(generation, _)| *generation == self.generation)
                {
                    if let Ok(mut commands) = self.tasks.duplex_commands.lock() {
                        commands.remove(&self.request_id);
                    }
                    completed.insert(
                        self.request_id.clone(),
                        Instant::now() + SUPPLIER_TUNNEL_RESUME_WINDOW,
                    );
                    active.remove(&self.request_id);
                }
            }
        }
    }
}

pub(crate) struct SupplierRequestTaskGuard {
    tasks: SupplierRequestTasks,
}

impl SupplierRequestTaskGuard {
    pub(crate) fn new(tasks: SupplierRequestTasks) -> Self {
        Self { tasks }
    }
}

impl Drop for SupplierRequestTaskGuard {
    fn drop(&mut self) {
        self.tasks.abort_all();
        self.tasks.responses_ws_pool.shutdown();
    }
}

async fn execute_supplier_request_and_report(
    client: &Client,
    supplier: &SupplierConfig,
    message: SupplierMessage,
    outbound: &SupplierOutbound,
) {
    let error_id = message.id.clone();
    if let Err(err) = handle_supplier_request(client, supplier, message, outbound).await {
        let payload = supplier_execution_error_payload(&err);
        if let Some(diagnostics) = payload
            .get(EXECUTION_EVIDENCE_FIELD)
            .and_then(|evidence| evidence.get("diagnostics"))
        {
            log::warn!(
                "[const-api][supplier] execution_failed request_id={} diagnostics={}",
                error_id,
                diagnostics
            );
        }
        let response = SupplierMessage {
            id: error_id,
            kind: "error".to_string(),
            payload,
        };
        let _ = send_supplier_message(outbound, response).await;
    }
}

fn supplier_execution_error_payload(
    error: &anyhow::Error,
) -> serde_json::Map<String, serde_json::Value> {
    // OAuth refresh happens before the inference request is sent. Preserve its
    // typed status so the server can fail over and mark an expired credential,
    // instead of turning a 401 invalid_grant into a generic 502 executor fault.
    if let Some(oauth) = oauth_wire_error_from_anyhow(error) {
        return subscription_wire_error_response(oauth.status, oauth.error_type, &oauth.message);
    }
    if let Some(mut payload) = crate::proxy::responses_websocket_connect_error_payload(error) {
        // The shared executor also serves HTTP bridges. Keep its ordinary
        // response envelope in addition to the native WS error frame.
        let data = payload.get("data").and_then(serde_json::Value::as_str).unwrap_or_default();
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::CONTENT_TYPE, reqwest::header::HeaderValue::from_static("application/json"));
        if let Ok(wire) = crate::surface_wire::http_response_payload(502, &headers, data.as_bytes()) {
            payload.extend(wire);
        }
        return payload;
    }
    let evidence = execution_evidence_for_error(error);
    let diagnostic_text = evidence.diagnostics.as_ref()
        .map(|diagnostics| format!(
            "supplier execution failed: class={}, phase={}, causes={}, os_error={:?}, diagnostic={}",
            diagnostics.error_class, diagnostics.phase, diagnostics.cause_codes.join(","),
            diagnostics.os_error, diagnostics.chain_fingerprint,
        ))
        .unwrap_or_default();
    let error_text = if evidence
        .diagnostics
        .as_ref()
        .is_some_and(|diagnostics| diagnostics.error_class != "unknown")
    {
        diagnostic_text.clone()
    } else {
        error.to_string()
    };
    let ambiguous_transport = evidence.error_code == "ambiguous_transport";
    let structured = error
        .chain()
        .find_map(|cause| structured_capability_mismatch(&cause.to_string()));
    let (status, body) = if ambiguous_transport {
        (
            502,
            serde_json::json!({
                "error_kind": "ambiguous_transport",
                "safe_to_retry_other_channel": false,
                "safe_to_retry_same_channel": false,
                "error": {
                    "message": format!("upstream request outcome is unknown; automatic replay was refused; {diagnostic_text}"),
                    "type": "ambiguous_transport",
                    "code": "ambiguous_transport",
                }
            })
            .to_string(),
        )
    } else if let Some(value) = structured.as_ref() {
        (422, serde_json::Value::Object(value.clone()).to_string())
    } else {
        let error_code = if evidence.error_code.trim().is_empty() {
            supplier_execution_error_code(&error_text)
        } else {
            evidence.error_code.as_str()
        };
        (
            502,
            serde_json::json!({
                "error_kind": "supplier_execution_error",
                "error": {
                    "message": error_text,
                    "type": "supplier_execution_error",
                    "code": error_code,
                }
            })
            .to_string(),
        )
    };
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    let mut payload = crate::surface_wire::http_response_payload(status, &headers, body.as_bytes())
        .unwrap_or_default();
    payload.insert(
        "error_kind".to_string(),
        serde_json::Value::String(
            if ambiguous_transport {
                "ambiguous_transport"
            } else {
                "supplier_execution_error"
            }
            .to_string(),
        ),
    );
    payload.insert(
        "code".to_string(),
        serde_json::Value::String(evidence.error_code.clone()),
    );
    attach_execution_evidence(&mut payload, &evidence);
    if ambiguous_transport {
        payload.insert("safe_to_retry_other_channel".to_string(), false.into());
        payload.insert("safe_to_retry_same_channel".to_string(), false.into());
    } else if evidence.recommendation.retry == "other_channel"
        && !evidence.upstream_attempted
        && !evidence.upstream_response_received
        && !evidence.response_started
        && !evidence.usable_output_produced
    {
        // Keep rolling upgrades safe: current servers adjudicate the structured
        // evidence, while older servers understand this conservative top-level
        // hint. It is only emitted when the provider was definitely untouched.
        payload.insert("safe_to_retry_other_channel".to_string(), true.into());
        payload.insert("safe_to_retry_same_channel".to_string(), false.into());
    }
    if let Some(value) = structured {
        for key in [
            "error",
            "error_kind",
            "issue_code",
            "feature",
            "protocol",
            "target_protocol",
            "model",
            "safe_to_retry_other_channel",
        ] {
            if let Some(field) = value.get(key) {
                payload.insert(key.to_string(), field.clone());
            }
        }
    }
    if let Some(fault) = ImprovementFault::execution_error(&error_text) {
        attach_improvement_faults(&mut payload, &[fault]);
    }
    payload
}

pub(crate) fn structured_capability_mismatch(
    error_text: &str,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let value = serde_json::from_str::<serde_json::Value>(error_text).ok()?;
    let object = value.as_object()?;
    if !matches!(
        object.get("error_kind").and_then(serde_json::Value::as_str),
        Some("capability_mismatch" | "conversation_state_incompatible")
    ) || object
        .get("safe_to_retry_other_channel")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        return None;
    }
    Some(object.clone())
}

fn supplier_execution_error_code(error_text: &str) -> &'static str {
    let normalized = error_text.to_ascii_lowercase();
    if normalized.contains("surface wire")
        || (normalized.contains("unknown variant") && normalized.contains("expected one of"))
    {
        return "invalid_surface_wire";
    }
    if normalized.contains("timeout") || normalized.contains("timed out") {
        return "upstream_timeout";
    }
    if normalized.contains("supplier transport closed") {
        return "supplier_transport_closed";
    }
    "supplier_execution_failed"
}

#[cfg(test)]
mod request_task_tests {
    use super::*;
    use crate::surface::{ApiOperation, ApiSurface};
    use crate::surface_wire::{SurfaceEnvelope, WireBody};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    static RETAINED_SUBSCRIPTION_E2E_LOCK: OnceLock<StdMutex<()>> = OnceLock::new();

    #[tokio::test]
    async fn active_and_recently_completed_request_ids_are_not_reexecuted() {
        let tasks = SupplierRequestTasks::default();
        let pending = tokio::spawn(std::future::pending::<()>());
        tasks
            .active
            .lock()
            .unwrap()
            .insert("same-request".to_string(), (1, pending.abort_handle()));
        assert!(tasks.should_ignore_duplicate("same-request"));
        tasks.active.lock().unwrap().clear();
        pending.abort();

        tasks.completed.lock().unwrap().insert(
            "same-request".to_string(),
            Instant::now() + SUPPLIER_TUNNEL_RESUME_WINDOW,
        );
        assert!(tasks.should_ignore_duplicate("same-request"));
        assert!(!tasks.should_ignore_duplicate("different-request"));
    }

    #[test]
    fn supplier_execution_errors_have_stable_safe_codes() {
        assert_eq!(
            supplier_execution_error_code(
                "unknown variant `openai`, expected one of `open_ai`, `anthropic`, `gemini`"
            ),
            "invalid_surface_wire"
        );
        assert_eq!(
            supplier_execution_error_code("upstream stream inactivity timeout"),
            "upstream_timeout"
        );
        assert_eq!(
            supplier_execution_error_code("supplier transport closed"),
            "supplier_transport_closed"
        );
    }

    #[test]
    fn expired_subscription_refresh_is_not_hidden_as_a_502_executor_error() {
        let error = anyhow::Error::new(SubscriptionOAuthError::from_response(
            "claude",
            400,
            &serde_json::json!({
                "error": "invalid_grant",
                "error_description": "Refresh token expired"
            }),
        ));
        let payload = supplier_execution_error_payload(&error);
        let wire = SurfaceEnvelope::from_payload(&payload).expect("OAuth error wire");
        let body: serde_json::Value =
            serde_json::from_str(wire.body.utf8().expect("OAuth error body"))
                .expect("OAuth error JSON");

        assert_eq!(wire.status, Some(401));
        assert_eq!(payload["error_kind"], "auth_error");
        assert_eq!(payload["safe_to_retry_other_channel"], true);
        assert_eq!(payload["safe_to_retry_same_channel"], false);
        assert_eq!(body["error"]["type"], "subscription_credential_expired");
    }

    #[test]
    fn route_mismatches_survive_supplier_error_reporting() {
        for (error_kind, issue_code, feature) in [
            (
                "capability_mismatch",
                "unsupported_required_feature",
                "video",
            ),
            (
                "conversation_state_incompatible",
                "conversation_state_incompatible",
                "protocol_continuation",
            ),
        ] {
            let error_text = format!("{issue_code}: route cannot preserve the request");
            let error = serde_json::json!({
                "error": error_text,
                "error_kind": error_kind,
                "issue_code": issue_code,
                "feature": feature,
                "protocol": "gemini_native",
                "target_protocol": "openai_chat",
                "model": "code-cheap",
                "safe_to_retry_other_channel": true
            })
            .to_string();

            let payload = supplier_execution_error_payload(&anyhow!(error));
            let wire = SurfaceEnvelope::from_payload(&payload).expect("route mismatch wire");
            let body: serde_json::Value =
                serde_json::from_str(wire.body.utf8().expect("route mismatch body"))
                    .expect("route mismatch json");

            assert_eq!(wire.status, Some(422));
            assert_eq!(payload["error_kind"], error_kind);
            assert_eq!(payload["error"], error_text);
            assert_eq!(payload["feature"], feature);
            assert_eq!(payload["safe_to_retry_other_channel"], true);
            assert_eq!(body["error_kind"], error_kind);
            assert_eq!(body["issue_code"], issue_code);
        }
    }

    #[test]
    fn execution_failure_payload_exposes_versioned_accounting_evidence() {
        let error = response_conversion_failure(false, false, "invalid provider response");
        let payload = supplier_execution_error_payload(&error);
        let evidence = &payload[EXECUTION_EVIDENCE_FIELD];

        assert_eq!(payload["error_kind"], "supplier_execution_error");
        assert_eq!(payload["code"], "response_conversion_failed");
        assert_eq!(evidence["version"], 1);
        assert_eq!(evidence["stage"], "response_protocol_convert");
        assert_eq!(evidence["recommendation"]["consumer_charge"], "deny");
        assert_eq!(evidence["recommendation"]["supplier_settlement"], "review");
        assert_eq!(evidence["recommendation"]["supplier_penalty"], "review");
    }

    #[test]
    fn pre_upstream_surface_rejection_is_retryable_for_old_and_new_servers() {
        let error = crate::channel_executor::surface_operation_not_supported_error(
            "channel does not implement this resource family",
        );
        let payload = supplier_execution_error_payload(&error);
        let evidence = &payload[EXECUTION_EVIDENCE_FIELD];

        assert_eq!(payload["safe_to_retry_other_channel"], true);
        assert_eq!(payload["safe_to_retry_same_channel"], false);
        assert_eq!(evidence["upstream_attempted"], false);
        assert_eq!(evidence["recommendation"]["retry"], "other_channel");
        assert_eq!(evidence["recommendation"]["supplier_penalty"], "ineligible");
    }

    #[test]
    fn ambiguous_transport_payload_forbids_every_automatic_replay() {
        let error = crate::channel_executor::ambiguous_transport_error("outcome unknown");
        let payload = supplier_execution_error_payload(&error);
        let wire = SurfaceEnvelope::from_payload(&payload).expect("ambiguous transport wire");
        let body: serde_json::Value =
            serde_json::from_str(wire.body.utf8().expect("ambiguous transport body"))
                .expect("ambiguous transport json");
        let evidence = &payload[EXECUTION_EVIDENCE_FIELD];

        assert_eq!(wire.status, Some(502));
        assert_eq!(payload["error_kind"], "ambiguous_transport");
        assert_eq!(payload["code"], "ambiguous_transport");
        assert_eq!(payload["safe_to_retry_other_channel"], false);
        assert_eq!(payload["safe_to_retry_same_channel"], false);
        assert_eq!(body["error_kind"], "ambiguous_transport");
        assert_eq!(body["safe_to_retry_other_channel"], false);
        assert_eq!(evidence["stage"], "upstream_transport");
        assert_eq!(evidence["recommendation"]["supplier_settlement"], "review");
        assert_eq!(evidence["recommendation"]["supplier_penalty"], "ineligible");
        assert_eq!(evidence["recommendation"]["retry"], "none");
    }

    fn request_message(id: &str, raw_query: &str, stream: bool) -> SupplierMessage {
        let body = format!(r#"{{"model":"code-cheap","messages":[],"stream":{stream}}}"#);
        let wire = SurfaceEnvelope {
            surface: Some(ApiSurface::OpenAi),
            operation: Some(ApiOperation::ChatCompletions),
            protocol: Some("openai_chat".to_string()),
            method: "POST".to_string(),
            path: "/v1/chat/completions".to_string(),
            raw_query: raw_query.to_string(),
            has_query: !raw_query.is_empty(),
            body: WireBody::from_bytes(body.as_bytes()),
            stream: Some(stream),
            ..SurfaceEnvelope::default()
        };
        SupplierMessage {
            id: id.to_string(),
            kind: "http_request".to_string(),
            payload: crate::surface_wire::wire_payload(wire).expect("wire payload"),
        }
    }

    fn cancel_message(id: &str) -> String {
        serde_json::to_string(&SupplierMessage {
            id: id.to_string(),
            kind: "request_cancel".to_string(),
            payload: serde_json::Map::new(),
        })
        .expect("cancel json")
    }

    async fn wait_for_task_count(tasks: &SupplierRequestTasks, expected: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if tasks.len() == expected {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("request task count timeout");
    }

    async fn read_http_request(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
        let mut request = Vec::new();
        let mut expected_len = None;
        loop {
            let mut chunk = [0_u8; 1024];
            let read = stream.read(&mut chunk).await.expect("read request");
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
            if expected_len.is_none() {
                if let Some(header_end) =
                    request.windows(4).position(|window| window == b"\r\n\r\n")
                {
                    let content_length = String::from_utf8_lossy(&request[..header_end])
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    expected_len = Some(header_end + 4 + content_length);
                }
            }
            if expected_len.is_some_and(|expected| request.len() >= expected) {
                break;
            }
        }
        request
    }

    fn supplier_for(address: std::net::SocketAddr) -> SupplierConfig {
        let mut supplier = default_supplier_config();
        supplier.api_format = "openai_chat".to_string();
        supplier.upstream_base_url = format!("http://{address}");
        supplier.surface_bindings = crate::source_driver_surface_bindings(
            crate::source_driver::SourceDriverId::CustomEndpoint,
            &supplier.upstream_base_url,
        );
        supplier.public_model = "code-cheap".to_string();
        supplier.upstream_model = "code-cheap".to_string();
        supplier
    }

    fn retained_subscription_supplier(provider: &str, credential_ref: String) -> SupplierConfig {
        let mut supplier = default_supplier_config();
        supplier.enabled = true;
        let source_driver = match provider {
            "openai" => crate::source_driver::SourceDriverId::OpenAiSubscription,
            "claude" => crate::source_driver::SourceDriverId::ClaudeSubscription,
            "antigravity" => crate::source_driver::SourceDriverId::GeminiSubscription,
            "grok" => crate::source_driver::SourceDriverId::GrokSubscription,
            other => panic!("unsupported retained subscription provider: {other}"),
        };
        let contract = crate::channel_v2_contract_for_source(source_driver);
        supplier.source_driver = source_driver;
        supplier.executor = contract.executor;
        supplier.default_target = contract.default_target;
        supplier.discovery = contract.discovery;
        supplier.surface_bindings = crate::source_driver_surface_bindings(source_driver, "");
        supplier.kind = "custom_endpoint".to_string();
        supplier.api_format = "openai_chat".to_string();
        supplier.upstream_base_url = "https://legacy-conflict.invalid/v1".to_string();
        supplier.upstream_model = match provider {
            "antigravity" => "gemini-2.0-flash".to_string(),
            "grok" => "grok-4.5".to_string(),
            _ => "gpt-5-codex".to_string(),
        };
        supplier.models = vec![supplier.upstream_model.clone()];
        supplier.credential_ref = credential_ref.clone();
        supplier.subscription = SubscriptionAdapterConfig {
            platform: match provider {
                "openai" => "gemini",
                "claude" => "openai",
                "antigravity" => "claude",
                "grok" => "antigravity",
                _ => unreachable!(),
            }
            .to_string(),
            credential_ref,
            audit_enabled: false,
            ..default_subscription_adapter_config()
        };
        supplier
    }

    fn retained_subscription_request(
        id: &str,
        surface: ApiSurface,
        operation: ApiOperation,
        protocol: &str,
        path: &str,
        body: &str,
    ) -> SupplierMessage {
        let wire = SurfaceEnvelope {
            surface: Some(surface),
            operation: Some(operation),
            protocol: Some(protocol.to_string()),
            method: "POST".to_string(),
            path: path.to_string(),
            body: WireBody::from_bytes(body.as_bytes()),
            stream: Some(false),
            ..SurfaceEnvelope::default()
        };
        SupplierMessage {
            id: id.to_string(),
            kind: "http_request".to_string(),
            payload: crate::surface_wire::wire_payload(wire).expect("wire request"),
        }
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // Serializes a test-only process-wide upstream fixture.
    async fn typed_retained_subscription_transport_ignores_legacy_conflicts_for_all_providers() {
        let _guard = RETAINED_SUBSCRIPTION_E2E_LOCK
            .get_or_init(|| StdMutex::new(()))
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind subscription upstream");
        let address = listener.local_addr().expect("subscription address");
        let response_body = br#"{"error":{"message":"retained subscription unavailable"}}"#;
        let upstream = tokio::spawn(async move {
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().await.expect("accept subscription");
                let _ = read_http_request(&mut stream).await;
                let headers = format!(
                    "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nSet-Cookie: a=1\r\nSet-Cookie: b=2\r\nX-Repeated: first\r\nX-Repeated: second\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response_body.len()
                );
                stream.write_all(headers.as_bytes()).await.unwrap();
                stream.write_all(response_body).await.unwrap();
            }
        });
        let endpoint = format!("http://{address}/subscription");
        for provider in ["openai", "claude", "antigravity", "grok"] {
            set_retained_subscription_upstream_override(provider, Some(&endpoint));
        }
        let credentials = tempfile::tempdir().expect("credential dir");
        let cases = [
            (
                "openai",
                ApiSurface::OpenAi,
                ApiOperation::Responses,
                "openai_responses",
                "/v1/responses",
                r#"{"model":"gpt-5-codex","input":"hi","stream":false}"#,
            ),
            (
                "claude",
                ApiSurface::Anthropic,
                ApiOperation::Messages,
                "anthropic_messages",
                "/v1/messages",
                r#"{"model":"gpt-5-codex","max_tokens":32,"messages":[{"role":"user","content":"hi"}]}"#,
            ),
            (
                "antigravity",
                ApiSurface::Gemini,
                ApiOperation::GenerateContent,
                "gemini_native",
                "/v1beta/models/gemini-2.0-flash:generateContent",
                r#"{"contents":[{"parts":[{"text":"hi"}]}]}"#,
            ),
            (
                "grok",
                ApiSurface::OpenAi,
                ApiOperation::Responses,
                "openai_responses",
                "/v1/responses",
                r#"{"model":"grok-4.5","input":"hi","stream":false}"#,
            ),
        ];
        for (provider, surface, operation, protocol, path, body) in cases {
            let credential = credentials.path().join(format!("{provider}.json"));
            fs::write(
                &credential,
                serde_json::to_vec(&serde_json::json!({
                    "access_token": format!("{provider}-token"),
                    "refresh_token": format!("{provider}-refresh"),
                    "project_id": "test-project",
                    "expired": now_unix() + 3600,
                }))
                .unwrap(),
            )
            .unwrap();
            let agent = SupplierAgentConfig::single(retained_subscription_supplier(
                provider,
                credential.to_string_lossy().to_string(),
            ));
            let request =
                retained_subscription_request(provider, surface, operation, protocol, path, body);
            let (outbound, mut inbound) = mpsc::channel(4);
            handle_supplier_inbound_text(
                &Client::new(),
                &agent,
                &outbound,
                &serde_json::to_string(&request).unwrap(),
            )
            .await;
            let response = inbound.recv().await.expect("subscription response");
            assert_eq!(response.kind, "http_response", "provider={provider}");
            for legacy in ["status", "headers", "body", "start", "chunk"] {
                assert!(
                    !response.payload.contains_key(legacy),
                    "provider={provider} leaked {legacy}"
                );
            }
            let wire = SurfaceEnvelope::from_payload(&response.payload).expect("response wire");
            let headers = wire.header_map().expect("response headers");
            assert_eq!(
                wire.status,
                Some(503),
                "provider={provider} body={}",
                String::from_utf8_lossy(&wire.body.decode().unwrap())
            );
            assert_eq!(
                wire.body.decode().unwrap(),
                response_body,
                "provider={provider}"
            );
            assert_eq!(
                headers
                    .get_all("set-cookie")
                    .iter()
                    .map(|value| value.to_str().unwrap())
                    .collect::<Vec<_>>(),
                ["a=1", "b=2"],
                "provider={provider}"
            );
            assert_eq!(
                headers
                    .get_all("x-repeated")
                    .iter()
                    .map(|value| value.to_str().unwrap())
                    .collect::<Vec<_>>(),
                ["first", "second"],
                "provider={provider}"
            );
            assert_eq!(
                headers.get("content-length").unwrap().to_str().unwrap(),
                response_body.len().to_string(),
                "provider={provider}"
            );
        }
        for provider in ["openai", "claude", "antigravity", "grok"] {
            set_retained_subscription_upstream_override(provider, None);
        }
        upstream.await.expect("subscription upstream");
    }

    #[tokio::test]
    async fn requests_remain_concurrent_and_cancel_removes_pending_task() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind upstream");
        let address = listener.local_addr().expect("upstream address");
        let slow_entered = Arc::new(Notify::new());
        let release_slow = Arc::new(Notify::new());
        let server = {
            let slow_entered = slow_entered.clone();
            let release_slow = release_slow.clone();
            tokio::spawn(async move {
                let mut handlers = Vec::new();
                for _ in 0..2 {
                    let (mut stream, _) = listener.accept().await.expect("accept request");
                    let slow_entered = slow_entered.clone();
                    let release_slow = release_slow.clone();
                    handlers.push(tokio::spawn(async move {
                        let request = read_http_request(&mut stream).await;
                        if request.starts_with(b"POST /v1/chat/completions?slow=1 ") {
                            slow_entered.notify_one();
                            release_slow.notified().await;
                            return;
                        }
                        let body = br#"{"id":"fast","choices":[]}"#;
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        stream.write_all(response.as_bytes()).await.expect("write headers");
                        stream.write_all(body).await.expect("write body");
                    }));
                }
                for handler in handlers {
                    let _ = handler.await;
                }
            })
        };
        let agent = SupplierAgentConfig::single(supplier_for(address));
        let tasks = SupplierRequestTasks::default();
        let (outbound, mut outbound_rx) = mpsc::channel(8);

        let slow = serde_json::to_string(&request_message("slow", "slow=1", false)).unwrap();
        handle_supplier_inbound_text_with_tasks(
            &Client::new(),
            &agent,
            &outbound,
            &outbound,
            Some(&tasks),
            &slow,
        )
        .await;
        slow_entered.notified().await;
        wait_for_task_count(&tasks, 1).await;

        let fast = serde_json::to_string(&request_message("fast", "fast=1", false)).unwrap();
        handle_supplier_inbound_text_with_tasks(
            &Client::new(),
            &agent,
            &outbound,
            &outbound,
            Some(&tasks),
            &fast,
        )
        .await;
        let response = tokio::time::timeout(Duration::from_secs(2), outbound_rx.recv())
            .await
            .expect("fast response timeout")
            .expect("fast response");
        assert_eq!(response.id, "fast");
        assert_eq!(
            response.kind, "http_response",
            "fast response payload: {:?}",
            response.payload
        );
        wait_for_task_count(&tasks, 1).await;

        handle_supplier_inbound_text_with_tasks(
            &Client::new(),
            &agent,
            &outbound,
            &outbound,
            Some(&tasks),
            &cancel_message("slow"),
        )
        .await;
        wait_for_task_count(&tasks, 0).await;
        release_slow.notify_one();
        server.await.expect("upstream server");
        assert!(
            tokio::time::timeout(Duration::from_millis(100), outbound_rx.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn cancel_after_first_chunk_aborts_remaining_stream() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind upstream");
        let address = listener.local_addr().expect("upstream address");
        let release = Arc::new(Notify::new());
        let server = {
            let release = release.clone();
            tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.expect("accept request");
                let _ = read_http_request(&mut stream).await;
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n")
                    .await
                    .expect("write stream headers");
                let first = b"data: first\n\n";
                stream
                    .write_all(format!("{:x}\r\n", first.len()).as_bytes())
                    .await
                    .unwrap();
                stream.write_all(first).await.unwrap();
                stream.write_all(b"\r\n").await.unwrap();
                stream.flush().await.unwrap();
                release.notified().await;
                let _ = stream.write_all(b"0\r\n\r\n").await;
            })
        };
        let agent = SupplierAgentConfig::single(supplier_for(address));
        let tasks = SupplierRequestTasks::default();
        let (outbound, mut outbound_rx) = mpsc::channel(8);
        let request = serde_json::to_string(&request_message("stream", "", true)).unwrap();
        handle_supplier_inbound_text_with_tasks(
            &Client::new(),
            &agent,
            &outbound,
            &outbound,
            Some(&tasks),
            &request,
        )
        .await;
        let start = tokio::time::timeout(Duration::from_secs(2), outbound_rx.recv())
            .await
            .expect("stream start timeout")
            .expect("stream start");
        let chunk = tokio::time::timeout(Duration::from_secs(2), outbound_rx.recv())
            .await
            .expect("stream chunk timeout")
            .expect("stream chunk");
        assert_eq!(start.kind, "stream_start");
        assert_eq!(chunk.kind, "stream_chunk");

        handle_supplier_inbound_text_with_tasks(
            &Client::new(),
            &agent,
            &outbound,
            &outbound,
            Some(&tasks),
            &cancel_message("stream"),
        )
        .await;
        wait_for_task_count(&tasks, 0).await;
        release.notify_one();
        server.await.expect("upstream server");
        assert!(
            tokio::time::timeout(Duration::from_millis(100), outbound_rx.recv())
                .await
                .is_err()
        );
    }
}
