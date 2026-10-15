use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameWorkspaceRequest {
    pub command_id: String,
    pub name: String,
}
