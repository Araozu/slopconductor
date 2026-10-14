use super::*;
use slop_protocol::chat::{DeleteMessageRequest, EditMessageRequest, ForkSessionRequest};

#[utoipa::path(
    post,
    path = "/v1/sessions/{session_id}/fork",
    tag = "chat",
    params(("session_id" = String, Path, description = "Source session id")),
    request_body = ForkSessionRequest,
    responses(
        (status = 202, description = "Fork accepted", body = slop_protocol::chat::CommandReceipt),
        (status = 400, description = "Invalid request", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Session not found", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn fork_session(
    State(state): State<Arc<AppState>>,
    Path(session): Path<String>,
    Json(request): Json<ForkSessionRequest>,
) -> Response {
    accepted(state.store.fork_session(&session, request).await)
}

#[utoipa::path(
    patch,
    path = "/v1/sessions/{session_id}/messages/{message_id}",
    tag = "chat",
    params(
        ("session_id" = String, Path, description = "Session id"),
        ("message_id" = String, Path, description = "Message id")
    ),
    request_body = EditMessageRequest,
    responses(
        (status = 202, description = "Edit accepted", body = slop_protocol::chat::CommandReceipt),
        (status = 400, description = "Invalid request", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Message not found", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn edit_message(
    State(state): State<Arc<AppState>>,
    Path((session, message)): Path<(String, String)>,
    Json(request): Json<EditMessageRequest>,
) -> Response {
    // Deduplication precedes lookups and provider preflight, so a lost response
    // is recoverable even after the source history or credentials change.
    match state.store.command_seen(&request.command_id).await {
        Ok(true) => {
            return accepted(
                state
                    .store
                    .edit_message(&session, &message, request, None)
                    .await,
            );
        }
        Err(e) => return store_error(e),
        Ok(false) => {}
    }
    let target = match state.store.message(&message).await {
        Ok(target) if target.session_id == session => target,
        Ok(_) => return store_error(StoreError::NotFound),
        Err(e) => return store_error(e),
    };
    let settings = if target.role == "user" {
        if !state.accepting.load(std::sync::atomic::Ordering::Acquire)
            || !state.runtime.accepting_work()
        {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "runtime_unavailable",
                "The chat execution runtime is unavailable.",
            );
        }
        let source = match state.store.session(&session).await {
            Ok(source) => source,
            Err(e) => return store_error(e),
        };
        let mut spec = source.spec;
        spec.prompt = request.text.clone();
        spec.model = format!("{}/{}", source.provider, source.model);
        spec.settings = source.settings;
        match crate::api::orchestration::preflight(&state, spec, Some(state.runtime.max_tokens())) {
            Ok(spec) => Some(spec.settings),
            Err(e) => return store_error(e),
        }
    } else {
        None
    };
    accepted(
        state
            .store
            .edit_message(&session, &message, request, settings)
            .await,
    )
}

#[utoipa::path(
    delete,
    path = "/v1/sessions/{session_id}/messages/{message_id}",
    tag = "chat",
    params(
        ("session_id" = String, Path, description = "Session id"),
        ("message_id" = String, Path, description = "Message id")
    ),
    request_body = DeleteMessageRequest,
    responses(
        (status = 202, description = "Delete accepted", body = slop_protocol::chat::CommandReceipt),
        (status = 400, description = "Invalid request or history busy", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Message not found", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn delete_message(
    State(state): State<Arc<AppState>>,
    Path((session, message)): Path<(String, String)>,
    Json(request): Json<DeleteMessageRequest>,
) -> Response {
    accepted(
        state
            .store
            .delete_message(&session, &message, request)
            .await,
    )
}
