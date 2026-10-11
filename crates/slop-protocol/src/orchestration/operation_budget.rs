use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OperationBudget {
    pub max_model_requests: u32,
    pub max_tool_calls: u32,
}
