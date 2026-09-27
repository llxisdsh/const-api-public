import type { TFunction } from "i18next";
import {
  ArrowDown,
  ArrowUp,
  ChevronDown,
  CircleDollarSign,
  RefreshCw,
  Search,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { DialogBackdrop } from "./DialogBackdrop";
import { accountDisplayAmountFromUSD, accountDisplayCurrency } from "./account/accountFormat";
import { currentAppLanguage } from "./i18n";
import { ModalCloseButton } from "./ModalCloseButton";
import { modelContextLabel, modelDisplayName } from "./modelPresentation";
import {
  MARKET_PRICE_CACHE_TTL_MS,
  createMarketPriceCache,
  type MarketPriceCache,
} from "./marketPriceCache";
import { PaginationControls } from "./PaginationControls";
import { createRefreshGate } from "./refreshPolicy";
import { useEscapeDismiss } from "./useEscapeDismiss";

export type MarketReferenceComponent = {
  meter_kind: string;
  qualifier?: string;
  unit: string;
  price_usd: number;
};

export type MarketRecentSummary = {
  transaction_count: number;
  supplier_count: number;
  weighted_median_ratio?: number;
  min_ratio?: number;
  max_ratio?: number;
};

export type MarketModelPrice = {
  model: string;
  context_tokens?: number;
  output_tokens?: number;
  currency: string;
  price_version_id?: string;
  pricing_rule_id?: string;
  pricing_confidence?: string;
  reference_components?: MarketReferenceComponent[];
  ready_supplier_count: number;
  ready_custom_supplier_count?: number;
  routable_supplier_count?: number;
  best_ratio?: number;
  indicative_ratio?: number;
  recent: MarketRecentSummary;
};

export type MarketModelAvailability = {
  state: "available" | "limited" | "unavailable";
  ready: number;
  customReady?: number;
  limited: number;
};

export type MarketSupplierOffer = {
  channel_id: string;
  supplier_unit_id: string;
  name: string;
  model: string;
  offer_version: string;
  posted_ratio: number;
  ready: boolean;
  routable?: boolean;
  route_pool?: string;
  route_reason?: string;
  has_comparison_market?: boolean;
  relative_price_weight: number;
  suggested_ratio: number;
  position: string;
};

export type MarketPagination = {
  page: number;
  page_size: number;
  total: number;
  total_pages: number;
};

export type MarketPriceSnapshot = {
  currency: string;
  observed_at: string;
  history_window_seconds: number;
  history_record_limit: number;
  routing_policy: string;
  price_exponent: number;
  max_price_ratio: number;
  models: MarketModelPrice[];
  my_offers: MarketSupplierOffer[];
  model_total?: number;
  offer_group_total?: number;
  view?: MarketView;
  sort?: string;
  order?: MarketOrder;
  pagination?: MarketPagination;
  warnings?: string[];
  variant_rules?: MarketVariantRule[];
};

export type MarketVariantRule = {
  id: string;
  suffix: string;
  mode: "free" | "minimum" | "multiplier";
  enabled: boolean;
  input_usd?: number;
  output_usd?: number;
  cached_input_usd?: number;
  multiplier?: number;
};

export type MarketChannelPriceContext = {
  channelId: string;
  channelName: string;
};

type MarketPricesModalProps = {
  platformAvailable: boolean;
  cache?: MarketPriceCache;
  cacheScope?: string;
  cnyPerUSD?: number;
  initialView?: MarketView;
  channelContext?: MarketChannelPriceContext;
  onClose: () => void;
};

type MarketView = "catalog" | "offers";
type MarketOrder = "default" | "asc" | "desc";
type CatalogSort = "model" | "input" | "output" | "ratio";
type OfferSort = "channel" | "models" | "ready" | "ratio";

type MarketOfferGroup = {
  key: string;
  name: string;
  offers: MarketSupplierOffer[];
  readyCount: number;
  priceWeightTotal: number;
  postedMin?: number;
  postedMax?: number;
};

type MarketPriceRequest = {
  version: number;
  scope: string;
  view: MarketView;
  query: string | null;
  channelId: string | null;
  sort: CatalogSort | OfferSort;
  order: MarketOrder;
  page: number;
  pageSize: number;
};

const MARKET_PAGE_SIZE = 10;
const MARKET_REQUEST_SETTLE_MS = 80;

export function marketModelAvailability(
  model: Pick<MarketModelPrice, "ready_supplier_count" | "ready_custom_supplier_count" | "routable_supplier_count">,
): MarketModelAvailability {
  const ready = Math.max(0, Math.trunc(Number(model.ready_supplier_count) || 0));
  const routable = Math.max(
    ready,
    Math.trunc(Number(model.routable_supplier_count ?? ready) || 0),
  );
  const limited = routable - ready;
  const customReady = typeof model.ready_custom_supplier_count === "number"
    && Number.isFinite(model.ready_custom_supplier_count)
    ? Math.min(ready, Math.max(0, Math.trunc(model.ready_custom_supplier_count)))
    : undefined;
  return {
    state: ready > 0 ? "available" : (limited > 0 ? "limited" : "unavailable"),
    ready,
    limited,
    ...(customReady === undefined ? {} : { customReady }),
  };
}

type MarketSortButtonProps = {
  active: boolean;
  direction: MarketOrder;
  label: string;
  title?: string;
  onClick: () => void;
};

function MarketSortButton({
  active,
  direction,
  label,
  title,
  onClick,
}: MarketSortButtonProps) {
  return (
    <button
      className={`market-sort-button${active ? " active" : ""}`}
      type="button"
      aria-pressed={active}
      title={title}
      onClick={onClick}
    >
      <span>{label}</span>
      {active && direction !== "default" && (
        direction === "desc"
          ? <ArrowDown aria-hidden="true" size={12} />
          : <ArrowUp aria-hidden="true" size={12} />
      )}
    </button>
  );
}

type MarketPagerProps = {
  pagination?: MarketPagination;
  loading: boolean;
  onPageChange: (page: number) => void;
  t: TFunction;
};

function MarketPager({ pagination, loading, onPageChange, t }: MarketPagerProps) {
  if (!pagination || pagination.total_pages <= 1) {
    return null;
  }
  const canGoToPreviousPage = pagination.page > 1;
  const canGoToNextPage = pagination.page < pagination.total_pages;
  return (
    <PaginationControls
      ariaLabel={t("market.paginationAria")}
      busy={loading}
      canGoNext={canGoToNextPage}
      canGoPrevious={canGoToPreviousPage}
      currentPage={pagination.page}
      currentPageLabel={t("market.paginationSummary", {
        page: pagination.page,
        pages: pagination.total_pages,
        total: pagination.total,
      })}
      nextLabel={t("market.nextPage")}
      onNext={() => onPageChange(pagination.page + 1)}
      onPrevious={() => onPageChange(pagination.page - 1)}
      previousLabel={t("market.previousPage")}
      summary={t("market.paginationTotal", { total: pagination.total })}
    />
  );
}

function formatRatio(value: number | undefined): string {
  return Number.isFinite(value) ? `${Number(value).toFixed(2)}\u00d7` : "\u2014";
}

function formatRatioRange(minimum: number | undefined, maximum: number | undefined): string {
  if (!Number.isFinite(minimum) || !Number.isFinite(maximum)) {
    return "\u2014";
  }
  return Number(minimum) === Number(maximum)
    ? formatRatio(minimum)
    : `${formatRatio(minimum)} \u2013 ${formatRatio(maximum)}`;
}

function formatAveragePriceWeight(total: number, count: number): string {
  if (count <= 0 || !Number.isFinite(total)) {
    return "\u2014";
  }
  return `${Math.round((total / count) * 100)}%`;
}

function formatMoney(value: number | undefined, cnyPerUSD?: number): string {
  if (!Number.isFinite(value)) {
    return "\u2014";
  }
  const amount = accountDisplayAmountFromUSD(Number(value), cnyPerUSD);
  const symbol = accountDisplayCurrency(cnyPerUSD) === "CNY" ? "\u00a5" : "$";
  if (amount === 0) {
    return `${symbol}0`;
  }
  if (Math.abs(amount) < 0.000001) {
    return `${symbol}${amount.toExponential(2)}`;
  }
  return `${symbol}${amount.toLocaleString(currentAppLanguage(), { maximumFractionDigits: 6 })}`;
}

function tokenPriceComponent(
  model: MarketModelPrice,
  meterKind: "input_tokens" | "output_tokens",
): MarketReferenceComponent | undefined {
  return (model.reference_components ?? []).find(
    (component) => component.meter_kind === meterKind && !component.qualifier,
  );
}

function indicativeUnitPrice(
  component: MarketReferenceComponent | undefined,
  model: MarketModelPrice,
  cnyPerUSD?: number,
): string {
  if (
    !component
    || model.ready_supplier_count <= 0
    || !Number.isFinite(model.indicative_ratio)
  ) {
    return "\u2014";
  }
  return formatMoney(component.price_usd * Number(model.indicative_ratio), cnyPerUSD);
}

function componentLabel(component: MarketReferenceComponent, t: TFunction): string {
  const meter = t(`market.meters.${component.meter_kind}`, {
    defaultValue: component.meter_kind.replace(/_/g, " "),
  });
  if (!component.qualifier) {
    return meter;
  }
  const qualifier = t(`market.qualifiers.${component.qualifier}`, {
    defaultValue: component.qualifier,
  });
  return t("market.qualifiedMeter", { meter, qualifier });
}

function unitLabel(unit: string, t: TFunction): string {
  return t(`market.units.${unit}`, {
    defaultValue: unit.replace(/_/g, " "),
  });
}

function executableUnitPrice(
  component: MarketReferenceComponent,
  model: MarketModelPrice,
  cnyPerUSD?: number,
): string {
  if (model.ready_supplier_count <= 0 || !Number.isFinite(model.indicative_ratio)) {
    return "\u2014";
  }
  return formatMoney(component.price_usd * Number(model.indicative_ratio), cnyPerUSD);
}

function marketErrorMessage(error: string, t: (key: string) => string): string {
  if (/account_http_401/i.test(error)) {
    return t("market.serverRestartRequired");
  }
  if (/auth_required|session_expired/i.test(error)) {
    return t("market.loginRequired");
  }
  if (/\b404\b|not found/i.test(error)) {
    return t("market.featureUnavailable");
  }
  return t("market.retryHint");
}

function marketWarningMessage(warning: string, t: (key: string) => string): string {
  switch (warning) {
    case "catalog_unavailable":
    case "pricing_unavailable":
    case "reference_price_unavailable":
      return t("market.warnings.referenceUnavailable");
    case "history_unavailable":
      return t("market.warnings.historyUnavailable");
    default:
      return t("market.warnings.partialData");
  }
}

function groupSupplierOffers(offers: MarketSupplierOffer[]): MarketOfferGroup[] {
  const groups = new Map<string, MarketOfferGroup>();
  for (const offer of offers) {
    const key = offer.channel_id || offer.supplier_unit_id || offer.name;
    const existing = groups.get(key);
    const group = existing ?? {
      key,
      name: offer.name || offer.channel_id || offer.supplier_unit_id,
      offers: [],
      readyCount: 0,
      priceWeightTotal: 0,
      postedMin: undefined,
      postedMax: undefined,
    };
    group.offers.push(offer);
    if (offer.ready) {
      group.readyCount += 1;
    }
    if (Number.isFinite(offer.relative_price_weight)) {
      group.priceWeightTotal += Math.max(0, offer.relative_price_weight);
    }
    if (Number.isFinite(offer.posted_ratio)) {
      group.postedMin = group.postedMin === undefined
        ? offer.posted_ratio
        : Math.min(group.postedMin, offer.posted_ratio);
      group.postedMax = group.postedMax === undefined
        ? offer.posted_ratio
        : Math.max(group.postedMax, offer.posted_ratio);
    }
    if (!existing) {
      groups.set(key, group);
    }
  }
  return [...groups.values()];
}

export function MarketPricesModal({
  platformAvailable,
  cache: sharedCache,
  cacheScope = "",
  cnyPerUSD,
  initialView = "catalog",
  channelContext,
  onClose,
}: MarketPricesModalProps) {
  useEscapeDismiss(onClose);
  const { t } = useTranslation();
  const [localCache] = useState(createMarketPriceCache);
  const priceCache = sharedCache ?? localCache;
  const [snapshotState, setSnapshotState] = useState(() => ({
    scope: cacheScope,
    data: platformAvailable ? priceCache.peek(cacheScope, {
      view: initialView, query: null,
      channelId: initialView === "offers" ? channelContext?.channelId ?? null : null,
      sort: initialView === "catalog" ? "model" : "channel", order: initialView === "catalog" ? "default" : "asc", page: 1, pageSize: MARKET_PAGE_SIZE,
    }) : null,
  }));
  const snapshot = platformAvailable
    && snapshotState.scope === cacheScope
    ? snapshotState.data
    : null;
  const [loading, setLoading] = useState(platformAvailable && !snapshot);
  const [error, setError] = useState("");
  const [refreshToken, setRefreshToken] = useState(0);
  const [activeView, setActiveView] = useState<MarketView>(initialView);
  const [query, setQuery] = useState("");
  const [debouncedQuery, setDebouncedQuery] = useState("");
  const [channelFilterId, setChannelFilterId] = useState<string | null>(
    initialView === "offers" ? channelContext?.channelId ?? null : null,
  );
  const [page, setPage] = useState(1);
  const [catalogSort, setCatalogSort] = useState<{
    key: CatalogSort;
    order: MarketOrder;
  }>({ key: "model", order: "default" });
  const [offerSort, setOfferSort] = useState<{
    key: OfferSort;
    order: MarketOrder;
  }>({ key: "channel", order: "asc" });
  const [expandedModels, setExpandedModels] = useState<Set<string>>(new Set());
  const [expandedOfferGroups, setExpandedOfferGroups] = useState<Set<string>>(new Set());
  const mountedRef = useRef(false);
  const platformAvailableRef = useRef(platformAvailable);
  const requestVersionRef = useRef(0);
  const requestInFlightRef = useRef(false);
  const pendingRequestRef = useRef<MarketPriceRequest | null>(null);
  const [refreshGate] = useState(createRefreshGate);

  platformAvailableRef.current = platformAvailable;

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      requestVersionRef.current += 1;
      pendingRequestRef.current = null;
    };
  }, []);

  useEffect(() => {
    const timer = window.setTimeout(() => {
      setDebouncedQuery(query.trim());
    }, 220);
    return () => window.clearTimeout(timer);
  }, [query]);

  const drainMarketRequests = useCallback(async () => {
    if (requestInFlightRef.current) return;
    requestInFlightRef.current = true;
    try {
      while (
        mountedRef.current
        && platformAvailableRef.current
        && document.visibilityState === "visible"
        && pendingRequestRef.current
      ) {
        await new Promise((resolve) => window.setTimeout(resolve, MARKET_REQUEST_SETTLE_MS));
        if (!mountedRef.current || !platformAvailableRef.current || document.visibilityState !== "visible") break;

        const request = pendingRequestRef.current;
        pendingRequestRef.current = null;
        if (!request) continue;

        try {
          const nextSnapshot = await priceCache.fetch(request.scope, {
            view: request.view,
            query: request.query,
            channelId: request.channelId,
            sort: request.sort,
            order: request.order,
            page: request.page,
            pageSize: request.pageSize,
          });
          if (
            mountedRef.current
            && platformAvailableRef.current
            && request.version === requestVersionRef.current
          ) {
            if (
              nextSnapshot?.pagination
              && nextSnapshot.pagination.page !== request.page
            ) {
              setPage(nextSnapshot.pagination.page);
            }
            setSnapshotState({
              scope: request.scope,
              data: nextSnapshot,
            });
          }
        } catch (reason) {
          if (
            mountedRef.current
            && platformAvailableRef.current
            && request.version === requestVersionRef.current
          ) {
            refreshGate.reset();
            setError(String(reason));
          }
        }
      }
    } finally {
      requestInFlightRef.current = false;
      if (mountedRef.current && !pendingRequestRef.current) {
        setLoading(false);
      }
    }
  }, [priceCache, refreshGate]);

  useEffect(() => {
    setExpandedModels(new Set());
    setExpandedOfferGroups(new Set(activeView === "offers" && channelFilterId ? [channelFilterId] : []));
  }, [activeView, catalogSort, offerSort, channelFilterId, debouncedQuery, page, cacheScope]);

  useEffect(() => {
    if (!platformAvailable) {
      requestVersionRef.current += 1;
      pendingRequestRef.current = null;
      refreshGate.reset();
      setSnapshotState({ scope: cacheScope, data: null });
      setError("");
      setLoading(false);
      return;
    }

    const activeSort = activeView === "catalog" ? catalogSort : offerSort;
    const version = ++requestVersionRef.current;
    const request = {
      version,
      scope: cacheScope,
      view: activeView,
      query: debouncedQuery || null,
      channelId: activeView === "offers" ? channelFilterId : null,
      sort: activeSort.key,
      order: activeSort.order,
      page,
      pageSize: MARKET_PAGE_SIZE,
    };
    const cached = priceCache.peek(cacheScope, request);
    if (cached) setSnapshotState({ scope: cacheScope, data: cached });
    if (document.visibilityState !== "visible") return;
    refreshGate.begin({ force: true });
    setLoading(true);
    setError("");
    pendingRequestRef.current = request;
    void drainMarketRequests();
  }, [
    activeView,
    catalogSort,
    cacheScope,
    channelFilterId,
    debouncedQuery,
    drainMarketRequests,
    offerSort,
    page,
    platformAvailable,
    priceCache,
    refreshGate,
    refreshToken,
  ]);

  useEffect(() => {
    if (!platformAvailable) {
      return undefined;
    }
    // Capacity changes the indicative price; poll only while this dialog is open and visible.
    const timer = window.setInterval(() => {
      if (document.visibilityState !== "visible") return;
      if (refreshGate.begin({ throttle: true })) {
        setRefreshToken((current) => current + 1);
      }
    }, MARKET_PRICE_CACHE_TTL_MS);
    const resume = () => {
      if (document.visibilityState === "visible") setRefreshToken((current) => current + 1);
    };
    document.addEventListener("visibilitychange", resume);
    return () => {
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", resume);
    };
  }, [platformAvailable, refreshGate]);

  const refreshSnapshot = useCallback(() => {
    if (!refreshGate.begin({ throttle: true })) return;
    setRefreshToken((current) => current + 1);
  }, [refreshGate]);

  const offerGroups = useMemo(
    () => groupSupplierOffers(snapshot?.my_offers ?? []),
    [snapshot?.my_offers],
  );

  const snapshotMatchesView = Boolean(
    snapshot && (!snapshot.view || snapshot.view === activeView),
  );
  const visibleModels = snapshotMatchesView && activeView === "catalog"
    ? snapshot?.models ?? []
    : [];
  const visibleOfferGroups = snapshotMatchesView && activeView === "offers"
    ? offerGroups
    : [];
  const modelTotal = snapshot?.model_total
    ?? (snapshot?.view === "offers" ? 0 : snapshot?.models.length ?? 0);
  const offerGroupTotal = snapshot?.offer_group_total
    ?? (snapshot?.view === "catalog" ? 0 : offerGroups.length);
  const visibleWarnings = (snapshot?.warnings ?? []).filter(
    (warning) => warning !== "history_unavailable",
  );

  const observedAt = useMemo(() => {
    if (!snapshot?.observed_at) {
      return "\u2014";
    }
    const value = new Date(snapshot.observed_at);
    return Number.isNaN(value.getTime()) ? snapshot.observed_at : value.toLocaleString();
  }, [snapshot?.observed_at]);

  const toggleModel = (model: string) => {
    setExpandedModels((current) => {
      const next = new Set(current);
      if (next.has(model)) {
        next.delete(model);
      } else {
        next.add(model);
      }
      return next;
    });
  };

  const toggleOfferGroup = (groupKey: string) => {
    setExpandedOfferGroups((current) => {
      const next = new Set(current);
      if (next.has(groupKey)) {
        next.delete(groupKey);
      } else {
        next.add(groupKey);
      }
      return next;
    });
  };

  const changeView = (view: MarketView) => {
    if (view === activeView) {
      return;
    }
    setActiveView(view);
    setPage(1);
    setExpandedModels(new Set());
    setExpandedOfferGroups(new Set(
      view === "offers" && channelFilterId ? [channelFilterId] : [],
    ));
  };

  const changeCatalogSort = (key: CatalogSort) => {
    setCatalogSort((current) => ({
      key,
      order: key === "model"
        ? (current.key !== "model" ? "default"
          : current.order === "default" ? "asc"
            : current.order === "asc" ? "desc" : "default")
        : current.key === key && current.order === "desc" ? "asc" : "desc",
    }));
    setPage(1);
    setExpandedModels(new Set());
  };

  const changeOfferSort = (key: OfferSort) => {
    setOfferSort((current) => ({
      key,
      order: current.key === key
        ? (current.order === "desc" ? "asc" : "desc")
        : (key === "channel" ? "asc" : "desc"),
    }));
    setPage(1);
    setExpandedOfferGroups(new Set());
  };

  return (
    <DialogBackdrop
      className="market-prices-backdrop"
      aria-labelledby="market-prices-title"
    >
      <div className="modal-panel market-prices-modal">
        <div className="section-title-row market-prices-title">
          <div>
            <h2 id="market-prices-title">{t("market.title")}</h2>
            <p className="section-hint">{t("market.subtitle")}</p>
          </div>
          <div className="market-prices-title-actions">
            <ModalCloseButton onClick={onClose} />
          </div>
        </div>

        <div className="market-overview-shell">
          <dl className="market-overview">
            <div>
              <dt>{t("market.routingPolicy")}</dt>
              <dd>
                <span>{t("market.withinBest")}</span>
                <small>{t("market.routingPolicyHint")}</small>
              </dd>
            </div>
            <div>
              <dt>{t("market.updatedAt")}</dt>
              <dd>{observedAt}</dd>
            </div>
          </dl>
          {platformAvailable && (
            <button
              className="quiet market-overview-refresh"
              type="button"
              disabled={loading}
              onClick={refreshSnapshot}
            >
              <RefreshCw className={loading ? "spin-icon" : ""} size={15} />
              {t("common.refresh")}
            </button>
          )}
        </div>

        <div className="market-snapshot-content">
          {!platformAvailable && (
            <div className="market-inline-notice" role="status">
              <CircleDollarSign aria-hidden="true" size={20} />
              <div>
                <strong>{t("market.platformRequired")}</strong>
                <span>{t("market.platformRequiredHint")}</span>
              </div>
            </div>
          )}

          {platformAvailable && error && (
            <div className="market-inline-notice" role="status">
              <CircleDollarSign aria-hidden="true" size={20} />
              <div>
                <strong>{t("market.unavailable")}</strong>
                <span>{marketErrorMessage(error, t)}</span>
              </div>
            </div>
          )}

          {visibleWarnings.length > 0 && (
            <div className="market-inline-notice" role="status">
              <CircleDollarSign aria-hidden="true" size={20} />
              <div>
                <strong>{t("market.partialData")}</strong>
                {visibleWarnings.map((warning) => (
                  <span key={warning}>{marketWarningMessage(warning, t)}</span>
                ))}
              </div>
            </div>
          )}

          {loading && !snapshot ? (
            <div className="market-empty">{t("common.loading")}</div>
          ) : !platformAvailable ? (
            <div className="market-empty">{t("market.platformPlaceholder")}</div>
          ) : error && !snapshot ? (
            <div className="market-empty">{t("market.unavailablePlaceholder")}</div>
          ) : snapshot ? (
            <div className="market-data-shell">
              <div className="market-data-toolbar">
                <div
                  className="market-view-tabs"
                  role="tablist"
                  aria-label={t("market.viewAria")}
                >
                  <button
                    className={activeView === "catalog" ? "active" : ""}
                    data-testid="market-catalog-tab"
                    type="button"
                    role="tab"
                    aria-selected={activeView === "catalog"}
                    onClick={() => changeView("catalog")}
                  >
                    {t("market.catalogTab")}
                    <span>{modelTotal}</span>
                  </button>
                  <button
                    className={activeView === "offers" ? "active" : ""}
                    data-testid="market-offers-tab"
                    type="button"
                    role="tab"
                    aria-selected={activeView === "offers"}
                    onClick={() => changeView("offers")}
                  >
                    {t("market.offersTab")}
                    <span>{offerGroupTotal}</span>
                  </button>
                </div>
                <label className="market-search">
                  <Search aria-hidden="true" size={15} />
                  <input
                    type="search"
                    value={query}
                    aria-label={t("market.searchAria")}
                    placeholder={t("market.searchPlaceholder")}
                    onChange={(event) => {
                      setQuery(event.currentTarget.value);
                      setPage(1);
                    }}
                  />
                </label>
              </div>

              {activeView === "catalog" ? (
                <div
                  className="market-view-panel"
                  role="tabpanel"
                  aria-label={t("market.catalogTab")}
                >
                  <div className="market-view-intro">
                    <p>{t("market.catalogHint")}</p>
                    <span>{t("market.resultCount", {
                      filtered: snapshot.pagination?.total ?? visibleModels.length,
                      total: modelTotal,
                    })}</span>
                  </div>
                  {visibleModels.length > 0 ? (
                    <>
                      <div className="market-list-header market-model-grid" role="row">
                        <MarketSortButton
                          active={catalogSort.key === "model"}
                          direction={catalogSort.order}
                          label={t(catalogSort.key === "model" && catalogSort.order === "default"
                            ? "market.columns.modelDefault" : "market.columns.model")}
                          title={t("market.modelSortHint")}
                          onClick={() => changeCatalogSort("model")}
                        />
                        <span
                          className="market-column-label"
                          title={t("market.modelStatus.hint")}
                        >
                          {t("market.columns.status")}
                        </span>
                        <MarketSortButton
                          active={catalogSort.key === "input"}
                          direction={catalogSort.order}
                          label={t("market.columns.inputPrice")}
                          onClick={() => changeCatalogSort("input")}
                        />
                        <MarketSortButton
                          active={catalogSort.key === "output"}
                          direction={catalogSort.order}
                          label={t("market.columns.outputPrice")}
                          onClick={() => changeCatalogSort("output")}
                        />
                        <MarketSortButton
                          active={catalogSort.key === "ratio"}
                          direction={catalogSort.order}
                          label={t("market.columns.executableRatio")}
                          onClick={() => changeCatalogSort("ratio")}
                        />
                        <span aria-hidden="true" />
                      </div>
                      <div className="market-price-list">
                        {visibleModels.map((model) => {
                          const expanded = expandedModels.has(model.model);
                          const componentCount = model.reference_components?.length ?? 0;
                          const inputPrice = tokenPriceComponent(model, "input_tokens");
                          const outputPrice = tokenPriceComponent(model, "output_tokens");
                          const availability = marketModelAvailability(model);
                          const availabilityLabel = t(`market.modelStatus.${availability.state}`);
                          const availabilityCounts = (availability.customReady ?? 0) > 0
                            ? t(availability.limited > 0
                              ? "market.modelStatus.readyWithCustomAndLimited"
                              : "market.modelStatus.readyWithCustom", {
                              ready: availability.ready,
                              custom: availability.customReady,
                              limited: availability.limited,
                            })
                            : availability.limited > 0 ? t("market.modelStatus.readyAndLimited", {
                              ready: availability.ready,
                              limited: availability.limited,
                            })
                            : t("market.modelStatus.readyOnly", {
                              ready: availability.ready,
                            });
                          return (
                            <article className="market-model-card" key={model.model}>
                              <div className="market-model-heading market-model-grid">
                                <div className="market-model-identity">
                                  <h3 title={model.model}>{modelDisplayName(model.model)}</h3>
                                  <small>{t("market.billingItems", {
                                    count: componentCount,
                                  })}{modelContextLabel(model.context_tokens) && ` · ${t("market.contextWindow", { capacity: modelContextLabel(model.context_tokens) })}`}</small>
                                </div>
                                <div
                                  className={`market-model-status market-model-status-${availability.state}`}
                                  aria-label={t("market.modelStatus.aria", {
                                    status: availabilityLabel,
                                    counts: availabilityCounts,
                                  })}
                                  title={t("market.modelStatus.hint")}
                                >
                                  <strong>
                                    <span className="market-model-status-dot" aria-hidden="true" />
                                    {availabilityLabel}
                                  </strong>
                                  <small>{availabilityCounts}</small>
                                </div>
                                <div
                                  className="market-row-metric market-row-token-price market-row-input-price"
                                  aria-label={t("market.columns.inputPrice")}
                                  title={t("market.indicativePriceHint")}
                                >
                                  <strong>{indicativeUnitPrice(inputPrice, model, cnyPerUSD)}</strong>
                                  {inputPrice && <small>{unitLabel(inputPrice.unit, t)}</small>}
                                </div>
                                <div
                                  className="market-row-metric market-row-token-price market-row-output-price"
                                  aria-label={t("market.columns.outputPrice")}
                                  title={t("market.indicativePriceHint")}
                                >
                                  <strong>{indicativeUnitPrice(outputPrice, model, cnyPerUSD)}</strong>
                                  {outputPrice && <small>{unitLabel(outputPrice.unit, t)}</small>}
                                </div>
                                <div className="market-row-metric market-row-metric-wide market-row-executable">
                                  <strong>
                                    {model.ready_supplier_count > 0
                                      ? formatRatio(model.indicative_ratio)
                                      : "\u2014"}
                                  </strong>
                                </div>
                                <button
                                  className="market-expand-button"
                                  type="button"
                                  aria-expanded={expanded}
                                  aria-label={t(
                                    expanded ? "market.collapseModel" : "market.expandModel",
                                    { model: modelDisplayName(model.model) },
                                  )}
                                  onClick={() => toggleModel(model.model)}
                                >
                                  <ChevronDown aria-hidden="true" size={17} />
                                </button>
                              </div>

                              {expanded && (
                                (model.reference_components ?? []).length > 0 ? (
                                  <>
                                    {model.pricing_confidence === "variant_fallback" && (
                                      <p className="section-hint" title={model.pricing_rule_id}>
                                        {t("market.variants.modelBasis")}
                                      </p>
                                    )}
                                    <div className="market-component-list">
                                      {(model.reference_components ?? []).map((component, index) => (
                                        <div
                                          className="market-component-row"
                                          key={`${component.meter_kind}|${component.qualifier ?? ""}|${component.unit}|${index}`}
                                        >
                                          <div>
                                            <span>{componentLabel(component, t)}</span>
                                            <small>{t("market.catalogPrice", {
                                              price: formatMoney(component.price_usd, cnyPerUSD),
                                            })}</small>
                                          </div>
                                          <div className="market-component-price">
                                            <strong>{executableUnitPrice(component, model, cnyPerUSD)}</strong>
                                            <small>{unitLabel(component.unit, t)}</small>
                                          </div>
                                        </div>
                                      ))}
                                    </div>
                                  </>
                                ) : (
                                  <p className="market-no-components">
                                    {t("market.noPriceComponents")}
                                  </p>
                                )
                              )}
                            </article>
                          );
                        })}
                      </div>
                      <MarketPager
                        pagination={snapshot.pagination}
                        loading={loading}
                        onPageChange={(nextPage) => {
                          setPage(nextPage);
                          setExpandedModels(new Set());
                        }}
                        t={t}
                      />
                    </>
                  ) : (
                    <div className="market-empty market-empty-compact">
                      {modelTotal > 0
                        ? t("market.noMatches")
                        : t("market.noModels")}
                    </div>
                  )}
                </div>
              ) : (
                <div
                  className="market-view-panel"
                  role="tabpanel"
                  aria-label={t("market.offersTab")}
                >
                  <div className="market-view-intro">
                    <p>{t("market.offersHint")}</p>
                    <span>{t("market.resultCount", {
                      filtered: snapshot.pagination?.total ?? visibleOfferGroups.length,
                      total: offerGroupTotal,
                    })}</span>
                  </div>
                  {channelFilterId && channelContext && (
                    <div className="market-channel-scope" role="status">
                      <span>{t("market.currentChannelScope", {
                        channel: channelContext.channelName,
                      })}</span>
                      <button
                        type="button"
                        onClick={() => {
                          setChannelFilterId(null);
                          setPage(1);
                          setExpandedOfferGroups(new Set());
                        }}
                      >
                        {t("market.showAllOffers")}
                      </button>
                    </div>
                  )}
                  {visibleOfferGroups.length > 0 ? (
                    <>
                      <div className="market-list-header market-offer-grid" role="row">
                        <MarketSortButton
                          active={offerSort.key === "channel"}
                          direction={offerSort.order}
                          label={t("market.columns.channel")}
                          onClick={() => changeOfferSort("channel")}
                        />
                        <MarketSortButton
                          active={offerSort.key === "models"}
                          direction={offerSort.order}
                          label={t("market.columns.models")}
                          onClick={() => changeOfferSort("models")}
                        />
                        <MarketSortButton
                          active={offerSort.key === "ready"}
                          direction={offerSort.order}
                          label={t("market.columns.ready")}
                          onClick={() => changeOfferSort("ready")}
                        />
                        <MarketSortButton
                          active={offerSort.key === "ratio"}
                          direction={offerSort.order}
                          label={t("market.columns.currentRatio")}
                          onClick={() => changeOfferSort("ratio")}
                        />
                        <span className="market-column-label" title={t("market.priceWeightHint")}>
                          {t("market.columns.priceWeight")}
                        </span>
                        <span aria-hidden="true" />
                      </div>
                      <div className="market-offer-groups">
                        {visibleOfferGroups.map((group) => {
                          const expanded = expandedOfferGroups.has(group.key);
                          return (
                            <section className="market-offer-group" key={group.key}>
                              <div className="market-offer-group-heading market-offer-grid">
                                <span className="market-offer-group-identity">
                                  <strong>{group.name}</strong>
                                  <small>{t("market.channelOfferSummary", {
                                    count: group.offers.length,
                                  })}</small>
                                </span>
                                <span className="market-offer-group-metric">
                                  <strong>{group.offers.length}</strong>
                                </span>
                                <span className="market-offer-group-metric">
                                  <strong>{group.readyCount}</strong>
                                </span>
                                <span className="market-offer-group-metric">
                                  <strong>{formatRatioRange(group.postedMin, group.postedMax)}</strong>
                                </span>
                                <span className="market-offer-group-metric" title={t("market.priceWeightHint")}>
                                  <strong>{formatAveragePriceWeight(
                                    group.priceWeightTotal,
                                    group.offers.length,
                                  )}</strong>
                                </span>
                                <button
                                  className="market-expand-button"
                                  type="button"
                                  aria-expanded={expanded}
                                  aria-label={t(
                                    expanded ? "market.collapseChannel" : "market.expandChannel",
                                    { channel: group.name },
                                  )}
                                  onClick={() => toggleOfferGroup(group.key)}
                                >
                                  <ChevronDown aria-hidden="true" size={17} />
                                </button>
                              </div>
                              {expanded && (
                                <>
                                  <table
                                    className="market-offer-table"
                                    aria-label={`${t("market.myOffers")} · ${group.name}`}
                                  >
                                    <thead>
                                      <tr>
                                        <th scope="col">{t("market.offerModel")}</th>
                                        <th scope="col">{t("market.posted")}</th>
                                        <th scope="col">{t("market.position")}</th>
                                        <th scope="col" title={t("market.suggestedHint")}>{t("market.suggested")}</th>
                                      </tr>
                                    </thead>
                                    <tbody>
                                      {group.offers.map((offer) => (
                                        <tr key={`${offer.offer_version}|${offer.model}`}>
                                          <td data-label={t("market.offerModel")}>
                                            <strong title={offer.model}>{modelDisplayName(offer.model)}</strong>
                                          </td>
                                          <td data-label={t("market.posted")}>
                                            {formatRatio(offer.posted_ratio)}
                                          </td>
                                          <td data-label={t("market.position")} title={t(`market.positionHints.${offer.position}`, {
                                            defaultValue: t("market.positionHint"),
                                          })}>
                                            {t(`market.positions.${offer.position}`, {
                                              defaultValue: offer.position,
                                            })}
                                          </td>
                                          <td data-label={t("market.suggested")}>
                                            {formatRatio(offer.suggested_ratio)}
                                          </td>
                                        </tr>
                                      ))}
                                    </tbody>
                                  </table>
                                </>
                              )}
                            </section>
                          );
                        })}
                      </div>
                      <MarketPager
                        pagination={snapshot.pagination}
                        loading={loading}
                        onPageChange={(nextPage) => {
                          setPage(nextPage);
                          setExpandedOfferGroups(new Set());
                        }}
                        t={t}
                      />
                    </>
                  ) : (
                    <div className="market-empty market-empty-compact">
                      {offerGroupTotal > 0
                        ? t("market.noMatches")
                        : t("market.noOffers")}
                    </div>
                  )}
                </div>
              )}
            </div>
          ) : null}
        </div>

        <p className="market-disclaimer">
          {t("market.disclaimer", { count: snapshot?.history_record_limit ?? 5000 })}
        </p>
      </div>
    </DialogBackdrop>
  );
}
