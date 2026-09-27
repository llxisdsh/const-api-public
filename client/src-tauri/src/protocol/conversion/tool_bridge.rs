//! Request-scoped tool transport adaptation. No global/session registry and no
//! guessing from a name prefix on responses: only this request's mappings apply.
use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::protocol::adapters::AdapterError;
use crate::protocol::ir::{
    CanonicalRequestV2, CanonicalResponseV2, CanonicalStreamEvent, ContentBlock, CustomTool,
    FunctionTool, NamespaceToolDefinition, ToolCall, ToolChoice, ToolDefinition, ToolKind,
};
use crate::protocol::stream::StreamError;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolIdentity {
    namespace: Option<String>,
    name: String,
    custom: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ToolWireMap {
    entries: HashMap<String, ToolIdentity>,
    names: HashMap<(String, String), String>,
    short_names: HashMap<String, Option<String>>,
}

impl ToolWireMap {
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn lookup(&self, namespace: Option<&str>, name: &str) -> Option<&str> {
        if let Some(wire) = self
            .names
            .get(&(namespace.unwrap_or_default().to_string(), name.to_string()))
        {
            return Some(wire);
        }
        if namespace.is_some() {
            return None;
        }
        if let Some((namespace, name)) = name.split_once('.') {
            return self
                .names
                .get(&(namespace.to_string(), name.to_string()))
                .map(String::as_str);
        }
        self.short_names.get(name).and_then(|name| name.as_deref())
    }

    fn insert(&mut self, wire: String, identity: ToolIdentity) {
        self.names.insert(
            (
                identity.namespace.clone().unwrap_or_default(),
                identity.name.clone(),
            ),
            wire.clone(),
        );
        self.short_names
            .entry(identity.name.clone())
            .and_modify(|entry| {
                if entry.as_deref() != Some(&wire) {
                    *entry = None;
                }
            })
            .or_insert_with(|| Some(wire.clone()));
        self.entries.insert(wire, identity);
    }

    pub(crate) fn restore_response(
        &self,
        response: &mut CanonicalResponseV2,
    ) -> Result<(), AdapterError> {
        for block in &mut response.blocks {
            if let ContentBlock::ToolCall(call) = block {
                self.restore_call(call)?;
            }
        }
        Ok(())
    }

    fn restore_call(&self, call: &mut ToolCall) -> Result<(), AdapterError> {
        let Some(identity) = self.entries.get(&call.name) else {
            return Ok(());
        };
        if identity.custom {
            let raw = call
                .raw_arguments
                .clone()
                .or_else(|| call.arguments.as_ref().map(Value::to_string))
                .unwrap_or_default();
            call.raw_arguments = Some(unwrap_input(&raw)?);
            call.arguments = None;
            call.kind = ToolKind::Custom;
        }
        call.name = identity.name.clone();
        call.metadata.tool_namespace = identity.namespace.clone();
        Ok(())
    }

    pub(crate) fn stream_mapper(self) -> ToolStreamMapper {
        ToolStreamMapper {
            mapping: self,
            pending: HashMap::new(),
        }
    }
}

fn unwrap_input(raw: &str) -> Result<String, AdapterError> {
    serde_json::from_str::<Value>(raw).ok()
        .and_then(|value| value.get("input").and_then(Value::as_str).map(str::to_string))
        .ok_or_else(|| AdapterError::new("custom_tool_input_invalid", "$.tools.input", "converted custom tool must return an object with a string input; no tool execution was emitted"))
}

fn custom_function(tool: &CustomTool) -> FunctionTool {
    let mut description = tool.description.clone().unwrap_or_default();
    description.push_str("\nTransport adaptation: call this function with a JSON object containing exactly one string property, input. Put the complete original free-text tool input in that string without adding any transport wrapper to its contents. Any instruction above saying not to wrap in JSON applies to the contents of input, not this transport envelope.");
    if !tool.grammar.is_null() {
        description.push_str("\nThe input string must follow this original format specification: ");
        description.push_str(&tool.grammar.to_string());
    }
    FunctionTool {
        name: tool.name.clone(),
        description: Some(description),
        input_schema: json!({"type":"object","properties":{"input":{"type":"string"}},"required":["input"],"additionalProperties":false}),
        strict: None,
        cache_policy: None,
        defer_loading: None,
        allowed_callers: Vec::new(),
        input_examples: Vec::new(),
        eager_input_streaming: None,
    }
}

fn namespace_wire_name(
    namespace: &str,
    name: &str,
    used: &mut HashSet<String>,
) -> Result<String, AdapterError> {
    let mut wire = format!("ns{}_{}_{}", namespace.len(), namespace, name);
    if wire.len() > 64 || used.contains(&wire) {
        let digest = Sha256::digest(format!("{namespace}\0{name}").as_bytes());
        wire = format!("const_tool_{}", hex::encode(&digest[..24]));
    }
    if !used.insert(wire.clone()) {
        return Err(AdapterError::new(
            "tool_definition_conflict",
            "$.tools",
            "tool transport names collide",
        ));
    }
    Ok(wire)
}

/// Promotes custom input to a string parameter, flattens namespaces and rewrites
/// history/choice consistently. Grammar remains format guidance, not a claim of
/// constrained sampling on a protocol that cannot provide it.
pub(crate) fn bridge_request_tools(
    request: &mut CanonicalRequestV2,
) -> Result<ToolWireMap, AdapterError> {
    if !request.tools.iter().any(|tool| matches!(tool, ToolDefinition::Custom(_) | ToolDefinition::Namespace(_)))
        && !request.turns.iter().flat_map(|turn| &turn.blocks).any(|block| matches!(block,
            ContentBlock::ToolCall(call) if call.kind == ToolKind::Custom || call.metadata.tool_namespace.is_some()))
    {
        return Ok(ToolWireMap::default());
    }
    let mut mapping = ToolWireMap::default();
    let mut used: HashSet<String> = request
        .tools
        .iter()
        .filter_map(|tool| match tool {
            ToolDefinition::Function(tool) => Some(tool.name.clone()),
            ToolDefinition::Custom(tool) => Some(tool.name.clone()),
            _ => None,
        })
        .collect();
    let mut tools = Vec::with_capacity(request.tools.len());
    for definition in &request.tools {
        match definition {
            ToolDefinition::Custom(tool) => {
                mapping.insert(
                    tool.name.clone(),
                    ToolIdentity {
                        namespace: None,
                        name: tool.name.clone(),
                        custom: true,
                    },
                );
                tools.push(ToolDefinition::Function(custom_function(tool)));
            }
            ToolDefinition::Namespace(namespace) => {
                for child in &namespace.tools {
                    let (mut tool, custom) = match child {
                        NamespaceToolDefinition::Function(tool) => (tool.clone(), false),
                        NamespaceToolDefinition::Custom(tool) => (custom_function(tool), true),
                    };
                    let wire = namespace_wire_name(&namespace.namespace, child.name(), &mut used)?;
                    mapping.insert(
                        wire.clone(),
                        ToolIdentity {
                            namespace: Some(namespace.namespace.clone()),
                            name: child.name().to_string(),
                            custom,
                        },
                    );
                    tool.name = wire;
                    tools.push(ToolDefinition::Function(tool));
                }
            }
            other => {
                if let ToolDefinition::Function(tool) = other {
                    mapping
                        .names
                        .insert((String::new(), tool.name.clone()), tool.name.clone());
                }
                tools.push(other.clone());
            }
        }
    }
    let mut call_names = HashMap::new();
    for turn in &mut request.turns {
        for block in &mut turn.blocks {
            let ContentBlock::ToolCall(call) = block else {
                continue;
            };
            let namespace = call.metadata.tool_namespace.as_deref();
            let wire = mapping.lookup(namespace, &call.name).map(str::to_string);
            if let Some(wire) = wire {
                call.name = wire;
                call.metadata.tool_namespace = None;
            } else if let Some(namespace) = namespace {
                // A completed tool may have been removed from the active list.
                // Keep its history portable without making it callable again.
                let wire = namespace_wire_name(namespace, &call.name, &mut used)?;
                mapping.insert(
                    wire.clone(),
                    ToolIdentity {
                        namespace: Some(namespace.to_string()),
                        name: call.name.clone(),
                        custom: call.kind == ToolKind::Custom,
                    },
                );
                call.name = wire;
                call.metadata.tool_namespace = None;
            }
            if call.kind == ToolKind::Custom {
                // Historical custom calls are also portable without a current
                // declaration; preserve raw strings even when they look like JSON.
                let input = call
                    .raw_arguments
                    .clone()
                    .or_else(|| call.arguments.as_ref().map(Value::to_string))
                    .unwrap_or_default();
                call.arguments = Some(json!({"input":input}));
                call.raw_arguments = None;
                call.kind = ToolKind::Function;
            }
            call_names.insert(call.id.clone(), call.name.clone());
        }
    }
    for turn in &mut request.turns {
        for block in &mut turn.blocks {
            if let ContentBlock::ToolResult(result) = block {
                if let Some(name) = call_names.get(&result.call_id) {
                    result.name = Some(name.clone());
                }
            }
        }
    }
    match &mut request.tool_choice {
        ToolChoice::Named { name } => {
            if let Some(wire) = mapping.lookup(None, name) {
                *name = wire.to_string();
            }
        }
        ToolChoice::Allowed { names, .. } => {
            let mut expanded = Vec::new();
            for name in names.iter() {
                if let Some(wire) = mapping.lookup(None, name) {
                    expanded.push(wire.to_string());
                } else {
                    let mut members = mapping
                        .entries
                        .iter()
                        .filter(|(_, item)| item.namespace.as_deref() == Some(name.as_str()))
                        .map(|(wire, _)| wire.clone())
                        .collect::<Vec<_>>();
                    members.sort();
                    if members.is_empty() {
                        expanded.push(name.clone());
                    } else {
                        expanded.extend(members);
                    }
                }
            }
            *names = expanded;
        }
        _ => {}
    }
    request.tools = tools;
    Ok(mapping)
}

pub(crate) struct ToolStreamMapper {
    mapping: ToolWireMap,
    pending: HashMap<u32, String>,
}

impl ToolStreamMapper {
    pub(crate) fn map(
        &mut self,
        events: &[CanonicalStreamEvent],
    ) -> Result<Vec<CanonicalStreamEvent>, StreamError> {
        let mut out = Vec::new();
        for event in events {
            match event {
                CanonicalStreamEvent::BlockStart { index, block } => {
                    let mut block = block.clone();
                    if let Some(identity) = block
                        .name
                        .as_ref()
                        .and_then(|name| self.mapping.entries.get(name))
                    {
                        block.name = Some(identity.name.clone());
                        block.tool_namespace = identity.namespace.clone();
                        if identity.custom {
                            block.tool_kind = Some(ToolKind::Custom);
                            self.pending.insert(*index, String::new());
                        }
                    }
                    out.push(CanonicalStreamEvent::BlockStart {
                        index: *index,
                        block,
                    });
                }
                CanonicalStreamEvent::ToolArgumentsDelta { index, data }
                    if self.pending.contains_key(index) =>
                {
                    let Some(buffer) = self.pending.get_mut(index) else {
                        continue;
                    };
                    if buffer.len().saturating_add(data.len())
                        > crate::protocol::stream::MAX_STREAM_BYTES
                    {
                        return Err(StreamError::new(
                            "custom_tool_input_too_large",
                            "converted tool input exceeded the stream limit",
                        ));
                    }
                    buffer.push_str(data);
                }
                CanonicalStreamEvent::BlockDone { index, .. } => {
                    self.flush(*index, &mut out)?;
                    out.push(event.clone());
                }
                CanonicalStreamEvent::ResponseDone(_) => {
                    let mut indices: Vec<_> = self.pending.keys().copied().collect();
                    indices.sort_unstable();
                    for index in indices {
                        self.flush(index, &mut out)?;
                    }
                    out.push(event.clone());
                }
                _ => out.push(event.clone()),
            }
        }
        Ok(out)
    }

    fn flush(
        &mut self,
        index: u32,
        out: &mut Vec<CanonicalStreamEvent>,
    ) -> Result<(), StreamError> {
        if let Some(raw) = self.pending.remove(&index) {
            let data = unwrap_input(&raw)
                .map_err(|error| StreamError::new(error.code(), error.to_string()))?;
            out.push(CanonicalStreamEvent::ToolArgumentsDelta { index, data });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::adapters::AdapterContext;
    use crate::protocol::ir::{AllowedMode, BlockHeader, ContentBlockKind};
    use crate::protocol::kind::ProtocolKind;

    fn request(body: Value) -> CanonicalRequestV2 {
        crate::protocol::decode_request(
            ProtocolKind::OpenAiResponses,
            &body,
            &AdapterContext::legacy_bridge("test-model"),
        )
        .unwrap()
    }

    #[test]
    fn namespace_names_choices_and_collisions_are_request_scoped() {
        let body = json!({"model":"test-model", "input":[], "tools":[
            {"type":"function","name":"ns1_a_run","parameters":{"type":"object"}},
            {"type":"namespace","name":"a","tools":[{"type":"custom","name":"run"}]},
            {"type":"namespace","name":"b","tools":[{"type":"function","name":"run","parameters":{"type":"object"}}]},
            {"type":"namespace","name":"a_very_long_namespace_that_would_exceed_the_target_name_limit","tools":[{"type":"custom","name":"apply_patch"}]}
        ], "tool_choice":{"type":"function","namespace":"b","name":"run"}});
        let mut first = request(body.clone());
        let mut second = request(body);
        let map = bridge_request_tools(&mut first).unwrap();
        assert_eq!(map, bridge_request_tools(&mut second).unwrap());
        let names: Vec<_> = first
            .tools
            .iter()
            .map(|tool| match tool {
                ToolDefinition::Function(tool) => tool.name.as_str(),
                _ => panic!("expected function"),
            })
            .collect();
        assert_eq!(names.iter().copied().collect::<HashSet<_>>().len(), 4);
        assert!(names.iter().all(|name| name.len() <= 64));
        assert_eq!(names[0], "ns1_a_run");
        assert!(names[1].starts_with("const_tool_"));
        assert!(names[3].starts_with("const_tool_"));
        assert_eq!(
            first.tool_choice,
            ToolChoice::Named {
                name: "ns1_b_run".into()
            }
        );
        assert_eq!(map.lookup(None, "run"), None);
        assert_eq!(map.lookup(None, "ns1_a_run"), Some("ns1_a_run"));
        let mut unrelated = ToolCall::function("call_plain", "ns1_a_run", json!({"ok":true}));
        map.restore_call(&mut unrelated).unwrap();
        assert_eq!(unrelated.kind, ToolKind::Function);
        assert!(unrelated.metadata.tool_namespace.is_none());
    }

    #[test]
    fn custom_history_can_outlive_its_declaration_without_changing_raw_input() {
        let raw_input = "{\"input\":\"this is the original input, not our envelope\"}";
        let mut request = request(json!({"model":"test-model","input":[
            {"type":"custom_tool_call","call_id":"call_history","name":"run","namespace":"old","input":raw_input},
            {"type":"custom_tool_call_output","call_id":"call_history","output":"done"}
        ]}));
        let mapping = bridge_request_tools(&mut request).unwrap();
        assert!(request.tools.is_empty());
        let call = request
            .turns
            .iter()
            .flat_map(|turn| &turn.blocks)
            .find_map(|block| match block {
                ContentBlock::ToolCall(call) => Some(call),
                _ => None,
            })
            .unwrap();
        assert_eq!(call.arguments.as_ref().unwrap()["input"], raw_input);
        assert_eq!(call.name, "ns3_old_run");
        let mut restored = call.clone();
        mapping.restore_call(&mut restored).unwrap();
        assert_eq!(restored.id, "call_history");
        assert_eq!(restored.kind, ToolKind::Custom);
        assert_eq!(restored.raw_arguments.as_deref(), Some(raw_input));
        assert_eq!(restored.metadata.tool_namespace.as_deref(), Some("old"));
    }

    #[test]
    fn namespace_allowed_choices_expand_only_the_selected_group() {
        let mut request = request(json!({"model":"test-model","input":[],"tools":[
            {"type":"namespace","name":"a","tools":[{"type":"custom","name":"one"},{"type":"custom","name":"two"}]},
            {"type":"namespace","name":"b","tools":[{"type":"custom","name":"one"}]}
        ],"tool_choice":{"type":"allowed_tools","mode":"required","tools":[{"type":"namespace","name":"a"}]}}));
        bridge_request_tools(&mut request).unwrap();
        assert_eq!(
            request.tool_choice,
            ToolChoice::Allowed {
                mode: AllowedMode::Required,
                names: vec!["ns1_a_one".into(), "ns1_a_two".into()]
            }
        );
    }

    #[test]
    fn malformed_custom_stream_never_emits_executable_input() {
        let mut request = request(
            json!({"model":"test-model","input":[],"tools":[{"type":"custom","name":"run"}]}),
        );
        let mut mapper = bridge_request_tools(&mut request).unwrap().stream_mapper();
        let header = BlockHeader {
            kind: ContentBlockKind::ToolCall,
            source_item_id: None,
            call_id: Some("call_1".into()),
            name: Some("run".into()),
            artifacts: Vec::new(),
            tool_kind: Some(ToolKind::Function),
            tool_namespace: None,
        };
        mapper
            .map(&[CanonicalStreamEvent::BlockStart {
                index: 0,
                block: header,
            }])
            .unwrap();
        for data in ["{\"input\":", "42}"] {
            assert!(
                mapper
                    .map(&[CanonicalStreamEvent::ToolArgumentsDelta {
                        index: 0,
                        data: data.into()
                    }])
                    .unwrap()
                    .is_empty()
            );
        }
        assert!(mapper.flush(0, &mut Vec::new()).is_err());
        assert!(unwrap_input("\"text without envelope\"").is_err());
        assert!(unwrap_input("{\"input\":\"incomplete").is_err());
    }

    #[test]
    fn ordinary_functions_do_not_enter_the_tool_bridge() {
        let mut request = request(
            json!({"model":"test-model","input":[],"tools":[{"type":"function","name":"plain","parameters":{"type":"object"}}]}),
        );
        let before = request.clone();
        assert!(bridge_request_tools(&mut request).unwrap().is_empty());
        assert_eq!(request, before);
    }
}
