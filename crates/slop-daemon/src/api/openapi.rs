//! OpenAPI document and Swagger UI routes for the daemon HTTP API.
//!
//! The document is generated at compile time with `utoipa`. To keep it accurate:
//! - Derive `utoipa::ToSchema` on every new `slop-protocol` request/response type.
//! - Annotate every new handler with `#[utoipa::path(...)]`.
//! - Register the handler and all new schemas in `ApiDoc` below.

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
        description = "Machine-local Slop Conductor service. Health is anonymous; every other route requires `Authorization: Bearer <local-api-token>`."
    ),
    servers(
        (url = "http://127.0.0.1:7331", description = "Local daemon default")
    ),
    paths(
        crate::health,
        crate::node,
    ),
    components(
        schemas(
            slop_protocol::HealthResponse,
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
        (name = "system", description = "Health and node identity"),
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

    #[test]
    fn openapi_document_contains_registered_paths_and_schemas() {
        let doc = ApiDoc::openapi();
        let json = serde_json::to_value(&doc).expect("OpenAPI document serializes to JSON");
        let paths = json
            .get("paths")
            .and_then(|paths| paths.as_object())
            .expect("document has paths");
        assert!(
            paths.contains_key("/v1/health"),
            "health path missing from OpenAPI document"
        );
        assert!(
            paths.contains_key("/v1/node"),
            "node path missing from OpenAPI document"
        );
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
        ] {
            assert!(schemas.contains_key(name), "schema {name} missing");
        }
    }
}
