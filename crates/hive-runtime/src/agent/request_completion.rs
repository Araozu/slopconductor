use super::super::providers::inference::InferenceResponse;
use super::tool_intent::ToolIntent;

#[derive(Debug, Clone)]
pub struct RequestCompletion {
    pub request_id: String,
    pub response: InferenceResponse,
    pub tools: Vec<ToolIntent>,
    pub compaction: Option<super::context::ContextCheckpoint>,
}
