use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct BudgetUsage {
    pub model_requests: u64,
    pub tool_calls: u64,
}
