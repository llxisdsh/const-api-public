import { describe, expect, test } from "vitest";
import {
  channelsVisibleInModelMarket,
  configuredModelMarketChannelCount,
  isLegacyLocalSupplierPlaceholder,
  applyLocalPriorities,
  channelsByLocalPriority,
  isValidLocalPriority,
  withLocalPriorities,
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
