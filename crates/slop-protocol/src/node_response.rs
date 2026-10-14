use serde::{Deserialize, Serialize};

/// Stable node identity returned by the authenticated node query.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct NodeResponse {
    pub node_id: String,
    pub name: String,
    pub os: String,
}
