import { describe, expect, test } from "vitest";

import { supplierConcurrencyGuidance, supplierRPMGuidance } from "./supplierGuidance";

describe("supplier capacity guidance", () => {
  test.each([
    [0, "supplier.advanced.concurrencyUnlimitedWarning", "warning"],
    [4, "supplier.advanced.concurrencyCriticalWarning", "warning"],
    [19, "supplier.advanced.concurrencyCapacityGuidance", "neutral"],
    [20, "supplier.advanced.concurrencyCapacityGuidance", "neutral"],
    [80, "supplier.advanced.concurrencyCapacityGuidance", "neutral"],
  ] as const)("classifies concurrency %s", (value, key, tone) => {
    expect(supplierConcurrencyGuidance(value)).toEqual({ key, tone });
  });

  test.each([
    [0, 20, "supplier.advanced.rpmUnlimitedRecommended", "neutral"],
    [10, 20, "supplier.advanced.rpmBelowConcurrencyWarning", "warning"],
    [10, 5, "supplier.advanced.rpmLowWarning", "warning"],
    [60, 20, "supplier.advanced.rpmConfigured", "neutral"],
  ] as const)("classifies rpm %s at concurrency %s", (rpm, concurrency, key, tone) => {
    expect(supplierRPMGuidance(rpm, concurrency)).toEqual({ key, tone });
  });
});
