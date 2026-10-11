use super::super::Usage;
use super::{FinishReason, InferenceMessage};

#[derive(Debug, Clone)]
pub struct InferenceResponse {
    pub resolved_model: Option<String>,
    pub message: InferenceMessage,
    pub usage: Usage,
    pub finish_reason: FinishReason,
}
