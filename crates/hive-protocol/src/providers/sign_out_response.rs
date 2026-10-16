use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SignOutResponse {
    pub account_id: String,
    pub remote_revocation_confirmed: bool,
}
