import { useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import mediaSkillTools from "../../tool-media-skill-tools.json";
import { DialogBackdrop } from "../DialogBackdrop";
import { ModalCloseButton } from "../ModalCloseButton";
import { localizedError } from "../i18n";
import { TOOL_CATALOG, TOOL_CATALOG_ORDER } from "../toolCatalog";
import { useEscapeDismiss } from "../useEscapeDismiss";

const configurableTools = TOOL_CATALOG_ORDER.filter((id) => id in mediaSkillTools);
const otherTools = TOOL_CATALOG_ORDER.filter((id) => !(id in mediaSkillTools));

export function MediaSkillToolsDialog({ selected, onSave, onClose }: {
  selected: string[];
  onSave: (tools: string[]) => Promise<void>;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const [edits, setEdits] = useState(() => new Map<string, boolean>());
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  // Settings can finish loading after this dialog opens. Only override explicit edits.
  const draft = new Set(selected);
  for (const [id, checked] of edits) {
    if (checked) draft.add(id);
    else draft.delete(id);
  }
  const changed = selected.length !== draft.size || selected.some((id) => !draft.has(id));
  useEscapeDismiss(() => { if (!saving) onClose(); });
  function toggle(id: string, checked: boolean) {
    setEdits((current) => {
      const next = new Map(current);
      if (checked === selected.includes(id)) next.delete(id);
      else next.set(id, checked);
      return next;
    });
  }
  async function save() {
    setSaving(true);
    setError("");
    try {
      await onSave([...draft]);
      onClose();
    } catch (reason) {
      setError(localizedError(reason));
      setSaving(false);
    }
  }
  return createPortal(<DialogBackdrop aria-labelledby="media-skill-tools-title" aria-describedby="media-skill-tools-hint">
    <div className="modal-panel model-alias-modal media-skill-tools-modal">
      <div className="section-title-row">
        <div><h2 id="media-skill-tools-title">{t("access.mediaSkillTools")}</h2>
          <p className="section-hint" id="media-skill-tools-hint">{t("access.mediaSkillToolsHint")}</p></div>
        <ModalCloseButton onClick={onClose} disabled={saving} />
      </div>
      <div className="media-skill-tool-list">
        <div className="media-skill-tool-grid">
          {configurableTools.map((id) => (
            <div className="model-equivalence-check" key={id}>
              <input type="checkbox" aria-labelledby={`media-skill-${id}`} checked={draft.has(id)} disabled={saving}
                onChange={(event) => toggle(id, event.currentTarget.checked)} />
              <span id={`media-skill-${id}`}>{TOOL_CATALOG[id].title}</span>
            </div>
          ))}
        </div>
        {otherTools.length > 0 && <section className="media-skill-other-tools" aria-labelledby="media-skill-other-title">
          <h3 id="media-skill-other-title">{t("access.mediaSkillOtherTools")}</h3>
          <ul className="media-skill-tool-grid">
            {otherTools.map((id) => <li key={id}>{TOOL_CATALOG[id].title}</li>)}
          </ul>
        </section>}
      </div>
      {error && <p className="test-result error" role="alert">{error}</p>}
      <div className="confirm-actions">
        <button type="button" className="primary" disabled={saving || !changed} onClick={() => void save()}>{t("common.save")}</button>
      </div>
    </div>
  </DialogBackdrop>, document.body);
}
