import { useEffect, useRef } from "react";

type EscapeDismissEntry = {
  id: symbol;
  dismiss: () => void;
};

const escapeDismissStack: EscapeDismissEntry[] = [];

function handleEscapeKey(event: KeyboardEvent) {
  if (
    event.key !== "Escape"
    || event.defaultPrevented
    || event.isComposing
  ) {
    return;
  }
  const activeEntry = escapeDismissStack[escapeDismissStack.length - 1];
  if (!activeEntry) return;
  event.preventDefault();
  event.stopPropagation();
  activeEntry.dismiss();
}

function removeEscapeEntry(id: symbol) {
  const index = escapeDismissStack.findIndex((entry) => entry.id === id);
  if (index >= 0) escapeDismissStack.splice(index, 1);
}

export function useEscapeDismiss(onDismiss: () => void, enabled = true) {
  const dismissRef = useRef(onDismiss);
  dismissRef.current = onDismiss;

  useEffect(() => {
    if (!enabled || typeof document === "undefined") return;

    const entry: EscapeDismissEntry = {
      id: Symbol("escape-dismiss"),
      dismiss: () => dismissRef.current(),
    };
    const shouldAttachListener = escapeDismissStack.length === 0;
    escapeDismissStack.push(entry);
    if (shouldAttachListener) {
      document.addEventListener("keydown", handleEscapeKey);
    }

    return () => {
      removeEscapeEntry(entry.id);
      if (escapeDismissStack.length === 0) {
        document.removeEventListener("keydown", handleEscapeKey);
      }
    };
  }, [enabled]);
}
