use std::collections::HashSet;
use std::fmt;

use crate::protocol::ir::{FunctionTool, NamespaceToolDefinition, ToolDefinition};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NamespaceError {
    NameCollision(String),
    CustomToolUnsupported(String),
}

impl fmt::Display for NamespaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NameCollision(name) => write!(formatter, "flattened tool name collides: {name}"),
            Self::CustomToolUnsupported(name) => {
                write!(
                    formatter,
                    "cannot flatten custom tool {name} into a JSON function"
                )
            }
        }
    }
}

impl std::error::Error for NamespaceError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NamespaceNameMap {
    pub(crate) flattened: String,
    pub(crate) namespace: String,
    pub(crate) name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FlattenedNamespaceTools {
    pub(crate) tools: Vec<FunctionTool>,
    pub(crate) names: Vec<NamespaceNameMap>,
}

impl FlattenedNamespaceTools {
    #[cfg(test)]
    pub(crate) fn reverse(&self, flattened: &str) -> Option<(&str, &str)> {
        self.names
            .iter()
            .find(|entry| entry.flattened == flattened)
            .map(|entry| (entry.namespace.as_str(), entry.name.as_str()))
    }
}

pub(crate) fn flatten_namespace_tools(
    definitions: &[ToolDefinition],
) -> Result<FlattenedNamespaceTools, NamespaceError> {
    let mut names = HashSet::new();
    let mut tools = Vec::new();
    let mut mappings = Vec::new();
    for definition in definitions {
        if let ToolDefinition::Function(tool) = definition {
            if !names.insert(tool.name.clone()) {
                return Err(NamespaceError::NameCollision(tool.name.clone()));
            }
            tools.push(tool.clone());
        }
    }
    for definition in definitions {
        let ToolDefinition::Namespace(namespace) = definition else {
            continue;
        };
        for tool in &namespace.tools {
            let NamespaceToolDefinition::Function(tool) = tool else {
                return Err(NamespaceError::CustomToolUnsupported(format!(
                    "{}.{}",
                    namespace.namespace,
                    tool.name()
                )));
            };
            let flattened = format!(
                "ns{}_{}_{}",
                namespace.namespace.len(),
                namespace.namespace,
                tool.name
            );
            if !names.insert(flattened.clone()) {
                return Err(NamespaceError::NameCollision(flattened));
            }
            let mut flattened_tool = tool.clone();
            flattened_tool.name = flattened.clone();
            tools.push(flattened_tool);
            mappings.push(NamespaceNameMap {
                flattened,
                namespace: namespace.namespace.clone(),
                name: tool.name.clone(),
            });
        }
    }
    Ok(FlattenedNamespaceTools {
        tools,
        names: mappings,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{NamespaceError, flatten_namespace_tools};
    use crate::protocol::ir::{FunctionTool, NamespaceTool, ToolDefinition};

    fn function(name: &str) -> FunctionTool {
        FunctionTool {
            name: name.to_string(),
            description: None,
            input_schema: json!({"type":"object"}),
            strict: None,
            cache_policy: None,
            defer_loading: None,
            allowed_callers: Vec::new(),
            input_examples: Vec::new(),
            eager_input_streaming: None,
        }
    }

    #[test]
    fn namespace_mapping_is_stable_reversible_and_keeps_regular_functions() {
        let tools = vec![
            ToolDefinition::Function(function("plain")),
            ToolDefinition::Namespace(NamespaceTool {
                namespace: "repo".to_string(),
                description: None,
                tools: vec![function("read").into(), function("write").into()],
            }),
        ];

        let flattened = flatten_namespace_tools(&tools).unwrap();

        assert_eq!(flattened.tools[0].name, "plain");
        assert_eq!(flattened.tools[1].name, "ns4_repo_read");
        assert_eq!(flattened.tools[2].name, "ns4_repo_write");
        assert_eq!(flattened.reverse("ns4_repo_read"), Some(("repo", "read")));
        assert_eq!(flattened.reverse("plain"), None);
    }

    #[test]
    fn namespace_mapping_rejects_flattened_name_collisions() {
        let tools = vec![
            ToolDefinition::Function(function("ns4_repo_read")),
            ToolDefinition::Namespace(NamespaceTool {
                namespace: "repo".to_string(),
                description: None,
                tools: vec![function("read").into()],
            }),
        ];

        let error = flatten_namespace_tools(&tools).unwrap_err();

        assert_eq!(
            error,
            NamespaceError::NameCollision("ns4_repo_read".to_string())
        );
    }
}
