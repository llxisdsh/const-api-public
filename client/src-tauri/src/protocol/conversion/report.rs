use serde::{Deserialize, Serialize};
use std::fmt;

use crate::protocol::capability::Feature;
use crate::protocol::ir::ArtifactKind;
use crate::protocol::kind::ProtocolKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConversionLevel {
    Native,
    Lossless,
    Compatible,
    Lossy,
    Blocked,
}

impl fmt::Display for ConversionLevel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Native => "native",
            Self::Lossless => "lossless",
            Self::Compatible => "compatible",
            Self::Lossy => "lossy",
            Self::Blocked => "blocked",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IssueSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IssueAction {
    Preserve,
    Encode,
    Rewrite,
    Filter,
    Approximate,
    Block,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ConversionIssue {
    pub(crate) code: String,
    pub(crate) severity: IssueSeverity,
    pub(crate) path: String,
    pub(crate) feature: Option<Feature>,
    pub(crate) target_limitation: String,
    pub(crate) action: IssueAction,
    pub(crate) summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RepairClass {
    LosslessStructural,
    CompatibleAllowlisted,
    DiagnosticLossy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ConversionRepair {
    pub(crate) code: String,
    pub(crate) class: RepairClass,
    pub(crate) path: String,
    pub(crate) action: String,
    pub(crate) reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FeatureDisposition {
    Preserve,
    Encode,
    CoalesceRoleEnvelope,
    FlattenNamespace,
    ApproximateJsonSchemaAsObject,
    DropStrictness,
    OmitUnsupportedFeature,
    OmitProviderExtension,
    Block,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FeatureAction {
    pub(crate) feature: Feature,
    pub(crate) path: String,
    pub(crate) action: FeatureDisposition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ArtifactDisposition {
    Preserve,
    Filter,
    Block,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ArtifactAction {
    pub(crate) path: String,
    pub(crate) kind: ArtifactKind,
    pub(crate) action: ArtifactDisposition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CompatibleAction {
    FilterSupplementalCrossIssuerArtifact,
    CoalesceRoleEnvelope,
    FlattenNamespaceWithoutCollision,
    NormalizeRequiredEmptyContent,
}

pub(crate) fn compatible_action_allowed(action: CompatibleAction) -> bool {
    matches!(
        action,
        CompatibleAction::FilterSupplementalCrossIssuerArtifact
            | CompatibleAction::CoalesceRoleEnvelope
            | CompatibleAction::FlattenNamespaceWithoutCollision
            | CompatibleAction::NormalizeRequiredEmptyContent
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ConversionReport {
    pub(crate) source_protocol: ProtocolKind,
    pub(crate) target_protocol: ProtocolKind,
    pub(crate) level: ConversionLevel,
    #[serde(default)]
    pub(crate) issues: Vec<ConversionIssue>,
    #[serde(default)]
    pub(crate) repairs: Vec<ConversionRepair>,
}
