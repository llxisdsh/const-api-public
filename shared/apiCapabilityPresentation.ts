import type {
  APIExecutionContext,
  APIFamilyContextStatus,
  APIOfficialFamilyContract,
} from "./apiSurfaceContract";

export type APIProductSupport = "supported" | "limited" | "unsupported";

export const API_EXECUTION_CONTEXT_LABELS: Readonly<Record<APIExecutionContext, string>> = {
  api_credential: "API 渠道",
  subscription_driver: "订阅账号",
  platform_supply: "平台供应",
};

export const API_SURFACE_LABELS = {
  openai: "OpenAI",
  anthropic: "Anthropic",
  gemini: "Gemini",
} as const;

const CHINESE_CAPABILITY_NAMES: Readonly<Record<string, string>> = {
  "openai.models": "模型列表",
  "openai.responses": "Responses 与状态管理",
  "openai.chat_completions": "Chat Completions 与已存储对话",
  "openai.legacy_completions": "旧版 Completions",
  "openai.conversations": "会话与内容项",
  "openai.embeddings": "嵌入向量",
  "openai.files_uploads": "文件与上传",
  "openai.batches": "批处理",
  "openai.images": "图片输入与生成",
  "openai.audio": "语音与音频",
  "openai.videos": "视频",
  "openai.realtime": "实时交互",
  "openai.codex_live": "Codex 实时语音（实验性）",
  "openai.moderations": "内容审核",
  "openai.content_provenance": "内容来源验证",
  "openai.vector_stores": "向量库与文件检索",
  "openai.containers": "沙箱容器与文件",
  "openai.skills": "技能与版本",
  "openai.assistants_threads": "Assistants 与 Threads（旧版）",
  "openai.fine_tuning_evals_chatkit": "微调、评测与 ChatKit",
  "openai.management": "组织与项目管理",
  "anthropic.models": "模型列表",
  "anthropic.messages": "消息、工具与令牌计数",
  "anthropic.files": "文件",
  "anthropic.message_batches": "消息批处理",
  "anthropic.skills_agents": "技能、托管 Agent 与运行环境",
  "anthropic.management": "组织与工作区管理",
  "gemini.models": "模型列表",
  "gemini.generation": "内容生成、流式与令牌计数",
  "gemini.interactions": "Interactions 多轮交互",
  "gemini.embeddings": "嵌入向量",
  "gemini.cached_contents": "上下文缓存",
  "gemini.files": "文件",
  "gemini.live": "实时交互",
  "gemini.batches": "批处理",
  "gemini.file_search": "文件检索",
  "gemini.generated_media": "图片、音频、视频与音乐",
  "gemini.operations": "长任务状态",
  "gemini.tuning_corpora_permissions": "模型调优、语料库与权限",
};

export function apiCapabilityName(
  family: APIOfficialFamilyContract,
  language = "zh-CN",
): string {
  if (!language.toLowerCase().startsWith("zh")) return family.name;
  return CHINESE_CAPABILITY_NAMES[family.id] ?? family.name;
}

export function apiProductSupport(status: APIFamilyContextStatus): APIProductSupport {
  switch (status.coverage) {
    case "complete":
      return "supported";
    case "partial":
    case "transport_only":
      return "limited";
    case "none":
    case "reserved":
      return "unsupported";
  }
}

export function apiProductSupportLabel(
  support: APIProductSupport,
  language = "zh-CN",
): string {
  const chinese = language.toLowerCase().startsWith("zh");
  if (support === "supported") return chinese ? "支持" : "Supported";
  if (support === "limited") return chinese ? "有限支持" : "Limited";
  return chinese ? "未支持" : "Not supported";
}

export function apiCoverageExplanation(
  status: APIFamilyContextStatus,
  language = "zh-CN",
): string {
  const chinese = language.toLowerCase().startsWith("zh");
  if (chinese) {
    switch (status.coverage) {
      case "complete":
        return "当前版本完整覆盖";
      case "partial":
        return "支持主要调用，部分扩展操作暂不可用";
      case "transport_only":
        return "当前版本仅支持同协议 API 渠道原生转发";
      case "none":
        return "当前版本尚未提供";
      case "reserved":
        return "由独立管理 API 提供";
    }
  }
  switch (status.coverage) {
    case "complete":
      return "Fully covered by this release";
    case "partial":
      return "Core requests are supported; some extended operations are unavailable";
    case "transport_only":
      return "Native pass-through for same-protocol API channels only";
    case "none":
      return "Not available in this release";
    case "reserved":
      return "Available through a separate management API";
  }
}

export function apiTechnicalStatus(status: APIFamilyContextStatus): string {
  return `${status.conversion.toUpperCase()} · ${status.maturity.toUpperCase()} · ${status.evidence}`;
}
