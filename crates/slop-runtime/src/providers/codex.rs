//! Headless Codex model access through the public OpenAI Responses API.
//!
//! Authentication uses a renewable Sign in with ChatGPT connection or a
//! Platform API key. Subscription requests use the documented plan-usage route.
//! Execution stays in the shared Rust runtime: this adapter never starts the
//! Codex CLI/app-server or reads their cached subscription credentials.
//! See https://developers.openai.com/api/docs/guides/text and
//! https://learn.chatgpt.com/docs/models for the API and model references.

use std::sync::Arc;

use super::chatgpt_auth::ChatGptConnection;
use slop_core::provider::ProviderId;

use super::{
    ChatRequest, ChatResponse, Provider, ProviderClient, ProviderError, ProviderFuture,
    ProviderModel, StreamDelta, WireProtocol, transport, wire,
};

pub const BASE_URL: &str = "https://api.openai.com/v1";
pub const ENV_KEY_VAR: &str = "OPENAI_API_KEY";

/// Explicit Responses mappings, verified against official docs on 2026-10-08.
/// This catalog declares adapter support; account entitlement is checked by
/// the upstream service. Live discovery does not enable unknown model ids.
pub const MODELS: &[ProviderModel] = &[
    model("gpt-6.1-sol", "GPT 6.1 Sol"),
    model("gpt-6-sol", "GPT 6 Sol"),
    model("gpt-6-luna", "GPT 6 Luna"),
    model("gpt-6-astra", "GPT 6 Astra"),
];

const fn model(id: &'static str, display_name: &'static str) -> ProviderModel {
    ProviderModel {
        id,
        display_name,
        wire: WireProtocol::OpenAiResponses,
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CodexProvider;

impl CodexProvider {
    #[must_use]
    pub fn instance() -> &'static Self {
        static INSTANCE: CodexProvider = CodexProvider;
        &INSTANCE
    }
}

impl Provider for CodexProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Codex
    }

    fn display_name(&self) -> &'static str {
        "Codex"
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

    fn auth_modes(&self) -> &'static [super::AuthMode] {
        &[
            super::AuthMode::ChatGptSubscription,
            super::AuthMode::ApiKey,
        ]
    }

    fn requires_output_limit(&self) -> bool {
        false
    }
}

enum Authentication {
    ApiKey(String),
    ChatGpt(Arc<ChatGptConnection>),
}

/// Reusable authenticated connection with a shared HTTP pool and no session
/// execution state. Deliberately lacks `Debug` to keep credentials out of logs.
pub struct CodexClient {
    http: reqwest::Client,
    auth: Authentication,
    base_url: String,
}

impl CodexClient {
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

    /// Attach a daemon-owned subscription registration. Share the same
    /// connection across all sessions using this account to serialize refresh.
    pub fn from_chatgpt(connection: Arc<ChatGptConnection>) -> Result<Self, ProviderError> {
        Ok(Self {
            http: transport::http_client()?,
            auth: Authentication::ChatGpt(connection),
            base_url: BASE_URL.to_owned(),
        })
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
                    .filter(|id| slop_core::provider::is_valid_model_id(id))
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
        let response = self.inference(request, true).await?.send().await?;
        transport::check_status(ProviderId::Codex, &response)?;
        if self.is_subscription() && !wire::is_event_stream(&response) {
            return Err(ProviderError::InvalidResponse {
                provider: ProviderId::Codex,
                detail: "subscription endpoint did not return SSE",
            });
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
    fn descriptor(&self) -> &dyn Provider {
        CodexProvider::instance()
    }

    fn auth_mode(&self) -> super::AuthMode {
        if self.is_subscription() {
            super::AuthMode::ChatGptSubscription
        } else {
            super::AuthMode::ApiKey
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
                .any(|message| message.role == super::Role::System)
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

#[cfg(test)]
mod tests {
    use super::super::{
        ChatMessage, TurnOutcome, UsageSource,
        chatgpt_auth::fixture_connection,
        test_http::{Reply, Server},
    };
    use super::*;

    fn request() -> ChatRequest {
        ChatRequest {
            model: "gpt-6.1-sol".to_owned(),
            messages: vec![
                ChatMessage {
                    role: super::super::Role::Developer,
                    content: "fixture instruction".to_owned(),
                },
                ChatMessage::user("fixture prompt"),
            ],
            max_tokens: None,
            session_id: "fixture-session".to_owned(),
        }
    }

    fn completed(text: &str) -> serde_json::Value {
        serde_json::json!({"status":"completed","model":"fixture-resolved-model","output":[{"type":"message","content":[{"type":"output_text","text":text}]}],"usage":{"input_tokens":5,"output_tokens":2}})
    }

    fn stream() -> String {
        format!(
            "data: {{\"type\":\"response.output_text.delta\",\"delta\":\"héllo\"}}\n\ndata: {}\n\n",
            serde_json::json!({"type":"response.completed","response":completed("héllo")})
        )
    }

    fn subscription(server: &Server) -> CodexClient {
        let connection = Arc::new(fixture_connection(std::path::PathBuf::new(), u64::MAX));
        let mut client = CodexClient::from_chatgpt(connection).unwrap();
        client.base_url = server.url.clone();
        client
    }

    #[tokio::test]
    async fn subscription_discovery_and_both_completion_modes_use_shared_surface() {
        let server = Server::new(vec![Reply::json(serde_json::json!({"models":[{"slug":"gpt-6.1-sol","visibility":"list"},{"slug":"hidden-model","visibility":"hide"}]})), Reply::sse(stream()), Reply::sse(stream())]).await;
        let concrete = subscription(&server);
        let client: &dyn ProviderClient = &concrete;
        assert_eq!(client.list_models().await.unwrap(), vec!["gpt-6.1-sol"]);
        let first = client.complete(&request()).await.unwrap();
        let mut text = String::new();
        let second = client
            .complete_streaming(&request(), &mut |delta| text.push_str(&delta.text))
            .await
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(text, "héllo");
        assert_eq!(first.outcome, TurnOutcome::Completed);
        assert_eq!(
            first.resolved_model.as_deref(),
            Some("fixture-resolved-model")
        );
        assert_eq!(first.usage.total_tokens, Some(7));
        assert_eq!(first.usage.total_source, Some(UsageSource::Derived));
        server.inspect(|requests| {
            assert_eq!(requests.len(), 3);
            assert_eq!(requests[0].path, "/models");
            for sent in requests {
                assert_eq!(sent.headers["authorization"], "Bearer fixture-access-token");
                assert!(
                    sent.headers["user-agent"]
                        .to_str()
                        .unwrap()
                        .starts_with("slopconductor/")
                );
                assert!(!sent.headers.contains_key("x-opencode-session"));
                assert!(!sent.body.contains("fixture-access-token"));
                if sent.path == "/responses" {
                    let body: serde_json::Value = serde_json::from_str(&sent.body).unwrap();
                    assert_eq!(body["store"], false);
                    assert_eq!(body["stream"], true);
                    assert!(body.get("max_output_tokens").is_none());
                    assert_eq!(body["input"][0]["role"], "developer");
                    assert_eq!(body["input"][1]["role"], "user");
                    assert_eq!(body["prompt_cache_key"], "fixture-session");
                }
            }
        });
    }

    #[tokio::test]
    async fn subscription_rejects_ignored_stream_setting() {
        let server = Server::new(vec![Reply::json(completed("unexpected JSON"))]).await;
        let client = subscription(&server);
        assert!(matches!(
            client.complete(&request()).await,
            Err(ProviderError::InvalidResponse {
                provider: ProviderId::Codex,
                ..
            })
        ));
        server.inspect(|requests| assert_eq!(requests.len(), 1));
    }

    #[tokio::test]
    async fn unsupported_configuration_fails_before_network_or_deltas() {
        let server = Server::new(vec![]).await;
        let client = subscription(&server);
        for invalid in [
            ChatRequest {
                max_tokens: Some(64),
                ..request()
            },
            ChatRequest {
                model: "unknown-model".to_owned(),
                ..request()
            },
            ChatRequest {
                messages: vec![ChatMessage {
                    role: super::super::Role::System,
                    content: "instruction".to_owned(),
                }],
                ..request()
            },
        ] {
            assert!(client.complete(&invalid).await.is_err());
            let mut emitted = false;
            assert!(
                client
                    .complete_streaming(&invalid, |_| emitted = true)
                    .await
                    .is_err()
            );
            assert!(!emitted);
        }
        server.inspect(|requests| assert!(requests.is_empty()));
    }

    #[tokio::test]
    async fn incomplete_failed_truncated_and_structured_turns_do_not_become_success() {
        let mut incomplete = completed("partial");
        incomplete["status"] = "incomplete".into();
        incomplete["incomplete_details"] = serde_json::json!({"reason":"max_output_tokens"});
        let cases = [
            (format!("data: {}\n\n",serde_json::json!({"type":"response.incomplete","response":incomplete})), true),
            ("data: {\"type\":\"response.failed\",\"response\":{\"error\":\"fixture-access-token\"}}\n\n".to_owned(), false),
            ("data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n".to_owned(), false),
            ("data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\"}}\n\n".to_owned(), false),
        ];
        for (body, preserves_partial) in cases {
            let server = Server::new(vec![Reply::sse(body)]).await;
            let client = subscription(&server);
            let result = client.complete(&request()).await;
            if preserves_partial {
                let result = result.unwrap();
                assert_eq!(result.text, "partial");
                assert!(matches!(result.outcome, TurnOutcome::Incomplete { .. }));
            } else {
                let error = result.unwrap_err();
                assert!(!error.to_string().contains("fixture-access-token"));
                if let ProviderError::InvalidResponse { provider, .. }
                | ProviderError::TurnFailed { provider } = error
                {
                    assert_eq!(provider, ProviderId::Codex);
                }
            }
            server.inspect(|requests| assert_eq!(requests.len(), 1));
        }
    }

    #[tokio::test]
    async fn api_key_path_preserves_cap_and_roles_and_redirects_are_not_followed() {
        let destination = Server::new(vec![Reply::json(completed("unexpected redirect"))]).await;
        let server = Server::new(vec![
            Reply::json(completed("ok")),
            Reply {
                status: axum::http::StatusCode::TEMPORARY_REDIRECT,
                content_type: "application/json",
                body: "fixture-key reflected secret".to_owned(),
                location: Some(destination.url.clone()),
            },
        ])
        .await;
        let mut concrete = CodexClient::new("fixture-key").unwrap();
        concrete.base_url = server.url.clone();
        let client: &dyn ProviderClient = &concrete;
        let mut input = request();
        input.max_tokens = Some(64);
        input.messages.insert(
            0,
            ChatMessage {
                role: super::super::Role::System,
                content: "leading instruction".to_owned(),
            },
        );
        assert_eq!(client.complete(&input).await.unwrap().text, "ok");
        let error = client.complete(&input).await.unwrap_err();
        assert!(matches!(
            error,
            ProviderError::UnexpectedStatus {
                provider: ProviderId::Codex,
                status: 307
            }
        ));
        assert!(!error.to_string().contains("fixture-key"));
        server.inspect(|requests| {
            assert_eq!(requests.len(), 2);
            let body: serde_json::Value = serde_json::from_str(&requests[0].body).unwrap();
            assert_eq!(body["max_output_tokens"], 64);
            assert_eq!(body["stream"], false);
            assert_eq!(body["input"][0]["role"], "system");
        });
        destination.inspect(|requests| assert!(requests.is_empty()));
        assert!(CodexClient::new("  ").is_err());
    }
}
