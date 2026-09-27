use serde::{Deserialize, Serialize};
use serde_json::Value;
#[cfg(test)]
use serde_json::json;

use super::IrError;
#[cfg(test)]
use super::redacted_blocks;
use super::{ContentBlock, ProviderExtension, Usage, validate_blocks};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ResponseStatus {
    InProgress,
    Completed,
    Incomplete,
    Failed,
    Cancelled,
    Refused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    PauseTurn,
    ContentFilter,
    Refusal,
    Error,
    Cancelled,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct FinishDetail {
    pub(crate) reason: FinishReason,
    pub(crate) original_reason: Option<String>,
    pub(crate) incomplete_details: Option<Value>,
}

impl FinishDetail {
    #[cfg(test)]
    pub(crate) fn stop() -> Self {
        Self {
            reason: FinishReason::Stop,
            original_reason: None,
            incomplete_details: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct CanonicalError {
    pub(crate) code: String,
    pub(crate) message: String,
    pub(crate) retryable: bool,
    pub(crate) provider_code: Option<String>,
    pub(crate) details: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct CanonicalResponseV2 {
    pub(crate) id: Option<String>,
    pub(crate) model: Option<String>,
    #[serde(default)]
    pub(crate) blocks: Vec<ContentBlock>,
    pub(crate) status: ResponseStatus,
    pub(crate) finish: FinishDetail,
    pub(crate) usage: Usage,
    pub(crate) error: Option<CanonicalError>,
    #[serde(default)]
    pub(crate) extensions: Vec<ProviderExtension>,
}

impl CanonicalResponseV2 {
    pub(crate) fn validate(&self) -> Result<(), IrError> {
        validate_blocks(&self.blocks, "response")
    }
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub(crate) struct RedactedResponseView(Value);

#[cfg(test)]
impl From<&CanonicalResponseV2> for RedactedResponseView {
    fn from(response: &CanonicalResponseV2) -> Self {
        Self(json!({
            "model": response.model,
            "blocks": redacted_blocks(&response.blocks),
            "status": response.status,
            "finish": response.finish,
            "usage": response.usage,
            "error": response.error.as_ref().map(|error| json!({
                "code": error.code,
                "retryable": error.retryable,
                "provider_code": error.provider_code,
                "message_chars": error.message.chars().count(),
                "details_redacted": error.details.is_some(),
            })),
            "extensions": response.extensions.iter().map(|extension| json!({
                "namespace": extension.namespace,
                "name": extension.name,
                "affinity": extension.affinity,
                "criticality": extension.criticality,
                "redacted": true,
            })).collect::<Vec<_>>(),
        }))
    }
}
