export const UI_REFRESH_THROTTLE_MS = 10_000;
export const UI_SNAPSHOT_AUTO_REFRESH_MS = 5 * 60_000;

export type RefreshOptions = {
  force?: boolean;
  throttle?: boolean;
};

export type RefreshGate = {
  begin(options?: RefreshOptions): boolean;
  reset(): void;
};

export function createRefreshGate(
  intervalMs = UI_REFRESH_THROTTLE_MS,
  now: () => number = Date.now,
): RefreshGate {
  let lastStartedAt = Number.NEGATIVE_INFINITY;
  return {
    begin(options = {}) {
      const current = now();
      const elapsed = current - lastStartedAt;
      if (!options.force && elapsed >= 0 && elapsed < intervalMs) return false;
      lastStartedAt = current;
      return true;
    },
    reset() {
      lastStartedAt = Number.NEGATIVE_INFINITY;
    },
  };
}
