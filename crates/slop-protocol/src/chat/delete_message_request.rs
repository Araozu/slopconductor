use serde::{Deserialize, Serialize};

/// Remove this message and its suffix from the conversation's visible history.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct DeleteMessageRequest {
    pub command_id: String,
    pub expected_revision: Option<u64>,
}
