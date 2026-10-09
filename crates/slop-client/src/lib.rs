//! Daemon API client reusable by the CLI and a future native TUI.
//!
//! Clients communicate through the public protocol and do not embed execution.

use std::{fmt, path::Path, time::Duration};

use reqwest::header::{AUTHORIZATION, HeaderValue};
use sha2::{Digest, Sha256};
use slop_protocol::{
    API_VERSION, ErrorResponse, HEALTH_PATH, HealthResponse, NODE_PATH, NodeResponse, SERVICE_NAME,
    chat::{
        CancelTurnRequest, CommandReceipt, CreateSessionRequest, EventFrame, MessageResponse,
        ModelResponse, Page, SendMessageRequest, SessionResponse, TurnResponse,
    },
    providers::{LoginResponse, LoginStatus, ProviderStatus, SetApiKeyRequest, StartLoginRequest},
};
use thiserror::Error;
use url::Url;

const MAX_TOKEN_FILE_BYTES: u64 = 66;
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_JSON_BYTES: usize = 16 * 1024 * 1024;
const MAX_EVENT_FRAME_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("invalid daemon URL: {0}")]
    InvalidUrl(#[from] url::ParseError),
    #[error("identifier must contain only ASCII letters, digits, hyphens, or underscores")]
    InvalidIdentifier,
    #[error("daemon URL must use http or https")]
    UnsupportedScheme,
    #[error("daemon URL must be an origin without a path, query, fragment, or credentials")]
    InvalidOrigin,
    #[error("daemon request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("could not read token file: {0}")]
    TokenFile(#[from] std::io::Error),
    #[error("token file must contain exactly 64 hexadecimal characters and an optional newline")]
    InvalidTokenFile,
    #[error("unexpected daemon service: {0}")]
    UnexpectedService(String),
    #[error("incompatible daemon API: expected {expected}, received {actual}")]
    IncompatibleApi { expected: u32, actual: u32 },
    #[error("daemon rejected authentication ({code}): {message}")]
    Unauthorized { code: String, message: String },
    #[error("daemon API request failed with HTTP {status} ({code}): {message}")]
    Api {
        status: u16,
        code: String,
        message: String,
    },
    #[error("daemon returned an invalid or oversized response")]
    InvalidResponse,
    #[error("daemon request body exceeds the configured size limit")]
    RequestTooLarge,
    #[error("daemon event stream returned an invalid or oversized frame")]
    InvalidEventFrame,
    #[error("could not generate a unique command ID")]
    CommandId,
    #[error("daemon event stream was idle beyond its heartbeat deadline")]
    StreamTimeout,
    #[error("mutation delivery outcome is unknown: {0}")]
    DeliveryUncertain(String),
    #[error("artifact data does not match its committed size and checksum")]
    ArtifactIntegrity,
    #[error("daemon does not advertise required capability: {0}")]
    UnsupportedCapability(&'static str),
}

#[derive(Clone)]
pub struct DaemonClient {
    http: reqwest::Client,
    stream_http: reqwest::Client,
    endpoint: Url,
    token: Option<HeaderValue>,
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
        })
    }

    pub async fn health(&self) -> Result<HealthResponse, ClientError> {
        let endpoint = self.endpoint.join(HEALTH_PATH)?;
        let response = self.http.get(endpoint).send().await?;
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
        let endpoint = self.endpoint.join(NODE_PATH)?;
        let mut request = self.http.get(endpoint);
        if let Some(token) = &self.token {
            request = request.header(AUTHORIZATION, token.clone());
        }
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

    pub async fn create_session(
        &self,
        request: &CreateSessionRequest,
    ) -> Result<CommandReceipt, ClientError> {
        if request.execution.is_some() {
            self.require_feature("tools").await?;
        }
        if request.settings.is_some() {
            self.require_feature("per-turn-settings").await?;
        }
        self.post_json(self.endpoint.join("/v1/sessions")?, request)
            .await
    }

    pub async fn list_sessions(
        &self,
        after: Option<u64>,
        limit: Option<u32>,
    ) -> Result<Page<SessionResponse>, ClientError> {
        self.get_json(query_url(
            &self.endpoint,
            "/v1/sessions",
            after,
            limit,
            None,
        )?)
        .await
    }

    pub async fn session(&self, session_id: &str) -> Result<SessionResponse, ClientError> {
        self.get_json(session_url(&self.endpoint, session_id, None)?)
            .await
    }

    pub async fn history(
        &self,
        session_id: &str,
        after: Option<u64>,
        limit: Option<u32>,
    ) -> Result<Page<MessageResponse>, ClientError> {
        let mut url = session_url(&self.endpoint, session_id, Some("messages"))?;
        append_page_query(&mut url, after, limit);
        self.get_json(url).await
    }

    pub async fn send_message(
        &self,
        session_id: &str,
        request: &SendMessageRequest,
    ) -> Result<CommandReceipt, ClientError> {
        if request.model.is_some() || request.settings.is_some() {
            self.require_feature("per-turn-settings").await?;
        }
        self.post_json(
            session_url(&self.endpoint, session_id, Some("messages"))?,
            request,
        )
        .await
    }

    pub async fn events(
        &self,
        session_id: &str,
        after: u64,
        follow: bool,
    ) -> Result<EventStream, ClientError> {
        self.health().await?;
        let mut url = session_url(&self.endpoint, session_id, Some("events"))?;
        url.query_pairs_mut()
            .append_pair("after", &after.to_string())
            .append_pair("follow", if follow { "true" } else { "false" });
        let mut request = self.stream_http.get(url);
        if let Some(token) = &self.token {
            request = request.header(AUTHORIZATION, token.clone());
        }
        let response = request.send().await?;
        let status = response.status();
        if !status.is_success() {
            let body = read_bounded(response).await?;
            return Err(api_error(status.as_u16(), &body, self.token.as_ref()));
        }
        Ok(EventStream {
            response,
            buffer: Vec::new(),
            finished: false,
        })
    }

    pub async fn turn(&self, turn_id: &str) -> Result<TurnResponse, ClientError> {
        self.get_json(id_url(&self.endpoint, "/v1/turns", turn_id, None)?)
            .await
    }

    pub async fn cancel_turn(
        &self,
        turn_id: &str,
        request: &CancelTurnRequest,
    ) -> Result<CommandReceipt, ClientError> {
        self.post_json(
            id_url(&self.endpoint, "/v1/turns", turn_id, Some("cancel"))?,
            request,
        )
        .await
    }

    pub async fn models(&self) -> Result<Vec<ModelResponse>, ClientError> {
        self.get_json(self.endpoint.join("/v1/models")?).await
    }

    pub async fn capabilities(
        &self,
    ) -> Result<slop_protocol::execution::CapabilitiesResponse, ClientError> {
        self.get_json(self.endpoint.join("/v1/capabilities")?).await
    }
    pub async fn message(&self, id: &str) -> Result<MessageResponse, ClientError> {
        self.get_json(id_url(&self.endpoint, "/v1/messages", id, None)?)
            .await
    }
    pub async fn model_requests(
        &self,
        turn: &str,
        after: Option<u64>,
        limit: Option<u32>,
    ) -> Result<Page<slop_protocol::execution::ModelRequestResponse>, ClientError> {
        let mut url = id_url(&self.endpoint, "/v1/turns", turn, Some("requests"))?;
        append_page_query(&mut url, after, limit);
        self.get_json(url).await
    }
    pub async fn tool_invocations(
        &self,
        turn: &str,
        after: Option<u64>,
        limit: Option<u32>,
    ) -> Result<Page<slop_protocol::execution::ToolInvocationResponse>, ClientError> {
        let mut url = id_url(&self.endpoint, "/v1/turns", turn, Some("tools"))?;
        append_page_query(&mut url, after, limit);
        self.get_json(url).await
    }
    pub async fn tool_invocation(
        &self,
        id: &str,
    ) -> Result<slop_protocol::execution::ToolInvocationResponse, ClientError> {
        self.get_json(id_url(&self.endpoint, "/v1/tools", id, None)?)
            .await
    }
    pub async fn artifact(
        &self,
        id: &str,
    ) -> Result<slop_protocol::execution::ArtifactResponse, ClientError> {
        self.get_json(id_url(&self.endpoint, "/v1/artifacts", id, None)?)
            .await
    }

    pub async fn artifact_content(&self, id: &str) -> Result<ArtifactStream, ClientError> {
        let metadata = self.artifact(id).await?;
        if metadata.id != id || metadata.sha256 != id || metadata.size_bytes > 64 * 1024 * 1024 {
            return Err(ClientError::InvalidResponse);
        }
        let pending = self
            .authorized(self.stream_http.get(id_url(
                &self.endpoint,
                "/v1/artifacts",
                id,
                Some("content"),
            )?))
            .send();
        let response = tokio::time::timeout(Duration::from_secs(10), pending)
            .await
            .map_err(|_| ClientError::StreamTimeout)??;
        let status = response.status();
        if !status.is_success() {
            let body = read_bounded(response).await?;
            return Err(api_error(status.as_u16(), &body, self.token.as_ref()));
        }
        Ok(ArtifactStream {
            metadata,
            response,
            received: 0,
            hash: Sha256::new(),
            finished: false,
        })
    }

    pub async fn providers(&self) -> Result<Vec<ProviderStatus>, ClientError> {
        self.get_json(self.endpoint.join(slop_protocol::PROVIDERS_PATH)?)
            .await
    }

    /// Idempotent replacement; transport failure may leave the key installed.
    pub async fn set_provider_api_key(
        &self,
        provider: &str,
        request: &SetApiKeyRequest,
    ) -> Result<ProviderStatus, ClientError> {
        self.mutate_json(
            reqwest::Method::PUT,
            id_url(
                &self.endpoint,
                slop_protocol::PROVIDERS_PATH,
                provider,
                Some("api-key"),
            )?,
            request,
        )
        .await
    }

    pub async fn start_codex_login(
        &self,
        request: &StartLoginRequest,
    ) -> Result<LoginResponse, ClientError> {
        self.post_json(self.endpoint.join("/v1/providers/codex/login")?, request)
            .await
    }

    pub async fn codex_login_status(&self, login_id: &str) -> Result<LoginStatus, ClientError> {
        self.get_json(id_url(
            &self.endpoint,
            "/v1/providers/codex/login",
            login_id,
            None,
        )?)
        .await
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: Url) -> Result<T, ClientError> {
        self.health().await?;
        let response = self.authorized(self.http.get(url)).send().await?;
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
                    .request(method, url)
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(body),
            )
            .send()
            .await
            .map_err(|error| ClientError::DeliveryUncertain(error.to_string()))?;
        let status = response.status();
        let body =
            read_bounded_to(response, MAX_JSON_BYTES)
                .await
                .map_err(|error| match error {
                    ClientError::Request(request) => {
                        ClientError::DeliveryUncertain(request.to_string())
                    }
                    other => other,
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

/// A bounded NDJSON decoder for the daemon's reconnectable event endpoint.
pub struct EventStream {
    response: reqwest::Response,
    buffer: Vec<u8>,
    finished: bool,
}

/// Bounded artifact download. Consume through EOF to validate the committed
/// checksum; callers should publish a destination only after validation.
pub struct ArtifactStream {
    pub metadata: slop_protocol::execution::ArtifactResponse,
    response: reqwest::Response,
    received: u64,
    hash: Sha256,
    finished: bool,
}
impl ArtifactStream {
    pub async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, ClientError> {
        if self.finished {
            return Ok(None);
        }
        let next = tokio::time::timeout(Duration::from_secs(45), self.response.chunk())
            .await
            .map_err(|_| ClientError::StreamTimeout)??;
        if let Some(chunk) = next {
            self.received = self.received.saturating_add(chunk.len() as u64);
            if self.received > self.metadata.size_bytes {
                return Err(ClientError::ArtifactIntegrity);
            }
            self.hash.update(&chunk);
            Ok(Some(chunk.to_vec()))
        } else {
            if self.received != self.metadata.size_bytes
                || format!("{:x}", self.hash.clone().finalize()) != self.metadata.sha256
            {
                return Err(ClientError::ArtifactIntegrity);
            }
            self.finished = true;
            Ok(None)
        }
    }
}

impl EventStream {
    pub async fn next_frame(&mut self) -> Result<Option<EventFrame>, ClientError> {
        loop {
            if let Some(newline) = self.buffer.iter().position(|byte| *byte == b'\n') {
                if newline > MAX_EVENT_FRAME_BYTES {
                    return Err(ClientError::InvalidEventFrame);
                }
                let mut line: Vec<u8> = self.buffer.drain(..=newline).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if line.is_empty() {
                    continue;
                }
                return serde_json::from_slice(&line)
                    .map(Some)
                    .map_err(|_| ClientError::InvalidEventFrame);
            }
            if self.buffer.len() > MAX_EVENT_FRAME_BYTES {
                return Err(ClientError::InvalidEventFrame);
            }
            if self.finished {
                return Ok(None);
            }
            match tokio::time::timeout(Duration::from_secs(45), self.response.chunk())
                .await
                .map_err(|_| ClientError::StreamTimeout)??
            {
                Some(chunk) => {
                    self.buffer.extend_from_slice(&chunk);
                }
                None => {
                    self.finished = true;
                    if self.buffer.is_empty() {
                        return Ok(None);
                    }
                    let line = std::mem::take(&mut self.buffer);
                    if line.len() > MAX_EVENT_FRAME_BYTES {
                        return Err(ClientError::InvalidEventFrame);
                    }
                    return serde_json::from_slice(&line)
                        .map(Some)
                        .map_err(|_| ClientError::InvalidEventFrame);
                }
            }
        }
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

fn session_url(endpoint: &Url, id: &str, suffix: Option<&str>) -> Result<Url, ClientError> {
    id_url(endpoint, "/v1/sessions", id, suffix)
}

fn id_url(endpoint: &Url, base: &str, id: &str, suffix: Option<&str>) -> Result<Url, ClientError> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(ClientError::InvalidIdentifier);
    }
    let mut url = endpoint.join(base)?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| ClientError::InvalidOrigin)?;
        segments.pop_if_empty().push(id);
        if let Some(suffix) = suffix {
            segments.push(suffix);
        }
    }
    Ok(url)
}

fn query_url(
    endpoint: &Url,
    path: &str,
    after: Option<u64>,
    limit: Option<u32>,
    extra: Option<(&str, &str)>,
) -> Result<Url, ClientError> {
    let mut url = endpoint.join(path)?;
    {
        let mut query = url.query_pairs_mut();
        if let Some(after) = after {
            query.append_pair("after", &after.to_string());
        }
        if let Some(limit) = limit {
            query.append_pair("limit", &limit.to_string());
        }
        if let Some((key, value)) = extra {
            query.append_pair(key, value);
        }
    }
    Ok(url)
}

fn append_page_query(url: &mut Url, after: Option<u64>, limit: Option<u32>) {
    let mut query = url.query_pairs_mut();
    if let Some(after) = after {
        query.append_pair("after", &after.to_string());
    }
    if let Some(limit) = limit {
        query.append_pair("limit", &limit.to_string());
    }
}

fn read_token_file(path: &Path) -> Result<String, ClientError> {
    use std::io::Read;

    let link_metadata = std::fs::symlink_metadata(path)?;
    if !link_metadata.file_type().is_file() {
        return Err(ClientError::InvalidTokenFile);
    }
    let file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(ClientError::InvalidTokenFile);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o077 != 0 {
            return Err(ClientError::InvalidTokenFile);
        }
    }
    let mut bytes = Vec::with_capacity(66);
    file.take(MAX_TOKEN_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_TOKEN_FILE_BYTES {
        return Err(ClientError::InvalidTokenFile);
    }
    if bytes.ends_with(b"\r\n") {
        bytes.truncate(bytes.len() - 2);
    } else if bytes.ends_with(b"\n") {
        bytes.truncate(bytes.len() - 1);
    }
    if bytes.len() != 64 || !bytes.iter().all(u8::is_ascii_hexdigit) {
        return Err(ClientError::InvalidTokenFile);
    }
    String::from_utf8(bytes).map_err(|_| ClientError::InvalidTokenFile)
}

async fn read_bounded(response: reqwest::Response) -> Result<Vec<u8>, ClientError> {
    read_bounded_to(response, MAX_RESPONSE_BYTES).await
}

async fn read_bounded_to(
    mut response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, ClientError> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return Err(ClientError::InvalidResponse);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn api_error(status: u16, body: &[u8], token: Option<&HeaderValue>) -> ClientError {
    let parsed = serde_json::from_slice::<ErrorResponse>(body).ok();
    let (code, message) = parsed
        .map(|error| (error.code, error.message))
        .unwrap_or_else(|| ("http_error".to_owned(), "daemon request failed".to_owned()));
    let (code, message) = token
        .and_then(|value| value.to_str().ok())
        .map(|authorization| {
            let secret = authorization.trim_start_matches("Bearer ");
            (
                code.replace(authorization, "[REDACTED]")
                    .replace(secret, "[REDACTED]"),
                message
                    .replace(authorization, "[REDACTED]")
                    .replace(secret, "[REDACTED]"),
            )
        })
        .unwrap_or((code, message));
    if status == 401 {
        ClientError::Unauthorized { code, message }
    } else {
        ClientError::Api {
            status,
            code,
            message,
        }
    }
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
    }

    async fn server(api_version: u32, seen: Arc<Mutex<Vec<String>>>, unauthorized: bool) -> String {
        async fn health(State(state): State<Arc<TestState>>) -> Json<HealthResponse> {
            Json(HealthResponse {
                service: SERVICE_NAME.into(),
                version: "test".into(),
                api_version: state.api_version,
                capabilities: vec!["health".into(), "node".into()],
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
