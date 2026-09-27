import { describe, expect, test } from "vitest";

import { changeAppLanguage } from "./i18n";
import {
  callLogRefreshErrorText,
  callLogStatsForRefresh,
  callLogCursorForRefresh,
  callSyncState,
  callSyncIsIncremental,
  callRecordCacheRate,
  callRecordOutputUsage,
  callStatsTotals,
  formatCacheRate,
  formatCallMoney,
  formatCallTime,
} from "./callLogs";

describe("call log cursor compatibility", () => {
  test("full resync resets the legacy timestamp while incremental polls retain it", () => {
    expect(callLogCursorForRefresh(2000, 1000, false)).toBe(1000);
    expect(callLogCursorForRefresh(2000, 0, false)).toBe(0);
    expect(callLogCursorForRefresh(2000, 1000, true)).toBe(2000);
    expect(callLogCursorForRefresh(2000, 3000, true)).toBe(3000);
  });
  test("old servers retain after_ms polling without inventing history metadata", () => {
    expect(callSyncIsIncremental({ data: [] }, true)).toBe(true);
    expect(callSyncState({ cursor_ms: 123 })).toEqual({ cursor: undefined, history: undefined, hasMore: undefined });
  });

  test("retention gaps replace the recent page, normal polls merge it", () => {
    expect(callSyncIsIncremental({ incremental: true }, true)).toBe(true);
    expect(callSyncIsIncremental({ incremental: false }, true)).toBe(false);
    expect(callSyncIsIncremental({ incremental: true, resync_required: true }, true)).toBe(false);
    expect(callSyncIsIncremental({ incremental: true }, false)).toBe(false);
    expect(callSyncState({ cursor: "opaque", history: { older_records_removed: true }, has_more: false }))
      .toEqual({ cursor: "opaque", history: { older_records_removed: true }, hasMore: false });
  });
});

describe("callLogStatsForRefresh", () => {
  test("a server summary without observed cache input stays unknown, not zero percent", () => {
    const totals = callStatsTotals([{ scope: "retained_history", requests: 4, input_tokens: 1000,
      cache_read_tokens: 0, cache_base_input_tokens: 0 }], []);
    expect(totals.requests).toBe(4);
    expect(totals.cacheRate).toBeUndefined();
  });

  test("incremental pages never replace the retained summary with recent records", () => {
    const current = [{ requests: 9000, input_tokens: 1000, cache_read_tokens: 700, cache_base_input_tokens: 1000 }];
    const selected = callLogStatsForRefresh(current, [], true);
    expect(selected).toBe(current);
    expect(callStatsTotals(selected, [{ status: "success", input_tokens: 2 }])).toMatchObject({ requests: 9000, cacheRate: 0.7 });
  });

  test("full refresh replaces old and empty summaries without stale accumulation", () => {
    const current = [{ requests: 9000 }];
    const updated = [{ requests: 9001 }];
    expect(callLogStatsForRefresh(current, updated, false)).toBe(updated);
    expect(callLogStatsForRefresh(current, [], false)).toEqual([]);
  });
});

describe("callRecordOutputUsage", () => {
  test("keeps inclusive OpenAI and Claude reasoning inside the output total", () => {
    for (const included of [{ included_in_total: true }, { metadata: { included_in_total: true } }]) {
      expect(callRecordOutputUsage({ output_tokens: 103, usage_meters: [
        { meter_kind: "reasoning_tokens", quantity: 94, ...included },
      ] })).toEqual({ outputTokens: 103, reasoningTokens: 94 });
    }
  });

  test("adds separate Gemini reasoning once for local and platform records", () => {
    for (const included of [{ included_in_total: false }, { metadata: { included_in_total: false } }]) {
      expect(callRecordOutputUsage({ output_tokens: 9, usage_meters: [
        { meter_kind: "reasoning_tokens", quantity: 94, ...included },
      ] })).toEqual({ outputTokens: 103, reasoningTokens: 94 });
    }
  });

  test("keeps missing old-client data distinct from a reported zero", () => {
    expect(callRecordOutputUsage({ output_tokens: 103 })).toEqual({ outputTokens: 103, reasoningTokens: undefined });
    expect(callRecordOutputUsage({ output_tokens: 103, usage_meters: [
      { meter_kind: "reasoning_tokens", quantity: 0, included_in_total: true },
    ] })).toEqual({ outputTokens: 103, reasoningTokens: 0 });
    expect(callRecordOutputUsage({ output_tokens: 9, usage_meters: [
      { meter_kind: "reasoning_tokens", quantity: 94, included_in_total: true },
    ] })).toEqual({ outputTokens: 9, reasoningTokens: undefined });
  });
});

function expectedLocalTime(value: string) {
  const date = new Date(value);
  const twoDigits = (part: number) => String(part).padStart(2, "0");
  return `${date.getFullYear()}-${twoDigits(date.getMonth() + 1)}-${twoDigits(date.getDate())} ${twoDigits(date.getHours())}:${twoDigits(date.getMinutes())}:${twoDigits(date.getSeconds())}`;
}

describe("formatCallTime", () => {
  test("converts an RFC3339 UTC timestamp to compact browser-local time", () => {
    const timestamp = "2026-07-18T19:07:52Z";
    const formatted = formatCallTime({ created_at: timestamp });

    expect(formatted).toBe(expectedLocalTime(timestamp));
    expect(formatted).not.toContain("T");
    expect(formatted).not.toContain("Z");
  });

  test("formats legacy Unix timestamps with the same local-time format", () => {
    const timestamp = "2026-07-18T19:07:52Z";
    expect(formatCallTime({ ts: Date.parse(timestamp) / 1000 })).toBe(expectedLocalTime(timestamp));
  });

  test("keeps invalid timestamps visible for diagnosis", () => {
    expect(formatCallTime({ created_at: "not-a-date" })).toBe("not-a-date");
    expect(formatCallTime({})).toBe("-");
  });
});

describe("formatCallMoney", () => {
  test("shows CNY for Chinese and USD for English without a parallel amount", async () => {
    await changeAppLanguage("zh-CN");
    expect(formatCallMoney(1, 7.2)).toBe("\u00a57.20");

    await changeAppLanguage("en-US");
    expect(formatCallMoney(1, 7.2)).toBe("$1.00");
    await changeAppLanguage("zh-CN");
  });
});

describe("callLogRefreshErrorText", () => {
  test("distinguishes signed-out platform logs from an empty result", () => {
    expect(callLogRefreshErrorText("platform", undefined, "signed_out"))
      .toBe("未登录，平台日志不可用");
  });

  test("uses a concise availability message for platform transport failures", () => {
    expect(callLogRefreshErrorText("platform", new Error("The operation has timed out"), "signed_in"))
      .toBe("平台暂时不可达");
  });

  test("does not misreport a platform device-key rejection as an expired account session", () => {
    expect(callLogRefreshErrorText("platform", new Error("local proxy returned HTTP 401"), "signed_in"))
      .toBe("平台设备凭证不可用，请重新登录");
  });

  test("keeps local supplier read failures source-specific", () => {
    expect(callLogRefreshErrorText("local_supplier", new Error("database is locked"), "signed_in"))
      .toBe("本机日志读取失败");
  });
});

describe("callStatsTotals", () => {
  test("uses provider cache totals from complete platform statistics", () => {
    const totals = callStatsTotals([{
      requests: 2,
      successes: 2,
      input_tokens: 100,
      output_tokens: 10,
      cache_read_tokens: 60,
      cache_write_tokens: 5,
      cache_base_input_tokens: 100,
    }], [{
      input_tokens: 999,
      cache_read_tokens: 999,
      cache_base_input_tokens: 999,
    }]);

    expect(totals.cacheReadTokens).toBe(60);
    expect(totals.cacheWriteTokens).toBe(5);
    expect(totals.cacheBaseInputTokens).toBe(100);
    expect(totals.cacheRate).toBe(0.6);
  });

  test("normalizes Anthropic cache writes when only 5-minute and 1-hour counters exist", () => {
    const totals = callStatsTotals([], [{
      status: "success",
      input_tokens: 10,
      output_tokens: 2,
      cache_read_tokens: 7,
      cache_write_5m_tokens: 5,
      cache_write_1h_tokens: 3,
      upstream_protocol: "anthropic_messages",
    }]);

    expect(totals.cacheReadTokens).toBe(7);
    expect(totals.cacheWriteTokens).toBe(8);
    expect(totals.cacheBaseInputTokens).toBe(25);
    expect(totals.cacheRate).toBe(7 / 25);
  });

  test("does not add cache-read tokens twice for inclusive OpenAI usage", () => {
    const totals = callStatsTotals([], [{
      status: "success",
      input_tokens: 100,
      output_tokens: 4,
      cache_read_tokens: 60,
      upstream_protocol: "openai_responses",
    }]);

    expect(totals.cacheBaseInputTokens).toBe(100);
    expect(totals.cacheRate).toBe(0.6);
  });

  test("excludes failed and unobserved records from the cache-rate denominator", () => {
    const totals = callStatsTotals([], [{
      status: "success",
      input_tokens: 100,
      usage_source: "upstream",
    }, {
      status: "failed",
      input_tokens: 900,
      cache_read_tokens: 900,
      usage_source: "upstream",
    }, {
      status: "success",
      input_tokens: 800,
      usage_source: "estimated",
    }]);

    expect(totals.cacheReadTokens).toBe(0);
    expect(totals.cacheBaseInputTokens).toBe(100);
    expect(totals.cacheRate).toBe(0);
  });

  test("excludes failed statistic buckets even when an older producer populated a cache base", () => {
    const totals = callStatsTotals([{
      status: "success",
      input_tokens: 100,
      cache_read_tokens: 60,
      cache_base_input_tokens: 100,
    }, {
      status: "failed",
      input_tokens: 900,
      cache_read_tokens: 900,
      cache_base_input_tokens: 900,
    }], []);

    expect(totals.cacheReadTokens).toBe(60);
    expect(totals.cacheBaseInputTokens).toBe(100);
    expect(totals.cacheRate).toBe(0.6);
  });
});

describe("formatCacheRate", () => {
  test("formats a token-weighted ratio and keeps unavailable data distinct from zero", () => {
    expect(formatCacheRate(0.125)).toBe("12.5%");
    expect(formatCacheRate(0)).toBe("0.0%");
    expect(formatCacheRate(undefined)).toBe("-");
  });

  test("distinguishes an authoritative zero cache result from unavailable metrics", () => {
    expect(callRecordCacheRate({
      status: "success",
      input_tokens: 100,
      usage_source: "upstream",
    })).toBe(0);
    expect(callRecordCacheRate({
      status: "success",
      input_tokens: 100,
      usage_source: "estimated",
    })).toBeUndefined();
    expect(callRecordCacheRate({
      status: "success",
      input_tokens: 100,
      usage_source: "upstream_usage",
    })).toBe(0);
  });
});
