import { Fragment, useEffect, useMemo, useState } from "react";
import { Check, ChevronDown, ChevronRight, Copy, ExternalLink, LoaderCircle, RefreshCw } from "lucide-react";
import { useTranslation } from "react-i18next";

import type { CopyField, ModelCompatibilityPolicyState } from "../appTypes";
import { DialogBackdrop } from "../DialogBackdrop";
import { ModalCloseButton } from "../ModalCloseButton";
import { publicModelId } from "../modelPresentation";
import type { RefreshOptions } from "../refreshPolicy";

type GuideModelListProps = {
  availableModels: string[];
  modelStatus: ModelCompatibilityPolicyState["status"];
  copiedField: CopyField | null;
  onCopy: (field: CopyField, label: string, value: string) => void;
  onRefreshModels: (options?: RefreshOptions) => void | Promise<void>;
};

type ConnectionGuideModalProps = GuideModelListProps & {
  rootUrl: string;
  apiKey: string;
  openaiBaseUrl: string;
  openaiChatUrl: string;
  anthropicBaseUrl: string;
  geminiBaseUrl: string;
  onOpenRoot: (url: string) => void;
  onCopyImageCommand: () => void;
  onOpenMediaSkillSettings: () => void;
  onClose: () => void;
};

type ProtocolGroup = {
  id: string;
  provider: string;
  title: string;
  hint?: string;
  addresses: {
    field: CopyField;
    label: string;
    url: string;
  }[];
};

export function ConnectionGuideModal({
  rootUrl,
  apiKey,
  openaiBaseUrl,
  openaiChatUrl,
  anthropicBaseUrl,
  geminiBaseUrl,
  availableModels,
  modelStatus,
  copiedField,
  onCopy,
  onRefreshModels,
  onOpenRoot,
  onCopyImageCommand,
  onOpenMediaSkillSettings,
  onClose,
}: ConnectionGuideModalProps) {
  const { t } = useTranslation();
  const [modelList, setModelList] = useState<"unopened" | "expanded" | "collapsed">("unopened");
  const modelsExpanded = modelList === "expanded";
  const modelsRequested = modelList !== "unopened";
  useEffect(() => {
    if (modelsRequested) void onRefreshModels();
  }, [modelsRequested, onRefreshModels]);

  function copyIconButton(field: CopyField, label: string, value: string, actionLabel: string) {
    const copied = copiedField === field;
    const title = copied ? t("common.copied") : actionLabel;
    return (
      <button
        type="button"
        className={`guide-copy-button copy-feedback-button${copied ? " copied" : ""}`}
        disabled={!value}
        title={title}
        aria-label={title}
        onClick={() => onCopy(field, label, value)}
      >
        {copied ? <Check size={15} aria-hidden="true" /> : <Copy size={15} aria-hidden="true" />}
      </button>
    );
  }

  function mediaApiCopyButton(kind: "image" | "video" | "speech") {
    const field = `guide-${kind}-usage` as const;
    const label = t(`access.guide.${kind}Copy`);
    return (
      <button
        type="button"
        className="inline-text-link"
        disabled={!openaiBaseUrl}
        onClick={() => {
          const options = { baseUrl: openaiBaseUrl };
          const instructions = [t("access.guide.mediaApiAuth", options), t(`access.guide.${kind}Usage`, options)];
          onCopy(field, label, instructions.join("\n\n"));
        }}
      >
        {copiedField === field ? t("common.copied") : label}
      </button>
    );
  }

  const protocols: ProtocolGroup[] = [
    {
      id: "openai",
      provider: "OpenAI",
      title: t("access.guide.openaiProtocol"),
      addresses: [
        { field: "guide-openai-url", label: t("access.guide.baseUrl"), url: openaiBaseUrl },
        {
          field: "guide-openai-responses-url",
          label: t("access.guide.responsesUrl"),
          url: openaiBaseUrl ? `${openaiBaseUrl}/responses` : "",
        },
        { field: "guide-openai-chat-url", label: t("access.guide.chatUrl"), url: openaiChatUrl },
        {
          field: "guide-openai-models-url",
          label: t("access.guide.modelsUrl"),
          url: openaiBaseUrl ? `${openaiBaseUrl}/models` : "",
        },
      ],
    },
    {
      id: "anthropic",
      provider: "Anthropic",
      title: t("access.guide.anthropicProtocol"),
      addresses: [
        { field: "guide-anthropic-url", label: t("access.guide.baseUrl"), url: anthropicBaseUrl },
        {
          field: "guide-anthropic-messages-url",
          label: t("access.guide.messagesUrl"),
          url: anthropicBaseUrl ? `${anthropicBaseUrl}/v1/messages` : "",
        },
        {
          field: "guide-anthropic-models-url",
          label: t("access.guide.modelsUrl"),
          url: anthropicBaseUrl ? `${anthropicBaseUrl}/v1/models` : "",
        },
      ],
    },
    {
      id: "gemini",
      provider: "Gemini",
      title: t("access.guide.geminiProtocol"),
      hint: t("access.guide.geminiModelHint"),
      addresses: [
        { field: "guide-gemini-url", label: t("access.guide.baseUrl"), url: geminiBaseUrl },
        {
          field: "guide-gemini-generate-url",
          label: t("access.guide.requestUrl"),
          url: geminiBaseUrl ? `${geminiBaseUrl}/v1beta/models/{model}:generateContent` : "",
        },
        {
          field: "guide-gemini-stream-url",
          label: t("access.guide.streamUrl"),
          url: geminiBaseUrl ? `${geminiBaseUrl}/v1beta/models/{model}:streamGenerateContent?alt=sse` : "",
        },
        {
          field: "guide-gemini-models-url",
          label: t("access.guide.modelsUrl"),
          url: geminiBaseUrl ? `${geminiBaseUrl}/v1beta/models` : "",
        },
      ],
    },
  ];

  return (
    <DialogBackdrop aria-labelledby="connection-guide-title">
      <div className="modal-panel access-guide-modal">
        <div className="section-title-row">
          <div>
            <h2 id="connection-guide-title">{t("access.connectionGuide")}</h2>
            <p className="section-hint">{t("access.connectionGuideHint")}</p>
          </div>
          <ModalCloseButton onClick={onClose} />
        </div>

        <section className="guide-root-card" aria-label={t("access.guide.rootTitle")}>
          <div>
            <strong>{t("access.guide.rootTitle")}</strong>
            <small>{t("access.guide.rootHint")}</small>
          </div>
          <button type="button" className="guide-root-link" onClick={() => onOpenRoot(rootUrl)}>
            <code>{rootUrl}</code>
            <ExternalLink size={15} />
          </button>
        </section>

        <section className="guide-surface-section" aria-labelledby="guide-protocols-title">
          <div className="guide-section-heading">
            <h3 id="guide-protocols-title">{t("access.guide.protocolsTitle")}</h3>
            <p>{t("access.guide.protocolsHint")}</p>
          </div>
          <div className="guide-surface-list">
            <div className="guide-surface-row">
              <div className="guide-surface-copy">
                <strong>{t("access.localApiKey")}</strong>
                <small>{t("access.guide.apiKeyHint")}</small>
              </div>
              <code>{apiKey || t("access.notSet")}</code>
              {copyIconButton("api-key", t("access.localApiKey"), apiKey, t("access.copyApiKey"))}
            </div>
          </div>
          {protocols.map((protocol) => (
            <section
              className="guide-protocol-group"
              key={protocol.id}
              aria-labelledby={`guide-protocol-${protocol.id}`}
            >
              <div className="guide-protocol-heading">
                <h4 id={`guide-protocol-${protocol.id}`}>{protocol.title}</h4>
                {protocol.hint && <p>{protocol.hint}</p>}
              </div>
              <div className="guide-surface-list">
                {protocol.addresses.map((address) => {
                  const label = `${protocol.provider} ${address.label}`;
                  return (
                    <div className="guide-surface-row" key={address.field}>
                      <span>{address.label}</span>
                      <code title={address.url}>{address.url || t("access.notSet")}</code>
                      {copyIconButton(address.field, label, address.url, t("access.copyConnectionValue", { value: label }))}
                    </div>
                  );
                })}
              </div>
            </section>
          ))}
        </section>

        <section className="guide-steps">
          <h3>{t("access.guide.stepsTitle")}</h3>
          <ol>
            <li>{t("access.guide.stepChooseSurface")}</li>
            <li>{t("access.guide.stepConfigure")}</li>
            <li>
              {t("access.guide.stepRequest")}{" "}
              <button
                type="button"
                className="inline-text-link guide-models-toggle"
                aria-expanded={modelsExpanded}
                aria-controls="guide-models-panel"
                onClick={() => setModelList(modelsExpanded ? "collapsed" : "expanded")}
              >
                {t("access.guide.showModelIds")}
                {modelsExpanded ? <ChevronDown size={13} aria-hidden="true" /> : <ChevronRight size={13} aria-hidden="true" />}
              </button>
              <div id="guide-models-panel" hidden={!modelsExpanded}>
                {modelsExpanded && (
                  <GuideModelList
                    availableModels={availableModels}
                    modelStatus={modelStatus}
                    copiedField={copiedField}
                    onCopy={onCopy}
                    onRefreshModels={onRefreshModels}
                  />
                )}
              </div>
            </li>
          </ol>
          <p className="guide-auth-note">{t("access.guide.authNote")}</p>
        </section>

        <details className="guide-surface-section guide-media" aria-labelledby="guide-media-title">
          <summary className="guide-section-heading">
            <h3 id="guide-media-title">{t("access.guide.mediaTitle")}</h3>
          </summary>
          <div className="guide-media-body">
            <h4>{t("access.guide.mediaChatTitle")}</h4>
            <p>{t("access.guide.mediaHint")}</p>
            <dl className="guide-media-prompts">
              {(["discover", "generate", "edit", "video", "speech", "music"] as const).map((kind) => {
                const prompt = t(`access.guide.prompts.${kind}`);
                const label = t(`access.guide.promptLabels.${kind}`);
                const field = `guide-prompt-${kind}` as const;
                return (
                  <Fragment key={kind}>
                    <dt>{label}</dt>
                    <dd>
                      <span>{prompt}</span>
                      {copyIconButton(field, label, prompt, t("access.guide.copyPrompt", { label }))}
                    </dd>
                  </Fragment>
                );
              })}
            </dl>
            <p className="guide-media-skill-help">
              {t("access.guide.mediaSkillHelp")}{" "}
              <button type="button" className="inline-text-link" aria-haspopup="dialog" onClick={onOpenMediaSkillSettings}>
                {t("access.guide.mediaSkillSettings", { setting: t("access.toolImageSkill") })}
              </button>
            </p>
            <details className="guide-media-api">
              <summary>{t("access.guide.mediaApiTitle")}</summary>
              <p>{t("access.guide.mediaApiHint")}</p>
              <dl>
                <dt>{t("access.guide.mediaImages")}</dt>
                <dd>
                  <code>POST /images/generations</code> · <code>POST /images/edits</code>
                  <p>{mediaApiCopyButton("image")} · {" "}
                    <button type="button" className="inline-text-link" onClick={onCopyImageCommand}>
                      {copiedField === "guide-image-command" ? t("common.copied") : t("access.guide.copyImageCommand")}
                    </button>
                  </p>
                </dd>
                <dt>{t("access.guide.mediaVideos")}</dt>
                <dd>
                  <code>POST /videos</code> → <code>{"GET /videos/{id}"}</code>
                  <p>{mediaApiCopyButton("video")}</p>
                </dd>
                <dt>{t("access.guide.mediaSpeech")}</dt>
                <dd>
                  <code>POST /audio/speech</code>
                  <p>{mediaApiCopyButton("speech")}</p>
                </dd>
              </dl>
            </details>
          </div>
        </details>
      </div>
    </DialogBackdrop>
  );
}

function GuideModelList({
  availableModels, modelStatus, copiedField, onCopy, onRefreshModels,
}: GuideModelListProps) {
  const { t } = useTranslation();
  const [copiedModelId, setCopiedModelId] = useState<string | null>(null);
  const modelsLoading = modelStatus === "idle" || modelStatus === "loading";
  const models = useMemo(() => [...new Set(availableModels.map(publicModelId).filter(Boolean))], [availableModels]);

  return (
    <>
      <div className="guide-models-header">
        <small>{t("access.guide.modelsHint")}</small>
        <div className="guide-model-actions">
          <button
            type="button"
            className="quiet"
            onClick={() => void onRefreshModels({ force: true })}
            disabled={modelsLoading}
            title={modelsLoading ? t("common.refreshing") : t("common.refresh")}
            aria-label={modelsLoading ? t("common.refreshing") : t("common.refresh")}
          >
            <RefreshCw
              className={modelsLoading ? "spin-icon" : undefined}
              size={14}
              aria-hidden="true"
            />
            {t("common.refresh")}
          </button>
          <button
            type="button"
            className={`quiet${copiedField === "guide-models" ? " copied" : ""}`}
            onClick={() => onCopy("guide-models", t("access.guide.modelsTitle"), models.join("\n"))}
            disabled={modelStatus !== "ready" || models.length === 0}
            title={copiedField === "guide-models"
              ? t("common.copied")
              : t("access.guide.copyAllModels")}
          >
            {copiedField === "guide-models" ? <Check size={14} /> : <Copy size={14} />}
            {copiedField === "guide-models" ? t("common.copied") : t("access.guide.copyAll")}
          </button>
        </div>
      </div>

      <div
        className="guide-models-content"
        aria-busy={modelsLoading}
        aria-live="polite"
      >
        {modelsLoading && (
          <div className="guide-model-state">
            <LoaderCircle className="spin-icon" size={16} aria-hidden="true" />
            <span>{t("access.guide.modelsLoading")}</span>
          </div>
        )}
        {modelStatus === "error" && (
          <div className="guide-model-state error">
            <span>{t("access.guide.modelsError")}</span>
          </div>
        )}
        {modelStatus === "ready" && models.length === 0 && (
          <div className="guide-model-state">
            <span>{t("access.guide.modelsEmpty")}</span>
          </div>
        )}
        {modelStatus === "ready" && models.length > 0 && (
          <>
            <div className="guide-model-count">
              {t("access.guide.modelsAvailable", { count: models.length })}
            </div>
            <div className="guide-model-list" aria-label={t("access.guide.modelListAria")}>
              {models.map((model) => {
                const modelCopied = copiedField === "guide-model-id" && copiedModelId === model;
                return (
                  <button
                    type="button"
                    className={`guide-model-item${modelCopied ? " copied" : ""}`}
                    key={model}
                    onClick={() => {
                      setCopiedModelId(model);
                      onCopy("guide-model-id", model, model);
                    }}
                    title={modelCopied
                      ? t("common.copied")
                      : t("access.guide.copyModel", { model })}
                    aria-label={modelCopied
                      ? t("access.guide.modelCopied", { model })
                      : t("access.guide.copyModel", { model })}
                  >
                    <code>{model}</code>
                    {modelCopied ? <Check size={13} /> : <Copy size={13} />}
                  </button>
                );
              })}
            </div>
          </>
        )}
      </div>
    </>
  );
}
