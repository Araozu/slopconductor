use serde::{Deserialize, Serialize};

/// Copy history through a message into an independent conversation.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct ForkSessionRequest {
    pub command_id: String,
    pub message_id: String,
    pub expected_revision: Option<u64>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct SessionFork {
    pub session_id: String,
    pub message_id: String,
    pub edited: bool,
}
