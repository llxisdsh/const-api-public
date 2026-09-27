import { describe, expect, it } from "vitest";
import {
  parseSupplierQuotaSnapshots,
  rememberSupplierQuotaSnapshot,
  resolveSupplierQuotaHealth,
} from "./supplierQuotaSnapshot";

describe("supplier quota snapshots", () => {
  it("keeps the last detected quota after runtime health disappears", () => {
    const snapshots = rememberSupplierQuotaSnapshot({}, "channel-1", {
      quota_status: "available",
      remaining_ratio: 0.84,
      quota_windows: [{ source: "codex", window: "7d", remaining_ratio: 0.84 }],
    }, 123);

    expect(resolveSupplierQuotaHealth(undefined, snapshots["channel-1"])).toMatchObject({
      quota_status: "available",
      remaining_ratio: 0.84,
      captured_at_unix: 123,
    });
  });

  it("does not replace useful cached quota with an unknown runtime observation", () => {
    const cached = {
      quota_status: "available",
      remaining_ratio: 0.75,
      quota_windows: [{ source: "codex", window: "7d", remaining_ratio: 0.75 }],
      captured_at_unix: 100,
    };

    expect(resolveSupplierQuotaHealth({ quota_status: "unknown" }, cached)).toMatchObject({
      quota_status: "available",
      remaining_ratio: 0.75,
    });
  });

  it("does not let serialized zero placeholders erase the last official quota windows", () => {
    const cached = {
      quota_status: "available",
      remaining_ratio: 0.75,
      quota_windows: [{ source: "codex", window: "7d", remaining_ratio: 0.75 }],
      captured_at_unix: 100,
    };

    expect(rememberSupplierQuotaSnapshot({ "channel-1": cached }, "channel-1", {
      quota_status: "unknown",
      remaining_ratio: 0,
      daily_used: 0,
      daily_limit: 0,
      quota_windows: [],
    }, 200)).toEqual({ "channel-1": cached });
    expect(resolveSupplierQuotaHealth({
      quota_status: "unknown",
      remaining_ratio: 0,
      daily_used: 0,
      daily_limit: 0,
      quota_windows: [],
    }, cached)).toEqual(cached);
  });

  it("prefers a fresh useful runtime quota observation", () => {
    const cached = {
      quota_status: "available",
      remaining_ratio: 0.75,
      captured_at_unix: 100,
    };

    expect(resolveSupplierQuotaHealth({
      quota_status: "available",
      remaining_ratio: 0.62,
    }, cached)).toMatchObject({
      quota_status: "available",
      remaining_ratio: 0.62,
    });
  });

  it("ignores malformed persisted snapshots", () => {
    expect(parseSupplierQuotaSnapshots("not-json")).toEqual({});
    expect(parseSupplierQuotaSnapshots('{"channel-1":{"quota_status":42}}')).toEqual({});
  });
});
