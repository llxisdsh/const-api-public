// Manual image checks use the same supplier executor as platform requests.
// Background health checks must not turn into costly media generation jobs.
struct MediaProbeKind {
    image: bool,
    non_text: bool,
}

fn automatic_probe_model(channel: &ChannelConfig) -> String {
    let candidates = channel
        .models
        .iter()
        .filter(|model| !media_probe_kind(channel, model).is_some_and(|kind| kind.non_text))
        .cloned()
        .collect::<Vec<_>>();
    // Keep the missing-model error for empty/invalid configurations.
    pick_primary_model(
        if candidates.is_empty() {
            &channel.models
        } else {
            &candidates
        },
        channel,
    )
}

fn channel_has_only_non_text_models(channel: &ChannelConfig) -> bool {
    !channel.models.is_empty()
        && channel
            .models
            .iter()
            .all(|model| media_probe_kind(channel, model).is_some_and(|kind| kind.non_text))
}

fn media_probe_kind(channel: &ChannelConfig, model: &str) -> Option<MediaProbeKind> {
    let profiles = channel
        .capability_profiles
        .iter()
        .filter(|profile| profile.model_pattern.eq_ignore_ascii_case(model))
        .collect::<Vec<_>>();
    let non_text = profiles
        .iter()
        .any(|profile| profile.catalog_metadata && profile.catalog_text_output == Some(false));
    // A chat model's hosted image tool is not an Images API model. Gemini's
    // native generation operation, unlike Images, can output both text/images.
    let image = profiles.iter().any(|profile| profile.image_output)
        && (non_text || image_probe_uses_gemini(channel, model));
    if image || non_text {
        return Some(MediaProbeKind { image, non_text });
    }
    // Older model directories may have no modality metadata. These are explicit
    // image families, not a substring match that could misclassify vision chat.
    let short = crate::config::short_model_name(model).to_ascii_lowercase();
    if ["gpt-image-", "dall-e-", "grok-imagine-image"]
        .iter()
        .any(|prefix| short.starts_with(prefix))
    {
        return Some(MediaProbeKind {
            image: true,
            non_text: true,
        });
    }
    None
}

fn image_probe_uses_gemini(channel: &ChannelConfig, model: &str) -> bool {
    channel.source_driver() == crate::source_driver::SourceDriverId::GeminiSubscription
        || crate::coding_gateway::channel_model_protocol(channel, model)
            == Some(crate::protocol::kind::ProtocolKind::GeminiNative)
        || channel.v2.default_target.protocol == crate::protocol::kind::ProtocolKind::GeminiNative
}

pub(crate) async fn test_channel_upstream_model_manual(
    channel: ChannelConfig,
    model: String,
    prompt: String,
) -> Result<ChannelUpstreamTestResult, String> {
    let channel = normalize_probe_channel(channel);
    let model = exact_probe_model(&channel, &model)?;
    let Some(kind) = media_probe_kind(&channel, &model) else {
        return Box::pin(test_channel_upstream_model_inner(channel, model, prompt)).await;
    };
    if !kind.image {
        return Err("this media model requires its native audio/video/embedding operation; the chat test was not sent".into());
    }
    let result = Box::pin(test_image_channel(&channel, &model, &prompt))
        .await
        .map_err(|error| error.to_string())?;
    availability::record_probe_observation(&channel, &result);
    health_recovery::record_model_success(&result.model);
    Ok(result)
}

fn image_probe_request(
    channel: &ChannelConfig,
    model: &str,
    prompt: &str,
) -> Result<crate::surface_wire::SurfaceEnvelope> {
    use crate::surface::{ApiOperation, ApiSurface};
    let prompt = if prompt.trim().is_empty() {
        "Create one small blue circle on a white background."
    } else {
        prompt.trim()
    };
    let gemini = image_probe_uses_gemini(channel, model);
    let (surface, operation, path, body) = if gemini {
        (
            ApiSurface::Gemini,
            ApiOperation::GenerateContent,
            format!("/v1beta/models/{model}:generateContent"),
            serde_json::json!({"contents":[{"role":"user","parts":[{"text":prompt}]}],
                "generationConfig":{"responseModalities":["IMAGE"]}}),
        )
    } else {
        (
            ApiSurface::OpenAi,
            ApiOperation::ImagesGenerations,
            "/v1/images/generations".into(),
            serde_json::json!({"model":model,"prompt":prompt,"n":1}),
        )
    };
    let mut wire = crate::surface_wire::SurfaceEnvelope {
        surface: Some(surface),
        operation: Some(operation),
        method: "POST".into(),
        path,
        body: crate::surface_wire::WireBody::from_bytes(body.to_string().as_bytes()),
        stream: Some(false),
        ..Default::default()
    };
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    wire.headers = crate::surface_wire::header_pairs(&headers);
    Ok(wire)
}

#[cfg(test)]
mod media_probe_tests {
    use super::*;

    #[tokio::test]
    async fn manual_media_probe_uses_images_while_automatic_probe_sends_nothing() {
        use warp::Filter as _;
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = calls.clone();
        let route = warp::path!("v1" / "images" / "generations")
            .and(warp::body::json())
            .map(move |body: serde_json::Value| {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                assert_eq!(body["model"], "gpt-image-2");
                assert_eq!(body["n"], 1);
                assert_eq!(body["prompt"], "blue dot");
                warp::reply::json(
                    &serde_json::json!({"data":[{"b64_json":"private-image-payload"}],
                    "usage":{"input_tokens":5,"output_tokens":10}}),
                )
            });
        let (address, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
        let server = tokio::spawn(server);
        let mut channel = crate::coding_gateway::tests::channel(
            crate::source_driver::SourceDriverId::OpenAiApi,
            &format!("http://{address}/v1"),
        );
        channel.id = "manual-image-probe-test".into();
        channel.v2.use_system_proxy = false;
        channel.models = vec!["gpt-image-2".into()];
        channel.upstream_model = "gpt-image-2".into();
        let error = test_channel_upstream_attempt(
            channel.clone(),
            Some("gpt-image-2".into()),
            "blue dot".into(),
        )
        .await
        .unwrap_err();
        assert!(error.deferred);
        assert!(error.failure.is_none());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        let result =
            test_channel_upstream_model_manual(channel, "gpt-image-2".into(), "blue dot".into())
                .await
                .unwrap();
        assert!(result.content.contains("1 image"));
        assert!(!result.raw.contains("private-image"));
        assert_eq!(result.output_tokens, 10);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        server.abort();
    }

    #[test]
    fn media_probe_respects_exact_modalities_and_gemini_contract() {
        let mut channel = crate::coding_gateway::tests::channel(
            crate::source_driver::SourceDriverId::GeminiApi,
            "https://example.test",
        );
        channel.v2.default_target.protocol = crate::protocol::kind::ProtocolKind::GeminiNative;
        channel.capability_profiles.push(ChannelCapabilityProfile {
            model_pattern: "gemini-fixture-image".into(),
            catalog_metadata: true,
            catalog_text_output: Some(true),
            image_output: true,
            ..Default::default()
        });
        let kind = media_probe_kind(&channel, "gemini-fixture-image").unwrap();
        assert!(kind.image && !kind.non_text);
        assert!(media_probe_kind(&channel, "gemini-fixture-chat").is_none());
        let request = image_probe_request(&channel, "gemini-fixture-image", "blue dot").unwrap();
        assert_eq!(
            request.path,
            "/v1beta/models/gemini-fixture-image:generateContent"
        );
        let body: serde_json::Value = serde_json::from_str(request.body.utf8().unwrap()).unwrap();
        assert_eq!(body["generationConfig"]["responseModalities"][0], "IMAGE");
        channel.models = vec!["gpt-image-2".into(), "gemini-fixture-chat".into()];
        channel.upstream_model = "gpt-image-2".into();
        assert_eq!(automatic_probe_model(&channel), "gemini-fixture-chat");
        assert!(!channel_has_only_non_text_models(&channel));
        channel.models = vec!["gpt-image-2".into()];
        assert!(channel_has_only_non_text_models(&channel));
        let mut text_channel = crate::coding_gateway::tests::channel(
            crate::source_driver::SourceDriverId::OpenAiApi,
            "https://example.test/v1",
        );
        text_channel
            .capability_profiles
            .push(ChannelCapabilityProfile {
                model_pattern: "gpt-chat-with-image-tool".into(),
                catalog_metadata: true,
                catalog_text_output: Some(true),
                image_output: true,
                ..Default::default()
            });
        assert!(media_probe_kind(&text_channel, "gpt-chat-with-image-tool").is_none());
    }
}

async fn test_image_channel(
    channel: &ChannelConfig,
    model: &str,
    prompt: &str,
) -> Result<ChannelUpstreamTestResult> {
    let wire = image_probe_request(channel, model, prompt)?;
    let request = SupplierMessage {
        output_credit: None,
        id: "manual-image-test".into(),
        kind: "http_request".into(),
        payload: crate::surface_wire::wire_payload(wire)?,
    };
    let supplier = supplier_from_channel_unfiltered(channel);
    let client = crate::http_client::channel_client(channel.v2.use_system_proxy)?;
    let (outbound, _inbound) = mpsc::channel(1);
    let started = std::time::Instant::now();
    let (result, version) = crate::upstream_transport::observe_http_version(Box::pin(
        forward_supplier_request(&client, &supplier, &request, false, &outbound),
    ))
    .await;
    let payload =
        result?.ok_or_else(|| anyhow!("image test did not return a buffered response"))?;
    let response = crate::surface_wire::SurfaceEnvelope::from_payload(&payload)?;
    let status = response.status.unwrap_or_default();
    let raw = response.body.utf8()?;
    let headers = response.header_map()?;
    let header_refs = headers
        .iter()
        .filter_map(|(name, value)| value.to_str().ok().map(|value| (name.as_str(), value)))
        .collect::<Vec<_>>();
    if !(200..300).contains(&status)
        || crate::upstream_failure::observe(status, raw, &header_refs, Some(model)).is_some()
    {
        return Err(ProbeError::response(status, raw, &header_refs, model).into());
    }
    let value: serde_json::Value = serde_json::from_str(raw)?;
    let count = returned_image_count(&value).max(
        value
            .get("candidates")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .flat_map(|candidate| {
                candidate
                    .pointer("/content/parts")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .filter(|part| {
                part.get("inlineData")
                    .or_else(|| part.get("inline_data"))
                    .is_some_and(|data| {
                        data.get("data")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|data| !data.is_empty())
                            && data
                                .get("mimeType")
                                .or_else(|| data.get("mime_type"))
                                .and_then(serde_json::Value::as_str)
                                .is_some_and(|mime| mime.starts_with("image/"))
                    })
            })
            .count() as u64,
    );
    if count == 0 {
        return Err(anyhow!(
            "inconclusive: image test returned no image; the model was not marked healthy"
        ));
    }
    let (input_tokens, output_tokens) = extract_usage_tokens(raw);
    // Do not move multi-megabyte base64 data or expiring private URLs into UI logs.
    let summary = serde_json::json!({"operation":"image_generation","images_returned":count,
        "input_tokens":input_tokens,"output_tokens":output_tokens})
    .to_string();
    Ok(ChannelUpstreamTestResult {
        http_status: status,
        latency_ms: started.elapsed().as_millis(),
        checked_at_unix: now_unix(),
        upstream_http_version: version
            .map(crate::upstream_transport::http_version_transport)
            .unwrap_or_default()
            .into(),
        model: model.into(),
        content: format!("Image generation succeeded: {count} image(s)."),
        raw: summary,
        input_tokens,
        output_tokens,
    })
}
