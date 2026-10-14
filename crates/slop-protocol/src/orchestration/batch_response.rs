use super::{BatchSpec, BudgetUsage};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct BatchResponse {
    pub id: String,
    pub owner_node_id: String,
    pub spec: BatchSpec,
    pub total: u32,
    pub statuses: std::collections::BTreeMap<String, u32>,
    #[serde(default)]
    pub cancellation_requested: bool,
    #[serde(default)]
    pub budget_usage: BudgetUsage,
}
