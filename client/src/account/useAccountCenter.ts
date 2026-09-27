import { useRef, useState } from "react";

import { localizedError, stableErrorCode, tr } from "../i18n";
import type { AccountApi } from "./accountApi";
import type {
  AccountAuthMode,
  AccountChallenge,
  AccountNotice,
  AccountRegistrationPolicy,
  AccountStatus,
  BillingBucket,
  BillingFilters,
  BillingRecord,
  BillingSummary,
} from "./accountTypes";
import { keepPreviousIfStructurallyEqual } from "../stateSnapshots";
import {
  createRefreshGate,
  type RefreshOptions,
} from "../refreshPolicy";

type AppliedBillingFilters = Pick<BillingFilters, "direction" | "status" | "model">;

export type BillingErrors = {
  summary: string;
  stats: string;
  list: string;
  detail: string;
};

export type OverviewErrors = {
  summary: string;
  stats: string;
  recent: string;
};

export type BillingErrorCodes = BillingErrors;
export type OverviewErrorCodes = OverviewErrors;

export type AccountActionFeedbackScope = "auth" | "email" | "password" | "account" | "transfer";

export type AccountActionFeedback = AccountNotice & {
  scope: AccountActionFeedbackScope;
};

type AccountCenterOptions = {
  api: AccountApi;
  initialStatus: AccountStatus;
  initialAuthMode?: AccountAuthMode;
  notify: (notice: AccountNotice) => void;
  afterIdentityChange: (status: AccountStatus) => void | Promise<void>;
};

export type AccountCenterController = {
  status: AccountStatus;
  registrationPolicy?: AccountRegistrationPolicy;
  busy: boolean;
  actionFeedback: AccountActionFeedback | null;
  email: string;
  authMode: AccountAuthMode;
  password: string;
  newPassword: string;
  passwordConfirm: string;
  passwordCode: string;
  passwordChallenge: AccountChallenge | null;
  code: string;
  inviteCode?: string;
  challenge: AccountChallenge | null;
  emailChallenge: AccountChallenge | null;
  billingSummary: BillingSummary | null;
  billingRecords: BillingRecord[];
  billingStats: BillingBucket[];
  overviewSummary: BillingSummary | null;
  overviewRecords: BillingRecord[];
  overviewStats: BillingBucket[];
  overviewErrors: OverviewErrors;
  overviewErrorCodes?: OverviewErrorCodes;
  overviewLoading: boolean;
  billingErrors: BillingErrors;
  billingErrorCodes?: BillingErrorCodes;
  billingDirection: "" | "usage" | "supply";
  billingStatus: string;
  billingModel: string;
  billingCursor: string;
  billingCursorHistory: string[];
  billingNextCursor: string;
  billingOlderArchived: boolean;
  billingLoading: boolean;
  billingTransferAmount: string;
  billingDetail: BillingRecord | null;
  billingDetailId: string;
  billingDetailLoading: boolean;
  setters: {
    email(value: string): void;
    authMode(value: AccountAuthMode): void;
    password(value: string): void;
    newPassword(value: string): void;
    passwordConfirm(value: string): void;
    passwordCode(value: string): void;
    code(value: string): void;
    inviteCode?(value: string): void;
    challenge(value: AccountChallenge | null): void;
    billingDirection(value: "" | "usage" | "supply"): void;
    billingStatus(value: string): void;
    billingModel(value: string): void;
    billingCursorHistory(value: string[] | ((current: string[]) => string[])): void;
    billingTransferAmount(value: string): void;
    billingDetail(value: BillingRecord | null): void;
  };
  actions: {
    readStatus(): Promise<AccountStatus>;
    refreshStatus(): Promise<AccountStatus>;
    initializeStatus(): Promise<AccountStatus | undefined>;
    refresh(options?: RefreshOptions): Promise<void>;
    requestCode(): Promise<void>;
    verifyCode(): Promise<void>;
    register(): Promise<void>;
    loginWithPassword(identifier?: string, password?: string): Promise<void>;
    changePassword(): Promise<void>;
    requestPasswordCode(): Promise<void>;
    requestEmailCode(): Promise<void>;
    verifyEmail(): Promise<void>;
    logout(): Promise<void>;
    loadOverview(options?: RefreshOptions): Promise<void>;
    applyBillingFilters(filters: AppliedBillingFilters): Promise<void>;
    loadBilling(cursor?: string, options?: RefreshOptions): Promise<void>;
    nextBillingPage(): Promise<void>;
    previousBillingPage(): Promise<void>;
    loadBillingDetail(id: string): Promise<void>;
    transferSupplierFunds(amountUSD?: number): Promise<void>;
    clearActionFeedback(scope?: AccountActionFeedbackScope): void;
  };
};

export function useAccountCenter({
  api,
  initialStatus,
  initialAuthMode = "password",
  notify,
  afterIdentityChange,
}: AccountCenterOptions): AccountCenterController {
  const [status, setStatus] = useState(initialStatus);
  const statusRef = useRef(initialStatus);
  const [registrationPolicy, setRegistrationPolicy] = useState<AccountRegistrationPolicy>({
    registration_enabled: true,
    password_login_enabled: true,
    email_login_configured: false,
    verified_email_registration_required: false,
    bootstrap_admin_password_login_always_enabled: true,
    referral_signup_reward_usd: 0,
    display_cny_per_usd: 7,
  });
  const [busy, setBusy] = useState(false);
  const [actionFeedback, setActionFeedback] = useState<AccountActionFeedback | null>(null);
  const [email, setEmail] = useState(initialStatus.email || "");
  const [authMode, setAuthMode] = useState<AccountAuthMode>(initialAuthMode);
  const [password, setPassword] = useState("");
  const [newPassword, setNewPassword] = useState("");
  const [passwordConfirm, setPasswordConfirm] = useState("");
  const [passwordCode, setPasswordCode] = useState("");
  const [passwordChallenge, setPasswordChallenge] = useState<(AccountChallenge & {
    email: string;
    userId: string;
    platformId: string;
  }) | null>(null);
  const [code, setCode] = useState("");
  const [inviteCode, setInviteCode] = useState("");
  const [challenge, setChallenge] = useState<AccountChallenge | null>(null);
  const [emailChallenge, setEmailChallenge] = useState<AccountChallenge | null>(null);
  const [accountSummary, setAccountSummary] = useState<BillingSummary | null>(null);
  const billingSummary = accountSummary;
  const overviewSummary = accountSummary;
  const [billingRecords, setBillingRecords] = useState<BillingRecord[]>([]);
  const [billingStats, setBillingStats] = useState<BillingBucket[]>([]);
  const [overviewRecords, setOverviewRecords] = useState<BillingRecord[]>([]);
  const [overviewStats, setOverviewStats] = useState<BillingBucket[]>([]);
  const [overviewErrors, setOverviewErrors] = useState<OverviewErrors>({ summary: "", stats: "", recent: "" });
  const [overviewErrorCodes, setOverviewErrorCodes] = useState<OverviewErrorCodes>({ summary: "", stats: "", recent: "" });
  const [overviewLoading, setOverviewLoading] = useState(false);
  const [billingErrors, setBillingErrors] = useState<BillingErrors>({ summary: "", stats: "", list: "", detail: "" });
  const [billingErrorCodes, setBillingErrorCodes] = useState<BillingErrorCodes>({ summary: "", stats: "", list: "", detail: "" });
  const [billingDirection, setBillingDirection] = useState<"" | "usage" | "supply">("");
  const [billingStatus, setBillingStatus] = useState("");
  const [billingModel, setBillingModel] = useState("");
  const [billingCursor, setBillingCursor] = useState("");
  const [billingCursorHistory, setBillingCursorHistory] = useState<string[]>([]);
  const [billingNextCursor, setBillingNextCursor] = useState("");
  const [billingOlderArchived, setBillingOlderArchived] = useState(false);
  const [billingListLoading, setBillingListLoading] = useState(false);
  const [billingTransferLoading, setBillingTransferLoading] = useState(false);
  const [billingTransferAmount, setBillingTransferAmount] = useState("");
  const [billingDetail, setBillingDetail] = useState<BillingRecord | null>(null);
  const [billingDetailId, setBillingDetailId] = useState("");
  const [billingDetailLoading, setBillingDetailLoading] = useState(false);
  const billingListGeneration = useRef(0);
  const overviewGeneration = useRef(0);
  const billingDetailGeneration = useRef(0);
  const billingMutationGeneration = useRef(0);
  const accountActionInFlight = useRef<Promise<void> | null>(null);
  const overviewLoadInFlight = useRef<Promise<void> | null>(null);
  const billingListInFlight = useRef<{
    key: string;
    promise: Promise<boolean>;
  } | null>(null);
  const billingTransferInFlight = useRef(false);
  const billingTransferAttempt = useRef<{ amount: number; requestId: string } | null>(null);
  const [overviewRefreshGate] = useState(createRefreshGate);
  const [billingRefreshGate] = useState(createRefreshGate);
  const [accountRefreshGate] = useState(createRefreshGate);
  const billingLoading = billingListLoading || billingTransferLoading;

  const globalSuccess = (text: string) => notify({ tone: "success", text });

  function showActionFeedback(
    scope: AccountActionFeedbackScope,
    text: string,
    tone: AccountNotice["tone"] = "error",
  ) {
    setActionFeedback({ scope, text, tone });
  }

  function clearActionFeedback(scope?: AccountActionFeedbackScope) {
    setActionFeedback((current) => {
      if (!current || (scope && current.scope !== scope)) return current;
      return null;
    });
  }

  function clearCredentials() {
    setPassword("");
    setPasswordCode("");
    setPasswordChallenge(null);
    setNewPassword("");
    setPasswordConfirm("");
  }

  function invalidateBillingList() {
    billingListGeneration.current += 1;
    billingListInFlight.current = null;
    setBillingListLoading(false);
  }

  function clearBillingState() {
    billingListGeneration.current += 1;
    overviewGeneration.current += 1;
    billingDetailGeneration.current += 1;
    billingMutationGeneration.current += 1;
    billingListInFlight.current = null;
    overviewLoadInFlight.current = null;
    setAccountSummary(null);
    setBillingRecords([]);
    setBillingStats([]);
    setOverviewRecords([]);
    setOverviewStats([]);
    setOverviewErrors({ summary: "", stats: "", recent: "" });
    setOverviewErrorCodes({ summary: "", stats: "", recent: "" });
    setOverviewLoading(false);
    setBillingErrors({ summary: "", stats: "", list: "", detail: "" });
    setBillingErrorCodes({ summary: "", stats: "", list: "", detail: "" });
    setBillingCursor("");
    setBillingCursorHistory([]);
    setBillingNextCursor("");
    setBillingOlderArchived(false);
    setBillingListLoading(false);
    setBillingTransferLoading(false);
    setBillingTransferAmount("");
    billingTransferInFlight.current = false;
    billingTransferAttempt.current = null;
    setBillingDetail(null);
    setBillingDetailId("");
    setBillingDetailLoading(false);
    overviewRefreshGate.reset();
    billingRefreshGate.reset();
    accountRefreshGate.reset();
    clearActionFeedback("transfer");
  }

  function applyStatus(next: AccountStatus) {
    const previous = statusRef.current;
    const settled = keepPreviousIfStructurallyEqual(previous, next);
    if (settled === previous) return;
    statusRef.current = settled;
    setStatus(settled);
    if (previous.user_id !== settled.user_id || previous.platform_id !== settled.platform_id
      || previous.email !== settled.email
      || settled.state !== "signed_in") {
      setPasswordCode("");
      setPasswordChallenge(null);
    }
    if (previous.state === "signed_in" && settled.state !== "signed_in") clearBillingState();
  }

  async function finishLogin(next: AccountStatus) {
    accountRefreshGate.reset();
    applyStatus(next);
    setEmail(next.email || "");
    setChallenge(null);
    setCode("");
    setInviteCode("");
    clearCredentials();
    clearActionFeedback();
    await afterIdentityChange(next);
  }

  function notifyLoginResult(next: AccountStatus, successText: string) {
    if (next.legal_state === "update_required") {
      notify({ tone: "error", text: tr("account.notices.legalUpdateRequired") });
      return;
    }
    if (next.legal_state === "pending" || next.legal_state === "local_acceptance_required") {
      notify({ tone: "info", text: tr("account.notices.legalSyncPending") });
      return;
    }
    if (next.legal_state === "grace_period") {
      notify({ tone: "info", text: tr("account.notices.legalGrace") });
      return;
    }
    globalSuccess(successText);
  }

  async function readStatus() {
    const next = await api.status();
    applyStatus(next);
    return next;
  }

  async function refreshStatus() {
    const next = await api.refresh();
    applyStatus(next);
    return next;
  }

  async function refreshRegistrationPolicy() {
    if (!api.registrationPolicy) return;
    try {
      setRegistrationPolicy(await api.registrationPolicy());
    } catch {
      // Older servers do not expose registration policy; keep compatible defaults.
    }
  }

  async function initializeStatus() {
    const policyRequest = refreshRegistrationPolicy();
    try {
      const next = await refreshStatus();
      await policyRequest;
      return next;
    } catch {
      try {
        const next = await readStatus();
        await policyRequest;
        return next;
      } catch {
        await policyRequest;
        return undefined;
      }
    }
  }

  function runAccountAction(scope: AccountActionFeedbackScope, action: () => Promise<void>) {
    if (accountActionInFlight.current) return accountActionInFlight.current;

    clearActionFeedback(scope);
    setBusy(true);
    let request: Promise<void>;
    request = Promise.resolve()
      .then(action)
      .catch((error) => {
        const scopeIsGone = (scope === "auth" && statusRef.current.state === "signed_in")
          || (scope === "account" && statusRef.current.state !== "signed_in");
        const message = scope === "email" && String(error).toLowerCase().includes("account_exists")
          ? tr("account.settings.emailOwnedByOtherAccount") : accountActionError(error);
        if (scopeIsGone) notify({ tone: "error", text: message });
        else showActionFeedback(scope, message);
      })
      .finally(() => {
        if (accountActionInFlight.current === request) {
          accountActionInFlight.current = null;
          setBusy(false);
        }
      });
    accountActionInFlight.current = request;
    return request;
  }

  async function requestCode() {
    const normalizedEmail = email.trim();
    if (!normalizedEmail) return;
    await runAccountAction("auth", async () => {
      setChallenge(await api.requestCode(normalizedEmail, authMode === "reset" ? "reset_password" : "login"));
      setCode("");
      showActionFeedback("auth", tr("account.notices.codeSent"), "success");
    });
  }

  async function refresh(options: RefreshOptions = { throttle: true }) {
    if (!accountRefreshGate.begin(options)) return;
    let succeeded = false;
    await runAccountAction("account", async () => {
      await refreshStatus();
      succeeded = true;
      showActionFeedback("account", tr("account.notices.accountRefreshed"), "success");
    });
    if (!succeeded) accountRefreshGate.reset();
  }

  async function verifyCode() {
    const normalizedCode = code.trim();
    if (!challenge || normalizedCode.length !== 6) return;
    if (authMode === "reset" && (newPassword.length < 8 || newPassword !== passwordConfirm)) return;
    await runAccountAction("auth", async () => {
      const normalizedEmail = email.trim();
      const next = authMode === "reset"
        ? await api.resetPassword(challenge.challenge_id, normalizedEmail, normalizedCode, newPassword)
        : await api.verifyCode(challenge.challenge_id, normalizedEmail, normalizedCode);
      await finishLogin(next);
      notifyLoginResult(next, authMode === "reset" ? tr("account.notices.passwordReset") : tr("account.notices.signedIn"));
    });
  }

  async function register() {
    const identifier = email.trim();
    if (!identifier || newPassword.length < 8 || newPassword !== passwordConfirm) return;
    const normalizedCode = code.trim();
    if (challenge && normalizedCode.length !== 6) return;
    await runAccountAction("auth", async () => {
      const next = await api.register(identifier, newPassword, challenge?.challenge_id, normalizedCode || undefined, inviteCode.trim() || undefined);
      if ("challenge_id" in next) {
        setChallenge(next);
        setCode("");
        showActionFeedback("auth", tr("account.notices.registrationCodeSent"), "success");
        return;
      }
      await finishLogin(next);
      notifyLoginResult(next, tr("account.notices.registered"));
    });
  }

  async function loginWithPassword(identifier = email, loginPassword = password) {
    const normalizedIdentifier = identifier.trim();
    if (!normalizedIdentifier || loginPassword.length < 8) return;
    await runAccountAction("auth", async () => {
      const next = await api.passwordLogin(normalizedIdentifier, loginPassword);
      await finishLogin(next);
      notifyLoginResult(next, tr("account.notices.signedIn"));
    });
  }

  async function requestPasswordCode() {
    const current = statusRef.current;
    if (current.state !== "signed_in" || !current.email.trim()) {
      showActionFeedback("password", tr("account.settings.passwordEmailRequired"));
      return;
    }
    await runAccountAction("password", async () => {
      const next = await api.requestCode(current.email.trim(), "reset_password", current.platform_id);
      const latest = statusRef.current;
      if (latest.state !== "signed_in" || latest.user_id !== current.user_id
        || latest.platform_id !== current.platform_id || latest.email !== current.email) return;
      setPasswordChallenge({ ...next, email: current.email.trim(), userId: current.user_id, platformId: current.platform_id });
      setPasswordCode("");
      showActionFeedback("password", tr("account.notices.codeSent"), "success");
    });
  }

  async function changePassword() {
    const current = statusRef.current;
    // The reset code itself proves mailbox ownership, including for legacy
    // password accounts created before SMTP was configured.
    if (current.state !== "signed_in" || !passwordChallenge
      || passwordChallenge.userId !== current.user_id || passwordChallenge.platformId !== current.platform_id
      || passwordChallenge.email !== current.email.trim() || !/^\d{6}$/.test(passwordCode)
      || newPassword.length < 8 || newPassword.length > 128 || newPassword !== passwordConfirm) return;
    await runAccountAction("password", async () => {
      const next = await api.resetPassword(
        passwordChallenge.challenge_id, passwordChallenge.email, passwordCode, newPassword, passwordChallenge.platformId,
      );
      applyStatus(next);
      clearCredentials();
      showActionFeedback("password", tr("account.notices.passwordChanged"), "success");
      // Password changes rotate credentials, just like signing in. Refresh the
      // running client configuration without losing the successful outcome if
      // that follow-up refresh temporarily fails.
      try { await afterIdentityChange(next); }
      catch (error) { notify({ tone: "error", text: accountActionError(error) }); }
    });
  }

  async function requestEmailCode() {
    const normalizedEmail = email.trim();
    if (!normalizedEmail) return;
    await runAccountAction("email", async () => {
      setEmailChallenge(await api.requestEmailCode(normalizedEmail));
      setCode("");
      showActionFeedback("email", tr("account.notices.codeSent"), "success");
    });
  }

  async function verifyEmail() {
    const normalizedCode = code.trim();
    if (!emailChallenge || normalizedCode.length !== 6) return;
    await runAccountAction("email", async () => {
      applyStatus(await api.verifyEmail(emailChallenge.challenge_id, email.trim(), normalizedCode));
      setEmailChallenge(null);
      setCode("");
      showActionFeedback("email", tr("account.notices.emailBound"), "success");
    });
  }

  async function logout() {
    await runAccountAction("account", async () => {
      const next = await api.logout();
      applyStatus(next);
      setChallenge(null);
      setEmailChallenge(null);
      clearCredentials();
      clearActionFeedback();
      await afterIdentityChange(next);
      globalSuccess(tr("account.notices.signedOut"));
    });
  }

  async function loadOverview(options: RefreshOptions = {}): Promise<void> {
    if (status.state !== "signed_in") return;
    if (overviewLoadInFlight.current) {
      if (!options.force) return overviewLoadInFlight.current;
      await overviewLoadInFlight.current;
    }
    if ((options.throttle || options.force) && !overviewRefreshGate.begin(options)) return;

    const generation = ++overviewGeneration.current;
    setOverviewLoading(true);
    let request: Promise<void>;
    request = (async () => {
      const [summaryResult, statsResult, recentResult] = await Promise.allSettled([
        api.billingSummary(),
        api.billingStats(),
        api.billingPage({ direction: "", status: "", model: "", cursor: undefined, limit: 5 }),
      ]);
      if (generation !== overviewGeneration.current) return;

      setOverviewErrors({
        summary: summaryResult.status === "rejected" ? accountActionError(summaryResult.reason) : "",
        stats: statsResult.status === "rejected" ? accountActionError(statsResult.reason) : "",
        recent: recentResult.status === "rejected" ? accountActionError(recentResult.reason) : "",
      });
      setOverviewErrorCodes({
        summary: summaryResult.status === "rejected" ? stableErrorCode(summaryResult.reason) : "",
        stats: statsResult.status === "rejected" ? stableErrorCode(statsResult.reason) : "",
        recent: recentResult.status === "rejected" ? stableErrorCode(recentResult.reason) : "",
      });
      if (summaryResult.status === "fulfilled") setAccountSummary(summaryResult.value);
      if (statsResult.status === "fulfilled") setOverviewStats(statsResult.value);
      if (recentResult.status === "fulfilled") setOverviewRecords(recentResult.value.items);
      if ([summaryResult, statsResult, recentResult].some((result) => result.status === "rejected")) {
        overviewRefreshGate.reset();
      }
    })().finally(() => {
      if (overviewLoadInFlight.current === request) {
        overviewLoadInFlight.current = null;
      }
      if (generation === overviewGeneration.current) {
        setOverviewLoading(false);
      }
    });
    overviewLoadInFlight.current = request;
    return request;
  }

  function loadBillingPage(cursor: string, filters: AppliedBillingFilters = {
    direction: billingDirection,
    status: billingStatus,
    model: billingModel,
  }): Promise<boolean> {
    if (status.state !== "signed_in") return Promise.resolve(false);
    const requestKey = JSON.stringify([
      cursor,
      filters.direction,
      filters.status,
      filters.model,
    ]);
    if (billingListInFlight.current?.key === requestKey) {
      return billingListInFlight.current.promise;
    }

    const generation = ++billingListGeneration.current;
    setBillingListLoading(true);
    const entry = {
      key: requestKey,
      promise: Promise.resolve(false),
    };
    entry.promise = (async () => {
      const [summaryResult, pageResult, statsResult] = await Promise.allSettled([
        api.billingSummary(),
        api.billingPage({ ...filters, cursor: cursor || undefined, limit: 30 }),
        api.billingStats(),
      ]);
      if (generation !== billingListGeneration.current) return false;

      const nextErrors = {
        summary: summaryResult.status === "rejected" ? accountActionError(summaryResult.reason) : "",
        list: pageResult.status === "rejected" ? accountActionError(pageResult.reason) : "",
        stats: statsResult.status === "rejected" ? accountActionError(statsResult.reason) : "",
      };
      setBillingErrors((current) => ({ ...current, ...nextErrors }));
      setBillingErrorCodes((current) => ({
        ...current,
        summary: summaryResult.status === "rejected" ? stableErrorCode(summaryResult.reason) : "",
        list: pageResult.status === "rejected" ? stableErrorCode(pageResult.reason) : "",
        stats: statsResult.status === "rejected" ? stableErrorCode(statsResult.reason) : "",
      }));
      if ([summaryResult, pageResult, statsResult].some((result) => result.status === "rejected")) {
        billingRefreshGate.reset();
      }

      if (summaryResult.status === "fulfilled") setAccountSummary(summaryResult.value);
      if (statsResult.status === "fulfilled") setBillingStats(statsResult.value);
      if (pageResult.status === "fulfilled") {
        setBillingRecords(pageResult.value.items);
        setBillingCursor(cursor);
        setBillingNextCursor(pageResult.value.next_cursor ?? "");
        setBillingOlderArchived(pageResult.value.older_archived === true);
        if (cursor === "") setBillingCursorHistory([]);
      }
      return pageResult.status === "fulfilled";
    })().finally(() => {
      if (billingListInFlight.current === entry) {
        billingListInFlight.current = null;
      }
      if (generation === billingListGeneration.current) {
        setBillingListLoading(false);
      }
    });
    billingListInFlight.current = entry;
    return entry.promise;
  }

  async function loadBilling(
    cursor = billingCursor,
    options: RefreshOptions = {},
  ) {
    if (options.force && billingListInFlight.current) {
      await billingListInFlight.current.promise;
    }
    if ((options.throttle || options.force) && !billingRefreshGate.begin(options)) return;
    await loadBillingPage(cursor);
  }

  async function applyBillingFilters(filters: AppliedBillingFilters) {
    const changed = filters.direction !== billingDirection
      || filters.status !== billingStatus
      || filters.model !== billingModel;
    if (await loadBillingPage("", filters) && changed) {
      setBillingDirection(filters.direction);
      setBillingStatus(filters.status);
      setBillingModel(filters.model);
    }
  }

  async function nextBillingPage() {
    if (!billingNextCursor) return;
    const nextCursor = billingNextCursor;
    const currentCursor = billingCursor;
    const currentHistory = billingCursorHistory;
    if (await loadBillingPage(nextCursor)) {
      setBillingCursorHistory([...currentHistory, currentCursor]);
    }
  }

  async function previousBillingPage() {
    if (billingCursorHistory.length === 0) return;
    const previousCursor = billingCursorHistory[billingCursorHistory.length - 1] ?? "";
    const currentHistory = billingCursorHistory;
    if (await loadBillingPage(previousCursor)) {
      setBillingCursorHistory(currentHistory.slice(0, -1));
    }
  }

  async function loadBillingDetail(id: string) {
    if (status.state !== "signed_in" || !id.trim()) return;
    const generation = ++billingDetailGeneration.current;
    setBillingDetailId(id);
    setBillingDetail(null);
    setBillingDetailLoading(true);
    try {
      const detail = await api.billingDetail(id);
      if (generation === billingDetailGeneration.current) {
        setBillingDetail(detail);
        setBillingErrors((current) => ({ ...current, detail: "" }));
        setBillingErrorCodes((current) => ({ ...current, detail: "" }));
      }
    } catch (error) {
      if (generation === billingDetailGeneration.current) {
        setBillingErrors((current) => ({ ...current, detail: accountActionError(error) }));
        setBillingErrorCodes((current) => ({ ...current, detail: stableErrorCode(error) }));
      }
    } finally {
      if (generation === billingDetailGeneration.current) setBillingDetailLoading(false);
    }
  }

  async function transferSupplierFunds(amountUSD?: number) {
    const amount = amountUSD ?? Number(billingTransferAmount);
    if (status.state !== "signed_in" || !Number.isFinite(amount) || amount <= 0 || billingTransferInFlight.current) return;
    const attempt = billingTransferAttempt.current?.amount === amount
      ? billingTransferAttempt.current
      : { amount, requestId: `desktop-${Date.now()}-${Math.random().toString(16).slice(2)}` };
    billingTransferAttempt.current = attempt;
    billingTransferInFlight.current = true;
    const generation = ++billingMutationGeneration.current;
    clearActionFeedback("transfer");
    setBillingTransferLoading(true);
    try {
      const summary = await api.transferSupplierFunds(amount, attempt.requestId);
      if (generation !== billingMutationGeneration.current) return;
      setAccountSummary(summary);
      setOverviewRecords([]);
      setOverviewStats([]);
      setOverviewErrors({ summary: "", stats: "", recent: "" });
      setOverviewErrorCodes({ summary: "", stats: "", recent: "" });
      if (billingTransferAttempt.current?.requestId === attempt.requestId) {
        billingTransferAttempt.current = null;
        setBillingTransferAmount("");
      }
      await Promise.all([
        loadBilling("", { force: true }),
        loadOverview({ force: true }),
      ]);
      if (generation !== billingMutationGeneration.current) return;
      showActionFeedback("transfer", tr("account.notices.fundsTransferred"), "success");
    } catch (error) {
      if (generation === billingMutationGeneration.current) {
        showActionFeedback("transfer", accountActionError(error));
      }
    } finally {
      if (generation === billingMutationGeneration.current) {
        billingTransferInFlight.current = false;
        setBillingTransferLoading(false);
      }
    }
  }

  return {
    status,
    registrationPolicy,
    busy,
    actionFeedback,
    email,
    authMode,
    password,
    newPassword,
    passwordConfirm,
    passwordCode,
    passwordChallenge,
    code,
    inviteCode,
    challenge,
    emailChallenge,
    billingSummary,
    billingRecords,
    billingStats,
    overviewSummary,
    overviewRecords,
    overviewStats,
    overviewErrors,
    overviewErrorCodes,
    overviewLoading,
    billingErrors,
    billingErrorCodes,
    billingDirection,
    billingStatus,
    billingModel,
    billingCursor,
    billingCursorHistory,
    billingNextCursor,
    billingOlderArchived,
    billingLoading,
    billingTransferAmount,
    billingDetail,
    billingDetailId,
    billingDetailLoading,
    setters: {
      email: (value) => {
        clearActionFeedback();
        setEmail(value);
      },
      authMode: (value) => {
        clearActionFeedback();
        setAuthMode(value);
        if (value === "register") void refreshRegistrationPolicy();
        setChallenge(null);
        setCode("");
        setInviteCode("");
        clearCredentials();
      },
      password: (value) => {
        clearActionFeedback("auth");
        setPassword(value);
      },
      newPassword: (value) => {
        clearActionFeedback();
        setNewPassword(value);
      },
      passwordConfirm: (value) => {
        clearActionFeedback();
        setPasswordConfirm(value);
      },
      passwordCode: (value) => {
        clearActionFeedback("password");
        setPasswordCode(value.replace(/\D/g, "").slice(0, 6));
      },
      code: (value) => {
        clearActionFeedback();
        setCode(value);
      },
      inviteCode: (value) => {
        clearActionFeedback("auth");
        setInviteCode(value.toUpperCase().replace(/[^23456789ABCDEFGHJKLMNPQRSTUVWXYZ]/g, "").slice(0, 8));
      },
      challenge: (value) => {
        clearActionFeedback("auth");
        setChallenge(value);
      },
      billingDirection: (value) => {
        if (value === billingDirection) return;
        invalidateBillingList();
        setBillingDirection(value);
        setBillingCursor("");
        setBillingCursorHistory([]);
        setBillingNextCursor("");
      },
      billingStatus: (value) => {
        if (value === billingStatus) return;
        invalidateBillingList();
        setBillingStatus(value);
        setBillingCursor("");
        setBillingCursorHistory([]);
        setBillingNextCursor("");
      },
      billingModel: (value) => {
        if (value === billingModel) return;
        invalidateBillingList();
        setBillingModel(value);
        setBillingCursor("");
        setBillingCursorHistory([]);
        setBillingNextCursor("");
      },
      billingCursorHistory: setBillingCursorHistory,
      billingTransferAmount: (value) => {
        clearActionFeedback("transfer");
        const nextAmount = Number(value);
        if (billingTransferAttempt.current && nextAmount !== billingTransferAttempt.current.amount) billingTransferAttempt.current = null;
        setBillingTransferAmount(value);
      },
      billingDetail: (value) => {
        if (value === null) {
          billingDetailGeneration.current += 1;
          setBillingDetailLoading(false);
          setBillingDetailId("");
          setBillingErrors((current) => ({ ...current, detail: "" }));
          setBillingErrorCodes((current) => ({ ...current, detail: "" }));
        }
        setBillingDetail(value);
      },
    },
    actions: {
      readStatus,
      refreshStatus,
      initializeStatus,
      refresh,
      requestCode,
      verifyCode,
      register,
      loginWithPassword,
      changePassword,
      requestPasswordCode,
      requestEmailCode,
      verifyEmail,
      logout,
      loadOverview,
      applyBillingFilters,
      loadBilling,
      nextBillingPage,
      previousBillingPage,
      loadBillingDetail,
      transferSupplierFunds,
      clearActionFeedback,
    },
  };
}

function accountActionError(error: unknown) {
  const raw = String(error);
  const normalized = raw.toLowerCase();
  const aliases: Array<[string, string]> = [
    ["account_unavailable", "account_unavailable"],
    ["no account endpoint is available", "account_endpoint_unavailable"],
    ["account platform endpoint is unavailable", "account_platform_unavailable"],
    ["invalid_code", "invalid_code"],
    ["invalid_credentials", "invalid_credentials"],
    ["invalid_password", "invalid_password"],
    ["account_exists", "account_exists"],
    ["registration_disabled", "registration_disabled"],
    ["registration_capacity_full", "registration_capacity_full"],
    ["email_registration_required", "email_registration_required"],
    ["invalid_invite_code", "invalid_invite_code"],
    ["referral_requires_verified_email", "referral_requires_verified_email"],
    ["mail_unavailable", "mail_unavailable"],
    ["password_transport_requires_https", "password_transport_requires_https"],
    ["session_expired", "session_expired"],
    ["endpoint_unreachable", "endpoint_unreachable"],
    ["delivery_unavailable", "delivery_unavailable"],
  ];
  const code = aliases.find(([needle]) => normalized.includes(needle))?.[1];
  return code ? tr(`errors.codes.${code}`) : localizedError(error);
}
