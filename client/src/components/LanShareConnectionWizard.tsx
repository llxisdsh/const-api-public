import { useState } from "react";
import { useTranslation } from "react-i18next";
import { DialogBackdrop } from "../DialogBackdrop";
import { ModalCloseButton } from "../ModalCloseButton";
import type { LanShareConnectionInfo } from "../lanShareConnectionInfo";
import { LanShareConnectionImport } from "./LanShareConnectionImport";

type LanShareConnectionWizardProps = {
  disabled?: boolean;
  onClose: () => void;
  onImport: (connection: LanShareConnectionInfo) => void;
  onSkip: () => void;
};

export function LanShareConnectionWizard({
  disabled = false,
  onClose,
  onImport,
  onSkip,
}: LanShareConnectionWizardProps) {
  const { t } = useTranslation();
  const [connection, setConnection] = useState<LanShareConnectionInfo | null>(null);

  return (
    <DialogBackdrop aria-labelledby="lan-share-connection-wizard-title">
      <div className="modal-panel lan-share-connection-wizard">
        <div className="section-title-row">
          <div>
            <h2 id="lan-share-connection-wizard-title">{t("lanShare.connectionWizardTitle")}</h2>
            <p className="section-hint">{t("lanShare.connectionWizardHint")}</p>
          </div>
          <ModalCloseButton onClick={onClose} disabled={disabled} />
        </div>

        <LanShareConnectionImport disabled={disabled} onParsed={setConnection} />

        <div className="actions lan-share-connection-wizard-actions">
          <button type="button" className="quiet" disabled={disabled} onClick={onSkip}>
            {t("lanShare.skipConnectionImport")}
          </button>
          <button
            type="button"
            className="primary"
            disabled={disabled || !connection}
            onClick={() => connection && onImport(connection)}
          >
            {t("lanShare.startConnectionSetup")}
          </button>
        </div>
      </div>
    </DialogBackdrop>
  );
}
