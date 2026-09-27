import { useMemo } from "react";
import { useTranslation } from "react-i18next";

import { DialogBackdrop } from "./DialogBackdrop";
import { accountDisplayAmountFromUSD, accountDisplayCurrency } from "./account/accountFormat";
import { currentAppLanguage, tr } from "./i18n";
import { ModalCloseButton } from "./ModalCloseButton";
import { useEscapeDismiss } from "./useEscapeDismiss";

export type CallRecord = Record<string, any>;
export type UsageStat = Record<string, any>;
export type CallLogSource = "platform" | "local_supplier";
export type CallLogAccountState = "signed_out" | "refreshing" | "signed_in" | "expired";

export type CallHistoryInfo = { older_records_removed?: boolean };

export type CallSyncState = { cursor?: string; history?: CallHistoryInfo; hasMore?: boolean };

export function callSyncState(result: Record<string, any>): CallSyncState {
  return {
    cursor: typeof result.cursor === "string" ? result.cursor : undefined,
    history: result.history && typeof result.history.older_records_removed === "boolean" ? result.history : undefined,
    hasMore: typeof result.has_more === "boolean" ? result.has_more : undefined,
  };
}

// Old servers omit incremental, and still understand the requested after_ms.
// A new server can explicitly replace the page after a retention gap/restart.
export function callSyncIsIncremental(result: Record<string, any>, requested: boolean) {
  return requested && result.resync_required !== true && result.incremental !== false;
}

export type ActiveCallLog = {
  source: CallLogSource;
  title: string;
  subtitle: string;
  records: CallRecord[];
  stats: UsageStat[];
  cursorMs: number;
  refreshedAt: number;
  refreshError?: string;
  history?: CallHistoryInfo;
  hasMore?: boolean;
};

// A recent records page cannot replace an aggregate over retained history.
// Full refreshes replace it (including a valid empty summary); polls keep it.
export function callLogStatsForRefresh(current: UsageStat[], incoming: UsageStat[], incremental: boolean) {
  return incremental ? current : incoming;
}

// A full snapshot can legitimately move backwards after server restoration or
// clock correction. Only incremental legacy polling keeps a high-water mark.
export function callLogCursorForRefresh(current: number, incoming: number, incremental: boolean) {
  return incremental ? Math.max(current, incoming) : incoming;
}

export function formatByteCount(value?: number) {
  if (value === undefined || Number.isNaN(value)) return "-";
  if (value < 1024) return `${value} B`;
  return `${(value / 1024).toFixed(value < 1024 * 100 ? 1 : 0)} KiB`;
}

export function CallLogModal({
  log,
  selected,
  cnyPerUSD,
  onSelect,
  onClose,
}: {
  log: ActiveCallLog;
  selected: CallRecord | null;
  cnyPerUSD?: number;
  onSelect: (record: CallRecord) => void;
  onClose: () => void;
}) {
  useEscapeDismiss(onClose);
  const { t } = useTranslation();
  const totals = useMemo(
    () => callStatsTotals(log.stats, log.records),
    [log.records, log.stats],
  );
  return (
    <DialogBackdrop aria-label={log.title} className="call-log-backdrop">
      <div className="modal-panel call-log-panel">
        <div className="call-log-header">
          <div>
            <h2>{log.title}</h2>
            <p>
              {log.subtitle} · {log.refreshError ? (
                <span className="call-log-refresh-error">{t("callLogs.autoRefreshFailed")} · {log.refreshError}</span>
              ) : (
                <>{t("callLogs.autoRefresh")} · {formatCallLogRefreshTime(log.refreshedAt)}</>
              )}
            </p>
            {log.source === "platform" && log.stats.some((stat) => stat.scope === "retained_history") && (
              <p>{t("callLogs.retainedSummaryHint")}</p>
            )}
          </div>
          <ModalCloseButton onClick={onClose} />
        </div>
        <div className="call-summary-grid modal-summary">
          <div><span>{t("callLogs.summary.requests")}</span><strong>{totals.requests}</strong></div>
          <div><span>{t("callLogs.summary.successes")}</span><strong>{totals.successes}</strong></div>
          <div><span>{t("callLogs.summary.failures")}</span><strong>{totals.failures}</strong></div>
          <div><span>{t("callLogs.summary.inboundBody")}</span><strong>{formatByteCount(totals.inboundRequestBytes)}</strong></div>
          <div><span>{t("callLogs.summary.upstreamBody")}</span><strong>{formatByteCount(totals.upstreamRequestBytes)}</strong></div>
          <div><span>{t("callLogs.summary.upstreamWire")}</span><strong>{formatByteCount(totals.responseWireBytes)}</strong></div>
          <div><span>{t("callLogs.summary.responseBody")}</span><strong>{formatByteCount(totals.responseBodyBytes)}</strong></div>
          <div><span>{t("callLogs.summary.tokens")}</span><strong>{formatTokenCount(totals.tokens)}</strong></div>
          <div><span>{t("callLogs.summary.cacheRead")}</span><strong>{formatTokenCount(totals.cacheReadTokens)}</strong></div>
          <div title={t("callLogs.summary.cacheRateHint")}>
            <span>{t("callLogs.summary.cacheRate")}</span>
            <strong>{formatCacheRate(totals.cacheRate)}</strong>
          </div>
        </div>
        <div className="call-log-body">
          <div className="call-record-list full">
            {log.records.length === 0 ? (
              <div className="empty-log">{log.refreshError ? t("callLogs.unavailable") : t("callLogs.empty")}</div>
            ) : log.records.map((record) => (
              <button
                key={callRecordID(record)}
                type="button"
                className={`call-record-row ${selected && callRecordID(selected) === callRecordID(record) ? "active" : ""}`}
                onClick={() => onSelect(record)}
              >
                <span className={`call-status-dot ${callRecordOK(record) ? "ok" : "bad"}`} />
                <div className="call-row-main">
                  <strong>{callRecordModel(record)}</strong>
                  <small>{formatCallTime(record)} · {callRecordProtocol(record)}</small>
                </div>
                <div className="call-row-meta">
                  <span>{callRecordStatus(record)}</span>
                  <small>{formatDurationMS(callRecordDuration(record))}</small>
                </div>
              </button>
            ))}
            {(log.history?.older_records_removed || log.hasMore) && (
              <div className="empty-log">{t(log.hasMore ? "callLogs.recentRecordsHint" : "callLogs.historyRemovedHint")}</div>
            )}
          </div>
          <div className="call-detail">
            {selected ? (
              <>
                <div className="call-detail-title">
                  <h3>{callRecordModel(selected)}</h3>
                  <span className={`status-pill ${callRecordOK(selected) ? "running" : "error"}`}>{callRecordStatus(selected)}</span>
                </div>
                {selected.details_compacted === true && <p className="subtle">{t("callLogs.detailsCompactedHint")}</p>}
                <div className="call-detail-grid">
                  {callDetailFields(selected, cnyPerUSD).map(([label, value]) => (
                    <div key={label}>
                      <span>{label}</span>
                      <strong>{value || "-"}</strong>
                    </div>
                  ))}
                </div>
                {callRecordError(selected) && (
                  <div className="call-error-box">
                    <span>{t("callLogs.errorSummary")}</span>
                    <pre>{callRecordError(selected)}</pre>
                  </div>
                )}
              </>
            ) : (
              <div className="empty-log">{t("callLogs.selectRecord")}</div>
            )}
          </div>
        </div>
      </div>
    </DialogBackdrop>
  );
}

export function callStatsTotals(stats: UsageStat[], records: CallRecord[]) {
  if (stats.length > 0) {
    return finishCallTotals(stats.reduce<ReturnType<typeof emptyCallTotals>>((acc, stat) => {
      acc.requests += Number(stat.requests ?? 0);
      acc.successes += Number(stat.successes ?? 0);
      acc.failures += Number(stat.failures ?? 0);
      acc.inboundRequestBytes += firstPositiveNumber(stat.inbound_request_bytes, stat.request_bytes);
      acc.upstreamRequestBytes += firstPositiveNumber(stat.upstream_request_bytes, stat.request_bytes);
      acc.responseWireBytes += firstPositiveNumber(stat.response_wire_bytes, stat.upstream_response_bytes, stat.response_bytes);
      acc.responseBodyBytes += firstPositiveNumber(stat.response_body_bytes, stat.returned_response_bytes, stat.response_bytes);
      acc.tokens += Number(stat.input_tokens ?? 0) + Number(stat.output_tokens ?? 0);
      if (usageStatCacheMetricObserved(stat)) {
        acc.cacheReadTokens += nonNegativeNumber(stat.cache_read_tokens);
        acc.cacheWriteTokens += cacheWriteTokenCount(stat);
        acc.cacheBaseInputTokens += cacheBaseInputTokenCount(stat);
      }
      return acc;
    }, emptyCallTotals()));
  }
  return finishCallTotals(records.reduce<ReturnType<typeof emptyCallTotals>>((acc, record) => {
    acc.requests += 1;
    if (callRecordOK(record)) acc.successes += 1;
    else acc.failures += 1;
    acc.inboundRequestBytes += callRecordInboundRequestBytes(record);
    acc.upstreamRequestBytes += callRecordUpstreamRequestBytes(record);
    acc.responseWireBytes += callRecordResponseWireBytes(record);
    acc.responseBodyBytes += callRecordResponseBodyBytes(record);
    acc.tokens += Number(record.input_tokens ?? 0) + Number(record.output_tokens ?? 0);
    if (cacheMetricObserved(record)) {
      acc.cacheReadTokens += nonNegativeNumber(record.cache_read_tokens);
      acc.cacheWriteTokens += cacheWriteTokenCount(record);
      acc.cacheBaseInputTokens += cacheBaseInputTokenCount(record);
    }
    return acc;
  }, emptyCallTotals()));
}

function emptyCallTotals() {
  return {
    requests: 0,
    successes: 0,
    failures: 0,
    inboundRequestBytes: 0,
    upstreamRequestBytes: 0,
    responseWireBytes: 0,
    responseBodyBytes: 0,
    tokens: 0,
    cacheReadTokens: 0,
    cacheWriteTokens: 0,
    cacheBaseInputTokens: 0,
  };
}

function finishCallTotals(totals: ReturnType<typeof emptyCallTotals>) {
  return {
    ...totals,
    cacheRate: totals.cacheBaseInputTokens > 0
      ? totals.cacheReadTokens / totals.cacheBaseInputTokens
      : undefined,
  };
}

function callDetailFields(record: CallRecord, cnyPerUSD?: number): Array<[string, string]> {
  const pricing = record.pricing_evidence ?? {};
  const outputUsage = callRecordOutputUsage(record);
  return [
    [tr("callLogs.fields.time"), formatCallTime(record)],
    [tr("callLogs.fields.source"), callRecordDirection(record)],
    [tr("callLogs.fields.route"), callRecordRoute(record)],
    [tr("callLogs.fields.path"), callRecordPath(record)],
    [tr("callLogs.fields.inboundProtocol"), callRecordInboundProtocol(record) || "-"],
    [tr("callLogs.fields.upstreamProtocol"), callRecordUpstreamProtocol(record) || "-"],
    [tr("callLogs.fields.requestModel"), callRecordModel(record)],
    [tr("callLogs.fields.upstreamModel"), String(record.upstream_model ?? "")],
    [tr("callLogs.fields.requestStream"), boolLabel(record.stream)],
    [tr("callLogs.fields.upstreamSse"), boolLabel(record.upstream_stream)],
    [tr("callLogs.fields.sseEvents"), optionalNumber(record.sse_event_count)],
    [tr("callLogs.fields.sseFinished"), boolLabel(record.sse_done)],
    [tr("callLogs.fields.sseLastEvent"), String(record.sse_last_event_type ?? "")],
    ["HTTP", String(record.http_status ?? record.status ?? "-")],
    [tr("callLogs.fields.responseType"), responseKindLabel(record.response_kind)],
    [tr("callLogs.fields.toolCalls"), String(record.tool_call_count ?? 0)],
    ["Finish", String(record.finish_reason ?? "")],
    [tr("callLogs.fields.inboundRaw"), formatByteCount(callRecordInboundRequestBytes(record))],
    [tr("callLogs.fields.upstreamActual"), formatByteCount(callRecordUpstreamRequestBytes(record))],
    [tr("callLogs.fields.upstreamWire"), formatByteCount(callRecordResponseWireBytes(record))],
    [tr("callLogs.fields.responseBody"), formatByteCount(callRecordResponseBodyBytes(record))],
    [tr("callLogs.fields.inputTokens"), String(record.input_tokens ?? 0)],
    [tr("callLogs.fields.outputTokens"), String(outputUsage.outputTokens)],
    [tr("callLogs.fields.reasoningTokens"), outputUsage.reasoningTokens === undefined
      ? tr("callLogs.fields.reasoningUnavailable") : formatTokenCount(outputUsage.reasoningTokens)],
    [
      tr("callLogs.fields.cacheReadTokens"),
      optionalTokenMetric(record.cache_read_tokens, cacheMetricObserved(record)),
    ],
    [tr("callLogs.fields.cacheWriteTokens"), optionalCacheWriteTokens(record)],
    [tr("callLogs.fields.cacheRate"), formatCacheRate(callRecordCacheRate(record))],
    [tr("callLogs.fields.duration"), formatDurationMS(callRecordDuration(record))],
    [tr("callLogs.fields.priceRatio"), formatPriceRatio(pricing.price_ratio)],
    [tr("callLogs.fields.baseAmount"), formatCallMoney(pricing.base_amount_usd, cnyPerUSD)],
    [tr("callLogs.fields.charge"), formatCallMoney(pricing.charged_usd ?? record.charged, cnyPerUSD)],
    [tr("callLogs.fields.earnings"), formatCallMoney(pricing.supplier_accrued_usd ?? record.supplier_earned, cnyPerUSD)],
    [tr("callLogs.fields.priceVersion"), String(pricing.price_version_id ?? "")],
    [tr("callLogs.fields.pricingRule"), String(pricing.pricing_rule_id ?? "")],
    ["Client", String(record.client_id ?? "")],
    ["Channel", String(record.channel_id ?? "")],
    ["Supplier Unit", String(record.supplier_unit_id ?? "")],
    [tr("callLogs.fields.errorLayer"), String(record.error_layer ?? "")],
    [tr("callLogs.fields.errorKind"), String(record.error_kind ?? "")],
    ["Request ID", String(record.request_id ?? record.id ?? "")],
  ];
}

function formatPriceRatio(value: unknown) {
  const ratio = Number(value);
  if (!Number.isFinite(ratio)) return "-";
  return `${ratio.toLocaleString(currentAppLanguage(), { maximumFractionDigits: 6 })}×`;
}

export function formatCallMoney(value: unknown, cnyPerUSD?: number) {
  const amount = Number(value);
  if (!Number.isFinite(amount)) return "-";
  const displayed = accountDisplayAmountFromUSD(amount, cnyPerUSD);
  const symbol = accountDisplayCurrency(cnyPerUSD) === "CNY" ? "\u00a5" : "$";
  return `${symbol}${displayed.toLocaleString(currentAppLanguage(), {
    minimumFractionDigits: displayed !== 0 && Math.abs(displayed) < 0.01 ? 6 : 2,
    maximumFractionDigits: 12,
  })}`;
}

export function callRecordOutputUsage(record: CallRecord) {
  let outputTokens = nonNegativeNumber(record.output_tokens);
  let reasoningTokens: number | undefined;
  if (Array.isArray(record.usage_meters)) {
    for (const meter of record.usage_meters) {
      if (meter?.meter_kind !== "reasoning_tokens") continue;
      const quantity = optionalNonNegativeNumber(meter.quantity);
      if (quantity === undefined || nonNegativeNumber(meter.quantity_scale) > 1) continue;
      reasoningTokens = quantity;
      const included = meter.included_in_total ?? meter.metadata?.included_in_total;
      if (included === false) outputTokens += quantity;
      break;
    }
  }
  return {
    outputTokens,
    reasoningTokens: reasoningTokens !== undefined && reasoningTokens <= outputTokens ? reasoningTokens : undefined,
  };
}

export function formatCacheRate(value: number | undefined) {
  if (value === undefined || !Number.isFinite(value) || value < 0) return "-";
  return `${(value * 100).toLocaleString(currentAppLanguage(), {
    minimumFractionDigits: 1,
    maximumFractionDigits: 2,
  })}%`;
}

function formatTokenCount(value: number) {
  return Math.round(value).toLocaleString(currentAppLanguage());
}

export function callRecordCacheRate(record: CallRecord) {
  if (!cacheMetricObserved(record)) return undefined;
  const base = cacheBaseInputTokenCount(record);
  return base > 0 ? nonNegativeNumber(record.cache_read_tokens) / base : undefined;
}

function optionalTokenMetric(value: unknown, observed = false) {
  if (value === undefined || value === null) return observed ? formatTokenCount(0) : "";
  const number = Number(value);
  return Number.isFinite(number) && number >= 0 ? formatTokenCount(number) : "";
}

function optionalCacheWriteTokens(record: CallRecord) {
  if (!cacheWriteMetricObserved(record)) return "";
  return formatTokenCount(cacheWriteTokenCount(record));
}

function cacheMetricObserved(record: CallRecord) {
  if (!callRecordOK(record)) return false;
  if (
    (record.cache_read_tokens !== undefined && record.cache_read_tokens !== null)
    || cacheWriteMetricObserved(record)
  ) {
    return true;
  }
  const source = firstNonEmptyString(record.usage_source, record.pricing_evidence?.usage_source)
    .toLowerCase();
  return ["upstream", "upstream_usage", "converted", "mixed"].includes(source)
    && optionalNonNegativeNumber(record.input_tokens) !== undefined;
}

function usageStatCacheMetricObserved(stat: UsageStat) {
  if (String(stat.status ?? "").toLowerCase() === "failed") return false;
  if (optionalNonNegativeNumber(stat.cache_base_input_tokens) !== undefined) return true;
  return cacheMetricObserved(stat);
}

function cacheWriteMetricObserved(record: CallRecord) {
  return [record.cache_write_tokens, record.cache_write_5m_tokens, record.cache_write_1h_tokens]
    .some((value) => value !== undefined && value !== null);
}

function cacheWriteTokenCount(value: CallRecord) {
  const total = nonNegativeNumber(value.cache_write_tokens);
  if (total > 0) return total;
  return nonNegativeNumber(value.cache_write_5m_tokens)
    + nonNegativeNumber(value.cache_write_1h_tokens);
}

function cacheBaseInputTokenCount(value: CallRecord) {
  const explicit = optionalNonNegativeNumber(value.cache_base_input_tokens);
  if (explicit !== undefined && (explicit > 0 || nonNegativeNumber(value.input_tokens) === 0 || value.scope === "retained_history")) {
    return explicit;
  }

  const protocol = firstNonEmptyString(
    value.upstream_protocol,
    value.target_protocol,
    value.inbound_protocol,
    protocolFromPath(String(value.request_path ?? value.path ?? "")),
  );
  const inputIncludesCacheRead = typeof value.input_tokens_include_cache_read === "boolean"
    ? value.input_tokens_include_cache_read
    : protocol !== "anthropic_messages";
  const inputIncludesCacheWrite = typeof value.input_tokens_include_cache_write === "boolean"
    ? value.input_tokens_include_cache_write
    : protocol !== "anthropic_messages";
  return nonNegativeNumber(value.input_tokens)
    + (inputIncludesCacheRead ? 0 : nonNegativeNumber(value.cache_read_tokens))
    + (inputIncludesCacheWrite ? 0 : cacheWriteTokenCount(value));
}

function optionalNonNegativeNumber(value: unknown) {
  if (value === undefined || value === null || value === "") return undefined;
  const number = Number(value);
  return Number.isFinite(number) && number >= 0 ? number : undefined;
}

function nonNegativeNumber(value: unknown) {
  return optionalNonNegativeNumber(value) ?? 0;
}

export function callRecordID(record: CallRecord) {
  return String(record.id ?? record.request_id ?? `${record.ts ?? ""}-${record.channel_id ?? ""}-${record.path ?? ""}`);
}

function boolLabel(value: unknown) {
  if (typeof value === "boolean") return value ? tr("common.yes") : tr("common.no");
  return "";
}

export function callLogRefreshErrorText(
  source: CallLogSource,
  error: unknown,
  accountState: CallLogAccountState,
) {
  if (source === "local_supplier") return tr("callLogs.errors.localRead");
  if (accountState === "signed_out") return tr("callLogs.errors.signedOut");
  if (accountState === "expired") return tr("callLogs.errors.sessionExpired");
  if (accountState === "refreshing") return tr("callLogs.errors.sessionRefreshing");

  const detail = String(error ?? "").toLowerCase();
  if (
    detail.includes("401")
    || detail.includes("403")
    || detail.includes("unauthorized")
    || detail.includes("forbidden")
  ) {
    return tr("callLogs.errors.deviceCredential");
  }
  if (
    detail.includes("timeout")
    || detail.includes("timed out")
    || detail.includes("connection")
    || detail.includes("network")
    || detail.includes("transport")
    || detail.includes("endpoint")
  ) {
    return tr("callLogs.errors.platformUnreachable");
  }
  return tr("callLogs.errors.platformRead");
}

function optionalNumber(value: unknown) {
  const number = Number(value ?? 0);
  return Number.isFinite(number) && number > 0 ? String(number) : "";
}

function callRecordCursorMs(record: CallRecord) {
  const direct = Number(record.cursor_ms ?? record.created_ms ?? record.ts_ms ?? 0);
  if (Number.isFinite(direct) && direct > 0) return direct;
  const seconds = Number(record.ts ?? 0);
  if (Number.isFinite(seconds) && seconds > 0) return seconds * 1000;
  const createdAt = String(record.created_at ?? "");
  if (createdAt) {
    const parsed = Date.parse(createdAt);
    if (Number.isFinite(parsed)) return parsed;
  }
  return 0;
}

export function callRecordsCursorMs(records: CallRecord[]) {
  return records.reduce((max, record) => Math.max(max, callRecordCursorMs(record)), 0);
}

export function mergeCallRecords(current: CallRecord[], incoming: CallRecord[]) {
  const byId = new Map<string, CallRecord>();
  for (const record of current ?? []) {
    byId.set(callRecordID(record), record);
  }
  for (const record of incoming ?? []) {
    byId.set(callRecordID(record), record);
  }
  return Array.from(byId.values()).sort((a, b) => callRecordCursorMs(b) - callRecordCursorMs(a));
}

export function snapshotCursorMs(
  snapshot: {
    platformRecords: CallRecord[];
    usageRecords: CallRecord[];
    localRecords: CallRecord[];
    platformCursorMs: number;
    localCursorMs: number;
  },
  source: CallLogSource,
) {
  if (source === "platform") {
    return snapshot.platformCursorMs || callRecordsCursorMs(snapshot.platformRecords.length ? snapshot.platformRecords : snapshot.usageRecords);
  }
  return snapshot.localCursorMs || callRecordsCursorMs(snapshot.localRecords);
}

function callRecordOK(record: CallRecord) {
  if (typeof record.ok === "boolean") return record.ok;
  if (record.status === "success") return true;
  if (record.status === "failed") return false;
  const httpStatus = Number(record.http_status ?? record.status ?? 0);
  return httpStatus >= 200 && httpStatus < 300;
}

function callRecordStatus(record: CallRecord) {
  if (record.status === "success" || record.status === "failed") return record.status;
  if (record.http_status) return `HTTP ${record.http_status}`;
  if (record.status) return String(record.status);
  return callRecordOK(record) ? "success" : "failed";
}

function callRecordModel(record: CallRecord) {
  return String(record.model ?? record.model_requested ?? "-");
}

function callRecordPath(record: CallRecord) {
  return String(record.request_path ?? record.path ?? "-");
}

function callRecordProtocol(record: CallRecord) {
  const inboundProtocol = callRecordInboundProtocol(record);
  const upstreamProtocol = callRecordUpstreamProtocol(record);
  if (inboundProtocol && upstreamProtocol && inboundProtocol !== upstreamProtocol) {
    return `${inboundProtocol} -> ${upstreamProtocol}`;
  }
  return upstreamProtocol || inboundProtocol || "-";
}

function callRecordInboundProtocol(record: CallRecord) {
  return firstNonEmptyString(
    record.inbound_protocol,
    record.request_protocol,
    protocolFromPath(callRecordPath(record)),
    record.protocol,
  );
}

function callRecordUpstreamProtocol(record: CallRecord) {
  if (String(record.direction ?? "") === "local_supplier") {
    return firstNonEmptyString(record.upstream_protocol);
  }
  return firstNonEmptyString(
    record.upstream_protocol,
    record.target_protocol,
    record.protocol,
  );
}

function callRecordDirection(record: CallRecord) {
  const value = String(record.direction ?? "");
  if (value === "platform_routed") return tr("callLogs.directions.platformRouted");
  if (value === "platform_provider") return tr("callLogs.directions.platformProvider");
  if (value === "local_supplier") return tr("callLogs.directions.localSupplier");
  return value || "-";
}

function callRecordRoute(record: CallRecord) {
  const route = String(record.route_mode ?? "");
  const direction = String(record.direction ?? "");
  const ingress = String(record.ingress ?? "");
  if (direction === "local_supplier") {
    if (ingress === "supplier_agent") return tr("callLogs.routes.platformMatch");
    if (ingress === "local_proxy") return tr("callLogs.routes.localMatch");
    return tr("callLogs.routes.supplyExecution");
  }
  if (route === "supplier") return tr("callLogs.routes.supplierNode");
  if (route === "provider") return tr("callLogs.routes.platformProvider");
  if (route === "local") return tr("callLogs.routes.localChannel");
  if (route === "platform_auto") return tr("callLogs.routes.platformAuto");
  return route || direction || "-";
}

function callRecordInboundRequestBytes(record: CallRecord) {
  return firstPositiveNumber(record.inbound_request_bytes, record.request_inbound_bytes, record.request_bytes);
}

function callRecordUpstreamRequestBytes(record: CallRecord) {
  return firstPositiveNumber(record.upstream_request_bytes, record.request_upstream_bytes, record.request_bytes);
}

function callRecordResponseWireBytes(record: CallRecord) {
  return firstPositiveNumber(record.response_wire_bytes, record.upstream_response_bytes, record.response_bytes);
}

function callRecordResponseBodyBytes(record: CallRecord) {
  return firstPositiveNumber(record.response_body_bytes, record.returned_response_bytes, record.response_bytes);
}

function firstPositiveNumber(...values: unknown[]) {
  for (const value of values) {
    const number = Number(value ?? 0);
    if (Number.isFinite(number) && number > 0) return number;
  }
  return 0;
}

function firstNonEmptyString(...values: unknown[]) {
  for (const value of values) {
    const text = String(value ?? "").trim();
    if (text) return text;
  }
  return "";
}

function protocolFromPath(path: string) {
  if (path.includes("/v1/responses")) return "openai_responses";
  if (path.includes("/v1/messages") || path.endsWith("/messages")) return "anthropic_messages";
  if (path.includes("/v1beta/") || path.includes("/v1/models/")) return "gemini_native";
  if (path.includes("/v1/chat/completions")) return "openai_chat";
  return "";
}

export function responseKindLabel(value: unknown) {
  const kind = String(value ?? "");
  if (kind === "tool_call") return tr("callLogs.responseKinds.toolCall");
  if (kind === "mixed") return tr("callLogs.responseKinds.mixed");
  if (kind === "text") return tr("callLogs.responseKinds.text");
  if (kind === "empty") return tr("callLogs.responseKinds.empty");
  if (kind === "error") return tr("common.error");
  if (kind === "unknown" || !kind) return "-";
  return kind;
}

function callRecordDuration(record: CallRecord) {
  return Number(record.duration_ms ?? record.latency_ms ?? 0);
}

function callRecordError(record: CallRecord) {
  if (callRecordOK(record)) return "";
  return String(record.error ?? record.error_kind ?? "");
}

export function formatCallTime(record: CallRecord) {
  const rawCreatedAt = String(record.created_at ?? "").trim();
  const date = rawCreatedAt
    ? new Date(rawCreatedAt)
    : new Date(Number(record.ts ?? 0) * 1000);
  if (Number.isNaN(date.getTime()) || (!rawCreatedAt && !Number(record.ts))) {
    return rawCreatedAt || "-";
  }
  const twoDigits = (value: number) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${twoDigits(date.getMonth() + 1)}-${twoDigits(date.getDate())} ${twoDigits(date.getHours())}:${twoDigits(date.getMinutes())}:${twoDigits(date.getSeconds())}`;
}

function formatCallLogRefreshTime(value: number) {
  if (!value) return "-";
  return new Date(value).toLocaleTimeString(currentAppLanguage());
}

function formatDurationMS(value: number) {
  if (!value || Number.isNaN(value)) return "-";
  return `${Math.round(value)}ms`;
}
