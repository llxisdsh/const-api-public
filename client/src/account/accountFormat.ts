import { currentAppLanguage, tr } from "../i18n";
import type { BillingRecord } from "./accountTypes";

function formatMoneyNumber(value: number) {
  return value.toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 6 });
}

export const DEFAULT_DISPLAY_CNY_PER_USD = 7;

function usableCNYRate(value?: number): value is number {
  return value !== undefined && Number.isFinite(value) && value > 0;
}

function resolvedCNYRate(cnyPerUSD?: number): number {
  return usableCNYRate(cnyPerUSD) ? cnyPerUSD : DEFAULT_DISPLAY_CNY_PER_USD;
}

export function accountDisplayCurrency(_cnyPerUSD?: number): "CNY" | "USD" {
  return currentAppLanguage() === "zh-CN" ? "CNY" : "USD";
}

export function accountDisplayAmountFromUSD(value: number, cnyPerUSD?: number): number {
  return accountDisplayCurrency(cnyPerUSD) === "CNY" ? value * resolvedCNYRate(cnyPerUSD) : value;
}

export function accountDisplayAmountToUSD(value: number, cnyPerUSD?: number): number {
  return accountDisplayCurrency(cnyPerUSD) === "CNY" ? value / resolvedCNYRate(cnyPerUSD) : value;
}

export function parseCNYMinor(value: string): number | null {
  const match = /^(\d{1,12})(?:\.(\d{1,2}))?$/.exec(value.trim());
  if (!match) return null;
  const whole = Number(match[1]);
  const fraction = Number((match[2] ?? "").padEnd(2, "0"));
  const minor = whole * 100 + fraction;
  return Number.isSafeInteger(minor) ? minor : null;
}

export function formatAccountMoney(value?: number, cnyPerUSD?: number) {
  if (value === undefined || !Number.isFinite(value)) return "-";
  const currency = accountDisplayCurrency(cnyPerUSD);
  const displayed = accountDisplayAmountFromUSD(value, cnyPerUSD);
  const sign = value > 0 ? "+" : "";
  return `${sign}${currency === "CNY" ? "¥" : "$"}${formatMoneyNumber(displayed)}`;
}

export function formatAccountUnsignedMoney(value?: number, cnyPerUSD?: number) {
  if (value === undefined || !Number.isFinite(value)) return "-";
  const currency = accountDisplayCurrency(cnyPerUSD);
  const displayed = accountDisplayAmountFromUSD(Math.abs(value), cnyPerUSD);
  return `${currency === "CNY" ? "¥" : "$"}${formatMoneyNumber(displayed)}`;
}

export function accountEntryTypeLabel(value?: string) {
  if (value === "billing") return tr("account.billing.usageCharge");
  if (value === "topup") return tr("account.billing.topup");
  if (value === "topup_refund") return tr("account.billing.topupRefund");
  if (value === "topup_refund_hold") return tr("account.billing.refundHold");
  if (value === "topup_refund_release") return tr("account.billing.refundRelease");
  if (value === "topup_refund_rehold") return tr("account.billing.refundRehold");
  if (value === "supplier_penalty") return tr("account.billing.penaltyCompensation");
  if (value === "supplier_transfer") return tr("account.billing.transferReceived");
  if (value === "referral_signup_reward") return tr("account.billing.referralSignupReward");
  if (value === "referral_commission") return tr("account.billing.referralCommission");
  if (value === "referral_commission_accrual") return tr("account.billing.referralCommissionAccrual");
  if (value === "admin_adjustment") return tr("account.billing.balanceAdjustment");
  if (value === "opening_balance" || value === "legacy_backfill") {
    return tr("account.billing.openingBalance");
  }
  if (value === "accrual") return tr("account.billing.accrual");
  if (value === "release") return tr("account.billing.earningsRelease");
  if (value === "penalty") return tr("account.billing.supplierPenalty");
  if (value === "transfer_to_usage") return tr("account.billing.transferToUsage");
  if (value === "withdrawal_hold") return tr("account.billing.withdrawalHold");
  if (value === "withdrawal_release") return tr("account.billing.withdrawalRelease");
  if (value === "withdrawal_complete") return tr("account.billing.withdrawalComplete");
  if (value === "refund_hold") return tr("account.billing.supplierRefundHold");
  if (value === "refund_release") return tr("account.billing.supplierRefundRelease");
  if (value === "chargeback") return tr("account.billing.chargeback");
  if (value === "reversal") return tr("account.billing.reversal");
  if (value === "correction") return tr("account.billing.correction");
  return value ? tr("account.billing.other") : "-";
}

export function formatAccountBillingAmount(
  record: Pick<BillingRecord, "amount" | "flow">,
  cnyPerUSD?: number,
) {
  if (record.flow === "transfer" || record.flow === "hold" ||
    record.flow === "release" || record.flow === "payout") {
    return formatAccountUnsignedMoney(record.amount, cnyPerUSD);
  }
  return formatAccountMoney(record.amount, cnyPerUSD);
}

const accountDateTimeFormatter = new Intl.DateTimeFormat("zh-CN", {
  year: "numeric",
  month: "2-digit",
  day: "2-digit",
  hour: "2-digit",
  minute: "2-digit",
  second: "2-digit",
  hour12: false,
});

export function formatAccountDateTime(value?: string) {
  if (!value) return "-";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  const parts = Object.fromEntries(
    accountDateTimeFormatter.formatToParts(date).map((part) => [part.type, part.value]),
  );
  return `${parts.year}-${parts.month}-${parts.day} ${parts.hour}:${parts.minute}:${parts.second}`;
}
