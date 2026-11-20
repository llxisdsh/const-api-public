//! Standalone video jobs and speech synthesis have different contracts from chat output.
use crate::model::ChannelConfig;
use crate::source_driver::{ExecutionKind, SourceDriverId, SubscriptionResponseMode};
use crate::surface::{ApiOperation, ApiSurface};
use std::collections::HashMap;

pub(crate) const VIDEO: &str = "openai.videos_create";
pub(crate) const SPEECH: &str = "openai.audio_speech";
pub(crate) const GEMINI_IMAGE: &str = "gemini.generate_content.image";
pub(crate) const GEMINI_IMAGE_EDIT: &str = "gemini.generate_content.image_edit";
pub(crate) const CHAT_MUSIC: &str = "openai.chat_completions.music";

pub(crate) fn is_native_operation(op: &str) -> bool {
    matches!(op, GEMINI_IMAGE | GEMINI_IMAGE_EDIT | CHAT_MUSIC)
}

/// Some generators are invoked inside their native conversation protocol.
/// Only an exact channel declaration may identify such a route. The public
/// audio modality alone cannot distinguish music, narration and conversation.
pub(crate) fn native_generation(
    channel: &ChannelConfig,
    p: &crate::model::ChannelCapabilityProfile,
) -> Option<&'static str> {
    if !p.catalog_metadata || p.catalog_protocol_supported == Some(false) {
        return None;
    }
    let driver = channel.source_driver();
    if p.protocol == "gemini_native"
        && p.image_output
        && (matches!(
            driver,
            SourceDriverId::GeminiApi | SourceDriverId::GeminiSubscription
        ) || (driver == SourceDriverId::OpencodeConsole
            && p.opencode_image_api == Some(crate::coding_gateway::ConsoleImageApi::Gemini)))
    {
        return crate::channel_surface::channel_protocol_target(
            channel,
            crate::protocol::kind::ProtocolKind::GeminiNative,
        )
        .map(|_| GEMINI_IMAGE);
    }
    // OpenRouter's Lyria entries both declare text+audio and Chat, just like
    // conversational audio models. Only these documented wire IDs establish music.
    if driver == SourceDriverId::Openrouter
        && p.protocol == "openai_chat"
        && p.audio_output
        && p.catalog_protocol_supported == Some(true)
        && matches!(
            p.catalog_source_model.as_deref(),
            Some("google/lyria-3-clip-preview" | "google/lyria-3-pro-preview")
        )
    {
        return crate::channel_surface::channel_protocol_target(
            channel,
            crate::protocol::kind::ProtocolKind::OpenAiChat,
        )
        .map(|_| CHAT_MUSIC);
    }
    None
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(crate) struct MediaOperations {
    pub(crate) video: bool,
    pub(crate) speech: bool,
}

impl MediaOperations {
    pub(crate) fn empty(&self) -> bool {
        !self.video && !self.speech
    }

    pub(crate) fn from_entry(entry: &serde_json::Value) -> Self {
        let explicit = entry.pointer("/const_api/media_operations");
        let Some(paths) = explicit
            .unwrap_or(&entry["supported_endpoints"])
            .as_array()
            .filter(|items| items.iter().all(serde_json::Value::is_string))
        else {
            return Self::default();
        };
        let has = |wanted: &[&str]| {
            paths.iter().filter_map(|value| value.as_str()).any(|s| {
                wanted.contains(
                    &s.trim()
                        .strip_prefix("/v1/")
                        .unwrap_or(s.trim())
                        .trim_start_matches('/'),
                )
            })
        };
        if explicit.is_some() {
            return Self {
                video: has(&[VIDEO]),
                speech: has(&[SPEECH]),
            };
        }
        Self {
            video: has(&["videos", "videos/generations", VIDEO])
                && has(&["videos/{id}", "videos/{video_id}", "openai.videos_get"]),
            speech: has(&["audio/speech", SPEECH]),
        }
    }

    pub(crate) fn union(&mut self, other: Self) {
        self.video |= other.video;
        self.speech |= other.speech;
    }

    pub(crate) fn names(self) -> impl Iterator<Item = &'static str> {
        [self.video.then_some(VIDEO), self.speech.then_some(SPEECH)]
            .into_iter()
            .flatten()
    }
}

pub(crate) struct ChannelMedia {
    driver: SourceDriverId,
    admitted: MediaOperations,
    exact: HashMap<String, MediaOperations>,
}

impl ChannelMedia {
    pub(crate) fn new(channel: &ChannelConfig) -> Self {
        let driver = channel.source_driver();
        let kind = crate::channel_executor::execution_kind_for_source(driver).ok();
        let openai = kind == Some(ExecutionKind::HttpSurface)
            && crate::channel_surface::channel_surface_target(
                channel,
                ApiSurface::OpenAi,
                None,
                true,
            )
            .is_some();
        let admits = |op| match kind {
            Some(ExecutionKind::HttpSurface) => {
                openai && crate::openrouter::supports_operation(driver, op)
            }
            Some(_) => [
                SubscriptionResponseMode::Buffered,
                SubscriptionResponseMode::Sse,
            ]
            .into_iter()
            .any(|mode| {
                crate::source_driver::admit_subscription_operation(
                    driver,
                    ApiSurface::OpenAi,
                    op,
                    None,
                    mode,
                )
                .is_ok()
            }),
            None => false,
        };
        let admitted = MediaOperations {
            video: admits(ApiOperation::VideosCreate) && admits(ApiOperation::VideosGet),
            speech: admits(ApiOperation::AudioSpeech),
        };
        let mut exact = HashMap::<String, MediaOperations>::new();
        for p in &channel.capability_profiles {
            if p.catalog_media_operations.empty()
                || p.model_pattern.is_empty()
                || p.model_pattern.contains(['*', '?'])
                || !matches!(p.verification_state.as_str(), "declared" | "verified")
                || !matches!(p.capability_layer.as_str(), "" | "model_wire")
                || matches!(p.release_status.as_str(), "suspended" | "prepared")
            {
                continue;
            }
            exact
                .entry(p.model_pattern.to_ascii_lowercase())
                .or_default()
                .union(p.catalog_media_operations);
        }
        Self {
            driver,
            admitted,
            exact,
        }
    }

    pub(crate) fn model(&self, wire: &str) -> MediaOperations {
        let wire = wire.to_ascii_lowercase();
        let mut ops = self.exact.get(&wire).copied().unwrap_or_default();
        // The official models list returns IDs without endpoint metadata. These
        // contracts apply only to IDs actually listed on that specific API.
        if self.driver == SourceDriverId::OpenAiApi
            && matches!(
                wire.as_str(),
                "tts-1"
                    | "tts-1-hd"
                    | "tts-1-1106"
                    | "tts-1-hd-1106"
                    | "gpt-4o-mini-tts"
                    | "gpt-4o-mini-tts-2025-12-15"
                    | "gpt-4o-mini-tts-2025-03-20"
            )
        {
            ops.speech = true;
        }
        MediaOperations {
            video: ops.video && self.admitted.video,
            speech: ops.speech && self.admitted.speech,
        }
    }
}
