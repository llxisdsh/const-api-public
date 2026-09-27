import { useTranslation } from "react-i18next";

import {
  API_SURFACE_CONTRACT,
  type APIExecutionContext,
  type APIOfficialFamilyContract,
  type APISurfaceOperationContract,
} from "../../../shared/apiSurfaceContract";
import {
  API_SURFACE_LABELS,
  apiCapabilityName,
  apiCoverageExplanation,
  apiProductSupport,
  apiProductSupportLabel,
  type APIProductSupport,
} from "../../../shared/apiCapabilityPresentation";
import type {
  SourceDriver,
  SubscriptionOperationCapability,
} from "../sourceDrivers";

const operationByID = new Map(
  API_SURFACE_CONTRACT.operations.map((operation) => [operation.id, operation]),
);

type VisibleCapability = {
  readonly family: APIOfficialFamilyContract;
  readonly support: Exclude<APIProductSupport, "unsupported">;
  readonly explanation?: string;
};

function operationCapability(
  driver: SourceDriver,
  operation: APISurfaceOperationContract,
): SubscriptionOperationCapability | undefined {
  const contract = driver.subscriptionContract;
  if (!contract) return undefined;
  if (operation.protocol && !contract.acceptedIngressProtocols.includes(operation.protocol)) {
    return undefined;
  }
  if ([
    "openai.chat_completions",
    "openai.responses",
    "anthropic.messages",
    "gemini.generate_content",
    "gemini.stream_generate_content",
  ].includes(operation.id)) {
    return contract.generation;
  }
  const operationName = operation.id.slice(operation.id.indexOf(".") + 1);
  const storageSurface = operation.surface === "openai" ? "open_ai" : operation.surface;
  return contract.specialOperations.find((special) =>
    special.surface === storageSurface && special.operation === operationName)?.capability;
}

function familyContextForDriver(driver: SourceDriver): APIExecutionContext {
  return driver.category === "subscription" ? "subscription_driver" : "api_credential";
}

function visibleFamilyCapability(
  driver: SourceDriver,
  family: APIOfficialFamilyContract,
  language: string,
): VisibleCapability | undefined {
  if (family.scope !== "model_api") return undefined;

  const contextStatus = family.contexts[familyContextForDriver(driver)];
  let support = apiProductSupport(contextStatus);
  if (support === "unsupported") return undefined;

  if (driver.subscriptionContract) {
    const operations = family.typed_operation_ids
      .map((operationID) => operationByID.get(operationID))
      .filter((operation): operation is APISurfaceOperationContract => Boolean(operation));
    const enabledOperations = operations.filter((operation) =>
      operation.ownership === "aggregated_control" ||
      operationCapability(driver, operation)?.defaultEnabled === true);

    if (enabledOperations.length === 0) return undefined;
    if (enabledOperations.length < operations.length) {
      support = "limited";
      return {
        family,
        support,
        explanation: language.toLowerCase().startsWith("zh")
          ? "支持核心调用，部分扩展操作暂不可用"
          : "Core requests are supported; some extended operations are unavailable",
      };
    }
  } else if (driver.category !== "official_api" && support === "supported") {
    support = "limited";
    return {
      family,
      support,
      explanation: language.toLowerCase().startsWith("zh")
        ? "实际范围取决于当前模型和上游渠道"
        : "Availability depends on the selected model and upstream",
    };
  }

  return {
    family,
    support,
    explanation: support === "limited"
      ? apiCoverageExplanation(contextStatus, language)
      : undefined,
  };
}

export function OperationCapabilityMatrix({ driver }: { readonly driver: SourceDriver }) {
  const { i18n, t } = useTranslation();
  const language = i18n.resolvedLanguage ?? i18n.language ?? "zh-CN";
  const surfaceIDs = new Set(driver.surfaceBindings.map((binding) =>
    binding.surface === "open_ai" ? "openai" : binding.surface));
  const capabilities = API_SURFACE_CONTRACT.official_families
    .filter((family) => surfaceIDs.has(family.surface))
    .map((family) => visibleFamilyCapability(driver, family, language))
    .filter((capability): capability is VisibleCapability => Boolean(capability));

  if (capabilities.length === 0) {
    return <p className="capability-empty">{t("supplier.capabilities.empty")}</p>;
  }

  return (
    <section className="operation-capability-matrix capability-overview-section">
      <header className="capability-section-heading">
        <h4>{t("supplier.capabilities.apiTitle")}</h4>
        <p>{t("supplier.capabilities.apiHint")}</p>
      </header>
      {[...surfaceIDs].map((surfaceID) => {
        const surfaceCapabilities = capabilities.filter(({ family }) => family.surface === surfaceID);
        if (surfaceCapabilities.length === 0) return null;
        return (
          <section className="capability-summary-group" key={surfaceID}>
            <h5>{API_SURFACE_LABELS[surfaceID]}</h5>
            <div className="capability-summary-list">
              {surfaceCapabilities.map(({ family, support, explanation }) => (
                <div className="capability-summary-row" key={family.id}>
                  <div>
                    <strong>{apiCapabilityName(family, language)}</strong>
                    {explanation && <small>{explanation}</small>}
                  </div>
                  <span className={`capability-support-label ${support}`}>
                    {apiProductSupportLabel(support, language)}
                  </span>
                </div>
              ))}
            </div>
          </section>
        );
      })}
    </section>
  );
}
