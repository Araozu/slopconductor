use crate::chat::ModelResponse;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProviderCatalog {
    pub provider: String,
    pub auth_mode: String,
    pub account_id: Option<String>,
    pub observed_at_unix_ms: u64,
    pub source: String,
    pub models: Vec<ModelResponse>,
}
