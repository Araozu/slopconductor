use super::RunResponse;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct RunResultResponse {
    pub run: RunResponse,
    pub output: Option<crate::chat::MessageResponse>,
}
