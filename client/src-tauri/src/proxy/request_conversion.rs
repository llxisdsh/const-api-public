#[cfg(test)]
pub(crate) fn upstream_request_for_api_format(
    api_format: &str,
    path: &str,
    body: &str,
    upstream_model: &str,
) -> Result<(String, String, String, String)> {
    let result = upstream_request_for_api_format_with_capability_profile(
        api_format,
        path,
        body,
        upstream_model,
        &crate::protocol::capability::CapabilityProfile::default(),
    )?;
    Ok((
        result.path,
        result.body,
        result.inbound_protocol,
        result.target_protocol,
    ))
}

pub(crate) struct UpstreamRequestConversion {
    pub(crate) tool_mapping: crate::protocol::conversion::ToolWireMap,
    pub(crate) path: String,
    pub(crate) body: String,
    pub(crate) inbound_protocol: String,
    pub(crate) target_protocol: String,
    pub(crate) faults: Vec<crate::supplier::ImprovementFault>,
}

pub(crate) fn upstream_request_for_api_format_with_profiles_report(
    api_format: &str,
    path: &str,
    body: &str,
    upstream_model: &str,
    profiles: &[ChannelCapabilityProfile],
) -> Result<UpstreamRequestConversion> {
    let target = crate::protocol::kind::ProtocolKind::parse(&protocol_from_api_format(api_format))
        .map_err(|error| anyhow!(error))?;
    let profile = capability_profile_from_channel_profiles(profiles, target, upstream_model);
    upstream_request_for_api_format_with_capability_profile(
        api_format,
        path,
        body,
        upstream_model,
        &profile,
    )
}

fn upstream_request_for_api_format_with_capability_profile(
    api_format: &str,
    path: &str,
    body: &str,
    upstream_model: &str,
    target_profile: &crate::protocol::capability::CapabilityProfile,
) -> Result<UpstreamRequestConversion> {
    let inbound = crate::protocol::kind::ProtocolKind::parse(&protocol_from_path(path))
        .map_err(|error| anyhow!(error))?;
    let target = crate::protocol::kind::ProtocolKind::parse(&protocol_from_api_format(api_format))
        .map_err(|error| anyhow!(error))?;
    if inbound == crate::protocol::kind::ProtocolKind::AnthropicMessages
        && target != crate::protocol::kind::ProtocolKind::AnthropicMessages
        && crate::supplier::claude_request_uses_server_side_compaction(body)
    {
        let requested_model = requested_model_from_body_or_path(body, path)
            .filter(|model| !model.trim().is_empty())
            .unwrap_or_else(|| upstream_model.to_string());
        return Err(anyhow!(
            serde_json::json!({
                "error": "unsupported_required_feature: Claude server-side compaction requires a native anthropic_messages target",
                "error_kind": "capability_mismatch",
                "issue_code": "unsupported_required_feature",
                "feature": "server_side_compaction",
                "protocol": inbound.as_str(),
                "target_protocol": target.as_str(),
                "model": requested_model,
                "safe_to_retry_other_channel": true
            })
            .to_string()
        ));
    }
    let stream_requested = crate::supplier::top_level_json_bool(body.as_bytes(), "stream")
        .unwrap_or_else(|| path.contains(":streamGenerateContent"));
    if inbound == target && !crate::protocol::continuation::raw_contains_carrier(body) {
        let mut value: serde_json::Value = serde_json::from_str(body)?;
        validate_same_protocol_evidence_extensions(
            inbound,
            &value,
            upstream_model,
            target_profile,
        )?;
        // Gemini generation uses the action name to select stream shape, but countTokens and
        // embedContent are distinct operations. Replacing every Gemini action with generation
        // silently changes semantics even though no protocol conversion was requested.
        let upstream_path = if target == crate::protocol::kind::ProtocolKind::GeminiNative
            && is_gemini_generation_operation(path)
        {
            target.path(upstream_model, stream_requested)
        } else {
            supplier_upstream_path_for_format(
                api_format,
                crate::surface::provider_relative_path(path),
                upstream_model,
            )
        };
        // The local client and the selected channel are different issuers even
        // when both use the Chat wire shape. Plain `reasoning_content` has no
        // portable signature envelope, so keep the visible answer/tool history
        // and remove only that hidden, non-standard history field.
        let removed_reasoning = if inbound == crate::protocol::kind::ProtocolKind::OpenAiChat {
            crate::protocol::conversion::strip_unsigned_chat_reasoning_history(&mut value)
        } else {
            Vec::new()
        };
        let upstream_body = if removed_reasoning.is_empty() {
            // Same-protocol requests are a transparent pipe. Keep unknown fields,
            // ordering, and whitespace byte-stable when the only owned change is
            // the selected model value.
            rewrite_supplier_body_model(body, upstream_model)
        } else {
            // Filtering unauthenticated hidden reasoning is the exceptional
            // semantic change that requires rebuilding the JSON document.
            rewrite_supplier_body_model(&serde_json::to_string(&value)?, upstream_model)
        };
        if let Some(sample_path) = removed_reasoning.first() {
            log::warn!(
                "[const-api][protocol] conversion warning code=reasoning_history_omitted source={} target={} model={} occurrences={} sample_path={} summary=Unsigned Chat reasoning history was omitted; visible assistant content and tool calls were preserved.",
                inbound.as_str(), target.as_str(), upstream_model, removed_reasoning.len(), sample_path
            );
        }
        let faults = removed_reasoning
            .into_iter()
            .map(|field_path| {
                let summary = "Unsigned Chat reasoning history was omitted; visible assistant content and tool calls were preserved.";
                crate::supplier::ImprovementFault::conversion_warning(
                    "reasoning_history_omitted",
                    inbound.as_str(),
                    target.as_str(),
                    upstream_model,
                    &field_path,
                    summary,
                )
            })
            .collect();
        return Ok(UpstreamRequestConversion {
            tool_mapping: Default::default(),
            path: upstream_path,
            body: upstream_body,
            inbound_protocol: inbound.as_str().to_string(),
            target_protocol: target.as_str().to_string(),
            faults,
        });
    }

    let value = serde_json::from_str(body)?;
    let requested_model = requested_model_from_body_or_path(body, path)
        .filter(|model| !model.trim().is_empty())
        .unwrap_or_else(|| upstream_model.to_string());
    let source_context = crate::protocol::adapters::AdapterContext::runtime(
        format!("inbound:{}", inbound.as_str()),
        "runtime-inbound",
        None,
        &requested_model,
    );
    let target_context = crate::protocol::adapters::AdapterContext::runtime(
        format!("channel:{api_format}"),
        format!("runtime-target:{api_format}"),
        None,
        upstream_model,
    );
    let conversion = crate::protocol::convert_non_stream(
        inbound,
        target,
        &value,
        &source_context,
        &target_context,
        target_profile,
        crate::protocol::conversion::ConversionPolicy::Production,
    )
    .map_err(|error| {
        let mismatch = error.report().and_then(|report| {
            report.issues.iter().find(|issue| {
                matches!(
                    issue.code.as_str(),
                    "unsupported_required_feature"
                        | "artifact_required_missing"
                        | "required_artifact_incompatible"
                        | "conversation_state_incompatible"
                )
            })
        });
        if let Some(issue) = mismatch {
            let feature = issue
                .feature
                .map(|feature| match feature {
                    crate::protocol::capability::Feature::CustomTools => serde_json::json!("custom_tool"),
                    feature => serde_json::to_value(feature).unwrap_or_default(),
                })
                .and_then(|feature| feature.as_str().map(str::to_string))
                .unwrap_or_else(|| "protocol_continuation".to_string());
            let error_kind = if issue.code == "conversation_state_incompatible" {
                "conversation_state_incompatible"
            } else {
                "capability_mismatch"
            };
            return anyhow!(serde_json::json!({
                "error": format!("{}: {error}", error.code()),
                "error_kind": error_kind,
                "issue_code": issue.code.clone(),
                "feature": feature,
                "protocol": inbound.as_str(),
                "target_protocol": target.as_str(),
                "model": requested_model,
                "safe_to_retry_other_channel": true
            })
            .to_string());
        }
        anyhow!("{}: {error}", error.code())
    })?;
    debug_assert!(conversion.plan.is_executable());
    let mut faults = improvement_faults_from_conversion_plan(
        &conversion.plan,
        inbound.as_str(),
        target.as_str(),
        &requested_model,
    );
    // Availability-first boundary: keep the existing best-effort conversion instead of adding a
    // routing hard filter, but make the semantic approximation observable. Remove this warning
    // only together with an operation-specific request and response adapter.
    if let Some(operation) = special_operation_for_path(path) {
        let summary = format!(
            "{operation} has no protocol-neutral equivalent; compatibility conversion was attempted and its semantics should be reviewed"
        );
        eprintln!(
            "[const-api][protocol] conversion warning code=special_operation_approximated source={} target={} path={} summary={}",
            inbound.as_str(),
            target.as_str(),
            path,
            summary
        );
        faults.push(crate::supplier::ImprovementFault::conversion_warning(
            "special_operation_approximated",
            inbound.as_str(),
            target.as_str(),
            &requested_model,
            path,
            &summary,
        ));
    }
    let mut target_body = conversion.body;
    if target == crate::protocol::kind::ProtocolKind::AnthropicMessages
        && crate::protocol::anthropic_dialect::anthropic_model_rejects_assistant_prefill(
            upstream_model,
        )
        && omit_trailing_anthropic_assistant_prefill(&mut target_body)
    {
        let summary = "A trailing assistant prefill unsupported by the selected Claude model was omitted; earlier conversation history was preserved.";
        eprintln!(
            "[const-api][protocol] conversion warning code=anthropic_assistant_prefill_omitted source={} target={} path=$.messages[-1] summary={}",
            inbound.as_str(),
            target.as_str(),
            summary
        );
        faults.push(crate::supplier::ImprovementFault::conversion_warning(
            "anthropic_assistant_prefill_omitted",
            inbound.as_str(),
            target.as_str(),
            &requested_model,
            "$.messages[-1]",
            summary,
        ));
    }
    if target != crate::protocol::kind::ProtocolKind::GeminiNative {
        target_body["stream"] = serde_json::Value::Bool(stream_requested);
    }
    Ok(UpstreamRequestConversion {
        tool_mapping: conversion.plan.tool_mapping,
        path: target.path(upstream_model, stream_requested),
        body: serde_json::to_string(&target_body)?,
        inbound_protocol: inbound.as_str().to_string(),
        target_protocol: target.as_str().to_string(),
        faults,
    })
}

fn omit_trailing_anthropic_assistant_prefill(body: &mut serde_json::Value) -> bool {
    let Some(messages) = body
        .get_mut("messages")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return false;
    };
    if !messages.last().is_some_and(|message| {
        message
            .get("role")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|role| role.trim().eq_ignore_ascii_case("assistant"))
    }) {
        return false;
    }

    messages.pop();
    if messages.is_empty() {
        messages.push(serde_json::json!({
            "role": "user",
            "content": [{"type": "text", "text": ""}]
        }));
    }
    true
}

fn validate_same_protocol_evidence_extensions(
    protocol: crate::protocol::kind::ProtocolKind,
    body: &serde_json::Value,
    model: &str,
    profile: &crate::protocol::capability::CapabilityProfile,
) -> Result<()> {
    use crate::protocol::capability::{support_is_confirmed_unsupported, Feature, SupportState};
    use crate::protocol::kind::ProtocolKind;

    // Native requests normally remain byte-for-byte compatible. Only reviewed private
    // extensions are inspected here, so unknown same-protocol fields keep their passthrough
    // behavior while model-gated dialects cannot bypass capability checks.
    let reviewed_extensions = [
        (
            Feature::AudioInput,
            protocol == ProtocolKind::OpenAiResponses
                && body
                    .get("input")
                    .is_some_and(|input| value_contains_typed_block(input, "input_audio")),
        ),
        (
            Feature::FileInput,
            request_contains_file_input(protocol, body),
        ),
    ];
    for (feature, present) in reviewed_extensions {
        if !present {
            continue;
        }
        let support = profile.resolve(feature, model, protocol);
        let blocked = match feature {
            // This is a reviewed private dialect extension. Positive evidence is required
            // because the public Responses wire cannot express it.
            Feature::AudioInput => support.state != SupportState::Supported,
            // Standard file wires remain available when evidence is missing. Only an exact
            // live negative suppresses the route; declarations are preference signals.
            Feature::FileInput => support_is_confirmed_unsupported(&support),
            _ => false,
        };
        if blocked {
            return Err(capability_mismatch_error(
                feature, protocol, protocol, model,
            ));
        }
    }
    Ok(())
}

fn request_contains_file_input(
    protocol: crate::protocol::kind::ProtocolKind,
    body: &serde_json::Value,
) -> bool {
    use crate::protocol::kind::ProtocolKind;

    match protocol {
        ProtocolKind::OpenAiResponses => value_contains_typed_block(body, "input_file"),
        ProtocolKind::OpenAiChat => {
            value_contains_typed_block(body, "input_file")
                || value_contains_typed_block(body, "file")
        }
        ProtocolKind::AnthropicMessages => value_contains_typed_block(body, "document"),
        ProtocolKind::GeminiNative => value_contains_gemini_file_part(body),
    }
}

fn request_features_requiring_channel_evidence(
    path: &str,
    body: &str,
) -> Vec<crate::protocol::capability::Feature> {
    use crate::protocol::capability::Feature;
    let Ok(protocol) = crate::protocol::kind::ProtocolKind::parse(&protocol_from_path(path)) else {
        return Vec::new();
    };
    let contains_reviewed_feature = match protocol {
        crate::protocol::kind::ProtocolKind::OpenAiResponses
        | crate::protocol::kind::ProtocolKind::OpenAiChat => {
            body.contains("input_file") || body.contains("input_audio") || body.contains("\"file\"")
        }
        crate::protocol::kind::ProtocolKind::AnthropicMessages => body.contains("\"document\""),
        crate::protocol::kind::ProtocolKind::GeminiNative => {
            body.contains("Data") || body.contains("_data")
        }
    };
    if !contains_reviewed_feature {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let mut required = Vec::with_capacity(2);
    if protocol == crate::protocol::kind::ProtocolKind::OpenAiResponses
        && value
            .get("input")
            .is_some_and(|input| value_contains_typed_block(input, "input_audio"))
    {
        required.push(Feature::AudioInput);
    }
    if request_contains_file_input(protocol, &value) {
        required.push(Feature::FileInput);
    }
    required
}

fn channel_profiles_required_request_features_rank(
    api_format: &str,
    model: &str,
    profiles: &[ChannelCapabilityProfile],
    required: &[crate::protocol::capability::Feature],
) -> Option<u8> {
    use crate::protocol::capability::{
        feature_uses_provider_evidence, protocol_evidence_extension_supports,
        protocol_wire_supports, support_availability_rank, SupportState,
    };

    if required.is_empty() {
        return Some(0);
    }
    let Ok(target) =
        crate::protocol::kind::ProtocolKind::parse(&protocol_from_api_format(api_format))
    else {
        return None;
    };
    let profile = capability_profile_from_channel_profiles(profiles, target, model);
    let mut rank = 0;
    for feature in required.iter().copied() {
        let wire_support = protocol_wire_supports(target, feature);
        let private_extension =
            !wire_support && protocol_evidence_extension_supports(target, feature);
        if !feature_uses_provider_evidence(feature) && !private_extension {
            continue;
        }
        let support = profile.resolve(feature, model, target);
        if private_extension && support.state != SupportState::Supported {
            return None;
        }
        rank = rank.max(support_availability_rank(&support)?);
    }
    Some(rank)
}

fn value_contains_gemini_file_part(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Array(items) => items.iter().any(value_contains_gemini_file_part),
        serde_json::Value::Object(object) => {
            if object.contains_key("fileData") || object.contains_key("file_data") {
                return true;
            }
            for key in ["inlineData", "inline_data"] {
                let Some(inline) = object.get(key).and_then(serde_json::Value::as_object) else {
                    continue;
                };
                let mime_type = inline
                    .get("mimeType")
                    .or_else(|| inline.get("mime_type"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if !mime_type.is_empty()
                    && !mime_type.starts_with("image/")
                    && !mime_type.starts_with("audio/")
                    && !mime_type.starts_with("video/")
                {
                    return true;
                }
            }
            object.values().any(value_contains_gemini_file_part)
        }
        _ => false,
    }
}

fn value_contains_typed_block(value: &serde_json::Value, expected: &str) -> bool {
    match value {
        serde_json::Value::Array(items) => items
            .iter()
            .any(|item| value_contains_typed_block(item, expected)),
        serde_json::Value::Object(object) => {
            object
                .get("type")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|value| value == expected)
                || object
                    .values()
                    .any(|value| value_contains_typed_block(value, expected))
        }
        _ => false,
    }
}

fn capability_mismatch_error(
    feature: crate::protocol::capability::Feature,
    inbound: crate::protocol::kind::ProtocolKind,
    target: crate::protocol::kind::ProtocolKind,
    model: &str,
) -> anyhow::Error {
    let feature = serde_json::to_value(feature)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string());
    anyhow!(
        serde_json::json!({
            "error": "unsupported_required_feature: target model does not support a required request feature",
            "error_kind": "capability_mismatch",
            "issue_code": "unsupported_required_feature",
            "feature": feature,
            "protocol": inbound.as_str(),
            "target_protocol": target.as_str(),
            "model": model,
            "safe_to_retry_other_channel": true
        })
        .to_string()
    )
}

fn is_gemini_generation_operation(path: &str) -> bool {
    let path = path.split('?').next().unwrap_or(path);
    path.ends_with(":generateContent") || path.ends_with(":streamGenerateContent")
}

fn special_operation_for_path(path: &str) -> Option<&'static str> {
    let path = path.split('?').next().unwrap_or(path);
    if path.ends_with("/responses/compact") {
        Some("responses_compact")
    } else if path.ends_with("/messages/count_tokens") || path.ends_with(":countTokens") {
        Some("count_tokens")
    } else if path.ends_with(":embedContent") {
        Some("embed_content")
    } else {
        None
    }
}

fn capability_profile_from_channel_profiles(
    profiles: &[ChannelCapabilityProfile],
    target: crate::protocol::kind::ProtocolKind,
    _model: &str,
) -> crate::protocol::capability::CapabilityProfile {
    use crate::protocol::capability::{
        CapabilityEvidence, CapabilityProfile, EvidenceSource, Feature, SupportState,
    };

    let mut evidence = Vec::new();
    for profile in profiles.iter().filter(|profile| {
        crate::protocol::kind::ProtocolKind::parse(&profile.protocol).ok() == Some(target)
            && crate::model::capability_layer_is_routable(&profile.capability_layer)
            && !matches!(
                profile.release_status.trim().to_ascii_lowercase().as_str(),
                "prepared" | "suspended"
            )
    }) {
        let source = match profile.verification_state.trim() {
            "driver_contract" => EvidenceSource::DriverContract,
            "verified" => EvidenceSource::LiveProbe,
            "declared" => EvidenceSource::ProviderMetadata,
            _ => EvidenceSource::Inferred,
        };
        let observed_at = u64::try_from(profile.verified_at_unix).unwrap_or_default();
        let model_pattern = (!profile.model_pattern.trim().is_empty())
            .then(|| profile.model_pattern.trim().to_string());
        macro_rules! push_state {
            ($feature:expr, $state:expr) => {
                evidence.push(CapabilityEvidence {
                    feature: $feature,
                    state: $state,
                    source,
                    model_pattern: model_pattern.clone(),
                    protocol: target,
                    observed_at: Some(observed_at),
                })
            };
        }
        macro_rules! push {
            ($feature:expr, $supported:expr) => {
                if $supported {
                    push_state!($feature, SupportState::Supported);
                }
            };
        }
        macro_rules! push_modality {
            ($feature:expr, $supported:expr, $authoritative:expr) => {
                if $supported {
                    push_state!($feature, SupportState::Supported);
                } else if $authoritative {
                    push_state!($feature, SupportState::Unsupported);
                }
            };
        }
        let input_authoritative = profile.input_modalities_authoritative;
        let output_authoritative = profile.output_modalities_authoritative;
        push!(Feature::Stream, profile.stream_sse);
        push!(Feature::Tools, profile.tool_calls);
        push!(Feature::ParallelTools, profile.parallel_tool_calls);
        for feature in [
            Feature::ToolChoiceNone,
            Feature::ToolChoiceRequired,
            Feature::ToolChoiceNamed,
            Feature::ToolChoiceAllowed,
        ] {
            push!(feature, profile.tool_choice);
        }
        push!(Feature::JsonSchema, profile.json_schema);
        push!(Feature::Reasoning, profile.reasoning || profile.thinking);
        push_modality!(
            Feature::ImageInput,
            profile.vision || profile.image_input,
            input_authoritative
        );
        push_modality!(
            Feature::AudioInput,
            profile.audio_input,
            input_authoritative
        );
        push_modality!(
            Feature::AudioOutput,
            profile.audio_output,
            output_authoritative
        );
        push_modality!(
            Feature::VideoInput,
            profile.video_input,
            input_authoritative
        );
        push_modality!(Feature::FileInput, profile.file_input, input_authoritative);
        push!(Feature::CacheControl, profile.cache_control);
        push!(Feature::CustomTools, profile.custom_tool);
        for hosted in &profile.hosted_tools {
            let feature = match hosted.trim() {
                "web_search" => Some(Feature::HostedWebSearch),
                "file_search" => Some(Feature::HostedFileSearch),
                "code_interpreter" | "code_execution" => Some(Feature::HostedCodeExecution),
                "computer_use" => Some(Feature::HostedComputerUse),
                "mcp" => Some(Feature::HostedMcp),
                "url_context" => Some(Feature::HostedUrlContext),
                _ => None,
            };
            if let Some(feature) = feature {
                push!(feature, true);
            }
        }
    }
    CapabilityProfile::new(evidence)
}

pub(crate) fn json_body_with_stream(body: &str, stream: bool) -> Result<String> {
    rewrite_top_level_json_field(body, "stream", if stream { "true" } else { "false" }, true)
}

pub(crate) fn improvement_faults_from_conversion_plan(
    plan: &crate::protocol::conversion::ConversionPlan,
    source_protocol: &str,
    target_protocol: &str,
    model: &str,
) -> Vec<crate::supplier::ImprovementFault> {
    // Only presentation is aggregated. Keep every structured issue, its exact
    // path, and the executable conversion plan unchanged.
    for group in
        conversion_warning_log_groups(&plan.report().issues, |level| log::log_enabled!(level))
    {
        log::log!(
            group.level,
            "[const-api][protocol] conversion warning code={} source={} target={} model={} path={} occurrences={} sample_path={} summary={}",
            group.issue.code, source_protocol, target_protocol, model, group.path, group.count,
            group.issue.path, group.issue.summary,
        );
    }
    plan.report()
        .issues
        .iter()
        .filter(|issue| issue.severity == crate::protocol::conversion::IssueSeverity::Warning)
        .map(|issue| {
            crate::supplier::ImprovementFault::conversion_warning(
                &issue.code,
                source_protocol,
                target_protocol,
                model,
                &issue.path,
                &issue.summary,
            )
        })
        .collect()
}

struct ConversionWarningLogGroup<'a> {
    issue: &'a crate::protocol::conversion::ConversionIssue,
    level: log::Level,
    path: String,
    count: usize,
}

fn conversion_warning_log_level(
    issue: &crate::protocol::conversion::ConversionIssue,
) -> log::Level {
    // These are expected interop adjustments, not request failures. Unknown limitations and
    // reasoning-history loss remain warnings; do not demote all unsupported features wholesale.
    if issue.code == "provider_extension_omitted"
        || (issue.code == "unsupported_feature_omitted"
            && issue.feature == Some(crate::protocol::capability::Feature::CacheControl))
    {
        log::Level::Debug
    } else {
        log::Level::Warn
    }
}

fn conversion_warning_log_groups(
    issues: &[crate::protocol::conversion::ConversionIssue],
    enabled: impl Fn(log::Level) -> bool,
) -> Vec<ConversionWarningLogGroup<'_>> {
    let mut groups: Vec<ConversionWarningLogGroup<'_>> = Vec::new();
    let mut indices: HashMap<
        (
            &str,
            String,
            Option<crate::protocol::capability::Feature>,
            log::Level,
        ),
        usize,
    > = HashMap::new();
    for issue in issues
        .iter()
        .filter(|issue| issue.severity == crate::protocol::conversion::IssueSeverity::Warning)
    {
        let level = conversion_warning_log_level(issue);
        if !enabled(level) {
            continue;
        }
        let path = conversion_log_field_pattern(&issue.path);
        let key = (issue.code.as_str(), path.clone(), issue.feature, level);
        if let Some(&index) = indices.get(&key) {
            groups[index].count += 1;
        } else {
            indices.insert(key, groups.len());
            groups.push(ConversionWarningLogGroup {
                issue,
                level,
                path,
                count: 1,
            });
        }
    }
    groups
}

fn conversion_log_field_pattern(path: &str) -> String {
    let mut result = String::with_capacity(path.len());
    let mut cursor = 0;
    for (start, _) in path.match_indices('[') {
        if start < cursor {
            continue;
        }
        let Some(relative_end) = path[start + 1..].find(']') else {
            break;
        };
        let end = start + 1 + relative_end;
        let index = &path[start + 1..end];
        if !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit()) {
            result.push_str(&path[cursor..start]);
            result.push_str("[*]");
            cursor = end + 1;
        }
    }
    result.push_str(&path[cursor..]);
    result
}

#[cfg(test)]
mod conversion_advisory_log_tests {
    use super::*;
    use crate::protocol::conversion::{ConversionIssue, IssueAction, IssueSeverity};

    #[test]
    fn long_history_logs_are_bounded_and_keep_unknown_warnings() {
        let mut issues: Vec<_> = (0..2000)
            .map(|index| ConversionIssue {
                code: "reasoning_history_omitted".into(),
                path: format!("$.turns[{index}].blocks[0]"),
                severity: IssueSeverity::Warning,
                feature: None,
                target_limitation: String::new(),
                action: IssueAction::Filter,
                summary: "foreign reasoning omitted".into(),
            })
            .collect();
        let original = issues.clone();
        let groups = conversion_warning_log_groups(&issues, |level| level <= log::Level::Info);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].count, 2000);
        assert_eq!(groups[0].level, log::Level::Warn);
        assert_eq!(issues, original);
        drop(groups);

        issues[0].code = "unsupported_feature_omitted".into();
        issues[0].feature = Some(crate::protocol::capability::Feature::CacheControl);
        assert_eq!(conversion_warning_log_level(&issues[0]), log::Level::Debug);
        issues[0].feature = None;
        assert_eq!(conversion_warning_log_level(&issues[0]), log::Level::Warn);
        issues[0].code = "future_warning_kind".into();
        assert_eq!(conversion_warning_log_level(&issues[0]), log::Level::Warn);
        issues[0].code = "provider_extension_omitted".into();
        assert!(
            conversion_warning_log_groups(&issues[..1], |level| level <= log::Level::Info)
                .is_empty()
        );
    }

    #[test]
    fn conversion_logs_group_array_indices_without_changing_structured_faults() {
        let issue = |code: &str, path: &str| ConversionIssue {
            code: code.into(),
            path: path.into(),
            severity: IssueSeverity::Warning,
            feature: None,
            target_limitation: String::new(),
            action: IssueAction::Filter,
            summary: String::new(),
        };
        let issues = vec![
            issue("provider_extension_omitted", "$.messages[1].traceId"),
            issue("provider_extension_omitted", "$.messages[42].traceId"),
            issue("provider_extension_omitted", "$.messages[42].agent"),
            issue("reasoning_history_omitted", "$.messages[43].reasoning"),
        ];
        let original = issues.clone();
        let groups = conversion_warning_log_groups(&issues, |_| true);
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].path, "$.messages[*].traceId");
        assert_eq!(groups[0].issue.path, "$.messages[1].traceId");
        assert_eq!(groups[0].count, 2);
        assert_eq!(groups[1].path, "$.messages[*].agent");
        assert_eq!(groups[0].level, log::Level::Debug);
        assert_eq!(groups[2].level, log::Level::Warn);
        assert_eq!(issues, original);
        assert_eq!(
            conversion_log_field_pattern("$.消息[12].parts[3].traceId"),
            "$.消息[*].parts[*].traceId"
        );
        assert_eq!(
            conversion_log_field_pattern("$['42'].field[]"),
            "$['42'].field[]"
        );
    }
}

#[cfg(test)]
pub(crate) fn chat_body_to_responses_body(body: &str, upstream_model: &str) -> Result<String> {
    upstream_request_for_api_format(
        "openai_responses",
        "/v1/chat/completions",
        body,
        upstream_model,
    )
    .map(|(_, body, _, _)| body)
}

#[cfg(test)]
pub(crate) fn responses_request_to_chat_body(body: &str, upstream_model: &str) -> Result<String> {
    upstream_request_for_api_format("openai_chat", "/v1/responses", body, upstream_model)
        .map(|(_, body, _, _)| body)
}
