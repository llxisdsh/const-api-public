import { describe, expect, test, vi } from "vitest";

import {
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
});
