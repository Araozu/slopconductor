use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct SessionResponse {
    pub id: String,
    pub owner_node_id: String,
    pub title: Option<String>,
    /// Derived execution/review status. Ready means successfully finished but not
    /// yet acknowledged via the shared inbox read_sequence. GET does not mark read.
    #[serde(default)]
    #[schema(read_only)]
    pub status: super::SessionStatus,
    pub provider: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub revision: u64,
    pub last_event_sequence: u64,
    /// Last accepted prompt, steering instruction, or queued-input edit, in Unix
    /// milliseconds (daemon clock). Null until input is accepted, including for
    /// migrated sessions whose historical input times are unknown. Retries and
    /// deferred delivery do not refresh this timestamp.
    #[serde(default)]
    pub last_user_activity_at: Option<u64>,
    #[serde(default)]
    pub settings: crate::execution::GenerationSettings,
    #[serde(default)]
    pub execution: Option<crate::execution::WorkspacePolicy>,
    pub workspace_id: String,
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
