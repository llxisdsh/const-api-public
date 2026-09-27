import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { QuotaVisual, quotaDetailLabel } from "./supplierQuota";

describe("QuotaVisual", () => {
  it("shows official windows for API-key plans without changing ordinary API channels", () => {
    const channel = { kind: "openai_compatible", discovery: { quota: true }, subscription: { daily_request_limit: 0 } };
    const health = { quota_windows: [{ source: "kimi_coding_usage", window: "5h", remaining_ratio: 0.8, used_percent: 20 }] };
    const { container, rerender } = render(<QuotaVisual health={health} channel={channel} formatCountdown={() => ""} />);
    expect(screen.getByText("5小时")).toBeVisible();
    expect(quotaDetailLabel(health, channel)).toContain("80%");
    expect(quotaDetailLabel({ quota_status: "unknown", remaining_ratio: 0 }, channel)).toBe("未知");
    rerender(<QuotaVisual health={health} channel={{ ...channel, discovery: { quota: false } }} formatCountdown={() => ""} />);
    expect(container).toBeEmptyDOMElement();
  });
  it.each([100.0000001, 100.01, 120])("caps displayed usage %s while leaving the raw observation intact", (used) => {
    const health = { quota_windows: [{ source: "anthropic_oauth_usage", window: "5h", used_percent: used, remaining_ratio: 0 }] };
    const { container } = render(<QuotaVisual channel={{ kind: "subscription_adapter", subscription: { daily_request_limit: 0 } }}
      health={health} formatCountdown={() => ""} />);
    expect(screen.getByText("已用 100%")).toBeVisible();
    expect(screen.getByText("0%")).toBeVisible();
    expect(container.querySelector<HTMLElement>(".quota-window-meter > span")?.style.width).toBe("0%");
    expect(health.quota_windows[0].used_percent).toBe(used);
  });

  it.each(["anthropic_oauth_usage", "anthropic_header", "openai_wham_usage", "codex_header", "codex_rate_limits"])(
    "keeps both primary periods directly visible, short first, including 0%% and 100%% usage: %s", (source) => {
      const { container } = render(<QuotaVisual channel={{ kind: "subscription_adapter", subscription: { daily_request_limit: 0 } }}
        health={{ quota_windows: [
          { source, window: "weekly", used_percent: 100, remaining_ratio: 0 },
          { source, window: "5h", used_percent: 0, remaining_ratio: 1 },
          { source: "anthropic_oauth_usage", window: "weekly_sonnet", used_percent: 100, remaining_ratio: 0 },
        ] }} formatCountdown={() => ""} />);
      const rows = [...container.querySelectorAll(".quota-visual > .quota-window-list > .quota-window-row")];
      expect(rows).toHaveLength(2);
      expect(rows[0]).toHaveTextContent("5小时");
      expect(rows[0]).toHaveTextContent("已用 0%");
      expect(rows[1]).toHaveTextContent("7天");
      expect(rows[1]).toHaveTextContent("已用 100%");
      expect(rows[0]).toBeVisible();
      expect(rows[1]).toBeVisible();
      expect(container.querySelector(".quota-extra")).not.toHaveAttribute("open");
    },
  );
  it.each([0.25, 0.5, 1, 99, 100])("shows Claude usage %s without confusing percentages and fractions", (used) => {
    render(<QuotaVisual channel={{ kind: "subscription_adapter", subscription: { daily_request_limit: 0 } }}
      health={{ quota_windows: [{ source: "anthropic_oauth_usage", window: "5h", used_percent: used,
        remaining_ratio: 1 - used / 100, checked_at_unix: Math.floor(Date.now() / 1000) }] }} formatCountdown={() => ""} />);
    expect(screen.getByText(`已用 ${used}%`)).toBeVisible();
    expect(screen.getByText(`${100 - used}%`)).toBeVisible();
  });

  it("keeps Claude main periods visible and extras folded without letting them replace the summary", () => {
    const health = { quota_windows: [
      { source: "anthropic_oauth_usage", window: "5h", remaining_ratio: 0.6, used_percent: 40 },
      { source: "anthropic_oauth_usage", window: "weekly", remaining_ratio: 0.3, used_percent: 70 },
      { source: "anthropic_oauth_usage", window: "weekly_sonnet", model: "claude-sonnet", remaining_ratio: 0, used_percent: 100 },
      { source: "anthropic_oauth_usage", window: "extra_usage", remaining_ratio: 0, used_percent: 100 },
    ] };
    const channel = { kind: "subscription_adapter", subscription: { daily_request_limit: 0 } };
    const { container } = render(<QuotaVisual health={health} channel={channel} formatCountdown={() => ""} />);
    expect(container.querySelectorAll(".quota-visual > .quota-window-list > .quota-window-row")).toHaveLength(2);
    expect(container.querySelectorAll(".quota-extra .quota-window-row")).toHaveLength(2);
    expect(screen.getByText("5小时")).toBeVisible();
    expect(quotaDetailLabel(health, channel)).not.toContain("模型额度");
  });

  it.each([
    [0.9975, "0.25%", "99.75%"],
    [0.0025, "99.75%", "0.25%"],
    [0.99999, "<0.01%", ">99.99%"],
  ] as const)("preserves precision when only remaining ratio %s is provided", (remaining, usedText, remainingText) => {
    render(<QuotaVisual channel={{ kind: "subscription_adapter", subscription: { daily_request_limit: 0 } }}
      health={{ quota_windows: [{ source: "anthropic_header", window: "5h", remaining_ratio: remaining }] }}
      formatCountdown={() => ""} />);
    expect(screen.getByText(`已用 ${usedText}`)).toBeVisible();
    expect(screen.getByText(remainingText)).toBeVisible();
  });

  it("does not invent an OpenAI short window and understands future explicit periods", () => {
    const channel = { kind: "subscription_adapter", subscription: { daily_request_limit: 0 } };
    const { container, rerender } = render(<QuotaVisual channel={channel} health={{ quota_windows: [
      { source: "openai_wham_usage", window: "weekly", token_type: "codex", remaining_ratio: 0.24 },
      { source: "openai_wham_usage", window: "spark:5h", token_type: "spark", remaining_ratio: 0 },
    ] }} formatCountdown={() => ""} />);
    expect(container.querySelectorAll(".quota-visual > .quota-window-list > .quota-window-row")).toHaveLength(1);
    expect(screen.queryByText("5 小时 · codex")).not.toBeInTheDocument();
    rerender(<QuotaVisual channel={channel} health={{ quota_windows: [
      { source: "openai_wham_usage", window: "3600s", token_type: "codex", remaining_ratio: 0.8 },
    ] }} formatCountdown={() => ""} />);
    expect(container.querySelectorAll(".quota-visual > .quota-window-list > .quota-window-row")).toHaveLength(1);
    expect(container.querySelector(".quota-extra")).toBeNull();
    expect(screen.getByText("1小时 · codex")).toBeVisible();
  });

  it("does not turn empty OpenAI slots into a synthetic full balance", () => {
    const channel = { kind: "subscription_adapter", subscription: { daily_request_limit: 0 } };
    const health = { remaining_ratio: 1, quota_windows: [
      { source: "codex_header", window: "secondary", used_percent: 0, remaining_ratio: 1, reset_at_unix: 0 },
      { source: "codex_rate_limits", window: "primary", remaining_ratio: 1 },
      { source: "openai_wham_usage", window: "secondary", used_percent: 0, remaining_ratio: 1,
        checked_at_unix: 100, reset_at_unix: 100 },
    ] };
    const { container } = render(<QuotaVisual channel={channel} health={health} formatCountdown={() => ""} />);
    expect(container.querySelector(".quota-window-row")).toBeNull();
    expect(quotaDetailLabel(health, channel)).toBe("未知");
  });

  it("preserves real unknown-period quotas and other providers' zero-use balances", () => {
    const channel = { kind: "subscription_adapter", subscription: { daily_request_limit: 0 } };
    const health = { quota_windows: [
      { source: "codex_header", window: "primary", used_percent: 100, remaining_ratio: 0 },
      { source: "codex_header", window: "secondary", remaining_ratio: 0.99999 },
      { source: "codex_header", window: "secondary", used_percent: 0, remaining_ratio: 1, checked_at_unix: 100, reset_at_unix: 200 },
      { source: "custom_quota", window: "secondary", used_percent: 0, remaining_ratio: 1 },
      { source: "antigravity_retrieve_user_quota", window: "gemini-flash", model: "gemini-flash", used_percent: 0, remaining_ratio: 1 },
      { source: "grok_header", window: "requests", token_type: "requests", used_percent: 0, remaining_ratio: 1 },
      { source: "grok_billing", window: "monthly", used_percent: 0, remaining_ratio: 1 },
    ] };
    const before = structuredClone(health);
    const { container } = render(<QuotaVisual channel={channel} health={health} formatCountdown={() => "1h"} />);
    expect(container.querySelector(".quota-visual > .quota-window-list")).toBeNull();
    expect(container.querySelectorAll(".quota-extra .quota-window-row")).toHaveLength(health.quota_windows.length);
    expect(health).toEqual(before);
  });

  it("does not turn auxiliary-only data or very low usage into a full main balance", () => {
    const channel = { kind: "subscription_adapter", subscription: { daily_request_limit: 0 } };
    expect(quotaDetailLabel({ remaining_ratio: 1, quota_status: "unknown", quota_windows: [
      { source: "anthropic_oauth_usage", window: "extra_usage", remaining_ratio: 1 },
    ] }, channel)).toBe("未知");
    render(<QuotaVisual channel={channel} health={{ quota_windows: [
      { source: "anthropic_oauth_usage", window: "5h", used_percent: 0.001, remaining_ratio: 0.99999 },
    ] }} formatCountdown={() => ""} />);
    expect(screen.getByText("已用 <0.01%")).toBeVisible();
    expect(screen.getByText(">99.99%")).toBeVisible();
    expect(screen.queryByText("100%")).not.toBeInTheDocument();
  });
  it("keeps stale quota visible and distinguishes it from newly updated windows", () => {
    const now = Math.floor(Date.now() / 1000);
    const { rerender } = render(
      <QuotaVisual
        channel={{ kind: "subscription_adapter", subscription: { daily_request_limit: 0 } }}
        health={{ quota_windows: [{
          source: "codex_rate_limits", window: "5h", remaining_ratio: 0.7, checked_at_unix: now - 1200,
        }] }}
        formatCountdown={() => ""}
      />,
    );
    expect(screen.getByText(/最近更新：/)).toBeInTheDocument();
    expect(screen.getByText(/部分额度超过 15 分钟未更新/)).toBeInTheDocument();
    expect(screen.getByText("OpenAI 官方额度")).toBeInTheDocument();
    expect(screen.getByText("70%")).toBeInTheDocument();
    rerender(
      <QuotaVisual
        channel={{ kind: "subscription_adapter", subscription: { daily_request_limit: 0 } }}
        health={{ quota_windows: [{
          source: "anthropic_oauth_usage", window: "5h", remaining_ratio: 0.6, checked_at_unix: now,
        }] }}
        formatCountdown={() => ""}
      />,
    );
    expect(screen.queryByText(/部分额度超过 15 分钟未更新/)).not.toBeInTheDocument();
    expect(screen.getByText("60%")).toBeInTheDocument();
  });

  it("shows a clear reset countdown without a redundant availability badge", () => {
    render(
      <QuotaVisual
        channel={{ kind: "subscription_adapter", subscription: { daily_request_limit: 0 } }}
        health={{
          quota_status: "available",
          quota_windows: [{
            source: "openai_wham_usage",
            window: "7d",
            token_type: "codex",
            remaining_ratio: 0.86,
            used_percent: 14,
            reset_at_unix: 123,
          }],
        }}
        formatCountdown={() => "139h"}
      />,
    );

    expect(screen.getByText("\u5df2\u7528 14% \u00b7 139 \u5c0f\u65f6\u540e\u91cd\u7f6e")).toBeInTheDocument();
    expect(screen.queryByText("\u53ef\u7528")).not.toBeInTheDocument();
  });

  it("shows model quota without a synthetic unknown summary or raw token type", () => {
    const health = {
      quota_status: "available",
      remaining_ratio: 0,
      quota_windows: [
        {
          source: "antigravity_retrieve_user_quota",
          window: "WTUS:gemini-3.6-flash",
          token_type: "WTUS",
          model: "gemini-3.6-flash",
          remaining_ratio: 0,
          used_percent: 100,
        },
        {
          source: "antigravity_retrieve_user_quota",
          window: "WTUS:claude-sonnet-4-6-thinking",
          token_type: "WTUS",
          model: "claude-sonnet-4-6-thinking",
          remaining_ratio: 1,
          used_percent: 0,
        },
      ],
    };
    const channel = {
      kind: "subscription_adapter",
      subscription: { daily_request_limit: 0 },
    };

    render(
      <QuotaVisual
        channel={channel}
        health={health}
        formatCountdown={() => "1h"}
      />,
    );

    expect(screen.getByText("模型额度（2）")).toBeInTheDocument();
    expect(screen.getByText("gemini-3.6-flash")).toBeInTheDocument();
    expect(screen.getByText("claude-sonnet-4-6-thinking")).toBeInTheDocument();
    expect(screen.queryByText("汇总额度")).not.toBeInTheDocument();
    expect(screen.queryByText("WTUS")).not.toBeInTheDocument();
    expect(quotaDetailLabel(health, channel)).toBe("模型额度 1/2 可用");
  });
});
