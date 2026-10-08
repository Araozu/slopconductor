//! Public wire types shared by the daemon and native clients.
//!
//! This crate must remain independent of the agent runtime and domain internals.

use serde::{Deserialize, Serialize};

/// Bootstrap API version. Compatibility is checked by native clients.
pub const API_VERSION: u32 = 1;

/// The only endpoint implemented by the bootstrap daemon.
pub const HEALTH_PATH: &str = "/v1/health";

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
