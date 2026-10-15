use super::chat::{AppState, PageQuery, accepted, checked_limit, error, storage_response};
use crate::auth::LocalToken;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use slop_protocol::projects::{
    RegisterProjectRequest, RemoveWorkspaceRequest, RenameWorkspaceRequest,
};
use std::sync::{Arc, atomic::Ordering};

pub fn router(token: Arc<LocalToken>) -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/filesystem", get(super::filesystem::browse))
        .route(
            "/v1/filesystem/directories",
            post(super::filesystem::create),
        )
        .route("/v1/projects", get(projects).post(register))
        .route("/v1/projects/create", post(create))
        .route("/v1/projects/{id}", get(project))
        .route("/v1/projects/{id}/rename", post(rename_project))
        .route("/v1/projects/{id}/workspaces", get(workspaces))
        .route("/v1/projects/{id}/events", get(events))
        .route("/v1/workspaces/{id}", get(workspace))
        .route("/v1/workspaces/{id}/diff", get(diff))
        .route("/v1/workspaces/{id}/remove", post(remove))
        .route("/v1/workspaces/{id}/rename", post(rename))
        .layer(axum::extract::DefaultBodyLimit::max(16 * 1024))
        .route_layer(middleware::from_fn(super::chat::normalize_rejections))
        .route_layer(middleware::from_fn_with_state(
            token,
            super::chat::authorize_request,
        ))
}
#[utoipa::path(
    get,
    path = "/v1/projects",
    tag = "projects",
    params(
        ("after" = Option<u64>, Query, description = "Cursor after which to page"),
        ("limit" = Option<usize>, Query, description = "Page limit, 1-200")
    ),
    responses(
        (status = 200, description = "Registered projects", body = slop_protocol::chat::Page<slop_protocol::projects::ProjectResponse>),
        (status = 400, description = "Invalid limit", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn projects(
    State(state): State<Arc<AppState>>,
    Query(query): Query<PageQuery>,
) -> Response {
    match checked_limit(query.limit) {
        Ok(limit) => storage_response(state.store.projects(query.after, limit).await),
        Err(error) => error.into_response(),
    }
}
#[utoipa::path(
    post,
    path = "/v1/projects",
    tag = "projects",
    request_body = RegisterProjectRequest,
    responses(
        (status = 202, description = "Project registration accepted", body = slop_protocol::projects::ProjectResponse),
        (status = 400, description = "Invalid request", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 503, description = "Daemon shutting down", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn register(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RegisterProjectRequest>,
) -> Response {
    if !state.accepting.load(Ordering::Acquire) {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "shutting_down",
            "The daemon is shutting down.",
        );
    }
    accepted(state.store.register_project(request).await)
}
#[utoipa::path(
    get,
    path = "/v1/projects/{id}",
    tag = "projects",
    params(("id" = String, Path, description = "Project id")),
    responses(
        (status = 200, description = "Project record", body = slop_protocol::projects::ProjectResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Project not found", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn project(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    storage_response(state.store.project(&id).await)
}
#[utoipa::path(
    post,
    path = "/v1/projects/{id}/rename",
    tag = "projects",
    params(("id" = String, Path, description = "Project id")),
    request_body = slop_protocol::projects::RenameProjectRequest,
    responses(
        (status = 202, description = "Project renamed", body = slop_protocol::projects::ProjectResponse),
        (status = 400, description = "Invalid request", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Project not found", body = slop_protocol::ErrorResponse),
        (status = 409, description = "Command state conflict", body = slop_protocol::ErrorResponse),
        (status = 503, description = "Daemon shutting down", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn rename_project(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<slop_protocol::projects::RenameProjectRequest>,
) -> Response {
    if !state.accepting.load(Ordering::Acquire) {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "shutting_down",
            "The daemon is shutting down.",
        );
    }
    accepted(state.store.rename_project(&id, request).await)
}
#[utoipa::path(
    get,
    path = "/v1/workspaces/{id}",
    tag = "projects",
    params(("id" = String, Path, description = "Workspace id")),
    responses(
        (status = 200, description = "Workspace record", body = slop_protocol::projects::WorkspaceResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Workspace not found", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn workspace(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    storage_response(state.store.workspace(&id).await)
}
#[utoipa::path(
    get,
    path = "/v1/projects/{id}/workspaces",
    tag = "projects",
    params(
        ("id" = String, Path, description = "Project id"),
        ("after" = Option<u64>, Query, description = "Cursor after which to page"),
        ("limit" = Option<usize>, Query, description = "Page limit, 1-200")
    ),
    responses(
        (status = 200, description = "Project workspaces", body = slop_protocol::chat::Page<slop_protocol::projects::WorkspaceResponse>),
        (status = 400, description = "Invalid limit", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn workspaces(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<PageQuery>,
) -> Response {
    match checked_limit(query.limit) {
        Ok(limit) => storage_response(state.store.workspaces(&id, query.after, limit).await),
        Err(error) => error.into_response(),
    }
}
#[utoipa::path(
    get,
    path = "/v1/projects/{id}/events",
    tag = "projects",
    params(
        ("id" = String, Path, description = "Project id"),
        ("after" = Option<u64>, Query, description = "Cursor after which to page"),
        ("limit" = Option<usize>, Query, description = "Page limit, 1-200")
    ),
    responses(
        (status = 200, description = "Project events", body = slop_protocol::chat::Page<slop_protocol::projects::ProjectEventResponse>),
        (status = 400, description = "Invalid limit", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn events(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<PageQuery>,
) -> Response {
    match checked_limit(query.limit) {
        Ok(limit) => storage_response(state.store.project_events(&id, query.after, limit).await),
        Err(error) => error.into_response(),
    }
}
#[utoipa::path(
    get,
    path = "/v1/workspaces/{id}/diff",
    tag = "projects",
    params(("id" = String, Path, description = "Workspace id")),
    responses(
        (status = 200, description = "Workspace diff", body = slop_protocol::projects::WorkspaceDiffResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Workspace not found", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn diff(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    storage_response(state.store.workspace_diff(&id).await)
}
#[utoipa::path(
    post,
    path = "/v1/workspaces/{id}/remove",
    tag = "projects",
    params(("id" = String, Path, description = "Workspace id")),
    request_body = RemoveWorkspaceRequest,
    responses(
        (status = 202, description = "Workspace removal accepted", body = slop_protocol::projects::WorkspaceResponse),
        (status = 400, description = "Invalid request", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Workspace not found", body = slop_protocol::ErrorResponse),
        (status = 503, description = "Daemon shutting down", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn remove(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<RemoveWorkspaceRequest>,
) -> Response {
    if !state.accepting.load(Ordering::Acquire) {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "shutting_down",
            "The daemon is shutting down.",
        );
    }
    accepted(state.store.remove_workspace(&id, request).await)
}

#[utoipa::path(
    post,
    path = "/v1/workspaces/{id}/rename",
    tag = "projects",
    params(("id" = String, Path, description = "Workspace id")),
    request_body = RenameWorkspaceRequest,
    responses(
        (status = 202, description = "Workspace renamed", body = slop_protocol::projects::WorkspaceResponse),
        (status = 400, description = "Invalid request", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Workspace not found", body = slop_protocol::ErrorResponse),
        (status = 409, description = "Workspace or command state conflict", body = slop_protocol::ErrorResponse),
        (status = 503, description = "Daemon shutting down", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn rename(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<RenameWorkspaceRequest>,
) -> Response {
    if !state.accepting.load(Ordering::Acquire) {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "shutting_down",
            "The daemon is shutting down.",
        );
    }
    accepted(state.store.rename_workspace(&id, request).await)
}

#[utoipa::path(
    post,
    path = "/v1/projects/create",
    tag = "projects",
    request_body = slop_protocol::filesystem::CreateProjectRequest,
    responses(
        (status = 202, description = "Project creation accepted; a service task owns completion", body = slop_protocol::projects::ProjectResponse),
        (status = 400, description = "Invalid request", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 500, description = "Project creation failed", body = slop_protocol::ErrorResponse),
        (status = 503, description = "Daemon shutting down", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn create(
    State(state): State<Arc<AppState>>,
    Json(request): Json<slop_protocol::filesystem::CreateProjectRequest>,
) -> Response {
    if !state.accepting.load(Ordering::Acquire) {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "shutting_down",
            "The daemon is shutting down.",
        );
    }
    // A detached service task owns creation even if the HTTP client disconnects.
    match tokio::spawn(async move { state.store.create_project(request).await }).await {
        Ok(result) => accepted(result),
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "project_creation_failed",
            "Inspect the target directory before retrying.",
        ),
    }
}
