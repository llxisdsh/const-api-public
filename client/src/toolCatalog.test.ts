import { describe, expect, test } from "vitest";
import {
  DEFAULT_VISIBLE_TOOL_IDS,
  TOOL_CATALOG,
  TOOL_CATALOG_ORDER,
  toolCatalogEntry,
} from "./toolCatalog";

describe("tool catalog", () => {
  test("keeps every supported tool on an official HTTPS install source", () => {
    const officialHosts = new Set([
      "anythingllm.com",
      "chatgpt.com",
      "claude.com",
      "docs.github.com",
      "geminicli.com",
      "github.com",
      "block.github.io",
      "opencode.ai",
      "openclaw.ai",
      "raven.evermind.ai",
      "www.deepseek.com",
      "hermes-agent.nousresearch.com",
      "openinterpreter.com",
      "open-design.ai",
      "code.visualstudio.com",
      "www.codebuddy.cn",
      "zcode.z.ai",
    ]);
    expect(Object.keys(TOOL_CATALOG)).toHaveLength(27);
    for (const entry of Object.values(TOOL_CATALOG)) {
      expect(entry.title).toBeTruthy();
      expect(entry.description.length).toBeGreaterThan(8);
      expect(entry.protocols).toContain(entry.defaultProtocol);
      expect(new Set(entry.protocols).size).toBe(entry.protocols.length);
      expect(["none", "selected", "catalog"]).toContain(entry.modelSyncPolicy);
      expect(entry.officialLinks.length).toBeGreaterThan(0);
      for (const link of entry.officialLinks) {
        const url = new URL(link.url);
        expect(url.protocol).toBe("https:");
        expect(officialHosts.has(url.hostname)).toBe(true);
      }
    }
  });

  test("uses the selected ai4s Open Science project rather than the similarly named CLI", () => {
    expect(TOOL_CATALOG.openscience.title).toBe("Open Science");
    expect(TOOL_CATALOG.openscience.officialLinks).toEqual([
      { label: "GitHub", url: "https://github.com/ai4s-research/open-science" },
    ]);
  });

  test("does not resolve unknown or inherited object keys", () => {
    expect(toolCatalogEntry("opencode")).toBe(TOOL_CATALOG.opencode);
    expect(toolCatalogEntry("toString")).toBeNull();
    expect(toolCatalogEntry("unknown")).toBeNull();
  });

  test("keeps managed integrations without advertising unsupported Trae tools", () => {
    expect(toolCatalogEntry("trae")).toBeNull();
    expect(toolCatalogEntry("trae-cn")).toBeNull();
    expect(TOOL_CATALOG.anythingllm.modelSyncPolicy).toBe("selected");
    expect(TOOL_CATALOG["open-interpreter"].officialLinks).toContainEqual({
      label: "GitHub", url: "https://github.com/openinterpreter/openinterpreter",
    });
  });

  test("presents the ChatGPT-backed desktop integration as Codex", () => {
    expect(TOOL_CATALOG.codex.title).toBe("Codex");
    expect(TOOL_CATALOG.codex.officialLinks).toContainEqual({
      label: "GitHub",
      url: "https://github.com/openai/codex",
    });
  });

  test("keeps every managed tool exactly once in the fixed dock order", () => {
    expect(TOOL_CATALOG.workbuddy.tool).toBe("workbuddy");
    expect(new Set(TOOL_CATALOG_ORDER).size).toBe(TOOL_CATALOG_ORDER.length);
    expect(new Set(TOOL_CATALOG_ORDER)).toEqual(new Set(Object.keys(TOOL_CATALOG)));
    expect(TOOL_CATALOG_ORDER.slice(0, DEFAULT_VISIBLE_TOOL_IDS.length))
      .toEqual(DEFAULT_VISIBLE_TOOL_IDS);
    expect(DEFAULT_VISIBLE_TOOL_IDS).toEqual([
      "codex",
      "claude",
      "claude-desktop",
      "vscode",
      "opencode",
      "hermes",
      "deepseek-harness",
      "workbuddy",
      "openclaw",
    ]);
    expect(DEFAULT_VISIBLE_TOOL_IDS).not.toContain("gemini");
    expect(DEFAULT_VISIBLE_TOOL_IDS).not.toContain("copilot");
    expect(DEFAULT_VISIBLE_TOOL_IDS).not.toContain("cline");
    expect(DEFAULT_VISIBLE_TOOL_IDS).not.toContain("raven");
    expect(DEFAULT_VISIBLE_TOOL_IDS).not.toContain("pi");
    expect(TOOL_CATALOG_ORDER[TOOL_CATALOG_ORDER.length - 1]).toBe("zcode");
    expect(DEFAULT_VISIBLE_TOOL_IDS.every((tool) => TOOL_CATALOG_ORDER.includes(tool))).toBe(true);
  });

  test("keeps Claude Science as a managed Anthropic tool", () => {
    expect(TOOL_CATALOG["claude-desktop"].title).toBe("Claude Desktop");
    expect(TOOL_CATALOG["claude-science"]).toMatchObject({
      tool: "claude-science",
      title: "Claude Science",
    });
  });

  test("keeps DeepSeek Harness available in the default dock", () => {
    expect(TOOL_CATALOG["deepseek-harness"]).toMatchObject({
      tool: "deepseek-harness",
      title: "DeepSeek Harness",
    });
    expect(TOOL_CATALOG["deepseek-harness"].officialLinks[0]?.url).toBe(
      "https://www.deepseek.com/harness/",
    );
    expect(DEFAULT_VISIBLE_TOOL_IDS).toContain("deepseek-harness");
  });
});
