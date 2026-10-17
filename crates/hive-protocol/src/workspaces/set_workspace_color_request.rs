use serde::{Deserialize, Serialize};

/// Set a workspace's opaque RGB display color.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SetWorkspaceColorRequest {
    pub command_id: String,
    /// Six hexadecimal RGB digits prefixed by #. Alpha and shorthand are not accepted.
    #[schema(pattern = "^#[0-9A-Fa-f]{6}$", example = "#3B82F6")]
    pub color: String,
}
