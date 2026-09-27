use crate::protocol::continuation::is_portable_artifact;
use crate::protocol::ir::{
    ArtifactAffinity, ArtifactCriticality, ArtifactKind, CanonicalRequestV2, CanonicalResponseV2,
    ContentBlock, OpaqueArtifact, ReplayPolicy, TurnRole,
};
use crate::protocol::kind::ProtocolKind;

use super::{
    ArtifactAction, ArtifactDisposition, CompatibleAction, ConversionIssue, IssueAction,
    IssueSeverity, IssuerIdentity, compatible_action_allowed,
};

#[derive(Debug, Default)]
pub(super) struct ArtifactEvaluation {
    pub(super) actions: Vec<ArtifactAction>,
    pub(super) issues: Vec<ConversionIssue>,
    pub(super) compatible: bool,
    pub(super) blocked: bool,
}

pub(super) fn evaluate_artifacts(
    request: &CanonicalRequestV2,
    target_protocol: ProtocolKind,
    target_issuer: &IssuerIdentity,
    omitted_reasoning_paths: &[String],
) -> ArtifactEvaluation {
    let mut artifacts = Vec::new();
    for (index, instruction) in request.instructions.iter().enumerate() {
        collect_artifacts(
            &instruction.blocks,
            &format!("instructions[{index}]"),
            &mut artifacts,
        );
    }
    for (index, turn) in request.turns.iter().enumerate() {
        collect_artifacts(&turn.blocks, &format!("turns[{index}]"), &mut artifacts);
    }

    let mut evaluation = ArtifactEvaluation::default();
    for (path, artifact) in artifacts {
        if omitted_reasoning_paths.iter().any(|block_path| {
            path.strip_prefix(block_path)
                .is_some_and(|suffix| suffix.starts_with(".artifacts["))
        }) {
            evaluation.actions.push(ArtifactAction {
                path,
                kind: artifact.kind.clone(),
                action: ArtifactDisposition::Filter,
            });
            continue;
        }
        let permitted = affinity_permits(
            &artifact.affinity,
            target_protocol,
            target_issuer,
            &request.model,
        );
        if permitted && artifact.replay != ReplayPolicy::Never {
            evaluation.actions.push(ArtifactAction {
                path,
                kind: artifact.kind.clone(),
                action: ArtifactDisposition::Preserve,
            });
            continue;
        }
        let required = (artifact.criticality == ArtifactCriticality::Required
            || artifact.replay == ReplayPolicy::Required)
            && !required_artifact_can_be_omitted_for_target(artifact, target_protocol);
        if required {
            evaluation.blocked = true;
            evaluation.actions.push(ArtifactAction {
                path: path.clone(),
                kind: artifact.kind.clone(),
                action: ArtifactDisposition::Block,
            });
            evaluation.issues.push(ConversionIssue {
                code: "conversation_state_incompatible".to_string(),
                severity: IssueSeverity::Error,
                path,
                feature: None,
                target_limitation: "target issuer does not match required artifact affinity"
                    .to_string(),
                action: IssueAction::Block,
                summary: "Target channel cannot safely replay required continuation data."
                    .to_string(),
            });
            continue;
        }
        let action = CompatibleAction::FilterSupplementalCrossIssuerArtifact;
        debug_assert!(compatible_action_allowed(action));
        evaluation.compatible = true;
        evaluation.actions.push(ArtifactAction {
            path: path.clone(),
            kind: artifact.kind.clone(),
            action: ArtifactDisposition::Filter,
        });
        evaluation.issues.push(ConversionIssue {
            code: "artifact_filtered".to_string(),
            severity: IssueSeverity::Warning,
            path,
            feature: None,
            target_limitation: "supplemental opaque artifact is not portable to target issuer"
                .to_string(),
            action: IssueAction::Filter,
            summary: "Optional source-channel continuation data was omitted.".to_string(),
        });
    }
    evaluation
}

pub(super) fn evaluate_response_artifacts(
    response: &CanonicalResponseV2,
    target_protocol: ProtocolKind,
    target_issuer: &IssuerIdentity,
    target_model: &str,
) -> ArtifactEvaluation {
    let mut artifacts = Vec::new();
    collect_artifacts(&response.blocks, "$", &mut artifacts);

    let mut evaluation = ArtifactEvaluation::default();
    for (path, artifact) in artifacts {
        let permitted = affinity_permits(
            &artifact.affinity,
            target_protocol,
            target_issuer,
            target_model,
        );
        let compatibility_envelope = is_portable_artifact(&artifact.kind);
        if (permitted || compatibility_envelope) && artifact.replay != ReplayPolicy::Never {
            evaluation.actions.push(ArtifactAction {
                path,
                kind: artifact.kind.clone(),
                action: ArtifactDisposition::Preserve,
            });
            continue;
        }

        // Response conversion is availability-first. A continuation artifact can be mandatory
        // when it is replayed in a later request, but omitting it from a response in another
        // protocol must not discard the otherwise usable response body. Make the loss observable
        // and let the caller's existing improvement-report path retain the diagnostic.
        evaluation.compatible = true;
        evaluation.actions.push(ArtifactAction {
            path: path.clone(),
            kind: artifact.kind.clone(),
            action: ArtifactDisposition::Filter,
        });
        evaluation.issues.push(ConversionIssue {
            code: "response_artifact_approximated".to_string(),
            severity: IssueSeverity::Warning,
            path,
            feature: None,
            target_limitation:
                "opaque response artifact is not representable by the target protocol or issuer"
                    .to_string(),
            action: IssueAction::Approximate,
            summary: "Provider-specific data cannot be represented by the target protocol; omitted it and returned the remaining response."
                .to_string(),
        });
    }
    evaluation
}

fn required_artifact_can_be_omitted_for_target(
    artifact: &OpaqueArtifact,
    target_protocol: ProtocolKind,
) -> bool {
    matches!(
        artifact.kind,
        ArtifactKind::AnthropicThinkingSignature | ArtifactKind::AnthropicRedactedThinking
    ) && target_protocol != ProtocolKind::AnthropicMessages
        || artifact.kind == ArtifactKind::GeminiThoughtSignature
}

pub(super) fn missing_gemini_signature_steps(request: &CanonicalRequestV2) -> Vec<String> {
    request
        .turns
        .iter()
        .enumerate()
        .filter(|(_, turn)| turn.role == TurnRole::Assistant)
        .filter_map(|(turn_index, turn)| {
            let (block_index, call) =
                turn.blocks
                    .iter()
                    .enumerate()
                    .find_map(|(block_index, block)| match block {
                        ContentBlock::ToolCall(call) => Some((block_index, call)),
                        _ => None,
                    })?;
            (!call
                .artifacts
                .iter()
                .any(|artifact| artifact.kind == ArtifactKind::GeminiThoughtSignature))
            .then(|| format!("turns[{turn_index}].blocks[{block_index}]"))
        })
        .collect()
}

fn collect_artifacts<'a>(
    blocks: &'a [ContentBlock],
    path: &str,
    output: &mut Vec<(String, &'a OpaqueArtifact)>,
) {
    for (index, block) in blocks.iter().enumerate() {
        let block_path = format!("{path}.blocks[{index}]");
        match block {
            ContentBlock::Reasoning(reasoning) => {
                for (artifact_index, artifact) in reasoning.artifacts.iter().enumerate() {
                    output.push((
                        format!("{block_path}.artifacts[{artifact_index}]"),
                        artifact,
                    ));
                }
            }
            ContentBlock::ToolCall(call) => {
                for (artifact_index, artifact) in call.artifacts.iter().enumerate() {
                    output.push((
                        format!("{block_path}.artifacts[{artifact_index}]"),
                        artifact,
                    ));
                }
            }
            ContentBlock::ToolResult(result) => {
                collect_artifacts(&result.content, &block_path, output)
            }
            ContentBlock::ProviderArtifact(artifact) => output.push((block_path, artifact)),
            _ => {}
        }
    }
}

pub(super) fn affinity_permits(
    affinity: &ArtifactAffinity,
    target_protocol: ProtocolKind,
    issuer: &IssuerIdentity,
    target_model: &str,
) -> bool {
    match affinity {
        ArtifactAffinity::Protocol { protocol } => *protocol == target_protocol,
        ArtifactAffinity::ProtocolModel { protocol, model } => {
            *protocol == target_protocol && model.eq_ignore_ascii_case(target_model)
        }
        ArtifactAffinity::Provider { provider } => provider == &issuer.provider,
        ArtifactAffinity::Account {
            provider,
            fingerprint,
        } => {
            provider == &issuer.provider
                && issuer.account_fingerprint.as_deref() == Some(fingerprint.as_str())
        }
        ArtifactAffinity::Endpoint {
            provider,
            fingerprint,
        } => provider == &issuer.provider && fingerprint == &issuer.endpoint_fingerprint,
        ArtifactAffinity::ModelFamily { provider, family } => {
            provider == &issuer.provider && issuer.model_family.as_deref() == Some(family.as_str())
        }
        ArtifactAffinity::ExactIssuer {
            provider,
            endpoint_fingerprint,
            account_fingerprint,
            model: expected_model,
        } => {
            provider == &issuer.provider
                && endpoint_fingerprint == &issuer.endpoint_fingerprint
                && account_fingerprint
                    .as_deref()
                    .is_none_or(|expected| issuer.account_fingerprint.as_deref() == Some(expected))
                && expected_model
                    .as_deref()
                    .is_none_or(|expected| expected == target_model)
        }
    }
}
