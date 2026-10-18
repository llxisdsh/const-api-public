export const UI_REFRESH_THROTTLE_MS = 10_000;
export const UI_SNAPSHOT_AUTO_REFRESH_MS = 5 * 60_000;
export const UI_HIDDEN_POLL_RETRY_MS = 60_000;

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

export type VisibilityPollOptions = {
  intervalMs: number;
  initialDelayMs?: number;
  hiddenRetryMs?: number;
  refresh: () => void | Promise<void>;
};

/**
 * Poll UI-only state while the window is visible. Hidden windows keep only a
 * cheap timer so a missed visibility event cannot leave the UI stale forever;
 * no refresh callback is invoked until the document is visible again.
 */
export function startVisibilityPoll({
  intervalMs,
  initialDelayMs = intervalMs,
  hiddenRetryMs = UI_HIDDEN_POLL_RETRY_MS,
  refresh,
}: VisibilityPollOptions): () => void {
  let disposed = false;
  let inFlight = false;
  let timer: number | undefined;

  const clearTimer = () => {
    if (timer !== undefined) {
      window.clearTimeout(timer);
      timer = undefined;
    }
  };

  const schedule = (delayMs: number) => {
    if (disposed) return;
    clearTimer();
    timer = window.setTimeout(() => {
      timer = undefined;
      void run();
    }, Math.max(0, delayMs));
  };

  const run = async () => {
    if (disposed) return;
    if (document.visibilityState !== "visible") {
      schedule(hiddenRetryMs);
      return;
    }
    if (inFlight) return;
    inFlight = true;
    try {
      await refresh();
    } finally {
      inFlight = false;
      if (!disposed) {
        schedule(document.visibilityState === "visible" ? intervalMs : hiddenRetryMs);
      }
    }
  };

  const refreshWhenVisible = () => {
    if (document.visibilityState !== "visible") return;
    clearTimer();
    void run();
  };

  document.addEventListener("visibilitychange", refreshWhenVisible);
  window.addEventListener("focus", refreshWhenVisible);
  schedule(document.visibilityState === "visible" ? initialDelayMs : hiddenRetryMs);

  return () => {
    disposed = true;
    clearTimer();
    document.removeEventListener("visibilitychange", refreshWhenVisible);
    window.removeEventListener("focus", refreshWhenVisible);
  };
}
