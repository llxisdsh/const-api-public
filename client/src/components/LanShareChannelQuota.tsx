import { invoke } from "@tauri-apps/api/core";
import { LoaderCircle, Network, RefreshCw } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { LanShareSelfStatus } from "../appTypes";

function errorText(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

function formatTokens(value: number) {
  return new Intl.NumberFormat(undefined, { maximumFractionDigits: 0 }).format(value);
}

export function LanShareChannelQuota({ channelId, enabled }: { channelId: string; enabled: boolean }) {
  const { t } = useTranslation();
  const [status, setStatus] = useState<LanShareSelfStatus | null>(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(enabled);
  const refreshInFlight = useRef(false);

  const refresh = useCallback(async (silent = false) => {
    if (!enabled || refreshInFlight.current) return;
    refreshInFlight.current = true;
    if (!silent) setLoading(true);
    try {
      const next = await invoke<LanShareSelfStatus>("get_lan_share_channel_status", { channelId });
      setStatus(next);
      setError("");
    } catch (nextError) {
      setError(errorText(nextError));
    } finally {
      refreshInFlight.current = false;
      if (!silent) setLoading(false);
    }
  }, [channelId, enabled]);

  useEffect(() => {
    setStatus(null);
    setError("");
    if (!enabled) {
      setLoading(false);
      return undefined;
    }
    void refresh();
    const timer = window.setInterval(() => void refresh(true), 5000);
    return () => window.clearInterval(timer);
  }, [channelId, enabled, refresh]);

  const remainingRatio = status && status.weekly_token_limit > 0
    ? Math.min(100, (status.remaining_tokens / status.weekly_token_limit) * 100)
    : 0;

  return (
    <section className="drawer-section lan-share-channel-quota">
      <div className="lan-share-channel-quota-heading">
        <div>
          <span className="lan-share-channel-quota-icon"><Network size={16} /></span>
          <div>
            <h2>{t("lanShare.memberStatusTitle")}</h2>
            <p>{t("lanShare.localOnlyChannel")}</p>
          </div>
        </div>
        <button type="button" className="quiet compact" disabled={!enabled || loading} onClick={() => void refresh()}>
          <RefreshCw size={14} />{t("common.refresh")}
        </button>
      </div>

      {!enabled ? <p className="lan-share-quota-muted">{t("labels.channel.disabled")}</p> : null}
      {loading && !status ? <p className="lan-share-quota-muted"><LoaderCircle className="spin" size={15} />{t("common.loading")}</p> : null}
      {error ? <p className="lan-share-quota-error">{t("lanShare.quotaUnavailable", { error })}</p> : null}
      {status && !status.enabled ? <p className="lan-share-quota-error">{t("lanShare.remotePaused")}</p> : null}
      {status ? (
        <div className="lan-share-quota-body">
          <div className="lan-share-quota-summary">
            <div>
              <span>{t("lanShare.weekRemaining")}</span>
              <strong>{formatTokens(status.remaining_tokens)} / {formatTokens(status.weekly_token_limit)}</strong>
            </div>
            <div>
              <span>{t("lanShare.usedTokens", { count: formatTokens(status.used_tokens) })}</span>
              <small>{t("lanShare.resetsAt", { time: new Date(status.resets_at_unix * 1000).toLocaleString() })}</small>
            </div>
          </div>
          <div className="lan-share-progress" aria-hidden="true"><span style={{ width: `${remainingRatio}%` }} /></div>
          <div className="lan-share-quota-details">
            <span>{t("lanShare.sharedBy", { name: status.name })}</span>
            <span>{t("lanShare.inputTokens", { count: formatTokens(status.input_tokens) })}</span>
            <span>{t("lanShare.outputTokens", { count: formatTokens(status.output_tokens) })}</span>
            <span>{t("lanShare.requests", { count: status.requests })}</span>
          </div>
        </div>
      ) : null}
    </section>
  );
}
