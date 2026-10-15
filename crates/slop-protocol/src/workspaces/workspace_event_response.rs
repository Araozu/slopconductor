use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct WorkspaceEventResponse {
    pub sequence: u64,
    pub workspace_id: String,
    pub kind: String,
}
