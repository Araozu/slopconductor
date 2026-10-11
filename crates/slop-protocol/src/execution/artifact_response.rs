use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactResponse {
    pub id: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub media_type: String,
}
