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
//! Secrets travel in headers only and never appear in errors, logs, or usage
//! records.

use std::time::Duration;

use serde_json::Value;
use slop_core::provider::ProviderId;

use super::{
    ChatRequest, ChatResponse, Provider, ProviderError, ProviderModel, Role, StreamDelta, Usage,
    WireProtocol, is_valid_session_id,
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

    /// Fetch the live model list (`GET /models`). Returns advertised ids.
    pub async fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        let response = self
            .http
            .get(OpencodeGoProvider::models_url())
            .bearer_auth(&self.api_key)
            .header(SESSION_HEADER, "slop-model-discovery")
            .send()
            .await?;
        check_status(ProviderId::OpencodeGo, response).await
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
    /// assembled turn (text joined from text deltas, best-effort usage).
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
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::UnexpectedStatus {
                provider: ProviderId::OpencodeGo,
                status,
                body: truncate_body(&body),
            });
        }
        let mut buffer = String::new();
        let mut text = String::new();
        let mut usage = Usage::default();
        while let Some(chunk) = response.chunk().await? {
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            while let Some((line, rest)) = split_line(&buffer) {
                buffer = rest;
                if let Some(delta) = parse_sse_line(&line, wire) {
                    if !delta.text.is_empty() {
                        text.push_str(&delta.text);
                    }
                    on_delta(delta);
                }
                if let Some(seen) = parse_sse_usage(&line) {
                    usage = seen;
                }
            }
        }
        if let Some(delta) = parse_sse_line(&buffer, wire) {
            if !delta.text.is_empty() {
                text.push_str(&delta.text);
            }
            on_delta(delta);
        }
        if let Some(seen) = parse_sse_usage(&buffer) {
            usage = seen;
        }
        Ok(ChatResponse {
            model: request.model.clone(),
            text,
            usage,
            wire,
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
        let (text, usage) = parse_chat_response(&body)?;
        Ok(ChatResponse {
            model: request.model.clone(),
            text,
            usage,
            wire: WireProtocol::OpenAiChatCompletions,
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
        let (text, usage) = parse_responses_response(&body)?;
        Ok(ChatResponse {
            model: request.model.clone(),
            text,
            usage,
            wire: WireProtocol::OpenAiResponses,
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
        let (text, usage) = parse_messages_response(&body)?;
        Ok(ChatResponse {
            model: request.model.clone(),
            text,
            usage,
            wire: WireProtocol::AnthropicMessages,
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
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(ProviderError::UnexpectedStatus {
                provider: ProviderId::OpencodeGo,
                status: status.as_u16(),
                body: truncate_body(&body),
            });
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

async fn check_status(
    provider: ProviderId,
    response: reqwest::Response,
) -> Result<Vec<String>, ProviderError> {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(ProviderError::UnexpectedStatus {
            provider,
            status: status.as_u16(),
            body: truncate_body(&body),
        });
    }
    parse_models_list(&body)
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

/// Translate neutral messages to a Responses `input` string.
///
/// Single user turn passes through unchanged; multi-message context joins as
/// labeled lines so the model still sees role boundaries.
fn to_responses_input(messages: &[super::ChatMessage]) -> String {
    if messages.len() == 1 {
        return messages[0].content.clone();
    }
    messages
        .iter()
        .map(|m| format!("{}: {}", m.role.as_str(), m.content))
        .collect::<Vec<_>>()
        .join("\n\n")
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

/// Parse a Chat Completions response into `(text, usage)`.
fn parse_chat_response(body: &Value) -> Result<(String, Usage), ProviderError> {
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
    Ok((text, parse_openai_usage(body)))
}

/// Parse a Responses response into `(text, usage)`.
///
/// Text comes from `message` items' `output_text` parts; reasoning items and
/// encrypted continuation blobs are intentionally ignored. An `incomplete`
/// status with an empty `output` array (usually `max_output_tokens` too small
/// for the model's reasoning effort) yields empty text rather than an error so
/// the caller can record usage and retry with a larger bound.
fn parse_responses_response(body: &Value) -> Result<(String, Usage), ProviderError> {
    let invalid = |detail| ProviderError::InvalidResponse {
        provider: ProviderId::OpencodeGo,
        detail,
    };
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
    Ok((text, parse_openai_usage(body)))
}

/// Parse an Anthropic Messages response into `(text, usage)`.
fn parse_messages_response(body: &Value) -> Result<(String, Usage), ProviderError> {
    let invalid = |detail| ProviderError::InvalidResponse {
        provider: ProviderId::OpencodeGo,
        detail,
    };
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
    Ok((
        text,
        Usage {
            input_tokens: input,
            output_tokens: output,
            total_tokens: input.saturating_add(output),
        },
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

/// Split one `\n`-terminated line off the SSE buffer.
fn split_line(buffer: &str) -> Option<(String, String)> {
    let end = buffer.find('\n')?;
    let (line, rest) = buffer.split_at(end + 1);
    Some((line.to_owned(), rest.to_owned()))
}

/// Extract a streaming text/reasoning delta from one SSE line.
fn parse_sse_line(line: &str, wire: WireProtocol) -> Option<StreamDelta> {
    for raw in line.split('\n') {
        let data = raw.strip_prefix("data:").map(str::trim).unwrap_or("");
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let value: Value = serde_json::from_str(data).ok()?;
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
                    continue;
                }
                return Some(StreamDelta { text, reasoning });
            }
            WireProtocol::AnthropicMessages => {
                if value.get("type").and_then(Value::as_str) != Some("content_block_delta") {
                    continue;
                }
                let text = value
                    .get("delta")?
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if text.is_empty() {
                    continue;
                }
                return Some(StreamDelta {
                    text,
                    reasoning: String::new(),
                });
            }
            WireProtocol::OpenAiResponses => {
                if value.get("type").and_then(Value::as_str) != Some("response.output_text.delta") {
                    continue;
                }
                let text = value
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if text.is_empty() {
                    continue;
                }
                return Some(StreamDelta {
                    text,
                    reasoning: String::new(),
                });
            }
        }
    }
    None
}

/// Extract best-effort usage from one SSE line's terminal event.
fn parse_sse_usage(line: &str) -> Option<Usage> {
    for raw in line.split('\n') {
        let data = raw.strip_prefix("data:").map(str::trim).unwrap_or("");
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let value: Value = serde_json::from_str(data).ok()?;
        // Chat chunks carry a final `usage`; Anthropic sends `message_delta`
        // with `usage`; Responses sends `response.completed` with a full body.
        if let Some(usage) = value.get("usage")
            && (usage.get("input_tokens").and_then(Value::as_u64).is_some()
                || usage.get("prompt_tokens").and_then(Value::as_u64).is_some())
        {
            return Some(parse_openai_usage(&value));
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
            return Some(Usage {
                input_tokens: input,
                output_tokens: output,
                total_tokens: input.saturating_add(output),
            });
        }
    }
    None
}

fn truncate_body(body: &str) -> String {
    const LIMIT: usize = 500;
    let trimmed = body.trim();
    if trimmed.len() <= LIMIT {
        return trimmed.to_owned();
    }
    format!("{}…", trimmed[..LIMIT].trim_end())
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
    fn provider_errors_never_contain_the_key() {
        let secret = "oc_sk_test_secret_value";
        let err = ProviderError::UnexpectedStatus {
            provider: ProviderId::OpencodeGo,
            status: 401,
            body: "unauthorized".to_owned(),
        }
        .to_string();
        assert!(!err.contains(secret));
    }

    #[test]
    fn parses_live_chat_shape() {
        let body = serde_json::json!({
            "id": "chatcmpl-test",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "Hi there!"},
            }],
            "usage": {"prompt_tokens": 20, "completion_tokens": 5, "total_tokens": 25},
        });
        let (text, usage) = parse_chat_response(&body).unwrap();
        assert_eq!(text, "Hi there!");
        assert_eq!(usage.total_tokens, 25);
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
        let (text, usage) = parse_responses_response(&body).unwrap();
        assert_eq!(text, "ok");
        assert_eq!(usage.total_tokens, 187);
    }

    #[test]
    fn incomplete_responses_yields_empty_text_with_usage() {
        let body = serde_json::json!({
            "status": "incomplete",
            "output": [],
            "usage": {"input_tokens": 14, "output_tokens": 64, "total_tokens": 78},
        });
        let (text, usage) = parse_responses_response(&body).unwrap();
        assert_eq!(text, "");
        assert_eq!(usage.total_tokens, 78);
    }

    #[test]
    fn parses_live_messages_shape() {
        let body = serde_json::json!({
            "content": [{"type": "text", "text": "Hello!"}],
            "usage": {"input_tokens": 18, "output_tokens": 14},
        });
        let (text, usage) = parse_messages_response(&body).unwrap();
        assert_eq!(text, "Hello!");
        assert_eq!(usage.total_tokens, 32);
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
