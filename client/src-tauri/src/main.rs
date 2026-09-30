#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]
// These lints mostly describe API shape or mechanical style. Tauri commands,
// protocol adapters and cross-platform cfg branches intentionally keep their
// current signatures and explicit forms; correctness-focused Clippy lints stay
// enabled and are denied by CI.
#![allow(
    clippy::blocks_in_conditions,
    clippy::cloned_ref_to_slice_refs,
    clippy::collapsible_if,
    clippy::collapsible_match,
    clippy::derivable_impls,
    clippy::double_ended_iterator_last,
    clippy::field_reassign_with_default,
    clippy::filter_map_bool_then,
    clippy::filter_next,
    clippy::if_same_then_else,
    clippy::iter_overeager_cloned,
    clippy::manual_clamp,
    clippy::manual_contains,
    clippy::manual_is_multiple_of,
    clippy::manual_repeat_n,
    clippy::map_clone,
    clippy::map_identity,
    clippy::match_result_ok,
    clippy::needless_as_bytes,
    clippy::needless_borrow,
    clippy::needless_borrows_for_generic_args,
    clippy::needless_lifetimes,
    clippy::needless_return,
    clippy::ptr_arg,
    clippy::question_mark,
    clippy::redundant_closure,
    clippy::redundant_closure_call,
    clippy::redundant_pattern_matching,
    clippy::single_element_loop,
    clippy::single_match,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::unnecessary_lazy_evaluations,
    clippy::unnecessary_map_or,
    clippy::unnecessary_mut_passed,
    clippy::useless_conversion
)]

macro_rules! eprintln {
    ($($arg:tt)*) => {
        log::warn!($($arg)*)
    };
}

#[cfg(test)]
#[macro_export]
macro_rules! bind_ephemeral {
    ($filter:expr, $address:expr) => {{
        let bind_address: std::net::SocketAddr = $address.into();
        let listener = tokio::net::TcpListener::bind(bind_address)
            .await
            .expect("bind ephemeral Warp listener");
        let address = listener
            .local_addr()
            .expect("read ephemeral Warp listener address");
        let server = warp::serve($filter).incoming(listener).run();
        (address, server)
    }};
}

mod account;
mod local_policy;

mod announcements;
mod antigravity_models;
mod autostart;
mod catalog_registry;
mod channel_executor;
mod channel_identity;
mod channel_surface;
mod channel_user_agent;
mod claude_client_profile;
mod client_toasts;
mod codex_identity;
mod coding_gateway;
mod coding_plan;
mod conditional_document;
mod config;
mod debug_console;
mod detection;
mod endpoint;

mod instance;
mod lan_share;
mod launch;
mod legal_notice;
mod logging;
#[cfg(all(debug_assertions, feature = "memory-diagnostics"))]
mod memory_diagnostics;
mod model;
mod model_catalog;
mod model_compatibility;
mod model_discovery;
mod native_i18n;
#[cfg(target_os = "macos")]
mod native_menu;
mod openrouter;
mod output_buffer;
mod platform_transport;
mod protocol;
mod proxy;
mod realtime_media;
mod release_sources;
mod runtime;
mod source_driver;
mod sse_buffer;
mod subscription_oauth_callback;
mod subscription_oauth_error;
mod supplier;
mod surface;
mod surface_wire;
mod tool_config;
mod tool_model_metadata;
mod update_activity;
mod update_relaunch;
mod update_sources;
mod upstream_failure;
mod upstream_transport;

use account::*;
use channel_identity::*;
use claude_client_profile::*;
use client_toasts::*;
use codex_identity::*;
use config::*;
use debug_console::*;
use detection::*;
use endpoint::*;
use instance::*;
use lan_share::*;
use launch::*;
use legal_notice::*;
use model::*;
use model_catalog::*;
use model_compatibility::*;
use native_i18n::text as native_text;
use proxy::*;
use runtime::*;
use source_driver::*;
use subscription_oauth_error::*;
use supplier::*;
use tool_config::*;
use tool_model_metadata::*;
use update_relaunch::*;
use update_sources::*;

use anyhow::{Context, Result, anyhow};
use base64::engine::general_purpose::STANDARD as BASE64;
use futures_util::FutureExt;
#[cfg(test)]
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::fs;
#[cfg(test)]
use std::path::Path;
#[cfg(debug_assertions)]
use std::time::{SystemTime, UNIX_EPOCH};
use std::{
    any::Any,
    future::Future,
    net::{IpAddr, SocketAddr},
    panic::AssertUnwindSafe,
    process::ExitCode,
    sync::{
        Arc, Mutex as StdMutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tauri::{Manager, State};
#[cfg(test)]
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
#[cfg(test)]
use tokio::sync::mpsc;
use tokio::sync::{Mutex, oneshot};
use warp::Filter;
#[cfg(test)]
use warp::{Reply, http::StatusCode};

#[cfg(test)]
pub(crate) async fn test_body_bytes<B>(body: B) -> std::result::Result<bytes::Bytes, B::Error>
where
    B: http_body::Body<Data = bytes::Bytes>,
{
    Ok(http_body_util::BodyExt::collect(body).await?.to_bytes())
}

#[cfg(test)]
pub(crate) fn test_stream_response<S, B, E>(stream: S) -> warp::reply::Response
where
    S: futures_util::Stream<Item = std::result::Result<B, E>> + Send + 'static,
    B: Into<bytes::Bytes>,
    E: Into<Box<dyn std::error::Error + Send + Sync>> + Send + 'static,
{
    warp::reply::stream(ThreadSafeStream::new(stream)).into_response()
}

#[cfg(all(debug_assertions, feature = "dangerous-raw-traffic-debug"))]
pub(crate) fn raw_debug_logs_enabled() -> bool {
    match std::env::var("CONST_API_RAW_DEBUG") {
        Ok(value) => {
            let value = value.trim().to_ascii_lowercase();
            matches!(value.as_str(), "1" | "true" | "on" | "yes")
        }
        // Raw traffic logging serializes every request and every streaming
        // chunk. Keep it opt-in even in development so ordinary dev builds
        // have production-like latency and disk behavior.
        Err(_) => false,
    }
}

#[cfg(not(all(debug_assertions, feature = "dangerous-raw-traffic-debug")))]
pub(crate) fn raw_debug_logs_enabled() -> bool {
    false
}

const LOCAL_PLACEHOLDER_KEY: &str = "<local-anything>";
const OPENAI_OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OPENAI_OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CODEX_MODELS_URL: &str = "https://chatgpt.com/backend-api/codex/models";
const CODEX_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";
const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const CLAUDE_OAUTH_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const CLAUDE_OAUTH_AUTHORIZE_URL: &str = "https://claude.com/cai/oauth/authorize";
const CLAUDE_OAUTH_REDIRECT_URI: &str = "https://platform.claude.com/oauth/code/callback";
const CLAUDE_OAUTH_TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const CLAUDE_AI_OAUTH_SCOPE: &str =
    "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
const CLAUDE_OAUTH_AUTHORIZE_SCOPE: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
const CLAUDE_MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages?beta=true";
const CLAUDE_OAUTH_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const GROK_OAUTH_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const GROK_OAUTH_AUTHORIZE_URL: &str = "https://auth.x.ai/oauth2/authorize";
const GROK_OAUTH_TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
const GROK_OAUTH_REDIRECT_URI: &str = "http://127.0.0.1:56121/callback";
const GROK_OAUTH_SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
const GROK_MODELS_URL: &str = "https://cli-chat-proxy.grok.com/v1/models";
const GROK_RESPONSES_URL: &str = "https://cli-chat-proxy.grok.com/v1/responses";
const GROK_BILLING_WEEKLY_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
const GROK_BILLING_MONTHLY_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing";
// Official https://x.ai/cli/stable, checked 2026-09-30.
const GROK_CLI_VERSION: &str = "1.0.44";
const GROK_CLI_USER_AGENT: &str = "xai-grok-workspace/1.0.44";
const GROK_BILLING_USER_AGENT: &str = "grok-pager/1.0.44 grok-shell/1.0.44 (macos; aarch64)";
const ANTIGRAVITY_OAUTH_CLIENT_ID: &str = match option_env!("CONST_LOCAL_ANTIGRAVITY_CLIENT_ID") { Some(value) => value, None => "" };
const ANTIGRAVITY_OAUTH_CLIENT_SECRET: &str = match option_env!("CONST_LOCAL_ANTIGRAVITY_CLIENT_SECRET") { Some(value) => value, None => "" };
const ANTIGRAVITY_OAUTH_AUTHORIZE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const ANTIGRAVITY_OAUTH_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const ANTIGRAVITY_USERINFO_URL: &str = "https://www.googleapis.com/oauth2/v2/userinfo?alt=json";
const ANTIGRAVITY_API_BASE_URL: &str = "https://cloudcode-pa.googleapis.com";
const ANTIGRAVITY_DAILY_API_BASE_URL: &str = "https://daily-cloudcode-pa.googleapis.com";
const ANTIGRAVITY_REDIRECT_URI: &str = "http://localhost:51121/oauth-callback";
const ANTIGRAVITY_SCOPES: &str = "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile https://www.googleapis.com/auth/cclog https://www.googleapis.com/auth/experimentsandconfigs";
// Official Antigravity Hub updater manifest, checked 2026-09-30.
const ANTIGRAVITY_DEFAULT_USER_AGENT_VERSION: &str = "2.18.1";
const ANTIGRAVITY_GOOG_API_CLIENT: &str = "gl-node/22.21.1";
const SKIP_LOCAL_SHORT_CIRCUIT_HEADER: &str = "x-const-api-skip-local";
const USE_LOCAL_SHORT_CIRCUIT_HEADER: &str = "x-const-api-use-local";
const MAX_PRICE_RATIO_HEADER: &str = "x-const-api-max-price-ratio";
const ALLOW_UNVERIFIED_PLATFORM_ROUTES_HEADER: &str = "x-const-api-allow-unverified-routes";
const SUPPLIER_TRANSPORT_FRAME_COMPRESSION_FEATURE: &str = "zstd_frame_v2";
const SUPPLIER_TRANSPORT_STREAM_RESUME_FEATURE: &str = "stream_resume_v1";
const SUPPLIER_TRANSPORT_STREAM_PROGRESS_FEATURE: &str = "stream_progress_v1";
const SUPPLIER_TRANSPORT_COMPRESSION_MIN_BYTES: usize = 1 << 10;
const SUPPLIER_TRANSPORT_COMPRESSION_MAX_RAW_LEN: usize = 64 << 20;
const SUPPLIER_TRANSPORT_FRAME_HEADER_BYTES: usize = 12;
const SUPPLIER_TRANSPORT_FRAME_MAGIC: [u8; 4] = [0, b'C', b'Z', b'2'];
#[cfg(target_os = "macos")]
const MACOS_TRAY_ICON_BYTES: &[u8] = include_bytes!("../icons/macos-tray.png");
const DESKTOP_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const DEV_RESET_RELAUNCH_MANAGED_ENV: &str = "CONST_API_DEV_RESET_RELAUNCH_MANAGED";
const LOCAL_DATA_RESET_MARKER_SUFFIX: &str = ".reset-pending";
const LOCAL_DATA_RESET_DELETE_TIMEOUT: Duration = Duration::from_secs(10);
const SUPPLIER_QUIC_ALPN: &[u8] = b"const-api-supplier/1";
pub(crate) const MODEL_REQUEST_TIMEOUT: Duration = Duration::from_secs(15 * 60);
static SHORT_HTTP_CLIENT: OnceLock<Client> = OnceLock::new();
static LONG_HTTP_CLIENT: OnceLock<Client> = OnceLock::new();

pub(crate) fn antigravity_user_agent_version() -> String {
    std::env::var("ANTIGRAVITY_USER_AGENT_VERSION")
        .ok()
        .filter(|version| {
            let segments = version.trim().split('.').collect::<Vec<_>>();
            segments.len() == 3
                && segments.iter().all(|segment| {
                    !segment.is_empty() && segment.chars().all(|ch| ch.is_ascii_digit())
                })
        })
        .unwrap_or_else(|| ANTIGRAVITY_DEFAULT_USER_AGENT_VERSION.to_string())
}

pub(crate) fn antigravity_user_agent() -> String {
    format!(
        "antigravity/hub/{} windows/amd64",
        antigravity_user_agent_version()
    )
}

pub(crate) fn antigravity_control_plane_user_agent() -> String {
    format!(
        "{} google-api-nodejs-client/10.3.0",
        antigravity_user_agent()
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ToolConfigUrls {
    root: String,
    openai: String,
    anthropic: String,
    gemini: String,
}

fn tool_config_urls(cfg: &ClientConfig) -> Result<ToolConfigUrls, String> {
    let configured: SocketAddr = cfg
        .listen
        .trim()
        .parse()
        .map_err(|err| format!("invalid listen address: {err}"))?;
    let root = format!("http://127.0.0.1:{}", configured.port());
    Ok(ToolConfigUrls {
        openai: format!("{root}/v1"),
        anthropic: format!("{root}/anthropic"),
        gemini: format!("{root}/gemini"),
        root,
    })
}

fn tool_config_root_for_tool<'a>(urls: &'a ToolConfigUrls, tool: &str) -> &'a str {
    match tool {
        "claude" | "claude-desktop" | "claude-science" => &urls.anthropic,
        "gemini" => &urls.gemini,
        _ => &urls.root,
    }
}
#[cfg(debug_assertions)]
const STARTUP_COMMAND_LOG_WINDOW_MS: u128 = 60_000;
#[cfg(debug_assertions)]
static STARTUP_LOG_ZERO: OnceLock<Instant> = OnceLock::new();

#[cfg(debug_assertions)]
fn startup_elapsed_ms() -> u128 {
    let zero = STARTUP_LOG_ZERO.get_or_init(Instant::now);
    zero.elapsed().as_millis()
}

pub(crate) fn short_http_client() -> &'static Client {
    SHORT_HTTP_CLIENT.get_or_init(|| {
        Client::builder()
            .timeout(Duration::from_secs(30))
            .retry(reqwest::retry::never())
            .build()
            .expect("short http client")
    })
}

pub(crate) fn long_http_client() -> &'static Client {
    LONG_HTTP_CLIENT.get_or_init(|| {
        Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(MODEL_REQUEST_TIMEOUT)
            .retry(reqwest::retry::never())
            .build()
            .expect("long http client")
    })
}

#[cfg(debug_assertions)]
fn startup_log(message: impl AsRef<str>) {
    let since_start_ms = startup_elapsed_ms();
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    log::debug!(
        "[const-api][startup][+{}ms][unix_ms={}] {}",
        since_start_ms,
        now_ms,
        message.as_ref()
    );
}

#[cfg(not(debug_assertions))]
fn startup_log(_message: impl AsRef<str>) {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecoveredPanic {
    pub(crate) scope: String,
    pub(crate) message: String,
}

pub(crate) async fn catch_runtime_panic<F, T>(
    scope: impl Into<String>,
    future: F,
) -> Result<T, RecoveredPanic>
where
    F: Future<Output = T>,
{
    let scope = scope.into();
    match AssertUnwindSafe(future).catch_unwind().await {
        Ok(value) => Ok(value),
        Err(payload) => {
            let recovered = RecoveredPanic {
                scope,
                message: panic_payload_message(payload.as_ref()),
            };
            log_runtime_panic(&recovered);
            Err(recovered)
        }
    }
}

pub(crate) fn spawn_logged<F>(scope: &'static str, future: F) -> tokio::task::JoinHandle<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        let _ = catch_runtime_panic(scope, future).await;
    })
}

pub(crate) fn spawn_supervised<F, Fut>(
    scope: &'static str,
    restart_delay: Duration,
    task: F,
) -> tokio::task::JoinHandle<()>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(supervise_runtime_task(scope, restart_delay, task))
}

async fn supervise_runtime_task<F, Fut>(scope: &'static str, restart_delay: Duration, mut task: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = ()>,
{
    loop {
        if catch_runtime_panic(scope, task()).await.is_ok() {
            return;
        }
        tokio::time::sleep(restart_delay).await;
    }
}

fn panic_payload_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "non-string panic payload".to_string()
}

fn log_runtime_panic(recovered: &RecoveredPanic) {
    let message = format!(
        "[panic][{}] recovered runtime panic: {}",
        recovered.scope, recovered.message
    );
    log::error!("{message}");
}

#[cfg(debug_assertions)]
fn startup_log_early_command(message: impl AsRef<str>) {
    if startup_elapsed_ms() <= STARTUP_COMMAND_LOG_WINDOW_MS {
        startup_log(message);
    }
}

#[cfg(not(debug_assertions))]
fn startup_log_early_command(_message: impl AsRef<str>) {}

pub(crate) struct DesktopLifecycle {
    runtime: RuntimeSupervisor,
    instance: Mutex<Option<InstanceOwner>>,
    quitting: AtomicBool,
    legal_notice_accepted: AtomicBool,
    tray_open: StdMutex<Option<tauri::menu::MenuItem<tauri::Wry>>>,
    tray_proxy_status: StdMutex<Option<tauri::menu::MenuItem<tauri::Wry>>>,
    tray_supplier_status: StdMutex<Option<tauri::menu::MenuItem<tauri::Wry>>>,
    tray_quit: StdMutex<Option<tauri::menu::MenuItem<tauri::Wry>>>,
}

impl DesktopLifecycle {
    fn new(
        runtime: RuntimeSupervisor,
        instance: InstanceOwner,
        legal_notice_accepted: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            instance: Mutex::new(Some(instance)),
            quitting: AtomicBool::new(false),
            legal_notice_accepted: AtomicBool::new(legal_notice_accepted),
            tray_open: StdMutex::new(None),
            tray_proxy_status: StdMutex::new(None),
            tray_supplier_status: StdMutex::new(None),
            tray_quit: StdMutex::new(None),
        })
    }

    fn request_quit(self: &Arc<Self>, app: tauri::AppHandle) {
        if self.quitting.swap(true, Ordering::SeqCst) {
            return;
        }
        log::info!("[const-api][shutdown] desktop quit requested");
        // End the native event loop first so the window and tray disappear
        // immediately. Owned runtime resources are drained, with a deadline,
        // after run_return completes.
        app.exit(0);
    }

    pub(crate) fn request_restart(
        self: &Arc<Self>,
        app: tauri::AppHandle,
        restart_mode: LaunchMode,
    ) {
        if self.quitting.swap(true, Ordering::SeqCst) {
            return;
        }
        log::info!(
            "[const-api][update] restarting after install with mode={}",
            restart_mode.as_str()
        );
        let lifecycle = self.clone();
        spawn_logged("desktop restart", async move {
            // Release the listener and instance lease before Tauri spawns the
            // replacement process. Otherwise it can observe this process as a
            // second desktop invocation and exit instead of completing update.
            lifecycle.shutdown_owned_resources().await;
            app.request_restart();
        });
    }

    async fn shutdown_owned_resources(&self) {
        let shutdown_started_at = Instant::now();
        log::info!("[const-api][shutdown] owned resource cleanup begin");
        let step_started_at = Instant::now();
        self.runtime.shutdown().await;
        log::info!(
            "[const-api][shutdown] runtime stopped in {}ms",
            step_started_at.elapsed().as_millis()
        );
        let step_started_at = Instant::now();
        if let Some(mut instance) = self.instance.lock().await.take() {
            instance.update_runtime_status(self.runtime.summary().await.control_status());
            instance.shutdown().await;
        }
        log::info!(
            "[const-api][shutdown] instance released in {}ms",
            step_started_at.elapsed().as_millis()
        );
        log::info!(
            "[const-api][shutdown] owned resource cleanup complete in {}ms",
            shutdown_started_at.elapsed().as_millis()
        );
        if let Err(error) = flush_subscription_safety_writer() {
            log::warn!("[const-api][shutdown] flush subscription safety state failed: {error}");
        }
        logging::flush_client_logging();
    }

    async fn refresh_runtime_status(&self) {
        let summary = self.runtime.summary().await;
        if let Some(instance) = self.instance.lock().await.as_ref() {
            instance.update_runtime_status(summary.control_status());
        }
        self.update_tray_status(&summary);
    }

    fn set_tray_items(
        &self,
        open: tauri::menu::MenuItem<tauri::Wry>,
        proxy: tauri::menu::MenuItem<tauri::Wry>,
        supplier: tauri::menu::MenuItem<tauri::Wry>,
        quit: tauri::menu::MenuItem<tauri::Wry>,
    ) {
        if let Ok(mut item) = self.tray_open.lock() {
            *item = Some(open);
        }
        if let Ok(mut item) = self.tray_proxy_status.lock() {
            *item = Some(proxy);
        }
        if let Ok(mut item) = self.tray_supplier_status.lock() {
            *item = Some(supplier);
        }
        if let Ok(mut item) = self.tray_quit.lock() {
            *item = Some(quit);
        }
    }

    fn update_tray_status(&self, summary: &RuntimeSummary) {
        if self.quitting.load(Ordering::SeqCst) {
            return;
        }
        if let Ok(item) = self.tray_open.lock() {
            if let Some(item) = item.as_ref() {
                let _ = item.set_text(native_text("打开 CONST API", "Open CONST API"));
            }
        }
        if let Ok(item) = self.tray_proxy_status.lock() {
            if let Some(item) = item.as_ref() {
                let _ = item.set_text(if summary.proxy_running {
                    native_text("本地 API：运行中", "Local API: running")
                } else {
                    native_text("本地 API：已停止", "Local API: stopped")
                });
            }
        }
        if let Ok(item) = self.tray_supplier_status.lock() {
            if let Some(item) = item.as_ref() {
                let _ = item.set_text(if summary.supplier_running {
                    native_text("供应：运行中", "Supply: running")
                } else {
                    native_text("供应：已停止", "Supply: stopped")
                });
            }
        }
        if let Ok(item) = self.tray_quit.lock() {
            if let Some(item) = item.as_ref() {
                let _ = item.set_text(native_text("退出 CONST API", "Quit CONST API"));
            }
        }
    }

    async fn finish_after_event_loop(&self) {
        match tokio::time::timeout(
            DESKTOP_SHUTDOWN_TIMEOUT,
            catch_runtime_panic(
                "desktop shutdown resources",
                self.shutdown_owned_resources(),
            ),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {
                log::error!(
                    "[const-api][shutdown] resource cleanup panicked; process exit will continue"
                );
            }
            Err(_) => {
                log::error!(
                    "[const-api][shutdown] resource cleanup exceeded {}ms; process exit will continue",
                    DESKTOP_SHUTDOWN_TIMEOUT.as_millis()
                );
            }
        }
    }
}

fn main() -> ExitCode {
    let mode = match parse_args(std::env::args_os().skip(1)) {
        Ok(mode) => mode,
        Err(error) => {
            report_launch_error(LaunchMode::Desktop, error.message());
            return ExitCode::from(2);
        }
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            report_launch_error(
                mode,
                &format!("failed to initialize async runtime: {error}"),
            );
            return ExitCode::FAILURE;
        }
    };
    // Share the process-owned runtime before Tauri can initialize its default
    // one. It stays alive through run_mode and desktop resource shutdown.
    tauri::async_runtime::set(runtime.handle().clone());
    runtime.block_on(run_mode(mode))
}

fn local_data_reset_marker_path(data_root: &std::path::Path) -> Result<std::path::PathBuf> {
    let directory_name = data_root
        .file_name()
        .context("local app data directory name is unavailable")?;
    let marker_name = format!(
        "{}{}",
        directory_name.to_string_lossy(),
        LOCAL_DATA_RESET_MARKER_SUFFIX
    );
    Ok(data_root.with_file_name(marker_name))
}

async fn apply_pending_local_data_reset(config_path: &std::path::Path) -> Result<bool> {
    let data_root = config_path
        .parent()
        .context("local app data directory is unavailable")?;
    let marker_path = local_data_reset_marker_path(data_root)?;
    if !marker_path.is_file() {
        return Ok(false);
    }
    // Config-path overrides are used by debug builds and isolated tests. They
    // are safe to run, but they must never become deletion
    // targets. Validate the fixed application data root only after a reset is
    // actually pending so ordinary isolated launches remain possible.
    if data_root != client_data_root() {
        return Err(anyhow!(
            "refusing to clear an unexpected local data directory"
        ));
    }

    let deadline = Instant::now() + LOCAL_DATA_RESET_DELETE_TIMEOUT;
    loop {
        if !data_root.exists() {
            if marker_path.exists() {
                std::fs::remove_file(&marker_path).with_context(|| {
                    format!("remove local data reset marker {}", marker_path.display())
                })?;
            }
            return Ok(true);
        }
        match std::fs::remove_dir_all(data_root) {
            Ok(()) => {
                if marker_path.exists() {
                    std::fs::remove_file(&marker_path).with_context(|| {
                        format!("remove local data reset marker {}", marker_path.display())
                    })?;
                }
                return Ok(true);
            }
            Err(_error) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(error) => {
                return Err(anyhow!(
                    "clear local app data {}: {error}",
                    data_root.display()
                ));
            }
        }
    }
}

async fn run_mode(requested_mode: LaunchMode) -> ExitCode {
    let config_path = default_config_path();
    if let Err(error) = apply_pending_local_data_reset(&config_path).await {
        report_launch_error(
            requested_mode,
            &format!("local data reset failed before restart: {error:#}"),
        );
        return ExitCode::from(1);
    }
    let config_root = config_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    if let Err(error) = logging::initialize_client_logging(false, config_root) {
        report_launch_error(
            requested_mode,
            &format!("failed to initialize client logging: {error:#}"),
        );
    }
    #[cfg(debug_assertions)]
    STARTUP_LOG_ZERO.get_or_init(Instant::now);

    let state_dir = state_dir_for_config(&config_path);
    // A version-bound marker is the authoritative hand-off because the Windows
    // updater exits inside install() and relaunches through an installer
    // process. It also avoids mutating the process environment after the
    // multithreaded runtime has started, which is unsafe in Rust 2024.
    let marker_mode = match peek_update_relaunch_mode(&state_dir, env!("CARGO_PKG_VERSION")) {
        Ok(mode) => mode,
        Err(error) => {
            log::warn!("[const-api][update] ignored invalid relaunch marker: {error:#}");
            None
        }
    };
    let update_mode = marker_mode;
    let mode = resolve_update_relaunch_mode(requested_mode, update_mode);
    let update_marker_pending = marker_mode.is_some();
    let restored_update_tray = mode == LaunchMode::Tray && update_mode == Some(LaunchMode::Tray);
    if update_mode.is_some() {
        log::info!(
            "[const-api][update] restoring post-update launch mode: {} -> {} source=marker",
            requested_mode.as_str(),
            mode.as_str()
        );
    }

    let mut owner = match InstanceOwner::acquire(&state_dir, mode).await {
        Ok(AcquireOutcome::Acquired(owner)) => owner,
        Ok(AcquireOutcome::Existing(existing)) => {
            log::info!(
                "[const-api][instance] existing desktop detected mode={} pid={} version={}",
                existing.mode.as_str(),
                existing.pid,
                existing.version
            );
            return handle_existing_instance(mode, &state_dir).await;
        }
        Err(error) => {
            report_launch_error(mode, &format!("instance coordination failed: {error:#}"));
            return ExitCode::from(1);
        }
    };
    let runtime_state = match load_or_create_runtime_state(config_path.clone()) {
        Ok(state) => state,
        Err(error) => {
            report_launch_error(mode, &format!("configuration startup failed: {error:#}"));
            owner.shutdown().await;
            return ExitCode::from(1);
        }
    };
    let runtime = RuntimeSupervisor::new(runtime_state);
    match run_desktop_host(
        mode,
        restored_update_tray,
        update_marker_pending,
        runtime,
        owner,
    )
    .await
    {
        Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
        Err(error) => {
            report_launch_error(mode, &format!("desktop startup failed: {error:#}"));
            ExitCode::from(1)
        }
    }
}

async fn handle_existing_instance(requested: LaunchMode, state_dir: &std::path::Path) -> ExitCode {
    match requested {
        LaunchMode::Desktop => match send_show(state_dir).await {
            Ok(_) => ExitCode::SUCCESS,
            Err(error) => {
                report_launch_error(
                    requested,
                    &format!("active desktop could not be shown: {error:#}"),
                );
                ExitCode::from(1)
            }
        },
        LaunchMode::Tray => ExitCode::SUCCESS,
    }
}

async fn run_desktop_host(
    mode: LaunchMode,
    restored_update_tray: bool,
    update_marker_pending: bool,
    runtime: RuntimeSupervisor,
    mut owner: InstanceOwner,
) -> Result<i32> {
    #[cfg(not(target_os = "macos"))]
    let _ = restored_update_tray;
    let show_requests = owner
        .take_show_requests()
        .ok_or_else(|| anyhow!("instance show receiver already taken"))?;
    let accepted_on_start = legal_notice_accepted(&runtime.state().config_path);
    let update_marker_state_dir = state_dir_for_config(&runtime.state().config_path);
    let lifecycle = DesktopLifecycle::new(runtime.clone(), owner, accepted_on_start);
    let setup_lifecycle = lifecycle.clone();
    let automatic_updates = runtime
        .state()
        .proxy_config
        .lock()
        .map(|config| config.automatic_updates)
        .unwrap_or(true);
    let native_update_manager = NativeUpdateManager::new(automatic_updates);
    let setup_update_manager = native_update_manager.clone();
    let setup_mode = mode;
    let page_mode = mode;
    let page_shown = Arc::new(AtomicBool::new(false));
    let page_shown_handler = page_shown.clone();
    let app_started_at = Instant::now();
    startup_log("tauri builder init");
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--tray"]),
        ));
    #[cfg(target_os = "macos")]
    let builder = builder.menu(|app| native_menu::build(app));
    let app = builder
        .setup(move |app| {
            let setup_started_at = Instant::now();
            startup_log("setup begin");
            app.manage(setup_lifecycle.runtime.state().clone());
            app.manage(setup_lifecycle.clone());
            app.manage(setup_update_manager.clone());
            let _ = setup_mode;
            #[cfg(target_os = "macos")]
            if setup_mode == LaunchMode::Tray {
                app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            }
            let app_handle = app.handle().clone();
            initialize_client_notices(app_handle.clone());
            #[cfg(target_os = "macos")]
            if let Err(error) = refresh_macos_close_button_tooltip(&app_handle) {
                log::warn!("[const-api][window] failed to set close-button tooltip: {error}");
            }
            match apply_new_install_autostart_default(
                &app_handle,
                &setup_lifecycle.runtime.state().config_path,
            ) {
                Ok(true) => {
                    log::info!("[const-api][autostart] enabled login startup for new installation")
                }
                Ok(false) => {}
                Err(error) => log::warn!(
                    "[const-api][autostart] failed to apply new-install default: {error:#}"
                ),
            }
            #[cfg(target_os = "macos")]
            if !development_profile_active() {
                if let Err(error) = autostart::associate_macos_launch_agent(&app_handle) {
                    log::warn!("[const-api][autostart] failed to associate login item: {error:#}");
                }
            }
            spawn_show_request_loop(app_handle.clone(), show_requests);
            if !development_profile_active() {
                start_native_update_manager(
                    app_handle.clone(),
                    setup_update_manager.clone(),
                    setup_lifecycle.clone(),
                );
            }
            let startup_lifecycle = setup_lifecycle.clone();
            spawn_logged("runtime startup", async move {
                if startup_lifecycle
                    .legal_notice_accepted
                    .load(Ordering::SeqCst)
                {
                    if let Err(error) = startup_lifecycle.runtime.start().await {
                        log::error!("[const-api][runtime] desktop startup degraded: {error}");
                    }
                }
                loop {
                    let _ = catch_runtime_panic("runtime status refresh", async {
                        startup_lifecycle.refresh_runtime_status().await;
                    })
                    .await;
                    if startup_lifecycle.quitting.load(Ordering::SeqCst) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            });
            startup_log(format!(
                "setup complete in {}ms",
                setup_started_at.elapsed().as_millis()
            ));
            Ok(())
        })
        .on_window_event({
            let lifecycle = lifecycle.clone();
            move |window, event| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    if !lifecycle.quitting.load(Ordering::SeqCst) {
                        api.prevent_close();
                        if !lifecycle.legal_notice_accepted.load(Ordering::SeqCst) {
                            lifecycle.request_quit(window.app_handle().clone());
                            return;
                        }
                        let _ = window.hide();
                        #[cfg(target_os = "macos")]
                        if let Err(error) = enter_macos_tray_mode(window.app_handle()) {
                            log::error!(
                                "[const-api][tray] failed to enter macOS tray mode: {error}"
                            );
                        }
                    }
                }
            }
        })
        .on_page_load(move |webview, payload| {
            startup_log(format!(
                "[webview] page_load {:?}, label={}, url={}",
                payload.event(),
                webview.label(),
                payload.url()
            ));
            if (page_mode == LaunchMode::Desktop || !accepted_on_start)
                && matches!(payload.event(), tauri::webview::PageLoadEvent::Finished)
                && !page_shown_handler.swap(true, Ordering::SeqCst)
            {
                show_main_window(webview.app_handle());
            }
        })
        .invoke_handler(tauri::generate_handler![
            legal_notice_status,
            accept_legal_notice,
            decline_legal_notice,
            get_config,
            save_config,
            save_supplier_config,
            get_lan_share_status,
            model_discovery::get_model_presentation,
            set_lan_share_connect_address,
            set_lan_share_models,
            create_lan_share_member,
            update_lan_share_member,
            reset_lan_share_member,
            regenerate_lan_share_member_key,
            delete_lan_share_member,
            get_lan_share_channel_status,
            set_development_endpoint,
            startup_log_from_ui,
            sync_native_theme,
            sync_native_language,
            start_proxy,
            stop_proxy,
            proxy_status,
            refresh_platform_status,
            fetch_me,
            account_status,
            account_registration_policy,
            account_request_code,
            account_register,
            account_password_login,
            account_verify_code,
            account_reset_password,
            account_refresh,
            account_logout,
            reset_local_app_data,
            account_change_password,
            account_request_email_code,
            account_verify_email,
            account_billing_summary,
            account_referral_overview,
            account_referral_activate,
            account_referral_reset_development,
            account_billing_records,
            account_billing_stats,
            account_billing_detail,
            account_transfer_funds,
            account_finance_availability,
            account_payment_orders,
            account_refunds,
            account_create_payment,
            account_refresh_payment,
            account_cancel_payment,
            account_complete_fake_payment,
            account_payout_accounts,
            account_create_payout_account,
            account_reauthenticate,
            account_usage_gifts,
            account_preview_gift_recipient,
            account_create_usage_gift,
            account_withdrawals,
            account_create_withdrawal,
            account_cancel_withdrawal,
            drain_client_toast_notices,
            request_client_attention,
            fetch_usage,
            fetch_calls,
            fetch_usage_stats,
            fetch_market_prices,
            fetch_local_channel_calls,
            fetch_local_channel_stats,
            get_model_catalog_version_info,
            fetch_model_compatibility,
            save_model_compatibility,
            reset_model_compatibility,
            fetch_providers,
            fetch_supplier_nodes,
            fetch_supplier_route_plan,
            refresh_endpoint_registry,
            get_release_source_status,
            detect_channel_upstream,
            refresh_channel_upstream,
            validate_channel_upstream,
            fetch_channel_upstream_models,
            inspect_subscription_adapter,
            scan_subscription_credentials,
            import_subscription_credential,
            create_subscription_oauth_session,
            wait_subscription_oauth_callback,
            cancel_subscription_oauth_callback,
            exchange_subscription_oauth_code,
            import_subscription_token_json,
            find_duplicate_channels,
            finalize_subscription_import,
            discard_subscription_import,
            test_local_proxy,
            debug_protocol_exchange,
            test_channel_upstream,
            activate_supplier_channel,
            start_supplier,
            recover_supplier_channel,
            notify_supplier_offline,
            close_main_window,
            native_update_status,
            set_automatic_updates,
            request_native_update_check,
            request_native_update_install,
            autostart_status,
            set_autostart,
            open_external_url,
            stop_supplier,
            stop_supplier_channel,
            refresh_supplier_registration,
            supplier_status,
            check_tool_config,
            begin_tool_config_operation,
            execute_tool_config_operation,
            cancel_tool_config_operation,
            wait_tool_config_operation_terminal,
            wait_tool_config_operation_update,
            start_tool_program,
            locate_tool_program,
            choose_tool_program,
            save_tool_program,
            scan_codex_session_history
        ])
        .build(tauri::generate_context!())
        .context("build Tauri desktop host")?;
    let event_lifecycle = lifecycle.clone();
    let exit_code = app.run_return(move |app, event| match event {
        tauri::RunEvent::Ready => {
            let result = install_or_ensure_tray(app, event_lifecycle.clone());
            #[cfg(target_os = "macos")]
            let result = result.and_then(|_| schedule_macos_tray_recreation(app));
            if let Err(error) = result {
                log::error!("[const-api][tray] ready-state visibility check failed: {error}");
                // Desktop and tray launches are defined by the presence of a
                // tray icon. Do not leave a window-only process running after
                // a native tray initialization failure.
                event_lifecycle.quitting.store(true, Ordering::SeqCst);
                app.exit(1);
            } else {
                if update_marker_pending {
                    match consume_update_relaunch_mode(
                        &update_marker_state_dir,
                        env!("CARGO_PKG_VERSION"),
                    ) {
                        Ok(Some(mode)) => log::info!(
                            "[const-api][update] consumed relaunch marker after ready mode={}",
                            mode.as_str()
                        ),
                        Ok(None) => log::warn!(
                            "[const-api][update] relaunch marker disappeared before ready"
                        ),
                        Err(error) => log::warn!(
                            "[const-api][update] failed to consume relaunch marker after ready: {error:#}"
                        ),
                    }
                }
                log::info!("[const-api][tray] ready and visible");
            }
        }
        tauri::RunEvent::ExitRequested { api, .. }
            if !event_lifecycle.quitting.load(Ordering::SeqCst) =>
        {
            api.prevent_exit();
            event_lifecycle.request_quit(app.clone());
        }
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen {
            has_visible_windows,
            ..
        } => {
            if should_suppress_macos_update_reopen(
                restored_update_tray,
                has_visible_windows,
                app_started_at.elapsed(),
            ) {
                log::info!(
                    "[const-api][update] suppressed startup Reopen while restoring tray mode"
                );
            } else {
                show_main_window(app);
            }
        }
        _ => {}
    });
    startup_log(format!(
        "tauri event loop exited after {}ms with code {exit_code}",
        app_started_at.elapsed().as_millis()
    ));
    lifecycle.finish_after_event_loop().await;
    Ok(exit_code)
}

fn install_tray(
    app: &tauri::AppHandle,
    lifecycle: Arc<DesktopLifecycle>,
) -> std::result::Result<
    (
        tauri::menu::MenuItem<tauri::Wry>,
        tauri::menu::MenuItem<tauri::Wry>,
        tauri::menu::MenuItem<tauri::Wry>,
        tauri::menu::MenuItem<tauri::Wry>,
    ),
    Box<dyn std::error::Error>,
> {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
    use tauri::tray::TrayIconBuilder;
    #[cfg(not(target_os = "macos"))]
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};

    let open = MenuItem::with_id(
        app,
        "tray_open",
        if development_profile_active() {
            native_text("打开 CONST API Dev", "Open CONST API Dev")
        } else {
            native_text("打开 CONST API", "Open CONST API")
        },
        true,
        None::<&str>,
    )?;
    let proxy = MenuItem::with_id(
        app,
        "tray_proxy_status",
        native_text("本地 API：启动中", "Local API: starting"),
        false,
        None::<&str>,
    )?;
    let supplier = MenuItem::with_id(
        app,
        "tray_supplier_status",
        native_text("供应：启动中", "Supply: starting"),
        false,
        None::<&str>,
    )?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(
        app,
        "tray_quit",
        if development_profile_active() {
            native_text("退出 CONST API Dev", "Quit CONST API Dev")
        } else {
            native_text("退出 CONST API", "Quit CONST API")
        },
        true,
        None::<&str>,
    )?;
    let menu = Menu::with_items(app, &[&open, &proxy, &supplier, &separator, &quit])?;
    let menu_lifecycle = lifecycle.clone();
    let mut builder = TrayIconBuilder::with_id("const-api")
        .menu(&menu)
        .tooltip(application_display_name())
        .show_menu_on_left_click(cfg!(target_os = "macos"))
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "tray_open" => show_main_window(app),
            "tray_quit" => menu_lifecycle.request_quit(app.clone()),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            #[cfg(target_os = "macos")]
            let _ = (tray, event);
            #[cfg(not(target_os = "macos"))]
            {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                {
                    show_main_window(tray.app_handle());
                }
            }
        });
    #[cfg(target_os = "macos")]
    {
        // Preserve the routing mark as transparent negative space and let
        // AppKit tint the black template for the current menu-bar state.
        // `include_bytes!` deliberately makes the PNG a rustc input, so
        // changing it cannot reuse stale bytes during incremental builds.
        builder = builder.icon(macos_tray_icon()?).icon_as_template(true);
    }
    #[cfg(not(target_os = "macos"))]
    {
        if let Some(icon) = app.default_window_icon() {
            builder = builder.icon(icon.clone());
        }
    }
    let tray = builder.build(app)?;
    // Be explicit across platforms: a previously hidden status item must be
    // recreated before the app switches between Regular and Accessory modes.
    tray.set_visible(true)?;
    log::info!("[const-api][tray] installed and visible");
    Ok((open, proxy, supplier, quit))
}

fn install_or_ensure_tray(
    app: &tauri::AppHandle,
    lifecycle: Arc<DesktopLifecycle>,
) -> Result<(), String> {
    if app.tray_by_id("const-api").is_some() {
        return ensure_tray_visible(app);
    }
    let (open_item, proxy_item, supplier_item, quit_item) =
        install_tray(app, lifecycle.clone()).map_err(|error| error.to_string())?;
    lifecycle.set_tray_items(open_item, proxy_item, supplier_item, quit_item);
    Ok(())
}

fn spawn_show_request_loop(
    app: tauri::AppHandle,
    mut show_requests: tokio::sync::mpsc::Receiver<()>,
) {
    spawn_logged("instance show requests", async move {
        while show_requests.recv().await.is_some() {
            let _ = catch_runtime_panic("instance show request", async {
                show_main_window(&app);
            })
            .await;
        }
    });
}

pub(crate) fn show_main_window(app: &tauri::AppHandle) {
    #[cfg(target_os = "macos")]
    {
        if let Err(error) = app.set_activation_policy(tauri::ActivationPolicy::Regular) {
            log::error!("[const-api][tray] failed to enter macOS regular mode: {error}");
        }
        if let Err(error) = ensure_tray_visible(app) {
            log::error!("[const-api][tray] failed to keep macOS tray visible: {error}");
        }
    }
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

pub(crate) fn request_main_window_attention(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.request_user_attention(Some(tauri::UserAttentionType::Informational));
    }
}

fn ensure_tray_visible(app: &tauri::AppHandle) -> Result<(), String> {
    let tray = app
        .tray_by_id("const-api")
        .ok_or_else(|| "CONST API tray icon is not registered".to_string())?;
    tray.set_visible(true).map_err(|error| error.to_string())
}

#[cfg(target_os = "macos")]
fn macos_tray_icon() -> tauri::Result<tauri::image::Image<'static>> {
    tauri::image::Image::from_bytes(MACOS_TRAY_ICON_BYTES)
}

#[cfg(target_os = "macos")]
fn schedule_macos_tray_recreation(app: &tauri::AppHandle) -> Result<(), String> {
    ensure_tray_visible(app)?;
    let app = app.clone();
    spawn_logged("macOS tray recreation", async move {
        // RunEvent::Ready can arrive before AppKit has assigned the status
        // item a real menu-bar window. Recreate it after the first layout turn
        // instead of preserving a zero-height item indefinitely.
        tokio::time::sleep(Duration::from_millis(250)).await;
        if let Err(error) = recreate_macos_tray(&app) {
            log::error!("[const-api][tray] delayed macOS recreation failed: {error}");
        } else {
            log::info!("[const-api][tray] recreated after AppKit became ready");
        }
    });
    Ok(())
}

#[cfg(target_os = "macos")]
fn recreate_macos_tray(app: &tauri::AppHandle) -> Result<(), String> {
    let tray = app
        .tray_by_id("const-api")
        .ok_or_else(|| "CONST API tray icon is not registered".to_string())?;
    let icon = macos_tray_icon().map_err(|error| error.to_string())?;
    // Store the decoded image before detaching the status item, then let AppKit
    // create a fresh item with those pixels. This covers both an update
    // hand-off and `tauri dev` replacing an incrementally built binary.
    tray.set_icon_with_as_template(Some(icon), true)
        .map_err(|error| error.to_string())?;
    tray.set_visible(false).map_err(|error| error.to_string())?;
    tray.set_visible(true).map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn enter_macos_tray_mode(app: &tauri::AppHandle) -> Result<(), String> {
    app.set_activation_policy(tauri::ActivationPolicy::Accessory)
        .map_err(|error| error.to_string())?;
    ensure_tray_visible(app)
}

fn report_launch_error(mode: LaunchMode, message: &str) {
    log::error!("{message}");
    if mode == LaunchMode::Desktop {
        let _ = rfd::MessageDialog::new()
            .set_title(application_display_name())
            .set_description(message)
            .set_level(rfd::MessageLevel::Error)
            .show();
    }
}

#[tauri::command]
fn legal_notice_status(state: State<'_, AppState>) -> LegalNoticeStatus {
    let manifest = current_legal_manifest();
    LegalNoticeStatus {
        accepted: legal_notice_accepted(&state.config_path),
        previously_accepted: legal_notice_previously_accepted(&state.config_path),
        agreement_version: manifest.agreement.version,
        agreement_sha256: manifest.agreement.content_sha256.clone(),
        privacy_version: manifest.privacy.version,
        privacy_sha256: manifest.privacy.content_sha256.clone(),
    }
}

#[tauri::command]
async fn accept_legal_notice(
    state: State<'_, AppState>,
    lifecycle: State<'_, Arc<DesktopLifecycle>>,
    agreement_version: u32,
    agreement_sha256: String,
    privacy_version: u32,
    privacy_sha256: String,
) -> Result<(), String> {
    let manifest = current_legal_manifest();
    if agreement_version != manifest.agreement.version
        || agreement_sha256 != manifest.agreement.content_sha256
        || privacy_version != manifest.privacy.version
        || privacy_sha256 != manifest.privacy.content_sha256
    {
        return Err(format!(
            "legal document mismatch: renderer=agreement:{agreement_version}:{agreement_sha256}/privacy:{privacy_version}:{privacy_sha256}, runtime=agreement:{}:{}/privacy:{}:{}",
            manifest.agreement.version,
            manifest.agreement.content_sha256,
            manifest.privacy.version,
            manifest.privacy.content_sha256,
        ));
    }
    record_legal_notice_acceptance(&state.config_path).map_err(|error| error.to_string())?;
    lifecycle
        .legal_notice_accepted
        .store(true, Ordering::SeqCst);
    if let Err(error) = lifecycle.runtime.start().await {
        // Acceptance and runtime availability are independent. The normal UI
        // can surface a degraded runtime and let the user repair its settings.
        log::error!("[const-api][runtime] startup after legal acceptance degraded: {error}");
    }
    lifecycle.refresh_runtime_status().await;
    Ok(())
}

#[tauri::command]
fn decline_legal_notice(app: tauri::AppHandle, lifecycle: State<'_, Arc<DesktopLifecycle>>) {
    lifecycle.request_quit(app);
}

#[tauri::command]
async fn get_config(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let started_at = Instant::now();
    startup_log_early_command("[command] get_config begin");
    let result = reload_config_authoritatively(&state.config_path, &state.proxy_config)
        .map_err(|err| err.to_string())
        .and_then(|cfg| config_for_renderer(&cfg));
    match &result {
        Ok(_) => startup_log_early_command(format!(
            "[command] get_config complete in {}ms",
            started_at.elapsed().as_millis()
        )),
        Err(err) => startup_log_early_command(format!(
            "[command] get_config failed in {}ms: {err}",
            started_at.elapsed().as_millis()
        )),
    }
    result
}

#[tauri::command]
async fn save_config(
    state: State<'_, AppState>,
    config: AccessConfigUpdate,
) -> Result<serde_json::Value, String> {
    let started_at = Instant::now();
    startup_log("[command] save_config begin");
    let result = save_config_authoritatively(&state.config_path, &state.proxy_config, config)
        .map_err(|err| err.to_string())
        .and_then(|authoritative| config_for_renderer(&authoritative));
    match &result {
        Ok(_) => startup_log(format!(
            "[command] save_config complete in {}ms",
            started_at.elapsed().as_millis()
        )),
        Err(err) => startup_log(format!(
            "[command] save_config failed in {}ms: {err}",
            started_at.elapsed().as_millis()
        )),
    }
    result
}

#[tauri::command]
async fn save_supplier_config(
    state: State<'_, AppState>,
    channels: Vec<ChannelConfig>,
    base_channels: Vec<ChannelConfig>,
) -> Result<serde_json::Value, String> {
    let authoritative = save_supplier_config_authoritatively(
        &state.config_path,
        &state.proxy_config,
        channels,
        base_channels,
    )
    .map_err(|err| err.to_string())?;
    config_for_renderer(&authoritative)
}

#[tauri::command]
async fn set_development_endpoint(
    state: State<'_, AppState>,
    endpoint_id: String,
) -> Result<serde_json::Value, String> {
    if !cfg!(debug_assertions) {
        return Err("development endpoint selection is unavailable in release builds".to_string());
    }

    let endpoint_id = normalize_development_endpoint(&endpoint_id);
    let (authoritative, ()) =
        commit_config_update(&state.config_path, &state.proxy_config, move |config| {
            config.development_endpoint = endpoint_id;
            Ok(())
        })
        .map_err(|err| err.to_string())?;
    config_for_renderer(&authoritative)
}

fn save_config_authoritatively(
    config_path: &std::path::PathBuf,
    live_config: &Arc<StdMutex<ClientConfig>>,
    config: AccessConfigUpdate,
) -> Result<ClientConfig> {
    let (authoritative, ()) = commit_config_update(config_path, live_config, move |current| {
        apply_renderer_access_config(current, &config);
        Ok(())
    })?;
    Ok(authoritative)
}

fn save_supplier_config_authoritatively(
    config_path: &std::path::PathBuf,
    live_config: &Arc<StdMutex<ClientConfig>>,
    channels: Vec<ChannelConfig>,
    base_channels: Vec<ChannelConfig>,
) -> Result<ClientConfig> {
    let (authoritative, ()) = commit_config_update(config_path, live_config, move |current| {
        apply_renderer_supplier_config(current, &channels, &base_channels);
        Ok(())
    })?;
    Ok(authoritative)
}

fn apply_renderer_supplier_config(
    current: &mut ClientConfig,
    incoming: &[ChannelConfig],
    baseline: &[ChannelConfig],
) {
    let incoming_ids = incoming
        .iter()
        .map(|channel| channel.id.as_str())
        .collect::<std::collections::HashSet<_>>();
    let baseline_ids = baseline
        .iter()
        .map(|channel| channel.id.as_str())
        .collect::<std::collections::HashSet<_>>();
    let baseline_retained_order = baseline
        .iter()
        .filter(|channel| incoming_ids.contains(channel.id.as_str()))
        .map(|channel| channel.id.as_str())
        .collect::<Vec<_>>();
    let incoming_baseline_order = incoming
        .iter()
        .filter(|channel| baseline_ids.contains(channel.id.as_str()))
        .map(|channel| channel.id.as_str())
        .collect::<Vec<_>>();
    let reorder_requested = baseline_retained_order != incoming_baseline_order;
    let deleted_ids = baseline
        .iter()
        .filter(|channel| !incoming_ids.contains(channel.id.as_str()))
        .map(|channel| channel.id.as_str())
        .collect::<std::collections::HashSet<_>>();
    current
        .channels
        .retain(|channel| !deleted_ids.contains(channel.id.as_str()));

    for incoming_channel in incoming {
        let unchanged = baseline
            .iter()
            .find(|channel| channel.id == incoming_channel.id)
            .is_some_and(|baseline_channel| {
                channel_configs_match(incoming_channel, baseline_channel)
            });
        if unchanged {
            continue;
        }
        if let Some(current_channel) = current
            .channels
            .iter_mut()
            .find(|channel| channel.id == incoming_channel.id)
        {
            let mut merged = incoming_channel.clone();
            if let Some(base) = baseline.iter().find(|channel| channel.id == merged.id) {
                preserve_unedited_channel_observations(&mut merged, base, current_channel);
            }
            *current_channel = merged;
        } else {
            current.channels.push(incoming_channel.clone());
        }
    }

    if reorder_requested {
        // Reorder only channels known to this renderer snapshot. Channels
        // added concurrently retain their slots and their latest contents.
        let ordered_channels = incoming
            .iter()
            .filter_map(|incoming_channel| {
                current
                    .channels
                    .iter()
                    .find(|channel| channel.id == incoming_channel.id)
                    .cloned()
            })
            .collect::<Vec<_>>();
        let mut ordered_channels = ordered_channels.into_iter();
        for channel in &mut current.channels {
            if incoming_ids.contains(channel.id.as_str()) {
                if let Some(ordered_channel) = ordered_channels.next() {
                    *channel = ordered_channel;
                }
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct AccessConfigUpdate {
    listen: String,
    allow_lan_access: bool,
    proxy_auto_start: bool,
    api_key: String,
    allow_model_equivalence: bool,
    prefer_local_supply: bool,
}

fn apply_renderer_access_config(current: &mut ClientConfig, incoming: &AccessConfigUpdate) {
    current.listen = incoming.listen.clone();
    current.allow_lan_access = incoming.allow_lan_access;
    current.proxy_auto_start = incoming.proxy_auto_start;
    current.api_key = incoming.api_key.clone();
    current.allow_model_equivalence = incoming.allow_model_equivalence;
    current.prefer_local_supply = incoming.prefer_local_supply;
}

#[cfg(test)]
fn access_config_update(config: &ClientConfig) -> AccessConfigUpdate {
    AccessConfigUpdate {
        listen: config.listen.clone(),
        allow_lan_access: config.allow_lan_access,
        proxy_auto_start: config.proxy_auto_start,
        api_key: config.api_key.clone(),
        allow_model_equivalence: config.allow_model_equivalence,
        prefer_local_supply: config.prefer_local_supply,
    }
}

fn config_for_renderer(config: &ClientConfig) -> Result<serde_json::Value, String> {
    let mut value = serde_json::to_value(config).map_err(|err| err.to_string())?;
    if let Some(object) = value.as_object_mut() {
        object.remove("account_device_api_key");
        object.remove("model_compatibility_profiles");
    }
    Ok(value)
}

#[tauri::command]
async fn startup_log_from_ui(message: String, elapsed_ms: Option<f64>) -> Result<(), String> {
    match elapsed_ms {
        Some(elapsed_ms) => startup_log(format!(
            "[ui] {message}, ui_elapsed={}ms",
            elapsed_ms.round()
        )),
        None => startup_log(format!("[ui] {message}")),
    }
    Ok(())
}

#[tauri::command]
fn sync_native_theme(window: tauri::Window, dark: bool) -> Result<(), String> {
    let theme = if dark {
        tauri::Theme::Dark
    } else {
        tauri::Theme::Light
    };
    // Tauri applies this app-wide on macOS and Linux; Windows receives the
    // additional DWM colors below so its native caption matches exactly.
    window
        .set_theme(Some(theme))
        .map_err(|error| format!("set native window theme: {error}"))?;

    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::Graphics::Dwm::{
            DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR,
            DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute,
        };

        fn color_ref(red: u8, green: u8, blue: u8) -> u32 {
            u32::from(red) | (u32::from(green) << 8) | (u32::from(blue) << 16)
        }

        unsafe fn set_attribute<T>(
            hwnd: windows_sys::Win32::Foundation::HWND,
            attribute: i32,
            value: &T,
        ) -> i32 {
            unsafe {
                DwmSetWindowAttribute(
                    hwnd,
                    attribute as u32,
                    value as *const T as *const std::ffi::c_void,
                    std::mem::size_of::<T>() as u32,
                )
            }
        }

        let hwnd = window
            .hwnd()
            .map_err(|error| format!("read Windows window handle: {error}"))?
            .0 as windows_sys::Win32::Foundation::HWND;
        let dark_mode = i32::from(dark);
        let caption = if dark {
            color_ref(0, 0, 0)
        } else {
            color_ref(255, 254, 250)
        };
        let text = if dark {
            color_ref(241, 239, 232)
        } else {
            color_ref(21, 22, 26)
        };
        let border = if dark {
            color_ref(56, 59, 54)
        } else {
            color_ref(213, 212, 206)
        };

        unsafe {
            let dark_result = set_attribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark_mode);
            if dark_result < 0 {
                // Windows 10 1809 used attribute 19 before the public constant became 20.
                let _ = set_attribute(hwnd, 19, &dark_mode);
            }
            let _ = set_attribute(hwnd, DWMWA_CAPTION_COLOR, &caption);
            let _ = set_attribute(hwnd, DWMWA_TEXT_COLOR, &text);
            let _ = set_attribute(hwnd, DWMWA_BORDER_COLOR, &border);
        }
    }

    Ok(())
}

#[cfg(target_os = "macos")]
fn refresh_macos_close_button_tooltip(app: &tauri::AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "main window is unavailable".to_string())?;
    let tooltip = native_text(
        "关闭窗口，CONST API 将继续在后台运行",
        "Close window; CONST API will keep running in the background",
    )
    .to_string();

    app.run_on_main_thread(move || {
        if let Err(error) = set_macos_close_button_tooltip(&window, &tooltip) {
            log::warn!("[const-api][window] failed to update close-button tooltip: {error}");
        }
    })
    .map_err(|error| format!("schedule close-button tooltip update: {error}"))
}

#[cfg(target_os = "macos")]
fn set_macos_close_button_tooltip(
    window: &tauri::WebviewWindow,
    tooltip: &str,
) -> Result<(), String> {
    use objc2_app_kit::{NSView, NSWindow, NSWindowButton};
    use objc2_foundation::NSString;

    let ns_window = window.ns_window().map_err(|error| error.to_string())?;
    // Tauri owns this NSWindow. The pointer is borrowed only for this call,
    // which is always dispatched to AppKit's main thread above.
    let ns_window = unsafe { ns_window.cast::<NSWindow>().as_ref() }
        .ok_or_else(|| "native main window is unavailable".to_string())?;
    let close_button = ns_window
        .standardWindowButton(NSWindowButton::CloseButton)
        .ok_or_else(|| "native close button is unavailable".to_string())?;
    let tooltip = NSString::from_str(tooltip);
    NSView::setToolTip(&close_button, Some(&tooltip));
    Ok(())
}

#[tauri::command]
async fn sync_native_language(
    language: String,
    _app: tauri::AppHandle,
    lifecycle: State<'_, Arc<DesktopLifecycle>>,
) -> Result<(), String> {
    native_i18n::set_display_language(&language);
    #[cfg(target_os = "macos")]
    let menu_result = native_menu::install(&_app);
    #[cfg(target_os = "macos")]
    if let Err(error) = refresh_macos_close_button_tooltip(&_app) {
        log::warn!("[const-api][window] failed to refresh close-button tooltip: {error}");
    }
    lifecycle.update_tray_status(&lifecycle.runtime.summary().await);
    #[cfg(target_os = "macos")]
    menu_result.map_err(|error| format!("refresh native application menu: {error}"))?;
    Ok(())
}

fn proxy_routes(
    shared: Arc<ProxyShared>,
) -> impl Filter<Extract = (warp::reply::Response,), Error = warp::Rejection> + Clone {
    let websocket = warp::get()
        .and(warp::path::full())
        .and(optional_raw_query())
        .and(warp::header::headers_cloned())
        .and(warp::ws())
        .and(with_shared(shared.clone()))
        .and_then(proxy_websocket_request);
    let streaming_upload = warp::method()
        .and(warp::path::full())
        .and_then(match_streaming_upload_request)
        .and(optional_raw_query())
        .and(warp::header::headers_cloned())
        .and(warp::body::stream())
        .and(with_shared(shared.clone()))
        .and_then(proxy_streaming_upload_request);
    let gemini_upload_session = warp::method()
        .and(warp::path::full())
        .and_then(match_gemini_upload_session_request)
        .and(optional_raw_query())
        .and(warp::header::headers_cloned())
        .and(warp::body::stream())
        .and(with_shared(shared.clone()))
        .and_then(proxy_gemini_upload_session_request);
    let http = warp::any()
        .and(warp::method())
        .and(warp::path::full())
        .and(optional_raw_query())
        .and(warp::header::headers_cloned())
        .and(warp::body::bytes())
        .and(with_shared(shared))
        .and_then(proxy_request);
    websocket
        .or(gemini_upload_session)
        .unify()
        .or(streaming_upload)
        .unify()
        .or(http)
        .unify()
}

async fn try_bind_proxy_server(
    shared: Arc<ProxyShared>,
    addr: SocketAddr,
    shutdown_rx: oneshot::Receiver<()>,
) -> std::result::Result<(SocketAddr, impl std::future::Future<Output = ()> + 'static), String> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|err| format!("failed to listen on {addr}: {err}"))?;
    let bound_addr = listener
        .local_addr()
        .map_err(|err| format!("failed to read listener address for {addr}: {err}"))?;
    let server = warp::serve(proxy_routes(shared))
        .incoming(listener)
        .graceful(async move {
            let _ = shutdown_rx.await;
        })
        .run();
    Ok((bound_addr, server))
}

fn proxy_bind_address(cfg: &ClientConfig) -> Result<SocketAddr, String> {
    cfg.listen
        .parse()
        .map_err(|err| format!("invalid listen address: {err}"))
}

async fn start_proxy_inner(state: &AppState) -> Result<ProxyStatus, String> {
    let total_started_at = Instant::now();
    startup_log("[proxy] start_proxy begin");
    let step_started_at = Instant::now();
    let cfg = reload_config_authoritatively(&state.config_path, &state.proxy_config)
        .map_err(|err| err.to_string())?;
    let legal_state = state.account.lock().await.status().legal_state;
    let platform_key_allowed = matches!(legal_state.as_str(), "accepted" | "server_unsupported");
    if let Ok(mut key) = state.platform_api_key.lock() {
        *key = if platform_key_allowed {
            cfg.account_device_api_key.clone()
        } else {
            String::new()
        };
    }
    startup_log(format!(
        "[proxy] config loaded in {}ms, listen={}",
        step_started_at.elapsed().as_millis(),
        cfg.listen
    ));
    let step_started_at = Instant::now();
    let addr = proxy_bind_address(&cfg)?;
    startup_log(format!(
        "[proxy] listen address parsed in {}ms: {}",
        step_started_at.elapsed().as_millis(),
        addr
    ));

    let step_started_at = Instant::now();
    let platform_access_token = state.account.lock().await.access_token_handle();
    let mut runtime = state.proxy.lock().await;
    startup_log(format!(
        "[proxy] runtime lock acquired in {}ms",
        step_started_at.elapsed().as_millis()
    ));
    if runtime.running {
        startup_log(format!(
            "[proxy] already running, returning status in {}ms",
            total_started_at.elapsed().as_millis()
        ));
        return Ok(runtime.status());
    }

    let step_started_at = Instant::now();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    // Platform control traffic and model forwarding share one negotiated
    // transport and its connection pools.
    let shared = Arc::new(ProxyShared::with_platform_api_key_and_transport(
        cfg.clone(),
        long_http_client().clone(),
        state.platform_api_key.clone(),
        platform_access_token,
        state.account_refresh_notify.clone(),
        state.proxy_config.clone(),
        state.local_channel_readiness.clone(),
        state.local_model_quota_routes.clone(),
        state.platform_transport.clone(),
    ));
    shared.probe_platform_endpoints();
    startup_log(format!(
        "[proxy] reqwest client/shared state built in {}ms",
        step_started_at.elapsed().as_millis()
    ));
    let step_started_at = Instant::now();
    let routes = shared.clone();
    startup_log(format!(
        "[proxy] routes built in {}ms",
        step_started_at.elapsed().as_millis()
    ));

    let bind_started_at = Instant::now();
    startup_log(format!("[proxy] warp bind begin: {addr}"));
    let (bound_addr, server) = try_bind_proxy_server(routes, addr, shutdown_rx).await?;
    startup_log(format!(
        "[proxy] warp bound in {}ms: {}",
        bind_started_at.elapsed().as_millis(),
        bound_addr
    ));
    let server_task = spawn_logged("proxy server", async move {
        server.await;
        startup_log(format!(
            "[proxy] warp server stopped after {}ms: {}",
            bind_started_at.elapsed().as_millis(),
            addr
        ));
    });
    startup_log(format!(
        "[proxy] warp task spawned in {}ms",
        total_started_at.elapsed().as_millis()
    ));

    runtime.running = true;
    runtime.listen = cfg.listen.clone();
    runtime.active_endpoint = cfg
        .endpoints
        .iter()
        .find(|e| e.enabled)
        .map(|e| e.name.clone());
    runtime.shutdown = Some(shutdown_tx);
    runtime.server_task = Some(server_task);
    startup_log(format!(
        "[proxy] start_proxy complete in {}ms",
        total_started_at.elapsed().as_millis()
    ));
    Ok(runtime.status())
}

#[tauri::command]
async fn start_proxy(state: State<'_, AppState>) -> Result<ProxyStatus, String> {
    start_proxy_inner(state.inner()).await
}

async fn stop_proxy_inner(state: &AppState) -> Result<ProxyStatus, String> {
    let (status, server_task) = {
        let mut runtime = state.proxy.lock().await;
        if let Some(shutdown) = runtime.shutdown.take() {
            let _ = shutdown.send(());
        }
        runtime.running = false;
        runtime.active_endpoint = None;
        if let Ok(mut transport) = runtime.platform_transport_state.lock() {
            *transport = platform_transport::PlatformTransportSnapshot::default();
        }
        (runtime.status(), runtime.server_task.take())
    };
    if let Some(mut task) = server_task {
        if tokio::time::timeout(Duration::from_secs(2), &mut task)
            .await
            .is_err()
        {
            task.abort();
            let _ = task.await;
        }
    }
    Ok(status)
}

#[tauri::command]
async fn stop_proxy(state: State<'_, AppState>) -> Result<ProxyStatus, String> {
    stop_proxy_inner(state.inner()).await
}

async fn proxy_status_inner(state: &AppState) -> Result<ProxyStatus, String> {
    let started_at = Instant::now();
    startup_log_early_command("[command] proxy_status begin");
    let runtime = state.proxy.lock().await;
    let status = runtime.status();
    startup_log_early_command(format!(
        "[command] proxy_status complete in {}ms, running={}",
        started_at.elapsed().as_millis(),
        status.running
    ));
    Ok(status)
}

#[tauri::command]
async fn proxy_status(state: State<'_, AppState>) -> Result<ProxyStatus, String> {
    proxy_status_inner(state.inner()).await
}

#[tauri::command]
async fn refresh_platform_status(state: State<'_, AppState>) -> Result<ProxyStatus, String> {
    if let Err(error) = refresh_platform_health_if_due(state.inner(), Duration::from_secs(15)).await
    {
        log::debug!("[const-api][runtime] on-demand platform status refresh failed: {error:#}");
    }
    proxy_status_inner(state.inner()).await
}

#[tauri::command]
async fn fetch_me(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    fetch_local_json(&state, "/api/me").await
}

#[tauri::command]
async fn fetch_usage(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    fetch_local_json(&state, "/api/usage").await
}

#[tauri::command]
async fn fetch_calls(
    state: State<'_, AppState>,
    after_ms: Option<i64>,
    cursor: Option<String>,
    limit: Option<usize>,
) -> Result<serde_json::Value, String> {
    let limit = limit.unwrap_or(200).clamp(1, 2000);
    // Keep after_ms for old servers, which ignore view/cursor. Encode opaque
    // cursors as query values rather than interpolating caller-supplied text.
    let mut url =
        reqwest::Url::parse("http://localhost/api/calls").map_err(|err| err.to_string())?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("view", "sync");
        query.append_pair("limit", &limit.to_string());
        if let Some(after_ms) = after_ms.filter(|value| *value > 0) {
            query.append_pair("after_ms", &after_ms.to_string());
        }
        if let Some(cursor) = cursor.filter(|value| !value.is_empty()) {
            query.append_pair("cursor", &cursor);
        }
    }
    let path = format!("{}?{}", url.path(), url.query().unwrap_or_default());
    fetch_local_json(&state, &path).await
}

#[tauri::command]
async fn fetch_usage_stats(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    // Old servers ignore the optional view and keep returning dimensional rows.
    fetch_local_json(&state, "/api/usage/stats?view=summary").await
}

#[tauri::command]
async fn fetch_market_prices(
    state: State<'_, AppState>,
    view: Option<String>,
    query: Option<String>,
    channel_id: Option<String>,
    sort: Option<String>,
    order: Option<String>,
    page: Option<usize>,
    page_size: Option<usize>,
    if_revision: Option<String>,
) -> Result<serde_json::Value, String> {
    account_market_prices(
        state.inner(),
        view,
        query,
        channel_id,
        sort,
        order,
        page,
        page_size,
        if_revision,
    )
    .await
}

#[tauri::command]
async fn fetch_local_channel_calls(
    limit: Option<usize>,
    after_ms: Option<i64>,
) -> Result<serde_json::Value, String> {
    let limit = limit.unwrap_or(200).clamp(1, 2000);
    read_subscription_usage_records(limit, after_ms.filter(|value| *value > 0))
        .map(|records| {
            let cursor_ms = records
                .iter()
                .filter_map(subscription_usage_record_cursor_ms)
                .max()
                .unwrap_or_default();
            serde_json::json!({
                "data": records,
                "cursor_ms": cursor_ms,
                "incremental": after_ms.unwrap_or_default() > 0,
            })
        })
        .map_err(|err| err.to_string())
}

#[tauri::command]
async fn fetch_local_channel_stats() -> Result<serde_json::Value, String> {
    read_subscription_usage_stats()
        .map(|records| serde_json::json!({ "data": records }))
        .map_err(|err| err.to_string())
}

#[tauri::command]
async fn fetch_providers(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    fetch_local_json(&state, "/api/providers").await
}

#[tauri::command]
async fn fetch_supplier_nodes(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    fetch_local_json(&state, "/api/supplier/nodes?view=summary").await
}

#[tauri::command]
async fn fetch_supplier_route_plan(
    state: State<'_, AppState>,
    model: String,
    path: String,
    protocol: String,
    body: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    fetch_supplier_route_plan_inner(&state.config_path, model, path, protocol, body).await
}

#[tauri::command]
async fn detect_channel_upstream(channel: ChannelConfig) -> Result<ChannelDetectionResult, String> {
    detect_channel(channel).await.map_err(|err| err.to_string())
}

#[tauri::command]
async fn refresh_channel_upstream(
    channel: ChannelConfig,
) -> Result<ChannelDetectionResult, String> {
    refresh_channel(channel)
        .await
        .map_err(|err| err.to_string())
}

#[tauri::command]
async fn validate_channel_upstream(
    channel: ChannelConfig,
) -> Result<SupplierChannelValidationResult, String> {
    let enabled = channel.enabled;
    let share_enabled = channel.share_enabled;
    let (mut channel, health) =
        supplier::validate_supplier_channel(channel, SupplierCheckMode::FullInteractive)
            .await
            .map_err(|err| err.to_string())?;
    channel.enabled = enabled;
    channel.share_enabled = share_enabled;
    Ok(SupplierChannelValidationResult { channel, health })
}

#[tauri::command]
async fn fetch_channel_upstream_models(channel: ChannelConfig) -> Result<Vec<String>, String> {
    snapshot_channel(channel)
        .await
        .map(|result| result.models)
        .map_err(|err| err.to_string())
}

#[tauri::command]
async fn inspect_subscription_adapter(
    channel: ChannelConfig,
) -> Result<SubscriptionAdapterInspection, String> {
    Ok(subscription_adapter_inspection(&channel))
}

#[tauri::command]
async fn scan_subscription_credentials() -> Result<Vec<SubscriptionCredentialCandidate>, String> {
    Ok(scan_subscription_credential_candidates())
}

#[tauri::command]
async fn import_subscription_credential(
    provider: String,
    path: String,
) -> Result<ChannelConfig, String> {
    prepare_subscription_credential_channel(&provider, &path).map_err(|err| err.to_string())
}

#[tauri::command]
async fn create_subscription_oauth_session(
    state: State<'_, AppState>,
    provider: String,
) -> Result<SubscriptionOAuthSession, String> {
    let mut session =
        create_subscription_oauth_session_data(&provider).map_err(|err| err.to_string())?;
    session.auto_callback = state
        .subscription_oauth_callbacks
        .prepare(&session.state, &session.redirect_uri)
        .await;
    Ok(session)
}

#[tauri::command]
async fn wait_subscription_oauth_callback(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    oauth_state: String,
) -> Result<Option<String>, String> {
    let callback = state.subscription_oauth_callbacks.wait(&oauth_state).await;
    if callback.is_some() {
        log::info!(
            "[const-api][subscription-oauth] automatic callback received; activating main window"
        );
        show_main_window(&app);
    }
    Ok(callback)
}

#[tauri::command]
async fn cancel_subscription_oauth_callback(
    state: State<'_, AppState>,
    oauth_state: String,
) -> Result<(), String> {
    state
        .subscription_oauth_callbacks
        .cancel(&oauth_state)
        .await;
    Ok(())
}

#[tauri::command]
async fn exchange_subscription_oauth_code(
    provider: String,
    code_or_callback: String,
    state: String,
    code_verifier: String,
    redirect_uri: String,
) -> Result<ChannelConfig, String> {
    exchange_subscription_oauth_code_to_channel(
        &provider,
        &code_or_callback,
        &state,
        &code_verifier,
        &redirect_uri,
    )
    .await
    .map_err(|err| err.to_string())
}

#[tauri::command]
async fn import_subscription_token_json(
    provider: String,
    token_json: String,
) -> Result<ChannelConfig, String> {
    import_subscription_token_json_channel(&provider, &token_json).map_err(|err| err.to_string())
}

#[tauri::command]
async fn find_duplicate_channels(
    state: State<'_, AppState>,
    channel: ChannelConfig,
) -> Result<Vec<ChannelDuplicateCandidate>, String> {
    let config = load_config_from_path(&state.config_path).map_err(|err| err.to_string())?;
    Ok(duplicate_channel_candidates(&config.channels, &channel))
}

#[tauri::command]
async fn finalize_subscription_import(
    channel: ChannelConfig,
    create_duplicate: bool,
) -> Result<ChannelConfig, String> {
    finalize_subscription_import_channel(channel, create_duplicate).map_err(|err| err.to_string())
}

#[tauri::command]
async fn discard_subscription_import(channel: ChannelConfig) -> Result<bool, String> {
    discard_subscription_import_channel(&channel).map_err(|err| err.to_string())
}

#[tauri::command]
async fn test_local_proxy(
    state: State<'_, AppState>,
    prompt: String,
    model: Option<String>,
    protocol: Option<String>,
    skip_local: Option<bool>,
) -> Result<LocalProxyTestResult, String> {
    let cfg = load_config_from_path(&state.config_path).map_err(|err| err.to_string())?;
    let model = model
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Select a model.".to_string())?;
    let protocol = normalize_access_test_protocol(protocol.as_deref());
    let spec = local_proxy_test_spec(&protocol, &model, &prompt);
    let skip_local = skip_local.unwrap_or(true);
    let started_at = std::time::Instant::now();
    let url = format!("http://{}{}", cfg.listen, spec.path);
    let mut request = long_http_client()
        .post(&url)
        .header("Authorization", local_proxy_auth_header(&cfg))
        .header("Content-Type", "application/json")
        .json(&spec.body);
    if spec.accept_event_stream {
        request = request.header("Accept", "text/event-stream, application/json");
    }
    if skip_local {
        request = request.header(SKIP_LOCAL_SHORT_CIRCUIT_HEADER, "1");
    } else {
        request = request.header(USE_LOCAL_SHORT_CIRCUIT_HEADER, "1");
    }
    let resp = request
        .send()
        .await
        .map_err(|err| format!("request local proxy failed: {err}"))?;
    let status = resp.status().as_u16();
    let raw = resp.text().await.map_err(|err| err.to_string())?;
    let content = extract_any_content(&raw).unwrap_or_else(|| raw.clone());
    if !(200..300).contains(&status) {
        return Err(format!("local proxy returned HTTP {status}: {content}"));
    }
    Ok(LocalProxyTestResult {
        http_status: status,
        latency_ms: started_at.elapsed().as_millis(),
        model,
        protocol: spec.protocol,
        path: spec.path,
        route: "platform".to_string(),
        skip_local,
        content,
        raw,
    })
}

#[tauri::command]
async fn debug_protocol_exchange(
    state: State<'_, AppState>,
    target_type: String,
    target_id: Option<String>,
    inbound_protocol: String,
    target_protocol: String,
    model: String,
    prompt: String,
    stream: bool,
    skip_local_short_circuit: bool,
    experimental_features: Option<Vec<String>>,
    execute: bool,
) -> Result<ProtocolDebugResult, String> {
    debug_console::debug_protocol_exchange_inner(
        state,
        target_type,
        target_id,
        inbound_protocol,
        target_protocol,
        model,
        prompt,
        stream,
        skip_local_short_circuit,
        experimental_features.unwrap_or_default(),
        execute,
    )
    .await
}

#[tauri::command]
async fn test_channel_upstream(
    state: State<'_, AppState>,
    channel: ChannelConfig,
    model: String,
    prompt: String,
) -> Result<ChannelUpstreamTestResult, String> {
    let channel_id = channel.id.clone();
    let failure_generation = supplier::local_model_failure_generation();
    let result = supplier::test_channel_upstream_model_inner(channel, model, prompt).await?;
    supplier::record_supplier_channel_upstream_observation(
        state.inner(),
        &channel_id,
        &result,
        failure_generation,
    )
    .await;
    Ok(result)
}

#[tauri::command]
async fn activate_supplier_channel(
    state: State<'_, AppState>,
    channel_id: String,
) -> Result<SupplierStatus, String> {
    supplier::activate_supplier_channel_inner(state.inner(), channel_id).await
}

#[tauri::command]
async fn start_supplier(
    state: State<'_, AppState>,
    channel_id: Option<String>,
    channel_ids: Option<Vec<String>>,
) -> Result<SupplierStatus, String> {
    let started_at = Instant::now();
    startup_log(format!(
        "[command] start_supplier begin, channel_id={:?}, channel_count={}",
        channel_id,
        channel_ids.as_ref().map(|ids| ids.len()).unwrap_or(0)
    ));
    let result = supplier::start_supplier_inner(state.inner(), channel_id, channel_ids).await;
    match &result {
        Ok(status) => startup_log(format!(
            "[command] start_supplier complete in {}ms, running={}, starting={}, active_transport={}",
            started_at.elapsed().as_millis(),
            status.running,
            status.starting,
            status.active_transport
        )),
        Err(err) => startup_log(format!(
            "[command] start_supplier failed in {}ms: {err}",
            started_at.elapsed().as_millis()
        )),
    }
    result
}

#[tauri::command]
async fn recover_supplier_channel(
    state: State<'_, AppState>,
    channel_id: String,
    full_check: Option<bool>,
) -> Result<SupplierStatus, String> {
    supplier::recover_supplier_channel_inner(state.inner(), channel_id, full_check.unwrap_or(true))
        .await
}

#[tauri::command]
async fn stop_supplier(state: State<'_, AppState>) -> Result<SupplierStatus, String> {
    supplier::stop_supplier_inner(state.inner()).await
}

#[tauri::command]
async fn notify_supplier_offline(state: State<'_, AppState>) -> Result<SupplierStatus, String> {
    supplier::notify_supplier_offline_inner(state.inner()).await
}

#[tauri::command]
async fn close_main_window(app: tauri::AppHandle) -> Result<(), String> {
    let Some(window) = app.get_webview_window("main") else {
        return Ok(());
    };
    window.hide().map_err(|err| err.to_string())?;
    #[cfg(target_os = "macos")]
    enter_macos_tray_mode(&app)?;
    Ok(())
}

#[tauri::command]
async fn reset_local_app_data(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    lifecycle: State<'_, Arc<DesktopLifecycle>>,
) -> Result<(), String> {
    let data_root = state
        .config_path
        .parent()
        .ok_or_else(|| "local app data directory is unavailable".to_string())?;
    if data_root != client_data_root() {
        return Err("refusing to clear an unexpected local data directory".to_string());
    }

    // Shut the runtime down while the current account session and channel identities
    // are still available. This unregisters every supplied channel before logout and
    // prevents background recovery from bringing one back during the reset.
    lifecycle.runtime.shutdown().await;
    account_logout_inner(state.inner()).await?;

    if let Some(window) = app.get_webview_window("main") {
        window
            .clear_all_browsing_data()
            .map_err(|error| error.to_string())?;
    }

    // Windows keeps SQLite and other process-owned files locked until this
    // process exits. The replacement process deletes them before opening any
    // local state of its own.
    if lifecycle.quitting.swap(true, Ordering::SeqCst) {
        return Err("application shutdown is already in progress".to_string());
    }
    let marker_path = local_data_reset_marker_path(data_root).map_err(|error| {
        lifecycle.quitting.store(false, Ordering::SeqCst);
        error.to_string()
    })?;
    let marker = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );
    if let Err(error) = std::fs::write(&marker_path, marker) {
        lifecycle.quitting.store(false, Ordering::SeqCst);
        return Err(format!(
            "write local data reset marker {}: {error}",
            marker_path.display()
        ));
    }
    if development_profile_active() && std::env::var_os(DEV_RESET_RELAUNCH_MANAGED_ENV).is_some() {
        app.exit(0);
        return Ok(());
    }
    app.request_restart();
    Ok(())
}

pub(crate) fn stage_update_relaunch_for_install_native(
    app: &tauri::AppHandle,
    target_version: &str,
) -> Result<LaunchMode, String> {
    if development_profile_active() {
        return Err("updates are disabled for the development profile".to_string());
    }
    let window_visible = match app.get_webview_window("main") {
        Some(window) => window.is_visible().unwrap_or_else(|error| {
            log::warn!(
                "[const-api][update] failed to inspect main-window visibility; preserving tray-only state: {error}"
            );
            false
        }),
        None => false,
    };
    let mode = update_relaunch_mode_for_window(window_visible);
    let state_dir = state_dir_for_config(&default_config_path());
    update_relaunch::stage_update_relaunch(&state_dir, mode, target_version)
        .map_err(|error| error.to_string())?;
    log::info!(
        "[const-api][update] staged relaunch mode={} target_version={target_version}",
        mode.as_str()
    );
    Ok(mode)
}

pub(crate) fn clear_staged_update_relaunch_native() -> Result<(), String> {
    let state_dir = state_dir_for_config(&default_config_path());
    clear_update_relaunch(&state_dir).map_err(|error| error.to_string())
}

#[tauri::command]
fn autostart_status(app: tauri::AppHandle) -> Result<bool, String> {
    if development_profile_active() {
        return Ok(false);
    }
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().map_err(|err| err.to_string())
}

fn apply_new_install_autostart_default(
    app: &tauri::AppHandle,
    config_path: &std::path::Path,
) -> Result<bool> {
    if development_profile_active() {
        return Ok(false);
    }
    use tauri_plugin_autostart::ManagerExt;

    let state_dir = state_dir_for_config(config_path);
    if !autostart::default_enablement_pending(&state_dir)? {
        return Ok(false);
    }

    let manager = app.autolaunch();
    if !manager
        .is_enabled()
        .context("read login startup before applying default")?
    {
        manager
            .enable()
            .context("enable login startup for new installation")?;
    }
    if !manager
        .is_enabled()
        .context("verify login startup after applying default")?
    {
        return Err(anyhow!(
            "login startup remained disabled after the native enable call"
        ));
    }
    autostart::mark_initialized(&state_dir)
        .context("persist completed login startup initialization")?;
    Ok(true)
}

#[tauri::command]
fn set_autostart(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<bool, String> {
    if development_profile_active() {
        return Err("login startup is disabled for the development profile".to_string());
    }
    use tauri_plugin_autostart::ManagerExt;

    // Once the user touches the switch, their choice wins permanently over a
    // not-yet-applied new-install default.
    autostart::mark_initialized(&state_dir_for_config(&state.config_path))
        .map_err(|err| err.to_string())?;
    let manager = app.autolaunch();
    if enabled {
        manager.enable().map_err(|err| err.to_string())?;
        #[cfg(target_os = "macos")]
        autostart::associate_macos_launch_agent(&app).map_err(|err| err.to_string())?;
    } else {
        manager.disable().map_err(|err| err.to_string())?;
    }
    let actual = manager.is_enabled().map_err(|err| err.to_string())?;
    if actual != enabled {
        return Err(format!(
            "login startup state did not change: requested={enabled}, actual={actual}"
        ));
    }
    Ok(actual)
}

#[tauri::command]
async fn open_external_url(url: String) -> Result<(), String> {
    let (program, args) = external_url_open_command(&url)?;
    std::process::Command::new(program)
        .args(args)
        .spawn()
        .map_err(|err| format!("open url failed: {err}"))?;
    Ok(())
}

fn external_url_open_command(raw: &str) -> Result<(&'static str, Vec<String>), String> {
    let parsed = reqwest::Url::parse(raw.trim()).map_err(|err| format!("invalid url: {err}"))?;
    if !matches!(parsed.scheme(), "http" | "https") || !parsed.has_host() {
        return Err("only http/https URLs can be opened".to_string());
    }
    let url = parsed.to_string();
    #[cfg(target_os = "macos")]
    {
        Ok(("open", vec![url]))
    }
    #[cfg(target_os = "windows")]
    {
        Ok((
            "rundll32",
            vec!["url.dll,FileProtocolHandler".to_string(), url],
        ))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Ok(("xdg-open", vec![url]))
    }
}

#[tauri::command]
async fn stop_supplier_channel(
    state: State<'_, AppState>,
    channel_id: String,
) -> Result<SupplierStatus, String> {
    supplier::stop_supplier_channel_inner(state.inner(), channel_id).await
}

#[tauri::command]
async fn refresh_supplier_registration(
    state: State<'_, AppState>,
    channel_id: Option<String>,
) -> Result<SupplierStatus, String> {
    supplier::refresh_supplier_registration_inner(state.inner(), channel_id.as_deref()).await
}

#[tauri::command]
async fn supplier_status(state: State<'_, AppState>) -> Result<SupplierStatus, String> {
    supplier::supplier_status_inner(state.inner()).await
}

#[derive(Serialize)]
struct ToolConfigOperationResponse {
    status: String,
    confirmation_required: bool,
    manual_close_required: bool,
    running: bool,
    confirmation_token: Option<String>,
    message: Option<String>,
    result: Option<ToolApplyResult>,
}

fn tool_config_operation_response(outcome: ToolConfigWriteOutcome) -> ToolConfigOperationResponse {
    match outcome {
        ToolConfigWriteOutcome::ConfirmationRequired {
            running,
            confirmation_token,
        } => ToolConfigOperationResponse {
            status: "confirmation_required".to_string(),
            confirmation_required: true,
            manual_close_required: false,
            running,
            confirmation_token: Some(confirmation_token),
            message: None,
            result: None,
        },
        ToolConfigWriteOutcome::ManualCloseRequired {
            running,
            confirmation_token,
            reason,
        } => ToolConfigOperationResponse {
            status: "manual_close_required".to_string(),
            confirmation_required: false,
            manual_close_required: true,
            running,
            confirmation_token: Some(confirmation_token),
            message: Some(reason),
            result: None,
        },
        ToolConfigWriteOutcome::Completed(result) => {
            let status = result
                .details
                .get("operation_status")
                .cloned()
                .unwrap_or_else(|| "completed".to_string());
            ToolConfigOperationResponse {
                status,
                confirmation_required: false,
                manual_close_required: false,
                running: false,
                confirmation_token: None,
                message: None,
                result: Some(result),
            }
        }
    }
}

#[tauri::command]
async fn begin_tool_config_operation(
    tool: String,
    action: String,
    restart: bool,
    confirmation_token: Option<String>,
) -> Result<ToolConfigOperationTicket, String> {
    let action = ToolConfigAction::parse(&action).map_err(|err| err.to_string())?;
    if confirmation_token.is_some() {
        create_confirmed_tool_config_operation(&tool, action, restart)
    } else {
        create_tool_config_operation(&tool, action, restart)
    }
    .map_err(|err| err.to_string())
}

#[tauri::command]
async fn cancel_tool_config_operation(operation_id: String) -> Result<ToolConfigCancelAck, String> {
    request_cancel_tool_config_operation(&operation_id).map_err(|err| err.to_string())
}

#[tauri::command]
async fn wait_tool_config_operation_terminal(
    operation_id: String,
    wait_ms: u64,
) -> Result<ToolConfigOperationStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        tool_config::wait_for_tool_config_operation_terminal(
            &operation_id,
            std::time::Duration::from_millis(wait_ms),
        )
        .map_err(|err| err.to_string())
    })
    .await
    .map_err(|err| format!("TOOL_CONFIG_TERMINAL_WAIT_JOIN_FAILED: {err}"))?
}

#[tauri::command]
async fn wait_tool_config_operation_update(
    operation_id: String,
    after_version: u64,
    wait_ms: u64,
) -> Result<ToolConfigOperationStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        tool_config::wait_for_tool_config_operation_update(
            &operation_id,
            after_version,
            std::time::Duration::from_millis(wait_ms),
        )
        .map_err(|err| err.to_string())
    })
    .await
    .map_err(|err| format!("TOOL_CONFIG_PROGRESS_WAIT_JOIN_FAILED: {err}"))?
}

#[tauri::command]
async fn execute_tool_config_operation(
    state: State<'_, AppState>,
    operation_id: String,
    tool: String,
    action: String,
    protocol: Option<String>,
    claude_model_settings: Option<ClaudeModelSettings>,
    codex_model_source: Option<CodexModelSource>,
    remove_mode: Option<String>,
    confirmation_token: Option<String>,
    restart: bool,
) -> Result<ToolConfigOperationResponse, String> {
    let action = ToolConfigAction::parse(&action).map_err(|err| err.to_string())?;
    let remove_mode = match action {
        ToolConfigAction::Remove => ToolConfigRemoveMode::parse(remove_mode.as_deref())
            .and_then(|mode| mode.validate_for_tool(&tool))
            .map_err(|err| err.to_string())?,
        ToolConfigAction::Apply if remove_mode.is_some() => {
            return Err("TOOL_CONFIG_REMOVE_MODE_WITH_APPLY: removeMode is valid only for remove operations".to_string());
        }
        ToolConfigAction::Apply => ToolConfigRemoveMode::RestorePreConst,
    };
    let operation = start_tool_config_operation(&operation_id, &tool, action, restart)
        .map_err(|err| err.to_string())?;
    let lease_operation = Arc::clone(&operation);
    let lease = match tauri::async_runtime::spawn_blocking(move || {
        acquire_tool_config_operation_lease(&lease_operation).map_err(|err| err.to_string())
    })
    .await
    {
        Ok(Ok(lease)) => lease,
        Ok(Err(err)) => {
            finish_tool_config_operation(&operation, false);
            return Err(err);
        }
        Err(err) => {
            finish_tool_config_operation(&operation, false);
            return Err(format!("TOOL_CONFIG_LOCK_JOIN_FAILED: {err}"));
        }
    };

    let result = match catch_runtime_panic(format!("tool config operation {operation_id}"), async {
        if matches!(tool.as_str(), "trae" | "trae-cn" | "trae-work") {
            execute_trae_tool_config_operation(
                &state,
                Arc::clone(&operation),
                &tool,
                action,
                protocol,
                confirmation_token,
            )
            .await
        } else if tool == "codex" {
            execute_codex_tool_config_operation_inner(
                &state,
                Arc::clone(&operation),
                action,
                remove_mode,
                protocol,
                codex_model_source,
                confirmation_token,
            )
            .await
        } else {
            execute_tool_config_operation_inner(
                &state,
                &operation,
                &tool,
                action,
                remove_mode,
                protocol,
                claude_model_settings,
                confirmation_token,
            )
            .await
        }
    })
    .await
    {
        Ok(result) => result,
        Err(recovered) => Err(format!("TOOL_CONFIG_RUNTIME_PANIC: {}", recovered.message)),
    };
    finish_tool_config_operation(&operation, result.is_ok());
    drop(lease);
    result
}

async fn execute_trae_tool_config_operation(
    state: &State<'_, AppState>,
    operation: Arc<ToolConfigOperationControl>,
    tool: &str,
    action: ToolConfigAction,
    protocol: Option<String>,
    confirmation_token: Option<String>,
) -> Result<ToolConfigOperationResponse, String> {
    let cfg = load_config_from_path(&state.config_path).map_err(|err| err.to_string())?;
    let urls = tool_config_urls(&cfg)?;
    let removing = action == ToolConfigAction::Remove;
    let api_key = if removing {
        cfg.api_key.trim().to_string()
    } else {
        require_config_api_key(&cfg)?
    };
    let protocol =
        resolve_tool_protocol(tool, protocol.as_deref()).map_err(|err| err.to_string())?;
    let models = if removing {
        Vec::new()
    } else {
        require_tool_model_snapshot_for_operation(&cfg, &operation, tool, tool).await?
    };
    let prepared = prepare_trae_config(tool, &urls.openai, &api_key, &models, protocol, removing)
        .await
        .map_err(|err| err.to_string())?;
    ensure_tool_config_operation_active(&operation, "Trae model configuration prepared")
        .map_err(|err| err.to_string())?;
    let context_digest = tool_config_operation_context_digest(
        &state.config_path,
        &api_key,
        &urls,
        Some(protocol),
        &prepared.context_digest().map_err(|err| err.to_string())?,
    );
    let runtime = tokio::runtime::Handle::current();
    let tool = tool.to_string();
    tauri::async_runtime::spawn_blocking(move || {
        let preview = prepared.preview();
        execute_atomic_tool_config_operation_with_preview(
            &operation,
            Some(protocol.as_str()),
            &context_digest,
            confirmation_token.as_deref(),
            || Ok(preview),
            || {
                runtime.block_on(prepared.commit(|completed, total| {
                    update_tool_config_operation_progress(
                        &operation,
                        "writing_config",
                        Some(completed as u64),
                        Some(total as u64),
                    );
                }))
            },
            |context| launch_tool_program_for_atomic_operation(&tool, context),
        )
        .map(tool_config_operation_response)
        .map_err(|err| err.to_string())
    })
    .await
    .map_err(|err| format!("TOOL_CONFIG_TRAE_JOIN_FAILED: {err}"))?
}

async fn execute_codex_tool_config_operation_inner(
    state: &State<'_, AppState>,
    operation: Arc<ToolConfigOperationControl>,
    action: ToolConfigAction,
    remove_mode: ToolConfigRemoveMode,
    protocol: Option<String>,
    model_source: Option<CodexModelSource>,
    confirmation_token: Option<String>,
) -> Result<ToolConfigOperationResponse, String> {
    ensure_tool_config_operation_active(&operation, "configuration load")
        .map_err(|err| err.to_string())?;
    let cfg = load_config_from_path(&state.config_path).map_err(|err| err.to_string())?;
    let urls = tool_config_urls(&cfg)?;
    let api_key = if action == ToolConfigAction::Remove {
        cfg.api_key.trim().to_string()
    } else {
        require_config_api_key(&cfg)?
    };
    ensure_tool_config_operation_active(&operation, "configuration preparation")
        .map_err(|err| err.to_string())?;
    let protocol = if action == ToolConfigAction::Remove {
        None
    } else {
        Some(resolve_tool_protocol("codex", protocol.as_deref()).map_err(|err| err.to_string())?)
    };
    let model_source = if action == ToolConfigAction::Remove {
        CodexModelSource::Codex
    } else {
        model_source.unwrap_or(current_codex_model_source().map_err(|err| err.to_string())?)
    };
    let models = if action == ToolConfigAction::Apply && model_source == CodexModelSource::Const {
        let models = fetch_local_platform_models_for_operation(&cfg, &operation, "codex").await?;
        if !models.iter().any(codex_catalog_model_eligible) {
            return Err(model_refresh_unavailable_error(
                &cfg, &operation, "codex", "Codex",
            ));
        }
        models
    } else {
        Vec::new()
    };
    let model_context = format!("{model_source:?}:{models:?}");
    let context_digest = tool_config_operation_context_digest(
        &state.config_path,
        &api_key,
        &urls,
        protocol,
        if action == ToolConfigAction::Remove {
            remove_mode.as_str()
        } else {
            &model_context
        },
    );

    // Session migration may scan and copy multi-gigabyte JSONL files. Keep the
    // complete atomic Codex operation off the async runtime that serves proxy
    // requests and supplier traffic.
    tauri::async_runtime::spawn_blocking(move || {
        let catalog =
            if action == ToolConfigAction::Apply && model_source == CodexModelSource::Const {
                Some(
                    build_codex_model_catalog(&models, &read_codex_cached_models())
                        .map_err(|err| err.to_string())?,
                )
            } else {
                None
            };
        execute_atomic_tool_config_operation(
            &operation,
            protocol.map(ToolProtocol::as_str),
            &context_digest,
            confirmation_token.as_deref(),
            || {
                let mut report = |stage, completed, total, ratio| {
                    update_tool_config_operation_progress_with_ratio(
                        &operation, stage, completed, total, ratio,
                    );
                };
                if action == ToolConfigAction::Remove {
                    remove_codex_config_with_progress_and_mode(
                        &urls.openai,
                        &api_key,
                        remove_mode,
                        &mut report,
                    )
                } else {
                    let mut result = apply_codex_config_with_catalog_and_progress(
                        &urls.openai,
                        &api_key,
                        Some(model_source),
                        catalog.as_ref(),
                        &mut report,
                    )?;
                    if let Some(protocol) = protocol {
                        attach_tool_protocol(&mut result, protocol);
                    }
                    Ok(result)
                }
            },
            |launch_context| launch_tool_program_for_atomic_operation("codex", launch_context),
        )
        .map(tool_config_operation_response)
        .map_err(|err| err.to_string())
    })
    .await
    .map_err(|err| format!("TOOL_CONFIG_CODEX_JOIN_FAILED: {err}"))?
}

async fn execute_tool_config_operation_inner(
    state: &State<'_, AppState>,
    operation: &ToolConfigOperationControl,
    tool: &str,
    action: ToolConfigAction,
    remove_mode: ToolConfigRemoveMode,
    protocol: Option<String>,
    claude_model_settings: Option<ClaudeModelSettings>,
    confirmation_token: Option<String>,
) -> Result<ToolConfigOperationResponse, String> {
    ensure_tool_config_operation_active(operation, "configuration load")
        .map_err(|err| err.to_string())?;
    let cfg = load_config_from_path(&state.config_path).map_err(|err| err.to_string())?;
    let urls = tool_config_urls(&cfg)?;
    let api_key = if action == ToolConfigAction::Remove {
        cfg.api_key.trim().to_string()
    } else {
        require_config_api_key(&cfg)?
    };
    ensure_tool_config_operation_active(operation, "configuration preparation")
        .map_err(|err| err.to_string())?;

    if action == ToolConfigAction::Remove {
        let root_url = tool_config_root_for_tool(&urls, tool);
        let context_digest = tool_config_operation_context_digest(
            &state.config_path,
            &api_key,
            &urls,
            None,
            remove_mode.as_str(),
        );
        let outcome = execute_atomic_tool_config_operation(
            operation,
            None,
            &context_digest,
            confirmation_token.as_deref(),
            || {
                remove_tool_config_by_name_with_mode(
                    tool,
                    &urls.openai,
                    root_url,
                    &api_key,
                    remove_mode,
                )
            },
            |launch_context| launch_tool_program_for_atomic_operation(tool, launch_context),
        )
        .map_err(|err| err.to_string())?;
        return Ok(tool_config_operation_response(outcome));
    }

    let protocol =
        resolve_tool_protocol(tool, protocol.as_deref()).map_err(|err| err.to_string())?;
    let outcome = if tool == "claude" {
        let settings = claude_model_settings.unwrap_or_default();
        validate_claude_model_settings(&settings).map_err(|err| err.to_string())?;
        let models = fetch_local_platform_models_for_operation(&cfg, operation, tool).await?;
        let settings = claude_settings_with_context(&cfg, &settings, &models);
        let context_digest = tool_config_operation_context_digest(
            &state.config_path,
            &api_key,
            &urls,
            Some(protocol),
            &format!("{settings:?}:{models:?}"),
        );
        execute_previewed_tool_config_operation(
            operation,
            Some(protocol.as_str()),
            &context_digest,
            confirmation_token.as_deref(),
            || {
                let mut result = apply_claude_config_with_model_info(
                    &urls.anthropic,
                    &api_key,
                    &settings,
                    &models,
                    &cfg,
                )?;
                attach_tool_protocol(&mut result, protocol);
                Ok(result)
            },
            |launch_context| {
                launch_claude_code_program_for_atomic_operation(
                    &urls.root,
                    &api_key,
                    launch_context,
                )
            },
        )
    } else if tool == "opencode" {
        let models =
            require_tool_model_snapshot_for_operation(&cfg, operation, tool, "OpenCode").await?;
        let context_digest = tool_config_operation_context_digest(
            &state.config_path,
            &api_key,
            &urls,
            Some(protocol),
            &format!("{models:?}"),
        );
        execute_previewed_tool_config_operation(
            operation,
            Some(protocol.as_str()),
            &context_digest,
            confirmation_token.as_deref(),
            || {
                apply_opencode_config_with_model_info_for_protocol(
                    &urls.openai,
                    &api_key,
                    &models,
                    protocol,
                )
            },
            |launch_context| launch_tool_program_for_atomic_operation(tool, launch_context),
        )
    } else if tool == "openclaw" {
        let models = openclaw_tool_model_snapshot_for_operation(&cfg, operation).await?;
        let root_url = tool_config_root_for_tool(&urls, tool);
        let context_digest = tool_config_operation_context_digest(
            &state.config_path,
            &api_key,
            &urls,
            Some(protocol),
            &format!("{models:?}"),
        );
        execute_previewed_tool_config_operation(
            operation,
            Some(protocol.as_str()),
            &context_digest,
            confirmation_token.as_deref(),
            || {
                apply_openclaw_config_with_model_info_for_protocol(
                    root_url, &api_key, &models, protocol,
                )
            },
            |launch_context| launch_tool_program_for_atomic_operation(tool, launch_context),
        )
    } else if tool == "vscode" {
        let models =
            require_tool_model_snapshot_for_operation(&cfg, operation, tool, "VS Code").await?;
        let context_digest = tool_config_operation_context_digest(
            &state.config_path,
            &api_key,
            &urls,
            Some(protocol),
            &format!("{models:?}"),
        );
        execute_previewed_tool_config_operation(
            operation,
            Some(protocol.as_str()),
            &context_digest,
            confirmation_token.as_deref(),
            || {
                apply_vscode_config_with_model_info_for_protocol(
                    &urls.openai,
                    &api_key,
                    &models,
                    protocol,
                )
            },
            |launch_context| launch_tool_program_for_atomic_operation(tool, launch_context),
        )
    } else if tool == "workbuddy" {
        let models =
            require_tool_model_snapshot_for_operation(&cfg, operation, tool, "WorkBuddy").await?;
        let context_digest = tool_config_operation_context_digest(
            &state.config_path,
            &api_key,
            &urls,
            Some(protocol),
            &format!("{models:?}"),
        );
        execute_previewed_tool_config_operation(
            operation,
            Some(protocol.as_str()),
            &context_digest,
            confirmation_token.as_deref(),
            || {
                apply_workbuddy_config_with_model_info_for_protocol(
                    &urls.openai,
                    &api_key,
                    &models,
                    protocol,
                )
            },
            |launch_context| launch_tool_program_for_atomic_operation(tool, launch_context),
        )
    } else if matches!(
        tool,
        "copilot"
            | "copilot-desktop"
            | "raven"
            | "pi"
            | "cline"
            | "reasonix"
            | "deepseek-harness"
            | "open-interpreter"
            | "anythingllm"
            | "goose"
            | "mistral-vibe"
            | "grok-build"
            | "minimax-code"
            | "open-design"
            | "kimicode"
            | "mimocode"
            | "qwencode"
            | "openscience"
            | "vibe-trading"
            | "zcode"
    ) {
        let tool_label = match tool {
            "copilot" => "GitHub Copilot CLI",
            "copilot-desktop" => "GitHub Copilot",
            "raven" => "Raven",
            "pi" => "Pi",
            "cline" => "Cline",
            "reasonix" => "DeepSeek Reasonix",
            "deepseek-harness" => "DeepSeek Harness",
            "open-interpreter" => "Open Interpreter",
            "anythingllm" => "AnythingLLM",
            "goose" => "Goose",
            "mistral-vibe" => "Mistral Vibe",
            "grok-build" => "Grok Build",
            "minimax-code" => "MiniMax Code",
            "open-design" => "Open Design",
            "kimicode" => "Kimi Code",
            "mimocode" => "MiMo Code",
            "qwencode" => "Qwen Code",
            "openscience" => "OpenScience",
            "vibe-trading" => "Vibe-Trading",
            "zcode" => "ZCode",
            _ => tool,
        };
        let models =
            additional_tool_model_snapshot_for_operation(&cfg, operation, tool, tool_label).await?;
        let context_digest = tool_config_operation_context_digest(
            &state.config_path,
            &api_key,
            &urls,
            Some(protocol),
            &format!("{models:?}"),
        );
        execute_previewed_tool_config_operation(
            operation,
            Some(protocol.as_str()),
            &context_digest,
            confirmation_token.as_deref(),
            || {
                apply_additional_tool_config_with_model_info_for_protocol(
                    tool,
                    &urls.openai,
                    &api_key,
                    &models,
                    protocol,
                )
            },
            |launch_context| {
                if tool == "copilot" {
                    launch_copilot_cli_program_for_atomic_operation(
                        &urls.openai,
                        &api_key,
                        launch_context,
                    )
                } else if tool == "goose" {
                    launch_goose_program_for_atomic_operation(&api_key, launch_context)
                } else {
                    launch_tool_program_for_atomic_operation(tool, launch_context)
                }
            },
        )
    } else if tool == "claude-desktop" {
        let models =
            require_tool_model_snapshot_for_operation(&cfg, operation, tool, "Claude Desktop")
                .await?;
        let context_digest = tool_config_operation_context_digest(
            &state.config_path,
            &api_key,
            &urls,
            Some(protocol),
            &format!("{models:?}"),
        );
        execute_previewed_tool_config_operation(
            operation,
            Some(protocol.as_str()),
            &context_digest,
            confirmation_token.as_deref(),
            || {
                let mut result = apply_claude_desktop_config_with_model_info(
                    &urls.anthropic,
                    &api_key,
                    &models,
                    &cfg,
                )?;
                attach_tool_protocol(&mut result, protocol);
                Ok(result)
            },
            |launch_context| launch_tool_program_for_atomic_operation(tool, launch_context),
        )
    } else {
        let root_url = tool_config_root_for_tool(&urls, tool);
        let context_digest = tool_config_operation_context_digest(
            &state.config_path,
            &api_key,
            &urls,
            Some(protocol),
            "",
        );
        execute_previewed_tool_config_operation(
            operation,
            Some(protocol.as_str()),
            &context_digest,
            confirmation_token.as_deref(),
            || {
                apply_tool_config_by_name_for_protocol(
                    tool,
                    &urls.openai,
                    root_url,
                    &api_key,
                    protocol,
                )
            },
            |launch_context| {
                if tool == "claude" {
                    launch_claude_code_program_for_atomic_operation(
                        &urls.root,
                        &api_key,
                        launch_context,
                    )
                } else if tool == "claude-science" {
                    launch_claude_science_program_for_atomic_operation(
                        &urls.anthropic,
                        &api_key,
                        launch_context,
                    )
                } else if tool == "gemini" {
                    launch_gemini_cli_program_for_atomic_operation(
                        &urls.root,
                        &api_key,
                        launch_context,
                    )
                } else {
                    launch_tool_program_for_atomic_operation(tool, launch_context)
                }
            },
        )
    }
    .map_err(|err| err.to_string())?;
    Ok(tool_config_operation_response(outcome))
}

fn tool_config_operation_context_digest(
    config_path: &std::path::Path,
    api_key: &str,
    urls: &ToolConfigUrls,
    protocol: Option<ToolProtocol>,
    extra_context: &str,
) -> String {
    let mut digest = <Sha256 as sha2::Digest>::new();
    let config_path = config_path.to_string_lossy();
    for value in [
        config_path.as_ref(),
        api_key,
        urls.root.as_str(),
        urls.openai.as_str(),
        urls.anthropic.as_str(),
        urls.gemini.as_str(),
        protocol.map(ToolProtocol::as_str).unwrap_or(""),
        extra_context,
    ] {
        sha2::Digest::update(&mut digest, (value.len() as u64).to_le_bytes());
        sha2::Digest::update(&mut digest, value.as_bytes());
    }
    hex::encode(sha2::Digest::finalize(digest))
}

fn execute_previewed_tool_config_operation<A, L>(
    operation: &ToolConfigOperationControl,
    protocol: Option<&str>,
    context_digest: &str,
    confirmation_token: Option<&str>,
    apply: A,
    launch: L,
) -> anyhow::Result<ToolConfigWriteOutcome>
where
    A: Fn() -> anyhow::Result<ToolApplyResult>,
    L: FnOnce(&ToolConfigLaunchContext<'_>) -> anyhow::Result<(String, std::path::PathBuf)>,
{
    let apply = &apply;
    execute_atomic_tool_config_operation_with_preview(
        operation,
        protocol,
        context_digest,
        confirmation_token,
        || preview_tool_config_apply(|| apply()),
        || apply(),
        launch,
    )
}

#[tauri::command]
async fn start_tool_program(
    state: State<'_, AppState>,
    tool: String,
    use_configured_environment: bool,
) -> Result<String, String> {
    let credentials = if use_configured_environment
        && matches!(
            tool.as_str(),
            "claude" | "claude-science" | "gemini" | "copilot" | "goose"
        ) {
        let cfg = load_config_from_path(&state.config_path).map_err(|err| err.to_string())?;
        Some((tool_config_urls(&cfg)?, require_config_api_key(&cfg)?))
    } else {
        None
    };
    tauri::async_runtime::spawn_blocking(move || {
        let launched = match (tool.as_str(), credentials) {
            ("claude", Some((urls, api_key))) => launch_claude_code_program(&urls.root, &api_key),
            ("claude-science", Some((urls, api_key))) => {
                launch_claude_science_program(&urls.anthropic, &api_key)
            }
            ("gemini", Some((urls, api_key))) => launch_gemini_cli_program(&urls.root, &api_key),
            ("copilot", Some((urls, api_key))) => {
                launch_copilot_cli_program(&urls.openai, &api_key)
            }
            ("goose", Some((_urls, api_key))) => launch_goose_program(&api_key),
            _ => tool_config::launch_tool_program(&tool),
        };
        launched
            .map(|(_, path)| path.display().to_string())
            .map_err(|err| err.to_string())
    })
    .await
    .map_err(|err| format!("TOOL_START_JOIN_FAILED: {err}"))?
}

#[tauri::command]
async fn locate_tool_program(tool: String) -> Result<ToolProgramLocationResult, String> {
    tool_config::locate_tool_program(&tool).map_err(|err| err.to_string())
}

#[tauri::command]
async fn choose_tool_program(tool: String) -> Result<ToolProgramLocationResult, String> {
    choose_tool_program_path(&tool).map_err(|err| err.to_string())
}

#[tauri::command]
async fn save_tool_program(
    tool: String,
    path: String,
) -> Result<ToolProgramLocationResult, String> {
    save_tool_program_path(&tool, path).map_err(|err| err.to_string())
}

#[tauri::command]
async fn check_tool_config(
    state: State<'_, AppState>,
    tool: String,
    claude_model_settings: Option<ClaudeModelSettings>,
) -> Result<ToolApplyResult, String> {
    let cfg = load_config_from_path(&state.config_path).map_err(|err| err.to_string())?;
    let api_key = cfg.api_key.trim().to_string();
    let urls = tool_config_urls(&cfg)?;
    let root_url = tool_config_root_for_tool(&urls, &tool);
    // This command backs the tool status dot, so it must remain a local configuration check.
    // Server readiness and model discovery are validated when configuration is applied or used.
    let mut result = if tool == "claude" {
        let settings = claude_model_settings.unwrap_or_default();
        validate_claude_model_settings(&settings).map_err(|err| err.to_string())?;
        let mut result =
            check_claude_config_with_model_settings(&urls.anthropic, &api_key, &settings)
                .map_err(|err| err.to_string())?;
        let protocol = resolve_tool_protocol("claude", None).map_err(|err| err.to_string())?;
        attach_tool_protocol(&mut result, protocol);
        result
    } else if tool == "opencode" {
        check_opencode_config(&urls.openai, &api_key).map_err(|err| err.to_string())?
    } else if tool == "vscode" {
        check_vscode_config(&urls.openai, &api_key).map_err(|err| err.to_string())?
    } else if tool == "claude-desktop" {
        let mut result = check_claude_desktop_config(&urls.anthropic, &api_key)
            .map_err(|err| err.to_string())?;
        let protocol =
            resolve_tool_protocol("claude-desktop", None).map_err(|err| err.to_string())?;
        attach_tool_protocol(&mut result, protocol);
        result
    } else {
        check_tool_config_by_name(&tool, &urls.openai, root_url, &api_key)
            .map_err(|err| err.to_string())?
    };
    result
        .details
        .entry("model_sync_policy".to_string())
        .or_insert_with(|| tool_model_sync_policy(&tool).as_str().to_string());
    tool_config::attach_tool_config_presence(&mut result).map_err(|err| err.to_string())?;
    attach_external_launch_environment_warning(&mut result, &tool);
    match tool_program_located_without_process_scan(&tool) {
        Ok(located) => {
            result
                .details
                .insert("program_located".to_string(), located.to_string());
        }
        Err(error) => {
            eprintln!(
                "[const-api][tool-config] program location check failed tool={} error={error:#}",
                tool
            );
        }
    }
    Ok(result)
}

#[tauri::command]
async fn scan_codex_session_history() -> Result<CodexSessionScanResult, String> {
    scan_codex_sessions().map_err(|err| err.to_string())
}

async fn fetch_local_platform_models_for_operation(
    cfg: &ClientConfig,
    operation: &ToolConfigOperationControl,
    tool: &str,
) -> Result<Vec<ToolModelInfo>, String> {
    update_tool_config_operation_progress(operation, "loading_models", None, None);
    let refresh_timeout = if operation.restarts() && existing_model_tool_config_is_ready(cfg, tool)
    {
        TOOL_CONFIG_OPTIONAL_MODEL_REFRESH_TIMEOUT
    } else {
        TOOL_CONFIG_MODEL_REFRESH_TIMEOUT
    };
    let timeout = tool_config_operation_stage_timeout(operation, "model refresh", refresh_timeout)
        .map_err(|err| err.to_string())?;
    let urls = tool_config_urls(cfg)?;
    let request = short_http_client()
        .get(format!("{}/models", urls.openai))
        .header("Authorization", local_proxy_auth_header(cfg))
        .timeout(timeout)
        .send();
    let response = tokio::select! {
        _ = wait_for_tool_config_operation_cancellation(operation) => {
            return Err("TOOL_CONFIG_OPERATION_CANCELLED: cancelled during model refresh".to_string());
        }
        response = request => response,
    };
    let response = match response {
        Ok(response) => response,
        Err(_) => {
            ensure_tool_config_operation_active(operation, "model refresh response")
                .map_err(|err| err.to_string())?;
            return Ok(Vec::new());
        }
    };
    if !response.status().is_success() {
        ensure_tool_config_operation_active(operation, "model refresh status")
            .map_err(|err| err.to_string())?;
        return Ok(Vec::new());
    }
    let value = tokio::select! {
        _ = wait_for_tool_config_operation_cancellation(operation) => {
            return Err("TOOL_CONFIG_OPERATION_CANCELLED: cancelled while reading models".to_string());
        }
        value = response.json::<serde_json::Value>() => value,
    };
    let value = match value {
        Ok(value) => value,
        Err(_) => {
            ensure_tool_config_operation_active(operation, "model refresh body")
                .map_err(|err| err.to_string())?;
            return Ok(Vec::new());
        }
    };
    ensure_tool_config_operation_active(operation, "model refresh completion")
        .map_err(|err| err.to_string())?;
    Ok(tool_models_from_response(&value))
}

async fn require_tool_model_snapshot_for_operation(
    cfg: &ClientConfig,
    operation: &ToolConfigOperationControl,
    tool: &str,
    tool_label: &str,
) -> Result<Vec<ToolModelInfo>, String> {
    let models = fetch_local_platform_models_for_operation(cfg, operation, tool).await?;
    if models.is_empty() {
        Err(model_refresh_unavailable_error(
            cfg, operation, tool, tool_label,
        ))
    } else {
        Ok(models)
    }
}

async fn openclaw_tool_model_snapshot_for_operation(
    cfg: &ClientConfig,
    operation: &ToolConfigOperationControl,
) -> Result<Vec<ToolModelInfo>, String> {
    let live_models = fetch_local_platform_models_for_operation(cfg, operation, "openclaw").await?;
    if !live_models.is_empty() {
        return Ok(live_models);
    }
    if operation.restarts() && existing_model_tool_config_is_ready(cfg, "openclaw") {
        return Err(format!(
            "{TOOL_CONFIG_MODEL_REFRESH_USE_EXISTING}: OpenClaw"
        ));
    }

    // OpenClaw needs an explicit model array, but configuring the pipe should
    // not depend on a transient /v1/models refresh. The signed/embedded active
    // catalog carries the same ToolModelInfo projection and is a safe offline
    // fallback; live data still wins whenever it is available.
    let fallback_models = active_tool_models();
    if fallback_models.is_empty() {
        Err("OpenClaw requires a model list, but both online discovery and the bundled catalog are unavailable.".to_string())
    } else {
        Ok(fallback_models)
    }
}

async fn additional_tool_model_snapshot_for_operation(
    cfg: &ClientConfig,
    operation: &ToolConfigOperationControl,
    tool: &str,
    tool_label: &str,
) -> Result<Vec<ToolModelInfo>, String> {
    let live_models = fetch_local_platform_models_for_operation(cfg, operation, tool).await?;
    if !live_models.is_empty() {
        return Ok(live_models);
    }
    if operation.restarts() && existing_model_tool_config_is_ready(cfg, tool) {
        return Err(format!(
            "{TOOL_CONFIG_MODEL_REFRESH_USE_EXISTING}: {tool_label}"
        ));
    }

    // These tools require an explicit model catalog. A transient local model
    // refresh must not make configuration unavailable when the signed/embedded
    // active catalog already contains the same normalized metadata.
    let fallback_models = active_tool_models();
    if fallback_models.is_empty() {
        Err(format!("TOOL_CONFIG_MODELS_UNAVAILABLE: {tool_label}"))
    } else {
        Ok(fallback_models)
    }
}

fn model_refresh_unavailable_error(
    cfg: &ClientConfig,
    operation: &ToolConfigOperationControl,
    tool: &str,
    tool_label: &str,
) -> String {
    if operation.restarts() && existing_model_tool_config_is_ready(cfg, tool) {
        format!("{TOOL_CONFIG_MODEL_REFRESH_USE_EXISTING}: {tool_label}")
    } else {
        format!("TOOL_CONFIG_MODELS_UNAVAILABLE: {tool_label}")
    }
}

fn existing_model_tool_config_is_ready(cfg: &ClientConfig, tool: &str) -> bool {
    if tool_model_sync_policy(tool) == ToolModelSyncPolicy::None
        && !(tool == "codex" && current_codex_model_source().ok() == Some(CodexModelSource::Const))
    {
        return false;
    }
    let Ok(urls) = tool_config_urls(cfg) else {
        return false;
    };
    let api_key = cfg.api_key.trim();
    if api_key.is_empty() {
        return false;
    }
    let checked = match tool {
        "opencode" => check_opencode_config(&urls.openai, api_key),
        "vscode" => check_vscode_config(&urls.openai, api_key),
        "claude-desktop" => check_claude_desktop_config(&urls.anthropic, api_key),
        _ => check_tool_config_by_name(
            tool,
            &urls.openai,
            tool_config_root_for_tool(&urls, tool),
            api_key,
        ),
    };
    checked.is_ok_and(|result| result.already_configured)
}

fn require_config_api_key(cfg: &ClientConfig) -> Result<String, String> {
    let api_key = cfg.api_key.trim();
    if api_key.is_empty() {
        Err("Enter the local API key first.".to_string())
    } else {
        Ok(api_key.to_string())
    }
}

async fn refresh_endpoint_registry_inner(_: &AppState) -> Result<EndpointDiscoveryResult, String> { Ok(EndpointDiscoveryResult { source: "local_only".into(), version: 0, platform_id: String::new(), endpoints: Vec::new(), refresh_warning: None }) }

#[tauri::command]
async fn refresh_endpoint_registry(
    state: State<'_, AppState>,
) -> Result<EndpointDiscoveryResult, String> {
    refresh_endpoint_registry_inner(state.inner()).await
}

#[tauri::command]
fn get_release_source_status(
    state: State<'_, AppState>,
) -> Result<crate::release_sources::ReleaseSourceStatus, String> {
    crate::release_sources::trusted_release_sources(&state.config_path)
        .map(|active| active.status())
        .map_err(|error| error.to_string())
}

async fn discover_endpoints(
    registry_sources: &[String],
    cache_path: &std::path::Path,
) -> Result<EndpointDiscoveryResult> {
    let (cache, mut last_err) = match load_cached_endpoint_manifest(cache_path) {
        Ok(cache) => (Some(cache), None),
        Err(err) => (None, Some(err)),
    };
    let bundled = match load_bundled_endpoint_manifest() {
        Ok(bundled) => Some(bundled),
        Err(error) => {
            last_err = Some(error);
            None
        }
    };
    let sources: Vec<_> = registry_sources
        .iter()
        .map(|source| source.trim().to_string())
        .filter(|source| !source.is_empty())
        .collect();
    if sources.is_empty() {
        return fallback_endpoint_discovery(
            cache.as_ref(),
            bundled.as_ref(),
            "no registry sources configured",
        );
    }
    let client = fresh_endpoint_http_client()?;
    let minimum_version = cache
        .as_ref()
        .map(VerifiedEndpointManifest::version)
        .into_iter()
        .chain(bundled.as_ref().map(VerifiedEndpointManifest::version))
        .max()
        .unwrap_or_default();
    for source in sources {
        match fetch_endpoint_manifest(&client, &source, cache_path, minimum_version).await {
            Ok(manifest) => {
                match endpoint_discovery_from_verified_manifest(source.clone(), &manifest) {
                    Ok(result) => return Ok(result),
                    Err(err) => {
                        last_err = Some(err);
                        continue;
                    }
                }
            }
            Err(err) => last_err = Some(err),
        }
    }
    fallback_endpoint_discovery(
        cache.as_ref(),
        bundled.as_ref(),
        last_err
            .unwrap_or_else(|| anyhow!("endpoint registry discovery failed"))
            .to_string(),
    )
}

async fn fetch_local_json(
    state: &State<'_, AppState>,
    path: &str,
) -> Result<serde_json::Value, String> {
    let cfg = load_config_from_path(&state.config_path).map_err(|err| err.to_string())?;
    let url = format!("http://{}{}", cfg.listen, path);
    let resp = short_http_client()
        .get(url)
        .header("Authorization", local_proxy_auth_header(&cfg))
        .send()
        .await
        .map_err(|err| err.to_string())?;
    let status = resp.status();
    let body = resp.text().await.map_err(|err| err.to_string())?;
    if !status.is_success() {
        let detail = body.trim().chars().take(240).collect::<String>();
        if detail.is_empty() {
            return Err(format!("local proxy returned HTTP {status}"));
        }
        return Err(format!("local proxy returned HTTP {status}: {detail}"));
    }
    serde_json::from_str::<serde_json::Value>(&body).map_err(|err| err.to_string())
}

async fn fetch_supplier_route_plan_inner(
    config_path: &std::path::PathBuf,
    model: String,
    path: String,
    protocol: String,
    body: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let cfg = load_config_from_path(config_path).map_err(|err| err.to_string())?;
    let url = format!("http://{}/api/supplier/route/plan", cfg.listen);
    let mut req = short_http_client()
        .post(url)
        .header("Authorization", local_proxy_auth_header(&cfg))
        .header("Content-Type", "application/json");
    if cfg.allow_model_equivalence {
        req = req
            .header("x-const-api-allow-model-substitution", "1")
            .header("x-const-api-allow-model-equivalence", "1");
    }
    let resp = req
        .json(&serde_json::json!({
            "model": model,
            "path": path,
            "protocol": protocol,
            "body": body,
        }))
        .send()
        .await
        .map_err(|err| {
            format!("Local entry is not running or the local API key is invalid: request local proxy failed: {err}")
        })?;
    let status = resp.status();
    let raw = resp.text().await.map_err(|err| err.to_string())?;
    if !status.is_success() {
        return Err(format!(
            "Local entry is not running or the local API key is invalid: local proxy returned HTTP {}: {}",
            status.as_u16(),
            raw
        ));
    }
    serde_json::from_str::<serde_json::Value>(&raw).map_err(|err| err.to_string())
}
