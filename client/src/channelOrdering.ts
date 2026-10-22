type ChannelWithCreationTime = {
  created_at_unix_ms?: number;
};

type ModelMarketChannel = ChannelWithCreationTime & {
  id?: string;
  name?: string;
  source_driver?: string;
  enabled?: boolean;
  share_enabled?: boolean;
};

export function isLegacyLocalSupplierPlaceholder(channel: ModelMarketChannel): boolean {
  return channel.id === "channel-1"
    && channel.name?.trim().toLowerCase() === "local supplier"
    && channel.source_driver === "custom_endpoint"
    && channel.created_at_unix_ms === 0
    && channel.enabled !== true
    && channel.share_enabled !== true;
}

export function channelsVisibleInModelMarket<T extends ModelMarketChannel>(
  channels: readonly T[],
  development: boolean,
): T[] {
  return development
    ? [...channels]
    : channels.filter((channel) => !isLegacyLocalSupplierPlaceholder(channel));
}

export function configuredModelMarketChannelCount<T extends ModelMarketChannel>(
  channels: readonly T[],
): number {
  return channels.filter((channel) => !isLegacyLocalSupplierPlaceholder(channel)).length;
}

export const MAX_LOCAL_PRIORITY = 0xffff_ffff;

export function isValidLocalPriority(value: number): boolean {
  return Number.isInteger(value) && value >= 1 && value <= MAX_LOCAL_PRIORITY;
}

/** Preserve legacy order; newly discovered channels join after the existing tiers. */
export function withLocalPriorities<T extends { id: string; local_priority?: number }>(channels: readonly T[]): (T & { local_priority: number })[] {
  let last = channels.reduce((max, channel) => Math.max(max, channel.local_priority ?? 0), 0);
  return channels.map((channel) => {
    if (channel.local_priority !== undefined) return { ...channel, local_priority: channel.local_priority };
    last = Math.min(last + 1, MAX_LOCAL_PRIORITY);
    return { ...channel, local_priority: last };
  });
}

export function channelsByLocalPriority<T extends { id: string; local_priority?: number }>(channels: readonly T[]) {
  return withLocalPriorities(channels).sort((a, b) => a.local_priority - b.local_priority);
}

/** Patch the latest channels, not the dialog's snapshot of credentials or observations. */
export function applyLocalPriorities<T extends { id: string; local_priority?: number }>(
  channels: readonly T[], priorities: Readonly<Record<string, number>>,
) {
  if (Object.values(priorities).some((value) => !isValidLocalPriority(value))) {
    throw new Error("Local priority must be a positive 32-bit integer");
  }
  return withLocalPriorities(channels)
    .map((channel) => ({ ...channel, local_priority: priorities[channel.id] ?? channel.local_priority }))
    // Also retain a sensible fallback order for adjacent older clients.
    .sort((a, b) => a.local_priority - b.local_priority);
}
