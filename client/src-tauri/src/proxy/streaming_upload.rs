const OPENAI_FILE_UPLOAD_BODY_LIMIT: u64 = 513 * 1024 * 1024;
const ANTHROPIC_FILE_UPLOAD_BODY_LIMIT: u64 = 501 * 1024 * 1024;
const GEMINI_FILE_UPLOAD_BODY_LIMIT: u64 = 2 * 1024 * 1024 * 1024 + 1024 * 1024;
const OPENAI_UPLOAD_PART_BODY_LIMIT: u64 = 65 * 1024 * 1024;
const OPENAI_IMAGE_MULTIPART_BODY_LIMIT: u64 = 51 * 1024 * 1024;
const OPENAI_AUDIO_MULTIPART_BODY_LIMIT: u64 = 26 * 1024 * 1024;
const OPENAI_PROVENANCE_MULTIPART_BODY_LIMIT: u64 = 513 * 1024 * 1024;
const OPENAI_VIDEO_MULTIPART_BODY_LIMIT: u64 = 4 * 1024 * 1024 * 1024;

pub(crate) struct StreamingUploadRequest {
    method: warp::http::Method,
    path: warp::path::FullPath,
    route: crate::surface::ApiRoute,
    body_limit: u64,
}

#[derive(Debug)]
struct StreamingUploadLimitExceeded;

impl std::fmt::Display for StreamingUploadLimitExceeded {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("streaming upload body exceeded the operation limit")
    }
}

impl std::error::Error for StreamingUploadLimitExceeded {}

pub(crate) async fn match_streaming_upload_request(
    method: warp::http::Method,
    path: warp::path::FullPath,
) -> std::result::Result<StreamingUploadRequest, warp::Rejection> {
    let Some(route) = crate::surface::resolve_api_route(method.as_str(), path.as_str(), false)
    else {
        return Err(warp::reject::not_found());
    };
    let body_limit = if method == warp::http::Method::POST {
        streaming_upload_body_limit(&route, path.as_str())
    } else {
        0
    };
    if body_limit == 0 {
        return Err(warp::reject::not_found());
    }
    Ok(StreamingUploadRequest {
        method,
        path,
        route,
        body_limit,
    })
}

pub(crate) async fn proxy_streaming_upload_request<S, B>(
    request: StreamingUploadRequest,
    raw_query: Option<String>,
    headers: warp::http::HeaderMap,
    body: S,
    shared: Arc<ProxyShared>,
) -> Result<warp::reply::Response, Infallible>
where
    S: Stream<Item = std::result::Result<B, warp::Error>> + Send + 'static,
    B: bytes::Buf + Send + 'static,
{
    let path = request.path.as_str();
    let has_query = raw_query.is_some();
    let raw_query = raw_query.unwrap_or_default();
    let config = shared.config_snapshot();
    if !local_proxy_request_authorized(path, &raw_query, &headers, &config) {
        return Ok(local_surface_error_response(
            request.route.surface,
            StatusCode::UNAUTHORIZED,
            "unauthorized",
        )
        .unwrap_or_else(|_| error_response(StatusCode::UNAUTHORIZED, "unauthorized")));
    }
    let limit = request.body_limit;
    if let Some(content_length) = request_content_length(&headers) {
        match content_length {
            Ok(content_length) if content_length <= limit => {}
            Ok(_) => {
                return Ok(local_surface_error_response(
                    request.route.surface,
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "upload body exceeds the operation limit",
                )
                .unwrap_or_else(|_| {
                    error_response(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "upload body exceeds the operation limit",
                    )
                }));
            }
            Err(()) => {
                return Ok(local_surface_error_response(
                    request.route.surface,
                    StatusCode::BAD_REQUEST,
                    "invalid content-length header",
                )
                .unwrap_or_else(|_| {
                    error_response(StatusCode::BAD_REQUEST, "invalid content-length header")
                }));
            }
        }
    }
    let Some(update_activity_guard) = crate::update_activity::try_begin_proxy_request() else {
        let mut response = local_surface_error_response(
            request.route.surface,
            StatusCode::SERVICE_UNAVAILABLE,
            "client update is starting; retry in 2 seconds",
        )
        .unwrap_or_else(|_| {
            error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "client update is starting; retry in 2 seconds",
            )
        });
        response
            .headers_mut()
            .insert("retry-after", warp::http::HeaderValue::from_static("2"));
        return Ok(response);
    };

    let ready_channel_ids = shared.ready_local_channel_ids();
    let needs_audio_model_routing = matches!(
        request.route.operation,
        crate::surface::ApiOperation::AudioTranscriptions
            | crate::surface::ApiOperation::AudioTranslations
    ) && select_direct_http_surface_channel_where(
        &config,
        request.route.surface,
        path,
        &[],
        true,
        |channel| ready_channel_ids.contains(&channel.id),
    )
    .is_some_and(|first| {
        select_direct_http_surface_channel_where(
            &config,
            request.route.surface,
            path,
            &[],
            true,
            |channel| channel.id != first.id && ready_channel_ids.contains(&channel.id),
        )
        .is_some()
    });
    let counted = limit_streaming_upload(body, limit);
    let (request_body, selection_body) = if needs_audio_model_routing {
        // Audio forms contain a model, possibly after the file part. Inspect a
        // bounded copy before routing, then forward the original multipart bytes.
        // Single-channel audio, files and video retain unbuffered uploads.
        let mut counted = Box::pin(counted);
        let mut bytes = bytes::BytesMut::new();
        while let Some(chunk) = counted.next().await {
            match chunk {
                Ok(chunk) => bytes.extend_from_slice(&chunk),
                Err(error) => {
                    let status = if error
                        .get_ref()
                        .is_some_and(|cause| cause.is::<StreamingUploadLimitExceeded>())
                    {
                        StatusCode::PAYLOAD_TOO_LARGE
                    } else {
                        StatusCode::BAD_REQUEST
                    };
                    return Ok(error_response(
                        status,
                        "audio upload could not be read within the operation limit",
                    ));
                }
            }
        }
        let bytes = bytes.freeze();
        let model = match audio_upload_model(&headers, bytes.clone()).await {
            Ok(model) => model,
            Err(error) => {
                return Ok(error_response(StatusCode::BAD_REQUEST, &error.to_string()));
            }
        };
        let selection = model
            .map(|model| {
                serde_json::to_vec(&serde_json::json!({"model": model})).unwrap_or_default()
            })
            .unwrap_or_default();
        (reqwest::Body::from(bytes), selection)
    } else {
        (reqwest::Body::wrap_stream(counted), Vec::new())
    };

    let selected = match select_resource_owned_local_channel(
        &shared.local_resource_owners,
        &config,
        &request.route,
        path,
        &[],
        &ready_channel_ids,
    ) {
        Ok(Some(selection)) => Some(selection.channel),
        Ok(None) => select_direct_http_surface_channel_where(
            &config,
            request.route.surface,
            path,
            &selection_body,
            true,
            |channel| ready_channel_ids.contains(&channel.id),
        ),
        Err(error) => {
            return Ok(local_surface_error_response(
                error.surface,
                error.status,
                error.message,
            )
            .unwrap_or_else(|_| error_response(error.status, error.message)));
        }
    };
    let Some(channel) = selected.cloned() else {
        return Ok(local_surface_error_response(
            request.route.surface,
            StatusCode::UNPROCESSABLE_ENTITY,
            "this operation requires a ready verified native API-credential channel",
        )
        .unwrap_or_else(|_| {
            error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "this operation requires a ready verified native API-credential channel",
            )
        }));
    };

    let reqwest_method =
        match reqwest::Method::from_bytes(request.method.as_str().as_bytes()) {
            Ok(method) => method,
            Err(_) => {
                return Ok(error_response(
                    StatusCode::METHOD_NOT_ALLOWED,
                    "invalid upload method",
                ));
            }
        };
    let mut upstream_headers = reqwest::header::HeaderMap::new();
    for (name, value) in &headers {
        if let (Ok(name), Ok(value)) = (
            reqwest::header::HeaderName::from_bytes(name.as_str().as_bytes()),
            reqwest::header::HeaderValue::from_bytes(value.as_bytes()),
        ) {
            upstream_headers.append(name, value);
        }
    }
    let channel_id = channel.id.clone();
    let result = crate::channel_executor::execute_native_streaming_channel_request(
        &shared.client,
        &channel,
        reqwest_method,
        path,
        &raw_query,
        has_query,
        upstream_headers,
        request_body,
    )
    .await;
    let mut transport_failure = false;
    let response = match result {
        Ok(mut response) => {
            if let Err(error) = register_gemini_upload_session_response(
                &shared.gemini_upload_sessions,
                &request.route,
                &channel,
                &headers,
                &config,
                &mut response,
            ) {
                return Ok(error_response(StatusCode::BAD_GATEWAY, &error.to_string()));
            }
            let response = match response_from_reqwest(response, false).await {
                Ok(response) => response,
                Err(error) => {
                    return Ok(error_response(StatusCode::BAD_GATEWAY, &error.to_string()));
                }
            };
            match capture_local_resource_owner_response(
                shared.local_resource_owners.clone(),
                request.route,
                request.method.as_str(),
                path,
                &channel_id,
                "",
                response,
            )
            .await
            {
                Ok(response) => response,
                Err(error) => error_response(StatusCode::BAD_GATEWAY, &error.to_string()),
            }
        }
        Err(error)
            if error
                .chain()
                .any(|cause| cause.is::<StreamingUploadLimitExceeded>()) =>
        {
            local_surface_error_response(
                request.route.surface,
                StatusCode::PAYLOAD_TOO_LARGE,
                "upload body exceeds the operation limit",
            )
            .unwrap_or_else(|_| {
                error_response(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "upload body exceeds the operation limit",
                )
            })
        }
        Err(error) => {
            transport_failure = local_execution_error_marks_channel_unready(&error);
            error_response(StatusCode::BAD_GATEWAY, &error.to_string())
        }
    };
    let channel_scoped = transport_failure
        || local_response_marks_channel_unready_for_request(
            request.method.as_str(),
            path,
            response.status(),
        );
    if let Some(ready) =
        local_channel_readiness_observation(response.status(), false, channel_scoped)
    {
        shared.set_local_channel_ready(&channel_id, ready);
    }
    Ok(hold_proxy_update_activity_until_body_end(
        response,
        update_activity_guard,
    ))
}

fn request_content_length(
    headers: &warp::http::HeaderMap,
) -> Option<std::result::Result<u64, ()>> {
    headers.get(warp::http::header::CONTENT_LENGTH).map(|value| {
        value
            .to_str()
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or(())
    })
}

async fn audio_upload_model(
    headers: &warp::http::HeaderMap,
    body: bytes::Bytes,
) -> Result<Option<String>> {
    let content_type = headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !content_type.to_ascii_lowercase().starts_with("multipart/") {
        // Some compatible providers accept JSON audio or file URLs. Keep their
        // existing passthrough behavior; extracting a hint is not schema validation.
        #[derive(serde::Deserialize)]
        struct ModelHint {
            model: Option<String>,
        }
        return Ok(serde_json::from_slice::<ModelHint>(&body)
            .ok()
            .and_then(|hint| hint.model)
            .filter(|model| !model.trim().is_empty()));
    }
    let boundary = multer::parse_boundary(content_type)
        .map_err(|_| anyhow!("audio upload requires a multipart/form-data boundary"))?;
    let stream = futures_util::stream::once(async move { Ok::<_, std::io::Error>(body) });
    let mut multipart = multer::Multipart::new(stream, boundary);
    let mut model: Option<String> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| anyhow!("invalid audio multipart form"))?
    {
        if field.name() != Some("model") || field.file_name().is_some() {
            continue;
        }
        let bytes = field
            .bytes()
            .await
            .map_err(|_| anyhow!("invalid audio model field"))?;
        if bytes.len() > 1024 {
            return Err(anyhow!("audio model field is too long"));
        }
        let value = std::str::from_utf8(&bytes)
            .map_err(|_| anyhow!("audio model field must be UTF-8"))?
            .trim();
        if model.as_deref().is_some_and(|previous| previous != value) {
            return Err(anyhow!("audio upload contains conflicting model fields"));
        }
        model = Some(value.to_string());
    }
    Ok(model.filter(|model| !model.is_empty()))
}

fn streaming_upload_body_limit(route: &crate::surface::ApiRoute, path: &str) -> u64 {
    match route.operation {
        // The multipart envelope needs a small allowance beyond the official
        // 512 MiB file limit. The upstream remains authoritative for the file
        // field itself.
        crate::surface::ApiOperation::FilesCreate => match route.surface {
            crate::surface::ApiSurface::OpenAi => OPENAI_FILE_UPLOAD_BODY_LIMIT,
            crate::surface::ApiSurface::Anthropic => ANTHROPIC_FILE_UPLOAD_BODY_LIMIT,
            crate::surface::ApiSurface::Gemini => GEMINI_FILE_UPLOAD_BODY_LIMIT,
        },
        crate::surface::ApiOperation::UploadPartsCreate => OPENAI_UPLOAD_PART_BODY_LIMIT,
        crate::surface::ApiOperation::ImagesVariations => OPENAI_IMAGE_MULTIPART_BODY_LIMIT,
        crate::surface::ApiOperation::AudioTranscriptions
        | crate::surface::ApiOperation::AudioTranslations => {
            OPENAI_AUDIO_MULTIPART_BODY_LIMIT
        }
        crate::surface::ApiOperation::ContentProvenanceChecks => {
            OPENAI_PROVENANCE_MULTIPART_BODY_LIMIT
        }
        crate::surface::ApiOperation::VideosCreate
        | crate::surface::ApiOperation::VideosEdit
        | crate::surface::ApiOperation::VideosExtend => OPENAI_VIDEO_MULTIPART_BODY_LIMIT,
        _ => match crate::surface::native_opaque_streaming_upload_family(path) {
            Some("openai_audio") => OPENAI_PROVENANCE_MULTIPART_BODY_LIMIT,
            Some("openai_resource") => OPENAI_FILE_UPLOAD_BODY_LIMIT,
            Some("anthropic_resource") => ANTHROPIC_FILE_UPLOAD_BODY_LIMIT,
            Some("gemini_file_search") => GEMINI_FILE_UPLOAD_BODY_LIMIT,
            _ => 0,
        },
    }
}

fn limit_streaming_upload<S, B>(
    body: S,
    limit: u64,
) -> impl Stream<Item = std::result::Result<bytes::Bytes, std::io::Error>>
where
    S: Stream<Item = std::result::Result<B, warp::Error>>,
    B: bytes::Buf,
{
    let seen = Arc::new(std::sync::atomic::AtomicU64::new(0));
    body.map({
        let seen = seen.clone();
        move |chunk| {
            let mut chunk = chunk.map_err(std::io::Error::other)?;
            let length = chunk.remaining() as u64;
            let prior = seen.fetch_add(length, std::sync::atomic::Ordering::Relaxed);
            if prior.saturating_add(length) > limit {
                return Err(std::io::Error::other(StreamingUploadLimitExceeded));
            }
            Ok(chunk.copy_to_bytes(length as usize))
        }
    })
}

#[cfg(test)]
mod streaming_upload_tests {
    use super::*;

    #[tokio::test]
    async fn audio_upload_model_can_follow_binary_file_without_rewriting_it() {
        let mut headers = warp::http::HeaderMap::new();
        headers.insert("content-type", "multipart/form-data; boundary=audio-test".parse().unwrap());
        let body = bytes::Bytes::from_static(
            b"--audio-test\r\nContent-Disposition: form-data; name=\"file\"; filename=\"test.wav\"\r\nContent-Type: audio/wav\r\n\r\nRIFF\0\xff\r\n--audio-test\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nwhisper-test\r\n--audio-test--\r\n",
        );
        let original = body.clone();
        assert_eq!(audio_upload_model(&headers, body.clone()).await.unwrap().as_deref(), Some("whisper-test"));
        assert_eq!(body, original);
    }

    #[tokio::test]
    async fn audio_upload_rejects_conflicting_models() {
        let mut headers = warp::http::HeaderMap::new();
        headers.insert("content-type", "multipart/form-data; boundary=audio-test".parse().unwrap());
        let body = bytes::Bytes::from_static(
            b"--audio-test\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nfirst\r\n--audio-test\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nsecond\r\n--audio-test--\r\n",
        );
        assert!(audio_upload_model(&headers, body).await.unwrap_err().to_string().contains("conflicting"));
    }

    #[tokio::test]
    async fn audio_upload_keeps_compatible_json_and_unknown_media_types() {
        let mut headers = warp::http::HeaderMap::new();
        headers.insert("content-type", "application/json".parse().unwrap());
        assert_eq!(
            audio_upload_model(&headers, bytes::Bytes::from_static(
                br#"{"model":"audio-custom","file_url":"https://example.test/voice.wav"}"#,
            )).await.unwrap().as_deref(),
            Some("audio-custom"),
        );
        headers.insert("content-type", "audio/wav".parse().unwrap());
        assert!(audio_upload_model(&headers, bytes::Bytes::from_static(b"RIFF\0\xff"))
            .await.unwrap().is_none());
    }

    #[tokio::test]
    async fn chunked_upload_limit_is_enforced_without_aggregation() {
        let body = futures_util::stream::iter([
            Ok::<bytes::Bytes, warp::Error>(bytes::Bytes::from_static(b"abc")),
            Ok(bytes::Bytes::from_static(b"de")),
        ]);
        let mut limited = Box::pin(limit_streaming_upload(body, 4));
        assert_eq!(
            limited.next().await.expect("first chunk").expect("allowed"),
            bytes::Bytes::from_static(b"abc")
        );
        let error = limited
            .next()
            .await
            .expect("limit error")
            .expect_err("second chunk must exceed the limit");
        assert!(error
            .get_ref()
            .is_some_and(|error| error.is::<StreamingUploadLimitExceeded>()));
    }

    #[test]
    fn only_contract_streaming_operations_are_admitted() {
        assert!(crate::surface::api_operation_uses_streaming_upload(
            crate::surface::ApiOperation::FilesCreate
        ));
        assert!(crate::surface::api_operation_uses_streaming_upload(
            crate::surface::ApiOperation::ContentProvenanceChecks
        ));
        assert!(!crate::surface::api_operation_uses_streaming_upload(
            crate::surface::ApiOperation::Responses
        ));
        assert_eq!(
            streaming_upload_body_limit(
                &crate::surface::resolve_api_route("POST", "/v1/skills", false).unwrap(),
                "/v1/skills",
            ),
            OPENAI_FILE_UPLOAD_BODY_LIMIT
        );
        assert_eq!(
            streaming_upload_body_limit(
                &crate::surface::resolve_api_route(
                    "POST",
                    "/gemini/upload/v1beta/fileSearchStores/store_1:uploadToFileSearchStore",
                    false,
                )
                .unwrap(),
                "/gemini/upload/v1beta/fileSearchStores/store_1:uploadToFileSearchStore",
            ),
            GEMINI_FILE_UPLOAD_BODY_LIMIT
        );
    }
}
