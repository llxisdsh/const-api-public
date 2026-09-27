use crate::channel_surface::{
    ChannelSurfaceTarget, channel_protocol_target, channel_surface_target, parse_http_surface_base,
    validate_channel_v2,
};
use crate::detection::gemini_openai_base;
use crate::model::ChannelConfig;
use crate::proxy::{json_body_with_stream, upstream_request_for_api_format_with_profiles_report};
use crate::supplier::{
    anthropic_body_with_automatic_cache, bedrock_anthropic_base, join_upstream_url,
    openai_body_with_cache_identity, requested_model_from_body_or_path,
    rewrite_supplier_body_model, supplier_target_stream_requested,
    supplier_upstream_model_for_request, supplier_upstream_path, top_level_json_bool,
};
use crate::surface::{ApiOperation, ApiRoute, resolve_api_route};
use crate::{
    SKIP_LOCAL_SHORT_CIRCUIT_HEADER, USE_LOCAL_SHORT_CIRCUIT_HEADER, supplier_from_channel,
};
use anyhow::{Context, Result, anyhow};
use bytes::Bytes;
use reqwest::{Client, Method, Response, Url, header::HeaderMap};
use std::{error::Error as StdError, fmt};

const TOKEN_COUNT_ESTIMATED_HEADER: &str = "x-const-api-token-count-estimated";

pub(crate) struct ChannelRequest {
    pub(crate) method: Method,
    pub(crate) path: String,
    pub(crate) raw_query: String,
    pub(crate) has_query: bool,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Bytes,
    pub(crate) stream_requested: Option<bool>,
    pub(crate) selected_upstream_model: Option<String>,
    pub(crate) selected_target_protocol: Option<String>,
    pub(crate) cache_identity: Option<String>,
    pub(crate) safety_identifier: Option<String>,
}

#[derive(Debug)]
pub(crate) struct ChannelExecution {
    pub(crate) tool_mapping: crate::protocol::conversion::ToolWireMap,
    pub(crate) response: Response,
    pub(crate) inbound_protocol: Option<String>,
    pub(crate) target_protocol: String,
    pub(crate) upstream_model: String,
    pub(crate) inbound_stream_requested: bool,
    pub(crate) native_passthrough: bool,
    pub(crate) opaque: bool,
    pub(crate) improvement_faults: Vec<crate::supplier::ImprovementFault>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChannelExecutionErrorCode {
    AmbiguousTransport,
    UpstreamRejected,
    ConversionFailed,
    SurfaceOperationNotSupported,
    AuthenticationFailed,
    UpstreamTimeout,
}

impl ChannelExecutionErrorCode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::AmbiguousTransport => "ambiguous_transport",
            Self::UpstreamRejected => "upstream_rejected",
            Self::ConversionFailed => "conversion_failed",
            Self::SurfaceOperationNotSupported => "surface_operation_not_supported",
            Self::AuthenticationFailed => "authentication_failed",
            Self::UpstreamTimeout => "upstream_timeout",
        }
    }
}

#[derive(Debug)]
struct ChannelExecutionError {
    code: ChannelExecutionErrorCode,
    message: String,
    source: Option<Box<dyn StdError + Send + Sync>>,
}

impl fmt::Display for ChannelExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ChannelExecutionError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn StdError + 'static))
    }
}

fn execution_error(code: ChannelExecutionErrorCode, error: impl fmt::Display) -> anyhow::Error {
    anyhow::Error::new(ChannelExecutionError {
        code,
        message: error.to_string(),
        source: None,
    })
}

fn execution_error_with_source(
    code: ChannelExecutionErrorCode,
    error: impl StdError + Send + Sync + 'static,
) -> anyhow::Error {
    anyhow::Error::new(ChannelExecutionError {
        code,
        message: error.to_string(),
        source: Some(Box::new(error)),
    })
}

pub(crate) fn channel_execution_error_code(error: &anyhow::Error) -> Option<&'static str> {
    error.chain().find_map(|cause| {
        cause
            .downcast_ref::<ChannelExecutionError>()
            .map(|error| error.code.as_str())
    })
}

pub(crate) fn response_conversion_error(error: impl fmt::Display) -> anyhow::Error {
    execution_error(ChannelExecutionErrorCode::ConversionFailed, error)
}

pub(crate) fn ambiguous_transport_error(error: impl fmt::Display) -> anyhow::Error {
    execution_error(ChannelExecutionErrorCode::AmbiguousTransport, error)
}

pub(crate) fn surface_operation_not_supported_error(error: impl fmt::Display) -> anyhow::Error {
    execution_error(
        ChannelExecutionErrorCode::SurfaceOperationNotSupported,
        error,
    )
}

impl ChannelExecution {
    #[cfg(test)]
    pub(crate) fn error_code(&self) -> Option<ChannelExecutionErrorCode> {
        match self.response.status().as_u16() {
            200..=299 => None,
            401 | 403 => Some(ChannelExecutionErrorCode::AuthenticationFailed),
            _ => Some(ChannelExecutionErrorCode::UpstreamRejected),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChannelExecutionBackend {
    HttpSurface,
    RetainedSubscription { provider: &'static str },
}

pub(crate) fn channel_execution_backend(
    channel: &ChannelConfig,
    operation: ApiOperation,
) -> Result<ChannelExecutionBackend> {
    if operation == ApiOperation::Opaque {
        return Ok(ChannelExecutionBackend::HttpSurface);
    }
    let execution_kind = execution_kind_for_source(channel.source_driver())?;
    Ok(
        match subscription_provider_for_execution_kind(execution_kind) {
            Some(provider) => ChannelExecutionBackend::RetainedSubscription { provider },
            None => ChannelExecutionBackend::HttpSurface,
        },
    )
}

pub(crate) fn admit_retained_subscription_operation(
    channel: &ChannelConfig,
    route: &ApiRoute,
    stream: bool,
) -> Result<&'static crate::source_driver::SubscriptionOperationCapability> {
    let mode = if stream {
        crate::source_driver::SubscriptionResponseMode::Sse
    } else {
        crate::source_driver::SubscriptionResponseMode::Buffered
    };
    crate::source_driver::admit_subscription_operation(
        channel.source_driver(),
        route.surface,
        route.operation,
        route.protocol,
        mode,
    )
    .map_err(|error| {
        execution_error(
            ChannelExecutionErrorCode::SurfaceOperationNotSupported,
            error,
        )
    })
}

pub(crate) fn execution_kind_for_source(
    source_driver: crate::source_driver::SourceDriverId,
) -> Result<crate::source_driver::ExecutionKind> {
    Ok(crate::source_driver::source_driver(source_driver)?.execution_kind())
}

pub(crate) fn subscription_provider_for_execution_kind(
    execution_kind: crate::source_driver::ExecutionKind,
) -> Option<&'static str> {
    match execution_kind {
        crate::source_driver::ExecutionKind::OpenAiSubscription => Some("openai"),
        crate::source_driver::ExecutionKind::ClaudeSubscription => Some("claude"),
        crate::source_driver::ExecutionKind::GeminiSubscription => Some("antigravity"),
        crate::source_driver::ExecutionKind::GrokSubscription => Some("grok"),
        crate::source_driver::ExecutionKind::HttpSurface => None,
    }
}

pub(crate) async fn execute_channel_request(
    client: &Client,
    channel: &ChannelConfig,
    request: ChannelRequest,
) -> Result<ChannelExecution> {
    let route = resolve_api_route(
        request.method.as_str(),
        &request.path,
        request_is_websocket(&request.headers),
    )
    .ok_or_else(|| {
        execution_error(
            ChannelExecutionErrorCode::SurfaceOperationNotSupported,
            "request path is outside the configured API surfaces",
        )
    })?;
    let opaque = route.operation == ApiOperation::Opaque;
    if !crate::detection::responses_features::channel_allows_path(channel, &request.path) {
        return Err(execution_error(
            ChannelExecutionErrorCode::SurfaceOperationNotSupported,
            "native Responses compaction endpoint was not supported in the channel check",
        ));
    }
    let selected_target_protocol = request
        .selected_target_protocol
        .as_deref()
        // A generation-protocol hint from platform routing must not redirect
        // resource/media operations away from their own API surface.
        .filter(|_| route.protocol.is_some())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let mut target = if opaque {
        channel_surface_target(channel, route.surface, route.protocol, true)
    } else if let Some(selected) = selected_target_protocol {
        crate::protocol::kind::ProtocolKind::parse(selected)
            .ok()
            .and_then(|protocol| channel_protocol_target(channel, protocol))
    } else {
        channel_surface_target(channel, route.surface, route.protocol, false)
    }
    .ok_or_else(|| {
        let message = if opaque {
            format!(
                "channel has no verified matching {} surface for opaque operation",
                route.surface.as_str()
            )
        } else if let Some(selected) = selected_target_protocol {
            format!("channel does not expose selected target protocol {selected}")
        } else {
            "channel has no executable API surface".to_string()
        };
        execution_error(
            ChannelExecutionErrorCode::SurfaceOperationNotSupported,
            message,
        )
    })?;
    validate_channel_v2(channel).map_err(|error| {
        execution_error(
            ChannelExecutionErrorCode::SurfaceOperationNotSupported,
            error,
        )
    })?;
    let body_text = std::str::from_utf8(&request.body).ok();
    let requested_model = body_text
        .and_then(|body| requested_model_from_body_or_path(body, &request.path))
        .or_else(|| requested_model_from_path(&request.path));
    let supplier = supplier_from_channel(channel);
    let mut upstream_model = request
        .selected_upstream_model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            supplier_upstream_model_for_request(&supplier, requested_model.as_deref())
        });
    if !opaque
        && channel.source_driver() == crate::source_driver::SourceDriverId::AnthropicApi
        && target.surface == crate::surface::ApiSurface::Anthropic
    {
        upstream_model = crate::config::without_context_hint(&upstream_model).to_string();
    }
    if route.protocol.is_some() {
        if let Some(protocol) =
            crate::coding_gateway::model_protocol(channel.source_driver(), &upstream_model)
        {
            let supported = crate::coding_gateway::catalog_protocol_support(
                &channel.capability_profiles,
                &upstream_model,
                target.protocol,
            )
            .unwrap_or_else(|| {
                crate::coding_gateway::model_supports_protocol(
                    channel.source_driver(),
                    &upstream_model,
                    target.protocol,
                )
            });
            if selected_target_protocol.is_some() && !supported {
                return Err(execution_error(
                    ChannelExecutionErrorCode::SurfaceOperationNotSupported,
                    format!(
                        "selected target {} does not match gateway model {upstream_model} dialect {protocol}",
                        target.protocol
                    ),
                ));
            }
            if !supported {
                target = channel_protocol_target(channel, protocol).ok_or_else(|| execution_error(
                ChannelExecutionErrorCode::SurfaceOperationNotSupported,
                format!("gateway model {upstream_model} requires an available {protocol} binding"),
            ))?;
            }
        }
    }
    let inbound_stream_requested = request.stream_requested.unwrap_or_else(|| {
        top_level_json_bool(&request.body, "stream")
            .unwrap_or_else(|| request.path.contains(":streamGenerateContent"))
    });

    let target_protocol = target.protocol.as_str().to_string();
    let inbound_protocol = route.protocol.map(|protocol| protocol.as_str().to_string());
    let upstream_stream_requested = if let Some(inbound_protocol) = &inbound_protocol {
        supplier_target_stream_requested(
            &supplier,
            inbound_protocol,
            &target_protocol,
            inbound_stream_requested,
        )
    } else {
        inbound_stream_requested
    };

    if matches!(
        route.operation,
        ApiOperation::CountTokens | ApiOperation::ResponsesInputTokens
    ) && (inbound_protocol
        .as_deref()
        .is_some_and(|inbound| inbound != target_protocol)
        // Gateway token-count support is a local estimate unless an actual
        // upstream operation has been independently verified.
        || (crate::coding_gateway::is_gateway(channel.source_driver())
            && crate::detection::responses_features::operation_support(
                &channel.detection_checks,
                &format!("{}.{}", route.surface.as_str(), route.operation.as_str()),
            ) != Some(true))
        || (route.operation == ApiOperation::ResponsesInputTokens
            && crate::detection::responses_features::operation_support(
                &channel.detection_checks,
                "openai.responses_input_tokens",
            ) == Some(false)))
    {
        let body = body_text.ok_or_else(|| {
            execution_error(
                ChannelExecutionErrorCode::ConversionFailed,
                "token-count operations require a UTF-8 JSON request body",
            )
        })?;
        let estimated = crate::supplier::estimate_subscription_tokens(body).max(1);
        let response_body = match route.surface {
            crate::surface::ApiSurface::Anthropic => serde_json::json!({
                "input_tokens": estimated,
            }),
            crate::surface::ApiSurface::Gemini => serde_json::json!({
                "totalTokens": estimated,
            }),
            crate::surface::ApiSurface::OpenAi => serde_json::json!({
                "object": "response.input_tokens",
                "input_tokens": estimated,
            }),
        };
        let summary = "The selected channel has no equivalent native token-count wire operation; CONST API returned a local estimate without sending a paid model request.";
        let fault = crate::supplier::ImprovementFault::conversion_warning(
            "token_count_locally_estimated",
            inbound_protocol.as_deref().unwrap_or_default(),
            &target_protocol,
            requested_model.as_deref().unwrap_or(&upstream_model),
            &request.path,
            summary,
        );
        let response = http::Response::builder()
            .status(http::StatusCode::OK)
            .header(http::header::CONTENT_TYPE, "application/json")
            .header(TOKEN_COUNT_ESTIMATED_HEADER, "true")
            .body(serde_json::to_vec(&response_body)?)?;
        return Ok(ChannelExecution {
            response: response.into(),
            inbound_protocol: inbound_protocol.clone(),
            target_protocol: inbound_protocol.clone().unwrap_or(target_protocol),
            upstream_model,
            inbound_stream_requested: false,
            native_passthrough: true,
            opaque: false,
            improvement_faults: vec![fault],
            tool_mapping: Default::default(),
        });
    }

    let resource_native_operation = matches!(
        route.operation,
        ApiOperation::RealtimeCallsCreate
            | ApiOperation::RealtimeLiveCallCreate
            | ApiOperation::ResponsesGet
            | ApiOperation::ResponsesDelete
            | ApiOperation::ResponsesCancel
            | ApiOperation::ResponsesInputItems
            | ApiOperation::ConversationsCreate
            | ApiOperation::ConversationsGet
            | ApiOperation::ConversationsUpdate
            | ApiOperation::ConversationsDelete
            | ApiOperation::ConversationItemsCreate
            | ApiOperation::ConversationItemsList
            | ApiOperation::ConversationItemGet
            | ApiOperation::ConversationItemDelete
            | ApiOperation::FilesCreate
            | ApiOperation::FilesList
            | ApiOperation::FilesGet
            | ApiOperation::FilesDelete
            | ApiOperation::FilesContent
            | ApiOperation::UploadsCreate
            | ApiOperation::UploadPartsCreate
            | ApiOperation::UploadsComplete
            | ApiOperation::UploadsCancel
            | ApiOperation::ImagesGenerations
            | ApiOperation::ImagesEdits
            | ApiOperation::ImagesVariations
            | ApiOperation::AudioSpeech
            | ApiOperation::AudioTranscriptions
            | ApiOperation::AudioTranslations
            | ApiOperation::VideosCreate
            | ApiOperation::VideosList
            | ApiOperation::VideosGet
            | ApiOperation::VideosDelete
            | ApiOperation::VideosContent
            | ApiOperation::VideosRemix
            | ApiOperation::VideosEdit
            | ApiOperation::VideosExtend
            | ApiOperation::BatchesCreate
            | ApiOperation::BatchesList
            | ApiOperation::BatchesGet
            | ApiOperation::BatchesCancel
            | ApiOperation::BatchesDelete
            | ApiOperation::BatchesResults
            | ApiOperation::ContentProvenanceChecks
            | ApiOperation::InteractionsCreate
            | ApiOperation::InteractionsGet
            | ApiOperation::InteractionsDelete
            | ApiOperation::InteractionsCancel
    );
    if resource_native_operation && target.surface != route.surface {
        return Err(execution_error(
            ChannelExecutionErrorCode::SurfaceOperationNotSupported,
            "stateful resource operations require a native matching API surface",
        ));
    }
    let mut tool_mapping = crate::protocol::conversion::ToolWireMap::default();
    let (mut upstream_path, mut upstream_body, native_passthrough, improvement_faults) =
        if resource_native_operation {
            (
                route.relative_path.clone(),
                request.body.clone(),
                true,
                Vec::new(),
            )
        } else if inbound_protocol.is_some() {
            let body = body_text.ok_or_else(|| {
                execution_error(
                    ChannelExecutionErrorCode::ConversionFailed,
                    "known generation operations require a UTF-8 JSON request body",
                )
            })?;
            let conversion_body = if upstream_stream_requested != inbound_stream_requested {
                json_body_with_stream(body, upstream_stream_requested).map_err(|error| {
                    execution_error(ChannelExecutionErrorCode::ConversionFailed, error)
                })?
            } else {
                body.to_string()
            };
            let conversion = upstream_request_for_api_format_with_profiles_report(
                &target_protocol,
                &request.path,
                &conversion_body,
                &upstream_model,
                &channel.capability_profiles,
            )
            .map_err(|error| execution_error(ChannelExecutionErrorCode::ConversionFailed, error))?;
            tool_mapping = conversion.tool_mapping;
            (
                conversion.path,
                Bytes::from(conversion.body),
                conversion.inbound_protocol == conversion.target_protocol
                    && upstream_stream_requested == inbound_stream_requested,
                conversion.faults,
            )
        } else if opaque {
            (
                route.relative_path.clone(),
                request.body.clone(),
                target.surface == route.surface,
                Vec::new(),
            )
        } else {
            let mut body = request.body.clone();
            if !upstream_model.trim().is_empty() {
                if let Ok(text) = std::str::from_utf8(&body) {
                    let rewritten = rewrite_supplier_body_model(text, &upstream_model);
                    body = Bytes::from(rewritten);
                }
            }
            // Embeddings has no protocol IR today. Preserve the compatibility attempt instead of
            // filtering the channel, and report the semantic risk until a real cross-surface adapter
            // exists.
            let improvement_faults = if route.operation == ApiOperation::Embeddings
                && target.surface != route.surface
            {
                let summary = format!(
                    "{} has no protocol-neutral cross-surface adapter; the original operation shape was forwarded as a compatibility attempt",
                    route.operation.as_str()
                );
                eprintln!(
                    "[const-api][protocol] conversion warning code=special_operation_approximated source={} target={} path={} summary={}",
                    route.surface.as_str(),
                    target_protocol,
                    request.path,
                    summary
                );
                vec![crate::supplier::ImprovementFault::conversion_warning(
                    "special_operation_approximated",
                    route.surface.as_str(),
                    &target_protocol,
                    requested_model.as_deref().unwrap_or(&upstream_model),
                    &request.path,
                    &summary,
                )]
            } else {
                Vec::new()
            };
            (
                supplier_upstream_path(&route.relative_path, &upstream_model),
                body,
                target.surface == route.surface,
                improvement_faults,
            )
        };

    let cross_protocol = inbound_protocol
        .as_deref()
        .is_some_and(|inbound| inbound != target_protocol);
    if cross_protocol
        && channel.source_driver() == crate::source_driver::SourceDriverId::OpenAiApi
        && matches!(
            target.protocol,
            crate::protocol::kind::ProtocolKind::OpenAiResponses
                | crate::protocol::kind::ProtocolKind::OpenAiChat
        )
    {
        if let Some(body) = std::str::from_utf8(&upstream_body).ok() {
            upstream_body = Bytes::from(openai_body_with_cache_identity(
                body,
                request.cache_identity.as_deref(),
            )?);
        }
    }
    if cross_protocol
        && channel.source_driver() == crate::source_driver::SourceDriverId::AnthropicApi
        && target.protocol == crate::protocol::kind::ProtocolKind::AnthropicMessages
    {
        if let Some(body) = std::str::from_utf8(&upstream_body).ok() {
            upstream_body = Bytes::from(anthropic_body_with_automatic_cache(body)?);
        }
    }

    // Gemini's generated stream path supplies an SSE default. A native caller's
    // explicit alt parameter owns the wire format; preserve its raw query once.
    if native_passthrough
        && target.protocol == crate::protocol::kind::ProtocolKind::GeminiNative
        && upstream_path.ends_with("?alt=sse")
        && request.raw_query.split('&').any(|pair| {
            let key = pair.split_once('=').map_or(pair, |(key, _)| key);
            urlencoding::decode(key).is_ok_and(|key| key == "alt")
        })
    {
        upstream_path.truncate(upstream_path.len() - "?alt=sse".len());
    }
    let url = resolve_target_url(
        channel,
        &target,
        &route,
        &request.method,
        &upstream_path,
        &upstream_model,
    )
    .map_err(|error| {
        execution_error(
            ChannelExecutionErrorCode::SurfaceOperationNotSupported,
            error,
        )
    })?;
    let url = crate::surface_wire::merge_raw_query(&url, &request.raw_query, request.has_query);
    validate_target_url(&url).map_err(|error| {
        execution_error(
            ChannelExecutionErrorCode::SurfaceOperationNotSupported,
            error,
        )
    })?;

    let target_host = Url::parse(&url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_string));
    if channel.source_driver() == crate::source_driver::SourceDriverId::OpenAiApi
        && target_host
            .as_deref()
            .is_some_and(|host| host.eq_ignore_ascii_case("api.openai.com"))
        && matches!(
            target.protocol,
            crate::protocol::kind::ProtocolKind::OpenAiResponses
                | crate::protocol::kind::ProtocolKind::OpenAiChat
        )
    {
        if let Some(identifier) = request.safety_identifier.as_deref() {
            upstream_body = Bytes::from(openai_body_with_safety_identifier(
                &upstream_body,
                identifier,
            ));
        }
    }
    if channel.source_driver() == crate::source_driver::SourceDriverId::AnthropicApi
        && target_host
            .as_deref()
            .is_some_and(|host| host.eq_ignore_ascii_case("api.anthropic.com"))
        && target.protocol == crate::protocol::kind::ProtocolKind::AnthropicMessages
    {
        if let Some(identifier) = request.safety_identifier.as_deref() {
            upstream_body = Bytes::from(anthropic_body_with_safety_identifier(
                &upstream_body,
                identifier,
            ));
        }
    }

    let mut headers = request.headers;
    strip_http_request_headers_for_channel(&mut headers, channel);
    crate::coding_gateway::prepare_headers(
        channel,
        route.operation,
        target.protocol,
        &mut headers,
        request.cache_identity.as_deref(),
    );
    crate::openrouter::prepare_headers(
        channel,
        route.operation,
        &mut headers,
        &upstream_body,
        request.cache_identity.as_deref(),
    );
    if !opaque {
        if !headers.contains_key(reqwest::header::CONTENT_TYPE)
            && !native_passthrough
            && (route.protocol.is_some() || !request.body.is_empty())
        {
            headers.insert(
                reqwest::header::CONTENT_TYPE,
                reqwest::header::HeaderValue::from_static("application/json"),
            );
        }
    }
    if !opaque && !native_passthrough {
        headers.insert(
            reqwest::header::ACCEPT_ENCODING,
            reqwest::header::HeaderValue::from_static("identity"),
        );
    }
    apply_upstream_auth(&mut headers, channel, &target, !opaque)
        .map_err(|error| execution_error(ChannelExecutionErrorCode::AuthenticationFailed, error))?;

    if !opaque {
        if let Ok(text) = std::str::from_utf8(&upstream_body) {
            if let std::borrow::Cow::Owned(body) =
                crate::openrouter::request_body(Some(channel), text)
            {
                upstream_body = Bytes::from(body);
            }
        }
    }

    if let Some(response) = crate::coding_plan::quota_rejection(channel, route.operation)? {
        return Ok(ChannelExecution {
            response,
            inbound_protocol,
            target_protocol,
            upstream_model,
            inbound_stream_requested,
            native_passthrough,
            opaque,
            improvement_faults,
            tool_mapping,
        });
    }
    let request = client
        .request(request.method, url)
        .headers(headers)
        .body(upstream_body);
    let mut response = crate::upstream_transport::send(request)
        .await
        .map_err(|error| {
            let code = if error.is_ambiguous() {
                ChannelExecutionErrorCode::AmbiguousTransport
            } else if error.is_timeout() {
                ChannelExecutionErrorCode::UpstreamTimeout
            } else {
                ChannelExecutionErrorCode::UpstreamRejected
            };
            execution_error_with_source(code, error)
        })?;
    strip_http_response_headers(response.headers_mut());
    let response = crate::openrouter::observe_response(channel, response);
    Ok(ChannelExecution {
        response,
        inbound_protocol,
        target_protocol,
        upstream_model,
        inbound_stream_requested,
        native_passthrough,
        opaque,
        improvement_faults,
        tool_mapping,
    })
}

pub(crate) fn openai_body_with_safety_identifier(body: &[u8], identifier: &str) -> Vec<u8> {
    let identifier = identifier.trim();
    if identifier.is_empty() || identifier.len() > 64 {
        return body.to_vec();
    }
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return body.to_vec();
    };
    let Some(object) = value.as_object_mut() else {
        return body.to_vec();
    };
    object.insert(
        "safety_identifier".to_string(),
        serde_json::Value::String(identifier.to_string()),
    );
    serde_json::to_vec(&value).unwrap_or_else(|_| body.to_vec())
}

fn anthropic_body_with_safety_identifier(body: &[u8], identifier: &str) -> Vec<u8> {
    let identifier = identifier.trim();
    if identifier.is_empty() || identifier.len() > 512 {
        return body.to_vec();
    }
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return body.to_vec();
    };
    let Some(object) = value.as_object_mut() else {
        return body.to_vec();
    };
    let metadata = object
        .entry("metadata")
        .or_insert_with(|| serde_json::json!({}));
    let Some(metadata) = metadata.as_object_mut() else {
        return body.to_vec();
    };
    metadata.insert(
        "user_id".to_string(),
        serde_json::Value::String(identifier.to_string()),
    );
    serde_json::to_vec(&value).unwrap_or_else(|_| body.to_vec())
}

pub(crate) async fn execute_native_streaming_channel_request(
    client: &Client,
    channel: &ChannelConfig,
    method: Method,
    path: &str,
    raw_query: &str,
    has_query: bool,
    mut headers: HeaderMap,
    body: reqwest::Body,
) -> Result<Response> {
    let route = resolve_api_route(method.as_str(), path, false).ok_or_else(|| {
        execution_error(
            ChannelExecutionErrorCode::SurfaceOperationNotSupported,
            "request path is outside the configured API surfaces",
        )
    })?;
    let typed_native_upload = crate::surface::api_operation_uses_streaming_upload(route.operation)
        && crate::surface::api_operation_requires_native_api_credential(route.operation);
    let opaque_native_upload = route.operation == ApiOperation::Opaque
        && crate::surface::native_opaque_streaming_upload_family(path).is_some();
    if !typed_native_upload && !opaque_native_upload {
        return Err(execution_error(
            ChannelExecutionErrorCode::SurfaceOperationNotSupported,
            "operation is not admitted by the native streaming-upload pipeline",
        ));
    }
    validate_channel_v2(channel).map_err(|error| {
        execution_error(
            ChannelExecutionErrorCode::SurfaceOperationNotSupported,
            error,
        )
    })?;
    let target =
        channel_surface_target(channel, route.surface, route.protocol, true).ok_or_else(|| {
            execution_error(
                ChannelExecutionErrorCode::SurfaceOperationNotSupported,
                format!(
                    "channel has no verified native {} surface",
                    route.surface.as_str()
                ),
            )
        })?;
    if target.surface != route.surface {
        return Err(execution_error(
            ChannelExecutionErrorCode::SurfaceOperationNotSupported,
            "streaming resource uploads require a native matching API surface",
        ));
    }
    let url = resolve_target_url(channel, &target, &route, &method, &route.relative_path, "")
        .and_then(|url| {
            let url = crate::surface_wire::merge_raw_query(&url, raw_query, has_query);
            validate_target_url(&url)?;
            Ok(url)
        })
        .map_err(|error| {
            execution_error(
                ChannelExecutionErrorCode::SurfaceOperationNotSupported,
                error,
            )
        })?;

    let content_length = headers.get(reqwest::header::CONTENT_LENGTH).cloned();
    strip_http_request_headers_for_channel(&mut headers, channel);
    if let Some(content_length) = content_length {
        headers.insert(reqwest::header::CONTENT_LENGTH, content_length);
    }
    headers.insert(
        reqwest::header::ACCEPT_ENCODING,
        reqwest::header::HeaderValue::from_static("identity"),
    );
    apply_upstream_auth(&mut headers, channel, &target, true)
        .map_err(|error| execution_error(ChannelExecutionErrorCode::AuthenticationFailed, error))?;

    let request = client.request(method, url).headers(headers).body(body);
    let mut response = crate::upstream_transport::send(request)
        .await
        .map_err(|error| {
            let code = if error.is_ambiguous() {
                ChannelExecutionErrorCode::AmbiguousTransport
            } else if error.is_timeout() {
                ChannelExecutionErrorCode::UpstreamTimeout
            } else {
                ChannelExecutionErrorCode::UpstreamRejected
            };
            execution_error_with_source(code, error)
        })?;
    strip_http_response_headers(response.headers_mut());
    Ok(response)
}

pub(crate) fn resolve_target_url(
    channel: &ChannelConfig,
    target: &ChannelSurfaceTarget,
    route: &ApiRoute,
    method: &Method,
    upstream_path: &str,
    upstream_model: &str,
) -> Result<String> {
    if route.operation == ApiOperation::Opaque {
        return join_opaque_target_url(&target.base_url, upstream_path);
    }
    // URL parsers may normalize encoded dot segments while joining. Apply the
    // same origin-form path safety gate before that happens, while retaining
    // protocol-owned query parameters such as Gemini's `?alt=sse`.
    validate_typed_provider_relative_target(upstream_path)?;

    if let Some(endpoint) = channel.surface_bindings.iter().find_map(|binding| {
        (binding.surface == target.surface).then(|| {
            binding.operation_overrides.iter().find(|endpoint| {
                endpoint.operation.trim() == route.operation.as_str()
                    && endpoint.method.trim().eq_ignore_ascii_case(method.as_str())
            })
        })?
    }) {
        let url = endpoint
            .url
            .trim()
            .replace("{model}", &urlencoding::encode(upstream_model));
        validate_target_url(&url)?;
        let base = parse_http_surface_base(&target.base_url)?;
        let override_url = Url::parse(&url).context("invalid operation endpoint override URL")?;
        if override_url.scheme() != base.scheme()
            || override_url.host_str() != base.host_str()
            || override_url.port_or_known_default() != base.port_or_known_default()
        {
            return Err(anyhow!(
                "operation endpoint override cannot replace the configured authority"
            ));
        }
        return Ok(url);
    }

    let base_url = match target.endpoint_profile.as_str() {
        "google_openai" => gemini_openai_base(&target.base_url),
        "bedrock_mantle_anthropic" => bedrock_anthropic_base(&target.base_url),
        _ => target.base_url.clone(),
    };
    if target.protocol == crate::protocol::kind::ProtocolKind::GeminiNative {
        crate::coding_gateway::gemini_url(channel.source_driver(), &base_url, upstream_path)
    } else {
        Ok(join_upstream_url(&base_url, upstream_path))
    }
}

fn join_opaque_target_url(base_url: &str, provider_relative_path: &str) -> Result<String> {
    parse_http_surface_base(base_url)?;
    validate_opaque_provider_relative_path(provider_relative_path)?;
    Ok(join_upstream_url(base_url, provider_relative_path))
}

fn validate_typed_provider_relative_target(target: &str) -> Result<()> {
    if target.contains('#') {
        return Err(anyhow!(
            "typed provider-relative target must not contain a fragment"
        ));
    }
    let path = target
        .split_once('?')
        .map(|(path, _query)| path)
        .unwrap_or(target);
    validate_provider_relative_path_component(path)
}

fn validate_opaque_provider_relative_path(path: &str) -> Result<()> {
    if path.contains('?') || path.contains('#') {
        return Err(anyhow!(
            "opaque provider-relative path must not contain a query or fragment"
        ));
    }
    validate_provider_relative_path_component(path)
}

fn validate_provider_relative_path_component(path: &str) -> Result<()> {
    if !path.starts_with('/') || path.starts_with("//") {
        return Err(anyhow!(
            "provider-relative path must use origin-form and must not replace authority"
        ));
    }
    if path.contains('\\') {
        return Err(anyhow!(
            "provider-relative path must not contain backslashes"
        ));
    }

    let (decoded, _) = decode_percent_layer(path.as_bytes(), true)?;
    validate_decoded_opaque_path(&decoded)?;
    if contains_percent_escape(&decoded) {
        return Err(anyhow!(
            "provider-relative path contains recursive percent encoding"
        ));
    }
    Ok(())
}

fn decode_percent_layer(bytes: &[u8], reject_invalid: bool) -> Result<(Vec<u8>, bool)> {
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut changed = false;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        let value = bytes
            .get(index + 1)
            .and_then(|high| hex_value(*high))
            .zip(bytes.get(index + 2).and_then(|low| hex_value(*low)))
            .map(|(high, low)| (high << 4) | low);
        if let Some(value) = value {
            decoded.push(value);
            changed = true;
            index += 3;
        } else if reject_invalid {
            return Err(anyhow!(
                "provider-relative path contains an invalid percent escape"
            ));
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    Ok((decoded, changed))
}

fn validate_decoded_opaque_path(path: &[u8]) -> Result<()> {
    if path.starts_with(b"//") {
        return Err(anyhow!(
            "provider-relative path must use origin-form and must not replace authority"
        ));
    }
    if path.contains(&b'\\') {
        return Err(anyhow!(
            "provider-relative path must not contain encoded backslashes"
        ));
    }
    if path
        .split(|byte| *byte == b'/')
        .any(|segment| segment == b"." || segment == b"..")
    {
        return Err(anyhow!(
            "provider-relative path must not contain path traversal"
        ));
    }
    Ok(())
}

fn contains_percent_escape(bytes: &[u8]) -> bool {
    bytes.windows(3).any(|escape| {
        escape[0] == b'%' && hex_value(escape[1]).is_some() && hex_value(escape[2]).is_some()
    })
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub(crate) fn apply_upstream_auth(
    headers: &mut HeaderMap,
    channel: &ChannelConfig,
    target: &ChannelSurfaceTarget,
    add_protocol_defaults: bool,
) -> Result<()> {
    let key = channel.upstream_api_key.trim();
    if key.is_empty() || target.auth_scheme == "none" || target.auth_scheme == "subscription" {
        return Ok(());
    }
    let value = reqwest::header::HeaderValue::from_str(key)?;
    if add_protocol_defaults
        && target.protocol == crate::protocol::kind::ProtocolKind::AnthropicMessages
    {
        headers
            .entry("anthropic-version")
            .or_insert(reqwest::header::HeaderValue::from_static("2023-06-01"));
    }
    match target.auth_scheme.as_str() {
        "api_key" => {
            headers.insert("api-key", value);
        }
        "x_api_key" => {
            headers.insert("x-api-key", value);
        }
        "x_goog_api_key" => {
            headers.insert("x-goog-api-key", value);
        }
        _ => {
            headers.insert(
                reqwest::header::AUTHORIZATION,
                reqwest::header::HeaderValue::from_str(&format!("Bearer {key}"))?,
            );
        }
    }
    Ok(())
}

fn strip_http_hop_by_hop_headers(headers: &mut HeaderMap) {
    let connection_headers = headers
        .get_all(reqwest::header::CONNECTION)
        .iter()
        .flat_map(|value| value.as_bytes().split(|byte| *byte == b','))
        .filter_map(|name| {
            let start = name.iter().position(|byte| !matches!(byte, b' ' | b'\t'))?;
            let end = name
                .iter()
                .rposition(|byte| !matches!(byte, b' ' | b'\t'))?
                + 1;
            reqwest::header::HeaderName::from_bytes(&name[start..end]).ok()
        })
        .collect::<Vec<_>>();

    for name in [
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
        "proxy-connection",
    ] {
        headers.remove(name);
    }
    for name in connection_headers {
        headers.remove(name);
    }
}

pub(crate) const ANTHROPIC_1M_BETA: &str = "context-1m-2025-08-07";

pub(crate) fn strip_http_request_headers(headers: &mut HeaderMap) {
    strip_http_hop_by_hop_headers(headers);
    for name in [
        "authorization",
        "host",
        "content-length",
        "proxy-authorization",
        "api-key",
        "x-api-key",
        "x-goog-api-key",
        "cookie",
        "set-cookie",
        SKIP_LOCAL_SHORT_CIRCUIT_HEADER,
        USE_LOCAL_SHORT_CIRCUIT_HEADER,
        crate::lan_share::LAN_SHARE_PATH_HEADER,
        crate::lan_share::LAN_SHARE_INTERNAL_PATH_HEADER,
    ] {
        headers.remove(name);
    }
}

fn strip_http_request_headers_for_channel(headers: &mut HeaderMap, channel: &ChannelConfig) {
    let lan_share_path = headers
        .remove(crate::lan_share::LAN_SHARE_INTERNAL_PATH_HEADER)
        .filter(|_| crate::source_driver::channel_is_lan_share(channel));
    strip_http_request_headers(headers);
    if let Some(path) = lan_share_path {
        headers.insert(crate::lan_share::LAN_SHARE_PATH_HEADER, path);
    }
    crate::channel_user_agent::apply_to_headers(channel, headers);
}

pub(crate) fn strip_http_response_headers(headers: &mut HeaderMap) {
    strip_http_hop_by_hop_headers(headers);
    for name in [
        "authorization",
        "proxy-authorization",
        "api-key",
        "x-api-key",
        "x-goog-api-key",
        crate::lan_share::LAN_SHARE_PATH_HEADER,
        crate::lan_share::LAN_SHARE_INTERNAL_PATH_HEADER,
    ] {
        headers.remove(name);
    }
}

fn validate_target_url(url: &str) -> Result<()> {
    let parsed = Url::parse(url.trim()).context("invalid configured upstream URL")?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(anyhow!("configured upstream URL must use HTTP or HTTPS"));
    }
    if parsed.username() != "" || parsed.password().is_some() {
        return Err(anyhow!(
            "configured upstream URL must not contain user info"
        ));
    }
    if parsed.fragment().is_some() {
        return Err(anyhow!(
            "configured upstream URL must not contain a fragment"
        ));
    }
    Ok(())
}

fn request_is_websocket(headers: &HeaderMap) -> bool {
    headers
        .get(reqwest::header::UPGRADE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
}

fn requested_model_from_path(path: &str) -> Option<String> {
    crate::supplier::gemini_model_from_path(path).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        ChannelProtocolBinding, ChannelSurfaceBinding, ProtocolVerification, SurfaceVerification,
    };
    use crate::surface::ApiSurface;
    use crate::{OperationEndpointOverride, channel_from_supplier, default_supplier_config};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::oneshot;

    #[derive(Debug)]
    struct CapturedRequest {
        method: String,
        target: String,
        headers: String,
        body: Vec<u8>,
    }

    async fn capture_server() -> (
        String,
        oneshot::Receiver<CapturedRequest>,
        tokio::task::JoinHandle<()>,
    ) {
        capture_server_with_response(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".to_vec(),
        )
        .await
    }

    async fn capture_server_with_response(
        response: Vec<u8>,
    ) -> (
        String,
        oneshot::Receiver<CapturedRequest>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = oneshot::channel();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let header_end = loop {
                let mut chunk = [0_u8; 1024];
                let read = stream.read(&mut chunk).await.unwrap();
                assert!(read > 0, "connection closed before request headers");
                request.extend_from_slice(&chunk[..read]);
                if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    break index + 4;
                }
            };
            let headers = std::str::from_utf8(&request[..header_end])
                .unwrap()
                .to_string();
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            while request.len() < header_end + content_length {
                let mut chunk = [0_u8; 1024];
                let read = stream.read(&mut chunk).await.unwrap();
                assert!(read > 0, "connection closed before request body");
                request.extend_from_slice(&chunk[..read]);
            }
            let mut request_line = headers.lines().next().unwrap().split_whitespace();
            let captured = CapturedRequest {
                method: request_line.next().unwrap().to_string(),
                target: request_line.next().unwrap().to_string(),
                headers,
                body: request[header_end..header_end + content_length].to_vec(),
            };
            sender.send(captured).unwrap();
            stream.write_all(&response).await.unwrap();
        });
        (format!("http://{address}"), receiver, task)
    }

    fn channel_for_surface(
        base_url: &str,
        surface: ApiSurface,
        verification_state: &str,
    ) -> ChannelConfig {
        let mut channel =
            channel_from_supplier("channel-1".to_string(), &default_supplier_config());
        let protocol = match surface {
            ApiSurface::OpenAi => "openai_chat",
            ApiSurface::Anthropic => "anthropic_messages",
            ApiSurface::Gemini => "gemini_native",
        };
        channel.set_source_driver(crate::source_driver::SourceDriverId::CustomEndpoint);
        channel.v2.default_target = crate::model::ChannelTarget {
            surface,
            protocol: crate::protocol::kind::ProtocolKind::parse(protocol).expect("test protocol"),
        };
        channel.v2.credential_ref.clear();
        channel.kind = "custom_endpoint".to_string();
        channel.api_format = protocol.to_string();
        channel.upstream_base_url = base_url.to_string();
        channel.upstream_api_key.clear();
        channel.surface_bindings = vec![ChannelSurfaceBinding {
            surface,
            base_url: base_url.to_string(),
            endpoint_profile: format!("{}_compatible", surface.as_str()),
            auth_scheme: "none".to_string(),
            protocols: vec![ChannelProtocolBinding {
                protocol: protocol.to_string(),
                preferred: true,
                verification: ProtocolVerification {
                    state: "verified".to_string(),
                    checked_at_unix: 1,
                    summary: String::new(),
                },
            }],
            operation_overrides: Vec::new(),
            verification: SurfaceVerification {
                state: verification_state.to_string(),
                checked_at_unix: 1,
                summary: String::new(),
            },
        }];
        channel
    }

    fn request(method: Method, path: &str, raw_query: &str, body: Bytes) -> ChannelRequest {
        ChannelRequest {
            method,
            path: path.to_string(),
            raw_query: raw_query.to_string(),
            has_query: !raw_query.is_empty(),
            headers: HeaderMap::new(),
            body,
            stream_requested: None,
            selected_upstream_model: Some("selected-target-model".to_string()),
            selected_target_protocol: None,
            cache_identity: None,
            safety_identifier: None,
        }
    }

    #[test]
    fn openai_safety_identifier_is_overridden_without_touching_invalid_input() {
        let body = br#"{"model":"gpt-5.6","safety_identifier":"caller"}"#;
        let updated = openai_body_with_safety_identifier(body, "const_stable_user");
        let value: serde_json::Value = serde_json::from_slice(&updated).expect("updated JSON");
        assert_eq!(value["safety_identifier"], "const_stable_user");

        let invalid = b"not-json";
        assert_eq!(
            openai_body_with_safety_identifier(invalid, "const_stable_user"),
            invalid
        );
        assert_eq!(
            openai_body_with_safety_identifier(body, &"x".repeat(65)),
            body
        );
    }

    #[tokio::test]
    async fn coding_gateways_preserve_native_tools_headers_and_streams() {
        use crate::source_driver::SourceDriverId;
        use reqwest::header::HeaderValue;
        for (driver, model, protocol, path) in [
            (
                SourceDriverId::OpencodeGo,
                "gpt-5.6-luna",
                "openai_responses",
                "/v1/responses",
            ),
            (
                SourceDriverId::OpencodeGo,
                "qwen3.8-max",
                "anthropic_messages",
                "/v1/messages",
            ),
            (
                SourceDriverId::OpencodeZen,
                "minimax-m3",
                "openai_chat",
                "/v1/chat/completions",
            ),
            (
                SourceDriverId::KiloGateway,
                "openai/gpt-5.6-luna",
                "openai_chat",
                "/v1/chat/completions",
            ),
            (
                SourceDriverId::ClineApi,
                "anthropic/claude-sonnet-5",
                "openai_chat",
                "/v1/chat/completions",
            ),
            (
                SourceDriverId::CommandCode,
                "openai/gpt-5.6-luna",
                "openai_responses",
                "/v1/responses",
            ),
            (
                SourceDriverId::CommandCode,
                "anthropic/claude-sonnet-5",
                "anthropic_messages",
                "/v1/messages",
            ),
            (
                SourceDriverId::KimiCode,
                "k3",
                "openai_responses",
                "/v1/responses",
            ),
            (
                SourceDriverId::KimiCode,
                "k3",
                "anthropic_messages",
                "/v1/messages",
            ),
            (
                SourceDriverId::GlmCodingPlan,
                "glm-5.3",
                "anthropic_messages",
                "/v1/messages",
            ),
            (
                SourceDriverId::MinimaxTokenPlan,
                "MiniMax-M3",
                "anthropic_messages",
                "/v1/messages",
            ),
            (
                SourceDriverId::OllamaCloud,
                "gpt-oss:120b",
                "openai_chat",
                "/v1/chat/completions",
            ),
            (
                SourceDriverId::OllamaCloud,
                "gpt-oss:120b",
                "anthropic_messages",
                "/v1/messages",
            ),
        ] {
            for stream in [false, true] {
                let response_body = if stream {
                    "data: {\"future_event\":\"keep\"}\n\ndata: [DONE]\n\n"
                } else {
                    "{\"future_result\":\"keep\"}"
                };
                let content_type = if stream {
                    "text/event-stream"
                } else {
                    "application/json"
                };
                let (base, captured, server) = capture_server_with_response(format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}", response_body.len()).into_bytes()).await;
                let channel = crate::coding_gateway::tests::channel(driver, &format!("{base}/v1"));
                let body = format!(
                    r#"{{ "model":"{model}","stream":{stream},"tools":[{{"type":"custom","name":"edit","format":{{"type":"text"}}}}],"future_option":{{"keep":true}},"input":[{{"additional_tools":[]}}] }}"#
                );
                let inbound_path = if protocol == "anthropic_messages" {
                    format!("/anthropic{path}")
                } else {
                    path.into()
                };
                let mut req = request(Method::POST, &inbound_path, "", Bytes::from(body.clone()));
                req.selected_upstream_model = Some(model.into());
                req.selected_target_protocol = Some(protocol.into());
                req.cache_identity = Some("user-model-session".into());
                req.headers.insert(
                    "user-agent",
                    HeaderValue::from_static("real-coding-agent/2.0"),
                );
                req.headers.insert(
                    "authorization",
                    HeaderValue::from_static("Bearer downstream-secret"),
                );
                req.headers
                    .insert("anthropic-beta", HeaderValue::from_static("future-beta"));
                if stream {
                    req.headers.insert(
                        "anthropic-version",
                        HeaderValue::from_static("future-version"),
                    );
                }
                let execution = execute_channel_request(&Client::new(), &channel, req)
                    .await
                    .unwrap();
                assert!(execution.native_passthrough);
                assert_eq!(execution.target_protocol, protocol);
                assert_eq!(execution.response.bytes().await.unwrap(), response_body);
                let captured = captured.await.unwrap();
                assert_eq!(captured.target, path);
                assert_eq!(captured.body, body.as_bytes());
                assert_eq!(
                    captured_header(&captured, "authorization"),
                    Some(format!("Bearer {}", channel.v2.credential_ref).as_str())
                );
                if protocol == "anthropic_messages" {
                    assert_eq!(
                        captured_header(&captured, "anthropic-version"),
                        Some(if stream {
                            "future-version"
                        } else {
                            "2023-06-01"
                        })
                    );
                }
                assert_eq!(
                    captured_header(&captured, "user-agent"),
                    Some("real-coding-agent/2.0")
                );
                assert_eq!(
                    captured_header(&captured, "anthropic-beta"),
                    Some("future-beta")
                );
                if matches!(
                    driver,
                    SourceDriverId::OpencodeGo | SourceDriverId::OpencodeZen
                ) {
                    assert_eq!(
                        captured_header(&captured, "x-opencode-session"),
                        Some("user-model-session")
                    );
                }
                server.await.unwrap();
            }
        }
    }

    #[test]
    fn anthropic_safety_identifier_preserves_other_metadata() {
        let body = br#"{"model":"claude-opus-5","metadata":{"trace":"keep","user_id":"caller"}}"#;
        let updated = anthropic_body_with_safety_identifier(body, "const_stable_user");
        let value: serde_json::Value = serde_json::from_slice(&updated).expect("updated JSON");
        assert_eq!(value["metadata"]["user_id"], "const_stable_user");
        assert_eq!(value["metadata"]["trace"], "keep");
    }

    #[tokio::test]
    async fn coding_gateway_zen_gemini_preserves_native_body_query_auth_and_sse() {
        use crate::source_driver::SourceDriverId;
        use reqwest::header::HeaderValue;
        // Whitespace and unknown extensions must survive same-protocol forwarding.
        let body = r#"{ "contents":[{"role":"user","parts":[{"text":"look"},{"inlineData":{"mimeType":"image/png","data":"aW1hZ2U="}}]},{"role":"model","parts":[{"functionCall":{"name":"lookup","args":{}},"thoughtSignature":"keep-signature"}]}],"tools":[{"functionDeclarations":[{"name":"lookup","parameters":{"type":"OBJECT"}}]}],"cachedContent":"cachedContents/kept","generationConfig":{"thinkingConfig":{"includeThoughts":true}},"futureOption":{"keep":true} }"#;
        for stream in [false, true] {
            let action = if stream {
                "streamGenerateContent"
            } else {
                "generateContent"
            };
            let query = if stream {
                "alt=sse&future=keep%2Bvalue"
            } else {
                "future=keep%2Bvalue"
            };
            let response_body = if stream {
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"name\":\"lookup\",\"args\":{}},\"thoughtSignature\":\"keep\"}]}}],\"futureEvent\":true}\n\n"
            } else {
                "{\"futureResult\":true}"
            };
            let content_type = if stream {
                "text/event-stream"
            } else {
                "application/json"
            };
            let (base, captured, server) = capture_server_with_response(format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}", response_body.len(),
            ).into_bytes()).await;
            let channel = crate::coding_gateway::tests::channel(
                SourceDriverId::OpencodeZen,
                &format!("{base}/zen/v1"),
            );
            let mut req = request(
                Method::POST,
                &format!("/gemini/v1beta/models/gemini-3.8-flash:{action}"),
                query,
                Bytes::from_static(body.as_bytes()),
            );
            req.selected_upstream_model = Some("gemini-3.8-flash".into());
            req.cache_identity = Some("stable-user-session".into());
            req.headers.insert(
                "authorization",
                HeaderValue::from_static("Bearer downstream-secret"),
            );
            req.headers.insert(
                "x-goog-api-key",
                HeaderValue::from_static("downstream-secret"),
            );
            req.headers
                .insert("user-agent", HeaderValue::from_static("actual-caller/1.0"));
            if stream {
                req.headers.insert(
                    "x-opencode-session",
                    HeaderValue::from_static("caller-session"),
                );
            }
            let execution = execute_channel_request(&Client::new(), &channel, req)
                .await
                .unwrap();
            assert!(execution.native_passthrough);
            assert_eq!(execution.target_protocol, "gemini_native");
            assert_eq!(execution.response.bytes().await.unwrap(), response_body);
            let captured = captured.await.unwrap();
            assert_eq!(
                captured.target,
                format!("/zen/v1/models/gemini-3.8-flash:{action}?{query}")
            );
            assert_eq!(captured.body, body.as_bytes());
            assert_eq!(
                captured_header(&captured, "x-goog-api-key"),
                Some("test-key")
            );
            assert_eq!(captured_header(&captured, "authorization"), None);
            assert_eq!(
                captured_header(&captured, "user-agent"),
                Some("actual-caller/1.0")
            );
            assert_eq!(
                captured_header(&captured, "x-opencode-session"),
                Some(if stream {
                    "caller-session"
                } else {
                    "stable-user-session"
                })
            );
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn coding_gateway_token_count_preflight_does_not_call_upstream() {
        let channel = crate::coding_gateway::tests::channel(
            crate::source_driver::SourceDriverId::OpencodeZen,
            "http://127.0.0.1:1/zen/v1",
        );
        let req = request(
            Method::POST,
            "/gemini/v1beta/models/gemini-3.8-flash:countTokens",
            "",
            Bytes::from_static(br#"{"contents":[{"parts":[{"text":"hello"}]}]}"#),
        );
        let execution = execute_channel_request(&Client::new(), &channel, req)
            .await
            .unwrap();
        assert_eq!(execution.response.status(), http::StatusCode::OK);
        assert_eq!(
            execution.response.headers()[TOKEN_COUNT_ESTIMATED_HEADER],
            "true"
        );
        let value: serde_json::Value = execution.response.json().await.unwrap();
        assert!(value["totalTokens"].as_u64().unwrap() > 0);
    }

    fn captured_header<'a>(captured: &'a CapturedRequest, name: &str) -> Option<&'a str> {
        captured.headers.lines().find_map(|line| {
            let (candidate, value) = line.split_once(':')?;
            candidate.eq_ignore_ascii_case(name).then(|| value.trim())
        })
    }

    #[test]
    fn execution_backend_uses_only_typed_source_driver_and_keeps_opaque_on_surface_guard() {
        let cases = [
            (
                crate::source_driver::SourceDriverId::OpenAiSubscription,
                ChannelExecutionBackend::RetainedSubscription { provider: "openai" },
            ),
            (
                crate::source_driver::SourceDriverId::ClaudeSubscription,
                ChannelExecutionBackend::RetainedSubscription { provider: "claude" },
            ),
            (
                crate::source_driver::SourceDriverId::GeminiSubscription,
                ChannelExecutionBackend::RetainedSubscription {
                    provider: "antigravity",
                },
            ),
            (
                crate::source_driver::SourceDriverId::GrokSubscription,
                ChannelExecutionBackend::RetainedSubscription { provider: "grok" },
            ),
            (
                crate::source_driver::SourceDriverId::CustomEndpoint,
                ChannelExecutionBackend::HttpSurface,
            ),
        ];

        for (source_driver, expected) in cases {
            let mut channel =
                channel_from_supplier("channel-backend".to_string(), &default_supplier_config());
            channel.set_source_driver(source_driver);
            channel.kind = "conflicting-legacy-kind".to_string();
            channel.api_format = "gemini_native".to_string();
            channel.subscription.platform = "conflicting-legacy-provider".to_string();

            assert_eq!(
                channel_execution_backend(&channel, ApiOperation::ChatCompletions).unwrap(),
                expected
            );
            assert_eq!(
                channel_execution_backend(&channel, ApiOperation::Opaque).unwrap(),
                ChannelExecutionBackend::HttpSurface
            );
        }
    }

    #[test]
    fn retained_subscription_admission_blocks_unsupported_and_approximate_operations() {
        let mut channel = channel_from_supplier(
            "subscription-admission".to_string(),
            &default_supplier_config(),
        );

        channel.set_source_driver(crate::source_driver::SourceDriverId::OpenAiSubscription);
        let responses = resolve_api_route("POST", "/v1/responses", false).unwrap();
        admit_retained_subscription_operation(&channel, &responses, true)
            .expect("OpenAI subscription Responses SSE");
        let embeddings = resolve_api_route("POST", "/v1/embeddings", false).unwrap();
        let unsupported =
            admit_retained_subscription_operation(&channel, &embeddings, false).unwrap_err();
        assert_eq!(
            channel_execution_error_code(&unsupported),
            Some("surface_operation_not_supported")
        );
        let response_get = resolve_api_route("GET", "/v1/responses/resp_123", false).unwrap();
        let lifecycle =
            admit_retained_subscription_operation(&channel, &response_get, false).unwrap_err();
        assert_eq!(
            channel_execution_error_code(&lifecycle),
            Some("surface_operation_not_supported")
        );

        channel.set_source_driver(crate::source_driver::SourceDriverId::GeminiSubscription);
        let count = resolve_api_route(
            "POST",
            "/gemini/v1beta/models/gemini-test:countTokens",
            false,
        )
        .unwrap();
        admit_retained_subscription_operation(&channel, &count, false)
            .expect("Antigravity countTokens");
        let embed = resolve_api_route(
            "POST",
            "/gemini/v1beta/models/gemini-test:embedContent",
            false,
        )
        .unwrap();
        let approximate =
            admit_retained_subscription_operation(&channel, &embed, false).unwrap_err();
        assert_eq!(
            channel_execution_error_code(&approximate),
            Some("surface_operation_not_supported")
        );
        assert!(approximate.to_string().contains("disabled"));
    }

    #[tokio::test]
    async fn resource_and_media_requests_keep_their_surface_despite_global_default() {
        for selected_protocol in [None, Some("anthropic_messages")] {
            for (method, path, body) in [
                (Method::GET, "/v1/files", Bytes::new()),
                (
                    Method::POST,
                    "/v1/embeddings",
                    Bytes::from_static(br#"{"model":"selected-target-model","input":"hello"}"#),
                ),
            ] {
                let (base_url, receiver, server) = capture_server().await;
                let mut channel = channel_for_surface(&base_url, ApiSurface::OpenAi, "verified");
                let anthropic =
                    channel_for_surface("http://127.0.0.1:1", ApiSurface::Anthropic, "verified");
                channel.surface_bindings.extend(anthropic.surface_bindings);
                channel.v2.default_target = anthropic.v2.default_target;
                let mut inbound = request(method.clone(), path, "", body.clone());
                inbound.selected_target_protocol = selected_protocol.map(str::to_string);
                execute_channel_request(&Client::new(), &channel, inbound)
                    .await
                    .unwrap();
                let captured = receiver.await.unwrap();
                server.await.unwrap();
                assert_eq!(captured.method, method.as_str());
                assert_eq!(captured.target, path);
                assert_eq!(captured.body, body);
            }
        }
    }

    #[tokio::test]
    async fn response_lifecycle_preserves_native_api_method_path_and_query() {
        let (base_url, receiver, server) = capture_server().await;
        let mut channel = channel_for_surface(&base_url, ApiSurface::OpenAi, "verified");
        channel.set_source_driver(crate::source_driver::SourceDriverId::OpenAiApi);
        channel.kind = "openai".to_string();
        channel.api_format = "openai_responses".to_string();
        channel.upstream_api_key = "sk-test".to_string();
        channel.surface_bindings = crate::source_driver_surface_bindings(
            crate::source_driver::SourceDriverId::OpenAiApi,
            &base_url,
        );
        for binding in &mut channel.surface_bindings {
            binding.verification = SurfaceVerification {
                state: "verified".to_string(),
                checked_at_unix: 1,
                summary: String::new(),
            };
            for protocol in &mut binding.protocols {
                protocol.verification = ProtocolVerification {
                    state: "verified".to_string(),
                    checked_at_unix: 1,
                    summary: String::new(),
                };
            }
        }
        channel.v2.default_target.protocol = crate::protocol::kind::ProtocolKind::OpenAiResponses;

        let mut lifecycle_request = request(
            Method::GET,
            "/v1/responses/resp_123",
            "include=input_items&include=output",
            Bytes::new(),
        );
        lifecycle_request.selected_target_protocol = Some("openai_responses".to_string());
        execute_channel_request(&Client::new(), &channel, lifecycle_request)
            .await
            .unwrap();
        let captured = receiver.await.unwrap();
        server.await.unwrap();
        assert_eq!(captured.method, "GET");
        assert_eq!(
            captured.target,
            "/v1/responses/resp_123?include=input_items&include=output"
        );
        assert!(captured.body.is_empty());
    }

    #[tokio::test]
    async fn gemini_interactions_preserve_native_lifecycle_method_path_query_and_body() {
        let cases = [
            (
                Method::POST,
                "/gemini/v1beta/interactions",
                "alt=sse",
                br#"{"model":"gemini-test","input":"hello","stream":true,"store":true,"background":false}"#
                    .as_slice(),
                "/v1beta/interactions?alt=sse",
            ),
            (
                Method::GET,
                "/gemini/v1beta/interactions/interaction_1",
                "stream=true&last_event_id=evt_7",
                b"".as_slice(),
                "/v1beta/interactions/interaction_1?stream=true&last_event_id=evt_7",
            ),
            (
                Method::POST,
                "/gemini/v1beta/interactions/interaction_1/cancel",
                "",
                b"".as_slice(),
                "/v1beta/interactions/interaction_1/cancel",
            ),
            (
                Method::DELETE,
                "/gemini/v1beta/interactions/interaction_1",
                "",
                b"".as_slice(),
                "/v1beta/interactions/interaction_1",
            ),
        ];

        for (method, path, query, body, expected_target) in cases {
            let (base_url, receiver, server) = capture_server().await;
            let mut channel = channel_for_surface(&base_url, ApiSurface::Gemini, "verified");
            channel.set_source_driver(crate::source_driver::SourceDriverId::GeminiApi);
            channel.surface_bindings = crate::source_driver_surface_bindings(
                crate::source_driver::SourceDriverId::GeminiApi,
                &base_url,
            );
            channel.surface_bindings[0].verification.state = "verified".to_string();
            channel.surface_bindings[0].protocols[0].verification.state = "verified".to_string();
            channel.upstream_api_key = "gemini-api-key".to_string();

            let execution = execute_channel_request(
                &Client::new(),
                &channel,
                request(method.clone(), path, query, Bytes::copy_from_slice(body)),
            )
            .await
            .expect("native Gemini Interactions request");
            let captured = receiver.await.expect("captured Interactions request");
            server.await.expect("capture server");

            assert!(execution.native_passthrough);
            assert!(!execution.opaque);
            assert_eq!(captured.method, method.as_str());
            assert_eq!(captured.target, expected_target);
            assert_eq!(captured.body, body);
            assert_eq!(
                captured_header(&captured, "x-goog-api-key"),
                Some("gemini-api-key")
            );
        }
    }

    #[tokio::test]
    async fn opaque_json_preserves_method_path_query_and_body_when_model_is_selected() {
        let (base_url, receiver, server) = capture_server().await;
        let mut channel = channel_for_surface(&base_url, ApiSurface::Gemini, "verified");
        channel.surface_bindings[0].operation_overrides = vec![OperationEndpointOverride {
            operation: "opaque".to_string(),
            method: "PATCH".to_string(),
            url: format!("{base_url}/rewritten/{{model}}"),
        }];
        let body = Bytes::from_static(
            br#"{ "model" : "body-model", "contents" : [{"text":"keep formatting"}] }"#,
        );

        let execution = execute_channel_request(
            &Client::new(),
            &channel,
            request(
                Method::PATCH,
                "/gemini/v1beta/models/path-model:futureAction",
                "alt=json&key=value%2Fpart",
                body.clone(),
            ),
        )
        .await
        .unwrap();
        let captured = receiver.await.unwrap();
        server.await.unwrap();

        assert!(execution.opaque);
        assert_eq!(execution.upstream_model, "selected-target-model");
        assert_eq!(captured.method, "PATCH");
        assert_eq!(
            captured.target,
            "/v1beta/models/path-model:futureAction?alt=json&key=value%2Fpart"
        );
        assert_eq!(captured.body, body.as_ref());
    }

    #[tokio::test]
    async fn declarative_native_api_route_preserves_official_wire_shape() {
        let (base_url, receiver, server) = capture_server().await;
        let channel = channel_for_surface(&base_url, ApiSurface::OpenAi, "verified");
        let body = Bytes::from_static(
            br#"{"model":"omni-moderation-latest","input":[{"type":"text","text":"safe"}],"future":{"keep":true}}"#,
        );

        let execution = execute_channel_request(
            &Client::new(),
            &channel,
            request(
                Method::POST,
                "/v1/moderations",
                "future_mode=strict",
                body.clone(),
            ),
        )
        .await
        .expect("native moderation request");
        let captured = receiver.await.expect("captured moderation request");
        server.await.expect("capture server");

        assert!(execution.opaque);
        assert!(execution.native_passthrough);
        assert_eq!(captured.method, "POST");
        assert_eq!(captured.target, "/v1/moderations?future_mode=strict");
        assert_eq!(captured.body, body.as_ref());
    }

    #[tokio::test]
    async fn opaque_binary_body_is_forwarded_byte_for_byte() {
        let (base_url, receiver, server) = capture_server().await;
        let channel = channel_for_surface(&base_url, ApiSurface::OpenAi, "verified");
        let body = Bytes::from_static(&[0, 255, 16, 128, b'{', b'}']);

        execute_channel_request(
            &Client::new(),
            &channel,
            request(Method::PUT, "/v1/files/raw", "upload=1", body.clone()),
        )
        .await
        .unwrap();
        let captured = receiver.await.unwrap();
        server.await.unwrap();

        assert_eq!(captured.method, "PUT");
        assert_eq!(captured.target, "/v1/files/raw?upload=1");
        assert_eq!(captured_header(&captured, "content-type"), None);
        assert_eq!(captured.body, body.as_ref());
    }

    #[tokio::test]
    async fn native_same_protocol_stream_detection_preserves_unknown_body_bytes() {
        let (base_url, receiver, server) = capture_server().await;
        let channel = channel_for_surface(&base_url, ApiSurface::OpenAi, "verified");
        let body = Bytes::from_static(
            br#"{
  "model" : "same-model",
  "future": {"stream": false, "nested": [1, {"keep": true}]},
  "truncation": "disabled",
  "previous_response_id": "resp-api-channel",
  "parallel_tool_calls": true,
  "stream" : true,
  "messages" : []
}"#,
        );
        let mut channel_request = request(Method::POST, "/v1/chat/completions", "", body.clone());
        channel_request.selected_upstream_model = Some("same-model".to_string());
        channel_request.cache_identity = Some("c1_gateway_must_not_mutate_native".to_string());

        let execution = execute_channel_request(&Client::new(), &channel, channel_request)
            .await
            .unwrap();
        let captured = receiver.await.unwrap();
        server.await.unwrap();

        assert!(execution.inbound_stream_requested);
        assert!(execution.native_passthrough);
        assert_eq!(captured_header(&captured, "content-type"), None);
        assert_eq!(captured.body, body.as_ref());
    }

    #[tokio::test]
    async fn official_openai_api_cross_protocol_request_gets_stable_cache_identity() {
        let (base_url, receiver, server) = capture_server().await;
        let mut channel = channel_for_surface(&base_url, ApiSurface::OpenAi, "verified");
        channel.set_source_driver(crate::source_driver::SourceDriverId::OpenAiApi);
        channel.api_format = "openai_responses".to_string();
        channel.v2.default_target.protocol = crate::protocol::kind::ProtocolKind::OpenAiResponses;
        channel.upstream_api_key = "public-api-key".to_string();
        channel.surface_bindings[0].auth_scheme = "bearer".to_string();
        channel.surface_bindings[0].protocols[0].protocol = "openai_responses".to_string();
        let body = Bytes::from_static(
            br#"{"model":"selected-target-model","messages":[{"role":"system","content":"stable system"},{"role":"user","content":"first"}]}"#,
        );
        let mut channel_request = request(Method::POST, "/v1/chat/completions", "", body);
        channel_request.cache_identity = Some("c1_platform_or_local".to_string());

        let execution = execute_channel_request(&Client::new(), &channel, channel_request)
            .await
            .expect("cross-protocol OpenAI API request");
        let captured = receiver.await.expect("captured OpenAI API request");
        server.await.expect("capture server");
        let upstream: serde_json::Value = serde_json::from_slice(&captured.body).unwrap();

        assert!(!execution.native_passthrough);
        assert_eq!(upstream["prompt_cache_key"], "c1_platform_or_local");
        assert_eq!(upstream["input"][0]["role"], "system");
        assert_eq!(upstream["input"][0]["content"][0]["text"], "stable system");
    }

    #[tokio::test]
    async fn official_anthropic_api_cross_protocol_request_gets_automatic_cache() {
        let (base_url, receiver, server) = capture_server().await;
        let mut channel = channel_for_surface(&base_url, ApiSurface::Anthropic, "verified");
        channel.set_source_driver(crate::source_driver::SourceDriverId::AnthropicApi);
        channel.upstream_api_key = "anthropic-api-key".to_string();
        channel.surface_bindings[0].auth_scheme = "x_api_key".to_string();
        let body = Bytes::from_static(
            br#"{"model":"selected-target-model","messages":[{"role":"system","content":"stable system"},{"role":"user","content":"first"},{"role":"assistant","content":"one"},{"role":"user","content":"second"},{"role":"assistant","content":"two"},{"role":"user","content":"latest"}],"tools":[{"type":"function","function":{"name":"lookup","description":"look up","parameters":{"type":"object"}}}]}"#,
        );

        let execution = execute_channel_request(
            &Client::new(),
            &channel,
            request(Method::POST, "/v1/chat/completions", "", body),
        )
        .await
        .expect("cross-protocol Anthropic API request");
        let captured = receiver.await.expect("captured Anthropic API request");
        server.await.expect("capture server");
        let upstream: serde_json::Value = serde_json::from_slice(&captured.body).unwrap();

        assert!(!execution.native_passthrough);
        assert_eq!(upstream["cache_control"]["type"], "ephemeral");
        assert!(upstream["tools"][0].get("cache_control").is_none());
        assert!(upstream["system"][0].get("cache_control").is_none());
    }

    #[tokio::test]
    async fn typed_public_openai_images_keep_native_wire_shape_and_bearer_auth() {
        let (base_url, receiver, server) = capture_server().await;
        let mut channel = channel_for_surface(&base_url, ApiSurface::OpenAi, "verified");
        channel.set_source_driver(crate::source_driver::SourceDriverId::OpenAiApi);
        channel.surface_bindings = crate::source_driver_surface_bindings(
            crate::source_driver::SourceDriverId::OpenAiApi,
            &base_url,
        );
        channel.surface_bindings[0].verification.state = "verified".to_string();
        channel.upstream_api_key = "public-image-api-key".to_string();

        let body = Bytes::from_static(
            br#"{ "model" : "gpt-image-2", "prompt" : "tiny test card", "future_image_option" : {"keep":true} }"#,
        );
        let mut image_request = request(Method::POST, "/v1/images/generations", "", body.clone());
        image_request.headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );

        let execution = execute_channel_request(&Client::new(), &channel, image_request)
            .await
            .expect("public OpenAI image API request");
        let captured = receiver.await.expect("captured public image API request");
        server.await.expect("capture server");

        assert!(!execution.opaque);
        assert!(execution.native_passthrough);
        assert_eq!(captured.method, "POST");
        assert_eq!(captured.target, "/v1/images/generations");
        assert_eq!(
            captured_header(&captured, "authorization"),
            Some("Bearer public-image-api-key")
        );
        assert_eq!(
            captured_header(&captured, "content-type"),
            Some("application/json")
        );
        assert_eq!(captured.body, body.as_ref());
    }

    #[tokio::test]
    async fn public_responses_api_preserves_native_continuation_for_stream_and_buffered_requests() {
        for stream in [false, true] {
            let (base_url, receiver, server) = capture_server().await;
            let mut channel = channel_for_surface(&base_url, ApiSurface::OpenAi, "verified");
            channel.set_source_driver(crate::source_driver::SourceDriverId::OpenAiApi);
            channel.api_format = "openai_responses".to_string();
            channel.v2.default_target.protocol =
                crate::protocol::kind::ProtocolKind::OpenAiResponses;
            channel.upstream_api_key = "public-api-key".to_string();
            channel.surface_bindings[0].auth_scheme = "bearer".to_string();
            channel.surface_bindings[0].protocols[0].protocol = "openai_responses".to_string();

            let body = serde_json::json!({
                "model": "selected-target-model",
                "previous_response_id": "resp_public_api_parent",
                "prompt_cache_key": "caller-cache-domain",
                "prompt_cache_options": {"mode": "implicit", "ttl": "30m"},
                "prompt_cache_retention": "24h",
                "input": [{
                    "type": "function_call_output",
                    "call_id": "call_public_api",
                    "output": "ok"
                }],
                "stream": stream
            })
            .to_string();
            let execution = execute_channel_request(
                &Client::new(),
                &channel,
                request(Method::POST, "/v1/responses", "", Bytes::from(body.clone())),
            )
            .await
            .expect("public Responses API request");
            let captured = receiver.await.expect("captured public API request");
            server.await.expect("capture server");

            assert!(execution.native_passthrough);
            let upstream: serde_json::Value =
                serde_json::from_slice(&captured.body).expect("upstream Responses body");
            assert_eq!(upstream["previous_response_id"], "resp_public_api_parent");
            assert_eq!(upstream["prompt_cache_key"], "caller-cache-domain");
            assert_eq!(upstream["prompt_cache_options"]["mode"], "implicit");
            assert_eq!(upstream["prompt_cache_options"]["ttl"], "30m");
            assert_eq!(upstream["prompt_cache_retention"], "24h");
            assert_eq!(upstream["input"][0]["call_id"], "call_public_api");
            assert_eq!(upstream["stream"], stream);
        }
    }

    #[tokio::test]
    async fn registered_operation_merges_fixed_and_inbound_raw_queries_lexically() {
        let (base_url, receiver, server) = capture_server().await;
        let mut channel = channel_for_surface(&base_url, ApiSurface::OpenAi, "verified");
        channel.surface_bindings[0].operation_overrides = vec![OperationEndpointOverride {
            operation: "chat_completions".to_string(),
            method: "POST".to_string(),
            url: format!("{base_url}/fixed?alt=sse&fixed=%2f"),
        }];

        execute_channel_request(
            &Client::new(),
            &channel,
            request(
                Method::POST,
                "/v1/chat/completions",
                "tag=one&tag=two%2Fthree&empty=&bare",
                Bytes::from_static(br#"{"model":"public-model","messages":[]}"#),
            ),
        )
        .await
        .unwrap();
        let captured = receiver.await.unwrap();
        server.await.unwrap();

        assert_eq!(
            captured.target,
            "/fixed?alt=sse&fixed=%2f&tag=one&tag=two%2Fthree&empty=&bare"
        );
    }

    #[tokio::test]
    async fn opaque_request_preserves_accept_encoding() {
        let (base_url, receiver, server) = capture_server().await;
        let channel = channel_for_surface(&base_url, ApiSurface::OpenAi, "verified");
        let mut request = request(Method::GET, "/v1/files/raw", "", Bytes::new());
        request.headers.insert(
            reqwest::header::ACCEPT_ENCODING,
            reqwest::header::HeaderValue::from_static("br, gzip"),
        );
        request.headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/octet-stream"),
        );
        request.headers.insert(
            reqwest::header::ACCEPT,
            reqwest::header::HeaderValue::from_static("application/vnd.future+json"),
        );
        request.headers.insert(
            "x-end-to-end",
            reqwest::header::HeaderValue::from_static("preserve exactly"),
        );

        execute_channel_request(&Client::new(), &channel, request)
            .await
            .unwrap();
        let captured = receiver.await.unwrap();
        server.await.unwrap();

        assert_eq!(
            captured_header(&captured, "accept-encoding"),
            Some("br, gzip")
        );
        assert_eq!(
            captured_header(&captured, "content-type"),
            Some("application/octet-stream")
        );
        assert_eq!(
            captured_header(&captured, "accept"),
            Some("application/vnd.future+json")
        );
        assert_eq!(
            captured_header(&captured, "x-end-to-end"),
            Some("preserve exactly")
        );
    }

    #[tokio::test]
    async fn opaque_request_strips_standard_and_connection_named_hop_headers() {
        let (base_url, receiver, server) = capture_server().await;
        let channel = channel_for_surface(&base_url, ApiSurface::OpenAi, "verified");
        let mut request = request(Method::GET, "/v1/files/raw", "", Bytes::new());
        for (name, value) in [
            ("connection", "x-remove-one, X-Remove-Two"),
            ("keep-alive", "timeout=5"),
            ("proxy-authenticate", "Basic realm=proxy"),
            ("proxy-authorization", "Basic secret"),
            ("te", "trailers"),
            ("trailer", "x-checksum"),
            ("transfer-encoding", "chunked"),
            ("upgrade", "h2c"),
            ("proxy-connection", "keep-alive"),
            ("x-remove-one", "connection scoped"),
            ("x-remove-two", "connection scoped too"),
            ("authorization", "Bearer inbound-secret"),
            ("api-key", "inbound-secret"),
            ("x-api-key", "inbound-secret"),
            ("x-goog-api-key", "inbound-secret"),
            ("cookie", "provider-session=inbound-secret"),
            ("set-cookie", "provider-session=inbound-secret"),
            (SKIP_LOCAL_SHORT_CIRCUIT_HEADER, "1"),
            (USE_LOCAL_SHORT_CIRCUIT_HEADER, "1"),
            ("x-end-to-end", "preserve exactly"),
        ] {
            request.headers.insert(
                reqwest::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                reqwest::header::HeaderValue::from_str(value).unwrap(),
            );
        }

        execute_channel_request(&Client::new(), &channel, request)
            .await
            .unwrap();
        let captured = receiver.await.unwrap();
        server.await.unwrap();

        for name in [
            "connection",
            "keep-alive",
            "proxy-authenticate",
            "proxy-authorization",
            "te",
            "trailer",
            "transfer-encoding",
            "upgrade",
            "proxy-connection",
            "x-remove-one",
            "x-remove-two",
            "authorization",
            "api-key",
            "x-api-key",
            "x-goog-api-key",
            "cookie",
            "set-cookie",
            SKIP_LOCAL_SHORT_CIRCUIT_HEADER,
            USE_LOCAL_SHORT_CIRCUIT_HEADER,
        ] {
            assert_eq!(captured_header(&captured, name), None, "header {name}");
        }
        assert_eq!(
            captured_header(&captured, "x-end-to-end"),
            Some("preserve exactly")
        );
    }

    #[tokio::test]
    async fn opaque_response_strips_hop_and_credentials_but_preserves_ordered_cookies() {
        let body = [0_u8, 255, 16, 128];
        let mut response = concat!(
            "HTTP/1.1 200 OK\r\n",
            "Content-Length: 4\r\n",
            "Connection: X-Remove-Response\r\n",
            "Keep-Alive: timeout=5\r\n",
            "Proxy-Authenticate: Basic realm=upstream\r\n",
            "Proxy-Authorization: Basic upstream-secret\r\n",
            "TE: trailers\r\n",
            "Trailer: X-Trailer\r\n",
            "Upgrade: h2c\r\n",
            "Proxy-Connection: keep-alive\r\n",
            "X-Remove-Response: connection-secret\r\n",
            "Authorization: Bearer upstream-secret\r\n",
            "Api-Key: upstream-secret\r\n",
            "X-Api-Key: upstream-secret\r\n",
            "X-Goog-Api-Key: upstream-secret\r\n",
            "Cookie: upstream-session=secret\r\n",
            "Set-Cookie: upstream-session=secret\r\n",
            "X-End-To-End: first\r\n",
            "X-End-To-End: second\r\n",
            "\r\n"
        )
        .as_bytes()
        .to_vec();
        response.extend_from_slice(&body);
        let (base_url, _receiver, server) = capture_server_with_response(response).await;
        let channel = channel_for_surface(&base_url, ApiSurface::OpenAi, "verified");

        let execution = execute_channel_request(
            &Client::new(),
            &channel,
            request(Method::GET, "/v1/files/raw", "", Bytes::new()),
        )
        .await
        .unwrap();
        for name in [
            "connection",
            "keep-alive",
            "proxy-authenticate",
            "proxy-authorization",
            "te",
            "trailer",
            "transfer-encoding",
            "upgrade",
            "proxy-connection",
            "x-remove-response",
            "authorization",
            "api-key",
            "x-api-key",
            "x-goog-api-key",
        ] {
            assert!(
                !execution.response.headers().contains_key(name),
                "response header {name} reached the tool boundary"
            );
        }
        assert_eq!(
            execution
                .response
                .headers()
                .get("content-length")
                .and_then(|value| value.to_str().ok()),
            Some("4")
        );
        assert_eq!(
            execution
                .response
                .headers()
                .get_all("cookie")
                .iter()
                .map(|value| value.to_str().unwrap())
                .collect::<Vec<_>>(),
            ["upstream-session=secret"]
        );
        assert_eq!(
            execution
                .response
                .headers()
                .get_all("set-cookie")
                .iter()
                .map(|value| value.to_str().unwrap())
                .collect::<Vec<_>>(),
            ["upstream-session=secret"]
        );
        assert_eq!(
            execution
                .response
                .headers()
                .get_all("x-end-to-end")
                .iter()
                .map(|value| value.to_str().unwrap())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );

        let tool_response = crate::proxy::response_from_reqwest(execution.response, false)
            .await
            .unwrap();
        assert!(!tool_response.headers().contains_key("content-type"));
        assert_eq!(
            tool_response
                .headers()
                .get("content-length")
                .and_then(|value| value.to_str().ok()),
            Some("4")
        );
        assert_eq!(
            tool_response
                .headers()
                .get_all("x-end-to-end")
                .iter()
                .map(|value| value.to_str().unwrap())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        let tool_body = crate::test_body_bytes(tool_response.into_body())
            .await
            .unwrap();
        assert_eq!(tool_body.as_ref(), body);
        server.await.unwrap();
    }

    #[test]
    fn header_sanitizer_removes_nominated_header_alongside_close_token() {
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::CONNECTION,
            reqwest::header::HeaderValue::from_static("close, X-Remove-Me"),
        );
        headers.insert(
            "x-remove-me",
            reqwest::header::HeaderValue::from_static("connection-scoped"),
        );
        headers.append(
            "x-end-to-end",
            reqwest::header::HeaderValue::from_static("first"),
        );
        headers.append(
            "x-end-to-end",
            reqwest::header::HeaderValue::from_static("second"),
        );

        strip_http_request_headers(&mut headers);

        assert!(!headers.contains_key("connection"));
        assert!(!headers.contains_key("x-remove-me"));
        assert_eq!(
            headers
                .get_all("x-end-to-end")
                .iter()
                .map(|value| value.to_str().unwrap())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
    }

    #[test]
    fn header_sanitizer_keeps_valid_nominations_when_connection_contains_obs_text() {
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::CONNECTION,
            reqwest::header::HeaderValue::from_bytes(b"X-Remove-Me, \x80invalid")
                .expect("opaque Connection value"),
        );
        headers.insert(
            "x-remove-me",
            reqwest::header::HeaderValue::from_static("connection-scoped"),
        );

        strip_http_request_headers(&mut headers);

        assert!(!headers.contains_key("connection"));
        assert!(!headers.contains_key("x-remove-me"));
    }

    #[test]
    fn lan_share_path_is_forwarded_only_to_another_lan_share_channel() {
        let ordinary = channel_for_surface("http://127.0.0.1:9/v1", ApiSurface::OpenAi, "verified");
        let mut lan_share = ordinary.clone();
        lan_share.set_source_driver(crate::source_driver::SourceDriverId::LanShare);

        let headers = || {
            let mut headers = HeaderMap::new();
            headers.insert(
                crate::lan_share::LAN_SHARE_PATH_HEADER,
                reqwest::header::HeaderValue::from_static("spoofed-public-path"),
            );
            headers.insert(
                crate::lan_share::LAN_SHARE_INTERNAL_PATH_HEADER,
                reqwest::header::HeaderValue::from_static("node-a,node-b"),
            );
            headers
        };

        let mut ordinary_headers = headers();
        strip_http_request_headers_for_channel(&mut ordinary_headers, &ordinary);
        assert!(!ordinary_headers.contains_key(crate::lan_share::LAN_SHARE_PATH_HEADER));
        assert!(!ordinary_headers.contains_key(crate::lan_share::LAN_SHARE_INTERNAL_PATH_HEADER));

        let mut lan_headers = headers();
        strip_http_request_headers_for_channel(&mut lan_headers, &lan_share);
        assert_eq!(
            lan_headers
                .get(crate::lan_share::LAN_SHARE_PATH_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some("node-a,node-b"),
        );
        assert!(!lan_headers.contains_key(crate::lan_share::LAN_SHARE_INTERNAL_PATH_HEADER));

        let mut response_headers = headers();
        strip_http_response_headers(&mut response_headers);
        assert!(!response_headers.contains_key(crate::lan_share::LAN_SHARE_PATH_HEADER));
        assert!(!response_headers.contains_key(crate::lan_share::LAN_SHARE_INTERNAL_PATH_HEADER));
    }

    #[tokio::test]
    async fn opaque_request_does_not_add_anthropic_version_header() {
        let (base_url, receiver, server) = capture_server().await;
        let mut channel = channel_for_surface(&base_url, ApiSurface::Anthropic, "verified");
        channel.upstream_api_key = "upstream-secret".to_string();
        channel.surface_bindings[0].auth_scheme = "x_api_key".to_string();

        execute_channel_request(
            &Client::new(),
            &channel,
            request(
                Method::POST,
                "/anthropic/v1/future-operation",
                "",
                Bytes::new(),
            ),
        )
        .await
        .unwrap();
        let captured = receiver.await.unwrap();
        server.await.unwrap();

        assert_eq!(
            captured_header(&captured, "x-api-key"),
            Some("upstream-secret")
        );
        assert_eq!(captured_header(&captured, "anthropic-version"), None);
    }

    #[tokio::test]
    async fn opaque_request_preserves_valid_percent_encoded_slash_spelling() {
        let (base_url, receiver, server) = capture_server().await;
        let channel = channel_for_surface(&base_url, ApiSurface::OpenAi, "verified");

        execute_channel_request(
            &Client::new(),
            &channel,
            request(
                Method::GET,
                "/v1/files/part%2Fname%2fname",
                "",
                Bytes::new(),
            ),
        )
        .await
        .unwrap();
        let captured = receiver.await.unwrap();
        server.await.unwrap();

        assert_eq!(captured.target, "/v1/files/part%2Fname%2fname");
    }

    #[tokio::test]
    async fn opaque_request_appends_repeated_raw_query_exactly_once() {
        let (base_url, receiver, server) = capture_server().await;
        let channel = channel_for_surface(&base_url, ApiSurface::OpenAi, "verified");
        let mut request = request(
            Method::GET,
            "/v1/files/raw",
            "tag=one&tag=two%2Fthree&tag=one",
            Bytes::new(),
        );
        request.has_query = false;

        execute_channel_request(&Client::new(), &channel, request)
            .await
            .unwrap();
        let captured = receiver.await.unwrap();
        server.await.unwrap();

        assert_eq!(
            captured.target,
            "/v1/files/raw?tag=one&tag=two%2Fthree&tag=one"
        );
    }

    #[tokio::test]
    async fn opaque_request_preserves_bare_trailing_query_delimiter() {
        let (base_url, receiver, server) = capture_server().await;
        let channel = channel_for_surface(&base_url, ApiSurface::OpenAi, "verified");
        let mut request = request(Method::GET, "/v1/files/raw", "", Bytes::new());
        request.has_query = true;

        execute_channel_request(&Client::new(), &channel, request)
            .await
            .unwrap();
        let captured = receiver.await.unwrap();
        server.await.unwrap();

        assert_eq!(captured.target, "/v1/files/raw?");
    }

    #[tokio::test]
    async fn opaque_request_rejects_authority_replacing_relative_path() {
        let (base_url, mut receiver, server) = capture_server().await;
        let channel = channel_for_surface(&base_url, ApiSurface::Anthropic, "verified");

        let error = match execute_channel_request(
            &Client::new(),
            &channel,
            request(Method::GET, "/anthropic//evil.example/x", "", Bytes::new()),
        )
        .await
        {
            Ok(_) => panic!("authority-replacing opaque path unexpectedly executed"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("provider-relative path"));
        assert!(matches!(
            receiver.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        server.abort();
    }

    #[tokio::test]
    async fn opaque_request_rejects_encoded_traversal() {
        let (base_url, mut receiver, server) = capture_server().await;
        let channel = channel_for_surface(&base_url, ApiSurface::OpenAi, "verified");

        let error = match execute_channel_request(
            &Client::new(),
            &channel,
            request(Method::GET, "/v1/files/%2e%2E%2fsecret", "", Bytes::new()),
        )
        .await
        {
            Ok(_) => panic!("encoded opaque traversal unexpectedly executed"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("path traversal"));
        assert!(matches!(
            receiver.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        server.abort();
    }

    #[tokio::test]
    async fn opaque_request_rejects_invalid_percent_escape() {
        let (base_url, mut receiver, server) = capture_server().await;
        let channel = channel_for_surface(&base_url, ApiSurface::OpenAi, "verified");

        let error = match execute_channel_request(
            &Client::new(),
            &channel,
            request(Method::GET, "/v1/files/bad%2", "", Bytes::new()),
        )
        .await
        {
            Ok(_) => panic!("invalid opaque percent escape unexpectedly executed"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("percent escape"));
        assert!(matches!(
            receiver.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        server.abort();
    }

    #[tokio::test]
    async fn opaque_request_rejects_configured_base_query() {
        let (base_url, mut receiver, server) = capture_server().await;
        let queried_base = format!("{base_url}/prefix?configured=1");
        let validation_error = parse_http_surface_base(&queried_base).expect_err("queried base");
        assert!(
            validation_error
                .to_string()
                .contains("must not contain a query")
        );
        let channel = channel_for_surface(&queried_base, ApiSurface::OpenAi, "verified");

        let error = match execute_channel_request(
            &Client::new(),
            &channel,
            request(Method::GET, "/v1/files/raw", "inbound=kept", Bytes::new()),
        )
        .await
        {
            Ok(_) => panic!("opaque request with queried base unexpectedly executed"),
            Err(error) => error,
        };

        assert_eq!(
            channel_execution_error_code(&error),
            Some("surface_operation_not_supported")
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        server.abort();
    }

    #[test]
    fn opaque_target_rejects_unsafe_provider_relative_forms() {
        let mut channel =
            channel_from_supplier("channel-1".to_string(), &default_supplier_config());
        channel.upstream_base_url = "https://api.example.com/root".to_string();
        channel.surface_bindings =
            crate::channel_surface::normalize_channel_surface_bindings(&channel);
        let target = crate::channel_surface::preferred_channel_surface_target(&channel).unwrap();
        let mut route = resolve_api_route("GET", "/v1/future-operation", false).unwrap();
        assert_eq!(route.operation, ApiOperation::Opaque);

        for unsafe_path in [
            "https://evil.example/x",
            "//evil.example/x",
            "/v1/../secret",
            "/v1/./secret",
            "/v1\\..\\secret",
            "/v1/%5c..%5Csecret",
        ] {
            route.relative_path = unsafe_path.to_string();
            assert!(
                resolve_target_url(
                    &channel,
                    &target,
                    &route,
                    &Method::GET,
                    unsafe_path,
                    "model",
                )
                .is_err(),
                "unsafe path was accepted: {unsafe_path}"
            );
        }
    }

    #[test]
    fn typed_target_retains_protocol_owned_query_but_rejects_unsafe_path_or_fragment() {
        assert!(
            validate_typed_provider_relative_target(
                "/v1beta/models/gemini:streamGenerateContent?alt=sse"
            )
            .is_ok()
        );
        assert!(
            validate_typed_provider_relative_target(
                "/v1beta/models/%2e%2e%2fsecret:streamGenerateContent?alt=sse"
            )
            .is_err()
        );
        assert!(
            validate_typed_provider_relative_target(
                "/v1beta/models/gemini:streamGenerateContent?alt=sse#fragment"
            )
            .is_err()
        );
    }

    #[test]
    fn opaque_target_rejects_recursively_encoded_unsafe_paths() {
        for unsafe_path in [
            "/%252f%252fevil.example/x",
            "/v1/%252e%252e%252fsecret",
            "/v1/%25255c..%25255csecret",
        ] {
            assert!(
                validate_opaque_provider_relative_path(unsafe_path).is_err(),
                "recursively encoded unsafe path was accepted: {unsafe_path}"
            );
        }

        let mut unstable = "/v1/%2e%2e%2fsecret".to_string();
        for _ in 0..8 {
            unstable = unstable.replace('%', "%25");
        }
        assert!(
            validate_opaque_provider_relative_path(&unstable).is_err(),
            "unstable recursive encoding was accepted: {unstable}"
        );
    }

    #[test]
    fn opaque_target_rejects_recursive_encoding_even_when_decoded_value_is_safe() {
        for recursive_path in [
            "/v1/files/name%2520with%2520spaces",
            "/v1/files/part%252Fname",
            "/v1/files/literal%2525value",
        ] {
            assert!(
                validate_opaque_provider_relative_path(recursive_path).is_err(),
                "recursive encoding was accepted: {recursive_path}"
            );
        }
    }

    #[test]
    fn opaque_target_accepts_single_encoded_non_traversal_characters() {
        for safe_path in [
            "/v1/files/name%20with%20spaces",
            "/v1/files/literal%25value",
            "/v1/files/part%2Fname%2fname",
        ] {
            validate_opaque_provider_relative_path(safe_path)
                .unwrap_or_else(|error| panic!("safe path {safe_path} was rejected: {error}"));
        }
    }

    #[test]
    fn opaque_target_validates_configured_http_surface_base() {
        let channel = channel_from_supplier("channel-1".to_string(), &default_supplier_config());
        let mut target = ChannelSurfaceTarget {
            surface: ApiSurface::OpenAi,
            base_url: String::new(),
            endpoint_profile: "openai_compatible".to_string(),
            auth_scheme: "none".to_string(),
            protocol: crate::protocol::kind::ProtocolKind::OpenAiChat,
        };
        let route = resolve_api_route("GET", "/v1/future-operation", false).unwrap();

        for invalid_base in [
            "ftp://api.example.com/root",
            "https:///root",
            "https://user@api.example.com/root",
            "https://@api.example.com/root",
            "https://api.example.com/root?configured=1",
            "https://api.example.com/root#fragment",
        ] {
            target.base_url = invalid_base.to_string();
            assert!(
                resolve_target_url(
                    &channel,
                    &target,
                    &route,
                    &Method::GET,
                    "/v1/future-operation",
                    "model",
                )
                .is_err(),
                "invalid base was accepted: {invalid_base}"
            );
        }

        target.base_url = "https://api.example.com:8443/root".to_string();
        let url = resolve_target_url(
            &channel,
            &target,
            &route,
            &Method::GET,
            "/v1/files/part%2Fname%2fname",
            "model",
        )
        .unwrap();
        assert_eq!(
            url,
            "https://api.example.com:8443/root/v1/files/part%2Fname%2fname"
        );
    }

    #[tokio::test]
    async fn opaque_request_rejects_cross_surface_target_before_http_send() {
        let (base_url, mut receiver, server) = capture_server().await;
        let channel = channel_for_surface(&base_url, ApiSurface::Anthropic, "verified");

        let error = match execute_channel_request(
            &Client::new(),
            &channel,
            request(Method::POST, "/v1/future-operation", "", Bytes::new()),
        )
        .await
        {
            Ok(_) => panic!("cross-surface opaque request unexpectedly executed"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("matching openai surface"));
        assert!(matches!(
            receiver.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        server.abort();
    }

    #[tokio::test]
    async fn opaque_request_rejects_unverified_surface_before_http_send() {
        let (base_url, mut receiver, server) = capture_server().await;
        let channel = channel_for_surface(&base_url, ApiSurface::OpenAi, "declared");

        let error = match execute_channel_request(
            &Client::new(),
            &channel,
            request(Method::POST, "/v1/future-operation", "", Bytes::new()),
        )
        .await
        {
            Ok(_) => panic!("unverified opaque request unexpectedly executed"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("matching openai surface"));
        assert!(matches!(
            receiver.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        server.abort();
    }

    #[tokio::test]
    async fn registered_operation_retains_existing_model_rewrite() {
        let (base_url, receiver, server) = capture_server().await;
        let channel = channel_for_surface(&base_url, ApiSurface::OpenAi, "verified");

        let execution = execute_channel_request(
            &Client::new(),
            &channel,
            request(
                Method::POST,
                "/v1/chat/completions",
                "",
                Bytes::from_static(br#"{"model":"public-model","messages":[]}"#),
            ),
        )
        .await
        .unwrap();
        let captured = receiver.await.unwrap();
        server.await.unwrap();

        assert!(!execution.opaque);
        assert_eq!(captured.target, "/v1/chat/completions");
        assert_eq!(
            captured.body,
            br#"{"model":"selected-target-model","messages":[]}"#
        );
    }

    #[tokio::test]
    async fn registered_operation_preserves_bare_trailing_query_delimiter() {
        let (base_url, receiver, server) = capture_server().await;
        let channel = channel_for_surface(&base_url, ApiSurface::OpenAi, "verified");
        let mut request = request(
            Method::POST,
            "/v1/chat/completions",
            "",
            Bytes::from_static(br#"{"model":"public-model","messages":[]}"#),
        );
        request.has_query = true;

        execute_channel_request(&Client::new(), &channel, request)
            .await
            .unwrap();
        let captured = receiver.await.unwrap();
        server.await.unwrap();

        assert_eq!(captured.target, "/v1/chat/completions?");
    }

    #[tokio::test]
    async fn registered_operation_override_preserves_its_bare_fixed_query_delimiter() {
        let (base_url, receiver, server) = capture_server().await;
        let mut channel = channel_for_surface(&base_url, ApiSurface::OpenAi, "verified");
        channel.surface_bindings[0].operation_overrides = vec![OperationEndpointOverride {
            operation: "chat_completions".to_string(),
            method: "POST".to_string(),
            url: format!("{base_url}/fixed?"),
        }];

        execute_channel_request(
            &Client::new(),
            &channel,
            request(
                Method::POST,
                "/v1/chat/completions",
                "",
                Bytes::from_static(br#"{"model":"public-model","messages":[]}"#),
            ),
        )
        .await
        .unwrap();
        let captured = receiver.await.unwrap();
        server.await.unwrap();

        assert_eq!(captured.target, "/fixed?");
    }

    #[tokio::test]
    async fn registered_protocol_less_operation_retains_existing_model_rewrite() {
        let (base_url, receiver, server) = capture_server().await;
        let channel = channel_for_surface(&base_url, ApiSurface::OpenAi, "verified");

        let execution = execute_channel_request(
            &Client::new(),
            &channel,
            request(
                Method::POST,
                "/v1/embeddings",
                "",
                Bytes::from_static(br#"{"model":"public-model","input":"hello"}"#),
            ),
        )
        .await
        .unwrap();
        let captured = receiver.await.unwrap();
        server.await.unwrap();

        assert!(!execution.opaque);
        assert_eq!(captured.target, "/v1/embeddings");
        assert_eq!(
            captured.body,
            br#"{"model":"selected-target-model","input":"hello"}"#
        );
    }

    #[tokio::test]
    async fn native_gemini_count_tokens_keeps_the_count_tokens_operation() {
        let (base_url, receiver, server) = capture_server().await;
        let channel = channel_for_surface(&base_url, ApiSurface::Gemini, "verified");

        let execution = execute_channel_request(
            &Client::new(),
            &channel,
            request(
                Method::POST,
                "/gemini/v1beta/models/public-model:countTokens",
                "",
                Bytes::from_static(
                    br#"{"contents":[{"role":"user","parts":[{"text":"count me"}]}]}"#,
                ),
            ),
        )
        .await
        .unwrap();
        let captured = receiver.await.unwrap();
        server.await.unwrap();

        assert!(execution.native_passthrough);
        assert!(execution.improvement_faults.is_empty());
        assert_eq!(
            captured.target,
            "/v1beta/models/selected-target-model:countTokens"
        );
    }

    #[tokio::test]
    async fn openrouter_wire_model_wrapper_covers_chat_responses_and_messages() {
        for (surface, protocol, path, tail) in [
            (
                ApiSurface::OpenAi,
                "openai_chat",
                "/v1/chat/completions",
                r#""messages":[{"role":"user","content":"ping"}]"#,
            ),
            (
                ApiSurface::OpenAi,
                "openai_responses",
                "/v1/responses",
                r#""input":[{"type":"custom_tool_call_output","call_id":"call_unchanged","output":"ok"},{"type":"reasoning","encrypted_content":"opaque"}],"store":false"#,
            ),
            (
                ApiSurface::Anthropic,
                "anthropic_messages",
                "/anthropic/v1/messages",
                r#""messages":[{"role":"user","content":"ping"}],"max_tokens":32"#,
            ),
        ] {
            let (base, receiver, server) = capture_server().await;
            let mut channel = channel_for_surface(&base, surface, "verified");
            channel.set_source_driver(crate::source_driver::SourceDriverId::Openrouter);
            channel.v2.default_target.surface = surface;
            channel.v2.default_target.protocol =
                crate::protocol::kind::ProtocolKind::parse(protocol).unwrap();
            channel.surface_bindings[0].protocols[0].protocol = protocol.into();
            channel.models = vec!["gpt-5.6-luna".into()];
            let body = format!("{{ \"model\": \"gpt-5.6-luna\", {tail} }}");
            let mut inbound = request(Method::POST, path, "", Bytes::from(body.clone()));
            inbound.selected_upstream_model = Some("gpt-5.6-luna".into());
            inbound.cache_identity = Some("c1_user_model_session".into());
            let execution = execute_channel_request(&Client::new(), &channel, inbound)
                .await
                .unwrap();
            let captured = receiver.await.unwrap();
            server.await.unwrap();
            assert_eq!(execution.upstream_model, "gpt-5.6-luna");
            assert_eq!(
                captured_header(&captured, "x-session-id"),
                Some("c1_user_model_session")
            );
            assert_eq!(
                captured_header(&captured, "x-openrouter-metadata"),
                Some("enabled")
            );
            assert_eq!(
                String::from_utf8(captured.body).unwrap(),
                body.replacen("gpt-5.6-luna", "openai/gpt-5.6-luna", 1)
            );
        }
    }

    #[tokio::test]
    async fn openrouter_preserves_native_streams_and_upstream_error_details() {
        for (surface, protocol, path, tail) in [
            (
                ApiSurface::OpenAi,
                "openai_chat",
                "/v1/chat/completions",
                r#""messages":[{"role":"user","content":"exact cache prefix"}]"#,
            ),
            (
                ApiSurface::OpenAi,
                "openai_responses",
                "/v1/responses",
                r#""input":[{"type":"custom_tool_call_output","call_id":"call_keep","output":"unchanged"}],"store":false"#,
            ),
            (
                ApiSurface::Anthropic,
                "anthropic_messages",
                "/anthropic/v1/messages",
                r#""messages":[],"max_tokens":16"#,
            ),
        ] {
            for (status, stream, content_type, response_body) in [
                (
                    200,
                    true,
                    "text/event-stream",
                    "event: provider_specific\ndata: {\"usage\":{\"cache_read_input_tokens\":2816},\"encrypted_content\":\"opaque\"}\n\ndata: [DONE]\n\n",
                ),
                (
                    200,
                    true,
                    "text/event-stream",
                    "event: error\ndata: {\"error\":{\"code\":\"provider_unavailable\",\"message\":\"original supplier detail\"}}\n\n",
                ),
                (
                    402,
                    false,
                    "application/json",
                    r#"{"error":{"code":402,"message":"Insufficient credits","metadata":{"provider_name":"fixture"}}}"#,
                ),
                (
                    429,
                    true,
                    "application/json",
                    r#"{"error":{"code":429,"message":"upstream rate limit","metadata":{"raw":"original supplier detail"}}}"#,
                ),
                (
                    503,
                    false,
                    "application/json",
                    r#"{"error":{"code":503,"message":"temporarily unavailable"}}"#,
                ),
            ] {
                let response = format!(
                    "HTTP/1.1 {status} Test\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nRetry-After: 7\r\nX-Request-Id: upstream-fixture\r\nConnection: close\r\n\r\n{response_body}",
                    response_body.len()
                );
                let (base, receiver, server) =
                    capture_server_with_response(response.into_bytes()).await;
                let mut channel = channel_for_surface(&base, surface, "verified");
                channel.set_source_driver(crate::source_driver::SourceDriverId::Openrouter);
                channel.v2.default_target = crate::model::ChannelTarget {
                    surface,
                    protocol: crate::protocol::kind::ProtocolKind::parse(protocol).unwrap(),
                };
                channel.surface_bindings[0].protocols[0].protocol = protocol.into();
                channel.models = vec!["Vendor/Branch/model:free".into()];
                let body = format!("{{ \"model\": \"model:free\", \"stream\":{stream}, {tail} }}");
                let mut inbound = request(Method::POST, path, "", Bytes::from(body.clone()));
                inbound.selected_upstream_model = None;
                inbound.stream_requested = Some(stream);
                let execution = execute_channel_request(&Client::new(), &channel, inbound)
                    .await
                    .unwrap();
                let captured = receiver.await.unwrap();
                server.await.unwrap();
                assert!(
                    execution.native_passthrough,
                    "{protocol} was unexpectedly converted"
                );
                assert_eq!(execution.response.status().as_u16(), status);
                assert_eq!(execution.response.headers()["retry-after"], "7");
                assert_eq!(
                    execution.response.headers()["x-request-id"],
                    "upstream-fixture"
                );
                assert_eq!(execution.response.text().await.unwrap(), response_body);
                assert_eq!(
                    String::from_utf8(captured.body).unwrap(),
                    body.replacen("model:free", "Vendor/Branch/model:free", 1)
                );
            }
        }
    }

    #[tokio::test]
    async fn caller_context_hint_survives_selection_of_an_unhinted_upstream_id() {
        for (surface, protocol, path, driver, wire, tail) in [
            (
                ApiSurface::Anthropic,
                "anthropic_messages",
                "/anthropic/v1/messages",
                crate::source_driver::SourceDriverId::AnthropicApi,
                "claude-sonnet-4-6",
                r#""messages":[],"max_tokens":16"#,
            ),
            (
                ApiSurface::OpenAi,
                "openai_chat",
                "/v1/chat/completions",
                crate::source_driver::SourceDriverId::Openrouter,
                "anthropic/claude-sonnet-4.6",
                r#""messages":[]"#,
            ),
            (
                ApiSurface::OpenAi,
                "openai_responses",
                "/v1/responses",
                crate::source_driver::SourceDriverId::Openrouter,
                "anthropic/claude-sonnet-4.6",
                r#""input":[],"store":false"#,
            ),
            (
                ApiSurface::Anthropic,
                "anthropic_messages",
                "/anthropic/v1/messages",
                crate::source_driver::SourceDriverId::CustomEndpoint,
                "Vendor/claude-custom[1m]",
                r#""messages":[],"max_tokens":16"#,
            ),
        ] {
            let (base, receiver, server) = capture_server().await;
            let mut channel = channel_for_surface(&base, surface, "verified");
            channel.set_source_driver(driver);
            channel.v2.default_target = crate::model::ChannelTarget {
                surface,
                protocol: crate::protocol::kind::ProtocolKind::parse(protocol).unwrap(),
            };
            channel.surface_bindings[0].protocols[0].protocol = protocol.into();
            channel.models = vec![wire.into()];
            let body = format!("{{ \"model\": \"claude-sonnet-4-6[1m]\", {tail} }}");
            let mut inbound = request(Method::POST, path, "", Bytes::from(body.clone()));
            inbound.selected_upstream_model = Some(wire.into());
            inbound
                .headers
                .insert("anthropic-beta", "compact-2026-01-12".parse().unwrap());
            execute_channel_request(&Client::new(), &channel, inbound)
                .await
                .unwrap();
            let captured = receiver.await.unwrap();
            server.await.unwrap();
            // Current native 1M Claude models need no legacy beta. Do not
            // manufacture an extra header from the tool-only context hint.
            assert_eq!(
                captured_header(&captured, "anthropic-beta"),
                Some("compact-2026-01-12")
            );
            assert_eq!(
                String::from_utf8(captured.body).unwrap(),
                body.replacen("claude-sonnet-4-6[1m]", wire, 1)
            );
        }
    }

    #[tokio::test]
    async fn priced_variants_keep_wire_identity_and_context_hints_are_not_upstream_skus() {
        for (driver, wire) in [
            (
                crate::source_driver::SourceDriverId::Openrouter,
                "nex-agi/nex-n2.5-mini:free",
            ),
            (
                crate::source_driver::SourceDriverId::Openrouter,
                "anthropic/claude-sonnet-4.6[1m]",
            ),
            (
                crate::source_driver::SourceDriverId::OpenAiApi,
                "Vendor/Model:extended",
            ),
        ] {
            for (protocol, path, tail) in [
                (
                    "openai_chat",
                    "/v1/chat/completions",
                    r#""messages":[{"role":"user","content":"exact cache prefix"}]"#,
                ),
                (
                    "openai_responses",
                    "/v1/responses",
                    r#""input":[{"type":"custom_tool_call_output","call_id":"call_keep","output":"exact cache prefix"},{"type":"reasoning","encrypted_content":"opaque state"}],"store":false"#,
                ),
            ] {
                let (base, receiver, server) = capture_server().await;
                let mut channel = channel_for_surface(&base, ApiSurface::OpenAi, "verified");
                channel.set_source_driver(driver);
                channel.v2.default_target.protocol =
                    crate::protocol::kind::ProtocolKind::parse(protocol).unwrap();
                channel.surface_bindings[0].protocols[0].protocol = protocol.into();
                channel.models = vec![wire.into()];
                let public = crate::config::short_model_name(wire).to_ascii_lowercase();
                let body = format!("{{ \"model\": \"{public}\", {tail} }}");
                let mut inbound = request(Method::POST, path, "", Bytes::from(body.clone()));
                // Resolve the tool's public short ID through the real channel
                // mapping, rather than supplying the answer in the test.
                inbound.selected_upstream_model = None;
                let result = execute_channel_request(&Client::new(), &channel, inbound)
                    .await
                    .unwrap();
                let captured = receiver.await.unwrap();
                server.await.unwrap();
                assert_eq!(result.upstream_model, wire);
                let expected = if driver == crate::source_driver::SourceDriverId::Openrouter {
                    crate::config::without_context_hint(wire)
                } else {
                    wire
                };
                assert_eq!(
                    String::from_utf8(captured.body).unwrap(),
                    body.replacen(&public, expected, 1)
                );
            }
        }
    }

    #[tokio::test]
    async fn qwen_catalog_billing_names_do_not_rewrite_upstream_requests_to_short_ids() {
        for (driver, requested) in [
            (
                crate::source_driver::SourceDriverId::GroqApi,
                "qwen/qwen3.8-27b",
            ),
            (
                crate::source_driver::SourceDriverId::Openrouter,
                "qwen/qwen3.8-27b",
            ),
            (
                crate::source_driver::SourceDriverId::Openrouter,
                "qwen3.8-27b",
            ),
        ] {
            let (base, receiver, server) = capture_server().await;
            let mut channel = channel_for_surface(&base, ApiSurface::OpenAi, "verified");
            channel.set_source_driver(driver);
            channel.v2.default_target.protocol = crate::protocol::kind::ProtocolKind::OpenAiChat;
            channel.models = vec!["qwen/qwen3.8-27b".into()];
            let body = format!(
                r#"{{"model":"{requested}","messages":[{{"role":"user","content":"qwen3.8-27b"}}]}}"#
            );
            let mut inbound = request(Method::POST, "/v1/chat/completions", "", Bytes::from(body));
            inbound.selected_upstream_model = Some(requested.into());
            execute_channel_request(&Client::new(), &channel, inbound)
                .await
                .unwrap();
            let captured = receiver.await.unwrap();
            server.await.unwrap();
            let wire: serde_json::Value = serde_json::from_slice(&captured.body).unwrap();
            assert_eq!(wire["model"], "qwen/qwen3.8-27b");
            assert_eq!(wire["messages"][0]["content"], "qwen3.8-27b");
        }
    }

    #[tokio::test]
    async fn absent_native_responses_extensions_do_not_send_generation_requests() {
        let mut channel = channel_for_surface("http://127.0.0.1:1", ApiSurface::OpenAi, "verified");
        channel.v2.default_target.protocol = crate::protocol::kind::ProtocolKind::OpenAiResponses;
        channel.surface_bindings[0].protocols[0].protocol = "openai_responses".into();
        for operation in ["openai.responses_compact", "openai.responses_input_tokens"] {
            channel
                .detection_checks
                .push(crate::model::ChannelDetectionCheck {
                    name: "responses_operation".into(),
                    capability: operation.into(),
                    status: "unsupported".into(),
                    ..Default::default()
                });
        }
        let body = Bytes::from_static(br#"{"model":"selected-target-model","input":"count me"}"#);
        let result = execute_channel_request(
            &Client::new(),
            &channel,
            request(Method::POST, "/v1/responses/input_tokens", "", body.clone()),
        )
        .await
        .unwrap();
        assert_eq!(
            result.response.headers()[TOKEN_COUNT_ESTIMATED_HEADER],
            "true"
        );
        assert!(
            result.response.json::<serde_json::Value>().await.unwrap()["input_tokens"]
                .as_i64()
                .unwrap()
                > 0
        );
        let error = execute_channel_request(
            &Client::new(),
            &channel,
            request(Method::POST, "/v1/responses/compact", "", body),
        )
        .await
        .unwrap_err();
        assert_eq!(
            channel_execution_error_code(&error),
            Some("surface_operation_not_supported")
        );
    }

    #[tokio::test]
    async fn cross_protocol_count_tokens_remains_available_but_reports_approximation() {
        let cases = [
            (
                ApiSurface::OpenAi,
                "/anthropic/v1/messages/count_tokens",
                br#"{"model":"claude-public","messages":[{"role":"user","content":"count me"}]}"#
                    .as_slice(),
                "/input_tokens",
            ),
            (
                ApiSurface::OpenAi,
                "/gemini/v1beta/models/gemini-public:countTokens",
                br#"{"contents":[{"role":"user","parts":[{"text":"count me"}]}]}"#.as_slice(),
                "/totalTokens",
            ),
            (
                ApiSurface::Anthropic,
                "/v1/responses/input_tokens",
                br#"{"model":"gpt-public","input":"count me"}"#.as_slice(),
                "/input_tokens",
            ),
        ];
        for (target_surface, path, body, token_pointer) in cases {
            let (base_url, receiver, server) = capture_server().await;
            let channel = channel_for_surface(&base_url, target_surface, "verified");
            let execution = execute_channel_request(
                &Client::new(),
                &channel,
                request(Method::POST, path, "", Bytes::copy_from_slice(body)),
            )
            .await
            .expect("availability-first compatibility attempt");
            assert_eq!(
                execution
                    .response
                    .headers()
                    .get(TOKEN_COUNT_ESTIMATED_HEADER)
                    .and_then(|value| value.to_str().ok()),
                Some("true")
            );
            let response_body = execution.response.bytes().await.unwrap();
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(50), receiver)
                    .await
                    .is_err(),
                "{path} must not send a paid upstream request"
            );
            server.abort();

            let response: serde_json::Value = serde_json::from_slice(&response_body).unwrap();
            assert!(
                response
                    .pointer(token_pointer)
                    .and_then(serde_json::Value::as_u64)
                    .is_some_and(|value| value > 0)
            );
            let faults = serde_json::to_value(execution.improvement_faults).unwrap();
            assert!(faults.as_array().is_some_and(|faults| {
                faults
                    .iter()
                    .any(|fault| fault["code"] == "token_count_locally_estimated")
            }));
        }
    }

    #[test]
    fn target_url_uses_v2_surface_binding_base() {
        let mut channel =
            channel_from_supplier("channel-1".to_string(), &default_supplier_config());
        channel.surface_bindings =
            crate::channel_surface::normalize_channel_surface_bindings(&channel);
        channel
            .surface_bindings
            .iter_mut()
            .find(|binding| binding.surface == ApiSurface::OpenAi)
            .expect("OpenAI surface binding")
            .base_url = "https://api.example.com/v1".to_string();
        let target = crate::channel_surface::preferred_channel_surface_target(&channel).unwrap();
        let route = resolve_api_route("POST", "/v1/chat/completions", false).unwrap();
        let url = resolve_target_url(
            &channel,
            &target,
            &route,
            &Method::POST,
            "/v1/chat/completions",
            "gpt-test",
        )
        .unwrap();
        assert_eq!(url, "https://api.example.com/v1/chat/completions");
    }

    #[test]
    fn operation_override_expands_encoded_model_placeholder() {
        let mut channel =
            channel_from_supplier("channel-1".to_string(), &default_supplier_config());
        channel.surface_bindings =
            crate::channel_surface::normalize_channel_surface_bindings(&channel);
        channel.surface_bindings[0].base_url = "https://api.example.com/v1".to_string();
        channel.surface_bindings[0].operation_overrides = vec![OperationEndpointOverride {
            operation: "chat_completions".to_string(),
            method: "POST".to_string(),
            url: "https://api.example.com/models/{model}/chat".to_string(),
        }];
        let target = crate::channel_surface::preferred_channel_surface_target(&channel).unwrap();
        let route = resolve_api_route("POST", "/v1/chat/completions", false).unwrap();
        let url = resolve_target_url(
            &channel,
            &target,
            &route,
            &Method::POST,
            "/v1/chat/completions",
            "Model With Space",
        )
        .unwrap();

        assert_eq!(
            url,
            "https://api.example.com/models/Model%20With%20Space/chat"
        );
    }

    #[test]
    fn operation_override_cannot_replace_authority_at_execution_time() {
        let mut channel =
            channel_from_supplier("channel-1".to_string(), &default_supplier_config());
        channel.surface_bindings =
            crate::channel_surface::normalize_channel_surface_bindings(&channel);
        channel.surface_bindings[0].base_url = "https://api.example.com/v1".to_string();
        let target = crate::channel_surface::preferred_channel_surface_target(&channel).unwrap();
        channel.surface_bindings[0].operation_overrides = vec![OperationEndpointOverride {
            operation: "chat_completions".to_string(),
            method: "POST".to_string(),
            url: "https://evil.example/v1/chat/completions?fixed=1".to_string(),
        }];
        let route = resolve_api_route("POST", "/v1/chat/completions", false).unwrap();

        let error = resolve_target_url(
            &channel,
            &target,
            &route,
            &Method::POST,
            "/v1/chat/completions",
            "model",
        )
        .expect_err("authority-replacing override must be rejected during execution");

        assert!(error.to_string().contains("configured authority"));
    }

    #[tokio::test]
    async fn stable_execution_error_codes_cover_surface_conversion_and_authentication() {
        let unsupported_surface =
            channel_for_surface("http://127.0.0.1:9", ApiSurface::Anthropic, "verified");
        let error = execute_channel_request(
            &Client::new(),
            &unsupported_surface,
            request(Method::GET, "/v1/files/file-1", "", Bytes::new()),
        )
        .await
        .expect_err("opaque operation must not cross API surfaces");
        assert_eq!(
            channel_execution_error_code(&error),
            Some("surface_operation_not_supported")
        );

        let conversion = channel_for_surface("http://127.0.0.1:9", ApiSurface::OpenAi, "verified");
        let error = execute_channel_request(
            &Client::new(),
            &conversion,
            request(
                Method::POST,
                "/v1/responses",
                "",
                Bytes::from_static(b"not-json"),
            ),
        )
        .await
        .expect_err("invalid cross-protocol body must fail conversion");
        assert_eq!(
            channel_execution_error_code(&error),
            Some("conversion_failed")
        );

        let mut authentication =
            channel_for_surface("http://127.0.0.1:9", ApiSurface::OpenAi, "verified");
        authentication.surface_bindings[0].auth_scheme = "bearer".to_string();
        authentication.upstream_api_key = "invalid\ncredential".to_string();
        let error = execute_channel_request(
            &Client::new(),
            &authentication,
            request(
                Method::POST,
                "/v1/chat/completions",
                "",
                Bytes::from_static(br#"{"model":"test","messages":[]}"#),
            ),
        )
        .await
        .expect_err("invalid credential header must fail authentication setup");
        assert_eq!(
            channel_execution_error_code(&error),
            Some("authentication_failed")
        );
    }

    #[tokio::test]
    async fn stable_execution_error_codes_classify_upstream_http_rejections() {
        for (status, expected) in [(401, "authentication_failed"), (429, "upstream_rejected")] {
            let response = format!(
                "HTTP/1.1 {status} Rejected\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
            )
            .into_bytes();
            let (base_url, receiver, server) = capture_server_with_response(response).await;
            let channel = channel_for_surface(&base_url, ApiSurface::OpenAi, "verified");
            let execution = execute_channel_request(
                &Client::new(),
                &channel,
                request(
                    Method::POST,
                    "/v1/chat/completions",
                    "",
                    Bytes::from_static(br#"{"model":"test","messages":[]}"#),
                ),
            )
            .await
            .expect("HTTP rejection remains a relayable response");
            receiver.await.expect("captured rejection request");
            server.await.expect("rejection server");

            assert_eq!(
                execution.error_code().map(|code| code.as_str()),
                Some(expected)
            );
        }
    }

    #[tokio::test]
    async fn stable_execution_error_codes_classify_real_upstream_timeout() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("timeout listener");
        let address = listener.local_addr().expect("timeout address");
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.expect("timeout accept");
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        });
        let channel =
            channel_for_surface(&format!("http://{address}"), ApiSurface::OpenAi, "verified");
        let client = Client::builder()
            .timeout(std::time::Duration::from_millis(20))
            .build()
            .expect("timeout client");
        let error = execute_channel_request(
            &client,
            &channel,
            request(
                Method::POST,
                "/v1/chat/completions",
                "",
                Bytes::from_static(br#"{"model":"test","messages":[]}"#),
            ),
        )
        .await
        .expect_err("stalled upstream must time out");
        server.abort();

        assert_eq!(
            channel_execution_error_code(&error),
            Some("upstream_timeout")
        );
    }
}
