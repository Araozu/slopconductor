use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
}
