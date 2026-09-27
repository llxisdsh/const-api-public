import { describe, expect, test } from "vitest";
import {
  channelsVisibleInModelMarket,
  configuredModelMarketChannelCount,
  isLegacyLocalSupplierPlaceholder,
  moveChannelInVisibleOrder,
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

describe("moveChannelInVisibleOrder", () => {
  test("swaps adjacent visible channels without moving a hidden placeholder", () => {
    const channels = [
      { id: "hidden" },
      { id: "first" },
      { id: "second" },
      { id: "third" },
    ];

    expect(moveChannelInVisibleOrder(
      channels,
      ["first", "second", "third"],
      "second",
      -1,
    )?.map((channel) => channel.id)).toEqual([
      "hidden",
      "second",
      "first",
      "third",
    ]);
    expect(moveChannelInVisibleOrder(channels, ["first", "second", "third"], "first", -1)).toBeNull();
  });
});
