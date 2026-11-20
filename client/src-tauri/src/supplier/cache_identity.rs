const SUPPLIER_CACHE_IDENTITY_VERSION: &str = "const-cache-identity-v1";

#[derive(serde::Serialize)]
struct SupplierCacheRoot {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    instructions: Vec<SupplierCachePart>,
    user: Vec<SupplierCachePart>,
}

#[derive(serde::Serialize)]
struct SupplierCachePart {
    kind: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    mime: String,
    digest: String,
}

/// Returns the stable conversation identity carried by a request. Header
/// precedence deliberately matches server/internal/app/auth_routing.go.
pub(crate) fn subscription_request_cache_session(
    headers: &reqwest::header::HeaderMap,
    body: &str,
) -> Option<String> {
    subscription_header_cache_identity(headers).or_else(|| subscription_body_cache_identity(body))
}

pub(crate) fn subscription_header_cache_identity(headers: &reqwest::header::HeaderMap) -> Option<String> {
    for name in [
        "x-claude-code-session-id",
        "session-id",
        "session_id",
        "x-session-id",
        "x-session-affinity",
        "x-conversation-id",
        "thread-id",
    ] {
        let value = headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| normalized_subscription_cache_identity(Some(value)));
        if let Some(value) = value {
            return Some(value.to_string());
        }
    }
    None
}

/// Builds the same opaque, user/model-scoped cache key as the platform. This
/// keeps a conversation on one upstream cache domain when routing changes
/// between the caller's local channel and the platform supplier tunnel.
pub(crate) fn scoped_subscription_cache_identity(
    user_id: &str,
    model: &str,
    headers: &reqwest::header::HeaderMap,
    body: &str,
) -> Option<String> {
    let (kind, root) = if let Some(session) = subscription_request_cache_session(headers, body) {
        let digest = Sha256::digest(session.as_bytes());
        ("explicit-affinity", hex::encode(&digest[..12]))
    } else {
        ("derived", derived_subscription_cache_root(body)?)
    };
    let normalized_model = normalize_model_name(model);
    let source = [
        SUPPLIER_CACHE_IDENTITY_VERSION,
        user_id.trim(),
        normalized_model.as_str(),
        kind,
        root.as_str(),
    ]
    .join("\0");
    let digest = Sha256::digest(source.as_bytes());
    Some(format!("c1_{}", hex::encode(&digest[..20])))
}

pub(crate) fn derived_subscription_cache_root(body: &str) -> Option<String> {
    let payload: serde_json::Value = serde_json::from_str(body).ok()?;
    let object = payload.as_object()?;
    let mut instructions = Vec::with_capacity(4);
    for key in [
        "instructions",
        "system",
        "systemInstruction",
        "system_instruction",
    ] {
        if let Some(value) = object.get(key) {
            append_supplier_cache_parts(&mut instructions, value);
        }
    }

    let user = if let Some(messages) = object.get("messages") {
        supplier_cache_message_root(messages, &mut instructions)
    } else if let Some(contents) = object.get("contents") {
        supplier_cache_message_root(contents, &mut instructions)
    } else if let Some(input) = object.get("input") {
        supplier_cache_input_root(input, &mut instructions, "")
    } else {
        Vec::new()
    };
    if user.is_empty() {
        return None;
    }
    let encoded = serde_json::to_vec(&SupplierCacheRoot { instructions, user }).ok()?;
    Some(hex::encode(Sha256::digest(encoded)))
}

fn supplier_cache_message_root(
    value: &serde_json::Value,
    instructions: &mut Vec<SupplierCachePart>,
) -> Vec<SupplierCachePart> {
    let Some(items) = value.as_array() else {
        return Vec::new();
    };
    for item in items {
        let Some(message) = item.as_object() else {
            continue;
        };
        let role = normalized_cache_role(message.get("role"));
        let content = supplier_cache_content_value(message);
        match role.as_str() {
            "system" | "developer" => append_supplier_cache_parts(instructions, content.as_ref()),
            "user" => return supplier_cache_parts(content.as_ref()),
            _ => {}
        }
    }
    Vec::new()
}

fn supplier_cache_input_root(
    value: &serde_json::Value,
    instructions: &mut Vec<SupplierCachePart>,
    inherited_role: &str,
) -> Vec<SupplierCachePart> {
    match value {
        serde_json::Value::String(_) => supplier_cache_parts(value),
        serde_json::Value::Array(items) => {
            for item in items {
                let user = supplier_cache_input_root(item, instructions, inherited_role);
                if !user.is_empty() {
                    return user;
                }
            }
            Vec::new()
        }
        serde_json::Value::Object(input) => {
            let mut role = normalized_cache_role(input.get("role"));
            if role.is_empty() {
                role = inherited_role.to_string();
            }
            if let Some(steps) = input.get("steps") {
                return supplier_cache_input_root(steps, instructions, &role);
            }
            let kind = normalized_cache_role(input.get("type"));
            let content = supplier_cache_content_value(input);
            if matches!(role.as_str(), "system" | "developer")
                || matches!(kind.as_str(), "system_instruction" | "developer_instruction")
            {
                append_supplier_cache_parts(instructions, content.as_ref());
                Vec::new()
            } else if role == "user"
                || kind == "user_input"
                || (role.is_empty() && kind == "message")
            {
                supplier_cache_parts(content.as_ref())
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

fn supplier_cache_content_value<'a>(
    value: &'a serde_json::Map<String, serde_json::Value>,
) -> std::borrow::Cow<'a, serde_json::Value> {
    for key in ["content", "parts", "text"] {
        if let Some(content) = value.get(key) {
            return std::borrow::Cow::Borrowed(content);
        }
    }
    std::borrow::Cow::Owned(serde_json::Value::Object(value.clone()))
}

fn supplier_cache_parts(value: &serde_json::Value) -> Vec<SupplierCachePart> {
    let mut parts = Vec::with_capacity(2);
    append_supplier_cache_parts(&mut parts, value);
    parts
}

fn append_supplier_cache_parts(parts: &mut Vec<SupplierCachePart>, value: &serde_json::Value) {
    match value {
        serde_json::Value::Null => {}
        serde_json::Value::String(value) => append_supplier_cache_part(parts, "text", "", value),
        serde_json::Value::Array(items) => {
            for item in items {
                append_supplier_cache_parts(parts, item);
            }
        }
        serde_json::Value::Object(object) => {
            if let Some(text) = object.get("text").and_then(serde_json::Value::as_str) {
                append_supplier_cache_part(parts, "text", "", text);
                return;
            }
            for key in ["content", "parts"] {
                if let Some(nested) = object.get(key) {
                    append_supplier_cache_parts(parts, nested);
                    return;
                }
            }
            let mime = first_supplier_cache_string(object, &["mimeType", "mime_type", "media_type"]);
            for (keys, kind) in [
                (&["image_url", "input_image"][..], "image"),
                (&["audio_url", "input_audio", "audio"][..], "audio"),
                (&["video_url", "input_video", "video"][..], "video"),
                (&["inlineData", "inline_data"][..], "media"),
                (
                    &["fileData", "file_data", "file_id", "file_url", "input_file", "file"][..],
                    "file",
                ),
            ] {
                if let Some(media) = first_supplier_cache_field(object, keys) {
                    append_supplier_cache_media_part(parts, kind, media, &mime);
                    return;
                }
            }
            if let Some(source) = object.get("source") {
                append_supplier_cache_media_part(
                    parts,
                    object.get("type").and_then(serde_json::Value::as_str).unwrap_or_default(),
                    source,
                    object
                        .get("media_type")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default(),
                );
                return;
            }
            let normalized = normalize_supplier_cache_json(value);
            if let Ok(encoded) = serde_json::to_string(&normalized) {
                append_supplier_cache_part(parts, "json", "", &encoded);
            }
        }
        other => {
            if let Ok(encoded) = serde_json::to_string(other) {
                append_supplier_cache_part(parts, "json", "", &encoded);
            }
        }
    }
}

fn append_supplier_cache_media_part(
    parts: &mut Vec<SupplierCachePart>,
    kind: &str,
    value: &serde_json::Value,
    fallback_mime: &str,
) {
    match value {
        serde_json::Value::String(value) => {
            let (mime, identity) = normalize_supplier_cache_media_value(fallback_mime, value);
            let kind = normalize_supplier_cache_media_kind(kind, &mime);
            append_supplier_cache_part(parts, &kind, &mime, &identity);
        }
        serde_json::Value::Object(object) => {
            let mut mime = first_supplier_cache_string(object, &["mimeType", "mime_type", "media_type"]);
            if mime.is_empty() {
                mime = fallback_mime.to_string();
            }
            if mime.is_empty() {
                mime = supplier_cache_mime_from_format(
                    kind,
                    &first_supplier_cache_string(object, &["format"]),
                );
            }
            let identifier = first_supplier_cache_string(
                object,
                &["url", "uri", "fileUri", "file_uri", "data", "file_id"],
            );
            let (mime, identifier) = normalize_supplier_cache_media_value(&mime, &identifier);
            let kind = normalize_supplier_cache_media_kind(kind, &mime);
            append_supplier_cache_part(parts, &kind, &mime, &identifier);
        }
        other => append_supplier_cache_parts(parts, other),
    }
}

fn normalize_supplier_cache_media_value(mime: &str, value: &str) -> (String, String) {
    let mut mime = mime.trim().to_ascii_lowercase();
    let value = value.trim();
    if value.len() < 6
        || !value
            .as_bytes()
            .get(..5)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"data:"))
    {
        return (mime, value.to_string());
    }
    let Some((header, payload)) = value[5..].split_once(',') else {
        return (mime, value.to_string());
    };
    if let Some(data_mime) = header.split(';').next().map(str::trim).filter(|value| !value.is_empty()) {
        mime = data_mime.to_ascii_lowercase();
    }
    (mime, payload.to_string())
}

fn normalize_supplier_cache_media_kind(kind: &str, mime: &str) -> String {
    let kind = kind.trim().to_ascii_lowercase();
    let mime = mime.trim().to_ascii_lowercase();
    for (prefix, normalized) in [
        ("image/", "image"),
        ("audio/", "audio"),
        ("video/", "video"),
        ("application/", "file"),
        ("text/", "file"),
    ] {
        if mime.starts_with(prefix) {
            return normalized.to_string();
        }
    }
    match kind.as_str() {
        "image" | "image_url" | "input_image" => "image".to_string(),
        "audio" | "audio_url" | "input_audio" => "audio".to_string(),
        "video" | "video_url" | "input_video" => "video".to_string(),
        "document" | "file" | "file_data" | "filedata" | "input_file" => "file".to_string(),
        "" | "inline_data" => "media".to_string(),
        _ => kind,
    }
}

fn supplier_cache_mime_from_format(kind: &str, format: &str) -> String {
    let format = format.trim().trim_start_matches('.').to_ascii_lowercase();
    if format.is_empty() || normalize_supplier_cache_media_kind(kind, "") != "audio" {
        return String::new();
    }
    match format.as_str() {
        "mp3" => "audio/mpeg".to_string(),
        "m4a" => "audio/mp4".to_string(),
        _ => format!("audio/{format}"),
    }
}

fn append_supplier_cache_part(
    parts: &mut Vec<SupplierCachePart>,
    kind: &str,
    mime: &str,
    value: &str,
) {
    if value.is_empty() {
        return;
    }
    parts.push(SupplierCachePart {
        kind: kind.trim().to_string(),
        mime: mime.trim().to_string(),
        digest: hex::encode(Sha256::digest(value.as_bytes())),
    });
}

fn first_supplier_cache_field<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Option<&'a serde_json::Value> {
    keys.iter().find_map(|key| object.get(*key))
}

fn first_supplier_cache_string(
    object: &serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> String {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(serde_json::Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
        .to_string()
}

fn normalize_supplier_cache_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(object) => serde_json::Value::Object(
            object
                .iter()
                .filter(|(key, _)| {
                    !key.eq_ignore_ascii_case("cache_control")
                        && !key.eq_ignore_ascii_case("prompt_cache_breakpoint")
                })
                .map(|(key, value)| (key.clone(), normalize_supplier_cache_json(value)))
                .collect(),
        ),
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items.iter().map(normalize_supplier_cache_json).collect(),
        ),
        other => other.clone(),
    }
}

fn normalized_cache_role(value: Option<&serde_json::Value>) -> String {
    value
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase()
}

#[cfg(test)]
mod cache_identity_tests {
    use super::*;

    #[test]
    fn cache_session_candidates_match_the_platform_contract() {
        // Both Rust and Go consume this fixture, including the final scoped
        // cache key, so extraction changes cannot silently split local/platform.
        let cases: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/cache-session-identity.json"
        ))
        .unwrap();
        for case in cases.as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let body = case["body"].to_string();
            let mut headers = reqwest::header::HeaderMap::new();
            if let Some(values) = case["headers"].as_object() {
                for (key, value) in values {
                    headers.insert(
                        reqwest::header::HeaderName::from_bytes(key.as_bytes()).unwrap(),
                        value.as_str().unwrap().parse().unwrap(),
                    );
                }
            }
            assert_eq!(
                subscription_request_cache_session(&headers, &body).as_deref(),
                case["session"].as_str(),
                "{name}"
            );
            assert_eq!(
                scoped_subscription_cache_identity("user-a", "gpt-5.6-sol", &headers, &body)
                    .as_deref(),
                case["cache_identity"].as_str(),
                "{name}"
            );
        }
    }

    /// Four synthetic requests (A, A, B, A), identical account/model/prefix and
    /// session, but two in-memory channel IDs. No desktop config or login writes.
    #[tokio::test]
    #[ignore = "requires explicit CONST_CACHE_PROBE_DRIVER/MODEL; consumes four dev requests"]
    async fn live_dev_gateway_cache_continuity() -> Result<()> {
        use crate::channel_executor::{ChannelRequest, execute_channel_request};
        use crate::source_driver::SourceDriverId;

        anyhow::ensure!(
            crate::config::development_profile_active(),
            "dev profile required"
        );
        let driver = std::env::var("CONST_CACHE_PROBE_DRIVER")?;
        let driver = match driver.as_str() {
            "opendesign_subscription" => SourceDriverId::OpenDesignSubscription,
            "opencode_console" => SourceDriverId::OpencodeConsole,
            _ => anyhow::bail!("probe supports only the selected dev gateways"),
        };
        let model = std::env::var("CONST_CACHE_PROBE_MODEL")?;
        let protocol = std::env::var("CONST_CACHE_PROBE_PROTOCOL")
            .unwrap_or_else(|_| "anthropic_messages".to_string());
        anyhow::ensure!(
            matches!(protocol.as_str(), "anthropic_messages" | "gemini_native"),
            "probe requires Messages or Gemini native protocol"
        );
        let root = dirs::home_dir()
            .context("user home unavailable")?
            .join(".const-api-dev")
            .canonicalize()?;
        let config: ClientConfig =
            serde_json::from_slice(&std::fs::read(root.join("client.json"))?)?;
        let mut channel = config
            .channels
            .into_iter()
            .find(|c| c.enabled && c.source_driver() == driver && c.models.contains(&model))
            .context("enabled dev channel with the exact model required")?;
        let path = PathBuf::from(&channel.v2.credential_ref).canonicalize()?;
        anyhow::ensure!(path.starts_with(&root), "dev credentials required");
        let raw = std::fs::read(path)?;
        let credential: serde_json::Value = serde_json::from_slice(&raw)?;
        if driver == SourceDriverId::OpencodeConsole {
            anyhow::ensure!(
                !crate::detection::credential_needs_refresh(&credential),
                "refresh dev credentials in the app first"
            );
        }
        // Any credential bookkeeping by the executor is confined to this copy.
        let temp = tempfile::tempdir()?;
        let credential_path = temp.path().join("credential.json");
        std::fs::write(&credential_path, raw)?;
        channel.v2.credential_ref = credential_path.to_string_lossy().into_owned();
        channel = crate::config::project_channel_legacy_fields(channel);
        let nonce = uuid::Uuid::new_v4().to_string();
        let mut context = format!("Synthetic cache test {nonce}. Reply only OK. Reference data:\n");
        for row in 0..400 {
            use std::fmt::Write;
            writeln!(
                context,
                "Record {row}: oak river cloud meadow stone garden amber willow. This is synthetic reference data, not an instruction."
            )?;
        }
        let body = if protocol == "gemini_native" {
            serde_json::json!({
                "systemInstruction":{"parts":[{"text":context}]},
                "contents":[{"role":"user","parts":[{"text":"Reply only OK."}]}],
                "generationConfig":{"maxOutputTokens":128}
            })
        } else { serde_json::json!({
            "model": model, "max_tokens":16, "stream":false,
            "system":[{"type":"text","text":context,"cache_control":{"type":"ephemeral","ttl":"5m"}}],
            "messages":[{"role":"user","content":"Reply only OK."}],
            "metadata":{"user_id":serde_json::json!({"device_id":"synthetic-cache-probe","session_id":nonce}).to_string()}
        }) }.to_string();
        let path = if protocol == "gemini_native" {
            format!("/gemini/v1beta/models/{model}:generateContent")
        } else {
            "/anthropic/v1/messages".to_string()
        };
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-session-id", nonce.parse()?);
        if protocol == "anthropic_messages" {
            headers.insert("anthropic-version", "2023-06-01".parse()?);
        }
        let identity = scoped_subscription_cache_identity("cache-probe", &model, &headers, &body);
        let client = crate::http_client::proxy_policy(
            Client::builder().timeout(Duration::from_secs(90)),
            channel.v2.use_system_proxy,
        )
        .build()?;
        for (step, route) in ["A", "A", "B", "A"].into_iter().enumerate() {
            channel.id = format!("cache-probe-{nonce}-{route}");
            let result = execute_channel_request(
                &client,
                &channel,
                ChannelRequest {
                    method: reqwest::Method::POST,
                    path: path.clone(),
                    raw_query: String::new(),
                    has_query: false,
                    headers: headers.clone(),
                    body: body.clone().into(),
                    stream_requested: Some(false),
                    selected_upstream_model: Some(model.clone()),
                    selected_target_protocol: Some(protocol.clone()),
                    cache_identity: identity.clone(),
                    safety_identifier: None,
                },
            )
            .await
            .map_err(|error| anyhow!(
                "cache probe executor failed at step {}: {}",
                step + 1,
                crate::channel_executor::channel_execution_error_code(&error).unwrap_or("executor_error")
            ))?;
            anyhow::ensure!(
                result.native_passthrough,
                "probe requires same-protocol forwarding"
            );
            let status = result.response.status();
            anyhow::ensure!(
                status.is_success(),
                "cache probe HTTP {status} at step {}",
                step + 1
            );
            let value: serde_json::Value = result.response.json().await?;
            let (input, write, read, output) = if protocol == "gemini_native" {
                let usage = value.get("usageMetadata").context("gateway omitted Gemini usage")?;
                (
                    usage["promptTokenCount"].as_u64(),
                    None,
                    usage["cachedContentTokenCount"].as_u64(),
                    usage["candidatesTokenCount"].as_u64(),
                )
            } else {
                anyhow::ensure!(value["type"] == "message", "gateway did not return a Messages response");
                let usage = value.get("usage").context("gateway omitted usage")?;
                (
                    usage["input_tokens"].as_u64(),
                    usage["cache_creation_input_tokens"].as_u64(),
                    usage["cache_read_input_tokens"].as_u64(),
                    usage["output_tokens"].as_u64(),
                )
            };
            // Do not infer a hit from timing or missing counters. Keep raw
            // numeric usage only; no prompt, account or credential output.
            println!(
                "cache_probe step={} route={} model={} input={:?} write={:?} read={:?} output={:?}",
                step + 1,
                route,
                model,
                input, write, read, output
            );
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        Ok(())
    }

    fn equivalent_text_tool_requests() -> [(&'static str, &'static str); 4] {
        [
            (
                "/v1/responses",
                r#"{"model":"gpt-5.4","instructions":"stable system","input":[{"role":"user","content":[{"type":"input_text","text":"first question"}]}],"tools":[{"type":"function","name":"lookup","description":"look up","parameters":{"type":"object","properties":{"q":{"type":"string"}}}}],"stream":true}"#,
            ),
            (
                "/v1/chat/completions",
                r#"{"model":"gpt-5.4","messages":[{"role":"system","content":"stable system"},{"role":"user","content":"first question"}],"tools":[{"type":"function","function":{"name":"lookup","description":"look up","parameters":{"type":"object","properties":{"q":{"type":"string"}}}}}],"stream":true}"#,
            ),
            (
                "/anthropic/v1/messages",
                r#"{"model":"gpt-5.4","system":"stable system","messages":[{"role":"user","content":[{"type":"text","text":"first question"}]}],"tools":[{"name":"lookup","description":"look up","input_schema":{"type":"object","properties":{"q":{"type":"string"}}}}],"stream":true,"max_tokens":128}"#,
            ),
            (
                "/gemini/v1beta/models/gpt-5.4:streamGenerateContent",
                r#"{"systemInstruction":{"parts":[{"text":"stable system"}]},"contents":[{"role":"user","parts":[{"text":"first question"}]}],"tools":[{"functionDeclarations":[{"name":"lookup","description":"look up","parameters":{"type":"object","properties":{"q":{"type":"string"}}}}]}]}"#,
            ),
        ]
    }

    #[test]
    fn cache_identity_matches_the_server_contract_for_explicit_sessions() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "x-conversation-id",
            reqwest::header::HeaderValue::from_static("cross-route-session"),
        );
        let identity = scoped_subscription_cache_identity(
            "user-a",
            "GPT-5.6-SOL",
            &headers,
            r#"{"input":"ignored when a session is explicit"}"#,
        );
        assert_eq!(
            identity.as_deref(),
            Some("c1_f4468667066147d5bfdcfbe96a8451a68674be1a")
        );
    }

    #[test]
    fn derived_cache_identity_is_stable_across_supported_protocols_and_growth() {
        let headers = reqwest::header::HeaderMap::new();
        let bodies = [
            r#"{"instructions":"stable system","input":[{"role":"user","content":[{"type":"input_text","text":"first question"}]}]}"#,
            r#"{"messages":[{"role":"system","content":"stable system"},{"role":"user","content":"first question"},{"role":"assistant","content":"answer"},{"role":"user","content":"follow up"}]}"#,
            r#"{"system":"stable system","messages":[{"role":"user","content":[{"type":"text","text":"first question"}]}]}"#,
            r#"{"systemInstruction":{"parts":[{"text":"stable system"}]},"contents":[{"role":"user","parts":[{"text":"first question"}]},{"role":"model","parts":[{"text":"answer"}]}]}"#,
        ];
        let identities = bodies
            .iter()
            .map(|body| {
                scoped_subscription_cache_identity("user-a", "shared-model", &headers, body)
                    .expect("derived cache identity")
            })
            .collect::<Vec<_>>();
        assert!(identities.iter().all(|identity| identity == &identities[0]));
        assert_eq!(
            identities[0],
            "c1_a17e387f5f765f6b880c54f7793a78b025d4a0e8"
        );
    }

    #[test]
    fn multimodal_cache_roots_match_across_openai_anthropic_and_gemini() {
        let headers = reqwest::header::HeaderMap::new();
        let assert_same = |bodies: &[&str]| {
            let identities = bodies
                .iter()
                .map(|body| {
                    scoped_subscription_cache_identity("user-a", "shared-model", &headers, body)
                        .expect("multimodal cache identity")
                })
                .collect::<Vec<_>>();
            assert!(identities.iter().all(|identity| identity == &identities[0]));
        };
        assert_same(&[
            r#"{"input":[{"role":"user","content":[{"type":"input_text","text":"inspect"},{"type":"input_image","image_url":"data:image/png;base64,AA=="}]}]}"#,
            r#"{"messages":[{"role":"user","content":[{"type":"text","text":"inspect"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"AA=="}}]}]}"#,
            r#"{"contents":[{"role":"user","parts":[{"text":"inspect"},{"inlineData":{"mimeType":"image/png","data":"AA=="}}]}]}"#,
        ]);
        assert_same(&[
            r#"{"input":[{"role":"user","content":[{"type":"input_text","text":"listen"},{"type":"input_audio","audio_url":"data:audio/wav;base64,AA=="}]}]}"#,
            r#"{"messages":[{"role":"user","content":[{"type":"text","text":"listen"},{"type":"input_audio","input_audio":{"format":"wav","data":"AA=="}}]}]}"#,
            r#"{"contents":[{"role":"user","parts":[{"text":"listen"},{"inlineData":{"mimeType":"audio/wav","data":"AA=="}}]}]}"#,
        ]);
        assert_same(&[
            r#"{"input":[{"role":"user","content":[{"type":"input_text","text":"read"},{"type":"input_file","file_data":"AA==","media_type":"application/pdf"}]}]}"#,
            r#"{"messages":[{"role":"user","content":[{"type":"text","text":"read"},{"type":"document","source":{"type":"base64","media_type":"application/pdf","data":"AA=="}}]}]}"#,
            r#"{"contents":[{"role":"user","parts":[{"text":"read"},{"inlineData":{"mimeType":"application/pdf","data":"AA=="}}]}]}"#,
        ]);
        assert_same(&[
            r#"{"input":[{"role":"user","content":[{"type":"input_text","text":"watch"},{"type":"input_video","video_url":"data:video/webm;base64,AA=="}]}]}"#,
            r#"{"messages":[{"role":"user","content":[{"type":"text","text":"watch"},{"type":"video","source":{"type":"base64","media_type":"video/webm","data":"AA=="}}]}]}"#,
            r#"{"contents":[{"role":"user","parts":[{"text":"watch"},{"inlineData":{"mimeType":"video/webm","data":"AA=="}}]}]}"#,
        ]);
    }

    #[test]
    fn equivalent_protocols_build_one_openai_cacheable_prefix() {
        let mut expected = None;
        for (path, body) in equivalent_text_tool_requests() {
            let converted = crate::proxy::upstream_request_for_api_format_with_profiles_report(
                "openai_responses",
                path,
                body,
                "gpt-5.4",
                &[],
            )
            .expect("convert cache-equivalent request");
            let prepared = prepare_codex_subscription_endpoint_request(
                &crate::default_supplier_config(),
                &converted.body,
                false,
                "gpt-5.4",
                Some("c1_shared"),
                None,
            )
            .expect("prepare OpenAI subscription request");
            let value: serde_json::Value =
                serde_json::from_str(&prepared.body).expect("prepared JSON");
            let prefix = serde_json::json!({
                "instructions": value.get("instructions"),
                "input": value.get("input"),
                "tools": value.get("tools"),
                "prompt_cache_key": value.get("prompt_cache_key"),
            });
            if let Some(expected) = expected.as_ref() {
                assert_eq!(&prefix, expected, "cacheable prefix split for {path}");
            } else {
                expected = Some(prefix);
            }
        }
    }

    #[test]
    fn equivalent_protocols_build_one_claude_cacheable_prefix() {
        let mut expected = None;
        for (path, body) in equivalent_text_tool_requests() {
            let converted = crate::proxy::upstream_request_for_api_format_with_profiles_report(
                "anthropic_messages",
                path,
                body,
                "claude-sonnet-4-6",
                &[],
            )
            .expect("convert cache-equivalent Claude request");
            let dialect = ensure_claude_messages_body_with_faults(
                &converted.body,
                "claude-sonnet-4-6",
            )
            .expect("prepare Claude Messages request");
            let prepared = prepare_claude_subscription_oauth_body_with_cache_identity(
                &dialect.body,
                "credential-a",
                Some("c1_shared"),
            )
            .expect("prepare Claude OAuth request");
            let value: serde_json::Value = serde_json::from_str(&prepared).unwrap();
            let prefix = serde_json::json!({
                "system": value.get("system"),
                "messages": value.get("messages"),
                "tools": value.get("tools"),
                "metadata": value.get("metadata"),
            });
            if let Some(expected) = expected.as_ref() {
                assert_eq!(&prefix, expected, "Claude cacheable prefix split for {path}");
            } else {
                expected = Some(prefix);
            }
        }
    }

    #[test]
    fn equivalent_protocols_build_one_antigravity_cacheable_prefix() {
        let mut expected = None;
        for (path, body) in equivalent_text_tool_requests() {
            let converted = crate::proxy::upstream_request_for_api_format_with_profiles_report(
                "gemini_native",
                path,
                body,
                "gemini-3.6-flash",
                &[],
            )
            .expect("convert cache-equivalent Gemini request");
            let prepared = gemini_native_body_to_antigravity_body_with_cache_identity(
                &converted.body,
                "gemini-3.6-flash",
                "project-a",
                Some("c1_shared"),
            )
            .expect("prepare Antigravity request");
            let value: serde_json::Value = serde_json::from_str(&prepared).unwrap();
            let prefix = serde_json::json!({
                "systemInstruction": value.pointer("/request/systemInstruction"),
                "contents": value.pointer("/request/contents"),
                "tools": value.pointer("/request/tools"),
                "sessionId": value.pointer("/request/sessionId"),
            });
            if let Some(expected) = expected.as_ref() {
                assert_eq!(
                    &prefix, expected,
                    "Antigravity cacheable prefix split for {path}"
                );
            } else {
                expected = Some(prefix);
            }
        }
    }
}
