import { useTranslation } from "react-i18next";

import {
  protocolLabel,
  surfaceLabel,
  surfaceProtocols,
  surfaceVerificationLabel,
} from "../appHelpers";
import type {
  ChannelSurfaceBinding,
  DebugProtocol,
} from "../appTypes";
import type { SourceDriverTarget } from "../sourceDrivers";

type SupplierSurfaceConfigProps = {
  bindings: ChannelSurfaceBinding[];
  rows?: ChannelSurfaceBinding[];
  defaultTarget?: SourceDriverTarget;
  mode: "custom-edit" | "readonly";
  placeholderBaseUrl?: string;
  onUpdateSurface?: (
    surface: ChannelSurfaceBinding["surface"],
    patch: Partial<ChannelSurfaceBinding>,
  ) => void;
  onToggleProtocol?: (
    surface: ChannelSurfaceBinding["surface"],
    protocol: DebugProtocol,
    enabled: boolean,
  ) => void;
  onSelectDefaultTarget?: (protocol: DebugProtocol) => void;
};

export function SupplierSurfaceConfig({
  bindings,
  rows = bindings,
  defaultTarget,
  mode,
  placeholderBaseUrl,
  onUpdateSurface,
  onToggleProtocol,
  onSelectDefaultTarget,
}: SupplierSurfaceConfigProps) {
  const { t } = useTranslation();
  const editable = mode === "custom-edit";
  const visibleRows = editable ? rows : bindings;
  const targetOptions = bindings.flatMap((binding) => binding.protocols.map((protocol) => ({
    surface: binding.surface,
    protocol: protocol.protocol as DebugProtocol,
  })));

  if (!editable && visibleRows.length === 0) {
    return <p className="supplier-surface-empty">{t("supplier.advanced.notConfigured")}</p>;
  }

  return (
    <div className={`supplier-surface-config supplier-surface-config-${mode}`}>
      {editable ? (
        <div className="supplier-surface-config-heading">
          <strong>{t("supplier.apiInterfaces.title")}</strong>
          <p>{t("supplier.apiInterfaces.description")}</p>
        </div>
      ) : null}
      <div className="supplier-surface-cards">
        {visibleRows.map((row) => {
          const configured = bindings.find((binding) => binding.surface === row.surface);
          const availableProtocols = surfaceProtocols(row.surface);
          const enabled = Boolean(configured?.protocols.length);
          return (
            <div
              className={`supplier-surface-card${enabled ? " enabled" : ""}`}
              key={row.surface}
            >
              <div className="supplier-surface-card-heading">
                <strong>{t("supplier.apiInterfaces.interfaceName", {
                  name: surfaceLabel(row.surface),
                })}</strong>
                <span className={configured?.verification.state === "rejected" ? "bad" : ""}>
                  {editable
                    ? enabled ? t("supplier.apiInterfaces.enabled") : t("supplier.advanced.notEnabled")
                    : surfaceVerificationLabel(configured?.verification)}
                </span>
              </div>
              {editable ? (
                <div
                  className="supplier-surface-protocols"
                  aria-label={t("supplier.advanced.protocolAria", {
                    surface: surfaceLabel(row.surface),
                  })}
                >
                  {availableProtocols.map((protocol) => {
                    const checked = Boolean(configured?.protocols.some(
                      (binding) => binding.protocol === protocol,
                    ));
                    return (
                      <label key={protocol}>
                        <input
                          type="checkbox"
                          checked={checked}
                          onChange={(event) => onToggleProtocol?.(
                            row.surface,
                            protocol,
                            event.target.checked,
                          )}
                        />
                        <span>{protocolLabel(protocol)}</span>
                      </label>
                    );
                  })}
                </div>
              ) : (
                <div className="supplier-surface-protocols readonly">
                  {configured?.protocols.map((protocol) => (
                    <span key={protocol.protocol}>{protocolLabel(protocol.protocol)}</span>
                  ))}
                </div>
              )}
              {enabled ? (
                editable ? (
                  <label className="supplier-surface-base-url">
                    Base URL
                    <input
                      value={configured?.base_url ?? row.base_url}
                      placeholder={placeholderBaseUrl || "https://api.example.com"}
                      onChange={(event) => onUpdateSurface?.(
                        row.surface,
                        { base_url: event.target.value },
                      )}
                    />
                  </label>
                ) : (
                  <div className="supplier-surface-readonly-url">
                    <span>Base URL</span>
                    <code>{configured?.base_url || t("supplier.apiInterfaces.managedConnection")}</code>
                  </div>
                )
              ) : null}
            </div>
          );
        })}
      </div>
      {editable ? (
        <>
          <label className="supplier-surface-default-target">
            {t("supplier.apiInterfaces.defaultTarget")}
            <select
              value={defaultTarget?.protocol ?? ""}
              onChange={(event) => onSelectDefaultTarget?.(event.target.value as DebugProtocol)}
            >
              {targetOptions.map((target) => (
                <option key={`${target.surface}:${target.protocol}`} value={target.protocol}>
                  {protocolLabel(target.protocol)}
                </option>
              ))}
            </select>
            <span className="field-hint">{t("supplier.apiInterfaces.defaultTargetHint")}</span>
          </label>
        </>
      ) : null}
    </div>
  );
}
