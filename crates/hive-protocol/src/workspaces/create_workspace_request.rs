use serde::{Deserialize, Serialize};
/// Create and open a new directory, optionally initializing a Git repository.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateWorkspaceRequest {
    pub command_id: String,
    pub parent: String,
    pub name: String,
    #[serde(default)]
    pub git: bool,
}
