use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolInvocationResponse {
    pub id: String,
    pub turn_id: String,
    pub request_id: String,
    pub call_id: String,
    pub name: String,
    pub arguments: Value,
    pub status: String,
    pub output: Option<String>,
    pub artifact_ids: Vec<String>,
    pub error_code: Option<String>,
    pub effects_unknown: bool,
}
