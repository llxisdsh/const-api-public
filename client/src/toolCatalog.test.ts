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
      "www.kimi.com",
      "zcode.z.ai",
      "www.trae.ai",
      "www.trae.cn",
    ]);
    expect(Object.keys(TOOL_CATALOG)).toHaveLength(33);
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

  test("shares desktop and terminal configuration through one canonical tool entry", () => {
    for (const tool of ["cline", "kimicode", "qwencode"] as const) {
      expect(TOOL_CATALOG[tool].modelSyncPolicy).toBe("catalog");
      expect(TOOL_CATALOG[tool].description).toContain("desktop and terminal");
      expect(TOOL_CATALOG_ORDER.filter((id) => id === tool)).toHaveLength(1);
    }
    expect(TOOL_CATALOG.cline.title).toBe("Cline");
  });

  test("keeps Copilot desktop separate from CLI configuration", () => {
    expect(TOOL_CATALOG["copilot-desktop"].protocolScope).toBe("model");
    expect(TOOL_CATALOG["copilot-desktop"].modelSyncPolicy).toBe("catalog");
    expect(TOOL_CATALOG["copilot-desktop"].protocols).toContain("anthropic_messages");
    expect(TOOL_CATALOG.copilot.modelSyncPolicy).toBe("selected");
  });

  test("only labels parallel CLI and CN editions that need disambiguation", () => {
    expect(TOOL_CATALOG_ORDER.flatMap((tool) => {
      const badge = toolCatalogEntry(tool)?.badge;
      return badge ? [[tool, badge]] : [];
    })).toEqual([
      ["copilot", "CLI"],
      ["trae-cn", "CN"],
    ]);
  });

  test("does not resolve unknown or inherited object keys", () => {
    expect(toolCatalogEntry("opencode")).toBe(TOOL_CATALOG.opencode);
    expect(toolCatalogEntry("toString")).toBeNull();
    expect(toolCatalogEntry("unknown")).toBeNull();
  });

  test("uses model-level Grok protocols and provider-level MiniMax protocols", () => {
    expect(TOOL_CATALOG["grok-build"].protocolScope).toBe("model");
    expect(toolCatalogEntry("minimax-code")?.protocolScope).not.toBe("model");
    expect(TOOL_CATALOG["minimax-code"].defaultProtocol).toBe("anthropic_messages");
    for (const tool of ["grok-build", "minimax-code"] as const) {
      expect(TOOL_CATALOG[tool].modelSyncPolicy).toBe("catalog");
      expect(TOOL_CATALOG[tool].protocols).toHaveLength(3);
    }
  });

  test("keeps Trae editions separately managed with per-model protocols", () => {
    for (const tool of ["trae", "trae-cn", "trae-work"] as const) {
      expect(TOOL_CATALOG[tool].protocolScope).toBe("model");
      expect(TOOL_CATALOG[tool].protocols).toEqual(["openai_chat", "openai_responses", "anthropic_messages"]);
      expect(TOOL_CATALOG[tool].modelSyncPolicy).toBe("catalog");
    }
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

  test("distinguishes fork and partial model-level protocol contracts", () => {
    expect(TOOL_CATALOG.mimocode.protocolScope).toBe("model");
    expect(TOOL_CATALOG.mimocode.protocols).toHaveLength(4);
    expect(TOOL_CATALOG.openscience.protocolScope).toBe("model");
    expect(TOOL_CATALOG.openscience.protocols).toEqual(["openai_chat", "openai_responses"]);
    expect(TOOL_CATALOG.kimicode.protocolScope).toBe("model");
    // Only Anthropic can override Kimi's provider; this menu chooses the
    // provider fallback, not a pretend four-protocol per-model schema.
    expect(TOOL_CATALOG.kimicode.protocols).toEqual(["openai_responses", "openai_chat"]);
    expect(toolCatalogEntry("qwencode")?.protocolScope).toBeUndefined();
    expect(toolCatalogEntry("cline")?.protocolScope).toBeUndefined();
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
