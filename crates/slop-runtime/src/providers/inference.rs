//! Structured, provider-neutral inference. One request is one model operation;
//! the daemon owns the surrounding tool loop and all side effects.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ChatRequest, Provider, ProviderError, Usage, WireProtocol};

pub const MAX_BLOCKS: usize = 256;
pub const MAX_TOOL_CALLS: usize = 16;
pub const MAX_ARGUMENT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationSettings {
    pub max_output_tokens: Option<u32>,
    /// Values are model-specific and validated against its descriptor. An
    /// absent value means the provider default, not a fabricated effort level.
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    System,
    Developer,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContentBlock {
    pub id: String,
    #[serde(flatten)]
    pub content: BlockContent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BlockContent {
    Text {
        text: String,
    },
    ToolCall {
        call_id: String,
        provider_call_id: String,
        name: String,
        arguments: Value,
    },
    ToolResult {
        call_id: String,
        provider_call_id: String,
        is_error: bool,
        output: String,
        artifact_ids: Vec<String>,
        effects_unknown: bool,
    },
    Refusal {
        text: String,
    },
}

/// Runtime-private replay material. It never appears in public message DTOs or
/// deltas. Required continuation may only be replayed with its original model.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Continuation {
    pub scope: String,
    pub provider: String,
    pub model: String,
    pub wire: String,
    pub required: bool,
    pub items: Vec<Value>,
}

impl std::fmt::Debug for Continuation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Continuation")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("required", &self.required)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InferenceMessage {
    pub role: MessageRole,
    pub blocks: Vec<ContentBlock>,
    pub continuation: Option<Continuation>,
}

impl InferenceMessage {
    pub fn text(role: MessageRole, id: String, text: String) -> Self {
        Self {
            role,
            blocks: vec![ContentBlock {
                id,
                content: BlockContent::Text { text },
            }],
            continuation: None,
        }
    }

    pub fn visible_text(&self) -> String {
        self.blocks
            .iter()
            .filter_map(|block| match &block.content {
                BlockContent::Text { text } | BlockContent::Refusal { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone)]
pub struct InferenceRequest {
    pub request_id: String,
    pub session_id: String,
    pub model: String,
    pub settings: GenerationSettings,
    pub messages: Vec<InferenceMessage>,
    pub tools: Vec<ToolDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    ToolCalls,
    OutputLimit,
    Refusal,
}

#[derive(Debug, Clone)]
pub struct InferenceResponse {
    pub resolved_model: Option<String>,
    pub message: InferenceMessage,
    pub usage: Usage,
    pub finish_reason: FinishReason,
}

#[derive(Debug, Clone)]
pub enum ProviderEvent {
    TextDelta {
        block_index: usize,
        text: String,
    },
    ToolArgumentsDelta {
        block_index: usize,
        fragment: String,
    },
    UsageUpdated {
        usage: Usage,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCapabilities {
    pub tools: bool,
    pub reasoning_efforts: Vec<String>,
}

pub fn capabilities(_provider: &dyn Provider, _model: &str) -> ModelCapabilities {
    // Gateway adapters implement all three wire formats. Gateway-specific
    // reasoning settings remain unadvertised until verified for each model.
    ModelCapabilities {
        tools: true,
        reasoning_efforts: Vec::new(),
    }
}

impl InferenceRequest {
    pub fn validate(
        &self,
        provider: &dyn Provider,
        capabilities: &ModelCapabilities,
    ) -> Result<WireProtocol, ProviderError> {
        let wire = provider.wire_protocol(&self.model)?;
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
                    serde_json::to_vec(block)
                        .map_err(|_| ProviderError::InvalidRequest("invalid block"))?
                        .len(),
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
                            || serde_json::to_vec(arguments).unwrap_or_default().len()
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
        let total = serde_json::to_vec(&self.messages)
            .map_err(|_| ProviderError::InvalidRequest("invalid context"))?
            .len();
        if bytes > ChatRequest::MAX_REQUEST_BYTES || total > ChatRequest::MAX_REQUEST_BYTES {
            return Err(ProviderError::LimitExceeded {
                detail: "structured context exceeds byte bound",
            });
        }
        if !super::is_valid_session_id(&self.session_id) {
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
