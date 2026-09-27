import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";

import {
  MarketPricesModal,
  marketModelAvailability,
  type MarketPriceSnapshot,
} from "./MarketPricesModal";
import { changeAppLanguage } from "./i18n";
import { useEscapeDismiss } from "./useEscapeDismiss";
import { MARKET_PRICE_CACHE_TTL_MS, createMarketPriceCache } from "./marketPriceCache";

const invoke = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({ invoke }));

afterEach(() => vi.restoreAllMocks());

const snapshot: MarketPriceSnapshot = {
  currency: "USD",
  observed_at: "2026-07-28T00:00:00Z",
  history_window_seconds: 86_400,
  history_record_limit: 5000,
  routing_policy: "inverse_square",
  price_exponent: 2,
  max_price_ratio: 3,
  models: [{
    model: "gpt-market",
    currency: "USD",
    price_version_id: "price-v1",
    pricing_rule_id: "rule-1",
    reference_components: [
      { meter_kind: "input_tokens", unit: "per_1m_tokens", price_usd: 2 },
      { meter_kind: "output_tokens", unit: "per_1m_tokens", price_usd: 10 },
    ],
    ready_supplier_count: 2,
    routable_supplier_count: 3,
    best_ratio: 0.8,
    indicative_ratio: 0.83,
    recent: {
      transaction_count: 3,
      supplier_count: 2,
      weighted_median_ratio: 0.85,
    },
  }],
  my_offers: [{
    channel_id: "channel-a",
    supplier_unit_id: "supplier-a",
    name: "My supplier",
    model: "gpt-market",
    offer_version: "offer-v1",
    posted_ratio: 0.9,
    ready: true,
    has_comparison_market: true,
    relative_price_weight: 0.79,
    suggested_ratio: 0.8,
    position: "weighted_price",
  }, {
    channel_id: "channel-a",
    supplier_unit_id: "supplier-a",
    name: "My supplier",
    model: "gpt-market-mini",
    offer_version: "offer-v2",
    posted_ratio: 0.82,
    ready: true,
    has_comparison_market: true,
    relative_price_weight: 0.95,
    suggested_ratio: 0.82,
    position: "weighted_price",
  }],
};

describe("MarketPricesModal", () => {
  test("shows context capacity once without a separate variant rules section", async () => {
    invoke.mockResolvedValue({data:{...snapshot, models:[{...snapshot.models[0], model:"claude-sonnet-4-6", context_tokens:1000000}], variant_rules:[{id:"const-1m-v2",suffix:"[1m]",mode:"multiplier",enabled:true,multiplier:1}]}});
    render(<MarketPricesModal platformAvailable onClose={vi.fn()} />);
    await screen.findByRole("heading", {name:"claude-sonnet-4-6"});
    expect(screen.getByText("2 项计费 · 1M 上下文")).toBeInTheDocument();
    expect(screen.queryByTestId("market-variant-rules")).not.toBeInTheDocument();
    expect(screen.queryByRole("heading", {name:/\[1m\]/})).not.toBeInTheDocument();
  });
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockResolvedValue({ data: snapshot });
  });

  test("hides catalog-owned variant rules while preserving free prices from older servers", async () => {
    const user = userEvent.setup();
    const { unmount } = render(<MarketPricesModal platformAvailable onClose={vi.fn()} />);
    await screen.findByRole("heading", { name: "gpt-market" });
    expect(screen.queryByTestId("market-variant-rules")).not.toBeInTheDocument();
    unmount();
    invoke.mockResolvedValue({ data: { ...snapshot, variant_rules: [
      { id: "free-v1", suffix: ":free", mode: "free", enabled: true },
      { id: "minimum-v1", suffix: ":minimum", mode: "minimum", enabled: true, input_usd: .1, output_usd: .3, cached_input_usd: .01 },
      { id: "1m-v1", suffix: "[1m]", mode: "multiplier", enabled: true, multiplier: 1.25 },
      { id: "off", suffix: ":disabled", mode: "multiplier", enabled: false, multiplier: 2 },
    ], models: [{ ...snapshot.models[0], model: "gpt-market:free", pricing_confidence: "variant_fallback",
      indicative_ratio: 3,
      reference_components: [
        { meter_kind: "input_tokens", unit: "per_1m_tokens", price_usd: 0 },
        { meter_kind: "output_tokens", unit: "per_1m_tokens", price_usd: 0 },
        { meter_kind: "cached_input_tokens", unit: "per_1m_tokens", price_usd: 0 },
        { meter_kind: "cache_write_tokens", unit: "per_1m_tokens", price_usd: 0 },
      ],
    }] } });
    const { container } = render(<MarketPricesModal platformAvailable onClose={vi.fn()} />);
    await screen.findByRole("heading", { name: "gpt-market:free" });
    expect(screen.queryByTestId("market-variant-rules")).not.toBeInTheDocument();
    expect(screen.queryByText("通用变体计价规则")).not.toBeInTheDocument();
    const prices = container.querySelectorAll(".market-row-token-price");
    expect(prices).toHaveLength(2);
    for (const price of prices) {
      expect(price).toHaveTextContent("¥0");
      expect(price).not.toHaveTextContent("—");
    }
    await user.click(container.querySelector<HTMLButtonElement>(".market-expand-button")!);
    expect(await screen.findByText(/此模型使用通用变体参考价/)).toBeInTheDocument();
  });

  test("shows short price and offer names while retaining full request identities", async () => {
    invoke.mockResolvedValue({ data: {
      ...snapshot,
      models: [{ ...snapshot.models[0], model: "qwen/qwen3.8-27b" }],
      my_offers: [{ ...snapshot.my_offers[0], model: "qwen/qwen3.8-27b" }],
    } });
    const user = userEvent.setup();
    const { container, unmount } = render(<MarketPricesModal platformAvailable onClose={vi.fn()} />);
    const heading = await screen.findByRole("heading", { name: "qwen3.8-27b" });
    expect(heading).toHaveAttribute("title", "qwen/qwen3.8-27b");
    await user.click(container.querySelector<HTMLButtonElement>(".market-expand-button")!);
    expect(container.querySelector(".market-component-list")).toBeInTheDocument();
    unmount();
    render(<MarketPricesModal platformAvailable initialView="offers" onClose={vi.fn()} />);
    await user.click(await screen.findByRole("button", { name: /My supplier/ }));
    expect(await screen.findByText("qwen3.8-27b")).toHaveAttribute("title", "qwen/qwen3.8-27b");
  });

  test("keeps catalog and channel rows compact until details are requested", async () => {
    const user = userEvent.setup();
    const { container } = render(
      <MarketPricesModal platformAvailable onClose={vi.fn()} />,
    );

    expect(await screen.findByRole("heading", { name: "gpt-market" })).toBeInTheDocument();
    expect(container.querySelector(".market-component-list")).not.toBeInTheDocument();
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
    expect(container.querySelector(".market-search")).toBeInTheDocument();
    expect(container.querySelector(".market-disclaimer")).not.toHaveTextContent("24 小时");
    expect(screen.getByText("当前预计倍率")).toBeInTheDocument();
    expect(screen.getByText("当前状态")).toBeInTheDocument();
    expect(screen.getByText("可用")).toBeInTheDocument();
    expect(screen.getByText("2 可用 · 1 受限")).toBeInTheDocument();
    expect(screen.getByText("0.83\u00d7")).toBeInTheDocument();
    expect(screen.queryByText("24 小时成交")).not.toBeInTheDocument();
    const tokenPrices = container.querySelectorAll(".market-row-token-price");
    expect(tokenPrices).toHaveLength(2);
    expect(tokenPrices[0]).toHaveTextContent("\u00a511.62");
    expect(tokenPrices[1]).toHaveTextContent("\u00a558.1");
    expect(tokenPrices[0]).not.toHaveTextContent("$");
    expect(tokenPrices[1]).not.toHaveTextContent("$");

    const modelExpand = container.querySelector<HTMLButtonElement>(".market-expand-button");
    expect(modelExpand).not.toBeNull();
    await user.click(modelExpand!);
    expect(container.querySelector(".market-component-list")).toBeInTheDocument();

    await user.click(screen.getByTestId("market-offers-tab"));
    expect(await screen.findByText("My supplier")).toBeInTheDocument();
    await waitFor(() => {
      expect(invoke).toHaveBeenNthCalledWith(2, "fetch_market_prices", {
        view: "offers",
        query: null,
        channelId: null,
        sort: "channel",
        order: "asc",
        page: 1,
        pageSize: 10,
      });
      expect(screen.getByRole("button", { name: "刷新" })).not.toBeDisabled();
    });
    expect(screen.queryByRole("table")).not.toBeInTheDocument();

    const offerGroupHeading = container.querySelector<HTMLElement>(
      ".market-offer-group-heading",
    );
    expect(offerGroupHeading).not.toBeNull();
    await user.click(offerGroupHeading!);
    expect(screen.queryByRole("table")).not.toBeInTheDocument();

    const channelExpand = container.querySelector<HTMLButtonElement>(
      ".market-offer-group .market-expand-button",
    );
    expect(channelExpand).not.toBeNull();
    await user.click(channelExpand!);
    expect(screen.getByRole("table")).toBeInTheDocument();
    expect(screen.getByText("gpt-market-mini")).toBeInTheDocument();

    expect(invoke).toHaveBeenNthCalledWith(1, "fetch_market_prices", {
      view: "catalog",
      query: null,
      channelId: null,
      sort: "model",
      order: "default",
      page: 1,
      pageSize: 10,
    });
  });

  test("shows only converted CNY prices for the Chinese interface", async () => {
    await changeAppLanguage("zh-CN");
    const { container } = render(
      <MarketPricesModal
        platformAvailable
        cnyPerUSD={7.2}
        onClose={vi.fn()}
      />,
    );

    expect(await screen.findByRole("heading", { name: "gpt-market" })).toBeInTheDocument();
    const tokenPrices = container.querySelectorAll(".market-row-token-price");
    expect(tokenPrices[0]).toHaveTextContent("\u00a511.952");
    expect(tokenPrices[1]).toHaveTextContent("\u00a559.76");
    expect(tokenPrices[0]).not.toHaveTextContent("$");
    expect(tokenPrices[1]).not.toHaveTextContent("$");
  });

  test("shows ready, temporarily limited, and unavailable model states", async () => {
    const baseModel = snapshot.models[0];
    invoke.mockResolvedValue({
      data: {
        ...snapshot,
        models: [
          {
            ...baseModel,
            model: "model-ready",
            ready_supplier_count: 2,
            routable_supplier_count: 3,
          },
          {
            ...baseModel,
            model: "model-limited",
            ready_supplier_count: 0,
            routable_supplier_count: 2,
          },
          {
            ...baseModel,
            model: "model-unavailable",
            ready_supplier_count: 0,
            routable_supplier_count: 0,
          },
        ],
      },
    });

    const { container } = render(
      <MarketPricesModal platformAvailable onClose={vi.fn()} />,
    );

    expect(await screen.findByRole("heading", { name: "model-ready" })).toBeInTheDocument();
    expect(container.querySelector(".market-model-status-available"))
      .toHaveTextContent("可用2 可用 · 1 受限");
    expect(container.querySelector(".market-model-status-limited"))
      .toHaveTextContent("暂时受限0 可用 · 2 受限");
    expect(container.querySelector(".market-model-status-unavailable"))
      .toHaveTextContent("不可用0 可用");
  });

  test.each([
    ["zh-CN", "2 可用 (含自定义1) · 1 受限", "2 可用", "0 可用 · 2 受限", "1 可用 (含自定义1)"],
    ["en-US", "2 ready (including 1 custom) · 1 limited", "2 ready", "0 ready · 2 limited", "1 ready (including 1 custom)"],
  ] as const)("shows positive custom counts and hides zero counts in %s", async (language, mixed, zero, limited, ready) => {
    await changeAppLanguage(language);
    invoke.mockResolvedValue({ data: {
      ...snapshot,
      models: [
        { ...snapshot.models[0], model: "mixed", ready_custom_supplier_count: 1 },
        { ...snapshot.models[0], model: "non-custom", routable_supplier_count: 2, ready_custom_supplier_count: 0 },
        { ...snapshot.models[0], model: "limited", ready_supplier_count: 0, routable_supplier_count: 2, ready_custom_supplier_count: 0 },
        { ...snapshot.models[0], model: "custom-only", ready_supplier_count: 1, routable_supplier_count: 1, ready_custom_supplier_count: 1 },
      ],
    } });
    try {
      render(<MarketPricesModal platformAvailable onClose={vi.fn()} />);
      expect(await screen.findByText(mixed)).toBeInTheDocument();
      expect(screen.getByText(zero)).toBeInTheDocument();
      expect(screen.getByText(limited)).toBeInTheDocument();
      expect(screen.getByText(ready)).toBeInTheDocument();
      expect(screen.getByText(mixed).parentElement).toHaveAttribute("aria-label", expect.stringContaining(mixed));
    } finally {
      await changeAppLanguage("zh-CN");
    }
  });

  test("Escape closes only the market modal when its search input is focused", async () => {
    const user = userEvent.setup();
    const dismissParent = vi.fn();
    function Harness() {
      const [open, setOpen] = useState(false);
      useEscapeDismiss(dismissParent);
      return <>
        <button type="button" onClick={() => setOpen(true)}>Open market</button>
        {open && <MarketPricesModal platformAvailable onClose={() => setOpen(false)} />}
      </>;
    }
    render(<Harness />);
    await user.click(screen.getByRole("button", { name: "Open market" }));
    await screen.findByRole("heading", { name: "gpt-market" });
    await user.type(screen.getByRole("searchbox"), "market");
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(dismissParent).not.toHaveBeenCalled();
    await user.keyboard("{Escape}");
    expect(dismissParent).toHaveBeenCalledTimes(1);
  });

  test.each(["loading", "error", "standalone"])("Escape remains available in the %s state", async (state) => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    if (state === "loading") invoke.mockReturnValue(new Promise(() => {}));
    if (state === "error") invoke.mockRejectedValue(new Error("unavailable"));
    const { container } = render(
      <MarketPricesModal platformAvailable={state !== "standalone"} onClose={onClose} />,
    );
    if (state === "error") {
      await waitFor(() => expect(container.querySelector(".market-inline-notice")).toBeInTheDocument());
    }
    await user.keyboard("{Escape}");
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  test("throttles repeated refreshes without closing the modal", async () => {
    const now = vi.spyOn(Date, "now").mockReturnValue(1_000);
    const user = userEvent.setup();
    const { container } = render(
      <MarketPricesModal platformAvailable onClose={vi.fn()} />,
    );
    await screen.findByRole("heading", { name: "gpt-market" });

    const refresh = container.querySelector<HTMLButtonElement>(".market-overview-refresh");
    expect(refresh).not.toBeNull();
    await user.click(refresh!);
    expect(invoke).toHaveBeenCalledTimes(1);

    now.mockReturnValue(11_000);
    await user.click(refresh!);
    await waitFor(() => expect(refresh).not.toBeDisabled());
    expect(invoke).toHaveBeenCalledTimes(1);
    now.mockReturnValue(31_000);
    await user.click(refresh!);
    await waitFor(() => expect(invoke).toHaveBeenCalledTimes(2));
    now.mockRestore();
  });

  test("updates the visible indicative price at the market snapshot cadence", async () => {
    const now = vi.spyOn(Date, "now").mockReturnValue(1_000);
    const intervals = vi.spyOn(window, "setInterval");
    invoke.mockResolvedValueOnce({ data: snapshot, revision: "official-price" });
    invoke.mockResolvedValueOnce({
      data: { ...snapshot, models: [{ ...snapshot.models[0], indicative_ratio: 0.05 }] },
      revision: "fallback-price",
    });

    render(<MarketPricesModal platformAvailable onClose={vi.fn()} />);
    await screen.findByText("0.83×");
    const priceTimer = intervals.mock.calls.find(([, delay]) => delay === MARKET_PRICE_CACHE_TTL_MS)?.[0];
    expect(typeof priceTimer).toBe("function");

    now.mockReturnValue(31_001);
    act(() => {
      if (typeof priceTimer === "function") priceTimer();
    });
    await screen.findByText("0.05×");
    expect(invoke).toHaveBeenCalledTimes(2);
  });

  test("reopens immediately from the app cache without refetching a fresh page", async () => {
    const cache = createMarketPriceCache();
    const first = render(<MarketPricesModal platformAvailable cache={cache} cacheScope="account-a" onClose={vi.fn()} />);
    await screen.findByRole("heading", { name: "gpt-market" });
    first.unmount();
    render(<MarketPricesModal platformAvailable cache={cache} cacheScope="account-a" onClose={vi.fn()} />);
    expect(screen.getByRole("heading", { name: "gpt-market" })).toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole("button", { name: "刷新" })).not.toBeDisabled());
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  test("keeps cached prices and expanded rows during an unchanged background update", async () => {
    let now = 0;
    const cache = createMarketPriceCache(() => now);
    const user = userEvent.setup();
    invoke.mockResolvedValueOnce({ data: snapshot, revision: "price-rev" });
    const first = render(<MarketPricesModal platformAvailable cache={cache} cacheScope="account-a" onClose={vi.fn()} />);
    await screen.findByRole("heading", { name: "gpt-market" });
    first.unmount();
    now = 31_000;
    let complete!: (value: unknown) => void;
    invoke.mockImplementationOnce(() => new Promise((resolve) => { complete = resolve; }));
    const { container } = render(<MarketPricesModal platformAvailable cache={cache} cacheScope="account-a" onClose={vi.fn()} />);
    expect(screen.getByRole("heading", { name: "gpt-market" })).toBeInTheDocument();
    await user.click(container.querySelector<HTMLButtonElement>(".market-expand-button")!);
    await waitFor(() => expect(invoke).toHaveBeenCalledTimes(2));
    expect(invoke.mock.calls[1][1].ifRevision).toBe("price-rev");
    complete({ not_modified: true, revision: "price-rev", observed_at: "2026-09-10T12:00:00Z" });
    await waitFor(() => expect(screen.getByRole("button", { name: "刷新" })).not.toBeDisabled());
    expect(container.querySelector(".market-component-list")).toBeInTheDocument();
  });

  test("does not display another account's cached prices during an identity change", async () => {
    const cache = createMarketPriceCache();
    const view = render(<MarketPricesModal platformAvailable cache={cache} cacheScope="account-a" onClose={vi.fn()} />);
    await screen.findByRole("heading", { name: "gpt-market" });
    invoke.mockResolvedValueOnce({ data: { ...snapshot, models: [], my_offers: [] } });
    view.rerender(<MarketPricesModal platformAvailable cache={cache} cacheScope="account-b" onClose={vi.fn()} />);
    expect(screen.queryByRole("heading", { name: "gpt-market" })).not.toBeInTheDocument();
    await waitFor(() => expect(invoke).toHaveBeenCalledTimes(2));
  });

  test("defers a hidden price page until visible and reuses cache on quick visibility changes", async () => {
    const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    render(<MarketPricesModal platformAvailable onClose={vi.fn()} />);
    await new Promise((resolve) => setTimeout(resolve, 100));
    expect(invoke).not.toHaveBeenCalled();
    visibility.mockReturnValue("visible");
    document.dispatchEvent(new Event("visibilitychange"));
    await screen.findByRole("heading", { name: "gpt-market" });
    visibility.mockReturnValue("hidden");
    document.dispatchEvent(new Event("visibilitychange"));
    visibility.mockReturnValue("visible");
    document.dispatchEvent(new Event("visibilitychange"));
    await waitFor(() => expect(screen.getByRole("button", { name: "刷新" })).not.toBeDisabled());
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  test("requests a new server page after using the pager", async () => {
    const user = userEvent.setup();
    invoke.mockResolvedValueOnce({
      data: {
        ...snapshot,
        view: "catalog",
        pagination: {
          page: 1,
          page_size: 10,
          total: 13,
          total_pages: 2,
        },
      },
    }).mockResolvedValue({
      data: {
        ...snapshot,
        view: "catalog",
        pagination: {
          page: 2,
          page_size: 10,
          total: 13,
          total_pages: 2,
        },
      },
    });
    render(
      <MarketPricesModal platformAvailable onClose={vi.fn()} />,
    );
    await screen.findByRole("heading", { name: "gpt-market" });

    expect(screen.getByRole("button", { name: "上一页" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "下一页" })).toBeEnabled();
    expect(screen.getByLabelText("第 1 / 2 页 · 13 项")).toHaveTextContent("1");
    expect(screen.getByText("13 项")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "下一页" }));

    await waitFor(() => expect(invoke).toHaveBeenNthCalledWith(2, "fetch_market_prices", {
      view: "catalog",
      query: null,
      channelId: null,
      sort: "model",
      order: "default",
      page: 2,
      pageSize: 10,
    }));
    expect(await screen.findByLabelText("第 2 / 2 页 · 13 项")).toHaveTextContent("2");
    expect(screen.getByText("13 项")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "上一页" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "下一页" })).toBeDisabled();
  });

  test("requests server-side sorting for displayed input and output prices", async () => {
    const user = userEvent.setup();
    const { container } = render(
      <MarketPricesModal platformAvailable onClose={vi.fn()} />,
    );
    await screen.findByRole("heading", { name: "gpt-market" });

    let sortButtons = container.querySelectorAll<HTMLButtonElement>(
      ".market-list-header.market-model-grid .market-sort-button",
    );
    expect(sortButtons).toHaveLength(4);
    await user.click(sortButtons[1]);

    await waitFor(() => expect(invoke).toHaveBeenNthCalledWith(2, "fetch_market_prices", {
      view: "catalog",
      query: null,
      channelId: null,
      sort: "input",
      order: "desc",
      page: 1,
      pageSize: 10,
    }));

    sortButtons = container.querySelectorAll<HTMLButtonElement>(
      ".market-list-header.market-model-grid .market-sort-button",
    );
    await user.click(sortButtons[2]);

    await waitFor(() => expect(invoke).toHaveBeenNthCalledWith(3, "fetch_market_prices", {
      view: "catalog",
      query: null,
      channelId: null,
      sort: "output",
      order: "desc",
      page: 1,
      pageSize: 10,
    }));
  });

  test("cycles default and alphabetical model order while reusing the default page cache", async () => {
    const models = ["aaa-model", "gpt-market", "zzz-model"].map((model) => ({ ...snapshot.models[0], model }));
    invoke.mockImplementation((_command, request) => Promise.resolve({ data: {
      ...snapshot,
      models: request.order === "default" ? [models[1], models[0], models[2]]
        : request.order === "desc" ? [...models].reverse() : models,
    } }));
    const user = userEvent.setup();
    const { container } = render(<MarketPricesModal platformAvailable onClose={vi.fn()} />);
    const names = () => [...container.querySelectorAll(".market-model-identity h3")].map((node) => node.textContent);
    await screen.findByRole("button", { name: "模型（默认）" });
    expect(names()).toEqual(["gpt-market", "aaa-model", "zzz-model"]);
    expect(invoke).toHaveBeenLastCalledWith("fetch_market_prices", expect.objectContaining({sort:"model",order:"default",page:1}));
    await user.click(screen.getByRole("button", { name: "模型（默认）" }));
    await waitFor(() => expect(names()).toEqual(["aaa-model", "gpt-market", "zzz-model"]));
    expect(invoke).toHaveBeenLastCalledWith("fetch_market_prices", expect.objectContaining({order:"asc",page:1}));
    await user.click(screen.getByRole("button", { name: "模型" }));
    await waitFor(() => expect(names()).toEqual(["zzz-model", "gpt-market", "aaa-model"]));
    expect(invoke).toHaveBeenLastCalledWith("fetch_market_prices", expect.objectContaining({order:"desc",page:1}));
    await user.click(screen.getByRole("button", { name: "模型" }));
    await screen.findByRole("button", { name: "模型（默认）" });
    await waitFor(() => expect(screen.getByRole("button", {name:"刷新"})).not.toBeDisabled());
    expect(names()).toEqual(["gpt-market", "aaa-model", "zzz-model"]);
    expect(invoke).toHaveBeenCalledTimes(3);
  });

  test("explains standalone mode without requesting the platform", () => {
    const { container } = render(
      <MarketPricesModal platformAvailable={false} onClose={vi.fn()} />,
    );

    expect(container.querySelector(".market-inline-notice")).toBeInTheDocument();
    expect(container.querySelector(".market-empty")).toBeInTheDocument();
    expect(container.querySelector(".market-prices-modal")).not.toHaveClass("compact");
    expect(invoke).not.toHaveBeenCalled();
  });

  test("shows a legacy null empty page and refreshes successfully when supply returns", async () => {
    let now = Date.now();
    vi.spyOn(Date, "now").mockImplementation(() => now);
    const user = userEvent.setup();
    invoke.mockResolvedValueOnce({ data: {
      ...snapshot, models: null, my_offers: [], model_total: 0, offer_group_total: 0,
    }, revision: "empty" });
    const { container } = render(<MarketPricesModal platformAvailable onClose={vi.fn()} />);
    expect(await screen.findByText("当前没有可报价的模型")).toBeInTheDocument();
    expect(container.querySelector(".market-inline-notice")).not.toBeInTheDocument();
    expect(screen.getByRole("tab", { name: /市场价格\s*0$/ })).toBeInTheDocument();
    now += 30_001;
    invoke.mockResolvedValueOnce({ data: snapshot, revision: "supply-returned" });
    await user.click(screen.getByRole("button", { name: "刷新" }));
    expect(await screen.findByRole("heading", { name: "gpt-market" })).toBeInTheDocument();
    expect(invoke).toHaveBeenLastCalledWith("fetch_market_prices", expect.objectContaining({ ifRevision: "empty" }));
    expect(container.querySelector(".market-inline-notice")).not.toBeInTheDocument();
  });

  test("does not expose raw platform errors in the dialog", async () => {
    invoke.mockRejectedValue("local proxy returned HTTP 404 Not Found: 404 page not found");

    const { container } = render(
      <MarketPricesModal platformAvailable onClose={vi.fn()} />,
    );

    await waitFor(() => {
      expect(container.querySelector(".market-inline-notice")).toBeInTheDocument();
    });
    expect(screen.queryByText(/local proxy returned/)).not.toBeInTheDocument();
  });

  test("does not surface unavailable history when the client no longer presents history", async () => {
    invoke.mockResolvedValue({
      data: {
        ...snapshot,
        warnings: ["history_unavailable"],
      },
    });

    const { container } = render(
      <MarketPricesModal platformAvailable onClose={vi.fn()} />,
    );

    expect(await screen.findByRole("heading", { name: "gpt-market" })).toBeInTheDocument();
    expect(container.querySelector(".market-inline-notice")).not.toBeInTheDocument();
  });

  test("opens a supplier reference on the exact channel with price-weight evidence", async () => {
    render(
      <MarketPricesModal
        platformAvailable
        initialView="offers"
        channelContext={{
          channelId: "channel-a",
          channelName: "My supplier",
        }}
        onClose={vi.fn()}
      />,
    );

    expect(await screen.findByText("价格优势")).toBeInTheDocument();
    expect(screen.getByRole("table")).toBeInTheDocument();
    expect(screen.getByText("当前渠道：My supplier")).toBeInTheDocument();
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("fetch_market_prices", {
      view: "offers",
      query: null,
      channelId: "channel-a",
      sort: "channel",
      order: "asc",
      page: 1,
      pageSize: 10,
    }));
  });

  test("uses the same indicative ratio in expanded price details", async () => {
    const user = userEvent.setup();
    const { container } = render(
      <MarketPricesModal platformAvailable onClose={vi.fn()} />,
    );

    expect(await screen.findByText("0.83×")).toBeInTheDocument();
    const modelExpand = container.querySelector<HTMLButtonElement>(".market-expand-button");
    expect(modelExpand).not.toBeNull();
    await user.click(modelExpand!);
    expect(screen.getAllByText("\u00a511.62").length).toBeGreaterThanOrEqual(1);
  });
});

test("derives model availability from ready and fallback routes", () => {
  expect(marketModelAvailability({
    ready_supplier_count: 2,
    routable_supplier_count: 4,
  })).toEqual({ state: "available", ready: 2, limited: 2 });
  expect(marketModelAvailability({
    ready_supplier_count: 0,
    routable_supplier_count: 2,
  })).toEqual({ state: "limited", ready: 0, limited: 2 });
  expect(marketModelAvailability({
    ready_supplier_count: 0,
    routable_supplier_count: 0,
  })).toEqual({ state: "unavailable", ready: 0, limited: 0 });
  expect(marketModelAvailability({
    ready_supplier_count: 1,
  })).toEqual({ state: "available", ready: 1, limited: 0 });
});

test("normalizes custom counts without inventing a zero for older servers", () => {
  const counts = { ready_supplier_count: 2, routable_supplier_count: 3 };
  expect(marketModelAvailability({ ...counts, ready_custom_supplier_count: 1 }))
    .toEqual({ state: "available", ready: 2, limited: 1, customReady: 1 });
  expect(marketModelAvailability({ ...counts, ready_custom_supplier_count: 99 }).customReady).toBe(2);
  expect(marketModelAvailability({ ...counts, ready_custom_supplier_count: -1 }).customReady).toBe(0);
  expect(marketModelAvailability({ ...counts, ready_custom_supplier_count: 1.9 }).customReady).toBe(1);
  expect(marketModelAvailability(counts)).not.toHaveProperty("customReady");
  expect(marketModelAvailability({ ...counts, ready_custom_supplier_count: Number.NaN }))
    .not.toHaveProperty("customReady");
});
