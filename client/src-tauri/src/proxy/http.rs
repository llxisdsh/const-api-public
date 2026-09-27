const PLATFORM_BUFFERED_REQUEST_TIMEOUT: std::time::Duration = crate::MODEL_REQUEST_TIMEOUT;
const PLATFORM_STREAM_RESUME_WINDOW: std::time::Duration = std::time::Duration::from_secs(120);
const PLATFORM_STREAM_RESUME_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(2);
const PLATFORM_STREAM_ID_HEADER: &str = "x-const-stream-id";
const PLATFORM_STREAM_OFFSET_HEADER: &str = "x-const-stream-resume-offset";
const PLATFORM_STREAM_RESUMABLE_HEADER: &str = "x-const-stream-resumable";
const PLATFORM_STREAM_RESUMED_HEADER: &str = "x-const-stream-resumed";
const PLATFORM_MODEL_ROUTE_MODE_HEADER: &str = "x-const-api-model-route-mode";
const PLATFORM_MODEL_ROUTE_MODE_APPLIED_HEADER: &str = "x-const-api-model-route-mode";
const PLATFORM_MODEL_FALLBACK_SAFE_HEADER: &str = "x-const-api-fallback-safe";
const PLATFORM_MODEL_FALLBACK_SCOPE_HEADER: &str = "x-const-api-fallback-scope";
const PLATFORM_MODEL_FALLBACK_SCOPE_LOCAL_ONLY: &str = "local-only";
const PLATFORM_ERROR_CODE_HEADER: &str = "x-const-error-code";
const PLATFORM_UPSTREAM_ATTEMPT_BUDGET_HEADER: &str =
    "x-const-api-upstream-attempt-budget";
const PLATFORM_UPSTREAM_ATTEMPTS_USED_HEADER: &str =
    "x-const-api-upstream-attempts-used";
const PLATFORM_REQUEST_COMPRESSION_MIN_BYTES: usize = 1 << 10;
const PLATFORM_REQUEST_ZSTD_LEVEL: i32 = 5;
const PLATFORM_REQUEST_COMPRESSION_MIN_SAVING: usize = 32;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum PlatformModelRouteMode {
    #[default]
    Configured,
    ExactOnly,
    CompatibleOnly,
}

impl PlatformModelRouteMode {
    fn header_value(self) -> Option<&'static str> {
        match self {
            Self::Configured => None,
            Self::ExactOnly => Some("exact-only"),
            Self::CompatibleOnly => Some("compatible-only"),
        }
    }
}

#[derive(Clone)]
struct PlatformResumableRequest {
    client: Client,
    method: reqwest::Method,
    url: String,
    headers: reqwest::header::HeaderMap,
    body: bytes::Bytes,
    request_version: Option<reqwest::Version>,
}

impl PlatformResumableRequest {
    async fn send(&self, resume_offset: Option<u64>) -> Result<reqwest::Response> { anyhow::bail!("Hosted platform access is unavailable in the local edition") }
}

#[derive(Debug)]
struct AmbiguousHttp3RequestError(String);

impl std::fmt::Display for AmbiguousHttp3RequestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for AmbiguousHttp3RequestError {}

fn ambiguous_http3_request_error(error: anyhow::Error) -> anyhow::Error {
    anyhow::Error::new(AmbiguousHttp3RequestError(format!(
        "HTTP/3 request outcome is unknown; refusing automatic replay: {error:#}"
    )))
}

fn is_ambiguous_http3_request_error(error: &anyhow::Error) -> bool {
    error.downcast_ref::<AmbiguousHttp3RequestError>().is_some()
}

#[cfg(test)]
pub(crate) async fn forward_once(
    method: warp::http::Method,
    path: &str,
    raw_query: &str,
    has_query: bool,
    headers: warp::http::HeaderMap,
    body: bytes::Bytes,
    client: &Client,
    cfg: &ClientConfig,
    endpoint: &Endpoint,
    platform_api_key: &str,
) -> Result<warp::reply::Response> {
    forward_once_to_base(
        method,
        path,
        raw_query,
        has_query,
        headers,
        body,
        client,
        cfg,
        &endpoint.base_url,
        None,
        platform_api_key,
        PlatformModelRouteMode::Configured,
        MODEL_REQUEST_MAX_UPSTREAM_ATTEMPTS,
    )
    .await
    .map(|(response, _)| response)
}

pub(crate) async fn forward_once_preferred(
    method: warp::http::Method,
    path: &str,
    raw_query: &str,
    has_query: bool,
    headers: warp::http::HeaderMap,
    body: bytes::Bytes,
    transport: &Arc<crate::platform_transport::PlatformHttpTransport>,
    cfg: &ClientConfig,
    endpoint: &Endpoint,
    platform_api_key: &str,
    model_route_mode: PlatformModelRouteMode,
    upstream_attempt_budget: usize,
) -> Result<warp::reply::Response> {
    let stream_requested = platform_request_is_stream(path, raw_query, &body);
    transport.start_probe(endpoint.clone());
    if let Some((http3_client, http3_base_url)) = transport.http3_target(endpoint) {
        match forward_once_to_base(
            method.clone(),
            path,
            raw_query,
            has_query,
            headers.clone(),
            body.clone(),
            &http3_client,
            cfg,
            &http3_base_url,
            Some(reqwest::Version::HTTP_3),
            platform_api_key,
            model_route_mode,
            upstream_attempt_budget,
        )
        .await
        {
            Ok((response, version)) => {
                transport.mark_response_version(endpoint, version);
                return Ok(response);
            }
            Err(error) => {
                transport.mark_http3_request_failed(endpoint, &error);
                if !http3_error_is_safe_to_fallback(&method, &error) {
                    return Err(ambiguous_http3_request_error(error));
                }
            }
        }
    }

    let result = forward_once_to_base(
        method,
        path,
        raw_query,
        has_query,
        headers,
        body,
        transport.tcp_client(stream_requested),
        cfg,
        &endpoint.base_url,
        None,
        platform_api_key,
        model_route_mode,
        upstream_attempt_budget,
    )
    .await;
    match result {
        Ok((response, version)) => {
            transport.mark_response_version(endpoint, version);
            Ok(response)
        }
        Err(error) => {
            transport.mark_tcp_request_failed(endpoint, &error);
            Err(error)
        }
    }
}

async fn forward_once_to_base(
    method: warp::http::Method,
    path: &str,
    raw_query: &str,
    has_query: bool,
    headers: warp::http::HeaderMap,
    body: bytes::Bytes,
    client: &Client,
    cfg: &ClientConfig,
    base_url: &str,
    request_version: Option<reqwest::Version>,
    platform_api_key: &str,
    model_route_mode: PlatformModelRouteMode,
    upstream_attempt_budget: usize,
) -> Result<(warp::reply::Response, reqwest::Version)> { anyhow::bail!("Hosted platform access is unavailable in the local edition") }

fn platform_request_headers(
    headers: &warp::http::HeaderMap,
    cfg: &ClientConfig,
    platform_api_key: &str,
    model_route_mode: PlatformModelRouteMode,
    upstream_attempt_budget: usize,
) -> Result<reqwest::header::HeaderMap> {
    let mut forwarded_headers = reqwest::header::HeaderMap::new();
    for (key, value) in headers.iter() {
        let name = reqwest::header::HeaderName::from_bytes(key.as_str().as_bytes())?;
        let value = reqwest::header::HeaderValue::from_bytes(value.as_bytes())?;
        forwarded_headers.append(name, value);
    }
    crate::channel_executor::strip_http_request_headers(&mut forwarded_headers);
    forwarded_headers.remove(reqwest::header::ACCEPT_ENCODING);
    forwarded_headers.remove(PLATFORM_STREAM_ID_HEADER);
    forwarded_headers.remove(PLATFORM_STREAM_OFFSET_HEADER);
    // These are client-owned policy headers. Never let a local caller bypass
    // the configured compatibility switch by forwarding its own values.
    forwarded_headers.remove("x-const-api-allow-model-substitution");
    forwarded_headers.remove("x-const-api-allow-model-equivalence");
    forwarded_headers.remove(PLATFORM_MODEL_ROUTE_MODE_HEADER);
    forwarded_headers.remove(PLATFORM_UPSTREAM_ATTEMPT_BUDGET_HEADER);
    forwarded_headers.remove(MAX_PRICE_RATIO_HEADER);
    forwarded_headers.remove(ALLOW_UNVERIFIED_PLATFORM_ROUTES_HEADER);
    // The desktop language owns CONST API's own actionable errors. Override a
    // tool's locale here so the platform can return guidance that matches the
    // language the user selected in the client.
    forwarded_headers.insert(
        reqwest::header::ACCEPT_LANGUAGE,
        reqwest::header::HeaderValue::from_static(crate::native_i18n::display_language_tag()),
    );
    if !platform_api_key.trim().is_empty() {
        forwarded_headers.insert(
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_str(&format!("Bearer {}", platform_api_key.trim()))?,
        );
    }
    let allow_model_equivalence = match model_route_mode {
        PlatformModelRouteMode::Configured => cfg.allow_model_equivalence,
        PlatformModelRouteMode::ExactOnly => false,
        PlatformModelRouteMode::CompatibleOnly => cfg.allow_model_equivalence,
    };
    if let Some(value) = model_route_mode.header_value() {
        forwarded_headers.insert(
            reqwest::header::HeaderName::from_static(PLATFORM_MODEL_ROUTE_MODE_HEADER),
            reqwest::header::HeaderValue::from_static(value),
        );
    }
    if allow_model_equivalence {
        forwarded_headers.insert(
            reqwest::header::HeaderName::from_static("x-const-api-allow-model-substitution"),
            reqwest::header::HeaderValue::from_static("1"),
        );
        forwarded_headers.insert(
            reqwest::header::HeaderName::from_static("x-const-api-allow-model-equivalence"),
            reqwest::header::HeaderValue::from_static("1"),
        );
    }
    // Older servers still require this opt-in header. Current servers apply
    // source priority themselves and ignore the retired preference.
    forwarded_headers.insert(
        reqwest::header::HeaderName::from_static(ALLOW_UNVERIFIED_PLATFORM_ROUTES_HEADER),
        reqwest::header::HeaderValue::from_static("1"),
    );
    forwarded_headers.insert(
        reqwest::header::HeaderName::from_static(PLATFORM_UPSTREAM_ATTEMPT_BUDGET_HEADER),
        reqwest::header::HeaderValue::from_str(
            &upstream_attempt_budget
                .clamp(1, MODEL_REQUEST_MAX_UPSTREAM_ATTEMPTS)
                .to_string(),
        )?,
    );
    Ok(forwarded_headers)
}

fn compress_platform_request_body(
    headers: &mut reqwest::header::HeaderMap,
    body: bytes::Bytes,
) -> Result<bytes::Bytes> {
    if body.len() < PLATFORM_REQUEST_COMPRESSION_MIN_BYTES
        || !platform_request_body_is_json(headers, &body)
        || headers
            .get(reqwest::header::CONTENT_ENCODING)
            .is_some_and(|value| {
                !value
                    .to_str()
                    .unwrap_or_default()
                    .trim()
                    .eq_ignore_ascii_case("identity")
            })
    {
        return Ok(body);
    }

    let compressed = zstd::bulk::compress(&body, PLATFORM_REQUEST_ZSTD_LEVEL)?;
    if compressed
        .len()
        .saturating_add(PLATFORM_REQUEST_COMPRESSION_MIN_SAVING)
        >= body.len()
    {
        return Ok(body);
    }
    headers.insert(
        reqwest::header::CONTENT_ENCODING,
        reqwest::header::HeaderValue::from_static("zstd"),
    );
    headers.remove(reqwest::header::CONTENT_LENGTH);
    Ok(bytes::Bytes::from(compressed))
}

fn platform_request_body_is_json(headers: &reqwest::header::HeaderMap, body: &[u8]) -> bool {
    if let Some(content_type) = headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
    {
        let media_type = content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        return media_type == "application/json" || media_type.ends_with("+json");
    }
    matches!(
        body.iter().copied().find(|byte| !byte.is_ascii_whitespace()),
        Some(b'{') | Some(b'[')
    )
}

fn platform_request_timeout(stream_requested: bool) -> Option<std::time::Duration> {
    (!stream_requested).then_some(PLATFORM_BUFFERED_REQUEST_TIMEOUT)
}

fn platform_request_is_stream(path: &str, raw_query: &str, body: &[u8]) -> bool {
    crate::supplier::top_level_json_bool(body, "stream").unwrap_or(false)
        || path.contains(":streamGenerateContent")
        || raw_query.split('&').any(|part| {
            part.split_once('=').is_some_and(|(key, value)| {
                key.eq_ignore_ascii_case("alt") && value.eq_ignore_ascii_case("sse")
            })
        })
}

fn http3_error_is_safe_to_fallback(method: &warp::http::Method, error: &anyhow::Error) -> bool {
    matches!(
        *method,
        warp::http::Method::GET | warp::http::Method::HEAD | warp::http::Method::OPTIONS
    ) || error
        .downcast_ref::<reqwest::Error>()
        .is_some_and(reqwest::Error::is_connect)
}

pub(crate) async fn response_from_reqwest(
    resp: reqwest::Response,
    stream_requested: bool,
) -> Result<warp::reply::Response> {
    response_from_reqwest_with_upstream_model(resp, stream_requested, None).await
}

pub(crate) async fn response_from_reqwest_with_upstream_model(
    resp: reqwest::Response,
    stream_requested: bool,
    upstream_model: Option<&str>,
) -> Result<warp::reply::Response> {
    response_from_platform_reqwest(resp, stream_requested, None, upstream_model).await
}

async fn response_from_platform_reqwest(
    resp: reqwest::Response,
    stream_requested: bool,
    resumable_request: Option<PlatformResumableRequest>,
    upstream_model: Option<&str>,
) -> Result<warp::reply::Response> {
    let status = warp::http::StatusCode::from_u16(resp.status().as_u16())?;
    let mut headers = resp.headers().clone();
    let resume_accepted = resumable_request
        .as_ref()
        .is_some_and(|request| platform_resume_headers_match(&headers, request, false));
    crate::channel_executor::strip_http_response_headers(&mut headers);
    strip_platform_resumability_headers(&mut headers);
    if !status.is_success() {
        // Error responses have not started at the caller yet, so buffer them
        // once to extract explicit model-scoped capacity evidence. This is the
        // retry boundary; successful SSE/binary responses remain fully
        // streaming and are never replayed after output begins.
        let content_type = headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("application/json")
            .to_string();
        let bytes = resp.bytes().await?;
        let raw = String::from_utf8_lossy(&bytes);
        let mut payload = subscription_http_response_payload(
            status.as_u16(),
            &content_type,
            raw.to_string(),
        );
        insert_subscription_route_model_failure_evidence(
            &mut payload,
            status.as_u16(),
            &raw,
            upstream_model,
        );
        let mut response = response_from_body(status, &headers, bytes)?;
        attach_local_model_failure_evidence(&mut response, &payload);
        return Ok(response);
    }
    if response_headers_are_event_stream(&headers) {
        if resume_accepted {
            if let Some(request) = resumable_request {
                return response_from_resumable_platform_event_stream(
                    status,
                    &headers,
                    resp.bytes_stream(),
                    request,
                );
            }
        }
        return response_from_platform_event_stream(status, &headers, resp.bytes_stream());
    }
    if stream_requested {
        return response_from_stream(
            status,
            &headers,
            buffered_platform_output(resp.bytes_stream()),
        );
    }
    let bytes = resp.bytes().await?;
    response_from_body(status, &headers, bytes)
}

fn response_headers_are_event_stream(headers: &reqwest::header::HeaderMap) -> bool {
    headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_ascii_lowercase().contains("text/event-stream"))
        .unwrap_or(false)
}

// Read ahead on the usage client, not on the server. Exact-resume stays inside
// the producer: its offset counts bytes retained here while the tool is slow.
// Dropping the tool response cancels even an idle read or recovery attempt;
// this buffer never re-executes a model request.
fn buffered_platform_output<S, E>(stream: S) -> impl Stream<Item = Result<bytes::Bytes, E>> + Send
where
    S: Stream<Item = Result<bytes::Bytes, E>> + Send + 'static,
    E: Send + 'static,
{
    let (sender, receiver) = crate::output_buffer::channel(Default::default());
    crate::spawn_logged("platform output read-ahead", async move {
        let producer = async {
            futures_util::pin_mut!(stream);
            while let Some(item) = stream.next().await {
                match item {
                    Ok(bytes) => {
                        let len = bytes.len();
                        if !sender.send(Ok(bytes), len).await {
                            break;
                        }
                    }
                    Err(error) => {
                        sender.finish(Err(error));
                        break;
                    }
                }
            }
        };
        tokio::select! {
            _ = sender.closed() => {}
            _ = producer => {}
        }
    });
    futures_util::stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|item| (item, receiver))
    })
}

pub(crate) fn response_from_stream<S, E>(
    status: warp::http::StatusCode,
    headers: &reqwest::header::HeaderMap,
    stream: S,
) -> Result<warp::reply::Response>
where
    S: Stream<Item = Result<bytes::Bytes, E>> + Send + 'static,
    E: Into<Box<dyn std::error::Error + Send + Sync>> + Send + 'static,
{
    let stream = ThreadSafeStream::new(stream);
    let body = warp::reply::stream(stream).into_response().into_body();
    response_builder_from_headers(status, headers)
        .body(body)
        .map_err(|err| anyhow!("build streaming response: {err}"))
}

pub(crate) fn response_from_body(
    status: warp::http::StatusCode,
    headers: &reqwest::header::HeaderMap,
    body: bytes::Bytes,
) -> Result<warp::reply::Response> {
    let mut headers = headers.clone();
    let content_length_matches = headers
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length == body.len());
    if !content_length_matches {
        headers.remove(reqwest::header::CONTENT_LENGTH);
    }
    response_builder_from_headers(status, &headers)
        .body(body.into())
        .map_err(|err| anyhow!("build response: {err}"))
}

fn strip_platform_resumability_headers(headers: &mut reqwest::header::HeaderMap) {
    headers.remove(PLATFORM_STREAM_ID_HEADER);
    headers.remove(PLATFORM_STREAM_OFFSET_HEADER);
    headers.remove(PLATFORM_STREAM_RESUMABLE_HEADER);
    headers.remove(PLATFORM_STREAM_RESUMED_HEADER);
}

fn response_from_platform_event_stream<S, E>(
    status: warp::http::StatusCode,
    headers: &reqwest::header::HeaderMap,
    stream: S,
) -> Result<warp::reply::Response>
where
    S: Stream<Item = Result<bytes::Bytes, E>> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    let stream = stream.scan(false, |failed, item| {
        let output = if *failed {
            None
        } else {
            match item {
                Ok(chunk) => Some(Ok::<bytes::Bytes, std::io::Error>(chunk)),
                Err(error) => {
                    *failed = true;
                    eprintln!(
                        "[const-api][platform] response stream interrupted after headers; returning terminal SSE error: {error}"
                    );
                    Some(Ok(bytes::Bytes::from_static(
                        b"event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"platform_stream_interrupted\",\"message\":\"platform response stream ended unexpectedly\"}}\n\n",
                    )))
                }
            }
        };
        futures_util::future::ready(output)
    });
    response_from_stream(status, headers, buffered_platform_output(stream))
}

fn response_from_resumable_platform_event_stream<S>(
    status: warp::http::StatusCode,
    headers: &reqwest::header::HeaderMap,
    stream: S,
    request: PlatformResumableRequest,
) -> Result<warp::reply::Response>
where
    S: Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
{
    struct ResumeState {
        stream: BoxStream<'static, Result<bytes::Bytes, reqwest::Error>>,
        request: PlatformResumableRequest,
        // Bytes handed to the local output buffer, not necessarily read by the
        // tool yet. The buffer must drain before any resumed suffix is emitted.
        delivered: u64,
        resume_deadline: Option<tokio::time::Instant>,
        finished: bool,
    }

    let state = ResumeState {
        stream: Box::pin(stream),
        request,
        delivered: 0,
        resume_deadline: None,
        finished: false,
    };
    let stream = futures_util::stream::unfold(state, |mut state| async move {
        if state.finished {
            return None;
        }
        loop {
            match state.stream.next().await {
                Some(Ok(chunk)) => {
                    state.delivered = state.delivered.saturating_add(chunk.len() as u64);
                    state.resume_deadline = None;
                    return Some((Ok::<bytes::Bytes, std::io::Error>(chunk), state));
                }
                Some(Err(error)) => {
                    let deadline = *state.resume_deadline.get_or_insert_with(|| {
                        eprintln!(
                            "[const-api][platform] response stream interrupted after {} bytes; attempting exact resume for up to {} seconds: {error}",
                            state.delivered,
                            PLATFORM_STREAM_RESUME_WINDOW.as_secs()
                        );
                        tokio::time::Instant::now() + PLATFORM_STREAM_RESUME_WINDOW
                    });
                    loop {
                        let now = tokio::time::Instant::now();
                        if now >= deadline {
                            state.finished = true;
                            eprintln!(
                                "[const-api][platform] exact stream resume expired after {} bytes",
                                state.delivered
                            );
                            return Some((Ok(platform_stream_interrupted_event()), state));
                        }
                        match tokio::time::timeout_at(
                            deadline,
                            state.request.send(Some(state.delivered)),
                        )
                        .await
                        {
                            Ok(Ok(response))
                                if response.status().is_success()
                                    && platform_resume_headers_match(
                                        response.headers(),
                                        &state.request,
                                        true,
                                    ) =>
                            {
                                eprintln!(
                                    "[const-api][platform] response stream resumed at byte {}",
                                    state.delivered
                                );
                                state.stream = Box::pin(response.bytes_stream());
                                state.resume_deadline = None;
                                break;
                            }
                            Ok(Ok(response)) => {
                                if let Some(reason) = platform_resume_rejection_reason(
                                    response.status(),
                                    response.headers(),
                                ) {
                                    state.finished = true;
                                    eprintln!(
                                        "[const-api][platform] exact stream resume stopped after {} bytes: HTTP {} code={reason}",
                                        state.delivered, response.status(),
                                    );
                                    return Some((Ok(platform_stream_resume_rejected_event(reason)), state));
                                }
                                eprintln!(
                                    "[const-api][platform] exact stream resume rejected with HTTP {}",
                                    response.status()
                                );
                            }
                            Ok(Err(error)) => {
                                eprintln!(
                                    "[const-api][platform] exact stream resume attempt failed: {error}"
                                );
                            }
                            Err(_) => {}
                        }
                        let remaining =
                            deadline.saturating_duration_since(tokio::time::Instant::now());
                        if remaining.is_zero() {
                            continue;
                        }
                        tokio::time::sleep(PLATFORM_STREAM_RESUME_RETRY_DELAY.min(remaining)).await;
                    }
                }
                None => return None,
            }
        }
    });
    response_from_stream(status, headers, buffered_platform_output(stream))
}

fn platform_resume_rejection_reason(
    status: reqwest::StatusCode,
    headers: &reqwest::header::HeaderMap,
) -> Option<&'static str> {
    // Exact replay cannot heal revoked access or missing/evicted replay state.
    // Unknown responses (including old servers) retain the bounded retry policy.
    match status.as_u16() {
        401 => Some("authentication_required"),
        403 => Some("access_denied"),
        400 | 409 => match headers.get(PLATFORM_ERROR_CODE_HEADER)?.to_str().ok()? {
            "stream_resume_unavailable" => Some("stream_resume_unavailable"),
            "stream_resume_offset_unavailable" => Some("stream_resume_offset_unavailable"),
            "invalid_stream_resume_offset" => Some("invalid_stream_resume_offset"),
            _ => None,
        },
        _ => None,
    }
}

fn platform_resume_headers_match(
    response_headers: &reqwest::header::HeaderMap,
    request: &PlatformResumableRequest,
    resumed: bool,
) -> bool {
    let Some(request_stream_id) = request.headers.get(PLATFORM_STREAM_ID_HEADER) else {
        return false;
    };
    response_headers
        .get(PLATFORM_STREAM_RESUMABLE_HEADER)
        .is_some_and(|value| value == "1")
        && response_headers
            .get(PLATFORM_STREAM_ID_HEADER)
            .is_some_and(|value| value == request_stream_id)
        && (!resumed
            || response_headers
                .get(PLATFORM_STREAM_RESUMED_HEADER)
                .is_some_and(|value| value == "1"))
}

fn platform_stream_interrupted_event() -> bytes::Bytes {
    bytes::Bytes::from_static(
        b"event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"platform_stream_interrupted\",\"message\":\"platform response stream ended unexpectedly\"}}\n\n",
    )
}

fn platform_stream_resume_rejected_event(reason: &str) -> bytes::Bytes {
    // Only the allow-listed reason above is forwarded, never a raw error body.
    // Keep the existing error type for clients that already recognize it.
    let data = serde_json::json!({
        "type": "error",
        "error": {
            "type": "platform_stream_interrupted",
            "code": reason,
            "message": format!("Platform response stream cannot be resumed: {reason}")
        }
    });
    bytes::Bytes::from(format!("event: error\ndata: {data}\n\n"))
}

fn response_builder_from_headers(
    status: warp::http::StatusCode,
    headers: &reqwest::header::HeaderMap,
) -> warp::http::response::Builder {
    let mut builder = warp::http::Response::builder().status(status);
    for (key, value) in headers.iter() {
        let Ok(name) = warp::http::header::HeaderName::from_bytes(key.as_str().as_bytes()) else {
            continue;
        };
        let Ok(value) = warp::http::HeaderValue::from_bytes(value.as_bytes()) else {
            continue;
        };
        builder = builder.header(name, value);
    }
    builder
}

pub(crate) fn error_response(status: StatusCode, message: &str) -> warp::reply::Response {
    warp::reply::with_status(
        warp::reply::json(&serde_json::json!({
            "error": {
                "message": message,
                "type": "client_proxy_error"
            }
        })),
        status,
    )
    .into_response()
}
