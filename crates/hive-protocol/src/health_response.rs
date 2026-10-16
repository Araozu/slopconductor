use serde::{Deserialize, Serialize};

/// Daemon identity and advertised capabilities, independent of any client UI.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct HealthResponse {
    pub service: String,
    pub version: String,
    pub api_version: u32,
    pub capabilities: Vec<String>,
}
