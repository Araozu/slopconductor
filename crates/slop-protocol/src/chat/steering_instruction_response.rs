use super::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct SteeringInstructionResponse {
    pub id: String,
    pub turn_id: String,
    pub text: String,
    pub delivery: DeliveryMode,
    pub status: String,
    pub error_code: Option<String>,
}
