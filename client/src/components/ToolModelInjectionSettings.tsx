import { lazy, Suspense, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { Save } from "lucide-react";
import { InlineActionFeedback, type InlineActionFeedbackValue } from "../account/InlineActionFeedback";
import { localizedError } from "../i18n";
import type { ToolModelInjectionSettings as Settings, ToolModelSyncResult } from "../toolModelInjection";

const InjectionEditor = lazy(() => import("./ToolModelInjectionEditor"));

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
      <div className="model-equivalence-check">
        <input type="checkbox" checked={value.all_models} disabled={saving}
          aria-label={t("toolModelInjection.allModels")}
          onChange={(event) => void toggleAllModels(event.target.checked)} />
        <span>
          <span className="model-equivalence-heading">
            <strong>{t("toolModelInjection.allModels")}</strong>
            <button type="button" className="inline-text-link model-alias-preview-button" onClick={() => onOpenChange(true)}>
              {t("toolModelInjection.editList")}
            </button>
          </span>
          <small>{t("toolModelInjection.hint")}</small>
        </span>
      </div>
    </div>
    {!open && <InlineActionFeedback feedback={feedback} />}
    {open && createPortal(
      <Suspense fallback={null}>
        <InjectionEditor value={value} onChange={change} saving={saving} feedback={status}
          saveButton={saveButton} onClose={() => { if (!saving) onOpenChange(false); }} />
      </Suspense>, document.body)}
  </>;
}
