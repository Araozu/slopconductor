use super::TaskSpec;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CombinationResponse {
    pub index: u32,
    pub prompt_index: u32,
    pub model_index: u32,
    pub settings_index: u32,
    pub spec: TaskSpec,
}
