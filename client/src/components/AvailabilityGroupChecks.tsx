import { useId, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { ChevronDown } from "lucide-react";
import type { AvailabilityGroupResult, ChannelDetectionEvidenceV5 } from "../sourceDrivers";
import { modelDisplayName } from "../modelPresentation";

const groupOrder = ["anthropic_closed", "google_closed", "openai_closed", "other"];
const states = ["available", "suspended", "inconclusive", "deferred"];

type Row = {
  group: string;
  status: string;
  checkedAt?: number;
  model?: string;
  attempts?: number;
  message?: string;
};

function checkedDate(timestamp?: number) {
  if (!timestamp || !Number.isFinite(timestamp)) return undefined;
  const date = new Date(timestamp * 1000);
  return timestamp > 0 && Number.isFinite(date.getTime()) ? date : undefined;
}

export type AvailabilityGroupChecksProps = {
  checks: ChannelDetectionEvidenceV5[];
  groups?: AvailabilityGroupResult[];
};

export function AvailabilityGroupChecks({ checks, groups, renderAction }: AvailabilityGroupChecksProps & {
  renderAction?: (showResults: () => void) => ReactNode;
}) {
  const { t, i18n } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  const detailsId = useId();
  // A present (even empty) runtime snapshot supersedes legacy config evidence.
  const rows: Row[] = groups !== undefined
    ? groups.map((group) => ({
        group: group.group, status: group.status, checkedAt: group.checked_at,
        model: group.representative, attempts: group.attempts, message: group.message,
      }))
    : checks.filter((check) => check.name === "availability_group").map((check) => ({
        group: check.capability || "", status: check.status,
        checkedAt: check.checked_at_unix, message: check.message,
      }));
  const byGroup = new Map<string, Row>();
  for (const row of rows) {
    if (!row.group) continue;
    const previous = byGroup.get(row.group);
    if (!previous || (row.checkedAt || 0) >= (previous.checkedAt || 0)) byGroup.set(row.group, row);
  }
  const rank = (group: string) => {
    const index = groupOrder.indexOf(group);
    return index < 0 ? groupOrder.length : index;
  };
  const visible = [...byGroup.values()].sort((a, b) => rank(a.group) - rank(b.group) || a.group.localeCompare(b.group));
  const latest = checkedDate(Math.max(0, ...visible.map((row) => (checkedDate(row.checkedAt)?.getTime() ?? 0) / 1000)));
  const channelPaused = visible.some((row) => row.group === "other" && row.status === "suspended");
  return (
    <section className="capability-overview-section availability-group-checks" aria-label={t("supplier.availability.title")}>
      <header className="supplier-check-actions">
        <div className="supplier-check-summary">
          {visible.length > 0 ? (
            <button type="button" className="availability-group-toggle"
              aria-expanded={expanded} aria-controls={detailsId}
              onClick={() => setExpanded((current) => !current)}>
              <span>{t("supplier.availability.hint")}</span>
              {latest && <span className="availability-group-checked">
                <time dateTime={latest.toISOString()}
                  title={`${t("supplier.availability.lastChecked")}: ${latest.toLocaleString(i18n.resolvedLanguage)}`}>
                  {latest.toLocaleString(i18n.resolvedLanguage, { dateStyle: "short", timeStyle: "short" })}
                </time>
              </span>}
              <ChevronDown size={14} aria-hidden="true" />
            </button>
          ) : <>
            <span>{t("supplier.availability.hint")}</span>
            <span className="field-hint">{t("supplier.availability.notChecked")}</span>
          </>}
        </div>
        {renderAction?.(() => setExpanded(true))}
      </header>
      {visible.length > 0 && (
        <div id={detailsId} hidden={!expanded}>
          {channelPaused && <p className="field-hint">{t("supplier.availability.channelPaused")}</p>}
          <div className="availability-group-list">
          {visible.map((row) => {
            const date = checkedDate(row.checkedAt);
            return (
              <div className="availability-group-item" key={row.group}>
                <div className="capability-row">
                  <strong>{groupOrder.includes(row.group) ? t(`supplier.availability.groups.${row.group}`) : row.group}</strong>
                  <span className="availability-group-state">
                    {states.includes(row.status) ? t(`supplier.availability.states.${row.status}`) : t("supplier.availability.states.inconclusive")}
                  </span>
                </div>
                <div className="availability-group-details field-hint">
                  <div className="availability-group-meta">
                    {row.model && <span>{t("supplier.availability.testModel")}: {modelDisplayName(row.model)}</span>}
                    {row.attempts !== undefined && <span>{t("supplier.availability.attempts", { count: row.attempts })}</span>}
                    {date && (!latest || date.getTime() !== latest.getTime()) && <span>{t("supplier.availability.checkedAt")}: {date.toLocaleString(i18n.resolvedLanguage)}</span>}
                  </div>
                  {row.message ? <p>{row.message}</p> : !row.model && <p>{t("supplier.availability.noDetails")}</p>}
                </div>
              </div>
            );
          })}
          </div>
        </div>
      )}
    </section>
  );
}
