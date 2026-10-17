//! OpenAPI document and Swagger UI routes for the daemon HTTP API.
//!
//! The document is generated at compile time with `utoipa` and served at
//! `GET /api-docs/openapi.json` (plus Swagger UI at `GET /docs`). The
//! generated JSON is deliberately *not* checked into source control: regenerate
//! it locally with `cargo run -p hive-daemon -- --dump-openapi` (see
//! `AGENTS.md`).
//!
//! To keep the document accurate:
//! - Derive `utoipa::ToSchema` on every new `hive-protocol` request/response type.
//! - Annotate every new handler with `#[utoipa::path(...)]`.
//! - Register the handler and all new schemas in `ApiDoc` below and extend
//!   `EXPECTED_API_PATHS` in the coverage test.

use utoipa::OpenApi;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::openapi::{OpenApi as OpenApiDoc, Server};
use utoipa_swagger_ui::SwaggerUi;

/// Bearer-token security scheme used by every authenticated daemon route.
///
/// Local development uses the token in
/// `<data-dir>/credentials/local-api-token` as `Authorization: Bearer <token>`.
struct BearerSecurity;

impl utoipa::Modify for BearerSecurity {
    fn modify(&self, openapi: &mut OpenApiDoc) {
        let scheme = SecurityScheme::Http(
            HttpBuilder::new()
                .scheme(HttpAuthScheme::Bearer)
                .bearer_format("opaque")
                .description(Some("Local daemon bearer token"))
                .build(),
        );
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme("bearer_auth", scheme);
        }
    }
}

/// Versioned public API served by `hived`.
///
/// Only handlers annotated with `#[utoipa::path]` appear under `paths`. Every
/// other schema listed here is still emitted under `components/schemas` so
/// clients can codegen against the full wire contract.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Hive Daemon API",
        version = "1.0",
        description = "Local administrator API, including the explicit peer gateway. Health is anonymous; protected routes require the local administrator bearer. The optional separate peer listener accepts scoped peer bearers and exposes only the session/turn inspection and control subset documented in docs/daemon-peering.md."
    ),
    servers(
        (url = "http://127.0.0.1:7331", description = "Local daemon default")
    ),
    paths(
        super::peers::grants,
        super::peers::create_grant,
        super::peers::revoke_grant,
        super::peers::list_peers,
        super::peers::register_peer,
        super::peers::remove_peer,
        super::peers::probe,
        super::peers::remote_health,
        super::peers::remote_node,
        super::peers::remote_sessions,
        super::peers::remote_session,
        super::peers::remote_history,
        super::peers::remote_instructions,
        super::peers::remote_turn,
        super::peers::remote_message,
        super::peers::remote_tool,
        super::peers::remote_send,
        super::peers::remote_cancel,
        super::peers::remote_pause,
        super::peers::remote_resume,
        super::peers::remote_events,
        crate::health,
        crate::node,
        // Chat: durable sessions, text chat, history, drafts, inbox, events,
        // turns, tool invocations, messages, artifacts, models.
        super::chat::execution::capabilities,
        super::chat::execution::models,
        super::chat::sessions::sessions,
        super::chat::sessions::create_session,
        super::chat::sessions::session,
        super::chat::sessions::rename_session,
        super::chat::history::fork_session,
        super::chat::history::edit_message,
        super::chat::history::delete_message,
        super::chat::sessions::update_session_inbox,
        super::chat::sessions::drafts,
        super::chat::sessions::save_draft,
        super::chat::sessions::remove_draft,
        super::chat::sessions::messages,
        super::chat::sessions::send_message,
        super::chat::queue::queued_messages,
        super::chat::queue::update_queued_message,
        super::chat::queue::cancel_queued_message,
        super::chat::queue::interrupt_session,
        super::chat::execution::steering_instructions,
        super::chat::events::events,
        super::chat::turns::turn,
        super::chat::execution::model_requests,
        super::chat::execution::tool_invocations,
        super::chat::execution::tool_invocation,
        super::chat::execution::message,
        super::chat::execution::artifact,
        super::chat::execution::artifact_content,
        super::chat::turns::cancel_turn,
        super::chat::turns::pause_turn,
        super::chat::turns::resume_turn,
        // Orchestration: tasks, runs, batches, children, waits, results.
        super::orchestration::create_task,
        super::orchestration::tasks,
        super::orchestration::task,
        super::orchestration::runs,
        super::orchestration::events,
        super::orchestration::instruction,
        super::orchestration::run,
        super::orchestration::pause,
        super::orchestration::resume,
        super::orchestration::cancel,
        super::orchestration::retry,
        super::orchestration::children,
        super::orchestration::create_child,
        super::orchestration::wait_children,
        super::orchestration::run_result,
        super::orchestration::preview,
        super::orchestration::submit,
        super::orchestration::batches,
        super::orchestration::batch,
        super::orchestration::members,
        super::orchestration::results,
        super::orchestration::retry_batch,
        super::orchestration::cancel_batch,
        // Provider credentials: statuses, API keys, discovery, Codex login.
        super::providers::statuses,
        super::providers::set_api_key,
        super::providers::remove_api_key,
        super::providers::discover,
        super::providers::accounts,
        super::providers::select_auth,
        super::providers::sign_out,
        super::providers::start_login,
        super::providers::login_callback,
        super::providers::login_status,
        // Workspaces and local filesystem navigation.
        super::filesystem::browse,
        super::filesystem::create,
        super::workspaces::open,
        super::workspaces::create,
        super::workspaces::worktree,
        super::workspaces::workspaces,
        super::workspaces::events,
        super::workspaces::workspace,
        super::workspaces::diff,
        super::workspaces::remove,
        super::workspaces::rename,
        super::workspaces::set_color,
    ),
    components(
        schemas(
            hive_protocol::HealthResponse,
            hive_protocol::peers::PeerScope,
            hive_protocol::peers::CreatePeerGrantRequest,
            hive_protocol::peers::PeerGrantResponse,
            hive_protocol::peers::RegisterPeerRequest,
            hive_protocol::peers::PeerResponse,
            hive_protocol::peers::PeerProbeResponse,

            hive_protocol::NodeResponse,
            hive_protocol::ErrorResponse,
            hive_protocol::filesystem::DirectoryEntry,
            hive_protocol::filesystem::DirectoryResponse,
            hive_protocol::filesystem::CreateDirectoryRequest,
            hive_protocol::workspaces::CreateWorkspaceRequest,
            hive_protocol::chat::CancelTurnRequest,
            hive_protocol::chat::CommandReceipt,
            hive_protocol::chat::QueuedMessageResponse,
            hive_protocol::chat::UpdateQueuedMessageRequest,
            hive_protocol::chat::InterruptSessionRequest,
            hive_protocol::chat::CreateSessionRequest,
            hive_protocol::chat::RenameSessionRequest,
            hive_protocol::chat::DeleteMessageRequest,
            hive_protocol::chat::DeliveryMode,
            hive_protocol::chat::EditMessageRequest,
            hive_protocol::chat::EventFrame,
            hive_protocol::chat::EventResponse,
            hive_protocol::chat::ForkSessionRequest,
            hive_protocol::chat::SessionFork,
            hive_protocol::chat::SessionInbox,
            hive_protocol::chat::UpdateSessionInboxRequest,
            hive_protocol::chat::ConversationDraft,
            hive_protocol::chat::SaveConversationDraftRequest,
            hive_protocol::chat::MessageResponse,
            hive_protocol::chat::ModelResponse,
            hive_protocol::chat::SendMessageRequest,
            hive_protocol::chat::SessionResponse,
            hive_protocol::chat::SessionStatus,
            hive_protocol::chat::SteeringInstructionResponse,
            hive_protocol::chat::TurnControlRequest,
            hive_protocol::chat::TurnResponse,
            hive_protocol::chat::UsageResponse,
            hive_protocol::execution::ArtifactResponse,
            hive_protocol::execution::BlockContent,
            hive_protocol::execution::CapabilitiesResponse,
            hive_protocol::execution::ContentBlock,
            hive_protocol::execution::GenerationSettings,
            hive_protocol::execution::ModelCapabilities,
            hive_protocol::execution::ModelRequestResponse,
            hive_protocol::execution::ToolDescriptor,
            hive_protocol::execution::ToolInvocationResponse,
            hive_protocol::execution::WorkspacePolicy,
            hive_protocol::orchestration::BatchCancelReceipt,
            hive_protocol::orchestration::BatchPreviewResponse,
            hive_protocol::orchestration::BatchReceipt,
            hive_protocol::orchestration::BatchResponse,
            hive_protocol::orchestration::BatchResultResponse,
            hive_protocol::orchestration::BatchSpec,
            hive_protocol::orchestration::BudgetUsage,
            hive_protocol::orchestration::ChildContext,
            hive_protocol::orchestration::ChildLink,
            hive_protocol::orchestration::CombinationResponse,
            hive_protocol::orchestration::CreateBatchRequest,
            hive_protocol::orchestration::CreateChildRequest,
            hive_protocol::orchestration::CreateTaskRequest,
            hive_protocol::orchestration::OperationBudget,
            hive_protocol::orchestration::OrchestrationPolicy,
            hive_protocol::orchestration::RetryBatchRequest,
            hive_protocol::orchestration::RetryRunRequest,
            hive_protocol::orchestration::RunResponse,
            hive_protocol::orchestration::RunResultResponse,
            hive_protocol::orchestration::SessionSpec,
            hive_protocol::orchestration::TaskEventResponse,
            hive_protocol::orchestration::TaskReceipt,
            hive_protocol::orchestration::WaitChildrenRequest,
            hive_protocol::workspaces::WorkspaceEventResponse,
            hive_protocol::workspaces::WorkspaceSelection,
            hive_protocol::workspaces::WorkspaceToolLimits,
            hive_protocol::workspaces::OpenWorkspaceRequest,
            hive_protocol::workspaces::CreateWorktreeRequest,
            hive_protocol::workspaces::RemoveWorkspaceRequest,
            hive_protocol::workspaces::RenameWorkspaceRequest,
            hive_protocol::workspaces::SetWorkspaceColorRequest,
            hive_protocol::workspaces::WorkspaceDiffResponse,
            hive_protocol::workspaces::WorkspaceResponse,
            hive_protocol::providers::LoginResponse,
            hive_protocol::providers::LoginCallbackRequest,
            hive_protocol::providers::LoginStatus,
            hive_protocol::providers::ProviderAccount,
            hive_protocol::providers::ProviderCatalog,
            hive_protocol::providers::ProviderStatus,
            hive_protocol::providers::SelectAuthRequest,
            hive_protocol::providers::SetApiKeyRequest,
            hive_protocol::providers::SignOutResponse,
            hive_protocol::providers::StartLoginRequest,
        )
    ),
    modifiers(&BearerSecurity),
    tags(
        (name = "peers", description = "Local peer administration and scoped owner access"),
        (name = "system", description = "Health and node identity"),
        (name = "chat", description = "Durable sessions and text chat"),
        (name = "orchestration", description = "Tasks, run attempts, and batch matrices"),
        (name = "providers", description = "Provider credentials and discovery"),
        (name = "workspaces", description = "Open directories in place; explicitly create worktrees from an existing workspace"),
    )
)]
pub struct ApiDoc;

/// Serve the generated document and interactive UI.
///
/// - `GET /api-docs/openapi.json` — the raw OpenAPI 3 document.
/// - `GET /docs` — Swagger UI explorer for the document above.
///
/// The router carries no state and must be merged **after** `with_state` in
/// `main` so an unauthenticated docs bundle never interferes with the
/// authenticated API routers.
pub fn router() -> axum::Router {
    let mut openapi = ApiDoc::openapi();
    // Keep the local default visible even when served from another bind address.
    openapi.servers = Some(vec![Server::new("http://127.0.0.1:7331")]);
    SwaggerUi::new("/docs")
        .url("/api-docs/openapi.json", openapi)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every axum route registered in `main` and `api::*::router` must have a
    /// matching OpenAPI path below. When adding a route, annotate the handler
    /// with `#[utoipa::path]`, register it in `ApiDoc::paths`, and extend
    /// this list.
    const EXPECTED_API_PATHS: &[&str] = &[
        "/v1/peer-grants",
        "/v1/peer-grants/{id}",
        "/v1/peer-grants/{id}/revoke",
        "/v1/peers",
        "/v1/peers/{id}",
        "/v1/peers/{id}/probe",
        "/v1/peers/{peer_id}/health",
        "/v1/peers/{peer_id}/node",
        "/v1/peers/{peer_id}/sessions",
        "/v1/peers/{peer_id}/sessions/{session_id}",
        "/v1/peers/{peer_id}/sessions/{session_id}/messages",
        "/v1/peers/{peer_id}/sessions/{session_id}/instructions",
        "/v1/peers/{peer_id}/sessions/{session_id}/events",
        "/v1/peers/{peer_id}/turns/{turn_id}",
        "/v1/peers/{peer_id}/turns/{turn_id}/cancel",
        "/v1/peers/{peer_id}/turns/{turn_id}/pause",
        "/v1/peers/{peer_id}/turns/{turn_id}/resume",
        "/v1/peers/{peer_id}/messages/{message_id}",
        "/v1/peers/{peer_id}/tools/{tool_id}",
        "/v1/health",
        "/v1/node",
        // Chat.
        "/v1/capabilities",
        "/v1/models",
        "/v1/sessions",
        "/v1/sessions/{session_id}",
        "/v1/sessions/{session_id}/fork",
        "/v1/sessions/{session_id}/messages/{message_id}",
        "/v1/sessions/{session_id}/inbox",
        "/v1/drafts",
        "/v1/drafts/{id}",
        "/v1/sessions/{session_id}/messages",
        "/v1/sessions/{session_id}/queue",
        "/v1/sessions/{session_id}/queue/{message_id}",
        "/v1/sessions/{session_id}/interrupt",
        "/v1/sessions/{session_id}/instructions",
        "/v1/sessions/{session_id}/events",
        "/v1/turns/{turn_id}",
        "/v1/turns/{turn_id}/requests",
        "/v1/turns/{turn_id}/tools",
        "/v1/tools/{id}",
        "/v1/messages/{id}",
        "/v1/artifacts/{id}",
        "/v1/artifacts/{id}/content",
        "/v1/turns/{turn_id}/cancel",
        "/v1/turns/{turn_id}/pause",
        "/v1/turns/{turn_id}/resume",
        // Orchestration.
        "/v1/tasks",
        "/v1/tasks/{id}",
        "/v1/tasks/{id}/runs",
        "/v1/tasks/{id}/events",
        "/v1/tasks/{id}/instructions",
        "/v1/runs/{id}",
        "/v1/runs/{id}/pause",
        "/v1/runs/{id}/resume",
        "/v1/runs/{id}/cancel",
        "/v1/runs/{id}/retry",
        "/v1/runs/{id}/children",
        "/v1/runs/{id}/wait",
        "/v1/runs/{id}/result",
        "/v1/batches/preview",
        "/v1/batches",
        "/v1/batches/{id}",
        "/v1/batches/{id}/members",
        "/v1/batches/{id}/results",
        "/v1/batches/{id}/retry",
        "/v1/batches/{id}/cancel",
        // Providers.
        "/v1/providers",
        "/v1/providers/{provider}/api-key",
        "/v1/providers/{provider}/models",
        "/v1/providers/codex/accounts",
        "/v1/providers/codex/auth",
        "/v1/providers/codex/accounts/{account_id}/logout",
        "/v1/providers/codex/login",
        "/v1/providers/codex/login/{login_id}",
        "/v1/providers/codex/login/{login_id}/callback",
        // Workspaces and filesystem.
        "/v1/filesystem",
        "/v1/filesystem/directories",
        "/v1/workspaces",
        "/v1/workspaces/create",
        "/v1/workspaces/{id}/worktrees",
        "/v1/workspaces/{id}/events",
        "/v1/workspaces/{id}",
        "/v1/workspaces/{id}/diff",
        "/v1/workspaces/{id}/remove",
        "/v1/workspaces/{id}/rename",
        "/v1/workspaces/{id}/color",
    ];

    #[test]
    fn openapi_document_covers_every_route() {
        let doc = ApiDoc::openapi();
        let json = serde_json::to_value(&doc).expect("OpenAPI document serializes to JSON");
        let paths = json
            .get("paths")
            .and_then(|paths| paths.as_object())
            .expect("document has paths");
        assert_eq!(
            paths.len(),
            EXPECTED_API_PATHS.len(),
            "OpenAPI path count drifted; register new routes in ApiDoc and EXPECTED_API_PATHS"
        );
        for path in EXPECTED_API_PATHS {
            assert!(
                paths.contains_key(*path),
                "route {path} missing from OpenAPI document"
            );
        }
        let schemas = json
            .pointer("/components/schemas")
            .and_then(|schemas| schemas.as_object())
            .expect("document has schemas");
        for name in [
            "HealthResponse",
            "NodeResponse",
            "ErrorResponse",
            "SessionResponse",
            "CreateSessionRequest",
            "TaskReceipt",
            "BatchResponse",
            "WorkspaceResponse",
            "OpenWorkspaceRequest",
            "CreateWorktreeRequest",
            "WorkspaceSelection",
            "WorkspaceToolLimits",
            "ProviderStatus",
            "DirectoryResponse",
        ] {
            assert!(schemas.contains_key(name), "schema {name} missing");
        }
    }
    #[test]
    fn session_status_is_derived_and_has_no_manual_state_endpoint() {
        let json = serde_json::to_value(ApiDoc::openapi()).unwrap();
        assert_eq!(
            json["components"]["schemas"]["SessionStatus"]["enum"],
            serde_json::json!([
                "idle",
                "queued",
                "running",
                "paused",
                "awaiting_children",
                "ready",
                "completed",
                "failed",
                "stopped"
            ])
        );
        assert!(
            json["components"]["schemas"]["SessionResponse"]["properties"]["status"].is_object()
        );
        let properties = &json["components"]["schemas"]["SessionResponse"]["properties"];
        assert_eq!(properties["status"]["readOnly"], true);
        assert!(properties.get("state").is_none());
        assert!(
            json["paths"]
                .get("/v1/sessions/{session_id}/state")
                .is_none()
        );
        assert!(json["components"]["schemas"].get("SessionState").is_none());
        assert!(json["paths"]["/v1/sessions/{session_id}/inbox"]["post"].is_object());
    }

    #[test]
    fn workspace_contract_requires_membership_and_has_only_explicit_worktree_creation() {
        let json = serde_json::to_value(ApiDoc::openapi()).unwrap();
        assert_eq!(
            json.pointer("/components/schemas/SetWorkspaceColorRequest/properties/color/pattern")
                .unwrap(),
            "^#[0-9A-Fa-f]{6}$"
        );
        assert_eq!(
            json.pointer("/components/schemas/WorkspaceResponse/properties/color/pattern")
                .unwrap(),
            "^#[0-9A-F]{6}$"
        );
        assert!(json["paths"]["/v1/workspaces/{id}/color"]["post"].is_object());
        for schema in ["CreateSessionRequest", "SessionSpec", "BatchSpec"] {
            let required = json
                .pointer(&format!("/components/schemas/{schema}/required"))
                .unwrap()
                .as_array()
                .unwrap();
            assert!(
                required.iter().any(|field| field == "workspace"),
                "{schema} must require a workspace"
            );
            let properties = json
                .pointer(&format!("/components/schemas/{schema}/properties"))
                .unwrap();
            assert!(properties.get("project").is_none());
        }
        assert_eq!(
            json.pointer("/components/schemas/SessionResponse/properties/workspace_id/type")
                .unwrap(),
            "string"
        );
        let activity = &json["components"]["schemas"]["SessionResponse"]["properties"]["last_user_activity_at"];
        assert_eq!(activity["format"], "int64");
        assert!(activity["description"].as_str().unwrap().contains("Unix"));
        assert!(
            json["paths"]
                .get("/v1/workspaces/{id}/worktrees")
                .unwrap()
                .get("post")
                .is_some()
        );
        assert!(
            json["paths"]
                .as_object()
                .unwrap()
                .keys()
                .all(|path| !path.starts_with("/v1/projects"))
        );
        let payload = serde_json::json!({"command_id":"thread","provider":"opencode-go","model":"glm-5.3-flash"});
        assert!(
            serde_json::from_value::<hive_protocol::chat::CreateSessionRequest>(payload.clone())
                .is_err()
        );
        let mut selected = payload;
        selected["workspace"] = serde_json::json!({"workspace_id":"default","allowed_tools":[]});
        assert!(
            serde_json::from_value::<hive_protocol::chat::CreateSessionRequest>(selected.clone())
                .is_ok()
        );
        selected["project"] = serde_json::json!({"project_id":"old"});
        assert!(
            serde_json::from_value::<hive_protocol::chat::CreateSessionRequest>(selected).is_err()
        );
    }
}
