use serde::{Deserialize, Serialize};

/// Safe structured error returned by public API endpoints.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorResponse {
    pub code: String,
    pub message: String,
}
