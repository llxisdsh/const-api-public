import { describe, expect, test } from "vitest";
import {
  channelsVisibleInModelMarket,
  configuredModelMarketChannelCount,
  isLegacyLocalSupplierPlaceholder,
  applyLocalPriorities,
  channelsByLocalPriority,
  isValidLocalPriority,
  withLocalPriorities,
  prioritiesForChannelDrop,
  MAX_LOCAL_PRIORITY,
} from "./channelOrdering";

describe("channelsVisibleInModelMarket", () => {
  const placeholder = {
    id: "channel-1",
    name: "Local Supplier",
    source_driver: "custom_endpoint",
    created_at_unix_ms: 0,
    enabled: false,
    share_enabled: false,
  };
  const userChannel = {
    id: "channel-user",
    name: "My endpoint",
    source_driver: "custom_endpoint",
    created_at_unix_ms: 100,
    enabled: false,
    share_enabled: false,
  };

  test("hides only the untouched legacy placeholder in production", () => {
    expect(isLegacyLocalSupplierPlaceholder(placeholder)).toBe(true);
    expect(channelsVisibleInModelMarket([placeholder, userChannel], false)).toEqual([
      userChannel,
    ]);
  });

  test("keeps the placeholder in development and never hides real channels", () => {
    expect(channelsVisibleInModelMarket([placeholder, userChannel], true)).toEqual([
      placeholder,
      userChannel,
    ]);
    expect(isLegacyLocalSupplierPlaceholder({ ...placeholder, enabled: true })).toBe(false);
    expect(isLegacyLocalSupplierPlaceholder({ ...placeholder, id: "channel-user" })).toBe(false);
  });

  test("does not count the development placeholder as a configured channel", () => {
    expect(configuredModelMarketChannelCount([placeholder])).toBe(0);
    expect(configuredModelMarketChannelCount([placeholder, userChannel])).toBe(1);
  });
});

describe("dragging local priority groups", () => {
  const channels = [
    { id: "a", local_priority: 1 }, { id: "b", local_priority: 1 },
    { id: "c", local_priority: 2 }, { id: "d", local_priority: 8 },
  ];
  test("joining keeps existing groups and same-group drops are unchanged", () => {
    expect(prioritiesForChannelDrop(channels, "d", { kind: "join", channelId: "b" })).toEqual({ d: 1 });
    expect(prioritiesForChannelDrop(channels, "b", { kind: "join", channelId: "a" })).toEqual({});
  });
  test("inserts a new tier while preserving every existing group", () => {
    const edits = prioritiesForChannelDrop(channels, "d", { kind: "insert", beforeChannelId: "a" });
    expect(edits).toEqual({ d: 1, a: 2, b: 2, c: 3 });
    expect(applyLocalPriorities(channels, edits).map((channel) => [channel.id, channel.local_priority]))
      .toEqual([["d", 1], ["a", 2], ["b", 2], ["c", 3]]);
  });
  test("splits one member into a separate tier and keeps numbers consecutive", () => {
    expect(prioritiesForChannelDrop(channels, "b", { kind: "insert", beforeChannelId: "d" })).toEqual({ b: 3, d: 4 });
    expect(prioritiesForChannelDrop(channels, "b", { kind: "insert", beforeChannelId: null })).toEqual({ d: 3, b: 4 });
  });
  test("moving down closes the old tier instead of creating a fifth number", () => {
    const four = ["a", "b", "c", "d"].map((id, index) => ({ id, local_priority: index + 1 }));
    const moved = prioritiesForChannelDrop(four, "a", { kind: "insert", beforeChannelId: null });
    expect(applyLocalPriorities(four, moved).map(channel => [channel.id, channel.local_priority]))
      .toEqual([["b", 1], ["c", 2], ["d", 3], ["a", 4]]);
    const joined = prioritiesForChannelDrop(four, "a", { kind: "join", channelId: "c" });
    expect(applyLocalPriorities(four, joined).map(channel => [channel.id, channel.local_priority]))
      .toEqual([["b", 1], ["a", 2], ["c", 2], ["d", 3]]);
  });
  test("does not renumber unchanged moves or persist missing drag targets", () => {
    expect(prioritiesForChannelDrop(channels, "c", { kind: "insert", beforeChannelId: "d" })).toEqual({});
    expect(prioritiesForChannelDrop(channels, "d", { kind: "insert", beforeChannelId: null })).toEqual({});
    expect(prioritiesForChannelDrop(channels, "deleted", { kind: "join", channelId: "a" })).toEqual({});
    expect(prioritiesForChannelDrop(channels, "d", { kind: "insert", beforeChannelId: "deleted" })).toEqual({});
  });
  test("includes hidden peers and normalizes large valid priorities", () => {
    const all = [...channels, { id: "hidden", local_priority: 2 }];
    expect(prioritiesForChannelDrop(all, "d", { kind: "insert", beforeChannelId: "c" }))
      .toEqual({ d: 2, c: 3, hidden: 3 });
    const extreme = [{ id: "a", local_priority: 1 }, { id: "b", local_priority: MAX_LOCAL_PRIORITY }, { id: "c", local_priority: MAX_LOCAL_PRIORITY }];
    const edits = prioritiesForChannelDrop(extreme, "a", { kind: "insert", beforeChannelId: null });
    expect(edits).toEqual({ b: 1, c: 1, a: 2 });
    expect(Object.values(edits).every(isValidLocalPriority)).toBe(true);
  });
});

describe("local priority tiers", () => {
  test("preserves legacy order and appends new channels after existing tiers", () => {
    const original = [{ id: "first" }, { id: "second" }];
    expect(withLocalPriorities(original).map((c) => c.local_priority)).toEqual([1, 2]);
    expect(original).toEqual([{ id: "first" }, { id: "second" }]);
    expect(withLocalPriorities([
      { id: "first", local_priority: 7 }, { id: "second", local_priority: 7 }, { id: "new" },
    ]).map((c) => c.local_priority)).toEqual([7, 7, 8]);
  });

  test("jumps directly to the first tier and groups accounts without changing their content", () => {
    const current = [
      { id: "hidden", local_priority: 3, name: "Hidden" },
      { id: "first", local_priority: 2, name: "Freshly refreshed name" },
      { id: "last", local_priority: 80, name: "Last" },
      { id: "new", name: "Discovered while dialog was open" },
    ];
    const result = applyLocalPriorities(current, { first: 1, last: 1, deleted: 1 });
    expect(result.map((c) => [c.id, c.local_priority])).toEqual([["first", 1], ["last", 1], ["hidden", 3], ["new", 81]]);
    expect(result[0].name).toBe("Freshly refreshed name");
    expect(current[1].local_priority).toBe(2);
    expect(channelsByLocalPriority(result).map((c) => c.id)).toEqual(result.map((c) => c.id));
  });

  test("rejects malformed input and keeps the wire integer boundary", () => {
    for (const value of [0, -1, 1.5, NaN, Infinity, 0x1_0000_0000]) {
      expect(isValidLocalPriority(value)).toBe(false);
      expect(() => applyLocalPriorities([{ id: "a" }], { a: value })).toThrow();
    }
    expect(withLocalPriorities([{ id: "a", local_priority: 0xffff_ffff }, { id: "b" }])
      .map((c) => c.local_priority)).toEqual([0xffff_ffff, 0xffff_ffff]);
  });
});
