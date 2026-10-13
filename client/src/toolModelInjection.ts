import type { ToolCatalogEntry } from "./toolCatalog";
import selectionOverrides from "../tool-model-injection-overrides.json";

// Shared with Rust; ordinary catalogs inherit selection from their sync policy.
const toolSelectionOverrides = new Map<string, boolean>(Object.entries(selectionOverrides));

export type ToolModelInjectionSettings = {
  all_models: boolean;
  extra_models: string[];
};

export type ToolModelRecommendations = { patterns: string[]; available_models: string[] };
export type ToolModelSyncResult = { synced: number; errors: string[] };
export const DEFAULT_TOOL_MODEL_INJECTION: ToolModelInjectionSettings = { all_models: true, extra_models: [] };

export function validToolModelAddition(pattern: string, existing: readonly string[]) {
  return Boolean(pattern) && new TextEncoder().encode(pattern).length <= 128
    && !/[*\/?\\\s]/.test(pattern.replace(/\*$/, "")) && !existing.includes(pattern);
}

export function toolSupportsModelInjectionSelection(tool: ToolCatalogEntry) {
  return toolSelectionOverrides.get(tool.tool) ?? (tool.modelSyncPolicy === "catalog");
}
