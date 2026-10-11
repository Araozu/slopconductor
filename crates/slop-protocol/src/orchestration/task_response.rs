use super::{BudgetUsage, ChildLink, RunResponse, TaskSpec};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskResponse {
    pub id: String,
    pub owner_node_id: String,
    pub spec: TaskSpec,
    /// Caller inputs before default settings and base-reference resolution.
    pub requested_spec: TaskSpec,
    pub batch_id: Option<String>,
    pub combination_index: Option<u32>,
    pub last_event_sequence: u64,
    pub latest_run: RunResponse,
    #[serde(default)]
    pub budget_usage: BudgetUsage,
    #[serde(default)]
    pub child: Option<ChildLink>,
}
