import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach, beforeAll } from "vitest";

import { changeAppLanguage, initializeI18n } from "../i18n";

beforeAll(async () => {
  await initializeI18n();
  await changeAppLanguage("zh-CN");
});

afterEach(() => {
  cleanup();
});
