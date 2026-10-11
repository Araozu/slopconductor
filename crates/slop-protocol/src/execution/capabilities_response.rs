use super::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilitiesResponse {
    pub structured_messages: bool,
    pub per_turn_model_selection: bool,
    pub per_turn_settings: bool,
    pub tools: Vec<ToolDescriptor>,
    pub max_model_requests_per_turn: u32,
    pub max_tool_calls_per_turn: u32,
    #[serde(default)]
    pub execution_steering: bool,
    #[serde(default)]
    pub turn_pause_resume: bool,
}
