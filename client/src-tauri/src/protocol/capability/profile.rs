use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::protocol::kind::ProtocolKind;

use super::{Feature, ResolvedSupport, resolve_support};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EvidenceSource {
    DriverContract,
    LiveProbe,
    ProviderMetadata,
    OfficialCatalog,
    UserDeclaration,
    Inferred,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SupportState {
    Supported,
    Unsupported,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CapabilityEvidence {
    pub(crate) feature: Feature,
    pub(crate) state: SupportState,
    pub(crate) source: EvidenceSource,
    pub(crate) model_pattern: Option<String>,
    pub(crate) protocol: ProtocolKind,
    pub(crate) observed_at: Option<u64>,
}

impl CapabilityEvidence {
    #[cfg(test)]
    pub(crate) fn exact_model(
        feature: Feature,
        state: SupportState,
        source: EvidenceSource,
        model: impl Into<String>,
        protocol: ProtocolKind,
        observed_at: u64,
    ) -> Self {
        Self {
            feature,
            state,
            source,
            model_pattern: Some(model.into()),
            protocol,
            observed_at: Some(observed_at),
        }
    }

    #[cfg(test)]
    pub(crate) fn protocol_default(
        feature: Feature,
        state: SupportState,
        source: EvidenceSource,
        protocol: ProtocolKind,
        observed_at: u64,
    ) -> Self {
        Self {
            feature,
            state,
            source,
            model_pattern: None,
            protocol,
            observed_at: Some(observed_at),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub(crate) struct CapabilityProfile {
    #[serde(default)]
    pub(crate) evidence: Vec<CapabilityEvidence>,
    #[serde(default)]
    required_features: BTreeSet<Feature>,
}

impl CapabilityProfile {
    pub(crate) fn new(evidence: Vec<CapabilityEvidence>) -> Self {
        Self {
            evidence,
            required_features: BTreeSet::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn require(&mut self, feature: Feature) {
        self.required_features.insert(feature);
    }

    pub(crate) fn requires(&self, feature: Feature) -> bool {
        self.required_features.contains(&feature)
    }

    pub(crate) fn resolve(
        &self,
        feature: Feature,
        model: &str,
        protocol: ProtocolKind,
    ) -> ResolvedSupport {
        resolve_support(&self.evidence, feature, model, protocol)
    }
}
