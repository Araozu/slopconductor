use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct SessionResponse {
    pub id: String,
    pub owner_node_id: String,
    pub title: Option<String>,
    pub provider: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub revision: u64,
    pub last_event_sequence: u64,
    #[serde(default)]
    pub settings: crate::execution::GenerationSettings,
    #[serde(default)]
    pub execution: Option<crate::execution::WorkspacePolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_workspace_id: Option<String>,
    pub spec: crate::orchestration::SessionSpec,
    pub requested_spec: crate::orchestration::SessionSpec,
    pub batch_id: Option<String>,
    pub combination_index: Option<u32>,
    pub latest_run: Option<crate::orchestration::RunResponse>,
    pub budget_usage: crate::orchestration::BudgetUsage,
    pub child: Option<crate::orchestration::ChildLink>,
    #[serde(default)]
    pub inbox: Option<super::SessionInbox>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork: Option<super::SessionFork>,
}
