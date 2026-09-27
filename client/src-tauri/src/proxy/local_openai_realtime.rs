const OPENAI_SUBSCRIPTION_REALTIME_MODEL: &str = "gpt-live-1-codex";
const OPENAI_REALTIME_LIVE_BASE_URL: &str = "https://api.openai.com/v1/live";
const OPENAI_REALTIME_SDP_MAX_BYTES: usize = 1024 * 1024;
const OPENAI_REALTIME_SESSION_MAX_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug)]
struct OpenAiSubscriptionRealtimeCallPayload {
    body: bytes::Bytes,
    content_type: &'static str,
    upstream_model: String,
}

fn openai_realtime_call_model_hint(body: &[u8]) -> String {
    fn model(value: &serde_json::Value) -> Option<String> {
        value
            .pointer("/session/model")
            .or_else(|| value.get("model"))
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    }

    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) {
        if let Some(model) = model(&value) {
            return model;
        }
    }

    // The route selector does not receive Content-Type. For multipart calls,
    // inspect only complete JSON values and treat the result as a routing hint;
    // the strict multipart parser below remains the execution boundary.
    let mut inspected = 0usize;
    for (offset, byte) in body.iter().enumerate() {
        if inspected >= 16 {
            break;
        }
        if *byte != b'{' {
            continue;
        }
        inspected += 1;
        let mut values = serde_json::Deserializer::from_slice(&body[offset..])
            .into_iter::<serde_json::Value>();
        if let Some(Ok(value)) = values.next() {
            if let Some(model) = model(&value) {
                return model;
            }
        }
    }
    OPENAI_SUBSCRIPTION_REALTIME_MODEL.to_string()
}

fn codex_realtime_calls_url() -> Result<String> {
    let mut url = reqwest::Url::parse(&codex_responses_url())
        .context("invalid Codex Responses URL while deriving Realtime calls URL")?;
    let current_path = url.path().trim_end_matches('/').to_string();
    let base_path = current_path
        .strip_suffix("/responses")
        .unwrap_or(&current_path);
    url.set_path(&format!("{base_path}/realtime/calls"));
    url.set_query(None);

    url.query_pairs_mut()
        .append_pair("intent", "quicksilver")
        .append_pair("architecture", "avas");
    Ok(url.to_string())
}

fn normalize_openai_subscription_realtime_session(
    session: serde_json::Value,
) -> Result<(serde_json::Value, String)> {
    let object = session
        .as_object()
        .ok_or_else(|| anyhow!("OpenAI Realtime session must be a JSON object"))?;
    if object
        .get("type")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| value != "realtime")
    {
        return Err(anyhow!(
            "OpenAI subscription Realtime currently supports conversational sessions only"
        ));
    }
    for unsupported in ["tools", "tool_choice", "tracing"] {
        if object.get(unsupported).is_some_and(|value| !value.is_null()) {
            return Err(anyhow!(
                "OpenAI subscription Realtime cannot preserve session.{unsupported} yet"
            ));
        }
    }
    for unsupported in [
        "/audio/input/turn_detection",
        "/audio/input/transcription",
        "/audio/input/noise_reduction",
        "/audio/output/speed",
    ] {
        if session
            .pointer(unsupported)
            .is_some_and(|value| !value.is_null())
        {
            return Err(anyhow!(
                "OpenAI subscription Realtime cannot preserve session{unsupported} yet"
            ));
        }
    }
    // Current Codex AVAS uses the frameless session dialect. Build its small
    // allowlist instead of forwarding public Realtime fields that happen to
    // share names but have different semantics.
    let mut normalized = serde_json::Map::new();
    if let Some(instructions) = object
        .get("instructions")
        .and_then(serde_json::Value::as_str)
    {
        normalized.insert(
            "instructions".to_string(),
            serde_json::Value::String(instructions.to_string()),
        );
    }
    if let Some(voice) = object
        .get("audio")
        .and_then(|audio| audio.pointer("/output/voice"))
        .and_then(serde_json::Value::as_str)
    {
        normalized.insert(
            "audio".to_string(),
            serde_json::json!({"output": {"voice": voice}}),
        );
    }
    if let Some(initial_items) = object
        .get("initial_items")
        .and_then(serde_json::Value::as_array)
    {
        normalized.insert(
            "initial_items".to_string(),
            serde_json::Value::Array(initial_items.clone()),
        );
    }
    normalized.insert(
        "delegation".to_string(),
        serde_json::json!({"type": "client"}),
    );
    let upstream_model = OPENAI_SUBSCRIPTION_REALTIME_MODEL.to_string();
    normalized.insert(
        "model".to_string(),
        serde_json::Value::String(upstream_model.clone()),
    );
    log::debug!(
        "[const-api][realtime] subscription call session normalized model_mapped={} field_count={}",
        normalized.get("model").and_then(serde_json::Value::as_str)
            == Some(OPENAI_SUBSCRIPTION_REALTIME_MODEL),
        normalized.len(),
    );
    Ok((serde_json::Value::Object(normalized), upstream_model))
}

fn normalize_openai_subscription_frameless_session(
    session: serde_json::Value,
) -> Result<(serde_json::Value, String)> {
    let mut object = session
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow!("Codex Live session must be a JSON object"))?;
    object.remove("id");
    object.remove("type");
    if let Some(delegation) = object.get("delegation") {
        let delegation = delegation
            .as_object()
            .ok_or_else(|| anyhow!("Codex Live session.delegation must be an object"))?;
        if delegation
            .get("type")
            .and_then(serde_json::Value::as_str)
            != Some("client")
        {
            return Err(anyhow!(
                "Codex Live session.delegation.type must be client"
            ));
        }
    } else {
        object.insert(
            "delegation".to_string(),
            serde_json::json!({"type": "client"}),
        );
    }
    // Native Codex owns the model and session dialect. Only supply the legacy
    // default when omitted; silently rewriting a future model breaks upgrades.
    let upstream_model = object
        .get("model")
        .and_then(serde_json::Value::as_str)
        .filter(|model| !model.trim().is_empty())
        .unwrap_or(OPENAI_SUBSCRIPTION_REALTIME_MODEL)
        .to_string();
    if object
        .get("model")
        .and_then(serde_json::Value::as_str)
        .is_none_or(|model| model.trim().is_empty())
    {
        object.insert("model".to_string(), upstream_model.clone().into());
    }
    Ok((serde_json::Value::Object(object), upstream_model))
}

async fn parse_openai_realtime_call_payload(
    headers: &warp::http::HeaderMap,
    body: bytes::Bytes,
) -> Result<serde_json::Value> {
    if body.len() > OPENAI_REALTIME_SDP_MAX_BYTES + OPENAI_REALTIME_SESSION_MAX_BYTES {
        return Err(anyhow!("OpenAI Realtime call is too large"));
    }
    let content_type = headers.get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok()).unwrap_or("application/sdp");
    let lower = content_type.to_ascii_lowercase();
    let value = if lower.starts_with("application/json") {
        serde_json::from_slice::<serde_json::Value>(&body).context("parse OpenAI Realtime call JSON")?
    } else if lower.starts_with("application/sdp") {
        serde_json::json!({"sdp": std::str::from_utf8(&body)?, "session": {}})
    } else if lower.starts_with("multipart/form-data") {
        let boundary = multer::parse_boundary(content_type)?;
        let stream = futures_util::stream::once(async move { Ok::<_, std::io::Error>(body) });
        let mut multipart = multer::Multipart::new(stream, boundary);
        let mut fields = serde_json::Map::new();
        while let Some(field) = multipart.next_field().await? {
            let name = field.name().unwrap_or_default().to_string();
            if !matches!(name.as_str(), "sdp" | "session") || fields.contains_key(&name) {
                return Err(anyhow!("OpenAI Realtime multipart contains invalid field {name}"));
            }
            let bytes = field.bytes().await?;
            let value = if name == "sdp" { serde_json::Value::String(std::str::from_utf8(&bytes)?.to_string()) }
                else { serde_json::from_slice(&bytes)? };
            fields.insert(name, value);
        }
        serde_json::Value::Object(fields)
    } else {
        return Err(anyhow!("OpenAI Realtime calls require application/sdp, application/json, or multipart/form-data"));
    };
    let sdp = value.get("sdp").and_then(serde_json::Value::as_str)
        .filter(|sdp| !sdp.trim().is_empty()).ok_or_else(|| anyhow!("OpenAI Realtime call must include sdp"))?;
    if sdp.len() > OPENAI_REALTIME_SDP_MAX_BYTES || !value.get("session").is_some_and(serde_json::Value::is_object) {
        return Err(anyhow!("OpenAI Realtime call contains invalid sdp or session"));
    }
    Ok(value)
}

async fn normalize_openai_subscription_realtime_call_payload(
    headers: &warp::http::HeaderMap,
    body: bytes::Bytes,
    native_frameless: bool,
) -> Result<OpenAiSubscriptionRealtimeCallPayload> {
    let mut value = parse_openai_realtime_call_payload(headers, body).await?;
    let session = value["session"].take();
    let (session, upstream_model) = if native_frameless {
        normalize_openai_subscription_frameless_session(session)?
    } else {
        normalize_openai_subscription_realtime_session(session)?
    };
    value["session"] = session;
    Ok(OpenAiSubscriptionRealtimeCallPayload {
        body: bytes::Bytes::from(serde_json::to_vec(&value)?),
        content_type: "application/json",
        upstream_model,
    })
}
fn codex_realtime_request_context_headers(
    inbound: &warp::http::HeaderMap,
) -> reqwest::header::HeaderMap {
    let mut headers = codex_model_request_context_headers(Some(inbound), &active_codex_identity());
    // Voice versions negotiate their own dialect. Preserve the native tool's
    // negotiation headers without changing the identity used for text requests.
    if crate::codex_request_is_native(Some(inbound)) {
        for name in ["openai-alpha", "openai-beta"] {
            if let Some(value) = inbound.get(name).filter(|value| {
                value.to_str().is_ok_and(|text| !text.trim().is_empty() && text.len() <= 4096)
            }) {
                headers.insert(reqwest::header::HeaderName::from_static(name), value.clone());
            }
        }
    }
    headers.entry("openai-alpha")
        .or_insert(reqwest::header::HeaderValue::from_static("quicksilver=v2"));
    headers
}

fn openai_subscription_realtime_call_request(
    client: &Client,
    url: &str,
    token: &str,
    credential: &serde_json::Value,
    inbound_headers: &warp::http::HeaderMap,
    payload: OpenAiSubscriptionRealtimeCallPayload,
) -> reqwest::RequestBuilder {
    let request_headers = codex_realtime_request_context_headers(inbound_headers);
    let mut request = client
        .post(url)
        .header(reqwest::header::ACCEPT, "application/sdp")
        .header(reqwest::header::CONTENT_TYPE, payload.content_type)
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
        .headers(request_headers)
        .body(payload.body);
    if let Some(account_id) = json_string(credential, &["account_id"])
        .or_else(|| json_string(credential, &["accountID"]))
        .or_else(|| json_string(credential, &["chatgpt_account_id"]))
        .filter(|value| !value.trim().is_empty())
    {
        request = request.header("Chatgpt-Account-Id", account_id);
    }
    request
}

async fn send_openai_subscription_realtime_call(
    client: &Client,
    channel: &ChannelConfig,
    url: &str,
    inbound_headers: &warp::http::HeaderMap,
    payload: OpenAiSubscriptionRealtimeCallPayload,
) -> Result<reqwest::Response> {
    let (token, credential, _) = ensure_openai_subscription_access_token(client, channel).await?;
    let response = crate::upstream_transport::send(openai_subscription_realtime_call_request(
        client,
        url,
        &token,
        &credential,
        inbound_headers,
        payload.clone(),
    ))
    .await?;
    if response.status() != reqwest::StatusCode::UNAUTHORIZED {
        return Ok(response);
    }

    let _ = response.bytes().await;
    let (token, credential, _) = ensure_openai_subscription_access_token_with_options(
        client,
        channel,
        Some(&token),
        OPENAI_OAUTH_TOKEN_URL,
    )
    .await?;
    crate::upstream_transport::send(openai_subscription_realtime_call_request(
        client,
        url,
        &token,
        &credential,
        inbound_headers,
        payload,
    ))
    .await
    .map_err(Into::into)
}

async fn forward_openai_subscription_realtime_call(
    client: &Client,
    channel: &ChannelConfig,
    inbound_headers: &warp::http::HeaderMap,
    body: bytes::Bytes,
    native_frameless: bool,
) -> Result<warp::reply::Response> {
    let payload = normalize_openai_subscription_realtime_call_payload(
        inbound_headers,
        body,
        native_frameless,
    )
        .await
        .map_err(crate::channel_executor::surface_operation_not_supported_error)?;
    let upstream_model = payload.upstream_model.clone();
    let url = codex_realtime_calls_url()?;
    let response = send_openai_subscription_realtime_call(
        client,
        channel,
        &url,
        inbound_headers,
        payload,
    )
    .await?;
    response_from_reqwest_with_upstream_model(response, false, Some(&upstream_model)).await
}

#[cfg(test)]
mod openai_subscription_realtime_tests {
    use super::*;

    #[tokio::test]
    async fn public_multipart_call_is_normalized_for_codex_subscription() {
        let boundary = "const-realtime-boundary";
        let body = bytes::Bytes::from(format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"sdp\"\r\nContent-Type: application/sdp\r\n\r\nv=0\r\n\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"session\"\r\nContent-Type: application/json\r\n\r\n{{\"type\":\"realtime\",\"model\":\"gpt-realtime-2.1\",\"audio\":{{\"output\":{{\"voice\":\"marin\"}}}}}}\r\n--{boundary}--\r\n"
        ));
        let mut headers = warp::http::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}")
                .parse()
                .expect("content type"),
        );

        let payload = normalize_openai_subscription_realtime_call_payload(&headers, body, false)
            .await
            .expect("normalize call");
        let value: serde_json::Value =
            serde_json::from_slice(&payload.body).expect("backend call JSON");

        assert_eq!(payload.content_type, "application/json");
        assert_eq!(payload.upstream_model, OPENAI_SUBSCRIPTION_REALTIME_MODEL);
        assert_eq!(value["sdp"], "v=0\r\n");
        assert!(value["session"].get("type").is_none());
        assert_eq!(
            value["session"]["model"],
            OPENAI_SUBSCRIPTION_REALTIME_MODEL
        );
        assert_eq!(value["session"]["delegation"]["type"], "client");
    }

    #[tokio::test]
    async fn public_sdp_call_gets_the_required_codex_bootstrap_session() {
        let mut headers = warp::http::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            "application/sdp".parse().expect("content type"),
        );

        let payload = normalize_openai_subscription_realtime_call_payload(
            &headers,
            bytes::Bytes::from_static(b"v=0\r\n"),
            false,
        )
        .await
        .expect("normalize SDP call");
        let value: serde_json::Value =
            serde_json::from_slice(&payload.body).expect("backend call JSON");

        assert_eq!(payload.content_type, "application/json");
        assert_eq!(value["sdp"], "v=0\r\n");
        assert_eq!(
            value["session"]["model"],
            OPENAI_SUBSCRIPTION_REALTIME_MODEL
        );
        assert_eq!(value["session"]["delegation"]["type"], "client");
    }

    #[test]
    fn unsupported_public_session_semantics_fail_before_upstream() {
        let error = normalize_openai_subscription_realtime_session(serde_json::json!({
            "type": "realtime",
            "tools": [{"type": "function", "name": "weather"}]
        }))
        .expect_err("standard tools are not equivalent to Codex delegation");
        assert!(error.to_string().contains("session.tools"));
    }

    #[test]
    fn native_codex_live_session_preserves_frameless_fields() {
        let (session, model) = normalize_openai_subscription_frameless_session(
            serde_json::json!({
                "id": "local-session",
                "model": "gpt-live-future-codex",
                "instructions": "speak briefly",
                "delegation": {"type": "client", "ack_filler": true},
                "initial_items": [{"type":"message","role":"developer","content":[]}],
                "future_frameless_field": {"enabled": true}
            }),
        )
        .expect("normalize native Codex Live session");

        assert_eq!(model, "gpt-live-future-codex");
        assert!(session.get("id").is_none());
        assert_eq!(session["model"], "gpt-live-future-codex");
        assert_eq!(session["delegation"]["ack_filler"], true);
        assert_eq!(session["future_frameless_field"]["enabled"], true);
    }

    #[test]
    fn call_url_uses_the_exact_codex_session_dialect() {
        let url = codex_realtime_calls_url().expect("derive realtime URL");
        let url = reqwest::Url::parse(&url).expect("parse realtime URL");
        let pairs = url.query_pairs().collect::<std::collections::HashMap<_, _>>();
        assert!(url.path().ends_with("/codex/realtime/calls"));
        assert_eq!(pairs.len(), 2);
        assert_eq!(
            pairs.get("intent").map(|value| value.as_ref()),
            Some("quicksilver")
        );
        assert_eq!(
            pairs.get("architecture").map(|value| value.as_ref()),
            Some("avas")
        );
    }
}
