use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceResponse {
    pub id: String,
    pub project_id: String,
    pub session_id: String,
    pub owner_node_id: String,
    pub path: String,
    pub base_commit: String,
    pub status: String,
    pub error_code: Option<String>,
    pub effects_unknown: bool,
}
