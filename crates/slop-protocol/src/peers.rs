//! Explicit peer trust and owner-targeted access. Secrets are never returned.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum PeerScope {
    #[default]
    Read,
    /// Includes read access and steering/control of existing owner sessions.
    Control,
}

#[derive(Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreatePeerGrantRequest {
    pub label: String,
    #[serde(default)]
    pub scope: PeerScope,
    /// A caller-generated 256-bit bearer, encoded as 64 lowercase hex digits.
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct PeerGrantResponse {
    pub id: String,
    pub owner_node_id: String,
    pub label: String,
    pub scope: PeerScope,
    pub revoked: bool,
}

#[derive(Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RegisterPeerRequest {
    /// Exact HTTPS origin. HTTP is accepted only for literal loopback addresses.
    pub origin: String,
    pub owner_node_id: String,
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct PeerResponse {
    pub id: String,
    pub origin: String,
    pub owner_node_id: String,
    /// Removed registrations remain tombstones; reuse requires a new alias.
    pub removed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PeerProbeResponse {
    pub peer: PeerResponse,
    pub node: crate::NodeResponse,
}

impl std::fmt::Debug for CreatePeerGrantRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreatePeerGrantRequest")
            .field("label", &self.label)
            .field("scope", &self.scope)
            .field("token", &"[REDACTED]")
            .finish()
    }
}
impl std::fmt::Debug for RegisterPeerRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisterPeerRequest")
            .field("origin", &self.origin)
            .field("owner_node_id", &self.owner_node_id)
            .field("token", &"[REDACTED]")
            .finish()
    }
}
