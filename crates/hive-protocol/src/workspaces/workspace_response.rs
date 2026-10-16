use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct WorkspaceResponse {
    pub id: String,
    pub name: String,
    pub owner_node_id: String,
    /// Canonical absolute directory on the owner.
    pub path: Option<String>,
    /// Optional Git identity. Ordinary folders need no Git installation.
    pub git_common_dir: Option<String>,
    /// Whether Hive owns creation and filesystem cleanup of this worktree.
    pub managed_worktree: bool,
    /// Built-in container for threads without a directory.
    pub is_default: bool,
    pub source_workspace_id: Option<String>,
    /// Exact starting commit for an explicitly created worktree.
    pub base_commit: Option<String>,
    /// allocating, ready, removing, removed, or failed.
    pub status: String,
    pub error_code: Option<String>,
    pub effects_unknown: bool,
}
