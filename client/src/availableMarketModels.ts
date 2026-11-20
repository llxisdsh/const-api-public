import { modelDisplayName } from "./modelPresentation";
import type { MarketModelPrice } from "./MarketPricesModal";

export const directoryOperations = {
  generate: ["openai.images_generations", "gemini.generate_content.image"],
  edit: ["openai.images_edits", "gemini.generate_content.image_edit"],
  video: ["openai.videos_create"],
  speech: ["openai.audio_speech"],
  music: ["openai.chat_completions.music"],
} as const;
export const outputKinds = { imageOutput: "image", videoOutput: "video", audioOutput: "audio" } as const;
export type DirectoryOperation = keyof typeof directoryOperations;
export type DirectoryFilter = DirectoryOperation | keyof typeof outputKinds | "imageInput" | "all";
export const operationKeys = Object.keys(directoryOperations) as DirectoryOperation[];
export const outputKeys = Object.keys(outputKinds) as (keyof typeof outputKinds)[];

type DirectoryTraits = {
  imageInput: boolean;
  outputs: string[];
  contextTokens?: number | null;
};
export type DirectoryModel = DirectoryTraits & {
  id: string;
  local: string[] | null;
  platform: string[] | null;
  localMedia?: DirectoryTraits;
  platformMedia?: DirectoryTraits;
};

export type PlatformDirectoryEntry = {
  id: string; operations?: string[]; imageInput?: boolean; outputs?: string[]; contextTokens?: number;
};

export function mergeMarketDirectory(local: DirectoryModel[], platform: PlatformDirectoryEntry[]): DirectoryModel[] {
  const models = new Map<string, DirectoryModel>(
    local.map((entry) => [entry.id, { ...entry, localMedia: entry }]),
  );
  for (const item of platform) {
    const traits = { imageInput: !!item.imageInput, outputs: item.outputs ?? [], contextTokens: item.contextTokens };
    const entry = models.get(item.id);
    if (entry) {
      entry.platform = item.operations ?? [];
      entry.platformMedia = traits;
      entry.imageInput ||= traits.imageInput;
      entry.outputs = [...new Set([...entry.outputs, ...traits.outputs])];
      entry.contextTokens = entry.contextTokens && traits.contextTokens
        ? Math.min(entry.contextTokens, traits.contextTokens) : entry.contextTokens ?? traits.contextTokens;
    } else {
      models.set(item.id, { id: item.id, local: null, platform: item.operations ?? [], ...traits, platformMedia: traits });
    }
  }
  return [...models.values()];
}

export function supportsOperation(operations: string[] | null | undefined, key: DirectoryOperation) {
  return directoryOperations[key].some((op) => operations?.includes(op));
}

export function sourceMatches(entry: DirectoryModel | undefined, side: "local" | "platform", purpose: DirectoryFilter) {
  if (entry?.[side] == null) return false;
  if (purpose === "all") return true;
  if (purpose in directoryOperations) return supportsOperation(entry[side], purpose as DirectoryOperation);
  const traits = (side === "local" ? entry.localMedia : entry.platformMedia)
    // Older combined directories only attribute modalities when there is a single source.
    ?? (entry.local == null || entry.platform == null ? entry : undefined);
  if (purpose === "imageInput") return !!traits?.imageInput;
  return !!traits?.outputs?.includes(outputKinds[purpose as keyof typeof outputKinds]);
}

function matchesPurpose(entry: DirectoryModel | undefined, purpose: DirectoryFilter) {
  if (purpose === "all") return true;
  if (!entry) return false;
  if (purpose === "imageInput") return entry.imageInput;
  if (purpose in outputKinds) return entry.outputs?.includes(outputKinds[purpose as keyof typeof outputKinds]);
  const key = purpose as DirectoryOperation;
  return supportsOperation(entry.local, key) || supportsOperation(entry.platform, key);
}

export function capabilityLabels(entry: DirectoryModel | undefined, local: boolean, platform: boolean) {
  if (!entry) return [];
  const traits = (local && !platform ? entry.localMedia : platform && !local ? entry.platformMedia : undefined) ?? entry;
  const operations = operationKeys.filter((op) =>
    (local && supportsOperation(entry.local, op)) || (platform && supportsOperation(entry.platform, op)));
  const impliedOutputs = new Set(operations.map((op) =>
    op === "generate" || op === "edit" ? "image" : op === "video" ? "video" : "audio"));
  return [
    ...(traits.imageInput ? ["imageInput"] : []), ...operations,
    ...outputKeys.filter((key) => traits.outputs?.includes(outputKinds[key]) && !impliedOutputs.has(outputKinds[key])),
  ];
}

export type CatalogEntry = { id: string; market: boolean; directory?: DirectoryModel };

// Both producers use the public request ID. Keep full IDs: equal display
// names from different vendors do not establish a shared price or capability.
export function filterAvailableModels(
  marketIds: readonly string[], directory: readonly DirectoryModel[], purpose: DirectoryFilter,
  query: { query: string | null; sort: string; order: string },
) {
  const rows = new Map<string, CatalogEntry>();
  for (const id of marketIds) rows.set(id, { id, market: true });
  for (const entry of directory) {
    const row = rows.get(entry.id);
    if (row) row.directory = entry;
    else rows.set(entry.id, { id: entry.id, market: false, directory: entry });
  }
  const needle = query.query?.trim().toLowerCase() ?? "";
  const matches = [...rows.values()].filter(({ id, directory }) =>
    (!needle || id.toLowerCase().includes(needle)) && matchesPurpose(directory, purpose));
  // Keep the server's price/rank order. Unquoted entries follow it. For an
  // explicit name sort, interleave local and market IDs across page boundaries.
  if (query.sort === "model" && query.order !== "default") {
    const direction = query.order === "desc" ? -1 : 1;
    matches.sort((a, b) => {
      const left = modelDisplayName(a.id), right = modelDisplayName(b.id);
      return direction * (left < right ? -1 : left > right ? 1 : 0);
    });
  }
  return { matches, total: rows.size };
}

export function availableModel(entry: CatalogEntry, price?: MarketModelPrice): MarketModelPrice {
  if (price) return { ...price, directory: entry.directory };
  return {
    model: entry.id, directory: entry.directory, directoryOnly: true, currency: "",
    ready_supplier_count: 0, recent: { transaction_count: 0, supplier_count: 0 },
  };
}
