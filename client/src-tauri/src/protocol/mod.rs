pub(crate) mod adapters;
pub(crate) mod anthropic_dialect;
pub(crate) mod capability;
pub(crate) mod continuation;
pub(crate) mod conversion;
pub(crate) mod error;
pub(crate) mod ir;
pub(crate) mod kind;
pub(crate) mod stream;

#[cfg(test)]
pub(crate) mod tests;

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use adapters::{
    AdapterContext, AdapterError, AnthropicMessagesAdapter, GeminiNativeAdapter, OpenAiChatAdapter,
    OpenAiResponsesAdapter, ProtocolAdapter,
};
use capability::CapabilityProfile;
use conversion::{
    CompatibleAction, ConversionError, ConversionLevel, ConversionPlan, ConversionPolicy,
    IssueSeverity, TargetRules, plan_conversion, validate_and_normalize_transcript,
};
use ir::{CanonicalRequestV2, CanonicalResponseV2};
use kind::ProtocolKind;

static OPENAI_RESPONSES: OpenAiResponsesAdapter = OpenAiResponsesAdapter;
static OPENAI_CHAT: OpenAiChatAdapter = OpenAiChatAdapter;
static ANTHROPIC_MESSAGES: AnthropicMessagesAdapter = AnthropicMessagesAdapter;
static GEMINI_NATIVE: GeminiNativeAdapter = GeminiNativeAdapter;

fn adapter(protocol: ProtocolKind) -> &'static dyn ProtocolAdapter {
    match protocol {
        ProtocolKind::OpenAiResponses => &OPENAI_RESPONSES,
        ProtocolKind::OpenAiChat => &OPENAI_CHAT,
        ProtocolKind::AnthropicMessages => &ANTHROPIC_MESSAGES,
        ProtocolKind::GeminiNative => &GEMINI_NATIVE,
    }
}

#[derive(Debug)]
pub(crate) enum ProtocolFacadeError {
    Adapter(AdapterError),
    Conversion(ConversionError),
}

impl ProtocolFacadeError {
    pub(crate) const fn code(&self) -> &'static str {
        match self {
            Self::Adapter(error) => error.code(),
            Self::Conversion(error) => error.code(),
        }
    }

    pub(crate) const fn report(&self) -> Option<&conversion::ConversionReport> {
        match self {
            Self::Adapter(_) => None,
            Self::Conversion(error) => Some(error.report()),
        }
    }
}

impl fmt::Display for ProtocolFacadeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Adapter(error) => error.fmt(formatter),
            Self::Conversion(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ProtocolFacadeError {}

impl From<AdapterError> for ProtocolFacadeError {
    fn from(error: AdapterError) -> Self {
        Self::Adapter(error)
    }
}

impl From<ConversionError> for ProtocolFacadeError {
    fn from(error: ConversionError) -> Self {
        Self::Conversion(error)
    }
}

#[derive(Debug)]
pub(crate) struct NonStreamConversion {
    pub(crate) body: Value,
    pub(crate) plan: ConversionPlan,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct ExchangePreview {
    pub(crate) inbound_protocol: String,
    pub(crate) target_protocol: String,
    pub(crate) requested_model: String,
    pub(crate) upstream_model: String,
    pub(crate) conversion_level: String,
    pub(crate) path: String,
    pub(crate) body: Value,
    pub(crate) unsupported_fields: Vec<String>,
    pub(crate) lossy_warnings: Vec<String>,
}

pub(crate) fn decode_request(
    protocol: ProtocolKind,
    body: &Value,
    context: &AdapterContext,
) -> Result<CanonicalRequestV2, ProtocolFacadeError> {
    Ok(adapter(protocol).decode_request(body, context)?)
}

pub(crate) fn encode_request(
    protocol: ProtocolKind,
    request: &CanonicalRequestV2,
    plan: &ConversionPlan,
    context: &AdapterContext,
) -> Result<Value, ProtocolFacadeError> {
    Ok(adapter(protocol).encode_request(request, plan, context)?)
}

pub(crate) fn decode_response(
    protocol: ProtocolKind,
    body: &Value,
    context: &AdapterContext,
) -> Result<CanonicalResponseV2, ProtocolFacadeError> {
    Ok(adapter(protocol).decode_response(body, context)?)
}

pub(crate) fn encode_response(
    protocol: ProtocolKind,
    response: &CanonicalResponseV2,
    plan: &ConversionPlan,
    context: &AdapterContext,
) -> Result<Value, ProtocolFacadeError> {
    Ok(adapter(protocol).encode_response(response, plan, context)?)
}

pub(crate) fn plan_request(
    request: &CanonicalRequestV2,
    source: ProtocolKind,
    target: ProtocolKind,
    source_context: &AdapterContext,
    target_context: &AdapterContext,
    target_profile: &CapabilityProfile,
    policy: ConversionPolicy,
) -> Result<ConversionPlan, ProtocolFacadeError> {
    Ok(plan_conversion(
        request,
        source,
        target,
        &source_context.issuer,
        &target_context.issuer,
        target_profile,
        policy,
    )?)
}

pub(crate) fn convert_non_stream(
    source: ProtocolKind,
    target: ProtocolKind,
    body: &Value,
    source_context: &AdapterContext,
    target_context: &AdapterContext,
    target_profile: &CapabilityProfile,
    policy: ConversionPolicy,
) -> Result<NonStreamConversion, ProtocolFacadeError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        convert_non_stream_inner(
            source,
            target,
            body,
            source_context,
            target_context,
            target_profile,
            policy,
        )
    }))
    .unwrap_or_else(|_| {
        eprintln!("[const-api][protocol] recovered request conversion panic");
        Err(protocol_panic_error("request conversion"))
    })
}

fn convert_non_stream_inner(
    source: ProtocolKind,
    target: ProtocolKind,
    body: &Value,
    source_context: &AdapterContext,
    target_context: &AdapterContext,
    target_profile: &CapabilityProfile,
    policy: ConversionPolicy,
) -> Result<NonStreamConversion, ProtocolFacadeError> {
    if source == target && !continuation::value_contains_carrier(body) {
        let model = target_context
            .issuer
            .model_family
            .as_deref()
            .or_else(|| body.get("model").and_then(Value::as_str))
            .or(source_context.issuer.model_family.as_deref())
            .unwrap_or_default();
        return Ok(NonStreamConversion {
            body: passthrough_request_with_model(target, body, model),
            plan: ConversionPlan::native_passthrough(target),
        });
    }

    let mut request = decode_request(source, body, source_context)?;
    if let Some(model) = target_context.issuer.model_family.as_deref() {
        request.model = model.to_string();
    }
    // The decoder retains each additional_tools item for native replay, but
    // also includes its definitions in request.tools. Drop only that envelope
    // before cross-protocol role normalization to avoid empty assistant turns.
    let mut repairs = Vec::new();
    if source == ProtocolKind::OpenAiResponses
        && target != ProtocolKind::OpenAiResponses
        && request.metadata.responses_tool_placement.is_some()
    {
        request.turns.retain(|turn| {
            !matches!(turn.blocks.as_slice(), [ir::ContentBlock::ProviderArtifact(artifact)]
                if artifact.payload.get("type").and_then(Value::as_str) == Some("additional_tools"))
        });
        repairs.push(conversion::ConversionRepair {
            code: "additional_tools_promoted".to_string(),
            class: conversion::RepairClass::CompatibleAllowlisted,
            path: "$.input".to_string(),
            action: "merge_additional_tools_into_target_declarations".to_string(),
            reason: "Target tools are request-wide; input tool declarations were retained in stable order in the target tool list.".to_string(),
        });
    }
    let tool_mapping = if source == ProtocolKind::OpenAiResponses && target != source {
        let mapping = conversion::bridge_request_tools(&mut request)?;
        if !mapping.is_empty() {
            repairs.push(conversion::ConversionRepair {
                code: "tool_transport_adapted".to_string(),
                class: conversion::RepairClass::CompatibleAllowlisted,
                path: "$.tools".to_string(),
                action: "wrap_custom_input_and_flatten_namespace".to_string(),
                reason: "Tool names and raw input are restored on responses. Grammar is retained as format guidance, not native constrained sampling.".to_string(),
            });
        }
        mapping
    } else {
        conversion::ToolWireMap::default()
    };
    let mut transcript_rules = TargetRules::new(source, target);
    if target == ProtocolKind::AnthropicMessages {
        transcript_rules = transcript_rules
            .require_alternating_roles()
            .group_parallel_tool_results();
        transcript_rules.allow(CompatibleAction::CoalesceRoleEnvelope);
    } else if target == ProtocolKind::GeminiNative {
        transcript_rules = transcript_rules.group_parallel_tool_results();
        transcript_rules.allow(CompatibleAction::CoalesceRoleEnvelope);
    }
    repairs.extend(validate_and_normalize_transcript(
        &mut request,
        &transcript_rules,
    )?);
    let mut plan = plan_request(
        &request,
        source,
        target,
        source_context,
        target_context,
        target_profile,
        policy,
    )?;
    plan.attach_repairs(repairs, policy)?;
    plan.tool_mapping = tool_mapping;
    let converted_body = encode_request(target, &request, &plan, target_context)?;
    Ok(NonStreamConversion {
        body: converted_body,
        plan,
    })
}

fn passthrough_request_with_model(protocol: ProtocolKind, body: &Value, model: &str) -> Value {
    let mut passthrough = body.clone();
    if matches!(
        protocol,
        ProtocolKind::OpenAiResponses | ProtocolKind::OpenAiChat | ProtocolKind::AnthropicMessages
    ) {
        if let Some(object) = passthrough.as_object_mut() {
            object.insert("model".to_string(), Value::String(model.to_string()));
        }
    }
    passthrough
}

pub(crate) fn convert_response_non_stream(
    source: ProtocolKind,
    target: ProtocolKind,
    body: &Value,
    source_context: &AdapterContext,
    target_context: &AdapterContext,
) -> Result<NonStreamConversion, ProtocolFacadeError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if source == target {
            return Ok(NonStreamConversion {
                body: body.clone(),
                plan: ConversionPlan::native_passthrough(target),
            });
        }
        let response = decode_response(source, body, source_context)?;
        convert_canonical_response_inner(
            source,
            target,
            response,
            body,
            source_context,
            target_context,
        )
    }))
    .unwrap_or_else(|_| {
        eprintln!("[const-api][protocol] recovered response conversion panic");
        Err(protocol_panic_error("response conversion"))
    })
}

pub(crate) fn convert_canonical_response(
    source: ProtocolKind,
    target: ProtocolKind,
    response: CanonicalResponseV2,
    native_body: &Value,
    source_context: &AdapterContext,
    target_context: &AdapterContext,
) -> Result<NonStreamConversion, ProtocolFacadeError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        convert_canonical_response_inner(
            source,
            target,
            response,
            native_body,
            source_context,
            target_context,
        )
    }))
    .unwrap_or_else(|_| {
        eprintln!("[const-api][protocol] recovered canonical response conversion panic");
        Err(protocol_panic_error("canonical response conversion"))
    })
}

fn convert_canonical_response_inner(
    source: ProtocolKind,
    target: ProtocolKind,
    response: CanonicalResponseV2,
    native_body: &Value,
    source_context: &AdapterContext,
    target_context: &AdapterContext,
) -> Result<NonStreamConversion, ProtocolFacadeError> {
    if source == target {
        return Ok(NonStreamConversion {
            body: native_body.clone(),
            plan: ConversionPlan::native_passthrough(target),
        });
    }
    let model = response
        .model
        .as_deref()
        .or(source_context.issuer.model_family.as_deref())
        .unwrap_or_default();
    let request = CanonicalRequestV2::new(model);
    let mut plan = plan_request(
        &request,
        source,
        target,
        source_context,
        target_context,
        &CapabilityProfile::default(),
        ConversionPolicy::Production,
    )?;
    plan.attach_response_extensions(
        &response.extensions,
        source,
        target,
        &target_context.issuer,
        target_context
            .issuer
            .model_family
            .as_deref()
            .unwrap_or(model),
        ConversionPolicy::Production,
    )?;
    plan.attach_response_content(
        &response,
        target,
        &target_context.issuer,
        target_context
            .issuer
            .model_family
            .as_deref()
            .unwrap_or(model),
    );
    let converted_body = encode_response(target, &response, &plan, target_context)?;
    Ok(NonStreamConversion {
        body: converted_body,
        plan,
    })
}

fn protocol_panic_error(operation: &str) -> ProtocolFacadeError {
    ProtocolFacadeError::Adapter(AdapterError::new(
        "protocol_conversion_panic",
        "$",
        format!("{operation} panicked; only this request was terminated"),
    ))
}

pub(crate) fn build_prompt_body(
    protocol: ProtocolKind,
    model: &str,
    prompt: &str,
    stream: bool,
) -> Value {
    let model = model.trim();
    match protocol {
        ProtocolKind::OpenAiResponses => serde_json::json!({
            "model": model,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": prompt}]
            }],
            "store": false,
            "stream": stream
        }),
        ProtocolKind::OpenAiChat => serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": prompt}],
            "stream": stream
        }),
        ProtocolKind::AnthropicMessages => serde_json::json!({
            "model": model,
            "max_tokens": 128,
            "messages": [{"role": "user", "content": prompt}],
            "stream": stream
        }),
        ProtocolKind::GeminiNative => serde_json::json!({
            "contents": [{"role": "user", "parts": [{"text": prompt}]}],
            "generationConfig": {"maxOutputTokens": 128}
        }),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_exchange_preview(
    inbound_protocol: &str,
    target_protocol: &str,
    requested_model: &str,
    upstream_model: &str,
    prompt: &str,
    stream: bool,
) -> Result<ExchangePreview, ProtocolFacadeError> {
    let inbound = ProtocolKind::parse(inbound_protocol).map_err(|error| {
        ProtocolFacadeError::Adapter(AdapterError::new(
            "invalid_protocol",
            "$.inbound_protocol",
            error.to_string(),
        ))
    })?;
    let target = ProtocolKind::parse(target_protocol).map_err(|error| {
        ProtocolFacadeError::Adapter(AdapterError::new(
            "invalid_protocol",
            "$.target_protocol",
            error.to_string(),
        ))
    })?;
    let requested_model = requested_model.trim();
    if requested_model.is_empty() {
        return Err(ProtocolFacadeError::Adapter(AdapterError::new(
            "model_required",
            "$.model",
            "model is required",
        )));
    }
    let upstream_model = if upstream_model.trim().is_empty() {
        requested_model
    } else {
        upstream_model.trim()
    };
    let inbound_body = build_prompt_body(inbound, requested_model, prompt, stream);
    let source_context = AdapterContext::runtime(
        format!("debug:{}", inbound.as_str()),
        "debug-inbound",
        None,
        requested_model,
    );
    let target_context = if inbound == target {
        source_context.clone()
    } else {
        AdapterContext::runtime(
            format!("debug:{}", target.as_str()),
            "debug-target",
            None,
            upstream_model,
        )
    };
    let mut request = decode_request(inbound, &inbound_body, &source_context)?;
    request.model = upstream_model.to_string();
    request.stream = stream;
    let plan = plan_request(
        &request,
        inbound,
        target,
        &source_context,
        &target_context,
        &CapabilityProfile::default(),
        ConversionPolicy::Diagnostic,
    )?;
    let body = if plan.level() == ConversionLevel::Native {
        let mut body = inbound_body;
        if target != ProtocolKind::GeminiNative {
            body["model"] = Value::String(upstream_model.to_string());
        }
        body
    } else {
        encode_request(target, &request, &plan, &target_context)?
    };
    let report = plan.report();
    Ok(ExchangePreview {
        inbound_protocol: inbound.as_str().to_string(),
        target_protocol: target.as_str().to_string(),
        requested_model: requested_model.to_string(),
        upstream_model: upstream_model.to_string(),
        conversion_level: report.level.to_string(),
        path: target.path(upstream_model, stream),
        body,
        unsupported_fields: report
            .issues
            .iter()
            .filter(|issue| issue.severity == IssueSeverity::Error)
            .map(|issue| issue.path.clone())
            .collect(),
        lossy_warnings: report
            .issues
            .iter()
            .filter(|issue| issue.severity == IssueSeverity::Warning)
            .map(|issue| format!("{}: {}", issue.code, issue.summary))
            .collect(),
    })
}
