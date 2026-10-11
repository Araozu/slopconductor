use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelCapabilities {
    pub tools: bool,
    pub reasoning_efforts: Vec<String>,
    pub incremental_streaming: bool,
    pub max_output_tokens: u32,
    #[serde(default = "default_output_token_cap_support")]
    pub output_token_cap_supported: bool,
}

fn default_output_token_cap_support() -> bool {
    true
}
