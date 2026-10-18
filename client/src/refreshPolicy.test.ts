import { describe, expect, test, vi } from "vitest";

import {
  startVisibilityPoll,
  UI_REFRESH_THROTTLE_MS,
  UI_SNAPSHOT_AUTO_REFRESH_MS,
  createRefreshGate,
} from "./refreshPolicy";

describe("refresh policy", () => {
  test("shares one ten-second gate between view and manual refreshes", () => {
    let now = 1_000;
    const gate = createRefreshGate(UI_REFRESH_THROTTLE_MS, () => now);
    expect(gate.begin()).toBe(true);
    now += 9_999;
    expect(gate.begin()).toBe(false);
    now += 1;
    expect(gate.begin()).toBe(true);
  });

  test("allows a confirmed state change to force one immediate refresh", () => {
    const now = vi.fn(() => 5_000);
    const gate = createRefreshGate(UI_REFRESH_THROTTLE_MS, now);
    expect(gate.begin()).toBe(true);
    expect(gate.begin()).toBe(false);
    expect(gate.begin({ force: true })).toBe(true);
    expect(gate.begin()).toBe(false);
  });

  test("does not remain blocked when the system clock moves backwards", () => {
    let now = 20_000;
    const gate = createRefreshGate(UI_REFRESH_THROTTLE_MS, () => now);
    expect(gate.begin()).toBe(true);
    now = 1_000;
    expect(gate.begin()).toBe(true);
  });

  test("uses a five-minute automatic refresh cadence for snapshots", () => {
    expect(UI_SNAPSHOT_AUTO_REFRESH_MS).toBe(300_000);
  });

  test("does not refresh a hidden window and refreshes once when it becomes visible", async () => {
    vi.useFakeTimers();
    const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    const refresh = vi.fn().mockResolvedValue(undefined);
    const stop = startVisibilityPoll({
      intervalMs: 5_000,
      initialDelayMs: 0,
      hiddenRetryMs: 60_000,
      refresh,
    });

    await vi.advanceTimersByTimeAsync(60_000);
    expect(refresh).not.toHaveBeenCalled();

    visibility.mockReturnValue("visible");
    document.dispatchEvent(new Event("visibilitychange"));
    await vi.advanceTimersByTimeAsync(0);
    expect(refresh).toHaveBeenCalledTimes(1);

    stop();
    visibility.mockRestore();
    vi.useRealTimers();
  });

  test("does not overlap a slow refresh with the next poll", async () => {
    vi.useFakeTimers();
    const pending: { resolve: () => void } = { resolve: () => undefined };
    const refresh = vi.fn(() => new Promise<void>((resolve) => {
      pending.resolve = resolve;
    }));
    const stop = startVisibilityPoll({ intervalMs: 5_000, initialDelayMs: 0, refresh });

    await vi.advanceTimersByTimeAsync(0);
    expect(refresh).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(5_000);
    expect(refresh).toHaveBeenCalledTimes(1);

    pending.resolve();
    await vi.advanceTimersByTimeAsync(0);
    stop();
    vi.useRealTimers();
  });
});
