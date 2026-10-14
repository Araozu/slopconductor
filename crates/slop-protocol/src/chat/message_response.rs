use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct MessageResponse {
    pub id: String,
    pub session_id: String,
    pub turn_id: String,
    pub role: String,
    pub text: String,
    pub status: String,
    /// Provider selected for this message's turn or originating model request.
    #[serde(default)]
    pub provider: String,
    /// Provider-reported model when available, otherwise the selected model ID.
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub blocks: Vec<crate::execution::ContentBlock>,
    #[serde(default)]
    pub request_id: Option<String>,
}
