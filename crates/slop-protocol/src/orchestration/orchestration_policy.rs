use serde::{Deserialize, Serialize};

fn default_concurrent_children() -> u32 {
    4
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OrchestrationPolicy {
    /// Maximum concurrently outstanding direct children (queued, active, or paused).
    /// Aggregate operation budgets remain the cumulative lifetime bound.
    pub max_children: u32,
    /// Maximum simultaneously running direct children; queued children use max_children.
    #[serde(default = "default_concurrent_children")]
    pub max_concurrent_children: u32,
    /// Remaining delegation depth; a leaf has no orchestration policy.
    pub max_depth: u32,
    /// Empty means every model in the daemon's supported provider-qualified catalog.
    pub allowed_models: Vec<String>,
    pub allowed_tools: Vec<String>,
}
