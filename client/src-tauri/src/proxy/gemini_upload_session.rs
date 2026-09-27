const GEMINI_UPLOAD_SESSION_PREFIX: &str = "/gemini/.upload-session/";
const GEMINI_UPLOAD_SESSION_TTL_SECONDS: i64 = 60 * 60;
const GEMINI_UPLOAD_SESSION_MAX_ENTRIES: usize = 1_024;
const GEMINI_UPLOAD_SESSION_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Clone, Debug)]
struct GeminiUploadSession {
    channel_id: String,
    upstream_url: String,
    expires_at_unix: i64,
    uploaded_bytes: u64,
    in_flight: bool,
}

#[derive(Debug)]
struct GeminiUploadSessionRegistry {
    sessions: std::sync::RwLock<HashMap<String, GeminiUploadSession>>,
}

impl GeminiUploadSessionRegistry {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            sessions: std::sync::RwLock::new(HashMap::new()),
        })
    }

    fn insert(&self, channel: &ChannelConfig, upstream_url: &str) -> Result<String> {
        validate_gemini_upload_session_url(channel, upstream_url)?;
        let token = hex::encode(crate::detection::random_bytes(24)?);
        let now = now_unix();
        let mut sessions = self
            .sessions
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        sessions.retain(|_, session| session.expires_at_unix > now);
        if sessions.len() >= GEMINI_UPLOAD_SESSION_MAX_ENTRIES {
            let oldest = sessions
                .iter()
                .min_by_key(|(_, session)| session.expires_at_unix)
                .map(|(token, _)| token.clone());
            if let Some(oldest) = oldest {
                sessions.remove(&oldest);
            }
        }
        sessions.insert(
            token.clone(),
            GeminiUploadSession {
                channel_id: channel.id.clone(),
                upstream_url: upstream_url.to_string(),
                expires_at_unix: now + GEMINI_UPLOAD_SESSION_TTL_SECONDS,
                uploaded_bytes: 0,
                in_flight: false,
            },
        );
        Ok(token)
    }

    fn begin(&self, token: &str, declared_length: Option<u64>) -> Result<GeminiUploadSession> {
        let now = now_unix();
        let mut sessions = self
            .sessions
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        sessions.retain(|_, session| session.expires_at_unix > now);
        let session = sessions
            .get_mut(token)
            .ok_or_else(|| anyhow!("Gemini upload session is missing or expired"))?;
        if session.in_flight {
            return Err(anyhow!(
                "Gemini upload session already has an in-flight request"
            ));
        }
        if declared_length.is_some_and(|length| {
            session.uploaded_bytes.saturating_add(length) > GEMINI_UPLOAD_SESSION_MAX_BYTES
        }) {
            return Err(anyhow!("Gemini upload exceeds the 2 GiB file limit"));
        }
        session.in_flight = true;
        Ok(session.clone())
    }

    fn finish(&self, token: &str, transferred: u64, remove: bool) -> Result<()> {
        let mut sessions = self
            .sessions
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if remove {
            sessions.remove(token);
            return Ok(());
        }
        let Some(session) = sessions.get_mut(token) else {
            return Ok(());
        };
        session.uploaded_bytes = session.uploaded_bytes.saturating_add(transferred);
        session.in_flight = false;
        if session.uploaded_bytes > GEMINI_UPLOAD_SESSION_MAX_BYTES {
            sessions.remove(token);
            return Err(anyhow!("Gemini upload exceeds the 2 GiB file limit"));
        }
        Ok(())
    }
}

fn validate_gemini_upload_session_url(channel: &ChannelConfig, value: &str) -> Result<()> {
    if value.len() > 8_192 {
        return Err(anyhow!("Gemini upload session URL is too long"));
    }
    let target = crate::channel_surface::channel_surface_target(
        channel,
        crate::surface::ApiSurface::Gemini,
        None,
        true,
    )
    .ok_or_else(|| anyhow!("channel has no verified native Gemini surface"))?;
    let base = reqwest::Url::parse(target.base_url.trim())?;
    let upload = reqwest::Url::parse(value)?;
    if !matches!(upload.scheme(), "http" | "https")
        || upload.username() != ""
        || upload.password().is_some()
        || upload.fragment().is_some()
        || upload.scheme() != base.scheme()
        || upload.host_str() != base.host_str()
        || upload.port_or_known_default() != base.port_or_known_default()
        || !upload.path().starts_with("/upload/")
    {
        return Err(anyhow!(
            "Gemini upload session URL is outside the verified upstream origin"
        ));
    }
    Ok(())
}

fn gemini_upload_limit_error(error: &(dyn std::error::Error + 'static)) -> bool {
    error
        .downcast_ref::<StreamingUploadLimitExceeded>()
        .is_some()
        || error.source().is_some_and(gemini_upload_limit_error)
}

fn local_gemini_upload_session_url(
    headers: &warp::http::HeaderMap,
    config: &ClientConfig,
    token: &str,
) -> String {
    let authority = headers
        .get(warp::http::header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<warp::http::uri::Authority>().ok())
        .map(|value| value.to_string())
        .unwrap_or_else(|| {
            config
                .listen
                .trim()
                .trim_start_matches("http://")
                .trim_start_matches("https://")
                .trim_end_matches('/')
                .to_string()
        });
    format!("http://{authority}{GEMINI_UPLOAD_SESSION_PREFIX}{token}")
}

fn register_gemini_upload_session_response(
    registry: &GeminiUploadSessionRegistry,
    route: &crate::surface::ApiRoute,
    channel: &ChannelConfig,
    request_headers: &warp::http::HeaderMap,
    config: &ClientConfig,
    response: &mut reqwest::Response,
) -> Result<()> {
    if route.surface != crate::surface::ApiSurface::Gemini
        || route.operation != crate::surface::ApiOperation::FilesCreate
        || !response.status().is_success()
    {
        return Ok(());
    }
    let Some(upstream_url) = response
        .headers()
        .get("x-goog-upload-url")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    let token = registry.insert(channel, upstream_url)?;
    let local_url = local_gemini_upload_session_url(request_headers, config, &token);
    response.headers_mut().insert(
        "x-goog-upload-url",
        reqwest::header::HeaderValue::from_str(&local_url)?,
    );
    Ok(())
}

pub(crate) struct GeminiUploadSessionRequest {
    method: warp::http::Method,
    token: String,
}

pub(crate) async fn match_gemini_upload_session_request(
    method: warp::http::Method,
    path: warp::path::FullPath,
) -> std::result::Result<GeminiUploadSessionRequest, warp::Rejection> {
    if !matches!(method, warp::http::Method::POST | warp::http::Method::PUT) {
        return Err(warp::reject::not_found());
    }
    let Some(token) = path.as_str().strip_prefix(GEMINI_UPLOAD_SESSION_PREFIX) else {
        return Err(warp::reject::not_found());
    };
    if token.len() != 48 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(warp::reject::not_found());
    }
    Ok(GeminiUploadSessionRequest {
        method,
        token: token.to_string(),
    })
}

pub(crate) async fn proxy_gemini_upload_session_request<S, B>(
    request: GeminiUploadSessionRequest,
    raw_query: Option<String>,
    headers: warp::http::HeaderMap,
    body: S,
    shared: Arc<ProxyShared>,
) -> Result<warp::reply::Response, Infallible>
where
    S: Stream<Item = std::result::Result<B, warp::Error>> + Send + 'static,
    B: bytes::Buf + Send + 'static,
{
    let config = shared.config_snapshot();
    if !local_proxy_request_authorized(
        &format!("{GEMINI_UPLOAD_SESSION_PREFIX}{}", request.token),
        raw_query.as_deref().unwrap_or_default(),
        &headers,
        &config,
    ) {
        return Ok(local_surface_error_response(
            crate::surface::ApiSurface::Gemini,
            StatusCode::UNAUTHORIZED,
            "unauthorized",
        )
        .unwrap_or_else(|_| error_response(StatusCode::UNAUTHORIZED, "unauthorized")));
    }
    if raw_query.is_some() {
        return Ok(local_surface_error_response(
            crate::surface::ApiSurface::Gemini,
            StatusCode::BAD_REQUEST,
            "Gemini upload session URLs do not accept caller query parameters",
        )
        .unwrap_or_else(|_| {
            error_response(StatusCode::BAD_REQUEST, "invalid upload session URL")
        }));
    }
    let declared_length = match request_content_length(&headers) {
        Some(Ok(length)) => Some(length),
        Some(Err(())) => {
            return Ok(local_surface_error_response(
                crate::surface::ApiSurface::Gemini,
                StatusCode::BAD_REQUEST,
                "invalid content-length header",
            )
            .unwrap_or_else(|_| {
                error_response(StatusCode::BAD_REQUEST, "invalid content-length header")
            }));
        }
        None => None,
    };
    let session = match shared
        .gemini_upload_sessions
        .begin(&request.token, declared_length)
    {
        Ok(session) => session,
        Err(error) => {
            let status = if error.to_string().contains("2 GiB") {
                StatusCode::PAYLOAD_TOO_LARGE
            } else if error.to_string().contains("in-flight") {
                StatusCode::CONFLICT
            } else {
                StatusCode::NOT_FOUND
            };
            return Ok(local_surface_error_response(
                crate::surface::ApiSurface::Gemini,
                status,
                &error.to_string(),
            )
            .unwrap_or_else(|_| error_response(status, "invalid Gemini upload session")));
        }
    };
    let Some(channel) = local_channels(&config)
        .find(|channel| channel.id == session.channel_id)
        .cloned()
    else {
        let _ = shared
            .gemini_upload_sessions
            .finish(&request.token, 0, true);
        return Ok(local_surface_error_response(
            crate::surface::ApiSurface::Gemini,
            StatusCode::SERVICE_UNAVAILABLE,
            "the Gemini upload owner channel is no longer configured",
        )
        .unwrap_or_else(|_| {
            error_response(StatusCode::SERVICE_UNAVAILABLE, "channel unavailable")
        }));
    };
    if !shared.ready_local_channel_ids().contains(&channel.id) {
        let _ = shared
            .gemini_upload_sessions
            .finish(&request.token, 0, false);
        return Ok(local_surface_error_response(
            crate::surface::ApiSurface::Gemini,
            StatusCode::SERVICE_UNAVAILABLE,
            "the Gemini upload owner channel is temporarily unavailable",
        )
        .unwrap_or_else(|_| {
            error_response(StatusCode::SERVICE_UNAVAILABLE, "channel unavailable")
        }));
    }
    let Some(update_activity_guard) = crate::update_activity::try_begin_proxy_request() else {
        let _ = shared
            .gemini_upload_sessions
            .finish(&request.token, 0, false);
        return Ok(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "client update is starting; retry in 2 seconds",
        ));
    };

    let transferred = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let counted = body.map({
        let transferred = transferred.clone();
        move |chunk| {
            let mut chunk = chunk.map_err(std::io::Error::other)?;
            let length = chunk.remaining() as u64;
            let prior = transferred.fetch_add(length, std::sync::atomic::Ordering::Relaxed);
            if session
                .uploaded_bytes
                .saturating_add(prior)
                .saturating_add(length)
                > GEMINI_UPLOAD_SESSION_MAX_BYTES
            {
                return Err(std::io::Error::other(StreamingUploadLimitExceeded));
            }
            Ok(chunk.copy_to_bytes(length as usize))
        }
    });
    let mut upstream_headers = reqwest::header::HeaderMap::new();
    for (name, value) in &headers {
        if let (Ok(name), Ok(value)) = (
            reqwest::header::HeaderName::from_bytes(name.as_str().as_bytes()),
            reqwest::header::HeaderValue::from_bytes(value.as_bytes()),
        ) {
            upstream_headers.append(name, value);
        }
    }
    let content_length = upstream_headers
        .get(reqwest::header::CONTENT_LENGTH)
        .cloned();
    crate::channel_executor::strip_http_request_headers(&mut upstream_headers);
    if let Some(content_length) = content_length {
        upstream_headers.insert(reqwest::header::CONTENT_LENGTH, content_length);
    }
    upstream_headers.insert(
        reqwest::header::ACCEPT_ENCODING,
        reqwest::header::HeaderValue::from_static("identity"),
    );
    let reqwest_method = reqwest::Method::from_bytes(request.method.as_str().as_bytes())
        .unwrap_or(reqwest::Method::POST);
    let upstream_request = shared
        .client
        .request(reqwest_method, &session.upstream_url)
        .headers(upstream_headers)
        .body(reqwest::Body::wrap_stream(counted));
    let result = crate::upstream_transport::send(upstream_request).await;
    let transferred = transferred.load(std::sync::atomic::Ordering::Relaxed);
    let finalize = headers
        .get("x-goog-upload-command")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|command| command.trim().eq_ignore_ascii_case("finalize"))
        });
    let mut transport_failure = false;
    let response = match result {
        Ok(mut upstream) => {
            crate::channel_executor::strip_http_response_headers(upstream.headers_mut());
            let success = upstream.status().is_success();
            let _ = shared.gemini_upload_sessions.finish(
                &request.token,
                transferred,
                finalize && success,
            );
            let response = match response_from_reqwest(upstream, false).await {
                Ok(response) => response,
                Err(error) => {
                    return Ok(error_response(StatusCode::BAD_GATEWAY, &error.to_string()));
                }
            };
            let Some(route) =
                crate::surface::resolve_api_route("POST", "/gemini/upload/v1beta/files", false)
            else {
                return Ok(error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Gemini file upload route is unavailable in this client build",
                ));
            };
            capture_local_resource_owner_response(
                shared.local_resource_owners.clone(),
                route,
                "POST",
                "/gemini/upload/v1beta/files",
                &channel.id,
                "",
                response,
            )
            .await
            .unwrap_or_else(|error| error_response(StatusCode::BAD_GATEWAY, &error.to_string()))
        }
        Err(error) => {
            let _ = shared
                .gemini_upload_sessions
                .finish(&request.token, 0, false);
            let status = if gemini_upload_limit_error(&error) {
                StatusCode::PAYLOAD_TOO_LARGE
            } else {
                transport_failure = true;
                StatusCode::BAD_GATEWAY
            };
            error_response(status, &error.to_string())
        }
    };
    let channel_scoped = transport_failure
        || local_response_marks_channel_unready_for_request(
            request.method.as_str(),
            GEMINI_UPLOAD_SESSION_PREFIX,
            response.status(),
        );
    if let Some(ready) =
        local_channel_readiness_observation(response.status(), false, channel_scoped)
    {
        shared.set_local_channel_ready(&channel.id, ready);
    }
    Ok(hold_proxy_update_activity_until_body_end(
        response,
        update_activity_guard,
    ))
}
