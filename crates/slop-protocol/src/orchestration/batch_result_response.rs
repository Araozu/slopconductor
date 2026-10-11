use super::TaskResponse;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BatchResultResponse {
    pub task: TaskResponse,
    pub output: Option<crate::chat::MessageResponse>,
}
