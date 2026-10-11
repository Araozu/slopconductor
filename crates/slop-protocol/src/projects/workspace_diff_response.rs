use serde::{Deserialize, Serialize};

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
