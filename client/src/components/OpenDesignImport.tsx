import { invoke } from "@tauri-apps/api/core";
import { LoaderCircle } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

/** Import only on an explicit action; closing the wizard discards late results. */
export function OpenDesignImport({ useSystemProxy, disabled, onImported }: {
  useSystemProxy: boolean;
  disabled: boolean;
  onImported: (credential: string) => void;
}) {
  const { t } = useTranslation();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const fileInput = useRef<HTMLInputElement>(null);
  const attempt = useRef(0);
  useEffect(() => {
    attempt.current += 1;
    setBusy(false);
    setError(null);
    return () => { attempt.current += 1; };
  }, [useSystemProxy]);
  async function importLocal(file?: File) {
    const current = ++attempt.current;
    setBusy(true);
    setError(null);
    try {
      const configJson = file ? await file.text() : undefined;
      if (current !== attempt.current) return;
      const credential = await invoke<string>("import_opendesign_local_credential", {
        useSystemProxy, ...(configJson === undefined ? {} : { configJson }),
      });
      if (current === attempt.current) onImported(credential);
    } catch (failure) {
      // The command exposes only a bounded diagnostic, never the credential body.
      if (current === attempt.current) setError(typeof failure === "string" ? failure : "");
    } finally {
      if (current === attempt.current) setBusy(false);
    }
  }
  return <div className="grid-wide">
    <p className="field-hint">{t("opendesign.hint")}</p>
    <div className="wizard-actions">
      <button type="button" className="quiet" disabled={disabled || busy} aria-busy={busy} onClick={() => void importLocal()}>
        {busy && <LoaderCircle className="spin-icon" size={16} aria-hidden="true" />}
        {t(busy ? "opendesign.importing" : "opendesign.importLocal")}
      </button>
      <button type="button" className="quiet" disabled={disabled || busy} onClick={() => fileInput.current?.click()}>
        {t("opendesign.chooseFile")}
      </button>
      <input ref={fileInput} type="file" accept="application/json,.json" hidden aria-label={t("opendesign.chooseFile")} onChange={event => {
        const file = event.currentTarget.files?.[0];
        event.currentTarget.value = "";
        if (file) void importLocal(file);
      }} />
    </div>
    {error !== null && <p className="field-hint field-hint-warning" role="alert">{t("opendesign.importFailed")} {error}</p>}
  </div>;
}
