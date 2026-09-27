import type { UpdateInstallReadiness } from "./appTypes";
import { tr } from "./i18n";

export function isRetryableUpdateTransportError(error: unknown) {
  const message = String(error).toLowerCase();
  return message.includes("error sending request")
    || message.includes("download request failed with status")
    || message.includes("all configured update sources failed")
    || message.includes("all update download sources failed")
    || message.includes("timed out")
    || message.includes("timeout")
    || message.includes("connection reset")
    || message.includes("connection refused")
    || message.includes("dns")
    || message.includes("network");
}

export function formatUpdateError(error: unknown) {
  const message = String(error);
  if (message.includes("Could not fetch a valid release JSON")) {
    return tr("updates.errors.missingReleaseJson");
  }
  if (message.includes("404") || message.toLowerCase().includes("not found")) {
    return tr("updates.errors.notFound");
  }
  if (isRetryableUpdateTransportError(error)) {
    return tr("updates.errors.network");
  }
  return message;
}

export function updateInstallWaitingText(readiness: UpdateInstallReadiness) {
  if (readiness.active_logical_requests > 0) {
    return tr("updates.waiting.logicalRequests", { count: readiness.active_logical_requests });
  }
  if (readiness.active_proxy_requests > 0) {
    return tr("updates.waiting.proxyRequests", { count: readiness.active_proxy_requests });
  }
  if (readiness.active_supplier_requests > 0) {
    return tr("updates.waiting.supplierRequests", { count: readiness.active_supplier_requests });
  }
  if (readiness.pending_supplier_outbound_messages > 0) {
    return tr("updates.waiting.outboundMessages", { count: readiness.pending_supplier_outbound_messages });
  }
  if (readiness.unacknowledged_supplier_replay_messages > 0) {
    return tr("updates.waiting.replayMessages", { count: readiness.unacknowledged_supplier_replay_messages });
  }
  if (readiness.reason === "update_install_in_progress") {
    return tr("updates.waiting.installInProgress");
  }
  return tr("updates.waiting.idle");
}
