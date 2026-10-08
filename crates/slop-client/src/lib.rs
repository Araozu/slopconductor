//! Daemon API client reusable by the CLI and a future native TUI.
//!
//! Clients communicate through the public protocol and do not embed execution.

use std::time::Duration;

use slop_protocol::{API_VERSION, HEALTH_PATH, HealthResponse, SERVICE_NAME};
use thiserror::Error;
use url::Url;

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
    #[error("unexpected daemon service: {0}")]
    UnexpectedService(String),
    #[error("incompatible daemon API: expected {expected}, received {actual}")]
    IncompatibleApi { expected: u32, actual: u32 },
}

#[derive(Debug, Clone)]
pub struct DaemonClient {
    http: reqwest::Client,
    endpoint: Url,
}

impl DaemonClient {
    pub fn new(endpoint: &str) -> Result<Self, ClientError> {
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

        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()?;

        Ok(Self { http, endpoint })
    }

    pub async fn health(&self) -> Result<HealthResponse, ClientError> {
        let endpoint = self.endpoint.join(HEALTH_PATH)?;
        let response: HealthResponse = self
            .http
            .get(endpoint)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

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
}
