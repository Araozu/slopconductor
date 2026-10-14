use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct ProviderStatus {
    pub provider: String,
    pub api_key_configured: bool,
    pub chatgpt_configured: bool,
    pub execution_supported: bool,
    #[serde(default)]
    pub active_auth_mode: Option<String>,
}
