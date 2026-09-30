import React, { useEffect, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Check,
  ChevronDown,
  ChevronRight,
  Copy,
  Plus,
  RotateCcw,
  type LucideIcon,
} from "lucide-react";
import azureIconUrl from "../assets/tool-icons/azure.svg";
import anythingllmIconUrl from "../assets/tool-icons/anythingllm.svg";
import bedrockIconUrl from "../assets/tool-icons/bedrock.svg";
import claudeIconUrl from "../assets/tool-icons/claude.svg";
import claudeScienceIconUrl from "../assets/tool-icons/claude-science.png";
import codexIconUrl from "../assets/tool-icons/codex.svg";
import copilotIconUrl from "../assets/tool-icons/copilot.svg";
import clineIconUrl from "../assets/tool-icons/cline.svg";
import traeIconUrl from "../assets/tool-icons/trae.svg";
import traeWorkIconUrl from "../assets/tool-icons/trae-work.svg";
import deepseekHarnessIconUrl from "../assets/tool-icons/deepseek-harness.svg";
import geminiIconUrl from "../assets/tool-icons/gemini.svg";
import gooseIconUrl from "../assets/tool-icons/goose.svg";
import grokIconUrl from "../assets/tool-icons/grok.svg";
import hermesIconUrl from "../assets/tool-icons/hermesagent.png";
// Desktop foreground marks; source/provenance lives in each SVG.
import kimiIconUrl from "../assets/tool-icons/kimi-code.svg";
import mimoCodeIconUrl from "../assets/tool-icons/mimocode.svg";
import miniMaxCodeIconUrl from "../assets/tool-icons/minimax-code.svg";
import omniRouteIconUrl from "../assets/tool-icons/omniroute.svg";
import openDesignIconUrl from "../assets/tool-icons/open-design.svg";
import openInterpreterIconUrl from "../assets/tool-icons/open-interpreter.svg";
import openClawIconUrl from "../assets/tool-icons/openclaw.svg";
import openCodeIconUrl from "../assets/tool-icons/opencode.svg";
// Application mark from ai4s-research/open-science/apps/desktop/src/assets/logo.webp;
// its favicon is a different illustration.
import openScienceIconUrl from "../assets/tool-icons/open-science.webp";
import qwenIconUrl from "../assets/tool-icons/qwen.svg";
import piIconUrl from "../assets/tool-icons/pi.svg";
import ravenIconUrl from "../assets/tool-icons/raven.svg";
import reasonixIconUrl from "../assets/tool-icons/reasonix.svg";
import mistralVibeIconUrl from "../assets/tool-icons/mistral-vibe.svg";
import vibeTradingIconUrl from "../assets/tool-icons/vibe-trading.png";
import vscodeIconUrl from "../assets/tool-icons/vscode.svg";
import workBuddyIconUrl from "../assets/tool-icons/workbuddy.svg";
import zcodeIconUrl from "../assets/tool-icons/zcode.svg";
import { useToolDockMenu } from "../ToolDockMenu";
import type { ToolCatalogId } from "../toolCatalog";
import {
  channelCapabilityFeatures,
  channelClientHostFeatures,
} from "../channelCapabilityPresentation";
import {
  TOOL_PROTOCOL_OPTIONS,
  toolConfigDetailState,
  toolProtocolMenuState,
  type CodexModelSource,
  type ToolConfigTone,
  type ToolProtocolId,
} from "../toolMenuPresentation";
import {
  CLAUDE_MODEL_AUTO,
  CLAUDE_MODEL_FOLLOW_MAIN,
  type ClaudeModelSettings,
} from "../claudeModelSettings";
import { DialogBackdrop } from "../DialogBackdrop";
import { useEscapeDismiss } from "../useEscapeDismiss";
import { modelDisplayName, modelInputOptions } from "../modelPresentation";
import type {
  BrandIconData,
  ChannelCapabilityProfile,
  ChannelDuplicateDecision,
  ChannelDuplicateDialogState,
  ConfirmDialogState,
  CustomBrandIcon,
  ToolRemoveDecision,
  ToolRemoveDialogState,
  ToolRemoveMode,
} from "../appTypes";

const TOOL_MENU_EDGE_INSET = 12;
const TOOL_MENU_PRIMARY_WIDTH = 172;
const TOOL_MENU_EXPANDED_WIDTH = 300;
const HIDDEN_TOOL_MENU_WIDTH = 232;
const HIDDEN_TOOL_MENU_GAP = 6;
const HIDDEN_TOOL_MENU_MAX_HEIGHT = 460;

type AnchoredMenuPlacement = {
  left: number;
  width: number;
};

type HiddenToolMenuPlacement = AnchoredMenuPlacement & {
  top: number | null;
  bottom: number | null;
  maxHeight: number;
};

function resolveAnchoredMenuPlacement(
  dock: HTMLElement | null,
  anchor: HTMLElement | null,
  preferredWidth: number,
): AnchoredMenuPlacement | null {
  if (!dock || !anchor) return null;

  const dockRect = dock.getBoundingClientRect();
  const anchorRect = anchor.getBoundingClientRect();
  const viewportWidth = window.innerWidth || document.documentElement.clientWidth || 1280;
  const mainRect = dock.closest("main")?.getBoundingClientRect();
  const hasMainBounds = Boolean(mainRect && mainRect.width > TOOL_MENU_EDGE_INSET * 2);
  let visibleLeft = TOOL_MENU_EDGE_INSET;
  let visibleRight = viewportWidth - TOOL_MENU_EDGE_INSET;

  if (hasMainBounds && mainRect) {
    visibleLeft = Math.max(visibleLeft, mainRect.left + TOOL_MENU_EDGE_INSET);
    visibleRight = Math.min(visibleRight, mainRect.right - TOOL_MENU_EDGE_INSET);
  }
  if (visibleRight <= visibleLeft) {
    visibleLeft = TOOL_MENU_EDGE_INSET;
    visibleRight = viewportWidth - TOOL_MENU_EDGE_INSET;
  }

  const availableWidth = Math.max(1, visibleRight - visibleLeft);
  const width = Math.min(preferredWidth, availableWidth);
  const globalLeft = Math.min(
    Math.max(anchorRect.left, visibleLeft),
    visibleRight - width,
  );

  return {
    left: globalLeft - dockRect.left,
    width,
  };
}

function anchoredMenuStyle(placement: AnchoredMenuPlacement): React.CSSProperties {
  return {
    left: `${placement.left}px`,
    right: "auto",
    width: `${placement.width}px`,
    minWidth: `${placement.width}px`,
    maxWidth: `${placement.width}px`,
  };
}

function resolveHiddenToolMenuPlacement(
  dock: HTMLElement | null,
  anchor: HTMLElement | null,
  cardCount: number,
): HiddenToolMenuPlacement | null {
  const horizontal = resolveAnchoredMenuPlacement(dock, anchor, HIDDEN_TOOL_MENU_WIDTH);
  if (!horizontal || !dock || !anchor) return null;

  const dockRect = dock.getBoundingClientRect();
  const anchorRect = anchor.getBoundingClientRect();
  const viewportHeight = window.innerHeight || document.documentElement.clientHeight || 720;
  const availableBelow = Math.max(
    0,
    viewportHeight - TOOL_MENU_EDGE_INSET - anchorRect.bottom - HIDDEN_TOOL_MENU_GAP,
  );
  const availableAbove = Math.max(
    0,
    anchorRect.top - TOOL_MENU_EDGE_INSET - HIDDEN_TOOL_MENU_GAP,
  );
  const desiredHeight = Math.min(
    HIDDEN_TOOL_MENU_MAX_HEIGHT,
    46 + (cardCount > 0 ? 30 + cardCount * 38 + 8 : 0),
  );
  const opensAbove = availableBelow < desiredHeight && availableAbove > availableBelow;
  const availableHeight = opensAbove ? availableAbove : availableBelow;

  return {
    ...horizontal,
    top: opensAbove
      ? null
      : anchorRect.bottom - dockRect.top + HIDDEN_TOOL_MENU_GAP,
    bottom: opensAbove
      ? dockRect.bottom - anchorRect.top + HIDDEN_TOOL_MENU_GAP
      : null,
    maxHeight: Math.max(1, Math.min(desiredHeight, availableHeight)),
  };
}

function hiddenToolMenuStyle(placement: HiddenToolMenuPlacement): React.CSSProperties {
  return {
    ...anchoredMenuStyle(placement),
    top: placement.top === null ? "auto" : `${placement.top}px`,
    bottom: placement.bottom === null ? "auto" : `${placement.bottom}px`,
    maxHeight: `${placement.maxHeight}px`,
  };
}

export function ModelInput({ value, models, onChange }: { value: string; models: string[]; onChange: (value: string) => void }) {
  const options = modelInputOptions(models, value);
  const selected = options.find((option) => option.value === value)
    ?? options.find((option) => option.label.toLowerCase() === modelDisplayName(value).toLowerCase());
  const selectedValue = selected?.value ?? value;
  useEffect(() => {
    if (models.length > 0 && selectedValue !== value) onChange(selectedValue);
  }, [models.length, onChange, selectedValue, value]);
  if (models.length > 0) {
    return (
      <select
        className="model-input-select"
        value={selectedValue}
        title={selectedValue}
        onChange={(event) => onChange(event.target.value)}
      >
        {options.map((option) => (
          <option key={option.value} value={option.value} title={option.value}>{option.label}</option>
        ))}
      </select>
    );
  }
  return <input value={value} onChange={(event) => onChange(event.target.value)} />;
}

export type CompactChoiceOption<T extends string> = {
  value: T;
  label: string;
  disabled?: boolean;
};

type CompactChoiceFloatingPlacement = {
  left: number;
  top: number;
  width: number;
  maxHeight: number;
};

function resolveCompactChoiceFloatingPlacement(
  trigger: HTMLButtonElement | null,
  optionCount: number,
): CompactChoiceFloatingPlacement | null {
  if (!trigger) return null;
  const rect = trigger.getBoundingClientRect();
  const edge = 8;
  const gap = 4;
  const viewportWidth = window.innerWidth;
  const viewportHeight = window.innerHeight;
  const width = Math.min(Math.max(rect.width, 160), Math.max(160, viewportWidth - edge * 2));
  const left = Math.max(edge, Math.min(rect.left, viewportWidth - edge - width));
  const desiredHeight = Math.min(360, Math.max(36, optionCount * 28 + 8));
  const spaceBelow = Math.max(0, viewportHeight - rect.bottom - gap - edge);
  const spaceAbove = Math.max(0, rect.top - gap - edge);
  const placeBelow = spaceBelow >= desiredHeight || spaceBelow >= spaceAbove;
  const availableHeight = placeBelow ? spaceBelow : spaceAbove;
  const maxHeight = Math.max(36, Math.min(desiredHeight, availableHeight));
  const top = placeBelow
    ? Math.min(rect.bottom + gap, viewportHeight - edge - maxHeight)
    : Math.max(edge, rect.top - gap - maxHeight);
  return { left, top, width, maxHeight };
}

export function CompactChoiceMenu<T extends string>({
  value,
  options,
  ariaLabel,
  disabled = false,
  floating = false,
  title,
  className,
  onChange,
}: {
  value: T;
  options: readonly CompactChoiceOption<T>[];
  ariaLabel: string;
  disabled?: boolean;
  floating?: boolean;
  title?: string;
  className?: string;
  onChange: (value: T) => void;
}) {
  const [open, setOpen] = useState(false);
  const [floatingPlacement, setFloatingPlacement] = useState<CompactChoiceFloatingPlacement | null>(null);
  const rootRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const optionRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const listboxId = `compact-choice-${useId().replace(/:/g, "")}`;
  const selectedOption = options.find((option) => option.value === value) ?? options[0];

  useEffect(() => {
    if (!open) return;
    const closeFromOutside = (event: PointerEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", closeFromOutside, true);
    return () => document.removeEventListener("pointerdown", closeFromOutside, true);
  }, [open]);

  useEffect(() => {
    if (!open || !floating) return;
    const updatePlacement = () => {
      setFloatingPlacement(resolveCompactChoiceFloatingPlacement(triggerRef.current, options.length));
    };
    updatePlacement();
    window.addEventListener("resize", updatePlacement);
    window.addEventListener("scroll", updatePlacement, true);
    return () => {
      window.removeEventListener("resize", updatePlacement);
      window.removeEventListener("scroll", updatePlacement, true);
    };
  }, [floating, open, options.length]);

  const afterRender = (callback: () => void) => {
    if (typeof window.requestAnimationFrame === "function") {
      window.requestAnimationFrame(callback);
    } else {
      queueMicrotask(callback);
    }
  };
  const focusOption = (index: number) => {
    afterRender(() => optionRefs.current[index]?.focus());
  };
  const openMenu = (
    preferredIndex = options.findIndex((option) => option.value === value),
    focusSelected = true,
  ) => {
    if (disabled) return;
    if (floating) {
      setFloatingPlacement(resolveCompactChoiceFloatingPlacement(triggerRef.current, options.length));
    }
    setOpen(true);
    if (!focusSelected) return;
    const enabledIndex = options[preferredIndex]?.disabled
      ? options.findIndex((option) => !option.disabled)
      : preferredIndex;
    focusOption(enabledIndex >= 0 ? enabledIndex : 0);
  };
  const closeMenu = (restoreFocus = false) => {
    setOpen(false);
    if (restoreFocus) afterRender(() => triggerRef.current?.focus());
  };
  const moveOptionFocus = (currentIndex: number, direction: 1 | -1) => {
    const enabledIndices = options
      .map((option, index) => option.disabled ? -1 : index)
      .filter((index) => index >= 0);
    const currentPosition = enabledIndices.indexOf(currentIndex);
    if (currentPosition < 0 || enabledIndices.length === 0) return;
    const nextPosition = (currentPosition + direction + enabledIndices.length) % enabledIndices.length;
    optionRefs.current[enabledIndices[nextPosition]]?.focus();
  };
  const selectOption = (option: CompactChoiceOption<T>) => {
    if (option.disabled) return;
    onChange(option.value);
    closeMenu(true);
  };

  return (
    <div
      ref={rootRef}
      className={`compact-choice ${open ? "is-open" : ""} ${className ?? ""}`.trim()}
    >
      <button
        ref={triggerRef}
        type="button"
        className="compact-choice-trigger"
        aria-label={ariaLabel}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={open ? listboxId : undefined}
        disabled={disabled}
        title={title ?? selectedOption?.label ?? value}
        onClick={() => open ? closeMenu() : openMenu(undefined, false)}
        onKeyDown={(event) => {
          if (event.key === "ArrowDown" || event.key === "ArrowUp") {
            event.preventDefault();
            openMenu(event.key === "ArrowDown" ? 0 : options.length - 1);
          }
          if (event.key === "Escape" && open) {
            event.preventDefault();
            closeMenu(true);
          }
        }}
      >
        <span>{selectedOption?.label ?? value}</span>
        <ChevronDown size={12} aria-hidden="true" />
      </button>
      {open && (
        <div
          id={listboxId}
          className={`compact-choice-list ${floating ? "is-floating" : ""}`.trim()}
          role="listbox"
          aria-label={ariaLabel}
          style={floating && floatingPlacement ? {
            position: "fixed",
            left: floatingPlacement.left,
            top: floatingPlacement.top,
            bottom: "auto",
            width: floatingPlacement.width,
            minWidth: floatingPlacement.width,
            maxWidth: floatingPlacement.width,
            maxHeight: floatingPlacement.maxHeight,
            overflowY: "auto",
          } : undefined}
        >
          {options.map((option, index) => (
            <button
              key={option.value}
              ref={(element) => { optionRefs.current[index] = element; }}
              type="button"
              className="compact-choice-option"
              role="option"
              aria-selected={option.value === value}
              title={option.label}
              disabled={option.disabled}
              onClick={() => selectOption(option)}
              onKeyDown={(event) => {
                if (event.key === "ArrowDown" || event.key === "ArrowUp") {
                  event.preventDefault();
                  moveOptionFocus(index, event.key === "ArrowDown" ? 1 : -1);
                } else if (event.key === "Home" || event.key === "End") {
                  event.preventDefault();
                  const targetIndex = event.key === "Home"
                    ? options.findIndex((item) => !item.disabled)
                    : options.reduce(
                        (lastIndex, item, itemIndex) => item.disabled ? lastIndex : itemIndex,
                        -1,
                      );
                  if (targetIndex >= 0) optionRefs.current[targetIndex]?.focus();
                } else if (event.key === "Escape") {
                  event.preventDefault();
                  closeMenu(true);
                }
              }}
            >
              <span>{option.label}</span>
              {option.value === value && <Check size={13} aria-hidden="true" />}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

export function CapabilityProfileList({ profiles }: { profiles: ChannelCapabilityProfile[] }) {
  const { t } = useTranslation();
  const visibleFeatures = channelCapabilityFeatures(profiles);
  const clientHostFeatures = channelClientHostFeatures(profiles);

  if (visibleFeatures.length === 0 && clientHostFeatures.length === 0) return null;

  return (
    <>
      {visibleFeatures.length > 0 && (
        <section className="capability-overview-section">
          <header className="capability-section-heading">
            <h4>{t("supplier.capabilities.primaryTitle")}</h4>
            <p>{t("supplier.capabilities.primaryHint")}</p>
          </header>
          <div className="capability-list">
            {visibleFeatures.map(({ id, support }) => (
              <div className="capability-row" key={id}>
                <strong>{t(`supplier.capabilities.features.${id}`)}</strong>
                <span className={`capability-support-label ${support}`}>
                  {t(`supplier.capabilities.${support}`)}
                </span>
              </div>
            ))}
          </div>
        </section>
      )}
      {clientHostFeatures.length > 0 && (
        <section className="capability-overview-section">
          <header className="capability-section-heading">
            <h4>{t("supplier.capabilities.clientHostTitle")}</h4>
            <p>{t("supplier.capabilities.clientHostHint")}</p>
          </header>
          <div className="capability-list">
            {clientHostFeatures.map(({ id, support }) => (
              <div className="capability-row" key={id}>
                <strong>
                  {id === "fileInput"
                    ? t("supplier.capabilities.clientHostFileInput")
                    : t(`supplier.capabilities.features.${id}`)}
                </strong>
                <span className={`capability-support-label ${support}`}>
                  {t(`supplier.capabilities.${support}`)}
                </span>
              </div>
            ))}
          </div>
        </section>
      )}
    </>
  );
}

export function SupplierChoiceCard({
  title,
  subtitle,
  icon,
  customIcon,
  fallback: Fallback,
  tone,
  actionLabel,
  onSelect,
}: {
  title: string;
  subtitle: string;
  icon: BrandIconData | null;
  customIcon?: CustomBrandIcon;
  fallback: LucideIcon;
  tone: string;
  actionLabel: string;
  onSelect: () => void;
}) {
  return (
    <button type="button" className="supplier-choice-card" onClick={onSelect}>
      <BrandBadge icon={icon} customIcon={customIcon} fallback={Fallback} tone={tone} />
      <span className="supplier-choice-copy">
        <strong>{title}</strong>
        <span>{subtitle}</span>
      </span>
      <span className="supplier-choice-action">
        <span>{actionLabel}</span>
        <ChevronRight size={16} />
      </span>
    </button>
  );
}

export function ToolProtocolPanel({
  title,
  state,
  configuredProtocol,
  disabled,
  onSelect,
}: {
  title: string;
  state: ReturnType<typeof toolProtocolMenuState>;
  configuredProtocol: ToolProtocolId | null;
  disabled: boolean;
  onSelect: (protocol: ToolProtocolId) => void;
}) {
  const { t } = useTranslation();
  return (
    <div className="tool-protocol-panel">
      <CompactChoiceMenu
        className={`tool-menu-choice ${state.fixed ? "fixed" : ""}`}
        ariaLabel={t("labels.toolMenu.protocolAria", { title })}
        value={state.selected}
        disabled={disabled || state.fixed}
        floating
        title={state.perModel ? t("labels.toolMenu.perModelProtocol") : state.fixed ? t("labels.toolMenu.fixedProtocol") : undefined}
        options={state.protocols.map((protocol) => ({
          value: protocol,
          label: TOOL_PROTOCOL_OPTIONS[protocol].label,
        }))}
        onChange={onSelect}
      />
      {state.changed && configuredProtocol && (
        <small className="tool-protocol-change">
          {t("labels.toolMenu.protocolChange", {
            protocol: TOOL_PROTOCOL_OPTIONS[configuredProtocol].label,
          })}
        </small>
      )}
    </div>
  );
}

export function CodexModelSourcePanel({ value, disabled, onChange }: {
  value: CodexModelSource;
  disabled: boolean;
  onChange: (value: CodexModelSource) => void;
}) {
  const { t } = useTranslation();
  return (
    <div className="tool-model-settings">
      <div className="tool-dock-protocol-field">
        <strong title={t("access.codexModels.title")}>{t("access.codexModels.title")}</strong>
        <div className="tool-protocol-panel">
          <CompactChoiceMenu
            className="tool-menu-choice"
            ariaLabel={t("access.codexModels.title")}
            value={value}
            disabled={disabled}
            floating
            options={[
              { value: "codex", label: t("access.codexModels.default") },
              { value: "const", label: t("access.codexModels.inject") },
            ]}
            onChange={onChange}
          />
          <small>{t(value === "const" ? "access.codexModels.injectHint" : "access.codexModels.hint")}</small>
        </div>
      </div>
    </div>
  );
}

type ClaudeModelRole = "opus" | "sonnet" | "haiku";

export function ClaudeModelSettingsPanel({
  settings,
  models,
  loading,
  changed,
  disabled,
  onChange,
}: {
  settings: ClaudeModelSettings;
  models: string[];
  loading: boolean;
  changed: boolean;
  disabled: boolean;
  onChange: (settings: ClaudeModelSettings) => void;
}) {
  const { t } = useTranslation();
  const availableModels = Array.from(new Set(models
    .map((model) => model.trim())
    .filter(Boolean)));
  const availableModelSet = new Set(availableModels);
  const modelOptions = availableModels.map((model) => ({ value: model, label: model }));
  const withCurrentModel = (value: string, options: Array<{ value: string; label: string; disabled?: boolean }>) => {
    if (!value
      || availableModelSet.has(value)
      || options.some((option) => option.value === value)) {
      return options;
    }
    return [{
      value,
      label: t("access.claudeModels.unavailableModel", { model: value }),
      disabled: true,
    }, ...options];
  };
  const mainOptions = withCurrentModel(settings.main, [
    { value: CLAUDE_MODEL_AUTO, label: t("access.claudeModels.auto") },
    ...modelOptions,
  ]);
  const roleOptions = (value: string) => withCurrentModel(value, [
    { value: CLAUDE_MODEL_AUTO, label: t("access.claudeModels.auto") },
    ...modelOptions,
  ]);
  const roles: Array<{ id: ClaudeModelRole; label: string }> = [
    { id: "opus", label: "Opus" },
    { id: "sonnet", label: "Sonnet" },
    { id: "haiku", label: "Haiku" },
  ];
  const visibleRoleValue = (role: ClaudeModelRole) => (
    settings[role] === CLAUDE_MODEL_FOLLOW_MAIN ? settings.main : settings[role]
  );
  return (
    <div className="tool-model-settings">
      <div className="claude-model-settings-heading">
        <span className="tool-model-settings-title" title={t("access.claudeModels.title")}>{t("access.claudeModels.title")}</span>
        {changed && <small>{t("labels.toolMenu.pendingApply")}</small>}
      </div>
      <div className="claude-model-field">
        <span title={t("access.claudeModels.main")}>{t("access.claudeModels.main")}</span>
        <CompactChoiceMenu
          className="tool-menu-choice"
          ariaLabel={t("access.claudeModels.mainAria")}
          value={settings.main}
          options={mainOptions}
          disabled={disabled}
          floating
          onChange={(main) => onChange({
            main,
            opus: main,
            sonnet: main,
            haiku: main,
          })}
        />
      </div>
      <details className="claude-role-models">
        <summary>
          <span title={t("access.claudeModels.roles")}>{t("access.claudeModels.roles")}</span>
        </summary>
        <div className="claude-role-model-fields">
          {roles.map(({ id, label }) => {
            const value = visibleRoleValue(id);
            return (
              <div className="claude-model-field claude-role-model-field" key={id}>
                <span title={label}>{label}</span>
                <CompactChoiceMenu
                  className="tool-menu-choice"
                  ariaLabel={t("access.claudeModels.roleAria", { role: label })}
                  value={value}
                  options={roleOptions(value)}
                  disabled={disabled}
                  floating
                  onChange={(nextValue) => onChange({ ...settings, [id]: nextValue })}
                />
              </div>
            );
          })}
        </div>
      </details>
      <p className="claude-model-settings-hint">
        {loading
          ? t("access.claudeModels.loading")
          : availableModels.length === 0
            ? t("access.claudeModels.empty")
            : t("access.claudeModels.compatibilityHint")}
      </p>
    </div>
  );
}

export function ToolConfigDetailPanel({
  state,
  onCopy,
}: {
  state: ReturnType<typeof toolConfigDetailState>;
  onCopy: (value: string) => Promise<boolean>;
}) {
  const { t } = useTranslation();
  return (
    <div className="tool-truth-panel">
      <ToolConfigPathField state={state} onCopy={onCopy} />
      {state.externalLaunchWarning && (
        <div className="tool-truth-row tool-truth-warning">
          <span title={t("labels.toolMenu.environment")}>{t("labels.toolMenu.environment")}</span>
          <strong>{state.externalLaunchWarning}</strong>
        </div>
      )}
      {state.capabilityWarning && (
        <div className="tool-truth-row tool-truth-warning">
          <span>{t("labels.toolMenu.capabilityLimit")}</span>
          <strong>{state.capabilityWarning}</strong>
        </div>
      )}
    </div>
  );
}

export function VsCodeExtensionLinkage({
  checking,
  vscodeConfigured,
  claudeConfigured,
  codexConfigured,
}: {
  checking: boolean;
  vscodeConfigured?: boolean;
  claudeConfigured?: boolean;
  codexConfigured?: boolean;
}) {
  const { t } = useTranslation();
  const extensions = [
    {
      name: "Copilot",
      fullName: "GitHub Copilot Chat",
      configured: vscodeConfigured,
      readyDetail: t("labels.toolMenu.useCurrent"),
      missingDetail: t("labels.toolMenu.afterCurrent"),
    },
    {
      name: "Codex",
      fullName: "Codex",
      configured: codexConfigured,
      readyDetail: t("labels.toolMenu.followCodex"),
      missingDetail: t("labels.toolMenu.afterCodex"),
    },
    {
      name: "Claude",
      fullName: "Claude Code",
      configured: claudeConfigured,
      readyDetail: t("labels.toolMenu.followClaude"),
      missingDetail: t("labels.toolMenu.afterClaude"),
    },
  ];

  return (
    <div className="tool-extension-linkage" aria-label={t("labels.toolMenu.extensionsAria")}>
      {extensions.map(({ name, fullName, configured, readyDetail, missingDetail }) => {
        const state = checking
          ? "checking"
          : configured === true
            ? "ready"
            : configured === false
              ? "missing"
              : "unknown";
        const detail = state === "checking"
          ? t("labels.toolMenu.checkConfiguration")
          : state === "ready"
            ? readyDetail
            : state === "missing"
              ? missingDetail
              : t("labels.toolMenu.waitingConfiguration");
        return (
          <div className={`tool-extension-linkage-row ${state}`} key={name}>
            <strong title={fullName}>{name}</strong>
            <span className="tool-extension-linkage-detail" title={detail}>
              <span className="tool-extension-linkage-dot" aria-hidden="true" />
              <span>{detail}</span>
            </span>
          </div>
        );
      })}
    </div>
  );
}

export function ToolConfigPathField({
  state,
  onCopy,
}: {
  state: ReturnType<typeof toolConfigDetailState>;
  onCopy: (value: string) => Promise<boolean>;
}) {
  const { t } = useTranslation();
  const [copied, setCopied] = useState(false);
  const copyable = state.fileCount > 0;
  async function copyPath() {
    if (!copyable || !await onCopy(state.primaryPath)) return;
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1200);
  }
  return (
    <div className="tool-config-file-row">
      <span title={t("labels.toolMenu.configuration")}>{t("labels.toolMenu.configuration")}</span>
      <div className="tool-config-file-copy">
        <span
          className="tool-config-file-path"
          aria-label={t("labels.toolMenu.configPath")}
          title={state.primaryPath}
        >
          {state.primaryPath}
        </span>
        <button
          type="button"
          className={`copy-feedback-button${copied ? " copied" : ""}`}
          aria-label={copied ? t("common.copied") : t("labels.toolMenu.copyConfigPath")}
          title={copied
            ? t("common.copied")
            : copyable
              ? t("labels.toolMenu.copyConfigPath")
              : t("labels.toolMenu.fileMissing")}
          disabled={!copyable}
          onClick={copyPath}
        >
          {copied ? <Check size={15} /> : <Copy size={15} />}
        </button>
      </div>
    </div>
  );
}

export type ToolDockPanel = {
  id: string;
  label: string;
  value?: string;
  content: React.ReactNode;
};

export function BlockingConfirmDialog({
  dialog,
  onResolve,
}: {
  dialog: ConfirmDialogState;
  onResolve: (confirmed: boolean) => void;
}) {
  const { t } = useTranslation();
  const dialogId = useId().replace(/:/g, "");
  const titleId = `confirm-title-${dialogId}`;
  const messageId = `confirm-message-${dialogId}`;
  const noticeOnly = dialog.mode === "notice";
  useEscapeDismiss(() => onResolve(noticeOnly));
  return (
    <DialogBackdrop
      className="confirm-backdrop"
      aria-labelledby={titleId}
      aria-describedby={messageId}
    >
      <div className="modal-panel confirm-modal">
        <div>
          <h2 id={titleId}>{dialog.title}</h2>
          <p id={messageId}>{dialog.message}</p>
        </div>
        <div className="confirm-actions">
          {!noticeOnly && (
            <button type="button" className="quiet" onClick={() => onResolve(false)}>
              {dialog.cancelText || t("common.cancel")}
            </button>
          )}
          <button
            type="button"
            className={dialog.tone === "danger" ? "danger" : "primary"}
            onClick={() => onResolve(true)}
          >
            {dialog.confirmText || t("common.confirm")}
          </button>
        </div>
      </div>
    </DialogBackdrop>
  );
}

export function ToolRemoveDialog({
  dialog,
  onResolve,
}: {
  dialog: ToolRemoveDialogState;
  onResolve: (decision: ToolRemoveDecision) => void;
}) {
  const { t } = useTranslation();
  const dialogId = useId().replace(/:/g, "");
  const titleId = `tool-remove-title-${dialogId}`;
  const warningId = `tool-remove-warning-${dialogId}`;
  const [selectedMode, setSelectedMode] =
    useState<ToolRemoveMode>("restore_pre_const");
  useEscapeDismiss(() => onResolve("cancel"));

  const option = (
    mode: ToolRemoveMode,
    title: string,
    description: string,
    badge?: string,
  ) => (
    <label
      className={`tool-remove-option${selectedMode === mode ? " is-selected" : ""}`}
    >
      <input
        type="radio"
        name={`tool-remove-mode-${dialogId}`}
        value={mode}
        checked={selectedMode === mode}
        onChange={() => setSelectedMode(mode)}
      />
      <span className="tool-remove-option-copy">
        <span className="tool-remove-option-title">
          <strong>{title}</strong>
          {badge ? <span className="tool-remove-option-badge">{badge}</span> : null}
        </span>
        <small>{description}</small>
      </span>
    </label>
  );

  return (
    <DialogBackdrop
      className="confirm-backdrop"
      aria-labelledby={titleId}
      aria-describedby={warningId}
    >
      <div className="modal-panel confirm-modal tool-remove-modal">
        <div>
          <h2 id={titleId}>
            {t("toolOperations.removeModes.title", { tool: dialog.toolTitle })}
          </h2>
        </div>
        <div
          className="tool-remove-options"
          role="radiogroup"
          aria-label={t("toolOperations.removeModes.selectionLabel")}
        >
          {option(
            "restore_pre_const",
            t("toolOperations.removeModes.restore.title"),
            t("toolOperations.removeModes.restore.description"),
            t("toolOperations.removeModes.defaultBadge"),
          )}
          {dialog.nativeOption
            ? option(
                "native_route",
                dialog.nativeOption.title,
                dialog.nativeOption.description,
              )
            : null}
        </div>
        <p id={warningId} className="tool-remove-warning">
          {t("toolOperations.removeModes.restartWarning", { tool: dialog.toolTitle })}
        </p>
        <div className="confirm-actions">
          <button type="button" className="quiet" onClick={() => onResolve("cancel")}>
            {t("common.cancel")}
          </button>
          <button type="button" className="danger" onClick={() => onResolve(selectedMode)}>
            {t("toolOperations.actions.removeConfiguration")}
          </button>
        </div>
      </div>
    </DialogBackdrop>
  );
}

export function ChannelDuplicateDialog({
  dialog,
  onResolve,
}: {
  dialog: ChannelDuplicateDialogState;
  onResolve: (decision: ChannelDuplicateDecision) => void;
}) {
  const { t } = useTranslation();
  const dialogId = useId().replace(/:/g, "");
  const titleId = `duplicate-channel-title-${dialogId}`;
  const messageId = `duplicate-channel-message-${dialogId}`;
  const [selectedChannelId, setSelectedChannelId] = useState(
    dialog.candidates[0]?.channel_id ?? "",
  );
  useEscapeDismiss(() => onResolve({ action: "cancel" }));
  const effectiveSelectedChannelId = dialog.candidates.some(
    (candidate) => candidate.channel_id === selectedChannelId,
  )
    ? selectedChannelId
    : dialog.candidates[0]?.channel_id ?? "";
  const selected = dialog.candidates.find(
    (candidate) => candidate.channel_id === effectiveSelectedChannelId,
  ) ?? dialog.candidates[0];
  return (
    <DialogBackdrop
      className="confirm-backdrop"
      aria-labelledby={titleId}
      aria-describedby={messageId}
    >
      <div className="modal-panel confirm-modal duplicate-channel-modal">
        <div>
          <h2 id={titleId}>{t("supplier.duplicate.title")}</h2>
          <p id={messageId}>
            {t("supplier.duplicate.message", { name: dialog.draftName })}
          </p>
          {dialog.candidates.length > 1 ? (
            <label>
              {t("supplier.duplicate.chooseExisting")}
              <select
                value={effectiveSelectedChannelId}
                onChange={(event) => setSelectedChannelId(event.target.value)}
              >
                {dialog.candidates.map((candidate) => (
                  <option key={candidate.channel_id} value={candidate.channel_id}>
                    {candidate.name || candidate.channel_id} · {candidate.channel_id}
                  </option>
                ))}
              </select>
            </label>
          ) : selected ? (
            <div className="duplicate-channel-target">
              <span>{t("supplier.duplicate.existingChannel")}</span>
              <strong>{selected.name || selected.channel_id}</strong>
            </div>
          ) : null}
        </div>
        <div className="confirm-actions duplicate-channel-actions">
          <button
            type="button"
            className="quiet"
            onClick={() => onResolve({ action: "cancel" })}
          >
            {t("common.cancel")}
          </button>
          <button
            type="button"
            className="quiet"
            onClick={() => onResolve({ action: "create_new" })}
          >
            {t("supplier.duplicate.createNew")}
          </button>
          <button
            type="button"
            className="primary"
            disabled={!selected}
            onClick={() => {
              if (selected) {
                onResolve({
                  action: "open_existing",
                  channelId: selected.channel_id,
                });
              }
            }}
          >
            {t("supplier.duplicate.openExisting")}
          </button>
        </div>
      </div>
    </DialogBackdrop>
  );
}

export function QuickCard({
  id,
  title,
  description,
  icon,
  customIcon,
  badge,
  fallback: Fallback,
  tone,
  configured,
  statusText,
  statusTone,
  actionLabel,
  disabled,
  busy,
  progress,
  onAction,
  onConfigure,
  onRemoveConfig,
  onLocate,
  onHide,
  panels = [],
}: {
  id?: string;
  title: string;
  description?: string;
  icon: BrandIconData | null;
  customIcon?: CustomBrandIcon;
  badge?: "CLI" | "CN";
  fallback: LucideIcon;
  tone: string;
  configured?: boolean;
  statusText?: string;
  statusTone?: ToolConfigTone;
  actionLabel: string;
  disabled?: boolean;
  busy?: boolean;
  progress?: number | null;
  onAction: () => void;
  onConfigure?: () => void;
  onRemoveConfig?: () => void;
  onLocate?: () => void;
  onHide?: () => void;
  panels?: ToolDockPanel[];
}) {
  const { t } = useTranslation();
  const toolMenu = useToolDockMenu();
  const menuId = id ?? title;
  const menuOpen = toolMenu.openMenuId === menuId;
  const menuTargeting = toolMenu.openMenuId !== null;
  const dockRef = useRef<HTMLDivElement>(null);
  const buttonRef = useRef<HTMLButtonElement>(null);
  const longPressTimerRef = useRef<number | null>(null);
  const longPressStartRef = useRef<{ pointerId: number; x: number; y: number } | null>(null);
  const suppressNextClickRef = useRef(false);
  const [menuPlacement, setMenuPlacement] = useState<AnchoredMenuPlacement>({
    left: 8,
    width: TOOL_MENU_PRIMARY_WIDTH,
  });
  const progressMaskId = `tool-progress-${useId().replace(/:/g, "")}`;
  const progressValue = typeof progress === "number"
    ? Math.max(0, Math.min(1, progress))
    : 0;
  const progressDegrees = progressValue * 360;
  const visibleLabel = busy ? statusText || t("labels.toolMenu.working") : "";
  const visibleLabelMatch = visibleLabel.replace(/\r?\n/g, " ").match(/^(.*?)(?:\s+)(\d+\/\d+)$/);
  const visibleLabelText = visibleLabelMatch?.[1] || visibleLabel;
  const visibleLabelCount = visibleLabelMatch?.[2] || "";
  const primaryPanel = panels[0] ?? null;
  const activePanel = toolMenu.contextPanelOpen ? primaryPanel : null;
  useEffect(() => {
    if (disabled && menuOpen) toolMenu.closeMenu();
  }, [disabled, menuOpen, toolMenu.closeMenu]);
  useEffect(() => () => {
    if (longPressTimerRef.current !== null) {
      window.clearTimeout(longPressTimerRef.current);
    }
  }, []);
  const updateMenuPlacement = (showContextPanel = Boolean(activePanel)) => {
    const next = resolveAnchoredMenuPlacement(
      dockRef.current,
      buttonRef.current,
      showContextPanel ? TOOL_MENU_EXPANDED_WIDTH : TOOL_MENU_PRIMARY_WIDTH,
    );
    if (!next) return;
    setMenuPlacement((current) => (
      current.left === next.left && current.width === next.width ? current : next
    ));
  };
  useEffect(() => {
    if (!menuOpen) return;
    const handleResize = () => updateMenuPlacement();
    window.addEventListener("resize", handleResize);
    return () => window.removeEventListener("resize", handleResize);
  }, [menuOpen, activePanel]);
  const openPinnedMenu = () => {
    if (disabled) return;
    if (!menuOpen) updateMenuPlacement();
    toolMenu.pinMenu(menuId);
  };
  const clearLongPress = () => {
    if (longPressTimerRef.current !== null) {
      window.clearTimeout(longPressTimerRef.current);
      longPressTimerRef.current = null;
    }
    longPressStartRef.current = null;
  };
  const runMenuAction = (action?: () => void) => {
    if (disabled) return;
    toolMenu.closeMenu();
    action?.();
  };
  const handlePointerDown = (event: React.PointerEvent<HTMLDivElement>) => {
    if (disabled || event.button !== 0) return;
    clearLongPress();
    suppressNextClickRef.current = false;
    const { pointerId, clientX, clientY } = event;
    longPressStartRef.current = { pointerId, x: clientX, y: clientY };
    longPressTimerRef.current = window.setTimeout(() => {
      longPressTimerRef.current = null;
      suppressNextClickRef.current = true;
      openPinnedMenu();
    }, 500);
  };
  const handlePointerMove = (event: React.PointerEvent<HTMLDivElement>) => {
    const start = longPressStartRef.current;
    if (!start || start.pointerId !== event.pointerId) return;
    if (Math.hypot(event.clientX - start.x, event.clientY - start.y) > 8) {
      clearLongPress();
    }
  };
  const handlePointerEnd = (event: React.PointerEvent<HTMLDivElement>) => {
    const start = longPressStartRef.current;
    if (start && start.pointerId !== event.pointerId) return;
    clearLongPress();
  };
  const handleToolClick = (event: React.MouseEvent<HTMLButtonElement>) => {
    if (suppressNextClickRef.current) {
      suppressNextClickRef.current = false;
      event.preventDefault();
      return;
    }
    runMenuAction(onAction);
  };
  const handleContextMenu = (event: React.MouseEvent<HTMLDivElement>) => {
    event.preventDefault();
    clearLongPress();
    if (event.button === 0) suppressNextClickRef.current = true;
    openPinnedMenu();
  };
  const handleToolKeyDown = (event: React.KeyboardEvent<HTMLButtonElement>) => {
    if ((event.shiftKey && event.key === "F10") || event.key === "ContextMenu") {
      event.preventDefault();
      openPinnedMenu();
    }
  };
  return (
    <div
      ref={dockRef}
      className={`quick-card tool-dock ${disabled ? "disabled" : ""} ${busy ? "busy" : ""} ${menuTargeting ? "menu-targeting" : ""}`}
      onPointerDown={handlePointerDown}
      onPointerMove={handlePointerMove}
      onPointerUp={handlePointerEnd}
      onPointerCancel={handlePointerEnd}
      onPointerLeave={clearLongPress}
      onContextMenu={handleContextMenu}
      aria-busy={busy || undefined}
    >
      <div className="tool-dock-icon-wrap">
        <button
          ref={buttonRef}
          type="button"
          id={id ? `${id}-tool-button` : undefined}
          className={`tool-dock-button ${menuOpen ? "active" : ""}`}
          style={{ "--tool-accent": tone } as React.CSSProperties}
          disabled={disabled}
          aria-label={title}
          aria-expanded={menuOpen}
          aria-controls={id ? `${id}-tool-menu` : undefined}
          onClick={handleToolClick}
          onKeyDown={handleToolKeyDown}
        >
          {busy && (
            <svg
              className="tool-dock-progress-reveal"
              viewBox="0 0 48 48"
              role="progressbar"
              aria-label={`${title} ${visibleLabel}`}
              aria-valuemin={0}
              aria-valuemax={100}
              aria-valuenow={Math.round(progressValue * 100)}
              aria-valuetext={visibleLabel}
            >
              <defs>
                <mask id={progressMaskId} maskUnits="userSpaceOnUse">
                  <rect width="48" height="48" rx="9" fill="white" />
                  <circle
                    className="tool-dock-progress-sector"
                    cx="24"
                    cy="24"
                    r="24"
                    pathLength="1"
                    fill="none"
                    stroke="black"
                    strokeWidth="48"
                    strokeDasharray={`${progressValue} ${1 - progressValue}`}
                    transform="rotate(-90 24 24)"
                  />
                </mask>
              </defs>
              <rect
                className="tool-dock-progress-overlay"
                width="48"
                height="48"
                rx="9"
                mask={`url(#${progressMaskId})`}
              />
              <rect
                className="tool-dock-progress-glass-rim"
                x="0.75"
                y="0.75"
                width="46.5"
                height="46.5"
                rx="8.25"
                mask={`url(#${progressMaskId})`}
              />
              {progressValue > 0 && progressValue < 1 && (
                <line
                  className="tool-dock-progress-edge"
                  x1="24"
                  y1="12"
                  x2="24"
                  y2="-10"
                  style={{ transform: `rotate(${progressDegrees}deg)` }}
                  vectorEffect="non-scaling-stroke"
                />
              )}
            </svg>
          )}
          <BrandBadge icon={icon} customIcon={customIcon} fallback={Fallback} tone={tone} />
        </button>
        {badge && <span className="tool-edition-badge" aria-hidden="true">{badge}</span>}
      </div>
      <small
        className={`tool-dock-status-text ${busy ? `${statusTone ?? "pending"} is-live` : "idle"}`}
        title={visibleLabel || undefined}
        aria-live={busy ? "polite" : undefined}
        aria-hidden={!busy || undefined}
      >
        {busy ? (
          <span className="tool-dock-status-copy" aria-label={visibleLabel}>
            <span className="tool-dock-status-message" aria-hidden="true">
              <span className="tool-dock-status-spinner" />
              <span className="tool-dock-status-content">
                <span className="tool-dock-status-label">{visibleLabelText}</span>
                {visibleLabelCount && (
                  <>
                    {" "}
                    <span className="tool-dock-status-count">{visibleLabelCount}</span>
                  </>
                )}
              </span>
            </span>
          </span>
        ) : (
          <span className={`tool-dock-hover-name ${configured ? "is-configured" : ""}`}>
            {configured && <span className="tool-dock-configured-dot" aria-hidden="true" />}
            <span className="tool-dock-hover-name-label">{title}</span>
          </span>
        )}
      </small>
      {menuOpen && !disabled && (
        <div
          className={`quick-card-menu-panel tool-dock-menu ${activePanel ? "has-context" : ""}`}
          id={id ? `${id}-tool-menu` : undefined}
          style={{
            ...anchoredMenuStyle(menuPlacement),
            "--tool-menu-primary-width": `${Math.min(TOOL_MENU_PRIMARY_WIDTH, menuPlacement.width)}px`,
            "--tool-menu-expanded-width": `${menuPlacement.width}px`,
          } as React.CSSProperties}
          onPointerDownCapture={() => toolMenu.pinMenu(menuId)}
          onPointerDown={(event) => event.stopPropagation()}
          onContextMenu={(event) => event.stopPropagation()}
        >
          <div className="tool-dock-menu-main">
            <div className="tool-dock-menu-header">
              <div className="tool-dock-menu-title">
                <BrandBadge icon={icon} customIcon={customIcon} fallback={Fallback} tone={tone} />
                <div className="tool-dock-menu-title-copy">
                  <strong title={description}>{title}</strong>
                  {statusText && (
                    <small
                      className={`tool-dock-menu-title-status ${statusTone ?? "pending"}`}
                      title={statusText}
                    >
                      {statusText}
                    </small>
                  )}
                </div>
              </div>
            </div>
            <div className="tool-dock-menu-actions">
              <button
                type="button"
                className="primary-menu-action"
                title={actionLabel}
                disabled={disabled}
                onClick={() => runMenuAction(onConfigure)}
              >
                {actionLabel}
              </button>
              {onRemoveConfig && <button type="button" title={t("labels.toolMenu.removeConfiguration")} onClick={() => runMenuAction(onRemoveConfig)}>{t("labels.toolMenu.removeConfiguration")}</button>}
              <button type="button" title={t("labels.toolMenu.locateProgram")} onClick={() => runMenuAction(onLocate)}>{t("labels.toolMenu.locateProgram")}</button>
              {onHide && (
                <button type="button" className="tool-dock-hide-action" title={t("labels.toolMenu.hide")} onClick={() => runMenuAction(onHide)}>
                  {t("labels.toolMenu.hide")}
                </button>
              )}
              {primaryPanel && (
                <button
                  type="button"
                  className={`tool-dock-context-toggle ${activePanel ? "active" : ""}`}
                  title={primaryPanel.label}
                  aria-label={activePanel
                    ? t("labels.toolMenu.collapsePanel", { panel: primaryPanel.label })
                    : `${title} ${primaryPanel.label}`}
                  aria-expanded={Boolean(activePanel)}
                  onClick={(event) => {
                    event.stopPropagation();
                    const nextOpen = !toolMenu.contextPanelOpen;
                    updateMenuPlacement(nextOpen);
                    toolMenu.setContextPanelOpen(nextOpen);
                  }}
                >
                  <span>{primaryPanel.label}</span>
                  <ChevronDown size={16} aria-hidden="true" />
                </button>
              )}
            </div>
          </div>
          {activePanel && (
            <div className="tool-dock-context" role="group" aria-label={`${title} ${activePanel.label}`}>
              <div className="tool-dock-context-body">{activePanel.content}</div>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

type HiddenToolCard = {
  tool: ToolCatalogId;
  title: string;
  badge?: "CLI" | "CN";
  icon: BrandIconData | null;
  customIcon?: CustomBrandIcon;
  fallback: LucideIcon;
  tone: string;
};

export function HiddenToolMenu({
  cards,
  onShow,
  onResetOrder,
}: {
  cards: readonly HiddenToolCard[];
  onShow: (tool: ToolCatalogId) => void;
  onResetOrder: () => void;
}) {
  const { t } = useTranslation();
  const toolMenu = useToolDockMenu();
  const menuId = "hidden-tools";
  const menuOpen = toolMenu.openMenuId === menuId;
  const dockRef = useRef<HTMLDivElement>(null);
  const buttonRef = useRef<HTMLButtonElement>(null);
  const longPressTimerRef = useRef<number | null>(null);
  const longPressStartRef = useRef<{ pointerId: number; x: number; y: number } | null>(null);
  const suppressNextClickRef = useRef(false);
  const [menuPlacement, setMenuPlacement] = useState<HiddenToolMenuPlacement>({
    left: 8,
    width: HIDDEN_TOOL_MENU_WIDTH,
    top: 54,
    bottom: null,
    maxHeight: 420,
  });
  useEffect(() => () => {
    if (longPressTimerRef.current !== null) {
      window.clearTimeout(longPressTimerRef.current);
    }
  }, []);

  const updateMenuPlacement = () => {
    const next = resolveHiddenToolMenuPlacement(
      dockRef.current,
      buttonRef.current,
      cards.length,
    );
    if (!next) return;
    setMenuPlacement((current) => (
      current.left === next.left
        && current.width === next.width
        && current.top === next.top
        && current.bottom === next.bottom
        && current.maxHeight === next.maxHeight
        ? current
        : next
    ));
  };

  useEffect(() => {
    if (!menuOpen) return;
    const handleResize = () => updateMenuPlacement();
    window.addEventListener("resize", handleResize);
    return () => window.removeEventListener("resize", handleResize);
  }, [menuOpen]);

  const toggleMenu = () => {
    updateMenuPlacement();
    toolMenu.togglePinnedMenu(menuId);
  };

  const openPinnedMenu = () => {
    updateMenuPlacement();
    toolMenu.pinMenu(menuId);
  };

  const clearLongPress = () => {
    if (longPressTimerRef.current !== null) {
      window.clearTimeout(longPressTimerRef.current);
      longPressTimerRef.current = null;
    }
    longPressStartRef.current = null;
  };

  const handlePointerDown = (event: React.PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    clearLongPress();
    suppressNextClickRef.current = false;
    longPressStartRef.current = {
      pointerId: event.pointerId,
      x: event.clientX,
      y: event.clientY,
    };
    longPressTimerRef.current = window.setTimeout(() => {
      longPressTimerRef.current = null;
      suppressNextClickRef.current = true;
      openPinnedMenu();
    }, 500);
  };

  const handlePointerMove = (event: React.PointerEvent<HTMLDivElement>) => {
    const start = longPressStartRef.current;
    if (!start || start.pointerId !== event.pointerId) return;
    if (Math.hypot(event.clientX - start.x, event.clientY - start.y) > 8) {
      clearLongPress();
    }
  };

  const handlePointerEnd = (event: React.PointerEvent<HTMLDivElement>) => {
    const start = longPressStartRef.current;
    if (start && start.pointerId !== event.pointerId) return;
    clearLongPress();
  };

  const handleClick = (event: React.MouseEvent<HTMLButtonElement>) => {
    if (suppressNextClickRef.current) {
      suppressNextClickRef.current = false;
      event.preventDefault();
      return;
    }
    toggleMenu();
  };

  const handleContextMenu = (event: React.MouseEvent<HTMLDivElement>) => {
    event.preventDefault();
    clearLongPress();
    openPinnedMenu();
  };

  const handleKeyDown = (event: React.KeyboardEvent<HTMLButtonElement>) => {
    if ((event.shiftKey && event.key === "F10") || event.key === "ContextMenu") {
      event.preventDefault();
      openPinnedMenu();
    }
  };

  return (
    <div
      ref={dockRef}
      className="quick-card tool-dock tool-dock-add"
      onPointerDown={handlePointerDown}
      onPointerMove={handlePointerMove}
      onPointerUp={handlePointerEnd}
      onPointerCancel={handlePointerEnd}
      onPointerLeave={clearLongPress}
      onContextMenu={handleContextMenu}
    >
      <div className="tool-dock-icon-wrap">
        <button
          ref={buttonRef}
          type="button"
          className={`tool-dock-button ${menuOpen ? "active" : ""}`}
          style={{ "--tool-accent": "#64748b" } as React.CSSProperties}
          aria-label={t("labels.toolMenu.addTool")}
          aria-expanded={menuOpen}
          aria-controls="hidden-tools-menu"
          onClick={handleClick}
          onKeyDown={handleKeyDown}
        >
          <Plus size={25} strokeWidth={1.8} aria-hidden="true" />
        </button>
      </div>
      <small className="tool-dock-status-text idle" aria-hidden="true">
        <span className="tool-dock-hover-name">
          <span className="tool-dock-hover-name-label">{t("labels.toolMenu.add")}</span>
        </span>
      </small>
      {menuOpen && (
        <div
          id="hidden-tools-menu"
          className="quick-card-menu-panel tool-dock-menu tool-hidden-menu"
          style={{
            ...hiddenToolMenuStyle(menuPlacement),
            "--tool-menu-primary-width": `${menuPlacement.width}px`,
          } as React.CSSProperties}
          role="menu"
          onPointerDownCapture={() => toolMenu.pinMenu(menuId)}
          onPointerDown={(event) => event.stopPropagation()}
          onContextMenu={(event) => event.stopPropagation()}
        >
          {cards.length > 0 && (
            <div className="tool-hidden-menu-heading" role="presentation">
              {t("labels.toolMenu.hiddenTools")}
            </div>
          )}
          <div className="tool-hidden-list">
            {cards.map((card) => (
              <button
                key={card.tool}
                type="button"
                role="menuitem"
                aria-label={t("labels.toolMenu.showTool", { tool: card.title })}
                onClick={() => {
                  toolMenu.closeMenu();
                  onShow(card.tool);
                }}
              >
                <BrandBadge
                  icon={card.icon}
                  customIcon={card.customIcon}
                  fallback={card.fallback}
                  tone={card.tone}
                />
                <span>{card.title}</span>
                {card.badge && <small className="tool-edition-label">{card.badge}</small>}
              </button>
            ))}
            <button
              type="button"
              className="tool-hidden-reset-order"
              role="menuitem"
              onClick={() => {
                toolMenu.closeMenu();
                onResetOrder();
              }}
            >
              <RotateCcw size={16} strokeWidth={1.8} aria-hidden="true" />
              <span>{t("labels.toolMenu.resetOrder")}</span>
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

export function BrandBadge({
  icon,
  customIcon,
  fallback: Fallback,
  tone,
}: {
  icon: BrandIconData | null;
  customIcon?: CustomBrandIcon;
  fallback: LucideIcon;
  tone: string;
}) {
  const customIconUrl = customIcon ? customBrandIconUrl(customIcon) : "";
  const renderArtwork = (className?: string) => (
    customIconUrl ? (
      <img className={className} src={customIconUrl} alt="" />
    ) : icon ? (
      <svg className={className} viewBox="0 0 24 24" aria-hidden="true">
        <path fill="currentColor" d={icon.path} />
      </svg>
    ) : (
      <Fallback className={className} size={22} strokeWidth={2.2} aria-hidden="true" />
    )
  );
  return (
    <div
      className={`brand-badge${customIcon ? ` brand-badge-${customIcon}` : ""}`}
      style={{ color: tone }}
    >
      {renderArtwork()}
    </div>
  );
}

export function customBrandIconUrl(icon: CustomBrandIcon) {
  switch (icon) {
    case "anythingllm":
      return anythingllmIconUrl;
    case "azure":
      return azureIconUrl;
    case "bedrock":
      return bedrockIconUrl;
    case "claude":
      return claudeIconUrl;
    case "claude-science":
      return claudeScienceIconUrl;
    case "codex":
      return codexIconUrl;
    case "copilot":
      return copilotIconUrl;
    case "cline":
      return clineIconUrl;
    case "trae":
      return traeIconUrl;
    case "trae-work":
      return traeWorkIconUrl;
    case "deepseek-harness":
      return deepseekHarnessIconUrl;
    case "gemini":
      return geminiIconUrl;
    case "goose":
      return gooseIconUrl;
    case "grok":
      return grokIconUrl;
    case "hermes":
      return hermesIconUrl;
    case "kimi":
      return kimiIconUrl;
    case "mimocode":
      return mimoCodeIconUrl;
    case "minimax-code":
      return miniMaxCodeIconUrl;
    case "omniroute":
      return omniRouteIconUrl;
    case "open-design":
      return openDesignIconUrl;
    case "open-interpreter":
      return openInterpreterIconUrl;
    case "openclaw":
      return openClawIconUrl;
    case "opencode":
      return openCodeIconUrl;
    case "openscience":
      return openScienceIconUrl;
    case "qwen":
      return qwenIconUrl;
    case "pi":
      return piIconUrl;
    case "raven":
      return ravenIconUrl;
    case "reasonix":
      return reasonixIconUrl;
    case "mistral-vibe":
      return mistralVibeIconUrl;
    case "vibe-trading":
      return vibeTradingIconUrl;
    case "vscode":
      return vscodeIconUrl;
    case "workbuddy":
      return workBuddyIconUrl;
    case "zcode":
      return zcodeIconUrl;
  }
}
