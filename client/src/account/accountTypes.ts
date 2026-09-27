export type AccountStatus = {
  state: "signed_out" | "refreshing" | "signed_in" | "expired";
  endpoint_state: "checking" | "online" | "offline";
  platform_id: string;
  user_id: string;
  email: string;
  username: string;
  role: "user" | "admin" | "";
  email_verified: boolean;
  must_change_password: boolean;
  balance?: number;
  display_cny_per_usd?: number;
  access_expires_at: string;
  credential_persistence: "none" | "file" | "process_only";
  legal_state: "not_applicable" | "checking" | "accepted" | "pending" | "grace_period" | "update_required" | "local_acceptance_required" | "server_unsupported";
  legal_required_agreement_version: number;
  legal_required_privacy_version: number;
  last_error: string;
};

export type AccountChallenge = {
  challenge_id: string;
  expires_in: number;
};

export type AccountRegistrationPolicy = {
  registration_enabled: boolean;
  registration_routing_available?: boolean;
  password_login_enabled: boolean;
  email_login_configured: boolean;
  verified_email_registration_required: boolean;
  bootstrap_admin_password_login_always_enabled: boolean;
  referral_signup_reward_usd?: number;
  display_cny_per_usd?: number;
};

export type ReferralOverview = {
  invite_code: string;
  partner_status?: "not_activated" | "active" | "paused";
  signup_reward_usd: number;
  level1_share_rate: number;
  level2_share_rate: number;
  direct_invites: number;
  paid_invites: number;
  level1_commission_usd: number;
  level2_commission_usd: number;
  total_commission_usd: number;
};

export type BillingSummary = {
  usage_balance: number;
  supplier_funds: { pending: number; available: number; held?: number; withdrawn: number };
  display_cny_per_usd?: number;
  ledger_currency?: "USD";
};

export type FinanceAvailability = {
  topups_enabled: boolean;
  topups_policy_enabled?: boolean;
  payment_provider: string;
  payment_provider_ready?: boolean;
  topup_cny_per_usd: number;
  minimum_topup_cny_minor: number;
  maximum_topup_cny_minor: number;
  maximum_open_topups_per_user?: number;
  withdrawals_enabled: boolean;
  payout_provider: string;
  settlement_share_rate: number;
  settlement_hold_hours: number;
  display_cny_per_usd?: number;
  withdrawal_fee_rate: number;
  minimum_withdrawal_usd: number;
  maximum_withdrawal_usd: number;
  daily_withdrawal_limit_usd: number;
  payout_account_cooldown_hours: number;
  currency: "USD";
  payout_account_kinds: Array<"alipay">;
  payment_methods: Array<"alipay">;
  fake_payment: boolean;
  usage_gifts_enabled: boolean;
  minimum_gift_usd: number;
  maximum_gift_usd: number;
  daily_gift_limit_usd: number;
};

export type PaymentOrder = {
  id: string;
  provider: string;
  currency: string;
  provider_currency?: "USD" | "CNY";
  provider_amount_minor?: number;
  exchange_cny_per_usd?: number;
  payment_type?: "alipay";
  amount_usd: number;
  credit_usd: number;
  refunded_usd: number;
  status:
    | "CREATED"
    | "PENDING"
    | "PENDING_RECONCILIATION"
    | "COMPLETED"
    | "REFUNDED"
    | "FAILED"
    | "CANCELLING"
    | "CANCELLED"
    | "EXPIRING"
    | "EXPIRED"
    | "MANUAL_REVIEW";
  checkout_url?: string;
  qr_code_content?: string;
  qr_code_image_url?: string;
  failure_code?: string;
  created_at: string;
  paid_at?: string;
  completed_at?: string;
  expires_at?: string;
  updated_at?: string;
};

export type Refund = {
  id: string;
  order_id: string;
  amount_usd: number;
  status:
    | "REQUESTED"
    | "VALIDATING"
    | "SUBMITTED"
    | "PENDING_RECONCILIATION"
    | "COMPLETED"
    | "FAILED"
    | "REJECTED"
    | "CANCELLED"
    | "MANUAL_REVIEW";
  created_at: string;
  submitted_at?: string;
  completed_at?: string;
  updated_at?: string;
};

export type PayoutAccount = {
  id: string;
  kind: "alipay";
  label: string;
  currency: string;
  account_holder_masked: string;
  destination_masked: string;
  status: string;
  usable_after: string;
  created_at: string;
};

export type Withdrawal = {
  id: string;
  payout_account_id: string;
  payout_kind: string;
  payout_label: string;
  account_holder_masked: string;
  destination_masked: string;
  provider: string;
  provider_payout_id?: string;
  gross_usd: number;
  fee_rate: number;
  fee_usd: number;
  provider_cost_usd: number;
  net_usd: number;
  status: "REVIEWING" | "APPROVED" | "PROCESSING" | "PAID" | "REJECTED" | "CANCELLED" | "FAILED" | "PAYOUT_UNKNOWN";
  reason?: string;
  requested_at: string;
  paid_at?: string;
};

export type UsageGift = {
  id: string;
  direction: "sent" | "received";
  counterparty: string;
  amount_usd: number;
  note?: string;
  status: "COMPLETED";
  created_at: string;
};

export type UsageGiftRecipient = {
  recipient_token: string;
  recipient_masked: string;
  expires_at: string;
};

export type UsageGiftList = {
  data: UsageGift[];
  giftable_usage_balance: number;
};

export type BillingRecord = {
  id: string;
  direction: "usage" | "supply";
  flow?: "credit" | "debit" | "transfer" | "hold" | "release" | "payout" | "adjustment";
  created_at: string;
  status: string;
  amount: number;
  balance_after?: number;
  request_id?: string;
  model?: string;
  input_tokens?: number;
  output_tokens?: number;
  supplier_unit_id?: string;
  entry_type?: string;
};

export type BillingBucket = {
  date: string;
  direction: "usage" | "supply";
  requests: number;
  amount: number;
};

export type BillingFilters = {
  direction: "" | "usage" | "supply";
  flow?: "credit" | "debit" | "transfer" | "hold" | "release" | "payout" | "adjustment";
  status: string;
  model: string;
  cursor?: string;
  limit: number;
};

export type BillingPage = {
  items: BillingRecord[];
  next_cursor?: string;
  has_more: boolean;
  older_archived?: boolean;
};

export type AccountAuthMode = "password" | "code" | "register" | "reset";

export type AccountView = "overview" | "funds" | "billing" | "referral" | "settings";

export type AccountFundsEntryReason = "onboarding" | "low_balance" | "insufficient_balance";

export type AccountViewRequest = {
  view: AccountView;
  token: number;
  reason?: AccountFundsEntryReason;
};

export type AccountNotice = {
  tone: "info" | "success" | "error";
  text: string;
};

export function emptyAccountStatus(): AccountStatus {
  return {
    state: "signed_out",
    endpoint_state: "checking",
    platform_id: "",
    user_id: "",
    email: "",
    username: "",
    role: "",
    email_verified: false,
    must_change_password: false,
    access_expires_at: "",
    credential_persistence: "none",
    legal_state: "not_applicable",
    legal_required_agreement_version: 0,
    legal_required_privacy_version: 0,
    last_error: "",
  };
}

export function accountDisplayName(status: AccountStatus): string {
  return status.username.trim() || status.email.trim() || "\u672a\u767b\u5f55";
}
