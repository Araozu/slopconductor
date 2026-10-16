use super::*;
use hive_protocol::chat::InterruptSessionRequest;

#[utoipa::path(
    get, path = "/v1/sessions/{session_id}/queue", tag = "chat",
    params(("session_id" = String, Path, description = "Session id")),
    responses(
        (status = 200, description = "Unsent messages in FIFO order", body = Vec<hive_protocol::chat::QueuedMessageResponse>),
        (status = 401, description = "Unauthorized", body = hive_protocol::ErrorResponse),
        (status = 404, description = "Session not found", body = hive_protocol::ErrorResponse)
    ), security(("bearer_auth" = []))
)]
pub(crate) async fn queued_messages(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Response {
    storage_response(state.store.queued_messages(&session_id).await)
}

#[utoipa::path(
    post, path = "/v1/sessions/{session_id}/interrupt", tag = "chat",
    params(("session_id" = String, Path, description = "Session id")),
    request_body = InterruptSessionRequest,
    responses(
        (status = 202, description = "Cancel execution and dispatch the leading queue batch", body = hive_protocol::chat::CommandReceipt),
        (status = 400, description = "Invalid request", body = hive_protocol::ErrorResponse),
        (status = 401, description = "Unauthorized", body = hive_protocol::ErrorResponse),
        (status = 404, description = "Session not found", body = hive_protocol::ErrorResponse)
    ), security(("bearer_auth" = []))
)]
pub(crate) async fn interrupt_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Json(request): Json<InterruptSessionRequest>,
) -> Response {
    if request.messages.is_empty() {
        return accepted(state.store.interrupt_session(&session_id, request).await);
    }
    let session = match state.store.session(&session_id).await {
        Ok(session) => session,
        Err(error) => return store_error(error),
    };
    let settings = request
        .messages
        .iter()
        .map(|message| {
            sessions::preflight_turn(
                &state,
                &session,
                message.model.as_deref(),
                message.settings.as_ref(),
            )
            .ok()
        })
        .collect();
    accepted(
        state
            .store
            .interrupt_session_preflighted(&session_id, request, settings)
            .await,
    )
}
