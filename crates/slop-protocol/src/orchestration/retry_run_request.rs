use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetryRunRequest {
    pub command_id: String,
    #[serde(default)]
    pub acknowledge_unknown_effects: bool,
}
