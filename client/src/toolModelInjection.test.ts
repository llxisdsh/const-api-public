import { describe, expect, it } from "vitest";
import { TOOL_CATALOG } from "./toolCatalog";
import selectionOverrides from "../tool-model-injection-overrides.json";
import { toolSupportsModelInjectionSelection, validToolModelAddition } from "./toolModelInjection";

describe("tool model injection rules", () => {
  it("shares only exceptions with the backend and inherits ordinary catalog behavior", () => {
    for (const [id, select] of Object.entries(selectionOverrides)) {
      const tool = Object.values(TOOL_CATALOG).find((entry) => entry.tool === id);
      expect(tool, id).toBeDefined();
      expect(select, `${id} should not repeat the default`).not.toBe(tool!.modelSyncPolicy === "catalog");
      expect(toolSupportsModelInjectionSelection(tool!)).toBe(select);
    }
    for (const tool of Object.values(TOOL_CATALOG).filter((entry) => !Object.prototype.hasOwnProperty.call(selectionOverrides, entry.tool))) {
      expect(toolSupportsModelInjectionSelection(tool), tool.tool).toBe(tool.modelSyncPolicy === "catalog");
    }
    expect(toolSupportsModelInjectionSelection({ ...TOOL_CATALOG.opencode, tool: "future-catalog-tool" })).toBe(true);
    expect(toolSupportsModelInjectionSelection({ ...TOOL_CATALOG.copilot, tool: "future-selected-tool" })).toBe(false);
  });
  it("keeps compatible, native-discovery, and selected-model tools exempt", () => {
    for (const id of ["claude", "claude-desktop", "claude-science", "gemini", "hermes", "copilot", "open-interpreter", "anythingllm", "open-design", "vibe-trading"] as const) {
      expect(toolSupportsModelInjectionSelection(TOOL_CATALOG[id]), id).toBe(false);
    }
    // Codex's native source is separately excluded by the configuration caller.
    for (const id of ["codex", "trae", "workbuddy", "workbuddy-ai", "opencode"] as const) {
      expect(toolSupportsModelInjectionSelection(TOOL_CATALOG[id]), id).toBe(true);
    }
  });
  it("accepts exact IDs or trailing wildcards, rejecting duplicates and malformed inputs", () => {
    for (const value of ["gpt-next", "local-*", "*"]) expect(validToolModelAddition(value, [])).toBe(true);
    for (const value of ["", "a/b", "a*b", "a?", "a b", "a\\b", "a".repeat(129), "中".repeat(43)]) expect(validToolModelAddition(value, [])).toBe(false);
    expect(validToolModelAddition("gpt-*", ["gpt-*"])).toBe(false);
  });
});
