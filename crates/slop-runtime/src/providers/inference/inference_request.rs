use super::super::{ChatRequest, Provider, ProviderError, WireProtocol};
use super::{
    BlockContent, GenerationSettings, InferenceMessage, MAX_ARGUMENT_BYTES, MAX_BLOCKS,
    MAX_TOOL_CALLS, MessageRole, ModelCapabilities, ToolDefinition,
};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct InferenceRequest {
    pub request_id: String,
    pub session_id: String,
    pub model: String,
    pub settings: GenerationSettings,
    pub messages: Vec<InferenceMessage>,
    pub tools: Vec<ToolDefinition>,
}

impl InferenceRequest {
    pub fn validate(
        &self,
        provider: &dyn Provider,
        capabilities: &ModelCapabilities,
    ) -> Result<WireProtocol, ProviderError> {
        let wire = provider.wire_protocol(&self.model)?;
        if self.settings.full_context == Some(true) && capabilities.context_window_tokens.is_none()
        {
            return Err(ProviderError::UnsupportedCapability {
                capability: "settings.full_context",
            });
        }
        if self.request_id.is_empty()
            || self.request_id.len() > 128
            || self.messages.is_empty()
            || self.messages.len() > ChatRequest::MAX_MESSAGES
            || self.tools.len() > MAX_TOOL_CALLS
        {
            return Err(ProviderError::InvalidRequest(
                "invalid structured request bounds",
            ));
        }
        if self.settings.max_output_tokens.is_none() && provider.requires_output_limit()
            || self
                .settings
                .max_output_tokens
                .is_some_and(|n| n == 0 || n > ChatRequest::MAX_TOKENS_LIMIT)
        {
            return Err(ProviderError::InvalidRequest("invalid output-token limit"));
        }
        if self
            .settings
            .reasoning_effort
            .as_ref()
            .is_some_and(|effort| !capabilities.reasoning_efforts.contains(effort))
        {
            return Err(ProviderError::UnsupportedCapability {
                capability: "settings.reasoning_effort",
            });
        }
        if !self.tools.is_empty() && !capabilities.tools {
            return Err(ProviderError::UnsupportedCapability {
                capability: "tools",
            });
        }
        let mut tool_names = HashSet::new();
        for tool in &self.tools {
            if !valid_name(&tool.name)
                || !tool_names.insert(&tool.name)
                || !tool.parameters.is_object()
            {
                return Err(ProviderError::InvalidRequest("invalid tool declaration"));
            }
        }
        let mut pending = HashMap::new();
        let mut calls = HashSet::new();
        let mut bytes = 0usize;
        for (index, message) in self.messages.iter().enumerate() {
            if message.blocks.len() > MAX_BLOCKS || message.blocks.is_empty() {
                return Err(ProviderError::InvalidRequest("invalid message blocks"));
            }
            if wire == WireProtocol::AnthropicMessages
                && (message.role == MessageRole::Developer
                    || message.role == MessageRole::System && index != 0)
            {
                return Err(ProviderError::UnsupportedCapability {
                    capability: "instruction role or placement",
                });
            }
            if !pending.is_empty() && message.role != MessageRole::Tool {
                return Err(ProviderError::InvalidRequest(
                    "tool calls require matching results",
                ));
            }
            if let Some(continuation) = &message.continuation {
                let compatible = continuation.provider == provider.id().as_str()
                    && continuation.model == self.model
                    && continuation.wire == wire.as_str();
                if continuation.required && !compatible {
                    return Err(ProviderError::UnsupportedCapability {
                        capability: "context_incompatible: required provider continuation",
                    });
                }
            }
            for block in &message.blocks {
                bytes = bytes.saturating_add(
                    crate::json::encoded_len(block)
                        .map_err(|_| ProviderError::InvalidRequest("invalid block"))?,
                );
                match &block.content {
                    BlockContent::ToolCall {
                        call_id,
                        provider_call_id,
                        name,
                        arguments,
                    } => {
                        if message.role != MessageRole::Assistant
                            || !valid_name(name)
                            || call_id.is_empty()
                            || call_id.len() > 256
                            || provider_call_id.is_empty()
                            || provider_call_id.len() > 256
                            || !arguments.is_object()
                            || crate::json::encoded_len(arguments).unwrap_or(usize::MAX)
                                > MAX_ARGUMENT_BYTES
                            || !calls.insert(call_id)
                            || pending.insert(call_id, provider_call_id).is_some()
                        {
                            return Err(ProviderError::InvalidRequest("invalid tool call"));
                        }
                    }
                    BlockContent::ToolResult {
                        call_id,
                        provider_call_id,
                        ..
                    } => {
                        if message.role != MessageRole::Tool
                            || pending.remove(call_id) != Some(provider_call_id)
                        {
                            return Err(ProviderError::InvalidRequest("unmatched tool result"));
                        }
                    }
                    _ if message.role == MessageRole::Tool => {
                        return Err(ProviderError::InvalidRequest("invalid tool-result message"));
                    }
                    _ => {}
                }
            }
        }
        if !pending.is_empty() {
            return Err(ProviderError::InvalidRequest(
                "tool calls require matching results",
            ));
        }
        let total = crate::json::encoded_len(&self.messages)
            .map_err(|_| ProviderError::InvalidRequest("invalid context"))?;
        if bytes > ChatRequest::MAX_REQUEST_BYTES || total > ChatRequest::MAX_REQUEST_BYTES {
            return Err(ProviderError::LimitExceeded {
                detail: "structured context exceeds byte bound",
            });
        }
        if !super::super::is_valid_session_id(&self.session_id) {
            return Err(ProviderError::InvalidRequest("invalid session id"));
        }
        Ok(wire)
    }
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
}
