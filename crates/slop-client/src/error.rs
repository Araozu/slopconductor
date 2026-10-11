use thiserror::Error;

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
