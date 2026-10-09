//! OpenCode Go subscription gateway with three documented wire shapes.
//!
//! Reference: <https://opencode.ai/docs/go/>. The Go catalog remains separate
//! from Zen: an identical model ID can use a different wire shape there.
//! Transport, bounded decoding, and safe diagnostics are shared internally.

use slop_core::provider::ProviderId;

use super::{
    ChatRequest, ChatResponse, Provider, ProviderClient, ProviderError, ProviderFuture,
    ProviderModel, StreamDelta, WireProtocol,
};

/// Base URL for all OpenCode Go endpoints.
pub const BASE_URL: &str = "https://opencode.ai/zen/go/v1";
/// Environment variable holding the OpenCode Go subscription key.
pub const ENV_KEY_VAR: &str = "OPENCODE_GO_API_KEY";
pub use super::opencode::{
    ANTHROPIC_VERSION, MAX_BODY_BYTES, MAX_SSE_LINE_BYTES, MAX_STREAM_BYTES, MAX_STREAM_TEXT_BYTES,
    SESSION_HEADER, USER_AGENT,
};

/// Static model catalog from the Go endpoints table.
///
/// Docs and the live `/models` list drift (live currently also advertises
/// `omen-alpha`, `glm-5.1`, `deepseek-flash`, `minimax-m2.5`, `qwen3.6-plus`,
/// `qwen3.7-max`); the table below is the documented wire mapping and every
/// entry is validated by [`Provider::wire_protocol`]. Unknown ids are rejected
/// rather than guessed so a wrong model fails closed instead of silently
/// hitting an incompatible endpoint.
pub const MODELS: &[ProviderModel] = &[
    // OpenAI Responses shape.
    response_model("grok-4.7", "Grok 4.7"),
    response_model("grok-4.6", "Grok 4.6"),
    response_model("gpt-6-luna", "GPT 6 Luna"),
    response_model("gpt-5.6-luna", "GPT 5.6 Luna"),
    response_model("muse-spark-1.3-contributor", "Muse Spark 1.3 Contributor"),
    response_model("muse-spark-1.2-contributor", "Muse Spark 1.2 Contributor"),
    // Anthropic Messages shape.
    messages_model("claude-haiku-5-5", "Claude Haiku 5.5"),
    messages_model("minimax-m3", "MiniMax M3"),
    messages_model("minimax-m2.7", "MiniMax M2.7"),
    messages_model("qwen3.8-max", "Qwen3.8 Max"),
    messages_model("qwen3.8-flash", "Qwen3.8 Flash"),
    messages_model("qwen3.7-plus", "Qwen3.7 Plus"),
    // OpenAI Chat Completions shape.
    chat_model("glm-5.3-flash", "GLM-5.3-Flash"),
    chat_model("glm-5.3", "GLM-5.3"),
    chat_model("glm-5.2", "GLM-5.2"),
    chat_model("kimi-k3", "Kimi K3"),
    chat_model("kimi-k2.7-code", "Kimi K2.7 Code"),
    chat_model("kimi-k2.6", "Kimi K2.6"),
    chat_model("longcat-2.0", "LongCat-2.0"),
    chat_model("longcat-2.5-preview-free", "LongCat 2.5 Preview Free"),
    chat_model("step-5-preview-free", "Step 5 Preview Free"),
    chat_model("deepseek-v4.1-flash", "DeepSeek V4.1 Flash"),
    chat_model("deepseek-v4-pro", "DeepSeek V4 Pro"),
    chat_model("deepseek-v4-flash", "DeepSeek V4 Flash"),
    chat_model(
        "deepseek-v4-flash-vision-exp",
        "DeepSeek V4 Flash Vision Exp",
    ),
    chat_model("mimo-v2.6-flash", "MiMo-V2.6-Flash"),
    chat_model("mimo-v2.6-pro", "MiMo-V2.6-Pro"),
    chat_model("mimo-v2.5", "MiMo-V2.5"),
    chat_model("mimo-v2.5-pro", "MiMo-V2.5-Pro"),
    chat_model("hy4-preview", "Hy4 preview"),
    chat_model("hy3", "Hy3"),
    chat_model("space-bunny", "Space Bunny"),
];

const fn chat_model(id: &'static str, display_name: &'static str) -> ProviderModel {
    ProviderModel {
        id,
        display_name,
        wire: WireProtocol::OpenAiChatCompletions,
    }
}

const fn response_model(id: &'static str, display_name: &'static str) -> ProviderModel {
    ProviderModel {
        id,
        display_name,
        wire: WireProtocol::OpenAiResponses,
    }
}

const fn messages_model(id: &'static str, display_name: &'static str) -> ProviderModel {
    ProviderModel {
        id,
        display_name,
        wire: WireProtocol::AnthropicMessages,
    }
}

/// OpenCode Go provider (unit struct; one instance per process).
#[derive(Debug, Clone, Copy, Default)]
pub struct OpencodeGoProvider;

impl OpencodeGoProvider {
    /// The single shared instance used by [`super::provider`].
    #[must_use]
    pub fn instance() -> &'static Self {
        static INSTANCE: OpencodeGoProvider = OpencodeGoProvider;
        &INSTANCE
    }

    /// Full inference URL for a wire shape, e.g. `.../v1/chat/completions`.
    #[must_use]
    pub fn inference_url(wire: WireProtocol) -> String {
        format!("{BASE_URL}{}", wire.path())
    }

    /// Full URL of the OpenAI-style model list.
    #[must_use]
    pub fn models_url() -> String {
        format!("{BASE_URL}/models")
    }
}

impl Provider for OpencodeGoProvider {
    fn id(&self) -> ProviderId {
        ProviderId::OpencodeGo
    }

    fn display_name(&self) -> &'static str {
        "OpenCode Go"
    }

    fn base_url(&self) -> &'static str {
        BASE_URL
    }

    fn env_key_var(&self) -> &'static str {
        ENV_KEY_VAR
    }

    fn models(&self) -> &'static [ProviderModel] {
        MODELS
    }
}

/// Authenticated OpenCode Go client, shared across sessions in the native runtime.
/// Deliberately has no `Debug` implementation so credentials cannot be logged.
pub struct OpencodeGoClient {
    pub(super) inner: super::opencode::OpencodeClient,
}

impl OpencodeGoClient {
    /// Validate and normalize a trusted endpoint override. HTTP is loopback-only.
    pub fn validate_base_url(base_url: &str) -> Result<String, ProviderError> {
        super::opencode::validate_base_url(base_url)
    }

    /// Build from an explicit API key, never logged or included in diagnostics.
    pub fn new(api_key: &str) -> Result<Self, ProviderError> {
        Ok(Self {
            inner: super::opencode::OpencodeClient::new(OpencodeGoProvider::instance(), api_key)?,
        })
    }

    /// Build with an explicitly trusted endpoint override. Plain HTTP is
    /// accepted only for localhost or a numeric loopback address.
    pub fn new_with_base_url(api_key: &str, base_url: &str) -> Result<Self, ProviderError> {
        Ok(Self {
            inner: super::opencode::OpencodeClient::new_with_base_url(
                OpencodeGoProvider::instance(),
                api_key,
                Some(base_url),
            )?,
        })
    }

    /// Build from `OPENCODE_GO_API_KEY`. No fallback to another gateway's credential.
    pub fn from_env() -> Result<Self, ProviderError> {
        Ok(Self {
            inner: super::opencode::OpencodeClient::from_env(OpencodeGoProvider::instance())?,
        })
    }

    /// Fetch advertised model IDs. Execution still requires a verified catalog mapping.
    pub async fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        self.inner.list_models().await
    }

    /// Execute one validated, text-only inference turn.
    pub async fn complete(&self, request: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        self.inner.complete(request).await
    }

    /// Stream provisional deltas and return the authoritative terminal response.
    pub async fn complete_streaming(
        &self,
        request: &ChatRequest,
        on_delta: impl FnMut(StreamDelta) + Send,
    ) -> Result<ChatResponse, ProviderError> {
        self.inner.complete_streaming(request, on_delta).await
    }
}

impl ProviderClient for OpencodeGoClient {
    fn capabilities(&self, model: &str) -> super::inference::ModelCapabilities {
        super::inference::capabilities(self.descriptor(), model)
    }

    fn infer<'a>(
        &'a self,
        request: &'a super::inference::InferenceRequest,
        on_event: &'a mut (dyn FnMut(super::inference::ProviderEvent) + Send),
    ) -> ProviderFuture<'a, super::inference::InferenceResponse> {
        Box::pin(self.inner.infer(request, on_event))
    }

    fn descriptor(&self) -> &dyn Provider {
        OpencodeGoProvider::instance()
    }

    fn list_models(&self) -> ProviderFuture<'_, Vec<String>> {
        Box::pin(Self::list_models(self))
    }

    fn complete<'a>(&'a self, request: &'a ChatRequest) -> ProviderFuture<'a, ChatResponse> {
        Box::pin(Self::complete(self, request))
    }

    fn complete_streaming<'a>(
        &'a self,
        request: &'a ChatRequest,
        on_delta: &'a mut (dyn FnMut(StreamDelta) + Send),
    ) -> ProviderFuture<'a, ChatResponse> {
        Box::pin(Self::complete_streaming(self, request, on_delta))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_covers_documented_models() {
        assert_eq!(MODELS.len(), 32);
        let provider = OpencodeGoProvider;
        assert_eq!(provider.models().len(), 32);
        // Spot-check each wire group from the endpoints table.
        for (model, wire) in [
            ("grok-4.7", WireProtocol::OpenAiResponses),
            ("gpt-6-luna", WireProtocol::OpenAiResponses),
            ("muse-spark-1.3-contributor", WireProtocol::OpenAiResponses),
            ("claude-haiku-5-5", WireProtocol::AnthropicMessages),
            ("minimax-m3", WireProtocol::AnthropicMessages),
            ("qwen3.8-flash", WireProtocol::AnthropicMessages),
            ("glm-5.3-flash", WireProtocol::OpenAiChatCompletions),
            ("kimi-k2.7-code", WireProtocol::OpenAiChatCompletions),
            ("deepseek-v4-pro", WireProtocol::OpenAiChatCompletions),
            ("space-bunny", WireProtocol::OpenAiChatCompletions),
        ] {
            assert_eq!(provider.wire_protocol(model).unwrap(), wire, "{model}");
        }
    }

    #[test]
    fn unknown_models_fail_closed() {
        let provider = OpencodeGoProvider;
        for bad in ["", "gpt-4o", "claude-3-5-sonnet", "has space"] {
            assert!(
                matches!(
                    provider.wire_protocol(bad),
                    Err(ProviderError::InvalidModel { .. })
                ),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn endpoint_urls_match_go_shapes() {
        assert_eq!(
            OpencodeGoProvider::inference_url(WireProtocol::OpenAiChatCompletions),
            "https://opencode.ai/zen/go/v1/chat/completions"
        );
        assert_eq!(
            OpencodeGoProvider::inference_url(WireProtocol::OpenAiResponses),
            "https://opencode.ai/zen/go/v1/responses"
        );
        assert_eq!(
            OpencodeGoProvider::inference_url(WireProtocol::AnthropicMessages),
            "https://opencode.ai/zen/go/v1/messages"
        );
        assert_eq!(
            OpencodeGoProvider::models_url(),
            "https://opencode.ai/zen/go/v1/models"
        );
    }

    #[test]
    fn client_rejects_empty_key_without_leaking_it() {
        let err = OpencodeGoClient::new("   ")
            .err()
            .expect("empty key must be rejected")
            .to_string();
        assert!(err.contains(ENV_KEY_VAR));
        assert!(!err.contains("   x"));
    }
}
