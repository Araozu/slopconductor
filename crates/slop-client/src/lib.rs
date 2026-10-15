//! Daemon API client reusable by the CLI and a future native TUI.
//!
//! Clients communicate through the public protocol and do not embed execution.

use std::{fmt, path::Path, time::Duration};

use reqwest::header::{AUTHORIZATION, HeaderValue};
use slop_protocol::{
    API_VERSION, HEALTH_PATH, HealthResponse, NODE_PATH, NodeResponse, SERVICE_NAME,
    chat::{
        CancelTurnRequest, CommandReceipt, CreateSessionRequest, DeleteMessageRequest,
        EditMessageRequest, EventFrame, ForkSessionRequest, MessageResponse, ModelResponse, Page,
        RenameSessionRequest, SendMessageRequest, SessionResponse, TurnResponse,
        UpdateSessionInboxRequest,
    },
    providers::{
        LoginResponse, LoginStatus, ProviderAccount, ProviderCatalog, ProviderStatus,
        SelectAuthRequest, SetApiKeyRequest, SignOutResponse, StartLoginRequest,
    },
};
use url::Url;

#[cfg(test)]
use sha2::{Digest, Sha256};
#[cfg(test)]
use streams::MAX_EVENT_FRAME_BYTES;

mod error;
mod execution;
mod orchestration;
mod peers;
mod providers;
mod sessions;
mod streams;
mod transport;
mod turns;
mod urls;
mod workspaces;

pub use error::ClientError;
pub use streams::{ArtifactStream, EventStream};
use transport::{api_error, read_bounded, read_bounded_to, read_token_file};
use urls::{append_page_query, id_url, query_url, session_url};

const MAX_JSON_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone)]
pub struct DaemonClient {
    http: reqwest::Client,
    stream_http: reqwest::Client,
    endpoint: Url,
    token: Option<HeaderValue>,
    peer: Option<String>,
    expected_owner: Option<HeaderValue>,
}

impl fmt::Debug for DaemonClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DaemonClient")
            .field("endpoint", &self.endpoint)
            .field("token", &self.token.as_ref().map(|_| "[REDACTED]"))
            .finish_non_exhaustive()
    }
}

impl DaemonClient {
    pub fn new(endpoint: &str) -> Result<Self, ClientError> {
        Self::with_token(endpoint, None)
    }

    /// Construct a client with a bearer token read from a private token file.
    /// The file is bounded and accepts 64 ASCII hex digits plus an optional LF
    /// or CRLF newline.
    pub fn new_with_token_file(endpoint: &str, path: &Path) -> Result<Self, ClientError> {
        let token = read_token_file(path)?;
        Self::with_token(endpoint, Some(token))
    }

    fn with_token(endpoint: &str, token: Option<String>) -> Result<Self, ClientError> {
        let endpoint = Url::parse(endpoint)?;
        if !matches!(endpoint.scheme(), "http" | "https") {
            return Err(ClientError::UnsupportedScheme);
        }
        if endpoint.path() != "/"
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
        {
            return Err(ClientError::InvalidOrigin);
        }

        let token = token
            .map(|token| {
                HeaderValue::from_str(&format!("Bearer {token}"))
                    .map(|mut value| {
                        value.set_sensitive(true);
                        value
                    })
                    .map_err(|_| ClientError::InvalidTokenFile)
            })
            .transpose()?;
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .build()?;
        // Event streams can legitimately stay open for the whole lifetime of a
        // client. Keep the connect bound, but do not apply the regular request
        // timeout to this independent pool.
        let stream_http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .build()?;

        Ok(Self {
            http,
            stream_http,
            endpoint,
            token,
            peer: None,
            expected_owner: None,
        })
    }

    pub async fn health(&self) -> Result<HealthResponse, ClientError> {
        // The selected peer's health route is protected by the gateway. Check
        // that gateway anonymously before sending its administrator credential.
        if self.peer.is_some() {
            self.health_at(self.endpoint.join(HEALTH_PATH)?, false)
                .await?;
        }
        self.health_at(
            self.routed_url(self.endpoint.join(HEALTH_PATH)?)?,
            self.peer.is_some(),
        )
        .await
    }

    async fn health_at(
        &self,
        endpoint: Url,
        authenticated: bool,
    ) -> Result<HealthResponse, ClientError> {
        let request = self.http.get(endpoint);
        let request = if authenticated {
            self.authorized(request)
        } else {
            request
        };
        let response = request.send().await?;
        let status = response.status();
        let body = read_bounded(response).await?;

        if !status.is_success() {
            return Err(api_error(status.as_u16(), &body, None));
        }
        let response: HealthResponse =
            serde_json::from_slice(&body).map_err(|_| ClientError::InvalidResponse)?;

        if response.service != SERVICE_NAME {
            return Err(ClientError::UnexpectedService(response.service));
        }
        if response.api_version != API_VERSION {
            return Err(ClientError::IncompatibleApi {
                expected: API_VERSION,
                actual: response.api_version,
            });
        }

        Ok(response)
    }

    /// Query the protected node endpoint. Health is checked anonymously first,
    /// so a token is never sent to an endpoint with an unknown identity/API.
    pub async fn node(&self) -> Result<NodeResponse, ClientError> {
        self.health().await?;
        let endpoint = self.routed_url(self.endpoint.join(NODE_PATH)?)?;
        let request = self.authorized(self.http.get(endpoint));
        let response = request.send().await?;
        let status = response.status();
        let body = read_bounded(response).await?;
        if !status.is_success() {
            return Err(api_error(status.as_u16(), &body, self.token.as_ref()));
        }
        serde_json::from_slice(&body).map_err(|_| ClientError::InvalidResponse)
    }

    pub fn token_is_configured(&self) -> bool {
        self.token.is_some()
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: Url) -> Result<T, ClientError> {
        self.health().await?;
        let response = self
            .authorized(self.http.get(self.routed_url(url)?))
            .send()
            .await?;
        self.decode_json(response).await
    }

    async fn post_json<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        url: Url,
        body: &B,
    ) -> Result<T, ClientError> {
        self.mutate_json(reqwest::Method::POST, url, body).await
    }

    async fn mutate_json<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        method: reqwest::Method,
        url: Url,
        body: &B,
    ) -> Result<T, ClientError> {
        let body = serde_json::to_vec(body).map_err(|_| ClientError::InvalidResponse)?;
        if body.len() > MAX_JSON_BYTES {
            return Err(ClientError::RequestTooLarge);
        }
        self.health().await?;
        let response = self
            .authorized(
                self.http
                    .request(method, self.routed_url(url)?)
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(body),
            )
            .send()
            .await
            .map_err(|error| ClientError::DeliveryUncertain(error.to_string()))?;
        let status = response.status();
        let body = read_bounded_to(response, MAX_JSON_BYTES)
            .await
            .map_err(|_| {
                ClientError::DeliveryUncertain(
                    "the mutation response was lost or invalid".to_owned(),
                )
            })?;
        if !status.is_success() {
            return Err(api_error(status.as_u16(), &body, self.token.as_ref()));
        }
        serde_json::from_slice(&body).map_err(|_| {
            ClientError::DeliveryUncertain(
                "daemon accepted a mutation but returned an invalid response".to_owned(),
            )
        })
    }

    async fn require_feature(&self, feature: &'static str) -> Result<(), ClientError> {
        if !self
            .health()
            .await?
            .capabilities
            .iter()
            .any(|f| f == feature)
        {
            return Err(ClientError::UnsupportedCapability(feature));
        }
        Ok(())
    }

    fn authorized(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let request = if let Some(owner) = &self.expected_owner {
            request.header("x-slop-expected-owner-node-id", owner.clone())
        } else {
            request
        };
        if let Some(token) = &self.token {
            request.header(AUTHORIZATION, token.clone())
        } else {
            request
        }
    }

    async fn decode_json<T: serde::de::DeserializeOwned>(
        &self,
        response: reqwest::Response,
    ) -> Result<T, ClientError> {
        let status = response.status();
        let body = read_bounded_to(response, MAX_JSON_BYTES).await?;
        if !status.is_success() {
            return Err(api_error(status.as_u16(), &body, self.token.as_ref()));
        }
        serde_json::from_slice(&body).map_err(|_| ClientError::InvalidResponse)
    }
}

/// Generate one globally unique opaque command identifier. Call once per
/// intended mutation and reuse its result for any explicit retry.
pub fn new_command_id() -> Result<String, ClientError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| ClientError::CommandId)?;
    let mut value = String::with_capacity(32);
    for byte in bytes {
        use fmt::Write as _;
        let _ = write!(&mut value, "{byte:02x}");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{
        Json, Router,
        extract::State,
        http::{HeaderMap, StatusCode, header::AUTHORIZATION},
        routing::{any, get},
    };
    use slop_protocol::{API_VERSION, ErrorResponse, HealthResponse, NodeResponse};

    use super::*;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    struct TestState {
        api_version: u32,
        seen: Arc<Mutex<Vec<String>>>,
        unauthorized: bool,
        capabilities: Vec<String>,
    }

    async fn server(api_version: u32, seen: Arc<Mutex<Vec<String>>>, unauthorized: bool) -> String {
        server_with_capabilities(
            api_version,
            seen,
            unauthorized,
            vec!["health".into(), "node".into()],
        )
        .await
    }

    async fn server_with_capabilities(
        api_version: u32,
        seen: Arc<Mutex<Vec<String>>>,
        unauthorized: bool,
        capabilities: Vec<String>,
    ) -> String {
        async fn health(State(state): State<Arc<TestState>>) -> Json<HealthResponse> {
            Json(HealthResponse {
                service: SERVICE_NAME.into(),
                version: "test".into(),
                api_version: state.api_version,
                capabilities: state.capabilities.clone(),
            })
        }
        async fn node(
            State(state): State<Arc<TestState>>,
            headers: HeaderMap,
        ) -> (StatusCode, Json<serde_json::Value>) {
            let authorization = headers
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            state.seen.lock().unwrap().push(authorization.clone());
            if state.unauthorized {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!(ErrorResponse {
                        code: format!("unauthorized-{authorization}"),
                        message: format!("Rejected credential {authorization}"),
                    })),
                );
            }
            (
                StatusCode::OK,
                Json(serde_json::json!(NodeResponse {
                    node_id: "node-1".into(),
                    name: "test-node".into(),
                    os: "linux".into(),
                })),
            )
        }
        async fn protected(
            State(state): State<Arc<TestState>>,
            headers: HeaderMap,
        ) -> Json<serde_json::Value> {
            let authorization = headers
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            state.seen.lock().unwrap().push(authorization);
            Json(serde_json::json!({}))
        }

        let state = Arc::new(TestState {
            api_version,
            seen,
            unauthorized,
            capabilities,
        });
        let app = Router::new()
            .route(HEALTH_PATH, get(health))
            .route(NODE_PATH, get(node))
            .route("/v1/{*path}", any(protected))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn subagent_policies_require_new_capability_on_legacy_child_daemons() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let endpoint = server_with_capabilities(
            API_VERSION,
            seen.clone(),
            false,
            vec![
                "persistent-conversations".into(),
                "child-tasks".into(),
                "unified-workspaces".into(),
            ],
        )
        .await;
        let client = DaemonClient::with_token(&endpoint, Some(TOKEN.into())).unwrap();
        let policy = slop_protocol::orchestration::OrchestrationPolicy {
            max_children: 4,
            max_concurrent_children: 2,
            max_depth: 1,
            allowed_models: Vec::new(),
            allowed_tools: Vec::new(),
        };
        assert!(matches!(
            client
                .create_session(&CreateSessionRequest {
                    command_id: "subagent-session".into(),
                    provider: "opencode-go".into(),
                    model: "glm-5.3-flash".into(),
                    orchestration: Some(policy.clone()),
                    ..Default::default()
                })
                .await,
            Err(ClientError::UnsupportedCapability("subagent-v2"))
        ));
        assert!(matches!(
            client
                .create_task(&slop_protocol::orchestration::CreateTaskRequest {
                    command_id: "subagent-task".into(),
                    spec: slop_protocol::orchestration::SessionSpec {
                        title: None,
                        prompt: "delegate".into(),
                        model: "opencode-go/glm-5.3-flash".into(),
                        settings: Default::default(),
                        workspace: Default::default(),
                        budget: None,
                        orchestration: Some(policy),
                    },
                })
                .await,
            Err(ClientError::UnsupportedCapability("subagent-v2"))
        ));
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn extended_mutations_fail_before_delivery_to_older_daemons() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let endpoint = server(API_VERSION, seen.clone(), false).await;
        let client = DaemonClient::with_token(&endpoint, Some(TOKEN.into())).unwrap();
        let error = client
            .send_message(
                "session",
                &SendMessageRequest {
                    command_id: "cmd".into(),
                    text: "hello".into(),
                    model: Some("opencode-go/glm-5.3".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ClientError::UnsupportedCapability("per-turn-settings")
        ));
        let error = client
            .create_session(&CreateSessionRequest {
                command_id: "managed".into(),
                provider: "opencode-go".into(),
                model: "glm-5.3-flash".into(),
                workspace: slop_protocol::workspaces::WorkspaceSelection {
                    workspace_id: "workspace".into(),
                    allowed_tools: vec!["edit".into()],
                    limits: None,
                },
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ClientError::UnsupportedCapability("unified-workspaces")
        ));
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn jobs_and_matrices_require_advertised_capabilities_before_delivery() {
        use slop_protocol::orchestration::*;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let endpoint = server(API_VERSION, seen.clone(), false).await;
        let client = DaemonClient::with_token(&endpoint, Some(TOKEN.into())).unwrap();
        let spec = SessionSpec {
            budget: None,
            orchestration: None,
            title: None,
            prompt: "work".into(),
            model: "opencode-go/glm-5.3-flash".into(),
            settings: Default::default(),
            workspace: Default::default(),
        };
        assert!(matches!(
            client
                .create_task(&CreateTaskRequest {
                    command_id: "job".into(),
                    spec
                })
                .await,
            Err(ClientError::UnsupportedCapability(
                "persistent-conversations"
            ))
        ));
        let batch = BatchSpec {
            budget: None,
            orchestration: None,
            name: "sweep".into(),
            prompts: vec!["work".into()],
            models: vec!["opencode-go/glm-5.3-flash".into()],
            settings: vec![Default::default()],
            workspace: Default::default(),
            max_concurrent_runs: 1,
            default_max_output_tokens: None,
        };
        assert!(matches!(
            client.preview_batch(&batch).await,
            Err(ClientError::UnsupportedCapability("batch-matrices"))
        ));
        assert!(matches!(
            client
                .create_batch(&CreateBatchRequest {
                    command_id: "batch".into(),
                    spec: batch
                })
                .await,
            Err(ClientError::UnsupportedCapability("batch-matrices"))
        ));
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn artifact_download_validates_size_checksum_and_authentication() {
        use slop_protocol::execution::ArtifactResponse;
        async fn fixture(
            bytes: Vec<u8>,
            size: u64,
            hash: String,
        ) -> (String, Arc<Mutex<Vec<String>>>) {
            let seen = Arc::new(Mutex::new(Vec::new()));
            let app = Router::new()
                .route(
                    HEALTH_PATH,
                    get(|| async {
                        Json(HealthResponse {
                            service: SERVICE_NAME.into(),
                            version: "test".into(),
                            api_version: API_VERSION,
                            capabilities: vec![],
                        })
                    }),
                )
                .route(
                    "/v1/artifacts/{id}",
                    get({
                        let seen = seen.clone();
                        let hash = hash.clone();
                        move |headers: HeaderMap| {
                            let seen = seen.clone();
                            let hash = hash.clone();
                            async move {
                                seen.lock()
                                    .unwrap()
                                    .push(headers[AUTHORIZATION].to_str().unwrap().to_owned());
                                Json(ArtifactResponse {
                                    id: hash.clone(),
                                    sha256: hash,
                                    size_bytes: size,
                                    media_type: "application/octet-stream".into(),
                                })
                            }
                        }
                    }),
                )
                .route(
                    "/v1/artifacts/{id}/content",
                    get({
                        let seen = seen.clone();
                        move |headers: HeaderMap| {
                            let seen = seen.clone();
                            let bytes = bytes.clone();
                            async move {
                                seen.lock()
                                    .unwrap()
                                    .push(headers[AUTHORIZATION].to_str().unwrap().to_owned());
                                bytes
                            }
                        }
                    }),
                );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            (format!("http://{address}"), seen)
        }
        let data = b"committed artifact";
        let hash = format!("{:x}", Sha256::digest(data));
        for (bytes, size, valid) in [
            (data.to_vec(), data.len() as u64, true),
            (b"corrupted artifact".to_vec(), data.len() as u64, false),
            (data.to_vec(), 1, false),
            (data.to_vec(), 100, false),
        ] {
            let (endpoint, seen) = fixture(bytes, size, hash.clone()).await;
            let token = token_file(TOKEN);
            let client = DaemonClient::new_with_token_file(&endpoint, token.path()).unwrap();
            let mut stream = client.artifact_content(&hash).await.unwrap();
            let mut downloaded = Vec::new();
            let success = loop {
                match stream.next_chunk().await {
                    Ok(Some(chunk)) => downloaded.extend(chunk),
                    Ok(None) => break true,
                    Err(ClientError::ArtifactIntegrity) => break false,
                    Err(error) => panic!("unexpected artifact error: {error}"),
                }
            };
            assert_eq!(success, valid);
            if valid {
                assert_eq!(downloaded, data);
            }
            assert_eq!(*seen.lock().unwrap(), vec![format!("Bearer {TOKEN}"); 2]);
        }
    }

    fn token_file(contents: &str) -> tempfile::NamedTempFile {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(contents.as_bytes()).unwrap();
        file
    }

    #[test]
    fn token_file_is_bounded_validated_and_redacted_from_debug() {
        let file = token_file(&format!("{TOKEN}\n"));
        let client =
            DaemonClient::new_with_token_file("http://localhost:7331", file.path()).unwrap();
        let debug = format!("{client:?}");
        assert!(!debug.contains(TOKEN));
        assert!(debug.contains("REDACTED"));

        let invalid = token_file("short\n");
        assert!(matches!(
            DaemonClient::new_with_token_file("http://localhost:7331", invalid.path()),
            Err(ClientError::InvalidTokenFile)
        ));
        let oversized = token_file(&format!("{TOKEN}extra"));
        assert!(matches!(
            DaemonClient::new_with_token_file("http://localhost:7331", oversized.path()),
            Err(ClientError::InvalidTokenFile)
        ));
        assert!(matches!(
            DaemonClient::new_with_token_file(
                "http://localhost:7331",
                invalid.path().parent().unwrap()
            ),
            Err(ClientError::InvalidTokenFile)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn token_file_rejects_group_or_world_readable_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let file = token_file(TOKEN);
        std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(matches!(
            DaemonClient::new_with_token_file("http://localhost:7331", file.path()),
            Err(ClientError::InvalidTokenFile)
        ));
    }

    #[tokio::test]
    async fn node_sends_bearer_and_parses_typed_response() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let endpoint = server(API_VERSION, Arc::clone(&seen), false).await;
        let file = token_file(TOKEN);
        let client = DaemonClient::new_with_token_file(&endpoint, file.path()).unwrap();
        let node = client.node().await.unwrap();
        assert_eq!(node.node_id, "node-1");
        assert_eq!(seen.lock().unwrap().as_slice(), [format!("Bearer {TOKEN}")]);
    }

    #[tokio::test]
    async fn selected_peer_checks_gateway_compatibility_before_sending_its_token() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let endpoint = server(API_VERSION + 1, seen.clone(), false).await;
        let client = DaemonClient::with_token(&endpoint, Some(TOKEN.into()))
            .unwrap()
            .via_peer("home")
            .unwrap();
        assert!(matches!(
            client.node().await,
            Err(ClientError::IncompatibleApi { .. })
        ));
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn node_checks_compatibility_before_sending_token() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let endpoint = server(API_VERSION + 1, Arc::clone(&seen), false).await;
        let file = token_file(TOKEN);
        let client = DaemonClient::new_with_token_file(&endpoint, file.path()).unwrap();
        assert!(matches!(
            client.node().await,
            Err(ClientError::IncompatibleApi { .. })
        ));
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn all_protected_chat_calls_check_compatibility_before_authentication() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let endpoint = server(API_VERSION + 1, Arc::clone(&seen), false).await;
        let file = token_file(TOKEN);
        let client = DaemonClient::new_with_token_file(&endpoint, file.path()).unwrap();
        let incompatible = |result: Result<(), ClientError>| {
            matches!(result, Err(ClientError::IncompatibleApi { .. }))
        };

        assert!(incompatible(client.models().await.map(|_| ())));
        assert!(incompatible(
            client.list_sessions(None, None).await.map(|_| ())
        ));
        assert!(incompatible(client.session("session-1").await.map(|_| ())));
        assert!(incompatible(
            client.history("session-1", None, None).await.map(|_| ())
        ));
        assert!(incompatible(client.turn("turn-1").await.map(|_| ())));
        assert!(incompatible(
            client.events("session-1", 0, false).await.map(|_| ())
        ));
        assert!(incompatible(
            client
                .create_session(&CreateSessionRequest {
                    command_id: "command-create".into(),
                    title: None,
                    provider: "opencode-go".into(),
                    model: "glm-5.3-flash".into(),
                    max_tokens: None,

                    ..Default::default()
                })
                .await
                .map(|_| ())
        ));
        assert!(incompatible(
            client
                .send_message(
                    "session-1",
                    &SendMessageRequest {
                        command_id: "command-send".into(),
                        text: "hello".into(),
                        expected_revision: None,

                        ..Default::default()
                    }
                )
                .await
                .map(|_| ())
        ));
        assert!(incompatible(
            client
                .cancel_turn(
                    "turn-1",
                    &CancelTurnRequest {
                        command_id: "command-cancel".into(),
                    }
                )
                .await
                .map(|_| ())
        ));
        assert!(
            seen.lock().unwrap().is_empty(),
            "protected routes received a credential before compatibility was checked"
        );
    }

    #[test]
    fn chat_identifiers_are_single_safe_path_segments() {
        let endpoint = Url::parse("http://127.0.0.1:7331").unwrap();
        let url = session_url(&endpoint, "abc-123_def", Some("messages")).unwrap();
        assert_eq!(url.path(), "/v1/sessions/abc-123_def/messages");
        assert!(matches!(
            session_url(&endpoint, "../other?token=x", Some("messages")),
            Err(ClientError::InvalidIdentifier)
        ));
    }

    async fn chunked_body(chunks: Vec<Vec<u8>>) -> Url {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut part = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let size = socket.read(&mut part).await.unwrap();
                if size == 0 {
                    break;
                }
                request.extend_from_slice(&part[..size]);
            }
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").await.unwrap();
            for chunk in chunks {
                socket
                    .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                    .await
                    .unwrap();
                socket.write_all(&chunk).await.unwrap();
                socket.write_all(b"\r\n").await.unwrap();
            }
            socket.write_all(b"0\r\n\r\n").await.unwrap();
            // Finish the response without resetting the connection while the
            // client's request writer is still draining on another runtime task.
            socket.shutdown().await.unwrap();
            while let Ok(size) = socket.read(&mut part).await {
                if size == 0 {
                    break;
                }
            }
        });
        Url::parse(&format!("http://{address}/events")).unwrap()
    }

    #[tokio::test]
    async fn event_stream_handles_split_utf8_crlf_and_final_partial_frame() {
        let event = EventFrame::Delta {
            session_id: "session-1".into(),
            turn_id: "turn-1".into(),
            text: "snowman ☃".into(),
            message_id: None,
            block_id: None,
            request_id: None,
            invocation_id: None,
            stream_id: String::new(),
            chunk_index: 0,
            kind: "text".into(),
        };
        let mut first = serde_json::to_vec(&event).unwrap();
        first.extend_from_slice(b"\r\n");
        let split = first
            .windows(3)
            .position(|window| window == "☃".as_bytes())
            .unwrap()
            + 1;
        let final_frame = serde_json::to_vec(&EventFrame::Heartbeat).unwrap();
        let url = chunked_body(vec![
            first[..split].to_vec(),
            first[split..].to_vec(),
            final_frame,
        ])
        .await;
        let response = reqwest::Client::new().get(url).send().await.unwrap();
        let mut stream = EventStream {
            response,
            buffer: Vec::new(),
            finished: false,
        };
        match stream.next_frame().await.unwrap().unwrap() {
            EventFrame::Delta { text, .. } => assert_eq!(text, "snowman ☃"),
            other => panic!("unexpected frame: {other:?}"),
        }
        assert!(matches!(
            stream.next_frame().await.unwrap(),
            Some(EventFrame::Heartbeat)
        ));
        assert!(stream.next_frame().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn event_stream_rejects_oversized_frames() {
        let oversized = vec![b'x'; MAX_EVENT_FRAME_BYTES + 1];
        let url = chunked_body(vec![oversized]).await;
        let response = reqwest::Client::new().get(url).send().await.unwrap();
        let mut stream = EventStream {
            response,
            buffer: Vec::new(),
            finished: false,
        };
        assert!(matches!(
            stream.next_frame().await,
            Err(ClientError::InvalidEventFrame)
        ));
    }

    #[tokio::test]
    async fn structured_unauthorized_error_is_reported_without_echoing_secret() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let endpoint = server(API_VERSION, Arc::clone(&seen), true).await;
        let file = token_file(TOKEN);
        let client = DaemonClient::new_with_token_file(&endpoint, file.path()).unwrap();
        let error = client.node().await.unwrap_err().to_string();
        assert!(error.contains("unauthorized"));
        assert!(!error.contains(TOKEN));
        assert!(!error.contains(&format!("Bearer {TOKEN}")));
    }
}
