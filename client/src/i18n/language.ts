export const LANGUAGE_STORAGE_KEY = "const-api:language:v1";

export const SUPPORTED_LANGUAGES = ["zh-CN", "en-US"] as const;

export type SupportedLanguage = (typeof SUPPORTED_LANGUAGES)[number];

export function normalizeSupportedLanguage(value: string | null | undefined): SupportedLanguage | null {
  if (!value) return null;
  const normalized = value.trim().replace(/_/g, "-").toLowerCase();
  if (normalized === "zh-cn") return "zh-CN";
  if (normalized === "en-us") return "en-US";
  return null;
}

export function systemLanguageToSupported(value: string | null | undefined): SupportedLanguage {
  if (!value) return "en-US";
  const normalized = value
    .trim()
    .replace(/_/g, "-")
    .replace(/\..*$/, "")
    .toLowerCase();
  if (
    normalized === "zh"
    || normalized.startsWith("zh-cn")
    || normalized.startsWith("zh-sg")
    || normalized.startsWith("zh-hans")
  ) {
    return "zh-CN";
  }
  return "en-US";
}

export function readLanguagePreference(
  storage: Pick<Storage, "getItem"> | undefined = safeStorage(),
): SupportedLanguage | null {
  try {
    return normalizeSupportedLanguage(storage?.getItem(LANGUAGE_STORAGE_KEY));
  } catch {
    return null;
  }
}

export function writeLanguagePreference(
  language: SupportedLanguage,
  storage: Pick<Storage, "setItem"> | undefined = safeStorage(),
) {
  try {
    storage?.setItem(LANGUAGE_STORAGE_KEY, language);
  } catch {
    // Language persistence is optional; the active session can still switch.
  }
}

export function resolveDisplayLanguage(
  preference: SupportedLanguage | null,
  systemLocale: string | null | undefined,
): SupportedLanguage {
  return preference ?? systemLanguageToSupported(systemLocale);
}

function safeStorage() {
  return typeof window === "undefined" ? undefined : window.localStorage;
}
