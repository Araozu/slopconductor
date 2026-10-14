use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RegisterProjectRequest {
    pub command_id: String,
    /// Absolute checkout path on the owner's machine.
    pub path: String,
}
