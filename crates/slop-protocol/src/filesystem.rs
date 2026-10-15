//! Filesystem navigation on the authenticated daemon's machine.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct DirectoryEntry {
    pub name: String,
    pub path: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct DirectoryResponse {
    pub path: String,
    pub parent: Option<String>,
    pub entries: Vec<DirectoryEntry>,
    pub next_after: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CreateDirectoryRequest {
    pub parent: String,
    pub name: String,
}
