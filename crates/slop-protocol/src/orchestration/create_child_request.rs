use super::{ChildContext, SessionSpec};
use serde::{Deserialize, Serialize};

fn default_notification() -> String {
    "completion".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CreateChildRequest {
    pub command_id: String,
    pub spec: SessionSpec,
    #[serde(default)]
    pub context: ChildContext,
    #[serde(default = "cancel_child_default")]
    pub cancel_with_parent: bool,
    #[serde(default = "default_notification")]
    pub notification_mode: String,
    #[serde(default)]
    pub detached: bool,
}

fn cancel_child_default() -> bool {
    true
}
