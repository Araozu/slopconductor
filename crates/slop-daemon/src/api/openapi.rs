//! OpenAPI document and Swagger UI routes for the daemon HTTP API.
//!
//! The document is generated at compile time with `utoipa` and served at
//! `GET /api-docs/openapi.json` (plus Swagger UI at `GET /docs`). The
//! generated JSON is deliberately *not* checked into source control: regenerate
//! it locally with `cargo run -p slop-daemon -- --dump-openapi` (see
//! `AGENTS.md`).
//!
//! To keep the document accurate:
//! - Derive `utoipa::ToSchema` on every new `slop-protocol` request/response type.
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

/// Versioned public API served by `slopd`.
///
/// Only handlers annotated with `#[utoipa::path]` appear under `paths`. Every
/// other schema listed here is still emitted under `components/schemas` so
/// clients can codegen against the full wire contract.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Slop Conductor Daemon API",
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
        super::chat::history::fork_session,
        super::chat::history::edit_message,
        super::chat::history::delete_message,
        super::chat::sessions::update_session_inbox,
        super::chat::sessions::drafts,
        super::chat::sessions::save_draft,
        super::chat::sessions::remove_draft,
        super::chat::sessions::messages,
        super::chat::sessions::send_message,
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
        super::providers::login_status,
        // Projects, workspaces, and local filesystem navigation.
        super::filesystem::browse,
        super::filesystem::create,
        super::projects::projects,
        super::projects::register,
        super::projects::create,
        super::projects::project,
        super::projects::workspaces,
        super::projects::events,
        super::projects::workspace,
        super::projects::diff,
        super::projects::remove,
    ),
    components(
        schemas(
            slop_protocol::HealthResponse,
            slop_protocol::peers::PeerScope,
            slop_protocol::peers::CreatePeerGrantRequest,
            slop_protocol::peers::PeerGrantResponse,
            slop_protocol::peers::RegisterPeerRequest,
            slop_protocol::peers::PeerResponse,
            slop_protocol::peers::PeerProbeResponse,

            slop_protocol::NodeResponse,
            slop_protocol::ErrorResponse,
            slop_protocol::filesystem::DirectoryEntry,
            slop_protocol::filesystem::DirectoryResponse,
            slop_protocol::filesystem::CreateDirectoryRequest,
            slop_protocol::filesystem::CreateProjectRequest,
            slop_protocol::chat::CancelTurnRequest,
            slop_protocol::chat::CommandReceipt,
            slop_protocol::chat::CreateSessionRequest,
            slop_protocol::chat::DeleteMessageRequest,
            slop_protocol::chat::DeliveryMode,
            slop_protocol::chat::EditMessageRequest,
            slop_protocol::chat::EventFrame,
            slop_protocol::chat::EventResponse,
            slop_protocol::chat::ForkSessionRequest,
            slop_protocol::chat::SessionFork,
            slop_protocol::chat::SessionInbox,
            slop_protocol::chat::UpdateSessionInboxRequest,
            slop_protocol::chat::ConversationDraft,
            slop_protocol::chat::SaveConversationDraftRequest,
            slop_protocol::chat::MessageResponse,
            slop_protocol::chat::ModelResponse,
            slop_protocol::chat::SendMessageRequest,
            slop_protocol::chat::SessionResponse,
            slop_protocol::chat::SteeringInstructionResponse,
            slop_protocol::chat::TurnControlRequest,
            slop_protocol::chat::TurnResponse,
            slop_protocol::chat::UsageResponse,
            slop_protocol::execution::ArtifactResponse,
            slop_protocol::execution::BlockContent,
            slop_protocol::execution::CapabilitiesResponse,
            slop_protocol::execution::ContentBlock,
            slop_protocol::execution::GenerationSettings,
            slop_protocol::execution::ModelCapabilities,
            slop_protocol::execution::ModelRequestResponse,
            slop_protocol::execution::ToolDescriptor,
            slop_protocol::execution::ToolInvocationResponse,
            slop_protocol::execution::WorkspacePolicy,
            slop_protocol::orchestration::BatchCancelReceipt,
            slop_protocol::orchestration::BatchPreviewResponse,
            slop_protocol::orchestration::BatchReceipt,
            slop_protocol::orchestration::BatchResponse,
            slop_protocol::orchestration::BatchResultResponse,
            slop_protocol::orchestration::BatchSpec,
            slop_protocol::orchestration::BudgetUsage,
            slop_protocol::orchestration::ChildContext,
            slop_protocol::orchestration::ChildLink,
            slop_protocol::orchestration::CombinationResponse,
            slop_protocol::orchestration::CreateBatchRequest,
            slop_protocol::orchestration::CreateChildRequest,
            slop_protocol::orchestration::CreateTaskRequest,
            slop_protocol::orchestration::OperationBudget,
            slop_protocol::orchestration::OrchestrationPolicy,
            slop_protocol::orchestration::RetryBatchRequest,
            slop_protocol::orchestration::RetryRunRequest,
            slop_protocol::orchestration::RunResponse,
            slop_protocol::orchestration::RunResultResponse,
            slop_protocol::orchestration::SessionSpec,
            slop_protocol::orchestration::TaskEventResponse,
            slop_protocol::orchestration::TaskReceipt,
            slop_protocol::orchestration::WaitChildrenRequest,
            slop_protocol::projects::ProjectEventResponse,
            slop_protocol::projects::ProjectResponse,
            slop_protocol::projects::ProjectWorkspaceRequest,
            slop_protocol::projects::RegisterProjectRequest,
            slop_protocol::projects::RemoveWorkspaceRequest,
            slop_protocol::projects::WorkspaceDiffResponse,
            slop_protocol::projects::WorkspaceResponse,
            slop_protocol::providers::LoginResponse,
            slop_protocol::providers::LoginStatus,
            slop_protocol::providers::ProviderAccount,
            slop_protocol::providers::ProviderCatalog,
            slop_protocol::providers::ProviderStatus,
            slop_protocol::providers::SelectAuthRequest,
            slop_protocol::providers::SetApiKeyRequest,
            slop_protocol::providers::SignOutResponse,
            slop_protocol::providers::StartLoginRequest,
        )
    ),
    modifiers(&BearerSecurity),
    tags(
        (name = "peers", description = "Local peer administration and scoped owner access"),
        (name = "system", description = "Health and node identity"),
        (name = "chat", description = "Durable sessions and text chat"),
        (name = "orchestration", description = "Tasks, run attempts, and batch matrices"),
        (name = "providers", description = "Provider credentials and discovery"),
        (name = "projects", description = "Projects, workspaces, and filesystem navigation"),
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
        // Projects and filesystem.
        "/v1/filesystem",
        "/v1/filesystem/directories",
        "/v1/projects",
        "/v1/projects/create",
        "/v1/projects/{id}",
        "/v1/projects/{id}/workspaces",
        "/v1/projects/{id}/events",
        "/v1/workspaces/{id}",
        "/v1/workspaces/{id}/diff",
        "/v1/workspaces/{id}/remove",
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
            "ProjectResponse",
            "ProviderStatus",
            "DirectoryResponse",
        ] {
            assert!(schemas.contains_key(name), "schema {name} missing");
        }
    }
}
