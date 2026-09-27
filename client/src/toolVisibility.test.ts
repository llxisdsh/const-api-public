import { describe, expect, test } from "vitest";
import { DEFAULT_VISIBLE_TOOL_IDS, TOOL_CATALOG_ORDER } from "./toolCatalog";
import {
  defaultToolOrder,
  defaultHiddenToolIds,
  isDefaultToolOrder,
  moveToolToEnd,
  parseHiddenToolIds,
  parseToolOrder,
  serializeHiddenToolIds,
  serializeToolOrder,
} from "./toolVisibility";

describe("tool dock visibility", () => {
  test("starts with a compact default set and keeps the rest restorable", () => {
    const hidden = defaultHiddenToolIds();
    expect(TOOL_CATALOG_ORDER.filter((tool) => !hidden.has(tool))).toEqual(DEFAULT_VISIBLE_TOOL_IDS);
    expect(hidden.has("opencode")).toBe(false);
    expect(hidden.has("hermes")).toBe(false);
    expect(hidden.has("workbuddy")).toBe(false);
    expect(hidden.has("deepseek-harness")).toBe(false);
    expect(hidden.has("gemini")).toBe(true);
    expect(hidden.has("copilot")).toBe(true);
    expect(hidden.has("cline")).toBe(true);
    expect(hidden.has("raven")).toBe(true);
    expect(hidden.has("pi")).toBe(true);
    expect(hidden.has("open-design")).toBe(true);
  });

  test("ignores unknown ids and serializes hidden tools in canonical order", () => {
    const hidden = parseHiddenToolIds(JSON.stringify(["zcode", "unknown", "copilot", "zcode"]));
    expect([...hidden]).toEqual(["zcode", "copilot"]);
    expect(serializeHiddenToolIds(hidden)).toBe(JSON.stringify(["copilot", "zcode"]));
  });

  test("falls back safely when stored visibility is invalid", () => {
    expect(parseHiddenToolIds("not-json")).toEqual(defaultHiddenToolIds());
    expect(parseHiddenToolIds(JSON.stringify({ hidden: ["raven"] }))).toEqual(defaultHiddenToolIds());
  });

  test("restores stored order while appending newly supported tools", () => {
    const stored = parseToolOrder(JSON.stringify(["zcode", "codex", "unknown", "zcode"]));

    expect(stored.slice(0, 2)).toEqual(["zcode", "codex"]);
    expect(stored).toHaveLength(TOOL_CATALOG_ORDER.length);
    expect(new Set(stored).size).toBe(TOOL_CATALOG_ORDER.length);
    expect(stored.slice(2)).toEqual(
      TOOL_CATALOG_ORDER.filter((tool) => tool !== "zcode" && tool !== "codex"),
    );
  });

  test("moves a restored tool to the dock end without disturbing the others", () => {
    const moved = moveToolToEnd(TOOL_CATALOG_ORDER, "codex");

    expect(moved[moved.length - 1]).toBe("codex");
    expect(moved.slice(0, -1)).toEqual(TOOL_CATALOG_ORDER.filter((tool) => tool !== "codex"));
    expect(isDefaultToolOrder(moved)).toBe(false);
    expect(isDefaultToolOrder(defaultToolOrder())).toBe(true);
    expect(serializeToolOrder(moved)).toBe(JSON.stringify(moved));
  });

  test("falls back safely when stored order is invalid", () => {
    expect(parseToolOrder("not-json")).toEqual(defaultToolOrder());
    expect(parseToolOrder(JSON.stringify({ order: ["zcode"] }))).toEqual(defaultToolOrder());
  });
});
