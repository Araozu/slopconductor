use super::*;

#[utoipa::path(
    get,
    path = "/v1/turns/{turn_id}",
    tag = "chat",
    params(("turn_id" = String, Path, description = "Turn id")),
    responses(
        (status = 200, description = "Turn record", body = hive_protocol::chat::TurnResponse),
        (status = 401, description = "Missing or invalid bearer token", body = hive_protocol::ErrorResponse),
        (status = 404, description = "Turn not found", body = hive_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn turn(
    State(state): State<Arc<AppState>>,
    Path(turn_id): Path<String>,
) -> Response {
    storage_response(state.store.turn(&turn_id).await)
}

#[utoipa::path(
    post,
    path = "/v1/turns/{turn_id}/cancel",
    tag = "chat",
    params(("turn_id" = String, Path, description = "Turn id")),
    request_body = CancelTurnRequest,
    responses(
        (status = 202, description = "Turn cancellation accepted", body = hive_protocol::chat::CommandReceipt),
        (status = 400, description = "Invalid request", body = hive_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = hive_protocol::ErrorResponse),
        (status = 503, description = "Daemon shutting down", body = hive_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn cancel_turn(
    State(state): State<Arc<AppState>>,
    Path(turn_id): Path<String>,
    Json(request): Json<CancelTurnRequest>,
) -> Response {
    if !state.accepting.load(std::sync::atomic::Ordering::Acquire) {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "shutting_down",
            "The daemon is shutting down.",
        );
    }
    accepted(state.store.cancel_turn(&turn_id, request).await)
}

#[utoipa::path(
    post,
    path = "/v1/turns/{turn_id}/pause",
    tag = "chat",
    params(("turn_id" = String, Path, description = "Turn id")),
    request_body = hive_protocol::chat::TurnControlRequest,
    responses(
        (status = 200, description = "Turn paused", body = hive_protocol::chat::CommandReceipt),
        (status = 400, description = "Invalid request", body = hive_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = hive_protocol::ErrorResponse),
        (status = 404, description = "Turn not found", body = hive_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn pause_turn(
    State(state): State<Arc<AppState>>,
    Path(turn_id): Path<String>,
    Json(request): Json<hive_protocol::chat::TurnControlRequest>,
) -> Response {
    storage_response(state.store.pause_turn(&turn_id, request).await)
}

#[utoipa::path(
    post,
    path = "/v1/turns/{turn_id}/resume",
    tag = "chat",
    params(("turn_id" = String, Path, description = "Turn id")),
    request_body = hive_protocol::chat::TurnControlRequest,
    responses(
        (status = 200, description = "Turn resumed", body = hive_protocol::chat::CommandReceipt),
        (status = 400, description = "Invalid request", body = hive_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = hive_protocol::ErrorResponse),
        (status = 404, description = "Turn not found", body = hive_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn resume_turn(
    State(state): State<Arc<AppState>>,
    Path(turn_id): Path<String>,
    Json(request): Json<hive_protocol::chat::TurnControlRequest>,
) -> Response {
    storage_response(state.store.resume_turn(&turn_id, request).await)
}
