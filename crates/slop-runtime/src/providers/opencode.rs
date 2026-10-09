//! Shared native HTTP transport for OpenCode gateways and bounded text decoders
//! reused by all compiled-in provider clients.
//!
//! Each adapter supplies its own identity, endpoint, credential variable, and
//! verified catalog. HTTP redirects/retries are disabled, and upstream error
//! bodies never enter diagnostics. The decoder never assigns provider identity.

use std::time::Duration;

use serde_json::Value;
use slop_core::provider::{ProviderId, is_valid_model_id};

use super::{
    ChatRequest, ChatResponse, Provider, ProviderClient, ProviderError, ProviderFuture, Role,
    StreamDelta, TurnOutcome, Usage, WireProtocol,
};

/// Required version header for the Anthropic Messages shape.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Session header used for routing and prompt caching.
pub const SESSION_HEADER: &str = "x-opencode-session";
/// This client's own identifier, shared by both OpenCode gateways.
pub const USER_AGENT: &str = concat!("slopconductor/", env!("CARGO_PKG_VERSION"));

/// Cap for blocking response bodies, enforced while reading.
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
/// Cap for one SSE line or assembled event payload.
pub const MAX_SSE_LINE_BYTES: usize = 256 * 1024;
/// Cap for assembled streamed text.
pub const MAX_STREAM_TEXT_BYTES: usize = 1024 * 1024;
/// Cap for total streamed bytes per request.
pub const MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;

#[cfg(test)]
mod http_tests;

pub(super) struct OpencodeClient {
    provider: &'static dyn Provider,
    base_url: String,
    http: reqwest::Client,
    api_key: String,
    continuation_scope: String,
}

impl OpencodeClient {
    pub async fn infer(
        &self,
        request: &super::inference::InferenceRequest,
        on_event: &mut (dyn FnMut(super::inference::ProviderEvent) + Send),
    ) -> Result<super::inference::InferenceResponse, ProviderError> {
        let wire = request.validate(
            self.provider,
            &super::inference::capabilities(self.provider, &request.model),
        )?;
        for message in &request.messages {
            if message
                .continuation
                .as_ref()
                .is_some_and(|c| c.required && c.scope != self.continuation_scope)
            {
                return Err(ProviderError::UnsupportedCapability {
                    capability: "context_incompatible: required account or endpoint continuation",
                });
            }
        }
        let payload = super::structured_wire::encode(
            request,
            wire,
            self.provider.id(),
            &self.continuation_scope,
        )?;
        let mut req = self.http.post(format!("{}{}", self.base_url, wire.path()));
        req = match wire {
            WireProtocol::AnthropicMessages => req
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", ANTHROPIC_VERSION),
            _ => req.bearer_auth(&self.api_key),
        };
        let mut response = req
            .header(SESSION_HEADER, &request.session_id)
            .header("Accept", "text/event-stream")
            .json(&payload)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(self.status_error(response.status().as_u16()));
        }
        if !is_event_stream(&response) {
            let body = self.read_body_limited(response).await?;
            let value =
                serde_json::from_str(&body).map_err(|_| ProviderError::InvalidResponse {
                    provider: self.provider.id(),
                    detail: "response body is not JSON",
                })?;
            let mut result =
                super::structured_wire::decode(&value, wire, request, self.provider.id())
                    .map_err(|error| error.for_provider(self.provider.id()))?;
            if let Some(c) = &mut result.message.continuation {
                c.scope = self.continuation_scope.clone();
            }
            return Ok(result);
        }
        let mut framing = SseStream::new();
        let mut fold = super::structured_wire::StructuredFold::new(wire);
        while let Some(chunk) = response.chunk().await? {
            for event in framing
                .push(&chunk)
                .map_err(|error| error.for_provider(self.provider.id()))?
            {
                fold.feed(&event, on_event)
                    .map_err(|error| error.for_provider(self.provider.id()))?;
            }
        }
        for event in framing
            .finish()
            .map_err(|error| error.for_provider(self.provider.id()))?
        {
            fold.feed(&event, on_event)
                .map_err(|error| error.for_provider(self.provider.id()))?;
        }
        let mut result = fold
            .finish(request, self.provider.id())
            .map_err(|error| error.for_provider(self.provider.id()))?;
        if let Some(c) = &mut result.message.continuation {
            c.scope = self.continuation_scope.clone();
        }
        Ok(result)
    }

    /// Build from an explicit key (never logged or included in errors).
    pub fn new(provider: &'static dyn Provider, api_key: &str) -> Result<Self, ProviderError> {
        Self::new_with_base_url(provider, api_key, None)
    }

    /// Build with an explicitly trusted base URL, for controlled gateways and
    /// local provider fixtures. HTTP is limited to loopback hosts.
    pub fn new_with_base_url(
        provider: &'static dyn Provider,
        api_key: &str,
        base_url: Option<&str>,
    ) -> Result<Self, ProviderError> {
        if api_key.trim().is_empty() {
            return Err(ProviderError::EmptyApiKey {
                env_var: provider.env_key_var(),
            });
        }
        let base_url = match base_url {
            Some(value) => validate_base_url(value)?,
            None => provider.base_url().to_owned(),
        };
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .build()?;
        use sha2::{Digest, Sha256};
        let mut scope = Sha256::new();
        scope.update(api_key.as_bytes());
        scope.update([0]);
        scope.update(base_url.as_bytes());
        let continuation_scope = format!("{:x}", scope.finalize());
        Ok(Self {
            provider,
            base_url,
            http,
            api_key: api_key.to_owned(),
            continuation_scope,
        })
    }

    /// Build from this adapter's environment variable, without fallback.
    pub fn from_env(provider: &'static dyn Provider) -> Result<Self, ProviderError> {
        match std::env::var(provider.env_key_var()) {
            Ok(key) => Self::new(provider, &key),
            Err(_) => Err(ProviderError::MissingApiKey {
                env_var: provider.env_key_var(),
            }),
        }
    }

    fn status_error(&self, status: u16) -> ProviderError {
        ProviderError::UnexpectedStatus {
            provider: self.provider.id(),
            status,
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
        String::from_utf8(bytes).map_err(|_| ProviderError::InvalidResponse {
            provider: self.provider.id(),
            detail: "response body is not valid UTF-8",
        })
    }

    /// Fetch the live model list (`GET /models`). Returns advertised ids.
    pub async fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        let response = self
            .http
            .get(format!("{}/models", self.base_url))
            .bearer_auth(&self.api_key)
            .header(SESSION_HEADER, "slop-model-discovery")
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(self.status_error(status.as_u16()));
        }
        let body = self.read_body_limited(response).await?;
        parse_models_list(&body).map_err(|error| error.for_provider(self.provider.id()))
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
                    .post(format!("{}{}", self.base_url, wire.path()))
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
                    .post(format!("{}{}", self.base_url, wire.path()))
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
                        .post(format!("{}{}", self.base_url, wire.path()))
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
            return Err(self.status_error(status));
        }
        if !is_event_stream(&response) {
            // The gateway answered 200 with plain JSON instead of SSE (it
            // ignored `stream: true`). Parse it as a blocking turn rather
            // than failing or misreading it as deltas.
            let body = self.read_body_limited(response).await?;
            let value: Value =
                serde_json::from_str(&body).map_err(|_| ProviderError::InvalidResponse {
                    provider: self.provider.id(),
                    detail: "response body is not JSON",
                })?;
            let result = self.blocking_from_value(wire, &request.model, &value)?;
            if !result.text.is_empty() {
                on_delta(StreamDelta {
                    text: result.text.clone(),
                    reasoning: String::new(),
                });
            }
            return Ok(result);
        }
        let mut stream = SseStream::new();
        let mut fold = StreamFold::default();
        while let Some(chunk) = response.chunk().await? {
            for line in stream
                .push(&chunk)
                .map_err(|error| error.for_provider(self.provider.id()))?
            {
                fold.feed(&line, wire, &mut on_delta)
                    .map_err(|error| error.for_provider(self.provider.id()))?;
            }
        }
        for line in stream
            .finish()
            .map_err(|error| error.for_provider(self.provider.id()))?
        {
            fold.feed(&line, wire, &mut on_delta)
                .map_err(|error| error.for_provider(self.provider.id()))?;
        }
        let outcome = fold.terminal.ok_or(ProviderError::InvalidResponse {
            provider: self.provider.id(),
            detail: "stream ended before terminal event",
        })?;
        Ok(ChatResponse {
            model: request.model.clone(),
            resolved_model: fold.resolved_model,
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
            WireProtocol::OpenAiChatCompletions => parse_chat_response(value),
            WireProtocol::OpenAiResponses => parse_responses_response(value),
            WireProtocol::AnthropicMessages => parse_messages_response(value),
        }
        .map_err(|error| error.for_provider(self.provider.id()))?;
        Ok(ChatResponse {
            model: model.to_owned(),
            resolved_model: reported_model(value)
                .map_err(|error| error.for_provider(self.provider.id()))?,
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
        let mut req = self.http.post(format!("{}{}", self.base_url, wire.path()));
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
            provider: self.provider.id(),
            detail: "response body is not JSON",
        })
    }
}

pub(super) fn validate_base_url(value: &str) -> Result<String, ProviderError> {
    use std::net::IpAddr;

    let url = reqwest::Url::parse(value)
        .map_err(|_| ProviderError::InvalidRequest("provider endpoint must be an absolute URL"))?;
    let host = url.host_str().ok_or(ProviderError::InvalidRequest(
        "provider endpoint must include a host",
    ))?;
    let is_local = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    let authority_has_userinfo = value
        .split_once("://")
        .and_then(|(_, remainder)| remainder.split(['/', '?', '#']).next())
        .is_some_and(|authority| authority.contains('@'));
    if url.username() != ""
        || url.password().is_some()
        || authority_has_userinfo
        || url.query().is_some()
        || url.fragment().is_some()
        || (url.scheme() != "https" && !(url.scheme() == "http" && is_local))
        || (url.scheme() == "https" && host.is_empty())
    {
        return Err(ProviderError::InvalidRequest(
            "provider endpoint URL is not allowed",
        ));
    }
    let mut base = url.to_string();
    while base.ends_with('/') {
        base.pop();
    }
    Ok(base)
}

impl ProviderClient for OpencodeClient {
    fn descriptor(&self) -> &dyn Provider {
        self.provider
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

/// Decoder failures have no service identity; the transport adds its descriptor.
#[derive(Debug)]
pub(super) enum DecodeError {
    InvalidResponse { detail: &'static str },
    UnsupportedCapability { capability: &'static str },
    TurnFailed,
    LimitExceeded { detail: &'static str },
}

impl DecodeError {
    pub(super) fn for_provider(self, provider: ProviderId) -> ProviderError {
        match self {
            Self::InvalidResponse { detail } => ProviderError::InvalidResponse { provider, detail },
            Self::UnsupportedCapability { capability } => {
                ProviderError::UnsupportedCapability { capability }
            }
            Self::TurnFailed => ProviderError::TurnFailed { provider },
            Self::LimitExceeded { detail } => ProviderError::LimitExceeded { detail },
        }
    }
}

pub(super) fn is_event_stream(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|content_type| content_type.contains("text/event-stream"))
}

/// Bounded SSE event decoder that never splits UTF-8 across decodes.
///
/// CR/LF bytes cannot appear inside a multi-byte UTF-8 sequence. Decode only
/// complete lines, accepting LF, CRLF and CR even across network chunks.
pub(super) struct SseStream {
    pending: Vec<u8>,
    event_data: Vec<String>,
    event_bytes: usize,
    total: usize,
}

impl SseStream {
    pub(super) fn new() -> Self {
        Self {
            pending: Vec::new(),
            event_data: Vec::new(),
            event_bytes: 0,
            total: 0,
        }
    }

    /// Feed one network chunk; dispatch only blank-line-terminated events.
    pub(super) fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>, DecodeError> {
        self.total = self.total.saturating_add(chunk.len());
        if self.total > MAX_STREAM_BYTES {
            return Err(DecodeError::LimitExceeded {
                detail: "stream exceeds byte bound",
            });
        }
        self.pending.extend_from_slice(chunk);
        if self.pending.len() > MAX_STREAM_BYTES {
            return Err(DecodeError::LimitExceeded {
                detail: "stream exceeds byte bound",
            });
        }
        self.drain_lines(false)
    }

    /// Drain complete lines, rejecting overlong lines instead of buffering
    /// them without bound.
    fn drain_lines(&mut self, eof: bool) -> Result<Vec<String>, DecodeError> {
        let mut events = Vec::new();
        let mut consumed = 0;
        while let Some(offset) = self.pending[consumed..]
            .iter()
            .position(|b| *b == b'\n' || *b == b'\r')
        {
            let delimiter = consumed + offset;
            if self.pending[delimiter] == b'\r' && delimiter + 1 == self.pending.len() && !eof {
                break;
            }
            let delimiter_bytes = if self.pending[delimiter] == b'\r'
                && self.pending.get(delimiter + 1) == Some(&b'\n')
            {
                2
            } else {
                1
            };
            let end = delimiter + delimiter_bytes;
            if end - consumed > MAX_SSE_LINE_BYTES {
                return Err(DecodeError::LimitExceeded {
                    detail: "stream line exceeds byte bound",
                });
            }
            let line = std::str::from_utf8(&self.pending[consumed..end]).map_err(|_| {
                DecodeError::InvalidResponse {
                    detail: "stream is not valid UTF-8",
                }
            })?;
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if !self.event_data.is_empty() {
                    events.push(self.event_data.join("\n"));
                    self.event_data.clear();
                    self.event_bytes = 0;
                }
            } else if line.starts_with("data:") {
                self.event_bytes = self.event_bytes.saturating_add(line.len() + 1);
                if self.event_bytes > MAX_SSE_LINE_BYTES {
                    return Err(DecodeError::LimitExceeded {
                        detail: "stream event exceeds byte bound",
                    });
                }
                self.event_data.push(line.to_owned());
            }
            consumed = end;
        }
        self.pending.drain(..consumed);
        if self.pending.len() > MAX_SSE_LINE_BYTES {
            return Err(DecodeError::LimitExceeded {
                detail: "stream line exceeds byte bound",
            });
        }
        Ok(events)
    }

    /// EOF cannot dispatch an unterminated event as a completed response.
    pub(super) fn finish(&mut self) -> Result<Vec<String>, DecodeError> {
        let events = self.drain_lines(true)?;
        if self.pending.is_empty() && self.event_data.is_empty() {
            return Ok(events);
        }
        Err(DecodeError::InvalidResponse {
            detail: "stream ended inside an SSE event",
        })
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
/// Validation permits one optional leading system prompt outside `messages`.
/// Other system placements are rejected before this conversion.
fn to_anthropic_parts(messages: &[super::ChatMessage]) -> (Option<String>, Vec<Value>) {
    let mut system = Vec::new();
    let mut rest = Vec::new();
    for m in messages {
        match m.role {
            Role::System | Role::Developer => system.push(m.content.clone()),
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
pub(super) fn parse_models_list(body: &str) -> Result<Vec<String>, DecodeError> {
    let value: Value = serde_json::from_str(body).map_err(|_| DecodeError::InvalidResponse {
        detail: "model list is not JSON",
    })?;
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or(DecodeError::InvalidResponse {
            detail: "model list has no data array",
        })?;
    let mut ids = Vec::with_capacity(data.len());
    for entry in data {
        let id = entry
            .get("id")
            .and_then(Value::as_str)
            .ok_or(DecodeError::InvalidResponse {
                detail: "model entry has no id",
            })?;
        if !is_valid_model_id(id) {
            return Err(invalid_terminal("model list contains an invalid model id"));
        }
        ids.push(id.to_owned());
    }
    Ok(ids)
}

fn incomplete(reason: Option<&str>) -> TurnOutcome {
    TurnOutcome::Incomplete {
        reason: match reason {
            Some("max_tokens") => "max_tokens",
            Some("max_output_tokens") => "max_output_tokens",
            Some("content_filter") => "content_filter",
            _ => "unknown",
        }
        .to_owned(),
    }
}

fn invalid_terminal(detail: &'static str) -> DecodeError {
    DecodeError::InvalidResponse { detail }
}

pub(super) fn reported_model(value: &Value) -> Result<Option<String>, DecodeError> {
    let model = value
        .get("model")
        .or_else(|| {
            value
                .get("message")
                .and_then(|message| message.get("model"))
        })
        .or_else(|| {
            value
                .get("response")
                .and_then(|response| response.get("model"))
        });
    match model {
        None => Ok(None),
        Some(Value::String(id)) if is_valid_model_id(id) => Ok(Some(id.clone())),
        Some(_) => Err(invalid_terminal("response contains an invalid model id")),
    }
}

fn chat_outcome(finish_reason: Option<&str>) -> Result<TurnOutcome, DecodeError> {
    match finish_reason {
        Some("stop") => Ok(TurnOutcome::Completed),
        Some("length") => Ok(incomplete(Some("max_tokens"))),
        Some("content_filter") => Ok(incomplete(Some("content_filter"))),
        Some("tool_calls" | "function_call") => Err(DecodeError::UnsupportedCapability {
            capability: "tool proposals",
        }),
        _ => Err(invalid_terminal("missing or unknown chat finish reason")),
    }
}

fn messages_outcome(stop_reason: Option<&str>) -> Result<TurnOutcome, DecodeError> {
    match stop_reason {
        Some("end_turn" | "stop_sequence") => Ok(TurnOutcome::Completed),
        Some("max_tokens") => Ok(incomplete(Some("max_tokens"))),
        Some("tool_use" | "pause_turn" | "refusal") => Err(DecodeError::UnsupportedCapability {
            capability: "structured message outcome",
        }),
        _ => Err(invalid_terminal("missing or unknown messages stop reason")),
    }
}

/// Parse a Chat Completions response into `(text, usage, outcome)`.
pub(super) fn parse_chat_response(
    body: &Value,
) -> Result<(String, Usage, TurnOutcome), DecodeError> {
    let invalid = |detail| DecodeError::InvalidResponse { detail };
    if has_content(body.get("error")) {
        return Err(DecodeError::TurnFailed);
    }
    let choices = body
        .get("choices")
        .and_then(Value::as_array)
        .ok_or(invalid("chat response has no choices"))?;
    if choices.len() > 1 {
        return Err(unsupported_content());
    }
    let choice = choices
        .first()
        .ok_or(invalid("chat response has no choices"))?;
    let message = choice
        .get("message")
        .ok_or(invalid("chat choice has no message"))?;
    validate_chat_content(message)?;
    let text = match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) | None => String::new(),
        _ => return Err(unsupported_content()),
    };
    let finish = choice.get("finish_reason").and_then(Value::as_str);
    Ok((text, parse_openai_usage(body), chat_outcome(finish)?))
}

/// Parse a Responses response into `(text, usage, outcome)`.
///
/// Text comes from `message` items' `output_text` parts; unsupported structured
/// output is rejected instead of silently discarded. A `failed` status
/// is an error, never an empty success; `incomplete` (usually
/// `max_output_tokens` too small for the model's reasoning effort) keeps its
/// reason alongside partial text and usage.
pub(super) fn parse_responses_response(
    body: &Value,
) -> Result<(String, Usage, TurnOutcome), DecodeError> {
    let invalid = |detail| DecodeError::InvalidResponse { detail };
    match body.get("status").and_then(Value::as_str) {
        Some("failed") => {
            return Err(DecodeError::TurnFailed);
        }
        Some("completed" | "incomplete") => {}
        _ => return Err(invalid("missing or unexpected responses status")),
    }
    let output = body
        .get("output")
        .and_then(Value::as_array)
        .ok_or(invalid("responses body has no output"))?;
    let mut text = String::new();
    for item in output {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {}
            Some("reasoning")
                if !has_content(item.get("summary"))
                    && !has_content(item.get("encrypted_content")) =>
            {
                continue;
            }
            _ => return Err(unsupported_content()),
        }
        let content = item
            .get("content")
            .and_then(Value::as_array)
            .ok_or(invalid("responses message has no content"))?;
        for part in content {
            if part.get("type").and_then(Value::as_str) != Some("output_text") {
                return Err(unsupported_content());
            }
            if has_content(part.get("annotations")) {
                return Err(unsupported_content());
            }
            let chunk = part
                .get("text")
                .and_then(Value::as_str)
                .ok_or(invalid("output text has no text string"))?;
            text.push_str(chunk);
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
pub(super) fn parse_messages_response(
    body: &Value,
) -> Result<(String, Usage, TurnOutcome), DecodeError> {
    let invalid = |detail| DecodeError::InvalidResponse { detail };
    if body.get("type").and_then(Value::as_str) == Some("error") {
        return Err(DecodeError::TurnFailed);
    }
    let content = body
        .get("content")
        .and_then(Value::as_array)
        .ok_or(invalid("messages body has no content"))?;
    let mut text = String::new();
    for block in content {
        if block.get("type").and_then(Value::as_str) != Some("text") {
            return Err(unsupported_content());
        }
        if has_content(block.get("citations")) {
            return Err(unsupported_content());
        }
        let chunk = block
            .get("text")
            .and_then(Value::as_str)
            .ok_or(invalid("text block has no text string"))?;
        text.push_str(chunk);
    }
    let outcome = messages_outcome(body.get("stop_reason").and_then(Value::as_str))?;
    Ok((text, parse_openai_usage(body), outcome))
}

fn parse_openai_usage(body: &Value) -> Usage {
    let Some(usage) = body.get("usage") else {
        return Usage::default();
    };
    parse_usage(usage)
}

fn parse_usage(usage: &Value) -> Usage {
    let input = usage
        .get("input_tokens")
        .or_else(|| usage.get("prompt_tokens"))
        .and_then(Value::as_u64);
    let output = usage
        .get("output_tokens")
        .or_else(|| usage.get("completion_tokens"))
        .and_then(Value::as_u64);
    let total = usage.get("total_tokens").and_then(Value::as_u64);
    Usage::from_reported(input, output, total)
}

fn unsupported_content() -> DecodeError {
    DecodeError::UnsupportedCapability {
        capability: "structured response content in the text-only adapter",
    }
}

fn has_content(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::String(text)) => !text.is_empty(),
        Some(_) => true,
    }
}

fn validate_chat_content(content: &Value) -> Result<(), DecodeError> {
    if ["tool_calls", "function_call", "refusal", "audio"]
        .iter()
        .any(|field| has_content(content.get(field)))
    {
        return Err(unsupported_content());
    }
    if content
        .get("content")
        .is_some_and(|value| !value.is_null() && !value.is_string())
    {
        return Err(unsupported_content());
    }
    Ok(())
}

fn validate_stream_content(value: &Value, wire: WireProtocol) -> Result<(), DecodeError> {
    match wire {
        WireProtocol::OpenAiChatCompletions => {
            let choices = value
                .get("choices")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid_terminal("chat stream event has no choices"))?;
            if choices.len() > 1 {
                return Err(unsupported_content());
            }
            if let Some(delta) = choices.first().and_then(|choice| choice.get("delta")) {
                validate_chat_content(delta)?;
            }
        }
        WireProtocol::AnthropicMessages => match value.get("type").and_then(Value::as_str) {
            Some("content_block_start") => {
                let block = value
                    .get("content_block")
                    .ok_or_else(|| invalid_terminal("block start has no content block"))?;
                if block.get("type").and_then(Value::as_str) != Some("text")
                    || has_content(block.get("citations"))
                {
                    return Err(unsupported_content());
                }
                if !block.get("text").is_some_and(Value::is_string) {
                    return Err(invalid_terminal("text block has no text string"));
                }
            }
            Some("content_block_delta") => {
                let delta = value
                    .get("delta")
                    .ok_or_else(|| invalid_terminal("block delta has no delta"))?;
                if delta.get("type").and_then(Value::as_str) != Some("text_delta") {
                    return Err(unsupported_content());
                }
                if !delta.get("text").is_some_and(Value::is_string) {
                    return Err(invalid_terminal("text delta has no text string"));
                }
            }
            Some("message_start") => {
                if value
                    .get("message")
                    .and_then(|message| message.get("content"))
                    .is_some_and(|content| !content.is_array() || has_content(Some(content)))
                {
                    return Err(invalid_terminal("message start content is not empty"));
                }
            }
            Some("message_delta" | "message_stop" | "content_block_stop" | "ping") => {}
            _ => return Err(invalid_terminal("unsupported messages stream event")),
        },
        WireProtocol::OpenAiResponses => match value.get("type").and_then(Value::as_str) {
            Some("response.output_item.added" | "response.output_item.done") => {
                let item = value
                    .get("item")
                    .ok_or_else(|| invalid_terminal("output item event has no item"))?;
                match item.get("type").and_then(Value::as_str) {
                    Some("message") => {
                        parse_responses_response(&serde_json::json!({
                            "status": "completed", "output": [item]
                        }))?;
                    }
                    Some("reasoning")
                        if !has_content(item.get("summary"))
                            && !has_content(item.get("encrypted_content")) => {}
                    _ => return Err(unsupported_content()),
                }
            }
            Some("response.content_part.added" | "response.content_part.done") => {
                let part = value
                    .get("part")
                    .ok_or_else(|| invalid_terminal("content part event has no part"))?;
                if part.get("type").and_then(Value::as_str) != Some("output_text")
                    || has_content(part.get("annotations"))
                {
                    return Err(unsupported_content());
                }
                if !part.get("text").is_some_and(Value::is_string) {
                    return Err(invalid_terminal("output text part has no text string"));
                }
            }
            Some("response.output_text.delta") => {
                if !value.get("delta").is_some_and(Value::is_string) {
                    return Err(invalid_terminal("text delta has no text string"));
                }
            }
            Some("response.output_text.done") => {
                if !value.get("text").is_some_and(Value::is_string) {
                    return Err(invalid_terminal("completed text event has no text string"));
                }
            }
            Some(
                "response.created"
                | "response.in_progress"
                | "response.completed"
                | "response.incomplete",
            ) => {}
            _ => return Err(unsupported_content()),
        },
    }
    Ok(())
}

/// Incremental fold of SSE lines into text, usage, and terminal outcome.
///
/// Shared by the async streaming loop and the synchronous test driver so both
/// enforce the same terminal, error, usage, and bound behavior.
#[derive(Debug, Default)]
pub(super) struct StreamFold {
    pub(super) text: String,
    pub(super) resolved_model: Option<String>,
    pub(super) usage: Usage,
    stop_reason: Option<String>,
    pub(super) terminal: Option<TurnOutcome>,
}

impl StreamFold {
    pub(super) fn feed(
        &mut self,
        line: &str,
        wire: WireProtocol,
        on_delta: &mut impl FnMut(StreamDelta),
    ) -> Result<(), DecodeError> {
        let payload = sse_payload(line);
        if payload.trim() == "[DONE]" {
            if wire != WireProtocol::OpenAiChatCompletions || self.terminal.is_none() {
                return Err(invalid_terminal(
                    "end marker without a valid terminal outcome",
                ));
            }
            return Ok(());
        }
        let value: Value = serde_json::from_str(&payload)
            .map_err(|_| invalid_terminal("stream event is not valid JSON"))?;
        if sse_stream_error(&value) {
            return Err(DecodeError::TurnFailed);
        }
        validate_stream_content(&value, wire)?;
        if let Some(model) = reported_model(&value)? {
            self.resolved_model = Some(model);
        }
        if let Some(reason) = sse_stop_reason(&value) {
            self.stop_reason = Some(reason);
        }
        if let Some(delta) = parse_stream_delta(&value, wire) {
            if self.terminal.is_some() {
                return Err(invalid_terminal("content received after terminal outcome"));
            }
            if self.text.len().saturating_add(delta.text.len()) > MAX_STREAM_TEXT_BYTES {
                return Err(DecodeError::LimitExceeded {
                    detail: "streamed text exceeds byte bound",
                });
            }
            if !delta.text.is_empty() {
                self.text.push_str(&delta.text);
            }
            on_delta(delta);
        }
        merge_sse_usage(&mut self.usage, &value);
        if let Some(outcome) = sse_terminal(&value, wire, self.stop_reason.as_deref())? {
            if self
                .terminal
                .as_ref()
                .is_some_and(|prior| *prior != outcome)
            {
                return Err(invalid_terminal("conflicting terminal outcomes"));
            }
            if wire == WireProtocol::OpenAiResponses {
                let (text, usage, _) = parse_responses_response(
                    value
                        .get("response")
                        .ok_or_else(|| invalid_terminal("terminal event has no response"))?,
                )?;
                if text.len() > MAX_STREAM_TEXT_BYTES {
                    return Err(DecodeError::LimitExceeded {
                        detail: "terminal text exceeds byte bound",
                    });
                }
                self.text = text;
                self.usage.merge(usage);
            }
            self.terminal.get_or_insert(outcome);
        }
        Ok(())
    }
}

/// SSE joins multiple data fields with newlines before JSON decoding.
fn sse_payload(event: &str) -> String {
    event
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|data| data.strip_prefix(' ').unwrap_or(data))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether one SSE line reports a provider-side failure.
///
/// Server error text is deliberately not propagated: the fixed
/// [`DecodeError::TurnFailed`] carries no upstream excerpt, so reflected
/// credentials cannot escape through streaming errors either.
fn sse_stream_error(value: &Value) -> bool {
    has_content(value.get("error"))
        || matches!(
            value.get("type").and_then(Value::as_str),
            Some("error" | "response.failed")
        )
}

/// Capture a stop/finish reason carried by one SSE line, if any.
fn sse_stop_reason(value: &Value) -> Option<String> {
    value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("finish_reason"))
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("delta")
                .and_then(|delta| delta.get("stop_reason"))
                .and_then(Value::as_str)
        })
        .map(str::to_owned)
}

/// Whether one SSE line is the wire's terminal event, with its outcome.
fn sse_terminal(
    value: &Value,
    wire: WireProtocol,
    stop_reason: Option<&str>,
) -> Result<Option<TurnOutcome>, DecodeError> {
    match wire {
        WireProtocol::OpenAiChatCompletions => {
            let finish = value
                .get("choices")
                .and_then(Value::as_array)
                .and_then(|choices| choices.first())
                .and_then(|choice| choice.get("finish_reason"))
                .and_then(Value::as_str);
            // A null finish reason marks an in-progress delta, not a terminal
            // event; only an explicit reason string terminates the stream.
            finish.map(|reason| chat_outcome(Some(reason))).transpose()
        }
        WireProtocol::AnthropicMessages => {
            if value.get("type").and_then(Value::as_str) == Some("message_stop") {
                messages_outcome(stop_reason).map(Some)
            } else {
                Ok(None)
            }
        }
        WireProtocol::OpenAiResponses => match value.get("type").and_then(Value::as_str) {
            Some("response.completed" | "response.incomplete") => {
                let response = value
                    .get("response")
                    .ok_or_else(|| invalid_terminal("terminal event has no response"))?;
                let (_, _, outcome) = parse_responses_response(response)?;
                let expected =
                    if value.get("type").and_then(Value::as_str) == Some("response.completed") {
                        "completed"
                    } else {
                        "incomplete"
                    };
                if response.get("status").and_then(Value::as_str) != Some(expected) {
                    return Err(invalid_terminal(
                        "terminal event contradicts response status",
                    ));
                }
                Ok(Some(outcome))
            }
            _ => Ok(None),
        },
    }
}

/// Extract a streaming text/reasoning delta from one SSE line.
fn parse_stream_delta(value: &Value, wire: WireProtocol) -> Option<StreamDelta> {
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
            let block = match value.get("type").and_then(Value::as_str) {
                Some("content_block_delta") => value.get("delta")?,
                Some("content_block_start") => value.get("content_block")?,
                _ => return None,
            };
            let text = block
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

/// Merge cumulative usage without turning absent fields into reported zero.
///
/// Standard Anthropic streams announce input tokens in `message_start` and
/// cumulative output in `message_delta`; Chat chunks and Responses terminal
/// events carry their own `usage`. Explicit zeros replace previous values.
fn merge_sse_usage(usage: &mut Usage, value: &Value) {
    if let Some(seen) = value
        .get("usage")
        .or_else(|| {
            value
                .get("message")
                .and_then(|message| message.get("usage"))
        })
        .or_else(|| {
            value
                .get("response")
                .and_then(|response| response.get("usage"))
        })
    {
        usage.merge(parse_usage(seen));
    }
}

#[cfg(test)]
fn parse_sse_line(event: &str, wire: WireProtocol) -> Option<StreamDelta> {
    let value = serde_json::from_str(&sse_payload(event)).ok()?;
    parse_stream_delta(&value, wire)
}

/// Synchronous SSE driver over a complete body, mirroring the async loop's
/// per-line logic. Used by tests to cover terminal, error, usage, and bound
/// behavior without HTTP.
#[cfg(test)]
fn process_sse_body(
    body: &str,
    wire: WireProtocol,
) -> Result<(String, Usage, TurnOutcome), DecodeError> {
    let mut stream = SseStream::new();
    let mut lines = stream.push(body.as_bytes())?;
    lines.extend(stream.finish()?);
    let mut fold = StreamFold::default();
    for line in &lines {
        fold.feed(line, wire, &mut |_| {})?;
    }
    fold.terminal
        .map(|outcome| (fold.text, fold.usage, outcome))
        .ok_or(DecodeError::InvalidResponse {
            detail: "stream ended before terminal event",
        })
}

#[cfg(test)]
mod tests {
    use super::super::OpencodeGoProvider;
    use super::*;

    #[test]
    fn upstream_status_errors_exclude_body_and_credentials() {
        let secret = "oc_sk_synthetic_secret_123";
        let client =
            OpencodeClient::new(OpencodeGoProvider::instance(), secret).expect("client builds");
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
        let (text, usage, outcome) = parse_chat_response(&body).unwrap();
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
        let (text, usage, outcome) = parse_responses_response(&body).unwrap();
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
            parse_responses_response(&body),
            Err(DecodeError::TurnFailed)
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
            Err(DecodeError::TurnFailed)
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
        let err = process_sse_body(chat, WireProtocol::OpenAiChatCompletions).unwrap_err();
        assert!(matches!(
            err,
            DecodeError::InvalidResponse { detail, .. }
            if detail == "stream ended before terminal event"
        ));

        let msg = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n";
        assert!(process_sse_body(msg, WireProtocol::AnthropicMessages).is_err());

        let resp = "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n";
        assert!(process_sse_body(resp, WireProtocol::OpenAiResponses).is_err());
    }

    #[test]
    fn in_stream_provider_errors_fail_per_shape() {
        let chat = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: {\"error\":{\"message\":\"boom\"}}\n\n";
        assert!(matches!(
            process_sse_body(chat, WireProtocol::OpenAiChatCompletions),
            Err(DecodeError::TurnFailed)
        ));

        let msg = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"x\",\"message\":\"boom\"}}\n\n";
        assert!(matches!(
            process_sse_body(msg, WireProtocol::AnthropicMessages),
            Err(DecodeError::TurnFailed)
        ));

        let resp =
            "event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{}}\n\n";
        assert!(matches!(
            process_sse_body(resp, WireProtocol::OpenAiResponses),
            Err(DecodeError::TurnFailed)
        ));
    }

    #[test]
    fn complete_native_sequences_resolve() {
        let chat = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n";
        let (text, _, outcome) =
            process_sse_body(chat, WireProtocol::OpenAiChatCompletions).unwrap();
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
            process_sse_body(msg, WireProtocol::AnthropicMessages).unwrap();
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
        let (text, usage, outcome) = process_sse_body(resp, WireProtocol::OpenAiResponses).unwrap();
        assert_eq!(text, "ok");
        assert_eq!(usage.total_tokens, Some(16));
        assert_eq!(outcome, TurnOutcome::Completed);
    }

    #[test]
    fn stream_accumulator_survives_split_unicode() {
        let line = "data: {\"choices\":[{\"delta\":{\"content\":\"héllo\"}}]}\n\n";
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
            Err(DecodeError::LimitExceeded { .. })
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
            parse_chat_response(&serde_json::json!({
                "choices": [{"message": {"content": "plausible"}}]
            }))
            .is_err()
        );
        assert!(
            parse_messages_response(&serde_json::json!({
                "content": [{"type": "text", "text": "plausible"}]
            }))
            .is_err()
        );
        assert!(parse_responses_response(&serde_json::json!({"output": []})).is_err());
        assert!(chat_outcome(Some("unrecognized")).is_err());
        assert!(messages_outcome(Some("unrecognized")).is_err());
    }

    #[test]
    fn incomplete_stream_outcomes_survive_trailing_markers() {
        let chat = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":64}}\n\n",
            "data: [DONE]\n\n",
        );
        let (text, usage, outcome) =
            process_sse_body(chat, WireProtocol::OpenAiChatCompletions).unwrap();
        assert_eq!(text, "partial");
        assert_eq!(usage.total_tokens, Some(74));
        assert_eq!(outcome, incomplete(Some("max_tokens")));

        let messages = concat!(
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let (_, _, outcome) = process_sse_body(messages, WireProtocol::AnthropicMessages).unwrap();
        assert_eq!(outcome, incomplete(Some("max_tokens")));
        let responses = "data: {\"type\":\"response.incomplete\",\"response\":{\"status\":\"incomplete\",\"output\":[]}}\n\n";
        let (_, _, outcome) = process_sse_body(responses, WireProtocol::OpenAiResponses).unwrap();
        assert_eq!(outcome, incomplete(None));
    }

    #[test]
    fn bare_end_markers_do_not_prove_success_on_any_wire() {
        for wire in [
            WireProtocol::OpenAiChatCompletions,
            WireProtocol::AnthropicMessages,
            WireProtocol::OpenAiResponses,
        ] {
            assert!(process_sse_body("data: [DONE]\n\n", wire).is_err());
        }
    }

    #[test]
    fn malformed_events_and_post_terminal_content_are_rejected() {
        let terminal = "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n";
        let malformed = format!("data: {{malformed}}\n\n{terminal}");
        assert!(process_sse_body(&malformed, WireProtocol::OpenAiChatCompletions).is_err());
        let late_content =
            format!("{terminal}data: {{\"choices\":[{{\"delta\":{{\"content\":\"late\"}}}}]}}\n\n");
        assert!(process_sse_body(&late_content, WireProtocol::OpenAiChatCompletions).is_err());
        let contradictory = format!(
            "{terminal}data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"length\"}}]}}\n\n"
        );
        assert!(process_sse_body(&contradictory, WireProtocol::OpenAiChatCompletions).is_err());
    }

    #[test]
    fn multiline_sse_events_wait_for_delimiter_and_reject_bad_utf8() {
        let body = concat!(
            "event: chunk\r\n",
            "data: {\"choices\":\r\n",
            "data: [{\"delta\":{\"content\":\"héllo\"},\"finish_reason\":\"stop\"}]}\r\n\r\n",
        );
        for split in 0..body.len() {
            let mut stream = SseStream::new();
            let mut frames = stream.push(&body.as_bytes()[..split]).unwrap();
            frames.extend(stream.push(&body.as_bytes()[split..]).unwrap());
            frames.extend(stream.finish().unwrap());
            assert_eq!(frames.len(), 1);
            let mut fold = StreamFold::default();
            fold.feed(&frames[0], WireProtocol::OpenAiChatCompletions, &mut |_| {})
                .unwrap();
            assert_eq!(fold.text, "héllo");
            assert_eq!(fold.terminal, Some(TurnOutcome::Completed));
        }
        let mut invalid = SseStream::new();
        assert!(invalid.push(b"data: \xff\n\n").is_err());
        assert!(
            process_sse_body(
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
                let mut stream = SseStream::new();
                let mut events = stream.push(&body.as_bytes()[..split]).unwrap();
                events.extend(stream.push(&body.as_bytes()[split..]).unwrap());
                events.extend(stream.finish().unwrap());
                assert_eq!(events.len(), 1, "ending {ending:?}, split {split}");
                let mut fold = StreamFold::default();
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
            parse_chat_response(&chat),
            Err(DecodeError::UnsupportedCapability { .. })
        ));
        for kind in ["function_call", "web_search_call", "unrecognized"] {
            let response = serde_json::json!({"status": "completed", "output": [{"type": kind}]});
            assert!(matches!(
                parse_responses_response(&response),
                Err(DecodeError::UnsupportedCapability { .. })
            ));
        }
        let continuation = serde_json::json!({"status": "completed", "output": [{"type": "reasoning", "encrypted_content": "opaque"}]});
        assert!(matches!(
            parse_responses_response(&continuation),
            Err(DecodeError::UnsupportedCapability { .. })
        ));
        let messages = serde_json::json!({"stop_reason": "end_turn", "content": [{"type": "tool_use", "id": "call-1"}]});
        assert!(matches!(
            parse_messages_response(&messages),
            Err(DecodeError::UnsupportedCapability { .. })
        ));
        let stream = "data: {\"type\":\"content_block_start\",\"content_block\":{\"type\":\"tool_use\"}}\n\n";
        assert!(matches!(
            process_sse_body(stream, WireProtocol::AnthropicMessages),
            Err(DecodeError::UnsupportedCapability { .. })
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
            let mut fold = StreamFold::default();
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
        let client = OpencodeClient::new(OpencodeGoProvider::instance(), "synthetic-key").unwrap();
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
        let mut fold = StreamFold::default();
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
