//! Public structured execution records. No provider-private continuation or
//! credentials cross this boundary.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationSettings {
    pub max_output_tokens: Option<u32>,
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspacePolicy {
    /// Absolute path on the authoritative daemon's machine.
    pub root: String,
    pub allowed_tools: Vec<String>,
    #[serde(default = "default_timeout")]
    pub shell_timeout_ms: u64,
    #[serde(default = "default_output")]
    pub max_output_bytes: usize,
    #[serde(default = "default_calls")]
    pub max_tool_calls: u32,
    #[serde(default = "default_requests")]
    pub max_model_requests: u32,
}
fn default_timeout() -> u64 {
    30_000
}
fn default_output() -> usize {
    1024 * 1024
}
fn default_calls() -> u32 {
    32
}
fn default_requests() -> u32 {
    16
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContentBlock {
    pub id: String,
    #[serde(flatten)]
    pub content: BlockContent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BlockContent {
    Text {
        text: String,
    },
    ToolCall {
        call_id: String,
        name: String,
        arguments: Value,
    },
    ToolResult {
        call_id: String,
        is_error: bool,
        output: String,
        artifact_ids: Vec<String>,
        effects_unknown: bool,
    },
    Refusal {
        text: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelCapabilities {
    pub tools: bool,
    pub reasoning_efforts: Vec<String>,
    pub incremental_streaming: bool,
    pub max_output_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub side_effects: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilitiesResponse {
    pub structured_messages: bool,
    pub per_turn_model_selection: bool,
    pub per_turn_settings: bool,
    pub tools: Vec<ToolDescriptor>,
    pub max_model_requests_per_turn: u32,
    pub max_tool_calls_per_turn: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelRequestResponse {
    pub id: String,
    pub turn_id: String,
    pub message_id: String,
    pub ordinal: u32,
    pub requested_model: String,
    pub resolved_model: Option<String>,
    pub requested_settings: GenerationSettings,
    pub effective_settings: GenerationSettings,
    pub status: String,
    pub finish_reason: Option<String>,
    pub usage: Option<crate::chat::UsageResponse>,
    pub error_code: Option<String>,
}

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactResponse {
    pub id: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub media_type: String,
}
