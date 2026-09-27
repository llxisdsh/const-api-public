import { stableErrorCode } from "./i18n";

export function toolConfigCanLaunchExistingAfterFailure(error: unknown): boolean {
  return stableErrorCode(error) === "tool_config_model_refresh_use_existing";
}
