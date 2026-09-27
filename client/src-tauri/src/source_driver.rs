use crate::{
    model::{ChannelConfig, ChannelExecutorLocator, ChannelTarget, DiscoveryProfile},
    protocol::kind::ProtocolKind,
    surface::{ApiOperation, ApiSurface},
};
use anyhow::{Context, Result, anyhow};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, sync::OnceLock};

const SOURCE_DRIVER_MANIFEST_JSON: &str = include_str!("../../source-drivers.json");
static SOURCE_DRIVER_MANIFEST: OnceLock<Result<SourceDriverManifest, String>> = OnceLock::new();

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SourceDriverId {
    #[serde(rename = "openai_api")]
    OpenAiApi,
    AnthropicApi,
    GeminiApi,
    XaiApi,
    MistralApi,
    DeepseekApi,
    DashscopeApi,
    MoonshotApi,
    ZhipuApi,
    MinimaxApi,
    StepfunApi,
    #[serde(rename = "azure_openai")]
    AzureOpenAi,
    BedrockMantle,
    GroqApi,
    TogetherApi,
    FireworksApi,
    PerplexityApi,
    HuggingfaceApi,
    NvidiaApi,
    SiliconflowApi,
    VolcengineArkApi,
    BaiduQianfanApi,
    TencentHunyuanApi,
    Openrouter,
    OpencodeGo,
    OpencodeZen,
    KiloGateway,
    ClineApi,
    CommandCode,
    KimiCode,
    GlmCodingPlan,
    MinimaxTokenPlan,
    OllamaCloud,
    Omniroute,
    Ollama,
    LmStudio,
    Vllm,
    LanShare,
    CustomEndpoint,
    #[serde(rename = "openai_subscription")]
    OpenAiSubscription,
    ClaudeSubscription,
    // Persisted compatibility key; this driver now implements Antigravity.
    GeminiSubscription,
    GrokSubscription,
}

pub(crate) const ALL_SOURCE_DRIVER_IDS: [SourceDriverId; 43] = [
    SourceDriverId::OpenAiApi,
    SourceDriverId::AnthropicApi,
    SourceDriverId::GeminiApi,
    SourceDriverId::XaiApi,
    SourceDriverId::MistralApi,
    SourceDriverId::DeepseekApi,
    SourceDriverId::DashscopeApi,
    SourceDriverId::MoonshotApi,
    SourceDriverId::ZhipuApi,
    SourceDriverId::MinimaxApi,
    SourceDriverId::StepfunApi,
    SourceDriverId::AzureOpenAi,
    SourceDriverId::BedrockMantle,
    SourceDriverId::GroqApi,
    SourceDriverId::TogetherApi,
    SourceDriverId::FireworksApi,
    SourceDriverId::PerplexityApi,
    SourceDriverId::HuggingfaceApi,
    SourceDriverId::NvidiaApi,
    SourceDriverId::SiliconflowApi,
    SourceDriverId::VolcengineArkApi,
    SourceDriverId::BaiduQianfanApi,
    SourceDriverId::TencentHunyuanApi,
    SourceDriverId::Openrouter,
    SourceDriverId::OpencodeGo,
    SourceDriverId::OpencodeZen,
    SourceDriverId::KiloGateway,
    SourceDriverId::ClineApi,
    SourceDriverId::CommandCode,
    SourceDriverId::KimiCode,
    SourceDriverId::GlmCodingPlan,
    SourceDriverId::MinimaxTokenPlan,
    SourceDriverId::OllamaCloud,
    SourceDriverId::Omniroute,
    SourceDriverId::Ollama,
    SourceDriverId::LmStudio,
    SourceDriverId::Vllm,
    SourceDriverId::LanShare,
    SourceDriverId::CustomEndpoint,
    SourceDriverId::OpenAiSubscription,
    SourceDriverId::ClaudeSubscription,
    SourceDriverId::GeminiSubscription,
    SourceDriverId::GrokSubscription,
];

pub(crate) fn channel_is_lan_share(channel: &ChannelConfig) -> bool {
    channel.source_driver() == SourceDriverId::LanShare
}

pub(crate) fn channel_is_platform_shareable(_: &ChannelConfig) -> bool { false }

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExecutionKind {
    HttpSurface,
    OpenAiSubscription,
    ClaudeSubscription,
    // Serialized as `gemini_subscription` to keep existing channel configs readable.
    GeminiSubscription,
    GrokSubscription,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct SourceDriverManifest {
    pub(crate) version: u32,
    pub(crate) drivers: Vec<SourceDriver>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct SourceDriver {
    pub(crate) id: SourceDriverId,
    pub(crate) category: String,
    pub(crate) order: u32,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) icon_key: String,
    pub(crate) action_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) homepage_url: Option<String>,
    #[serde(default)]
    pub(crate) advanced: bool,
    #[serde(default)]
    pub(crate) fixed_connection: bool,
    pub(crate) execution_kind: ExecutionKind,
    pub(crate) executor: ChannelExecutorLocator,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) subscription_contract: Option<SubscriptionDriverContract>,
    pub(crate) kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) subscription_provider: Option<String>,
    pub(crate) protocols: Vec<SourceDriverProtocol>,
    pub(crate) surfaces: Vec<SourceDriverSurface>,
    pub(crate) default_target: ChannelTarget,
    pub(crate) discovery: DiscoveryProfile,
    pub(crate) creation: CreationVerificationPolicy,
    pub(crate) periodic: PeriodicDetectionPolicy,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct SourceDriverProtocol {
    pub(crate) protocol: String,
    #[serde(default)]
    pub(crate) preferred: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct SourceDriverSurface {
    pub(crate) surface: ApiSurface,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) base_url: Option<String>,
    pub(crate) endpoint_profile: String,
    pub(crate) auth_scheme: String,
    pub(crate) protocols: Vec<String>,
    pub(crate) preferred_protocol: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct SubscriptionDriverContract {
    pub(crate) version: u32,
    pub(crate) accepted_ingress_protocols: Vec<ProtocolKind>,
    pub(crate) generation: SubscriptionOperationCapability,
    #[serde(default)]
    pub(crate) special_operations: Vec<SubscriptionSpecialOperation>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct SubscriptionSpecialOperation {
    pub(crate) surface: ApiSurface,
    pub(crate) operation: ApiOperation,
    pub(crate) capability: SubscriptionOperationCapability,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct SubscriptionOperationCapability {
    pub(crate) modes: Vec<SubscriptionResponseMode>,
    pub(crate) conversion: SubscriptionConversionContract,
    pub(crate) usage_confidence: UsageConfidencePolicy,
    pub(crate) evidence: DriverEvidenceLevel,
    pub(crate) default_enabled: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SubscriptionResponseMode {
    Buffered,
    Sse,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConversionLevel {
    C0,
    C1,
    C2,
    C3,
}

impl ConversionLevel {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::C0 => "c0",
            Self::C1 => "c1",
            Self::C2 => "c2",
            Self::C3 => "c3",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct SubscriptionConversionContract {
    pub(crate) request: ConversionLevel,
    pub(crate) response: ConversionLevel,
    pub(crate) stream: ConversionLevel,
    pub(crate) error: ConversionLevel,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UsageConfidencePolicy {
    Reported,
    ReportedOrUnknown,
    Derived,
    Unknown,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DriverEvidenceLevel {
    Declared,
    ComponentVerified,
    LiveVerified,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct CreationVerificationPolicy {
    pub(crate) trigger: CreationVerificationTrigger,
    pub(crate) basic_verification: BasicVerificationPolicy,
    pub(crate) advanced_conversational: AdvancedVerificationPolicy,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CreationVerificationTrigger {
    Guided,
    OnAdd,
    OnSave,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BasicVerificationPolicy {
    SurfacePreferred,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AdvancedVerificationPolicy {
    None,
    DriverPreferredOnly,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct PeriodicDetectionPolicy {
    pub(crate) catalog: bool,
    pub(crate) quota: bool,
    pub(crate) metadata: bool,
    pub(crate) conversational_probe: bool,
}

impl SourceDriverManifest {
    pub(crate) fn driver(&self, id: SourceDriverId) -> Option<&SourceDriver> {
        self.drivers.iter().find(|driver| driver.id == id)
    }
}

impl SourceDriver {
    pub(crate) fn protocols(&self) -> Vec<&str> {
        self.protocols
            .iter()
            .map(|binding| binding.protocol.as_str())
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn preferred_protocol(&self) -> Option<&str> {
        self.protocols
            .iter()
            .find(|binding| binding.preferred)
            .map(|binding| binding.protocol.as_str())
    }

    pub(crate) fn execution_kind(&self) -> ExecutionKind {
        self.execution_kind
    }

    pub(crate) fn subscription_contract(&self) -> Option<&SubscriptionDriverContract> {
        self.subscription_contract.as_ref()
    }
}

pub(crate) fn source_driver_manifest() -> Result<&'static SourceDriverManifest> {
    SOURCE_DRIVER_MANIFEST
        .get_or_init(|| {
            parse_source_driver_manifest(SOURCE_DRIVER_MANIFEST_JSON).map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|message| anyhow!(message.clone()))
}

pub(crate) fn source_driver(id: SourceDriverId) -> Result<&'static SourceDriver> {
    source_driver_manifest()?
        .driver(id)
        .ok_or_else(|| anyhow!("source driver {id:?} is missing from manifest"))
}

pub(crate) fn subscription_operation_capability(
    source_driver_id: SourceDriverId,
    surface: ApiSurface,
    operation: ApiOperation,
    protocol: Option<ProtocolKind>,
) -> Result<&'static SubscriptionOperationCapability> {
    let driver = source_driver(source_driver_id)?;
    let contract = driver.subscription_contract().ok_or_else(|| {
        anyhow!("source driver {source_driver_id:?} is not a retained subscription driver")
    })?;
    if let Some(special) = contract
        .special_operations
        .iter()
        .find(|special| special.surface == surface && special.operation == operation)
    {
        return Ok(&special.capability);
    }
    if !is_generation_operation(operation) {
        return Err(anyhow!(
            "subscription driver {source_driver_id:?} does not declare {}.{}",
            surface.as_str(),
            operation.as_str()
        ));
    }
    let protocol = protocol.ok_or_else(|| {
        anyhow!(
            "subscription generation operation {}.{} has no ingress protocol",
            surface.as_str(),
            operation.as_str()
        )
    })?;
    if !contract.accepted_ingress_protocols.contains(&protocol) {
        return Err(anyhow!(
            "subscription driver {source_driver_id:?} does not accept ingress protocol {protocol}"
        ));
    }
    Ok(&contract.generation)
}

pub(crate) fn admit_subscription_operation(
    source_driver_id: SourceDriverId,
    surface: ApiSurface,
    operation: ApiOperation,
    protocol: Option<ProtocolKind>,
    mode: SubscriptionResponseMode,
) -> Result<&'static SubscriptionOperationCapability> {
    let capability =
        subscription_operation_capability(source_driver_id, surface, operation, protocol)?;
    if !capability.default_enabled {
        return Err(anyhow!(
            "subscription operation {}.{} is disabled because its conversion is {}",
            surface.as_str(),
            operation.as_str(),
            capability.conversion.request.as_str()
        ));
    }
    if !capability.modes.contains(&mode) {
        return Err(anyhow!(
            "subscription operation {}.{} does not support {mode:?}",
            surface.as_str(),
            operation.as_str()
        ));
    }
    let compatible = capability.conversion.request <= ConversionLevel::C1
        && capability.conversion.response <= ConversionLevel::C1
        && capability.conversion.error <= ConversionLevel::C1
        && (mode != SubscriptionResponseMode::Sse
            || capability.conversion.stream <= ConversionLevel::C1);
    if !compatible {
        return Err(anyhow!(
            "subscription operation {}.{} is not C0/C1 compatible",
            surface.as_str(),
            operation.as_str()
        ));
    }
    Ok(capability)
}

pub(crate) fn parse_source_driver_manifest(raw: &str) -> Result<SourceDriverManifest> {
    let manifest: SourceDriverManifest =
        serde_json::from_str(raw).context("parse source driver manifest JSON")?;
    validate_source_driver_manifest(&manifest)?;
    Ok(manifest)
}

fn validate_source_driver_manifest(manifest: &SourceDriverManifest) -> Result<()> {
    if manifest.version == 0 {
        return Err(anyhow!("source driver manifest version must be positive"));
    }
    let mut ids = HashSet::new();
    for driver in &manifest.drivers {
        if !ids.insert(driver.id) {
            return Err(anyhow!("duplicate source driver id: {:?}", driver.id));
        }
        if driver.kind.trim().is_empty() {
            return Err(anyhow!("source driver {:?} has an empty kind", driver.id));
        }
        if let Some(homepage_url) = driver.homepage_url.as_deref() {
            validate_manifest_url(driver.id, homepage_url)?;
        }
        if !matches!(
            driver.category.as_str(),
            "subscription"
                | "official_api"
                | "cloud_platform"
                | "local_runtime"
                | "compatible_gateway"
        ) || driver.order == 0
            || driver.title.trim().is_empty()
            || driver.description.trim().is_empty()
            || driver.icon_key.trim().is_empty()
            || driver.action_label.trim().is_empty()
        {
            return Err(anyhow!(
                "source driver {:?} has invalid frontend metadata",
                driver.id
            ));
        }
        if driver.periodic.conversational_probe {
            return Err(anyhow!(
                "source driver {:?} enables a periodic conversational probe",
                driver.id
            ));
        }
        let expected_execution = match driver.id {
            SourceDriverId::OpenAiSubscription => ExecutionKind::OpenAiSubscription,
            SourceDriverId::ClaudeSubscription => ExecutionKind::ClaudeSubscription,
            SourceDriverId::GeminiSubscription => ExecutionKind::GeminiSubscription,
            SourceDriverId::GrokSubscription => ExecutionKind::GrokSubscription,
            _ => ExecutionKind::HttpSurface,
        };
        if driver.execution_kind != expected_execution {
            return Err(anyhow!(
                "source driver {:?} has inconsistent execution kind",
                driver.id
            ));
        }
        validate_executor_locator(driver)?;
        validate_subscription_driver_contract(driver)?;
        if driver.discovery.strategy.trim().is_empty()
            || (!driver.discovery.catalog && !driver.discovery.quota && !driver.discovery.metadata)
        {
            return Err(anyhow!(
                "source driver {:?} has invalid discovery policy",
                driver.id
            ));
        }
        if !driver.periodic.catalog && !driver.periodic.quota && !driver.periodic.metadata {
            return Err(anyhow!(
                "source driver {:?} has no periodic metadata policy",
                driver.id
            ));
        }

        let mut protocols = HashSet::new();
        let mut preferred_count = 0;
        for binding in &driver.protocols {
            let protocol = ProtocolKind::parse(&binding.protocol)
                .with_context(|| format!("source driver {:?} has invalid protocol", driver.id))?;
            if !protocols.insert(protocol.as_str().to_string()) {
                return Err(anyhow!(
                    "source driver {:?} declares protocol {} more than once",
                    driver.id,
                    protocol
                ));
            }
            preferred_count += usize::from(binding.preferred);
        }
        if preferred_count != 1 {
            return Err(anyhow!(
                "source driver {:?} must declare exactly one preferred protocol",
                driver.id
            ));
        }

        let mut surfaces = HashSet::new();
        if driver.surfaces.is_empty() {
            return Err(anyhow!(
                "source driver {:?} must declare at least one surface",
                driver.id
            ));
        }
        for surface in &driver.surfaces {
            if !surfaces.insert(surface.surface) {
                return Err(anyhow!(
                    "source driver {:?} declares surface {} more than once",
                    driver.id,
                    surface.surface.as_str()
                ));
            }
            if surface.endpoint_profile.trim().is_empty() || surface.auth_scheme.trim().is_empty() {
                return Err(anyhow!(
                    "source driver {:?} has an incomplete endpoint policy for {}",
                    driver.id,
                    surface.surface.as_str()
                ));
            }
            match driver.execution_kind {
                ExecutionKind::HttpSurface => {
                    let base_url = surface.base_url.as_deref().ok_or_else(|| {
                        anyhow!(
                            "source driver {:?} HTTP surface {} is missing a base URL",
                            driver.id,
                            surface.surface.as_str()
                        )
                    })?;
                    validate_manifest_url(driver.id, base_url)?;
                }
                _ if surface.base_url.is_some() => {
                    return Err(anyhow!(
                        "source driver {:?} subscription surface cannot declare a base URL",
                        driver.id
                    ));
                }
                _ => {}
            }
            if surface.protocols.is_empty() {
                return Err(anyhow!(
                    "source driver {:?} surface {} has no protocols",
                    driver.id,
                    surface.surface.as_str()
                ));
            }
            let mut surface_protocols = HashSet::new();
            for protocol in &surface.protocols {
                let parsed = ProtocolKind::parse(protocol).with_context(|| {
                    format!(
                        "source driver {:?} surface {} has invalid protocol",
                        driver.id,
                        surface.surface.as_str()
                    )
                })?;
                let value = parsed.as_str();
                if surface_for_protocol(parsed) != surface.surface
                    || !protocols.contains(value)
                    || !surface_protocols.insert(value)
                {
                    return Err(anyhow!(
                        "source driver {:?} surface {} has an undeclared or duplicate protocol {}",
                        driver.id,
                        surface.surface.as_str(),
                        value
                    ));
                }
            }
            let preferred = ProtocolKind::parse(&surface.preferred_protocol)?;
            if !surface_protocols.contains(preferred.as_str()) {
                return Err(anyhow!(
                    "source driver {:?} surface {} preference is not declared",
                    driver.id,
                    surface.surface.as_str()
                ));
            }
        }
        if !driver.surfaces.iter().any(|surface| {
            surface.surface == driver.default_target.surface
                && surface
                    .protocols
                    .iter()
                    .any(|protocol| protocol == driver.default_target.protocol.as_str())
        }) {
            return Err(anyhow!(
                "source driver {:?} has a dangling default target",
                driver.id
            ));
        }
    }
    if ids.len() != ALL_SOURCE_DRIVER_IDS.len()
        || ALL_SOURCE_DRIVER_IDS.iter().any(|id| !ids.contains(id))
    {
        return Err(anyhow!(
            "source driver manifest must contain every SourceDriverId exactly once"
        ));
    }
    Ok(())
}

fn validate_executor_locator(driver: &SourceDriver) -> Result<()> {
    match (driver.execution_kind, &driver.executor) {
        (ExecutionKind::HttpSurface, ChannelExecutorLocator::HttpSurface)
            if driver.subscription_provider.is_none() =>
        {
            Ok(())
        }
        (
            ExecutionKind::OpenAiSubscription
            | ExecutionKind::ClaudeSubscription
            | ExecutionKind::GeminiSubscription
            | ExecutionKind::GrokSubscription,
            ChannelExecutorLocator::RetainedSubscription { provider },
        ) if driver.subscription_provider.as_deref() == Some(provider.as_str()) => Ok(()),
        _ => Err(anyhow!(
            "source driver {:?} has an executor locator inconsistent with its execution kind",
            driver.id
        )),
    }
}

fn validate_subscription_driver_contract(driver: &SourceDriver) -> Result<()> {
    let retained = driver.execution_kind != ExecutionKind::HttpSurface;
    let Some(contract) = driver.subscription_contract.as_ref() else {
        return if retained {
            Err(anyhow!(
                "source driver {:?} is missing its subscription operation contract",
                driver.id
            ))
        } else {
            Ok(())
        };
    };
    if !retained {
        return Err(anyhow!(
            "HTTP source driver {:?} cannot declare a subscription operation contract",
            driver.id
        ));
    }
    if contract.version == 0 || contract.accepted_ingress_protocols.is_empty() {
        return Err(anyhow!(
            "source driver {:?} has an invalid subscription contract version or ingress protocols",
            driver.id
        ));
    }
    let accepted = contract
        .accepted_ingress_protocols
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    if accepted.len() != contract.accepted_ingress_protocols.len() {
        return Err(anyhow!(
            "source driver {:?} repeats a subscription ingress protocol",
            driver.id
        ));
    }
    for native in &driver.protocols {
        let native = ProtocolKind::parse(&native.protocol)?;
        if !accepted.contains(&native) {
            return Err(anyhow!(
                "source driver {:?} subscription contract omits native protocol {native}",
                driver.id
            ));
        }
    }
    validate_subscription_capability(driver.id, "generation", &contract.generation)?;
    if !contract.generation.default_enabled
        || !contract
            .generation
            .modes
            .contains(&SubscriptionResponseMode::Buffered)
        || !contract
            .generation
            .modes
            .contains(&SubscriptionResponseMode::Sse)
    {
        return Err(anyhow!(
            "source driver {:?} must enable buffered and SSE generation",
            driver.id
        ));
    }
    let mut special_operations = HashSet::new();
    for special in &contract.special_operations {
        if !operation_belongs_to_surface(special.surface, special.operation)
            || !special_operations.insert((special.surface, special.operation))
        {
            return Err(anyhow!(
                "source driver {:?} has an invalid or duplicate subscription special operation {}.{}",
                driver.id,
                special.surface.as_str(),
                special.operation.as_str()
            ));
        }
        validate_subscription_capability(
            driver.id,
            &format!(
                "{}.{}",
                special.surface.as_str(),
                special.operation.as_str()
            ),
            &special.capability,
        )?;
    }
    Ok(())
}

fn validate_subscription_capability(
    driver: SourceDriverId,
    label: &str,
    capability: &SubscriptionOperationCapability,
) -> Result<()> {
    let modes = capability.modes.iter().copied().collect::<HashSet<_>>();
    if modes.is_empty() || modes.len() != capability.modes.len() {
        return Err(anyhow!(
            "source driver {driver:?} capability {label} has invalid response modes"
        ));
    }
    let has_stream = modes.contains(&SubscriptionResponseMode::Sse);
    if has_stream == (capability.conversion.stream == ConversionLevel::C3) {
        return Err(anyhow!(
            "source driver {driver:?} capability {label} has inconsistent stream conversion"
        ));
    }
    if capability.conversion.request == ConversionLevel::C3
        || capability.conversion.response == ConversionLevel::C3
        || capability.conversion.error == ConversionLevel::C3
    {
        return Err(anyhow!(
            "source driver {driver:?} capability {label} declares an unusable conversion"
        ));
    }
    if capability.default_enabled
        && (capability.conversion.request > ConversionLevel::C1
            || capability.conversion.response > ConversionLevel::C1
            || capability.conversion.error > ConversionLevel::C1
            || (has_stream && capability.conversion.stream > ConversionLevel::C1))
    {
        return Err(anyhow!(
            "source driver {driver:?} capability {label} enables a C2/C3 conversion by default"
        ));
    }
    Ok(())
}

fn is_generation_operation(operation: ApiOperation) -> bool {
    matches!(
        operation,
        ApiOperation::ChatCompletions
            | ApiOperation::Responses
            | ApiOperation::Messages
            | ApiOperation::GenerateContent
            | ApiOperation::StreamGenerateContent
    )
}

fn operation_belongs_to_surface(surface: ApiSurface, operation: ApiOperation) -> bool {
    match surface {
        ApiSurface::OpenAi => matches!(
            operation,
            ApiOperation::ListModels
                | ApiOperation::GetModel
                | ApiOperation::ChatCompletions
                | ApiOperation::Responses
                | ApiOperation::ResponsesGet
                | ApiOperation::ResponsesDelete
                | ApiOperation::ResponsesCancel
                | ApiOperation::ResponsesInputItems
                | ApiOperation::ResponsesCompact
                | ApiOperation::RealtimeCallsCreate
                | ApiOperation::RealtimeLiveCallCreate
                | ApiOperation::RealtimeLiveWebSocket
                | ApiOperation::RealtimeLiveConnect
                | ApiOperation::Embeddings
                | ApiOperation::ContentProvenanceChecks
        ),
        ApiSurface::Anthropic => matches!(
            operation,
            ApiOperation::ListModels
                | ApiOperation::GetModel
                | ApiOperation::Messages
                | ApiOperation::CountTokens
        ),
        ApiSurface::Gemini => matches!(
            operation,
            ApiOperation::ListModels
                | ApiOperation::GetModel
                | ApiOperation::GenerateContent
                | ApiOperation::StreamGenerateContent
                | ApiOperation::CountTokens
                | ApiOperation::EmbedContent
        ),
    }
}

fn surface_for_protocol(protocol: ProtocolKind) -> ApiSurface {
    match protocol {
        ProtocolKind::OpenAiResponses | ProtocolKind::OpenAiChat => ApiSurface::OpenAi,
        ProtocolKind::AnthropicMessages => ApiSurface::Anthropic,
        ProtocolKind::GeminiNative => ApiSurface::Gemini,
    }
}

fn validate_manifest_url(id: SourceDriverId, value: &str) -> Result<()> {
    let url =
        Url::parse(value).with_context(|| format!("source driver {id:?} has invalid base URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(anyhow!("source driver {id:?} has invalid base URL"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_manifest_is_valid_and_complete() {
        let manifest = source_driver_manifest().expect("valid source driver manifest");

        assert_eq!(manifest.drivers.len(), ALL_SOURCE_DRIVER_IDS.len());
        assert_eq!(
            manifest.driver(SourceDriverId::OpenAiApi).unwrap().kind,
            "openai"
        );
        assert_eq!(
            manifest
                .driver(SourceDriverId::BedrockMantle)
                .unwrap()
                .surfaces
                .len(),
            2
        );
    }

    #[test]
    fn deepseek_declares_native_responses_without_changing_the_chat_default() {
        let manifest = source_driver_manifest().expect("valid source driver manifest");
        let deepseek = manifest
            .driver(SourceDriverId::DeepseekApi)
            .expect("DeepSeek API driver");

        assert_eq!(
            deepseek.protocols(),
            vec!["openai_chat", "openai_responses", "anthropic_messages"]
        );
        assert_eq!(deepseek.preferred_protocol(), Some("openai_chat"));

        let openai = deepseek
            .surfaces
            .iter()
            .find(|binding| binding.surface == ApiSurface::OpenAi)
            .expect("DeepSeek OpenAI surface");
        assert_eq!(openai.protocols, vec!["openai_chat", "openai_responses"]);
        assert_eq!(openai.preferred_protocol, "openai_chat");
    }

    #[test]
    fn every_driver_declares_execution_default_target_and_discovery() {
        let value: serde_json::Value =
            serde_json::from_str(include_str!("../../source-drivers.json")).unwrap();
        let drivers = value["drivers"].as_array().unwrap();
        let subscription_execution = [
            ("openai_subscription", "open_ai_subscription"),
            ("claude_subscription", "claude_subscription"),
            ("gemini_subscription", "gemini_subscription"),
            ("grok_subscription", "grok_subscription"),
        ];

        assert_eq!(drivers.len(), ALL_SOURCE_DRIVER_IDS.len());
        for driver in drivers {
            let id = driver["id"].as_str().unwrap();
            let expected_execution = subscription_execution
                .iter()
                .find_map(|(candidate, execution)| (*candidate == id).then_some(*execution))
                .unwrap_or("http_surface");
            assert_eq!(driver["execution_kind"], expected_execution, "driver {id}");
            assert!(driver["discovery"].is_object(), "driver {id}");

            let target = driver["default_target"]
                .as_object()
                .expect("default target");
            let surface = target["surface"].as_str().unwrap();
            let protocol = target["protocol"].as_str().unwrap();
            assert!(
                driver["surfaces"].as_array().is_some_and(|surfaces| {
                    !surfaces.is_empty()
                        && surfaces.iter().any(|binding| {
                            binding["surface"] == surface
                                && binding["protocols"].as_array().is_some_and(|protocols| {
                                    protocols.iter().any(|item| item == protocol)
                                })
                        })
                }),
                "driver {id} has a dangling default target"
            );
        }
    }

    #[test]
    fn shared_manifest_preserves_utf8_chinese_copy() {
        let manifest = source_driver_manifest().expect("valid source driver manifest");

        let subscription = manifest
            .driver(SourceDriverId::OpenAiSubscription)
            .expect("OpenAI subscription driver");
        assert_eq!(subscription.title, "OpenAI 账号订阅");
        assert_eq!(
            subscription.description,
            "连接 ChatGPT 账号，使用账号订阅额度"
        );

        let openrouter = manifest
            .driver(SourceDriverId::Openrouter)
            .expect("OpenRouter driver");
        assert_eq!(
            openrouter.description,
            "支持 OpenAI Responses/Chat 与 Anthropic Messages；高级能力以检测为准"
        );

        let omniroute = manifest
            .driver(SourceDriverId::Omniroute)
            .expect("OmniRoute driver");
        assert_eq!(omniroute.kind, "custom_endpoint");
        assert_eq!(
            omniroute.homepage_url.as_deref(),
            Some("https://github.com/diegosouzapw/OmniRoute")
        );
    }

    #[test]
    fn subscriptions_keep_native_protocol_contracts() {
        let manifest = source_driver_manifest().expect("valid source driver manifest");
        let cases = [
            (
                SourceDriverId::OpenAiSubscription,
                "openai",
                "openai_responses",
            ),
            (
                SourceDriverId::ClaudeSubscription,
                "claude",
                "anthropic_messages",
            ),
            (
                SourceDriverId::GeminiSubscription,
                "antigravity",
                "gemini_native",
            ),
            (SourceDriverId::GrokSubscription, "grok", "openai_responses"),
        ];

        for (id, provider, protocol) in cases {
            let driver = manifest.driver(id).expect("subscription driver");
            assert_eq!(driver.subscription_provider.as_deref(), Some(provider));
            assert_eq!(driver.protocols(), vec![protocol]);
            assert_eq!(driver.preferred_protocol(), Some(protocol));
        }
    }

    #[test]
    fn subscription_contracts_distinguish_equivalent_approximate_and_unsupported_operations() {
        let generation = admit_subscription_operation(
            SourceDriverId::OpenAiSubscription,
            ApiSurface::OpenAi,
            ApiOperation::Responses,
            Some(ProtocolKind::OpenAiResponses),
            SubscriptionResponseMode::Sse,
        )
        .expect("OpenAI subscription Responses stream");
        assert_eq!(generation.conversion.request, ConversionLevel::C1);
        assert_eq!(generation.conversion.response, ConversionLevel::C1);
        assert_eq!(generation.conversion.stream, ConversionLevel::C1);

        let compact = admit_subscription_operation(
            SourceDriverId::OpenAiSubscription,
            ApiSurface::OpenAi,
            ApiOperation::ResponsesCompact,
            Some(ProtocolKind::OpenAiResponses),
            SubscriptionResponseMode::Buffered,
        )
        .expect("OpenAI subscription compact");
        assert_eq!(compact.conversion.stream, ConversionLevel::C3);

        assert!(
            subscription_operation_capability(
                SourceDriverId::OpenAiSubscription,
                ApiSurface::OpenAi,
                ApiOperation::Embeddings,
                None,
            )
            .is_err()
        );

        let approximate = subscription_operation_capability(
            SourceDriverId::GeminiSubscription,
            ApiSurface::Gemini,
            ApiOperation::EmbedContent,
            Some(ProtocolKind::GeminiNative),
        )
        .expect("Antigravity approximate embedContent declaration");
        assert_eq!(approximate.conversion.request, ConversionLevel::C2);
        assert!(!approximate.default_enabled);
        assert!(
            admit_subscription_operation(
                SourceDriverId::GeminiSubscription,
                ApiSurface::Gemini,
                ApiOperation::EmbedContent,
                Some(ProtocolKind::GeminiNative),
                SubscriptionResponseMode::Buffered,
            )
            .is_err()
        );

        admit_subscription_operation(
            SourceDriverId::ClaudeSubscription,
            ApiSurface::Anthropic,
            ApiOperation::CountTokens,
            Some(ProtocolKind::AnthropicMessages),
            SubscriptionResponseMode::Buffered,
        )
        .expect("Claude subscription count_tokens");
        admit_subscription_operation(
            SourceDriverId::GeminiSubscription,
            ApiSurface::Gemini,
            ApiOperation::CountTokens,
            Some(ProtocolKind::GeminiNative),
            SubscriptionResponseMode::Buffered,
        )
        .expect("Antigravity countTokens");
    }

    #[test]
    fn manifest_rejects_default_enabled_approximate_subscription_operations() {
        let raw = include_str!("../../source-drivers.json").replacen(
            r#""default_enabled": false"#,
            r#""default_enabled": true"#,
            1,
        );
        let error = parse_source_driver_manifest(&raw).unwrap_err();
        assert!(error.to_string().contains("C2/C3"));
    }

    #[test]
    fn manifest_requires_operation_contracts_only_on_subscription_drivers() {
        let mut value: serde_json::Value =
            serde_json::from_str(include_str!("../../source-drivers.json")).unwrap();
        value["drivers"][0]
            .as_object_mut()
            .unwrap()
            .remove("subscription_contract");
        let missing = parse_source_driver_manifest(&serde_json::to_string(&value).unwrap())
            .expect_err("subscription driver without operation contract");
        assert!(missing.to_string().contains("missing"));

        let contract = value["drivers"][1]["subscription_contract"].clone();
        value["drivers"][0]["subscription_contract"] = contract;
        let contract = value["drivers"][0]["subscription_contract"].clone();
        let http_driver = value["drivers"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|driver| driver["execution_kind"] == "http_surface")
            .expect("HTTP source driver");
        http_driver["subscription_contract"] = contract;
        let invalid = parse_source_driver_manifest(&serde_json::to_string(&value).unwrap())
            .expect_err("HTTP driver with subscription operation contract");
        assert!(invalid.to_string().contains("HTTP source driver"));
    }

    #[test]
    fn local_driver_ids_are_distinct_serializable_identities() {
        let ids = [
            SourceDriverId::Ollama,
            SourceDriverId::LmStudio,
            SourceDriverId::Vllm,
        ];

        let encoded = ids
            .iter()
            .map(|id| serde_json::to_string(id).unwrap())
            .collect::<Vec<_>>();

        assert_eq!(encoded, ["\"ollama\"", "\"lm_studio\"", "\"vllm\""]);
        assert_eq!(
            serde_json::from_str::<SourceDriverId>("\"lm_studio\"").unwrap(),
            SourceDriverId::LmStudio
        );
    }

    #[test]
    fn manifest_rejects_conversational_periodic_probes() {
        let raw = include_str!("../../source-drivers.json").replace(
            "\"conversational_probe\": false",
            "\"conversational_probe\": true",
        );

        let error = parse_source_driver_manifest(&raw).unwrap_err();
        assert!(error.to_string().contains("periodic conversational probe"));
    }

    #[test]
    fn manifest_rejects_executor_locators_that_conflict_with_execution_kind() {
        let raw = include_str!("../../source-drivers.json").replace(
            r#""type": "retained_subscription", "provider": "openai""#,
            r#""type": "http_surface""#,
        );

        let error = parse_source_driver_manifest(&raw).unwrap_err();
        assert!(error.to_string().contains("executor"));
    }
}
