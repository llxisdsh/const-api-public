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

export function moveChannelInVisibleOrder<T extends { id: string }>(
  channels: readonly T[],
  visibleChannelIds: readonly string[],
  channelId: string,
  direction: -1 | 1,
): T[] | null {
  const visibleIndex = visibleChannelIds.indexOf(channelId);
  const targetId = visibleChannelIds[visibleIndex + direction];
  if (visibleIndex < 0 || !targetId) return null;

  const channelIndex = channels.findIndex((channel) => channel.id === channelId);
  const targetIndex = channels.findIndex((channel) => channel.id === targetId);
  if (channelIndex < 0 || targetIndex < 0) return null;

  const reordered = [...channels];
  [reordered[channelIndex], reordered[targetIndex]] = [
    reordered[targetIndex],
    reordered[channelIndex],
  ];
  return reordered;
}
