import { useState } from "react";
import { Check, Copy, ExternalLink, LoaderCircle, RefreshCw } from "lucide-react";
import { useTranslation } from "react-i18next";

import type { CopyField, ModelCompatibilityPolicyState } from "../appTypes";
import { DialogBackdrop } from "../DialogBackdrop";
import { ModalCloseButton } from "../ModalCloseButton";

type ConnectionGuideModalProps = {
  rootUrl: string;
  apiKey: string;
  openaiBaseUrl: string;
  openaiChatUrl: string;
  anthropicBaseUrl: string;
  geminiBaseUrl: string;
  availableModels: string[];
  modelStatus: ModelCompatibilityPolicyState["status"];
  copiedField: CopyField | null;
  onCopy: (field: CopyField, label: string, value: string) => void;
  onRefreshModels: () => void | Promise<void>;
  onOpenRoot: (url: string) => void;
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
  onClose,
}: ConnectionGuideModalProps) {
  const { t } = useTranslation();
  const [copiedModelId, setCopiedModelId] = useState<string | null>(null);
  const modelListText = availableModels.join("\n");
  const modelsLoading = modelStatus === "idle" || modelStatus === "loading";
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

        <section className="guide-surface-section">
          <div className="guide-section-heading">
            <h3>{t("access.guide.protocolsTitle")}</h3>
            <p>{t("access.guide.protocolsHint")}</p>
          </div>
          <div className="guide-surface-list">
            <div className="guide-surface-row">
              <div className="guide-surface-copy">
                <strong>{t("access.localApiKey")}</strong>
                <small>{t("access.guide.apiKeyHint")}</small>
              </div>
              <code>{apiKey || t("access.notSet")}</code>
              <button
                type="button"
                className={`copy-feedback-button${copiedField === "api-key" ? " copied" : ""}`}
                disabled={!apiKey}
                onClick={() => onCopy("api-key", t("access.localApiKey"), apiKey)}
                title={copiedField === "api-key" ? t("common.copied") : t("access.copyApiKey")}
                aria-label={copiedField === "api-key" ? t("common.copied") : t("access.copyApiKey")}
              >
                {copiedField === "api-key" ? <Check size={15} /> : <Copy size={15} />}
              </button>
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
                  const copied = copiedField === address.field;
                  const copyLabel = copied ? t("common.copied") : t("access.copyConnectionValue", { value: label });
                  return (
                    <div className="guide-surface-row" key={address.field}>
                      <span>{address.label}</span>
                      <code title={address.url}>{address.url || t("access.notSet")}</code>
                      <button
                        type="button"
                        className={`copy-feedback-button${copied ? " copied" : ""}`}
                        disabled={!address.url}
                        onClick={() => onCopy(address.field, label, address.url)}
                        title={copyLabel}
                        aria-label={copyLabel}
                      >
                        {copied ? <Check size={15} /> : <Copy size={15} />}
                      </button>
                    </div>
                  );
                })}
              </div>
            </section>
          ))}

          <div className="guide-models">
            <div className="guide-models-header">
              <div>
                <strong>{t("access.guide.modelsTitle")}</strong>
                <small>{t("access.guide.modelsHint")}</small>
              </div>
              <div className="guide-model-actions">
                <button
                  type="button"
                  className="quiet"
                  onClick={() => void onRefreshModels()}
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
                  onClick={() => onCopy("guide-models", t("access.guide.modelsTitle"), modelListText)}
                  disabled={availableModels.length === 0}
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
              {modelStatus === "ready" && availableModels.length === 0 && (
                <div className="guide-model-state">
                  <span>{t("access.guide.modelsEmpty")}</span>
                </div>
              )}
              {modelStatus === "ready" && availableModels.length > 0 && (
                <>
                  <div className="guide-model-count">
                    {t("access.guide.modelsAvailable", { count: availableModels.length })}
                  </div>
                  <div className="guide-model-list" aria-label={t("access.guide.modelListAria")}>
                    {availableModels.map((model) => {
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
          </div>
        </section>

        <section className="guide-steps">
          <h3>{t("access.guide.stepsTitle")}</h3>
          <ol>
            <li>{t("access.guide.stepChooseSurface")}</li>
            <li>{t("access.guide.stepConfigure")}</li>
            <li>{t("access.guide.stepRequest")}</li>
          </ol>
          <p className="guide-auth-note">{t("access.guide.authNote")}</p>
        </section>
      </div>
    </DialogBackdrop>
  );
}
