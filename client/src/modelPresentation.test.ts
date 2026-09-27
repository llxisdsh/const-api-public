import { describe, expect, test } from "vitest";
import { modelContextLabel, modelDisplayName, modelDisplayNames, modelInputOptions } from "./modelPresentation";

describe("model presentation conversion", () => {
  test("context hints share one model without a display suffix", () => {
    expect([...modelDisplayNames(["vendor/Claude[1M]", "vendor/claude"]).values()]).toEqual(["claude"]);
  });
  test("retains context formatting for model specification views", () => {
    expect(modelContextLabel(1000000)).toBe("1M");
    expect(modelContextLabel(1048576)).toBe("1.04M");
    expect(modelContextLabel(undefined)).toBe("");
  });
  test.each([
    ["gpt-5.6-luna", "gpt-5.6-luna"],
    ["qwen/qwen3.8-27b", "qwen3.8-27b"],
    ["anthropic/openai/gpt-5.6-luna", "gpt-5.6-luna"],
    ["thinkingmachines/inkling:free", "inkling:free"],
    ["anthropic/claude-sonnet-5[1m]", "claude-sonnet-5"],
    ["vendor/model-20260909", "model-20260909"],
    ["~vendor/model-latest", "model-latest"],
    ["  vendor/model  ", "model"],
    ["", ""],
    ["vendor/", "vendor/"],
    ["Owner/Qwen3.8-27B", "qwen3.8-27b"],
  ])("converts %s for display without erasing variants", (model, expected) => {
    expect(modelDisplayName(model)).toBe(expected);
  });

  test("uses the same labels in dropdowns and other model views", () => {
    const models = ["vendor-a/model", "gateway/vendor-b/Model", "vendor-a/model:free"];
    const labels = modelDisplayNames(models);
    expect(modelInputOptions(models)).toEqual([
      { value: models[0], label: "model" },
      { value: models[2], label: "model:free" },
    ]);
    expect(modelInputOptions(models).map((option) => option.label)).toEqual([...labels.values()]);
  });

  test("keeps the first complete ID when short names collide", () => {
    expect(modelInputOptions(["a/model", "a/model", "b/model"])).toEqual([
      { value: "a/model", label: "model" },
    ]);
  });

  test("sorts lowercase short labels only after selecting the first source", () => {
    expect(modelInputOptions(["z/Zebra", "a/Beta", "b/beta", "v/Alpha"])).toEqual([
      { value: "v/Alpha", label: "alpha" },
      { value: "a/Beta", label: "beta" },
      { value: "z/Zebra", label: "zebra" },
    ]);
  });

  test("uses the first matching server rule, then short-name order without filtering hidden models", () => {
    const policy = { schema_version: 1, rules: [
      { pattern: "gpt-z", hidden: true },
      { pattern: "gpt-*", hidden: false },
      { pattern: "gpt-b", hidden: false },
      { pattern: "claude-*", hidden: false },
    ] };
    expect([...modelDisplayNames([
      "vendor/zebra", "first/GPT-Z[1m]", "second/gpt-z", "vendor/claude-a", "vendor/gpt-b", "vendor/gpt-a", "vendor/alpha",
    ], policy)]).toEqual([
      ["first/GPT-Z[1m]", "gpt-z"],
      ["vendor/gpt-a", "gpt-a"],
      ["vendor/gpt-b", "gpt-b"],
      ["vendor/claude-a", "claude-a"],
      ["vendor/alpha", "alpha"],
      ["vendor/zebra", "zebra"],
    ]);
  });

  test("uses the first catalog source instead of a colliding saved selection", () => {
    expect(modelInputOptions(["qwen3.8-27b"], "qwen/qwen3.8-27b")).toEqual([
      { value: "qwen3.8-27b", label: "qwen3.8-27b" },
    ]);
  });

  test("retains a missing saved selection only when its short name is unique", () => {
    expect(modelInputOptions(["new/model"], "old/another")).toEqual([
      { value: "old/another", label: "another" },
      { value: "new/model", label: "model" },
    ]);
  });
});
