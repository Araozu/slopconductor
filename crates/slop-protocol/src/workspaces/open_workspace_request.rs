use serde::{Deserialize, Serialize};
/// Open an existing directory in place. Never creates a worktree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenWorkspaceRequest {
    pub command_id: String,
    /// Existing absolute directory on the authoritative daemon.
    pub path: String,
}
