use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct RenameSessionRequest {
    pub command_id: String,
    pub title: String,
}
