use super::{OperationBudget, OrchestrationPolicy};
use crate::{execution::GenerationSettings, workspaces::WorkspaceSelection};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionSpec {
    pub title: Option<String>,
    pub prompt: String,
    /// Provider-qualified model identifier.
    pub model: String,
    #[serde(default)]
    pub settings: GenerationSettings,
    pub workspace: WorkspaceSelection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<OperationBudget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestration: Option<OrchestrationPolicy>,
}
