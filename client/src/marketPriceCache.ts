import { invoke } from "@tauri-apps/api/core";
import type { MarketPriceSnapshot } from "./MarketPricesModal";

export const MARKET_PRICE_CACHE_TTL_MS = 30_000;
const MAX_CACHED_MARKET_PAGES = 32;

export type MarketPriceQuery = {
  view: "catalog" | "offers";
  query: string | null;
  channelId: string | null;
  sort: string;
  order: "default" | "asc" | "desc";
  page: number;
  pageSize: number;
};

type WirePriceSnapshot = Omit<MarketPriceSnapshot, "models" | "my_offers"> & {
  models?: MarketPriceSnapshot["models"] | null;
  my_offers?: MarketPriceSnapshot["my_offers"] | null;
};

type PriceResponse = {
  data?: WirePriceSnapshot;
  revision?: string;
  not_modified?: boolean;
  observed_at?: string;
};

type CachedPricePage = {
  snapshot: MarketPriceSnapshot;
  revision?: string;
  checkedAt: number;
};

export type MarketPriceCache = ReturnType<typeof createMarketPriceCache>;

// One cache per app window, retained across modal mounts, never persisted to
// disk. Keys include the account scope and the complete page query. There is
// no background timer here: only an open, visible price page requests data.
export function createMarketPriceCache(now: () => number = Date.now) {
  const pages = new Map<string, CachedPricePage>();
  const inFlight = new Map<string, Promise<MarketPriceSnapshot>>();
  let generation = 0;
  const keyFor = (scope: string, query: MarketPriceQuery) => JSON.stringify([
    scope, query.view, query.query, query.channelId,
    query.sort, query.order, query.page, query.pageSize,
  ]);
  const read = (key: string) => {
    const entry = pages.get(key);
    if (entry) {
      pages.delete(key);
      pages.set(key, entry);
    }
    return entry;
  };
  return {
    clear() {
      generation += 1;
      pages.clear();
      inFlight.clear();
    },
    peek(scope: string, query: MarketPriceQuery) {
      return read(keyFor(scope, query))?.snapshot ?? null;
    },
    fetch(scope: string, query: MarketPriceQuery): Promise<MarketPriceSnapshot> {
      const key = keyFor(scope, query);
      const cached = read(key);
      const age = cached ? now() - cached.checkedAt : Infinity;
      if (cached && age >= 0 && age < MARKET_PRICE_CACHE_TTL_MS) {
        return Promise.resolve(cached.snapshot);
      }
      const pending = inFlight.get(key);
      if (pending) return pending;
      const startedGeneration = generation;
      const request = invoke<PriceResponse>("fetch_market_prices", {
        ...query,
        ...(cached?.revision ? { ifRevision: cached.revision } : {}),
      }).then((response) => {
        let snapshot: MarketPriceSnapshot;
        if (response.not_modified) {
          if (!cached?.revision || response.revision !== cached.revision) {
            throw new Error("Market price revision does not match the cached page");
          }
          snapshot = response.observed_at && response.observed_at !== cached.snapshot.observed_at
            ? { ...cached.snapshot, observed_at: response.observed_at }
            : cached.snapshot;
        } else {
          const data = response.data;
          if (!data || typeof data !== "object" || Array.isArray(data)
            || (!("models" in data) && !("my_offers" in data))
            || (data.models != null && !Array.isArray(data.models))
            || (data.my_offers != null && !Array.isArray(data.my_offers))) {
            throw new Error("Invalid market price snapshot response");
          }
          // Go nil slices (including an empty catalog page) encode as null.
          // Older servers may omit an unused collection. Normalize only at
          // the wire boundary; the renderer/cache always receives arrays.
          snapshot = data.models && data.my_offers
            ? data as MarketPriceSnapshot
            : { ...data, models: data.models ?? [], my_offers: data.my_offers ?? [] };
        }
        if (generation === startedGeneration) {
          pages.delete(key);
          pages.set(key, { snapshot, revision: response.revision, checkedAt: now() });
          while (pages.size > MAX_CACHED_MARKET_PAGES) pages.delete(pages.keys().next().value!);
        }
        return snapshot;
      }).finally(() => {
        if (inFlight.get(key) === request) inFlight.delete(key);
      });
      inFlight.set(key, request);
      return request;
    },
  };
}
