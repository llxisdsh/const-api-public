use std::fmt;

use serde::{Deserialize, Serialize};

use crate::protocol::capability::{
    CapabilityProfile, Feature, SupportState, extract_required_features,
    feature_uses_provider_evidence, protocol_evidence_extension_supports, protocol_wire_supports,
    support_is_confirmed_unsupported,
};
use crate::protocol::ir::{
    ArtifactKind, CanonicalRequestV2, CanonicalResponseV2, ContentBlock, ExtensionCriticality,
    ProviderExtension,
};
use crate::protocol::kind::ProtocolKind;

use super::{
    ArtifactAction, ConversionIssue, ConversionLevel, ConversionRepair, ConversionReport,
    FeatureAction, FeatureDisposition, IssueAction, IssueSeverity, affinity_permits,
    evaluate_artifacts, evaluate_response_artifacts, missing_gemini_signature_steps,
    reasoning_history_omissions,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct IssuerIdentity {
    pub(crate) provider: String,
    pub(crate) endpoint_fingerprint: String,
    pub(crate) account_fingerprint: Option<String>,
    pub(crate) model_family: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConversionPolicy {
    Production,
    Diagnostic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConversionPlan {
    pub(crate) tool_mapping: super::ToolWireMap,
    target_protocol: ProtocolKind,
    level: ConversionLevel,
    feature_actions: Vec<FeatureAction>,
    artifact_actions: Vec<ArtifactAction>,
    repairs: Vec<ConversionRepair>,
    report: ConversionReport,
    executable: bool,
}

impl ConversionPlan {
    pub(crate) fn native_passthrough(protocol: ProtocolKind) -> Self {
        Self {
            tool_mapping: super::ToolWireMap::default(),
            target_protocol: protocol,
            level: ConversionLevel::Native,
            feature_actions: Vec::new(),
            artifact_actions: Vec::new(),
            repairs: Vec::new(),
            report: ConversionReport {
                source_protocol: protocol,
                target_protocol: protocol,
                level: ConversionLevel::Native,
                issues: Vec::new(),
                repairs: Vec::new(),
            },
            executable: true,
        }
    }

    pub(crate) const fn target_protocol(&self) -> ProtocolKind {
        self.target_protocol
    }

    pub(crate) const fn level(&self) -> ConversionLevel {
        self.level
    }

    pub(crate) fn feature_actions(&self) -> &[FeatureAction] {
        &self.feature_actions
    }

    pub(crate) fn omits_feature_at(&self, feature: Feature, path: &str) -> bool {
        self.feature_actions.iter().any(|action| {
            action.feature == feature
                && action.path == path
                && action.action == FeatureDisposition::OmitUnsupportedFeature
        })
    }

    pub(crate) fn artifact_actions(&self) -> &[ArtifactAction] {
        &self.artifact_actions
    }

    #[cfg(test)]
    pub(crate) fn repairs(&self) -> &[ConversionRepair] {
        &self.repairs
    }

    pub(crate) const fn report(&self) -> &ConversionReport {
        &self.report
    }

    pub(crate) const fn is_executable(&self) -> bool {
        self.executable
    }

    pub(crate) fn attach_repairs(
        &mut self,
        repairs: Vec<ConversionRepair>,
        _policy: ConversionPolicy,
    ) -> Result<(), ConversionError> {
        if repairs.is_empty() {
            return Ok(());
        }
        let repair_level = repairs
            .iter()
            .map(|repair| match repair.class {
                super::RepairClass::LosslessStructural => ConversionLevel::Lossless,
                super::RepairClass::CompatibleAllowlisted => ConversionLevel::Compatible,
                super::RepairClass::DiagnosticLossy => ConversionLevel::Lossy,
            })
            .max()
            .unwrap_or(ConversionLevel::Native);
        self.level = self.level.max(repair_level);
        self.report.level = self.level;
        self.report.repairs.extend(repairs.iter().cloned());
        self.repairs.extend(repairs);

        Ok(())
    }

    pub(crate) fn attach_response_extensions(
        &mut self,
        extensions: &[ProviderExtension],
        source: ProtocolKind,
        target: ProtocolKind,
        target_issuer: &IssuerIdentity,
        model: &str,
        policy: ConversionPolicy,
    ) -> Result<(), ConversionError> {
        if extensions.is_empty() {
            return Ok(());
        }

        let mut feature_actions = Vec::new();
        let mut issues = Vec::new();
        let mut blocked = false;
        let mut lossy = false;
        evaluate_provider_extensions(
            extensions,
            "response.extensions",
            source,
            target,
            target_issuer,
            model,
            source == target,
            &mut feature_actions,
            &mut issues,
            &mut blocked,
            &mut lossy,
            policy,
        );
        self.feature_actions.extend(feature_actions);
        self.report.issues.extend(issues);

        if blocked {
            self.level = ConversionLevel::Blocked;
            self.report.level = ConversionLevel::Blocked;
            self.executable = false;
            return Err(ConversionError {
                code: "conversion_blocked",
                report: self.report.clone(),
            });
        }
        if lossy {
            self.level = self.level.max(ConversionLevel::Lossy);
            self.report.level = self.level;
        }
        Ok(())
    }

    pub(crate) fn attach_response_content(
        &mut self,
        response: &CanonicalResponseV2,
        target: ProtocolKind,
        target_issuer: &IssuerIdentity,
        model: &str,
    ) {
        let mut issues = response_content_issues(response, target, target_issuer, model);
        let artifact_evaluation =
            evaluate_response_artifacts(response, target, target_issuer, model);
        self.artifact_actions.extend(artifact_evaluation.actions);
        issues.extend(artifact_evaluation.issues);
        if issues.is_empty() {
            return;
        }
        self.report.issues.extend(issues);
        self.level = self.level.max(ConversionLevel::Lossy);
        self.report.level = self.level;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConversionError {
    code: &'static str,
    report: ConversionReport,
}

impl ConversionError {
    pub(crate) const fn code(&self) -> &'static str {
        self.code
    }

    pub(crate) const fn report(&self) -> &ConversionReport {
        &self.report
    }

    pub(crate) fn blocked_issue(
        code: &'static str,
        source: ProtocolKind,
        target: ProtocolKind,
        path: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            code,
            report: ConversionReport {
                source_protocol: source,
                target_protocol: target,
                level: ConversionLevel::Blocked,
                issues: vec![ConversionIssue {
                    code: code.to_string(),
                    severity: IssueSeverity::Error,
                    path: path.into(),
                    feature: None,
                    target_limitation: "transcript cannot be encoded safely".to_string(),
                    action: IssueAction::Block,
                    summary: summary.into(),
                }],
                repairs: Vec::new(),
            },
        }
    }
}

impl fmt::Display for ConversionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}: conversion from {} to {} is {}",
            self.code, self.report.source_protocol, self.report.target_protocol, self.report.level
        )
    }
}

impl std::error::Error for ConversionError {}

pub(crate) fn plan_conversion(
    request: &CanonicalRequestV2,
    source: ProtocolKind,
    target: ProtocolKind,
    source_issuer: &IssuerIdentity,
    target_issuer: &IssuerIdentity,
    profile: &CapabilityProfile,
    policy: ConversionPolicy,
) -> Result<ConversionPlan, ConversionError> {
    let native = source == target && source_issuer == target_issuer;
    let mut level = if native {
        ConversionLevel::Native
    } else {
        ConversionLevel::Lossless
    };
    let mut feature_actions = Vec::new();
    let mut issues = Vec::new();
    let mut blocked = false;
    let mut lossy = false;
    let mut compatible = false;

    for usage in extract_required_features(request) {
        if usage.feature == Feature::DeveloperInstruction
            && !protocol_wire_supports(target, Feature::DeveloperInstruction)
        {
            compatible = true;
            feature_actions.push(FeatureAction {
                feature: usage.feature,
                path: usage.path.clone(),
                action: FeatureDisposition::CoalesceRoleEnvelope,
            });
            issues.push(ConversionIssue {
                code: "developer_instruction_coalesced".to_string(),
                severity: IssueSeverity::Info,
                path: usage.path,
                feature: Some(usage.feature),
                target_limitation: "target has one system instruction envelope".to_string(),
                action: IssueAction::Rewrite,
                summary: "Developer instructions merged into target system instructions."
                    .to_string(),
            });
            continue;
        }
        if usage.feature == Feature::NamespaceTools
            && !protocol_wire_supports(target, Feature::NamespaceTools)
            && protocol_wire_supports(target, Feature::Tools)
        {
            match super::flatten_namespace_tools(&request.tools) {
                Ok(_) => {
                    compatible = true;
                    feature_actions.push(FeatureAction {
                        feature: usage.feature,
                        path: usage.path.clone(),
                        action: FeatureDisposition::FlattenNamespace,
                    });
                    issues.push(ConversionIssue {
                        code: "namespace_flattened".to_string(),
                        severity: IssueSeverity::Info,
                        path: usage.path,
                        feature: Some(usage.feature),
                        target_limitation: "target represents namespaced tools as flat functions"
                            .to_string(),
                        action: IssueAction::Rewrite,
                        summary: "Namespace tools flattened with reversible names.".to_string(),
                    });
                }
                Err(error) => {
                    let custom = matches!(error, super::NamespaceError::CustomToolUnsupported(_));
                    blocked = true;
                    feature_actions.push(FeatureAction {
                        feature: usage.feature,
                        path: usage.path.clone(),
                        action: FeatureDisposition::Block,
                    });
                    issues.push(ConversionIssue {
                        code: if custom {
                            "unsupported_required_feature"
                        } else {
                            "namespace_name_collision"
                        }
                        .to_string(),
                        severity: IssueSeverity::Error,
                        path: usage.path,
                        feature: Some(if custom {
                            Feature::CustomTools
                        } else {
                            usage.feature
                        }),
                        target_limitation: error.to_string(),
                        action: IssueAction::Block,
                        summary: if custom {
                            "Target protocol cannot losslessly map free-text tools to JSON functions."
                        } else {
                            "Flattened namespace names conflict with existing tools."
                        }
                        .to_string(),
                    });
                }
            }
            continue;
        }
        let support = profile.resolve(usage.feature, &request.model, target);
        // Exact provider/model evidence may enable a non-standard but lossless dialect extension.
        // This is required for subscription transports such as Codex Responses audio while the
        // public Responses schema remains fail-closed.
        if (protocol_wire_supports(target, usage.feature)
            && (!feature_uses_provider_evidence(usage.feature)
                || !support_is_confirmed_unsupported(&support)))
            || (protocol_evidence_extension_supports(target, usage.feature)
                && support.state == SupportState::Supported)
        {
            feature_actions.push(FeatureAction {
                feature: usage.feature,
                path: usage.path.clone(),
                action: if native {
                    FeatureDisposition::Preserve
                } else {
                    FeatureDisposition::Encode
                },
            });
            if support.state == SupportState::Unsupported {
                issues.push(ConversionIssue {
                    code: "capability_evidence_conflict".to_string(),
                    severity: IssueSeverity::Warning,
                    path: usage.path,
                    feature: Some(usage.feature),
                    target_limitation:
                        "capability evidence reports unsupported, but the target wire can represent the feature"
                            .to_string(),
                    action: IssueAction::Encode,
                    summary: "Capability evidence conflicts with the wire format; deferring to upstream.".to_string(),
                });
            }
            continue;
        }
        if artifact_policy_feature(usage.feature) {
            feature_actions.push(FeatureAction {
                feature: usage.feature,
                path: usage.path,
                action: FeatureDisposition::Encode,
            });
            continue;
        }
        if safely_omittable_feature(usage.feature) {
            lossy = true;
            let feature_name = format!("{:?}", usage.feature);
            feature_actions.push(FeatureAction {
                feature: usage.feature,
                path: usage.path.clone(),
                action: FeatureDisposition::OmitUnsupportedFeature,
            });
            issues.push(ConversionIssue {
                code: "unsupported_feature_omitted".to_string(),
                severity: IssueSeverity::Warning,
                path: usage.path,
                feature: Some(usage.feature),
                target_limitation: format!(
                    "target protocol {target} cannot represent optional feature {feature_name}"
                ),
                action: IssueAction::Filter,
                summary: format!(
                    "Target protocol {target} cannot represent optional feature {feature_name}; omitted it and continued."
                ),
            });
            continue;
        }
        blocked = true;
        feature_actions.push(FeatureAction {
            feature: usage.feature,
            path: usage.path.clone(),
            action: FeatureDisposition::Block,
        });
        issues.push(ConversionIssue {
            code: "unsupported_required_feature".to_string(),
            severity: IssueSeverity::Error,
            path: usage.path,
            feature: Some(usage.feature),
            target_limitation: "target wire protocol cannot represent this required feature"
                .to_string(),
            action: IssueAction::Block,
            summary: "Target protocol cannot represent or safely omit this request content."
                .to_string(),
        });
    }

    evaluate_provider_extensions(
        &request.extensions,
        "extensions",
        source,
        target,
        target_issuer,
        &request.model,
        native,
        &mut feature_actions,
        &mut issues,
        &mut blocked,
        &mut lossy,
        policy,
    );
    evaluate_provider_extensions(
        &request.reasoning.provider_extensions,
        "reasoning.provider_extensions",
        source,
        target,
        target_issuer,
        &request.model,
        native,
        &mut feature_actions,
        &mut issues,
        &mut blocked,
        &mut lossy,
        policy,
    );

    let reasoning_omissions =
        reasoning_history_omissions(request, source, target, source_issuer, target_issuer);
    for path in &reasoning_omissions {
        lossy = true;
        feature_actions.push(FeatureAction {
            feature: Feature::Reasoning,
            path: path.clone(),
            action: FeatureDisposition::OmitUnsupportedFeature,
        });
        issues.push(ConversionIssue {
            code: "reasoning_history_omitted".to_string(),
            severity: IssueSeverity::Warning,
            path: path.clone(),
            feature: Some(Feature::Reasoning),
            target_limitation:
                "historical hidden reasoning is not authenticated for the target issuer"
                    .to_string(),
            action: IssueAction::Filter,
            summary: "Unsigned or foreign reasoning history was omitted; visible assistant content and tool calls were preserved."
                .to_string(),
        });
    }

    if target == ProtocolKind::GeminiNative
        && profile.requires(Feature::GeminiThoughtSignatureReplay)
    {
        for path in missing_gemini_signature_steps(request) {
            blocked = true;
            issues.push(ConversionIssue {
                code: "artifact_required_missing".to_string(),
                severity: IssueSeverity::Error,
                path,
                feature: Some(Feature::GeminiThoughtSignatureReplay),
                target_limitation:
                    "selected Gemini target requires a thought signature on the first function call of each assistant step"
                        .to_string(),
                action: IssueAction::Block,
                summary: "Gemini tool step is missing a required thought signature.".to_string(),
            });
        }
    }

    let artifact_evaluation =
        evaluate_artifacts(request, target, target_issuer, &reasoning_omissions);
    blocked |= artifact_evaluation.blocked;
    compatible |= artifact_evaluation.compatible;
    if compatible && !blocked && !lossy {
        level = ConversionLevel::Compatible;
    }
    issues.extend(artifact_evaluation.issues);

    if blocked {
        level = ConversionLevel::Blocked;
    } else if lossy {
        level = ConversionLevel::Lossy;
    }
    let report = ConversionReport {
        source_protocol: source,
        target_protocol: target,
        level,
        issues,
        repairs: Vec::new(),
    };
    if blocked {
        return Err(ConversionError {
            code: "conversion_blocked",
            report,
        });
    }
    Ok(ConversionPlan {
        tool_mapping: super::ToolWireMap::default(),
        target_protocol: target,
        level,
        feature_actions,
        artifact_actions: artifact_evaluation.actions,
        repairs: Vec::new(),
        report,
        executable: true,
    })
}

fn safely_omittable_feature(feature: Feature) -> bool {
    matches!(
        feature,
        Feature::CacheControl
            | Feature::PromptCacheOptions
            | Feature::ReasoningSummary
            | Feature::OutputVerbosity
            | Feature::ResponseStorage
            | Feature::UserIdentity
    )
}

fn artifact_policy_feature(feature: Feature) -> bool {
    matches!(
        feature,
        Feature::EncryptedReasoning
            | Feature::AnthropicSignatureReplay
            | Feature::GeminiThoughtSignatureReplay
    )
}

fn response_content_issues(
    response: &CanonicalResponseV2,
    target: ProtocolKind,
    target_issuer: &IssuerIdentity,
    model: &str,
) -> Vec<ConversionIssue> {
    let mut issues = Vec::new();
    if let Some(error) = &response.error {
        if target == ProtocolKind::AnthropicMessages && (error.retryable || error.details.is_some())
        {
            push_response_approximation(
                &mut issues,
                "response_error_metadata_approximated",
                "$.error",
                "Anthropic error envelopes expose only a provider type and message",
                "Anthropic errors cannot carry all retry hints or extra details; error type and message are preserved.",
            );
        }
    }
    for (index, block) in response.blocks.iter().enumerate() {
        let path = format!("$.blocks[{index}]");
        match block {
            ContentBlock::Text(_)
            | ContentBlock::ToolCall(_)
            | ContentBlock::ProviderArtifact(_) => {}
            ContentBlock::Reasoning(reasoning) => {
                if target == ProtocolKind::AnthropicMessages {
                    let has_signature = reasoning.artifacts.iter().any(|artifact| {
                        matches!(
                            artifact.kind,
                            ArtifactKind::AnthropicThinkingSignature
                                | ArtifactKind::AnthropicRedactedThinking
                        ) && affinity_permits(&artifact.affinity, target, target_issuer, model)
                    });
                    if !has_signature {
                        push_response_approximation(
                            &mut issues,
                            "response_reasoning_approximated",
                            &path,
                            "Anthropic response thinking requires an issuer-valid signature",
                            "Reasoning lacks a valid target Anthropic signature; returning it as plain text.",
                        );
                    }
                }
                if !reasoning.summary.is_empty() && target != ProtocolKind::OpenAiResponses {
                    push_response_approximation(
                        &mut issues,
                        "response_reasoning_summary_omitted",
                        &format!("{path}.summary"),
                        "target response protocol has no equivalent reasoning-summary field",
                        "Target protocol has no separate reasoning summary; response content is preserved.",
                    );
                }
            }
            ContentBlock::Refusal(_)
                if matches!(
                    target,
                    ProtocolKind::AnthropicMessages | ProtocolKind::GeminiNative
                ) =>
            {
                push_response_approximation(
                    &mut issues,
                    "response_refusal_approximated",
                    &path,
                    "target protocol represents refusal text as ordinary text",
                    "Target protocol has no refusal block; returning it as plain text.",
                );
            }
            ContentBlock::Refusal(_) => {}
            ContentBlock::Image(_)
            | ContentBlock::Audio(_)
            | ContentBlock::Video(_)
            | ContentBlock::File(_)
                if target != ProtocolKind::GeminiNative =>
            {
                push_response_approximation(
                    &mut issues,
                    "response_media_omitted",
                    &path,
                    "target response encoder has no validated equivalent media output block",
                    "No verified target media output mapping; omitted the block and preserved the remaining response.",
                );
            }
            ContentBlock::Image(_)
            | ContentBlock::Audio(_)
            | ContentBlock::Video(_)
            | ContentBlock::File(_) => {}
            ContentBlock::ToolResult(_) if target != ProtocolKind::GeminiNative => {
                push_response_approximation(
                    &mut issues,
                    "response_tool_result_omitted",
                    &path,
                    "target response protocol does not allow tool-result items in assistant output",
                    "Target protocol cannot represent assistant tool results; omitted the block and preserved the remaining response.",
                );
            }
            ContentBlock::ToolResult(result) => {
                if result
                    .content
                    .iter()
                    .any(|block| !matches!(block, ContentBlock::Text(_)))
                {
                    push_response_approximation(
                        &mut issues,
                        "response_tool_result_content_approximated",
                        &format!("{path}.content"),
                        "Gemini tool-result output can retain only the normalized JSON/text body",
                        "Non-text tool results normalized to JSON/text.",
                    );
                }
            }
        }
    }
    issues
}

fn push_response_approximation(
    issues: &mut Vec<ConversionIssue>,
    code: &str,
    path: &str,
    target_limitation: &str,
    summary: &str,
) {
    issues.push(ConversionIssue {
        code: code.to_string(),
        severity: IssueSeverity::Warning,
        path: path.to_string(),
        feature: None,
        target_limitation: target_limitation.to_string(),
        action: IssueAction::Approximate,
        summary: summary.to_string(),
    });
}

#[allow(clippy::too_many_arguments)]
fn evaluate_provider_extensions(
    extensions: &[ProviderExtension],
    base_path: &str,
    source: ProtocolKind,
    target: ProtocolKind,
    target_issuer: &IssuerIdentity,
    target_model: &str,
    native: bool,
    feature_actions: &mut Vec<FeatureAction>,
    issues: &mut Vec<ConversionIssue>,
    blocked: &mut bool,
    lossy: &mut bool,
    policy: ConversionPolicy,
) {
    for (index, extension) in extensions.iter().enumerate() {
        let action_path = format!("{base_path}[{index}]");
        let issue_path = extension
            .nested_path()
            .map(str::to_string)
            .unwrap_or_else(|| match base_path {
                "extensions" => format!("$.{}", extension.name),
                "response.extensions" => format!("$.{}", extension.name),
                "reasoning.provider_extensions" => {
                    format!("$.reasoning.{}", extension.name)
                }
                _ => format!("{action_path}.{}", extension.name),
            });
        let extension_name = extension.nested_path().unwrap_or(extension.name.as_str());
        let extension_label = format!("{}.{}", extension.namespace, extension_name);
        if (native || source == target)
            && affinity_permits(&extension.affinity, target, target_issuer, target_model)
        {
            feature_actions.push(FeatureAction {
                feature: Feature::ProviderExtension,
                path: action_path,
                action: FeatureDisposition::Preserve,
            });
            continue;
        }
        if extension.criticality == ExtensionCriticality::Critical
            && policy == ConversionPolicy::Diagnostic
        {
            *blocked = true;
            feature_actions.push(FeatureAction {
                feature: Feature::ProviderExtension,
                path: action_path,
                action: FeatureDisposition::Block,
            });
            issues.push(ConversionIssue {
                code: "provider_extension_not_portable".to_string(),
                severity: IssueSeverity::Error,
                path: issue_path,
                feature: Some(Feature::ProviderExtension),
                target_limitation: format!(
                    "critical provider extension {extension_label} has no registered target mapping"
                ),
                action: IssueAction::Block,
                summary: format!("Target protocol cannot safely preserve required provider extension {extension_label}."),
            });
        } else {
            *lossy = true;
            feature_actions.push(FeatureAction {
                feature: Feature::ProviderExtension,
                path: action_path,
                action: FeatureDisposition::OmitProviderExtension,
            });
            let (code, target_limitation, summary) = if extension.criticality
                == ExtensionCriticality::Critical
            {
                (
                    "provider_extension_approximated",
                    format!(
                        "required provider extension {extension_label} has no registered target mapping"
                    ),
                    format!(
                        "Required provider extension {extension_label} has no target mapping; omitted it and continued."
                    ),
                )
            } else {
                (
                    "provider_extension_omitted",
                    format!(
                        "advisory provider extension {extension_label} has no registered target mapping"
                    ),
                    format!("Unmapped provider extension {extension_label} will be omitted."),
                )
            };
            issues.push(ConversionIssue {
                code: code.to_string(),
                severity: IssueSeverity::Warning,
                path: issue_path,
                feature: Some(Feature::ProviderExtension),
                target_limitation,
                action: IssueAction::Approximate,
                summary,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::protocol::capability::{
        CapabilityEvidence, CapabilityProfile, EvidenceSource, Feature, SupportState,
    };
    use crate::protocol::ir::{
        ArtifactAffinity, ArtifactCriticality, BlockMetadata, CanonicalRequestV2, ContentBlock,
        CustomTool, ExtensionCriticality, FunctionTool, MediaBlock, MediaSource, NamespaceTool,
        OpaqueArtifact, ProviderExtension, ReplayPolicy, ResponseFormat, TextBlock, ToolCall,
        ToolChoice, ToolDefinition, Turn, TurnRole,
    };
    use crate::protocol::kind::ProtocolKind;

    fn issuer(provider: &str, endpoint: &str, account: &str) -> IssuerIdentity {
        IssuerIdentity {
            provider: provider.to_string(),
            endpoint_fingerprint: endpoint.to_string(),
            account_fingerprint: Some(account.to_string()),
            model_family: Some("test-family".to_string()),
        }
    }

    fn text_request() -> CanonicalRequestV2 {
        let mut request = CanonicalRequestV2::new("test-model");
        request.turns.push(Turn {
            id: None,
            role: TurnRole::User,
            blocks: vec![ContentBlock::Text(TextBlock::new("hello"))],
            status: None,
        });
        request
    }

    #[test]
    fn planner_uses_the_same_minimal_loss_policy_in_production_and_diagnostics() {
        let same = issuer("openai", "endpoint-a", "account-a");
        let native = plan_conversion(
            &text_request(),
            ProtocolKind::OpenAiResponses,
            ProtocolKind::OpenAiResponses,
            &same,
            &same,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .expect("native plan");
        assert_eq!(native.level(), ConversionLevel::Native);
        assert!(native.is_executable());

        let mut tools = text_request();
        tools.tools.push(ToolDefinition::Function(FunctionTool {
            name: "lookup".to_string(),
            description: None,
            input_schema: json!({"type":"object"}),
            strict: None,
            cache_policy: None,
            defer_loading: None,
            allowed_callers: Vec::new(),
            input_examples: Vec::new(),
            eager_input_streaming: None,
        }));
        tools.tool_choice = ToolChoice::Named {
            name: "lookup".to_string(),
        };
        let lossless = plan_conversion(
            &tools,
            ProtocolKind::OpenAiChat,
            ProtocolKind::AnthropicMessages,
            &same,
            &issuer("anthropic", "endpoint-b", "account-b"),
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .expect("lossless tools plan");
        assert_eq!(lossless.level(), ConversionLevel::Lossless);

        let mut encrypted = text_request();
        encrypted.turns[0]
            .blocks
            .push(ContentBlock::ProviderArtifact(OpaqueArtifact {
                kind: crate::protocol::ir::ArtifactKind::OpenAiEncryptedReasoning,
                payload: json!("opaque"),
                affinity: ArtifactAffinity::ExactIssuer {
                    provider: "openai".to_string(),
                    endpoint_fingerprint: "endpoint-a".to_string(),
                    account_fingerprint: Some("account-a".to_string()),
                    model: Some("test-model".to_string()),
                },
                replay: ReplayPolicy::FilterWhenIncompatible,
                criticality: ArtifactCriticality::Supplemental,
            }));
        let compatible = plan_conversion(
            &encrypted,
            ProtocolKind::OpenAiResponses,
            ProtocolKind::OpenAiResponses,
            &same,
            &issuer("openai", "endpoint-b", "account-b"),
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .expect("compatible filter plan");
        assert_eq!(compatible.level(), ConversionLevel::Compatible);
        assert_eq!(
            compatible.artifact_actions()[0].action,
            crate::protocol::conversion::ArtifactDisposition::Filter
        );
        assert_eq!(compatible.report().issues[0].code, "artifact_filtered");

        let mut missing_signature = text_request();
        missing_signature.turns.push(Turn {
            id: None,
            role: TurnRole::Assistant,
            blocks: vec![ContentBlock::ToolCall(ToolCall::function(
                "call-1",
                "lookup",
                json!({}),
            ))],
            status: None,
        });
        let mut gemini_profile = CapabilityProfile::default();
        gemini_profile.require(Feature::GeminiThoughtSignatureReplay);
        let missing = plan_conversion(
            &missing_signature,
            ProtocolKind::GeminiNative,
            ProtocolKind::GeminiNative,
            &issuer("google", "endpoint-g", "account-g"),
            &issuer("google", "endpoint-g", "account-g"),
            &gemini_profile,
            ConversionPolicy::Production,
        )
        .expect_err("missing required Gemini signature");
        assert_eq!(missing.code(), "conversion_blocked");
        assert_eq!(missing.report().issues[0].code, "artifact_required_missing");

        let mut image = text_request();
        image.turns[0].blocks.push(ContentBlock::Image(MediaBlock {
            source: MediaSource::InlineBase64 {
                media_type: "image/png".to_string(),
                data: "AA==".to_string(),
            },
            detail: None,
            metadata: BlockMetadata::default(),
        }));
        let unsupported_image = CapabilityProfile::new(vec![CapabilityEvidence::exact_model(
            Feature::ImageInput,
            SupportState::Unsupported,
            EvidenceSource::LiveProbe,
            "test-model",
            ProtocolKind::AnthropicMessages,
            10,
        )]);
        let declared_unsupported = plan_conversion(
            &image,
            ProtocolKind::OpenAiChat,
            ProtocolKind::AnthropicMessages,
            &same,
            &issuer("anthropic", "endpoint-b", "account-b"),
            &unsupported_image,
            ConversionPolicy::Production,
        )
        .expect("wire-representable input is sent even when capability evidence disagrees");
        assert_eq!(
            declared_unsupported.report().level,
            ConversionLevel::Lossless
        );
        assert_eq!(
            declared_unsupported.report().issues[0].code,
            "capability_evidence_conflict"
        );

        let mut schema = text_request();
        schema.response_format = ResponseFormat::JsonSchema {
            name: "answer".to_string(),
            description: None,
            schema: json!({"type":"object"}),
            strict: Some(true),
        };
        let schema_profile = CapabilityProfile::new(vec![
            CapabilityEvidence::protocol_default(
                Feature::JsonSchema,
                SupportState::Unsupported,
                EvidenceSource::LiveProbe,
                ProtocolKind::AnthropicMessages,
                10,
            ),
            CapabilityEvidence::protocol_default(
                Feature::JsonObject,
                SupportState::Supported,
                EvidenceSource::LiveProbe,
                ProtocolKind::AnthropicMessages,
                10,
            ),
        ]);
        let production_schema = plan_conversion(
            &schema,
            ProtocolKind::OpenAiResponses,
            ProtocolKind::AnthropicMessages,
            &same,
            &issuer("anthropic", "endpoint-b", "account-b"),
            &schema_profile,
            ConversionPolicy::Production,
        )
        .expect("production sends wire-representable schema");
        assert_eq!(production_schema.level(), ConversionLevel::Lossless);
        assert_eq!(
            production_schema.report().issues[0].code,
            "capability_evidence_conflict"
        );

        let diagnostic_schema = plan_conversion(
            &schema,
            ProtocolKind::OpenAiResponses,
            ProtocolKind::AnthropicMessages,
            &same,
            &issuer("anthropic", "endpoint-b", "account-b"),
            &schema_profile,
            ConversionPolicy::Diagnostic,
        )
        .expect("diagnostic sends the same wire-representable schema");
        assert_eq!(diagnostic_schema.level(), ConversionLevel::Lossless);
        assert!(diagnostic_schema.is_executable());
        assert_eq!(diagnostic_schema.report(), production_schema.report());
    }

    #[test]
    fn capability_resolution_prefers_exact_fresh_evidence_over_defaults() {
        let profile = CapabilityProfile::new(vec![
            CapabilityEvidence::protocol_default(
                Feature::ImageInput,
                SupportState::Supported,
                EvidenceSource::Inferred,
                ProtocolKind::OpenAiChat,
                100,
            ),
            CapabilityEvidence::exact_model(
                Feature::ImageInput,
                SupportState::Unsupported,
                EvidenceSource::LiveProbe,
                "vision-disabled",
                ProtocolKind::OpenAiChat,
                50,
            ),
        ]);
        assert_eq!(
            profile
                .resolve(
                    Feature::ImageInput,
                    "vision-disabled",
                    ProtocolKind::OpenAiChat
                )
                .state,
            SupportState::Unsupported
        );
        assert_eq!(
            profile
                .resolve(Feature::ImageInput, "other", ProtocolKind::OpenAiChat)
                .state,
            SupportState::Supported
        );
    }

    #[test]
    fn model_evidence_activates_only_reviewed_dialect_extensions() {
        let source = issuer("google", "gemini", "account");
        let target = issuer("openai", "responses", "account");

        let mut video = text_request();
        video.turns[0].blocks.push(ContentBlock::Video(MediaBlock {
            source: MediaSource::InlineBase64 {
                media_type: "video/mp4".to_string(),
                data: "AA==".to_string(),
            },
            detail: None,
            metadata: BlockMetadata::default(),
        }));
        let claimed_video = CapabilityProfile::new(vec![CapabilityEvidence::exact_model(
            Feature::VideoInput,
            SupportState::Supported,
            EvidenceSource::ProviderMetadata,
            "test-model",
            ProtocolKind::OpenAiResponses,
            1,
        )]);
        let error = plan_conversion(
            &video,
            ProtocolKind::GeminiNative,
            ProtocolKind::OpenAiResponses,
            &source,
            &target,
            &claimed_video,
            ConversionPolicy::Production,
        )
        .expect_err("evidence cannot invent an unimplemented Responses video wire shape");
        assert!(
            error
                .report()
                .issues
                .iter()
                .any(|issue| issue.feature == Some(Feature::VideoInput))
        );

        let mut audio = text_request();
        audio.turns[0].blocks.push(ContentBlock::Audio(MediaBlock {
            source: MediaSource::InlineBase64 {
                media_type: "audio/wav".to_string(),
                data: "AA==".to_string(),
            },
            detail: None,
            metadata: BlockMetadata::default(),
        }));
        let verified_audio = CapabilityProfile::new(vec![CapabilityEvidence::exact_model(
            Feature::AudioInput,
            SupportState::Supported,
            EvidenceSource::ProviderMetadata,
            "test-model",
            ProtocolKind::OpenAiResponses,
            1,
        )]);
        plan_conversion(
            &audio,
            ProtocolKind::OpenAiChat,
            ProtocolKind::OpenAiResponses,
            &issuer("openai", "chat", "account"),
            &target,
            &verified_audio,
            ConversionPolicy::Production,
        )
        .expect("reviewed Codex Responses audio extension");
    }

    #[test]
    fn provider_extensions_follow_affinity_and_criticality_without_hidden_drops() {
        let source_issuer = issuer("openai", "endpoint-a", "account-a");
        let extension = ProviderExtension {
            namespace: "openai".to_string(),
            name: "future_option".to_string(),
            value: json!({"secret":"value"}),
            affinity: ArtifactAffinity::ExactIssuer {
                provider: "openai".to_string(),
                endpoint_fingerprint: "endpoint-a".to_string(),
                account_fingerprint: Some("account-a".to_string()),
                model: Some("test-model".to_string()),
            },
            criticality: ExtensionCriticality::Advisory,
        };
        let mut native_request = text_request();
        native_request.extensions.push(extension.clone());
        let native = plan_conversion(
            &native_request,
            ProtocolKind::OpenAiResponses,
            ProtocolKind::OpenAiResponses,
            &source_issuer,
            &source_issuer,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .expect("native extension passthrough");
        assert_eq!(native.level(), ConversionLevel::Native);

        let target_issuer = issuer("anthropic", "endpoint-b", "account-b");
        let production = plan_conversion(
            &native_request,
            ProtocolKind::OpenAiResponses,
            ProtocolKind::AnthropicMessages,
            &source_issuer,
            &target_issuer,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .expect("production omits advisory cross-protocol extension");
        assert_eq!(production.level(), ConversionLevel::Lossy);
        assert_eq!(
            production.report().issues[0].code,
            "provider_extension_omitted"
        );
        assert_eq!(production.report().issues[0].path, "$.future_option");
        assert!(
            production.report().issues[0]
                .summary
                .contains("openai.future_option")
        );

        let diagnostic = plan_conversion(
            &native_request,
            ProtocolKind::OpenAiResponses,
            ProtocolKind::AnthropicMessages,
            &source_issuer,
            &target_issuer,
            &CapabilityProfile::default(),
            ConversionPolicy::Diagnostic,
        )
        .expect("diagnostic applies the same advisory extension omission");
        assert_eq!(diagnostic.level(), ConversionLevel::Lossy);
        assert_eq!(diagnostic.report(), production.report());

        let mut critical_request = text_request();
        critical_request.extensions.push(ProviderExtension {
            criticality: ExtensionCriticality::Critical,
            ..extension
        });
        let production_critical = plan_conversion(
            &critical_request,
            ProtocolKind::OpenAiResponses,
            ProtocolKind::AnthropicMessages,
            &source_issuer,
            &target_issuer,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .expect("production prioritizes availability and reports the approximation");
        assert_eq!(production_critical.level(), ConversionLevel::Lossy);
        assert_eq!(
            production_critical.report().issues[0].code,
            "provider_extension_approximated"
        );
        assert_eq!(
            production_critical.report().issues[0].severity,
            IssueSeverity::Warning
        );

        let critical = plan_conversion(
            &critical_request,
            ProtocolKind::OpenAiResponses,
            ProtocolKind::AnthropicMessages,
            &source_issuer,
            &target_issuer,
            &CapabilityProfile::default(),
            ConversionPolicy::Diagnostic,
        )
        .expect_err("critical extension cannot be dropped");
        assert_eq!(critical.code(), "conversion_blocked");
        assert_eq!(
            critical.report().issues[0].code,
            "provider_extension_not_portable"
        );
    }

    #[test]
    fn namespaces_flatten_only_when_collision_free_and_custom_grammars_block() {
        let source = issuer("openai", "responses", "account");
        let target = issuer("openai", "chat", "account");
        let mut request = text_request();
        request.tools.push(ToolDefinition::Namespace(NamespaceTool {
            namespace: "repo".to_string(),
            description: None,
            tools: vec![
                FunctionTool {
                    name: "read".to_string(),
                    description: None,
                    input_schema: json!({"type":"object"}),
                    strict: None,
                    cache_policy: None,
                    defer_loading: None,
                    allowed_callers: Vec::new(),
                    input_examples: Vec::new(),
                    eager_input_streaming: None,
                }
                .into(),
            ],
        }));

        let plan = plan_conversion(
            &request,
            ProtocolKind::OpenAiResponses,
            ProtocolKind::OpenAiChat,
            &source,
            &target,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap();
        assert_eq!(plan.level(), ConversionLevel::Compatible);
        assert!(plan.feature_actions().iter().any(|action| {
            action.feature == Feature::NamespaceTools
                && action.action == FeatureDisposition::FlattenNamespace
        }));

        request.tools.insert(
            0,
            ToolDefinition::Function(FunctionTool {
                name: "ns4_repo_read".to_string(),
                description: None,
                input_schema: json!({"type":"object"}),
                strict: None,
                cache_policy: None,
                defer_loading: None,
                allowed_callers: Vec::new(),
                input_examples: Vec::new(),
                eager_input_streaming: None,
            }),
        );
        let collision = plan_conversion(
            &request,
            ProtocolKind::OpenAiResponses,
            ProtocolKind::OpenAiChat,
            &source,
            &target,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap_err();
        assert!(
            collision
                .report()
                .issues
                .iter()
                .any(|issue| issue.code == "namespace_name_collision")
        );

        let mut custom = text_request();
        custom.tools.push(ToolDefinition::Custom(CustomTool {
            name: "shell".to_string(),
            description: None,
            grammar: json!({"type":"grammar","syntax":"lark","definition":"start: WORD"}),
        }));
        let blocked = plan_conversion(
            &custom,
            ProtocolKind::OpenAiResponses,
            ProtocolKind::OpenAiChat,
            &source,
            &target,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap_err();
        assert!(
            blocked
                .report()
                .issues
                .iter()
                .any(|issue| issue.feature == Some(Feature::CustomTools))
        );
    }

    #[test]
    fn developer_instruction_coalescing_is_an_explicit_compatible_action() {
        let source = issuer("openai", "chat", "account");
        let target = issuer("anthropic", "messages", "account");
        let mut request = text_request();
        request.instructions.push(crate::protocol::ir::Instruction {
            role: crate::protocol::ir::InstructionRole::Developer,
            blocks: vec![ContentBlock::Text(TextBlock::new("developer rule"))],
        });

        let plan = plan_conversion(
            &request,
            ProtocolKind::OpenAiChat,
            ProtocolKind::AnthropicMessages,
            &source,
            &target,
            &CapabilityProfile::default(),
            ConversionPolicy::Production,
        )
        .unwrap();

        assert_eq!(plan.level(), ConversionLevel::Compatible);
        assert!(plan.feature_actions().iter().any(|action| {
            action.feature == Feature::DeveloperInstruction
                && action.action == FeatureDisposition::CoalesceRoleEnvelope
        }));
    }

    #[test]
    fn compatible_action_registry_contains_only_reviewed_deterministic_actions() {
        use crate::protocol::conversion::{CompatibleAction, compatible_action_allowed};

        for action in [
            CompatibleAction::FilterSupplementalCrossIssuerArtifact,
            CompatibleAction::CoalesceRoleEnvelope,
            CompatibleAction::FlattenNamespaceWithoutCollision,
            CompatibleAction::NormalizeRequiredEmptyContent,
        ] {
            assert!(compatible_action_allowed(action));
        }
    }

    #[test]
    fn required_artifact_affinity_mismatch_blocks_instead_of_filtering() {
        let source_issuer = issuer("anthropic", "endpoint-a", "account-a");
        let target_issuer = issuer("anthropic", "endpoint-b", "account-b");
        let mut request = text_request();
        request.turns[0]
            .blocks
            .push(ContentBlock::ProviderArtifact(OpaqueArtifact {
                kind: crate::protocol::ir::ArtifactKind::AnthropicThinkingSignature,
                payload: json!("required-signature"),
                affinity: ArtifactAffinity::ExactIssuer {
                    provider: "anthropic".to_string(),
                    endpoint_fingerprint: "endpoint-a".to_string(),
                    account_fingerprint: Some("account-a".to_string()),
                    model: Some("test-model".to_string()),
                },
                replay: ReplayPolicy::Required,
                criticality: ArtifactCriticality::Required,
            }));
        let error = plan_conversion(
            &request,
            ProtocolKind::AnthropicMessages,
            ProtocolKind::AnthropicMessages,
            &source_issuer,
            &target_issuer,
            &CapabilityProfile::default(),
            ConversionPolicy::Diagnostic,
        )
        .expect_err("required signature cannot be filtered");
        assert_eq!(error.code(), "conversion_blocked");
        assert_eq!(
            error.report().issues[0].code,
            "conversation_state_incompatible"
        );
    }

    #[test]
    fn gemini_requires_a_signature_on_the_first_call_of_each_assistant_step() {
        let google = issuer("google", "endpoint-g", "account-g");
        let mut request = text_request();
        let mut signed = ToolCall::function("call-1", "first", json!({}));
        signed.artifacts.push(OpaqueArtifact {
            kind: crate::protocol::ir::ArtifactKind::GeminiThoughtSignature,
            payload: json!("sig-1"),
            affinity: ArtifactAffinity::ExactIssuer {
                provider: "google".to_string(),
                endpoint_fingerprint: "endpoint-g".to_string(),
                account_fingerprint: Some("account-g".to_string()),
                model: Some("test-model".to_string()),
            },
            replay: ReplayPolicy::Required,
            criticality: ArtifactCriticality::Required,
        });
        request.turns.push(Turn {
            id: None,
            role: TurnRole::Assistant,
            blocks: vec![ContentBlock::ToolCall(signed)],
            status: None,
        });
        request.turns.push(Turn {
            id: None,
            role: TurnRole::Assistant,
            blocks: vec![ContentBlock::ToolCall(ToolCall::function(
                "call-2",
                "second",
                json!({}),
            ))],
            status: None,
        });
        let mut profile = CapabilityProfile::default();
        profile.require(Feature::GeminiThoughtSignatureReplay);

        let error = plan_conversion(
            &request,
            ProtocolKind::GeminiNative,
            ProtocolKind::GeminiNative,
            &google,
            &google,
            &profile,
            ConversionPolicy::Production,
        )
        .expect_err("the second assistant step is missing its signature");

        assert!(error.report().issues.iter().any(|issue| {
            issue.code == "artifact_required_missing" && issue.path == "turns[2].blocks[0]"
        }));
    }
}
