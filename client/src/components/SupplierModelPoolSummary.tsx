import { ChevronRight } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { SupplierModelPoolDetails } from "../supplierModelPool";

export function SupplierModelPoolSummary({ details, platformRegistered, onViewDetails }: {
  details: SupplierModelPoolDetails;
  platformRegistered: boolean;
  onViewDetails: () => void;
}) {
  const { t } = useTranslation();
  return (
    <div className="supplier-model-pool-field">
      <div className="supplier-field-heading">
        <span>{t("supplier.modelPool.supplyModels")}</span>
        <button type="button" className="supplier-field-link" onClick={onViewDetails}>
          {t("supplier.modelPool.viewDetails")}
          <ChevronRight size={12} />
        </button>
      </div>
      <div className="supplier-model-pool-overview">
        <span className="supplier-model-pool-count">{t(platformRegistered ? "supplier.modelPool.listedSummary" : "supplier.modelPool.discoveredSummary", {
          accepted: details.accepted.length,
          total: details.total,
        })}</span>
      </div>
      <span className="field-hint">{t("supplier.modelPool.supplyHint")}</span>
    </div>
  );
}
