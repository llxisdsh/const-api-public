import { ChevronDown, ChevronUp, Search, X } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

type BrowserFindWindow = Window & {
  find?: (
    text: string,
    caseSensitive?: boolean,
    backwards?: boolean,
    wrapAround?: boolean,
    wholeWord?: boolean,
    searchInFrames?: boolean,
    showDialog?: boolean,
  ) => boolean;
};

export type PageFindOptions = {
  backwards: boolean;
  restart: boolean;
};

export type PageFindRunner = (text: string, options: PageFindOptions) => boolean;

export function runBrowserPageFind(
  text: string,
  { backwards, restart }: PageFindOptions,
  browserWindow: BrowserFindWindow = window,
  browserDocument: Document = document,
) {
  if (!text || typeof browserWindow.find !== "function") return false;

  if (restart) {
    const root = browserDocument.body;
    const selection = browserWindow.getSelection();
    if (root && selection) {
      const range = browserDocument.createRange();
      range.selectNodeContents(root);
      range.collapse(!backwards);
      selection.removeAllRanges();
      selection.addRange(range);
    }
  }

  return browserWindow.find.call(
    browserWindow,
    text,
    false,
    backwards,
    true,
    false,
    true,
    false,
  );
}

export function PageFindBar({
  enabled,
  findText = runBrowserPageFind,
}: {
  enabled: boolean;
  findText?: PageFindRunner;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [found, setFound] = useState<boolean | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const previousFocusRef = useRef<HTMLElement | null>(null);
  const openRef = useRef(false);
  const queryRef = useRef("");

  openRef.current = open;
  queryRef.current = query;

  const focusInput = useCallback((select: boolean) => {
    window.requestAnimationFrame(() => {
      inputRef.current?.focus({ preventScroll: true });
      if (select) inputRef.current?.select();
    });
  }, []);

  const openFind = useCallback(() => {
    if (!openRef.current) {
      previousFocusRef.current = document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null;
      setOpen(true);
    }
    focusInput(true);
  }, [focusInput]);

  const closeFind = useCallback(() => {
    setOpen(false);
    setFound(null);
    window.requestAnimationFrame(() => previousFocusRef.current?.focus({ preventScroll: true }));
  }, []);

  const search = useCallback((backwards: boolean, restart = false) => {
    const text = queryRef.current;
    setFound(text ? findText(text, { backwards, restart }) : null);
    focusInput(false);
  }, [findText, focusInput]);

  useEffect(() => {
    if (!enabled) return;
    const handleShortcut = (event: KeyboardEvent) => {
      const key = event.key.toLowerCase();
      if (event.metaKey && !event.ctrlKey && !event.altKey && key === "f") {
        event.preventDefault();
        event.stopPropagation();
        openFind();
        return;
      }
      if (!openRef.current) return;
      if (event.metaKey && !event.ctrlKey && !event.altKey && key === "g") {
        event.preventDefault();
        event.stopPropagation();
        search(event.shiftKey);
        return;
      }
      if (key === "escape") {
        event.preventDefault();
        event.stopPropagation();
        closeFind();
      }
    };
    window.addEventListener("keydown", handleShortcut, true);
    return () => window.removeEventListener("keydown", handleShortcut, true);
  }, [closeFind, enabled, openFind, search]);

  if (!enabled || !open) return null;

  return (
    <div className="page-find-bar" role="search" aria-label={t("pageFind.label")}>
      <Search size={15} aria-hidden="true" />
      <input
        ref={inputRef}
        type="search"
        value={query}
        aria-label={t("pageFind.inputLabel")}
        placeholder={t("pageFind.placeholder")}
        onChange={(event) => {
          const text = event.target.value;
          queryRef.current = text;
          setQuery(text);
          setFound(text ? findText(text, { backwards: false, restart: true }) : null);
          focusInput(false);
        }}
        onKeyDown={(event) => {
          if (event.key !== "Enter") return;
          event.preventDefault();
          event.stopPropagation();
          search(event.shiftKey);
        }}
      />
      {found === false ? (
        <span className="page-find-status" role="status">{t("pageFind.noMatches")}</span>
      ) : null}
      <button
        type="button"
        className="page-find-action"
        disabled={!query}
        aria-label={t("pageFind.previous")}
        title={t("pageFind.previous")}
        onClick={() => search(true)}
      >
        <ChevronUp size={16} aria-hidden="true" />
      </button>
      <button
        type="button"
        className="page-find-action"
        disabled={!query}
        aria-label={t("pageFind.next")}
        title={t("pageFind.next")}
        onClick={() => search(false)}
      >
        <ChevronDown size={16} aria-hidden="true" />
      </button>
      <button
        type="button"
        className="page-find-action"
        aria-label={t("pageFind.close")}
        title={t("pageFind.close")}
        onClick={closeFind}
      >
        <X size={16} aria-hidden="true" />
      </button>
    </div>
  );
}
