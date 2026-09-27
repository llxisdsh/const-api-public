use serde_json::Value;

use crate::protocol::ir::{
    ArtifactKind, CanonicalRequestV2, ContentBlock, ReasoningBlock, ReplayPolicy,
};
use crate::protocol::kind::ProtocolKind;

use super::{IssuerIdentity, affinity_permits};

/// Find historical hidden-reasoning blocks that cannot be authenticated for
/// the target protocol and issuer.
///
/// Reasoning depth is a portable request setting. Historical reasoning text is
/// different: providers bind it to opaque continuation data or signatures.
/// Replaying bare text across issuers can be rejected as an invalid signature,
/// and must not be confused with mapping low/medium/high request effort.
pub(super) fn reasoning_history_omissions(
    request: &CanonicalRequestV2,
    source: ProtocolKind,
    target: ProtocolKind,
    source_issuer: &IssuerIdentity,
    target_issuer: &IssuerIdentity,
) -> Vec<String> {
    if source == target && source_issuer == target_issuer {
        return Vec::new();
    }

    let mut omissions = Vec::new();
    for (index, instruction) in request.instructions.iter().enumerate() {
        collect_omissions(
            &instruction.blocks,
            &format!("instructions[{index}]"),
            request,
            target,
            target_issuer,
            &mut omissions,
        );
    }
    for (index, turn) in request.turns.iter().enumerate() {
        collect_omissions(
            &turn.blocks,
            &format!("turns[{index}]"),
            request,
            target,
            target_issuer,
            &mut omissions,
        );
    }
    omissions
}

fn collect_omissions(
    blocks: &[ContentBlock],
    base_path: &str,
    request: &CanonicalRequestV2,
    target: ProtocolKind,
    target_issuer: &IssuerIdentity,
    omissions: &mut Vec<String>,
) {
    for (index, block) in blocks.iter().enumerate() {
        let path = format!("{base_path}.blocks[{index}]");
        match block {
            ContentBlock::Reasoning(reasoning)
                if !reasoning_can_replay(reasoning, request, target, target_issuer) =>
            {
                omissions.push(path)
            }
            ContentBlock::ToolResult(result) => collect_omissions(
                &result.content,
                &path,
                request,
                target,
                target_issuer,
                omissions,
            ),
            _ => {}
        }
    }
}

fn reasoning_can_replay(
    reasoning: &ReasoningBlock,
    request: &CanonicalRequestV2,
    target: ProtocolKind,
    target_issuer: &IssuerIdentity,
) -> bool {
    let required_kind = match target {
        // Chat compatibility APIs have no portable signed reasoning-history
        // envelope. Preserve the visible assistant answer and tool calls only.
        ProtocolKind::OpenAiChat => return false,
        ProtocolKind::OpenAiResponses => ArtifactKind::OpenAiEncryptedReasoning,
        ProtocolKind::AnthropicMessages if reasoning.encrypted => {
            ArtifactKind::AnthropicRedactedThinking
        }
        ProtocolKind::AnthropicMessages => ArtifactKind::AnthropicThinkingSignature,
        ProtocolKind::GeminiNative => ArtifactKind::GeminiThoughtSignature,
    };

    reasoning.artifacts.iter().any(|artifact| {
        artifact.kind == required_kind
            && artifact.replay != ReplayPolicy::Never
            && affinity_permits(&artifact.affinity, target, target_issuer, &request.model)
    })
}

/// Remove the non-standard plaintext reasoning fields from assistant history
/// before forwarding a Chat request to another issuer. Top-level reasoning
/// controls are intentionally untouched.
pub(crate) fn strip_unsigned_chat_reasoning_history(body: &mut Value) -> Vec<String> {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    for (index, message) in messages.iter_mut().enumerate() {
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(message) = message.as_object_mut() else {
            continue;
        };
        for field in ["reasoning_content", "reasoning"] {
            if message.get(field).is_some_and(Value::is_string) {
                message.remove(field);
                removed.push(format!("$.messages[{index}].{field}"));
            }
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::strip_unsigned_chat_reasoning_history;

    #[test]
    fn strips_only_plaintext_assistant_history() {
        let mut body = json!({
            "reasoning_effort": "high",
            "messages": [
                {"role":"user", "content":"question", "reasoning_content":"keep-user-field"},
                {"role":"assistant", "content":"answer", "reasoning_content":"hidden"},
                {"role":"assistant", "content":"answer 2", "reasoning":"hidden alias"},
                {"role":"assistant", "content":"answer 3", "reasoning":{"provider":"keep-structured"}}
            ]
        });

        let removed = strip_unsigned_chat_reasoning_history(&mut body);

        assert_eq!(removed.len(), 2);
        assert_eq!(body["reasoning_effort"], "high");
        assert_eq!(body["messages"][0]["reasoning_content"], "keep-user-field");
        assert!(body["messages"][1].get("reasoning_content").is_none());
        assert!(body["messages"][2].get("reasoning").is_none());
        assert_eq!(
            body["messages"][3]["reasoning"]["provider"],
            "keep-structured"
        );
    }
}
