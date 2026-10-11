use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetryBatchRequest {
    pub command_id: String,
    pub indices: Vec<u32>,
    #[serde(default)]
    pub acknowledge_unknown_effects: bool,
}
