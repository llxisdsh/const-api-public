fn is_openai_subscription_image_request(method: &warp::http::Method, path: &str) -> bool {
    *method == warp::http::Method::POST && openai_subscription_image_operation(path).is_some()
}

fn openai_subscription_image_operation(path: &str) -> Option<&'static str> {
    match path.split_once('?').map_or(path, |(path, _)| path) {
        "/v1/images/generations" => Some("images/generations"),
        "/v1/images/edits" => Some("images/edits"),
        _ => None,
    }
}

#[derive(Clone, Debug)]
struct OpenAiSubscriptionImagePayload {
    body: bytes::Bytes,
    content_type: String,
}

fn openai_subscription_image_content_type(headers: &warp::http::HeaderMap) -> String {
    headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= 512 && value.is_ascii())
        .unwrap_or("application/json")
        .to_string()
}

fn insert_openai_image_form_value(
    object: &mut serde_json::Map<String, serde_json::Value>,
    key: String,
    value: serde_json::Value,
) {
    use serde_json::map::Entry;

    match object.entry(key) {
        Entry::Vacant(entry) => {
            entry.insert(value);
        }
        Entry::Occupied(mut entry) => match entry.get_mut() {
            serde_json::Value::Array(values) => values.push(value),
            existing => {
                let first = std::mem::replace(existing, serde_json::Value::Null);
                *existing = serde_json::Value::Array(vec![first, value]);
            }
        },
    }
}

fn openai_image_form_scalar(name: &str, value: String) -> serde_json::Value {
    let trimmed = value.trim();
    if matches!(name, "n" | "output_compression" | "partial_images") {
        if let Ok(value) = trimmed.parse::<i64>() {
            return serde_json::Value::Number(value.into());
        }
    }
    if name == "stream" {
        if let Ok(value) = trimmed.parse::<bool>() {
            return serde_json::Value::Bool(value);
        }
    }
    serde_json::Value::String(value)
}

fn append_openai_image_reference(images: &mut Vec<serde_json::Value>, value: String) {
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&value) {
        match parsed {
            serde_json::Value::Array(values) => {
                images.extend(values);
                return;
            }
            serde_json::Value::Object(_) => {
                images.push(parsed);
                return;
            }
            serde_json::Value::String(image_url) => {
                images.push(serde_json::json!({"image_url": image_url}));
                return;
            }
            _ => {}
        }
    }
    images.push(serde_json::json!({"image_url": value}));
}

fn openai_image_data_url(
    content_type: Option<&str>,
    file_name: Option<&str>,
    bytes: &[u8],
) -> String {
    let content_type = content_type
        .map(str::trim)
        .filter(|value| value.starts_with("image/"))
        .map(str::to_string)
        .or_else(|| {
            let file_name = file_name?;
            let extension = file_name
                .rsplit_once('.')
                .map_or(file_name, |(_, extension)| extension)
                .to_ascii_lowercase();
            match extension.as_str() {
                "png" => Some("image/png".to_string()),
                "jpg" | "jpeg" => Some("image/jpeg".to_string()),
                "webp" => Some("image/webp".to_string()),
                "gif" => Some("image/gif".to_string()),
                _ => None,
            }
        })
        .unwrap_or_else(|| "application/octet-stream".to_string());
    format!(
        "data:{content_type};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

async fn normalize_openai_subscription_image_payload(
    operation: &str,
    headers: &warp::http::HeaderMap,
    body: bytes::Bytes,
) -> Result<OpenAiSubscriptionImagePayload> {
    let content_type = openai_subscription_image_content_type(headers);
    if serde_json::from_slice::<serde_json::Value>(&body).is_ok() {
        return Ok(OpenAiSubscriptionImagePayload {
            body,
            content_type: "application/json".to_string(),
        });
    }
    if operation != "images/edits" || !content_type.to_ascii_lowercase().starts_with("multipart/") {
        return Ok(OpenAiSubscriptionImagePayload { body, content_type });
    }

    let boundary = multer::parse_boundary(&content_type)
        .map_err(|err| anyhow!("invalid OpenAI image edit multipart boundary: {err}"))?;
    let stream = futures_util::stream::once(async move { Ok::<_, std::io::Error>(body) });
    let mut multipart = multer::Multipart::new(stream, boundary);
    let mut object = serde_json::Map::new();
    let mut images = Vec::new();
    let mut mask = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|err| anyhow!("parse OpenAI image edit multipart body: {err}"))?
    {
        let name = field
            .name()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("OpenAI image edit multipart field has no name"))?
            .to_string();
        let file_name = field.file_name().map(str::to_string);
        let field_content_type = field.content_type().map(ToString::to_string);
        let is_image_file = matches!(name.as_str(), "image" | "image[]" | "images" | "images[]");
        let is_mask_file = name == "mask";
        let is_file = file_name.is_some()
            || field_content_type
                .as_deref()
                .is_some_and(|value| value.starts_with("image/"));
        let value = field
            .bytes()
            .await
            .map_err(|err| anyhow!("read OpenAI image edit multipart field {name}: {err}"))?;

        if is_file {
            if !is_image_file && !is_mask_file {
                return Err(anyhow!(
                    "unsupported OpenAI image edit multipart file field {name}"
                ));
            }
            let data_url =
                openai_image_data_url(field_content_type.as_deref(), file_name.as_deref(), &value);
            if is_mask_file {
                if mask.is_some() {
                    return Err(anyhow!(
                        "OpenAI image edit multipart request contains more than one mask"
                    ));
                }
                mask = Some(serde_json::json!({"image_url": data_url}));
            } else {
                images.push(serde_json::json!({"image_url": data_url}));
            }
            continue;
        }

        let text = String::from_utf8(value.to_vec())
            .map_err(|_| anyhow!("OpenAI image edit multipart field {name} is not UTF-8"))?;
        match name.as_str() {
            "image" | "image[]" | "images" | "images[]" => {
                append_openai_image_reference(&mut images, text);
            }
            "mask[file_id]" => {
                let mask_object = mask.get_or_insert_with(|| serde_json::json!({}));
                mask_object["file_id"] = serde_json::Value::String(text);
            }
            "mask[image_url]" | "mask" => {
                let mask_object = mask.get_or_insert_with(|| serde_json::json!({}));
                mask_object["image_url"] = serde_json::Value::String(text);
            }
            _ => insert_openai_image_form_value(
                &mut object,
                name.clone(),
                openai_image_form_scalar(&name, text),
            ),
        }
    }

    if !images.is_empty() {
        object.insert("images".to_string(), serde_json::Value::Array(images));
    }
    if let Some(mask) = mask {
        object.insert("mask".to_string(), mask);
    }

    Ok(OpenAiSubscriptionImagePayload {
        body: bytes::Bytes::from(
            serde_json::to_vec(&serde_json::Value::Object(object))
                .map_err(|err| anyhow!("encode OpenAI image edit JSON body: {err}"))?,
        ),
        content_type: "application/json".to_string(),
    })
}

fn codex_image_url_from_responses_url(responses_url: &str, operation: &str) -> String {
    let operation = operation.trim_matches('/');
    if let Ok(mut url) = reqwest::Url::parse(responses_url) {
        let current_path = url.path().trim_end_matches('/').to_string();
        let base_path = current_path
            .strip_suffix("/responses")
            .unwrap_or(&current_path);
        let image_path = format!("{base_path}/{operation}");
        url.set_path(&image_path);
        return url.to_string();
    }
    format!(
        "{}/{}",
        responses_url
            .trim_end_matches('/')
            .trim_end_matches("/responses"),
        operation
    )
}

fn openai_subscription_image_request_builder(
    client: &Client,
    url: &str,
    token: &str,
    credential: &serde_json::Value,
    inbound_headers: &warp::http::HeaderMap,
    payload: OpenAiSubscriptionImagePayload,
) -> reqwest::RequestBuilder {
    let identity = active_codex_identity();
    let request_context = codex_model_request_context_headers(Some(inbound_headers), &identity);
    let mut request = client
        .post(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::CONTENT_TYPE, &payload.content_type)
        .header("Authorization", format!("Bearer {token}"))
        .headers(request_context)
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

async fn send_openai_subscription_image_request(
    client: &Client,
    channel: &ChannelConfig,
    url: &str,
    operation: &str,
    inbound_headers: &warp::http::HeaderMap,
    body: bytes::Bytes,
) -> Result<reqwest::Response> {
    let payload =
        normalize_openai_subscription_image_payload(operation, inbound_headers, body).await?;
    let (token, credential, _) = ensure_openai_subscription_access_token(client, channel).await?;
    let request = openai_subscription_image_request_builder(
        client,
        url,
        &token,
        &credential,
        inbound_headers,
        payload.clone(),
    );
    let response = crate::upstream_transport::send(request).await?;
    if response.status() != reqwest::StatusCode::UNAUTHORIZED {
        return Ok(response);
    }

    let _ = response.bytes().await;
    let (fresh_token, fresh_credential, _) = ensure_openai_subscription_access_token_with_options(
        client,
        channel,
        Some(&token),
        OPENAI_OAUTH_TOKEN_URL,
    )
    .await?;
    crate::upstream_transport::send(openai_subscription_image_request_builder(
        client,
        url,
        &fresh_token,
        &fresh_credential,
        inbound_headers,
        payload,
    ))
    .await
    .map_err(Into::into)
}

async fn forward_openai_subscription_image_request(
    client: &Client,
    channel: &ChannelConfig,
    path: &str,
    inbound_headers: &warp::http::HeaderMap,
    body: bytes::Bytes,
) -> Result<warp::reply::Response> {
    let operation = openai_subscription_image_operation(path)
        .ok_or_else(|| anyhow!("unsupported OpenAI subscription image operation"))?;
    let upstream_url = codex_image_url_from_responses_url(&codex_responses_url(), operation);
    let response = send_openai_subscription_image_request(
        client,
        channel,
        &upstream_url,
        operation,
        inbound_headers,
        body,
    )
    .await?;

    // Image responses can contain large base64 payloads. Relay them without a
    // full-body buffer and without synthesizing a Responses-shaped result.
    response_from_reqwest(response, true).await
}
