use crate::config::*;
use crate::model::*;
use crate::supplier::*;
use crate::*;
use anyhow::{Result, anyhow, bail};
use http_body_util::BodyExt;
use reqwest::Client;
use tauri::State;

pub(crate) struct LocalProxyTestSpec {
    pub(crate) protocol: String,
    pub(crate) path: String,
    pub(crate) body: serde_json::Value,
    pub(crate) accept_event_stream: bool,
}

pub(crate) fn normalize_access_test_protocol(protocol: Option<&str>) -> String {
    match protocol.map(str::trim).unwrap_or("") {
        "openai_chat" => "openai_chat".to_string(),
        "anthropic_messages" => "anthropic_messages".to_string(),
        "gemini_native" => "gemini_native".to_string(),
        _ => "openai_responses".to_string(),
    }
}

pub(crate) fn local_proxy_test_spec(
    protocol: &str,
    model: &str,
    prompt: &str,
) -> LocalProxyTestSpec {
    match protocol {
        "openai_chat" => LocalProxyTestSpec {
            protocol: "openai_chat".to_string(),
            path: "/v1/chat/completions".to_string(),
            body: serde_json::json!({
                "model": model,
                "messages": [
                    {"role": "user", "content": prompt}
                ]
            }),
            accept_event_stream: false,
        },
        "anthropic_messages" => LocalProxyTestSpec {
            protocol: "anthropic_messages".to_string(),
            path: "/v1/messages".to_string(),
            body: serde_json::json!({
                "model": model,
                "messages": [
                    {"role": "user", "content": prompt}
                ],
                "max_tokens": 128
            }),
            accept_event_stream: false,
        },
        "gemini_native" => LocalProxyTestSpec {
            protocol: "gemini_native".to_string(),
            path: format!(
                "/v1beta/models/{}:generateContent",
                model.replace('/', "%2F")
            ),
            body: serde_json::json!({
                "contents": [{"parts": [{"text": prompt}]}],
                "generationConfig": {"maxOutputTokens": 128}
            }),
            accept_event_stream: false,
        },
        _ => LocalProxyTestSpec {
            protocol: "openai_responses".to_string(),
            path: "/v1/responses".to_string(),
            body: serde_json::json!({
            "model": model,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": prompt}]
            }],
            "store": false,
            "stream": true
            }),
            accept_event_stream: true,
        },
    }
}

#[tauri::command]
pub(crate) async fn debug_protocol_exchange_inner(
    state: State<'_, AppState>,
    target_type: String,
    target_id: Option<String>,
    inbound_protocol: String,
    target_protocol: String,
    model: String,
    prompt: String,
    stream: bool,
    skip_local_short_circuit: bool,
    experimental_features: Vec<String>,
    execute: bool,
) -> Result<ProtocolDebugResult, String> {
    let cfg = load_config_from_path(&state.config_path).map_err(|err| err.to_string())?;
    let target_type = target_type.trim().to_string();
    let target_id = target_id.unwrap_or_default().trim().to_string();
    let requested_model = model.trim();
    if requested_model.is_empty() {
        return Err("model is required".to_string());
    }
    let upstream_model = debug_upstream_model(&cfg, &target_type, &target_id, requested_model)
        .map_err(|err| format!("target: {err}"))?;
    let mut preview = crate::protocol::build_exchange_preview(
        &inbound_protocol,
        &target_protocol,
        requested_model,
        &upstream_model,
        &prompt,
        stream,
    )
    .map_err(|err| format!("conversion: {err}"))?;
    apply_debug_experimental_features(&mut preview, &experimental_features)
        .map_err(|err| format!("experiment: {err}"))?;
    let execution_path =
        debug_execution_path(&target_type, &preview).map_err(|err| format!("target: {err}"))?;
    let request_headers = debug_request_headers(
        &target_type,
        &target_id,
        &cfg.api_key,
        skip_local_short_circuit,
    );
    let request_body_bytes = preview.body.to_string().as_bytes().len();
    let mut result = ProtocolDebugResult {
        target_type: target_type.clone(),
        target_id: target_id.clone(),
        inbound_protocol: preview.inbound_protocol.clone(),
        target_protocol: preview.target_protocol.clone(),
        requested_model: preview.requested_model.clone(),
        upstream_model: preview.upstream_model.clone(),
        conversion_level: preview.conversion_level.clone(),
        path: execution_path.clone(),
        request_headers: request_headers.clone(),
        request_body: preview.body.clone(),
        unsupported_fields: preview.unsupported_fields.clone(),
        lossy_warnings: preview.lossy_warnings.clone(),
        executed: execute,
        http_status: None,
        latency_ms: None,
        content_type: String::new(),
        content: String::new(),
        raw: String::new(),
        metrics: protocol_debug_metrics(request_body_bytes, "", 0, ""),
        error_layer: String::new(),
    };
    if !execute {
        return Ok(result);
    }

    let client = long_http_client().clone();
    let started_at = std::time::Instant::now();
    let execution = match target_type.as_str() {
        "local_channel" => execute_debug_local_channel(&cfg, &client, &target_id, &preview).await,
        "platform_auto" | "platform_node" | "platform_provider" => {
            execute_debug_platform(
                &cfg,
                &client,
                &target_type,
                &target_id,
                &preview,
                &execution_path,
                skip_local_short_circuit,
            )
            .await
        }
        _ => Err(anyhow!("unknown debug target type: {target_type}")),
    };
    result.latency_ms = Some(started_at.elapsed().as_millis());
    match execution {
        Ok(http) => {
            result.http_status = Some(http.status);
            if !(200..300).contains(&http.status) {
                result.error_layer = debug_error_layer_for_target(&target_type).to_string();
            }
            result.content_type = http.content_type;
            result.content = protocol_debug_response_content(&http.raw);
            result.metrics = protocol_debug_metrics(
                request_body_bytes,
                &http.raw,
                http.raw_bytes,
                &result.content,
            );
            result.raw = http.raw;
        }
        Err(err) => {
            let raw = err.to_string();
            result.error_layer = debug_error_layer_for_target(&target_type).to_string();
            result.content = raw.clone();
            result.metrics = protocol_debug_metrics(
                request_body_bytes,
                &raw,
                raw.as_bytes().len(),
                &result.content,
            );
            result.raw = raw;
        }
    }
    Ok(result)
}

pub(crate) fn debug_error_layer_for_target(target_type: &str) -> &'static str {
    if target_type == "local_channel" {
        "upstream"
    } else {
        "local_proxy"
    }
}

pub(crate) fn protocol_debug_response_content(raw: &str) -> String {
    protocol_safe_response_content(raw)
}

pub(crate) fn protocol_debug_metrics(
    request_body_bytes: usize,
    raw: &str,
    raw_bytes: usize,
    content: &str,
) -> ProtocolDebugMetrics {
    let shape = response_shape_summary(raw);
    ProtocolDebugMetrics {
        request_body_bytes,
        response_raw_bytes: raw_bytes,
        response_content_bytes: content.as_bytes().len(),
        response_content_chars: content.chars().count(),
        response_raw_lines: if raw.is_empty() {
            0
        } else {
            raw.lines().count()
        },
        response_sse_events: shape.sse_event_count,
        response_sse_done: shape.sse_done,
        response_sse_last_event_type: shape.sse_last_event_type,
        response_kind: shape.kind,
        tool_call_count: shape.tool_call_count,
        finish_reason: shape.finish_reason,
    }
}

pub(crate) struct DebugHttpResult {
    pub(crate) status: u16,
    pub(crate) content_type: String,
    pub(crate) raw: String,
    pub(crate) raw_bytes: usize,
}

#[cfg(test)]
pub(crate) fn debug_channel_models(channel: &ChannelConfig) -> Vec<String> {
    visible_channel_models(channel)
}

pub(crate) const DEBUG_EXPERIMENT_CLAUDE_SERVER_SIDE_COMPACTION: &str =
    "claude_server_side_compaction";
pub(crate) const DEBUG_CLAUDE_COMPACTION_TRIGGER_INPUT_TOKENS: u64 = 50_000;

pub(crate) fn apply_debug_experimental_features(
    preview: &mut crate::protocol::ExchangePreview,
    experimental_features: &[String],
) -> Result<()> {
    for feature in experimental_features {
        match feature.trim().to_ascii_lowercase().as_str() {
            "" => {}
            DEBUG_EXPERIMENT_CLAUDE_SERVER_SIDE_COMPACTION => {
                if preview.inbound_protocol != "anthropic_messages"
                    || preview.target_protocol != "anthropic_messages"
                    || preview.conversion_level != "native"
                {
                    bail!(
                        "Claude server-side compaction requires a native anthropic_messages request"
                    );
                }
                let body = preview
                    .body
                    .as_object_mut()
                    .ok_or_else(|| anyhow!("debug request body must be a JSON object"))?;
                body.insert(
                    "context_management".to_string(),
                    serde_json::json!({
                        "edits": [{
                            "type": "compact_20260112",
                            "trigger": {
                                "type": "input_tokens",
                                "value": DEBUG_CLAUDE_COMPACTION_TRIGGER_INPUT_TOKENS
                            }
                        }]
                    }),
                );
            }
            unknown => bail!("unknown debug experimental feature: {unknown}"),
        }
    }
    Ok(())
}

pub(crate) fn debug_upstream_model(
    cfg: &ClientConfig,
    target_type: &str,
    target_id: &str,
    requested_model: &str,
) -> Result<String> {
    let requested_model = requested_model.trim();
    let requested_key = normalize_model_name(requested_model);
    if target_type != "local_channel" {
        return Ok(requested_model.to_string());
    }
    let channel = debug_find_channel(cfg, target_id)?;
    let public_key = normalize_model_name(&channel.public_model);
    let upstream_key = normalize_model_name(&channel.upstream_model);
    if let Some(model) = channel
        .models
        .iter()
        .find(|item| normalize_model_name(item) == requested_key)
    {
        return Ok(model.trim().to_string());
    }
    if upstream_key == requested_key {
        return Ok(channel.upstream_model.trim().to_string());
    }
    if public_key == requested_key {
        return Ok(if channel.upstream_model.trim().is_empty() {
            channel.public_model.trim().to_string()
        } else {
            channel.upstream_model.trim().to_string()
        });
    }
    Ok(requested_model.to_string())
}

pub(crate) fn debug_execution_path(
    target_type: &str,
    preview: &crate::protocol::ExchangePreview,
) -> Result<String> {
    if target_type == "local_channel" {
        // A selected local channel executes against its provider-relative
        // surface. Platform targets enter through the public proxy mounts.
        return Ok(preview.path.clone());
    }
    if !matches!(
        target_type,
        "platform_auto" | "platform_node" | "platform_provider"
    ) {
        bail!("unknown debug target type: {target_type}");
    }

    let protocol = crate::protocol::kind::ProtocolKind::parse(&preview.target_protocol)
        .map_err(|error| anyhow!(error))?;
    let surface = match protocol {
        crate::protocol::kind::ProtocolKind::OpenAiResponses
        | crate::protocol::kind::ProtocolKind::OpenAiChat => crate::surface::ApiSurface::OpenAi,
        crate::protocol::kind::ProtocolKind::AnthropicMessages => {
            crate::surface::ApiSurface::Anthropic
        }
        crate::protocol::kind::ProtocolKind::GeminiNative => crate::surface::ApiSurface::Gemini,
    };
    if crate::surface::surface_from_path(&preview.path) == Some(surface) {
        return Ok(preview.path.clone());
    }
    Ok(format!("{}{}", surface.mount(), preview.path))
}

pub(crate) fn debug_find_channel<'a>(
    cfg: &'a ClientConfig,
    target_id: &str,
) -> Result<&'a ChannelConfig> {
    let channels = if cfg.channels.is_empty() {
        return Err(anyhow!("no local channels configured"));
    } else {
        &cfg.channels
    };
    if target_id.trim().is_empty() {
        return channels
            .first()
            .ok_or_else(|| anyhow!("no local channels configured"));
    }
    channels
        .iter()
        .find(|channel| channel.id == target_id)
        .ok_or_else(|| anyhow!("local channel not found: {target_id}"))
}

pub(crate) fn debug_request_headers(
    target_type: &str,
    target_id: &str,
    _api_key: &str,
    skip_local_short_circuit: bool,
) -> serde_json::Value {
    let mut headers = serde_json::Map::new();
    headers.insert(
        "content-type".to_string(),
        serde_json::json!("application/json"),
    );
    if target_type != "local_channel" {
        headers.insert(
            "authorization".to_string(),
            // This value is rendered in the Debug Console and commonly copied into
            // screenshots. Execution builds its real Authorization header separately.
            serde_json::json!("<redacted>"),
        );
        if skip_local_short_circuit {
            headers.insert(
                SKIP_LOCAL_SHORT_CIRCUIT_HEADER.to_string(),
                serde_json::json!("1"),
            );
        } else {
            headers.insert(
                USE_LOCAL_SHORT_CIRCUIT_HEADER.to_string(),
                serde_json::json!("1"),
            );
        }
    }
    if target_type == "platform_node" && !target_id.trim().is_empty() {
        headers.insert(
            "x-const-api-debug-node".to_string(),
            serde_json::json!(target_id),
        );
    }
    if target_type == "platform_provider" && !target_id.trim().is_empty() {
        headers.insert(
            "x-const-api-debug-provider".to_string(),
            serde_json::json!(target_id),
        );
    }
    serde_json::Value::Object(headers)
}

pub(crate) fn local_proxy_auth_header(cfg: &ClientConfig) -> String {
    local_proxy_auth_header_value(&cfg.api_key)
}

pub(crate) fn local_proxy_auth_header_value(api_key: &str) -> String {
    let key = api_key.trim();
    format!("Bearer {key}")
}

pub(crate) async fn execute_debug_local_channel(
    cfg: &ClientConfig,
    client: &Client,
    target_id: &str,
    preview: &crate::protocol::ExchangePreview,
) -> Result<DebugHttpResult> {
    let channel = debug_find_channel(cfg, target_id)?;
    let mut channel = channel.clone();
    channel.upstream_model = preview.upstream_model.clone();
    if channel.public_model.trim().is_empty() {
        channel.public_model = preview.requested_model.clone();
    }
    let response = forward_local_channel_once(
        warp::http::Method::POST,
        &preview.path,
        "",
        warp::http::HeaderMap::new(),
        bytes::Bytes::from(preview.body.to_string()),
        client,
        cfg,
        &channel,
    )
    .await?;
    response_to_debug_http_result(response).await
}

pub(crate) async fn execute_debug_platform(
    cfg: &ClientConfig,
    client: &Client,
    target_type: &str,
    target_id: &str,
    preview: &crate::protocol::ExchangePreview,
    execution_path: &str,
    skip_local_short_circuit: bool,
) -> Result<DebugHttpResult> {
    let url = format!("http://{}{}", cfg.listen, execution_path);
    let mut req = client
        .post(&url)
        .header("Authorization", local_proxy_auth_header(cfg))
        .header("Content-Type", "application/json")
        .json(&preview.body);
    if skip_local_short_circuit {
        req = req.header(SKIP_LOCAL_SHORT_CIRCUIT_HEADER, "1");
    } else {
        req = req.header(USE_LOCAL_SHORT_CIRCUIT_HEADER, "1");
    }
    if target_type == "platform_node" && !target_id.trim().is_empty() {
        req = req.header("x-const-api-debug-node", target_id.trim());
    }
    if target_type == "platform_provider" && !target_id.trim().is_empty() {
        req = req.header("x-const-api-debug-provider", target_id.trim());
    }
    let resp = req.send().await?;
    let status = resp.status().as_u16();
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    let body = resp.bytes().await?;
    let raw_bytes = body.len();
    let raw = String::from_utf8_lossy(&body).to_string();
    Ok(DebugHttpResult {
        status,
        content_type,
        raw,
        raw_bytes,
    })
}

pub(crate) async fn response_to_debug_http_result(
    response: warp::reply::Response,
) -> Result<DebugHttpResult> {
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    let body = response.into_body().collect().await?.to_bytes();
    let raw_bytes = body.len();
    Ok(DebugHttpResult {
        status,
        content_type,
        raw: String::from_utf8_lossy(&body).to_string(),
        raw_bytes,
    })
}
