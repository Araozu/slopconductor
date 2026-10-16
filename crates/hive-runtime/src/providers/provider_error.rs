use std::fmt;

use hive_core::provider::ProviderId;
use thiserror::Error;

/// Errors for provider configuration, validation, and inference.
///
/// Upstream response bodies are never retained in diagnostics. Transport
/// failures have a fixed display message; their source is runtime-private.
#[derive(Error)]
pub enum ProviderError {
    #[error("missing API key: set {env_var}")]
    MissingApiKey { env_var: &'static str },
    #[error("empty API key in {env_var}")]
    EmptyApiKey { env_var: &'static str },
    #[error("unsupported model for {provider}: {model}")]
    InvalidModel { provider: ProviderId, model: String },
    #[error("invalid chat request: {0}")]
    InvalidRequest(&'static str),
    #[error("provider {provider} returned HTTP {status}")]
    UnexpectedStatus { provider: ProviderId, status: u16 },
    #[error("provider context window exceeded")]
    ContextLengthExceeded,
    #[error("provider request rate limit reached")]
    RateLimited {
        retry_after: Option<std::time::Duration>,
    },
    #[error("provider account quota exhausted")]
    QuotaExceeded,
    /// Only documented, allowlisted codes are retained, never upstream text.
    #[error("ChatGPT plan usage failed: {code}")]
    PlanUsage { code: &'static str },
    #[error("provider {provider} returned an unrecognized response: {detail}")]
    InvalidResponse {
        provider: ProviderId,
        detail: &'static str,
    },
    #[error("unsupported provider content or capability: {capability}")]
    UnsupportedCapability { capability: &'static str },
    #[error("provider {provider} reported a failed turn")]
    TurnFailed { provider: ProviderId },
    #[error("request or response exceeded a bound: {detail}")]
    LimitExceeded { detail: &'static str },
    #[error("ChatGPT authentication failed: {detail}")]
    Authentication { detail: &'static str },
    #[error("ChatGPT credential storage failed")]
    CredentialStorage,
    #[error("provider transport request failed")]
    Http(#[from] reqwest::Error),
}

impl fmt::Debug for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
