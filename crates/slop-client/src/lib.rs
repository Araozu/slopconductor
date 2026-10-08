//! Daemon API client reusable by the CLI and a future native TUI.
//!
//! Clients communicate through the public protocol and do not embed execution.

use std::{fmt, path::Path, time::Duration};

use reqwest::header::{AUTHORIZATION, HeaderValue};
use slop_protocol::{
    API_VERSION, ErrorResponse, HEALTH_PATH, HealthResponse, NODE_PATH, NodeResponse, SERVICE_NAME,
};
use thiserror::Error;
use url::Url;

const MAX_TOKEN_FILE_BYTES: u64 = 66;
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("invalid daemon URL: {0}")]
    InvalidUrl(#[from] url::ParseError),
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
}

#[derive(Clone)]
pub struct DaemonClient {
    http: reqwest::Client,
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

        Ok(Self {
            http,
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

async fn read_bounded(mut response: reqwest::Response) -> Result<Vec<u8>, ClientError> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
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
        routing::get,
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

        let state = Arc::new(TestState {
            api_version,
            seen,
            unauthorized,
        });
        let app = Router::new()
            .route(HEALTH_PATH, get(health))
            .route(NODE_PATH, get(node))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{address}")
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
