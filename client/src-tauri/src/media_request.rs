//! Routing metadata for media uploads. The original body stays on the wire;
//! only the selected supplier adapter may normalize its protocol.
use anyhow::{Result, anyhow, ensure};
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{Map, Value, json};

const MAX_ROUTING_FIELD_BYTES: usize = 64 << 10;

pub(crate) async fn bounded_response(response: reqwest::Response) -> Result<Vec<u8>> {
    read_response_with_limit(response, crate::MODEL_MESSAGE_MAX_BYTES).await
}

async fn read_response_with_limit(response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    ensure!(
        response.content_length().is_none_or(|n| n <= limit as u64),
        "response exceeds the gateway message limit"
    );
    let mut bytes = Vec::new();
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(reqwest::Error::without_url)?;
        ensure!(
            bytes.len().saturating_add(chunk.len()) <= limit,
            "response exceeds the gateway message limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub(crate) async fn prepare_supplier_image_request(
    driver: crate::source_driver::SourceDriverId,
    operation: crate::surface::ApiOperation,
    request: &mut crate::channel_executor::ChannelRequest,
) -> Result<()> {
    use crate::source_driver::SourceDriverId::{
        GrokSubscription, SiliconflowApi, TogetherApi, VolcengineArkApi, XaiApi,
    };
    use crate::supplier::{
        remove_top_level_json_field, rewrite_top_level_json_field, top_level_json_bool,
        top_level_json_raw_token, top_level_json_string,
    };
    use crate::surface::ApiOperation::{ImagesEdits, ImagesGenerations};

    let inline_format = match (driver, operation) {
        (XaiApi | GrokSubscription, ImagesGenerations | ImagesEdits)
        | (VolcengineArkApi, ImagesGenerations) => Some("b64_json"),
        (TogetherApi, ImagesGenerations) => Some("base64"),
        (SiliconflowApi, ImagesGenerations) => None,
        _ => return Ok(()),
    };
    let prefer_inline = request
        .headers
        .get(crate::supplier::IMAGE_RESPONSE_FORMAT_HEADER)
        .is_some_and(|value| value == "b64_json");
    if operation == ImagesEdits {
        let content_type = request
            .headers
            .get(reqwest::header::CONTENT_TYPE)
            .map(|value| value.to_str())
            .transpose()?
            .unwrap_or("application/json");
        if content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .eq_ignore_ascii_case("multipart/form-data")
        {
            // Converting to JSON must not bypass multipart routing field validation.
            media_routing_body(operation, &request.headers, &request.body).await?;
            request.body = crate::supplier::xai_multipart_image_edit(
                content_type,
                request.body.clone(),
                request.selected_upstream_model.as_deref(),
                prefer_inline,
            )
            .await?;
            request.headers.insert(
                reqwest::header::CONTENT_TYPE,
                reqwest::header::HeaderValue::from_static("application/json"),
            );
            return Ok(());
        }
    }
    // In portable mode the client saves the actual returned encoding. xAI has
    // no codec selector and SiliconFlow's is model-specific. Together and Ark omit WebP.
    if prefer_inline {
        let omit_codec = match driver {
            XaiApi | GrokSubscription | SiliconflowApi => {
                top_level_json_raw_token(&request.body, "output_format").is_some()
            }
            TogetherApi | VolcengineArkApi => {
                top_level_json_string(&request.body, "output_format").as_deref() == Some("webp")
            }
            _ => false,
        };
        if omit_codec {
            request.body = Bytes::from(remove_top_level_json_field(
                std::str::from_utf8(&request.body)?,
                "output_format",
            )?);
        }
    }
    // A native request owns its explicit result format. Deferred xAI jobs
    // support URL results only, even if the caller prefers inline bytes.
    let Some(inline_format) = inline_format else {
        return Ok(());
    };
    if !prefer_inline && driver != TogetherApi {
        return Ok(());
    }
    let format = top_level_json_raw_token(&request.body, "response_format");
    let needs_format = match format {
        None | Some("null") => {
            prefer_inline
                && !(matches!(driver, XaiApi | GrokSubscription)
                    && top_level_json_bool(&request.body, "deferred") == Some(true))
        }
        Some(raw) => {
            driver == TogetherApi
                && serde_json::from_str::<String>(raw).is_ok_and(|format| format == "b64_json")
        }
    };
    if needs_format {
        let raw = std::str::from_utf8(&request.body)?;
        request.body = bytes::Bytes::from(rewrite_top_level_json_field(
            raw,
            "response_format",
            &serde_json::to_string(inline_format)?,
            true,
        )?);
    }
    Ok(())
}

pub(crate) async fn media_routing_body(
    operation: crate::surface::ApiOperation,
    headers: &reqwest::header::HeaderMap,
    body: &Bytes,
) -> Result<Bytes> {
    if !matches!(
        operation,
        crate::surface::ApiOperation::ImagesEdits
            | crate::surface::ApiOperation::AudioTranscriptions
            | crate::surface::ApiOperation::AudioTranslations
            | crate::surface::ApiOperation::VideosCreate
    ) {
        return Ok(body.clone());
    }
    let content_type = headers
        .get(reqwest::header::CONTENT_TYPE)
        .map(|value| value.to_str())
        .transpose()?
        .unwrap_or("application/json");
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if media_type == "application/json" || media_type.ends_with("+json") {
        // Validate without allocating another copy of inline base64 strings.
        if body.iter().find(|byte| !byte.is_ascii_whitespace()) != Some(&b'{')
            || serde_json::from_slice::<serde::de::IgnoredAny>(body).is_err()
        {
            return Err(anyhow!(
                "media request requires a JSON object or multipart/form-data"
            ));
        }
        return Ok(body.clone());
    }
    if media_type != "multipart/form-data" {
        return Err(anyhow!(
            "media request requires application/json or multipart/form-data with a boundary"
        ));
    }
    let boundary = multer::parse_boundary(content_type)
        .map_err(|_| anyhow!("media request requires a multipart boundary"))?;
    let body = body.clone();
    let stream = futures_util::stream::once(async move { Ok::<_, std::io::Error>(body) });
    let mut multipart = multer::Multipart::new(stream, boundary);
    let mut fields = Map::new();
    let mut images = Vec::new();
    let mut mask = Map::new();
    let mut count = 0;
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|_| anyhow!("invalid media multipart fields"))?
    {
        count += 1;
        if count > 1000 {
            return Err(anyhow!("excessive media multipart fields"));
        }
        let name = field.name().unwrap_or_default().to_string();
        let file = field.file_name().is_some()
            || field
                .content_type()
                .is_some_and(|kind| kind.type_() == "image");
        let routing_field = matches!(
            name.as_str(),
            "model"
                | "stream"
                | "service_tier"
                | "response_format"
                | "resolution"
                | "size"
                | "seconds"
                | "input_reference"
                | "image"
                | "image[]"
                | "images"
                | "images[]"
                | "mask"
                | "mask[file_id]"
                | "mask[image_url]"
        );
        if file || !routing_field {
            if file && matches!(name.as_str(), "model" | "stream" | "service_tier") {
                return Err(anyhow!("media routing fields must be text"));
            }
            // Drain without retaining file bytes or large prompt fields.
            while field.chunk().await?.is_some() {}
            continue;
        }
        let mut value = Vec::new();
        while let Some(chunk) = field.chunk().await? {
            if value.len() + chunk.len() > MAX_ROUTING_FIELD_BYTES {
                return Err(anyhow!(
                    "media routing fields must be at most 64 KiB; upload media bytes as files"
                ));
            }
            value.extend_from_slice(&chunk);
        }
        let text = std::str::from_utf8(&value)
            .map_err(|_| anyhow!("media routing fields must be UTF-8 text"))?
            .trim();
        match name.as_str() {
            "input_reference" => {
                if fields.contains_key(&name) {
                    return Err(anyhow!("duplicate media reference"));
                }
                fields.insert(
                    name,
                    serde_json::from_slice(&value)
                        .map_err(|_| anyhow!("invalid media reference"))?,
                );
            }
            "model" | "stream" | "service_tier" | "response_format" | "resolution" | "size"
            | "seconds" => {
                if fields.contains_key(&name) {
                    return Err(anyhow!("duplicate media routing field"));
                }
                let value = if name == "stream" {
                    match text {
                        "true" => Value::Bool(true),
                        "false" => Value::Bool(false),
                        _ => return Err(anyhow!("media stream must be true or false")),
                    }
                } else {
                    Value::String(text.to_string())
                };
                fields.insert(name, value);
            }
            "image" | "image[]" | "images" | "images[]" => {
                if let Ok(value) = serde_json::from_slice::<Value>(&value) {
                    match value {
                        Value::Array(values) => images.extend(values),
                        value => images.push(value),
                    }
                }
            }
            "mask" => {
                if let Ok(Value::Object(reference)) = serde_json::from_slice(&value) {
                    if !mask.is_empty() {
                        return Err(anyhow!("duplicate image mask reference"));
                    }
                    mask = reference;
                }
            }
            "mask[file_id]" | "mask[image_url]" => {
                let key = name.trim_start_matches("mask[").trim_end_matches(']');
                if mask.insert(key.to_string(), json!(text)).is_some() {
                    return Err(anyhow!("duplicate image mask reference"));
                }
            }
            _ => unreachable!("routing fields are filtered above"),
        }
    }
    if fields
        .get("model")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        return Err(anyhow!("media request requires a non-empty model field"));
    }
    fields.insert("images".into(), Value::Array(images));
    fields.insert("mask".into(), Value::Object(mask));
    Ok(Bytes::from(serde_json::to_vec(&fields)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn media_response_checks_known_length_and_cumulative_chunk_bytes() {
        let known = reqwest::Response::from(http::Response::new("abcde"));
        assert_eq!(known.content_length(), Some(5));
        let error = read_response_with_limit(known, 4).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "response exceeds the gateway message limit"
        );

        let streamed = |tail: &'static [u8]| {
            let chunks = [
                Ok::<_, std::io::Error>(Bytes::from_static(b"ab")),
                Ok(Bytes::from_static(tail)),
            ];
            reqwest::Response::from(http::Response::new(reqwest::Body::wrap_stream(
                futures_util::stream::iter(chunks),
            )))
        };
        let exact = streamed(b"cd");
        assert_eq!(exact.content_length(), None);
        assert_eq!(read_response_with_limit(exact, 4).await.unwrap(), b"abcd");
        let error = read_response_with_limit(streamed(b"cde"), 4)
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "response exceeds the gateway message limit"
        );
    }

    #[tokio::test]
    async fn audio_metadata_after_file_preserves_native_upload_and_rejects_duplicate_model() {
        let prefix = "--image-test\r\nContent-Disposition: form-data; name=\"file\"; filename=\"voice.wav\"\r\nContent-Type: audio/wav\r\n\r\n";
        let mut body = prefix.as_bytes().to_vec();
        body.extend_from_slice(b"RIFF\0\xff\r\n");
        body.extend_from_slice(
            (part("model", "whisper-1") + &part("response_format", "json") + "--image-test--\r\n")
                .as_bytes(),
        );
        let body = Bytes::from(body);
        let original = body.clone();
        let metadata = media_routing_body(
            crate::surface::ApiOperation::AudioTranscriptions,
            &headers(),
            &body,
        )
        .await
        .unwrap();
        let metadata: Value = serde_json::from_slice(&metadata).unwrap();
        assert_eq!(metadata["model"], "whisper-1");
        assert_eq!(metadata["response_format"], "json");
        assert_eq!(body, original);
        let duplicate =
            Bytes::from(part("model", "one") + &part("model", "two") + "--image-test--\r\n");
        assert!(
            media_routing_body(
                crate::surface::ApiOperation::AudioTranscriptions,
                &headers(),
                &duplicate
            )
            .await
            .is_err()
        );
    }

    fn part(name: &str, value: &str) -> String {
        format!(
            "--image-test\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
        )
    }

    fn headers() -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "content-type",
            "multipart/form-data; boundary=image-test".parse().unwrap(),
        );
        headers
    }

    #[tokio::test]
    async fn image_upload_routing_excludes_binary_and_prompt_without_rewriting() {
        let mut body = (part("model", "gpt-image-2")
            + &part("stream", "true")
            + &part("mask[file_id]", "file-mask")
            + &part("images", r#"[{"file_id":"file-reference"}]"#)
            + &part("prompt", &"private prompt".repeat(6000)))
            .into_bytes();
        body.extend_from_slice(b"--image-test\r\nContent-Disposition: form-data; name=\"image[]\"; filename=\"image.png\"\r\nContent-Type: image/png\r\n\r\n\0\xffprivate-image\r\n--image-test--\r\n");
        let body = Bytes::from(body);
        let original = body.clone();
        let routing =
            media_routing_body(crate::surface::ApiOperation::ImagesEdits, &headers(), &body)
                .await
                .unwrap();
        let routing: Value = serde_json::from_slice(&routing).unwrap();
        assert_eq!(body, original);
        assert_eq!(routing["model"], "gpt-image-2");
        assert_eq!(routing["stream"], true);
        assert_eq!(routing["images"][0]["file_id"], "file-reference");
        assert_eq!(routing["mask"]["file_id"], "file-mask");
        assert!(!routing.to_string().contains("private"));
    }

    #[tokio::test]
    async fn image_upload_routing_rejects_ambiguous_fields() {
        for body in [
            part("model", "one") + &part("model", "two"),
            part("model", "one") + &part("stream", "yes"),
            part("model", ""),
            part("model", &"x".repeat(MAX_ROUTING_FIELD_BYTES + 1)),
            part("model", "one") + &part("mask[file_id]", "one") + &part("mask[file_id]", "two"),
        ] {
            let body = Bytes::from(body + "--image-test--\r\n");
            assert!(
                media_routing_body(crate::surface::ApiOperation::ImagesEdits, &headers(), &body)
                    .await
                    .is_err()
            );
        }
    }
}
