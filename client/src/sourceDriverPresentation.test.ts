import { beforeAll, describe, expect, it } from "vitest";

import { changeAppLanguage, initializeI18n } from "./i18n";
import { sourceDriverById } from "./sourceDrivers";
import {
  sourceDriverActionLabel,
  sourceDriverDescription,
  sourceDriverDisplayName,
  sourceDriverTitle,
} from "./sourceDriverPresentation";

beforeAll(async () => {
  await initializeI18n();
});

describe("source driver presentation", () => {
  const driver = sourceDriverById("openai_subscription");

  it("localizes manifest copy without changing the manifest", async () => {
    await changeAppLanguage("en-US");
    expect(sourceDriverTitle(driver)).toBe("OpenAI account subscription");
    expect(sourceDriverDescription(driver)).toContain("ChatGPT");
    expect(sourceDriverActionLabel(driver)).toBe("Connect");
    expect(driver.title).toBe("OpenAI 账号订阅");
  });

  it("keeps custom channel names and localizes only the manifest default", async () => {
    await changeAppLanguage("en-US");
    expect(sourceDriverDisplayName(driver, driver.title)).toBe("OpenAI account subscription");
    expect(sourceDriverDisplayName(driver, "My subscription")).toBe("My subscription");
  });
});
