use crate::LOCAL_PLACEHOLDER_KEY;
use crate::model::*;
use crate::supplier::now_unix;
use crate::tool_model_metadata::ToolModelInfo;
#[cfg(test)]
use crate::tool_model_metadata::tool_models_from_ids;
use anyhow::{Context, Result, anyhow};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use rand::{TryRng, rngs::SysRng};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    ffi::OsStr,
    fs,
    io::{BufRead, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc, Condvar, Mutex, MutexGuard, OnceLock, TryLockError,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use toml_edit::{DocumentMut, Item, Table, value as toml_value};

pub(crate) const CODEX_CONST_API_PROVIDER_ID: &str = "CONST_API";
const LEGACY_CONST_API_PROVIDER_ID: &str = "const_api";
pub(crate) const CODEX_LEGACY_PROVIDER_ID: &str = "custom";
#[cfg(test)]
pub(crate) const TOOL_CONFIG_ROOT_URL: &str = "http://127.0.0.1:38787";
#[cfg(test)]
pub(crate) const TOOL_CONFIG_OPENAI_BASE_URL: &str = "http://127.0.0.1:38787/v1";
#[cfg(test)]
pub(crate) const TOOL_CONFIG_ANTHROPIC_BASE_URL: &str = "http://127.0.0.1:38787/anthropic";
const HERMES_CONST_API_PROVIDER_ID: &str = "custom:CONST_API";
const HERMES_LEGACY_CONST_API_PROVIDER_ID: &str = "custom:const_api";
const CONST_API_DISPLAY_NAME: &str = "CONST API";
const CONST_API_LEGACY_DISPLAY_NAME: &str = "Const API";
const CODEX_SESSION_SYNC_MANIFEST: &str = "codex-session-sync.json";
const TOOL_CONFIG_MANIFEST: &str = "tool-config-manifest.json";
// Configuration mutations share one shutdown policy: immediately force-kill
// each revalidated app process tree, then poll for up to five seconds before
// asking the user whether to cancel or continue.
const TOOL_PROCESS_CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(not(target_os = "windows"))]
const TOOL_PROCESS_FORCE_KILL_TIMEOUT: Duration = Duration::from_secs(5);
const TOOL_PROCESS_START_TIMEOUT: Duration = Duration::from_secs(5);
const TOOL_PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(200);
// One logical full-system process snapshot is allowed per user operation. Unix may collect
// identity and command-line ownership in two bounded `ps` passes; Windows reads both from the
// same native process handle. A missing process proceeds immediately.
const TOOL_PROCESS_SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(10);
// Non-snapshot helpers retain a little more room for discovery and launch.
const TOOL_HELPER_COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
const TOOL_DISCOVERY_HELPER_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(not(target_os = "windows"))]
const TOOL_HELPER_TREE_TERM_GRACE: Duration = Duration::from_millis(150);
const TOOL_HELPER_TREE_KILL_TIMEOUT: Duration = Duration::from_secs(3);
const TOOL_HELPER_READER_TIMEOUT: Duration = Duration::from_secs(3);
const TOOL_CONFIG_CANCEL_SETTLE_GRACE: Duration = Duration::from_secs(12);
const TOOL_CONFIG_PROGRESS_WAIT_MAX: Duration = Duration::from_secs(15);
pub(crate) const TOOL_CONFIG_MODEL_REFRESH_TIMEOUT: Duration = Duration::from_secs(15);
pub(crate) const TOOL_CONFIG_OPTIONAL_MODEL_REFRESH_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const TOOL_CONFIG_MODEL_REFRESH_USE_EXISTING: &str =
    "TOOL_CONFIG_MODEL_REFRESH_USE_EXISTING";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolModelSyncPolicy {
    None,
    Selected,
    Catalog,
}

impl ToolModelSyncPolicy {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Selected => "selected",
            Self::Catalog => "catalog",
        }
    }
}
const COMMAND_START_STABILITY_WINDOW: Duration = Duration::from_millis(750);
// Leave enough time to read and act on the confirmation dialog. The token is
// still one-time, operation-bound, and its process snapshot is revalidated
// before any running tool is stopped.
const TOOL_CONFIG_CONFIRMATION_TTL: Duration = Duration::from_secs(5 * 60);
// Configuration operations have no global deadline. Individual process and
// helper stages remain bounded by their own local timeouts.
const TOOL_CONFIG_OPERATION_RETENTION: Duration = Duration::from_secs(60);
const CONFIG_BACKUP_RETENTION: usize = 5;
const DATABASE_BACKUP_RETENTION: usize = 2;

include!("tool_config/production/core.rs");
include!("tool_config/production/catalog.rs");
#[cfg(target_os = "windows")]
include!("tool_config/production/windows_process.rs");
#[cfg(target_os = "windows")]
include!("tool_config/production/windows_installed_programs.rs");
include!("tool_config/production/storage.rs");
include!("tool_config/production/codex.rs");
include!("tool_config/production/codex_catalog.rs");
include!("tool_config/production/tool_writers.rs");
include!("tool_config/production/additional_tools.rs");
include!("tool_config/production/deepseek_harness_settings.rs");
include!("tool_config/production/open_interpreter.rs");
include!("tool_config/production/anythingllm.rs");
include!("tool_config/production/grok_build.rs");
include!("tool_config/production/minimax_code.rs");
#[path = "tool_config/trae.rs"]
mod trae;
pub(crate) use trae::{check_trae_config, prepare_trae_config};
#[path = "tool_config/copilot_desktop.rs"]
mod copilot_desktop;
include!("tool_config/production/operation.rs");
include!("tool_config/production/program_runtime.rs");
include!("tool_config/production/formats.rs");

#[cfg(test)]
mod tests {
    include!("tool_config/tests/config_files.rs");
    include!("tool_config/tests/deepseek_harness_settings.rs");
    include!("tool_config/tests/runtime_guard.rs");
    include!("tool_config/tests/operation_lifecycle.rs");
    include!("tool_config/tests/codex_sessions.rs");
    include!("tool_config/tests/codex_indexed_sessions.rs");
    include!("tool_config/tests/codex_catalog.rs");
    include!("tool_config/tests/program_launch.rs");
    include!("tool_config/tests/open_interpreter.rs");
    include!("tool_config/tests/anythingllm.rs");
    include!("tool_config/tests/integration_contracts.rs");
    include!("tool_config/tests/model_protocols.rs");
    include!("tool_config/tests/model_capabilities.rs");
    include!("tool_config/tests/model_order.rs");
    include!("tool_config/tests/grok_minimax.rs");
}
