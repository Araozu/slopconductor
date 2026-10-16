use crate::chat::SessionResponse;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct BatchResultResponse {
    pub task: SessionResponse,
    pub output: Option<crate::chat::MessageResponse>,
}
