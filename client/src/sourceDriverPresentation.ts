import { tr } from "./i18n";
import type { SourceDriver } from "./sourceDrivers";

export function sourceDriverTitle(driver: SourceDriver) {
  return tr(`sourceDrivers.${driver.id}.title`, { defaultValue: driver.title });
}

export function sourceDriverDescription(driver: SourceDriver) {
  return tr(`sourceDrivers.${driver.id}.description`, { defaultValue: driver.description });
}

export function sourceDriverActionLabel(driver: SourceDriver) {
  const action = driver.actionLabel === "连接向导"
    ? "connect"
    : driver.actionLabel === "检测并添加"
      ? "detectAndAdd"
      : "add";
  return tr(`sourceDrivers.actions.${action}`, { defaultValue: driver.actionLabel });
}

export function sourceDriverDisplayName(
  driver: SourceDriver,
  configuredName: string | null | undefined,
  fallback = "",
) {
  const name = configuredName?.trim();
  const defaultName = sourceDriverTitle(driver);
  return !name || name === driver.title ? defaultName || fallback : name;
}
