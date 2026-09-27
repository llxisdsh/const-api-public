type QuotaWindowLike = {
  source?: string;
  window?: string;
  remaining_ratio?: number;
  used_percent?: number;
  reset_at_unix?: number;
  checked_at_unix?: number;
  model?: string;
  token_type?: string;
};

type SupplierChannelHealthLike = {
  quota_status?: string;
  remaining_ratio?: number;
  daily_used?: number;
  daily_limit?: number;
  quota_checked_at_unix?: number;
  quota_windows?: QuotaWindowLike[];
};

type ChannelLike = {
  kind: string;
  discovery?: { quota: boolean };
  subscription: {
    daily_request_limit: number;
  };
};

type QuotaVisualItem = {
  key: string;
  label: string;
  detail: string;
  remainingRatio: number;
  source: string;
  sortGroup: number;
  sortSeconds: number;
  sortIndex: number;
  modelScoped: boolean;
};

type QuotaVisualProps = {
  health: SupplierChannelHealthLike | undefined;
  channel: ChannelLike;
  formatCountdown: (unixSeconds: number) => string;
};

export function quotaDetailLabel(
  health: SupplierChannelHealthLike | undefined,
  channel: ChannelLike,
) {
  if (channel.kind !== "subscription_adapter" && !channel.discovery?.quota) return "API Key";
  if (health) {
    const modelSummary = quotaModelSummary(health);
    if (modelSummary) {
      return tr("quota.modelAvailable", modelSummary);
    }
    const officialWindows = quotaOfficialWindowItems(health);
    if (officialWindows.length > 0) {
      const tightest = tightestQuotaItem(officialWindows);
      const count = officialWindows.length > 1 ? tr("quota.windowCountSuffix", { count: officialWindows.length }) : "";
      return tightest ? `${tightest.label} ${formatQuotaRatio(tightest.remainingRatio)}${count}` : tr("common.unknown");
    }
    if (health.quota_windows?.length) return tr("common.unknown");
  }
  if (!health) return tr("quota.waitingDetection");
  const used = health.daily_used ?? 0;
  const limit = health.daily_limit ?? channel.subscription.daily_request_limit;
  const ratio = health.remaining_ratio;
  const ratioText = typeof ratio === "number" && Number.isFinite(ratio) ? formatQuotaRatio(ratio) : "";
  if (limit > 0) {
    return ratioText ? tr("quota.remainingWithUsage", { ratio: ratioText, used, limit }) : `${used}/${limit}`;
  }
  if (health.quota_status === "unknown" || health.quota_status === "not_applicable") return tr("common.unknown");
  return ratioText ? tr("quota.remaining", { ratio: ratioText }) : tr("common.unknown");
}

export function QuotaVisual({ health, channel, formatCountdown }: QuotaVisualProps) {
  const { t, i18n } = useTranslation();
  if (channel.kind !== "subscription_adapter" && !channel.discovery?.quota) return null;
  const items = quotaVisualItems(health, channel, formatCountdown);
  const extraItems = quotaExtraWindowItems(health, formatCountdown);
  const hasQuotaItems = items.length > 0 || extraItems.length > 0;
  const checkedTimes = (health?.quota_windows?.length
    ? health.quota_windows.filter((window) => !isEmptyOpenAIQuotaWindow(window)).map((window) => window.checked_at_unix)
    : [health?.quota_checked_at_unix])
    .filter((value): value is number => typeof value === "number" && Number.isFinite(value) && value > 0 && value < 8.64e12);
  const lastChecked = checkedTimes.length ? Math.max(...checkedTimes) : undefined;
  const hasStaleQuota = checkedTimes.some((time) => Date.now() / 1000 - time >= 15 * 60);
  return (
    <div className="drawer-section quota-visual">
      <div className="section-title-row compact">
        <div>
          <h2>{t("quota.title")}</h2>
          <p className="section-hint">{lastChecked
            ? t("quota.updatedAt", { time: new Date(lastChecked * 1000).toLocaleString(i18n.resolvedLanguage) })
            : t("quota.hint")}</p>
          {hasStaleQuota && <p className="section-hint">{t("quota.staleHint")}</p>}
        </div>
      </div>

      {!health ? (
        <p className="quota-empty">{t("quota.waitingHint")}</p>
      ) : !hasQuotaItems ? (
        <p className="quota-empty">{t("quota.empty")}</p>
      ) : (
        <>
          {items.length > 0 && (
            <div className="quota-window-list">
              {quotaWindowRows(items)}
            </div>
          )}
          {extraItems.length > 0 && (
            <details className="quota-extra">
              <summary>
                {extraItems.every((item) => item.modelScoped)
                  ? t("quota.modelBuckets", { count: extraItems.length })
                  : t("quota.otherBuckets", { count: extraItems.length })}
              </summary>
              <div className="quota-window-list compact">
                {quotaWindowRows(extraItems)}
              </div>
            </details>
          )}
        </>
      )}
    </div>
  );
}

function quotaWindowRows(items: QuotaVisualItem[]) {
  return items.map((item) => {
    const percent = item.remainingRatio * 100;
    return (
      <div key={item.key} className="quota-window-row">
        <div className="quota-window-head">
          <strong title={item.label}>{item.label}</strong>
          <span title={item.source}>{item.source}</span>
        </div>
        <div className="quota-window-meter" aria-label={tr("quota.remainingAria", { label: item.label, percent })}>
          <span style={{ width: `${Math.max(0, Math.min(100, percent))}%` }} />
        </div>
        <div className="quota-window-meta">
          <strong title={formatQuotaRatio(item.remainingRatio)}>{formatQuotaRatio(item.remainingRatio)}</strong>
          <span title={item.detail}>{item.detail}</span>
        </div>
      </div>
    );
  });
}

function quotaOfficialWindowItems(
  health: SupplierChannelHealthLike | undefined,
  formatCountdown?: (unixSeconds: number) => string,
): QuotaVisualItem[] {
  return quotaWindowItems(health, formatCountdown, true);
}

function quotaExtraWindowItems(
  health: SupplierChannelHealthLike | undefined,
  formatCountdown?: (unixSeconds: number) => string,
): QuotaVisualItem[] {
  return quotaWindowItems(health, formatCountdown, false);
}

function quotaWindowItems(
  health: SupplierChannelHealthLike | undefined,
  formatCountdown: ((unixSeconds: number) => string) | undefined,
  primary: boolean,
): QuotaVisualItem[] {
  const items = (health?.quota_windows ?? [])
    .filter((window) => typeof window.remaining_ratio === "number" && Number.isFinite(window.remaining_ratio))
    .filter((window) => !isEmptyOpenAIQuotaWindow(window))
    .filter((window) => isPrimaryQuotaWindowBucket(window) === primary)
    .map((window, index) => {
      const remainingRatio = (window.used_percent ?? 0) >= 100 ? 0 : clampRatio(window.remaining_ratio ?? 0);
      const usedPercent = typeof window.used_percent === "number" && Number.isFinite(window.used_percent)
        ? Math.max(0, Math.min(100, window.used_percent))
        : (1 - remainingRatio) * 100;
      const reset = window.reset_at_unix && formatCountdown
        ? tr("quota.resetSuffix", { duration: readableCountdown(formatCountdown(window.reset_at_unix)) })
        : "";
      const sortSeconds = quotaWindowPeriodSeconds(window.window);
      const bucket = quotaBucketLabel(window);
      const model = (window.model || "").trim();
      return {
        key: `official-${window.source || "unknown"}-${window.window || index}-${index}`,
        label: model
          ? model
          : bucket
            ? `${quotaWindowLabel(window.window)} \u00b7 ${bucket}`
            : quotaWindowLabel(window.window),
        detail: tr("quota.used", { percent: formatQuotaUsedPercent(usedPercent), reset }),
        remainingRatio,
        source: quotaSourceLabel(window.source),
        sortGroup: primary ? 1 : (Number.isFinite(sortSeconds) ? 0 : 1),
        sortSeconds,
        sortIndex: index,
        modelScoped: model.length > 0,
      };
    });
  return sortQuotaItems(collapseModelQuotaItems(items));
}

function isPrimaryQuotaWindowBucket(window: QuotaWindowLike) {
  // Main rows need a declared period, not a primary/secondary slot or a plan
  // name. Unknown-period limits remain in extras; supply gating is unchanged.
  const period = quotaWindowNormalizedName(window.window);
  if (window.source === "grok_billing" && period === "monthly") return false;
  return (Number.isFinite(quotaWindowPeriodSeconds(window.window))
    || period === "monthly") && isStandardQuotaBucket(window);
}

function isEmptyOpenAIQuotaWindow(window: QuotaWindowLike) {
  if (!["openai_wham_usage", "codex_header", "codex_rate_limits"].includes(window.source || "")) return false;
  if (!["primary", "secondary"].includes(quotaWindowNormalizedName(window.window))) return false;
  if (window.remaining_ratio !== 1 || (window.used_percent !== undefined && window.used_percent !== 0)) return false;
  const resetAt = window.reset_at_unix ?? 0;
  const checkedAt = window.checked_at_unix ?? 0;
  // reset_after_seconds=0 may be normalized to the observation time. It does
  // not establish a reset schedule. Keep any actual usage or future reset.
  const hasReset = Number.isFinite(resetAt) && resetAt > 0 && (!(checkedAt > 0) || resetAt > checkedAt);
  return !hasReset;
}

function isStandardQuotaBucket(window: QuotaWindowLike) {
  if ((window.model || "").trim()) return false;
  const name = window.window || "";
  const prefix = name.includes(":") ? name.slice(0, name.lastIndexOf(":")) : "";
  return [window.token_type || "", prefix].every((bucket) =>
    ["", "codex", "default", "global", "all"].includes(bucket.trim().toLowerCase()));
}

function quotaBucketLabel(window: QuotaWindowLike) {
  return (window.token_type || window.model || "").trim();
}

function sortQuotaItems(items: QuotaVisualItem[]) {
  return [...items].sort((a, b) => {
    if (a.sortGroup !== b.sortGroup) return a.sortGroup - b.sortGroup;
    if (a.sortSeconds !== b.sortSeconds) return a.sortSeconds - b.sortSeconds;
    return a.sortIndex - b.sortIndex;
  });
}

function quotaWindowPeriodSeconds(window?: string) {
  const normalized = quotaWindowNormalizedName(window);
  if (normalized === "1h") return 60 * 60;
  if (normalized === "5h") return 5 * 60 * 60;
  if (normalized === "daily" || normalized === "24h") return 24 * 60 * 60;
  if (normalized === "weekly" || normalized === "7d") return 7 * 24 * 60 * 60;

  if (normalized === "hourly") return 3600;
  const match = normalized.match(/^(\d+)(s|m|h|d|w)$/);
  if (!match) return Number.POSITIVE_INFINITY;
  const amount = Number(match[1]);
  if (!Number.isFinite(amount) || amount <= 0) return Number.POSITIVE_INFINITY;
  const unit = match[2];
  if (unit === "s") return amount;
  if (unit === "m") return amount * 60;
  if (unit === "h") return amount * 3600;
  if (unit === "d") return amount * 86400;
  if (unit === "w") return amount * 604800;
  return Number.POSITIVE_INFINITY;
}

function quotaVisualItems(
  health: SupplierChannelHealthLike | undefined,
  channel: ChannelLike,
  formatCountdown: (unixSeconds: number) => string,
): QuotaVisualItem[] {
  if (!health) return [];
  const items = quotaOfficialWindowItems(health, formatCountdown);
  const used = health.daily_used ?? 0;
  const limit = health.daily_limit ?? channel.subscription.daily_request_limit;
  if (limit > 0) {
    const remaining = Math.max(0, limit - used);
    items.push({
      key: "local-daily",
      label: tr("quota.daily"),
      detail: `${used}/${limit}`,
      remainingRatio: clampRatio(remaining / limit),
      source: tr("quota.local"),
      sortGroup: 0,
      sortSeconds: 24 * 60 * 60,
      sortIndex: -1,
      modelScoped: false,
    });
  } else if (
    items.length === 0
    && (health.quota_windows ?? []).length === 0
    && typeof health.remaining_ratio === "number"
    && Number.isFinite(health.remaining_ratio)
  ) {
    items.push({
      key: "current",
      label: tr("quota.summary"),
      detail: tr("quota.periodUnknown"),
      remainingRatio: clampRatio(health.remaining_ratio),
      source: tr("quota.upstreamSummary"),
      sortGroup: 2,
      sortSeconds: Number.POSITIVE_INFINITY,
      sortIndex: 0,
      modelScoped: false,
    });
  }
  return sortQuotaItems(items);
}

function collapseModelQuotaItems(items: QuotaVisualItem[]) {
  const output: QuotaVisualItem[] = [];
  const modelIndexes = new Map<string, number>();
  for (const item of items) {
    if (!item.modelScoped) {
      output.push(item);
      continue;
    }
    const key = item.label.trim().toLowerCase();
    const existingIndex = modelIndexes.get(key);
    if (existingIndex === undefined) {
      modelIndexes.set(key, output.length);
      output.push(item);
      continue;
    }
    if (item.remainingRatio < output[existingIndex].remainingRatio) {
      output[existingIndex] = item;
    }
  }
  return output;
}

function quotaModelSummary(health: SupplierChannelHealthLike | undefined) {
  const models = new Map<string, number>();
  for (const window of health?.quota_windows ?? []) {
    if (!isAntigravityQuotaWindow(window)) continue;
    const model = (window.model || "").trim().toLowerCase();
    const ratio = window.remaining_ratio;
    if (!model || typeof ratio !== "number" || !Number.isFinite(ratio)) continue;
    models.set(model, Math.min(models.get(model) ?? 1, clampRatio(ratio)));
  }
  if (models.size === 0) return undefined;
  return {
    total: models.size,
    available: [...models.values()].filter((ratio) => ratio > 0.050000001).length,
  };
}

function isAntigravityQuotaWindow(window: QuotaWindowLike) {
  return window.source === "antigravity_retrieve_user_quota" || window.source === "antigravity_fetch_available_models";
}

function quotaSourceLabel(source?: string) {
  switch ((source || "").trim()) {
    case "antigravity_retrieve_user_quota":
      return tr("quota.sources.antigravityLive");
    case "antigravity_fetch_available_models":
      return tr("quota.sources.antigravityCatalog");
    case "openai_wham_usage":
    case "codex_header":
    case "codex_rate_limits":
      return tr("quota.sources.openai");
    case "anthropic_oauth_usage":
    case "anthropic_header":
      return tr("quota.sources.claude");
    case "grok_billing":
    case "grok_header":
      return tr("quota.sources.grok");
    case "gemini_retrieve_user_quota":
      return tr("quota.sources.gemini");
    case "official_header":
    case "kimi_coding_usage":
    case "glm_coding_usage":
    case "minimax_token_usage":
      return tr("quota.sources.official");
    case "":
      return tr("quota.sources.upstream");
    default:
      return source || tr("quota.sources.upstream");
  }
}

function tightestQuotaItem(items: QuotaVisualItem[]) {
  return items.reduce<QuotaVisualItem | undefined>((selected, item) => {
    if (!selected || item.remainingRatio < selected.remainingRatio) return item;
    return selected;
  }, undefined);
}

function clampRatio(value: number) {
  return Math.max(0, Math.min(1, value));
}

function formatQuotaRatio(value: number) {
  return formatQuotaUsedPercent(clampRatio(value) * 100);
}

function formatQuotaUsedPercent(value: number) {
  if (value > 0 && value < 0.01) return "<0.01%";
  if (value > 99.99 && value < 100) return ">99.99%";
  return `${Number(value.toFixed(value < 1 || value > 99 ? 2 : 1))}%`;
}

function quotaWindowLabel(window?: string) {
  const value = (window || "").trim();
  const normalized = quotaWindowNormalizedName(value);
  const seconds = quotaWindowPeriodSeconds(value);
  if (!value) return tr("quota.official");
  if (seconds === 604800) return tr("quota.sevenDays");
  if (seconds === 86400) return tr("quota.daily");
  if (seconds === 18000) return tr("quota.fiveHours");
  if (seconds === 3600) return tr("quota.oneHour");
  if (normalized === "primary") return tr("quota.primaryWindow");
  if (normalized === "secondary") return tr("quota.secondaryWindow");
  return value;
}

function quotaWindowNormalizedName(window?: string) {
  const value = (window || "").trim().toLowerCase();
  return value.includes(":") ? value.split(":").pop() || value : value;
}

function readableCountdown(value: string) {
  return value
    .replace(/^(\d+)h$/, (_, amount) => tr("quota.duration.hours", { amount }))
    .replace(/^(\d+)d$/, (_, amount) => tr("quota.duration.days", { amount }))
    .replace(/^(\d+)m$/, (_, amount) => tr("quota.duration.minutes", { amount }));
}
import { useTranslation } from "react-i18next";

import { tr } from "./i18n";
