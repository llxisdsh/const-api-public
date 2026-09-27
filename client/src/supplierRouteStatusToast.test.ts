import { describe, expect, test } from "vitest";

import {
  shouldShowSupplierRouteStatusToast,
  supplierRouteStatusBlocksReady,
  supplierRouteStatusDetail,
  supplierRouteStatusToastIdentity,
  supplierRouteStatusToastSnapshot,
  supplierRouteStatusToastText,
  supplierPricingWarnings,
} from "./appHelpers";
import type { SupplierNodeRouteStatus } from "./appTypes";

function status(overrides: Partial<SupplierNodeRouteStatus> = {}): SupplierNodeRouteStatus {
  return {
    channel_id: "channel-openai",
    supplier_unit_id: "supplier-openai",
    state: "ready",
    updated_at_unix: 100,
    ...overrides,
  };
}

describe("supplier route status notifications", () => {
  test("normal variant tariffs do not create warnings but real pricing fallbacks do", () => {
    const normal = status({fallback_priced_models:[{model:"vendor/model:free", confidence:"variant_fallback"}]});
    expect(supplierPricingWarnings(normal)).toEqual([]);
    expect(supplierRouteStatusToastSnapshot(normal).signature).toBe(supplierRouteStatusToastSnapshot(status()).signature);
    const genuine = {model:"vendor/unknown", confidence:"family_fallback"};
    expect(supplierPricingWarnings(status({fallback_priced_models:[...normal.fallback_priced_models!,genuine]}))).toEqual([genuine]);
  });
  test("does not describe the first ready snapshot as a recovery", () => {
    const next = supplierRouteStatusToastSnapshot(status());

    expect(shouldShowSupplierRouteStatusToast(undefined, next)).toBe(false);
  });

  test("ignores timestamps when the effective status is unchanged", () => {
    const previous = supplierRouteStatusToastSnapshot(status({ updated_at_unix: 100 }));
    const next = supplierRouteStatusToastSnapshot(status({ updated_at_unix: 200 }));

    expect(previous.signature).toBe(next.signature);
    expect(shouldShowSupplierRouteStatusToast(previous, next)).toBe(false);
  });

  test("shows recovery only after an observed non-ready state", () => {
    const fallback = supplierRouteStatusToastSnapshot(status({
      state: "fallback",
      reason: "temporary_failure",
      message: "provider overloaded",
    }));
    const ready = supplierRouteStatusToastSnapshot(status({ updated_at_unix: 300 }));

    expect(shouldShowSupplierRouteStatusToast(undefined, fallback)).toBe(true);
    expect(shouldShowSupplierRouteStatusToast(fallback, ready)).toBe(true);
  });

  test("keeps changed failure diagnostics visible without repeating identical ones", () => {
    const first = supplierRouteStatusToastSnapshot(status({
      state: "fallback",
      reason: "temporary_failure",
      message: "provider overloaded",
    }));
    const repeated = supplierRouteStatusToastSnapshot(status({
      state: "fallback",
      reason: "temporary_failure",
      message: "provider overloaded",
      updated_at_unix: 500,
    }));
    const changed = supplierRouteStatusToastSnapshot(status({
      state: "fallback",
      reason: "quota_exhausted",
      message: "quota exhausted",
    }));

    expect(shouldShowSupplierRouteStatusToast(first, repeated)).toBe(false);
    expect(shouldShowSupplierRouteStatusToast(first, changed)).toBe(true);
    expect(supplierRouteStatusToastIdentity(status())).toBe("supplier-openai");
  });

  test("renders structured QUIC write deadlines as readable network evidence", () => {
    const detail = supplierRouteStatusDetail(status({
      state: "fallback",
      reason: "timeout",
      message: JSON.stringify({
        direction: "server_to_client",
        error_kind: "timeout",
        failure_scope: "channel",
        message: "deadline exceeded",
        phase: "server_write",
        timeout_ms: 10_000,
        transport: "quic",
      }),
    }));

    expect(detail).toBe("回退池：QUIC 服务端向客户端写入超时（10 秒；底层：deadline exceeded）");
  });

  test("keeps legacy scoped deadline errors readable without exposing JSON", () => {
    const detail = supplierRouteStatusDetail(status({
      state: "fallback",
      reason: "channel_failure",
      message: JSON.stringify({ failure_scope: "channel", message: "deadline exceeded" }),
    }));

    expect(detail).toBe("回退池：网络通道超过等待时限（底层：deadline exceeded）");
    expect(detail).not.toContain("failure_scope");
  });

  test("retains the reason when an unrelated JSON message has no transport schema", () => {
    const detail = supplierRouteStatusDetail(status({
      state: "fallback",
      reason: "temporary_failure",
      message: JSON.stringify({ provider: "temporarily unavailable" }),
    }));

    expect(detail).toContain("临时异常");
    expect(detail).toContain("temporarily unavailable");
  });

  test.each<{ reason: string; state: string; acceptedModels: string[] }>([
    { reason: "model_not_supported", state: "degraded", acceptedModels: [] },
    { reason: "models_partially_supported", state: "degraded", acceptedModels: ["gpt-5.5"] },
  ])("keeps catalog admission status silent for $reason", ({ reason, state, acceptedModels }) => {
    const routeStatus = status({
      state,
      reason,
      accepted_models: acceptedModels,
      unsupported_models: ["future-model"],
    });
    const snapshot = supplierRouteStatusToastSnapshot(routeStatus);

    expect(shouldShowSupplierRouteStatusToast(undefined, snapshot)).toBe(true);
    expect(supplierRouteStatusToastText(routeStatus, "Subscription")).toBe("");
    expect(supplierRouteStatusBlocksReady(routeStatus)).toBe(false);
  });
});
