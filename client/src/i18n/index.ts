import i18n, { type TOptions } from "i18next";
import { initReactI18next } from "react-i18next";
import { locale as tauriLocale } from "@tauri-apps/plugin-os";

import enUS from "./locales/en-US.json";
import zhCN from "./locales/zh-CN.json";
import {
  readLanguagePreference,
  resolveDisplayLanguage,
  writeLanguagePreference,
  type SupportedLanguage,
} from "./language";

const resources = {
  "en-US": { translation: enUS },
  "zh-CN": { translation: zhCN },
} as const;

let initialization: Promise<SupportedLanguage> | null = null;

function isTauriRuntime() {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

async function readSystemLocale() {
  if (isTauriRuntime()) {
    try {
      const value = await tauriLocale();
      if (value) return value;
    } catch {
      // The browser fallback keeps renderer-only development usable.
    }
  }
  if (typeof navigator === "undefined") return null;
  return navigator.languages?.[0] ?? navigator.language ?? null;
}

function applyDocumentLanguage(language: SupportedLanguage) {
  if (typeof document !== "undefined") {
    document.documentElement.lang = language;
    document.documentElement.dataset.language = language;
  }
}

export function initializeI18n() {
  initialization ??= (async () => {
    const language = resolveDisplayLanguage(
      readLanguagePreference(),
      await readSystemLocale(),
    );
    await i18n
      .use(initReactI18next)
      .init({
        resources,
        lng: language,
        fallbackLng: "en-US",
        supportedLngs: ["zh-CN", "en-US"],
        load: "currentOnly",
        interpolation: {
          escapeValue: false,
        },
        react: {
          useSuspense: false,
        },
        returnNull: false,
      });
    applyDocumentLanguage(language);
    return language;
  })();
  return initialization;
}

export async function changeAppLanguage(language: SupportedLanguage) {
  writeLanguagePreference(language);
  if (!i18n.isInitialized) await initializeI18n();
  await i18n.changeLanguage(language);
  applyDocumentLanguage(language);
}

export function currentAppLanguage(): SupportedLanguage {
  return i18n.resolvedLanguage === "zh-CN" ? "zh-CN" : "en-US";
}

export function tr(key: string, options?: TOptions) {
  return String(i18n.t(key, options));
}

type ErrorDescriptor = {
  code: string;
  params: Record<string, string | number | boolean>;
};

function translationParams(value: unknown): Record<string, string | number | boolean> {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return Object.fromEntries(
    Object.entries(value)
      .filter(([, item]) => ["string", "number", "boolean"].includes(typeof item))
      .map(([key, item]) => [key, item as string | number | boolean]),
  );
}

function descriptorFromObject(value: unknown): ErrorDescriptor | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const payload = value as {
    code?: unknown;
    params?: unknown;
    error?: unknown;
  };
  const code = String(payload.code ?? "").trim();
  if (code) {
    return {
      code,
      params: translationParams(payload.params),
    };
  }
  return descriptorFromObject(payload.error);
}

function normalizeErrorDescriptor(descriptor: ErrorDescriptor): ErrorDescriptor {
  if (descriptor.code === "try_later") {
    const retryAfter = Number(descriptor.params.retry_after_seconds);
    if (Number.isFinite(retryAfter) && retryAfter > 0) {
      return {
        code: "retry_after",
        params: { ...descriptor.params, seconds: Math.ceil(retryAfter) },
      };
    }
  }
  const accountHttp = descriptor.code.match(/^account_http_(\d{3})$/i);
  if (!accountHttp?.[1]) return descriptor;

  const status = Number(accountHttp[1]);
  const params = { ...descriptor.params, status };
  if (status === 400 || status === 422) return { code: "account_invalid_request", params };
  if (status === 401) return { code: "account_authentication_failed", params };
  if (status === 403) return { code: "account_forbidden", params };
  if (status === 404) return { code: "account_endpoint_not_found", params };
  if (status === 408 || status === 504) return { code: "account_timeout", params };
  if (status === 409) return { code: "account_conflict", params };
  if (status === 429) return { code: "account_rate_limited", params };
  if (status === 502 || status === 503) return { code: "account_service_unreachable", params };
  if (status >= 500) return { code: "account_server_error", params };
  return { code: "account_http_error", params };
}

function errorDescriptor(value: unknown): ErrorDescriptor {
  const message = value instanceof Error ? value.message : String(value ?? "");
  const direct = descriptorFromObject(value);
  if (direct) return direct;
  try {
    const parsed = JSON.parse(message);
    const fromJson = descriptorFromObject(parsed);
    if (fromJson) return fromJson;
  } catch {
    // Plain-text server errors are handled by the stable-code patterns below.
  }
  const objectStart = message.indexOf("{");
  const objectEnd = message.lastIndexOf("}");
  if (objectStart >= 0 && objectEnd > objectStart) {
    try {
      const embedded = descriptorFromObject(JSON.parse(message.slice(objectStart, objectEnd + 1)));
      if (embedded) return embedded;
    } catch {
      // Continue with code-only patterns when a transport prefix contains non-JSON braces.
    }
  }
  const jsonMatch = message.match(/"code"\s*:\s*"([^"]+)"/i);
  if (jsonMatch?.[1]) return { code: jsonMatch[1], params: {} };
  const prefixMatch = message.match(/(?:^|\s)(?:error[_ -]?code|code)\s*[:=]\s*([a-z0-9_.-]+)/i);
  if (prefixMatch?.[1]) return { code: prefixMatch[1], params: {} };
  const plainCode = message.trim();
  if (/^[a-z][a-z0-9]*(?:_[a-z0-9]+)+$/i.test(plainCode)) {
    return { code: plainCode, params: {} };
  }
  const accountHttpMatch = message.match(/\b(account_http_\d{3})\b/i);
  if (accountHttpMatch?.[1]) return { code: accountHttpMatch[1], params: {} };
  const stableCodeMatch = message.match(/\b([A-Z][A-Z0-9]*(?:_[A-Z0-9]+){2,})\b/);
  if (!stableCodeMatch?.[1]) return { code: "", params: {} };
  const detail = message
    .slice((stableCodeMatch.index ?? 0) + stableCodeMatch[1].length)
    .replace(/^\s*[:：-]\s*/, "")
    .trim();
  return {
    code: stableCodeMatch[1],
    params: detail ? { detail } : {},
  };
}

export function localizedError(value: unknown) {
  const raw = value instanceof Error ? value.message : String(value ?? "");
  const { code, params } = normalizeErrorDescriptor(errorDescriptor(value));
  if (!code) return raw;
  const key = `errors.codes.${code.toLowerCase().replace(/-/g, "_")}`;
  if (i18n.exists(key, { lng: currentAppLanguage() })) return tr(key, params);
  if (i18n.exists(key, { lng: "en-US" })) {
    return String(i18n.t(key, { ...params, lng: "en-US" }));
  }
  return raw;
}

export function stableErrorCode(value: unknown): string {
  return normalizeErrorDescriptor(errorDescriptor(value)).code
    .trim()
    .toLowerCase()
    .replace(/-/g, "_");
}

export { default as i18n } from "i18next";
export type { SupportedLanguage } from "./language";
