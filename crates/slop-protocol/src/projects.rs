//! Daemon-owned repositories and managed session workspaces.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RegisterProjectRequest {
    pub command_id: String,
    /// Absolute checkout path on the owner's machine.
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectResponse {
    pub id: String,
    pub owner_node_id: String,
    pub path: String,
    /// Canonical Git common directory; separate clones are separate projects.
    pub git_common_dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectWorkspaceRequest {
    pub project_id: String,
    /// Resolved to an exact commit at acceptance. Omission selects HEAD.
    pub base_ref: Option<String>,
    pub allowed_tools: Vec<String>,
}

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceDiffResponse {
    pub workspace_id: String,
    pub base_commit: String,
    pub head_commit: String,
    /// Tracked changes relative to the frozen base, including staged changes.
    pub patch: String,
    /// Git porcelain status also lists untracked and ignored paths.
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoveWorkspaceRequest {
    pub command_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectEventResponse {
    pub sequence: u64,
    pub project_id: String,
    pub workspace_id: Option<String>,
    pub kind: String,
}
