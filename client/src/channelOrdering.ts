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

/** Patch the latest channels, not a stale UI snapshot of credentials or observations. */
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

export type PriorityDropTarget =
  | { kind: "join"; channelId: string }
  | { kind: "insert"; beforeChannelId: string | null };

/** Plan a row move using every channel, including channels hidden by the view. */
export function prioritiesForChannelDrop(
  channels: readonly { id: string; local_priority?: number }[],
  channelId: string,
  target: PriorityDropTarget,
): Record<string, number> {
  const ordered = channelsByLocalPriority(channels);
  const source = ordered.find((channel) => channel.id === channelId);
  if (!source) return {};
  if (target.kind === "join") {
    const peer = ordered.find((channel) => channel.id === target.channelId);
    if (!peer || peer.local_priority === source.local_priority) return {};
  }
  const groups: { priority: number; ids: string[] }[] = [];
  for (const channel of ordered) {
    const last = groups[groups.length - 1];
    if (last?.priority === channel.local_priority) last.ids.push(channel.id);
    else groups.push({ priority: channel.local_priority, ids: [channel.id] });
  }
  const sourceIndex = groups.findIndex((group) => group.ids.includes(channelId));
  const sourceWasAlone = groups[sourceIndex].ids.length === 1;
  const remaining = groups.map((group) => ({ ...group, ids: group.ids.filter((id) => id !== channelId) }))
    .filter((group) => group.ids.length > 0);
  if (target.kind === "join") {
    remaining.find((group) => group.ids.includes(target.channelId))!.ids.push(channelId);
  } else {
    const index = target.beforeChannelId === null ? remaining.length
      : remaining.findIndex((group) => group.ids.includes(target.beforeChannelId!));
    if (index < 0 || (sourceWasAlone && index === sourceIndex)) return {};
    remaining.splice(index, 0, { priority: 0, ids: [channelId] });
  }
  // These numbers describe ordered tiers. Close the vacated tier as well as
  // opening the new one, so moving four channels never creates a fifth number.
  const previous = new Map(ordered.map((channel) => [channel.id, channel.local_priority]));
  return Object.fromEntries(remaining.flatMap((group, index) => group.ids
    .filter((id) => previous.get(id) !== index + 1)
    .map((id) => [id, index + 1])));
}
