//! Public wire types shared by the daemon and native clients.
//!
//! This crate must remain independent of the agent runtime and domain internals.

use serde::{Deserialize, Serialize};

/// Bootstrap API version. Compatibility is checked by native clients.
pub const API_VERSION: u32 = 1;

/// Anonymous health endpoint.
pub const HEALTH_PATH: &str = "/v1/health";

/// Authenticated query for the daemon's stable node identity.
pub const NODE_PATH: &str = "/v1/node";

/// Local development endpoint; remote authentication is a later milestone.
pub const DEFAULT_DAEMON_URL: &str = "http://127.0.0.1:7331";

/// Service identity used to detect an accidentally selected endpoint.
pub const SERVICE_NAME: &str = "slopconductor-daemon";

/// Daemon identity and advertised capabilities, independent of any client UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub service: String,
    pub version: String,
    pub api_version: u32,
    pub capabilities: Vec<String>,
}

/// Stable node identity returned by the authenticated node query.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeResponse {
    pub node_id: String,
    pub name: String,
    pub os: String,
}

/// Safe structured error returned by public API endpoints.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorResponse {
    pub code: String,
    pub message: String,
}
