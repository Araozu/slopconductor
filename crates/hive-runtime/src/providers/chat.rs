use hive_core::provider::is_valid_model_id;

use super::ProviderError;

/// Conversation role in a provider-neutral chat request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    System,
    Developer,
    User,
    Assistant,
}

impl Role {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Developer => "developer",
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
    /// Optional generated-token limit (not a billing cap). Each authenticated
    /// adapter validates whether a limit is supported or required.
    pub max_tokens: Option<u32>,
    /// Stable per-conversation id for provider routing/cache hints. OpenCode
    /// sends `x-opencode-session`; Codex sends `prompt_cache_key`.
    pub session_id: String,
}

impl ChatRequest {
    /// Bounds for `max_tokens`: large enough for reasoning models, small
    /// enough to catch misconfiguration before spending budget.
    pub const MAX_TOKENS_LIMIT: u32 = 65_536;
    /// Cap on turns per request so one oversized context cannot exhaust the
    /// shared daemon's memory.
    pub const MAX_MESSAGES: usize = 8192;
    /// Cap on aggregate UTF-8 input bytes per request.
    pub const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;

    pub fn validate(&self) -> Result<(), ProviderError> {
        if !is_valid_model_id(&self.model) {
            return Err(ProviderError::InvalidRequest("model id is invalid"));
        }
        if self.messages.is_empty() {
            return Err(ProviderError::InvalidRequest("messages must not be empty"));
        }
        if self.messages.len() > Self::MAX_MESSAGES {
            return Err(ProviderError::LimitExceeded {
                detail: "too many messages",
            });
        }
        if self.messages.iter().any(|m| m.content.is_empty()) {
            return Err(ProviderError::InvalidRequest(
                "message content must not be empty",
            ));
        }
        let bytes: usize = self.messages.iter().map(|m| m.content.len()).sum();
        if bytes > Self::MAX_REQUEST_BYTES {
            return Err(ProviderError::LimitExceeded {
                detail: "request content exceeds byte bound",
            });
        }
        if self
            .max_tokens
            .is_some_and(|limit| limit == 0 || limit > Self::MAX_TOKENS_LIMIT)
        {
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
