use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct ProjectResponse {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub owner_node_id: String,
    pub path: String,
    /// Canonical Git common directory, or an empty string for a plain folder project.
    pub git_common_dir: String,
}
