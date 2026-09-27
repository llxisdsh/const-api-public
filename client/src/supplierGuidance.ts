export type SupplierGuidanceTone = "neutral" | "warning";

export type SupplierLimitGuidance = {
  key: string;
  tone: SupplierGuidanceTone;
};

function normalizedLimit(value: number): number {
  if (!Number.isFinite(value)) return 0;
  return Math.max(0, Math.trunc(value));
}

export function supplierConcurrencyGuidance(value: number): SupplierLimitGuidance {
  const concurrency = normalizedLimit(value);
  if (concurrency === 0) {
    return { key: "supplier.advanced.concurrencyUnlimitedWarning", tone: "warning" };
  }
  if (concurrency < 5) {
    return { key: "supplier.advanced.concurrencyCriticalWarning", tone: "warning" };
  }
  return { key: "supplier.advanced.concurrencyCapacityGuidance", tone: "neutral" };
}

export function supplierRPMGuidance(rpmValue: number, concurrencyValue: number): SupplierLimitGuidance {
  const rpm = normalizedLimit(rpmValue);
  const concurrency = normalizedLimit(concurrencyValue);
  if (rpm === 0) {
    return { key: "supplier.advanced.rpmUnlimitedRecommended", tone: "neutral" };
  }
  if (concurrency > 0 && rpm < concurrency) {
    return { key: "supplier.advanced.rpmBelowConcurrencyWarning", tone: "warning" };
  }
  if (rpm < DEFAULT_SUPPLIER_MAX_CONCURRENCY) {
    return { key: "supplier.advanced.rpmLowWarning", tone: "warning" };
  }
  return { key: "supplier.advanced.rpmConfigured", tone: "neutral" };
}
import { DEFAULT_SUPPLIER_MAX_CONCURRENCY } from "./pricingPolicy";
