import { useCallback, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import { tr } from "./i18n";

export type DiagnosticAction = "check_update" | "refresh_registry" | "start_proxy" | "open_access" | "open_supplier" | "open_logs";

export type DiagnosticIssue = {
  id: string;
  scope: "about" | "access" | "supplier";
  severity: "warning" | "error";
  title: string;
  message: string;
  action?: DiagnosticAction;
  createdAt: number;
};

type UpdateStatus = {
  status: "idle" | "checking" | "downloading" | "ready" | "ready_waiting_idle" | "installing" | "error";
  error?: string;
  showDiagnostic?: boolean;
};

export function useDiagnostics(
  updateState: UpdateStatus,
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
    () => buildActiveDiagnostics(issues, updateState, derivedIssues),
    [issues, updateState.status, updateState.error, updateState.showDiagnostic, derivedIssues],
  );

  return {
    activeDiagnostics,
    hasDiagnostics: activeDiagnostics.length > 0,
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
          {issue.action && (
            <button type="button" className="quiet" onClick={() => onAction(issue)}>
              {diagnosticActionLabel(issue.action)}
            </button>
          )}
        </div>
      ))}
    </div>
  );
}

function buildActiveDiagnostics(
  issues: DiagnosticIssue[],
  updateState: UpdateStatus,
  derivedIssues: Omit<DiagnosticIssue, "createdAt">[],
) {
  const active = [...issues];
  for (const issue of derivedIssues) {
    if (!active.some((item) => item.id === issue.id)) {
      active.push({ ...issue, createdAt: 0 });
    }
  }
  if (updateState.status === "error" && updateState.showDiagnostic !== false && !active.some((issue) => issue.id === "update-source")) {
    active.push({
      id: "update-source",
      scope: "about",
      severity: "warning",
      title: tr("diagnostics.updateSourceFailed"),
      message: updateState.error || tr("diagnostics.updateUnavailable"),
      action: "check_update",
      createdAt: Date.now(),
    });
  }
  return active.sort((a, b) => severityWeight(b.severity) - severityWeight(a.severity) || b.createdAt - a.createdAt);
}

function severityWeight(severity: DiagnosticIssue["severity"]) {
  return severity === "error" ? 2 : 1;
}

function diagnosticActionLabel(action: DiagnosticAction) {
  switch (action) {
    case "check_update":
      return tr("diagnostics.actions.retryUpdate");
    case "refresh_registry":
      return tr("diagnostics.actions.refreshEndpoints");
    case "start_proxy":
      return tr("diagnostics.actions.startEntry");
    case "open_access":
      return tr("diagnostics.actions.openUse");
    case "open_supplier":
      return tr("diagnostics.actions.openSupply");
    case "open_logs":
      return tr("diagnostics.actions.openLogs");
    default:
      return tr("diagnostics.actions.resolve");
  }
}
