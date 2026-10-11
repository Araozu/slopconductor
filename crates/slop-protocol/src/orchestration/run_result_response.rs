use super::RunResponse;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunResultResponse {
    pub run: RunResponse,
    pub output: Option<crate::chat::MessageResponse>,
}
