import { useState } from "react";
import { ScrollText, ShieldCheck } from "lucide-react";
import { useTranslation } from "react-i18next";

import logo from "./assets/logo.png";
import { currentAppLanguage } from "./i18n";
import { ModalCloseButton } from "./ModalCloseButton";
import { useEscapeDismiss } from "./useEscapeDismiss";
import agreementDocument from "./legal/user-agreement.v1.json";
import agreementDocumentEnglish from "./legal/user-agreement.v1.en-US.json";
import legalManifest from "./legal/manifest.json";
import privacyDocument from "./legal/privacy-policy.v1.json";
import privacyDocumentEnglish from "./legal/privacy-policy.v1.en-US.json";

export type LegalDocumentId = "agreement" | "privacy";

export const LEGAL_EFFECTIVE_DATE = agreementDocument.effective_date;
export const USER_AGREEMENT_VERSION = legalManifest.agreement.version;
export const USER_AGREEMENT_SHA256 = legalManifest.agreement.content_sha256;
export const PRIVACY_POLICY_VERSION = legalManifest.privacy.version;
export const PRIVACY_POLICY_SHA256 = legalManifest.privacy.content_sha256;

type LegalNoticeProps = {
  mode: "gate" | "review";
  initialDocument?: LegalDocumentId;
  busy?: boolean;
  error?: string;
  onAccept?: () => void;
  onDecline?: () => void;
  onClose?: () => void;
};

export function LegalNotice({
  mode,
  initialDocument = "agreement",
  busy = false,
  error = "",
  onAccept,
  onDecline,
  onClose,
}: LegalNoticeProps) {
  const { t } = useTranslation();
  const [activeDocument, setActiveDocument] = useState<LegalDocumentId>(initialDocument);
  const isGate = mode === "gate";
  useEscapeDismiss(() => onClose?.(), !isGate && Boolean(onClose));
  const english = currentAppLanguage() === "en-US";
  const activeContent = activeDocument === "agreement"
    ? (english ? agreementDocumentEnglish : agreementDocument)
    : (english ? privacyDocumentEnglish : privacyDocument);
  const sections = activeContent.sections;
  const documentTitle = activeContent.title;
  const documentVersion = activeDocument === "agreement" ? USER_AGREEMENT_VERSION : PRIVACY_POLICY_VERSION;

  return (
    <div className={isGate ? "legal-notice-screen" : "modal-backdrop legal-notice-backdrop"}>
      <div
        className={`legal-notice-panel ${isGate ? "gate" : "review"}`}
        role="dialog"
        aria-modal="true"
        aria-labelledby="legal-notice-title"
        data-backdrop-dismissible="false"
        onClick={(event) => event.stopPropagation()}
      >
        <header className="legal-notice-header">
          <div className="legal-notice-brand">
            <img src={logo} alt="" aria-hidden="true" />
            <div>
              <h1 id="legal-notice-title">{t("legal.noticeTitle")}</h1>
              <p>
                {isGate
                  ? t("legal.gateIntro")
                  : t("legal.reviewIntro")}
              </p>
            </div>
          </div>
          {!isGate && onClose ? <ModalCloseButton onClick={onClose} /> : null}
        </header>

        <nav className="legal-document-tabs" aria-label={t("legal.tabsAria")} role="tablist">
          <button
            type="button"
            role="tab"
            className={activeDocument === "agreement" ? "active" : ""}
            aria-selected={activeDocument === "agreement"}
            onClick={() => setActiveDocument("agreement")}
          >
            <ScrollText size={16} aria-hidden="true" />
            {t("legal.viewAgreement")}
          </button>
          <button
            type="button"
            role="tab"
            className={activeDocument === "privacy" ? "active" : ""}
            aria-selected={activeDocument === "privacy"}
            onClick={() => setActiveDocument("privacy")}
          >
            <ShieldCheck size={16} aria-hidden="true" />
            {t("legal.viewPrivacy")}
          </button>
        </nav>

        <article className="legal-document" role="tabpanel" aria-labelledby="legal-document-heading">
          <div className="legal-document-intro">
            <div>
              <h2 id="legal-document-heading">CONST API {documentTitle}</h2>
              <p>{t("legal.documentMeta", {
                version: documentVersion,
                date: activeContent.effective_date,
              })}</p>
            </div>
            <p>{activeDocument === "agreement" ? t("legal.agreementFocus") : t("legal.privacyFocus")}</p>
          </div>
          <div className="legal-document-sections">
            {sections.map((section) => (
              <section key={section.title}>
                <h3>{section.title}</h3>
                {section.paragraphs.map((paragraph) => <p key={paragraph}>{paragraph}</p>)}
              </section>
            ))}
          </div>
        </article>

        <footer className="legal-notice-footer">
          <div className="legal-consent-copy">
            {isGate ? (
              <>
                <strong>{t("legal.consent")}</strong>
                <span>{t("legal.declineHint")}</span>
              </>
            ) : (
              <span>{t("legal.currentDocuments", {
                agreement: USER_AGREEMENT_VERSION,
                privacy: PRIVACY_POLICY_VERSION,
              })}</span>
            )}
            {error ? <span className="legal-notice-error" role="alert">{error}</span> : null}
          </div>
          <div className="actions legal-notice-actions">
            {isGate ? (
              <>
                <button className="quiet" type="button" disabled={busy} onClick={onDecline}>{t("legal.decline")}</button>
                <button className="primary" type="button" disabled={busy} onClick={onAccept}>
                  {busy ? t("legal.saving") : t("legal.accept")}
                </button>
              </>
            ) : (
              <button className="primary" type="button" onClick={onClose}>{t("common.close")}</button>
            )}
          </div>
        </footer>
      </div>
    </div>
  );
}

export function LegalNoticeLoading() {
  const { t } = useTranslation();
  return (
    <div className="legal-notice-screen legal-notice-loading" role="status" aria-label={t("legal.loadingAria")}>
      <img src={logo} alt="" aria-hidden="true" />
      <strong>CONST API</strong>
      <span>{t("legal.loadingStatus")}</span>
    </div>
  );
}
