use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StartLoginRequest {
    pub command_id: String,
    /// A stable local registration label. Omission selects the current
    /// registration, or "default" on first login. A new label adds an account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// Accept the browser callback through the authenticated API instead of a
    /// loopback listener. The browser may run on another machine.
    #[serde(default, skip_serializing_if = "is_false")]
    #[schema(default = false)]
    pub headless: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}
