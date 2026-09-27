use anyhow::{Context, Result};
use flexi_logger::{
    Age, Cleanup, Criterion, DeferredNow, Duplicate, FileSpec, Logger, LoggerHandle, Naming,
    WriteMode,
    filter::{LogLineFilter, LogLineWriter},
    writers::FileLogWriter,
};
use log::Record;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

const LOG_RETENTION_DAYS: usize = 3;
const LOG_HISTORY_MAX_FILES: usize = 8;
const LOG_CLEANUP_INTERVAL: Duration = Duration::from_secs(5 * 60);
const RUNTIME_LOG_FILE_BYTES: u64 = 8 * 1024 * 1024;
const RAW_LOG_FILE_BYTES: u64 = 32 * 1024 * 1024;
const LOG_TIMESTAMP_FORMAT: &str = "%Y-%m-%d %H:%M:%S%.3f %:z";
// Development and release use the same quiet default. Request summaries and real failures stay
// visible; per-field, frame and timing diagnostics must not flood an ordinary development run.
const DEFAULT_LOG_SPEC: &str =
    "info,h2=warn,hyper=warn,hyper_util=warn,reqwest=warn,quinn=warn,quinn_proto=warn";

static LOGGER_HANDLE: OnceLock<LoggerHandle> = OnceLock::new();

#[derive(Debug)]
struct HistoricalLogFile {
    path: PathBuf,
    modified: SystemTime,
}

fn managed_log_file_is_current(name: &str) -> Option<bool> {
    let suffix = name
        .strip_prefix("client_r")
        .or_else(|| name.strip_prefix("raw_r"))?;
    if suffix == "CURRENT.log" {
        return Some(true);
    }
    (!suffix.is_empty() && suffix.ends_with(".log")).then_some(false)
}

fn remove_log_file(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn cleanup_log_directory_with_limits(
    directory: &Path,
    now: SystemTime,
    retention: Duration,
    max_history_files: usize,
) -> std::io::Result<()> {
    let mut history = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(is_current) = name.to_str().and_then(managed_log_file_is_current) else {
            continue;
        };
        let metadata = match entry.metadata() {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if is_current {
            continue;
        }
        history.push(HistoricalLogFile {
            path: entry.path(),
            modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        });
    }

    let mut retained = Vec::with_capacity(history.len());
    for file in history {
        let expired = now
            .duration_since(file.modified)
            .is_ok_and(|age| age > retention);
        if expired {
            remove_log_file(&file.path)?;
        } else {
            retained.push(file);
        }
    }

    retained.sort_by(|left, right| {
        left.modified
            .cmp(&right.modified)
            .then_with(|| left.path.cmp(&right.path))
    });
    let remove_count = retained.len().saturating_sub(max_history_files);
    for file in retained.into_iter().take(remove_count) {
        remove_log_file(&file.path)?;
    }
    Ok(())
}

fn cleanup_log_directory(directory: &Path) -> std::io::Result<()> {
    cleanup_log_directory_with_limits(
        directory,
        SystemTime::now(),
        Duration::from_secs(LOG_RETENTION_DAYS as u64 * 24 * 60 * 60),
        LOG_HISTORY_MAX_FILES,
    )
}

fn start_log_cleanup_worker(directory: PathBuf) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("const-api-log-cleanup".to_string())
        .spawn(move || {
            loop {
                std::thread::sleep(LOG_CLEANUP_INTERVAL);
                if let Err(error) = cleanup_log_directory(&directory) {
                    log::warn!("client log cleanup failed: {error}");
                }
            }
        })?;
    Ok(())
}

struct ClientLogFilter;

fn is_benign_incomplete_local_http_message(record: &Record<'_>) -> bool {
    if record.level() != log::Level::Error {
        return false;
    }
    let is_warp_server =
        record.target() == "warp::server::run" || record.module_path() == Some("warp::server::run");
    is_warp_server
        && record.args().to_string() == "server connection error: hyper::Error(IncompleteMessage)"
}

impl LogLineFilter for ClientLogFilter {
    fn write(
        &self,
        now: &mut DeferredNow,
        record: &Record<'_>,
        log_line_writer: &dyn LogLineWriter,
    ) -> std::io::Result<()> {
        if !is_benign_incomplete_local_http_message(record) {
            log_line_writer.write(now, record)?;
        }
        Ok(())
    }
}

fn client_log_format(
    writer: &mut dyn Write,
    now: &mut DeferredNow,
    record: &Record<'_>,
) -> std::io::Result<()> {
    write!(
        writer,
        "[{}] {} [{}] {}",
        now.format(LOG_TIMESTAMP_FORMAT),
        record.level(),
        record.module_path().unwrap_or("<unnamed>"),
        record.args()
    )
}

pub(crate) fn initialize_client_logging(terminal_output: bool, config_root: &Path) -> Result<()> {
    if LOGGER_HANDLE.get().is_some() {
        return Ok(());
    }
    let directory = config_root.join("logs");
    fs::create_dir_all(&directory).context("create client log directory")?;
    let initial_cleanup_error = cleanup_log_directory(&directory).err();
    let raw_writer = FileLogWriter::builder(
        FileSpec::default()
            .directory(&directory)
            .basename("raw")
            .suppress_timestamp(),
    )
    .append()
    .rotate(
        Criterion::AgeOrSize(Age::Day, RAW_LOG_FILE_BYTES),
        Naming::Timestamps,
        Cleanup::KeepForDays(LOG_RETENTION_DAYS),
    )
    .write_mode(WriteMode::Async)
    .try_build()
    .context("create raw log writer")?;

    let duplicate = if terminal_output {
        Duplicate::Info
    } else if cfg!(debug_assertions) {
        Duplicate::All
    } else {
        Duplicate::None
    };
    let handle = Logger::try_with_str(DEFAULT_LOG_SPEC)
        .context("configure client logger")?
        .format_for_files(client_log_format)
        .format_for_stderr(client_log_format)
        .log_to_file(
            FileSpec::default()
                .directory(&directory)
                .basename("client")
                .suppress_timestamp(),
        )
        .append()
        .rotate(
            Criterion::AgeOrSize(Age::Day, RUNTIME_LOG_FILE_BYTES),
            Naming::Timestamps,
            Cleanup::KeepForDays(LOG_RETENTION_DAYS),
        )
        .write_mode(WriteMode::Async)
        .duplicate_to_stderr(duplicate)
        .add_writer("Raw", Box::new(raw_writer))
        .filter(Box::new(ClientLogFilter))
        .start()
        .context("start client logger")?;
    let _ = LOGGER_HANDLE.set(handle);
    if let Some(error) = initial_cleanup_error {
        log::warn!("initial client log cleanup failed: {error}");
    }
    if let Err(error) = start_log_cleanup_worker(directory) {
        log::warn!("start client log cleanup worker failed: {error}");
    }
    Ok(())
}

pub(crate) fn write_raw_log(record: &serde_json::Value) -> Result<()> {
    let serialized = serde_json::to_string(record)?;
    log::info!(target: "{Raw}", "{serialized}");
    Ok(())
}

pub(crate) fn flush_client_logging() {
    if let Some(handle) = LOGGER_HANDLE.get() {
        handle.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flexi_logger::LogSpecification;

    #[test]
    fn client_log_format_includes_local_timestamp_with_milliseconds_and_offset() {
        let record = log::Record::builder()
            .args(format_args!("hello"))
            .level(log::Level::Info)
            .module_path(Some("const_api_client::proxy"))
            .build();
        let mut output = Vec::new();

        client_log_format(&mut output, &mut DeferredNow::new(), &record)
            .expect("format log record");

        let line = String::from_utf8(output).expect("utf-8 log line");
        let (timestamp, message) = line
            .strip_prefix('[')
            .and_then(|line| line.split_once("] "))
            .expect("timestamp prefix");
        assert_eq!(timestamp.len(), 30, "{timestamp}");
        assert_eq!(&timestamp[4..5], "-");
        assert_eq!(&timestamp[7..8], "-");
        assert_eq!(&timestamp[10..11], " ");
        assert_eq!(&timestamp[19..20], ".");
        assert_eq!(&timestamp[23..24], " ");
        assert!(matches!(&timestamp[24..25], "+" | "-"), "{timestamp}");
        assert_eq!(&timestamp[27..28], ":");
        assert_eq!(message, "INFO [const_api_client::proxy] hello");
    }

    #[test]
    fn client_log_spec_keeps_app_diagnostics_without_transport_frame_noise() {
        let noisy_dependency_targets = [
            "h2::codec::framed_read",
            "h2::codec::framed_write",
            "hyper::proto::h1::io",
            "hyper_util::client::legacy::pool",
            "reqwest::connect",
            "quinn::connection",
            "quinn_proto::connection",
        ];
        let parsed = LogSpecification::parse(DEFAULT_LOG_SPEC).expect("valid client log spec");
        for target in noisy_dependency_targets {
            assert!(
                !parsed.enabled(log::Level::Debug, target),
                "target={target}"
            );
            assert!(!parsed.enabled(log::Level::Info, target), "target={target}");
            assert!(parsed.enabled(log::Level::Warn, target), "target={target}");
        }
        for target in ["const_api_client::supplier", "const_api_client::proxy"] {
            assert!(!parsed.enabled(log::Level::Trace, target));
            assert!(!parsed.enabled(log::Level::Debug, target));
            assert!(parsed.enabled(log::Level::Info, target));
            assert!(parsed.enabled(log::Level::Warn, target));
            assert!(parsed.enabled(log::Level::Error, target));
        }
    }

    #[test]
    fn client_log_filter_only_suppresses_warp_incomplete_message_disconnects() {
        let benign = log::Record::builder()
            .args(format_args!(
                "server connection error: hyper::Error(IncompleteMessage)"
            ))
            .level(log::Level::Error)
            .target("warp::server::run")
            .module_path(Some("warp::server::run"))
            .build();
        assert!(is_benign_incomplete_local_http_message(&benign));

        let other_warp_error = log::Record::builder()
            .args(format_args!("server connection error: address in use"))
            .level(log::Level::Error)
            .target("warp::server::run")
            .module_path(Some("warp::server::run"))
            .build();
        assert!(!is_benign_incomplete_local_http_message(&other_warp_error));

        let other_module = log::Record::builder()
            .args(format_args!(
                "server connection error: hyper::Error(IncompleteMessage)"
            ))
            .level(log::Level::Error)
            .target("const_api_client::proxy")
            .module_path(Some("const_api_client::proxy"))
            .build();
        assert!(!is_benign_incomplete_local_http_message(&other_module));
    }

    #[test]
    fn log_cleanup_keeps_current_files_and_removes_expired_history() {
        let directory = tempfile::tempdir().expect("temp log directory");
        fs::write(directory.path().join("client_rCURRENT.log"), b"current")
            .expect("write current log");
        fs::write(directory.path().join("raw_rCURRENT.log"), b"raw-current")
            .expect("write raw current log");
        fs::write(
            directory.path().join("client_r2026-08-01_00-00-00.log"),
            b"old",
        )
        .expect("write historical log");
        fs::write(directory.path().join("unrelated.log"), b"unrelated")
            .expect("write unrelated file");

        cleanup_log_directory_with_limits(
            directory.path(),
            SystemTime::now() + Duration::from_secs(4 * 24 * 60 * 60),
            Duration::from_secs(3 * 24 * 60 * 60),
            32,
        )
        .expect("clean logs");

        assert!(directory.path().join("client_rCURRENT.log").exists());
        assert!(directory.path().join("raw_rCURRENT.log").exists());
        assert!(
            !directory
                .path()
                .join("client_r2026-08-01_00-00-00.log")
                .exists()
        );
        assert!(directory.path().join("unrelated.log").exists());
    }

    #[test]
    fn log_cleanup_enforces_history_count() {
        let directory = tempfile::tempdir().expect("temp log directory");
        fs::write(directory.path().join("client_rCURRENT.log"), b"1234")
            .expect("write current log");
        for index in 0..4 {
            fs::write(
                directory
                    .path()
                    .join(format!("client_r2026-08-01_00-00-0{index}.log")),
                [b'0' + index; 4],
            )
            .expect("write historical log");
        }

        cleanup_log_directory_with_limits(
            directory.path(),
            SystemTime::now(),
            Duration::from_secs(24 * 60 * 60),
            2,
        )
        .expect("clean logs");

        let history = fs::read_dir(directory.path())
            .expect("read log directory")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| managed_log_file_is_current(name) == Some(false))
            })
            .count();
        assert_eq!(history, 2);
        assert!(directory.path().join("client_rCURRENT.log").exists());
    }
}
