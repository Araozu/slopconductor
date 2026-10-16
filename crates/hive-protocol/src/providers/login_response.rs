use serde::{Deserialize, Serialize};

/// The URL is an ephemeral authorization link, never an access token.
#[derive(Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct LoginResponse {
    pub login_id: String,
    pub authorization_url: String,
    #[serde(default)]
    pub headless: bool,
}
