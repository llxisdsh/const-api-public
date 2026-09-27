import { describe, expect, it } from "vitest";

import {
  installWebviewShortcutGuard,
  shouldBlockWebviewShortcut,
  shouldInstallWebviewShortcutGuard,
} from "./webviewShortcutGuard";

const shortcut = (
  key: string,
  modifiers: Partial<Pick<KeyboardEvent, "metaKey" | "ctrlKey" | "altKey" | "shiftKey">> = {},
) => ({
  key,
  metaKey: false,
  ctrlKey: false,
  altKey: false,
  shiftKey: false,
  ...modifiers,
});

describe("webview product shortcut guard", () => {
  it("installs only in packaged Tauri builds", () => {
    expect(shouldInstallWebviewShortcutGuard(false, true)).toBe(true);
    expect(shouldInstallWebviewShortcutGuard(true, true)).toBe(false);
    expect(shouldInstallWebviewShortcutGuard(false, false)).toBe(false);
  });

  it.each([
    shortcut("F5"),
    shortcut("F12"),
    shortcut("r", { metaKey: true }),
    shortcut("R", { metaKey: true, shiftKey: true }),
    shortcut("r", { ctrlKey: true }),
    shortcut("u", { metaKey: true, altKey: true }),
    shortcut("u", { ctrlKey: true }),
    shortcut("i", { metaKey: true, altKey: true }),
    shortcut("j", { ctrlKey: true, shiftKey: true }),
    shortcut("c", { ctrlKey: true, shiftKey: true }),
  ])("blocks reload, source, or developer shortcut $key", (event) => {
    expect(shouldBlockWebviewShortcut(event)).toBe(true);
  });

  it.each([
    shortcut("f", { metaKey: true }),
    shortcut("f", { ctrlKey: true }),
    shortcut("c", { metaKey: true }),
    shortcut("c", { ctrlKey: true }),
    shortcut("v", { metaKey: true }),
    shortcut("x", { metaKey: true }),
    shortcut("a", { metaKey: true }),
    shortcut("z", { metaKey: true }),
    shortcut("+", { metaKey: true }),
    shortcut("w", { metaKey: true }),
  ])("preserves standard product shortcut $key", (event) => {
    expect(shouldBlockWebviewShortcut(event)).toBe(false);
  });

  it("prevents blocked keys at the window capture boundary", () => {
    const uninstall = installWebviewShortcutGuard(window);
    const reload = new KeyboardEvent("keydown", { key: "F5", cancelable: true });
    const find = new KeyboardEvent("keydown", {
      key: "f",
      metaKey: true,
      cancelable: true,
    });

    window.dispatchEvent(reload);
    window.dispatchEvent(find);

    expect(reload.defaultPrevented).toBe(true);
    expect(find.defaultPrevented).toBe(false);
    uninstall();
  });

  it("preserves the native context menu for editing and selected text", () => {
    const uninstall = installWebviewShortcutGuard(window);
    const input = document.createElement("input");
    const textarea = document.createElement("textarea");
    const editable = document.createElement("div");
    editable.setAttribute("contenteditable", "true");
    const copyable = document.createElement("p");
    copyable.textContent = "copy this text";
    document.body.append(input, textarea, editable, copyable);

    try {
      for (const target of [input, textarea, editable]) {
        const contextMenu = new MouseEvent("contextmenu", {
          bubbles: true,
          button: 2,
          cancelable: true,
        });
        target.dispatchEvent(contextMenu);
        expect(contextMenu.defaultPrevented).toBe(false);
      }

      const selection = window.getSelection();
      const range = document.createRange();
      range.selectNodeContents(copyable);
      selection?.removeAllRanges();
      selection?.addRange(range);

      const selectedTextMenu = new MouseEvent("contextmenu", {
        bubbles: true,
        button: 2,
        cancelable: true,
      });
      copyable.dispatchEvent(selectedTextMenu);
      expect(selectedTextMenu.defaultPrevented).toBe(false);
    } finally {
      window.getSelection()?.removeAllRanges();
      input.remove();
      textarea.remove();
      editable.remove();
      copyable.remove();
      uninstall();
    }
  });

  it("suppresses the native menu on ordinary surfaces without blocking app handlers", () => {
    const uninstall = installWebviewShortcutGuard(window);
    const tool = document.createElement("button");
    let appHandlerCalled = false;
    let appHandlerSawPreventedEvent = false;
    tool.addEventListener("contextmenu", (event) => {
      appHandlerCalled = true;
      appHandlerSawPreventedEvent = event.defaultPrevented;
    });
    document.body.appendChild(tool);

    try {
      const contextMenu = new MouseEvent("contextmenu", {
        bubbles: true,
        button: 2,
        cancelable: true,
      });
      tool.dispatchEvent(contextMenu);

      expect(contextMenu.defaultPrevented).toBe(true);
      expect(appHandlerCalled).toBe(true);
      expect(appHandlerSawPreventedEvent).toBe(true);
    } finally {
      tool.remove();
      uninstall();
    }
  });
});
