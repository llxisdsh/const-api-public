import { Undo2 } from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type PointerEvent as ReactPointerEvent, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { MAX_LOCAL_PRIORITY, prioritiesForChannelDrop, withLocalPriorities, type PriorityDropTarget } from "../channelOrdering";

type PriorityChannel = { id: string; local_priority: number };
type Gesture = {
  id: string; pointerId: number;
  x: number; y: number; moved: boolean; target: PriorityDropTarget | null;
  insertionArea: { top: number; bottom: number } | null;
  offsetX: number; offsetY: number; width: number; height: number;
};
type Undo = { before: Record<string, number>; after: Record<string, number> };

export function ChannelPriorityTable<T extends PriorityChannel>({
  channels, allChannels, disabled, selectedId, header, empty, name, onOpen, onSave, children,
}: {
  channels: readonly T[];
  allChannels: readonly { id: string; local_priority?: number }[];
  disabled: boolean;
  selectedId: string;
  header: ReactNode;
  empty: ReactNode;
  name: (channel: T) => string;
  onOpen: (id: string) => void;
  onSave: (priorities: Readonly<Record<string, number>>) => Promise<boolean>;
  children: (channel: T, handle: ReactNode) => ReactNode;
}) {
  const { t } = useTranslation();
  const table = useRef<HTMLDivElement>(null);
  const gesture = useRef<Gesture | null>(null);
  const suppressClick = useRef(false);
  const savingRef = useRef(false);
  const layoutBefore = useRef<Map<string, number> | null>(null);
  const rowMotions = useRef(new Map<string, Animation>());
  const renderedOrder = useRef("");
  const [drag, setDrag] = useState<Gesture | null>(null);
  const [keyboardPreview, setKeyboardPreview] = useState<{ id: string; priority: number } | null>(null);
  const [pending, setPending] = useState<Record<string, number> | null>(null);
  const [saving, setSaving] = useState(false);
  const [undo, setUndo] = useState<Undo | null>(null);
  const [failed, setFailed] = useState(false);
  const current = useRef({ channels, allChannels, disabled, onSave });
  current.current = { channels, allChannels, disabled, onSave };
  const signature = JSON.stringify(allChannels.map((channel) => [channel.id, channel.local_priority]));
  function captureLayout() {
    if (!table.current) return;
    const top = table.current.getBoundingClientRect().top;
    layoutBefore.current = new Map([...table.current.querySelectorAll<HTMLElement>("[data-priority-channel]")]
      .map((row) => [row.dataset.priorityChannel!, row.getBoundingClientRect().top - top]));
    rowMotions.current.forEach((motion) => motion.cancel());
    rowMotions.current.clear();
  }
  const cancel = () => { captureLayout(); gesture.current = null; setDrag(null); setKeyboardPreview(null); };

  useLayoutEffect(() => {
    const previous = layoutBefore.current;
    layoutBefore.current = null;
    if (!previous || window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    const top = table.current!.getBoundingClientRect().top;
    for (const row of table.current!.querySelectorAll<HTMLElement>("[data-priority-channel]")) {
      const oldTop = previous.get(row.dataset.priorityChannel!);
      const offset = oldTop === undefined ? 0 : oldTop - (row.getBoundingClientRect().top - top);
      if (Math.abs(offset) < 1) continue;
      rowMotions.current.set(row.dataset.priorityChannel!, row.animate([
        { transform: `translateY(${offset}px)` }, { transform: "translateY(0)" },
      ], { duration: 160, easing: "ease-out" }));
    }
  });

  // Live health/name refreshes can continue during a drag. Configuration changes
  // cancel its snapshot, and an unrelated priority edit invalidates stale undo.
  useEffect(() => {
    cancel();
    const priorities = new Map(withLocalPriorities(allChannels).map((channel) => [channel.id, channel.local_priority]));
    setUndo((previous) => previous && Object.entries(previous.after)
      .every(([id, value]) => priorities.get(id) === value) ? previous : null);
  }, [signature]);
  useEffect(() => { if (disabled) cancel(); }, [disabled]);

  async function save(priorities: Record<string, number>, restoring = false) {
    if (savingRef.current || current.current.disabled || Object.keys(priorities).length === 0) return;
    const before = Object.fromEntries(withLocalPriorities(current.current.allChannels)
      .filter((channel) => channel.id in priorities).map((channel) => [channel.id, channel.local_priority]));
    savingRef.current = true;
    setSaving(true);
    setPending(priorities);
    setFailed(false);
    try {
      if (await current.current.onSave(priorities)) {
        setUndo(restoring ? null : { before, after: priorities });
      } else setFailed(true);
    } catch {
      setFailed(true);
    } finally {
      savingRef.current = false;
      setSaving(false);
      setPending(null);
    }
  }

  useEffect(() => {
    let frame = 0;
    function targetAt(x: number, y: number, id: string): PriorityDropTarget | null {
      if (!table.current) return null;
      const bounds = table.current.getBoundingClientRect();
      if (x < bounds.left || x > bounds.right) return null;
      const rows = [...table.current.querySelectorAll<HTMLElement>("[data-priority-channel]")];
      const sourceIndex = rows.findIndex((row) => row.dataset.priorityChannel === id);
      const sourcePriority = current.current.channels.find((channel) => channel.id === id)!.local_priority;
      const peers = current.current.channels.filter((channel) => channel.id !== id);
      const groups = peers.filter((channel, index) => index === 0 || peers[index - 1].local_priority !== channel.local_priority);
      // The top/bottom edges are drop zones too, including a vertical overshoot.
      // Use settled geometry so an animating row cannot undo this edge choice.
      const firstTop = bounds.top + table.current.clientTop + rows[0].offsetTop;
      const lastRow = rows[rows.length - 1];
      const lastBottom = bounds.top + table.current.clientTop + lastRow.offsetTop + lastRow.getBoundingClientRect().height;
      if (y <= firstTop + 8 || y >= lastBottom - 8) {
        gesture.current!.insertionArea = null;
        return { kind: "insert", beforeChannelId: y <= firstTop + 8 ? groups[0]?.id ?? null : null };
      }
      for (const row of rows) {
        const rect = row.getBoundingClientRect();
        // Hit-test settled layout, not the transient animation passing through.
        const rowTop = bounds.top + table.current.clientTop + row.offsetTop;
        // A gap belongs to the following row, so leaving a number into a gap
        // still gives a position preview instead of retaining a stale merge.
        if (y > rowTop + rect.height) continue;
        const rowId = row.dataset.priorityChannel!;
        const channel = current.current.channels.find((item) => item.id === rowId)!;
        const number = row.querySelector<HTMLElement>(".supplier-channel-priority")!.getBoundingClientRect();
        const inNumberColumn = x >= number.left && x <= number.right;
        const previousTarget = gesture.current!.target;
        const insert = (beforeChannelId: string | null): PriorityDropTarget => {
          gesture.current!.insertionArea = { top: rowTop - bounds.top, bottom: rowTop + rect.height - bounds.top };
          return { kind: "insert", beforeChannelId };
        };
        if (rowId === id) {
          // The open slot follows the preview. Staying in it must not undo the
          // move just because its former neighbour has now slid out of the way.
          if (previousTarget?.kind === "insert") return previousTarget;
          if (!previousTarget && inNumberColumn) return null;
          const groupPriority = previousTarget?.kind === "join"
            ? current.current.channels.find((item) => item.id === previousTarget.channelId)!.local_priority
            : sourcePriority;
          const groupIndex = groups.findIndex((item) => item.local_priority === groupPriority);
          if (groupIndex < 0) return null;
          return insert(y < rowTop + rect.height / 2
            ? groups[groupIndex].id : groups[groupIndex + 1]?.id ?? null);
        }
        const numberTop = rowTop + number.top - rect.top;
        if (inNumberColumn && y >= numberTop && y <= numberTop + number.height) {
          return { kind: "join", channelId: rowId };
        }
        // Keep an approach stable, but a merge ends at the number's boundary.
        if (inNumberColumn && previousTarget?.kind !== "join") return previousTarget;
        const area = gesture.current!.insertionArea;
        if (previousTarget?.kind === "insert" && area && y >= bounds.top + area.top && y <= bounds.top + area.bottom) {
          return previousTarget;
        }
        // Entering another tier moves past it; no need to cross its midpoint.
        // Within the source tier, either half can split the dragged peer out.
        const sourceGroup = previousTarget?.kind === "join"
          ? current.current.channels.find((item) => item.id === previousTarget.channelId)!.local_priority
          : previousTarget?.kind === "insert" ? null : sourcePriority;
        const before = channel.local_priority === sourceGroup
          ? y < rowTop + rect.height / 2 : rows.indexOf(row) < sourceIndex;
        const groupIndex = groups.findIndex((item) => item.local_priority === channel.local_priority);
        return insert(before ? groups[groupIndex].id : groups[groupIndex + 1]?.id ?? null);
      }
      return null;
    }
    function update(value: Gesture) {
      value.target = targetAt(value.x, value.y, value.id);
      if (value.target?.kind !== "insert") value.insertionArea = null;
      const values = value.target ? prioritiesForChannelDrop(current.current.allChannels, value.id, value.target) : {};
      const order = [...current.current.channels].sort((a, b) => (values[a.id] ?? a.local_priority) - (values[b.id] ?? b.local_priority));
      if (JSON.stringify(order.map((channel) => channel.id)) !== renderedOrder.current) captureLayout();
      setDrag({ ...value });
    }
    function autoScroll() {
      const value = gesture.current;
      if (!value?.moved || !table.current) return;
      let scroll = table.current.parentElement;
      while (scroll && !(scroll.scrollHeight > scroll.clientHeight && /auto|scroll/.test(getComputedStyle(scroll).overflowY))) {
        scroll = scroll.parentElement;
      }
      const element = scroll ?? document.scrollingElement;
      if (element) {
        const bounds = scroll?.getBoundingClientRect();
        const top = Math.max(0, bounds?.top ?? 0), bottom = Math.min(window.innerHeight, bounds?.bottom ?? window.innerHeight);
        const distance = value.y < top + 44 ? -Math.min(12, (top + 44 - value.y) / 3)
          : value.y > bottom - 44 ? Math.min(12, (value.y - bottom + 44) / 3) : 0;
        if (distance) {
          const previousTop = element.scrollTop;
          element.scrollTop += distance;
          if (element.scrollTop !== previousTop) update(value);
        }
      }
      frame = requestAnimationFrame(autoScroll);
    }
    function move(event: PointerEvent) {
      const value = gesture.current;
      if (!value || event.pointerId !== value.pointerId) return;
      if (!value.moved) {
        // A click can include a small pointer wobble; keep its gentle lift.
        if (Math.hypot(event.clientX - value.x, event.clientY - value.y) < 4) return;
        value.moved = true;
        suppressClick.current = true;
        frame = requestAnimationFrame(autoScroll);
      }
      value.x = event.clientX; value.y = event.clientY;
      event.preventDefault();
      update(value);
    }
    function finish(event: PointerEvent) {
      const value = gesture.current;
      if (!value) { setTimeout(() => { suppressClick.current = false; }, 0); return; }
      if (event.pointerId !== value.pointerId) return;
      const bounds = table.current!.getBoundingClientRect();
      // Keep the insertion preview stable, but only merge when the release is
      // still inside a number, including releases without a final pointermove.
      const target = event.clientX >= bounds.left && event.clientX <= bounds.right
        ? value.target?.kind === "join" ? targetAt(event.clientX, event.clientY, value.id) : value.target
        : null;
      cancel();
      cancelAnimationFrame(frame);
      if (value.moved && target) void save(prioritiesForChannelDrop(current.current.allChannels, value.id, target));
      // A captured pointer can still produce a click after its pointerup.
      setTimeout(() => { suppressClick.current = false; }, 0);
    }
    function abort() { cancel(); cancelAnimationFrame(frame); }
    function interrupt() { abort(); suppressClick.current = false; }
    function escape(event: KeyboardEvent) { if (event.key === "Escape" && gesture.current) { event.preventDefault(); abort(); } }
    window.addEventListener("pointermove", move, { passive: false });
    window.addEventListener("pointerup", finish);
    window.addEventListener("pointercancel", interrupt);
    window.addEventListener("blur", interrupt);
    window.addEventListener("keydown", escape);
    return () => {
      gesture.current = null;
      cancelAnimationFrame(frame);
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", finish);
      window.removeEventListener("pointercancel", interrupt);
      window.removeEventListener("blur", interrupt);
      window.removeEventListener("keydown", escape);
      rowMotions.current.forEach((motion) => motion.cancel());
    };
  }, []);

  function start(event: ReactPointerEvent<HTMLButtonElement>, id: string) {
    if (event.button !== 0 || disabled || savingRef.current) return;
    event.stopPropagation();
    setKeyboardPreview(null);
    suppressClick.current = false;
    event.currentTarget.setPointerCapture(event.pointerId);
    const bounds = event.currentTarget.closest<HTMLElement>("[data-priority-channel]")!.getBoundingClientRect();
    gesture.current = {
      id, pointerId: event.pointerId,
      x: event.clientX, y: event.clientY, moved: false, target: null, insertionArea: null,
      offsetX: event.clientX - bounds.left,
      offsetY: event.clientY - bounds.top,
      width: bounds.width, height: bounds.height,
    };
    setDrag({ ...gesture.current });
  }
  function keyDown(event: ReactKeyboardEvent<HTMLButtonElement>, channel: T) {
    if (disabled || savingRef.current || gesture.current) return;
    const value = keyboardPreview?.id === channel.id ? keyboardPreview.priority : channel.local_priority;
    if (event.key === "ArrowUp" || event.key === "ArrowDown") {
      event.preventDefault();
      setKeyboardPreview({ id: channel.id, priority: Math.max(1, Math.min(MAX_LOCAL_PRIORITY, value + (event.key === "ArrowUp" ? -1 : 1))) });
    } else if (event.key === "Enter" && keyboardPreview) {
      event.preventDefault();
      setKeyboardPreview(null);
      if (value !== channel.local_priority) void save({ [channel.id]: value });
    } else if (event.key === "Escape") {
      event.preventDefault();
      setKeyboardPreview(null);
    }
  }
  const target = drag?.target;
  const join = target?.kind === "join" ? channels.find((channel) => channel.id === target.channelId) : undefined;
  const dragged = channels.find((channel) => channel.id === drag?.id);
  const preview = drag?.target ? prioritiesForChannelDrop(allChannels, drag.id, drag.target)
    : keyboardPreview ? { [keyboardPreview.id]: keyboardPreview.priority } : pending ?? {};
  const priorityNumber = (channel: T) => preview[channel.id] ?? channel.local_priority;
  const orderedChannels = [...channels].sort((a, b) => priorityNumber(a) - priorityNumber(b));
  renderedOrder.current = JSON.stringify(orderedChannels.map((channel) => channel.id));
  const priorityClass = (channel: T) => `supplier-channel-priority${join && (join.id === channel.id || drag?.id === channel.id) ? " priority-number-target" : ""}`;
  const previewTop = drag ? Math.max(0, Math.min(drag.y - drag.offsetY - 3, window.innerHeight - drag.height)) : 0;
  // Once dragging, clear the pointer and its target number. Keep normal pointer
  // tracking immediate; animate only the extra lift away from the drop target.
  const previewLift = drag?.moved ? Math.min(previewTop, drag.height - drag.offsetY + 9) : 0;

  return <>
    <div ref={table} className={`supplier-channel-table${drag ? " priority-dragging" : ""}`} aria-busy={saving}>
      <div className="supplier-channel-header" aria-hidden="true">{header}</div>
      {orderedChannels.map((channel, index) => {
        const first = index === 0 || priorityNumber(orderedChannels[index - 1]) !== priorityNumber(channel);
        const sameAsPrevious = index > 0 && !first;
        return <div key={channel.id} data-priority-channel={channel.id}
          className={`supplier-channel-row${channel.id === selectedId ? " active" : ""}${sameAsPrevious ? " priority-same-group" : ""}${join && priorityNumber(join) === priorityNumber(channel) ? " priority-drop-join" : ""}${channel.id === drag?.id ? " priority-drag-source" : ""}`}
          onClick={() => { if (!suppressClick.current && !gesture.current?.moved) onOpen(channel.id); }}>
          {children(channel, <button type="button" className={priorityClass(channel)} disabled={disabled || saving}
            aria-label={t("supplier.priority.dragChannel", { channel: name(channel), priority: priorityNumber(channel) })}
            title={drag ? undefined : t("supplier.priority.dragChannel", { channel: name(channel), priority: priorityNumber(channel) })}
            aria-keyshortcuts="ArrowUp ArrowDown Enter Escape"
            onPointerDown={(event) => start(event, channel.id)}
            onKeyDown={(event) => keyDown(event, channel)} onBlur={() => setKeyboardPreview(null)}
            onClick={(event) => event.stopPropagation()}>
            <span>{priorityNumber(channel)}</span>
          </button>)}
        </div>;
      })}
      {channels.length === 0 && <div className="supplier-channel-empty">{empty}</div>}
    </div>
    <div className="supplier-priority-feedback" role="status" aria-live="polite">
      {drag ? null : keyboardPreview ? t("supplier.priority.keyboardHint") : saving ? t("common.saving") : failed ? t("supplier.priority.dragFailed") : undo ? <>
        <span>{t("supplier.priority.dragSaved")}</span>
        <button type="button" className="inline-text-link" disabled={disabled} onClick={() => void save(undo.before, true)}>
          <Undo2 size={13} />{t("supplier.priority.undo")}
        </button>
      </> : null}
    </div>
    {drag && dragged && createPortal(<div className="supplier-channel-table supplier-priority-drag-preview" aria-hidden="true" inert
      style={{ width: drag.width, left: Math.max(0, Math.min(drag.x - drag.offsetX, window.innerWidth - drag.width)),
        top: previewTop, transform: `translateY(${-previewLift}px)` }}>
      <div className="supplier-channel-row" style={{ minHeight: drag.height }}>
        {children(dragged, <span className={priorityClass(dragged)}>
          <span>{priorityNumber(dragged)}</span>
        </span>)}
      </div>
    </div>, document.body)}
  </>;
}
