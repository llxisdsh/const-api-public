import { describe, expect, it } from "vitest";

import { emptyAccountStatus } from "./account/accountTypes";
import {
  clearLocalModeChoice,
  hasSeenOnboarding,
  hasChosenLocalMode,
  LOW_USAGE_BALANCE_USD,
  markLocalModeChosen,
  markOnboardingSeen,
  onboardingStageAfterSignIn,
  pendingOnboardingStage,
  resolveOnboardingGuideState,
  resolvedUsageBalance,
  usageBalanceIsLow,
} from "./onboarding";

describe("new-user onboarding state", () => {
  function memoryStorage() {
    const values = new Map<string, string>();
    return {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => values.set(key, value),
    };
  }

  it("sends a zero-balance account to top-up and a credited account to Use", () => {
    expect(onboardingStageAfterSignIn({ ...emptyAccountStatus(), balance: 0 })).toBe("needs_balance");
    expect(onboardingStageAfterSignIn({ ...emptyAccountStatus(), balance: 2 })).toBe("ready_to_use");
  });

  it("starts for any signed-in account when this installation has no guide record", () => {
    const storage = memoryStorage();
    const existingAdmin = { ...emptyAccountStatus(), state: "signed_in" as const, balance: 2 };

    expect(pendingOnboardingStage(existingAdmin, storage)).toBe("ready_to_use");
    expect(hasSeenOnboarding(storage)).toBe(false);

    markOnboardingSeen(storage);

    expect(hasSeenOnboarding(storage)).toBe(true);
    expect(pendingOnboardingStage(existingAdmin, storage)).toBe("idle");
  });

  it("does not start before authentication even without a local guide record", () => {
    expect(pendingOnboardingStage(emptyAccountStatus(), memoryStorage())).toBe("idle");
  });

  it("prefers refreshed billing data over the login snapshot", () => {
    const status = { ...emptyAccountStatus(), balance: 0 };
    expect(resolvedUsageBalance(status, {
      usage_balance: 8,
      supplier_funds: { pending: 0, available: 0, withdrawn: 0 },
    })).toBe(8);
    expect(resolvedUsageBalance({ ...emptyAccountStatus(), balance: undefined })).toBeNull();
  });

  it("always uses local mode regardless of an old choice", () => {
    const storage = memoryStorage();
    expect(hasChosenLocalMode(storage)).toBe(true);

    markLocalModeChosen(storage);
    expect(hasChosenLocalMode(storage)).toBe(true);
    expect(hasSeenOnboarding(storage)).toBe(false);

    clearLocalModeChoice(storage);
    expect(hasChosenLocalMode(storage)).toBe(true);
  });

  it("uses one ledger-dollar threshold regardless of display currency", () => {
    expect(usageBalanceIsLow(0.5)).toBe(true);
    expect(usageBalanceIsLow(0.999999)).toBe(true);
    expect(usageBalanceIsLow(LOW_USAGE_BALANCE_USD)).toBe(false);
    expect(usageBalanceIsLow(1.000001)).toBe(false);
    expect(usageBalanceIsLow(0)).toBe(false);
  });

  it("keeps task guides on the page where their copy is relevant", () => {
    const base = {
      signedIn: false,
      localModeChosen: false,
      localModelReady: false,
      onboardingSeen: false,
      onboardingStage: "idle" as const,
      needsUsageBalance: false,
      lowUsageBalance: false,
      supplierTotalCount: 0,
    };

    expect(resolveOnboardingGuideState({ ...base, page: "access" })).toBe("choose_mode");
    expect(resolveOnboardingGuideState({ ...base, page: "access", localModeChosen: true })).toBe("local_setup");
    expect(resolveOnboardingGuideState({ ...base, page: "supplier" })).toBe("supplier_intro");
    expect(resolveOnboardingGuideState({ ...base, page: "account" })).toBeNull();
    expect(resolveOnboardingGuideState({ ...base, page: "about" })).toBeNull();
  });

  it("guides a local-only user through Models and back to Use", () => {
    const localSetup = {
      page: "supplier" as const,
      signedIn: false,
      localModeChosen: true,
      localModelReady: false,
      onboardingSeen: false,
      onboardingStage: "idle" as const,
      needsUsageBalance: false,
      lowUsageBalance: false,
      supplierTotalCount: 0,
    };

    expect(resolveOnboardingGuideState(localSetup)).toBe("local_setup");
    expect(resolveOnboardingGuideState({
      ...localSetup,
      supplierTotalCount: 1,
    })).toBe("local_channel_setup");
    expect(resolveOnboardingGuideState({
      ...localSetup,
      localModelReady: true,
      supplierTotalCount: 1,
    })).toBe("local_ready");
    expect(resolveOnboardingGuideState({
      ...localSetup,
      page: "access",
      localModelReady: true,
      supplierTotalCount: 1,
    })).toBeNull();
    expect(resolveOnboardingGuideState({
      ...localSetup,
      page: "access",
      localModelReady: true,
      onboardingSeen: true,
      supplierTotalCount: 1,
    })).toBeNull();
  });

  it("shows balance and tool guidance only on Use", () => {
    const signedIn = {
      page: "access" as const,
      signedIn: true,
      localModeChosen: false,
      localModelReady: false,
      onboardingSeen: false,
      onboardingStage: "needs_balance" as const,
      needsUsageBalance: true,
      lowUsageBalance: false,
      supplierTotalCount: 0,
    };

    expect(resolveOnboardingGuideState(signedIn)).toBe("needs_balance");
    expect(resolveOnboardingGuideState({ ...signedIn, page: "supplier" })).toBe("supplier_intro");
    expect(resolveOnboardingGuideState({
      ...signedIn,
      onboardingStage: "ready_to_use",
      needsUsageBalance: false,
    })).toBe("ready_to_use");
  });

  it("does not add a supplier introduction over an existing channel list", () => {
    expect(resolveOnboardingGuideState({
      page: "supplier",
      signedIn: true,
      localModeChosen: false,
      localModelReady: false,
      onboardingSeen: true,
      onboardingStage: "idle",
      needsUsageBalance: false,
      lowUsageBalance: false,
      supplierTotalCount: 1,
    })).toBeNull();
  });
});
