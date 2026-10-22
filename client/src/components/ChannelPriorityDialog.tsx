import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { DialogBackdrop } from "../DialogBackdrop";
import { ModalCloseButton } from "../ModalCloseButton";
import { useEscapeDismiss } from "../useEscapeDismiss";
import { isValidLocalPriority, MAX_LOCAL_PRIORITY } from "../channelOrdering";

type PriorityChannel = { id: string; name: string; local_priority: number };

export function ChannelPriorityDialog({ channels, initialChannelId, onClose, onSave }: {
  channels: readonly PriorityChannel[];
  initialChannelId?: string;
  onClose: () => void;
  onSave: (priorities: Readonly<Record<string, number>>) => Promise<boolean>;
}) {
  const { t } = useTranslation();
  // Keep rows still while typing; apply only edited priorities to live config on save.
  const [rows] = useState(channels);
  const [draft, setDraft] = useState(() => Object.fromEntries(channels.map((c) => [c.id, String(c.local_priority)])));
  const [selected, setSelected] = useState<string[]>([]);
  const [batch, setBatch] = useState("1");
  const [saving, setSaving] = useState(false);
  const panel = useRef<HTMLFormElement>(null);
  const selectAll = useRef<HTMLInputElement>(null);
  const initialInput = useRef<HTMLInputElement>(null);
  const valid = Object.values(draft).every((value) => isValidLocalPriority(Number(value)));
  const edits = Object.fromEntries(rows.filter((row) => Number(draft[row.id]) !== row.local_priority)
    .map((row) => [row.id, Number(draft[row.id])]));
  const changed = Object.keys(edits).length > 0;
  useEscapeDismiss(onClose, !saving);

  useEffect(() => {
    const previous = document.activeElement;
    initialInput.current?.focus();
    initialInput.current?.select();
    return () => { if (previous instanceof HTMLElement && previous.isConnected) previous.focus(); };
  }, []);
  useEffect(() => {
    if (selectAll.current) selectAll.current.indeterminate = selected.length > 0 && selected.length < rows.length;
  }, [selected.length, rows.length]);

  async function save() {
    if (saving || !valid || !changed) return;
    setSaving(true);
    try { if (await onSave(edits)) onClose(); }
    finally { setSaving(false); }
  }

  return (
    <DialogBackdrop aria-labelledby="channel-priority-title" aria-describedby="channel-priority-hint">
      <form ref={panel} noValidate className="modal-panel channel-priority-dialog" onSubmit={(event) => { event.preventDefault(); void save(); }}
        onKeyDown={(event) => {
          if (event.key !== "Tab") return;
          const controls = panel.current?.querySelectorAll<HTMLElement>("button:not(:disabled), input:not(:disabled)");
          if (!controls?.length) return;
          const first = controls[0], last = controls[controls.length - 1];
          if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
          else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
        }}>
        <div className="channel-priority-heading">
          <h2 id="channel-priority-title">{t("supplier.priority.title")}</h2>
          <ModalCloseButton onClick={onClose} disabled={saving} />
        </div>
        <p id="channel-priority-hint" className="channel-priority-hint">{t("supplier.priority.hint")}</p>
        <div className="channel-priority-batch">
          <label className="channel-priority-select-all">
            <input ref={selectAll} type="checkbox" checked={rows.length > 0 && selected.length === rows.length} disabled={saving}
              onChange={(event) => setSelected(event.target.checked ? rows.map((row) => row.id) : [])} />
            {t("supplier.priority.selectAll")}
          </label>
          <span className="channel-priority-selection">{t("supplier.priority.selected", { count: selected.length })}</span>
          <label htmlFor="channel-priority-batch">{t("supplier.priority.setTo")}</label>
          <input id="channel-priority-batch" type="number" min={1} max={MAX_LOCAL_PRIORITY} step={1}
            value={batch} disabled={saving} onChange={(event) => setBatch(event.target.value)} />
          <button type="button" className="quiet" disabled={saving || selected.length === 0 || !isValidLocalPriority(Number(batch))}
            onClick={() => setDraft((current) => ({ ...current, ...Object.fromEntries(selected.map((id) => [id, batch])) }))}>
            {t("common.apply")}
          </button>
        </div>
        <div className="channel-priority-list">
          <div className="channel-priority-row channel-priority-columns" aria-hidden="true">
            <span /><span>{t("supplier.channel")}</span><span>{t("supplier.localPriority")}</span>
          </div>
          {rows.map((row, index) => (
            <div className="channel-priority-row" key={row.id}>
              <input type="checkbox" aria-label={t("supplier.priority.selectChannel", { channel: row.name })}
                checked={selected.includes(row.id)} disabled={saving}
                onChange={(event) => setSelected((current) => event.target.checked ? [...current, row.id] : current.filter((id) => id !== row.id))} />
              <label htmlFor={`channel-priority-${row.id}`}>{row.name}</label>
              <input id={`channel-priority-${row.id}`} ref={row.id === initialChannelId || (!initialChannelId && index === 0) ? initialInput : undefined}
                type="number" min={1} max={MAX_LOCAL_PRIORITY} step={1} value={draft[row.id]} disabled={saving}
                aria-label={t("supplier.priority.channelValue", { channel: row.name })}
                aria-invalid={!isValidLocalPriority(Number(draft[row.id]))}
                onChange={(event) => setDraft((current) => ({ ...current, [row.id]: event.target.value }))} />
            </div>
          ))}
        </div>
        {!valid && <p className="channel-priority-error" role="alert">{t("supplier.priority.invalid")}</p>}
        <div className="channel-priority-footer">
          <small>{t("supplier.priority.scope")}</small>
          <div className="actions">
            <button type="button" className="quiet" disabled={saving} onClick={onClose}>{t("common.cancel")}</button>
            <button type="submit" className="primary" disabled={saving || !valid || !changed}>{t(saving ? "common.saving" : "common.save")}</button>
          </div>
        </div>
      </form>
    </DialogBackdrop>
  );
}
