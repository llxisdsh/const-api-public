/// Count all returned UTF-8 bytes without retaining an entire second SSE body.
/// The upstream body is separate: it is still needed for errors and accounting.
pub(crate) struct SubscriptionOutputCapture {
    bytes: usize,
    raw: Option<String>,
}

impl SubscriptionOutputCapture {
    pub(crate) fn new(capture_raw: bool) -> Self {
        Self {
            bytes: 0,
            raw: capture_raw.then(String::new),
        }
    }

    pub(crate) fn push_str(&mut self, text: &str) {
        self.bytes = self.bytes.saturating_add(text.len());
        if let Some(raw) = self.raw.as_mut() {
            raw.push_str(text);
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.bytes
    }

    pub(crate) fn as_str(&self) -> &str {
        self.raw.as_deref().unwrap_or_default()
    }
}

pub(crate) fn append_subscription_audit(
    config: &SupplierConfig,
    path: &str,
    request_body: &str,
    response_payload: &serde_json::Map<String, serde_json::Value>,
    elapsed: Duration,
) -> Result<()> {
    let _ = append_subscription_usage(config, path, request_body, response_payload, elapsed);
    let _ = subscription_safety_record_result_for_path(
        config,
        response_payload,
        elapsed,
        request_body,
        path,
    );
    Ok(())
}

pub(crate) fn append_supplier_raw_exchange_log(
    config: &SupplierConfig,
    provider: &str,
    inbound_path: &str,
    upstream_url: &str,
    inbound_body: &str,
    upstream_body: &str,
    upstream_status: u16,
    upstream_content_type: &str,
    upstream_raw_body: &str,
    returned_content_type: &str,
    returned_body: &str,
    stream_debug: Option<&SubscriptionStreamDebug>,
) -> Result<()> {
    if !raw_debug_logs_enabled() {
        return Ok(());
    }

    let upstream_shape = response_shape_summary(upstream_raw_body);
    let returned_shape = response_shape_summary(returned_body);
    let record = serde_json::json!({
        "source": "supplier_exchange",
        "created_at_unix": now_unix(),
        "channel_id": config.channel_id,
        "channel_name": config.name,
        "provider": provider,
        "inbound": {
            "path": inbound_path,
            "body": inbound_body,
            "bytes": inbound_body.as_bytes().len()
        },
        "upstream": {
            "url": upstream_url,
            "request_body": upstream_body,
            "request_bytes": upstream_body.as_bytes().len(),
            "status": upstream_status,
            "content_type": upstream_content_type,
            "response_body": upstream_raw_body,
            "response_bytes": upstream_raw_body.as_bytes().len(),
            "shape": response_shape_json(&upstream_shape)
        },
        "returned": {
            "content_type": returned_content_type,
            "body": returned_body,
            "bytes": returned_body.as_bytes().len(),
            "shape": response_shape_json(&returned_shape)
        },
        "stream_debug": stream_debug.map(SubscriptionStreamDebug::to_json)
    });
    crate::logging::write_raw_log(&record)
}

fn response_shape_json(shape: &ResponseShapeSummary) -> serde_json::Value {
    serde_json::json!({
        "response_kind": shape.kind,
        "tool_call_count": shape.tool_call_count,
        "finish_reason": shape.finish_reason,
        "sse_event_count": shape.sse_event_count,
        "sse_done": shape.sse_done,
        "sse_last_event_type": shape.sse_last_event_type,
    })
}

fn insert_subscription_exchange_metrics(
    payload: &mut serde_json::Map<String, serde_json::Value>,
    inbound_body: &str,
    upstream_body: &str,
    upstream_raw_body: &str,
    returned_body: &str,
) {
    insert_subscription_exchange_metrics_with_returned_bytes(
        payload,
        inbound_body,
        upstream_body,
        upstream_raw_body,
        returned_body.len(),
    );
}

fn insert_subscription_exchange_metrics_with_returned_bytes(
    payload: &mut serde_json::Map<String, serde_json::Value>,
    inbound_body: &str,
    upstream_body: &str,
    upstream_raw_body: &str,
    returned_body_bytes: usize,
) {
    let upstream_model = requested_model_from_body_or_path(upstream_body, "");
    if let Some(model) = upstream_model.as_deref() {
        payload.insert("upstream_model".to_string(), serde_json::json!(model));
    }
    payload.insert(
        "stream".to_string(),
        serde_json::json!(json_body_stream_flag(inbound_body).unwrap_or(false)),
    );
    payload.insert(
        "upstream_stream".to_string(),
        serde_json::json!(json_body_stream_flag(upstream_body).unwrap_or(false)),
    );
    payload.insert(
        "inbound_request_bytes".to_string(),
        serde_json::json!(inbound_body.as_bytes().len()),
    );
    payload.insert(
        "upstream_request_bytes".to_string(),
        serde_json::json!(upstream_body.as_bytes().len()),
    );
    payload.insert(
        "response_wire_bytes".to_string(),
        serde_json::json!(upstream_raw_body.as_bytes().len()),
    );
    payload.insert(
        "upstream_response_bytes".to_string(),
        serde_json::json!(upstream_raw_body.as_bytes().len()),
    );
    payload.insert(
        "response_body_bytes".to_string(),
        serde_json::json!(returned_body_bytes),
    );
    payload.insert(
        "returned_response_bytes".to_string(),
        serde_json::json!(returned_body_bytes),
    );
    let status = payload
        .get("status")
        .and_then(serde_json::Value::as_u64)
        .and_then(|status| u16::try_from(status).ok())
        .unwrap_or_default();
    insert_subscription_route_model_failure_evidence(
        payload,
        status,
        upstream_raw_body,
        upstream_model.as_deref(),
    );
}

pub(crate) fn mark_subscription_stream_exchange(
    payload: &mut serde_json::Map<String, serde_json::Value>,
) {
    payload.insert("stream".to_string(), serde_json::json!(true));
    payload.insert("upstream_stream".to_string(), serde_json::json!(true));
}

fn insert_subscription_stream_debug_metrics(
    payload: &mut serde_json::Map<String, serde_json::Value>,
    stream_debug: &SubscriptionStreamDebug,
) {
    payload.insert(
        "upstream_send_ms".to_string(),
        serde_json::json!(stream_debug.upstream_send_ms),
    );
    payload.insert(
        "first_body_chunk_ms".to_string(),
        serde_json::json!(stream_debug.first_body_chunk_ms),
    );
    payload.insert(
        "first_sse_event_ms".to_string(),
        serde_json::json!(stream_debug.first_sse_event_ms),
    );
    payload.insert(
        "first_meaningful_event_ms".to_string(),
        serde_json::json!(stream_debug.first_meaningful_event_ms),
    );
    payload.insert(
        "routing_latency_ms".to_string(),
        serde_json::json!(stream_debug.routing_latency_ms()),
    );
    payload.insert(
        "body_read_ms".to_string(),
        serde_json::json!(stream_debug.body_read_ms),
    );
    payload.insert(
        "upstream_chunk_count".to_string(),
        serde_json::json!(stream_debug.upstream_chunk_count),
    );
    payload.insert(
        "largest_upstream_chunk_bytes".to_string(),
        serde_json::json!(stream_debug.largest_upstream_chunk_bytes),
    );
}

fn json_body_stream_flag(body: &str) -> Option<bool> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("stream").and_then(|stream| stream.as_bool()))
}

fn subscription_payload_i64(
    payload: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<i64> {
    payload.get(key).and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_u64().and_then(|raw| i64::try_from(raw).ok()))
    })
}

pub(crate) fn append_subscription_usage(
    config: &SupplierConfig,
    path: &str,
    request_body: &str,
    response_payload: &serde_json::Map<String, serde_json::Value>,
    elapsed: Duration,
) -> Result<()> {
    if is_local_subscription_block(response_payload) {
        return Ok(());
    }
    let wire = crate::surface_wire::SurfaceEnvelope::from_payload(response_payload)?;
    let status = u64::from(wire.status.unwrap_or_default());
    let response_body = wire.body.utf8()?;
    let error_kind = response_payload
        .get("error_kind")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let error = subscription_upstream_error_summary(status, response_body, error_kind);
    let (body_input_tokens, body_output_tokens) = extract_usage_tokens(response_body);
    // Streaming executors can keep the wire body out of the audit envelope and
    // attach the provider-reported counts directly. Prefer those exact values;
    // body parsing remains the compatibility fallback for buffered responses.
    let upstream_usage = response_payload
        .get(SUPPLIER_UPSTREAM_USAGE_FIELD)
        .and_then(|value| serde_json::from_value::<SupplierUpstreamUsage>(value.clone()).ok());
    let usage_i64 =
        |value: Option<u64>| value.map(|value| i64::try_from(value).unwrap_or(i64::MAX));
    let input_tokens = upstream_usage
        .as_ref()
        .and_then(|usage| usage_i64(usage.input_tokens))
        .or_else(|| subscription_payload_i64(response_payload, "input_tokens"))
        .unwrap_or(body_input_tokens);
    let output_tokens = upstream_usage
        .as_ref()
        .and_then(|usage| usage_i64(usage.output_tokens))
        .or_else(|| subscription_payload_i64(response_payload, "output_tokens"))
        .unwrap_or(body_output_tokens);
    let cache_read_tokens = upstream_usage
        .as_ref()
        .and_then(|usage| usage_i64(usage.cache_read_tokens));
    let cache_write_tokens = upstream_usage
        .as_ref()
        .and_then(|usage| usage_i64(usage.cache_write_tokens));
    let cache_write_5m_tokens = upstream_usage
        .as_ref()
        .and_then(|usage| usage_i64(usage.cache_write_5m_tokens));
    let cache_write_1h_tokens = upstream_usage
        .as_ref()
        .and_then(|usage| usage_i64(usage.cache_write_1h_tokens));
    let cache_write_tokens_for_base =
        cache_write_tokens.or_else(|| match (cache_write_5m_tokens, cache_write_1h_tokens) {
            (Some(five_minutes), Some(one_hour)) => Some(five_minutes.saturating_add(one_hour)),
            (Some(value), None) | (None, Some(value)) => Some(value),
            (None, None) => None,
        });
    let input_tokens_include_cache_read = upstream_usage
        .as_ref()
        .is_some_and(|usage| usage.input_tokens_include_cache_read);
    let input_tokens_include_cache_write = upstream_usage
        .as_ref()
        .is_some_and(|usage| usage.input_tokens_include_cache_write);
    let provider_usage_observed = upstream_usage.is_some() || input_tokens > 0 || output_tokens > 0;
    let cache_base_input_tokens =
        ((200..300).contains(&status) && provider_usage_observed).then(|| {
            input_tokens
                .saturating_add(if input_tokens_include_cache_read {
                    0
                } else {
                    cache_read_tokens.unwrap_or(0)
                })
                .saturating_add(if input_tokens_include_cache_write {
                    0
                } else {
                    cache_write_tokens_for_base.unwrap_or(0)
                })
        });
    let response_shape = response_shape_summary(response_body);
    let content_type = wire.header_utf8("content-type")?.unwrap_or("");
    let stream = response_payload
        .get("stream")
        .and_then(|value| value.as_bool())
        .or_else(|| json_body_stream_flag(request_body))
        .unwrap_or(false);
    let upstream_stream = response_payload
        .get("upstream_stream")
        .and_then(|value| value.as_bool())
        .unwrap_or_else(|| {
            content_type.contains("event-stream") || looks_like_sse_body(response_body)
        });
    let requested_model = requested_model_from_body_or_path(request_body, path);
    let upstream_model = response_payload
        .get("upstream_model")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| supplier_upstream_model_for_request(config, requested_model.as_deref()));
    let inbound_request_bytes = subscription_payload_i64(response_payload, "inbound_request_bytes")
        .unwrap_or_else(|| request_body.as_bytes().len() as i64);
    let upstream_request_bytes =
        subscription_payload_i64(response_payload, "upstream_request_bytes")
            .unwrap_or(inbound_request_bytes);
    let response_body_bytes = subscription_payload_i64(response_payload, "response_body_bytes")
        .unwrap_or_else(|| response_body.as_bytes().len() as i64);
    let response_wire_bytes = subscription_payload_i64(response_payload, "response_wire_bytes")
        .or_else(|| subscription_payload_i64(response_payload, "upstream_response_bytes"))
        .unwrap_or(response_body_bytes);
    let upstream_send_ms = subscription_payload_i64(response_payload, "upstream_send_ms");
    let first_body_chunk_ms = subscription_payload_i64(response_payload, "first_body_chunk_ms");
    let first_sse_event_ms = subscription_payload_i64(response_payload, "first_sse_event_ms");
    let first_meaningful_event_ms =
        subscription_payload_i64(response_payload, "first_meaningful_event_ms");
    let routing_latency_ms = subscription_payload_i64(response_payload, "routing_latency_ms");
    let body_read_ms = subscription_payload_i64(response_payload, "body_read_ms");
    let upstream_chunk_count = subscription_payload_i64(response_payload, "upstream_chunk_count");
    let largest_upstream_chunk_bytes =
        subscription_payload_i64(response_payload, "largest_upstream_chunk_bytes");
    let inbound_protocol = protocol_for_path(path);
    let upstream_protocol = response_payload
        .get("upstream_protocol")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| subscription_upstream_protocol(config).to_string());
    let ts_ms = now_unix_millis();
    let mut record = serde_json::json!({
        "ts": ts_ms / 1000,
        "ts_ms": ts_ms,
        "direction": "local_supplier",
        "ingress": "supplier_agent",
        "provider": subscription_provider_from_config(config),
        "channel_id": config.channel_id,
        "supplier_unit_id": config.node_id,
        "channel": config.name,
        "path": path,
        "inbound_protocol": inbound_protocol,
        "target_protocol": inbound_protocol,
        "upstream_protocol": upstream_protocol,
        "model": requested_model
            .unwrap_or_else(|| config.upstream_model.clone()),
        "upstream_model": upstream_model,
        "stream": stream,
        "upstream_stream": upstream_stream,
        "status": status,
        "ok": (200..300).contains(&status),
        "latency_ms": elapsed.as_millis(),
        "inbound_request_bytes": inbound_request_bytes,
        "upstream_request_bytes": upstream_request_bytes,
        "request_bytes": upstream_request_bytes,
        "response_wire_bytes": response_wire_bytes,
        "upstream_response_bytes": response_wire_bytes,
        "response_body_bytes": response_body_bytes,
        "response_bytes": response_wire_bytes,
        "upstream_send_ms": upstream_send_ms,
        "first_body_chunk_ms": first_body_chunk_ms,
        "body_read_ms": body_read_ms,
        "upstream_chunk_count": upstream_chunk_count,
        "largest_upstream_chunk_bytes": largest_upstream_chunk_bytes,
        "response_kind": response_shape.kind,
        "tool_call_count": response_shape.tool_call_count,
        "finish_reason": response_shape.finish_reason,
        "sse_event_count": response_shape.sse_event_count,
        "sse_done": response_shape.sse_done,
        "sse_last_event_type": response_shape.sse_last_event_type,
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "usage_source": if provider_usage_observed { "upstream" } else { "unavailable" },
        "error_kind": error_kind,
    });
    record["cache_read_tokens"] = serde_json::json!(cache_read_tokens);
    record["cache_write_tokens"] = serde_json::json!(cache_write_tokens);
    record["cache_write_5m_tokens"] = serde_json::json!(cache_write_5m_tokens);
    record["cache_write_1h_tokens"] = serde_json::json!(cache_write_1h_tokens);
    record["cache_base_input_tokens"] = serde_json::json!(cache_base_input_tokens);
    record["input_tokens_include_cache_read"] = serde_json::json!(input_tokens_include_cache_read);
    record["input_tokens_include_cache_write"] =
        serde_json::json!(input_tokens_include_cache_write);
    record["usage_meters"] =
        serde_json::json!(upstream_usage.as_ref().map(|usage| &usage.usage_meters));
    record["first_sse_event_ms"] = serde_json::json!(first_sse_event_ms);
    record["first_meaningful_event_ms"] = serde_json::json!(first_meaningful_event_ms);
    record["routing_latency_ms"] = serde_json::json!(routing_latency_ms);
    record["error"] = serde_json::Value::String(error);
    append_subscription_usage_record(&record)?;
    Ok(())
}

pub(crate) fn subscription_upstream_error_summary(
    status: u64,
    body: &str,
    error_kind: &str,
) -> String {
    if (200..300).contains(&status) {
        return String::new();
    }
    let parsed = subscription_error_value(body);
    let message = parsed
        .as_ref()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .or_else(|| value.pointer("/body/error/message"))
                .or_else(|| value.pointer("/response/error/message"))
                .or_else(|| value.pointer("/response/status_details/error/message"))
                .or_else(|| value.get("detail"))
                .or_else(|| value.get("message"))
                .or_else(|| value.get("error"))
        })
        .and_then(|value| value.as_str());
    if let Some(message) = message {
        let normalized = message.split_whitespace().collect::<Vec<_>>().join(" ");
        if !normalized.is_empty() {
            return normalized.chars().take(512).collect();
        }
    }
    if !error_kind.trim().is_empty() {
        return error_kind.trim().to_string();
    }
    format!("HTTP {status}")
}

fn subscription_error_value(body: &str) -> Option<serde_json::Value> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        return Some(value);
    }
    // Native Responses WebSocket auditing stores the exact event stream so
    // usage and byte accounting remain unchanged. Recover the last structured
    // provider error from SSE/NDJSON instead of degrading every terminal WS
    // error to a generic HTTP 502 summary.
    body.lines()
        .filter_map(|line| {
            let line = line.trim();
            let candidate = line.strip_prefix("data:").map(str::trim).unwrap_or(line);
            if candidate.is_empty() || candidate == "[DONE]" {
                return None;
            }
            serde_json::from_str::<serde_json::Value>(candidate).ok()
        })
        .filter(|value| {
            value.get("error").is_some()
                || value.pointer("/body/error").is_some()
                || value.pointer("/response/error").is_some()
                || value.pointer("/response/status_details/error").is_some()
        })
        .last()
}

pub(crate) fn read_subscription_usage_records(
    limit: usize,
    after_ms: Option<i64>,
) -> Result<Vec<serde_json::Value>> {
    query_subscription_usage_records(limit, after_ms)
}

pub(crate) fn subscription_usage_record_cursor_ms(record: &serde_json::Value) -> Option<i64> {
    record
        .get("ts_ms")
        .and_then(|value| value.as_i64())
        .or_else(|| {
            record
                .get("ts")
                .and_then(|value| value.as_i64())
                .map(|seconds| seconds.saturating_mul(1000))
        })
}

pub(crate) fn read_subscription_usage_stats() -> Result<Vec<serde_json::Value>> {
    query_subscription_usage_stats()
}

fn subscription_usage_stat_key(record: &serde_json::Value) -> String {
    let ts = record
        .get("ts")
        .and_then(|value| value.as_i64())
        .unwrap_or_else(now_unix);
    let bucket = unix_day_bucket(ts);
    [
        bucket.as_str(),
        record
            .get("provider")
            .and_then(|value| value.as_str())
            .unwrap_or(""),
        record
            .get("channel_id")
            .and_then(|value| value.as_str())
            .unwrap_or(""),
        record
            .get("model")
            .and_then(|value| value.as_str())
            .unwrap_or(""),
        record
            .get("path")
            .and_then(|value| value.as_str())
            .unwrap_or(""),
        record
            .get("status")
            .and_then(|value| value.as_u64())
            .map(|value| {
                if (200..300).contains(&value) {
                    "success"
                } else {
                    "failed"
                }
            })
            .unwrap_or("unknown"),
    ]
    .join("\u{0}")
}

fn updated_subscription_usage_stat(
    record: &serde_json::Value,
    existing: Option<serde_json::Value>,
) -> (String, String, serde_json::Value) {
    let ts = record
        .get("ts")
        .and_then(|value| value.as_i64())
        .unwrap_or_else(now_unix);
    let bucket = unix_day_bucket(ts);
    let key = subscription_usage_stat_key(record);
    let mut entry = existing.unwrap_or_else(|| {
        serde_json::json!({
            "bucket": bucket,
            "provider": record.get("provider").cloned().unwrap_or_default(),
            "channel_id": record.get("channel_id").cloned().unwrap_or_default(),
            "model": record.get("model").cloned().unwrap_or_default(),
            "path": record.get("path").cloned().unwrap_or_default(),
            "status": if record.get("ok").and_then(|value| value.as_bool()).unwrap_or(false) { "success" } else { "failed" },
            "requests": 0,
            "successes": 0,
            "failures": 0,
            "input_tokens": 0,
            "output_tokens": 0,
            "cache_read_tokens": 0,
            "cache_write_tokens": 0,
            "cache_write_5m_tokens": 0,
            "cache_write_1h_tokens": 0,
            "cache_base_input_tokens": 0,
            "cache_hit_rate": 0.0,
            "inbound_request_bytes": 0,
            "upstream_request_bytes": 0,
            "request_bytes": 0,
            "response_wire_bytes": 0,
            "response_body_bytes": 0,
            "response_bytes": 0,
            "latency_ms": 0
        })
    });
    if let Some(obj) = entry.as_object_mut() {
        increment_json_i64(obj, "requests", 1);
        if record
            .get("ok")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
        {
            increment_json_i64(obj, "successes", 1);
        } else {
            increment_json_i64(obj, "failures", 1);
        }
        increment_json_i64(
            obj,
            "input_tokens",
            record
                .get("input_tokens")
                .and_then(|value| value.as_i64())
                .unwrap_or(0),
        );
        increment_json_i64(
            obj,
            "output_tokens",
            record
                .get("output_tokens")
                .and_then(|value| value.as_i64())
                .unwrap_or(0),
        );
        for field in [
            "cache_read_tokens",
            "cache_write_tokens",
            "cache_write_5m_tokens",
            "cache_write_1h_tokens",
            "cache_base_input_tokens",
        ] {
            increment_json_i64(
                obj,
                field,
                record
                    .get(field)
                    .and_then(|value| value.as_i64())
                    .unwrap_or(0),
            );
        }
        let cache_read_tokens = obj
            .get("cache_read_tokens")
            .and_then(|value| value.as_i64())
            .unwrap_or(0);
        let cache_base_input_tokens = obj
            .get("cache_base_input_tokens")
            .and_then(|value| value.as_i64())
            .unwrap_or(0);
        obj.insert(
            "cache_hit_rate".to_string(),
            serde_json::json!(if cache_base_input_tokens > 0 {
                cache_read_tokens as f64 / cache_base_input_tokens as f64
            } else {
                0.0
            }),
        );
        increment_json_i64(
            obj,
            "inbound_request_bytes",
            record
                .get("inbound_request_bytes")
                .and_then(|value| value.as_i64())
                .unwrap_or_else(|| {
                    record
                        .get("request_bytes")
                        .and_then(|value| value.as_i64())
                        .unwrap_or(0)
                }),
        );
        increment_json_i64(
            obj,
            "upstream_request_bytes",
            record
                .get("upstream_request_bytes")
                .and_then(|value| value.as_i64())
                .unwrap_or_else(|| {
                    record
                        .get("request_bytes")
                        .and_then(|value| value.as_i64())
                        .unwrap_or(0)
                }),
        );
        increment_json_i64(
            obj,
            "request_bytes",
            record
                .get("request_bytes")
                .and_then(|value| value.as_i64())
                .unwrap_or(0),
        );
        increment_json_i64(
            obj,
            "response_wire_bytes",
            record
                .get("response_wire_bytes")
                .and_then(|value| value.as_i64())
                .unwrap_or_else(|| {
                    record
                        .get("response_bytes")
                        .and_then(|value| value.as_i64())
                        .unwrap_or(0)
                }),
        );
        increment_json_i64(
            obj,
            "response_body_bytes",
            record
                .get("response_body_bytes")
                .and_then(|value| value.as_i64())
                .unwrap_or_else(|| {
                    record
                        .get("response_bytes")
                        .and_then(|value| value.as_i64())
                        .unwrap_or(0)
                }),
        );
        increment_json_i64(
            obj,
            "response_bytes",
            record
                .get("response_bytes")
                .and_then(|value| value.as_i64())
                .unwrap_or(0),
        );
        increment_json_i64(
            obj,
            "latency_ms",
            record
                .get("latency_ms")
                .and_then(|value| value.as_i64())
                .unwrap_or(0),
        );
    }
    (key, bucket, entry)
}

fn increment_json_i64(obj: &mut serde_json::Map<String, serde_json::Value>, key: &str, delta: i64) {
    let current = obj.get(key).and_then(|value| value.as_i64()).unwrap_or(0);
    obj.insert(
        key.to_string(),
        serde_json::json!(current.saturating_add(delta)),
    );
}

fn unix_day_bucket(ts: i64) -> String {
    let day = ts.div_euclid(86_400) * 86_400;
    chrono_like_utc_day(day)
}

fn protocol_for_path(path: &str) -> &'static str {
    if path.contains("/v1/responses") {
        "openai_responses"
    } else if path.contains("/v1/messages") || path.ends_with("/messages") {
        "anthropic_messages"
    } else if path.contains("/v1beta/") {
        "gemini_native"
    } else if path.contains("/v1/chat/completions") {
        "openai_chat"
    } else {
        ""
    }
}

fn subscription_upstream_protocol(config: &SupplierConfig) -> &'static str {
    match subscription_provider_from_config(config).as_str() {
        "openai" | "codex" => "openai_responses",
        "gemini" | "antigravity" => "gemini_native",
        "claude" => "anthropic_messages",
        _ => protocol_for_api_format(config.api_format.trim()),
    }
}

fn protocol_for_api_format(api_format: &str) -> &'static str {
    match api_format {
        "openai_responses" => "openai_responses",
        "anthropic_messages" => "anthropic_messages",
        "gemini_native" => "gemini_native",
        "openai_chat" | "gemini_openai" => "openai_chat",
        _ => "",
    }
}

fn chrono_like_utc_day(day_unix: i64) -> String {
    const DAYS_FROM_CIVIL_1970: i64 = 719468;
    let z = day_unix.div_euclid(86_400) + DAYS_FROM_CIVIL_1970;
    let era = if z >= 0 { z } else { z - 146096 }.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096).div_euclid(365);
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2).div_euclid(153);
    let d = doy - (153 * mp + 2).div_euclid(5) + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if m <= 2 { 1 } else { 0 };
    format!("{year:04}-{m:02}-{d:02}")
}

pub(crate) fn subscription_usage_stats_path() -> PathBuf {
    crate::client_data_root()
        .join("state")
        .join("channel-stats-daily.json")
}

pub(crate) fn is_local_subscription_block(
    response_payload: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    let error_kind = response_payload
        .get("error_kind")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if !matches!(
        error_kind,
        "quota_exhausted" | "quota_reserved" | "rate_limited"
    ) {
        return false;
    }
    crate::surface_wire::SurfaceEnvelope::from_payload(response_payload)
        .ok()
        .and_then(|wire| wire.body.utf8().ok().map(str::to_string))
        .map(|body| body.contains("locally"))
        .unwrap_or(false)
}

const OFFICIAL_QUOTA_SIGNAL_MAX_AGE_SECONDS: i64 = 15 * 60;

#[cfg(test)]
mod subscription_error_summary_tests {
    use super::*;

    #[test]
    fn streaming_output_counts_bytes_without_retaining_the_response() {
        let mut counted = SubscriptionOutputCapture::new(false);
        let chunk = "中文 SSE data\n\n".repeat(1024);
        for _ in 0..1024 {
            counted.push_str(&chunk);
        }
        assert_eq!(counted.len(), chunk.len() * 1024);
        assert!(
            counted.raw.is_none(),
            "a long stream must not allocate a second full body"
        );
        let mut captured = SubscriptionOutputCapture::new(true);
        for text in ["data: ", "中文", "\r\n\r\n"] {
            captured.push_str(text);
        }
        assert_eq!(captured.as_str(), "data: 中文\r\n\r\n");
        assert_eq!(captured.len(), captured.as_str().len());
    }

    #[test]
    fn streaming_output_byte_metrics_match_buffered_metrics() {
        let inbound = r#"{"model":"model","stream":true}"#;
        let upstream = r#"{"model":"upstream-model","stream":true}"#;
        let raw = "data: {\"type\":\"error\",\"error\":{\"code\":\"model_not_found\",\"message\":\"model unavailable\"}}\n\n";
        let returned = "data: 中文\n\ndata: [DONE]\n\n";
        let mut old = serde_json::json!({"status":404})
            .as_object()
            .unwrap()
            .clone();
        let mut counted = old.clone();
        insert_subscription_exchange_metrics(&mut old, inbound, upstream, raw, returned);
        insert_subscription_exchange_metrics_with_returned_bytes(
            &mut counted,
            inbound,
            upstream,
            raw,
            returned.len(),
        );
        assert_eq!(
            counted, old,
            "route error evidence and byte metrics must not depend on output retention"
        );
        assert_eq!(counted["returned_response_bytes"], returned.len());
    }

    #[test]
    fn native_websocket_sse_keeps_the_provider_error_message() {
        let body = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp-1\"}}\n\n",
            "event: error\n",
            "data: {\"type\":\"error\",\"error\":{\"type\":\"server_error\",\"code\":\"server_is_overloaded\",\"message\":\"Selected model is at capacity. Please try a different model.\"}}\n\n"
        );
        assert_eq!(
            subscription_upstream_error_summary(503, body, "server_is_overloaded"),
            "Selected model is at capacity. Please try a different model."
        );
    }

    #[test]
    fn response_failed_nested_error_is_recovered_from_sse() {
        let body = "data: {\"type\":\"response.failed\",\"response\":{\"status_details\":{\"error\":{\"message\":\"upstream failed\"}}}}\n\n";
        assert_eq!(
            subscription_upstream_error_summary(502, body, "response.failed"),
            "upstream failed"
        );
    }

    #[test]
    fn wrapped_websocket_body_error_is_recovered_from_sse() {
        let body = "data: {\"type\":\"error\",\"status\":429,\"body\":{\"error\":{\"code\":\"websocket_connection_limit_reached\",\"message\":\"create a new websocket connection\"}}}\n\n";
        assert_eq!(
            subscription_upstream_error_summary(429, body, "websocket_connection_limit_reached"),
            "create a new websocket connection"
        );
    }
}
