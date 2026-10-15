use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateSessionRequest {
    pub command_id: String,
    pub title: Option<String>,
    pub provider: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<crate::execution::GenerationSettings>,
    pub workspace: crate::workspaces::WorkspaceSelection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Consume a saved compose draft atomically with session acceptance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<crate::orchestration::OperationBudget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestration: Option<crate::orchestration::OrchestrationPolicy>,
}
