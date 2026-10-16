use serde::{Deserialize, Serialize};
/// Explicitly create another workspace as a detached Git worktree of an existing workspace.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateWorktreeRequest {
    pub command_id: String,
    /// Freeze this revision at acceptance; omission selects the source workspace's HEAD.
    pub base_ref: Option<String>,
    pub name: Option<String>,
}
