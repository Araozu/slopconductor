use serde::{Deserialize, Serialize};

fn default_mode() -> String {
    "all".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WaitChildrenRequest {
    pub command_id: String,
    #[serde(default)]
    pub child_run_ids: Vec<String>,
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}
