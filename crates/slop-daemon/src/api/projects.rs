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
use slop_protocol::projects::{RegisterProjectRequest, RemoveWorkspaceRequest};
use std::sync::{Arc, atomic::Ordering};

pub fn router(token: Arc<LocalToken>) -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/projects", get(projects).post(register))
        .route("/v1/projects/{id}", get(project))
        .route("/v1/projects/{id}/workspaces", get(workspaces))
        .route("/v1/projects/{id}/events", get(events))
        .route("/v1/workspaces/{id}", get(workspace))
        .route("/v1/workspaces/{id}/diff", get(diff))
        .route("/v1/workspaces/{id}/remove", post(remove))
        .layer(axum::extract::DefaultBodyLimit::max(16 * 1024))
        .route_layer(middleware::from_fn(super::chat::normalize_rejections))
        .route_layer(middleware::from_fn_with_state(
            token,
            super::chat::authorize_request,
        ))
}
async fn projects(State(state): State<Arc<AppState>>, Query(query): Query<PageQuery>) -> Response {
    match checked_limit(query.limit) {
        Ok(limit) => storage_response(state.store.projects(query.after, limit).await),
        Err(error) => error.into_response(),
    }
}
async fn register(
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
async fn project(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    storage_response(state.store.project(&id).await)
}
async fn workspace(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    storage_response(state.store.workspace(&id).await)
}
async fn workspaces(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<PageQuery>,
) -> Response {
    match checked_limit(query.limit) {
        Ok(limit) => storage_response(state.store.workspaces(&id, query.after, limit).await),
        Err(error) => error.into_response(),
    }
}
async fn events(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<PageQuery>,
) -> Response {
    match checked_limit(query.limit) {
        Ok(limit) => storage_response(state.store.project_events(&id, query.after, limit).await),
        Err(error) => error.into_response(),
    }
}
async fn diff(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    storage_response(state.store.workspace_diff(&id).await)
}
async fn remove(
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
