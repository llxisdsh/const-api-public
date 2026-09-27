import { beforeEach, expect, test, vi } from "vitest";
import { createMarketPriceCache, type MarketPriceQuery } from "./marketPriceCache";
import type { MarketPriceSnapshot } from "./MarketPricesModal";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

const query: MarketPriceQuery = {
  view: "catalog", query: null, channelId: null,
  sort: "model", order: "asc", page: 1, pageSize: 10,
};
const data: MarketPriceSnapshot = {
  currency: "USD", observed_at: "2026-09-10T00:00:00Z", history_window_seconds: 86400,
  history_record_limit: 5000, routing_policy: "inverse_square", price_exponent: 2,
  max_price_ratio: 3, models: [], my_offers: [],
};
beforeEach(() => { invoke.mockReset(); invoke.mockResolvedValue({ data, revision: "r1" }); });

test.each([
  { models: null, my_offers: [] },
  { models: [], my_offers: null },
  { models: null, my_offers: null },
  { models: undefined, my_offers: [] },
  { models: [], my_offers: undefined },
])("normalizes legacy empty collections before caching: %j", async (collections) => {
  let now = 0;
  const cache = createMarketPriceCache(() => now);
  invoke.mockResolvedValueOnce({ data: { ...data, ...collections }, revision: "empty-page" });
  const empty = await cache.fetch("a", query);
  expect(empty.models).toEqual([]);
  expect(empty.my_offers).toEqual([]);
  expect(cache.peek("a", query)).toBe(empty);
  now = 30_000;
  invoke.mockResolvedValueOnce({ not_modified: true, revision: "empty-page" });
  expect(await cache.fetch("a", query)).toBe(empty);
  now = 60_000;
  invoke.mockResolvedValueOnce({ data, revision: "supply-returned" });
  expect(await cache.fetch("a", query)).toBe(data);
});

test.each([
  undefined, null, {}, [],
  { models: "wrong", my_offers: [] },
  { models: [], my_offers: {} },
])("still rejects malformed snapshots instead of caching false empty success: %j", async (payload) => {
  const cache = createMarketPriceCache();
  invoke.mockResolvedValueOnce({ data: payload });
  await expect(cache.fetch("a", query)).rejects.toThrow("Invalid market price snapshot response");
  expect(cache.peek("a", query)).toBeNull();
});

test("deduplicates concurrent fetches and skips fresh server requests", async () => {
  let now = 0;
  const cache = createMarketPriceCache(() => now);
  await Promise.all(Array.from({ length: 20 }, () => cache.fetch("a", query)));
  now = 29_999;
  expect(await cache.fetch("a", query)).toBe(data);
  expect(invoke).toHaveBeenCalledTimes(1);
});

test("revalidates expired content and merges only the timestamp when unchanged", async () => {
  let now = 0;
  const cache = createMarketPriceCache(() => now);
  await cache.fetch("a", query);
  now = 30_000;
  invoke.mockResolvedValueOnce({ not_modified: true, revision: "r1", observed_at: "2026-09-10T01:00:00Z" });
  const next = await cache.fetch("a", query);
  expect(invoke).toHaveBeenLastCalledWith("fetch_market_prices", { ...query, ifRevision: "r1" });
  expect(next.models).toBe(data.models);
  expect(next.observed_at).toBe("2026-09-10T01:00:00Z");
  now = 59_999;
  await cache.fetch("a", query);
  expect(invoke).toHaveBeenCalledTimes(2);
});

test("keeps old-server full responses compatible and replaces changed pages", async () => {
  let now = 0;
  const cache = createMarketPriceCache(() => now);
  invoke.mockResolvedValueOnce({ data });
  await cache.fetch("a", query);
  now = 30_000;
  const next = { ...data, max_price_ratio: 2 };
  invoke.mockResolvedValueOnce({ data: next });
  expect(await cache.fetch("a", query)).toBe(next);
  expect(invoke).toHaveBeenLastCalledWith("fetch_market_prices", query);
});

test("separates account, view, search, channel, sort and pagination keys", async () => {
  const cache = createMarketPriceCache();
  await cache.fetch("a", query);
  expect(cache.peek("b", query)).toBeNull();
  for (const change of [{ view: "offers" as const }, { query: "search" }, { channelId: "b" }, { sort: "ratio" }, { order: "desc" as const }, { page: 2 }, { pageSize: 20 }]) {
    expect(cache.peek("a", { ...query, ...change })).toBeNull();
    await cache.fetch("a", { ...query, ...change });
  }
  await cache.fetch("b", query);
  expect(invoke).toHaveBeenCalledTimes(9);
});

test("invalidated in-flight responses cannot repopulate an identity's cache", async () => {
  let finish!: (value: unknown) => void;
  invoke.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
  const cache = createMarketPriceCache();
  const pending = cache.fetch("a", query);
  cache.clear();
  finish({ data, revision: "r1" });
  await pending;
  expect(cache.peek("a", query)).toBeNull();
  await cache.fetch("a", query);
  expect(invoke).toHaveBeenCalledTimes(2);
});

test("retains cached prices on failure without treating failure as a successful check", async () => {
  let now = 0;
  const cache = createMarketPriceCache(() => now);
  await cache.fetch("a", query);
  now = 30_000;
  invoke.mockRejectedValueOnce(new Error("offline"));
  await expect(cache.fetch("a", query)).rejects.toThrow("offline");
  expect(cache.peek("a", query)).toBe(data);
  await cache.fetch("a", query);
  expect(invoke).toHaveBeenCalledTimes(3);
});

test("rejects an unmatched not-modified response and bounds cached pages", async () => {
  const cache = createMarketPriceCache();
  invoke.mockResolvedValueOnce({ not_modified: true, revision: "unknown" });
  await expect(cache.fetch("a", query)).rejects.toThrow("revision");
  for (let page = 1; page <= 33; page += 1) await cache.fetch("a", { ...query, page });
  expect(cache.peek("a", query)).toBeNull();
  expect(cache.peek("a", { ...query, page: 33 })).toBe(data);
});
