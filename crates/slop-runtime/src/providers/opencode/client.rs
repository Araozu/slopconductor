use super::decode::{
    parse_chat_response, parse_messages_response, parse_models_list, parse_responses_response,
};
use super::requests::{to_anthropic_parts, to_openai_messages, to_responses_input};
use super::sse::{SseStream, is_event_stream};
use super::stream::StreamFold;
use super::*;

pub(in crate::providers) struct OpencodeClient {
    provider: &'static dyn Provider,
    pub(super) base_url: String,
    http: reqwest::Client,
    api_key: String,
    continuation_scope: String,
}

impl OpencodeClient {
    pub async fn infer(
        &self,
        request: &super::super::inference::InferenceRequest,
        on_event: &mut (dyn FnMut(super::super::inference::ProviderEvent) + Send),
    ) -> Result<super::super::inference::InferenceResponse, ProviderError> {
        let wire = request.validate(
            self.provider,
            &super::super::inference::capabilities(self.provider, &request.model),
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
        let payload = super::super::structured_wire::encode(
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
        let req = req
            .header(SESSION_HEADER, &request.session_id)
            .header("Accept", "text/event-stream")
            .json(&payload);
        // The request builder owns the encoded body; the intermediate JSON
        // tree must not stay alive throughout inference and response streaming.
        drop(payload);
        let mut response = req.send().await?;
        super::super::transport::check_status(self.provider.id(), &mut response).await?;
        if !is_event_stream(&response) {
            let body = self.read_body_limited(response).await?;
            let value =
                serde_json::from_str(&body).map_err(|_| ProviderError::InvalidResponse {
                    provider: self.provider.id(),
                    detail: "response body is not JSON",
                })?;
            let mut result =
                super::super::structured_wire::decode(&value, wire, request, self.provider.id())
                    .map_err(|error| error.for_provider(self.provider.id()))?;
            if let Some(c) = &mut result.message.continuation {
                c.scope = self.continuation_scope.clone();
            }
            return Ok(result);
        }
        let mut framing = SseStream::new();
        let mut fold = super::super::structured_wire::StructuredFold::new(wire);
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

    #[cfg(test)]
    pub(super) fn status_error(&self, status: u16) -> ProviderError {
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
        let mut response = self
            .http
            .get(format!("{}/models", self.base_url))
            .bearer_auth(&self.api_key)
            .header(SESSION_HEADER, "slop-model-discovery")
            .send()
            .await?;
        super::super::transport::check_status(self.provider.id(), &mut response).await?;
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
        super::super::transport::check_status(self.provider.id(), &mut response).await?;
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

    pub(super) fn blocking_from_value(
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
        let mut response = req
            .header(SESSION_HEADER, session_id)
            .json(&payload)
            .send()
            .await?;
        super::super::transport::check_status(self.provider.id(), &mut response).await?;
        let body = self.read_body_limited(response).await?;
        serde_json::from_str(&body).map_err(|_| ProviderError::InvalidResponse {
            provider: self.provider.id(),
            detail: "response body is not JSON",
        })
    }
}

pub(in crate::providers) fn validate_base_url(value: &str) -> Result<String, ProviderError> {
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
