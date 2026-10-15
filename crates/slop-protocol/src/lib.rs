//! Public wire types shared by the daemon and native clients.
//!
//! This crate must remain independent of the agent runtime and domain internals.

/// Bootstrap API version. Compatibility is checked by native clients.
pub const API_VERSION: u32 = 1;

/// Anonymous health endpoint.
pub const HEALTH_PATH: &str = "/v1/health";

/// Authenticated query for the daemon's stable node identity.
pub const NODE_PATH: &str = "/v1/node";

/// Session and durable text chat routes.
pub const SESSIONS_PATH: &str = "/v1/sessions";
pub const MODELS_PATH: &str = "/v1/models";
pub const PROVIDERS_PATH: &str = "/v1/providers";

/// Local development endpoint; remote authentication is a later milestone.
pub const DEFAULT_DAEMON_URL: &str = "http://127.0.0.1:7331";

/// Service identity used to detect an accidentally selected endpoint.
pub const SERVICE_NAME: &str = "slopconductor-daemon";

mod health_response;
pub use health_response::HealthResponse;
mod node_response;
pub use node_response::NodeResponse;
mod error_response;
pub use error_response::ErrorResponse;

pub mod chat;
pub mod execution;
pub mod orchestration;
pub mod providers;
pub mod workspaces;

pub mod filesystem;
pub mod peers;
