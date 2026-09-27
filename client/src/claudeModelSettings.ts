export const CLAUDE_MODEL_AUTO = "";
export const CLAUDE_MODEL_FOLLOW_MAIN = "__const_follow_main__";

export type ClaudeModelSettings = {
  main: string;
  opus: string;
  sonnet: string;
  haiku: string;
};

export const DEFAULT_CLAUDE_MODEL_SETTINGS: ClaudeModelSettings = {
  main: CLAUDE_MODEL_AUTO,
  opus: CLAUDE_MODEL_AUTO,
  sonnet: CLAUDE_MODEL_AUTO,
  haiku: CLAUDE_MODEL_AUTO,
};

const CLAUDE_MODEL_SETTINGS_KEY = "const-api:claude-model-settings:v1";

function normalizedChoice(value: unknown, fallback: string) {
  return typeof value === "string" ? value.trim() : fallback;
}

export function normalizeClaudeModelSettings(value: unknown): ClaudeModelSettings {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return { ...DEFAULT_CLAUDE_MODEL_SETTINGS };
  }
  const candidate = value as Partial<Record<keyof ClaudeModelSettings, unknown>>;
  const main = normalizedChoice(candidate.main, CLAUDE_MODEL_AUTO);
  const normalizedMain = main === CLAUDE_MODEL_FOLLOW_MAIN ? CLAUDE_MODEL_AUTO : main;
  const normalizedRole = (value: unknown) => {
    const role = normalizedChoice(value, CLAUDE_MODEL_AUTO);
    return role === CLAUDE_MODEL_FOLLOW_MAIN ? normalizedMain : role;
  };
  return {
    main: normalizedMain,
    opus: normalizedRole(candidate.opus),
    sonnet: normalizedRole(candidate.sonnet),
    haiku: normalizedRole(candidate.haiku),
  };
}

export function readClaudeModelSettings(): ClaudeModelSettings {
  if (typeof window === "undefined") return { ...DEFAULT_CLAUDE_MODEL_SETTINGS };
  try {
    const stored = window.localStorage.getItem(CLAUDE_MODEL_SETTINGS_KEY);
    return normalizeClaudeModelSettings(stored ? JSON.parse(stored) : null);
  } catch {
    return { ...DEFAULT_CLAUDE_MODEL_SETTINGS };
  }
}

export function writeClaudeModelSettings(settings: ClaudeModelSettings) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(
      CLAUDE_MODEL_SETTINGS_KEY,
      JSON.stringify(normalizeClaudeModelSettings(settings)),
    );
  } catch {
    // UI preferences are best-effort; tool configuration remains authoritative.
  }
}

export function claudeModelSettingsEqual(
  left: ClaudeModelSettings,
  right: ClaudeModelSettings,
) {
  return left.main === right.main
    && left.opus === right.opus
    && left.sonnet === right.sonnet
    && left.haiku === right.haiku;
}
