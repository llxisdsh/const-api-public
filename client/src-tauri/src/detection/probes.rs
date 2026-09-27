#[cfg(test)]
pub(crate) async fn fetch_anthropic_models(
    client: &Client,
    channel: &ChannelConfig,
    base_url: &str,
    api_key: &str,
) -> Result<Vec<String>> {
    Ok(model_ids(
        fetch_anthropic_catalog(client, channel, base_url, api_key).await?,
    ))
}

pub(crate) async fn fetch_anthropic_catalog(
    client: &Client,
    channel: &ChannelConfig,
    base_url: &str,
    api_key: &str,
) -> Result<Vec<crate::model_catalog::ModelObservation>> {
    if crate::coding_gateway::shares_catalog(channel.source_driver()) {
        return fetch_openai_compatible_catalog(client, channel, base_url, api_key).await;
    }
    let mut url = Url::parse(&join_upstream_url(base_url, "/v1/models"))?;
    url.query_pairs_mut().append_pair("limit", "1000");
    let mut headers = HeaderMap::new();
    crate::channel_user_agent::apply_to_headers(channel, &mut headers);
    headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
    if !api_key.trim().is_empty() {
        headers.insert("x-api-key", HeaderValue::from_str(api_key.trim())?);
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", api_key.trim()))?,
        );
    }
    fetch_model_catalog(client, url, headers, ModelCatalogKind::Anthropic).await
}

pub(crate) async fn fetch_gemini_native_catalog(
    client: &Client,
    channel: &ChannelConfig,
    base_url: &str,
    api_key: &str,
) -> Result<Vec<crate::model_catalog::ModelObservation>> {
    // OpenCode shares one OpenAI-shaped catalog across all native protocols.
    // This also matters when only the Gemini binding has been verified.
    if crate::coding_gateway::is_opencode(channel.source_driver()) {
        return fetch_openai_compatible_catalog(client, channel, base_url, api_key).await;
    }
    let url = Url::parse(&join_gemini_native_url(base_url, "/v1beta/models")?)?;
    let mut headers = HeaderMap::new();
    crate::channel_user_agent::apply_to_headers(channel, &mut headers);
    if !api_key.trim().is_empty() {
        headers.insert("x-goog-api-key", HeaderValue::from_str(api_key.trim())?);
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", api_key.trim()))?,
        );
    }
    fetch_model_catalog(client, url, headers, ModelCatalogKind::Gemini).await
}

pub(crate) async fn fetch_ollama_tags_models(
    client: &Client,
    channel: &ChannelConfig,
    base_url: &str,
) -> Result<Vec<String>> {
    let request = client.get(ollama_tags_url(base_url)?);
    let resp = crate::channel_user_agent::apply_to_request(channel, request)
        .send_adaptive()
        .await?;
    let status = resp.status();
    let value = resp.json::<serde_json::Value>().await?;
    if !status.is_success() {
        return Err(anyhow!("ollama tags returned {status}: {value}"));
    }
    let models = models_from_ollama_tags_value(&value);
    if models.is_empty() {
        return Err(anyhow!("ollama returned no models"));
    }
    Ok(models)
}

pub(crate) async fn probe_openai_responses(
    client: &Client,
    channel: &ChannelConfig,
    model: &str,
) -> Result<()> {
    let url = join_upstream_url(&channel.upstream_base_url, "/v1/responses");
    let body = serde_json::json!({"model": model, "input": "ping"});
    let resp = openai_request_with_base(
        client.post(url),
        channel,
        &channel.upstream_api_key,
        &channel.upstream_base_url,
    )
    .json(&body)
    .send_adaptive()
    .await?;
    ensure_probe_success(resp, "OpenAI Responses").await
}

pub(crate) async fn probe_openai_responses_stream(
    client: &Client,
    channel: &ChannelConfig,
    model: &str,
) -> Result<()> {
    let url = join_upstream_url(&channel.upstream_base_url, "/v1/responses");
    let body = serde_json::json!({
        "model": model,
        "input": "ping",
        "stream": true,
        "max_output_tokens": 16
    });
    let resp = openai_request_with_base(
        client.post(url),
        channel,
        &channel.upstream_api_key,
        &channel.upstream_base_url,
    )
    .json(&body)
    .send_adaptive()
    .await?;
    ensure_sse_probe_success(resp, "OpenAI Responses stream").await
}


pub(crate) async fn probe_openai_responses_json_schema(
    client: &Client,
    channel: &ChannelConfig,
    model: &str,
) -> Result<()> {
    let url = join_upstream_url(&channel.upstream_base_url, "/v1/responses");
    let body = serde_json::json!({
        "model": model,
        "input": "Return {\"ok\":true}.",
        "text": {
            "format": {
                "type": "json_schema",
                "name": "const_probe_schema",
                "strict": true,
                "schema": {
                    "type": "object",
                    "properties": {"ok": {"type": "boolean"}},
                    "required": ["ok"],
                    "additionalProperties": false
                }
            }
        },
        "max_output_tokens": 32
    });
    let resp = openai_request_with_base(
        client.post(url),
        channel,
        &channel.upstream_api_key,
        &channel.upstream_base_url,
    )
    .json(&body)
    .send_adaptive()
    .await?;
    let status = resp.status();
    let value = resp.json::<serde_json::Value>().await?;
    if !status.is_success() {
        return Err(anyhow!(
            "OpenAI Responses JSON schema probe returned {status}: {value}"
        ));
    }
    let content = value
        .get("output_text")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            value
                .pointer("/output/0/content/0/text")
                .and_then(serde_json::Value::as_str)
        })
        .unwrap_or_default();
    let parsed: serde_json::Value = serde_json::from_str(content).with_context(|| {
        format!("Responses JSON schema probe returned non-JSON content: {content}")
    })?;
    if parsed.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(anyhow!(
            "Responses JSON schema probe returned unexpected payload: {parsed}"
        ));
    }
    Ok(())
}

pub(crate) async fn probe_openai_chat(
    client: &Client,
    channel: &ChannelConfig,
    model: &str,
) -> Result<()> {
    probe_openai_chat_with_base(
        client,
        channel,
        &channel.upstream_base_url,
        &channel.upstream_api_key,
        model,
    )
    .await
}

pub(crate) async fn probe_openai_chat_with_base(
    client: &Client,
    channel: &ChannelConfig,
    base_url: &str,
    api_key: &str,
    model: &str,
) -> Result<()> {
    let url = join_upstream_url(base_url, "/v1/chat/completions");
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": "ping"}],
        "max_tokens": 4
    });
    let resp = openai_request_with_base(client.post(url), channel, api_key, base_url)
        .json(&body)
        .send_adaptive()
        .await?;
    ensure_probe_success(resp, "OpenAI Chat Completions").await
}

pub(crate) async fn probe_openai_chat_stream(
    client: &Client,
    channel: &ChannelConfig,
    model: &str,
) -> Result<()> {
    let url = join_upstream_url(&channel.upstream_base_url, "/v1/chat/completions");
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": "ping"}],
        "stream": true,
        "max_tokens": 4
    });
    let resp = openai_request_with_base(
        client.post(url),
        channel,
        &channel.upstream_api_key,
        &channel.upstream_base_url,
    )
    .json(&body)
    .send_adaptive()
    .await?;
    let status = resp.status();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let raw = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!("OpenAI Chat stream returned {status}: {raw}"));
    }
    if !content_type.contains("text/event-stream") && !raw.trim_start().starts_with("data:") {
        return Err(anyhow!(
            "OpenAI Chat stream did not return SSE content-type or data frames"
        ));
    }
    Ok(())
}


pub(crate) async fn probe_openai_chat_json_schema(
    client: &Client,
    channel: &ChannelConfig,
    base_url: &str,
    api_key: &str,
    model: &str,
) -> Result<()> {
    let url = join_upstream_url(base_url, "/v1/chat/completions");
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": "Return {\"ok\":true}."}],
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "const_probe_schema",
                "strict": true,
                "schema": {
                    "type": "object",
                    "properties": {"ok": {"type": "boolean"}},
                    "required": ["ok"],
                    "additionalProperties": false
                }
            }
        },
        "max_tokens": 32
    });
    let resp = openai_request_with_base(client.post(url), channel, api_key, base_url)
        .json(&body)
        .send_adaptive()
        .await?;
    let status = resp.status();
    let value = resp.json::<serde_json::Value>().await?;
    if !status.is_success() {
        return Err(anyhow!(
            "OpenAI Chat JSON schema probe returned {status}: {value}"
        ));
    }
    let content = value
        .pointer("/choices/0/message/content")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let parsed: serde_json::Value = serde_json::from_str(content)
        .with_context(|| format!("JSON schema probe returned non-JSON content: {content}"))?;
    if parsed.get("ok").and_then(|value| value.as_bool()) != Some(true) {
        return Err(anyhow!(
            "JSON schema probe returned unexpected payload: {parsed}"
        ));
    }
    Ok(())
}

pub(crate) async fn probe_anthropic_messages(
    client: &Client,
    channel: &ChannelConfig,
    model: &str,
) -> Result<()> {
    let body = serde_json::json!({
        "model": model,
        "max_tokens": 4,
        "messages": [{"role": "user", "content": "ping"}]
    });
    let resp = anthropic_messages_request(client, channel, &body)
        .send_adaptive()
        .await?;
    ensure_probe_success(resp, "Anthropic Messages").await
}

pub(crate) async fn probe_anthropic_messages_stream(
    client: &Client,
    channel: &ChannelConfig,
    model: &str,
) -> Result<()> {
    let body = serde_json::json!({
        "model": model,
        "max_tokens": 4,
        "stream": true,
        "messages": [{"role": "user", "content": "ping"}]
    });
    let resp = anthropic_messages_request(client, channel, &body)
        .send_adaptive()
        .await?;
    ensure_sse_probe_success(resp, "Anthropic Messages stream").await
}


pub(crate) async fn probe_anthropic_messages_cache_control(
    client: &Client,
    channel: &ChannelConfig,
    model: &str,
) -> Result<()> {
    let body = serde_json::json!({
        "model": model,
        "max_tokens": 4,
        "system": [{
            "type": "text",
            "text": "cache probe",
            "cache_control": {"type": "ephemeral"}
        }],
        "messages": [{"role": "user", "content": "ping"}]
    });
    let resp = anthropic_messages_request(client, channel, &body)
        .send_adaptive()
        .await?;
    let status = resp.status();
    let value = resp.json::<serde_json::Value>().await?;
    if !status.is_success() {
        return Err(anyhow!(
            "Anthropic Messages cache_control returned {status}: {value}"
        ));
    }
    if !anthropic_has_cache_usage_fields(&value) {
        return Err(anyhow!(
            "Anthropic Messages cache_control returned no cache usage fields: {value}"
        ));
    }
    Ok(())
}

fn anthropic_messages_request(
    client: &Client,
    channel: &ChannelConfig,
    body: &serde_json::Value,
) -> reqwest::RequestBuilder {
    let base_url = if channel.kind.trim().eq_ignore_ascii_case("aws_bedrock") {
        bedrock_anthropic_base(&channel.upstream_base_url)
    } else {
        channel.upstream_base_url.clone()
    };
    let url = join_upstream_url(&base_url, "/v1/messages");
    let mut req = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("anthropic-version", "2023-06-01")
        .json(&body);
    if !channel.upstream_api_key.trim().is_empty() {
        req = req
            .header("x-api-key", channel.upstream_api_key.trim())
            .header(
                "Authorization",
                format!("Bearer {}", channel.upstream_api_key.trim()),
            );
    }
    crate::channel_user_agent::apply_to_request(channel, req)
}

fn anthropic_has_cache_usage_fields(value: &serde_json::Value) -> bool {
    let Some(usage) = value.get("usage").and_then(|value| value.as_object()) else {
        return false;
    };
    ["cache_creation_input_tokens", "cache_read_input_tokens"]
        .iter()
        .any(|key| usage.get(*key).and_then(|value| value.as_i64()).is_some())
}

pub(crate) async fn probe_gemini_native(
    client: &Client,
    channel: &ChannelConfig,
    model: &str,
) -> Result<()> {
    let path = format!("/v1beta/models/{model}:generateContent");
    let url = crate::coding_gateway::gemini_url(
        channel.source_driver(),
        &channel.upstream_base_url,
        &path,
    )?;
    let body = serde_json::json!({
        "contents": [{"parts": [{"text": "ping"}]}],
        "generationConfig": {"maxOutputTokens": 4}
    });
    let mut req = client
        .post(url)
        .header("Content-Type", "application/json")
        .json(&body);
    if !channel.upstream_api_key.trim().is_empty() {
        req = req
            .header("x-goog-api-key", channel.upstream_api_key.trim())
            .header(
                "Authorization",
                format!("Bearer {}", channel.upstream_api_key.trim()),
            );
    }
    req = crate::channel_user_agent::apply_to_request(channel, req);
    let resp = req.send_adaptive().await?;
    ensure_probe_success(resp, "Gemini native").await
}

pub(crate) async fn probe_gemini_native_stream(
    client: &Client,
    channel: &ChannelConfig,
    model: &str,
) -> Result<()> {
    let path = format!("/v1beta/models/{model}:streamGenerateContent?alt=sse");
    let body = serde_json::json!({
        "contents": [{"parts": [{"text": "ping"}]}],
        "generationConfig": {"maxOutputTokens": 4}
    });
    let resp = gemini_native_request(client, channel, &path, &body)?
        .send_adaptive()
        .await?;
    ensure_sse_probe_success(resp, "Gemini native stream").await
}


pub(crate) async fn probe_gemini_native_json_schema(
    client: &Client,
    channel: &ChannelConfig,
    model: &str,
) -> Result<()> {
    let path = format!("/v1beta/models/{model}:generateContent");
    let body = serde_json::json!({
        "contents": [{"parts": [{"text": "Return {\"ok\":true}."}]}],
        "generationConfig": {
            "responseMimeType": "application/json",
            "responseSchema": {
                "type": "object",
                "properties": {"ok": {"type": "boolean"}},
                "required": ["ok"]
            },
            "maxOutputTokens": 32
        }
    });
    let resp = gemini_native_request(client, channel, &path, &body)?
        .send_adaptive()
        .await?;
    let status = resp.status();
    let value = resp.json::<serde_json::Value>().await?;
    if !status.is_success() {
        return Err(anyhow!(
            "Gemini native JSON schema probe returned {status}: {value}"
        ));
    }
    let content = value
        .pointer("/candidates/0/content/parts/0/text")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let parsed: serde_json::Value = serde_json::from_str(content)
        .with_context(|| format!("Gemini JSON schema probe returned non-JSON text: {content}"))?;
    if parsed.get("ok").and_then(|value| value.as_bool()) != Some(true) {
        return Err(anyhow!(
            "Gemini JSON schema probe returned unexpected payload: {parsed}"
        ));
    }
    Ok(())
}

fn gemini_native_request(
    client: &Client,
    channel: &ChannelConfig,
    path: &str,
    body: &serde_json::Value,
) -> Result<reqwest::RequestBuilder> {
    let url = crate::coding_gateway::gemini_url(
        channel.source_driver(),
        &channel.upstream_base_url,
        path,
    )?;
    let mut req = client
        .post(url)
        .header("Content-Type", "application/json")
        .json(&body);
    if !channel.upstream_api_key.trim().is_empty() {
        req = req
            .header("x-goog-api-key", channel.upstream_api_key.trim())
            .header(
                "Authorization",
                format!("Bearer {}", channel.upstream_api_key.trim()),
            );
    }
    Ok(crate::channel_user_agent::apply_to_request(channel, req))
}

pub(crate) async fn ensure_sse_probe_success(resp: reqwest::Response, label: &str) -> Result<()> {
    let status = resp.status();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let raw = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!("{label} returned {status}: {raw}"));
    }
    let trimmed = raw.trim_start();
    if !content_type.contains("text/event-stream")
        && !trimmed.starts_with("data:")
        && !trimmed.starts_with("event:")
    {
        return Err(anyhow!(
            "{label} did not return SSE content-type or event/data frames"
        ));
    }
    Ok(())
}

pub(crate) async fn ensure_probe_success(resp: reqwest::Response, label: &str) -> Result<()> {
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if status.is_success() {
        return Ok(());
    }
    Err(anyhow!("{label} returned {status}: {text}"))
}

pub(crate) fn openai_request_with_base(
    req: reqwest::RequestBuilder,
    channel: &ChannelConfig,
    api_key: &str,
    base_url: &str,
) -> reqwest::RequestBuilder {
    let req = crate::channel_user_agent::apply_to_request(channel, req)
        .header("Content-Type", "application/json");
    if api_key.trim().is_empty() {
        req
    } else {
        let req = req
            .header("Authorization", format!("Bearer {}", api_key.trim()))
            .header("X-API-Key", api_key.trim());
        if is_azure_openai_base_url(base_url) {
            req.header("api-key", api_key.trim())
        } else {
            req
        }
    }
}

#[cfg(test)]
pub(crate) fn models_from_codex_oauth_value(value: &serde_json::Value) -> Vec<String> {
    model_ids(model_observations_from_value(
        ModelCatalogKind::Codex,
        value,
        "codex",
    ))
}

pub(crate) fn models_from_ollama_tags_value(value: &serde_json::Value) -> Vec<String> {
    model_ids(model_observations_from_value(
        ModelCatalogKind::Ollama,
        value,
        "ollama",
    ))
}

pub(crate) fn normalize_probe_channel(mut channel: ChannelConfig) -> ChannelConfig {
    channel = project_channel_legacy_fields(channel);
    channel.surface_bindings = crate::channel_surface::normalize_channel_surface_bindings(&channel);
    channel.supported_protocols = crate::channel_surface::protocols_from_surface_bindings(&channel);
    let hint = channel_provider_hint(&channel);
    if hint == "openai" && channel.upstream_base_url.trim().is_empty() {
        channel.upstream_base_url = "https://api.openai.com/v1".to_string();
    }
    if hint == "anthropic" && channel.upstream_base_url.trim().is_empty() {
        channel.upstream_base_url = "https://api.anthropic.com".to_string();
    }
    if hint == "gemini" && channel.upstream_base_url.trim().is_empty() {
        channel.upstream_base_url = "https://generativelanguage.googleapis.com".to_string();
    }
    channel
}

pub(crate) fn channel_provider_hint(channel: &ChannelConfig) -> String {
    use crate::source_driver::SourceDriverId;
    match channel.source_driver() {
        SourceDriverId::OpenAiApi => return "openai".to_string(),
        SourceDriverId::AnthropicApi | SourceDriverId::ClaudeSubscription => {
            return "anthropic".to_string()
        }
        SourceDriverId::GeminiApi | SourceDriverId::GeminiSubscription => {
            return "gemini".to_string()
        }
        SourceDriverId::Ollama | SourceDriverId::LmStudio | SourceDriverId::Vllm => {
            return "local_model".to_string()
        }
        SourceDriverId::BedrockMantle => {
            return match channel
                .surface_bindings
                .first()
                .map(|binding| binding.surface)
            {
                Some(crate::surface::ApiSurface::Anthropic) => "anthropic".to_string(),
                _ => "openai_compatible".to_string(),
            }
        }
        SourceDriverId::LanShare
        | SourceDriverId::CustomEndpoint
        | SourceDriverId::Omniroute
        | SourceDriverId::XaiApi
        | SourceDriverId::MistralApi
        | SourceDriverId::DeepseekApi
        | SourceDriverId::DashscopeApi
        | SourceDriverId::MoonshotApi
        | SourceDriverId::ZhipuApi
        | SourceDriverId::MinimaxApi
        | SourceDriverId::StepfunApi
        | SourceDriverId::GroqApi
        | SourceDriverId::TogetherApi
        | SourceDriverId::FireworksApi
        | SourceDriverId::PerplexityApi
        | SourceDriverId::HuggingfaceApi
        | SourceDriverId::NvidiaApi
        | SourceDriverId::SiliconflowApi
        | SourceDriverId::VolcengineArkApi
        | SourceDriverId::BaiduQianfanApi
        | SourceDriverId::TencentHunyuanApi
        | SourceDriverId::OpencodeGo
        | SourceDriverId::OpencodeZen
        | SourceDriverId::KiloGateway
        | SourceDriverId::ClineApi
        | SourceDriverId::CommandCode
        | SourceDriverId::KimiCode
        | SourceDriverId::GlmCodingPlan
        | SourceDriverId::MinimaxTokenPlan
        | SourceDriverId::OllamaCloud
        | SourceDriverId::Openrouter => {
            return match channel
                .surface_bindings
                .first()
                .map(|binding| binding.surface)
            {
                Some(crate::surface::ApiSurface::Anthropic) => "anthropic".to_string(),
                Some(crate::surface::ApiSurface::Gemini) => "gemini".to_string(),
                _ => "openai_compatible".to_string(),
            }
        }
        SourceDriverId::AzureOpenAi
        | SourceDriverId::OpenAiSubscription
        | SourceDriverId::GrokSubscription => return "openai_compatible".to_string(),
    }
}

pub(crate) fn pick_primary_model(models: &[String], channel: &ChannelConfig) -> String {
    for configured in [&channel.upstream_model, &channel.public_model] {
        let configured = configured.trim();
        if configured.is_empty() {
            continue;
        }
        if let Some(observed) = models
            .iter()
            .find(|model| model.trim().eq_ignore_ascii_case(configured))
        {
            return observed.clone();
        }
    }

    let Some(family) = conservative_probe_model_family(channel) else {
        return channel.default_model_from(models);
    };
    models
        .iter()
        .enumerate()
        .min_by_key(|(index, model)| (conservative_probe_model_rank(family, model), *index))
        .map(|(_, model)| model.clone())
        .unwrap_or_else(|| channel.default_model_from(models))
}

fn conservative_probe_model_family(channel: &ChannelConfig) -> Option<&str> {
    use crate::source_driver::SourceDriverId;
    match channel.source_driver() {
        SourceDriverId::OpenAiApi | SourceDriverId::OpenAiSubscription => Some("openai"),
        SourceDriverId::GroqApi => Some("groq"),
        SourceDriverId::ClaudeSubscription => Some("claude"),
        SourceDriverId::GeminiSubscription => Some("antigravity"),
        SourceDriverId::GrokSubscription => Some("grok"),
        _ if channel.kind.trim() == "subscription_adapter" => {
            let platform = platform_or_default(&channel.subscription.platform).trim();
            (!platform.is_empty()).then_some(platform)
        }
        _ => None,
    }
}

fn conservative_probe_model_rank(family: &str, model: &str) -> u8 {
    let family = family.trim().to_ascii_lowercase();
    let model = model.trim().to_ascii_lowercase();
    match family.as_str() {
        "claude" | "anthropic" => {
            if model.contains("haiku") {
                0
            } else if model.contains("sonnet") {
                1
            } else if model.contains("opus") {
                2
            } else if model.contains("fable") {
                4
            } else {
                3
            }
        }
        "antigravity" | "gemini" => {
            if model.contains("flash-lite") {
                0
            } else if model.contains("flash") {
                1
            } else if model.contains("pro") {
                2
            } else {
                3
            }
        }
        "openai" | "codex" | "chatgpt" => {
            if probe_model_is_non_conversational(&model) {
                4
            } else if model.contains("luna") || model.contains("mini") || model.contains("nano") {
                0
            } else if model.contains("sol") {
                1
            } else if model.contains("terra") {
                2
            } else {
                3
            }
        }
        "grok" => {
            if model.contains("fast") || model.contains("mini") {
                0
            } else {
                1
            }
        }
        "groq" => {
            if probe_model_is_non_conversational(&model) {
                4
            } else if model.contains("instant")
                || model.contains("mini")
                || model.contains("nano")
                || model.contains("8b")
                || model.contains("20b")
            {
                0
            } else if ["llama", "qwen", "mistral", "gemma"]
                .iter()
                .any(|family| model.contains(family))
            {
                1
            } else {
                2
            }
        }
        _ => 0,
    }
}

fn probe_model_is_non_conversational(model: &str) -> bool {
    [
        "audio",
        "dall-e",
        "embedding",
        "guard",
        "image",
        "moderation",
        "realtime",
        "speech",
        "transcribe",
        "tts",
        "whisper",
    ]
    .iter()
    .any(|marker| model.contains(marker))
}

pub(crate) fn subscription_supported_protocols(platform: &str) -> Vec<String> {
    match platform_or_default(platform) {
        "openai" | "codex" | "chatgpt" => vec!["openai_responses".to_string()],
        "grok" => vec!["openai_responses".to_string()],
        "antigravity" => vec!["gemini_native".to_string()],
        "claude" | "anthropic" => vec!["anthropic_messages".to_string()],
        _ => Vec::new(),
    }
}

pub(crate) fn platform_or_default(platform: &str) -> &str {
    if platform.trim().is_empty() {
        "claude"
    } else {
        platform
    }
}

pub(crate) fn should_try_ollama_tags(channel: &ChannelConfig) -> bool {
    let marker = format!(
        "{} {} {}",
        channel.kind, channel.name, channel.upstream_base_url
    )
    .to_lowercase();
    marker.contains("ollama") || marker.contains("11434")
}

pub(crate) fn gemini_openai_base(base_url: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    if base.ends_with("/v1beta/openai") || base.ends_with("/v1beta/openai/") {
        return base.trim_end_matches('/').to_string();
    }
    if base.contains("generativelanguage.googleapis.com") {
        return "https://generativelanguage.googleapis.com/v1beta/openai".to_string();
    }
    format!("{base}/v1beta/openai")
}

pub(crate) fn join_gemini_native_url(base_url: &str, path: &str) -> Result<String> {
    let parsed = reqwest::Url::parse(base_url.trim())?;
    let mut root = format!(
        "{}://{}",
        parsed.scheme(),
        parsed
            .host_str()
            .ok_or_else(|| anyhow!("missing Gemini upstream host"))?
    );
    if let Some(port) = parsed.port() {
        root.push_str(&format!(":{port}"));
    }
    Ok(format!("{}{}", root.trim_end_matches('/'), path))
}

pub(crate) fn ollama_tags_url(base_url: &str) -> Result<String> {
    let parsed = reqwest::Url::parse(base_url.trim())?;
    let mut root = format!(
        "{}://{}",
        parsed.scheme(),
        parsed
            .host_str()
            .ok_or_else(|| anyhow!("missing upstream host"))?
    );
    if let Some(port) = parsed.port() {
        root.push_str(&format!(":{port}"));
    }
    Ok(format!("{root}/api/tags"))
}
