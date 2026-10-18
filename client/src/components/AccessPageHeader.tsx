import { BookOpen, Settings } from "lucide-react";
import { useTranslation } from "react-i18next";

type AccessPageHeaderProps = {
  running: boolean;
  issue?: string;
  settingsOpen: boolean;
  onOpenGuide: () => void;
  onOpenSettings: () => void;
  onOpenMarketPrices: () => void;
  onOpenUsageLog: () => void;
};

export function AccessPageHeader({
  running,
  issue,
  settingsOpen,
  onOpenGuide,
  onOpenSettings,
  onOpenMarketPrices,
  onOpenUsageLog,
}: AccessPageHeaderProps) {
  const { t } = useTranslation();

  return (
    <header className="page-header-with-actions">
      <div className="page-header-topline">
        <div className="access-header-heading">
          <h1>{t("access.title")}</h1>
          <span
            className={`access-api-status ${running ? "running" : "stopped"}`}
            role="status"
            title={t(running ? "access.runningStatus" : "access.stoppedStatus")}
          >
            <span className="local-entry-state-dot" aria-hidden="true" />
            {t(running ? "access.apiRunning" : "access.apiStopped")}
          </span>
        </div>
        <div className="actions">
          <button className="quiet" type="button" onClick={onOpenGuide}>
            <BookOpen size={16} />{t("access.connectionGuide")}
          </button>
          <button
            className="quiet"
            type="button"
            aria-haspopup="dialog"
            aria-expanded={settingsOpen}
            aria-controls={settingsOpen ? "access-settings-panel" : undefined}
            onClick={onOpenSettings}
          >
            <Settings size={16} />{t("access.advancedSettings")}
          </button>
        </div>
      </div>
      <p className="access-principle">
        <span>{t("access.principleIntro")}</span>{" "}
        <span>{t("access.routingIntro")}</span>{" "}
        <span className="page-intro-links">
          <button className="inline-text-link" type="button" onClick={onOpenMarketPrices}>
            {t("market.action")}
          </button>
          <span aria-hidden="true">·</span>
          <button className="inline-text-link" type="button" onClick={onOpenUsageLog}>
            {t("access.platformUsageLog")}
          </button>
        </span>
      </p>
      {!running && issue && (
        <p className="access-connection-issue" role="alert">{issue}</p>
      )}
    </header>
  );
}
