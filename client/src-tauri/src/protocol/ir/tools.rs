use serde::{Deserialize, Serialize};

use super::{BlockMetadata, CachePolicy, ContentBlock, IrError, ItemStatus, OpaqueArtifact};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct FunctionTool {
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) input_schema: serde_json::Value,
    pub(crate) strict: Option<bool>,
    pub(crate) cache_policy: Option<CachePolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) defer_loading: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) allowed_callers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) input_examples: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) eager_input_streaming: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct CustomTool {
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) grammar: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct NamespaceTool {
    pub(crate) namespace: String,
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) tools: Vec<NamespaceToolDefinition>,
}

// Keep the previous untagged function representation readable in IR snapshots.
// A custom tool has a grammar instead of an input_schema; it must never be
// decoded as a function with an empty parameter schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum NamespaceToolDefinition {
    Function(FunctionTool),
    Custom(CustomTool),
}

impl NamespaceToolDefinition {
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Function(tool) => &tool.name,
            Self::Custom(tool) => &tool.name,
        }
    }
}

impl From<FunctionTool> for NamespaceToolDefinition {
    fn from(tool: FunctionTool) -> Self {
        Self::Function(tool)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HostedToolKind {
    WebSearch,
    FileSearch,
    CodeExecution,
    ComputerUse,
    Mcp,
    UrlContext,
    ProviderSpecific,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct HostedTool {
    pub(crate) kind: HostedToolKind,
    pub(crate) provider: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) config: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ToolDefinition {
    Function(FunctionTool),
    Custom(CustomTool),
    Namespace(NamespaceTool),
    Hosted(HostedTool),
}

impl ToolDefinition {
    pub(crate) fn validate(&self) -> Result<(), IrError> {
        match self {
            Self::Function(tool) => {
                validate_name(&tool.name, "tools.function.name")?;
                if tool
                    .allowed_callers
                    .iter()
                    .any(|caller| caller.trim().is_empty())
                {
                    return Err(IrError::new(
                        "invalid_tool_caller",
                        "tools.function.allowed_callers",
                        "allowed caller is empty",
                    ));
                }
                Ok(())
            }
            Self::Custom(tool) => validate_name(&tool.name, "tools.custom.name"),
            Self::Namespace(tool) => {
                validate_name(&tool.namespace, "tools.namespace.name")?;
                for nested in &tool.tools {
                    validate_name(nested.name(), "tools.namespace.tools.name")?;
                }
                Ok(())
            }
            Self::Hosted(tool) => {
                if tool
                    .name
                    .as_deref()
                    .is_some_and(|name| name.trim().is_empty())
                {
                    return Err(IrError::new(
                        "invalid_tool_name",
                        "tools.hosted.name",
                        "hosted tool name is empty",
                    ));
                }
                Ok(())
            }
        }
    }
}

fn validate_name(name: &str, path: &str) -> Result<(), IrError> {
    if name.trim().is_empty() {
        Err(IrError::new(
            "invalid_tool_name",
            path,
            "tool name is empty",
        ))
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AllowedMode {
    Auto,
    Required,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ToolChoice {
    #[default]
    Auto,
    None,
    Required,
    Named {
        name: String,
    },
    Allowed {
        mode: AllowedMode,
        names: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ToolKind {
    Function,
    Custom,
    Namespace,
    Hosted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ToolCall {
    pub(crate) id: String,
    pub(crate) source_item_id: Option<String>,
    pub(crate) kind: ToolKind,
    pub(crate) name: String,
    pub(crate) arguments: Option<serde_json::Value>,
    pub(crate) raw_arguments: Option<String>,
    pub(crate) status: ItemStatus,
    #[serde(default)]
    pub(crate) artifacts: Vec<OpaqueArtifact>,
    #[serde(default)]
    pub(crate) metadata: BlockMetadata,
}

impl ToolCall {
    #[cfg(test)]
    pub(crate) fn function(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            id: id.into(),
            source_item_id: None,
            kind: ToolKind::Function,
            name: name.into(),
            arguments: Some(arguments),
            raw_arguments: None,
            status: ItemStatus::Completed,
            artifacts: Vec::new(),
            metadata: BlockMetadata::default(),
        }
    }

    pub(super) fn validate(&self, path: &str) -> Result<(), IrError> {
        if self.id.trim().is_empty() {
            return Err(IrError::new(
                "invalid_tool_call_id",
                path,
                "tool call ID is empty",
            ));
        }
        validate_name(&self.name, path)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ToolResult {
    pub(crate) call_id: String,
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) content: Vec<ContentBlock>,
    pub(crate) status: ItemStatus,
    pub(crate) is_error: bool,
    #[serde(default)]
    pub(crate) metadata: BlockMetadata,
}
