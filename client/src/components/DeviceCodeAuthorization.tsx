import { invoke } from "@tauri-apps/api/core";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

type DeviceSession = {
  device_code: string;
  user_code: string;
  verification_uri: string;
  expires_at_unix_ms: number;
  interval_seconds: number;
};
type PollResult =
  | { state: "pending" | "slow_down" }
  | { state: "retry"; after_seconds: number }
  | { state: "authorized"; api_key: string };
type AuthError = { code: string; status?: number };

export function DeviceCodeAuthorization({ provider = "muse_code", disabled, onStarting, onAuthorized, onOpenUrl }: {
  provider?: "muse_code" | "copilot" | "cursor";
  disabled: boolean;
  onStarting?: () => void;
  onAuthorized: (key: string) => void;
  onOpenUrl: (url: string) => void;
}) {
  const { t } = useTranslation();
  const labels = provider === "muse_code" ? "museCode" : provider;
  const [starting, setStarting] = useState(false);
  const [session, setSession] = useState<DeviceSession | null>(null);
  const [error, setError] = useState<AuthError | null>(null);
  const [status, setStatus] = useState<"idle" | "waiting" | "retrying" | "success">("idle");
  const attempt = useRef(0);
  const onAuthorizedRef = useRef(onAuthorized);
  useEffect(() => { onAuthorizedRef.current = onAuthorized; }, [onAuthorized]);
  useEffect(() => () => { attempt.current += 1; }, []);
  useEffect(() => {
    if (provider !== "cursor" || !session) return;
    return () => { void invoke("cancel_cursor_auth_session", { deviceCode: session.device_code }); };
  }, [provider, session]);

  function stop() {
    attempt.current += 1;
    setStarting(false);
    setSession(null);
    setStatus("idle");
  }

  async function start() {
    const current = ++attempt.current;
    onStarting?.();
    setError(null);
    setStarting(true);
    setStatus("idle");
    try {
      const next = await invoke<DeviceSession>(`create_${provider}_auth_session`);
      if (attempt.current !== current) {
        if (provider === "cursor") await invoke("cancel_cursor_auth_session", { deviceCode: next.device_code });
        return;
      }
      setSession(next);
      setStatus("waiting");
    } catch (failure) {
      if (attempt.current === current) setError(authError(failure));
    } finally {
      if (attempt.current === current) setStarting(false);
    }
  }

  useEffect(() => {
    if (!session || disabled) return;
    let cancelled = false;
    let interval = session.interval_seconds * 1000;
    let timer: ReturnType<typeof setTimeout>;
    const expiryTimer = setTimeout(() => {
      cancelled = true;
      setError({ code: "expired" });
      setSession(null);
    }, Math.max(0, session.expires_at_unix_ms - Date.now()));
    const schedule = () => {
      timer = setTimeout(() => void poll(), Math.min(interval, Math.max(0, session.expires_at_unix_ms - Date.now())));
    };
    const poll = async () => {
      if (cancelled) return;
      if (Date.now() >= session.expires_at_unix_ms) {
        setError({ code: "expired" });
        setSession(null);
        return;
      }
      try {
        const result = await invoke<PollResult>(`poll_${provider}_auth_session`, { deviceCode: session.device_code });
        if (cancelled) return;
        if (result.state === "authorized") {
          onAuthorizedRef.current(result.api_key);
          setStatus("success");
          setSession(null);
          return;
        }
        if (result.state === "slow_down") interval += 5000;
        if (result.state === "retry") interval = Math.max(interval, result.after_seconds * 1000);
        setStatus(result.state === "retry" ? "retrying" : "waiting");
        schedule();
      } catch (failure) {
        if (!cancelled) {
          setError(authError(failure));
          setSession(null);
        }
      }
    };
    schedule();
    return () => { cancelled = true; clearTimeout(timer); clearTimeout(expiryTimer); };
  }, [session, disabled, provider]);

  return <div className="grid-wide">
    <p className="field-hint">{t(`${labels}.hint`)}</p>
    <div className="actions">
      <button type="button" className="quiet" disabled={disabled || starting || !!session} onClick={() => void start()}>
        {t(`${labels}.${starting ? "starting" : "login"}`)}
      </button>
      {(starting || session) && <button type="button" className="quiet" onClick={stop}>{t("common.cancel")}</button>}
    </div>
    {session && <div className="wizard-operation-status" role="status">
      {session.user_code && <span>{t(`${labels}.code`)} <code>{session.user_code}</code></span>}
      <button type="button" className="quiet" onClick={() => onOpenUrl(session.verification_uri)}>{t(`${labels}.openLogin`)}</button>
      <p>{t(`${labels}.${status === "retrying" ? "retrying" : "waiting"}`)}</p>
    </div>}
    {error && <p className="field-hint field-hint-warning" role="alert">
      {t(`${labels}.errors.${error.code}`, { defaultValue: t(`${labels}.errors.upstream_error`) })}
      {error.status ? ` (HTTP ${error.status})` : ""}
    </p>}
    {status === "success" && <p className="field-hint" role="status">{t(`${labels}.success`)}</p>}
  </div>;
}

function authError(value: unknown): AuthError {
  if (value && typeof value === "object" && "code" in value && typeof value.code === "string") {
    return { code: value.code, status: "status" in value && typeof value.status === "number" ? value.status : undefined };
  }
  return { code: "upstream_error" };
}
