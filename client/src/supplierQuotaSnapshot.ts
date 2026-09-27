export type SupplierQuotaWindowSnapshot = {
  source?: string;
  window?: string;
  remaining_ratio?: number;
  used_percent?: number;
  reset_at_unix?: number;
  checked_at_unix?: number;
  model?: string;
  token_type?: string;
};

export type SupplierQuotaSnapshot = {
  quota_status?: string;
  remaining_ratio?: number;
  daily_used?: number;
  daily_limit?: number;
  quota_checked_at_unix?: number;
  quota_windows?: SupplierQuotaWindowSnapshot[];
  captured_at_unix: number;
};

export type SupplierQuotaSnapshotMap = Record<string, SupplierQuotaSnapshot>;

type SupplierQuotaObservation = Omit<SupplierQuotaSnapshot, "captured_at_unix" | "quota_windows"> & {
  quota_windows?: unknown[];
};

const EMPTY_QUOTA_STATUSES = new Set(["", "unknown", "not_applicable", "pending"]);

export function parseSupplierQuotaSnapshots(raw: string | null): SupplierQuotaSnapshotMap {
  if (!raw) return {};
  try {
    const parsed = JSON.parse(raw) as unknown;
    if (!isRecord(parsed)) return {};
    const snapshots: SupplierQuotaSnapshotMap = {};
    for (const [channelId, value] of Object.entries(parsed)) {
      if (!channelId.trim() || !isQuotaSnapshot(value)) continue;
      snapshots[channelId] = value;
    }
    return snapshots;
  } catch {
    return {};
  }
}

export function rememberSupplierQuotaSnapshot(
  current: SupplierQuotaSnapshotMap,
  channelId: string,
  observation: SupplierQuotaObservation | undefined,
  capturedAtUnix = Math.floor(Date.now() / 1000),
): SupplierQuotaSnapshotMap {
  const normalizedId = channelId.trim();
  const snapshot = quotaSnapshotFromObservation(observation, capturedAtUnix);
  if (!normalizedId || !snapshot) return current;
  const previous = current[normalizedId];
  if (previous && sameQuota(previous, snapshot)) return current;
  return { ...current, [normalizedId]: snapshot };
}

export function resolveSupplierQuotaHealth<T extends SupplierQuotaObservation>(
  runtime: T | undefined,
  cached: SupplierQuotaSnapshot | undefined,
): T | SupplierQuotaSnapshot | undefined {
  if (hasUsefulQuota(runtime)) return runtime;
  return cached ?? runtime;
}

function quotaSnapshotFromObservation(
  observation: SupplierQuotaObservation | undefined,
  capturedAtUnix: number,
): SupplierQuotaSnapshot | undefined {
  if (!observation || !hasUsefulQuota(observation)) return undefined;
  return {
    quota_status: observation.quota_status,
    remaining_ratio: finiteNumber(observation.remaining_ratio),
    daily_used: finiteNumber(observation.daily_used),
    daily_limit: finiteNumber(observation.daily_limit),
    quota_checked_at_unix: finiteNumber(observation.quota_checked_at_unix),
    quota_windows: normalizedQuotaWindows(observation.quota_windows),
    captured_at_unix: capturedAtUnix,
  };
}

function hasUsefulQuota(observation: SupplierQuotaObservation | undefined): boolean {
  if (!observation) return false;
  if (Array.isArray(observation.quota_windows) && observation.quota_windows.length > 0) return true;
  const status = observation.quota_status?.trim().toLowerCase() ?? "";
  if (!EMPTY_QUOTA_STATUSES.has(status)) return true;
  // Rust health snapshots serialize unavailable numeric quota fields as zero.
  // Treating that placeholder as fresh evidence discards the last observed
  // official quota window whenever its short routing TTL expires.
  return (Number.isFinite(observation.remaining_ratio) && (observation.remaining_ratio ?? 0) > 0)
    || (Number.isFinite(observation.daily_limit) && (observation.daily_limit ?? 0) > 0);
}

function sameQuota(left: SupplierQuotaSnapshot, right: SupplierQuotaSnapshot): boolean {
  return JSON.stringify({ ...left, captured_at_unix: 0 }) === JSON.stringify({ ...right, captured_at_unix: 0 });
}

function finiteNumber(value: number | undefined): number | undefined {
  return Number.isFinite(value) ? value : undefined;
}

function normalizedQuotaWindows(value: unknown[] | undefined): SupplierQuotaWindowSnapshot[] | undefined {
  if (!Array.isArray(value)) return undefined;
  const windows = value.filter(isRecord).map((item) => ({
    source: typeof item.source === "string" ? item.source : undefined,
    window: typeof item.window === "string" ? item.window : undefined,
    remaining_ratio: finiteNumber(item.remaining_ratio),
    used_percent: finiteNumber(item.used_percent),
    reset_at_unix: finiteNumber(item.reset_at_unix),
    checked_at_unix: finiteNumber(item.checked_at_unix),
    model: typeof item.model === "string" ? item.model : undefined,
    token_type: typeof item.token_type === "string" ? item.token_type : undefined,
  }));
  return windows.length > 0 ? windows : undefined;
}

function isQuotaSnapshot(value: unknown): value is SupplierQuotaSnapshot {
  if (!isRecord(value) || !Number.isFinite(value.captured_at_unix)) return false;
  if (value.quota_status !== undefined && typeof value.quota_status !== "string") return false;
  for (const key of ["remaining_ratio", "daily_used", "daily_limit", "quota_checked_at_unix"] as const) {
    if (value[key] !== undefined && !Number.isFinite(value[key])) return false;
  }
  if (value.quota_windows !== undefined && !Array.isArray(value.quota_windows)) return false;
  return true;
}

function isRecord(value: unknown): value is Record<string, any> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
