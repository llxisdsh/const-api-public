use crate::protocol::kind::ProtocolKind;
use serde::{Deserialize, Serialize};

const API_SURFACE_CONTRACT_JSON: &str =
    include_str!("../../../shared/api-surface-contract.json");

fn api_surface_contract_document() -> &'static serde_json::Value {
    static CONTRACT: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
    CONTRACT.get_or_init(|| {
        serde_json::from_str(API_SURFACE_CONTRACT_JSON).unwrap_or_else(|error| {
            eprintln!("[const-api][surface] embedded API contract is invalid: {error}");
            serde_json::Value::Null
        })
    })
}

pub(crate) fn official_model_api_family_ids(surface: ApiSurface) -> Vec<String> {
    api_surface_contract_document()["official_families"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|family| {
            family["surface"].as_str() == Some(surface.as_str())
                && family["scope"].as_str() == Some("model_api")
        })
        .filter_map(|family| family["id"].as_str().map(str::to_string))
        .collect()
}

pub(crate) fn official_model_api_family_id_for_path(
    surface: ApiSurface,
    path: &str,
) -> Option<String> {
    let path = path.split_once('?').map_or(path, |(path, _)| path);
    api_surface_contract_document()["official_families"]
        .as_array()?
        .iter()
        .filter(|family| {
            family["surface"].as_str() == Some(surface.as_str())
                && family["scope"].as_str() == Some("model_api")
        })
        .flat_map(|family| {
            family["official_paths"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(move |template| {
                    let template = template.as_str()?;
                    let static_prefix = template.split_once('{').map_or(template, |(head, _)| head);
                    let matched = if static_prefix.len() != template.len() {
                        path.starts_with(static_prefix)
                    } else {
                        path == template
                            || path
                                .strip_prefix(template)
                                .is_some_and(|tail| tail.starts_with('/'))
                    };
                    matched.then(|| {
                        (
                            static_prefix.len(),
                            family["id"].as_str().unwrap_or_default().to_string(),
                        )
                    })
                })
        })
        .max_by_key(|(prefix_len, _)| *prefix_len)
        .map(|(_, family)| family)
        .filter(|family| !family.is_empty())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ApiSurface {
    #[serde(alias = "openai")]
    OpenAi,
    Anthropic,
    Gemini,
}

impl Default for ApiSurface {
    fn default() -> Self {
        Self::OpenAi
    }
}

impl ApiSurface {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
        }
    }

    pub(crate) const fn mount(self) -> &'static str {
        match self {
            Self::OpenAi => "/v1",
            Self::Anthropic => "/anthropic",
            Self::Gemini => "/gemini",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ApiOperation {
    ListModels,
    GetModel,
    ChatCompletions,
    Responses,
    ResponsesGet,
    ResponsesDelete,
    ResponsesCancel,
    ResponsesInputItems,
    ResponsesInputTokens,
    ResponsesCompact,
    #[serde(rename = "responses_websocket", alias = "responses_web_socket")]
    ResponsesWebSocket,
    #[serde(rename = "realtime_websocket", alias = "realtime_web_socket")]
    RealtimeWebSocket,
    RealtimeCallsCreate,
    RealtimeLiveCallCreate,
    RealtimeLiveConnect,
    RealtimeLiveWebSocket,
    #[serde(
        rename = "realtime_translation_websocket",
        alias = "realtime_translation_web_socket"
    )]
    RealtimeTranslationWebSocket,
    ConversationsCreate,
    ConversationsGet,
    ConversationsUpdate,
    ConversationsDelete,
    ConversationItemsCreate,
    ConversationItemsList,
    ConversationItemGet,
    ConversationItemDelete,
    FilesCreate,
    FilesList,
    FilesGet,
    FilesDelete,
    FilesContent,
    UploadsCreate,
    UploadPartsCreate,
    UploadsComplete,
    UploadsCancel,
    ImagesGenerations,
    ImagesEdits,
    ImagesVariations,
    AudioSpeech,
    AudioTranscriptions,
    AudioTranslations,
    VideosCreate,
    VideosList,
    VideosGet,
    VideosDelete,
    VideosContent,
    VideosRemix,
    VideosEdit,
    VideosExtend,
    BatchesCreate,
    BatchesList,
    BatchesGet,
    BatchesCancel,
    BatchesDelete,
    BatchesResults,
    Embeddings,
    ContentProvenanceChecks,
    Messages,
    CountTokens,
    GenerateContent,
    StreamGenerateContent,
    EmbedContent,
    InteractionsCreate,
    InteractionsGet,
    InteractionsDelete,
    InteractionsCancel,
    CachedContentsCreate,
    CachedContentsList,
    CachedContentsGet,
    CachedContentsUpdate,
    CachedContentsDelete,
    #[serde(rename = "live_websocket", alias = "live_web_socket")]
    LiveWebSocket,
    Opaque,
}

impl ApiOperation {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ListModels => "list_models",
            Self::GetModel => "get_model",
            Self::ChatCompletions => "chat_completions",
            Self::Responses => "responses",
            Self::ResponsesGet => "responses_get",
            Self::ResponsesDelete => "responses_delete",
            Self::ResponsesCancel => "responses_cancel",
            Self::ResponsesInputItems => "responses_input_items",
            Self::ResponsesInputTokens => "responses_input_tokens",
            Self::ResponsesCompact => "responses_compact",
            Self::ResponsesWebSocket => "responses_websocket",
            Self::RealtimeWebSocket => "realtime_websocket",
            Self::RealtimeCallsCreate => "realtime_calls_create",
            Self::RealtimeLiveCallCreate => "realtime_live_call_create",
            Self::RealtimeLiveConnect => "realtime_live_connect",
            Self::RealtimeLiveWebSocket => "realtime_live_websocket",
            Self::RealtimeTranslationWebSocket => "realtime_translation_websocket",
            Self::ConversationsCreate => "conversations_create",
            Self::ConversationsGet => "conversations_get",
            Self::ConversationsUpdate => "conversations_update",
            Self::ConversationsDelete => "conversations_delete",
            Self::ConversationItemsCreate => "conversation_items_create",
            Self::ConversationItemsList => "conversation_items_list",
            Self::ConversationItemGet => "conversation_item_get",
            Self::ConversationItemDelete => "conversation_item_delete",
            Self::FilesCreate => "files_create",
            Self::FilesList => "files_list",
            Self::FilesGet => "files_get",
            Self::FilesDelete => "files_delete",
            Self::FilesContent => "files_content",
            Self::UploadsCreate => "uploads_create",
            Self::UploadPartsCreate => "upload_parts_create",
            Self::UploadsComplete => "uploads_complete",
            Self::UploadsCancel => "uploads_cancel",
            Self::ImagesGenerations => "images_generations",
            Self::ImagesEdits => "images_edits",
            Self::ImagesVariations => "images_variations",
            Self::AudioSpeech => "audio_speech",
            Self::AudioTranscriptions => "audio_transcriptions",
            Self::AudioTranslations => "audio_translations",
            Self::VideosCreate => "videos_create",
            Self::VideosList => "videos_list",
            Self::VideosGet => "videos_get",
            Self::VideosDelete => "videos_delete",
            Self::VideosContent => "videos_content",
            Self::VideosRemix => "videos_remix",
            Self::VideosEdit => "videos_edit",
            Self::VideosExtend => "videos_extend",
            Self::BatchesCreate => "batches_create",
            Self::BatchesList => "batches_list",
            Self::BatchesGet => "batches_get",
            Self::BatchesCancel => "batches_cancel",
            Self::BatchesDelete => "batches_delete",
            Self::BatchesResults => "batches_results",
            Self::Embeddings => "embeddings",
            Self::ContentProvenanceChecks => "content_provenance_checks",
            Self::Messages => "messages",
            Self::CountTokens => "count_tokens",
            Self::GenerateContent => "generate_content",
            Self::StreamGenerateContent => "stream_generate_content",
            Self::EmbedContent => "embed_content",
            Self::InteractionsCreate => "interactions_create",
            Self::InteractionsGet => "interactions_get",
            Self::InteractionsDelete => "interactions_delete",
            Self::InteractionsCancel => "interactions_cancel",
            Self::CachedContentsCreate => "cached_contents_create",
            Self::CachedContentsList => "cached_contents_list",
            Self::CachedContentsGet => "cached_contents_get",
            Self::CachedContentsUpdate => "cached_contents_update",
            Self::CachedContentsDelete => "cached_contents_delete",
            Self::LiveWebSocket => "live_websocket",
            Self::Opaque => "opaque",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TransportKind {
    Http,
    Websocket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OperationOwnership {
    AggregatedControl,
    RoutedKnown,
    RoutedOpaque,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Statefulness {
    Stateless,
    ResourceCreate,
    ResourceRead,
    ResourceMutate,
    ResourceDelete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BillingClass {
    None,
    Metered,
    Conditional,
}

#[derive(Debug, Clone, Copy)]
enum RoutePath {
    Exact(&'static str),
    ModelDetail(&'static str),
    GeminiAction(&'static str),
    OpenAiResponseResource(&'static str),
    OpenAiConversationResource(&'static str),
    OpenAiConversationItem,
    SingleSegmentResource(&'static str, &'static str, &'static [&'static str]),
    GeminiInteractionResource(&'static str),
}

impl RoutePath {
    fn matches(self, path: &str) -> bool {
        match self {
            Self::Exact(expected) => path == expected,
            Self::ModelDetail(prefix) => path.strip_prefix(prefix).is_some_and(|value| {
                !value.is_empty() && !value.contains('/') && !value.contains(':')
            }),
            Self::GeminiAction(suffix) => path
                .strip_prefix("/gemini/v1beta/models/")
                .and_then(|value| value.strip_suffix(suffix))
                .is_some_and(|model| !model.is_empty() && !model.contains('/')),
            Self::OpenAiResponseResource(suffix) => path
                .strip_prefix("/v1/responses/")
                .and_then(|value| {
                    if suffix.is_empty() {
                        Some(value)
                    } else {
                        value.strip_suffix(suffix)
                    }
                })
                .is_some_and(|id| {
                    !id.is_empty() && id != "compact" && id != "input_tokens" && !id.contains('/')
                }),
            Self::OpenAiConversationResource(suffix) => path
                .strip_prefix("/v1/conversations/")
                .and_then(|value| {
                    if suffix.is_empty() {
                        Some(value)
                    } else {
                        value.strip_suffix(suffix)
                    }
                })
                .is_some_and(|id| !id.is_empty() && !id.contains('/')),
            Self::OpenAiConversationItem => path
                .strip_prefix("/v1/conversations/")
                .and_then(|value| value.split_once("/items/"))
                .is_some_and(|(conversation_id, item_id)| {
                    !conversation_id.is_empty()
                        && !item_id.is_empty()
                        && !conversation_id.contains('/')
                        && !item_id.contains('/')
                }),
            Self::SingleSegmentResource(prefix, suffix, reserved) => path
                .strip_prefix(prefix)
                .and_then(|value| {
                    if suffix.is_empty() {
                        Some(value)
                    } else {
                        value.strip_suffix(suffix)
                    }
                })
                .is_some_and(|id| !id.is_empty() && !id.contains('/') && !reserved.contains(&id)),
            Self::GeminiInteractionResource(suffix) => path
                .strip_prefix("/gemini/v1beta/interactions/")
                .and_then(|value| {
                    if suffix.is_empty() {
                        Some(value)
                    } else {
                        value.strip_suffix(suffix)
                    }
                })
                .is_some_and(|id| !id.is_empty() && !id.contains('/')),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RouteSpec {
    pub(crate) method: &'static str,
    pub(crate) path_template: &'static str,
    pub(crate) surface: ApiSurface,
    pub(crate) protocol: Option<ProtocolKind>,
    pub(crate) operation: ApiOperation,
    pub(crate) ownership: OperationOwnership,
    pub(crate) transport: TransportKind,
    pub(crate) statefulness: Statefulness,
    pub(crate) billing_class: BillingClass,
    operation_name: Option<&'static str>,
    native_api_only: bool,
    path: RoutePath,
}

impl RouteSpec {
    fn matches(self, method: &str, path: &str) -> bool {
        self.method == method && self.path.matches(path)
    }

    fn operation_id(self) -> String {
        format!(
            "{}.{}",
            self.surface.as_str(),
            self.operation_name.unwrap_or(self.operation.as_str())
        )
    }

    fn request_body_mode(self) -> &'static str {
        if self.transport == TransportKind::Websocket {
            return "duplex_frames";
        }
        if api_operation_uses_streaming_upload(self.operation) {
            return "streaming_upload";
        }
        if self.method == "GET" || self.method == "DELETE" {
            return "none";
        }
        match self.ownership {
            OperationOwnership::AggregatedControl => "none",
            OperationOwnership::RoutedKnown | OperationOwnership::RoutedOpaque => "small_buffered",
        }
    }

    const fn response_modes(self) -> &'static [&'static str] {
        if matches!(self.transport, TransportKind::Websocket) {
            return &["websocket"];
        }
        match self.operation {
            ApiOperation::ChatCompletions
            | ApiOperation::Responses
            | ApiOperation::Messages
            | ApiOperation::InteractionsCreate
            | ApiOperation::InteractionsGet
            | ApiOperation::ImagesGenerations
            | ApiOperation::ImagesEdits
            | ApiOperation::AudioTranscriptions => &["buffered", "sse"],
            ApiOperation::StreamGenerateContent => &["sse"],
            ApiOperation::FilesContent
            | ApiOperation::VideosContent
            | ApiOperation::BatchesResults => &["binary_stream"],
            ApiOperation::AudioSpeech => &["binary_stream", "sse"],
            _ => &["buffered"],
        }
    }

    const fn execution_policy(self) -> &'static str {
        if matches!(
            self.operation,
            ApiOperation::RealtimeWebSocket
                | ApiOperation::RealtimeLiveConnect
                | ApiOperation::LiveWebSocket
                | ApiOperation::RealtimeCallsCreate
                | ApiOperation::RealtimeLiveCallCreate
        ) {
            return "capability_routed";
        }
        if self.native_api_only || api_operation_requires_native_api_credential(self.operation) {
            return "native_backend_only";
        }
        if api_operation_requires_local_backend(self.operation) {
            return "local_backend_only";
        }
        match self.ownership {
            OperationOwnership::AggregatedControl => "aggregated_control",
            OperationOwnership::RoutedKnown | OperationOwnership::RoutedOpaque => {
                "capability_routed"
            }
        }
    }

    const fn required_context_declarations(self) -> &'static [&'static str] {
        if matches!(
            self.operation,
            ApiOperation::RealtimeWebSocket | ApiOperation::LiveWebSocket
        ) {
            return &["api_credential", "platform_supply"];
        }
        if matches!(
            self.operation,
            ApiOperation::RealtimeLiveConnect
                | ApiOperation::RealtimeCallsCreate
                | ApiOperation::RealtimeLiveCallCreate
        ) {
            return &["api_credential", "subscription_driver", "platform_supply"];
        }
        if self.native_api_only || api_operation_requires_native_api_credential(self.operation) {
            return &["api_credential"];
        }
        if api_operation_requires_local_backend(self.operation) {
            return &["api_credential", "subscription_driver"];
        }
        match self.ownership {
            OperationOwnership::AggregatedControl => &[],
            OperationOwnership::RoutedKnown | OperationOwnership::RoutedOpaque => {
                &["api_credential", "subscription_driver", "platform_supply"]
            }
        }
    }

    const fn required_conversion_evidence(self) -> &'static [&'static str] {
        match self.ownership {
            OperationOwnership::AggregatedControl => &["response", "error"],
            OperationOwnership::RoutedKnown | OperationOwnership::RoutedOpaque => {
                if matches!(
                    self.operation,
                    ApiOperation::ChatCompletions
                        | ApiOperation::Responses
                        | ApiOperation::ResponsesWebSocket
                        | ApiOperation::RealtimeWebSocket
                        | ApiOperation::RealtimeLiveConnect
                        | ApiOperation::RealtimeLiveWebSocket
                        | ApiOperation::RealtimeTranslationWebSocket
                        | ApiOperation::Messages
                        | ApiOperation::InteractionsCreate
                        | ApiOperation::InteractionsGet
                        | ApiOperation::StreamGenerateContent
                        | ApiOperation::FilesContent
                        | ApiOperation::ImagesGenerations
                        | ApiOperation::ImagesEdits
                        | ApiOperation::AudioSpeech
                        | ApiOperation::AudioTranscriptions
                        | ApiOperation::VideosContent
                        | ApiOperation::BatchesResults
                        | ApiOperation::LiveWebSocket
                ) {
                    &["request", "response", "stream", "error", "cancel"]
                } else {
                    &["request", "response", "error", "cancel"]
                }
            }
        }
    }
}

pub(crate) const fn api_operation_requires_native_api_credential(operation: ApiOperation) -> bool {
    matches!(
        operation,
        ApiOperation::RealtimeWebSocket
            | ApiOperation::RealtimeTranslationWebSocket
            | ApiOperation::ResponsesInputTokens
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
            | ApiOperation::CachedContentsCreate
            | ApiOperation::CachedContentsList
            | ApiOperation::CachedContentsGet
            | ApiOperation::CachedContentsUpdate
            | ApiOperation::CachedContentsDelete
            | ApiOperation::LiveWebSocket
    )
}

pub(crate) const fn api_operation_requires_local_backend(operation: ApiOperation) -> bool {
    matches!(
        operation,
        ApiOperation::ImagesEdits
            | ApiOperation::RealtimeCallsCreate
            | ApiOperation::RealtimeLiveCallCreate
            | ApiOperation::RealtimeLiveConnect
            | ApiOperation::RealtimeLiveWebSocket
    )
}

pub(crate) const fn api_operation_uses_streaming_upload(operation: ApiOperation) -> bool {
    matches!(
        operation,
        ApiOperation::FilesCreate
            | ApiOperation::UploadPartsCreate
            | ApiOperation::ImagesVariations
            | ApiOperation::AudioTranscriptions
            | ApiOperation::AudioTranslations
            | ApiOperation::VideosCreate
            | ApiOperation::VideosEdit
            | ApiOperation::VideosExtend
            | ApiOperation::ContentProvenanceChecks
    )
}

pub(crate) fn native_opaque_streaming_upload_family(path: &str) -> Option<&'static str> {
    let path = path.split_once('?').map_or(path, |(path, _)| path);
    match path {
        "/v1/audio/voice_consents" | "/v1/audio/voices" => Some("openai_audio"),
        "/v1/skills" => Some("openai_resource"),
        "/anthropic/v1/skills" => Some("anthropic_resource"),
        _ if path.starts_with("/v1/skills/") && path.ends_with("/versions") => {
            Some("openai_resource")
        }
        _ if path.starts_with("/v1/containers/") && path.ends_with("/files") => {
            Some("openai_resource")
        }
        _ if path.starts_with("/anthropic/v1/skills/") && path.ends_with("/versions") => {
            Some("anthropic_resource")
        }
        _ if path.starts_with("/gemini/upload/v1beta/fileSearchStores/") => {
            Some("gemini_file_search")
        }
        _ => None,
    }
}

pub(crate) const fn api_operation_streams_binary_response(operation: ApiOperation) -> bool {
    matches!(
        operation,
        ApiOperation::FilesContent
            | ApiOperation::AudioSpeech
            | ApiOperation::VideosContent
            | ApiOperation::BatchesResults
    )
}

pub(crate) fn registered_supplier_http_operations()
-> Vec<(ApiSurface, ApiOperation, Option<ProtocolKind>)> {
    let mut seen = std::collections::HashSet::new();
    ROUTE_SPECS
        .iter()
        .filter(|spec| {
            spec.transport == TransportKind::Http
                && spec.ownership == OperationOwnership::RoutedKnown
                && !spec.native_api_only
                && !api_operation_requires_local_backend(spec.operation)
        })
        .filter_map(|spec| {
            let key = (spec.surface, spec.operation);
            seen.insert(key)
                .then_some((spec.surface, spec.operation, spec.protocol))
        })
        .collect()
}

const ROUTE_SPECS: &[RouteSpec] = &[
    route(
        "GET",
        "/v1/models",
        ApiSurface::OpenAi,
        None,
        ApiOperation::ListModels,
        OperationOwnership::AggregatedControl,
        TransportKind::Http,
        Statefulness::Stateless,
        BillingClass::None,
        RoutePath::Exact("/v1/models"),
    ),
    route(
        "GET",
        "/v1/models/{model}",
        ApiSurface::OpenAi,
        None,
        ApiOperation::GetModel,
        OperationOwnership::AggregatedControl,
        TransportKind::Http,
        Statefulness::Stateless,
        BillingClass::None,
        RoutePath::ModelDetail("/v1/models/"),
    ),
    native_api_route(
        "DELETE",
        "/v1/models/{model}",
        "delete_model",
        ApiSurface::OpenAi,
        Statefulness::ResourceDelete,
        BillingClass::None,
        RoutePath::ModelDetail("/v1/models/"),
    ),
    route(
        "POST",
        "/v1/chat/completions",
        ApiSurface::OpenAi,
        Some(ProtocolKind::OpenAiChat),
        ApiOperation::ChatCompletions,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::Stateless,
        BillingClass::Metered,
        RoutePath::Exact("/v1/chat/completions"),
    ),
    native_api_route(
        "GET",
        "/v1/chat/completions",
        "chat_completions_list",
        ApiSurface::OpenAi,
        Statefulness::ResourceRead,
        BillingClass::None,
        RoutePath::Exact("/v1/chat/completions"),
    ),
    native_api_route(
        "GET",
        "/v1/chat/completions/{completion_id}",
        "chat_completions_get",
        ApiSurface::OpenAi,
        Statefulness::ResourceRead,
        BillingClass::None,
        RoutePath::SingleSegmentResource("/v1/chat/completions/", "", &[]),
    ),
    native_api_route(
        "POST",
        "/v1/chat/completions/{completion_id}",
        "chat_completions_update",
        ApiSurface::OpenAi,
        Statefulness::ResourceMutate,
        BillingClass::None,
        RoutePath::SingleSegmentResource("/v1/chat/completions/", "", &[]),
    ),
    native_api_route(
        "DELETE",
        "/v1/chat/completions/{completion_id}",
        "chat_completions_delete",
        ApiSurface::OpenAi,
        Statefulness::ResourceDelete,
        BillingClass::None,
        RoutePath::SingleSegmentResource("/v1/chat/completions/", "", &[]),
    ),
    native_api_route(
        "GET",
        "/v1/chat/completions/{completion_id}/messages",
        "chat_completion_messages_list",
        ApiSurface::OpenAi,
        Statefulness::ResourceRead,
        BillingClass::None,
        RoutePath::SingleSegmentResource("/v1/chat/completions/", "/messages", &[]),
    ),
    route(
        "POST",
        "/v1/responses",
        ApiSurface::OpenAi,
        Some(ProtocolKind::OpenAiResponses),
        ApiOperation::Responses,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceCreate,
        BillingClass::Metered,
        RoutePath::Exact("/v1/responses"),
    ),
    route(
        "GET",
        "/v1/responses",
        ApiSurface::OpenAi,
        Some(ProtocolKind::OpenAiResponses),
        ApiOperation::ResponsesWebSocket,
        OperationOwnership::RoutedKnown,
        TransportKind::Websocket,
        Statefulness::ResourceCreate,
        BillingClass::Metered,
        RoutePath::Exact("/v1/responses"),
    ),
    route(
        "GET",
        "/v1/realtime",
        ApiSurface::OpenAi,
        None,
        ApiOperation::RealtimeWebSocket,
        OperationOwnership::RoutedKnown,
        TransportKind::Websocket,
        Statefulness::ResourceCreate,
        BillingClass::Metered,
        RoutePath::Exact("/v1/realtime"),
    ),
    route(
        "POST",
        "/v1/realtime/calls",
        ApiSurface::OpenAi,
        None,
        ApiOperation::RealtimeCallsCreate,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceCreate,
        BillingClass::Metered,
        RoutePath::Exact("/v1/realtime/calls"),
    ),
    route(
        "POST",
        "/v1/live",
        ApiSurface::OpenAi,
        None,
        ApiOperation::RealtimeLiveCallCreate,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceCreate,
        BillingClass::Metered,
        RoutePath::Exact("/v1/live"),
    ),
    route(
        "GET",
        "/v1/live",
        ApiSurface::OpenAi,
        None,
        ApiOperation::RealtimeLiveConnect,
        OperationOwnership::RoutedKnown,
        TransportKind::Websocket,
        Statefulness::ResourceCreate,
        BillingClass::Metered,
        RoutePath::Exact("/v1/live"),
    ),
    route(
        "GET",
        "/v1/live/{call_id}",
        ApiSurface::OpenAi,
        None,
        ApiOperation::RealtimeLiveWebSocket,
        OperationOwnership::RoutedKnown,
        TransportKind::Websocket,
        Statefulness::ResourceMutate,
        BillingClass::Metered,
        RoutePath::SingleSegmentResource("/v1/live/", "", &[]),
    ),
    native_api_route(
        "POST",
        "/v1/realtime/calls/{call_id}/accept",
        "realtime_calls_accept",
        ApiSurface::OpenAi,
        Statefulness::ResourceMutate,
        BillingClass::Metered,
        RoutePath::SingleSegmentResource("/v1/realtime/calls/", "/accept", &[]),
    ),
    native_api_route(
        "POST",
        "/v1/realtime/calls/{call_id}/hangup",
        "realtime_calls_hangup",
        ApiSurface::OpenAi,
        Statefulness::ResourceDelete,
        BillingClass::None,
        RoutePath::SingleSegmentResource("/v1/realtime/calls/", "/hangup", &[]),
    ),
    native_api_route(
        "POST",
        "/v1/realtime/calls/{call_id}/refer",
        "realtime_calls_refer",
        ApiSurface::OpenAi,
        Statefulness::ResourceMutate,
        BillingClass::None,
        RoutePath::SingleSegmentResource("/v1/realtime/calls/", "/refer", &[]),
    ),
    native_api_route(
        "POST",
        "/v1/realtime/calls/{call_id}/reject",
        "realtime_calls_reject",
        ApiSurface::OpenAi,
        Statefulness::ResourceDelete,
        BillingClass::None,
        RoutePath::SingleSegmentResource("/v1/realtime/calls/", "/reject", &[]),
    ),
    native_api_route(
        "POST",
        "/v1/realtime/client_secrets",
        "realtime_client_secrets_create",
        ApiSurface::OpenAi,
        Statefulness::ResourceCreate,
        BillingClass::None,
        RoutePath::Exact("/v1/realtime/client_secrets"),
    ),
    native_api_route(
        "POST",
        "/v1/realtime/sessions",
        "realtime_sessions_create",
        ApiSurface::OpenAi,
        Statefulness::ResourceCreate,
        BillingClass::None,
        RoutePath::Exact("/v1/realtime/sessions"),
    ),
    native_api_route(
        "POST",
        "/v1/realtime/transcription_sessions",
        "realtime_transcription_sessions_create",
        ApiSurface::OpenAi,
        Statefulness::ResourceCreate,
        BillingClass::None,
        RoutePath::Exact("/v1/realtime/transcription_sessions"),
    ),
    native_api_route(
        "POST",
        "/v1/realtime/translations/client_secrets",
        "realtime_translation_client_secrets_create",
        ApiSurface::OpenAi,
        Statefulness::ResourceCreate,
        BillingClass::None,
        RoutePath::Exact("/v1/realtime/translations/client_secrets"),
    ),
    route(
        "GET",
        "/v1/realtime/translations",
        ApiSurface::OpenAi,
        None,
        ApiOperation::RealtimeTranslationWebSocket,
        OperationOwnership::RoutedKnown,
        TransportKind::Websocket,
        Statefulness::ResourceCreate,
        BillingClass::Metered,
        RoutePath::Exact("/v1/realtime/translations"),
    ),
    native_api_route(
        "POST",
        "/v1/realtime/translations/calls",
        "realtime_translation_calls_create",
        ApiSurface::OpenAi,
        Statefulness::ResourceCreate,
        BillingClass::Metered,
        RoutePath::Exact("/v1/realtime/translations/calls"),
    ),
    route(
        "POST",
        "/v1/responses/compact",
        ApiSurface::OpenAi,
        Some(ProtocolKind::OpenAiResponses),
        ApiOperation::ResponsesCompact,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::Stateless,
        BillingClass::Metered,
        RoutePath::Exact("/v1/responses/compact"),
    ),
    route(
        "POST",
        "/v1/responses/input_tokens",
        ApiSurface::OpenAi,
        Some(ProtocolKind::OpenAiResponses),
        ApiOperation::ResponsesInputTokens,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::Stateless,
        BillingClass::None,
        RoutePath::Exact("/v1/responses/input_tokens"),
    ),
    route(
        "GET",
        "/v1/responses/{response_id}",
        ApiSurface::OpenAi,
        Some(ProtocolKind::OpenAiResponses),
        ApiOperation::ResponsesGet,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceRead,
        BillingClass::None,
        RoutePath::OpenAiResponseResource(""),
    ),
    route(
        "DELETE",
        "/v1/responses/{response_id}",
        ApiSurface::OpenAi,
        Some(ProtocolKind::OpenAiResponses),
        ApiOperation::ResponsesDelete,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceDelete,
        BillingClass::None,
        RoutePath::OpenAiResponseResource(""),
    ),
    route(
        "POST",
        "/v1/responses/{response_id}/cancel",
        ApiSurface::OpenAi,
        Some(ProtocolKind::OpenAiResponses),
        ApiOperation::ResponsesCancel,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceMutate,
        BillingClass::None,
        RoutePath::OpenAiResponseResource("/cancel"),
    ),
    route(
        "GET",
        "/v1/responses/{response_id}/input_items",
        ApiSurface::OpenAi,
        Some(ProtocolKind::OpenAiResponses),
        ApiOperation::ResponsesInputItems,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceRead,
        BillingClass::None,
        RoutePath::OpenAiResponseResource("/input_items"),
    ),
    route(
        "POST",
        "/v1/conversations",
        ApiSurface::OpenAi,
        None,
        ApiOperation::ConversationsCreate,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceCreate,
        BillingClass::None,
        RoutePath::Exact("/v1/conversations"),
    ),
    route(
        "GET",
        "/v1/conversations/{conversation_id}",
        ApiSurface::OpenAi,
        None,
        ApiOperation::ConversationsGet,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceRead,
        BillingClass::None,
        RoutePath::OpenAiConversationResource(""),
    ),
    route(
        "POST",
        "/v1/conversations/{conversation_id}",
        ApiSurface::OpenAi,
        None,
        ApiOperation::ConversationsUpdate,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceMutate,
        BillingClass::None,
        RoutePath::OpenAiConversationResource(""),
    ),
    route(
        "DELETE",
        "/v1/conversations/{conversation_id}",
        ApiSurface::OpenAi,
        None,
        ApiOperation::ConversationsDelete,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceDelete,
        BillingClass::None,
        RoutePath::OpenAiConversationResource(""),
    ),
    route(
        "POST",
        "/v1/conversations/{conversation_id}/items",
        ApiSurface::OpenAi,
        None,
        ApiOperation::ConversationItemsCreate,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceMutate,
        BillingClass::None,
        RoutePath::OpenAiConversationResource("/items"),
    ),
    route(
        "GET",
        "/v1/conversations/{conversation_id}/items",
        ApiSurface::OpenAi,
        None,
        ApiOperation::ConversationItemsList,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceRead,
        BillingClass::None,
        RoutePath::OpenAiConversationResource("/items"),
    ),
    route(
        "GET",
        "/v1/conversations/{conversation_id}/items/{item_id}",
        ApiSurface::OpenAi,
        None,
        ApiOperation::ConversationItemGet,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceRead,
        BillingClass::None,
        RoutePath::OpenAiConversationItem,
    ),
    route(
        "DELETE",
        "/v1/conversations/{conversation_id}/items/{item_id}",
        ApiSurface::OpenAi,
        None,
        ApiOperation::ConversationItemDelete,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceDelete,
        BillingClass::None,
        RoutePath::OpenAiConversationItem,
    ),
    route(
        "POST",
        "/v1/embeddings",
        ApiSurface::OpenAi,
        None,
        ApiOperation::Embeddings,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::Stateless,
        BillingClass::Metered,
        RoutePath::Exact("/v1/embeddings"),
    ),
    native_api_route(
        "POST",
        "/v1/moderations",
        "moderations",
        ApiSurface::OpenAi,
        Statefulness::Stateless,
        BillingClass::None,
        RoutePath::Exact("/v1/moderations"),
    ),
    route(
        "POST",
        "/v1/content_provenance_checks",
        ApiSurface::OpenAi,
        None,
        ApiOperation::ContentProvenanceChecks,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::Stateless,
        BillingClass::None,
        RoutePath::Exact("/v1/content_provenance_checks"),
    ),
    route(
        "POST",
        "/v1/files",
        ApiSurface::OpenAi,
        None,
        ApiOperation::FilesCreate,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceCreate,
        BillingClass::Conditional,
        RoutePath::Exact("/v1/files"),
    ),
    route(
        "GET",
        "/v1/files",
        ApiSurface::OpenAi,
        None,
        ApiOperation::FilesList,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceRead,
        BillingClass::Conditional,
        RoutePath::Exact("/v1/files"),
    ),
    route(
        "GET",
        "/v1/files/{file_id}",
        ApiSurface::OpenAi,
        None,
        ApiOperation::FilesGet,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceRead,
        BillingClass::Conditional,
        RoutePath::SingleSegmentResource("/v1/files/", "", &[]),
    ),
    route(
        "DELETE",
        "/v1/files/{file_id}",
        ApiSurface::OpenAi,
        None,
        ApiOperation::FilesDelete,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceDelete,
        BillingClass::Conditional,
        RoutePath::SingleSegmentResource("/v1/files/", "", &[]),
    ),
    route(
        "GET",
        "/v1/files/{file_id}/content",
        ApiSurface::OpenAi,
        None,
        ApiOperation::FilesContent,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceRead,
        BillingClass::Conditional,
        RoutePath::SingleSegmentResource("/v1/files/", "/content", &[]),
    ),
    route(
        "POST",
        "/v1/uploads",
        ApiSurface::OpenAi,
        None,
        ApiOperation::UploadsCreate,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceCreate,
        BillingClass::None,
        RoutePath::Exact("/v1/uploads"),
    ),
    route(
        "POST",
        "/v1/uploads/{upload_id}/parts",
        ApiSurface::OpenAi,
        None,
        ApiOperation::UploadPartsCreate,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceMutate,
        BillingClass::None,
        RoutePath::SingleSegmentResource("/v1/uploads/", "/parts", &[]),
    ),
    route(
        "POST",
        "/v1/uploads/{upload_id}/complete",
        ApiSurface::OpenAi,
        None,
        ApiOperation::UploadsComplete,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceMutate,
        BillingClass::None,
        RoutePath::SingleSegmentResource("/v1/uploads/", "/complete", &[]),
    ),
    route(
        "POST",
        "/v1/uploads/{upload_id}/cancel",
        ApiSurface::OpenAi,
        None,
        ApiOperation::UploadsCancel,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceDelete,
        BillingClass::None,
        RoutePath::SingleSegmentResource("/v1/uploads/", "/cancel", &[]),
    ),
    route(
        "POST",
        "/v1/batches",
        ApiSurface::OpenAi,
        None,
        ApiOperation::BatchesCreate,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceCreate,
        BillingClass::Conditional,
        RoutePath::Exact("/v1/batches"),
    ),
    route(
        "GET",
        "/v1/batches",
        ApiSurface::OpenAi,
        None,
        ApiOperation::BatchesList,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceRead,
        BillingClass::Conditional,
        RoutePath::Exact("/v1/batches"),
    ),
    route(
        "GET",
        "/v1/batches/{batch_id}",
        ApiSurface::OpenAi,
        None,
        ApiOperation::BatchesGet,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceRead,
        BillingClass::Conditional,
        RoutePath::SingleSegmentResource("/v1/batches/", "", &[]),
    ),
    route(
        "POST",
        "/v1/batches/{batch_id}/cancel",
        ApiSurface::OpenAi,
        None,
        ApiOperation::BatchesCancel,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceMutate,
        BillingClass::Conditional,
        RoutePath::SingleSegmentResource("/v1/batches/", "/cancel", &[]),
    ),
    route(
        "POST",
        "/v1/images/generations",
        ApiSurface::OpenAi,
        None,
        ApiOperation::ImagesGenerations,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::Stateless,
        BillingClass::Metered,
        RoutePath::Exact("/v1/images/generations"),
    ),
    route(
        "POST",
        "/v1/images/edits",
        ApiSurface::OpenAi,
        None,
        ApiOperation::ImagesEdits,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::Stateless,
        BillingClass::Metered,
        RoutePath::Exact("/v1/images/edits"),
    ),
    route(
        "POST",
        "/v1/images/variations",
        ApiSurface::OpenAi,
        None,
        ApiOperation::ImagesVariations,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::Stateless,
        BillingClass::Metered,
        RoutePath::Exact("/v1/images/variations"),
    ),
    route(
        "POST",
        "/v1/audio/speech",
        ApiSurface::OpenAi,
        None,
        ApiOperation::AudioSpeech,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::Stateless,
        BillingClass::Metered,
        RoutePath::Exact("/v1/audio/speech"),
    ),
    route(
        "POST",
        "/v1/audio/transcriptions",
        ApiSurface::OpenAi,
        None,
        ApiOperation::AudioTranscriptions,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::Stateless,
        BillingClass::Metered,
        RoutePath::Exact("/v1/audio/transcriptions"),
    ),
    route(
        "POST",
        "/v1/audio/translations",
        ApiSurface::OpenAi,
        None,
        ApiOperation::AudioTranslations,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::Stateless,
        BillingClass::Metered,
        RoutePath::Exact("/v1/audio/translations"),
    ),
    route(
        "POST",
        "/v1/videos",
        ApiSurface::OpenAi,
        None,
        ApiOperation::VideosCreate,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceCreate,
        BillingClass::Metered,
        RoutePath::Exact("/v1/videos"),
    ),
    route(
        "GET",
        "/v1/videos",
        ApiSurface::OpenAi,
        None,
        ApiOperation::VideosList,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceRead,
        BillingClass::None,
        RoutePath::Exact("/v1/videos"),
    ),
    route(
        "GET",
        "/v1/videos/{video_id}",
        ApiSurface::OpenAi,
        None,
        ApiOperation::VideosGet,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceRead,
        BillingClass::None,
        RoutePath::SingleSegmentResource("/v1/videos/", "", &["edits", "extensions", "characters"]),
    ),
    route(
        "DELETE",
        "/v1/videos/{video_id}",
        ApiSurface::OpenAi,
        None,
        ApiOperation::VideosDelete,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceDelete,
        BillingClass::None,
        RoutePath::SingleSegmentResource("/v1/videos/", "", &["edits", "extensions", "characters"]),
    ),
    route(
        "GET",
        "/v1/videos/{video_id}/content",
        ApiSurface::OpenAi,
        None,
        ApiOperation::VideosContent,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceRead,
        BillingClass::None,
        RoutePath::SingleSegmentResource("/v1/videos/", "/content", &[]),
    ),
    route(
        "POST",
        "/v1/videos/{video_id}/remix",
        ApiSurface::OpenAi,
        None,
        ApiOperation::VideosRemix,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceCreate,
        BillingClass::Metered,
        RoutePath::SingleSegmentResource("/v1/videos/", "/remix", &[]),
    ),
    route(
        "POST",
        "/v1/videos/edits",
        ApiSurface::OpenAi,
        None,
        ApiOperation::VideosEdit,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceCreate,
        BillingClass::Metered,
        RoutePath::Exact("/v1/videos/edits"),
    ),
    route(
        "POST",
        "/v1/videos/extensions",
        ApiSurface::OpenAi,
        None,
        ApiOperation::VideosExtend,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceCreate,
        BillingClass::Metered,
        RoutePath::Exact("/v1/videos/extensions"),
    ),
    route(
        "GET",
        "/anthropic/v1/models",
        ApiSurface::Anthropic,
        None,
        ApiOperation::ListModels,
        OperationOwnership::AggregatedControl,
        TransportKind::Http,
        Statefulness::Stateless,
        BillingClass::None,
        RoutePath::Exact("/anthropic/v1/models"),
    ),
    route(
        "GET",
        "/anthropic/v1/models/{model}",
        ApiSurface::Anthropic,
        None,
        ApiOperation::GetModel,
        OperationOwnership::AggregatedControl,
        TransportKind::Http,
        Statefulness::Stateless,
        BillingClass::None,
        RoutePath::ModelDetail("/anthropic/v1/models/"),
    ),
    route(
        "POST",
        "/anthropic/v1/messages",
        ApiSurface::Anthropic,
        Some(ProtocolKind::AnthropicMessages),
        ApiOperation::Messages,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::Stateless,
        BillingClass::Metered,
        RoutePath::Exact("/anthropic/v1/messages"),
    ),
    route(
        "POST",
        "/anthropic/v1/messages/count_tokens",
        ApiSurface::Anthropic,
        Some(ProtocolKind::AnthropicMessages),
        ApiOperation::CountTokens,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::Stateless,
        BillingClass::None,
        RoutePath::Exact("/anthropic/v1/messages/count_tokens"),
    ),
    route(
        "POST",
        "/anthropic/v1/files",
        ApiSurface::Anthropic,
        None,
        ApiOperation::FilesCreate,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceCreate,
        BillingClass::Conditional,
        RoutePath::Exact("/anthropic/v1/files"),
    ),
    route(
        "GET",
        "/anthropic/v1/files",
        ApiSurface::Anthropic,
        None,
        ApiOperation::FilesList,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceRead,
        BillingClass::Conditional,
        RoutePath::Exact("/anthropic/v1/files"),
    ),
    route(
        "GET",
        "/anthropic/v1/files/{file_id}",
        ApiSurface::Anthropic,
        None,
        ApiOperation::FilesGet,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceRead,
        BillingClass::Conditional,
        RoutePath::SingleSegmentResource("/anthropic/v1/files/", "", &[]),
    ),
    route(
        "DELETE",
        "/anthropic/v1/files/{file_id}",
        ApiSurface::Anthropic,
        None,
        ApiOperation::FilesDelete,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceDelete,
        BillingClass::Conditional,
        RoutePath::SingleSegmentResource("/anthropic/v1/files/", "", &[]),
    ),
    route(
        "GET",
        "/anthropic/v1/files/{file_id}/content",
        ApiSurface::Anthropic,
        None,
        ApiOperation::FilesContent,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceRead,
        BillingClass::Conditional,
        RoutePath::SingleSegmentResource("/anthropic/v1/files/", "/content", &[]),
    ),
    route(
        "POST",
        "/anthropic/v1/messages/batches",
        ApiSurface::Anthropic,
        None,
        ApiOperation::BatchesCreate,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceCreate,
        BillingClass::Conditional,
        RoutePath::Exact("/anthropic/v1/messages/batches"),
    ),
    route(
        "GET",
        "/anthropic/v1/messages/batches",
        ApiSurface::Anthropic,
        None,
        ApiOperation::BatchesList,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceRead,
        BillingClass::Conditional,
        RoutePath::Exact("/anthropic/v1/messages/batches"),
    ),
    route(
        "GET",
        "/anthropic/v1/messages/batches/{message_batch_id}",
        ApiSurface::Anthropic,
        None,
        ApiOperation::BatchesGet,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceRead,
        BillingClass::Conditional,
        RoutePath::SingleSegmentResource("/anthropic/v1/messages/batches/", "", &[]),
    ),
    route(
        "POST",
        "/anthropic/v1/messages/batches/{message_batch_id}/cancel",
        ApiSurface::Anthropic,
        None,
        ApiOperation::BatchesCancel,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceMutate,
        BillingClass::Conditional,
        RoutePath::SingleSegmentResource("/anthropic/v1/messages/batches/", "/cancel", &[]),
    ),
    route(
        "DELETE",
        "/anthropic/v1/messages/batches/{message_batch_id}",
        ApiSurface::Anthropic,
        None,
        ApiOperation::BatchesDelete,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceDelete,
        BillingClass::Conditional,
        RoutePath::SingleSegmentResource("/anthropic/v1/messages/batches/", "", &[]),
    ),
    route(
        "GET",
        "/anthropic/v1/messages/batches/{message_batch_id}/results",
        ApiSurface::Anthropic,
        None,
        ApiOperation::BatchesResults,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceRead,
        BillingClass::Conditional,
        RoutePath::SingleSegmentResource("/anthropic/v1/messages/batches/", "/results", &[]),
    ),
    route(
        "GET",
        "/gemini/v1beta/models",
        ApiSurface::Gemini,
        None,
        ApiOperation::ListModels,
        OperationOwnership::AggregatedControl,
        TransportKind::Http,
        Statefulness::Stateless,
        BillingClass::None,
        RoutePath::Exact("/gemini/v1beta/models"),
    ),
    route(
        "GET",
        "/gemini/v1beta/models/{model}",
        ApiSurface::Gemini,
        None,
        ApiOperation::GetModel,
        OperationOwnership::AggregatedControl,
        TransportKind::Http,
        Statefulness::Stateless,
        BillingClass::None,
        RoutePath::ModelDetail("/gemini/v1beta/models/"),
    ),
    route(
        "POST",
        "/gemini/v1beta/models/{model}:generateContent",
        ApiSurface::Gemini,
        Some(ProtocolKind::GeminiNative),
        ApiOperation::GenerateContent,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::Stateless,
        BillingClass::Metered,
        RoutePath::GeminiAction(":generateContent"),
    ),
    route(
        "POST",
        "/gemini/v1beta/models/{model}:streamGenerateContent",
        ApiSurface::Gemini,
        Some(ProtocolKind::GeminiNative),
        ApiOperation::StreamGenerateContent,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::Stateless,
        BillingClass::Metered,
        RoutePath::GeminiAction(":streamGenerateContent"),
    ),
    route(
        "POST",
        "/gemini/v1beta/models/{model}:countTokens",
        ApiSurface::Gemini,
        Some(ProtocolKind::GeminiNative),
        ApiOperation::CountTokens,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::Stateless,
        BillingClass::None,
        RoutePath::GeminiAction(":countTokens"),
    ),
    route(
        "POST",
        "/gemini/v1beta/models/{model}:embedContent",
        ApiSurface::Gemini,
        Some(ProtocolKind::GeminiNative),
        ApiOperation::EmbedContent,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::Stateless,
        BillingClass::Metered,
        RoutePath::GeminiAction(":embedContent"),
    ),
    native_api_route(
        "POST",
        "/gemini/v1beta/models/{model}:batchEmbedContents",
        "batch_embed_contents",
        ApiSurface::Gemini,
        Statefulness::Stateless,
        BillingClass::Metered,
        RoutePath::GeminiAction(":batchEmbedContents"),
    ),
    route(
        "POST",
        "/gemini/v1beta/interactions",
        ApiSurface::Gemini,
        Some(ProtocolKind::GeminiNative),
        ApiOperation::InteractionsCreate,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceCreate,
        BillingClass::Metered,
        RoutePath::Exact("/gemini/v1beta/interactions"),
    ),
    route(
        "GET",
        "/gemini/v1beta/interactions/{interaction_id}",
        ApiSurface::Gemini,
        Some(ProtocolKind::GeminiNative),
        ApiOperation::InteractionsGet,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceRead,
        BillingClass::None,
        RoutePath::GeminiInteractionResource(""),
    ),
    route(
        "DELETE",
        "/gemini/v1beta/interactions/{interaction_id}",
        ApiSurface::Gemini,
        Some(ProtocolKind::GeminiNative),
        ApiOperation::InteractionsDelete,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceDelete,
        BillingClass::None,
        RoutePath::GeminiInteractionResource(""),
    ),
    route(
        "POST",
        "/gemini/v1beta/interactions/{interaction_id}/cancel",
        ApiSurface::Gemini,
        Some(ProtocolKind::GeminiNative),
        ApiOperation::InteractionsCancel,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceMutate,
        BillingClass::None,
        RoutePath::GeminiInteractionResource("/cancel"),
    ),
    route(
        "POST",
        "/gemini/v1beta/cachedContents",
        ApiSurface::Gemini,
        Some(ProtocolKind::GeminiNative),
        ApiOperation::CachedContentsCreate,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceCreate,
        BillingClass::Conditional,
        RoutePath::Exact("/gemini/v1beta/cachedContents"),
    ),
    route(
        "GET",
        "/gemini/v1beta/cachedContents",
        ApiSurface::Gemini,
        Some(ProtocolKind::GeminiNative),
        ApiOperation::CachedContentsList,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceRead,
        BillingClass::Conditional,
        RoutePath::Exact("/gemini/v1beta/cachedContents"),
    ),
    route(
        "GET",
        "/gemini/v1beta/cachedContents/{cached_content_id}",
        ApiSurface::Gemini,
        Some(ProtocolKind::GeminiNative),
        ApiOperation::CachedContentsGet,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceRead,
        BillingClass::Conditional,
        RoutePath::SingleSegmentResource("/gemini/v1beta/cachedContents/", "", &[]),
    ),
    route(
        "PATCH",
        "/gemini/v1beta/cachedContents/{cached_content_id}",
        ApiSurface::Gemini,
        Some(ProtocolKind::GeminiNative),
        ApiOperation::CachedContentsUpdate,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceMutate,
        BillingClass::Conditional,
        RoutePath::SingleSegmentResource("/gemini/v1beta/cachedContents/", "", &[]),
    ),
    route(
        "DELETE",
        "/gemini/v1beta/cachedContents/{cached_content_id}",
        ApiSurface::Gemini,
        Some(ProtocolKind::GeminiNative),
        ApiOperation::CachedContentsDelete,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceDelete,
        BillingClass::Conditional,
        RoutePath::SingleSegmentResource("/gemini/v1beta/cachedContents/", "", &[]),
    ),
    route(
        "POST",
        "/gemini/upload/v1beta/files",
        ApiSurface::Gemini,
        None,
        ApiOperation::FilesCreate,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceCreate,
        BillingClass::None,
        RoutePath::Exact("/gemini/upload/v1beta/files"),
    ),
    route(
        "GET",
        "/gemini/v1beta/files",
        ApiSurface::Gemini,
        None,
        ApiOperation::FilesList,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceRead,
        BillingClass::None,
        RoutePath::Exact("/gemini/v1beta/files"),
    ),
    route(
        "GET",
        "/gemini/v1beta/files/{file_id}",
        ApiSurface::Gemini,
        None,
        ApiOperation::FilesGet,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceRead,
        BillingClass::None,
        RoutePath::SingleSegmentResource("/gemini/v1beta/files/", "", &[]),
    ),
    route(
        "DELETE",
        "/gemini/v1beta/files/{file_id}",
        ApiSurface::Gemini,
        None,
        ApiOperation::FilesDelete,
        OperationOwnership::RoutedKnown,
        TransportKind::Http,
        Statefulness::ResourceDelete,
        BillingClass::None,
        RoutePath::SingleSegmentResource("/gemini/v1beta/files/", "", &[]),
    ),
    native_api_route(
        "POST",
        "/gemini/v1beta/auth_tokens",
        "live_auth_tokens_create",
        ApiSurface::Gemini,
        Statefulness::ResourceCreate,
        BillingClass::None,
        RoutePath::Exact("/gemini/v1beta/auth_tokens"),
    ),
    route(
        "GET",
        "/gemini/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent",
        ApiSurface::Gemini,
        Some(ProtocolKind::GeminiNative),
        ApiOperation::LiveWebSocket,
        OperationOwnership::RoutedKnown,
        TransportKind::Websocket,
        Statefulness::ResourceCreate,
        BillingClass::Metered,
        RoutePath::Exact(
            "/gemini/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent",
        ),
    ),
];

const fn route(
    method: &'static str,
    path_template: &'static str,
    surface: ApiSurface,
    protocol: Option<ProtocolKind>,
    operation: ApiOperation,
    ownership: OperationOwnership,
    transport: TransportKind,
    statefulness: Statefulness,
    billing_class: BillingClass,
    path: RoutePath,
) -> RouteSpec {
    RouteSpec {
        method,
        path_template,
        surface,
        protocol,
        operation,
        ownership,
        transport,
        statefulness,
        billing_class,
        operation_name: None,
        native_api_only: false,
        path,
    }
}

const fn native_api_route(
    method: &'static str,
    path_template: &'static str,
    operation_name: &'static str,
    surface: ApiSurface,
    statefulness: Statefulness,
    billing_class: BillingClass,
    path: RoutePath,
) -> RouteSpec {
    RouteSpec {
        method,
        path_template,
        surface,
        protocol: None,
        operation: ApiOperation::Opaque,
        ownership: OperationOwnership::RoutedKnown,
        transport: TransportKind::Http,
        statefulness,
        billing_class,
        operation_name: Some(operation_name),
        native_api_only: true,
        path,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApiRoute {
    pub(crate) surface: ApiSurface,
    pub(crate) operation: ApiOperation,
    pub(crate) protocol: Option<ProtocolKind>,
    pub(crate) relative_path: String,
    pub(crate) path_template: &'static str,
    pub(crate) ownership: OperationOwnership,
    pub(crate) transport: TransportKind,
    pub(crate) statefulness: Statefulness,
    pub(crate) billing_class: BillingClass,
}

pub(crate) fn surface_from_path(path: &str) -> Option<ApiSurface> {
    if path == "/v1" || path.starts_with("/v1/") {
        Some(ApiSurface::OpenAi)
    } else if path == "/anthropic" || path.starts_with("/anthropic/") {
        Some(ApiSurface::Anthropic)
    } else if path == "/gemini" || path.starts_with("/gemini/") {
        Some(ApiSurface::Gemini)
    } else {
        None
    }
}

pub(crate) fn is_reserved_management_path(path: &str) -> bool {
    [
        "/v1/organization",
        "/v1/projects",
        "/anthropic/v1/organizations",
    ]
    .iter()
    .any(|prefix| path == *prefix || path.starts_with(&format!("{prefix}/")))
}

pub(crate) fn resolve_api_route(method: &str, path: &str, websocket: bool) -> Option<ApiRoute> {
    if is_reserved_management_path(path) {
        return None;
    }
    let surface = surface_from_path(path)?;
    let transport = if websocket {
        TransportKind::Websocket
    } else {
        TransportKind::Http
    };
    if let Some(spec) = ROUTE_SPECS.iter().copied().find(|spec| {
        spec.surface == surface && spec.transport == transport && spec.matches(method, path)
    }) {
        return Some(ApiRoute {
            surface,
            operation: spec.operation,
            protocol: spec.protocol,
            relative_path: surface_relative_path(surface, path).to_string(),
            path_template: spec.path_template,
            ownership: spec.ownership,
            transport: spec.transport,
            statefulness: spec.statefulness,
            billing_class: spec.billing_class,
        });
    }
    if websocket {
        return None;
    }
    Some(ApiRoute {
        surface,
        operation: ApiOperation::Opaque,
        protocol: None,
        relative_path: surface_relative_path(surface, path).to_string(),
        path_template: "{opaque}",
        ownership: OperationOwnership::RoutedOpaque,
        transport: TransportKind::Http,
        statefulness: Statefulness::Stateless,
        billing_class: BillingClass::Conditional,
    })
}

pub(crate) fn known_route_allowed_methods(
    path: &str,
    websocket: bool,
) -> Option<(ApiSurface, Vec<&'static str>)> {
    let surface = surface_from_path(path)?;
    let transport = if websocket {
        TransportKind::Websocket
    } else {
        TransportKind::Http
    };
    let mut methods = Vec::new();
    for spec in ROUTE_SPECS.iter().filter(|spec| {
        spec.surface == surface && spec.transport == transport && spec.path.matches(path)
    }) {
        if !methods.contains(&spec.method) {
            methods.push(spec.method);
        }
    }
    (!methods.is_empty()).then_some((surface, methods))
}

pub(crate) fn provider_relative_path(path: &str) -> &str {
    surface_from_path(path)
        .map(|surface| surface_relative_path(surface, path))
        .unwrap_or(path)
}

fn surface_relative_path(surface: ApiSurface, path: &str) -> &str {
    match surface {
        ApiSurface::OpenAi => path,
        ApiSurface::Anthropic => path.strip_prefix("/anthropic").unwrap_or(path),
        ApiSurface::Gemini => path.strip_prefix("/gemini").unwrap_or(path),
    }
}

pub(crate) fn surface_manifest() -> serde_json::Value {
    let contract = api_surface_contract_document();
    let surfaces = [ApiSurface::OpenAi, ApiSurface::Anthropic, ApiSurface::Gemini]
        .into_iter()
        .map(|surface| {
            let protocols = match surface {
                ApiSurface::OpenAi => vec!["openai_responses", "openai_chat"],
                ApiSurface::Anthropic => vec!["anthropic_messages"],
                ApiSurface::Gemini => vec!["gemini_native"],
            };
            let authentication = match surface {
                ApiSurface::OpenAi => vec!["bearer", "x_api_key", "api_key"],
                ApiSurface::Anthropic => vec!["x_api_key", "bearer"],
                ApiSurface::Gemini => vec!["x_goog_api_key", "query_key", "bearer"],
            };
            serde_json::json!({
                "id": surface.as_str(),
                "mount": surface.mount(),
                "protocols": protocols,
                "authentication": authentication,
                "operations": ROUTE_SPECS.iter().filter(|spec| spec.surface == surface).map(|spec| serde_json::json!({
                    "id": spec.operation_id(),
                    "method": spec.method,
                    "path": spec.path_template,
                    "operation": spec.operation.as_str(),
                    "transport": spec.transport,
                    "ownership": spec.ownership,
                    "statefulness": spec.statefulness,
                    "billing_class": spec.billing_class,
                    "request_body_mode": spec.request_body_mode(),
                    "response_modes": spec.response_modes(),
                    "execution_policy": spec.execution_policy(),
                    "required_context_declarations": spec.required_context_declarations(),
                    "required_conversion_evidence": spec.required_conversion_evidence(),
                })).collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "version": contract["manifest_version"],
        "contract": {
            "schema_version": contract["schema_version"],
            "execution_contexts": contract["execution_contexts"],
            "conversion_levels": contract["conversion_levels"],
            "maturity_levels": contract["maturity_levels"],
            "support_evidence": "declared_per_backend"
        },
        "official_families": contract["official_families"],
        "surfaces": surfaces,
        "limits": { "max_opaque_request_bytes": 67108864 },
        "management_api": {
            "exposed": false,
            "reason": "Organization, workspace, and project administration require independent management authentication and audit."
        }
    })
}

pub(crate) fn surface_index_document() -> serde_json::Value {
    serde_json::json!({
        "name": "CONST API",
        "status": "running",
        "description": "Standard OpenAI, Anthropic, and Gemini API surfaces backed by configured API credentials, subscription accounts, or platform supply.",
        "surfaces": [
            {"id": "openai", "base_path": "/v1"},
            {"id": "anthropic", "base_path": "/anthropic"},
            {"id": "gemini", "base_path": "/gemini"}
        ],
        "execution_backends": [
            {"id": "api_credential", "short": "A", "public_protocol": false},
            {"id": "subscription_driver", "short": "S", "public_protocol": false},
            {"id": "platform_supply", "short": "P", "public_protocol": false}
        ],
        "links": {
            "capabilities": "/.well-known/const-api",
            "health": "/healthz"
        },
        "management_api": {
            "exposed": false,
            "reason": "Organization, workspace, and project administration are intentionally outside the model API proxy."
        }
    })
}

pub(crate) const fn surface_index_html() -> &'static str {
    r#"<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <title>CONST API</title>
  <style>
    :root{color-scheme:light dark;font-family:ui-sans-serif,system-ui,-apple-system,sans-serif}
    body{max-width:760px;margin:0 auto;padding:48px 24px;line-height:1.6}
    h1{margin:0 0 8px;font-size:32px}.muted{opacity:.68}
    .grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(190px,1fr));gap:12px;margin:28px 0}
    .card{border:1px solid color-mix(in srgb,currentColor 18%,transparent);border-radius:12px;padding:16px}
    code{font-family:ui-monospace,SFMono-Regular,Consolas,monospace}
    a{color:inherit}.status{display:inline-flex;align-items:center;gap:8px}
    .dot{width:9px;height:9px;border-radius:50%;background:#0a6}
  </style>
</head>
<body>
  <div class="status"><span class="dot"></span><span>本地 API 运行中</span></div>
  <h1>CONST API</h1>
  <p class="muted">调用方只使用标准 API。API Key、账号订阅和平台供应是内部执行后端，不是额外的私有调用协议。</p>
  <div class="grid">
    <div class="card"><strong>OpenAI</strong><br><code>/v1</code></div>
    <div class="card"><strong>Anthropic</strong><br><code>/anthropic</code></div>
    <div class="card"><strong>Gemini</strong><br><code>/gemini</code></div>
  </div>
  <p><a href="/.well-known/const-api">查看机器可读能力清单</a> · <a href="/healthz">健康检查</a></p>
  <p class="muted">组织与项目管理 API 需要独立的管理认证和审计，因此不通过本模型 API 代理公开。</p>
</body>
</html>"#
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_surface_keeps_its_v5_storage_name_and_accepts_buggy_writes() {
        assert_eq!(
            serde_json::to_string(&ApiSurface::OpenAi).unwrap(),
            r#""open_ai""#
        );
        assert_eq!(
            serde_json::from_str::<ApiSurface>(r#""openai""#).unwrap(),
            ApiSurface::OpenAi
        );
        assert_eq!(
            serde_json::from_str::<ApiSurface>(r#""open_ai""#).unwrap(),
            ApiSurface::OpenAi
        );
    }

    #[test]
    fn websocket_operations_use_the_cross_language_contract_names() {
        for (operation, canonical, legacy) in [
            (
                ApiOperation::ResponsesWebSocket,
                "responses_websocket",
                "responses_web_socket",
            ),
            (
                ApiOperation::RealtimeWebSocket,
                "realtime_websocket",
                "realtime_web_socket",
            ),
            (
                ApiOperation::LiveWebSocket,
                "live_websocket",
                "live_web_socket",
            ),
        ] {
            assert_eq!(
                serde_json::to_string(&operation).unwrap(),
                format!("\"{canonical}\"")
            );
            assert_eq!(
                serde_json::from_str::<ApiOperation>(&format!("\"{canonical}\"")).unwrap(),
                operation
            );
            assert_eq!(
                serde_json::from_str::<ApiOperation>(&format!("\"{legacy}\"")).unwrap(),
                operation
            );
        }
    }

    #[test]
    fn resolves_the_three_canonical_surfaces() {
        let responses = resolve_api_route("POST", "/v1/responses", false).unwrap();
        assert_eq!(responses.surface, ApiSurface::OpenAi);
        assert_eq!(responses.operation, ApiOperation::Responses);
        assert_eq!(responses.protocol, Some(ProtocolKind::OpenAiResponses));

        let messages = resolve_api_route("POST", "/anthropic/v1/messages", false).unwrap();
        assert_eq!(messages.surface, ApiSurface::Anthropic);
        assert_eq!(messages.protocol, Some(ProtocolKind::AnthropicMessages));
        assert_eq!(messages.relative_path, "/v1/messages");

        let gemini = resolve_api_route(
            "POST",
            "/gemini/v1beta/models/gemini-3:streamGenerateContent",
            false,
        )
        .unwrap();
        assert_eq!(gemini.surface, ApiSurface::Gemini);
        assert_eq!(gemini.protocol, Some(ProtocolKind::GeminiNative));
        assert_eq!(
            gemini.relative_path,
            "/v1beta/models/gemini-3:streamGenerateContent"
        );
    }

    #[test]
    fn route_registry_carries_transport_ownership_state_and_billing() {
        let models = resolve_api_route("GET", "/v1/models", false).unwrap();
        assert_eq!(models.ownership, OperationOwnership::AggregatedControl);
        assert_eq!(models.billing_class, BillingClass::None);
        let websocket = resolve_api_route("GET", "/v1/responses", true).unwrap();
        assert_eq!(websocket.operation, ApiOperation::ResponsesWebSocket);
        assert_eq!(websocket.transport, TransportKind::Websocket);
        let opaque = resolve_api_route("POST", "/v1/future-operation", false).unwrap();
        assert_eq!(opaque.ownership, OperationOwnership::RoutedOpaque);
        assert_eq!(opaque.billing_class, BillingClass::Conditional);
    }

    #[test]
    fn model_details_are_control_operations_not_generation_protocols() {
        let route = resolve_api_route("GET", "/v1/models/gpt-5.6", false).unwrap();
        assert_eq!(route.operation, ApiOperation::GetModel);
        assert_eq!(route.protocol, None);
    }

    #[test]
    fn official_family_lookup_uses_the_longest_public_path_and_excludes_management() {
        assert_eq!(
            official_model_api_family_id_for_path(
                ApiSurface::OpenAi,
                "/v1/chat/completions/chatcmpl_1/messages"
            )
            .as_deref(),
            Some("openai.chat_completions")
        );
        assert_eq!(
            official_model_api_family_id_for_path(
                ApiSurface::Anthropic,
                "/anthropic/v1/memory_stores/store_1/memories"
            )
            .as_deref(),
            Some("anthropic.skills_agents")
        );
        assert_eq!(
            official_model_api_family_id_for_path(
                ApiSurface::Gemini,
                "/gemini/v1beta/models/veo:predictLongRunning"
            )
            .as_deref(),
            Some("gemini.generated_media")
        );
        assert!(
            official_model_api_family_id_for_path(
                ApiSurface::OpenAi,
                "/v1/organization/audit_logs"
            )
            .is_none()
        );
    }

    #[test]
    fn unknown_surface_paths_are_opaque_but_control_paths_are_not_surfaces() {
        let opaque = resolve_api_route("POST", "/v1/future-operation", false).unwrap();
        assert_eq!(opaque.operation, ApiOperation::Opaque);
        assert_eq!(opaque.surface, ApiSurface::OpenAi);
        assert!(resolve_api_route("GET", "/api/me", false).is_none());
    }

    #[test]
    fn organization_and_project_management_are_never_opaque_model_routes() {
        for path in [
            "/v1/organization",
            "/v1/organization/audit_logs",
            "/v1/organization/projects/project_1",
            "/v1/projects/project_1/api_keys",
            "/anthropic/v1/organizations",
            "/anthropic/v1/organizations/workspaces/workspace_1",
        ] {
            assert!(is_reserved_management_path(path), "{path}");
            assert!(resolve_api_route("GET", path, false).is_none(), "{path}");
            assert!(known_route_allowed_methods(path, false).is_none(), "{path}");
        }
        assert!(!is_reserved_management_path("/v1/organizations"));
        assert!(!is_reserved_management_path("/v1/project"));
        assert!(!is_reserved_management_path("/anthropic/v1/organization"));
    }

    #[test]
    fn strips_only_the_public_surface_mount() {
        assert_eq!(provider_relative_path("/v1/responses"), "/v1/responses");
        assert_eq!(
            provider_relative_path("/anthropic/v1/messages"),
            "/v1/messages"
        );
        assert_eq!(
            provider_relative_path("/gemini/v1beta/models/gemini-3:generateContent"),
            "/v1beta/models/gemini-3:generateContent"
        );
        assert_eq!(provider_relative_path("/api/me"), "/api/me");
    }

    #[test]
    fn known_paths_report_allowed_methods_without_claiming_unknown_paths() {
        let (surface, methods) = known_route_allowed_methods("/v1/responses", false).unwrap();
        assert_eq!(surface, ApiSurface::OpenAi);
        assert_eq!(methods, vec!["POST"]);
        assert!(known_route_allowed_methods("/v1/future-operation", false).is_none());
    }

    #[test]
    fn conversation_routes_are_typed_and_native_backend_only() {
        let cases = [
            (
                "POST",
                "/v1/conversations",
                ApiOperation::ConversationsCreate,
            ),
            (
                "GET",
                "/v1/conversations/conv_1",
                ApiOperation::ConversationsGet,
            ),
            (
                "POST",
                "/v1/conversations/conv_1",
                ApiOperation::ConversationsUpdate,
            ),
            (
                "DELETE",
                "/v1/conversations/conv_1",
                ApiOperation::ConversationsDelete,
            ),
            (
                "POST",
                "/v1/conversations/conv_1/items",
                ApiOperation::ConversationItemsCreate,
            ),
            (
                "GET",
                "/v1/conversations/conv_1/items",
                ApiOperation::ConversationItemsList,
            ),
            (
                "GET",
                "/v1/conversations/conv_1/items/item_1",
                ApiOperation::ConversationItemGet,
            ),
            (
                "DELETE",
                "/v1/conversations/conv_1/items/item_1",
                ApiOperation::ConversationItemDelete,
            ),
        ];
        for (method, path, operation) in cases {
            let route = resolve_api_route(method, path, false).expect("conversation route");
            assert_eq!(route.operation, operation);
            assert!(api_operation_requires_native_api_credential(operation));
        }
    }

    #[test]
    fn current_cache_and_token_count_resources_are_typed_without_false_backend_claims() {
        let input_tokens = resolve_api_route("POST", "/v1/responses/input_tokens", false).unwrap();
        assert_eq!(input_tokens.operation, ApiOperation::ResponsesInputTokens);
        assert!(api_operation_requires_native_api_credential(
            input_tokens.operation
        ));
        assert!(
            resolve_api_route("GET", "/v1/responses/input_tokens", false)
                .is_some_and(|route| route.operation == ApiOperation::Opaque)
        );

        let cases = [
            (
                "POST",
                "/gemini/v1beta/cachedContents",
                ApiOperation::CachedContentsCreate,
            ),
            (
                "GET",
                "/gemini/v1beta/cachedContents",
                ApiOperation::CachedContentsList,
            ),
            (
                "GET",
                "/gemini/v1beta/cachedContents/cache_1",
                ApiOperation::CachedContentsGet,
            ),
            (
                "PATCH",
                "/gemini/v1beta/cachedContents/cache_1",
                ApiOperation::CachedContentsUpdate,
            ),
            (
                "DELETE",
                "/gemini/v1beta/cachedContents/cache_1",
                ApiOperation::CachedContentsDelete,
            ),
        ];
        for (method, path, operation) in cases {
            let route = resolve_api_route(method, path, false).unwrap();
            assert_eq!(route.operation, operation);
            assert!(api_operation_requires_native_api_credential(operation));
        }
    }

    #[test]
    fn declarative_native_routes_are_typed_without_opening_subscription_or_platform_backends() {
        let cases = [
            (
                "DELETE",
                "/v1/models/gpt-custom",
                "openai.delete_model",
                Statefulness::ResourceDelete,
            ),
            (
                "GET",
                "/v1/chat/completions/chatcmpl_1/messages",
                "openai.chat_completion_messages_list",
                Statefulness::ResourceRead,
            ),
            (
                "POST",
                "/v1/moderations",
                "openai.moderations",
                Statefulness::Stateless,
            ),
            (
                "POST",
                "/v1/realtime/calls/call_1/accept",
                "openai.realtime_calls_accept",
                Statefulness::ResourceMutate,
            ),
            (
                "POST",
                "/v1/realtime/translations/client_secrets",
                "openai.realtime_translation_client_secrets_create",
                Statefulness::ResourceCreate,
            ),
            (
                "POST",
                "/gemini/v1beta/models/gemini-embedding-001:batchEmbedContents",
                "gemini.batch_embed_contents",
                Statefulness::Stateless,
            ),
        ];
        for (method, path, operation_id, statefulness) in cases {
            let route = resolve_api_route(method, path, false).expect("native API route");
            assert_eq!(route.operation, ApiOperation::Opaque);
            assert_eq!(route.statefulness, statefulness);
            let spec = ROUTE_SPECS
                .iter()
                .copied()
                .find(|spec| spec.matches(method, path))
                .expect("native API route spec");
            assert_eq!(spec.operation_id(), operation_id);
            assert!(spec.native_api_only);
            assert_eq!(spec.execution_policy(), "native_backend_only");
            assert_eq!(spec.required_context_declarations(), ["api_credential"]);
        }
    }

    #[test]
    fn media_routes_expose_transport_and_backend_boundaries() {
        let cases = [
            (
                "POST",
                "/v1/images/generations",
                ApiOperation::ImagesGenerations,
                "small_buffered",
                "capability_routed",
            ),
            (
                "POST",
                "/v1/images/variations",
                ApiOperation::ImagesVariations,
                "streaming_upload",
                "native_backend_only",
            ),
            (
                "POST",
                "/v1/audio/speech",
                ApiOperation::AudioSpeech,
                "small_buffered",
                "native_backend_only",
            ),
            (
                "POST",
                "/v1/audio/transcriptions",
                ApiOperation::AudioTranscriptions,
                "streaming_upload",
                "native_backend_only",
            ),
            (
                "POST",
                "/v1/videos",
                ApiOperation::VideosCreate,
                "streaming_upload",
                "native_backend_only",
            ),
            (
                "GET",
                "/v1/videos/video_1/content",
                ApiOperation::VideosContent,
                "none",
                "native_backend_only",
            ),
        ];
        for (method, path, operation, body_mode, policy) in cases {
            let route = resolve_api_route(method, path, false).expect("typed media route");
            assert_eq!(route.operation, operation);
            let spec = ROUTE_SPECS
                .iter()
                .find(|spec| spec.operation == operation)
                .expect("media route spec");
            assert_eq!(spec.request_body_mode(), body_mode);
            assert_eq!(spec.execution_policy(), policy);
        }
        assert_eq!(
            resolve_api_route("GET", "/v1/videos/edits", false)
                .expect("opaque reserved collection segment")
                .operation,
            ApiOperation::Opaque
        );

        let provenance = resolve_api_route("POST", "/v1/content_provenance_checks", false)
            .expect("content provenance route");
        assert_eq!(provenance.operation, ApiOperation::ContentProvenanceChecks);
        assert!(api_operation_uses_streaming_upload(provenance.operation));
        assert!(api_operation_requires_native_api_credential(
            provenance.operation
        ));
    }

    #[test]
    fn codex_frameless_live_routes_keep_their_execution_boundaries() {
        let create = resolve_api_route("POST", "/v1/live", false).expect("Live call create");
        assert_eq!(create.operation, ApiOperation::RealtimeLiveCallCreate);
        assert_eq!(create.statefulness, Statefulness::ResourceCreate);
        let create_spec = ROUTE_SPECS
            .iter()
            .copied()
            .find(|spec| spec.operation == ApiOperation::RealtimeLiveCallCreate)
            .expect("Live call create spec");
        assert_eq!(create_spec.execution_policy(), "capability_routed");

        let connect = resolve_api_route("GET", "/v1/live", true).expect("Live standalone route");
        assert_eq!(connect.operation, ApiOperation::RealtimeLiveConnect);
        assert_eq!(connect.transport, TransportKind::Websocket);
        assert_eq!(connect.statefulness, Statefulness::ResourceCreate);
        assert!(api_operation_requires_local_backend(connect.operation));
        let connect_spec = ROUTE_SPECS
            .iter()
            .copied()
            .find(|spec| spec.operation == ApiOperation::RealtimeLiveConnect)
            .expect("Live standalone spec");
        assert_eq!(connect_spec.execution_policy(), "capability_routed");

        let sideband =
            resolve_api_route("GET", "/v1/live/rtc_1", true).expect("Live sideband route");
        assert_eq!(sideband.operation, ApiOperation::RealtimeLiveWebSocket);
        assert_eq!(sideband.transport, TransportKind::Websocket);
        let sideband_spec = ROUTE_SPECS
            .iter()
            .copied()
            .find(|spec| spec.operation == ApiOperation::RealtimeLiveWebSocket)
            .expect("Live sideband spec");
        assert_eq!(sideband_spec.execution_policy(), "local_backend_only");
        assert_eq!(sideband_spec.request_body_mode(), "duplex_frames");
    }

    #[test]
    fn surface_manifest_advertises_all_typed_websocket_surfaces() {
        let manifest = surface_manifest();
        let operations = manifest["surfaces"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|surface| surface["operations"].as_array().unwrap())
            .collect::<Vec<_>>();
        let websocket = operations
            .iter()
            .filter(|operation| operation["transport"] == "websocket")
            .collect::<Vec<_>>();
        assert_eq!(websocket.len(), 6);
        let operation_ids = websocket
            .iter()
            .map(|operation| operation["operation"].as_str().unwrap())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(
            operation_ids,
            std::collections::HashSet::from([
                "responses_websocket",
                "realtime_websocket",
                "realtime_live_connect",
                "realtime_live_websocket",
                "realtime_translation_websocket",
                "live_websocket",
            ])
        );
    }
}

#[cfg(test)]
#[path = "surface_contract_tests.rs"]
mod contract_tests;
