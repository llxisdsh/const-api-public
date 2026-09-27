import { invoke } from "@tauri-apps/api/core";

import type {
  AccountChallenge,
  AccountRegistrationPolicy,
  AccountStatus,
  BillingBucket,
  BillingFilters,
  BillingPage,
  BillingRecord,
  BillingSummary,
  FinanceAvailability,
  PaymentOrder,
  PayoutAccount,
  Refund,
  UsageGift,
  UsageGiftList,
  UsageGiftRecipient,
  Withdrawal,
} from "./accountTypes";

export type CreatePaymentInput = {
  amountUSD: number;
  amountCNYMinor: number;
  paymentType: "alipay";
};

export interface AccountFinanceApi {
  reauthenticate(password: string): Promise<void>;
  availability(): Promise<FinanceAvailability>;
  paymentOrders(): Promise<PaymentOrder[]>;
  refunds(): Promise<Refund[]>;
  createPayment(input: CreatePaymentInput, requestId: string): Promise<PaymentOrder>;
  refreshPayment(id: string): Promise<PaymentOrder>;
  cancelPayment(id: string, requestId: string): Promise<PaymentOrder>;
  completeFakePayment(id: string): Promise<PaymentOrder>;
  payoutAccounts(): Promise<PayoutAccount[]>;
  createPayoutAccount(label: string, accountHolderName: string, alipayAccount: string, requestId: string): Promise<PayoutAccount>;
  withdrawals(): Promise<Withdrawal[]>;
  usageTransfers(): Promise<BillingRecord[]>;
  createWithdrawal(amount: number, payoutAccountId: string, requestId: string): Promise<Withdrawal>;
  cancelWithdrawal(id: string): Promise<Withdrawal>;
  usageGifts(): Promise<UsageGiftList>;
  previewGiftRecipient(identifier: string): Promise<UsageGiftRecipient>;
  createUsageGift(recipientToken: string, amountUSD: number, note: string, requestId: string): Promise<UsageGift>;
}

export interface AccountApi {
  status(): Promise<AccountStatus>;
  refresh(): Promise<AccountStatus>;
  registrationPolicy?(): Promise<AccountRegistrationPolicy>;
  requestCode(email: string, purpose: "login" | "reset_password", platformId?: string): Promise<AccountChallenge>;
  verifyCode(challengeId: string, email: string, code: string): Promise<AccountStatus>;
  register(identifier: string, password: string, challengeId?: string, code?: string, inviteCode?: string): Promise<AccountStatus | AccountChallenge>;
  passwordLogin(identifier: string, password: string): Promise<AccountStatus>;
  resetPassword(challengeId: string, email: string, code: string, newPassword: string, platformId?: string): Promise<AccountStatus>;
  changePassword(currentPassword: string, newPassword: string): Promise<AccountStatus>;
  requestEmailCode(email: string): Promise<AccountChallenge>;
  verifyEmail(challengeId: string, email: string, code: string): Promise<AccountStatus>;
  logout(): Promise<AccountStatus>;
  billingSummary(): Promise<BillingSummary>;
  billingPage(filters: BillingFilters): Promise<BillingPage>;
  billingStats(): Promise<BillingBucket[]>;
  billingDetail(id: string): Promise<BillingRecord>;
  transferSupplierFunds(amount: number, requestId: string): Promise<BillingSummary>;
}

export const tauriAccountApi: AccountApi = {
  status: () => invoke<AccountStatus>("account_status"),
  refresh: () => invoke<AccountStatus>("account_refresh"),
  registrationPolicy: () => invoke<AccountRegistrationPolicy>("account_registration_policy"),
  requestCode: (email, purpose, platformId) => invoke<AccountChallenge>("account_request_code", {
    email, purpose, ...(platformId ? { platformId } : {}),
  }),
  verifyCode: (challengeId, email, code) => invoke<AccountStatus>("account_verify_code", { challengeId, email, code }),
  register: (identifier, password, challengeId, code, inviteCode) => invoke<AccountStatus | AccountChallenge>("account_register", {
    identifier,
    password,
    ...(challengeId ? { challengeId } : {}),
    ...(code ? { code } : {}),
    ...(inviteCode ? { inviteCode } : {}),
  }),
  passwordLogin: (identifier, password) => invoke<AccountStatus>("account_password_login", { identifier, password }),
  resetPassword: (challengeId, email, code, newPassword, platformId) => invoke<AccountStatus>("account_reset_password", {
    challengeId,
    email,
    code,
    newPassword,
    ...(platformId ? { platformId } : {}),
  }),
  changePassword: (currentPassword, newPassword) => invoke<AccountStatus>("account_change_password", {
    currentPassword,
    newPassword,
  }),
  requestEmailCode: (email) => invoke<AccountChallenge>("account_request_email_code", { email }),
  verifyEmail: (challengeId, email, code) => invoke<AccountStatus>("account_verify_email", { challengeId, email, code }),
  logout: () => invoke<AccountStatus>("account_logout"),
  billingSummary: () => invoke<BillingSummary>("account_billing_summary"),
  billingPage: async (filters) => {
    const page = await invoke<{ data?: BillingRecord[]; next_cursor?: string; has_more?: boolean; older_archived?: boolean }>("account_billing_records", {
      direction: filters.direction || null,
      flow: filters.flow || null,
      status: filters.status || null,
      model: filters.model.trim() || null,
      cursor: filters.cursor || null,
      limit: filters.limit,
    });
    const nextCursor = page.next_cursor || undefined;
    return {
      items: page.data ?? [],
      next_cursor: nextCursor,
      has_more: page.has_more ?? Boolean(nextCursor),
      older_archived: page.older_archived === true,
    };
  },
  billingStats: async () => {
    const response = await invoke<{ data?: BillingBucket[] }>("account_billing_stats", { days: 30 });
    return response.data ?? [];
  },
  billingDetail: (id) => invoke<BillingRecord>("account_billing_detail", { id }),
  transferSupplierFunds: (amount, requestId) => invoke<BillingSummary>("account_transfer_funds", { amount, requestId }),
};

export const tauriAccountFinanceApi: AccountFinanceApi = {
  reauthenticate: async (password) => { await invoke("account_reauthenticate", { password }); },
  availability: () => invoke<FinanceAvailability>("account_finance_availability"),
  paymentOrders: async () => (await invoke<{ data?: PaymentOrder[] }>("account_payment_orders")).data ?? [],
  refunds: async () => (await invoke<{ data?: Refund[] }>("account_refunds")).data ?? [],
  createPayment: (input, requestId) => invoke<PaymentOrder>("account_create_payment", {
    amountUsd: input.amountUSD,
    amountCnyMinor: input.amountCNYMinor,
    paymentType: input.paymentType,
    requestId,
  }),
  refreshPayment: (id) => invoke<PaymentOrder>("account_refresh_payment", { id }),
  cancelPayment: (id, requestId) => invoke<PaymentOrder>("account_cancel_payment", { id, requestId }),
  completeFakePayment: (id) => invoke<PaymentOrder>("account_complete_fake_payment", { id }),
  payoutAccounts: async () => (await invoke<{ data?: PayoutAccount[] }>("account_payout_accounts")).data ?? [],
  createPayoutAccount: (label, accountHolderName, alipayAccount, requestId) => invoke<PayoutAccount>("account_create_payout_account", {
    label,
    accountHolderName,
    alipayAccount,
    requestId,
  }),
  withdrawals: async () => (await invoke<{ data?: Withdrawal[] }>("account_withdrawals")).data ?? [],
  usageTransfers: async () => {
    const records = (await invoke<{ data?: BillingRecord[] }>("account_billing_records", {
      direction: "usage",
      flow: "transfer",
      status: null,
      model: null,
      cursor: null,
      limit: 20,
    })).data ?? [];
    // Older desktop/server processes ignore the new `flow` argument during a
    // rolling upgrade. Never let their broad usage statement masquerade as
    // transfer history in the renderer.
    return records.filter((record) => record.direction === "usage"
      && record.flow === "transfer"
      && record.entry_type === "supplier_transfer");
  },
  createWithdrawal: (amount, payoutAccountId, requestId) => invoke<Withdrawal>("account_create_withdrawal", { amount, payoutAccountId, requestId }),
  cancelWithdrawal: (id) => invoke<Withdrawal>("account_cancel_withdrawal", { id }),
  usageGifts: () => invoke<UsageGiftList>("account_usage_gifts"),
  previewGiftRecipient: (identifier) => invoke<UsageGiftRecipient>("account_preview_gift_recipient", { identifier }),
  createUsageGift: (recipientToken, amountUSD, note, requestId) => invoke<UsageGift>("account_create_usage_gift", {
    recipientToken,
    amount: amountUSD,
    note,
    requestId,
  }),
};
