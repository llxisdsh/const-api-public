//! Shared model-scoped checks and retention of incomplete feature evidence.
use super::*;

fn uses_model_scoped_checks(channel: &ChannelConfig) -> bool {
    channel.source_driver() == crate::source_driver::SourceDriverId::Openrouter
        || crate::coding_gateway::is_gateway(channel.source_driver())
}

pub(super) fn probe_uses_named_tool_choice(
    channel: &ChannelConfig,
    catalog: &[ModelObservation],
    model: &str,
) -> bool {
    if crate::protocol::anthropic_dialect::anthropic_model_dialect(model)
        .forced_tool_choice_unsupported
    {
        return false;
    }
    !uses_model_scoped_checks(channel)
        || catalog
            .iter()
            .find(|entry| entry.id.eq_ignore_ascii_case(model))
            .and_then(|entry| entry.supported_parameters.as_ref())
            .is_none_or(|parameters| {
                parameters
                    .iter()
                    .any(|parameter| parameter == "tool_choice")
            })
}

/// One aggregated model's semantic check is not evidence for all other models.
pub(super) fn scope_probed_model_features(
    channel: &ChannelConfig,
    model: &str,
    profiles: &mut Vec<ChannelCapabilityProfile>,
) {
    if !uses_model_scoped_checks(channel) {
        return;
    }
    let mut scoped = Vec::new();
    for profile in profiles
        .iter_mut()
        .filter(|profile| profile.model_pattern.is_empty())
    {
        if profile.tool_calls || profile.tool_choice || profile.json_schema {
            scoped.push(ChannelCapabilityProfile {
                protocol: profile.protocol.clone(),
                model_pattern: model.to_string(),
                tool_calls: std::mem::take(&mut profile.tool_calls),
                tool_choice: std::mem::take(&mut profile.tool_choice),
                json_schema: std::mem::take(&mut profile.json_schema),
                verification_state: "verified".into(),
                verified_at_unix: profile.verified_at_unix,
                ..Default::default()
            });
        }
    }
    profiles.extend(scoped);
}

#[derive(Clone, Copy)]
pub(super) enum Feature {
    Stream,
    Tools,
    JsonSchema,
    CacheControl,
    CustomTool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_55_probes_do_not_force_an_unsupported_tool_choice() {
        let channel = channel_from_supplier("claude-55-probe".into(), &default_supplier_config());
        for model in [
            "claude-sonnet-5-5",
            "anthropic/claude-sonnet-5.5",
            "claude-opus-5-5",
        ] {
            assert!(!probe_uses_named_tool_choice(&channel, &[], model));
        }
        assert!(probe_uses_named_tool_choice(
            &channel,
            &[],
            "claude-sonnet-5"
        ));
    }

    #[test]
    fn named_tool_choice_follows_aggregator_model_metadata() {
        use crate::source_driver::{ALL_SOURCE_DRIVER_IDS, SourceDriverId};
        let catalog = vec![ModelObservation {
            id: "vendor/tool-model".into(),
            supported_parameters: Some(vec!["tools".into()]),
            ..Default::default()
        }];
        let mut channel = channel_from_supplier("probe-choice".into(), &default_supplier_config());
        for driver in ALL_SOURCE_DRIVER_IDS {
            channel.set_source_driver(driver);
            assert_eq!(
                probe_uses_named_tool_choice(&channel, &catalog, "vendor/tool-model"),
                driver != SourceDriverId::Openrouter && !crate::coding_gateway::is_gateway(driver)
            );
            assert!(probe_uses_named_tool_choice(
                &channel,
                &[],
                "vendor/tool-model"
            ));
        }
    }

    #[test]
    fn semantic_proof_is_model_scoped_for_aggregated_gateways() {
        use crate::source_driver::{ALL_SOURCE_DRIVER_IDS, SourceDriverId};
        let generic = ChannelCapabilityProfile {
            protocol: "openai_chat".into(),
            tool_calls: true,
            tool_choice: true,
            json_schema: true,
            stream_sse: true,
            verification_state: "verified".into(),
            verified_at_unix: 42,
            ..Default::default()
        };
        let mut channel = channel_from_supplier("probe-scope".into(), &default_supplier_config());
        for driver in ALL_SOURCE_DRIVER_IDS {
            channel.set_source_driver(driver);
            let mut profiles = vec![generic.clone()];
            scope_probed_model_features(&channel, "vendor/selected", &mut profiles);
            assert!(profiles[0].stream_sse);
            let aggregated =
                driver == SourceDriverId::Openrouter || crate::coding_gateway::is_gateway(driver);
            assert_eq!(profiles[0].tool_calls, !aggregated);
            if aggregated {
                assert_eq!(profiles.len(), 2);
                assert_eq!(profiles[1].model_pattern, "vendor/selected");
                assert!(
                    profiles[1].tool_calls && profiles[1].tool_choice && profiles[1].json_schema
                );
                assert_eq!(profiles[1].verification_state, "verified");
                assert_eq!(profiles[1].verified_at_unix, 42);
                scope_probed_model_features(&channel, "vendor/selected", &mut profiles);
                assert_eq!(profiles.len(), 2);
            } else {
                assert_eq!(profiles, vec![generic.clone()]);
            }
        }
    }

    fn budget_error() -> anyhow::Error {
        crate::upstream_transport::UpstreamTransportError::ProbeBudgetExceeded.into()
    }

    #[test]
    fn retained_features_stay_protocol_and_model_scoped() {
        let mut channel =
            channel_from_supplier("retained-feature-scope".into(), &default_supplier_config());
        channel.set_source_driver(crate::source_driver::SourceDriverId::Openrouter);
        channel.capability_profiles = ["", "selected"]
            .into_iter()
            .map(|model| ChannelCapabilityProfile {
                protocol: "openai_chat".into(),
                model_pattern: model.into(),
                tool_calls: true,
                tool_choice: true,
                json_schema: true,
                verification_state: "verified".into(),
                ..Default::default()
            })
            .collect();
        for (protocol, model, expected) in [
            ("openai_chat", "vendor/selected", true),
            ("openai_chat", "different", false),
            ("openai_responses", "selected", false),
        ] {
            let mut profile = basic_protocol_capability(protocol, "verified");
            retain_incomplete(
                &channel,
                model,
                &mut profile,
                Feature::Tools,
                &budget_error(),
            );
            retain_incomplete(
                &channel,
                model,
                &mut profile,
                Feature::JsonSchema,
                &budget_error(),
            );
            assert_eq!(profile.tool_calls, expected);
            assert_eq!(profile.tool_choice, expected);
            assert_eq!(profile.json_schema, expected);
            let mut profiles = vec![profile];
            scope_probed_model_features(&channel, model, &mut profiles);
            assert!(!profiles[0].tool_calls && !profiles[0].json_schema);
            if expected {
                assert_eq!(profiles[1].model_pattern, model);
            }
        }
    }

    #[test]
    fn explicit_failure_or_unverified_evidence_is_never_retained() {
        let mut channel =
            channel_from_supplier("retained-feature-state".into(), &default_supplier_config());
        for (state, release_status, catalog_metadata, expected) in [
            ("verified", "", false, true),
            ("declared", "", false, false),
            ("rejected", "", false, false),
            ("verified", "prepared", false, false),
            ("verified", "suspended", false, false),
            ("verified", "", true, false),
        ] {
            channel.capability_profiles = vec![ChannelCapabilityProfile {
                protocol: "anthropic_messages".into(),
                cache_control: true,
                verification_state: state.into(),
                release_status: release_status.into(),
                catalog_metadata,
                ..Default::default()
            }];
            let mut profile = basic_protocol_capability("anthropic_messages", "verified");
            retain_incomplete(
                &channel,
                "claude-test",
                &mut profile,
                Feature::CacheControl,
                &anyhow!("cache_control is unsupported"),
            );
            assert!(!profile.cache_control);
            retain_incomplete(
                &channel,
                "claude-test",
                &mut profile,
                Feature::CacheControl,
                &budget_error(),
            );
            assert_eq!(profile.cache_control, expected);
        }
    }

    #[test]
    fn interrupted_stream_check_preserves_explicit_unsupported_evidence() {
        let mut channel =
            channel_from_supplier("retained-stream-state".into(), &default_supplier_config());
        channel.capability_profiles = vec![ChannelCapabilityProfile {
            protocol: "openai_chat".into(),
            stream_sse: false,
            stream_sse_unsupported: true,
            verification_state: "verified".into(),
            ..Default::default()
        }];
        let mut profile = basic_protocol_capability("openai_chat", "verified");
        retain_incomplete(
            &channel,
            "selected",
            &mut profile,
            Feature::Stream,
            &budget_error(),
        );
        assert!(!profile.stream_sse && profile.stream_sse_unsupported);
    }
}

pub(super) fn retain_incomplete(
    channel: &ChannelConfig,
    model: &str,
    current: &mut ChannelCapabilityProfile,
    feature: Feature,
    error: &anyhow::Error,
) {
    if !check_deadline::incomplete(error) {
        return;
    }
    // Aggregated gateways move tool/schema checks to an exact-model profile after
    // enrichment. Do not restore another model's evidence into that profile.
    let model_scoped = !current.model_pattern.is_empty()
        || (uses_model_scoped_checks(channel)
            && matches!(feature, Feature::Tools | Feature::JsonSchema));
    let pattern = if model_scoped {
        crate::config::public_model_name(model)
    } else {
        String::new()
    };
    let previous = channel
        .capability_profiles
        .iter()
        .filter(|profile| {
            profile.protocol == current.protocol
                && !profile.catalog_metadata
                && profile.verification_state == "verified"
                && !matches!(profile.release_status.as_str(), "prepared" | "suspended")
                && crate::config::public_model_name(&profile.model_pattern) == pattern
        })
        .max_by_key(|profile| profile.verified_at_unix);
    let Some(previous) = previous else {
        return;
    };
    match feature {
        Feature::Stream => {
            current.stream_sse = previous.stream_sse;
            current.stream_sse_unsupported = previous.stream_sse_unsupported;
        }
        Feature::Tools => {
            current.tool_calls = previous.tool_calls;
            current.tool_choice = previous.tool_choice;
        }
        Feature::JsonSchema => current.json_schema = previous.json_schema,
        Feature::CacheControl => current.cache_control = previous.cache_control,
        Feature::CustomTool => {
            current.custom_tool = previous.custom_tool;
            current.verified_at_unix = previous.verified_at_unix;
        }
    }
}
