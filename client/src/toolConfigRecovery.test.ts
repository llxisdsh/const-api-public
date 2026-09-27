import { describe, expect, test } from "vitest";

import { toolConfigCanLaunchExistingAfterFailure } from "./toolConfigRecovery";

describe("tool configuration recovery", () => {
  test("allows fallback only for the backend-verified existing-model case", () => {
    expect(toolConfigCanLaunchExistingAfterFailure(
      "TOOL_CONFIG_MODEL_REFRESH_USE_EXISTING: VS Code",
    )).toBe(true);
    expect(toolConfigCanLaunchExistingAfterFailure(
      "TOOL_CONFIG_MODELS_UNAVAILABLE: VS Code",
    )).toBe(false);
    expect(toolConfigCanLaunchExistingAfterFailure("network error")).toBe(false);
  });
});
