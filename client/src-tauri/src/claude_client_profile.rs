//! Synthetic Claude Code profile used only when adapting a non-native request
//! to the Claude account-subscription executor.
//!
//! Native Claude Code requests keep their original body and identity headers.
//! Keep the values below together so a future compatibility update cannot
//! accidentally mix several Claude Code releases in one request.

// Version pinned by the official Claude Agent SDK 0.2.162 (_cli_version.py).
pub(crate) const CLAUDE_CODE_VERSION: &str = "2.1.285";
pub(crate) const CLAUDE_CODE_USER_AGENT: &str = "claude-cli/2.1.285 (external, sdk-cli)";
pub(crate) const CLAUDE_CODE_CONTROL_USER_AGENT: &str = "claude-code/2.1.285";
pub(crate) const CLAUDE_CODE_ENTRYPOINT: &str = "sdk-cli";
pub(crate) const CLAUDE_CODE_STAINLESS_PACKAGE_VERSION: &str = "0.112.1";
pub(crate) const CLAUDE_CODE_STAINLESS_RUNTIME_VERSION: &str = "v26.3.0";
pub(crate) const CLAUDE_OAUTH_AXIOS_USER_AGENT: &str = "axios/1.15.2";
pub(crate) const CLAUDE_DIRECT_BROWSER_ACCESS: &str = "true";

// Captured from the official Claude Code 2.1.258 Agent SDK request. The
// fallback-credit beta is intentionally omitted: the official client retries
// without it when the upstream rejects that optional capability.
pub(crate) const CLAUDE_CODE_BETA_HEADER: &str = "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,thinking-token-count-2026-05-13,context-management-2025-06-27,prompt-caching-scope-2026-01-05,mid-conversation-system-2026-04-07,effort-2025-11-24,extended-cache-ttl-2025-04-11";
pub(crate) const CLAUDE_SERVER_SIDE_COMPACTION_BETA: &str = "compact-2026-01-12";
pub(crate) const CLAUDE_CATALOG_BETA_HEADER: &str = "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,fine-grained-tool-streaming-2025-05-14";
pub(crate) const CLAUDE_COUNT_TOKENS_BETA_HEADER: &str = "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,thinking-token-count-2026-05-13,context-management-2025-06-27,prompt-caching-scope-2026-01-05,mid-conversation-system-2026-04-07,effort-2025-11-24,extended-cache-ttl-2025-04-11,token-counting-2024-11-01";

pub(crate) const CLAUDE_CODE_IDENTITY_PROMPT: &str =
    "You are a Claude agent, built on Anthropic's Claude Agent SDK.";

pub(crate) fn claude_code_stainless_os() -> &'static str {
    if cfg!(target_os = "windows") {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "MacOS"
    } else if cfg!(target_os = "linux") {
        "Linux"
    } else {
        "Unknown"
    }
}

pub(crate) fn claude_code_stainless_arch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "x86") {
        "x86"
    } else if cfg!(target_arch = "arm") {
        "arm"
    } else {
        "unknown"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_profile_is_one_coherent_release() {
        assert!(CLAUDE_CODE_USER_AGENT.contains(CLAUDE_CODE_VERSION));
        assert!(CLAUDE_CODE_CONTROL_USER_AGENT.ends_with(CLAUDE_CODE_VERSION));
        assert_eq!(CLAUDE_CODE_ENTRYPOINT, "sdk-cli");
        assert!(CLAUDE_CODE_BETA_HEADER.contains("oauth-2025-04-20"));
        assert!(!CLAUDE_CODE_BETA_HEADER.contains("fallback-credit"));
    }
}
