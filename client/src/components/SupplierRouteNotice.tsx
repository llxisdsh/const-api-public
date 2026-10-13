import { useTranslation } from "react-i18next";
import { supplierRouteStatusBlocksReady, supplierRouteStatusDetail } from "../appHelpers";
import type { SupplierNodeRouteStatus } from "../appTypes";
import { currentAppLanguage } from "../i18n";

export function SupplierRouteNotice({ status }: { status: SupplierNodeRouteStatus | undefined }) {
  const { t } = useTranslation();
  const detail = supplierRouteStatusDetail(status);
  if (!supplierRouteStatusBlocksReady(status) || !detail) return null;
  const formatTime = (unix: number) => new Date(unix * 1000).toLocaleString(currentAppLanguage(), { hour12: false });
  const metadata = [
    status?.updated_at_unix ? t("supplierRoute.updatedAt", { time: formatTime(status.updated_at_unix) }) : "",
    status?.reason ? t("supplierRoute.reasonCode", { code: status.reason }) : "",
  ].filter(Boolean).join(" · ");
  const cooldown = status?.cooldown_until_unix ?? 0;
  return (
    <div className="supplier-route-diagnostic-notice" role="status">
      <strong>{t("supplierRoute.routeDetailTitle")}</strong>
      <span>{detail}</span>
      {metadata && <small>{metadata}</small>}
      {cooldown > 0 && <small>{t(
        cooldown * 1000 > Date.now() ? "supplierRoute.cooldownUntil" : "supplierRoute.cooldownElapsed",
        { time: formatTime(cooldown) },
      )}</small>}
    </div>
  );
}
