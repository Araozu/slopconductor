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
