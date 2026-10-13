import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { Lock, Plus, RefreshCw, RotateCcw, Save, Trash2 } from "lucide-react";
import { DialogBackdrop } from "../DialogBackdrop";
import { ModalCloseButton } from "../ModalCloseButton";
import { useEscapeDismiss } from "../useEscapeDismiss";
import { InlineActionFeedback, type InlineActionFeedbackValue } from "../account/InlineActionFeedback";
import { localizedError } from "../i18n";
import {
  validToolModelAddition,
  type ToolModelInjectionSettings as Settings,
  type ToolModelRecommendations,
  type ToolModelSyncResult,
} from "../toolModelInjection";

type Props = {
  settings: Settings;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSave: (settings: Settings, onProgress: (tool: string) => void, syncTools?: boolean) => Promise<ToolModelSyncResult>;
};

export function ToolModelInjectionSettings({ settings, open, onOpenChange, onSave }: Props) {
  const { t } = useTranslation();
  const [draft, setDraft] = useState<Settings | null>(null);
  const [saving, setSaving] = useState(false);
  const [progress, setProgress] = useState("");
  const [feedback, setFeedback] = useState<InlineActionFeedbackValue | null>(null);
  const value = draft ?? settings;
  const dirty = value.all_models !== settings.all_models
    || value.extra_models.length !== settings.extra_models.length
    || value.extra_models.some((model, index) => model !== settings.extra_models[index]);
  function change(next: Settings) {
    setDraft(next);
    setFeedback(null);
  }
  async function toggleAllModels(all_models: boolean) {
    setSaving(true);
    setFeedback(null);
    try {
      // The Use-page switch saves immediately, without bulk tool writes or
      // committing unconfirmed additions left in the list editor.
      await onSave({ ...settings, all_models }, setProgress, false);
      setDraft((current) => current ? { ...current, all_models } : null);
    } catch (error) {
      setFeedback({ tone: "error", text: localizedError(error) });
    } finally {
      setProgress("");
      setSaving(false);
    }
  }
  async function save() {
    setSaving(true);
    setFeedback(null);
    try {
      const result = await onSave(value, setProgress);
      setDraft(null);
      setFeedback(result.errors.length
        ? { tone: "error", text: t("toolModelInjection.partial", { errors: result.errors.join("\n") }) }
        : { tone: "success", text: t("toolModelInjection.saved", { count: result.synced }) });
    } catch (error) {
      setFeedback({ tone: "error", text: localizedError(error) });
    } finally {
      setProgress("");
      setSaving(false);
    }
  }
  const status = saving ? { tone: "info" as const, text: progress
    ? t("toolModelInjection.syncing", { tool: progress }) : t("common.saving") } : feedback;
  const canSave = dirty || feedback?.tone === "error";
  const saveButton = <button type="button" className={canSave ? "primary" : "quiet"} disabled={saving || !canSave}
    onClick={() => void save()}><Save size={14} />{t("toolModelInjection.saveAndSync")}</button>;
  return <>
    <div className="model-equivalence-toggle advanced-strategy-toggle">
      <label className="model-equivalence-check">
        <input type="checkbox" checked={value.all_models} disabled={saving}
          onChange={(event) => void toggleAllModels(event.target.checked)} />
        <span><strong>{t("toolModelInjection.allModels")}</strong><small>{t("toolModelInjection.hint")}</small></span>
      </label>
      <button type="button" className="quiet model-alias-preview-button" onClick={() => onOpenChange(true)}>
        {t("toolModelInjection.editList")}
      </button>
    </div>
    {!open && <InlineActionFeedback feedback={status} />}
    {open && createPortal(<InjectionEditor value={value} onChange={change} saving={saving} feedback={status}
      saveButton={saveButton} onClose={() => { if (!saving) onOpenChange(false); }} />, document.body)}
  </>;
}

function InjectionEditor({ value, onChange, saving, feedback, saveButton, onClose }: {
  value: Settings;
  onChange: (settings: Settings) => void;
  saving: boolean;
  feedback: InlineActionFeedbackValue | null;
  saveButton: ReactNode;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  useEscapeDismiss(onClose);
  const [catalog, setCatalog] = useState<ToolModelRecommendations | null>(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(true);
  const [addition, setAddition] = useState("");
  // The editor is mounted only on demand; loading is an external IPC effect.
  const load = useCallback(async () => {
    setLoading(true);
    try {
      setCatalog(await invoke<ToolModelRecommendations>("get_tool_model_recommendations"));
      setError("");
    } catch (reason) {
      setError(localizedError(reason));
    } finally {
      setLoading(false);
    }
  }, []);
  useEffect(() => { void load(); }, [load]);
  const normalized = addition.trim().toLowerCase();
  const valid = validToolModelAddition(normalized, [...(catalog?.patterns ?? []), ...value.extra_models]);
  const add = () => {
    if (!valid || value.extra_models.length >= 128) return;
    onChange({ ...value, extra_models: [...value.extra_models, normalized] });
    setAddition("");
  };
  return <DialogBackdrop aria-labelledby="tool-model-injection-title">
    <div className="modal-panel model-alias-modal compatibility-editor-modal">
      <div className="section-title-row">
        <div><h2 id="tool-model-injection-title">{t("toolModelInjection.title")}</h2>
          <p className="section-hint">{t("toolModelInjection.editorHint")}</p></div>
        <ModalCloseButton onClick={onClose} disabled={saving} />
      </div>
      {value.all_models && <p className="model-rule-note">{t("toolModelInjection.allModelsNote")}</p>}
      {loading && <p role="status">{t("common.loading")}</p>}
      {error && <div className="test-result error compatibility-editor-load-error" role="alert">
        <span>{t("toolModelInjection.loadFailed", { error })}</span>
        <button type="button" className="quiet" disabled={loading} onClick={() => void load()}>
          <RefreshCw size={14} />{t("common.retry")}</button>
      </div>}
      <div className="compatibility-editor-groups">
        <section className="model-rule-section">
          <div className="compatibility-editor-group-header"><h3>{t("toolModelInjection.recommended")}</h3>
            <Lock size={14} aria-label={t("toolModelInjection.fixed")} /></div>
          <ul className="model-rule-patterns" aria-label={t("toolModelInjection.recommended")}>{catalog?.patterns.map((pattern) => <li key={pattern}><code>{pattern}</code></li>)}</ul>
          {catalog?.patterns.length === 0 && <p className="section-hint">{t("toolModelInjection.noRecommendations")}</p>}
        </section>
        <section className="model-rule-section">
          <div className="compatibility-editor-group-header"><h3>{t("toolModelInjection.additions")}</h3></div>
          {value.extra_models.length > 0 && <ol className="model-rule-list" aria-label={t("toolModelInjection.additions")}>{value.extra_models.map((model, index) => <li className="model-rule-row" key={model}>
            <span className="compatibility-model-order" aria-hidden="true">{index + 1}</span><code title={model}>{model}</code>
            <button type="button" className="quiet danger model-rule-remove" disabled={saving}
              aria-label={t("toolModelInjection.remove", { model })} title={t("toolModelInjection.remove", { model })}
              onClick={() => onChange({ ...value, extra_models: value.extra_models.filter((item) => item !== model) })}>
              <Trash2 size={14} /></button>
          </li>)}</ol>}
          <form className="compatibility-model-add" onSubmit={(event) => { event.preventDefault(); add(); }}>
            <input aria-label={t("toolModelInjection.modelInput")} placeholder={t("toolModelInjection.placeholder")}
              list="tool-model-injection-options" maxLength={128} value={addition} disabled={saving}
              onChange={(event) => setAddition(event.target.value)} />
            <datalist id="tool-model-injection-options">{catalog?.available_models.map((model) => <option key={model} value={model} />)}</datalist>
            <button type="submit" className="quiet" disabled={saving || !valid || value.extra_models.length >= 128}>
              <Plus size={14} />{t("toolModelInjection.add")}</button>
          </form>
          {addition && !valid && <p className="section-hint">{t("toolModelInjection.invalid")}</p>}
        </section>
      </div>
      <InlineActionFeedback feedback={feedback} />
      <div className="compatibility-editor-footer">
        <span>{t("toolModelInjection.syncHint")}</span>
        <div className="actions secondary-actions compact-actions">
          <button type="button" className="quiet" disabled={saving || !value.extra_models.length}
            onClick={() => onChange({ ...value, extra_models: [] })}><RotateCcw size={14} />{t("toolModelInjection.restoreDefaults")}</button>
          {saveButton}
        </div>
      </div>
    </div>
  </DialogBackdrop>;
}
