use super::TaskSpec;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CreateTaskRequest {
    pub command_id: String,
    pub spec: TaskSpec,
}
