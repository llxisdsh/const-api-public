export type ToolCatalogLink = {
  label: string;
  url: string;
};

export type ToolProtocolId = "openai_responses" | "openai_chat" | "anthropic_messages" | "gemini_native";
export type ToolModelSyncPolicy = "none" | "selected" | "catalog";

export type ToolCatalogEntry = {
  tool: string;
  title: string;
  // Distinguish parallel editions, not every terminal-based tool.
  badge?: "CLI" | "CN";
  description: string;
  officialLinks: readonly ToolCatalogLink[];
  fallbackLinks?: readonly ToolCatalogLink[];
  protocols: readonly ToolProtocolId[];
  defaultProtocol: ToolProtocolId;
  modelSyncPolicy: ToolModelSyncPolicy;
  protocolScope?: "model";
};

// Official links point to vendor-owned sites or canonical project repositories.
// Third-party sources belong in fallbackLinks for use when official downloads or updates fail.
export const TOOL_CATALOG = {
  codex: {
    tool: "codex",
    title: "Codex",
    description: "OpenAI Codex with support for the current ChatGPT and legacy Codex desktop apps",
    protocols: ["openai_responses"],
    defaultProtocol: "openai_responses",
    modelSyncPolicy: "none",
    officialLinks: [
      { label: "Official download", url: "https://chatgpt.com/download/" },
      { label: "GitHub", url: "https://github.com/openai/codex" },
    ],
    fallbackLinks: [
      { label: "Codex App Manager", url: "https://codexapp.agentsmirror.com/" },
    ],
  },
  claude: {
    tool: "claude",
    title: "Claude Code",
    description: "Anthropic coding agent for the terminal",
    protocols: ["anthropic_messages"],
    defaultProtocol: "anthropic_messages",
    modelSyncPolicy: "none",
    officialLinks: [
      { label: "Official website", url: "https://claude.com/product/claude-code" },
      { label: "GitHub", url: "https://github.com/anthropics/claude-code" },
    ],
  },
  "claude-desktop": {
    tool: "claude-desktop",
    title: "Claude Desktop",
    description: "Claude desktop app with extensions and local tool connections",
    protocols: ["anthropic_messages"],
    defaultProtocol: "anthropic_messages",
    modelSyncPolicy: "catalog",
    officialLinks: [
      { label: "Official download", url: "https://claude.com/download" },
    ],
  },
  "claude-science": {
    tool: "claude-science",
    title: "Claude Science",
    description: "Research workspace; CONST API connectivity has not been verified",
    protocols: ["anthropic_messages"],
    defaultProtocol: "anthropic_messages",
    modelSyncPolicy: "none",
    officialLinks: [
      { label: "Official website", url: "https://claude.com/product/claude-science" },
    ],
  },
  gemini: {
    tool: "gemini",
    title: "Gemini CLI",
    description: "Google's open-source, extensible terminal AI agent",
    protocols: ["gemini_native"],
    defaultProtocol: "gemini_native",
    modelSyncPolicy: "none",
    officialLinks: [
      { label: "Official website", url: "https://geminicli.com/" },
      { label: "GitHub", url: "https://github.com/google-gemini/gemini-cli" },
    ],
  },
  copilot: {
    tool: "copilot",
    title: "GitHub Copilot CLI",
    badge: "CLI",
    description: "GitHub's terminal coding agent using an OpenAI-compatible BYOK provider",
    protocols: ["openai_chat"],
    defaultProtocol: "openai_chat",
    modelSyncPolicy: "selected",
    officialLinks: [
      { label: "Official website", url: "https://github.com/features/copilot/cli" },
      { label: "Documentation", url: "https://docs.github.com/en/copilot/concepts/agents/copilot-cli/about-copilot-cli" },
    ],
  },
  "copilot-desktop": {
    tool: "copilot-desktop",
    title: "GitHub Copilot",
    description: "GitHub Copilot desktop app with automatic BYOK models and system-keychain credentials",
    protocols: ["openai_responses", "openai_chat", "anthropic_messages"],
    defaultProtocol: "openai_responses",
    protocolScope: "model",
    modelSyncPolicy: "catalog",
    officialLinks: [
      { label: "Documentation and download", url: "https://docs.github.com/en/copilot/how-tos/github-copilot-app/use-byok-models" },
    ],
  },
  cline: {
    tool: "cline",
    title: "Cline",
    description: "Cline's desktop and terminal coding agent with shared model configuration",
    protocols: ["openai_responses", "openai_chat"],
    defaultProtocol: "openai_responses",
    modelSyncPolicy: "catalog",
    officialLinks: [
      { label: "GitHub", url: "https://github.com/cline/cline" },
    ],
  },
  opencode: {
    tool: "opencode",
    title: "OpenCode",
    description: "Open-source AI coding agent for terminal, desktop, and IDE",
    protocols: ["openai_responses", "anthropic_messages", "gemini_native", "openai_chat"],
    protocolScope: "model",
    defaultProtocol: "openai_responses",
    modelSyncPolicy: "catalog",
    officialLinks: [
      { label: "Official website", url: "https://opencode.ai/" },
      { label: "GitHub", url: "https://github.com/anomalyco/opencode" },
    ],
  },
  trae: {
    tool: "trae",
    title: "TraeCode",
    description: "Trae Code with automatic custom-model configuration; sign in to Trae first",
    protocols: ["openai_chat", "openai_responses", "anthropic_messages"],
    defaultProtocol: "openai_chat",
    protocolScope: "model",
    modelSyncPolicy: "catalog",
    officialLinks: [{ label: "Official download", url: "https://www.trae.ai/download" }],
  },
  "trae-cn": {
    tool: "trae-cn",
    title: "TraeCode CN",
    badge: "CN",
    description: "Trae Code China with automatic custom-model configuration; sign in to Trae first",
    protocols: ["openai_chat", "openai_responses", "anthropic_messages"],
    defaultProtocol: "openai_chat",
    protocolScope: "model",
    modelSyncPolicy: "catalog",
    officialLinks: [{ label: "Official download", url: "https://www.trae.cn/download" }],
  },
  "trae-work": {
    tool: "trae-work",
    title: "TraeWork",
    description: "TraeWork desktop custom models for local tasks; sign in to TraeWork first",
    protocols: ["openai_chat", "openai_responses", "anthropic_messages"],
    defaultProtocol: "openai_chat",
    protocolScope: "model",
    modelSyncPolicy: "catalog",
    officialLinks: [{ label: "Official download", url: "https://www.trae.cn/download" }],
  },
  openclaw: {
    tool: "openclaw",
    title: "OpenClaw",
    description: "Personal AI assistant for local use and chat channels",
    protocols: ["openai_responses", "openai_chat", "anthropic_messages", "gemini_native"],
    defaultProtocol: "openai_responses",
    protocolScope: "model",
    modelSyncPolicy: "catalog",
    officialLinks: [
      { label: "Official website", url: "https://openclaw.ai/" },
      { label: "GitHub", url: "https://github.com/openclaw/openclaw" },
    ],
  },
  goose: {
    tool: "goose",
    title: "Goose",
    description: "Block's open-source desktop and terminal AI agent",
    protocols: ["openai_chat"],
    defaultProtocol: "openai_chat",
    modelSyncPolicy: "catalog",
    officialLinks: [
      { label: "Official website", url: "https://block.github.io/goose/" },
      { label: "GitHub", url: "https://github.com/aaif-goose/goose" },
    ],
  },
  raven: {
    tool: "raven",
    title: "Raven",
    description: "EverMind's memory-first, self-improving agent harness",
    protocols: ["openai_chat"],
    defaultProtocol: "openai_chat",
    modelSyncPolicy: "catalog",
    officialLinks: [
      { label: "Official website", url: "https://raven.evermind.ai/" },
      { label: "GitHub", url: "https://github.com/EverMind-AI/Raven" },
    ],
  },
  reasonix: {
    tool: "reasonix",
    title: "DeepSeek Reasonix",
    description: "Open-source reasoning and coding agent with desktop and terminal clients",
    protocols: ["openai_chat"],
    defaultProtocol: "openai_chat",
    modelSyncPolicy: "catalog",
    officialLinks: [
      { label: "GitHub", url: "https://github.com/esengine/DeepSeek-Reasonix" },
    ],
  },
  "deepseek-harness": {
    tool: "deepseek-harness",
    title: "DeepSeek Harness",
    description: "DeepSeek's open-source, plugin-based agent harness with desktop and local Web clients",
    protocols: ["openai_chat", "openai_responses", "anthropic_messages"],
    defaultProtocol: "openai_chat",
    modelSyncPolicy: "catalog",
    officialLinks: [
      { label: "Official website", url: "https://www.deepseek.com/harness/" },
      { label: "GitHub", url: "https://github.com/deepseek-ai/deepseek-harness" },
    ],
  },
  pi: {
    tool: "pi",
    title: "Pi",
    description: "Minimal, extensible terminal coding agent from pi-mono",
    protocols: ["openai_responses", "anthropic_messages", "gemini_native", "openai_chat"],
    protocolScope: "model",
    defaultProtocol: "openai_responses",
    modelSyncPolicy: "catalog",
    officialLinks: [
      { label: "GitHub", url: "https://github.com/badlogic/pi-mono" },
    ],
  },
  hermes: {
    tool: "hermes",
    title: "Hermes",
    description: "Nous Research open-source AI agent with learning loops and persistent memory",
    protocols: ["openai_chat"],
    defaultProtocol: "openai_chat",
    modelSyncPolicy: "none",
    officialLinks: [
      { label: "Official website", url: "https://hermes-agent.nousresearch.com/" },
      { label: "GitHub", url: "https://github.com/NousResearch/hermes-agent" },
    ],
  },
  vscode: {
    tool: "vscode",
    title: "VS Code",
    description: "VS Code Chat and Agent with model-specific protocol selection",
    protocols: ["openai_responses", "openai_chat", "anthropic_messages"],
    defaultProtocol: "openai_responses",
    protocolScope: "model",
    modelSyncPolicy: "catalog",
    officialLinks: [
      { label: "Official download", url: "https://code.visualstudio.com/Download" },
    ],
  },
  workbuddy: {
    tool: "workbuddy",
    title: "WorkBuddy",
    description: "Professional desktop agent for office work, research, and multi-step tasks",
    protocols: ["openai_chat"],
    defaultProtocol: "openai_chat",
    modelSyncPolicy: "catalog",
    officialLinks: [
      { label: "Official website", url: "https://www.codebuddy.cn/work/" },
    ],
  },
  kimicode: {
    tool: "kimicode",
    title: "Kimi Code",
    description: "Kimi Code desktop and terminal agent with shared model configuration",
    protocols: ["openai_responses", "openai_chat"],
    defaultProtocol: "openai_responses",
    protocolScope: "model",
    modelSyncPolicy: "catalog",
    officialLinks: [
      { label: "Official download", url: "https://www.kimi.com/code" },
      { label: "GitHub", url: "https://github.com/MoonshotAI/kimi-code" },
    ],
  },
  mimocode: {
    tool: "mimocode",
    title: "MiMo Code",
    description: "Xiaomi MiMo's open-source coding agent for the terminal",
    protocols: ["openai_chat", "openai_responses", "anthropic_messages", "gemini_native"],
    defaultProtocol: "openai_chat",
    protocolScope: "model",
    modelSyncPolicy: "catalog",
    officialLinks: [
      { label: "GitHub", url: "https://github.com/XiaomiMiMo/MiMo-Code" },
    ],
  },
  qwencode: {
    tool: "qwencode",
    title: "Qwen Code",
    description: "Qwen Code desktop and terminal agent with shared model configuration",
    protocols: ["openai_chat"],
    defaultProtocol: "openai_chat",
    modelSyncPolicy: "catalog",
    officialLinks: [
      { label: "GitHub", url: "https://github.com/QwenLM/qwen-code" },
    ],
  },
  openscience: {
    tool: "openscience",
    title: "Open Science",
    description: "Open-source research workspace and scientific agent",
    protocols: ["openai_chat", "openai_responses"],
    defaultProtocol: "openai_chat",
    protocolScope: "model",
    modelSyncPolicy: "catalog",
    officialLinks: [
      { label: "GitHub", url: "https://github.com/ai4s-research/open-science" },
    ],
  },
  "open-interpreter": {
    tool: "open-interpreter",
    title: "Open Interpreter",
    description: "Natural-language computer and coding agent for the terminal",
    protocols: ["openai_chat"],
    defaultProtocol: "openai_chat",
    modelSyncPolicy: "selected",
    officialLinks: [
      { label: "Official website", url: "https://openinterpreter.com/" },
      { label: "GitHub", url: "https://github.com/openinterpreter/openinterpreter" },
    ],
  },
  "mistral-vibe": {
    tool: "mistral-vibe",
    title: "Mistral Vibe",
    description: "Mistral AI's open-source terminal coding agent",
    protocols: ["openai_chat"],
    defaultProtocol: "openai_chat",
    modelSyncPolicy: "catalog",
    officialLinks: [
      { label: "GitHub", url: "https://github.com/mistralai/mistral-vibe" },
    ],
  },
  anythingllm: {
    tool: "anythingllm",
    title: "AnythingLLM",
    description: "Desktop AI workspace with local knowledge bases; configures the default language model",
    protocols: ["openai_chat"],
    defaultProtocol: "openai_chat",
    modelSyncPolicy: "selected",
    officialLinks: [
      { label: "Official website", url: "https://anythingllm.com/" },
      { label: "GitHub", url: "https://github.com/Mintplex-Labs/anything-llm" },
    ],
  },
  "open-design": {
    tool: "open-design",
    title: "Open Design",
    description: "Local-first open-source AI design workspace using its official Codex adapter",
    protocols: ["openai_responses"],
    defaultProtocol: "openai_responses",
    modelSyncPolicy: "selected",
    officialLinks: [
      { label: "Official website", url: "https://open-design.ai/" },
      { label: "GitHub", url: "https://github.com/nexu-io/open-design" },
    ],
  },
  "vibe-trading": {
    tool: "vibe-trading",
    title: "Vibe-Trading",
    description: "Open-source AI trading research and agent workspace",
    protocols: ["openai_chat"],
    defaultProtocol: "openai_chat",
    modelSyncPolicy: "selected",
    officialLinks: [
      { label: "GitHub", url: "https://github.com/HKUDS/Vibe-Trading" },
    ],
  },
  "grok-build": {
    tool: "grok-build",
    title: "Grok Build",
    description: "xAI terminal coding agent with a custom model catalog and per-model API protocols",
    protocols: ["openai_responses", "anthropic_messages", "openai_chat"],
    defaultProtocol: "openai_responses",
    protocolScope: "model",
    modelSyncPolicy: "catalog",
    officialLinks: [
      { label: "GitHub", url: "https://github.com/xai-org/grok-build" },
    ],
  },
  "minimax-code": {
    tool: "minimax-code",
    title: "MiniMax Code",
    description: "MiniMax desktop and terminal agent with shared custom-provider and model configuration",
    protocols: ["anthropic_messages", "openai_responses", "openai_chat"],
    defaultProtocol: "anthropic_messages",
    modelSyncPolicy: "catalog",
    officialLinks: [
      { label: "GitHub", url: "https://github.com/MiniMax-AI/minimax-code" },
    ],
  },
  zcode: {
    tool: "zcode",
    title: "ZCode",
    description: "Z.ai desktop coding agent with custom OpenAI-compatible providers",
    protocols: ["openai_chat"],
    defaultProtocol: "openai_chat",
    modelSyncPolicy: "catalog",
    officialLinks: [
      { label: "Official website", url: "https://zcode.z.ai/en" },
    ],
  },
} as const satisfies Record<string, ToolCatalogEntry>;

export type ToolCatalogId = keyof typeof TOOL_CATALOG;

// This is the canonical/default dock order. Users can move a tool to the end by
// hiding and restoring it, while newly supported tools reconcile against this list.
export const TOOL_CATALOG_ORDER = [
  "codex",
  "claude",
  "claude-desktop",
  "vscode",
  "opencode",
  "hermes",
  "deepseek-harness",
  "workbuddy",
  "openclaw",
  "gemini",
  "copilot",
  "copilot-desktop",
  "cline",
  "trae",
  "trae-cn",
  "trae-work",
  "raven",
  "pi",
  "claude-science",
  "goose",
  "reasonix",
  "kimicode",
  "mimocode",
  "qwencode",
  "mistral-vibe",
  "open-interpreter",
  "anythingllm",
  "openscience",
  "open-design",
  "vibe-trading",
  "grok-build",
  "minimax-code",
  "zcode",
] as const satisfies readonly ToolCatalogId[];

export const DEFAULT_VISIBLE_TOOL_IDS = [
  "codex",
  "claude",
  "claude-desktop",
  "vscode",
  "opencode",
  "hermes",
  "deepseek-harness",
  "workbuddy",
  "openclaw",
] as const satisfies readonly ToolCatalogId[];

export function toolCatalogEntry(tool: string): ToolCatalogEntry | null {
  return Object.prototype.hasOwnProperty.call(TOOL_CATALOG, tool)
    ? TOOL_CATALOG[tool as ToolCatalogId]
    : null;
}
