import { describe, expect, it } from "vitest";

import {
  normalizeSupportedLanguage,
  readLanguagePreference,
  resolveDisplayLanguage,
  systemLanguageToSupported,
  writeLanguagePreference,
} from "./language";

describe("display language resolution", () => {
  it.each(["zh", "zh-CN", "zh_CN.UTF-8", "zh-Hans", "zh-Hans-CN", "zh-SG"])(
    "maps simplified Chinese locale %s to zh-CN",
    (locale) => expect(systemLanguageToSupported(locale)).toBe("zh-CN"),
  );

  it.each(["en", "en-GB", "ja-JP", "zh-TW", "zh-Hant", null])(
    "falls unsupported locale %s back to English",
    (locale) => expect(systemLanguageToSupported(locale)).toBe("en-US"),
  );

  it("lets an explicit preference override the system locale", () => {
    expect(resolveDisplayLanguage("en-US", "zh-CN")).toBe("en-US");
    expect(resolveDisplayLanguage("zh-CN", "en-US")).toBe("zh-CN");
  });

  it("persists only supported display languages", () => {
    const values = new Map<string, string>();
    const storage = {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => values.set(key, value),
    };
    writeLanguagePreference("zh-CN", storage);
    expect(readLanguagePreference(storage)).toBe("zh-CN");
    values.set("const-api:language:v1", "fr-FR");
    expect(readLanguagePreference(storage)).toBeNull();
    expect(normalizeSupportedLanguage("en-US")).toBe("en-US");
  });
});
