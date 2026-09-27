use serde::{Deserialize, Serialize};

use crate::protocol::kind::ProtocolKind;

use super::{BlockMetadata, TextBlock};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ArtifactKind {
    AnthropicThinkingSignature,
    AnthropicRedactedThinking,
    GeminiThoughtSignature,
    #[serde(rename = "openai_encrypted_reasoning")]
    OpenAiEncryptedReasoning,
    ContinuationToken,
    ProviderSpecific,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub(crate) enum ArtifactAffinity {
    Protocol {
        protocol: ProtocolKind,
    },
    ProtocolModel {
        protocol: ProtocolKind,
        model: String,
    },
    Provider {
        provider: String,
    },
    Account {
        provider: String,
        fingerprint: String,
    },
    Endpoint {
        provider: String,
        fingerprint: String,
    },
    ModelFamily {
        provider: String,
        family: String,
    },
    ExactIssuer {
        provider: String,
        endpoint_fingerprint: String,
        account_fingerprint: Option<String>,
        model: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReplayPolicy {
    ExactWhenCompatible,
    FilterWhenIncompatible,
    Required,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ArtifactCriticality {
    Supplemental,
    Required,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct OpaqueArtifact {
    pub(crate) kind: ArtifactKind,
    pub(crate) payload: serde_json::Value,
    pub(crate) affinity: ArtifactAffinity,
    pub(crate) replay: ReplayPolicy,
    pub(crate) criticality: ArtifactCriticality,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ReasoningBlock {
    pub(crate) text: Option<String>,
    #[serde(default)]
    pub(crate) summary: Vec<TextBlock>,
    #[serde(default)]
    pub(crate) artifacts: Vec<OpaqueArtifact>,
    pub(crate) encrypted: bool,
    #[serde(default)]
    pub(crate) metadata: BlockMetadata,
}
