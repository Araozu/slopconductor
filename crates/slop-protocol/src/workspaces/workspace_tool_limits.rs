use serde::{Deserialize, Serialize};
/// Per-session execution limits, independent of workspace lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspaceToolLimits {
    pub shell_timeout_ms: u64,
    pub max_output_bytes: usize,
    pub max_tool_calls: u32,
    pub max_model_requests: u32,
}
impl Default for WorkspaceToolLimits {
    fn default() -> Self {
        Self {
            shell_timeout_ms: 30_000,
            max_output_bytes: 1024 * 1024,
            max_tool_calls: u32::MAX,
            max_model_requests: u32::MAX,
        }
    }
}
