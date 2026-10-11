use serde::{Deserialize, Serialize};

/// The URL is an ephemeral authorization link, never an access token.
#[derive(Clone, Serialize, Deserialize)]
pub struct LoginResponse {
    pub login_id: String,
    pub authorization_url: String,
}
