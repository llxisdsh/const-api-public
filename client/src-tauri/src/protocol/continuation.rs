use std::collections::HashSet;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::protocol::ir::{
    ArtifactAffinity, ArtifactCriticality, ArtifactKind, ContentBlockKind, OpaqueArtifact,
    ReplayPolicy,
};
use crate::protocol::kind::ProtocolKind;

const CARRIER_PREFIX: &str = "const-api-continuation-v1:";
const MAX_CARRIER_BYTES: usize = 2 * 1024 * 1024;
const MAX_CARRIER_ENTRIES: usize = 8;
const MAX_CALL_ID_BYTES: usize = 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContinuationCarrier {
    pub(crate) entries: Vec<ContinuationEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContinuationEntry {
    pub(crate) owner: ContinuationOwner,
    pub(crate) artifact: OpaqueArtifact,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "owner", rename_all = "snake_case")]
pub(crate) enum ContinuationOwner {
    Reasoning,
    ToolCall { call_id: String },
    PreviousPart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CarrierDecodeError {
    TooLarge,
    InvalidEncoding,
    InvalidEnvelope,
}

impl CarrierDecodeError {
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::TooLarge => "continuation carrier exceeds the supported size",
            Self::InvalidEncoding => "continuation carrier is not valid base64url",
            Self::InvalidEnvelope => "continuation carrier has an invalid or unsafe envelope",
        }
    }
}

pub(crate) fn is_portable_artifact(kind: &ArtifactKind) -> bool {
    matches!(
        kind,
        ArtifactKind::AnthropicThinkingSignature
            | ArtifactKind::AnthropicRedactedThinking
            | ArtifactKind::GeminiThoughtSignature
            | ArtifactKind::OpenAiEncryptedReasoning
            | ArtifactKind::ContinuationToken
    )
}

pub(crate) fn reasoning_artifacts_are_encrypted(artifacts: &[OpaqueArtifact]) -> bool {
    artifacts.iter().any(|artifact| {
        matches!(
            artifact.kind,
            ArtifactKind::OpenAiEncryptedReasoning | ArtifactKind::AnthropicRedactedThinking
        )
    })
}

pub(crate) fn encode_carrier(entries: Vec<ContinuationEntry>) -> Option<String> {
    let carrier = ContinuationCarrier { entries };
    validate_carrier(&carrier).ok()?;
    let encoded = serde_json::to_vec(&carrier).ok()?;
    if encoded.len() > MAX_CARRIER_BYTES {
        return None;
    }
    Some(format!(
        "{CARRIER_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(encoded)
    ))
}

pub(crate) fn decode_carrier(
    value: &Value,
) -> Result<Option<ContinuationCarrier>, CarrierDecodeError> {
    let Some(raw) = value.as_str() else {
        return Ok(None);
    };
    let raw = raw.trim();
    let Some(encoded) = raw.strip_prefix(CARRIER_PREFIX) else {
        return Ok(None);
    };
    if encoded.len() > MAX_CARRIER_BYTES.saturating_mul(4) / 3 + 16 {
        return Err(CarrierDecodeError::TooLarge);
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| CarrierDecodeError::InvalidEncoding)?;
    if decoded.len() > MAX_CARRIER_BYTES {
        return Err(CarrierDecodeError::TooLarge);
    }
    let carrier = serde_json::from_slice::<ContinuationCarrier>(&decoded)
        .map_err(|_| CarrierDecodeError::InvalidEnvelope)?;
    validate_carrier(&carrier)?;
    Ok(Some(carrier))
}

pub(crate) fn raw_contains_carrier(value: &str) -> bool {
    value.contains(CARRIER_PREFIX)
}

pub(crate) fn value_contains_carrier(value: &Value) -> bool {
    let mut pending = vec![value];
    let mut visited = 0_usize;
    while let Some(value) = pending.pop() {
        visited = visited.saturating_add(1);
        if visited > 100_000 {
            // A very large document should take the validating path rather than risk forwarding
            // an unexamined CONST-owned envelope to an upstream provider.
            return true;
        }
        match value {
            Value::String(value) if value.trim_start().starts_with(CARRIER_PREFIX) => return true,
            Value::Array(values) => pending.extend(values),
            Value::Object(values) => pending.extend(values.values()),
            _ => {}
        }
    }
    false
}

pub(crate) fn reasoning_entries(artifacts: &[OpaqueArtifact]) -> Vec<ContinuationEntry> {
    entries_for_owner(artifacts, ContinuationOwner::Reasoning)
}

pub(crate) fn tool_call_entries(
    call_id: &str,
    artifacts: &[OpaqueArtifact],
) -> Vec<ContinuationEntry> {
    entries_for_owner(
        artifacts,
        ContinuationOwner::ToolCall {
            call_id: call_id.to_string(),
        },
    )
}

pub(crate) fn previous_part_entries(artifacts: &[OpaqueArtifact]) -> Vec<ContinuationEntry> {
    entries_for_owner(artifacts, ContinuationOwner::PreviousPart)
}

fn entries_for_owner(
    artifacts: &[OpaqueArtifact],
    owner: ContinuationOwner,
) -> Vec<ContinuationEntry> {
    artifacts
        .iter()
        .filter(|artifact| is_portable_artifact(&artifact.kind))
        .cloned()
        .map(|artifact| ContinuationEntry {
            owner: owner.clone(),
            artifact,
        })
        .collect()
}

pub(crate) fn stream_artifact(
    kind: ArtifactKind,
    payload: Value,
    owner: ContentBlockKind,
    model: Option<&str>,
) -> OpaqueArtifact {
    let protocol = artifact_protocol(&kind);
    let required = matches!(
        kind,
        ArtifactKind::AnthropicThinkingSignature | ArtifactKind::AnthropicRedactedThinking
    ) || (kind == ArtifactKind::GeminiThoughtSignature
        && owner == ContentBlockKind::ToolCall);
    OpaqueArtifact {
        kind,
        payload,
        affinity: model
            .filter(|model| !model.trim().is_empty())
            .map(|model| ArtifactAffinity::ProtocolModel {
                protocol,
                model: model.to_string(),
            })
            .unwrap_or(ArtifactAffinity::Protocol { protocol }),
        replay: if required {
            ReplayPolicy::Required
        } else {
            ReplayPolicy::ExactWhenCompatible
        },
        criticality: if required {
            ArtifactCriticality::Required
        } else {
            ArtifactCriticality::Supplemental
        },
    }
}

pub(crate) const fn artifact_protocol(kind: &ArtifactKind) -> ProtocolKind {
    match kind {
        ArtifactKind::AnthropicThinkingSignature | ArtifactKind::AnthropicRedactedThinking => {
            ProtocolKind::AnthropicMessages
        }
        ArtifactKind::GeminiThoughtSignature => ProtocolKind::GeminiNative,
        ArtifactKind::OpenAiEncryptedReasoning | ArtifactKind::ContinuationToken => {
            ProtocolKind::OpenAiResponses
        }
        ArtifactKind::ProviderSpecific => ProtocolKind::OpenAiResponses,
    }
}

fn validate_carrier(carrier: &ContinuationCarrier) -> Result<(), CarrierDecodeError> {
    if carrier.entries.is_empty() || carrier.entries.len() > MAX_CARRIER_ENTRIES {
        return Err(CarrierDecodeError::InvalidEnvelope);
    }
    let mut owners = HashSet::new();
    for entry in &carrier.entries {
        if !is_portable_artifact(&entry.artifact.kind)
            || entry.artifact.replay == ReplayPolicy::Never
        {
            return Err(CarrierDecodeError::InvalidEnvelope);
        }
        let payload = entry
            .artifact
            .payload
            .as_str()
            .filter(|payload| !payload.trim().is_empty())
            .ok_or(CarrierDecodeError::InvalidEnvelope)?;
        if payload.trim_start().starts_with(CARRIER_PREFIX) {
            return Err(CarrierDecodeError::InvalidEnvelope);
        }
        let owner_key = match &entry.owner {
            ContinuationOwner::Reasoning => format!("reasoning:{:?}", entry.artifact.kind),
            ContinuationOwner::ToolCall { call_id } => {
                if call_id.trim().is_empty() || call_id.len() > MAX_CALL_ID_BYTES {
                    return Err(CarrierDecodeError::InvalidEnvelope);
                }
                format!("tool:{call_id}:{:?}", entry.artifact.kind)
            }
            ContinuationOwner::PreviousPart => {
                format!("previous_part:{:?}", entry.artifact.kind)
            }
        };
        if !owners.insert(owner_key) {
            return Err(CarrierDecodeError::InvalidEnvelope);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn gemini_entry(call_id: &str) -> ContinuationEntry {
        ContinuationEntry {
            owner: ContinuationOwner::ToolCall {
                call_id: call_id.to_string(),
            },
            artifact: stream_artifact(
                ArtifactKind::GeminiThoughtSignature,
                json!("provider-signature"),
                ContentBlockKind::ToolCall,
                Some("gemini-test"),
            ),
        }
    }

    #[test]
    fn carrier_round_trips_without_losing_owner_or_affinity() {
        let entry = gemini_entry("call-1");
        let encoded = encode_carrier(vec![entry.clone()]).expect("carrier");
        let decoded = decode_carrier(&json!(encoded))
            .expect("valid carrier")
            .expect("marked carrier");
        assert_eq!(decoded.entries, vec![entry]);
    }

    #[test]
    fn carrier_rejects_malformed_nested_and_duplicate_entries() {
        assert_eq!(
            decode_carrier(&json!(format!("{CARRIER_PREFIX}not-base64"))),
            Err(CarrierDecodeError::InvalidEncoding)
        );

        let nested = encode_carrier(vec![gemini_entry("call-1")]).expect("carrier");
        let mut entry = gemini_entry("call-2");
        entry.artifact.payload = json!(nested);
        assert!(encode_carrier(vec![entry]).is_none());

        let duplicate = gemini_entry("call-3");
        assert!(encode_carrier(vec![duplicate.clone(), duplicate]).is_none());
    }

    #[test]
    fn ordinary_provider_payload_is_not_a_carrier() {
        assert_eq!(decode_carrier(&json!("provider-opaque")), Ok(None));
        assert_eq!(decode_carrier(&json!({"opaque":true})), Ok(None));
    }
}
