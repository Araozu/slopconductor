use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationSettings {
    pub max_output_tokens: Option<u32>,
    pub reasoning_effort: Option<String>,
    /// Use the model's full window; absent inherits the session's opt-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_context: Option<bool>,
}
