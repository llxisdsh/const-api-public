use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProtocolError {
    code: &'static str,
    value: String,
}

impl ProtocolError {
    pub(crate) fn invalid(value: impl Into<String>) -> Self {
        Self {
            code: "invalid_protocol",
            value: value.into(),
        }
    }

    pub(crate) fn unsupported(value: impl Into<String>) -> Self {
        Self {
            code: "unsupported_protocol",
            value: value.into(),
        }
    }

    #[cfg(test)]
    pub(crate) fn code(&self) -> &'static str {
        self.code
    }

    #[cfg(test)]
    pub(crate) fn action_message(&self) -> String {
        if matches!(
            self.value.as_str(),
            "bedrock" | "bedrock_converse" | "aws_bedrock"
        ) {
            return "该渠道使用已移除的 Bedrock Converse，请通过 Amazon Bedrock Mantle 重新配置。"
                .to_string();
        }
        self.to_string()
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.value.is_empty() {
            write!(formatter, "{}: protocol is empty", self.code)
        } else if self.code == "unsupported_protocol" {
            write!(
                formatter,
                "{}: unsupported protocol: {}",
                self.code, self.value
            )
        } else {
            write!(formatter, "{}: {}", self.code, self.value)
        }
    }
}

impl std::error::Error for ProtocolError {}
