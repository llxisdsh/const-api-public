import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  ArrowDown,
  ArrowUp,
  Check,
  ChevronDown,
  ChevronRight,
  CircleDollarSign,
  ClipboardPaste,
  CloudUpload,
  Copy,
  Download,
  ExternalLink,
  Eye,
  EyeOff,
  FolderOpen,
  Info,
  KeyRound,
  LoaderCircle,
  MessageSquareText,
  Minus,
  Network,
  Play,
  Plug,
  Plus,
  RefreshCw,
  RotateCw,
  Save,
  Square,
  Terminal,
  Trash2,
  Undo2,
  X
} from "lucide-react";
import React, { useEffect, useMemo, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import { useTranslation } from "react-i18next";
import titlebarLogo from "../src-tauri/icons/20x20.png";
import titlebarLogo2x from "../src-tauri/icons/40x40.png";
import { AppErrorBoundary, RendererFailure } from "./AppErrorBoundary";
import { DialogBackdrop } from "./DialogBackdrop";
import {
  LegalNotice,
  LegalNoticeLoading,
  PRIVACY_POLICY_SHA256,
  PRIVACY_POLICY_VERSION,
  USER_AGREEMENT_SHA256,
  USER_AGREEMENT_VERSION,
  type LegalDocumentId,
} from "./LegalNotice";
import { ModalCloseButton } from "./ModalCloseButton";
import { PriceRatioInput } from "./PriceRatioInput";
import { modelDisplayName } from "./modelPresentation";
import { createMarketPriceCache } from "./marketPriceCache";
import { ToolDockMenuProvider } from "./ToolDockMenu";
import { SupplierSurfaceConfig } from "./components/SupplierSurfaceConfig";
import { SupplierChannelChecks } from "./components/SupplierChannelChecks";
import { PageFindBar } from "./components/PageFindBar";
import {
  formatAccountUnsignedMoney,
} from "./account/accountFormat";
import {
  type AccountAuthMode,
  type AccountFundsEntryReason,
  type AccountViewRequest
} from "./account/accountTypes";
import {
  INITIAL_SYSTEM_PREFERS_DARK,
  INITIAL_THEME_PREFERENCE,
  THEME_OPTIONS,
  UI_SCRIPT_STARTED_AT,
  hasTauriRuntime,
  sourceDriverCardPresentation,
  startupConsoleInfo,
  startupLogFromUi,
  toolConfigVisualProgressRatio,
  useAppController,
  type AppTab
} from "./appController";
import {
  MAX_PRICE_RATIO,
  aboutServerLabel,
  aboutUpdateHint,
  channelIsPooled,
  channelKindLabel,
  channelOnlineLabel,
  channelStateTone,
  channelSwitchLabel,
  credentialStatusLabel,
  debugEmptyTargetText,
  debugProtocolOptions,
  debugTargetProtocolHint,
  debugTargetTypeLabel,
  dedupeModels,
  defaultSurfaceOperationUrl,
  emptyConfig,
  endpointNetworkState,
  endpointRegistrySourceLabel,
  endpointRegistrySourceNote,
  formatRouteScore,
  formatUnixCountdown,
  generateLocalApiKey,
  healthStatusLabel,
  modelCatalogSourceLabel,
  modelCatalogSourceNote,
  modelCatalogVersionDetails,
  modelCatalogVersionHint,
  modelCatalogVersionLabel,
  protocolLabel,
  protocolDebugOutcome,
  quotaStatusLabel,
  routeCandidateFilterText,
  routeCandidateHealthText,
  routeCandidateProtocolText,
  routeNodeLabel,
  routePlanCacheText,
  routePlanStickyText,
  safetyDetailLabel,
  safetyStatusLabel,
  subscriptionProviderLabel,
  subscriptionWizardTitle,
  supplierChannelTransportForChannel,
  supplierChannelModelCount,
  supplierNetworkState,
  supplierPricingFallbackSummary,
  supplierPricingWarnings,
  supplierProtocolLatencyLabel,
  supplierProtocolLatencyParts,
  supplierRouteStatusBlocksReady,
  supplierRouteStatusDetail,
  supplierRouteStatusForChannel,
  surfaceLabel,
  surfaceOperationOptions,
  updateSourceLabel,
  updateSourceNote,
  updateStatusLabel
} from "./appHelpers";
import type {
  DebugProtocol
} from "./appTypes";
import {
  CallLogModal,
  formatByteCount,
  responseKindLabel
} from "./callLogs";
import { claudeModelSettingsEqual } from "./claudeModelSettings";
import { useClientModePolicy, type ClientModePolicy } from "./clientModePolicy";
import {
  BlockingConfirmDialog,
  CapabilityProfileList,
  ChannelDuplicateDialog,
  ClaudeModelSettingsPanel,
  CodexModelSourcePanel,
  CompactChoiceMenu,
  HiddenToolMenu,
  ModelInput,
  QuickCard,
  SupplierChoiceCard,
  ToolConfigDetailPanel,
  ToolConfigPathField,
  ToolProtocolPanel,
  ToolRemoveDialog,
  VsCodeExtensionLinkage,
} from "./components/AppControls";
import { GuidanceNoticeCard } from "./components/GuidanceNoticeCard";
import { LanShareChannelQuota } from "./components/LanShareChannelQuota";
import { OnboardingGuide } from "./components/OnboardingGuide";
import { DiagnosticsPanel } from "./diagnostics";
import {
  activeGuidanceNotice,
  balanceInterruptionNoticeKey,
} from "./guidanceNotices";
import {
  changeAppLanguage,
  currentAppLanguage,
  initializeI18n,
  localizedError,
  type SupportedLanguage,
} from "./i18n";
import {
  LOCAL_API_CONNECTIONS,
  type LocalApiConnectionId,
} from "./localApiSurfaces";
import { useNativeToasts } from "./nativeToasts";
import {
  dismissPlatformAnnouncement,
  platformAnnouncementDismissed,
  platformAnnouncementNoticeKey,
} from "./platformAnnouncements";
import {
  clearLocalModeChoice,
  hasChosenLocalMode,
  hasSeenOnboarding,
  markLocalModeChosen,
  markOnboardingSeen,
  pendingOnboardingStage,
  resolveOnboardingGuideState,
  resolvedUsageBalance,
  usageBalanceIsLow,
  type OnboardingGuideState,
  type OnboardingStage,
} from "./onboarding";
import {
  APPLICATION_DISPLAY_NAME,
  DEFAULT_DEVELOPMENT_ENDPOINT,
  DEVELOPMENT_PROFILE,
  PRODUCT_DISPLAY_NAME,
} from "./runtimeProfile";
import { installAutoHidingScrollbars } from "./scrollbarVisibility";
import {
  sourceDriverActionLabel,
  sourceDriverDescription,
  sourceDriverDisplayName,
  sourceDriverTitle,
} from "./sourceDriverPresentation";
import {
  sourceDriverById,
  type SourceDriver
} from "./sourceDrivers";
import "./styles.css";
import "./styles/window-shell.css";
import { supplierConcurrencyGuidance } from "./supplierGuidance";
import { QuotaVisual, quotaDetailLabel } from "./supplierQuota";
import {
  applyDocumentTheme,
  detectPlatform
} from "./theme";
import { toolCatalogEntry } from "./toolCatalog";
import {
  DEFAULT_TOOL_PROTOCOLS,
  codexModelSourceFromStatus,
  toolConfigDetailState,
  toolConfigurationActionKey,
  toolPrimaryAction,
  toolProtocolId,
  toolProtocolMenuState,
  toolSyncsModelsOnLaunch
} from "./toolMenuPresentation";
import {
  compareToolProgramCandidates,
  isToolProgramCandidateLaunchable,
} from "./toolProgramCandidates";
import { useEscapeDismiss } from "./useEscapeDismiss";
import {
  installWebviewShortcutGuard,
  shouldInstallWebviewShortcutGuard,
} from "./webviewShortcutGuard";
export {
  advanceToolConfigProgress,
  toolFirstConfigurationNotice,
  toolConfigOperationResultNotice,
  toolConfigProgressLabel,
  toolConfigProgressRatio,
  toolConfigSuccessNotice,
  toolConfigVisualProgressRatio,
  watchToolConfigOperation
} from "./appController";

const AccountPage = React.lazy(() => import("./account/AccountPage")
  .then((module) => ({ default: module.AccountPage })));
const ConnectionGuideModal = React.lazy(() => import("./components/ConnectionGuideModal")
  .then((module) => ({ default: module.ConnectionGuideModal })));
const LanSharingDrawer = React.lazy(() => import("./components/LanSharingPanel")
  .then((module) => ({ default: module.LanSharingDrawer })));
const LanShareConnectionWizard = React.lazy(() => import("./components/LanShareConnectionWizard")
  .then((module) => ({ default: module.LanShareConnectionWizard })));
const MarketPricesModal = React.lazy(() => import("./MarketPricesModal")
  .then((module) => ({ default: module.MarketPricesModal })));
const ModelCompatibilityEditor = React.lazy(() => import("./components/ModelCompatibilityEditor")
  .then((module) => ({ default: module.ModelCompatibilityEditor })));
const OperationCapabilityMatrix = React.lazy(() => import("./components/OperationCapabilityMatrix")
  .then((module) => ({ default: module.OperationCapabilityMatrix })));
const SupplierModelPoolDialog = React.lazy(() => import("./components/SupplierModelPoolDialog")
  .then((module) => ({ default: module.SupplierModelPoolDialog })));
type ConstApiWindow = typeof window & {
  __CONST_API_ROOT__?: ReturnType<typeof createRoot>;
  __CONST_API_SCROLLBAR_VISIBILITY_CLEANUP__?: () => void;
  __CONST_API_SHORTCUT_GUARD_CLEANUP__?: () => void;
};

const appWindow = window as ConstApiWindow;
applyDocumentTheme(INITIAL_THEME_PREFERENCE, INITIAL_SYSTEM_PREFERS_DARK);
document.documentElement.dataset.platform = detectPlatform();
document.documentElement.dataset.desktopRuntime = hasTauriRuntime() ? "tauri" : "browser";
appWindow.__CONST_API_SCROLLBAR_VISIBILITY_CLEANUP__?.();
appWindow.__CONST_API_SCROLLBAR_VISIBILITY_CLEANUP__ = installAutoHidingScrollbars();
if (shouldInstallWebviewShortcutGuard(import.meta.env.DEV, hasTauriRuntime())) {
  appWindow.__CONST_API_SHORTCUT_GUARD_CLEANUP__?.();
  appWindow.__CONST_API_SHORTCUT_GUARD_CLEANUP__ = installWebviewShortcutGuard();
}
startupConsoleInfo("[const-api][startup][ui] script loaded, readyState=%s", document.readyState);
void startupLogFromUi("script loaded");

type ProductAppProps = {
  initialTab?: AppTab;
  initialAccountAuthMode?: AccountAuthMode;
  clientModePolicy: ClientModePolicy;
  setProductionPresentationPreview: (enabled: boolean) => void;
};

type MarketPricesContext = {
  initialView: "catalog" | "offers";
  channel?: {
    channelId: string;
    channelName: string;
  };
};

function ResetLocalDataDialog({
  busy,
  onCancel,
  onConfirm,
}: {
  busy: boolean;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const { t } = useTranslation();
  const [toolsHandled, setToolsHandled] = useState(false);
  useEscapeDismiss(onCancel, !busy);

  return (
    <DialogBackdrop
      className="confirm-backdrop"
      aria-labelledby="reset-local-data-title"
      aria-describedby="reset-local-data-message"
    >
      <div className="modal-panel confirm-modal reset-local-data-modal">
        <div className="section-title-row reset-local-data-header">
          <div>
            <h2 id="reset-local-data-title">{t("about.resetLocalDataConfirmTitle")}</h2>
            <p id="reset-local-data-message">{t("about.resetLocalDataConfirmMessage")}</p>
          </div>
          <ModalCloseButton onClick={onCancel} disabled={busy} />
        </div>
        <div className="reset-local-data-tools">
          <strong>{t("about.resetLocalDataToolsTitle")}</strong>
          <p>{t("about.resetLocalDataToolsHint")}</p>
          <label>
            <input
              type="checkbox"
              checked={toolsHandled}
              onChange={(event) => setToolsHandled(event.target.checked)}
            />
            <span>{t("about.resetLocalDataToolsConfirmed")}</span>
          </label>
        </div>
        <div className="confirm-actions">
          <button type="button" className="quiet" disabled={busy} onClick={onCancel}>
            {t("common.cancel")}
          </button>
          <button
            type="button"
            className="danger"
            disabled={busy || !toolsHandled}
            onClick={onConfirm}
          >
            {busy ? t("about.resettingLocalData") : t("about.resetLocalDataConfirmAction")}
          </button>
        </div>
      </div>
    </DialogBackdrop>
  );
}

function ProductApp({
  initialTab = "access",
  initialAccountAuthMode = "password",
  clientModePolicy,
  setProductionPresentationPreview,
}: ProductAppProps) {
  const { t } = useTranslation();
  const [onboardingStage, setOnboardingStage] = useState<OnboardingStage>("idle");
  const [onboardingSeen, setOnboardingSeen] = useState(() => hasSeenOnboarding());
  const [localModeChosen, setLocalModeChosen] = useState(() => hasChosenLocalMode());
  const [onboardingGuideCollapsed, setOnboardingGuideCollapsed] = useState(false);
  const {
    tab,
    setTab,
    config,
    setConfig,
    status,
    supplierStatus,
    account,
    activeCallLog,
    selectedCallRecord,
    setSelectedCallRecord,
    toolConfigStatuses,
    toolConfigChecking,
    toolProtocolDraftTouchedRef,
    toolProtocolSelections,
    codexModelSource,
    changeCodexModelSource,
    setToolProtocolSelections,
    claudeModelSettings,
    claudeModelDraft,
    setClaudeModelDraft,
    toolLaunching,
    toolOperationLabels,
    toolOperationProgress,
    toolLocator,
    setToolLocator,
    showMissingToolCandidates,
    setShowMissingToolCandidates,
    subscriptionWizardProvider,
    subscriptionWizardOpen,
    subscriptionOAuthSession,
    subscriptionCallbackInput,
    setSubscriptionCallbackInput,
    subscriptionTokenJson,
    setSubscriptionTokenJson,
    subscriptionManualOpen,
    setSubscriptionManualOpen,
    subscriptionWizardBusy,
    subscriptionWizardProgress,
    subscriptionImportCanRetry,
    supplierAddOpen,
    setSupplierAddOpen,
    supplierDetailOpen,
    supplierStartingChannelId,
    pendingSupplierChannelIds,
    showAccessAdvanced,
    setShowAccessAdvanced,
    accessGuideOpen,
    setAccessGuideOpen,
    localApiConnectionId,
    setLocalApiConnectionId,
    modelAliasPreviewOpen,
    setModelAliasPreviewOpen,
    modelCompatibilityPolicy,
    modelCompatibilityDraft,
    setModelCompatibilityDraft,
    modelCompatibilitySaving,
    showSupplierAdvanced,
    setShowSupplierAdvanced,
    showSupplierSurfaceBindings,
    setShowSupplierSurfaceBindings,
    showSupplierProtocolDeclaration,
    setShowSupplierProtocolDeclaration,
    debugConsoleOpen,
    setDebugConsoleOpen,
    debugTargetType,
    expandedDebugTargetGroups,
    debugInboundProtocol,
    debugTargetProtocol,
    setDebugTargetProtocol,
    setDebugModel,
    debugPrompt,
    setDebugPrompt,
    debugStream,
    setDebugStream,
    debugSkipLocalShortCircuit,
    setDebugSkipLocalShortCircuit,
    debugClaudeServerSideCompaction,
    changeDebugClaudeServerSideCompaction,
    debugResult,
    debugRunning,
    routePlanState,
    debugEndpointRefreshing,
    debugEndpointLastRefresh,
    debugEndpointError,
    supplierTestPrompt,
    setSupplierTestPrompt,
    supplierTestResult,
    updateState,
    autoInstallUpdates,
    setAutoInstallUpdates,
    autostartEnabled,
    autostartBusy,
    changeAutostart,
    developmentEndpointBusy,
    changeDevelopmentEndpoint,
    themePreference,
    setThemePreference,
    appMeta,
    toast,
    showToast,
    guidanceNotices,
    showGuidanceNotice,
    dismissGuidanceNotice,
    confirmDialog,
    toolRemoveDialog,
    channelDuplicateDialog,
    appLogs,
    setAppLogs,
    logOpen,
    setLogOpen,
    copiedField,
    hasChanges,
    configActionBusy,
    activeDiagnostics,
    hasDiagnostics,
    localEntryIssue,
    brandVersion,
    updateReady,
    selectedLocalApiConnection,
    localApiRoot,
    localBaseUrl,
    localOpenAiChatUrl,
    localAnthropicBaseUrl,
    localGeminiBaseUrl,
    channels,
    displayChannels,
    selectedChannel,
    selectedApiFormat,
    selectedModelOptions,
    selectedChannelKindLabel,
    selectedChannelFields,
    selectedPrimaryProtocolOptions,
    selectedSurfaceBindings,
    selectedSurfaceEditorRows,
    selectedChannelModel,
    selectedChannelCanDetect,
    selectedChannelCanSupply,
    selectedChannelHealth,
    selectedChannelProtocolLatencyHealth,
    selectedChannelQuotaHealth,
    selectedChannelRouteStatus,
    selectedChannelRouteLabel,
    selectedChannelSwitchOn,
    selectedChannelHasChanges,
    selectedChannelOperation,
    selectedChannelBusy,
    supplierLocalReadyCount,
    selectedChannelLifecycle,
    supplierTotalCount,
    debugTargetGroups,
    selectedDebugTarget,
    effectiveDebugTargetId,
    debugModelOptions,
    effectiveDebugModel,
    debugResponseContent,
    debugRawResponse,
    debugMetrics,
    routePlanUnavailableReason,
    routePlanSourceText,
    debugEndpointStatusText,
    recommendedDebugTargetProtocol,
    debugConversionText,
    visibleToolCards,
    hiddenToolCards,
    hideToolCard,
    showToolCard,
    resetToolOrder,
    subscriptionCards,
    apiAccessCards,
    lanShareCards,
    customCards,
    logText,
    handleDiagnosticAction,
    refreshModelCompatibilityPolicy,
    saveModelCompatibilityPolicy,
    resetModelCompatibilityPolicy,
    save,
    start,
    stop,
    restart,
    cancelChanges,
    changeLanAccess,
    closeSupplierDetail,
    openSupplierChannel,
    moveLocalChannel,
    openLocalSupplierCallLog,
    closeCallLog,
    refreshAccount,
    checkAndDownloadUpdate,
    installPreparedUpdate,
    updateSupplier,
    updateSupplierSurfaceBinding,
    toggleSupplierSurfaceProtocol,
    selectCustomSupplierDefaultProtocol,
    addSupplierOperationOverride,
    updateSupplierOperationOverride,
    removeSupplierOperationOverride,
    removeSelectedChannel,
    supplierChannelNeedsDetection,
    saveSupplierChanges,
    startSupplier,
    stopSelectedChannel,
    refreshSupplierModels,
    refreshRoutePlan,
    runProtocolDebug,
    testSupplierUpstream,
    selectDebugTarget,
    updateDebugInboundProtocol,
    toggleDebugTargetGroup,
    askConfirm,
    resolveConfirmDialog,
    resolveToolRemoveDialog,
    resolveChannelDuplicateDialog,
    applyToolConfig,
    startToolProgram,
    launchToolConfig,
    removeToolConfig,
    openToolLocator,
    chooseToolProgram,
    saveToolProgram,
    createSubscriptionOAuth,
    exchangeSubscriptionOAuth,
    importSubscriptionOAuthFromClipboard,
    importSubscriptionJson,
    closeSubscriptionWizard,
    addSubscriptionPreset,
    addPresetChannel,
    addLanSharePresetChannel,
    copyValue,
    copyText,
    openExternalUrl,
  } = useAppController({
    initialTab,
    initialAccountAuthMode,
    showDevelopmentPresentation: clientModePolicy.presentation.showDevelopmentTools,
  });

  const selectedChannelProtocolLatency = supplierProtocolLatencyParts(
    selectedChannelProtocolLatencyHealth,
  );

  const [requestedAccountView, setRequestedAccountView] = useState<AccountViewRequest | null>(null);
  const [previewAutoInstallUpdates, setPreviewAutoInstallUpdates] = useState(false);
  const [previewAutostartEnabled, setPreviewAutostartEnabled] = useState(false);
  const [lanShareWizardDriver, setLanShareWizardDriver] = useState<SourceDriver | null>(null);
  const [supplierAddTab, setSupplierAddTab] = useState<"official" | "local" | "lan">("official");
  const releasePreferencesPreviewOnly = clientModePolicy.presentation.showReleasePreferences
    && !clientModePolicy.capabilities.releasePreferences;
  const debugOutcome = debugResult ? protocolDebugOutcome(debugResult) : null;
  const debugOutcomeCopy = debugOutcome === "request_failed"
    ? {
        tone: "error",
        title: t("debugConsole.result.outcome.failedTitle"),
        detail: t("debugConsole.result.outcome.failedDetail", {
          status: debugResult?.http_status ?? "-",
          layer: debugResult?.error_layer || t("debugConsole.result.outcome.responseLayer"),
        }),
      }
    : debugOutcome === "compaction_triggered"
      ? {
          tone: "success",
          title: t("debugConsole.result.outcome.triggeredTitle"),
          detail: t("debugConsole.result.outcome.triggeredDetail"),
        }
      : debugOutcome === "compaction_not_triggered"
        ? {
            tone: "warning",
            title: t("debugConsole.result.outcome.notTriggeredTitle"),
            detail: t("debugConsole.result.outcome.notTriggeredDetail"),
          }
        : debugOutcome === "request_succeeded"
          ? {
              tone: "success",
              title: t("debugConsole.result.outcome.succeededTitle"),
              detail: t("debugConsole.result.outcome.succeededDetail", {
                status: debugResult?.http_status ?? "-",
              }),
            }
          : debugOutcome === "preview"
            ? {
                tone: "preview",
                title: t("debugConsole.result.outcome.previewTitle"),
                detail: t("debugConsole.result.outcome.previewDetail"),
              }
            : null;

  useEffect(() => {
    if (clientModePolicy.preview.active) return;
    setPreviewAutoInstallUpdates(autoInstallUpdates);
    setPreviewAutostartEnabled(autostartEnabled);
  }, [
    autoInstallUpdates,
    autostartEnabled,
    clientModePolicy.preview.active,
  ]);

  const usageBalance = resolvedUsageBalance(
    account.status,
    account.billingSummary,
    account.overviewSummary,
  );
  const signedIn = account.status.state === "signed_in";
  const cnyPerUSD = account.billingSummary?.display_cny_per_usd
    ?? account.overviewSummary?.display_cny_per_usd
    ?? account.status.display_cny_per_usd;
  const needsUsageBalance = signedIn && (
    onboardingStage === "needs_balance"
    || (usageBalance !== null && usageBalance <= 0)
  );
  const lowUsageBalance = signedIn
    && !needsUsageBalance
    && usageBalanceIsLow(usageBalance);
  const localModelReady = supplierLocalReadyCount > 0;
  const onboardingGuideState = resolveOnboardingGuideState({
    page: tab,
    signedIn,
    localModeChosen,
    localModelReady,
    onboardingSeen,
    onboardingStage,
    needsUsageBalance,
    lowUsageBalance,
    supplierTotalCount,
  });
  const previousOnboardingGuideStateByTabRef = useRef(
    new Map<AppTab, OnboardingGuideState | null>([[tab, onboardingGuideState]]),
  );
  const usageBalanceLabel = usageBalance === null
    ? ""
    : formatAccountUnsignedMoney(usageBalance, cnyPerUSD);

  const openAccountFunds = (reason: AccountFundsEntryReason = "onboarding") => {
    setRequestedAccountView({ view: "funds", token: Date.now(), reason });
    setTab("account");
  };

  const completeOnboarding = () => {
    markOnboardingSeen();
    setOnboardingSeen(true);
    setOnboardingStage("idle");
  };

  const startRegistration = () => {
    clearLocalModeChoice();
    setLocalModeChosen(false);
    account.setters.authMode("register");
    setTab("account");
  };

  const chooseLocalMode = () => {
    markLocalModeChosen();
    setLocalModeChosen(true);
    if (!localModelReady) setTab("supplier");
  };

  const focusOnboardingGuide = (shake = false) => {
    window.requestAnimationFrame(() => {
      const guide = document.getElementById("onboarding-guide");
      if (shake && guide && !window.matchMedia?.("(prefers-reduced-motion: reduce)").matches) {
        guide.classList.remove("is-gate-attention");
        void guide.offsetWidth;
        guide.classList.add("is-gate-attention");
        guide.addEventListener("animationend", () => {
          guide.classList.remove("is-gate-attention");
        }, { once: true });
      }
      guide?.scrollIntoView?.({ behavior: "smooth", block: "start" });
      guide?.focus({ preventScroll: true });
    });
  };

  const prepareToolUse = () => {
    if (!signedIn && !localModeChosen) {
      setOnboardingGuideCollapsed(false);
      setTab("access");
      focusOnboardingGuide(true);
      return false;
    }
    if (!signedIn && !localModelReady) {
      setOnboardingGuideCollapsed(false);
      setTab("supplier");
      focusOnboardingGuide(true);
      return false;
    }
    completeOnboarding();
    return true;
  };

  useEffect(() => {
    const previousStates = previousOnboardingGuideStateByTabRef.current;
    if (!previousStates.has(tab)) {
      previousStates.set(tab, onboardingGuideState);
      return;
    }
    const previousState = previousStates.get(tab);
    previousStates.set(tab, onboardingGuideState);
    if (previousState === onboardingGuideState) return;
    setOnboardingGuideCollapsed(false);
  }, [onboardingGuideState, tab]);

  useEffect(() => {
    if (!signedIn) {
      setOnboardingStage("idle");
      return;
    }
    if (onboardingSeen || usageBalance === null) return;
    const pendingStage = pendingOnboardingStage({ ...account.status, balance: usageBalance });
    if (pendingStage === "idle") return;
    setOnboardingStage(pendingStage);
  }, [signedIn, account.status, onboardingSeen, usageBalance]);

  useEffect(() => {
    if (onboardingStage === "needs_balance" || onboardingStage === "ready_to_use") {
      setTab("access");
    }
  }, [onboardingStage]);

  useEffect(() => {
    if (onboardingStage === "needs_balance" && usageBalance !== null && usageBalance > 0) {
      setOnboardingStage("ready_to_use");
    }
  }, [onboardingStage, usageBalance]);

  useNativeToasts(hasTauriRuntime(), (notice) => {
    if (notice.code === "insufficient_balance") {
      dismissGuidanceNotice(balanceInterruptionNoticeKey);
      showGuidanceNotice({
        dedupeKey: balanceInterruptionNoticeKey,
        scope: "global",
        tone: "warning",
        icon: "account",
        eyebrow: t("nativeNotices.insufficientBalance.eyebrow"),
        title: t("nativeNotices.insufficientBalance.title"),
        body: t("nativeNotices.insufficientBalance.body"),
        actions: [{
          id: "open_account_funds",
          label: t("nativeNotices.insufficientBalance.action"),
        }],
        dismissible: true,
      });
      return;
    }
    if (notice.code === "platform_access_restricted") {
      const restrictionUntil = notice.restrictionUntil
        ? new Date(notice.restrictionUntil)
        : null;
      const restrictionUntilLabel = restrictionUntil && Number.isFinite(restrictionUntil.getTime())
        ? restrictionUntil.toLocaleString(currentAppLanguage(), { hour12: false })
        : null;
      showGuidanceNotice({
        dedupeKey: notice.dedupeKey,
        scope: "global",
        tone: "warning",
        icon: "warning",
        eyebrow: t("nativeNotices.platformAccessRestricted.eyebrow"),
        title: t("nativeNotices.platformAccessRestricted.title"),
        body: restrictionUntilLabel
          ? t("nativeNotices.platformAccessRestricted.bodyWithTime", { until: restrictionUntilLabel })
          : t("nativeNotices.platformAccessRestricted.body"),
        dismissible: true,
      });
      return;
    }
    if (notice.code === "platform_announcement_clear") {
      dismissGuidanceNotice(platformAnnouncementNoticeKey);
      return;
    }
    if (platformAnnouncementDismissed(notice.version)) return;
    showGuidanceNotice({
      dedupeKey: notice.dedupeKey,
      persistentDismissalKey: notice.version,
      scope: "global",
      tone: "info",
      icon: "info",
      title: notice.title,
      body: notice.body,
      dismissible: true,
    });
    void invoke("request_client_attention").catch(() => {
      // Browser builds and a shutting-down native host cannot request attention.
    });
  });

  const connectionGuideModels = useMemo(
    () => dedupeModels([
      ...modelCompatibilityPolicy.localAvailableModels,
      ...modelCompatibilityPolicy.platformAvailableModels,
    ]),
    [
      modelCompatibilityPolicy.localAvailableModels,
      modelCompatibilityPolicy.platformAvailableModels,
    ],
  );
  const claudeModelOptions = useMemo(() => {
    const catalogById = new Map(modelCompatibilityPolicy.catalogModels.map((model) => [
      model.id.trim().toLowerCase(),
      model,
    ]));
    return connectionGuideModels.filter((model) => {
      const normalized = model.trim().toLowerCase();
      if (!normalized || normalized === "code-cheap" || normalized === "<local-anything>") {
        return false;
      }
      return catalogById.get(normalized)?.tool_call !== false;
    });
  }, [connectionGuideModels, modelCompatibilityPolicy.catalogModels]);
  const selectedLocalApiValueLabel = selectedLocalApiConnection.kind === "request-url"
    ? t("access.openaiChatUrl")
    : t("access.surfaceBaseUrl", { surface: selectedLocalApiConnection.label });

  const selectedSourceDriver = sourceDriverById(selectedChannel.source_driver);
  const [legalDocumentOpen, setLegalDocumentOpen] = useState<LegalDocumentId | null>(null);
  const [resetLocalDataConfirmOpen, setResetLocalDataConfirmOpen] = useState(false);
  const [resetLocalDataBusy, setResetLocalDataBusy] = useState(false);
  const [resetLocalDataError, setResetLocalDataError] = useState("");
  const [marketPricesContext, setMarketPricesContext] = useState<MarketPricesContext | null>(null);
  const [marketPriceCache] = useState(createMarketPriceCache);
  const marketPriceScope = JSON.stringify([
    account.status.state, account.status.platform_id, account.status.user_id,
  ]);
  useEffect(() => {
    marketPriceCache.clear();
  }, [marketPriceCache, marketPriceScope]);
  const [supplierModelPoolOpen, setSupplierModelPoolOpen] = useState(false);
  const [lanSharingOpen, setLanSharingOpen] = useState(false);
  const experimentalToolsUnlockedRef = useRef(clientModePolicy.experimental.toolsUnlocked);
  const selectedChannelTransport = supplierChannelTransportForChannel(
    supplierStatus,
    selectedChannel.id,
  );
  const selectedChannelRouteDetail = supplierRouteStatusDetail(selectedChannelRouteStatus);
  const selectedConcurrencyGuidance = supplierConcurrencyGuidance(
    selectedChannel.max_concurrency,
  );
  useEffect(() => {
    setSupplierModelPoolOpen(false);
  }, [selectedChannel.id]);
  useEffect(() => {
    if (!supplierDetailOpen) setSupplierModelPoolOpen(false);
  }, [supplierDetailOpen]);
  useEffect(() => {
    if (tab !== "supplier") setLanSharingOpen(false);
  }, [tab]);
  useEffect(() => {
    if (!clientModePolicy.presentation.showDebugConsole && debugConsoleOpen) {
      setDebugConsoleOpen(false);
    }
  }, [
    clientModePolicy.presentation.showDebugConsole,
    debugConsoleOpen,
    setDebugConsoleOpen,
  ]);
  useEffect(() => {
    const wasUnlocked = experimentalToolsUnlockedRef.current;
    const isUnlocked = clientModePolicy.experimental.toolsUnlocked;
    experimentalToolsUnlockedRef.current = isUnlocked;
    if (wasUnlocked || !isUnlocked) return;
    if (tab === "access") setShowAccessAdvanced(true);
    showToast(t("debugConsole.experimental.unlocked"), "success", 4200);
  }, [
    clientModePolicy.experimental.toolsUnlocked,
    setShowAccessAdvanced,
    showToast,
    t,
    tab,
  ]);
  const inlineOverlayOpen = Boolean(
    accessGuideOpen
    || marketPricesContext
    || toolLocator
    || supplierAddOpen
    || subscriptionWizardOpen
    || supplierDetailOpen
    || modelAliasPreviewOpen
    || debugConsoleOpen
    || lanSharingOpen
    || logOpen,
  );
  const guidanceLayerSuspended = Boolean(
    inlineOverlayOpen
    || confirmDialog
    || toolRemoveDialog
    || resetLocalDataConfirmOpen
    || channelDuplicateDialog
    || legalDocumentOpen,
  );
  const visibleGuidanceNotice = activeGuidanceNotice(guidanceNotices, tab);
  const visibleOnboardingGuideState = guidanceLayerSuspended ? null : onboardingGuideState;
  const onboardingGuideRenderedCollapsed = onboardingGuideCollapsed;
  const guidanceNoticeCount = guidanceLayerSuspended
    ? 0
    : (visibleOnboardingGuideState ? 1 : 0) + (visibleGuidanceNotice ? 1 : 0);
  useEscapeDismiss(() => {
    if (marketPricesContext) {
      setMarketPricesContext(null);
    } else if (logOpen) {
      setLogOpen(false);
    } else if (debugConsoleOpen) {
      setDebugConsoleOpen(false);
    } else if (supplierDetailOpen) {
      void closeSupplierDetail();
    } else if (subscriptionWizardOpen && !subscriptionWizardBusy) {
      closeSubscriptionWizard();
    } else if (supplierAddOpen) {
      setSupplierAddOpen(false);
    } else if (toolLocator) {
      setToolLocator(null);
    } else if (accessGuideOpen) {
      setAccessGuideOpen(false);
    }
  }, inlineOverlayOpen);

  const locatorCatalogEntry = toolLocator ? toolCatalogEntry(toolLocator.tool) : null;
  const updateCheckedAt = updateState.checkedAt || appMeta.updateLastCheckedAt;
  const aboutEndpointSourceNote = endpointRegistrySourceNote(appMeta);
  const aboutModelCatalogSourceNote = modelCatalogSourceNote(appMeta);
  const aboutUpdateHeadline = updateState.status === "ready_waiting_idle"
    ? updateStatusLabel(updateState)
    : updateReady
    ? t("about.downloaded", { version: updateState.version })
    : updateState.status === "idle"
      ? updateCheckedAt ? t("about.latest") : t("about.notChecked")
      : updateStatusLabel(updateState);
  const aboutUpdateDetail = aboutUpdateHint(updateState, appMeta.updateLastCheckedAt);
  const activeLanguage = currentAppLanguage();

  const resetLocalAppData = async () => {
    setResetLocalDataConfirmOpen(false);
    setResetLocalDataBusy(true);
    setResetLocalDataError("");
    try {
      await invoke("reset_local_app_data");
    } catch (error) {
      setResetLocalDataBusy(false);
      setResetLocalDataError(localizedError(error));
    }
  };

  return (
      <main>
        <aside>
          <button className={`sidebar-nav-start ${tab === "access" ? "active" : ""}`} onClick={() => setTab("access")}>
            <span className="nav-icon"><Plug size={18} /></span>
            <span>{t("nav.access")}</span>
          </button>
          <button className={tab === "supplier" ? "active" : ""} onClick={() => setTab("supplier")}>
            <span className="nav-icon"><CloudUpload size={18} /></span>
            <span>{t("nav.supplier")}</span>
          </button>
          <button
            className={`sidebar-about ${tab === "about" ? "active" : ""}`}
            type="button"
            onClick={() => setTab("about")}
            title={updateReady
              ? t("nav.updateReadyTitle", { version: brandVersion })
              : t("nav.aboutTitle", { version: brandVersion })}
          >
            <span className="nav-icon"><Info size={18} /></span>
            <span>{t("nav.about")}</span>
            {hasDiagnostics && (
              <span
                className="endpoint-dot error"
                title={t("nav.diagnostics", { count: activeDiagnostics.length })}
              />
            )}
          </button>
        </aside>

        <section>
          {tab === "access" ? (
            <>
              <header className="access-page-header">
                <div>
                  <h1>{t("access.title")}</h1>
                  <p className="access-principle">
                    <span>{t("access.principleIntro")}</span>{" "}
                    <span>{t("access.routingIntro")}</span>{" "}
                    <span className="page-intro-links">
                      <button
                        className="inline-text-link"
                        type="button"
                        onClick={() => void openLocalSupplierCallLog()}
                      >
                        {t("access.platformUsageLog")}
                      </button>
                    </span>
                  </p>
                </div>
              </header>

              <div className="access-section-intro">
                <h2>{t("access.toolsTitle")}</h2>
                <p>{t("access.toolsIntro")}</p>
              </div>
              <div className="quick-grid">
                <ToolDockMenuProvider>
                {visibleToolCards.map((card) => {
                  const configStatus = toolConfigStatuses[card.tool];
                  const configuredOnDisk = Boolean(configStatus?.already_configured);
                  const configuredProtocol = toolProtocolId(configStatus?.details?.tool_protocol);
                  const protocolState = toolProtocolMenuState(
                    card.tool,
                    toolProtocolSelections[card.tool] ?? DEFAULT_TOOL_PROTOCOLS[card.tool],
                    configuredOnDisk ? configuredProtocol : null,
                  );
                  const protocolChanged = protocolState.changed;
                  const modelSettingsChanged = card.tool === "claude"
                    && !claudeModelSettingsEqual(claudeModelSettings, claudeModelDraft);
                  const codexModelsChanged = card.tool === "codex"
                    && codexModelSource !== codexModelSourceFromStatus(configStatus);
                  const configurationChanged = protocolChanged || modelSettingsChanged || codexModelsChanged;
                  const configDetail = toolConfigDetailState(
                    configStatus,
                    toolConfigChecking,
                    configurationChanged,
                  );
                  const statusText = configDetail.status;
                  const statusTone = configDetail.tone;
                  const launching = Boolean(toolLaunching[card.tool]);
                  const operationLabel = toolOperationLabels[card.tool];
                  const operationProgress = toolOperationProgress[card.tool];
                  const primaryAction = toolPrimaryAction(configStatus, configurationChanged);
                  const syncModelsOnLaunch = toolSyncsModelsOnLaunch(configStatus);
                  const actionLabel = t(toolConfigurationActionKey(configStatus));
                  const disabled = launching;
                  const codexDetails = card.tool === "codex" ? configStatus?.details : undefined;
                  const detailPanel = card.tool === "codex" ? (
                    <div className="tool-truth-panel">
                      <ToolConfigPathField
                        state={configDetail}
                        onCopy={(value) => copyValue("tool-config-path", t("labels.toolMenu.configPath"), value)}
                      />
                      {codexDetails?.external_launcher_warning && (
                        <div className="tool-truth-row tool-truth-warning">
                          <span title={t("access.toolDetail.conflict")}>{t("access.toolDetail.conflict")}</span>
                          <strong>{codexDetails.external_launcher_warning}</strong>
                        </div>
                      )}
                    </div>
                  ) : (
                    <ToolConfigDetailPanel
                      state={configDetail}
                      onCopy={(value) => copyValue("tool-config-path", t("labels.toolMenu.configPath"), value)}
                    />
                  );
                  return (
                    <QuickCard
                      key={card.tool}
                      id={card.tool}
                      title={card.title}
                      description={t(`toolCatalog.${card.tool}.description`, { defaultValue: card.description })}
                      icon={card.icon}
                      customIcon={card.customIcon}
                      fallback={card.fallback}
                      tone={card.tone}
                      configured={configuredOnDisk}
                      statusText={operationLabel || statusText}
                      statusTone={launching ? "pending" : statusTone}
                      actionLabel={actionLabel}
                      disabled={disabled}
                      busy={launching}
                      progress={launching ? toolConfigVisualProgressRatio(operationProgress) : undefined}
                      onAction={() => {
                        if (!prepareToolUse()) return;
                        if (primaryAction === "locate") {
                          void openToolLocator(card.tool, card.title, true);
                          return;
                        }
                        if (primaryAction === "configure_and_launch" || syncModelsOnLaunch) {
                          void launchToolConfig(card.tool, card.title);
                          return;
                        }
                        void startToolProgram(card.tool, card.title, true);
                      }}
                      onConfigure={() => {
                        if (!prepareToolUse()) return;
                        void applyToolConfig(card.tool, card.title);
                      }}
                      onRemoveConfig={() => removeToolConfig(card.tool, card.title)}
                      onLocate={() => openToolLocator(card.tool, card.title)}
                      onHide={() => hideToolCard(card.tool)}
                      panels={[
                        {
                          id: "tool-info",
                          label: t("access.toolDetail.more"),
                          content: (
                            <div className="tool-dock-combined-panel">
                              <div className="tool-dock-combined-section">
                                <div className="tool-dock-protocol-field">
                                  <strong title={t("access.toolDetail.protocol")}>{t("access.toolDetail.protocol")}</strong>
                                  <ToolProtocolPanel
                                    title={card.title}
                                    state={protocolState}
                                    configuredProtocol={configuredProtocol}
                                    disabled={disabled}
                                    onSelect={(protocol) => {
                                      toolProtocolDraftTouchedRef.current[card.tool] = true;
                                      setToolProtocolSelections((current) => ({ ...current, [card.tool]: protocol }));
                                    }}
                                  />
                                </div>
                              </div>
                              <div className="tool-dock-combined-section">
                                {detailPanel}
                                {card.tool === "vscode" && (
                                  <VsCodeExtensionLinkage
                                    checking={toolConfigChecking}
                                    vscodeConfigured={configStatus?.already_configured}
                                    claudeConfigured={toolConfigStatuses.claude?.already_configured}
                                    codexConfigured={toolConfigStatuses.codex?.already_configured}
                                  />
                                )}
                              </div>
                              {card.tool === "claude" && (
                                <div className="tool-dock-combined-section">
                                  <ClaudeModelSettingsPanel
                                    settings={claudeModelDraft}
                                    models={claudeModelOptions}
                                    loading={modelCompatibilityPolicy.status === "idle"
                                      || modelCompatibilityPolicy.status === "loading"}
                                    changed={!claudeModelSettingsEqual(
                                      claudeModelSettings,
                                      claudeModelDraft,
                                    )}
                                    disabled={disabled}
                                    onChange={setClaudeModelDraft}
                                  />
                                </div>
                              )}
                              {card.tool === "codex" && (
                                <div className="tool-dock-combined-section">
                                  <CodexModelSourcePanel
                                    value={codexModelSource}
                                    disabled={disabled}
                                    onChange={changeCodexModelSource}
                                  />
                                </div>
                              )}
                            </div>
                          ),
                        },
                      ]}
                    />
                  );
                })}
                <HiddenToolMenu
                  cards={hiddenToolCards}
                  onShow={showToolCard}
                  onResetOrder={resetToolOrder}
                />
                </ToolDockMenuProvider>
              </div>
              <div className={`local-entry-panel ${status.running ? "running" : "error"}`}>
                <div className="local-entry-topline">
                  <div className="local-entry-heading">
                    <div
                      className="local-entry-status"
                      title={status.running
                        ? t("access.runningStatus")
                        : localEntryIssue?.message || t("access.stoppedStatus")}
                    >
                      <h2>{t("access.localApi")}</h2>
                      <span className="local-entry-state-dot" aria-hidden="true" />
                      <span>{status.running ? t("access.running") : t("access.notRunning")}</span>
                    </div>
                    <p>
                      {t("access.localApiIntro")}
                      <span className="inline-link-separator" aria-hidden="true">·</span>
                      <button
                        className="inline-text-link"
                        type="button"
                        onClick={() => {
                          if (!prepareToolUse()) return;
                          setAccessGuideOpen(true);
                        }}
                      >
                        {t("access.connectionGuide")}
                      </button>
                    </p>
                  </div>
                </div>
                <div className="local-entry-fields">
                  <div className="local-entry-surface-field">
                    <CompactChoiceMenu
                      className="local-entry-surface-select"
                      ariaLabel={t("access.connectionValueAria")}
                      value={localApiConnectionId}
                      options={LOCAL_API_CONNECTIONS.map((connection) => ({
                        value: connection.id,
                        label: connection.label,
                      }))}
                      onChange={(connection) => setLocalApiConnectionId(connection as LocalApiConnectionId)}
                    />
                    <code title={selectedLocalApiConnection.url}>{selectedLocalApiConnection.url}</code>
                    <button
                      type="button"
                      className={`copy-feedback-button${copiedField === "base-url" ? " copied" : ""}`}
                      onClick={() => copyValue("base-url", selectedLocalApiValueLabel, selectedLocalApiConnection.url)}
                      title={copiedField === "base-url"
                        ? t("common.copied")
                        : t("access.copyConnectionValue", { value: selectedLocalApiValueLabel })}
                      aria-label={copiedField === "base-url"
                        ? t("common.copied")
                        : t("access.copyConnectionValue", { value: selectedLocalApiValueLabel })}
                    >
                      {copiedField === "base-url" ? <Check size={15} /> : <Copy size={15} />}
                    </button>
                  </div>
                  <div className="local-entry-key-field" aria-label={t("access.apiKey")}>
                    <span>Key</span>
                    <code title={config.api_key || t("access.notSet")}>{config.api_key || t("access.notSet")}</code>
                    <button
                      type="button"
                      className={`copy-feedback-button${copiedField === "api-key" ? " copied" : ""}`}
                      onClick={() => copyValue("api-key", t("access.localApiKey"), config.api_key)}
                      title={copiedField === "api-key" ? t("common.copied") : t("access.copyApiKey")}
                      aria-label={copiedField === "api-key" ? t("common.copied") : t("access.copyApiKey")}
                    >
                      {copiedField === "api-key" ? <Check size={15} /> : <Copy size={15} />}
                    </button>
                  </div>
                </div>
                <div className="local-entry-footer">
                  <button
                    className="quiet advanced-panel-toggle local-entry-advanced-toggle"
                    type="button"
                    aria-expanded={showAccessAdvanced}
                    onClick={() => setShowAccessAdvanced((value) => !value)}
                  >
                    {showAccessAdvanced ? <EyeOff size={16} /> : <Eye size={16} />}
                    {showAccessAdvanced ? t("access.collapseAdvancedSettings") : t("access.advancedSettings")}
                  </button>
                </div>
              </div>

              {showAccessAdvanced && (
                <div className="advanced-panel">
                  <div className="advanced-section advanced-local-section">
                    <div className="control-strip">
                      <div className="advanced-section-copy">
                        <h2>{t("access.localConnection")}</h2>
                        <p>{t("access.localConnectionHint")}</p>
                      </div>
                      <div className="actions">
                        {clientModePolicy.capabilities.debugConsole
                          && clientModePolicy.presentation.showDebugConsole && (
                          <button className="quiet" type="button" onClick={() => setDebugConsoleOpen(true)}>
                            <Terminal size={16} />{t("access.debugConsole")}
                          </button>
                        )}
                        {status.running ? (
                          <>
                            {hasChanges && (
                              <>
                                <button className="primary" onClick={restart} disabled={configActionBusy}><RotateCw size={16} />{t("access.saveAndRestart")}</button>
                                <button className="quiet" onClick={cancelChanges} disabled={configActionBusy}><Undo2 size={16} />{t("common.cancel")}</button>
                              </>
                            )}
                            <button className="stop" onClick={stop} disabled={configActionBusy}><Square size={16} />{t("access.stopConnection")}</button>
                          </>
                        ) : (
                          <>
                            {hasChanges && (
                              <>
                                <button className="primary" onClick={save} disabled={configActionBusy}><Save size={16} />{t("common.save")}</button>
                                <button className="quiet" onClick={cancelChanges} disabled={configActionBusy}><Undo2 size={16} />{t("common.cancel")}</button>
                              </>
                            )}
                            <button className="start" onClick={start} disabled={configActionBusy}><Play size={16} />{t("access.startConnection")}</button>
                          </>
                        )}
                      </div>
                    </div>
                    <div className="grid advanced-local-fields">
                      <label>
                        {t("access.listenAddress")}
                        <input value={config.listen} onChange={(e) => setConfig({ ...config, listen: e.target.value })} />
                      </label>
                      <label>
                        {t("access.apiKey")}
                        <div className="copy-input">
                          <input value={config.api_key} onChange={(e) => setConfig({ ...config, api_key: e.target.value })} />
                          <button
                            type="button"
                            className="icon-button local-access-reset-button"
                            onClick={() => setConfig((current) => ({
                              ...current,
                              listen: emptyConfig.listen,
                              api_key: generateLocalApiKey(),
                              allow_lan_access: emptyConfig.allow_lan_access,
                            }))}
                            disabled={
                              config.listen === emptyConfig.listen
                              && config.api_key === emptyConfig.api_key
                              && config.allow_lan_access === emptyConfig.allow_lan_access
                            }
                            aria-label={t("access.restoreDefaults")}
                            title={t("access.restoreDefaults")}
                          >
                            <Undo2 size={15} strokeWidth={1.8} />
                          </button>
                        </div>
                      </label>
                    </div>
                  </div>
                  <div className="advanced-section advanced-routing-section">
                    <h2>{t("access.accessAndRouting")}</h2>
                    <div className="advanced-routing-options">
                      <div className="model-equivalence-toggle advanced-strategy-toggle">
                        <div className="model-equivalence-check">
                          <input
                            type="checkbox"
                            checked={Boolean(config.prefer_local_supply)}
                            onChange={(event) => setConfig({ ...config, prefer_local_supply: event.target.checked })}
                            aria-label={t("access.preferLocal")}
                          />
                          <span>
                            <strong>{t("access.preferLocal")}</strong>
                            <small>{t("access.preferLocalHint")}</small>
                          </span>
                        </div>
                      </div>
                      <div className="model-equivalence-toggle advanced-strategy-toggle">
                        <div className="model-equivalence-check">
                          <input
                            type="checkbox"
                            checked={config.allow_model_equivalence}
                            onChange={(event) => setConfig({ ...config, allow_model_equivalence: event.target.checked })}
                            aria-label={t("access.compatibleFallback")}
                          />
                          <span>
                            <strong>{t("access.compatibleFallback")}</strong>
                            <small>{t("access.compatibleFallbackHint")}</small>
                          </span>
                        </div>
                        <button
                          type="button"
                          className="quiet model-alias-preview-button"
                          onClick={() => setModelAliasPreviewOpen(true)}
                        >
                          <Eye size={14} />{t("access.customizeModels")}
                        </button>
                      </div>
                    </div>
                  </div>
                </div>
              )}

              {accessGuideOpen && (
                <React.Suspense fallback={null}><ConnectionGuideModal
                  rootUrl={localApiRoot}
                  apiKey={config.api_key}
                  openaiBaseUrl={localBaseUrl}
                  openaiChatUrl={localOpenAiChatUrl}
                  anthropicBaseUrl={localAnthropicBaseUrl}
                  geminiBaseUrl={localGeminiBaseUrl}
                  availableModels={connectionGuideModels}
                  modelStatus={modelCompatibilityPolicy.status}
                  copiedField={copiedField}
                  onCopy={(field, label, value) => void copyValue(field, label, value)}
                  onRefreshModels={refreshModelCompatibilityPolicy}
                  onOpenRoot={(url) => void openExternalUrl(url)}
                  onClose={() => setAccessGuideOpen(false)}
                /></React.Suspense>
              )}

              {modelAliasPreviewOpen && (
                <React.Suspense fallback={null}><ModelCompatibilityEditor
                  enabled={config.allow_model_equivalence}
                  policy={modelCompatibilityPolicy}
                  draft={modelCompatibilityDraft}
                  setDraft={setModelCompatibilityDraft}
                  saving={modelCompatibilitySaving}
                  onClose={() => setModelAliasPreviewOpen(false)}
                  onRefresh={refreshModelCompatibilityPolicy}
                  onSave={saveModelCompatibilityPolicy}
                  onReset={resetModelCompatibilityPolicy}
                /></React.Suspense>
              )}

              {toolLocator && (
                <DialogBackdrop aria-labelledby="tool-locator-title">
                  <div className="modal-panel tool-locator-modal">
                    <div className="section-title-row">
                      <div>
                        <h2 id="tool-locator-title">{t("access.locator.title", { tool: toolLocator.title })}</h2>
                        <p className="section-hint">{t("access.locator.hint")}</p>
                      </div>
                      <ModalCloseButton onClick={() => setToolLocator(null)} />
                    </div>
                    <div className="actions secondary-actions compact-actions">
                      <button
                        type="button"
                        onClick={() => openToolLocator(
                          toolLocator.tool,
                          toolLocator.title,
                          toolLocator.continueWithLaunch,
                        )}
                        disabled={toolLocator.loading}
                      >
                        <RefreshCw size={16} />{t("access.locator.searchAgain")}
                      </button>
                      <button
                        type="button"
                        onClick={() => chooseToolProgram(
                          toolLocator.tool,
                          toolLocator.title,
                          toolLocator.continueWithLaunch,
                        )}
                        disabled={toolLocator.loading}
                      >
                        <FolderOpen size={16} />{t("access.locator.chooseManually")}
                      </button>
                    </div>
                    {locatorCatalogEntry && (
                      <div className="tool-locator-downloads">
                        <div className="tool-locator-source-row">
                          <span>{t("access.locator.officialHint")}</span>
                          <div className="tool-locator-source-actions">
                            {locatorCatalogEntry.officialLinks.map((link) => (
                              <button
                                type="button"
                                className="quiet"
                                key={link.url}
                                onClick={() => void openExternalUrl(link.url)}
                              >
                                <ExternalLink size={15} />
                                {link.url.includes("github.com")
                                  ? t("toolCatalog.links.github")
                                  : link.url.includes("/download") || link.url.includes("Download")
                                    ? t("toolCatalog.links.download")
                                    : t("toolCatalog.links.website")}
                              </button>
                            ))}
                          </div>
                        </div>
                        {Boolean(locatorCatalogEntry.fallbackLinks?.length) && (
                          <div className="tool-locator-source-row tool-locator-fallback">
                            <div className="tool-locator-source-copy">
                              <strong>{t("access.locator.fallbackTitle")}</strong>
                              <span id="tool-locator-fallback-hint">{t("access.locator.fallbackHint")}</span>
                            </div>
                            <div className="tool-locator-source-actions">
                              {locatorCatalogEntry.fallbackLinks?.map((link) => (
                                <button
                                  type="button"
                                  className="quiet"
                                  key={link.url}
                                  title={link.url}
                                  aria-describedby="tool-locator-fallback-hint"
                                  onClick={() => void openExternalUrl(link.url)}
                                >
                                  <ExternalLink size={15} />{link.label}
                                </button>
                              ))}
                            </div>
                          </div>
                        )}
                      </div>
                    )}
                    {toolLocator.error && <div className="test-result error">{toolLocator.error}</div>}
                    {toolLocator.loading ? (
                      <p className="section-hint">{t("access.locator.searching")}</p>
                    ) : (
                      (() => {
                        const candidates = toolLocator.result?.candidates ?? [];
                        const availableCandidates = candidates.filter((candidate) => candidate.exists || candidate.selected);
                        const missingCandidates = candidates.filter((candidate) => !candidate.exists && !candidate.selected);
                        const visibleCandidates = showMissingToolCandidates
                          ? [...availableCandidates, ...missingCandidates]
                          : availableCandidates;
                        const sortedCandidates = [...visibleCandidates].sort(compareToolProgramCandidates);
                        return (
                          <div className="program-candidate-list">
                            {sortedCandidates.length ? (
                              sortedCandidates.map((candidate) => {
                                const launchable = isToolProgramCandidateLaunchable(candidate);
                                const hermesInstaller = toolLocator.tool === "hermes"
                                  && /hermes[-_ ]?setup|installer|安装器/i.test(`${candidate.label} ${candidate.path}`);
                                const status = candidate.selected && launchable
                                  ? t("access.locator.launchable")
                                  : candidate.exists
                                    ? launchable ? t("access.locator.available") : t("access.locator.unavailable")
                                    : t("access.locator.missing");
                                const title = candidate.selected
                                  ? t("access.locator.current")
                                  : hermesInstaller
                                    ? t("access.locator.hermesInstaller")
                                    : /自动发现|auto[-_ ]?detect/i.test(candidate.label)
                                      ? t("access.locator.autoDetected")
                                      : candidate.label;
                                const kindKey = candidate.kind === "windows_exe"
                                  ? "windowsExecutable"
                                  : candidate.kind === "windows_appx"
                                    ? "windowsApp"
                                    : candidate.kind === "mac_app"
                                      ? "macApplication"
                                      : candidate.kind === "command"
                                        ? "command"
                                        : "path";
                                const reason = !candidate.exists
                                  ? t("access.locator.reasons.missing")
                                  : hermesInstaller
                                    ? t("access.locator.reasons.installer")
                                    : launchable
                                      ? t("access.locator.reasons.launchable")
                                      : t("access.locator.reasons.unavailable");
                                return (
                                  <div
                                    className={`program-candidate ${candidate.selected ? "selected" : ""} ${candidate.exists ? "" : "missing"} ${launchable ? "" : "blocked"} ${candidate.recommended ? "recommended" : ""}`}
                                    key={`${candidate.kind}:${candidate.path}`}
                                  >
                                    <div>
                                      <div className="program-candidate-title">
                                        <strong>{title}</strong>
                                        {candidate.recommended && <small className="recommended-badge">{t("access.locator.recommended")}</small>}
                                        <small>{status}</small>
                                      </div>
                                      <span>{candidate.path}</span>
                                      <small>{t(`access.locator.kinds.${kindKey}`)} · {reason}</small>
                                    </div>
                                    <button
                                      type="button"
                                      className={launchable && !candidate.selected ? "primary" : "quiet"}
                                      disabled={!launchable || candidate.selected}
                                      onClick={() => saveToolProgram(
                                        toolLocator.tool,
                                        toolLocator.title,
                                        candidate.path,
                                        toolLocator.continueWithLaunch,
                                      )}
                                    >
                                      {candidate.selected ? t("access.locator.selected") : launchable ? t("access.locator.use") : t("access.locator.unavailable")}
                                    </button>
                                  </div>
                                );
                              })
                            ) : (
                              <div className="empty-state compact">
                                <strong>{t("access.locator.noneTitle")}</strong>
                                <span>{t("access.locator.noneHint")}</span>
                              </div>
                            )}
                            {missingCandidates.length > 0 && (
                              <button
                                type="button"
                                className="quiet program-toggle-missing"
                                onClick={() => setShowMissingToolCandidates((value) => !value)}
                              >
                                {showMissingToolCandidates
                                  ? t("access.locator.hideMissing")
                                  : t("access.locator.showMissing", { count: missingCandidates.length })}
                              </button>
                            )}
                          </div>
                        );
                      })()
                    )}
                  </div>
                </DialogBackdrop>
              )}
            </>
          ) : tab === "supplier" ? (
            <>
              <header>
                <div>
                  <h1>{t("supplier.title")}</h1>
                  <p>
                    {t("supplier.subtitle")}{" "}
                    <span className="page-intro-links">

                      <button
                        className="inline-text-link"
                        type="button"
                        onClick={() => void openLocalSupplierCallLog()}
                      >
                        {t("supplier.supplyLog")}
                      </button>
                    </span>
                  </p>
                </div>
              </header>

              <div className="supplier-list-panel">
                <div className="section-title-row supplier-list-title">
                  <div className="supplier-title-stack">
                    <div className="supplier-title-line">
                      <h2>{t("supplier.channels")}</h2>
                      <div className="supplier-command-metrics" aria-label={t("supplier.statusAria")}>
                        <span className="supplier-command-metric"><b>{supplierLocalReadyCount}</b> {t("supplier.localAvailableLabel")}</span>

                        <span className="supplier-command-metric"><b>{supplierTotalCount}</b> {t("supplier.totalLabel")}</span>
                        <span className="supplier-command-metric supplier-command-metric-lan-share">
                          <a
                            href="#lan-sharing"
                            className={`lan-share-model-page-entry${config.allow_lan_access ? " enabled" : ""}`}
                            aria-label={`${t("lanShare.title")} · ${config.allow_lan_access ? t("lanShare.enabled") : t("lanShare.disabled")}`}
                            title={t("lanShare.settingsHint")}
                            onClick={(event) => {
                              event.preventDefault();
                              setLanSharingOpen(true);
                            }}
                          >
                            <i className="lan-share-model-page-entry-icon" aria-hidden="true">
                              <Network size={14} />
                              {config.allow_lan_access ? (
                                <i className="lan-share-model-page-entry-dot" />
                              ) : null}
                            </i>
                            {t("lanShare.title")}
                          </a>
                        </span>
                      </div>
                    </div>
                  </div>
                  <div className="actions">
                    <button className="quiet" onClick={() => setSupplierAddOpen(true)}>
                      <Plus size={16} />{t("supplier.addChannel")}
                    </button>
                  </div>
                </div>
                <div className="supplier-channel-table">
                  <div className="supplier-channel-header" aria-hidden="true">
                    <span />
                    <span>{t("supplier.channel")}</span>
                    <span className="priority">{t("supplier.localPriority")}</span>
                    <span>{t("supplier.switch")}</span>
                    <span>{t("supplier.status")}</span>
                    <span>{t("supplier.model")}</span>
                    <span className="latency">{t("supplier.latency")}</span>
                    <span className="safety">{t("supplier.safety")}</span>
                    <span className="quota">{t("supplier.quota")}</span>
                    <span />
                  </div>
                  {displayChannels.map((channel, index) => {
                    const health = supplierStatus.channels?.find((item) => item.channel_id === channel.id);
                    const routeStatus = supplierRouteStatusForChannel(supplierStatus, channel.id);
                    const routeDetail = supplierRouteStatusDetail(routeStatus);
                    const stateTone = channelStateTone(channel, health, supplierStatus, supplierStartingChannelId, pendingSupplierChannelIds);
                    const runningLabel = channelOnlineLabel(channel, health, supplierStatus, pendingSupplierChannelIds);
                    const routeAcceptedModelCount = routeStatus?.accepted_models?.length ?? 0;
                    const totalModelCount = supplierChannelModelCount(channel, health, routeStatus);
                    const onlineModelCount = routeStatus?.accepted_models
                      ? routeAcceptedModelCount
                      : runningLabel === t("labels.channel.platformOnline")
                        ? totalModelCount
                        : 0;
                    const modelCountLabel = totalModelCount > 0 ? `${onlineModelCount}/${totalModelCount}` : "-";
                    const channelDisplayName = sourceDriverDisplayName(
                      sourceDriverById(channel.source_driver),
                      channel.name,
                      channel.id,
                    );
                    return (
                      <div
                        key={channel.id}
                        className={`supplier-channel-row ${channel.id === selectedChannel.id ? "active" : ""}`}
                        onClick={() => void openSupplierChannel(channel.id)}
                      >
                        <span className={`channel-state ${stateTone}`} title={`${t("supplier.status")}: ${runningLabel}`} />
                        <div className="channel-main">
                          <strong>{channelDisplayName}</strong>
                          <small>{channelKindLabel(channel.kind)} · {modelDisplayName(channel.upstream_model || channel.public_model) || t("supplier.notSelected")}</small>
                        </div>
                        <div
                          className="supplier-channel-priority-controls"
                          onClick={(event) => event.stopPropagation()}
                        >
                          <button
                            type="button"
                            aria-label={t("supplier.moveChannelUp", { channel: channel.name })}
                            title={t("supplier.moveChannelUp", { channel: channel.name })}
                            disabled={configActionBusy || selectedChannelHasChanges || selectedChannelBusy || index === 0}
                            onClick={() => void moveLocalChannel(channel.id, -1)}
                          >
                            <ArrowUp size={14} />
                          </button>
                          <button
                            type="button"
                            aria-label={t("supplier.moveChannelDown", { channel: channel.name })}
                            title={t("supplier.moveChannelDown", { channel: channel.name })}
                            disabled={configActionBusy || selectedChannelHasChanges || selectedChannelBusy || index === displayChannels.length - 1}
                            onClick={() => void moveLocalChannel(channel.id, 1)}
                          >
                            <ArrowDown size={14} />
                          </button>
                        </div>
                        <div className="channel-stat pool">
                          <small>{t("supplier.switch")}</small>
                          <span className={`channel-value-chip ${channelIsPooled(channel) ? "enabled" : "muted"}`}>{channelSwitchLabel(channel)}</span>
                        </div>
                        <div className="channel-stat online">
                          <small>{t("supplier.status")}</small>
                          <span className={`channel-value-chip ${stateTone}`} title={routeDetail || undefined}>{runningLabel}</span>
                        </div>
                        <div className="channel-stat models">
                          <small>{t("supplier.model")}</small>
                          <span title={totalModelCount > 0 ? t("supplier.poolModelCount", { online: onlineModelCount, total: totalModelCount }) : undefined}>{modelCountLabel}</span>
                        </div>
                        <div className="channel-stat latency">
                          <small>{t("supplier.latency")}</small>
                          <span>{health?.latency_ms ? `${health.latency_ms}ms` : "-"}</span>
                        </div>
                        <div className="channel-stat safety">
                          <small>{t("supplier.safety")}</small>
                          <span>{safetyStatusLabel(health, channel)}</span>
                        </div>
                        <div className="channel-stat quota wide">
                          <small>{t("supplier.quota")}</small>
                          <span>{quotaStatusLabel(health?.quota_status, channel)}</span>
                        </div>
                        <button
                          type="button"
                          className="supplier-channel-expand"
                          aria-label={`${t("common.open")} ${channelDisplayName}`}
                          title={`${t("common.open")} ${channelDisplayName}`}
                          onClick={(event) => {
                            event.stopPropagation();
                            void openSupplierChannel(channel.id);
                          }}
                        >
                          <ChevronRight size={18} />
                        </button>
                      </div>
                    );
                  })}
                  {displayChannels.length === 0 && (
                    <div className="supplier-channel-empty">
                      {t("supplier.noChannels")}
                    </div>
                  )}
                </div>
                <p className="supplier-market-hint">{t("supplier.channelUsageHint")}</p>
              </div>

              {lanSharingOpen ? (
                <React.Suspense fallback={null}><LanSharingDrawer
                  enabled={Boolean(config.allow_lan_access)}
                  proxyRunning={status.running}
                  disabled={configActionBusy}
                  onToggle={changeLanAccess}
                  copyText={copyText}
                  askConfirm={askConfirm}
                  onClose={() => setLanSharingOpen(false)}
                /></React.Suspense>
              ) : null}

              {supplierAddOpen && (
                <DialogBackdrop aria-labelledby="supplier-add-title">
                  <div className="modal-panel supplier-add-modal">
                    <div className="section-title-row">
                      <div>
                        <h2 id="supplier-add-title">{t("supplier.newChannel")}</h2>
                        <p className="section-hint">{t("supplier.newChannelHint")}</p>
                      </div>
                      <ModalCloseButton onClick={() => setSupplierAddOpen(false)} />
                    </div>

                    <div
                      className="account-center-tabs supplier-add-tabs"
                      role="tablist"
                      aria-label={t("supplier.addChannelTabsAria")}
                    >
                      {(["official", "local", "lan"] as const).map((item) => (
                        <button
                          key={item}
                          id={`supplier-add-tab-${item}`}
                          type="button"
                          role="tab"
                          aria-controls={`supplier-add-panel-${item}`}
                          aria-selected={supplierAddTab === item}
                          className={supplierAddTab === item ? "active" : ""}
                          onClick={() => setSupplierAddTab(item)}
                        >
                          {t(`supplier.addChannelTabs.${item}`)}
                        </button>
                      ))}
                    </div>

                    <div
                      id={`supplier-add-panel-${supplierAddTab}`}
                      className="supplier-add-tab-panel"
                      role="tabpanel"
                      aria-labelledby={`supplier-add-tab-${supplierAddTab}`}
                    >
                      {supplierAddTab === "official" && (
                        <>
                          <div className="add-channel-group">
                            <div className="section-title-row compact">
                              <div>
                                <h2>{t("supplier.accountSubscriptions")}</h2>
                                <p className="section-hint">{t("supplier.subscriptionHint")}</p>
                              </div>
                            </div>
                            <div className="add-channel-list">
                              {subscriptionCards.map((driver) => {
                                const card = sourceDriverCardPresentation(driver);
                                return <SupplierChoiceCard
                                  key={driver.id}
                                  title={sourceDriverTitle(driver)}
                                  subtitle={sourceDriverDescription(driver)}
                                  {...card}
                                  actionLabel={sourceDriverActionLabel(driver)}
                                  onSelect={() => addSubscriptionPreset(driver)}
                                />;
                              })}
                            </div>
                          </div>
                          <div className="add-channel-group">
                            <div className="section-title-row compact">
                              <div>
                                <h2>{t("supplier.apiAccess")}</h2>
                                <p className="section-hint">{t("supplier.apiAccessHint")}</p>
                              </div>
                            </div>
                            <div className="add-channel-list add-channel-list-wide">
                              {apiAccessCards.map((driver) => {
                                const card = sourceDriverCardPresentation(driver);
                                return <SupplierChoiceCard
                                  key={driver.id}
                                  title={sourceDriverTitle(driver)}
                                  subtitle={sourceDriverDescription(driver)}
                                  {...card}
                                  actionLabel={sourceDriverActionLabel(driver)}
                                  onSelect={() => addPresetChannel(driver)}
                                />;
                              })}
                            </div>
                          </div>
                        </>
                      )}

                      {supplierAddTab === "local" && (
                        <div className="add-channel-group">
                          <div className="section-title-row compact">
                            <div>
                              <h2>{t("supplier.localAndCustom")}</h2>
                              <p className="section-hint">{t("supplier.localAndCustomHint")}</p>
                            </div>
                          </div>
                          <div className="add-channel-list add-channel-list-wide">
                            {customCards.map((driver) => {
                              const card = sourceDriverCardPresentation(driver);
                              return <SupplierChoiceCard
                                key={driver.id}
                                title={sourceDriverTitle(driver)}
                                subtitle={sourceDriverDescription(driver)}
                                {...card}
                                actionLabel={sourceDriverActionLabel(driver)}
                                onSelect={() => addPresetChannel(driver)}
                              />;
                            })}
                          </div>
                        </div>
                      )}

                      {supplierAddTab === "lan" && (
                        <div className="add-channel-group">
                          <div className="section-title-row compact">
                            <div>
                              <h2>{t("supplier.lanSharing")}</h2>
                              <p className="section-hint">{t("supplier.lanSharingHint")}</p>
                            </div>
                          </div>
                          <div className="add-channel-list">
                            {lanShareCards.map((driver) => {
                              const card = sourceDriverCardPresentation(driver);
                              return <SupplierChoiceCard
                                key={driver.id}
                                title={sourceDriverTitle(driver)}
                                subtitle={sourceDriverDescription(driver)}
                                {...card}
                                actionLabel={sourceDriverActionLabel(driver)}
                                onSelect={() => setLanShareWizardDriver(driver)}
                              />;
                            })}
                          </div>
                        </div>
                      )}
                    </div>
                  </div>
                </DialogBackdrop>
              )}

              {lanShareWizardDriver && (
                <React.Suspense fallback={null}><LanShareConnectionWizard
                  disabled={configActionBusy}
                  onClose={() => setLanShareWizardDriver(null)}
                  onImport={(connection) => {
                    const driver = lanShareWizardDriver;
                    setLanShareWizardDriver(null);
                    void addLanSharePresetChannel(driver, connection);
                  }}
                  onSkip={() => {
                    const driver = lanShareWizardDriver;
                    setLanShareWizardDriver(null);
                    void addLanSharePresetChannel(driver);
                  }}
                /></React.Suspense>
              )}

              {subscriptionWizardOpen && (
                <DialogBackdrop aria-labelledby="subscription-wizard-title">
                  <div className="subscription-wizard modal-panel">
                    <div className="section-title-row">
                      <div>
                        <h2 id="subscription-wizard-title">{subscriptionWizardTitle(subscriptionWizardProvider)}</h2>
                        <p className="section-hint">{t("supplier.oauthLocalHint")}</p>
                      </div>
                      <ModalCloseButton
                        onClick={closeSubscriptionWizard}
                        disabled={subscriptionWizardBusy}
                      />
                    </div>

                    <div className="wizard-steps">
                      <div className={`wizard-step ${subscriptionOAuthSession ? "done" : "active"}`}>
                        <div className="wizard-step-index">1</div>
                        <div>
                          <strong>{t("supplier.oauthSteps.createTitle")}</strong>
                          <span>{t("supplier.oauthSteps.createHint")}</span>
                        </div>
                      </div>
                      <div className={`wizard-step ${subscriptionOAuthSession ? "active" : ""}`}>
                        <div className="wizard-step-index">2</div>
                        <div>
                          <strong>{t("supplier.oauthSteps.browserTitle")}</strong>
                          <span>{t("supplier.oauthSteps.browserHint")}</span>
                        </div>
                      </div>
                      <div className={`wizard-step ${subscriptionCallbackInput.trim() ? "active" : ""}`}>
                        <div className="wizard-step-index">3</div>
                        <div>
                          <strong>{t("supplier.oauthSteps.importTitle")}</strong>
                          <span>{t("supplier.oauthSteps.importHint")}</span>
                        </div>
                      </div>
                    </div>

                    <div className="wizard-grid">
                      <label>
                        {t("supplier.accountType")}
                        <input
                          value={subscriptionProviderLabel(subscriptionWizardProvider)}
                          readOnly
                          aria-readonly="true"
                        />
                        <span className="field-hint">{t("supplier.accountTypeHint")}</span>
                      </label>
                      <div className="wizard-actions wizard-create-action">
                        <button className="primary" onClick={() => createSubscriptionOAuth()} disabled={subscriptionWizardBusy}>
                          <KeyRound size={16} />{subscriptionOAuthSession ? t("supplier.regenerateLink") : t("supplier.generateLink")}
                        </button>
                      </div>

                      {subscriptionOAuthSession && (
                        <>
                          <div className="wizard-url-row grid-wide">
                            <label>
                              {t("supplier.authorizationLink")}
                              <input value={subscriptionOAuthSession.auth_url} readOnly />
                              <span className="field-hint">{subscriptionOAuthSession.callback_hint}</span>
                            </label>
                            <div className="wizard-url-actions">
                              <button className="quiet" onClick={() => openExternalUrl(subscriptionOAuthSession.auth_url)}>
                                <ExternalLink size={16} />{t("supplier.openAuthorization")}
                              </button>
                              <button className="quiet" onClick={() => copyText(subscriptionOAuthSession.auth_url, t("common.copied"))}>
                                <Copy size={16} />{t("common.copy")}
                              </button>
                            </div>
                          </div>
                          <label className="grid-wide">
                            {t("supplier.callback")}
                            <input
                              value={subscriptionCallbackInput}
                              onChange={(event) => setSubscriptionCallbackInput(event.target.value)}
                              placeholder={t("supplier.callbackPlaceholder")}
                            />
                          </label>
                          <div className="wizard-actions grid-wide">
                            <button
                              className="quiet"
                              onClick={importSubscriptionOAuthFromClipboard}
                              disabled={subscriptionWizardBusy}
                            >
                              <ClipboardPaste size={16} />{t("supplier.clipboardImport")}
                            </button>
                            <button
                              className="start"
                              onClick={exchangeSubscriptionOAuth}
                              disabled={
                                subscriptionWizardBusy
                                || (!subscriptionImportCanRetry && !subscriptionCallbackInput.trim())
                              }
                            >
                              {subscriptionWizardBusy
                                ? <LoaderCircle className="spin-icon" size={16} />
                                : <Plug size={16} />}
                              {subscriptionWizardBusy
                                ? t("supplier.checkingAndStarting")
                                : subscriptionImportCanRetry ? t("supplier.retryImport") : t("supplier.completeImport")}
                            </button>
                          </div>
                        </>
                      )}

                      <div className="manual-import-toggle grid-wide">
                        <button className="quiet" onClick={() => setSubscriptionManualOpen((value) => !value)}>
                          {subscriptionManualOpen ? <EyeOff size={16} /> : <Eye size={16} />}
                          {subscriptionManualOpen ? t("supplier.manualImportHide") : t("supplier.manualImportShow")}
                        </button>
                      </div>

                      {subscriptionManualOpen && (
                        <>
                          <label className="grid-wide">
                            token JSON
                            <textarea
                              value={subscriptionTokenJson}
                              onChange={(event) => setSubscriptionTokenJson(event.target.value)}
                              placeholder='{"access_token":"...","refresh_token":"...","email":"..."}'
                            />
                            <span className="field-hint">{t("supplier.manualImportHint")}</span>
                          </label>
                          <div className="wizard-actions grid-wide">
                            <button className="primary" onClick={importSubscriptionJson} disabled={subscriptionWizardBusy || !subscriptionTokenJson.trim()}>
                              <Save size={16} />{t("supplier.saveImportJson")}
                            </button>
                          </div>
                        </>
                      )}
                      {subscriptionWizardProgress && (
                        <div
                          className={`wizard-operation-status ${subscriptionWizardProgress.tone}`}
                          role="status"
                        >
                          {subscriptionWizardProgress.message}
                        </div>
                      )}
                    </div>
                  </div>
                </DialogBackdrop>
              )}

              {supplierDetailOpen && (
                <DialogBackdrop aria-labelledby="supplier-detail-title" variant="drawer">
                  <div className="detail-drawer">
                    <div className="drawer-header">
                      <div>
                        <h2 id="supplier-detail-title">{sourceDriverDisplayName(
                          selectedSourceDriver,
                          selectedChannel.name,
                          selectedChannel.id,
                        )}</h2>
                        <p>
                          {selectedChannel.source_driver === "lan_share"
                            ? t("lanShare.channelDescription")
                            : <>
                              {selectedChannelKindLabel} · {healthStatusLabel(selectedChannelHealth, selectedChannel, supplierStatus.running)}
                              {selectedChannelRouteLabel ? ` · ${selectedChannelRouteLabel}` : ""}
                            </>}
                        </p>
                      </div>
                      <ModalCloseButton onClick={() => void closeSupplierDetail()} />
                    </div>

                    <div className="drawer-summary">
                      <div>
                        <span>{t("supplier.defaultModel")}</span>
                        <strong>{modelDisplayName(selectedChannelModel) || "-"}</strong>
                      </div>
                      <div>
                        <span>{t("supplier.modelCount")}</span>
                        <strong>{supplierChannelModelCount(selectedChannel, selectedChannelHealth, selectedChannelRouteStatus) || "-"}</strong>
                        <button
                          className="drawer-summary-detail-link"
                          type="button"
                          onClick={() => setSupplierModelPoolOpen(true)}
                        >
                          {t("supplier.modelPool.viewDetails")}
                        </button>
                      </div>
                      <div>
                        <span>{t("supplier.protocolLatency")}</span>
                        <strong>{selectedChannelProtocolLatency.primary}</strong>
                        <small>{selectedChannelProtocolLatency.secondary}</small>
                      </div>
                      <div>
                        <span>{t("supplier.credential")}</span>
                        <strong>{credentialStatusLabel(selectedChannelHealth?.credential_status, selectedChannel)}</strong>
                      </div>
                      <div>
                        <span>{t("supplier.quota")}</span>
                        <strong>{quotaStatusLabel(selectedChannelQuotaHealth?.quota_status, selectedChannel)}</strong>
                        <small>{quotaDetailLabel(selectedChannelQuotaHealth, selectedChannel)}</small>
                      </div>
                      <div>
                        <span>{t("supplier.safety")}</span>
                        <strong>{safetyStatusLabel(selectedChannelHealth, selectedChannel)}</strong>
                        <small>{safetyDetailLabel(selectedChannelHealth, selectedChannel)}</small>
                      </div>
                    </div>

                    {supplierRouteStatusBlocksReady(selectedChannelRouteStatus) && selectedChannelRouteDetail && (
                      <div className="supplier-route-diagnostic-notice" role="status">
                        <strong>{t("supplierRoute.routeDetailTitle")}</strong>
                        <span>{selectedChannelRouteDetail}</span>
                      </div>
                    )}

                    {supplierPricingWarnings(selectedChannelRouteStatus).length > 0 && (
                      <div className="pricing-fallback-notice" role="status">
                        <strong>{t("supplier.partialBillingFallback")}</strong>
                        <span>{supplierPricingFallbackSummary(selectedChannelRouteStatus)}</span>
                      </div>
                    )}

                    {selectedChannel.source_driver !== "lan_share" ? (
                      <QuotaVisual
                        health={selectedChannelQuotaHealth}
                        channel={selectedChannel}
                        formatCountdown={formatUnixCountdown}
                      />
                    ) : null}

                    {selectedChannel.source_driver === "lan_share" ? (
                      <LanShareChannelQuota
                        channelId={selectedChannel.id}
                        enabled={selectedChannel.enabled}
                      />
                    ) : null}

                    <div className="drawer-section supplier-basic-section">
                      <div className="section-title-row compact">
                        <div>
                          <h2>{t("supplier.basicConfiguration")}</h2>
                          <p className="section-hint">{t("supplier.basicConfigurationHint")}</p>
                        </div>
                        <div className="actions channel-actions">
                          {selectedChannelHasChanges && (
                            <>
                              <button
                                className="primary"
                                disabled={selectedChannelBusy}
                                onClick={() => saveSupplierChanges(selectedChannel)}
                              >
                                {selectedChannelOperation === "saving"
                                  ? <LoaderCircle className="spin-icon" size={16} />
                                  : supplierChannelNeedsDetection(selectedChannel) ? <RotateCw size={16} /> : <Save size={16} />}
                                {selectedChannelOperation === "saving"
                                  ? t("common.saving")
                                  : supplierChannelNeedsDetection(selectedChannel) ? t("supplier.validateAndSave") : t("common.save")}
                              </button>
                              <button className="quiet" disabled={selectedChannelBusy} onClick={cancelChanges}>
                                <Undo2 size={16} />{t("common.cancel")}
                              </button>
                            </>
                          )}
                          <button
                            type="button"
                            className="quiet"
                            onClick={() => void refreshSupplierModels()}
                            disabled={selectedChannelHasChanges || selectedChannelBusy || !selectedChannelCanDetect}
                          >
                            <RefreshCw
                              className={selectedChannelOperation === "refreshing" ? "spin-icon" : undefined}
                              size={16}
                            />
                            {selectedChannelOperation === "refreshing" ? t("common.refreshing") : t("supplier.refreshChannel")}
                          </button>
                          <button
                            type="button"
                            className={selectedChannelSwitchOn ? "stop" : "start"}
                            title={selectedChannelHasChanges ? t("supplier.unsavedChanges") : ""}
                            disabled={
                              selectedChannelHasChanges
                              || selectedChannelBusy
                              || (!selectedChannelSwitchOn && !selectedChannelCanSupply)
                            }
                            onClick={() => void (selectedChannelSwitchOn
                              ? stopSelectedChannel(selectedChannel)
                              : startSupplier(selectedChannel))}
                          >
                            {selectedChannelOperation === "enabling" || selectedChannelOperation === "disabling"
                              ? <LoaderCircle className="spin-icon" size={16} />
                              : selectedChannelSwitchOn ? <Square size={16} /> : <Play size={16} />}
                            {selectedChannelOperation === "enabling"
                              ? t("supplier.enabling")
                              : selectedChannelOperation === "disabling"
                                ? t("supplier.stopping")
                                : selectedChannelSwitchOn
                                  ? t("supplier.disableChannel")
                                  : selectedChannel.source_driver === "lan_share"
                                    ? t("lanShare.enableChannel")
                                    : t("supplier.enableAndStart")}
                          </button>
                        </div>
                      </div>
                      <div className="supplier-lifecycle-status" role="status" aria-label={t("supplier.statusAria")}>
                        <span className={selectedChannelHasChanges ? "pending" : "muted"}>
                          {selectedChannelLifecycle.configLabel}
                        </span>
                        <span className={selectedChannelLifecycle.localTone}>
                          {selectedChannelLifecycle.localLabel}
                        </span>
                        {selectedChannel.source_driver !== "lan_share" ? (
                          <span className={selectedChannelLifecycle.platformTone}>
                            {selectedChannelLifecycle.platformLabel}
                          </span>
                        ) : null}
                      </div>
                      <div className="grid">
                        <label>
                          {t("supplier.channelName")}
                          <input
                            value={sourceDriverDisplayName(
                              selectedSourceDriver,
                              selectedChannel.name,
                              selectedChannel.id,
                            )}
                            onChange={(e) => updateSupplier({ name: e.target.value })}
                          />
                        </label>
                        <label>
                          {selectedChannel.kind === "custom_endpoint"
                            ? t("supplier.sourceAndDefaultTarget")
                            : t("supplier.sourceAndProtocol")}
                          <input
                            value={selectedSourceDriver.fixedConnection
                              ? `${sourceDriverTitle(selectedSourceDriver)} / ${selectedSourceDriver.supportedProtocols.length > 1
                                ? t("supplier.managedModelProtocol") : protocolLabel(selectedApiFormat)}`
                              : `${channelKindLabel(selectedChannel.kind)} / ${protocolLabel(selectedApiFormat)}`}
                            disabled
                            readOnly
                          />
                          <span className="field-hint">
                            {selectedSourceDriver.fixedConnection
                              ? t("supplier.fixedConnectionHint")
                              : selectedChannel.kind === "custom_endpoint"
                              ? t("supplier.sourceAndDefaultTargetHint")
                              : t("supplier.sourceAndProtocolHint")}
                          </span>
                        </label>
                        {selectedChannelFields.showBaseUrl && selectedChannel.kind !== "custom_endpoint" && (
                        <label>
                          {selectedChannel.source_driver === "lan_share" ? "Base URL" : t("supplier.upstreamBaseUrl")}
                          <input
                            value={selectedChannel.upstream_base_url}
                            readOnly={selectedChannelFields.baseUrlReadOnly}
                            onChange={(e) => updateSupplier({ upstream_base_url: e.target.value })}
                          />
                          {selectedChannelFields.baseUrlReadOnly && (
                            <span className="field-hint">{t("supplier.fixedBaseUrlHint")}</span>
                          )}
                        </label>
                        )}
                        {selectedChannelFields.showApiKey && (
                          <div className={`supplier-credential-field${selectedChannel.kind === "custom_endpoint" ? " shared grid-wide" : ""}`}>
                            <label>
                              {selectedChannel.source_driver === "lan_share" ? "API Key" : t("supplier.upstreamApiKey")}
                              <input
                                value={selectedChannel.upstream_api_key}
                                aria-describedby={selectedChannel.kind === "custom_endpoint" ? "supplier-shared-credential-hint" : undefined}
                                onChange={(e) => updateSupplier({ upstream_api_key: e.target.value })}
                              />
                              {selectedChannel.kind === "azure_openai" && (
                                <span className="field-hint">{t("supplier.azureAuthHint")}</span>
                              )}
                            </label>
                            {selectedChannel.kind === "custom_endpoint" && (
                              <span id="supplier-shared-credential-hint" className="field-hint supplier-credential-note">
                                {t("supplier.apiInterfaces.sharedCredentialHint")}
                              </span>
                            )}
                          </div>
                        )}
                        {selectedSourceDriver.homepageUrl && (
                          <div className="supplier-source-help">
                            <span>{t("supplier.sourceHelp", { source: sourceDriverTitle(selectedSourceDriver) })}</span>
                            <button
                              type="button"
                              onClick={() => void openExternalUrl(selectedSourceDriver.homepageUrl!)}
                            >
                              <ExternalLink size={14} />
                              {t("supplier.projectHomepage")}
                            </button>
                          </div>
                        )}
                        {selectedChannel.kind === "custom_endpoint" && (
                          <div className="grid-wide">
                            <SupplierSurfaceConfig
                              mode="custom-edit"
                              bindings={selectedSurfaceBindings}
                              rows={selectedSurfaceEditorRows}
                              defaultTarget={selectedChannel.default_target}
                              placeholderBaseUrl={selectedChannel.upstream_base_url}
                              onUpdateSurface={updateSupplierSurfaceBinding}
                              onToggleProtocol={toggleSupplierSurfaceProtocol}
                              onSelectDefaultTarget={selectCustomSupplierDefaultProtocol}
                            />
                          </div>
                        )}
                        <label>
                          {t("supplier.defaultModel")}
                          <div>
                            <ModelInput
                              value={selectedChannel.default_model}
                              models={selectedModelOptions}
                              onChange={(value) => updateSupplier({ default_model: value, upstream_model: value, public_model: value })}
                            />
                            <span className="field-hint">{t("supplier.defaultModelHint")}</span>
                            {selectedChannel.kind === "aws_bedrock" && (
                              <span className="field-hint">{t("supplier.bedrockModelHint")}</span>
                            )}
                          </div>
                        </label>
                        {selectedChannel.source_driver !== "lan_share" ? (
                          <>
                            {false && (<label>
                              {t("supplier.priceRatio")}
                              <div className="supplier-market-control-row">
                                <PriceRatioInput
                                  key={selectedChannel.id}
                                  max={MAX_PRICE_RATIO}
                                  value={selectedChannel.price_ratio}
                                  onValueChange={(priceRatio) => updateSupplier({ price_ratio: priceRatio })}
                                />
                                <button
                                  type="button"
                                  className="quiet supplier-market-link"
                                  onClick={() => setMarketPricesContext({
                                    initialView: "offers",
                                    channel: {
                                      channelId: selectedChannel.id,
                                      channelName: selectedChannel.name,
                                    },
                                  })}
                                >
                                  <CircleDollarSign size={14} />
                                  {t("market.supplierAction")}
                                </button>
                              </div>
                              <span className="field-hint">{t("supplier.priceRatioHintDetailed", { max: MAX_PRICE_RATIO })}</span>
                            </label>)}
                            <label>
                              {t("supplier.advanced.maxConcurrency")}
                              <input
                                type="number"
                                min={0}
                                title={t("supplier.advanced.unlimitedHint")}
                                value={selectedChannel.max_concurrency}
                                onChange={(e) => updateSupplier({ max_concurrency: Number(e.target.value) })}
                              />
                              <span className={`field-hint${selectedConcurrencyGuidance.tone === "warning" ? " field-hint-warning" : ""}`}>
                                {t(selectedConcurrencyGuidance.key)}
                              </span>
                            </label>
                            <label>
                              {t("supplier.advanced.quotaReservePercent")}
                              <input
                                type="number"
                                min={0}
                                max={100}
                                step={1}
                                title={t("supplier.advanced.quotaReservePercentHint")}
                                value={selectedChannel.quota_reserve_percent}
                                onChange={(e) => updateSupplier({
                                  quota_reserve_percent: Math.min(100, Math.max(0, Number(e.target.value) || 0)),
                                })}
                              />
                              <span className="field-hint">{t("supplier.advanced.quotaReservePercentHint")}</span>
                            </label>
                          </>
                        ) : (
                          <div className="lan-share-local-only-note">
                            <Network size={15} />
                            <span>{t("lanShare.localOnlyChannel")}</span>
                          </div>
                        )}
                      </div>
                    </div>

                    <SupplierChannelChecks
                      key={selectedChannel.id}
                      checks={selectedChannel.detection_evidence ?? []}
                      groups={supplierStatus.availability_groups?.[selectedChannel.id]}
                      busy={selectedChannelBusy}
                      canSupply={selectedChannelCanSupply}
                      saving={selectedChannelOperation === "saving"}
                      testing={selectedChannelOperation === "testing"}
                      model={modelDisplayName(selectedChannelModel)}
                      prompt={supplierTestPrompt}
                      onPromptChange={setSupplierTestPrompt}
                      onFullCheck={() => saveSupplierChanges(selectedChannel, true)}
                      onTest={() => void testSupplierUpstream()}
                    >
                      {supplierTestResult.status !== "idle" && (
                        <div className={`test-result ${supplierTestResult.status}`}>
                          <div className="test-meta">
                            <span>{t("common.status")}: {supplierTestResult.status === "running" ? t("supplier.testRunning") : supplierTestResult.status === "success" ? t("supplier.testComplete") : t("common.failed")}</span>
                            <span>HTTP: {supplierTestResult.httpStatus ?? "-"}</span>
                            <span>{t("supplier.protocolLatency")}: {supplierProtocolLatencyLabel({
                              upstream_http_version: supplierTestResult.upstreamHttpVersion || "",
                              latency_ms: supplierTestResult.latencyMs ?? 0,
                            })}</span>
                            <span>{t("supplier.model")}: {modelDisplayName(supplierTestResult.model || selectedChannelModel) || "-"}</span>
                            <span>{t("supplier.input")}: {supplierTestResult.inputTokens ?? "-"}</span>
                            <span>{t("supplier.output")}: {supplierTestResult.outputTokens ?? "-"}</span>
                          </div>
                          <pre>
                            {supplierTestResult.status === "running"
                              ? t("supplier.testAwaitingResponse")
                              : supplierTestResult.error || supplierTestResult.content || t("supplier.testEmpty")}
                          </pre>
                        </div>
                      )}
                    </SupplierChannelChecks>

                    <div className="actions secondary-actions supplier-more-actions">
                      <button
                        className="quiet advanced-panel-toggle"
                        type="button"
                        aria-expanded={showSupplierAdvanced}
                        onClick={() => setShowSupplierAdvanced((value) => !value)}
                      >
                        {showSupplierAdvanced ? <EyeOff size={16} /> : <Eye size={16} />}
                        {showSupplierAdvanced ? t("supplier.hideAdvanced") : t("supplier.showAdvanced")}
                      </button>
                    </div>

                    {showSupplierAdvanced && (
                      <div className="advanced-panel supplier-debug drawer-section">
                        <h2>{t("supplier.showAdvanced")}</h2>
                        <p className="section-hint">{t("supplier.advancedHint")}</p>
                        <div className={`advanced-disclosure ${showSupplierSurfaceBindings ? "open" : ""}`}>
                          <button
                            className="advanced-disclosure-toggle"
                            type="button"
                            aria-expanded={showSupplierSurfaceBindings}
                            aria-label={showSupplierSurfaceBindings
                              ? t("supplier.advanced.collapse")
                              : t("supplier.advanced.expand")}
                            onClick={() => setShowSupplierSurfaceBindings((value) => !value)}
                          >
                            <span>
                              <strong>{t("supplier.advanced.surfaceTitle")}</strong>
                              <small>{selectedSurfaceBindings.map((binding) => surfaceLabel(binding.surface)).join(" / ") || t("supplier.advanced.notConfigured")}</small>
                            </span>
                            <ChevronDown className="advanced-disclosure-chevron" aria-hidden="true" size={17} />
                          </button>
                          {showSupplierSurfaceBindings && (
                            <div className="advanced-disclosure-body surface-binding-list">
                              {selectedChannel.source_driver !== "custom_endpoint" ? (
                                <SupplierSurfaceConfig
                                  mode="readonly"
                                  bindings={selectedSurfaceBindings}
                                />
                              ) : selectedSurfaceBindings.map((binding) => (
                                  <div className="surface-binding-row surface-binding-row-operations" key={binding.surface}>
                                    <div className="surface-binding-heading">
                                      <strong>{surfaceLabel(binding.surface)}</strong>
                                      <span>{binding.operation_overrides.length > 0
                                        ? t("supplier.advanced.operationCount", {
                                          count: binding.operation_overrides.length,
                                        })
                                        : t("supplier.advanced.defaultEndpoints")}</span>
                                    </div>
                                      <div className="surface-operation-editor">
                                        <div className="surface-operation-heading">
                                          <span>{t("supplier.advanced.operationOverrides")}</span>
                                          <button
                                            className="icon-button compact"
                                            title={t("supplier.advanced.addOperation")}
                                            onClick={() => addSupplierOperationOverride(binding.surface)}
                                          >
                                            <Plus size={15} />
                                          </button>
                                        </div>
                                        {binding.operation_overrides.map((endpoint, index) => (
                                          <div className="surface-operation-row" key={`${endpoint.operation}-${index}`}>
                                            <select
                                              aria-label={t("supplier.advanced.operation")}
                                              value={endpoint.operation}
                                              onChange={(event) => {
                                                const option = surfaceOperationOptions(binding.surface)
                                                  .find((item) => item.operation === event.target.value);
                                                updateSupplierOperationOverride(binding.surface, index, {
                                                  operation: event.target.value,
                                                  method: option?.method ?? endpoint.method,
                                                  url: defaultSurfaceOperationUrl(
                                                    binding.base_url,
                                                    binding.surface,
                                                    event.target.value,
                                                  ),
                                                });
                                              }}
                                            >
                                              {surfaceOperationOptions(binding.surface).map((option) => (
                                                <option key={option.operation} value={option.operation}>
                                                  {option.label}
                                                </option>
                                              ))}
                                            </select>
                                            <select
                                              aria-label={t("supplier.advanced.httpMethod")}
                                              value={endpoint.method.toUpperCase()}
                                              onChange={(event) => updateSupplierOperationOverride(binding.surface, index, { method: event.target.value })}
                                            >
                                              {['GET', 'POST', 'PUT', 'PATCH', 'DELETE'].map((method) => (
                                                <option key={method} value={method}>{method}</option>
                                              ))}
                                            </select>
                                            <input
                                              aria-label={t("supplier.advanced.fullUrl")}
                                              value={endpoint.url}
                                              placeholder="https://api.example.com/path"
                                              onChange={(event) => updateSupplierOperationOverride(binding.surface, index, { url: event.target.value })}
                                            />
                                            <button
                                              className="icon-button compact"
                                              title={t("supplier.advanced.removeOperation")}
                                              onClick={() => removeSupplierOperationOverride(binding.surface, index)}
                                            >
                                              <Trash2 size={15} />
                                            </button>
                                          </div>
                                        ))}
                                        <small>{t("supplier.advanced.operationHint")}</small>
                                      </div>
                                  </div>
                                ))}
                            </div>
                          )}
                        </div>
                        <div className={`advanced-disclosure ${showSupplierProtocolDeclaration ? "open" : ""}`}>
                          <button
                            className="advanced-disclosure-toggle"
                            type="button"
                            aria-expanded={showSupplierProtocolDeclaration}
                            aria-label={showSupplierProtocolDeclaration
                              ? t("supplier.advanced.collapse")
                              : t("supplier.advanced.expand")}
                            onClick={() => setShowSupplierProtocolDeclaration((value) => !value)}
                          >
                            <span>
                              <strong>{t("supplier.advanced.capabilityEvidence")}</strong>
                              <small>{t("supplier.advanced.capabilityEvidenceHint")}</small>
                            </span>
                            <ChevronDown className="advanced-disclosure-chevron" aria-hidden="true" size={17} />
                          </button>
                          {showSupplierProtocolDeclaration && (
                            <div className="advanced-disclosure-body capability-overview">
                              <CapabilityProfileList profiles={selectedChannel.capability_profiles ?? []} />
                              <React.Suspense fallback={null}>
                                <OperationCapabilityMatrix driver={selectedSourceDriver} />
                              </React.Suspense>
                            </div>
                          )}
                        </div>
                        <div className="grid">
                          <label>
                            {t("supplier.advanced.internalSourceType")}
                            <input
                              value={channelKindLabel(selectedChannel.kind)}
                              aria-label={channelKindLabel(selectedChannel.kind)}
                              disabled
                              readOnly
                            />
                            <span className="field-hint">{t("supplier.advanced.internalSourceTypeHint")}</span>
                          </label>
                          <label>
                            {t("supplier.advanced.publicModel")}
                            <input value={selectedChannel.upstream_model} disabled readOnly />
                            <span className="field-hint">{t("supplier.advanced.publicModelHint")}</span>
                          </label>
                          {!selectedSourceDriver.fixedConnection && selectedChannel.kind !== "custom_endpoint" && selectedChannel.kind !== "subscription_adapter" && (<label>
                            {t("supplier.advanced.primaryUpstreamProtocol")}
                            <select
                              value={selectedApiFormat}
                              disabled={selectedPrimaryProtocolOptions.length <= 1}
                              onChange={(e) => {
                                selectCustomSupplierDefaultProtocol(e.target.value as DebugProtocol);
                              }}
                            >
                              {selectedPrimaryProtocolOptions.map((option) => (
                                <option key={option.value} value={option.value}>{option.label}</option>
                              ))}
                            </select>
                            <span className="field-hint">{t("supplier.advanced.primaryUpstreamProtocolHint")}</span>
                          </label>)}
                          {selectedSourceDriver.category !== "subscription" && (
                            <label>
                              {t("supplier.advanced.userAgentProfile")}
                              <select
                                value={selectedChannel.user_agent_profile ?? ""}
                                onChange={(event) => updateSupplier({ user_agent_profile: event.target.value })}
                              >
                                <option value="">{t("supplier.advanced.userAgentProfileDefault")}</option>
                                <option value="claude_code">Claude Code</option>
                                <option value="codex">Codex</option>
                                <option value="opencode">OpenCode</option>
                              </select>
                              <span className="field-hint">{t("supplier.advanced.userAgentProfileHint")}</span>
                            </label>
                          )}
                          {selectedChannel.kind === "subscription_adapter" && (
                            <>
                              <label className="supplier-subscription-reference">
                                {t("supplier.advanced.subscriptionPlatform")}
                                <div className="readonly-field-value">
                                  {selectedChannel.subscription.platform === "claude"
                                    ? "Claude"
                                    : selectedChannel.subscription.platform === "grok"
                                      ? "Grok"
                                    : selectedChannel.subscription.platform === "antigravity"
                                      ? "Antigravity"
                                      : selectedChannel.subscription.platform === "gemini"
                                        ? t("supplier.advanced.legacyGemini")
                                        : "OpenAI"}
                                </div>
                              </label>
                              <div
                                className="readonly-copy-field readonly-copy-field-singleline supplier-subscription-reference"
                                title={selectedChannel.subscription.credential_ref
                                  ? t("supplier.advanced.credentialReferenceHint", {
                                    path: selectedChannel.subscription.credential_ref,
                                  })
                                  : t("common.notSet")}
                              >
                                <span>{t("supplier.advanced.credentialReference")}</span>
                                <div className="readonly-copy-value">
                                  <span title={selectedChannel.subscription.credential_ref}>
                                    {selectedChannel.subscription.credential_ref || t("common.notSet")}
                                  </span>
                                  <button
                                    type="button"
                                    className={`copy-feedback-button${copiedField === "supplier-credential-reference" ? " copied" : ""}`}
                                    title={copiedField === "supplier-credential-reference"
                                      ? t("common.copied")
                                      : t("supplier.advanced.copyCredentialReference")}
                                    aria-label={copiedField === "supplier-credential-reference"
                                      ? t("common.copied")
                                      : t("supplier.advanced.copyCredentialReference")}
                                    onClick={() => void copyValue(
                                      "supplier-credential-reference",
                                      t("supplier.advanced.credentialReference"),
                                      selectedChannel.subscription.credential_ref,
                                    )}
                                  >
                                    {copiedField === "supplier-credential-reference"
                                      ? <Check size={15} />
                                      : <Copy size={15} />}
                                  </button>
                                </div>
                              </div>
                            </>
                          )}
                        </div>
                        {channels.length > 1 && (
                          <div className="supplier-danger-zone">
                            <div>
                              <strong>{t("supplier.deleteChannel")}</strong>
                              <span>{t("supplier.deleteChannelHint")}</span>
                            </div>
                            <button
                              type="button"
                              className="danger"
                              disabled={selectedChannelBusy}
                              onClick={() => removeSelectedChannel(selectedChannel.id)}
                            >
                              {selectedChannelOperation === "deleting"
                                ? <LoaderCircle className="spin-icon" size={16} />
                                : <Trash2 size={16} />}
                              {selectedChannelOperation === "deleting" ? t("supplier.deleteChannelProgress") : t("supplier.deleteChannel")}
                            </button>
                          </div>
                        )}
                      </div>
                    )}
                  </div>
                </DialogBackdrop>
              )}

              {supplierModelPoolOpen && supplierDetailOpen && (
                <React.Suspense fallback={null}><SupplierModelPoolDialog
                  channelName={sourceDriverDisplayName(
                    selectedSourceDriver,
                    selectedChannel.name,
                    selectedChannel.id,
                  )}
                  observedModels={selectedChannel.models.length > 0
                    ? selectedChannel.models
                    : [selectedChannelModel].filter(Boolean)}
                  routeStatus={selectedChannelRouteStatus}
                  platformRegistered={selectedChannelTransport?.platform_registered === true}
                  onClose={() => setSupplierModelPoolOpen(false)}
                  onCopy={copyText}
                /></React.Suspense>
              )}
            </>
          ) : tab === "account" ? (
            <React.Suspense fallback={<div className="boot-state">{t("common.loading")}</div>}>
              <AccountPage
                controller={account}
                notify={showToast}
                requestedView={requestedAccountView}
                onRequestedViewHandled={() => setRequestedAccountView(null)}
                showDevelopmentTools={clientModePolicy.presentation.showDevelopmentTools}
              />
            </React.Suspense>
          ) : (
            <>
              <header className="page-header-with-actions">
                <h1>{t("about.title")}</h1>
                <p>{t("about.subtitle")}</p>
                <div className="actions page-header-actions about-legal-links" aria-label={t("about.legalFiles")}>
                  <button className="quiet about-legal-link" type="button" onClick={() => setLegalDocumentOpen("agreement")}>
                    {t("about.agreement")}
                  </button>
                  <button className="quiet about-legal-link" type="button" onClick={() => setLegalDocumentOpen("privacy")}>
                    {t("about.privacy")}
                  </button>
                </div>
              </header>

              <div className="about-panel">
                <div className="about-section">
                  <div className="section-title-row compact-title-row">
                    <div>
                      <h2>{t("about.appStatusAndSettings")}</h2>
                      <p className="section-hint">{t("about.appStatusAndSettingsHint")}</p>
                    </div>
                    <div className="actions">
                      <button className="quiet" onClick={() => setLogOpen(true)}>
                        <Terminal size={16} />{t("about.runtimeLog")}
                      </button>
                      {clientModePolicy.capabilities.releaseUpdates && (updateState.status === "ready" || updateState.status === "ready_waiting_idle" || updateState.status === "installing" ? (
                        <button
                          className="start"
                          onClick={installPreparedUpdate}
                          disabled={updateState.status === "ready_waiting_idle" || updateState.status === "installing"}
                        >
                          <RotateCw size={16} />{
                            updateState.status === "installing"
                              ? t("about.updating")
                              : updateState.status === "ready_waiting_idle"
                                ? t("about.waitingIdle")
                                : t("about.restartUpdate")
                          }
                        </button>
                      ) : (
                        <button
                          className="quiet"
                          onClick={() => checkAndDownloadUpdate(false)}
                          disabled={updateState.status === "checking" || updateState.status === "downloading"}
                        >
                          <Download size={16} />
                          {updateState.status === "checking"
                            ? t("about.checking")
                            : updateState.status === "downloading"
                              ? t("about.downloading")
                              : t("about.checkUpdate")}
                        </button>
                      ))}
                    </div>
                  </div>
                  <div className={`about-info-sheet ${updateReady ? "highlight" : ""}`}>
                    {clientModePolicy.preview.available && (
                      <div className={`about-info-row about-presentation-preview ${clientModePolicy.preview.active ? "active" : ""}`}>
                        <span className="about-info-label">{t("about.productionPresentationPreview")}</span>
                        <div className="about-info-value">
                          <strong id="production-presentation-preview-hint">
                            {t("about.productionPresentationPreviewHint")}
                          </strong>
                        </div>
                        <label className={`supply-toggle ${clientModePolicy.preview.active ? "checked" : ""}`}>
                          <input
                            type="checkbox"
                            role="switch"
                            checked={clientModePolicy.preview.active}
                            aria-describedby="production-presentation-preview-hint"
                            onChange={(event) => setProductionPresentationPreview(event.target.checked)}
                          />
                          <span className="supply-toggle-track" aria-hidden="true"><span /></span>
                          <span>{clientModePolicy.preview.active
                            ? t("about.productionPresentationPreviewEnabled")
                            : t("about.productionPresentationPreviewDisabled")}</span>
                        </label>
                      </div>
                    )}
                    <div className="about-info-row">
                      <span className="about-info-label">{t("about.clientVersion")}</span>
                      <div className="about-info-value">
                        <strong>v{appMeta.currentVersion}{clientModePolicy.presentation.showDevelopmentBranding ? " · Dev" : ""}</strong>
                        {clientModePolicy.capabilities.releaseUpdates && (
                          <>
                            <span className="about-info-status">{aboutUpdateHeadline}</span>
                            {aboutUpdateDetail && <small>{aboutUpdateDetail}</small>}
                          </>
                        )}
                      </div>
                      {clientModePolicy.capabilities.releaseUpdates && (
                        <div className="about-info-source">
                          <span>{t("about.updateSource")}</span>
                          <strong>{updateSourceLabel(appMeta)}</strong>
                          {updateSourceNote(appMeta) && (
                            <small title={appMeta.updateLastError}>{updateSourceNote(appMeta)}</small>
                          )}
                        </div>
                      )}
                    </div>
                    {false && (<div className="about-info-row">
                      <span className="about-info-label">{t("about.platformEndpoint")}</span>
                      <div className="about-info-value">
                        <strong>{aboutServerLabel(status, config)}</strong>
                        <small>{supplierNetworkState(supplierStatus)} · {endpointNetworkState(status)}</small>
                      </div>
                      <div className="about-info-source">
                        <span>{t("about.endpointSource")}</span>
                        <strong>{endpointRegistrySourceLabel(appMeta, config)}</strong>
                        {aboutEndpointSourceNote && (
                          <small title={appMeta.endpointLastError || appMeta.endpointRefreshWarning}>
                            {aboutEndpointSourceNote}
                          </small>
                        )}
                      </div>
                    </div>)}
                    {clientModePolicy.capabilities.developmentEndpoint
                      && clientModePolicy.presentation.showDevelopmentTools && (
                      <div className="about-info-row about-development-endpoint-row">
                        <span className="about-info-label">{t("about.developmentEndpoint")}</span>
                        <div className="about-info-value">
                          <select
                            className="about-development-endpoint-select"
                            value={config.development_endpoint || DEFAULT_DEVELOPMENT_ENDPOINT}
                            disabled={developmentEndpointBusy}
                            onChange={(event) => void changeDevelopmentEndpoint(event.target.value)}
                          >
                            <option value="auto">{t("about.developmentEndpointAuto")}</option>
                            {config.development_endpoint !== "auto"
                              && !config.endpoints.some((endpoint) => endpoint.name === config.development_endpoint) && (
                                <option value={config.development_endpoint}>
                                  {t("about.developmentEndpointUnavailable", {
                                    endpoint: config.development_endpoint,
                                  })}
                                </option>
                              )}
                            {config.endpoints.map((endpoint) => (
                              <option key={endpoint.name} value={endpoint.name}>
                                {endpoint.name === "local-dev"
                                  ? t("about.developmentEndpointLocal", { url: endpoint.base_url })
                                  : `${endpoint.name} (${endpoint.base_url})`}
                              </option>
                            ))}
                          </select>
                          <small>{developmentEndpointBusy
                            ? t("about.developmentEndpointChanging")
                            : t("about.developmentEndpointHint")}</small>
                        </div>
                      </div>
                    )}
                    <div
                      className="about-info-row"
                      title={modelCatalogVersionDetails(appMeta.modelCatalog)}
                    >
                      <span className="about-info-label">{t("about.catalogVersion")}</span>
                      <div className="about-info-value">
                        <strong>{modelCatalogVersionLabel(appMeta.modelCatalog)}</strong>
                        <small>{modelCatalogVersionHint(appMeta)}</small>
                      </div>
                      <div className="about-info-source">
                        <span>{t("about.catalogSource")}</span>
                        <strong>{modelCatalogSourceLabel(appMeta.modelCatalog)}</strong>
                        {aboutModelCatalogSourceNote && (
                          <small title={appMeta.modelCatalogLastError}>
                            {aboutModelCatalogSourceNote}
                          </small>
                        )}
                      </div>
                    </div>
                  </div>
                  <div className="about-preference-bar">
                    {clientModePolicy.presentation.showReleasePreferences && (
                      <div className="about-preference-toggles">
                        <span className="about-info-label">{t("about.startupAndUpdates")}</span>
                        <div className="about-preference-options">
                          {false && (<label className="about-compact-toggle">
                            <input
                              type="checkbox"
                              checked={releasePreferencesPreviewOnly
                                ? previewAutoInstallUpdates
                                : autoInstallUpdates}
                              onChange={(event) => {
                                if (releasePreferencesPreviewOnly) {
                                  setPreviewAutoInstallUpdates(event.target.checked);
                                } else {
                                  setAutoInstallUpdates(event.target.checked);
                                }
                              }}
                            />
                            <span>{t("about.autoRestartUpdate")}</span>
                          </label>)}
                          <label className="about-compact-toggle">
                            <input
                              type="checkbox"
                              checked={releasePreferencesPreviewOnly
                                ? previewAutostartEnabled
                                : autostartEnabled}
                              disabled={!releasePreferencesPreviewOnly && autostartBusy}
                              onChange={(event) => {
                                if (releasePreferencesPreviewOnly) {
                                  setPreviewAutostartEnabled(event.target.checked);
                                } else {
                                  void changeAutostart(event.target.checked);
                                }
                              }}
                            />
                            <span>{!releasePreferencesPreviewOnly && autostartBusy
                              ? t("about.changing")
                              : t("about.launchAtLogin")}</span>
                          </label>
                        </div>
                      </div>
                    )}
                    <div className="about-appearance-controls">
                      <span className="about-info-label">{t("about.interfaceSettings")}</span>
                      <div className="about-appearance-options">
                        <div className="about-theme-choice">
                          <div className="theme-segmented" role="group" aria-label={t("about.themeAria")}>
                            {THEME_OPTIONS.map((option) => {
                              const Icon = option.icon;
                              return (
                                <button
                                  key={option.id}
                                  type="button"
                                  className={themePreference === option.id ? "active" : ""}
                                  aria-pressed={themePreference === option.id}
                                  onClick={() => setThemePreference(option.id)}
                                >
                                  <Icon size={15} aria-hidden="true" />
                                  {t(`about.${option.id}`)}
                                </button>
                              );
                            })}
                          </div>
                        </div>
                        <div className="about-theme-choice about-language-choice">
                          <div className="theme-segmented" role="group" aria-label={t("about.languageAria")}>
                            {(["en-US", "zh-CN"] as SupportedLanguage[]).map((language) => (
                              <button
                                key={language}
                                type="button"
                                className={activeLanguage === language ? "active" : ""}
                                aria-pressed={activeLanguage === language}
                                onClick={() => void changeAppLanguage(language)}
                              >
                                {t(`language.${language}`)}
                              </button>
                            ))}
                          </div>
                        </div>
                      </div>
                    </div>
                  </div>
                  <div className="about-reset-row">
                    <span className="about-info-label">{t("about.localData")}</span>
                    <div className="about-reset-copy">
                      <span>{t("about.resetLocalDataHint")}</span>
                      {resetLocalDataError ? (
                        <small className="error-text">{t("about.resetLocalDataFailed", { error: resetLocalDataError })}</small>
                      ) : null}
                    </div>
                    <button
                      className="danger"
                      type="button"
                      disabled={resetLocalDataBusy}
                      onClick={() => setResetLocalDataConfirmOpen(true)}
                    >
                      <Trash2 size={16} aria-hidden="true" />
                      {resetLocalDataBusy ? t("about.resettingLocalData") : t("about.resetLocalDataAction")}
                    </button>
                  </div>
                </div>
                <DiagnosticsPanel issues={activeDiagnostics} onAction={handleDiagnosticAction} />
              </div>
            </>
          )}
          {clientModePolicy.capabilities.debugConsole
            && clientModePolicy.presentation.showDebugConsole
            && debugConsoleOpen && (
            <DialogBackdrop aria-labelledby="debug-console-title" className="debug-modal-backdrop">
              <div className="modal-panel debug-modal-panel">
                <div className="debug-modal-header">
                  <div>
                    <h2 id="debug-console-title">{t("debugConsole.title")}</h2>
                    <p>{t("debugConsole.subtitle")}</p>
                  </div>
                  <ModalCloseButton onClick={() => setDebugConsoleOpen(false)} />
                </div>

                <div className="debug-modal-body">
                  <div className="debug-console">
                    <div className="debug-target-browser">
                      <div className="section-title-row compact">
                        <div>
                          <h2>{t("debugConsole.testTargets")}</h2>
                          <p className="section-hint">{t("debugConsole.testTargetsHint")}</p>
                          <span className={`debug-refresh-status ${debugEndpointError ? "error-text" : ""}`}>
                            {debugEndpointStatusText}{debugEndpointError ? `：${debugEndpointError}` : ""}
                          </span>
                        </div>
                        <button className="quiet" type="button" onClick={() => refreshAccount()} disabled={debugEndpointRefreshing}>
                        <RefreshCw size={16} />{debugEndpointRefreshing
                          ? t("common.refreshing")
                          : t("debugConsole.refreshEndpointData")}
                        </button>
                      </div>
                      <div className="debug-target-groups">
                        <div className="debug-target-header">
                        <span>{t("debugConsole.columns.target")}</span>
                        <span>{t("common.status")}</span>
                        <span>{t("supplier.model")}</span>
                        <span>{t("debugConsole.columns.execution")}</span>
                        </div>
                        {debugTargetGroups.map((group) => {
                          const expanded = expandedDebugTargetGroups.includes(group.type);
                          const shouldLimit = group.type !== "platform_auto" && !expanded && group.targets.length > 5;
                          const visibleTargets = shouldLimit ? group.targets.slice(0, 5) : group.targets;
                          return (
                            <div className="debug-target-group" key={group.type}>
                              <div className="debug-target-group-title">
                                <strong>{group.title}</strong>
                                {group.type !== "platform_auto" && group.targets.length > 5 ? (
                                  <button className="inline-text-button" type="button" onClick={() => toggleDebugTargetGroup(group.type)}>
                                  {expanded
                                    ? t("debugConsole.collapse")
                                    : t("debugConsole.showAll", { count: group.targets.length })}
                                  </button>
                                ) : null}
                              </div>
                              {group.targets.length === 0 ? (
                                <span className="empty-target">{debugEmptyTargetText(group.type, debugEndpointLastRefresh)}</span>
                              ) : (
                                visibleTargets.map((target) => (
                                  <button
                                    className={debugTargetType === group.type && effectiveDebugTargetId === target.id ? "debug-target-row selected" : "debug-target-row"}
                                    key={`${group.type}-${target.id || "auto"}`}
                                    type="button"
                                    onClick={() => selectDebugTarget(group.type, target)}
                                  >
                                    <span>
                                      <b>{target.name}</b>
                                      <small>{target.subtitle}</small>
                                    </span>
                                    <span>{target.status}</span>
                                    <span>{target.models.slice(0, 3).join(" / ") || "-"}</span>
                                  <span>{group.type === "platform_auto"
                                    ? t("debugConsole.execution.platform")
                                    : group.type === "local_channel"
                                      ? t("debugConsole.execution.local")
                                      : t("debugConsole.execution.supplier")}</span>
                                  </button>
                                ))
                              )}
                            </div>
                          );
                        })}
                      </div>
                    </div>

                    <div className="debug-current-target">
                    <span>{t("debugConsole.currentTarget")}</span>
                      <strong>{selectedDebugTarget?.name ?? "-"}</strong>
                    <small>{debugTargetTypeLabel(debugTargetType)} · {selectedDebugTarget?.status ?? "-"} · {t("debugConsole.executeSelectedProtocol")}</small>
                    </div>

                    <div className="debug-experiments" aria-labelledby="debug-experiments-title">
                      <div className="debug-section-title">
                        <h2 id="debug-experiments-title">{t("debugConsole.experimental.title")}</h2>
                        <p className="section-hint">{t("debugConsole.experimental.hint")}</p>
                      </div>
                      <div className="debug-experiment-row">
                        <div>
                          <strong>{t("debugConsole.experimental.claudeCompaction.title")}</strong>
                          <small id="debug-claude-compaction-hint">
                            {t("debugConsole.experimental.claudeCompaction.hint")}
                          </small>
                        </div>
                        <label className={`supply-toggle ${debugClaudeServerSideCompaction ? "checked" : ""}`}>
                          <input
                            type="checkbox"
                            role="switch"
                            aria-label={t("debugConsole.experimental.claudeCompaction.title")}
                            aria-describedby="debug-claude-compaction-hint"
                            checked={debugClaudeServerSideCompaction}
                            onChange={(event) => changeDebugClaudeServerSideCompaction(event.target.checked)}
                          />
                          <span className="supply-toggle-track" aria-hidden="true"><span /></span>
                          <span>{debugClaudeServerSideCompaction
                            ? t("debugConsole.experimental.enabled")
                            : t("debugConsole.experimental.disabled")}</span>
                        </label>
                      </div>
                    </div>

                    <div className="debug-section-title">
                    <h2>{t("debugConsole.requestSettings")}</h2>
                    <p className="section-hint">{t("debugConsole.requestSettingsHint")}</p>
                    </div>

                    <div className="debug-console-controls">
                      <label>
                      {t("supplier.model")}
                        <ModelInput value={effectiveDebugModel} models={debugModelOptions} onChange={setDebugModel} />
                      </label>
                      <label>
                      {t("debugConsole.inboundProtocol")}
                        <select
                          value={debugInboundProtocol}
                          disabled={debugClaudeServerSideCompaction}
                          onChange={(event) => updateDebugInboundProtocol(event.target.value as DebugProtocol)}
                        >
                          {debugProtocolOptions().map((protocol) => (
                            <option key={protocol} value={protocol}>{protocolLabel(protocol)}</option>
                          ))}
                        </select>
                      </label>
                      <label>
                      {t("debugConsole.targetProtocol")}
                        <div className="debug-protocol-row">
                          <select
                            value={debugTargetProtocol}
                            disabled={debugClaudeServerSideCompaction}
                            onChange={(event) => setDebugTargetProtocol(event.target.value as DebugProtocol)}
                          >
                            {debugProtocolOptions().map((protocol) => (
                              <option key={protocol} value={protocol}>{protocolLabel(protocol)}</option>
                            ))}
                          </select>
                          <button
                            className="quiet"
                            type="button"
                            disabled={debugClaudeServerSideCompaction}
                            onClick={() => setDebugTargetProtocol(recommendedDebugTargetProtocol)}
                          >
                          <Undo2 size={16} />{t("debugConsole.recommended")}
                          </button>
                        </div>
                        <span className="field-hint">{debugTargetProtocolHint(selectedDebugTarget, debugTargetProtocol, debugInboundProtocol, debugTargetType)}</span>
                      </label>
                      <label className="debug-prompt-field">
                        {t("debugConsole.prompt")}
                        <textarea
                          rows={4}
                          value={debugPrompt}
                          onChange={(event) => setDebugPrompt(event.target.value)}
                        />
                      </label>
                    </div>

                    <div className="debug-switches">
                      <label className="check">
                        <input type="checkbox" checked={debugStream} onChange={(event) => setDebugStream(event.target.checked)} />
                        stream
                      </label>
                      <label className="check">
                        <input type="checkbox" checked={debugSkipLocalShortCircuit} onChange={(event) => setDebugSkipLocalShortCircuit(event.target.checked)} />
                      {t("debugConsole.skipLocalShortCircuit")}
                      </label>
                    </div>

                    <div className="route-preview debug-preview">
                    <span>{t("debugConsole.preview.inbound")}: {protocolLabel(debugInboundProtocol)}</span>
                    <span>{t("debugConsole.preview.targetProtocol")}: {protocolLabel(debugTargetProtocol)}</span>
                    <span>{t("debugConsole.preview.conversion")}: {debugConversionText}</span>
                    <span>{t("supplier.model")}: {effectiveDebugModel}</span>
                    </div>

                    <div className={debugTargetType === "platform_auto" ? "route-decision-panel" : "route-decision-panel muted"}>
                      <div className="route-decision-title">
                        <div>
                        <strong>{t("debugConsole.route.title")}</strong>
                          <small>
                            {routePlanUnavailableReason || routePlanSourceText}
                          </small>
                        </div>
                        <button
                          className="quiet"
                          type="button"
                          onClick={() => refreshRoutePlan()}
                          disabled={routePlanState.status === "loading" || Boolean(routePlanUnavailableReason)}
                        >
                        <RefreshCw size={16} />{routePlanState.status === "loading"
                          ? t("common.refreshing")
                          : t("debugConsole.route.refresh")}
                        </button>
                      </div>

                      {routePlanUnavailableReason ? (
                        <p className="route-decision-empty">{routePlanUnavailableReason}</p>
                      ) : routePlanState.status === "idle" ? (
                      <p className="route-decision-empty">{t("debugConsole.route.idleHint")}</p>
                      ) : routePlanState.status === "loading" ? (
                      <p className="route-decision-empty">{t("debugConsole.route.loading")}</p>
                      ) : routePlanState.status === "error" || routePlanState.status === "unavailable" ? (
                      <p className="route-decision-empty error-text">{routePlanState.error || t("debugConsole.route.unavailable")}</p>
                      ) : routePlanState.decision ? (
                        <>
                          <div className="test-meta route-decision-meta">
                        <span>{routePlanState.decision.selected
                          ? t("debugConsole.route.selected")
                          : t("debugConsole.route.notSelected")}</span>
                        <span>{t("debugConsole.route.node")}: {routeNodeLabel(routePlanState.decision.selected_node)}</span>
                        <span>{t("debugConsole.route.reason")}: {routePlanState.decision.selection_reason || "-"}</span>
                        <span>{t("debugConsole.route.protocol")}: {routePlanState.decision.selected_protocol ? protocolLabel(routePlanState.decision.selected_protocol) : "-"}</span>
                        <span>{t("debugConsole.route.upstreamModel")}: {routePlanState.decision.selected_upstream_model || routePlanState.decision.selected_node?.upstream_model || "-"}</span>
                        <span>{t("debugConsole.route.price")}: {routePlanState.decision.selected_price_ratio === undefined
                          ? "-"
                          : `${formatRouteScore(routePlanState.decision.selected_price_ratio)}× / ${routePlanState.decision.best_price_ratio === undefined
                            ? "-"
                            : `${formatRouteScore(routePlanState.decision.best_price_ratio)}×`}`}</span>
                        <span>{t("debugConsole.route.priceReason")}: {routePlanState.decision.price_selection_reason || "-"}</span>
                        <span>{t("debugConsole.preview.conversion")}: {routePlanState.decision.conversion_level || "-"}</span>
                        <span>{t("debugConsole.route.sticky")}: {routePlanStickyText(routePlanState.decision)}</span>
                        <span>{t("debugConsole.route.cachedRoute")}: {routePlanCacheText(routePlanState.decision)}</span>
                          </div>
                          <div className="route-candidate-list">
                            <div className="route-candidate-header">
                            <span>{t("debugConsole.route.node")}</span>
                            <span>{t("supplier.model")}</span>
                            <span>{t("debugConsole.route.protocol")}</span>
                            <span>{t("debugConsole.route.filter")}</span>
                            <span>{t("debugConsole.route.score")}</span>
                            <span>{t("debugConsole.route.healthQuota")}</span>
                            </div>
                            {(routePlanState.decision.candidates ?? []).length > 0 ? (
                              (routePlanState.decision.candidates ?? []).map((candidate) => (
                                <div
                                  className={candidate.node_id === routePlanState.decision?.selected_node?.node_id ? "route-candidate-row selected" : "route-candidate-row"}
                                  key={candidate.node_id}
                                >
                                  <span title={candidate.cache_domain_id || undefined}>{candidate.node_id || "-"}</span>
                                  <span>{candidate.public_model || "-"}</span>
                                  <span>{routeCandidateProtocolText(candidate)}</span>
                                  <span>{routeCandidateFilterText(candidate)}</span>
                                  <span>{formatRouteScore(candidate.score)}</span>
                                  <span>{routeCandidateHealthText(candidate)}</span>
                                </div>
                              ))
                            ) : (
                            <p className="route-decision-empty">{t("debugConsole.route.noCandidates")}</p>
                            )}
                          </div>
                        </>
                      ) : null}
                    </div>

                    <div className="actions debug-actions">
                      <button className="quiet" type="button" onClick={() => runProtocolDebug(false)} disabled={debugRunning !== "idle"}>
                      <Eye size={16} />{debugRunning === "preview"
                        ? t("debugConsole.previewing")
                        : t("debugConsole.previewOnly")}
                      </button>
                      <button className="primary" type="button" onClick={() => runProtocolDebug(true)} disabled={debugRunning !== "idle"}>
                      <MessageSquareText size={16} />{debugRunning === "execute"
                        ? t("debugConsole.sending")
                        : t("debugConsole.convertAndSend")}
                      </button>
                    </div>
                  </div>

                  <div className={`debug-result ${debugOutcome === "request_failed" ? "error" : debugResult?.executed ? "success" : ""}`}>
                    <div className="section-title-row">
                      <div>
                      <h2>{t("debugConsole.result.title")}</h2>
                      <p className="section-hint">{t("debugConsole.result.hint")}</p>
                      </div>
                    </div>
                    {debugOutcomeCopy && (
                      <div className={`debug-outcome ${debugOutcomeCopy.tone}`} role="status">
                        <strong>{debugOutcomeCopy.title}</strong>
                        <span>{debugOutcomeCopy.detail}</span>
                      </div>
                    )}
                    <div className="test-meta">
                    <span>{t("debugConsole.preview.conversion")}: {debugResult?.conversion_level ?? "-"}</span>
                      <span>HTTP: {debugResult?.http_status ?? "-"}</span>
                    <span>{t("debugConsole.result.latency")}: {debugResult?.latency_ms ? `${debugResult.latency_ms}ms` : "-"}</span>
                    <span>{t("debugConsole.result.path")}: {debugResult?.path ?? "-"}</span>
                    <span>{t("debugConsole.result.errorLayer")}: {debugResult?.error_layer || "-"}</span>
                    <span>{t("debugConsole.result.request")}: {formatByteCount(debugMetrics?.request_body_bytes)}</span>
                    <span>{t("debugConsole.result.responseType")}: {responseKindLabel(debugMetrics?.response_kind)}</span>
                    <span>{t("debugConsole.result.toolCalls")}: {debugMetrics?.tool_call_count ?? "-"}</span>
                      <span>Finish: {debugMetrics?.finish_reason || "-"}</span>
                    <span>{t("debugConsole.result.rawResponse")}: {formatByteCount(debugMetrics?.response_raw_bytes)}</span>
                    <span>{t("debugConsole.result.responseContent")}: {formatByteCount(debugMetrics?.response_content_bytes)}</span>
                    <span>{t("debugConsole.result.contentChars")}: {debugMetrics?.response_content_chars ?? "-"}</span>
                    <span>{t("debugConsole.result.sseEvents")}: {debugMetrics?.response_sse_events ?? "-"}</span>
                    <span>{t("debugConsole.result.sseDone")}: {debugMetrics?.response_sse_done ? t("common.yes") : t("common.no")}</span>
                    <span>{t("debugConsole.result.sseLast")}: {debugMetrics?.response_sse_last_event_type || "-"}</span>
                    <span>{t("debugConsole.result.rawLines")}: {debugMetrics?.response_raw_lines ?? "-"}</span>
                    </div>
                    <div className="debug-result-grid">
                      <div>
                      <strong>{t("debugConsole.result.requestHeaders")}</strong>
                      {debugResult ? <pre>{JSON.stringify(debugResult.request_headers, null, 2)}</pre> : <p className="debug-empty">{t("debugConsole.waitingPreview")}</p>}
                      </div>
                      <div>
                      <strong>{t("debugConsole.result.requestBody")}</strong>
                      {debugResult ? <pre>{JSON.stringify(debugResult.request_body, null, 2)}</pre> : <p className="debug-empty">{t("debugConsole.waitingPreview")}</p>}
                      </div>
                      <div>
                      <strong>{t("debugConsole.result.conversionDiagnostics")}</strong>
                        {debugResult ? <pre>{JSON.stringify({
                          unsupported_fields: debugResult.unsupported_fields,
                          lossy_warnings: debugResult.lossy_warnings,
                          requested_model: debugResult.requested_model,
                          upstream_model: debugResult.upstream_model,
                          content_type: debugResult.content_type,
                      }, null, 2)}</pre> : <p className="debug-empty">{t("debugConsole.waitingPreview")}</p>}
                      </div>
                      <div>
                      <strong>{t("debugConsole.result.responseContent")}</strong>
                      {debugResult?.executed ? <pre>{debugResponseContent}</pre> : <p className="debug-empty">{t("debugConsole.waitingExecute")}</p>}
                      </div>
                      {debugRawResponse ? (
                        <div className="debug-raw-response">
                        <strong>{t("debugConsole.result.rawResponse")}</strong>
                          <pre>{debugRawResponse}</pre>
                        </div>
                      ) : null}
                    </div>
                  </div>
                </div>
              </div>
            </DialogBackdrop>
          )}
          {marketPricesContext && (
            <React.Suspense fallback={null}><MarketPricesModal
              key={marketPriceScope}
              cache={marketPriceCache}
              cacheScope={marketPriceScope}
              platformAvailable={account.status.state === "signed_in"}
              cnyPerUSD={account.status.display_cny_per_usd}
              initialView={marketPricesContext.initialView}
              channelContext={marketPricesContext.channel}
              onClose={() => setMarketPricesContext(null)}
            /></React.Suspense>
          )}
          {activeCallLog && (
            <CallLogModal
              log={activeCallLog}
              selected={selectedCallRecord}
              cnyPerUSD={account.status.display_cny_per_usd}
              onSelect={setSelectedCallRecord}
              onClose={closeCallLog}
            />
          )}
          {logOpen && (
            <DialogBackdrop aria-labelledby="runtime-log-title">
              <div className="modal-panel log-panel">
                <div className="log-header">
                  <div>
                    <h2 id="runtime-log-title">{t("runtimeLog.title")}</h2>
                    <p>{t("runtimeLog.hint")}</p>
                  </div>
                  <div className="actions log-header-actions">
                    <button className="quiet" onClick={() => copyText(logText(), t("runtimeLog.copied"))} disabled={appLogs.length === 0}>
                      <Copy size={16} />{t("common.copy")}
                    </button>
                    <button className="quiet" onClick={() => setAppLogs([])} disabled={appLogs.length === 0}>
                      <Trash2 size={16} />{t("runtimeLog.clear")}
                    </button>
                  </div>
                  <ModalCloseButton onClick={() => setLogOpen(false)} />
                </div>
                <div className="log-list">
                  {appLogs.length === 0 ? (
                    <div className="empty-log">{t("runtimeLog.empty")}</div>
                  ) : (
                    appLogs.map((entry) => (
                      <div className={`log-entry ${entry.tone}`} key={entry.id}>
                        <span>{entry.time}</span>
                        <strong>{entry.tone}</strong>
                        <p>{entry.text}</p>
                      </div>
                    ))
                  )}
                </div>
              </div>
            </DialogBackdrop>
          )}
          {confirmDialog && (
            <BlockingConfirmDialog dialog={confirmDialog} onResolve={resolveConfirmDialog} />
          )}
          {toolRemoveDialog && (
            <ToolRemoveDialog
              dialog={toolRemoveDialog}
              onResolve={resolveToolRemoveDialog}
            />
          )}
          {resetLocalDataConfirmOpen && (
            <ResetLocalDataDialog
              busy={resetLocalDataBusy}
              onCancel={() => setResetLocalDataConfirmOpen(false)}
              onConfirm={() => void resetLocalAppData()}
            />
          )}
          {channelDuplicateDialog && (
            <ChannelDuplicateDialog
              dialog={channelDuplicateDialog}
              onResolve={resolveChannelDuplicateDialog}
            />
          )}
          {legalDocumentOpen && (
            <LegalNotice
              mode="review"
              initialDocument={legalDocumentOpen}
              onClose={() => setLegalDocumentOpen(null)}
            />
          )}
          {guidanceNoticeCount ? (
            <div
              className={`guidance-notice-stack${guidanceNoticeCount > 1 ? " is-scrollable" : ""}${visibleOnboardingGuideState && onboardingGuideRenderedCollapsed && !visibleGuidanceNotice ? " is-bubble-only" : ""}`}
              role="region"
              aria-label={t("notifications.region")}
            >
              {visibleGuidanceNotice ? (
                <GuidanceNoticeCard
                  key={visibleGuidanceNotice.dedupeKey}
                  variant="tray"
                  notice={visibleGuidanceNotice}
                  dismissLabel={t("notifications.close")}
                  onAction={(actionId) => {
                    if (actionId === "open_account_funds") {
                      dismissGuidanceNotice(visibleGuidanceNotice.dedupeKey);
                      openAccountFunds("insufficient_balance");
                    }
                  }}
                  onDismiss={() => {
                    if (visibleGuidanceNotice.persistentDismissalKey) {
                      dismissPlatformAnnouncement(visibleGuidanceNotice.persistentDismissalKey);
                    }
                    dismissGuidanceNotice(visibleGuidanceNotice.dedupeKey);
                  }}
                />
              ) : null}
              {visibleOnboardingGuideState ? (
                <OnboardingGuide
                  variant="tray"
                  state={visibleOnboardingGuideState}
                  balanceLabel={usageBalanceLabel}
                  collapsed={onboardingGuideRenderedCollapsed}
                  onRegister={startRegistration}
                  onChooseLocal={chooseLocalMode}
                  onAddModelChannel={() => {
                    setTab("supplier");
                    window.requestAnimationFrame(() => setSupplierAddOpen(true));
                  }}
                  onReviewModelChannel={() => {
                    setTab("supplier");
                    const channel = displayChannels[0];
                    if (channel) {
                      window.requestAnimationFrame(() => void openSupplierChannel(channel.id));
                    }
                  }}
                  onOpenFunds={openAccountFunds}
                  onReturnToUse={tab === "access" ? undefined : () => setTab("access")}
                  onCollapse={() => setOnboardingGuideCollapsed(true)}
                  onExpand={() => setOnboardingGuideCollapsed(false)}
                />
              ) : null}
            </div>
          ) : null}
          {toast ? (
            <button className={`toast ${toast.tone}`} type="button" onClick={() => setLogOpen(true)} title={t("runtimeLog.open")}>
              {toast.text}
            </button>
          ) : null}
        </section>
      </main>
    );
}

type LegalGateState = "checking" | "required" | "accepted";
type LegalNoticeStatus = {
  accepted: boolean;
  previously_accepted: boolean;
  agreement_version: number;
  agreement_sha256: string;
  privacy_version: number;
  privacy_sha256: string;
};

function DesktopWindowFrame({
  children,
  applicationDisplayName = APPLICATION_DISPLAY_NAME,
}: {
  children: React.ReactNode;
  applicationDisplayName?: string;
}) {
  const { t } = useTranslation();
  const [isWindowMaximized, setIsWindowMaximized] = useState(false);

  useEffect(() => {
    if (!hasTauriRuntime()) return;

    const appWindow = getCurrentWindow();
    let disposed = false;
    let unlisten: (() => void) | undefined;
    const syncMaximizedState = () => {
      void appWindow.isMaximized()
        .then((maximized) => {
          if (!disposed) setIsWindowMaximized(maximized);
        })
        .catch(() => undefined);
    };

    syncMaximizedState();
    void appWindow.onResized(syncMaximizedState)
      .then((stopListening) => {
        if (disposed) stopListening();
        else unlisten = stopListening;
      })
      .catch(() => undefined);

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  const runWindowCommand = (command: (appWindow: ReturnType<typeof getCurrentWindow>) => Promise<void>) => {
    if (!hasTauriRuntime()) return;
    void command(getCurrentWindow()).catch(() => undefined);
  };

  return (
    <>
      <div
        className="native-titlebar-surface"
        data-tauri-drag-region
      >
        <div className="native-titlebar-brand" data-tauri-drag-region aria-label={applicationDisplayName}>
          <img
            src={titlebarLogo}
            srcSet={`${titlebarLogo} 1x, ${titlebarLogo2x} 2x`}
            alt=""
            aria-hidden="true"
            draggable={false}
            data-tauri-drag-region
          />
          <strong data-tauri-drag-region>{applicationDisplayName}</strong>
        </div>
        <div className="native-window-controls">
          <button
            type="button"
            aria-label={t("windowControls.minimize")}
            title={t("windowControls.minimize")}
            onClick={() => runWindowCommand((appWindow) => appWindow.minimize())}
          >
            <Minus size={16} aria-hidden="true" />
          </button>
          <button
            type="button"
            aria-label={isWindowMaximized ? t("windowControls.restore") : t("windowControls.maximize")}
            title={isWindowMaximized ? t("windowControls.restore") : t("windowControls.maximize")}
            onClick={() => runWindowCommand((appWindow) => appWindow.toggleMaximize())}
          >
            {isWindowMaximized
              ? <Copy size={14} aria-hidden="true" />
              : <Square size={13} aria-hidden="true" />}
          </button>
          <button
            className="native-window-close"
            type="button"
            aria-label={t("windowControls.closeToBackground")}
            title={t("windowControls.closeToBackground")}
            onClick={() => runWindowCommand((appWindow) => appWindow.close())}
          >
            <X size={17} aria-hidden="true" />
          </button>
        </div>
      </div>
      <PageFindBar
        enabled={hasTauriRuntime() && document.documentElement.dataset.platform === "macos"}
      />
      {children}
    </>
  );
}

function App() {
  const { t, i18n } = useTranslation();
  const tauriRuntime = hasTauriRuntime();
  const clientMode = useClientModePolicy({
    developmentProfile: DEVELOPMENT_PROFILE,
    viteDevelopment: import.meta.env.DEV,
  });
  const presentationDisplayName = clientMode.policy.presentation.showDevelopmentBranding
    ? APPLICATION_DISPLAY_NAME
    : PRODUCT_DISPLAY_NAME;
  const [gateState, setGateState] = useState<LegalGateState>(() => tauriRuntime ? "checking" : "accepted");
  const [gateBusy, setGateBusy] = useState(false);
  const [gateError, setGateError] = useState("");
  const [firstUseRegistration, setFirstUseRegistration] = useState(false);

  useEffect(() => {
    if (!tauriRuntime) return;
    void invoke("sync_native_language", {
      language: i18n.resolvedLanguage === "zh-CN" ? "zh-CN" : "en-US",
    }).catch(() => {
      // Native labels are secondary; renderer localization remains available.
    });
  }, [i18n.resolvedLanguage, tauriRuntime]);

  useEffect(() => {
    if (!tauriRuntime) return;
    let cancelled = false;
    void invoke<LegalNoticeStatus>("legal_notice_status")
      .then((status) => {
        if (cancelled) return;
        setFirstUseRegistration(!status.previously_accepted);
        if (
          status.agreement_version !== USER_AGREEMENT_VERSION
          || status.agreement_sha256 !== USER_AGREEMENT_SHA256
          || status.privacy_version !== PRIVACY_POLICY_VERSION
          || status.privacy_sha256 !== PRIVACY_POLICY_SHA256
        ) {
          setGateError(t("legal.versionMismatch"));
          setGateState("required");
          return;
        }
        setGateState(status.accepted ? "accepted" : "required");
      })
      .catch((error) => {
        if (cancelled) return;
        setFirstUseRegistration(true);
        setGateError(t("legal.readStatusFailed", { error: localizedError(error) }));
        setGateState("required");
      });
    return () => {
      cancelled = true;
    };
  }, [tauriRuntime]);

  async function acceptLegalDocuments() {
    setGateBusy(true);
    setGateError("");
    try {
      await invoke("accept_legal_notice", {
        agreementVersion: USER_AGREEMENT_VERSION,
        agreementSha256: USER_AGREEMENT_SHA256,
        privacyVersion: PRIVACY_POLICY_VERSION,
        privacySha256: PRIVACY_POLICY_SHA256,
      });
      setGateState("accepted");
    } catch (error) {
      setGateError(t("legal.saveFailed", { error: localizedError(error) }));
    } finally {
      setGateBusy(false);
    }
  }

  async function declineLegalDocuments() {
    setGateBusy(true);
    setGateError("");
    try {
      await invoke("decline_legal_notice");
    } catch (error) {
      setGateError(t("legal.exitFailed", { error: localizedError(error) }));
      setGateBusy(false);
      try {
        await getCurrentWindow().close();
      } catch {
        // The native process may already be exiting.
      }
    }
  }

  if (gateState === "checking") {
    return (
      <DesktopWindowFrame applicationDisplayName={presentationDisplayName}>
        <LegalNoticeLoading />
      </DesktopWindowFrame>
    );
  }
  if (gateState === "required") {
    return (
      <DesktopWindowFrame applicationDisplayName={presentationDisplayName}>
        <LegalNotice
          mode="gate"
          busy={gateBusy}
          error={gateError}
          onAccept={() => void acceptLegalDocuments()}
          onDecline={() => void declineLegalDocuments()}
        />
      </DesktopWindowFrame>
    );
  }
  return (
    <DesktopWindowFrame applicationDisplayName={presentationDisplayName}>
      <ProductApp
        initialTab={firstUseRegistration ? "account" : "access"}
        initialAccountAuthMode={firstUseRegistration ? "register" : "password"}
        clientModePolicy={clientMode.policy}
        setProductionPresentationPreview={clientMode.setProductionPresentationPreview}
      />
    </DesktopWindowFrame>
  );
}

async function renderApplication() {
  await initializeI18n();
  const rootElement = document.getElementById("root")!;
  appWindow.__CONST_API_ROOT__ ??= createRoot(rootElement);
  startupConsoleInfo("[const-api][startup][ui] root render start in %dms", Math.round(performance.now() - UI_SCRIPT_STARTED_AT));
  startupLogFromUi("root render start");
  appWindow.__CONST_API_ROOT__.render(
    <AppErrorBoundary fallback={<DesktopWindowFrame><RendererFailure /></DesktopWindowFrame>}>
      <App />
    </AppErrorBoundary>,
  );
  startupConsoleInfo("[const-api][startup][ui] root render scheduled in %dms", Math.round(performance.now() - UI_SCRIPT_STARTED_AT));
  startupLogFromUi("root render scheduled");
}

void renderApplication();
