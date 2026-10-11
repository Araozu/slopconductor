use super::*;

pub(super) async fn turn(
    State(state): State<Arc<AppState>>,
    Path(turn_id): Path<String>,
) -> Response {
    storage_response(state.store.turn(&turn_id).await)
}

pub(super) async fn cancel_turn(
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

pub(super) async fn pause_turn(
    State(state): State<Arc<AppState>>,
    Path(turn_id): Path<String>,
    Json(request): Json<slop_protocol::chat::TurnControlRequest>,
) -> Response {
    storage_response(state.store.pause_turn(&turn_id, request).await)
}

pub(super) async fn resume_turn(
    State(state): State<Arc<AppState>>,
    Path(turn_id): Path<String>,
    Json(request): Json<slop_protocol::chat::TurnControlRequest>,
) -> Response {
    storage_response(state.store.resume_turn(&turn_id, request).await)
}
