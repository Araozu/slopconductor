use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct TaskEventResponse {
    pub task_id: String,
    pub sequence: u64,
    pub run_id: String,
    pub kind: String,
    pub status: String,
    pub session_event_sequence: Option<u64>,
}
