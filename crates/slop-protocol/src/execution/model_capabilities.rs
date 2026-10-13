use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelCapabilities {
    pub tools: bool,
    pub reasoning_efforts: Vec<String>,
    pub incremental_streaming: bool,
    pub max_output_tokens: u32,
    #[serde(default = "default_output_token_cap_support")]
    pub output_token_cap_supported: bool,
    #[serde(default)]
    pub context_window_tokens: Option<u64>,
    #[serde(default)]
    pub max_input_tokens: Option<u64>,
}

fn default_output_token_cap_support() -> bool {
    true
}
