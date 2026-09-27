import {
  AlertTriangle,
  CheckCircle2,
  CircleDollarSign,
  CloudUpload,
  Cpu,
  Info,
  KeyRound,
  Laptop,
  Minus,
  Plug,
  X,
  type LucideIcon,
} from "lucide-react";

import type { GuidanceNotice, GuidanceNoticeIcon } from "../guidanceNotices";

const noticeIcons: Record<GuidanceNoticeIcon, LucideIcon> = {
  access: KeyRound,
  account: CircleDollarSign,
  connection: Plug,
  device: Laptop,
  info: Info,
  local: Cpu,
  supplier: CloudUpload,
  success: CheckCircle2,
  warning: AlertTriangle,
};

export function GuidanceNoticeCard({
  notice,
  variant,
  htmlId,
  dismissLabel,
  dismissIcon = "close",
  semanticRole,
  onAction,
  onDismiss,
}: {
  notice: GuidanceNotice;
  variant: "inline" | "tray";
  htmlId?: string;
  dismissLabel?: string;
  dismissIcon?: "close" | "minimize";
  semanticRole?: "region" | "status" | "alert";
  onAction?(actionId: string): void;
  onDismiss?(): void;
}) {
  const Icon = noticeIcons[notice.icon];
  const isTray = variant === "tray";
  const canDismiss = notice.dismissible && Boolean(onDismiss);
  const orderedActions = notice.actions
    ? [...notice.actions].sort((left, right) => (
        Number(left.emphasis !== "secondary") - Number(right.emphasis !== "secondary")
      ))
    : [];

  return (
    <article
      id={htmlId}
      className={`guidance-notice guidance-notice-${variant} ${notice.tone}${isTray ? (canDismiss ? " is-dismissible" : "") : " onboarding-callout"}`}
      role={semanticRole ?? (isTray ? (notice.tone === "error" ? "alert" : "status") : "region")}
      aria-label={notice.title}
      tabIndex={htmlId ? -1 : undefined}
    >
      <span className="onboarding-callout-icon"><Icon aria-hidden="true" size={isTray ? 19 : 21} /></span>
      <div className="onboarding-callout-copy">
        {notice.eyebrow ? <span className="onboarding-callout-eyebrow">{notice.eyebrow}</span> : null}
        <h2>{notice.title}</h2>
        <p>{notice.body}</p>
        {notice.footnote ? <small className="onboarding-facts">{notice.footnote}</small> : null}
      </div>
      {orderedActions.length ? (
        <div className="onboarding-callout-actions guidance-notice-actions">
          {orderedActions.map((action) => (
            <button
              key={action.id}
              className={action.emphasis === "secondary" ? "quiet" : "primary"}
              type="button"
              onClick={() => onAction?.(action.id)}
            >
              {action.label}
            </button>
          ))}
        </div>
      ) : null}
      {canDismiss ? (
        <button
          className="guidance-notice-dismiss"
          type="button"
          aria-label={dismissLabel}
          title={dismissLabel}
          onClick={onDismiss}
        >
          {dismissIcon === "minimize"
            ? <Minus aria-hidden="true" size={17} />
            : <X aria-hidden="true" size={17} />}
        </button>
      ) : null}
    </article>
  );
}

export function GuidanceNoticeBubble({
  notice,
  htmlId,
  expandLabel,
  onExpand,
}: {
  notice: GuidanceNotice;
  htmlId?: string;
  expandLabel: string;
  onExpand(): void;
}) {
  const Icon = noticeIcons[notice.icon];

  return (
    <button
      id={htmlId}
      className={`guidance-notice-bubble ${notice.tone}`}
      type="button"
      aria-label={expandLabel}
      title={expandLabel}
      onClick={onExpand}
    >
      <Icon aria-hidden="true" size={19} />
    </button>
  );
}
