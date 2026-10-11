use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OrchestrationPolicy {
    /// Maximum children across this root's entire tree, and direct fan-out at this node.
    pub max_children: u32,
    /// Remaining delegation depth; a leaf has no orchestration policy.
    pub max_depth: u32,
    pub allowed_models: Vec<String>,
    pub allowed_tools: Vec<String>,
}
