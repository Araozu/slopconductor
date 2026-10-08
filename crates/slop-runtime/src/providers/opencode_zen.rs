//! OpenCode Zen pay-as-you-go gateway over the shared text-only provider surface.
//!
//! Model IDs and endpoint mappings were checked on 2026-10-08 against
//! <https://docs.opencode.ai/docs/zen/#endpoints>. Zen's Gemini (Google) and
//! Jev (System One) wire formats are not implemented; live listing may advertise
//! them, but execution rejects IDs outside this verified three-wire catalog.
//! In particular, MiniMax and Qwen3.8 Max use Chat here and Messages on Go.

use slop_core::provider::ProviderId;

use super::{
    ChatRequest, ChatResponse, Provider, ProviderClient, ProviderError, ProviderFuture,
    ProviderModel, StreamDelta, WireProtocol,
};

/// Base URL for the OpenCode Zen gateway (separate from the Go subscription).
pub const BASE_URL: &str = "https://opencode.ai/zen/v1";
/// Owner-supplied Zen API key. No Go credential is reused implicitly.
pub const ENV_KEY_VAR: &str = "OPENCODE_ZEN_API_KEY";

pub use super::opencode::{
    ANTHROPIC_VERSION, MAX_BODY_BYTES, MAX_SSE_LINE_BYTES, MAX_STREAM_BYTES, MAX_STREAM_TEXT_BYTES,
    SESSION_HEADER, USER_AGENT,
};

/// Static, documented execution mappings; live advertised IDs are not proof
/// that their wire formats are supported. Unknown IDs fail before dispatch.
pub const MODELS: &[ProviderModel] = &[
    // OpenAI Responses shape.
    response_model("gpt-6-astra", "GPT 6 Astra"),
    response_model("gpt-6-sol", "GPT 6 Sol"),
    response_model("gpt-6.1-sol", "GPT 6.1 Sol"),
    response_model("gpt-6-luna", "GPT 6 Luna"),
    response_model("gpt-5.6-sol", "GPT 5.6 Sol"),
    response_model("gpt-5.6-terra", "GPT 5.6 Terra"),
    response_model("gpt-5.6-luna", "GPT 5.6 Luna"),
    response_model("gpt-5.5", "GPT 5.5"),
    response_model("gpt-5.5-pro", "GPT 5.5 Pro"),
    response_model("gpt-5.4", "GPT 5.4"),
    response_model("gpt-5.4-pro", "GPT 5.4 Pro"),
    response_model("gpt-5.4-mini", "GPT 5.4 Mini"),
    response_model("gpt-5.4-nano", "GPT 5.4 Nano"),
    response_model("gpt-5.3-codex", "GPT 5.3 Codex"),
    response_model("gpt-5.3-codex-spark", "GPT 5.3 Codex Spark"),
    response_model("gpt-5.2", "GPT 5.2"),
    response_model("gpt-5.2-codex", "GPT 5.2 Codex"),
    response_model("gpt-5.1", "GPT 5.1"),
    response_model("gpt-5.1-codex", "GPT 5.1 Codex"),
    response_model("gpt-5.1-codex-max", "GPT 5.1 Codex Max"),
    response_model("gpt-5.1-codex-mini", "GPT 5.1 Codex Mini"),
    response_model("gpt-5", "GPT 5"),
    response_model("gpt-5-codex", "GPT 5 Codex"),
    response_model("gpt-5-nano", "GPT 5 Nano"),
    response_model("grok-4.7", "Grok 4.7"),
    response_model("grok-4.6", "Grok 4.6"),
    response_model("grok-4.5", "Grok 4.5"),
    response_model("grok-build-0.1", "Grok Build 0.1"),
    response_model("muse-spark-1.3", "Muse Spark 1.3"),
    response_model("muse-spark-1.2", "Muse Spark 1.2"),
    response_model(
        "muse-spark-1.3-contributor-free",
        "Muse Spark 1.3 Contributor Free",
    ),
    // Anthropic Messages shape.
    messages_model("claude-fable-5-1", "Claude Fable 5.1"),
    messages_model("claude-fable-5", "Claude Fable 5"),
    messages_model("claude-opus-5-5", "Claude Opus 5.5"),
    messages_model("claude-opus-5", "Claude Opus 5"),
    messages_model("claude-opus-4-8", "Claude Opus 4.8"),
    messages_model("claude-opus-4-7", "Claude Opus 4.7"),
    messages_model("claude-opus-4-6", "Claude Opus 4.6"),
    messages_model("claude-opus-4-5", "Claude Opus 4.5"),
    messages_model("claude-sonnet-5-5", "Claude Sonnet 5.5"),
    messages_model("claude-sonnet-5", "Claude Sonnet 5"),
    messages_model("claude-sonnet-4-6", "Claude Sonnet 4.6"),
    messages_model("claude-sonnet-4-5", "Claude Sonnet 4.5"),
    messages_model("claude-haiku-5-5", "Claude Haiku 5.5"),
    messages_model("claude-haiku-4-5", "Claude Haiku 4.5"),
    messages_model("qwen3.8-flash", "Qwen3.8 Flash"),
    messages_model("qwen3.7-max", "Qwen3.7 Max"),
    messages_model("qwen3.7-plus", "Qwen3.7 Plus"),
    messages_model("qwen3.6-plus", "Qwen3.6 Plus"),
    messages_model("qwen3.5-plus", "Qwen3.5 Plus"),
    // OpenAI Chat Completions shape.
    chat_model("qwen3.8-max", "Qwen3.8 Max"),
    chat_model("deepseek-v4.1-flash", "DeepSeek V4.1 Flash"),
    chat_model("deepseek-v4-pro", "DeepSeek V4 Pro"),
    chat_model("deepseek-v4-flash", "DeepSeek V4 Flash"),
    chat_model(
        "deepseek-v4-flash-vision-exp",
        "DeepSeek V4 Flash Vision Exp",
    ),
    chat_model("minimax-m3", "MiniMax M3"),
    chat_model("minimax-m2.7", "MiniMax M2.7"),
    chat_model("minimax-m2.5", "MiniMax M2.5"),
    chat_model("glm-5.3-flash", "GLM 5.3 Flash"),
    chat_model("glm-5.3", "GLM 5.3"),
    chat_model("glm-5.2", "GLM 5.2"),
    chat_model("glm-5.1", "GLM 5.1"),
    chat_model("glm-5", "GLM 5"),
    chat_model("kimi-k2.5", "Kimi K2.5"),
    chat_model("kimi-k2.6", "Kimi K2.6"),
    chat_model("kimi-k2.7-code", "Kimi K2.7 Code"),
    chat_model("kimi-k3", "Kimi K3"),
    chat_model("mistral-large-4", "Mistral Large 4"),
    chat_model("big-pickle", "Big Pickle"),
    chat_model("space-bunny-free", "Space Bunny Free"),
    chat_model("longcat-2.5-preview-free", "LongCat 2.5 Preview Free"),
    chat_model("step-5-preview-free", "Step 5 Preview Free"),
    chat_model("exo-free", "Exo Free"),
    chat_model("mimo-v2.6-flash-free", "MiMo-V2.6-Flash Free"),
    chat_model("mimo-v2.5-free", "MiMo-V2.5 Free"),
    chat_model("ling-3.1-flash-free", "Ling 3.1 Flash Free"),
    chat_model("ling-3.0-flash-fin-free", "Ling 3.0 Flash Fin Free"),
    chat_model("nemotron-3-ultra-free", "Nemotron 3 Ultra Free"),
    chat_model("nemotron-3.5-lightning-free", "Nemotron 3.5 Lightning Free"),
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

/// Compiled-in OpenCode Zen metadata, with no credentials or execution state.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpencodeZenProvider;

impl OpencodeZenProvider {
    #[must_use]
    pub fn instance() -> &'static Self {
        static INSTANCE: OpencodeZenProvider = OpencodeZenProvider;
        &INSTANCE
    }

    #[must_use]
    pub fn inference_url(wire: WireProtocol) -> String {
        format!("{BASE_URL}{}", wire.path())
    }

    #[must_use]
    pub fn models_url() -> String {
        format!("{BASE_URL}/models")
    }
}

impl Provider for OpencodeZenProvider {
    fn id(&self) -> ProviderId {
        ProviderId::OpencodeZen
    }
    fn display_name(&self) -> &'static str {
        "OpenCode Zen"
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

/// Authenticated OpenCode Zen client, shared across sessions in the native runtime.
/// Deliberately has no `Debug` implementation so credentials cannot be logged.
pub struct OpencodeZenClient {
    pub(super) inner: super::opencode::OpencodeClient,
}

impl OpencodeZenClient {
    /// Build from an explicit API key, never logged or included in diagnostics.
    pub fn new(api_key: &str) -> Result<Self, ProviderError> {
        Ok(Self {
            inner: super::opencode::OpencodeClient::new(OpencodeZenProvider::instance(), api_key)?,
        })
    }

    /// Build from `OPENCODE_ZEN_API_KEY`. No fallback to another gateway's credential.
    pub fn from_env() -> Result<Self, ProviderError> {
        Ok(Self {
            inner: super::opencode::OpencodeClient::from_env(OpencodeZenProvider::instance())?,
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

impl ProviderClient for OpencodeZenClient {
    fn descriptor(&self) -> &dyn Provider {
        OpencodeZenProvider::instance()
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
    use std::collections::HashSet;

    use super::*;
    use slop_core::provider::is_valid_model_id;

    #[test]
    fn catalog_has_unique_valid_ids_and_documented_wire_shapes() {
        let provider = OpencodeZenProvider;
        let mut ids = HashSet::new();
        for model in provider.models() {
            assert!(is_valid_model_id(model.id));
            assert!(ids.insert(model.id), "duplicate model {}", model.id);
            assert_eq!(provider.wire_protocol(model.id).unwrap(), model.wire);
        }
        for (model, wire) in [
            ("gpt-6.1-sol", WireProtocol::OpenAiResponses),
            ("gpt-5.3-codex", WireProtocol::OpenAiResponses),
            ("grok-4.7", WireProtocol::OpenAiResponses),
            (
                "muse-spark-1.3-contributor-free",
                WireProtocol::OpenAiResponses,
            ),
            ("claude-sonnet-4-6", WireProtocol::AnthropicMessages),
            ("qwen3.8-flash", WireProtocol::AnthropicMessages),
            ("glm-5.3-flash", WireProtocol::OpenAiChatCompletions),
            ("big-pickle", WireProtocol::OpenAiChatCompletions),
            ("minimax-m3", WireProtocol::OpenAiChatCompletions),
            ("qwen3.8-max", WireProtocol::OpenAiChatCompletions),
        ] {
            assert_eq!(provider.wire_protocol(model).unwrap(), wire, "{model}");
        }
        for model in ["minimax-m3", "minimax-m2.7", "qwen3.8-max"] {
            assert_eq!(
                super::super::OpencodeGoProvider
                    .wire_protocol(model)
                    .unwrap(),
                WireProtocol::AnthropicMessages
            );
            assert_eq!(
                provider.wire_protocol(model).unwrap(),
                WireProtocol::OpenAiChatCompletions
            );
        }
    }

    #[test]
    fn unmapped_and_unsupported_wire_formats_fail_closed() {
        for model in [
            "",
            "has space",
            "unknown-model",
            "gemini-3-flash",
            "jev-1.13",
            "space-bunny",
        ] {
            assert!(matches!(
                OpencodeZenProvider.wire_protocol(model),
                Err(ProviderError::InvalidModel {
                    provider: ProviderId::OpencodeZen,
                    ..
                })
            ));
        }
    }

    #[test]
    fn metadata_and_endpoints_are_separate_from_go() {
        let provider = OpencodeZenProvider;
        assert_eq!(provider.id().as_str(), "opencode-zen");
        assert_eq!(provider.display_name(), "OpenCode Zen");
        assert_eq!(provider.env_key_var(), "OPENCODE_ZEN_API_KEY");
        assert_eq!(provider.base_url(), "https://opencode.ai/zen/v1");
        assert_eq!(
            OpencodeZenProvider::models_url(),
            "https://opencode.ai/zen/v1/models"
        );
        for (wire, endpoint) in [
            (
                WireProtocol::OpenAiChatCompletions,
                "https://opencode.ai/zen/v1/chat/completions",
            ),
            (
                WireProtocol::OpenAiResponses,
                "https://opencode.ai/zen/v1/responses",
            ),
            (
                WireProtocol::AnthropicMessages,
                "https://opencode.ai/zen/v1/messages",
            ),
        ] {
            assert_eq!(OpencodeZenProvider::inference_url(wire), endpoint);
        }
        let err = OpencodeZenClient::new(" \n ").err().unwrap();
        assert!(matches!(
            err,
            ProviderError::EmptyApiKey {
                env_var: ENV_KEY_VAR
            }
        ));
    }
}
