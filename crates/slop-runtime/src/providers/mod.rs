//! Native provider integrations owned by the machine daemon.
//!
//! Each provider is compiled in (no runtime plugins). All providers share the
//! same shape: a static model catalog mapping every model to one of three wire
//! protocols (OpenAI Chat Completions, OpenAI Responses, Anthropic Messages),
//! daemon-owned credentials (API keys or renewable ChatGPT authorization), and a
//! provider-neutral chat request/response used by the future agent loop.
//!
//! Implemented: OpenCode Go (`OPENCODE_GO_API_KEY`), OpenCode Zen
//! (`OPENCODE_ZEN_API_KEY`), and headless Codex model access (ChatGPT subscription
//! login or `OPENAI_API_KEY`). Direct OpenAI and Anthropic identities remain reserved.

pub mod chatgpt_auth;
pub mod codex;
pub mod inference;
mod opencode;
pub mod opencode_go;
pub mod opencode_zen;
mod structured_wire;
#[cfg(test)]
mod test_http;
mod transport;
mod wire;

use std::{fmt, future::Future, pin::Pin};

use slop_core::provider::{ProviderId, is_valid_model_id};
use thiserror::Error;

pub use codex::CodexProvider;
pub use opencode_go::OpencodeGoProvider;
pub use opencode_zen::OpencodeZenProvider;

/// The three HTTP API shapes a provider model can use.
///
/// OpenCode Go and Zen expose all three under separate gateway base URLs (see
/// their documented endpoint tables); direct OpenAI
/// and Anthropic integrations each use their native shape. Codex uses the
/// public Responses API with ChatGPT plan usage or Platform API-key auth.
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
/// Upstream response bodies are never retained in diagnostics. Transport
/// failures have a fixed display message; their source is runtime-private.
#[derive(Error)]
pub enum ProviderError {
    #[error("missing API key: set {env_var}")]
    MissingApiKey { env_var: &'static str },
    #[error("empty API key in {env_var}")]
    EmptyApiKey { env_var: &'static str },
    #[error("unsupported model for {provider}: {model}")]
    InvalidModel { provider: ProviderId, model: String },
    #[error("invalid chat request: {0}")]
    InvalidRequest(&'static str),
    #[error("provider {provider} returned HTTP {status}")]
    UnexpectedStatus { provider: ProviderId, status: u16 },
    #[error("provider {provider} returned an unrecognized response: {detail}")]
    InvalidResponse {
        provider: ProviderId,
        detail: &'static str,
    },
    #[error("unsupported provider content or capability: {capability}")]
    UnsupportedCapability { capability: &'static str },
    #[error("provider {provider} reported a failed turn")]
    TurnFailed { provider: ProviderId },
    #[error("request or response exceeded a bound: {detail}")]
    LimitExceeded { detail: &'static str },
    #[error("ChatGPT authentication failed: {detail}")]
    Authentication { detail: &'static str },
    #[error("ChatGPT credential storage failed")]
    CredentialStorage,
    #[error("provider transport request failed")]
    Http(#[from] reqwest::Error),
}

impl fmt::Debug for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// Authentication implemented by a compiled-in adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    ApiKey,
    ChatGptSubscription,
}

/// A compiled-in model provider.
pub trait Provider: Send + Sync {
    fn id(&self) -> ProviderId;
    fn display_name(&self) -> &'static str;
    fn base_url(&self) -> &'static str;
    /// Environment variable holding this provider's API key.
    fn env_key_var(&self) -> &'static str;
    fn models(&self) -> &'static [ProviderModel];

    fn auth_modes(&self) -> &'static [AuthMode] {
        &[AuthMode::ApiKey]
    }

    /// Whether the current integration requires an explicit output-token limit.
    fn requires_output_limit(&self) -> bool {
        true
    }

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
/// OpenCode Go, Zen, and Codex have runtime integrations; the remaining
/// [`ProviderId`] variants are reserved so callers can match on the same
/// structure without a plugin mechanism.
#[must_use]
pub fn provider(id: ProviderId) -> Option<&'static dyn Provider> {
    match id {
        ProviderId::OpencodeGo => Some(OpencodeGoProvider::instance()),
        ProviderId::OpencodeZen => Some(OpencodeZenProvider::instance()),
        ProviderId::Codex => Some(CodexProvider::instance()),
        ProviderId::OpenAi | ProviderId::Anthropic => None,
    }
}

/// A provider operation scheduled on the daemon's shared async runtime.
pub type ProviderFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ProviderError>> + Send + 'a>>;

/// Shared execution interface for the current, text-only one-turn slice.
///
/// The metadata registry and authenticated clients have separate lifetimes.
/// This object-safe interface lets execution select a client without matching
/// its concrete integration. Account discovery, structured blocks, cancellation,
/// and continuation are still proposed in `docs/provider-interface.md`.
pub trait ProviderClient: Send + Sync {
    fn descriptor(&self) -> &dyn Provider;

    fn capabilities(&self, _model: &str) -> inference::ModelCapabilities {
        inference::ModelCapabilities {
            tools: false,
            reasoning_efforts: Vec::new(),
        }
    }

    /// One inference attempt. Dropping this future cancels local transport;
    /// adapters never execute tools or retry an inference behind the supervisor.
    fn infer<'a>(
        &'a self,
        request: &'a inference::InferenceRequest,
        on_event: &'a mut (dyn FnMut(inference::ProviderEvent) + Send),
    ) -> ProviderFuture<'a, inference::InferenceResponse> {
        Box::pin(async move {
            use inference::*;
            request.validate(self.descriptor(), &self.capabilities(&request.model))?;
            let mut messages = Vec::new();
            for message in &request.messages {
                if message
                    .blocks
                    .iter()
                    .any(|block| !matches!(block.content, BlockContent::Text { .. }))
                    || message
                        .continuation
                        .as_ref()
                        .is_some_and(|continuation| continuation.required)
                {
                    return Err(ProviderError::UnsupportedCapability {
                        capability: "structured history",
                    });
                }
                let role = match message.role {
                    MessageRole::System => Role::System,
                    MessageRole::Developer => Role::Developer,
                    MessageRole::User => Role::User,
                    MessageRole::Assistant => Role::Assistant,
                    MessageRole::Tool => {
                        return Err(ProviderError::UnsupportedCapability {
                            capability: "tool results",
                        });
                    }
                };
                messages.push(ChatMessage {
                    role,
                    content: message.visible_text(),
                });
            }
            let legacy = ChatRequest {
                model: request.model.clone(),
                messages,
                max_tokens: request.settings.max_output_tokens,
                session_id: request.session_id.clone(),
            };
            let result = self
                .complete_streaming(&legacy, &mut |delta| {
                    if !delta.text.is_empty() {
                        on_event(ProviderEvent::TextDelta {
                            block_index: 0,
                            text: delta.text,
                        });
                    }
                })
                .await?;
            Ok(InferenceResponse {
                resolved_model: result.resolved_model,
                message: InferenceMessage::text(
                    MessageRole::Assistant,
                    format!("{}:block:0", request.request_id),
                    result.text,
                ),
                usage: result.usage,
                finish_reason: match result.outcome {
                    TurnOutcome::Completed => FinishReason::Stop,
                    TurnOutcome::Incomplete { .. } => FinishReason::OutputLimit,
                },
            })
        })
    }

    fn auth_mode(&self) -> AuthMode {
        AuthMode::ApiKey
    }

    /// Validate before any inference network access. No automatic fallback.
    fn validate(&self, request: &ChatRequest) -> Result<WireProtocol, ProviderError> {
        request.validate()?;
        if self.descriptor().requires_output_limit() && request.max_tokens.is_none() {
            return Err(ProviderError::InvalidRequest("max_tokens is required"));
        }
        let wire = self.descriptor().wire_protocol(&request.model)?;
        if wire == WireProtocol::AnthropicMessages {
            if request
                .messages
                .iter()
                .any(|message| message.role == Role::Developer)
            {
                return Err(ProviderError::UnsupportedCapability {
                    capability: "developer messages for Anthropic Messages",
                });
            }
            // The current adapter accepts one optional leading system message.
            // Hoisting later instructions or joining messages would change the
            // supplied context without an explicit conversion policy.
            if request
                .messages
                .iter()
                .skip(1)
                .any(|message| message.role == Role::System)
            {
                return Err(ProviderError::UnsupportedCapability {
                    capability: "multiple or non-leading system messages",
                });
            }
            if request.messages.iter().all(|m| m.role == Role::System) {
                return Err(ProviderError::InvalidRequest(
                    "messages require a user or assistant turn",
                ));
            }
        }
        Ok(wire)
    }

    fn list_models(&self) -> ProviderFuture<'_, Vec<String>>;
    fn complete<'a>(&'a self, request: &'a ChatRequest) -> ProviderFuture<'a, ChatResponse>;
    fn complete_streaming<'a>(
        &'a self,
        request: &'a ChatRequest,
        on_delta: &'a mut (dyn FnMut(StreamDelta) + Send),
    ) -> ProviderFuture<'a, ChatResponse>;
}

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
    pub const MAX_MESSAGES: usize = 256;
    /// Cap on aggregate UTF-8 input bytes per request.
    pub const MAX_REQUEST_BYTES: usize = 1_000_000;

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

/// Provenance of a known total. Input and output counters are provider-reported.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum UsageSource {
    Reported,
    Derived,
}

/// Best-known provider-neutral token usage for a turn.
///
/// Missing counters remain unknown. Stream updates are cumulative snapshots,
/// not increments; explicit zero is a known value. Detailed usage counters and
/// per-counter completeness remain future additions.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq, Default)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub total_source: Option<UsageSource>,
}

impl Usage {
    #[must_use]
    pub fn from_reported(input: Option<u64>, output: Option<u64>, total: Option<u64>) -> Self {
        let derived = input.zip(output).and_then(|(i, o)| i.checked_add(o));
        Self {
            input_tokens: input,
            output_tokens: output,
            total_tokens: total.or(derived),
            total_source: if total.is_some() {
                Some(UsageSource::Reported)
            } else {
                derived.map(|_| UsageSource::Derived)
            },
        }
    }

    /// Merge a cumulative provider snapshot without dropping absent fields or
    /// double-counting repeated updates. Recompute derived totals after changes.
    pub fn merge(&mut self, seen: Self) {
        self.input_tokens = seen.input_tokens.or(self.input_tokens);
        self.output_tokens = seen.output_tokens.or(self.output_tokens);
        if seen.total_source == Some(UsageSource::Reported) {
            self.total_tokens = seen.total_tokens;
            self.total_source = seen.total_source;
        } else if seen.input_tokens.is_some() || seen.output_tokens.is_some() {
            let updated = Self::from_reported(self.input_tokens, self.output_tokens, None);
            self.total_tokens = updated.total_tokens;
            self.total_source = updated.total_source;
        }
    }
}

/// Provider-neutral terminal outcome for a turn.
///
/// Reasoning models can exhaust `max_tokens` on thinking before producing
/// visible text; that `Incomplete` state is recorded with the provider's
/// reason instead of masquerading as success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnOutcome {
    Completed,
    Incomplete { reason: String },
}

/// Provider-neutral completed turn with the recorded usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatResponse {
    /// Requested model selection. The upstream may resolve an alias differently.
    pub model: String,
    /// Actual model reported upstream, or unknown when omitted.
    pub resolved_model: Option<String>,
    pub text: String,
    pub usage: Usage,
    pub wire: WireProtocol,
    pub outcome: TurnOutcome,
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
    fn registry_resolves_implemented_providers() {
        for id in [
            ProviderId::OpencodeGo,
            ProviderId::OpencodeZen,
            ProviderId::Codex,
        ] {
            assert_eq!(provider(id).unwrap().id(), id);
        }
        assert!(provider(ProviderId::OpenAi).is_none());
        assert!(provider(ProviderId::Anthropic).is_none());
    }

    #[test]
    fn chat_request_validation() {
        let base = ChatRequest {
            model: "glm-5.3-flash".to_owned(),
            messages: vec![ChatMessage::user("hi")],
            max_tokens: Some(64),
            session_id: "test-session_1".to_owned(),
        };
        base.validate().unwrap();

        let empty = ChatRequest {
            messages: Vec::new(),
            ..base.clone()
        };
        assert!(empty.validate().is_err());

        let bad_tokens = ChatRequest {
            max_tokens: Some(0),
            ..base.clone()
        };
        assert!(bad_tokens.validate().is_err());

        let bad_session = ChatRequest {
            session_id: "has space".to_owned(),
            ..base.clone()
        };
        assert!(bad_session.validate().is_err());
    }

    #[test]
    fn chat_request_memory_bounds() {
        let base = ChatRequest {
            model: "glm-5.3-flash".to_owned(),
            messages: vec![ChatMessage::user("hi")],
            max_tokens: Some(64),
            session_id: "s".to_owned(),
        };
        let many = ChatRequest {
            messages: vec![ChatMessage::user("hi"); ChatRequest::MAX_MESSAGES + 1],
            ..base.clone()
        };
        assert!(matches!(
            many.validate(),
            Err(ProviderError::LimitExceeded { .. })
        ));

        let big = ChatRequest {
            messages: vec![ChatMessage {
                role: Role::User,
                content: "x".repeat(ChatRequest::MAX_REQUEST_BYTES + 1),
            }],
            ..base.clone()
        };
        assert!(matches!(
            big.validate(),
            Err(ProviderError::LimitExceeded { .. })
        ));
    }
}
