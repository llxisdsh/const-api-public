use std::collections::{BTreeSet, HashMap, HashSet};

use crate::protocol::ir::{
    ArtifactKind, CanonicalRequestV2, ContentBlock, ToolKind, Turn, TurnRole,
};
use crate::protocol::kind::ProtocolKind;

use super::{
    CompatibleAction, ConversionError, ConversionRepair, RepairClass, compatible_action_allowed,
};

pub(crate) type Repair = ConversionRepair;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TargetRules {
    source_protocol: ProtocolKind,
    target_protocol: ProtocolKind,
    require_json_arguments: bool,
    require_alternating_roles: bool,
    group_parallel_tool_results: bool,
    allowed_actions: BTreeSet<CompatibleAction>,
}

impl TargetRules {
    pub(crate) fn new(source_protocol: ProtocolKind, target_protocol: ProtocolKind) -> Self {
        Self {
            source_protocol,
            target_protocol,
            require_json_arguments: true,
            require_alternating_roles: false,
            group_parallel_tool_results: false,
            allowed_actions: BTreeSet::new(),
        }
    }

    pub(crate) fn require_alternating_roles(mut self) -> Self {
        self.require_alternating_roles = true;
        self
    }

    pub(crate) fn group_parallel_tool_results(mut self) -> Self {
        self.group_parallel_tool_results = true;
        self
    }

    pub(crate) fn allow(&mut self, action: CompatibleAction) {
        if compatible_action_allowed(action) {
            self.allowed_actions.insert(action);
        }
    }

    fn allows(&self, action: CompatibleAction) -> bool {
        self.allowed_actions.contains(&action)
    }
}

pub(crate) fn validate_and_normalize_transcript(
    request: &mut CanonicalRequestV2,
    rules: &TargetRules,
) -> Result<Vec<Repair>, ConversionError> {
    if !needs_normalization(&request.turns, rules) {
        validate_tool_ledger(&request.turns, rules)?;
        return Ok(Vec::new());
    }
    let mut candidate = request.turns.clone();
    let mut repairs = normalize_hosted_tool_results(&mut candidate);
    repairs.extend(normalize_turn_envelopes(&mut candidate, rules)?);
    normalize_complete_arguments(&mut candidate, rules, &mut repairs);
    validate_tool_ledger(&candidate, rules)?;
    request.turns = candidate;
    Ok(repairs)
}

fn needs_normalization(turns: &[Turn], rules: &TargetRules) -> bool {
    let envelope_repair = turns.windows(2).any(|pair| {
        let same_role = pair[0].role == pair[1].role;
        let grouped_results = rules.group_parallel_tool_results
            && pair[0].role == TurnRole::User
            && only_tool_results(&pair[0])
            && only_tool_results(&pair[1]);
        same_role && (rules.require_alternating_roles || grouped_results)
    });
    let hosted_calls = hosted_tool_call_ids(turns);
    let hosted_result_repair = turns.iter().any(|turn| {
        turn.role != TurnRole::User
            && turn.blocks.iter().any(|block| {
                matches!(block, ContentBlock::ToolResult(result) if hosted_calls.contains(&result.call_id))
            })
    });
    envelope_repair
        || hosted_result_repair
        || (rules.require_json_arguments
            && turns.iter().flat_map(|turn| &turn.blocks).any(|block| {
                matches!(block, ContentBlock::ToolCall(call) if call.arguments.is_none()
                    && call.raw_arguments.as_deref().is_some_and(|raw| serde_json::from_str::<serde_json::Value>(raw).is_ok()))
            }))
}

fn hosted_tool_call_ids(turns: &[Turn]) -> HashSet<String> {
    turns
        .iter()
        .flat_map(|turn| turn.blocks.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolCall(call) if call.kind == ToolKind::Hosted => Some(call.id.clone()),
            _ => None,
        })
        .collect()
}

fn normalize_hosted_tool_results(turns: &mut Vec<Turn>) -> Vec<Repair> {
    let hosted_calls = hosted_tool_call_ids(turns);
    if hosted_calls.is_empty() {
        return Vec::new();
    }

    let mut normalized = Vec::with_capacity(turns.len());
    let mut repairs = Vec::new();
    for (turn_index, turn) in std::mem::take(turns).into_iter().enumerate() {
        let mut segment_role = turn.role;
        let mut segment_blocks = Vec::new();
        let mut segment_index = 0usize;
        for (block_index, block) in turn.blocks.into_iter().enumerate() {
            let block_role = match &block {
                ContentBlock::ToolResult(result) if hosted_calls.contains(&result.call_id) => {
                    if turn.role != TurnRole::User {
                        repairs.push(ConversionRepair {
                            code: "hosted_tool_result_relocated".to_string(),
                            class: RepairClass::LosslessStructural,
                            path: format!("turns[{turn_index}].blocks[{block_index}]"),
                            action: "split_hosted_tool_result_into_user_turn".to_string(),
                            reason: "target tool transcripts represent completed tool results in a user semantic envelope".to_string(),
                        });
                    }
                    TurnRole::User
                }
                _ => turn.role,
            };
            if !segment_blocks.is_empty() && block_role != segment_role {
                normalized.push(Turn {
                    id: (segment_index == 0).then(|| turn.id.clone()).flatten(),
                    role: segment_role,
                    blocks: std::mem::take(&mut segment_blocks),
                    status: turn.status,
                });
                segment_index += 1;
            }
            segment_role = block_role;
            segment_blocks.push(block);
        }
        if !segment_blocks.is_empty() {
            normalized.push(Turn {
                id: (segment_index == 0).then_some(turn.id).flatten(),
                role: segment_role,
                blocks: segment_blocks,
                status: turn.status,
            });
        }
    }
    *turns = normalized;
    repairs
}

fn normalize_turn_envelopes(
    turns: &mut Vec<Turn>,
    rules: &TargetRules,
) -> Result<Vec<Repair>, ConversionError> {
    let mut repairs = Vec::new();
    let mut index = 1;
    while index < turns.len() {
        let same_role = turns[index - 1].role == turns[index].role;
        let grouped_results = rules.group_parallel_tool_results
            && turns[index - 1].role == TurnRole::User
            && only_tool_results(&turns[index - 1])
            && only_tool_results(&turns[index]);
        if same_role && (rules.require_alternating_roles || grouped_results) {
            let action = CompatibleAction::CoalesceRoleEnvelope;
            if !rules.allows(action) {
                return Err(error(
                    rules,
                    "target_role_sequence",
                    format!("turns[{index}]"),
                    "Target protocol requires alternating roles; merging adjacent messages is not authorized.",
                ));
            }
            let mut next = turns.remove(index);
            turns[index - 1].blocks.append(&mut next.blocks);
            if turns[index - 1].status.is_none() {
                turns[index - 1].status = next.status;
            }
            repairs.push(ConversionRepair {
                code: "adjacent_roles_coalesced".to_string(),
                class: RepairClass::CompatibleAllowlisted,
                path: format!("turns[{}..={index}]", index - 1),
                action: "coalesce_role_envelope".to_string(),
                reason: "target requires one envelope for adjacent same-role content".to_string(),
            });
            continue;
        }
        index += 1;
    }
    Ok(repairs)
}

fn normalize_complete_arguments(
    turns: &mut [Turn],
    rules: &TargetRules,
    repairs: &mut Vec<Repair>,
) {
    if !rules.require_json_arguments {
        return;
    }
    for (turn_index, turn) in turns.iter_mut().enumerate() {
        for (block_index, block) in turn.blocks.iter_mut().enumerate() {
            let ContentBlock::ToolCall(call) = block else {
                continue;
            };
            if call.arguments.is_some() {
                continue;
            }
            let Some(raw) = call.raw_arguments.as_deref() else {
                continue;
            };
            let Ok(arguments) = serde_json::from_str::<serde_json::Value>(raw) else {
                continue;
            };
            call.arguments = Some(arguments);
            repairs.push(ConversionRepair {
                code: "tool_arguments_parsed".to_string(),
                class: RepairClass::LosslessStructural,
                path: format!("turns[{turn_index}].blocks[{block_index}].arguments"),
                action: "parse_complete_json_arguments".to_string(),
                reason: "target requires structured JSON arguments".to_string(),
            });
        }
    }
}

fn validate_tool_ledger(turns: &[Turn], rules: &TargetRules) -> Result<(), ConversionError> {
    let mut all_calls = HashSet::new();
    let mut hosted_calls = HashSet::new();
    let mut pending = HashMap::<String, (usize, usize)>::new();
    let mut pending_order = Vec::new();

    for (turn_index, turn) in turns.iter().enumerate() {
        let mut has_unrelated_user_content = false;
        for (block_index, block) in turn.blocks.iter().enumerate() {
            let path = format!("turns[{turn_index}].blocks[{block_index}]");
            match block {
                ContentBlock::ToolCall(call) => {
                    if call.name.trim().is_empty() {
                        return Err(error(
                            rules,
                            "tool_name_missing",
                            path,
                            "Tool call name is missing.",
                        ));
                    }
                    if call.id.trim().is_empty() {
                        return Err(error(
                            rules,
                            "invalid_tool_call_id",
                            path,
                            "Tool call ID is missing.",
                        ));
                    }
                    if !all_calls.insert(call.id.clone()) {
                        return Err(error(
                            rules,
                            "duplicate_tool_call_id",
                            path,
                            "Duplicate tool call ID within one response.",
                        ));
                    }
                    // Freeform tool input is text, not incomplete function JSON.
                    // Protocol capability planning decides whether it can be carried.
                    if rules.require_json_arguments
                        && call.kind != ToolKind::Custom
                        && call.arguments.is_none()
                    {
                        let valid = call.raw_arguments.as_deref().is_some_and(|raw| {
                            serde_json::from_str::<serde_json::Value>(raw).is_ok()
                        });
                        if !valid {
                            return Err(error(
                                rules,
                                "tool_arguments_incomplete",
                                path.clone(),
                                "Tool arguments are not complete JSON.",
                            ));
                        }
                    }
                    for artifact in &call.artifacts {
                        if matches!(
                            artifact.kind,
                            ArtifactKind::GeminiThoughtSignature
                                | ArtifactKind::AnthropicThinkingSignature
                        ) {
                            if let Some(attached_call) = artifact
                                .payload
                                .get("call_id")
                                .and_then(|value| value.as_str())
                            {
                                if attached_call != call.id {
                                    return Err(error(
                                        rules,
                                        "signature_wrong_call",
                                        path.clone(),
                                        "Reasoning signature is attached to the wrong tool call.",
                                    ));
                                }
                            }
                        }
                    }
                    if call.kind == ToolKind::Hosted {
                        // Provider-hosted tools are executed and completed by the provider. They
                        // are part of assistant output and never wait for a user tool-result turn.
                        hosted_calls.insert(call.id.clone());
                    } else {
                        pending.insert(call.id.clone(), (turn_index, block_index));
                        pending_order.push(call.id.clone());
                    }
                }
                ContentBlock::ToolResult(result) => {
                    if hosted_calls.contains(&result.call_id) {
                        continue;
                    }
                    if turn.role != TurnRole::User {
                        return Err(error(
                            rules,
                            "tool_result_wrong_role",
                            path,
                            "Tool results must be in a user turn.",
                        ));
                    }
                    if pending.remove(&result.call_id).is_none() {
                        return Err(error(
                            rules,
                            "tool_result_orphaned",
                            path,
                            "Tool result has no matching pending call.",
                        ));
                    }
                }
                _ if turn.role == TurnRole::User => has_unrelated_user_content = true,
                _ => {}
            }
        }
        if has_unrelated_user_content && !pending.is_empty() {
            let Some((call_id, call_turn, call_block)) = first_pending(&pending, &pending_order)
            else {
                return Err(error(
                    rules,
                    "tool_call_state_invalid",
                    format!("turns[{turn_index}]"),
                    "Incomplete tool call state; conversion cannot safely continue.",
                ));
            };
            return Err(error(
                rules,
                "tool_call_unanswered",
                format!("turns[{call_turn}].blocks[{call_block}]"),
                format!("Tool call {call_id} has no result before new user content."),
            ));
        }
    }
    if let Some((call_id, turn, block)) = first_pending(&pending, &pending_order) {
        return Err(error(
            rules,
            "tool_call_unanswered",
            format!("turns[{turn}].blocks[{block}]"),
            format!("Tool call {call_id} has no result."),
        ));
    }
    Ok(())
}

fn first_pending<'a>(
    pending: &'a HashMap<String, (usize, usize)>,
    order: &'a [String],
) -> Option<(&'a str, usize, usize)> {
    order.iter().find_map(|call_id| {
        pending
            .get(call_id)
            .map(|(turn, block)| (call_id.as_str(), *turn, *block))
    })
}

fn only_tool_results(turn: &Turn) -> bool {
    !turn.blocks.is_empty()
        && turn
            .blocks
            .iter()
            .all(|block| matches!(block, ContentBlock::ToolResult(_)))
}

fn error(
    rules: &TargetRules,
    code: &'static str,
    path: impl Into<String>,
    summary: impl Into<String>,
) -> ConversionError {
    ConversionError::blocked_issue(
        code,
        rules.source_protocol,
        rules.target_protocol,
        path,
        summary,
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::protocol::conversion::{CompatibleAction, RepairClass};
    use crate::protocol::ir::{
        ArtifactAffinity, ArtifactCriticality, ArtifactKind, BlockMetadata, CanonicalRequestV2,
        ContentBlock, ItemStatus, OpaqueArtifact, ReplayPolicy, TextBlock, ToolCall, ToolResult,
        Turn, TurnRole,
    };
    use crate::protocol::kind::ProtocolKind;

    fn call(id: &str, name: &str) -> ContentBlock {
        ContentBlock::ToolCall(ToolCall::function(id, name, json!({"path":"a.txt"})))
    }

    fn result(id: &str) -> ContentBlock {
        ContentBlock::ToolResult(ToolResult {
            call_id: id.to_string(),
            name: Some("read_file".to_string()),
            content: vec![ContentBlock::Text(TextBlock::new("ok"))],
            status: ItemStatus::Completed,
            is_error: false,
            metadata: BlockMetadata::default(),
        })
    }

    fn parallel_transcript() -> CanonicalRequestV2 {
        let mut request = CanonicalRequestV2::new("model");
        request.turns = vec![
            Turn {
                id: Some("assistant".to_string()),
                role: TurnRole::Assistant,
                blocks: vec![call("call-a", "read_file"), call("call-b", "read_file")],
                status: Some(ItemStatus::Completed),
            },
            Turn {
                id: Some("results".to_string()),
                role: TurnRole::User,
                blocks: vec![result("call-a"), result("call-b")],
                status: Some(ItemStatus::Completed),
            },
        ];
        request
    }

    fn rules() -> TargetRules {
        TargetRules::new(ProtocolKind::OpenAiChat, ProtocolKind::AnthropicMessages)
    }

    #[test]
    fn valid_parallel_transcript_uses_original_turn_allocation() {
        let mut request = parallel_transcript();
        let pointer = request.turns.as_ptr();
        let before = request.clone();
        let repairs = validate_and_normalize_transcript(&mut request, &rules()).unwrap();
        assert!(repairs.is_empty());
        assert_eq!(request, before);
        assert_eq!(request.turns.as_ptr(), pointer);
    }

    #[test]
    fn adversarial_transcripts_fail_with_stable_codes() {
        let mut duplicate = parallel_transcript();
        duplicate.turns[0].blocks.push(call("call-a", "other"));
        assert_eq!(
            validate_and_normalize_transcript(&mut duplicate, &rules())
                .unwrap_err()
                .code(),
            "duplicate_tool_call_id"
        );

        let mut orphan = CanonicalRequestV2::new("model");
        orphan.turns.push(Turn {
            id: None,
            role: TurnRole::User,
            blocks: vec![result("missing")],
            status: None,
        });
        assert_eq!(
            validate_and_normalize_transcript(&mut orphan, &rules())
                .unwrap_err()
                .code(),
            "tool_result_orphaned"
        );

        let mut unanswered = CanonicalRequestV2::new("model");
        unanswered.turns = vec![
            Turn {
                id: None,
                role: TurnRole::Assistant,
                blocks: vec![call("pending", "lookup")],
                status: None,
            },
            Turn {
                id: None,
                role: TurnRole::User,
                blocks: vec![ContentBlock::Text(TextBlock::new("new question"))],
                status: None,
            },
        ];
        assert_eq!(
            validate_and_normalize_transcript(&mut unanswered, &rules())
                .unwrap_err()
                .code(),
            "tool_call_unanswered"
        );

        let mut missing_name = parallel_transcript();
        if let ContentBlock::ToolCall(call) = &mut missing_name.turns[0].blocks[0] {
            call.name.clear();
        }
        assert_eq!(
            validate_and_normalize_transcript(&mut missing_name, &rules())
                .unwrap_err()
                .code(),
            "tool_name_missing"
        );

        let mut truncated = parallel_transcript();
        if let ContentBlock::ToolCall(call) = &mut truncated.turns[0].blocks[0] {
            call.arguments = None;
            call.raw_arguments = Some("{\"path\":\"a.txt\"".to_string());
        }
        assert_eq!(
            validate_and_normalize_transcript(&mut truncated, &rules())
                .unwrap_err()
                .code(),
            "tool_arguments_incomplete"
        );

        let mut wrong_signature = parallel_transcript();
        if let ContentBlock::ToolCall(call) = &mut wrong_signature.turns[0].blocks[0] {
            call.artifacts.push(OpaqueArtifact {
                kind: ArtifactKind::GeminiThoughtSignature,
                payload: json!({"call_id":"call-b","signature":"opaque"}),
                affinity: ArtifactAffinity::Provider {
                    provider: "google".to_string(),
                },
                replay: ReplayPolicy::Required,
                criticality: ArtifactCriticality::Required,
            });
        }
        assert_eq!(
            validate_and_normalize_transcript(&mut wrong_signature, &rules())
                .unwrap_err()
                .code(),
            "signature_wrong_call"
        );
    }

    #[test]
    fn adjacent_roles_are_repaired_only_when_rule_is_allowlisted() {
        let mut request = CanonicalRequestV2::new("model");
        request.turns = vec![
            Turn {
                id: None,
                role: TurnRole::User,
                blocks: vec![ContentBlock::Text(TextBlock::new("first"))],
                status: None,
            },
            Turn {
                id: None,
                role: TurnRole::User,
                blocks: vec![ContentBlock::Text(TextBlock::new("second"))],
                status: None,
            },
        ];
        let error = validate_and_normalize_transcript(
            &mut request.clone(),
            &rules().require_alternating_roles(),
        )
        .unwrap_err();
        assert_eq!(error.code(), "target_role_sequence");

        let mut allowed = rules().require_alternating_roles();
        allowed.allow(CompatibleAction::CoalesceRoleEnvelope);
        let repairs = validate_and_normalize_transcript(&mut request, &allowed).unwrap();
        assert_eq!(request.turns.len(), 1);
        assert_eq!(request.turns[0].blocks.len(), 2);
        assert_eq!(repairs.len(), 1);
        assert_eq!(repairs[0].code, "adjacent_roles_coalesced");
        assert_eq!(repairs[0].class, RepairClass::CompatibleAllowlisted);
    }

    #[test]
    fn complete_raw_arguments_are_parsed_once_as_a_lossless_repair() {
        let mut request = parallel_transcript();
        if let ContentBlock::ToolCall(call) = &mut request.turns[0].blocks[0] {
            call.arguments = None;
            call.raw_arguments = Some("{\"path\":\"a.txt\"}".to_string());
        }
        let repairs = validate_and_normalize_transcript(&mut request, &rules()).unwrap();
        assert_eq!(repairs.len(), 1);
        assert_eq!(repairs[0].code, "tool_arguments_parsed");
        assert_eq!(repairs[0].class, RepairClass::LosslessStructural);
        let ContentBlock::ToolCall(call) = &request.turns[0].blocks[0] else {
            panic!("tool call");
        };
        assert_eq!(call.arguments, Some(json!({"path":"a.txt"})));
    }
}
