use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct ModelResponse {
    pub id: String,
    pub provider: String,
    pub model: String,
    pub display_name: String,
    pub ready: bool,
    pub is_default: bool,
    pub reason: Option<String>,
    #[serde(default)]
    pub capabilities: crate::execution::ModelCapabilities,
}
