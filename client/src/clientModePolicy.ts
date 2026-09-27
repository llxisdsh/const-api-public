import { useCallback, useEffect, useMemo, useState } from "react";

import { installWebviewShortcutGuard } from "./webviewShortcutGuard";

export type ClientPresentationMode = "development" | "production";

export type ClientModePolicy = {
  runtime: {
    developmentProfile: boolean;
    viteDevelopment: boolean;
  };
  preview: {
    available: boolean;
    active: boolean;
  };
  experimental: {
    toolsUnlocked: boolean;
  };
  presentation: {
    mode: ClientPresentationMode;
    showDevelopmentBranding: boolean;
    showDevelopmentTools: boolean;
    showDebugConsole: boolean;
    showReleasePreferences: boolean;
    guardWebviewChrome: boolean;
  };
  capabilities: {
    debugConsole: boolean;
    developmentEndpoint: boolean;
    releaseUpdates: boolean;
    releasePreferences: boolean;
  };
};

type ClientModeInput = {
  developmentProfile: boolean;
  viteDevelopment: boolean;
  productionPresentationRequested: boolean;
  experimentalToolsUnlocked?: boolean;
};

type ExperimentalToolsShortcutEvent = Pick<
  KeyboardEvent,
  "key" | "metaKey" | "ctrlKey" | "altKey" | "shiftKey" | "repeat"
>;

export function isExperimentalToolsShortcut(event: ExperimentalToolsShortcutEvent) {
  const primaryModifier = event.metaKey || event.ctrlKey;
  return !event.repeat
    && primaryModifier
    && event.altKey
    && event.shiftKey
    && event.key.toLowerCase() === "d";
}

export function resolveClientModePolicy({
  developmentProfile,
  viteDevelopment,
  productionPresentationRequested,
  experimentalToolsUnlocked = false,
}: ClientModeInput): ClientModePolicy {
  const productionPreviewActive = developmentProfile && productionPresentationRequested;
  const productionPresentation = !developmentProfile || productionPreviewActive;

  return {
    runtime: {
      developmentProfile,
      viteDevelopment,
    },
    preview: {
      available: developmentProfile,
      active: productionPreviewActive,
    },
    experimental: {
      toolsUnlocked: experimentalToolsUnlocked,
    },
    presentation: {
      mode: productionPresentation ? "production" : "development",
      showDevelopmentBranding: developmentProfile && !productionPreviewActive,
      showDevelopmentTools: developmentProfile && !productionPreviewActive,
      showDebugConsole: (developmentProfile && !productionPreviewActive) || experimentalToolsUnlocked,
      showReleasePreferences: productionPresentation,
      guardWebviewChrome: !viteDevelopment || productionPreviewActive,
    },
    // These describe real runtime authority. The presentation preview must never
    // turn a Dev process into a release updater, data root, or startup manager.
    capabilities: {
      debugConsole: developmentProfile || experimentalToolsUnlocked,
      developmentEndpoint: viteDevelopment,
      releaseUpdates: false,
      releasePreferences: false,
    },
  };
}

export function useClientModePolicy({
  developmentProfile,
  viteDevelopment,
}: Omit<ClientModeInput, "productionPresentationRequested" | "experimentalToolsUnlocked">) {
  const [productionPresentationRequested, setProductionPresentationRequested] = useState(false);
  const [experimentalToolsUnlocked, setExperimentalToolsUnlocked] = useState(false);
  const policy = useMemo(
    () => resolveClientModePolicy({
      developmentProfile,
      viteDevelopment,
      productionPresentationRequested,
      experimentalToolsUnlocked,
    }),
    [developmentProfile, experimentalToolsUnlocked, productionPresentationRequested, viteDevelopment],
  );

  const setProductionPresentationPreview = useCallback((enabled: boolean) => {
    if (!developmentProfile) return;
    setProductionPresentationRequested(enabled);
  }, [developmentProfile]);

  useEffect(() => {
    const unlockExperimentalTools = (event: KeyboardEvent) => {
      if (!isExperimentalToolsShortcut(event)) return;
      event.preventDefault();
      event.stopPropagation();
      setExperimentalToolsUnlocked(true);
    };
    window.addEventListener("keydown", unlockExperimentalTools, true);
    return () => window.removeEventListener("keydown", unlockExperimentalTools, true);
  }, []);

  useEffect(() => {
    const root = document.documentElement;
    const previousMode = root.dataset.clientPresentation;
    root.dataset.clientPresentation = policy.presentation.mode;
    return () => {
      if (previousMode === undefined) delete root.dataset.clientPresentation;
      else root.dataset.clientPresentation = previousMode;
    };
  }, [policy.presentation.mode]);

  useEffect(() => {
    // Packaged release builds install this guard before React mounts. This
    // session-scoped effect exists only to let the isolated Dev app preview it.
    if (!policy.preview.active) return;
    return installWebviewShortcutGuard(window);
  }, [policy.preview.active]);

  return {
    policy,
    setProductionPresentationPreview,
  };
}
