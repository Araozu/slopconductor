//! Unified workspace operations. Opening and selecting never create worktrees.
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
use hive_protocol::workspaces::*;
use std::sync::{Arc, atomic::Ordering};

pub fn router(token: Arc<LocalToken>) -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/filesystem", get(super::filesystem::browse))
        .route(
            "/v1/filesystem/directories",
            post(super::filesystem::create),
        )
        .route("/v1/workspaces", get(workspaces).post(open))
        .route("/v1/workspaces/create", post(create))
        .route("/v1/workspaces/{id}", get(workspace))
        .route("/v1/workspaces/{id}/worktrees", post(worktree))
        .route("/v1/workspaces/{id}/events", get(events))
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

/// List all workspaces, including opened directories and explicitly created worktrees.
#[utoipa::path(
    get, path = "/v1/workspaces", tag = "workspaces",
    params(("after" = Option<u64>, Query, description = "Cursor after which to page"),("limit" = Option<usize>, Query, description = "Page limit, 1-200")),
    responses(
        (status = 200, description = "List all workspaces, including opened directories and explicitly created worktrees.", body = hive_protocol::chat::Page<WorkspaceResponse>),
        (status = 400, description = "Invalid request or unsupported capability", body = hive_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = hive_protocol::ErrorResponse),
        (status = 404, description = "Workspace not found", body = hive_protocol::ErrorResponse),
        (status = 409, description = "Workspace busy, unavailable, or command conflicts", body = hive_protocol::ErrorResponse),
        (status = 429, description = "Workspace or service capacity reached", body = hive_protocol::ErrorResponse),
        (status = 500, description = "Operation failed; inspect workspace state before retrying", body = hive_protocol::ErrorResponse),
        (status = 503, description = "Daemon or storage unavailable", body = hive_protocol::ErrorResponse),
    ), security(("bearer_auth" = []))
)]
pub(crate) async fn workspaces(
    State(state): State<Arc<AppState>>,
    Query(query): Query<PageQuery>,
) -> Response {
    let limit = match checked_limit(query.limit) {
        Ok(limit) => limit,
        Err(error) => return error.into_response(),
    };
    storage_response(state.store.workspaces(query.after, limit).await)
}

/// Open an existing directory in place. Git detection never creates a worktree.
#[utoipa::path(
    post, path = "/v1/workspaces", tag = "workspaces",
    request_body = OpenWorkspaceRequest,
    responses(
        (status = 202, description = "Open an existing directory in place. Git detection never creates a worktree.", body = WorkspaceResponse),
        (status = 400, description = "Invalid request or unsupported capability", body = hive_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = hive_protocol::ErrorResponse),
        (status = 404, description = "Workspace not found", body = hive_protocol::ErrorResponse),
        (status = 409, description = "Workspace busy, unavailable, or command conflicts", body = hive_protocol::ErrorResponse),
        (status = 429, description = "Workspace or service capacity reached", body = hive_protocol::ErrorResponse),
        (status = 500, description = "Operation failed; inspect workspace state before retrying", body = hive_protocol::ErrorResponse),
        (status = 503, description = "Daemon or storage unavailable", body = hive_protocol::ErrorResponse),
    ), security(("bearer_auth" = []))
)]
pub(crate) async fn open(
    State(state): State<Arc<AppState>>,
    Json(request): Json<OpenWorkspaceRequest>,
) -> Response {
    if !state.accepting.load(Ordering::Acquire) {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "shutting_down",
            "The daemon is shutting down.",
        );
    }
    accepted(state.store.open_workspace(request).await)
}

/// Explicitly create a new folder, optionally initializing Git. Returns allocating; poll until ready or failed. Never creates a worktree.
#[utoipa::path(
    post, path = "/v1/workspaces/create", tag = "workspaces",
    request_body = CreateWorkspaceRequest,
    responses(
        (status = 202, description = "Explicitly create a new folder, optionally initializing Git. Returns allocating; poll until ready or failed. Never creates a worktree.", body = WorkspaceResponse),
        (status = 400, description = "Invalid request or unsupported capability", body = hive_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = hive_protocol::ErrorResponse),
        (status = 404, description = "Workspace not found", body = hive_protocol::ErrorResponse),
        (status = 409, description = "Workspace busy, unavailable, or command conflicts", body = hive_protocol::ErrorResponse),
        (status = 429, description = "Workspace or service capacity reached", body = hive_protocol::ErrorResponse),
        (status = 500, description = "Operation failed; inspect workspace state before retrying", body = hive_protocol::ErrorResponse),
        (status = 503, description = "Daemon or storage unavailable", body = hive_protocol::ErrorResponse),
    ), security(("bearer_auth" = []))
)]
pub(crate) async fn create(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateWorkspaceRequest>,
) -> Response {
    if !state.accepting.load(Ordering::Acquire) {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "shutting_down",
            "The daemon is shutting down.",
        );
    }
    owned_create(state, request).await
}

/// Inspect a workspace.
#[utoipa::path(
    get, path = "/v1/workspaces/{id}", tag = "workspaces",
    params(("id" = String, Path, description = "Workspace id")),
    responses(
        (status = 200, description = "Inspect a workspace.", body = WorkspaceResponse),
        (status = 400, description = "Invalid request or unsupported capability", body = hive_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = hive_protocol::ErrorResponse),
        (status = 404, description = "Workspace not found", body = hive_protocol::ErrorResponse),
        (status = 409, description = "Workspace busy, unavailable, or command conflicts", body = hive_protocol::ErrorResponse),
        (status = 429, description = "Workspace or service capacity reached", body = hive_protocol::ErrorResponse),
        (status = 500, description = "Operation failed; inspect workspace state before retrying", body = hive_protocol::ErrorResponse),
        (status = 503, description = "Daemon or storage unavailable", body = hive_protocol::ErrorResponse),
    ), security(("bearer_auth" = []))
)]
pub(crate) async fn workspace(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    storage_response(state.store.workspace(&id).await)
}

/// Explicitly create a detached worktree from an existing workspace. Returns allocating; poll the workspace until ready or failed. Uncommitted files are not copied.
#[utoipa::path(
    post, path = "/v1/workspaces/{id}/worktrees", tag = "workspaces",
    params(("id" = String, Path, description = "Workspace id")),
    request_body = CreateWorktreeRequest,
    responses(
        (status = 202, description = "Explicitly create a detached worktree from an existing workspace. Returns allocating; poll the workspace until ready or failed. Uncommitted files are not copied.", body = WorkspaceResponse),
        (status = 400, description = "Invalid request or unsupported capability", body = hive_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = hive_protocol::ErrorResponse),
        (status = 404, description = "Workspace not found", body = hive_protocol::ErrorResponse),
        (status = 409, description = "Workspace busy, unavailable, or command conflicts", body = hive_protocol::ErrorResponse),
        (status = 429, description = "Workspace or service capacity reached", body = hive_protocol::ErrorResponse),
        (status = 500, description = "Operation failed; inspect workspace state before retrying", body = hive_protocol::ErrorResponse),
        (status = 503, description = "Daemon or storage unavailable", body = hive_protocol::ErrorResponse),
    ), security(("bearer_auth" = []))
)]
pub(crate) async fn worktree(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<CreateWorktreeRequest>,
) -> Response {
    if !state.accepting.load(Ordering::Acquire) {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "shutting_down",
            "The daemon is shutting down.",
        );
    }
    owned_worktree(state, id, request).await
}

/// List durable workspace events.
#[utoipa::path(
    get, path = "/v1/workspaces/{id}/events", tag = "workspaces",
    params(("id" = String, Path, description = "Workspace id"),("after" = Option<u64>, Query, description = "Cursor after which to page"),("limit" = Option<usize>, Query, description = "Page limit, 1-200")),
    responses(
        (status = 200, description = "List durable workspace events.", body = hive_protocol::chat::Page<WorkspaceEventResponse>),
        (status = 400, description = "Invalid request or unsupported capability", body = hive_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = hive_protocol::ErrorResponse),
        (status = 404, description = "Workspace not found", body = hive_protocol::ErrorResponse),
        (status = 409, description = "Workspace busy, unavailable, or command conflicts", body = hive_protocol::ErrorResponse),
        (status = 429, description = "Workspace or service capacity reached", body = hive_protocol::ErrorResponse),
        (status = 500, description = "Operation failed; inspect workspace state before retrying", body = hive_protocol::ErrorResponse),
        (status = 503, description = "Daemon or storage unavailable", body = hive_protocol::ErrorResponse),
    ), security(("bearer_auth" = []))
)]
pub(crate) async fn events(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<PageQuery>,
) -> Response {
    let limit = match checked_limit(query.limit) {
        Ok(limit) => limit,
        Err(error) => return error.into_response(),
    };
    storage_response(state.store.workspace_events(&id, query.after, limit).await)
}

/// Inspect Git changes. Opened checkouts compare against current HEAD; created worktrees compare against their frozen base. Ordinary folders return unsupported_capability.
#[utoipa::path(
    get, path = "/v1/workspaces/{id}/diff", tag = "workspaces",
    params(("id" = String, Path, description = "Workspace id")),
    responses(
        (status = 200, description = "Inspect Git changes. Opened checkouts compare against current HEAD; created worktrees compare against their frozen base. Ordinary folders return unsupported_capability.", body = WorkspaceDiffResponse),
        (status = 400, description = "Invalid request or unsupported capability", body = hive_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = hive_protocol::ErrorResponse),
        (status = 404, description = "Workspace not found", body = hive_protocol::ErrorResponse),
        (status = 409, description = "Workspace busy, unavailable, or command conflicts", body = hive_protocol::ErrorResponse),
        (status = 429, description = "Workspace or service capacity reached", body = hive_protocol::ErrorResponse),
        (status = 500, description = "Operation failed; inspect workspace state before retrying", body = hive_protocol::ErrorResponse),
        (status = 503, description = "Daemon or storage unavailable", body = hive_protocol::ErrorResponse),
    ), security(("bearer_auth" = []))
)]
pub(crate) async fn diff(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    storage_response(state.store.workspace_diff(&id).await)
}

/// Remove a workspace registration. Opened directories are retained. Hive-created worktrees are removed asynchronously only when clean and unchanged from their base.
#[utoipa::path(
    post, path = "/v1/workspaces/{id}/remove", tag = "workspaces",
    params(("id" = String, Path, description = "Workspace id")),
    request_body = RemoveWorkspaceRequest,
    responses(
        (status = 202, description = "Remove a workspace registration. Opened directories are retained. Hive-created worktrees are removed asynchronously only when clean and unchanged from their base.", body = WorkspaceResponse),
        (status = 400, description = "Invalid request or unsupported capability", body = hive_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = hive_protocol::ErrorResponse),
        (status = 404, description = "Workspace not found", body = hive_protocol::ErrorResponse),
        (status = 409, description = "Workspace busy, unavailable, or command conflicts", body = hive_protocol::ErrorResponse),
        (status = 429, description = "Workspace or service capacity reached", body = hive_protocol::ErrorResponse),
        (status = 500, description = "Operation failed; inspect workspace state before retrying", body = hive_protocol::ErrorResponse),
        (status = 503, description = "Daemon or storage unavailable", body = hive_protocol::ErrorResponse),
    ), security(("bearer_auth" = []))
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

/// Rename a workspace without changing its path.
#[utoipa::path(
    post, path = "/v1/workspaces/{id}/rename", tag = "workspaces",
    params(("id" = String, Path, description = "Workspace id")),
    request_body = RenameWorkspaceRequest,
    responses(
        (status = 202, description = "Rename a workspace without changing its path.", body = WorkspaceResponse),
        (status = 400, description = "Invalid request or unsupported capability", body = hive_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = hive_protocol::ErrorResponse),
        (status = 404, description = "Workspace not found", body = hive_protocol::ErrorResponse),
        (status = 409, description = "Workspace busy, unavailable, or command conflicts", body = hive_protocol::ErrorResponse),
        (status = 429, description = "Workspace or service capacity reached", body = hive_protocol::ErrorResponse),
        (status = 500, description = "Operation failed; inspect workspace state before retrying", body = hive_protocol::ErrorResponse),
        (status = 503, description = "Daemon or storage unavailable", body = hive_protocol::ErrorResponse),
    ), security(("bearer_auth" = []))
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

async fn owned_create(state: Arc<AppState>, request: CreateWorkspaceRequest) -> Response {
    match tokio::spawn(async move { state.store.create_workspace(request).await }).await {
        Ok(result) => accepted(result),
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "workspace_creation_failed",
            "Inspect the target directory before retrying.",
        ),
    }
}
async fn owned_worktree(
    state: Arc<AppState>,
    id: String,
    request: CreateWorktreeRequest,
) -> Response {
    match tokio::spawn(async move { state.store.create_worktree(&id, request).await }).await {
        Ok(result) => accepted(result),
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "worktree_creation_failed",
            "Inspect workspace records before retrying.",
        ),
    }
}
