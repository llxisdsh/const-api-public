import {
  DEFAULT_VISIBLE_TOOL_IDS,
  TOOL_CATALOG_ORDER,
  type ToolCatalogId,
} from "./toolCatalog";

export const TOOL_VISIBILITY_STORAGE_KEY = "const-api.tool-dock.hidden.v1";
export const TOOL_ORDER_STORAGE_KEY = "const-api.tool-dock.order.v1";

const knownToolIds = new Set<string>(TOOL_CATALOG_ORDER);
const defaultVisibleToolIds = new Set<string>(DEFAULT_VISIBLE_TOOL_IDS);

export function defaultHiddenToolIds(): Set<ToolCatalogId> {
  return new Set(TOOL_CATALOG_ORDER.filter((tool) => !defaultVisibleToolIds.has(tool)));
}

export function parseHiddenToolIds(raw: string | null): Set<ToolCatalogId> {
  if (!raw) return defaultHiddenToolIds();
  try {
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) return defaultHiddenToolIds();
    return new Set(parsed.filter(
      (tool): tool is ToolCatalogId => typeof tool === "string" && knownToolIds.has(tool),
    ));
  } catch {
    return defaultHiddenToolIds();
  }
}

export function readHiddenToolIds(): Set<ToolCatalogId> {
  if (typeof window === "undefined") return defaultHiddenToolIds();
  return parseHiddenToolIds(window.localStorage.getItem(TOOL_VISIBILITY_STORAGE_KEY));
}

export function serializeHiddenToolIds(hidden: ReadonlySet<string>): string {
  return JSON.stringify(TOOL_CATALOG_ORDER.filter((tool) => hidden.has(tool)));
}

export function writeHiddenToolIds(hidden: ReadonlySet<string>) {
  if (typeof window === "undefined") return;
  window.localStorage.setItem(TOOL_VISIBILITY_STORAGE_KEY, serializeHiddenToolIds(hidden));
}

export function defaultToolOrder(): ToolCatalogId[] {
  return [...TOOL_CATALOG_ORDER];
}

export function normalizeToolOrder(order: readonly unknown[]): ToolCatalogId[] {
  const seen = new Set<ToolCatalogId>();
  const normalized: ToolCatalogId[] = [];

  for (const tool of order) {
    if (typeof tool !== "string" || !knownToolIds.has(tool)) continue;
    const knownTool = tool as ToolCatalogId;
    if (seen.has(knownTool)) continue;
    seen.add(knownTool);
    normalized.push(knownTool);
  }

  for (const tool of TOOL_CATALOG_ORDER) {
    if (seen.has(tool)) continue;
    seen.add(tool);
    normalized.push(tool);
  }

  return normalized;
}

export function parseToolOrder(raw: string | null): ToolCatalogId[] {
  if (!raw) return defaultToolOrder();
  try {
    const parsed = JSON.parse(raw) as unknown;
    return Array.isArray(parsed) ? normalizeToolOrder(parsed) : defaultToolOrder();
  } catch {
    return defaultToolOrder();
  }
}

export function readToolOrder(): ToolCatalogId[] {
  if (typeof window === "undefined") return defaultToolOrder();
  return parseToolOrder(window.localStorage.getItem(TOOL_ORDER_STORAGE_KEY));
}

export function serializeToolOrder(order: readonly unknown[]): string {
  return JSON.stringify(normalizeToolOrder(order));
}

export function writeToolOrder(order: readonly unknown[]) {
  if (typeof window === "undefined") return;
  window.localStorage.setItem(TOOL_ORDER_STORAGE_KEY, serializeToolOrder(order));
}

export function moveToolToEnd(
  order: readonly unknown[],
  tool: ToolCatalogId,
): ToolCatalogId[] {
  return [...normalizeToolOrder(order).filter((candidate) => candidate !== tool), tool];
}

export function isDefaultToolOrder(order: readonly unknown[]): boolean {
  const normalized = normalizeToolOrder(order);
  return normalized.every((tool, index) => tool === TOOL_CATALOG_ORDER[index]);
}
