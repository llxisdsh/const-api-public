import { ClipboardPaste } from "lucide-react";
import { useEffect, useId, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  parseLanShareConnectionInfo,
  type LanShareConnectionInfo,
} from "../lanShareConnectionInfo";

type LanShareConnectionImportProps = {
  disabled?: boolean;
  onParsed: (connection: LanShareConnectionInfo | null) => void;
};

export function LanShareConnectionImport({ disabled = false, onParsed }: LanShareConnectionImportProps) {
  const { t } = useTranslation();
  const inputId = `lan-share-connection-import-${useId().replace(/:/g, "")}`;
  const [pastedText, setPastedText] = useState("");
  const [status, setStatus] = useState<"idle" | "success" | "error">("idle");

  useEffect(() => {
    if (!navigator.clipboard?.readText) return undefined;
    let cancelled = false;
    void navigator.clipboard.readText()
      .then((text) => {
        if (cancelled) return;
        const connection = parseLanShareConnectionInfo(text);
        if (!connection) return;
        setPastedText(text);
        onParsed(connection);
        setStatus("success");
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [onParsed]);

  function updateConnectionText(text: string) {
    setPastedText(text);
    if (!text.trim()) {
      onParsed(null);
      setStatus("idle");
      return;
    }
    const connection = parseLanShareConnectionInfo(text);
    if (!connection) {
      onParsed(null);
      setStatus("error");
      return;
    }
    onParsed(connection);
    setStatus("success");
  }

  function importPastedText(event: React.ClipboardEvent<HTMLTextAreaElement>) {
    event.preventDefault();
    updateConnectionText(event.clipboardData.getData("text"));
  }

  return (
    <div className="lan-share-connection-import">
      <label htmlFor={inputId}>
        <span className="lan-share-connection-import-label">
          <ClipboardPaste size={15} />
          {t("lanShare.pasteConnection")}
        </span>
        <textarea
          id={inputId}
          rows={16}
          disabled={disabled}
          value={pastedText}
          spellCheck={false}
          placeholder={t("lanShare.pasteConnectionPlaceholder")}
          onChange={(event) => updateConnectionText(event.target.value)}
          onPaste={importPastedText}
        />
      </label>
      <small className={status === "error" ? "error" : status === "success" ? "success" : undefined} role="status">
        {status === "error"
          ? t("lanShare.connectionImportInvalid")
          : status === "success" ? t("lanShare.connectionImported") : t("lanShare.pasteConnectionHint")}
      </small>
    </div>
  );
}
