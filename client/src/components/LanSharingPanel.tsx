import { invoke } from "@tauri-apps/api/core";
import {
  Check,
  ChevronDown,
  Copy,
  KeyRound,
  LoaderCircle,
  Pause,
  Play,
  Plus,
  RefreshCw,
  RotateCcw,
  Save,
  Square,
  Trash2,
  Undo2,
} from "lucide-react";
import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { ConfirmDialogState, LanShareHostStatus, LanShareMemberStatus } from "../appTypes";
import { DialogBackdrop } from "../DialogBackdrop";
import { ModalCloseButton } from "../ModalCloseButton";
import { useEscapeDismiss } from "../useEscapeDismiss";
import { InlineActionFeedback, type InlineActionFeedbackValue } from "../account/InlineActionFeedback";
import { CompactChoiceMenu } from "./AppControls";
import { lanShareConnectionHost, lanShareOpenAiBaseUrl } from "../lanShareConnectionInfo";
import { modelDisplayName, modelDisplayNames, type ModelPresentationPolicy } from "../modelPresentation";

type LanSharingPanelProps = {
  enabled: boolean;
  proxyRunning: boolean;
  disabled?: boolean;
  onToggle: (enabled: boolean) => Promise<void> | void;
  copyText: (value: string, message: string) => Promise<boolean>;
  askConfirm: (dialog: ConfirmDialogState) => Promise<boolean>;
  onClose?: () => void;
};

type MemberAction = "regenerate" | "delete" | null;

function errorText(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

function normalizedQuota(value: number) {
  if (!Number.isFinite(value)) return 1;
  return Math.max(1, Math.floor(value));
}

function formatTokens(value: number) {
  return new Intl.NumberFormat(undefined, { maximumFractionDigits: 0 }).format(value);
}

function modelSetsMatch(left: string[], right: string[]) {
  if (left.length !== right.length) return false;
  const rightKeys = new Set(right.map((model) => model.trim().toLocaleLowerCase()));
  return left.every((model) => rightKeys.has(model.trim().toLocaleLowerCase()));
}

function LanShareMemberEditor({
  member,
  connectUrl,
  sharedModels,
  busy,
  expanded,
  onToggle,
  onUpdate,
  onReset,
  onRegenerate,
  onDelete,
  copyText,
  askConfirm,
}: {
  member: LanShareMemberStatus;
  connectUrl: string;
  sharedModels: string[];
  busy: boolean;
  expanded: boolean;
  onToggle: () => void;
  onUpdate: (memberId: string, name: string, weeklyTokenLimit: number, enabled: boolean) => Promise<boolean>;
  onReset: (memberId: string) => Promise<boolean>;
  onRegenerate: (memberId: string) => Promise<boolean>;
  onDelete: (memberId: string) => Promise<boolean>;
  copyText: LanSharingPanelProps["copyText"];
  askConfirm: LanSharingPanelProps["askConfirm"];
}) {
  const { t } = useTranslation();
  const bodyId = `lan-share-member-${useId().replace(/:/g, "")}`;
  const [draft, setDraft] = useState<{ name: string; weeklyTokenLimit: number } | null>(null);
  const [confirming, setConfirming] = useState<MemberAction>(null);
  const [copied, setCopied] = useState(false);
  const [resetDone, setResetDone] = useState(false);
  const resetDoneTimerRef = useRef<number | null>(null);
  const name = draft?.name ?? member.name;
  const weeklyTokenLimit = draft?.weeklyTokenLimit ?? member.weekly_token_limit;
  const dirty = draft !== null
    && (name.trim() !== member.name || normalizedQuota(weeklyTokenLimit) !== member.weekly_token_limit);

  const memberBaseUrl = lanShareOpenAiBaseUrl(connectUrl);
  const memberHost = lanShareConnectionHost(memberBaseUrl);
  const generatedChannelName = memberHost
    ? t("lanShare.generatedChannelName", { host: memberHost })
    : t("lanShare.channelName");
  const inviteText = [
    t("lanShare.inviteHeading"),
    `${t("lanShare.channelNameField")}: ${generatedChannelName}`,
    `Base URL: ${memberBaseUrl}`,
    `API Key: ${member.api_key}`,
    `${t("lanShare.weeklyQuota")}: ${formatTokens(member.weekly_token_limit)}`,
    `${t("lanShare.sharedModels")}: ${sharedModels.join(", ") || t("lanShare.noModels")}`,
    "",
    `${t("lanShare.usageMethod")}:`,
    `1. ${t("lanShare.usageStepOpen")}`,
    `2. ${t("lanShare.usageStepPaste")}`,
    `3. ${t("lanShare.usageStepSave")}`,
  ].join("\n");

  useEffect(() => () => {
    if (resetDoneTimerRef.current !== null) window.clearTimeout(resetDoneTimerRef.current);
  }, []);

  async function copyInvite() {
    const didCopy = await copyText(inviteText, t("lanShare.inviteCopied"));
    if (!didCopy) return;
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1200);
  }

  async function saveMember() {
    if (await onUpdate(member.id, name.trim(), normalizedQuota(weeklyTokenLimit), member.enabled)) {
      setDraft(null);
    }
  }

  async function toggleMemberAccess() {
    await onUpdate(
      member.id,
      member.name,
      member.weekly_token_limit,
      !member.enabled,
    );
  }

  async function resetUsage() {
    const confirmed = await askConfirm({
      title: t("lanShare.resetUsageConfirmTitle"),
      message: t("lanShare.resetUsageConfirmMessage", { name: member.name }),
      confirmText: t("lanShare.confirmResetUsage"),
      tone: "danger",
    });
    if (!confirmed || !await onReset(member.id)) return;
    setResetDone(true);
    if (resetDoneTimerRef.current !== null) window.clearTimeout(resetDoneTimerRef.current);
    resetDoneTimerRef.current = window.setTimeout(() => {
      setResetDone(false);
      resetDoneTimerRef.current = null;
    }, 1600);
  }

  return (
    <article className={`lan-share-member${expanded ? " expanded" : ""}`}>
      <div className="lan-share-member-summary">
        <button
          type="button"
          className="lan-share-member-disclosure"
          aria-controls={bodyId}
          aria-expanded={expanded}
          onClick={() => {
            onToggle();
            setConfirming(null);
          }}
        >
          <span className="lan-share-member-identity">
            <strong>{member.name}</strong>
          </span>
          <span className="lan-share-member-status-cell">
            <span className={`lan-share-member-status${member.enabled ? " enabled" : " paused"}`}>
              {member.enabled ? t("lanShare.memberActive") : t("lanShare.memberPaused")}
            </span>
            {dirty ? <span className="lan-share-member-dirty">{t("lanShare.unsavedChanges")}</span> : null}
          </span>
          <span className="lan-share-member-quota-summary">
            <strong>{formatTokens(member.used_tokens)} / {formatTokens(member.weekly_token_limit)}</strong>
          </span>
          <span className="lan-share-member-expand-label">
            <span className="lan-share-member-expand-text">
              {expanded ? t("lanShare.collapseMember") : t("lanShare.expandMember")}
            </span>
            <ChevronDown className="lan-share-member-chevron" size={16} aria-hidden="true" />
          </span>
        </button>
      </div>

      {expanded ? (
        <div className="lan-share-member-body" id={bodyId}>
          <div className="lan-share-usage-details">
            <small>
              {member.exhausted
                ? t("lanShare.quotaExhausted")
                : t("lanShare.remaining", { count: formatTokens(member.remaining_tokens) })}
              {" · "}{t("lanShare.requests", { count: member.requests })}
              {" · "}{t("lanShare.inputTokens", { count: formatTokens(member.input_tokens) })}
              {" · "}{t("lanShare.outputTokens", { count: formatTokens(member.output_tokens) })}
              {" · "}{t("lanShare.resetsAt", { time: new Date(member.resets_at_unix * 1000).toLocaleString() })}
            </small>
          </div>

          <div className="lan-share-member-fields">
            <label>
              {t("lanShare.remark")}
              <input
                value={name}
                maxLength={80}
                onChange={(event) => setDraft({ name: event.target.value, weeklyTokenLimit })}
              />
            </label>
            <label>
              {t("lanShare.weeklyQuota")}
              <input
                type="number"
                min={1}
                step={1000}
                value={weeklyTokenLimit}
                onChange={(event) => setDraft({ name, weeklyTokenLimit: Number(event.target.value) })}
              />
            </label>
          </div>

          <div className="lan-share-member-key">
            <KeyRound size={15} />
            <code title={member.api_key}>{member.api_key}</code>
          </div>

          <div className="lan-share-member-actions">
            {dirty ? (
              <>
                <button type="button" className="primary compact" onClick={() => void saveMember()} disabled={busy || !name.trim()}>
                  <Save size={14} />{t("lanShare.saveMember")}
                </button>
                <button type="button" className="quiet compact" onClick={() => setDraft(null)} disabled={busy}>
                  <Undo2 size={14} />{t("common.cancel")}
                </button>
              </>
            ) : null}
            <button
              type="button"
              className={member.enabled ? "stop compact" : "start compact"}
              onClick={() => void toggleMemberAccess()}
              disabled={busy}
            >
              {member.enabled ? <Pause size={14} /> : <Play size={14} />}
              {member.enabled ? t("lanShare.pauseMember") : t("lanShare.resumeMember")}
            </button>
            <button type="button" className="quiet compact" onClick={() => void resetUsage()} disabled={busy}>
              {resetDone ? <Check size={14} /> : <RotateCcw size={14} />}
              {resetDone ? t("lanShare.usageReset") : t("lanShare.resetUsage")}
            </button>
            {confirming === "regenerate" ? (
              <>
                <button type="button" className="quiet compact" onClick={() => setConfirming(null)} disabled={busy}>{t("common.cancel")}</button>
                <button type="button" className="stop compact" onClick={() => void onRegenerate(member.id).then((updated) => updated && setConfirming(null))} disabled={busy}>
                  {t("lanShare.confirmRegenerate")}
                </button>
              </>
            ) : (
              <button type="button" className="quiet compact" onClick={() => setConfirming("regenerate")} disabled={busy}>
                <KeyRound size={14} />{t("lanShare.regenerateKey")}
              </button>
            )}
            {confirming === "delete" ? (
              <>
                <button type="button" className="quiet compact" onClick={() => setConfirming(null)} disabled={busy}>{t("common.cancel")}</button>
                <button type="button" className="stop compact" onClick={() => void onDelete(member.id)} disabled={busy}>
                  {t("lanShare.confirmDelete")}
                </button>
              </>
            ) : (
              <button type="button" className="quiet compact" onClick={() => setConfirming("delete")} disabled={busy}>
                <Trash2 size={14} />{t("common.delete")}
              </button>
            )}
            <button
              type="button"
              className="quiet compact lan-share-copy-invite"
              onClick={() => void copyInvite()}
              disabled={busy || !connectUrl}
            >
              {copied ? <Check size={15} /> : <Copy size={15} />}
              {copied ? t("common.copied") : t("lanShare.copyInvite")}
            </button>
          </div>
        </div>
      ) : null}
    </article>
  );
}

export function LanSharingPanel({
  enabled,
  proxyRunning,
  disabled = false,
  onToggle,
  copyText,
  askConfirm,
  onClose,
}: LanSharingPanelProps) {
  const { t } = useTranslation();
  const [status, setStatus] = useState<LanShareHostStatus | null>(null);
  const [presentation, setPresentation] = useState<ModelPresentationPolicy | null>(null);
  const [loading, setLoading] = useState(true);
  const [actionBusy, setActionBusy] = useState(false);
  const [error, setError] = useState("");
  const [memberName, setMemberName] = useState("");
  const [memberFeedback, setMemberFeedback] = useState<InlineActionFeedbackValue | null>(null);
  const [memberQuota, setMemberQuota] = useState(1_000_000);
  const [modelDraft, setModelDraft] = useState<string[] | null>(null);
  const [addressDraft, setAddressDraft] = useState<string | null>(null);
  const [expandedMemberId, setExpandedMemberId] = useState<string | null>(null);
  const memberFormId = useId().replace(/:/g, "");
  const memberQuotaHintId = `lan-share-member-quota-hint-${memberFormId}`;
  const memberFeedbackId = `lan-share-member-feedback-${memberFormId}`;
  const refreshInFlight = useRef(false);
  const presentationInFlight = useRef(false);

  const refreshPresentation = useCallback(async () => {
    if (!proxyRunning || presentationInFlight.current) return;
    presentationInFlight.current = true;
    try {
      setPresentation(await invoke<ModelPresentationPolicy>("get_model_presentation"));
    } catch (reason) {
      // Sorting is optional: keep the last rules (or alphabetical order) offline.
      console.warn("[const-api] Model display rules unavailable; retaining current order", reason);
    } finally {
      presentationInFlight.current = false;
    }
  }, [proxyRunning]);

  const refresh = useCallback(async (silent = false) => {
    if (refreshInFlight.current) return;
    refreshInFlight.current = true;
    // Usage polling must not keep fetching the model directory.
    if (!silent) void refreshPresentation();
    if (!silent) setLoading(true);
    try {
      const next = await invoke<LanShareHostStatus>("get_lan_share_status");
      setStatus(next);
      setError("");
    } catch (nextError) {
      if (!silent) setError(errorText(nextError));
    } finally {
      refreshInFlight.current = false;
      if (!silent) setLoading(false);
    }
  }, [refreshPresentation]);

  useEffect(() => {
    void refresh();
    if (!enabled) return undefined;
    const timer = window.setInterval(() => void refresh(true), 5000);
    return () => window.clearInterval(timer);
  }, [enabled, refresh]);

  async function runAction(
    action: () => Promise<LanShareHostStatus>,
    onSuccess?: (next: LanShareHostStatus) => void,
  ) {
    if (actionBusy) return false;
    setActionBusy(true);
    try {
      const next = await action();
      setStatus(next);
      onSuccess?.(next);
      setError("");
      return true;
    } catch (nextError) {
      setError(errorText(nextError));
      return false;
    } finally {
      setActionBusy(false);
    }
  }

  const persistedModels = status?.shared_models ?? [];
  const selectedModelList = modelDraft ?? persistedModels;
  const modelDirty = modelDraft !== null && !modelSetsMatch(modelDraft, persistedModels);
  const selectedModels = useMemo(
    () => new Set(selectedModelList.map((model) => model.toLocaleLowerCase())),
    [selectedModelList],
  );
  const sharedModelLabels = useMemo(
    () => [...modelDisplayNames(status?.shared_models ?? [], presentation).values()],
    [status?.shared_models, presentation],
  );
  const modelOptions = useMemo(() => {
    const available = status?.available_models ?? [];
    const shared = new Set((status?.shared_models ?? []).map((model) => model.toLocaleLowerCase()));
    // Keep an explicitly shared wire ID when short names collide. Display
    // normalization must not switch the grant to another vendor's model.
    return modelDisplayNames([
      ...available.filter((model) => shared.has(model.toLocaleLowerCase())),
      ...available,
    ], presentation);
  }, [status?.available_models, status?.shared_models, presentation]);
  const selectedModelCount = useMemo(
    () => new Set(selectedModelList.map(modelDisplayName)).size,
    [selectedModelList],
  );
  const connectAddresses = status?.connect_addresses ?? [];
  const persistedConnectUrl = status?.connect_url || connectAddresses[0]?.url || "";
  const draftAddressAvailable = addressDraft !== null
    && connectAddresses.some((address) => address.url === addressDraft);
  const validAddressDraft = draftAddressAvailable ? addressDraft : null;
  const connectAddress = validAddressDraft ?? persistedConnectUrl;
  const addressDirty = validAddressDraft !== null && validAddressDraft !== persistedConnectUrl;
  const addressOptions = connectAddresses.map((address) => ({
    value: address.url,
    label: `${address.interface_name ? `${address.interface_name} · ` : ""}${address.url}${address.is_primary ? ` · ${t("lanShare.primaryAddress")}` : ""}`,
  }));
  const activeMemberCount = status?.members.filter((member) => member.enabled).length ?? 0;
  const totalWeeklyUsage = status?.members.reduce((total, member) => total + member.used_tokens, 0) ?? 0;
  const effectiveBusy = disabled || actionBusy;

  function toggleModel(model: string) {
    setModelDraft((current) => {
      const base = current ?? status?.shared_models ?? [];
      const key = model.toLocaleLowerCase();
      return base.some((item) => item.toLocaleLowerCase() === key)
        ? base.filter((item) => modelDisplayName(item) !== modelDisplayName(model))
        : [...base, model];
    });
  }

  async function saveModels() {
    if (!modelDirty || modelDraft === null) return;
    const saved = await runAction(() => invoke<LanShareHostStatus>("set_lan_share_models", {
      models: modelDraft,
    }));
    if (saved) setModelDraft(null);
  }

  async function saveConnectAddress() {
    if (!addressDirty || addressDraft === null) return;
    const saved = await runAction(() => invoke<LanShareHostStatus>("set_lan_share_connect_address", {
      connectUrl: addressDraft,
    }));
    if (saved) setAddressDraft(null);
  }

  async function createMember() {
    const name = memberName.trim();
    if (!name) {
      setMemberFeedback({ tone: "error", text: t("lanShare.memberNameRequired") });
      return;
    }
    setMemberFeedback(null);
    const existingMemberIds = new Set(status?.members.map((member) => member.id) ?? []);
    const created = await runAction(() => invoke<LanShareHostStatus>("create_lan_share_member", {
      name,
      weeklyTokenLimit: normalizedQuota(memberQuota),
    }), (next) => {
      const newMember = next.members.find((member) => !existingMemberIds.has(member.id));
      if (newMember) setExpandedMemberId(newMember.id);
    });
    if (created) setMemberName("");
  }

  return (
    <div className="lan-share-drawer-content">
      <div className="drawer-header">
        <div>
          <h2 id="lan-share-title">{t("lanShare.title")}</h2>
          <p id="lan-share-description">{t("lanShare.description")}</p>
        </div>
        {onClose ? <ModalCloseButton onClick={onClose} /> : null}
      </div>

      {status ? (
        <div className="drawer-summary lan-share-summary">
          <div>
            <span>{t("lanShare.shareStatus")}</span>
            <strong className={enabled ? "good" : "muted"}>{enabled ? t("lanShare.enabled") : t("lanShare.disabled")}</strong>
            <small>{proxyRunning ? t("lanShare.proxyReady") : t("lanShare.proxyNotRunning")}</small>
          </div>
          <div>
            <span>{t("lanShare.networkAdapters")}</span>
            <strong>{connectAddresses.length || "-"}</strong>
            <small>{connectAddresses.find((address) => address.is_primary)?.ip || "-"}</small>
          </div>
          <div>
            <span>{t("lanShare.sharedModels")}</span>
            <strong>{sharedModelLabels.length}</strong>
            <small>{t("lanShare.availableModelCount", { count: modelOptions.size })}</small>
          </div>
          <div>
            <span>{t("lanShare.members")}</span>
            <strong>{status.members.length}</strong>
            <small>{t("lanShare.activeMemberCount", { count: activeMemberCount })}</small>
          </div>
          <div>
            <span>{t("lanShare.weekUsage")}</span>
            <strong>{formatTokens(totalWeeklyUsage)}</strong>
            <small>{t("lanShare.weightedTokens")}</small>
          </div>
        </div>
      ) : null}

      {error ? <div className="lan-share-error">{error}</div> : null}
      {loading && !status ? (
        <div className="lan-share-loading"><LoaderCircle className="spin" size={18} />{t("common.loading")}</div>
      ) : null}

      {status ? (
        <>
          <div className="drawer-section lan-share-drawer-section">
            <div className="section-title-row compact">
              <div>
                <h2>{t("lanShare.connectionAndAccess")}</h2>
              </div>
              <div className="actions channel-actions">
                <button type="button" className="quiet" onClick={() => void refresh()} disabled={effectiveBusy}>
                  <RefreshCw size={15} />{t("common.refresh")}
                </button>
                <button
                  type="button"
                  className={enabled ? "stop" : "start"}
                  onClick={() => void onToggle(!enabled)}
                  disabled={effectiveBusy}
                >
                  {enabled ? <Square size={15} /> : <Play size={15} />}
                  {enabled ? t("lanShare.stopSharing") : t("lanShare.startSharing")}
                </button>
              </div>
            </div>
            {!enabled ? <div className="lan-share-disabled-notice">{t("lanShare.disabledHint")}</div> : null}
            {enabled && !proxyRunning ? <div className="lan-share-warning">{t("lanShare.proxyStopped")}</div> : null}
            <div className="lan-share-address-field">
              <label>{t("lanShare.teamAddress")}</label>
              <div className="lan-share-address-control-row">
                {addressOptions.length > 1 ? (
                  <CompactChoiceMenu
                    value={connectAddress}
                    options={addressOptions}
                    ariaLabel={t("lanShare.selectAddress")}
                    className="lan-share-address-choice"
                    disabled={effectiveBusy}
                    floating
                    onChange={(value) => setAddressDraft(value === persistedConnectUrl ? null : value)}
                  />
                ) : (
                  <div className="lan-share-address-value">
                    <code>{connectAddress || t("lanShare.addressUnavailable")}</code>
                  </div>
                )}
                {addressDirty ? (
                  <div className="actions lan-share-address-actions">
                    <button type="button" className="primary compact" onClick={() => void saveConnectAddress()} disabled={effectiveBusy}>
                      <Save size={14} />{t("lanShare.saveAddress")}
                    </button>
                    <button type="button" className="quiet compact" onClick={() => setAddressDraft(null)} disabled={effectiveBusy}>
                      <Undo2 size={14} />{t("common.cancel")}
                    </button>
                  </div>
                ) : null}
              </div>
              <small className="field-hint">{t("lanShare.selectAddressHint")}</small>
            </div>
            <small className="lan-share-weight-hint">{t("lanShare.networkNotice")}</small>
          </div>

          <div className="drawer-section lan-share-drawer-section">
            <div className="section-title-row compact">
              <div>
                <h2>{t("lanShare.selectModels")}</h2>
                <p className="section-hint">
                  {t("lanShare.selectedModelCount", { selected: selectedModelCount, total: modelOptions.size })}
                </p>
              </div>
              <div className="actions channel-actions lan-share-model-actions">
                {modelDirty ? (
                  <>
                    <button type="button" className="primary" onClick={() => void saveModels()} disabled={effectiveBusy}>
                      <Save size={15} />{t("lanShare.saveModels")}
                    </button>
                    <button type="button" className="quiet" onClick={() => setModelDraft(null)} disabled={effectiveBusy}>
                      <Undo2 size={15} />{t("common.cancel")}
                    </button>
                  </>
                ) : null}
                <button type="button" className="quiet" onClick={() => setModelDraft([...modelOptions.keys()])} disabled={effectiveBusy}>{t("lanShare.selectAll")}</button>
                <button type="button" className="quiet" onClick={() => setModelDraft([])} disabled={effectiveBusy}>{t("lanShare.clear")}</button>
              </div>
            </div>
            <p className="lan-share-section-description">{t("lanShare.selectModelsHint")}</p>
            <div className="lan-share-models">
              {modelOptions.size > 0 ? [...modelOptions].map(([model, label]) => (
                <label key={model} className={selectedModels.has(model.toLocaleLowerCase()) ? "selected" : ""}>
                  <input
                    type="checkbox"
                    checked={selectedModels.has(model.toLocaleLowerCase())}
                    onChange={() => toggleModel(model)}
                    disabled={effectiveBusy}
                  />
                  <span>{label}</span>
                </label>
              )) : <p className="lan-share-empty">{t("lanShare.noAvailableModels")}</p>}
            </div>
          </div>

          <div className="drawer-section lan-share-drawer-section">
            <div className="section-title-row compact">
              <div>
                <h2>{t("lanShare.members")}</h2>
                <p className="section-hint">{t("lanShare.membersHint")}</p>
              </div>
            </div>
            <form
              className="lan-share-add-member"
              noValidate
              onSubmit={(event) => {
                event.preventDefault();
                void createMember();
              }}
            >
              <label className="lan-share-add-member-name">
                {t("lanShare.remark")}
                <input
                  value={memberName}
                  maxLength={80}
                  required
                  aria-invalid={memberFeedback?.tone === "error" || undefined}
                  aria-describedby={memberFeedback ? memberFeedbackId : undefined}
                  placeholder={t("lanShare.remarkPlaceholder")}
                  onChange={(event) => {
                    setMemberName(event.target.value);
                    setMemberFeedback(null);
                  }}
                />
              </label>
              <label className="lan-share-add-member-quota">
                {t("lanShare.weeklyQuota")}
                <input
                  type="number"
                  min={1}
                  step={1000}
                  value={memberQuota}
                  aria-describedby={memberQuotaHintId}
                  onChange={(event) => setMemberQuota(Number(event.target.value))}
                />
              </label>
              <button type="submit" className="primary" disabled={effectiveBusy}>
                <Plus size={15} />{t("lanShare.addMember")}
              </button>
              <small id={memberQuotaHintId} className="lan-share-add-member-weight-hint">
                {t("lanShare.weightHint", { input: status.input_weight, output: status.output_weight })}
              </small>
              <InlineActionFeedback id={memberFeedbackId} feedback={memberFeedback} />
            </form>
            {status.members.length > 0 ? (
              <div className="lan-share-member-list">
                <div className="lan-share-member-list-header" aria-hidden="true">
                  <span>{t("lanShare.memberListName")}</span>
                  <span>{t("lanShare.memberListStatus")}</span>
                  <span>{t("lanShare.memberListWeekUsage")}</span>
                  <span />
                </div>
                {status.members.map((member) => (
                  <LanShareMemberEditor
                    key={member.id}
                    member={member}
                    connectUrl={connectAddress}
                    sharedModels={sharedModelLabels}
                    busy={effectiveBusy}
                    expanded={expandedMemberId === member.id}
                    onToggle={() => setExpandedMemberId((current) => current === member.id ? null : member.id)}
                    copyText={copyText}
                    askConfirm={askConfirm}
                    onUpdate={(memberId, name, weeklyTokenLimit, memberEnabled) => runAction(() => invoke<LanShareHostStatus>("update_lan_share_member", {
                      memberId,
                      name,
                      weeklyTokenLimit,
                      enabled: memberEnabled,
                    }))}
                    onReset={(memberId) => runAction(() => invoke<LanShareHostStatus>("reset_lan_share_member", { memberId }))}
                    onRegenerate={(memberId) => runAction(() => invoke<LanShareHostStatus>("regenerate_lan_share_member_key", { memberId }))}
                    onDelete={(memberId) => runAction(() => invoke<LanShareHostStatus>("delete_lan_share_member", { memberId }))}
                  />
                ))}
              </div>
            ) : <p className="lan-share-empty">{t("lanShare.noMembers")}</p>}
          </div>
        </>
      ) : null}
    </div>
  );
}

export function LanSharingDrawer({
  onClose,
  ...panelProps
}: LanSharingPanelProps & { onClose: () => void }) {
  useEscapeDismiss(onClose);

  return (
    <DialogBackdrop
      aria-labelledby="lan-share-title"
      aria-describedby="lan-share-description"
      variant="drawer"
    >
      <div className="detail-drawer lan-share-drawer">
        <LanSharingPanel {...panelProps} onClose={onClose} />
      </div>
    </DialogBackdrop>
  );
}
