//! OpenCode Go provider: one subscription key, three wire shapes.
//!
//! Reference: [OpenCode Go docs](https://opencode.ai/docs/go/). Model ids,
//! wire mapping, and header requirements below were verified against that page
//! and live `https://opencode.ai/zen/go/v1` responses:
//!
//! - `GET /models` and `POST /chat/completions`, `/responses` use
//!   `Authorization: Bearer <OPENCODE_GO_API_KEY>`.
//! - `POST /messages` (Anthropic shape) uses `x-api-key: <key>` plus
//!   `anthropic-version: 2023-06-01`.
//! - Every request identifies this client with its own `User-Agent`
//!   (`slopconductor/<version>`) and sends a stable per-conversation
//!   `x-opencode-session` id for routing and prompt caching.
//!
//! Secrets travel in headers only. Upstream error bodies are excluded from
//! diagnostics.

use serde_json::Value;
use slop_core::provider::ProviderId;

#[cfg(test)]
use super::{TurnOutcome, Usage};
use super::{transport, wire::*};

use super::{
    ChatRequest, ChatResponse, Provider, ProviderClient, ProviderError, ProviderFuture,
    ProviderModel, StreamDelta, WireProtocol,
};

/// Base URL for all OpenCode Go endpoints.
pub const BASE_URL: &str = "https://opencode.ai/zen/go/v1";
/// Environment variable holding the OpenCode Go subscription key.
pub const ENV_KEY_VAR: &str = "OPENCODE_GO_API_KEY";
/// Required version header for the Anthropic Messages shape.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Session header used for routing and prompt caching.
pub const SESSION_HEADER: &str = "x-opencode-session";
/// This client's identifier; Go asks clients not to use a generic SDK name.
pub const USER_AGENT: &str = concat!("slopconductor/", env!("CARGO_PKG_VERSION"));

/// Cap for blocking response bodies, enforced while reading.
pub const MAX_BODY_BYTES: usize = super::wire::MAX_BODY_BYTES;
/// Cap for one SSE line or assembled event payload.
pub const MAX_SSE_LINE_BYTES: usize = super::wire::MAX_SSE_LINE_BYTES;
/// Cap for assembled streamed text.
pub const MAX_STREAM_TEXT_BYTES: usize = super::wire::MAX_STREAM_TEXT_BYTES;
/// Cap for total streamed bytes per request.
pub const MAX_STREAM_BYTES: usize = super::wire::MAX_STREAM_BYTES;

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

/// Authenticated OpenCode Go client. Owns no execution state; one client can
/// serve many sessions from the daemon's shared runtime.
///
/// Deliberately has no `Debug` impl so the key cannot leak through logs.
pub struct OpencodeGoClient {
    http: reqwest::Client,
    api_key: String,
}

impl OpencodeGoClient {
    /// Build from an explicit key (never logged or included in errors).
    pub fn new(api_key: &str) -> Result<Self, ProviderError> {
        if api_key.trim().is_empty() {
            return Err(ProviderError::EmptyApiKey {
                env_var: ENV_KEY_VAR,
            });
        }
        let http = transport::http_client()?;
        Ok(Self {
            http,
            api_key: api_key.to_owned(),
        })
    }

    /// Build from `OPENCODE_GO_API_KEY`.
    pub fn from_env() -> Result<Self, ProviderError> {
        match std::env::var(ENV_KEY_VAR) {
            Ok(key) => Self::new(&key),
            Err(_) => Err(ProviderError::MissingApiKey {
                env_var: ENV_KEY_VAR,
            }),
        }
    }

    fn status_error(&self, status: u16) -> ProviderError {
        ProviderError::UnexpectedStatus {
            provider: ProviderId::OpencodeGo,
            status,
        }
    }

    /// Read a response body with a byte cap enforced while reading.
    async fn read_body_limited(
        &self,
        response: reqwest::Response,
    ) -> Result<String, ProviderError> {
        transport::read_body_limited(ProviderId::OpencodeGo, response).await
    }

    /// Fetch the live model list (`GET /models`). Returns advertised ids.
    pub async fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        let response = self
            .http
            .get(OpencodeGoProvider::models_url())
            .bearer_auth(&self.api_key)
            .header(SESSION_HEADER, "slop-model-discovery")
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(self.status_error(status.as_u16()));
        }
        let body = self.read_body_limited(response).await?;
        parse_models_list(ProviderId::OpencodeGo, &body)
    }

    /// One non-streaming inference turn, dispatching on the model's wire shape.
    pub async fn complete(&self, request: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        let wire = self.validate(request)?;
        match wire {
            WireProtocol::OpenAiChatCompletions => self.complete_chat(request).await,
            WireProtocol::OpenAiResponses => self.complete_responses(request).await,
            WireProtocol::AnthropicMessages => self.complete_messages(request).await,
        }
    }

    /// Streaming inference. Calls `on_delta` per SSE delta and returns the
    /// assembled turn. EOF before the wire's terminal event is an error, never
    /// a silent partial success.
    pub async fn complete_streaming(
        &self,
        request: &ChatRequest,
        mut on_delta: impl FnMut(StreamDelta) + Send,
    ) -> Result<ChatResponse, ProviderError> {
        let wire = self.validate(request)?;
        let (payload, mut req) = match wire {
            WireProtocol::OpenAiChatCompletions => (
                serde_json::json!({
                    "model": request.model,
                    "messages": to_openai_messages(&request.messages),
                    "max_tokens": request.max_tokens,
                    "stream": true,
                }),
                self.http
                    .post(OpencodeGoProvider::inference_url(wire))
                    .bearer_auth(&self.api_key),
            ),
            WireProtocol::OpenAiResponses => (
                serde_json::json!({
                    "model": request.model,
                    "input": to_responses_input(&request.messages),
                    "max_output_tokens": request.max_tokens,
                    "stream": true,
                }),
                self.http
                    .post(OpencodeGoProvider::inference_url(wire))
                    .bearer_auth(&self.api_key),
            ),
            WireProtocol::AnthropicMessages => {
                let (system, messages) = to_anthropic_parts(&request.messages);
                let mut body = serde_json::json!({
                    "model": request.model,
                    "max_tokens": request.max_tokens,
                    "messages": messages,
                    "stream": true,
                });
                if let Some(system) = system {
                    body["system"] = system.into();
                }
                (
                    body,
                    self.http
                        .post(OpencodeGoProvider::inference_url(wire))
                        .header("x-api-key", &self.api_key)
                        .header("anthropic-version", ANTHROPIC_VERSION),
                )
            }
        };
        req = req
            .header(SESSION_HEADER, request.session_id.as_str())
            .header("Accept", "text/event-stream")
            .json(&payload);
        let response = req.send().await?;
        transport::read_streaming_response(
            ProviderId::OpencodeGo,
            wire,
            &request.model,
            response,
            &mut on_delta,
        )
        .await
    }

    fn blocking_from_value(
        &self,
        wire: WireProtocol,
        model: &str,
        value: &Value,
    ) -> Result<ChatResponse, ProviderError> {
        decode_response(ProviderId::OpencodeGo, wire, model, value)
    }

    async fn complete_chat(&self, request: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        let body = self
            .post_json(
                WireProtocol::OpenAiChatCompletions,
                &request.session_id,
                serde_json::json!({
                    "model": request.model,
                    "messages": to_openai_messages(&request.messages),
                    "max_tokens": request.max_tokens,
                }),
            )
            .await?;
        self.blocking_from_value(WireProtocol::OpenAiChatCompletions, &request.model, &body)
    }

    async fn complete_responses(
        &self,
        request: &ChatRequest,
    ) -> Result<ChatResponse, ProviderError> {
        let body = self
            .post_json(
                WireProtocol::OpenAiResponses,
                &request.session_id,
                serde_json::json!({
                    "model": request.model,
                    "input": to_responses_input(&request.messages),
                    "max_output_tokens": request.max_tokens,
                }),
            )
            .await?;
        self.blocking_from_value(WireProtocol::OpenAiResponses, &request.model, &body)
    }

    async fn complete_messages(
        &self,
        request: &ChatRequest,
    ) -> Result<ChatResponse, ProviderError> {
        let (system, messages) = to_anthropic_parts(&request.messages);
        let mut payload = serde_json::json!({
            "model": request.model,
            "max_tokens": request.max_tokens,
            "messages": messages,
        });
        if let Some(system) = system {
            payload["system"] = system.into();
        }
        let body = self
            .post_json(
                WireProtocol::AnthropicMessages,
                &request.session_id,
                payload,
            )
            .await?;
        self.blocking_from_value(WireProtocol::AnthropicMessages, &request.model, &body)
    }

    async fn post_json(
        &self,
        wire: WireProtocol,
        session_id: &str,
        payload: Value,
    ) -> Result<Value, ProviderError> {
        let mut req = self.http.post(OpencodeGoProvider::inference_url(wire));
        req = match wire {
            WireProtocol::AnthropicMessages => req
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", ANTHROPIC_VERSION),
            WireProtocol::OpenAiChatCompletions | WireProtocol::OpenAiResponses => {
                req.bearer_auth(&self.api_key)
            }
        };
        let response = req
            .header(SESSION_HEADER, session_id)
            .json(&payload)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(self.status_error(status.as_u16()));
        }
        let body = self.read_body_limited(response).await?;
        serde_json::from_str(&body).map_err(|_| ProviderError::InvalidResponse {
            provider: ProviderId::OpencodeGo,
            detail: "response body is not JSON",
        })
    }
}

impl ProviderClient for OpencodeGoClient {
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

    #[test]
    fn upstream_status_errors_exclude_body_and_credentials() {
        let secret = "oc_sk_synthetic_secret_123";
        let client = OpencodeGoClient::new(secret).expect("client builds");
        let err = client.status_error(401);
        for rendered in [err.to_string(), format!("{err:?}")] {
            assert!(!rendered.contains(secret), "{rendered}");
            assert!(rendered.contains("401"), "{rendered}");
        }
    }

    #[test]
    fn transport_diagnostics_exclude_raw_error_urls() {
        let secret = "synthetic_query_secret";
        let url = reqwest::Url::parse(&format!("https://example.invalid/?token={secret}")).unwrap();
        let source = reqwest::Client::new()
            .get("invalid relative URL")
            .build()
            .unwrap_err()
            .with_url(url);
        let err = ProviderError::Http(source);
        for rendered in [err.to_string(), format!("{err:?}")] {
            assert!(!rendered.contains(secret), "{rendered}");
            assert!(!rendered.contains("example.invalid"), "{rendered}");
        }
    }

    #[test]
    fn parses_live_chat_shape() {
        let body = serde_json::json!({
            "id": "chatcmpl-test",
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": {"role": "assistant", "content": "Hi there!"},
            }],
            "usage": {"prompt_tokens": 20, "completion_tokens": 5, "total_tokens": 25},
        });
        let (text, usage, outcome) = parse_chat_response(ProviderId::OpencodeGo, &body).unwrap();
        assert_eq!(text, "Hi there!");
        assert_eq!(usage.total_tokens, Some(25));
        assert_eq!(outcome, TurnOutcome::Completed);
    }

    #[test]
    fn chat_length_finish_maps_to_incomplete() {
        let body = serde_json::json!({
            "choices": [{
                "finish_reason": "length",
                "message": {"role": "assistant", "content": null},
            }],
            "usage": {"prompt_tokens": 19, "completion_tokens": 32, "total_tokens": 51},
        });
        let (text, _, outcome) = parse_chat_response(ProviderId::OpencodeGo, &body).unwrap();
        assert_eq!(text, "");
        assert_eq!(
            outcome,
            TurnOutcome::Incomplete {
                reason: "max_tokens".to_owned()
            }
        );
    }

    #[test]
    fn parses_live_responses_shape() {
        let body = serde_json::json!({
            "status": "completed",
            "output": [{
                "type": "message",
                "content": [{"type": "output_text", "text": "ok"}],
            }],
            "usage": {"input_tokens": 12, "output_tokens": 175, "total_tokens": 187},
        });
        let (text, usage, outcome) =
            parse_responses_response(ProviderId::OpencodeGo, &body).unwrap();
        assert_eq!(text, "ok");
        assert_eq!(usage.total_tokens, Some(187));
        assert_eq!(outcome, TurnOutcome::Completed);
    }

    #[test]
    fn incomplete_responses_keep_reason_with_usage() {
        let body = serde_json::json!({
            "status": "incomplete",
            "incomplete_details": {"reason": "max_output_tokens"},
            "output": [],
            "usage": {"input_tokens": 14, "output_tokens": 64, "total_tokens": 78},
        });
        let (text, usage, outcome) =
            parse_responses_response(ProviderId::OpencodeGo, &body).unwrap();
        assert_eq!(text, "");
        assert_eq!(usage.total_tokens, Some(78));
        assert_eq!(
            outcome,
            TurnOutcome::Incomplete {
                reason: "max_output_tokens".to_owned()
            }
        );
    }

    #[test]
    fn failed_responses_are_errors_not_empty_success() {
        let body = serde_json::json!({"status": "failed", "output": []});
        assert!(matches!(
            parse_responses_response(ProviderId::OpencodeGo, &body),
            Err(ProviderError::TurnFailed { .. })
        ));
    }

    #[test]
    fn parses_live_messages_shape() {
        let body = serde_json::json!({
            "stop_reason": "end_turn",
            "content": [{"type": "text", "text": "Hello!"}],
            "usage": {"input_tokens": 18, "output_tokens": 14},
        });
        let (text, usage, outcome) =
            parse_messages_response(ProviderId::OpencodeGo, &body).unwrap();
        assert_eq!(text, "Hello!");
        assert_eq!(usage.total_tokens, Some(32));
        assert_eq!(outcome, TurnOutcome::Completed);
    }

    #[test]
    fn messages_max_tokens_maps_to_incomplete() {
        let body = serde_json::json!({
            "stop_reason": "max_tokens",
            "content": [{"type": "text", "text": "partial"}],
            "usage": {"input_tokens": 18, "output_tokens": 64},
        });
        let (_, _, outcome) = parse_messages_response(ProviderId::OpencodeGo, &body).unwrap();
        assert_eq!(
            outcome,
            TurnOutcome::Incomplete {
                reason: "max_tokens".to_owned()
            }
        );
    }

    #[test]
    fn messages_error_envelope_is_a_failure() {
        let body = serde_json::json!({"type": "error", "error": {"message": "bad"}});
        assert!(matches!(
            parse_messages_response(ProviderId::OpencodeGo, &body),
            Err(ProviderError::TurnFailed { .. })
        ));
    }

    #[test]
    fn parses_live_models_list() {
        let ids = parse_models_list(ProviderId::OpencodeGo,
            r#"{"object":"list","data":[{"id":"glm-5.3-flash"},{"id":"muse-spark-1.3-contributor"}]}"#,
        )
        .unwrap();
        assert_eq!(ids, vec!["glm-5.3-flash", "muse-spark-1.3-contributor"]);
    }

    #[test]
    fn responses_input_preserves_roles() {
        use super::super::{ChatMessage, Role};
        let single = to_responses_input(&[ChatMessage::user("hi")]);
        assert_eq!(single, Value::String("hi".to_owned()));

        let multi = to_responses_input(&[
            ChatMessage {
                role: Role::System,
                content: "be brief".to_owned(),
            },
            ChatMessage::user("hi"),
        ]);
        assert_eq!(
            multi,
            serde_json::json!([
                {"role": "system", "content": "be brief"},
                {"role": "user", "content": "hi"},
            ])
        );
    }

    #[test]
    fn parses_sse_deltas_per_wire_shape() {
        let chat = parse_sse_line(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
            WireProtocol::OpenAiChatCompletions,
        )
        .unwrap();
        assert_eq!(chat.text, "hi");

        let reasoning = parse_sse_line(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"thinking\"}}]}\n\n",
            WireProtocol::OpenAiChatCompletions,
        )
        .unwrap();
        assert_eq!(reasoning.reasoning, "thinking");

        let msg = parse_sse_line(
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
            WireProtocol::AnthropicMessages,
        )
        .unwrap();
        assert_eq!(msg.text, "ok");

        let resp = parse_sse_line(
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n",
            WireProtocol::OpenAiResponses,
        )
        .unwrap();
        assert_eq!(resp.text, "ok");

        assert!(parse_sse_line("data: [DONE]\n", WireProtocol::OpenAiChatCompletions).is_none());
    }

    #[test]
    fn truncated_streams_are_errors_per_shape() {
        // No terminal event in any of these bodies.
        let chat = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n";
        let err = process_sse_body(
            ProviderId::OpencodeGo,
            chat,
            WireProtocol::OpenAiChatCompletions,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ProviderError::InvalidResponse { detail, .. }
            if detail == "stream ended before terminal event"
        ));

        let msg = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n";
        assert!(
            process_sse_body(ProviderId::OpencodeGo, msg, WireProtocol::AnthropicMessages).is_err()
        );

        let resp = "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n";
        assert!(
            process_sse_body(ProviderId::OpencodeGo, resp, WireProtocol::OpenAiResponses).is_err()
        );
    }

    #[test]
    fn in_stream_provider_errors_fail_per_shape() {
        let chat = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: {\"error\":{\"message\":\"boom\"}}\n\n";
        assert!(matches!(
            process_sse_body(
                ProviderId::OpencodeGo,
                chat,
                WireProtocol::OpenAiChatCompletions
            ),
            Err(ProviderError::TurnFailed { .. })
        ));

        let msg = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"x\",\"message\":\"boom\"}}\n\n";
        assert!(matches!(
            process_sse_body(ProviderId::OpencodeGo, msg, WireProtocol::AnthropicMessages),
            Err(ProviderError::TurnFailed { .. })
        ));

        let resp =
            "event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{}}\n\n";
        assert!(matches!(
            process_sse_body(ProviderId::OpencodeGo, resp, WireProtocol::OpenAiResponses),
            Err(ProviderError::TurnFailed { .. })
        ));
    }

    #[test]
    fn complete_native_sequences_resolve() {
        let chat = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n";
        let (text, _, outcome) = process_sse_body(
            ProviderId::OpencodeGo,
            chat,
            WireProtocol::OpenAiChatCompletions,
        )
        .unwrap();
        assert_eq!(text, "ok");
        assert_eq!(outcome, TurnOutcome::Completed);

        let msg = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":18,\"output_tokens\":0}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":4}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let (text, usage, outcome) =
            process_sse_body(ProviderId::OpencodeGo, msg, WireProtocol::AnthropicMessages).unwrap();
        assert_eq!(text, "ok");
        assert_eq!(usage.input_tokens, Some(18));
        assert_eq!(usage.output_tokens, Some(4));
        assert_eq!(usage.total_tokens, Some(22));
        assert_eq!(outcome, TurnOutcome::Completed);

        let resp = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"ok\"}]}],\"usage\":{\"input_tokens\":11,\"output_tokens\":5,\"total_tokens\":16}}}\n\n",
        );
        let (text, usage, outcome) =
            process_sse_body(ProviderId::OpencodeGo, resp, WireProtocol::OpenAiResponses).unwrap();
        assert_eq!(text, "ok");
        assert_eq!(usage.total_tokens, Some(16));
        assert_eq!(outcome, TurnOutcome::Completed);
    }

    #[test]
    fn stream_accumulator_survives_split_unicode() {
        let line = "data: {\"choices\":[{\"delta\":{\"content\":\"héllo\"}}]}\n\n";
        for split in 0..line.len() {
            let (head, tail) = line.as_bytes().split_at(split);
            let mut stream = SseStream::new(ProviderId::OpencodeGo);
            let mut lines = stream.push(head).unwrap();
            lines.extend(stream.push(tail).unwrap());
            lines.extend(stream.finish().unwrap());
            let joined = lines.concat();
            assert!(joined.contains("héllo"), "split at {split}");
        }
    }

    #[test]
    fn oversized_stream_line_is_rejected() {
        let mut stream = SseStream::new(ProviderId::OpencodeGo);
        let big = vec![b'x'; MAX_SSE_LINE_BYTES + 1];
        assert!(matches!(
            stream.push(&big),
            Err(ProviderError::LimitExceeded { .. })
        ));
    }

    #[test]
    fn anthropic_parts_extract_system() {
        use super::super::{ChatMessage, Role};
        let (system, messages) = to_anthropic_parts(&[
            ChatMessage {
                role: Role::System,
                content: "be brief".to_owned(),
            },
            ChatMessage::user("hi"),
        ]);
        assert_eq!(system.as_deref(), Some("be brief"));
        assert_eq!(messages.len(), 1);
    }

    #[test]
    fn missing_usage_is_unknown_and_reported_zero_is_known() {
        assert_eq!(parse_openai_usage(&serde_json::json!({})), Usage::default());
        let zero = parse_usage(&serde_json::json!({"input_tokens": 0, "output_tokens": 0}));
        assert_eq!(zero.input_tokens, Some(0));
        assert_eq!(zero.output_tokens, Some(0));
        assert_eq!(zero.total_tokens, Some(0));
        assert_eq!(zero.total_source, Some(super::super::UsageSource::Derived));
        let partial = parse_usage(&serde_json::json!({"output_tokens": 8}));
        assert_eq!(partial.input_tokens, None);
        assert_eq!(partial.total_tokens, None);
        let overflowing = Usage::from_reported(Some(u64::MAX), Some(1), None);
        assert_eq!(overflowing.total_tokens, None);
    }

    #[test]
    fn cumulative_usage_preserves_missing_fields_and_reported_totals() {
        let mut usage = Usage::default();
        for value in [
            serde_json::json!({"type": "message_start", "message": {"usage": {"input_tokens": 18, "output_tokens": 1}}}),
            serde_json::json!({"usage": {"output_tokens": 4}}),
            serde_json::json!({"usage": {"output_tokens": 4}}),
        ] {
            merge_sse_usage(&mut usage, &value);
        }
        assert_eq!(usage.total_tokens, Some(22));
        merge_sse_usage(
            &mut usage,
            &serde_json::json!({"usage": {"output_tokens": 0}}),
        );
        assert_eq!(usage.input_tokens, Some(18));
        assert_eq!(usage.output_tokens, Some(0));
        assert_eq!(usage.total_tokens, Some(18));
        merge_sse_usage(
            &mut usage,
            &serde_json::json!({"usage": {"total_tokens": 99}}),
        );
        merge_sse_usage(&mut usage, &serde_json::json!({"usage": {}}));
        assert_eq!(usage.total_tokens, Some(99));
        assert_eq!(
            usage.total_source,
            Some(super::super::UsageSource::Reported)
        );
    }

    #[test]
    fn blocking_responses_require_explicit_terminal_evidence() {
        assert!(
            parse_chat_response(
                ProviderId::OpencodeGo,
                &serde_json::json!({
                    "choices": [{"message": {"content": "plausible"}}]
                })
            )
            .is_err()
        );
        assert!(
            parse_messages_response(
                ProviderId::OpencodeGo,
                &serde_json::json!({
                    "content": [{"type": "text", "text": "plausible"}]
                })
            )
            .is_err()
        );
        assert!(
            parse_responses_response(ProviderId::OpencodeGo, &serde_json::json!({"output": []}))
                .is_err()
        );
        assert!(chat_outcome(ProviderId::OpencodeGo, Some("unrecognized")).is_err());
        assert!(messages_outcome(ProviderId::OpencodeGo, Some("unrecognized")).is_err());
    }

    #[test]
    fn incomplete_stream_outcomes_survive_trailing_markers() {
        let chat = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":64}}\n\n",
            "data: [DONE]\n\n",
        );
        let (text, usage, outcome) = process_sse_body(
            ProviderId::OpencodeGo,
            chat,
            WireProtocol::OpenAiChatCompletions,
        )
        .unwrap();
        assert_eq!(text, "partial");
        assert_eq!(usage.total_tokens, Some(74));
        assert_eq!(outcome, incomplete(Some("max_tokens")));

        let messages = concat!(
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let (_, _, outcome) = process_sse_body(
            ProviderId::OpencodeGo,
            messages,
            WireProtocol::AnthropicMessages,
        )
        .unwrap();
        assert_eq!(outcome, incomplete(Some("max_tokens")));
        let responses = "data: {\"type\":\"response.incomplete\",\"response\":{\"status\":\"incomplete\",\"output\":[]}}\n\n";
        let (_, _, outcome) = process_sse_body(
            ProviderId::OpencodeGo,
            responses,
            WireProtocol::OpenAiResponses,
        )
        .unwrap();
        assert_eq!(outcome, incomplete(None));
    }

    #[test]
    fn bare_end_markers_do_not_prove_success_on_any_wire() {
        for wire in [
            WireProtocol::OpenAiChatCompletions,
            WireProtocol::AnthropicMessages,
            WireProtocol::OpenAiResponses,
        ] {
            assert!(process_sse_body(ProviderId::OpencodeGo, "data: [DONE]\n\n", wire).is_err());
        }
    }

    #[test]
    fn malformed_events_and_post_terminal_content_are_rejected() {
        let terminal = "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n";
        let malformed = format!("data: {{malformed}}\n\n{terminal}");
        assert!(
            process_sse_body(
                ProviderId::OpencodeGo,
                &malformed,
                WireProtocol::OpenAiChatCompletions
            )
            .is_err()
        );
        let late_content =
            format!("{terminal}data: {{\"choices\":[{{\"delta\":{{\"content\":\"late\"}}}}]}}\n\n");
        assert!(
            process_sse_body(
                ProviderId::OpencodeGo,
                &late_content,
                WireProtocol::OpenAiChatCompletions
            )
            .is_err()
        );
        let contradictory = format!(
            "{terminal}data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"length\"}}]}}\n\n"
        );
        assert!(
            process_sse_body(
                ProviderId::OpencodeGo,
                &contradictory,
                WireProtocol::OpenAiChatCompletions
            )
            .is_err()
        );
    }

    #[test]
    fn multiline_sse_events_wait_for_delimiter_and_reject_bad_utf8() {
        let body = concat!(
            "event: chunk\r\n",
            "data: {\"choices\":\r\n",
            "data: [{\"delta\":{\"content\":\"héllo\"},\"finish_reason\":\"stop\"}]}\r\n\r\n",
        );
        for split in 0..body.len() {
            let mut stream = SseStream::new(ProviderId::OpencodeGo);
            let mut frames = stream.push(&body.as_bytes()[..split]).unwrap();
            frames.extend(stream.push(&body.as_bytes()[split..]).unwrap());
            frames.extend(stream.finish().unwrap());
            assert_eq!(frames.len(), 1);
            let mut fold = StreamFold::new(ProviderId::OpencodeGo);
            fold.feed(&frames[0], WireProtocol::OpenAiChatCompletions, &mut |_| {})
                .unwrap();
            assert_eq!(fold.text, "héllo");
            assert_eq!(fold.terminal, Some(TurnOutcome::Completed));
        }
        let mut invalid = SseStream::new(ProviderId::OpencodeGo);
        assert!(invalid.push(b"data: \xff\n\n").is_err());
        assert!(
            process_sse_body(
                ProviderId::OpencodeGo,
                "data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n",
                WireProtocol::OpenAiChatCompletions
            )
            .is_err()
        );
    }

    #[test]
    fn all_sse_line_endings_survive_chunk_boundaries() {
        for ending in ["\n", "\r\n", "\r"] {
            let body = format!(
                "data: {{\"choices\":[{{\"delta\":{{\"content\":\"ok\"}},\"finish_reason\":\"stop\"}}]}}{ending}{ending}"
            );
            for split in 0..=body.len() {
                let mut stream = SseStream::new(ProviderId::OpencodeGo);
                let mut events = stream.push(&body.as_bytes()[..split]).unwrap();
                events.extend(stream.push(&body.as_bytes()[split..]).unwrap());
                events.extend(stream.finish().unwrap());
                assert_eq!(events.len(), 1, "ending {ending:?}, split {split}");
                let mut fold = StreamFold::new(ProviderId::OpencodeGo);
                fold.feed(&events[0], WireProtocol::OpenAiChatCompletions, &mut |_| {})
                    .unwrap();
                assert_eq!(fold.text, "ok");
                assert_eq!(fold.terminal, Some(TurnOutcome::Completed));
            }
        }
    }

    #[test]
    fn structured_content_is_rejected_instead_of_silently_dropped() {
        let chat = serde_json::json!({
            "choices": [{"finish_reason": "stop", "message": {"content": "text", "tool_calls": [{"id": "call-1"}]}}]
        });
        assert!(matches!(
            parse_chat_response(ProviderId::OpencodeGo, &chat),
            Err(ProviderError::UnsupportedCapability { .. })
        ));
        for kind in ["function_call", "web_search_call", "unrecognized"] {
            let response = serde_json::json!({"status": "completed", "output": [{"type": kind}]});
            assert!(matches!(
                parse_responses_response(ProviderId::OpencodeGo, &response),
                Err(ProviderError::UnsupportedCapability { .. })
            ));
        }
        let continuation = serde_json::json!({"status": "completed", "output": [{"type": "reasoning", "encrypted_content": "opaque"}]});
        assert!(matches!(
            parse_responses_response(ProviderId::OpencodeGo, &continuation),
            Err(ProviderError::UnsupportedCapability { .. })
        ));
        let messages = serde_json::json!({"stop_reason": "end_turn", "content": [{"type": "tool_use", "id": "call-1"}]});
        assert!(matches!(
            parse_messages_response(ProviderId::OpencodeGo, &messages),
            Err(ProviderError::UnsupportedCapability { .. })
        ));
        let stream = "data: {\"type\":\"content_block_start\",\"content_block\":{\"type\":\"tool_use\"}}\n\n";
        assert!(matches!(
            process_sse_body(
                ProviderId::OpencodeGo,
                stream,
                WireProtocol::AnthropicMessages
            ),
            Err(ProviderError::UnsupportedCapability { .. })
        ));
    }

    #[test]
    fn nested_stream_content_is_validated_before_it_can_be_discarded() {
        let cases = [
            (
                WireProtocol::AnthropicMessages,
                serde_json::json!({
                    "type": "message_start", "message": {"content": [{"type": "tool_use"}]}
                }),
            ),
            (
                WireProtocol::OpenAiResponses,
                serde_json::json!({
                    "type": "response.output_item.done", "item": {"type": "message", "content": [{"type": "refusal", "refusal": "declined"}]}
                }),
            ),
            (
                WireProtocol::OpenAiResponses,
                serde_json::json!({
                    "type": "response.content_part.added", "part": {"type": "output_text"}
                }),
            ),
        ];
        for (wire, value) in cases {
            let mut fold = StreamFold::new(ProviderId::OpencodeGo);
            let mut emitted = false;
            assert!(
                fold.feed(&format!("data: {value}"), wire, &mut |_| emitted = true)
                    .is_err()
            );
            assert!(!emitted);
        }
    }

    #[test]
    fn responses_terminal_record_is_authoritative_and_keeps_model_identity() {
        let client = OpencodeGoClient::new("synthetic-key").unwrap();
        let response = serde_json::json!({
            "model": "resolved-alias",
            "status": "completed",
            "output": [{"type": "message", "content": [{"type": "output_text", "text": "canonical"}]}]
        });
        let parsed = client
            .blocking_from_value(WireProtocol::OpenAiResponses, "requested-alias", &response)
            .unwrap();
        assert_eq!(parsed.model, "requested-alias");
        assert_eq!(parsed.resolved_model.as_deref(), Some("resolved-alias"));
        let mut fold = StreamFold::new(ProviderId::OpencodeGo);
        fold.feed(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"provisional\"}",
            WireProtocol::OpenAiResponses,
            &mut |_| {},
        )
        .unwrap();
        let terminal = format!(
            "data: {}",
            serde_json::json!({"type": "response.completed", "response": response})
        );
        fold.feed(&terminal, WireProtocol::OpenAiResponses, &mut |_| {})
            .unwrap();
        assert_eq!(fold.text, "canonical");
        assert_eq!(fold.resolved_model.as_deref(), Some("resolved-alias"));
        assert_eq!(fold.usage, Usage::default());
    }
}
