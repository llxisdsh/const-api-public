import { describe, expect, it } from "vitest";
import {
  applyDocumentTheme,
  detectPlatform,
  normalizeThemePreference,
  readThemePreference,
  resolveTheme,
  writeThemePreference,
} from "./theme";

describe("theme preferences", () => {
  it("defaults to light while preserving an explicit system preference", () => {
    expect(normalizeThemePreference(null)).toBe("light");
    expect(normalizeThemePreference("sepia")).toBe("light");
    expect(normalizeThemePreference("system")).toBe("system");
    expect(readThemePreference({ getItem: () => null })).toBe("light");
    expect(readThemePreference({ getItem: () => "system" })).toBe("system");
  });

  it("resolves the system preference without changing explicit choices", () => {
    expect(resolveTheme("system", true)).toBe("dark");
    expect(resolveTheme("system", false)).toBe("light");
    expect(resolveTheme("light", true)).toBe("light");
  });

  it("persists and applies a preference", () => {
    const values = new Map<string, string>();
    const root = document.createElement("html");
    writeThemePreference("dark", { setItem: (key, value) => values.set(key, value) });
    expect(applyDocumentTheme("dark", false, root)).toBe("dark");
    expect(root.dataset.theme).toBe("dark");
    expect(root.dataset.themePreference).toBe("dark");
    expect(Array.from(values.values())).toEqual(["dark"]);
  });

  it("detects the desktop platform used for restrained platform-specific chrome", () => {
    expect(detectPlatform("Mozilla/5.0 (Windows NT 10.0; Win64; x64)")).toBe("windows");
    expect(detectPlatform("Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0)")).toBe("macos");
  });
});
