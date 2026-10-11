use super::{OperationBudget, OrchestrationPolicy};
use crate::{execution::GenerationSettings, projects::ProjectWorkspaceRequest};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BatchSpec {
    pub name: String,
    pub prompts: Vec<String>,
    pub models: Vec<String>,
    pub settings: Vec<GenerationSettings>,
    pub project: Option<ProjectWorkspaceRequest>,
    pub max_concurrent_runs: u32,
    /// Preview freezes the daemon default for uncapped API-key cells.
    #[serde(default)]
    pub default_max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<OperationBudget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestration: Option<OrchestrationPolicy>,
}
