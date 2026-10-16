use super::EventResponse;
use serde::{Deserialize, Serialize};

fn default_delta_kind() -> String {
    "text".to_owned()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventFrame {
    Durable {
        event: EventResponse,
    },
    Delta {
        session_id: String,
        turn_id: String,
        text: String,
        #[serde(default)]
        message_id: Option<String>,
        #[serde(default)]
        block_id: Option<String>,
        #[serde(default)]
        request_id: Option<String>,
        #[serde(default)]
        invocation_id: Option<String>,
        #[serde(default)]
        stream_id: String,
        #[serde(default)]
        chunk_index: u64,
        #[serde(default = "default_delta_kind")]
        kind: String,
    },
    Heartbeat,
}
