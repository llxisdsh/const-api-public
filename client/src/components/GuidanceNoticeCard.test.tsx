import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { GuidanceNoticeBubble, GuidanceNoticeCard } from "./GuidanceNoticeCard";

describe("GuidanceNoticeCard", () => {
  it("keeps a tray notice visible after its action and dismisses only from close", () => {
    const onAction = vi.fn();
    const onDismiss = vi.fn();
    render(<GuidanceNoticeCard
      variant="tray"
      notice={{
        dedupeKey: "balance",
        tone: "warning",
        icon: "account",
        title: "Insufficient balance",
        body: "Finish what you are doing, then top up.",
        actions: [
          { id: "topup", label: "Top up" },
          { id: "later", label: "Later", emphasis: "secondary" },
        ],
        dismissible: true,
      }}
      dismissLabel="Close notification"
      onAction={onAction}
      onDismiss={onDismiss}
    />);

    fireEvent.click(screen.getByRole("button", { name: "Top up" }));
    expect(onAction).toHaveBeenCalledWith("topup");
    expect(screen.getByText("Insufficient balance")).toBeInTheDocument();
    expect(onDismiss).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Later" })).toHaveClass("quiet");
    const noticeRegion = screen.getByRole("status", { name: "Insufficient balance" });
    expect(noticeRegion).toHaveClass("is-dismissible");
    expect(Array.from(noticeRegion.querySelectorAll(".guidance-notice-actions button"))
      .map((button) => button.textContent)).toEqual(["Later", "Top up"]);
    expect(screen.getByRole("button", { name: "Close notification" }).querySelector(".lucide-x"))
      .toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Close notification" }));
    expect(onDismiss).toHaveBeenCalledTimes(1);
  });

  it("uses the same card structure for an inline onboarding guide", () => {
    render(<GuidanceNoticeCard
      variant="inline"
      htmlId="usage-readiness-guide"
      notice={{
        dedupeKey: "mode",
        tone: "decision",
        icon: "account",
        eyebrow: "Before you start",
        title: "Choose a mode",
        body: "Sign in or stay local.",
      }}
    />);

    expect(screen.getByRole("region", { name: "Choose a mode" }))
      .toHaveClass("onboarding-callout", "guidance-notice-inline");
  });

  it("uses a minus icon when the action minimizes instead of closing", () => {
    render(<GuidanceNoticeCard
      variant="tray"
      notice={{
        dedupeKey: "onboarding-guide:recommend_registration",
        tone: "decision",
        icon: "access",
        title: "Choose a mode",
        body: "Sign in or stay local.",
        dismissible: true,
      }}
      dismissLabel="Minimize notice"
      dismissIcon="minimize"
      onDismiss={() => undefined}
    />);

    expect(screen.getByRole("button", { name: "Minimize notice" }).querySelector(".lucide-minus"))
      .toBeInTheDocument();
  });

  it("does not announce a persistent onboarding decision as a new status", () => {
    render(<GuidanceNoticeCard
      variant="tray"
      semanticRole="region"
      notice={{
        dedupeKey: "onboarding-guide:recommend_registration",
        tone: "decision",
        icon: "access",
        title: "Choose a mode",
        body: "Sign in or stay local.",
      }}
    />);

    expect(screen.getByRole("region", { name: "Choose a mode" })).toBeInTheDocument();
    expect(screen.queryByRole("status", { name: "Choose a mode" })).not.toBeInTheDocument();
  });

  it("restores a minimized guide from its compact icon", () => {
    const onExpand = vi.fn();
    render(<GuidanceNoticeBubble
      notice={{
        dedupeKey: "onboarding-guide:low_balance",
        tone: "warning",
        icon: "account",
        title: "Low balance",
        body: "Top up soon.",
      }}
      expandLabel="Expand notice: Low balance"
      onExpand={onExpand}
    />);

    const bubble = screen.getByRole("button", { name: "Expand notice: Low balance" });
    expect(bubble).toHaveClass("guidance-notice-bubble", "warning");
    fireEvent.click(bubble);
    expect(onExpand).toHaveBeenCalledTimes(1);
  });
});
