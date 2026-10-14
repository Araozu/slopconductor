use super::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct TurnResponse {
    pub id: String,
    pub session_id: String,
    pub user_message_id: String,
    pub assistant_message_id: Option<String>,
    pub status: String,
    pub requested_model: String,
    pub resolved_model: Option<String>,
    pub usage: Option<UsageResponse>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    #[serde(default)]
    pub settings: crate::execution::GenerationSettings,
    #[serde(default)]
    pub model_request_ids: Vec<String>,
    #[serde(default)]
    pub tool_invocation_ids: Vec<String>,
}
