import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, test, vi } from "vitest";

import agreementRaw from "./legal/user-agreement.v1.json?raw";
import privacyRaw from "./legal/privacy-policy.v1.json?raw";
import legalManifest from "./legal/manifest.json";
import { LegalNotice } from "./LegalNotice";

async function sha256(value: string) {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value));
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
}

describe("LegalNotice", () => {
  test("requires an explicit first-run decision and exposes both documents", () => {
    const onAccept = vi.fn();
    const onDecline = vi.fn();
    const { container } = render(
      <LegalNotice mode="gate" onAccept={onAccept} onDecline={onDecline} />,
    );

    const dialog = screen.getByRole("dialog", { name: "用户协议与隐私政策" });
    expect(dialog).toHaveAttribute("data-backdrop-dismissible", "false");
    expect(screen.getByRole("heading", { name: "CONST API 用户协议" })).toBeInTheDocument();
    expect(screen.getByText(/同意《用户协议》和《隐私政策》/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("tab", { name: "隐私政策" }));
    expect(screen.getByRole("heading", { name: "CONST API 隐私政策" })).toBeInTheDocument();
    expect(screen.getByText("三、网络传输与数据去向")).toBeInTheDocument();

    fireEvent.click(container.querySelector(".legal-notice-screen")!);
    expect(onAccept).not.toHaveBeenCalled();
    expect(onDecline).not.toHaveBeenCalled();

    fireEvent.keyDown(document, { key: "Escape" });
    expect(onAccept).not.toHaveBeenCalled();
    expect(onDecline).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "同意并继续" }));
    expect(onAccept).toHaveBeenCalledOnce();
  });

  test("opens the requested permanent document and closes explicitly", () => {
    const onClose = vi.fn();
    render(<LegalNotice mode="review" initialDocument="privacy" onClose={onClose} />);

    expect(screen.getByRole("heading", { name: "CONST API 隐私政策" })).toBeInTheDocument();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(onClose).toHaveBeenCalledOnce();
  });

  test("pins every version to the exact document content", async () => {
    const agreement = JSON.parse(agreementRaw) as { id: string; version: number };
    const privacy = JSON.parse(privacyRaw) as { id: string; version: number };

    expect(agreement.id).toBe(legalManifest.agreement.id);
    expect(agreement.version).toBe(legalManifest.agreement.version);
    expect(await sha256(agreementRaw)).toBe(legalManifest.agreement.content_sha256);
    expect(privacy.id).toBe(legalManifest.privacy.id);
    expect(privacy.version).toBe(legalManifest.privacy.version);
    expect(await sha256(privacyRaw)).toBe(legalManifest.privacy.content_sha256);
  });
});
