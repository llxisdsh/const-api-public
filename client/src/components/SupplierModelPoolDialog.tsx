import { Copy } from "lucide-react";
import { useMemo } from "react";
import { useTranslation } from "react-i18next";

import type { SupplierNodeRouteStatus } from "../appTypes";
import { DialogBackdrop } from "../DialogBackdrop";
import { ModalCloseButton } from "../ModalCloseButton";
import { useEscapeDismiss } from "../useEscapeDismiss";
import { resolveSupplierModelPoolDetails } from "../supplierModelPool";

export function SupplierModelPoolDialog({
  channelName,
  observedModels,
  routeStatus,
  platformRegistered,
  onClose,
  onCopy,
}: {
  channelName: string;
  observedModels: string[];
  routeStatus?: SupplierNodeRouteStatus;
  platformRegistered: boolean;
  onClose: () => void;
  onCopy: (value: string, message: string) => void | Promise<boolean>;
}) {
  const { t } = useTranslation();
  useEscapeDismiss(onClose);

  const details = useMemo(
    () => resolveSupplierModelPoolDetails({
      observedModels,
      routeStatus,
      platformRegistered,
    }),
    [observedModels, platformRegistered, routeStatus],
  );
  const detailText = useMemo(() => {
    const section = (label: string, models: string[]) => [
      `${label} (${models.length})`,
      models.length > 0 ? models.join("\n") : t("common.none"),
    ].join("\n");
    if (!platformRegistered) return section(t("supplier.modelPool.discovered"), details.unreported);
    return [
      section(t("supplier.modelPool.accepted"), details.accepted),
      section(t("supplier.modelPool.unsupported"), details.unsupported),
      section(t("supplier.modelPool.unreported"), details.unreported),
    ].join("\n\n");
  }, [details, platformRegistered, t]);

  return (
    <DialogBackdrop
      className="supplier-model-pool-backdrop"
      aria-labelledby="supplier-model-pool-title"
    >
      <div className="modal-panel supplier-model-pool-modal">
        <div className="supplier-model-pool-header">
          <div>
            <h2 id="supplier-model-pool-title">{t(platformRegistered ? "supplier.modelPool.title" : "supplier.modelPool.localTitle")}</h2>
            <p>{channelName}</p>
          </div>
          <ModalCloseButton onClick={onClose} />
        </div>

        <p className="supplier-model-pool-summary">
          {t(platformRegistered ? "supplier.modelPool.summary" : "supplier.modelPool.discoveredSummary", {
            total: details.total,
            accepted: details.accepted.length,
            unsupported: details.unsupported.length,
            unreported: details.unreported.length,
          })}
        </p>

        <textarea
          className="supplier-model-pool-text"
          aria-label={t("supplier.modelPool.textAria")}
          readOnly
          spellCheck={false}
          value={detailText}
        />

        <div className="supplier-model-pool-footer">
          {platformRegistered && <small>
            {routeStatus?.catalog_release_id
              ? t("supplier.modelPool.catalogRelease", {
                release: routeStatus.catalog_release_id,
              })
              : t("supplier.modelPool.noCatalogRelease")}
          </small>}
          <div className="actions">
            <button
              className="quiet"
              type="button"
              onClick={() => void onCopy(
                detailText,
                t("supplier.modelPool.copied"),
              )}
            >
              <Copy size={16} />
              {t("supplier.modelPool.copy")}
            </button>
            <button className="primary" type="button" onClick={onClose}>
              {t("common.close")}
            </button>
          </div>
        </div>
      </div>
    </DialogBackdrop>
  );
}
