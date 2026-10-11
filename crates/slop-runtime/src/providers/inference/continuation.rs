use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Runtime-private replay material. It never appears in public message DTOs or
/// deltas. Required continuation may only be replayed with its original model.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Continuation {
    pub scope: String,
    pub provider: String,
    pub model: String,
    pub wire: String,
    pub required: bool,
    pub items: Vec<Value>,
}

impl std::fmt::Debug for Continuation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Continuation")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("required", &self.required)
            .finish_non_exhaustive()
    }
}
