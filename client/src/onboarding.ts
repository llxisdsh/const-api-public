import type { AccountStatus, BillingSummary } from "./account/accountTypes";

export type OnboardingStage = "idle" | "needs_balance" | "ready_to_use";

export type OnboardingPage = "access" | "supplier" | "account" | "about";

export type OnboardingGuideState =
  | "choose_mode"
  | "recommend_registration"
  | "local_setup"
  | "local_channel_setup"
  | "local_ready"
  | "needs_balance"
  | "low_balance"
  | "ready_to_use"
  | "supplier_intro";

export type OnboardingGuideContext = {
  page: OnboardingPage;
  signedIn: boolean;
  localModeChosen: boolean;
  localModelReady: boolean;
  onboardingSeen: boolean;
  onboardingStage: OnboardingStage;
  needsUsageBalance: boolean;
  lowUsageBalance: boolean;
  supplierTotalCount: number;
};

type OnboardingStorage = Pick<Storage, "getItem" | "setItem">;

export const ONBOARDING_SEEN_STORAGE_KEY = "const-api.onboarding.v1.seen";
export const LOCAL_MODE_STORAGE_KEY = "const-api.access-mode.v1.local";
export const LOW_USAGE_BALANCE_USD = 1;

function browserStorage(): OnboardingStorage | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

export function hasSeenOnboarding(
  storage: OnboardingStorage | null = browserStorage(),
): boolean {
  try {
    return storage?.getItem(ONBOARDING_SEEN_STORAGE_KEY) === "1";
  } catch {
    return false;
  }
}

export function markOnboardingSeen(
  storage: OnboardingStorage | null = browserStorage(),
): void {
  try {
    storage?.setItem(ONBOARDING_SEEN_STORAGE_KEY, "1");
  } catch {
    // A blocked storage backend must not prevent sign-in or the guide itself.
  }
}

export function hasChosenLocalMode(
  _storage: OnboardingStorage | null = browserStorage(),
): boolean { return true; }

export function markLocalModeChosen(
  storage: OnboardingStorage | null = browserStorage(),
): void {
  try {
    storage?.setItem(LOCAL_MODE_STORAGE_KEY, "1");
  } catch {
    // Local mode remains usable for the current session when storage is blocked.
  }
}

export function clearLocalModeChoice(
  storage: OnboardingStorage | null = browserStorage(),
): void {
  try {
    storage?.setItem(LOCAL_MODE_STORAGE_KEY, "0");
  } catch {
    // Authentication navigation must not depend on local storage.
  }
}

export function usageBalanceIsLow(balanceUSD: number | null): boolean {
  if (balanceUSD === null || !Number.isFinite(balanceUSD) || balanceUSD <= 0) return false;
  return balanceUSD < LOW_USAGE_BALANCE_USD;
}

export function resolvedUsageBalance(
  status: AccountStatus,
  billingSummary?: BillingSummary | null,
  overviewSummary?: BillingSummary | null,
): number | null {
  const candidates = [
    billingSummary?.usage_balance,
    overviewSummary?.usage_balance,
    status.balance,
  ];
  const balance = candidates.find((candidate) => (
    typeof candidate === "number" && Number.isFinite(candidate)
  ));
  return balance ?? null;
}

export function onboardingStageAfterSignIn(status: AccountStatus): OnboardingStage {
  return typeof status.balance === "number" && status.balance > 0
    ? "ready_to_use"
    : "needs_balance";
}

export function pendingOnboardingStage(
  status: AccountStatus,
  storage: OnboardingStorage | null = browserStorage(),
): OnboardingStage {
  if (status.state !== "signed_in" || hasSeenOnboarding(storage)) return "idle";
  return onboardingStageAfterSignIn(status);
}

/**
 * Resolves the single task guide that belongs to the page currently in view.
 * Runtime notices such as an interrupted request are deliberately handled by
 * the separate global guidance-notice queue.
 */
export function resolveOnboardingGuideState({
  page,
  signedIn,
  localModeChosen,
  localModelReady,
  onboardingSeen,
  onboardingStage,
  needsUsageBalance,
  lowUsageBalance,
  supplierTotalCount,
}: OnboardingGuideContext): OnboardingGuideState | null {
  if (page === "access") {
    if (signedIn) {
      if (needsUsageBalance) return "needs_balance";
      if (lowUsageBalance) return "low_balance";
      return onboardingStage === "ready_to_use" && !onboardingSeen
        ? "ready_to_use"
        : null;
    }

    if (!localModeChosen) return "choose_mode";
    if (!localModelReady) {
      return supplierTotalCount > 0 ? "local_channel_setup" : "local_setup";
    }
    return null;
  }

  if (page === "supplier") {
    if (!signedIn && localModeChosen) {
      if (!localModelReady) {
        return supplierTotalCount > 0 ? "local_channel_setup" : "local_setup";
      }
      if (!onboardingSeen) return "local_ready";
    }

    return supplierTotalCount === 0 ? "supplier_intro" : null;
  }

  return null;
}
