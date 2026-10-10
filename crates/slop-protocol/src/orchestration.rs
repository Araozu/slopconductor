//! Durable jobs, explicit attempts, and prompt/model/settings matrices.
use crate::{chat::TurnResponse, execution::GenerationSettings, projects::ProjectWorkspaceRequest};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskSpec {
    pub title: Option<String>,
    pub prompt: String,
    /// Provider-qualified model identifier.
    pub model: String,
    #[serde(default)]
    pub settings: GenerationSettings,
    pub project: Option<ProjectWorkspaceRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CreateTaskRequest {
    pub command_id: String,
    pub spec: TaskSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunResponse {
    pub id: String,
    pub task_id: String,
    pub attempt: u32,
    pub retry_of: Option<String>,
    pub session_id: String,
    pub workspace_id: Option<String>,
    pub effects_unknown: bool,
    pub turn: TurnResponse,
}

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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskReceipt {
    pub command_id: String,
    pub task_id: String,
    pub run_id: String,
    pub session_id: String,
    pub turn_id: String,
    pub event_sequence: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetryRunRequest {
    pub command_id: String,
    #[serde(default)]
    pub acknowledge_unknown_effects: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskEventResponse {
    pub task_id: String,
    pub sequence: u64,
    pub run_id: String,
    pub kind: String,
    pub status: String,
    pub session_event_sequence: Option<u64>,
}

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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CombinationResponse {
    pub index: u32,
    pub prompt_index: u32,
    pub model_index: u32,
    pub settings_index: u32,
    pub spec: TaskSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BatchPreviewResponse {
    /// Effective settings and exact base commit are safe to submit unchanged.
    pub spec: BatchSpec,
    pub combinations: Vec<CombinationResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CreateBatchRequest {
    pub command_id: String,
    pub spec: BatchSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BatchReceipt {
    pub command_id: String,
    pub batch_id: String,
    pub members: Vec<TaskReceipt>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BatchResponse {
    pub id: String,
    pub owner_node_id: String,
    pub spec: BatchSpec,
    pub total: u32,
    pub statuses: std::collections::BTreeMap<String, u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BatchResultResponse {
    pub task: TaskResponse,
    pub output: Option<crate::chat::MessageResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetryBatchRequest {
    pub command_id: String,
    pub indices: Vec<u32>,
    #[serde(default)]
    pub acknowledge_unknown_effects: bool,
}
