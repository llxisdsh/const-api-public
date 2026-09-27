import { Copy } from "lucide-react";
import { useMemo } from "react";
import { useTranslation } from "react-i18next";

import type { SupplierNodeRouteStatus } from "../appTypes";
import { DialogBackdrop } from "../DialogBackdrop";
import { ModalCloseButton } from "../ModalCloseButton";
import { useEscapeDismiss } from "../useEscapeDismiss";
import { modelDisplayName, modelDisplayNames } from "../modelPresentation";

export type SupplierModelPoolDetails = {
  total: number;
  accepted: string[];
  unsupported: string[];
  unreported: string[];
};

function normalizedModels(models: string[]) {
  const seen = new Set<string>();
  const result: string[] = [];
  for (const rawModel of models) {
    const model = rawModel.trim();
    const key = model.toLowerCase();
    if (!model || seen.has(key)) continue;
    seen.add(key);
    result.push(model);
  }
  return result;
}

export function resolveSupplierModelPoolDetails({
  observedModels,
  routeStatus,
  platformRegistered,
}: {
  observedModels: string[];
  routeStatus?: SupplierNodeRouteStatus;
  platformRegistered: boolean;
}): SupplierModelPoolDetails {
  const observed = normalizedModels(observedModels);
  const acceptedReport = normalizedModels(routeStatus?.accepted_models ?? []);
  const unsupported = normalizedModels(routeStatus?.unsupported_models ?? []);
  const hasExplicitReport = acceptedReport.length > 0 || unsupported.length > 0;

  const accepted = hasExplicitReport
    ? acceptedReport
    : platformRegistered
      ? observed
      : [];
  // First-source ownership is shared across groups: a later rejected duplicate
  // must not also appear as the already accepted short model (or vice versa).
  const labels = modelDisplayNames([...observed, ...accepted, ...unsupported]);
  const owners = new Map([...labels].map(([wire, name]) => [name, wire.trim().replace(/\[1m\]$/i, "").toLowerCase()]));
  const display = (models: string[]) => [...new Set(models.flatMap((model) => {
    const label = modelDisplayName(model);
    const wire = model.trim().replace(/\[1m\]$/i, "").toLowerCase();
    return owners.get(label) === wire || label === wire ? [label] : [];
  }))].sort();
  const acceptedNames = display(accepted);
  const unsupportedNames = display(unsupported).filter((name) => !acceptedNames.includes(name));
  const reportedKeys = new Set([...acceptedNames, ...unsupportedNames]);
  return {
    total: labels.size,
    accepted: acceptedNames,
    unsupported: unsupportedNames,
    unreported: display(observed).filter((model) => !reportedKeys.has(model)),
  };
}

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
    return [
      section(t("supplier.modelPool.accepted"), details.accepted),
      section(t("supplier.modelPool.unsupported"), details.unsupported),
      section(t("supplier.modelPool.unreported"), details.unreported),
    ].join("\n\n");
  }, [details, t]);

  return (
    <DialogBackdrop
      className="supplier-model-pool-backdrop"
      aria-labelledby="supplier-model-pool-title"
    >
      <div className="modal-panel supplier-model-pool-modal">
        <div className="supplier-model-pool-header">
          <div>
            <h2 id="supplier-model-pool-title">{t("supplier.modelPool.title")}</h2>
            <p>{channelName}</p>
          </div>
          <ModalCloseButton onClick={onClose} />
        </div>

        <p className="supplier-model-pool-summary">
          {t("supplier.modelPool.summary", {
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
          <small>
            {routeStatus?.catalog_release_id
              ? t("supplier.modelPool.catalogRelease", {
                release: routeStatus.catalog_release_id,
              })
              : t("supplier.modelPool.noCatalogRelease")}
          </small>
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
