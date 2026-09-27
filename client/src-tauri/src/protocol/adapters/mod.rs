mod anthropic_messages;
mod gemini_native;
mod openai_chat;
mod openai_responses;

pub(crate) use openai_chat::OpenAiChatAdapter;
pub(crate) use openai_responses::OpenAiResponsesAdapter;

use std::{collections::HashMap, fmt};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::protocol::conversion::{ConversionPlan, IssuerIdentity};
use crate::protocol::ir::{
    ArtifactAffinity, BlockMetadata, CanonicalRequestV2, CanonicalResponseV2, ContentBlock,
    ExtensionCriticality, FinishReason, PromptCacheBreakpointMode, PromptCacheConfig,
    PromptCacheMode, ProviderExtension, ResponseStatus, Turn,
};
use crate::protocol::kind::ProtocolKind;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProviderDialect {
    OpenAi,
    AzureOpenAi,
    DeepSeek,
    Compatible { provider: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AdapterContext {
    pub(crate) dialect: ProviderDialect,
    pub(crate) issuer: IssuerIdentity,
}

impl AdapterContext {
    #[cfg(test)]
    pub(crate) fn legacy_bridge(model: &str) -> Self {
        Self {
            dialect: ProviderDialect::Compatible {
                provider: "legacy_bridge".to_string(),
            },
            issuer: IssuerIdentity {
                provider: "legacy_bridge".to_string(),
                endpoint_fingerprint: "legacy_bridge".to_string(),
                account_fingerprint: None,
                model_family: Some(model.to_string()),
            },
        }
    }

    pub(crate) fn runtime(
        provider: impl Into<String>,
        endpoint_fingerprint: impl Into<String>,
        account_fingerprint: Option<String>,
        model: impl Into<String>,
    ) -> Self {
        let provider = provider.into();
        Self {
            dialect: ProviderDialect::Compatible {
                provider: provider.clone(),
            },
            issuer: IssuerIdentity {
                provider,
                endpoint_fingerprint: endpoint_fingerprint.into(),
                account_fingerprint,
                model_family: Some(model.into()),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AdapterError {
    code: &'static str,
    path: String,
    message: String,
}

impl AdapterError {
    pub(crate) fn new(
        code: &'static str,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            path: path.into(),
            message: message.into(),
        }
    }

    pub(crate) const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for AdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} at {}: {}",
            self.code, self.path, self.message
        )
    }
}

impl std::error::Error for AdapterError {}

pub(crate) trait ProtocolAdapter {
    fn decode_request(
        &self,
        body: &serde_json::Value,
        context: &AdapterContext,
    ) -> Result<CanonicalRequestV2, AdapterError>;
    fn encode_request(
        &self,
        request: &CanonicalRequestV2,
        plan: &ConversionPlan,
        context: &AdapterContext,
    ) -> Result<serde_json::Value, AdapterError>;
    fn decode_response(
        &self,
        body: &serde_json::Value,
        context: &AdapterContext,
    ) -> Result<CanonicalResponseV2, AdapterError>;
    fn encode_response(
        &self,
        response: &CanonicalResponseV2,
        plan: &ConversionPlan,
        context: &AdapterContext,
    ) -> Result<serde_json::Value, AdapterError>;
}

pub(super) fn ensure_encoding_plan(
    plan: &ConversionPlan,
    target: ProtocolKind,
) -> Result<(), AdapterError> {
    if !plan.is_executable() || plan.target_protocol() != target {
        return Err(AdapterError::new(
            "encoder_plan_violation",
            "$",
            format!("plan does not authorize {target} encoding"),
        ));
    }
    Ok(())
}

pub(super) fn exact_issuer_affinity(context: &AdapterContext, model: &str) -> ArtifactAffinity {
    ArtifactAffinity::ExactIssuer {
        provider: context.issuer.provider.clone(),
        endpoint_fingerprint: context.issuer.endpoint_fingerprint.clone(),
        account_fingerprint: context.issuer.account_fingerprint.clone(),
        model: Some(model.to_string()),
    }
}

pub(super) const fn protocol_affinity(protocol: ProtocolKind) -> ArtifactAffinity {
    ArtifactAffinity::Protocol { protocol }
}

pub(super) fn protocol_model_affinity(protocol: ProtocolKind, model: &str) -> ArtifactAffinity {
    ArtifactAffinity::ProtocolModel {
        protocol,
        model: model.to_string(),
    }
}

pub(super) fn continuation_artifact_affinity(
    context: &AdapterContext,
    model: &str,
    protocol: ProtocolKind,
) -> ArtifactAffinity {
    if context.issuer.endpoint_fingerprint == "response-upstream" {
        protocol_model_affinity(protocol, model)
    } else {
        exact_issuer_affinity(context, model)
    }
}

pub(super) fn link_tool_result_names(turns: &mut [Turn]) {
    let call_names = turns
        .iter()
        .flat_map(|turn| &turn.blocks)
        .filter_map(|block| match block {
            ContentBlock::ToolCall(call) => Some((call.id.clone(), call.name.clone())),
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    for result in turns
        .iter_mut()
        .flat_map(|turn| &mut turn.blocks)
        .filter_map(|block| match block {
            ContentBlock::ToolResult(result) => Some(result),
            _ => None,
        })
    {
        if result.name.is_none() {
            result.name = call_names.get(&result.call_id).cloned();
        }
    }
}

fn extension_namespace_protocol(namespace: &str) -> Option<ProtocolKind> {
    match namespace {
        "openai_responses" => Some(ProtocolKind::OpenAiResponses),
        "openai_chat" => Some(ProtocolKind::OpenAiChat),
        "anthropic_messages" => Some(ProtocolKind::AnthropicMessages),
        "gemini_native" => Some(ProtocolKind::GeminiNative),
        _ => None,
    }
}

pub(super) fn collect_nested_unknown_fields(
    extensions: &mut Vec<ProviderExtension>,
    namespace: &str,
    value: Option<&Value>,
    recognized: &[&str],
    path: &str,
    context: &AdapterContext,
    model: &str,
) {
    let Some(object) = value.and_then(Value::as_object) else {
        return;
    };
    for (name, value) in object {
        if recognized.contains(&name.as_str()) {
            continue;
        }
        extensions.push(ProviderExtension {
            namespace: namespace.to_string(),
            name: ProviderExtension::nested_name(&json_path_child(path, name)),
            value: value.clone(),
            affinity: extension_namespace_protocol(namespace)
                .map(protocol_affinity)
                .unwrap_or_else(|| exact_issuer_affinity(context, model)),
            criticality: ExtensionCriticality::Advisory,
        });
    }
}

fn json_path_child(path: &str, name: &str) -> String {
    if name.chars().enumerate().all(|(index, character)| {
        character == '_'
            || character.is_ascii_alphanumeric() && (index > 0 || !character.is_ascii_digit())
    }) {
        format!("{path}.{name}")
    } else {
        let quoted = serde_json::to_string(name).unwrap_or_else(|_| format!("\"{name}\""));
        format!("{path}[{quoted}]")
    }
}

pub(super) fn affinity_matches_context(
    affinity: &ArtifactAffinity,
    context: &AdapterContext,
    model: &str,
    protocol: ProtocolKind,
) -> bool {
    match affinity {
        ArtifactAffinity::ExactIssuer {
            provider,
            endpoint_fingerprint,
            account_fingerprint,
            model: expected_model,
        } => {
            provider == &context.issuer.provider
                && endpoint_fingerprint == &context.issuer.endpoint_fingerprint
                && account_fingerprint.as_ref().is_none_or(|expected| {
                    context.issuer.account_fingerprint.as_ref() == Some(expected)
                })
                && expected_model
                    .as_deref()
                    .is_none_or(|expected| model.is_empty() || expected.eq_ignore_ascii_case(model))
        }
        ArtifactAffinity::Protocol { protocol: expected } => *expected == protocol,
        ArtifactAffinity::ProtocolModel {
            protocol: expected,
            model: expected_model,
        } => {
            *expected == protocol
                && (model.is_empty() || expected_model.eq_ignore_ascii_case(model))
        }
        _ => false,
    }
}

pub(super) fn response_status_from_finish(reason: &FinishReason) -> ResponseStatus {
    match reason {
        FinishReason::Unknown | FinishReason::PauseTurn => ResponseStatus::InProgress,
        FinishReason::ContentFilter | FinishReason::Refusal => ResponseStatus::Refused,
        FinishReason::Error => ResponseStatus::Failed,
        FinishReason::Cancelled => ResponseStatus::Cancelled,
        FinishReason::Length => ResponseStatus::Incomplete,
        FinishReason::Stop | FinishReason::ToolCalls => ResponseStatus::Completed,
    }
}

pub(super) fn decode_openai_prompt_cache(body: &Value) -> PromptCacheConfig {
    let options = body.get("prompt_cache_options");
    PromptCacheConfig {
        key: body
            .get("prompt_cache_key")
            .and_then(Value::as_str)
            .map(str::to_string),
        mode: match options
            .and_then(|value| value.get("mode"))
            .and_then(Value::as_str)
        {
            Some("implicit") => Some(PromptCacheMode::Implicit),
            Some("explicit") => Some(PromptCacheMode::Explicit),
            _ => None,
        },
        ttl: options
            .and_then(|value| value.get("ttl"))
            .and_then(Value::as_str)
            .map(str::to_string),
        legacy_retention: body
            .get("prompt_cache_retention")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

pub(super) fn encode_openai_prompt_cache(body: &mut Value, config: &PromptCacheConfig) {
    if let Some(key) = &config.key {
        body["prompt_cache_key"] = json!(key);
    }
    if config.mode.is_some() || config.ttl.is_some() {
        let mut options = json!({});
        if let Some(mode) = config.mode {
            options["mode"] = json!(match mode {
                PromptCacheMode::Implicit => "implicit",
                PromptCacheMode::Explicit => "explicit",
            });
        }
        if let Some(ttl) = &config.ttl {
            options["ttl"] = json!(ttl);
        }
        body["prompt_cache_options"] = options;
    }
    if let Some(retention) = &config.legacy_retention {
        body["prompt_cache_retention"] = json!(retention);
    }
}

pub(super) fn decode_prompt_cache_breakpoint(value: &Value) -> Option<PromptCacheBreakpointMode> {
    (value
        .get("prompt_cache_breakpoint")
        .and_then(|breakpoint| breakpoint.get("mode"))
        .and_then(Value::as_str)
        == Some("explicit"))
    .then_some(PromptCacheBreakpointMode::Explicit)
}

pub(super) fn encode_prompt_cache_breakpoint(value: &mut Value, metadata: &BlockMetadata) {
    if metadata.prompt_cache_breakpoint == Some(PromptCacheBreakpointMode::Explicit) {
        value["prompt_cache_breakpoint"] = json!({"mode":"explicit"});
    }
}

pub(crate) use anthropic_messages::AnthropicMessagesAdapter;
pub(crate) use gemini_native::GeminiNativeAdapter;
