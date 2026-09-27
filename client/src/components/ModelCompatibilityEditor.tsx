import { useMemo, useState, type Dispatch, type SetStateAction } from "react";
import { useTranslation } from "react-i18next";
import { ArrowDown, ArrowUp, ChevronDown, Lock, Plus, RefreshCw, RotateCcw, Save, Trash2 } from "lucide-react";
import { DialogBackdrop } from "../DialogBackdrop";
import { ModalCloseButton } from "../ModalCloseButton";
import { useEscapeDismiss } from "../useEscapeDismiss";
import type {
  ModelCompatibilityCatalogModel,
  ModelCompatibilityGroup,
  ModelCompatibilityPolicyState,
} from "../appTypes";

type ModelCompatibilityEditorProps = {
  enabled: boolean;
  policy: ModelCompatibilityPolicyState;
  draft: ModelCompatibilityGroup[];
  setDraft: Dispatch<SetStateAction<ModelCompatibilityGroup[]>>;
  saving: boolean;
  onClose: () => void;
  onRefresh: () => void | Promise<void>;
  onSave: () => void | Promise<void>;
  onReset: () => void | Promise<void>;
};

function modelKey(value: string) {
  return value.trim().toLowerCase();
}

function policySignature(groups: ModelCompatibilityGroup[]) {
  return JSON.stringify(groups.map((group) => ({
    id: group.id,
    disabled: Boolean(group.disabled),
    models: group.models,
  })));
}

function supplyRank(
  model: ModelCompatibilityCatalogModel,
  localModels: Set<string>,
  platformModels: Set<string>,
) {
  const key = modelKey(model.id);
  if (localModels.has(key) && platformModels.has(key)) return 0;
  if (localModels.has(key)) return 1;
  if (platformModels.has(key)) return 2;
  return 3;
}

export function compatibilityCatalogCandidates(
  catalog: ModelCompatibilityCatalogModel[],
  localModelIds: string[],
) {
  const candidates = [...catalog];
  const seen = new Set(candidates.map((model) => modelKey(model.id)));
  for (const id of localModelIds) {
    const normalized = id.trim();
    const key = modelKey(normalized);
    if (!normalized || seen.has(key)) continue;
    seen.add(key);
    candidates.push({ id: normalized, display_name: normalized, vendor: "local" });
  }
  return candidates;
}

export function ModelCompatibilityEditor({
  enabled,
  policy,
  draft,
  setDraft,
  saving,
  onClose,
  onRefresh,
  onSave,
  onReset,
}: ModelCompatibilityEditorProps) {
  useEscapeDismiss(onClose);
  const { t } = useTranslation();
  const [addSelections, setAddSelections] = useState<Record<string, string>>({});
  const [collapsedGroups, setCollapsedGroups] = useState<Set<string>>(() => new Set());
  const localModels = useMemo(
    () => new Set(policy.localAvailableModels.map(modelKey)),
    [policy.localAvailableModels],
  );
  const platformModels = useMemo(
    () => new Set(policy.platformAvailableModels.map(modelKey)),
    [policy.platformAvailableModels],
  );
  const usedModels = useMemo(
    () => new Set(draft.flatMap((group) => group.models.map(modelKey))),
    [draft],
  );
  const systemModelsByGroup = useMemo(
    () => new Map(policy.templateModelGroups.map((group) => [
      modelKey(group.id),
      new Set(group.models.map(modelKey)),
    ])),
    [policy.templateModelGroups],
  );
  const systemModels = useMemo(
    () => new Set(policy.templateModelGroups.flatMap((group) => [
      ...group.models.map(modelKey),
      ...(group.match_models ?? []).map(modelKey),
    ])),
    [policy.templateModelGroups],
  );
  const sortedCatalog = useMemo(
    () => compatibilityCatalogCandidates(policy.catalogModels, policy.localAvailableModels).sort((left, right) => {
      const rank = supplyRank(left, localModels, platformModels) - supplyRank(right, localModels, platformModels);
      return rank || left.id.localeCompare(right.id);
    }),
    [policy.catalogModels, policy.localAvailableModels, localModels, platformModels],
  );
  const hasChanges = policySignature(draft) !== policySignature(policy.modelGroups);
  const platformSyncAvailable = policy.platformSyncAvailable !== false;
  // In local mode the persisted profile may be an older non-customized cache.
  // Reset stays available so it can be replaced by the active shared catalog
  // snapshot (signed public LKG or its same-source packaged copy).
  // Reset is deliberately idempotent and always available. Besides clearing
  // visible edits, it is the user's repair action when a stale server/cache
  // incorrectly reports `customized: false`.
  const resetLabel = policy.customized === true || hasChanges
    ? t("modelCompatibility.clearCustom")
    : t("modelCompatibility.restoreCurrentDefault");
  const syncLabel = policy.syncStatus === "pending"
    ? platformSyncAvailable ? t("modelCompatibility.sync.pending") : t("modelCompatibility.sync.afterSignIn")
    : policy.syncStatus === "conflict"
      ? t("modelCompatibility.sync.conflict")
      : platformSyncAvailable && policy.syncStatus !== "local"
        ? t("modelCompatibility.sync.synced")
        : t("modelCompatibility.sync.local");
  const policySourceLabel = policy.source === "user"
    ? t("modelCompatibility.source.platformCustom")
    : policy.source === "local_user"
      ? t("modelCompatibility.source.localCustom")
      : policy.source === "client"
        ? t("modelCompatibility.source.clientDefault")
        : t("modelCompatibility.source.server");
  const localizedGroupDescription = (group: ModelCompatibilityGroup, groupIndex: number) => {
    if (/^T[0-7]$/i.test(group.label || "")) {
      return groupIndex === 0
        ? t("modelCompatibility.tierDescriptionTop", { tier: group.label })
        : t("modelCompatibility.tierDescription", { tier: group.label });
    }
    return group.description;
  };

  function updateGroup(groupId: string, update: (group: ModelCompatibilityGroup) => ModelCompatibilityGroup) {
    setDraft((current) => current.map((group) => group.id === groupId ? update(group) : group));
  }

  function moveModel(groupId: string, index: number, direction: -1 | 1) {
    updateGroup(groupId, (group) => {
      const target = index + direction;
      if (target < 0 || target >= group.models.length) return group;
      const systemInGroup = systemModelsByGroup.get(modelKey(groupId)) ?? new Set<string>();
      if (systemInGroup.has(modelKey(group.models[index])) || systemInGroup.has(modelKey(group.models[target]))) {
        return group;
      }
      const models = [...group.models];
      [models[index], models[target]] = [models[target], models[index]];
      return { ...group, models };
    });
  }

  function removeModel(groupId: string, model: string) {
    if (systemModels.has(modelKey(model))) return;
    updateGroup(groupId, (group) => {
      const models = group.models.filter((candidate) => modelKey(candidate) !== modelKey(model));
      return { ...group, models, disabled: false };
    });
  }

  function addModel(groupId: string, fallbackModel: string) {
    const selected = addSelections[groupId] || fallbackModel;
    if (!selected || systemModels.has(modelKey(selected)) || usedModels.has(modelKey(selected))) return;
    updateGroup(groupId, (group) => ({
      ...group,
      models: [...group.models, selected],
    }));
    setAddSelections((current) => ({ ...current, [groupId]: "" }));
  }

  function toggleGroup(groupId: string) {
    setCollapsedGroups((current) => {
      const next = new Set(current);
      if (next.has(groupId)) {
        next.delete(groupId);
      } else {
        next.add(groupId);
      }
      return next;
    });
  }

  return (
    <DialogBackdrop aria-labelledby="model-compatibility-title">
      <div className="modal-panel model-alias-modal compatibility-editor-modal">
        <div className="section-title-row">
          <div>
            <h2 id="model-compatibility-title">{t("modelCompatibility.title")}</h2>
            <p className="section-hint">
              {t("modelCompatibility.hint")}
            </p>
          </div>
          <ModalCloseButton onClick={onClose} />
        </div>

        {!enabled && (
          <div className="test-result">
            {t("modelCompatibility.disabledHint")}
          </div>
        )}

        {policy.status === "loading" && <div className="test-result">{t("modelCompatibility.loading")}</div>}
        {policy.status === "error" && (
          <div className="test-result error compatibility-editor-load-error">
            <span>{t("modelCompatibility.unavailable", { error: policy.error || t("modelCompatibility.unknownError") })}</span>
            <button type="button" className="quiet" onClick={() => void onRefresh()}>
              <RefreshCw size={14} />{t("common.retry")}
            </button>
          </div>
        )}

        {policy.status === "ready" && (
          <>
            <div className="compatibility-editor-summary">
              <span>{t("modelCompatibility.currentPolicy", { source: policySourceLabel })}</span>
              <span className={`sync-${policy.syncStatus ?? "synced"}`}>{syncLabel}</span>
              <span>{t("modelCompatibility.template", { version: policy.baseReleaseId || t("common.unknown") })}</span>
              <button
                type="button"
                className="quiet"
                title={hasChanges ? t("modelCompatibility.saveBeforeRefresh") : t("modelCompatibility.refreshHint")}
                onClick={() => void onRefresh()}
                disabled={saving || hasChanges}
              >
                <RefreshCw size={13} />{t("modelCompatibility.refreshSupply")}
              </button>
            </div>

            {!platformSyncAvailable && (
              <div className="test-result compatibility-editor-policy-note">
                {t("modelCompatibility.localPolicyHint")}
              </div>
            )}

            {policy.templateChanged && (
              <div className="test-result">{t("modelCompatibility.templateChanged")}</div>
            )}
            {policy.syncError && (
              <div className={`test-result ${policy.syncStatus === "conflict" ? "error" : ""}`}>
                {policy.syncError}
              </div>
            )}

            <div className="compatibility-editor-legend">
              <div className="compatibility-editor-legend-status">
                <span><i className="supply-dot local" />{t("modelCompatibility.legendLocal")}</span>
                <span><i className="supply-dot platform" />{t("modelCompatibility.legendPlatform")}</span>
              </div>
              <small>{t("modelCompatibility.legendHint")}</small>
            </div>

            <div className="compatibility-editor-groups">
              {draft.map((group, groupIndex) => {
                const addable = sortedCatalog.filter((model) =>
                  !usedModels.has(modelKey(model.id)) && !systemModels.has(modelKey(model.id)));
                const selected = addSelections[group.id] && addable.some((model) => model.id === addSelections[group.id])
                  ? addSelections[group.id]
                  : addable[0]?.id ?? "";
                const higherGroups = draft
                  .slice(0, groupIndex)
                  .reverse()
                  .map((candidate) => candidate.label || candidate.id);
                const groupLabel = group.label || group.id;
                const groupHeadingId = `compatibility-group-${groupIndex}-heading`;
                const groupBodyId = `compatibility-group-${groupIndex}-body`;
                const collapsed = collapsedGroups.has(group.id);
                return (
                  <article className="compatibility-editor-group" key={group.id}>
                    <div className="compatibility-editor-group-header">
                      <div>
                        <h3 id={groupHeadingId}>{groupLabel}</h3>
                        {localizedGroupDescription(group, groupIndex) && (
                          <p>{localizedGroupDescription(group, groupIndex)}</p>
                        )}
                      </div>
                      <button
                        type="button"
                        className="compatibility-group-toggle"
                        aria-controls={groupBodyId}
                        aria-expanded={!collapsed}
                        aria-label={t(
                          collapsed
                            ? "modelCompatibility.expandGroup"
                            : "modelCompatibility.collapseGroup",
                          { group: groupLabel },
                        )}
                        title={t(
                          collapsed
                            ? "modelCompatibility.expandGroup"
                            : "modelCompatibility.collapseGroup",
                          { group: groupLabel },
                        )}
                        onClick={() => toggleGroup(group.id)}
                      >
                        <ChevronDown
                          aria-hidden="true"
                          className={collapsed ? "is-collapsed" : undefined}
                          size={18}
                        />
                      </button>
                    </div>

                    <div
                      id={groupBodyId}
                      className="compatibility-editor-group-body"
                      role="region"
                      aria-labelledby={groupHeadingId}
                      hidden={collapsed}
                    >
                    <div className="compatibility-group-fallback">
                      {groupIndex === 0
                        ? t("modelCompatibility.topFallback")
                        : higherGroups.length > 0
                          ? t("modelCompatibility.upwardFallback", { groups: higherGroups.join(" → ") })
                          : t("modelCompatibility.noUpperGroup")}
                    </div>

                    <div className="compatibility-model-rows">
                      {group.models.map((model, index) => {
                        const key = modelKey(model);
                        const local = localModels.has(key);
                        const platform = platformModels.has(key);
                        const systemInGroup = systemModelsByGroup.get(modelKey(group.id)) ?? new Set<string>();
                        const system = systemInGroup.has(key);
                        const previousIsSystem = index > 0 && systemInGroup.has(modelKey(group.models[index - 1]));
                        const nextIsSystem = index + 1 < group.models.length &&
                          systemInGroup.has(modelKey(group.models[index + 1]));
                        return (
                          <div className="compatibility-model-row" key={`${group.id}:${model}`}>
                            <span className="compatibility-model-order">{index + 1}</span>
                            <code>{model}</code>
                            <div className="compatibility-model-supply">
                              {local && <span className="local"><i className="supply-dot local" />{t("modelCompatibility.local")}</span>}
                              {platform && <span className="platform"><i className="supply-dot platform" />{t("modelCompatibility.platform")}</span>}
                              {!local && !platform && <span className="offline">{t("modelCompatibility.noSupply")}</span>}
                            </div>
                            <div className="compatibility-model-actions">
                              {system ? (
                                <span
                                  className="compatibility-model-fixed"
                                  aria-label={t("modelCompatibility.fixedAria")}
                                  title={t("modelCompatibility.fixedHint")}
                                >
                                  <Lock size={13} aria-hidden="true" />
                                  {t("modelCompatibility.fixed")}
                                </span>
                              ) : (
                                <>
                                  <button type="button" className="quiet" title={t("modelCompatibility.moveUp")} disabled={index === 0 || previousIsSystem} onClick={() => moveModel(group.id, index, -1)}>
                                    <ArrowUp size={14} />
                                  </button>
                                  <button type="button" className="quiet" title={t("modelCompatibility.moveDown")} disabled={index === group.models.length - 1 || nextIsSystem} onClick={() => moveModel(group.id, index, 1)}>
                                    <ArrowDown size={14} />
                                  </button>
                                  <button
                                    type="button"
                                    className="quiet danger"
                                    title={t("common.remove")}
                                    onClick={() => removeModel(group.id, model)}
                                  >
                                    <Trash2 size={14} />
                                  </button>
                                </>
                              )}
                            </div>
                          </div>
                        );
                      })}
                    </div>

                    <div className="compatibility-model-add">
                      <select
                        value={selected}
                        disabled={addable.length === 0}
                        onChange={(event) => setAddSelections((current) => ({
                          ...current,
                          [group.id]: event.target.value,
                        }))}
                      >
                        {addable.length === 0 ? (
                          <option value="">{t("modelCompatibility.noAddableModels")}</option>
                        ) : addable.map((model) => {
                          const key = modelKey(model.id);
                          const supply = localModels.has(key) && platformModels.has(key)
                            ? t("modelCompatibility.localAndPlatform")
                            : localModels.has(key) ? t("modelCompatibility.local") : platformModels.has(key) ? t("modelCompatibility.platform") : t("modelCompatibility.noSupply");
                          return <option value={model.id} key={model.id}>{model.id} · {supply}</option>;
                        })}
                      </select>
                      <button type="button" className="quiet" disabled={!selected} onClick={() => addModel(group.id, selected)}>
                        <Plus size={14} />{t("modelCompatibility.addModel")}
                      </button>
                    </div>
                    </div>
                  </article>
                );
              })}
            </div>

            <div className="compatibility-editor-footer">
              <div>
                {hasChanges ? t("modelCompatibility.unsaved") : t("modelCompatibility.saved")}
              </div>
              <div className="actions secondary-actions compact-actions">
                <button
                  type="button"
                  className="quiet"
                  disabled={saving}
                  onClick={() => void onReset()}
                >
                  <RotateCcw size={15} />{resetLabel}
                </button>
                <button type="button" disabled={saving || !hasChanges} onClick={() => void onSave()}>
                  {saving ? <RefreshCw className="spin" size={15} /> : <Save size={15} />}
                  {platformSyncAvailable ? t("modelCompatibility.saveAndSync") : t("modelCompatibility.saveLocal")}
                </button>
              </div>
            </div>
          </>
        )}
      </div>
    </DialogBackdrop>
  );
}
