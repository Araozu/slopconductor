use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationSettings {
    pub max_output_tokens: Option<u32>,
    /// Values are model-specific and validated against its descriptor. An
    /// absent value means the provider default, not a fabricated effort level.
    pub reasoning_effort: Option<String>,
    /// Explicit opt-in to the model's full window instead of the local cost cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_context: Option<bool>,
}
