use super::{ChildContext, SessionSpec};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CreateChildRequest {
    pub command_id: String,
    pub spec: SessionSpec,
    #[serde(default)]
    pub context: ChildContext,
    #[serde(default = "cancel_child_default")]
    pub cancel_with_parent: bool,
}

fn cancel_child_default() -> bool {
    true
}
