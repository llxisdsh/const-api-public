import { act, renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { RELEASE_UPDATES_SUPPORTED } from "./runtimeProfile";

import {
  isExperimentalToolsShortcut,
  resolveClientModePolicy,
  useClientModePolicy,
} from "./clientModePolicy";

describe("client mode policy", () => {
  it("keeps the normal Dev profile unrestricted and exposes Dev surfaces", () => {
    const policy = resolveClientModePolicy({
      developmentProfile: true,
      viteDevelopment: true,
      productionPresentationRequested: false,
    });

    expect(policy.preview).toEqual({ available: true, active: false });
    expect(policy.experimental).toEqual({ toolsUnlocked: false });
    expect(policy.presentation).toEqual({
      mode: "development",
      showDevelopmentBranding: true,
      showDevelopmentTools: true,
      showDebugConsole: true,
      showReleasePreferences: false,
      guardWebviewChrome: false,
    });
    expect(policy.capabilities).toEqual({
      debugConsole: true,
      developmentEndpoint: true,
      releaseUpdates: false,
      releasePreferences: false,
    });
  });

  it("changes only presentation while previewing production from Dev", () => {
    const policy = resolveClientModePolicy({
      developmentProfile: true,
      viteDevelopment: true,
      productionPresentationRequested: true,
    });

    expect(policy.preview).toEqual({ available: true, active: true });
    expect(policy.presentation).toEqual({
      mode: "production",
      showDevelopmentBranding: false,
      showDevelopmentTools: false,
      showDebugConsole: false,
      showReleasePreferences: true,
      guardWebviewChrome: true,
    });
    expect(policy.capabilities).toEqual({
      debugConsole: true,
      developmentEndpoint: true,
      releaseUpdates: false,
      releasePreferences: false,
    });
  });

  it("cannot enable the preview outside the isolated Dev profile", () => {
    const policy = resolveClientModePolicy({
      developmentProfile: false,
      viteDevelopment: false,
      productionPresentationRequested: true,
    });

    expect(policy.preview).toEqual({ available: false, active: false });
    expect(policy.presentation.mode).toBe("production");
    expect(policy.presentation.showDebugConsole).toBe(false);
    expect(policy.capabilities.debugConsole).toBe(false);
    expect(policy.capabilities.releaseUpdates).toBe(RELEASE_UPDATES_SUPPORTED);
    expect(policy.capabilities.releasePreferences).toBe(false);
  });

  it("unlocks only the debug console in a production build", () => {
    const policy = resolveClientModePolicy({
      developmentProfile: false,
      viteDevelopment: false,
      productionPresentationRequested: false,
      experimentalToolsUnlocked: true,
    });

    expect(policy.experimental).toEqual({ toolsUnlocked: true });
    expect(policy.presentation.showDevelopmentTools).toBe(false);
    expect(policy.presentation.showDevelopmentBranding).toBe(false);
    expect(policy.presentation.showDebugConsole).toBe(true);
    expect(policy.capabilities.debugConsole).toBe(true);
    expect(policy.capabilities.developmentEndpoint).toBe(false);
  });

  it("keeps updates unavailable in an edition without an update service", () => {
    const policy = resolveClientModePolicy({
      developmentProfile: false,
      viteDevelopment: false,
      productionPresentationRequested: true,
      experimentalToolsUnlocked: true,
      releaseUpdatesSupported: false,
    });
    expect(policy.presentation.mode).toBe("production");
    expect(policy.capabilities.releaseUpdates).toBe(false);
  });

  it("requires the full primary+alt+shift+d chord", () => {
    const base = {
      key: "d",
      metaKey: false,
      ctrlKey: true,
      altKey: true,
      shiftKey: true,
      repeat: false,
    };
    expect(isExperimentalToolsShortcut(base)).toBe(true);
    expect(isExperimentalToolsShortcut({ ...base, ctrlKey: false, metaKey: true })).toBe(true);
    expect(isExperimentalToolsShortcut({ ...base, altKey: false })).toBe(false);
    expect(isExperimentalToolsShortcut({ ...base, shiftKey: false })).toBe(false);
    expect(isExperimentalToolsShortcut({ ...base, key: "x" })).toBe(false);
    expect(isExperimentalToolsShortcut({ ...base, repeat: true })).toBe(false);
  });

  it("unlocks experimental tools for only the current hook session", () => {
    const { result, unmount } = renderHook(() => useClientModePolicy({
      developmentProfile: false,
      viteDevelopment: false,
    }));

    expect(result.current.policy.presentation.showDebugConsole).toBe(false);
    const unlock = new KeyboardEvent("keydown", {
      key: "D",
      ctrlKey: true,
      altKey: true,
      shiftKey: true,
      cancelable: true,
    });
    act(() => window.dispatchEvent(unlock));

    expect(unlock.defaultPrevented).toBe(true);
    expect(result.current.policy.experimental.toolsUnlocked).toBe(true);
    expect(result.current.policy.presentation.showDebugConsole).toBe(true);
    unmount();

    const nextSession = renderHook(() => useClientModePolicy({
      developmentProfile: false,
      viteDevelopment: false,
    }));
    expect(nextSession.result.current.policy.presentation.showDebugConsole).toBe(false);
    nextSession.unmount();
  });

  it("installs and removes the production interaction guard with the Dev preview", () => {
    const { result, unmount } = renderHook(() => useClientModePolicy({
      developmentProfile: true,
      viteDevelopment: true,
    }));

    expect(document.documentElement.dataset.clientPresentation).toBe("development");

    act(() => result.current.setProductionPresentationPreview(true));
    expect(document.documentElement.dataset.clientPresentation).toBe("production");
    const guardedShortcut = new KeyboardEvent("keydown", {
      key: "F12",
      cancelable: true,
    });
    window.dispatchEvent(guardedShortcut);
    expect(guardedShortcut.defaultPrevented).toBe(true);

    act(() => result.current.setProductionPresentationPreview(false));
    const restoredShortcut = new KeyboardEvent("keydown", {
      key: "F12",
      cancelable: true,
    });
    window.dispatchEvent(restoredShortcut);
    expect(restoredShortcut.defaultPrevented).toBe(false);

    unmount();
    expect(document.documentElement.dataset.clientPresentation).toBeUndefined();
  });
});
