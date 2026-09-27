type ShortcutEvent = Pick<
  KeyboardEvent,
  "key" | "metaKey" | "ctrlKey" | "altKey" | "shiftKey"
>;

const DEVTOOLS_KEYS = new Set(["c", "i", "j"]);

export function shouldInstallWebviewShortcutGuard(
  isDevelopment: boolean,
  isTauriRuntime: boolean,
) {
  return !isDevelopment && isTauriRuntime;
}

export function shouldBlockWebviewShortcut(event: ShortcutEvent) {
  const key = event.key.toLowerCase();
  if (key === "f5" || key === "f12") return true;

  const primaryModifier = event.metaKey || event.ctrlKey;
  if (primaryModifier && (key === "r" || key === "u")) return true;

  const developerToolsModifier =
    (event.metaKey && event.altKey)
    || (event.ctrlKey && event.shiftKey);
  return developerToolsModifier && DEVTOOLS_KEYS.has(key);
}

export function installWebviewShortcutGuard(target: Window = window) {
  const blockUnsafeShortcut = (event: KeyboardEvent) => {
    if (!shouldBlockWebviewShortcut(event)) return;
    event.preventDefault();
    event.stopPropagation();
  };

  const suppressNativeContextMenu = (event: MouseEvent) => {
    const editableTarget = event.composedPath().some((candidate) =>
      candidate instanceof Element
      && candidate.matches(
        "input, textarea, [contenteditable]:not([contenteditable='false'])",
      ));
    const selection = target.getSelection();
    const hasSelectedText = Boolean(
      selection
      && !selection.isCollapsed
      && selection.toString().length > 0,
    );

    // The native WebView menu is the mouse-accessible Cut/Copy/Paste surface.
    if (editableTarget || hasSelectedText) return;

    // Keep the event flowing so app-owned context menus, such as the tool dock,
    // can still handle right click after the WebView menu has been suppressed.
    event.preventDefault();
  };

  target.addEventListener("keydown", blockUnsafeShortcut, true);
  target.addEventListener("contextmenu", suppressNativeContextMenu, true);
  return () => {
    target.removeEventListener("keydown", blockUnsafeShortcut, true);
    target.removeEventListener("contextmenu", suppressNativeContextMenu, true);
  };
}
