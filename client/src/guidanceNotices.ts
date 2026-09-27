export type GuidanceNoticeTone = "info" | "success" | "warning" | "error" | "decision" | "local";

export type GuidanceNoticePage = "access" | "supplier" | "account" | "about";
export type GuidanceNoticeScope = "global" | GuidanceNoticePage;

export type GuidanceNoticeIcon =
  | "access"
  | "account"
  | "connection"
  | "device"
  | "info"
  | "local"
  | "supplier"
  | "success"
  | "warning";

export type GuidanceNoticeAction = {
  id: string;
  label: string;
  emphasis?: "primary" | "secondary";
};

export type GuidanceNotice = {
  dedupeKey: string;
  scope?: GuidanceNoticeScope;
  tone: GuidanceNoticeTone;
  icon: GuidanceNoticeIcon;
  eyebrow?: string;
  title: string;
  body: string;
  footnote?: string;
  actions?: GuidanceNoticeAction[];
  dismissible?: boolean;
  persistentDismissalKey?: string;
};

export const MAX_ACTIVE_GUIDANCE_NOTICES = 20;
export const balanceInterruptionNoticeKey = "account:insufficient_balance";

export function upsertGuidanceNotice(
  current: GuidanceNotice[],
  notice: GuidanceNotice,
  limit = MAX_ACTIVE_GUIDANCE_NOTICES,
): GuidanceNotice[] {
  const withoutPreviousVersion = current.filter((item) => item.dedupeKey !== notice.dedupeKey);
  return [notice, ...withoutPreviousVersion].slice(0, Math.max(1, limit));
}

export function dismissGuidanceNotice(
  current: GuidanceNotice[],
  dedupeKey: string,
): GuidanceNotice[] {
  return current.filter((notice) => notice.dedupeKey !== dedupeKey);
}

export function guidanceNoticeVisibleOnPage(
  notice: GuidanceNotice,
  page: GuidanceNoticePage,
): boolean {
  return !notice.scope || notice.scope === "global" || notice.scope === page;
}

export function activeGuidanceNotice(
  current: GuidanceNotice[],
  page: GuidanceNoticePage,
): GuidanceNotice | null {
  return current.find((notice) => guidanceNoticeVisibleOnPage(notice, page)) ?? null;
}
