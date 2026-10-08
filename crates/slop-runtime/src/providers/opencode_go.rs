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
//! Secrets travel in headers only. Upstream body excerpts stored in errors are
//! sanitized against the configured key and truncated at a character boundary.

use std::time::Duration;

use serde_json::Value;
use slop_core::provider::ProviderId;

use super::{
    ChatRequest, ChatResponse, Provider, ProviderError, ProviderModel, Role, StreamDelta,
    TurnOutcome, Usage, WireProtocol, is_valid_session_id,
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
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
/// Cap for one SSE line; longer lines are rejected instead of buffered.
pub const MAX_SSE_LINE_BYTES: usize = 256 * 1024;
/// Cap for assembled streamed text.
pub const MAX_STREAM_TEXT_BYTES: usize = 1024 * 1024;
/// Cap for total streamed bytes per request.
pub const MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;
/// Visible length of sanitized upstream excerpts in errors.
pub const ERROR_EXCERPT_CHARS: usize = 500;

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
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .build()?;
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

    /// Replace occurrences of the configured key so reflected credentials
    /// cannot escape through error text.
    fn redact(&self, text: &str) -> String {
        text.replace(&self.api_key, "[REDACTED]")
    }

    fn status_error(&self, status: u16, body: &str) -> ProviderError {
        ProviderError::UnexpectedStatus {
            provider: ProviderId::OpencodeGo,
            status,
            body: truncate_text(&self.redact(body), ERROR_EXCERPT_CHARS),
        }
    }

    /// Read a response body with a byte cap enforced while reading.
    async fn read_body_limited(
        &self,
        mut response: reqwest::Response,
    ) -> Result<String, ProviderError> {
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if bytes.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
                return Err(ProviderError::LimitExceeded {
                    detail: "response body exceeds byte bound",
                });
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(String::from_utf8_lossy(&bytes).into_owned())
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
        let body = self.read_body_limited(response).await?;
        if !status.is_success() {
            return Err(self.status_error(status.as_u16(), &body));
        }
        parse_models_list(&body)
    }

    /// One blocking inference turn, dispatching on the model's wire shape.
    pub async fn complete(&self, request: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        request.validate()?;
        let wire = OpencodeGoProvider.wire_protocol(&request.model)?;
        validate_session(&request.session_id)?;
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
        mut on_delta: impl FnMut(StreamDelta),
    ) -> Result<ChatResponse, ProviderError> {
        request.validate()?;
        let wire = OpencodeGoProvider.wire_protocol(&request.model)?;
        validate_session(&request.session_id)?;
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
        let mut response = req.send().await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = self.read_body_limited(response).await?;
            return Err(self.status_error(status, &body));
        }
        if !is_event_stream(&response) {
            // The gateway answered 200 with plain JSON instead of SSE (it
            // ignored `stream: true`). Parse it as a blocking turn rather
            // than failing or misreading it as deltas.
            let body = self.read_body_limited(response).await?;
            let value: Value =
                serde_json::from_str(&body).map_err(|_| ProviderError::InvalidResponse {
                    provider: ProviderId::OpencodeGo,
                    detail: "response body is not JSON",
                })?;
            return self.blocking_from_value(wire, &request.model, &value);
        }
        let mut stream = SseStream::new();
        let mut fold = StreamFold::default();
        while let Some(chunk) = response.chunk().await? {
            for line in stream.push(&chunk)? {
                fold.feed(&line, wire, &mut on_delta)?;
            }
        }
        for line in stream.finish()? {
            fold.feed(&line, wire, &mut on_delta)?;
        }
        let outcome = fold.terminal.ok_or(ProviderError::InvalidResponse {
            provider: ProviderId::OpencodeGo,
            detail: "stream ended before terminal event",
        })?;
        Ok(ChatResponse {
            model: request.model.clone(),
            text: fold.text,
            usage: fold.usage,
            wire,
            outcome,
        })
    }

    fn blocking_from_value(
        &self,
        wire: WireProtocol,
        model: &str,
        value: &Value,
    ) -> Result<ChatResponse, ProviderError> {
        let (text, usage, outcome) = match wire {
            WireProtocol::OpenAiChatCompletions => parse_chat_response(value)?,
            WireProtocol::OpenAiResponses => parse_responses_response(value)?,
            WireProtocol::AnthropicMessages => parse_messages_response(value)?,
        };
        Ok(ChatResponse {
            model: model.to_owned(),
            text,
            usage,
            wire,
            outcome,
        })
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
        let (text, usage, outcome) = parse_chat_response(&body)?;
        Ok(ChatResponse {
            model: request.model.clone(),
            text,
            usage,
            wire: WireProtocol::OpenAiChatCompletions,
            outcome,
        })
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
        let (text, usage, outcome) = parse_responses_response(&body)?;
        Ok(ChatResponse {
            model: request.model.clone(),
            text,
            usage,
            wire: WireProtocol::OpenAiResponses,
            outcome,
        })
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
        let (text, usage, outcome) = parse_messages_response(&body)?;
        Ok(ChatResponse {
            model: request.model.clone(),
            text,
            usage,
            wire: WireProtocol::AnthropicMessages,
            outcome,
        })
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
        let body = self.read_body_limited(response).await?;
        if !status.is_success() {
            return Err(self.status_error(status.as_u16(), &body));
        }
        serde_json::from_str(&body).map_err(|_| ProviderError::InvalidResponse {
            provider: ProviderId::OpencodeGo,
            detail: "response body is not JSON",
        })
    }
}

fn validate_session(session_id: &str) -> Result<(), ProviderError> {
    if !is_valid_session_id(session_id) {
        return Err(ProviderError::InvalidRequest("session_id is invalid"));
    }
    Ok(())
}

fn is_event_stream(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|content_type| content_type.contains("text/event-stream"))
}

/// Byte accumulator for SSE that never splits UTF-8 across decodes.
///
/// A `\n` byte (0x0A) cannot appear inside a multi-byte UTF-8 sequence, so
/// every `\n`-terminated prefix is a valid character boundary. Only complete
/// lines are decoded; the unterminated tail stays buffered as bytes.
struct SseStream {
    pending: Vec<u8>,
    total: usize,
}

impl SseStream {
    fn new() -> Self {
        Self {
            pending: Vec::new(),
            total: 0,
        }
    }

    /// Feed one network chunk; returns newly completed lines.
    fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>, ProviderError> {
        self.total = self.total.saturating_add(chunk.len());
        if self.total > MAX_STREAM_BYTES {
            return Err(ProviderError::LimitExceeded {
                detail: "stream exceeds byte bound",
            });
        }
        self.pending.extend_from_slice(chunk);
        if self.pending.len() > MAX_STREAM_BYTES {
            return Err(ProviderError::LimitExceeded {
                detail: "stream exceeds byte bound",
            });
        }
        self.drain_lines()
    }

    /// Drain complete lines, rejecting overlong lines instead of buffering
    /// them without bound.
    fn drain_lines(&mut self) -> Result<Vec<String>, ProviderError> {
        let mut lines = Vec::new();
        while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
            let raw: Vec<u8> = self.pending.drain(..=end).collect();
            if raw.len() > MAX_SSE_LINE_BYTES {
                return Err(ProviderError::LimitExceeded {
                    detail: "stream line exceeds byte bound",
                });
            }
            lines.push(String::from_utf8_lossy(&raw).into_owned());
        }
        if self.pending.len() > MAX_SSE_LINE_BYTES {
            return Err(ProviderError::LimitExceeded {
                detail: "stream line exceeds byte bound",
            });
        }
        Ok(lines)
    }

    /// Flush the unterminated tail after EOF.
    fn finish(&mut self) -> Result<Vec<String>, ProviderError> {
        if self.pending.is_empty() {
            return Ok(Vec::new());
        }
        if self.pending.len() > MAX_SSE_LINE_BYTES {
            return Err(ProviderError::LimitExceeded {
                detail: "stream line exceeds byte bound",
            });
        }
        let tail = std::mem::take(&mut self.pending);
        Ok(vec![String::from_utf8_lossy(&tail).into_owned()])
    }
}

/// Translate neutral messages to OpenAI Chat `messages`.
fn to_openai_messages(messages: &[super::ChatMessage]) -> Vec<Value> {
    messages
        .iter()
        .map(|m| {
            serde_json::json!({
                "role": m.role.as_str(),
                "content": m.content,
            })
        })
        .collect()
}

/// Translate neutral messages to a Responses `input` value.
///
/// A single user turn passes through as a plain string (the shape verified
/// live). Anything with roles the string form cannot express — system
/// instructions, assistant history, multi-turn context — becomes the
/// structured message array so role boundaries survive.
fn to_responses_input(messages: &[super::ChatMessage]) -> Value {
    if messages.len() == 1 && messages[0].role == Role::User {
        return Value::String(messages[0].content.clone());
    }
    Value::Array(
        messages
            .iter()
            .map(|m| {
                serde_json::json!({
                    "role": m.role.as_str(),
                    "content": m.content,
                })
            })
            .collect(),
    )
}

/// Split neutral messages into Anthropic `system` plus `messages`.
///
/// Anthropic takes at most one leading system prompt outside `messages`;
/// system-role entries collapse into it and remaining turns pass through.
fn to_anthropic_parts(messages: &[super::ChatMessage]) -> (Option<String>, Vec<Value>) {
    let mut system = Vec::new();
    let mut rest = Vec::new();
    for m in messages {
        match m.role {
            Role::System => system.push(m.content.clone()),
            Role::User | Role::Assistant => rest.push(serde_json::json!({
                "role": m.role.as_str(),
                "content": m.content,
            })),
        }
    }
    let system = if system.is_empty() {
        None
    } else {
        Some(system.join("\n\n"))
    };
    (system, rest)
}

/// Parse the OpenAI-style model list into advertised ids.
fn parse_models_list(body: &str) -> Result<Vec<String>, ProviderError> {
    let value: Value = serde_json::from_str(body).map_err(|_| ProviderError::InvalidResponse {
        provider: ProviderId::OpencodeGo,
        detail: "model list is not JSON",
    })?;
    let data =
        value
            .get("data")
            .and_then(Value::as_array)
            .ok_or(ProviderError::InvalidResponse {
                provider: ProviderId::OpencodeGo,
                detail: "model list has no data array",
            })?;
    let mut ids = Vec::with_capacity(data.len());
    for entry in data {
        let id = entry
            .get("id")
            .and_then(Value::as_str)
            .ok_or(ProviderError::InvalidResponse {
                provider: ProviderId::OpencodeGo,
                detail: "model entry has no id",
            })?;
        ids.push(id.to_owned());
    }
    Ok(ids)
}

fn incomplete(reason: Option<&str>) -> TurnOutcome {
    TurnOutcome::Incomplete {
        reason: reason.unwrap_or("unknown").to_owned(),
    }
}

fn chat_outcome(finish_reason: Option<&str>) -> TurnOutcome {
    match finish_reason {
        None | Some("stop") => TurnOutcome::Completed,
        Some("length") => incomplete(Some("max_tokens")),
        Some(other) => incomplete(Some(other)),
    }
}

fn messages_outcome(stop_reason: Option<&str>) -> TurnOutcome {
    match stop_reason {
        None | Some("end_turn") | Some("stop_sequence") => TurnOutcome::Completed,
        Some("max_tokens") => incomplete(Some("max_tokens")),
        Some(other) => incomplete(Some(other)),
    }
}

/// Parse a Chat Completions response into `(text, usage, outcome)`.
fn parse_chat_response(body: &Value) -> Result<(String, Usage, TurnOutcome), ProviderError> {
    let invalid = |detail| ProviderError::InvalidResponse {
        provider: ProviderId::OpencodeGo,
        detail,
    };
    let choice = body
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
        .ok_or(invalid("chat response has no choices"))?;
    // `message` for blocking calls, `delta` for SSE-accumulated payloads.
    let message = choice
        .get("message")
        .or_else(|| choice.get("delta"))
        .ok_or(invalid("chat choice has no message"))?;
    let text = message
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let finish = choice.get("finish_reason").and_then(Value::as_str);
    Ok((text, parse_openai_usage(body), chat_outcome(finish)))
}

/// Parse a Responses response into `(text, usage, outcome)`.
///
/// Text comes from `message` items' `output_text` parts; reasoning items and
/// encrypted continuation blobs are intentionally ignored. A `failed` status
/// is an error, never an empty success; `incomplete` (usually
/// `max_output_tokens` too small for the model's reasoning effort) keeps its
/// reason alongside partial text and usage.
fn parse_responses_response(body: &Value) -> Result<(String, Usage, TurnOutcome), ProviderError> {
    let invalid = |detail| ProviderError::InvalidResponse {
        provider: ProviderId::OpencodeGo,
        detail,
    };
    match body.get("status").and_then(Value::as_str) {
        Some("failed") => {
            return Err(ProviderError::TurnFailed {
                provider: ProviderId::OpencodeGo,
            });
        }
        Some("completed" | "incomplete") | None => {}
        Some(_) => return Err(invalid("unexpected responses status")),
    }
    let output = body
        .get("output")
        .and_then(Value::as_array)
        .ok_or(invalid("responses body has no output"))?;
    let mut text = String::new();
    for item in output {
        if item.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let content = item
            .get("content")
            .and_then(Value::as_array)
            .ok_or(invalid("responses message has no content"))?;
        for part in content {
            if part.get("type").and_then(Value::as_str) != Some("output_text") {
                continue;
            }
            if let Some(chunk) = part.get("text").and_then(Value::as_str) {
                text.push_str(chunk);
            }
        }
    }
    let outcome = match body.get("status").and_then(Value::as_str) {
        Some("incomplete") => incomplete(
            body.get("incomplete_details")
                .and_then(|d| d.get("reason"))
                .and_then(Value::as_str),
        ),
        _ => TurnOutcome::Completed,
    };
    Ok((text, parse_openai_usage(body), outcome))
}

/// Parse an Anthropic Messages response into `(text, usage, outcome)`.
fn parse_messages_response(body: &Value) -> Result<(String, Usage, TurnOutcome), ProviderError> {
    let invalid = |detail| ProviderError::InvalidResponse {
        provider: ProviderId::OpencodeGo,
        detail,
    };
    if body.get("type").and_then(Value::as_str) == Some("error") {
        return Err(ProviderError::TurnFailed {
            provider: ProviderId::OpencodeGo,
        });
    }
    let content = body
        .get("content")
        .and_then(Value::as_array)
        .ok_or(invalid("messages body has no content"))?;
    let mut text = String::new();
    for block in content {
        if block.get("type").and_then(Value::as_str) != Some("text") {
            continue;
        }
        if let Some(chunk) = block.get("text").and_then(Value::as_str) {
            text.push_str(chunk);
        }
    }
    let usage = body.get("usage");
    let input = usage
        .and_then(|u| u.get("input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output = usage
        .and_then(|u| u.get("output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let outcome = messages_outcome(body.get("stop_reason").and_then(Value::as_str));
    Ok((
        text,
        Usage {
            input_tokens: input,
            output_tokens: output,
            total_tokens: input.saturating_add(output),
        },
        outcome,
    ))
}

fn parse_openai_usage(body: &Value) -> Usage {
    let usage = body.get("usage");
    let input = usage
        .and_then(|u| u.get("input_tokens").or_else(|| u.get("prompt_tokens")))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output = usage
        .and_then(|u| {
            u.get("output_tokens")
                .or_else(|| u.get("completion_tokens"))
        })
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total = usage
        .and_then(|u| u.get("total_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or_else(|| input.saturating_add(output));
    Usage {
        input_tokens: input,
        output_tokens: output,
        total_tokens: total,
    }
}

/// Incremental fold of SSE lines into text, usage, and terminal outcome.
///
/// Shared by the async streaming loop and the synchronous test driver so both
/// enforce the same terminal, error, usage, and bound behavior.
#[derive(Debug, Default)]
struct StreamFold {
    text: String,
    usage: Usage,
    stop_reason: Option<String>,
    terminal: Option<TurnOutcome>,
}

impl StreamFold {
    fn feed(
        &mut self,
        line: &str,
        wire: WireProtocol,
        on_delta: &mut impl FnMut(StreamDelta),
    ) -> Result<(), ProviderError> {
        if sse_stream_error(line, wire) {
            return Err(ProviderError::TurnFailed {
                provider: ProviderId::OpencodeGo,
            });
        }
        if let Some(reason) = sse_stop_reason(line) {
            self.stop_reason = Some(reason);
        }
        if let Some(delta) = parse_sse_line(line, wire) {
            if self.text.len().saturating_add(delta.text.len()) > MAX_STREAM_TEXT_BYTES {
                return Err(ProviderError::LimitExceeded {
                    detail: "streamed text exceeds byte bound",
                });
            }
            if !delta.text.is_empty() {
                self.text.push_str(&delta.text);
            }
            on_delta(delta);
        }
        merge_sse_usage(&mut self.usage, line);
        if let Some(outcome) = sse_terminal(line, wire, self.stop_reason.as_deref()) {
            self.terminal = Some(outcome);
        }
        Ok(())
    }
}

/// Extract one SSE `data:` payload as JSON, if present.
fn sse_data(line: &str) -> Option<Value> {
    for raw in line.split('\n') {
        // `event:` lines carry no payload; only `data:` lines do.
        if raw.trim_start().starts_with("event:") {
            continue;
        }
        let data = raw
            .strip_prefix("data:")
            .map(str::trim)
            .unwrap_or(raw.trim());
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(data) {
            return Some(value);
        }
    }
    None
}

/// Whether one SSE line reports a provider-side failure.
///
/// Server error text is deliberately not propagated: the fixed
/// [`ProviderError::TurnFailed`] carries no upstream excerpt, so reflected
/// credentials cannot escape through streaming errors either.
fn sse_stream_error(line: &str, wire: WireProtocol) -> bool {
    let Some(value) = sse_data(line) else {
        return false;
    };
    match wire {
        WireProtocol::OpenAiChatCompletions => value.get("error").is_some(),
        WireProtocol::AnthropicMessages => {
            value.get("type").and_then(Value::as_str) == Some("error")
        }
        WireProtocol::OpenAiResponses => matches!(
            value.get("type").and_then(Value::as_str),
            Some("response.failed")
        ),
    }
}

/// Capture a stop/finish reason carried by one SSE line, if any.
fn sse_stop_reason(line: &str) -> Option<String> {
    let value = sse_data(line)?;
    value
        .get("choices")?
        .as_array()?
        .first()?
        .get("finish_reason")?
        .as_str()
        .map(str::to_owned)
        .or_else(|| {
            value
                .get("delta")?
                .get("stop_reason")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
}

/// Whether one SSE line is the wire's terminal event, with its outcome.
fn sse_terminal(line: &str, wire: WireProtocol, stop_reason: Option<&str>) -> Option<TurnOutcome> {
    if line.trim() == "data: [DONE]" {
        return Some(TurnOutcome::Completed);
    }
    let value = sse_data(line)?;
    match wire {
        WireProtocol::OpenAiChatCompletions => {
            let finish = value
                .get("choices")?
                .as_array()?
                .first()?
                .get("finish_reason")
                .and_then(Value::as_str);
            // A null finish reason marks an in-progress delta, not a terminal
            // event; only an explicit reason string terminates the stream.
            finish.is_some().then(|| chat_outcome(finish))
        }
        WireProtocol::AnthropicMessages => {
            if value.get("type").and_then(Value::as_str) == Some("message_stop") {
                Some(messages_outcome(stop_reason))
            } else {
                None
            }
        }
        WireProtocol::OpenAiResponses => match value.get("type").and_then(Value::as_str) {
            Some("response.completed") => Some(TurnOutcome::Completed),
            Some("response.incomplete") => Some(incomplete(
                value
                    .get("response")?
                    .get("incomplete_details")?
                    .get("reason")?
                    .as_str(),
            )),
            _ => None,
        },
    }
}

/// Extract a streaming text/reasoning delta from one SSE line.
fn parse_sse_line(line: &str, wire: WireProtocol) -> Option<StreamDelta> {
    let value = sse_data(line)?;
    match wire {
        WireProtocol::OpenAiChatCompletions => {
            let delta = value.get("choices")?.as_array()?.first()?.get("delta")?;
            let text = delta
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let reasoning = delta
                .get("reasoning_content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if text.is_empty() && reasoning.is_empty() {
                return None;
            }
            Some(StreamDelta { text, reasoning })
        }
        WireProtocol::AnthropicMessages => {
            if value.get("type").and_then(Value::as_str) != Some("content_block_delta") {
                return None;
            }
            let text = value
                .get("delta")?
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if text.is_empty() {
                return None;
            }
            Some(StreamDelta {
                text,
                reasoning: String::new(),
            })
        }
        WireProtocol::OpenAiResponses => {
            if value.get("type").and_then(Value::as_str) != Some("response.output_text.delta") {
                return None;
            }
            let text = value
                .get("delta")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if text.is_empty() {
                return None;
            }
            Some(StreamDelta {
                text,
                reasoning: String::new(),
            })
        }
    }
}

/// Merge best-effort usage from one SSE line into the running totals.
///
/// Standard Anthropic streams announce input tokens in `message_start` and
/// cumulative output in `message_delta`; Chat chunks and Responses terminal
/// events carry their own `usage`. Nonzero fields win so a later partial
/// update cannot zero a known count.
fn merge_sse_usage(usage: &mut Usage, line: &str) {
    let Some(value) = sse_data(line) else {
        return;
    };
    if value.get("type").and_then(Value::as_str) == Some("message_start") {
        if let Some(input) = value
            .get("message")
            .and_then(|m| m.get("usage"))
            .and_then(|u| u.get("input_tokens"))
            .and_then(Value::as_u64)
        {
            usage.input_tokens = input;
            usage.total_tokens = usage.input_tokens.saturating_add(usage.output_tokens);
        }
        return;
    }
    if let Some(seen) = value.get("usage") {
        let input = seen
            .get("input_tokens")
            .or_else(|| seen.get("prompt_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let output = seen
            .get("output_tokens")
            .or_else(|| seen.get("completion_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if input != 0 {
            usage.input_tokens = input;
        }
        if output != 0 {
            usage.output_tokens = output;
        }
        usage.total_tokens = usage.input_tokens.saturating_add(usage.output_tokens);
        return;
    }
    if let Some(response) = value.get("response").and_then(|r| r.get("usage")) {
        let input = response
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let output = response
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if input != 0 {
            usage.input_tokens = input;
        }
        if output != 0 {
            usage.output_tokens = output;
        }
        usage.total_tokens = usage.input_tokens.saturating_add(usage.output_tokens);
    }
}

/// Truncate to a character (not byte) boundary so multibyte text cannot panic.
fn truncate_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let end = text
        .char_indices()
        .take(max_chars)
        .last()
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    format!("{}…", text[..end].trim_end())
}

/// Synchronous SSE driver over a complete body, mirroring the async loop's
/// per-line logic. Used by tests to cover terminal, error, usage, and bound
/// behavior without HTTP.
#[cfg(test)]
fn process_sse_body(
    body: &str,
    wire: WireProtocol,
) -> Result<(String, Usage, TurnOutcome), ProviderError> {
    let mut stream = SseStream::new();
    let mut lines = stream.push(body.as_bytes())?;
    lines.extend(stream.finish()?);
    let mut fold = StreamFold::default();
    for line in &lines {
        fold.feed(line, wire, &mut |_| {})?;
    }
    fold.terminal
        .map(|outcome| (fold.text, fold.usage, outcome))
        .ok_or(ProviderError::InvalidResponse {
            provider: ProviderId::OpencodeGo,
            detail: "stream ended before terminal event",
        })
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
    fn upstream_errors_redact_a_reflected_key() {
        let secret = "oc_sk_synthetic_secret_123";
        let client = OpencodeGoClient::new(secret).expect("client builds");
        let err = client.status_error(401, &format!("bad key {secret} rejected"));
        for rendered in [err.to_string(), format!("{err:?}")] {
            assert!(!rendered.contains(secret), "{rendered}");
            assert!(rendered.contains("[REDACTED]"), "{rendered}");
        }
    }

    #[test]
    fn truncation_stops_at_a_character_boundary() {
        // Byte 500 lands inside `é`; slicing there would panic.
        let body = format!("{}{}", "a".repeat(499), "é".repeat(10));
        let out = truncate_text(&body, 500);
        assert!(out.starts_with(&"a".repeat(499)));
        assert!(out.contains('é'));
        assert!(out.ends_with('…'));
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
        let (text, usage, outcome) = parse_chat_response(&body).unwrap();
        assert_eq!(text, "Hi there!");
        assert_eq!(usage.total_tokens, 25);
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
        let (text, _, outcome) = parse_chat_response(&body).unwrap();
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
        let (text, usage, outcome) = parse_responses_response(&body).unwrap();
        assert_eq!(text, "ok");
        assert_eq!(usage.total_tokens, 187);
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
        let (text, usage, outcome) = parse_responses_response(&body).unwrap();
        assert_eq!(text, "");
        assert_eq!(usage.total_tokens, 78);
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
            parse_responses_response(&body),
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
        let (text, usage, outcome) = parse_messages_response(&body).unwrap();
        assert_eq!(text, "Hello!");
        assert_eq!(usage.total_tokens, 32);
        assert_eq!(outcome, TurnOutcome::Completed);
    }

    #[test]
    fn messages_max_tokens_maps_to_incomplete() {
        let body = serde_json::json!({
            "stop_reason": "max_tokens",
            "content": [{"type": "text", "text": "partial"}],
            "usage": {"input_tokens": 18, "output_tokens": 64},
        });
        let (_, _, outcome) = parse_messages_response(&body).unwrap();
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
            parse_messages_response(&body),
            Err(ProviderError::TurnFailed { .. })
        ));
    }

    #[test]
    fn parses_live_models_list() {
        let ids = parse_models_list(
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
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n",
            WireProtocol::OpenAiChatCompletions,
        )
        .unwrap();
        assert_eq!(chat.text, "hi");

        let reasoning = parse_sse_line(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"thinking\"}}]}\n",
            WireProtocol::OpenAiChatCompletions,
        )
        .unwrap();
        assert_eq!(reasoning.reasoning, "thinking");

        let msg = parse_sse_line(
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n",
            WireProtocol::AnthropicMessages,
        )
        .unwrap();
        assert_eq!(msg.text, "ok");

        let resp = parse_sse_line(
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n",
            WireProtocol::OpenAiResponses,
        )
        .unwrap();
        assert_eq!(resp.text, "ok");

        assert!(parse_sse_line("data: [DONE]\n", WireProtocol::OpenAiChatCompletions).is_none());
    }

    #[test]
    fn truncated_streams_are_errors_per_shape() {
        // No terminal event in any of these bodies.
        let chat = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n";
        let err = process_sse_body(chat, WireProtocol::OpenAiChatCompletions).unwrap_err();
        assert!(matches!(
            err,
            ProviderError::InvalidResponse { detail, .. }
            if detail == "stream ended before terminal event"
        ));

        let msg = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n";
        assert!(process_sse_body(msg, WireProtocol::AnthropicMessages).is_err());

        let resp = "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n";
        assert!(process_sse_body(resp, WireProtocol::OpenAiResponses).is_err());
    }

    #[test]
    fn in_stream_provider_errors_fail_per_shape() {
        let chat = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\ndata: {\"error\":{\"message\":\"boom\"}}\n";
        assert!(matches!(
            process_sse_body(chat, WireProtocol::OpenAiChatCompletions),
            Err(ProviderError::TurnFailed { .. })
        ));

        let msg = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"x\",\"message\":\"boom\"}}\n";
        assert!(matches!(
            process_sse_body(msg, WireProtocol::AnthropicMessages),
            Err(ProviderError::TurnFailed { .. })
        ));

        let resp = "event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{}}\n";
        assert!(matches!(
            process_sse_body(resp, WireProtocol::OpenAiResponses),
            Err(ProviderError::TurnFailed { .. })
        ));
    }

    #[test]
    fn complete_native_sequences_resolve() {
        let chat = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n";
        let (text, _, outcome) =
            process_sse_body(chat, WireProtocol::OpenAiChatCompletions).unwrap();
        assert_eq!(text, "ok");
        assert_eq!(outcome, TurnOutcome::Completed);

        let msg = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":18,\"output_tokens\":0}}}\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":4}}\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n",
        );
        let (text, usage, outcome) =
            process_sse_body(msg, WireProtocol::AnthropicMessages).unwrap();
        assert_eq!(text, "ok");
        assert_eq!(usage.input_tokens, 18);
        assert_eq!(usage.output_tokens, 4);
        assert_eq!(usage.total_tokens, 22);
        assert_eq!(outcome, TurnOutcome::Completed);

        let resp = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":11,\"output_tokens\":5,\"total_tokens\":16}}}\n",
        );
        let (text, usage, outcome) = process_sse_body(resp, WireProtocol::OpenAiResponses).unwrap();
        assert_eq!(text, "ok");
        assert_eq!(usage.total_tokens, 16);
        assert_eq!(outcome, TurnOutcome::Completed);
    }

    #[test]
    fn stream_accumulator_survives_split_unicode() {
        let line = "data: {\"choices\":[{\"delta\":{\"content\":\"héllo\"}}]}\n";
        for split in 0..line.len() {
            let (head, tail) = line.as_bytes().split_at(split);
            let mut stream = SseStream::new();
            let mut lines = stream.push(head).unwrap();
            lines.extend(stream.push(tail).unwrap());
            lines.extend(stream.finish().unwrap());
            let joined = lines.concat();
            assert!(joined.contains("héllo"), "split at {split}");
        }
    }

    #[test]
    fn oversized_stream_line_is_rejected() {
        let mut stream = SseStream::new();
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
}
