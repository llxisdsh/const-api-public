mod artifact_policy;
mod namespace;
pub(crate) mod planner;
mod reasoning_history;
mod report;
mod schema;
mod tool_bridge;
mod transcript;

pub(crate) use artifact_policy::affinity_permits;
use artifact_policy::*;
pub(crate) use namespace::*;
pub(crate) use planner::*;
pub(crate) use reasoning_history::*;
pub(crate) use report::*;
pub(crate) use schema::*;
pub(crate) use tool_bridge::*;
pub(crate) use transcript::*;

// Identically named fields do not imply the same capacity/pricing contract.
// Keep OpenAI's two wire formats compatible; other protocols use their own
// controls (e.g. Anthropic speed vs service_tier capacity commitments).
pub(crate) fn service_tier_is_portable(
    source: crate::protocol::kind::ProtocolKind,
    target: crate::protocol::kind::ProtocolKind,
) -> bool {
    use crate::protocol::kind::ProtocolKind::{OpenAiChat, OpenAiResponses};
    source == target
        || (matches!(source, OpenAiChat | OpenAiResponses)
            && matches!(target, OpenAiChat | OpenAiResponses))
}
