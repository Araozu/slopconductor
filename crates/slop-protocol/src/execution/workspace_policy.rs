use serde::{Deserialize, Serialize};

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
