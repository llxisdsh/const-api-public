use serde::{Deserialize, Serialize};

use super::{
    ArtifactKind, CanonicalError, ContentBlock, ContentBlockKind, FinishDetail, OpaqueArtifact,
};
use super::{ResponseStatus, Usage};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ResponseMeta {
    pub(crate) id: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) status: ResponseStatus,
    pub(crate) provider_event_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct BlockHeader {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tool_kind: Option<super::ToolKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tool_namespace: Option<String>,
    pub(crate) kind: ContentBlockKind,
    pub(crate) source_item_id: Option<String>,
    pub(crate) call_id: Option<String>,
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) artifacts: Vec<OpaqueArtifact>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub(crate) enum CanonicalStreamEvent {
    ResponseStart(ResponseMeta),
    BlockStart {
        index: u32,
        block: BlockHeader,
    },
    TextDelta {
        index: u32,
        text: String,
    },
    ReasoningDelta {
        index: u32,
        text: String,
    },
    ReasoningSummaryDelta {
        index: u32,
        part: u32,
        text: String,
    },
    ArtifactDelta {
        index: u32,
        kind: ArtifactKind,
        data: String,
    },
    ToolArgumentsDelta {
        index: u32,
        data: String,
    },
    RefusalDelta {
        index: u32,
        text: String,
    },
    BlockDone {
        index: u32,
        block: Option<ContentBlock>,
    },
    UsageUpdate(Box<Usage>),
    StatusUpdate(ResponseStatus),
    Error(CanonicalError),
    ResponseDone(FinishDetail),
}
