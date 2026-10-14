use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct TaskReceipt {
    pub command_id: String,
    pub task_id: String,
    pub run_id: String,
    pub session_id: String,
    pub turn_id: String,
    pub event_sequence: u64,
}
