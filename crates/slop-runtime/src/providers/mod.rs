//! Native provider integrations owned by the machine daemon.
//!
//! Each provider is compiled in (no runtime plugins). All providers share the
//! same shape: a static model catalog mapping every model to one of three wire
//! protocols (OpenAI Chat Completions, OpenAI Responses, Anthropic Messages),
//! an API-key credential referenced by environment variable, and a
//! provider-neutral chat request/response used by the future agent loop.
//!
//! Implemented: OpenCode Go (`OPENCODE_GO_API_KEY`). Reserved for later
//! milestones: direct OpenAI, Anthropic, and Codex integrations.

pub mod opencode_go;

use slop_core::provider::{ProviderId, is_valid_model_id};
use thiserror::Error;

pub use opencode_go::OpencodeGoProvider;

/// The three HTTP API shapes a provider model can use.
///
/// OpenCode Go exposes all three under one base URL (see its
/// [endpoints table](https://opencode.ai/docs/go/#endpoints)); direct OpenAI
/// and Anthropic integrations each use their native shape. Codex-style clients
/// use the Responses shape with their own session header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WireProtocol {
    /// `POST {base}/chat/completions` (OpenAI Chat Completions compatible).
    OpenAiChatCompletions,
    /// `POST {base}/responses` (OpenAI Responses compatible).
    OpenAiResponses,
    /// `POST {base}/messages` (Anthropic Messages compatible).
    AnthropicMessages,
}

impl WireProtocol {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiChatCompletions => "openai-chat-completions",
            Self::OpenAiResponses => "openai-responses",
            Self::AnthropicMessages => "anthropic-messages",
        }
    }

    /// Path appended to the provider base URL for inference.
    #[must_use]
    pub fn path(self) -> &'static str {
        match self {
            Self::OpenAiChatCompletions => "/chat/completions",
            Self::OpenAiResponses => "/responses",
            Self::AnthropicMessages => "/messages",
        }
    }
}

/// One model offered by a provider and the wire shape it uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProviderModel {
    /// Opaque model id sent to the provider, e.g. `glm-5.3-flash`.
    pub id: &'static str,
    /// Human-readable label from the provider docs.
    pub display_name: &'static str,
    /// Which HTTP API shape serves this model.
    pub wire: WireProtocol,
}

/// Errors for provider configuration, validation, and inference.
///
/// Never contains API keys: keys travel in headers only and are never
/// interpolated into messages.
#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("missing API key: set {env_var}")]
    MissingApiKey { env_var: &'static str },
    #[error("empty API key in {env_var}")]
    EmptyApiKey { env_var: &'static str },
    #[error("unsupported model for {provider}: {model}")]
    InvalidModel { provider: ProviderId, model: String },
    #[error("invalid chat request: {0}")]
    InvalidRequest(&'static str),
    #[error("provider {provider} returned HTTP {status}: {body}")]
    UnexpectedStatus {
        provider: ProviderId,
        status: u16,
        body: String,
    },
    #[error("provider {provider} returned an unrecognized response: {detail}")]
    InvalidResponse {
        provider: ProviderId,
        detail: &'static str,
    },
    #[error("provider request failed: {0}")]
    Http(#[from] reqwest::Error),
}

/// A compiled-in model provider.
pub trait Provider: Send + Sync {
    fn id(&self) -> ProviderId;
    fn display_name(&self) -> &'static str;
    fn base_url(&self) -> &'static str;
    /// Environment variable holding this provider's API key.
    fn env_key_var(&self) -> &'static str;
    fn models(&self) -> &'static [ProviderModel];

    /// Look up which wire shape serves `model`, rejecting unknown ids.
    fn wire_protocol(&self, model: &str) -> Result<WireProtocol, ProviderError> {
        if !is_valid_model_id(model) {
            return Err(ProviderError::InvalidModel {
                provider: self.id(),
                model: model.to_owned(),
            });
        }
        self.models()
            .iter()
            .find(|m| m.id == model)
            .map(|m| m.wire)
            .ok_or_else(|| ProviderError::InvalidModel {
                provider: self.id(),
                model: model.to_owned(),
            })
    }
}

/// Look up a compiled-in provider by id.
///
/// Only OpenCode Go has a runtime integration today; the remaining
/// [`ProviderId`] variants are reserved so callers can match on the same
/// structure without a plugin mechanism.
#[must_use]
pub fn provider(id: ProviderId) -> Option<&'static dyn Provider> {
    match id {
        ProviderId::OpencodeGo => Some(OpencodeGoProvider::instance()),
        ProviderId::OpenAi | ProviderId::Anthropic | ProviderId::Codex => None,
    }
}

/// Conversation role in a provider-neutral chat request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
}

impl Role {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

/// One message in a provider-neutral chat request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

impl ChatMessage {
    #[must_use]
    pub fn user(content: &str) -> Self {
        Self {
            role: Role::User,
            content: content.to_owned(),
        }
    }
}

/// Provider-neutral one-turn chat request.
///
/// The runtime translates this into the model's wire shape. Tool calls,
/// attachments, and multi-turn context assembly are future work; this type
/// covers the smallest useful inference used by the M1 provider slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatRequest {
    /// Opaque model id, validated against the provider catalog at send time.
    pub model: String,
    pub messages: Vec<ChatMessage>,
    /// Upper bound for generated tokens (per-request, not a billing cap).
    pub max_tokens: u32,
    /// Stable per-conversation id sent as `x-opencode-session` for routing
    /// and prompt caching. One conversation keeps one id.
    pub session_id: String,
}

impl ChatRequest {
    /// Bounds for `max_tokens`: large enough for reasoning models, small
    /// enough to catch misconfiguration before spending budget.
    pub const MAX_TOKENS_LIMIT: u32 = 65_536;

    pub fn validate(&self) -> Result<(), ProviderError> {
        if !is_valid_model_id(&self.model) {
            return Err(ProviderError::InvalidRequest("model id is invalid"));
        }
        if self.messages.is_empty() {
            return Err(ProviderError::InvalidRequest("messages must not be empty"));
        }
        if self.messages.iter().any(|m| m.content.is_empty()) {
            return Err(ProviderError::InvalidRequest(
                "message content must not be empty",
            ));
        }
        if self.max_tokens == 0 || self.max_tokens > Self::MAX_TOKENS_LIMIT {
            return Err(ProviderError::InvalidRequest("max_tokens is out of range"));
        }
        if !is_valid_session_id(&self.session_id) {
            return Err(ProviderError::InvalidRequest("session_id is invalid"));
        }
        Ok(())
    }
}

/// Stable per-conversation session ids: ASCII letters, digits, `.`, `-`, `_`.
#[must_use]
pub fn is_valid_session_id(session: &str) -> bool {
    if session.is_empty() || session.len() > 128 {
        return false;
    }
    session
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// Provider-neutral token usage for a completed turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

/// Provider-neutral completed turn with the recorded usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatResponse {
    pub model: String,
    pub text: String,
    pub usage: Usage,
    pub wire: WireProtocol,
}

/// One streaming delta from an inference request.
///
/// `text` carries visible assistant deltas; `reasoning` carries
/// provider-specific thinking deltas (e.g. `reasoning_content`). Either may be
/// empty; both empty means a keep-alive frame.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StreamDelta {
    pub text: String,
    pub reasoning: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_resolves_opencode_go_only() {
        assert!(provider(ProviderId::OpencodeGo).is_some());
        assert!(provider(ProviderId::OpenAi).is_none());
        assert!(provider(ProviderId::Anthropic).is_none());
        assert!(provider(ProviderId::Codex).is_none());
    }

    #[test]
    fn chat_request_validation() {
        let base = ChatRequest {
            model: "glm-5.3-flash".to_owned(),
            messages: vec![ChatMessage::user("hi")],
            max_tokens: 64,
            session_id: "test-session_1".to_owned(),
        };
        base.validate().unwrap();

        let empty = ChatRequest {
            messages: Vec::new(),
            ..base.clone()
        };
        assert!(empty.validate().is_err());

        let bad_tokens = ChatRequest {
            max_tokens: 0,
            ..base.clone()
        };
        assert!(bad_tokens.validate().is_err());

        let bad_session = ChatRequest {
            session_id: "has space".to_owned(),
            ..base.clone()
        };
        assert!(bad_session.validate().is_err());
    }
}
