use crate::{
    orchestration::OrchestrationPolicy,
    providers::{
        Usage,
        inference::{GenerationSettings, InferenceMessage},
    },
    tools::WorkspacePolicy,
};

use super::ContextMessage;

/// Durable input chosen when the repository claims a turn. Later queued user
/// messages must never be appended to this context after claim.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnWork {
    pub turn_id: String,
    pub session_id: String,
    pub requested_model: String,
    pub max_tokens: Option<u32>,
    pub messages: Vec<ContextMessage>,
    pub history: Vec<InferenceMessage>,
    pub settings: GenerationSettings,
    pub requested_settings: GenerationSettings,
    pub execution: Option<WorkspacePolicy>,
    pub orchestration: Option<OrchestrationPolicy>,
    pub next_request_ordinal: u32,
    pub max_model_requests: u32,
    pub tool_calls_used: u32,
    pub output_limits_seen: u32,
    pub initial_usage: Usage,
    pub resume_count: u32,
    pub context_usage: Option<crate::agent::context::ContextUsage>,
    pub context_token_budget: u64,
    pub context_overflow: bool,
    pub context_through_ordinal: i64,
    pub compactions_seen: u32,
}
