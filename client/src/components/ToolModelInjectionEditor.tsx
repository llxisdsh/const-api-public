import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Lock, Plus, RefreshCw, RotateCcw, Trash2 } from "lucide-react";
import { DialogBackdrop } from "../DialogBackdrop";
import { ModalCloseButton } from "../ModalCloseButton";
import { useEscapeDismiss } from "../useEscapeDismiss";
import { InlineActionFeedback, type InlineActionFeedbackValue } from "../account/InlineActionFeedback";
import { localizedError } from "../i18n";
import { validToolModelAddition, type ToolModelInjectionSettings as Settings, type ToolModelRecommendations } from "../toolModelInjection";

export default function InjectionEditor({ value, onChange, saving, feedback, saveButton, onClose }: {
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
