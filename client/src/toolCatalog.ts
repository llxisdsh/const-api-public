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
  badge?: "AI" | "CLI" | "CN";
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
    description: "OpenAI Codex is a desktop-first flagship coding agent; it is more than autocomplete, supervising parallel agents, understanding large repositories, editing files, running tests, and carrying long tasks across the app, IDE, terminal, and cloud.",
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
    description: "Anthropic’s Claude Code is a powerful partner for complex fixes, tests, and migrations; it works across the terminal, IDE, Slack, and web, explaining trade-offs clearly and with unusual nuance when the work needs both engineering strength and good communication.",
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
    description: "Anthropic’s Claude desktop app starts with natural conversation and connects local files, apps, and tools through extensions; it is a strong fit for long-form reading, writing, analysis, and everyday work in your computer’s own context.",
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
    description: "Anthropic’s Claude Science is a public-beta research workbench for connecting scientific databases and compute environments, running specialist analyses, and keeping a reviewable evidence trail for work that must stand up to scrutiny.",
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
    description: "Google’s Gemini CLI is an open-source terminal agent that understands a codebase, writes features, debugs failures, and handles build and deployment in one conversation; it is a natural command-line choice if you already use Google’s model ecosystem.",
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
    description: "GitHub Copilot CLI brings GitHub’s widely used coding assistant into the terminal: explore repositories, explain unfamiliar code, edit files, and run commands with plain-language requests, all while staying close to the workflow your team already knows.",
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
    description: "GitHub Copilot’s desktop app provides project-focused Agent sessions with GitHub-hosted models or your own provider key; it is the better fit when GitHub’s review habits, repositories, and team workflow matter as much as the model.",
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
    description: "Cline is an open-source desktop and terminal coding agent with IDE support; every file edit, command, and test is visible for your review, making it a good choice when you want AI to do the work without giving up control.",
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
    description: "OpenCode is an open-source AI coding agent for terminal, desktop, or IDE workflows that connects to your preferred model provider; it is a strong choice if you want freedom to change models without changing your working style.",
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
    title: "TRAE",
    description: "TRAE’s global edition combines an AI IDE with autonomous Agent and SOLO modes to understand projects, plan work, call tools, and ship working software; choose it when you want to move from an idea to a runnable result in one place.",
    protocols: ["openai_chat", "openai_responses", "anthropic_messages"],
    defaultProtocol: "openai_chat",
    protocolScope: "model",
    modelSyncPolicy: "catalog",
    officialLinks: [{ label: "Official download", url: "https://www.trae.ai/download" }],
  },
  "trae-cn": {
    tool: "trae-cn",
    title: "TRAE CN",
    badge: "CN",
    description: "TRAE’s China edition switches between IDE and Agent modes, first understanding the project and then coordinating tools to turn an idea into a runnable result; it is the natural option when your development environment and services are China-based.",
    protocols: ["openai_chat", "openai_responses", "anthropic_messages"],
    defaultProtocol: "openai_chat",
    protocolScope: "model",
    modelSyncPolicy: "catalog",
    officialLinks: [{ label: "Official download", url: "https://www.trae.cn/download" }],
  },
  "trae-work": {
    tool: "trae-work",
    title: "TraeWork",
    description: "TraeWork is TRAE’s legacy workspace for research, office tasks, documents, and delivery; if this entry appears, the safer path is usually to move to the newer TRAE product.",
    protocols: ["openai_chat", "openai_responses", "anthropic_messages"],
    defaultProtocol: "openai_chat",
    protocolScope: "model",
    modelSyncPolicy: "catalog",
    officialLinks: [{ label: "Official download", url: "https://www.trae.cn/download" }],
  },
  openclaw: {
    tool: "openclaw",
    title: "OpenClaw",
    description: "OpenClaw is an open-source personal AI assistant that can act on your computer and through messaging channels; self-hosting, cross-platform use, and data ownership make it attractive when the assistant should live in your environment.",
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
    description: "Block’s Goose is an extensible open-source agent for installing dependencies, inspecting and editing files, executing commands, and running tests with the LLM you choose; it rewards people who like assembling their own toolchain.",
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
    description: "EverMind’s Raven is a multi-agent orchestration workspace that assembles specialists around a goal, lets them collaborate, and keeps each handoff visible; it is made for work that benefits from division, review, and steady progress.",
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
    description: "Reasonix is an open-source coding agent for terminal, desktop, browser, or editor work that reads and edits projects, runs commands, and verifies results; think of it as a development helper that can follow you across work surfaces.",
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
    description: "DeepSeek Harness is an open-source plugin-based agent workspace that treats tools as replaceable building blocks for office, analysis, and coding tasks on desktop or web; it suits people who want to assemble their own AI workbench.",
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
    description: "Pi is a lightweight, extensible terminal-agent toolkit with a unified model API, interactive TUI, and coding CLI; it leaves the workflow to you, which is exactly why it appeals to people who like composing their own models, prompts, and tools.",
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
    description: "Nous Research’s Hermes is a persistent AI agent that works across terminal, desktop, and chat, remembers useful context over time, and builds reusable skills; it is designed for a long-running relationship with your workflow rather than one-off answers.",
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
    description: "Microsoft’s VS Code Chat and Agents put AI inside the editor you already know: understand code, edit projects, call tools, and run checks without leaving one workspace; it is the practical choice when context switching is the problem.",
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
    description: "WorkBuddy China lets you describe a goal in one sentence, then uses specialist agents to process local files and deliver documents, spreadsheets, or presentations; it feels more like a digital colleague who gets things done than a chatbot that only replies.",
    protocols: ["openai_chat"],
    defaultProtocol: "openai_chat",
    modelSyncPolicy: "catalog",
    officialLinks: [
      { label: "Official website", url: "https://www.codebuddy.cn/work/" },
    ],
  },
  "workbuddy-ai": {
    tool: "workbuddy-ai",
    title: "WorkBuddy AI",
    badge: "AI",
    description: "WorkBuddy international is an all-in-one AI workspace for planning and executing research, documents, coding, and everyday file tasks, then delivering usable results; it is for people who want one assistant to plan and finish scattered work.",
    protocols: ["openai_chat"],
    defaultProtocol: "openai_chat",
    modelSyncPolicy: "catalog",
    officialLinks: [
      { label: "Official website", url: "https://www.workbuddy.ai/" },
    ],
  },
  codebuddy: {
    tool: "codebuddy",
    title: "CodeBuddy",
    description: "Tencent CodeBuddy is a programming assistant that takes work from code writing through deployment, with model switching, plugins, and agent-style automation; it is built for developers who want coding, debugging, and delivery in one flow.",
    protocols: ["openai_chat"],
    defaultProtocol: "openai_chat",
    modelSyncPolicy: "catalog",
    officialLinks: [
      { label: "Official website", url: "https://www.codebuddy.ai/" },
      { label: "Models documentation", url: "https://www.codebuddy.ai/docs/cli/models" },
    ],
  },
  qoder: {
    tool: "qoder",
    title: "Qoder",
    description: "Qoder is an agentic coding platform that understands the whole codebase, plans from a requirement, implements changes, runs tests, and carries the task through delivery; it becomes most useful when the job is a whole project task rather than a single question.",
    protocols: ["openai_responses", "openai_chat", "anthropic_messages"],
    defaultProtocol: "openai_responses",
    modelSyncPolicy: "catalog",
    officialLinks: [
      { label: "Official website", url: "https://qoder.com/" },
      { label: "Custom models documentation", url: "https://docs.qoder.com/qoder/custom-models" },
    ],
  },
  "qoder-cn": {
    tool: "qoder-cn",
    title: "Qoder CN",
    badge: "CN",
    description: "Qoder CN is a China-deployed agent workspace with IDE, JetBrains plugin, and terminal entry points for understanding projects and completing coding tasks; it fits teams already working in China-based developer environments.",
    protocols: ["openai_responses", "openai_chat", "anthropic_messages"],
    defaultProtocol: "anthropic_messages",
    modelSyncPolicy: "catalog",
    officialLinks: [
      { label: "Official website", url: "https://qoder.com/" },
      { label: "Custom models documentation", url: "https://docs.qoder.com/qoder/custom-models" },
    ],
  },
  octop: {
    tool: "octop",
    title: "Octop",
    description: "Tencent Cloud’s Octop is an open-source, self-hosted assistant that lets multiple users and agents collaborate across web, terminal, chat platforms, connectors, and MCP tools; it is for teams that want their own AI hub instead of another single-vendor SaaS.",
    protocols: ["openai_chat"],
    defaultProtocol: "openai_chat",
    modelSyncPolicy: "catalog",
    officialLinks: [
      { label: "GitHub", url: "https://github.com/TencentCloud/Octop" },
    ],
  },
  toonflow: {
    tool: "toonflow",
    title: "Toonflow",
    description: "Toonflow is an open-source AI short-drama and video workspace that connects canvas scripts, storyboards, asset management, and image or video generation; it is aimed at keeping a short-film, motion-comic, or video pipeline in one place.",
    protocols: ["openai_responses", "openai_chat"],
    defaultProtocol: "openai_responses",
    modelSyncPolicy: "catalog",
    officialLinks: [
      { label: "GitHub", url: "https://github.com/HBAI-Ltd/Toonflow-app" },
      { label: "Releases", url: "https://github.com/HBAI-Ltd/Toonflow-app/releases" },
    ],
  },
  kimicode: {
    tool: "kimicode",
    title: "Kimi Code",
    description: "Moonshot AI’s Kimi Code is a desktop and terminal coding agent that reads and edits code, runs commands, searches files and web pages, and carries a vague question through to a concrete, reviewable change.",
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
    description: "Xiaomi’s MiMo Code is a terminal-native coding assistant that reads and writes code, runs commands, manages Git, and keeps persistent memory; it also powers the companion desktop app for people who want command-line depth with a desktop front end.",
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
    description: "Qwen Code is an open-source desktop and terminal coding agent with editor support, memory, skills, subagents, and multiple protocol options; it has the room to grow into reusable automation for larger development tasks.",
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
    description: "Open Science is a local-first, model-agnostic research workbench that brings notes, files, MCP tools, runs, and reproducible experiment artifacts into one place; it is for researchers who care about privacy, repeatability, and a record of how results were made.",
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
    description: "Open Interpreter is a natural-language coding agent for open models that lets you read files, run code, and complete development tasks from a terminal conversation; it is a direct starting point when you want your own model to operate a computer.",
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
    description: "Mistral Vibe is a minimal terminal coding agent for understanding a project, making focused edits, and completing development tasks without a heavy interface; it is for people who want to start working before they start configuring.",
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
    description: "AnythingLLM is a local-first AI workspace that turns documents into a searchable knowledge base, then uses agents, workflows, and local or cloud models to work with it; choose it when private documents, Q&A, and automation should live together.",
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
    description: "OpenDesign is a local-first collaborative design workspace for creating prototypes, websites, slides, images, and video with AI while sharing feedback in the same project; it brings design, content, and product work closer together.",
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
    description: "Vibe-Trading is a personal trading-research agent combining market data, analysis, backtesting, and strategy workflows; use it to investigate ideas and test assumptions, not as a promise of returns or a substitute for trading judgment.",
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
    description: "xAI’s Grok Build is a full-screen terminal coding agent for understanding code, editing files, running commands, searching the web, and handling long-running tasks; it suits people who want one persistent terminal companion for the whole job.",
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
    description: "MiniMax Code starts with a plain-language requirement, generates working software, then builds, tests, and iterates from the terminal; it is useful when a rough idea needs to become a runnable prototype quickly.",
    protocols: ["anthropic_messages", "openai_responses", "openai_chat"],
    defaultProtocol: "anthropic_messages",
    modelSyncPolicy: "catalog",
    officialLinks: [
      { label: "GitHub", url: "https://github.com/MiniMax-AI/minimax-code" },
    ],
  },
  "muse-code": {
    tool: "muse-code",
    title: "Muse Code",
    description: "Meta’s Muse Code is a terminal and CI coding agent with approval gates and an OS sandbox, designed for long tasks and parallel development workflows; its strength is controlled, auditable automation rather than disposable code generation.",
    protocols: ["openai_responses"],
    defaultProtocol: "openai_responses",
    modelSyncPolicy: "catalog",
    officialLinks: [
      { label: "Official website", url: "https://dev.meta.ai/docs/muse-code" },
    ],
  },
  zcode: {
    tool: "zcode",
    title: "ZCode",
    description: "Z.ai’s ZCode is a desktop development environment tuned for GLM-5.3, with goal-driven long tasks, multi-agent collaboration, and continuous verification; it is a natural choice if you want a Chinese model ecosystem to stay involved throughout a real project.",
    protocols: ["openai_chat"],
    defaultProtocol: "openai_chat",
    modelSyncPolicy: "catalog",
    officialLinks: [
      { label: "Official website", url: "https://zcode.z.ai/en" },
    ],
  },
} as const satisfies Record<string, ToolCatalogEntry>;

export type ToolCatalogId = keyof typeof TOOL_CATALOG;

// Keep legacy IDs and adapters for persisted configuration, but no longer offer them in the UI.
export const RETIRED_TOOL_IDS: ReadonlySet<ToolCatalogId> = new Set(["trae-work"]);

// This is the canonical/default dock order. Users can move a tool to the end by
// hiding and restoring it. Append newly supported tools; do not insert editions
// next to their siblings and disturb the established default order.
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
  "workbuddy-ai",
  "muse-code",
  "codebuddy",
  "qoder",
  "qoder-cn",
  "octop",
  "toonflow",
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
] as const satisfies readonly ToolCatalogId[];

export function toolCatalogEntry(tool: string): ToolCatalogEntry | null {
  return Object.prototype.hasOwnProperty.call(TOOL_CATALOG, tool)
    ? TOOL_CATALOG[tool as ToolCatalogId]
    : null;
}
