use std::sync::Arc;

use hive_core::provider::ProviderId;
use sha2::{Digest, Sha256};

use super::super::chatgpt_auth::ChatGptConnection;
use super::super::inference::{
    InferenceRequest, InferenceResponse, ModelCapabilities, ProviderEvent,
};
use super::super::{
    ChatRequest, ChatResponse, Provider, ProviderClient, ProviderError, ProviderFuture,
    StreamDelta, WireProtocol, transport, wire,
};
use super::{BASE_URL, ENV_KEY_VAR};

enum Authentication {
    ApiKey(String),
    ChatGpt(Arc<ChatGptConnection>),
}

/// Reusable authenticated connection with a shared HTTP pool and no session
/// execution state. Deliberately lacks `Debug` to keep credentials out of logs.
pub struct CodexClient {
    http: reqwest::Client,
    auth: Authentication,
    pub(super) base_url: String,
}

impl CodexClient {
    async fn continuation_scope(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(b"slop-codex-responses-v1\0");
        hash.update(self.base_url.as_bytes());
        hash.update([0]);
        match &self.auth {
            Authentication::ApiKey(key) => {
                hash.update(b"api-key\0");
                hash.update(key.as_bytes());
            }
            Authentication::ChatGpt(connection) => {
                hash.update(b"chatgpt\0");
                hash.update(connection.continuation_identity().await.as_bytes());
            }
        }
        format!("{:x}", hash.finalize())
    }

    /// One structured inference attempt. The daemon owns tools, context and
    /// cancellation; this future owns only a bounded HTTP stream.
    pub async fn infer(
        &self,
        request: &InferenceRequest,
        on_event: &mut (dyn FnMut(ProviderEvent) + Send),
    ) -> Result<InferenceResponse, ProviderError> {
        let wire = request.validate(self.descriptor(), &self.capabilities(&request.model))?;
        if self.is_subscription() {
            if request.settings.max_output_tokens.is_some() {
                return Err(ProviderError::UnsupportedCapability {
                    capability: "max_output_tokens for ChatGPT plan usage",
                });
            }
            if request
                .messages
                .iter()
                .any(|message| message.role == super::super::inference::MessageRole::System)
            {
                return Err(ProviderError::UnsupportedCapability {
                    capability: "system messages for ChatGPT plan usage",
                });
            }
        }
        let scope = self.continuation_scope().await;
        for message in &request.messages {
            if message
                .continuation
                .as_ref()
                .is_some_and(|c| c.required && c.scope != scope)
            {
                return Err(ProviderError::UnsupportedCapability {
                    capability: "context_incompatible: required account or endpoint continuation",
                });
            }
        }
        let mut payload =
            super::super::structured_wire::encode(request, wire, ProviderId::Codex, &scope)?;
        payload["prompt_cache_key"] = request.session_id.clone().into();
        if self.is_subscription()
            && let Some(tools) = payload
                .as_object_mut()
                .and_then(|body| body.remove("tools"))
        {
            // Plan usage accepts function tools through additional_tools input
            // items. Tool results still use the ordinary function-call IDs.
            payload["input"]
                .as_array_mut()
                .expect("encoded input array")
                .insert(
                    0,
                    serde_json::json!({"type":"additional_tools","role":"developer","tools":tools}),
                );
        }
        let req = self
            .http
            .post(format!("{}/responses", self.base_url))
            .bearer_auth(self.access_token().await?)
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .json(&payload);
        drop(payload);
        let response = req.send().await?;
        let mut result = self
            .read_inference_response(request, wire, response, on_event)
            .await?;
        if let Some(continuation) = &mut result.message.continuation {
            continuation.scope = scope;
        }
        Ok(result)
    }

    pub(super) async fn read_inference_response(
        &self,
        request: &InferenceRequest,
        wire: WireProtocol,
        mut response: reqwest::Response,
        on_event: &mut (dyn FnMut(ProviderEvent) + Send),
    ) -> Result<InferenceResponse, ProviderError> {
        transport::check_status(ProviderId::Codex, &mut response).await?;
        // Subscription requests always ask for SSE. The direct route can omit
        // Content-Type even while returning valid Responses events. Parse the
        // stream itself and require terminal evidence instead of trusting that
        // header alone. JSON and HTML cannot complete the SSE decoder.
        if self.is_subscription() || wire::is_event_stream(&response) {
            let mut framing = super::super::opencode::SseStream::new();
            let mut fold = super::super::structured_wire::StructuredFold::new(wire);
            while let Some(chunk) = response.chunk().await? {
                for event in framing
                    .push(&chunk)
                    .map_err(|e| e.for_provider(ProviderId::Codex))?
                {
                    fold.feed(&event, on_event)
                        .map_err(|e| e.for_provider(ProviderId::Codex))?;
                }
            }
            for event in framing
                .finish()
                .map_err(|e| e.for_provider(ProviderId::Codex))?
            {
                fold.feed(&event, on_event)
                    .map_err(|e| e.for_provider(ProviderId::Codex))?;
            }
            fold.finish(request, ProviderId::Codex)
                .map_err(|e| e.for_provider(ProviderId::Codex))
        } else {
            let body = transport::read_body_limited(ProviderId::Codex, response).await?;
            let value =
                serde_json::from_str(&body).map_err(|_| ProviderError::InvalidResponse {
                    provider: ProviderId::Codex,
                    detail: "response body is not JSON",
                })?;
            super::super::structured_wire::decode(&value, wire, request, ProviderId::Codex)
                .map_err(|e| e.for_provider(ProviderId::Codex))
        }
    }

    pub fn new(api_key: &str) -> Result<Self, ProviderError> {
        if api_key.trim().is_empty() {
            return Err(ProviderError::EmptyApiKey {
                env_var: ENV_KEY_VAR,
            });
        }
        Ok(Self {
            http: transport::http_client()?,
            auth: Authentication::ApiKey(api_key.to_owned()),
            base_url: BASE_URL.to_owned(),
        })
    }

    pub fn new_with_base_url(api_key: &str, base_url: &str) -> Result<Self, ProviderError> {
        let mut client = Self::new(api_key)?;
        client.base_url = super::super::opencode::validate_base_url(base_url)?;
        Ok(client)
    }

    /// Attach a daemon-owned subscription registration. Share the same
    /// connection across all sessions using this account to serialize refresh.
    pub fn from_chatgpt(connection: Arc<ChatGptConnection>) -> Result<Self, ProviderError> {
        Ok(Self {
            http: transport::http_client()?,
            auth: Authentication::ChatGpt(connection),
            base_url: BASE_URL.to_owned(),
        })
    }

    pub fn from_chatgpt_with_base_url(
        connection: Arc<ChatGptConnection>,
        base_url: &str,
    ) -> Result<Self, ProviderError> {
        let mut client = Self::from_chatgpt(connection)?;
        client.base_url = super::super::opencode::validate_base_url(base_url)?;
        Ok(client)
    }

    async fn access_token(&self) -> Result<String, ProviderError> {
        match &self.auth {
            Authentication::ApiKey(key) => Ok(key.clone()),
            Authentication::ChatGpt(connection) => connection.access_token().await,
        }
    }

    fn is_subscription(&self) -> bool {
        matches!(self.auth, Authentication::ChatGpt(_))
    }

    pub fn from_env() -> Result<Self, ProviderError> {
        let key = std::env::var(ENV_KEY_VAR).map_err(|_| ProviderError::MissingApiKey {
            env_var: ENV_KEY_VAR,
        })?;
        Self::new(&key)
    }

    /// Fetch advertised API model ids. Unknown ids remain unavailable through
    /// `validate`, even if the account's live catalog includes them.
    pub async fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        let response = self
            .http
            .get(format!("{}/models", self.base_url))
            .bearer_auth(self.access_token().await?)
            .send()
            .await?;
        let body = transport::read_body_limited(ProviderId::Codex, response).await?;
        if self.is_subscription() {
            let value: serde_json::Value =
                serde_json::from_str(&body).map_err(|_| ProviderError::InvalidResponse {
                    provider: ProviderId::Codex,
                    detail: "model list is not JSON",
                })?;
            let models = value
                .get("models")
                .and_then(serde_json::Value::as_array)
                .ok_or(ProviderError::InvalidResponse {
                    provider: ProviderId::Codex,
                    detail: "subscription model list has no models array",
                })?;
            let mut ids = Vec::new();
            for model in models {
                let id = model
                    .get("slug")
                    .and_then(serde_json::Value::as_str)
                    .filter(|id| hive_core::provider::is_valid_model_id(id))
                    .ok_or(ProviderError::InvalidResponse {
                        provider: ProviderId::Codex,
                        detail: "subscription model entry has no valid slug",
                    })?;
                if model.get("visibility").and_then(serde_json::Value::as_str) == Some("list") {
                    ids.push(id.to_owned());
                }
            }
            Ok(ids)
        } else {
            wire::parse_models_list(ProviderId::Codex, &body)
        }
    }

    /// Perform one inference attempt without hidden retry or fallback.
    pub async fn complete(&self, request: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        self.validate(request)?;
        if self.is_subscription() {
            // A collected result still consumes exactly one streamed request;
            // subscription HTTP inference cannot use stream=false.
            return self.complete_streaming(request, |_| {}).await;
        }
        let response = self.inference(request, false).await?.send().await?;
        let body = transport::read_body_limited(ProviderId::Codex, response).await?;
        let value = serde_json::from_str(&body).map_err(|_| ProviderError::InvalidResponse {
            provider: ProviderId::Codex,
            detail: "response body is not JSON",
        })?;
        wire::decode_response(
            ProviderId::Codex,
            WireProtocol::OpenAiResponses,
            &request.model,
            &value,
        )
    }

    pub async fn complete_streaming(
        &self,
        request: &ChatRequest,
        mut on_delta: impl FnMut(StreamDelta) + Send,
    ) -> Result<ChatResponse, ProviderError> {
        self.validate(request)?;
        let mut response = self.inference(request, true).await?.send().await?;
        transport::check_status(ProviderId::Codex, &mut response).await?;
        if self.is_subscription() {
            return transport::read_sse_response(
                ProviderId::Codex,
                WireProtocol::OpenAiResponses,
                &request.model,
                response,
                &mut on_delta,
            )
            .await;
        }
        transport::read_streaming_response(
            ProviderId::Codex,
            WireProtocol::OpenAiResponses,
            &request.model,
            response,
            &mut on_delta,
        )
        .await
    }

    async fn inference(
        &self,
        request: &ChatRequest,
        stream: bool,
    ) -> Result<reqwest::RequestBuilder, ProviderError> {
        // Keep every role and message boundary. The session id is a cache hint,
        // not upstream conversation storage or execution ownership.
        let input: Vec<_> = request
            .messages
            .iter()
            .map(|message| {
                serde_json::json!({
                    "role": message.role.as_str(),
                    "content": message.content,
                })
            })
            .collect();
        let mut payload = serde_json::json!({
            "model": request.model, "input": input,
            "prompt_cache_key": request.session_id, "store": false, "stream": stream,
        });
        if let Some(limit) = request.max_tokens {
            payload["max_output_tokens"] = limit.into();
        }
        Ok(self
            .http
            .post(format!("{}/responses", self.base_url))
            .bearer_auth(self.access_token().await?)
            .header(
                reqwest::header::ACCEPT,
                if stream {
                    "text/event-stream"
                } else {
                    "application/json"
                },
            )
            .json(&payload))
    }
}

impl ProviderClient for CodexClient {
    fn capabilities(&self, model: &str) -> ModelCapabilities {
        super::model_capabilities(model)
    }

    fn infer<'a>(
        &'a self,
        request: &'a InferenceRequest,
        on_event: &'a mut (dyn FnMut(ProviderEvent) + Send),
    ) -> ProviderFuture<'a, InferenceResponse> {
        Box::pin(Self::infer(self, request, on_event))
    }

    fn descriptor(&self) -> &dyn Provider {
        super::CodexProvider::instance()
    }

    fn auth_mode(&self) -> super::super::AuthMode {
        if self.is_subscription() {
            super::super::AuthMode::ChatGptSubscription
        } else {
            super::super::AuthMode::ApiKey
        }
    }

    fn validate(&self, request: &ChatRequest) -> Result<WireProtocol, ProviderError> {
        request.validate()?;
        let wire = self.descriptor().wire_protocol(&request.model)?;
        if self.is_subscription() {
            if request.max_tokens.is_some() {
                return Err(ProviderError::UnsupportedCapability {
                    capability: "max_tokens for ChatGPT plan usage",
                });
            }
            if request
                .messages
                .iter()
                .any(|message| message.role == super::super::Role::System)
            {
                return Err(ProviderError::UnsupportedCapability {
                    capability: "system messages for ChatGPT plan usage",
                });
            }
        }
        Ok(wire)
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
