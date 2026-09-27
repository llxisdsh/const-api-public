export type ThemePreference = "system" | "light" | "dark";
export type ResolvedTheme = "light" | "dark";

export const THEME_STORAGE_KEY = "const-api:theme";

export function normalizeThemePreference(value: string | null | undefined): ThemePreference {
  return value === "system" || value === "light" || value === "dark" ? value : "light";
}

export function resolveTheme(preference: ThemePreference, systemPrefersDark: boolean): ResolvedTheme {
  if (preference === "system") return systemPrefersDark ? "dark" : "light";
  return preference;
}

export function systemPrefersDark() {
  try {
    return typeof window !== "undefined"
      && typeof window.matchMedia === "function"
      && window.matchMedia("(prefers-color-scheme: dark)").matches;
  } catch {
    return false;
  }
}

export function readThemePreference(storage: Pick<Storage, "getItem"> | undefined = safeStorage()): ThemePreference {
  try {
    return normalizeThemePreference(storage?.getItem(THEME_STORAGE_KEY));
  } catch {
    return "light";
  }
}

export function writeThemePreference(
  preference: ThemePreference,
  storage: Pick<Storage, "setItem"> | undefined = safeStorage(),
) {
  try {
    storage?.setItem(THEME_STORAGE_KEY, preference);
  } catch {
    // Theme persistence is optional; the active session can still use the selected theme.
  }
}

export function applyDocumentTheme(
  preference: ThemePreference,
  systemPrefersDark: boolean,
  root: HTMLElement | undefined = safeDocumentRoot(),
): ResolvedTheme {
  const resolved = resolveTheme(preference, systemPrefersDark);
  if (root) {
    root.dataset.theme = resolved;
    root.dataset.themePreference = preference;
  }
  return resolved;
}

export function detectPlatform(userAgent = safeUserAgent()) {
  if (/windows/i.test(userAgent)) return "windows";
  if (/macintosh|mac os/i.test(userAgent)) return "macos";
  if (/linux/i.test(userAgent)) return "linux";
  return "unknown";
}

function safeStorage() {
  return typeof window === "undefined" ? undefined : window.localStorage;
}

function safeDocumentRoot() {
  return typeof document === "undefined" ? undefined : document.documentElement;
}

function safeUserAgent() {
  return typeof navigator === "undefined" ? "" : navigator.userAgent;
}
