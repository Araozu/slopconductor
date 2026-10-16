use serde::{Deserialize, Serialize};

/// Contains a single-use authorization code. Never log or persist this request.
#[derive(Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LoginCallbackRequest {
    pub callback_url: String,
}
