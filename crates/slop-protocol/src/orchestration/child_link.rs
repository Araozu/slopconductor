use super::ChildContext;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChildLink {
    pub parent_task_id: String,
    pub parent_run_id: String,
    pub root_task_id: String,
    pub depth: u32,
    pub cancel_with_parent: bool,
    pub context: ChildContext,
}
