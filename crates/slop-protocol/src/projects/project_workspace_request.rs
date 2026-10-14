use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectWorkspaceRequest {
    pub project_id: String,
    /// Resolved to an exact commit at acceptance. Omission selects HEAD.
    pub base_ref: Option<String>,
    pub allowed_tools: Vec<String>,
}
