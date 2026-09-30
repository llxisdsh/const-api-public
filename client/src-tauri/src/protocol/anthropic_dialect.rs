#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AnthropicModelDialect {
    pub(crate) adaptive_thinking_only: bool,
    pub(crate) thinking_always_on: bool,
    pub(crate) thinking_between_tools: bool,
    pub(crate) forced_tool_choice_unsupported: bool,
}

/// Returns only model-generation constraints documented by Anthropic.
///
/// Unknown and older models deliberately keep the legacy behavior. Native
/// Anthropic requests bypass protocol re-encoding. These constraints apply to
/// cross-protocol conversion and to probes synthesized by CONST API.
pub(crate) fn anthropic_model_dialect(model: &str) -> AnthropicModelDialect {
    let model = model
        .trim()
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let model = model.replace('.', "-");
    // Both 5.5 families reject forced tool choice. Only Sonnet 5.5 offers
    // between_tools as the replacement for an explicit disabled request.
    let opus_55 = model_family_matches(&model, "claude-opus-5-5");
    let sonnet_55 = model_family_matches(&model, "claude-sonnet-5-5");
    if opus_55 || sonnet_55 {
        return AnthropicModelDialect {
            adaptive_thinking_only: true,
            thinking_always_on: true,
            thinking_between_tools: sonnet_55,
            forced_tool_choice_unsupported: true,
        };
    }
    let always_on = model.starts_with("claude-fable-5")
        || model.starts_with("claude-mythos-5")
        || model.starts_with("claude-mythos-preview");
    if always_on {
        return AnthropicModelDialect {
            adaptive_thinking_only: true,
            thinking_always_on: true,
            ..Default::default()
        };
    }

    let adaptive_thinking_only = ["claude-opus-", "claude-sonnet-"]
        .into_iter()
        .find_map(|prefix| anthropic_numbered_generation(&model, prefix))
        .is_some_and(|(major, minor)| major > 4 || (major == 4 && minor >= 7));
    AnthropicModelDialect {
        adaptive_thinking_only,
        thinking_always_on: false,
        ..Default::default()
    }
}

/// Returns whether Anthropic currently documents server-side compaction for this
/// model family. Keep this deliberately fail-closed: an unknown or older model
/// must not be advertised to the marketplace as compaction-capable and then fail
/// with an upstream 400 after selection.
pub(crate) fn anthropic_model_supports_server_side_compaction(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    if model_family_matches(&model, "claude-fable-5")
        || model_family_matches(&model, "claude-mythos-5")
        || model_family_matches(&model, "claude-mythos-preview")
    {
        return true;
    }

    if let Some((major, minor)) = anthropic_numbered_generation(&model, "claude-opus-") {
        return major == 5 || (major == 4 && (6..=8).contains(&minor));
    }
    if let Some((major, minor)) = anthropic_numbered_generation(&model, "claude-sonnet-") {
        return major == 5 || (major == 4 && minor == 6);
    }
    false
}

/// Returns whether Anthropic rejects a trailing assistant message used as a
/// response prefill for this model family.
///
/// Native Anthropic requests are still passed through unchanged. This guard is
/// used only after another protocol has been converted into Anthropic Messages,
/// where retaining the unsupported prefill would turn an otherwise executable
/// request into an upstream 400.
pub(crate) fn anthropic_model_rejects_assistant_prefill(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    if model_family_matches(&model, "claude-fable-5")
        || model_family_matches(&model, "claude-mythos-5")
        || model_family_matches(&model, "claude-mythos-preview")
    {
        return true;
    }

    ["claude-opus-", "claude-sonnet-", "claude-haiku-"]
        .into_iter()
        .find_map(|prefix| anthropic_numbered_generation(&model, prefix))
        .is_some_and(|(major, minor)| major > 4 || (major == 4 && minor >= 6))
}

fn model_family_matches(model: &str, family: &str) -> bool {
    model == family
        || model
            .strip_prefix(family)
            .is_some_and(|rest| rest.starts_with('-'))
}

fn anthropic_numbered_generation(model: &str, prefix: &str) -> Option<(u32, u32)> {
    let suffix = model.strip_prefix(prefix)?;
    let mut parts = suffix.split('-');
    let major = parts.next()?.parse().ok()?;
    let minor = parts
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or_default();
    Some((major, minor))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_55_constraints_cover_native_and_aggregator_names() {
        for model in [
            "claude-sonnet-5-5",
            "anthropic/claude-sonnet-5.5",
            "claude-sonnet-5-5-20260929",
        ] {
            let dialect = anthropic_model_dialect(model);
            assert!(dialect.thinking_between_tools && dialect.thinking_always_on);
            assert!(dialect.forced_tool_choice_unsupported);
        }
        let opus = anthropic_model_dialect("claude-opus-5-5");
        assert!(opus.thinking_always_on && opus.forced_tool_choice_unsupported);
        assert!(!opus.thinking_between_tools);
        assert!(!anthropic_model_dialect("claude-sonnet-5").forced_tool_choice_unsupported);
    }

    #[test]
    fn classifies_only_documented_adaptive_generations() {
        for model in [
            "claude-opus-4-7",
            "claude-opus-4-8-20260801",
            "claude-opus-5",
            "claude-sonnet-5",
        ] {
            let dialect = anthropic_model_dialect(model);
            assert!(dialect.adaptive_thinking_only, "model={model}");
            assert!(!dialect.thinking_always_on, "model={model}");
        }
        for model in ["claude-opus-4-6", "claude-sonnet-4-5", "custom-claude"] {
            assert_eq!(
                anthropic_model_dialect(model),
                AnthropicModelDialect::default(),
                "model={model}"
            );
        }
    }

    #[test]
    fn classifies_fable_and_mythos_as_always_on() {
        for model in [
            "claude-fable-5",
            "claude-fable-5-20260801",
            "claude-mythos-5",
            "claude-mythos-preview",
        ] {
            let dialect = anthropic_model_dialect(model);
            assert!(dialect.adaptive_thinking_only, "model={model}");
            assert!(dialect.thinking_always_on, "model={model}");
        }
    }

    #[test]
    fn server_side_compaction_is_limited_to_documented_model_families() {
        for model in [
            "claude-fable-5",
            "claude-fable-5-20260801",
            "claude-mythos-5",
            "claude-mythos-preview",
            "claude-opus-4-6",
            "claude-opus-4-7",
            "claude-opus-4-8-20260801",
            "claude-opus-5",
            "claude-sonnet-4-6",
            "claude-sonnet-5",
        ] {
            assert!(
                anthropic_model_supports_server_side_compaction(model),
                "model={model}"
            );
        }
        for model in [
            "claude-haiku-4-5",
            "claude-sonnet-4-5",
            "claude-opus-4-5",
            "claude-fable-50",
            "custom-claude",
        ] {
            assert!(
                !anthropic_model_supports_server_side_compaction(model),
                "model={model}"
            );
        }
    }

    #[test]
    fn assistant_prefill_rejection_tracks_claude_4_6_and_newer() {
        for model in [
            "claude-opus-4-6",
            "claude-opus-5",
            "claude-sonnet-4-6-20260801",
            "claude-haiku-4-7",
            "claude-fable-5",
            "claude-mythos-5",
            "claude-mythos-preview",
        ] {
            assert!(
                anthropic_model_rejects_assistant_prefill(model),
                "model={model}"
            );
        }
        for model in [
            "claude-opus-4-5",
            "claude-sonnet-4-5",
            "claude-haiku-4-5",
            "claude-fable-50",
            "custom-claude",
        ] {
            assert!(
                !anthropic_model_rejects_assistant_prefill(model),
                "model={model}"
            );
        }
    }
}
