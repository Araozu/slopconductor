use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct ArtifactResponse {
    pub id: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub media_type: String,
}
