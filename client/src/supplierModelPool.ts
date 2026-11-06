import type { SupplierNodeRouteStatus } from "./appTypes";
import { modelDisplayName, modelDisplayNames } from "./modelPresentation";

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

export function resolveSupplierModelPoolDetails({ observedModels, routeStatus, platformRegistered }: {
  observedModels: string[];
  routeStatus?: SupplierNodeRouteStatus;
  platformRegistered: boolean;
}): SupplierModelPoolDetails {
  const observed = normalizedModels(observedModels);
  // A retained receipt does not mean an offline/local channel is still listed.
  const report = platformRegistered ? routeStatus : undefined;
  const acceptedReport = normalizedModels(report?.accepted_models ?? []);
  const unsupported = normalizedModels(report?.unsupported_models ?? []);
  // Empty lists are an explicit receipt too; only older missing fields use the fallback.
  const hasExplicitReport = report?.accepted_models !== undefined || report?.unsupported_models !== undefined;
  const accepted = hasExplicitReport ? acceptedReport : platformRegistered ? observed : [];
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
