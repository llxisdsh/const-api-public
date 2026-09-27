impl ProxyRuntime {
    pub(crate) fn status(&self) -> ProxyStatus {
        let platform = self
            .platform_transport_state
            .lock()
            .map(|state| state.clone())
            .unwrap_or_default();
        ProxyStatus {
            running: self.running,
            listen: self.listen.clone(),
            active_endpoint: if platform.endpoint.is_empty() {
                self.active_endpoint.clone()
            } else {
                Some(platform.endpoint)
            },
            platform_connection_state: platform.connection_state,
            platform_transport: platform.connected_transport,
            platform_transport_error: platform.last_error,
            platform_server_version: platform.server_version,
        }
    }
}

const MODEL_REQUEST_MAX_UPSTREAM_ATTEMPTS: usize = 3;
const MODEL_CATALOG_UPSTREAM_TIMEOUT: Duration = Duration::from_secs(5);
const MODEL_CATALOG_CACHE_HEADER: &str = "x-const-model-catalog-cache";
const MODEL_CATALOG_FRESH_TTL: Duration = Duration::from_secs(30);
const MODEL_CATALOG_CACHE_MAX_BYTES: usize = 8 * 1024 * 1024;
const LOCAL_MODEL_ROUTE_PREFERENCE_IDLE_SECONDS: i64 = 5 * 60 * 60;
const LOCAL_MODEL_ROUTE_PREFERENCE_CAPACITY: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ModelCatalogCacheKey {
    surface: crate::surface::ApiSurface,
    raw_query: String,
}

#[derive(Clone)]
struct CachedModelCatalogResponse {
    platform_api_key: String,
    body: bytes::Bytes,
    received_at: std::time::Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LocalModelRoutePreferenceContext {
    platform_authenticated: bool,
    prefer_local: bool,
    policy_fingerprint: u64,
}

#[derive(Clone, Debug)]
struct LocalModelRoutePreference {
    route_key: String,
    context: LocalModelRoutePreferenceContext,
    last_success_at_unix: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LocalModelRouteFailureEvidence {
    failure_model: String,
    error_kind: String,
    retry_after_seconds: i64,
    soft: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LocalDeclaredFailureScope {
    Channel,
    Request,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LocalAdmissionFailure {
    ConcurrencyFull,
}

#[derive(Clone, Copy, Debug)]
struct UpstreamAttemptBudget {
    remaining: usize,
}

impl Default for UpstreamAttemptBudget {
    fn default() -> Self {
        Self {
            remaining: MODEL_REQUEST_MAX_UPSTREAM_ATTEMPTS,
        }
    }
}

impl UpstreamAttemptBudget {
    fn available(self) -> bool {
        self.remaining > 0
    }

    fn consume(&mut self, attempts: usize) {
        self.remaining = self.remaining.saturating_sub(attempts);
    }
}

pub(crate) struct ProxyShared {
    pub(crate) config: ClientConfig,
    pub(crate) live_config: Arc<StdMutex<ClientConfig>>,
    pub(crate) local_channel_readiness: Arc<StdMutex<HashMap<String, bool>>>,
    pub(crate) local_model_quota_routes:
        Arc<StdMutex<HashMap<String, crate::supplier::LocalModelQuotaRouteState>>>,
    pub(crate) client: Client,
    pub(crate) platform_transport: Arc<crate::platform_transport::PlatformHttpTransport>,
    pub(crate) platform_duplex_pool: crate::supplier::PlatformDuplexPool,
    pub(crate) platform_access_token: Arc<StdMutex<String>>,
    pub(crate) account_refresh_notify: Arc<tokio::sync::Notify>,
    pub(crate) active_index: Mutex<usize>,
    pub(crate) platform_api_key: Arc<StdMutex<String>>,
    model_catalog_upstream_timeout: Duration,
    model_catalog_snapshots:
        Arc<StdMutex<HashMap<ModelCatalogCacheKey, CachedModelCatalogResponse>>>,
    local_model_route_preferences: Arc<StdMutex<HashMap<String, LocalModelRoutePreference>>>,
    local_resource_owners: Arc<LocalResourceOwnerRegistry>,
    gemini_upload_sessions: Arc<GeminiUploadSessionRegistry>,
    platform_voice_calls: Arc<Mutex<HashMap<String, Arc<PlatformVoiceCall>>>>,
}

impl ProxyShared {
    #[cfg(test)]
    pub(crate) fn from_config(config: ClientConfig, client: Client) -> Self {
        let platform_api_key = Arc::new(StdMutex::new(config.account_device_api_key.clone()));
        let readiness = config
            .channels
            .iter()
            .map(|channel| (channel.id.clone(), true))
            .collect();
        Self::with_platform_api_key(
            config.clone(),
            client,
            platform_api_key,
            Arc::new(StdMutex::new(config)),
            Arc::new(StdMutex::new(readiness)),
        )
    }

    #[cfg(test)]
    pub(crate) fn with_platform_api_key(
        config: ClientConfig,
        client: Client,
        platform_api_key: Arc<StdMutex<String>>,
        live_config: Arc<StdMutex<ClientConfig>>,
        local_channel_readiness: Arc<StdMutex<HashMap<String, bool>>>,
    ) -> Self {
        Self::with_platform_api_key_and_transport_state(
            config,
            client,
            platform_api_key,
            Arc::new(StdMutex::new(String::new())),
            Arc::new(tokio::sync::Notify::new()),
            live_config,
            local_channel_readiness,
            Arc::new(StdMutex::new(HashMap::new())),
            Arc::new(StdMutex::new(
                crate::platform_transport::PlatformTransportSnapshot::default(),
            )),
        )
    }

    #[cfg(test)]
    pub(crate) fn with_platform_api_key_and_transport_state(
        config: ClientConfig,
        client: Client,
        platform_api_key: Arc<StdMutex<String>>,
        platform_access_token: Arc<StdMutex<String>>,
        account_refresh_notify: Arc<tokio::sync::Notify>,
        live_config: Arc<StdMutex<ClientConfig>>,
        local_channel_readiness: Arc<StdMutex<HashMap<String, bool>>>,
        local_model_quota_routes: Arc<
            StdMutex<HashMap<String, crate::supplier::LocalModelQuotaRouteState>>,
        >,
        platform_transport_state: Arc<
            StdMutex<crate::platform_transport::PlatformTransportSnapshot>,
        >,
    ) -> Self {
        let platform_transport = Arc::new(crate::platform_transport::PlatformHttpTransport::new(
            client.clone(),
            platform_transport_state,
        ));
        Self::with_platform_api_key_and_transport(
            config,
            client,
            platform_api_key,
            platform_access_token,
            account_refresh_notify,
            live_config,
            local_channel_readiness,
            local_model_quota_routes,
            platform_transport,
        )
    }

    pub(crate) fn with_platform_api_key_and_transport(
        config: ClientConfig,
        client: Client,
        platform_api_key: Arc<StdMutex<String>>,
        platform_access_token: Arc<StdMutex<String>>,
        account_refresh_notify: Arc<tokio::sync::Notify>,
        live_config: Arc<StdMutex<ClientConfig>>,
        local_channel_readiness: Arc<StdMutex<HashMap<String, bool>>>,
        local_model_quota_routes: Arc<
            StdMutex<HashMap<String, crate::supplier::LocalModelQuotaRouteState>>,
        >,
        platform_transport: Arc<crate::platform_transport::PlatformHttpTransport>,
    ) -> Self {
        #[cfg(test)]
        let local_resource_owners = LocalResourceOwnerRegistry::in_memory();
        #[cfg(not(test))]
        let local_resource_owners = LocalResourceOwnerRegistry::open(
            local_resource_owner_database_path(&crate::default_config_path()),
        );
        Self {
            config,
            live_config,
            local_channel_readiness,
            local_model_quota_routes,
            client,
            platform_transport,
            platform_duplex_pool: crate::supplier::PlatformDuplexPool::default(),
            platform_access_token,
            account_refresh_notify,
            active_index: Mutex::new(0),
            platform_api_key,
            model_catalog_upstream_timeout: MODEL_CATALOG_UPSTREAM_TIMEOUT,
            model_catalog_snapshots: Arc::new(StdMutex::new(HashMap::new())),
            local_model_route_preferences: Arc::new(StdMutex::new(HashMap::new())),
            local_resource_owners,
            gemini_upload_sessions: GeminiUploadSessionRegistry::new(),
            platform_voice_calls: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) fn probe_platform_endpoints(&self) {
        if let Some(endpoint) = self
            .config_snapshot()
            .endpoints
            .into_iter()
            .filter(|endpoint| endpoint.enabled && !endpoint.base_url.trim().is_empty())
            .next()
        {
            self.platform_transport.start_probe(endpoint);
        }
    }

    fn config_snapshot(&self) -> ClientConfig {
        self.live_config
            .lock()
            .map(|config| config.clone())
            .unwrap_or_else(|_| self.config.clone())
    }

    fn ready_local_channel_ids(&self) -> HashSet<String> {
        self.local_channel_readiness
            .lock()
            .map(|readiness| {
                readiness
                    .iter()
                    .filter(|(_, ready)| **ready)
                    .map(|(channel_id, _)| channel_id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn remember_model_catalog_snapshot(
        &self,
        surface: crate::surface::ApiSurface,
        raw_query: &str,
        platform_api_key: &str,
        body: &bytes::Bytes,
    ) {
        if body.len() > MODEL_CATALOG_CACHE_MAX_BYTES || !model_catalog_body_has_models(surface, body) {
            return;
        }
        let platform_api_key = platform_api_key.trim();
        if platform_api_key.is_empty() {
            return;
        }
        let current_platform_api_key = self
            .platform_api_key
            .lock()
            .map(|key| key.trim().to_string())
            .unwrap_or_default();
        if current_platform_api_key != platform_api_key {
            return;
        }
        if let Ok(mut snapshots) = self.model_catalog_snapshots.lock() {
            // Query variants must not turn a tool's discovery cache into an
            // unbounded resident copy of the catalog. Evict the oldest first.
            while snapshots.len() >= 16 || snapshots.values().map(|item| item.body.len()).sum::<usize>() + body.len() > MODEL_CATALOG_CACHE_MAX_BYTES {
                let Some(oldest) = snapshots.iter().min_by_key(|(_, item)| item.received_at).map(|(key, _)| key.clone()) else { break };
                snapshots.remove(&oldest);
            }
            snapshots.insert(
                ModelCatalogCacheKey {
                    surface,
                    raw_query: raw_query.to_string(),
                },
                CachedModelCatalogResponse {
                    platform_api_key: platform_api_key.to_string(),
                    body: body.clone(),
                    received_at: std::time::Instant::now(),
                },
            );
        }
    }

    fn cached_model_catalog_response(
        &self,
        surface: crate::surface::ApiSurface,
        raw_query: &str,
    ) -> Result<Option<warp::reply::Response>> {
        self.model_catalog_snapshot_response(surface, raw_query, false)
    }

    fn model_catalog_snapshot_response(
        &self,
        surface: crate::surface::ApiSurface,
        raw_query: &str,
        fresh_only: bool,
    ) -> Result<Option<warp::reply::Response>> {
        let platform_api_key = self
            .platform_api_key
            .lock()
            .map(|key| key.trim().to_string())
            .unwrap_or_default();
        if platform_api_key.is_empty() {
            return Ok(None);
        }
        let cached = self
            .model_catalog_snapshots
            .lock()
            .ok()
            .and_then(|snapshots| {
                snapshots
                    .get(&ModelCatalogCacheKey {
                        surface,
                        raw_query: raw_query.to_string(),
                    })
                    .filter(|cached| cached.platform_api_key == platform_api_key)
                    .filter(|cached| !fresh_only || cached.received_at.elapsed() < MODEL_CATALOG_FRESH_TTL)
                    .cloned()
            });
        let Some(cached) = cached else {
            return Ok(None);
        };
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        headers.insert(
            MODEL_CATALOG_CACHE_HEADER,
            reqwest::header::HeaderValue::from_static(if fresh_only { "hit" } else { "stale" }),
        );
        response_from_body(StatusCode::OK, &headers, cached.body).map(Some)
    }

    fn clear_model_catalog_snapshot(
        &self,
        surface: crate::surface::ApiSurface,
        raw_query: &str,
        platform_api_key: &str,
    ) {
        if let Ok(mut snapshots) = self.model_catalog_snapshots.lock() {
            let platform_api_key = platform_api_key.trim();
            snapshots.retain(|key, cached| {
                key.surface != surface
                    || key.raw_query != raw_query
                    || cached.platform_api_key != platform_api_key
            });
        }
    }

    fn set_local_channel_ready(&self, channel_id: &str, ready: bool) {
        if let Ok(mut readiness) = self.local_channel_readiness.lock() {
            readiness.insert(channel_id.to_string(), ready);
        }
    }

    fn record_local_model_response(
        &self,
        channel_id: &str,
        model: &str,
        status: StatusCode,
        headers: &warp::http::HeaderMap,
        evidence: Option<&LocalModelRouteFailureEvidence>,
    ) -> bool {
        let key = crate::supplier::model_quota_route_key(channel_id, model);
        let Ok(mut routes) = self.local_model_quota_routes.lock() else {
            return false;
        };
        if status.is_success() {
            // HTTP 200 headers are not a completed streaming model response.
            // Recovery is committed by LocalModelOutcomeGuard on completion.
            return false;
        }
        let now = now_unix();
        let matching_evidence = evidence.filter(|evidence| {
            normalize_model_name(&evidence.failure_model) == normalize_model_name(model)
        });
        if matching_evidence.is_some() {
            let retry_after = matching_evidence
                .map(|evidence| evidence.retry_after_seconds)
                .unwrap_or_default()
                .max({
                    headers
                        .get("retry-after")
                        .and_then(|value| value.to_str().ok())
                        .and_then(|value| value.trim().parse::<i64>().ok())
                        .unwrap_or_default()
                });
            routes
                .entry(key)
                .or_default()
                .record_runtime_failure(
                    now,
                    retry_after,
                    matching_evidence.is_some_and(|evidence| evidence.soft),
                );
            return true;
        }
        false
    }

    fn claim_local_model_route(&self, route_key: &str, now: i64) -> bool {
        let Ok(mut routes) = self.local_model_quota_routes.lock() else {
            return true;
        };
        routes
            .get_mut(route_key)
            .is_none_or(|state| state.claim_runtime_probe(now))
    }

    fn release_local_model_route_probe(&self, route_key: &str) {
        if let Ok(mut routes) = self.local_model_quota_routes.lock() {
            if let Some(state) = routes.get_mut(route_key) {
                state.release_runtime_probe();
            }
        }
    }

    fn preferred_local_model_route(
        &self,
        affinity_key: &str,
        context: LocalModelRoutePreferenceContext,
        now: i64,
    ) -> Option<String> {
        let Ok(mut preferences) = self.local_model_route_preferences.lock() else {
            return None;
        };
        let preference = preferences.get(affinity_key)?.clone();
        if preference.context != context
            || now.saturating_sub(preference.last_success_at_unix)
                >= LOCAL_MODEL_ROUTE_PREFERENCE_IDLE_SECONDS
        {
            preferences.remove(affinity_key);
            return None;
        }
        Some(preference.route_key)
    }

    fn promote_local_model_route(
        &self,
        affinity_key: &str,
        route_key: &str,
        context: LocalModelRoutePreferenceContext,
        now: i64,
    ) {
        if affinity_key.is_empty() || route_key.is_empty() {
            return;
        }
        let Ok(mut preferences) = self.local_model_route_preferences.lock() else {
            return;
        };
        if !preferences.contains_key(affinity_key)
            && preferences.len() >= LOCAL_MODEL_ROUTE_PREFERENCE_CAPACITY
        {
            if let Some(oldest) = preferences
                .iter()
                .min_by_key(|(_, preference)| preference.last_success_at_unix)
                .map(|(key, _)| key.clone())
            {
                preferences.remove(&oldest);
            }
        }
        preferences.insert(
            affinity_key.to_string(),
            LocalModelRoutePreference {
                route_key: route_key.to_string(),
                context,
                last_success_at_unix: now,
            },
        );
    }

    fn forget_local_model_route(&self, affinity_key: &str, route_key: &str) {
        if let Ok(mut preferences) = self.local_model_route_preferences.lock() {
            if preferences
                .get(affinity_key)
                .is_some_and(|preference| preference.route_key == route_key)
            {
                preferences.remove(affinity_key);
            }
        }
    }
}

pub(crate) fn with_shared(
    shared: Arc<ProxyShared>,
) -> impl Filter<Extract = (Arc<ProxyShared>,), Error = Infallible> + Clone {
    warp::any().map(move || shared.clone())
}

pub(crate) fn optional_raw_query(
) -> impl Filter<Extract = (Option<String>,), Error = Infallible> + Clone {
    warp::query::raw()
        .map(Some)
        .or(warp::any().map(|| None::<String>))
        .unify()
}

pub(crate) async fn proxy_request(
    method: warp::http::Method,
    path: warp::path::FullPath,
    raw_query: Option<String>,
    headers: warp::http::HeaderMap,
    body: bytes::Bytes,
    shared: Arc<ProxyShared>,
) -> Result<warp::reply::Response, Infallible> {
    let path_str = path.as_str();
    let has_query = raw_query.is_some();
    let raw_query = raw_query.unwrap_or_default();
    if path_str == "/" {
        if method != warp::http::Method::GET {
            let mut response = error_response(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
            response
                .headers_mut()
                .insert("allow", warp::http::HeaderValue::from_static("GET"));
            return Ok(response);
        }
        let wants_json = headers
            .get("accept")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.to_ascii_lowercase().contains("application/json"));
        let response = if wants_json {
            warp::reply::json(&crate::surface::surface_index_document()).into_response()
        } else {
            warp::reply::html(crate::surface::surface_index_html()).into_response()
        };
        return Ok(secure_surface_index_response(response));
    }
    if path_str == "/healthz" {
        return Ok(warp::reply::json(&serde_json::json!({"ok": true})).into_response());
    }
    #[cfg(all(debug_assertions, feature = "memory-diagnostics"))]
    if path_str == "/__const/dev/memory" && crate::development_profile_active() {
        if method != warp::http::Method::GET {
            return Ok(error_response(StatusCode::METHOD_NOT_ALLOWED, "method not allowed"));
        }
        // Do not clone the full channel catalog just to read the owner key;
        // doing so would perturb the very allocation snapshot being measured.
        let authorized = shared.live_config.lock().is_ok_and(|config| {
            !config.api_key.trim().is_empty()
                && local_proxy_header_authorized(&headers, config.api_key.trim())
        });
        if !authorized {
            return Ok(error_response(StatusCode::UNAUTHORIZED, "unauthorized"));
        }
        let mut response = warp::reply::json(&crate::memory_diagnostics::snapshot()).into_response();
        response.headers_mut().insert("cache-control", warp::http::HeaderValue::from_static("no-store"));
        return Ok(response);
    }
    if path_str == "/.well-known/const-api" {
        if method != warp::http::Method::GET {
            return Ok(error_response(
                StatusCode::METHOD_NOT_ALLOWED,
                "method not allowed",
            ));
        }
        return Ok(warp::reply::json(&crate::surface::surface_manifest()).into_response());
    }
    if !is_local_proxy_api_path(path_str) {
        return Ok(error_response(StatusCode::NOT_FOUND, "not found"));
    }
    if crate::surface::is_reserved_management_path(path_str) {
        let mut response = local_surface_error_response(
            crate::surface::ApiSurface::OpenAi,
            StatusCode::FORBIDDEN,
            "organization and project administration require an independent management endpoint, authentication policy, and audit trail",
        )
        .unwrap_or_else(|_| error_response(StatusCode::FORBIDDEN, "management API is not exposed"));
        response.headers_mut().insert(
            "x-const-error-code",
            warp::http::HeaderValue::from_static("management_surface_not_exposed"),
        );
        return Ok(response);
    }
    let websocket = headers
        .get("upgrade")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"));
    if websocket {
        let response = crate::surface::surface_from_path(path_str)
            .and_then(|surface| {
                local_surface_error_response(
                    surface,
                    StatusCode::NOT_IMPLEMENTED,
                    "websocket surface forwarding is not implemented",
                )
                .ok()
            })
            .unwrap_or_else(|| {
                error_response(
                    StatusCode::NOT_IMPLEMENTED,
                    "websocket surface forwarding is not implemented",
                )
            });
        return Ok(response);
    }
    if let Some((surface, allowed)) =
        crate::surface::known_route_allowed_methods(path_str, websocket)
    {
        if !allowed
            .iter()
            .any(|allowed_method| method.as_str().eq_ignore_ascii_case(allowed_method))
        {
            let mut response = local_surface_error_response(
                surface,
                StatusCode::METHOD_NOT_ALLOWED,
                "method not allowed",
            )
            .unwrap_or_else(|_| {
                error_response(StatusCode::METHOD_NOT_ALLOWED, "method not allowed")
            });
            if let Ok(value) = allowed.join(", ").parse() {
                response.headers_mut().insert("allow", value);
            }
            return Ok(response);
        }
    }
    let config = shared.config_snapshot();
    let identity = match local_proxy_request_identity(path_str, &raw_query, &headers, &config) {
        Ok(Some(identity)) => identity,
        Ok(None) => return Ok(error_response(StatusCode::UNAUTHORIZED, "unauthorized")),
        Err(error) => {
            return Ok(lan_share_error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                &error.to_string(),
                "lan_share_members_unavailable",
            ));
        }
    };
    if path_str == "/api/lan-share/me" {
        if method != warp::http::Method::GET {
            let mut response = lan_share_error_response(
                StatusCode::METHOD_NOT_ALLOWED,
                "method not allowed",
                "method_not_allowed",
            );
            response
                .headers_mut()
                .insert("allow", warp::http::HeaderValue::from_static("GET"));
            return Ok(response);
        }
        let LocalProxyRequestIdentity::LanShareMember { member, .. } = identity else {
            return Ok(lan_share_error_response(
                StatusCode::FORBIDDEN,
                "a LAN share member key is required",
                "lan_share_member_key_required",
            ));
        };
        return Ok(match crate::lan_share::lan_share_self_status(&config, &member) {
            Ok(status) => {
                let mut response = warp::reply::json(&status).into_response();
                response.headers_mut().insert(
                    "cache-control",
                    warp::http::HeaderValue::from_static("no-store"),
                );
                response
            }
            Err(error) => lan_share_error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                &error.to_string(),
                "lan_share_usage_unavailable",
            ),
        });
    }

    let member = match &identity {
        LocalProxyRequestIdentity::Owner { .. } => None,
        LocalProxyRequestIdentity::LanShareMember { member, .. } => Some(member.clone()),
    };
    let mut request_config = config.clone();
    if let Some(member) = member.as_ref() {
        if path_str.starts_with("/api/") {
            return Ok(lan_share_error_response(
                StatusCode::FORBIDDEN,
                "this member key can only access shared model APIs",
                "lan_share_scope_forbidden",
            ));
        }
        let incoming_path = match headers
            .get(crate::lan_share::LAN_SHARE_PATH_HEADER)
            .map(|value| value.to_str())
            .transpose()
        {
            Ok(value) => value,
            Err(_) => {
                return Ok(lan_share_error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid LAN share path header",
                    "lan_share_path_invalid",
                ));
            }
        };
        let path_nodes = match crate::lan_share::parse_lan_share_path(incoming_path) {
            Ok(path) => path,
            Err(error) => {
                return Ok(lan_share_error_response(
                    StatusCode::BAD_REQUEST,
                    &error.to_string(),
                    "lan_share_path_invalid",
                ));
            }
        };
        if path_nodes.iter().any(|node| node == &config.client_id) {
            return Ok(lan_share_error_response(
                StatusCode::LOOP_DETECTED,
                "LAN share forwarding loop detected",
                "lan_share_loop_detected",
            ));
        }
        if path_nodes.len() > crate::lan_share::LAN_SHARE_MAX_HOPS {
            return Ok(lan_share_error_response(
                StatusCode::LOOP_DETECTED,
                "LAN share forwarding hop limit reached",
                "lan_share_hop_limit",
            ));
        }
        if !member.enabled {
            return Ok(lan_share_error_response(
                StatusCode::FORBIDDEN,
                "this LAN share member is paused",
                "lan_share_member_paused",
            ));
        }
        let usage = match crate::lan_share::lan_share_usage_snapshot(&member.id) {
            Ok(usage) => usage,
            Err(error) => {
                return Ok(lan_share_error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    &error.to_string(),
                    "lan_share_usage_unavailable",
                ));
            }
        };
        if usage.used_tokens >= member.weekly_token_limit {
            return Ok(lan_share_error_response(
                StatusCode::TOO_MANY_REQUESTS,
                "this member's weekly LAN share quota is exhausted",
                "lan_share_quota_exhausted",
            ));
        }
        let body_text = String::from_utf8_lossy(&body);
        request_config = crate::lan_share::lan_share_restricted_config(&config);
        if requested_model_from_body_or_path(&body_text, path_str).is_some_and(|requested| {
            !crate::lan_share::lan_share_model_is_allowed(&request_config, &requested)
        }) {
            return Ok(lan_share_error_response(
                StatusCode::FORBIDDEN,
                "the requested model is not shared with this member",
                "lan_share_model_not_shared",
            ));
        }
    }
    let Some(update_activity_guard) = crate::update_activity::try_begin_proxy_request() else {
        let mut response = error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "client update is starting; retry in 2 seconds",
        );
        response
            .headers_mut()
            .insert("retry-after", warp::http::HeaderValue::from_static("2"));
        response.headers_mut().insert(
            "x-const-error-code",
            warp::http::HeaderValue::from_static("client_updating"),
        );
        return Ok(response);
    };

    let raw_query = strip_local_auth_query_for_key(
        path_str,
        &raw_query,
        &headers,
        identity.api_key(),
    );
    let method_for_log = method.clone();
    let path_for_log = path_str.to_string();
    let query_for_log = raw_query.clone();
    let request_headers_for_log = headers.clone();
    let request_body_for_log = body.clone();
    let lan_share_metering = member.as_ref().and_then(|_| {
        lan_share_metering_for_request(&method_for_log, path_str, &request_body_for_log)
    });
    let response = match forward_with_failover_with_query_presence(
        method,
        path_str,
        &raw_query,
        has_query,
        headers,
        body,
        &request_config,
        &shared,
        member.is_none(),
    )
    .await
    {
        Ok(resp) => resp,
        Err(err) => proxy_execution_error_response(&err),
    };
    crate::client_toasts::record_platform_toast(&response);
    let _ = write_local_proxy_recent_request_status(
        &method_for_log,
        &path_for_log,
        &request_headers_for_log,
        &request_body_for_log,
        response.status(),
    );
    let response = append_raw_proxy_exchange_log_and_rebuild_response(
        &method_for_log,
        &path_for_log,
        &query_for_log,
        &request_headers_for_log,
        &request_body_for_log,
        response,
    )
    .await;
    let response = if response.status().is_success() {
        match (member, lan_share_metering) {
            (Some(member), Some(metering)) => meter_lan_share_response(
                response,
                member.id,
                request_body_for_log.len(),
                metering.protocol,
            ),
            _ => response,
        }
    } else {
        response
    };
    Ok(hold_proxy_update_activity_until_body_end(
        response,
        update_activity_guard,
    ))
}

#[derive(Debug, Clone, Copy)]
struct LanShareMetering {
    protocol: Option<crate::protocol::kind::ProtocolKind>,
}

fn lan_share_metering_for_request(
    method: &warp::http::Method,
    path: &str,
    body: &[u8],
) -> Option<LanShareMetering> {
    let route = crate::surface::resolve_api_route(method.as_str(), path, false)?;
    (route.billing_class == crate::surface::BillingClass::Metered
        && requested_model_from_body_or_path(&String::from_utf8_lossy(body), path).is_some())
    .then_some(LanShareMetering {
        protocol: route.protocol,
    })
}

struct LanShareUsageRecorder {
    member_id: String,
    request_bytes: usize,
    response_bytes: usize,
    response_tail: Vec<u8>,
    usage: Option<crate::supplier::SupplierUpstreamUsageAccumulator>,
}

impl LanShareUsageRecorder {
    const TAIL_LIMIT: usize = 512 * 1024;

    fn observe(&mut self, chunk: &[u8]) {
        self.response_bytes = self.response_bytes.saturating_add(chunk.len());
        if let Some(usage) = self.usage.as_mut() {
            usage.push(chunk);
        }
        if chunk.len() >= Self::TAIL_LIMIT {
            self.response_tail.clear();
            self.response_tail
                .extend_from_slice(&chunk[chunk.len() - Self::TAIL_LIMIT..]);
            return;
        }
        let overflow = self
            .response_tail
            .len()
            .saturating_add(chunk.len())
            .saturating_sub(Self::TAIL_LIMIT);
        if overflow > 0 {
            self.response_tail.drain(..overflow);
        }
        self.response_tail.extend_from_slice(chunk);
    }
}

impl Drop for LanShareUsageRecorder {
    fn drop(&mut self) {
        let upstream_usage = self
            .usage
            .take()
            .and_then(crate::supplier::SupplierUpstreamUsageAccumulator::finish)
            .map(|usage| usage.lan_share_token_counts())
            .unwrap_or_default();
        let response_tail = String::from_utf8_lossy(&self.response_tail);
        let reported_usage = upstream_usage.merge(crate::supplier::extract_usage_token_counts(
            &response_tail,
        ));
        if let Err(error) = crate::lan_share::record_lan_share_usage(
            &self.member_id,
            self.request_bytes,
            reported_usage,
            self.response_bytes,
        ) {
            log::warn!(
                "[const-api][lan-share] failed to record usage for member {}: {error}",
                self.member_id
            );
        }
    }
}

fn meter_lan_share_response(
    response: warp::reply::Response,
    member_id: String,
    request_bytes: usize,
    protocol: Option<crate::protocol::kind::ProtocolKind>,
) -> warp::reply::Response {
    let (parts, body) = response.into_parts();
    let mut recorder = LanShareUsageRecorder {
        member_id,
        request_bytes,
        response_bytes: 0,
        response_tail: Vec::new(),
        usage: protocol.map(|protocol| {
            crate::supplier::SupplierUpstreamUsageAccumulator::new(protocol.as_str())
        }),
    };
    let body = body.into_data_stream().map(move |result| {
        if let Ok(chunk) = result.as_ref() {
            recorder.observe(chunk);
        }
        result
    });
    let body = warp::reply::stream(body).into_response().into_body();
    warp::http::Response::from_parts(parts, body)
}

fn hold_proxy_update_activity_until_body_end(
    response: warp::reply::Response,
    update_activity_guard: crate::update_activity::UpdateActivityGuard,
) -> warp::reply::Response {
    let (parts, body) = response.into_parts();
    let body = body.into_data_stream().map_ok(move |chunk| {
        let _ = &update_activity_guard;
        chunk
    });
    let body = warp::reply::stream(body).into_response().into_body();
    warp::http::Response::from_parts(parts, body)
}

pub(crate) fn write_local_proxy_recent_request_status(
    method: &warp::http::Method,
    path: &str,
    request_headers: &warp::http::HeaderMap,
    request_body: &[u8],
    response_status: warp::http::StatusCode,
) -> Result<()> {
    let record = serde_json::json!({
        "created_at_unix": now_unix(),
        "method": method.as_str(),
        "path": path,
        "status": response_status.as_u16(),
        "tool": infer_tool_from_headers(request_headers),
        "model": request_model_from_body(request_body),
    });
    let scope = crate::client_data_root();
    let mut recent = local_proxy_recent_request_slot()
        .lock()
        .map_err(|_| anyhow!("local proxy recent request state poisoned"))?;
    *recent = Some((scope, record));
    Ok(())
}

fn local_proxy_recent_request_slot(
) -> &'static StdMutex<Option<(PathBuf, serde_json::Value)>> {
    static RECENT: std::sync::OnceLock<
        StdMutex<Option<(PathBuf, serde_json::Value)>>,
    > = std::sync::OnceLock::new();
    RECENT.get_or_init(|| StdMutex::new(None))
}

pub(crate) fn local_proxy_recent_request_status() -> Option<serde_json::Value> {
    let scope = crate::client_data_root();
    local_proxy_recent_request_slot()
        .lock()
        .ok()
        .and_then(|recent| recent.as_ref()
            .filter(|(saved_scope, _)| saved_scope == &scope)
            .map(|(_, record)| record.clone()))
}

fn infer_tool_from_headers(headers: &warp::http::HeaderMap) -> &'static str {
    let ua = headers
        .get("user-agent")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if ua.contains("codex") {
        "codex"
    } else if ua.contains("claude") {
        "claude"
    } else if ua.contains("gemini") {
        "gemini"
    } else if ua.contains("opencode") {
        "opencode"
    } else if ua.contains("hermes") {
        "hermes"
    } else {
        "unknown"
    }
}

fn request_model_from_body(body: &[u8]) -> Option<String> {
    crate::supplier::top_level_json_string(body, "model")
}

#[cfg(test)]
pub(crate) async fn forward_with_failover(
    method: warp::http::Method,
    path: &str,
    raw_query: &str,
    headers: warp::http::HeaderMap,
    body: bytes::Bytes,
    shared: &Arc<ProxyShared>,
) -> Result<warp::reply::Response> {
    let config = shared.config_snapshot();
    forward_with_failover_with_query_presence(
        method,
        path,
        raw_query,
        !raw_query.is_empty(),
        headers,
        body,
        &config,
        shared,
        true,
    )
    .await
}

async fn forward_with_failover_with_query_presence(
    method: warp::http::Method,
    path: &str,
    raw_query: &str,
    has_query: bool,
    mut headers: warp::http::HeaderMap,
    body: bytes::Bytes,
    config: &ClientConfig,
    shared: &Arc<ProxyShared>,
    platform_allowed: bool,
) -> Result<warp::reply::Response> {
    let mut ready_local_channel_ids = None;
    if let Some(response) = try_platform_voice_hangup(&method, path, &headers, shared).await {
        return Ok(response);
    }
    if platform_allowed && is_platform_voice_call(&method, path) {
        if let Some(response) = try_platform_voice_call(path, raw_query, &headers, &body, config, shared).await? {
            return Ok(response);
        }
    }
    let skip_local_short_circuit = headers
        .get(SKIP_LOCAL_SHORT_CIRCUIT_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(|value| matches!(value, "1" | "true" | "yes"))
        .unwrap_or(false);
    let explicit_local_short_circuit = headers
        .get(USE_LOCAL_SHORT_CIRCUIT_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(|value| matches!(value, "1" | "true" | "yes"))
        .unwrap_or(false);
    let platform_api_key = if platform_allowed {
        shared
            .platform_api_key
            .lock()
            .map(|value| value.clone())
            .unwrap_or_default()
    } else {
        String::new()
    };
    let platform_authenticated = !platform_api_key.trim().is_empty();
    let local_request_allowed = !path.starts_with("/api/") && !skip_local_short_circuit;
    if local_request_allowed && crate::surface::resolve_api_route(method.as_str(), path, false)
        .is_some_and(|route| matches!(route.operation, crate::surface::ApiOperation::ChatCompletions
            | crate::surface::ApiOperation::Responses | crate::surface::ApiOperation::Messages
            | crate::surface::ApiOperation::GenerateContent | crate::surface::ApiOperation::StreamGenerateContent)) {
        if let Some(model) = request_model_from_body(&body)
            .or_else(|| requested_model_from_body_or_path("{}", path)) {
            for channel in config.channels.iter().filter(|channel| channel.enabled) {
                // Only real generation demand can schedule a paid due-group recheck;
                // browsing models, refreshing metadata and count_tokens cannot.
                crate::supplier::availability::allows(channel, &model);
            }
        }
    }
    let requires_local_backend = crate::surface::resolve_api_route(method.as_str(), path, false)
        .is_some_and(|route| crate::surface::api_operation_requires_local_backend(route.operation));
    let local_first = local_request_allowed
        && (requires_local_backend
            || explicit_local_short_circuit
            || config.prefer_local_supply
            || !platform_authenticated);
    let local_after_platform = local_request_allowed && platform_authenticated && !local_first;
    let model_control_route = crate::surface::resolve_api_route(method.as_str(), path, false)
        .filter(|route| {
            matches!(
                route.operation,
                crate::surface::ApiOperation::ListModels | crate::surface::ApiOperation::GetModel
            )
        });
    let is_model_control = model_control_route.is_some();
    let is_model_catalog = model_control_route
        .as_ref()
        .is_some_and(|route| route.operation == crate::surface::ApiOperation::ListModels);
    let compact_catalog = headers.get(crate::model_discovery::CATALOG_HEADER)
        .and_then(|value| value.to_str().ok()) != Some("full-v1");
    if is_model_catalog {
        // New servers return a small transport view; old servers ignore this
        // header and are compacted locally. Other operations are untouched.
        headers.insert(crate::model_discovery::CATALOG_HEADER, warp::http::HeaderValue::from_static(
            if compact_catalog { "compact-v1" } else { "full-v1" }
        ));
    }
    let mut local_model_entries = if is_model_control && local_request_allowed {
        let ready = ready_local_channel_ids.get_or_insert_with(|| shared.ready_local_channel_ids());
        local_model_entries(config, ready)
    } else {
        Vec::new()
    };
    if is_model_catalog && compact_catalog {
        // Collapse before surface rendering/merging to avoid cloning multi-MB
        // evidence arrays for each local+platform view.
        local_model_entries.iter_mut().for_each(crate::model_discovery::compact_entry);
    }
    if let Some(route) = model_control_route.as_ref() {
        let local_has_model = route.operation == crate::surface::ApiOperation::GetModel
            && local_model_for_route(config, route, &local_model_entries).is_some();
        if local_request_allowed && (!platform_authenticated || (local_first && local_has_model)) {
            return local_model_control_response(config, route, &local_model_entries, compact_catalog);
        }
    }

    if is_model_catalog && compact_catalog && platform_authenticated {
        let force_refresh = headers.get("cache-control").and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.split(',').any(|item| matches!(item.trim(), "no-cache" | "no-store" | "max-age=0")));
        if !force_refresh {
            if let Some(route) = model_control_route.as_ref() {
                if let Some(response) = shared.model_catalog_snapshot_response(route.surface, raw_query, true)? {
                    return merge_local_models_response(response, config, route.surface, &local_model_entries, local_first, true).await;
                }
            }
        }
    }

    if local_request_allowed
        && config.allow_model_equivalence
        && model_request_uses_fidelity_stages(&method, path, &body)
    {
        return forward_model_request_by_fidelity(
            method,
            path,
            raw_query,
            has_query,
            headers,
            body,
            config,
            shared,
            &platform_api_key,
            local_first,
        )
        .await;
    }

    let mut local_err: Option<anyhow::Error> = None;
    let mut local_server_response: Option<warp::reply::Response> = None;
    if local_first && !is_model_control {
        let ready = ready_local_channel_ids.get_or_insert_with(|| shared.ready_local_channel_ids());
        if let Some(result) = forward_ready_local_channel(
            config, path, raw_query, has_query, &method, &headers, &body, ready, shared,
        )
        .await
        {
            match result {
                Ok(resp) if !local_response_marks_channel_unready(resp.status()) => {
                    return Ok(resp);
                }
                Ok(resp) => {
                    if requires_local_backend || !platform_authenticated {
                        return Ok(resp);
                    }
                    local_server_response = Some(resp);
                }
                Err(err) => {
                    if requires_local_backend || !platform_authenticated {
                        return Err(err);
                    }
                    local_err = Some(err);
                }
            }
        }
        if requires_local_backend || !platform_authenticated {
            return Ok(error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                if requires_local_backend {
                    "this operation requires a ready local API or subscription channel"
                } else {
                    "no ready local channel matches the requested model and protocol"
                },
            ));
        }
    }

    let endpoint_count = config
        .endpoints
        .iter()
        .filter(|e| e.enabled && !e.base_url.trim().is_empty())
        .count();
    if endpoint_count == 0 {
        if local_after_platform && !is_model_control {
            let ready =
                ready_local_channel_ids.get_or_insert_with(|| shared.ready_local_channel_ids());
            if let Some(result) = forward_ready_local_channel(
                config, path, raw_query, has_query, &method, &headers, &body, ready, shared,
            )
            .await
            {
                return result;
            }
        }
        if let Some(response) = local_server_response {
            return Ok(response);
        }
        if let Some(route) = model_control_route
            .as_ref()
            .filter(|_| local_request_allowed)
        {
            return local_model_control_response(config, route, &local_model_entries, compact_catalog);
        }
        if let Some(err) = local_err {
            return Err(err);
        }
        return Err(anyhow!("no enabled endpoint"));
    }

    let start = *shared.active_index.lock().await % endpoint_count;
    let mut last_err: Option<anyhow::Error> = None;
    let mut last_server_response: Option<warp::reply::Response> = None;
    let mut model_catalog_transient_failure = false;
    let model_catalog_deadline = is_model_catalog
        .then(|| tokio::time::Instant::now() + shared.model_catalog_upstream_timeout);
    for (offset, ep) in config
        .endpoints
        .iter()
        .filter(|endpoint| endpoint.enabled && !endpoint.base_url.trim().is_empty())
        .cycle()
        .skip(start)
        .take(endpoint_count)
        .enumerate()
    {
        let idx = (start + offset) % endpoint_count;
        let forward = forward_once_preferred(
            method.clone(),
            path,
            raw_query,
            has_query,
            headers.clone(),
            body.clone(),
            &shared.platform_transport,
            config,
            ep,
            &platform_api_key,
            PlatformModelRouteMode::Configured,
            MODEL_REQUEST_MAX_UPSTREAM_ATTEMPTS,
        );
        let response = if let Some(deadline) = model_catalog_deadline {
            match tokio::time::timeout_at(deadline, forward).await {
                Ok(response) => response,
                Err(_) => {
                    model_catalog_transient_failure = true;
                    last_err = Some(anyhow!(
                        "model catalog upstream request timed out after {:?}",
                        shared.model_catalog_upstream_timeout
                    ));
                    break;
                }
            }
        } else {
            forward.await
        };
        if let Ok(response) = response.as_ref() {
            if is_model_catalog {
                if matches!(
                    response.status(),
                    StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
                ) {
                    if let Some(route) = model_control_route.as_ref() {
                        shared.clear_model_catalog_snapshot(
                            route.surface,
                            raw_query,
                            &platform_api_key,
                        );
                    }
                }
                model_catalog_transient_failure |=
                    model_catalog_response_is_transient(response.status());
            }
        }
        match response {
            Ok(resp) if is_model_control && !resp.status().is_success() => {
                last_server_response = Some(resp);
            }
            Ok(resp)
                if resp.status().is_server_error()
                    || (local_after_platform
                        && platform_response_allows_local_fallback(resp.status())) =>
            {
                last_server_response = Some(resp);
            }
            Ok(resp) => {
                *shared.active_index.lock().await = idx;
                let resp = if let Some(route) = model_control_route.as_ref().filter(|_| is_model_catalog && compact_catalog)
                {
                    remember_model_catalog_response(
                        resp,
                        shared,
                        route.surface,
                        raw_query,
                        &platform_api_key,
                    )
                    .await?
                } else {
                    resp
                };
                if is_model_catalog {
                    if let Some(route) = model_control_route.as_ref() {
                        return merge_local_models_response(
                            resp,
                            config,
                            route.surface,
                            &local_model_entries,
                            local_first,
                            compact_catalog,
                        )
                        .await;
                    }
                }
                return Ok(resp);
            }
            Err(err) if is_ambiguous_http3_request_error(&err) => {
                return Err(err);
            }
            Err(err) => {
                model_catalog_transient_failure |= is_model_catalog;
                last_err = Some(err);
            }
        }
    }
    if is_model_catalog && compact_catalog && model_catalog_transient_failure {
        if let Some(route) = model_control_route.as_ref() {
            if let Some(response) = shared.cached_model_catalog_response(
                route.surface,
                raw_query,
            )? {
                return merge_local_models_response(
                    response,
                    config,
                    route.surface,
                    &local_model_entries,
                    local_first,
                    true,
                )
                .await;
            }
        }
    }
    if local_after_platform && !is_model_control {
        let ready = ready_local_channel_ids.get_or_insert_with(|| shared.ready_local_channel_ids());
        if let Some(result) = forward_ready_local_channel(
            config, path, raw_query, has_query, &method, &headers, &body, ready, shared,
        )
        .await
        {
            return result;
        }
    }
    if let Some(response) = local_server_response {
        return Ok(response);
    }
    if let Some(route) = model_control_route
        .as_ref()
        .filter(|_| local_request_allowed)
    {
        return local_model_control_response(config, route, &local_model_entries, compact_catalog);
    }
    if let Some(err) = local_err {
        return Err(err);
    }
    if let Some(response) = last_server_response {
        return Ok(response);
    }
    Err(last_err.unwrap_or_else(|| anyhow!("all endpoints failed")))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ModelSupplyScope {
    Local,
    Platform,
}

enum ModelRouteAttempt {
	Unavailable,
	RetryResponse(warp::reply::Response),
	LocalOnlyResponse(warp::reply::Response),
	RetryError(anyhow::Error),
    CompleteResponse(warp::reply::Response),
    CompleteError(anyhow::Error),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LocalRouteRetryScope {
    None,
    Request,
    Model,
    Channel,
    Capacity,
}

struct LocalChannelExecution {
    channel_id: Option<String>,
    route_key: String,
    retry_scope: LocalRouteRetryScope,
    result: Result<warp::reply::Response>,
}

enum ModelRouteTerminalFailure {
    Response(warp::reply::Response),
    Error(anyhow::Error),
}

#[derive(Default)]
struct ModelRouteFailures {
    response: Option<(u8, warp::reply::Response)>,
    error: Option<anyhow::Error>,
    balance_terminal: Option<ModelRouteTerminalFailure>,
}

impl ModelRouteFailures {
    fn remember_response(
        &mut self,
        scope: ModelSupplyScope,
        response: warp::reply::Response,
    ) {
        let balance_rejection = scope == ModelSupplyScope::Platform
            && platform_response_is_balance_rejection(&response);
        if balance_rejection || self.balance_terminal.is_some() {
            // Once balance rejection enters the route sequence, it is final
            // only until another fallback is actually attempted. Replacing
            // this slot preserves both platform-first and local-first order.
            self.balance_terminal = Some(ModelRouteTerminalFailure::Response(response));
            return;
        }
        let priority = model_route_failure_priority(&response);
        if self
            .response
            .as_ref()
            .is_none_or(|(current, _)| priority > *current)
        {
            self.response = Some((priority, response));
        }
    }

    fn remember_error(&mut self, _scope: ModelSupplyScope, error: anyhow::Error) {
        if self.balance_terminal.is_some() {
            self.balance_terminal = Some(ModelRouteTerminalFailure::Error(error));
        } else {
            self.error = Some(error);
        }
    }

    fn finish(self) -> Result<warp::reply::Response> {
        if let Some(failure) = self.balance_terminal {
            return match failure {
                ModelRouteTerminalFailure::Response(response) => Ok(response),
                ModelRouteTerminalFailure::Error(error) => Err(error),
            };
        }
        if let Some((_, response)) = self.response {
            return Ok(response);
        }
        if let Some(error) = self.error {
            return Err(error);
        }
        Ok(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "no local or platform route matches the requested model and protocol",
        ))
    }
}

fn model_route_failure_priority(response: &warp::reply::Response) -> u8 {
	if platform_response_requires_local_only_fallback(response) {
		return 110;
	}
	let status = response.status();
    match status {
        StatusCode::PAYMENT_REQUIRED => 100,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => 90,
        StatusCode::LOCKED | StatusCode::TOO_MANY_REQUESTS => 80,
        StatusCode::NOT_FOUND => 70,
        StatusCode::REQUEST_TIMEOUT => 60,
        status if status.is_server_error() => 20,
        _ => 10,
    }
}

fn model_request_uses_fidelity_stages(
    method: &warp::http::Method,
    path: &str,
    body: &[u8],
) -> bool {
    let Some(route) = crate::surface::resolve_api_route(method.as_str(), path, false) else {
        return false;
    };
    if !matches!(
        route.operation,
        crate::surface::ApiOperation::ChatCompletions
            | crate::surface::ApiOperation::Responses
            | crate::surface::ApiOperation::Messages
            | crate::surface::ApiOperation::CountTokens
            | crate::surface::ApiOperation::GenerateContent
            | crate::surface::ApiOperation::StreamGenerateContent
            | crate::surface::ApiOperation::InteractionsCreate
    ) {
        return false;
    }
    let body = String::from_utf8_lossy(body);
    requested_model_from_body_or_path(&body, path).is_some_and(|model| !model.trim().is_empty())
}

fn local_model_route_affinity_key(
    path: &str,
    headers: &warp::http::HeaderMap,
    body: &[u8],
) -> Option<String> {
    let body_text = String::from_utf8_lossy(body);
    let requested_model = requested_model_from_body_or_path(&body_text, path)?;
    let session = subscription_request_cache_session(headers, &body_text);
    if let Some(session) = session {
        let digest = <sha2::Sha256 as sha2::Digest>::digest(session.as_bytes());
        return Some(format!(
            "{}\0cache-session:{}",
            normalize_model_name(&requested_model),
            hex::encode(&digest[..12])
        ));
    }
    if let Some(root) = crate::supplier::derived_subscription_cache_root(&body_text) {
        return Some(format!(
            "{}\0cache-root:{}",
            normalize_model_name(&requested_model),
            root
        ));
    }
    Some(format!(
        "{}\0{}",
        normalize_model_name(&requested_model),
        path.trim()
    ))
}

fn local_model_route_preference_context(
    config: &ClientConfig,
    platform_authenticated: bool,
    prefer_local: bool,
) -> LocalModelRoutePreferenceContext {
    let mut fingerprint = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&config.account_platform_id, &mut fingerprint);
    std::hash::Hash::hash(&config.account_user_id, &mut fingerprint);
    std::hash::Hash::hash(&config.registry_version, &mut fingerprint);
    if let Some(profile_key) = crate::model_compatibility::model_compatibility_profile_key(
        &config.account_platform_id,
        &config.account_user_id,
    ) {
        if let Some(profile) = config.model_compatibility_profiles.get(&profile_key) {
            std::hash::Hash::hash(&profile.base_release_id, &mut fingerprint);
            std::hash::Hash::hash(&profile.model_version_id, &mut fingerprint);
            std::hash::Hash::hash(&profile.compatibility_version_id, &mut fingerprint);
            std::hash::Hash::hash(&profile.revision, &mut fingerprint);
            for group in &profile.model_groups {
                std::hash::Hash::hash(&group.id, &mut fingerprint);
                std::hash::Hash::hash(&group.aliases, &mut fingerprint);
                std::hash::Hash::hash(&group.models, &mut fingerprint);
                std::hash::Hash::hash(&group.match_models, &mut fingerprint);
                std::hash::Hash::hash(&group.disabled, &mut fingerprint);
            }
        }
    }
    for endpoint in &config.endpoints {
        std::hash::Hash::hash(&endpoint.base_url, &mut fingerprint);
        std::hash::Hash::hash(&endpoint.enabled, &mut fingerprint);
    }
    for channel in &config.channels {
        std::hash::Hash::hash(&channel.id, &mut fingerprint);
        std::hash::Hash::hash(&channel.enabled, &mut fingerprint);
        std::hash::Hash::hash(&channel.source_driver(), &mut fingerprint);
        std::hash::Hash::hash(&channel.api_format, &mut fingerprint);
        std::hash::Hash::hash(&channel.public_model, &mut fingerprint);
        std::hash::Hash::hash(&channel.upstream_model, &mut fingerprint);
        std::hash::Hash::hash(&channel.models, &mut fingerprint);
        std::hash::Hash::hash(&channel.supported_protocols, &mut fingerprint);
    }
    LocalModelRoutePreferenceContext {
        platform_authenticated,
        prefer_local,
        policy_fingerprint: std::hash::Hasher::finish(&fingerprint),
    }
}

fn local_model_failure_evidence_from_payload(
    payload: &serde_json::Map<String, serde_json::Value>,
) -> Option<LocalModelRouteFailureEvidence> {
    let soft = payload
        .get("model_failure_soft")
        .and_then(serde_json::Value::as_bool)
        == Some(true);
    let model_scoped = payload
        .get("failure_scope")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|scope| {
            scope.eq_ignore_ascii_case("model")
                || (soft && scope.eq_ignore_ascii_case("request"))
        });
    if !model_scoped {
        return None;
    }
    let failure_model = payload
        .get("failure_model")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())?
        .to_string();
    Some(LocalModelRouteFailureEvidence {
        failure_model,
        error_kind: payload
            .get("error_kind")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|kind| !kind.is_empty())
            .unwrap_or("model_route_failure")
            .to_string(),
        retry_after_seconds: payload
            .get("retry_after_seconds")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or_default()
            .max(0),
        soft,
    })
}

fn attach_local_model_failure_evidence(
    response: &mut warp::reply::Response,
    payload: &serde_json::Map<String, serde_json::Value>,
) {
    if payload
        .get("error_kind")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|kind| kind.trim().eq_ignore_ascii_case("concurrency_full"))
    {
        response
            .extensions_mut()
            .insert(LocalAdmissionFailure::ConcurrencyFull);
    }
    if let Some(evidence) = local_model_failure_evidence_from_payload(payload) {
        response.extensions_mut().insert(evidence);
        return;
    }
    match payload
        .get("failure_scope")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("channel") => {
            response
                .extensions_mut()
                .insert(LocalDeclaredFailureScope::Channel);
        }
        Some("request" | "operation" | "model") => {
            // A malformed model declaration is diagnostic; it must not widen
            // into channel health merely because its model identity is absent
            // or mismatched.
            response
                .extensions_mut()
                .insert(LocalDeclaredFailureScope::Request);
        }
        _ => {}
    }
}

fn local_response_upstream_status(response: &warp::reply::Response) -> StatusCode {
    response
        .headers()
        .get("x-const-api-upstream-status")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u16>().ok())
        .and_then(|value| StatusCode::from_u16(value).ok())
        .unwrap_or_else(|| response.status())
}

async fn forward_model_request_by_fidelity(
    method: warp::http::Method,
    path: &str,
    raw_query: &str,
    has_query: bool,
    headers: warp::http::HeaderMap,
    body: bytes::Bytes,
    config: &ClientConfig,
    shared: &Arc<ProxyShared>,
    platform_api_key: &str,
    prefer_local: bool,
) -> Result<warp::reply::Response> {
    let scopes = if prefer_local {
        [ModelSupplyScope::Local, ModelSupplyScope::Platform]
    } else {
        [ModelSupplyScope::Platform, ModelSupplyScope::Local]
    };
	let mut failures = ModelRouteFailures::default();
    let mut platform_routes_blocked = false;
    let mut budget = UpstreamAttemptBudget::default();
    let mut attempted_local_routes = HashSet::new();
    let mut capacity_excluded_local_channels = HashSet::new();
    let affinity_key = local_model_route_affinity_key(path, &headers, &body).unwrap_or_default();
    let preference_context = local_model_route_preference_context(
        config,
        !platform_api_key.trim().is_empty(),
        prefer_local,
    );
    let preferred_local_route =
        shared.preferred_local_model_route(&affinity_key, preference_context, now_unix());
    let mut preferred_local_route_attempted = false;

    for match_mode in [ModelMatchMode::ExactOnly, ModelMatchMode::CompatibleOnly] {
		for scope in scopes {
			if matches!(scope, ModelSupplyScope::Platform) && platform_routes_blocked {
				continue;
			}
            if !budget.available() {
                return failures.finish();
            }
            if scope == ModelSupplyScope::Local && !preferred_local_route_attempted {
                preferred_local_route_attempted = true;
                // Keep a stable session on its last successful local channel
                // for cache affinity, but only when routing reaches the local
                // supply stage selected by `prefer_local`. A sticky route must
                // not silently turn platform-first back into local-first.
                if let Some(preferred_route) = preferred_local_route.as_deref() {
                    let preferred_attempt = attempt_local_model_route(
                        config,
                        path,
                        raw_query,
                        has_query,
                        &method,
                        &headers,
                        &body,
                        shared,
                        ModelMatchMode::Configured,
                        Some(preferred_route),
                        &affinity_key,
                        preference_context,
                        &mut attempted_local_routes,
                        &mut capacity_excluded_local_channels,
                        &mut budget,
                    )
                    .await;
                    if let Some(result) = apply_model_route_attempt(
                        preferred_attempt,
                        ModelSupplyScope::Local,
                        &mut failures,
                    ) {
                        return result;
                    }
                    if !budget.available() {
                        return failures.finish();
                    }
                }
            }
            let attempt = match scope {
                ModelSupplyScope::Local => {
                    attempt_local_model_route(
                        config,
                        path,
                        raw_query,
                        has_query,
                        &method,
                        &headers,
                        &body,
                        shared,
                        match_mode,
                        None,
                        &affinity_key,
                        preference_context,
                        &mut attempted_local_routes,
                        &mut capacity_excluded_local_channels,
                        &mut budget,
                    )
                    .await
                }
                ModelSupplyScope::Platform => {
                    let platform_mode = match match_mode {
                        ModelMatchMode::ExactOnly => PlatformModelRouteMode::ExactOnly,
                        ModelMatchMode::CompatibleOnly => PlatformModelRouteMode::CompatibleOnly,
                        ModelMatchMode::Configured => PlatformModelRouteMode::Configured,
                    };
                    attempt_platform_model_route(
                        method.clone(),
                        path,
                        raw_query,
                        has_query,
                        headers.clone(),
                        body.clone(),
                        config,
                        shared,
                        platform_api_key,
                        platform_mode,
                        &mut budget,
                    )
                    .await
                }
            };
            if matches!(scope, ModelSupplyScope::Platform)
                && matches!(
                    &attempt,
                    ModelRouteAttempt::CompleteResponse(response) if response.status().is_success()
                )
            {
                if let Some(preferred_route) = preferred_local_route.as_deref() {
                    shared.forget_local_model_route(&affinity_key, preferred_route);
                }
            }
			let attempt = match attempt {
				ModelRouteAttempt::LocalOnlyResponse(response) => {
					failures.remember_response(ModelSupplyScope::Platform, response);
					platform_routes_blocked = true;
					continue;
				}
				other => other,
			};
			if let Some(result) = apply_model_route_attempt(attempt, scope, &mut failures) {
                return result;
            }
        }
    }
    failures.finish()
}

fn apply_model_route_attempt(
    attempt: ModelRouteAttempt,
    scope: ModelSupplyScope,
    failures: &mut ModelRouteFailures,
) -> Option<Result<warp::reply::Response>> {
    match attempt {
        ModelRouteAttempt::Unavailable => None,
		ModelRouteAttempt::RetryResponse(response) => {
			failures.remember_response(scope, response);
			None
		}
		ModelRouteAttempt::LocalOnlyResponse(response) => {
			failures.remember_response(scope, response);
			None
		}
        ModelRouteAttempt::RetryError(error) => {
            failures.remember_error(scope, error);
            None
        }
        ModelRouteAttempt::CompleteResponse(response) => Some(Ok(response)),
        ModelRouteAttempt::CompleteError(error) => Some(Err(error)),
    }
}

async fn attempt_local_model_route(
    config: &ClientConfig,
    path: &str,
    raw_query: &str,
    has_query: bool,
    method: &warp::http::Method,
    headers: &warp::http::HeaderMap,
    body: &bytes::Bytes,
    shared: &Arc<ProxyShared>,
    match_mode: ModelMatchMode,
    required_route_key: Option<&str>,
    affinity_key: &str,
    preference_context: LocalModelRoutePreferenceContext,
    attempted_route_keys: &mut HashSet<String>,
    capacity_excluded_channel_ids: &mut HashSet<String>,
    budget: &mut UpstreamAttemptBudget,
) -> ModelRouteAttempt {
    let mut last_response = None;
    let mut last_error = None;
    while budget.available() {
        let ready = shared.ready_local_channel_ids();
        let Some(execution) = forward_ready_local_channel_with_match(
            config,
            path,
            raw_query,
            has_query,
            method,
            headers,
            body,
            &ready,
            shared,
            match_mode,
            required_route_key,
            attempted_route_keys,
            capacity_excluded_channel_ids,
        )
        .await
        else {
            break;
        };
        if execution.route_key.is_empty() {
            return match execution.result {
                Ok(response) => ModelRouteAttempt::CompleteResponse(response),
                Err(error) => ModelRouteAttempt::CompleteError(error),
            };
        }
        budget.consume(1);
        attempted_route_keys.insert(execution.route_key.clone());
        if execution.retry_scope == LocalRouteRetryScope::Capacity {
            if let Some(channel_id) = execution.channel_id.as_ref() {
                capacity_excluded_channel_ids.insert(channel_id.clone());
            }
        }
        match execution.result {
            Ok(response) if execution.retry_scope != LocalRouteRetryScope::None => {
                if execution.retry_scope != LocalRouteRetryScope::Capacity {
                    shared.forget_local_model_route(affinity_key, &execution.route_key);
                }
                last_response = Some(response);
            }
            Ok(response) => {
                if response.status().is_success() {
                    shared.promote_local_model_route(
                        affinity_key,
                        &execution.route_key,
                        preference_context,
                        now_unix(),
                    );
                }
                return ModelRouteAttempt::CompleteResponse(response);
            }
            Err(error) if execution.retry_scope != LocalRouteRetryScope::None => {
                shared.forget_local_model_route(affinity_key, &execution.route_key);
                last_error = Some(error);
            }
            Err(error) => return ModelRouteAttempt::CompleteError(error),
        }
        if required_route_key.is_some() {
            break;
        }
    }
    if let Some(response) = last_response {
        ModelRouteAttempt::RetryResponse(response)
    } else if let Some(error) = last_error {
        ModelRouteAttempt::RetryError(error)
    } else {
        ModelRouteAttempt::Unavailable
    }
}

async fn attempt_platform_model_route(
    method: warp::http::Method,
    path: &str,
    raw_query: &str,
    has_query: bool,
    headers: warp::http::HeaderMap,
    body: bytes::Bytes,
    config: &ClientConfig,
    shared: &Arc<ProxyShared>,
    platform_api_key: &str,
    model_route_mode: PlatformModelRouteMode,
    budget: &mut UpstreamAttemptBudget,
) -> ModelRouteAttempt {
    // Platform endpoints require the runtime-authorized device credential. A
    // configured endpoint may remain in the local config after logout; sending
    // an empty credential there would turn a local-only compatibility request
    // into a terminal 401 before ready local channels are considered.
    if platform_api_key.trim().is_empty() {
        return ModelRouteAttempt::Unavailable;
    }
    let endpoint_count = config
        .endpoints
        .iter()
        .filter(|endpoint| endpoint.enabled && !endpoint.base_url.trim().is_empty())
        .count();
    if endpoint_count == 0 {
        return ModelRouteAttempt::Unavailable;
    }

    let start = *shared.active_index.lock().await % endpoint_count;
    let mut last_error = None;
    let mut last_response = None;
    for (offset, endpoint) in config
        .endpoints
        .iter()
        .filter(|endpoint| endpoint.enabled && !endpoint.base_url.trim().is_empty())
        .cycle()
        .skip(start)
        .take(endpoint_count)
        .enumerate()
    {
        if !budget.available() {
            break;
        }
        let index = (start + offset) % endpoint_count;
        match forward_once_preferred(
            method.clone(),
            path,
            raw_query,
            has_query,
            headers.clone(),
            body.clone(),
            &shared.platform_transport,
            config,
            endpoint,
            platform_api_key,
            model_route_mode,
            budget.remaining,
        )
        .await
        {
            Ok(response) if response.status().is_server_error() => {
                budget.consume(platform_response_upstream_attempts(&response));
                match classify_platform_model_response(response) {
                    ModelRouteAttempt::RetryResponse(response) => last_response = Some(response),
                    complete => return complete,
                }
            }
            Ok(response) => {
                budget.consume(platform_response_upstream_attempts(&response));
                *shared.active_index.lock().await = index;
                return classify_platform_model_response(response);
            }
            Err(error) if is_ambiguous_http3_request_error(&error) => {
                budget.consume(1);
                return ModelRouteAttempt::CompleteError(error);
            }
            Err(error) => {
                budget.consume(1);
                last_error = Some(error);
            }
        }
    }
    if let Some(response) = last_response {
        return classify_platform_model_response(response);
    }
    last_error.map_or(
        ModelRouteAttempt::Unavailable,
        ModelRouteAttempt::RetryError,
    )
}

fn platform_response_upstream_attempts(response: &warp::reply::Response) -> usize {
    response
        .headers()
        .get(PLATFORM_UPSTREAM_ATTEMPTS_USED_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<usize>().ok())
        .map(|value| value.min(MODEL_REQUEST_MAX_UPSTREAM_ATTEMPTS))
        // Older servers do not report this header. Counting one is
        // conservative and prevents a mixed-version deployment from
        // multiplying the retry budget.
        .unwrap_or(1)
}

fn classify_platform_model_response(response: warp::reply::Response) -> ModelRouteAttempt {
	crate::client_toasts::record_platform_access_notice(&response);
	if platform_response_blocks_remaining_platform_routes(&response) {
		ModelRouteAttempt::LocalOnlyResponse(response)
	} else if platform_response_allows_model_stage_fallback(&response) {
        ModelRouteAttempt::RetryResponse(response)
    } else {
        ModelRouteAttempt::CompleteResponse(response)
    }
}

fn platform_response_blocks_remaining_platform_routes(
    response: &warp::reply::Response,
) -> bool {
    platform_response_requires_local_only_fallback(response)
        || platform_response_is_balance_rejection(response)
}

fn platform_response_is_balance_rejection(response: &warp::reply::Response) -> bool {
    if response.status() == StatusCode::PAYMENT_REQUIRED {
        return true;
    }
    response
        .headers()
        .get(PLATFORM_ERROR_CODE_HEADER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("insufficient_balance"))
}

fn platform_response_requires_local_only_fallback(response: &warp::reply::Response) -> bool {
    if response
        .headers()
        .get(PLATFORM_MODEL_FALLBACK_SCOPE_HEADER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .trim()
                .eq_ignore_ascii_case(PLATFORM_MODEL_FALLBACK_SCOPE_LOCAL_ONLY)
        })
    {
        return true;
    }
    let code = response
        .headers()
        .get(PLATFORM_ERROR_CODE_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase();
    code == "cyber_policy"
        || code == "content_policy_violation"
        || code == "platform_access_restricted"
        || code.starts_with("anthropic_refusal_")
        || matches!(
            code.as_str(),
            "gemini_prohibited_content"
                | "gemini_image_prohibited_content"
                | "gemini_escalation"
                | "gemini_jailbreak"
        )
}

fn platform_response_allows_model_stage_fallback(response: &warp::reply::Response) -> bool {
    let truthy = |name: &'static str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| matches!(value.trim(), "1" | "true" | "yes"))
    };
    if truthy(PLATFORM_MODEL_FALLBACK_SAFE_HEADER) {
        return true;
    }
    // A 402 is a pre-execution billing rejection, so replaying the request on
    // an available local route cannot duplicate model execution. This also
    // keeps mixed-version servers from leaking an intermediate platform balance
    // error to the calling tool when they omit the newer fallback-safe header.
    if platform_response_is_balance_rejection(response) {
        return true;
    }
    // Older servers do not echo the applied mode or the explicit safety bit.
    // Preserve their status-based fallback behavior during a server-first rollout.
    if response
        .headers()
        .get(PLATFORM_MODEL_ROUTE_MODE_APPLIED_HEADER)
        .is_none()
    {
        return platform_response_allows_local_fallback(response.status());
    }
    false
}

async fn forward_ready_local_channel(
    config: &ClientConfig,
    path: &str,
    raw_query: &str,
    has_query: bool,
    method: &warp::http::Method,
    headers: &warp::http::HeaderMap,
    body: &bytes::Bytes,
    ready_local_channel_ids: &HashSet<String>,
    shared: &Arc<ProxyShared>,
) -> Option<Result<warp::reply::Response>> {
    let excluded_routes = HashSet::new();
    let excluded_channels = HashSet::new();
    forward_ready_local_channel_with_match(
        config,
        path,
        raw_query,
        has_query,
        method,
        headers,
        body,
        ready_local_channel_ids,
        shared,
        ModelMatchMode::Configured,
        None,
        &excluded_routes,
        &excluded_channels,
    )
    .await
    .map(|execution| execution.result)
}

async fn forward_ready_local_channel_with_match(
    config: &ClientConfig,
    path: &str,
    raw_query: &str,
    has_query: bool,
    method: &warp::http::Method,
    headers: &warp::http::HeaderMap,
    body: &bytes::Bytes,
    ready_local_channel_ids: &HashSet<String>,
    shared: &Arc<ProxyShared>,
    match_mode: ModelMatchMode,
    required_route_key: Option<&str>,
    excluded_route_keys: &HashSet<String>,
    excluded_channel_ids: &HashSet<String>,
) -> Option<LocalChannelExecution> {
    let mut claim_excluded = excluded_route_keys.clone();
    let (selection, route_key, resource_owned) = loop {
        let route = crate::surface::resolve_api_route(method.as_str(), path, false);
        let owned = route
            .as_ref()
            .filter(|_| match_mode != ModelMatchMode::CompatibleOnly)
            .map(|route| {
                select_resource_owned_local_channel(
                    &shared.local_resource_owners,
                    config,
                    route,
                    path,
                    body,
                    ready_local_channel_ids,
                )
            });
        let (selection, resource_owned) = match owned {
            Some(Ok(Some(selection))) => (Some(selection), true),
            Some(Err(error)) => {
                return Some(LocalChannelExecution {
                    channel_id: None,
                    route_key: String::new(),
                    retry_scope: LocalRouteRetryScope::None,
                    result: local_surface_error_response(
                        error.surface,
                        error.status,
                        error.message,
                    ),
                });
            }
            _ if let Ok(model_quota_routes) = shared.local_model_quota_routes.lock() => (
                select_ready_local_channel_with_model_route_policy(
                    config,
                    method,
                    path,
                    body,
                    ready_local_channel_ids,
                    &model_quota_routes,
                    now_unix(),
                    match_mode,
                    required_route_key,
                    &claim_excluded,
                    excluded_channel_ids,
                ),
                false,
            ),
            _ => (
                select_ready_local_channel_with_model_route_policy(
                    config,
                    method,
                    path,
                    body,
                    ready_local_channel_ids,
                    &HashMap::new(),
                    now_unix(),
                    match_mode,
                    required_route_key,
                    &claim_excluded,
                    excluded_channel_ids,
                ),
                false,
            ),
        };
        let selection = selection?;
        let route_key = local_model_route_key(selection.channel, &selection.route_model);
        if shared.claim_local_model_route(&route_key, now_unix()) {
            break (selection, route_key, resource_owned);
        }
        claim_excluded.insert(route_key);
    };
    let channel = selection.channel;
    let channel_id = channel.id.clone();
    let channel_name = channel.name.clone();
    let resource_route_model = selection.route_model.clone();
    let route_upstream_model =
        channel_upstream_model_for_request(channel, Some(&selection.route_model));
    let selected_upstream_model = selection.substituted.then(|| route_upstream_model.clone());
    let api_route = crate::surface::resolve_api_route(method.as_str(), path, false);
    let model_operation = api_route
        .as_ref()
        .is_some_and(|route| local_operation_proves_model_health(route.operation));
    let mut model_observation = model_operation
        .then(|| LocalModelOutcomeGuard::new(shared.clone(), &channel_id, &route_upstream_model));
    let result = forward_local_channel_once_with_query_presence(
        method.clone(),
        path,
        raw_query,
        has_query,
        headers.clone(),
        body.clone(),
        &shared.client,
        config,
        channel,
        selected_upstream_model.as_deref(),
    )
    .await;

    Some(match result {
        Ok(response) => {
            let response = match crate::surface::resolve_api_route(method.as_str(), path, false) {
                Some(route) => {
                    match capture_local_resource_owner_response(
                        shared.local_resource_owners.clone(),
                        route,
                        method.as_str(),
                        path,
                        &channel_id,
                        &resource_route_model,
                        response,
                    )
                    .await
                    {
                        Ok(response) => response,
                        Err(error) => {
                            shared.release_local_model_route_probe(&route_key);
                            return Some(LocalChannelExecution {
                                channel_id: Some(channel_id),
                                route_key,
                                retry_scope: LocalRouteRetryScope::None,
                                result: Err(error),
                            });
                        }
                    }
                }
                None => response,
            };
            let status = local_response_upstream_status(&response);
            let evidence = response
                .extensions()
                .get::<LocalModelRouteFailureEvidence>()
                .cloned();
            let declared_scope = response
                .extensions()
                .get::<LocalDeclaredFailureScope>()
                .copied();
            let capacity_scoped = response
                .extensions()
                .get::<LocalAdmissionFailure>()
                .is_some_and(|failure| *failure == LocalAdmissionFailure::ConcurrencyFull);
            let model_scoped = model_operation
                && shared.record_local_model_response(
                    &channel_id,
                    &route_upstream_model,
                    status,
                    response.headers(),
                    evidence.as_ref(),
                );
            if model_scoped {
                let error_kind = evidence
                    .as_ref()
                    .map(|evidence| evidence.error_kind.as_str())
                    .unwrap_or("model_route_unavailable");
                let retry_after_seconds = evidence
                    .as_ref()
                    .map(|evidence| evidence.retry_after_seconds)
                    .unwrap_or_default();
                log::info!(
                    "[const-api][model-health] channel={} model={} status={} kind={} retry_after_seconds={}",
                    channel_id,
                    route_upstream_model,
                    status.as_u16(),
                    error_kind,
                    retry_after_seconds
                );
            }
            let channel_scoped = !model_scoped
                && (declared_scope == Some(LocalDeclaredFailureScope::Channel)
                    || (evidence.is_none()
                        && declared_scope != Some(LocalDeclaredFailureScope::Request)
                        && local_response_marks_channel_unready_for_request(
                            method.as_str(),
                            path,
                            status,
                        )));
            let request_scoped = declared_scope == Some(LocalDeclaredFailureScope::Request)
                || (!model_scoped
                    && local_response_allows_request_retry(method.as_str(), path, status));
            if model_observation.is_none() && !model_scoped {
                shared.release_local_model_route_probe(&route_key);
            }
            if let Some(ready) =
                local_channel_readiness_observation(status, model_scoped, channel_scoped)
            {
                shared.set_local_channel_ready(&channel_id, ready);
            }
            let retry_scope = if status.is_success() {
                LocalRouteRetryScope::None
            } else if capacity_scoped && resource_owned {
                // Resource lifecycle requests cannot move to another account.
                // Return the owner's capacity failure without replaying it.
                LocalRouteRetryScope::None
            } else if capacity_scoped {
                LocalRouteRetryScope::Capacity
            } else if model_scoped {
                LocalRouteRetryScope::Model
            } else if channel_scoped {
                LocalRouteRetryScope::Channel
            } else if request_scoped {
                LocalRouteRetryScope::Request
            } else {
                LocalRouteRetryScope::None
            };
            let response = if status.is_success() {
                match model_observation.take() {
                    Some(observation) => observe_local_model_response(
                        response,
                        observation,
                        api_route.and_then(|route| route.protocol),
                    ),
                    None => response,
                }
            } else {
                response
            };
            LocalChannelExecution {
                channel_id: Some(channel_id),
                route_key,
                retry_scope,
                result: Ok(response),
            }
        }
        Err(err) => {
            if model_observation.is_none() {
                shared.release_local_model_route_probe(&route_key);
            }
            let route_mismatch =
                crate::supplier::structured_capability_mismatch(&err.to_string()).is_some();
            let retryable = openai_subscription_image_operation(path).is_none()
                && local_execution_error_marks_channel_unready(&err);
            if retryable {
                shared.set_local_channel_ready(&channel_id, false);
            }
            let result = if route_mismatch {
                Err(err)
            } else {
                Err(anyhow!("local channel {channel_name} failed: {err}"))
            };
            LocalChannelExecution {
                channel_id: Some(channel_id),
                route_key,
                retry_scope: if retryable {
                    LocalRouteRetryScope::Channel
                } else if route_mismatch {
                    // Conversion/state incompatibility says nothing about channel or model
                    // health. It may, however, be resolved by the next eligible route (normally
                    // the sticky origin), within the shared three-attempt budget.
                    LocalRouteRetryScope::Request
                } else {
                    LocalRouteRetryScope::None
                },
                result,
            }
        }
    })
}

fn proxy_execution_error_response(error: &anyhow::Error) -> warp::reply::Response {
    if crate::upstream_transport::is_ambiguous_transport_error(error) {
        let payload = serde_json::json!({
            "error_kind": "ambiguous_transport",
            "safe_to_retry_other_channel": false,
            "safe_to_retry_same_channel": false,
            "error": {
                "message": "upstream request outcome is unknown; automatic replay was refused",
                "type": "ambiguous_transport",
                "code": "ambiguous_transport",
            }
        });
        let mut response = warp::reply::with_status(
            warp::reply::json(&payload),
            StatusCode::BAD_GATEWAY,
        )
        .into_response();
        response.headers_mut().insert(
            "x-const-error-code",
            warp::http::HeaderValue::from_static("ambiguous_transport"),
        );
        return response;
    }
    if let Some(payload) = crate::supplier::structured_capability_mismatch(&error.to_string()) {
        let error_code = payload
            .get("error_kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("capability_mismatch")
            .to_string();
        let mut response = warp::reply::with_status(
            warp::reply::json(&serde_json::Value::Object(payload)),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .into_response();
        if let Ok(value) = warp::http::HeaderValue::from_str(&error_code) {
            response.headers_mut().insert("x-const-error-code", value);
        }
        return response;
    }
    error_response(StatusCode::BAD_GATEWAY, &error.to_string())
}

#[derive(Clone, Debug)]
enum LocalProxyRequestIdentity {
    Owner { api_key: String },
    LanShareMember {
        api_key: String,
        member: LanShareMember,
    },
}

impl LocalProxyRequestIdentity {
    fn api_key(&self) -> &str {
        match self {
            Self::Owner { api_key } | Self::LanShareMember { api_key, .. } => api_key,
        }
    }
}

fn local_proxy_request_identity(
    path: &str,
    raw_query: &str,
    headers: &warp::http::HeaderMap,
    cfg: &ClientConfig,
) -> Result<Option<LocalProxyRequestIdentity>> {
    let owner_api_key = cfg.api_key.trim();
    if !owner_api_key.is_empty()
        && local_proxy_key_presented(path, raw_query, headers, owner_api_key)
    {
        return Ok(Some(LocalProxyRequestIdentity::Owner {
            api_key: owner_api_key.to_string(),
        }));
    }
    if !cfg.allow_lan_access {
        return Ok(None);
    }
    let members = crate::lan_share::lan_share_members()?;
    Ok(local_proxy_request_identity_with_members(
        path, raw_query, headers, cfg, &members,
    ))
}

fn local_proxy_request_identity_with_members(
    path: &str,
    raw_query: &str,
    headers: &warp::http::HeaderMap,
    cfg: &ClientConfig,
    members: &[LanShareMember],
) -> Option<LocalProxyRequestIdentity> {
    let api_key = cfg.api_key.trim();
    if !api_key.is_empty() && local_proxy_key_presented(path, raw_query, headers, api_key) {
        return Some(LocalProxyRequestIdentity::Owner {
            api_key: api_key.to_string(),
        });
    }
    if !cfg.allow_lan_access {
        return None;
    }
    members
        .iter()
        .filter(|member| member.api_key != api_key)
        .find_map(|member| {
            local_proxy_key_presented(path, raw_query, headers, &member.api_key).then(|| {
                LocalProxyRequestIdentity::LanShareMember {
                    api_key: member.api_key.clone(),
                    member: member.clone(),
                }
            })
        })
}

fn local_proxy_key_presented(
    path: &str,
    raw_query: &str,
    headers: &warp::http::HeaderMap,
    api_key: &str,
) -> bool {
    if api_key.trim().is_empty() {
        return false;
    }
    if local_proxy_header_authorized(headers, api_key) {
        return true;
    }
    if crate::surface::surface_from_path(path) != Some(crate::surface::ApiSurface::Gemini) {
        return false;
    }
    reqwest::Url::parse(&format!("http://localhost/?{raw_query}"))
        .ok()
        .is_some_and(|url| {
            url.query_pairs()
                .any(|(name, value)| name == "key" && value == api_key)
        })
}

pub(crate) fn local_proxy_request_authorized(
    path: &str,
    raw_query: &str,
    headers: &warp::http::HeaderMap,
    cfg: &ClientConfig,
) -> bool {
    path == "/healthz"
        || !is_local_proxy_api_path(path)
        || local_proxy_key_presented(path, raw_query, headers, cfg.api_key.trim())
}

pub(crate) fn strip_local_auth_query(
    path: &str,
    raw_query: &str,
    headers: &warp::http::HeaderMap,
    cfg: &ClientConfig,
) -> String {
    strip_local_auth_query_for_key(path, raw_query, headers, cfg.api_key.trim())
}

fn strip_local_auth_query_for_key(
    path: &str,
    raw_query: &str,
    headers: &warp::http::HeaderMap,
    api_key: &str,
) -> String {
    if crate::surface::surface_from_path(path) != Some(crate::surface::ApiSurface::Gemini) {
        return raw_query.to_string();
    }
    if api_key.is_empty() || local_proxy_header_authorized(headers, api_key) {
        return raw_query.to_string();
    }
    remove_exact_local_auth_segment(raw_query, api_key)
}

fn lan_share_error_response(
    status: StatusCode,
    message: &str,
    code: &'static str,
) -> warp::reply::Response {
    let mut response = error_response(status, message);
    response.headers_mut().insert(
        "x-const-error-code",
        warp::http::HeaderValue::from_static(code),
    );
    response
}

fn local_proxy_header_authorized(headers: &warp::http::HeaderMap, api_key: &str) -> bool {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().strip_prefix("Bearer "))
        .is_some_and(|value| value.trim() == api_key)
        || ["x-api-key", "api-key", "x-goog-api-key"]
            .into_iter()
            .any(|name| {
                headers
                    .get(name)
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| value.trim() == api_key)
            })
}

fn remove_exact_local_auth_segment(raw_query: &str, api_key: &str) -> String {
    raw_query
        .split('&')
        .filter(|segment| !raw_query_segment_is_local_auth(segment, api_key))
        .collect::<Vec<_>>()
        .join("&")
}

fn raw_query_segment_is_local_auth(segment: &str, api_key: &str) -> bool {
    reqwest::Url::parse(&format!("http://localhost/?{segment}"))
        .ok()
        .and_then(|url| {
            url.query_pairs()
                .next()
                .map(|(name, value)| name == "key" && value == api_key)
        })
        .unwrap_or(false)
}

fn secure_surface_index_response(mut response: warp::reply::Response) -> warp::reply::Response {
    let headers = response.headers_mut();
    headers.insert(
        "cache-control",
        warp::http::HeaderValue::from_static("no-store"),
    );
    headers.insert(
        "content-security-policy",
        warp::http::HeaderValue::from_static(
            "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; frame-ancestors 'none'",
        ),
    );
    headers.insert(
        "x-content-type-options",
        warp::http::HeaderValue::from_static("nosniff"),
    );
    headers.insert("vary", warp::http::HeaderValue::from_static("Accept"));
    response
}

pub(crate) fn is_local_proxy_api_path(path: &str) -> bool {
    crate::surface::surface_from_path(path).is_some() || path == "/api" || path.starts_with("/api/")
}

pub(crate) async fn append_raw_proxy_exchange_log_and_rebuild_response(
    method: &warp::http::Method,
    path: &str,
    raw_query: &str,
    request_headers: &warp::http::HeaderMap,
    request_body: &[u8],
    response: warp::reply::Response,
) -> warp::reply::Response {
    if !raw_debug_logs_enabled() {
        return response;
    }
    let request_id = format!("raw-{}-{:016x}", now_unix(), rand::random::<u64>());
    let status = response.status();
    let response_headers = response.headers().clone();
    let _ = append_raw_proxy_request_log(
        &request_id,
        method,
        path,
        raw_query,
        request_headers,
        request_body,
        status,
        &response_headers,
    );
    let mut builder = warp::http::Response::builder().status(status);
    for (key, value) in response_headers.iter() {
        if key.as_str().eq_ignore_ascii_case("content-length")
            || key.as_str().eq_ignore_ascii_case("content-encoding")
        {
            continue;
        }
        if let Ok(value_str) = value.to_str() {
            builder = builder.header(key.as_str(), value_str);
        }
    }
    let chunk_request_id = request_id.clone();
    let body = response
        .into_body()
        .into_data_stream()
        .map_ok(move |chunk| {
            let _ = append_raw_proxy_response_chunk_log(&chunk_request_id, &chunk);
            chunk
        });
    let body = warp::reply::stream(body).into_response().into_body();
    builder
        .body(body)
        .unwrap_or_else(|err| error_response(StatusCode::BAD_GATEWAY, &err.to_string()))
}

pub(crate) fn append_raw_proxy_request_log(
    request_id: &str,
    method: &warp::http::Method,
    path: &str,
    raw_query: &str,
    request_headers: &warp::http::HeaderMap,
    request_body: &[u8],
    response_status: warp::http::StatusCode,
    response_headers: &warp::http::HeaderMap,
) -> Result<()> {
    let record = serde_json::json!({
        "source": "proxy_exchange",
        "event": "request",
        "request_id": request_id,
        "created_at_unix": now_unix(),
        "method": method.as_str(),
        "path": path,
        "query": raw_query,
        "request": {
            "headers": raw_proxy_headers(request_headers),
            "body": String::from_utf8_lossy(request_body),
            "bytes": request_body.len()
        },
        "response": {
            "status": response_status.as_u16(),
            "headers": raw_proxy_headers(response_headers)
        }
    });
    append_raw_proxy_log_record(&record)
}

pub(crate) fn append_raw_proxy_response_chunk_log(request_id: &str, chunk: &[u8]) -> Result<()> {
    let record = serde_json::json!({
        "source": "proxy_exchange",
        "event": "response_chunk",
        "request_id": request_id,
        "created_at_unix": now_unix(),
        "response": {
            "body": String::from_utf8_lossy(chunk),
            "bytes": chunk.len()
        }
    });
    append_raw_proxy_log_record(&record)
}

pub(crate) fn append_raw_proxy_log_record(record: &serde_json::Value) -> Result<()> {
    crate::logging::write_raw_log(record)
}

pub(crate) fn raw_proxy_headers(headers: &warp::http::HeaderMap) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for (key, value) in headers.iter() {
        let name = key.as_str().to_ascii_lowercase();
        out.insert(
            name,
            serde_json::json!(value.to_str().unwrap_or("<non-utf8>").to_string()),
        );
    }
    serde_json::Value::Object(out)
}

#[cfg(test)]
pub(crate) fn sanitized_proxy_headers(headers: &warp::http::HeaderMap) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for (key, value) in headers.iter() {
        let name = key.as_str().to_ascii_lowercase();
        let value = if proxy_header_is_sensitive(&name) {
            "<redacted>".to_string()
        } else {
            value.to_str().unwrap_or("<non-utf8>").to_string()
        };
        out.insert(name, serde_json::json!(value));
    }
    serde_json::Value::Object(out)
}

#[cfg(test)]
pub(crate) fn proxy_header_is_sensitive(name: &str) -> bool {
    matches!(
        name,
        "authorization"
            | "proxy-authorization"
            | "x-api-key"
            | "api-key"
            | "cookie"
            | "set-cookie"
            | "chatgpt-account-id"
            | "openai-organization"
            | "anthropic-api-key"
            | "x-goog-api-key"
    )
}

pub(crate) fn visible_channel_models(channel: &ChannelConfig) -> Vec<String> {
    crate::supplier::availability::effective_models(channel, normalize_model_list(
        channel.models.clone(),
        &channel.public_model,
        &channel.upstream_model,
    ))
}

pub(crate) fn local_model_entries(
    cfg: &ClientConfig,
    ready_channel_ids: &HashSet<String>,
) -> Vec<serde_json::Value> {
    let mut positions: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut models: Vec<LocalModelEntry> = Vec::new();
    for channel in local_channels(cfg).filter(|channel| ready_channel_ids.contains(&channel.id)) {
        let mut channel_names = HashSet::new();
        for model in visible_channel_models(channel) {
            let id = model.trim();
            let key = public_model_name(id);
            if key.is_empty() || !channel_names.insert(key.clone()) {
                continue;
            }
            let capability = LocalModelCapability::from_channel(channel, id);
            if let Some(index) = positions.get(&key).copied() {
                models[index].capability.merge(&capability);
            } else {
                positions.insert(key, models.len());
                models.push(LocalModelEntry {
                    id: id.to_string(),
                    capability: capability.clone(),
                });
            }
        }
    }
    models
        .into_iter()
        .map(|model| {
            serde_json::json!({
                "id": model.id,
                "object": "model",
                "owned_by": "local",
                "const_api": model.capability.into_metadata()
            })
        })
        .collect()
}

struct LocalModelEntry {
    id: String,
    capability: LocalModelCapability,
}

#[derive(Clone, Default)]
struct LocalModelCapability {
    context_tokens: Option<u64>,
    output_tokens: Option<u64>,
    protocols: Vec<String>,
    reasoning: bool,
    thinking: bool,
    vision: bool,
    image_output: bool,
    audio_input: bool,
    audio_output: bool,
    video_input: bool,
    video_output: bool,
    file_input: bool,
    file_output: bool,
    tool_calls: bool,
    tool_choice: bool,
    parallel_tool_calls: bool,
    json_schema: bool,
    cache_control: bool,
}

impl LocalModelCapability {
    fn from_channel(channel: &ChannelConfig, model: &str) -> Self {
        let mut capability = Self::default();
        let native = crate::coding_gateway::model_protocol(channel.source_driver(), model);
        (capability.context_tokens, capability.output_tokens) = crate::tool_model_metadata::channel_token_limits(channel, model);
        for protocol in &channel.supported_protocols {
            if native.is_none_or(|native| native.as_str() == protocol) {
                push_unique_string(&mut capability.protocols, protocol);
            }
        }
        for profile in &channel.capability_profiles {
            if native.is_some_and(|native| native.as_str() != profile.protocol) { continue; }
            if !crate::model::capability_layer_is_routable(&profile.capability_layer) {
                continue;
            }
            if !model_pattern_matches_channel_profile(&profile.model_pattern, model) {
                continue;
            }
            if !matches!(
                profile.verification_state.trim(),
                "" | "verified" | "declared"
            ) || matches!(
                profile.release_status.trim().to_ascii_lowercase().as_str(),
                "prepared" | "suspended"
            ) {
                continue;
            }
            push_unique_string(&mut capability.protocols, &profile.protocol);
            capability.reasoning |= profile.reasoning;
            capability.thinking |= profile.thinking;
            capability.vision |= profile.vision || profile.image_input;
            capability.image_output |= profile.image_output;
            capability.audio_input |= profile.audio_input;
            capability.audio_output |= profile.audio_output;
            capability.video_input |= profile.video_input;
            capability.video_output |= profile.video_output;
            capability.file_input |= profile.file_input;
            capability.file_output |= profile.file_output;
            capability.tool_calls |= profile.tool_calls;
            capability.tool_choice |= profile.tool_choice;
            capability.parallel_tool_calls |= profile.parallel_tool_calls;
            capability.json_schema |= profile.json_schema;
            capability.cache_control |= profile.cache_control;
        }
        capability
    }

    fn merge(&mut self, other: &Self) {
        self.context_tokens = self.context_tokens.zip(other.context_tokens).map(|(a, b)| a.min(b));
        self.output_tokens = self.output_tokens.zip(other.output_tokens).map(|(a, b)| a.min(b));
        for protocol in &other.protocols {
            push_unique_string(&mut self.protocols, protocol);
        }
        self.reasoning |= other.reasoning;
        self.thinking |= other.thinking;
        self.vision |= other.vision;
        self.image_output |= other.image_output;
        self.audio_input |= other.audio_input;
        self.audio_output |= other.audio_output;
        self.video_input |= other.video_input;
        self.video_output |= other.video_output;
        self.file_input |= other.file_input;
        self.file_output |= other.file_output;
        self.tool_calls |= other.tool_calls;
        self.tool_choice |= other.tool_choice;
        self.parallel_tool_calls |= other.parallel_tool_calls;
        self.json_schema |= other.json_schema;
        self.cache_control |= other.cache_control;
    }

    fn into_metadata(mut self) -> serde_json::Value {
        self.protocols.sort();
        let preferred_protocol = [
            "openai_responses",
            "openai_chat",
            "anthropic_messages",
            "gemini_native",
        ]
        .into_iter()
        .find(|candidate| self.protocols.iter().any(|value| value == candidate))
        .unwrap_or_default();
        serde_json::json!({
            "source": "local_supplier",
            "context_tokens": self.context_tokens,
            "output_tokens": self.output_tokens,
            "supports_1m": self.context_tokens.is_some_and(|tokens| tokens >= 1_000_000),
            "supported_protocols": self.protocols,
            "preferred_protocol": preferred_protocol,
            "reasoning": self.reasoning,
            "thinking": self.thinking,
            "vision": self.vision,
            "image_input": self.vision,
            "image_output": self.image_output,
            "audio_input": self.audio_input,
            "audio_output": self.audio_output,
            "video_input": self.video_input,
            "video_output": self.video_output,
            "file_input": self.file_input,
            "file_output": self.file_output,
            "tool_calls": self.tool_calls,
            "tool_choice": self.tool_choice,
            "parallel_tool_calls": self.parallel_tool_calls,
            "json_schema": self.json_schema,
            "cache_control": self.cache_control
        })
    }
}

fn model_pattern_matches_channel_profile(pattern: &str, model: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() || pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return model
            .to_ascii_lowercase()
            .starts_with(&prefix.to_ascii_lowercase());
    }
    pattern.eq_ignore_ascii_case(model)
}

fn push_unique_string(values: &mut Vec<String>, value: &str) {
    let value = value.trim();
    if !value.is_empty() && !values.iter().any(|current| current == value) {
        values.push(value.to_string());
    }
}

fn local_models_response(
    config: &ClientConfig,
    surface: crate::surface::ApiSurface,
    models: &[serde_json::Value],
    compact: bool,
) -> Result<warp::reply::Response> {
    let mut rendered = render_models_for_surface(config, surface, models);
    if compact {
        rendered.iter_mut().for_each(crate::model_discovery::compact_entry);
    }
    crate::model_discovery::ModelPresentation::packaged().apply(&mut rendered);
    let mut payload = models_payload_for_surface(surface, rendered);
    payload[crate::model_discovery::POLICY_FIELD] = serde_json::to_value(crate::model_discovery::ModelPresentation::packaged())?;
    let body = serde_json::to_vec(&payload)?;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    response_from_body(StatusCode::OK, &headers, body.into())
}

fn model_catalog_body_has_models(
    surface: crate::surface::ApiSurface,
    body: &[u8],
) -> bool {
    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    let collection_key = match surface {
        crate::surface::ApiSurface::OpenAi | crate::surface::ApiSurface::Anthropic => "data",
        crate::surface::ApiSurface::Gemini => "models",
    };
    payload
        .get(collection_key)
        .and_then(serde_json::Value::as_array)
        .is_some()
}

fn model_catalog_response_is_transient(status: StatusCode) -> bool {
    status.is_server_error()
        || matches!(
            status,
            StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_MANY_REQUESTS
        )
}

async fn remember_model_catalog_response(
    response: warp::reply::Response,
    shared: &ProxyShared,
    surface: crate::surface::ApiSurface,
    raw_query: &str,
    platform_api_key: &str,
) -> Result<warp::reply::Response> {
    let (parts, body) = response.into_parts();
    let body = body.collect().await?.to_bytes();
    // Persist the compact unfiltered platform view, including its policy, for
    // both fresh reuse and network-failure fallback. Never cache an error body.
    let body = if let Ok(mut payload) = serde_json::from_slice::<serde_json::Value>(&body) {
        let key = if surface == crate::surface::ApiSurface::Gemini { "models" } else { "data" };
        if let Some(models) = payload.get_mut(key).and_then(serde_json::Value::as_array_mut) {
            models.iter_mut().for_each(crate::model_discovery::compact_entry);
        }
        bytes::Bytes::from(serde_json::to_vec(&payload)?)
    } else { body };
    shared.remember_model_catalog_snapshot(
        surface,
        raw_query,
        platform_api_key,
        &body,
    );
    rebuild_response(parts, body)
}

fn local_model_control_response(
    config: &ClientConfig,
    route: &crate::surface::ApiRoute,
    models: &[serde_json::Value],
    compact: bool,
) -> Result<warp::reply::Response> {
    if route.operation == crate::surface::ApiOperation::ListModels {
        return local_models_response(config, route.surface, models, compact);
    }
    if let Some(rendered) = local_model_for_route(config, route, models) {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        return response_from_body(
            StatusCode::OK,
            &headers,
            serde_json::to_vec(&rendered)?.into(),
        );
    }
    local_surface_error_response(route.surface, StatusCode::NOT_FOUND, "model not found")
}

fn local_model_for_route(
    config: &ClientConfig,
    route: &crate::surface::ApiRoute,
    models: &[serde_json::Value],
) -> Option<serde_json::Value> {
    let prefix = match route.surface {
        crate::surface::ApiSurface::Gemini => "/v1beta/models/",
        _ => "/v1/models/",
    };
    let requested = public_model_name(route.relative_path.strip_prefix(prefix)?);
    if requested.is_empty() {
        return None;
    }
    render_models_for_surface(config, route.surface, models)
        .into_iter()
        .find(|model| {
            model_id_for_surface(route.surface, model)
                .is_some_and(|id| normalize_model_name(&id) == requested)
        })
}

fn local_surface_error_response(
    surface: crate::surface::ApiSurface,
    status: StatusCode,
    message: &str,
) -> Result<warp::reply::Response> {
    let request_id = format!("req_local_{}", now_unix());
    let payload = match surface {
        crate::surface::ApiSurface::Anthropic => serde_json::json!({
            "type": "error",
            "error": {
                "type": if status == StatusCode::NOT_FOUND { "not_found_error" } else { "invalid_request_error" },
                "message": message
            },
            "request_id": &request_id
        }),
        crate::surface::ApiSurface::Gemini => serde_json::json!({
            "error": {
                "code": status.as_u16(),
                "message": message,
                "status": if status == StatusCode::NOT_FOUND { "NOT_FOUND" } else { "INVALID_ARGUMENT" }
            }
        }),
        crate::surface::ApiSurface::OpenAi => serde_json::json!({
            "error": { "message": message, "type": "const_api_error" }
        }),
    };
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    headers.insert(
        "x-request-id",
        reqwest::header::HeaderValue::from_str(&request_id)?,
    );
    response_from_body(status, &headers, serde_json::to_vec(&payload)?.into())
}

async fn merge_local_models_response(
    response: warp::reply::Response,
    config: &ClientConfig,
    surface: crate::surface::ApiSurface,
    local_models: &[serde_json::Value],
    prefer_local: bool,
    compact: bool,
) -> Result<warp::reply::Response> {
    let (parts, body) = response.into_parts();
    let original_body = body.collect().await?.to_bytes();
    let Ok(mut payload) = serde_json::from_slice::<serde_json::Value>(&original_body) else {
        return rebuild_response(parts, original_body);
    };
    let presentation = crate::model_discovery::ModelPresentation::from_payload(&payload);
    let presentation = presentation.as_ref().unwrap_or_else(|| crate::model_discovery::ModelPresentation::packaged());
    let collection_key = match surface {
        crate::surface::ApiSurface::OpenAi | crate::surface::ApiSurface::Anthropic => "data",
        crate::surface::ApiSurface::Gemini => "models",
    };
    let Some(platform_models) = payload
        .get_mut(collection_key)
        .and_then(|value| value.as_array_mut())
    else {
        return rebuild_response(parts, original_body);
    };

    let local_models = render_models_for_surface(config, surface, local_models);
    let rendered_platform_models = render_models_for_surface(config, surface, platform_models);
    let mut positions = std::collections::HashMap::new();
    let mut merged = Vec::with_capacity(local_models.len() + rendered_platform_models.len());
    let ordered_models = if prefer_local {
        local_models
            .iter()
            .chain(rendered_platform_models.iter())
            .collect::<Vec<_>>()
    } else {
        rendered_platform_models
            .iter()
            .chain(local_models.iter())
            .collect::<Vec<_>>()
    };
    for model in ordered_models {
        let Some(id) = model_id_for_surface(surface, model) else {
            merged.push(model.clone());
            continue;
        };
        let key = normalize_model_name(&id);
        if let Some(index) = positions.get(&key).copied() {
            merge_model_entry_metadata(&mut merged[index], model);
        } else {
            positions.insert(key, merged.len());
            merged.push(model.clone());
        }
    }
    if compact {
        merged.iter_mut().for_each(crate::model_discovery::compact_entry);
    }
    presentation.apply(&mut merged);
    *platform_models = merged;
    if surface == crate::surface::ApiSurface::Anthropic {
        let first_id = platform_models
            .first()
            .and_then(|model| model.get("id"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let last_id = platform_models
            .last()
            .and_then(|model| model.get("id"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        payload["first_id"] = first_id
            .map(serde_json::Value::String)
            .unwrap_or(serde_json::Value::Null);
        payload["last_id"] = last_id
            .map(serde_json::Value::String)
            .unwrap_or(serde_json::Value::Null);
        payload["has_more"] = serde_json::Value::Bool(false);
    }
    payload[crate::model_discovery::POLICY_FIELD] = serde_json::to_value(presentation)?;
    rebuild_response(parts, serde_json::to_vec(&payload)?.into())
}

fn models_payload_for_surface(
    surface: crate::surface::ApiSurface,
    models: Vec<serde_json::Value>,
) -> serde_json::Value {
    match surface {
        crate::surface::ApiSurface::OpenAi => serde_json::json!({
            "object": "list",
            "data": models
        }),
        crate::surface::ApiSurface::Anthropic => {
            let first_id = models
                .first()
                .and_then(|model| model.get("id"))
                .and_then(serde_json::Value::as_str);
            let last_id = models
                .last()
                .and_then(|model| model.get("id"))
                .and_then(serde_json::Value::as_str);
            serde_json::json!({
                "data": models,
                "has_more": false,
                "first_id": first_id,
                "last_id": last_id
            })
        }
        crate::surface::ApiSurface::Gemini => serde_json::json!({
            "models": models
        }),
    }
}

fn render_model_for_surface(
    surface: crate::surface::ApiSurface,
    model: &serde_json::Value,
) -> serde_json::Value {
    if !model.is_object() {
        return model.clone();
    }
    let original_id = model
        .get("id")
        .and_then(serde_json::Value::as_str)
        .or_else(|| model.get("name").and_then(serde_json::Value::as_str))
        .unwrap_or_default();
    let id = public_model_name(original_id);
    let mut model = model.clone();
    // Evidence was evaluated against the source spelling. Scope matching
    // evidence to this public entry before exposing its renamed ID.
    if let Some(metadata) = model
        .get_mut("const_api")
        .and_then(serde_json::Value::as_object_mut)
    {
        for field in ["native_capabilities", "effective_capabilities"] {
            if let Some(evidence) = metadata
                .get_mut(field)
                .and_then(serde_json::Value::as_array_mut)
            {
                evidence.retain_mut(|item| {
                    let pattern = item
                        .get("model_pattern")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    if pattern.trim().is_empty() || pattern.trim() == "*" {
                        return true;
                    }
                    if !model_pattern_matches_channel_profile(pattern, original_id)
                        && !model_pattern_matches_channel_profile(
                            pattern,
                            original_id.trim_start_matches("models/"),
                        )
                    {
                        return false;
                    }
                    item["model_pattern"] = serde_json::json!(id);
                    true
                });
            }
        }
    }
    let metadata = model
        .get("const_api")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    match surface {
        crate::surface::ApiSurface::OpenAi => {
            model["id"] = serde_json::json!(id);
            for field in ["name", "display_name", "displayName"] {
                if model.get(field).is_some() {
                    model[field] = serde_json::json!(id);
                }
            }
            model
        }
        crate::surface::ApiSurface::Anthropic => serde_json::json!({
            "id": id,
            "type": "model",
            "display_name": id,
            "const_api": metadata
        }),
        crate::surface::ApiSurface::Gemini => {
            model["name"] = serde_json::json!(format!("models/{id}"));
            model["baseModelId"] = serde_json::json!(id);
            model["displayName"] = serde_json::json!(id);
            if let Some(object) = model.as_object_mut() {
                object.remove("id");
                object
                    .entry("version")
                    .or_insert_with(|| serde_json::json!(id));
                object
                    .entry("supportedGenerationMethods")
                    .or_insert_with(|| {
                        serde_json::json!([
                            "generateContent", "streamGenerateContent", "countTokens"
                        ])
                    });
            }
            model
        }
    }
}

fn render_models_for_surface(
    config: &ClientConfig,
    surface: crate::surface::ApiSurface,
    models: &[serde_json::Value],
) -> Vec<serde_json::Value> {
    let mut seen = HashSet::new();
    let normalized_models = models
        .iter()
        .map(|model| render_model_for_surface(crate::surface::ApiSurface::OpenAi, model))
        .filter(|model| {
            model
                .get("id")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|id| !id.is_empty() && seen.insert(id.to_string()))
        })
        .collect::<Vec<_>>();
    let models = normalized_models.as_slice();
    if surface != crate::surface::ApiSurface::Anthropic {
        let mut rendered = models
            .iter()
            .map(|model| render_model_for_surface(surface, model))
            .collect::<Vec<_>>();
        rendered.sort_by_cached_key(|model| model_id_for_surface(surface, model).unwrap_or_default());
        return rendered;
    }
    let model_ids = models
        .iter()
        .filter_map(|model| model_id_for_surface(crate::surface::ApiSurface::OpenAi, model))
        .collect::<Vec<_>>();
    let routes = crate::model_compatibility::anthropic_model_routes(config, &model_ids);
    let model_info = crate::tool_model_metadata::tool_models_from_response(
        &serde_json::json!({"data": models}),
    );
    let mut rendered = routes
        .into_iter()
        .map(|route| {
            let route_name = route;
            let mut rendered = serde_json::json!({
                "id": route_name,
                "type": "model",
                "display_name": route_name,
                "const_api": {}
            });
            // The Claude-facing route is a compatibility alias. Preserve only
            // the first actually available candidate's metadata: this matches
            // routing precedence and avoids advertising the union of
            // capabilities from fallback models that would not handle the
            // request while the preferred candidate is ready.
            for candidate in std::iter::once(route_name.clone()).chain(
                crate::model_compatibility::model_compatibility_candidates(config, &route_name),
            ) {
                let candidate = normalize_model_name(&candidate);
                let matching = models
                    .iter()
                    .filter(|model| {
                        model_id_for_surface(crate::surface::ApiSurface::OpenAi, model)
                            .is_some_and(|id| normalize_model_name(&id) == candidate)
                    })
                    .collect::<Vec<_>>();
                for model in &matching {
                    merge_model_entry_metadata(&mut rendered, model);
                }
                if !matching.is_empty() {
                    break;
                }
            }
            // Keep preferred capabilities, but intersect capacity across every
            // eligible compatibility fallback. Never promise the largest window.
            let (context, output, supports_1m) = crate::tool_model_metadata::route_token_limits(config, &route_name, &model_info);
            rendered["const_api"]["context_tokens"] = serde_json::json!(context);
            rendered["const_api"]["output_tokens"] = serde_json::json!(output);
            rendered["const_api"]["supports_1m"] = serde_json::json!(supports_1m);
            rendered
        })
        .collect::<Vec<_>>();
    rendered.sort_by_cached_key(|model| model_id_for_surface(surface, model).unwrap_or_default());
    rendered
}

fn model_id_for_surface(
    surface: crate::surface::ApiSurface,
    model: &serde_json::Value,
) -> Option<String> {
    match surface {
        crate::surface::ApiSurface::OpenAi | crate::surface::ApiSurface::Anthropic => model
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        crate::surface::ApiSurface::Gemini => model
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map(|name| name.strip_prefix("models/").unwrap_or(name).to_string()),
    }
}

fn merge_model_entry_metadata(preferred: &mut serde_json::Value, additional: &serde_json::Value) {
    let additional_metadata = additional.get("const_api").unwrap_or(&serde_json::Value::Null);
    let preferred_id = preferred.get("id").and_then(serde_json::Value::as_str).unwrap_or_default();
    let preferred_fallback = crate::tool_model_metadata::catalog_token_limits(preferred_id);
    let additional_id = additional.get("id").and_then(serde_json::Value::as_str).unwrap_or_default();
    let additional_fallback = crate::tool_model_metadata::catalog_token_limits(additional_id);
    let Some(preferred_object) = preferred.as_object_mut() else {
        return;
    };
    let preferred_metadata = preferred_object
        .entry("const_api")
        .or_insert(serde_json::Value::Object(Default::default()));
    if !preferred_metadata.is_object() {
        *preferred_metadata = serde_json::json!({});
    }
    let old_limits = ["context_tokens", "output_tokens"].map(|key| preferred_metadata.get(key).cloned());
    let supports_1m = preferred_metadata.get("supports_1m").and_then(serde_json::Value::as_bool) != Some(false)
        && additional_metadata.get("supports_1m").and_then(serde_json::Value::as_bool) != Some(false);
    merge_metadata_value(preferred_metadata, additional_metadata);
    for (index, key) in ["context_tokens", "output_tokens"].iter().enumerate() {
        let next = crate::tool_model_metadata::metadata_token_limit(additional_metadata.get(key), if index == 0 { additional_fallback.0 } else { additional_fallback.1 });
        let old = crate::tool_model_metadata::metadata_token_limit(old_limits[index].as_ref(), if index == 0 { preferred_fallback.0 } else { preferred_fallback.1 });
        let limit = old.zip(next).map(|(a,b)| a.min(b));
        preferred_metadata[*key] = serde_json::json!(limit);
    }
    preferred_metadata["supports_1m"] = serde_json::json!(supports_1m && preferred_metadata.get("context_tokens").and_then(serde_json::Value::as_u64).is_some_and(|tokens| tokens >= 1_000_000));
}

fn merge_metadata_value(preferred: &mut serde_json::Value, additional: &serde_json::Value) {
    match (preferred, additional) {
        (serde_json::Value::Object(preferred), serde_json::Value::Object(additional)) => {
            for (key, value) in additional {
                if let Some(current) = preferred.get_mut(key) {
                    merge_metadata_value(current, value);
                } else {
                    preferred.insert(key.clone(), value.clone());
                }
            }
        }
        (serde_json::Value::Array(preferred), serde_json::Value::Array(additional)) => {
            for value in additional {
                if !preferred.contains(value) {
                    preferred.push(value.clone());
                }
            }
        }
        (serde_json::Value::Bool(preferred), serde_json::Value::Bool(additional)) => {
            *preferred |= *additional;
        }
        (preferred @ serde_json::Value::Null, additional) => {
            *preferred = additional.clone();
        }
        _ => {}
    }
}

fn rebuild_response(
    parts: warp::http::response::Parts,
    body: bytes::Bytes,
) -> Result<warp::reply::Response> {
    let mut builder = warp::http::Response::builder().status(parts.status);
    for (name, value) in &parts.headers {
        if name.as_str().eq_ignore_ascii_case("content-length") {
            continue;
        }
        builder = builder.header(name.clone(), value.clone());
    }
    builder
        .body(body.into())
        .map_err(|err| anyhow!("rebuild model response: {err}"))
}

pub(crate) fn local_channels(cfg: &ClientConfig) -> impl Iterator<Item = &ChannelConfig> {
    cfg.channels.iter().filter(move |channel| {
        channel.enabled
            && channel_can_forward_locally(channel)
            && !channel_points_to_current_proxy(channel, &cfg.listen)
    })
}

#[cfg(test)]
pub(crate) fn select_local_channel<'a>(
    cfg: &'a ClientConfig,
    path: &str,
    body: &[u8],
) -> Option<&'a ChannelConfig> {
    select_local_channel_route_where(cfg, path, body, |_| true).map(|selection| selection.channel)
}

struct LocalChannelSelection<'a> {
    channel: &'a ChannelConfig,
    route_model: String,
    substituted: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ModelMatchMode {
    #[default]
    Configured,
    ExactOnly,
    CompatibleOnly,
}

#[cfg(test)]
fn select_ready_local_channel<'a>(
    cfg: &'a ClientConfig,
    method: &warp::http::Method,
    path: &str,
    body: &[u8],
    ready_channel_ids: &HashSet<String>,
) -> std::result::Result<Option<LocalChannelSelection<'a>>, crate::surface::ApiSurface> {
    select_ready_local_channel_with_match(
        cfg,
        method,
        path,
        body,
        ready_channel_ids,
        ModelMatchMode::Configured,
    )
}

#[cfg(test)]
fn select_ready_local_channel_with_match<'a>(
    cfg: &'a ClientConfig,
    method: &warp::http::Method,
    path: &str,
    body: &[u8],
    ready_channel_ids: &HashSet<String>,
    match_mode: ModelMatchMode,
) -> std::result::Result<Option<LocalChannelSelection<'a>>, crate::surface::ApiSurface> {
    select_ready_local_channel_with_model_quota_and_match(
        cfg,
        method,
        path,
        body,
        ready_channel_ids,
        &HashMap::new(),
        now_unix(),
        match_mode,
    )
}

#[cfg(test)]
fn select_ready_local_channel_with_model_quota<'a>(
    cfg: &'a ClientConfig,
    method: &warp::http::Method,
    path: &str,
    body: &[u8],
    ready_channel_ids: &HashSet<String>,
    model_quota_routes: &HashMap<String, crate::supplier::LocalModelQuotaRouteState>,
    now: i64,
) -> std::result::Result<Option<LocalChannelSelection<'a>>, crate::surface::ApiSurface> {
    select_ready_local_channel_with_model_quota_and_match(
        cfg,
        method,
        path,
        body,
        ready_channel_ids,
        model_quota_routes,
        now,
        ModelMatchMode::Configured,
    )
}

fn select_ready_local_channel_with_model_quota_and_match<'a>(
    cfg: &'a ClientConfig,
    method: &warp::http::Method,
    path: &str,
    body: &[u8],
    ready_channel_ids: &HashSet<String>,
    model_quota_routes: &HashMap<String, crate::supplier::LocalModelQuotaRouteState>,
    now: i64,
    match_mode: ModelMatchMode,
) -> std::result::Result<Option<LocalChannelSelection<'a>>, crate::surface::ApiSurface> {
    Ok(select_ready_local_channel_with_model_route_policy(
        cfg,
        method,
        path,
        body,
        ready_channel_ids,
        model_quota_routes,
        now,
        match_mode,
        None,
        &HashSet::new(),
        &HashSet::new(),
    ))
}

#[allow(clippy::too_many_arguments)]
fn select_ready_local_channel_with_model_route_policy<'a>(
    cfg: &'a ClientConfig,
    method: &warp::http::Method,
    path: &str,
    body: &[u8],
    ready_channel_ids: &HashSet<String>,
    model_quota_routes: &HashMap<String, crate::supplier::LocalModelQuotaRouteState>,
    now: i64,
    match_mode: ModelMatchMode,
    required_route_key: Option<&str>,
    excluded_route_keys: &HashSet<String>,
    excluded_channel_ids: &HashSet<String>,
) -> Option<LocalChannelSelection<'a>> {
    let select = |allow_soft_failure| {
        if let Some(route) = crate::surface::resolve_api_route(method.as_str(), path, false) {
            let subscription_image = is_openai_subscription_image_request(method, path);
            let realtime_call = matches!(
                route.operation,
                crate::surface::ApiOperation::RealtimeCallsCreate
                    | crate::surface::ApiOperation::RealtimeLiveCallCreate
            );
            if matches!(
                route.operation,
                crate::surface::ApiOperation::Opaque | crate::surface::ApiOperation::Embeddings
            ) || crate::surface::api_operation_requires_native_api_credential(route.operation)
                || crate::surface::api_operation_requires_local_backend(route.operation)
                || subscription_image
            {
                if match_mode == ModelMatchMode::CompatibleOnly {
                    return None;
                }
                let direct_route_model = if realtime_call {
                    openai_realtime_call_model_hint(body)
                } else {
                    requested_model_from_body_or_path(&String::from_utf8_lossy(body), path)
                        .unwrap_or_default()
                };
                if realtime_call {
                    return select_openai_realtime_call_channel_where(cfg, &direct_route_model, |channel| {
                        ready_channel_ids.contains(&channel.id)
                            && !excluded_channel_ids.contains(&channel.id)
                            && local_model_quota_route_available(
                                model_quota_routes,
                                channel,
                                &direct_route_model,
                                now,
                                allow_soft_failure,
                            )
                            && local_model_route_policy_allows(
                                channel,
                                &direct_route_model,
                                required_route_key,
                                excluded_route_keys,
                            )
                    })
                    .map(|channel| LocalChannelSelection {
                        channel,
                        route_model: direct_route_model,
                        substituted: false,
                    });
                }
                if subscription_image {
                    let subscription = select_openai_subscription_image_channel_where(cfg, |channel| {
                            ready_channel_ids.contains(&channel.id)
                                && !excluded_channel_ids.contains(&channel.id)
                                && local_model_quota_route_available(
                                    model_quota_routes,
                                    channel,
                                    &direct_route_model,
                                    now,
                                    allow_soft_failure,
                                )
                                && local_model_route_policy_allows(
                                    channel,
                                    &direct_route_model,
                                    required_route_key,
                                    excluded_route_keys,
                                )
                        });
                    if let Some(channel) = subscription {
                        return Some(LocalChannelSelection {
                            channel,
                            route_model: direct_route_model,
                            substituted: false,
                        });
                    }
                }
                let require_verified_surface = route.operation == crate::surface::ApiOperation::Opaque
                    || crate::surface::api_operation_requires_native_api_credential(route.operation)
                    || crate::surface::api_operation_requires_local_backend(route.operation)
                    || subscription_image;
                let direct = select_direct_http_surface_channel_where(
                    cfg,
                    route.surface,
                    path,
                    body,
                    require_verified_surface,
                    |channel| {
                        ready_channel_ids.contains(&channel.id)
                            && !excluded_channel_ids.contains(&channel.id)
                            && local_model_quota_route_available(
                                model_quota_routes,
                                channel,
                                &direct_route_model,
                                now,
                                allow_soft_failure,
                            )
                            && local_model_route_policy_allows(
                                channel,
                                &direct_route_model,
                                required_route_key,
                                excluded_route_keys,
                            )
                    },
                );
                if let Some(channel) = direct {
                    return Some(LocalChannelSelection {
                        channel,
                        route_model: direct_route_model,
                        substituted: false,
                    });
                }
                return None;
            }
        }

        select_local_channel_route_where_with_model_and_match(
            cfg,
            path,
            body,
            match_mode,
            |channel, route_model| {
                ready_channel_ids.contains(&channel.id)
                    && !excluded_channel_ids.contains(&channel.id)
                    && local_model_quota_route_available(model_quota_routes, channel, route_model, now, allow_soft_failure)
                    && local_model_route_policy_allows(
                        channel,
                        route_model,
                        required_route_key,
                        excluded_route_keys,
                    )
            },
        )
    };
    // Prefer healthy routes in the existing manual/protocol order. A weak
    // failure must not turn the only remaining route into "no supplier".
    // Sticky lookup cannot bypass healthy alternatives through this fallback.
    select(false).or_else(|| required_route_key.is_none().then(|| select(true)).flatten())
}

fn local_model_route_key(channel: &ChannelConfig, route_model: &str) -> String {
    let upstream_model = channel_upstream_model_for_request(channel, Some(route_model));
    crate::supplier::model_quota_route_key(&channel.id, &upstream_model)
}

fn local_model_route_policy_allows(
    channel: &ChannelConfig,
    route_model: &str,
    required_route_key: Option<&str>,
    excluded_route_keys: &HashSet<String>,
) -> bool {
    let key = local_model_route_key(channel, route_model);
    crate::supplier::availability::allows(channel, route_model)
        && !excluded_route_keys.contains(&key) && required_route_key.is_none_or(|required| required == key)
}

fn local_model_quota_route_available(
    routes: &HashMap<String, crate::supplier::LocalModelQuotaRouteState>,
    channel: &ChannelConfig,
    route_model: &str,
    now: i64,
    allow_soft_failure: bool,
) -> bool {
    let key = local_model_route_key(channel, route_model);
    routes.get(&key).is_none_or(|state| {
        state.available_at(now) || (allow_soft_failure && state.available_as_last_resort(now))
    })
}

fn select_direct_http_surface_channel_where<'a>(
    cfg: &'a ClientConfig,
    surface: crate::surface::ApiSurface,
    path: &str,
    body: &[u8],
    require_verified_surface: bool,
    predicate: impl Fn(&ChannelConfig) -> bool,
) -> Option<&'a ChannelConfig> {
    let candidates = local_channels(cfg)
        .filter(|channel| {
            predicate(channel)
                && crate::detection::responses_features::channel_allows_path(channel, path)
                && crate::channel_executor::execution_kind_for_source(channel.source_driver())
                    .is_ok_and(|kind| kind == crate::source_driver::ExecutionKind::HttpSurface)
                && crate::channel_surface::channel_surface_target(
                    channel,
                    surface,
                    None,
                    require_verified_surface,
                )
                .is_some_and(|target| target.surface == surface)
        })
        .collect::<Vec<_>>();

    let family = crate::surface::official_model_api_family_id_for_path(surface, path);
    let evidence_rank = |channel: &ChannelConfig| {
        let Some(family) = family.as_deref() else {
            return 1_u8;
        };
        channel
            .detection_checks
            .iter()
            .filter(|check| {
                check.name == "surface_operation_family" && check.capability == family
            })
            .map(|check| match check.status.trim().to_ascii_lowercase().as_str() {
                "driver_contract" | "verified" => 0,
                "unknown" => 1,
                "credential_limited" => 2,
                "unsupported" => 3,
                _ => 1,
            })
            .min()
            .unwrap_or(1)
    };

    if let Some(requested_model) =
        requested_model_from_body_or_path(&String::from_utf8_lossy(body), path)
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty())
    {
        let matching = candidates
            .iter()
            .copied()
            .filter(|channel| channel_supports_model(channel, &requested_model))
            .collect::<Vec<_>>();
        if let Some((_, channel)) = matching
            .iter()
            .copied()
            .enumerate()
            .min_by_key(|(index, channel)| (evidence_rank(channel), *index))
        {
            return Some(channel);
        }
    }

    candidates
        .iter()
        .copied()
        .enumerate()
        .min_by_key(|(index, channel)| (evidence_rank(channel), *index))
        .map(|(_, channel)| channel)
}

fn select_openai_subscription_image_channel_where<'a>(
    cfg: &'a ClientConfig,
    predicate: impl Fn(&ChannelConfig) -> bool,
) -> Option<&'a ChannelConfig> {
    local_channels(cfg).find(|channel| {
        predicate(channel)
                && crate::channel_executor::execution_kind_for_source(channel.source_driver())
                    .is_ok_and(|kind| {
                        kind == crate::source_driver::ExecutionKind::OpenAiSubscription
                    })
                && channel_can_execute_protocol(channel, "openai_responses")
    })
}

fn select_openai_realtime_call_channel_where<'a>(
    cfg: &'a ClientConfig,
    model: &str,
    predicate: impl Fn(&ChannelConfig) -> bool,
) -> Option<&'a ChannelConfig> {
    let eligible = |channel: &&ChannelConfig| {
        if !predicate(channel) {
            return false;
        }
        match crate::channel_executor::execution_kind_for_source(channel.source_driver()) {
            Ok(crate::source_driver::ExecutionKind::OpenAiSubscription) => true,
            Ok(crate::source_driver::ExecutionKind::HttpSurface) => {
                crate::channel_surface::channel_surface_target(
                    channel,
                    crate::surface::ApiSurface::OpenAi,
                    None,
                    true,
                )
                .is_some_and(|target| target.surface == crate::surface::ApiSurface::OpenAi)
            }
            _ => false,
        }
    };
    // A public Realtime model with a native API channel must not be silently
    // mapped to private GPT-Live just because a subscription appears first.
    local_channels(cfg)
        .filter(eligible)
        .find(|channel| {
            !model.is_empty()
                && channel_supports_model(channel, model)
                && crate::channel_executor::execution_kind_for_source(channel.source_driver())
                    .is_ok_and(|kind| kind == crate::source_driver::ExecutionKind::HttpSurface)
        })
        .or_else(|| local_channels(cfg).find(eligible))
}

#[cfg(test)]
fn select_local_channel_route_where<'a>(
    cfg: &'a ClientConfig,
    path: &str,
    body: &[u8],
    predicate: impl Fn(&ChannelConfig) -> bool,
) -> Option<LocalChannelSelection<'a>> {
    select_local_channel_route_where_with_model(cfg, path, body, |channel, _| predicate(channel))
}

#[cfg(test)]
fn select_local_channel_route_where_with_model<'a>(
    cfg: &'a ClientConfig,
    path: &str,
    body: &[u8],
    predicate: impl Fn(&ChannelConfig, &str) -> bool,
) -> Option<LocalChannelSelection<'a>> {
    select_local_channel_route_where_with_model_and_match(
        cfg,
        path,
        body,
        ModelMatchMode::Configured,
        predicate,
    )
}

fn select_local_channel_route_where_with_model_and_match<'a>(
    cfg: &'a ClientConfig,
    path: &str,
    body: &[u8],
    match_mode: ModelMatchMode,
    predicate: impl Fn(&ChannelConfig, &str) -> bool,
) -> Option<LocalChannelSelection<'a>> {
    let body = String::from_utf8_lossy(body);
    let requested = requested_model_from_body_or_path(&body, path)?;
    let requested = requested.trim();
    if requested.is_empty() {
        return None;
    }
    let protocol = protocol_from_path(path);
    let requires_claude_server_side_compaction = protocol == "anthropic_messages"
        && crate::supplier::claude_request_uses_server_side_compaction(&body);
    let required_channel_evidence = request_features_requiring_channel_evidence(path, &body);
    let compatibility =
        || crate::model_compatibility::model_compatibility_candidates(cfg, requested);
    let route_models = match match_mode {
        ModelMatchMode::Configured => {
            let mut models = vec![requested.to_string()];
            models.extend(compatibility());
            models
        }
        ModelMatchMode::ExactOnly => vec![requested.to_string()],
        ModelMatchMode::CompatibleOnly => compatibility(),
    };
    for route_model in route_models {
        if let Some((channel, _, _)) = local_channels(cfg)
            .filter_map(|channel| {
                if !predicate(channel, &route_model)
                    || !crate::detection::responses_features::channel_allows_path(channel, path)
                    || !channel_supports_model(channel, &route_model)
                    || !channel_can_execute_protocol(channel, &protocol)
                    || (requires_claude_server_side_compaction
                        && !local_channel_supports_claude_server_side_compaction(
                            channel,
                            &route_model,
                        ))
                {
                    return None;
                }
                let upstream_model = channel_upstream_model_for_request(channel, Some(&route_model));
                let native = crate::coding_gateway::model_protocol(channel.source_driver(), &upstream_model);
                channel_profiles_required_request_features_rank(
                    native.map(|protocol| protocol.as_str()).unwrap_or(&channel.api_format),
                    &upstream_model,
                    &channel.capability_profiles,
                    &required_channel_evidence,
                )
                .map(|rank| {
                    (
                        channel,
                        rank,
                        local_channel_protocol_fidelity_rank(channel, &protocol, native),
                    )
                })
            })
            // Capability evidence protects request semantics first. Protocol
            // fidelity then avoids an unnecessary conversion; the stable
            // iterator order remains the user's manual priority for ties.
            .min_by_key(|(_, evidence_rank, protocol_rank)| {
                (*evidence_rank, *protocol_rank)
            })
        {
            return Some(LocalChannelSelection {
                channel,
                substituted: normalize_model_name(&route_model) != normalize_model_name(requested),
                route_model,
            });
        }
    }
    None
}

fn local_channel_supports_claude_server_side_compaction(
    channel: &ChannelConfig,
    route_model: &str,
) -> bool {
    if channel.source_driver() != crate::source_driver::SourceDriverId::ClaudeSubscription
        || channel_target_protocol(channel)
            != Some(crate::protocol::kind::ProtocolKind::AnthropicMessages)
    {
        return false;
    }
    let upstream_model = channel_upstream_model_for_request(channel, Some(route_model));
    crate::protocol::anthropic_dialect::anthropic_model_supports_server_side_compaction(
        &upstream_model,
    )
}

fn local_response_marks_channel_unready(status: StatusCode) -> bool {
    status.is_server_error()
        || matches!(
            status,
            StatusCode::UNAUTHORIZED
                | StatusCode::FORBIDDEN
                | StatusCode::PAYMENT_REQUIRED
                | StatusCode::NOT_FOUND
                | StatusCode::REQUEST_TIMEOUT
                | StatusCode::LOCKED
                | StatusCode::TOO_MANY_REQUESTS
        )
}

fn local_channel_readiness_observation(
    status: StatusCode,
    model_scoped: bool,
    channel_scoped: bool,
) -> Option<bool> {
    if status.is_success() {
        // Successful real traffic is direct channel-health evidence. Exact
        // model health is recovered independently on successful completion.
        return Some(true);
    }
    if model_scoped {
        return None;
    }
    // Failed traffic may take a channel out of service only when it carries
    // channel-wide evidence. Request/model failures do not mutate this layer.
    channel_scoped.then_some(false)
}

fn local_response_marks_channel_unready_for_request(
    method: &str,
    path: &str,
    status: StatusCode,
) -> bool {
    if status == StatusCode::NOT_IMPLEMENTED {
        return false;
    }

    let canonical_path = path.split_once('?').map_or(path, |(path, _)| path);
    let operation = crate::surface::resolve_api_route(method, canonical_path, false)
        .map(|route| route.operation);
    if matches!(
        operation,
        Some(
            crate::surface::ApiOperation::RealtimeCallsCreate
                | crate::surface::ApiOperation::RealtimeLiveCallCreate
        )
    )
        && status == StatusCode::FORBIDDEN
    {
        // Codex Voice entitlement is independent of the same account's normal
        // Responses entitlement. A pre-execution Voice denial must not take the
        // entire OpenAI subscription channel out of local routing.
        return false;
    }

    if matches!(
        status,
        StatusCode::UNAUTHORIZED
            | StatusCode::PAYMENT_REQUIRED
            | StatusCode::FORBIDDEN
            | StatusCode::REQUEST_TIMEOUT
            | StatusCode::LOCKED
    ) {
        return true;
    }

    if operation.is_some_and(local_operation_uses_model_health) {
        return false;
    }
    if matches!(
        operation,
        Some(
            crate::surface::ApiOperation::Opaque
                | crate::surface::ApiOperation::ResponsesCompact
                | crate::surface::ApiOperation::Embeddings
                | crate::surface::ApiOperation::ImagesGenerations
                | crate::surface::ApiOperation::ImagesEdits
                | crate::surface::ApiOperation::ImagesVariations
                | crate::surface::ApiOperation::CountTokens
                | crate::surface::ApiOperation::EmbedContent
        )
    ) {
        return false;
    }

    local_response_marks_channel_unready(status)
}

fn local_response_allows_request_retry(method: &str, path: &str, status: StatusCode) -> bool {
    if status == StatusCode::NOT_IMPLEMENTED
        || !(status == StatusCode::NOT_FOUND
            || status == StatusCode::TOO_MANY_REQUESTS
            || status.is_server_error())
    {
        return false;
    }
    let canonical_path = path.split_once('?').map_or(path, |(path, _)| path);
    crate::surface::resolve_api_route(method, canonical_path, false)
        .is_some_and(|route| local_operation_uses_model_health(route.operation))
}

fn local_operation_uses_model_health(operation: crate::surface::ApiOperation) -> bool {
    use crate::surface::ApiOperation;
    matches!(
        operation,
        ApiOperation::GetModel
            | ApiOperation::ChatCompletions
            | ApiOperation::Responses
            | ApiOperation::ResponsesCompact
            | ApiOperation::ResponsesWebSocket
            | ApiOperation::RealtimeWebSocket
            | ApiOperation::RealtimeCallsCreate
            | ApiOperation::RealtimeLiveCallCreate
            | ApiOperation::RealtimeLiveWebSocket
            | ApiOperation::RealtimeLiveConnect
            | ApiOperation::RealtimeTranslationWebSocket
            | ApiOperation::ImagesGenerations
            | ApiOperation::ImagesEdits
            | ApiOperation::ImagesVariations
            | ApiOperation::AudioSpeech
            | ApiOperation::AudioTranscriptions
            | ApiOperation::AudioTranslations
            | ApiOperation::VideosCreate
            | ApiOperation::VideosRemix
            | ApiOperation::VideosEdit
            | ApiOperation::VideosExtend
            | ApiOperation::Embeddings
            | ApiOperation::Messages
            | ApiOperation::CountTokens
            | ApiOperation::GenerateContent
            | ApiOperation::StreamGenerateContent
            | ApiOperation::EmbedContent
            | ApiOperation::InteractionsCreate
            | ApiOperation::LiveWebSocket
    )
}

fn local_operation_proves_model_health(operation: crate::surface::ApiOperation) -> bool {
    use crate::surface::ApiOperation;
    local_operation_uses_model_health(operation)
        && !matches!(
            operation,
            ApiOperation::GetModel
                | ApiOperation::CountTokens
                | ApiOperation::ResponsesInputTokens
                | ApiOperation::ResponsesCompact
                | ApiOperation::RealtimeCallsCreate
                | ApiOperation::RealtimeLiveCallCreate
                | ApiOperation::RealtimeLiveConnect
        )
}

fn local_execution_error_marks_channel_unready(error: &anyhow::Error) -> bool {
    !crate::upstream_transport::is_ambiguous_transport_error(error)
        && !matches!(
            crate::channel_executor::channel_execution_error_code(error),
            Some("surface_operation_not_supported" | "conversion_failed" | "ambiguous_transport")
        )
}

fn platform_response_allows_local_fallback(status: StatusCode) -> bool {
    local_response_marks_channel_unready(status)
}

pub(crate) fn channel_can_forward_locally(channel: &ChannelConfig) -> bool {
    crate::channel_executor::execution_kind_for_source(channel.source_driver())
        .is_ok_and(|kind| kind != crate::source_driver::ExecutionKind::HttpSurface)
        || crate::channel_surface::normalize_channel_surface_bindings(channel)
            .iter()
            .any(|binding| {
                !binding.base_url.trim().is_empty()
                    && !binding
                        .base_url
                        .trim()
                        .to_ascii_lowercase()
                        .starts_with("local://")
            })
}

fn channel_points_to_current_proxy(channel: &ChannelConfig, listen: &str) -> bool {
    crate::channel_surface::normalize_channel_surface_bindings(channel)
        .iter()
        .any(|binding| is_current_local_proxy(&binding.base_url, listen))
}

pub(crate) fn channel_supports_model(channel: &ChannelConfig, requested: &str) -> bool {
    visible_channel_models(channel)
        .iter()
        .any(|model| model_name_matches(model, requested))
}

pub(crate) fn channel_can_execute_protocol(channel: &ChannelConfig, protocol: &str) -> bool {
    if crate::protocol::kind::ProtocolKind::parse(protocol).is_err() {
        return false;
    }
    channel_target_protocol(channel).is_some()
}

fn local_channel_protocol_fidelity_rank(channel: &ChannelConfig, protocol: &str, native: Option<crate::protocol::kind::ProtocolKind>) -> u8 {
    let Ok(protocol) = crate::protocol::kind::ProtocolKind::parse(protocol) else {
        return 2;
    };
    let surface = match protocol {
        crate::protocol::kind::ProtocolKind::OpenAiResponses
        | crate::protocol::kind::ProtocolKind::OpenAiChat => crate::surface::ApiSurface::OpenAi,
        crate::protocol::kind::ProtocolKind::AnthropicMessages => {
            crate::surface::ApiSurface::Anthropic
        }
        crate::protocol::kind::ProtocolKind::GeminiNative => crate::surface::ApiSurface::Gemini,
    };
    let target = match native {
        Some(native) => crate::channel_surface::channel_protocol_target(channel, native),
        None => crate::channel_surface::channel_surface_target(channel, surface, Some(protocol), false),
    };
    match target {
        Some(target) if target.protocol == protocol => 0,
        Some(target) if target.surface == surface => 1,
        _ => 2,
    }
}

fn channel_target_protocol(channel: &ChannelConfig) -> Option<crate::protocol::kind::ProtocolKind> {
    crate::channel_surface::preferred_channel_surface_target(channel).map(|target| target.protocol)
}

pub(crate) async fn forward_local_channel_once(
    method: warp::http::Method,
    path: &str,
    raw_query: &str,
    headers: warp::http::HeaderMap,
    body: bytes::Bytes,
    client: &Client,
    cfg: &ClientConfig,
    channel: &ChannelConfig,
) -> Result<warp::reply::Response> {
    forward_local_channel_once_with_query_presence(
        method,
        path,
        raw_query,
        !raw_query.is_empty(),
        headers,
        body,
        client,
        cfg,
        channel,
        None,
    )
    .await
}

async fn forward_local_channel_once_with_query_presence(
    method: warp::http::Method,
    path: &str,
    raw_query: &str,
    has_query: bool,
    mut headers: warp::http::HeaderMap,
    body: bytes::Bytes,
    client: &Client,
    cfg: &ClientConfig,
    channel: &ChannelConfig,
    selected_upstream_model: Option<&str>,
) -> Result<warp::reply::Response> {
    let route = crate::surface::resolve_api_route(method.as_str(), path, false)
        .ok_or_else(|| anyhow!("request path is outside the configured API surfaces"))?;
    let incoming_lan_share_path = headers
        .get(crate::lan_share::LAN_SHARE_PATH_HEADER)
        .map(|value| value.to_str())
        .transpose()
        .map_err(|_| anyhow!("invalid LAN share path header"))?
        .map(str::to_string);
    headers.remove(crate::lan_share::LAN_SHARE_PATH_HEADER);
    headers.remove(crate::lan_share::LAN_SHARE_INTERNAL_PATH_HEADER);
    if crate::source_driver::channel_is_lan_share(channel) {
        let next_path = crate::lan_share::lan_share_path_for_next_hop(
            incoming_lan_share_path.as_deref(),
            &cfg.client_id,
        )?;
        headers.insert(
            crate::lan_share::LAN_SHARE_INTERNAL_PATH_HEADER,
            warp::http::HeaderValue::from_str(&next_path)?,
        );
    }
    if is_openai_subscription_image_request(&method, path)
        && crate::channel_executor::execution_kind_for_source(channel.source_driver())
            .is_ok_and(|kind| kind == crate::source_driver::ExecutionKind::OpenAiSubscription)
    {
        return forward_openai_subscription_image_request(client, channel, path, &headers, body)
            .await;
    }
    if matches!(
        route.operation,
        crate::surface::ApiOperation::RealtimeCallsCreate
            | crate::surface::ApiOperation::RealtimeLiveCallCreate
    )
        && crate::channel_executor::execution_kind_for_source(channel.source_driver())
            .is_ok_and(|kind| kind == crate::source_driver::ExecutionKind::OpenAiSubscription)
    {
        return forward_openai_subscription_realtime_call(
            client,
            channel,
            &headers,
            body,
            route.operation == crate::surface::ApiOperation::RealtimeLiveCallCreate,
        )
        .await;
    }
    if let crate::channel_executor::ChannelExecutionBackend::RetainedSubscription { provider } =
        crate::channel_executor::channel_execution_backend(channel, route.operation)?
    {
        let supplier = supplier_from_channel(channel);
        let original_body_text = String::from_utf8_lossy(&body).to_string();
        let routed_body_text = crate::supplier::rewrite_retained_subscription_body_model(
            provider, path, &original_body_text, selected_upstream_model,
        );
        let body_text = routed_body_text.as_deref().unwrap_or(&original_body_text);
        let routed_path =
            selected_upstream_model.map(|model| rewrite_local_request_model_path(path, model));
        let request_path = routed_path.as_deref().unwrap_or(path);
        let stream_requested = serde_json::from_str::<serde_json::Value>(&body_text)
            .ok()
            .and_then(|json| json.get("stream").and_then(|v| v.as_bool()))
            .unwrap_or_else(|| request_path.contains(":streamGenerateContent"));
        let provider = provider.to_string();
        let force_buffered = subscription_operation_requires_buffered(&provider, request_path);
        crate::channel_executor::admit_retained_subscription_operation(
            channel,
            &route,
            stream_requested && !force_buffered,
        )?;
        let cache_model = channel_upstream_model_for_request(
            channel,
            requested_model_from_body_or_path(body_text, request_path).as_deref(),
        );
        let cache_identity = scoped_subscription_cache_identity(
            &cfg.account_user_id,
            &cache_model,
            &headers,
            body_text,
        );
        let payload = if provider == "openai" || provider == "codex" {
            if stream_requested && !force_buffered {
                return forward_openai_subscription_local_stream_response_with_cache_identity(
                    client,
                    &supplier,
                    request_path,
                    body_text,
                    &headers,
                    cache_identity.as_deref(),
                    None,
                )
                .await;
            }
            forward_openai_subscription_buffered_request_with_capacity(
                client,
                &supplier,
                request_path,
                body_text,
                stream_requested,
                None,
                None,
                cache_identity.as_deref(),
                Some(&headers),
            )
            .await?
        } else if stream_requested && !force_buffered {
            return forward_non_openai_subscription_local_stream_response_with_cache_identity(
                client,
                &supplier,
                request_path,
                body_text,
                None,
                Some(headers),
                cache_identity.as_deref(),
            )
            .await;
        } else {
            forward_subscription_supplier_request_with_capacity(
                client,
                &supplier,
                request_path,
                body_text,
                false,
                None,
                None,
                cache_identity.as_deref(),
                Some(&headers),
            )
            .await
        };
        if payload
            .get("error_kind")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|kind| kind == "ambiguous_transport")
        {
            return Err(crate::channel_executor::ambiguous_transport_error(
                "upstream request outcome is unknown; another channel was not attempted",
            ));
        }
        let wire = crate::surface_wire::SurfaceEnvelope::from_payload(&payload)?;
        let status = wire.status.unwrap_or(502);
        let client_status = local_subscription_status_for_client(status, &headers);
        let mut response_headers = wire.header_map()?;
        if !response_headers.contains_key(reqwest::header::CONTENT_TYPE) {
            response_headers.insert(
                reqwest::header::CONTENT_TYPE,
                reqwest::header::HeaderValue::from_static("application/json"),
            );
        }
        if client_status != status {
            response_headers.insert(
                "x-const-api-upstream-status",
                reqwest::header::HeaderValue::from_str(&status.to_string())
                    .unwrap_or_else(|_| reqwest::header::HeaderValue::from_static("429")),
            );
        }
        if let Some(retry_after) = payload
            .get("retry_after_seconds")
            .and_then(|value| value.as_i64())
            .filter(|value| *value > 0)
        {
            if let Ok(value) = reqwest::header::HeaderValue::from_str(&retry_after.to_string()) {
                response_headers.insert(reqwest::header::RETRY_AFTER, value);
            }
        }
        let mut response = response_from_body(
            warp::http::StatusCode::from_u16(client_status)?,
            &response_headers,
            wire.body.decode()?.into(),
        )?;
        attach_local_model_failure_evidence(&mut response, &payload);
        return Ok(response);
    }
    if channel_points_to_current_proxy(channel, &cfg.listen) {
        return Err(anyhow!("local channel points to current proxy"));
    }
    let req_method = reqwest::Method::from_bytes(method.as_str().as_bytes())?;
    let mut request_headers = reqwest::header::HeaderMap::new();
    for (key, value) in headers.iter() {
        if let (Ok(name), Ok(value)) = (
            reqwest::header::HeaderName::from_bytes(key.as_str().as_bytes()),
            reqwest::header::HeaderValue::from_bytes(value.as_bytes()),
        ) {
            request_headers.append(name, value);
        }
    }
    let body_text = String::from_utf8_lossy(&body);
    let requested_body_model = requested_model_from_body_or_path(&body_text, path);
    let cache_model = channel_upstream_model_for_request(
        channel,
        selected_upstream_model.or(requested_body_model.as_deref()),
    );
    let cache_identity = scoped_subscription_cache_identity(
        &cfg.account_user_id,
        &cache_model,
        &headers,
        &body_text,
    );
    let execution = crate::channel_executor::execute_channel_request(
        client,
        channel,
        crate::channel_executor::ChannelRequest {
            method: req_method,
            path: path.to_string(),
            raw_query: raw_query.to_string(),
            has_query,
            headers: request_headers,
            body,
            stream_requested: None,
            selected_upstream_model: selected_upstream_model.map(str::to_string),
            selected_target_protocol: None,
            cache_identity,
            safety_identifier: None,
        },
    )
    .await?;
    let upstream_is_stream = response_content_type(&execution.response).contains("event-stream");
    let response_mode_matches_request = upstream_is_stream == execution.inbound_stream_requested;
    if execution.opaque
        || execution.inbound_protocol.is_none()
        || (execution.native_passthrough && response_mode_matches_request)
    {
        // Opaque media/file operations may return large binary bodies without
        // a JSON `stream` flag. Relay those bodies incrementally instead of
        // buffering them in memory; this transport rule is platform-neutral.
        let relay_as_stream = execution.opaque
            || execution.inbound_stream_requested
            || crate::surface::api_operation_streams_binary_response(route.operation);
        return response_from_reqwest_with_upstream_model(
            execution.response,
            relay_as_stream,
            Some(&execution.upstream_model),
        )
        .await;
    }
    response_from_target_as_inbound(
        execution.response,
        execution.inbound_protocol.as_deref().unwrap_or_default(),
        &execution.target_protocol,
        &execution.upstream_model,
        execution.inbound_stream_requested,
        execution.tool_mapping,
    )
    .await
    .map_err(crate::channel_executor::response_conversion_error)
}

include!("local_openai_images.rs");
include!("local_openai_realtime.rs");

fn rewrite_local_request_model_path(path: &str, upstream_model: &str) -> String {
    crate::supplier::supplier_upstream_path(path, upstream_model)
}

include!("local_subscription_stream.rs");

pub(crate) fn channel_upstream_model_for_request(
    channel: &ChannelConfig,
    requested_model: Option<&str>,
) -> String {
    let requested = requested_model
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(requested) = requested {
        let requested_key = normalize_model_name(requested);
        let public_key = normalize_model_name(&channel.public_model);
        let upstream_key = normalize_model_name(&channel.upstream_model);
        if let Some(model) = crate::config::resolve_model_name(&channel.models, requested)
        {
            return model.trim().to_string();
        }
        if requested_key == upstream_key {
            return channel.upstream_model.trim().to_string();
        }
        if requested_key == public_key {
            if !channel.upstream_model.trim().is_empty() {
                return channel.upstream_model.trim().to_string();
            }
            return channel.public_model.trim().to_string();
        }
        if !channel.upstream_model.trim().is_empty()
            && (model_name_matches(&channel.upstream_model, requested)
                || model_name_matches(&channel.public_model, requested))
        {
            return channel.upstream_model.trim().to_string();
        }
        return requested.to_string();
    }
    if !channel.upstream_model.trim().is_empty() {
        return channel.upstream_model.trim().to_string();
    }
    requested.unwrap_or_default().to_string()
}

pub(crate) fn protocol_from_path(path: &str) -> String {
    let path = path.split_once('?').map_or(path, |(path, _)| path);
    if let Some(protocol) =
        crate::surface::resolve_api_route("POST", path, false).and_then(|route| route.protocol)
    {
        return protocol.as_str().to_string();
    }
    if path.starts_with("/v1/responses") {
        "openai_responses".to_string()
    } else if path.starts_with("/v1/messages") || path == "/messages" {
        "anthropic_messages".to_string()
    } else if path.starts_with("/v1beta/models/") {
        "gemini_native".to_string()
    } else if path.starts_with("/v1/chat/completions") {
        "openai_chat".to_string()
    } else {
        String::new()
    }
}

pub(crate) fn protocol_from_api_format(api_format: &str) -> String {
    match api_format.trim() {
        "openai_responses" => "openai_responses",
        "anthropic_messages" => "anthropic_messages",
        "gemini_native" => "gemini_native",
        "openai_chat" | "gemini_openai" => "openai_chat",
        _ => "",
    }
    .to_string()
}
