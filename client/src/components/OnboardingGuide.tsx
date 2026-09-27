import { useTranslation } from "react-i18next";

import type { AccountFundsEntryReason } from "../account/accountTypes";
import type { GuidanceNotice, GuidanceNoticeIcon, GuidanceNoticeTone } from "../guidanceNotices";
import type { OnboardingGuideState } from "../onboarding";
import { GuidanceNoticeBubble, GuidanceNoticeCard } from "./GuidanceNoticeCard";

export function OnboardingGuide({
  variant = "inline",
  state,
  balanceLabel,
  collapsed = false,
  onRegister,
  onChooseLocal,
  onAddModelChannel,
  onReviewModelChannel,
  onOpenFunds,
  onReturnToUse,
  onCollapse,
  onExpand,
}: {
  variant?: "inline" | "tray";
  state: OnboardingGuideState;
  balanceLabel: string;
  collapsed?: boolean;
  onRegister(): void;
  onChooseLocal(): void;
  onAddModelChannel(): void;
  onReviewModelChannel(): void;
  onOpenFunds(reason: AccountFundsEntryReason): void;
  onReturnToUse?(): void;
  onCollapse?(): void;
  onExpand?(): void;
}) {
  const { t } = useTranslation();
  const presentation = {
    choose_mode: {
      tone: "decision",
      icon: "access",
      eyebrow: t("onboarding.registrationRecommendation.eyebrow"),
      title: t("onboarding.registrationRecommendation.title"),
      body: t("onboarding.registrationRecommendation.body"),
    },
    recommend_registration: {
      tone: "decision",
      icon: "access",
      eyebrow: t("onboarding.registrationRecommendation.eyebrow"),
      title: t("onboarding.registrationRecommendation.title"),
      body: t("onboarding.registrationRecommendation.body"),
    },
    local_setup: {
      tone: "local",
      icon: "local",
      eyebrow: t("onboarding.local.eyebrow"),
      title: t("onboarding.local.title"),
      body: t("onboarding.local.body"),
    },
    local_channel_setup: {
      tone: "local",
      icon: "local",
      eyebrow: t("onboarding.localChannel.eyebrow"),
      title: t("onboarding.localChannel.title"),
      body: t("onboarding.localChannel.body"),
    },
    local_ready: {
      tone: "success",
      icon: "device",
      eyebrow: t("onboarding.localReady.eyebrow"),
      title: t("onboarding.localReady.title"),
      body: t("onboarding.localReady.body"),
    },
    needs_balance: {
      tone: "warning",
      icon: "account",
      eyebrow: t("onboarding.balance.eyebrow"),
      title: t("onboarding.balance.title"),
      body: t("onboarding.balance.body"),
    },
    low_balance: {
      tone: "warning",
      icon: "account",
      eyebrow: t("onboarding.lowBalance.eyebrow"),
      title: t("onboarding.lowBalance.title"),
      body: t("onboarding.lowBalance.body", {
        balance: balanceLabel,
      }),
    },
    ready_to_use: {
      tone: "info",
      icon: "connection",
      eyebrow: t("onboarding.usage.eyebrow"),
      title: t("onboarding.usage.title"),
      body: t("onboarding.usage.body"),
    },
    supplier_intro: {
      tone: "info",
      icon: "supplier",
      eyebrow: t("onboarding.supplier.eyebrow"),
      title: t("onboarding.supplier.offlineTitle"),
      body: t("onboarding.supplier.offlineBody"),
      footnote: t("onboarding.supplier.requirements"),
    },
  } satisfies Record<NonNullable<OnboardingGuideState>, {
    tone: GuidanceNoticeTone;
    icon: GuidanceNoticeIcon;
    eyebrow: string;
    title: string;
    body: string;
    footnote?: string;
  }>;
  const actions: GuidanceNotice["actions"] = state === "choose_mode"
    ? [
        { id: "register", label: t("onboarding.registrationRecommendation.registerAction") },
        { id: "choose_local", label: t("onboarding.registrationRecommendation.localAction"), emphasis: "secondary" },
      ]
      : state === "recommend_registration"
        ? [{ id: "register", label: t("onboarding.registrationRecommendation.registerAction") }]
        : state === "local_setup"
          ? [
              { id: "add_model_channel", label: t("onboarding.local.action") },
              { id: "register", label: t("onboarding.registrationRecommendation.registerAction"), emphasis: "secondary" },
            ]
          : state === "local_channel_setup"
            ? [
                { id: "review_model_channel", label: t("onboarding.localChannel.action") },
                { id: "register", label: t("onboarding.registrationRecommendation.registerAction"), emphasis: "secondary" },
              ]
            : state === "local_ready" && onReturnToUse
              ? [{ id: "return_to_use", label: t("onboarding.localReady.action") }]
              : state === "needs_balance"
                ? [{ id: "open_onboarding_funds", label: t("onboarding.balance.action") }]
                : state === "low_balance"
                  ? [{ id: "open_low_balance_funds", label: t("onboarding.lowBalance.action") }]
                  : state === "supplier_intro"
                    ? [{ id: "add_model_channel", label: t("onboarding.supplier.action") }]
                    : [];
  const notice: GuidanceNotice = {
    dedupeKey: `onboarding-guide:${state}`,
    ...presentation[state],
    actions: actions.filter(action => action.id !== "register"),
    dismissible: variant === "tray",
  };

  if (variant === "tray" && collapsed) {
    return <GuidanceNoticeBubble
      htmlId="onboarding-guide"
      notice={notice}
      expandLabel={t("notifications.expand", { title: notice.title })}
      onExpand={() => onExpand?.()}
    />;
  }

  return <GuidanceNoticeCard
    variant={variant}
    htmlId="onboarding-guide"
    notice={notice}
    semanticRole="region"
    dismissLabel={t("notifications.minimize")}
    dismissIcon="minimize"
    onAction={(actionId) => {
      if (actionId === "register") onRegister();
      else if (actionId === "choose_local") onChooseLocal();
      else if (actionId === "add_model_channel") onAddModelChannel();
      else if (actionId === "review_model_channel") onReviewModelChannel();
      else if (actionId === "return_to_use") onReturnToUse?.();
      else if (actionId === "open_onboarding_funds") onOpenFunds("onboarding");
      else if (actionId === "open_low_balance_funds") onOpenFunds("low_balance");
    }}
    onDismiss={onCollapse}
  />;
}
