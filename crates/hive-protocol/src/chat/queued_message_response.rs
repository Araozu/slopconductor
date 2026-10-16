use super::DeliveryMode;
use serde::{Deserialize, Serialize};

/// A durable, unsent message in session FIFO order.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct QueuedMessageResponse {
    pub id: String,
    pub text: String,
    pub delivery: DeliveryMode,
}

/// Change an unsent message in place, preserving its FIFO position and settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct UpdateQueuedMessageRequest {
    pub command_id: String,
    pub text: Option<String>,
    pub delivery: Option<DeliveryMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct InterruptSessionRequest {
    pub command_id: String,
    /// Pin cancellation to the run the client saw, so a late request cannot
    /// cancel its successor. Omit to interrupt the current run.
    pub turn_id: Option<String>,
    /// Atomically accept prompts already submitted by the client, including
    /// requests whose HTTP acknowledgement has not arrived yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<super::SendMessageRequest>,
}
