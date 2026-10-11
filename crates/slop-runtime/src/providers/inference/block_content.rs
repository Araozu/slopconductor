use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BlockContent {
    Text {
        text: String,
    },
    ToolCall {
        call_id: String,
        provider_call_id: String,
        name: String,
        arguments: Value,
    },
    ToolResult {
        call_id: String,
        provider_call_id: String,
        is_error: bool,
        output: String,
        artifact_ids: Vec<String>,
        effects_unknown: bool,
    },
    Refusal {
        text: String,
    },
}
