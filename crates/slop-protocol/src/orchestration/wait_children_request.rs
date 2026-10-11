use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WaitChildrenRequest {
    pub command_id: String,
    pub child_run_ids: Vec<String>,
}
