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

test("filters 450 models before paging and batches only public prices for visible matches", async () => {
  let now = 0;
  const cache = createMarketPriceCache(() => now);
  const ids = Array.from({ length: 450 }, (_, i) => `model-${String(i).padStart(3, "0")}`);
  const price = (model: string) => ({ model, currency: "USD", ready_supplier_count: 1, recent: { transaction_count: 0, supplier_count: 0 } });
  const edit = "openai.images_edits";
  const index = { ...data, models: ids.slice(0, 10).map(price), model_index: ids,
    model_directory: ids.map((id, i) => ({ id, operations: i >= 439 ? [edit] : [] })) };
  const local = [{ id: "model-449", local: [edit], outputs: [] }, { id: "private-local", local: [edit], outputs: [] }];
  invoke.mockImplementation(async (command, args) => {
    if (command === "get_available_model_directory") { expect(args).toEqual({ localOnly: true }); return local; }
    if (args.ifRevision) return { not_modified: true, revision: args.ifRevision, observed_at: "2026-11-20T00:00:30Z" };
    if (args.view === "catalog_index") return { data: index, revision: "index" };
    expect(args.view).toBe("selection");
    expect(args.modelIds.length).toBeLessThanOrEqual(10);
    expect(args.modelIds).not.toContain("private-local");
    return { data: { ...data, models: args.modelIds.map(price) }, revision: args.modelIds.join(",") };
  });
  const defaultOrder = { ...query, order: "default" as const };
  const all = await cache.fetchAvailable("a", defaultOrder, "all", true);
  expect(all.model_total).toBe(451);
  expect(invoke).toHaveBeenCalledTimes(2); // Index with first page plus in-process local readiness.
  const edits = await cache.fetchAvailable("a", defaultOrder, "edit", true);
  expect(edits.pagination?.total).toBe(12);
  expect(edits.models.map((m) => m.model)).toEqual(ids.slice(439, 449));
  const lastQuery = { ...defaultOrder, page: 2 };
  const last = await cache.fetchAvailable("a", lastQuery, "edit", true);
  expect(last.models.map((m) => m.model)).toEqual(["model-449", "private-local"]);
  expect(last.models[0].directory).toMatchObject({ local: [edit], platform: [edit] });
  expect(last.models[1]).toMatchObject({ directoryOnly: true, ready_supplier_count: 0 });
  expect(cache.peekAvailable("a", defaultOrder, "edit", true)?.models.map((m) => m.model)).toEqual(ids.slice(439, 449));
  expect(cache.peekAvailable("a", { ...defaultOrder, page: 2 }, "all", true)?.models[0]).toMatchObject({ model: "model-010", pricePending: true });
  expect(cache.peekAvailable("b", defaultOrder, "edit", true)).toBeNull();
  expect(cache.peekAvailable("a", defaultOrder, "edit", false)?.models.map((m) => m.model)).toEqual(["model-449", "private-local"]);
  const search = await cache.fetchAvailable("a", { ...defaultOrder, query: "private-local" }, "all", true);
  expect(search.models).toHaveLength(1);
  expect(invoke).toHaveBeenCalledTimes(4); // No network for a private-only search.
  now += 30_000;
  await cache.fetchAvailable("a", lastQuery, "edit", true);
  expect(invoke.mock.calls.slice(4).map(([command, args]) => [command, args.ifRevision ?? "local"])).toEqual([
    ["fetch_market_prices", "index"], ["get_available_model_directory", "local"], ["fetch_market_prices", "model-449"],
  ]);
});

test("a closed view or changed account cannot continue a pending index request into local reads or price batches", async () => {
  const cache = createMarketPriceCache();
  let resolve!: (value: unknown) => void;
  invoke.mockImplementationOnce(() => new Promise((done) => { resolve = done; }));
  const pending = cache.fetchAvailable("a", query, "all", true);
  cache.clear();
  resolve({ data: { ...data, model_index: ["model"], model_directory: [] } });
  await expect(pending).rejects.toMatchObject({ name: "AbortError" });
  expect(invoke).toHaveBeenCalledTimes(1);
  expect(cache.peekAvailable("a", query, "all", true)).toBeNull();
});

test("a price-only refresh retains the index and its task filters only in the originating cache", async () => {
  let now = 0;
  const cache = createMarketPriceCache(() => now);
  const model = { model: "canvas", currency: "USD", ready_supplier_count: 1, recent: { transaction_count: 0, supplier_count: 0 } };
  const models = [model];
  const index = { ...data, models, model_index: ["canvas"], model_directory: [{ id: "canvas", operations: ["openai.images_edits"] }] };
  const delta = { reuse_model_index: true, revision: "new-price", data: { ...data, models: [{ ...model, indicative_ratio: 1.5 }] } };
  invoke.mockImplementation(async (command) => command === "get_available_model_directory" ? []
    : now ? delta : { data: index, revision: "old-price" });
  await cache.fetchAvailable("owner", query, "all", true);
  now += 30_000;
  const edits = await cache.fetchAvailable("owner", query, "edit", true);
  expect(edits.models).toHaveLength(1);
  expect(edits.models[0]).toMatchObject({ model: "canvas", indicative_ratio: 1.5, directory: { platform: ["openai.images_edits"] } });
  await expect(cache.fetch("other", { ...query, view: "catalog_index" })).rejects.toThrow("No matching cached model index");
});

test("joins adjacent-server pages with the old directory before filtering and reuses those pages", async () => {
  const cache = createMarketPriceCache();
  const models = Array.from({ length: 61 }, (_, i) => ({
    model: `model-${String(i).padStart(2, "0")}`, currency: "USD", ready_supplier_count: 1,
    recent: { transaction_count: 0, supplier_count: 0 },
  }));
  const edit = "openai.images_edits";
  invoke.mockImplementation(async (command: string, args) => {
    if (command === "get_available_model_directory") {
      expect(args).toEqual({ localOnly: false });
      return [
        { id: "model-60", local: null, platform: [edit], outputs: ["image"] },
        { id: "private-edit", local: [edit], platform: null, outputs: ["image"] },
      ];
    }
    expect(args.view).not.toBe("selection");
    const { page, pageSize } = args;
    return { data: { ...data, models: models.slice((page - 1) * pageSize, page * pageSize),
      pagination: { page, page_size: pageSize, total: models.length, total_pages: Math.ceil(models.length / pageSize) } } };
  });
  const page = await cache.fetchAvailable("a", query, "edit", true);
  expect(page.model_total).toBe(62);
  expect(page.models.map((row) => row.model)).toEqual(["model-60", "private-edit"]);
  const filtered = await cache.fetchAvailable("a", { ...query, query: "private" }, "edit", true);
  expect(filtered.models.map((row) => row.model)).toEqual(["private-edit"]);
  expect(cache.peekAvailable("a", query, "edit", true)?.models.map((row) => row.model)).toEqual(["model-60", "private-edit"]);
  expect(invoke).toHaveBeenCalledTimes(4); // First page, combined directory and two legacy pages.
});

test("local-only capability matches never wait for an unrelated platform price, including after a failed local refresh", async () => {
  let now = 10;
  const cache = createMarketPriceCache(() => now);
  const index = { ...data, model_index: ["shared"], model_directory: [{ id: "shared", imageInput: true }] };
  const local = [{ id: "shared", local: ["openai.images_edits"], platform: null, outputs: ["image"] }];
  invoke.mockImplementation(async (command, args) => {
    if (command === "get_available_model_directory") {
      if (now > 10) throw new Error("local read failed");
      return local;
    }
    expect(args.view).toBe("catalog_index");
    return { data: index, revision: "index" };
  });
  await cache.fetchAvailable("owner", query, "edit", true);
  const edit = cache.peekAvailable("owner", { ...query, query: "shared" }, "edit", true);
  expect(edit?.models[0]).toMatchObject({ model: "shared", directoryOnly: true });
  expect(edit?.models[0].pricePending).toBeUndefined();
  now += 30_000;
  const stale = await cache.fetchAvailable("owner", query, "edit", true);
  expect(stale.directoryUnavailable).toBe(true);
  expect(stale.models[0].directory?.local).toEqual(["openai.images_edits"]);
  expect(invoke).toHaveBeenCalledTimes(4); // Two index and two native reads, zero selection requests.
});

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
