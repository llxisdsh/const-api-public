import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";

export const TOOL_DOCK_CLOSE_DELAY_MS = 320;

type ToolDockMenuContextValue = {
  openMenuId: string | null;
  contextPanelOpen: boolean;
  setContextPanelOpen: (open: boolean) => void;
  openMenu: (id: string) => void;
  pinMenu: (id: string) => void;
  togglePinnedMenu: (id: string) => void;
  closeMenu: () => void;
  scheduleClose: () => void;
};

const ToolDockMenuContext = createContext<ToolDockMenuContextValue | null>(null);

export function ToolDockMenuProvider({ children }: { children: ReactNode }) {
  const [openMenuId, setOpenMenuId] = useState<string | null>(null);
  const [contextPanelOpen, setContextPanelOpen] = useState(false);
  const menuActive = openMenuId !== null;
  const openMenuIdRef = useRef<string | null>(null);
  const pinnedMenuIdRef = useRef<string | null>(null);
  const closeTimer = useRef<number | null>(null);

  const clearCloseTimer = useCallback(() => {
    if (closeTimer.current === null) return;
    window.clearTimeout(closeTimer.current);
    closeTimer.current = null;
  }, []);

  const openMenu = useCallback((id: string) => {
    clearCloseTimer();
    openMenuIdRef.current = id;
    setOpenMenuId((current) => current === id ? current : id);
  }, [clearCloseTimer]);

  const pinMenu = useCallback((id: string) => {
    clearCloseTimer();
    pinnedMenuIdRef.current = id;
    openMenuIdRef.current = id;
    setOpenMenuId((current) => current === id ? current : id);
  }, [clearCloseTimer]);

  const togglePinnedMenu = useCallback((id: string) => {
    clearCloseTimer();
    const next = pinnedMenuIdRef.current === id ? null : id;
    pinnedMenuIdRef.current = next;
    openMenuIdRef.current = next;
    setOpenMenuId(next);
    if (next === null) setContextPanelOpen(false);
  }, [clearCloseTimer]);

  const closeMenu = useCallback(() => {
    clearCloseTimer();
    pinnedMenuIdRef.current = null;
    openMenuIdRef.current = null;
    setOpenMenuId(null);
    setContextPanelOpen(false);
  }, [clearCloseTimer]);

  const scheduleClose = useCallback(() => {
    clearCloseTimer();
    if (pinnedMenuIdRef.current === openMenuIdRef.current && pinnedMenuIdRef.current !== null) return;
    closeTimer.current = window.setTimeout(() => {
      const pinnedMenuId = pinnedMenuIdRef.current;
      openMenuIdRef.current = pinnedMenuId;
      setOpenMenuId(pinnedMenuId);
      if (pinnedMenuId === null) setContextPanelOpen(false);
      closeTimer.current = null;
    }, TOOL_DOCK_CLOSE_DELAY_MS);
  }, [clearCloseTimer]);

  useEffect(() => clearCloseTimer, [clearCloseTimer]);

  useEffect(() => {
    if (!menuActive) return;
    const closeOnOutsidePointerDown = (event: PointerEvent) => {
      const target = event.target;
      if (target instanceof Element && target.closest(".tool-dock")) return;
      closeMenu();
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      closeMenu();
    };
    document.addEventListener("pointerdown", closeOnOutsidePointerDown);
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("pointerdown", closeOnOutsidePointerDown);
      document.removeEventListener("keydown", closeOnEscape);
    };
  }, [closeMenu, menuActive]);

  const value = useMemo(() => ({
    openMenuId,
    contextPanelOpen,
    setContextPanelOpen,
    openMenu,
    pinMenu,
    togglePinnedMenu,
    closeMenu,
    scheduleClose,
  }), [closeMenu, contextPanelOpen, openMenu, openMenuId, pinMenu, scheduleClose, togglePinnedMenu]);

  return <ToolDockMenuContext.Provider value={value}>{children}</ToolDockMenuContext.Provider>;
}

export function useToolDockMenu() {
  const value = useContext(ToolDockMenuContext);
  if (!value) throw new Error("useToolDockMenu must be used inside ToolDockMenuProvider");
  return value;
}
