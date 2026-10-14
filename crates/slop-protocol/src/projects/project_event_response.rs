use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct ProjectEventResponse {
    pub sequence: u64,
    pub project_id: String,
    pub workspace_id: Option<String>,
    pub kind: String,
}
