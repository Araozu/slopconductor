use serde::{Deserialize, Serialize};
/// Select an existing workspace for execution. Selection never creates files or worktrees.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSelection {
    pub workspace_id: String,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<super::WorkspaceToolLimits>,
}

impl Default for WorkspaceSelection {
    fn default() -> Self {
        Self {
            workspace_id: super::DEFAULT_WORKSPACE_ID.into(),
            allowed_tools: Vec::new(),
            limits: None,
        }
    }
}
