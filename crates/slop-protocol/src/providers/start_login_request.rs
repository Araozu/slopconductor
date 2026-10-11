use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartLoginRequest {
    pub command_id: String,
    /// A stable local registration label. Omission selects the current
    /// registration, or "default" on first login. A new label adds an account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}
