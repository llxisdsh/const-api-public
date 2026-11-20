import { invoke } from "@tauri-apps/api/core";
import type { MarketModelPrice, MarketPriceSnapshot } from "./MarketPricesModal";
import { availableModel, filterAvailableModels, mergeMarketDirectory, sourceMatches, type DirectoryFilter, type DirectoryModel } from "./availableMarketModels";

export const MARKET_PRICE_CACHE_TTL_MS = 30_000;
const MAX_CACHED_MARKET_PAGES = 32;

export type MarketPriceQuery = {
  view: "catalog" | "offers" | "catalog_index" | "selection";
  query: string | null;
  channelId: string | null;
  sort: string;
  order: "default" | "asc" | "desc";
  page: number;
  pageSize: number;
  modelIds?: string[];
};

type WirePriceSnapshot = Omit<MarketPriceSnapshot, "models" | "my_offers"> & {
  models?: MarketPriceSnapshot["models"] | null;
  my_offers?: MarketPriceSnapshot["my_offers"] | null;
};

type PriceResponse = {
  data?: WirePriceSnapshot;
  revision?: string;
  not_modified?: boolean;
  reuse_model_index?: boolean;
  observed_at?: string;
};

type CachedPricePage = {
  snapshot: MarketPriceSnapshot;
  revision?: string;
  checkedAt: number;
};

const localPriceSnapshot: MarketPriceSnapshot = {
  currency: "", observed_at: "", routing_policy: "", price_exponent: 0, max_price_ratio: 0,
  history_window_seconds: 0, history_record_limit: 0, models: [], my_offers: [],
};

function indexQueryFor(query: MarketPriceQuery): MarketPriceQuery {
  return { ...query, view: "catalog_index", page: 1, query: null };
}

function needsLegacyPages(index: MarketPriceSnapshot) {
  return !index.model_index && (index.pagination?.total ?? 0) > index.models.length;
}

function legacyPageQuery(query: MarketPriceQuery, page: number): MarketPriceQuery {
  return { ...query, view: "catalog", query: null, page, pageSize: 50 };
}

export type MarketPriceCache = ReturnType<typeof createMarketPriceCache>;

// One cache per app window, retained across modal mounts, never persisted to
// disk. Keys include the account scope and the complete page query. There is
// no background timer here: only an open, visible price page requests data.
export function createMarketPriceCache(now: () => number = Date.now) {
  const pages = new Map<string, CachedPricePage>();
  const inFlight = new Map<string, Promise<MarketPriceSnapshot>>();
  let generation = 0;
  let directoryCache: { scope: string; localOnly: boolean; checkedAt: number; rows: Promise<DirectoryModel[]> } | undefined;
  let lastDirectory: { scope: string; localOnly: boolean; rows: DirectoryModel[] } | undefined;
  let filtered: {
    scope: string;
    filterKey: string;
    index: MarketPriceSnapshot;
    local: DirectoryModel[];
    prices: MarketModelPrice[];
    catalog: ReturnType<typeof filterAvailableModels>;
    byId: Map<string, MarketModelPrice>;
  } | undefined;
  let lastAvailable: { key: string; scope: string; snapshot: MarketPriceSnapshot } | undefined;
  const keyFor = (scope: string, query: MarketPriceQuery) => JSON.stringify([
    scope, query.view, query.query, query.channelId,
    query.sort, query.order, query.page, query.pageSize, query.modelIds,
  ]);
  const read = (key: string) => {
    const entry = pages.get(key);
    if (entry) {
      pages.delete(key);
      pages.set(key, entry);
    }
    return entry;
  };
  function fetchPage(scope: string, query: MarketPriceQuery): Promise<MarketPriceSnapshot> {
    const key = keyFor(scope, query);
    const cached = read(key);
    const startedAt = now();
    const age = cached ? startedAt - cached.checkedAt : Infinity;
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
          || (data.my_offers != null && !Array.isArray(data.my_offers))
          || (data.model_index != null && !Array.isArray(data.model_index))
          || (data.model_directory != null && !Array.isArray(data.model_directory))) {
          throw new Error("Invalid market price snapshot response");
        }
        // Go nil slices (including an empty catalog page) encode as null.
        // Older servers may omit an unused collection. Normalize only at
        // the wire boundary; the renderer/cache always receives arrays.
        snapshot = data.models && data.my_offers
          ? data as MarketPriceSnapshot
          : { ...data, models: data.models ?? [], my_offers: data.my_offers ?? [] };
        if (response.reuse_model_index) {
          if (query.view !== "catalog_index" || !cached?.revision || !cached.snapshot.model_index
            || !("model_directory" in cached.snapshot)) {
            throw new Error("No matching cached model index for the updated price page");
          }
          snapshot = { ...snapshot, model_index: cached.snapshot.model_index, model_directory: cached.snapshot.model_directory };
        }
      }
      if (generation === startedGeneration) {
        pages.delete(key);
        pages.set(key, { snapshot, revision: response.revision, checkedAt: startedAt });
        while (pages.size > MAX_CACHED_MARKET_PAGES) pages.delete(pages.keys().next().value!);
      }
      return snapshot;
    }).finally(() => {
      if (inFlight.get(key) === request) inFlight.delete(key);
    });
    inFlight.set(key, request);
    return request;
  }

  function directory(scope: string, localOnly: boolean) {
    const age = directoryCache ? now() - directoryCache.checkedAt : Infinity;
    if (directoryCache?.scope === scope && directoryCache.localOnly === localOnly && age >= 0 && age < MARKET_PRICE_CACHE_TTL_MS) {
      return directoryCache.rows;
    }
    const rows = invoke<DirectoryModel[]>("get_available_model_directory", { localOnly }).then((value) => {
      if (!Array.isArray(value)) throw new Error("Invalid model directory");
      if (directoryCache?.rows === rows) lastDirectory = { scope, localOnly, rows: value };
      return value;
    }).catch((error) => {
      if (directoryCache?.rows === rows) directoryCache = undefined;
      throw error;
    });
    directoryCache = { scope, localOnly, checkedAt: now(), rows };
    return rows;
  }

  const availableKey = (scope: string, query: MarketPriceQuery, purpose: DirectoryFilter, online: boolean) =>
    JSON.stringify([keyFor(scope, query), purpose, online]);

  // Cache only the active ordered result set. Navigating either direction slices
  // ten IDs and borrows their prices from the same bounded cache as revalidation.
  function planPage(
    scope: string, query: MarketPriceQuery, purpose: DirectoryFilter,
    index: MarketPriceSnapshot, local: DirectoryModel[], prices: MarketModelPrice[],
  ) {
    const filterKey = JSON.stringify([purpose, query.query, query.sort, query.order]);
    if (filtered?.scope !== scope || filtered.filterKey !== filterKey
      || filtered.index !== index || filtered.local !== local || filtered.prices !== prices) {
      const directory = "model_directory" in index ? mergeMarketDirectory(local, index.model_directory ?? []) : local;
      const ids = index.model_index ?? prices.map((row) => row.model);
      filtered = {
        scope, filterKey, index, local, prices,
        catalog: filterAvailableModels(ids, directory, purpose, query),
        byId: new Map(prices.map((row) => [row.model, row])),
      };
    }
    const { catalog, byId } = filtered;
    const totalPages = Math.ceil(catalog.matches.length / query.pageSize);
    const page = Math.min(query.page, Math.max(1, totalPages));
    const start = (page - 1) * query.pageSize;
    const rows = catalog.matches.slice(start, start + query.pageSize);
    const missing = rows
      .filter((row) => row.market && !byId.has(row.id) && (purpose === "all" || sourceMatches(row.directory, "platform", purpose)))
      .map((row) => row.id);
    const detailQuery: MarketPriceQuery | undefined = missing.length ? {
      ...indexQueryFor(query), view: "selection", sort: "model", order: "asc", modelIds: missing.sort(),
    } : undefined;
    function snapshot(details?: MarketPriceSnapshot, pending = false): MarketPriceSnapshot {
      const extra = new Map(details?.models.map((row) => [row.model, row]));
      return {
        ...index, view: "catalog", directoryPurpose: purpose, model_total: catalog.total,
        pagination: { page, page_size: query.pageSize, total: catalog.matches.length, total_pages: totalPages },
        models: rows.map((row) => {
          const model = availableModel(row, byId.get(row.id) ?? extra.get(row.id));
          if (pending && missing.includes(row.id)) model.pricePending = true;
          return model;
        }),
      };
    }
    return { detailQuery, snapshot };
  }

  function peekAvailable(scope: string, query: MarketPriceQuery, purpose: DirectoryFilter, online: boolean) {
    if (lastAvailable?.key === availableKey(scope, query, purpose, online)) return lastAvailable.snapshot;
    const index = online ? read(keyFor(scope, indexQueryFor(query)))?.snapshot : localPriceSnapshot;
    if (!index || lastDirectory?.scope !== scope) return null;
    const localOnly = !online || "model_directory" in index;
    if (lastDirectory.localOnly !== localOnly) return null;
    const local = lastDirectory.rows;
    if (needsLegacyPages(index) && purpose === "all" && !local.length) {
      const page = query.page === 1 && !query.query ? index : read(keyFor(scope, query))?.snapshot;
      return page ? { ...page, view: "catalog" as const, directoryPurpose: purpose } : null;
    }
    let prices = index.models;
    if (needsLegacyPages(index)) {
      if (filtered?.scope === scope && filtered.index === index) {
        prices = filtered.prices;
      } else {
        const first = read(keyFor(scope, legacyPageQuery(query, 1)))?.snapshot;
        if (!first) return null;
        prices = [...first.models];
        for (let page = 2; page <= (first.pagination?.total_pages ?? 1); page++) {
          const cached = read(keyFor(scope, legacyPageQuery(query, page)))?.snapshot;
          if (!cached || cached.pagination?.page !== page) return null;
          prices.push(...cached.models);
        }
      }
    }
    const plan = planPage(scope, query, purpose, index, local, prices);
    const cached = plan.detailQuery && read(keyFor(scope, plan.detailQuery));
    return plan.snapshot(cached?.snapshot, !!plan.detailQuery && !cached);
  }

  async function fetchAvailable(scope: string, query: MarketPriceQuery, purpose: DirectoryFilter, online: boolean, current: () => boolean = () => true) {
    const startedGeneration = generation;
    const checkCurrent = () => {
      if (generation !== startedGeneration || !current()) throw new DOMException("View changed", "AbortError");
    };
    const indexQuery = indexQueryFor(query);
    let marketError: string | undefined;
    const index = online ? await fetchPage(scope, indexQuery).catch((error) => {
      marketError = String(error);
      return read(keyFor(scope, indexQuery))?.snapshot ?? null;
    }) : null;
    checkCurrent();
    // An adjacent old server ignores the index view. Only it needs the
    // existing combined discovery endpoint; current servers need no second HTTP call.
    const localOnly = !index || "model_directory" in index;
    let directoryUnavailable = false;
    const rows = await directory(scope, localOnly).catch((error) => {
      directoryUnavailable = true;
      if (lastDirectory?.scope === scope && lastDirectory.localOnly === localOnly) return lastDirectory.rows;
      if (!index) throw new Error(marketError ?? String(error));
      return [];
    });
    checkCurrent();
    const base = index ?? localPriceSnapshot;
    const finish = (result: MarketPriceSnapshot) => {
      checkCurrent();
      const snapshot: MarketPriceSnapshot = {
        ...result, view: "catalog", directoryPurpose: purpose, directoryUnavailable, marketError,
      };
      lastAvailable = { key: availableKey(scope, query, purpose, online), scope, snapshot };
      return snapshot;
    };
    let prices = base.models;
    if (index && needsLegacyPages(index)) {
      // Adjacent older servers provide normal sorted pages but no exact-ID index.
      // Keep their ordinary fast path unless a full local/purpose join is needed.
      if (purpose === "all" && !rows.length) {
        const page = query.page === 1 && !query.query ? index : await fetchPage(scope, query);
        return finish(page);
      }
      // Use only paginated public prices even on older servers. Each page is
      // cached and conditionally revalidated; never request the unpaged ledger.
      const first = await fetchPage(scope, legacyPageQuery(query, 1));
      prices = [...first.models];
      for (let page = 2; page <= (first.pagination?.total_pages ?? 1); page++) {
        checkCurrent();
        const next = await fetchPage(scope, legacyPageQuery(query, page));
        prices.push(...next.models);
        if (!next.models.length || next.pagination?.page !== page) break;
      }
      checkCurrent();
    }
    const plan = planPage(scope, query, purpose, base, rows, prices);
    let details: MarketPriceSnapshot | undefined;
    if (plan.detailQuery) {
      checkCurrent();
      const detailQuery = plan.detailQuery;
      details = await fetchPage(scope, detailQuery).catch((error) => {
        marketError = String(error);
        return read(keyFor(scope, detailQuery))?.snapshot;
      });
    }
    return finish(plan.snapshot(details));
  }

  return {
    clear() {
      generation += 1;
      pages.clear();
      inFlight.clear();
      directoryCache = undefined;
      lastDirectory = undefined;
      filtered = undefined;
      lastAvailable = undefined;
    },
    peek(scope: string, query: MarketPriceQuery) {
      return read(keyFor(scope, query))?.snapshot ?? null;
    },
    peekAvailable,
    availableTotal(scope: string) {
      return lastAvailable?.scope === scope ? lastAvailable.snapshot.model_total : undefined;
    },
    fetch: fetchPage, fetchAvailable,
  };
}
