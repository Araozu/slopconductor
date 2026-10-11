use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectAuthRequest {
    pub auth_mode: String,
    #[serde(default)]
    pub account_id: Option<String>,
}
