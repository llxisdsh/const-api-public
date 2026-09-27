import type { ClientConfig } from "./appTypes";

/** Keeps an unsaved access-page draft while accepting authoritative data elsewhere. */
export function overlayAccessConfig(
  authoritative: ClientConfig,
  draft: ClientConfig,
  baseline: ClientConfig,
): ClientConfig {
  const draftValue = <K extends keyof ClientConfig>(key: K) => (
    draft[key] !== baseline[key] ? draft[key] : authoritative[key]
  );
  return {
    ...authoritative,
    listen: draftValue("listen"),
    allow_lan_access: draftValue("allow_lan_access"),
    proxy_auto_start: draftValue("proxy_auto_start"),
    api_key: draftValue("api_key"),
    allow_model_equivalence: draftValue("allow_model_equivalence"),
    prefer_local_supply: draftValue("prefer_local_supply"),
  };
}

function structurallyEqual(left: unknown, right: unknown) {
  return JSON.stringify(left) === JSON.stringify(right);
}

function mergeSupplierChannelDraft(
  authoritative: ClientConfig["channels"],
  draft: ClientConfig["channels"],
  baseline: ClientConfig["channels"],
) {
  const draftIds = new Set(draft.map((channel) => channel.id));
  const deletedIds = new Set(
    baseline.filter((channel) => !draftIds.has(channel.id)).map((channel) => channel.id),
  );
  const merged = authoritative.filter((channel) => !deletedIds.has(channel.id));

  for (const draftChannel of draft) {
    const baselineChannel = baseline.find((channel) => channel.id === draftChannel.id);
    if (baselineChannel && structurallyEqual(draftChannel, baselineChannel)) continue;
    const index = merged.findIndex((channel) => channel.id === draftChannel.id);
    if (index >= 0) merged[index] = draftChannel;
    else merged.push(draftChannel);
  }
  return merged;
}

/** Keeps an unsaved supplier-page draft while accepting authoritative data elsewhere. */
export function overlaySupplierConfig(
  authoritative: ClientConfig,
  draft: ClientConfig,
  baseline: ClientConfig,
): ClientConfig {
  return {
    ...authoritative,
    supplier_auto_start: draft.supplier_auto_start !== baseline.supplier_auto_start
      ? draft.supplier_auto_start
      : authoritative.supplier_auto_start,
    channels: mergeSupplierChannelDraft(
      authoritative.channels,
      draft.channels,
      baseline.channels,
    ),
    supplier: !structurallyEqual(draft.supplier, baseline.supplier)
      ? draft.supplier
      : authoritative.supplier,
  };
}
