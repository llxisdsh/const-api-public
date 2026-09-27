import type { ReactNode } from "react";
import { LoaderCircle, MessageSquareText, RefreshCw } from "lucide-react";
import { useTranslation } from "react-i18next";
import { AvailabilityGroupChecks, type AvailabilityGroupChecksProps } from "./AvailabilityGroupChecks";

type Props = AvailabilityGroupChecksProps & {
  busy: boolean;
  canSupply: boolean;
  saving: boolean;
  testing: boolean;
  model: string;
  prompt: string;
  onPromptChange: (prompt: string) => void;
  onFullCheck: () => void | Promise<void>;
  onTest: () => void;
  children?: ReactNode;
};

// The same check controls and result ordering apply to every channel driver.
export function SupplierChannelChecks(props: Props) {
  const { t } = useTranslation();
  const disabled = props.busy || !props.canSupply;
  return (
    <div className="supplier-check-panel">
      <AvailabilityGroupChecks checks={props.checks} groups={props.groups} renderAction={(showResults) => (
        <button type="button" className="quiet" disabled={disabled} onClick={async () => {
          // Only this explicit action expands results, never background checks or saves.
          try { await props.onFullCheck(); } finally { showResults(); }
        }}>
          {props.saving ? <LoaderCircle className="spin-icon" size={16} /> : <RefreshCw size={16} />}
          {t("supplier.availability.fullCheck")}
        </button>
      )} />
      <section className="supplier-test-panel">
        <div className="supplier-model-test-controls">
          <span className="supplier-test-model" title={t("supplier.availability.currentModel", { model: props.model || "—" })}>{props.model || "—"}</span>
          <input aria-label={t("supplier.availability.testPrompt")} value={props.prompt}
            onChange={(event) => props.onPromptChange(event.target.value)} />
          <button type="button" className="quiet" onClick={props.onTest} disabled={disabled}>
            {props.testing ? <LoaderCircle className="spin-icon" size={16} /> : <MessageSquareText size={16} />}
            {props.testing ? t("supplier.availability.testing") : t("supplier.availability.testCurrentModel")}
          </button>
        </div>
        {props.children}
      </section>
    </div>
  );
}
