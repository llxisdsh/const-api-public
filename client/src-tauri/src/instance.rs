use crate::launch::LaunchMode;
use anyhow::{Context, Result, anyhow};
use fs2::FileExt;
use rand::TryRng;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::ErrorKind,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

const LOCK_FILE_NAME: &str = "client-runtime.lock";
const METADATA_FILE_NAME: &str = "client-runtime.json";
const CONTROL_MAX_LINE_BYTES: u64 = 8 * 1024;
const CONTROL_TIMEOUT: Duration = Duration::from_millis(500);
const OWNER_DISCOVERY_ATTEMPTS: usize = 10;

#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub(crate) struct RuntimeControlStatus {
    pub(crate) lifecycle: String,
    pub(crate) proxy_running: bool,
    pub(crate) supplier_running: bool,
    pub(crate) readiness: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub(crate) struct InstanceSnapshot {
    pub(crate) pid: u32,
    pub(crate) mode: LaunchMode,
    pub(crate) version: String,
    pub(crate) started_at_unix: u64,
    pub(crate) reachable: bool,
    pub(crate) runtime: RuntimeControlStatus,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct InstanceMetadata {
    pid: u32,
    mode: LaunchMode,
    version: String,
    started_at_unix: u64,
    control_addr: SocketAddr,
    control_token: String,
}

impl InstanceMetadata {
    fn snapshot(&self, reachable: bool, runtime: RuntimeControlStatus) -> InstanceSnapshot {
        InstanceSnapshot {
            pid: self.pid,
            mode: self.mode,
            version: self.version.clone(),
            started_at_unix: self.started_at_unix,
            reachable,
            runtime,
        }
    }
}

#[derive(Deserialize, Serialize)]
struct ControlRequest {
    token: String,
    command: String,
}

#[derive(Deserialize, Serialize)]
struct ControlResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instance: Option<InstanceSnapshot>,
    #[serde(default)]
    show_accepted: bool,
}

pub(crate) enum AcquireOutcome {
    Acquired(InstanceOwner),
    Existing(InstanceSnapshot),
}

pub(crate) struct InstanceOwner {
    lock_file: Option<File>,
    metadata_path: PathBuf,
    runtime_status: Arc<StdMutex<RuntimeControlStatus>>,
    show_requests: Option<mpsc::Receiver<()>>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    control_task: Option<JoinHandle<()>>,
}

impl InstanceOwner {
    pub(crate) async fn acquire(state_dir: &Path, mode: LaunchMode) -> Result<AcquireOutcome> {
        fs::create_dir_all(state_dir)
            .with_context(|| format!("create runtime state directory {}", state_dir.display()))?;
        let lock_path = state_dir.join(LOCK_FILE_NAME);
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("open runtime lock {}", lock_path.display()))?;

        match lock_file.try_lock_exclusive() {
            Ok(()) => {}
            Err(error) if is_lock_contended(&error) => {
                let snapshot = discover_existing_owner(state_dir).await?;
                return Ok(AcquireOutcome::Existing(snapshot));
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("lock runtime lease {}", lock_path.display()));
            }
        }

        let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .await
            .context("bind instance control listener")?;
        let control_addr = listener
            .local_addr()
            .context("read instance control address")?;
        let mut token_bytes = [0_u8; 32];
        rand::rngs::SysRng
            .try_fill_bytes(&mut token_bytes)
            .context("generate instance control token")?;
        let metadata = InstanceMetadata {
            pid: std::process::id(),
            mode,
            version: env!("CARGO_PKG_VERSION").to_string(),
            started_at_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            control_addr,
            control_token: hex::encode(token_bytes),
        };
        let metadata_path = state_dir.join(METADATA_FILE_NAME);
        write_metadata_atomically(&metadata_path, &metadata)?;

        let runtime_status = Arc::new(StdMutex::new(RuntimeControlStatus {
            lifecycle: "created".to_string(),
            readiness: "starting".to_string(),
            ..RuntimeControlStatus::default()
        }));
        let (show_tx, show_rx) = mpsc::channel(4);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let control_task = crate::spawn_logged(
            "instance control listener",
            run_control_listener(
                listener,
                metadata.clone(),
                runtime_status.clone(),
                show_tx,
                shutdown_rx,
            ),
        );

        Ok(AcquireOutcome::Acquired(Self {
            lock_file: Some(lock_file),
            metadata_path,
            runtime_status,
            show_requests: Some(show_rx),
            shutdown_tx: Some(shutdown_tx),
            control_task: Some(control_task),
        }))
    }

    pub(crate) fn update_runtime_status(&self, status: RuntimeControlStatus) {
        if let Ok(mut current) = self.runtime_status.lock() {
            *current = status;
        }
    }

    pub(crate) fn take_show_requests(&mut self) -> Option<mpsc::Receiver<()>> {
        self.show_requests.take()
    }

    pub(crate) async fn shutdown(&mut self) {
        if let Some(shutdown) = self.shutdown_tx.take() {
            let _ = shutdown.send(());
        }
        if let Some(mut task) = self.control_task.take() {
            if tokio::time::timeout(Duration::from_millis(500), &mut task)
                .await
                .is_err()
            {
                task.abort();
                let _ = task.await;
            }
        }
        self.remove_metadata_and_unlock();
    }

    fn remove_metadata_and_unlock(&mut self) {
        let _ = fs::remove_file(&self.metadata_path);
        if let Some(lock_file) = self.lock_file.take() {
            let _ = FileExt::unlock(&lock_file);
        }
    }
}

impl Drop for InstanceOwner {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown_tx.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.control_task.take() {
            task.abort();
        }
        self.remove_metadata_and_unlock();
    }
}

pub(crate) fn state_dir_for_config(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("state")
}

#[cfg(test)]
pub(crate) async fn inspect_active_runtime(state_dir: &Path) -> Result<Option<InstanceSnapshot>> {
    fs::create_dir_all(state_dir)
        .with_context(|| format!("create runtime state directory {}", state_dir.display()))?;
    let lock_path = state_dir.join(LOCK_FILE_NAME);
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("open runtime lock {}", lock_path.display()))?;
    match lock_file.try_lock_exclusive() {
        Ok(()) => {
            let _ = FileExt::unlock(&lock_file);
            Ok(None)
        }
        Err(error) if is_lock_contended(&error) => {
            discover_existing_owner(state_dir).await.map(Some)
        }
        Err(error) => Err(error).context("inspect runtime lease"),
    }
}

fn is_lock_contended(error: &std::io::Error) -> bool {
    if error.kind() == ErrorKind::WouldBlock {
        return true;
    }
    #[cfg(target_os = "windows")]
    {
        // LockFileEx reports ERROR_LOCK_VIOLATION (33) for a non-blocking
        // conflict; some filesystems surface ERROR_SHARING_VIOLATION (32).
        return matches!(error.raw_os_error(), Some(32 | 33));
    }
    #[cfg(not(target_os = "windows"))]
    false
}

pub(crate) async fn send_show(state_dir: &Path) -> Result<InstanceSnapshot> {
    let response = send_control_from_state_dir(state_dir, "show").await?;
    if !response.ok {
        return Err(anyhow!(
            "active runtime rejected show request: {}",
            response
                .error
                .unwrap_or_else(|| "unknown error".to_string())
        ));
    }
    if !response.show_accepted {
        return Err(anyhow!("active runtime has no desktop window"));
    }
    response
        .instance
        .ok_or_else(|| anyhow!("active runtime returned no instance status"))
}

async fn discover_existing_owner(state_dir: &Path) -> Result<InstanceSnapshot> {
    let mut last_error = None;
    for attempt in 0..OWNER_DISCOVERY_ATTEMPTS {
        match send_control_from_state_dir(state_dir, "status").await {
            Ok(response) if response.ok => {
                if let Some(instance) = response.instance {
                    return Ok(instance);
                }
                last_error = Some(anyhow!("instance control returned no status"));
            }
            Ok(response) => {
                last_error = Some(anyhow!(
                    "instance control rejected status: {}",
                    response
                        .error
                        .unwrap_or_else(|| "unknown error".to_string())
                ));
            }
            Err(error) => last_error = Some(error),
        }
        if attempt + 1 < OWNER_DISCOVERY_ATTEMPTS {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    let metadata = read_metadata(&state_dir.join(METADATA_FILE_NAME)).with_context(|| {
        format!(
            "runtime lease is held but owner metadata/control is unavailable: {}",
            last_error
                .map(|error| format!("{error:#}"))
                .unwrap_or_else(|| "unknown control error".to_string())
        )
    })?;
    Ok(metadata.snapshot(false, RuntimeControlStatus::default()))
}

async fn send_control_from_state_dir(state_dir: &Path, command: &str) -> Result<ControlResponse> {
    let metadata = read_metadata(&state_dir.join(METADATA_FILE_NAME))?;
    if !metadata.control_addr.ip().is_loopback() {
        return Err(anyhow!("instance control address is not loopback"));
    }
    let stream = tokio::time::timeout(CONTROL_TIMEOUT, TcpStream::connect(metadata.control_addr))
        .await
        .context("instance control connect timed out")?
        .context("connect to instance control")?;
    let (reader, mut writer) = stream.into_split();
    let request = serde_json::to_vec(&ControlRequest {
        token: metadata.control_token,
        command: command.to_string(),
    })?;
    tokio::time::timeout(CONTROL_TIMEOUT, async {
        writer.write_all(&request).await?;
        writer.write_all(b"\n").await?;
        writer.shutdown().await
    })
    .await
    .context("instance control write timed out")??;
    let mut line = String::new();
    let mut limited = BufReader::new(reader).take(CONTROL_MAX_LINE_BYTES + 1);
    tokio::time::timeout(CONTROL_TIMEOUT, limited.read_line(&mut line))
        .await
        .context("instance control read timed out")??;
    if line.len() as u64 > CONTROL_MAX_LINE_BYTES {
        return Err(anyhow!("instance control response is too large"));
    }
    serde_json::from_str(line.trim_end()).context("decode instance control response")
}

async fn run_control_listener(
    listener: TcpListener,
    metadata: InstanceMetadata,
    runtime_status: Arc<StdMutex<RuntimeControlStatus>>,
    show_tx: mpsc::Sender<()>,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    loop {
        let accepted = tokio::select! {
            _ = &mut shutdown_rx => break,
            accepted = listener.accept() => accepted,
        };
        let Ok((stream, peer)) = accepted else {
            break;
        };
        if !peer.ip().is_loopback() {
            continue;
        }
        let metadata = metadata.clone();
        let runtime_status = runtime_status.clone();
        let show_tx = show_tx.clone();
        crate::spawn_logged("instance control connection", async move {
            let _ = handle_control_connection(stream, metadata, runtime_status, show_tx).await;
        });
    }
}

async fn handle_control_connection(
    stream: TcpStream,
    metadata: InstanceMetadata,
    runtime_status: Arc<StdMutex<RuntimeControlStatus>>,
    show_tx: mpsc::Sender<()>,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut line = String::new();
    let mut limited = BufReader::new(reader).take(CONTROL_MAX_LINE_BYTES + 1);
    tokio::time::timeout(CONTROL_TIMEOUT, limited.read_line(&mut line))
        .await
        .context("control request timed out")??;
    if line.len() as u64 > CONTROL_MAX_LINE_BYTES {
        return Err(anyhow!("control request is too large"));
    }
    let request: ControlRequest = serde_json::from_str(line.trim_end())?;
    let runtime = runtime_status
        .lock()
        .map(|status| status.clone())
        .unwrap_or_default();
    let instance = metadata.snapshot(true, runtime);
    let response = if request.token != metadata.control_token {
        ControlResponse {
            ok: false,
            error: Some("unauthorized".to_string()),
            instance: None,
            show_accepted: false,
        }
    } else {
        match request.command.as_str() {
            "status" => ControlResponse {
                ok: true,
                error: None,
                instance: Some(instance),
                show_accepted: false,
            },
            "show" => ControlResponse {
                ok: true,
                error: None,
                instance: Some(instance),
                show_accepted: show_tx.try_send(()).is_ok(),
            },
            _ => ControlResponse {
                ok: false,
                error: Some("unknown command".to_string()),
                instance: Some(instance),
                show_accepted: false,
            },
        }
    };
    let encoded = serde_json::to_vec(&response)?;
    tokio::time::timeout(CONTROL_TIMEOUT, async {
        writer.write_all(&encoded).await?;
        writer.write_all(b"\n").await?;
        writer.shutdown().await
    })
    .await
    .context("control response timed out")??;
    Ok(())
}

fn read_metadata(path: &Path) -> Result<InstanceMetadata> {
    let bytes =
        fs::read(path).with_context(|| format!("read runtime metadata {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("decode runtime metadata {}", path.display()))
}

fn write_metadata_atomically(path: &Path, metadata: &InstanceMetadata) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("runtime metadata path has no parent"))?;
    fs::create_dir_all(parent)?;
    let temp_path = parent.join(format!(".{METADATA_FILE_NAME}.{}.tmp", std::process::id()));
    let encoded = serde_json::to_vec_pretty(metadata)?;
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temp_path)
        .with_context(|| format!("create runtime metadata {}", temp_path.display()))?;
    use std::io::Write;
    file.write_all(&encoded)?;
    file.sync_all()?;
    drop(file);
    #[cfg(target_os = "windows")]
    if path.exists() {
        fs::remove_file(path)
            .with_context(|| format!("replace stale runtime metadata {}", path.display()))?;
    }
    fs::rename(&temp_path, path)
        .with_context(|| format!("publish runtime metadata {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn one_owner_is_reported_and_can_receive_show() {
        let temp = tempfile::tempdir().unwrap();
        let mut owner = match InstanceOwner::acquire(temp.path(), LaunchMode::Tray)
            .await
            .unwrap()
        {
            AcquireOutcome::Acquired(owner) => owner,
            AcquireOutcome::Existing(_) => panic!("unexpected existing owner"),
        };
        owner.update_runtime_status(RuntimeControlStatus {
            lifecycle: "running".to_string(),
            proxy_running: true,
            supplier_running: false,
            readiness: "degraded".to_string(),
        });
        let mut show_requests = owner.take_show_requests().unwrap();

        let inspected = inspect_active_runtime(temp.path()).await.unwrap().unwrap();
        assert_eq!(inspected.mode, LaunchMode::Tray);
        assert!(inspected.reachable);
        assert!(inspected.runtime.proxy_running);

        let shown = send_show(temp.path()).await.unwrap();
        assert_eq!(shown.pid, std::process::id());
        tokio::time::timeout(Duration::from_secs(1), show_requests.recv())
            .await
            .unwrap()
            .unwrap();

        match InstanceOwner::acquire(temp.path(), LaunchMode::Desktop)
            .await
            .unwrap()
        {
            AcquireOutcome::Existing(snapshot) => assert_eq!(snapshot.pid, std::process::id()),
            AcquireOutcome::Acquired(_) => panic!("second owner acquired the lease"),
        }

        owner.shutdown().await;
        assert!(inspect_active_runtime(temp.path()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn stale_metadata_does_not_claim_ownership() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(METADATA_FILE_NAME);
        fs::write(&path, b"stale").unwrap();
        assert!(inspect_active_runtime(temp.path()).await.unwrap().is_none());
        let mut owner = match InstanceOwner::acquire(temp.path(), LaunchMode::Desktop)
            .await
            .unwrap()
        {
            AcquireOutcome::Acquired(owner) => owner,
            AcquireOutcome::Existing(_) => panic!("stale metadata claimed ownership"),
        };
        owner.shutdown().await;
    }
}
