use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandReceipt {
    pub command_id: String,
    pub session_id: String,
    pub turn_id: Option<String>,
    pub message_id: Option<String>,
    pub revision: u64,
    pub event_sequence: u64,
}
