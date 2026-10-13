use super::*;
use slop_protocol::chat::{DeleteMessageRequest, EditMessageRequest, ForkSessionRequest};

pub(super) async fn fork_session(
    State(state): State<Arc<AppState>>,
    Path(session): Path<String>,
    Json(request): Json<ForkSessionRequest>,
) -> Response {
    accepted(state.store.fork_session(&session, request).await)
}

pub(super) async fn edit_message(
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

pub(super) async fn delete_message(
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
