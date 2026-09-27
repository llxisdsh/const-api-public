use crate::{
    account::{AccountRuntime, run_account_refresh_loop},
    cleanup_backup_retention,
    config::{
        default_config, load_config_from_path, load_config_from_path_with_recovery,
        write_config_to_path,
    },
    instance::RuntimeControlStatus,
    model::{AppState, ClientConfig, ProxyRuntime, SupplierRuntime, SupplierTransportSnapshot},
    platform_transport, refresh_endpoint_registry_inner, source_driver_manifest, spawn_logged,
    spawn_supervised, start_proxy_inner, stop_proxy_inner, supplier,
};
use anyhow::{Context, Result};
use serde::Serialize;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{sync::Mutex, task::JoinHandle};

const PLATFORM_STATUS_BACKGROUND_REFRESH_INTERVAL: Duration = Duration::from_secs(30 * 60);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RuntimeLifecycle {
    Created,
    Starting,
    Running,
    Degraded,
    Stopping,
    Stopped,
}

impl RuntimeLifecycle {
    fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Degraded => "degraded",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
        }
    }
}

struct SupervisorControl {
    lifecycle: RuntimeLifecycle,
    tasks: Vec<JoinHandle<()>>,
}

#[derive(Clone)]
pub(crate) struct RuntimeSupervisor {
    state: AppState,
    control: Arc<Mutex<SupervisorControl>>,
    operation: Arc<Mutex<()>>,
    shutdown_requested: Arc<AtomicBool>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct RuntimeSummary {
    pub(crate) lifecycle: RuntimeLifecycle,
    pub(crate) proxy_running: bool,
    pub(crate) proxy_listen: String,
    pub(crate) supplier_running: bool,
    pub(crate) platform_configured: bool,
    pub(crate) enabled_local_channels: usize,
    pub(crate) enabled_shared_channels: usize,
    pub(crate) ready_local_channels: usize,
    pub(crate) readiness: String,
    pub(crate) warnings: Vec<String>,
}

impl RuntimeSummary {
    pub(crate) fn control_status(&self) -> RuntimeControlStatus {
        RuntimeControlStatus {
            lifecycle: self.lifecycle.as_str().to_string(),
            proxy_running: self.proxy_running,
            supplier_running: self.supplier_running,
            readiness: self.readiness.clone(),
        }
    }
}

pub(crate) fn load_or_create_runtime_state(config_path: PathBuf) -> Result<AppState> {
    source_driver_manifest().context("validate source driver manifest")?;
    if let Err(error) = cleanup_backup_retention() {
        log::warn!("[const-api][runtime] backup retention cleanup failed: {error:#}");
    }
    if !config_path.exists() {
        if !crate::development_profile_active() {
            crate::autostart::stage_default_enablement(&crate::instance::state_dir_for_config(
                &config_path,
            ))
            .context("stage default login startup")?;
        }
        let config = default_config();
        write_config_to_path(&config_path, &config).context("write default config")?;
    }
    let initial_config =
        load_config_from_path_with_recovery(&config_path).context("load initial config")?;
    Ok(create_runtime_state(config_path, initial_config))
}

pub(crate) fn create_runtime_state(config_path: PathBuf, initial_config: ClientConfig) -> AppState {
    // Authorization is deliberately enabled later by the account runtime.
    // Until that happens, use only the signed public LKG or the same-source
    // packaged snapshot. Account state never selects a second model catalog.
    crate::tool_model_metadata::restore_embedded_tool_model_metadata();
    if let Ok(release) = crate::catalog_registry::load_cached_catalog_tool(&config_path, true) {
        if let Err(error) = release.install() {
            log::warn!("[const-api][runtime] cached catalog tool metadata rejected: {error:#}");
        }
    }
    let listen = initial_config.listen.clone();
    let account = AccountRuntime::with_persisted_session(&config_path, &initial_config);
    // Account credentials are enabled only after the current local legal
    // acceptance has been reconciled with the selected platform. This avoids
    // a startup window where a stale saved key can route platform traffic.
    let platform_api_key = String::new();
    let platform_transport_state = Arc::new(StdMutex::new(
        platform_transport::PlatformTransportSnapshot::default(),
    ));
    let platform_transport = Arc::new(platform_transport::PlatformHttpTransport::new(
        crate::long_http_client().clone(),
        platform_transport_state.clone(),
    ));
    AppState {
        config_path,
        proxy: Arc::new(Mutex::new(ProxyRuntime {
            running: false,
            listen,
            active_endpoint: None,
            platform_transport_state,
            shutdown: None,
            server_task: None,
        })),
        proxy_config: Arc::new(StdMutex::new(initial_config)),
        local_channel_readiness: Arc::new(StdMutex::new(HashMap::new())),
        local_model_quota_routes: Arc::new(StdMutex::new(HashMap::new())),
        supplier: Arc::new(Mutex::new(SupplierRuntime {
            running: false,
            starting: false,
            node_id: String::new(),
            server_ws_url: String::new(),
            server_quic_url: String::new(),
            transport_preference: String::new(),
            transport_state: Arc::new(StdMutex::new(SupplierTransportSnapshot::default())),
            nodes: Vec::new(),
            channels: Vec::new(),
            last_error: String::new(),
            channel_agents: HashMap::new(),
        })),
        account: Arc::new(Mutex::new(account)),
        model_compatibility_http_cache: Arc::new(StdMutex::new(Default::default())),
        account_refresh_notify: Arc::new(tokio::sync::Notify::new()),
        platform_api_key: Arc::new(StdMutex::new(platform_api_key)),
        platform_transport,
        subscription_oauth_callbacks:
            crate::subscription_oauth_callback::SubscriptionOAuthCallbackManager::default(),
    }
}

pub(crate) async fn refresh_platform_health_if_due(
    state: &AppState,
    minimum_interval: Duration,
) -> Result<bool> {
    let (running, active_endpoint) = {
        let proxy = state.proxy.lock().await;
        (proxy.running, proxy.status().active_endpoint)
    };
    if !running {
        return Ok(false);
    }

    let config = state
        .proxy_config
        .lock()
        .map(|config| config.clone())
        .unwrap_or_else(|_| default_config());
    let endpoint = active_endpoint
        .as_deref()
        .and_then(|active| {
            config
                .endpoints
                .iter()
                .find(|endpoint| endpoint.enabled && endpoint.name == active)
        })
        .or_else(|| config.endpoints.iter().find(|endpoint| endpoint.enabled))
        .cloned();
    let Some(endpoint) = endpoint else {
        return Ok(false);
    };

    state
        .platform_transport
        .refresh_health_if_due(&endpoint, minimum_interval)
        .await
}

impl RuntimeSupervisor {
    pub(crate) fn new(state: AppState) -> Self {
        Self {
            state,
            control: Arc::new(Mutex::new(SupervisorControl {
                lifecycle: RuntimeLifecycle::Created,
                tasks: Vec::new(),
            })),
            operation: Arc::new(Mutex::new(())),
            shutdown_requested: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn state(&self) -> &AppState {
        &self.state
    }

    pub(crate) async fn start(&self) -> Result<RuntimeSummary, String> {
        let _operation = self.operation.lock().await;
        if self.shutdown_requested.load(Ordering::SeqCst) {
            return Err("runtime shutdown has been requested".to_string());
        }
        {
            let mut control = self.control.lock().await;
            match control.lifecycle {
                RuntimeLifecycle::Starting
                | RuntimeLifecycle::Running
                | RuntimeLifecycle::Degraded => {
                    return Ok(self.summary_inner(control.lifecycle).await);
                }
                RuntimeLifecycle::Stopping => {
                    return Err("runtime is stopping".to_string());
                }
                RuntimeLifecycle::Created | RuntimeLifecycle::Stopped => {
                    control.lifecycle = RuntimeLifecycle::Starting;
                }
            }
        }

        let cfg = match load_config_from_path(&self.state.config_path) {
            Ok(config) => config,
            Err(error) => {
                self.set_lifecycle(RuntimeLifecycle::Stopped).await;
                return Err(error.to_string());
            }
        };

        let account_state = self.state.clone();
        let account_task = spawn_supervised("account_refresh", Duration::from_secs(1), move || {
            run_account_refresh_loop(account_state.clone())
        });
        let announcement_state = self.state.clone();
        let announcement_task =
            spawn_supervised("announcement_refresh", Duration::from_secs(1), move || {
                crate::announcements::run_platform_announcement_refresh_loop(
                    announcement_state.clone(),
                )
            });
        let release_source_state = self.state.clone();
        let release_source_task = spawn_supervised(
            "release_source_refresh",
            Duration::from_secs(1),
            move || run_release_source_refresh_loop(release_source_state.clone()),
        );
        let endpoint_state = self.state.clone();
        let endpoint_task = spawn_supervised(
            "endpoint_registry_refresh",
            Duration::from_secs(1),
            move || {
                let endpoint_state = endpoint_state.clone();
                async move {
                    loop {
                        if let Err(error) = refresh_endpoint_registry_inner(&endpoint_state).await {
                            log::warn!(
                                "[const-api][runtime] endpoint registry refresh failed: {error}"
                            );
                        }
                        // Reuse the registry maintenance cadence instead of
                        // adding another timer. This keeps server-version
                        // metadata fresh even while the window is hidden.
                        if let Err(error) = refresh_platform_health_if_due(
                            &endpoint_state,
                            PLATFORM_STATUS_BACKGROUND_REFRESH_INTERVAL,
                        )
                        .await
                        {
                            log::debug!(
                                "[const-api][runtime] platform status refresh failed: {error:#}"
                            );
                        }
                        tokio::time::sleep(PLATFORM_STATUS_BACKGROUND_REFRESH_INTERVAL).await;
                    }
                }
            },
        );
        let catalog_state = self.state.clone();
        let catalog_task = spawn_supervised(
            "catalog_registry_refresh",
            Duration::from_secs(1),
            move || run_catalog_registry_refresh_loop(catalog_state.clone()),
        );
        let compatibility_state = self.state.clone();
        let compatibility_task = spawn_supervised(
            "model_compatibility_sync",
            Duration::from_secs(1),
            move || run_model_compatibility_sync_loop(compatibility_state.clone()),
        );
        let supplier_recovery_state = self.state.clone();
        let supplier_recovery_task = spawn_supervised(
            "supplier_auto_recovery",
            Duration::from_secs(1),
            move || supplier::run_supplier_auto_recovery_loop(supplier_recovery_state.clone()),
        );
        let local_channel_health_state = self.state.clone();
        let local_channel_health_task = spawn_supervised(
            "local_only_channel_health",
            Duration::from_secs(1),
            move || {
                supplier::run_local_only_channel_health_loop(local_channel_health_state.clone())
            },
        );
        {
            let mut control = self.control.lock().await;
            control.tasks.push(account_task);
            control.tasks.push(announcement_task);
            control.tasks.push(release_source_task);
            control.tasks.push(endpoint_task);
            control.tasks.push(catalog_task);
            control.tasks.push(compatibility_task);
            control.tasks.push(supplier_recovery_task);
            control.tasks.push(local_channel_health_task);
        }

        if cfg.proxy_auto_start {
            if let Err(error) = start_proxy_inner(&self.state).await {
                log::error!("[const-api][runtime] local proxy startup failed: {error}");
                self.cleanup_failed_start().await;
                return Err(error);
            }
        }

        let has_enabled_channel = cfg.channels.iter().any(|channel| {
            channel.enabled && crate::source_driver::channel_is_platform_shareable(channel)
        });
        if cfg.supplier_auto_start && has_enabled_channel {
            let supplier_state = self.state.clone();
            let supplier_task = spawn_logged("supplier_auto_start", async move {
                if let Err(error) =
                    supplier::start_supplier_inner(&supplier_state, None, None).await
                {
                    log::warn!("[const-api][runtime] supplier auto-start degraded: {error}");
                }
            });
            self.control.lock().await.tasks.push(supplier_task);
        }

        let preliminary = self.summary_inner(RuntimeLifecycle::Running).await;
        let lifecycle = if preliminary.readiness == "ready" && preliminary.warnings.is_empty() {
            RuntimeLifecycle::Running
        } else {
            RuntimeLifecycle::Degraded
        };
        self.set_lifecycle(lifecycle).await;
        let summary = self.summary_inner(lifecycle).await;
        log_runtime_summary(&summary);
        Ok(summary)
    }

    pub(crate) async fn summary(&self) -> RuntimeSummary {
        let lifecycle = self.control.lock().await.lifecycle;
        let mut summary = self.summary_inner(lifecycle).await;
        if matches!(
            lifecycle,
            RuntimeLifecycle::Running | RuntimeLifecycle::Degraded
        ) {
            let next = if summary.readiness == "ready" && summary.warnings.is_empty() {
                RuntimeLifecycle::Running
            } else {
                RuntimeLifecycle::Degraded
            };
            if next != lifecycle {
                self.set_lifecycle(next).await;
                summary.lifecycle = next;
                log::info!(
                    "[const-api][runtime] lifecycle changed {} -> {}",
                    lifecycle.as_str(),
                    next.as_str()
                );
            }
        }
        summary
    }

    pub(crate) async fn shutdown(&self) {
        self.shutdown_requested.store(true, Ordering::SeqCst);
        let _operation = self.operation.lock().await;
        {
            let mut control = self.control.lock().await;
            if matches!(
                control.lifecycle,
                RuntimeLifecycle::Stopping | RuntimeLifecycle::Stopped
            ) {
                return;
            }
            control.lifecycle = RuntimeLifecycle::Stopping;
        }

        // Stop any in-flight auto-start before draining supplier handles so it
        // cannot repopulate the runtime after stop_supplier_inner returns.
        self.abort_background_tasks().await;
        let _ = tokio::time::timeout(
            Duration::from_millis(700),
            supplier::notify_supplier_offline_inner(&self.state),
        )
        .await;
        let _ = supplier::stop_supplier_inner(&self.state).await;
        let _ = stop_proxy_inner(&self.state).await;
        self.set_lifecycle(RuntimeLifecycle::Stopped).await;
        log::info!("[const-api][runtime] stopped");
    }

    async fn cleanup_failed_start(&self) {
        let _ = supplier::stop_supplier_inner(&self.state).await;
        let _ = stop_proxy_inner(&self.state).await;
        self.abort_background_tasks().await;
        self.set_lifecycle(RuntimeLifecycle::Stopped).await;
    }

    async fn abort_background_tasks(&self) {
        let tasks = {
            let mut control = self.control.lock().await;
            std::mem::take(&mut control.tasks)
        };
        for task in tasks {
            task.abort();
            let _ = task.await;
        }
    }

    async fn set_lifecycle(&self, lifecycle: RuntimeLifecycle) {
        self.control.lock().await.lifecycle = lifecycle;
    }

    async fn summary_inner(&self, lifecycle: RuntimeLifecycle) -> RuntimeSummary {
        // Report the configuration actually used by requests. Commits and
        // explicit reloads already update this state; polling status must not
        // reread, normalize or clone entire channel catalogs every two seconds.
        let (
            platform_configured,
            proxy_auto_start,
            supplier_auto_start,
            enabled_channel_ids,
            enabled_shared_channels,
        ) = {
            let cfg = self
                .state
                .proxy_config
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let enabled = cfg.channels.iter().filter(|channel| channel.enabled);
            (
                !cfg.account_device_api_key.trim().is_empty(),
                cfg.proxy_auto_start,
                cfg.supplier_auto_start,
                enabled
                    .clone()
                    .map(|channel| channel.id.clone())
                    .collect::<Vec<_>>(),
                // LAN channels are local, never platform suppliers.
                enabled
                    .filter(|channel| crate::source_driver::channel_is_platform_shareable(channel))
                    .count(),
            )
        };
        let enabled_local_channels = enabled_channel_ids.len();
        let ready_local_channels = self
            .state
            .local_channel_readiness
            .lock()
            .map(|readiness| {
                enabled_channel_ids
                    .iter()
                    .filter(|id| readiness.get(*id) == Some(&true))
                    .count()
            })
            .unwrap_or_default();
        // Do not build the detailed supplier/transport status for two fields.
        // Keep all synchronous guards out of the awaits below.
        let (proxy_running, proxy_listen) = {
            let proxy = self.state.proxy.lock().await;
            (proxy.running, proxy.listen.clone())
        };
        let (supplier_running, supplier_error) = {
            let supplier = self.state.supplier.lock().await;
            (supplier.running, supplier.last_error.clone())
        };
        let mut warnings = Vec::new();
        if !proxy_auto_start {
            warnings.push("local proxy auto-start is disabled".to_string());
        }
        if !platform_configured && ready_local_channels == 0 {
            warnings.push(
                "no authenticated platform route or ready local channel; generation requests will return 503 until the client is configured"
                    .to_string(),
            );
        }
        if supplier_auto_start && enabled_shared_channels == 0 {
            warnings.push("supplier auto-start is enabled but no channel is enabled".to_string());
        }
        if !supplier_error.trim().is_empty() {
            warnings.push(format!("supplier: {supplier_error}"));
        }
        let readiness = if proxy_running && (platform_configured || ready_local_channels > 0) {
            "ready"
        } else {
            "degraded"
        }
        .to_string();
        RuntimeSummary {
            lifecycle,
            proxy_running,
            proxy_listen,
            supplier_running,
            platform_configured,
            enabled_local_channels,
            enabled_shared_channels,
            ready_local_channels,
            readiness,
            warnings,
        }
    }
}

async fn run_release_source_refresh_loop(state: AppState) {
    loop {
        match crate::release_sources::refresh_release_sources(
            crate::short_http_client(),
            &state.config_path,
        )
        .await
        {
            Ok(refresh) => {
                log::info!(
                    "[const-api][runtime] release sources verified source={} sequence={} changed={}",
                    refresh.active.delivery_source(),
                    refresh.active.sequence(),
                    refresh.changed,
                );
                if refresh.changed {
                    if let Err(error) = refresh_endpoint_registry_inner(&state).await {
                        log::warn!(
                            "[const-api][runtime] endpoint refresh after release source change failed: {error}"
                        );
                    }
                }
            }
            Err(error) => {
                let fallback = crate::release_sources::trusted_release_sources(&state.config_path)
                    .map(|active| {
                        format!(
                            "source={} sequence={}",
                            active.delivery_source(),
                            active.sequence()
                        )
                    })
                    .unwrap_or_else(|fallback_error| format!("unavailable={fallback_error:#}"));
                log::warn!(
                    "[const-api][runtime] release source refresh deferred: {error:#}; active {fallback}"
                );
            }
        }
        tokio::time::sleep(crate::catalog_registry::catalog_refresh_jitter(
            Duration::from_secs(30 * 60),
        ))
        .await;
    }
}

async fn run_model_compatibility_sync_loop(state: AppState) {
    let mut last_platform_key = String::new();
    let mut next_periodic_sync = Instant::now();
    loop {
        let platform_key = state
            .platform_api_key
            .lock()
            .map(|key| key.trim().to_string())
            .unwrap_or_default();
        if platform_key.is_empty() {
            // Clearing this marker makes the next successful login synchronize
            // immediately, even when the device key is reused across sessions.
            last_platform_key.clear();
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }
        if platform_key != last_platform_key || Instant::now() >= next_periodic_sync {
            match crate::model_compatibility::sync_model_compatibility_for_runtime(&state).await {
                Err(error) => {
                    log::warn!("[const-api][runtime] model compatibility sync deferred: {error:#}");
                    next_periodic_sync = Instant::now()
                        + crate::catalog_registry::catalog_refresh_jitter(Duration::from_secs(10));
                }
                Ok(()) => {
                    next_periodic_sync = Instant::now()
                        + crate::catalog_registry::catalog_refresh_jitter(Duration::from_secs(
                            5 * 60,
                        ));
                }
            }
            last_platform_key = platform_key;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn run_catalog_registry_refresh_loop(_: AppState) { std::future::pending::<()>().await; }

fn log_runtime_summary(summary: &RuntimeSummary) {
    log::info!(
        "[const-api][runtime] lifecycle={} proxy_running={} listen={} supplier_running={} readiness={}",
        summary.lifecycle.as_str(),
        summary.proxy_running,
        summary.proxy_listen,
        summary.supplier_running,
        summary.readiness
    );
    for warning in &summary.warnings {
        log::warn!("[const-api][runtime] {warning}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::default_config;
    use warp::Filter;

    #[test]
    fn saved_platform_key_is_not_enabled_before_legal_reconciliation() {
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("client.json");
        let mut config = default_config();
        config.account_device_api_key = "platform-device-key".to_string();

        let state = create_runtime_state(config_path, config);

        assert_eq!(state.platform_api_key.lock().unwrap().as_str(), "");
    }

    #[tokio::test]
    async fn enabled_channel_becomes_ready_before_platform_is_online() {
        let models = warp::path!("v1" / "models").map(|| {
            warp::reply::json(&serde_json::json!({
                "object": "list",
                "data": [{"id": "local-ready-model", "object": "model"}]
            }))
        });
        let responses = warp::path!("v1" / "responses").and(warp::body::json()).map(
            |_body: serde_json::Value| {
                warp::reply::json(&serde_json::json!({
                    "id": "resp_ready",
                    "output": [{"content": [{"text": "ready ok"}]}],
                    "usage": {"input_tokens": 1, "output_tokens": 1}
                }))
            },
        );
        let chat = warp::path!("v1" / "chat" / "completions")
            .and(warp::body::json())
            .map(|_body: serde_json::Value| {
                warp::reply::json(&serde_json::json!({
                    "id": "chat_ready",
                    "object": "chat.completion",
                    "model": "local-ready-model",
                    "choices": [{"message": {"role": "assistant", "content": "ready ok"}}],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1}
                }))
            });
        let (upstream_addr, upstream_server) =
            crate::bind_ephemeral!(models.or(responses).or(chat), ([127, 0, 0, 1], 0));
        let upstream_task = tokio::spawn(upstream_server);

        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("client.json");
        let mut config = default_config();
        let channel = &mut config.channels[0];
        channel.enabled = true;
        channel.share_enabled = false;
        channel.server_ws_url = "ws://127.0.0.1:9/supplier/ws".to_string();
        channel.server_quic_url.clear();
        channel.upstream_base_url = format!("http://{upstream_addr}/v1");
        channel.surface_bindings = crate::source_driver_surface_bindings(
            channel.source_driver(),
            &channel.upstream_base_url,
        );
        for binding in &mut channel.surface_bindings {
            binding.verification.state = "verified".to_string();
            for protocol in &mut binding.protocols {
                protocol.verification.state = "verified".to_string();
            }
        }
        config.account_device_api_key.clear();
        config.supplier_auto_start = false;
        let config = crate::normalize_config(config);
        assert!(config.supplier_auto_start);
        assert!(config.channels[0].share_enabled);
        crate::write_config_to_path(&config_path, &config).unwrap();

        let state = create_runtime_state(config_path, config.clone());
        let status = crate::supplier::start_supplier_inner(&state, None, None)
            .await
            .expect("start local channel while platform is offline");

        assert!(status.running);
        assert_eq!(status.channels.len(), 1);
        assert_eq!(status.channels[0].status, "available");
        assert!(!status.channels[0].message.is_empty());
        assert_eq!(
            state
                .local_channel_readiness
                .lock()
                .unwrap()
                .get(&config.channels[0].id),
            Some(&true)
        );
        assert!(
            status
                .channel_transports
                .iter()
                .all(|transport| !transport.platform_registered)
        );

        crate::supplier::stop_supplier_inner(&state)
            .await
            .expect("stop supplier runtime");
        upstream_task.abort();
    }

    #[tokio::test]
    async fn local_only_activation_is_atomic_and_weak_refresh_failure_preserves_readiness() {
        let models = warp::path!("v1" / "models").map(|| {
            warp::reply::json(&serde_json::json!({
                "object": "list",
                "data": [{"id": "lan-model", "object": "model"}]
            }))
        });
        let chat = warp::path!("v1" / "chat" / "completions")
            .and(warp::post())
            .map(|| {
                warp::reply::json(&serde_json::json!({
                    "id": "chatcmpl-lan-health",
                    "object": "chat.completion",
                    "model": "lan-model",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "ok"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                }))
            });
        let (upstream_addr, upstream_server) =
            crate::bind_ephemeral!(models.or(chat), ([127, 0, 0, 1], 0));
        let upstream_task = tokio::spawn(upstream_server);

        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("client.json");
        let mut config = default_config();
        let channel = &mut config.channels[0];
        channel.set_source_driver(crate::source_driver::SourceDriverId::LanShare);
        channel.kind = "lan_share".to_string();
        channel.enabled = false;
        channel.share_enabled = false;
        channel.upstream_base_url = format!("http://{upstream_addr}/v1");
        channel.v2.credential_ref = "member-key".to_string();
        channel.models = vec!["lan-model".to_string()];
        channel.surface_bindings = crate::source_driver_surface_bindings(
            channel.source_driver(),
            &channel.upstream_base_url,
        );
        for binding in &mut channel.surface_bindings {
            if binding.surface != crate::surface::ApiSurface::OpenAi {
                continue;
            }
            binding.verification.state = "verified".to_string();
            for protocol in &mut binding.protocols {
                protocol.verification.state = "verified".to_string();
            }
        }
        let config = crate::write_config_to_path(&config_path, &config).unwrap();
        let channel_id = config.channels[0].id.clone();
        let state = create_runtime_state(config_path, config);

        let activated =
            crate::supplier::activate_supplier_channel_inner(&state, channel_id.clone())
                .await
                .expect("activate reachable LAN host");
        assert_eq!(
            activated
                .channels
                .iter()
                .find(|health| health.channel_id == channel_id)
                .map(|health| health.status.as_str()),
            Some("available")
        );
        let saved = crate::load_config_from_path(&state.config_path).unwrap();
        assert!(saved.channels[0].enabled);
        assert!(!saved.channels[0].share_enabled);
        assert_eq!(
            state
                .local_channel_readiness
                .lock()
                .unwrap()
                .get(&channel_id),
            Some(&true)
        );

        upstream_task.abort();
        let _ = upstream_task.await;
        crate::supplier::refresh_local_only_channel_health_once(&state)
            .await
            .expect("ignore weak unreachable LAN snapshot");
        assert_eq!(
            state
                .local_channel_readiness
                .lock()
                .unwrap()
                .get(&channel_id),
            Some(&true)
        );
        assert_eq!(
            state.supplier.lock().await.status().channels[0].status,
            "available"
        );
        assert!(
            state
                .supplier
                .lock()
                .await
                .status()
                .channel_transports
                .is_empty(),
            "local-only channel must not create a platform supplier transport"
        );
    }

    #[tokio::test]
    async fn summary_tracks_live_config_and_explicit_reload_without_polling_disk() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("client.json");
        let mut config = default_config();
        config.channels.truncate(1);
        config.channels[0].enabled = true;
        let id = config.channels[0].id.clone();
        let state = create_runtime_state(path.clone(), config);
        state
            .local_channel_readiness
            .lock()
            .unwrap()
            .insert(id, true);
        state.proxy.lock().await.running = true;
        let supervisor = RuntimeSupervisor::new(state.clone());

        // No config file exists yet: status describes the live request state.
        let summary = supervisor.summary().await;
        assert_eq!(summary.enabled_local_channels, 1);
        assert_eq!(summary.ready_local_channels, 1);
        assert_eq!(summary.readiness, "ready");
        assert!(!path.exists());

        let mut saved = state.proxy_config.lock().unwrap().clone();
        saved.channels[0].enabled = false;
        write_config_to_path(&path, &saved).unwrap();
        assert_eq!(supervisor.summary().await.ready_local_channels, 1);
        crate::config::reload_config_authoritatively(&path, &state.proxy_config).unwrap();
        let summary = supervisor.summary().await;
        assert_eq!(summary.enabled_local_channels, 0);
        assert_eq!(summary.ready_local_channels, 0);
        assert_eq!(summary.readiness, "degraded");

        crate::config::commit_config_update(&path, &state.proxy_config, |cfg| {
            cfg.channels[0].enabled = true;
            Ok(())
        })
        .unwrap();
        assert_eq!(supervisor.summary().await.ready_local_channels, 1);
    }

    #[tokio::test]
    async fn constructs_without_tauri_and_starts_and_stops_proxy() {
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("client.json");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut config = default_config();
        config.listen = format!("127.0.0.1:{port}");
        config.supplier_auto_start = false;
        write_config_to_path(&config_path, &config).unwrap();
        let state = create_runtime_state(config_path, config);
        let supervisor = RuntimeSupervisor::new(state);

        let started = supervisor.start().await.unwrap();
        assert!(started.proxy_running);
        assert_eq!(started.proxy_listen, format!("127.0.0.1:{port}"));
        supervisor.shutdown().await;
        assert!(!supervisor.summary().await.proxy_running);

        std::net::TcpListener::bind(("127.0.0.1", port))
            .expect("proxy port should be reusable after shutdown");
    }

    #[tokio::test]
    async fn start_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("client.json");
        let mut config = default_config();
        config.proxy_auto_start = false;
        write_config_to_path(&config_path, &config).unwrap();
        let supervisor = RuntimeSupervisor::new(create_runtime_state(config_path, config));
        supervisor.start().await.unwrap();
        let tasks_after_first_start = supervisor.control.lock().await.tasks.len();
        assert_eq!(tasks_after_first_start, 8);
        supervisor.start().await.unwrap();
        assert_eq!(
            supervisor.control.lock().await.tasks.len(),
            tasks_after_first_start
        );
        supervisor.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_cannot_be_undone_by_a_queued_start() {
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("client.json");
        let mut config = default_config();
        config.proxy_auto_start = false;
        write_config_to_path(&config_path, &config).unwrap();
        let supervisor = RuntimeSupervisor::new(create_runtime_state(config_path, config));
        let starting = {
            let supervisor = supervisor.clone();
            tokio::spawn(async move { supervisor.start().await })
        };

        supervisor.shutdown().await;
        let start_result = starting.await.unwrap();
        assert!(
            start_result.is_ok()
                || start_result
                    .as_ref()
                    .is_err_and(|error| error.contains("shutdown"))
        );
        let summary = supervisor.summary().await;
        assert_eq!(summary.lifecycle, RuntimeLifecycle::Stopped);
        assert!(!summary.proxy_running);
        assert!(supervisor.control.lock().await.tasks.is_empty());
    }
}
