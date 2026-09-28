use crate::{DesktopLifecycle, config::commit_config_update, model::AppState};
use futures_util::{StreamExt, stream::FuturesUnordered};
use reqwest::header::{ACCEPT, RANGE};
use semver::Version;
use serde::Serialize;
use std::{
    cmp::Ordering,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering as AtomicOrdering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter, Manager, State, Wry};
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::{
    sync::Notify,
    time::{Instant, sleep_until},
};

const DEFAULT_CHECK_TIMEOUT_MS: u64 = 15_000;
const PROBE_BYTES: usize = 64 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const SOURCE_SETTLE_WINDOW: Duration = Duration::from_millis(700);
const PROBE_SETTLE_WINDOW: Duration = Duration::from_millis(700);
const PRIMARY_SPEED_FLOOR_PERCENT: u64 = 80;
const NATIVE_UPDATE_INITIAL_DELAY: Duration = Duration::from_secs(5);
const NATIVE_UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(30 * 60);
const NATIVE_UPDATE_NATURAL_IDLE_WINDOW: Duration = Duration::from_secs(10 * 60);
const NATIVE_UPDATE_STATUS_HEARTBEAT: Duration = Duration::from_secs(5);
const NATIVE_UPDATE_EVENT: &str = "const-api://update-state";

struct SourceCheck {
    source_index: usize,
    manifest_url: String,
    manifest_latency_ms: u64,
    result: Result<Option<Update>, String>,
}

#[derive(Clone, Copy, Default)]
struct DownloadProbe {
    successful: bool,
    first_byte_ms: Option<u64>,
    elapsed_ms: Option<u64>,
    bytes: usize,
    bytes_per_second: u64,
}

struct Candidate {
    source_index: usize,
    manifest_url: String,
    manifest_latency_ms: u64,
    version: Version,
    update: Update,
    probe: DownloadProbe,
}

#[derive(Clone, Copy)]
struct RankMetrics {
    source_index: usize,
    manifest_latency_ms: u64,
    probe_successful: bool,
    bytes_per_second: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateSourceMetadata {
    source_url: String,
    status: String,
    latency_ms: Option<u64>,
    version: Option<String>,
    error: Option<String>,
}

fn configured_update_endpoints(_: &AppState) -> Result<Vec<String>, String> { Ok(Vec::new()) }

async fn check_source(
    app: AppHandle<Wry>,
    source_index: usize,
    manifest_url: String,
    timeout: Duration,
) -> SourceCheck {
    let started = Instant::now();
    let result = async {
        let endpoint: reqwest::Url = manifest_url
            .parse()
            .map_err(|error| format!("invalid update endpoint: {error}"))?;
        let updater = app
            .updater_builder()
            .endpoints(vec![endpoint])
            .map_err(|error| error.to_string())?
            .timeout(timeout)
            .build()
            .map_err(|error| error.to_string())?;
        updater.check().await.map_err(|error| error.to_string())
    }
    .await;

    SourceCheck {
        source_index,
        manifest_url,
        manifest_latency_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        result,
    }
}

async fn collect_source_checks(
    app: &AppHandle<Wry>,
    endpoints: &[String],
    timeout: Duration,
) -> Vec<SourceCheck> {
    let mut pending = endpoints
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, endpoint)| check_source(app.clone(), index, endpoint, timeout))
        .collect::<FuturesUnordered<_>>();
    let mut completed = Vec::with_capacity(endpoints.len());
    let mut settle_deadline = None;

    while !pending.is_empty() {
        let next = match settle_deadline {
            Some(deadline) => tokio::select! {
                result = pending.next() => result,
                _ = sleep_until(deadline) => None,
            },
            None => pending.next().await,
        };
        let Some(check) = next else {
            break;
        };
        if check.result.is_ok() && settle_deadline.is_none() {
            // Give another region a short opportunity to report a newer
            // manifest without making a healthy source wait for a blocked one.
            settle_deadline = Some(Instant::now() + SOURCE_SETTLE_WINDOW);
        }
        completed.push(check);
    }
    completed
}

async fn probe_download(update: &Update) -> DownloadProbe {
    let client = match reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(4))
        .timeout(PROBE_TIMEOUT)
        .user_agent("CONST-API-Updater-Probe")
        .build()
    {
        Ok(client) => client,
        Err(_) => return DownloadProbe::default(),
    };

    let started = Instant::now();
    let response = match client
        .get(update.download_url.clone())
        .header(ACCEPT, "application/octet-stream")
        .header(RANGE, format!("bytes=0-{}", PROBE_BYTES - 1))
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => response,
        _ => return DownloadProbe::default(),
    };

    let mut stream = response.bytes_stream();
    let mut bytes = 0usize;
    let mut first_byte_ms = None;
    while bytes < PROBE_BYTES {
        match stream.next().await {
            Some(Ok(chunk)) => {
                if first_byte_ms.is_none() {
                    first_byte_ms =
                        Some(started.elapsed().as_millis().min(u64::MAX as u128) as u64);
                }
                bytes = bytes.saturating_add(chunk.len().min(PROBE_BYTES - bytes));
            }
            Some(Err(_)) => return DownloadProbe::default(),
            None => break,
        }
    }

    if bytes == 0 {
        return DownloadProbe::default();
    }
    let elapsed_ms = started.elapsed().as_millis().max(1).min(u64::MAX as u128) as u64;
    let bytes_per_second =
        ((bytes as u128) * 1_000 / elapsed_ms as u128).min(u64::MAX as u128) as u64;
    DownloadProbe {
        successful: true,
        first_byte_ms,
        elapsed_ms: Some(elapsed_ms),
        bytes,
        bytes_per_second,
    }
}

async fn collect_download_probes(candidates: &[Candidate]) -> Vec<DownloadProbe> {
    let mut completed = vec![DownloadProbe::default(); candidates.len()];
    let mut pending = candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let update = candidate.update.clone();
            async move { (index, probe_download(&update).await) }
        })
        .collect::<FuturesUnordered<_>>();
    let mut settle_deadline = None;

    while !pending.is_empty() {
        let next = match settle_deadline {
            Some(deadline) => tokio::select! {
                result = pending.next() => result,
                _ = sleep_until(deadline) => None,
            },
            None => pending.next().await,
        };
        let Some((index, probe)) = next else {
            break;
        };
        if probe.successful && settle_deadline.is_none() {
            settle_deadline = Some(Instant::now() + PROBE_SETTLE_WINDOW);
        }
        completed[index] = probe;
    }
    completed
}

fn rank_order(left: RankMetrics, right: RankMetrics, fastest_probe: u64) -> Ordering {
    if fastest_probe == 0 {
        return left
            .source_index
            .cmp(&right.source_index)
            .then_with(|| left.manifest_latency_ms.cmp(&right.manifest_latency_ms));
    }
    let speed_floor = fastest_probe.saturating_mul(PRIMARY_SPEED_FLOOR_PERCENT) / 100;
    let left_preferred = left.probe_successful && left.bytes_per_second >= speed_floor;
    let right_preferred = right.probe_successful && right.bytes_per_second >= speed_floor;

    right_preferred
        .cmp(&left_preferred)
        .then_with(|| {
            if left_preferred && right_preferred {
                left.source_index.cmp(&right.source_index)
            } else {
                right.bytes_per_second.cmp(&left.bytes_per_second)
            }
        })
        .then_with(|| left.manifest_latency_ms.cmp(&right.manifest_latency_ms))
        .then_with(|| left.source_index.cmp(&right.source_index))
}

fn candidate_order(left: &Candidate, right: &Candidate, fastest_probe: u64) -> Ordering {
    rank_order(
        RankMetrics {
            source_index: left.source_index,
            manifest_latency_ms: left.manifest_latency_ms,
            probe_successful: left.probe.successful,
            bytes_per_second: left.probe.bytes_per_second,
        },
        RankMetrics {
            source_index: right.source_index,
            manifest_latency_ms: right.manifest_latency_ms,
            probe_successful: right.probe.successful,
            bytes_per_second: right.probe.bytes_per_second,
        },
        fastest_probe,
    )
}

struct RankedUpdateSelection {
    candidates: Vec<Candidate>,
    sources: Vec<UpdateSourceMetadata>,
    preferred_source_url: Option<String>,
}

/// Checks every configured updater manifest independently, keeps only the
/// newest version, then orders equivalent mirrors by a bounded range probe.
/// The first configured source remains preferred while it reaches at least
/// 80% of the fastest measured source. Both the native background updater and
/// the UI command use this single selection path.
async fn ranked_update_candidates(
    app: &AppHandle<Wry>,
    state: &AppState,
    timeout: Duration,
) -> Result<RankedUpdateSelection, String> {
    let endpoints = configured_update_endpoints(state)?;
    let checks = collect_source_checks(app, &endpoints, timeout).await;

    let mut healthy_sources = 0usize;
    let mut failures = Vec::new();
    let mut candidates = Vec::new();
    let mut healthy_rank = Vec::new();
    let mut sources = endpoints
        .iter()
        .map(|source_url| UpdateSourceMetadata {
            source_url: source_url.clone(),
            status: "deferred".to_string(),
            latency_ms: None,
            version: None,
            error: None,
        })
        .collect::<Vec<_>>();
    for check in checks {
        let source = &mut sources[check.source_index];
        source.latency_ms = Some(check.manifest_latency_ms);
        match check.result {
            Ok(None) => {
                healthy_sources += 1;
                source.status = "current".to_string();
                healthy_rank.push((
                    check.source_index,
                    check.manifest_latency_ms,
                    check.manifest_url,
                ));
            }
            Ok(Some(update)) => match Version::parse(&update.version) {
                Ok(version) => {
                    healthy_sources += 1;
                    source.status = "available".to_string();
                    source.version = Some(update.version.clone());
                    healthy_rank.push((
                        check.source_index,
                        check.manifest_latency_ms,
                        check.manifest_url.clone(),
                    ));
                    candidates.push(Candidate {
                        source_index: check.source_index,
                        manifest_url: check.manifest_url,
                        manifest_latency_ms: check.manifest_latency_ms,
                        version,
                        update,
                        probe: DownloadProbe::default(),
                    });
                }
                Err(error) => {
                    let message = format!(
                        "{}: invalid updater version {}: {error}",
                        check.manifest_url, update.version
                    );
                    source.status = "failed".to_string();
                    source.error = Some(message.clone());
                    failures.push(message);
                }
            },
            Err(error) => {
                let message = format!("{}: {error}", check.manifest_url);
                source.status = "failed".to_string();
                source.error = Some(message.clone());
                failures.push(message);
            }
        }
    }

    if candidates.is_empty() {
        if healthy_sources > 0 {
            healthy_rank.sort();
            return Ok(RankedUpdateSelection {
                candidates,
                sources,
                preferred_source_url: healthy_rank.first().map(|item| item.2.clone()),
            });
        }
        return Err(format!(
            "all configured update sources failed: {}",
            failures.join("; ")
        ));
    }

    let Some(newest) = candidates
        .iter()
        .map(|candidate| candidate.version.clone())
        .max()
    else {
        return Err("update candidates disappeared during ranking".to_string());
    };
    candidates.retain(|candidate| candidate.version == newest);

    if candidates.len() > 1 {
        let probes = collect_download_probes(&candidates).await;
        for (candidate, probe) in candidates.iter_mut().zip(probes) {
            candidate.probe = probe;
        }
    }
    let fastest_probe = candidates
        .iter()
        .map(|candidate| candidate.probe.bytes_per_second)
        .max()
        .unwrap_or_default();
    candidates.sort_by(|left, right| candidate_order(left, right, fastest_probe));
    let preferred_source_url = candidates
        .first()
        .map(|candidate| candidate.manifest_url.clone());

    for candidate in &candidates {
        log::info!(
            "[const-api][update] source={} version={} manifest_ms={} probe_first_byte_ms={} probe_bytes={} probe_ms={} probe_bps={}",
            candidate.manifest_url,
            candidate.update.version,
            candidate.manifest_latency_ms,
            candidate.probe.first_byte_ms.unwrap_or_default(),
            candidate.probe.bytes,
            candidate.probe.elapsed_ms.unwrap_or_default(),
            candidate.probe.bytes_per_second,
        );
    }
    Ok(RankedUpdateSelection {
        candidates,
        sources,
        preferred_source_url,
    })
}

struct PreparedNativeUpdate {
    update: Update,
    bytes: Vec<u8>,
    version: String,
    source_url: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeUpdateStatus {
    status: String,
    version: Option<String>,
    error: Option<String>,
    checked_at_unix_ms: u64,
    source_url: Option<String>,
    sources: Vec<UpdateSourceMetadata>,
    preferred_source_url: Option<String>,
    automatic: bool,
    readiness: crate::update_activity::UpdateInstallReadiness,
}

pub(crate) struct NativeUpdateManager {
    automatic: AtomicBool,
    check_requested: AtomicBool,
    install_requested: AtomicBool,
    wake: Notify,
    operation: tokio::sync::Mutex<()>,
    prepared: tokio::sync::Mutex<Option<PreparedNativeUpdate>>,
    status: StdMutex<NativeUpdateStatus>,
}

impl NativeUpdateManager {
    pub(crate) fn new(automatic: bool) -> Arc<Self> {
        Arc::new(Self {
            automatic: AtomicBool::new(automatic),
            check_requested: AtomicBool::new(false),
            install_requested: AtomicBool::new(false),
            wake: Notify::new(),
            operation: tokio::sync::Mutex::new(()),
            prepared: tokio::sync::Mutex::new(None),
            status: StdMutex::new(NativeUpdateStatus {
                status: "idle".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
                error: None,
                checked_at_unix_ms: 0,
                source_url: None,
                sources: Vec::new(),
                preferred_source_url: None,
                automatic,
                readiness: crate::update_activity::update_install_readiness(),
            }),
        })
    }

    fn snapshot(&self) -> NativeUpdateStatus {
        let mut status = self
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        status.automatic = self.automatic.load(AtomicOrdering::Acquire);
        status.readiness = crate::update_activity::update_install_readiness();
        status
    }

    fn publish(
        &self,
        app: &AppHandle<Wry>,
        update: impl FnOnce(&mut NativeUpdateStatus),
    ) -> NativeUpdateStatus {
        let status = {
            let mut status = self
                .status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            update(&mut status);
            status.automatic = self.automatic.load(AtomicOrdering::Acquire);
            status.readiness = crate::update_activity::update_install_readiness();
            status.clone()
        };
        if let Err(error) = app.emit(NATIVE_UPDATE_EVENT, status) {
            log::debug!("[const-api][update] status event skipped: {error}");
        }
        self.snapshot()
    }

    async fn run_cycle(
        self: &Arc<Self>,
        app: &AppHandle<Wry>,
        lifecycle: &Arc<DesktopLifecycle>,
        mut force_install: bool,
    ) {
        let _operation = self.operation.lock().await;
        force_install |= self.install_requested.swap(false, AtomicOrdering::AcqRel);
        if self.prepared.lock().await.is_some() {
            force_install |= self.install_requested.swap(false, AtomicOrdering::AcqRel);
            if self.automatic.load(AtomicOrdering::Acquire) || force_install {
                self.install_prepared(app, lifecycle, force_install).await;
            }
            return;
        }

        self.publish(app, |status| {
            status.status = "checking".to_string();
            status.error = None;
            status.checked_at_unix_ms = unix_now_ms();
        });
        let app_state = app.state::<AppState>();
        let selection = match ranked_update_candidates(
            app,
            app_state.inner(),
            Duration::from_millis(DEFAULT_CHECK_TIMEOUT_MS),
        )
        .await
        {
            Ok(selection) => selection,
            Err(error) => {
                self.publish(app, |status| {
                    status.status = "error".to_string();
                    status.error = Some(error.clone());
                    status.checked_at_unix_ms = unix_now_ms();
                });
                log::warn!("[const-api][update] native check failed: {error}");
                return;
            }
        };
        let sources = selection.sources.clone();
        let preferred_source_url = selection.preferred_source_url.clone();
        if selection.candidates.is_empty() {
            self.publish(app, |status| {
                status.status = "idle".to_string();
                status.version = Some(env!("CARGO_PKG_VERSION").to_string());
                status.error = None;
                status.checked_at_unix_ms = unix_now_ms();
                status.source_url = preferred_source_url.clone();
                status.sources = sources.clone();
                status.preferred_source_url = preferred_source_url.clone();
            });
            return;
        }

        let target_version = selection.candidates[0].update.version.clone();
        self.publish(app, |status| {
            status.status = "downloading".to_string();
            status.version = Some(target_version.clone());
            status.error = None;
            status.checked_at_unix_ms = unix_now_ms();
            status.sources = sources.clone();
            status.preferred_source_url = preferred_source_url.clone();
        });

        let prepared = match download_ranked_update(selection.candidates).await {
            Ok(prepared) => prepared,
            Err(error) => {
                self.publish(app, |status| {
                    status.status = "error".to_string();
                    status.version = Some(target_version.clone());
                    status.error = Some(error.clone());
                    status.checked_at_unix_ms = unix_now_ms();
                    status.sources = sources.clone();
                    status.preferred_source_url = preferred_source_url.clone();
                });
                log::warn!("[const-api][update] native download failed: {error}");
                return;
            }
        };
        let selected_source = prepared.source_url.clone();
        *self.prepared.lock().await = Some(prepared);
        self.publish(app, |status| {
            status.status = "ready".to_string();
            status.version = Some(target_version.clone());
            status.error = None;
            status.checked_at_unix_ms = unix_now_ms();
            status.source_url = Some(selected_source.clone());
            status.sources = sources.clone();
            status.preferred_source_url = preferred_source_url.clone();
        });

        force_install |= self.install_requested.swap(false, AtomicOrdering::AcqRel);
        if force_install || self.automatic.load(AtomicOrdering::Acquire) {
            self.install_prepared(app, lifecycle, force_install).await;
        }
    }

    async fn install_prepared(
        self: &Arc<Self>,
        app: &AppHandle<Wry>,
        lifecycle: &Arc<DesktopLifecycle>,
        mut force_install: bool,
    ) {
        let version = match self.prepared.lock().await.as_ref() {
            Some(prepared) => prepared.version.clone(),
            None => return,
        };
        let natural_idle_deadline = Instant::now() + NATIVE_UPDATE_NATURAL_IDLE_WINDOW;
        let mut draining = force_install;
        if draining {
            crate::update_activity::begin_update_drain();
        }

        loop {
            force_install |= self.install_requested.swap(false, AtomicOrdering::AcqRel);
            if force_install && !draining {
                draining = true;
                crate::update_activity::begin_update_drain();
            }
            if !force_install && !self.automatic.load(AtomicOrdering::Acquire) {
                crate::update_activity::release_update_install_gate();
                self.publish(app, |status| status.status = "ready".to_string());
                return;
            }
            if !draining && Instant::now() >= natural_idle_deadline {
                draining = true;
                crate::update_activity::begin_update_drain();
                log::info!(
                    "[const-api][update] entering native drain after {}s natural-idle wait",
                    NATIVE_UPDATE_NATURAL_IDLE_WINDOW.as_secs()
                );
            }

            let readiness = crate::update_activity::try_prepare_update_install();
            if readiness.gate_acquired {
                break;
            }
            self.publish(app, |status| {
                status.status = "ready_waiting_idle".to_string();
                status.version = Some(version.clone());
                status.error = None;
            });
            tokio::select! {
                _ = crate::update_activity::wait_for_update_activity_change() => {}
                _ = self.wake.notified() => {}
                _ = tokio::time::sleep(NATIVE_UPDATE_STATUS_HEARTBEAT) => {}
            }
        }

        let Some(prepared) = self.prepared.lock().await.take() else {
            crate::update_activity::release_update_install_gate();
            return;
        };
        let restart_mode = match crate::stage_update_relaunch_for_install_native(app, &version) {
            Ok(mode) => mode,
            Err(error) => {
                *self.prepared.lock().await = Some(prepared);
                crate::update_activity::release_update_install_gate();
                self.publish(app, |status| {
                    status.status = "ready".to_string();
                    status.error = Some(error.clone());
                });
                return;
            }
        };
        self.publish(app, |status| {
            status.status = "installing".to_string();
            status.version = Some(version.clone());
            status.error = None;
        });
        log::info!("[const-api][update] native install begin version={version}");
        if let Err(error) = prepared.update.install(&prepared.bytes) {
            let error = error.to_string();
            let _ = crate::clear_staged_update_relaunch_native();
            *self.prepared.lock().await = Some(prepared);
            crate::update_activity::release_update_install_gate();
            self.publish(app, |status| {
                status.status = "ready".to_string();
                status.error = Some(error.clone());
            });
            log::error!("[const-api][update] native install failed: {error}");
            return;
        }
        lifecycle.request_restart(app.clone(), restart_mode);
    }
}

async fn download_ranked_update(
    candidates: Vec<Candidate>,
) -> Result<PreparedNativeUpdate, String> {
    let mut failures = Vec::new();
    for candidate in candidates {
        let source_url = candidate.manifest_url.clone();
        match candidate.update.download(|_, _| {}, || {}).await {
            Ok(bytes) => {
                return Ok(PreparedNativeUpdate {
                    version: candidate.update.version.clone(),
                    update: candidate.update,
                    bytes,
                    source_url,
                });
            }
            Err(error) if retryable_update_transport_error(&error) => {
                failures.push(format!("{source_url}: {error}"));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Err(format!(
        "all update download sources failed with network errors: {}",
        failures.join("; ")
    ))
}

fn retryable_update_transport_error(error: &tauri_plugin_updater::Error) -> bool {
    matches!(
        error,
        tauri_plugin_updater::Error::Reqwest(_) | tauri_plugin_updater::Error::Network(_)
    )
}

fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

pub(crate) fn start_native_update_manager(
    app: AppHandle<Wry>,
    manager: Arc<NativeUpdateManager>,
    lifecycle: Arc<DesktopLifecycle>,
) {}

async fn run_native_update_manager(
    app: AppHandle<Wry>,
    manager: Arc<NativeUpdateManager>,
    lifecycle: Arc<DesktopLifecycle>,
) {
    // A previous manager generation may have panicked after beginning a drain.
    // Clear only the updater-owned gate before resuming the same state machine.
    crate::update_activity::release_update_install_gate();
    tokio::select! {
        _ = tokio::time::sleep(NATIVE_UPDATE_INITIAL_DELAY) => {}
        _ = manager.wake.notified() => {}
    }
    let mut periodic_check_due = true;
    loop {
        let check_requested = manager.check_requested.swap(false, AtomicOrdering::AcqRel);
        let install_requested = manager.install_requested.load(AtomicOrdering::Acquire);
        if periodic_check_due || check_requested || install_requested {
            manager.run_cycle(&app, &lifecycle, install_requested).await;
        }
        periodic_check_due = false;
        tokio::select! {
            _ = manager.wake.notified() => {}
            _ = tokio::time::sleep(NATIVE_UPDATE_CHECK_INTERVAL) => {
                periodic_check_due = true;
            }
        }
    }
}

#[tauri::command]
pub(crate) fn native_update_status(
    manager: State<'_, Arc<NativeUpdateManager>>,
) -> NativeUpdateStatus {
    manager.snapshot()
}

#[tauri::command]
pub(crate) fn set_automatic_updates() -> Result<NativeUpdateStatus, String> { Err("Application updates are unavailable in the local edition".into()) }

#[tauri::command]
pub(crate) fn request_native_update_check() -> Result<NativeUpdateStatus, String> { Err("Application updates are unavailable in the local edition".into()) }

#[tauri::command]
pub(crate) fn request_native_update_install() -> Result<NativeUpdateStatus, String> { Err("Application updates are unavailable in the local edition".into()) }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_manager_starts_with_the_persisted_automatic_policy() {
        assert!(!NativeUpdateManager::new(false).snapshot().automatic);
        assert!(NativeUpdateManager::new(true).snapshot().automatic);
    }

    #[test]
    fn speed_floor_keeps_primary_when_it_is_close_to_fastest() {
        let primary = RankMetrics {
            source_index: 0,
            manifest_latency_ms: 90,
            probe_successful: true,
            bytes_per_second: 900,
        };
        let secondary = RankMetrics {
            source_index: 1,
            manifest_latency_ms: 50,
            probe_successful: true,
            bytes_per_second: 1_000,
        };
        assert_eq!(rank_order(primary, secondary, 1_000), Ordering::Less);

        let slow_primary = RankMetrics {
            bytes_per_second: 600,
            ..primary
        };
        assert_eq!(
            rank_order(slow_primary, secondary, 1_000),
            Ordering::Greater
        );
    }

    #[test]
    fn failed_probe_never_precedes_a_successful_probe() {
        let failed_primary = RankMetrics {
            source_index: 0,
            manifest_latency_ms: 10,
            probe_successful: false,
            bytes_per_second: 0,
        };
        let successful_secondary = RankMetrics {
            source_index: 1,
            manifest_latency_ms: 100,
            probe_successful: true,
            bytes_per_second: 1_000,
        };
        assert_eq!(
            rank_order(failed_primary, successful_secondary, 1_000),
            Ordering::Greater
        );
    }

    #[test]
    fn configured_order_wins_when_every_probe_fails() {
        let primary = RankMetrics {
            source_index: 0,
            manifest_latency_ms: 500,
            probe_successful: false,
            bytes_per_second: 0,
        };
        let faster_manifest = RankMetrics {
            source_index: 1,
            manifest_latency_ms: 10,
            probe_successful: false,
            bytes_per_second: 0,
        };
        assert_eq!(rank_order(primary, faster_manifest, 0), Ordering::Less);
    }
}
