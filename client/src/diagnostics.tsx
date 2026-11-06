import { useCallback, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

export type DiagnosticAction = "open_logs";

export type DiagnosticIssue = {
  id: string;
  scope: "about" | "access" | "supplier";
  severity: "warning" | "error";
  title: string;
  message: string;
  action?: DiagnosticAction;
  createdAt: number;
};

export function useDiagnostics(
  derivedIssues: Omit<DiagnosticIssue, "createdAt">[] = [],
) {
  const [issues, setIssues] = useState<DiagnosticIssue[]>([]);

  const pushDiagnostic = useCallback((issue: Omit<DiagnosticIssue, "createdAt">) => {
    setIssues((current) => {
      const nextIssue = { ...issue, createdAt: Date.now() };
      return [nextIssue, ...current.filter((item) => item.id !== issue.id)].slice(0, 40);
    });
  }, []);

  const resolveDiagnostic = useCallback((id: string) => {
    setIssues((current) => current.filter((issue) => issue.id !== id));
  }, []);

  const activeDiagnostics = useMemo(
    () => [...issues, ...derivedIssues.map((issue) => ({ ...issue, createdAt: 0 }))],
    [issues, derivedIssues],
  );

  return {
    activeDiagnostics,
    pushDiagnostic,
    resolveDiagnostic,
  };
}

export function DiagnosticsPanel({
  issues,
  onAction,
}: {
  issues: DiagnosticIssue[];
  onAction: (issue: DiagnosticIssue) => void;
}) {
  const { t } = useTranslation();
  if (issues.length === 0) return null;

  return (
    <div className="diagnostic-panel">
      <div className="diagnostic-heading">
        <strong>{t("diagnostics.title")}</strong>
        <span>{t("diagnostics.issueCount", { count: issues.length })}</span>
      </div>
      {issues.map((issue) => (
        <div className={`diagnostic-item ${issue.severity}`} key={issue.id}>
          <div>
            <strong>{issue.title}</strong>
            <span>{issue.message}</span>
          </div>
          {issue.action === "open_logs" && (
            <button type="button" className="quiet" onClick={() => onAction(issue)}>
              {t("diagnostics.actions.openLogs")}
            </button>
          )}
        </div>
      ))}
    </div>
  );
}

export function DiagnosticNotice({ issue }: {
  issue: Omit<DiagnosticIssue, "createdAt">;
}) {
  return (
    <div className="supplier-route-diagnostic-notice" role="status">
      <strong>{issue.title}</strong>
      <span>{issue.message}</span>
    </div>
  );
}
