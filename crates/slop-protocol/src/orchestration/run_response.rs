use crate::chat::TurnResponse;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct RunResponse {
    pub id: String,
    pub task_id: String,
    pub attempt: u32,
    pub retry_of: Option<String>,
    pub session_id: String,
    pub workspace_id: Option<String>,
    pub effects_unknown: bool,
    pub turn: TurnResponse,
}
