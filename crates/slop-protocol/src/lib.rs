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

/// Session and durable text chat routes.
pub const SESSIONS_PATH: &str = "/v1/sessions";
pub const MODELS_PATH: &str = "/v1/models";
pub const PROVIDERS_PATH: &str = "/v1/providers";

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

/// Credential management never returns stored secrets.
pub mod providers {
    use serde::{Deserialize, Serialize};

    pub const MAX_API_KEY_BYTES: usize = 16 * 1024;

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct SetApiKeyRequest {
        pub api_key: String,
    }

    impl std::fmt::Debug for SetApiKeyRequest {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("SetApiKeyRequest([REDACTED])")
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    pub struct ProviderStatus {
        pub provider: String,
        pub api_key_configured: bool,
        pub chatgpt_configured: bool,
        pub execution_supported: bool,
        #[serde(default)]
        pub active_auth_mode: Option<String>,
    }

    /// The URL is an ephemeral authorization link, never an access token.
    #[derive(Clone, Serialize, Deserialize)]
    pub struct LoginResponse {
        pub login_id: String,
        pub authorization_url: String,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct StartLoginRequest {
        pub command_id: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    pub struct LoginStatus {
        pub login_id: String,
        pub status: String,
        pub error_code: Option<String>,
    }
}

/// Durable text-chat wire objects. These types intentionally contain no
/// provider SDK or runtime implementation details.
pub mod chat {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    pub struct SessionResponse {
        pub id: String,
        pub owner_node_id: String,
        pub title: Option<String>,
        pub provider: String,
        pub model: String,
        pub max_tokens: Option<u32>,
        pub revision: u64,
        pub last_event_sequence: u64,
        #[serde(default)]
        pub settings: crate::execution::GenerationSettings,
        #[serde(default)]
        pub execution: Option<crate::execution::WorkspacePolicy>,
    }

    #[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
    pub struct CreateSessionRequest {
        pub command_id: String,
        pub title: Option<String>,
        pub provider: String,
        pub model: String,
        pub max_tokens: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub settings: Option<crate::execution::GenerationSettings>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub execution: Option<crate::execution::WorkspacePolicy>,
    }

    #[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
    pub struct SendMessageRequest {
        pub command_id: String,
        pub text: String,
        pub expected_revision: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub delivery: Option<DeliveryMode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub settings: Option<crate::execution::GenerationSettings>,
    }

    #[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "kebab-case")]
    pub enum DeliveryMode {
        #[default]
        AfterTurn,
        NextBoundary,
        Immediate,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    pub struct SteeringInstructionResponse {
        pub id: String,
        pub turn_id: String,
        pub text: String,
        pub delivery: DeliveryMode,
        pub status: String,
        pub error_code: Option<String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    pub struct TurnControlRequest {
        pub command_id: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    pub struct CancelTurnRequest {
        pub command_id: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    pub struct CommandReceipt {
        pub command_id: String,
        pub session_id: String,
        pub turn_id: Option<String>,
        pub message_id: Option<String>,
        pub revision: u64,
        pub event_sequence: u64,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    pub struct MessageResponse {
        pub id: String,
        pub session_id: String,
        pub turn_id: String,
        pub role: String,
        pub text: String,
        pub status: String,
        #[serde(default)]
        pub blocks: Vec<crate::execution::ContentBlock>,
        #[serde(default)]
        pub request_id: Option<String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    pub struct UsageResponse {
        pub input_tokens: Option<u64>,
        pub output_tokens: Option<u64>,
        pub total_tokens: Option<u64>,
        #[serde(default)]
        pub total_source: Option<String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    pub struct TurnResponse {
        pub id: String,
        pub session_id: String,
        pub user_message_id: String,
        pub assistant_message_id: Option<String>,
        pub status: String,
        pub requested_model: String,
        pub resolved_model: Option<String>,
        pub usage: Option<UsageResponse>,
        pub error_code: Option<String>,
        pub error_message: Option<String>,
        #[serde(default)]
        pub settings: crate::execution::GenerationSettings,
        #[serde(default)]
        pub model_request_ids: Vec<String>,
        #[serde(default)]
        pub tool_invocation_ids: Vec<String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    pub struct Page<T> {
        pub items: Vec<T>,
        pub next_after: Option<u64>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    pub struct EventResponse {
        pub session_id: String,
        pub sequence: u64,
        pub kind: String,
        pub turn_id: Option<String>,
        pub message_id: Option<String>,
        pub revision: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub request_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub invocation_id: Option<String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    pub struct ModelResponse {
        pub id: String,
        pub provider: String,
        pub model: String,
        pub display_name: String,
        pub ready: bool,
        pub is_default: bool,
        pub reason: Option<String>,
        #[serde(default)]
        pub capabilities: crate::execution::ModelCapabilities,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum EventFrame {
        Durable {
            event: EventResponse,
        },
        Delta {
            session_id: String,
            turn_id: String,
            text: String,
            #[serde(default)]
            message_id: Option<String>,
            #[serde(default)]
            block_id: Option<String>,
            #[serde(default)]
            request_id: Option<String>,
            #[serde(default)]
            invocation_id: Option<String>,
            #[serde(default)]
            stream_id: String,
            #[serde(default)]
            chunk_index: u64,
            #[serde(default = "default_delta_kind")]
            kind: String,
        },
        Heartbeat,
    }

    fn default_delta_kind() -> String {
        "text".to_owned()
    }
}

pub mod execution;
