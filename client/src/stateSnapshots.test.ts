import { describe, expect, it } from "vitest";

import { keepPreviousIfStructurallyEqual } from "./stateSnapshots";

describe("state snapshots", () => {
  it("keeps the previous reference when a polled payload is unchanged", () => {
    const previous = { running: true, channels: [{ id: "channel-1", status: "available" }] };
    const next = { running: true, channels: [{ id: "channel-1", status: "available" }] };

    expect(keepPreviousIfStructurallyEqual(previous, next)).toBe(previous);
  });

  it("returns the new payload when a polled value changes", () => {
    const previous = { running: true, channels: [{ id: "channel-1", status: "available" }] };
    const next = { running: true, channels: [{ id: "channel-1", status: "offline" }] };

    expect(keepPreviousIfStructurallyEqual(previous, next)).toBe(next);
  });
});
