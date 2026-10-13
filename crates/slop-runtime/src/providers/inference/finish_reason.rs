use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    ToolCalls,
    OutputLimit,
    /// An incomplete response without a recognized token-limit reason.
    Incomplete,
    ContentFilter,
    Refusal,
}
