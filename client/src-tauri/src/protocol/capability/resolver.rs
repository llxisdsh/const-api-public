use crate::protocol::kind::ProtocolKind;
use std::time::{SystemTime, UNIX_EPOCH};

use super::{CapabilityEvidence, EvidenceSource, Feature, SupportState};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedSupport {
    pub(crate) state: SupportState,
    pub(crate) evidence: Option<CapabilityEvidence>,
}

pub(super) fn resolve_support(
    evidence: &[CapabilityEvidence],
    feature: Feature,
    model: &str,
    protocol: ProtocolKind,
) -> ResolvedSupport {
    let selected = evidence
        .iter()
        .filter(|item| item.feature == feature && item.protocol == protocol)
        .filter_map(|item| {
            model_scope_rank(item.model_pattern.as_deref(), model).map(|rank| (rank, item))
        })
        .max_by_key(|(scope_rank, item)| {
            (
                *scope_rank,
                source_rank(item.source),
                item.observed_at.unwrap_or_default(),
            )
        })
        .map(|(_, item)| item.clone());
    if let Some(evidence) = selected {
        return ResolvedSupport {
            state: evidence.state,
            evidence: Some(evidence),
        };
    }
    ResolvedSupport {
        state: SupportState::Unknown,
        evidence: None,
    }
}

fn model_scope_rank(pattern: Option<&str>, model: &str) -> Option<u8> {
    match pattern {
        None => Some(1),
        Some(pattern) if pattern.eq_ignore_ascii_case(model) => Some(3),
        Some(pattern)
            if pattern.ends_with('*')
                && model
                    .to_lowercase()
                    .starts_with(&pattern[..pattern.len() - 1].to_lowercase()) =>
        {
            Some(2)
        }
        Some(_) => None,
    }
}

fn source_rank(source: EvidenceSource) -> u8 {
    match source {
        EvidenceSource::DriverContract => 6,
        EvidenceSource::LiveProbe => 5,
        EvidenceSource::ProviderMetadata => 4,
        EvidenceSource::OfficialCatalog => 3,
        EvidenceSource::UserDeclaration => 2,
        EvidenceSource::Inferred => 1,
    }
}

const CONFIRMED_CAPABILITY_MISMATCH_TTL_SECS: u64 = 60 * 60;
const FRESH_CAPABILITY_SUCCESS_TTL_SECS: u64 = 24 * 60 * 60;

fn current_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn exact_model_evidence(evidence: &CapabilityEvidence) -> bool {
    evidence
        .model_pattern
        .as_deref()
        .is_some_and(|pattern| !pattern.trim().is_empty() && !pattern.ends_with('*'))
}

fn evidence_is_fresh(evidence: &CapabilityEvidence, now: u64, ttl_secs: u64) -> bool {
    evidence.observed_at.is_some_and(|observed_at| {
        observed_at <= now.saturating_add(60) && now.saturating_sub(observed_at) <= ttl_secs
    })
}

pub(crate) fn protocol_wire_supports(protocol: ProtocolKind, feature: Feature) -> bool {
    use Feature::*;
    use ProtocolKind::*;
    match feature {
        Stream | JsonObject | Tools | ParallelTools | ToolChoiceNone | ToolChoiceRequired
        | ToolChoiceNamed | ImageInput | Reasoning => true,
        DeveloperInstruction => matches!(protocol, OpenAiResponses | OpenAiChat),
        JsonSchema | StrictSchema => {
            matches!(
                protocol,
                OpenAiResponses | OpenAiChat | AnthropicMessages | GeminiNative
            )
        }
        ToolChoiceAllowed => matches!(protocol, OpenAiResponses | GeminiNative),
        ReasoningSummary => matches!(protocol, OpenAiResponses | GeminiNative),
        EncryptedReasoning => matches!(protocol, OpenAiResponses),
        AnthropicSignatureReplay => matches!(protocol, AnthropicMessages),
        GeminiThoughtSignatureReplay => matches!(protocol, GeminiNative),
        // Gemini cachedContent references a separately-created cache resource; it cannot encode
        // the per-block TTL directives represented by CachePolicy.
        CacheControl => matches!(protocol, AnthropicMessages),
        PromptCacheOptions | PromptCacheBreakpoint => {
            matches!(protocol, OpenAiResponses | OpenAiChat)
        }
        // Public Responses currently has no standard input-audio message shape. A
        // provider-specific Responses dialect (notably ChatGPT Codex subscriptions) may opt in
        // through exact capability evidence; the generic public input wire stays fail-closed.
        AudioInput => matches!(protocol, OpenAiChat | GeminiNative),
        // Responses already has a lossless output-audio configuration and response shape in the
        // adapter. Model evidence still decides whether a concrete upstream can honor it.
        AudioOutput => matches!(protocol, OpenAiResponses | OpenAiChat | GeminiNative),
        OutputVerbosity => matches!(protocol, OpenAiResponses | OpenAiChat),
        ResponseStorage => matches!(protocol, OpenAiResponses | OpenAiChat),
        UserIdentity => matches!(protocol, OpenAiResponses | OpenAiChat | AnthropicMessages),
        VideoInput => matches!(protocol, GeminiNative),
        FileInput => matches!(protocol, OpenAiResponses | AnthropicMessages | GeminiNative),
        HostedWebSearch | HostedFileSearch | HostedCodeExecution | HostedComputerUse
        | HostedMcp | HostedUrlContext | HostedProviderTool => {
            matches!(protocol, OpenAiResponses | GeminiNative)
        }
        CustomTools | NamespaceTools => matches!(protocol, OpenAiResponses),
        ToolDeferLoading | ToolAllowedCallers => {
            matches!(protocol, OpenAiResponses | AnthropicMessages)
        }
        ToolInputExamples | ToolEagerInputStreaming => matches!(protocol, AnthropicMessages),
        ProviderExtension => false,
    }
}

/// Features whose provider evidence should influence channel preference and temporary
/// suppression. Missing evidence remains an availability fallback when the target wire has a
/// deliberate lossless representation.
pub(crate) const fn feature_uses_provider_evidence(feature: Feature) -> bool {
    matches!(feature, Feature::FileInput)
}

/// A versioned driver contract is deterministic. A live semantic mismatch is suppressive only
/// when it is exact-model and recent. Catalog and declaration negatives can be stale or
/// incomplete, so they remain lower-priority fallbacks.
pub(crate) fn support_is_confirmed_unsupported(support: &ResolvedSupport) -> bool {
    support_is_confirmed_unsupported_at(support, current_unix())
}

fn support_is_confirmed_unsupported_at(support: &ResolvedSupport, now: u64) -> bool {
    support.state == SupportState::Unsupported
        && support
            .evidence
            .as_ref()
            .is_some_and(|evidence| match evidence.source {
                EvidenceSource::DriverContract => exact_model_evidence(evidence),
                EvidenceSource::LiveProbe => {
                    exact_model_evidence(evidence)
                        && evidence_is_fresh(evidence, now, CONFIRMED_CAPABILITY_MISMATCH_TTL_SECS)
                }
                _ => false,
            })
}

/// Lower is preferred. Confirmed negatives are returned as `None`; callers may keep unknown and
/// declared-negative paths as availability fallbacks.
pub(crate) fn support_availability_rank(support: &ResolvedSupport) -> Option<u8> {
    support_availability_rank_at(support, current_unix())
}

fn support_availability_rank_at(support: &ResolvedSupport, now: u64) -> Option<u8> {
    if support_is_confirmed_unsupported_at(support, now) {
        return None;
    }
    match support.state {
        SupportState::Supported => {
            let evidence = support.evidence.as_ref();
            let rank = evidence.map_or(6, |evidence| match evidence.source {
                EvidenceSource::LiveProbe
                    if !evidence_is_fresh(evidence, now, FRESH_CAPABILITY_SUCCESS_TTL_SECS) =>
                {
                    2
                }
                source => 6_u8.saturating_sub(source_rank(source)),
            });
            Some(rank)
        }
        SupportState::Unknown => Some(6),
        SupportState::Unsupported => Some(7),
    }
}

/// Provider/model evidence may activate only extensions for which the target adapter has a
/// deliberate lossless wire representation. Evidence must never turn an arbitrary unsupported
/// feature into an executable plan.
pub(crate) fn protocol_evidence_extension_supports(
    protocol: ProtocolKind,
    feature: Feature,
) -> bool {
    matches!(
        (protocol, feature),
        (ProtocolKind::OpenAiResponses, Feature::AudioInput)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(state: SupportState, source: EvidenceSource, observed_at: u64) -> ResolvedSupport {
        ResolvedSupport {
            state,
            evidence: Some(CapabilityEvidence::exact_model(
                Feature::FileInput,
                state,
                source,
                "model-a",
                ProtocolKind::OpenAiResponses,
                observed_at,
            )),
        }
    }

    #[test]
    fn live_negative_suppression_expires_back_to_availability() {
        let now = 10_000;
        let recent = resolved(
            SupportState::Unsupported,
            EvidenceSource::LiveProbe,
            now - CONFIRMED_CAPABILITY_MISMATCH_TTL_SECS,
        );
        assert!(support_is_confirmed_unsupported_at(&recent, now));
        assert_eq!(support_availability_rank_at(&recent, now), None);

        let stale = resolved(
            SupportState::Unsupported,
            EvidenceSource::LiveProbe,
            now - CONFIRMED_CAPABILITY_MISMATCH_TTL_SECS - 1,
        );
        assert!(!support_is_confirmed_unsupported_at(&stale, now));
        assert_eq!(support_availability_rank_at(&stale, now), Some(7));
    }

    #[test]
    fn driver_contract_is_stable_but_only_for_an_exact_model() {
        let now = 10_000;
        let contract = resolved(SupportState::Unsupported, EvidenceSource::DriverContract, 1);
        assert!(support_is_confirmed_unsupported_at(&contract, now));

        let mut broad = contract;
        broad
            .evidence
            .as_mut()
            .expect("contract evidence")
            .model_pattern = Some("model-*".to_string());
        assert!(!support_is_confirmed_unsupported_at(&broad, now));
    }

    #[test]
    fn stale_live_success_loses_priority_without_becoming_unavailable() {
        let now = 100_000;
        let fresh = resolved(
            SupportState::Supported,
            EvidenceSource::LiveProbe,
            now - FRESH_CAPABILITY_SUCCESS_TTL_SECS,
        );
        assert_eq!(support_availability_rank_at(&fresh, now), Some(1));

        let stale = resolved(
            SupportState::Supported,
            EvidenceSource::LiveProbe,
            now - FRESH_CAPABILITY_SUCCESS_TTL_SECS - 1,
        );
        assert_eq!(support_availability_rank_at(&stale, now), Some(2));
    }
}
