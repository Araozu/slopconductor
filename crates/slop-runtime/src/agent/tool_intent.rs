use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolIntent {
    pub id: String,
    pub request_id: String,
    pub result_message_id: String,
    pub call_id: String,
    pub provider_call_id: String,
    pub name: String,
    pub arguments: Value,
}
