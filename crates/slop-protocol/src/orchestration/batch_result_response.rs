use crate::chat::SessionResponse;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BatchResultResponse {
    pub task: SessionResponse,
    pub output: Option<crate::chat::MessageResponse>,
}
