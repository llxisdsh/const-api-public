#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ToolProcessIdentity {
    pid: u32,
    executable_path: String,
    start_identity: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ToolProcessScope {
    executable_names: BTreeSet<String>,
    exact_executable_paths: BTreeSet<String>,
    install_roots: BTreeSet<String>,
    package_family_names: BTreeSet<String>,
    command_line_markers: BTreeSet<String>,
    allow_name_fallback: bool,
}

impl ToolProcessScope {
    fn has_concrete_path_owner(&self) -> bool {
        !self.exact_executable_paths.is_empty()
            || !self.install_roots.is_empty()
            || !self.package_family_names.is_empty()
    }

    fn has_concrete_owner(&self) -> bool {
        self.has_concrete_path_owner() || !self.command_line_markers.is_empty()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ToolProcessSnapshot {
    processes: Vec<ToolProcessIdentity>,
    parent_process_ids: BTreeMap<u32, u32>,
    scope: ToolProcessScope,
}

#[derive(Debug)]
struct HelperCommandOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    // Windows production probes currently need only status/stdout; Unix and
    // tests retain stderr for actionable process and launch diagnostics.
    #[cfg_attr(all(target_os = "windows", not(test)), allow(dead_code))]
    stderr: Vec<u8>,
}

struct HelperOutputReader {
    result: std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>,
    thread: thread::JoinHandle<()>,
}

impl ToolProcessSnapshot {
    #[cfg(any(test, not(target_os = "windows")))]
    fn new(mut processes: Vec<ToolProcessIdentity>) -> Self {
        processes.sort();
        processes.dedup();
        Self {
            processes,
            ..Default::default()
        }
    }

    #[cfg(target_os = "windows")]
    fn scoped(
        mut processes: Vec<ToolProcessIdentity>,
        parent_process_ids: BTreeMap<u32, u32>,
        scope: ToolProcessScope,
    ) -> Self {
        processes.sort();
        processes.dedup();
        Self {
            processes,
            parent_process_ids,
            scope,
        }
    }

    fn running(&self) -> bool {
        !self.processes.is_empty()
    }

    #[cfg(target_os = "windows")]
    fn process_ids(&self) -> BTreeSet<u32> {
        self.processes.iter().map(|process| process.pid).collect()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ToolProtocol {
    OpenAiResponses,
    OpenAiChat,
    AnthropicMessages,
    GeminiNative,
}

pub(crate) const CLAUDE_MODEL_FOLLOW_MAIN: &str = "__const_follow_main__";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct ClaudeModelSettings {
    #[serde(default)]
    pub(crate) main: String,
    #[serde(default = "default_claude_role_model")]
    pub(crate) opus: String,
    #[serde(default = "default_claude_role_model")]
    pub(crate) sonnet: String,
    #[serde(default = "default_claude_role_model")]
    pub(crate) haiku: String,
}

fn default_claude_role_model() -> String {
    CLAUDE_MODEL_FOLLOW_MAIN.to_string()
}

impl Default for ClaudeModelSettings {
    fn default() -> Self {
        Self {
            main: String::new(),
            opus: default_claude_role_model(),
            sonnet: default_claude_role_model(),
            haiku: default_claude_role_model(),
        }
    }
}

#[derive(Debug)]
pub(crate) enum ToolConfigWriteOutcome {
    ConfirmationRequired {
        running: bool,
        confirmation_token: String,
    },
    ManualCloseRequired {
        running: bool,
        confirmation_token: String,
        reason: String,
    },
    Completed(ToolApplyResult),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ToolConfigAction {
    Apply,
    Remove,
}

impl ToolConfigAction {
    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            "apply" => Ok(Self::Apply),
            "remove" => Ok(Self::Remove),
            _ => Err(anyhow!("TOOL_CONFIG_INVALID_ACTION: {value}")),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Apply => "apply",
            Self::Remove => "remove",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ToolConfigOperationTicket {
    pub(crate) operation_id: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ToolConfigCancelAck {
    pub(crate) operation_id: String,
    pub(crate) state: String,
    pub(crate) cancellation_accepted: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ToolConfigOperationStatus {
    pub(crate) operation_id: String,
    pub(crate) state: String,
    pub(crate) terminal: bool,
    pub(crate) cancellation_requested: bool,
    pub(crate) progress_version: u64,
    pub(crate) progress_stage: String,
    pub(crate) progress_completed: Option<u64>,
    pub(crate) progress_total: Option<u64>,
    pub(crate) progress_ratio: Option<f64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ToolConfigRemoveMode {
    RestorePreConst,
    NativeRoute,
}

impl ToolConfigRemoveMode {
    pub(crate) fn parse(value: Option<&str>) -> Result<Self> {
        match value.unwrap_or("restore_pre_const") {
            "restore_pre_const" => Ok(Self::RestorePreConst),
            "native_route" => Ok(Self::NativeRoute),
            value => Err(anyhow!("TOOL_CONFIG_INVALID_REMOVE_MODE: {value}")),
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::RestorePreConst => "restore_pre_const",
            Self::NativeRoute => "native_route",
        }
    }

    pub(crate) fn validate_for_tool(self, tool: &str) -> Result<Self> {
        if self == Self::NativeRoute
            && !matches!(tool, "codex" | "claude" | "claude-desktop" | "gemini")
        {
            return Err(anyhow!(
                "TOOL_CONFIG_NATIVE_ROUTE_UNSUPPORTED: {tool} does not have one canonical native route"
            ));
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq)]
struct ToolConfigOperationProgress {
    version: u64,
    stage: String,
    completed: Option<u64>,
    total: Option<u64>,
    ratio: Option<f64>,
}

impl Default for ToolConfigOperationProgress {
    fn default() -> Self {
        Self {
            version: 0,
            stage: "queued".to_string(),
            completed: None,
            total: None,
            ratio: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ToolConfigOperationState {
    Pending,
    Running,
    Committing,
    LaunchStarted,
    CommittedNotStarted,
    Completed,
    Failed,
    Cancelled,
}

impl ToolConfigOperationState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Committing => "committing",
            Self::LaunchStarted => "launch_started",
            Self::CommittedNotStarted => "committed_not_started",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    const fn terminal(self) -> bool {
        matches!(
            self,
            Self::CommittedNotStarted | Self::Completed | Self::Failed | Self::Cancelled
        )
    }
}

pub(crate) struct ToolConfigOperationControl {
    id: String,
    tool: String,
    action: ToolConfigAction,
    restart: bool,
    retain_until: Mutex<Instant>,
    cancelled: AtomicBool,
    state: Mutex<ToolConfigOperationState>,
    progress: Mutex<ToolConfigOperationProgress>,
    state_changed: Condvar,
    cancel_requested: tokio::sync::Notify,
}

impl ToolConfigOperationControl {
    pub(crate) const fn restarts(&self) -> bool {
        self.restart
    }
}

pub(crate) struct ToolConfigLaunchContext<'a> {
    operation: &'a ToolConfigOperationControl,
}

const CLAUDE_CODE_ROUTE_AUTH_ENV_NAMES: &[&str] = &[
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AWS_API_KEY",
    "ANTHROPIC_AWS_BASE_URL",
    "ANTHROPIC_AWS_WORKSPACE_ID",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_BEDROCK_BASE_URL",
    "ANTHROPIC_BEDROCK_MANTLE_BASE_URL",
    "ANTHROPIC_CUSTOM_HEADERS",
    "ANTHROPIC_FEDERATION_RULE_ID",
    "ANTHROPIC_FOUNDRY_API_KEY",
    "ANTHROPIC_FOUNDRY_AUTH_TOKEN",
    "ANTHROPIC_FOUNDRY_BASE_URL",
    "ANTHROPIC_FOUNDRY_RESOURCE",
    "ANTHROPIC_ORGANIZATION_ID",
    "ANTHROPIC_PROFILE",
    "ANTHROPIC_UNIX_SOCKET",
    "ANTHROPIC_VERTEX_BASE_URL",
    "ANTHROPIC_VERTEX_PROJECT_ID",
    "ANTHROPIC_WORKSPACE_ID",
    "AWS_BEARER_TOKEN_BEDROCK",
    "CLAUDE_CODE_API_KEY_HELPER_TTL_MS",
    "CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
    "CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST",
    "CLAUDE_CODE_SKIP_BEDROCK_AUTH",
    "CLAUDE_CODE_SKIP_FOUNDRY_AUTH",
    "CLAUDE_CODE_SKIP_VERTEX_AUTH",
    "CLAUDE_CODE_USE_ANTHROPIC_AWS",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CODE_USE_MANTLE",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CONFIG_DIR",
];
const CLAUDE_CODE_CONFLICTING_ENV_NAMES: &[&str] = CLAUDE_CODE_ROUTE_AUTH_ENV_NAMES;
const CLAUDE_CODE_NATIVE_ROUTE_SETTING_NAMES: &[&str] = &[
    "apiKeyHelper",
    "awsAuthRefresh",
    "awsCredentialExport",
];

const CLAUDE_SCIENCE_CONFLICTING_ENV_NAMES: &[&str] = &["ANTHROPIC_API_KEY"];
const CLAUDE_SCIENCE_EXTERNAL_ENVIRONMENT_NAMES: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_API_KEY",
];

const GEMINI_CLI_CONFLICTING_ENV_NAMES: &[&str] = &[
    "GOOGLE_APPLICATION_CREDENTIALS",
    "GOOGLE_API_KEY",
    "GOOGLE_GENAI_USE_GCA",
    "GOOGLE_GENAI_USE_VERTEXAI",
    "GOOGLE_VERTEX_BASE_URL",
    "GEMINI_CLI_USE_COMPUTE_ADC",
    "GEMINI_CLI_HOME",
];

const CLAUDE_CODE_EXTERNAL_ENVIRONMENT_NAMES: &[&str] =
    CLAUDE_CODE_ROUTE_AUTH_ENV_NAMES;

const GEMINI_CLI_EXTERNAL_ENVIRONMENT_NAMES: &[&str] = &[
    "GOOGLE_GEMINI_BASE_URL",
    "GEMINI_API_KEY",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "GOOGLE_API_KEY",
    "GOOGLE_GENAI_USE_GCA",
    "GOOGLE_GENAI_USE_VERTEXAI",
    "GOOGLE_VERTEX_BASE_URL",
    "GEMINI_CLI_USE_COMPUTE_ADC",
    "GEMINI_CLI_HOME",
];

fn tool_external_environment_names(tool: &str) -> &'static [&'static str] {
    tool_profile(tool)
        .map(|profile| profile.external_environment_names)
        .unwrap_or_default()
}

fn defined_tool_environment_variables_with(
    tool: &str,
    mut is_defined: impl FnMut(&str) -> bool,
) -> Vec<&'static str> {
    tool_external_environment_names(tool)
        .iter()
        .copied()
        .filter(|name| is_defined(name))
        .collect()
}

fn external_launch_environment_warning(defined: &[&str]) -> String {
    format!(
        "CONST API inherited these environment variables: {}. Launching the tool from CONST API overrides required values and removes known conflicts. Launching elsewhere may inherit external values and bypass CONST API or fail authentication. Only variable names are shown.",
        defined.join(", ")
    )
}

pub(crate) fn attach_external_launch_environment_warning(
    result: &mut ToolApplyResult,
    tool: &str,
) {
    let defined =
        defined_tool_environment_variables_with(tool, |name| std::env::var_os(name).is_some());
    if defined.is_empty() {
        return;
    }

    result.details.insert(
        "defined_environment_variables".to_string(),
        defined.join(","),
    );
    result.details.insert(
        "external_launch_warning".to_string(),
        external_launch_environment_warning(&defined),
    );
}

#[derive(Debug)]
struct ToolConfigCommittedNotStarted;

impl std::fmt::Display for ToolConfigCommittedNotStarted {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TOOL_CONFIG_COMMITTED_NOT_STARTED")
    }
}

impl std::error::Error for ToolConfigCommittedNotStarted {}

impl ToolConfigLaunchContext<'_> {
    fn spawn<T>(&self, purpose: &str, spawn: impl FnOnce() -> Result<T>) -> Result<T> {
        let mut state = self
            .operation
            .state
            .lock()
            .map_err(|_| anyhow!("TOOL_CONFIG_OPERATION_STATE_POISONED"))?;
        if *state != ToolConfigOperationState::Committing {
            return Err(anyhow!(
                "TOOL_CONFIG_OPERATION_INVALID_STATE: cannot spawn {purpose} from {}",
                state.as_str()
            ));
        }
        if self.operation.cancelled.load(Ordering::Acquire) {
            *state = ToolConfigOperationState::CommittedNotStarted;
            self.operation.state_changed.notify_all();
            self.operation.cancel_requested.notify_waiters();
            return Err(anyhow!(ToolConfigCommittedNotStarted));
        }
        let spawned = spawn();
        if spawned.is_ok() {
            *state = ToolConfigOperationState::LaunchStarted;
            self.operation.state_changed.notify_all();
        }
        spawned
    }

    fn spawn_command(&self, command: &mut Command, purpose: &str) -> Result<Child> {
        self.spawn(purpose, || {
            command.spawn().with_context(|| format!("launch {purpose}"))
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ToolConfigConfirmationState {
    Pending,
    Consumed,
    Expired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ToolConfigConfirmationMode {
    CloseAndContinue,
    ForceContinue,
}

#[derive(Clone, Debug)]
struct ToolConfigConfirmationCapability {
    tool: String,
    action: ToolConfigAction,
    protocol: Option<String>,
    restart: bool,
    context_digest: String,
    process_snapshot: ToolProcessSnapshot,
    mode: ToolConfigConfirmationMode,
    expires_at: Instant,
    retain_until: Instant,
    state: ToolConfigConfirmationState,
}

#[derive(Default)]
struct ToolConfigLifecycleOwners {
    owners: Mutex<HashMap<String, String>>,
    changed: Condvar,
}

pub(crate) struct ToolConfigOperationLease {
    tool: String,
    operation_id: String,
}

impl Drop for ToolConfigOperationLease {
    fn drop(&mut self) {
        let lifecycle = tool_config_lifecycle_owners();
        let Ok(mut owners) = lifecycle.owners.lock() else {
            return;
        };
        if owners.get(&self.tool) == Some(&self.operation_id) {
            owners.remove(&self.tool);
            lifecycle.changed.notify_all();
        }
    }
}

impl ToolProtocol {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiResponses => "openai_responses",
            Self::OpenAiChat => "openai_chat",
            Self::AnthropicMessages => "anthropic_messages",
            Self::GeminiNative => "gemini_native",
        }
    }

    const fn surface(self) -> &'static str {
        match self {
            Self::OpenAiResponses | Self::OpenAiChat => "v1",
            Self::AnthropicMessages => "anthropic",
            Self::GeminiNative => "gemini",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "openai_responses" => Some(Self::OpenAiResponses),
            "openai_chat" => Some(Self::OpenAiChat),
            "anthropic_messages" => Some(Self::AnthropicMessages),
            "gemini_native" => Some(Self::GeminiNative),
            _ => None,
        }
    }
}

fn default_tool_protocol(tool: &str) -> Result<ToolProtocol> {
    tool_profile(tool)
        .map(|profile| profile.default_protocol)
        .ok_or_else(|| anyhow!("unknown tool: {tool}"))
}

fn tool_supports_protocol(tool: &str, protocol: ToolProtocol) -> bool {
    tool_profile(tool)
        .is_some_and(|profile| profile.protocols.contains(&protocol))
}

pub(crate) fn resolve_tool_protocol(tool: &str, requested: Option<&str>) -> Result<ToolProtocol> {
    let protocol = match requested.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => {
            ToolProtocol::parse(value).ok_or_else(|| anyhow!("unknown tool protocol: {value}"))?
        }
        None => default_tool_protocol(tool)?,
    };
    if !tool_supports_protocol(tool, protocol) {
        return Err(anyhow!(
            "{tool} does not support tool protocol {}",
            protocol.as_str()
        ));
    }
    Ok(protocol)
}

pub(crate) fn attach_tool_protocol(result: &mut ToolApplyResult, protocol: ToolProtocol) {
    result
        .details
        .insert("tool_protocol".to_string(), protocol.as_str().to_string());
}

fn attach_tool_protocol_if_missing(result: &mut ToolApplyResult, protocol: ToolProtocol) {
    result
        .details
        .entry("tool_protocol".to_string())
        .or_insert_with(|| protocol.as_str().to_string());
}

#[derive(Default)]
struct ToolApplyBuilder {
    files: Vec<String>,
    backups: Vec<String>,
    file_statuses: Vec<ToolFileStatus>,
}

// Planning reuses the real configuration builders so every tool follows the
// same decision path. While this synchronous guard is active, shared storage
// primitives observe the intended semantic state without writing anything.
thread_local! {
    static TOOL_CONFIG_PREVIEW_DEPTH: Cell<u32> = const { Cell::new(0) };
}

struct ToolConfigPreviewGuard;

impl ToolConfigPreviewGuard {
    fn enter() -> Self {
        TOOL_CONFIG_PREVIEW_DEPTH.with(|depth| depth.set(depth.get().saturating_add(1)));
        Self
    }
}

impl Drop for ToolConfigPreviewGuard {
    fn drop(&mut self) {
        TOOL_CONFIG_PREVIEW_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

fn tool_config_preview_active() -> bool {
    TOOL_CONFIG_PREVIEW_DEPTH.with(|depth| depth.get() > 0)
}

pub(crate) fn preview_tool_config_apply<T>(apply: impl FnOnce() -> Result<T>) -> Result<T> {
    let _guard = ToolConfigPreviewGuard::enter();
    apply()
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct ToolConfigManifest {
    version: u32,
    #[serde(default)]
    files: HashMap<String, ToolConfigOwnership>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ToolConfigOwnership {
    tool: String,
    #[serde(default)]
    format: ToolConfigFormat,
    #[serde(default)]
    fields: Vec<ToolConfigOwnedField>,
    original_exists: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    original_content_base64: Option<String>,
    original_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    applied_content_base64: Option<String>,
    applied_sha256: String,
    #[serde(default = "default_true")]
    whole_file_restore_safe: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ToolConfigFormat {
    Json,
    Toml,
    Env,
    Yaml,
    #[default]
    Text,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
enum ToolConfigFieldPath {
    Key(String),
    Named(String),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
enum ToolConfigFieldValue {
    Missing,
    Value(serde_json::Value),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
struct ToolConfigOwnedField {
    path: Vec<ToolConfigFieldPath>,
    original: ToolConfigFieldValue,
    applied: ToolConfigFieldValue,
}

const TOML_VALUE_PREFIX: &str = "__const_toml_value__:";

const fn default_true() -> bool {
    true
}

impl ToolApplyBuilder {
    fn finish(self, tool: &str) -> ToolApplyResult {
        let already_configured = !self.file_statuses.is_empty()
            && self
                .file_statuses
                .iter()
                .all(|status| status.already_configured);
        ToolApplyResult {
            tool: tool.to_string(),
            files: self.files,
            backups: self.backups,
            file_statuses: self.file_statuses,
            already_configured,
            details: HashMap::new(),
        }
    }
}

pub(crate) fn home_dir() -> PathBuf {
    if let Some(home) = const_api_test_home() {
        return home;
    }
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn test_home_active() -> bool {
    const_api_test_home().is_some()
}

#[cfg(any(test, debug_assertions))]
fn const_api_test_home() -> Option<PathBuf> {
    #[cfg(test)]
    if let Ok(override_home) = TEST_HOME_OVERRIDE
        .get_or_init(|| Mutex::new(None))
        .lock()
    {
        if override_home.is_some() {
            return override_home.clone();
        }
    }
    std::env::var_os("CONST_API_TEST_HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

#[cfg(test)]
static TEST_HOME_OVERRIDE: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

#[cfg(test)]
pub(crate) fn set_test_home_override(override_home: Option<PathBuf>) -> Option<PathBuf> {
    std::mem::replace(
        &mut *TEST_HOME_OVERRIDE
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
        override_home,
    )
}

#[cfg(not(any(test, debug_assertions)))]
fn const_api_test_home() -> Option<PathBuf> {
    None
}

#[cfg(target_os = "windows")]
fn windows_hidden_console_flag() -> u32 {
    0x08000000
}

#[cfg(target_os = "windows")]
fn windows_visible_terminal_flag() -> u32 {
    0x00000010
}

#[cfg(target_os = "windows")]
fn helper_command(program: impl AsRef<OsStr>) -> Command {
    use std::os::windows::process::CommandExt;

    let mut command = Command::new(program);
    command.creation_flags(windows_hidden_console_flag());
    command
}

#[cfg(not(target_os = "windows"))]
fn helper_command(program: impl AsRef<OsStr>) -> Command {
    Command::new(program)
}

fn run_helper_command_with_timeout(
    command: &mut Command,
    timeout: Duration,
    purpose: &str,
) -> Result<HelperCommandOutput> {
    run_helper_command_with_timeout_inner(command, timeout, purpose, None)
}

#[cfg(target_os = "macos")]
fn run_launch_helper_command_with_timeout(
    command: &mut Command,
    timeout: Duration,
    purpose: &str,
    launch_context: &ToolConfigLaunchContext<'_>,
) -> Result<HelperCommandOutput> {
    run_helper_command_with_timeout_inner(command, timeout, purpose, Some(launch_context))
}

fn run_helper_command_with_timeout_inner(
    command: &mut Command,
    timeout: Duration,
    purpose: &str,
    launch_context: Option<&ToolConfigLaunchContext<'_>>,
) -> Result<HelperCommandOutput> {
    #[cfg(not(target_os = "windows"))]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match launch_context {
        Some(context) => context.spawn_command(command, purpose),
        None => command
            .spawn()
            .with_context(|| format!("TOOL_CONFIG_HELPER_START_FAILED: {purpose}")),
    }?;
    #[cfg(target_os = "windows")]
    let helper_job = WindowsHelperJob::assign(&child).ok();
    let stdout = child.stdout.take().map(spawn_helper_output_reader);
    let stderr = child.stderr.take().map(spawn_helper_output_reader);
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .with_context(|| format!("TOOL_CONFIG_HELPER_WAIT_FAILED: {purpose}"))?
        {
            break status;
        }
        if Instant::now() >= deadline {
            #[cfg(target_os = "windows")]
            let tree_kill_error = match helper_job.as_ref() {
                Some(job) => terminate_helper_job(job, &mut child, purpose).err(),
                None => terminate_helper_process_tree(&mut child, purpose).err(),
            };
            #[cfg(not(target_os = "windows"))]
            let tree_kill_error = terminate_helper_process_tree(&mut child, purpose).err();
            let stdout_error = collect_helper_output(stdout, purpose, "stdout").err();
            let stderr_error = collect_helper_output(stderr, purpose, "stderr").err();
            return Err(anyhow!(
                "TOOL_CONFIG_HELPER_TIMEOUT: {purpose} exceeded {} ms; tree_kill_error={tree_kill_error:?}; stdout_error={stdout_error:?}; stderr_error={stderr_error:?}",
                timeout.as_millis()
            ));
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = collect_helper_output(stdout, purpose, "stdout")?;
    let stderr = collect_helper_output(stderr, purpose, "stderr")?;
    Ok(HelperCommandOutput {
        status,
        stdout,
        stderr,
    })
}

#[cfg(target_os = "windows")]
struct WindowsHelperJob(windows_sys::Win32::Foundation::HANDLE);

#[cfg(target_os = "windows")]
impl WindowsHelperJob {
    fn assign(child: &Child) -> Result<Self> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::{
            Foundation::GetLastError,
            System::JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
                SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            },
        };

        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(anyhow!(
                "TOOL_CONFIG_HELPER_JOB_CREATE_FAILED: Windows error {}",
                unsafe { GetLastError() }
            ));
        }
        let job = Self(handle);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let limits_size = u32::try_from(std::mem::size_of_val(&limits))
            .context("TOOL_CONFIG_HELPER_JOB_SIZE_INVALID")?;
        let configured = unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&limits).cast(),
                limits_size,
            )
        };
        if configured == 0 {
            return Err(anyhow!(
                "TOOL_CONFIG_HELPER_JOB_CONFIG_FAILED: Windows error {}",
                unsafe { GetLastError() }
            ));
        }
        let assigned = unsafe {
            AssignProcessToJobObject(job.0, child.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE)
        };
        if assigned == 0 {
            return Err(anyhow!(
                "TOOL_CONFIG_HELPER_JOB_ASSIGN_FAILED: Windows error {}",
                unsafe { GetLastError() }
            ));
        }
        Ok(job)
    }
}

#[cfg(target_os = "windows")]
impl Drop for WindowsHelperJob {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(target_os = "windows")]
fn terminate_helper_job(
    job: &WindowsHelperJob,
    child: &mut Child,
    purpose: &str,
) -> Result<()> {
    let terminated =
        unsafe { windows_sys::Win32::System::JobObjects::TerminateJobObject(job.0, 1) };
    if terminated == 0 {
        return Err(anyhow!(
            "TOOL_CONFIG_HELPER_JOB_TERMINATE_FAILED: Windows error {}",
            unsafe { windows_sys::Win32::Foundation::GetLastError() }
        ));
    }
    child
        .wait()
        .with_context(|| format!("TOOL_CONFIG_HELPER_WAIT_FAILED: reap {purpose}"))?;
    Ok(())
}

fn spawn_helper_output_reader(mut pipe: impl Read + Send + 'static) -> HelperOutputReader {
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    let thread = thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = pipe.read_to_end(&mut bytes).map(|_| bytes);
        let _ = result_tx.send(result);
    });
    HelperOutputReader {
        result: result_rx,
        thread,
    }
}

fn collect_helper_output(
    reader: Option<HelperOutputReader>,
    purpose: &str,
    stream: &str,
) -> Result<Vec<u8>> {
    let Some(reader) = reader else {
        return Ok(Vec::new());
    };
    let deadline = Instant::now() + TOOL_HELPER_READER_TIMEOUT;
    let bytes = reader
        .result
        .recv_timeout(TOOL_HELPER_READER_TIMEOUT)
        .map_err(|err| {
            anyhow!(
                "TOOL_CONFIG_HELPER_OUTPUT_TIMEOUT: {purpose} {stream} reader did not finish: {err}"
            )
        })?
        .with_context(|| format!("TOOL_CONFIG_HELPER_OUTPUT_FAILED: read {purpose} {stream}"))?;
    while !reader.thread.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(1));
    }
    if !reader.thread.is_finished() {
        return Err(anyhow!(
            "TOOL_CONFIG_HELPER_OUTPUT_TIMEOUT: {purpose} {stream} reader join exceeded {} ms",
            TOOL_HELPER_READER_TIMEOUT.as_millis()
        ));
    }
    reader.thread.join().map_err(|_| {
        anyhow!("TOOL_CONFIG_HELPER_OUTPUT_FAILED: {purpose} {stream} reader panicked")
    })?;
    Ok(bytes)
}

#[cfg(target_os = "windows")]
fn terminate_helper_process_tree(child: &mut Child, purpose: &str) -> Result<()> {
    let pid = child.id().to_string();
    let mut taskkill = helper_command("taskkill");
    taskkill
        .args(["/PID", &pid, "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut killer = taskkill
        .spawn()
        .with_context(|| format!("TOOL_CONFIG_HELPER_TREE_KILL_FAILED: taskkill {purpose}"))?;
    let deadline = Instant::now() + TOOL_HELPER_TREE_KILL_TIMEOUT;
    let killer_status = loop {
        if let Some(status) = killer.try_wait().with_context(|| {
            format!("TOOL_CONFIG_HELPER_TREE_KILL_FAILED: wait taskkill {purpose}")
        })? {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = killer.kill();
            let _ = killer.wait();
            break None;
        }
        thread::sleep(Duration::from_millis(10));
    };
    if child.try_wait()?.is_none() {
        let _ = child.kill();
    }
    child
        .wait()
        .with_context(|| format!("TOOL_CONFIG_HELPER_WAIT_FAILED: reap {purpose}"))?;
    match killer_status {
        Some(status) if status.success() || status.code() == Some(128) => Ok(()),
        Some(status) => Err(anyhow!(
            "TOOL_CONFIG_HELPER_TREE_KILL_FAILED: taskkill /PID {pid} /T /F exited with {status}"
        )),
        None => Err(anyhow!(
            "TOOL_CONFIG_HELPER_TREE_KILL_TIMEOUT: taskkill /PID {pid} /T /F exceeded {} ms",
            TOOL_HELPER_TREE_KILL_TIMEOUT.as_millis()
        )),
    }
}

#[cfg(not(target_os = "windows"))]
fn terminate_helper_process_tree(child: &mut Child, purpose: &str) -> Result<()> {
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    const SIGTERM: i32 = 15;
    const SIGKILL: i32 = 9;

    let process_group = i32::try_from(child.id()).map_err(|_| {
        anyhow!(
            "TOOL_CONFIG_HELPER_TREE_KILL_FAILED: PID {} exceeds process-group range",
            child.id()
        )
    })?;
    let term_result = unsafe { kill(-process_group, SIGTERM) };
    let term_error = (term_result != 0).then(std::io::Error::last_os_error);
    let grace_deadline = Instant::now() + TOOL_HELPER_TREE_TERM_GRACE;
    while Instant::now() < grace_deadline {
        if child.try_wait()?.is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let kill_result = unsafe { kill(-process_group, SIGKILL) };
    let kill_error = (kill_result != 0).then(std::io::Error::last_os_error);
    if child.try_wait()?.is_none() {
        let _ = child.kill();
    }
    child
        .wait()
        .with_context(|| format!("TOOL_CONFIG_HELPER_WAIT_FAILED: reap {purpose}"))?;
    if term_error.is_some() && kill_error.is_some() {
        return Err(anyhow!(
            "TOOL_CONFIG_HELPER_TREE_KILL_FAILED: {purpose}; TERM={term_error:?}; KILL={kill_error:?}"
        ));
    }
    Ok(())
}

#[cfg(any(test, not(target_os = "windows")))]
fn run_process_probe_command_with_timeout(
    command: &mut Command,
    timeout: Duration,
    purpose: &str,
) -> Result<HelperCommandOutput> {
    let output = run_helper_command_with_timeout(command, timeout, purpose)?;
    if !output.status.success() {
        return Err(anyhow!(
            "TOOL_CONFIG_PROCESS_PROBE_FAILED: {purpose} exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output)
}

#[cfg(test)]
fn run_close_helper_command_with_timeout(
    command: &mut Command,
    timeout: Duration,
    purpose: &str,
    not_found_exit_codes: &[i32],
) -> Result<bool> {
    let output = run_helper_command_with_timeout(command, timeout, purpose)?;
    if output.status.success() {
        return Ok(true);
    }
    if output
        .status
        .code()
        .is_some_and(|code| not_found_exit_codes.contains(&code))
    {
        return Ok(false);
    }
    Err(anyhow!(
        "TOOL_CONFIG_HELPER_FAILED: {purpose} exited with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

#[cfg(test)]
pub(crate) fn test_home_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct TestToolRuntimeState {
    running: bool,
    close_calls: usize,
    launch_calls: usize,
    close_leaves_running: bool,
    close_timeout_immediately: bool,
    state_check_delay: Duration,
    close_delay: Duration,
    process_id: u32,
    process_start_identity: u64,
    probe_failure: bool,
}

#[cfg(test)]
fn test_tool_runtime_registry() -> &'static std::sync::Mutex<HashMap<String, TestToolRuntimeState>>
{
    static REGISTRY: std::sync::OnceLock<std::sync::Mutex<HashMap<String, TestToolRuntimeState>>> =
        std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

#[cfg(test)]
fn clear_test_tool_runtime_registry() {
    test_tool_runtime_registry()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .clear();
    test_process_match_name_registry()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .clear();
    let _ = test_before_authorized_close_hook()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .take();
}

#[cfg(test)]
fn test_process_match_name_registry() -> &'static std::sync::Mutex<HashMap<String, HashSet<String>>>
{
    static REGISTRY: std::sync::OnceLock<std::sync::Mutex<HashMap<String, HashSet<String>>>> =
        std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

#[cfg(test)]
fn set_test_process_match_names(tool: &str, names: HashSet<String>) {
    test_process_match_name_registry()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .insert(tool.to_string(), names);
}

#[cfg(test)]
fn test_process_match_names(tool: &str) -> Option<HashSet<String>> {
    test_process_match_name_registry()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .get(tool)
        .cloned()
}

#[cfg(test)]
type TestBeforeAuthorizedCloseHook = Box<dyn FnOnce() + Send>;

#[cfg(test)]
fn test_before_authorized_close_hook(
) -> &'static std::sync::Mutex<Option<TestBeforeAuthorizedCloseHook>> {
    static HOOK: std::sync::OnceLock<std::sync::Mutex<Option<TestBeforeAuthorizedCloseHook>>> =
        std::sync::OnceLock::new();
    HOOK.get_or_init(|| std::sync::Mutex::new(None))
}

#[cfg(test)]
fn set_test_before_authorized_close_hook(hook: impl FnOnce() + Send + 'static) {
    *test_before_authorized_close_hook()
        .lock()
        .unwrap_or_else(|err| err.into_inner()) = Some(Box::new(hook));
}

#[cfg(test)]
fn run_test_before_authorized_close_hook() {
    let hook = test_before_authorized_close_hook()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .take();
    if let Some(hook) = hook {
        hook();
    }
}

#[cfg(test)]
fn set_test_tool_runtime(tool: &str, running: bool) {
    let identity = next_test_tool_process_identity();
    test_tool_runtime_registry()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .insert(
            tool.to_string(),
            TestToolRuntimeState {
                running,
                process_id: identity,
                process_start_identity: u64::from(identity),
                ..Default::default()
            },
        );
}

#[cfg(test)]
fn next_test_tool_process_identity() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};

    static NEXT_IDENTITY: AtomicU32 = AtomicU32::new(10_000);
    NEXT_IDENTITY.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
fn replace_test_tool_process(tool: &str) {
    let identity = next_test_tool_process_identity();
    let mut registry = test_tool_runtime_registry()
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let state = registry.get_mut(tool).expect("test tool runtime");
    state.running = true;
    state.process_id = identity;
    state.process_start_identity = u64::from(identity);
}

#[cfg(test)]
fn test_tool_process_snapshot(tool: &str) -> Option<ToolProcessSnapshot> {
    let state = test_tool_runtime_snapshot(tool)?;
    Some(if state.running {
        ToolProcessSnapshot::new(vec![ToolProcessIdentity {
            pid: state.process_id,
            executable_path: format!("test-runtime://{tool}/executable"),
            start_identity: state.process_start_identity.to_string(),
        }])
    } else {
        ToolProcessSnapshot::default()
    })
}

#[cfg(test)]
fn set_test_tool_runtime_close_timeout(tool: &str) {
    let mut registry = test_tool_runtime_registry()
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let state = registry.entry(tool.to_string()).or_default();
    state.running = true;
    state.close_leaves_running = true;
    state.close_timeout_immediately = true;
}

#[cfg(test)]
fn set_test_tool_runtime_state_check_delay(tool: &str, delay: Duration) {
    let mut registry = test_tool_runtime_registry()
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    registry
        .entry(tool.to_string())
        .or_default()
        .state_check_delay = delay;
}

#[cfg(test)]
fn set_test_tool_runtime_close_delay(tool: &str, delay: Duration) {
    let mut registry = test_tool_runtime_registry()
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    registry.entry(tool.to_string()).or_default().close_delay = delay;
}

#[cfg(test)]
fn set_test_tool_runtime_probe_failure(tool: &str) {
    let mut registry = test_tool_runtime_registry()
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    registry.entry(tool.to_string()).or_default().probe_failure = true;
}

#[cfg(test)]
fn test_tool_runtime_snapshot(tool: &str) -> Option<TestToolRuntimeState> {
    test_tool_runtime_registry()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .get(tool)
        .copied()
}

#[cfg(test)]
fn test_tool_runtime_close(tool: &str) -> Option<bool> {
    let (delay, was_running) = {
        let mut registry = test_tool_runtime_registry()
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let state = registry.get_mut(tool)?;
        state.close_calls += 1;
        (state.close_delay, state.running)
    };
    if !delay.is_zero() {
        thread::sleep(delay);
    }
    let mut registry = test_tool_runtime_registry()
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let state = registry.get_mut(tool)?;
    if !state.close_leaves_running {
        state.running = false;
    }
    Some(was_running)
}

#[cfg(test)]
fn test_tool_runtime_launch(tool: &str) -> Option<Result<(String, PathBuf)>> {
    let mut registry = test_tool_runtime_registry()
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let state = registry.get_mut(tool)?;
    state.running = true;
    state.launch_calls += 1;
    let identity = next_test_tool_process_identity();
    state.process_id = identity;
    state.process_start_identity = u64::from(identity);
    Some(Ok((
        "Configure and restart".to_string(),
        PathBuf::from(format!("test-runtime://{tool}")),
    )))
}

pub(crate) fn hermes_config_paths() -> Vec<PathBuf> {
    let home = home_dir();
    if !test_home_active() {
        if let Some(root) = nonempty_env_path("HERMES_HOME") {
            return vec![expand_tool_home_path(root).join("config.yaml")];
        }
    }
    let mut paths = Vec::new();

    #[cfg(target_os = "windows")]
    {
        if let Some(test_home) = const_api_test_home() {
            paths.push(
                test_home
                    .join("AppData")
                    .join("Local")
                    .join("hermes")
                    .join("config.yaml"),
            );
        } else {
            if let Some(hermes_home) = read_windows_user_env_path("HERMES_HOME") {
                paths.push(hermes_home.join("config.yaml"));
            }
            if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
                paths.push(local_app_data.join("hermes").join("config.yaml"));
            }
        }
    }

    paths.push(home.join(".hermes").join("config.yaml"));
    dedupe_paths(paths)
}

#[cfg(target_os = "windows")]
fn read_windows_user_env_path(name: &str) -> Option<PathBuf> {
    let mut command = helper_command("reg");
    command.args(["query", "HKCU\\Environment", "/v", name]);
    let output = run_helper_command_with_timeout(
        &mut command,
        TOOL_HELPER_COMMAND_TIMEOUT,
        &format!("reg query {name}"),
    )
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    let value = raw.lines().find_map(|line| {
        let trimmed = line.trim();
        if !trimmed.starts_with(name) {
            return None;
        }
        let parts = trimmed.split_whitespace().collect::<Vec<_>>();
        if parts.len() < 3 {
            return None;
        }
        Some(parts[2..].join(" "))
    })?;
    let expanded = expand_windows_env_vars(&value);
    if expanded.trim().is_empty() {
        None
    } else {
        Some(PathBuf::from(expanded))
    }
}

#[cfg(target_os = "windows")]
fn expand_windows_env_vars(value: &str) -> String {
    let mut expanded = value.to_string();
    for (key, val) in std::env::vars() {
        let needle = format!("%{key}%");
        if expanded
            .to_ascii_uppercase()
            .contains(&needle.to_ascii_uppercase())
        {
            expanded = replace_ascii_case_insensitive(&expanded, &needle, &val);
        }
    }
    expanded
}

#[cfg(target_os = "windows")]
fn replace_ascii_case_insensitive(input: &str, needle: &str, replacement: &str) -> String {
    let mut output = String::new();
    let mut rest = input;
    let needle_lower = needle.to_ascii_lowercase();
    while let Some(pos) = rest.to_ascii_lowercase().find(&needle_lower) {
        output.push_str(&rest[..pos]);
        output.push_str(replacement);
        rest = &rest[pos + needle.len()..];
    }
    output.push_str(rest);
    output
}

fn dedupe_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut unique = Vec::new();
    for path in paths {
        if !unique.iter().any(|existing| existing == &path) {
            unique.push(path);
        }
    }
    unique
}

pub(crate) fn backup_root() -> PathBuf {
    crate::client_data_root().join("backups")
}
